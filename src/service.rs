//! The daemon's D-Bus face, `dev.tlum.Tlum` on the session bus: KWin's scripts call back
//! here with the pointer, and `tlum trigger` / `tlum define` drive a running daemon.

use std::sync::Mutex;
use std::sync::mpsc::Sender;

use crate::engine::{Engine, Job};
use crate::kwin;

pub struct Service {
    pub engine: Engine,
    pub pointer_replies: Mutex<Sender<String>>,
}

#[zbus::interface(name = "dev.tlum.Tlum")]
impl Service {
    /// A one-shot KWin script reporting the pointer.
    fn pointer(&self, at: String) { let _ = self.pointer_replies.lock().unwrap().send(at); }
    /// The KWin shortcut, with the pointer.
    fn trigger_at(&self, at: String) { self.engine.send(Job::Trigger(kwin::parse_pointer(&at), "shortcut")); }
    /// Look up what's under the pointer.
    fn trigger(&self) { self.engine.send(Job::Trigger(None, "command")); }
    /// Look up `text`, shown at the pointer.
    fn look_up(&self, text: String) { self.engine.send(Job::Text(text)); }
    fn close(&self) { crate::engine::close_all(); }
}
