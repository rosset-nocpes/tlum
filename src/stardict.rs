//! StarDict dictionaries (`.ifo`, `.idx`, `.dict`), the format FreeDict, GoldenDict and most
//! free Linux dictionaries ship in. Each is a folder under the dictionaries directory, with its
//! index and data stored uncompressed (`library::normalize` unpacks `.idx.gz` and `.dict.dz`).
//!
//! The index stays in memory as raw bytes plus entry starts, sorted as StarDict sorts it (ASCII
//! case-insensitively, then bytewise), so a lookup is a binary search; the data is read from
//! disk at the entry's offset.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::dict::{Definition, Meaning, Sense, strip_html};

pub struct StarDict {
    idx: Vec<u8>,
    /// Where each entry starts in `idx`.
    starts: Vec<u32>,
    offset64: bool,
    dict: PathBuf,
    /// `sametypesequence`: the field types every entry has, so entries carry no type bytes.
    types: Option<Vec<u8>>,
}

/// The `key=value` lines of an `.ifo` file.
pub fn read_ifo(path: &Path) -> Result<Vec<(String, String)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut lines = text.lines();
    if !lines.next().is_some_and(|l| l.trim_start_matches('\u{feff}').starts_with("StarDict's dict ifo file")) {
        return Err(format!("{} isn't a StarDict .ifo file", path.display()));
    }
    Ok(lines.filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect())
}

/// The `.ifo` file in `dir`.
pub fn find_ifo(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "ifo"))
}

impl StarDict {
    pub fn open(dir: &Path) -> Result<StarDict, String> {
        let ifo = find_ifo(dir).ok_or_else(|| format!("no .ifo file in {}", dir.display()))?;
        let info = read_ifo(&ifo)?;
        let get = |k: &str| info.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        let stem = ifo.with_extension("");
        let idx_path = stem.with_extension("idx");
        let idx = std::fs::read(&idx_path).map_err(|e| format!("{}: {e}", idx_path.display()))?;
        let offset64 = get("idxoffsetbits") == Some("64");
        let tail = if offset64 { 12 } else { 8 };
        let mut starts = Vec::with_capacity(get("wordcount").and_then(|n| n.parse().ok()).unwrap_or(0));
        let mut i = 0;
        while i < idx.len() {
            let Some(nul) = idx[i..].iter().position(|b| *b == 0) else { break };
            if i + nul + 1 + tail > idx.len() { break; }
            starts.push(i as u32);
            i += nul + 1 + tail;
        }
        let dict = stem.with_extension("dict");
        if !dict.exists() { return Err(format!("{} is missing", dict.display())); }
        Ok(StarDict {
            idx, starts, offset64, dict,
            types: get("sametypesequence").filter(|s| !s.is_empty()).map(|s| s.as_bytes().to_vec()),
        })
    }

    fn word(&self, i: usize) -> &[u8] {
        let s = self.starts[i] as usize;
        let nul = self.idx[s..].iter().position(|b| *b == 0).unwrap_or(0);
        &self.idx[s..s + nul]
    }

    fn location(&self, i: usize) -> (u64, u32) {
        let s = self.starts[i] as usize + self.word(i).len() + 1;
        let b = &self.idx[s..];
        if self.offset64 {
            (u64::from_be_bytes(b[..8].try_into().unwrap()), u32::from_be_bytes(b[8..12].try_into().unwrap()))
        } else {
            (u32::from_be_bytes(b[..4].try_into().unwrap()) as u64, u32::from_be_bytes(b[4..8].try_into().unwrap()))
        }
    }

    /// Entries whose headword is `word` ignoring ASCII case, exact matches first.
    fn find(&self, word: &str) -> Vec<usize> {
        let key = word.as_bytes();
        let fold = |a: &[u8], b: &[u8]| a.iter().map(u8::to_ascii_lowercase).cmp(b.iter().map(u8::to_ascii_lowercase));
        let (mut lo, mut hi) = (0, self.starts.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            if fold(self.word(mid), key).is_lt() { lo = mid + 1 } else { hi = mid }
        }
        let mut hits: Vec<usize> = (lo..self.starts.len()).take_while(|&i| fold(self.word(i), key).is_eq()).collect();
        hits.sort_by_key(|&i| self.word(i) != key);
        hits
    }

    /// The raw fields of entry `i`: (type, bytes).
    fn fields(&self, i: usize) -> Result<Vec<(u8, Vec<u8>)>, String> {
        let (offset, size) = self.location(i);
        let mut f = std::fs::File::open(&self.dict).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let mut data = vec![0; size as usize];
        f.read_exact(&mut data).map_err(|e| e.to_string())?;
        Ok(split_fields(&data, self.types.as_deref()))
    }

    /// The entry for `word` (or its lower-case form), as the popover shows a definition.
    pub fn define(&self, word: &str) -> Option<Definition> {
        let mut hits = self.find(word);
        if hits.is_empty() && word.to_lowercase() != word { hits = self.find(&word.to_lowercase()); }
        let headword = String::from_utf8_lossy(self.word(*hits.first()?)).into_owned();
        let mut phonetic = None;
        let mut senses = Vec::new();
        for i in hits.into_iter().take(4) {
            for (kind, bytes) in self.fields(i).ok()? {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                match kind {
                    b't' => { phonetic.get_or_insert(text.trim().to_string()); }
                    b'h' | b'g' | b'x' => senses.extend(html_senses(&text)),
                    b'm' | b'l' | b'y' | b'k' | b'w' => senses.extend(text.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| Sense { text: l.to_string(), example: None })),
                    _ => {}
                }
            }
        }
        // A plain-text entry often repeats its headword (and pronunciation) on the first line.
        if senses.len() > 1 && senses[0].text.eq_ignore_ascii_case(&headword) { senses.remove(0); }
        senses.dedup();
        (!senses.is_empty()).then(|| Definition {
            headword, phonetic, note: None, url: String::new(),
            meanings: vec![Meaning { part_of_speech: String::new(), senses, ..Meaning::default() }],
        })
    }
}

/// An entry's fields. With `sametypesequence` the types are given and only the last field runs
/// to the end; otherwise each field leads with its type byte. Lower-case types are
/// NUL-terminated text, upper-case ones are a big-endian size and that many bytes.
fn split_fields(data: &[u8], types: Option<&[u8]>) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut n = 0;
    while i < data.len() {
        let kind = match types {
            Some(t) => match t.get(n) { Some(k) => *k, None => break },
            None => { let k = data[i]; i += 1; k }
        };
        let last = types.is_some_and(|t| n + 1 == t.len());
        n += 1;
        let (bytes, next) = if last {
            (&data[i..], data.len())
        } else if kind.is_ascii_lowercase() {
            let end = data[i..].iter().position(|b| *b == 0).map_or(data.len(), |p| i + p);
            (&data[i..end], end + 1)
        } else {
            if i + 4 > data.len() { break; }
            let size = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as usize;
            let end = (i + 4 + size).min(data.len());
            (&data[i + 4..end], end)
        };
        out.push((kind, bytes.to_vec()));
        i = next;
    }
    out
}

/// Senses in an HTML entry: its list items if it has any, else its lines.
fn html_senses(html: &str) -> Vec<Sense> {
    let lower = html.to_lowercase();
    let pieces: Vec<String> = if lower.contains("<li") {
        // Text up to each item's end, item by item; nested lists flatten into their items.
        let mut v = Vec::new();
        let mut rest = html;
        while let Some(start) = rest.to_lowercase().find("<li") {
            rest = &rest[start + 3..];
            let end = ["<li", "</li", "</ol", "</ul"].iter().filter_map(|t| rest.to_lowercase().find(t)).min().unwrap_or(rest.len());
            v.push(rest[rest.find('>').map_or(0, |g| (g + 1).min(end))..end].to_string());
        }
        v
    } else {
        let breaks = html.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n").replace("</p>", "\n").replace("</div>", "\n");
        breaks.lines().map(str::to_string).collect()
    };
    pieces.iter().map(|p| strip_html(p)).filter(|t| !t.is_empty()).map(|text| Sense { text, example: None }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_dict(dir: &Path, entries: &[(&str, &str)], types: &str) {
        let mut sorted: Vec<_> = entries.to_vec();
        sorted.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()).then(a.0.cmp(b.0)));
        let (mut idx, mut dict) = (Vec::new(), Vec::new());
        for (w, d) in &sorted {
            idx.extend_from_slice(w.as_bytes());
            idx.push(0);
            idx.extend_from_slice(&(dict.len() as u32).to_be_bytes());
            idx.extend_from_slice(&(d.len() as u32).to_be_bytes());
            dict.extend_from_slice(d.as_bytes());
        }
        std::fs::write(dir.join("t.idx"), idx).unwrap();
        std::fs::write(dir.join("t.dict"), dict).unwrap();
        std::fs::write(dir.join("t.ifo"), format!("StarDict's dict ifo file\nversion=3.0.0\nbookname=Test\nwordcount={}\nsametypesequence={types}\n", entries.len())).unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tlum-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_entries_ignoring_case_and_splits_html_lists() {
        let d = tmp("stardict");
        write_dict(&d, &[("apple", "<ol><li><div>Apfel</div></li><li>Apfelbaum</li></ol>"), ("Bank", "<div>bench</div><br>bank"), ("zebra", "Zebra")], "h");
        let s = StarDict::open(&d).unwrap();
        assert_eq!(s.starts.len(), 3);
        let a = s.define("Apple").unwrap();
        assert_eq!(a.headword, "apple");
        assert_eq!(a.meanings[0].senses.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["Apfel", "Apfelbaum"]);
        assert_eq!(s.define("bank").unwrap().meanings[0].senses.len(), 2);
        assert!(s.define("zeb").is_none() && s.define("zz").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn typed_fields_split() {
        let mut data = vec![b't'];
        data.extend_from_slice(b"ab\0");
        data.push(b'm');
        data.extend_from_slice(b"text\0");
        assert_eq!(split_fields(&data, None), vec![(b't', b"ab".to_vec()), (b'm', b"text".to_vec())]);
        assert_eq!(split_fields(b"ph\0body", Some(b"tm")), vec![(b't', b"ph".to_vec()), (b'm', b"body".to_vec())]);
    }
}
