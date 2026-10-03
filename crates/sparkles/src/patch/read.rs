//! Reading RDF Patch: the rows of a patch, from its text form or its binary form.
//!
//! The text form follows the row grammar of Jena's `RDFPatchReaderText`. A row is a code
//! (`H`, `TX` or its synonym `TB`, `TC`, `TA`, `PA`, `PD`, `A`, `D`, `Z`), its terms and
//! a dot. Terms are written as in N-Triples and Turtle, as Jena's tokenizer reads them:
//! IRIs, blank nodes as `_:label` or `<_:label>`, literals in single or double quotes
//! (also long quotes) with a language tag and an RDF 1.2 base direction or a datatype,
//! Turtle numbers, `true` and `false`, and triple terms as `<<( s p o )>>`. `#` starts
//! a comment.
//!
//! The binary form is a sequence of `RDF_Patch_Row` structs of Jena's RDF Thrift schema
//! in the Thrift compact protocol ([`super::thrift`]).
//!
//! The reader is streaming. It holds one row at a time, whatever the size of the patch.

use super::thrift::ThriftRows;
use oxrdf::{
    BaseDirection, BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple,
};
use std::io::{self, BufRead, BufReader, Read};

/// One row of a patch.
#[derive(Clone, Debug, PartialEq)]
pub enum PatchRow {
    /// `H name value .`
    Header(String, Term),
    /// `TX .` (also `TB .`)
    Begin,
    /// `TC .`
    Commit,
    /// `TA .`
    Abort,
    /// `Z .`
    Segment,
    /// `A s p o [g] .`
    Add(Quad),
    /// `D s p o [g] .`
    Delete(Quad),
    /// `PA prefix iri [g] .` A graph term is read and dropped.
    PrefixSet(String, String),
    /// `PD prefix [g] .` A graph term is read and dropped.
    PrefixRemove(String),
}

impl PatchRow {
    /// The row's code in the text form.
    pub fn code(&self) -> &'static str {
        match self {
            PatchRow::Header(..) => "H",
            PatchRow::Begin => "TX",
            PatchRow::Commit => "TC",
            PatchRow::Abort => "TA",
            PatchRow::Segment => "Z",
            PatchRow::Add(_) => "A",
            PatchRow::Delete(_) => "D",
            PatchRow::PrefixSet(..) => "PA",
            PatchRow::PrefixRemove(_) => "PD",
        }
    }
}

/// What went wrong with a patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchErrorKind {
    /// The patch does not follow the RDF Patch grammar.
    Syntax,
    /// A row is well formed but holds a term the store cannot hold, such as an invalid
    /// IRI, an invalid language tag or a literal in the subject position.
    Term,
    /// The patch's `prev` header names a commit of this dataset that is not the head.
    PrevMismatch,
}

impl PatchErrorKind {
    /// The `code` of the HTTP error body.
    pub fn code(self) -> &'static str {
        match self {
            PatchErrorKind::Syntax => "patch-syntax",
            PatchErrorKind::Term => "patch-term",
            PatchErrorKind::PrevMismatch => "prev-mismatch",
        }
    }
}

/// An error in a patch, with where it is: the line and column of the text form, or the
/// row and byte offset of the binary form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchError {
    pub kind: PatchErrorKind,
    pub message: String,
    /// the row, counted from 1
    pub row: Option<u64>,
    /// the line and column of the text form, counted from 1
    pub line: Option<u64>,
    pub column: Option<u64>,
    /// the byte offset of the binary form
    pub offset: Option<u64>,
    /// a `prev` mismatch: the commit IRI the patch named, and the head
    pub mismatch: Option<Box<PrevMismatch>>,
}

/// The `prev` header of a patch that named a commit other than the head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrevMismatch {
    /// the commit IRI of the `prev` header
    pub prev: String,
    pub expected: u64,
    pub head: u64,
}

impl PatchError {
    pub fn new(kind: PatchErrorKind, message: impl Into<String>) -> PatchError {
        PatchError {
            kind,
            message: message.into(),
            row: None,
            line: None,
            column: None,
            offset: None,
            mismatch: None,
        }
    }

    /// A `prev` header that names commit `expected` of this dataset when `head` is the
    /// head.
    pub fn prev_mismatch(prev: &str, expected: u64, head: u64) -> PatchError {
        PatchError {
            mismatch: Some(Box::new(PrevMismatch {
                prev: prev.to_string(),
                expected,
                head,
            })),
            ..PatchError::new(
                PatchErrorKind::PrevMismatch,
                format!("the patch expects commit {expected} as the head; the head is {head}"),
            )
        }
    }

    fn at_text(mut self, row: u64, line: u64, column: u64) -> PatchError {
        self.row = Some(row);
        self.line = Some(line);
        self.column = Some(column);
        self
    }

    pub(super) fn at_binary(mut self, row: u64, offset: u64) -> PatchError {
        self.row = Some(row);
        self.offset = Some(offset);
        self
    }
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.kind {
            PatchErrorKind::Syntax => "RDF Patch syntax error",
            PatchErrorKind::Term => "RDF Patch term error",
            PatchErrorKind::PrevMismatch => return f.write_str(&self.message),
        };
        match (self.line, self.column, self.row, self.offset) {
            (Some(l), Some(c), _, _) => write!(f, "{what} at line {l}, column {c}: ")?,
            (_, _, Some(r), Some(o)) => write!(f, "{what} in row {r} (byte {o}): ")?,
            (_, _, Some(r), None) => write!(f, "{what} in row {r}: ")?,
            _ => write!(f, "{what}: ")?,
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for PatchError {}

impl From<PatchError> for crate::Error {
    fn from(e: PatchError) -> crate::Error {
        crate::Error::Patch(Box::new(e))
    }
}

/// The longest term (an IRI, a literal's lexical form, a label) the readers accept.
pub const MAX_TERM_BYTES: usize = 64 << 20;
/// The deepest nesting of triple terms the readers accept.
pub(super) const MAX_TRIPLE_DEPTH: usize = 64;

/// A streaming reader of the rows of a patch, in the text or the binary form.
pub struct PatchReader<R: Read> {
    inner: Inner<R>,
    done: bool,
}

enum Inner<R: Read> {
    Text(TextRows<R>),
    Binary(ThriftRows<R>),
}

impl<R: Read> PatchReader<R> {
    /// A reader of the binary form when `binary`, else of the text form.
    pub fn new(r: R, binary: bool) -> PatchReader<R> {
        PatchReader {
            inner: if binary {
                Inner::Binary(ThriftRows::new(r))
            } else {
                Inner::Text(TextRows::new(r))
            },
            done: false,
        }
    }

    pub fn text(r: R) -> PatchReader<R> {
        PatchReader::new(r, false)
    }

    pub fn binary(r: R) -> PatchReader<R> {
        PatchReader::new(r, true)
    }

    /// The rows read so far.
    pub fn rows(&self) -> u64 {
        match &self.inner {
            Inner::Text(t) => t.rows,
            Inner::Binary(b) => b.rows(),
        }
    }

    /// The next row, `None` at the end. After an error, the reader returns nothing more.
    pub fn next_row(&mut self) -> crate::Result<Option<PatchRow>> {
        if self.done {
            return Ok(None);
        }
        let r = match &mut self.inner {
            Inner::Text(t) => t.next_row(),
            Inner::Binary(b) => b.next_row(),
        };
        if !matches!(r, Ok(Some(_))) {
            self.done = true;
        }
        r
    }
}

impl<R: Read> Iterator for PatchReader<R> {
    type Item = crate::Result<PatchRow>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_row().transpose()
    }
}

/// The form a file's extension names: `.rdfp` text, `.trp` binary, as Jena names them.
/// `None` for another extension.
pub fn binary_for_path(path: &std::path::Path) -> Option<bool> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "rdfp" => Some(false),
        "trp" => Some(true),
        _ => None,
    }
}

// ------------------------------------------------------------------ text form ------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Iri(String),
    BNode(String),
    Literal(Literal),
    LTriple,
    RTriple,
    Dot,
    Eof,
}

/// A token and where it starts.
struct Spanned {
    tok: Tok,
    line: u64,
    column: u64,
}

/// Characters with a position, read from a byte stream as UTF-8, with lookahead.
struct Chars<R: Read> {
    r: BufReader<R>,
    ahead: Vec<char>,
    line: u64,
    column: u64,
}

impl<R: Read> Chars<R> {
    fn new(r: R) -> Chars<R> {
        Chars {
            r: BufReader::with_capacity(64 << 10, r),
            ahead: Vec::new(),
            line: 1,
            column: 1,
        }
    }

    fn decode(&mut self) -> Result<Option<char>, PatchError> {
        let first = match self.r.fill_buf().map_err(io_error)?.first() {
            None => return Ok(None),
            Some(b) => *b,
        };
        let len = match first {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => return Err(self.error("the patch is not UTF-8")),
        };
        let mut buf = [0u8; 4];
        for (i, b) in buf.iter_mut().enumerate().take(len) {
            let next = self.r.fill_buf().map_err(io_error)?;
            match next.first() {
                Some(&x) if i == 0 || x & 0xc0 == 0x80 => *b = x,
                _ => return Err(self.error("the patch is not UTF-8")),
            }
            self.r.consume(1);
        }
        std::str::from_utf8(&buf[..len])
            .ok()
            .and_then(|s| s.chars().next())
            .map(Some)
            .ok_or_else(|| self.error("the patch is not UTF-8"))
    }

    /// The character `n` places ahead, without consuming it.
    fn peek_at(&mut self, n: usize) -> Result<Option<char>, PatchError> {
        while self.ahead.len() <= n {
            match self.decode()? {
                Some(c) => self.ahead.push(c),
                None => return Ok(None),
            }
        }
        Ok(Some(self.ahead[n]))
    }

    fn peek(&mut self) -> Result<Option<char>, PatchError> {
        self.peek_at(0)
    }

    fn bump(&mut self) -> Result<Option<char>, PatchError> {
        let c = if self.ahead.is_empty() {
            self.decode()?
        } else {
            Some(self.ahead.remove(0))
        };
        match c {
            Some('\n') => {
                self.line += 1;
                self.column = 1;
            }
            Some(_) => self.column += 1,
            None => {}
        }
        Ok(c)
    }

    fn error(&self, msg: impl Into<String>) -> PatchError {
        let mut e = PatchError::new(PatchErrorKind::Syntax, msg);
        e.line = Some(self.line);
        e.column = Some(self.column);
        e
    }
}

fn io_error(e: io::Error) -> PatchError {
    PatchError::new(PatchErrorKind::Syntax, format!("reading the patch: {e}"))
}

/// The text form's rows.
struct TextRows<R: Read> {
    chars: Chars<R>,
    peeked: Option<Spanned>,
    rows: u64,
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

impl<R: Read> TextRows<R> {
    fn new(r: R) -> TextRows<R> {
        TextRows {
            chars: Chars::new(r),
            peeked: None,
            rows: 0,
        }
    }

    fn next_row(&mut self) -> crate::Result<Option<PatchRow>> {
        let first = self.next_tok()?;
        if first.tok == Tok::Eof {
            return Ok(None);
        }
        self.rows += 1;
        let (row, line, column) = (self.rows, first.line, first.column);
        self.row(first).map(Some).map_err(|e| {
            // a term error is reported where its row starts; a syntax error where it is
            let e = if e.line.is_some() {
                PatchError {
                    row: Some(row),
                    ..e
                }
            } else {
                e.at_text(row, line, column)
            };
            crate::Error::Patch(Box::new(e))
        })
    }

    fn row(&mut self, first: Spanned) -> Result<PatchRow, PatchError> {
        let code = match first.tok {
            Tok::Word(ref w) => w.clone(),
            Tok::Dot => return Err(self.syntax_at(&first, "an empty row")),
            _ => return Err(self.syntax_at(&first, "expected a row code at the start of a row")),
        };
        let row = match code.as_str() {
            "H" => {
                let key = self.next_tok()?;
                let name = match key.tok {
                    Tok::Word(w) => w,
                    Tok::Literal(l) if l.datatype() == oxrdf::vocab::xsd::STRING => {
                        l.value().to_string()
                    }
                    _ => return Err(self.syntax_at(&key, "a header's key must be a word")),
                };
                let value = self.node()?;
                PatchRow::Header(name, value)
            }
            "A" | "D" => {
                let s = self.node()?;
                let p = self.node()?;
                let o = self.node()?;
                let g = self.node_maybe()?;
                // a term error is reported where its row starts
                let quad = quad(s, p, o, g)?;
                if code == "A" {
                    PatchRow::Add(quad)
                } else {
                    PatchRow::Delete(quad)
                }
            }
            "PA" => {
                let prefix = self.prefix_name()?;
                let t = self.next_tok()?;
                let iri = match t.tok {
                    Tok::Iri(i) => i,
                    Tok::Literal(l) if l.datatype() == oxrdf::vocab::xsd::STRING => {
                        l.value().to_string()
                    }
                    _ => {
                        return Err(self.syntax_at(&t, "a prefix's IRI must be an IRI or a string"));
                    }
                };
                self.node_maybe()?;
                PatchRow::PrefixSet(prefix, iri)
            }
            "PD" => {
                let prefix = self.prefix_name()?;
                self.node_maybe()?;
                PatchRow::PrefixRemove(prefix)
            }
            "TX" | "TB" => PatchRow::Begin,
            "TC" => PatchRow::Commit,
            "TA" => PatchRow::Abort,
            "Z" => PatchRow::Segment,
            other => {
                return Err(self.syntax_at(&first, format!("unknown row code {other:?}")));
            }
        };
        let end = self.next_tok()?;
        if end.tok != Tok::Dot {
            return Err(self.syntax_at(&end, "expected the dot that ends the row"));
        }
        Ok(row)
    }

    fn prefix_name(&mut self) -> Result<String, PatchError> {
        let t = self.next_tok()?;
        match t.tok {
            Tok::Literal(l) if l.datatype() == oxrdf::vocab::xsd::STRING => Ok(l.value().into()),
            Tok::Dot | Tok::Eof => Err(self.syntax_at(&t, "the row is too short")),
            _ => Err(self.syntax_at(&t, "a prefix name must be a string")),
        }
    }

    fn syntax_at(&self, t: &Spanned, msg: impl Into<String>) -> PatchError {
        let mut e = PatchError::new(PatchErrorKind::Syntax, msg);
        e.line = Some(t.line);
        e.column = Some(t.column);
        e
    }

    /// The next term of a row (required).
    fn node(&mut self) -> Result<Term, PatchError> {
        let t = self.next_tok()?;
        self.term_of(t, 0)
    }

    /// An optional last term before the row's dot.
    fn node_maybe(&mut self) -> Result<Option<Term>, PatchError> {
        let t = self.peek_tok()?;
        match t.tok {
            Tok::Dot => Ok(None),
            Tok::Eof => {
                let t = self.next_tok()?;
                Err(self.syntax_at(&t, "the patch ends inside a row (no dot)"))
            }
            _ => self.node().map(Some),
        }
    }

    fn term_of(&mut self, t: Spanned, depth: usize) -> Result<Term, PatchError> {
        let at = (t.line, t.column);
        let term = match t.tok {
            Tok::Iri(i) => match i.strip_prefix("_:") {
                Some(label) => Term::BlankNode(bnode(label).map_err(|e| e.at(at))?),
                None => Term::NamedNode(named(&i).map_err(|e| e.at(at))?),
            },
            Tok::BNode(label) => Term::BlankNode(bnode(&label).map_err(|e| e.at(at))?),
            Tok::Literal(l) => Term::Literal(l),
            Tok::Word(w) if w == "true" || w == "false" => {
                Term::Literal(Literal::new_typed_literal(w, oxrdf::vocab::xsd::BOOLEAN))
            }
            Tok::LTriple => {
                if depth >= MAX_TRIPLE_DEPTH {
                    return Err(self.syntax_at(&t, "triple terms nested too deeply"));
                }
                let mut parts = Vec::with_capacity(3);
                for _ in 0..3 {
                    let n = self.next_tok()?;
                    parts.push(self.term_of(n, depth + 1)?);
                }
                let close = self.next_tok()?;
                if close.tok != Tok::RTriple {
                    return Err(self.syntax_at(&close, "expected )>> to close the triple term"));
                }
                let o = parts.pop().unwrap();
                let p = parts.pop().unwrap();
                let s = parts.pop().unwrap();
                Term::Triple(Box::new(triple(s, p, o).map_err(|e| e.at(at))?))
            }
            Tok::Dot | Tok::Eof => return Err(self.syntax_at(&t, "the row is too short")),
            Tok::RTriple => return Err(self.syntax_at(&t, "unexpected )>>")),
            Tok::Word(ref w) => {
                return Err(self.syntax_at(&t, format!("expected a term, found {w:?}")));
            }
        };
        Ok(term)
    }

    fn peek_tok(&mut self) -> Result<&Spanned, PatchError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex()?);
        }
        Ok(self.peeked.as_ref().unwrap())
    }

    fn next_tok(&mut self) -> Result<Spanned, PatchError> {
        match self.peeked.take() {
            Some(t) => Ok(t),
            None => self.lex(),
        }
    }

    fn lex(&mut self) -> Result<Spanned, PatchError> {
        // whitespace and comments
        loop {
            match self.chars.peek()? {
                Some(c) if c.is_whitespace() => {
                    self.chars.bump()?;
                }
                Some('#') => {
                    while let Some(c) = self.chars.bump()? {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        let (line, column) = (self.chars.line, self.chars.column);
        let span = |tok| Spanned { tok, line, column };
        let Some(c) = self.chars.peek()? else {
            return Ok(span(Tok::Eof));
        };
        let tok = match c {
            '.' if !self.chars.peek_at(1)?.is_some_and(|d| d.is_ascii_digit()) => {
                self.chars.bump()?;
                Tok::Dot
            }
            '<' => {
                if self.chars.peek_at(1)? == Some('<') {
                    if self.chars.peek_at(2)? != Some('(') {
                        return Err(self.chars.error(
                            "a reified triple (<< … >>) cannot be written in a patch; use a triple term <<( … )>>",
                        ));
                    }
                    for _ in 0..3 {
                        self.chars.bump()?;
                    }
                    Tok::LTriple
                } else {
                    Tok::Iri(self.iri()?)
                }
            }
            ')' => {
                if self.chars.peek_at(1)? == Some('>') && self.chars.peek_at(2)? == Some('>') {
                    for _ in 0..3 {
                        self.chars.bump()?;
                    }
                    Tok::RTriple
                } else {
                    return Err(self.chars.error("unexpected )"));
                }
            }
            '_' if self.chars.peek_at(1)? == Some(':') => {
                self.chars.bump()?;
                self.chars.bump()?;
                Tok::BNode(self.label()?)
            }
            '"' | '\'' => Tok::Literal(self.literal()?),
            '+' | '-' | '0'..='9' | '.' => Tok::Literal(self.number()?),
            c if c.is_ascii_alphabetic() => {
                let mut w = String::new();
                while let Some(c) = self.chars.peek()? {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        w.push(c);
                        self.chars.bump()?;
                    } else {
                        break;
                    }
                }
                if self.chars.peek()? == Some(':') {
                    return Err(self.chars.error(format!(
                        "prefixed names ({w}:…) cannot be used in a patch; write the full IRI"
                    )));
                }
                Tok::Word(w)
            }
            c => return Err(self.chars.error(format!("unexpected character {c:?}"))),
        };
        Ok(span(tok))
    }

    fn iri(&mut self) -> Result<String, PatchError> {
        self.chars.bump()?; // <
        let mut s = String::new();
        loop {
            match self.chars.bump()? {
                None => return Err(self.chars.error("the patch ends inside an IRI")),
                Some('>') => break,
                Some('\n') => return Err(self.chars.error("a line break inside an IRI")),
                Some('\\') => match self.chars.bump()? {
                    Some('u') => s.push(self.hex(4)?),
                    Some('U') => s.push(self.hex(8)?),
                    _ => return Err(self.chars.error("an invalid escape in an IRI")),
                },
                Some(c) => s.push(c),
            }
            if s.len() > MAX_TERM_BYTES {
                return Err(self.chars.error("an IRI is too long"));
            }
        }
        Ok(s)
    }

    fn hex(&mut self, n: usize) -> Result<char, PatchError> {
        let mut v = 0u32;
        for _ in 0..n {
            let d = self
                .chars
                .bump()?
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| self.chars.error("an invalid \\u escape"))?;
            v = v * 16 + d;
        }
        char::from_u32(v).ok_or_else(|| self.chars.error("a \\u escape of no character"))
    }

    /// A blank node label after `_:`: name characters and dots, not ending in a dot.
    fn label(&mut self) -> Result<String, PatchError> {
        let mut s = String::new();
        loop {
            match self.chars.peek()? {
                Some(c) if is_name_char(c) => {
                    s.push(c);
                    self.chars.bump()?;
                }
                Some('.') if self.chars.peek_at(1)?.is_some_and(is_name_char) => {
                    s.push('.');
                    self.chars.bump()?;
                }
                _ => break,
            }
            if s.len() > MAX_TERM_BYTES {
                return Err(self.chars.error("a blank node label is too long"));
            }
        }
        if s.is_empty() {
            return Err(self.chars.error("a blank node without a label"));
        }
        Ok(s)
    }

    fn literal(&mut self) -> Result<Literal, PatchError> {
        let q = self.chars.bump()?.unwrap();
        let long = self.chars.peek()? == Some(q) && self.chars.peek_at(1)? == Some(q);
        if long {
            self.chars.bump()?;
            self.chars.bump()?;
        }
        let mut s = String::new();
        loop {
            let Some(c) = self.chars.bump()? else {
                return Err(self.chars.error("the patch ends inside a string"));
            };
            if c == q {
                if !long {
                    break;
                }
                if self.chars.peek()? == Some(q) && self.chars.peek_at(1)? == Some(q) {
                    // a quote right before the closing three belongs to the string
                    if self.chars.peek_at(2)? == Some(q) {
                        s.push(c);
                        continue;
                    }
                    self.chars.bump()?;
                    self.chars.bump()?;
                    break;
                }
                s.push(c);
                continue;
            }
            match c {
                '\\' => {
                    let e = self.chars.bump()?;
                    s.push(match e {
                        Some('t') => '\t',
                        Some('b') => '\u{8}',
                        Some('n') => '\n',
                        Some('r') => '\r',
                        Some('f') => '\u{c}',
                        Some('"') => '"',
                        Some('\'') => '\'',
                        Some('\\') => '\\',
                        Some('u') => self.hex(4)?,
                        Some('U') => self.hex(8)?,
                        _ => return Err(self.chars.error("an invalid escape in a string")),
                    });
                }
                '\n' | '\r' if !long => {
                    return Err(self.chars.error("a line break inside a short string"));
                }
                c => s.push(c),
            }
            if s.len() > MAX_TERM_BYTES {
                return Err(self.chars.error("a string is too long"));
            }
        }
        let at = (self.chars.line, self.chars.column);
        match self.chars.peek()? {
            Some('@') => {
                self.chars.bump()?;
                let mut tag = String::new();
                let mut dir = None;
                while let Some(c) = self.chars.peek()? {
                    if c == '-' && self.chars.peek_at(1)? == Some('-') {
                        self.chars.bump()?;
                        self.chars.bump()?;
                        let mut d = String::new();
                        while let Some(c) = self.chars.peek()? {
                            if !c.is_ascii_alphabetic() {
                                break;
                            }
                            d.push(c);
                            self.chars.bump()?;
                        }
                        dir = Some(match d.as_str() {
                            "ltr" => BaseDirection::Ltr,
                            "rtl" => BaseDirection::Rtl,
                            _ => {
                                return Err(PatchError::new(
                                    PatchErrorKind::Term,
                                    format!("unknown base direction {d:?}"),
                                )
                                .at(at));
                            }
                        });
                        break;
                    }
                    if c.is_ascii_alphanumeric() || c == '-' {
                        tag.push(c);
                        self.chars.bump()?;
                    } else {
                        break;
                    }
                }
                let l = match dir {
                    Some(d) => Literal::new_directional_language_tagged_literal(s, &tag, d),
                    None => Literal::new_language_tagged_literal(s, &tag),
                };
                l.map_err(|e| {
                    PatchError::new(
                        PatchErrorKind::Term,
                        format!("invalid language tag {tag:?}: {e}"),
                    )
                    .at(at)
                })
            }
            Some('^') if self.chars.peek_at(1)? == Some('^') => {
                self.chars.bump()?;
                self.chars.bump()?;
                if self.chars.peek()? != Some('<') {
                    return Err(self
                        .chars
                        .error("a datatype must be a full IRI in angle brackets"));
                }
                let dt = self.iri()?;
                typed(s, &dt).map_err(|e| e.at(at))
            }
            _ => Ok(Literal::new_simple_literal(s)),
        }
    }

    /// A Turtle number: an integer, a decimal or a double, as written.
    fn number(&mut self) -> Result<Literal, PatchError> {
        let mut s = String::new();
        if let Some(c @ ('+' | '-')) = self.chars.peek()? {
            s.push(c);
            self.chars.bump()?;
        }
        let digits = |me: &mut Self, s: &mut String| -> Result<usize, PatchError> {
            let mut n = 0;
            while let Some(c) = me.chars.peek()? {
                if !c.is_ascii_digit() {
                    break;
                }
                s.push(c);
                me.chars.bump()?;
                n += 1;
            }
            Ok(n)
        };
        let int = digits(self, &mut s)?;
        let mut frac = 0;
        let mut dot = false;
        if self.chars.peek()? == Some('.')
            && self.chars.peek_at(1)?.is_some_and(|c| c.is_ascii_digit())
        {
            dot = true;
            s.push('.');
            self.chars.bump()?;
            frac = digits(self, &mut s)?;
        }
        let mut exp = false;
        if let Some(e @ ('e' | 'E')) = self.chars.peek()? {
            exp = true;
            s.push(e);
            self.chars.bump()?;
            if let Some(c @ ('+' | '-')) = self.chars.peek()? {
                s.push(c);
                self.chars.bump()?;
            }
            if digits(self, &mut s)? == 0 {
                return Err(self.chars.error("a number with an empty exponent"));
            }
        }
        if int + frac == 0 {
            return Err(self.chars.error(format!("expected a term, found {s:?}")));
        }
        let dt = if exp {
            "double"
        } else if dot {
            "decimal"
        } else {
            "integer"
        };
        Ok(Literal::new_typed_literal(
            s,
            NamedNode::new_unchecked(format!("{XSD}{dt}")),
        ))
    }
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '\u{b7}') || (c as u32) >= 0x300
}

trait At {
    fn at(self, at: (u64, u64)) -> Self;
}

impl At for PatchError {
    fn at(mut self, (line, column): (u64, u64)) -> PatchError {
        if self.line.is_none() {
            self.line = Some(line);
            self.column = Some(column);
        }
        self
    }
}

// ------------------------------------------------------- terms (both forms) ------

pub(super) fn term_error(msg: impl Into<String>) -> PatchError {
    PatchError::new(PatchErrorKind::Term, msg)
}

pub(super) fn named(iri: &str) -> Result<NamedNode, PatchError> {
    NamedNode::new(iri).map_err(|e| term_error(format!("invalid IRI <{iri}>: {e}")))
}

/// A blank node of any label. Labels only name nodes inside a request (or a stored node,
/// by its number), so they are kept as written.
pub(super) fn bnode(label: &str) -> Result<BlankNode, PatchError> {
    if label.is_empty() || label.contains(|c: char| c.is_whitespace() || c == '>') {
        return Err(term_error(format!("invalid blank node label {label:?}")));
    }
    Ok(BlankNode::new_unchecked(label))
}

pub(super) fn typed(lex: String, dt: &str) -> Result<Literal, PatchError> {
    let dt = named(dt)?;
    if dt.as_str() == oxrdf::vocab::rdf::LANG_STRING.as_str()
        || dt.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#dirLangString"
    {
        return Err(term_error(format!(
            "a literal of type <{dt}> needs a language tag",
            dt = dt.as_str()
        )));
    }
    Ok(Literal::new_typed_literal(lex, dt))
}

pub(super) fn triple(s: Term, p: Term, o: Term) -> Result<Triple, PatchError> {
    Ok(Triple::new(subject(s)?, predicate(p)?, o))
}

fn subject(t: Term) -> Result<NamedOrBlankNode, PatchError> {
    match t {
        Term::NamedNode(n) => Ok(n.into()),
        Term::BlankNode(b) => Ok(b.into()),
        t => Err(term_error(format!(
            "a subject must be an IRI or a blank node, not {t}"
        ))),
    }
}

fn predicate(t: Term) -> Result<NamedNode, PatchError> {
    match t {
        Term::NamedNode(n) => Ok(n),
        t => Err(term_error(format!("a predicate must be an IRI, not {t}"))),
    }
}

/// The quad of a data row's terms.
pub(super) fn quad(s: Term, p: Term, o: Term, g: Option<Term>) -> Result<Quad, PatchError> {
    let g = match g {
        None => GraphName::DefaultGraph,
        Some(Term::NamedNode(n)) => GraphName::NamedNode(n),
        Some(Term::BlankNode(b)) => GraphName::BlankNode(b),
        Some(t) => {
            return Err(term_error(format!(
                "a graph name must be an IRI or a blank node, not {t}"
            )));
        }
    };
    Ok(Quad::new(subject(s)?, predicate(p)?, o, g))
}
