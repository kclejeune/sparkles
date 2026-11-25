//! `SERVICE view:read`: the rows of a `table` view (spec F13 §2.2).
//!
//! ```sparql
//! PREFIX view: <urn:x-sparkles:view#>
//! SELECT ?team ?members WHERE {
//!   SERVICE view:read { [] view:name "team-sizes" ; view:bind ?team, ?members . }
//! }
//! ```
//!
//! `view:bind` lists variables, each a column of the view by name. The rows come with
//! their multiplicities. `view:commit ?c` binds the commit the rows reflect, and
//! `view:allowStale true` reads the latest state without waiting for the view to reach
//! the query's commit. The views themselves come from the query options
//! ([`QueryOptions::views`](super::QueryOptions::views)).

use super::ctx::Ctx;
use super::plan::{Kind, Node, Planner};
use super::table::{Table, VarId};
use crate::access::GraphAccess;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::incr::inference::Inference;
use crate::incr::terms::ViewTerms;
use oxrdf::Term;
use rustc_hash::FxHashMap;
use spargebra::Query;
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// The view vocabulary.
pub const NS: &str = "urn:x-sparkles:view#";
/// The IRI of the service.
pub const READ: &str = "urn:x-sparkles:view#read";

pub fn is_read(name: &NamedNodePattern) -> bool {
    matches!(name, NamedNodePattern::NamedNode(n) if n.as_str() == READ)
}

/// Lazy supplier for a reading query's upstream memory parent, invoked only when
/// fallback evaluation (`read_back` / `evaluate_under`) actually runs.
pub type MemoryParentFn<'a> = &'a dyn Fn() -> Result<sparkles_budget::ParentHandle>;

/// A read of a view.
pub struct ViewRead<'a> {
    pub name: &'a str,
    /// the commit of the query's snapshot
    pub at: u64,
    /// the query's snapshot, to evaluate the view at when it has moved past `at` and
    /// its history does not reach back
    pub snap: Option<&'a Arc<crate::store::Snapshot>>,
    pub allow_stale: bool,
    /// the reader's view of the dataset (`None`: every graph)
    pub caller: Option<&'a Arc<GraphAccess>>,
    pub deadline: Option<Instant>,
    /// the reader's reading of errors in aggregates (`None`: the view's). Under another
    /// reading than the view's, the view's query is evaluated at the reader's snapshot
    /// and the view's state is neither read nor changed.
    pub aggregate_errors: Option<super::AggregateErrors>,
    /// lazy upstream memory parent for fallback evaluation (`read_back` /
    /// `evaluate_under`), binding any temporary circuit to the reading query's budget
    /// and pool without promoting `Ctx` on the common ready-cache hit path
    pub memory_parent: Option<MemoryParentFn<'a>>,
    /// query pool backing `memory_parent`, when the reading query runs under a pool
    pub memory_pool: Option<&'a Arc<super::pool::QueryPool>>,
    /// local memory ceiling for fallback evaluation (`None`: inherit the view's limit)
    pub max_memory_bytes: Option<u64>,
}

/// The rows of a view.
pub struct ViewRows {
    pub columns: Vec<String>,
    /// each row's terms in the order of `columns`, with its multiplicity
    pub rows: Vec<(Vec<Option<Term>>, i64)>,
    /// the commit the rows reflect
    pub commit: u64,
    /// the reading of errors in aggregates the rows were evaluated under, when it is
    /// the reader's and not the view's, so that the rows come from the view's query
    /// and not its state
    pub evaluated_under: Option<super::AggregateErrors>,
    /// optional reservation covering `rows` from fallback evaluation (`read_back` /
    /// `evaluate_under`), declared last so `rows` drops strictly before credit returns
    pub lease: Option<sparkles_budget::MemoryLease>,
}

/// A single charge shared by every version that uses the same allocation.
#[derive(Debug)]
pub(crate) struct PublishedBytes {
    total: Arc<AtomicU64>,
    bytes: u64,
}

impl PublishedBytes {
    pub(crate) fn new(total: &Arc<AtomicU64>, bytes: u64) -> Self {
        total.fetch_add(bytes, Ordering::Relaxed);
        Self {
            total: total.clone(),
            bytes,
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for PublishedBytes {
    fn drop(&mut self) {
        self.total.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// Flat row ids and multiplicities of one immutable view version.
#[derive(Debug, Default)]
pub struct ViewTable {
    pub width: usize,
    pub ids: Vec<Id>,
    pub weights: Vec<i64>,
    /// Sum of positive weights across all rows.
    pub total_rows: u64,
    pub(crate) published: Option<PublishedBytes>,
}

impl ViewTable {
    pub fn len(&self) -> usize {
        self.weights.len()
    }

    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }

    pub fn row(&self, index: usize) -> (&[Id], i64) {
        let start = index * self.width;
        (&self.ids[start..start + self.width], self.weights[index])
    }

    pub fn retained_bytes(&self) -> u64 {
        (std::mem::size_of::<Self>()
            + self.ids.capacity() * std::mem::size_of::<Id>()
            + self.weights.capacity() * std::mem::size_of::<i64>()) as u64
    }
}

/// An immutable, shared result of a view at one commit, with its frozen term table and
/// snapshot so streaming cursors read it batch by batch without holding the view's lock
/// or copying its rows.
#[derive(Clone)]
pub struct ViewVersion {
    pub columns: Arc<[String]>,
    pub commit: u64,
    pub snap: Arc<crate::store::Snapshot>,
    pub terms: Arc<ViewTerms>,
    pub table: Arc<ViewTable>,
    pub evaluated_under: Option<super::AggregateErrors>,
    pub(crate) published: Option<Arc<PublishedBytes>>,
    pub(crate) published_columns: Option<Arc<PublishedBytes>>,
}

impl std::fmt::Debug for ViewVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewVersion")
            .field("columns", &self.columns)
            .field("commit", &self.commit)
            .field("rows", &self.table.len())
            .field("evaluated_under", &self.evaluated_under)
            .finish()
    }
}

impl ViewVersion {
    pub(crate) fn published_bytes(&self) -> u64 {
        self.published.as_deref().map_or(0, PublishedBytes::bytes)
            + self
                .published_columns
                .as_deref()
                .map_or(0, PublishedBytes::bytes)
            + self
                .table
                .published
                .as_ref()
                .map_or(0, PublishedBytes::bytes)
            + self.terms.published_bytes()
    }

    /// Decode all rows into owned RDF terms for callers of [`ViewSource::read_view`].
    pub fn to_view_rows(&self) -> ViewRows {
        let mut rows = Vec::with_capacity(self.table.len());
        for i in 0..self.table.len() {
            let (ids, w) = self.table.row(i);
            let terms = ids
                .iter()
                .map(|&id| self.terms.term(&self.snap, id))
                .collect();
            rows.push((terms, w));
        }
        ViewRows {
            columns: self.columns.to_vec(),
            rows,
            commit: self.commit,
            evaluated_under: self.evaluated_under,
            lease: None,
        }
    }

    /// Size of a published version, including Arc headers. Frozen term tables may
    /// share individual keys with other tables and the circuit.
    pub fn retained_bytes(&self) -> u64 {
        let published = self.published_bytes();
        if published != 0 {
            return published;
        }
        let cols = self.columns.len() * std::mem::size_of::<String>()
            + self.columns.iter().map(String::capacity).sum::<usize>();
        (std::mem::size_of::<Self>() + cols + 64) as u64
            + self.terms.retained_bytes()
            + self.table.retained_bytes()
    }
}

/// A shared view version at a newer commit paired with the admitted net changes since
/// `commit`, streamed without copying the version's rows.
pub struct RewoundView {
    pub version: Arc<ViewVersion>,
    pub commit: u64,
    /// Rows added since `commit` (positive net delta), subtracted while scanning `version`.
    pub added: FxHashMap<Vec<Option<Term>>, i64>,
    /// Rows removed since `commit` (stored with positive weights), emitted after `version`.
    pub removed: Vec<(Vec<Option<Term>>, i64)>,
}

impl RewoundView {
    /// Retained heap bytes of `added` and `removed` (the shared `version` is not owned by
    /// the query).
    pub fn retained_bytes(&self) -> u64 {
        self.added_retained_bytes()
            .saturating_add(self.removed_retained_bytes())
    }

    pub fn added_retained_bytes(&self) -> u64 {
        if self.added.capacity() == 0 {
            return 0;
        }
        let table = sparkles_budget::sizes::table_bytes(
            self.added.capacity(),
            std::mem::size_of::<(Vec<Option<Term>>, i64)>(),
        ) as u64;
        let rows: u64 = self.added.keys().map(row_retained_bytes).sum();
        table.saturating_add(rows)
    }

    pub fn removed_retained_bytes(&self) -> u64 {
        let slots = self.removed.capacity() * std::mem::size_of::<(Vec<Option<Term>>, i64)>();
        let rows: u64 = self
            .removed
            .iter()
            .map(|(r, _)| row_retained_bytes(r))
            .sum();
        (slots as u64).saturating_add(rows)
    }
}

/// Native REDUCED and VALUES occurrences in the saved view query. The keys
/// identify nodes in that query's allocation and are never dereferenced.
#[derive(Clone, Debug, Default)]
pub struct ViewPatterns {
    pub(crate) native: rustc_hash::FxHashSet<usize>,
}

impl ViewPatterns {
    pub(crate) fn contains(&self, pattern: &GraphPattern) -> bool {
        self.native
            .contains(&(std::ptr::from_ref(pattern) as usize))
    }

    fn allocation_bytes(count: usize) -> u64 {
        let cap = sparkles_budget::sizes::table_grown(0, 0, count);
        (sparkles_budget::sizes::table_bytes(cap, std::mem::size_of::<usize>()) as u64)
            .saturating_add(std::mem::size_of::<Self>() as u64 + 128)
    }

    /// Remap recorded occurrences from `source` onto a structurally identical heap-pinned
    /// clone `cloned`, charging the projected hash set through `ctx` before allocating it.
    pub(super) fn project_retained(
        &self,
        source: &GraphPattern,
        cloned: &GraphPattern,
        ctx: &super::ctx::Ctx,
    ) -> Result<Option<(Self, u64, Option<super::ctx::RetainedCharge>)>> {
        if self.native.is_empty() {
            return Ok(None);
        }
        let mut count = 0usize;
        walk_gp_pair(source, cloned, &mut |s, _| {
            ctx.check()?;
            if self.contains(s) {
                count = count.saturating_add(1);
            }
            Ok(())
        })?;
        if count == 0 {
            return Ok(None);
        }
        let bytes = Self::allocation_bytes(count);
        let charge = ctx.retained_charge(bytes)?;
        let mut native = rustc_hash::FxHashSet::default();
        native
            .try_reserve(count)
            .map_err(|_| ctx.memory_exceeded(bytes))?;
        walk_gp_pair(source, cloned, &mut |s, c| {
            ctx.check()?;
            if self.contains(s) {
                native.insert(std::ptr::from_ref(c) as usize);
            }
            Ok(())
        })?;
        Ok(Some((Self { native }, bytes, charge)))
    }

    /// Checked variant of [`Self::project_retained`] that polls `preparation` and admits
    /// the projected set's storage before allocating it.
    pub(crate) fn project_checked(
        &self,
        source: &GraphPattern,
        cloned: &GraphPattern,
        preparation: &crate::access::Preparation,
    ) -> Result<Option<(Self, crate::access::Reservation)>> {
        if self.native.is_empty() {
            return Ok(None);
        }
        let mut count = 0usize;
        walk_gp_pair(source, cloned, &mut |s, _| {
            preparation.check()?;
            if self.contains(s) {
                count = count.saturating_add(1);
            }
            Ok(())
        })?;
        if count == 0 {
            return Ok(None);
        }
        let bytes = Self::allocation_bytes(count);
        let held = preparation.reserve(bytes)?;
        let mut native = rustc_hash::FxHashSet::default();
        native
            .try_reserve(count)
            .map_err(|_| preparation.exhausted())?;
        walk_gp_pair(source, cloned, &mut |s, c| {
            preparation.check()?;
            if self.contains(s) {
                native.insert(std::ptr::from_ref(c) as usize);
            }
            Ok(())
        })?;
        Ok(Some((Self { native }, held)))
    }
}

fn walk_gp_pair(
    source: &GraphPattern,
    cloned: &GraphPattern,
    visit: &mut impl FnMut(&GraphPattern, &GraphPattern) -> Result<()>,
) -> Result<()> {
    use spargebra::algebra::{AggregateExpression as A, GraphPattern as G, OrderExpression as O};
    visit(source, cloned)?;
    match (source, cloned) {
        (G::Bgp { .. }, G::Bgp { .. })
        | (G::Path { .. }, G::Path { .. })
        | (G::Values { .. }, G::Values { .. }) => Ok(()),
        (
            G::Join {
                left: sl,
                right: sr,
            },
            G::Join {
                left: cl,
                right: cr,
            },
        )
        | (
            G::Lateral {
                left: sl,
                right: sr,
            },
            G::Lateral {
                left: cl,
                right: cr,
            },
        )
        | (
            G::Union {
                left: sl,
                right: sr,
            },
            G::Union {
                left: cl,
                right: cr,
            },
        )
        | (
            G::Minus {
                left: sl,
                right: sr,
            },
            G::Minus {
                left: cl,
                right: cr,
            },
        )
        | (
            G::SemiJoin {
                left: sl,
                right: sr,
            },
            G::SemiJoin {
                left: cl,
                right: cr,
            },
        )
        | (
            G::AntiJoin {
                left: sl,
                right: sr,
            },
            G::AntiJoin {
                left: cl,
                right: cr,
            },
        ) => {
            walk_gp_pair(sl, cl, visit)?;
            walk_gp_pair(sr, cr, visit)
        }
        (
            G::LeftJoin {
                left: sl,
                right: sr,
                expression: se,
            },
            G::LeftJoin {
                left: cl,
                right: cr,
                expression: ce,
            },
        ) => {
            walk_gp_pair(sl, cl, visit)?;
            walk_gp_pair(sr, cr, visit)?;
            if let (Some(se), Some(ce)) = (se, ce) {
                walk_expr_pair(se, ce, visit)?;
            }
            Ok(())
        }
        (
            G::Filter {
                expr: se,
                inner: si,
            },
            G::Filter {
                expr: ce,
                inner: ci,
            },
        ) => {
            walk_gp_pair(si, ci, visit)?;
            walk_expr_pair(se, ce, visit)
        }
        (G::Graph { inner: si, .. }, G::Graph { inner: ci, .. })
        | (G::Project { inner: si, .. }, G::Project { inner: ci, .. })
        | (G::Distinct { inner: si }, G::Distinct { inner: ci })
        | (G::Reduced { inner: si }, G::Reduced { inner: ci })
        | (G::Slice { inner: si, .. }, G::Slice { inner: ci, .. })
        | (G::Service { inner: si, .. }, G::Service { inner: ci, .. }) => {
            walk_gp_pair(si, ci, visit)
        }
        (
            G::Extend {
                inner: si,
                expression: se,
                ..
            },
            G::Extend {
                inner: ci,
                expression: ce,
                ..
            },
        )
        | (
            G::Assign {
                inner: si,
                expression: se,
                ..
            },
            G::Assign {
                inner: ci,
                expression: ce,
                ..
            },
        )
        | (
            G::Unfold {
                inner: si,
                expression: se,
                ..
            },
            G::Unfold {
                inner: ci,
                expression: ce,
                ..
            },
        ) => {
            walk_gp_pair(si, ci, visit)?;
            walk_expr_pair(se, ce, visit)
        }
        (
            G::OrderBy {
                inner: si,
                expression: se,
            },
            G::OrderBy {
                inner: ci,
                expression: ce,
            },
        ) => {
            walk_gp_pair(si, ci, visit)?;
            for (so, co) in se.iter().zip(ce) {
                let (O::Asc(se) | O::Desc(se), O::Asc(ce) | O::Desc(ce)) = (so, co);
                walk_expr_pair(se, ce, visit)?;
            }
            Ok(())
        }
        (
            G::Group {
                inner: si,
                aggregates: sa,
                ..
            },
            G::Group {
                inner: ci,
                aggregates: ca,
                ..
            },
        ) => {
            walk_gp_pair(si, ci, visit)?;
            for ((_, sa), (_, ca)) in sa.iter().zip(ca) {
                match (sa, ca) {
                    (A::CountSolutions { .. }, A::CountSolutions { .. }) => {}
                    (A::FunctionCall { expr: se, .. }, A::FunctionCall { expr: ce, .. }) => {
                        walk_expr_pair(se, ce, visit)?;
                    }
                    (
                        A::Fold {
                            expr: se,
                            value: sv,
                            order: so,
                            ..
                        },
                        A::Fold {
                            expr: ce,
                            value: cv,
                            order: co,
                            ..
                        },
                    ) => {
                        walk_expr_pair(se, ce, visit)?;
                        if let (Some(sv), Some(cv)) = (sv, cv) {
                            walk_expr_pair(sv, cv, visit)?;
                        }
                        for (so, co) in so.iter().zip(co) {
                            let (O::Asc(se) | O::Desc(se), O::Asc(ce) | O::Desc(ce)) = (so, co);
                            walk_expr_pair(se, ce, visit)?;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn walk_expr_pair(
    source: &spargebra::algebra::Expression,
    cloned: &spargebra::algebra::Expression,
    visit: &mut impl FnMut(&GraphPattern, &GraphPattern) -> Result<()>,
) -> Result<()> {
    use spargebra::algebra::Expression as E;
    match (source, cloned) {
        (E::Exists(sp), E::Exists(cp)) => walk_gp_pair(sp, cp, visit),
        (E::Or(sa, sb), E::Or(ca, cb))
        | (E::And(sa, sb), E::And(ca, cb))
        | (E::Equal(sa, sb), E::Equal(ca, cb))
        | (E::SameTerm(sa, sb), E::SameTerm(ca, cb))
        | (E::Greater(sa, sb), E::Greater(ca, cb))
        | (E::GreaterOrEqual(sa, sb), E::GreaterOrEqual(ca, cb))
        | (E::Less(sa, sb), E::Less(ca, cb))
        | (E::LessOrEqual(sa, sb), E::LessOrEqual(ca, cb))
        | (E::Add(sa, sb), E::Add(ca, cb))
        | (E::Subtract(sa, sb), E::Subtract(ca, cb))
        | (E::Multiply(sa, sb), E::Multiply(ca, cb))
        | (E::Divide(sa, sb), E::Divide(ca, cb)) => {
            walk_expr_pair(sa, ca, visit)?;
            walk_expr_pair(sb, cb, visit)
        }
        (E::UnaryPlus(sa), E::UnaryPlus(ca))
        | (E::UnaryMinus(sa), E::UnaryMinus(ca))
        | (E::Not(sa), E::Not(ca)) => walk_expr_pair(sa, ca, visit),
        (E::In(sa, sl), E::In(ca, cl)) => {
            walk_expr_pair(sa, ca, visit)?;
            for (se, ce) in sl.iter().zip(cl) {
                walk_expr_pair(se, ce, visit)?;
            }
            Ok(())
        }
        (E::If(sa, sb, sc), E::If(ca, cb, cc)) => {
            walk_expr_pair(sa, ca, visit)?;
            walk_expr_pair(sb, cb, visit)?;
            walk_expr_pair(sc, cc, visit)
        }
        (E::Coalesce(sl), E::Coalesce(cl)) | (E::FunctionCall(_, sl), E::FunctionCall(_, cl)) => {
            for (se, ce) in sl.iter().zip(cl) {
                walk_expr_pair(se, ce, visit)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// A view that must be evaluated directly at the query's snapshot (expired
/// history or another `AggregateErrors` reading), streamed as a child cursor charged to
/// the caller's query memory budget.
pub struct ViewRebuild {
    pub columns: Arc<[String]>,
    pub query: Arc<Query>,
    pub snap: Arc<crate::store::Snapshot>,
    pub access: Option<Arc<GraphAccess>>,
    pub deadline: Option<Instant>,
    pub max_memory_bytes: Option<u64>,
    pub union_default_graph: bool,
    pub patterns: Option<Arc<ViewPatterns>>,
    /// The solution column order used by the maintained CONSTRUCT template hash.
    pub template_columns: Option<Arc<[String]>>,
    pub inference: Option<Arc<Inference>>,
    pub aggregate_errors: super::AggregateErrors,
    pub commit: u64,
    pub evaluated_under: Option<super::AggregateErrors>,
    /// The view runtime's shared [`QueryPool`], so an unpooled caller running a
    /// streaming fallback rebuild still charges the runtime's pool.
    pub fallback_pool: Option<Arc<super::pool::QueryPool>>,
}

impl ViewRebuild {
    pub(crate) fn snapshot_bytes(&self) -> u64 {
        if self.union_default_graph == self.snap.union_default_graph {
            0
        } else {
            (std::mem::size_of::<crate::store::Snapshot>() + 16) as u64
        }
    }
}

/// The opened state of a streaming view read, held by the cursor with no view lock.
pub enum OpenedView {
    /// An immutable shared version owned by the view's circuit (current or stale read).
    Shared(Arc<ViewVersion>),
    /// An unshared version built specifically for this query, whose bytes were admitted.
    Owned(Arc<ViewVersion>),
    /// A shared version rewound to an earlier commit using admitted history deltas.
    Rewound(RewoundView),
    /// A view evaluated directly at the query's snapshot as a child cursor.
    Rebuild(Box<ViewRebuild>),
    /// Predecoded rows admitted by a streaming source before allocation.
    Rows(ViewRows),
}

/// Parameters for opening a streaming view read.
pub struct ViewOpen<'a> {
    pub read: ViewRead<'a>,
}

fn term_heap_bytes(t: &Term) -> usize {
    match t {
        Term::NamedNode(n) => n.as_str().len(),
        Term::BlankNode(b) => b.as_str().len(),
        Term::Literal(l) => {
            let mut bytes = l.value().len();
            if let Some(lang) = l.language() {
                bytes += lang.len();
            } else if l.datatype() != oxrdf::vocab::xsd::STRING {
                bytes += l.datatype().as_str().len();
            }
            bytes
        }
        Term::Triple(t) => {
            let subj = match &t.subject {
                oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str().len(),
                oxrdf::NamedOrBlankNode::BlankNode(b) => b.as_str().len(),
            };
            std::mem::size_of::<oxrdf::Triple>()
                + subj
                + t.predicate.as_str().len()
                + term_heap_bytes(&t.object)
        }
    }
}

/// Retained heap bytes of one decoded view row: its backing `Option<Term>` slice plus
/// out-of-line heap bytes owned by its terms.
#[allow(clippy::ptr_arg, reason = "the capacity of the row is what it keeps")]
pub fn row_retained_bytes(row: &Vec<Option<Term>>) -> u64 {
    let slots = row.capacity() * std::mem::size_of::<Option<Term>>();
    let terms: usize = row.iter().flatten().map(term_heap_bytes).sum();
    (slots + terms) as u64
}

/// Retained heap bytes of a [`ViewRows`] payload.
pub fn view_rows_retained_bytes(rows: &ViewRows) -> u64 {
    let cols = rows.columns.capacity() * std::mem::size_of::<String>()
        + rows.columns.iter().map(String::capacity).sum::<usize>();
    let entries = rows.rows.capacity() * std::mem::size_of::<(Vec<Option<Term>>, i64)>();
    let cells: u64 = rows.rows.iter().map(|(r, _)| row_retained_bytes(r)).sum();
    (std::mem::size_of::<ViewRows>() + cols + entries) as u64 + cells
}

/// What answers `SERVICE view:read`: a dataset's views.
pub trait ViewSource: Send + Sync + std::fmt::Debug {
    fn read_view(&self, r: &ViewRead<'_>) -> Result<ViewRows>;

    /// Whether this source implements preadmitted streaming opens for cursors.
    fn streams_views(&self) -> bool {
        false
    }
    /// Open a view for streaming batch-by-batch reads without holding the view's lock
    /// or decoding its whole result upfront.
    ///
    /// `check` polls query cancellation, deadlines and memory failures while waiting,
    /// rewinding history or rebuilding. `admit` reserves query memory for any state
    /// allocated for this read before allocation.
    fn open_view(
        &self,
        _open: &ViewOpen<'_>,
        _check: &dyn Fn() -> Result<()>,
        _admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<OpenedView> {
        Err(Error::Unsupported(
            "this view source only supports eager reads".into(),
        ))
    }
    /// The reading of errors in aggregates that view `name` keeps, `None` when the
    /// source does not say or has no such view. Planning reads it to note, also under
    /// EXPLAIN, a read that evaluates the view's query instead of its state. A source
    /// answers by `deadline`, the query's, or not at all.
    fn view_reading(
        &self,
        _name: &str,
        _deadline: Option<std::time::Instant>,
    ) -> Option<super::AggregateErrors> {
        None
    }
}

/// The warning of a read of view `name` under the reader's reading `m`, which is not
/// the view's.
pub(super) fn evaluated(name: &str, m: super::AggregateErrors) -> super::ctx::PlanWarning {
    super::ctx::PlanWarning {
        code: "view-evaluated",
        message: format!(
            "the view {name:?} keeps another reading of aggregate errors, so its query is evaluated under {m}"
        ),
    }
}

#[derive(Clone, Debug)]
pub struct ViewSpec {
    pub name: String,
    /// each bound variable and the view column it reads
    pub bind: Vec<(String, VarId)>,
    pub commit: Option<VarId>,
    pub allow_stale: bool,
}

pub(super) fn bad(msg: impl Into<String>) -> Error {
    Error::invalid(format!("SERVICE view:read: {}", msg.into()))
}

/// Plan `SERVICE view:read { … }`.
pub(super) fn view_leaf(p: &Planner<'_>, inner: &GraphPattern) -> Result<Node> {
    let GraphPattern::Bgp { patterns } = inner else {
        return Err(bad("the block holds view: properties of one subject"));
    };
    let mut spec = ViewSpec {
        name: String::new(),
        bind: Vec::new(),
        commit: None,
        allow_stale: false,
    };
    let mut subject: Option<&TermPattern> = None;
    for t in patterns {
        if subject.is_some_and(|s| s != &t.subject) {
            return Err(bad("every property belongs to one subject"));
        }
        subject = Some(&t.subject);
        let NamedNodePattern::NamedNode(pred) = &t.predicate else {
            return Err(bad("a property cannot be a variable"));
        };
        let Some(local) = pred.as_str().strip_prefix(NS) else {
            return Err(bad(format!("unknown property {pred}")));
        };
        match local {
            "name" => match &t.object {
                TermPattern::Literal(l) => spec.name = l.value().to_string(),
                _ => return Err(bad("view:name takes a string")),
            },
            "bind" => match &t.object {
                TermPattern::Variable(v) => spec
                    .bind
                    .push((v.as_str().to_string(), p.ctx.var(v.as_str()))),
                _ => return Err(bad("view:bind takes variables")),
            },
            "commit" => match &t.object {
                TermPattern::Variable(v) => spec.commit = Some(p.ctx.var(v.as_str())),
                _ => return Err(bad("view:commit takes a variable")),
            },
            "allowStale" => match &t.object {
                TermPattern::Literal(l) => {
                    spec.allow_stale = l.value() == "true" || l.value() == "1"
                }
                _ => return Err(bad("view:allowStale takes a boolean")),
            },
            other => return Err(bad(format!("unknown property view:{other}"))),
        }
    }
    if spec.name.is_empty() {
        return Err(bad("view:name is required"));
    }
    let mut vars: Vec<VarId> = spec.bind.iter().map(|b| b.1).collect();
    if let Some(c) = spec.commit {
        vars.push(c);
    }
    vars.dedup();
    // a read under another reading than the view's evaluates the view's query
    let reading = p.ctx.aggregate_errors;
    if let Some(views) = &p.ctx.views
        && views
            .view_reading(&spec.name, p.ctx.current_deadline())
            .is_some_and(|m| m != reading)
    {
        p.ctx.warn(evaluated(&spec.name, reading));
    }
    let desc = format!("view {:?}", spec.name);
    let mut n = Node::leaf(Kind::ViewRead(Box::new(spec)), vars, 1000.0, desc);
    // a view's columns can be unbound
    n.certain.clear();
    Ok(n)
}

/// Read the view's rows into a table of `vars`.
pub(super) fn run(ctx: &Ctx, spec: &ViewSpec, vars: &[VarId]) -> Result<Table> {
    if ctx
        .read_source
        .as_ref()
        .is_some_and(crate::store::BoundReadSource::is_txn)
    {
        return Err(Error::Unsupported(
            "view providers are not supported on a scoped transaction view".into(),
        ));
    }
    let Some(source) = ctx.views.as_ref() else {
        return Err(crate::incr::error(
            "view-not-found",
            format!("no view {:?}: views are not available here", spec.name),
        ));
    };
    let get_parent = || ctx.memory_parent();
    let memory_pool = ctx.grant().map(|g| g.pool());
    let max_memory_bytes = (ctx.mem_limit != u64::MAX).then_some(ctx.mem_limit);
    let rows = source.read_view(&ViewRead {
        name: &spec.name,
        at: ctx.snap.commit,
        snap: Some(&ctx.snap),
        allow_stale: spec.allow_stale,
        caller: ctx.graphs.as_ref(),
        deadline: ctx.current_deadline(),
        aggregate_errors: Some(ctx.aggregate_errors),
        memory_parent: Some(&get_parent),
        memory_pool,
        max_memory_bytes,
    })?;
    if let Some(m) = rows.evaluated_under {
        ctx.warn(evaluated(&spec.name, m));
    }
    let cols: Vec<Option<usize>> = vars
        .iter()
        .map(|v| {
            spec.bind
                .iter()
                .find(|b| b.1 == *v)
                .map(|b| rows.columns.iter().position(|c| *c == b.0))
                .unwrap_or(None)
        })
        .collect();
    for (name, _) in &spec.bind {
        if !rows.columns.iter().any(|c| c == name) {
            return Err(bad(format!(
                "the view {:?} has no column ?{name} (its columns: {})",
                spec.name,
                rows.columns
                    .iter()
                    .map(|c| format!("?{c}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )));
        }
    }
    let commit = Id::from_i64(rows.commit as i64).unwrap_or(Id::UNDEF);
    let mut t = Table::new(vars.to_vec());
    for (row, n) in &rows.rows {
        ctx.check()?;
        let ids: Vec<Id> = vars
            .iter()
            .zip(&cols)
            .map(|(v, c)| match c {
                Some(i) => row[*i].as_ref().map_or(Id::UNDEF, |t| ctx.intern_term(t)),
                None if Some(*v) == spec.commit => commit,
                None => Id::UNDEF,
            })
            .collect();
        for _ in 0..(*n).max(0) {
            t.push_row(&ids);
        }
    }
    ctx.produced(t.len())?;
    Ok(t)
}
