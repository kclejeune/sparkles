//! Prepared queries and their results (`FfiQuery`).

use crate::encode::{self, Item, Reader, RowWriter, TermTable};
use crate::error::{ErrorKind, FfiError, FfiResult};
use crate::labels::Labels;
use crate::read::Batch;
use oxrdf::Term;
use parking_lot::Mutex;
use sparkles::sparql::{QueryKind, QueryOptions, QueryResult};
use sparkles::store::Snapshot;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The named graph that reasoning writes and `include_inferred` reads.
pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";

/// Rows of a later batch at most, and its size in bytes at most.
const MAX_ROWS: u32 = 1 << 16;
const MAX_BATCH_BYTES: usize = 1 << 20;

/// The options of one query or update.
#[derive(uniffi::Record)]
pub struct QueryOpts {
    pub base_iri: Option<String>,
    /// overrides the store's union default graph for this request
    pub union_default_graph: Option<bool>,
    /// merge the reasoner's `urn:x-sparkles:inferred` graph into the default graph
    pub include_inferred: bool,
    pub timeout_ms: Option<u64>,
    pub max_rows: Option<u64>,
    pub max_memory_bytes: Option<u64>,
    pub max_rows_produced: Option<u64>,
    /// run SERVICE clauses
    pub allow_service: bool,
    /// SERVICE and remote `LOAD` may reach loopback and private addresses (Jena's
    /// behaviour); false applies the server's rules
    pub allow_private_network: bool,
    /// pre-bound variables: the names, and their values as one batch of terms in the same
    /// order (tag 0 leaves a variable unbound)
    pub binding_names: Vec<String>,
    pub binding_values: Vec<u8>,
    /// neither read nor write the result cache
    pub no_cache: bool,
}

impl QueryOpts {
    /// The engine's options, with `cancel` as the cancellation flag.
    pub fn to_options(&self, labels: &Labels, cancel: Arc<AtomicBool>) -> FfiResult<QueryOptions> {
        let mut o = QueryOptions {
            base_iri: self.base_iri.clone(),
            union_default_graph: self.union_default_graph,
            timeout: self.timeout_ms.map(Duration::from_millis),
            max_rows: self.max_rows.map(|m| m as usize),
            max_memory_bytes: self.max_memory_bytes,
            max_rows_produced: self.max_rows_produced,
            allow_service: self.allow_service,
            cancel: Some(cancel),
            no_cache: self.no_cache,
            ..Default::default()
        };
        o.outbound.allow_private = self.allow_private_network;
        if self.include_inferred {
            o.default_graph_extra = vec![INFERRED_GRAPH.to_string()];
        }
        if !self.binding_names.is_empty() {
            labels.with(|resolve, _| -> FfiResult<()> {
                let mut r = Reader::new(&self.binding_values, resolve);
                for name in &self.binding_names {
                    if let Item::Term(t) = r.item()? {
                        o.initial_bindings
                            .push((name.trim_start_matches(['?', '$']).to_string(), t));
                    }
                }
                Ok(())
            })?;
        }
        Ok(o)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiQueryKind {
    Select,
    Ask,
    Construct,
    Describe,
}

#[derive(Clone, Debug, Default, uniffi::Record)]
pub struct Timing {
    pub parse_ms: f64,
    pub plan_ms: f64,
    pub exec_ms: f64,
    pub total_ms: f64,
}

/// What `execute` returns: the query's form, its variables (SELECT) or the answer (ASK),
/// the first batch of rows, and whether more follow. CONSTRUCT and DESCRIBE give quads
/// in four columns, the graph first.
#[derive(Debug, uniffi::Record)]
pub struct Execution {
    pub kind: FfiQueryKind,
    pub variables: Vec<String>,
    pub boolean: bool,
    pub batch: Vec<u8>,
    pub done: bool,
    pub timing: Timing,
}

enum Rows {
    Select(Box<QueryResult>, TermTable<u64>),
    /// CONSTRUCT and DESCRIBE quads; `None` keys the default graph
    Quads(Vec<(Option<Term>, [Term; 3])>, TermTable<Option<Term>>),
}

struct State {
    rows: Rows,
    pos: usize,
}

/// A prepared query and, once executed, its result table. `cancel` works from any
/// thread while `execute` runs on another.
#[derive(uniffi::Object)]
pub struct FfiQuery {
    snap: Arc<Snapshot>,
    text: String,
    opts: QueryOptions,
    cancel: Arc<AtomicBool>,
    labels: Labels,
    term_cache: usize,
    state: Mutex<Option<State>>,
    executed: AtomicBool,
}

impl FfiQuery {
    pub fn prepare(
        snap: Arc<Snapshot>,
        text: String,
        opts: &QueryOpts,
        dataset: &sparkles::Dataset,
        labels: Labels,
        term_cache: usize,
    ) -> FfiResult<FfiQuery> {
        let cancel = Arc::new(AtomicBool::new(false));
        let include_inferred = opts.include_inferred;
        let opts = opts.to_options(&labels, cancel.clone())?;
        let mut applied = dataset.with_query_defaults(&opts).into_owned();
        if !include_inferred {
            applied.default_graph_extra.clear();
        }
        let opts = applied;
        Ok(FfiQuery {
            snap,
            text,
            opts,
            cancel,
            labels,
            term_cache,
            state: Mutex::new(None),
            executed: AtomicBool::new(false),
        })
    }

    fn batch(&self, st: &mut State, max: u32) -> Vec<u8> {
        let max = max.clamp(1, MAX_ROWS) as usize;
        self.labels.with(|_, label| match &mut st.rows {
            Rows::Select(r, table) => {
                let cols = r.table.cols.len();
                let n = r.table.len();
                let mut w = RowWriter::new(table, cols as u16);
                let mut row = vec![0u32; cols];
                while st.pos < n && (w.rows() as usize) < max && w.bytes() < MAX_BATCH_BYTES {
                    let i = st.pos;
                    for (c, cell) in row.iter_mut().enumerate() {
                        let id = r.table.cols[c][i];
                        *cell = if id.is_undef() {
                            0
                        } else {
                            w.cell(id.0, || r.term(id), label)
                        };
                    }
                    w.push_row(&row);
                    st.pos += 1;
                }
                w.finish()
            }
            Rows::Quads(quads, table) => {
                let mut w = RowWriter::new(table, 4);
                while st.pos < quads.len()
                    && (w.rows() as usize) < max
                    && w.bytes() < MAX_BATCH_BYTES
                {
                    let (g, [s, p, o]) = &quads[st.pos];
                    let gc = match g {
                        None => w.default_graph(None),
                        Some(g) => w.cell(Some(g.clone()), || Some(g.clone()), label),
                    };
                    let sc = w.cell(Some(s.clone()), || Some(s.clone()), label);
                    let pc = w.cell(Some(p.clone()), || Some(p.clone()), label);
                    let oc = w.cell(Some(o.clone()), || Some(o.clone()), label);
                    w.push_row(&[gc, sc, pc, oc]);
                    st.pos += 1;
                }
                w.finish()
            }
        })
    }

    fn remaining(st: &State) -> bool {
        match &st.rows {
            Rows::Select(r, _) => st.pos < r.table.len(),
            Rows::Quads(q, _) => st.pos < q.len(),
        }
    }
}

#[uniffi::export]
impl FfiQuery {
    /// Run the query and return the first `first_rows` rows. The result table is freed
    /// in the same call when no rows remain.
    pub fn execute(&self, first_rows: u32) -> FfiResult<Execution> {
        if self.executed.swap(true, Ordering::SeqCst) {
            return Err(FfiError::new(
                ErrorKind::Invalid,
                "the query has been executed already",
            ));
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(sparkles::Error::Cancelled.into());
        }
        let r = sparkles::sparql::query(self.snap.clone(), &self.text, &self.opts)?;
        let t = &r.timing;
        let timing = Timing {
            parse_ms: t.parse_ms,
            plan_ms: t.plan_ms,
            exec_ms: t.exec_ms,
            total_ms: t.total_ms,
        };
        let kind = match r.kind {
            QueryKind::Select => FfiQueryKind::Select,
            QueryKind::Ask => FfiQueryKind::Ask,
            QueryKind::Construct => FfiQueryKind::Construct,
            QueryKind::Describe => FfiQueryKind::Describe,
        };
        let variables = r.vars.clone();
        let boolean = r.boolean;
        let rows = match r.kind {
            QueryKind::Ask => {
                return Ok(Execution {
                    kind,
                    variables,
                    boolean,
                    batch: encode::empty_batch(0),
                    done: true,
                    timing,
                });
            }
            QueryKind::Select => Rows::Select(Box::new(r), TermTable::new(self.term_cache)),
            QueryKind::Construct | QueryKind::Describe => {
                let mut q: Vec<(Option<Term>, [Term; 3])> = r
                    .triples
                    .into_iter()
                    .map(|t| (None, [t.subject.into(), t.predicate.into(), t.object]))
                    .collect();
                q.extend(r.quads.into_iter().map(|q| {
                    let g: Option<Term> = match q.graph_name {
                        oxrdf::GraphName::DefaultGraph => None,
                        oxrdf::GraphName::NamedNode(n) => Some(n.into()),
                        oxrdf::GraphName::BlankNode(b) => Some(b.into()),
                    };
                    (g, [q.subject.into(), q.predicate.into(), q.object])
                }));
                Rows::Quads(q, TermTable::new(self.term_cache))
            }
        };
        let mut st = State { rows, pos: 0 };
        let batch = self.batch(&mut st, first_rows);
        let done = !Self::remaining(&st);
        if !done {
            *self.state.lock() = Some(st);
        }
        Ok(Execution {
            kind,
            variables,
            boolean,
            batch,
            done,
            timing,
        })
    }

    /// The next batch of up to `max_rows` rows (fewer when the batch reaches 1 MiB).
    pub fn next_batch(&self, max_rows: u32) -> Batch {
        let mut guard = self.state.lock();
        let Some(st) = guard.as_mut() else {
            return Batch {
                batch: encode::empty_batch(0),
                done: true,
            };
        };
        let batch = self.batch(st, max_rows);
        let done = !Self::remaining(st);
        if done {
            *guard = None;
        }
        Batch { batch, done }
    }

    /// Ask a running `execute` to stop; it fails with `Cancelled`.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Free the result table.
    pub fn release(&self) {
        *self.state.lock() = None;
    }
}
