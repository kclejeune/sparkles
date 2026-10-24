//! Words as Lucene's `StandardTokenizer` finds them, for the analyzers that follow
//! Lucene's (`english-lucene`, the light stemmers and `dutch-lucene`).
//!
//! The tokenizer applies the word boundary rules of Unicode's UAX #29, as Lucene's does.
//! Letters and digits join into one word, and so do two letters around an apostrophe,
//! a period or a colon (`aren't`, `l'homme`, `u.s.a`) and two digits around a comma, a
//! period or a semicolon (`3,14`). An underscore joins what is around it. A run of
//! Katakana is one word, and each Han ideograph and each Hiragana character is a word
//! of its own. Combining marks and format characters stay with the character before
//! them. Everything else separates words and is dropped. A word longer than 255
//! characters is cut into pieces of 255, as Lucene cuts it.
//!
//! The character classes come from the Rust standard library and from the code points
//! that UAX #29 lists for the joining punctuation, without Unicode's property tables,
//! so the rules for Hebrew quotes, regional indicators and emoji are not reproduced.
//! Those characters separate words here.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// The longest word, in characters, as Lucene's default `maxTokenLength`.
const MAX_CHARS: usize = 255;

/// The tokenizer of the analyzers that follow Lucene's.
#[derive(Clone, Default)]
pub(crate) struct StandardTokenizer;

pub(crate) struct StandardTokenStream {
    tokens: std::vec::IntoIter<Token>,
    token: Token,
}

impl Tokenizer for StandardTokenizer {
    type TokenStream<'a> = StandardTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> StandardTokenStream {
        StandardTokenStream {
            tokens: tokens(text).into_iter(),
            token: Token::default(),
        }
    }
}

impl TokenStream for StandardTokenStream {
    fn advance(&mut self) -> bool {
        match self.tokens.next() {
            Some(t) => {
                self.token = t;
                true
            }
            None => false,
        }
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

/// The word break class of a character (UAX #29), reduced to what the rules use.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Letter,
    Numeric,
    Katakana,
    /// a Han ideograph or a Hiragana character: a word by itself
    Single,
    ExtendNumLet,
    MidLetter,
    MidNum,
    /// a period, an apostrophe and their relatives, which join letters and digits
    MidNumLet,
    /// combining marks, joiners and format characters, which attach to what precedes
    Extend,
    Other,
}

pub(super) fn is_han(u: u32) -> bool {
    matches!(u,
        0x2E80..=0x2FDF | 0x3005 | 0x3007 | 0x3021..=0x3029 | 0x3038..=0x303B
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        | 0x20000..=0x2A6DF | 0x2A700..=0x2EBEF | 0x2F800..=0x2FA1F | 0x30000..=0x3134F)
}

fn is_extend(u: u32) -> bool {
    matches!(u,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2
        | 0x05C4..=0x05C5 | 0x05C7 | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED
        | 0x0900..=0x0903 | 0x093A..=0x094F | 0x0951..=0x0957 | 0x0962..=0x0963
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200C..=0x200D | 0x20D0..=0x20FF
        | 0x302A..=0x302F | 0x3099..=0x309A | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F
        | 0xFF9E..=0xFF9F | 0xE0100..=0xE01EF
        // format characters
        | 0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x200E..=0x200F
        | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB)
}

/// Superscript, subscript, fraction, circled and other numbers that are not decimal
/// digits (Unicode's `No`), which do not form words.
fn is_other_number(u: u32) -> bool {
    matches!(u,
        0xB2..=0xB3 | 0xB9 | 0xBC..=0xBE | 0x9F4..=0x9F9 | 0xB72..=0xB77 | 0xBF0..=0xBF2
        | 0xC78..=0xC7E | 0xD58..=0xD5E | 0xD70..=0xD78 | 0xF2A..=0xF33 | 0x1369..=0x137C
        | 0x17F0..=0x17F9 | 0x19DA | 0x2070..=0x2079 | 0x2080..=0x2089 | 0x2150..=0x215F
        | 0x2189 | 0x2460..=0x249B | 0x24EA..=0x24FF | 0x2776..=0x2793 | 0x2CFD
        | 0x3192..=0x3195 | 0x3220..=0x3229 | 0x3248..=0x324F | 0x3251..=0x325F
        | 0x3280..=0x3289 | 0x32B1..=0x32BF | 0xA830..=0xA835 | 0x10107..=0x10133
        | 0x1F100..=0x1F10C)
}

fn class(c: char) -> Class {
    let u = c as u32;
    match u {
        0x5F | 0x203F | 0x2040 | 0x2054 | 0xFE33 | 0xFE34 | 0xFE4D..=0xFE4F | 0xFF3F | 0x202F => {
            return Class::ExtendNumLet;
        }
        0x3A | 0xB7 | 0x387 | 0x55F | 0x5F4 | 0x2027 | 0xFE13 | 0xFE55 | 0xFF1A => {
            return Class::MidLetter;
        }
        0x2C | 0x3B | 0x37E | 0x589 | 0x60C | 0x60D | 0x66C | 0x7F8 | 0x2044 | 0xFE10 | 0xFE14
        | 0xFE50 | 0xFE54 | 0xFF0C | 0xFF1B => return Class::MidNum,
        0x27 | 0x2E | 0x2018 | 0x2019 | 0x2024 | 0xFE52 | 0xFF07 | 0xFF0E => {
            return Class::MidNumLet;
        }
        0x30A0..=0x30FA
        | 0x30FC..=0x30FF
        | 0x31F0..=0x31FF
        | 0x32D0..=0x32FE
        | 0x3300..=0x3357
        | 0xFF66..=0xFF9D
        | 0x3031..=0x3035
        | 0x309B..=0x309C => {
            return Class::Katakana;
        }
        0x3041..=0x3096 | 0x309D..=0x309F => return Class::Single,
        _ => {}
    }
    if is_extend(u) {
        Class::Extend
    } else if is_han(u) {
        Class::Single
    } else if c.is_alphabetic() {
        Class::Letter
    } else if c.is_numeric() && !is_other_number(u) {
        Class::Numeric
    } else {
        Class::Other
    }
}

/// Whether a word joins across the boundary between `a` and `b`, where `prev` is the
/// class before `a` and `next` the class after `b` (Extend characters skipped).
fn joins(prev: Option<Class>, a: Class, b: Class, next: Option<Class>) -> bool {
    use Class::*;
    let mid_l = |c: Class| matches!(c, MidLetter | MidNumLet);
    let mid_n = |c: Class| matches!(c, MidNum | MidNumLet);
    match (a, b) {
        (Letter, Letter) | (Letter, Numeric) | (Numeric, Letter) | (Numeric, Numeric) => true,
        (Katakana, Katakana) => true,
        (Letter | Numeric | Katakana | ExtendNumLet, ExtendNumLet) => true,
        (ExtendNumLet, Letter | Numeric | Katakana) => true,
        // WB6: a letter, then joining punctuation and another letter
        (Letter, m) if mid_l(m) => next == Some(Letter),
        // WB7: the punctuation between two letters, then the second letter
        (m, Letter) if mid_l(m) => prev == Some(Letter),
        // WB12 and WB11 for digits
        (Numeric, m) if mid_n(m) => next == Some(Numeric),
        (m, Numeric) if mid_n(m) => prev == Some(Numeric),
        _ => false,
    }
}

/// The kind of a word, as Lucene's tokenizer types its tokens: the CJK bigram filter of
/// `cjk-lucene` pairs the characters of the CJK kinds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// letters and digits of any other script, or a mix
    Alphanum,
    Ideographic,
    Hiragana,
    /// a run of Katakana only
    Katakana,
    /// a run of Hangul only
    Hangul,
}

fn is_hangul(u: u32) -> bool {
    matches!(u, 0x1100..=0x11FF | 0x3131..=0x318E | 0xA960..=0xA97F | 0xAC00..=0xD7AF | 0xD7B0..=0xD7FF)
}

/// The words of `text` with their byte offsets and positions.
pub(crate) fn tokens(text: &str) -> Vec<Token> {
    typed_tokens(text).into_iter().map(|(t, _)| t).collect()
}

/// The words of `text` with their byte offsets, positions and kinds.
pub(crate) fn typed_tokens(text: &str) -> Vec<(Token, Kind)> {
    // The characters with their classes, Extend characters folded into the one before
    // (WB4): `units` holds (class, start byte, end byte).
    let mut units: Vec<(Class, usize, usize)> = Vec::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        let end = i + c.len_utf8();
        let k = class(c);
        if k == Class::Extend
            && let Some(last) = units.last_mut()
            && !matches!(text[last.1..].chars().next(), Some('\n' | '\r'))
        {
            last.2 = end;
            continue;
        }
        units.push((k, i, end));
    }
    let mut out: Vec<(Token, Kind)> = Vec::new();
    let mut emit = |from: usize, to: usize, kind: Kind| {
        let word = &text[from..to];
        // a word longer than MAX_CHARS characters becomes pieces of MAX_CHARS
        let mut start = from;
        let mut n = 0;
        for (i, _) in word.char_indices() {
            if n == MAX_CHARS {
                out.push((token(text, start, from + i, out.len()), kind));
                start = from + i;
                n = 0;
            }
            n += 1;
        }
        out.push((token(text, start, to, out.len()), kind));
    };
    let first = |u: &(Class, usize, usize)| text[u.1..].chars().next().map_or(0, |c| c as u32);
    let mut i = 0;
    while i < units.len() {
        let (k, from, _) = units[i];
        match k {
            Class::Single => {
                let kind = if is_han(first(&units[i])) {
                    Kind::Ideographic
                } else {
                    Kind::Hiragana
                };
                emit(from, units[i].2, kind);
                i += 1;
            }
            Class::Letter | Class::Numeric | Class::Katakana | Class::ExtendNumLet => {
                let start = i;
                while i + 1 < units.len() {
                    let prev = i.checked_sub(1).map(|p| units[p].0);
                    let next = units.get(i + 2).map(|u| u.0);
                    if !joins(prev, units[i].0, units[i + 1].0, next) {
                        break;
                    }
                    i += 1;
                }
                let run = &units[start..=i];
                // a run of underscores alone is not a word
                if run.iter().any(|u| u.0 != Class::ExtendNumLet) {
                    let kind = if run.iter().all(|u| u.0 == Class::Katakana) {
                        Kind::Katakana
                    } else if run
                        .iter()
                        .all(|u| u.0 == Class::Letter && is_hangul(first(u)))
                    {
                        Kind::Hangul
                    } else {
                        Kind::Alphanum
                    };
                    emit(units[start].1, units[i].2, kind);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

fn token(text: &str, from: usize, to: usize, position: usize) -> Token {
    Token {
        offset_from: from,
        offset_to: to,
        position,
        text: text[from..to].to_string(),
        position_length: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<String> {
        tokens(s).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn words_join_across_inner_punctuation() {
        assert_eq!(
            texts("John's cars aren't U.S.A. e.g. 3,14 l’arbre"),
            [
                "John's",
                "cars",
                "aren't",
                "U.S.A",
                "e.g",
                "3,14",
                "l’arbre"
            ]
        );
        // punctuation at the edges separates
        assert_eq!(
            texts("'quoted' end. 1,2,3, x:y"),
            ["quoted", "end", "1,2,3", "x:y"]
        );
        // letters and digits join, and underscores join both
        assert_eq!(texts("abc123 foo_bar __ _x"), ["abc123", "foo_bar", "_x"]);
        // a period between a letter and a digit separates them
        assert_eq!(texts("a.1 1.a"), ["a", "1", "1", "a"]);
        assert_eq!(texts("Straße naïve café"), ["Straße", "naïve", "café"]);
        // a combining mark stays with its letter
        assert_eq!(texts("cafe\u{301} x"), ["cafe\u{301}", "x"]);
    }

    #[test]
    fn cjk_characters() {
        // each ideograph and Hiragana character alone, Katakana and Hangul as runs
        assert_eq!(
            texts("東京にコーヒー한국어"),
            ["東", "京", "に", "コーヒー", "한국어"]
        );
    }

    #[test]
    fn offsets_positions_and_long_words() {
        let t = tokens("ab, cd");
        assert_eq!(
            t.iter()
                .map(|t| (t.position, t.offset_from, t.offset_to))
                .collect::<Vec<_>>(),
            [(0, 0, 2), (1, 4, 6)]
        );
        let long = "x".repeat(600);
        assert_eq!(
            texts(&long).iter().map(|w| w.len()).collect::<Vec<_>>(),
            [255, 255, 90]
        );
    }
}
