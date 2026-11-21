//! Reads mice and touchpads from `/dev/input/event*` (the user must be in the `input` group)
//! and runs their gestures. Keyboards are never opened. Devices that appear later (a mouse
//! plugged in, a Bluetooth touchpad waking) are picked up through inotify.

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Instant;

use crate::gesture::{self, Effect, Mouse, Settings, Touchpad};

const IOC_READ: libc::c_ulong = 2;
const fn ioc(dir: libc::c_ulong, nr: libc::c_ulong, size: libc::c_ulong) -> libc::c_ulong {
    (dir << 30) | (size << 16) | ((b'E' as libc::c_ulong) << 8) | nr
}
const fn eviocgbit(ev: u16, len: usize) -> libc::c_ulong { ioc(IOC_READ, 0x20 + ev as libc::c_ulong, len as libc::c_ulong) }
const fn eviocgprop(len: usize) -> libc::c_ulong { ioc(IOC_READ, 0x09, len as libc::c_ulong) }
const fn eviocgname(len: usize) -> libc::c_ulong { ioc(IOC_READ, 0x06, len as libc::c_ulong) }
const fn eviocgabs(abs: u16) -> libc::c_ulong { ioc(IOC_READ, 0x40 + abs as libc::c_ulong, 24) }

const INPUT_PROP_DIRECT: usize = 1;
const ABS_X: u16 = 0;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AbsInfo { value: i32, minimum: i32, maximum: i32, fuzz: i32, flat: i32, resolution: i32 }

#[repr(C)]
#[derive(Clone, Copy)]
struct RawEvent { time: libc::timeval, kind: u16, code: u16, value: i32 }

fn bits(fd: RawFd, request: libc::c_ulong, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    // SAFETY: the ioctl writes at most `len` bytes into `buf`.
    let n = unsafe { libc::ioctl(fd, request as _, buf.as_mut_ptr()) };
    if n < 0 { buf.clear(); }
    buf
}

fn has(bits: &[u8], bit: usize) -> bool { bits.get(bit / 8).is_some_and(|b| b & (1 << (bit % 8)) != 0) }

pub enum Kind { Mouse(Mouse), Touchpad(Touchpad) }

pub struct Device { fd: OwnedFd, pub name: String, pub kind: Kind }

fn abs_info(fd: RawFd, abs: u16) -> AbsInfo {
    let mut info = AbsInfo::default();
    // SAFETY: EVIOCGABS fills one input_absinfo (24 bytes).
    unsafe { libc::ioctl(fd, eviocgabs(abs) as _, &mut info as *mut AbsInfo) };
    info
}

fn units_per_mm(info: AbsInfo) -> f64 {
    if info.resolution > 0 { info.resolution as f64 } else { ((info.maximum - info.minimum) as f64 / 100.0).max(1.0) }
}

/// Opens `path` if it's a mouse or a touchpad.
pub fn open(path: &str) -> Result<Option<Device>, std::io::Error> {
    let c = CString::new(path).unwrap();
    // SAFETY: open(2) on a NUL-terminated path; the descriptor is owned right after.
    let raw = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if raw < 0 { return Err(std::io::Error::last_os_error()); }
    // SAFETY: `raw` is a fresh descriptor nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let r = fd.as_raw_fd();
    let ev = bits(r, eviocgbit(0, 4), 4);
    let keys = bits(r, eviocgbit(gesture::EV_KEY, 96), 96);
    let rel = bits(r, eviocgbit(gesture::EV_REL, 4), 4);
    let abs = bits(r, eviocgbit(gesture::EV_ABS, 8), 8);
    let props = bits(r, eviocgprop(4), 4);
    let name = {
        let b = bits(r, eviocgname(256), 256);
        String::from_utf8_lossy(&b[..b.iter().position(|c| *c == 0).unwrap_or(b.len())]).into_owned()
    };
    let touchpad = has(&ev, gesture::EV_ABS as usize) && has(&abs, gesture::ABS_MT_POSITION_X as usize)
        && has(&keys, gesture::BTN_TOOL_FINGER as usize) && !has(&props, INPUT_PROP_DIRECT);
    let mouse = has(&ev, gesture::EV_REL as usize) && has(&rel, gesture::REL_X as usize) && has(&keys, gesture::BTN_LEFT as usize);
    // Automation tools' virtual devices replay input; their presses aren't a person's.
    if name.to_lowercase().contains("ydotool") { return Ok(None); }
    let kind = if touchpad {
        let mut x = abs_info(r, gesture::ABS_MT_POSITION_X);
        let mut y = abs_info(r, gesture::ABS_MT_POSITION_Y);
        if x.maximum == 0 { x = abs_info(r, ABS_X); y = abs_info(r, ABS_X + 1); }
        Kind::Touchpad(Touchpad::new(units_per_mm(x), units_per_mm(y)))
    } else if mouse {
        Kind::Mouse(Mouse::default())
    } else {
        return Ok(None);
    };
    Ok(Some(Device { fd, name, kind }))
}

fn event_paths() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("/dev/input").map(|d| d.flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.rsplit('/').next().is_some_and(|n| n.starts_with("event")))
        .collect()).unwrap_or_default();
    v.sort();
    v
}

/// What `devices` found, for `tlum doctor`.
pub fn survey() -> (Vec<(String, String, &'static str)>, usize) {
    let mut found = Vec::new();
    let mut denied = 0;
    for p in event_paths() {
        match open(&p) {
            Ok(Some(d)) => found.push((p, d.name, match d.kind { Kind::Mouse(_) => "mouse", Kind::Touchpad(_) => "touchpad" })),
            Ok(None) => {}
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied += 1,
            Err(_) => {}
        }
    }
    (found, denied)
}

fn inotify() -> Option<OwnedFd> {
    // SAFETY: inotify_init1 returns a fresh descriptor or -1.
    let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if fd < 0 { return None; }
    let dir = CString::new("/dev/input").unwrap();
    // SAFETY: plain syscall on our descriptor.
    unsafe { libc::inotify_add_watch(fd, dir.as_ptr(), libc::IN_CREATE | libc::IN_ATTRIB) };
    // SAFETY: `fd` is ours.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Runs forever on the calling thread, reporting effects to `sink`.
pub fn run(settings: Settings, sink: impl Fn(Effect, &str)) {
    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut devices: HashMap<String, Device> = HashMap::new();
    let notify = inotify();
    let mut rescan = true;
    let mut warned = false;
    loop {
        if rescan {
            rescan = false;
            let mut denied = 0;
            for p in event_paths() {
                if devices.contains_key(&p) { continue; }
                match open(&p) {
                    Ok(Some(d)) => { eprintln!("tlum: listening to {} ({})", d.name, match d.kind { Kind::Mouse(_) => "mouse", Kind::Touchpad(_) => "touchpad" }); devices.insert(p, d); }
                    Ok(None) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied += 1,
                    Err(_) => {}
                }
            }
            if devices.is_empty() && denied > 0 && !warned {
                warned = true;
                eprintln!("tlum: can't read /dev/input (add yourself to the `input` group: sudo usermod -aG input $USER, then log in again). The keyboard shortcut still works.");
            }
        }
        let t = now();
        let deadline = devices.values().filter_map(|d| match &d.kind {
            Kind::Mouse(m) => m.deadline(&settings), Kind::Touchpad(p) => p.deadline(&settings),
        }).min();
        let timeout = deadline.map_or(-1, |d| d.saturating_sub(t).min(60_000) as i32);
        let mut fds: Vec<libc::pollfd> = devices.values().map(|d| libc::pollfd { fd: d.fd.as_raw_fd(), events: libc::POLLIN, revents: 0 }).collect();
        let paths: Vec<String> = devices.keys().cloned().collect();
        if let Some(n) = &notify { fds.push(libc::pollfd { fd: n.as_raw_fd(), events: libc::POLLIN, revents: 0 }); }
        // SAFETY: `fds` is a valid array for the duration of the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) };
        if n < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted { std::thread::sleep(std::time::Duration::from_millis(200)); }
        let t = now();
        let mut gone = Vec::new();
        for (i, pfd) in fds.iter().enumerate() {
            if pfd.revents == 0 { continue; }
            if i == paths.len() {
                let mut buf = [0u8; 4096];
                // SAFETY: drain inotify into a local buffer.
                while unsafe { libc::read(pfd.fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
                rescan = true;
                continue;
            }
            let path = &paths[i];
            if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 { gone.push(path.clone()); continue; }
            let Some(dev) = devices.get_mut(path) else { continue };
            let mut evs = [RawEvent { time: libc::timeval { tv_sec: 0, tv_usec: 0 }, kind: 0, code: 0, value: 0 }; 64];
            loop {
                // SAFETY: reads whole input_event structs into `evs`.
                let got = unsafe { libc::read(pfd.fd, evs.as_mut_ptr().cast(), std::mem::size_of_val(&evs)) };
                if got <= 0 {
                    if got < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENODEV) { gone.push(path.clone()); }
                    break;
                }
                for e in &evs[..got as usize / std::mem::size_of::<RawEvent>()] {
                    match &mut dev.kind {
                        Kind::Mouse(m) => if let Some(fx) = m.event(&settings, e.kind, e.code, e.value, t) { sink(fx, &dev.name) },
                        Kind::Touchpad(p) => for fx in p.event(&settings, e.kind, e.code, e.value, t) { sink(fx, &dev.name) },
                    }
                }
            }
        }
        for g in gone { if let Some(d) = devices.remove(&g) { eprintln!("tlum: {} went away", d.name); } }
        for dev in devices.values_mut() {
            let fx = match &mut dev.kind { Kind::Mouse(m) => m.tick(&settings, t), Kind::Touchpad(p) => p.tick(&settings, t) };
            if let Some(fx) = fx { sink(fx, &dev.name); }
        }
    }
}
