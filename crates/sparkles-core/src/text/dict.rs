//! Dictionary segmentation of Chinese, Japanese and Korean text for the `ja-dict`,
//! `zh-dict` and `ko-dict` analyzers (cargo feature `text-cjk-dict`).
//!
//! A dictionary lists words with costs, and the text between two word boundaries is cut
//! into the sequence of words whose summed costs are the lowest, found by the Viterbi
//! algorithm over the lattice of every word that starts at every character. This is how
//! MeCab, and after it Lucene's Kuromoji (Japanese) and Nori (Korean), segment text.
//!
//! * `ja-dict` reads a MeCab dictionary laid out as IPADIC: lexicon files of
//!   `surface,left id,right id,cost,` and nine features (four parts of speech, two
//!   inflection fields, the base form and two readings), `matrix.def` with the cost of
//!   each pair of adjacent ids, and `char.def` and `unk.def` for the words the lexicon
//!   lacks. As in Kuromoji's search mode, a word of more than two Han characters, or of
//!   more than seven other characters, costs more for each character over that, so
//!   compounds split into their parts. Particles, auxiliary verbs, conjunctions and
//!   symbols are dropped by their part of speech, inflected words are replaced by their
//!   base form, and a long Katakana word loses a final `ー`, as Kuromoji's analyzer does.
//! * `ko-dict` reads a MeCab dictionary laid out as mecab-ko-dic, whose features are the
//!   part of speech, a semantic class, the final consonant, the reading, the type, the
//!   first and last parts of speech and the expression. Endings, particles, symbols and
//!   the other parts of speech that Nori's analyzer drops by default are dropped, and a
//!   compound or an inflected word is replaced by its parts, as Nori does by default.
//!   The Hangul words of a sentence are segmented together across the spaces between
//!   them, and an ending, particle, copula or suffix costs more after a space, as in
//!   Nori.
//! * `zh-dict` reads a word list with frequencies, one `word frequency [tag]` per line,
//!   as jieba's `dict.txt` is laid out. A word costs the negative logarithm of its share
//!   of all occurrences, and a character the list lacks costs as a word seen once.
//!
//! Only runs of adjacent Han, Hiragana, Katakana and Hangul words, as Lucene's tokenizer
//! finds them ([`super::standard`]), are segmented, with Korean runs joined across
//! whitespace. Other words pass through whole.
//! Widths are folded first, as Lucene's CJK width filter folds them.
//!
//! A dictionary is compiled from its source files into one file, `<name>.sdict`, whose
//! format starts with a magic number and a version. A reader refuses a version it does
//! not know, and the file ends with the SHA-256 of what precedes it, which the reader
//! checks. The index configuration records that hash, so a changed dictionary is not
//! read silently into an index built with another.

use crate::error::{Error, Result};
use sha2::Digest;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tantivy::tokenizer::{Token, Tokenizer};
use tantivy_fst::raw::Fst;

/// The version of the compiled dictionary format.
pub const FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"SPKSDICT";
/// A string id that stands for no string.
const NONE: u32 = u32::MAX;
/// MeCab's longest group of unknown characters.
const MAX_GROUP: usize = 24;

/// How a dictionary's source is laid out, which fixes its features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// MeCab's IPADIC, for `ja-dict`
    Ipadic = 1,
    /// mecab-ko-dic, for `ko-dict`
    KoDic = 2,
    /// a word list with frequencies, for `zh-dict`
    Frequency = 3,
}

impl Layout {
    /// The layout of an analyzer's dictionary, by the analyzer's name.
    pub fn of(name: &str) -> Option<Layout> {
        match name {
            "ja-dict" => Some(Layout::Ipadic),
            "ko-dict" => Some(Layout::KoDic),
            "zh-dict" => Some(Layout::Frequency),
            _ => None,
        }
    }

    fn from_u8(b: u8) -> Option<Layout> {
        match b {
            1 => Some(Layout::Ipadic),
            2 => Some(Layout::KoDic),
            3 => Some(Layout::Frequency),
            _ => None,
        }
    }
}

fn bad(msg: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("text dictionary: {msg}"))
}

/// A word of the dictionary: its connection ids, its cost and its features.
#[derive(Clone, Copy, Debug)]
struct Entry {
    left: u16,
    right: u16,
    cost: i16,
    /// the part of speech as one string (string id)
    pos: u32,
    /// the base form of an inflected Japanese word, or the morphemes of an inflected or
    /// preanalyzed Korean word (`surface/POS/*` joined by `+`), or NONE
    base: u32,
    /// a Korean compound's parts, `surface/POS/*` joined by `+`, or NONE
    parts: u32,
}

/// A character category of `char.def`.
#[derive(Clone, Debug)]
struct Category {
    name: String,
    /// look for unknown words even where the lexicon has words
    invoke: bool,
    /// make one unknown word of the whole run of the category
    group: bool,
    /// make unknown words of 1 to `length` characters
    length: u8,
}

/// The source of a dictionary, parsed: what the compiled file holds.
#[derive(Default)]
struct Parts {
    strings: Vec<String>,
    entries: Vec<Entry>,
    /// surface → the entries' range in `entries` (sorted by surface)
    words: BTreeMap<String, (u32, u32)>,
    matrix: (u16, u16, Vec<i16>),
    categories: Vec<Category>,
    /// code point ranges in order, each with its categories (a bit mask) and the
    /// default one; a later range overrides an earlier one
    ranges: Vec<(u32, u32, u32, u8)>,
    /// per category, the range of its unknown-word entries in `entries`
    unknown: Vec<(u32, u32)>,
}

struct Interner {
    strings: Vec<String>,
    ids: std::collections::HashMap<String, u32>,
}

impl Interner {
    fn new() -> Self {
        Interner {
            strings: Vec::new(),
            ids: Default::default(),
        }
    }

    fn id(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.ids.get(s) {
            return i;
        }
        let i = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.ids.insert(s.to_string(), i);
        i
    }
}

/// Splits a MeCab CSV line, where a field may be quoted with `"`.
fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// The text of a source file, in UTF-8 or else EUC-JP (IPADIC's encoding).
fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| bad(format!("{}: {e}", path.display())))?;
    match String::from_utf8(bytes) {
        Ok(s) => Ok(s),
        Err(e) => {
            let (s, _, errors) = encoding_rs::EUC_JP.decode(e.as_bytes());
            if errors {
                return Err(bad(format!(
                    "{} is neither UTF-8 nor EUC-JP",
                    path.display()
                )));
            }
            Ok(s.into_owned())
        }
    }
}

/// An entry of a lexicon or `unk.def` line from its fields after the surface.
fn entry_of(f: &[String], layout: Layout, strings: &mut Interner, at: &str) -> Result<Entry> {
    let num = |i: usize| -> Result<i64> {
        f.get(i)
            .and_then(|v| v.trim().parse::<i64>().ok())
            .ok_or_else(|| bad(format!("{at}: field {} is not a number", i + 2)))
    };
    let (left, right, cost) = (num(0)?, num(1)?, num(2)?);
    if !(0..=u16::MAX as i64).contains(&left) || !(0..=u16::MAX as i64).contains(&right) {
        return Err(bad(format!("{at}: connection id out of range")));
    }
    let cost = cost.clamp(i16::MIN as i64, i16::MAX as i64) as i16;
    let feat = |i: usize| f.get(3 + i).map(String::as_str).unwrap_or("*");
    let (pos, base, parts) = match layout {
        Layout::Ipadic => {
            let pos: Vec<&str> = (0..4).map(feat).filter(|p| *p != "*").collect();
            let base = feat(6);
            (
                strings.id(&pos.join("-")),
                if base == "*" { NONE } else { strings.id(base) },
                NONE,
            )
        }
        Layout::KoDic => {
            let expr = |strings: &mut Interner| {
                if feat(7) == "*" {
                    NONE
                } else {
                    strings.id(feat(7))
                }
            };
            match feat(4) {
                "Compound" => (strings.id(feat(0)), NONE, expr(strings)),
                "Inflect" | "Preanalysis" => (strings.id(feat(0)), expr(strings), NONE),
                _ => (strings.id(feat(0)), NONE, NONE),
            }
        }
        Layout::Frequency => (NONE, NONE, NONE),
    };
    Ok(Entry {
        left: left as u16,
        right: right as u16,
        cost,
        pos,
        base,
        parts,
    })
}

/// Parses a MeCab source dictionary: every `*.csv` in `dir`, `matrix.def`, `char.def`
/// and `unk.def`.
fn parse_mecab(dir: &Path, layout: Layout) -> Result<Parts> {
    let mut strings = Interner::new();
    let mut words: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| bad(format!("{}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("csv")))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(bad(format!(
            "{} has no lexicon (*.csv) files",
            dir.display()
        )));
    }
    for file in &files {
        let text = read_text(file)?;
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let f = csv_fields(line);
            let at = format!("{}:{}", file.display(), n + 1);
            if f.len() < 4 || f[0].is_empty() {
                return Err(bad(format!("{at}: expected surface,left,right,cost,…")));
            }
            let e = entry_of(&f[1..], layout, &mut strings, &at)?;
            words.entry(f[0].clone()).or_default().push(e);
        }
    }
    // matrix.def: "size1 size2", then "right-id left-id cost" lines
    let matrix_text = read_text(&dir.join("matrix.def"))?;
    let mut lines = matrix_text.lines();
    let dims: Vec<usize> = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    let [r, l] = dims[..] else {
        return Err(bad("matrix.def does not start with its two sizes"));
    };
    if r == 0 || l == 0 || r > u16::MAX as usize || l > u16::MAX as usize {
        return Err(bad("matrix.def has sizes out of range"));
    }
    let mut costs = vec![0i16; r * l];
    for (n, line) in lines.enumerate() {
        let v: Vec<i64> = line
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        match v[..] {
            [] => {}
            [a, b, c] if (0..r as i64).contains(&a) && (0..l as i64).contains(&b) => {
                costs[a as usize * l + b as usize] =
                    c.clamp(i16::MIN as i64, i16::MAX as i64) as i16;
            }
            _ => return Err(bad(format!("matrix.def:{}: bad line", n + 2))),
        }
    }
    let (categories, ranges) = parse_char_def(&read_text(&dir.join("char.def"))?)?;
    // unk.def: lexicon lines whose surface is a category name
    let mut unk: Vec<Vec<Entry>> = vec![Vec::new(); categories.len()];
    let unk_text = read_text(&dir.join("unk.def"))?;
    for (n, line) in unk_text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let f = csv_fields(line);
        let at = format!("unk.def:{}", n + 1);
        let Some(c) = categories.iter().position(|c| c.name == f[0]) else {
            return Err(bad(format!("{at}: unknown category {:?}", f[0])));
        };
        unk[c].push(entry_of(&f[1..], layout, &mut strings, &at)?);
    }
    let check = |e: &Entry| (e.right as usize) < r && (e.left as usize) < l;
    if !words
        .values()
        .flatten()
        .chain(unk.iter().flatten())
        .all(check)
    {
        return Err(bad(
            "an entry has a connection id beyond matrix.def's sizes",
        ));
    }
    Ok(assemble(
        strings,
        words,
        (r as u16, l as u16, costs),
        categories,
        ranges,
        unk,
    ))
}

fn assemble(
    strings: Interner,
    words: BTreeMap<String, Vec<Entry>>,
    matrix: (u16, u16, Vec<i16>),
    categories: Vec<Category>,
    ranges: Vec<(u32, u32, u32, u8)>,
    unk: Vec<Vec<Entry>>,
) -> Parts {
    let mut entries = Vec::new();
    let mut index = BTreeMap::new();
    for (w, es) in words {
        index.insert(w, (entries.len() as u32, es.len() as u32));
        entries.extend(es);
    }
    let mut unknown = Vec::new();
    for es in unk {
        unknown.push((entries.len() as u32, es.len() as u32));
        entries.extend(es);
    }
    Parts {
        strings: strings.strings,
        entries,
        words: index,
        matrix,
        categories,
        ranges,
        unknown,
    }
}

/// `char.def`: category lines `NAME invoke group length`, then code point lines
/// `0xXXXX[..0xYYYY] NAME [NAME…]`.
#[allow(clippy::type_complexity)]
fn parse_char_def(text: &str) -> Result<(Vec<Category>, Vec<(u32, u32, u32, u8)>)> {
    let mut cats: Vec<Category> = Vec::new();
    let mut ranges = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let at = format!("char.def:{}", n + 1);
        if f[0].starts_with("0x") {
            let (lo, hi) = match f[0].split_once("..") {
                Some((a, b)) => (a, b),
                None => (f[0], f[0]),
            };
            let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16);
            let (Ok(lo), Ok(hi)) = (hex(lo), hex(hi)) else {
                return Err(bad(format!("{at}: bad code point range")));
            };
            let mut mask = 0u32;
            let mut default = None;
            for name in &f[1..] {
                let Some(c) = cats.iter().position(|c| c.name == *name) else {
                    return Err(bad(format!("{at}: unknown category {name:?}")));
                };
                mask |= 1 << c;
                default.get_or_insert(c as u8);
            }
            let Some(default) = default else {
                return Err(bad(format!("{at}: no category")));
            };
            ranges.push((lo, hi, mask, default));
        } else {
            let [name, invoke, group, length] = f[..] else {
                return Err(bad(format!("{at}: expected NAME invoke group length")));
            };
            if cats.len() == 32 {
                return Err(bad("char.def has more than 32 categories"));
            }
            cats.push(Category {
                name: name.to_string(),
                invoke: invoke == "1",
                group: group == "1",
                length: length
                    .parse()
                    .map_err(|_| bad(format!("{at}: bad length")))?,
            });
        }
    }
    if !cats.iter().any(|c| c.name == "DEFAULT") {
        return Err(bad("char.def has no DEFAULT category"));
    }
    Ok((cats, ranges))
}

/// Parses a word list with frequencies: `word frequency [tag]` per line.
fn parse_frequency(file: &Path) -> Result<Parts> {
    let text = read_text(file)?;
    let mut freq: BTreeMap<String, u64> = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.is_empty() {
            continue;
        }
        let at = format!("{}:{}", file.display(), n + 1);
        let Some(Ok(c)) = f.get(1).map(|v| v.parse::<u64>()) else {
            return Err(bad(format!("{at}: expected word frequency [tag]")));
        };
        *freq.entry(f[0].to_string()).or_default() += c;
    }
    if freq.is_empty() {
        return Err(bad(format!("{} lists no words", file.display())));
    }
    let total: f64 = freq.values().map(|&c| c as f64).sum();
    // costs are negative log probabilities, scaled to fit i16 (a word seen once in a
    // billion occurrences costs about 20.7 × 1,500)
    let cost = |c: f64| ((total.ln() - c.max(1.0).ln()) * 1500.0).min(i16::MAX as f64) as i16;
    let words = freq
        .into_iter()
        .map(|(w, c)| {
            (
                w,
                vec![Entry {
                    left: 0,
                    right: 0,
                    cost: cost(c as f64),
                    pos: NONE,
                    base: NONE,
                    parts: NONE,
                }],
            )
        })
        .collect();
    // one category: a character on its own is always a candidate, at a frequency of one
    let categories = vec![Category {
        name: "DEFAULT".into(),
        invoke: true,
        group: false,
        length: 1,
    }];
    let unk = vec![vec![Entry {
        left: 0,
        right: 0,
        cost: cost(1.0),
        pos: NONE,
        base: NONE,
        parts: NONE,
    }]];
    Ok(assemble(
        Interner::new(),
        words,
        (1, 1, vec![0]),
        categories,
        Vec::new(),
        unk,
    ))
}

/// Compiles a dictionary from its source: a MeCab dictionary directory for
/// [`Layout::Ipadic`] and [`Layout::KoDic`], a word list file for
/// [`Layout::Frequency`]. Returns the compiled file's bytes.
pub fn compile(source: &Path, layout: Layout) -> Result<Vec<u8>> {
    let parts = match layout {
        Layout::Frequency => parse_frequency(source)?,
        _ => parse_mecab(source, layout)?,
    };
    encode(&parts, layout)
}

fn encode(p: &Parts, layout: Layout) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.push(layout as u8);
    let u32s = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    u32s(&mut out, p.strings.len() as u32);
    for s in &p.strings {
        u32s(&mut out, s.len() as u32);
        out.extend_from_slice(s.as_bytes());
    }
    let put_entry = |out: &mut Vec<u8>, e: &Entry| {
        out.extend_from_slice(&e.left.to_le_bytes());
        out.extend_from_slice(&e.right.to_le_bytes());
        out.extend_from_slice(&e.cost.to_le_bytes());
        out.extend_from_slice(&e.pos.to_le_bytes());
        out.extend_from_slice(&e.base.to_le_bytes());
        out.extend_from_slice(&e.parts.to_le_bytes());
    };
    u32s(&mut out, p.entries.len() as u32);
    for e in &p.entries {
        put_entry(&mut out, e);
    }
    // the words: an FST from surface to its index in the ranges, and the ranges
    let mut b = tantivy_fst::MapBuilder::memory();
    let mut word_ranges = Vec::with_capacity(p.words.len());
    for (i, (w, r)) in p.words.iter().enumerate() {
        b.insert(w.as_bytes(), i as u64).map_err(bad)?;
        word_ranges.push(*r);
    }
    let fst = b.into_inner().map_err(bad)?;
    u32s(&mut out, word_ranges.len() as u32);
    for (s, n) in &word_ranges {
        u32s(&mut out, *s);
        u32s(&mut out, *n);
    }
    out.extend_from_slice(&(fst.len() as u64).to_le_bytes());
    out.extend_from_slice(&fst);
    let (r, l, costs) = &p.matrix;
    out.extend_from_slice(&r.to_le_bytes());
    out.extend_from_slice(&l.to_le_bytes());
    for c in costs {
        out.extend_from_slice(&c.to_le_bytes());
    }
    u32s(&mut out, p.categories.len() as u32);
    for c in &p.categories {
        u32s(&mut out, c.name.len() as u32);
        out.extend_from_slice(c.name.as_bytes());
        out.extend_from_slice(&[c.invoke as u8, c.group as u8, c.length]);
    }
    u32s(&mut out, p.ranges.len() as u32);
    for (lo, hi, mask, d) in &p.ranges {
        u32s(&mut out, *lo);
        u32s(&mut out, *hi);
        u32s(&mut out, *mask);
        out.push(*d);
    }
    for (s, n) in &p.unknown {
        u32s(&mut out, *s);
        u32s(&mut out, *n);
    }
    let digest = sha2::Sha256::digest(&out);
    out.extend_from_slice(&digest);
    Ok(out)
}

/// A reader over the compiled bytes.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| bad("the file is truncated"))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| bad("a string is not UTF-8"))
    }
    /// A count of items of at least `size` bytes each, checked against what is left.
    fn count(&mut self, size: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(size) > self.b.len() - self.at {
            return Err(bad("a count runs past the end of the file"));
        }
        Ok(n)
    }
}

/// A compiled dictionary in memory.
pub struct Dictionary {
    layout: Layout,
    strings: Vec<String>,
    entries: Vec<Entry>,
    word_ranges: Vec<(u32, u32)>,
    fst: Fst<Vec<u8>>,
    size_l: usize,
    costs: Vec<i16>,
    categories: Vec<Category>,
    /// the categories (mask, default) of each code point below 0x10000
    bmp: Vec<(u32, u8)>,
    /// the ranges above it, last match wins
    astral: Vec<(u32, u32, u32, u8)>,
    unknown: Vec<(u32, u32)>,
    /// the SHA-256 of the compiled file, hex
    pub sha256: String,
}

impl Dictionary {
    /// Reads a compiled dictionary, checking its magic number, version and hash.
    pub fn from_bytes(bytes: &[u8]) -> Result<Dictionary> {
        if bytes.len() < MAGIC.len() + 4 + 1 + 32 || &bytes[..8] != MAGIC {
            return Err(bad("not a compiled Sparkles dictionary"));
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if version != FORMAT_VERSION {
            return Err(bad(format!(
                "format version {version} is not one this build reads ({FORMAT_VERSION}); \
                 compile the dictionary again with this build"
            )));
        }
        let (body, digest) = bytes.split_at(bytes.len() - 32);
        let sum = sha2::Sha256::digest(body);
        if sum.as_slice() != digest {
            return Err(bad("the file is damaged (its checksum does not match)"));
        }
        let mut r = Reader { b: body, at: 12 };
        let layout = Layout::from_u8(r.u8()?).ok_or_else(|| bad("unknown layout"))?;
        let n = r.count(4)?;
        let mut strings = Vec::with_capacity(n);
        for _ in 0..n {
            strings.push(r.string()?);
        }
        let n = r.count(18)?;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            entries.push(Entry {
                left: r.u16()?,
                right: r.u16()?,
                cost: r.i16()?,
                pos: r.u32()?,
                base: r.u32()?,
                parts: r.u32()?,
            });
        }
        let n = r.count(8)?;
        let mut word_ranges = Vec::with_capacity(n);
        for _ in 0..n {
            word_ranges.push((r.u32()?, r.u32()?));
        }
        let fst_len = r.u64()? as usize;
        let fst = Fst::new(r.take(fst_len)?.to_vec()).map_err(bad)?;
        let size_r = r.u16()? as usize;
        let size_l = r.u16()? as usize;
        let mut costs = Vec::with_capacity(size_r * size_l);
        for _ in 0..size_r * size_l {
            costs.push(r.i16()?);
        }
        let n = r.count(7)?;
        let mut categories = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.string()?;
            let (invoke, group, length) = (r.u8()? != 0, r.u8()? != 0, r.u8()?);
            categories.push(Category {
                name,
                invoke,
                group,
                length,
            });
        }
        let default = categories
            .iter()
            .position(|c| c.name == "DEFAULT")
            .ok_or_else(|| bad("no DEFAULT category"))? as u8;
        let n = r.count(13)?;
        let mut bmp = vec![(1u32 << default, default); 0x10000];
        let mut astral = Vec::new();
        for _ in 0..n {
            let (lo, hi, mask, d) = (r.u32()?, r.u32()?, r.u32()?, r.u8()?);
            if d as usize >= categories.len() || lo > hi {
                return Err(bad("a character range names no category"));
            }
            for c in lo..=hi.min(0xFFFF) {
                bmp[c as usize] = (mask, d);
            }
            if hi > 0xFFFF {
                astral.push((lo.max(0x10000), hi, mask, d));
            }
        }
        let mut unknown = Vec::with_capacity(categories.len());
        for _ in 0..categories.len() {
            unknown.push((r.u32()?, r.u32()?));
        }
        if r.at != body.len() {
            return Err(bad("unexpected bytes at the end"));
        }
        // every id the segmenter follows must be in range
        let ok = entries.iter().all(|e| {
            (e.right as usize) < size_r
                && (e.left as usize) < size_l
                && [e.pos, e.base, e.parts]
                    .iter()
                    .all(|&s| s == NONE || (s as usize) < strings.len())
        }) && word_ranges
            .iter()
            .chain(&unknown)
            .all(|&(s, n)| (s as usize + n as usize) <= entries.len());
        if !ok {
            return Err(bad("an entry refers past the end of a table"));
        }
        Ok(Dictionary {
            layout,
            strings,
            entries,
            word_ranges,
            fst,
            size_l,
            costs,
            categories,
            bmp,
            astral,
            unknown,
            sha256: hex(&sha2::Sha256::digest(bytes)),
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// The number of words with entries.
    pub fn words(&self) -> usize {
        self.word_ranges.len()
    }

    fn category(&self, c: char) -> (u32, u8) {
        let u = c as u32;
        if u < 0x10000 {
            return self.bmp[u as usize];
        }
        self.astral
            .iter()
            .rev()
            .find(|r| r.0 <= u && u <= r.1)
            .map(|r| (r.2, r.3))
            .unwrap_or(self.bmp[0])
    }

    fn connect(&self, right: u16, left: u16) -> i32 {
        self.costs[right as usize * self.size_l + left as usize] as i32
    }

    /// The entries of the words that start at `text[from..]`: (end byte, entry index).
    fn lookup(&self, text: &str, from: usize, out: &mut Vec<(usize, u32)>) {
        let bytes = text.as_bytes();
        let mut node = self.fst.root();
        let mut acc = 0u64;
        for (i, &b) in bytes[from..].iter().enumerate() {
            let Some(t) = node.find_input(b) else {
                return;
            };
            let t = node.transition(t);
            acc += t.out.value();
            node = self.fst.node(t.addr);
            let end = from + i + 1;
            if node.is_final() && text.is_char_boundary(end) {
                let w = (acc + node.final_output().value()) as usize;
                let (s, n) = self.word_ranges[w];
                out.extend((s..s + n).map(|e| (end, e)));
            }
        }
    }

    /// Cuts `text`, a run of CJK characters, into words: the byte range and entry of
    /// each, in order. `spaced[i]`, when given, says that whitespace came before
    /// character `i` in the source, which makes some words cost more there.
    fn segment(&self, text: &str, spaced: &[bool]) -> Vec<(usize, usize, u32)> {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let n = chars.len();
        if n == 0 {
            return Vec::new();
        }
        let byte_at = |i: usize| chars.get(i).map_or(text.len(), |c| c.0);
        let char_at: std::collections::HashMap<usize, usize> =
            (0..=n).map(|i| (byte_at(i), i)).collect();
        // nodes[i]: the nodes ending at character i: (start, entry, total cost, back)
        // where back indexes nodes[start]; BOS is nodes[0][0]
        #[derive(Clone, Copy)]
        struct Node {
            start: usize,
            entry: u32,
            total: i64,
            back: usize,
        }
        let mut nodes: Vec<Vec<Node>> = vec![Vec::new(); n + 1];
        nodes[0].push(Node {
            start: 0,
            entry: NONE,
            total: 0,
            back: 0,
        });
        let mut found = Vec::new();
        for i in 0..n {
            if nodes[i].is_empty() {
                continue;
            }
            found.clear();
            self.lookup(text, byte_at(i), &mut found);
            let mut cands: Vec<(usize, u32)> =
                found.iter().map(|&(end, e)| (char_at[&end], e)).collect();
            self.unknown_words(&chars, i, !cands.is_empty(), &mut cands);
            for (end, e) in cands {
                let entry = &self.entries[e as usize];
                let mut penalty = self.penalty(&chars[i..end]);
                if spaced.get(i) == Some(&true) {
                    penalty += self.space_penalty(entry);
                }
                let mut best: Option<(i64, usize)> = None;
                for (k, p) in nodes[i].iter().enumerate() {
                    let right = if p.entry == NONE {
                        0
                    } else {
                        self.entries[p.entry as usize].right
                    };
                    let c = p.total + self.connect(right, entry.left) as i64;
                    if best.is_none_or(|b| c < b.0) {
                        best = Some((c, k));
                    }
                }
                let (c, k) = best.expect("a reachable start has a node");
                nodes[end].push(Node {
                    start: i,
                    entry: e,
                    total: c + entry.cost as i64 + penalty,
                    back: k,
                });
            }
        }
        // EOS: the node at n with the lowest total including its connection to EOS
        let mut best: Option<(i64, usize)> = None;
        for (k, p) in nodes[n].iter().enumerate() {
            let c = p.total + self.connect(self.entries[p.entry as usize].right, 0) as i64;
            if best.is_none_or(|b| c < b.0) {
                best = Some((c, k));
            }
        }
        let Some((_, mut k)) = best else {
            // unreachable with a DEFAULT category, which always yields a word
            return vec![];
        };
        let mut out = Vec::new();
        let mut end = n;
        while end > 0 {
            let node = nodes[end][k];
            out.push((byte_at(node.start), byte_at(end), node.entry));
            k = node.back;
            end = node.start;
        }
        out.reverse();
        out
    }

    /// Kuromoji's search mode: a long word of Han characters, or a longer one of others,
    /// costs more, so that compounds split.
    fn penalty(&self, word: &[(usize, char)]) -> i64 {
        if self.layout != Layout::Ipadic {
            return 0;
        }
        let len = word.len() as i64;
        if len > 2 {
            if word.iter().all(|(_, c)| super::standard::is_han(*c as u32)) {
                return (len - 2) * 3000;
            }
            if len > 7 {
                return (len - 7) * 1700;
            }
        }
        0
    }

    /// Nori's cost of a word after whitespace when it can only follow another word:
    /// an ending, a particle, a copula or a suffix.
    fn space_penalty(&self, entry: &Entry) -> i64 {
        if self.layout != Layout::KoDic || entry.pos == NONE {
            return 0;
        }
        let pos = self.strings[entry.pos as usize].as_str();
        let first = pos.split('+').next().unwrap_or(pos);
        if first.starts_with('E')
            || first.starts_with('J')
            || matches!(first, "VCP" | "XSA" | "XSN" | "XSV")
        {
            3000
        } else {
            0
        }
    }

    /// The words of `text` with its whitespace taken out, so that the lattice runs
    /// across it as Nori's does: (byte range in `text`, entry).
    fn segment_spaced(&self, text: &str) -> Vec<(usize, usize, u32)> {
        let mut compact = String::with_capacity(text.len());
        // per byte of `compact`, its byte in `text`
        let mut at: Vec<usize> = Vec::with_capacity(text.len());
        let mut spaced = Vec::new();
        let mut space = false;
        for (i, c) in text.char_indices() {
            if c.is_whitespace() {
                space = true;
                continue;
            }
            compact.push(c);
            at.extend(i..i + c.len_utf8());
            spaced.push(std::mem::take(&mut space));
        }
        self.segment(&compact, &spaced)
            .into_iter()
            .map(|(a, b, e)| (at[a], at[b - 1] + 1, e))
            .collect()
    }

    /// The unknown words that start at character `i`, as MeCab makes them from
    /// `char.def` and `unk.def`.
    fn unknown_words(
        &self,
        chars: &[(usize, char)],
        i: usize,
        known: bool,
        out: &mut Vec<(usize, u32)>,
    ) {
        let (mask, cat) = self.category(chars[i].1);
        let c = &self.categories[cat as usize];
        if known && !c.invoke {
            return;
        }
        let (s, n) = self.unknown[cat as usize];
        let add = |end: usize, out: &mut Vec<(usize, u32)>| {
            out.extend((s..s + n).map(|e| (end, e)));
        };
        let kind = |j: usize| j < chars.len() && self.category(chars[j].1).0 & mask != 0;
        let before = out.len();
        let mut group_end = None;
        if c.group {
            let mut j = i + 1;
            while kind(j) && j - i < MAX_GROUP {
                j += 1;
            }
            add(j, out);
            group_end = Some(j);
        }
        for j in (i + 1..).take(c.length as usize) {
            if Some(j) != group_end {
                add(j, out);
            }
            if !kind(j) {
                break;
            }
        }
        if out.len() == before && !known {
            add(i + 1, out);
        }
    }

    /// The words of a CJK run as tokens: (byte range, text), after the analyzer's
    /// part-of-speech filter, base forms and Korean compounds.
    fn words_of(&self, text: &str) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        let words = if self.layout == Layout::KoDic {
            self.segment_spaced(text)
        } else {
            self.segment(text, &[])
        };
        for (from, to, e) in words {
            let entry = &self.entries[e as usize];
            let surface = &text[from..to];
            let pos = (entry.pos != NONE).then(|| self.strings[entry.pos as usize].as_str());
            match self.layout {
                Layout::Ipadic => {
                    if pos.is_some_and(ja_stop) {
                        continue;
                    }
                    let word = if entry.base != NONE {
                        self.strings[entry.base as usize].clone()
                    } else {
                        surface.to_string()
                    };
                    out.push((from, to, katakana_stem(word)));
                }
                Layout::KoDic => {
                    if entry.parts != NONE
                        && let Some(parts) = compound_parts(&self.strings[entry.parts as usize])
                        && parts.iter().map(|(s, _)| *s).collect::<String>() == surface
                    {
                        let mut at = from;
                        for (s, p) in parts {
                            let end = at + s.len();
                            if !ko_stop(p) {
                                out.push((at, end, s.to_string()));
                            }
                            at = end;
                        }
                        continue;
                    }
                    // an inflected word: each morpheme over the whole word, as Nori
                    // gives them
                    if entry.base != NONE
                        && let Some(parts) = compound_parts(&self.strings[entry.base as usize])
                    {
                        for (s, p) in parts {
                            if !ko_stop(p) {
                                out.push((from, to, s.to_string()));
                            }
                        }
                        continue;
                    }
                    if pos.is_some_and(ko_stop) {
                        continue;
                    }
                    out.push((from, to, surface.to_string()));
                }
                Layout::Frequency => out.push((from, to, surface.to_string())),
            }
        }
        out
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The parts of speech Kuromoji's analyzer drops by default: conjunctions, particles,
/// auxiliary verbs, symbols other than letters, interjections of other kinds, fillers
/// and non-verbal sounds.
fn ja_stop(pos: &str) -> bool {
    pos == "接続詞"
        || pos.starts_with("助詞")
        || pos == "助動詞"
        || matches!(
            pos,
            "記号"
                | "記号-一般"
                | "記号-読点"
                | "記号-句点"
                | "記号-空白"
                | "記号-括弧開"
                | "記号-括弧閉"
        )
        || pos == "その他-間投"
        || pos == "フィラー"
        || pos == "非言語音"
}

/// The parts of speech Nori's analyzer drops by default: endings, interjections,
/// particles, adverbs and determiners, symbols, affixes and unknown or non-Korean ones.
/// A tag joined from several (`VV+EC`) is judged by its first.
fn ko_stop(pos: &str) -> bool {
    let first = pos.split('+').next().unwrap_or(pos);
    first.starts_with('E')
        || first.starts_with('J')
        || matches!(
            first,
            "IC" | "MAG"
                | "MAJ"
                | "MM"
                | "SP"
                | "SSC"
                | "SSO"
                | "SC"
                | "SE"
                | "XPN"
                | "XSA"
                | "XSN"
                | "XSV"
                | "UNA"
                | "NA"
                | "VSV"
        )
}

/// A Katakana word of four characters or more without a final `ー`, as Kuromoji's
/// Katakana stem filter makes it.
fn katakana_stem(word: String) -> String {
    let n = word.chars().count();
    if n >= 4
        && word.ends_with('ー')
        && word
            .chars()
            .all(|c| matches!(c as u32, 0x30A0..=0x30FF | 0x31F0..=0x31FF))
    {
        let mut w = word;
        w.pop();
        return w;
    }
    word
}

/// A mecab-ko-dic expression `a/NNG/*+b/NNG/*` as (surface, part of speech) pairs.
fn compound_parts(expr: &str) -> Option<Vec<(&str, &str)>> {
    expr.split('+')
        .map(|p| {
            let mut f = p.split('/');
            Some((f.next()?, f.next()?))
        })
        .collect()
}

// ------------------------------------------------------------------- installed files

static DIR: RwLock<Option<PathBuf>> = RwLock::new(None);
#[allow(clippy::type_complexity)]
static LOADED: Mutex<Vec<(PathBuf, std::time::SystemTime, Arc<Dictionary>)>> =
    Mutex::new(Vec::new());

/// Sets the directory of installed dictionaries for this process. Without it, the
/// directory is `$SPARKLES_TEXT_DICTS`, or `data/text-dicts`.
pub fn set_dir(dir: impl Into<PathBuf>) {
    *DIR.write().unwrap() = Some(dir.into());
}

/// The directory of installed dictionaries.
pub fn dir() -> PathBuf {
    if let Some(d) = DIR.read().unwrap().clone() {
        return d;
    }
    std::env::var_os("SPARKLES_TEXT_DICTS")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("data/text-dicts"), PathBuf::from)
}

/// The compiled file of an analyzer's dictionary in [`dir`].
pub fn path_of(name: &str) -> PathBuf {
    dir().join(format!("{name}.sdict"))
}

/// The installed dictionary of an analyzer (`ja-dict`, …), read once per file version.
pub fn load(name: &str) -> Result<Arc<Dictionary>> {
    let path = path_of(name);
    let modified = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map_err(|_| {
            bad(format!(
                "{name} needs its dictionary at {}; install it with `sparkles text-dict`",
                path.display()
            ))
        })?;
    let mut loaded = LOADED.lock().unwrap();
    if let Some((_, _, d)) = loaded.iter().find(|(p, m, _)| *p == path && *m == modified) {
        return Ok(d.clone());
    }
    let bytes = std::fs::read(&path).map_err(|e| bad(format!("{}: {e}", path.display())))?;
    let d = Arc::new(Dictionary::from_bytes(&bytes)?);
    if Some(d.layout) != Layout::of(name) {
        return Err(bad(format!(
            "{} is not a dictionary for {name}",
            path.display()
        )));
    }
    loaded.retain(|(p, ..)| *p != path);
    loaded.push((path, modified, d.clone()));
    Ok(d)
}

/// Writes a compiled dictionary as an analyzer's installed one, replacing any, and
/// returns its SHA-256.
pub fn install(name: &str, bytes: &[u8]) -> Result<String> {
    let d = Dictionary::from_bytes(bytes)?;
    if Some(d.layout) != Layout::of(name) {
        return Err(bad(format!("the dictionary's layout does not suit {name}")));
    }
    let path = path_of(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::store::write_atomic(&path, bytes)?;
    Ok(d.sha256)
}

// ------------------------------------------------------------------------ tokenizer

/// The tokenizer of the dictionary analyzers.
#[derive(Clone)]
pub(crate) struct DictTokenizer(pub(crate) Arc<Dictionary>);

impl Tokenizer for DictTokenizer {
    type TokenStream<'a> = super::cjk::CjkTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> super::cjk::CjkTokenStream {
        super::cjk::CjkTokenStream::of(tokens(&self.0, text))
    }
}

/// The tokens of `text`: its words as Lucene's tokenizer finds them, with each run of
/// adjacent CJK words segmented by the dictionary.
fn tokens(dict: &Dictionary, text: &str) -> Vec<Token> {
    use super::standard::{Kind, typed_tokens};
    let mut out: Vec<Token> = Vec::new();
    let push = |out: &mut Vec<Token>, text: String, from: usize, to: usize| {
        let position = out.len();
        out.push(Token {
            offset_from: from,
            offset_to: to,
            position,
            text,
            position_length: 1,
        });
    };
    // a run of adjacent CJK words: its start and end in `text`
    let mut run: Option<(usize, usize)> = None;
    let flush = |out: &mut Vec<Token>, run: &mut Option<(usize, usize)>| {
        let Some((from, to)) = run.take() else {
            return;
        };
        // fold widths, a voiced mark joining the kana before it, and keep the source
        // range of each folded character
        let mut chars: Vec<(char, usize, usize)> = Vec::new();
        for (i, c) in text[from..to].char_indices() {
            let f = super::cjk::fold(c);
            let end = from + i + c.len_utf8();
            if matches!(f as u32, 0x3099 | 0x309A)
                && let Some(last) = chars.last_mut()
                && let Some(v) = super::cjk::voiced(last.0, f)
            {
                last.0 = v;
                last.2 = end;
                continue;
            }
            chars.push((f, from + i, end));
        }
        let mut folded = String::new();
        // per byte of `folded`, the source range of its character
        let mut origin: Vec<(usize, usize)> = Vec::new();
        for (c, s, e) in chars {
            folded.push(c);
            origin.extend(std::iter::repeat_n((s, e), c.len_utf8()));
        }
        for (a, b, word) in dict.words_of(&folded) {
            push(out, word, origin[a].0, origin[b - 1].1);
        }
    };
    for (t, kind) in typed_tokens(text) {
        if kind == Kind::Alphanum {
            flush(&mut out, &mut run);
            push(
                &mut out,
                super::cjk::fold_word(&t.text),
                t.offset_from,
                t.offset_to,
            );
            continue;
        }
        let joins = |to: usize| {
            to == t.offset_from
                || (dict.layout == Layout::KoDic
                    && text[to..t.offset_from].chars().all(char::is_whitespace))
        };
        match run {
            Some((f, to)) if joins(to) => run = Some((f, t.offset_to)),
            _ => {
                flush(&mut out, &mut run);
                run = Some((t.offset_from, t.offset_to));
            }
        }
    }
    flush(&mut out, &mut run);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a small MeCab dictionary in IPADIC's layout to `dir`.
    pub(crate) fn write_ipadic(dir: &Path) {
        // ids: 0 BOS/EOS, 1 noun, 2 particle, 3 verb, 4 suffix
        std::fs::write(
            dir.join("lex.csv"),
            "東京,1,1,3000,名詞,固有名詞,地域,一般,*,*,東京,トウキョウ,トーキョー\n\
             京都,1,1,2500,名詞,固有名詞,地域,一般,*,*,京都,キョウト,キョート\n\
             東,1,1,6000,名詞,一般,*,*,*,*,東,ヒガシ,ヒガシ\n\
             都,4,4,2000,名詞,接尾,地域,*,*,*,都,ト,ト\n\
             都庁,1,1,3000,名詞,一般,*,*,*,*,都庁,トチョウ,トチョー\n\
             東京都庁,1,1,1000,名詞,固有名詞,組織,*,*,*,東京都庁,トウキョウトチョウ,トーキョートチョー\n\
             に,2,2,500,助詞,格助詞,一般,*,*,*,に,ニ,ニ\n\
             と,2,2,500,助詞,並立助詞,*,*,*,*,と,ト,ト\n\
             住ん,3,3,4000,動詞,自立,*,*,五段・マ行,連用タ接続,住む,スン,スン\n\
             で,2,2,500,助詞,接続助詞,*,*,*,*,で,デ,デ\n\
             いる,3,3,1500,動詞,非自立,*,*,一段,基本形,いる,イル,イル\n",
        )
        .unwrap();
        let mut m = String::from("5 5\n");
        for r in 0..5 {
            for l in 0..5 {
                // a particle after a noun and a verb after a particle are cheap
                let c = match (r, l) {
                    (1, 2) | (4, 2) | (2, 3) | (3, 2) | (1, 4) => -500,
                    _ => 100,
                };
                m.push_str(&format!("{r} {l} {c}\n"));
            }
        }
        std::fs::write(dir.join("matrix.def"), m).unwrap();
        std::fs::write(
            dir.join("char.def"),
            "DEFAULT 0 1 0\nSPACE 0 1 0\nKANJI 0 0 2\nHIRAGANA 0 1 2\nKATAKANA 1 1 2\n\
             0x0020 SPACE\n0x3041..0x309F HIRAGANA\n0x30A1..0x30FF KATAKANA\n\
             0x30FC KATAKANA\n0x4E00..0x9FFF KANJI\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("unk.def"),
            "DEFAULT,1,1,9000,名詞,一般,*,*,*,*,*\nSPACE,2,2,9000,記号,空白,*,*,*,*,*\n\
             KANJI,1,1,9000,名詞,一般,*,*,*,*,*\nHIRAGANA,1,1,9000,名詞,一般,*,*,*,*,*\n\
             KATAKANA,1,1,4000,名詞,一般,*,*,*,*,*\n",
        )
        .unwrap();
    }

    /// Writes a small MeCab dictionary in mecab-ko-dic's layout to `dir`.
    pub(crate) fn write_kodic(dir: &Path) {
        std::fs::write(
            dir.join("lex.csv"),
            "서울,1,1,1000,NNP,지명,T,서울,*,*,*,*\n\
             은,2,2,500,JX,*,T,은,*,*,*,*\n\
             썸남,1,1,1500,NNG,*,T,썸남,Compound,*,*,썸/NNG/*+남/NNG/*\n\
             이,2,2,500,JKS,*,F,이,*,*,*,*\n\
             입니다,2,2,800,VCP+EF,*,F,입니다,Inflect,VCP,EF,이/VCP/*+ㅂ니다/EF/*\n",
        )
        .unwrap();
        std::fs::write(dir.join("matrix.def"), "3 3\n0 1 0\n1 2 -200\n2 0 0\n").unwrap();
        std::fs::write(
            dir.join("char.def"),
            "DEFAULT 0 1 0\nHANGUL 0 1 2\n0xAC00..0xD7A3 HANGUL\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("unk.def"),
            "DEFAULT,1,1,9000,SY,*,*,*,*,*,*,*\nHANGUL,1,1,9000,UNKNOWN,*,*,*,*,*,*,*\n",
        )
        .unwrap();
    }

    pub(crate) fn write_frequency(file: &Path) {
        std::fs::write(
            file,
            "研究 2000 vn\n研究生 300 n\n生命 1500 n\n起源 800 n\n命 20 n\n",
        )
        .unwrap();
    }

    fn texts(d: &Dictionary, s: &str) -> Vec<String> {
        tokens(d, s).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn japanese_words_parts_of_speech_and_base_forms() {
        let tmp = tempfile::tempdir().unwrap();
        write_ipadic(tmp.path());
        let d = Dictionary::from_bytes(&compile(tmp.path(), Layout::Ipadic).unwrap()).unwrap();
        // particles go, and the inflected verb becomes its base form
        assert_eq!(
            texts(&d, "東京都に住んでいる"),
            ["東京", "都", "住む", "いる"]
        );
        // the search mode penalty splits the four-character compound
        assert_eq!(texts(&d, "東京都庁"), ["東京", "都庁"]);
        // an unknown Katakana run is one word, without its final ー
        assert_eq!(texts(&d, "コンピューター"), ["コンピュータ"]);
        // half-width Katakana is folded first, and offsets point into the original
        let t = tokens(&d, "ｺﾝﾋﾟｭｰﾀｰと東京");
        assert_eq!(t[0].text, "コンピュータ");
        assert_eq!((t[0].offset_from, t[0].offset_to), (0, "ｺﾝﾋﾟｭｰﾀｰ".len()));
        assert_eq!(t[1].text, "東京");
        assert_eq!(
            (t[1].offset_from, t[1].offset_to),
            ("ｺﾝﾋﾟｭｰﾀｰと".len(), "ｺﾝﾋﾟｭｰﾀｰと東京".len())
        );
        // other words pass through
        assert_eq!(texts(&d, "Tokyo 2024 東京"), ["Tokyo", "2024", "東京"]);
    }

    #[test]
    fn korean_compounds_and_particles() {
        let tmp = tempfile::tempdir().unwrap();
        write_kodic(tmp.path());
        let d = Dictionary::from_bytes(&compile(tmp.path(), Layout::KoDic).unwrap()).unwrap();
        assert_eq!(texts(&d, "서울은"), ["서울"]);
        assert_eq!(texts(&d, "썸남이"), ["썸", "남"]);
        // an inflected word becomes its morphemes, each over the whole word, and the
        // ending goes
        let t = tokens(&d, "서울입니다");
        assert_eq!(
            t.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["서울", "이"]
        );
        assert_eq!((t[1].offset_from, t[1].offset_to), (6, 15));
        // the words of a sentence are segmented together across its spaces, with the
        // offsets of the source
        let t = tokens(&d, "서울은  썸남이");
        assert_eq!(
            t.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["서울", "썸", "남"]
        );
        assert_eq!((t[1].offset_from, t[2].offset_to), (11, 17));
    }

    #[test]
    fn korean_words_after_a_space_cost_more_when_they_follow_another() {
        let tmp = tempfile::tempdir().unwrap();
        write_kodic(tmp.path());
        let d = Dictionary::from_bytes(&compile(tmp.path(), Layout::KoDic).unwrap()).unwrap();
        let entry = |pos: &str| {
            *d.entries
                .iter()
                .find(|e| e.pos != NONE && d.strings[e.pos as usize] == pos)
                .unwrap()
        };
        assert_eq!(d.space_penalty(&entry("JX")), 3000);
        assert_eq!(d.space_penalty(&entry("VCP+EF")), 3000);
        assert_eq!(d.space_penalty(&entry("NNP")), 0);
    }

    #[test]
    fn chinese_words_by_frequency() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("dict.txt");
        write_frequency(&f);
        let d = Dictionary::from_bytes(&compile(&f, Layout::Frequency).unwrap()).unwrap();
        assert_eq!(texts(&d, "研究生命起源"), ["研究", "生命", "起源"]);
        // characters the list lacks stand alone
        assert_eq!(texts(&d, "研究猫"), ["研究", "猫"]);
        assert_eq!(d.words(), 5);
    }

    /// Segments the lines of `$SPARKLES_SEGMENT_IN` with the installed dictionary
    /// `$SPARKLES_DICT` and writes them to `$SPARKLES_SEGMENT_OUT` (for checking a real
    /// dictionary by hand).
    #[test]
    #[ignore]
    fn segment_file() {
        let d = load(&std::env::var("SPARKLES_DICT").unwrap()).unwrap();
        let input = std::fs::read_to_string(std::env::var("SPARKLES_SEGMENT_IN").unwrap()).unwrap();
        let out: Vec<String> = input.lines().map(|l| texts(&d, l).join(" ")).collect();
        std::fs::write(
            std::env::var("SPARKLES_SEGMENT_OUT").unwrap(),
            out.join("\n") + "\n",
        )
        .unwrap();
    }

    #[test]
    fn the_format_is_versioned_and_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("dict.txt");
        write_frequency(&f);
        let bytes = compile(&f, Layout::Frequency).unwrap();
        let mut newer = bytes.clone();
        newer[8..12].copy_from_slice(&2u32.to_le_bytes());
        let e = Dictionary::from_bytes(&newer).err().unwrap().to_string();
        assert!(e.contains("format version 2"), "{e}");
        let mut damaged = bytes.clone();
        let n = damaged.len();
        damaged[n / 2] ^= 1;
        let e = Dictionary::from_bytes(&damaged).err().unwrap().to_string();
        assert!(e.contains("damaged"), "{e}");
        assert!(Dictionary::from_bytes(b"not a dictionary at all, really not").is_err());
        // the same source compiles to the same bytes
        assert_eq!(bytes, compile(&f, Layout::Frequency).unwrap());
        // a malformed source is refused with its line
        std::fs::write(&f, "研究 lots\n").unwrap();
        let e = compile(&f, Layout::Frequency).err().unwrap().to_string();
        assert!(e.contains(":1:"), "{e}");
    }
}
