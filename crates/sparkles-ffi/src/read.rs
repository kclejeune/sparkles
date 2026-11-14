//! Reads over one snapshot: `find` with its cursor, `contains`, `count` and the graph
//! names. A read transaction, a write transaction (on its view after a flush) and the
//! dataset outside a transaction (on the head snapshot) share them.

use crate::encode::{self, Indexed, Item, Reader, RowWriter, TermTable};
use crate::error::{ErrorKind, FfiError, FfiResult};
use crate::labels::Labels;
use oxrdf::{NamedOrBlankNode, Term};
use parking_lot::Mutex;
use sparkles::QuadIter;
use sparkles::embed::{GraphMatch, QuadPattern, contains_in, count_in, quads_in, term_id_in};
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
    Ok(pattern_of(g, s, p, o))
}

/// The pattern of four items in the order graph, subject, predicate and object, or
/// `None` when a term is of a kind that cannot be in its position.
fn pattern_of(g: Item, s: Item, p: Item, o: Item) -> Option<QuadPattern> {
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
        Item::Term(_) => return None,
    };
    let subject = match s {
        Item::None => None,
        Item::Term(Term::NamedNode(n)) => Some(NamedOrBlankNode::NamedNode(n)),
        Item::Term(Term::BlankNode(b)) => Some(NamedOrBlankNode::BlankNode(b)),
        _ => return None,
    };
    let predicate = match p {
        Item::None => None,
        Item::Term(Term::NamedNode(n)) => Some(n),
        _ => return None,
    };
    let object = match o {
        Item::None => None,
        Item::Term(t) => Some(t),
        _ => return None,
    };
    Some(QuadPattern {
        graph,
        subject,
        predicate,
        object,
    })
}

/// One position of a pattern in a batch of probes, with a term as its index among the
/// terms the batch has given so far.
#[derive(Clone, Copy)]
enum Slot {
    None,
    DefaultGraph,
    UnionGraph,
    Term(usize),
}

/// The terms a batch of probes has given so far, with the id of each one that a probe
/// has looked up, so that a term shared by many quads of the batch is looked up once.
struct Probes<'a> {
    snap: &'a Arc<Snapshot>,
    terms: Vec<Term>,
    /// `None` until the term is looked up, then its id, or `None` when the snapshot does
    /// not know the term
    ids: Vec<Option<Option<Id>>>,
}

impl Probes<'_> {
    fn id(&mut self, term: usize) -> Option<Id> {
        *self.ids[term].get_or_insert_with(|| term_id_in(self.snap, self.terms[term].as_ref()))
    }

    /// Whether one pattern of the batch matches a quad.
    fn contains(&mut self, [g, s, p, o]: [Slot; 4]) -> FfiResult<bool> {
        // A quad whose four terms are given is looked up by its ids, without a copy of
        // its terms. It is the lookup that `contains_in` makes for such a pattern, except
        // where the default graph is read as the union of the named graphs.
        let graph = match g {
            Slot::DefaultGraph if !self.snap.union_default_graph => Some(None),
            Slot::Term(i) => match &self.terms[i] {
                Term::NamedNode(n)
                    if n.as_str() == encode::DEFAULT_GRAPH_IRI
                        || n.as_str() == encode::UNION_GRAPH_IRI =>
                {
                    None
                }
                Term::NamedNode(_) | Term::BlankNode(_) => Some(Some(i)),
                _ => None,
            },
            _ => None,
        };
        if let (Some(graph), Slot::Term(s), Slot::Term(p), Slot::Term(o)) = (graph, s, p, o)
            && matches!(self.terms[s], Term::NamedNode(_) | Term::BlankNode(_))
            && matches!(self.terms[p], Term::NamedNode(_))
        {
            let graph = match graph {
                None => Some(Id::DEFAULT_GRAPH),
                Some(i) => self.id(i),
            };
            // a term that the snapshot does not know is in no quad
            let (Some(g), Some(s)) = (graph, self.id(s)) else {
                return Ok(false);
            };
            let (Some(p), Some(o)) = (self.id(p), self.id(o)) else {
                return Ok(false);
            };
            return Ok(self.snap.contains(&[s, p, o, g])?);
        }
        let item = |slot: Slot| match slot {
            Slot::None => Item::None,
            Slot::DefaultGraph => Item::DefaultGraph,
            Slot::UnionGraph => Item::UnionGraph,
            Slot::Term(i) => Item::Term(self.terms[i].clone()),
        };
        match pattern_of(item(g), item(s), item(p), item(o)) {
            Some(pattern) => Ok(contains_in(self.snap, &pattern)?),
            None => Ok(false),
        }
    }
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

    /// Every quad of a pattern in one batch, for a caller that copies a graph across the
    /// boundary in one call, such as the JVM's ontology model (spec F21 §2.3).
    pub fn find_all(&self, pattern: &[u8]) -> FfiResult<Vec<u8>> {
        let Some(p) = self.pattern(pattern)? else {
            return Ok(encode::empty_batch(4));
        };
        let mut st = CursorState {
            iter: quads_in(self.snap.clone(), &p),
            table: TermTable::new(self.term_cache),
            peeked: None,
            done: false,
        };
        st.batch(self.snap, self.labels, u32::MAX)
    }

    pub fn count(&self, pattern: &[u8]) -> FfiResult<u64> {
        match self.pattern(pattern)? {
            Some(p) => Ok(count_in(self.snap, &p)?),
            None => Ok(0),
        }
    }

    pub fn contains(&self, pattern: &[u8]) -> FfiResult<bool> {
        match self.pattern(pattern)? {
            Some(p) => Ok(contains_in(self.snap, &p)?),
            None => Ok(false),
        }
    }

    /// Whether each pattern of a batch matches a quad: one byte per pattern, 1 for a
    /// match, in the order of the batch. The batch holds the patterns one after another,
    /// four items each as [`decode_pattern`] reads them, and a term may repeat an earlier
    /// term of the batch, so a batch of quads sends each shared IRI once. Each answer is
    /// what [`contains`](Self::contains) gives for the pattern alone.
    pub fn contains_all(&self, batch: &[u8]) -> FfiResult<Vec<u8>> {
        self.labels.with(|resolve, _| {
            let mut r = Reader::new(batch, resolve);
            let mut probes = Probes {
                snap: self.snap,
                terms: Vec::new(),
                ids: Vec::new(),
            };
            let mut found = Vec::new();
            while !r.at_end() {
                let mut slots = [Slot::None; 4];
                for slot in &mut slots {
                    *slot = match r.indexed()? {
                        Indexed::None => Slot::None,
                        Indexed::DefaultGraph => Slot::DefaultGraph,
                        Indexed::UnionGraph => Slot::UnionGraph,
                        Indexed::First(t) => {
                            probes.terms.push(t);
                            probes.ids.push(None);
                            Slot::Term(probes.terms.len() - 1)
                        }
                        Indexed::Repeat(i) => Slot::Term(i),
                    };
                }
                found.push(u8::from(probes.contains(slots)?));
            }
            Ok(found)
        })
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
