//! X11: the pointer, the screen's pixels, the PRIMARY selection and a global key grab, for X11
//! sessions under any window manager. On GNOME's Wayland session the same calls reach
//! XWayland, where a short-lived input window finds the pointer (`probe_pointer`) and Mutter
//! mirrors the primary selection.
//!
//! Coordinates here are X pixels; callers divide by the desktop's scale to get points.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt, CreateWindowAux, EventMask, GrabMode, ImageFormat, ModMask, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use crate::scan::Shot;

pub struct X11 {
    conn: RustConnection,
    root: Window,
    depth: u8,
}

impl X11 {
    pub fn connect() -> Result<X11, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| format!("no X display: {e}"))?;
        let s = &conn.setup().roots[screen];
        let (root, depth) = (s.root, s.root_depth);
        Ok(X11 { conn, root, depth })
    }

    /// The pointer in root pixels, as X last saw it.
    pub fn pointer(&self) -> Result<(i32, i32), String> {
        let r = self.conn.query_pointer(self.root).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
        Ok((r.root_x as i32, r.root_y as i32))
    }

    /// On XWayland the X pointer is stale while it's over Wayland windows. A transparent
    /// input-only window over everything gets an enter event where the pointer is (once any
    /// button held on a Wayland window is released).
    pub fn probe_pointer(&self, timeout: Duration) -> Result<(i32, i32), String> {
        let c = &self.conn;
        let s = &c.setup().roots[0];
        let win = c.generate_id().map_err(|e| e.to_string())?;
        let aux = CreateWindowAux::new().override_redirect(1).event_mask(EventMask::ENTER_WINDOW | EventMask::POINTER_MOTION);
        c.create_window(0, win, self.root, 0, 0, s.width_in_pixels, s.height_in_pixels, 0, WindowClass::INPUT_ONLY, 0, &aux).map_err(|e| e.to_string())?;
        c.map_window(win).map_err(|e| e.to_string())?;
        c.flush().map_err(|e| e.to_string())?;
        let deadline = Instant::now() + timeout;
        let found = loop {
            match c.poll_for_event() {
                Ok(Some(Event::EnterNotify(e))) => break Some((e.root_x as i32, e.root_y as i32)),
                Ok(Some(Event::MotionNotify(e))) => break Some((e.root_x as i32, e.root_y as i32)),
                Ok(Some(_)) => continue,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(4)),
                _ => break None,
            }
        };
        let _ = c.destroy_window(win);
        let _ = c.flush();
        found.ok_or_else(|| "the pointer didn't enter the probe window".into())
    }

    /// `rect` in root pixels, as RGBA. A rootless X server (XWayland) can't read the root
    /// window, only each window's own contents: then the topmost window under the area.
    pub fn capture(&self, rect: [i32; 4]) -> Result<Shot, String> {
        match self.capture_from(self.root, rect, [0, 0]) {
            Ok(shot) => Ok(shot),
            Err(root_error) => {
                let (win, origin) = self.window_at(rect).ok_or(root_error)?;
                // Clip to the window.
                let g = self.conn.get_geometry(win).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
                let x0 = rect[0].max(origin[0]);
                let y0 = rect[1].max(origin[1]);
                let x1 = (rect[0] + rect[2]).min(origin[0] + g.width as i32);
                let y1 = (rect[1] + rect[3]).min(origin[1] + g.height as i32);
                if x1 <= x0 || y1 <= y0 { return Err("no window under the pointer to read".into()); }
                self.capture_from(win, [x0, y0, x1 - x0, y1 - y0], origin)
            }
        }
    }

    /// The topmost mapped top-level window containing the centre of `rect`, and its origin.
    fn window_at(&self, rect: [i32; 4]) -> Option<(Window, [i32; 2])> {
        let (cx, cy) = (rect[0] + rect[2] / 2, rect[1] + rect[3] / 2);
        let tree = self.conn.query_tree(self.root).ok()?.reply().ok()?;
        for &w in tree.children.iter().rev() {
            let Ok(Ok(attrs)) = self.conn.get_window_attributes(w).map(|c| c.reply()) else { continue };
            if attrs.map_state != x11rb::protocol::xproto::MapState::VIEWABLE || attrs.class == WindowClass::INPUT_ONLY { continue }
            let Ok(Ok(g)) = self.conn.get_geometry(w).map(|c| c.reply()) else { continue };
            let Ok(Ok(t)) = self.conn.translate_coordinates(w, self.root, 0, 0).map(|c| c.reply()) else { continue };
            let (x, y) = (t.dst_x as i32, t.dst_y as i32);
            if std::env::var_os("TLUM_DEBUG").is_some() { eprintln!("tlum: X window {w:#x} at {x},{y} {}x{} (looking for {cx},{cy})", g.width, g.height); }
            if cx >= x && cy >= y && cx < x + g.width as i32 && cy < y + g.height as i32 { return Some((w, [x, y])); }
        }
        None
    }

    fn capture_from(&self, drawable: Window, rect: [i32; 4], origin: [i32; 2]) -> Result<Shot, String> {
        let img = self.conn.get_image(ImageFormat::Z_PIXMAP, drawable, (rect[0] - origin[0]) as i16, (rect[1] - origin[1]) as i16, rect[2] as u16, rect[3] as u16, !0)
            .map_err(|e| e.to_string())?.reply().map_err(|e| format!("GetImage failed: {e}"))?;
        if img.depth != 24 && img.depth != 32 && self.depth != 24 { return Err(format!("unsupported X depth {}", img.depth)); }
        let (w, h) = (rect[2] as u32, rect[3] as u32);
        let stride = img.data.len() / h.max(1) as usize;
        if stride < w as usize * 4 { return Err(format!("unexpected X image layout ({} bytes for {w}x{h})", img.data.len())); }
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            for p in img.data[y * stride..y * stride + w as usize * 4].chunks_exact(4) { rgba.extend_from_slice(&[p[2], p[1], p[0], 255]); }
        }
        Ok(Shot { width: w, height: h, rgba, origin: [rect[0] as f64, rect[1] as f64], scale: 1.0 })
    }

    /// Whether a compositing manager runs (it owns `_NET_WM_CM_S<screen>`).
    pub fn composited(&self) -> bool {
        let Ok(atom) = self.atom("_NET_WM_CM_S0") else { return true };
        self.conn.get_selection_owner(atom).ok().and_then(|c| c.reply().ok()).is_some_and(|r| r.owner != x11rb::NONE)
    }

    fn atom(&self, name: &str) -> Result<u32, String> {
        Ok(self.conn.intern_atom(false, name.as_bytes()).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?.atom)
    }

    /// The PRIMARY selection as text.
    pub fn primary(&self, timeout: Duration) -> Option<String> {
        let c = &self.conn;
        let win = c.generate_id().ok()?;
        c.create_window(0, win, self.root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new()).ok()?;
        let (utf8, prop) = (self.atom("UTF8_STRING").ok()?, self.atom("TLUM_SEL").ok()?);
        c.convert_selection(win, AtomEnum::PRIMARY.into(), utf8, prop, x11rb::CURRENT_TIME).ok()?;
        c.flush().ok()?;
        let deadline = Instant::now() + timeout;
        let mut text = None;
        loop {
            match c.poll_for_event() {
                Ok(Some(Event::SelectionNotify(e))) if e.requestor == win => {
                    if e.property != x11rb::NONE
                        && let Ok(r) = c.get_property(true, win, prop, AtomEnum::ANY, 0, 1024).map(|r| r.reply())
                        && let Ok(r) = r { text = String::from_utf8(r.value).ok(); }
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(3)),
                _ => break,
            }
        }
        let _ = c.destroy_window(win);
        let _ = c.flush();
        text
    }
}

/// Grabs `keys` ("Meta+Ctrl+D") on the root window and calls `on_press(x, y)` with the
/// pointer, in root pixels, each time it's pressed. Runs on its own thread and connection.
pub fn grab_shortcut(keys: &str, on_press: impl Fn(i32, i32) + Send + 'static) -> Result<(), String> {
    let x = X11::connect()?;
    let mut mods = ModMask::from(0u16);
    let mut key = None;
    for part in keys.split('+').map(|p| p.trim().to_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= ModMask::CONTROL,
            "alt" => mods |= ModMask::M1,
            "shift" => mods |= ModMask::SHIFT,
            "meta" | "super" | "win" | "mod4" => mods |= ModMask::M4,
            k if k.chars().count() == 1 => key = k.chars().next(),
            other => return Err(format!("can't grab key '{other}' (use one letter or digit)")),
        }
    }
    let keysym = key.ok_or("no key in the shortcut")? as u32;
    let setup = x.conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = x.conn.get_keyboard_mapping(min, max - min + 1).map_err(|e| e.to_string())?.reply().map_err(|e| e.to_string())?;
    let per = map.keysyms_per_keycode as usize;
    let code = map.keysyms.chunks(per).position(|syms| syms.first() == Some(&keysym)).map(|i| min + i as u8)
        .ok_or_else(|| format!("no key types '{}'", key.unwrap()))?;
    // With and without Caps Lock and Num Lock, which X counts as modifiers.
    for extra in [0u16, u16::from(ModMask::LOCK), u16::from(ModMask::M2), u16::from(ModMask::LOCK) | u16::from(ModMask::M2)] {
        x.conn.grab_key(true, x.root, mods | ModMask::from(extra), code, GrabMode::ASYNC, GrabMode::ASYNC)
            .map_err(|e| e.to_string())?.check().map_err(|e| format!("another program holds {keys}: {e}"))?;
    }
    x.conn.flush().map_err(|e| e.to_string())?;
    std::thread::Builder::new().name("x11-shortcut".into()).spawn(move || {
        while let Ok(ev) = x.conn.wait_for_event() {
            if let Event::KeyPress(e) = ev { on_press(e.root_x as i32, e.root_y as i32); }
        }
    }).map_err(|e| e.to_string())?;
    Ok(())
}
