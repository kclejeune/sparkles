//! RDF syntaxes Jena speaks that the Rust RDF crates do not: RDF Thrift and RDF
//! Protobuf, Jena's binary encodings, RDF/JSON, and TriX (whose reader and writer are
//! [`crate::trix`]). `RDFConnectionFuseki` sends graphs
//! and datasets in RDF Thrift and asks for RDF Thrift back, and for SELECT results in
//! SPARQL Results Thrift.
//!
//! Request bodies in these syntaxes are read into N-Quads or N-Triples for the loader
//! ([`transcode`]). Responses are written by [`RdfWriter`] and [`ThriftResults`].
//!
//! The encodings follow Jena's `BinaryRDF.thrift` (the Thrift compact protocol, one
//! `RDF_StreamRow` after another) and `binary-rdf.proto` (length-delimited
//! `RDF_StreamRow` messages). Readers accept every form Jena writes: prefix names,
//! values (`valInteger`, `valDouble`, `valDecimal`) and triple terms. Writers write
//! full IRIs and literals, as Jena's default `RDF_THRIFT` and `RDF_PROTO` formats do.

use oxrdf::vocab::xsd;
use oxrdf::{
    BaseDirection, BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple,
};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, BufReader, Read, Write};

/// A syntax of this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JenaFormat {
    Thrift,
    Protobuf,
    RdfJson,
    TriX,
}

impl JenaFormat {
    pub fn from_media_type(ct: &str) -> Option<JenaFormat> {
        let base = ct.split(';').next()?.trim().to_ascii_lowercase();
        Some(match base.as_str() {
            "application/rdf+thrift" => JenaFormat::Thrift,
            "application/rdf+protobuf" | "application/x-protobuf" => JenaFormat::Protobuf,
            "application/rdf+json" => JenaFormat::RdfJson,
            "application/trix+xml" | "application/trix" => JenaFormat::TriX,
            _ => return None,
        })
    }

    /// The format of a file name's extension (Jena's: `rt`, `trdf`, `rpb`, `pbrdf`, `rj`,
    /// `trix`).
    pub fn from_file_name(name: &str) -> Option<JenaFormat> {
        let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
        Some(match ext.as_str() {
            "rt" | "trdf" => JenaFormat::Thrift,
            "rpb" | "pbrdf" => JenaFormat::Protobuf,
            "rj" => JenaFormat::RdfJson,
            "trix" => JenaFormat::TriX,
            _ => return None,
        })
    }

    /// The format of a file path, looking past a compression extension
    /// (`data.trix.gz`).
    pub fn from_path(path: &std::path::Path) -> Option<JenaFormat> {
        let name = path.file_name()?.to_str()?;
        JenaFormat::from_file_name(crate::codec::Codec::strip_extension(name))
    }

    /// The format by one of the names `sparkles convert --syntax` and `?format=` take,
    /// or by media type.
    pub fn from_name(name: &str) -> Option<JenaFormat> {
        Some(match name.to_ascii_lowercase().as_str() {
            "trix" => JenaFormat::TriX,
            "thrift" | "rdf-thrift" | "rt" | "trdf" => JenaFormat::Thrift,
            "protobuf" | "rdf-protobuf" | "rpb" | "pbrdf" => JenaFormat::Protobuf,
            "rdf/json" | "rdfjson" | "rdf-json" | "json-rdf" | "rj" => JenaFormat::RdfJson,
            other => return JenaFormat::from_media_type(other),
        })
    }

    /// Whether the format holds named graphs (RDF/JSON holds one graph).
    pub fn quads(self) -> bool {
        self != JenaFormat::RdfJson
    }

    pub fn file_extension(self) -> &'static str {
        match self {
            JenaFormat::Thrift => "rt",
            JenaFormat::Protobuf => "rpb",
            JenaFormat::RdfJson => "rj",
            JenaFormat::TriX => crate::trix::FILE_EXTENSION,
        }
    }

    pub fn media_type(self) -> &'static str {
        match self {
            JenaFormat::Thrift => "application/rdf+thrift",
            JenaFormat::Protobuf => "application/rdf+protobuf",
            JenaFormat::RdfJson => "application/rdf+json",
            JenaFormat::TriX => crate::trix::MEDIA_TYPE,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            JenaFormat::Thrift => "RDF Thrift",
            JenaFormat::Protobuf => "RDF Protobuf",
            JenaFormat::RdfJson => "RDF/JSON",
            JenaFormat::TriX => "TriX",
        }
    }
}

/// A malformed body.
#[derive(Debug)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

type Res<T> = Result<T, DecodeError>;

fn bad<T>(msg: impl Into<String>) -> Res<T> {
    Err(DecodeError(msg.into()))
}

impl From<crate::trix::TrixError> for DecodeError {
    fn from(e: crate::trix::TrixError) -> DecodeError {
        DecodeError(e.to_string())
    }
}

impl From<io::Error> for DecodeError {
    fn from(e: io::Error) -> DecodeError {
        DecodeError(format!("cannot read the body: {e}"))
    }
}

/// Read `input` in `fmt` and write it to `out` as N-Quads, or as N-Triples when
/// `quads` is false (then a quad in a named graph is an error). Returns the number of
/// statements.
pub fn transcode(
    fmt: JenaFormat,
    input: impl Read,
    quads: bool,
    out: impl Write,
) -> Result<u64, DecodeError> {
    transcode_with_base(fmt, input, quads, out, None)
}

/// [`transcode`], resolving the relative IRIs of a TriX document against `base` (the
/// other formats hold absolute IRIs only).
pub fn transcode_with_base(
    fmt: JenaFormat,
    input: impl Read,
    quads: bool,
    out: impl Write,
    base: Option<&str>,
) -> Result<u64, DecodeError> {
    let target = if quads {
        oxrdfio::RdfFormat::NQuads
    } else {
        oxrdfio::RdfFormat::NTriples
    };
    let mut ser = oxrdfio::RdfSerializer::from_format(target).for_writer(out);
    let mut n = 0u64;
    let mut sink = |q: Quad| -> Res<()> {
        if !quads && q.graph_name != GraphName::DefaultGraph {
            return bad(format!(
                "{} body for a graph holds a quad in graph {}",
                fmt.name(),
                q.graph_name
            ));
        }
        ser.serialize_quad(&q)?;
        n += 1;
        Ok(())
    };
    match fmt {
        JenaFormat::Thrift => ThriftReader::new(input).read_stream(&mut sink)?,
        JenaFormat::Protobuf => ProtoReader::new(input).read_stream(&mut sink)?,
        JenaFormat::RdfJson => read_rdf_json(input, &mut sink)?,
        JenaFormat::TriX => crate::trix::parse(BufReader::new(input), base, &mut sink)?,
    }
    ser.finish()?;
    Ok(n)
}

// --------------------------------------------------------------- terms ------

/// A term as the wire formats carry it, before prefix names are expanded.
enum WireTerm {
    Iri(String),
    BNode(String),
    Literal {
        lex: String,
        lang: Option<String>,
        dir: Option<String>,
        datatype: Option<String>,
        dt_prefix: Option<(String, String)>,
    },
    PrefixName(String, String),
    Triple(Box<[WireTerm; 3]>),
    Integer(i64),
    Double(f64),
    Decimal(i64, i32),
    /// variables, ANY, UNDEF, REPEAT: not RDF data
    NotData(&'static str),
}

/// Turns wire terms into RDF terms: prefix names expanded with the stream's prefixes,
/// blank node labels mapped to blank nodes (one per label).
#[derive(Default)]
struct Terms {
    prefixes: HashMap<String, String>,
    bnodes: HashMap<String, BlankNode>,
}

impl Terms {
    /// An IRI, or a blank node for Jena's `_:label` form (`RiotLib.createIRIorBNode`).
    fn iri(&mut self, s: String) -> Res<Term> {
        if let Some(label) = s.strip_prefix("_:") {
            return Ok(Term::BlankNode(self.bnode(label.to_string())));
        }
        NamedNode::new(s)
            .map(Term::NamedNode)
            .map_err(|e| DecodeError(format!("invalid IRI: {e}")))
    }

    fn bnode(&mut self, label: String) -> BlankNode {
        self.bnodes.entry(label).or_default().clone()
    }

    fn expand(&self, prefix: &str, local: &str) -> Res<String> {
        match self.prefixes.get(prefix) {
            Some(ns) => Ok(format!("{ns}{local}")),
            None => bad(format!("undeclared prefix '{prefix}:'")),
        }
    }

    fn term(&mut self, t: WireTerm) -> Res<Term> {
        Ok(match t {
            WireTerm::Iri(s) => self.iri(s)?,
            WireTerm::BNode(l) => Term::BlankNode(self.bnode(l)),
            WireTerm::PrefixName(p, l) => {
                let iri = self.expand(&p, &l)?;
                self.iri(iri)?
            }
            WireTerm::Literal {
                lex,
                lang,
                dir,
                datatype,
                dt_prefix,
            } => {
                let lang = lang.filter(|l| !l.is_empty());
                if let Some(lang) = lang {
                    let dir = match dir.as_deref().filter(|d| !d.is_empty()) {
                        None => None,
                        Some("ltr") => Some(BaseDirection::Ltr),
                        Some("rtl") => Some(BaseDirection::Rtl),
                        Some(d) => return bad(format!("invalid base direction '{d}'")),
                    };
                    let lit = match dir {
                        Some(d) => Literal::new_directional_language_tagged_literal(lex, lang, d),
                        None => Literal::new_language_tagged_literal(lex, lang),
                    };
                    Term::Literal(
                        lit.map_err(|e| DecodeError(format!("invalid language tag: {e}")))?,
                    )
                } else {
                    let dt = match (datatype.filter(|d| !d.is_empty()), dt_prefix) {
                        (Some(d), _) => Some(d),
                        (None, Some((p, l))) => Some(self.expand(&p, &l)?),
                        (None, None) => None,
                    };
                    match dt {
                        None => Term::Literal(Literal::new_simple_literal(lex)),
                        Some(d) => {
                            let d = NamedNode::new(d)
                                .map_err(|e| DecodeError(format!("invalid datatype IRI: {e}")))?;
                            Term::Literal(Literal::new_typed_literal(lex, d))
                        }
                    }
                }
            }
            WireTerm::Integer(i) => {
                Term::Literal(Literal::new_typed_literal(i.to_string(), xsd::INTEGER))
            }
            WireTerm::Double(d) => {
                Term::Literal(Literal::new_typed_literal(double_lexical(d), xsd::DOUBLE))
            }
            WireTerm::Decimal(v, scale) => Term::Literal(Literal::new_typed_literal(
                decimal_lexical(v, scale),
                xsd::DECIMAL,
            )),
            WireTerm::Triple(t) => {
                let [s, p, o] = *t;
                Term::Triple(Box::new(self.triple(s, p, o)?))
            }
            WireTerm::NotData(what) => return bad(format!("{what} in RDF data")),
        })
    }

    fn triple(&mut self, s: WireTerm, p: WireTerm, o: WireTerm) -> Res<Triple> {
        let subject = match self.term(s)? {
            Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
            Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
            t => return bad(format!("subject {t} is neither an IRI nor a blank node")),
        };
        let predicate = match self.term(p)? {
            Term::NamedNode(n) => n,
            t => return bad(format!("predicate {t} is not an IRI")),
        };
        Ok(Triple {
            subject,
            predicate,
            object: self.term(o)?,
        })
    }

    fn quad(&mut self, s: WireTerm, p: WireTerm, o: WireTerm, g: Option<WireTerm>) -> Res<Quad> {
        let t = self.triple(s, p, o)?;
        let graph_name = match g {
            None => GraphName::DefaultGraph,
            Some(g) => match self.term(g)? {
                Term::NamedNode(n) if is_default_graph(n.as_str()) => GraphName::DefaultGraph,
                Term::NamedNode(n) => GraphName::NamedNode(n),
                Term::BlankNode(b) => GraphName::BlankNode(b),
                t => return bad(format!("graph name {t} is neither an IRI nor a blank node")),
            },
        };
        Ok(Quad {
            subject: t.subject,
            predicate: t.predicate,
            object: t.object,
            graph_name,
        })
    }
}

/// Jena's names for the default graph.
fn is_default_graph(iri: &str) -> bool {
    matches!(iri, "urn:x-arq:DefaultGraph" | "urn:x-arq:DefaultGraphNode")
}

/// A lexical form of an `xsd:double`.
fn double_lexical(d: f64) -> String {
    if d.is_nan() {
        "NaN".into()
    } else if d.is_infinite() {
        if d > 0.0 { "INF" } else { "-INF" }.into()
    } else {
        // the canonical form: a mantissa with a decimal point
        let s = format!("{d:E}");
        match s.split_once('E') {
            Some((m, e)) if !m.contains('.') => format!("{m}.0E{e}"),
            _ => s,
        }
    }
}

/// The plain decimal `value × 10^-scale` (Java's `BigDecimal.toPlainString`).
fn decimal_lexical(value: i64, scale: i32) -> String {
    let neg = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut s = if scale <= 0 {
        format!("{digits}{}", "0".repeat(scale.unsigned_abs() as usize))
    } else {
        let scale = scale as usize;
        let padded = if digits.len() <= scale {
            format!("{}{digits}", "0".repeat(scale + 1 - digits.len()))
        } else {
            digits
        };
        let (int, frac) = padded.split_at(padded.len() - scale);
        format!("{int}.{frac}")
    };
    if neg {
        s.insert(0, '-');
    }
    s
}

// --------------------------------------------------- Thrift compact protocol ------

mod ttype {
    pub const TRUE: u8 = 1;
    pub const FALSE: u8 = 2;
    pub const BYTE: u8 = 3;
    pub const I16: u8 = 4;
    pub const I32: u8 = 5;
    pub const I64: u8 = 6;
    pub const DOUBLE: u8 = 7;
    pub const BINARY: u8 = 8;
    pub const LIST: u8 = 9;
    pub const SET: u8 = 10;
    pub const MAP: u8 = 11;
    pub const STRUCT: u8 = 12;
}

/// The longest string a body may hold (a guard against a corrupt length).
const MAX_STRING: u64 = 1 << 30;
/// How deeply terms may nest (triple terms in triple terms).
const MAX_DEPTH: usize = 64;

struct ThriftReader<R> {
    r: BufReader<R>,
}

impl<R: Read> ThriftReader<R> {
    fn new(r: R) -> Self {
        ThriftReader {
            r: BufReader::with_capacity(64 << 10, r),
        }
    }

    fn at_end(&mut self) -> Res<bool> {
        Ok(self.r.fill_buf()?.is_empty())
    }

    fn byte(&mut self) -> Res<u8> {
        let mut b = [0u8; 1];
        match self.r.read_exact(&mut b) {
            Ok(()) => Ok(b[0]),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => bad("truncated RDF Thrift body"),
            Err(e) => Err(e.into()),
        }
    }

    fn varint(&mut self) -> Res<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bad("malformed varint in RDF Thrift body")
    }

    fn zigzag(&mut self) -> Res<i64> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }

    fn binary(&mut self) -> Res<Vec<u8>> {
        let n = self.varint()?;
        if n > MAX_STRING {
            return bad("string too long in RDF Thrift body");
        }
        let mut buf = Vec::new();
        (&mut self.r).take(n).read_to_end(&mut buf)?;
        if buf.len() as u64 != n {
            return bad("truncated RDF Thrift body");
        }
        Ok(buf)
    }

    fn string(&mut self) -> Res<String> {
        String::from_utf8(self.binary()?)
            .map_err(|_| DecodeError("invalid UTF-8 in RDF Thrift body".into()))
    }

    /// The next field of a struct (`None` at its end): its id and type.
    fn field(&mut self, last: &mut i16) -> Res<Option<(i16, u8)>> {
        let b = self.byte()?;
        if b == 0 {
            return Ok(None);
        }
        let ty = b & 0x0f;
        let delta = (b >> 4) as i16;
        let id = if delta == 0 {
            self.zigzag()? as i16
        } else {
            *last + delta
        };
        *last = id;
        Ok(Some((id, ty)))
    }

    fn skip(&mut self, ty: u8, depth: usize) -> Res<()> {
        if depth > MAX_DEPTH {
            return bad("RDF Thrift body nests too deeply");
        }
        match ty {
            ttype::TRUE | ttype::FALSE => {}
            ttype::BYTE => {
                self.byte()?;
            }
            ttype::I16 | ttype::I32 | ttype::I64 => {
                self.varint()?;
            }
            ttype::DOUBLE => {
                let mut b = [0u8; 8];
                self.r.read_exact(&mut b)?;
            }
            ttype::BINARY => {
                self.binary()?;
            }
            ttype::LIST | ttype::SET => {
                let (n, et) = self.list_header()?;
                for _ in 0..n {
                    if et == ttype::TRUE || et == ttype::FALSE {
                        self.byte()?;
                    } else {
                        self.skip(et, depth + 1)?;
                    }
                }
            }
            ttype::MAP => {
                let n = self.varint()?;
                if n > 0 {
                    let kv = self.byte()?;
                    for _ in 0..n {
                        self.skip(kv >> 4, depth + 1)?;
                        self.skip(kv & 0x0f, depth + 1)?;
                    }
                }
            }
            ttype::STRUCT => {
                let mut last = 0;
                while let Some((_, t)) = self.field(&mut last)? {
                    self.skip(t, depth + 1)?;
                }
            }
            t => return bad(format!("unknown Thrift type {t} in RDF Thrift body")),
        }
        Ok(())
    }

    fn list_header(&mut self) -> Res<(u64, u8)> {
        let b = self.byte()?;
        let n = (b >> 4) as u64;
        let n = if n == 15 { self.varint()? } else { n };
        Ok((n, b & 0x0f))
    }

    fn double(&mut self) -> Res<f64> {
        let mut b = [0u8; 8];
        self.r.read_exact(&mut b)?;
        Ok(f64::from_le_bytes(b))
    }

    /// A struct of string fields: field id → value (other fields skipped).
    fn strings(&mut self, depth: usize) -> Res<BTreeMap<i16, String>> {
        let mut out = BTreeMap::new();
        let mut last = 0;
        while let Some((id, ty)) = self.field(&mut last)? {
            if ty == ttype::BINARY {
                out.insert(id, self.string()?);
            } else {
                self.skip(ty, depth + 1)?;
            }
        }
        Ok(out)
    }

    fn term(&mut self, depth: usize) -> Res<WireTerm> {
        if depth > MAX_DEPTH {
            return bad("RDF Thrift body nests too deeply");
        }
        let mut last = 0;
        let mut term = None;
        while let Some((id, ty)) = self.field(&mut last)? {
            let t = match (id, ty) {
                (1, ttype::STRUCT) => {
                    WireTerm::Iri(self.strings(depth)?.remove(&1).unwrap_or_default())
                }
                (2, ttype::STRUCT) => {
                    WireTerm::BNode(self.strings(depth)?.remove(&1).unwrap_or_default())
                }
                (3, ttype::STRUCT) => self.literal(depth)?,
                (4, ttype::STRUCT) => {
                    let mut m = self.strings(depth)?;
                    WireTerm::PrefixName(
                        m.remove(&1).unwrap_or_default(),
                        m.remove(&2).unwrap_or_default(),
                    )
                }
                (5, ttype::STRUCT) => {
                    self.skip(ty, depth)?;
                    WireTerm::NotData("a variable")
                }
                (6, ttype::STRUCT) => {
                    self.skip(ty, depth)?;
                    WireTerm::NotData("ANY")
                }
                (7, ttype::STRUCT) => {
                    self.skip(ty, depth)?;
                    WireTerm::NotData("an undefined term")
                }
                (8, ttype::STRUCT) => {
                    self.skip(ty, depth)?;
                    WireTerm::NotData("a repeated term")
                }
                (9, ttype::STRUCT) => {
                    let [s, p, o, _] = self.statement(depth + 1)?;
                    WireTerm::Triple(Box::new([
                        s.unwrap_or(WireTerm::NotData("a missing subject")),
                        p.unwrap_or(WireTerm::NotData("a missing predicate")),
                        o.unwrap_or(WireTerm::NotData("a missing object")),
                    ]))
                }
                (10, ttype::I64) => WireTerm::Integer(self.zigzag()?),
                (11, ttype::DOUBLE) => WireTerm::Double(self.double()?),
                (12, ttype::STRUCT) => {
                    let (mut value, mut scale) = (0i64, 0i32);
                    let mut l = 0;
                    while let Some((fid, fty)) = self.field(&mut l)? {
                        match (fid, fty) {
                            (1, ttype::I64) => value = self.zigzag()?,
                            (2, ttype::I32) => scale = self.zigzag()? as i32,
                            _ => self.skip(fty, depth + 1)?,
                        }
                    }
                    WireTerm::Decimal(value, scale)
                }
                _ => {
                    self.skip(ty, depth)?;
                    continue;
                }
            };
            term = Some(t);
        }
        term.ok_or_else(|| DecodeError("an empty term in RDF Thrift body".into()))
    }

    fn literal(&mut self, depth: usize) -> Res<WireTerm> {
        let (mut lex, mut lang, mut dir, mut datatype, mut dt_prefix) =
            (String::new(), None, None, None, None);
        let mut last = 0;
        while let Some((id, ty)) = self.field(&mut last)? {
            match (id, ty) {
                (1, ttype::BINARY) => lex = self.string()?,
                (2, ttype::BINARY) => lang = Some(self.string()?),
                (3, ttype::BINARY) => datatype = Some(self.string()?),
                (5, ttype::BINARY) => dir = Some(self.string()?),
                (4, ttype::STRUCT) => {
                    let mut m = self.strings(depth + 1)?;
                    dt_prefix = Some((
                        m.remove(&1).unwrap_or_default(),
                        m.remove(&2).unwrap_or_default(),
                    ));
                }
                _ => self.skip(ty, depth + 1)?,
            }
        }
        Ok(WireTerm::Literal {
            lex,
            lang,
            dir,
            datatype,
            dt_prefix,
        })
    }

    /// An `RDF_Triple` or `RDF_Quad`: S, P, O and G.
    fn statement(&mut self, depth: usize) -> Res<[Option<WireTerm>; 4]> {
        let mut out = [None, None, None, None];
        let mut last = 0;
        while let Some((id, ty)) = self.field(&mut last)? {
            match (id, ty) {
                (1..=4, ttype::STRUCT) => out[id as usize - 1] = Some(self.term(depth + 1)?),
                _ => self.skip(ty, depth)?,
            }
        }
        Ok(out)
    }

    fn read_stream(&mut self, sink: &mut dyn FnMut(Quad) -> Res<()>) -> Res<()> {
        let mut terms = Terms::default();
        while !self.at_end()? {
            let mut last = 0;
            while let Some((id, ty)) = self.field(&mut last)? {
                match (id, ty) {
                    (1, ttype::STRUCT) => {
                        let mut m = self.strings(0)?;
                        terms.prefixes.insert(
                            m.remove(&1).unwrap_or_default(),
                            m.remove(&2).unwrap_or_default(),
                        );
                    }
                    (2 | 3, ttype::STRUCT) => {
                        let [s, p, o, g] = self.statement(0)?;
                        let (Some(s), Some(p), Some(o)) = (s, p, o) else {
                            return bad("a statement without subject, predicate or object");
                        };
                        sink(terms.quad(s, p, o, g)?)?;
                    }
                    _ => self.skip(ty, 0)?,
                }
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ Protobuf ------

struct ProtoReader<R> {
    r: BufReader<R>,
}

/// A protobuf message being read from a slice.
struct Msg<'a> {
    b: &'a [u8],
}

enum Field<'a> {
    Varint(u64),
    Fixed64([u8; 8]),
    Bytes(&'a [u8]),
    Fixed32,
}

fn proto_varint(b: &mut &[u8]) -> Res<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let Some((&x, rest)) = b.split_first() else {
            return bad("truncated RDF Protobuf message");
        };
        *b = rest;
        v |= u64::from(x & 0x7f) << shift;
        if x & 0x80 == 0 {
            return Ok(v);
        }
    }
    bad("malformed varint in RDF Protobuf body")
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

impl<'a> Msg<'a> {
    fn next(&mut self) -> Res<Option<(u64, Field<'a>)>> {
        if self.b.is_empty() {
            return Ok(None);
        }
        let key = proto_varint(&mut self.b)?;
        let field = match key & 7 {
            0 => Field::Varint(proto_varint(&mut self.b)?),
            1 => {
                if self.b.len() < 8 {
                    return bad("truncated RDF Protobuf message");
                }
                let (x, rest) = self.b.split_at(8);
                self.b = rest;
                Field::Fixed64(x.try_into().unwrap())
            }
            2 => {
                let n = proto_varint(&mut self.b)? as usize;
                if self.b.len() < n {
                    return bad("truncated RDF Protobuf message");
                }
                let (x, rest) = self.b.split_at(n);
                self.b = rest;
                Field::Bytes(x)
            }
            5 => {
                if self.b.len() < 4 {
                    return bad("truncated RDF Protobuf message");
                }
                self.b = &self.b[4..];
                Field::Fixed32
            }
            w => return bad(format!("unsupported protobuf wire type {w}")),
        };
        Ok(Some((key >> 3, field)))
    }
}

fn utf8(b: &[u8]) -> Res<String> {
    String::from_utf8(b.to_vec())
        .map_err(|_| DecodeError("invalid UTF-8 in RDF Protobuf body".into()))
}

/// A message of string fields: field number → value.
fn proto_strings(b: &[u8]) -> Res<BTreeMap<u64, String>> {
    let mut m = Msg { b };
    let mut out = BTreeMap::new();
    while let Some((n, f)) = m.next()? {
        if let Field::Bytes(x) = f {
            out.insert(n, utf8(x)?);
        }
    }
    Ok(out)
}

fn proto_term(b: &[u8], depth: usize) -> Res<WireTerm> {
    if depth > MAX_DEPTH {
        return bad("RDF Protobuf body nests too deeply");
    }
    let mut m = Msg { b };
    let mut term = None;
    while let Some((n, f)) = m.next()? {
        let t = match (n, f) {
            (1, Field::Bytes(x)) => WireTerm::Iri(proto_strings(x)?.remove(&1).unwrap_or_default()),
            (2, Field::Bytes(x)) => {
                WireTerm::BNode(proto_strings(x)?.remove(&1).unwrap_or_default())
            }
            (3, Field::Bytes(x)) => proto_literal(x)?,
            (4, Field::Bytes(x)) => {
                let mut s = proto_strings(x)?;
                WireTerm::PrefixName(
                    s.remove(&1).unwrap_or_default(),
                    s.remove(&2).unwrap_or_default(),
                )
            }
            (5, _) => WireTerm::NotData("a variable"),
            (6, Field::Bytes(x)) => {
                let [s, p, o, _] = proto_statement(x, depth + 1)?;
                WireTerm::Triple(Box::new([
                    s.unwrap_or(WireTerm::NotData("a missing subject")),
                    p.unwrap_or(WireTerm::NotData("a missing predicate")),
                    o.unwrap_or(WireTerm::NotData("a missing object")),
                ]))
            }
            (7, _) => WireTerm::NotData("ANY"),
            (8, _) => WireTerm::NotData("an undefined term"),
            (9, _) => WireTerm::NotData("a repeated term"),
            (20, Field::Varint(v)) => WireTerm::Integer(unzigzag(v)),
            (21, Field::Fixed64(x)) => WireTerm::Double(f64::from_le_bytes(x)),
            (22, Field::Bytes(x)) => {
                let mut d = Msg { b: x };
                let (mut value, mut scale) = (0i64, 0i32);
                while let Some((dn, df)) = d.next()? {
                    match (dn, df) {
                        (1, Field::Varint(v)) => value = unzigzag(v),
                        (2, Field::Varint(v)) => scale = unzigzag(v) as i32,
                        _ => {}
                    }
                }
                WireTerm::Decimal(value, scale)
            }
            _ => continue,
        };
        term = Some(t);
    }
    term.ok_or_else(|| DecodeError("an empty term in RDF Protobuf body".into()))
}

fn proto_literal(b: &[u8]) -> Res<WireTerm> {
    let mut m = Msg { b };
    let (mut lex, mut lang, mut dir, mut datatype, mut dt_prefix) =
        (String::new(), None, None, None, None);
    while let Some((n, f)) = m.next()? {
        match (n, f) {
            (1, Field::Bytes(x)) => lex = utf8(x)?,
            (2, Field::Bytes(x)) => lang = Some(utf8(x)?),
            (5, Field::Bytes(x)) => {
                let ld = utf8(x)?;
                match ld.split_once("--") {
                    Some((l, d)) => {
                        lang = Some(l.to_string());
                        dir = Some(d.to_string());
                    }
                    None => return bad(format!("invalid language and direction '{ld}'")),
                }
            }
            (3, Field::Bytes(x)) => datatype = Some(utf8(x)?),
            (4, Field::Bytes(x)) => {
                let mut s = proto_strings(x)?;
                dt_prefix = Some((
                    s.remove(&1).unwrap_or_default(),
                    s.remove(&2).unwrap_or_default(),
                ));
            }
            _ => {}
        }
    }
    Ok(WireTerm::Literal {
        lex,
        lang,
        dir,
        datatype,
        dt_prefix,
    })
}

fn proto_statement(b: &[u8], depth: usize) -> Res<[Option<WireTerm>; 4]> {
    let mut m = Msg { b };
    let mut out = [None, None, None, None];
    while let Some((n, f)) = m.next()? {
        if let (1..=4, Field::Bytes(x)) = (n, f) {
            out[n as usize - 1] = Some(proto_term(x, depth + 1)?);
        }
    }
    Ok(out)
}

impl<R: Read> ProtoReader<R> {
    fn new(r: R) -> Self {
        ProtoReader {
            r: BufReader::with_capacity(64 << 10, r),
        }
    }

    /// The next length-delimited message, `None` at the end.
    fn message(&mut self, buf: &mut Vec<u8>) -> Res<bool> {
        if self.r.fill_buf()?.is_empty() {
            return Ok(false);
        }
        let mut n = 0u64;
        for shift in (0..64).step_by(7) {
            let mut b = [0u8; 1];
            if self.r.read_exact(&mut b).is_err() {
                return bad("truncated RDF Protobuf body");
            }
            n |= u64::from(b[0] & 0x7f) << shift;
            if b[0] & 0x80 == 0 {
                break;
            }
        }
        if n > MAX_STRING {
            return bad("message too long in RDF Protobuf body");
        }
        buf.clear();
        (&mut self.r).take(n).read_to_end(buf)?;
        if buf.len() as u64 != n {
            return bad("truncated RDF Protobuf body");
        }
        Ok(true)
    }

    fn read_stream(&mut self, sink: &mut dyn FnMut(Quad) -> Res<()>) -> Res<()> {
        let mut terms = Terms::default();
        let mut buf = Vec::new();
        while self.message(&mut buf)? {
            let mut m = Msg { b: &buf };
            while let Some((n, f)) = m.next()? {
                match (n, f) {
                    (1, Field::Bytes(x)) => {
                        let mut s = proto_strings(x)?;
                        terms.prefixes.insert(
                            s.remove(&1).unwrap_or_default(),
                            s.remove(&2).unwrap_or_default(),
                        );
                    }
                    (2 | 3, Field::Bytes(x)) => {
                        let [s, p, o, g] = proto_statement(x, 0)?;
                        let (Some(s), Some(p), Some(o)) = (s, p, o) else {
                            return bad("a statement without subject, predicate or object");
                        };
                        sink(terms.quad(s, p, o, g)?)?;
                    }
                    // the base IRI: Jena resolves nothing against it either
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ RDF/JSON ------

fn read_rdf_json(input: impl Read, sink: &mut dyn FnMut(Quad) -> Res<()>) -> Res<()> {
    use serde_json::Value as J;
    let doc: J = serde_json::from_reader(input)
        .map_err(|e| DecodeError(format!("invalid RDF/JSON: {e}")))?;
    let J::Object(subjects) = doc else {
        return bad("RDF/JSON: the document is not an object");
    };
    let mut terms = Terms::default();
    for (s, preds) in subjects {
        let J::Object(preds) = preds else {
            return bad(format!(
                "RDF/JSON: the value of subject {s} is not an object"
            ));
        };
        for (p, objects) in preds {
            let J::Array(objects) = objects else {
                return bad(format!(
                    "RDF/JSON: the value of predicate {p} is not an array"
                ));
            };
            for o in objects {
                let get = |k: &str| o.get(k).and_then(J::as_str).map(str::to_string);
                let value = get("value")
                    .ok_or_else(|| DecodeError("RDF/JSON: an object without value".into()))?;
                let obj = match get("type").as_deref() {
                    Some("uri") => WireTerm::Iri(value),
                    Some("bnode") => {
                        WireTerm::BNode(value.strip_prefix("_:").unwrap_or(&value).to_string())
                    }
                    Some("literal") => WireTerm::Literal {
                        lex: value,
                        lang: get("lang"),
                        dir: get("direction"),
                        datatype: get("datatype"),
                        dt_prefix: None,
                    },
                    t => return bad(format!("RDF/JSON: unknown object type {t:?}")),
                };
                let subject = match s.strip_prefix("_:") {
                    Some(l) => WireTerm::BNode(l.to_string()),
                    None => WireTerm::Iri(s.clone()),
                };
                sink(terms.quad(subject, WireTerm::Iri(p.clone()), obj, None)?)?;
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- writers ------

/// Writes Thrift compact-protocol values.
struct TOut<W> {
    w: W,
    /// the last field id of each open struct
    last: Vec<i16>,
}

impl<W: Write> TOut<W> {
    fn new(w: W) -> Self {
        TOut { w, last: vec![0] }
    }

    fn varint(&mut self, mut v: u64) -> io::Result<()> {
        let mut buf = [0u8; 10];
        let mut i = 0;
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                buf[i] = b;
                i += 1;
                break;
            }
            buf[i] = b | 0x80;
            i += 1;
        }
        self.w.write_all(&buf[..i])
    }

    fn zigzag(&mut self, v: i64) -> io::Result<()> {
        self.varint(((v << 1) ^ (v >> 63)) as u64)
    }

    fn field(&mut self, id: i16, ty: u8) -> io::Result<()> {
        let last = self.last.last_mut().expect("an open struct");
        let delta = id - *last;
        *last = id;
        if (1..=15).contains(&delta) {
            self.w.write_all(&[((delta as u8) << 4) | ty])
        } else {
            self.w.write_all(&[ty])?;
            self.zigzag(i64::from(id))
        }
    }

    fn begin(&mut self) {
        self.last.push(0);
    }

    fn end(&mut self) -> io::Result<()> {
        self.last.pop();
        self.w.write_all(&[0])
    }

    fn string_field(&mut self, id: i16, s: &str) -> io::Result<()> {
        self.field(id, ttype::BINARY)?;
        self.varint(s.len() as u64)?;
        self.w.write_all(s.as_bytes())
    }

    /// A struct field holding one string (`RDF_IRI`, `RDF_BNode`, `RDF_VAR`).
    fn wrapped_string(&mut self, id: i16, s: &str) -> io::Result<()> {
        self.field(id, ttype::STRUCT)?;
        self.begin();
        self.string_field(1, s)?;
        self.end()
    }

    /// An `RDF_Term` (a struct whose one field is the term's kind).
    fn term(&mut self, t: &Term) -> io::Result<()> {
        self.begin();
        match t {
            Term::NamedNode(n) => self.wrapped_string(1, n.as_str())?,
            Term::BlankNode(b) => self.wrapped_string(2, b.as_str())?,
            Term::Literal(l) => {
                self.field(3, ttype::STRUCT)?;
                self.begin();
                self.string_field(1, l.value())?;
                if let Some(lang) = l.language() {
                    self.string_field(2, lang)?;
                    if let Some(d) = l.direction() {
                        self.string_field(5, direction(d))?;
                    }
                } else if l.datatype() != xsd::STRING {
                    self.string_field(3, l.datatype().as_str())?;
                }
                self.end()?;
            }
            Term::Triple(t) => {
                self.field(9, ttype::STRUCT)?;
                self.triple_body(t)?;
            }
        }
        self.end()
    }

    fn undef(&mut self) -> io::Result<()> {
        self.begin();
        self.field(7, ttype::STRUCT)?;
        self.begin();
        self.end()?;
        self.end()
    }

    /// The fields of an `RDF_Triple` (S, P, O) as a struct.
    fn triple_body(&mut self, t: &Triple) -> io::Result<()> {
        self.begin();
        self.field(1, ttype::STRUCT)?;
        self.term(&t.subject.clone().into())?;
        self.field(2, ttype::STRUCT)?;
        self.term(&t.predicate.clone().into())?;
        self.field(3, ttype::STRUCT)?;
        self.term(&t.object)?;
        self.end()
    }
}

fn direction(d: BaseDirection) -> &'static str {
    match d {
        BaseDirection::Ltr => "ltr",
        BaseDirection::Rtl => "rtl",
    }
}

/// Writes protobuf messages into a buffer.
#[derive(Default)]
struct PbOut {
    b: Vec<u8>,
}

impl PbOut {
    fn varint(&mut self, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.b.push(b);
                return;
            }
            self.b.push(b | 0x80);
        }
    }

    fn bytes(&mut self, field: u64, x: &[u8]) {
        self.varint((field << 3) | 2);
        self.varint(x.len() as u64);
        self.b.extend_from_slice(x);
    }

    fn message(&mut self, field: u64, f: impl FnOnce(&mut PbOut)) {
        let mut inner = PbOut::default();
        f(&mut inner);
        self.bytes(field, &inner.b);
    }

    fn term(&mut self, field: u64, t: &Term) {
        self.message(field, |m| match t {
            Term::NamedNode(n) => m.message(1, |i| i.bytes(1, n.as_str().as_bytes())),
            Term::BlankNode(b) => m.message(2, |i| i.bytes(1, b.as_str().as_bytes())),
            Term::Literal(l) => m.message(3, |i| {
                i.bytes(1, l.value().as_bytes());
                if let Some(lang) = l.language() {
                    match l.direction() {
                        Some(d) => i.bytes(5, format!("{lang}--{}", direction(d)).as_bytes()),
                        None => i.bytes(2, lang.as_bytes()),
                    }
                } else if l.datatype() != xsd::STRING {
                    i.bytes(3, l.datatype().as_str().as_bytes());
                } else {
                    // `simple = true`
                    i.varint(9 << 3);
                    i.varint(1);
                }
            }),
            Term::Triple(t) => m.message(6, |i| i.triple(t)),
        });
    }

    fn triple(&mut self, t: &Triple) {
        self.term(1, &t.subject.clone().into());
        self.term(2, &t.predicate.clone().into());
        self.term(3, &t.object);
    }
}

/// Writes triples or quads in one of this module's syntaxes.
pub struct RdfWriter<W: Write> {
    fmt: JenaFormat,
    w: W,
    /// RDF/JSON: subject → predicate → objects, written at the end
    json: BTreeMap<String, BTreeMap<String, Vec<serde_json::Value>>>,
    trix: crate::trix::TrixSerializer,
}

impl<W: Write> RdfWriter<W> {
    pub fn new(fmt: JenaFormat, w: W) -> Self {
        RdfWriter {
            fmt,
            w,
            json: BTreeMap::new(),
            trix: crate::trix::TrixSerializer::new(),
        }
    }

    pub fn triple(&mut self, t: &Triple) -> io::Result<()> {
        match self.fmt {
            JenaFormat::Thrift => {
                let mut o = TOut::new(&mut self.w);
                o.field(2, ttype::STRUCT)?;
                o.triple_body(t)?;
                o.end()
            }
            JenaFormat::Protobuf => {
                let mut row = PbOut::default();
                row.message(2, |m| m.triple(t));
                self.delimited(&row.b)
            }
            JenaFormat::RdfJson => {
                let s = match &t.subject {
                    NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
                    NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
                };
                let o = rdf_json_object(&t.object)?;
                self.json
                    .entry(s)
                    .or_default()
                    .entry(t.predicate.as_str().to_string())
                    .or_default()
                    .push(o);
                Ok(())
            }
            JenaFormat::TriX => self.trix.triple(&mut self.w, t.as_ref()),
        }
    }

    /// A quad. RDF/JSON holds one graph: the graph name is dropped.
    pub fn quad(&mut self, q: &Quad) -> io::Result<()> {
        let t = Triple {
            subject: q.subject.clone(),
            predicate: q.predicate.clone(),
            object: q.object.clone(),
        };
        let g: Option<Term> = match &q.graph_name {
            GraphName::DefaultGraph => None,
            GraphName::NamedNode(n) => Some(n.clone().into()),
            GraphName::BlankNode(b) => Some(b.clone().into()),
        };
        match self.fmt {
            JenaFormat::RdfJson => self.triple(&t),
            JenaFormat::TriX => self.trix.quad(&mut self.w, q.as_ref()),
            JenaFormat::Thrift => {
                let mut o = TOut::new(&mut self.w);
                o.field(3, ttype::STRUCT)?;
                o.begin();
                o.field(1, ttype::STRUCT)?;
                o.term(&t.subject.into())?;
                o.field(2, ttype::STRUCT)?;
                o.term(&t.predicate.into())?;
                o.field(3, ttype::STRUCT)?;
                o.term(&t.object)?;
                if let Some(g) = &g {
                    o.field(4, ttype::STRUCT)?;
                    o.term(g)?;
                }
                o.end()?;
                o.end()
            }
            JenaFormat::Protobuf => {
                let mut row = PbOut::default();
                row.message(3, |m| {
                    m.triple(&t);
                    if let Some(g) = &g {
                        m.term(4, g);
                    }
                });
                self.delimited(&row.b)
            }
        }
    }

    fn delimited(&mut self, msg: &[u8]) -> io::Result<()> {
        let mut len = PbOut::default();
        len.varint(msg.len() as u64);
        self.w.write_all(&len.b)?;
        self.w.write_all(msg)
    }

    pub fn finish(mut self) -> io::Result<W> {
        if self.fmt == JenaFormat::RdfJson {
            let doc: serde_json::Map<String, serde_json::Value> = std::mem::take(&mut self.json)
                .into_iter()
                .map(|(s, preds)| {
                    let preds: serde_json::Map<String, serde_json::Value> = preds
                        .into_iter()
                        .map(|(p, os)| (p, serde_json::Value::Array(os)))
                        .collect();
                    (s, serde_json::Value::Object(preds))
                })
                .collect();
            serde_json::to_writer(&mut self.w, &doc).map_err(io::Error::other)?;
        }
        if self.fmt == JenaFormat::TriX {
            self.trix.finish(&mut self.w)?;
        }
        self.w.flush()?;
        Ok(self.w)
    }
}

fn rdf_json_object(t: &Term) -> io::Result<serde_json::Value> {
    use serde_json::json;
    Ok(match t {
        Term::NamedNode(n) => json!({ "type": "uri", "value": n.as_str() }),
        Term::BlankNode(b) => json!({ "type": "bnode", "value": format!("_:{}", b.as_str()) }),
        Term::Literal(l) => {
            let mut o = serde_json::Map::new();
            o.insert("type".into(), "literal".into());
            o.insert("value".into(), l.value().into());
            if let Some(lang) = l.language() {
                o.insert("lang".into(), lang.into());
                if let Some(d) = l.direction() {
                    o.insert("direction".into(), direction(d).into());
                }
            } else {
                o.insert("datatype".into(), l.datatype().as_str().into());
            }
            serde_json::Value::Object(o)
        }
        Term::Triple(_) => {
            return Err(io::Error::other("RDF/JSON cannot hold a triple term"));
        }
    })
}

/// SPARQL results in Jena's Thrift encoding (`application/sparql-results+thrift`): an
/// `RDF_VarTuple` of the variables, then an `RDF_DataTuple` per row.
pub struct ThriftResults<W: Write> {
    out: TOut<W>,
}

pub const RESULTS_THRIFT: &str = "application/sparql-results+thrift";

impl<W: Write> ThriftResults<W> {
    pub fn new(w: W, vars: &[String]) -> io::Result<Self> {
        let mut out = TOut::new(w);
        out.field(1, ttype::LIST)?;
        list_header(&mut out, vars.len(), ttype::STRUCT)?;
        for v in vars {
            out.begin();
            out.string_field(1, v)?;
            out.end()?;
        }
        out.end()?;
        Ok(ThriftResults { out })
    }

    pub fn row(&mut self, row: &[Option<Term>]) -> io::Result<()> {
        self.out.begin();
        self.out.field(1, ttype::LIST)?;
        list_header(&mut self.out, row.len(), ttype::STRUCT)?;
        for t in row {
            match t {
                Some(t) => self.out.term(t)?,
                None => self.out.undef()?,
            }
        }
        self.out.end()
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.out.w.flush()?;
        Ok(self.out.w)
    }
}

fn list_header<W: Write>(out: &mut TOut<W>, n: usize, ty: u8) -> io::Result<()> {
    if n < 15 {
        out.w.write_all(&[((n as u8) << 4) | ty])
    } else {
        out.w.write_all(&[0xf0 | ty])?;
        out.varint(n as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Quad> {
        let ex = |s: &str| NamedNode::new(format!("http://example.org/{s}")).unwrap();
        let b = BlankNode::new("b0").unwrap();
        let t = Triple {
            subject: ex("s").into(),
            predicate: ex("p"),
            object: Literal::new_simple_literal("x").into(),
        };
        vec![
            Quad::new(
                ex("s"),
                ex("p"),
                Literal::new_simple_literal("plain"),
                GraphName::DefaultGraph,
            ),
            Quad::new(
                ex("s"),
                ex("p"),
                Literal::new_typed_literal("42", xsd::INTEGER),
                GraphName::DefaultGraph,
            ),
            Quad::new(
                ex("s"),
                ex("p"),
                Literal::new_language_tagged_literal("hola", "es").unwrap(),
                ex("g"),
            ),
            Quad::new(
                b.clone(),
                ex("p"),
                Literal::new_directional_language_tagged_literal("x", "ar", BaseDirection::Rtl)
                    .unwrap(),
                ex("g"),
            ),
            Quad::new(
                b,
                ex("q"),
                Term::Triple(Box::new(t)),
                GraphName::DefaultGraph,
            ),
            Quad::new(ex("s"), ex("p"), ex("o"), BlankNode::new("g1").unwrap()),
        ]
    }

    fn roundtrip(fmt: JenaFormat) {
        let mut w = RdfWriter::new(fmt, Vec::new());
        for q in sample() {
            w.quad(&q).unwrap();
        }
        let bytes = w.finish().unwrap();
        let mut out = Vec::new();
        let n = transcode(fmt, &bytes[..], true, &mut out).unwrap();
        assert_eq!(n, 6);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("\"42\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
            "{text}"
        );
        assert!(
            text.contains("\"hola\"@es <http://example.org/g>"),
            "{text}"
        );
        assert!(text.contains("\"x\"@ar--rtl"), "{text}");
        assert!(
            text.contains("<<( <http://example.org/s> <http://example.org/p> \"x\" )>>"),
            "{text}"
        );
        // the blank node subject of two quads stays one node
        let bnodes: std::collections::HashSet<&str> = text
            .lines()
            .filter(|l| l.starts_with("_:"))
            .map(|l| l.split(' ').next().unwrap())
            .collect();
        assert_eq!(bnodes.len(), 1, "{text}");
    }

    #[test]
    fn thrift_roundtrip() {
        roundtrip(JenaFormat::Thrift);
    }

    #[test]
    fn protobuf_roundtrip() {
        roundtrip(JenaFormat::Protobuf);
    }

    #[test]
    fn rdf_json_roundtrip() {
        let mut w = RdfWriter::new(JenaFormat::RdfJson, Vec::new());
        for q in sample().into_iter().take(4) {
            w.quad(&q).unwrap();
        }
        let bytes = w.finish().unwrap();
        let mut out = Vec::new();
        assert_eq!(
            transcode(JenaFormat::RdfJson, &bytes[..], false, &mut out).unwrap(),
            4
        );
    }

    /// A Thrift stream as Jena writes it, with a prefix declaration, a prefix name,
    /// values and a quad (bytes from Jena 6's `RDFFormat.RDF_THRIFT_VALUES`).
    #[test]
    fn thrift_values_and_prefix_names() {
        let mut o = TOut::new(Vec::new());
        // prefixDecl ex: <http://example.org/>
        o.field(1, ttype::STRUCT).unwrap();
        o.begin();
        o.string_field(1, "ex").unwrap();
        o.string_field(2, "http://example.org/").unwrap();
        o.end().unwrap();
        o.end().unwrap();
        // triple ex:s ex:p 42 (valInteger), then ex:s ex:p 1.50 (valDecimal), 2.5 (valDouble)
        for value in 0..3 {
            o.last = vec![0];
            o.field(2, ttype::STRUCT).unwrap();
            o.begin();
            for (fid, local) in [(1, "s"), (2, "p")] {
                o.field(fid, ttype::STRUCT).unwrap();
                o.begin();
                o.field(4, ttype::STRUCT).unwrap();
                o.begin();
                o.string_field(1, "ex").unwrap();
                o.string_field(2, local).unwrap();
                o.end().unwrap();
                o.end().unwrap();
            }
            o.field(3, ttype::STRUCT).unwrap();
            o.begin();
            match value {
                0 => {
                    o.field(10, ttype::I64).unwrap();
                    o.zigzag(42).unwrap();
                }
                1 => {
                    o.field(12, ttype::STRUCT).unwrap();
                    o.begin();
                    o.field(1, ttype::I64).unwrap();
                    o.zigzag(150).unwrap();
                    o.field(2, ttype::I32).unwrap();
                    o.zigzag(2).unwrap();
                    o.end().unwrap();
                }
                _ => {
                    o.field(11, ttype::DOUBLE).unwrap();
                    o.w.write_all(&2.5f64.to_le_bytes()).unwrap();
                }
            }
            o.end().unwrap();
            o.end().unwrap();
            o.end().unwrap();
        }
        let mut out = Vec::new();
        transcode(JenaFormat::Thrift, &o.w[..], false, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("<http://example.org/s> <http://example.org/p> \"42\"^^<http://www.w3.org/2001/XMLSchema#integer>"), "{text}");
        assert!(
            text.contains("\"1.50\"^^<http://www.w3.org/2001/XMLSchema#decimal>"),
            "{text}"
        );
        assert!(
            text.contains("\"2.5E0\"^^<http://www.w3.org/2001/XMLSchema#double>"),
            "{text}"
        );
    }

    #[test]
    fn named_graph_quads_are_refused_for_a_graph() {
        let mut w = RdfWriter::new(JenaFormat::Thrift, Vec::new());
        for q in sample() {
            w.quad(&q).unwrap();
        }
        let bytes = w.finish().unwrap();
        let e = transcode(JenaFormat::Thrift, &bytes[..], false, Vec::new()).unwrap_err();
        assert!(e.0.contains("graph"), "{e}");
    }

    #[test]
    fn truncated_bodies_are_errors() {
        let mut w = RdfWriter::new(JenaFormat::Thrift, Vec::new());
        w.quad(&sample()[0]).unwrap();
        let bytes = w.finish().unwrap();
        assert!(
            transcode(
                JenaFormat::Thrift,
                &bytes[..bytes.len() - 3],
                true,
                Vec::new()
            )
            .is_err()
        );
        let mut w = RdfWriter::new(JenaFormat::Protobuf, Vec::new());
        w.quad(&sample()[0]).unwrap();
        let bytes = w.finish().unwrap();
        assert!(
            transcode(
                JenaFormat::Protobuf,
                &bytes[..bytes.len() - 3],
                true,
                Vec::new()
            )
            .is_err()
        );
    }

    #[test]
    fn decimals() {
        assert_eq!(decimal_lexical(150, 2), "1.50");
        assert_eq!(decimal_lexical(-5, 3), "-0.005");
        assert_eq!(decimal_lexical(42, 0), "42");
        assert_eq!(decimal_lexical(42, -2), "4200");
        assert_eq!(double_lexical(3.5), "3.5E0");
        assert_eq!(double_lexical(100.0), "1.0E2");
        assert_eq!(double_lexical(-0.001), "-1.0E-3");
    }
}
