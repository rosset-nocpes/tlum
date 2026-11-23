//! What the popover shows: a dictionary entry with its thesaurus, and a Wikipedia summary,
//! fetched in parallel. Sources (all free, no key):
//!
//! - Free Dictionary API (`api.dictionaryapi.dev`, Wiktionary data): pronunciation, parts of
//!   speech, senses, examples, synonyms and antonyms;
//! - Wiktionary's REST definitions, for words the first doesn't know (inflections, other
//!   languages);
//! - Wikipedia's REST page summary, with its thumbnail.
//!
//! Only the looked-up word or phrase leaves the machine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sense {
    pub text: String,
    pub example: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meaning {
    pub part_of_speech: String,
    pub senses: Vec<Sense>,
    pub synonyms: Vec<String>,
    pub antonyms: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Definition {
    pub headword: String,
    pub phonetic: Option<String>,
    pub meanings: Vec<Meaning>,
    /// "Inflection of …" when the word was found under another form.
    pub note: Option<String>,
    pub url: String,
    /// The dictionary that answered, when it isn't the section's own: an offline one standing
    /// in for the online dictionary. The section's header names it in place of its link.
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wiki {
    pub title: String,
    pub description: Option<String>,
    pub extract: String,
    pub url: String,
    pub thumbnail: Option<String>,
    pub disambiguation: bool,
}

/// One section's progress.
#[derive(Clone, Debug, PartialEq)]
pub enum Part<T> { Loading, Found(T), Missing, Failed(String) }

impl<T> Part<T> {
    pub fn found(&self) -> Option<&T> { if let Part::Found(t) = self { Some(t) } else { None } }
    pub fn settled(&self) -> bool { !matches!(self, Part::Loading) }
}

const UA: &str = concat!("tlum/", env!("CARGO_PKG_VERSION"), " (desktop dictionary popover)");

/// One agent for every lookup, so its TLS setup is made once and its pool keeps connections to
/// the dictionary and Wikipedia open between lookups that come close together.
fn agent() -> ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .http_status_as_error(false)
        .user_agent(UA)
        .build()
        .into()).clone()
}

fn get_json<T: for<'de> Deserialize<'de>>(agent: &ureq::Agent, url: &str) -> Result<Option<T>, String> {
    let mut resp = agent.get(url).header("Accept", "application/json").call().map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        200 => resp.body_mut().with_config().limit(4 << 20).read_json::<T>().map(Some).map_err(|e| e.to_string()),
        404 => Ok(None),
        s => Err(format!("HTTP {s}")),
    }
}

pub fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = agent().get(url).call().map_err(|e| e.to_string())?;
    if resp.status().as_u16() != 200 { return Err(format!("HTTP {}", resp.status())); }
    resp.body_mut().with_config().limit(8 << 20).read_to_vec().map_err(|e| e.to_string())
}

fn enc(s: &str) -> String { urlencoding::encode(s).into_owned() }

// --- Free Dictionary API -------------------------------------------------------------------

#[derive(Deserialize)]
struct FdEntry {
    word: String,
    #[serde(default)] phonetic: Option<String>,
    #[serde(default)] phonetics: Vec<FdPhonetic>,
    #[serde(default)] meanings: Vec<FdMeaning>,
    #[serde(default, rename = "sourceUrls")] source_urls: Vec<String>,
}
#[derive(Deserialize)]
struct FdPhonetic { #[serde(default)] text: Option<String> }
#[derive(Deserialize)]
struct FdMeaning {
    #[serde(rename = "partOfSpeech")] part_of_speech: String,
    #[serde(default)] definitions: Vec<FdDef>,
    #[serde(default)] synonyms: Vec<String>,
    #[serde(default)] antonyms: Vec<String>,
}
#[derive(Deserialize)]
struct FdDef {
    definition: String,
    #[serde(default)] example: Option<String>,
    #[serde(default)] synonyms: Vec<String>,
    #[serde(default)] antonyms: Vec<String>,
}

fn push_unique(into: &mut Vec<String>, more: impl IntoIterator<Item = String>) {
    for m in more { if !into.iter().any(|x| x.eq_ignore_ascii_case(&m)) { into.push(m); } }
}

fn from_free_dictionary(entries: Vec<FdEntry>) -> Option<Definition> {
    let first = entries.first()?;
    let headword = first.word.clone();
    let phonetic = entries.iter().find_map(|e| e.phonetic.clone().filter(|p| !p.is_empty()))
        .or_else(|| entries.iter().flat_map(|e| &e.phonetics).find_map(|p| p.text.clone().filter(|t| !t.is_empty())));
    let url = first.source_urls.first().cloned().unwrap_or_else(|| format!("https://en.wiktionary.org/wiki/{}", enc(&headword)));
    // Entries split by etymology; macOS lists one entry per part of speech, so merge.
    let mut meanings: Vec<Meaning> = Vec::new();
    for m in entries.into_iter().flat_map(|e| e.meanings) {
        let idx = match meanings.iter().position(|x| x.part_of_speech == m.part_of_speech) {
            Some(i) => i,
            None => { meanings.push(Meaning { part_of_speech: m.part_of_speech.clone(), ..Meaning::default() }); meanings.len() - 1 }
        };
        let into = &mut meanings[idx];
        push_unique(&mut into.synonyms, m.synonyms);
        push_unique(&mut into.antonyms, m.antonyms);
        for d in m.definitions {
            push_unique(&mut into.synonyms, d.synonyms);
            push_unique(&mut into.antonyms, d.antonyms);
            into.senses.push(Sense { text: d.definition, example: d.example.filter(|e| !e.is_empty()) });
        }
    }
    meanings.retain(|m| !m.senses.is_empty());
    (!meanings.is_empty()).then_some(Definition { headword, phonetic, meanings, note: None, url, source: None })
}

// --- Wiktionary REST ------------------------------------------------------------------------

#[derive(Deserialize)]
struct WtUsage {
    #[serde(rename = "partOfSpeech")] part_of_speech: String,
    #[serde(default)] definitions: Vec<WtDef>,
}
#[derive(Deserialize)]
struct WtDef { definition: String, #[serde(default)] examples: Vec<String> }

/// Wiktionary's definitions are HTML fragments.
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c { '<' => depth += 1, '>' => depth = depth.saturating_sub(1), _ if depth == 0 => out.push(c), _ => {} }
    }
    let out = out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn from_wiktionary(word: &str, lang: &str, mut by_lang: HashMap<String, Vec<WtUsage>>) -> Option<Definition> {
    let usages = by_lang.remove(lang).or_else(|| by_lang.into_values().next())?;
    let meanings: Vec<Meaning> = usages.into_iter().map(|u| Meaning {
        part_of_speech: u.part_of_speech.to_lowercase(),
        senses: u.definitions.into_iter().map(|d| Sense { text: strip_html(&d.definition), example: d.examples.first().map(|e| strip_html(e)) })
            .filter(|s| !s.text.is_empty()).collect(),
        ..Meaning::default()
    }).filter(|m| !m.senses.is_empty()).collect();
    (!meanings.is_empty()).then(|| Definition {
        headword: word.to_string(), phonetic: None, meanings, note: None, source: None,
        url: format!("https://{lang}.wiktionary.org/wiki/{}", enc(word)),
    })
}

/// Forms to try when a word isn't in the dictionary as written: `running` → `run`.
pub fn base_forms(word: &str) -> Vec<String> {
    let w = word.to_lowercase();
    let mut v = vec![];
    let mut add = |s: String| if s.chars().count() >= 2 && s != w && !v.contains(&s) { v.push(s) };
    let strip = |suffix: &str| w.strip_suffix(suffix).map(str::to_string);
    if let Some(s) = strip("'s").or_else(|| strip("’s")) { add(s); }
    if let Some(s) = strip("ies") { add(format!("{s}y")); }
    if let Some(s) = strip("ied") { add(format!("{s}y")); }
    if let Some(s) = strip("es") { add(s); }
    if let Some(s) = strip("s").filter(|_| !w.ends_with("ss")) { add(s); }
    for suffix in ["ing", "ed", "er", "est"] {
        if let Some(s) = strip(suffix) {
            add(format!("{s}e"));
            let b = s.as_bytes();
            // Doubled final consonant: running → run, stopped → stop.
            if b.len() >= 3 && b[b.len() - 1] == b[b.len() - 2] && !b"aeiou".contains(&b[b.len() - 1]) { add(s[..s.len() - 1].to_string()); }
            add(s);
        }
    }
    if let Some(s) = strip("ly") { add(s.clone()); if let Some(t) = s.strip_suffix('i') { add(format!("{t}y")); } }
    v
}

pub fn define(word: &str, lang: &str) -> Part<Definition> {
    let agent = agent();
    let mut failure = None;
    let free = |w: &str| get_json::<Vec<FdEntry>>(&agent, &format!("https://api.dictionaryapi.dev/api/v2/entries/{lang}/{}", enc(w)));
    let candidates: Vec<String> = std::iter::once(word.to_string())
        .chain((word.chars().next().is_some_and(char::is_uppercase)).then(|| word.to_lowercase()))
        .collect();
    if lang == "en" {
        for w in &candidates {
            match free(w) {
                Ok(Some(e)) => if let Some(d) = from_free_dictionary(e) { return Part::Found(d) },
                Ok(None) => {}
                Err(e) => { failure = Some(e); break; }
            }
        }
    }
    // Wiktionary knows inflections and other languages.
    let wt = |w: &str| get_json::<HashMap<String, Vec<WtUsage>>>(&agent, &format!("https://{lang}.wiktionary.org/api/rest_v1/page/definition/{}", enc(w)));
    for w in &candidates {
        match wt(w) {
            Ok(Some(m)) => if let Some(d) = from_wiktionary(w, lang, m) {
                // "present participle of run": look the base word up for a fuller entry.
                if lang == "en" && let Some(base) = form_of(&d) && let Ok(Some(e)) = free(&base) && let Some(mut full) = from_free_dictionary(e) {
                    full.note = Some(format!("{} is {}", word, d.meanings[0].senses[0].text.trim_end_matches('.')));
                    return Part::Found(full);
                }
                return Part::Found(d);
            },
            Ok(None) => {}
            Err(e) => { failure.get_or_insert(e); break; }
        }
    }
    if lang == "en" && failure.is_none() {
        for base in base_forms(word).into_iter().take(5) {
            if let Ok(Some(e)) = free(&base) && let Some(mut d) = from_free_dictionary(e) {
                d.note = Some(format!("from {}", word.to_lowercase()));
                return Part::Found(d);
            }
        }
    }
    match failure { Some(e) => Part::Failed(e), None => Part::Missing }
}

/// The base word in a Wiktionary "… of X" sense.
fn form_of(d: &Definition) -> Option<String> {
    let first = &d.meanings.first()?.senses.first()?.text;
    let (_, rest) = first.rsplit_once(" of ")?;
    let base = rest.trim().trim_end_matches(['.', ';', ':']).split([';', ',', '(']).next()?.trim().to_string();
    (!base.is_empty() && base.split_whitespace().count() <= 2 && first.split_whitespace().count() <= 8).then_some(base)
}

// --- Wikipedia --------------------------------------------------------------------------------

#[derive(Deserialize)]
struct WpSummary {
    #[serde(default, rename = "type")] kind: String,
    title: String,
    #[serde(default)] description: Option<String>,
    #[serde(default)] extract: String,
    #[serde(default)] thumbnail: Option<WpThumb>,
    #[serde(default)] content_urls: Option<WpUrls>,
}
#[derive(Deserialize)]
struct WpThumb { source: String }
#[derive(Deserialize)]
struct WpUrls { desktop: WpUrl }
#[derive(Deserialize)]
struct WpUrl { page: String }

pub fn wikipedia(query: &str, lang: &str) -> Part<Wiki> {
    let agent = agent();
    let title = |q: &str| {
        let mut c = q.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default().replace(' ', "_")
    };
    match get_json::<WpSummary>(&agent, &format!("https://{lang}.wikipedia.org/api/rest_v1/page/summary/{}?redirect=true", enc(&title(query)))) {
        Ok(Some(s)) if !s.extract.trim().is_empty() => Part::Found(Wiki {
            url: s.content_urls.map(|u| u.desktop.page).unwrap_or_else(|| format!("https://{lang}.wikipedia.org/wiki/{}", enc(&title(query)))),
            disambiguation: s.kind == "disambiguation",
            title: s.title, description: s.description, extract: s.extract,
            thumbnail: s.thumbnail.map(|t| t.source),
        }),
        Ok(_) => Part::Missing,
        Err(e) => Part::Failed(e),
    }
}

// --- sections ---------------------------------------------------------------------------------

/// What a section shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry { Definition(Definition), Wiki(Wiki) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Dictionary, Thesaurus, Wikipedia }

/// One source's part of the popover, in the order the settings give.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    pub part: Part<Entry>,
}

impl Section {
    pub fn wiki(&self) -> Option<&Wiki> { match self.part.found() { Some(Entry::Wiki(w)) => Some(w), _ => None } }
    /// The page the section links to.
    pub fn url(&self) -> Option<&str> {
        match self.part.found()? {
            Entry::Definition(d) => (!d.url.is_empty()).then_some(d.url.as_str()),
            Entry::Wiki(w) => Some(&w.url),
        }
    }
}

/// Empty sections for these sources, each loading.
pub fn sections(ids: &[String]) -> Vec<Section> {
    let installed = crate::library::installed();
    ids.iter().filter_map(|id| {
        let (title, kind) = match id.as_str() {
            "dictionary" => ("Dictionary".to_string(), Kind::Dictionary),
            "thesaurus" => ("Thesaurus".to_string(), Kind::Thesaurus),
            "wikipedia" => ("Wikipedia".to_string(), Kind::Wikipedia),
            other => (installed.iter().find(|d| d.id == other)?.name.clone(), Kind::Dictionary),
        };
        Some(Section { id: id.clone(), title, kind, part: Part::Loading })
    }).collect()
}

/// One lookup's sources, fetched in parallel. The online dictionary is fetched once, for the
/// dictionary and the thesaurus both.
pub struct Lookup {
    pub query: String,
    pub language: String,
    pub offline: bool,
    /// The sources this lookup shows: an offline dictionary with a section of its own doesn't
    /// stand in for the online one too.
    shown: Vec<String>,
    online: Mutex<Option<Part<Definition>>>,
}

impl Lookup {
    pub fn new(query: &str, language: &str, offline: bool, shown: &[String]) -> Lookup {
        Lookup { query: query.to_string(), language: language.to_string(), offline, shown: shown.to_vec(), online: Mutex::new(None) }
    }

    /// The online dictionary's stand-in: the first installed dictionary of the lookup
    /// language that has the word (WordNet first, for English), among those not shown anyway.
    fn offline_definition(&self) -> Option<Definition> {
        let lang = crate::library::iso3(&self.language);
        let mut candidates: Vec<crate::library::Installed> = crate::library::installed().into_iter()
            .filter(|d| !self.shown.contains(&d.id) && d.language.as_deref() == Some(lang))
            .collect();
        candidates.sort_by_key(|d| d.id != "wordnet");
        candidates.into_iter().find_map(|d| match crate::library::define(&d.id, &self.query, lang == "eng") {
            Part::Found(mut def) => { def.source = Some(d.name); Some(def) }
            _ => None,
        })
    }

    fn online(&self) -> Part<Definition> {
        let mut o = self.online.lock().unwrap();
        o.get_or_insert_with(|| define(&self.query, &self.language)).clone()
    }

    /// What source `id` has for the query.
    pub fn fetch(&self, id: &str) -> Part<Entry> {
        let english = |lang: Option<&str>| lang.map_or(self.query.is_ascii(), |l| l == "eng");
        match id {
            // Online, and an installed dictionary when the network fails, the word isn't there,
            // or tlum is offline.
            "dictionary" => {
                let online = if self.offline { Part::Missing } else { self.online() };
                match online {
                    Part::Found(d) => Part::Found(Entry::Definition(d)),
                    other => match self.offline_definition() {
                        Some(d) => Part::Found(Entry::Definition(d)),
                        None => to_entry(other),
                    },
                }
            }
            "wikipedia" if self.offline => Part::Missing,
            "wikipedia" => match wikipedia(&self.query, &self.language) {
                Part::Found(w) => Part::Found(Entry::Wiki(w)),
                Part::Loading => Part::Loading,
                Part::Missing => Part::Missing,
                Part::Failed(e) => Part::Failed(e),
            },
            // Synonyms from the online dictionary, else from WordNet when it's installed.
            "thesaurus" => {
                let has = |d: &Definition| d.meanings.iter().any(|m| !m.synonyms.is_empty() || !m.antonyms.is_empty());
                let online = if self.offline || self.language != "en" { Part::Missing } else { self.online() };
                if let Part::Found(d) = &online && has(d) { return Part::Found(Entry::Definition(d.clone())); }
                if crate::library::is_installed("wordnet") && english(Some("eng")) && let Part::Found(d) = crate::library::define("wordnet", &self.query, true) && has(&d) {
                    return Part::Found(Entry::Definition(d));
                }
                match online { Part::Failed(e) => Part::Failed(e), _ => Part::Missing }
            }
            other => {
                let lang = crate::library::installed().into_iter().find(|d| d.id == other).and_then(|d| d.language);
                to_entry(crate::library::define(other, &self.query, english(lang.as_deref())))
            }
        }
    }
}

fn to_entry(p: Part<Definition>) -> Part<Entry> {
    match p { Part::Found(d) => Part::Found(Entry::Definition(d)), Part::Loading => Part::Loading, Part::Missing => Part::Missing, Part::Failed(e) => Part::Failed(e) }
}

/// Recent lookups, so looking the same word up again is instant. Keyed by the query and the
/// sources asked, so changing the settings asks again.
#[derive(Clone, Default)]
pub struct Cache(Arc<Mutex<Vec<(String, Vec<Section>)>>>);

impl Cache {
    pub fn get(&self, key: &str) -> Option<Vec<Section>> {
        self.0.lock().unwrap().iter().find(|e| e.0 == key).map(|e| e.1.clone())
    }
    pub fn put(&self, key: &str, sections: &[Section]) {
        // Failures (offline) aren't worth keeping, nor lookups still under way.
        if sections.iter().any(|s| matches!(s.part, Part::Failed(_) | Part::Loading)) { return; }
        let mut v = self.0.lock().unwrap();
        v.retain(|e| e.0 != key);
        v.push((key.to_string(), sections.to_vec()));
        if v.len() > 64 { v.remove(0); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflections_have_base_forms() {
        assert!(base_forms("running").contains(&"run".to_string()));
        assert!(base_forms("stopped").contains(&"stop".to_string()));
        assert!(base_forms("cities").contains(&"city".to_string()));
        assert!(base_forms("baked").contains(&"bake".to_string()));
        assert!(base_forms("happily").contains(&"happy".to_string()));
        assert!(base_forms("dog's").contains(&"dog".to_string()));
        assert!(!base_forms("glass").contains(&"glas".to_string()));
    }

    #[test]
    fn html_is_stripped() {
        assert_eq!(strip_html("<span>present participle of <a href=\"x\">run</a></span>&nbsp;"), "present participle of run");
    }

    #[test]
    fn free_dictionary_entries_merge_by_part_of_speech() {
        let json = r#"[{"word":"word","phonetic":"/wɜːd/","meanings":[{"partOfSpeech":"noun","definitions":[{"definition":"A unit of language.","example":"a word","synonyms":["term"]}],"synonyms":["vocable"]}],"sourceUrls":["https://en.wiktionary.org/wiki/word"]},
                       {"word":"word","meanings":[{"partOfSpeech":"noun","definitions":[{"definition":"A promise."}]},{"partOfSpeech":"verb","definitions":[{"definition":"To say in words."}]}]}]"#;
        let d = from_free_dictionary(serde_json::from_str(json).unwrap()).unwrap();
        assert_eq!(d.phonetic.as_deref(), Some("/wɜːd/"));
        assert_eq!(d.meanings.len(), 2);
        assert_eq!(d.meanings[0].senses.len(), 2);
        assert_eq!(d.meanings[0].synonyms, vec!["vocable", "term"]);
    }

    #[test]
    fn wiktionary_form_of_finds_the_base() {
        let d = Definition { meanings: vec![Meaning { senses: vec![Sense { text: "present participle and gerund of run".into(), example: None }], ..Meaning::default() }], ..Definition::default() };
        assert_eq!(form_of(&d).as_deref(), Some("run"));
    }
}
