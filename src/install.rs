//! `tlum install`, `uninstall`, `doctor` and `ocr`.

use std::path::PathBuf;

use crate::config::Config;

const DESKTOP_ID: &str = "dev.tlum.Tlum.desktop";

fn home() -> PathBuf { PathBuf::from(std::env::var_os("HOME").unwrap_or_default()) }
fn data_home() -> PathBuf { std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")) }
fn config_home() -> PathBuf { std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")) }
fn bin_path() -> PathBuf { home().join(".local/bin/tlum") }

fn desktop_entry(exec: &str, autostart: bool) -> String {
    format!("[Desktop Entry]
Type=Application
Name=tlum
GenericName=Dictionary popover
Comment=Force-click, three-finger tap or press and hold a word to look it up
Exec={exec}
Icon=accessories-dictionary
Terminal=false
Categories=Utility;Dictionary;
NoDisplay={}
X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2
{}",
        autostart, if autostart { "X-KDE-autostart-phase=2\n" } else { "" })
}

fn write(path: &PathBuf, contents: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?; }
    std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn install() -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?.canonicalize().map_err(|e| e.to_string())?;
    let bin = bin_path();
    if me != bin {
        if let Some(dir) = bin.parent() { std::fs::create_dir_all(dir).map_err(|e| e.to_string())?; }
        // Replace rather than overwrite, so a running daemon keeps its executable.
        let tmp = bin.with_extension("new");
        std::fs::copy(&me, &tmp).map_err(|e| format!("copying to {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &bin).map_err(|e| e.to_string())?;
    }
    let exec = bin.to_string_lossy().into_owned();
    let app = data_home().join("applications").join(DESKTOP_ID);
    let autostart = config_home().join("autostart").join(DESKTOP_ID);
    write(&app, &desktop_entry(&exec, false))?;
    write(&autostart, &desktop_entry(&exec, true))?;
    // KWin checks the screenshot grant through KService's cache of desktop files.
    for tool in ["kbuildsycoca6", "kbuildsycoca5"] {
        if std::process::Command::new(tool).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok() { break; }
    }
    let kind = crate::desktop::kind(&Config::load().backend, zbus::blocking::Connection::session().ok().as_ref());
    if kind == crate::desktop::Kind::Wayland && desktop_name().contains("gnome") {
        match gnome_shortcut(&exec) {
            Ok(()) => println!("Added the shortcut Super+Ctrl+D (Settings → Keyboard → Custom Shortcuts)"),
            Err(e) => println!("Couldn't add a GNOME shortcut: {e}"),
        }
    }
    println!("Installed {exec}");
    println!("  {}  (grants KWin screenshots to this executable)", app.display());
    println!("  {}  (starts at login)", autostart.display());
    if !in_input_group() {
        println!("\nTo use touchpad and mouse gestures, let tlum read input devices:\n  sudo usermod -aG input $USER   (then log out and back in)");
    }
    if kind == crate::desktop::Kind::Wayland && !desktop_name().contains("gnome") {
        println!("\nKeyboard shortcut and start at login: {}", shortcut_hint());
    }
    println!("\nStart it now with:  tlum &   — or log out and back in.");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    for p in [data_home().join("applications").join(DESKTOP_ID), config_home().join("autostart").join(DESKTOP_ID), bin_path()] {
        if std::fs::remove_file(&p).is_ok() { println!("removed {}", p.display()); }
    }
    let _ = std::process::Command::new("kbuildsycoca6").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    if let Ok(conn) = zbus::blocking::Connection::session() { crate::kwin::unregister_shortcut(&conn); }
    Ok(())
}

fn desktop_name() -> String { std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase() }

/// How to give tlum a keyboard shortcut where it can't take one itself.
pub fn shortcut_hint() -> String {
    let env = |k: &str| std::env::var_os(k).is_some();
    if env("HYPRLAND_INSTANCE_SIGNATURE") {
        "add to hyprland.conf:  bind = SUPER CTRL, D, exec, tlum trigger   and   exec-once = tlum".into()
    } else if env("SWAYSOCK") {
        "add to sway's config:  bindsym Mod4+Ctrl+d exec tlum trigger   and   exec tlum".into()
    } else if env("NIRI_SOCKET") {
        "add to niri's config:  binds { Mod+Ctrl+D { spawn \"tlum\" \"trigger\"; } }   and   spawn-at-startup \"tlum\"".into()
    } else if desktop_name().contains("gnome") {
        "`tlum install` adds Super+Ctrl+D under Settings → Keyboard → Custom Shortcuts".into()
    } else {
        "bind `tlum trigger` to a key in your desktop's keyboard settings".into()
    }
}

/// GNOME: a custom shortcut running `tlum trigger`.
fn gnome_shortcut(exec: &str) -> Result<(), String> {
    const LIST: (&str, &str) = ("org.gnome.settings-daemon.plugins.media-keys", "custom-keybindings");
    const PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/tlum/";
    let run = |args: &[&str]| -> Result<String, String> {
        let out = std::process::Command::new("gsettings").args(args).output().map_err(|e| format!("gsettings: {e}"))?;
        if !out.status.success() { return Err(String::from_utf8_lossy(&out.stderr).trim().to_string()); }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let current = run(&["get", LIST.0, LIST.1])?;
    let mut paths: Vec<String> = current.trim_start_matches("@as").trim().trim_matches(['[', ']']).split(',')
        .map(|p| p.trim().trim_matches('\'').to_string()).filter(|p| !p.is_empty()).collect();
    if !paths.iter().any(|p| p == PATH) { paths.push(PATH.into()); }
    let list = format!("[{}]", paths.iter().map(|p| format!("'{p}'")).collect::<Vec<_>>().join(", "));
    run(&["set", LIST.0, LIST.1, &list])?;
    let schema = format!("{}.custom-keybinding:{PATH}", LIST.0);
    run(&["set", &schema, "name", "tlum"])?;
    run(&["set", &schema, "command", &format!("{exec} trigger")])?;
    run(&["set", &schema, "binding", "<Super><Control>d"])?;
    Ok(())
}

fn in_input_group() -> bool {
    std::fs::read_dir("/dev/input").ok().into_iter().flatten().flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("event") && std::fs::File::open(e.path()).is_ok())
}

pub fn doctor() {
    let ok = |b: bool| if b { "ok  " } else { "FAIL" };
    println!("session      {} {}", std::env::var("XDG_SESSION_TYPE").unwrap_or_default(), std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default());
    let bus = zbus::blocking::Connection::session().ok();
    let cfg0 = Config::load();
    let kind = crate::desktop::kind(&cfg0.backend, bus.as_ref());
    let (_tx, rx) = std::sync::mpsc::channel();
    let screens: Vec<crate::engine::Screen> = Vec::new();
    match crate::desktop::connect(kind, bus.clone(), rx, &screens) {
        Ok(mut d) => {
            println!("[ok  ] backend: {}", d.name());
            if kind != crate::desktop::Kind::KWin {
                match d.pointer() {
                    Ok(p) => {
                        println!("[ok  ] pointer at {:.0},{:.0}{}", p.x, p.y, p.output.as_ref().map(|o| format!(" on {o}")).unwrap_or_default());
                        match d.capture([p.x as i32 - 4, p.y as i32 - 4, 8, 8], &p) {
                            Ok(s) => println!("[ok  ] screen capture ({}x{} at scale {})", s.width, s.height, s.scale),
                            Err(e) => println!("[FAIL] screen capture: {e}"),
                        }
                    }
                    Err(e) => println!("[FAIL] pointer: {e}"),
                }
                println!("       shortcut: {}", if kind == crate::desktop::Kind::X11 { "built in (X11 key grab)".to_string() } else { shortcut_hint() });
            }
        }
        Err(e) => println!("[FAIL] backend {kind:?}: {e}"),
    }
    let (devices, denied) = crate::input::survey();
    println!("[{}] input devices: {}{}", ok(!devices.is_empty()), devices.len(), if denied > 0 && devices.is_empty() { format!(" ({denied} denied: join the `input` group)") } else { String::new() });
    for (path, name, kind) in &devices { println!("       {kind:8} {name}  {path}"); }
    let cfg = Config::load();
    match crate::ocr::Ocr::new(&cfg.ocr_language) {
        Ok(_) => println!("[ok  ] tesseract ({}, {})", cfg.ocr_language, crate::ocr::tessdata_dir(cfg.ocr_language.split('+').next().unwrap_or("eng")).unwrap_or_default()),
        Err(e) => println!("[FAIL] tesseract: {e}"),
    }
    let conn = zbus::blocking::Connection::session();
    match &conn {
        Ok(c) => {
            if kind == crate::desktop::Kind::KWin {
                match crate::kwin::capture_area(c, [0, 0, 8, 8]) {
                    Ok(s) => println!("[ok  ] KWin screenshots ({}x{} at scale {})", s.width, s.height, s.scale),
                    Err(e) => println!("[FAIL] KWin screenshots: {e}"),
                }
            }
            let running = zbus::blocking::fdo::DBusProxy::new(c).ok().and_then(|d| d.name_has_owner(crate::kwin::SERVICE.try_into().unwrap()).ok()).unwrap_or(false);
            println!("[{}] daemon running", ok(running));
        }
        Err(e) => println!("[FAIL] session bus: {e}"),
    }
    match crate::selection::primary() {
        Some(s) => println!("[ok  ] primary selection ({} characters selected)", s.chars().count()),
        None => println!("[ -- ] primary selection: nothing selected, or the compositor lacks data-control"),
    }
    let s = cfg.gestures();
    println!("gestures     hold {} ms · three-finger tap {} · force click {} · mouse hold {} · mouse button {:?} · natural scroll {}",
        s.hold_ms, s.three_finger_tap, s.force_click, s.mouse_hold, cfg.mouse_button, s.natural_scroll);
}

/// Recognises `path` (a screenshot) and, given a point in its pixels, the word there.
pub fn ocr_file(path: &str, x: Option<f64>, y: Option<f64>) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let (w, h, rgba) = crate::image::decode_png(&bytes)?;
    let shot = crate::scan::Shot { width: w, height: h, rgba, origin: [0.0, 0.0], scale: 1.0 };
    let at = [x.unwrap_or(w as f64 / 2.0), y.unwrap_or(h as f64 / 2.0)];
    let cfg = Config::load();
    let mut ocr = crate::ocr::Ocr::new(&cfg.ocr_language)?;
    let t = std::time::Instant::now();
    let prepared = crate::scan::prepare(&shot, at);
    let words = ocr.words(&prepared.gray, prepared.width, prepared.height);
    println!("{} words in {} ms", words.len(), t.elapsed().as_millis());
    for wd in &words { println!("  line {:2} {:5.1}% {:?} {:?}", wd.line, wd.conf, wd.bbox.map(|v| (v as f64 / prepared.per_point) as i32), wd.text); }
    match crate::scan::pick(&words, shot.origin, prepared.per_point, at) {
        Some(hit) => println!("at {:?}: {:?} {:?}", at, hit.query, hit.rect),
        None => println!("at {at:?}: no word"),
    }
    Ok(())
}
