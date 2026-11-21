//! The lookup pipeline, on one worker thread: pointer → capture around it → OCR → the word
//! (or the selected phrase) → highlight and popover → dictionary and Wikipedia, as they
//! arrive. Newer triggers supersede older ones by generation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::blocking::Connection;

use crate::config::Config;
use crate::dict::{self, Cache, Part};
use crate::desktop::{self, Desktop};
use crate::kwin::Pointer;
use crate::ocr::Ocr;
use crate::scan::{self, Hit};
use crate::ui::{self, Popover, Shown};

pub enum Job {
    /// Look up what's under the pointer (`None`: ask the desktop where it is).
    Trigger(Option<Pointer>, &'static str),
    /// Look up this text at the pointer.
    Text(String),
}

/// A display Rune made an overlay for, in global logical points.
#[derive(Clone, Debug)]
pub struct Screen { pub name: Option<String>, pub rect: [f64; 4], pub primary: bool, pub scale: f64 }

#[derive(Clone)]
pub struct Engine {
    jobs: Sender<Job>,
    pub generation: Arc<AtomicU64>,
    /// When the popover last opened, to tell a drag that follows its own press.
    pub opened: Arc<Mutex<Option<Instant>>>,
}

impl Engine {
    pub fn send(&self, job: Job) { let _ = self.jobs.send(job); }

    /// The press that opened the popover kept going: it was a drag, not a lookup.
    pub fn dragged(&self) {
        let recent = self.opened.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        if recent { close_all(); }
    }
}

pub fn close_all() {
    if let Some(all) = ui::OVERLAYS.get() {
        for o in all { o.update(|s, _| ui::dismiss(s)); }
    }
}

fn close_all_now() {
    if let Some(all) = ui::OVERLAYS.get() {
        for o in all { o.update(ui::dismiss_now); }
    }
}

pub fn start(config: Config, kind: desktop::Kind, screens: Vec<Screen>, pointer_replies: Receiver<String>) -> Engine {
    let (tx, rx) = mpsc::channel();
    let engine = Engine { jobs: tx, generation: Arc::new(AtomicU64::new(0)), opened: Arc::default() };
    let e = engine.clone();
    std::thread::Builder::new().name("tlum".into()).spawn(move || worker(e, config, kind, screens, rx, pointer_replies)).unwrap();
    engine
}

struct Ctx {
    engine: Engine,
    config: Config,
    screens: Vec<Screen>,
    desktop: Result<Box<dyn Desktop>, String>,
    ocr: Option<Result<Ocr, String>>,
    cache: Cache,
}

fn worker(engine: Engine, config: Config, kind: desktop::Kind, screens: Vec<Screen>, jobs: Receiver<Job>, replies: Receiver<String>) {
    let bus = Connection::session().map_err(|e| eprintln!("tlum: no session bus: {e}")).ok();
    let desktop = desktop::connect(kind, bus, replies, &screens);
    match &desktop {
        Ok(d) => eprintln!("tlum: desktop backend: {}", d.name()),
        Err(e) => eprintln!("tlum: no desktop backend: {e}"),
    }
    let mut cx = Ctx { engine, config, screens, desktop, ocr: None, cache: Cache::default() };
    // Load Tesseract now, not on the first lookup.
    cx.ocr = Some(Ocr::new(&cx.config.ocr_language));
    if let Some(Err(e)) = &cx.ocr { eprintln!("tlum: OCR unavailable: {e}"); }
    while let Ok(job) = jobs.recv() {
        // Only the newest trigger matters.
        let mut job = job;
        while let Ok(next) = jobs.try_recv() { job = next; }
        let started = Instant::now();
        let result = match job {
            Job::Trigger(at, why) => trigger(&mut cx, at).map(|q| format!("{why}: {q}")),
            Job::Text(t) => text(&mut cx, t.clone()).map(|_| format!("text: {t}")),
        };
        match result {
            Ok(what) => eprintln!("tlum: {what} ({} ms)", started.elapsed().as_millis()),
            Err(e) => eprintln!("tlum: {e}"),
        }
    }
}

fn desktop(cx: &mut Ctx) -> Result<&mut Box<dyn Desktop>, String> { cx.desktop.as_mut().map_err(|e| e.clone()) }

fn pointer(cx: &mut Ctx) -> Result<Pointer, String> {
    let p = desktop(cx)?.pointer()?;
    if std::env::var_os("TLUM_DEBUG").is_some() { eprintln!("tlum: pointer {p:?}"); }
    Ok(p)
}

/// The screen a pointer is on, and that screen's logical rect.
fn screen_for(cx: &Ctx, p: &Pointer) -> (usize, [f64; 4]) {
    let by_name = p.output.as_ref().and_then(|n| cx.screens.iter().position(|s| s.name.as_deref() == Some(n.as_str())));
    let by_rect = || cx.screens.iter().position(|s| p.x >= s.rect[0] && p.y >= s.rect[1] && p.x < s.rect[0] + s.rect[2] && p.y < s.rect[1] + s.rect[3]);
    let i = by_name.or_else(by_rect).or_else(|| cx.screens.iter().position(|s| s.primary)).unwrap_or(0);
    let rect = p.screen.or_else(|| cx.screens.get(i).map(|s| s.rect)).unwrap_or([0.0, 0.0, 1920.0, 1080.0]);
    (i, rect)
}

/// Hides a showing popover first, so the capture sees the page and not us.
fn clear_screen() {
    let showing = ui::VISIBLE.lock().unwrap().is_some();
    if showing {
        close_all_now();
        std::thread::sleep(Duration::from_millis(90));
    }
}

fn capture(cx: &mut Ctx, p: &Pointer, screen: [f64; 4]) -> Result<scan::Shot, String> {
    let (hw, hh) = (cx.config.capture_width / 2.0, cx.config.capture_height / 2.0);
    let x0 = (p.x - hw).max(screen[0]);
    let y0 = (p.y - hh).max(screen[1]);
    let x1 = (p.x + hw).min(screen[0] + screen[2]);
    let y1 = (p.y + hh).min(screen[1] + screen[3]);
    let rect = [x0.floor() as i32, y0.floor() as i32, (x1 - x0).ceil().max(1.0) as i32, (y1 - y0).ceil().max(1.0) as i32];
    desktop(cx)?.capture(rect, p)
}

fn find(cx: &mut Ctx, p: &Pointer, shot: &scan::Shot) -> Result<Option<Hit>, String> {
    let ocr = match cx.ocr.as_mut() {
        Some(Ok(o)) => o,
        Some(Err(e)) => return Err(format!("can't read the screen: {e}")),
        None => unreachable!(),
    };
    let prepared = scan::prepare(shot, [p.x, p.y]);
    let words = ocr.words(&prepared.gray, prepared.width, prepared.height);
    if std::env::var_os("TLUM_DEBUG").is_some() {
        eprintln!("tlum: OCR {}x{} → {:?}", prepared.width, prepared.height, words.iter().map(|w| (&w.text, w.conf as i32)).collect::<Vec<_>>());
        let _ = crate::image::write_gray_png(&crate::config::runtime_dir().join("last-ocr.png"), &prepared.gray, prepared.width, prepared.height);
    }
    Ok(scan::pick(&words, shot.origin, prepared.per_point, [p.x, p.y]))
}

fn trigger(cx: &mut Ctx, at: Option<Pointer>) -> Result<String, String> {
    clear_screen();
    let mut p = match at { Some(p) => p, None => pointer(cx)? };
    let (display, screen) = screen_for(cx, &p);
    p.screen = Some(screen);
    let shot = capture(cx, &p, screen)?;
    let Some(hit) = find(cx, &p, &shot)? else { return Err(format!("no word under the pointer at {:.0},{:.0}", p.x, p.y)) };
    let selection = desktop(cx)?.selection();
    let hit = scan::with_selection(hit, selection.as_deref());
    // Baked for both schemes: a style may colour the marker differently in each.
    let themes = crate::style::themes();
    let bake = |t: &crate::style::Theme| scan::highlight(&shot, hit.rect, t.marker.background.0, t.marker.ink.0, t.marker.padding);
    let marker = bake(&themes.0).map(|light| {
        let dark = if themes.1.marker == themes.0.marker { None } else { bake(&themes.1) };
        (light, dark)
    });
    let query = hit.query.clone();
    show(cx, display, &p, hit.query, hit.rect, marker);
    Ok(query)
}

fn text(cx: &mut Ctx, t: String) -> Result<(), String> {
    clear_screen();
    let mut p = pointer(cx).unwrap_or(Pointer { x: 400.0, y: 300.0, output: None, screen: None, scale: 1.0, area: None });
    let (display, screen) = screen_for(cx, &p);
    p.screen = Some(screen);
    show(cx, display, &p, t.trim().to_string(), [p.x - 1.0, p.y - 10.0, 2.0, 20.0], None);
    Ok(())
}

/// Shows the popover. Positions go to the overlay relative to its output; the overlay
/// subtracts where its surface starts (below a top panel, say), which `p.area` tells when the
/// desktop knows and the overlay measures otherwise.
type Baked = (u32, u32, Vec<u8>, [f64; 4]);

fn show(cx: &mut Ctx, display: usize, p: &Pointer, query: String, rect: [f64; 4], marker: Option<(Baked, Option<Baked>)>) {
    let Some(overlay) = ui::OVERLAYS.get().and_then(|v| v.get(display)).cloned() else { return };
    let screen = p.screen.unwrap_or([0.0, 0.0, 1920.0, 1080.0]);
    let offset = p.area.map(|a| [(a[0] - screen[0]) as f32, (a[1] - screen[1]) as f32]);
    let pointer = [(p.x - screen[0]) as f32, (p.y - screen[1]) as f32];
    // No compositor: the overlay can't be see-through, so it shows the screen as it is now.
    if cx.desktop.as_mut().is_ok_and(|d| d.opaque_overlays()) {
        let whole = desktop(cx).and_then(|d| d.capture([screen[0] as i32, screen[1] as i32, screen[2] as i32, screen[3] as i32], p));
        if let Ok(w) = whole {
            let px = rune::Pixels::new();
            px.set(w.width, w.height, w.rgba);
            overlay.update(move |s, _| s.backdrop = Some(px));
        }
    }
    let generation = cx.engine.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let local = |r: [f64; 4]| [(r[0] - screen[0]) as f32, (r[1] - screen[1]) as f32, r[2] as f32, r[3] as f32];
    let pixels = |(w, h, rgba, _): Baked| { let px = rune::Pixels::new(); px.set(w, h, rgba); px };
    let (word, pixels) = match marker {
        Some((light, dark)) => {
            let covered = light.3;
            let light = pixels(light);
            let dark = dark.map(pixels).unwrap_or_else(|| light.clone());
            (local(covered), Some((light, dark)))
        }
        None => (local(rect), None),
    };
    let cached = cx.cache.get(&query);
    let (definition, wiki) = cached.clone().unwrap_or((Part::Loading, Part::Loading));
    let shown = Shown {
        generation, query: query.clone(), word, marker: pixels, screen: [screen[2] as f32, screen[3] as f32],
        offset, pointer, shown_at: Instant::now(), definition, wiki, thumbnail: None,
    };
    overlay.update(move |s: &mut Popover, _| { s.hover = false; s.shown = Some(shown); });
    overlay.set_input(rune::OverlayInput::All);
    overlay.show();
    overlay.request_keys();
    if std::env::var_os("TLUM_DEBUG").is_some() {
        let o = overlay.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            eprintln!("tlum: overlay {:?} {:?} {:?} {:?}", o.availability(), o.failure(), o.surface_description(), o.capabilities());
        });
    }
    *ui::VISIBLE.lock().unwrap() = Some((generation, display));
    *cx.engine.opened.lock().unwrap() = Some(Instant::now());

    if let Some((_, Part::Found(w))) = &cached { fetch_thumbnail(&overlay, generation, w.thumbnail.clone()); }
    if cached.is_some() { return; }
    let lang = cx.config.language.clone();
    let results: Arc<Mutex<(Option<Part<dict::Definition>>, Option<Part<dict::Wiki>>)>> = Arc::default();
    let finish = {
        let (cache, query, results) = (cx.cache.clone(), query.clone(), results.clone());
        move || {
            let r = results.lock().unwrap();
            if let (Some(d), Some(w)) = (&r.0, &r.1) { cache.put(&query, d.clone(), w.clone()); }
        }
    };
    {
        let (overlay, q, lang, results, finish) = (overlay.clone(), query.clone(), lang.clone(), results.clone(), finish.clone());
        std::thread::spawn(move || {
            let d = dict::define(&q, &lang);
            results.lock().unwrap().0 = Some(d.clone());
            finish();
            overlay.update(move |s: &mut Popover, _| if let Some(v) = s.shown.as_mut().filter(|v| v.generation == generation) { v.definition = d; });
        });
    }
    std::thread::spawn(move || {
        let w = dict::wikipedia(&query, &lang);
        results.lock().unwrap().1 = Some(w.clone());
        finish();
        let thumb = w.found().and_then(|w| w.thumbnail.clone());
        overlay.update(move |s: &mut Popover, _| if let Some(v) = s.shown.as_mut().filter(|v| v.generation == generation) { v.wiki = w; });
        fetch_thumbnail(&overlay, generation, thumb);
    });
}

fn fetch_thumbnail(overlay: &rune::OverlayHandle<Popover>, generation: u64, url: Option<String>) {
    let Some(url) = url else { return };
    let overlay = overlay.clone();
    std::thread::spawn(move || {
        let Ok(bytes) = dict::get_bytes(&url) else { return };
        overlay.update(move |s: &mut Popover, _| {
            let Some(images) = &s.images else { return };
            let img = images.load(bytes);
            if let Some(v) = s.shown.as_mut().filter(|v| v.generation == generation) { v.thumbnail = Some(img); }
        });
    });
}
