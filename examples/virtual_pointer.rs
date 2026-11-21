//! Test helper, not part of tlum: gives a headless wlroots compositor (sway) a pointer
//! through `wlr-virtual-pointer`, so tests can put it on a word. Reads `X Y W H` lines on
//! stdin (a point in an output of W×H) and moves the pointer there; exits on EOF.

use std::io::BufRead;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry::WlRegistry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

struct S;
impl Dispatch<WlRegistry, GlobalListContents> for S {
    fn event(_: &mut Self, _: &WlRegistry, _: wayland_client::protocol::wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(S: ignore WlSeat);
delegate_noop!(S: ZwlrVirtualPointerManagerV1);
delegate_noop!(S: ZwlrVirtualPointerV1);

fn main() {
    let conn = Connection::connect_to_env().expect("wayland");
    let (globals, mut q) = registry_queue_init::<S>(&conn).unwrap();
    let qh = q.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
    let mgr: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).expect("no wlr-virtual-pointer");
    let ptr = mgr.create_virtual_pointer(Some(&seat), &qh, ());
    q.roundtrip(&mut S).unwrap();
    for (t, line) in std::io::stdin().lock().lines().map_while(Result::ok).enumerate() {
        let n: Vec<u32> = line.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        if n.len() < 4 { continue; }
        ptr.motion_absolute(t as u32 * 10, n[0], n[1], n[2], n[3]);
        ptr.frame();
        q.roundtrip(&mut S).unwrap();
        eprintln!("virtual pointer at {} {}", n[0], n[1]);
    }
}
