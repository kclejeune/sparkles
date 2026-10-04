//! The batch encoding of P04 §2.4: RDF terms in both directions, as byte batches.
//!
//! A string is a LEB128 length followed by UTF-8 bytes, and integers are little-endian.
//! A term starts with a tag byte:
//!
//! | tag | term | payload |
//! |---|---|---|
//! | 0 | none (a wildcard in a pattern) | |
//! | 1 | IRI | string |
//! | 2 | blank node | label |
//! | 3 | `xsd:string` literal | lexical form |
//! | 4 | language-tagged string | lexical form, language tag |
//! | 5 | directional language-tagged string | lexical form, language tag, direction (0 `ltr`, 1 `rtl`) |
//! | 6 | literal of a common datatype | lexical form, index into [`DATATYPES`] |
//! | 7 | literal of another datatype | lexical form, datatype IRI |
//! | 8 | triple term | subject, predicate, object |
//! | 9 | the default graph | |
//! | 10 | the union graph (`urn:x-arq:UnionGraph`) | |
//! | 11 | a repeat of an earlier term of the batch | its position, LEB128 |
//!
//! Positions for tag 11 count the terms of a batch at its top level whose tag is 1 to 8,
//! from 0; terms inside a triple term and tags 0, 9, 10 and 11 take no position.
//!
//! Results go the other way as row batches ([`RowWriter`]): a header, the terms that
//! appear for the first time, then the rows as 32-bit cells that index the result's term
//! table (0 is unbound).

use oxrdf::{BaseDirection, BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use rustc_hash::FxHashMap;
use std::hash::Hash;
use std::sync::Arc;

/// The version of the encoding, which the Kotlin side checks when it loads the library.
pub const ENCODING_VERSION: u32 = 1;

pub const NONE: u8 = 0;
pub const IRI: u8 = 1;
pub const BNODE: u8 = 2;
pub const STRING: u8 = 3;
pub const LANG: u8 = 4;
pub const DIR_LANG: u8 = 5;
pub const COMMON: u8 = 6;
pub const TYPED: u8 = 7;
pub const TRIPLE: u8 = 8;
pub const DEFAULT_GRAPH: u8 = 9;
pub const UNION_GRAPH: u8 = 10;
pub const REPEAT: u8 = 11;

/// Operations of a write batch.
pub const OP_ADD: u8 = 1;
pub const OP_DELETE: u8 = 2;

/// Row batches start a new term table (both sides forget the terms sent before).
pub const FLAG_RESTART: u8 = 1;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const UNION_GRAPH_IRI: &str = "urn:x-arq:UnionGraph";
pub const DEFAULT_GRAPH_IRI: &str = "urn:x-arq:DefaultGraph";

/// The datatypes of tag 6, by index. The Kotlin side has the same table.
pub const DATATYPES: [&str; 24] = [
    "http://www.w3.org/2001/XMLSchema#integer",
    "http://www.w3.org/2001/XMLSchema#decimal",
    "http://www.w3.org/2001/XMLSchema#double",
    "http://www.w3.org/2001/XMLSchema#float",
    "http://www.w3.org/2001/XMLSchema#boolean",
    "http://www.w3.org/2001/XMLSchema#dateTime",
    "http://www.w3.org/2001/XMLSchema#date",
    "http://www.w3.org/2001/XMLSchema#time",
    "http://www.w3.org/2001/XMLSchema#duration",
    "http://www.w3.org/2001/XMLSchema#dayTimeDuration",
    "http://www.w3.org/2001/XMLSchema#yearMonthDuration",
    "http://www.w3.org/2001/XMLSchema#long",
    "http://www.w3.org/2001/XMLSchema#int",
    "http://www.w3.org/2001/XMLSchema#short",
    "http://www.w3.org/2001/XMLSchema#byte",
    "http://www.w3.org/2001/XMLSchema#nonNegativeInteger",
    "http://www.w3.org/2001/XMLSchema#positiveInteger",
    "http://www.w3.org/2001/XMLSchema#nonPositiveInteger",
    "http://www.w3.org/2001/XMLSchema#negativeInteger",
    "http://www.w3.org/2001/XMLSchema#unsignedLong",
    "http://www.w3.org/2001/XMLSchema#unsignedInt",
    "http://www.w3.org/2001/XMLSchema#gYear",
    "http://www.w3.org/2001/XMLSchema#anyURI",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#JSON",
];

fn datatype_index(iri: &str) -> Option<u8> {
    if !(iri.starts_with(XSD) || iri.starts_with(RDF)) {
        return None;
    }
    DATATYPES.iter().position(|d| *d == iri).map(|i| i as u8)
}

/// A decoding error: the batch does not follow the encoding.
#[derive(Debug)]
pub struct Malformed(pub String);

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed batch: {}", self.0)
    }
}

fn bad(m: impl Into<String>) -> Malformed {
    Malformed(m.into())
}

// ---------------------------------------------------------------------- writing ----

fn put_len(buf: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            buf.push(b);
            return;
        }
        buf.push(b | 0x80);
    }
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    put_len(buf, s.len() as u64);
    buf.extend_from_slice(s.as_bytes());
}

/// Write a term. `label` gives the label a blank node is sent with (the binding's label
/// table), given its stored label.
pub fn write_term(buf: &mut Vec<u8>, t: &Term, label: &dyn Fn(&str) -> Option<Arc<str>>) {
    match t {
        Term::NamedNode(n) => {
            buf.push(IRI);
            put_str(buf, n.as_str());
        }
        Term::BlankNode(b) => write_bnode(buf, b, label),
        Term::Literal(l) => write_literal(buf, l),
        Term::Triple(tr) => write_triple(buf, tr, label),
    }
}

fn write_bnode(buf: &mut Vec<u8>, b: &BlankNode, label: &dyn Fn(&str) -> Option<Arc<str>>) {
    buf.push(BNODE);
    match label(b.as_str()) {
        Some(l) => put_str(buf, &l),
        None => put_str(buf, b.as_str()),
    }
}

fn write_triple(buf: &mut Vec<u8>, tr: &Triple, label: &dyn Fn(&str) -> Option<Arc<str>>) {
    buf.push(TRIPLE);
    match &tr.subject {
        NamedOrBlankNode::NamedNode(n) => {
            buf.push(IRI);
            put_str(buf, n.as_str());
        }
        NamedOrBlankNode::BlankNode(b) => write_bnode(buf, b, label),
    }
    buf.push(IRI);
    put_str(buf, tr.predicate.as_str());
    write_term(buf, &tr.object, label);
}

fn write_literal(buf: &mut Vec<u8>, l: &Literal) {
    if let Some(lang) = l.language() {
        match l.direction() {
            Some(d) => {
                buf.push(DIR_LANG);
                put_str(buf, l.value());
                put_str(buf, lang);
                buf.push(match d {
                    BaseDirection::Ltr => 0,
                    BaseDirection::Rtl => 1,
                });
            }
            None => {
                buf.push(LANG);
                put_str(buf, l.value());
                put_str(buf, lang);
            }
        }
        return;
    }
    let dt = l.datatype().as_str();
    if dt == "http://www.w3.org/2001/XMLSchema#string" {
        buf.push(STRING);
        put_str(buf, l.value());
    } else if let Some(i) = datatype_index(dt) {
        buf.push(COMMON);
        put_str(buf, l.value());
        buf.push(i);
    } else {
        buf.push(TYPED);
        put_str(buf, l.value());
        put_str(buf, dt);
    }
}

/// A graph term of a row: the default graph has a tag of its own.
pub enum Cell<'a> {
    Term(&'a Term),
    DefaultGraph,
}

/// The term table of one result: each distinct term gets a dense index the first time a
/// batch sends it, keyed by `K` (the term's id, or the term itself for results that have
/// no ids). When it holds `limit` terms, the next batch restarts it.
pub struct TermTable<K> {
    index: FxHashMap<K, u32>,
    limit: usize,
}

impl<K: Hash + Eq> TermTable<K> {
    pub fn new(limit: usize) -> TermTable<K> {
        TermTable {
            index: FxHashMap::default(),
            limit: limit.max(1),
        }
    }
}

/// Builds one row batch:
///
/// ```text
/// u8   version
/// u8   flags          bit 0: the term table restarts at this batch
/// u32  new terms      then that many terms
/// u32  rows
/// u16  columns
/// u32  cells[rows × columns]   0 = unbound, n = term n − 1 of the table
/// ```
pub struct RowWriter<'t, K> {
    table: &'t mut TermTable<K>,
    flags: u8,
    terms: Vec<u8>,
    new_terms: u32,
    cells: Vec<u32>,
    columns: u16,
    rows: u32,
}

impl<'t, K: Hash + Eq> RowWriter<'t, K> {
    pub fn new(table: &'t mut TermTable<K>, columns: u16) -> RowWriter<'t, K> {
        let mut flags = 0;
        if table.index.len() >= table.limit {
            table.index.clear();
            flags |= FLAG_RESTART;
        }
        RowWriter {
            table,
            flags,
            terms: Vec::new(),
            new_terms: 0,
            cells: Vec::new(),
            columns,
            rows: 0,
        }
    }

    /// The cell of a term: its index in the table, plus one. `term` is called only for
    /// a key the table does not have yet, and `None` from it is an unbound cell.
    pub fn cell(
        &mut self,
        key: K,
        term: impl FnOnce() -> Option<Term>,
        label: &dyn Fn(&str) -> Option<Arc<str>>,
    ) -> u32 {
        if let Some(i) = self.table.index.get(&key) {
            return i + 1;
        }
        let Some(t) = term() else { return 0 };
        write_term(&mut self.terms, &t, label);
        self.add(key)
    }

    /// The cell of the default graph.
    pub fn default_graph(&mut self, key: K) -> u32 {
        if let Some(i) = self.table.index.get(&key) {
            return i + 1;
        }
        self.terms.push(DEFAULT_GRAPH);
        self.add(key)
    }

    fn add(&mut self, key: K) -> u32 {
        let i = self.table.index.len() as u32;
        self.table.index.insert(key, i);
        self.new_terms += 1;
        i + 1
    }

    /// Add a row of cells (as [`cell`](Self::cell) returns them).
    pub fn push_row(&mut self, cells: &[u32]) {
        debug_assert_eq!(cells.len(), self.columns as usize);
        self.cells.extend_from_slice(cells);
        self.rows += 1;
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }

    /// The bytes of terms and cells so far, to stop a batch at a size.
    pub fn bytes(&self) -> usize {
        self.terms.len() + self.cells.len() * 4
    }

    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 + self.terms.len() + self.cells.len() * 4);
        out.push(ENCODING_VERSION as u8);
        out.push(self.flags);
        out.extend_from_slice(&self.new_terms.to_le_bytes());
        out.extend_from_slice(&self.terms);
        out.extend_from_slice(&self.rows.to_le_bytes());
        out.extend_from_slice(&self.columns.to_le_bytes());
        for c in self.cells {
            out.extend_from_slice(&c.to_le_bytes());
        }
        out
    }
}

/// An empty batch of `columns` columns.
pub fn empty_batch(columns: u16) -> Vec<u8> {
    let mut out = vec![ENCODING_VERSION as u8, 0];
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&columns.to_le_bytes());
    out
}

// ---------------------------------------------------------------------- reading ----

/// One item of an incoming batch.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// a wildcard
    None,
    DefaultGraph,
    UnionGraph,
    Term(Term),
}

/// Reads the terms of an incoming batch. `resolve` maps a blank node label the caller
/// sent to the stored label it stands for, if any (the binding's label table).
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    seen: Vec<Term>,
    resolve: &'a dyn Fn(&str) -> Option<BlankNode>,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8], resolve: &'a dyn Fn(&str) -> Option<BlankNode>) -> Reader<'a> {
        Reader {
            buf,
            pos: 0,
            seen: Vec::new(),
            resolve,
        }
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn byte(&mut self) -> Result<u8, Malformed> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| bad("unexpected end"))?;
        self.pos += 1;
        Ok(b)
    }

    fn len(&mut self) -> Result<u64, Malformed> {
        let mut n: u64 = 0;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            if shift >= 64 {
                return Err(bad("length too long"));
            }
            n |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(n);
            }
            shift += 7;
        }
    }

    pub fn string(&mut self) -> Result<&'a str, Malformed> {
        let n = self.len()? as usize;
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.buf.len())
            .ok_or_else(|| bad("string past the end"))?;
        let s = std::str::from_utf8(&self.buf[self.pos..end]).map_err(|_| bad("not UTF-8"))?;
        self.pos = end;
        Ok(s)
    }

    /// The next item of the batch.
    pub fn item(&mut self) -> Result<Item, Malformed> {
        let tag = self.byte()?;
        Ok(match tag {
            NONE => Item::None,
            DEFAULT_GRAPH => Item::DefaultGraph,
            UNION_GRAPH => Item::UnionGraph,
            REPEAT => {
                let i = self.len()? as usize;
                Item::Term(
                    self.seen
                        .get(i)
                        .cloned()
                        .ok_or_else(|| bad(format!("repeat of term {i}, which is not there")))?,
                )
            }
            _ => {
                let t = self.term_of(tag)?;
                self.seen.push(t.clone());
                Item::Term(t)
            }
        })
    }

    fn term_of(&mut self, tag: u8) -> Result<Term, Malformed> {
        match tag {
            // Jena accepts any string as an IRI, relative ones included, and so does the
            // store; the parsers are where IRIs are checked
            IRI => Ok(NamedNode::new_unchecked(self.string()?).into()),
            BNODE => Ok(self.bnode()?.into()),
            STRING => Ok(Literal::new_simple_literal(self.string()?).into()),
            LANG => {
                let v = self.string()?;
                let l = self.string()?;
                Literal::new_language_tagged_literal(v, l)
                    .map(Term::Literal)
                    .map_err(|e| bad(format!("language tag {l:?}: {e}")))
            }
            DIR_LANG => {
                let v = self.string()?;
                let l = self.string()?;
                let d = match self.byte()? {
                    0 => BaseDirection::Ltr,
                    1 => BaseDirection::Rtl,
                    d => return Err(bad(format!("direction {d}"))),
                };
                Literal::new_directional_language_tagged_literal(v, l, d)
                    .map(Term::Literal)
                    .map_err(|e| bad(format!("language tag {l:?}: {e}")))
            }
            COMMON => {
                let v = self.string()?;
                let i = self.byte()? as usize;
                let dt = DATATYPES
                    .get(i)
                    .ok_or_else(|| bad(format!("datatype index {i}")))?;
                Ok(Literal::new_typed_literal(v, NamedNode::new_unchecked(*dt)).into())
            }
            TYPED => {
                let v = self.string()?;
                let dt = NamedNode::new_unchecked(self.string()?);
                Ok(Literal::new_typed_literal(v, dt).into())
            }
            TRIPLE => {
                let s = match self.nested()? {
                    Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
                    Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
                    _ => return Err(bad("a triple term's subject is an IRI or a blank node")),
                };
                let Term::NamedNode(p) = self.nested()? else {
                    return Err(bad("a triple term's predicate is an IRI"));
                };
                let o = self.nested()?;
                Ok(Triple::new(s, p, o).into())
            }
            t => Err(bad(format!("tag {t}"))),
        }
    }

    /// A term inside a triple term (no positions, no repeats).
    fn nested(&mut self) -> Result<Term, Malformed> {
        let tag = self.byte()?;
        self.term_of(tag)
    }

    fn bnode(&mut self) -> Result<BlankNode, Malformed> {
        let label = self.string()?;
        if let Some(b) = (self.resolve)(label) {
            return Ok(b);
        }
        BlankNode::new(label).map_err(|e| bad(format!("blank node label {label:?}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_kind() -> Vec<Term> {
        let mut terms: Vec<Term> = vec![
            NamedNode::new_unchecked("http://ex.org/a").into(),
            BlankNode::new_unchecked("x1").into(),
            Literal::new_simple_literal("plain é").into(),
            Literal::new_language_tagged_literal_unchecked("chat", "fr").into(),
            Literal::new_directional_language_tagged_literal_unchecked(
                "שלום",
                "he",
                BaseDirection::Rtl,
            )
            .into(),
            Literal::new_typed_literal("x", NamedNode::new_unchecked("http://ex.org/dt")).into(),
        ];
        for dt in DATATYPES {
            terms.push(Literal::new_typed_literal("1", NamedNode::new_unchecked(dt)).into());
        }
        terms.push(
            Triple::new(
                BlankNode::new_unchecked("t"),
                NamedNode::new_unchecked("http://ex.org/p"),
                Triple::new(
                    NamedNode::new_unchecked("http://ex.org/s"),
                    NamedNode::new_unchecked("http://ex.org/p"),
                    Literal::from(3),
                ),
            )
            .into(),
        );
        terms
    }

    #[test]
    fn terms_round_trip() {
        let terms = every_kind();
        let mut buf = Vec::new();
        for t in &terms {
            write_term(&mut buf, t, &|_| None);
        }
        // a repeat of the first and the third term, and the special items
        buf.push(REPEAT);
        put_len(&mut buf, 0);
        buf.push(REPEAT);
        put_len(&mut buf, 2);
        buf.extend([NONE, DEFAULT_GRAPH, UNION_GRAPH]);
        let none = |_: &str| None;
        let mut r = Reader::new(&buf, &none);
        for t in &terms {
            assert_eq!(r.item().unwrap(), Item::Term(t.clone()));
        }
        assert_eq!(r.item().unwrap(), Item::Term(terms[0].clone()));
        assert_eq!(r.item().unwrap(), Item::Term(terms[2].clone()));
        assert_eq!(r.item().unwrap(), Item::None);
        assert_eq!(r.item().unwrap(), Item::DefaultGraph);
        assert_eq!(r.item().unwrap(), Item::UnionGraph);
        assert!(r.at_end());
        assert!(r.item().is_err());
    }

    #[test]
    fn labels_are_mapped_both_ways() {
        let b: Term = BlankNode::new_unchecked("b1f").into();
        let mut buf = Vec::new();
        write_term(&mut buf, &b, &|l| {
            (l == "b1f").then(|| Arc::from("jena-label"))
        });
        let resolve = |l: &str| (l == "jena-label").then(|| BlankNode::new_unchecked("b1f"));
        assert_eq!(Reader::new(&buf, &resolve).item().unwrap(), Item::Term(b));
    }

    #[test]
    fn long_strings_and_bad_input() {
        let s = "x".repeat(70_000);
        let mut buf = Vec::new();
        write_term(&mut buf, &Literal::new_simple_literal(&s).into(), &|_| None);
        let none = |_: &str| None;
        assert_eq!(
            Reader::new(&buf, &none).item().unwrap(),
            Item::Term(Literal::new_simple_literal(s).into())
        );
        for bad in [&[42u8][..], &[IRI, 5, b'a'], &[REPEAT, 0], &[IRI, 1, 0xff]] {
            assert!(Reader::new(bad, &none).item().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn row_batches_send_each_term_once() {
        let mut table = TermTable::<u64>::new(3);
        let a: Term = NamedNode::new_unchecked("http://ex.org/a").into();
        let mut w = RowWriter::new(&mut table, 2);
        let c1 = w.cell(1, || Some(a.clone()), &|_| None);
        let c2 = w.default_graph(0);
        let c3 = w.cell(1, || unreachable!(), &|_| None);
        let c4 = w.cell(7, || None, &|_| None);
        w.push_row(&[c1, c2]);
        w.push_row(&[c3, c4]);
        assert_eq!((c1, c2, c3, c4), (1, 2, 1, 0));
        let b = w.finish();
        assert_eq!(b[0], ENCODING_VERSION as u8);
        assert_eq!(b[1], 0);
        assert_eq!(u32::from_le_bytes(b[2..6].try_into().unwrap()), 2);
        // a full table restarts at the next batch
        let mut w = RowWriter::new(&mut table, 1);
        let c = w.cell(2, || Some(a.clone()), &|_| None);
        w.push_row(&[c]);
        let _ = w.finish();
        let w = RowWriter::new(&mut table, 1);
        assert_eq!(w.flags, FLAG_RESTART);
        assert_eq!(empty_batch(4).len(), 12);
    }
}
