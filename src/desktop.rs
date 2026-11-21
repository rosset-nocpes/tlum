//! Where the pointer is and what's on screen there, per desktop. Everything here speaks
//! global logical points (what Wayland compositors call logical coordinates; X pixels divided
//! by the desktop's scale on X11).
//!
//! | Backend | Pointer | Pixels | Shortcut |
//! | --- | --- | --- | --- |
//! | KWin (KDE Plasma, Wayland) | KWin script | `ScreenShot2` | KWin script |
//! | X11 (any window manager) | `QueryPointer` | `GetImage` | `XGrabKey` |
//! | Wayland (sway, Hyprland, niri, COSMIC, …) | Hyprland IPC, else a layer-shell probe | `ext-image-copy-capture` / `wlr-screencopy` | compositor binding |
//! | GNOME (Wayland) | XWayland probe | Screenshot portal | custom shortcut (`tlum install`) |

use std::sync::mpsc::Receiver;
use std::time::Duration;

use zbus::blocking::Connection;

use crate::engine::Screen;
use crate::kwin::{self, Pointer};
use crate::scan::Shot;

pub trait Desktop: Send {
    fn name(&self) -> String;
    /// The pointer; the engine fills in the screen if the backend can't say.
    fn pointer(&mut self) -> Result<Pointer, String>;
    /// `rect` (global points) on the screen `p` is on.
    fn capture(&mut self, rect: [i32; 4], p: &Pointer) -> Result<Shot, String>;
    fn selection(&mut self) -> Option<String> { crate::selection::primary() }
    /// Whether overlays here are opaque (X11 with no compositor), and so need the screen
    /// drawn behind them.
    fn opaque_overlays(&mut self) -> bool { false }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { KWin, X11, Wayland }

/// Which backend fits this session (`choice` is the config's `backend`: auto, kwin, x11, wayland).
pub fn kind(choice: &str, bus: Option<&Connection>) -> Kind {
    match choice {
        "kwin" => return Kind::KWin,
        "x11" => return Kind::X11,
        "wayland" => return Kind::Wayland,
        _ => {}
    }
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
    if !wayland && std::env::var_os("DISPLAY").is_some() { return Kind::X11; }
    if bus.is_some_and(kwin::is_kwin) { Kind::KWin } else { Kind::Wayland }
}

pub fn connect(kind: Kind, bus: Option<Connection>, replies: Receiver<String>, screens: &[Screen]) -> Result<Box<dyn Desktop>, String> {
    Ok(match kind {
        Kind::KWin => Box::new(KWin { bus: bus.ok_or("KWin needs the session bus")?, replies }),
        Kind::X11 => Box::new(X11 { x: crate::x11::X11::connect()?, scale: screens.first().map_or(1.0, |s| s.scale) }),
        Kind::Wayland => {
            let wl = crate::wl::Wayland::connect().map_err(|e| eprintln!("tlum: {e}")).ok();
            let xwayland = if wl.as_ref().is_some_and(|w| w.can_probe()) || crate::hypr::available() { None } else { crate::x11::X11::connect().ok() };
            if crate::hypr::available() { crate::hypr::quiet_layers(); }
            Box::new(Wayland { wl, xwayland, bus, screens: screens.to_vec() })
        }
    })
}

// --- KDE Plasma ---------------------------------------------------------------------------

struct KWin { bus: Connection, replies: Receiver<String> }

impl Desktop for KWin {
    fn name(&self) -> String { "KWin".into() }
    fn pointer(&mut self) -> Result<Pointer, String> { kwin::pointer(&self.bus, &self.replies) }
    fn capture(&mut self, rect: [i32; 4], p: &Pointer) -> Result<Shot, String> {
        match kwin::capture_area(&self.bus, rect) {
            Ok(shot) => Ok(shot),
            Err(e) => {
                eprintln!("tlum: {e}; trying Spectacle");
                let screen = p.screen.unwrap_or([0.0, 0.0, 1920.0, 1080.0]);
                kwin::capture_with_spectacle(rect, screen, p.scale)
            }
        }
    }
}

// --- X11 ----------------------------------------------------------------------------------

struct X11 { x: crate::x11::X11, scale: f64 }

impl Desktop for X11 {
    fn name(&self) -> String { "X11".into() }
    fn pointer(&mut self) -> Result<Pointer, String> {
        let (x, y) = self.x.pointer()?;
        Ok(Pointer { x: x as f64 / self.scale, y: y as f64 / self.scale, output: None, screen: None, scale: self.scale, area: None })
    }
    fn capture(&mut self, rect: [i32; 4], _: &Pointer) -> Result<Shot, String> {
        let k = self.scale;
        let px = [(rect[0] as f64 * k) as i32, (rect[1] as f64 * k) as i32, (rect[2] as f64 * k).ceil() as i32, (rect[3] as f64 * k).ceil() as i32];
        // The image may be clipped to a window (rootless X): keep its own origin.
        let mut shot = self.x.capture(px)?;
        shot.origin = [shot.origin[0] / k, shot.origin[1] / k];
        shot.scale = k;
        Ok(shot)
    }
    fn selection(&mut self) -> Option<String> { self.x.primary(Duration::from_millis(250)) }
    fn opaque_overlays(&mut self) -> bool { !self.x.composited() }
}

// --- other Wayland compositors ------------------------------------------------------------

struct Wayland {
    wl: Option<crate::wl::Wayland>,
    /// GNOME: no layer-shell, so the pointer comes from XWayland.
    xwayland: Option<crate::x11::X11>,
    bus: Option<Connection>,
    screens: Vec<Screen>,
}

impl Desktop for Wayland {
    fn name(&self) -> String {
        let how = self.wl.as_ref().map(|w| w.protocols()).unwrap_or_default();
        let pointer = if crate::hypr::available() { "Hyprland IPC" } else if self.wl.as_ref().is_some_and(|w| w.can_probe()) { "layer-shell probe" } else { "XWayland probe" };
        format!("Wayland ({how}; pointer: {pointer}{})", if self.wl.as_ref().is_some_and(|w| w.can_capture()) { "" } else { "; pixels: Screenshot portal" })
    }

    fn pointer(&mut self) -> Result<Pointer, String> {
        if crate::hypr::available() {
            let (x, y) = crate::hypr::cursor()?;
            let mon = crate::hypr::monitor_at((x, y)).ok();
            return Ok(Pointer { x, y, output: mon.as_ref().map(|m| m.0.clone()), screen: mon.as_ref().map(|m| m.1), scale: 1.0, area: mon.map(|m| m.2) });
        }
        if let Some(wl) = self.wl.as_mut().filter(|w| w.can_probe()) {
            let (output, x, y) = wl.probe_pointer(Duration::from_secs(4))?;
            let screen = wl.outputs().into_iter().find(|o| o.name == output).map(|o| o.rect());
            return Ok(Pointer { x, y, output, screen, scale: 1.0, area: None });
        }
        let x = self.xwayland.as_ref().ok_or("can't find the pointer: no layer-shell and no XWayland")?;
        let (px, py) = x.probe_pointer(Duration::from_secs(4))?;
        Ok(Pointer { x: px as f64, y: py as f64, output: None, screen: None, scale: 1.0, area: None })
    }

    fn capture(&mut self, rect: [i32; 4], p: &Pointer) -> Result<Shot, String> {
        if let Some(wl) = self.wl.as_mut().filter(|w| w.can_capture()) {
            return wl.capture(rect, p.output.as_deref(), [p.x, p.y]);
        }
        let bus = self.bus.as_ref().ok_or("no session bus for the Screenshot portal")?;
        let (w, h, rgba) = crate::portal::screenshot(bus)?;
        // The portal's image covers every screen.
        let union = self.screens.iter().fold(None::<[f64; 4]>, |u, s| Some(match u {
            None => s.rect,
            Some(u) => { let (x0, y0) = (u[0].min(s.rect[0]), u[1].min(s.rect[1])); [x0, y0, (u[0] + u[2]).max(s.rect[0] + s.rect[2]) - x0, (u[1] + u[3]).max(s.rect[1] + s.rect[3]) - y0] }
        })).unwrap_or([0.0, 0.0, w as f64, h as f64]);
        crate::wl::crop(w, h, rgba, union, rect)
    }

    fn selection(&mut self) -> Option<String> {
        crate::selection::primary().or_else(|| self.xwayland.as_ref().and_then(|x| x.primary(Duration::from_millis(250))))
    }
}
