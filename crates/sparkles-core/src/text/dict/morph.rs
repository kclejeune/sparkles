//! The segmentation of Lucene's Kuromoji (`ja-dict`) and Nori (`ko-dict`) tokenizers, and
//! the filters of their analyzers.
//!
//! Both tokenizers run the Viterbi search over the lattice of every word of the
//! dictionary, and of every unknown word that `char.def` makes, that starts at each
//! character of the whole text, with the costs of the words and of each pair of adjacent
//! ones. They commit to a prefix of the best path wherever a single path is left alive,
//! and they drop words that start with punctuation.
//!
//! * Kuromoji runs on the text with its widths folded. An unknown word is one character,
//!   or the run of characters of its category, punctuation or not as the first is. In
//!   search mode, where a word on the best path is long (more than two Han characters,
//!   or more than seven other characters), the lattice under it is scored again with a
//!   penalty on long words, and the best path under it is taken instead when it costs no
//!   more than the long word with its penalty. The analyzer replaces inflected words by
//!   their base form, drops the parts of speech of `stoptags.txt` and the words of
//!   `stopwords.txt`, removes the final `ー` of a Katakana word of four characters or
//!   more and lowercases.
//! * Nori skips one space before a word and charges 3,000 for a word after a space that
//!   cannot start one (an ending, a particle, a copula or a suffix). An unknown word is
//!   one character, or the run of characters of one script, punctuation or digits or
//!   not as the first is. A compound is replaced by its parts and an inflected or
//!   preanalyzed word by its morphemes. The analyzer drops the parts of speech of its
//!   default stop tags, replaces a Hanja word by its Hangul reading and lowercases.
//!
//! The tokenizers read the text as UTF-16 code units, and Java's character properties
//! of Unicode 15.0 ([`super::java_tables`]). A character above U+FFFF counts as two
//! units, and has the properties of its high surrogate, as it does in Java.
//!
//! This module is a port of `JapaneseTokenizer`, `KoreanTokenizer` and the filters of
//! `JapaneseAnalyzer` and `KoreanAnalyzer` in Lucene 9.11.1 (Apache-2.0, Copyright The
//! Apache Software Foundation). Kuromoji's `stoptags.txt` and `stopwords.txt` are copied
//! from the same release.

use super::java_tables::RANGES;
use super::{Dictionary, Layout, NONE};
use tantivy::tokenizer::Token;

const PUNCT: u16 = 1;
const SPACE_SEPARATOR: u16 = 2;
const NON_SPACING_MARK: u16 = 4;
const DIGIT: u16 = 8;
const SCRIPT_COMMON: u16 = 1;
const SCRIPT_INHERITED: u16 = 2;
/// Kuromoji's and Nori's longest unknown word, in UTF-16 code units.
const MAX_UNKNOWN: u32 = 1024;
/// How far, in UTF-16 code units, the tokenizers search past the committed prefix
/// before they commit to the cheapest path so far.
const MAX_BACKTRACE_GAP: u32 = 1024;

/// The Java properties of `c`, or of its high surrogate above U+FFFF.
fn java(c: char) -> u16 {
    let u = c as u32;
    let unit = if u > 0xFFFF {
        0xD800 + ((u - 0x10000) >> 10)
    } else {
        u
    } as u16;
    let i = RANGES.partition_point(|r| r.1 < unit);
    RANGES.get(i).map_or(0, |r| r.2)
}

/// A character of the text with what the tokenizers ask of it.
#[derive(Clone, Copy)]
struct Ch {
    /// its byte offset in the analyzed text
    at: usize,
    /// its `char.def` category, as Lucene's builder keeps it
    class: u8,
    java: u16,
    units: u8,
    /// its offset in UTF-16 code units
    unit: u32,
}

impl Ch {
    fn punct(self) -> bool {
        self.java & PUNCT != 0
    }
    fn script(self) -> u16 {
        self.java >> 8
    }
}

/// A node of the lattice: a word that ends at a position, with the cost of the best path
/// to its end and the node before it.
#[derive(Clone, Copy)]
struct Node {
    cost: i64,
    right: u16,
    /// where the node before it ends (Nori: before the skipped space)
    back: usize,
    /// where the word starts (after Nori's skipped space)
    start: usize,
    /// the node before it, at `back`
    back_index: usize,
    entry: u32,
}

/// A word of the best path: its characters and its dictionary entry.
type Word = (usize, usize, u32);

struct Lattice<'a> {
    d: &'a Dictionary,
    text: &'a str,
    /// the length of `text` in UTF-16 code units
    units: u32,
    cs: Vec<Ch>,
    /// the character at each byte offset of `text` where one starts
    char_at: Vec<u32>,
    nodes: Vec<Vec<Node>>,
    /// the furthest position a node ends at so far
    max_end: usize,
    /// where the last committed prefix ends
    committed: usize,
    words: Vec<Word>,
    found: Vec<(usize, u32)>,
}

impl<'a> Lattice<'a> {
    fn new(d: &'a Dictionary, text: &'a str) -> Self {
        let mut cs = Vec::with_capacity(text.len());
        let mut char_at = vec![u32::MAX; text.len() + 1];
        let mut unit = 0;
        for (at, c) in text.char_indices() {
            char_at[at] = cs.len() as u32;
            cs.push(Ch {
                at,
                class: d.lucene_class(c),
                java: java(c),
                units: c.len_utf16() as u8,
                unit,
            });
            unit += c.len_utf16() as u32;
        }
        let units = unit;
        char_at[text.len()] = cs.len() as u32;
        let mut nodes = vec![Vec::new(); cs.len() + 1];
        nodes[0].push(Node {
            cost: 0,
            right: 0,
            back: usize::MAX,
            start: 0,
            back_index: 0,
            entry: NONE,
        });
        Lattice {
            d,
            text,
            units,
            cs,
            char_at,
            nodes,
            max_end: 0,
            committed: 0,
            words: Vec::new(),
            found: Vec::new(),
        }
    }

    fn byte(&self, i: usize) -> usize {
        self.cs.get(i).map_or(self.text.len(), |c| c.at)
    }

    /// The UTF-16 offset of position `i`.
    fn unit(&self, i: usize) -> u32 {
        self.cs.get(i).map_or(self.units, |c| c.unit)
    }

    /// Whether the search has gone `MAX_BACKTRACE_GAP` units past the committed prefix.
    fn gap(&self, pos: usize) -> bool {
        self.unit(pos) - self.unit(self.committed) >= MAX_BACKTRACE_GAP
    }

    /// Keeps only the cheapest node at or after `pos`, as Lucene does before it forces a
    /// backtrace, and returns its position, where it is now the only node.
    fn prune_to_cheapest(&mut self, pos: usize) -> usize {
        let mut least: Option<(i64, usize, usize)> = None;
        for p in pos..=self.max_end {
            for (k, n) in self.nodes[p].iter().enumerate() {
                if least.is_none_or(|l| n.cost < l.0) {
                    least = Some((n.cost, p, k));
                }
            }
        }
        // a node always ends at `pos`, which the caller checked
        let (_, at, k) = least.expect("a live path");
        for p in pos..=self.max_end {
            if p == at {
                let n = self.nodes[p][k];
                self.nodes[p].clear();
                self.nodes[p].push(n);
            } else {
                self.nodes[p] = Vec::new();
            }
        }
        at
    }

    /// Frees the nodes of the positions before the committed prefix's end, which no path
    /// reads again.
    fn release(&mut self, from: usize) {
        for p in from..self.committed {
            self.nodes[p] = Vec::new();
        }
    }

    /// The dictionary words that start at character `i`: (end, entry).
    fn known(&mut self, i: usize) -> Vec<(usize, u32)> {
        self.found.clear();
        self.d.lookup(self.text, self.cs[i].at, &mut self.found);
        self.found
            .iter()
            .map(|&(end, e)| (self.char_at[end] as usize, e))
            .collect()
    }

    /// The entries of the unknown words of category `class`.
    fn unknown(&self, class: u8) -> std::ops::Range<u32> {
        let (s, n) = self.d.unknown[class as usize];
        s..s + n
    }

    /// The best node at `from` for a word with connection id `left`, and the cost
    /// through it.
    fn best_before(&self, from: usize, left: u16, extra: i64) -> (i64, usize) {
        let mut best = (i64::MAX, 0);
        for (k, n) in self.nodes[from].iter().enumerate() {
            let c = n.cost + self.d.connect(n.right, left) as i64 + extra;
            if c < best.0 {
                best = (c, k);
            }
        }
        best
    }

    /// The best node at the end of the text, with the cost of ending there.
    fn best_end(&self) -> usize {
        let n = self.cs.len();
        let mut best = (i64::MAX, 0);
        for (k, node) in self.nodes[n].iter().enumerate() {
            let c = node.cost + self.d.connect(node.right, 0) as i64;
            if c < best.0 {
                best = (c, k);
            }
        }
        best.1
    }

    /// Whether the search may commit to the best path up to `pos`: one node ends there
    /// and none after it.
    fn frontier(&self, pos: usize) -> bool {
        pos > self.committed && self.nodes[pos].len() == 1 && self.max_end <= pos
    }

    // ------------------------------------------------------------------- Kuromoji

    /// The words of the best path of Kuromoji's search mode.
    fn kuromoji(mut self) -> Vec<Word> {
        let n = self.cs.len();
        let mut pos = 0;
        while pos < n {
            if self.nodes[pos].is_empty() {
                pos += 1;
                continue;
            }
            if self.frontier(pos) {
                self.backtrace_ja(pos, 0);
                self.nodes[pos][0].cost = 0;
            }
            if self.gap(pos) {
                // Lucene's safety: commit to the cheapest path so far, which may not be
                // the best path of the whole text
                let at = self.prune_to_cheapest(pos);
                self.backtrace_ja(at, 0);
                for n in &mut self.nodes[at] {
                    n.cost = 0;
                }
                pos = at;
                continue;
            }
            let known = self.known(pos);
            for &(end, e) in &known {
                self.add_ja(pos, end, e, false);
            }
            let first = self.cs[pos];
            let cat = &self.d.categories[first.class as usize];
            if known.is_empty() || cat.invoke {
                // Lucene counts the length in UTF-16 units, and a run whose limit falls
                // inside a surrogate pair ends before that pair here
                let (mut len, mut units) = (1, first.units as u32);
                if cat.group {
                    while pos + len < n
                        && units + self.cs[pos + len].units as u32 <= MAX_UNKNOWN
                        && self.cs[pos + len].class == first.class
                        && self.cs[pos + len].punct() == first.punct()
                    {
                        units += self.cs[pos + len].units as u32;
                        len += 1;
                    }
                }
                for e in self.unknown(first.class) {
                    self.add_ja(pos, pos + len, e, false);
                }
            }
            pos += 1;
        }
        if n > 0 {
            let k = self.best_end();
            self.backtrace_ja(n, k);
        }
        self.words
    }

    fn add_ja(&mut self, from: usize, end: usize, e: u32, penalize: bool) {
        let entry = self.d.entries[e as usize];
        let (cost, k) = self.best_before(from, entry.left, 0);
        let mut cost = cost + entry.cost as i64;
        if penalize {
            cost += self.penalty(from, end);
        }
        // Kuromoji's dictionary keeps one id, the left one
        self.nodes[end].push(Node {
            cost,
            right: entry.left,
            back: from,
            start: from,
            back_index: k,
            entry: e,
        });
        self.max_end = self.max_end.max(end);
    }

    /// Kuromoji's search mode penalty of the word from `from` to `end`.
    fn penalty(&self, from: usize, end: usize) -> i64 {
        let word = &self.cs[from..end];
        let len: i64 = word.iter().map(|c| c.units as i64).sum();
        if len <= 2 {
            return 0;
        }
        if word.iter().all(|c| self.d.kanji.contains(&c.class)) {
            (len - 2) * 3000
        } else if len > 7 {
            (len - 7) * 1700
        } else {
            0
        }
    }

    /// Follows the best path back from node `k` at `end` to the committed prefix, and
    /// appends its words.
    fn backtrace_ja(&mut self, end: usize, k: usize) {
        if end == self.committed {
            return;
        }
        let mut words = Vec::new();
        let mut pos = end;
        let mut best = k;
        // the start of a long word replaced by the path under it
        let mut alternative: Option<usize> = None;
        // the left id of the word after this one
        let mut next_left: Option<u16> = None;
        while pos > self.committed {
            let node = self.nodes[pos][best];
            let (mut back, mut e, mut next) = (node.back, node.entry, node.back_index);
            if alternative.is_none() {
                let penalty = self.penalty(back, pos);
                if penalty > 0 {
                    let right = self.d.entries[e as usize].left;
                    let mut max_cost = node.cost + penalty;
                    if let Some(l) = next_left {
                        max_cost += self.d.connect(right, l) as i64;
                    }
                    self.rescore(back, pos, node.back_index);
                    let mut least: Option<(i64, usize)> = None;
                    for (i, n) in self.nodes[pos].iter().enumerate() {
                        let mut c = n.cost;
                        if let Some(l) = next_left {
                            c += self.d.connect(n.right, l) as i64;
                        }
                        if least.is_none_or(|x| c < x.0) {
                            least = Some((c, i));
                        }
                    }
                    if let Some((c, i)) = least
                        && c <= max_cost
                        && self.nodes[pos][i].back != back
                    {
                        alternative = Some(back);
                        let n = self.nodes[pos][i];
                        (back, e, next) = (n.back, n.entry, n.back_index);
                    }
                }
            }
            if alternative.is_some_and(|s| s >= back) {
                alternative = None;
            }
            if !self.cs[back].punct() {
                words.push((back, pos, e));
            }
            next_left = Some(self.d.entries[e as usize].left);
            pos = back;
            best = next;
        }
        let from = self.committed;
        self.committed = end;
        self.release(from);
        words.reverse();
        self.words.extend(words);
    }

    /// Scores the lattice from `start` to `end` again with the penalty on long words,
    /// keeping only the nodes that start at or after `start` and continuing from node
    /// `start_index` at `start` (Kuromoji's `pruneAndRescore`).
    fn rescore(&mut self, start: usize, end: usize, start_index: usize) {
        let mut forward: Vec<Vec<(usize, u32)>> = vec![Vec::new(); end - start];
        for p in (start + 1..=end).rev() {
            for n in &self.nodes[p] {
                if n.back != usize::MAX && n.back >= start {
                    forward[n.back - start].push((p, n.entry));
                }
            }
            self.nodes[p].clear();
        }
        for p in start..end {
            let arcs = std::mem::take(&mut forward[p - start]);
            if self.nodes[p].is_empty() {
                continue;
            }
            if p == start {
                let s = self.nodes[start][start_index];
                let right = if start == 0 {
                    0
                } else {
                    self.d.entries[s.entry as usize].left
                };
                for (to, e) in arcs {
                    let entry = self.d.entries[e as usize];
                    let cost = s.cost
                        + entry.cost as i64
                        + self.d.connect(right, entry.left) as i64
                        + self.penalty(p, to);
                    self.nodes[to].push(Node {
                        cost,
                        right: entry.left,
                        back: p,
                        start: p,
                        back_index: start_index,
                        entry: e,
                    });
                }
            } else {
                for (to, e) in arcs {
                    self.add_ja(p, to, e, true);
                }
            }
        }
    }

    // ----------------------------------------------------------------------- Nori

    /// The words of Nori's best path: (start after the skipped space, end, entry).
    fn nori(mut self) -> Vec<Word> {
        let n = self.cs.len();
        let mut pos = 0;
        while pos < n {
            if self.nodes[pos].is_empty() {
                pos += 1;
                continue;
            }
            if self.frontier(pos) {
                self.backtrace_ko(pos, 0);
                self.nodes[pos][0].cost = 0;
            }
            if self.gap(pos) {
                let at = self.prune_to_cheapest(pos);
                self.backtrace_ko(at, 0);
                for n in &mut self.nodes[at] {
                    n.cost = 0;
                }
                pos = at;
                continue;
            }
            let from = pos;
            // one space before the word is skipped
            if self.cs[pos].java & SPACE_SEPARATOR != 0 {
                pos += 1;
                if pos == n {
                    pos = from;
                }
            }
            let known = self.known(pos);
            for &(end, e) in &known {
                self.add_ko(from, pos, end, e);
            }
            let first = self.cs[pos];
            let cat = &self.d.categories[first.class as usize];
            if known.is_empty() || cat.invoke {
                let mut class = first.class;
                let (mut len, mut units) = (1, first.units as u32);
                if cat.group {
                    let mut script = first.script();
                    let common = |s: u16| s == SCRIPT_COMMON || s == SCRIPT_INHERITED;
                    while pos + len < n && units + self.cs[pos + len].units as u32 <= MAX_UNKNOWN {
                        let ch = self.cs[pos + len];
                        let sc = ch.script();
                        let same = script == sc
                            || common(script)
                            || common(sc)
                            || ch.java & NON_SPACING_MARK != 0;
                        if same
                            && ch.punct() == first.punct()
                            && (ch.java & DIGIT) == (first.java & DIGIT)
                            && self.d.categories[ch.class as usize].group
                        {
                            units += ch.units as u32;
                            len += 1;
                        } else {
                            break;
                        }
                        if common(script) && !common(sc) {
                            script = sc;
                            class = ch.class;
                        }
                    }
                }
                for e in self.unknown(class) {
                    self.add_ko(from, pos, pos + len, e);
                }
            }
            pos += 1;
        }
        if n > 0 {
            let k = self.best_end();
            self.backtrace_ko(n, k);
        }
        self.words
    }

    fn add_ko(&mut self, from: usize, start: usize, end: usize, e: u32) {
        let entry = self.d.entries[e as usize];
        let spaces = start - from;
        let penalty = if spaces > 0 && space_penalized(self.d.tag(entry.pos)) {
            3000
        } else {
            0
        };
        let (cost, k) = self.best_before(from, entry.left, penalty);
        self.nodes[end].push(Node {
            cost: cost + entry.cost as i64,
            right: entry.right,
            back: from,
            start,
            back_index: k,
            entry: e,
        });
        self.max_end = self.max_end.max(end);
    }

    fn backtrace_ko(&mut self, end: usize, k: usize) {
        if end == self.committed {
            return;
        }
        let mut words = Vec::new();
        let mut pos = end;
        let mut best = k;
        while pos > self.committed {
            let node = self.nodes[pos][best];
            words.push((node.start, pos, node.entry));
            pos = node.back;
            best = node.back_index;
        }
        let from = self.committed;
        self.committed = end;
        self.release(from);
        words.reverse();
        self.words.extend(words);
    }
}

/// Nori's part of speech of a tag: every ending is `E` and every particle `J`.
fn resolve(tag: &str) -> &str {
    if tag.starts_with(['J', 'j']) {
        "J"
    } else if tag.starts_with(['E', 'e']) {
        "E"
    } else {
        tag
    }
}

/// Whether Nori charges a word with this left part of speech for following a space.
fn space_penalized(tag: &str) -> bool {
    matches!(resolve(tag), "E" | "J" | "VCP" | "XSA" | "XSN" | "XSV")
}

/// Nori's default stop tags.
fn ko_stop(tag: &str) -> bool {
    matches!(
        resolve(tag),
        "E" | "IC"
            | "J"
            | "MAG"
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

/// The parts of speech of Kuromoji's `stoptags.txt`.
const JA_STOP_TAGS: [&str; 27] = [
    "接続詞",
    "助詞",
    "助詞-格助詞",
    "助詞-格助詞-一般",
    "助詞-格助詞-引用",
    "助詞-格助詞-連語",
    "助詞-接続助詞",
    "助詞-係助詞",
    "助詞-副助詞",
    "助詞-間投助詞",
    "助詞-並立助詞",
    "助詞-終助詞",
    "助詞-副助詞／並立助詞／終助詞",
    "助詞-連体化",
    "助詞-副詞化",
    "助詞-特殊",
    "助動詞",
    "記号",
    "記号-一般",
    "記号-読点",
    "記号-句点",
    "記号-空白",
    "記号-括弧開",
    "記号-括弧閉",
    "その他-間投",
    "フィラー",
    "非言語音",
];

/// The words of Kuromoji's `stopwords.txt`.
const JA_STOP_WORDS: [&str; 109] = [
    "の",
    "に",
    "は",
    "を",
    "た",
    "が",
    "で",
    "て",
    "と",
    "し",
    "れ",
    "さ",
    "ある",
    "いる",
    "も",
    "する",
    "から",
    "な",
    "こと",
    "として",
    "い",
    "や",
    "れる",
    "など",
    "なっ",
    "ない",
    "この",
    "ため",
    "その",
    "あっ",
    "よう",
    "また",
    "もの",
    "という",
    "あり",
    "まで",
    "られ",
    "なる",
    "へ",
    "か",
    "だ",
    "これ",
    "によって",
    "により",
    "おり",
    "より",
    "による",
    "ず",
    "なり",
    "られる",
    "において",
    "ば",
    "なかっ",
    "なく",
    "しかし",
    "について",
    "せ",
    "だっ",
    "その後",
    "できる",
    "それ",
    "う",
    "ので",
    "なお",
    "のみ",
    "でき",
    "き",
    "つ",
    "における",
    "および",
    "いう",
    "さらに",
    "でも",
    "ら",
    "たり",
    "その他",
    "に関する",
    "たち",
    "ます",
    "ん",
    "なら",
    "に対して",
    "特に",
    "せる",
    "及び",
    "これら",
    "とき",
    "では",
    "にて",
    "ほか",
    "ながら",
    "うち",
    "そして",
    "とともに",
    "ただし",
    "かつて",
    "それぞれ",
    "または",
    "お",
    "ほど",
    "ものの",
    "に対する",
    "ほとんど",
    "と共に",
    "といった",
    "です",
    "とも",
    "ところ",
    "ここ",
];

/// A Katakana word of four characters or more without a final `ー`, as Kuromoji's
/// Katakana stem filter makes it: every character is of the Katakana block.
fn katakana_stem(mut word: String) -> String {
    if word.encode_utf16().count() >= 4
        && word.ends_with('ー')
        && word.chars().all(|c| matches!(c as u32, 0x30A0..=0x30FF))
    {
        word.pop();
    }
    word
}

/// The analyzer's tokens of the kept words, each one position after the last kept one
/// and the dropped ones. The analyzer lowercases them afterwards.
struct Out {
    tokens: Vec<Token>,
    position: usize,
}

impl Out {
    fn new() -> Self {
        Out {
            tokens: Vec::new(),
            position: 0,
        }
    }
    fn drop_one(&mut self) {
        self.position += 1;
    }
    fn keep(&mut self, text: String, from: usize, to: usize) {
        self.tokens.push(Token {
            offset_from: from,
            offset_to: to,
            position: self.position,
            text,
            position_length: 1,
        });
        self.position += 1;
    }
}

/// The tokens of `ja-dict`: Kuromoji's words of the text with its widths folded, after
/// the filters of Lucene's `JapaneseAnalyzer`.
pub(super) fn japanese(d: &Dictionary, text: &str) -> Vec<Token> {
    debug_assert_eq!(d.layout, Layout::Ipadic);
    let folded = super::super::cjk::fold_chars(text);
    let analyzed: String = folded.iter().map(|c| c.0).collect();
    // per character of `analyzed`, its source range
    let origin: Vec<(usize, usize)> = folded.iter().map(|c| (c.1, c.2)).collect();
    let lattice = Lattice::new(d, &analyzed);
    let bytes: Vec<usize> = (0..=lattice.cs.len()).map(|i| lattice.byte(i)).collect();
    let words = lattice.kuromoji();
    let mut out = Out::new();
    for (s, e, entry) in words {
        let en = d.entries[entry as usize];
        if en.pos != NONE && JA_STOP_TAGS.contains(&d.strings[en.pos as usize].as_str()) {
            out.drop_one();
            continue;
        }
        let term = if en.base != NONE {
            d.strings[en.base as usize].clone()
        } else {
            analyzed[bytes[s]..bytes[e]].to_string()
        };
        if JA_STOP_WORDS.contains(&term.as_str()) {
            out.drop_one();
            continue;
        }
        out.keep(katakana_stem(term), origin[s].0, origin[e - 1].1);
    }
    out.tokens
}

/// The tokens of `ko-dict`: Nori's words, compounds and inflected words split into their
/// parts, after the filters of Lucene's `KoreanAnalyzer`.
pub(super) fn korean(d: &Dictionary, text: &str) -> Vec<Token> {
    debug_assert_eq!(d.layout, Layout::KoDic);
    let lattice = Lattice::new(d, text);
    let bytes: Vec<usize> = (0..=lattice.cs.len()).map(|i| lattice.byte(i)).collect();
    let first_punct: Vec<bool> = lattice.cs.iter().map(|c| c.punct()).collect();
    let words = lattice.nori();
    let mut out = Out::new();
    for (s, e, entry) in words {
        let en = d.entries[entry as usize];
        let (from, to) = (bytes[s], bytes[e]);
        let surface = &text[from..to];
        let morphemes = (en.kind != super::KIND_MORPHEME && en.parts != NONE)
            .then(|| morphemes(&d.strings[en.parts as usize], en.kind, surface))
            .filter(|m| !m.is_empty());
        let Some(morphemes) = morphemes else {
            if en.kind == super::KIND_MORPHEME && first_punct[s] {
                continue;
            }
            if en.pos != NONE && ko_stop(d.tag(en.pos)) {
                out.drop_one();
                continue;
            }
            let term = if en.base != NONE {
                d.strings[en.base as usize].clone()
            } else {
                surface.to_string()
            };
            out.keep(term, from, to);
            continue;
        };
        // a compound's parts cover their own characters, counted back from its end;
        // the morphemes of an inflected or preanalyzed word cover all of it
        let mut ranges = vec![(from, to); morphemes.len()];
        if en.kind == super::KIND_COMPOUND {
            let mut end = to;
            for (i, (form, _)) in morphemes.iter().enumerate().rev() {
                let start = end.saturating_sub(form.len()).max(from);
                ranges[i] = (start, end);
                end = start;
            }
        }
        for ((form, tag), (a, b)) in morphemes.into_iter().zip(ranges) {
            if ko_stop(tag) {
                out.drop_one();
            } else {
                out.keep(form, a, b);
            }
        }
    }
    out.tokens
}

/// The morphemes of a mecab-ko-dic expression (`form/TAG/*` joined by `+`) as Nori reads
/// them: an inflected word keeps the forms of the expression, and a compound or a
/// preanalyzed word takes from its surface as many characters as each form has.
fn morphemes<'e>(expr: &'e str, kind: u8, surface: &str) -> Vec<(String, &'e str)> {
    let mut out = Vec::new();
    let units: Vec<u16> = surface.encode_utf16().collect();
    let mut at = 0;
    for part in expr.split('+') {
        let mut f = part.split('/');
        let (Some(form), Some(tag)) = (f.next(), f.next()) else {
            continue;
        };
        if form.trim().is_empty() {
            continue;
        }
        if kind == super::KIND_INFLECT {
            out.push((form.to_string(), tag));
        } else {
            let n = form.encode_utf16().count();
            let end = (at + n).min(units.len());
            out.push((String::from_utf16_lossy(&units[at..end]), tag));
            at = end;
        }
    }
    out
}
