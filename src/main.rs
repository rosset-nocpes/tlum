//! tlum: a dictionary popover for Linux inspired by macOS Look Up.
//!
//! Force-click a word (press a touchpad down and hold it still), tap it with three fingers,
//! press and hold it with a mouse, or press Meta+Ctrl+D over it, and a popover shows its
//! definition, synonyms and Wikipedia article, with the word marked in yellow.

#![deny(unsafe_op_in_unsafe_fn)]

mod config;
mod desktop;
mod dict;
mod engine;
mod gesture;
mod hypr;
mod image;
mod input;
mod install;
mod kwin;
mod ocr;
mod portal;
mod preview;
mod scan;
mod selection;
mod service;
mod style;
mod ui;
mod wl;
mod x11;

use std::sync::Mutex;

use rune::{App, DisplayTarget, KeyPolicy, OverlayInput, OverlayLevel, OverlayOptions, OverlayPlacement, OverlaySpec, RunOptions, WindowSpec};

use crate::config::Config;
use crate::engine::{Engine, Job, Screen};
use crate::gesture::Effect;
use crate::ui::Popover;

const USAGE: &str = "\
tlum — macOS-style Look Up for Linux

USAGE:
    tlum [run]          start the daemon (one per session)
    tlum trigger        look up the word under the pointer (bind it to a key anywhere)
    tlum define WORD…   show the popover for WORD at the pointer
    tlum close          close the popover
    tlum install        install to ~/.local/bin, grant KWin screenshots, start at login
    tlum uninstall      undo install
    tlum doctor         check what works on this desktop
    tlum ocr IMAGE.png [X Y]   recognise words in an image (testing)
    tlum config         print the settings in effect
    tlum style          check ~/.config/tlum/style.css (the popover's CSS)
    tlum style --init   start that file from the built-in style
    tlum style --default   print the built-in style

TRIGGERS:
    touchpad   force click: press the pad and hold it still · three-finger tap
    mouse      press and hold the left button still · optional side button
    keyboard   Meta+Ctrl+D (System Settings → Shortcuts → KWin to change)

Settings: ~/.config/tlum/config.json";

fn call(method: &str, body: Option<String>) -> Result<(), String> {
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let proxy = zbus::blocking::Proxy::new(&conn, kwin::SERVICE, kwin::PATH, kwin::SERVICE).map_err(|e| e.to_string())?;
    let r: zbus::Result<()> = match body { Some(b) => proxy.call(method, &(b,)), None => proxy.call(method, &()) };
    r.map_err(|e| match e {
        zbus::Error::MethodError(n, _, _) if n.as_str().ends_with("ServiceUnknown") => "tlum isn't running (start it with `tlum`)".into(),
        e => e.to_string(),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a.starts_with("--") && a != "--help") {
        return preview::run();
    }
    let result = match args.first().map(String::as_str) {
        None | Some("run") => { daemon(); Ok(()) }
        Some("trigger") => call("Trigger", None),
        Some("define") if args.len() > 1 => call("LookUp", Some(args[1..].join(" "))),
        Some("close") => call("Close", None),
        Some("install") => install::install(),
        Some("uninstall") => install::uninstall(),
        Some("doctor") => { install::doctor(); Ok(()) }
        Some("ocr") if args.len() > 1 => install::ocr_file(&args[1], args.get(2).and_then(|a| a.parse().ok()), args.get(3).and_then(|a| a.parse().ok())),
        Some("capture") if args.len() == 5 => {
            let n: Vec<i32> = args[1..5].iter().filter_map(|a| a.parse().ok()).collect();
            zbus::blocking::Connection::session().map_err(|e| e.to_string())
                .and_then(|c| kwin::capture_area(&c, [n[0], n[1], n[2], n[3]]))
                .map(|s| println!("{}x{} scale {} first pixels {:?}", s.width, s.height, s.scale, &s.rgba[..16.min(s.rgba.len())]))
        }
        // Debugging: where the backend finds the pointer.
        Some("probe") => (|| {
            let bus = zbus::blocking::Connection::session().ok();
            let kind = desktop::kind(&Config::load().backend, bus.as_ref());
            let (_tx, rx) = std::sync::mpsc::channel();
            let mut d = desktop::connect(kind, bus, rx, &[])?;
            let t = std::time::Instant::now();
            let p = d.pointer()?;
            println!("{}: {p:?} in {} ms", d.name(), t.elapsed().as_millis());
            Ok(())
        })(),
        // Debugging: `capture-file X Y W H OUT.png` saves one capture.
        Some("capture-file") if args.len() == 6 => (|| {
            let n: Vec<i32> = args[1..5].iter().filter_map(|a| a.parse().ok()).collect();
            let bus = zbus::blocking::Connection::session().ok();
            let kind = desktop::kind(&Config::load().backend, bus.as_ref());
            let (_tx, rx) = std::sync::mpsc::channel();
            let mut d = desktop::connect(kind, bus, rx, &[])?;
            let at = kwin::Pointer { x: (n[0] + n[2] / 2) as f64, y: (n[1] + n[3] / 2) as f64, output: None, screen: None, scale: 1.0, area: None };
            let s = d.capture([n[0], n[1], n[2], n[3]], &at)?;
            println!("origin {} {} scale {}", s.origin[0], s.origin[1], s.scale);
            image::write_rgba_png(std::path::Path::new(&args[5]), &s.rgba, s.width, s.height)
        })(),
        // Debugging: `film X Y W H FRAMES DIR` saves rapid captures, to watch an animation.
        Some("film") if args.len() == 7 => (|| {
            let n: Vec<i32> = args[1..6].iter().filter_map(|a| a.parse().ok()).collect();
            let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
            let start = std::time::Instant::now();
            for i in 0..n[4] {
                let s = kwin::capture_area(&conn, [n[0], n[1], n[2], n[3]])?;
                let t = start.elapsed().as_millis();
                image::write_rgba_png(&std::path::Path::new(&args[6]).join(format!("{i:03}-{t:04}ms.png")), &s.rgba, s.width, s.height)?;
            }
            Ok(())
        })(),
        Some("style") => style_command(args.get(1).map(String::as_str)),
        Some("config") => { println!("{}", serde_json::to_string_pretty(&Config::load()).unwrap()); Ok(()) }
        Some("help" | "-h" | "--help") => { println!("{USAGE}"); Ok(()) }
        _ => { eprintln!("{USAGE}"); std::process::exit(2) }
    };
    if let Err(e) = result {
        eprintln!("tlum: {e}");
        std::process::exit(1);
    }
}

fn style_command(arg: Option<&str>) -> Result<(), String> {
    let path = style::user_path();
    match arg {
        Some("--default") => { print!("{}", style::DEFAULT_CSS); Ok(()) }
        Some("--path") => { println!("{}", path.display()); Ok(()) }
        Some("--init") => {
            if path.exists() { return Err(format!("{} already exists", path.display())); }
            if let Some(dir) = path.parent() { std::fs::create_dir_all(dir).map_err(|e| e.to_string())?; }
            std::fs::write(&path, style::DEFAULT_CSS).map_err(|e| e.to_string())?;
            println!("Wrote {}: edit it and save; a running tlum restyles at once.", path.display());
            Ok(())
        }
        None | Some("--check") => {
            let user = std::fs::read_to_string(&path).ok();
            let (_, _, warnings) = style::build(user.as_deref());
            match (&user, warnings.is_empty()) {
                (None, _) => println!("No {} yet: the built-in style is in use (`tlum style --init` to start one).", path.display()),
                (Some(_), true) => println!("{}: no problems.", path.display()),
                (Some(_), false) => {}
            }
            for w in &warnings { println!("{w}"); }
            if warnings.is_empty() { Ok(()) } else { Err(format!("{} problem(s)", warnings.len())) }
        }
        Some(other) => Err(format!("unknown option {other} (--check, --init, --default, --path)")),
    }
}

fn setup_images(s: &mut Popover, waker: rune::Waker) { s.images = Some(rune::ImageCache::new(waker)); }

fn daemon() {
    let config = Config::load();
    let conn = match zbus::blocking::Connection::session() {
        Ok(c) => c,
        Err(e) => { eprintln!("tlum: no D-Bus session bus: {e}"); std::process::exit(1) }
    };
    match conn.request_name_with_flags(kwin::SERVICE, zbus::fdo::RequestNameFlags::DoNotQueue.into()) {
        Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {}
        Ok(_) => { eprintln!("tlum is already running"); return; }
        Err(e) => { eprintln!("tlum: can't claim {}: {e}", kwin::SERVICE); std::process::exit(1) }
    }
    let kind = desktop::kind(&config.backend, Some(&conn));
    rune::set_app_id(kwin::SERVICE);
    rune::run_windows_with(RunOptions { accessory: true }, move |displays| {
        let mut app = App::new(preview::spec()).windowless();
        let mut handles = Vec::new();
        let mut screens = Vec::new();
        for (i, d) in displays.iter().enumerate() {
            let state = Popover { display: i, ..Popover::default() };
            let target = d.id.clone().map_or(DisplayTarget::Primary, DisplayTarget::Id);
            // The layer-shell namespace is also the window type KWin gives the surface: an
            // unknown one is a normal window, which KWin's Scale effect zooms open and shut
            // from the screen's centre, dragging the marker and popover with it. A dock isn't
            // animated, so only our own transitions play. Elsewhere it's ours, for rules
            // (Hyprland's `layerrule = noanim, tlum`, which we also set over IPC).
            let namespace = if kind == desktop::Kind::KWin { "dock" } else { "tlum" };
            // X11 reports pixels; everything else here is in points.
            let x11 = matches!(d.native_id(), Some(rune::NativeDisplayId::X11Crtc(_) | rune::NativeDisplayId::X11Output(_)));
            let rect = d.native_bounds().map(|v| if x11 { v / d.scale } else { v });
            let overlay = app.overlay(OverlaySpec::new(namespace, (rect[2] as f32, rect[3] as f32), ui::view).options(OverlayOptions {
                display: target, placement: Some(OverlayPlacement::FullScreen), level: OverlayLevel::AboveOverlays,
                input: OverlayInput::All, keys: KeyPolicy::OnDemand,
                // GNOME has no layer-shell: the overlay is an XWayland window, which may take
                // clicks only when asked.
                xwayland_input: true, ..Default::default()
            }).on_key(ui::on_key).setup(setup_images).animate_reveal(), state);
            handles.push(overlay);
            screens.push(Screen { name: d.name.clone(), rect, primary: d.primary, scale: d.scale });
        }
        eprintln!("tlum: displays {:?}", screens);
        let _ = ui::OVERLAYS.set(handles);

        let (replies_tx, replies_rx) = std::sync::mpsc::channel();
        let scale = screens.first().map_or(1.0, |s| s.scale);
        let engine = engine::start(config.clone(), kind, screens, replies_rx);
        let service = service::Service { engine: engine.clone(), pointer_replies: Mutex::new(replies_tx) };
        if let Err(e) = conn.object_server().at(kwin::PATH, service) { eprintln!("tlum: D-Bus service failed: {e}"); }
        register_shortcut(kind, &conn, &config.shortcut, &engine, scale);
        // The popover's CSS: report mistakes now, and restyle whenever it's saved.
        for w in style::reload() { eprintln!("tlum: {w}"); }
        style::watch(|| if let Some(all) = ui::OVERLAYS.get() { for o in all { o.update(|_, _| {}); } });
        let gestures = config.gestures();
        let e = engine.clone();
        std::thread::Builder::new().name("input".into()).spawn(move || input::run(gestures, move |fx, dev| on_gesture(&e, fx, dev))).unwrap();
        // Keep the connection (and with it the service and its name) for the life of the app.
        std::mem::forget(conn);
        vec![WindowSpec::from_app("tlum", app)]
    });
}

/// KWin and X11 take a global shortcut from us; other desktops bind `tlum trigger`.
fn register_shortcut(kind: desktop::Kind, conn: &zbus::blocking::Connection, keys: &str, engine: &Engine, scale: f64) {
    let result = match kind {
        desktop::Kind::KWin => kwin::register_shortcut(conn, keys).map(|_| "KWin"),
        desktop::Kind::X11 => {
            let e = engine.clone();
            x11::grab_shortcut(keys, move |x, y| {
                let p = kwin::Pointer { x: x as f64 / scale, y: y as f64 / scale, output: None, screen: None, scale, area: None };
                e.send(Job::Trigger(Some(p), "shortcut"));
            }).map(|_| "X11")
        }
        desktop::Kind::Wayland => Err(install::shortcut_hint()),
    };
    match result {
        Ok(by) => eprintln!("tlum: shortcut {keys} registered with {by}"),
        Err(e) => eprintln!("tlum: no built-in shortcut: {e}"),
    }
}

fn on_gesture(engine: &Engine, fx: Effect, device: &str) {
    match fx {
        Effect::Trigger(source) => {
            let why = match source {
                gesture::Source::ThreeFingerTap => "three-finger tap",
                gesture::Source::ForceClick => "force click",
                gesture::Source::MouseHold => "press and hold",
                gesture::Source::MouseButton => "mouse button",
            };
            let _ = device;
            engine.send(Job::Trigger(None, why));
        }
        Effect::Dragged => engine.dragged(),
        Effect::Scroll(dy) => {
            let Some((_, display)) = *ui::VISIBLE.lock().unwrap() else { return };
            if let Some(o) = ui::OVERLAYS.get().and_then(|v| v.get(display)) {
                o.update_if(move |s, tx| ui::scroll_by(s, dy, tx));
            }
        }
    }
}
