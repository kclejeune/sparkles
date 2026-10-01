//! Chunks of whole lines: read from the input, then formatted and checked on their own (in
//! parallel with feature `parallel`).
//!
//! Each statement is formatted from oxttl's quad, in the term spelling of canonical
//! N-Triples ([`super::canonical`]), followed by its trailing comment. The check is per
//! statement: the formatted lines parse (oxttl again) to exactly the input's quads,
//! blank node labels kept, and scan to the same comments, so formatting them again gives
//! the same lines.

use super::canonical;
use super::scan::{LineKind, scan, split_lines, version_directive, version_line};
use crate::{Check, FormatError, Language};
use oxrdf::{GraphName, Quad};
use oxttl::{NQuadsParser, NTriplesParser, TurtleSyntaxError};
use std::borrow::Cow;
use std::io::BufRead;
use std::ops::Range;

/// The bytes a chunk aims at; a longer line makes a longer chunk.
pub const CHUNK_BYTES: usize = 256 << 10;

/// Whole lines of the input, as read.
#[derive(Debug)]
pub struct RawChunk {
    pub bytes: Vec<u8>,
    /// the index of its first line (0-based)
    pub first_line: u64,
    /// the byte offset of `bytes` in the input
    pub offset: u64,
}

/// Reads the input in chunks that end at a line break (or at the end of the input).
pub struct ChunkReader<R> {
    r: R,
    /// read past the end of the last chunk
    rest: Vec<u8>,
    line: u64,
    offset: u64,
    started: bool,
    /// whether the input starts with a byte order mark (dropped)
    pub bom: bool,
    done: bool,
}

impl<R: BufRead> ChunkReader<R> {
    pub fn new(r: R) -> ChunkReader<R> {
        ChunkReader {
            r,
            rest: Vec::new(),
            line: 0,
            offset: 0,
            started: false,
            bom: false,
            done: false,
        }
    }

    /// The next chunk, `None` at the end of the input.
    pub fn next_chunk(&mut self, size: usize) -> std::io::Result<Option<RawChunk>> {
        let mut buf = std::mem::take(&mut self.rest);
        // where the search for a line break starts: the bytes before held none
        let mut from = 0;
        loop {
            if buf.len() >= size || self.done {
                // the last line break that is surely complete (a final `\r` may be the
                // first half of `\r\n`), else the end of the input
                let cut = last_break(&buf[from..], self.done)
                    .map(|c| c + from)
                    .or_else(|| (self.done && !buf.is_empty()).then_some(buf.len()));
                let Some(cut) = cut else {
                    if self.done {
                        return Ok(None);
                    }
                    from = buf.len().saturating_sub(1);
                    self.read_more(&mut buf, size)?;
                    continue;
                };
                self.rest = buf.split_off(cut);
                if !self.started {
                    self.started = true;
                    if buf.starts_with(b"\xEF\xBB\xBF") {
                        self.bom = true;
                        buf.drain(..3);
                        self.offset = 3;
                    }
                }
                let chunk = RawChunk {
                    first_line: self.line,
                    offset: self.offset,
                    bytes: buf,
                };
                self.line += breaks(&chunk.bytes);
                self.offset += chunk.bytes.len() as u64;
                return Ok(Some(chunk));
            }
            self.read_more(&mut buf, size)?;
        }
    }

    /// What is left to read: the bytes read past the last chunk, then the reader.
    pub fn into_rest(self) -> (Vec<u8>, R) {
        (self.rest, self.r)
    }

    /// Append up to `size` more bytes (fewer when `buf` is short of `size`) to `buf`.
    fn read_more(&mut self, buf: &mut Vec<u8>, size: usize) -> std::io::Result<()> {
        let data = loop {
            match self.r.fill_buf() {
                Ok(d) => break d,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        };
        if data.is_empty() {
            self.done = true;
            return Ok(());
        }
        let want = match buf.len() < size {
            true => size - buf.len(),
            false => size,
        };
        let take = data.len().min(want.max(1));
        buf.extend_from_slice(&data[..take]);
        self.r.consume(take);
        Ok(())
    }
}

/// The end of the last complete line break in `b`: after a `\n`, or after a `\r` that is
/// followed by something else (or ends the input).
fn last_break(b: &[u8], at_end: bool) -> Option<usize> {
    let mut i = b.len();
    while i > 0 {
        i -= 1;
        match b[i] {
            b'\n' => return Some(i + 1),
            b'\r' if i + 1 < b.len() || at_end => return Some(i + 1),
            _ => {}
        }
    }
    None
}

/// The line breaks in `b` (`\r\n` is one).
fn breaks(b: &[u8]) -> u64 {
    let mut n = 0;
    for (i, &c) in b.iter().enumerate() {
        if c == b'\n' || (c == b'\r' && b.get(i + 1) != Some(&b'\n')) {
            n += 1;
        }
    }
    n
}

/// One line of a formatted chunk. Ranges are into [`Chunk::text`], `out` into
/// [`Chunk::printed`].
#[derive(Clone, Debug)]
pub struct Line {
    pub kind: LineKind,
    /// the line without its break
    pub src: Range<usize>,
    /// the statement or directive without the whitespace around it
    pub body: Range<usize>,
    /// a comment line's text, or a statement's trailing comment
    pub comment: Option<Range<usize>>,
    /// the formatted statement or directive line (trailing comment included)
    pub out: Range<usize>,
    /// how long the formatted line is without its trailing comment (the sort key)
    pub key_len: usize,
    /// the line ends with `\n` (not `\r\n`, `\r` or the end of the input)
    pub lf: bool,
    /// the output of this line is the line as it is, line break included
    pub same: bool,
}

/// A formatted and checked chunk.
#[derive(Debug)]
pub struct Chunk {
    pub text: String,
    pub printed: String,
    pub lines: Vec<Line>,
    /// the statements' quads, when they are wanted (`canonicalize`)
    pub quads: Vec<Quad>,
    pub first_line: u64,
}

impl Chunk {
    pub fn src(&self, l: &Line) -> &str {
        &self.text[l.src.clone()]
    }

    pub fn comment(&self, l: &Line) -> Option<&str> {
        l.comment.clone().map(|c| &self.text[c])
    }

    /// The formatted statement or directive line.
    pub fn out(&self, l: &Line) -> &str {
        &self.printed[l.out.clone()]
    }

    /// The formatted statement without its comment.
    pub fn key(&self, l: &Line) -> &str {
        &self.printed[l.out.start..l.out.start + l.key_len]
    }

    /// The statement as written, from its first significant character (what
    /// `# sparkles-fmt: ignore` prints).
    pub fn verbatim(&self, l: &Line) -> &str {
        &self.text[l.body.start..l.src.end]
    }
}

/// Format and check one chunk.
pub fn format_chunk(raw: RawChunk, lang: Language, keep_quads: bool) -> Result<Chunk, FormatError> {
    let text = match String::from_utf8(raw.bytes) {
        Ok(t) => t,
        Err(e) => {
            let bytes = e.into_bytes();
            let valid = std::str::from_utf8(&bytes).map_or_else(|e| e.valid_up_to(), |s| s.len());
            let text = String::from_utf8_lossy(&bytes[..valid]);
            return Err(positioned(
                &text,
                raw.first_line,
                raw.offset,
                valid,
                "the input is not valid UTF-8".into(),
            ));
        }
    };
    let mut lines = Vec::new();
    let mut versions = Vec::new();
    let mut at = 0;
    for (line, eol) in split_lines(&text) {
        let s = scan(line);
        let shift = |r: Range<usize>| r.start + at..r.end + at;
        if s.kind == LineKind::Version {
            versions.push(at..at + line.len());
        }
        let same_line = |t: &str| eol == "\n" && t == line;
        let same = match s.kind {
            LineKind::Blank => same_line(""),
            LineKind::Comment => s
                .comment
                .as_ref()
                .is_some_and(|c| same_line(&line[c.clone()])),
            // decided once the line is printed
            LineKind::Statement | LineKind::Version => eol == "\n",
        };
        lines.push(Line {
            kind: s.kind,
            src: at..at + line.len(),
            body: shift(s.body),
            comment: s.comment.map(shift),
            out: 0..0,
            key_len: 0,
            lf: eol == "\n",
            same,
        });
        at += line.len() + eol.len();
    }
    // the reference parse of the statements; version lines are not oxttl's
    let quads = parse(&blanked(&text, &versions), lang)
        .map_err(|(off, msg)| positioned(&text, raw.first_line, raw.offset, off, msg))?;
    let statements = lines
        .iter()
        .filter(|l| l.kind == LineKind::Statement)
        .count();
    if quads.len() != statements {
        return Err(FormatError::Unsupported {
            message: format!("{} statements on {statements} lines", quads.len()),
            line: raw.first_line as u32 + 1,
            column: 1,
        });
    }

    // print
    let mut printed = String::with_capacity(text.len() + text.len() / 8);
    let mut q = quads.iter();
    let mut out_versions = Vec::new();
    for l in &mut lines {
        let start = printed.len();
        match l.kind {
            LineKind::Statement => {
                let quad = q.next().expect("one quad per statement line");
                canonical::write_quad(&mut printed, quad);
            }
            LineKind::Version => {
                printed.push_str(&version_line(&text[l.body.clone()]));
                out_versions.push(start..start);
            }
            LineKind::Blank | LineKind::Comment => continue,
        }
        l.key_len = printed.len() - start;
        if let Some(c) = &l.comment {
            printed.push(' ');
            printed.push_str(&text[c.clone()]);
        }
        l.out = start..printed.len();
        if let Some(v) = out_versions.last_mut()
            && v.start == start
        {
            v.end = printed.len();
        }
        l.same &= printed[start..] == text[l.src.clone()];
        printed.push('\n');
    }

    // the check: the printed lines parse to the same quads and scan to the same comments
    let unsafe_ = |check| FormatError::Unsafe { check };
    let again =
        parse(&blanked(&printed, &out_versions), lang).map_err(|_| unsafe_(Check::Graph))?;
    if again != quads {
        return Err(unsafe_(Check::Graph));
    }
    for l in lines.iter().filter(|l| l.out.end > l.out.start) {
        let line = &printed[l.out.clone()];
        let s = scan(line);
        let comment = s.comment.map(|c| &line[c]);
        if comment != l.comment.clone().map(|c| &text[c]) {
            return Err(unsafe_(Check::Comments));
        }
        if s.kind != l.kind || s.body != (0..l.key_len) {
            return Err(unsafe_(Check::Idempotence));
        }
        if l.kind == LineKind::Version && !version_directive(&line[..l.key_len]) {
            return Err(unsafe_(Check::Idempotence));
        }
    }
    Ok(Chunk {
        text,
        printed,
        lines,
        quads: if keep_quads { quads } else { Vec::new() },
        first_line: raw.first_line,
    })
}

/// `text` with the bytes of `ranges` replaced by spaces (offsets kept).
fn blanked<'t>(text: &'t str, ranges: &[Range<usize>]) -> Cow<'t, str> {
    if ranges.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut b = text.as_bytes().to_vec();
    for r in ranges {
        b[r.clone()].fill(b' ');
    }
    // spaces replace whole lines' UTF-8 sequences
    Cow::Owned(String::from_utf8(b).expect("whole lines blanked"))
}

/// The quads of `text` (N-Triples or N-Quads), or the first error's offset and message.
fn parse(text: &str, lang: Language) -> Result<Vec<Quad>, (usize, String)> {
    let err = |e: TurtleSyntaxError| (e.location().start.offset as usize, e.message().to_string());
    match lang {
        Language::NTriples => NTriplesParser::new()
            .for_slice(text)
            .map(|t| t.map(|t| t.in_graph(GraphName::DefaultGraph)).map_err(err))
            .collect(),
        _ => NQuadsParser::new()
            .for_slice(text)
            .map(|q| q.map_err(err))
            .collect(),
    }
}

/// A syntax error at byte `off` of a chunk of `text` that starts at line `first_line`
/// (0-based) and byte `offset` of the input.
fn positioned(
    text: &str,
    first_line: u64,
    offset: u64,
    off: usize,
    message: String,
) -> FormatError {
    let mut off = off.min(text.len());
    while !text.is_char_boundary(off) {
        off -= 1;
    }
    let mut line = first_line;
    let mut start = 0;
    for (l, eol) in split_lines(text) {
        let end = start + l.len() + eol.len();
        if end > off || eol.is_empty() {
            break;
        }
        line += 1;
        start = end;
    }
    FormatError::Syntax {
        message,
        line: (line + 1).min(u32::MAX as u64) as u32,
        column: text[start..off].chars().count() as u32 + 1,
        offset: (offset as usize).saturating_add(off),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(input: &str, size: usize) -> Vec<(String, u64, u64)> {
        let mut r = ChunkReader::new(input.as_bytes());
        let mut v = Vec::new();
        while let Some(c) = r.next_chunk(size).unwrap() {
            v.push((String::from_utf8(c.bytes).unwrap(), c.first_line, c.offset));
        }
        v
    }

    #[test]
    fn chunks_end_at_line_breaks() {
        let v = chunks("ab\ncd\r\nef\rgh", 3);
        assert_eq!(
            v,
            [
                ("ab\n".to_string(), 0, 0),
                ("cd\r\n".to_string(), 1, 3),
                ("ef\r".to_string(), 2, 7),
                ("gh".to_string(), 3, 10),
            ]
        );
        let v = chunks("\u{feff}a\nb\n", 100);
        assert_eq!(v, [("a\nb\n".to_string(), 0, 3)]);
        // a line longer than the chunk size
        let v = chunks("abcdefgh\nx\n", 2);
        assert_eq!(v[0].0, "abcdefgh\n");
        assert_eq!(chunks("", 10), []);
    }

    #[test]
    fn formats_and_checks_statements() {
        let raw = RawChunk {
            bytes: b"# c\n<http://a>  <http://b> \"x\"^^<http://www.w3.org/2001/XMLSchema#string> . # t \n\n".to_vec(),
            first_line: 0,
            offset: 0,
        };
        let c = format_chunk(raw, Language::NTriples, false).unwrap();
        assert_eq!(c.lines.len(), 3);
        let s = &c.lines[1];
        assert_eq!(c.out(s), "<http://a> <http://b> \"x\" . # t");
        assert_eq!(c.key(s), "<http://a> <http://b> \"x\" .");
        assert!(!s.same);
        assert!(c.lines[0].same && c.lines[2].same);
    }

    #[test]
    fn errors_are_positioned_in_the_input() {
        let raw = RawChunk {
            bytes: "<http://a> <http://b> <http://c> .\n<http://é> <http://b> .\n"
                .as_bytes()
                .to_vec(),
            first_line: 9,
            offset: 100,
        };
        let e = format_chunk(raw, Language::NTriples, false).unwrap_err();
        let FormatError::Syntax { line, .. } = e else {
            panic!("{e:?}")
        };
        assert_eq!(line, 11);
        let raw = RawChunk {
            bytes: b"<http://a> <http://b> \"\xff\" .\n".to_vec(),
            first_line: 0,
            offset: 0,
        };
        let e = format_chunk(raw, Language::NTriples, false).unwrap_err();
        assert!(
            matches!(
                e,
                FormatError::Syntax {
                    line: 1,
                    column: 24,
                    offset: 23,
                    ..
                }
            ),
            "{e:?}"
        );
    }
}
