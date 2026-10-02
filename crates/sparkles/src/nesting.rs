//! How deeply an RDF document or a SPARQL results document nests, checked as it streams
//! in, before a parser sees it.
//!
//! The RDF parsers keep their own stacks, but what they build is recursive: a triple term
//! nested a few thousand levels deep overflows the stack of the code that renames its
//! blank nodes or drops it, a JSON-LD document nested a thousand objects deep overflows
//! the JSON-LD expansion, and the SPARQL results parsers read a nested triple term by
//! recursion. A stack overflow aborts the whole process, so documents are refused past a
//! depth these all handle on a 2 MiB thread stack:
//!
//! - Turtle, TriG, N-Triples, N-Quads and N3: [`MAX_TRIPLE_TERMS`] nested `<<` (or `<<(`)
//!   triple terms (blank nodes and collections nest without recursion);
//! - JSON-LD: [`MAX_JSON_LD`] nested arrays and objects;
//! - RDF/XML, SPARQL JSON and XML results: [`MAX_ELEMENTS`] nested elements, arrays and
//!   objects (a triple term takes two).
//!
//! The scanners skip strings, IRIs and comments, as the parsers would.

use crate::error::Error;
use oxrdfio::RdfFormat;
use std::io::{self, Read};

/// The deepest triple terms an RDF document may nest, as many as a SPARQL query may.
pub const MAX_TRIPLE_TERMS: usize = spargebra::nesting::MAX_NESTING;

/// The deepest arrays and objects a JSON-LD document may nest (the JSON-LD expansion
/// takes about 2 KiB of stack a level).
pub const MAX_JSON_LD: usize = 256;

/// The deepest elements (or JSON arrays and objects) an RDF/XML document or a SPARQL
/// results document may nest.
pub const MAX_ELEMENTS: usize = 1024;

/// Refuse to store a triple term nested deeper than [`MAX_TRIPLE_TERMS`]: an update can
/// wrap a stored triple term in one more (`INSERT { ?s ?p <<( ?s ?p ?o )>> } WHERE …`), so
/// without this bound repeated updates would grow one past what reading it can recurse.
pub fn check_triple_term(t: &oxrdf::Term) -> crate::Result<()> {
    let mut depth = 0;
    let mut t = t;
    while let oxrdf::Term::Triple(triple) = t {
        depth += 1;
        if depth > MAX_TRIPLE_TERMS {
            return Err(Error::invalid(format!(
                "triple term nested deeper than {MAX_TRIPLE_TERMS} levels"
            )));
        }
        t = &triple.object;
    }
    Ok(())
}

/// What to count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    /// `<<` triple terms in Turtle, TriG, N-Triples, N-Quads and N3
    Turtle,
    /// arrays and objects
    Json,
    /// elements
    Xml,
}

impl Syntax {
    /// The syntax of an RDF format and the depth it may reach.
    pub fn of(format: RdfFormat) -> (Syntax, usize) {
        match format {
            RdfFormat::JsonLd { .. } => (Syntax::Json, MAX_JSON_LD),
            RdfFormat::RdfXml => (Syntax::Xml, MAX_ELEMENTS),
            _ => (Syntax::Turtle, MAX_TRIPLE_TERMS),
        }
    }
}

/// Refuse an RDF document in memory that nests too deeply (see [`Syntax::of`]); `name`
/// names it in the error.
pub fn check(format: RdfFormat, bytes: &[u8], name: &str) -> crate::Result<()> {
    let (syntax, max) = Syntax::of(format);
    // most documents hold no triple term at all
    if syntax == Syntax::Turtle && !bytes.windows(2).any(|w| w == b"<<") {
        return Ok(());
    }
    let mut scan = Scanner::new(syntax, max);
    scan.feed(bytes)
        .map_err(|max| Error::RdfParse(too_deep(name, syntax, max)))
}

fn too_deep(name: &str, syntax: Syntax, max: usize) -> String {
    let what = match syntax {
        Syntax::Turtle => "triple terms",
        Syntax::Json => "arrays and objects",
        Syntax::Xml => "elements",
    };
    format!("{name}: {what} nested deeper than {max} levels")
}

/// A reader that scans what it reads, and fails once the document nests deeper than
/// `max` (with the error `error` makes of the message, inside an [`io::Error`] that
/// [`crate::codec::io_error`] unwraps).
pub struct Guarded<R> {
    inner: R,
    scan: Scanner,
    name: String,
    error: fn(String) -> Error,
}

impl<R: Read> Guarded<R> {
    pub fn new(
        inner: R,
        syntax: Syntax,
        max: usize,
        name: &str,
        error: fn(String) -> Error,
    ) -> Self {
        Guarded {
            inner,
            scan: Scanner::new(syntax, max),
            name: name.to_string(),
            error,
        }
    }

    /// Guard a reader of an RDF document (see [`Syntax::of`]); the error is a parse error.
    pub fn rdf(inner: R, format: RdfFormat, name: &str) -> Self {
        let (syntax, max) = Syntax::of(format);
        Guarded::new(inner, syntax, max, name, Error::RdfParse)
    }
}

impl<R: Read> Read for Guarded<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if let Err(max) = self.scan.feed(&buf[..n]) {
            let e = (self.error)(too_deep(&self.name, self.scan.syntax, max));
            return Err(io::Error::other(e));
        }
        Ok(n)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Normal,
    // Turtle
    /// after `<`: `<<` or an IRI
    Lt,
    /// after `>`: `>>` or not
    Gt,
    Iri,
    Comment,
    /// after `\` outside a string (an escape in a local name)
    Escape,
    /// after one quote: a string, or `""` and maybe a long string
    Quote1(u8),
    Quote2(u8),
    Short(u8),
    ShortEscape(u8),
    /// in a long string, after this many closing quotes
    Long(u8, u8),
    LongEscape(u8),
    // JSON
    Str,
    StrEscape,
    // XML
    /// after `<`
    Tag,
    StartTag,
    /// after `/` in a start tag: `/>` closes it
    StartTagSlash,
    Attribute(u8),
    EndTag,
    /// `<?…?>`, after a `?` or not
    Pi(bool),
    /// after `<!`
    Bang,
    BangDash,
    /// `<!--…-->`, after this many `-`
    XmlComment(u8),
    /// `<![CDATA[…]]>`, after this many `]`
    Cdata(u8),
    /// `<!DOCTYPE …>` and other declarations
    Decl,
}

/// A streaming scanner: [`Scanner::feed`] takes the document in pieces of any size.
pub struct Scanner {
    syntax: Syntax,
    max: usize,
    state: State,
    depth: usize,
}

impl Scanner {
    pub fn new(syntax: Syntax, max: usize) -> Self {
        Scanner {
            syntax,
            max,
            state: State::Normal,
            depth: 0,
        }
    }

    /// Scan the next bytes; `Err(max)` once the document nests deeper than `max`.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), usize> {
        let mut i = 0;
        while i < bytes.len() {
            if self.step(bytes[i])? {
                i += 1;
            }
        }
        Ok(())
    }

    fn open(&mut self) -> Result<(), usize> {
        self.depth += 1;
        if self.depth > self.max {
            return Err(self.max);
        }
        Ok(())
    }

    fn close(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Take one byte in the current state; `false` when the byte is to be taken again in
    /// the new state.
    fn step(&mut self, b: u8) -> Result<bool, usize> {
        use State as S;
        let next = match (self.syntax, self.state, b) {
            // Turtle
            (Syntax::Turtle, S::Normal, b'<') => S::Lt,
            (Syntax::Turtle, S::Normal, b'>') => S::Gt,
            (Syntax::Turtle, S::Normal, b'#') => S::Comment,
            (Syntax::Turtle, S::Normal, b'\\') => S::Escape,
            (Syntax::Turtle, S::Normal, b'"' | b'\'') => S::Quote1(b),
            (Syntax::Turtle, S::Normal, _) => S::Normal,
            (_, S::Lt, b'<') => {
                self.open()?;
                S::Normal
            }
            (_, S::Lt, b'>') => S::Normal,
            (_, S::Lt, _) => {
                self.state = S::Iri;
                return Ok(false);
            }
            (_, S::Iri, b'>') => S::Normal,
            (_, S::Iri, b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | 0..=b' ') => {
                self.state = S::Normal;
                return Ok(false);
            }
            (_, S::Iri, _) => S::Iri,
            (_, S::Gt, b'>') => {
                self.close();
                S::Normal
            }
            (_, S::Gt, _) => {
                self.state = S::Normal;
                return Ok(false);
            }
            (_, S::Comment, b'\n' | b'\r') => S::Normal,
            (_, S::Comment, _) => S::Comment,
            (_, S::Escape, _) => S::Normal,
            (_, S::Quote1(q), _) if b == q => S::Quote2(q),
            (_, S::Quote1(q), _) => {
                self.state = S::Short(q);
                return Ok(false);
            }
            (_, S::Quote2(q), _) if b == q => S::Long(q, 0),
            (_, S::Quote2(_), _) => {
                // `""`, the empty string
                self.state = S::Normal;
                return Ok(false);
            }
            (_, S::Short(q), _) if b == q => S::Normal,
            (_, S::Short(_), b'\n' | b'\r') => S::Normal,
            (_, S::Short(q), b'\\') => S::ShortEscape(q),
            (_, S::Short(q), _) => S::Short(q),
            (_, S::ShortEscape(q), _) => S::Short(q),
            (_, S::Long(q, 2), _) if b == q => S::Normal,
            (_, S::Long(q, n), _) if b == q => S::Long(q, n + 1),
            (_, S::Long(q, _), b'\\') => S::LongEscape(q),
            (_, S::Long(q, _), _) => S::Long(q, 0),
            (_, S::LongEscape(q), _) => S::Long(q, 0),
            // JSON
            (Syntax::Json, S::Normal, b'[' | b'{') => {
                self.open()?;
                S::Normal
            }
            (Syntax::Json, S::Normal, b']' | b'}') => {
                self.close();
                S::Normal
            }
            (Syntax::Json, S::Normal, b'"') => S::Str,
            (Syntax::Json, S::Normal, _) => S::Normal,
            (_, S::Str, b'"') => S::Normal,
            (_, S::Str, b'\\') => S::StrEscape,
            (_, S::Str, _) => S::Str,
            (_, S::StrEscape, _) => S::Str,
            // XML
            (Syntax::Xml, S::Normal, b'<') => S::Tag,
            (Syntax::Xml, S::Normal, _) => S::Normal,
            (_, S::Tag, b'/') => S::EndTag,
            (_, S::Tag, b'?') => S::Pi(false),
            (_, S::Tag, b'!') => S::Bang,
            (_, S::Tag, _) => {
                self.open()?;
                S::StartTag
            }
            (_, S::StartTag | S::StartTagSlash, b'"' | b'\'') => S::Attribute(b),
            (_, S::StartTag, b'/') => S::StartTagSlash,
            (_, S::StartTag | S::StartTagSlash, b'>') => {
                if self.state == S::StartTagSlash {
                    self.close();
                }
                S::Normal
            }
            (_, S::StartTag | S::StartTagSlash, _) => S::StartTag,
            (_, S::Attribute(q), _) if b == q => S::StartTag,
            (_, S::Attribute(q), _) => S::Attribute(q),
            (_, S::EndTag, b'>') => {
                self.close();
                S::Normal
            }
            (_, S::EndTag, _) => S::EndTag,
            (_, S::Pi(true), b'>') => S::Normal,
            (_, S::Pi(_), b'?') => S::Pi(true),
            (_, S::Pi(_), _) => S::Pi(false),
            (_, S::Bang, b'-') => S::BangDash,
            (_, S::BangDash, b'-') => S::XmlComment(0),
            (_, S::Bang, b'[') => S::Cdata(0),
            (_, S::Bang | S::BangDash, _) => {
                self.state = S::Decl;
                return Ok(false);
            }
            (_, S::XmlComment(n), b'>') if n >= 2 => S::Normal,
            (_, S::XmlComment(n), b'-') => S::XmlComment((n + 1).min(2)),
            (_, S::XmlComment(_), _) => S::XmlComment(0),
            (_, S::Cdata(n), b'>') if n >= 2 => S::Normal,
            (_, S::Cdata(n), b']') => S::Cdata((n + 1).min(2)),
            (_, S::Cdata(_), _) => S::Cdata(0),
            (_, S::Decl, b'>') => S::Normal,
            (_, S::Decl, _) => S::Decl,
        };
        self.state = next;
        Ok(true)
    }
}
