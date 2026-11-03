//! The normalized text of the entity search: case and diacritics folded, every run of
//! characters that are neither letters nor digits one space, and the words of the result.

use super::fold_table::FOLD;
use std::ops::Range;

/// Whether `c` is a combining mark that folding drops (the combining diacritical mark
/// blocks).
fn combining(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// Writes the normalized form of `s`, calling `mark(source, output_len)` before each
/// output byte run that character `source` (a byte range of `s`) produced.
fn fold_into(s: &str, out: &mut String, mut mark: impl FnMut(Range<usize>, usize)) {
    let mut space = false;
    let mut push = |out: &mut String, c: char, src: Range<usize>, space: &mut bool| {
        if c.is_alphanumeric() {
            if *space && !out.is_empty() {
                out.push(' ');
            }
            *space = false;
            mark(src, out.len());
            out.push(c);
        } else {
            *space = true;
        }
    };
    for (i, c) in s.char_indices() {
        let src = i..i + c.len_utf8();
        if c.is_ascii() {
            push(out, c.to_ascii_lowercase(), src, &mut space);
        } else if combining(c) {
            continue;
        } else if let Ok(k) = FOLD.binary_search_by(|(f, _)| f.cmp(&c)) {
            for r in FOLD[k].1.chars() {
                push(out, r, src.clone(), &mut space);
            }
        } else {
            for r in c.to_lowercase() {
                push(out, r, src.clone(), &mut space);
            }
        }
    }
}

/// The normalized form of `s`: case and diacritics folded, the characters that are
/// neither letters nor digits taken as separators, and words separated by one space.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    fold_into(s, &mut out, |_, _| {});
    out
}

/// [`normalize`] with, for each byte of the output, the byte range of `s` it came from:
/// the character that produced it, or for a separating space the characters between
/// the two words.
pub(crate) fn normalize_mapped(s: &str) -> (String, Vec<Range<usize>>) {
    let mut out = String::with_capacity(s.len());
    let mut marks: Vec<(Range<usize>, usize)> = Vec::new();
    fold_into(s, &mut out, |src, at| marks.push((src, at)));
    let mut map: Vec<Range<usize>> = vec![0..0; out.len()];
    for (k, (src, at)) in marks.iter().enumerate() {
        let next = marks.get(k + 1);
        let end = next.map_or(out.len(), |(_, a)| *a);
        for m in &mut map[*at..end] {
            *m = src.clone();
        }
        // a space before the next character maps to the gap between the two
        if let Some((n, _)) = next
            && end > *at
            && out.as_bytes()[end - 1] == b' '
        {
            map[end - 1] = src.end..n.start.max(src.end);
        }
    }
    (out, map)
}

/// The words of a normalized string.
pub(crate) fn words(n: &str) -> impl Iterator<Item = &str> {
    n.split(' ').filter(|w| !w.is_empty())
}

/// The words a local name adds besides its normalized words: its camel-case parts and
/// the letters and digits it runs together, such as `has part` for `hasPart` and
/// `go 0005634` for `GO0005634`.
pub(crate) fn name_words(local: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in local.chars() {
        let split = match prev {
            Some(p) => {
                !c.is_alphanumeric()
                    || (p.is_lowercase() && c.is_uppercase())
                    || (p.is_alphabetic() && c.is_numeric())
                    || (p.is_numeric() && c.is_alphabetic())
            }
            None => false,
        };
        if split && !cur.is_empty() {
            out.push(normalize(&cur));
            cur.clear();
        }
        if c.is_alphanumeric() {
            cur.push(c);
        }
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(normalize(&cur));
    }
    let plain = normalize(local);
    let plain: Vec<&str> = words(&plain).collect();
    out.retain(|w| !w.is_empty() && !plain.contains(&w.as_str()));
    out.sort();
    out.dedup();
    out
}

/// The number of characters of `s`.
pub(crate) fn chars(s: &str) -> usize {
    s.chars().count()
}

/// The byte ranges of `text` that the byte ranges `n` of its normalized form cover,
/// merged where they touch.
pub(crate) fn source_ranges(map: &[Range<usize>], n: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in n {
        if r.start >= r.end || r.end > map.len() {
            continue;
        }
        let a = map[r.start].start;
        let b = map[r.end - 1].end;
        if a >= b {
            continue;
        }
        match out.last_mut() {
            Some(l) if l.end >= a => l.end = l.end.max(b),
            _ => out.push(a..b),
        }
    }
    out.sort_by_key(|r| r.start);
    out
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_diacritics_and_punctuation() {
        assert_eq!(normalize("D-Glucose"), "d glucose");
        assert_eq!(
            normalize("  (+)-car-3-ene-2,5-dione "),
            "car 3 ene 2 5 dione"
        );
        assert_eq!(normalize("Éthanol"), "ethanol");
        assert_eq!(normalize("E\u{301}thanol"), "ethanol");
        assert_eq!(normalize("Straße"), "strasse");
        assert_eq!(normalize("H₂O"), "h2o");
        assert_eq!(normalize("ﬁbre"), "fibre");
        assert_eq!(normalize("Ωmega ΑΒΓ"), "ωmega αβγ");
        assert_eq!(normalize("Ærø"), "aero");
        assert_eq!(normalize("CHEBI:15377"), "chebi 15377");
        assert_eq!(normalize("水分子"), "水分子");
        // composed and decomposed letters of any script fold alike
        assert_eq!(normalize("ёлка"), "елка");
        assert_eq!(normalize("е\u{308}лка"), "елка");
        assert_eq!(normalize("Ἀθῆναι"), "αθηναι");
        // kana voicing marks and Hangul syllables are not diacritics
        assert_eq!(normalize("がっこう"), "がっこう");
        assert_eq!(normalize("한국어"), "한국어");
        assert_eq!(normalize("--"), "");
    }

    #[test]
    fn words_split_on_spaces() {
        let n = normalize("glucose 6-phosphate");
        assert_eq!(words(&n).collect::<Vec<_>>(), ["glucose", "6", "phosphate"]);
    }

    #[test]
    fn local_names_add_camel_case_and_digit_parts() {
        assert_eq!(name_words("hasPart"), ["has", "part"]);
        assert_eq!(name_words("GO0005634"), ["0005634", "go"]);
        assert!(name_words("CHEBI_15377").is_empty());
        assert_eq!(name_words("ProteinComplex"), ["complex", "protein"]);
    }

    #[test]
    fn normalized_ranges_map_back_to_the_source() {
        let s = "Éthyl-Alcohol";
        let (n, map) = normalize_mapped(s);
        assert_eq!(n, "ethyl alcohol");
        assert_eq!(map.len(), n.len());
        // "eth" covers "Éth" (É is two bytes)
        assert_eq!(source_ranges(&map, &[0..3]), [0..4]);
        // "alcohol"
        assert_eq!(source_ranges(&map, &[6..13]), [7..14]);
        assert_eq!(&s[7..14], "Alcohol");
        let s = "Straße";
        let (n, map) = normalize_mapped(s);
        assert_eq!(n, "strasse");
        // "stras" ends inside the two letters of ß
        assert_eq!(source_ranges(&map, &[0..5]), [0..6]);
    }
}
