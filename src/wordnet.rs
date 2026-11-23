//! Princeton WordNet, read straight from its database files (`index.noun`, `data.noun`, … and
//! the `*.exc` exception lists): an offline English dictionary and thesaurus.
//!
//! The index files and exception lists are mapped and binary-searched (`Lines`); a synset is
//! read from its data file at the offset the index gives. Inflected forms find their lemma as WordNet's own `morphy`
//! does: the exception lists, then each part of speech's suffix rules.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::dict::{Definition, Meaning, Sense};

const POS: [(&str, &str); 4] = [("noun", "noun"), ("verb", "verb"), ("adj", "adjective"), ("adv", "adverb")];

/// The files a WordNet folder needs, as `library` keeps them.
pub const FILES: [&str; 12] = [
    "index.noun", "index.verb", "index.adj", "index.adv",
    "data.noun", "data.verb", "data.adj", "data.adv",
    "noun.exc", "verb.exc", "adj.exc", "adv.exc",
];

/// A file of lines sorted by their first word (an index, an exception list), mapped, with where
/// each line starts: a lookup is a binary search over a few of its pages.
struct Lines {
    text: crate::mapped::Mapped,
    starts: Vec<u32>,
}

impl Lines {
    /// A file that can't be read is an empty list, unless it's `required`.
    fn open(path: &Path, required: bool) -> Result<Lines, String> {
        let text = match crate::mapped::Mapped::open(path) {
            Ok(t) => t,
            Err(_) if !required => return Ok(Lines { text: crate::mapped::Mapped::empty(), starts: Vec::new() }),
            Err(e) => return Err(e),
        };
        if text.len() > u32::MAX as usize { return Err(format!("{} is too large", path.display())); }
        let mut lines = Lines { text, starts: Vec::new() };
        let mut starts = Vec::new();
        let mut at = 0;
        for line in lines.text.split_inclusive(|b| *b == b'\n') {
            // The licence header's lines start with spaces.
            if !line.starts_with(b" ") && !line.trim_ascii().is_empty() { starts.push(at as u32); }
            at += line.len();
        }
        // WordNet keeps them sorted, as its own binary search needs; make sure of it.
        if !starts.is_sorted_by(|a, b| lines.key(*a) <= lines.key(*b)) { starts.sort_by(|a, b| lines.key(*a).cmp(lines.key(*b))); }
        lines.starts = starts;
        // Finding the lines read every page: let them go until a lookup wants some.
        lines.text.release();
        Ok(lines)
    }

    fn line(&self, start: u32) -> &[u8] {
        let s = &self.text[start as usize..];
        &s[..s.iter().position(|b| *b == b'\n').unwrap_or(s.len())]
    }

    fn key(&self, start: u32) -> &[u8] {
        let l = self.line(start);
        &l[..l.iter().position(|b| *b == b' ').unwrap_or(l.len())]
    }

    /// The lines whose first word is `key`.
    fn find(&self, key: &str) -> impl Iterator<Item = &str> + use<'_> {
        let lo = self.starts.partition_point(|s| self.key(*s) < key.as_bytes());
        let n = self.starts[lo..].iter().take_while(|s| self.key(**s) == key.as_bytes()).count();
        self.starts[lo..lo + n].iter().filter_map(|s| std::str::from_utf8(self.line(*s)).ok())
    }
}

pub struct WordNet {
    dir: PathBuf,
    /// Each part of speech's index.
    index: Vec<Lines>,
    /// Each part of speech's inflected forms and their lemmas (`axes ax axis`).
    exceptions: Vec<Lines>,
}

impl WordNet {
    pub fn open(dir: &Path) -> Result<WordNet, String> {
        let mut index = Vec::new();
        let mut exceptions = Vec::new();
        for (file, _) in POS {
            index.push(Lines::open(&dir.join(format!("index.{file}")), true)?);
            exceptions.push(Lines::open(&dir.join(format!("{file}.exc")), false)?);
        }
        Ok(WordNet { dir: dir.to_path_buf(), index, exceptions })
    }

    /// Whether an index or exception list changed on disk since it was opened.
    pub fn changed(&self) -> bool { self.index.iter().chain(&self.exceptions).any(|l| l.text.changed()) }

    /// The index line for `lemma` under part of speech `p`.
    fn entry(&self, p: usize, lemma: &str) -> Option<&str> {
        self.index[p].find(lemma).next()
    }

    /// Lemmas `word` may be a form of under part of speech `p`, as `morphy` finds them.
    fn lemmas(&self, p: usize, word: &str) -> Vec<String> {
        let mut out = vec![word.to_string()];
        for line in self.exceptions[p].find(word) { out.extend(line.split_whitespace().skip(1).map(str::to_string)); }
        let rules: &[(&str, &str)] = match p {
            0 => &[("s", ""), ("ses", "s"), ("xes", "x"), ("zes", "z"), ("ches", "ch"), ("shes", "sh"), ("men", "man"), ("ies", "y")],
            1 => &[("s", ""), ("ies", "y"), ("es", "e"), ("es", ""), ("ed", "e"), ("ed", ""), ("ing", "e"), ("ing", "")],
            2 => &[("er", ""), ("est", ""), ("er", "e"), ("est", "e")],
            _ => &[],
        };
        for (suffix, with) in rules {
            if let Some(stem) = word.strip_suffix(suffix) && !stem.is_empty() { out.push(format!("{stem}{with}")); }
        }
        // Doubled consonants: running → run, bigger → big.
        if p != 0 {
            for suffix in ["ing", "ed", "er", "est"] {
                if let Some(stem) = word.strip_suffix(suffix) && stem.len() > 2 {
                    let b = stem.as_bytes();
                    if b[b.len() - 1] == b[b.len() - 2] { out.push(stem[..stem.len() - 1].to_string()); }
                }
            }
        }
        let mut seen = Vec::new();
        out.retain(|l| if seen.contains(l) { false } else { seen.push(l.clone()); true });
        out
    }

    fn synset(&self, p: usize, offset: u64) -> Option<Synset> {
        let f = std::fs::File::open(self.dir.join(format!("data.{}", POS[p].0))).ok()?;
        let mut r = BufReader::new(f);
        r.seek(SeekFrom::Start(offset)).ok()?;
        let mut line = String::new();
        r.read_line(&mut line).ok()?;
        Synset::parse(&line)
    }

    pub fn define(&self, word: &str) -> Option<Definition> {
        let w = word.trim().to_lowercase().replace(' ', "_");
        let mut meanings = Vec::new();
        let mut headword = None;
        for p in 0..POS.len() {
            let Some((lemma, line)) = self.lemmas(p, &w).into_iter().find_map(|l| self.entry(p, &l).map(|e| (l, e))) else { continue };
            headword.get_or_insert(lemma.replace('_', " "));
            let fields: Vec<&str> = line.split_whitespace().collect();
            let synsets: usize = fields.get(2)?.parse().ok()?;
            let pointers: usize = fields.get(3)?.parse().ok()?;
            let offsets = fields.iter().skip(4 + pointers + 2).take(synsets).filter_map(|o| o.parse::<u64>().ok());
            let mut m = Meaning { part_of_speech: POS[p].1.into(), ..Meaning::default() };
            for (i, offset) in offsets.enumerate() {
                let Some(s) = self.synset(p, offset) else { continue };
                // Synonyms from the commonest senses, as a thesaurus lists them.
                if i < 4 {
                    let others = s.words.iter().filter(|x| !x.eq_ignore_ascii_case(&lemma)).map(|x| x.replace('_', " "));
                    for o in others { if !m.synonyms.contains(&o) { m.synonyms.push(o); } }
                    for (sym, target, tp, src_tgt) in &s.pointers {
                        if sym != "!" { continue; }
                        let src = usize::from_str_radix(&src_tgt[..2], 16).unwrap_or(0);
                        if src != 0 && !s.words.get(src - 1).is_some_and(|x| x.eq_ignore_ascii_case(&lemma)) { continue; }
                        let tp = match tp.as_str() { "n" => 0, "v" => 1, "a" | "s" => 2, _ => 3 };
                        if let Some(t) = self.synset(tp, *target) {
                            let tgt = usize::from_str_radix(&src_tgt[2..], 16).unwrap_or(0);
                            if let Some(a) = t.words.get(tgt.saturating_sub(1)) {
                                let a = a.replace('_', " ");
                                if !m.antonyms.contains(&a) { m.antonyms.push(a); }
                            }
                        }
                    }
                }
                m.senses.push(Sense { text: s.gloss, example: s.examples.into_iter().next() });
            }
            if !m.senses.is_empty() { meanings.push(m); }
        }
        let headword = headword?;
        if meanings.is_empty() { return None; }
        let note = (headword.to_lowercase() != word.trim().to_lowercase()).then(|| format!("from {}", word.trim().to_lowercase()));
        Some(Definition { headword, phonetic: None, meanings, note, url: String::new(), source: None })
    }
}

struct Synset {
    words: Vec<String>,
    /// (symbol, target offset, target part of speech, source/target word numbers as hex).
    pointers: Vec<(String, u64, String, String)>,
    gloss: String,
    examples: Vec<String>,
}

impl Synset {
    /// `offset lex_filenum ss_type w_cnt word lex_id … p_cnt ptr… [frames] | gloss`.
    fn parse(line: &str) -> Option<Synset> {
        let (head, gloss) = line.split_once(" | ").unwrap_or((line, ""));
        let f: Vec<&str> = head.split_whitespace().collect();
        let n = usize::from_str_radix(f.get(3)?, 16).ok()?;
        let words = (0..n).filter_map(|i| f.get(4 + i * 2)).map(|w| {
            // Adjectives may carry a syntactic marker: `galore(ip)`.
            w.split_once('(').map_or(*w, |(a, _)| a).to_string()
        }).collect();
        let at = 4 + n * 2;
        let p: usize = f.get(at)?.parse().ok()?;
        let pointers = (0..p).filter_map(|i| {
            let b = at + 1 + i * 4;
            Some((f.get(b)?.to_string(), f.get(b + 1)?.parse().ok()?, f.get(b + 2)?.to_string(), f.get(b + 3)?.to_string()))
        }).collect();
        // `definition; "an example"; "another"`.
        let gloss = gloss.trim();
        let (def, examples) = match gloss.find("; \"").or_else(|| gloss.starts_with('"').then_some(0)) {
            Some(i) => {
                let ex = gloss[i..].split('"').skip(1).step_by(2).map(|e| e.trim().to_string()).filter(|e| !e.is_empty()).collect();
                (gloss[..i].trim(), ex)
            }
            None => (gloss, Vec::new()),
        };
        let mut gloss = def.trim_end_matches(';').trim().to_string();
        if let Some(c) = gloss.get(..1) { gloss.replace_range(..1, &c.to_uppercase()); }
        Some(Synset { words, pointers, gloss, examples })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_synset_line_parses() {
        let line = "02086723 05 n 03 dog 0 domestic_dog 0 Canis_familiaris 0 002 @ 02085998 n 0000 ! 01234567 n 0101 | a member of the genus Canis; \"the dog barked all night\"; \"a second\"\n";
        let s = Synset::parse(line).unwrap();
        assert_eq!(s.words, ["dog", "domestic_dog", "Canis_familiaris"]);
        assert_eq!(s.pointers.len(), 2);
        assert_eq!(s.pointers[1], ("!".into(), 1234567, "n".into(), "0101".into()));
        assert_eq!(s.gloss, "A member of the genus Canis");
        assert_eq!(s.examples, ["the dog barked all night", "a second"]);
    }

    /// Runs against a real WordNet when `TLUM_WORDNET` points at one.
    #[test]
    fn sorted_lines_find_every_line_of_a_key() {
        let dir = std::env::temp_dir().join(format!("tlum-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("noun.exc");
        // A header line led by spaces, an empty line, CRLF, a repeated key, out of order, and
        // no newline at the end.
        std::fs::write(&path, "  1 licence text\n\nmice mouse\r\naxes ax\naxes axis\ngeese goose").unwrap();
        let l = Lines::open(&path, true).unwrap();
        assert_eq!(l.find("axes").collect::<Vec<_>>(), ["axes ax", "axes axis"]);
        assert_eq!(l.find("geese").collect::<Vec<_>>(), ["geese goose"]);
        assert_eq!(l.find("mice").collect::<Vec<_>>(), ["mice mouse\r"]);
        assert_eq!(l.find("licence").count() + l.find("").count() + l.find("ax").count(), 0);
        assert!(Lines::open(&dir.join("missing"), false).unwrap().find("axes").next().is_none());
        assert!(Lines::open(&dir.join("missing"), true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn looks_up_a_real_wordnet() {
        let Some(dir) = std::env::var_os("TLUM_WORDNET") else { return };
        let wn = WordNet::open(Path::new(&dir)).unwrap();
        let d = wn.define("dogs").unwrap();
        assert_eq!(d.headword, "dog");
        assert_eq!(d.meanings[0].part_of_speech, "noun");
        assert!(d.meanings[0].synonyms.iter().any(|s| s == "domestic dog"), "{:?}", d.meanings[0].synonyms);
        assert_eq!(wn.define("geese").unwrap().headword, "goose");
        assert_eq!(wn.define("running").unwrap().headword, "running");
        assert!(wn.define("good").unwrap().meanings.iter().any(|m| m.antonyms.iter().any(|a| a == "bad")));
        assert!(wn.define("qwxz").is_none());
    }
}
