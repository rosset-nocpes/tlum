//! Wayland without KWin's APIs: sway and other wlroots compositors, Hyprland, niri, COSMIC.
//!
//! - outputs: `wl_output` + `xdg-output` for names and logical geometry;
//! - pixels: `ext-image-copy-capture-v1` (the standard) or `wlr-screencopy-unstable-v1`, into
//!   shared memory;
//! - pointer: a transparent layer-shell surface over each output, with no exclusive zone so it
//!   covers panels too, reads where `wl_pointer.enter` lands (output-local) and goes away. While
//!   a button is held on another window the compositor keeps the pointer there, so the probe
//!   answers when it's released.

use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

use crate::scan::Shot;

#[derive(Clone, Debug, Default)]
pub struct Output {
    global: u32,
    pub name: Option<String>,
    /// x, y, width, height in global logical coordinates.
    pub logical: Option<[i32; 4]>,
    /// Pixels of the current mode.
    pub mode: (i32, i32),
    pub scale: i32,
}

impl Output {
    pub fn rect(&self) -> [f64; 4] {
        let r = self.logical.unwrap_or([0, 0, self.mode.0 / self.scale.max(1), self.mode.1 / self.scale.max(1)]);
        r.map(f64::from)
    }
}

#[derive(Default)]
struct Capture {
    size: Option<(u32, u32)>,
    formats: Vec<wl_shm::Format>,
    stride: u32,
    done: bool,
    ready: bool,
    failed: Option<String>,
    y_invert: bool,
}

#[derive(Default)]
struct State {
    outputs: Vec<(WlOutput, Output)>,
    cap: Capture,
    configures: Vec<(usize, u32, u32, u32)>,
    probe_surfaces: Vec<WlSurface>,
    entered: Option<(usize, f64, f64)>,
    pointer: Option<WlPointer>,
    /// The seat has a pointer (asking for one otherwise is a protocol error).
    has_pointer: bool,
    closed: bool,
    /// shm formats offered that we don't read, for the error message.
    other_formats: Vec<u32>,
}

pub struct Wayland {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    shm: WlShm,
    compositor: Option<WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    viewporter: Option<WpViewporter>,
    seat: Option<WlSeat>,
    ext_sources: Option<ExtOutputImageCaptureSourceManagerV1>,
    ext_copy: Option<ExtImageCopyCaptureManagerV1>,
    wlr_copy: Option<ZwlrScreencopyManagerV1>,
    /// wlroots compositors pick the surface under the pointer only when it moves: a
    /// zero-distance motion makes them notice the probe.
    nudge: Option<ZwlrVirtualPointerManagerV1>,
}

impl Wayland {
    pub fn connect() -> Result<Wayland, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let mut state = State::default();
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).map_err(|_| "no wl_shm")?;
        let xdg: Option<ZxdgOutputManagerV1> = globals.bind(&qh, 2..=3, ()).ok();
        let list = globals.contents().clone_list();
        for g in list.iter().filter(|g| g.interface == "wl_output") {
            let o: WlOutput = globals.registry().bind(g.name, g.version.min(4), &qh, g.name);
            if let Some(x) = &xdg { x.get_xdg_output(&o, &qh, g.name); }
            state.outputs.push((o, Output { global: g.name, scale: 1, ..Output::default() }));
        }
        let me = Wayland {
            compositor: globals.bind(&qh, 4..=6, ()).ok(),
            layer_shell: globals.bind(&qh, 1..=4, ()).ok(),
            viewporter: globals.bind(&qh, 1..=1, ()).ok(),
            seat: globals.bind(&qh, 1..=7, ()).ok(),
            ext_sources: globals.bind(&qh, 1..=1, ()).ok(),
            ext_copy: globals.bind(&qh, 1..=1, ()).ok(),
            wlr_copy: globals.bind(&qh, 1..=3, ()).ok(),
            nudge: globals.bind(&qh, 1..=2, ()).ok(),
            conn, queue: { queue.roundtrip(&mut state).map_err(|e| e.to_string())?; queue }, qh, state, shm,
        };
        let mut me = me;
        me.queue.roundtrip(&mut me.state).map_err(|e| e.to_string())?;
        Ok(me)
    }

    pub fn outputs(&self) -> Vec<Output> { self.state.outputs.iter().map(|o| o.1.clone()).collect() }
    pub fn can_capture(&self) -> bool { (self.ext_sources.is_some() && self.ext_copy.is_some()) || self.wlr_copy.is_some() }
    pub fn can_probe(&self) -> bool { self.layer_shell.is_some() && self.compositor.is_some() && self.seat.is_some() }
    pub fn protocols(&self) -> String {
        let mut v = vec![];
        if self.ext_copy.is_some() { v.push("ext-image-copy-capture"); }
        if self.wlr_copy.is_some() { v.push("wlr-screencopy"); }
        if self.layer_shell.is_some() { v.push("layer-shell"); }
        v.join(", ")
    }

    /// Dispatches until `done` says so or `timeout` passes.
    fn wait(&mut self, timeout: Duration, done: impl Fn(&State) -> bool) -> Result<bool, String> {
        let deadline = Instant::now() + timeout;
        loop {
            self.queue.dispatch_pending(&mut self.state).map_err(|e| e.to_string())?;
            if done(&self.state) { return Ok(true); }
            self.queue.flush().map_err(|e| e.to_string())?;
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() { return Ok(false); }
            if let Some(guard) = self.queue.prepare_read() {
                let mut pfd = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: one valid pollfd for the call.
                #[allow(unsafe_code)]
                let n = unsafe { libc::poll(&mut pfd, 1, left.as_millis().min(i32::MAX as u128) as i32) };
                if n > 0 { guard.read().map_err(|e| e.to_string())?; }
            }
        }
    }

    fn sync(&mut self) -> Result<(), String> { self.queue.roundtrip(&mut self.state).map(|_| ()).map_err(|e| e.to_string()) }

    fn shm_buffer(&self, w: u32, h: u32, stride: u32, format: wl_shm::Format) -> Result<(File, WlBuffer), String> {
        let size = (stride * h) as usize;
        // SAFETY: memfd_create returns a fresh descriptor we own.
        #[allow(unsafe_code)]
        let file = unsafe {
            let fd = libc::memfd_create(c"tlum-shm".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 { return Err(std::io::Error::last_os_error().to_string()); }
            File::from_raw_fd(fd)
        };
        file.set_len(size as u64).map_err(|e| e.to_string())?;
        let pool: WlShmPool = self.shm.create_pool(file.as_fd(), size as i32, &self.qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, &self.qh, ());
        pool.destroy();
        Ok((file, buffer))
    }

    fn output(&self, name: Option<&str>, at: [f64; 2]) -> Option<(WlOutput, Output)> {
        let all = &self.state.outputs;
        all.iter().find(|o| name.is_some() && o.1.name.as_deref() == name)
            .or_else(|| all.iter().find(|o| { let r = o.1.rect(); at[0] >= r[0] && at[1] >= r[1] && at[0] < r[0] + r[2] && at[1] < r[1] + r[3] }))
            .or_else(|| all.first()).cloned()
    }

    /// `rect` (global logical) on the output under `at`.
    pub fn capture(&mut self, rect: [i32; 4], output: Option<&str>, at: [f64; 2]) -> Result<Shot, String> {
        self.sync()?;
        let (wl_out, out) = self.output(output, at).ok_or("no outputs")?;
        let o = out.rect();
        if self.ext_sources.is_some() && self.ext_copy.is_some() {
            match self.capture_ext(&wl_out) {
                Ok((w, h, rgba)) => return crop(w, h, rgba, o, rect),
                Err(e) if self.wlr_copy.is_none() => return Err(e),
                Err(e) => eprintln!("tlum: ext-image-copy-capture failed ({e}); trying wlr-screencopy"),
            }
        }
        if self.wlr_copy.is_some() {
            // Only the region we need, in output-local logical coordinates.
            let local = [rect[0] - o[0] as i32, rect[1] - o[1] as i32, rect[2], rect[3]];
            let (w, h, rgba) = self.capture_wlr(&wl_out, local)?;
            let k = w as f64 / rect[2].max(1) as f64;
            return Ok(Shot { width: w, height: h, rgba, origin: [rect[0] as f64, rect[1] as f64], scale: k });
        }
        Err("this compositor offers no screen capture protocol".into())
    }

    fn capture_ext(&mut self, output: &WlOutput) -> Result<(u32, u32, Vec<u8>), String> {
        let (sources, copy) = (self.ext_sources.clone().unwrap(), self.ext_copy.clone().unwrap());
        self.state.cap = Capture::default();
        let source: ExtImageCaptureSourceV1 = sources.create_source(output, &self.qh, ());
        let session: ExtImageCopyCaptureSessionV1 = copy.create_session(&source, Options::empty(), &self.qh, ());
        let result = (|| {
            if !self.wait(Duration::from_secs(2), |s| s.cap.done || s.cap.failed.is_some())? { return Err("capture session never said its size".to_string()); }
            if let Some(e) = self.state.cap.failed.take() { return Err(e); }
            let (w, h) = self.state.cap.size.ok_or("no buffer size")?;
            let format = pick_format(&self.state.cap.formats).ok_or_else(|| format!("no shared-memory format we read (offered {:?} {:x?})", self.state.cap.formats, self.state.other_formats))?;
            let stride = stride_for(w, format);
            let (file, buffer) = self.shm_buffer(w, h, stride, format)?;
            let frame: ExtImageCopyCaptureFrameV1 = session.create_frame(&self.qh, ());
            frame.attach_buffer(&buffer);
            frame.damage_buffer(0, 0, w as i32, h as i32);
            frame.capture();
            let ok = self.wait(Duration::from_secs(2), |s| s.cap.ready || s.cap.failed.is_some())?;
            frame.destroy();
            buffer.destroy();
            if let Some(e) = self.state.cap.failed.take() { return Err(e); }
            if !ok { return Err("the capture timed out".into()); }
            let mut data = vec![0u8; (stride * h) as usize];
            file.read_exact_at(&mut data, 0).map_err(|e| e.to_string())?;
            Ok((w, h, to_rgba(data, w, h, stride, format, false)))
        })();
        session.destroy();
        source.destroy();
        result
    }

    fn capture_wlr(&mut self, output: &WlOutput, local: [i32; 4]) -> Result<(u32, u32, Vec<u8>), String> {
        let copy = self.wlr_copy.clone().unwrap();
        self.state.cap = Capture::default();
        let frame: ZwlrScreencopyFrameV1 = copy.capture_output_region(0, output, local[0], local[1], local[2], local[3], &self.qh, ());
        let result = (|| {
            if !self.wait(Duration::from_secs(2), |s| s.cap.done || s.cap.failed.is_some())? { return Err("screencopy never offered a buffer".to_string()); }
            if let Some(e) = self.state.cap.failed.take() { return Err(e); }
            let (w, h) = self.state.cap.size.ok_or("no buffer size")?;
            let format = pick_format(&self.state.cap.formats).ok_or_else(|| format!("no shared-memory format we read (offered {:?} {:x?})", self.state.cap.formats, self.state.other_formats))?;
            let stride = self.state.cap.stride.max(stride_for(w, format));
            let (file, buffer) = self.shm_buffer(w, h, stride, format)?;
            frame.copy(&buffer);
            let ok = self.wait(Duration::from_secs(2), |s| s.cap.ready || s.cap.failed.is_some())?;
            buffer.destroy();
            if let Some(e) = self.state.cap.failed.take() { return Err(e); }
            if !ok { return Err("the capture timed out".into()); }
            let mut data = vec![0u8; (stride * h) as usize];
            file.read_exact_at(&mut data, 0).map_err(|e| e.to_string())?;
            Ok((w, h, to_rgba(data, w, h, stride, format, self.state.cap.y_invert)))
        })();
        frame.destroy();
        result
    }

    /// Where the pointer is: the output's name and the point in global logical coordinates.
    pub fn probe_pointer(&mut self, timeout: Duration) -> Result<(Option<String>, f64, f64), String> {
        let (compositor, shell) = (self.compositor.clone().ok_or("no wl_compositor")?, self.layer_shell.clone().ok_or("no layer-shell")?);
        self.sync()?;
        if !self.state.has_pointer { return Err("the seat has no pointer".into()); }
        if self.state.pointer.is_none() {
            let seat = self.seat.clone().ok_or("no seat")?;
            self.state.pointer = Some(seat.get_pointer(&self.qh, ()));
        }
        self.state.entered = None;
        self.state.configures.clear();
        let outs: Vec<(WlOutput, Output)> = self.state.outputs.clone();
        let mut layers = Vec::new();
        self.state.probe_surfaces.clear();
        for (i, (wo, _)) in outs.iter().enumerate() {
            let surface = compositor.create_surface(&self.qh, ());
            let layer = shell.get_layer_surface(&surface, Some(wo), Layer::Overlay, "tlum-probe".into(), &self.qh, i);
            layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            surface.commit();
            self.state.probe_surfaces.push(surface.clone());
            layers.push((surface, layer));
        }
        // One buffer (and viewport) per surface; later configures only resize it.
        let mut buffers: Vec<Option<(File, WlBuffer, Option<WpViewport>)>> = (0..layers.len()).map(|_| None).collect();
        let deadline = Instant::now() + timeout;
        let mut nudge_at = Some(Instant::now() + Duration::from_millis(80));
        let mut nudger: Option<ZwlrVirtualPointerV1> = None;
        let result = loop {
            if nudge_at.is_some_and(|t| Instant::now() >= t) && buffers.iter().all(Option::is_some) {
                nudge_at = None;
                if let (Some(m), Some(seat)) = (&self.nudge, &self.seat) {
                    let vp = m.create_virtual_pointer(Some(seat), &self.qh, ());
                    vp.motion(0, 0.0, 0.0);
                    vp.frame();
                    nudger = Some(vp);
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() { break Err("the pointer didn't reach the probe".to_string()); }
            self.wait(left.min(Duration::from_millis(50)), |s| s.entered.is_some() || !s.configures.is_empty() || s.closed)?;
            for (i, serial, w, h) in std::mem::take(&mut self.state.configures) {
                let (surface, layer) = &layers[i];
                layer.ack_configure(serial);
                if let Some((_, _, Some(vp))) = &buffers[i] {
                    vp.set_destination(w.max(1) as i32, h.max(1) as i32);
                    surface.commit();
                    continue;
                }
                if let Some((_, old, None)) = buffers[i].take() { old.destroy(); }
                let (bw, bh) = if self.viewporter.is_some() { (1, 1) } else { (w.max(1), h.max(1)) };
                let (file, buffer) = self.shm_buffer(bw, bh, bw * 4, wl_shm::Format::Argb8888)?;
                let viewport = self.viewporter.as_ref().map(|v| { let vp = v.get_viewport(surface, &self.qh, ()); vp.set_destination(w.max(1) as i32, h.max(1) as i32); vp });
                surface.attach(Some(&buffer), 0, 0);
                surface.damage_buffer(0, 0, bw as i32, bh as i32);
                surface.commit();
                buffers[i] = Some((file, buffer, viewport));
            }
            if let Some((i, x, y)) = self.state.entered.take() {
                let o = outs[i].1.rect();
                break Ok((outs[i].1.name.clone(), o[0] + x, o[1] + y));
            }
            if self.state.closed { break Err("the compositor closed the probe".into()); }
        };
        if let Some(vp) = nudger { vp.destroy(); }
        for (surface, layer) in layers { layer.destroy(); surface.destroy(); }
        for (_, b, vp) in buffers.into_iter().flatten() { if let Some(vp) = vp { vp.destroy(); } b.destroy(); }
        self.state.probe_surfaces.clear();
        let _ = self.conn.flush();
        result
    }
}

fn pick_format(formats: &[wl_shm::Format]) -> Option<wl_shm::Format> {
    use wl_shm::Format::*;
    [Xrgb8888, Argb8888, Xbgr8888, Abgr8888, Bgr888, Rgb888].into_iter().find(|f| formats.contains(f))
}

/// Bytes a pixel, and whether its bytes run blue first.
fn layout(format: wl_shm::Format) -> (u32, bool) {
    use wl_shm::Format::*;
    match format {
        // DRM formats name channels from the most significant bit; memory is little-endian.
        Xrgb8888 | Argb8888 => (4, true),
        Xbgr8888 | Abgr8888 => (4, false),
        Rgb888 => (3, true),
        _ => (3, false), // Bgr888
    }
}

/// A row's bytes, 4-aligned as compositors expect.
fn stride_for(w: u32, format: wl_shm::Format) -> u32 { (w * layout(format).0).div_ceil(4) * 4 }

fn to_rgba(data: Vec<u8>, w: u32, h: u32, stride: u32, format: wl_shm::Format, y_invert: bool) -> Vec<u8> {
    let (bpp, blue_first) = layout(format);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let y = if y_invert { h - 1 - row } else { row };
        let start = (y * stride) as usize;
        for p in data[start..start + (w * bpp) as usize].chunks_exact(bpp as usize) {
            if blue_first { out.extend_from_slice(&[p[2], p[1], p[0], 255]) } else { out.extend_from_slice(&[p[0], p[1], p[2], 255]) }
        }
    }
    out
}

/// The part of a whole-output image (`out` is its logical rect) covering `rect`.
pub fn crop(w: u32, h: u32, rgba: Vec<u8>, out: [f64; 4], rect: [i32; 4]) -> Result<Shot, String> {
    let k = w as f64 / out[2].max(1.0);
    let x0 = (((rect[0] as f64 - out[0]) * k).floor().max(0.0) as u32).min(w);
    let y0 = (((rect[1] as f64 - out[1]) * k).floor().max(0.0) as u32).min(h);
    let x1 = (((rect[0] + rect[2]) as f64 - out[0]) * k).ceil().clamp(0.0, w as f64) as u32;
    let y1 = (((rect[1] + rect[3]) as f64 - out[1]) * k).ceil().clamp(0.0, h as f64) as u32;
    if x1 <= x0 || y1 <= y0 { return Err("the area is off this output".into()); }
    let mut px = Vec::with_capacity(((x1 - x0) * (y1 - y0) * 4) as usize);
    for y in y0..y1 { px.extend_from_slice(&rgba[((y * w + x0) * 4) as usize..((y * w + x1) * 4) as usize]); }
    Ok(Shot { width: x1 - x0, height: y1 - y0, rgba: px, origin: [out[0] + x0 as f64 / k, out[1] + y0 as f64 / k], scale: k })
}

// --- event plumbing ------------------------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlOutput, u32> for State {
    fn event(s: &mut Self, _: &WlOutput, e: wl_output::Event, global: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let Some((_, o)) = s.outputs.iter_mut().find(|o| o.1.global == *global) else { return };
        match e {
            wl_output::Event::Name { name } => o.name = Some(name),
            wl_output::Event::Scale { factor } => o.scale = factor,
            wl_output::Event::Mode { flags: WEnum::Value(f), width, height, .. } if f.contains(wl_output::Mode::Current) => o.mode = (width, height),
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, u32> for State {
    fn event(s: &mut Self, _: &ZxdgOutputV1, e: zxdg_output_v1::Event, global: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let Some((_, o)) = s.outputs.iter_mut().find(|o| o.1.global == *global) else { return };
        let r = o.logical.get_or_insert([0; 4]);
        match e {
            zxdg_output_v1::Event::LogicalPosition { x, y } => { r[0] = x; r[1] = y; }
            zxdg_output_v1::Event::LogicalSize { width, height } => { r[2] = width; r[3] = height; }
            zxdg_output_v1::Event::Name { name } => if o.name.is_none() { o.name = Some(name) },
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(s: &mut Self, _: &ExtImageCopyCaptureSessionV1, e: ext_image_copy_capture_session_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => s.cap.size = Some((width, height)),
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => match format {
                WEnum::Value(f) => s.cap.formats.push(f),
                WEnum::Unknown(u) => s.other_formats.push(u),
            },
            ext_image_copy_capture_session_v1::Event::Done => s.cap.done = true,
            ext_image_copy_capture_session_v1::Event::Stopped => s.cap.failed = Some("the compositor stopped the capture".into()),
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(s: &mut Self, _: &ExtImageCopyCaptureFrameV1, e: ext_image_copy_capture_frame_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            ext_image_copy_capture_frame_v1::Event::Ready => s.cap.ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => s.cap.failed = Some(format!("capture failed ({reason:?})")),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(s: &mut Self, frame: &ZwlrScreencopyFrameV1, e: zwlr_screencopy_frame_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            zwlr_screencopy_frame_v1::Event::Buffer { format, width, height, stride } => {
                match format { WEnum::Value(f) => s.cap.formats.push(f), WEnum::Unknown(u) => s.other_formats.push(u) }
                s.cap.size = Some((width, height));
                s.cap.stride = stride;
                // Before version 3 there's no buffer_done: the shm buffer is all that's offered.
                if frame.version() < 3 { s.cap.done = true; }
            }
            zwlr_screencopy_frame_v1::Event::Flags { flags: WEnum::Value(f) } => s.cap.y_invert = f.contains(zwlr_screencopy_frame_v1::Flags::YInvert),
            // Version 3 lists every buffer type and then says it's done; older versions only
            // offer the shm buffer.
            zwlr_screencopy_frame_v1::Event::BufferDone => s.cap.done = true,
            zwlr_screencopy_frame_v1::Event::Ready { .. } => s.cap.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => s.cap.failed = Some("screencopy failed".into()),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, usize> for State {
    fn event(s: &mut Self, _: &ZwlrLayerSurfaceV1, e: zwlr_layer_surface_v1::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        match e {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => s.configures.push((*i, serial, width, height)),
            zwlr_layer_surface_v1::Event::Closed => s.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(s: &mut Self, _: &WlSeat, e: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(c) } = e { s.has_pointer = c.contains(wl_seat::Capability::Pointer); }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(s: &mut Self, _: &WlPointer, e: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_pointer::Event::Enter { surface, surface_x, surface_y, .. } = e
            && let Some(i) = s.probe_surfaces.iter().position(|p| p.id() == surface.id()) {
            s.entered = Some((i, surface_x, surface_y));
        }
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(_: &mut Self, _: &WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(State: ignore WlShm);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ZxdgOutputManagerV1);
delegate_noop!(State: ZwlrLayerShellV1);
delegate_noop!(State: WpViewporter);
delegate_noop!(State: WpViewport);
delegate_noop!(State: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ExtImageCaptureSourceV1);
delegate_noop!(State: ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ZwlrScreencopyManagerV1);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);
