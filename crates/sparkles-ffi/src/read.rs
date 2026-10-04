//! Reads over one snapshot: `find` with its cursor, `contains`, `count` and the graph
//! names. A read transaction, a write transaction (on its view after a flush) and the
//! dataset outside a transaction (on the head snapshot) share them.

use crate::encode::{self, Item, Reader, RowWriter, TermTable};
use crate::error::{ErrorKind, FfiError, FfiResult};
use crate::labels::Labels;
use oxrdf::{NamedOrBlankNode, Term};
use parking_lot::Mutex;
use sparkles::QuadIter;
use sparkles::embed::{GraphMatch, QuadPattern, count_in, quads_in};
use sparkles::id::Id;
use sparkles::store::Snapshot;
use std::sync::Arc;

/// Largest batch a cursor returns.
pub const MAX_FIND_ROWS: u32 = 1 << 16;

/// A pattern decoded from a batch, or `None` when a term is of a kind that cannot be in
/// its position (a literal subject), so nothing matches.
pub fn decode_pattern(
    batch: &[u8],
    resolve: &dyn Fn(&str) -> Option<oxrdf::BlankNode>,
) -> FfiResult<Option<QuadPattern>> {
    let mut r = Reader::new(batch, resolve);
    let g = r.item()?;
    let s = r.item()?;
    let p = r.item()?;
    let o = r.item()?;
    let graph = match g {
        Item::None => GraphMatch::Any,
        Item::DefaultGraph => GraphMatch::Default,
        Item::UnionGraph => GraphMatch::Union,
        Item::Term(Term::NamedNode(n)) if n.as_str() == encode::DEFAULT_GRAPH_IRI => {
            GraphMatch::Default
        }
        Item::Term(Term::NamedNode(n)) if n.as_str() == encode::UNION_GRAPH_IRI => {
            GraphMatch::Union
        }
        Item::Term(Term::NamedNode(n)) => GraphMatch::Named(n.into()),
        Item::Term(Term::BlankNode(b)) => GraphMatch::Named(b.into()),
        Item::Term(_) => return Ok(None),
    };
    let subject = match s {
        Item::None => None,
        Item::Term(Term::NamedNode(n)) => Some(NamedOrBlankNode::NamedNode(n)),
        Item::Term(Term::BlankNode(b)) => Some(NamedOrBlankNode::BlankNode(b)),
        _ => return Ok(None),
    };
    let predicate = match p {
        Item::None => None,
        Item::Term(Term::NamedNode(n)) => Some(n),
        _ => return Ok(None),
    };
    let object = match o {
        Item::None => None,
        Item::Term(t) => Some(t),
        _ => return Ok(None),
    };
    Ok(Some(QuadPattern {
        graph,
        subject,
        predicate,
        object,
    }))
}

/// The first batch of a `find`, with the cursor for the rest (absent when the first
/// batch held every match).
#[derive(uniffi::Record)]
pub struct FindResult {
    pub batch: Vec<u8>,
    pub cursor: Option<Arc<FfiCursor>>,
}

/// A batch of a cursor or a query result; `done` when no rows follow it.
#[derive(uniffi::Record)]
pub struct Batch {
    pub batch: Vec<u8>,
    pub done: bool,
}

struct CursorState {
    iter: QuadIter,
    table: TermTable<u64>,
    peeked: Option<[Id; 4]>,
    done: bool,
}

/// The rest of the quads of a `find`, read from one snapshot.
#[derive(uniffi::Object)]
pub struct FfiCursor {
    snap: Arc<Snapshot>,
    labels: Labels,
    state: Mutex<Option<CursorState>>,
}

impl CursorState {
    /// Up to `max` quads as a batch of four columns, the graph first.
    fn batch(&mut self, snap: &Snapshot, labels: &Labels, max: u32) -> FfiResult<Vec<u8>> {
        let mut w = RowWriter::new(&mut self.table, 4);
        labels.with(|_, label| -> FfiResult<()> {
            while w.rows() < max {
                let q = match self.peeked.take() {
                    Some(q) => q,
                    None => match self.iter.next_ids() {
                        Some(q) => q?,
                        None => {
                            self.done = true;
                            break;
                        }
                    },
                };
                let g = if q[3] == Id::DEFAULT_GRAPH {
                    w.default_graph(q[3].0)
                } else {
                    w.cell(q[3].0, || snap.term(q[3]), label)
                };
                let s = w.cell(q[0].0, || snap.term(q[0]), label);
                let p = w.cell(q[1].0, || snap.term(q[1]), label);
                let o = w.cell(q[2].0, || snap.term(q[2]), label);
                w.push_row(&[g, s, p, o]);
            }
            Ok(())
        })?;
        // find out whether more follow, so that a cursor that is done says so
        if !self.done && self.peeked.is_none() {
            match self.iter.next_ids() {
                Some(q) => self.peeked = Some(q?),
                None => self.done = true,
            }
        }
        Ok(w.finish())
    }
}

#[uniffi::export]
impl FfiCursor {
    /// The next batch of up to `max_rows` quads.
    pub fn next_batch(&self, max_rows: u32) -> FfiResult<Batch> {
        let mut st = self.state.lock();
        let Some(s) = st.as_mut() else {
            return Ok(Batch {
                batch: encode::empty_batch(4),
                done: true,
            });
        };
        let batch = s.batch(&self.snap, &self.labels, max_rows.clamp(1, MAX_FIND_ROWS))?;
        let done = s.done;
        if done {
            *st = None;
        }
        Ok(Batch { batch, done })
    }

    /// Free the cursor's state (and its hold on the snapshot's scan) before the object
    /// itself is freed.
    pub fn release(&self) {
        *self.state.lock() = None;
    }
}

/// Reads over one snapshot.
pub struct SnapReader<'a> {
    pub snap: &'a Arc<Snapshot>,
    pub labels: &'a Labels,
    pub term_cache: usize,
}

impl SnapReader<'_> {
    fn pattern(&self, batch: &[u8]) -> FfiResult<Option<QuadPattern>> {
        self.labels
            .with(|resolve, _| decode_pattern(batch, resolve))
    }

    pub fn find(&self, pattern: &[u8], first_rows: u32) -> FfiResult<FindResult> {
        let Some(p) = self.pattern(pattern)? else {
            return Ok(FindResult {
                batch: encode::empty_batch(4),
                cursor: None,
            });
        };
        let mut st = CursorState {
            iter: quads_in(self.snap.clone(), &p),
            table: TermTable::new(self.term_cache),
            peeked: None,
            done: false,
        };
        let batch = st.batch(self.snap, self.labels, first_rows.clamp(1, MAX_FIND_ROWS))?;
        let cursor = (!st.done).then(|| {
            Arc::new(FfiCursor {
                snap: self.snap.clone(),
                labels: self.labels.clone(),
                state: Mutex::new(Some(st)),
            })
        });
        Ok(FindResult { batch, cursor })
    }

    pub fn count(&self, pattern: &[u8]) -> FfiResult<u64> {
        match self.pattern(pattern)? {
            Some(p) => Ok(count_in(self.snap, &p)?),
            None => Ok(0),
        }
    }

    pub fn contains(&self, pattern: &[u8]) -> FfiResult<bool> {
        Ok(self.count(pattern)? > 0)
    }

    /// The named graphs that have quads, as a batch of one column.
    pub fn graph_names(&self) -> FfiResult<Vec<u8>> {
        let ids = self.snap.graph_ids()?;
        let mut table = TermTable::<u64>::new(usize::MAX);
        let mut w = RowWriter::new(&mut table, 1);
        self.labels.with(|_, label| {
            for g in ids {
                let c = w.cell(g.0, || self.snap.term(g), label);
                if c != 0 {
                    w.push_row(&[c]);
                }
            }
        });
        Ok(w.finish())
    }
}

/// A pattern whose graph is the union graph cannot be written to.
pub fn not_writable(what: &str) -> FfiError {
    FfiError::new(ErrorKind::Invalid, format!("{what} the union graph"))
}
