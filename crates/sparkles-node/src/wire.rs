//! The binary form of a result batch that JavaScript decodes (P05 §6.2).
//!
//! A batch is one string of term text and one `Uint32Array`. The array starts with four
//! header words: the number of term entries (entry 0 stands for unbound), the number of
//! rows, the cells per row and a flags word whose bit 0 says the result is drained. Four
//! words per term entry follow, then the cells of every row in order. A cell is the index
//! of a term entry, or 0 when the variable is unbound.
//!
//! A term entry is `kind, a, b, c`, where `a` and `b` are UTF-16 offsets into the text,
//! so that JavaScript can cut the term's text with `slice` and need not decode bytes.
//!
//! | kind | term | `a..b` | `c` |
//! |---|---|---|---|
//! | 1 | IRI | the IRI | 0 |
//! | 2 | blank node | the label | 0 |
//! | 3 | typed literal | the lexical form | the entry of the datatype IRI |
//! | 4 | language-tagged string | the lexical form | the end of the tag, which starts at `b` |
//! | 5 | the same with direction `ltr` | as 4 | as 4 |
//! | 6 | the same with direction `rtl` | as 4 | as 4 |
//! | 7 | the default graph | 0, 0 | 0 |
//! | 8 | triple term | `a`, `b` and `c` are the entries of its subject, predicate and object | |

use napi::bindgen_prelude::Uint32Array;
use napi_derive::napi;
use oxrdf::{BaseDirection, NamedOrBlankNode, Term};
use std::collections::HashMap;

pub const HEADER: usize = 4;
const NAMED: u32 = 1;
const BLANK: u32 = 2;
const TYPED: u32 = 3;
const LANG: u32 = 4;
const LANG_LTR: u32 = 5;
const LANG_RTL: u32 = 6;
const DEFAULT_GRAPH: u32 = 7;
const TRIPLE: u32 = 8;

/// A cell of a wire row: a term, or the default graph in the fourth cell of a quad.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Cell {
    Term(Term),
    DefaultGraph,
}

/// One batch as JavaScript receives it.
#[napi(object)]
pub struct WireBatch {
    pub text: String,
    pub data: Uint32Array,
}

/// Builds the term entries of a batch, each distinct term once.
pub struct TermTable {
    text: String,
    /// the length of `text` in UTF-16 code units
    units: u32,
    entries: Vec<u32>,
    index: HashMap<Cell, u32>,
    /// the entries of the datatype IRIs, which few batches have more than a handful of
    datatypes: Vec<(String, u32)>,
}

impl TermTable {
    pub fn new() -> Self {
        let mut entries = Vec::with_capacity(4 * 256);
        entries.extend([0; 4]);
        TermTable {
            text: String::with_capacity(16 << 10),
            units: 0,
            entries,
            index: HashMap::with_capacity(256),
            datatypes: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len() / 4
    }

    /// The bytes of term text so far, for the batch's byte limit.
    pub fn text_bytes(&self) -> usize {
        self.text.len()
    }

    fn push_text(&mut self, s: &str) -> (u32, u32) {
        let start = self.units;
        self.text.push_str(s);
        self.units += if s.is_ascii() {
            s.len() as u32
        } else {
            s.encode_utf16().count() as u32
        };
        (start, self.units)
    }

    fn push_entry(&mut self, kind: u32, a: u32, b: u32, c: u32) -> u32 {
        let id = self.len() as u32;
        self.entries.extend([kind, a, b, c]);
        id
    }

    /// The entry of a cell, added if the batch does not have it yet.
    pub fn cell(&mut self, cell: Cell) -> u32 {
        if let Some(id) = self.index.get(&cell) {
            return *id;
        }
        let id = match &cell {
            Cell::DefaultGraph => self.push_entry(DEFAULT_GRAPH, 0, 0, 0),
            Cell::Term(t) => self.term(t),
        };
        self.index.insert(cell, id);
        id
    }

    fn term(&mut self, t: &Term) -> u32 {
        match t {
            Term::NamedNode(n) => {
                let (a, b) = self.push_text(n.as_str());
                self.push_entry(NAMED, a, b, 0)
            }
            Term::BlankNode(n) => {
                let (a, b) = self.push_text(n.as_str());
                self.push_entry(BLANK, a, b, 0)
            }
            Term::Literal(l) => match l.language() {
                Some(language) => {
                    let kind = match l.direction() {
                        None => LANG,
                        Some(BaseDirection::Ltr) => LANG_LTR,
                        Some(BaseDirection::Rtl) => LANG_RTL,
                    };
                    let (a, b) = self.push_text(l.value());
                    let (_, c) = self.push_text(language);
                    self.push_entry(kind, a, b, c)
                }
                None => {
                    let datatype = self.datatype(l.datatype().as_str());
                    let (a, b) = self.push_text(l.value());
                    self.push_entry(TYPED, a, b, datatype)
                }
            },
            Term::Triple(t) => {
                let subject = self.cell(Cell::Term(match &t.subject {
                    NamedOrBlankNode::NamedNode(n) => n.clone().into(),
                    NamedOrBlankNode::BlankNode(n) => n.clone().into(),
                }));
                let predicate = self.cell(Cell::Term(t.predicate.clone().into()));
                let object = self.cell(Cell::Term(t.object.clone()));
                self.push_entry(TRIPLE, subject, predicate, object)
            }
        }
    }

    fn datatype(&mut self, iri: &str) -> u32 {
        if let Some((_, id)) = self.datatypes.iter().find(|(d, _)| d == iri) {
            return *id;
        }
        let (a, b) = self.push_text(iri);
        let id = self.push_entry(NAMED, a, b, 0);
        self.datatypes.push((iri.to_owned(), id));
        id
    }

    /// The finished batch: `cells` holds `rows` rows of `width` cells.
    pub fn finish(self, cells: Vec<u32>, rows: usize, width: usize, done: bool) -> WireBatch {
        let mut data = Vec::with_capacity(HEADER + self.entries.len() + cells.len());
        data.extend([
            self.len() as u32,
            rows as u32,
            width as u32,
            u32::from(done),
        ]);
        data.extend_from_slice(&self.entries);
        data.extend_from_slice(&cells);
        WireBatch {
            text: self.text,
            data: Uint32Array::new(data),
        }
    }
}
