//! TriX, RDF triples in XML (Carroll and Stickler, HPL-2004-56), read and written as
//! Apache Jena reads and writes it.
//!
//! A document is a `<trix>` (or `<TriX>`) element holding `<graph>` elements. A graph
//! may start with its name (`<uri>`, `<id>` or `<qname>`); a graph without a name holds
//! triples of the default graph. A `<triple>` holds three terms: `<uri>`, `<id>` (a blank
//! node), `<qname>` (a prefixed name resolved against the XML namespaces in scope),
//! `<plainLiteral>` with an optional `xml:lang`, `<typedLiteral datatype="…">`, or a
//! nested `<triple>`, which is a triple term. The content of an `rdf:XMLLiteral` typed
//! literal is the literal's XML, kept as it was written. Elements are recognised by
//! their local names, whatever their namespace, as Jena does.
//!
//! As in Jena, plain literals are simple literals (`xsd:string`) and language-tagged
//! strings, and every other literal is a typed literal. A directional language string
//! is a `plainLiteral` whose `xml:lang` carries the direction after `--` (`en--ltr`), the
//! form Jena's reader understands.
//!
//! The writer follows Jena's `StreamWriterTriX`: a `<trix>` root in the TriX namespace,
//! one `<graph>` element per run of quads of a graph, the default graph's without a
//! name, full IRIs in `<uri>` and two-space indentation.

use oxrdf::vocab::{rdf, xsd};
use oxrdf::{
    BaseDirection, BlankNode, GraphName, GraphNameRef, Literal, NamedNode, NamedOrBlankNode, Quad,
    QuadRef, Term, TermRef, Triple, TripleRef,
};
use quick_xml::events::Event;
use quick_xml::name::{QName, ResolveResult};
use quick_xml::reader::NsReader;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};

/// The TriX namespace, on the root element the writer writes.
pub const NAMESPACE: &str = "http://www.w3.org/2004/03/trix/trix-1/";

/// The media type the writer's output is sent as.
pub const MEDIA_TYPE: &str = "application/trix+xml";

/// The file extension of TriX documents.
pub const FILE_EXTENSION: &str = "trix";

/// Whether `media_type` (parameters allowed) is TriX: `application/trix+xml` or Jena's
/// `application/trix`.
pub fn is_media_type(media_type: &str) -> bool {
    let base = media_type.split(';').next().unwrap_or("").trim();
    base.eq_ignore_ascii_case("application/trix+xml")
        || base.eq_ignore_ascii_case("application/trix")
}

/// Whether a file name like `data.trix` or `data.trix.gz` names a TriX document.
pub fn is_path(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    let name = crate::codec::Codec::strip_extension(&name);
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| ext == FILE_EXTENSION)
}

/// A document that is not well-formed XML or not TriX, with the byte offset where the
/// reader noticed.
#[derive(Debug, Clone)]
pub struct TrixError {
    pub message: String,
    pub position: u64,
}

impl std::fmt::Display for TrixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TriX: {} (at byte {})", self.message, self.position)
    }
}

impl std::error::Error for TrixError {}

impl From<TrixError> for crate::error::Error {
    fn from(e: TrixError) -> Self {
        crate::error::Error::RdfParse(e.to_string())
    }
}

/// Where the reader is in the document.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// before the root element
    Outer,
    /// in the root element
    Trix,
    /// in a `<graph>`, before or between its triples
    Graph,
    /// in a `<triple>` (or a triple term nested in one)
    Triple,
    /// after the root element
    Done,
}

/// The namespace of `xml:lang`.
const XML_NS: &[u8] = b"http://www.w3.org/XML/1998/namespace";

/// An attribute: its namespace, local name and value.
struct Attr {
    ns: Option<Vec<u8>>,
    local: Vec<u8>,
    value: String,
}

/// The terms of an open `<triple>`.
struct Open {
    terms: Vec<Term>,
}

/// Read a TriX document, calling `sink` with each quad in document order. Relative IRIs
/// are resolved against `base`; without one they are an error. A syntax error stops the
/// read.
pub fn parse<E: From<TrixError>>(
    input: impl BufRead,
    base: Option<&str>,
    mut sink: impl FnMut(Quad) -> Result<(), E>,
) -> Result<(), E> {
    let mut r = Reader::new(input, base)?;
    while let Some(q) = r.next_quad()? {
        sink(q)?;
    }
    Ok(())
}

/// The streaming reader behind [`parse`].
struct Reader<R: BufRead> {
    xml: NsReader<R>,
    buf: Vec<u8>,
    base: Option<oxiri::Iri<String>>,
    state: State,
    graph: Option<GraphName>,
    /// whether the open graph has had a triple (its name must come first)
    graph_has_triples: bool,
    stack: Vec<Open>,
    /// blank node labels that are not valid N-Triples labels, mapped to fresh nodes
    bnodes: HashMap<String, BlankNode>,
}

impl<R: BufRead> Reader<R> {
    fn new(input: R, base: Option<&str>) -> Result<Self, TrixError> {
        let mut xml = NsReader::from_reader(input);
        let cfg = xml.config_mut();
        cfg.expand_empty_elements = true;
        cfg.check_end_names = true;
        let base = match base {
            Some(b) => Some(oxiri::Iri::parse(b.to_string()).map_err(|e| TrixError {
                message: format!("invalid base IRI '{b}': {e}"),
                position: 0,
            })?),
            None => None,
        };
        Ok(Reader {
            xml,
            buf: Vec::new(),
            base,
            state: State::Outer,
            graph: None,
            graph_has_triples: false,
            stack: Vec::new(),
            bnodes: HashMap::new(),
        })
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T, TrixError> {
        Err(TrixError {
            message: message.into(),
            position: self.xml.buffer_position(),
        })
    }

    fn xml_err<T>(&self, e: impl std::fmt::Display) -> Result<T, TrixError> {
        Err(TrixError {
            message: format!("XML error: {e}"),
            position: self.xml.error_position(),
        })
    }

    /// The next quad, or `None` at the end of the document.
    fn next_quad(&mut self) -> Result<Option<Quad>, TrixError> {
        loop {
            self.buf.clear();
            let event = match self.xml.read_event_into(&mut self.buf) {
                Ok(e) => e.into_owned(),
                Err(e) => return self.xml_err(e),
            };
            match event {
                Event::Start(start) => {
                    let local = start.local_name().as_ref().to_vec();
                    let tag = String::from_utf8_lossy(&local).into_owned();
                    self.start(&tag, &start)?;
                }
                Event::End(end) => {
                    let local = end.local_name().as_ref().to_vec();
                    if let Some(q) = self.end(&local)? {
                        return Ok(Some(q));
                    }
                }
                Event::Eof => {
                    return match self.state {
                        State::Done => Ok(None),
                        State::Outer => self.err("no TriX element in the document"),
                        _ => self.err("the document ends inside an element"),
                    };
                }
                // character data between the structural elements, comments, processing
                // instructions, the XML declaration and a DOCTYPE are skipped
                _ => {}
            }
        }
    }

    fn start(
        &mut self,
        tag: &str,
        start: &quick_xml::events::BytesStart<'_>,
    ) -> Result<(), TrixError> {
        let misplaced = |r: &Self| r.err(format!("out of place XML element <{tag}>"));
        match tag {
            "trix" | "TriX" => {
                if self.state != State::Outer {
                    return misplaced(self);
                }
                self.state = State::Trix;
            }
            "graph" => {
                if self.state != State::Trix {
                    return misplaced(self);
                }
                self.state = State::Graph;
                self.graph = None;
                self.graph_has_triples = false;
            }
            "triple" => match self.state {
                State::Graph => {
                    self.state = State::Triple;
                    self.graph_has_triples = true;
                    self.stack.push(Open { terms: Vec::new() });
                }
                State::Triple => {
                    self.check_room()?;
                    if self.stack.len() > crate::nesting::MAX_TRIPLE_TERMS {
                        return self.err(format!(
                            "triple terms nested deeper than {}",
                            crate::nesting::MAX_TRIPLE_TERMS
                        ));
                    }
                    self.stack.push(Open { terms: Vec::new() });
                }
                _ => return misplaced(self),
            },
            "uri" | "id" | "qname" => {
                let term = self.node(tag)?;
                match self.state {
                    State::Graph => {
                        if self.graph.is_some() {
                            return self.err("duplicate graph name");
                        }
                        if self.graph_has_triples {
                            return self.err("a graph name must come before the graph's triples");
                        }
                        self.graph = Some(match term {
                            Term::NamedNode(n) => GraphName::NamedNode(n),
                            Term::BlankNode(b) => GraphName::BlankNode(b),
                            _ => unreachable!("node() gives IRIs and blank nodes"),
                        });
                    }
                    State::Triple => self.push(term)?,
                    _ => return misplaced(self),
                }
            }
            "plainLiteral" | "typedLiteral" => {
                if self.state == State::Graph {
                    return self.err("a graph name cannot be a literal");
                }
                if self.state != State::Triple {
                    return misplaced(self);
                }
                let lit = if tag == "plainLiteral" {
                    self.plain_literal(start)?
                } else {
                    self.typed_literal(start)?
                };
                self.push(Term::Literal(lit))?;
            }
            other => return self.err(format!("unrecognized XML element <{other}>")),
        }
        Ok(())
    }

    /// Fail early when the innermost open triple already has its three terms.
    fn check_room(&self) -> Result<(), TrixError> {
        if self.stack.last().is_some_and(|o| o.terms.len() >= 3) {
            return self.err("too many terms for a triple");
        }
        Ok(())
    }

    fn push(&mut self, t: Term) -> Result<(), TrixError> {
        self.check_room()?;
        self.stack.last_mut().expect("in a triple").terms.push(t);
        Ok(())
    }

    fn end(&mut self, local: &[u8]) -> Result<Option<Quad>, TrixError> {
        match local {
            b"triple" => {
                let open = self.stack.pop().expect("a <triple> is open");
                let n = open.terms.len();
                if n < 3 {
                    let count = ["zero", "one", "two"][n];
                    return self.err(format!("too few terms ({count}) for a triple"));
                }
                let mut it = open.terms.into_iter();
                let (s, p, o) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
                let subject = match s {
                    Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
                    Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
                    Term::Literal(_) => return self.err("the subject of a triple is a literal"),
                    Term::Triple(_) => {
                        return self.err("the subject of a triple is a triple term");
                    }
                };
                let predicate = match p {
                    Term::NamedNode(n) => n,
                    Term::Literal(_) => return self.err("the predicate of a triple is a literal"),
                    _ => return self.err("the predicate of a triple must be an IRI"),
                };
                let t = Triple::new(subject, predicate, o);
                if let Some(outer) = self.stack.last_mut() {
                    // a triple term
                    outer.terms.push(Term::Triple(Box::new(t)));
                    return Ok(None);
                }
                self.state = State::Graph;
                let g = self.graph.clone().unwrap_or(GraphName::DefaultGraph);
                Ok(Some(Quad::new(t.subject, t.predicate, t.object, g)))
            }
            b"graph" => {
                self.state = State::Trix;
                self.graph = None;
                Ok(None)
            }
            b"trix" | b"TriX" => {
                self.state = State::Done;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// The text content of the element just started, up to its end tag. Elements inside
    /// it are an error; comments and processing instructions are skipped.
    fn text(&mut self, tag: &str) -> Result<String, TrixError> {
        let mut out = String::new();
        loop {
            self.buf.clear();
            let event = match self.xml.read_event_into(&mut self.buf) {
                Ok(e) => e,
                Err(e) => return self.xml_err(e),
            };
            match event {
                Event::Text(t) => match t.unescape() {
                    Ok(s) => out.push_str(&s),
                    Err(e) => return self.xml_err(e),
                },
                Event::CData(c) => match std::str::from_utf8(&c) {
                    Ok(s) => out.push_str(s),
                    Err(e) => return self.xml_err(e),
                },
                Event::End(_) => return Ok(out),
                Event::Start(s) => {
                    let inner = String::from_utf8_lossy(s.local_name().as_ref()).into_owned();
                    return self.err(format!("unexpected element <{inner}> inside <{tag}>"));
                }
                Event::Eof => return self.err(format!("the document ends inside <{tag}>")),
                _ => {}
            }
        }
    }

    /// The raw XML content of the element just started, up to its end tag: the lexical
    /// form of an `rdf:XMLLiteral`.
    fn raw_xml(&mut self) -> Result<String, TrixError> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        loop {
            self.buf.clear();
            let event = match self.xml.read_event_into(&mut self.buf) {
                Ok(e) => e,
                Err(e) => return self.xml_err(e),
            };
            match event {
                Event::Start(s) => {
                    depth += 1;
                    out.push(b'<');
                    out.extend_from_slice(&s);
                    out.push(b'>');
                }
                Event::End(e) => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    out.extend_from_slice(b"</");
                    out.extend_from_slice(&e);
                    out.push(b'>');
                }
                Event::Text(t) => out.extend_from_slice(&t),
                Event::CData(c) => {
                    out.extend_from_slice(b"<![CDATA[");
                    out.extend_from_slice(&c);
                    out.extend_from_slice(b"]]>");
                }
                Event::Comment(c) => {
                    out.extend_from_slice(b"<!--");
                    out.extend_from_slice(&c);
                    out.extend_from_slice(b"-->");
                }
                Event::PI(p) => {
                    out.extend_from_slice(b"<?");
                    out.extend_from_slice(&p);
                    out.extend_from_slice(b"?>");
                }
                Event::Eof => return self.err("the document ends inside <typedLiteral>"),
                _ => {}
            }
        }
        match String::from_utf8(out) {
            Ok(s) => Ok(s),
            Err(e) => self.xml_err(e),
        }
    }

    /// An `<uri>`, `<qname>` or `<id>` element just started, read to its end.
    fn node(&mut self, tag: &str) -> Result<Term, TrixError> {
        let text = self.text(tag)?;
        let text = text.trim();
        match tag {
            "uri" => Ok(Term::NamedNode(self.iri(text)?)),
            "qname" => {
                // the local part may hold more colons
                let Some((prefix, local)) = text.split_once(':') else {
                    return self.err(format!("expected ':' in the prefixed name '{text}'"));
                };
                let probe = format!("{prefix}:x");
                let (ns, _) = self.xml.resolve(QName(probe.as_bytes()), false);
                let ns = match ns {
                    ResolveResult::Bound(ns) => String::from_utf8_lossy(ns.as_ref()).into_owned(),
                    _ => return self.err(format!("undefined namespace prefix '{prefix}'")),
                };
                Ok(Term::NamedNode(self.iri(&format!("{ns}{local}"))?))
            }
            _ => {
                if text.is_empty() {
                    return self.err("an empty blank node label");
                }
                Ok(Term::BlankNode(match BlankNode::new(text) {
                    Ok(b) => b,
                    Err(_) => self.bnodes.entry(text.to_string()).or_default().clone(),
                }))
            }
        }
    }

    fn iri(&self, text: &str) -> Result<NamedNode, TrixError> {
        let resolved = match &self.base {
            Some(b) => b.resolve(text).map(oxiri::Iri::into_inner),
            None => oxiri::Iri::parse(text.to_string()).map(oxiri::Iri::into_inner),
        };
        match resolved {
            Ok(iri) => Ok(NamedNode::new_unchecked(iri)),
            Err(e) => self.err(format!("invalid IRI '{text}': {e}")),
        }
    }

    /// The attributes of an element, leaving out namespace declarations.
    fn attributes(
        &self,
        start: &quick_xml::events::BytesStart<'_>,
    ) -> Result<Vec<Attr>, TrixError> {
        let mut out = Vec::new();
        for a in start.attributes() {
            let a = match a {
                Ok(a) => a,
                Err(e) => return self.xml_err(e),
            };
            let key = a.key;
            if key.as_ref() == b"xmlns" || key.as_ref().starts_with(b"xmlns:") {
                continue;
            }
            let (ns, local) = self.xml.resolve_attribute(key);
            let ns = match ns {
                ResolveResult::Bound(n) => Some(n.as_ref().to_vec()),
                ResolveResult::Unbound => None,
                // the `xml` prefix is bound by definition
                ResolveResult::Unknown(p) if p == b"xml" => Some(XML_NS.to_vec()),
                ResolveResult::Unknown(p) => {
                    return self.err(format!(
                        "undefined namespace prefix '{}'",
                        String::from_utf8_lossy(&p)
                    ));
                }
            };
            let value = match a.unescape_value() {
                Ok(v) => v.into_owned(),
                Err(e) => return self.xml_err(e),
            };
            out.push(Attr {
                ns,
                local: local.as_ref().to_vec(),
                value,
            });
        }
        Ok(out)
    }

    fn plain_literal(
        &mut self,
        start: &quick_xml::events::BytesStart<'_>,
    ) -> Result<Literal, TrixError> {
        let mut lang = None;
        for a in self.attributes(start)? {
            if a.ns.as_deref() == Some(XML_NS) && a.local == b"lang" {
                lang = Some(a.value);
            } else {
                return self.err(format!(
                    "unexpected attribute '{}' on <plainLiteral>",
                    String::from_utf8_lossy(&a.local)
                ));
            }
        }
        let lex = self.text("plainLiteral")?;
        match lang {
            // xml:lang="" undoes an inherited language: a simple literal
            None => Ok(Literal::new_simple_literal(lex)),
            Some(l) if l.is_empty() => Ok(Literal::new_simple_literal(lex)),
            Some(l) => self.lang_literal(lex, &l),
        }
    }

    fn lang_literal(&self, lex: String, tag: &str) -> Result<Literal, TrixError> {
        let r = match tag.split_once("--") {
            Some((lang, dir)) => {
                let dir = match dir.to_ascii_lowercase().as_str() {
                    "ltr" => BaseDirection::Ltr,
                    "rtl" => BaseDirection::Rtl,
                    _ => return self.err(format!("invalid base direction in '{tag}'")),
                };
                Literal::new_directional_language_tagged_literal(lex, lang, dir)
            }
            None => Literal::new_language_tagged_literal(lex, tag),
        };
        match r {
            Ok(l) => Ok(l),
            Err(e) => self.err(format!("invalid language tag '{tag}': {e}")),
        }
    }

    fn typed_literal(
        &mut self,
        start: &quick_xml::events::BytesStart<'_>,
    ) -> Result<Literal, TrixError> {
        let attrs = self.attributes(start)?;
        if attrs.len() > 1 {
            return self.err("<typedLiteral> takes one attribute, datatype");
        }
        let datatype = match attrs.into_iter().next() {
            // the attribute is unqualified, or in the TriX namespace
            Some(a)
                if a.local == b"datatype"
                    && a.ns.as_deref().is_none_or(|n| n == NAMESPACE.as_bytes()) =>
            {
                a.value
            }
            Some(a) => {
                return self.err(format!(
                    "unexpected attribute '{}' on <typedLiteral>",
                    String::from_utf8_lossy(&a.local)
                ));
            }
            None => return self.err("<typedLiteral> without a datatype attribute"),
        };
        let datatype = match NamedNode::new(datatype.trim()) {
            Ok(d) => d,
            Err(e) => return self.err(format!("invalid datatype IRI '{datatype}': {e}")),
        };
        if datatype == rdf::LANG_STRING || datatype == rdf::DIR_LANG_STRING {
            return self.err(format!(
                "a <typedLiteral> cannot have the datatype {datatype}: use <plainLiteral xml:lang=\"…\">"
            ));
        }
        let lex = if datatype == rdf::XML_LITERAL {
            self.raw_xml()?
        } else {
            self.text("typedLiteral")?
        };
        Ok(Literal::new_typed_literal(lex, datatype))
    }
}

// ------------------------------------------------------------------ writer ------

/// The state of a TriX serialization, writing to a writer passed to each call (for
/// writers that own their output elsewhere). [`TrixWriter`] owns its writer.
#[derive(Default)]
pub struct TrixSerializer {
    started: bool,
    /// the graph of the open `<graph>` element
    open: Option<GraphName>,
}

impl TrixSerializer {
    pub fn new() -> Self {
        Self::default()
    }

    fn start(&mut self, w: &mut impl Write) -> io::Result<()> {
        if !self.started {
            self.started = true;
            writeln!(w, "<trix xmlns=\"{NAMESPACE}\">")?;
        }
        Ok(())
    }

    /// Write a quad. Consecutive quads of one graph share a `<graph>` element.
    pub fn quad(&mut self, w: &mut impl Write, q: QuadRef<'_>) -> io::Result<()> {
        self.start(w)?;
        if self
            .open
            .as_ref()
            .is_some_and(|g| g.as_ref() != q.graph_name)
        {
            w.write_all(b"  </graph>\n")?;
            self.open = None;
        }
        if self.open.is_none() {
            w.write_all(b"  <graph>\n")?;
            match q.graph_name {
                GraphNameRef::DefaultGraph => {}
                GraphNameRef::NamedNode(n) => write_node(w, 4, TermRef::NamedNode(n))?,
                GraphNameRef::BlankNode(b) => write_node(w, 4, TermRef::BlankNode(b))?,
            }
            self.open = Some(q.graph_name.into_owned());
        }
        write_triple(w, 4, TripleRef::new(q.subject, q.predicate, q.object))
    }

    /// Write a triple of the default graph.
    pub fn triple(&mut self, w: &mut impl Write, t: TripleRef<'_>) -> io::Result<()> {
        self.quad(w, t.in_graph(GraphNameRef::DefaultGraph))
    }

    /// Close the open elements. A document without quads is an empty `<trix>`.
    pub fn finish(&mut self, w: &mut impl Write) -> io::Result<()> {
        self.start(w)?;
        if self.open.take().is_some() {
            w.write_all(b"  </graph>\n")?;
        }
        w.write_all(b"</trix>\n")
    }
}

/// A TriX writer that owns its output.
pub struct TrixWriter<W: Write> {
    w: W,
    ser: TrixSerializer,
}

impl<W: Write> TrixWriter<W> {
    pub fn new(w: W) -> Self {
        TrixWriter {
            w,
            ser: TrixSerializer::new(),
        }
    }

    pub fn quad<'a>(&mut self, q: impl Into<QuadRef<'a>>) -> io::Result<()> {
        self.ser.quad(&mut self.w, q.into())
    }

    pub fn triple<'a>(&mut self, t: impl Into<TripleRef<'a>>) -> io::Result<()> {
        self.ser.triple(&mut self.w, t.into())
    }

    /// End the document and flush; returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.ser.finish(&mut self.w)?;
        self.w.flush()?;
        Ok(self.w)
    }
}

fn indent(w: &mut impl Write, n: usize) -> io::Result<()> {
    const SPACES: &[u8] = b"                                                                ";
    let mut n = n;
    while n > 0 {
        let k = n.min(SPACES.len());
        w.write_all(&SPACES[..k])?;
        n -= k;
    }
    Ok(())
}

fn write_triple(w: &mut impl Write, depth: usize, t: TripleRef<'_>) -> io::Result<()> {
    indent(w, depth)?;
    w.write_all(b"<triple>\n")?;
    write_node(w, depth + 2, t.subject.into())?;
    write_node(w, depth + 2, t.predicate.into())?;
    write_node(w, depth + 2, t.object)?;
    indent(w, depth)?;
    w.write_all(b"</triple>\n")
}

fn write_node(w: &mut impl Write, depth: usize, t: TermRef<'_>) -> io::Result<()> {
    match t {
        TermRef::NamedNode(n) => {
            indent(w, depth)?;
            w.write_all(b"<uri>")?;
            escape_text(w, n.as_str())?;
            w.write_all(b"</uri>\n")
        }
        TermRef::BlankNode(b) => {
            indent(w, depth)?;
            w.write_all(b"<id>")?;
            escape_text(w, b.as_str())?;
            w.write_all(b"</id>\n")
        }
        TermRef::Literal(l) => {
            indent(w, depth)?;
            if let Some(lang) = l.language() {
                w.write_all(b"<plainLiteral xml:lang=\"")?;
                escape_attribute(w, lang)?;
                match l.direction() {
                    Some(BaseDirection::Ltr) => w.write_all(b"--ltr")?,
                    Some(BaseDirection::Rtl) => w.write_all(b"--rtl")?,
                    None => {}
                }
                w.write_all(b"\">")?;
                escape_text(w, l.value())?;
                w.write_all(b"</plainLiteral>\n")
            } else if l.datatype() == xsd::STRING {
                w.write_all(b"<plainLiteral>")?;
                escape_text(w, l.value())?;
                w.write_all(b"</plainLiteral>\n")
            } else {
                w.write_all(b"<typedLiteral datatype=\"")?;
                escape_attribute(w, l.datatype().as_str())?;
                w.write_all(b"\">")?;
                if l.datatype() == rdf::XML_LITERAL && is_xml_content(l.value()) {
                    // well-formed XML content is written as XML, as Jena does
                    w.write_all(l.value().as_bytes())?;
                } else {
                    escape_text(w, l.value())?;
                }
                w.write_all(b"</typedLiteral>\n")
            }
        }
        TermRef::Triple(t) => write_triple(w, depth, t.as_ref()),
    }
}

/// Whether `s` is well-formed XML content (balanced elements, valid references).
fn is_xml_content(s: &str) -> bool {
    let doc = format!("<x>{s}</x>");
    let mut r = quick_xml::Reader::from_str(&doc);
    r.config_mut().check_end_names = true;
    let mut depth = 0i64;
    loop {
        match r.read_event() {
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(_)) => depth -= 1,
            Ok(Event::Text(t)) => {
                if t.unescape().is_err() {
                    return false;
                }
            }
            Ok(Event::Eof) => return depth == 0,
            Ok(Event::Decl(_) | Event::DocType(_)) => return false,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

/// A character XML 1.0 cannot hold, even as a character reference.
fn xml_forbidden(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}')
}

fn forbidden_error(c: char) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "TriX (XML 1.0) cannot hold the character U+{:04X}",
            c as u32
        ),
    )
}

fn escape_text(w: &mut impl Write, s: &str) -> io::Result<()> {
    let mut last = 0;
    for (i, c) in s.char_indices() {
        let rep: &[u8] = match c {
            '&' => b"&amp;",
            '<' => b"&lt;",
            '>' => b"&gt;",
            // a literal carriage return would read back as a line feed
            '\r' => b"&#13;",
            c if xml_forbidden(c) => return Err(forbidden_error(c)),
            _ => continue,
        };
        w.write_all(&s.as_bytes()[last..i])?;
        w.write_all(rep)?;
        last = i + c.len_utf8();
    }
    w.write_all(&s.as_bytes()[last..])
}

fn escape_attribute(w: &mut impl Write, s: &str) -> io::Result<()> {
    let mut last = 0;
    for (i, c) in s.char_indices() {
        let rep: &[u8] = match c {
            '&' => b"&amp;",
            '<' => b"&lt;",
            '>' => b"&gt;",
            '"' => b"&quot;",
            '\r' => b"&#13;",
            '\n' => b"&#10;",
            '\t' => b"&#9;",
            c if xml_forbidden(c) => return Err(forbidden_error(c)),
            _ => continue,
        };
        w.write_all(&s.as_bytes()[last..i])?;
        w.write_all(rep)?;
        last = i + c.len_utf8();
    }
    w.write_all(&s.as_bytes()[last..])
}

#[cfg(test)]
mod tests;
