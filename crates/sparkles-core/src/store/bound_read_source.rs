//! Genuine physical input ownership. Checked query/read integration must retain
//! this source before preparing work and through each final physical output.
use super::BoundEvidenceView;
use crate::access::{GraphAccess, Preparation, PreparedView, Reservation};
use crate::history::{At, ReadLimit};
use crate::{Error, Result};
use std::sync::{Arc, Weak};

// Admitted transaction-view owner retained inline inside PreparedTxnView's
// PreparedView. Never exposes a `'static` handle outside PreparedTxnView<'a>.
pub(crate) struct PreparedTxn {
    snapshot: Arc<super::Snapshot>,
    family: Weak<super::family::Family>,
    dataset: uuid::Uuid,
    logical_branch: uuid::Uuid,
    _held: Reservation,
}

impl PreparedTxn {
    pub(crate) fn new(
        snapshot: Arc<super::Snapshot>,
        store: &super::Store,
        held: Reservation,
    ) -> Self {
        Self {
            snapshot,
            family: Arc::downgrade(&store.family),
            dataset: store.owner_dataset_id(),
            logical_branch: store.branch_id(),
            _held: held,
        }
    }

    pub(crate) fn snapshot(&self) -> &Arc<super::Snapshot> {
        &self.snapshot
    }

    pub(crate) fn validate_instance(&self, store: &super::Store) -> Result<()> {
        if self.dataset != store.owner_dataset_id()
            || self.logical_branch != store.branch_id()
            || !self.family.ptr_eq(&Arc::downgrade(&store.family))
        {
            return Err(Error::invalid(
                "transaction read view belongs to another Store instance",
            ));
        }
        Ok(())
    }
}

/// A budgeted transaction read view coupled to the lifetime of its [`super::WriteTxn`].
///
/// Borrowing the writer prevents mutating, committing, calling
/// [`super::WriteTxn::discard_terms`], or rolling back a dry run while this view or any
/// [`ScopedQueryResult`] derived from it is alive.
///
/// ```compile_fail
/// use sparkles_core::access::Preparation;
/// use sparkles_core::store::Store;
/// use sparkles_core::task::Control;
/// fn outlive_discard(store: &Store) {
///     let mut txn = store.write();
///     let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
///     let view = txn.prepare_read_view(&prep).unwrap();
///     txn.discard_terms().unwrap();
///     let _ = view.len();
/// }
/// ```
///
/// ```compile_fail
/// use sparkles_core::access::Preparation;
/// use sparkles_core::store::Store;
/// use sparkles_core::task::Control;
/// fn outlive_drop(store: &Store) {
///     let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
///     let view = {
///         let txn = store.write();
///         txn.prepare_read_view(&prep).unwrap()
///     };
///     let _ = view.len();
/// }
/// ```
pub struct PreparedTxnView<'a> {
    store: &'a super::Store,
    view: PreparedView,
}

impl<'a> PreparedTxnView<'a> {
    pub(crate) fn new(store: &'a super::Store, view: PreparedView) -> Self {
        Self { store, view }
    }

    /// Execute a SPARQL query against this scoped transaction view under `opts`.
    /// The returned [`ScopedQueryResult`] stays bound to this view's lifetime so the
    /// transaction snapshot cannot survive vocabulary rollback.
    pub fn query<'q>(
        &'q self,
        q: &str,
        opts: &crate::sparql::QueryOptions,
    ) -> Result<ScopedQueryResult<'q>> {
        let result = crate::sparql::query_source(self.view.read_source(), self.store, q, opts)?;
        Ok(ScopedQueryResult {
            result,
            _view: std::marker::PhantomData,
        })
    }

    /// Whether the graph named by `term` (the default graph when `None`) is readable.
    pub fn readable(&self, term: Option<&oxrdf::Term>) -> bool {
        self.view.readable(term)
    }

    /// Whether the graph named by `iri` is readable under this prepared policy.
    pub fn allows_read_iri(&self, iri: &str) -> bool {
        self.view.allows_read_iri(iri)
    }

    /// Whether the graph named by `term` (the default graph when `None`) is writable.
    pub fn writable(&self, term: Option<&oxrdf::Term>) -> bool {
        self.view.writable(term)
    }

    /// Whether the graph named by `iri` is writable under this prepared policy.
    pub fn writable_iri(&self, iri: &str) -> bool {
        self.view.writable_iri(iri)
    }

    /// Whether reads see every graph without graph-level filtering.
    pub fn reads_all(&self) -> bool {
        self.view.reads_all()
    }

    /// Whether reads see every graph and every triple without a mask.
    pub fn reads_everything(&self) -> bool {
        self.view.reads_everything()
    }

    /// Whether this prepared policy hides triples from reads.
    pub fn hides_triples(&self) -> bool {
        self.view.hides_triples()
    }

    /// Commit sequence reflected by this transaction view.
    pub fn commit(&self) -> u64 {
        self.view.commit()
    }

    /// Number of quads in the underlying masked snapshot (before graph-level filtering).
    pub fn len(&self) -> u64 {
        self.view.len()
    }

    /// Whether the underlying masked snapshot has no quads.
    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    /// Whether the prepared snapshot carries a triple-level access mask.
    pub fn has_mask(&self) -> bool {
        self.view.has_mask()
    }

    /// Visit each quad visible under both this view's triple mask and graph read policy.
    pub fn for_each_visible_quad(
        &self,
        f: impl FnMut([crate::id::Id; 4]) -> Result<()>,
    ) -> Result<()> {
        self.view.for_each_visible_quad(f)
    }
}

/// A query result bound to the lifetime of a [`PreparedTxnView`].
///
/// Neither this result nor the transaction snapshot behind it can outlive the
/// borrowing [`super::WriteTxn`] or survive [`super::WriteTxn::discard_terms`] or
/// dry-run rollback.
///
/// ```compile_fail
/// use sparkles_core::access::Preparation;
/// use sparkles_core::sparql::QueryOptions;
/// use sparkles_core::store::Store;
/// use sparkles_core::task::Control;
/// fn result_outlives_discard(store: &Store) {
///     let mut txn = store.write();
///     let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
///     let view = txn.prepare_read_view(&prep).unwrap();
///     let res = view.query("SELECT * WHERE { ?s ?p ?o }", &QueryOptions::default()).unwrap();
///     drop(view);
///     txn.discard_terms().unwrap();
///     let _ = res.boolean();
/// }
/// ```
pub struct ScopedQueryResult<'a> {
    result: crate::sparql::QueryResult,
    _view: std::marker::PhantomData<&'a ()>,
}

impl ScopedQueryResult<'_> {
    pub fn kind(&self) -> crate::sparql::QueryKind {
        self.result.kind
    }

    pub fn boolean(&self) -> bool {
        self.result.boolean
    }

    pub fn timing(&self) -> &crate::sparql::Timing {
        &self.result.timing
    }

    pub fn triples(&self) -> &[oxrdf::Triple] {
        self.result.triples()
    }

    pub fn quads(&self) -> &[oxrdf::Quad] {
        self.result.quads()
    }

    /// Visit each decoded SELECT row while keeping the underlying query context
    /// scoped to the transaction view.
    pub fn for_each_row(
        &self,
        mut f: impl FnMut(&[Option<oxrdf::Term>]) -> Result<()>,
    ) -> Result<()> {
        let rows = self.result.rows()?;
        for row in rows.iter() {
            f(row.values())?;
        }
        Ok(())
    }

    /// Decode SELECT rows into owned RDF terms that retain no reference to the
    /// transaction snapshot or delta vocabulary.
    pub fn collect_rows(&self) -> Result<Vec<Vec<Option<oxrdf::Term>>>> {
        let rows = self.result.rows()?;
        Ok(rows.iter().map(|row| row.values().to_vec()).collect())
    }
}

// Only Store factories construct this inline owner. It has no Clone, public
// constructor or raw extractor. Payloads precede the actual selection loan.
pub(crate) struct PreparedRead {
    snapshot: Arc<super::Snapshot>,
    // The invocation's policy while the view binds it, then only its weak
    // identity. The weak reference keeps the Arc allocation, so its address
    // cannot be reused by a replacement policy while this view lives.
    policy: Option<Arc<GraphAccess>>,
    identity: Weak<GraphAccess>,
    family: Weak<super::family::Family>,
    dataset: uuid::Uuid,
    logical_branch: uuid::Uuid,
    _held: Reservation,
}

impl PreparedRead {
    pub(crate) fn snapshot(&self) -> &Arc<super::Snapshot> {
        &self.snapshot
    }

    pub(crate) fn policy(&self) -> &Arc<GraphAccess> {
        self.policy
            .as_ref()
            .expect("the original policy is retained until binding finishes")
    }

    pub(crate) fn into_identity(mut self) -> Self {
        self.policy = None;
        self
    }

    pub(crate) fn is_original_policy(&self, policy: &Arc<GraphAccess>) -> bool {
        std::ptr::eq(self.identity.as_ptr(), Arc::as_ptr(policy))
    }

    pub(crate) fn validate_instance(&self, store: &super::Store) -> Result<()> {
        if self.dataset != store.owner_dataset_id()
            || self.logical_branch != store.branch_id()
            || !self.family.ptr_eq(&Arc::downgrade(&store.family))
        {
            return Err(Error::invalid(
                "physical source belongs to another Store instance",
            ));
        }
        Ok(())
    }
}

impl super::Store {
    /// Prepare the current physical RDF state with this invocation's actual
    /// current policy and original admission. This creates no semantic evidence.
    /// Use `sparql::select_source_cursor` for strict invocation checks. Existing
    /// raw view getters and shared-header ownership remain separate migration
    /// obligations; this factory alone does not certify every public owner.
    pub fn prepare_read(
        &self,
        policy: Arc<GraphAccess>,
        preparation: &Preparation,
    ) -> Result<PreparedView> {
        self.prepare_read_at(&At::Head, policy, preparation)
    }

    /// Prepare an ordinary retained state, including legacy/unbound RDF history.
    /// A selector inherited from another branch needs the actual owning main
    /// Store through `prepare_read_with_branch`, as with ordinary history reads.
    pub fn prepare_read_at(
        &self,
        at: &At,
        policy: Arc<GraphAccess>,
        preparation: &Preparation,
    ) -> Result<PreparedView> {
        self.prepare_physical_read(self, at, policy, preparation, false)
    }

    /// Resolve a logical branch through its actual owning main Store. The
    /// retained physical ancestor never replaces the logical invocation origin.
    pub fn prepare_read_with_branch(
        &self,
        logical: &super::Store,
        at: &At,
        policy: Arc<GraphAccess>,
        preparation: &Preparation,
    ) -> Result<PreparedView> {
        if self.is_branch()
            || self.owner_dataset_id() != logical.owner_dataset_id()
            || !Arc::ptr_eq(&self.family, &logical.family)
        {
            return Err(Error::invalid(
                "physical selection requires the actual owning main Store",
            ));
        }
        self.prepare_physical_read(logical, at, policy, preparation, true)
    }

    fn prepare_physical_read(
        &self,
        logical: &super::Store,
        at: &At,
        policy: Arc<GraphAccess>,
        preparation: &Preparation,
        inherited: bool,
    ) -> Result<PreparedView> {
        preparation.check()?;
        // The estimate below is not admission. This atomic reservation must
        // succeed before any cold metadata/decoder/source allocation. Another
        // grant can take the free granules first, so a refused estimate retries
        // once with the room the grant already covers.
        let (allowance, mut held) = {
            let allowance = preparation.selection_room();
            match preparation.reserve(allowance) {
                Ok(held) => (allowance, held),
                Err(Error::BudgetExceeded(_)) => {
                    let covered = preparation.covered_room();
                    (covered, preparation.reserve(covered)?)
                }
                Err(error) => return Err(error),
            }
        };
        let snapshot = if inherited {
            let (snapshot, resolved, _) =
                self.snapshot_at_prepared(logical, at, preparation, allowance)?;
            drop(resolved);
            snapshot
        } else {
            let (snapshot, resolved) =
                logical.snapshot_at_inner(at, preparation, Some(allowance))?;
            drop(resolved);
            snapshot
        };
        if snapshot.mask.is_some() {
            return Err(Error::invalid(
                "strict physical selection requires original unmasked input",
            ));
        }
        let limit = ReadLimit {
            options: preparation,
            memory: Some(allowance),
            held: 0,
        };
        let retained = snapshot
            .retained_preparation_bytes()
            .checked_add(snapshot.generation.retained_bytes_limited(&limit)?)
            .and_then(|bytes| bytes.checked_add(crate::history::READ_METADATA_BYTES))
            .ok_or_else(|| preparation.exhausted())?;
        if retained > allowance {
            return Err(preparation.exhausted());
        }
        // Read temporaries are gone. Keep the actual selected snapshot and its
        // remaining credit coupled before any permission-mask construction.
        held.release_to(retained);
        let selected = PreparedRead {
            snapshot,
            identity: Arc::downgrade(&policy),
            policy: Some(policy),
            family: Arc::downgrade(&logical.family),
            dataset: logical.owner_dataset_id(),
            logical_branch: logical.branch_id(),
            _held: held,
        };
        crate::access::triples::controlled::prepare_selected(selected, preparation)
    }
}

/// An opaque shallow owner minted only by a genuine bound or prepared view.
/// Clones keep the actual original view backing and its credit alive. This
/// carrier grants no current access, and does not pay newly allocated outputs.
///
/// It exposes no raw snapshot or arbitrary-owner constructor.
///
/// ```compile_fail
/// use sparkles_core::store::{BoundReadSource, Snapshot};
/// fn escape(source: &BoundReadSource) -> Snapshot {
///     source.snapshot().clone()
/// }
/// ```
#[derive(Clone)]
pub struct BoundReadSource {
    source: Source,
}

#[derive(Clone)]
enum Source {
    Evidence(BoundEvidenceView),
    Prepared(PreparedView),
}

impl BoundReadSource {
    // Only checked construction may borrow these genuine physical inputs. The
    // query context retains this carrier before it clones or works on them.
    pub(crate) fn query_input(
        &self,
    ) -> (
        &std::sync::Arc<super::Snapshot>,
        Option<&std::sync::Arc<crate::access::GraphAccess>>,
        &crate::access::Preparation,
    ) {
        match &self.source {
            Source::Prepared(view) => {
                let (snapshot, access, preparation) = view.query_input();
                (snapshot, Some(access), preparation)
            }
            Source::Evidence(view) => {
                let (snapshot, preparation) = view.query_input();
                (snapshot, None, preparation)
            }
        }
    }

    pub(crate) fn validate_query_instance(&self, store: &super::Store) -> crate::Result<()> {
        match &self.source {
            Source::Evidence(view) => view.validate_instance(store),
            Source::Prepared(view) => view.validate_query_instance(store),
        }
    }

    pub(crate) fn validate_strict_use(
        &self,
        store: &super::Store,
        opts: &crate::sparql::QueryOptions,
    ) -> Result<()> {
        match &self.source {
            Source::Evidence(view) => view.validate_instance(store)?,
            Source::Prepared(view) => view.validate_instance(store)?,
        }
        let policy = opts.graphs.as_ref().ok_or_else(|| {
            Error::NotPermitted("strict source requires this invocation's current policy".into())
        })?;
        if let Source::Prepared(view) = &self.source
            && !view.is_original_policy(policy)
        {
            return Err(Error::NotPermitted(
                "strict source cannot replace its original current policy".into(),
            ));
        }
        // Validate complete controls, actual grant and absolute deadline before
        // query parsing can allocate or obscure a source/policy refusal.
        self.query_input().2.checked_for_query(opts)?;
        Ok(())
    }

    pub(crate) fn evidence(view: BoundEvidenceView) -> Self {
        Self {
            source: Source::Evidence(view),
        }
    }

    pub(crate) fn prepared(view: PreparedView) -> Self {
        Self {
            source: Source::Prepared(view),
        }
    }
}

impl std::fmt::Debug for BoundReadSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            Source::Evidence(view) => f
                .debug_tuple("BoundReadSource::Evidence")
                .field(view)
                .finish(),
            Source::Prepared(view) => {
                // Touch the genuine owner without exposing an extractor or claiming
                // authorization from the retained physical source.
                let _ = view.access();
                f.debug_tuple("BoundReadSource::Prepared").finish()
            }
        }
    }
}

#[cfg(test)]
mod tests;
