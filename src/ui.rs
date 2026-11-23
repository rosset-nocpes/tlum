//! The tlum popover, drawn by Rune in a transparent full-screen overlay (a layer-shell
//! surface on Wayland) on the display under the pointer:
//!
//! - the word, repainted on a yellow marker that pops in where the word is;
//! - a popover with an arrow at the word: Dictionary (pronunciation, parts of speech,
//!   numbered senses with examples), Thesaurus, then Wikipedia with its picture;
//! - clicking outside, Escape, or dragging on from the press that opened it closes it.
//!
//! The overlay takes every click while it shows, as a popover's transient dismissal does.

use std::sync::{Arc, Mutex, OnceLock};

use rune::{Cx, El, Key, KeyEvent, OverlayHandle, Pixels, ScrollPos, ScrollSpec, Shape, Span, Spring, SpringPair, SurfaceFit, Transition, Tx, canvas, col, el, icon, rich, row, text};

use crate::dict::{Definition, Entry, Kind, Part, Section, Wiki};
use crate::style::{Text, Theme};

/// The popover keeps this far from the screen's edges.
const EDGE: f32 = 10.0;

/// What one display's overlay shows.
#[derive(Default)]
pub struct Popover {
    pub display: usize,
    pub shown: Option<Shown>,
    pub hover: bool,
    pub scroll: Arc<Mutex<Option<ScrollPos>>>,
    pub images: Option<rune::ImageCache>,
    /// Headless previews only: the theme to draw (a live overlay follows the system).
    pub force_dark: Option<bool>,
    /// The overlay surface's size, once laid out: a layer surface keeps out of panels.
    pub size: Option<[f32; 2]>,
    /// X11 without a compositor can't show through the overlay, so it shows the screen as it
    /// was instead, until the overlay goes.
    pub backdrop: Option<Pixels>,
}

pub struct Shown {
    pub generation: u64,
    pub query: String,
    /// The word on screen, in this display's points.
    pub word: [f32; 4],
    /// The word repainted for the light and the dark scheme (the style may differ).
    pub marker: Option<(Pixels, Pixels)>,
    /// The display's size in points.
    pub screen: [f32; 2],
    /// Where the overlay surface starts on the display, once measured (`calibrate`).
    pub offset: Option<[f32; 2]>,
    /// The display minus panels, in its points, when the desktop says: the popover stays
    /// inside it, though the surface covers the panels too.
    pub area: Option<[f32; 4]>,
    /// The pointer when triggered, in the display's points: the overlay's first pointer event
    /// tells where its surface starts when the desktop doesn't.
    pub pointer: [f32; 2],
    pub shown_at: std::time::Instant,
    /// One per source, in the settings' order, each filling in as it arrives.
    pub sections: Vec<Section>,
    pub thumbnail: Option<rune::Image>,
}

/// Where each display's overlay surface starts, as last measured.
static OFFSETS: Mutex<Vec<Option<[f32; 2]>>> = Mutex::new(Vec::new());

fn cached_offset(display: usize) -> Option<[f32; 2]> { OFFSETS.lock().unwrap().get(display).copied().flatten() }

/// The surface's offset into the display. Rune's full-screen layer surface claims exclusive
/// zone -1, so it covers panels and starts at the display's corner. A surface that is kept out
/// of panels' zones instead is smaller than the display by their size; which side they're on
/// the pointer shows (`calibrate`), and until then a top bar is the likelier guess.
fn offset(s: &Popover, v: &Shown) -> [f32; 2] {
    if let Some(o) = v.offset.or_else(|| cached_offset(s.display)) { return o; }
    match s.size {
        Some(size) => [0.0, (v.screen[1] - size[1]).max(0.0)],
        None => [0.0, 0.0],
    }
}

/// The first pointer event after a lookup opens says where the surface is: the pointer
/// hasn't moved since the trigger, so its display position minus its surface position is the
/// surface's offset, which can only be 0 or the panels' total on each axis.
fn calibrate(s: &mut Popover, at: [f32; 2], tx: &mut Tx) {
    let (Some(v), Some(size)) = (s.shown.as_mut(), s.size) else { return tx.live_only() };
    if v.offset.is_some() || v.shown_at.elapsed() > std::time::Duration::from_millis(1500) { return tx.live_only(); }
    let snap = |measured: f32, room: f32| [0.0, room.max(0.0)].into_iter().min_by(|a, b| (a - measured).abs().total_cmp(&(b - measured).abs())).unwrap();
    let measured = [v.pointer[0] - at[0], v.pointer[1] - at[1]];
    let o = [snap(measured[0], v.screen[0] - size[0]), snap(measured[1], v.screen[1] - size[1])];
    if (o[0] - measured[0]).abs() > 40.0 || (o[1] - measured[1]).abs() > 40.0 { return tx.live_only(); }
    tx.instant();
    v.offset = Some(o);
    let mut all = OFFSETS.lock().unwrap();
    if all.len() <= s.display { all.resize(s.display + 1, None); }
    all[s.display] = Some(o);
}

/// Every display's overlay, in display order.
pub static OVERLAYS: OnceLock<Vec<OverlayHandle<Popover>>> = OnceLock::new();
/// The generation showing, and on which overlay.
pub static VISIBLE: Mutex<Option<(u64, usize)>> = Mutex::new(None);

pub fn hide(display: usize) {
    if let Some(o) = OVERLAYS.get().and_then(|v| v.get(display)) { o.hide(); }
}

/// How long the closing fade takes before the surface goes.
const CLOSE: std::time::Duration = std::time::Duration::from_millis(260);

fn forget(s: &mut Popover) -> bool {
    let had = s.shown.take().is_some();
    if had {
        let mut v = VISIBLE.lock().unwrap();
        if v.is_some_and(|(_, d)| d == s.display) { *v = None; }
    }
    s.hover = false;
    *s.scroll.lock().unwrap() = None;
    had
}

/// Closes the popover: it and the marker fade out where they are, clicks pass through at
/// once, and the surface goes when the fade is done (unless a new lookup reused it).
pub fn dismiss(s: &mut Popover) {
    if !forget(s) { return hide(s.display); }
    let Some(o) = OVERLAYS.get().and_then(|v| v.get(s.display)).cloned() else { return };
    o.set_input(rune::OverlayInput::None);
    std::thread::spawn(move || {
        std::thread::sleep(CLOSE);
        o.update(|s, _| if s.shown.is_none() { s.backdrop = None; hide(s.display) });
    });
}

/// Closes at once, without the fade: before capturing the screen under it.
pub fn dismiss_now(s: &mut Popover, tx: &mut Tx) {
    tx.instant();
    forget(s);
    s.backdrop = None;
    hide(s.display);
}

/// A click or scroll somewhere on screen, read from evdev. Over the popover it's the popover's
/// own; elsewhere it goes to the app under the pointer, and the popover closes, as on macOS.
/// True when it closed.
pub fn outside(s: &mut Popover) -> bool {
    if s.shown.is_none() || s.hover { return false; }
    dismiss(s);
    true
}

/// How long after a release the compositor has handed the pointer to the surface under it.
const HANDBACK: std::time::Duration = std::time::Duration::from_millis(200);

/// The press that opened the popover went on to drag before letting go. Let go on the popover,
/// the person went to it, and it stays; elsewhere it was a drag that began with a pause (a
/// selection, a move), not a lookup, and the popover closes. While the button was down the app
/// under the press held the pointer; once it's up the compositor gives it to the surface under
/// it, and `hover` says whether that's the popover.
pub fn released_after_drag() {
    let Some((generation, display)) = *VISIBLE.lock().unwrap() else { return };
    let Some(o) = OVERLAYS.get().and_then(|v| v.get(display)).cloned() else { return };
    std::thread::spawn(move || {
        std::thread::sleep(HANDBACK);
        o.update_if(move |s, _| s.shown.as_ref().is_some_and(|v| v.generation == generation) && outside(s));
    });
}

/// Losing the keyboard closes the popover: a tap on another window (which evdev doesn't see as
/// a click) or switching apps.
pub fn on_focus(s: &mut Popover, focused: bool, _: &mut Tx) {
    if !focused && s.shown.is_some() { dismiss(s); }
}

/// Scrolls the popover by `dy` points while the pointer is over it; elsewhere a scroll closes
/// it (`outside`). True when the view must run again.
pub fn scroll_by(s: &mut Popover, dy: f32, tx: &mut Tx) -> bool {
    if outside(s) { return true; }
    if s.shown.is_none() { return false; }
    let Some(pos) = *s.scroll.lock().unwrap() else { return false };
    let y = (pos.offset[1] + dy).clamp(0.0, pos.max[1].max(0.0));
    tx.scroll_to_instant("lu-body", 0.0, y);
    false
}

pub fn on_key(s: &mut Popover, e: &KeyEvent, tx: &mut Tx) -> bool {
    match &e.key {
        Key::Escape => { dismiss(s); true }
        Key::Down | Key::Up => {
            let was = s.hover;
            s.hover = true;
            scroll_by(s, if e.key == Key::Down { 40.0 } else { -40.0 }, tx);
            s.hover = was;
            true
        }
        Key::Enter => {
            if let Some(url) = s.shown.as_ref().and_then(|v| v.sections.iter().find_map(|s| s.url().map(str::to_string))) {
                let _ = tx.open_url(&url);
                dismiss(s);
            }
            true
        }
        Key::Command('c') => {
            if let Some(v) = &s.shown { tx.copy(&v.query); }
            true
        }
        _ => false,
    }
}

/// A part's text, styled.
fn txt(t: &Text, s: impl Into<String>, family: Option<&'static str>) -> El {
    let mut e = text(s).font(t.style(family)).color(t.color.color());
    if let Some(c) = t.case() { e = e.text_case(c); }
    e
}

/// A span inside a rich text, styled as `t` (colour, weight, slant, and size if it differs).
fn span(t: &Text, start: usize, end: usize, base: &Text) -> Span {
    let mut s = Span::new(start, end).color(t.color.color()).weight(t.weight);
    if t.italic { s = s.italic(); }
    if t.size != base.size { s = s.size(t.size); }
    s
}

/// Where the popover goes for a word at `word` on a `screen`-sized display: the panel's left
/// edge, whether it hangs below the word, the arrow's x within the panel, and the room left.
pub fn place(word: [f32; 4], screen: [f32; 2], t: &Theme) -> (f32, bool, f32, f32) {
    let (w, arrow_w, arrow_h) = (t.popover.width, t.arrow.width, if t.arrow.hidden { 0.0 } else { t.arrow.height });
    let cx = word[0] + word[2] / 2.0;
    let left = (cx - w / 2.0).clamp(EDGE, (screen[0] - w - EDGE).max(EDGE));
    let below_room = screen[1] - (word[1] + word[3] + 2.0 + arrow_h) - EDGE;
    let above_room = word[1] - 2.0 - arrow_h - EDGE;
    let below = below_room >= 260.0 || below_room >= above_room;
    let inset = (t.popover.radius + 2.0).min(w / 2.0 - arrow_w / 2.0).max(0.0);
    let arrow = (cx - left - arrow_w / 2.0).clamp(inset, (w - arrow_w - inset).max(inset));
    (left, below, arrow, if below { below_room } else { above_room }.min(t.popover.max_height).max(80.0))
}

/// What a section's link says, from where it goes.
fn link_name(url: &str) -> &'static str {
    if url.contains("wiktionary.org") { "Wiktionary" } else if url.contains("wikipedia.org") { "Wikipedia" } else { "Open" }
}

/// A section's title row, with its link. `live`: the link opens (the settings' preview draws
/// the popover without handlers).
fn section(t: &Theme, label: &str, url: Option<&str>, live: bool) -> El {
    let f = t.popover.family;
    let mut header = row().w_full().justify_between().center_y();
    if !t.section_title.hidden { header = header.child(txt(&t.section_title, label, f)); } else { header = header.child(el()); }
    if let Some(url) = url.filter(|_| !t.link.hidden) {
        let mut link = txt(&t.link, format!("{} ↗", link_name(url)), f);
        if live {
            let url = url.to_string();
            link = link.cursor(rune::Cursor::Pointer).on_press_tx(move |s: &mut Popover, tx| { let _ = tx.open_url(&url); dismiss(s); });
        }
        header = header.child(link);
    }
    header
}

fn sense_row(t: &Theme, n: usize, sense: &crate::dict::Sense) -> El {
    let f = t.popover.family;
    let number = if t.sense_number.hidden { String::new() } else { format!("{n}  ") };
    let body = format!("{number}{}", sense.text);
    let spans = if number.is_empty() { vec![] } else { vec![span(&t.sense_number, 0, number.len(), &t.sense)] };
    let mut c = col().w_full().gap(2.0).child(rich(body, spans).font(t.sense.style(f)).color(t.sense.color.color()).wrap());
    if let Some(ex) = sense.example.as_ref().filter(|_| !t.example.hidden) {
        c = c.child(txt(&t.example, format!("“{}”", ex.trim_matches(['"', '“', '”'])), f).wrap().pl(18.0));
    }
    c
}

fn dictionary(t: &Theme, title: &str, d: &Definition, live: bool) -> El {
    let f = t.popover.family;
    let mut c = col().w_full().gap(8.0).child(section(t, title, (!d.url.is_empty()).then_some(d.url.as_str()), live));
    let mut head = row().gap(10.0).center_y();
    if !t.headword.hidden { head = head.child(txt(&t.headword, &d.headword, f)); }
    if let Some(ph) = d.phonetic.as_ref().filter(|_| !t.pronunciation.hidden) {
        head = head.child(txt(&t.pronunciation, format!("| {} |", ph.trim_matches('/').trim_matches(['[', ']'])), f));
    }
    c = c.child(head);
    if let Some(note) = d.note.as_ref().filter(|_| !t.note.hidden) { c = c.child(txt(&t.note, note, f).wrap()); }
    for m in d.meanings.iter().take(5) {
        let mut senses = col().w_full().gap(6.0);
        if !t.part_of_speech.hidden && !m.part_of_speech.is_empty() { senses = senses.child(txt(&t.part_of_speech, &m.part_of_speech, f)); }
        if !t.sense.hidden {
            for (i, s) in m.senses.iter().take(t.senses.max(1)).enumerate() { senses = senses.child(sense_row(t, i + 1, s)); }
        }
        c = c.child(senses);
    }
    c
}

fn thesaurus(t: &Theme, title: &str, d: &Definition) -> Option<El> {
    if t.synonyms.hidden { return None; }
    let label = &t.synonyms_label;
    let rows: Vec<El> = d.meanings.iter().filter(|m| !m.synonyms.is_empty() || !m.antonyms.is_empty()).take(4).map(|m| {
        let lead = if label.hidden || m.part_of_speech.is_empty() { String::new() } else { format!("{}  ", m.part_of_speech) };
        let mut body = format!("{lead}{}", m.synonyms.iter().take(10).cloned().collect::<Vec<_>>().join(", "));
        let mut spans = if lead.is_empty() { vec![] } else { vec![span(label, 0, lead.len(), &t.synonyms)] };
        if !m.antonyms.is_empty() {
            let opp = if m.synonyms.is_empty() { "opposites: " } else { "; opposites: " };
            spans.push(span(label, body.len(), body.len() + opp.len(), &t.synonyms));
            body.push_str(opp);
            body.push_str(&m.antonyms.iter().take(6).cloned().collect::<Vec<_>>().join(", "));
        }
        rich(body, spans).font(t.synonyms.style(t.popover.family)).color(t.synonyms.color.color()).wrap()
    }).collect();
    if rows.is_empty() { return None; }
    let mut c = col().w_full().gap(6.0).child(section(t, title, None, false));
    for r in rows { c = c.child(r); }
    Some(c)
}

fn wikipedia(t: &Theme, title: &str, w: &Wiki, thumb: Option<&rune::Image>, live: bool) -> El {
    let f = t.popover.family;
    let mut words = col().grow(1.0).min_w(0.0).gap(4.0);
    if !t.wiki_title.hidden { words = words.child(txt(&t.wiki_title, &w.title, f).wrap()); }
    if let Some(d) = w.description.as_ref().filter(|_| !t.wiki_description.hidden) { words = words.child(txt(&t.wiki_description, d, f).wrap()); }
    if !t.wiki_extract.hidden {
        let lines = if w.disambiguation { t.wiki_lines.min(3) } else { t.wiki_lines };
        words = words.child(txt(&t.wiki_extract, &w.extract, f).wrap().max_lines(lines.max(1)));
    }
    let mut body = row().w_full().gap(12.0).child(words);
    if let Some(img) = thumb.filter(|i| i.is_ready() && !t.thumbnail.hidden) {
        body = body.child(el().size(t.thumbnail.width, t.thumbnail.height).radius(t.thumbnail.radius)
            .pixels_fit(img.pixels(), SurfaceFit::Fill).transition(Transition::opacity()));
    }
    col().w_full().gap(8.0).child(section(t, title, Some(&w.url), live)).child(body)
}

fn status(t: &Theme, s: &str) -> El { txt(&t.status, s, t.popover.family).wrap() }

/// Whether the style hides a section (`.dictionary`, `.thesaurus`, `.wikipedia { display: none }`).
fn hidden(t: &Theme, s: &Section) -> bool {
    match s.kind { Kind::Dictionary => t.hide_dictionary, Kind::Thesaurus => t.hide_thesaurus, Kind::Wikipedia => t.hide_wikipedia }
}

/// Opens tlum's settings window (or brings it forward, when it's open).
pub fn open_settings() {
    let Ok(exe) = std::env::current_exe() else { return };
    let null = std::process::Stdio::null;
    match std::process::Command::new(exe).arg("settings").stdin(null()).stdout(null()).spawn() {
        // Reap it when it closes.
        Ok(mut child) => { std::thread::spawn(move || child.wait()); }
        Err(e) => eprintln!("tlum: couldn't open the settings: {e}"),
    }
}

/// "Settings…" at the end of the popover, as macOS's Look Up has.
fn footer(t: &Theme, live: bool) -> El {
    let f = &t.footer;
    let mut b = row().key("lu-settings").gap(5.0).center_y().radius(5.0).px(4.0).py(2.0).ml(-4.0).label("Settings").role("button")
        .child(icon(&crate::icons::GEAR, (f.size + 1.0).round()).color(f.color.color()).a11y_hidden())
        .child(txt(f, "Settings…", t.popover.family).a11y_hidden());
    if live {
        b = b.cursor(rune::Cursor::Pointer).hover(f.color.color().alpha(0.12))
            .on_press(|s: &mut Popover| { dismiss(s); open_settings(); });
    }
    row().w_full().child(b)
}

/// The sections that have something, in order. Until one does: the word and "Looking up…";
/// when none has anything: why.
fn content(t: &Theme, v: &Shown, live: bool) -> El {
    let p = &t.popover;
    let mut c = col().w_full().gap(p.gap).pt(p.padding[0]).pr(p.padding[1]).pb(p.padding[2]).pl(p.padding[3]);
    let rule = || if t.rule.hidden { None } else { Some(el().w_full().h(t.rule.height).fill(t.rule.color.color())) };
    let add = |c: El, part: El, sep: bool| -> El { let c = if sep && let Some(r) = rule() { c.child(r) } else { c }; c.child(part) };
    let sections: Vec<&Section> = v.sections.iter().filter(|s| !hidden(t, s)).collect();
    let mut any = false;
    for s in &sections {
        let block = match (s.part.found(), s.kind) {
            (Some(Entry::Definition(d)), Kind::Thesaurus) => thesaurus(t, &s.title, d),
            (Some(Entry::Definition(d)), _) => Some(dictionary(t, &s.title, d, live)),
            (Some(Entry::Wiki(w)), _) => Some(wikipedia(t, &s.title, w, v.thumbnail.as_ref(), live)),
            (None, _) => None,
        };
        if let Some(b) = block { c = add(c, b, any); any = true; }
    }
    let with_footer = |c: El| if t.footer.hidden { c } else { add(c, footer(t, live), true) };
    if any { return with_footer(c); }
    let word = || txt(&t.headword, &v.query, p.family);
    if sections.is_empty() {
        c = add(c, col().w_full().gap(6.0).child(word()).child(status(t, "No dictionaries are turned on. Choose some in tlum's settings.")), false);
    } else if !sections.iter().all(|s| s.part.settled()) {
        c = add(c, col().w_full().gap(8.0).child(word()).child(status(t, "Looking up…")), false);
    } else {
        let mut block = col().w_full().gap(6.0).child(word());
        let failed: Vec<(&str, &str)> = sections.iter().filter_map(|s| if let Part::Failed(e) = &s.part { Some((s.title.as_str(), e.as_str())) } else { None }).collect();
        if failed.is_empty() { block = block.child(status(t, "No entries found")); }
        for (title, e) in failed { block = block.child(status(t, &format!("Couldn't reach {title} ({e})"))); }
        c = add(c, block, false);
    }
    with_footer(c)
}

/// A popover arrow pointing up (or down), its base overlapping the panel's border so the
/// border breaks where the arrow joins.
fn arrow(t: &Theme, up: bool) -> El {
    let (w, h) = (t.arrow.width, t.arrow.height + t.popover.border_width.max(1.0));
    let (tip, base) = if up { (0.5, h) } else { (h - 0.5, 0.0) };
    let mut shape = Shape::new().fill(vec![[0.0, base], [w / 2.0, tip], [w, base]], t.popover.background.color());
    if t.popover.border_width > 0.0 {
        shape = shape.stroke(vec![[0.0, base], [w / 2.0, tip], [w, base]], t.popover.border_width, t.popover.border.color());
    }
    canvas(shape).size(w, h)
}

/// The popover, arrow included, hanging `below` the word or above it, its arrow `arrow_x` in.
/// `live` (the overlay's scroll position, which it reports) wires its handlers; without it,
/// it's a picture, as the settings' preview draws it.
pub fn popover(t: &Theme, v: &Shown, below: bool, arrow_x: f32, room: f32, live: Option<Arc<Mutex<Option<ScrollPos>>>>) -> El {
    let (w, arrow_h) = (t.popover.width, if t.arrow.hidden { 0.0 } else { t.arrow.height });
    let mut body = col().key("lu-body").w_full().max_h(room).scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(12.0)
        .child(content(t, v, live.is_some()));
    if let Some(scroll) = live.clone() { body = body.on_scroll_offset(move |pos| *scroll.lock().unwrap() = Some(pos)); }
    let mut panel = col().key("lu-panel").w(w).radius(t.popover.radius).fill(t.popover.background.color()).clip_rounded(t.popover.radius).child(body);
    if live.is_some() { panel = panel.on_press(|_: &mut Popover| {}); }
    if t.popover.border_width > 0.0 { panel = panel.border(t.popover.border_width, t.popover.border.color()); }
    if let Some(sh) = t.popover.shadow { panel = panel.box_shadow(sh.rune()); }
    // Grows from the arrow's tip; closing shrinks back a little as it fades.
    let origin_x = (arrow_x + t.arrow.width / 2.0) / w;
    let mut pop_el = col().key(format!("lu-pop-{}", v.generation)).w(w).origin(origin_x, if below { 0.0 } else { 1.0 })
        .transition(presence(t.popover.animate, t.popover.scale_from, t.popover.duration, 0.18));
    // The popover is the overlay's one hit region, and where `hover` holds: a click or wheel on
    // it is its own and doesn't close it.
    if live.is_some() {
        pop_el = pop_el.hit().on_hover_change(|s: &mut Popover, on, _| s.hover = on).on_wheel(|_: &mut Popover, _, _| true);
    }
    pop_el = if below { pop_el.pt(arrow_h) } else { pop_el.pb(arrow_h) };
    pop_el = pop_el.child(panel);
    if !t.arrow.hidden {
        pop_el = pop_el.child(arrow(t, below).pin(Some(arrow_x), if below { Some(0.0) } else { None }, None, if below { None } else { Some(0.0) }));
    }
    pop_el
}

/// An entrance that scales from `from` and fades, on a spring of `duration` seconds; its exit
/// fades, shrinking back a little. `animate: false` (`transition: none`) shows and hides at once.
fn presence(animate: bool, from: f32, duration: f32, bounce: f32) -> Transition {
    if !animate { return Transition::none(); }
    let enter = if (from - 1.0).abs() > 0.001 { Transition::scale(from).combined(Transition::opacity()) } else { Transition::opacity() };
    let exit = if from < 1.0 { Transition::scale(1.0 - (1.0 - from) * 0.4).combined(Transition::opacity()) } else { Transition::opacity() };
    Transition::asymmetric(enter, exit).springs(SpringPair::new(Spring::new(duration.max(0.05), bounce), Spring::new((duration * 0.6).max(0.05), 0.0)))
}

pub fn view(s: &Popover, cx: &mut Cx<Popover>) -> El {
    // The overlay takes the pointer only over the popover (`OverlayInput::Regions`): elsewhere
    // the desktop under it hovers and clicks as usual, and a click or scroll there closes the
    // popover through evdev (`outside`). Over a backdrop (no compositor: the screen as it was)
    // nothing under it is live, so the background takes every click and wheel and closes it.
    let root = el().key("lu-root").w_full().h_full().on_press(dismiss)
        .on_wheel(|s: &mut Popover, _, _| { if s.shown.is_some() { dismiss(s); } true })
        .on_layout(|s: &mut Popover, r| if s.size != Some([r[2], r[3]]) { s.size = Some([r[2], r[3]]) })
        .on_pointer_move_local(|s: &mut Popover, p, tx| calibrate(s, [p.window_x, p.window_y], tx));
    let root = match &s.backdrop {
        Some(b) => root.hit().child(el().key("lu-backdrop").w_full().h_full().pixels(b).pin(Some(0.0), Some(0.0), None, None)),
        None => root,
    };
    let Some(v) = &s.shown else { return root };
    let dark = s.force_dark.or_else(|| crate::config::current().appearance.forced_dark()).unwrap_or_else(|| cx.system_dark());
    let t = crate::style::theme(dark);
    let off = offset(s, v);
    let surface = s.size.unwrap_or([v.screen[0] - off[0], v.screen[1] - off[1]]);
    let word = [v.word[0] - off[0], v.word[1] - off[1], v.word[2], v.word[3]];
    // The popover keeps off panels: it fits the desktop's work area where it says, on the surface.
    let fit = v.area.map_or([0.0, 0.0, surface[0], surface[1]], |a| {
        let (x0, y0) = ((a[0] - off[0]).max(0.0), (a[1] - off[1]).max(0.0));
        let (x1, y1) = ((a[0] + a[2] - off[0]).min(surface[0]), (a[1] + a[3] - off[1]).min(surface[1]));
        [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)]
    });
    let (left, below, arrow_x, room) = place([word[0] - fit[0], word[1] - fit[1], word[2], word[3]], [fit[2], fit[3]], &t);
    let left = left + fit[0];
    let mut pop_el = popover(&t, v, below, arrow_x, room, Some(s.scroll.clone()));
    pop_el = if below {
        pop_el.pin(Some(left), Some(word[1] + word[3] + 2.0), None, None)
    } else {
        pop_el.pin(Some(left), None, None, Some(surface[1] - (word[1] - 2.0)))
    };

    let mut root = root;
    if let Some((light, dark_px)) = v.marker.as_ref().filter(|_| !t.marker.hidden) {
        let mut marker = el().key(format!("lu-marker-{}", v.generation)).size(word[2], word[3]).radius(t.marker.radius)
            .pixels(if dark { dark_px } else { light })
            // Pops in with a bounce, as macOS's marker does; fades out in place.
            .transition(presence(t.marker.animate, t.marker.scale_from, t.marker.duration, 0.45))
            .pin(Some(word[0]), Some(word[1]), None, None);
        if let Some(sh) = t.marker.shadow { marker = marker.box_shadow(sh.rune()); }
        root = root.child(marker);
    }
    root.child(pop_el)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    fn settle(rt: &mut Runtime<Popover>) { for _ in 0..90 { rt.frame(1.0 / 60.0); } }
    fn offset(rt: &Runtime<Popover>) -> f32 { rt.state.scroll.lock().unwrap().map_or(-1.0, |p| p.offset[1]) }

    #[test]
    fn evdev_scrolls_move_the_popover_under_the_pointer_and_close_it_elsewhere() {
        let mut rt = Runtime::new(crate::preview::init(), view, (1000.0, 680.0), Env { system_dark: true, scale: 2.0 });
        rt.frame(0.0);
        settle(&mut rt);
        let max = rt.state.scroll.lock().unwrap().expect("the body reports its scroll").max[1];
        assert!(max > 20.0, "the sample entry overflows the popover ({max})");
        let step = max / 2.0;
        rt.update(move |s, tx| { s.hover = true; scroll_by(s, step, tx); });
        settle(&mut rt);
        assert!((offset(&rt) - step).abs() < 0.5, "scrolled to {} of {max}", offset(&rt));
        rt.update(|s, tx| { scroll_by(s, 10_000.0, tx); });
        settle(&mut rt);
        assert!((offset(&rt) - max).abs() < 0.5, "clamped at the end");
        rt.update(move |s, tx| { s.hover = false; scroll_by(s, step, tx); });
        settle(&mut rt);
        assert!(rt.state.shown.is_none(), "a scroll outside the popover closes it");
    }

    #[test]
    fn a_wheel_scrolls_the_popover_over_it_and_closes_it_elsewhere() {
        let mut rt = Runtime::new(crate::preview::init(), view, (1000.0, 680.0), Env { system_dark: true, scale: 2.0 });
        rt.frame(0.0);
        settle(&mut rt);
        let (x, y) = rt.find("lu-body").unwrap();
        rt.wheel_event(rune::WheelEvent::new(0.0, -120.0).at(x, y).precise(false));
        settle(&mut rt);
        assert!(rt.state.shown.is_some() && offset(&rt) > 0.0, "the body scrolled ({})", offset(&rt));
        // A new gesture, after the last one has gone idle.
        for _ in 0..30 { rt.frame(1.0 / 60.0); }
        rt.wheel_event(rune::WheelEvent::new(0.0, -120.0).at(950.0, 600.0).precise(false));
        settle(&mut rt);
        assert!(rt.state.shown.is_none(), "a wheel outside the popover closes it");
    }

    /// Where the marker and popover draw over the first frames of a show, as the surface
    /// grows from `from` to full size.
    fn track(from: (f32, f32)) -> Vec<([f32; 4], [f32; 4], f32)> {
        let mut state = crate::preview::init();
        let shown = state.shown.take();
        let mut rt = Runtime::new(state, view, from, Env { system_dark: true, scale: 1.0 });
        rt.frame(0.0);
        settle(&mut rt);
        rt.update(move |s, _| s.shown = shown);
        rt.set_viewport((1000.0, 680.0), 1.0);
        let mut out = Vec::new();
        for _ in 0..30 {
            rt.frame(1.0 / 60.0);
            let m = rt.presented("lu-marker-1").map_or([0.0; 4], |p| p.0);
            let (p, o) = rt.presented("lu-pop-1").unwrap_or(([0.0; 4], 0.0));
            out.push((m, p, o));
        }
        out
    }

    #[test]
    fn showing_never_slides_the_marker_or_the_popover() {
        for from in [(400.0, 300.0), (1000.0, 680.0)] {
            let frames = track(from);
            let (m0, p0, _) = frames[0];
            for (i, (m, p, _)) in frames.iter().enumerate() {
                eprintln!("{from:?} frame {i}: marker {m:?} pop {p:?}");
                // Scale may change the drawn size about the anchor, but the anchor stays put.
                assert!((m[0] + m[2] / 2.0 - (m0[0] + m0[2] / 2.0)).abs() < 1.0 && (m[1] + m[3] / 2.0 - (m0[1] + m0[3] / 2.0)).abs() < 1.0, "marker moved");
                assert!((p[1] - p0[1]).abs() < 1.0, "popover moved vertically");
            }
        }
    }

    /// The surface covers the panels (exclusive zone -1), so the marker sits on the word itself;
    /// the work area only keeps the popover clear of the panels.
    #[test]
    fn a_top_panel_neither_shifts_the_marker_nor_takes_the_popover() {
        let panel = 32.0;
        let mut state = crate::preview::init();
        let v = state.shown.as_mut().unwrap();
        let word = v.word;
        v.offset = None;
        v.area = Some([0.0, panel, 1000.0, 680.0 - panel]);
        // A word just under the panel: room above it is all panel, so the popover hangs below.
        v.word = [word[0], panel + 4.0, word[2], word[3]];
        let at = v.word;
        let mut rt = Runtime::new(state, view, (1000.0, 680.0), Env { system_dark: true, scale: 1.0 });
        rt.frame(0.0);
        settle(&mut rt);
        let m = rt.presented("lu-marker-1").expect("marker").0;
        assert!((m[0] - at[0]).abs() < 0.5 && (m[1] - at[1]).abs() < 0.5, "marker at {m:?}, word at {at:?}");
        let p = rt.presented("lu-pop-1").expect("popover").0;
        assert!(p[1] >= at[1] + at[3], "popover hangs below the word, clear of the panel: {p:?}");
    }

    /// The overlay takes the pointer only over the popover, so the desktop around it still
    /// hovers and clicks; over a backdrop (no compositor) it takes the whole screen.
    #[test]
    fn only_the_popover_takes_the_pointer_unless_over_a_backdrop() {
        let mut rt = Runtime::new(crate::preview::init(), view, (1000.0, 680.0), Env { system_dark: true, scale: 1.0 });
        rt.frame(0.0);
        settle(&mut rt);
        let p = rt.presented("lu-pop-1").expect("popover").0;
        let regions = rt.hit_regions().to_vec();
        assert!(!regions.is_empty(), "the popover takes the pointer");
        for r in &regions {
            assert!(r[0] >= p[0] - 1.0 && r[1] >= p[1] - 1.0 && r[2] <= p[0] + p[2] + 1.0 && r[3] <= p[1] + p[3] + 1.0,
                "region {r:?} reaches past the popover {p:?}");
        }
        rt.update(|s, _| { let px = Pixels::new(); px.set(1, 1, vec![0; 4]); s.backdrop = Some(px); });
        settle(&mut rt);
        assert!(rt.hit_regions().iter().any(|r| r[2] - r[0] >= 999.0 && r[3] - r[1] >= 679.0), "{:?}", rt.hit_regions());
    }

    #[test]
    fn escape_and_a_click_outside_close_it() {
        let mut rt = Runtime::new(crate::preview::init(), view, (1000.0, 680.0), Env { system_dark: true, scale: 2.0 });
        rt.frame(0.0);
        settle(&mut rt);
        // A click on the panel keeps it.
        let (x, y) = rt.find("lu-panel").unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        settle(&mut rt);
        assert!(rt.state.shown.is_some());
        // A click elsewhere closes it.
        rt.pointer_down(950.0, 30.0);
        rt.pointer_up(950.0, 30.0);
        settle(&mut rt);
        assert!(rt.state.shown.is_none());
    }

    #[test]
    fn the_popover_hangs_below_with_room_and_flips_above_near_the_bottom() {
        let screen = [1920.0, 1080.0];
        let t = crate::style::theme(false);
        let (left, below, arrow, room) = place([500.0, 100.0, 60.0, 18.0], screen, &t);
        assert!(below && room >= 260.0);
        assert!((left + arrow + t.arrow.width / 2.0 - 530.0).abs() < 0.01, "arrow points at the word's middle");
        let (_, below, _, _) = place([500.0, 1000.0, 60.0, 18.0], screen, &t);
        assert!(!below);
        // At the screen's edge the panel stays on screen and the arrow follows the word,
        // clear of the panel's rounded corner.
        let (left, _, arrow, _) = place([2.0, 100.0, 30.0, 18.0], screen, &t);
        assert_eq!(left, EDGE);
        assert_eq!(arrow, t.popover.radius + 2.0);
    }
}
