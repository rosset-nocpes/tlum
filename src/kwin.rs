//! KDE Plasma (KWin) on Wayland: where the pointer is, a screenshot of the area around it, and
//! the global keyboard shortcut. Wayland gives no client the global pointer or other windows'
//! pixels, so we ask the compositor:
//!
//! - pointer: a one-shot KWin script reads `workspace.cursorPos` and calls back over D-Bus;
//! - pixels: `org.kde.KWin.ScreenShot2.CaptureArea`, which KWin allows for executables whose
//!   desktop file lists it in `X-KDE-DBUS-Restricted-Interfaces` (`tlum install` writes it);
//!   without that, Spectacle's background capture is the fallback;
//! - shortcut: a resident KWin script registers Meta+Ctrl+D (⌃⌘D on a Mac) under
//!   System Settings → Shortcuts → KWin, and calls back with the pointer when pressed.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

use crate::scan::Shot;

pub const SERVICE: &str = "dev.tlum.Tlum";
pub const PATH: &str = "/dev/tlum/Tlum";
const SHORTCUT_PLUGIN: &str = "tlum-shortcut";
const POINTER_PLUGIN: &str = "tlum-pointer";

/// The pointer and the output it's on, in global logical coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Pointer {
    pub x: f64,
    pub y: f64,
    pub output: Option<String>,
    /// The output's logical rect.
    pub screen: Option<[f64; 4]>,
    pub scale: f64,
    /// The output minus panels' exclusive zones, which the popover keeps inside.
    pub area: Option<[f64; 4]>,
}

/// `"x,y"` or `"x,y,name,sx,sy,sw,sh,scale[,ax,ay,aw,ah]"`, as the scripts send it.
pub fn parse_pointer(s: &str) -> Option<Pointer> {
    let parts: Vec<&str> = s.split(',').collect();
    let num = |i: usize| parts.get(i).and_then(|p| p.trim().parse::<f64>().ok());
    let (x, y) = (num(0)?, num(1)?);
    let output = parts.get(2).map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let screen = (|| Some([num(3)?, num(4)?, num(5)?, num(6)?]))();
    let area = (|| Some([num(8)?, num(9)?, num(10)?, num(11)?]))().filter(|a| a[2] > 0.0 && a[3] > 0.0);
    Some(Pointer { x, y, output, screen, scale: num(7).filter(|s| *s > 0.0).unwrap_or(1.0), area })
}

/// The JavaScript every callback runs: the pointer plus its screen's name, geometry and scale.
fn report_js(method: &str) -> String {
    format!(r#"(function () {{
    const p = workspace.cursorPos;
    let o = null;
    try {{ o = workspace.screenAt(p); }} catch (e) {{}}
    if (!o) {{ try {{ o = workspace.activeScreen; }} catch (e) {{}} }}
    let extra = "";
    if (o) {{
        const g = o.geometry;
        extra = "," + o.name + "," + g.x + "," + g.y + "," + g.width + "," + g.height + "," + (o.devicePixelRatio || 1);
        try {{
            const a = workspace.clientArea(KWin.MaximizeArea, o, workspace.currentDesktop);
            extra += "," + a.x + "," + a.y + "," + a.width + "," + a.height;
        }} catch (e) {{}}
    }}
    callDBus("{SERVICE}", "{PATH}", "{SERVICE}", "{method}", p.x + "," + p.y + extra);
}})"#)
}

fn script_dir() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("tlum");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn scripting(conn: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new(conn, "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting")
}

/// Loads `js` as plugin `name` and runs it.
fn run_script(conn: &Connection, name: &str, js: &str) -> Result<(), String> {
    let path = script_dir().join(format!("{name}.js"));
    std::fs::write(&path, js).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    let scripting = scripting(conn).map_err(|e| e.to_string())?;
    let _: Result<bool, _> = scripting.call("unloadScript", &(name,));
    let id: i32 = scripting.call("loadScript", &(path.to_string_lossy().as_ref(), name)).map_err(|e| format!("KWin scripting unavailable: {e}"))?;
    if id < 0 { return Err("KWin refused the script".into()); }
    // KWin 6 serves scripts at /Scripting/Script<id>; KWin 5 at /<id>.
    for path in [format!("/Scripting/Script{id}"), format!("/{id}")] {
        if let Ok(p) = Proxy::new(conn, "org.kde.KWin", path, "org.kde.kwin.Script")
            && p.call::<_, _, ()>("run", &()).is_ok() { return Ok(()); }
    }
    Err("KWin loaded the script but wouldn't run it".into())
}

pub fn is_kwin(conn: &Connection) -> bool {
    zbus::blocking::fdo::DBusProxy::new(conn).ok()
        .and_then(|d| d.name_has_owner("org.kde.KWin".try_into().unwrap()).ok())
        .unwrap_or(false)
}

/// Asks KWin where the pointer is. `replies` receives what the script's `Pointer` call
/// delivers to our service.
pub fn pointer(conn: &Connection, replies: &mpsc::Receiver<String>) -> Result<Pointer, String> {
    while replies.try_recv().is_ok() {}
    let js = format!("{}();", report_js("Pointer"));
    run_script(conn, POINTER_PLUGIN, &js)?;
    let reply = replies.recv_timeout(Duration::from_millis(600)).map_err(|_| "KWin didn't report the pointer".to_string());
    if let Ok(s) = scripting(conn) { let _: Result<bool, _> = s.call("unloadScript", &(POINTER_PLUGIN,)); }
    parse_pointer(&reply?).ok_or_else(|| "KWin reported a pointer we couldn't read".into())
}

/// Registers the global shortcut. It stays until KWin restarts or `unregister_shortcut`.
pub fn register_shortcut(conn: &Connection, keys: &str) -> Result<(), String> {
    let js = format!(r#"registerShortcut("Tlum", "tlum: word under pointer", "{keys}", {});"#, report_js("TriggerAt"));
    run_script(conn, SHORTCUT_PLUGIN, &js)
}

pub fn unregister_shortcut(conn: &Connection) {
    if let Ok(s) = scripting(conn) { let _: Result<bool, _> = s.call("unloadScript", &(SHORTCUT_PLUGIN,)); }
}

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills two fresh descriptors we take ownership of.
    #[allow(unsafe_code)]
    unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 { return Err(std::io::Error::last_os_error()); }
        use std::os::fd::FromRawFd;
        Ok((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])))
    }
}

fn get_u32(map: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    let v = map.get(key)?;
    u32::try_from(v).ok().or_else(|| i32::try_from(v).ok().map(|i| i as u32))
}

fn get_f64(map: &HashMap<String, OwnedValue>, key: &str) -> Option<f64> {
    let v = map.get(key)?;
    f64::try_from(v).ok().or_else(|| u32::try_from(v).ok().map(f64::from)).or_else(|| i32::try_from(v).ok().map(f64::from))
}

/// QImage formats KWin sends, to straight RGBA.
fn to_rgba(data: &[u8], width: u32, height: u32, stride: u32, format: u32) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let row = data.get((y * stride) as usize..(y * stride + width * 4) as usize)?;
        for p in row.chunks_exact(4) {
            let (r, g, b, a) = match format {
                // ARGB32 / RGB32 / ARGB32_Premultiplied: 0xAARRGGBB, little-endian BGRA bytes.
                4..=6 => (p[2], p[1], p[0], if format == 4 { 255 } else { p[3] }),
                // RGBX8888 / RGBA8888 / RGBA8888_Premultiplied: bytes in order.
                16..=18 => (p[0], p[1], p[2], if format == 16 { 255 } else { p[3] }),
                _ => return None,
            };
            // Unpremultiply; a screenshot is opaque almost everywhere anyway.
            let un = |c: u8| if (format == 6 || format == 18) && a > 0 && a < 255 { ((c as u32 * 255) / a as u32).min(255) as u8 } else { c };
            out.extend_from_slice(&[un(r), un(g), un(b), 255]);
        }
    }
    Some(out)
}

/// A rect of the screen, in global logical coordinates, at the output's native resolution.
pub fn capture_area(conn: &Connection, rect: [i32; 4]) -> Result<Shot, String> {
    let proxy = Proxy::new(conn, "org.kde.KWin", "/org/kde/KWin/ScreenShot2", "org.kde.KWin.ScreenShot2").map_err(|e| e.to_string())?;
    let (read, write) = pipe().map_err(|e| e.to_string())?;
    let reader = std::thread::spawn(move || {
        let mut f = std::fs::File::from(read);
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map(|_| buf)
    });
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("include-cursor", Value::from(false));
    options.insert("native-resolution", Value::from(true));
    let fd = zbus::zvariant::Fd::from(&write);
    let reply: zbus::Result<HashMap<String, OwnedValue>> =
        proxy.call("CaptureArea", &(rect[0], rect[1], rect[2] as u32, rect[3] as u32, options, fd));
    drop(write);
    let meta = reply.map_err(|e| match &e {
        zbus::Error::MethodError(name, _, _) if name.as_str().contains("NoAuthorized") || name.as_str().contains("NotAuthorized") =>
            "KWin won't share the screen with this executable; run `tlum install` so its desktop file grants org.kde.KWin.ScreenShot2".to_string(),
        _ => format!("KWin screenshot failed: {e}"),
    })?;
    let data = reader.join().map_err(|_| "screenshot reader panicked")?.map_err(|e| e.to_string())?;
    if std::env::var_os("TLUM_DEBUG").is_some() {
        let nonzero = data.iter().filter(|b| **b != 0).count();
        eprintln!("tlum: capture {rect:?} → {meta:?}, {} bytes ({nonzero} non-zero)", data.len());
    }
    let (w, h) = (get_u32(&meta, "width").ok_or("no width")?, get_u32(&meta, "height").ok_or("no height")?);
    let stride = get_u32(&meta, "stride").unwrap_or(w * 4);
    let format = get_u32(&meta, "format").unwrap_or(5);
    let rgba = to_rgba(&data, w, h, stride, format).ok_or_else(|| format!("unexpected screenshot format {format} ({} bytes for {w}x{h})", data.len()))?;
    let scale = get_f64(&meta, "scale").filter(|s| *s > 0.0).unwrap_or(w as f64 / rect[2].max(1) as f64);
    Ok(Shot { width: w, height: h, rgba, origin: [rect[0] as f64, rect[1] as f64], scale })
}

/// Spectacle's non-interactive full-screen capture, cropped: slower, but needs no permission.
pub fn capture_with_spectacle(rect: [i32; 4], screen: [f64; 4], scale: f64) -> Result<Shot, String> {
    let file = script_dir().join("spectacle.png");
    let _ = std::fs::remove_file(&file);
    let status = crate::gpu::scrub(std::process::Command::new("spectacle").args(["-b", "-n", "-m", "-o"]).arg(&file))
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status()
        .map_err(|e| format!("spectacle isn't available: {e}"))?;
    if !status.success() { return Err("spectacle failed".into()); }
    let full = crate::image::decode_png(&std::fs::read(&file).map_err(|e| e.to_string())?)?;
    let _ = std::fs::remove_file(&file);
    let k = full.0 as f64 / screen[2].max(1.0);
    let k = if k.is_finite() && k > 0.0 { k } else { scale };
    let crop = [((rect[0] as f64 - screen[0]) * k) as i64, ((rect[1] as f64 - screen[1]) * k) as i64, (rect[2] as f64 * k) as i64, (rect[3] as f64 * k) as i64];
    let (x0, y0) = (crop[0].clamp(0, full.0 as i64) as u32, crop[1].clamp(0, full.1 as i64) as u32);
    let (x1, y1) = (((crop[0] + crop[2]).clamp(0, full.0 as i64)) as u32, ((crop[1] + crop[3]).clamp(0, full.1 as i64)) as u32);
    if x1 <= x0 || y1 <= y0 { return Err("the pointer is off the captured screen".into()); }
    let mut rgba = Vec::with_capacity(((x1 - x0) * (y1 - y0) * 4) as usize);
    for y in y0..y1 { rgba.extend_from_slice(&full.2[((y * full.0 + x0) * 4) as usize..((y * full.0 + x1) * 4) as usize]); }
    Ok(Shot { width: x1 - x0, height: y1 - y0, rgba, origin: [screen[0] + x0 as f64 / k, screen[1] + y0 as f64 / k], scale: k })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_reports_parse() {
        assert_eq!(parse_pointer("10.5,20"), Some(Pointer { x: 10.5, y: 20.0, output: None, screen: None, scale: 1.0, area: None }));
        let p = parse_pointer("1930,40,DP-2,1920,0,2560,1440,1.5").unwrap();
        assert_eq!(p.area, None);
        let p = parse_pointer("1930,40,DP-2,1920,0,2560,1440,1.5,1920,33,2560,1407").unwrap();
        assert_eq!(p.area, Some([1920.0, 33.0, 2560.0, 1407.0]));
        assert_eq!(p.output.as_deref(), Some("DP-2"));
        assert_eq!(p.screen, Some([1920.0, 0.0, 2560.0, 1440.0]));
        assert_eq!(p.scale, 1.5);
        assert_eq!(parse_pointer("nope"), None);
    }

    #[test]
    fn qimage_formats_become_rgba() {
        let bgra = [10, 20, 30, 255, 1, 2, 3, 255];
        assert_eq!(to_rgba(&bgra, 2, 1, 8, 5).unwrap(), vec![30, 20, 10, 255, 3, 2, 1, 255]);
        let rgba = [10, 20, 30, 0];
        assert_eq!(to_rgba(&rgba, 1, 1, 4, 16).unwrap(), vec![10, 20, 30, 255]);
        assert!(to_rgba(&rgba, 1, 1, 4, 99).is_none());
    }
}
