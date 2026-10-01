//! Positions as the Language Server Protocol counts them, and the smallest edit between
//! two texts.
//!
//! A position is a 0-based line and a 0-based character within the line. Lines end at
//! `\n`, `\r\n` or a lone `\r`; characters count UTF-16 code units (the protocol's
//! default) or bytes (`utf-8`, when the client offers it). Positions past the end of a
//! line mean its end, and lines past the end of the text mean the end of the text.

/// How characters within a line are counted (the negotiated `positionEncoding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    /// `utf-8` or `utf-16`
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16 => "utf-16",
        }
    }

    fn width(self, c: char) -> usize {
        match self {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
        }
    }
}

/// The byte offset of `line`/`character` in `text`. A character inside a UTF-8 or
/// UTF-16 sequence means the start of that scalar value.
pub fn offset(text: &str, line: u32, character: u32, enc: Encoding) -> usize {
    let b = text.as_bytes();
    let mut start = 0;
    for _ in 0..line {
        match b[start..].iter().position(|&c| c == b'\n' || c == b'\r') {
            Some(i) => {
                start += i + 1;
                if b[start - 1] == b'\r' && b.get(start) == Some(&b'\n') {
                    start += 1;
                }
            }
            None => return text.len(),
        }
    }
    let rest = &text[start..];
    let end = rest.find(['\n', '\r']).unwrap_or(rest.len());
    let mut units = 0;
    for (i, c) in rest[..end].char_indices() {
        let next = units + enc.width(c);
        if next > character as usize {
            return start + i;
        }
        units = next;
    }
    start + end
}

/// The position of byte `offset` in `text` (inside a scalar value: its start; between the
/// `\r` and `\n` of a line break: the end of the line).
pub fn position(text: &str, offset: usize, enc: Encoding) -> (u32, u32) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let b = text.as_bytes();
    let (mut line, mut start, mut i) = (0u32, 0, 0);
    while i < offset {
        match b[i] {
            b'\n' => {
                line += 1;
                start = i + 1;
            }
            b'\r' if b.get(i + 1) == Some(&b'\n') => {
                if i + 1 == offset {
                    // the middle of `\r\n`
                    offset = i;
                    break;
                }
                line += 1;
                start = i + 2;
                i += 1;
            }
            b'\r' => {
                line += 1;
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let character: usize = text[start..offset].chars().map(|c| enc.width(c)).sum();
    (line, character as u32)
}

/// One replacement: bytes `start..end` of the old text become `insert`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub insert: String,
}

/// The smallest single edit from `old` to `new`: their common prefix and suffix trimmed,
/// never inside a scalar value (so never between the halves of a UTF-16 surrogate pair)
/// nor between the `\r` and `\n` of a line break of `old`, which no position can name.
/// `None` when they are equal.
pub fn minimal_edit(old: &str, new: &str) -> Option<Edit> {
    if old == new {
        return None;
    }
    let (o, n) = (old.as_bytes(), new.as_bytes());
    let max = o.len().min(n.len());
    let mut p = o.iter().zip(n).take_while(|(a, b)| a == b).count();
    while !(old.is_char_boundary(p) && new.is_char_boundary(p)) {
        p -= 1;
    }
    if p > 0 && o[p - 1] == b'\r' && o.get(p) == Some(&b'\n') {
        p -= 1;
    }
    let mut s = o
        .iter()
        .rev()
        .zip(n.iter().rev())
        .take(max - p)
        .take_while(|(a, b)| a == b)
        .count();
    while !(old.is_char_boundary(o.len() - s) && new.is_char_boundary(n.len() - s)) {
        s -= 1;
    }
    let end = o.len() - s;
    if end > 0 && o[end - 1] == b'\r' && o.get(end) == Some(&b'\n') {
        s -= 1;
    }
    Some(Edit {
        start: p,
        end: o.len() - s,
        insert: new[p..n.len() - s].to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use Encoding::{Utf8, Utf16};

    /// Apply `e` to `old`.
    fn apply(old: &str, e: &Edit) -> String {
        format!("{}{}{}", &old[..e.start], e.insert, &old[e.end..])
    }

    #[test]
    fn positions_round_trip_through_every_line_break() {
        // 𝄞 is two UTF-16 units and four bytes; é is one unit and two bytes
        let text = "a𝄞b\r\né\rc\n\n𝄞";
        for (byte, utf16, utf8) in [
            (0, (0, 0), (0, 0)),
            (1, (0, 1), (0, 1)),
            (5, (0, 3), (0, 5)),
            (6, (0, 4), (0, 6)),
            (8, (1, 0), (1, 0)),
            (10, (1, 1), (1, 2)),
            (11, (2, 0), (2, 0)),
            (12, (2, 1), (2, 1)),
            (13, (3, 0), (3, 0)),
            (14, (4, 0), (4, 0)),
            (18, (4, 2), (4, 4)),
        ] {
            assert_eq!(position(text, byte, Utf16), utf16, "byte {byte}");
            assert_eq!(position(text, byte, Utf8), utf8, "byte {byte}");
            assert_eq!(offset(text, utf16.0, utf16.1, Utf16), byte, "{utf16:?}");
            assert_eq!(offset(text, utf8.0, utf8.1, Utf8), byte, "{utf8:?}");
        }
    }

    #[test]
    fn positions_out_of_range_or_inside_a_character() {
        let text = "a𝄞b\r\nc";
        // past the end of a line: its end (before the `\r\n`)
        assert_eq!(offset(text, 0, 99, Utf16), 6);
        // past the last line: the end of the text
        assert_eq!(offset(text, 7, 0, Utf16), text.len());
        // the middle of a surrogate pair, or of a UTF-8 sequence: the character's start
        assert_eq!(offset(text, 0, 2, Utf16), 1);
        assert_eq!(offset(text, 0, 3, Utf8), 1);
        // between `\r` and `\n`: the end of the line; inside a character: its start
        assert_eq!(position(text, 7, Utf16), (0, 4));
        assert_eq!(position(text, 3, Utf16), (0, 1));
        assert_eq!(position(text, 99, Utf16), (1, 1));
        // a text ending in a line break has an empty last line
        assert_eq!(position("x\r\n", 3, Utf16), (1, 0));
        assert_eq!(offset("x\r\n", 1, 0, Utf16), 3);
    }

    #[test]
    fn minimal_edits() {
        assert_eq!(minimal_edit("same", "same"), None);
        let e = minimal_edit("select * {?s ?p ?o}", "SELECT * { ?s ?p ?o }").unwrap();
        assert_eq!((e.start, e.end), (0, 18));
        let e = minimal_edit("SELECT  *\n", "SELECT *\n").unwrap();
        assert_eq!((e.start, e.end, e.insert.as_str()), (7, 8, ""));
        for (old, new) in [
            ("", "x\n"),
            ("x\n", ""),
            ("a\r\nb\r\n", "a\nb\n"),
            ("a\nb", "a\r\nb"),
            ("x\r\n", "x\r\r\n"),
            // two scalar values sharing their leading bytes
            ("é", "è"),
            ("𝄞", "𝄢"),
            ("a𝄞𝄞b", "a𝄞b"),
            ("a\r", "a\r\n"),
        ] {
            let e = minimal_edit(old, new).unwrap();
            assert_eq!(apply(old, &e), new, "{old:?} → {new:?}");
            assert!(old.is_char_boundary(e.start) && old.is_char_boundary(e.end));
            // never between `\r` and `\n`
            for at in [e.start, e.end] {
                assert!(
                    !(at > 0 && old.as_bytes()[at - 1] == b'\r' && old[at..].starts_with('\n'))
                );
            }
        }
        // the edit ends after a whole `\r\n`
        let e = minimal_edit("a\r\nb\r\n", "a\nb\n").unwrap();
        assert_eq!((e.start, e.end, e.insert.as_str()), (1, 6, "\nb\n"));
        // 𝄞 and 𝄢 share three of their four bytes: the edit replaces the whole character
        let e = minimal_edit("x𝄞y", "x𝄢y").unwrap();
        assert_eq!((e.start, e.end, e.insert.as_str()), (1, 5, "𝄢"));
    }
}
