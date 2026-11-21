//! From a screenshot around the pointer to the word under it: preparing the image for OCR,
//! choosing the word (or the selected phrase it belongs to), and painting the yellow
//! highlight macOS draws over a looked-up word.

use crate::ocr::Word;

/// A captured part of the screen.
#[derive(Clone)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    /// Straight RGBA, row-major, `width * 4` bytes a row.
    pub rgba: Vec<u8>,
    /// Top-left of the capture in global logical (compositor) coordinates.
    pub origin: [f64; 2],
    /// Image pixels per logical point.
    pub scale: f64,
}

impl Shot {
    pub fn lum(&self, x: u32, y: u32) -> f32 {
        let i = ((y * self.width + x) * 4) as usize;
        let p = &self.rgba[i..i + 3];
        (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
    }
}

/// The image Tesseract reads: grayscale, dark text on light, upscaled so UI text is ~3x.
pub struct Prepared {
    pub gray: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// OCR pixels per logical point.
    pub per_point: f64,
}

/// The luminance most of `rect` (image pixels) has: the background behind text.
fn background_lum(shot: &Shot, rect: [u32; 4]) -> f32 {
    let mut hist = [0u32; 32];
    for y in rect[1]..rect[3].min(shot.height) {
        for x in rect[0]..rect[2].min(shot.width) {
            hist[((shot.lum(x, y) * 31.0).round() as usize).min(31)] += 1;
        }
    }
    let (i, _) = hist.iter().enumerate().max_by_key(|(_, n)| **n).unwrap_or((31, &0));
    i as f32 / 31.0
}

pub fn prepare(shot: &Shot, pointer: [f64; 2]) -> Prepared {
    // Polarity from what's around the pointer: a dark page inverts so text reads dark.
    let (px, py) = (((pointer[0] - shot.origin[0]) * shot.scale) as i64, ((pointer[1] - shot.origin[1]) * shot.scale) as i64);
    let r = (60.0 * shot.scale) as i64;
    let clamp = |v: i64, hi: u32| v.clamp(0, hi as i64) as u32;
    let near = [clamp(px - r, shot.width), clamp(py - r / 3, shot.height), clamp(px + r, shot.width), clamp(py + r / 3, shot.height)];
    let invert = background_lum(shot, near) < 0.5;

    let k = (3.0 / shot.scale).round().max(1.0) as u32;
    let (w, h) = (shot.width * k, shot.height * k);
    let mut gray = vec![0u8; (w * h) as usize];
    let src = |x: u32, y: u32| -> f32 { let l = shot.lum(x.min(shot.width - 1), y.min(shot.height - 1)); if invert { 1.0 - l } else { l } };
    for y in 0..h {
        let fy = (y as f32 + 0.5) / k as f32 - 0.5;
        let (y0, ty) = (fy.floor().max(0.0) as u32, fy - fy.floor());
        for x in 0..w {
            let fx = (x as f32 + 0.5) / k as f32 - 0.5;
            let (x0, tx) = (fx.floor().max(0.0) as u32, fx - fx.floor());
            let top = src(x0, y0) * (1.0 - tx) + src(x0 + 1, y0) * tx;
            let bot = src(x0, y0 + 1) * (1.0 - tx) + src(x0 + 1, y0 + 1) * tx;
            gray[(y * w + x) as usize] = ((top * (1.0 - ty) + bot * ty) * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    Prepared { gray, width: w, height: h, per_point: shot.scale * k as f64 }
}

/// A word on screen, in global logical coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenWord {
    pub text: String,
    /// x, y, width, height.
    pub rect: [f64; 4],
    pub line: u32,
}

/// What a trigger found under the pointer.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// What to look up.
    pub query: String,
    /// The words to highlight, unioned.
    pub rect: [f64; 4],
    /// Neighbours on the same line, for phrases (proper names).
    pub line: Vec<ScreenWord>,
    /// The pointer's word within `line`.
    pub index: usize,
}

fn is_word_char(c: char) -> bool { c.is_alphanumeric() }

/// The word without surrounding punctuation, and how much of the box that trims (fractions
/// of the length from each end), so the highlight hugs the letters.
pub fn clean(raw: &str) -> Option<(String, f64, f64)> {
    let chars: Vec<char> = raw.chars().collect();
    let start = chars.iter().position(|c| is_word_char(*c))?;
    let end = chars.iter().rposition(|c| is_word_char(*c))? + 1;
    let word: String = chars[start..end].iter().collect();
    // Inner punctuation that isn't part of a word (`end.Next`, `a/b`) keeps the part under
    // the pointer only when the caller asks; here keep apostrophes and hyphens.
    if !word.chars().any(char::is_alphabetic) { return None; }
    let n = chars.len().max(1) as f64;
    Some((word, start as f64 / n, (chars.len() - end) as f64 / n))
}

fn to_screen(w: &Word, origin: [f64; 2], per_point: f64) -> Option<ScreenWord> {
    let (text, lead, trail) = clean(w.text.trim())?;
    if w.conf < 35.0 { return None; }
    let [l, t, r, b] = w.bbox.map(|v| v as f64 / per_point);
    let width = r - l;
    let (l, r) = (l + width * lead, r - width * trail);
    Some(ScreenWord { text, rect: [origin[0] + l, origin[1] + t, r - l, b - t], line: w.line })
}

fn union(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let (x0, y0) = (a[0].min(b[0]), a[1].min(b[1]));
    let (x1, y1) = ((a[0] + a[2]).max(b[0] + b[2]), (a[1] + a[3]).max(b[1] + b[3]));
    [x0, y0, x1 - x0, y1 - y0]
}

/// The word under `pointer`, or the nearest one within a few points on its line.
pub fn pick(words: &[Word], origin: [f64; 2], per_point: f64, pointer: [f64; 2]) -> Option<Hit> {
    let screen: Vec<ScreenWord> = words.iter().filter_map(|w| to_screen(w, origin, per_point)).collect();
    let dist = |r: &[f64; 4]| {
        let dx = (r[0] - pointer[0]).max(pointer[0] - (r[0] + r[2])).max(0.0);
        let dy = (r[1] - pointer[1]).max(pointer[1] - (r[1] + r[3])).max(0.0);
        (dx, dy)
    };
    let best = screen.iter().enumerate()
        .map(|(i, w)| (i, dist(&w.rect)))
        .filter(|(_, (dx, dy))| *dx <= 6.0 && *dy <= 3.0)
        .min_by(|a, b| (a.1.0 + a.1.1 * 2.0).total_cmp(&(b.1.0 + b.1.1 * 2.0)))?.0;
    let line_no = screen[best].line;
    let line: Vec<ScreenWord> = screen.iter().filter(|w| w.line == line_no).cloned().collect();
    let index = line.iter().position(|w| *w == screen[best]).unwrap_or(0);
    Some(Hit { query: screen[best].text.clone(), rect: screen[best].rect, line, index })
}

fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '’' && c != '-')
        .filter(|t| !t.is_empty()).map(|t| t.to_lowercase()).collect()
}

/// A selection the pointer is on: macOS looks the selected phrase up when a force click lands
/// inside it. On Linux we can't ask where the selection is drawn, so a selection counts when
/// it's short and the words around the pointer spell it.
pub fn with_selection(hit: Hit, selection: Option<&str>) -> Hit {
    let Some(sel) = selection.map(str::trim).filter(|s| !s.is_empty() && s.chars().count() <= 120 && !s.contains('\n')) else { return hit };
    let want = tokens(sel);
    if want.len() < 2 || want.len() > 8 { return hit; }
    let line: Vec<String> = hit.line.iter().map(|w| w.text.to_lowercase()).collect();
    for start in hit.index.saturating_sub(want.len() - 1)..=hit.index {
        if start + want.len() > line.len() { break; }
        let run = &line[start..start + want.len()];
        let matches = run.iter().zip(&want).enumerate().all(|(j, (a, b))| {
            // The selection may begin or end mid-word.
            if j == 0 { a.ends_with(b.as_str()) } else if j == want.len() - 1 { a.starts_with(b.as_str()) } else { a == b }
        });
        if matches {
            let rect = hit.line[start..start + want.len()].iter().fold(hit.line[start].rect, |r, w| union(r, w.rect));
            return Hit { query: sel.split_whitespace().collect::<Vec<_>>().join(" "), rect, ..hit };
        }
    }
    hit
}

/// The word as macOS shows it while its popover is open: its own pixels repainted in `ink` on
/// a `paper` marker (straight-alpha sRGB, the style's `.marker` colours), `pad` points around
/// it. `rect` is in global logical points; returns RGBA at the shot's resolution and the rect
/// it covers.
pub fn highlight(shot: &Shot, rect: [f64; 4], paper_color: [u8; 4], ink_color: [u8; 4], pad: [f32; 2]) -> Option<(u32, u32, Vec<u8>, [f64; 4])> {
    let pad = [pad[0] as f64, pad[1] as f64];
    let r = [rect[0] - pad[0], rect[1] - pad[1], rect[2] + pad[0] * 2.0, rect[3] + pad[1] * 2.0];
    let to_px = |v: f64, o: f64| ((v - o) * shot.scale).round();
    let x0 = to_px(r[0], shot.origin[0]).max(0.0) as u32;
    let y0 = to_px(r[1], shot.origin[1]).max(0.0) as u32;
    let x1 = (to_px(r[0] + r[2], shot.origin[0]) as u32).min(shot.width);
    let y1 = (to_px(r[1] + r[3], shot.origin[1]) as u32).min(shot.height);
    if x1 <= x0 + 1 || y1 <= y0 + 1 { return None; }
    let (w, h) = (x1 - x0, y1 - y0);
    let bg = background_lum(shot, [x0, y0, x1, y1]);
    let invert = bg < 0.5;
    let lum = |x, y| { let l = shot.lum(x, y); if invert { 1.0 - l } else { l } };
    let paper = if invert { 1.0 - bg } else { bg };
    let mut ink = 1.0f32;
    for y in y0..y1 { for x in x0..x1 { ink = ink.min(lum(x, y)); } }
    let span = (paper - ink).max(0.15);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y1 {
        for x in x0..x1 {
            let t = ((lum(x, y) - ink) / span).clamp(0.0, 1.0);
            for c in 0..4 { out.push((ink_color[c] as f32 + (paper_color[c] as f32 - ink_color[c] as f32) * t).round() as u8); }
        }
    }
    let covered = [shot.origin[0] + x0 as f64 / shot.scale, shot.origin[1] + y0 as f64 / shot.scale, w as f64 / shot.scale, h as f64 / shot.scale];
    Some((w, h, out, covered))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, l: i32, line: u32) -> Word {
        Word { text: text.into(), conf: 90.0, bbox: [l, 30, l + text.len() as i32 * 20, 60], line }
    }

    #[test]
    fn punctuation_is_trimmed_and_the_box_follows() {
        assert_eq!(clean("“Hello,").map(|c| c.0), Some("Hello".into()));
        assert_eq!(clean("don't").map(|c| c.0), Some("don't".into()));
        assert_eq!(clean("—"), None);
        assert_eq!(clean("1984"), None);
        let (_, lead, trail) = clean("(word)").unwrap();
        assert!((lead - 1.0 / 6.0).abs() < 1e-9 && (trail - 1.0 / 6.0).abs() < 1e-9);
    }

    #[test]
    fn the_word_under_the_pointer_wins_and_near_misses_snap() {
        // 3 OCR px per point, capture at (100, 200).
        let words = [word("The", 0, 0), word("quick", 90, 0), word("fox.", 210, 0), word("below", 0, 1)];
        let hit = pick(&words, [100.0, 200.0], 3.0, [100.0 + 120.0 / 3.0, 215.0]).unwrap();
        assert_eq!(hit.query, "quick");
        assert_eq!(hit.line.len(), 3);
        assert_eq!(hit.index, 1);
        // Between words: the nearer one.
        let hit = pick(&words, [100.0, 200.0], 3.0, [100.0 + 200.0 / 3.0, 215.0]).unwrap();
        assert_eq!(hit.query, "fox");
        // Far from every word: nothing.
        assert!(pick(&words, [100.0, 200.0], 3.0, [100.0 + 600.0 / 3.0, 215.0]).is_none());
    }

    #[test]
    fn a_selection_around_the_pointer_becomes_the_query() {
        let words = [word("in", 0, 0), word("New", 60, 0), word("York", 140, 0), word("City", 240, 0)];
        let hit = pick(&words, [0.0, 0.0], 3.0, [150.0 / 3.0, 15.0]).unwrap();
        assert_eq!(hit.query, "York");
        let both = with_selection(hit.clone(), Some("New York"));
        assert_eq!(both.query, "New York");
        assert!(both.rect[0] < hit.rect[0] && both.rect[2] > hit.rect[2]);
        // A selection elsewhere on the page doesn't take over.
        assert_eq!(with_selection(hit.clone(), Some("something else")).query, "York");
        assert_eq!(with_selection(hit, Some("York")).query, "York");
    }

    #[test]
    fn the_highlight_turns_light_and_dark_pages_into_dark_ink_on_yellow() {
        for dark_page in [false, true] {
            let (w, h) = (20u32, 10u32);
            let mut rgba = Vec::new();
            for y in 0..h { for x in 0..w {
                let ink = (8..12).contains(&x) && (3..7).contains(&y);
                let v = if ink != dark_page { 20 } else { 235 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            } }
            let shot = Shot { width: w, height: h, rgba, origin: [0.0, 0.0], scale: 1.0 };
            let (iw, _, px, _) = highlight(&shot, [4.0, 2.0, 12.0, 6.0], [255, 219, 66, 255], [34, 28, 5, 255], [3.0, 1.0]).unwrap();
            let at = |x: u32, y: u32| &px[((y * iw + x) * 4) as usize..][..3];
            let paper = at(0, 0);
            let ink = at(8 - 1, 3 - 1);
            assert!(paper[0] > 200 && paper[2] < 120, "yellow paper {paper:?}");
            assert!(ink[0] < 60, "dark ink {ink:?} (dark page: {dark_page})");
        }
    }
}
