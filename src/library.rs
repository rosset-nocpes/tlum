//! Dictionaries on disk: what's installed, what can be downloaded, and installing, adding and
//! removing them, as macOS's Dictionary settings do.
//!
//! Each lives in its own folder under `$XDG_DATA_HOME/tlum/dictionaries/<id>/` with a
//! `tlum.json` describing it. Two kinds:
//!
//! - WordNet (`wordnet`): Princeton's English lexical database, an offline dictionary and
//!   thesaurus;
//! - StarDict: FreeDict's bilingual dictionaries (`freedict-eng-deu`, …), downloaded from its
//!   catalogue, and any StarDict dictionary the person adds (`stardict-…`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::dict::{Definition, Part};
use crate::stardict::StarDict;
use crate::wordnet::WordNet;

const WORDNET_URL: &str = "https://wordnetcode.princeton.edu/wn3.1.dict.tar.gz";
const WORDNET_SIZE: u64 = 16_358_468;
const FREEDICT_DATABASE: &str = "https://freedict.org/freedict-database.json";

fn home() -> PathBuf { PathBuf::from(std::env::var_os("HOME").unwrap_or_default()) }

pub fn dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("tlum/dictionaries")
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".cache")).join("tlum")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind { WordNet, StarDict }

/// `tlum.json` in a dictionary's folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Installed {
    #[serde(skip)]
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// "English → German · 460,315 entries".
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub version: String,
    /// The language its headwords are in (ISO 639-3), when known: inflections are tried for English.
    #[serde(default)]
    pub language: Option<String>,
    #[serde(skip)]
    pub size: u64,
}

fn folder_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
}

/// Every installed dictionary, by name.
pub fn installed() -> Vec<Installed> {
    let mut v: Vec<Installed> = std::fs::read_dir(dir()).into_iter().flatten().flatten().filter_map(|e| {
        let id = e.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') { return None; }
        let meta = std::fs::read_to_string(e.path().join("tlum.json")).ok()?;
        let mut d: Installed = serde_json::from_str(&meta).ok()?;
        d.id = id;
        d.size = folder_size(&e.path());
        Some(d)
    }).collect();
    v.sort_by_key(|d| d.name.to_lowercase());
    v
}

pub fn is_installed(id: &str) -> bool { dir().join(id).join("tlum.json").exists() }

// --- lookups ----------------------------------------------------------------------------------

enum Book { WordNet(WordNet), StarDict(StarDict) }

static OPEN: Mutex<Vec<(String, Arc<Result<Book, String>>)>> = Mutex::new(Vec::new());

fn open(id: &str) -> Arc<Result<Book, String>> {
    let mut all = OPEN.lock().unwrap();
    if let Some((_, b)) = all.iter().find(|(i, _)| i == id) { return b.clone(); }
    let path = dir().join(id);
    let kind = std::fs::read_to_string(path.join("tlum.json")).ok().and_then(|m| serde_json::from_str::<Installed>(&m).ok()).map(|m| m.kind);
    let book = Arc::new(match kind {
        Some(Kind::WordNet) => WordNet::open(&path).map(Book::WordNet),
        Some(Kind::StarDict) => StarDict::open(&path).map(Book::StarDict),
        None => Err(format!("{id} isn't installed")),
    });
    all.push((id.to_string(), book.clone()));
    book
}

/// Forgets an open dictionary, after it's removed or replaced.
fn close(id: &str) { OPEN.lock().unwrap().retain(|(i, _)| i != id); }

/// Loads the dictionaries ahead of the first lookup.
pub fn warm(ids: &[String]) { for id in ids { open(id); } }

/// Looks `word` up in an installed dictionary.
pub fn define(id: &str, word: &str, english: bool) -> Part<Definition> {
    let book = open(id);
    let found = match &*book {
        Ok(Book::WordNet(w)) => w.define(word),
        Ok(Book::StarDict(s)) => s.define(word).or_else(|| {
            // English headwords: try the base form of an inflection (`running` → `run`).
            if !english { return None; }
            crate::dict::base_forms(word).iter().take(5).find_map(|b| s.define(b)).map(|mut d| { d.note = Some(format!("from {}", word.to_lowercase())); d })
        }),
        Err(e) => return Part::Failed(e.clone()),
    };
    found.map_or(Part::Missing, Part::Found)
}

// --- the catalogue ------------------------------------------------------------------------------

/// A dictionary that can be downloaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub url: String,
    pub size: u64,
    pub kind: Kind,
    pub version: String,
    /// Source and target languages (ISO 639-3), for search and for inflections.
    pub languages: Vec<String>,
}

impl Offer {
    /// Whether `query` (a language or a name) matches.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        q.is_empty() || self.name.to_lowercase().contains(&q) || self.detail.to_lowercase().contains(&q) || self.languages.contains(&q)
    }
}

pub fn wordnet_offer() -> Offer {
    Offer {
        id: "wordnet".into(), name: "WordNet (English)".into(),
        detail: "Offline English definitions and thesaurus · Princeton University".into(),
        url: WORDNET_URL.into(), size: WORDNET_SIZE, kind: Kind::WordNet, version: "3.1".into(), languages: vec!["eng".into()],
    }
}

#[derive(Deserialize)]
struct FdDict {
    #[serde(default)] name: Option<String>,
    #[serde(default)] headwords: Option<String>,
    #[serde(default)] releases: Vec<FdRelease>,
}
#[derive(Deserialize)]
struct FdRelease {
    #[serde(rename = "URL")] url: String,
    platform: String,
    #[serde(default)] size: String,
    #[serde(default)] version: String,
    #[serde(default)] date: String,
}

/// The ISO 639-3 code for a Wikipedia language code (`en` → `eng`), as dictionaries are marked.
pub fn iso3(code: &str) -> &str {
    match code {
        "af" => "afr", "ar" => "ara", "bg" => "bul", "br" => "bre", "ca" => "cat", "cs" => "ces", "cy" => "cym", "da" => "dan",
        "de" => "deu", "el" => "ell", "en" => "eng", "eo" => "epo", "es" => "spa", "fi" => "fin", "fr" => "fra", "ga" => "gle",
        "gd" => "gla", "he" => "heb", "hi" => "hin", "hr" => "hrv", "hu" => "hun", "id" => "ind", "is" => "isl", "it" => "ita",
        "ja" => "jpn", "ko" => "kor", "la" => "lat", "lt" => "lit", "mk" => "mkd", "nb" => "nob", "nl" => "nld", "nn" => "nno",
        "no" => "nor", "pl" => "pol", "pt" => "por", "ro" => "ron", "ru" => "rus", "sk" => "slk", "sl" => "slv", "sr" => "srp",
        "sv" => "swe", "sw" => "swh", "tr" => "tur", "uk" => "ukr", "zh" => "zho",
        other => other,
    }
}

/// English names of the languages FreeDict covers (ISO 639-3).
pub fn language_name(code: &str) -> &str {
    match code {
        "afr" => "Afrikaans", "ara" => "Arabic", "ast" => "Asturian", "bre" => "Breton", "bul" => "Bulgarian",
        "cat" => "Catalan", "ces" => "Czech", "ckb" => "Central Kurdish", "cym" => "Welsh", "dan" => "Danish",
        "deu" => "German", "ell" => "Greek", "eng" => "English", "epo" => "Esperanto", "fin" => "Finnish",
        "fra" => "French", "gla" => "Scottish Gaelic", "gle" => "Irish", "hin" => "Hindi", "hrv" => "Croatian",
        "hun" => "Hungarian", "ind" => "Indonesian", "isl" => "Icelandic", "ita" => "Italian", "jpn" => "Japanese",
        "kha" => "Khasi", "kmr" => "Northern Kurdish", "kur" => "Kurdish", "lat" => "Latin", "lit" => "Lithuanian",
        "mkd" => "Macedonian", "mlg" => "Malagasy", "nld" => "Dutch", "nno" => "Norwegian Nynorsk", "nob" => "Norwegian Bokmål",
        "nor" => "Norwegian", "oci" => "Occitan", "pol" => "Polish", "por" => "Portuguese", "rom" => "Romani",
        "ron" => "Romanian", "rus" => "Russian", "san" => "Sanskrit", "slk" => "Slovak", "slv" => "Slovenian",
        "spa" => "Spanish", "srp" => "Serbian", "swe" => "Swedish", "swh" => "Swahili", "tur" => "Turkish",
        "ukr" => "Ukrainian", "wol" => "Wolof", "zho" => "Chinese",
        other => other,
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) { out.push(','); }
        out.push(c);
    }
    out
}

fn parse_freedict(json: &str) -> Result<Vec<Offer>, String> {
    let all: Vec<FdDict> = serde_json::from_str(json).map_err(|e| format!("FreeDict's catalogue: {e}"))?;
    let mut v: Vec<Offer> = all.into_iter().filter_map(|d| {
        let name = d.name?;
        let (from, to) = name.split_once('-')?;
        let r = d.releases.into_iter().filter(|r| r.platform == "stardict").max_by(|a, b| a.date.cmp(&b.date))?;
        let entries = d.headwords.and_then(|h| h.parse::<u64>().ok()).map(|n| format!(" · {} entries", thousands(n))).unwrap_or_default();
        Some(Offer {
            id: format!("freedict-{name}"),
            name: format!("{} → {}", language_name(from), language_name(to)),
            detail: format!("FreeDict {}{entries}", r.version),
            url: r.url, size: r.size.parse().unwrap_or(0), kind: Kind::StarDict, version: r.version,
            languages: vec![from.to_string(), to.to_string()],
        })
    }).collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(timeout)
        .http_status_as_error(false)
        .user_agent(concat!("tlum/", env!("CARGO_PKG_VERSION"), " (desktop dictionary popover)"))
        .build()
        .into()
}

/// What can be downloaded: WordNet, then FreeDict's dictionaries. The FreeDict catalogue is
/// fetched once a week (or when `refresh`), and kept for offline use.
pub fn catalog(refresh: bool) -> Result<Vec<Offer>, String> {
    let cached = cache_dir().join("freedict-database.json");
    let fresh = std::fs::metadata(&cached).and_then(|m| m.modified()).ok()
        .and_then(|t| t.elapsed().ok()).is_some_and(|age| age < Duration::from_secs(7 * 24 * 3600));
    let json = if fresh && !refresh {
        std::fs::read_to_string(&cached).map_err(|e| e.to_string())?
    } else {
        let fetched = (|| -> Result<String, String> {
            let mut resp = agent(Some(Duration::from_secs(30))).get(FREEDICT_DATABASE).call().map_err(|e| e.to_string())?;
            if resp.status().as_u16() != 200 { return Err(format!("HTTP {}", resp.status())); }
            resp.body_mut().with_config().limit(16 << 20).read_to_string().map_err(|e| e.to_string())
        })();
        match fetched {
            Ok(j) => {
                let _ = std::fs::create_dir_all(cache_dir());
                let _ = std::fs::write(&cached, &j);
                j
            }
            // Offline: last week's list is better than none.
            Err(e) => std::fs::read_to_string(&cached).map_err(|_| format!("couldn't fetch FreeDict's catalogue ({e})"))?,
        }
    };
    let mut offers = vec![wordnet_offer()];
    offers.extend(parse_freedict(&json)?);
    Ok(offers)
}

// --- installing -----------------------------------------------------------------------------------

/// Downloads `url` to `to`, reporting (bytes so far, total) as it goes. `cancel` stops it.
fn download(url: &str, to: &Path, expected: u64, progress: &dyn Fn(u64, u64), cancel: &dyn Fn() -> bool) -> Result<(), String> {
    let mut resp = agent(None).get(url).call().map_err(|e| e.to_string())?;
    if resp.status().as_u16() != 200 { return Err(format!("HTTP {} from {url}", resp.status())); }
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(expected);
    let mut reader = resp.body_mut().with_config().limit(2 << 30).reader();
    let mut out = std::fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut buf = vec![0u8; 64 << 10];
    let mut got = 0u64;
    loop {
        if cancel() { return Err("cancelled".into()); }
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 { break; }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        progress(got, total);
    }
    Ok(())
}

fn untar(archive: &Path, into: &Path) -> Result<(), String> {
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    // tar recognises gzip and xz by itself when reading a file.
    let out = std::process::Command::new("tar").arg("-xf").arg(archive).arg("-C").arg(into).output()
        .map_err(|e| format!("tar: {e}"))?;
    if !out.status.success() { return Err(format!("tar: {}", String::from_utf8_lossy(&out.stderr).trim())); }
    Ok(())
}

fn find_file(root: &Path, pred: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() { if let Some(f) = find_file(&p, pred) { return Some(f); } } else if pred(&p) { return Some(p); }
    }
    None
}

fn gunzip(from: &Path, to: &Path) -> Result<(), String> {
    let f = std::fs::File::open(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let mut d = flate2::read::MultiGzDecoder::new(std::io::BufReader::new(f));
    let mut out = std::fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    std::io::copy(&mut d, &mut out).map_err(|e| format!("{}: {e}", from.display()))?;
    Ok(())
}

/// Copies a StarDict dictionary (the folder holding `ifo`) into `into`, its index and data
/// unpacked. Returns its `.ifo` settings.
fn copy_stardict(ifo: &Path, into: &Path) -> Result<Vec<(String, String)>, String> {
    let info = crate::stardict::read_ifo(ifo)?;
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    let stem = ifo.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let from = ifo.parent().unwrap_or(Path::new("."));
    std::fs::copy(ifo, into.join(format!("{stem}.ifo"))).map_err(|e| e.to_string())?;
    for (plain, packed) in [("idx", "idx.gz"), ("dict", "dict.dz"), ("syn", "syn.gz")] {
        let (p, z) = (from.join(format!("{stem}.{plain}")), from.join(format!("{stem}.{packed}")));
        let to = into.join(format!("{stem}.{plain}"));
        if p.exists() {
            std::fs::copy(&p, &to).map_err(|e| e.to_string())?;
        } else if z.exists() {
            gunzip(&z, &to)?;
        } else if plain != "syn" {
            return Err(format!("{} has no .{plain} file", ifo.display()));
        }
    }
    Ok(info)
}

fn write_meta(into: &Path, meta: &Installed) -> Result<(), String> {
    std::fs::write(into.join("tlum.json"), serde_json::to_string_pretty(meta).unwrap()).map_err(|e| e.to_string())
}

/// Moves a finished folder into place, replacing an older copy.
fn settle(staging: &Path, id: &str) -> Result<(), String> {
    let target = dir().join(id);
    close(id);
    if target.exists() { std::fs::remove_dir_all(&target).map_err(|e| e.to_string())?; }
    std::fs::rename(staging, &target).map_err(|e| e.to_string())
}

/// Downloads and installs `offer`.
pub fn install(offer: &Offer, progress: &dyn Fn(u64, u64), cancel: &dyn Fn() -> bool) -> Result<(), String> {
    let base = dir();
    std::fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
    let work = base.join(format!(".download-{}", offer.id));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        let archive = work.join("archive");
        download(&offer.url, &archive, offer.size, progress, cancel)?;
        let unpacked = work.join("unpacked");
        untar(&archive, &unpacked)?;
        let staging = work.join("dict");
        std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        let meta = match offer.kind {
            Kind::WordNet => {
                let index = find_file(&unpacked, &|p| p.file_name().is_some_and(|n| n == "index.noun")).ok_or("no WordNet database in the download")?;
                let from = index.parent().unwrap();
                for f in crate::wordnet::FILES {
                    std::fs::copy(from.join(f), staging.join(f)).map_err(|e| format!("{f}: {e}"))?;
                }
                for f in ["LICENSE", "README"] { let _ = std::fs::copy(from.join(f), staging.join(f)); }
                Installed { id: offer.id.clone(), name: offer.name.clone(), kind: Kind::WordNet, detail: offer.detail.clone(), version: offer.version.clone(), language: Some("eng".into()), size: 0 }
            }
            Kind::StarDict => {
                let ifo = find_file(&unpacked, &|p| p.extension().is_some_and(|x| x == "ifo")).ok_or("no StarDict dictionary in the download")?;
                copy_stardict(&ifo, &staging)?;
                Installed {
                    id: offer.id.clone(), name: offer.name.clone(), kind: Kind::StarDict, detail: offer.detail.clone(),
                    version: offer.version.clone(), language: offer.languages.first().cloned(), size: 0,
                }
            }
        };
        write_meta(&staging, &meta)?;
        settle(&staging, &offer.id)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn slug(s: &str) -> String {
    let mut out: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    while out.contains("--") { out = out.replace("--", "-"); }
    out.trim_matches('-').chars().take(48).collect()
}

/// Adds a StarDict dictionary the person has: its `.ifo` file, its folder, or an archive of it.
/// Returns its id.
pub fn add_local(path: &Path) -> Result<String, String> {
    let base = dir();
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let work = base.join(format!(".add-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let result = (|| {
        let is_ifo = |p: &Path| p.extension().is_some_and(|x| x == "ifo");
        let ifo = if path.is_dir() {
            find_file(path, &is_ifo).ok_or_else(|| format!("no StarDict .ifo file in {}", path.display()))?
        } else if is_ifo(path) {
            path.to_path_buf()
        } else {
            untar(path, &work.join("unpacked")).map_err(|_| format!("{} isn't a StarDict dictionary (.ifo), a folder or an archive of one", path.display()))?;
            find_file(&work.join("unpacked"), &is_ifo).ok_or("no StarDict .ifo file in that archive")?
        };
        let staging = work.join("dict");
        let info = copy_stardict(&ifo, &staging)?;
        let get = |k: &str| info.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        let name = get("bookname").unwrap_or_else(|| ifo.file_stem().unwrap_or_default().to_string_lossy().into_owned());
        let id = format!("stardict-{}", slug(&name));
        let entries = get("wordcount").and_then(|n| n.parse::<u64>().ok()).map(|n| format!("{} entries", thousands(n))).unwrap_or_default();
        let detail = ["StarDict".to_string(), entries].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        write_meta(&staging, &Installed { id: id.clone(), name, kind: Kind::StarDict, detail, version: get("version").unwrap_or_default(), language: None, size: 0 })?;
        // Check it reads before it's listed.
        StarDict::open(&staging)?;
        settle(&staging, &id)?;
        Ok(id)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

pub fn remove(id: &str) -> Result<(), String> {
    if id.is_empty() || id.contains('/') || id.starts_with('.') { return Err(format!("bad dictionary id {id:?}")); }
    close(id);
    std::fs::remove_dir_all(dir().join(id)).map_err(|e| e.to_string())
}

/// `1.2 MB`.
pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 { format!("{:.1} GB", b / 1e9) } else if b >= 1e6 { format!("{:.1} MB", b / 1e6) } else if b >= 1e3 { format!("{:.0} KB", b / 1e3) } else { format!("{bytes} B") }
}

/// Downloads in progress, shared with the settings window: id → (done, total) or the error.
pub type Progress = Arc<Mutex<HashMap<String, Result<(u64, u64), String>>>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freedict_catalogue_parses() {
        let json = r#"[{"name":"afr-eng","headwords":"5129","releases":[
            {"URL":"https://x/a.slob","platform":"slob","size":"1","version":"0.2.2","date":"2019-01-01"},
            {"URL":"https://x/a.stardict.tar.xz","platform":"stardict","size":"102492","version":"0.2.2","date":"2024-01-10"}]},
            {"software":{}}]"#;
        let v = parse_freedict(json).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, "freedict-afr-eng");
        assert_eq!(v[0].name, "Afrikaans → English");
        assert_eq!(v[0].size, 102492);
        assert!(v[0].detail.contains("5,129 entries"));
        assert!(v[0].matches("english") && v[0].matches("afr") && !v[0].matches("german"));
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(human_size(16_358_468), "16.4 MB");
        assert_eq!(thousands(460315), "460,315");
        assert_eq!(slug("Afrikaans-English FreeDict Dictionary (af-en)"), "afrikaans-english-freedict-dictionary-af-en");
    }
}
