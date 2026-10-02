//! RDF Patch output: the change format of Apache Jena (`jena-rdfpatch`) and RDF Delta.
//!
//! A patch is a sequence of rows. Sparkles writes header rows that identify the change,
//! a transaction, and one row per quad deleted (`D`) or added (`A`):
//!
//! ```text
//! H id <urn:uuid:3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa#commit:42> .
//! H prev <urn:uuid:3f1c9a2e-7b4d-4c1e-9a55-0c2b8e61d7aa#commit:41> .
//! TX .
//! D <urn:a> <urn:p> "1"^^<http://www.w3.org/2001/XMLSchema#integer> .
//! A <urn:b> <urn:p> "two"@en <urn:g1> .
//! TC .
//! ```
//!
//! `id` names the state the patch leads to and `prev` the state it applies to, as
//! commit IRIs ([`commit_iri`]). A quad in the default graph has no fourth term.
//!
//! The text form (`application/rdf-patch`) writes terms in N-Triples syntax, except
//! blank nodes, which are written `<_:label>`: Jena's reader keeps that label, while it
//! drops the first character of a `_:label`. Triple terms are written `<<( s p o )>>`.
//!
//! The binary form (`application/rdf-patch+thrift`) is a sequence of `RDF_Patch_Row`
//! structs of Jena's RDF Thrift schema (`BinaryRDF.thrift`) in the Thrift compact
//! protocol, one per row, with no framing. A literal of type `xsd:string` or with a
//! language tag carries no datatype, as Jena writes it. A base direction goes in the
//! literal's `baseDirection` field, which Jena's patch reader ignores.

use crate::store::DiffOp;
use oxrdf::{GraphName, NamedOrBlankNode, Quad, Term};
use std::io::{self, Write};

/// The media type of the text form.
pub const MEDIA_TYPE: &str = "application/rdf-patch";
/// The media type of the binary (RDF Thrift) form.
pub const MEDIA_TYPE_BINARY: &str = "application/rdf-patch+thrift";

/// The IRI that names commit `seq` of dataset `dataset_id` in patch headers:
/// `urn:uuid:<dataset id>#commit:<seq>`.
pub fn commit_iri(dataset_id: uuid::Uuid, seq: u64) -> String {
    format!("urn:uuid:{dataset_id}#commit:{seq}")
}

/// Writes RDF Patch rows, as text or as binary.
pub struct PatchWriter<W: Write> {
    w: W,
    binary: bool,
    buf: Vec<u8>,
}

impl<W: Write> PatchWriter<W> {
    pub fn new(w: W, binary: bool) -> PatchWriter<W> {
        PatchWriter {
            w,
            binary,
            buf: Vec::with_capacity(256),
        }
    }

    /// A header row whose value is an IRI (`H name <iri> .`).
    pub fn header(&mut self, name: &str, iri: &str) -> io::Result<()> {
        if self.binary {
            let mut t = Thrift::new(&mut self.buf);
            t.field(1, STRUCT); // RDF_Patch_Row.header
            t.field(1, BINARY); // Patch_Header.name
            t.string(name);
            t.field(2, STRUCT); // Patch_Header.value
            t.iri(iri);
            t.stop(); // Patch_Header
            t.stop(); // RDF_Patch_Row
        } else {
            writeln!(self.buf, "H {name} <{iri}> .")?;
        }
        self.flush_row()
    }

    /// The start of a transaction (`TX .`).
    pub fn begin(&mut self) -> io::Result<()> {
        self.txn(0, "TX")
    }

    /// The commit of a transaction (`TC .`).
    pub fn commit(&mut self) -> io::Result<()> {
        self.txn(1, "TC")
    }

    fn txn(&mut self, code: i32, text: &str) -> io::Result<()> {
        if self.binary {
            let mut t = Thrift::new(&mut self.buf);
            t.field(6, I32); // RDF_Patch_Row.txn (PatchTxn)
            t.i32(code);
            t.stop();
        } else {
            self.buf.extend_from_slice(text.as_bytes());
            self.buf.extend_from_slice(b" .\n");
        }
        self.flush_row()
    }

    /// A quad deleted or added (`D …` or `A …`).
    pub fn change(&mut self, op: DiffOp, q: &Quad) -> io::Result<()> {
        if self.binary {
            let mut t = Thrift::new(&mut self.buf);
            // RDF_Patch_Row.dataAdd (2) or .dataDel (3): s, p, o and an optional g
            t.field(if op == DiffOp::Add { 2 } else { 3 }, STRUCT);
            t.field(1, STRUCT);
            t.subject(&q.subject);
            t.field(2, STRUCT);
            t.iri(q.predicate.as_str());
            t.field(3, STRUCT);
            t.term(&q.object);
            match &q.graph_name {
                GraphName::DefaultGraph => {}
                GraphName::NamedNode(n) => {
                    t.field(4, STRUCT);
                    t.iri(n.as_str());
                }
                GraphName::BlankNode(b) => {
                    t.field(4, STRUCT);
                    t.bnode(b.as_str());
                }
            }
            t.stop();
            t.stop();
        } else {
            self.buf
                .extend_from_slice(if op == DiffOp::Add { b"A " } else { b"D " });
            text_subject(&mut self.buf, &q.subject);
            self.buf.push(b' ');
            write!(self.buf, "{}", q.predicate)?;
            self.buf.push(b' ');
            text_term(&mut self.buf, &q.object);
            match &q.graph_name {
                GraphName::DefaultGraph => {}
                GraphName::NamedNode(n) => write!(self.buf, " {n}")?,
                GraphName::BlankNode(b) => write!(self.buf, " <_:{}>", b.as_str())?,
            }
            self.buf.extend_from_slice(b" .\n");
        }
        self.flush_row()
    }

    fn flush_row(&mut self) -> io::Result<()> {
        let r = self.w.write_all(&self.buf);
        self.buf.clear();
        r
    }

    pub fn into_inner(self) -> W {
        self.w
    }
}

/// Write a whole patch: the headers, then a transaction of the changes.
pub fn write_patch<'a, W: Write>(
    w: &mut PatchWriter<W>,
    id: &str,
    prev: Option<&str>,
    changes: impl IntoIterator<Item = (DiffOp, &'a Quad)>,
) -> io::Result<()> {
    w.header("id", id)?;
    if let Some(p) = prev {
        w.header("prev", p)?;
    }
    w.begin()?;
    for (op, q) in changes {
        w.change(op, q)?;
    }
    w.commit()
}

fn text_subject(buf: &mut Vec<u8>, s: &NamedOrBlankNode) {
    match s {
        NamedOrBlankNode::NamedNode(n) => {
            let _ = write!(buf, "{n}");
        }
        NamedOrBlankNode::BlankNode(b) => {
            let _ = write!(buf, "<_:{}>", b.as_str());
        }
    }
}

fn text_term(buf: &mut Vec<u8>, t: &Term) {
    match t {
        Term::NamedNode(n) => {
            let _ = write!(buf, "{n}");
        }
        Term::BlankNode(b) => {
            let _ = write!(buf, "<_:{}>", b.as_str());
        }
        Term::Literal(l) => {
            let _ = write!(buf, "{l}");
        }
        Term::Triple(tr) => {
            buf.extend_from_slice(b"<<( ");
            text_subject(buf, &tr.subject);
            let _ = write!(buf, " {} ", tr.predicate);
            text_term(buf, &tr.object);
            buf.extend_from_slice(b" )>>");
        }
    }
}

// Thrift compact protocol type codes
const BINARY: u8 = 8;
const I32: u8 = 5;
const STRUCT: u8 = 12;

/// A minimal writer of the Thrift compact protocol: structs, strings and i32 values.
struct Thrift<'b> {
    out: &'b mut Vec<u8>,
    /// the last field id written in each open struct
    last: Vec<i16>,
}

impl<'b> Thrift<'b> {
    fn new(out: &'b mut Vec<u8>) -> Thrift<'b> {
        Thrift { out, last: vec![0] }
    }

    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.out.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.out.push(v as u8);
    }

    /// A field header; a struct field opens a nested struct.
    fn field(&mut self, id: i16, ty: u8) {
        let last = self.last.last_mut().expect("an open struct");
        let delta = id - *last;
        *last = id;
        if (1..=15).contains(&delta) {
            self.out.push(((delta as u8) << 4) | ty);
        } else {
            self.out.push(ty);
            let z = ((id << 1) ^ (id >> 15)) as u16;
            self.varint(z as u64);
        }
        if ty == STRUCT {
            self.last.push(0);
        }
    }

    /// The end of the innermost open struct.
    fn stop(&mut self) {
        self.out.push(0);
        self.last.pop();
    }

    fn string(&mut self, s: &str) {
        self.varint(s.len() as u64);
        self.out.extend_from_slice(s.as_bytes());
    }

    fn i32(&mut self, v: i32) {
        let z = ((v << 1) ^ (v >> 31)) as u32;
        self.varint(z as u64);
    }

    /// The fields of an `RDF_Term` (the struct is open) holding an IRI, then its end.
    fn iri(&mut self, iri: &str) {
        self.field(1, STRUCT); // RDF_Term.iri
        self.field(1, BINARY); // RDF_IRI.iri
        self.string(iri);
        self.stop();
        self.stop();
    }

    fn bnode(&mut self, label: &str) {
        self.field(2, STRUCT); // RDF_Term.bnode
        self.field(1, BINARY); // RDF_BNode.label
        self.string(label);
        self.stop();
        self.stop();
    }

    fn subject(&mut self, s: &NamedOrBlankNode) {
        match s {
            NamedOrBlankNode::NamedNode(n) => self.iri(n.as_str()),
            NamedOrBlankNode::BlankNode(b) => self.bnode(b.as_str()),
        }
    }

    fn term(&mut self, t: &Term) {
        match t {
            Term::NamedNode(n) => self.iri(n.as_str()),
            Term::BlankNode(b) => self.bnode(b.as_str()),
            Term::Literal(l) => {
                self.field(3, STRUCT); // RDF_Term.literal
                self.field(1, BINARY); // RDF_Literal.lex
                self.string(l.value());
                if let Some(lang) = l.language() {
                    self.field(2, BINARY); // langtag
                    self.string(lang);
                } else if l.datatype() != oxrdf::vocab::xsd::STRING {
                    self.field(3, BINARY); // datatype
                    self.string(l.datatype().as_str());
                }
                if let Some(d) = l.direction() {
                    self.field(5, BINARY); // baseDirection
                    self.string(&d.to_string());
                }
                self.stop();
                self.stop();
            }
            Term::Triple(tr) => {
                self.field(9, STRUCT); // RDF_Term.tripleTerm
                self.field(1, STRUCT);
                self.subject(&tr.subject);
                self.field(2, STRUCT);
                self.iri(tr.predicate.as_str());
                self.field(3, STRUCT);
                self.term(&tr.object);
                self.stop();
                self.stop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{BlankNode, Literal, NamedNode};

    fn n(s: &str) -> NamedNode {
        NamedNode::new_unchecked(s)
    }

    #[test]
    fn text_rows() {
        let mut w = PatchWriter::new(Vec::new(), false);
        let q1 = Quad::new(
            BlankNode::new_unchecked("b1"),
            n("urn:p"),
            Literal::new_language_tagged_literal_unchecked("a\"b\nc", "en"),
            GraphName::DefaultGraph,
        );
        let q2 = Quad::new(
            n("urn:s"),
            n("urn:p"),
            Literal::new_typed_literal("1", oxrdf::vocab::xsd::INTEGER),
            n("urn:g"),
        );
        write_patch(
            &mut w,
            "urn:uuid:x#commit:2",
            Some("urn:uuid:x#commit:1"),
            [(DiffOp::Remove, &q1), (DiffOp::Add, &q2)],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(w.into_inner()).unwrap(),
            "H id <urn:uuid:x#commit:2> .\nH prev <urn:uuid:x#commit:1> .\nTX .\n\
             D <_:b1> <urn:p> \"a\\\"b\\nc\"@en .\n\
             A <urn:s> <urn:p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> <urn:g> .\nTC .\n"
        );
    }

    #[test]
    fn compact_protocol_rows() {
        let mut w = PatchWriter::new(Vec::new(), true);
        w.begin().unwrap();
        w.commit().unwrap();
        // field 6 (delta 6, type i32) = zigzag(0), stop; then zigzag(1) = 2
        assert_eq!(w.into_inner(), [0x65, 0x00, 0x00, 0x65, 0x02, 0x00]);
        let mut w = PatchWriter::new(Vec::new(), true);
        w.header("id", "u:1").unwrap();
        assert_eq!(
            w.into_inner(),
            [
                0x1C, // row.header (struct)
                0x18, 2, b'i', b'd', // name
                0x1C, // value (RDF_Term, field 2: delta 1)
                0x1C, // .iri (RDF_IRI)
                0x18, 3, b'u', b':', b'1', // .iri
                0, 0, 0, 0,
            ]
        );
        // a long field-id jump uses the long form: type byte, zigzag varint id
        let mut out = Vec::new();
        let mut t = Thrift::new(&mut out);
        t.field(20, I32);
        assert_eq!(out, [5, 40]);
    }
}
