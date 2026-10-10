//! Terms that JavaScript sends in binary form (P05 §6.2), and each dataset's table of
//! the terms that JavaScript refers to by number.
//!
//! A request has the layout of a result batch (`wire.rs`): one string of term text and
//! one `Uint32Array` with four header words (flags, term entries including the unused
//! entry 0, rows and cells per row), four words per term entry and then the cells. The
//! first word of an entry is the kind of `wire.rs` plus a handle number shifted left by
//! 8. A handle that is not zero asks the dataset's table to keep the term under that
//! number, which JavaScript chose and will send instead of the term next time. A cell,
//! and the datatype or triple-term parts of an entry, is 0 for none, an entry index, or
//! a handle number with bit 31 set. Flag bit 0 empties the table before the request is
//! read.
//!
//! The table also keeps the engine id of each IRI and literal, which saves the
//! vocabulary lookup when the term comes again. An id is used only while the store has
//! the vocabulary generation it came from and the dataset's epoch is unchanged. The
//! epoch moves when a transaction ends without a commit, since a failed commit takes
//! back the terms it added.

use crate::wire::Cell;
use oxrdf::{
    BaseDirection, BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple,
};
use parking_lot::Mutex;
use sparkles::embed::{GraphMatch, QuadPattern};
use sparkles::id::{Id, Tag};
use sparkles::store::Snapshot;
use sparkles::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Flag bit 0: empty the table first.
const RESET: u32 = 1;
/// A cell or part that names a handle rather than an entry of the request.
const HANDLE_BIT: u32 = 1 << 31;
/// Handles at most, which bounds the table's memory. JavaScript keeps the same limit.
const MAX_HANDLES: usize = 1 << 16;

const NAMED: u32 = 1;
const BLANK: u32 = 2;
const TYPED: u32 = 3;
const LANG: u32 = 4;
const LANG_LTR: u32 = 5;
const LANG_RTL: u32 = 6;
const DEFAULT_GRAPH: u32 = 7;
const TRIPLE: u32 = 8;

/// Where an id is valid: the vocabulary generation and the dataset's epoch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub uid: u64,
    pub epoch: u64,
}

/// A term of a request, with the engine id it had when last used.
pub struct Handle {
    pub cell: Cell,
    /// no blank nodes, whose labels a transaction scopes, so the id may be kept
    keep: bool,
    id: Mutex<Known>,
}

/// What a handle knows of its term in the store.
#[derive(Clone, Copy)]
enum Known {
    Nothing,
    Id(Stamp, Id),
    /// Not in the vocabulary while the delta vocabulary had this many terms. The
    /// vocabulary of a generation only grows until a rollback, which changes the epoch.
    Absent(Stamp, u64),
}

impl Handle {
    fn new(cell: Cell) -> Handle {
        let keep = match &cell {
            Cell::DefaultGraph | Cell::Term(Term::NamedNode(_)) => true,
            Cell::Term(Term::Literal(l)) => !sparkles::sparql::cdt::may_name_bnodes(l),
            Cell::Term(_) => false,
        };
        Handle {
            cell,
            keep,
            id: Mutex::new(Known::Nothing),
        }
    }

    fn cached(&self, stamp: Stamp) -> Option<Id> {
        match *self.id.lock() {
            Known::Id(s, id) if s == stamp => Some(id),
            _ => None,
        }
    }

    fn remember(&self, stamp: Stamp, id: Id) {
        if self.keep {
            *self.id.lock() = Known::Id(stamp, id);
        }
    }

    /// Whether the term was missing from a vocabulary of the same length, so that a probe
    /// for an absent term, such as `has()` with a value that was never stored, skips the
    /// lookup of its text.
    fn known_absent(&self, stamp: Stamp, dvocab_len: u64) -> bool {
        matches!(*self.id.lock(), Known::Absent(s, n) if s == stamp && n == dvocab_len)
    }

    fn remember_absent(&self, stamp: Stamp, dvocab_len: u64) {
        if self.keep {
            *self.id.lock() = Known::Absent(stamp, dvocab_len);
        }
    }

    fn term(&self) -> Result<&Term> {
        match &self.cell {
            Cell::Term(t) => Ok(t),
            Cell::DefaultGraph => Err(Error::invalid("the default graph is not a term here")),
        }
    }

    fn subject(&self) -> Result<NamedOrBlankNode> {
        match &self.cell {
            Cell::Term(Term::NamedNode(n)) => Ok(n.clone().into()),
            Cell::Term(Term::BlankNode(b)) => Ok(b.clone().into()),
            _ => Err(Error::invalid(
                "subject and graph must be an IRI or blank node",
            )),
        }
    }

    fn predicate(&self) -> Result<NamedNode> {
        match &self.cell {
            Cell::Term(Term::NamedNode(n)) => Ok(n.clone()),
            _ => Err(Error::invalid("predicate must be an IRI")),
        }
    }

    fn graph(&self) -> Result<GraphName> {
        Ok(match &self.cell {
            Cell::DefaultGraph => GraphName::DefaultGraph,
            _ => match self.subject()? {
                NamedOrBlankNode::NamedNode(n) => n.into(),
                NamedOrBlankNode::BlankNode(b) => b.into(),
            },
        })
    }

    /// Whether the term can stand in position `pos` (0 to 3) of a quad.
    fn check(&self, pos: usize) -> Result<()> {
        match pos {
            0 => self.subject().map(drop),
            1 => self.predicate().map(drop),
            2 => self.term().map(drop),
            _ => self.graph().map(drop),
        }
    }
}

/// A dataset's handles. Only the JavaScript thread reads requests, so the lock is never
/// contended.
#[derive(Default)]
pub struct Handles {
    table: Mutex<Vec<Option<Arc<Handle>>>>,
    epoch: AtomicU64,
}

impl Handles {
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Forget every kept id, after a transaction that may have taken back terms.
    pub fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

/// A request's cells, `rows` rows of `width`, each a term or none.
pub struct Request {
    pub rows: usize,
    pub width: usize,
    pub cells: Vec<Option<Arc<Handle>>>,
}

/// The text of a request, cut by UTF-16 offsets.
enum Text<'a> {
    Ascii(&'a str),
    Wide(Vec<u16>),
}

impl Text<'_> {
    fn slice(&self, a: u32, b: u32) -> Result<String> {
        let (a, b) = (a as usize, b as usize);
        match self {
            Text::Ascii(s) => s
                .get(a..b)
                .map(str::to_owned)
                .ok_or_else(|| Error::invalid("term text out of range")),
            Text::Wide(units) => units
                .get(a..b)
                .and_then(|u| String::from_utf16(u).ok())
                .ok_or_else(|| Error::invalid("term text out of range")),
        }
    }
}

/// Read a request, keeping the terms it gives handles. A request that fails empties the
/// table, and JavaScript then starts its own over.
pub fn read(handles: &Handles, text: &str, data: &[u32]) -> Result<Request> {
    let mut table = handles.table.lock();
    let r = read_into(&mut table, text, data);
    if r.is_err() {
        table.clear();
    }
    r
}

fn read_into(table: &mut Vec<Option<Arc<Handle>>>, text: &str, data: &[u32]) -> Result<Request> {
    let bad = || Error::invalid("malformed term request");
    let [flags, entries, rows, width] = *data.get(..4).ok_or_else(bad)? else {
        return Err(bad());
    };
    let (entries, rows, width) = (entries as usize, rows as usize, width as usize);
    let cells_at = entries
        .checked_mul(4)
        .and_then(|n| n.checked_add(4))
        .ok_or_else(bad)?;
    // The request is a prefix of `data`: JavaScript hands over its whole request buffer,
    // which saves making a view of the exact length for every call.
    let data = match rows
        .checked_mul(width)
        .and_then(|n| n.checked_add(cells_at))
    {
        Some(len) if entries > 0 && len <= data.len() => &data[..len],
        _ => return Err(bad()),
    };
    if flags & RESET != 0 {
        table.clear();
    }
    let text = if text.is_ascii() {
        Text::Ascii(text)
    } else {
        Text::Wide(text.encode_utf16().collect())
    };
    let mut terms: Vec<Option<Arc<Handle>>> = Vec::with_capacity(entries);
    terms.push(None);
    for i in 1..entries {
        let m = 4 + 4 * i;
        let (head, a, b, c) = (data[m], data[m + 1], data[m + 2], data[m + 3]);
        let (kind, handle) = (head & 0xff, (head >> 8) as usize);
        let part = |v: u32| resolve(table, &terms, v)?.ok_or_else(bad);
        let cell = match kind {
            NAMED => Cell::Term(
                NamedNode::new(text.slice(a, b)?)
                    .map_err(|e| Error::invalid(e.to_string()))?
                    .into(),
            ),
            BLANK => Cell::Term(
                BlankNode::new(text.slice(a, b)?)
                    .map_err(|e| Error::invalid(e.to_string()))?
                    .into(),
            ),
            TYPED => {
                let datatype = part(c)?.predicate()?;
                Cell::Term(Literal::new_typed_literal(text.slice(a, b)?, datatype).into())
            }
            LANG | LANG_LTR | LANG_RTL => {
                let value = text.slice(a, b)?;
                let language = text.slice(b, c)?;
                let literal = match kind {
                    LANG => Literal::new_language_tagged_literal(value, language),
                    _ => Literal::new_directional_language_tagged_literal(
                        value,
                        language,
                        if kind == LANG_LTR {
                            BaseDirection::Ltr
                        } else {
                            BaseDirection::Rtl
                        },
                    ),
                }
                .map_err(|e| Error::invalid(e.to_string()))?;
                Cell::Term(literal.into())
            }
            DEFAULT_GRAPH => Cell::DefaultGraph,
            TRIPLE => Cell::Term(Term::Triple(Box::new(Triple::new(
                part(a)?.subject()?,
                part(b)?.predicate()?,
                part(c)?.term()?.clone(),
            )))),
            _ => return Err(Error::invalid(format!("unknown term kind {kind}"))),
        };
        let h = Arc::new(Handle::new(cell));
        if handle != 0 {
            if handle >= MAX_HANDLES {
                return Err(bad());
            }
            if table.len() <= handle {
                table.resize(handle + 1, None);
            }
            table[handle] = Some(h.clone());
        }
        terms.push(Some(h));
    }
    let cells = data[cells_at..]
        .iter()
        .map(|&v| resolve(table, &terms, v))
        .collect::<Result<_>>()?;
    Ok(Request { rows, width, cells })
}

fn resolve(
    table: &[Option<Arc<Handle>>],
    terms: &[Option<Arc<Handle>>],
    v: u32,
) -> Result<Option<Arc<Handle>>> {
    if v == 0 {
        return Ok(None);
    }
    let found = if v & HANDLE_BIT != 0 {
        table.get((v & !HANDLE_BIT) as usize)
    } else {
        terms.get(v as usize)
    };
    match found {
        Some(Some(h)) => Ok(Some(h.clone())),
        _ => Err(Error::invalid("unknown term in a request")),
    }
}

impl Request {
    fn row(&self, r: usize) -> &[Option<Arc<Handle>>] {
        &self.cells[r * self.width..(r + 1) * self.width]
    }

    fn quad_handles(&self, r: usize) -> Result<[&Handle; 4]> {
        let row = self.row(r);
        if row.len() != 4 {
            return Err(Error::invalid("a quad has four terms"));
        }
        let get = |i: usize| -> Result<&Handle> {
            let h = row[i]
                .as_deref()
                .ok_or_else(|| Error::invalid("a quad needs all four terms"))?;
            h.check(i)?;
            Ok(h)
        };
        Ok([get(0)?, get(1)?, get(2)?, get(3)?])
    }

    /// Row `r` as a quad.
    pub fn quad(&self, r: usize) -> Result<Quad> {
        let [s, p, o, g] = self.quad_handles(r)?;
        Ok(Quad::new(
            s.subject()?,
            p.predicate()?,
            o.term()?.clone(),
            g.graph()?,
        ))
    }

    /// Row `r` as a pattern, where a missing term matches any.
    pub fn pattern(&self, r: usize) -> Result<QuadPattern> {
        let row = self.row(r);
        if row.len() != 4 {
            return Err(Error::invalid("a quad pattern has four terms"));
        }
        Ok(QuadPattern {
            subject: row[0].as_deref().map(Handle::subject).transpose()?,
            predicate: row[1].as_deref().map(Handle::predicate).transpose()?,
            object: row[2].as_deref().map(|h| h.term().cloned()).transpose()?,
            graph: match row[3].as_deref() {
                None => GraphMatch::Any,
                Some(h) => match &h.cell {
                    Cell::DefaultGraph => GraphMatch::Default,
                    _ => GraphMatch::Named(h.subject()?),
                },
            },
        })
    }

    /// Insert every row in `tx`, through the kept ids where they are valid; the number
    /// of quads that were not there.
    pub fn insert(&self, tx: &mut sparkles::Transaction<'_>, epoch: u64) -> Result<u32> {
        let stamp = Stamp {
            uid: tx.vocab_uid(),
            epoch,
        };
        let mut n = 0;
        for r in 0..self.rows {
            let hs = self.quad_handles(r)?;
            let mut ids = [Id::DEFAULT_GRAPH; 4];
            for (id, h) in ids.iter_mut().zip(hs) {
                *id = match h.cached(stamp) {
                    Some(id) => id,
                    None => {
                        let id = match &h.cell {
                            Cell::DefaultGraph => Id::DEFAULT_GRAPH,
                            Cell::Term(t) => tx.intern_term(t)?,
                        };
                        h.remember(stamp, id);
                        id
                    }
                };
            }
            n += u32::from(tx.insert_ids(ids)?);
        }
        Ok(n)
    }

    /// Remove every row from `tx`; the number of quads that were there.
    pub fn remove(&self, tx: &mut sparkles::Transaction<'_>) -> Result<u32> {
        let mut n = 0;
        for r in 0..self.rows {
            n += u32::from(tx.remove(self.quad(r)?.as_ref())?);
        }
        Ok(n)
    }

    /// Whether `snap` has the quad of row 0, looked up through the kept ids where they
    /// are valid. Returns `None` when the lookup needs the general pattern match: a
    /// blank node or triple term, or the default graph read as the union of the named
    /// graphs.
    pub fn contains(&self, snap: &Snapshot, epoch: u64) -> Result<Option<bool>> {
        let hs = self.quad_handles(0)?;
        if hs.iter().any(|h| !h.keep)
            || (snap.union_default_graph && matches!(hs[3].cell, Cell::DefaultGraph))
        {
            return Ok(None);
        }
        let stamp = Stamp {
            uid: snap.generation.uid,
            epoch,
        };
        let mut ids = [Id::DEFAULT_GRAPH; 4];
        for (id, h) in ids.iter_mut().zip(hs) {
            *id = match h.cached(stamp) {
                // a term that a transaction added after this snapshot is not in it
                Some(id) if id.tag() == Tag::Delta && id.payload() >= snap.dvocab_len => {
                    return Ok(Some(false));
                }
                Some(id) => id,
                None => {
                    if h.known_absent(stamp, snap.dvocab_len) {
                        return Ok(Some(false));
                    }
                    let found = match &h.cell {
                        Cell::DefaultGraph => Some(Id::DEFAULT_GRAPH),
                        Cell::Term(t) => snap.lookup_term(t),
                    };
                    let Some(id) = found else {
                        h.remember_absent(stamp, snap.dvocab_len);
                        return Ok(Some(false));
                    };
                    h.remember(stamp, id);
                    id
                }
            };
        }
        snap.contains(&ids).map(Some)
    }
}
