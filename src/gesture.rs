//! The triggers, as state machines over raw evdev events (`input.rs` feeds them):
//!
//! - **Three-finger tap** on a touchpad: macOS's "Look up: tap with three fingers".
//! - **Force click** on a touchpad without pressure sensing: press the pad down and hold it
//!   still. macOS's default trigger is a deeper press; the closest a plain clickpad can say is
//!   a press that lasts.
//! - **Press and hold** with a mouse: the left button held still, the mouse's force click.
//! - Optionally a **mouse button** (a side button) that looks up at once.
//!
//! Events reach apps as usual: we only listen. A trigger fires while the button is still
//! down, as macOS shows the popover on the deep press, and moving on with the button held
//! (a drag that began with a pause) takes the popover back.
//!
//! Two-finger scrolls and wheel turns are reported too, for scrolling the popover where it gets
//! no wheel events of its own: as an XWayland window, on Wayland without layer-shell (GNOME).

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_SIDE: u16 = 0x113;
pub const BTN_EXTRA: u16 = 0x114;
pub const BTN_FORWARD: u16 = 0x115;
pub const BTN_BACK: u16 = 0x116;
pub const BTN_TOOL_FINGER: u16 = 0x145;
pub const BTN_TOOL_QUINTTAP: u16 = 0x148;
pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_TOOL_DOUBLETAP: u16 = 0x14d;
pub const BTN_TOOL_TRIPLETAP: u16 = 0x14e;
pub const BTN_TOOL_QUADTAP: u16 = 0x14f;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const EV_ABS: u16 = 3;
pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;
pub const REL_X: u16 = 0;
pub const REL_Y: u16 = 1;
pub const REL_WHEEL: u16 = 8;
pub const REL_WHEEL_HI_RES: u16 = 11;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
pub const ABS_MT_TRACKING_ID: u16 = 0x39;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source { ThreeFingerTap, ForceClick, MouseHold, MouseButton }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    Trigger(Source),
    /// The button that triggered is still down and moved: the person meant to drag.
    Dragged,
    /// Points to scroll the popover by, positive toward later content.
    Scroll(f32),
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub hold_ms: u64,
    pub three_finger_tap: bool,
    pub force_click: bool,
    pub mouse_hold: bool,
    /// An evdev button code that looks up at once (`BTN_SIDE`, `BTN_EXTRA`, …).
    pub mouse_button: Option<u16>,
    pub natural_scroll: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { hold_ms: 600, three_finger_tap: true, force_click: true, mouse_hold: true, mouse_button: None, natural_scroll: false }
    }
}

/// A press waiting to become a force click.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hold {
    Idle,
    /// Down since `since` (ms), having travelled `moved` (device units for a mouse, mm for a pad).
    Pending { since: u64, moved: f64 },
    /// Fired; still down.
    Fired { moved: f64 },
    /// Cancelled or reported as a drag; waiting for the release.
    Spent,
}

impl Hold {
    fn press(now: u64) -> Hold { Hold::Pending { since: now, moved: 0.0 } }

    fn travel(&mut self, d: f64, cancel_at: f64, drag_at: f64) -> Option<Effect> {
        match self {
            Hold::Pending { moved, .. } => { *moved += d; if *moved > cancel_at { *self = Hold::Spent; } None }
            Hold::Fired { moved } => { *moved += d; if *moved > drag_at { *self = Hold::Spent; Some(Effect::Dragged) } else { None } }
            _ => None,
        }
    }

    fn deadline(&self, hold_ms: u64) -> Option<u64> {
        match self { Hold::Pending { since, .. } => Some(since + hold_ms), _ => None }
    }

    fn tick(&mut self, now: u64, hold_ms: u64, source: Source) -> Option<Effect> {
        if let Hold::Pending { since, .. } = *self && now >= since + hold_ms {
            *self = Hold::Fired { moved: 0.0 };
            return Some(Effect::Trigger(source));
        }
        None
    }
}

pub struct Mouse {
    hold: Hold,
    held: u32,
    wheel_hi_res: bool,
}

impl Default for Mouse {
    fn default() -> Self { Mouse { hold: Hold::Idle, held: 0, wheel_hi_res: false } }
}

/// Counts (about 1/400 in on a typical mouse) a still press may wander, and a fired one may
/// travel before it counts as a drag.
const MOUSE_SLOP: f64 = 6.0;
const MOUSE_DRAG: f64 = 14.0;
/// Points one wheel notch scrolls.
const NOTCH: f32 = 48.0;

impl Mouse {
    pub fn event(&mut self, s: &Settings, kind: u16, code: u16, value: i32, now: u64) -> Option<Effect> {
        match (kind, code) {
            (EV_KEY, BTN_LEFT..=BTN_BACK) => {
                let down = value != 0;
                if down { self.held += 1 } else { self.held = self.held.saturating_sub(1) }
                if down && s.mouse_button == Some(code) { return Some(Effect::Trigger(Source::MouseButton)); }
                match (code, down) {
                    (BTN_LEFT, true) if self.held == 1 && s.mouse_hold => self.hold = Hold::press(now),
                    (BTN_LEFT, false) => self.hold = Hold::Idle,
                    // A chord (another button with the left) isn't a force click.
                    (_, true) if matches!(self.hold, Hold::Pending { .. }) => self.hold = Hold::Spent,
                    _ => {}
                }
                None
            }
            (EV_REL, REL_X | REL_Y) => self.hold.travel(value.unsigned_abs() as f64, MOUSE_SLOP, MOUSE_DRAG),
            (EV_REL, REL_WHEEL_HI_RES) => { self.wheel_hi_res = true; Some(Effect::Scroll(-(value as f32) / 120.0 * NOTCH)) }
            (EV_REL, REL_WHEEL) if !self.wheel_hi_res => Some(Effect::Scroll(-(value as f32) * NOTCH)),
            _ => None,
        }
    }

    pub fn deadline(&self, s: &Settings) -> Option<u64> { self.hold.deadline(s.hold_ms) }
    pub fn tick(&mut self, s: &Settings, now: u64) -> Option<Effect> { self.hold.tick(now, s.hold_ms, Source::MouseHold) }
}

const SLOTS: usize = 10;
/// Millimetres a tapping finger may move (libinput allows about 1.3; taps are sloppier with three).
const TAP_SLOP_MM: f64 = 3.0;
const TAP_MAX_MS: u64 = 300;
/// Millimetres a held press may wander before it isn't a force click, and travel once fired
/// before it counts as a drag.
const PRESS_SLOP_MM: f64 = 1.5;
const PRESS_DRAG_MM: f64 = 4.0;
/// Popover points per millimetre of two-finger travel.
const SCROLL_PER_MM: f32 = 7.0;

#[derive(Clone, Copy, Debug, Default)]
struct Slot { active: bool, x: i32, y: i32, start: (i32, i32), seen: bool }

/// A touch session: from the first finger down to the last finger up.
#[derive(Clone, Copy, Debug, Default)]
struct Session { since: u64, max_fingers: u8, moved: bool, clicked: bool }

pub struct Touchpad {
    /// Device units per millimetre, x and y.
    res: (f64, f64),
    slots: [Slot; SLOTS],
    slot: usize,
    tools: u8,
    touching: bool,
    fingers: u8,
    session: Option<Session>,
    hold: Hold,
    button: bool,
    /// Mean y of the two scrolling fingers at the last frame.
    scroll_y: Option<f64>,
    /// Frame-local: positions changed since the last report.
    dirty: bool,
}

impl Touchpad {
    pub fn new(res_x: f64, res_y: f64) -> Touchpad {
        Touchpad {
            res: (res_x.max(1.0), res_y.max(1.0)), slots: [Slot::default(); SLOTS], slot: 0, tools: 0, touching: false,
            fingers: 0, session: None, hold: Hold::Idle, button: false, scroll_y: None, dirty: false,
        }
    }

    fn tool_count(code: u16) -> Option<u8> {
        Some(match code { BTN_TOOL_FINGER => 1, BTN_TOOL_DOUBLETAP => 2, BTN_TOOL_TRIPLETAP => 3, BTN_TOOL_QUADTAP => 4, BTN_TOOL_QUINTTAP => 5, _ => return None })
    }

    fn mm(&self, a: (i32, i32), b: (i32, i32)) -> f64 {
        let dx = (a.0 - b.0) as f64 / self.res.0;
        let dy = (a.1 - b.1) as f64 / self.res.1;
        (dx * dx + dy * dy).sqrt()
    }

    pub fn event(&mut self, s: &Settings, kind: u16, code: u16, value: i32, now: u64) -> Vec<Effect> {
        match (kind, code) {
            (EV_ABS, ABS_MT_SLOT) => self.slot = (value.max(0) as usize).min(SLOTS - 1),
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                let slot = &mut self.slots[self.slot];
                if value < 0 { slot.active = false; } else { *slot = Slot { active: true, seen: false, ..*slot }; }
                self.dirty = true;
            }
            (EV_ABS, ABS_MT_POSITION_X) => { self.slots[self.slot].x = value; self.dirty = true; }
            (EV_ABS, ABS_MT_POSITION_Y) => { self.slots[self.slot].y = value; self.dirty = true; }
            (EV_KEY, BTN_TOUCH) => self.touching = value != 0,
            (EV_KEY, BTN_LEFT) => {
                self.button = value != 0;
                if let Some(session) = &mut self.session && self.button { session.clicked = true; }
                if self.button && s.force_click && self.fingers <= 1 { self.hold = Hold::press(now); }
                if !self.button { self.hold = Hold::Idle; }
            }
            (EV_KEY, c) if Self::tool_count(c).is_some() => {
                let n = Self::tool_count(c).unwrap();
                if value != 0 { self.tools = n } else if self.tools == n { self.tools = 0 }
            }
            (EV_SYN, SYN_REPORT) => return self.frame(s, now),
            (EV_SYN, SYN_DROPPED) => { self.session = None; self.scroll_y = None; self.hold = Hold::Idle; }
            _ => {}
        }
        Vec::new()
    }

    fn frame(&mut self, s: &Settings, now: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        let fingers = if self.touching { self.tools.max(1) } else { 0 };
        // New touches start where they first report.
        for slot in self.slots.iter_mut().filter(|s| s.active && !s.seen) { slot.start = (slot.x, slot.y); slot.seen = true; }
        let far = self.slots.iter().filter(|s| s.active).map(|s| self.mm((s.x, s.y), s.start)).fold(0.0, f64::max);

        // Tap sessions.
        if fingers > 0 && self.fingers == 0 { self.session = Some(Session { since: now, max_fingers: fingers, moved: false, clicked: self.button }); }
        if let Some(session) = &mut self.session {
            session.max_fingers = session.max_fingers.max(fingers);
            if far > TAP_SLOP_MM { session.moved = true; }
            if fingers == 0 {
                let tap = session.max_fingers == 3 && !session.moved && !session.clicked && now - session.since <= TAP_MAX_MS;
                self.session = None;
                if tap && s.three_finger_tap { out.push(Effect::Trigger(Source::ThreeFingerTap)); }
            }
        }

        // A press that started with one finger stops being a force click when a second lands.
        if fingers > 1 && matches!(self.hold, Hold::Pending { .. }) { self.hold = Hold::Spent; }
        if self.dirty && self.button {
            let primary = self.slots.iter().find(|s| s.active).copied();
            if let Some(p) = primary {
                // Travel since the press, measured from where the finger was when it went down.
                let d = self.mm((p.x, p.y), p.start);
                match &mut self.hold {
                    Hold::Pending { moved, .. } => { *moved = d; if d > PRESS_SLOP_MM { self.hold = Hold::Spent; } }
                    Hold::Fired { moved } => { if d - *moved > PRESS_DRAG_MM { self.hold = Hold::Spent; out.push(Effect::Dragged); } }
                    _ => {}
                }
            }
        }

        // Two-finger scrolling.
        if fingers == 2 && !self.button {
            let ys: Vec<f64> = self.slots.iter().filter(|s| s.active).map(|s| s.y as f64 / self.res.1).collect();
            if ys.len() == 2 {
                let y = (ys[0] + ys[1]) / 2.0;
                if let Some(last) = self.scroll_y {
                    let dy = (y - last) as f32 * SCROLL_PER_MM;
                    // Traditional scrolling: fingers moving up scroll toward later content.
                    let dy = if s.natural_scroll { dy } else { -dy };
                    if dy.abs() > 0.01 { out.push(Effect::Scroll(dy)); }
                }
                self.scroll_y = Some(y);
            }
        } else {
            self.scroll_y = None;
        }
        self.fingers = fingers;
        self.dirty = false;
        out
    }

    pub fn deadline(&self, s: &Settings) -> Option<u64> { self.hold.deadline(s.hold_ms) }

    pub fn tick(&mut self, s: &Settings, now: u64) -> Option<Effect> {
        let fired = self.hold.tick(now, s.hold_ms, Source::ForceClick);
        if fired.is_some() {
            // Drags measure from where the force click fired.
            let at = self.slots.iter().find(|s| s.active).map_or(0.0, |p| self.mm((p.x, p.y), p.start));
            self.hold = Hold::Fired { moved: at };
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RES: f64 = 10.0; // units per mm

    struct Pad { pad: Touchpad, s: Settings, t: u64, out: Vec<Effect> }

    impl Pad {
        fn new() -> Pad { Pad { pad: Touchpad::new(RES, RES), s: Settings::default(), t: 1000, out: Vec::new() } }
        fn ev(&mut self, kind: u16, code: u16, value: i32) { let e = self.pad.event(&self.s, kind, code, value, self.t); self.out.extend(e); }
        fn syn(&mut self) { self.ev(EV_SYN, SYN_REPORT, 0); }
        fn down(&mut self, slot: i32, x: i32, y: i32) {
            self.ev(EV_ABS, ABS_MT_SLOT, slot); self.ev(EV_ABS, ABS_MT_TRACKING_ID, slot + 10);
            self.ev(EV_ABS, ABS_MT_POSITION_X, x); self.ev(EV_ABS, ABS_MT_POSITION_Y, y);
        }
        fn up(&mut self, slot: i32) { self.ev(EV_ABS, ABS_MT_SLOT, slot); self.ev(EV_ABS, ABS_MT_TRACKING_ID, -1); }
        fn tools(&mut self, n: u8) {
            for (c, k) in [(BTN_TOOL_FINGER, 1), (BTN_TOOL_DOUBLETAP, 2), (BTN_TOOL_TRIPLETAP, 3)] {
                self.ev(EV_KEY, c, (k == n) as i32);
            }
            self.ev(EV_KEY, BTN_TOUCH, (n > 0) as i32);
        }
        fn wait(&mut self, ms: u64) {
            let end = self.t + ms;
            while let Some(d) = self.pad.deadline(&self.s).filter(|d| *d <= end) { self.t = d; let e = self.pad.tick(&self.s, self.t); self.out.extend(e); }
            self.t = end;
        }
        fn three_finger_tap(&mut self, hold: u64, travel: i32) {
            self.down(0, 100, 100); self.tools(1); self.syn();
            self.t += 20;
            self.down(1, 300, 100); self.down(2, 500, 100); self.tools(3); self.syn();
            self.t += hold;
            self.ev(EV_ABS, ABS_MT_SLOT, 1); self.ev(EV_ABS, ABS_MT_POSITION_X, 300 + travel); self.syn();
            self.up(0); self.up(1); self.up(2); self.tools(0); self.syn();
        }
    }

    #[test]
    fn a_quick_still_three_finger_tap_looks_up() {
        let mut p = Pad::new();
        p.three_finger_tap(80, 5);
        assert_eq!(p.out, vec![Effect::Trigger(Source::ThreeFingerTap)]);
    }

    #[test]
    fn slow_or_moving_three_finger_touches_are_gestures_not_taps() {
        let mut p = Pad::new();
        p.three_finger_tap(500, 0);
        p.three_finger_tap(80, 80); // 8 mm: a three-finger swipe
        assert!(p.out.is_empty(), "{:?}", p.out);
        p.s.three_finger_tap = false;
        p.three_finger_tap(80, 0);
        assert!(p.out.is_empty());
    }

    #[test]
    fn a_held_still_click_is_a_force_click_and_moving_after_it_is_a_drag() {
        let mut p = Pad::new();
        p.down(0, 100, 100); p.tools(1); p.syn();
        p.ev(EV_KEY, BTN_LEFT, 1); p.syn();
        p.wait(300);
        assert!(p.out.is_empty());
        p.ev(EV_ABS, ABS_MT_POSITION_X, 105); p.syn(); // 0.5 mm of pressure wobble
        p.wait(400);
        assert_eq!(p.out, vec![Effect::Trigger(Source::ForceClick)]);
        p.ev(EV_ABS, ABS_MT_POSITION_X, 160); p.syn();
        assert_eq!(p.out.last(), Some(&Effect::Dragged));
        p.ev(EV_KEY, BTN_LEFT, 0); p.syn();
    }

    #[test]
    fn a_click_that_drags_before_the_hold_never_fires() {
        let mut p = Pad::new();
        p.down(0, 100, 100); p.tools(1); p.syn();
        p.ev(EV_KEY, BTN_LEFT, 1); p.syn();
        p.t += 100;
        p.ev(EV_ABS, ABS_MT_POSITION_Y, 140); p.syn();
        p.wait(1000);
        assert!(p.out.is_empty(), "{:?}", p.out);
    }

    #[test]
    fn two_fingers_scroll() {
        let mut p = Pad::new();
        p.down(0, 100, 300); p.down(1, 300, 300); p.tools(2); p.syn();
        for i in 1..=3 {
            p.t += 10;
            p.ev(EV_ABS, ABS_MT_SLOT, 0); p.ev(EV_ABS, ABS_MT_POSITION_Y, 300 - i * 10);
            p.ev(EV_ABS, ABS_MT_SLOT, 1); p.ev(EV_ABS, ABS_MT_POSITION_Y, 300 - i * 10);
            p.syn();
        }
        let total: f32 = p.out.iter().map(|e| if let Effect::Scroll(d) = e { *d } else { 0.0 }).sum();
        assert!((total - 3.0 * SCROLL_PER_MM).abs() < 0.01, "fingers up 3 mm scroll down: {total}");
    }

    #[test]
    fn a_held_still_mouse_button_looks_up_and_jitter_is_forgiven() {
        let s = Settings::default();
        let mut m = Mouse::default();
        assert_eq!(m.event(&s, EV_KEY, BTN_LEFT, 1, 0), None);
        assert_eq!(m.event(&s, EV_REL, REL_X, 2, 100), None);
        assert_eq!(m.deadline(&s), Some(s.hold_ms));
        assert_eq!(m.tick(&s, s.hold_ms), Some(Effect::Trigger(Source::MouseHold)));
        assert_eq!(m.event(&s, EV_REL, REL_X, 20, s.hold_ms + 50), Some(Effect::Dragged));
        m.event(&s, EV_KEY, BTN_LEFT, 0, s.hold_ms + 100);

        // A click and a drag don't.
        m.event(&s, EV_KEY, BTN_LEFT, 1, 2000);
        m.event(&s, EV_KEY, BTN_LEFT, 0, 2100);
        assert_eq!(m.deadline(&s), None);
        m.event(&s, EV_KEY, BTN_LEFT, 1, 3000);
        m.event(&s, EV_REL, REL_Y, 30, 3100);
        assert_eq!(m.tick(&s, 3000 + s.hold_ms), None);
    }

    #[test]
    fn a_configured_side_button_looks_up_at_once_and_wheels_scroll() {
        let s = Settings { mouse_button: Some(BTN_SIDE), ..Settings::default() };
        let mut m = Mouse::default();
        assert_eq!(m.event(&s, EV_KEY, BTN_SIDE, 1, 0), Some(Effect::Trigger(Source::MouseButton)));
        assert_eq!(m.event(&s, EV_REL, REL_WHEEL, -1, 0), Some(Effect::Scroll(NOTCH)));
        assert_eq!(m.event(&s, EV_REL, REL_WHEEL_HI_RES, -60, 0), Some(Effect::Scroll(NOTCH / 2.0)));
        assert_eq!(m.event(&s, EV_REL, REL_WHEEL, -1, 0), None, "hi-res wheels report twice");
    }
}
