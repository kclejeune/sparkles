//! The Porter stemmer (M. F. Porter, "An algorithm for suffix stripping", Program 14(3),
//! 1980), in the form of Porter's own reference implementations, which Lucene's
//! `PorterStemFilter` and so jena-text's English analyzer use. Those depart from the
//! paper in two rules of step 2: `bli` becomes `ble` where the paper turns `abli` into
//! `able`, and `logi` becomes `log`. Words of one or two letters are left as they are.
//!
//! The `porter` analyzer of a full-text index uses it in place of the Snowball English
//! stemmer, so that English searches stem as Jena's do.

use tantivy::tokenizer::{Token, TokenFilter, TokenStream, Tokenizer};

/// The stem of a lowercase word.
pub fn stem(word: &str) -> String {
    let mut s = Stem {
        b: word.chars().collect(),
        k: 0,
        j: 0,
    };
    if s.b.len() <= 2 {
        return word.to_string();
    }
    s.k = s.b.len() - 1;
    // the reference implementations check the length once, before step 1
    s.step1ab();
    s.step1c();
    s.step2();
    s.step3();
    s.step4();
    s.step5();
    s.b[..=s.k].iter().collect()
}

/// The word being stemmed: `b[..=k]` is its current end, and `j` the end of the stem
/// before the suffix the last successful [`ends`](Stem::ends) found.
struct Stem {
    b: Vec<char>,
    k: usize,
    j: usize,
}

impl Stem {
    /// Whether `b[i]` is a consonant: not a vowel, and `y` only after a vowel or first.
    fn cons(&self, i: usize) -> bool {
        match self.b[i] {
            'a' | 'e' | 'i' | 'o' | 'u' => false,
            'y' => i == 0 || !self.cons(i - 1),
            _ => true,
        }
    }

    /// The number of vowel-consonant sequences in `b[..=j]` (`m` of the paper).
    fn m(&self) -> usize {
        if self.j == usize::MAX {
            return 0;
        }
        let (mut n, mut i, j) = (0, 0, self.j);
        loop {
            if i > j {
                return n;
            }
            if !self.cons(i) {
                break;
            }
            i += 1;
        }
        i += 1;
        loop {
            loop {
                if i > j {
                    return n;
                }
                if self.cons(i) {
                    break;
                }
                i += 1;
            }
            i += 1;
            n += 1;
            loop {
                if i > j {
                    return n;
                }
                if !self.cons(i) {
                    break;
                }
                i += 1;
            }
            i += 1;
        }
    }

    /// Whether `b[..=j]` has a vowel.
    fn vowel_in_stem(&self) -> bool {
        self.j != usize::MAX && (0..=self.j).any(|i| !self.cons(i))
    }

    /// Whether `b[j-1..=j]` is a double consonant.
    fn double_c(&self, j: usize) -> bool {
        j >= 1 && self.b[j] == self.b[j - 1] && self.cons(j)
    }

    /// Whether `b[i-2..=i]` is consonant, vowel, consonant, and the last is not `w`, `x`
    /// or `y` (`*o` of the paper).
    fn cvc(&self, i: usize) -> bool {
        if i < 2 || !self.cons(i) || self.cons(i - 1) || !self.cons(i - 2) {
            return false;
        }
        !matches!(self.b[i], 'w' | 'x' | 'y')
    }

    /// Whether the word ends with `s`; then `j` is the end of what precedes it.
    fn ends(&mut self, s: &str) -> bool {
        let n = s.chars().count();
        if n > self.k + 1 {
            return false;
        }
        let start = self.k + 1 - n;
        if !self.b[start..=self.k].iter().copied().eq(s.chars()) {
            return false;
        }
        // `j` wraps for a suffix that is the whole word; `m` and the vowel test then see
        // an empty stem, as the reference implementations' j = -1 does
        self.j = start.wrapping_sub(1);
        true
    }

    /// Replace `b[j+1..=k]` with `s`.
    fn set_to(&mut self, s: &str) {
        let at = self.j.wrapping_add(1);
        self.b.truncate(at);
        self.b.extend(s.chars());
        self.k = self.b.len() - 1;
    }

    /// [`set_to`](Self::set_to) when the stem has a vowel-consonant sequence.
    fn r(&mut self, s: &str) {
        if self.m() > 0 {
            self.set_to(s);
        }
    }

    fn step1ab(&mut self) {
        if self.b[self.k] == 's' {
            if self.ends("sses") {
                self.k -= 2;
            } else if self.ends("ies") {
                self.set_to("i");
            } else if self.b[self.k - 1] != 's' {
                self.k -= 1;
            }
        }
        if self.ends("eed") {
            if self.m() > 0 {
                self.k -= 1;
            }
        } else if (self.ends("ed") || self.ends("ing")) && self.vowel_in_stem() {
            self.k = self.j;
            if self.ends("at") {
                self.set_to("ate");
            } else if self.ends("bl") {
                self.set_to("ble");
            } else if self.ends("iz") {
                self.set_to("ize");
            } else if self.double_c(self.k) {
                self.k -= 1;
                if matches!(self.b[self.k], 'l' | 's' | 'z') {
                    self.k += 1;
                }
            } else if self.m() == 1 && self.cvc(self.k) {
                self.set_to("e");
            }
        }
        self.b.truncate(self.k + 1);
    }

    fn step1c(&mut self) {
        if self.ends("y") && self.vowel_in_stem() {
            self.b[self.k] = 'i';
        }
    }

    /// The first suffix of `rules` that the word ends with is replaced, when its stem
    /// has a vowel-consonant sequence.
    fn first(&mut self, rules: &[(&str, &str)]) {
        for (suffix, to) in rules {
            if self.ends(suffix) {
                self.r(to);
                return;
            }
        }
    }

    fn step2(&mut self) {
        if self.k == 0 {
            return;
        }
        match self.b[self.k - 1] {
            'a' => self.first(&[("ational", "ate"), ("tional", "tion")]),
            'c' => self.first(&[("enci", "ence"), ("anci", "ance")]),
            'e' => self.first(&[("izer", "ize")]),
            'l' => self.first(&[
                ("bli", "ble"),
                ("alli", "al"),
                ("entli", "ent"),
                ("eli", "e"),
                ("ousli", "ous"),
            ]),
            'o' => self.first(&[("ization", "ize"), ("ation", "ate"), ("ator", "ate")]),
            's' => self.first(&[
                ("alism", "al"),
                ("iveness", "ive"),
                ("fulness", "ful"),
                ("ousness", "ous"),
            ]),
            't' => self.first(&[("aliti", "al"), ("iviti", "ive"), ("biliti", "ble")]),
            'g' => self.first(&[("logi", "log")]),
            _ => {}
        }
    }

    fn step3(&mut self) {
        match self.b[self.k] {
            'e' => self.first(&[("icate", "ic"), ("ative", ""), ("alize", "al")]),
            'i' => self.first(&[("iciti", "ic")]),
            'l' => self.first(&[("ical", "ic"), ("ful", "")]),
            's' => self.first(&[("ness", "")]),
            _ => {}
        }
    }

    fn step4(&mut self) {
        if self.k == 0 {
            return;
        }
        let found = match self.b[self.k - 1] {
            'a' => self.ends("al"),
            'c' => self.ends("ance") || self.ends("ence"),
            'e' => self.ends("er"),
            'i' => self.ends("ic"),
            'l' => self.ends("able") || self.ends("ible"),
            'n' => self.ends("ant") || self.ends("ement") || self.ends("ment") || self.ends("ent"),
            'o' => {
                (self.ends("ion") && self.j != usize::MAX && matches!(self.b[self.j], 's' | 't'))
                    || self.ends("ou")
            }
            's' => self.ends("ism"),
            't' => self.ends("ate") || self.ends("iti"),
            'u' => self.ends("ous"),
            'v' => self.ends("ive"),
            'z' => self.ends("ize"),
            _ => false,
        };
        if found && self.m() > 1 {
            self.k = self.j;
            self.b.truncate(self.k + 1);
        }
    }

    fn step5(&mut self) {
        self.j = self.k;
        if self.b[self.k] == 'e' {
            let a = self.m();
            if a > 1 || (a == 1 && !self.cvc(self.k - 1)) {
                self.k -= 1;
            }
        }
        // `j` stays at the old end: a removed final vowel adds no sequence to `m`
        if self.b[self.k] == 'l' && self.double_c(self.k) && self.m() > 1 {
            self.k -= 1;
        }
    }
}

/// The Porter stemmer as a token filter.
#[derive(Clone, Copy, Default)]
pub struct PorterStemmer;

impl TokenFilter for PorterStemmer {
    type Tokenizer<T: Tokenizer> = PorterFilter<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> PorterFilter<T> {
        PorterFilter { inner: tokenizer }
    }
}

#[derive(Clone)]
pub struct PorterFilter<T> {
    inner: T,
}

impl<T: Tokenizer> Tokenizer for PorterFilter<T> {
    type TokenStream<'a> = PorterStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        PorterStream {
            tail: self.inner.token_stream(text),
        }
    }
}

pub struct PorterStream<T> {
    tail: T,
}

impl<T: TokenStream> TokenStream for PorterStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let token = self.tail.token_mut();
        token.text = stem(&token.text);
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::stem;

    /// The examples of the paper, with the reference implementations' departures.
    #[test]
    fn the_papers_examples() {
        for (w, s) in [
            // step 1a
            ("caresses", "caress"),
            ("ponies", "poni"),
            ("ties", "ti"),
            ("caress", "caress"),
            ("cats", "cat"),
            // step 1b
            ("feed", "feed"),
            ("agreed", "agre"),
            ("plastered", "plaster"),
            ("bled", "bled"),
            ("motoring", "motor"),
            ("sing", "sing"),
            ("conflated", "conflat"),
            ("troubled", "troubl"),
            ("sized", "size"),
            ("hopping", "hop"),
            ("tanned", "tan"),
            ("falling", "fall"),
            ("hissing", "hiss"),
            ("fizzed", "fizz"),
            ("failing", "fail"),
            ("filing", "file"),
            // step 1c
            ("happy", "happi"),
            ("sky", "sky"),
            // step 2
            ("relational", "relat"),
            ("conditional", "condit"),
            ("rational", "ration"),
            ("valenci", "valenc"),
            ("hesitanci", "hesit"),
            ("digitizer", "digit"),
            ("conformabli", "conform"),
            ("radicalli", "radic"),
            ("differentli", "differ"),
            ("vileli", "vile"),
            ("analogousli", "analog"),
            ("vietnamization", "vietnam"),
            ("predication", "predic"),
            ("operator", "oper"),
            ("feudalism", "feudal"),
            ("decisiveness", "decis"),
            ("hopefulness", "hope"),
            ("callousness", "callous"),
            ("formaliti", "formal"),
            ("sensitiviti", "sensit"),
            ("sensibiliti", "sensibl"),
            // step 3
            ("triplicate", "triplic"),
            ("formative", "form"),
            ("formalize", "formal"),
            ("electriciti", "electr"),
            ("electrical", "electr"),
            ("hopeful", "hope"),
            ("goodness", "good"),
            // step 4
            ("revival", "reviv"),
            ("allowance", "allow"),
            ("inference", "infer"),
            ("airliner", "airlin"),
            ("gyroscopic", "gyroscop"),
            ("adjustable", "adjust"),
            ("defensible", "defens"),
            ("irritant", "irrit"),
            ("replacement", "replac"),
            ("adjustment", "adjust"),
            ("dependent", "depend"),
            ("adoption", "adopt"),
            ("homologou", "homolog"),
            ("communism", "commun"),
            ("activate", "activ"),
            ("angulariti", "angular"),
            ("homologous", "homolog"),
            ("effective", "effect"),
            ("bowdlerize", "bowdler"),
            // step 5
            ("probate", "probat"),
            ("rate", "rate"),
            ("cease", "ceas"),
            ("controll", "control"),
            ("roll", "roll"),
            // whole words
            ("generalizations", "gener"),
            ("oscillators", "oscil"),
            ("theories", "theori"),
            ("theory", "theori"),
            ("relativity", "rel"),
            ("running", "run"),
            ("runs", "run"),
            // the departures: bli → ble, logi → log
            ("possibli", "possibl"),
            ("analogi", "analog"),
            // short words and empty stems
            ("is", "is"),
            ("as", "as"),
            ("ed", "ed"),
            ("ing", "ing"),
            ("sses", "ss"),
            ("ies", "i"),
            ("yes", "ye"),
            ("ion", "ion"),
            ("über", "über"),
        ] {
            assert_eq!(stem(w), s, "{w}");
        }
    }
}
