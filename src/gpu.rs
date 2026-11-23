//! Loading only the Vulkan driver tlum draws with.
//!
//! wgpu asks the Vulkan loader for every GPU, and the loader loads every installed driver to
//! answer: on a laptop with Intel graphics and an NVIDIA card that is NVIDIA's driver (and with
//! it, its GL libraries), and Mesa's software renderer with LLVM, about 70 MB a process keeps
//! for its whole life, besides waking the NVIDIA card. Rune draws on the compositor's GPU (or,
//! without one it can name, a display's), so this names that GPU's driver to the loader
//! (`VK_LOADER_DRIVERS_SELECT`) before Rune starts. When anything is uncertain (a GPU whose
//! driver isn't known here, or loader or wgpu settings of the person's own) it changes nothing.

use std::path::{Path, PathBuf};

/// The Vulkan drivers (manifest file name patterns) for a kernel graphics driver.
fn vulkan_drivers(kernel: &str) -> Option<&'static str> {
    Some(match kernel {
        "i915" | "xe" => "*intel*",
        "amdgpu" | "radeon" => "*radeon*,*amd*",
        "nouveau" => "*nouveau*",
        "nvidia" => "*nvidia*",
        "virtio_gpu" | "virtio-pci" => "*virtio*",
        "msm" => "*freedreno*",
        "panfrost" | "panthor" => "*panfrost*",
        "v3d" | "vc4" => "*broadcom*",
        "asahi" => "*asahi*",
        "powervr" => "*powervr*",
        _ => return None,
    })
}

/// Settings that already choose drivers or adapters: then the person's choice stands.
const OVERRIDES: [&str; 8] = [
    "VK_LOADER_DRIVERS_SELECT", "VK_LOADER_DRIVERS_DISABLE", "VK_DRIVER_FILES", "VK_ICD_FILENAMES",
    "VK_ADD_DRIVER_FILES", "WGPU_ADAPTER_NAME", "WGPU_POWER_PREF", "WGPU_BACKEND",
];

/// Names the drawing GPU's Vulkan driver to the loader. Call it before any thread starts (it
/// sets an environment variable) and before Rune makes its GPU.
pub fn load_only_the_drawing_driver() {
    if OVERRIDES.iter().any(|v| std::env::var_os(v).is_some()) { return; }
    let devices = match compositor_device() {
        Some(d) => vec![d],
        None => displaying_devices(),
    };
    let mut patterns: Vec<&str> = Vec::new();
    for d in &devices {
        let Some(p) = kernel_driver(d).as_deref().and_then(vulkan_drivers) else { return };
        if !patterns.contains(&p) { patterns.push(p); }
    }
    if patterns.is_empty() { return; }
    let select = patterns.join(",");
    if std::env::var_os("TLUM_DEBUG").is_some() { eprintln!("tlum: Vulkan drivers {select}"); }
    // SAFETY: called first thing in `main`'s GUI commands, before any thread exists.
    unsafe { std::env::set_var("VK_LOADER_DRIVERS_SELECT", select) };
}

/// The kernel driver bound to a DRM device (`/sys/dev/char/226:0/device`, `/sys/class/drm/card1/device`).
fn kernel_driver(device: &Path) -> Option<String> {
    Some(std::fs::read_link(device.join("driver")).ok()?.file_name()?.to_str()?.to_string())
}

/// The cards with a display connected, as Rune falls back to off Wayland.
fn displaying_devices() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir("/sys/class/drm").into_iter().flatten().flatten().filter_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        // Connectors are `card1-eDP-1`; their card is `card1`.
        let (card, _) = name.split_once('-')?;
        let connected = std::fs::read_to_string(e.path().join("status")).is_ok_and(|s| s.trim() == "connected");
        connected.then(|| Path::new("/sys/class/drm").join(card).join("device"))
    }).collect();
    v.sort();
    v.dedup();
    v
}

/// The compositor's main device, from its default dmabuf feedback, as Rune finds it.
fn compositor_device() -> Option<PathBuf> {
    let dev = dmabuf::main_device()?;
    let dev = match dev.len() {
        8 => u64::from_ne_bytes(dev.try_into().ok()?),
        4 => u64::from(u32::from_ne_bytes(dev.try_into().ok()?)),
        _ => return None,
    };
    let major = ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0x0fff);
    let minor = ((dev >> 12) & 0xffff_ff00) | (dev & 0xff);
    Some(PathBuf::from(format!("/sys/dev/char/{major}:{minor}/device")))
}

mod dmabuf {
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_registry::WlRegistry;
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::{self, ZwpLinuxDmabufFeedbackV1};
    use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;

    #[derive(Default)]
    struct State { main: Option<Vec<u8>>, done: bool }

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ZwpLinuxDmabufV1, ()> for State {
        fn event(_: &mut Self, _: &ZwpLinuxDmabufV1, _: <ZwpLinuxDmabufV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }

    impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for State {
        fn event(state: &mut Self, _: &ZwpLinuxDmabufFeedbackV1, event: zwp_linux_dmabuf_feedback_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            match event {
                zwp_linux_dmabuf_feedback_v1::Event::MainDevice { device } => state.main = Some(device),
                zwp_linux_dmabuf_feedback_v1::Event::Done => state.done = true,
                _ => {}
            }
        }
    }

    /// The compositor's main device `dev_t`, on a short-lived connection. None off Wayland or
    /// before `zwp_linux_dmabuf_v1` version 4; version 6 stops sending it, so bind at most 5.
    pub(super) fn main_device() -> Option<Vec<u8>> {
        std::env::var_os("WAYLAND_DISPLAY")?;
        let conn = Connection::connect_to_env().ok()?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let dmabuf = globals.bind::<ZwpLinuxDmabufV1, _, _>(&queue.handle(), 4..=5, ()).ok()?;
        let feedback = dmabuf.get_default_feedback(&queue.handle(), ());
        let mut state = State::default();
        for _ in 0..4 {
            if state.done { break; }
            queue.roundtrip(&mut state).ok()?;
        }
        feedback.destroy();
        dmabuf.destroy();
        queue.flush().ok();
        state.main
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_kernel_drivers_name_their_vulkan_drivers_and_others_change_nothing() {
        assert_eq!(vulkan_drivers("i915"), Some("*intel*"));
        assert_eq!(vulkan_drivers("amdgpu"), Some("*radeon*,*amd*"));
        assert_eq!(vulkan_drivers("vmwgfx"), None, "a GPU without a known driver keeps every driver (lavapipe)");
    }
}
