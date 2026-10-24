//! Token filters of the analyzers that follow Lucene's: a filter that replaces each
//! token's text by a function of it, and the functions for elision, the English
//! possessive and Lucene's Dutch stem dictionary.

use tantivy::tokenizer::{Token, TokenFilter, TokenStream, Tokenizer};

/// A filter that replaces each token's text by `f` of it.
#[derive(Clone, Copy)]
pub(crate) struct MapFilter(pub(crate) fn(&str) -> Option<String>);

impl TokenFilter for MapFilter {
    type Tokenizer<T: Tokenizer> = MapTokenizer<T>;

    fn transform<T: Tokenizer>(self, inner: T) -> MapTokenizer<T> {
        MapTokenizer { inner, f: self.0 }
    }
}

#[derive(Clone)]
pub(crate) struct MapTokenizer<T> {
    inner: T,
    f: fn(&str) -> Option<String>,
}

impl<T: Tokenizer> Tokenizer for MapTokenizer<T> {
    type TokenStream<'a> = MapStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        MapStream {
            tail: self.inner.token_stream(text),
            f: self.f,
        }
    }
}

pub(crate) struct MapStream<T> {
    tail: T,
    f: fn(&str) -> Option<String>,
}

impl<T: TokenStream> TokenStream for MapStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let token = self.tail.token_mut();
        if let Some(t) = (self.f)(&token.text) {
            token.text = t;
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// The word lowercased character by character, as Java's `Character.toLowerCase` and
/// so Lucene's `LowerCaseFilter` do: `İ` becomes `i` rather than `i` and a combining
/// dot, and `Σ` is always `σ`.
pub(crate) fn lowercase(word: &str) -> Option<String> {
    let changes = |c: char| {
        let mut l = c.to_lowercase();
        !(l.next() == Some(c) && l.next().is_none())
    };
    if !word.chars().any(changes) {
        return None;
    }
    let mut out = String::with_capacity(word.len());
    for c in word.chars() {
        match c {
            '\u{130}' => out.push('i'),
            c => out.extend(c.to_lowercase()),
        }
    }
    Some(out)
}

/// The articles that Lucene's French analyzer removes before an apostrophe.
const FRENCH_ELISIONS: [&str; 13] = [
    "l", "m", "t", "qu", "n", "s", "j", "d", "c", "jusqu", "quoiqu", "lorsqu", "puisqu",
];

/// The articles that Lucene's Italian analyzer removes before an apostrophe.
const ITALIAN_ELISIONS: [&str; 21] = [
    "c", "l", "all", "dall", "dell", "nell", "sull", "coll", "pell", "gl", "agl", "dagl", "degl",
    "negl", "sugl", "un", "m", "t", "s", "v", "d",
];

/// The word after an elided article (`l'homme` → `homme`): the part before the first
/// apostrophe, in any case, is one of `articles`.
fn elide(word: &str, articles: &[&str]) -> Option<String> {
    let i = word.find(['\'', '\u{2019}'])?;
    let head = &word[..i];
    articles
        .iter()
        .any(|a| a.eq_ignore_ascii_case(head))
        .then(|| word[i + word[i..].chars().next().unwrap().len_utf8()..].to_string())
}

pub(crate) fn french_elision(word: &str) -> Option<String> {
    elide(word, &FRENCH_ELISIONS)
}

pub(crate) fn italian_elision(word: &str) -> Option<String> {
    elide(word, &ITALIAN_ELISIONS)
}

/// The word without a final possessive `'s` (with an ASCII, right single or
/// full-width apostrophe, and `s` in either case), as Lucene's
/// `EnglishPossessiveFilter` removes it.
pub(crate) fn possessive(word: &str) -> Option<String> {
    let mut it = word.chars().rev();
    match (it.next(), it.next()) {
        (Some('s' | 'S'), Some('\'' | '\u{2019}' | '\u{FF07}')) => {
            let cut = word.len() - 1 - word[..word.len() - 1].chars().next_back()?.len_utf8();
            Some(word[..cut].to_string())
        }
        _ => None,
    }
}

/// The Dutch stem of a lowercase word as Lucene's Dutch analyzer makes it: the words of
/// its stem dictionary map to their entries, and the rest go through the Snowball
/// Dutch stemmer.
pub(crate) fn dutch(word: &str) -> Option<String> {
    let fixed = match word {
        "fiets" => "fiets",
        "bromfiets" => "bromfiets",
        "ei" => "eier",
        "kind" => "kinder",
        _ => {
            static STEMMER: std::sync::OnceLock<rust_stemmers::Stemmer> =
                std::sync::OnceLock::new();
            let s = STEMMER
                .get_or_init(|| rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::Dutch));
            return match s.stem(word) {
                std::borrow::Cow::Borrowed(_) => None,
                std::borrow::Cow::Owned(o) => Some(o),
            };
        }
    };
    Some(fixed.to_string())
}

pub(crate) fn french_light(word: &str) -> Option<String> {
    Some(super::light::french(word))
}

pub(crate) fn spanish_light(word: &str) -> Option<String> {
    Some(super::light::spanish(word))
}

pub(crate) fn italian_light(word: &str) -> Option<String> {
    Some(super::light::italian(word))
}

pub(crate) fn portuguese_light(word: &str) -> Option<String> {
    Some(super::light::portuguese(word))
}

pub(crate) fn german_light(word: &str) -> Option<String> {
    Some(super::light::german(&super::light::german_normalize(word)))
}

/// The Snowball Russian stem of a lowercase word as Lucene's Russian analyzer makes it:
/// its Snowball stemmer reads `ё` as `е`.
pub(crate) fn russian(word: &str) -> Option<String> {
    static STEMMER: std::sync::OnceLock<rust_stemmers::Stemmer> = std::sync::OnceLock::new();
    let s =
        STEMMER.get_or_init(|| rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::Russian));
    let plain;
    let w = if word.contains('ё') {
        plain = word.replace('ё', "е");
        plain.as_str()
    } else {
        word
    };
    Some(s.stem(w).into_owned())
}

pub(crate) fn swedish(word: &str) -> Option<String> {
    static STEMMER: std::sync::OnceLock<rust_stemmers::Stemmer> = std::sync::OnceLock::new();
    let s =
        STEMMER.get_or_init(|| rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::Swedish));
    match s.stem(word) {
        std::borrow::Cow::Borrowed(_) => None,
        std::borrow::Cow::Owned(o) => Some(o),
    }
}

/// Lucene's Swedish stop words: the Snowball list, with `sitta` where Tantivy's copy has
/// `sitt`.
pub(crate) const SWEDISH_STOP_WORDS: [&str; 114] = [
    "alla", "allt", "att", "av", "blev", "bli", "blir", "blivit", "de", "dem", "den", "denna",
    "deras", "dess", "dessa", "det", "detta", "dig", "din", "dina", "ditt", "du", "där", "då",
    "efter", "ej", "eller", "en", "er", "era", "ert", "ett", "från", "för", "ha", "hade", "han",
    "hans", "har", "henne", "hennes", "hon", "honom", "hur", "här", "i", "icke", "ingen", "inom",
    "inte", "jag", "ju", "kan", "kunde", "man", "med", "mellan", "men", "mig", "min", "mina",
    "mitt", "mot", "mycket", "ni", "nu", "när", "någon", "något", "några", "och", "om", "oss",
    "på", "samma", "sedan", "sig", "sin", "sina", "sitta", "själv", "skulle", "som", "så", "sådan",
    "sådana", "sådant", "till", "under", "upp", "ut", "utan", "vad", "var", "vara", "varför",
    "varit", "varje", "vars", "vart", "vem", "vi", "vid", "vilka", "vilkas", "vilken", "vilket",
    "vår", "våra", "vårt", "än", "är", "åt", "över",
];

pub(crate) fn porter(word: &str) -> Option<String> {
    Some(super::porter::stem(word))
}

/// The stop words of Lucene's `CJKAnalyzer`: English ones, since CJK text mixes in
/// English, and `s`, `t` and `www`.
pub(crate) const CJK_STOP_WORDS: [&str; 35] = [
    "a", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is", "it", "no",
    "not", "of", "on", "or", "s", "such", "t", "that", "the", "their", "then", "there", "these",
    "they", "this", "to", "was", "will", "with", "www",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elision_and_possessive() {
        assert_eq!(french_elision("l'homme").as_deref(), Some("homme"));
        assert_eq!(french_elision("L’Arbre").as_deref(), Some("Arbre"));
        assert_eq!(french_elision("aujourd'hui"), None);
        assert_eq!(italian_elision("dell'arte").as_deref(), Some("arte"));
        assert_eq!(possessive("John's").as_deref(), Some("John"));
        assert_eq!(possessive("JOHN'S").as_deref(), Some("JOHN"));
        assert_eq!(possessive("boss’s").as_deref(), Some("boss"));
        assert_eq!(possessive("'s").as_deref(), Some(""));
        assert_eq!(possessive("cats"), None);
        assert_eq!(possessive("s"), None);
    }

    #[test]
    fn dutch_dictionary() {
        assert_eq!(dutch("kind").as_deref(), Some("kinder"));
        assert_eq!(dutch("ei").as_deref(), Some("eier"));
        assert_eq!(dutch("fiets").as_deref(), Some("fiets"));
        assert_eq!(dutch("fietsen").as_deref(), Some("fiets"));
    }
}
