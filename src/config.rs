//! `~/.config/tlum/config.json`. Every field is optional; `tlum config` prints the
//! defaults merged with the file.

use serde::{Deserialize, Serialize};

use crate::gesture::{self, Settings};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// How to find the pointer and read the screen: "auto", "kwin", "x11" or "wayland".
    pub backend: String,
    /// How long a still press counts as a force click, in milliseconds.
    pub hold_ms: u64,
    /// Touchpad: tap with three fingers (macOS: "Look up & data detectors: Tap with three fingers").
    pub three_finger_tap: bool,
    /// Touchpad: press the pad and hold it still (macOS: "Force Click with one finger").
    pub force_click: bool,
    /// Mouse: press the left button and hold it still.
    pub mouse_hold: bool,
    /// Mouse: a button that looks up at once: "side", "extra", "forward", "back", "middle" or null.
    pub mouse_button: Option<String>,
    /// Two-finger scrolling in the popover follows the fingers (null: read KDE's touchpad setting).
    pub natural_scroll: Option<bool>,
    /// The global shortcut KWin registers (System Settings → Shortcuts → KWin can change it).
    pub shortcut: String,
    /// Wikipedia and Wiktionary language.
    pub language: String,
    /// Tesseract language(s), e.g. "eng" or "eng+ukr".
    pub ocr_language: String,
    /// The area read around the pointer, in points.
    pub capture_width: f64,
    pub capture_height: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            backend: "auto".into(), hold_ms: 600, three_finger_tap: false, force_click: true, mouse_hold: true, mouse_button: None,
            natural_scroll: None, shortcut: "Meta+Ctrl+D".into(), language: "en".into(), ocr_language: "eng".into(),
            capture_width: 640.0, capture_height: 72.0,
        }
    }
}

pub fn config_path() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("tlum/config.json")
}

pub fn runtime_dir() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("tlum");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

impl Config {
    pub fn load() -> Config {
        let mut c = match std::fs::read_to_string(config_path()) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| { eprintln!("tlum: ignoring {}: {e}", config_path().display()); Config::default() }),
            Err(_) => Config::default(),
        };
        if let Ok(b) = std::env::var("TLUM_BACKEND") { c.backend = b; }
        c
    }

    pub fn gestures(&self) -> Settings {
        let mouse_button = self.mouse_button.as_deref().and_then(|b| Some(match b {
            "side" => gesture::BTN_SIDE, "extra" => gesture::BTN_EXTRA, "forward" => gesture::BTN_FORWARD,
            "back" => gesture::BTN_BACK, "middle" => gesture::BTN_MIDDLE, _ => return None,
        }));
        Settings {
            hold_ms: self.hold_ms.clamp(150, 3000), three_finger_tap: self.three_finger_tap, force_click: self.force_click,
            mouse_hold: self.mouse_hold, mouse_button, natural_scroll: self.natural_scroll.unwrap_or_else(kde_natural_scroll),
        }
    }
}

/// Whether any touchpad has natural scrolling on in KDE's input settings.
fn kde_natural_scroll() -> bool {
    let path = config_path().parent().and_then(|p| p.parent()).map(|p| p.join("kcminputrc"));
    path.and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|s| {
        let mut touchpad = false;
        s.lines().any(|l| {
            if l.starts_with('[') { touchpad = l.to_lowercase().contains("touchpad"); }
            touchpad && l.trim() == "NaturalScroll=true"
        })
    })
}
