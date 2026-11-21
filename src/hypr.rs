//! Hyprland's IPC socket: the global pointer (`cursorpos`) and each monitor's reserved edges
//! (where bars keep windows out), which also keep our overlay out.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn socket() -> Option<std::path::PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| "/tmp".into());
    [runtime.join("hypr").join(&sig).join(".socket.sock"), std::path::PathBuf::from("/tmp/hypr").join(&sig).join(".socket.sock")]
        .into_iter().find(|p| p.exists())
}

pub fn available() -> bool { socket().is_some() }

fn ask(cmd: &str) -> Result<serde_json::Value, String> {
    let mut s = UnixStream::connect(socket().ok_or("not Hyprland")?).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_millis(500))).ok();
    s.write_all(format!("j/{cmd}").as_bytes()).map_err(|e| e.to_string())?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| e.to_string())?;
    serde_json::from_str(&out).map_err(|e| format!("hyprctl {cmd}: {e}"))
}

/// The pointer in global logical coordinates.
pub fn cursor() -> Result<(f64, f64), String> {
    let v = ask("cursorpos")?;
    Ok((v["x"].as_f64().ok_or("no x")?, v["y"].as_f64().ok_or("no y")?))
}

/// The monitor containing `at`: its name, logical rect, and the rect left after reserved
/// edges.
pub fn monitor_at(at: (f64, f64)) -> Result<(String, [f64; 4], [f64; 4]), String> {
    let v = ask("monitors")?;
    for m in v.as_array().ok_or("no monitors")? {
        let f = |k: &str| m[k].as_f64().unwrap_or(0.0);
        let scale = f("scale").max(0.1);
        let (w, h) = if m["transform"].as_i64().unwrap_or(0) % 2 == 1 { (f("height"), f("width")) } else { (f("width"), f("height")) };
        let r = [f("x"), f("y"), w / scale, h / scale];
        if at.0 >= r[0] && at.1 >= r[1] && at.0 < r[0] + r[2] && at.1 < r[1] + r[3] {
            let res: Vec<f64> = m["reserved"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
            let [l, t, rr, b] = [0, 1, 2, 3].map(|i| res.get(i).copied().unwrap_or(0.0));
            return Ok((m["name"].as_str().unwrap_or_default().to_string(), r, [r[0] + l, r[1] + t, r[2] - l - rr, r[3] - t - b]));
        }
    }
    Err("no monitor under the pointer".into())
}

fn send(cmd: &str) -> Result<String, String> {
    let mut s = UnixStream::connect(socket().ok_or("not Hyprland")?).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_millis(500))).ok();
    s.write_all(cmd.as_bytes()).map_err(|e| e.to_string())?;
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    Ok(out)
}

/// Hyprland animates layer surfaces by default (a pop-in from the middle), which would carry
/// our full-screen overlay's marker and popover with it. Its rules can turn that off per
/// namespace; the syntax changed in 0.53, so try both.
pub fn quiet_layers() {
    for rule in ["keyword layerrule no_anim on, match:namespace ^(tlum)$", "keyword layerrule noanim, ^(tlum)$"] {
        if send(rule).is_ok_and(|r| r.trim() == "ok") { break; }
    }
}
