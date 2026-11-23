//! Headless previews of the popover (`tlum --snap out.png --scene light`), through Rune's
//! agent tooling: the same view over a stand-in page, with canned lookups.

use rune::{AppSpec, Color, Cx, El, Pixels, TextStyle, el, text};

use crate::dict::{Definition, Meaning, Part, Sense, Wiki};
use crate::style::Rgba;
use crate::ui::{self, Popover, Shown};

const SIZE: (f32, f32) = (1000.0, 680.0);
const WORD: [f32; 4] = [452.0, 118.0, 96.0, 24.0];

fn definition() -> Definition {
    Definition {
        headword: "serendipity".into(),
        phonetic: Some("/ˌsɛɹ.ən.ˈdɪp.ɪ.ti/".into()),
        meanings: vec![Meaning {
            part_of_speech: "noun".into(),
            senses: vec![
                Sense { text: "An unsought, unintended, and/or unexpected, but fortunate, discovery and/or learning experience that happens by accident.".into(), example: Some("Meeting her old friend on the train was pure serendipity.".into()) },
                Sense { text: "The faculty of making such discoveries.".into(), example: None },
            ],
            synonyms: vec!["chance".into(), "fluke".into(), "luck".into(), "fortune".into(), "happy accident".into()],
            antonyms: vec!["misfortune".into()],
        }],
        note: None,
        url: "https://en.wiktionary.org/wiki/serendipity".into(),
    }
}

fn wiki() -> Wiki {
    Wiki {
        title: "Serendipity".into(),
        description: Some("Unplanned fortunate discovery".into()),
        extract: "Serendipity is an unplanned fortunate discovery. The term was coined by Horace Walpole in 1754. In a letter he wrote to his friend Horace Mann, Walpole explained an unexpected discovery he had made about a lost painting by reference to a Persian fairy tale, The Three Princes of Serendip.".into(),
        url: "https://en.wikipedia.org/wiki/Serendipity".into(),
        thumbnail: None,
        disambiguation: false,
    }
}

/// A plain marker in each scheme's `.marker` colour (the preview draws the word itself).
fn marker() -> (Pixels, Pixels) {
    let themes = crate::style::themes();
    let solid = |t: &crate::style::Theme| { let p = Pixels::new(); p.set(4, 4, t.marker.background.0.repeat(16)); p };
    (solid(&themes.0), solid(&themes.1))
}

fn shown(word: [f32; 4], definition: Part<Definition>, wiki: Part<Wiki>) -> Shown {
    Shown {
        generation: 1, query: "serendipity".into(), word, marker: Some(marker()), screen: [SIZE.0, SIZE.1],
        offset: Some([0.0, 0.0]), area: None, pointer: [word[0] + 20.0, word[1] + 10.0], shown_at: std::time::Instant::now(),
        definition, wiki, thumbnail: None,
    }
}

pub(crate) fn init() -> Popover {
    Popover { shown: Some(shown(WORD, Part::Found(definition()), Part::Found(wiki()))), force_dark: Some(true), ..Popover::default() }
}

const PAGE: TextStyle = TextStyle::new(17.0, 400.0).lh(28.0);

fn view(s: &Popover, cx: &mut Cx<Popover>) -> El {
    let dark = s.force_dark.unwrap_or(true);
    let rgb = |r, g, b| Rgba([r, g, b, 255]).color();
    let (bg, ink) = if dark { (rgb(30, 30, 32), rgb(220, 220, 224)) } else { (Color::WHITE, rgb(28, 28, 30)) };
    let word = s.shown.as_ref().map_or(WORD, |v| v.word);
    // The sentence flows around the marked word, as on a real page.
    let y = word[1] - 3.0;
    let before = text("Few discoveries are planned; most are").font(PAGE).color(ink).single_line()
        .pin(None, Some(y), Some(SIZE.0 - word[0] + 1.0), None);
    let after = text("and luck.").font(PAGE).color(ink).single_line().pin(Some(word[0] + word[2] + 5.0), Some(y), None, None);
    let label = text("serendipity").font(PAGE).color({ let m = crate::style::theme(dark).marker; if m.hidden { ink } else { m.ink.color() } }).single_line().pin(Some(word[0] + 4.0), Some(y), None, None);
    el().w_full().h_full().fill(bg).child(before).child(after).child(ui::view(s, cx).pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))).child(label)
}

pub fn spec() -> AppSpec<Popover> {
    AppSpec {
        scenes: &[
            ("light", |s| s.force_dark = Some(false)),
            ("loading", |s| if let Some(v) = &mut s.shown { v.definition = Part::Loading; v.wiki = Part::Loading; }),
            ("missing", |s| if let Some(v) = &mut s.shown { v.definition = Part::Missing; v.wiki = Part::Missing; }),
            ("offline", |s| if let Some(v) = &mut s.shown { v.definition = Part::Failed("timed out".into()); v.wiki = Part::Failed("timed out".into()); }),
            ("above", |s| s.shown = Some(shown([452.0, 600.0, 96.0, 24.0], Part::Found(definition()), Part::Found(wiki())))),
            ("edge", |s| s.shown = Some(shown([452.0, 118.0, 96.0, 24.0], Part::Found(definition()), Part::Missing))),
        ],
        ..AppSpec::new("tlum", SIZE, init, view)
    }
}

pub fn run() { rune::run(spec()); }
