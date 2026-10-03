//! Chinese, Japanese and Korean text without a dictionary: overlapping bigrams, as
//! Lucene's `CJKAnalyzer` makes them.
//!
//! A run of Han, Hiragana, Katakana or Hangul characters becomes the pairs of adjacent
//! characters, so `東京都` is `東京` and `京都`. A run of one character is a token of its
//! own. Other letters and digits form words, split at the other characters, as the
//! standard tokenizer splits them. Before that, full-width ASCII becomes ASCII and
//! half-width Katakana becomes full-width, as Lucene's `CJKWidthFilter` does, so `ＡＢＣ`
//! matches `abc` and `ｶﾞ` matches `ガ`. The analyzer lowercases the tokens afterwards.
//!
//! A search for a word is the OR of its bigrams, as Lucene's query parser reads it, and
//! a phrase keeps them in order.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// The tokenizer of the `cjk` analyzer.
#[derive(Clone, Default)]
pub(crate) struct CjkTokenizer;

pub(crate) struct CjkTokenStream {
    tokens: std::vec::IntoIter<Token>,
    token: Token,
}

impl Tokenizer for CjkTokenizer {
    type TokenStream<'a> = CjkTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> CjkTokenStream {
        CjkTokenStream {
            tokens: tokens(text).into_iter(),
            token: Token::default(),
        }
    }
}

impl TokenStream for CjkTokenStream {
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

/// Whether `c` is written in a CJK script whose runs are cut into bigrams.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        // Han: the unified ideographs, their extensions and the compatibility ones
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        | 0x20000..=0x2A6DF | 0x2A700..=0x2EBEF | 0x2F800..=0x2FA1F | 0x30000..=0x3134F
        // Hiragana, and Katakana without the middle dot, which separates words
        | 0x3041..=0x309F | 0x30A0..=0x30FA | 0x30FC..=0x30FF | 0x31F0..=0x31FF
        // Hangul syllables and jamo
        | 0x1100..=0x11FF | 0x3131..=0x318E | 0xA960..=0xA97F | 0xAC00..=0xD7AF | 0xD7B0..=0xD7FF)
}

/// Full-width forms of the half-width Katakana U+FF65 to U+FF9F (Lucene's
/// `CJKWidthFilter`); the last two are the combining voiced and semi-voiced marks.
const KANA: [u32; 59] = [
    0x30FB, 0x30F2, 0x30A1, 0x30A3, 0x30A5, 0x30A7, 0x30A9, 0x30E3, 0x30E5, 0x30E7, 0x30C3, 0x30FC,
    0x30A2, 0x30A4, 0x30A6, 0x30A8, 0x30AA, 0x30AB, 0x30AD, 0x30AF, 0x30B1, 0x30B3, 0x30B5, 0x30B7,
    0x30B9, 0x30BB, 0x30BD, 0x30BF, 0x30C1, 0x30C4, 0x30C6, 0x30C8, 0x30CA, 0x30CB, 0x30CC, 0x30CD,
    0x30CE, 0x30CF, 0x30D2, 0x30D5, 0x30D8, 0x30DB, 0x30DE, 0x30DF, 0x30E0, 0x30E1, 0x30E2, 0x30E4,
    0x30E6, 0x30E8, 0x30E9, 0x30EA, 0x30EB, 0x30EC, 0x30ED, 0x30EF, 0x30F3, 0x3099, 0x309A,
];

/// `c` with its width folded: full-width ASCII to ASCII, half-width Katakana to
/// full-width.
fn fold(c: char) -> char {
    let u = c as u32;
    let f = match u {
        0xFF01..=0xFF5E => u - 0xFEE0,
        0xFF65..=0xFF9F => KANA[(u - 0xFF65) as usize],
        _ => u,
    };
    char::from_u32(f).unwrap_or(c)
}

/// The voiced (`mark` U+3099) or semi-voiced (U+309A) form of the Katakana `k`, if it
/// has one.
fn voiced(k: char, mark: char) -> Option<char> {
    let u = k as u32;
    let v = match mark as u32 {
        0x3099 => match u {
            // カ to ト, every other code point, with ッ between チ and ツ
            0x30AB..=0x30C2 if (u - 0x30AB).is_multiple_of(2) => u + 1,
            0x30C4 | 0x30C6 | 0x30C8 => u + 1,
            0x30CF..=0x30DD if (u - 0x30CF).is_multiple_of(3) => u + 1,
            0x30A6 => 0x30F4,
            0x30EF => 0x30F7,
            0x30F2 => 0x30FA,
            _ => return None,
        },
        0x309A => match u {
            0x30CF..=0x30DD if (u - 0x30CF).is_multiple_of(3) => u + 2,
            _ => return None,
        },
        _ => return None,
    };
    char::from_u32(v)
}

/// The tokens of `text`: bigrams of its CJK runs and its other words, in order.
fn tokens(text: &str) -> Vec<Token> {
    // the folded characters with the byte range each covers in `text`
    let mut chars: Vec<(char, usize, usize)> = Vec::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        let f = fold(c);
        let end = i + c.len_utf8();
        if matches!(f as u32, 0x3099 | 0x309A)
            && let Some(last) = chars.last_mut()
            && let Some(v) = voiced(last.0, f)
        {
            last.0 = v;
            last.2 = end;
            continue;
        }
        chars.push((f, i, end));
    }
    let mut out = Vec::new();
    let mut push = |text: String, from: usize, to: usize| {
        out.push(Token {
            offset_from: from,
            offset_to: to,
            position: out.len(),
            text,
            position_length: 1,
        });
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].0;
        if is_cjk(c) {
            let start = i;
            while i < chars.len() && is_cjk(chars[i].0) {
                i += 1;
            }
            let run = &chars[start..i];
            if run.len() == 1 {
                push(run[0].0.to_string(), run[0].1, run[0].2);
            } else {
                for w in run.windows(2) {
                    push([w[0].0, w[1].0].iter().collect(), w[0].1, w[1].2);
                }
            }
        } else if c.is_alphanumeric() {
            let start = i;
            while i < chars.len() && chars[i].0.is_alphanumeric() && !is_cjk(chars[i].0) {
                i += 1;
            }
            let run = &chars[start..i];
            push(
                run.iter().map(|x| x.0).collect(),
                run[0].1,
                run[run.len() - 1].2,
            );
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<String> {
        tokens(s).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn bigrams_of_cjk_runs() {
        assert_eq!(texts("東京都"), ["東京", "京都"]);
        // one character alone is a token
        assert_eq!(texts("東"), ["東"]);
        // other words stay whole, and punctuation splits runs
        assert_eq!(
            texts("Tokyo 東京都、日本 2024年"),
            ["Tokyo", "東京", "京都", "日本", "2024", "年"]
        );
        // Hiragana, Katakana and Hangul
        assert_eq!(texts("すし"), ["すし"]);
        assert_eq!(texts("コーヒー"), ["コー", "ーヒ", "ヒー"]);
        assert_eq!(texts("서울시"), ["서울", "울시"]);
        // the Katakana middle dot separates words
        assert_eq!(texts("アン・ドゥ"), ["アン", "ドゥ"]);
        // offsets and positions
        let t = tokens("a 東京都");
        assert_eq!(
            t.iter()
                .map(|t| (t.position, t.offset_from, t.offset_to))
                .collect::<Vec<_>>(),
            [(0, 0, 1), (1, 2, 8), (2, 5, 11)]
        );
    }

    #[test]
    fn widths_are_folded() {
        assert_eq!(texts("ＡＢＣ１２３"), ["ABC123"]);
        assert_eq!(texts("ｶﾀｶﾅ"), ["カタ", "タカ", "カナ"]);
        // a voiced mark joins the kana before it
        assert_eq!(texts("ｶﾞｷﾞ"), ["ガギ"]);
        assert_eq!(texts("ﾊﾟﾝ"), ["パン"]);
        assert_eq!(texts("ｳﾞｧ"), ["ヴァ"]);
    }
}
