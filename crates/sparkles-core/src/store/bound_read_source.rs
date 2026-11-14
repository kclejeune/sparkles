//! Genuine physical input ownership. Checked query/read integration must retain
//! this source before preparing work and through each final physical output.
use super::BoundEvidenceView;
use crate::access::{GraphAccess, Preparation, PreparedView, Reservation};
use crate::history::{At, ReadLimit};
use crate::{Error, Result};
use std::sync::{Arc, Weak};

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
            Source::Prepared(_) => Ok(()),
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
