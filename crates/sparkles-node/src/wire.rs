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

use napi::bindgen_prelude::{ToNapiValue, TypeName, Uint32Array, ValidateNapiValue};
use napi_derive::napi;
use oxrdf::{BaseDirection, NamedOrBlankNode, Term};
use rustc_hash::FxHashMap;
use sparkles::id::Id;
use sparkles::store::Snapshot;

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
    pub text: Text,
    #[napi(ts_type = "Uint32Array")]
    pub data: Words,
}

/// The words of a batch. A small batch, such as a lookup's, is copied into a new
/// `ArrayBuffer`, which V8 frees with less work than an external buffer that calls back
/// into Rust. A larger one is handed over without a copy.
pub struct Words(Vec<u32>);

/// The largest batch, in words, that is copied rather than handed over.
const COPY_WORDS: usize = 4096;

impl TypeName for Words {
    fn type_name() -> &'static str {
        "Uint32Array"
    }
    fn value_type() -> napi::ValueType {
        napi::ValueType::Object
    }
}

impl ValidateNapiValue for Words {}

impl ToNapiValue for Words {
    unsafe fn to_napi_value(
        env: napi::sys::napi_env,
        val: Self,
    ) -> napi::Result<napi::sys::napi_value> {
        let len = val.0.len();
        if len == 0 || len > COPY_WORDS {
            // SAFETY: as napi-rs's own conversion of a `Uint32Array`
            return unsafe { Uint32Array::to_napi_value(env, Uint32Array::new(val.0)) };
        }
        let bytes = len * size_of::<u32>();
        let mut buffer = std::ptr::null_mut();
        let mut data = std::ptr::null_mut();
        // SAFETY: V8 allocates `bytes` bytes at `data`, which the copy fills
        let status =
            unsafe { napi::sys::napi_create_arraybuffer(env, bytes, &mut data, &mut buffer) };
        napi::check_status!(status, "failed to create a result buffer")?;
        // SAFETY: `data` holds `bytes` bytes, and the vector as many
        unsafe {
            std::ptr::copy_nonoverlapping(val.0.as_ptr().cast::<u8>(), data.cast::<u8>(), bytes)
        };
        let mut out = std::ptr::null_mut();
        // SAFETY: the array views the whole buffer just made
        let status = unsafe {
            napi::sys::napi_create_typedarray(
                env,
                napi::sys::TypedarrayType::uint32_array,
                len,
                buffer,
                0,
                &mut out,
            )
        };
        napi::check_status!(status, "failed to create a result array")?;
        Ok(out)
    }
}

impl napi::bindgen_prelude::FromNapiValue for Words {
    unsafe fn from_napi_value(
        env: napi::sys::napi_env,
        value: napi::sys::napi_value,
    ) -> napi::Result<Self> {
        // SAFETY: as napi-rs's own conversion of a `Uint32Array`
        let array = unsafe { Uint32Array::from_napi_value(env, value)? };
        Ok(Words(array.to_vec()))
    }
}

/// The text of a batch. Text that is all ASCII, as IRIs nearly always are, becomes a
/// JavaScript string through `napi_create_string_latin1`, which copies the bytes without
/// decoding UTF-8.
pub struct Text {
    text: String,
    ascii: bool,
}

impl TypeName for Text {
    fn type_name() -> &'static str {
        "String"
    }
    fn value_type() -> napi::ValueType {
        napi::ValueType::String
    }
}

impl ValidateNapiValue for Text {}

impl ToNapiValue for Text {
    unsafe fn to_napi_value(
        env: napi::sys::napi_env,
        val: Self,
    ) -> napi::Result<napi::sys::napi_value> {
        if !val.ascii {
            // SAFETY: as napi-rs's own conversion of a `String`
            return unsafe { String::to_napi_value(env, val.text) };
        }
        let mut out = std::ptr::null_mut();
        // SAFETY: the bytes are valid for the call, and ASCII is valid Latin-1
        let status = unsafe {
            napi::sys::napi_create_string_latin1(
                env,
                val.text.as_ptr().cast(),
                val.text.len() as isize,
                &mut out,
            )
        };
        napi::check_status!(status, "failed to create a result string")?;
        Ok(out)
    }
}

impl napi::bindgen_prelude::FromNapiValue for Text {
    unsafe fn from_napi_value(
        env: napi::sys::napi_env,
        value: napi::sys::napi_value,
    ) -> napi::Result<Self> {
        // SAFETY: as napi-rs's own conversion of a `String`
        let text = unsafe { String::from_napi_value(env, value)? };
        Ok(Text {
            ascii: text.is_ascii(),
            text,
        })
    }
}

/// Builds the term entries of a batch, each distinct term once.
pub struct TermTable {
    text: String,
    /// the length of `text` in UTF-16 code units
    units: u32,
    /// `text` is all ASCII
    ascii: bool,
    entries: Vec<u32>,
    index: FxHashMap<Cell, u32>,
    /// the entries of the engine ids that `quad_ids` decoded
    ids: FxHashMap<u64, u32>,
    /// the entries of the datatype IRIs, which few batches have more than a handful of
    datatypes: Vec<(String, u32)>,
}

impl TermTable {
    /// A table sized for a batch of about `rows` rows. It grows as needed, and a small
    /// first batch, such as a lookup's, allocates little.
    pub fn new(rows: usize) -> Self {
        let terms = rows.clamp(4, 256) * 2;
        let mut entries = Vec::with_capacity(4 * terms);
        entries.extend([0; 4]);
        TermTable {
            text: String::with_capacity(terms * 48),
            units: 0,
            ascii: true,
            entries,
            index: FxHashMap::default(),
            ids: FxHashMap::default(),
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
            self.ascii = false;
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

    /// The entries of a quad of engine ids, each id decoded once per batch; `None` when
    /// one of them has no term.
    pub fn quad_ids(&mut self, snap: &Snapshot, q: [Id; 4]) -> Option<[u32; 4]> {
        let mut out = [0; 4];
        for (cell, id) in out.iter_mut().zip(q) {
            *cell = match self.ids.get(&id.0) {
                Some(e) => *e,
                None => {
                    let e = if id == Id::DEFAULT_GRAPH {
                        self.cell(Cell::DefaultGraph)
                    } else {
                        // an id is one term, so the id map alone keeps it once
                        self.term(&snap.term(id)?)
                    };
                    self.ids.insert(id.0, e);
                    e
                }
            };
        }
        Some(out)
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
            text: Text {
                text: self.text,
                ascii: self.ascii,
            },
            data: Words(data),
        }
    }
}
