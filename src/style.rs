//! The popover's look, in a small dialect of CSS: `assets/default.css` is built in, and
//! `~/.config/tlum/style.css` (or `$TLUM_STYLE`) cascades over it, reloaded when saved.
//!
//! Each part of the popover is one class (`.popover`, `.headword`, `.sense`, …; the default
//! sheet lists them all). Supported: selector lists, `:root` custom properties and `var()`,
//! `@media (prefers-color-scheme: dark|light)`, comments, colours (`#rgb[a]`, `#rrggbb[aa]`,
//! `rgb[a]()`, `hsl[a]()`, a few names), lengths in `px`, `pt`, `em`, `rem` and `%` of the
//! panel's width, and durations in `s` or `ms`. Later rules win, as in CSS; specificity doesn't
//! come into it since every selector is one class.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use rune::{Color, Family, TextCase, TextStyle};

pub const DEFAULT_CSS: &str = include_str!("../assets/default.css");

/// A colour as written: sRGB bytes with straight alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    /// Rune's colours are linear light.
    pub fn color(self) -> Color {
        let [r, g, b, a] = self.0;
        rune::color::hex(&format!("#{r:02x}{g:02x}{b:02x}{a:02x}")).unwrap_or(Color::BLACK)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Shadow { pub x: f32, pub y: f32, pub blur: f32, pub spread: f32, pub color: Rgba }

impl Shadow {
    pub fn rune(self) -> rune::Shadow {
        rune::Shadow { x: self.x, y: self.y, blur: self.blur, spread: self.spread, color: self.color.color(), inset: false }
    }
}

/// One kind of text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Text {
    pub color: Rgba,
    pub size: f32,
    pub weight: f32,
    pub italic: bool,
    pub family: Option<&'static str>,
    /// Points; 0 lets the font decide.
    pub line_height: f32,
    pub letter_spacing: f32,
    /// 0 as written, 1 upper case, 2 lower case.
    pub case: u8,
    pub hidden: bool,
}

/// What CSS gives text nobody styled: black, 13 px, regular.
impl Default for Text {
    fn default() -> Self {
        Text { color: Rgba([0, 0, 0, 255]), size: 13.0, weight: 400.0, italic: false, family: None, line_height: 0.0, letter_spacing: 0.0, case: 0, hidden: false }
    }
}

impl Text {
    pub fn style(&self, inherited: Option<&'static str>) -> TextStyle {
        let mut s = TextStyle::new(self.size, self.weight).lh(self.line_height).tracking(self.letter_spacing);
        if let Some(f) = self.family.or(inherited).filter(|f| !f.is_empty()) { s = s.family(Family::Named(f)); }
        if self.italic { s = s.italic(); }
        s
    }
    pub fn case(&self) -> Option<TextCase> {
        match self.case { 1 => Some(TextCase::Uppercase), 2 => Some(TextCase::Lowercase), _ => None }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panel {
    pub width: f32,
    pub max_height: f32,
    pub padding: [f32; 4],
    pub gap: f32,
    pub radius: f32,
    pub border_width: f32,
    pub border: Rgba,
    pub background: Rgba,
    pub shadow: Option<Shadow>,
    pub family: Option<&'static str>,
    pub animate: bool,
    /// Seconds the opening spring takes.
    pub duration: f32,
    /// How far it starts scaled down (1 = no scale).
    pub scale_from: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marker {
    pub background: Rgba,
    pub ink: Rgba,
    pub radius: f32,
    pub padding: [f32; 2],
    pub shadow: Option<Shadow>,
    pub hidden: bool,
    pub animate: bool,
    pub duration: f32,
    pub scale_from: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Block { pub width: f32, pub height: f32, pub radius: f32, pub color: Rgba, pub hidden: bool }

/// Everything the popover draws with, resolved for one colour scheme.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Theme {
    pub popover: Panel,
    pub arrow: Block,
    pub marker: Marker,
    pub rule: Block,
    pub thumbnail: Block,
    pub section_title: Text,
    pub link: Text,
    pub headword: Text,
    pub pronunciation: Text,
    pub note: Text,
    pub part_of_speech: Text,
    pub sense: Text,
    pub sense_number: Text,
    pub example: Text,
    pub synonyms: Text,
    pub synonyms_label: Text,
    pub wiki_title: Text,
    pub wiki_description: Text,
    pub wiki_extract: Text,
    pub status: Text,
    pub hide_dictionary: bool,
    pub hide_thesaurus: bool,
    pub hide_wikipedia: bool,
    pub wiki_lines: usize,
    pub senses: usize,
}

// --- parsing ------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Rule {
    selectors: Vec<String>,
    decls: Vec<(String, String, usize)>,
    /// `Some(true)` inside `@media (prefers-color-scheme: dark)`.
    dark: Option<bool>,
}

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        match rest[i + 2..].find("*/") {
            // Keep the comment's newlines so line numbers still match.
            Some(j) => { out.extend(rest[i..i + 2 + j + 2].chars().filter(|c| *c == '\n')); rest = &rest[i + 2 + j + 2..]; }
            None => { rest = ""; }
        }
    }
    out.push_str(rest);
    out
}

fn parse(src: &str, name: &str, warnings: &mut Vec<String>) -> Vec<Rule> {
    let src = strip_comments(src);
    let mut rules = Vec::new();
    parse_block(&src, 1, None, name, &mut rules, warnings);
    rules
}

/// Rules in `src` (which starts on `line`), until its end.
fn parse_block(src: &str, line: usize, dark: Option<bool>, name: &str, rules: &mut Vec<Rule>, warnings: &mut Vec<String>) {
    let mut i = 0;
    let bytes = src.as_bytes();
    let line_at = |pos: usize| line + src[..pos].matches('\n').count();
    while i < src.len() {
        let Some(open) = src[i..].find('{').map(|o| o + i) else {
            if !src[i..].trim().is_empty() { warnings.push(format!("{name}:{}: stray text {:?}", line_at(i), src[i..].trim())); }
            break;
        };
        // The matching brace.
        let mut depth = 0;
        let mut close = None;
        for (k, b) in bytes.iter().enumerate().skip(open) {
            match b { b'{' => depth += 1, b'}' => { depth -= 1; if depth == 0 { close = Some(k); break; } } _ => {} }
        }
        let Some(close) = close else { warnings.push(format!("{name}:{}: unclosed {{", line_at(open))); break };
        let head = src[i..open].trim();
        let body_line = line_at(open);
        if let Some(media) = head.strip_prefix("@media") {
            let m = media.replace(' ', "").to_lowercase();
            let scheme = if m.contains("prefers-color-scheme:dark") { Some(true) } else if m.contains("prefers-color-scheme:light") { Some(false) } else { None };
            match scheme {
                Some(d) => parse_block(&src[open + 1..close], body_line, Some(d), name, rules, warnings),
                None => warnings.push(format!("{name}:{}: only @media (prefers-color-scheme: dark|light) is supported", line_at(i))),
            }
        } else if head.starts_with('@') {
            warnings.push(format!("{name}:{}: {} isn't supported", line_at(i), head.split_whitespace().next().unwrap_or(head)));
        } else {
            let selectors: Vec<String> = head.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let mut decls = Vec::new();
            let body = &src[open + 1..close];
            let mut at = 0;
            for decl in body.split(';') {
                let decl_line = body_line + body[..at].matches('\n').count();
                let lead = decl[..decl.len() - decl.trim_start().len()].matches('\n').count();
                at += decl.len() + 1;
                let d = decl.trim();
                if d.is_empty() { continue; }
                match d.split_once(':') {
                    Some((k, v)) => decls.push((k.trim().to_lowercase(), v.trim().trim_end_matches("!important").trim().to_string(), decl_line + lead)),
                    None => warnings.push(format!("{name}:{}: expected `property: value`, found {d:?}", decl_line + lead)),
                }
            }
            rules.push(Rule { selectors, decls, dark });
        }
        i = close + 1;
    }
}

// --- values -------------------------------------------------------------------------------

fn named(name: &str) -> Option<[u8; 4]> {
    Some(match name {
        "transparent" => [0, 0, 0, 0], "white" => [255, 255, 255, 255], "black" => [0, 0, 0, 255],
        "red" => [255, 0, 0, 255], "green" => [0, 128, 0, 255], "blue" => [0, 0, 255, 255],
        "yellow" => [255, 255, 0, 255], "orange" => [255, 165, 0, 255], "purple" => [128, 0, 128, 255],
        "pink" => [255, 192, 203, 255], "gray" | "grey" => [128, 128, 128, 255], "gold" => [255, 215, 0, 255],
        "silver" => [192, 192, 192, 255], "navy" => [0, 0, 128, 255], "teal" => [0, 128, 128, 255],
        _ => return None,
    })
}

pub fn color(v: &str) -> Result<Rgba, String> {
    let v = v.trim().to_lowercase();
    if let Some(h) = v.strip_prefix('#') {
        let d = |s: &str| u8::from_str_radix(s, 16).map_err(|_| format!("bad colour #{h}"));
        let c = match h.len() {
            3 | 4 => {
                let n: Vec<u8> = h.chars().map(|c| d(&c.to_string()).map(|x| x * 17)).collect::<Result<_, _>>()?;
                [n[0], n[1], n[2], *n.get(3).unwrap_or(&255)]
            }
            6 | 8 => [d(&h[0..2])?, d(&h[2..4])?, d(&h[4..6])?, if h.len() == 8 { d(&h[6..8])? } else { 255 }],
            _ => return Err(format!("bad colour #{h}")),
        };
        return Ok(Rgba(c));
    }
    if let Some(c) = named(&v) { return Ok(Rgba(c)); }
    let (func, args) = v.split_once('(').ok_or_else(|| format!("unknown colour {v:?}"))?;
    let args: Vec<&str> = args.trim_end_matches(')').split([',', ' ', '/']).map(str::trim).filter(|a| !a.is_empty()).collect();
    let num = |s: &str, scale: f32| -> Result<f32, String> {
        match s.strip_suffix('%') {
            Some(p) => p.parse::<f32>().map(|p| p / 100.0 * scale).map_err(|_| format!("bad number {s:?}")),
            None => s.trim_end_matches("deg").parse::<f32>().map_err(|_| format!("bad number {s:?}")),
        }
    };
    let alpha = |i: usize| -> Result<u8, String> { args.get(i).map_or(Ok(255), |a| num(a, 1.0).map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8)) };
    match func {
        "rgb" | "rgba" if args.len() >= 3 => {
            let ch = |i: usize| num(args[i], 255.0).map(|x| x.round().clamp(0.0, 255.0) as u8);
            Ok(Rgba([ch(0)?, ch(1)?, ch(2)?, alpha(3)?]))
        }
        "hsl" | "hsla" if args.len() >= 3 => {
            let (h, s, l) = (num(args[0], 360.0)?.rem_euclid(360.0), num(args[1], 1.0)?, num(args[2], 1.0)?);
            let (s, l) = (if s > 1.0 { s / 100.0 } else { s }, if l > 1.0 { l / 100.0 } else { l });
            let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
            let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
            let (r, g, b) = match (h / 60.0) as u32 { 0 => (c, x, 0.0), 1 => (x, c, 0.0), 2 => (0.0, c, x), 3 => (0.0, x, c), 4 => (x, 0.0, c), _ => (c, 0.0, x) };
            let m = l - c / 2.0;
            let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
            Ok(Rgba([to(r), to(g), to(b), alpha(3)?]))
        }
        _ => Err(format!("unknown colour {v:?}")),
    }
}

/// A length in points. `em` is relative to `em` points; `%` to `percent_of`.
pub fn length(v: &str, em: f32, percent_of: f32) -> Result<f32, String> {
    let v = v.trim().to_lowercase();
    let (n, unit) = v.find(|c: char| c.is_ascii_alphabetic() || c == '%').map_or((v.as_str(), ""), |i| v.split_at(i));
    let n: f32 = n.parse().map_err(|_| format!("bad length {v:?}"))?;
    Ok(match unit {
        "" | "px" | "pt" => n,
        "em" => n * em,
        "rem" => n * 13.0,
        "%" => n / 100.0 * percent_of,
        u => return Err(format!("unknown unit {u:?} in {v:?}")),
    })
}

fn seconds(v: &str) -> Result<f32, String> {
    let v = v.trim().to_lowercase();
    if let Some(ms) = v.strip_suffix("ms") { return ms.trim().parse::<f32>().map(|x| x / 1000.0).map_err(|_| format!("bad duration {v:?}")); }
    v.strip_suffix('s').unwrap_or(&v).trim().parse().map_err(|_| format!("bad duration {v:?}"))
}

/// Splits on spaces outside parentheses: `0 10px 36px rgba(0, 0, 0, .3)`.
fn words(v: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut depth = 0;
    for c in v.trim().chars() {
        match c {
            '(' => { depth += 1; out.last_mut().unwrap().push(c); }
            ')' => { depth -= 1; out.last_mut().unwrap().push(c); }
            c if c.is_whitespace() && depth == 0 => if !out.last().unwrap().is_empty() { out.push(String::new()) },
            c => out.last_mut().unwrap().push(c),
        }
    }
    out.retain(|w| !w.is_empty());
    out
}

fn shadow(v: &str) -> Result<Option<Shadow>, String> {
    if v.trim() == "none" { return Ok(None); }
    // One shadow: the first of a list, split at commas outside parentheses.
    let mut depth = 0;
    let end = v.char_indices().find_map(|(i, c)| match c { '(' => { depth += 1; None } ')' => { depth -= 1; None } ',' if depth == 0 => Some(i), _ => None });
    let first = &v[..end.unwrap_or(v.len())];
    let mut lens = Vec::new();
    let mut col = Rgba([0, 0, 0, 90]);
    for w in words(first) {
        match length(&w, 13.0, 0.0) {
            Ok(n) => lens.push(n),
            Err(_) => col = color(&w)?,
        }
    }
    if lens.len() < 2 { return Err(format!("box-shadow needs at least x and y offsets: {v:?}")); }
    Ok(Some(Shadow { x: lens[0], y: lens[1], blur: *lens.get(2).unwrap_or(&0.0), spread: *lens.get(3).unwrap_or(&0.0), color: col }))
}

fn sides(v: &str) -> Result<[f32; 4], String> {
    let n: Vec<f32> = words(v).iter().map(|w| length(w, 13.0, 0.0)).collect::<Result<_, _>>()?;
    Ok(match n.len() { 1 => [n[0]; 4], 2 => [n[0], n[1], n[0], n[1]], 3 => [n[0], n[1], n[2], n[1]], 4 => [n[0], n[1], n[2], n[3]], _ => return Err(format!("bad padding {v:?}")) })
}

fn weight(v: &str) -> Result<f32, String> {
    match v.trim() { "normal" => Ok(400.0), "bold" => Ok(700.0), "lighter" => Ok(300.0), "bolder" => Ok(800.0), n => n.parse().map_err(|_| format!("bad font-weight {n:?}")) }
}

/// Font names live for the program; the same name is stored once.
fn intern(s: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut all = NAMES.lock().unwrap();
    if let Some(n) = all.iter().find(|n| **n == s) { return n; }
    let n: &'static str = Box::leak(s.to_string().into_boxed_str());
    all.push(n);
    n
}

/// `None` inherits the popover's family; `Some("")` is the built-in sans (Inter).
fn family(v: &str) -> Option<&'static str> {
    let v = v.trim();
    if v.is_empty() || v == "inherit" { return None; }
    if matches!(v, "system-ui" | "sans-serif" | "initial") { return Some(""); }
    Some(intern(&v.split(',').map(|f| f.trim().trim_matches(['"', '\'']).to_string()).collect::<Vec<_>>().join(", ")))
}

fn hidden(prop: &str, v: &str) -> Option<bool> {
    match (prop, v.trim()) {
        ("display", "none") | ("visibility", "hidden") => Some(true),
        ("display", _) | ("visibility", "visible") => Some(false),
        _ => None,
    }
}

fn text_prop(t: &mut Text, prop: &str, v: &str) -> Result<bool, String> {
    if let Some(h) = hidden(prop, v) { t.hidden = h; return Ok(true); }
    match prop {
        "color" => t.color = color(v)?,
        "font-size" => t.size = length(v, t.size.max(1.0), t.size)?,
        "font-weight" => t.weight = weight(v)?,
        "font-style" => t.italic = matches!(v.trim(), "italic" | "oblique"),
        "font-family" => t.family = family(v),
        "line-height" => t.line_height = match v.trim().parse::<f32>() { Ok(k) => k * t.size, Err(_) => length(v, t.size, t.size)? },
        "letter-spacing" => t.letter_spacing = if v.trim() == "normal" { 0.0 } else { length(v, t.size, t.size)? },
        "text-transform" => t.case = match v.trim() { "uppercase" => 1, "lowercase" => 2, "none" => 0, o => return Err(format!("text-transform {o:?} isn't supported")) },
        _ => return Ok(false),
    }
    Ok(true)
}

fn animation(prop: &str, v: &str, animate: &mut bool, duration: &mut f32, scale_from: &mut f32) -> Result<bool, String> {
    match prop {
        "transition" | "animation" => {
            if v.trim() == "none" { *animate = false; } else {
                *animate = true;
                if let Some(d) = words(v).iter().find_map(|w| seconds(w).ok().filter(|_| w.ends_with('s'))) { *duration = d; }
            }
        }
        "transition-duration" | "animation-duration" => { *duration = seconds(v)?; *animate = *duration > 0.0; }
        "--scale-from" => *scale_from = v.trim().parse().map_err(|_| format!("bad scale {v:?}"))?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn apply(t: &mut Theme, selector: &str, prop: &str, v: &str) -> Result<(), String> {
    let known = match selector {
        ".popover" => {
            let p = &mut t.popover;
            match prop {
                "width" => { p.width = length(v, 13.0, p.width)?; true }
                "max-height" => { p.max_height = length(v, 13.0, p.max_height)?; true }
                "padding" => { p.padding = sides(v)?; true }
                "gap" | "row-gap" => { p.gap = length(v, 13.0, 0.0)?; true }
                "border-radius" => { p.radius = length(v, 13.0, 0.0)?; true }
                "border-width" => { p.border_width = length(v, 13.0, 0.0)?; true }
                "border-color" => { p.border = color(v)?; true }
                "border" => {
                    if v.trim() == "none" { p.border_width = 0.0; } else {
                        for w in words(v) {
                            if let Ok(n) = length(&w, 13.0, 0.0) { p.border_width = n; } else if w != "solid" { p.border = color(&w)?; }
                        }
                    }
                    true
                }
                "background" | "background-color" => { p.background = color(v)?; true }
                "box-shadow" => { p.shadow = shadow(v)?; true }
                "font-family" => { p.family = family(v); true }
                _ => animation(prop, v, &mut p.animate, &mut p.duration, &mut p.scale_from)?,
            }
        }
        ".marker" => {
            let m = &mut t.marker;
            if let Some(h) = hidden(prop, v) { m.hidden = h; true } else {
                match prop {
                    "background" | "background-color" => { m.background = color(v)?; true }
                    "color" => { m.ink = color(v)?; true }
                    "border-radius" => { m.radius = length(v, 13.0, 0.0)?; true }
                    "padding" => { let s = sides(v)?; m.padding = [s[1], s[0]]; true }
                    "box-shadow" => { m.shadow = shadow(v)?; true }
                    _ => animation(prop, v, &mut m.animate, &mut m.duration, &mut m.scale_from)?,
                }
            }
        }
        ".arrow" | ".rule" | ".thumbnail" => {
            let b = match selector { ".arrow" => &mut t.arrow, ".rule" => &mut t.rule, _ => &mut t.thumbnail };
            if let Some(h) = hidden(prop, v) { b.hidden = h; true } else {
                match prop {
                    "width" => { b.width = length(v, 13.0, 0.0)?; true }
                    "height" => { b.height = length(v, 13.0, 0.0)?; true }
                    "size" => { let n = length(v, 13.0, 0.0)?; b.width = n; b.height = n; true }
                    "border-radius" => { b.radius = length(v, 13.0, 0.0)?; true }
                    "background" | "background-color" | "color" => { b.color = color(v)?; true }
                    _ => false,
                }
            }
        }
        ".dictionary" | ".thesaurus" | ".wikipedia" => match hidden(prop, v) {
            Some(h) => { *match selector { ".dictionary" => &mut t.hide_dictionary, ".thesaurus" => &mut t.hide_thesaurus, _ => &mut t.hide_wikipedia } = h; true }
            None => false,
        },
        _ => {
            let text = match selector {
                ".section-title" => &mut t.section_title, ".link" => &mut t.link, ".headword" => &mut t.headword,
                ".pronunciation" => &mut t.pronunciation, ".note" => &mut t.note, ".part-of-speech" => &mut t.part_of_speech,
                ".sense" => &mut t.sense, ".sense-number" => &mut t.sense_number, ".example" => &mut t.example,
                ".synonyms" => &mut t.synonyms, ".synonyms-label" => &mut t.synonyms_label, ".wiki-title" => &mut t.wiki_title,
                ".wiki-description" => &mut t.wiki_description, ".wiki-extract" => &mut t.wiki_extract, ".status" => &mut t.status,
                s => return Err(format!("unknown selector {s:?} (see `tlum style --default` for the parts)")),
            };
            match (selector, prop) {
                (".wiki-extract", "max-lines" | "line-clamp" | "-webkit-line-clamp") => { t.wiki_lines = v.trim().parse().map_err(|_| format!("bad line count {v:?}"))?; true }
                (".sense", "max-count" | "--max-count") => { t.senses = v.trim().parse().map_err(|_| format!("bad count {v:?}"))?; true }
                _ => text_prop(text, prop, v)?,
            }
        }
    };
    if known { Ok(()) } else { Err(format!("{selector} doesn't take {prop:?}")) }
}

/// `var(--name, fallback)` replaced from `vars`.
fn substitute(v: &str, vars: &HashMap<String, String>) -> Result<String, String> {
    let mut out = v.to_string();
    for _ in 0..8 {
        let Some(start) = out.find("var(") else { return Ok(out) };
        let mut depth = 0;
        let end = out[start..].char_indices().find_map(|(i, c)| match c { '(' => { depth += 1; None } ')' => { depth -= 1; (depth == 0).then_some(start + i) } _ => None })
            .ok_or_else(|| format!("unclosed var() in {v:?}"))?;
        let inner = &out[start + 4..end];
        let (name, fallback) = inner.split_once(',').map_or((inner.trim(), None), |(n, f)| (n.trim(), Some(f.trim())));
        let value = vars.get(name).map(String::as_str).or(fallback).ok_or_else(|| format!("{name} isn't defined"))?.to_string();
        out.replace_range(start..=end, &value);
    }
    Err(format!("var() nested too deeply in {v:?}"))
}

fn compute(rules: &[(Rule, &str)], dark: bool, warnings: &mut Vec<String>) -> Theme {
    let applies = |r: &Rule| r.dark.is_none_or(|d| d == dark);
    let mut vars = HashMap::new();
    for (r, _) in rules.iter().filter(|(r, _)| applies(r)) {
        if r.selectors.iter().any(|s| s == ":root" || s == "*" || s == "html") {
            for (k, v, _) in r.decls.iter().filter(|d| d.0.starts_with("--")) { vars.insert(k.clone(), v.clone()); }
        }
    }
    let mut t = Theme::default();
    for (r, file) in rules.iter().filter(|(r, _)| applies(r)) {
        for sel in &r.selectors {
            if sel == ":root" || sel == "html" { continue; }
            for (k, v, line) in &r.decls {
                if k.starts_with("--") && k != "--scale-from" && k != "--max-count" { continue; }
                let result = substitute(v, &vars).and_then(|v| apply(&mut t, sel, k, &v));
                if let Err(e) = result { warnings.push(format!("{file}:{line}: {e}")); }
            }
        }
    }
    t
}

// --- the current style --------------------------------------------------------------------

pub fn user_path() -> std::path::PathBuf {
    std::env::var_os("TLUM_STYLE").map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::config::config_path().with_file_name("style.css"))
}

/// The light and dark themes, from the defaults and the user's sheet.
pub fn build(user: Option<&str>) -> (Theme, Theme, Vec<String>) {
    let mut warnings = Vec::new();
    let mut rules: Vec<(Rule, &str)> = parse(DEFAULT_CSS, "default.css", &mut warnings).into_iter().map(|r| (r, "default.css")).collect();
    let name: &'static str = intern(&user_path().file_name().map_or("style.css".into(), |n| n.to_string_lossy().into_owned()));
    if let Some(src) = user { rules.extend(parse(src, name, &mut warnings).into_iter().map(|r| (r, name))); }
    let mut dup = Vec::new();
    let light = compute(&rules, false, &mut warnings);
    let dark = compute(&rules, true, &mut dup);
    for w in dup { if !warnings.contains(&w) { warnings.push(w); } }
    (light, dark, warnings)
}

static CURRENT: RwLock<Option<Arc<(Theme, Theme)>>> = RwLock::new(None);

/// Reads the user's sheet again; returns what was wrong with it.
pub fn reload() -> Vec<String> {
    let user = std::fs::read_to_string(user_path()).ok();
    let (light, dark, warnings) = build(user.as_deref());
    *CURRENT.write().unwrap() = Some(Arc::new((light, dark)));
    warnings
}

pub fn themes() -> Arc<(Theme, Theme)> {
    if let Some(t) = CURRENT.read().unwrap().clone() { return t; }
    for w in reload() { eprintln!("tlum: {w}"); }
    CURRENT.read().unwrap().clone().unwrap()
}

pub fn theme(dark: bool) -> Theme { let t = themes(); if dark { t.1.clone() } else { t.0.clone() } }

/// Reloads the sheet whenever it's saved and calls `changed`. Editors save by writing in
/// place or by renaming a new file over it, so this watches the folder.
pub fn watch(changed: impl Fn() + Send + 'static) {
    let path = user_path();
    let Some(dir) = path.parent().map(|d| d.to_path_buf()) else { return };
    let _ = std::fs::create_dir_all(&dir);
    std::thread::Builder::new().name("style-watch".into()).spawn(move || {
        #[allow(unsafe_code)]
        // SAFETY: plain inotify calls on a descriptor this thread owns.
        unsafe {
            let fd = libc::inotify_init1(libc::IN_CLOEXEC);
            if fd < 0 { return; }
            let c = std::ffi::CString::new(dir.to_string_lossy().as_bytes()).unwrap();
            libc::inotify_add_watch(fd, c.as_ptr(), libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
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
                let warnings = reload();
                if warnings.is_empty() { eprintln!("tlum: style reloaded"); }
                for w in warnings { eprintln!("tlum: {w}"); }
                changed();
            }
        }
    }).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_sheet_is_clean_and_complete() {
        let (light, dark, warnings) = build(None);
        assert!(warnings.is_empty(), "{warnings:#?}");
        for t in [&light, &dark] {
            assert!(t.popover.width >= 200.0 && t.popover.background.0[3] == 255);
            assert!(t.headword.size > t.sense.size && t.sense.color.0[3] > 0);
            assert!(t.wiki_lines > 0 && t.senses > 0);
        }
        assert_ne!(light.popover.background, dark.popover.background);
    }

    #[test]
    fn colours_parse_like_css() {
        assert_eq!(color("#fff").unwrap(), Rgba([255, 255, 255, 255]));
        assert_eq!(color("#11223380").unwrap(), Rgba([0x11, 0x22, 0x33, 0x80]));
        assert_eq!(color("rgba(10, 20, 30, 0.5)").unwrap(), Rgba([10, 20, 30, 128]));
        assert_eq!(color("rgb(10 20 30 / 50%)").unwrap(), Rgba([10, 20, 30, 128]));
        assert_eq!(color("hsl(120, 100%, 50%)").unwrap(), Rgba([0, 255, 0, 255]));
        assert_eq!(color("transparent").unwrap().0[3], 0);
        assert!(color("#12").is_err() && color("bogus").is_err());
    }

    #[test]
    fn user_rules_cascade_over_the_defaults_per_scheme_with_variables() {
        let css = r#"
            /* a comment */
            :root { --accent: #ff0066; --size: 30px }
            .headword, .sense-number { color: var(--accent) }
            .headword { font-size: var(--size); font-family: "Noto Serif", serif }
            @media (prefers-color-scheme: dark) {
                .popover { background: #000000cc; border: none; box-shadow: none }
            }
            .thesaurus { display: none }
            .wiki-extract { -webkit-line-clamp: 3 }
            .marker { background: hsl(200, 80%, 60%); transition: none }
        "#;
        let (light, dark, warnings) = build(Some(css));
        assert!(warnings.is_empty(), "{warnings:#?}");
        assert_eq!(light.headword.color, Rgba([255, 0, 102, 255]));
        assert_eq!(dark.sense_number.color, Rgba([255, 0, 102, 255]));
        assert_eq!(light.headword.size, 30.0);
        assert_eq!(light.headword.family, Some("Noto Serif, serif"));
        assert_eq!(build(Some(".link { font-family: sans-serif }")).0.link.family, Some(""), "the default sans, not the popover's font");
        assert_eq!(dark.popover.background, Rgba([0, 0, 0, 0xcc]));
        assert!(dark.popover.shadow.is_none() && dark.popover.border_width == 0.0);
        assert!(light.popover.shadow.is_some(), "the light scheme keeps its shadow");
        assert!(light.hide_thesaurus && dark.hide_thesaurus && !light.hide_wikipedia);
        assert_eq!(light.wiki_lines, 3);
        assert!(!light.marker.animate);
    }

    #[test]
    fn mistakes_are_reported_with_their_lines() {
        let css = ".headword {\n  colour: red;\n  font-size: big;\n}\n.nope { color: red }\n.sense { color: var(--missing) }\n";
        let (_, _, w) = build(Some(css));
        assert!(w.iter().any(|w| w.starts_with("style.css:2:") && w.contains("colour")), "{w:#?}");
        assert!(w.iter().any(|w| w.starts_with("style.css:3:") && w.contains("big")), "{w:#?}");
        assert!(w.iter().any(|w| w.contains(".nope")), "{w:#?}");
        assert!(w.iter().any(|w| w.contains("--missing")), "{w:#?}");
    }
}
