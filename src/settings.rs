//! `tlum settings`: a window over `config.json`, in four pages:
//!
//! - **General**: whether tlum runs and starts at login, the gestures and the shortcut;
//! - **Dictionaries**: the popover's sources, on or off and in order (drag them), the lookup
//!   language, offline only, and dictionaries to download (WordNet, FreeDict) or add (StarDict);
//! - **Appearance**: the theme, scheme, colours, font, sizes and parts shown, with a live preview;
//! - **Advanced**: how the screen is read (backend, OCR languages, capture area), diagnostics,
//!   and resetting.
//!
//! Every change is saved at once, as macOS's settings are; a running daemon follows the file.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rune::{Align, AppSpec, Color, Cx, Drag, El, FileDialogOptions, Icon, Phase, ScrollSpec, Spring, TextEdit, Transition, Tx, col, el, icon, row, spacer, text};
use rune_kit::{Button, ButtonVariant, Checkbox, IconButton, IconButtonVariant, Kit, Progress, SegmentedControl, Select, Spinner, TextInput};

use crate::config::{self, Appearance, BUILT_IN, Config, Source};
use crate::icons;
use crate::dict::{Part, Section};
use crate::library::{self, Installed, Offer};
use crate::style::{self, Theme};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page { General, Dictionaries, Appearance, Advanced }

/// Each page: its name, what it's for, its icon and the icon's tile colour.
const PAGES: [(Page, &str, &str, &Icon, [u8; 3]); 4] = [
    (Page::General, "General", "Turning tlum on, and how you look words up", &icons::GEAR, [142, 142, 147]),
    (Page::Dictionaries, "Dictionaries", "Where definitions come from, and in what order", &icons::BOOK, [229, 72, 77]),
    (Page::Appearance, "Appearance", "How the popover looks", &icons::PALETTE, [10, 132, 255]),
    (Page::Advanced, "Advanced", "Reading the screen, diagnostics and resetting", &icons::SLIDERS, [99, 99, 102]),
];

fn page_named(name: &str) -> Option<Page> {
    PAGES.iter().find(|p| p.1.eq_ignore_ascii_case(name)).map(|p| p.0)
}

/// What background work hands back to the window.
enum Msg {
    Catalog(Result<Vec<Offer>, String>),
    Installed(String, Result<(), String>),
    Added(Result<String, String>),
    Doctor(String),
    Running(bool),
}

/// A source row being dragged to a new place.
#[derive(Clone, Copy)]
struct Reorder { from: usize, to: usize, dy: f32 }

pub struct Settings {
    page: Page,
    config: Config,
    /// The last save or action that failed.
    error: Option<String>,
    /// A passing note ("Added …"), and when it came; it hides after a few seconds.
    notice: Option<(String, Instant)>,
    /// Stopping or restarting the daemon.
    busy: bool,
    /// The select that's open.
    open: Option<&'static str>,
    force_dark: Option<bool>,

    installed: Vec<Installed>,
    catalog: Option<Result<Vec<Offer>, String>>,
    search: TextEdit,
    downloads: library::Progress,
    cancelled: Arc<Mutex<HashSet<String>>>,
    reorder: Option<Reorder>,

    running: Option<bool>,
    checked_at: Option<Instant>,
    /// Whether tlum registers its own shortcut here (KWin, X11), or the desktop binds one.
    own_shortcut: bool,
    autostart: bool,
    shortcut: TextEdit,

    themes: (Theme, Theme),
    presets: Vec<(Theme, Theme)>,
    user_css: Option<String>,
    css_warnings: Vec<String>,
    fonts: Vec<String>,

    ocr_languages: Vec<String>,
    doctor: Option<String>,
    confirm_reset: bool,

    inbox: Arc<Mutex<Vec<Msg>>>,
    waker: Option<rune::Waker>,
}

impl Settings {
    fn new() -> Settings {
        let (config, error) = match Config::read() {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("{} couldn't be read ({e}); saving will replace it.", config::config_path().display()))),
        };
        let user_css = std::fs::read_to_string(style::user_path()).ok();
        let (light, dark, css_warnings) = style::build_with(&config.appearance, user_css.as_deref());
        let presets = style::THEMES.iter().map(|(id, _, _)| {
            let (l, d, _) = style::build_with(&Appearance { theme: id.to_string(), ..Appearance::default() }, None);
            (l, d)
        }).collect();
        let own_shortcut = matches!(crate::desktop::kind(&config.backend, zbus::blocking::Connection::session().ok().as_ref()), crate::desktop::Kind::KWin | crate::desktop::Kind::X11);
        Settings {
            page: Page::General, shortcut: TextEdit::new(config.shortcut.clone()), config, error, notice: None, busy: false, open: None, force_dark: None,
            installed: library::installed(), catalog: None, search: TextEdit::new(""), downloads: Arc::default(), cancelled: Arc::default(), reorder: None,
            running: None, checked_at: None,
            own_shortcut,
            autostart: crate::install::autostart_enabled(),
            themes: (light, dark), presets, user_css, css_warnings, fonts: Vec::new(),
            ocr_languages: crate::ocr::installed_languages(), doctor: None, confirm_reset: false,
            inbox: Arc::default(), waker: None,
        }
    }

    /// Runs `work` on a thread; its answer reaches `poll` through the inbox.
    fn spawn(&self, work: impl FnOnce() -> Msg + Send + 'static) {
        let (inbox, waker) = (self.inbox.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let m = work();
            inbox.lock().unwrap().push(m);
            if let Some(w) = waker { w(); }
        });
    }

    /// Saves the settings, and rebuilds the preview's themes.
    fn save(&mut self) {
        self.restyle();
        if let Err(e) = self.config.save() { self.error = Some(format!("Couldn't save the settings: {e}")); } else { self.error = None; }
    }

    fn restyle(&mut self) {
        let (l, d, w) = style::build_with(&self.config.appearance, self.user_css.as_deref());
        self.themes = (l, d);
        self.css_warnings = w;
    }

    fn note(&mut self, m: impl Into<String>) { self.notice = Some((m.into(), Instant::now())); }

    fn set(&mut self, f: impl FnOnce(&mut Config)) {
        f(&mut self.config);
        self.save();
    }

    fn check_running(&mut self) {
        self.checked_at = Some(Instant::now());
        self.spawn(|| Msg::Running(daemon_running()));
    }

    fn load_catalog(&mut self, refresh: bool) {
        self.catalog = None;
        self.spawn(move || Msg::Catalog(library::catalog(refresh)));
    }

    /// The installed list changed: list it again, and put new dictionaries in the order.
    fn refresh_installed(&mut self) {
        self.installed = library::installed();
        self.config.dictionaries = self.config.sources();
        self.save();
    }

    fn go(&mut self, page: Page) {
        self.page = page;
        self.open = None;
        self.confirm_reset = false;
        if page == Page::Dictionaries && self.catalog.is_none() { self.load_catalog(false); }
        if page == Page::Appearance && self.fonts.is_empty() { self.fonts = font_families(); }
    }
}

fn daemon_running() -> bool {
    zbus::blocking::Connection::session().ok()
        .and_then(|c| zbus::blocking::fdo::DBusProxy::new(&c).ok().and_then(|d| d.name_has_owner(crate::kwin::SERVICE.try_into().ok()?).ok()))
        .unwrap_or(false)
}

/// Font families fontconfig knows, for the font menu.
fn font_families() -> Vec<String> {
    let out = std::process::Command::new("fc-list").args([":", "family"]).output().ok();
    let mut v: Vec<String> = out.map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.split(',').next()).map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty() && !f.starts_with('.')).collect()).unwrap_or_default();
    v.sort_by_key(|f| f.to_lowercase());
    v.dedup();
    v
}

fn setup(s: &mut Settings, waker: rune::Waker) {
    s.waker = Some(waker);
    s.check_running();
    if s.page == Page::Dictionaries && s.catalog.is_none() { s.load_catalog(false); }
}

fn poll(s: &mut Settings, _: f64, tx: &mut Tx) -> bool {
    let msgs: Vec<Msg> = std::mem::take(&mut *s.inbox.lock().unwrap());
    let mut changed = !msgs.is_empty();
    for m in msgs {
        match m {
            Msg::Catalog(c) => s.catalog = Some(c),
            Msg::Installed(id, r) => {
                s.downloads.lock().unwrap().remove(&id);
                s.cancelled.lock().unwrap().remove(&id);
                match r {
                    Ok(()) => { s.refresh_installed(); s.note(format!("Installed {}.", name_of(s, &id))); }
                    Err(e) if e == "cancelled" => {}
                    Err(e) => { s.downloads.lock().unwrap().insert(id, Err(e)); }
                }
            }
            Msg::Added(r) => match r {
                Ok(id) => { s.refresh_installed(); s.note(format!("Added {}.", name_of(s, &id))); }
                Err(e) => s.error = Some(e),
            },
            Msg::Doctor(out) => s.doctor = Some(out),
            Msg::Running(on) => { s.running = Some(on); s.busy = false; }
        }
    }
    // Downloads report progress through the shared map; redraw while any runs.
    if s.downloads.lock().unwrap().values().any(|d| d.is_ok()) { changed = true; tx.wake_after(Duration::from_millis(250)); }
    if s.notice.as_ref().is_some_and(|n| n.1.elapsed() > Duration::from_secs(4)) { s.notice = None; changed = true; }
    if s.notice.is_some() { tx.wake_after(Duration::from_secs(1)); }
    // Whether the daemon runs, every few seconds.
    if !s.busy && s.checked_at.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) { s.check_running(); }
    tx.wake_after(Duration::from_secs(3));
    changed
}

fn on_key(s: &mut Settings, e: &rune::KeyEvent, _: &mut Tx) -> bool {
    if e.key == rune::Key::Escape && s.open.is_some() { s.open = None; return true; }
    false
}

fn name_of(s: &Settings, id: &str) -> String {
    BUILT_IN.iter().find(|b| b.0 == id).map(|b| b.1.to_string())
        .or_else(|| s.installed.iter().find(|d| d.id == id).map(|d| d.name.clone()))
        .unwrap_or_else(|| id.to_string())
}

// --- building blocks ----------------------------------------------------------------------------

fn accent(dark: bool) -> Color { rune::tok::accent(0, dark) }

fn rgb(r: u8, g: u8, b: u8) -> Color { style::Rgba([r, g, b, 255]).color() }

fn title(k: &Kit, s: &str) -> El { text(s).font(k.ty.body_medium).color(k.c.text_strong).wrap() }
fn detail(k: &Kit, s: &str) -> El { text(s).font(k.ty.text_small).color(k.c.text_weak).wrap() }

/// A rounded group of rows with hairlines between them, under an optional heading.
fn card(k: &Kit, key: &str, heading: Option<&str>, rows: Vec<El>) -> El {
    let n = rows.len();
    let mut c = col().key(format!("{key}-card")).w_full().radius(10.0).fill(k.c.surface_raised_base).border(1.0, k.c.border_weak).clip_rounded(10.0);
    for (i, r) in rows.into_iter().enumerate() {
        c = c.child(r);
        if i + 1 < n { c = c.child(el().w_full().h(1.0).fill(k.c.border_weak).a11y_hidden()); }
    }
    let mut out = col().key(key.to_string()).w_full().gap(8.0);
    if let Some(h) = heading { out = out.child(text(h).font(k.ty.small_medium).color(k.c.text_weak).pl(4.0)); }
    out.child(c)
}

/// A setting: its name and explanation, and its control at the end.
fn setting(k: &Kit, key: &str, label: &str, about: Option<&str>, control: El) -> El {
    let mut words = col().grow(1.0).min_w(0.0).gap(2.0).child(title(k, label));
    if let Some(a) = about { words = words.child(detail(k, a)); }
    row().key(key.to_string()).w_full().min_h(48.0).px(14.0).py(9.0).gap(16.0).center_y().child(words).child(control)
}

/// A switch, in opaque colours that read on any card.
fn switch(k: &Kit, label: &str, on: bool, flip: impl Fn(&mut Settings) + 'static) -> El {
    let t = rune::tok::theme(k.dark);
    let (w, h, knob) = (38.0, 22.0, 18.0);
    let inset = (h - knob) / 2.0;
    el().key(format!("switch-{label}")).role("switch").label(label).checked(on).focusable(true)
        .size(w, h).radius(h / 2.0).fill(if on { accent(k.dark) } else { t.control_off })
        .motion(rune::tok::motion::SNAPPY)
        .on_press(flip)
        .child(el().key("knob").size(knob, knob).absolute(if on { w - knob - inset } else { inset }, inset).radius(knob / 2.0)
            .fill(t.knob).shadow(1.0, 3.0, t.knob_shadow).shadow(0.0, 0.5, t.edge).a11y_hidden())
}

fn toggle(k: &Kit, key: &str, label: &str, about: Option<&str>, on: bool, flip: impl Fn(&mut Settings) + 'static) -> El {
    setting(k, key, label, about, switch(k, label, on, flip))
}

/// A select bound to `s.open`.
fn select(k: &Kit, layer: &mut Vec<El>, s: &Settings, key: &'static str, label: &str, options: &[String], current: &str, width: f32,
          choose: impl Fn(&mut Settings, &str) + Clone + 'static) -> El {
    Select::new(key, options).label(label).current(Some(current)).width(width).open(s.open == Some(key))
        .on_toggle(move |s: &mut Settings| s.open = if s.open == Some(key) { None } else { Some(key) })
        .on_close(|s: &mut Settings| s.open = None)
        .on_choose(move |s: &mut Settings, v: &str, _| { s.open = None; choose(s, v); })
        .build(k, layer)
}

/// A slider over `min..=max` in `step`s. It shows its value as `show` says, applies as it
/// moves (the preview follows) and saves when let go.
#[allow(clippy::too_many_arguments)]
fn slider(k: &Kit, key: &str, label: &str, value: f32, (min, max, step): (f32, f32, f32), show: String,
          get: fn(&Settings) -> f32, set: fn(&mut Settings, f32)) -> El {
    let (w, h, knob, bar) = (150.0, 22.0, 18.0, 4.0);
    let travel = w - knob;
    let frac = ((value - min) / (max - min)).clamp(0.0, 1.0);
    let snap = move |v: f32| ((v - min) / step).round() * step + min;
    let at = move |x: f32| snap(min + ((x - knob / 2.0) / travel).clamp(0.0, 1.0) * (max - min)).clamp(min, max);
    let dark = k.dark;
    let control = el().key(format!("{key}-slider")).role("slider").label(label).focusable(true)
        .range_value(f64::from(value), f64::from(min), f64::from(max)).range_step(f64::from(step)).a11y_value(show.clone())
        .on_step(move |s: &mut Settings, steps, _| { let v = (get(s) + steps as f32 * step).clamp(min, max); set(s, v); s.save(); })
        .size(w, h)
        .on_drag(move |s: &mut Settings, d: &mut Drag, tx| match d.phase {
            Phase::Down => { d.data = get(s); set(s, at(d.x)); s.restyle(); }
            Phase::Move => { set(s, at(d.x)); s.restyle(); tx.instant(); }
            Phase::Up => { if d.cancelled() { set(s, d.data); } s.save(); }
        })
        .child(el().size(w, bar).absolute(0.0, (h - bar) / 2.0).radius(bar / 2.0).fill(k.c.border_base).a11y_hidden())
        .child(el().size(knob / 2.0 + frac * travel, bar).absolute(0.0, (h - bar) / 2.0).radius(bar / 2.0).fill(accent(dark)).a11y_hidden())
        .child(el().size(knob, knob).absolute(frac * travel, (h - knob) / 2.0).radius(knob / 2.0).fill(k.c.knob)
            .shadow(1.0, 3.0, Color::BLACK.alpha(0.3)).a11y_hidden());
    let readout = text(show).font(k.ty.text_small).color(k.c.text_weak).w(52.0).align(Align::End).a11y_hidden();
    setting(k, key, label, None, row().gap(10.0).center_y().child(control).child(readout))
}

fn button(k: &Kit, label: &str, variant: ButtonVariant, press: impl Fn(&mut Settings, &mut Tx) + 'static) -> El {
    Button::new(label).variant(variant).on_press_tx(press).build(k)
}

// --- the window ---------------------------------------------------------------------------------

fn view(s: &Settings, cx: &mut Cx<Settings>) -> El {
    let dark = s.force_dark.unwrap_or_else(|| cx.system_dark());
    let k = Kit::neutral(dark);
    let mut layer = Vec::new();
    let (_, heading, about, _, _) = PAGES.iter().find(|p| p.0 == s.page).copied().unwrap();
    let body = match s.page {
        Page::General => general(s, &k, &mut layer),
        Page::Dictionaries => dictionaries(s, &k, &mut layer),
        Page::Appearance => appearance(s, &k, &mut layer, cx.system_dark()),
        Page::Advanced => advanced(s, &k, &mut layer),
    };
    let mut banner = col().w_full().gap(6.0);
    if let Some(e) = &s.error {
        banner = banner.child(row().w_full().px(12.0).py(8.0).radius(8.0).fill(k.c.danger.bg).gap(8.0).center_y()
            .child(text(e).font(k.ty.text_small).color(k.c.danger.fg).wrap().grow(1.0).min_w(0.0))
            .child(Button::new("Dismiss").variant(ButtonVariant::Ghost).on_press(|s: &mut Settings| s.error = None).build(&k)));
    }
    let max = if s.page == Page::Appearance { 10_000.0 } else { 720.0 };
    let header = col().w_full().max_w(max).px(28.0).pt(26.0).pb(16.0).gap(3.0)
        .child(text(heading).font(rune::TextStyle::new(24.0, 700.0).tracking(-0.3)).color(k.c.text_strong))
        .child(text(about).font(k.ty.ui).color(k.c.text_weak))
        .child(banner.pt(8.0));
    let mut main = col().key("main").grow(1.0).min_w(0.0).h_full().fill(k.c.bg_base).items_center().child(header).child(body);
    // A passing note floats over the bottom of the page and fades away.
    if let Some((n, _)) = &s.notice {
        let toast = row().key(format!("toast-{n}")).gap(8.0).center_y().px(14.0).py(9.0).radius(999.0)
            .fill(k.c.surface_float).border(1.0, k.c.border_weak).shadow(6.0, 24.0, Color::BLACK.alpha(0.22))
            .transition(Transition::opacity())
            .child(icon(&icons::CHECK, 14.0).color(k.c.success.fg).a11y_hidden())
            .child(text(n).font(k.ty.ui_medium).color(k.c.text_strong).single_line())
            .role("status").label(n.clone());
        main = main.child(row().w_full().justify_center().pin(Some(0.0), None, Some(0.0), Some(22.0)).child(toast));
    }
    rune_kit::root(row().w_full().h_full().child(sidebar(s, &k)).child(main), layer)
}

fn sidebar(s: &Settings, k: &Kit) -> El {
    let mut c = col().key("sidebar").w(214.0).h_full().fill(k.c.bg_deep).pt(22.0).px(10.0).gap(2.0)
        .child(row().px(8.0).pb(18.0).gap(10.0).center_y()
            .child(app_tile(30.0))
            .child(col().gap(0.0)
                .child(text("tlum").font(rune::TextStyle::new(15.0, 700.0)).color(k.c.text_strong))
                .child(text("Look Up for Linux").font(k.ty.text_small).color(k.c.text_weak))));
    let pick = accent(k.dark);
    for (page, name, _, glyph, color) in PAGES {
        let selected = s.page == page;
        let mut r = row().key(format!("nav-{name}")).role("tab").label(name).checked(selected).focusable(true).w_full().h(34.0).px(8.0).gap(10.0).radius(8.0).center_y()
            .on_press(move |s: &mut Settings| s.go(page))
            .child(tile_icon(22.0, color, glyph))
            .child(text(name).font(k.ty.ui_medium).color(if selected { Color::WHITE } else { k.c.text_strong }).a11y_hidden());
        r = if selected { r.fill(pick) } else { k.interactive(r) };
        c = c.child(r);
    }
    let (dot, label) = match s.running {
        Some(true) => (k.c.success.fg, "Running"),
        Some(false) => (k.c.text_faint, "Not running"),
        None => (Color::TRANSPARENT, ""),
    };
    c.child(spacer()).child(col().px(10.0).pb(16.0).gap(3.0)
        .child(row().gap(6.0).center_y().child(el().size(7.0, 7.0).radius(4.0).fill(dot)).child(text(label).font(k.ty.text_small).color(k.c.text_weak)))
        .child(text(concat!("Version ", env!("CARGO_PKG_VERSION"))).font(k.ty.text_small).color(k.c.text_faint)))
}

/// A page's scrolling column of cards.
fn page(key: &str, children: Vec<El>) -> El {
    page_max(key, 720.0, children)
}

/// [`page`], at most `max` points wide, centred.
fn page_max(key: &str, max: f32, children: Vec<El>) -> El {
    let mut c = col().w_full().max_w(max).px(28.0).pb(40.0).pt(4.0).gap(24.0);
    for ch in children { c = c.child(ch); }
    col().key(format!("{key}-scroll")).w_full().grow(1.0).min_h(0.0).items_center().scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(10.0).child(c)
}

/// A rounded tile holding an icon or a few letters, as macOS's settings mark each item.
fn tile(size: f32, color: Color, inside: El) -> El {
    el().size(size, size).radius((size * 0.26).round()).fill(color).center().child(inside)
}

fn tile_icon(size: f32, rgb3: [u8; 3], i: &'static Icon) -> El {
    tile(size, rgb(rgb3[0], rgb3[1], rgb3[2]), icon(i, (size * 0.62).round()).color(Color::WHITE).a11y_hidden())
}

fn tile_text(size: f32, rgb3: [u8; 3], s: &str, font: rune::TextStyle) -> El {
    tile(size, rgb(rgb3[0], rgb3[1], rgb3[2]), text(s).font(font).color(Color::WHITE).single_line().a11y_hidden())
}

/// tlum's own mark: the word on its yellow highlight.
fn app_tile(size: f32) -> El {
    tile(size, rgb(255, 219, 66), text("Aa").font(rune::TextStyle::new((size * 0.46).round(), 700.0)).color(rgb(34, 28, 5)).single_line().a11y_hidden())
}

// --- General ------------------------------------------------------------------------------------

const MOUSE_BUTTONS: [(&str, &str); 6] = [("", "None"), ("side", "Side button (back)"), ("extra", "Extra button (forward)"), ("back", "Back"), ("forward", "Forward"), ("middle", "Middle button")];

fn general(s: &Settings, k: &Kit, layer: &mut Vec<El>) -> El {
    let c = &s.config;
    let (status, about) = match (s.busy, s.running) {
        (true, _) => ("Working…", "One moment."),
        (_, Some(true)) => ("tlum is running", "Changes you make here apply right away."),
        (_, Some(false)) => ("tlum isn't running", "Words can't be looked up until it starts."),
        (_, None) => ("Checking…", ""),
    };
    let controls = match (s.busy, s.running) {
        (true, _) | (_, None) => row().child(Spinner::new().build(k)),
        (_, Some(true)) => row().gap(8.0)
            .child(button(k, "Stop", ButtonVariant::Ghost, |s, _| restart(s, false)))
            .child(button(k, "Restart", ButtonVariant::Neutral, |s, _| restart(s, true))),
        (_, Some(false)) => row().child(button(k, "Start tlum", ButtonVariant::Contrast, |s, _| restart(s, true))),
    };
    let mut words = col().grow(1.0).min_w(0.0).gap(2.0).child(text(status).font(rune::TextStyle::new(15.0, 650.0)).color(k.c.text_strong));
    if !about.is_empty() { words = words.child(detail(k, about)); }
    let running = row().key("daemon").w_full().px(14.0).py(14.0).gap(14.0).center_y()
        .child(app_tile(44.0)).child(words).child(controls);
    let login = toggle(k, "login", "Open at login", Some("Start tlum when you log in, so it's always ready."), s.autostart, |s| {
        match crate::install::set_autostart(!s.autostart) {
            Ok(()) => s.autostart = !s.autostart,
            Err(e) => s.error = Some(format!("Couldn't change starting at login: {e}")),
        }
    });

    let hold = c.hold_ms as f32;
    let triggers = vec![
        toggle(k, "force-click", "Force click", Some("Touchpad: press the pad down on a word and hold it still."), c.force_click, |s| s.set(|c| c.force_click = !c.force_click)),
        toggle(k, "three-finger", "Tap with three fingers", Some("Touchpad. libinput may also paste on a three-finger tap."), c.three_finger_tap, |s| s.set(|c| c.three_finger_tap = !c.three_finger_tap)),
        toggle(k, "mouse-hold", "Press and hold", Some("Mouse: hold the left button still on a word."), c.mouse_hold, |s| s.set(|c| c.mouse_hold = !c.mouse_hold)),
        slider(k, "hold", "Hold for", hold, (250.0, 1500.0, 50.0), format!("{hold:.0} ms"), |s| s.config.hold_ms as f32, |s, v| s.config.hold_ms = v as u64),
        setting(k, "mouse-button", "Mouse button", Some("A button that looks up the word under the pointer at once."), {
            let names: Vec<String> = MOUSE_BUTTONS.iter().map(|b| b.1.to_string()).collect();
            let cur = MOUSE_BUTTONS.iter().find(|b| b.0 == c.mouse_button.as_deref().unwrap_or("")).map_or("None", |b| b.1);
            select(k, layer, s, "mouse-button", "Mouse button", &names, cur, 200.0, |s, v| {
                let id = MOUSE_BUTTONS.iter().find(|b| b.1 == v).map(|b| b.0).unwrap_or("");
                s.set(|c| c.mouse_button = (!id.is_empty()).then(|| id.to_string()));
            })
        }),
    ];

    let shortcut = if s.own_shortcut {
        let changed = s.shortcut.text.trim() != c.shortcut;
        let field = TextInput::new(&s.shortcut).label("Keyboard shortcut").placeholder("Meta+Ctrl+D").width(Some(170.0))
            .on_edit(|s: &mut Settings| &mut s.shortcut)
            .on_submit(|s: &mut Settings, _| { let v = s.shortcut.text.trim().to_string(); if !v.is_empty() { s.set(|c| c.shortcut = v); } })
            .build(k);
        let mut control = row().gap(8.0).center_y().child(field);
        if changed {
            control = control.child(button(k, "Apply", ButtonVariant::Neutral, |s, _| {
                let v = s.shortcut.text.trim().to_string();
                if !v.is_empty() { s.set(|c| c.shortcut = v); }
            }));
        }
        setting(k, "shortcut", "Keyboard shortcut", Some("Looks up the word under the pointer. Modifiers and a key, like Meta+Ctrl+D; tlum restarts to take it."), control)
    } else {
        setting(k, "shortcut", "Keyboard shortcut", Some(&format!("This desktop doesn't let tlum take a shortcut itself: {}", crate::install::shortcut_hint())), el())
    };
    let natural = setting(k, "natural", "Scrolling in the popover", Some("Which way two-finger scrolling moves it."), {
        let names = ["Follow the system".to_string(), "Natural".to_string(), "Traditional".to_string()];
        let cur = match c.natural_scroll { None => "Follow the system", Some(true) => "Natural", Some(false) => "Traditional" };
        select(k, layer, s, "natural", "Scrolling", &names, cur, 170.0, |s, v| s.set(|c| c.natural_scroll = match v { "Natural" => Some(true), "Traditional" => Some(false), _ => None }))
    });

    page("general", vec![
        card(k, "status", None, vec![running, login]),
        card(k, "triggers", Some("Look up with"), triggers),
        card(k, "keys", Some("Keyboard and scrolling"), vec![shortcut, natural]),
        detail(k, "In the popover: Escape or a click outside closes it, Return opens the full entry, Ctrl+C copies the word. A selected phrase is looked up whole."),
    ])
}

/// Stops the daemon, and starts it again when `start`.
fn restart(s: &mut Settings, start: bool) {
    s.busy = true;
    s.spawn(move || {
        if daemon_running() {
            let _ = crate::call("Quit", None);
            for _ in 0..40 { if !daemon_running() { break; } std::thread::sleep(Duration::from_millis(50)); }
        }
        if start {
            let null = std::process::Stdio::null;
            if let Ok(mut child) = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).stdin(null()).stdout(null()).stderr(null()).spawn()) {
                std::thread::spawn(move || child.wait());
            }
            for _ in 0..60 { if daemon_running() { break; } std::thread::sleep(Duration::from_millis(50)); }
        }
        Msg::Running(daemon_running())
    });
}

// --- Dictionaries -------------------------------------------------------------------------------

const ROW: f32 = 58.0;

/// Wikipedia and Wiktionary languages offered (any other code can go in the config file).
const LANGUAGES: [(&str, &str); 24] = [
    ("en", "English"), ("de", "German"), ("fr", "French"), ("es", "Spanish"), ("it", "Italian"), ("pt", "Portuguese"),
    ("nl", "Dutch"), ("pl", "Polish"), ("uk", "Ukrainian"), ("ru", "Russian"), ("cs", "Czech"), ("sv", "Swedish"),
    ("da", "Danish"), ("fi", "Finnish"), ("no", "Norwegian"), ("el", "Greek"), ("tr", "Turkish"), ("hu", "Hungarian"),
    ("ro", "Romanian"), ("ja", "Japanese"), ("zh", "Chinese"), ("ko", "Korean"), ("ar", "Arabic"), ("he", "Hebrew"),
];

fn source_row(s: &Settings, k: &Kit, i: usize, src: &Source) -> El {
    let builtin = BUILT_IN.iter().find(|b| b.0 == src.id);
    let installed = s.installed.iter().find(|d| d.id == src.id);
    let name = builtin.map(|b| b.1.to_string()).or_else(|| installed.map(|d| d.name.clone())).unwrap_or_else(|| src.id.clone());
    let off = s.config.offline && src.id == "wikipedia";
    let offline_answer = || {
        let lang = library::iso3(&s.config.language);
        s.installed.iter().filter(|d| d.language.as_deref() == Some(lang)).min_by_key(|d| d.id != "wordnet").map(|d| d.name.clone())
    };
    let about = if off {
        "Off while “Offline only” is on".to_string()
    } else if s.config.offline && src.id == "dictionary" {
        match offline_answer() {
            Some(n) => format!("Offline · answered by {n}"),
            None => "Offline · download a dictionary below to answer it".to_string(),
        }
    } else if s.config.offline && src.id == "thesaurus" {
        if s.installed.iter().any(|d| d.id == "wordnet") { "Offline · answered by WordNet".into() } else { "Offline · download WordNet below to answer it".into() }
    } else if src.id == "dictionary" && let Some(n) = offline_answer() {
        format!("Online · Wiktionary, or {n} when it can't answer")
    } else if let Some(b) = builtin {
        format!("Online · {}", b.2)
    } else if let Some(d) = installed {
        format!("{} · {}", d.detail, library::human_size(d.size))
    } else {
        String::new()
    };
    let id = src.id.clone();
    let check = Checkbox::new(src.enabled && !off).label(name.clone()).hide_label().disabled(off)
        .on_toggle(move |s: &mut Settings| {
            let mut list = s.config.sources();
            if let Some(x) = list.iter_mut().find(|x| x.id == id) { x.enabled = !x.enabled; }
            s.set(|c| c.dictionaries = list);
        }).build(k);
    let grip = el().key("grip").size(22.0, ROW).center().cursor(rune::Cursor::Grab).label(format!("Move {name}")).role("button")
        .child(icon(&icons::GRIP, 16.0).color(k.c.text_faint).a11y_hidden())
        .on_drag(move |s: &mut Settings, d: &mut Drag, tx| {
            let n = s.config.sources().len();
            match d.phase {
                Phase::Down => s.reorder = Some(Reorder { from: i, to: i, dy: 0.0 }),
                Phase::Move => if let Some(r) = &mut s.reorder {
                    r.dy = d.dy;
                    r.to = (r.from as f32 + (d.dy / ROW).round()).clamp(0.0, n.saturating_sub(1) as f32) as usize;
                },
                Phase::Up => {
                    let r = s.reorder.take();
                    if let Some(r) = r.filter(|r| !d.cancelled() && r.from != r.to) {
                        let mut list = s.config.sources();
                        let item = list.remove(r.from);
                        list.insert(r.to, item);
                        s.set(|c| c.dictionaries = list);
                    }
                    tx.instant();
                }
            }
        });
    let mut words = col().grow(1.0).min_w(0.0).gap(2.0).child(title(k, &name).opacity(if off { 0.5 } else { 1.0 }));
    if !about.is_empty() { words = words.child(text(about).font(k.ty.text_small).color(k.c.text_weak).truncate()); }
    let mut r = row().key(format!("src-{}", src.id)).w_full().h(ROW).pl(4.0).pr(10.0).gap(10.0).center_y()
        .child(grip).child(check).child(source_badge(&src.id, installed).opacity(if off { 0.45 } else { 1.0 })).child(words);
    if installed.is_some() {
        let id = src.id.clone();
        r = r.child(IconButton::new(&icons::TRASH).label(format!("Remove {name}")).help("Remove this dictionary").variant(IconButtonVariant::Ghost)
            .on_press(move |s: &mut Settings| {
                match library::remove(&id) {
                    Ok(()) => { s.note(format!("Removed {}.", name_of(s, &id))); s.refresh_installed(); }
                    Err(e) => s.error = Some(format!("Couldn't remove it: {e}")),
                }
            }).build(k));
    }
    // A hairline above every row but the first.
    if i > 0 { r = r.child(el().h(1.0).fill(k.c.border_weak).pin(Some(62.0), Some(0.0), Some(0.0), None).a11y_hidden()); }
    // Dragging: the row follows the pointer over the others, which step aside.
    if let Some(rd) = s.reorder {
        if rd.from == i {
            r = r.offset(0.0, rd.dy).raise().fill(k.c.surface_raised_hover).shadow(4.0, 16.0, Color::BLACK.alpha(0.25)).radius(8.0);
        } else if rd.from < i && i <= rd.to {
            r = r.offset(0.0, -ROW).offset_motion(Spring::new(0.22, 0.0));
        } else if rd.to <= i && i < rd.from {
            r = r.offset(0.0, ROW).offset_motion(Spring::new(0.22, 0.0));
        } else {
            r = r.offset_motion(Spring::new(0.22, 0.0));
        }
    }
    r
}

/// A source's tile: what kind it is at a glance, or its languages.
fn source_badge(id: &str, installed: Option<&Installed>) -> El {
    let small = rune::TextStyle::new(10.0, 750.0);
    match id {
        "dictionary" => tile_icon(30.0, [229, 72, 77], &icons::BOOK),
        "thesaurus" => tile_text(30.0, [247, 107, 21], "≈", rune::TextStyle::new(18.0, 700.0)),
        "wikipedia" => tile_text(30.0, [110, 110, 115], "W", rune::TextStyle::new(16.0, 600.0).family(rune::Family::Named("serif"))),
        "wordnet" => tile_text(30.0, [48, 164, 108], "WN", small),
        _ => match id.strip_prefix("freedict-").and_then(|p| p.split_once('-')) {
            Some((from, to)) => tile(30.0, rgb(0, 144, 255), col().items_center().gap(0.0)
                .child(text(two(from)).font(small).color(Color::WHITE).single_line())
                .child(text(two(to)).font(rune::TextStyle::new(8.0, 600.0)).color(Color::WHITE.alpha(0.85)).single_line()).a11y_hidden()),
            None => match installed {
                Some(_) => tile_icon(30.0, [142, 78, 198], &icons::BOOK),
                None => tile_icon(30.0, [142, 142, 147], &icons::BOOK),
            },
        },
    }
}

/// A language's two-letter mark: `deu` → `DE`, `ces` → `CS`.
fn two(code: &str) -> String {
    let iso1 = match code {
        "afr" => "af", "ara" => "ar", "ast" => "ast", "bre" => "br", "bul" => "bg", "cat" => "ca", "ces" => "cs", "ckb" => "ckb",
        "cym" => "cy", "dan" => "da", "deu" => "de", "ell" => "el", "eng" => "en", "epo" => "eo", "fin" => "fi", "fra" => "fr",
        "gla" => "gd", "gle" => "ga", "hin" => "hi", "hrv" => "hr", "hun" => "hu", "ind" => "id", "isl" => "is", "ita" => "it",
        "jpn" => "ja", "kmr" => "kmr", "kur" => "ku", "lat" => "la", "lit" => "lt", "mkd" => "mk", "mlg" => "mg", "nld" => "nl",
        "nno" => "nn", "nob" => "nb", "nor" => "no", "oci" => "oc", "pol" => "pl", "por" => "pt", "ron" | "rom" => "ro",
        "rus" => "ru", "san" => "sa", "slk" => "sk", "slv" => "sl", "spa" => "es", "srp" => "sr", "swe" => "sv", "swh" => "sw",
        "tur" => "tr", "ukr" => "uk", "wol" => "wo", "zho" => "zh",
        other => other,
    };
    iso1.chars().take(3).collect::<String>().to_uppercase()
}

fn offer_row(s: &Settings, k: &Kit, o: &Offer) -> El {
    let installed = s.installed.iter().any(|d| d.id == o.id);
    let state = s.downloads.lock().unwrap().get(&o.id).cloned();
    let size = if o.size > 0 { format!(" · {}", library::human_size(o.size)) } else { String::new() };
    let words = col().grow(1.0).min_w(0.0).gap(2.0).child(title(k, &o.name))
        .child(text(format!("{}{size}", o.detail)).font(k.ty.text_small).color(k.c.text_weak).truncate());
    let badge = source_badge(&o.id, None);
    let id = o.id.clone();
    let control = match (installed, state) {
        (_, Some(Ok((got, total)))) => row().gap(8.0).center_y()
            .child(el().w(120.0).child(Progress::new((total > 0).then(|| got as f32 / total as f32)).label(format!("Downloading {}", o.name)).build(k)))
            .child(button(k, "Cancel", ButtonVariant::Ghost, move |s, _| { s.cancelled.lock().unwrap().insert(id.clone()); })),
        (_, Some(Err(e))) => {
            let offer = o.clone();
            row().gap(8.0).center_y()
                .child(text(e).font(k.ty.text_small).color(k.c.danger.fg).w(160.0).truncate())
                .child(button(k, "Retry", ButtonVariant::Neutral, move |s, _| download(s, offer.clone())))
        }
        (true, None) => row().gap(5.0).center_y().pr(6.0)
            .child(icon(&icons::CHECK, 14.0).color(k.c.success.fg).a11y_hidden())
            .child(text("Installed").font(k.ty.text_small).color(k.c.text_weak)),
        (false, None) => {
            let offer = o.clone();
            row().child(Button::new("Get").icon(&icons::DOWNLOAD).on_press(move |s: &mut Settings| download(s, offer.clone())).build(k))
        }
    };
    row().key(format!("offer-{}", o.id)).w_full().min_h(54.0).px(14.0).py(8.0).gap(12.0).center_y().child(badge).child(words).child(control)
}

fn download(s: &mut Settings, offer: Offer) {
    let id = offer.id.clone();
    s.downloads.lock().unwrap().insert(id.clone(), Ok((0, offer.size)));
    s.cancelled.lock().unwrap().remove(&id);
    let (progress, cancelled, waker) = (s.downloads.clone(), s.cancelled.clone(), s.waker.clone());
    s.spawn(move || {
        let last = Mutex::new(Instant::now());
        let r = library::install(&offer, &|got, total| {
            progress.lock().unwrap().insert(offer.id.clone(), Ok((got, total)));
            let mut l = last.lock().unwrap();
            if l.elapsed() > Duration::from_millis(100) { *l = Instant::now(); if let Some(w) = &waker { w(); } }
        }, &|| cancelled.lock().unwrap().contains(&offer.id));
        Msg::Installed(offer.id.clone(), r)
    });
}

fn add_local(s: &mut Settings, path: std::path::PathBuf) {
    s.note(format!("Adding {}…", path.display()));
    s.spawn(move || Msg::Added(library::add_local(&path)));
}

fn dictionaries(s: &Settings, k: &Kit, layer: &mut Vec<El>) -> El {
    let sources = s.config.sources();
    let mut list = col().key("sources").w_full().radius(10.0).fill(k.c.surface_raised_base).border(1.0, k.c.border_weak).clip_rounded(10.0);
    for (i, src) in sources.iter().enumerate() { list = list.child(source_row(s, k, i, src)); }
    let order = col().key("order").w_full().gap(8.0)
        .child(text("Sources").font(k.ty.small_medium).color(k.c.text_weak).pl(4.0))
        .child(list)
        .child(detail(k, "Tick the ones the popover shows, and drag them by their handle into the order it shows them in.").pl(4.0));

    let lang = s.config.language.clone();
    let lang_name = LANGUAGES.iter().find(|l| l.0 == lang).map_or(lang.clone(), |l| l.1.to_string());
    let mut names: Vec<String> = LANGUAGES.iter().map(|l| l.1.to_string()).collect();
    if !names.contains(&lang_name) { names.insert(0, lang_name.clone()); }
    let options = card(k, "lookup", Some("Looking up"), vec![
        setting(k, "language", "Language", Some("Of Wikipedia, and of Wiktionary for words the English dictionary doesn't have."),
            select(k, layer, s, "language", "Language", &names, &lang_name, 170.0, |s, v| {
                if let Some(code) = LANGUAGES.iter().find(|l| l.1 == v).map(|l| l.0) { s.set(|c| c.language = code.to_string()); }
            })),
        toggle(k, "offline", "Offline only", Some("Use only downloaded dictionaries: nothing you look up leaves this computer."), s.config.offline, |s| s.set(|c| c.offline = !c.offline)),
    ]);

    // The catalogue.
    let mut get = col().key("get").w_full().gap(8.0)
        .child(row().w_full().pl(4.0).center_y().gap(8.0)
            .child(text("Download dictionaries").font(k.ty.small_medium).color(k.c.text_weak).grow(1.0))
            .child(Button::new("Add from File…").icon(&icons::PLUS).variant(ButtonVariant::Ghost).on_press_tx(|_: &mut Settings, tx| {
                let o = FileDialogOptions { title: "Add a StarDict dictionary".into(), ..Default::default() }
                    .filter("StarDict dictionary (.ifo or an archive)", &["ifo", "gz", "tgz", "xz", "bz2", "zst", "tar"]);
                tx.open_file(o, |s: &mut Settings, r, _| match r { Ok(Some(p)) => add_local(s, p), Ok(None) => {}, Err(e) => s.error = Some(format!("No file dialog: {e}")) });
            }).build(k))
            .child(Button::new("Add Folder…").icon(&icons::FOLDER).variant(ButtonVariant::Ghost).on_press_tx(|_: &mut Settings, tx| {
                let o = FileDialogOptions { title: "Add a StarDict dictionary folder".into(), ..Default::default() };
                tx.open_folder(o, |s: &mut Settings, r, _| match r { Ok(Some(p)) => add_local(s, p), Ok(None) => {}, Err(e) => s.error = Some(format!("No folder dialog: {e}")) });
            }).build(k)));
    let search = TextInput::new(&s.search).label("Search dictionaries").placeholder("Filter by language, such as German or Ukrainian").width(None).leading_icon(&icons::SEARCH)
        .on_edit(|s: &mut Settings| &mut s.search).build(k);
    let mut rows = vec![row().w_full().px(10.0).py(10.0).child(search)];
    match &s.catalog {
        None => rows.push(row().px(14.0).py(14.0).gap(10.0).center_y().child(Spinner::new().build(k)).child(detail(k, "Loading the list of dictionaries…"))),
        Some(Err(e)) => rows.push(setting(k, "catalog-error", "Couldn't load the list of dictionaries", Some(e),
            button(k, "Try Again", ButtonVariant::Neutral, |s, _| s.load_catalog(true)))),
        Some(Ok(all)) => {
            let q = s.search.text.trim();
            let hits: Vec<&Offer> = all.iter().filter(|o| o.matches(q)).collect();
            const SHOWN: usize = 40;
            for o in hits.iter().take(SHOWN) { rows.push(offer_row(s, k, o)); }
            if hits.is_empty() {
                rows.push(row().px(14.0).py(14.0).child(detail(k, &format!("No dictionaries match “{q}”."))));
            } else if hits.len() > SHOWN {
                rows.push(row().px(14.0).py(12.0).child(detail(k, &format!("{} more: type a language to narrow the list.", hits.len() - SHOWN))));
            }
        }
    }
    get = get.child(card(k, "catalog", None, rows))
        .child(detail(k, "WordNet is Princeton University's English dictionary and thesaurus. Bilingual dictionaries come from the FreeDict project. You can also add any StarDict dictionary, such as GoldenDict's.").pl(4.0));
    page("dictionaries", vec![order, options, get])
}

// --- Appearance ---------------------------------------------------------------------------------

const ACCENTS: [(&str, &str); 7] = [("Blue", "#0a64d6"), ("Purple", "#8e44ad"), ("Pink", "#d63384"), ("Red", "#d0342c"), ("Orange", "#e8710a"), ("Green", "#2e9e44"), ("Graphite", "#6e6e73")];
const HIGHLIGHTS: [(&str, &str); 6] = [("Yellow", "#ffdb42"), ("Green", "#9be38a"), ("Blue", "#9cd1ff"), ("Pink", "#ffb3d1"), ("Orange", "#ffc078"), ("Purple", "#d4b8ff")];

/// A row of colour swatches; the first, unfilled, keeps the theme's colour.
fn swatches(k: &Kit, key: &str, current: Option<&str>, theme_color: Color, colors: &'static [(&'static str, &'static str)], set: fn(&mut Appearance, Option<String>)) -> El {
    let d = 20.0;
    let mut r = row().key(format!("{key}-swatches")).role("radiogroup").gap(8.0).center_y();
    let ring = |e: El, on: bool| if on { e.border(2.0, k.c.text_strong) } else { e.border(1.0, k.c.border_base) };
    let theme = el().key("theme").role("radio").label("Theme's").checked(current.is_none()).focusable(true).size(d, d).radius(d / 2.0)
        .fill(theme_color).help("The theme's colour")
        .on_press(move |s: &mut Settings| s.set(|c| set(&mut c.appearance, None)))
        .child(el().size(d - 8.0, 2.0).absolute(4.0, d / 2.0 - 1.0).fill(k.c.text_strong.alpha(0.6)).a11y_hidden());
    r = r.child(ring(theme, current.is_none()));
    for (name, hex) in colors {
        let on = current.is_some_and(|c| c.eq_ignore_ascii_case(hex));
        let col = style::color(hex).map(|c| c.color()).unwrap_or(Color::BLACK);
        let sw = el().key(*name).role("radio").label(*name).checked(on).focusable(true).size(d, d).radius(d / 2.0).fill(col).help(*name)
            .on_press(move |s: &mut Settings| s.set(|c| set(&mut c.appearance, Some(hex.to_string()))));
        r = r.child(ring(sw, on));
    }
    r
}

fn theme_tile(s: &Settings, k: &Kit, i: usize, dark: bool) -> El {
    let (id, name, _) = style::THEMES[i];
    let t = if dark { &s.presets[i].1 } else { &s.presets[i].0 };
    let on = s.config.appearance.theme == id;
    let ink = t.headword.color.color();
    let faint = t.sense.color.color().alpha(0.55);
    let mini = col().w(76.0).h(54.0).radius(t.popover.radius.min(8.0)).fill(t.popover.background.color()).border(1.0, k.c.border_weak)
        .padding(8.0).gap(5.0).clip_rounded(t.popover.radius.min(8.0))
        .child(row().gap(4.0).center_y()
            .child(el().w(26.0).h(6.0).radius(2.0).fill(ink))
            .child(el().w(16.0).h(8.0).radius(t.marker.radius.min(3.0)).fill(if t.marker.hidden { Color::TRANSPARENT } else { t.marker.background.color() })))
        .child(el().w(52.0).h(3.0).radius(2.0).fill(faint))
        .child(el().w(42.0).h(3.0).radius(2.0).fill(faint))
        .child(el().w(30.0).h(3.0).radius(2.0).fill(t.link.color.color().alpha(0.8)));
    let mini = if on { mini.border(2.0, accent(k.dark)) } else { mini };
    col().key(format!("theme-{id}")).role("radio").label(name).checked(on).focusable(true).gap(6.0).items_center()
        .on_press(move |s: &mut Settings| s.set(|c| c.appearance.theme = id.to_string()))
        .child(mini)
        .child(text(name).font(if on { k.ty.small_medium } else { k.ty.small }).color(if on { k.c.text_strong } else { k.c.text_base }))
}

/// The sample popover the settings preview: the built-in sections, in the settings' order.
fn sample(c: &Config) -> crate::ui::Shown {
    let mut v = crate::preview::shown([0.0, 0.0, 96.0, 24.0], Part::Found(crate::preview::definition()), Part::Found(crate::preview::wiki()));
    let all: Vec<Section> = std::mem::take(&mut v.sections);
    let order = c.active_sources();
    v.sections = order.iter().filter_map(|id| all.iter().find(|x| &x.id == id).cloned()).collect();
    v
}

fn preview(s: &Settings, k: &Kit, system_dark: bool) -> El {
    let dark = s.config.appearance.forced_dark().unwrap_or(system_dark);
    let t = if dark { &s.themes.1 } else { &s.themes.0 };
    let (page_bg, ink) = if dark { (rgb(30, 30, 32), rgb(220, 220, 224)) } else { (Color::WHITE, rgb(28, 28, 30)) };
    let page_font = rune::TextStyle::new(16.0, 400.0);
    let mut word = el().key("pv-word").px(t.marker.padding[0]).py(t.marker.padding[1]).radius(t.marker.radius)
        .child(text("serendipity").font(page_font).color(if t.marker.hidden { ink } else { t.marker.ink.color() }).single_line());
    if !t.marker.hidden {
        word = word.fill(t.marker.background.color());
        if let Some(sh) = t.marker.shadow { word = word.box_shadow(sh.rune()); }
    }
    let sentence = row().gap(5.0).center_y()
        .child(text("most are").font(page_font).color(ink).single_line())
        .child(word)
        .child(text("and luck.").font(page_font).color(ink).single_line());
    let w = t.popover.width;
    let arrow_x = (w - t.arrow.width) / 2.0;
    let pop = crate::ui::popover(t, &sample(&s.config), true, arrow_x, t.popover.max_height.min(560.0), None);
    let scheme = if dark { "Dark" } else { "Light" };
    col().key("preview").w((w + 56.0).clamp(400.0, 660.0)).h_full().fill(page_bg).border(1.0, k.c.border_weak).radius(12.0).clip_rounded(12.0)
        .pt(14.0).gap(14.0).items_center()
        .child(text(format!("Preview · {scheme}")).font(k.ty.text_small).color(ink.alpha(0.5)))
        .child(sentence.pt(6.0))
        .child(col().w_full().items_center().mt(-12.0).child(pop))
}

fn appearance(s: &Settings, k: &Kit, layer: &mut Vec<El>, system_dark: bool) -> El {
    let a = &s.config.appearance;
    let dark = a.forced_dark().unwrap_or(system_dark);
    let t = if dark { &s.themes.1 } else { &s.themes.0 };
    let mut tiles = row().key("themes").w_full().px(14.0).py(14.0).gap(12.0);
    for i in 0..style::THEMES.len() { tiles = tiles.child(theme_tile(s, k, i, dark)); }
    let schemes = ["System", "Light", "Dark"];
    let scheme = match a.scheme.as_str() { "light" => 1, "dark" => 2, _ => 0 };
    let theme_card = card(k, "theme", Some("Theme"), vec![
        tiles,
        setting(k, "scheme", "Appearance", None, SegmentedControl::new(&schemes, Some(scheme)).label("Appearance").width(Some(210.0))
            .on_pick(|s: &mut Settings, i, _| s.set(|c| c.appearance.scheme = ["system", "light", "dark"][i].to_string())).build(k)),
        setting(k, "accent", "Accent colour", Some("Links and section titles."),
            swatches(k, "accent", a.accent.as_deref(), s.presets[style::THEMES.iter().position(|x| x.0 == a.theme).unwrap_or(0)].0.link.color.color(), &ACCENTS, |a, v| a.accent = v)),
        setting(k, "highlight", "Highlight colour", Some("Behind the word you looked up."),
            swatches(k, "highlight", a.highlight.as_deref(), s.presets[style::THEMES.iter().position(|x| x.0 == a.theme).unwrap_or(0)].0.marker.background.color(), &HIGHLIGHTS, |a, v| a.highlight = v)),
        setting(k, "font", "Font", None, {
            let mut names = vec!["Theme's font".to_string(), "Sans serif".to_string(), "Serif".to_string(), "Monospace".to_string()];
            names.extend(s.fonts.iter().cloned());
            let cur = match a.font.as_deref() { None => "Theme's font".to_string(), Some("sans-serif") => "Sans serif".into(), Some("serif") => "Serif".into(), Some("monospace") => "Monospace".into(), Some(f) => f.to_string() };
            if !names.contains(&cur) { names.insert(4, cur.clone()); }
            select(k, layer, s, "font", "Font", &names, &cur, 200.0, |s, v| {
                let f = match v { "Theme's font" => None, "Sans serif" => Some("sans-serif"), "Serif" => Some("serif"), "Monospace" => Some("monospace"), f => Some(f) };
                s.set(|c| c.appearance.font = f.map(str::to_string));
            })
        }),
    ]);

    let scale = a.text_scale;
    let size_card = card(k, "size", Some("Size"), vec![
        slider(k, "text-size", "Text size", scale, (0.8, 1.5, 0.05), format!("{:.0}%", scale * 100.0), |s| s.config.appearance.text_scale, |s, v| s.config.appearance.text_scale = v),
        slider(k, "width", "Width", t.popover.width, (300.0, 640.0, 10.0), format!("{:.0} pt", t.popover.width), |s| s.themes.0.popover.width, |s, v| s.config.appearance.width = Some(v)),
        slider(k, "max-height", "Maximum height", t.popover.max_height, (240.0, 900.0, 20.0), format!("{:.0} pt", t.popover.max_height), |s| s.themes.0.popover.max_height, |s, v| s.config.appearance.max_height = Some(v)),
    ]);

    let shows = |on: bool| on;
    let content_card = card(k, "content", Some("Show"), vec![
        slider(k, "senses", "Definitions per part of speech", t.senses as f32, (1.0, 12.0, 1.0), format!("{}", t.senses), |s| s.themes.0.senses as f32, |s, v| s.config.appearance.senses = Some(v as usize)),
        slider(k, "wiki-lines", "Lines of Wikipedia", t.wiki_lines as f32, (1.0, 15.0, 1.0), format!("{}", t.wiki_lines), |s| s.themes.0.wiki_lines as f32, |s, v| s.config.appearance.wiki_lines = Some(v as usize)),
        toggle(k, "pronunciation", "Pronunciation", None, shows(!t.pronunciation.hidden), |s| { let on = !s.themes.0.pronunciation.hidden; s.set(|c| c.appearance.pronunciation = Some(!on)); }),
        toggle(k, "examples", "Examples", Some("A sentence under each definition, where there is one."), shows(!t.example.hidden), |s| { let on = !s.themes.0.example.hidden; s.set(|c| c.appearance.examples = Some(!on)); }),
        toggle(k, "pictures", "Pictures", Some("Wikipedia's picture beside its summary."), shows(!t.thumbnail.hidden), |s| { let on = !s.themes.0.thumbnail.hidden; s.set(|c| c.appearance.pictures = Some(!on)); }),
        toggle(k, "marker", "Highlight the word", None, shows(!t.marker.hidden), |s| { let on = !s.themes.0.marker.hidden; s.set(|c| c.appearance.marker = Some(!on)); }),
        toggle(k, "arrow", "Arrow to the word", None, shows(!t.arrow.hidden), |s| { let on = !s.themes.0.arrow.hidden; s.set(|c| c.appearance.arrow = Some(!on)); }),
        toggle(k, "animations", "Animations", Some("The popover grows in and the highlight pops."), t.popover.animate, |s| { let on = s.themes.0.popover.animate; s.set(|c| c.appearance.animations = Some(!on)); }),
        toggle(k, "footer", "Settings button", Some("“Settings…” at the end of the popover, which opens this window."), !t.footer.hidden, |s| { let on = !s.themes.0.footer.hidden; s.set(|c| c.appearance.footer = Some(!on)); }),
    ]);

    let path = style::user_path();
    let css_state = match (&s.user_css, s.css_warnings.len()) {
        (None, _) => format!("No {} yet. It cascades over everything here, for full control.", path.display()),
        (Some(_), 0) => format!("{} is in use.", path.display()),
        (Some(_), n) => format!("{} is in use, with {n} problem{}:", path.display(), if n == 1 { "" } else { "s" }),
    };
    let mut css_rows = vec![setting(k, "css", "Custom CSS", Some(&css_state), row().gap(8.0)
        .child(button(k, if s.user_css.is_some() { "Edit" } else { "Create" }, ButtonVariant::Neutral, |s, tx| {
            let path = style::user_path();
            if !path.exists() {
                let made = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(&path, style::DEFAULT_CSS));
                if let Err(e) = made { s.error = Some(format!("Couldn't create {}: {e}", path.display())); return; }
                s.user_css = Some(style::DEFAULT_CSS.to_string());
                s.restyle();
            }
            if tx.open_url(&format!("file://{}", path.display())).is_err() { s.error = Some(format!("Couldn't open {}", path.display())); }
        }))
        .child(button(k, "Reload", ButtonVariant::Ghost, |s, _| { s.user_css = std::fs::read_to_string(style::user_path()).ok(); s.restyle(); })))];
    if s.user_css.is_some() && !s.css_warnings.is_empty() {
        let mut list = col().px(14.0).pb(12.0).gap(2.0);
        for w in s.css_warnings.iter().take(8) { list = list.child(text(w).font(k.ty.mono).color(k.c.danger.fg).wrap()); }
        css_rows.push(list);
    }
    let css_card = card(k, "custom", Some("Advanced styling"), css_rows);
    let reset = row().pl(4.0).child(button(k, "Reset Appearance", ButtonVariant::Outline, |s, _| s.set(|c| c.appearance = Appearance::default())));

    let controls = page_max("appearance", 640.0, vec![theme_card, size_card, content_card, css_card, reset]);
    row().w_full().grow(1.0).min_h(0.0).gap(0.0)
        .child(col().grow(1.0).min_w(380.0).h_full().child(controls))
        .child(col().h_full().pr(28.0).pb(28.0).pt(4.0).child(preview(s, k, system_dark)))
}

// --- Advanced -----------------------------------------------------------------------------------

const BACKENDS: [(&str, &str); 4] = [("auto", "Automatic"), ("kwin", "KWin"), ("x11", "X11"), ("wayland", "Wayland (wlroots, Hyprland, GNOME)")];

fn advanced(s: &Settings, k: &Kit, layer: &mut Vec<El>) -> El {
    let c = &s.config;
    let names: Vec<String> = BACKENDS.iter().map(|b| b.1.to_string()).collect();
    let cur = BACKENDS.iter().find(|b| b.0 == c.backend).map_or(c.backend.clone(), |b| b.1.to_string());
    let backend = setting(k, "backend", "Screen backend", Some("How tlum finds the pointer and reads the screen. Changing it restarts tlum."),
        select(k, layer, s, "backend", "Backend", &names, &cur, 230.0, |s, v| {
            if let Some(id) = BACKENDS.iter().find(|b| b.1 == v).map(|b| b.0) { s.set(|c| c.backend = id.to_string()); }
        }));
    let chosen: Vec<&str> = c.ocr_language.split('+').filter(|l| !l.is_empty()).collect();
    let mut langs = row().w_full().gap(14.0).px(14.0).pb(12.0);
    let mut available = s.ocr_languages.clone();
    for l in &chosen { if !available.iter().any(|a| a == l) { available.push(l.to_string()); } }
    for l in available {
        let on = chosen.contains(&l.as_str());
        let name = library::language_name(&l).to_string();
        let lang = l.clone();
        langs = langs.child(Checkbox::new(on).label(if name == l { l.clone() } else { format!("{name} ({l})") }).on_toggle(move |s: &mut Settings| {
            let mut v: Vec<String> = s.config.ocr_language.split('+').filter(|x| !x.is_empty()).map(str::to_string).collect();
            if v.contains(&lang) { if v.len() > 1 { v.retain(|x| *x != lang); } } else { v.push(lang.clone()); }
            s.set(|c| c.ocr_language = v.join("+"));
        }).build(k));
    }
    let ocr = col().w_full()
        .child(setting(k, "ocr", "Text recognition languages", Some("The scripts Tesseract reads on screen. Add languages with your package manager (tesseract-langpack-…, tesseract-ocr-…)."), el()))
        .child(langs);
    let (cw, ch) = (c.capture_width as f32, c.capture_height as f32);
    let reading = card(k, "reading", Some("Reading the screen"), vec![
        backend, ocr,
        slider(k, "capture-w", "Area read: width", cw, (240.0, 1600.0, 20.0), format!("{cw:.0} pt"), |s| s.config.capture_width as f32, |s, v| s.config.capture_width = v as f64),
        slider(k, "capture-h", "Area read: height", ch, (32.0, 200.0, 4.0), format!("{ch:.0} pt"), |s| s.config.capture_height as f32, |s, v| s.config.capture_height = v as f64),
    ]);

    let mut diag = vec![setting(k, "doctor", "Check this desktop", Some("Tests the backend, screen capture, input devices and text recognition, as `tlum doctor` does."),
        button(k, if s.doctor.is_some() { "Check Again" } else { "Run Checks" }, ButtonVariant::Neutral, |s, _| {
            s.doctor = Some(String::new());
            s.spawn(|| {
                let out = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg("doctor").output());
                Msg::Doctor(match out { Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr), Err(e) => e.to_string() })
            });
        }))];
    match s.doctor.as_deref() {
        Some("") => diag.push(row().px(14.0).pb(12.0).gap(8.0).center_y().child(Spinner::new().build(k)).child(detail(k, "Checking…"))),
        Some(out) => diag.push(col().w_full().px(14.0).pb(12.0).child(text(out.trim_end()).font(k.ty.mono).color(k.c.text_base).wrap().selectable(true))),
        None => {}
    }
    let diagnostics = card(k, "diagnostics", Some("Diagnostics"), diag);

    let files = card(k, "files", Some("Files"), vec![
        setting(k, "config-file", "Settings file", Some(&config::config_path().display().to_string()),
            button(k, "Open", ButtonVariant::Ghost, |s, tx| { if tx.open_url(&format!("file://{}", config::config_path().display())).is_err() { s.error = Some("Couldn't open the settings file".into()); } })),
        setting(k, "dict-folder", "Dictionaries folder", Some(&library::dir().display().to_string()),
            button(k, "Open", ButtonVariant::Ghost, |s, tx| {
                let _ = std::fs::create_dir_all(library::dir());
                if tx.open_url(&format!("file://{}", library::dir().display())).is_err() { s.error = Some("Couldn't open the folder".into()); }
            })),
        setting(k, "reset", "Reset all settings", Some("Back to how tlum came. Dictionaries you downloaded stay."),
            if s.confirm_reset {
                row().gap(8.0)
                    .child(button(k, "Cancel", ButtonVariant::Ghost, |s, _| s.confirm_reset = false))
                    .child(button(k, "Reset", ButtonVariant::Danger, |s, _| { s.confirm_reset = false; s.config = Config::default(); s.shortcut = TextEdit::new(s.config.shortcut.clone()); s.save(); s.note("Settings reset."); }))
            } else {
                button(k, "Reset…", ButtonVariant::Outline, |s, _| s.confirm_reset = true)
            }),
    ]);
    page("advanced", vec![reading, diagnostics, files])
}

// --- running it ---------------------------------------------------------------------------------

pub fn spec() -> AppSpec<Settings> {
    AppSpec {
        scenes: &[
            ("dictionaries", |s| { s.page = Page::Dictionaries; s.catalog = Some(library::catalog(false)); }),
            ("german", |s| {
                s.page = Page::Dictionaries;
                s.catalog = Some(library::catalog(false));
                s.search = TextEdit::new("german");
                s.downloads.lock().unwrap().insert("freedict-deu-eng".into(), Ok((4_000_000, 10_000_000)));
            }),
            ("appearance", |s| s.page = Page::Appearance),
            ("advanced", |s| s.page = Page::Advanced),
            ("dark", |s| s.force_dark = Some(true)),
            ("light", |s| s.force_dark = Some(false)),
        ],
        min_size: Some((900.0, 560.0)),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        ..AppSpec::new("tlum Settings", (1120.0, 720.0), Settings::new, view)
    }
}

const BUS_NAME: &str = "dev.tlum.Settings";
const BUS_PATH: &str = "/dev/tlum/Settings";

/// The open window's D-Bus face: a second `tlum settings` (or the popover's "Settings…")
/// brings it forward instead of opening another.
struct Window { handle: rune::AppHandle<Settings> }

#[zbus::interface(name = "dev.tlum.Settings")]
impl Window {
    /// Shows the window, on `page` when it names one.
    fn show(&self, page: String) {
        if let Some(p) = page_named(&page) { self.handle.update(move |s, _| s.go(p)); }
        self.handle.open();
    }
}

/// `tlum settings [PAGE] [rune flags]`.
pub fn run(mut args: Vec<String>) {
    let page = args.first().filter(|a| !a.starts_with('-')).and_then(|a| page_named(a));
    if page.is_some() || args.first().is_some_and(|a| !a.starts_with('-')) { args.remove(0); }
    // Headless runs (`--snap`, `--tree`, …) stand alone.
    let headless = !args.is_empty();
    let bus = if headless { None } else { zbus::blocking::Connection::session().ok() };
    if let Some(c) = &bus {
        match c.request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into()) {
            Ok(zbus::fdo::RequestNameReply::PrimaryOwner) | Err(_) => {}
            Ok(_) => {
                let shown = zbus::blocking::Proxy::new(c, BUS_NAME, BUS_PATH, BUS_NAME)
                    .and_then(|p| p.call::<_, _, ()>("Show", &(page.map_or("", |p| PAGES.iter().find(|x| x.0 == p).unwrap().1),)));
                if shown.is_ok() { return; }
            }
        }
    }
    rune::set_app_id(BUS_NAME);
    let app = rune::App::new(spec()).args(args);
    let handle = app.handle();
    if let Some(p) = page { handle.update(move |s, _| s.go(p)); }
    if let Some(c) = &bus { let _ = c.object_server().at(BUS_PATH, Window { handle }); }
    app.run();
}
