//! `~/.config/tlum/config.json`. Every field is optional; `tlum config` prints the
//! defaults merged with the file. `tlum settings` edits it, and a running daemon follows the
//! file as it changes (`watch`).

use std::sync::{Arc, RwLock};

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
    /// The popover's sources, in the order it shows them; each can be turned off. The built-in
    /// `dictionary`, `thesaurus` and `wikipedia`, and installed dictionaries by id (`wordnet`,
    /// `freedict-eng-deu`, …). Missing ones are added after these (`Config::sources`).
    pub dictionaries: Vec<Source>,
    /// Never use the network: only installed dictionaries answer.
    pub offline: bool,
    pub appearance: Appearance,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool { true }

/// The online sources, by id, with what the settings call them.
pub const BUILT_IN: [(&str, &str, &str); 3] = [
    ("dictionary", "Dictionary", "Wiktionary's definitions, or a downloaded dictionary's when it can't answer"),
    ("thesaurus", "Thesaurus", "Synonyms and opposites"),
    ("wikipedia", "Wikipedia", "The article's summary and picture"),
];

/// The popover's look, on top of its theme and under `style.css`. `None` keeps the theme's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// "default", "paper", "nord" or "minimal".
    pub theme: String,
    /// "system", "light" or "dark".
    pub scheme: String,
    /// Links and section titles.
    pub accent: Option<String>,
    /// The highlight behind the word.
    pub highlight: Option<String>,
    pub font: Option<String>,
    /// Every text size, times this.
    pub text_scale: f32,
    pub width: Option<f32>,
    pub max_height: Option<f32>,
    /// Definitions per part of speech.
    pub senses: Option<usize>,
    /// Lines of the Wikipedia summary.
    pub wiki_lines: Option<usize>,
    /// Parts shown or hidden (`None`: as the theme has it).
    pub pronunciation: Option<bool>,
    pub examples: Option<bool>,
    pub pictures: Option<bool>,
    pub marker: Option<bool>,
    pub arrow: Option<bool>,
    pub animations: Option<bool>,
    /// "Settings…" at the end of the popover.
    pub footer: Option<bool>,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            theme: "default".into(), scheme: "system".into(), accent: None, highlight: None, font: None, text_scale: 1.0,
            width: None, max_height: None, senses: None, wiki_lines: None,
            pronunciation: None, examples: None, pictures: None, marker: None, arrow: None, animations: None, footer: None,
        }
    }
}

impl Appearance {
    /// `Some(dark)` when the scheme is forced.
    pub fn forced_dark(&self) -> Option<bool> {
        match self.scheme.as_str() { "light" => Some(false), "dark" => Some(true), _ => None }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            backend: "auto".into(), hold_ms: 600, three_finger_tap: false, force_click: true, mouse_hold: true, mouse_button: None,
            natural_scroll: None, shortcut: "Meta+Ctrl+D".into(), language: "en".into(), ocr_language: "eng".into(),
            capture_width: 640.0, capture_height: 72.0, dictionaries: Vec::new(), offline: false, appearance: Appearance::default(),
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
        let mut c = Config::read().unwrap_or_else(|e| { eprintln!("tlum: ignoring {}: {e}", config_path().display()); Config::default() });
        if let Ok(b) = std::env::var("TLUM_BACKEND") { c.backend = b; }
        c
    }

    /// The file as written (no environment overrides); the defaults when there's none.
    pub fn read() -> Result<Config, String> {
        match std::fs::read_to_string(config_path()) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| e.to_string()),
            Err(_) => Ok(Config::default()),
        }
    }

    /// Writes the file whole, by renaming a new one over it, so a reader never sees half.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir).map_err(|e| e.to_string())?; }
        let tmp = path.with_extension("json.new");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap() + "\n").map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Every source in order: those listed, then built-in and installed ones not yet listed
    /// (on), less listed dictionaries no longer installed.
    pub fn sources(&self) -> Vec<Source> {
        let installed: Vec<String> = crate::library::installed().into_iter().map(|d| d.id).collect();
        let known = |id: &str| BUILT_IN.iter().any(|b| b.0 == id) || installed.iter().any(|i| i == id);
        let mut v: Vec<Source> = Vec::new();
        for s in &self.dictionaries {
            if known(&s.id) && !v.iter().any(|x| x.id == s.id) { v.push(s.clone()); }
        }
        for id in BUILT_IN.iter().map(|b| b.0.to_string()).chain(installed) {
            if !v.iter().any(|x| x.id == id) { v.push(Source { id, enabled: true }); }
        }
        v
    }

    /// The sources a lookup asks, in order: those on, less Wikipedia when offline (the
    /// dictionary and thesaurus stay: downloaded dictionaries answer them).
    pub fn active_sources(&self) -> Vec<String> {
        self.sources().into_iter().filter(|s| s.enabled && !(self.offline && s.id == "wikipedia")).map(|s| s.id).collect()
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

static CURRENT: RwLock<Option<Arc<Config>>> = RwLock::new(None);

/// The settings in effect, as last read.
pub fn current() -> Arc<Config> {
    if let Some(c) = CURRENT.read().unwrap().clone() { return c; }
    reload()
}

/// Reads the file again.
pub fn reload() -> Arc<Config> {
    let c = Arc::new(Config::load());
    *CURRENT.write().unwrap() = Some(c.clone());
    c
}

/// Calls `changed` (on its own thread) whenever `path` is saved. Editors save by writing in
/// place or by renaming a new file over it, so this watches the folder.
pub fn watch_file(path: std::path::PathBuf, changed: impl Fn() + Send + 'static) {
    let Some(dir) = path.parent().map(|d| d.to_path_buf()) else { return };
    let _ = std::fs::create_dir_all(&dir);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    std::thread::Builder::new().name(format!("watch {name}")).spawn(move || {
        #[allow(unsafe_code)]
        // SAFETY: plain inotify calls on a descriptor this thread owns.
        unsafe {
            let fd = libc::inotify_init1(libc::IN_CLOEXEC);
            if fd < 0 { return; }
            let c = std::ffi::CString::new(dir.to_string_lossy().as_bytes()).unwrap();
            libc::inotify_add_watch(fd, c.as_ptr(), libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE);
            let mut buf = [0u8; 4096];
            loop {
                let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
                if n <= 0 { break; }
                // Only our file: walk the events' names.
                let mut off = 0usize;
                let mut ours = false;
                while off + 16 <= n as usize {
                    let len = u32::from_ne_bytes(buf[off + 12..off + 16].try_into().unwrap()) as usize;
                    let raw = &buf[off + 16..(off + 16 + len).min(n as usize)];
                    let ev = String::from_utf8_lossy(&raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())]).into_owned();
                    ours |= ev == name;
                    off += 16 + len;
                }
                if !ours { continue; }
                // Let a burst of writes settle.
                std::thread::sleep(std::time::Duration::from_millis(80));
                changed();
            }
        }
    }).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_config_still_reads_and_lists_the_built_in_sources() {
        let c: Config = serde_json::from_str(r#"{ "hold_ms": 500, "language": "de" }"#).unwrap();
        assert_eq!(c.hold_ms, 500);
        assert_eq!(c.appearance, Appearance::default());
        let ids: Vec<String> = c.dictionaries.iter().map(|s| s.id.clone()).collect();
        assert!(ids.is_empty());
        let c: Config = serde_json::from_str(r#"{ "dictionaries": [{ "id": "wikipedia" }, { "id": "dictionary", "enabled": false }, { "id": "gone" }] }"#).unwrap();
        let s = c.sources();
        assert_eq!(s[0], Source { id: "wikipedia".into(), enabled: true });
        assert_eq!(s[1], Source { id: "dictionary".into(), enabled: false });
        assert_eq!(s[2].id, "thesaurus");
        assert!(!s.iter().any(|x| x.id == "gone"), "a dictionary no longer installed drops out");
        assert_eq!(c.active_sources()[0], "wikipedia");
        let offline = Config { offline: true, ..c };
        assert!(!offline.active_sources().iter().any(|s| s == "wikipedia"));
    }
}
