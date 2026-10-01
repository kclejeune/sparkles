//! Lines of N-Triples and N-Quads: where they break, and what each one holds (nothing, a
//! comment, a statement with an optional trailing comment, or a `VERSION` directive).
//!
//! The grammars put one statement on each line (N-Triples 1.2 `ntriplesDoc ::=
//! statement? (EOL statement)* EOL?`, `EOL ::= [#xD#xA]+`), whitespace is space and
//! tab, and a `#` outside IRIs and strings starts a comment that runs to the end of the
//! line. The statements themselves are read by oxttl; this scanner only finds the
//! comment, so a wrong split shows up as a failed check of the formatted line, never as
//! changed meaning.

use std::ops::Range;

/// What a line holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// only spaces and tabs
    Blank,
    /// a comment on its own line
    Comment,
    /// a triple or quad (oxttl decides whether it is one), maybe with a trailing comment
    Statement,
    /// `VERSION "…"` (RDF 1.2), maybe with a trailing comment
    Version,
}

/// A scanned line: ranges into the line's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanned {
    pub kind: LineKind,
    /// the statement or directive without the whitespace around it (empty for blank and
    /// comment lines)
    pub body: Range<usize>,
    /// the comment, from `#` to its last non-whitespace character
    pub comment: Option<Range<usize>>,
}

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// `line` (no line break in it) without its trailing spaces and tabs.
fn trim_end(line: &str, end: usize) -> usize {
    let b = line.as_bytes();
    let mut e = end;
    while e > 0 && is_ws(b[e - 1]) {
        e -= 1;
    }
    e
}

/// Scan one line (without its line break).
pub fn scan(line: &str) -> Scanned {
    let b = line.as_bytes();
    let start = b.iter().position(|&c| !is_ws(c)).unwrap_or(b.len());
    if start == b.len() {
        return Scanned {
            kind: LineKind::Blank,
            body: start..start,
            comment: None,
        };
    }
    if b[start] == b'#' {
        return Scanned {
            kind: LineKind::Comment,
            body: start..start,
            comment: Some(start..trim_end(line, b.len())),
        };
    }
    let hash = comment_start(b, start);
    let body_end = trim_end(line, hash.unwrap_or(b.len()));
    let kind = match version_directive(&line[start..body_end]) {
        true => LineKind::Version,
        false => LineKind::Statement,
    };
    Scanned {
        kind,
        body: start..body_end,
        comment: hash.map(|h| h..trim_end(line, b.len())),
    }
}

/// The offset of the `#` that starts a comment after `from`, outside IRIs and strings.
fn comment_start(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'#' => return Some(i),
            // `<<(` opens a triple term; any other `<` an IRI, which holds no `>`
            b'<' if b.get(i + 1) == Some(&b'<') => i += 2,
            b'<' => {
                i += 1;
                while i < b.len() && b[i] != b'>' {
                    i += 1;
                }
                i += 1;
            }
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    // an escape: the next byte is never the end of the string
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Whether `body` is `VERSION` and one quoted string: the RDF 1.2 version announcement,
/// which N-Triples 1.2 and N-Quads 1.2 allow as a line of its own
/// (`versionDirective ::= 'VERSION' versionSpecifier`).
pub fn version_directive(body: &str) -> bool {
    let Some(rest) = body.strip_prefix("VERSION") else {
        return false;
    };
    let spec = rest.trim_start_matches([' ', '\t']);
    if spec.len() == rest.len() && !rest.starts_with(['"', '\'']) {
        return false;
    }
    let b = spec.as_bytes();
    let Some(&q @ (b'"' | b'\'')) = b.first() else {
        return false;
    };
    let mut i = 1;
    while i < b.len() && b[i] != q {
        if matches!(b[i], b'\\' | b'\n' | b'\r') {
            return false;
        }
        i += 1;
    }
    i + 1 == b.len()
}

/// The text of a `VERSION` directive in its printed form: `VERSION`, one space, the
/// string as written.
pub fn version_line(body: &str) -> String {
    let spec = body["VERSION".len()..].trim_start_matches([' ', '\t']);
    format!("VERSION {spec}")
}

/// Split `text` into lines at `\n`, `\r\n` and `\r`: `(line, break)` pairs, where
/// `break` is the line break's bytes (empty for a last line without one).
pub fn split_lines(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let b = rest.as_bytes();
        let (end, len) = match b.iter().position(|&c| c == b'\n' || c == b'\r') {
            None => (b.len(), 0),
            Some(i) if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') => (i, 2),
            Some(i) => (i, 1),
        };
        let (line, eol) = (&rest[..end], &rest[end..end + len]);
        rest = &rest[end + len..];
        Some((line, eol))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(line: &str) -> (LineKind, &str, Option<&str>) {
        let s = scan(line);
        (s.kind, &line[s.body], s.comment.map(|c| &line[c]))
    }

    #[test]
    fn kinds_bodies_and_comments() {
        assert_eq!(parts(""), (LineKind::Blank, "", None));
        assert_eq!(parts(" \t "), (LineKind::Blank, "", None));
        assert_eq!(parts("  # c  "), (LineKind::Comment, "", Some("# c")));
        assert_eq!(
            parts(" <a> <b> <c> .   # first "),
            (LineKind::Statement, "<a> <b> <c> .", Some("# first"))
        );
        assert_eq!(
            parts("<http://e/#a> <b> \"#x\\\"#\" .#c"),
            (
                LineKind::Statement,
                "<http://e/#a> <b> \"#x\\\"#\" .",
                Some("#c")
            )
        );
        assert_eq!(
            parts("<a> <b> <<( <s#> <p> _:o )>> . # t"),
            (
                LineKind::Statement,
                "<a> <b> <<( <s#> <p> _:o )>> .",
                Some("# t")
            )
        );
        // a label ends before `#`, as oxttl reads it
        assert_eq!(
            parts("_:a#b <p> <o> ."),
            (LineKind::Statement, "_:a", Some("#b <p> <o> ."))
        );
        assert_eq!(
            parts("VERSION \"1.2\" # v"),
            (LineKind::Version, "VERSION \"1.2\"", Some("# v"))
        );
    }

    #[test]
    fn version_directives() {
        assert!(version_directive("VERSION \"1.2\""));
        assert!(version_directive("VERSION\t'1.2-basic'"));
        assert!(version_directive("VERSION\"1.2\""));
        assert!(!version_directive("VERSION"));
        assert!(!version_directive("VERSION 1.2"));
        assert!(!version_directive("VERSION \"1.2\" ."));
        assert!(!version_directive("VERSIONS \"1.2\""));
        assert!(!version_directive("version \"1.2\""));
        assert!(!version_directive("VERSION \"1.\\u0032\""));
        assert_eq!(version_line("VERSION\t \"1.2\""), "VERSION \"1.2\"");
    }

    #[test]
    fn line_breaks() {
        let v: Vec<_> = split_lines("a\nb\r\nc\rd").collect();
        assert_eq!(v, [("a", "\n"), ("b", "\r\n"), ("c", "\r"), ("d", "")]);
        let v: Vec<_> = split_lines("\n\na\n").collect();
        assert_eq!(v, [("", "\n"), ("", "\n"), ("a", "\n")]);
        assert_eq!(split_lines("").count(), 0);
    }
}
