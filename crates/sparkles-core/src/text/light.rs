//! Light stemmers for French, Spanish, Italian, Portuguese and German, after Jacques
//! Savoy's algorithms, which Lucene's language analyzers for these languages (and so
//! jena-text's) use in place of Snowball's.
//!
//! * J. Savoy, "Report on CLEF-2001 experiments: effective combined query-translation
//!   approach", CLEF 2001, LNCS 2406, gives the stemmers for French, Spanish and Italian
//!   that remove inflectional suffixes (the plural and the feminine).
//! * J. Savoy, "Light stemming approaches for the French, Portuguese, German and
//!   Hungarian languages", ACM SAC 2006, gives the French stemmer that also removes some
//!   derivational suffixes, and the Portuguese and German ones.
//!
//! Each function takes a lowercase word and returns its stem. The stemmers work on
//! characters, keep short words as they are and replace accented vowels, as the papers
//! describe. Lucene's implementations of these algorithms carry a few departures from
//! the papers (for example the French `-x` plural of `-aux`), and these stemmers follow
//! Lucene there, since their purpose is to stem as jena-text does.

/// Whether `s[..len]` ends with `suffix`.
fn ends(s: &[char], len: usize, suffix: &str) -> bool {
    let n = suffix.chars().count();
    n <= len && s[len - n..len].iter().copied().eq(suffix.chars())
}

/// The French light stemmer (Savoy, 2006).
pub fn french(word: &str) -> String {
    let mut s: Vec<char> = word.chars().collect();
    let len = french_stem(&mut s);
    s[..len].iter().collect()
}

fn french_stem(s: &mut [char]) -> usize {
    let mut len = s.len();
    // the plural: -aux becomes -al (not after e), and a final x or s goes
    if len > 5 && s[len - 1] == 'x' {
        if s[len - 3] == 'a' && s[len - 2] == 'u' && s[len - 4] != 'e' {
            s[len - 2] = 'l';
        }
        len -= 1;
    }
    if len > 3 && s[len - 1] == 'x' {
        len -= 1;
    }
    if len > 3 && s[len - 1] == 's' {
        len -= 1;
    }
    // derivational suffixes
    if len > 9 && ends(s, len, "issement") {
        len -= 6;
        s[len - 1] = 'r';
        return french_norm(s, len);
    }
    if len > 8 && ends(s, len, "issant") {
        len -= 4;
        s[len - 1] = 'r';
        return french_norm(s, len);
    }
    if len > 6 && ends(s, len, "ement") {
        len -= 4;
        if len > 3 && ends(s, len, "ive") {
            len -= 1;
            s[len - 1] = 'f';
        }
        return french_norm(s, len);
    }
    if len > 11 && ends(s, len, "ficatrice") {
        len -= 5;
        s[len - 2] = 'e';
        s[len - 1] = 'r';
        return french_norm(s, len);
    }
    if len > 10 && ends(s, len, "ficateur") {
        len -= 4;
        s[len - 2] = 'e';
        s[len - 1] = 'r';
        return french_norm(s, len);
    }
    if len > 9 && ends(s, len, "catrice") {
        len -= 3;
        s[len - 4] = 'q';
        s[len - 3] = 'u';
        s[len - 2] = 'e';
        return french_norm(s, len);
    }
    if len > 8 && ends(s, len, "cateur") {
        len -= 2;
        s[len - 4] = 'q';
        s[len - 3] = 'u';
        s[len - 2] = 'e';
        s[len - 1] = 'r';
        return french_norm(s, len);
    }
    if len > 8 && ends(s, len, "atrice") {
        len -= 4;
        s[len - 2] = 'e';
        s[len - 1] = 'r';
        return french_norm(s, len);
    }
    if len > 7 && ends(s, len, "ateur") {
        len -= 3;
        s[len - 2] = 'e';
        s[len - 1] = 'r';
        return french_norm(s, len);
    }
    if len > 6 && ends(s, len, "trice") {
        len -= 1;
        s[len - 3] = 'e';
        s[len - 2] = 'u';
        s[len - 1] = 'r';
    }
    if len > 5 && ends(s, len, "ième") {
        return french_norm(s, len - 4);
    }
    if len > 7 && ends(s, len, "teuse") {
        len -= 2;
        s[len - 1] = 'r';
        return french_norm(s, len);
    }
    if len > 6 && ends(s, len, "teur") {
        len -= 1;
        s[len - 1] = 'r';
        return french_norm(s, len);
    }
    if len > 5 && ends(s, len, "euse") {
        return french_norm(s, len - 2);
    }
    if len > 8 && ends(s, len, "ère") {
        len -= 1;
        s[len - 2] = 'e';
        return french_norm(s, len);
    }
    if len > 7 && ends(s, len, "ive") {
        len -= 1;
        s[len - 1] = 'f';
        return french_norm(s, len);
    }
    if len > 4 && (ends(s, len, "folle") || ends(s, len, "molle")) {
        len -= 2;
        s[len - 1] = 'u';
        return french_norm(s, len);
    }
    if len > 9 && ends(s, len, "nnelle") {
        return french_norm(s, len - 5);
    }
    if len > 9 && ends(s, len, "nnel") {
        return french_norm(s, len - 3);
    }
    if len > 4 && ends(s, len, "ète") {
        len -= 1;
        s[len - 2] = 'e';
    }
    if len > 8 && ends(s, len, "ique") {
        len -= 4;
    }
    if len > 8 && ends(s, len, "esse") {
        return french_norm(s, len - 3);
    }
    if len > 7 && ends(s, len, "inage") {
        return french_norm(s, len - 3);
    }
    if len > 9 && ends(s, len, "isation") {
        len -= 7;
        if len > 5 && ends(s, len, "ual") {
            s[len - 2] = 'e';
        }
        return french_norm(s, len);
    }
    if len > 9 && ends(s, len, "isateur") {
        return french_norm(s, len - 7);
    }
    if len > 8 && ends(s, len, "ation") {
        return french_norm(s, len - 5);
    }
    if len > 8 && ends(s, len, "ition") {
        return french_norm(s, len - 5);
    }
    french_norm(s, len)
}

/// The end of the French stemmer: accents go and doubled letters become one in words
/// longer than four characters, then a final `-ie`, `r`, `e`, `e` and a doubled last
/// letter go.
fn french_norm(s: &mut [char], mut len: usize) -> usize {
    if len > 4 {
        for c in s[..len].iter_mut() {
            *c = match *c {
                'à' | 'á' | 'â' => 'a',
                'ô' => 'o',
                'è' | 'é' | 'ê' => 'e',
                'ù' | 'û' => 'u',
                'î' => 'i',
                'ç' => 'c',
                c => c,
            };
        }
        let mut ch = s[0];
        let mut i = 1;
        while i < len {
            if s[i] == ch && ch.is_alphabetic() {
                s.copy_within(i + 1..len, i);
                len -= 1;
            } else {
                ch = s[i];
                i += 1;
            }
        }
    }
    if len > 4 && ends(s, len, "ie") {
        len -= 2;
    }
    if len > 4 {
        if s[len - 1] == 'r' {
            len -= 1;
        }
        if s[len - 1] == 'e' {
            len -= 1;
        }
        if s[len - 1] == 'e' {
            len -= 1;
        }
        if s[len - 1] == s[len - 2] && s[len - 1].is_alphabetic() {
            len -= 1;
        }
    }
    len
}

/// Spanish, Italian and Portuguese vowels without their accents, and with `tilde` set
/// also `ã`, `õ` and `ç` as `a`, `o` and `c`.
fn plain_vowels(s: &mut [char], tilde: bool) {
    for c in s.iter_mut() {
        *c = match *c {
            'à' | 'á' | 'â' | 'ä' => 'a',
            'ò' | 'ó' | 'ô' | 'ö' => 'o',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ã' if tilde => 'a',
            'õ' if tilde => 'o',
            'ç' if tilde => 'c',
            c => c,
        };
    }
}

/// The Spanish light stemmer (Savoy, 2001): the final vowel and the plural go.
pub fn spanish(word: &str) -> String {
    let mut s: Vec<char> = word.chars().collect();
    let len = s.len();
    if len < 5 {
        return word.to_string();
    }
    plain_vowels(&mut s, false);
    let n = match s[len - 1] {
        'o' | 'a' | 'e' => len - 1,
        's' if s[len - 2] == 'e' && s[len - 3] == 's' && s[len - 4] == 'e' => len - 2,
        's' if s[len - 2] == 'e' && s[len - 3] == 'c' => {
            s[len - 3] = 'z';
            len - 2
        }
        's' if matches!(s[len - 2], 'o' | 'a' | 'e') => len - 2,
        _ => len,
    };
    s[..n].iter().collect()
}

/// The Italian light stemmer (Savoy, 2001): the final vowel goes, with an `i` or `h`
/// before some of them.
pub fn italian(word: &str) -> String {
    let mut s: Vec<char> = word.chars().collect();
    let len = s.len();
    if len < 6 {
        return word.to_string();
    }
    plain_vowels(&mut s, false);
    let n = match (s[len - 1], s[len - 2]) {
        ('e', 'i' | 'h') | ('i', 'h' | 'i') | ('a' | 'o', 'i') => len - 2,
        ('e' | 'i' | 'a' | 'o', _) => len - 1,
        _ => len,
    };
    s[..n].iter().collect()
}

/// The Portuguese light stemmer (Savoy, 2006): plurals, the adverbial `-mente`, the
/// feminine and a final vowel go.
pub fn portuguese(word: &str) -> String {
    let mut s: Vec<char> = word.chars().collect();
    if s.len() < 4 {
        return word.to_string();
    }
    let mut len = portuguese_suffix(&mut s);
    if len > 3 && s[len - 1] == 'a' {
        len = portuguese_feminine(&mut s, len);
    }
    if len > 4 && matches!(s[len - 1], 'e' | 'a' | 'o') {
        len -= 1;
    }
    plain_vowels(&mut s[..len], true);
    s[..len].iter().collect()
}

fn portuguese_suffix(s: &mut [char]) -> usize {
    let len = s.len();
    if len > 4 && ends(s, len, "es") && matches!(s[len - 3], 'r' | 's' | 'l' | 'z') {
        return len - 2;
    }
    if len > 3 && ends(s, len, "ns") {
        s[len - 2] = 'm';
        return len - 1;
    }
    if len > 4 && (ends(s, len, "eis") || ends(s, len, "éis")) {
        s[len - 3] = 'e';
        s[len - 2] = 'l';
        return len - 1;
    }
    if len > 4 && ends(s, len, "ais") {
        s[len - 2] = 'l';
        return len - 1;
    }
    if len > 4 && ends(s, len, "óis") {
        s[len - 3] = 'o';
        s[len - 2] = 'l';
        return len - 1;
    }
    if len > 4 && ends(s, len, "is") {
        s[len - 1] = 'l';
        return len;
    }
    if len > 3 && (ends(s, len, "ões") || ends(s, len, "ães")) {
        s[len - 3] = 'ã';
        s[len - 2] = 'o';
        return len - 1;
    }
    if len > 6 && ends(s, len, "mente") {
        return len - 5;
    }
    if len > 3 && s[len - 1] == 's' {
        return len - 1;
    }
    len
}

fn portuguese_feminine(s: &mut [char], len: usize) -> usize {
    if len > 7 && (ends(s, len, "inha") || ends(s, len, "iaca") || ends(s, len, "eira")) {
        s[len - 1] = 'o';
        return len;
    }
    if len > 6 {
        if ["osa", "ica", "ida", "ada", "iva", "ama"]
            .iter()
            .any(|x| ends(s, len, x))
        {
            s[len - 1] = 'o';
            return len;
        }
        if ends(s, len, "ona") {
            s[len - 3] = 'ã';
            s[len - 2] = 'o';
            return len - 1;
        }
        if ends(s, len, "ora") {
            return len - 1;
        }
        if ends(s, len, "esa") {
            s[len - 3] = 'ê';
            return len - 1;
        }
        if ends(s, len, "na") {
            s[len - 1] = 'o';
            return len;
        }
    }
    len
}

/// German umlauts and the letter pairs that stand for them made plain, as the
/// German2 variant of the Snowball German stemmer and Lucene's
/// `GermanNormalizationFilter` do: `ä`, `ö` and `ü` become `a`, `o` and `u`, `ß`
/// becomes `ss`, and the `e` of `ae`, `oe` and `ue` goes, except in `ue` after a
/// vowel (`aue`) or after `q`.
pub fn german_normalize(word: &str) -> String {
    // the state after the last character: a consonant or the start, a vowel, or an
    // `a`, `o` or `u` that an `e` may follow as an umlaut
    #[derive(PartialEq)]
    enum St {
        N,
        V,
        U,
    }
    let mut out = String::with_capacity(word.len() + 1);
    let mut st = St::N;
    for c in word.chars() {
        match c {
            'a' | 'o' => {
                out.push(c);
                st = St::U;
            }
            'u' => {
                out.push(c);
                st = if st == St::N { St::U } else { St::V };
            }
            'e' => {
                if st != St::U {
                    out.push(c);
                }
                st = St::V;
            }
            'i' | 'q' | 'y' => {
                out.push(c);
                st = St::V;
            }
            'ä' => {
                out.push('a');
                st = St::V;
            }
            'ö' => {
                out.push('o');
                st = St::V;
            }
            'ü' => {
                out.push('u');
                st = St::V;
            }
            'ß' => {
                out.push_str("ss");
                st = St::N;
            }
            c => {
                out.push(c);
                st = St::N;
            }
        }
    }
    out
}

/// The German light stemmer (Savoy, 2006), on a word that [`german_normalize`] made
/// plain: accents go, then two steps of inflectional suffixes.
pub fn german(word: &str) -> String {
    let mut s: Vec<char> = word.chars().collect();
    for c in s.iter_mut() {
        *c = match *c {
            'ä' | 'à' | 'á' | 'â' => 'a',
            'ö' | 'ò' | 'ó' | 'ô' => 'o',
            'ï' | 'ì' | 'í' | 'î' => 'i',
            'ü' | 'ù' | 'ú' | 'û' => 'u',
            c => c,
        };
    }
    let st = |c: char| matches!(c, 'b' | 'd' | 'f' | 'g' | 'h' | 'k' | 'l' | 'm' | 'n' | 't');
    let mut len = s.len();
    // step 1: -ern, -em, -en, -er, -es, -e, and -s after a valid s-ending
    if len > 5 && ends(&s, len, "ern") {
        len -= 3;
    } else if len > 4 && s[len - 2] == 'e' && matches!(s[len - 1], 'm' | 'n' | 'r' | 's') {
        len -= 2;
    } else if (len > 3 && s[len - 1] == 'e') || (len > 3 && s[len - 1] == 's' && st(s[len - 2])) {
        len -= 1;
    }
    // step 2: -est, -er, -en, and -st after a valid st-ending
    if len > 5 && ends(&s, len, "est") {
        len -= 3;
    } else if (len > 4 && s[len - 2] == 'e' && matches!(s[len - 1], 'r' | 'n'))
        || (len > 4 && ends(&s, len, "st") && st(s[len - 3]))
    {
        len -= 2;
    }
    s[..len].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn french_examples() {
        for (w, s) in [
            ("chevaux", "cheval"),
            ("cheval", "cheval"),
            ("nationales", "national"),
            ("rapidement", "rapid"),
            ("chanteuse", "chant"),
            ("chanteur", "chant"),
            ("maisons", "maison"),
            ("roi", "roi"),
        ] {
            assert_eq!(french(w), s, "{w}");
        }
    }

    #[test]
    fn spanish_italian_portuguese_examples() {
        assert_eq!(spanish("niños"), "niñ");
        assert_eq!(spanish("luces"), "luz");
        assert_eq!(spanish("casa"), "casa");
        assert_eq!(italian("ragazzi"), "ragazz");
        assert_eq!(italian("amiche"), "amic");
        assert_eq!(portuguese("nações"), "naca");
        assert_eq!(portuguese("rapidamente"), "rapid");
    }
}
