//! Authenticated claim proposals (spec C25 §5 and §7): the typed writer of source
//! records, evidence references, claim occurrences, evidence links and assessments.
//!
//! [`propose`] publishes one batch as one commit of the evidence owner, in this order.
//!
//! 1. The input is prepared, its records are minted and placed in the settings' source
//!    and proposal graphs, and the caller's update grants on those graphs are checked.
//! 2. The managed-write capability is taken. A later change of the managed-graph
//!    registry, such as a detach, refuses the commit.
//! 3. A repeated idempotency key returns the first operation's receipt when its payload
//!    is the same, and conflicts otherwise.
//! 4. The references to existing records and stored facts are resolved under the
//!    caller's read access, so a reference it cannot read answers as a missing one.
//! 5. The referenced stored facts of this dataset and of other datasets in `catalog`
//!    are held durably under a hold named by the operation, before the commit that
//!    refers to them.
//! 6. Under the branch writer the expected head, the idempotency key again and the
//!    identity of every record are checked, the commit is stamped, the server sets the
//!    receipt, the first availability, each record digest and the operation marker,
//!    and right before publication the caller's authority and every hold are checked
//!    in ascending dataset-UUID order.
//!
//! A failure after step 5 can leave holds in place. Reconciliation can release a hold
//! after its grace period once the operation is absent from its origin and cannot
//! recover at reopen. Each published proposal runs a bounded reconciliation on its
//! own dataset and on each dataset it held. A retry can reacquire the same holds.

use super::resolve::{
    MAX_REFERENCES, MEMORY_BYTES, ProposedPassage, ProposedStructured, Resolution, SelectionProbe,
    SelectionRequirements, unavailable,
};
use crate::access::GraphAccess;
use crate::branch::CommitRef;
use crate::catalog::ReconcileCatalogIdentities;
use crate::managed::{ManagedError, ManagedErrorKind, OwnerKind};
use crate::store::{HistoryBound, HistoryQuery, ListedBranches, PublishedWriter, Store};
use crate::task::Control;
use crate::{Catalog, Dataset, Error, Result};
use oxrdf::{NamedNode, Term};
use sparkles_core::access::{Preparation, Reservation};
use sparkles_core::evidence::write::{
    self, ExistingRecord, Plan, ProposalOptions, ProtectedGraph, Reads, Stamp,
};
use sparkles_core::evidence::{
    EvidenceInput, EvidenceRefInput, EvidenceRefTarget, JSON_LEXICAL_UTF8_V1,
    JSON_LEXICAL_UTF8_VERSION, PassageInput, RecordKind, RecordRef, RenditionContent,
    RenditionInput, ResolutionRequirement, Settings, Sha256Digest, StoredFactRef,
};
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, hash_map::Entry};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub use sparkles_core::evidence::write::{HeldCommit, operation_id, operation_iri};

/// The memory allowance of the writer's own reads of its head.
pub(super) const READS_BYTES: u64 = 16 << 20;

/// A check of the caller's authority that a host runs right before the commit
/// publishes, such as a server's check that the credential was not revoked.
pub trait AuthorityFence: Send + Sync {
    /// Called under the branch writer right before publication, on the thread that
    /// commits. `Ok` returns a guard that the same thread holds until the commit is
    /// published or refused, so that a change of authority it excludes is ordered
    /// after the commit. An `Err` refuses the commit, usually as `authority-changed`.
    /// It must not take a dataset writer.
    fn enter(&self) -> Result<Box<dyn std::any::Any>>;

    /// Check authority to change the foreign datasets' source holds. The returned
    /// guard stays alive through acquisition. The supplied names are pinned in the
    /// catalog during the check, so this callback must not take the catalog lock.
    /// Trusted Rust callers can use the default when they control every dataset.
    fn enter_source_holds(&self, _targets: &[&Dataset]) -> Result<Box<dyn std::any::Any>> {
        Ok(Box::new(()))
    }
}

thread_local! {
    /// The authority guards of the commits this thread is publishing.
    static FENCES: std::cell::RefCell<Vec<Box<dyn std::any::Any>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Releases the authority guards a commit kept in this thread's slot, also when the
/// commit unwinds, so that no guard outlives its commit.
pub(super) struct Release(usize);

impl Drop for Release {
    fn drop(&mut self) {
        let released = FENCES.try_with(|f| {
            f.try_borrow_mut()
                .map(|mut f| {
                    let at = self.0.min(f.len());
                    f.split_off(at)
                })
                .unwrap_or_default()
        });
        drop(released);
    }
}

/// The publish hook of a typed evidence commit: it runs `first` and then the caller's
/// authority fence on the committing thread, inside the commit, and keeps the guards
/// they return in this thread's slot until the returned [`Release`] is dropped once the
/// commit has published or been refused.
#[allow(clippy::type_complexity)]
pub(super) fn publish_hook(
    authority: Option<Arc<dyn AuthorityFence>>,
    first: impl Fn() -> Result<Option<Box<dyn std::any::Any>>> + Send + Sync + 'static,
) -> (Release, crate::guard::PublishHook) {
    let release = Release(FENCES.with(|f| f.borrow().len()));
    let hook = crate::guard::PublishHook::new(move || {
        if let Some(guard) = first()? {
            FENCES.with(|f| f.borrow_mut().push(guard));
        }
        if let Some(authority) = &authority {
            let guard = authority.enter()?;
            FENCES.with(|f| f.borrow_mut().push(guard));
        }
        Ok(())
    });
    (release, hook)
}

/// How [`propose`] publishes a batch.
#[derive(Clone)]
pub struct Options {
    /// The authenticated principal: the creation activity's agent and the importer
    /// whose identity scopes the minted record identities.
    pub principal: NamedNode,
    /// Makes a retry with the same payload return the first receipt. The operation IRI
    /// is derived from the dataset, the principal and the key.
    pub idempotency_key: Option<String>,
    /// Publish only while this commit is the branch head.
    pub if_head: Option<u64>,
    /// Check and preview the write without committing it or acquiring a hold.
    pub dry_run: bool,
    /// The source-record graph, needed when the settings name more than one.
    pub source_graph: Option<NamedNode>,
    /// The proposal graph, needed when the settings name more than one.
    pub proposal_graph: Option<NamedNode>,
    /// The identity graph of identity proposals, needed when the settings name more
    /// than one.
    pub identity_graph: Option<NamedNode>,
    /// The commit message.
    pub message: Option<Arc<str>>,
    /// Who makes the write, recorded with its changes in the change log.
    pub author: Option<Arc<str>>,
    /// What the caller may write on the branch, `None` for every graph. The written
    /// graphs must be writable, and the commit applies the view's protections.
    pub write: Option<Arc<GraphAccess>>,
    /// The caller's authority, checked right before publication.
    pub authority: Option<Arc<dyn AuthorityFence>>,
    /// Ordinary quads that publish in the same commit as the batch, such as the source
    /// graph an ingestion registers with its proposals. They must lie in named graphs
    /// that no owner of the managed-graph registry protects.
    pub ordinary: Option<OrdinaryChanges>,
}

/// Ordinary changes of a proposal's commit: quads deleted, then quads inserted, outside
/// every managed graph. An idempotent replay writes none of them, because the first
/// publication carried them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrdinaryChanges {
    pub delete: Vec<oxrdf::Quad>,
    pub insert: Vec<oxrdf::Quad>,
}

impl OrdinaryChanges {
    /// Whether there is nothing to change.
    pub fn is_empty(&self) -> bool {
        self.delete.is_empty() && self.insert.is_empty()
    }
}

impl Options {
    /// Options for `principal` with no key, precondition, graph choice or limits.
    pub fn new(principal: NamedNode) -> Options {
        Options {
            principal,
            idempotency_key: None,
            if_head: None,
            dry_run: false,
            source_graph: None,
            proposal_graph: None,
            identity_graph: None,
            message: None,
            author: None,
            write: None,
            authority: None,
            ordinary: None,
        }
    }
}

/// One record of a proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposedRecord {
    /// The batch alias.
    pub key: String,
    pub kind: RecordKind,
    pub iri: NamedNode,
    pub graph: NamedNode,
    /// The digest of the record's immutable content, its `ev:recordDigest`.
    pub digest: Sha256Digest,
    /// Whether an earlier operation had already published the record with equal
    /// content, so this one did not write it again.
    pub existing: bool,
}

/// The outcome of [`propose`].
#[derive(Debug)]
pub struct Proposed {
    /// The operation IRI, which is also every record's availability receipt.
    pub operation: NamedNode,
    /// Whether this call published a commit.
    pub committed: bool,
    /// Whether the idempotency key had already published the same payload, whose
    /// receipt this is. Nothing was written.
    pub replayed: bool,
    /// The publishing commit, the first one for a replay, or the head for a dry run.
    pub commit: u64,
    /// The branch the commit published on.
    pub branch: Uuid,
    /// The commit's time, the first availability of its source revisions, as an
    /// `xsd:dateTime` lexical form. `None` for a dry run.
    pub first_available_at: Option<String>,
    /// The source hold of the operation's referenced stored facts, across this dataset
    /// and any other datasets of the catalog.
    pub hold: Option<Uuid>,
    pub records: Vec<ProposedRecord>,
    /// A dry run's preview.
    pub preview: Option<Box<crate::preview::Preview>>,
}

pub(super) fn managed(kind: ManagedErrorKind, message: impl Into<String>) -> Error {
    ManagedError::error(kind, message)
}

/// The settings recorded in `main`, and their recorded bytes.
pub(super) fn settings_of(main: &Dataset) -> Result<(Settings, Arc<[u8]>)> {
    let Some(snapshot) = main.store().managed_snapshot()? else {
        return Err(managed(
            ManagedErrorKind::UnsupportedProfile,
            "the evidence profile is not enabled in this dataset",
        ));
    };
    if let Some(reason) = snapshot.unavailable {
        return Err(managed(ManagedErrorKind::Unavailable, reason));
    }
    let Some(bytes) = snapshot.settings.get(&OwnerKind::Evidence) else {
        return Err(managed(
            ManagedErrorKind::UnsupportedProfile,
            "the evidence profile is not enabled in this dataset",
        ));
    };
    Ok((super::recorded(bytes)?, bytes.clone()))
}

/// The record-role graphs of `settings`, other than accepted projections.
pub(super) fn record_graphs(settings: &Settings) -> Vec<NamedNode> {
    let mut out: Vec<NamedNode> = settings
        .source_records
        .iter()
        .chain(&settings.proposals)
        .chain(&settings.decisions)
        .chain(&settings.supports)
        .chain(&settings.identity)
        .filter_map(|g| NamedNode::new(g.as_str()).ok())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Prepared-input refusals that name a resolution outcome keep its stable code.
fn prepared_error(e: Error) -> Error {
    match e {
        Error::Invalid(m) => {
            for (prefix, kind) in [
                ("span-mismatch: ", ManagedErrorKind::SpanMismatch),
                (
                    "unanchored-evidence: ",
                    ManagedErrorKind::UnanchoredEvidence,
                ),
            ] {
                if let Some(rest) = m.strip_prefix(prefix) {
                    return managed(kind, rest.to_string());
                }
            }
            Error::Invalid(m)
        }
        Error::Unsupported(m) => managed(
            ManagedErrorKind::UnsupportedProfile,
            m.strip_prefix("unsupported-profile: ")
                .unwrap_or(&m)
                .to_string(),
        ),
        e => e,
    }
}

fn result(plan: &Plan, skip: &[bool], digests: &[Sha256Digest]) -> Vec<ProposedRecord> {
    plan.records
        .iter()
        .enumerate()
        .map(|(i, r)| ProposedRecord {
            key: r.key.clone(),
            kind: r.kind,
            iri: r.iri.clone(),
            graph: r.graph.clone(),
            digest: digests.get(i).copied().unwrap_or(r.digest),
            existing: skip.get(i).copied().unwrap_or(false),
        })
        .collect()
}

/// What a repeated key means: the first receipt, or a conflict.
fn replay(
    plan: &Plan,
    found: write::RecordedOperationDetails,
    reads: &mut Reads<'_>,
    records: &[ProtectedGraph],
    branch: Uuid,
    control: &Control,
) -> Result<Proposed> {
    let write::RecordedOperationDetails {
        operation: found,
        source_hold,
    } = found;
    if !found.authenticated {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "operation <{}> was recorded outside the evidence profile's protection",
                plan.operation_iri.as_str()
            ),
        ));
    }
    if found.payload_digest != Some(plan.payload_digest) {
        return Err(managed(
            ManagedErrorKind::IdempotencyConflict,
            "this idempotency key was used with a different payload",
        ));
    }
    // Every record of the replayed payload must still be present and authenticated in
    // its graph with the same digest (or its reproducible legacy blank-node digest for
    // records published before canonical blank-node digesting).
    // A record whose receipt is another operation was already published when the
    // first operation ran, which reported it as existing.
    let mut existing = Vec::with_capacity(plan.records.len());
    let mut digests = Vec::with_capacity(plan.records.len());
    for r in &plan.records {
        let recorded = match reads.record(&r.iri, records)? {
            ExistingRecord::Present {
                graph,
                digest: Some(recorded),
                authenticated: true,
            } if graph == r.graph && r.matches_recorded_digest(recorded, control)? => recorded,
            _ => {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "{}: <{}> from replayed operation <{}> is not authenticated",
                        r.key,
                        r.iri.as_str(),
                        plan.operation_iri.as_str()
                    ),
                ));
            }
        };
        digests.push(recorded);
        existing.push(
            reads
                .receipt_of(&r.iri, &r.graph)?
                .is_some_and(|op| op != plan.operation_iri),
        );
    }
    Ok(Proposed {
        operation: plan.operation_iri.clone(),
        committed: false,
        replayed: true,
        commit: found.commit.unwrap_or_default(),
        branch: found.branch.unwrap_or(branch),
        first_available_at: found.ended_at.map(|l| l.value().to_string()),
        hold: source_hold,
        records: result(plan, &existing, &digests),
        preview: None,
    })
}

/// Conservative byte charge for inserting a new entry into a `BTreeMap`/`BTreeSet`.
/// The first insertion allocates a full leaf node (up to 11 entries plus node metadata);
/// subsequent insertions cover the entry and amortized node-split overhead.
fn btree_insert_charge<K, V>(empty: bool) -> u64 {
    const BTREE_NODE_KEYS: u64 = 11;
    let entry = (std::mem::size_of::<K>() as u64)
        .saturating_add(std::mem::size_of::<V>() as u64)
        .saturating_add(16);
    if empty {
        BTREE_NODE_KEYS.saturating_mul(entry).saturating_add(128)
    } else {
        entry.saturating_mul(2).saturating_add(64)
    }
}

/// Vector whose heap capacity is admitted under a [`Preparation`] before growth and
/// released when dropped. Storage is declared before `held` so it drops first.
struct AdmittedVec<T> {
    values: Vec<T>,
    held: Reservation,
}

impl<T> AdmittedVec<T> {
    fn new(prep: &Preparation, admit: &impl Fn(Error) -> Error) -> Result<Self> {
        Ok(Self {
            values: Vec::new(),
            held: prep.reserve(0).map_err(admit)?,
        })
    }

    fn push(
        &mut self,
        prep: &Preparation,
        value: T,
        admit: &impl Fn(Error) -> Error,
    ) -> Result<()> {
        prep.check().map_err(admit)?;
        if self.values.len() == self.values.capacity() {
            let next_cap = self
                .values
                .capacity()
                .checked_mul(2)
                .map(|n| n.max(1))
                .ok_or_else(|| admit(prep.exhausted()))?;
            let bytes = next_cap
                .checked_mul(std::mem::size_of::<T>())
                .and_then(|n| u64::try_from(n).ok())
                .ok_or_else(|| admit(prep.exhausted()))?;
            // Admit the replacement buffer while `self.held` still covers the old one,
            // then drop the old buffer before releasing its reservation.
            let mut new_held = prep.reserve(bytes).map_err(admit)?;
            let mut next = Vec::new();
            next.try_reserve_exact(next_cap)
                .map_err(|_| admit(prep.exhausted()))?;
            if std::mem::size_of::<T>() != 0 && next.capacity() > next_cap {
                let extra = (next.capacity() - next_cap)
                    .checked_mul(std::mem::size_of::<T>())
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| admit(prep.exhausted()))?;
                new_held.grow_by(extra).map_err(admit)?;
            }
            for item in self.values.drain(..) {
                prep.check().map_err(admit)?;
                next.push(item);
            }
            let old = std::mem::replace(&mut self.values, next);
            drop(old);
            self.held = new_held;
        }
        self.values.push(value);
        Ok(())
    }

    fn as_slice(&self) -> &[T] {
        &self.values
    }

    fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.values
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Set whose node allocations are admitted before insertion and released when dropped.
/// Storage is declared before `held` so it drops first.
struct AdmittedSet<T> {
    values: BTreeSet<T>,
    held: Reservation,
}

impl<T: Ord> AdmittedSet<T> {
    fn new(prep: &Preparation, admit: &impl Fn(Error) -> Error) -> Result<Self> {
        Ok(Self {
            values: BTreeSet::new(),
            held: prep.reserve(0).map_err(admit)?,
        })
    }

    fn insert(&mut self, value: T, admit: &impl Fn(Error) -> Error) -> Result<bool> {
        if self.values.contains(&value) {
            return Ok(false);
        }
        self.held
            .grow_by(btree_insert_charge::<T, ()>(self.values.is_empty()))
            .map_err(admit)?;
        self.values.insert(value);
        Ok(true)
    }
}

/// Map whose node allocations are admitted before insertion and released when dropped.
/// Storage is declared before `held` so it drops first.
struct AdmittedMap<K, V> {
    values: BTreeMap<K, V>,
    held: Reservation,
}

impl<K: Ord, V> AdmittedMap<K, V> {
    fn new(prep: &Preparation, admit: &impl Fn(Error) -> Error) -> Result<Self> {
        Ok(Self {
            values: BTreeMap::new(),
            held: prep.reserve(0).map_err(admit)?,
        })
    }

    fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.values.get(key)
    }

    fn insert(&mut self, key: K, value: V, admit: &impl Fn(Error) -> Error) -> Result<Option<V>> {
        if let Some(existing) = self.values.get_mut(&key) {
            return Ok(Some(std::mem::replace(existing, value)));
        }
        self.held
            .grow_by(btree_insert_charge::<K, V>(self.values.is_empty()))
            .map_err(admit)?;
        Ok(self.values.insert(key, value))
    }

    fn get_or_insert_with(
        &mut self,
        key: K,
        make: impl FnOnce() -> Result<V>,
        admit: &impl Fn(Error) -> Error,
    ) -> Result<&mut V> {
        if self.values.contains_key(&key) {
            return Ok(self.values.get_mut(&key).expect("present key"));
        }
        self.held
            .grow_by(btree_insert_charge::<K, V>(self.values.is_empty()))
            .map_err(admit)?;
        let value = make()?;
        Ok(self.values.entry(key).or_insert(value))
    }
}

/// Verify every requirement of a proposal under one shared [`Resolution`] (64 MiB
/// preparation allowance, request deadline and cancellation) through the caller's read
/// access before any source hold is acquired or commit is written.
#[allow(clippy::too_many_arguments)]
fn verify_requirements(
    catalog: Option<&Catalog>,
    main: &Dataset,
    logical: &Dataset,
    input: &EvidenceInput,
    plan: &Plan,
    requirements: &[ResolutionRequirement],
    read: &mut dyn FnMut(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    control: &Control,
    if_head: Option<u64>,
) -> Result<()> {
    verify_requirements_with_memory(
        catalog,
        main,
        logical,
        input,
        plan,
        requirements,
        read,
        control,
        MEMORY_BYTES,
        if_head,
    )
}

enum GenericReq<'a> {
    Record(&'a NamedNode, &'a [RecordKind], SelectionRequirements),
    Fact(&'a str, &'a StoredFactRef),
}

type PassageGroup<'a> = (usize, &'a NamedNode, AdmittedVec<ProposedPassage<'a>>);
type StructuredGroup<'a> = (usize, &'a NamedNode, AdmittedVec<ProposedStructured<'a>>);

#[allow(clippy::too_many_arguments)]
fn check_deferred_literal_shapes(
    resolution: &mut Resolution,
    main: &Dataset,
    logical: &Dataset,
    access: Option<Arc<GraphAccess>>,
    passages: &mut [PassageGroup<'_>],
    structured: &mut [StructuredGroup<'_>],
    limit_req_idx: usize,
    control: &Control,
) -> Result<()> {
    passages.sort_unstable_by_key(|(req_idx, _, _)| *req_idx);
    structured.sort_unstable_by_key(|(req_idx, _, _)| *req_idx);
    let mut pi = 0usize;
    let mut si = 0usize;
    loop {
        control.check()?;
        let p_idx = passages.get(pi).map_or(usize::MAX, |(idx, _, _)| *idx);
        let s_idx = structured.get(si).map_or(usize::MAX, |(idx, _, _)| *idx);
        let e_idx = resolution
            .next_pending_passage_req_idx()
            .unwrap_or(usize::MAX);
        if p_idx >= limit_req_idx && s_idx >= limit_req_idx && e_idx >= limit_req_idx {
            break;
        }
        if p_idx <= s_idx && p_idx <= e_idx {
            let iri = passages[pi].1;
            pi += 1;
            resolution.verify_selection_literal_shape(
                iri.as_str(),
                main,
                logical,
                access.clone(),
                iri,
                SelectionRequirements {
                    rendition_content: true,
                    ..Default::default()
                },
            )?;
        } else if s_idx <= e_idx {
            let iri = structured[si].1;
            si += 1;
            resolution.verify_selection_literal_shape(
                iri.as_str(),
                main,
                logical,
                access.clone(),
                iri,
                SelectionRequirements {
                    structured_content: true,
                    ..Default::default()
                },
            )?;
        } else {
            resolution
                .verify_next_pending_passage(main, logical, access.clone())
                .result?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_requirements_with_memory(
    catalog: Option<&Catalog>,
    main: &Dataset,
    logical: &Dataset,
    input: &EvidenceInput,
    plan: &Plan,
    requirements: &[ResolutionRequirement],
    read: &mut dyn FnMut(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    control: &Control,
    memory_bytes: u64,
    if_head: Option<u64>,
) -> Result<()> {
    if requirements.is_empty() {
        return Ok(());
    }
    let admit = |e: Error| match e {
        Error::BudgetExceeded(_) => managed(
            ManagedErrorKind::IncompleteEvidence,
            "proposal requirements exceeded the resolution memory limit",
        ),
        e => e,
    };
    let mut resolution = Resolution::with_memory(control.clone(), memory_bytes).map_err(admit)?;
    if let Some(expected) = if_head {
        resolution.pin_commit(main, logical, expected);
    }
    let prep = resolution.preparation().clone();
    let mut generic_count = 0usize;
    let mut generic_reqs: AdmittedVec<GenericReq<'_>> = AdmittedVec::new(&prep, &admit)?;
    let mut passage_groups: AdmittedVec<PassageGroup<'_>> = AdmittedVec::new(&prep, &admit)?;
    let mut passage_refs: AdmittedVec<(&str, &NamedNode, &NamedNode, &NamedNode)> =
        AdmittedVec::new(&prep, &admit)?;
    let mut structured_groups: AdmittedVec<StructuredGroup<'_>> = AdmittedVec::new(&prep, &admit)?;
    {
        // Build O(log N) lookup and grouping maps once upfront, charged under the
        // shared preparation allowance, and drop them before Phase 1 runs.
        let mut records_by_key: AdmittedMap<&str, &NamedNode> = AdmittedMap::new(&prep, &admit)?;
        for rec in &plan.records {
            control.check()?;
            records_by_key.insert(rec.key.as_str(), &rec.iri, &admit)?;
        }
        let mut renditions_by_key: AdmittedMap<&str, &RenditionInput> =
            AdmittedMap::new(&prep, &admit)?;
        for r in &input.renditions {
            control.check()?;
            renditions_by_key.insert(r.key.as_str(), r, &admit)?;
        }
        let mut passages_by_key: AdmittedMap<&str, &PassageInput> =
            AdmittedMap::new(&prep, &admit)?;
        for p in &input.passages {
            control.check()?;
            passages_by_key.insert(p.key.as_str(), p, &admit)?;
        }
        let mut references_by_key: AdmittedMap<&str, &EvidenceRefInput> =
            AdmittedMap::new(&prep, &admit)?;
        for r in &input.references {
            control.check()?;
            references_by_key.insert(r.key.as_str(), r, &admit)?;
        }
        fn resolve_ref<'a>(
            records: &AdmittedMap<&str, &'a NamedNode>,
            r: &'a RecordRef,
        ) -> Option<&'a NamedNode> {
            match r {
                RecordRef::Local(k) => records.get(k.as_str()).copied(),
                RecordRef::Existing(iri) => Some(iri),
            }
        }
        let resolve = |r| resolve_ref(&records_by_key, r);
        let mut seen_records: AdmittedSet<(&NamedNode, u32)> = AdmittedSet::new(&prep, &admit)?;
        let mut passage_group_idx: AdmittedMap<&NamedNode, usize> =
            AdmittedMap::new(&prep, &admit)?;
        let mut queued_passages: AdmittedSet<&str> = AdmittedSet::new(&prep, &admit)?;
        let mut structured_group_idx: AdmittedMap<&NamedNode, usize> =
            AdmittedMap::new(&prep, &admit)?;
        let mut selection_needs: AdmittedMap<&NamedNode, SelectionRequirements> =
            AdmittedMap::new(&prep, &admit)?;
        for requirement in requirements {
            control.check()?;
            match requirement {
                ResolutionRequirement::ExistingRecord {
                    record,
                    expected_kinds,
                } => {
                    generic_count += 1;
                    if generic_count > MAX_REFERENCES {
                        return Err(Error::invalid(format!(
                            "a resolution names at most {MAX_REFERENCES} references"
                        )));
                    }
                    let kinds_mask = expected_kinds.iter().fold(0u32, |mask, kind| {
                        let bit = match kind {
                            RecordKind::SourceRevision => 0,
                            RecordKind::Rendition => 1,
                            RecordKind::Passage => 2,
                            RecordKind::EvidenceRef => 3,
                            RecordKind::Claim => 4,
                            RecordKind::EvidenceLink => 5,
                            RecordKind::Assessment => 6,
                            RecordKind::Note => 7,
                            RecordKind::Decision => 8,
                            RecordKind::Task => 9,
                            RecordKind::ReviewDecision => 10,
                            RecordKind::IdentityProposal => 11,
                            RecordKind::AcceptanceSupport => 12,
                            RecordKind::Withdrawal => 13,
                            RecordKind::Projection => 14,
                            RecordKind::ResolutionMap => 15,
                        };
                        mask | (1u32 << bit)
                    });
                    // Deduplicate identical `(record, expected_kinds)` probes while
                    // preserving every distinct kind constraint on the same IRI.
                    if seen_records.insert((record, kinds_mask), &admit)? {
                        generic_reqs.push(
                            &prep,
                            GenericReq::Record(
                                record,
                                expected_kinds.as_slice(),
                                SelectionRequirements::default(),
                            ),
                            &admit,
                        )?;
                    }
                }
                ResolutionRequirement::StoredFact { reference_key } => {
                    generic_count += 1;
                    if generic_count > MAX_REFERENCES {
                        return Err(Error::invalid(format!(
                            "a resolution names at most {MAX_REFERENCES} references"
                        )));
                    }
                    let fact = references_by_key
                        .get(reference_key.as_str())
                        .and_then(|r| match &r.target {
                            EvidenceRefTarget::StoredFact(f) => Some(f),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            Error::invalid(format!("{reference_key}: no stored fact"))
                        })?;
                    generic_reqs.push(
                        &prep,
                        GenericReq::Fact(reference_key.as_str(), fact),
                        &admit,
                    )?;
                }
                ResolutionRequirement::PassageIdentity { passage_key } => {
                    let passage = passages_by_key
                        .get(passage_key.as_str())
                        .copied()
                        .ok_or_else(|| Error::invalid(format!("{passage_key}: no passage")))?;
                    match &passage.rendition {
                        RecordRef::Local(rendition) => {
                            let rendition = renditions_by_key
                                .get(rendition.as_str())
                                .copied()
                                .ok_or_else(|| {
                                    Error::invalid(format!("{passage_key}: no rendition"))
                                })?;
                            if resolve(&rendition.revision) != resolve(&passage.revision) {
                                return Err(Error::invalid(format!(
                                    "{passage_key}: the passage and its rendition name different revisions"
                                )));
                            }
                        }
                        RecordRef::Existing(rendition) => {
                            let revision = resolve(&passage.revision).ok_or_else(|| {
                                Error::invalid(format!("{passage_key}: no revision"))
                            })?;
                            selection_needs
                                .get_or_insert_with(
                                    rendition,
                                    || Ok(SelectionRequirements::default()),
                                    &admit,
                                )?
                                .rendition_content = true;
                            if queued_passages.insert(passage.key.as_str(), &admit)? {
                                let idx = *passage_group_idx.get_or_insert_with(
                                    rendition,
                                    || {
                                        let i = passage_groups.len();
                                        let group = AdmittedVec::new(&prep, &admit)?;
                                        passage_groups.push(
                                            &prep,
                                            (usize::MAX, rendition, group),
                                            &admit,
                                        )?;
                                        Ok(i)
                                    },
                                    &admit,
                                )?;
                                passage_groups.as_mut_slice()[idx].2.push(
                                    &prep,
                                    ProposedPassage {
                                        key: passage.key.as_str(),
                                        revision,
                                        start: passage.start,
                                        end: passage.end,
                                        text_digest: passage.text_digest,
                                        quote: passage.quote.as_deref(),
                                    },
                                    &admit,
                                )?;
                            }
                        }
                    }
                }
                ResolutionRequirement::RetainedPassage { passage_key } => {
                    let passage = passages_by_key
                        .get(passage_key.as_str())
                        .copied()
                        .ok_or_else(|| Error::invalid(format!("{passage_key}: no passage")))?;
                    match &passage.rendition {
                        RecordRef::Local(k) => {
                            let local_text = renditions_by_key.get(k.as_str()).is_some_and(|r| {
                                matches!(r.content, RenditionContent::NormalizedText(_))
                            });
                            if !local_text {
                                return Err(unavailable(passage_key));
                            }
                        }
                        RecordRef::Existing(rendition) => {
                            let revision = resolve(&passage.revision).ok_or_else(|| {
                                Error::invalid(format!("{passage_key}: no revision"))
                            })?;
                            selection_needs
                                .get_or_insert_with(
                                    rendition,
                                    || Ok(SelectionRequirements::default()),
                                    &admit,
                                )?
                                .rendition_content = true;
                            if queued_passages.insert(passage.key.as_str(), &admit)? {
                                let idx = *passage_group_idx.get_or_insert_with(
                                    rendition,
                                    || {
                                        let i = passage_groups.len();
                                        let group = AdmittedVec::new(&prep, &admit)?;
                                        passage_groups.push(
                                            &prep,
                                            (usize::MAX, rendition, group),
                                            &admit,
                                        )?;
                                        Ok(i)
                                    },
                                    &admit,
                                )?;
                                passage_groups.as_mut_slice()[idx].2.push(
                                    &prep,
                                    ProposedPassage {
                                        key: passage.key.as_str(),
                                        revision,
                                        start: passage.start,
                                        end: passage.end,
                                        text_digest: passage.text_digest,
                                        quote: passage.quote.as_deref(),
                                    },
                                    &admit,
                                )?;
                            }
                        }
                    }
                }
                ResolutionRequirement::PassageReferenceIdentity { reference_key } => {
                    let reference = references_by_key
                        .get(reference_key.as_str())
                        .copied()
                        .ok_or_else(|| Error::invalid(format!("{reference_key}: no reference")))?;
                    let EvidenceRefTarget::Passage {
                        revision,
                        rendition,
                        passage,
                    } = &reference.target
                    else {
                        return Err(Error::invalid(format!("{reference_key}: no passage")));
                    };
                    match passage {
                        RecordRef::Local(p_key) => {
                            let local_passage = passages_by_key
                                .get(p_key.as_str())
                                .copied()
                                .ok_or_else(|| {
                                    Error::invalid(format!("{reference_key}: no passage"))
                                })?;
                            if resolve(revision) != resolve(&local_passage.revision)
                                || resolve(rendition) != resolve(&local_passage.rendition)
                            {
                                return Err(Error::invalid(format!(
                                    "{reference_key}: the reference names another revision or rendition than its passage"
                                )));
                            }
                        }
                        RecordRef::Existing(p_iri) => {
                            let expected_rev = resolve(revision).ok_or_else(|| {
                                Error::invalid(format!("{reference_key}: no revision"))
                            })?;
                            let expected_rend = resolve(rendition).ok_or_else(|| {
                                Error::invalid(format!("{reference_key}: no rendition"))
                            })?;
                            selection_needs
                                .get_or_insert_with(
                                    p_iri,
                                    || Ok(SelectionRequirements::default()),
                                    &admit,
                                )?
                                .passage_lineage = true;
                            passage_refs.push(
                                &prep,
                                (reference.key.as_str(), p_iri, expected_rev, expected_rend),
                                &admit,
                            )?;
                        }
                    }
                }
                ResolutionRequirement::StructuredValue { reference_key } => {
                    let reference = references_by_key
                        .get(reference_key.as_str())
                        .copied()
                        .ok_or_else(|| Error::invalid(format!("{reference_key}: no reference")))?;
                    let EvidenceRefTarget::Structured {
                        revision,
                        pointer,
                        method,
                        value_digest,
                    } = &reference.target
                    else {
                        return Err(Error::invalid(format!(
                            "{reference_key}: no structured reference"
                        )));
                    };
                    if method.method.as_str() != JSON_LEXICAL_UTF8_V1
                        || method.version.as_str() != JSON_LEXICAL_UTF8_VERSION
                    {
                        return Err(managed(
                            ManagedErrorKind::IncompleteEvidence,
                            format!(
                                "{reference_key}: structured normalization method <{}> version `{}` is not supported by this build, so nothing was written",
                                method.method.as_str(),
                                method.version.as_str()
                            ),
                        ));
                    }
                    match revision {
                        RecordRef::Local(_) => {
                            // A local retained revision was already verified by `prepare_input`;
                            // a local metadata-only revision carries no retained bytes.
                            return Err(unavailable(reference_key));
                        }
                        RecordRef::Existing(rev_iri) => {
                            selection_needs
                                .get_or_insert_with(
                                    rev_iri,
                                    || Ok(SelectionRequirements::default()),
                                    &admit,
                                )?
                                .structured_content = true;
                            let idx = *structured_group_idx.get_or_insert_with(
                                rev_iri,
                                || {
                                    let i = structured_groups.len();
                                    let group = AdmittedVec::new(&prep, &admit)?;
                                    structured_groups.push(
                                        &prep,
                                        (usize::MAX, rev_iri, group),
                                        &admit,
                                    )?;
                                    Ok(i)
                                },
                                &admit,
                            )?;
                            structured_groups.as_mut_slice()[idx].2.push(
                                &prep,
                                ProposedStructured {
                                    key: reference.key.as_str(),
                                    pointer: pointer.as_str(),
                                    value_digest: *value_digest,
                                },
                                &admit,
                            )?;
                        }
                    }
                }
            }
        }
        for (req_idx, req) in generic_reqs.as_mut_slice().iter_mut().enumerate() {
            control.check()?;
            if let GenericReq::Record(iri, _, selection) = req
                && let Some(needs) = selection_needs.get(iri)
            {
                *selection = *needs;
                if needs.rendition_content
                    && let Some(&g_idx) = passage_group_idx.get(iri)
                {
                    let first = &mut passage_groups.as_mut_slice()[g_idx].0;
                    if *first == usize::MAX {
                        *first = req_idx;
                    }
                }
                if needs.structured_content
                    && let Some(&g_idx) = structured_group_idx.get(iri)
                {
                    let first = &mut structured_groups.as_mut_slice()[g_idx].0;
                    if *first == usize::MAX {
                        *first = req_idx;
                    }
                }
            }
        }
    }
    // Phase 1: probe every ExistingRecord (with selection-target availability attached
    // without reconstructing compatible retained literals yet) and every StoredFact so
    // missing, graph-denied, metadata-only and predicate-masked targets share identical
    // precedence, then release `generic_reqs` before reading any retained literal.
    let mut record_access: Option<Option<Arc<GraphAccess>>> = None;
    let mut has_deferred_literal = false;
    let mut phase1_err: Option<(usize, Error)> = None;
    for (req_idx, req) in generic_reqs.as_slice().iter().enumerate() {
        control.check()?;
        let outcome = match req {
            GenericReq::Fact(key, fact) => {
                let res = match catalog {
                    Some(catalog) if fact.dataset != main.dataset_id() => {
                        resolution.stored_fact(key, catalog, fact, &mut *read)
                    }
                    _ => resolution.stored_fact_in(key, main, fact, &mut *read),
                };
                res.map(|_| SelectionProbe::Complete)
            }
            GenericReq::Record(iri, kinds, selection) => {
                let granted = match &record_access {
                    Some(access) => access.clone(),
                    None => {
                        let access = resolution.logical_access(main, logical, read)?;
                        record_access = Some(access.clone());
                        access
                    }
                };
                resolution.probe_record_with_selection_at(
                    iri.as_str(),
                    main,
                    logical,
                    granted,
                    iri,
                    kinds,
                    *selection,
                    req_idx,
                )
            }
        };
        match outcome {
            Ok(SelectionProbe::Complete) => {}
            Ok(SelectionProbe::DeferredLiteralShape) => has_deferred_literal = true,
            Err(e) => {
                if !has_deferred_literal {
                    return Err(e);
                }
                phase1_err = Some((req_idx, e));
                break;
            }
        }
    }
    drop(generic_reqs);
    if let Some((err_idx, err)) = phase1_err {
        let access = match record_access {
            Some(access) => access,
            None => resolution.logical_access(main, logical, read)?,
        };
        check_deferred_literal_shapes(
            &mut resolution,
            main,
            logical,
            access,
            passage_groups.as_mut_slice(),
            structured_groups.as_mut_slice(),
            err_idx,
            control,
        )?;
        return Err(err);
    }
    // Phase 2: read each retained literal once for content/identity verification,
    // dropping each phase's admitted buffer as soon as it finishes so its reservation
    // is released before the next.
    if !passage_groups.is_empty() {
        let access = match &record_access {
            Some(access) => access.clone(),
            None => {
                let access = resolution.logical_access(main, logical, read)?;
                record_access = Some(access.clone());
                access
            }
        };
        for idx in 0..passage_groups.len() {
            control.check()?;
            let (group_req_idx, rendition) = {
                let g = &passage_groups.as_slice()[idx];
                (g.0, g.1)
            };
            let outcome = resolution.verify_proposed_passages_checked(
                main,
                logical,
                access.clone(),
                rendition,
                passage_groups.as_slice()[idx].2.as_slice(),
                group_req_idx,
            );
            if let Err(e) = outcome.result {
                let limit = if let Some(failed_req_idx) = outcome.failed_req_idx {
                    Some(failed_req_idx)
                } else {
                    match e.code() {
                        "invalid" | "span-mismatch" => Some(usize::MAX),
                        "incomplete-evidence" | "immutable-record-conflict" => {
                            Some(if outcome.shape_confirmed {
                                usize::MAX
                            } else {
                                group_req_idx
                            })
                        }
                        "evidence-unavailable" => Some(group_req_idx),
                        _ => None,
                    }
                };
                if let Some(limit) = limit {
                    check_deferred_literal_shapes(
                        &mut resolution,
                        main,
                        logical,
                        access,
                        &mut passage_groups.as_mut_slice()[idx + 1..],
                        structured_groups.as_mut_slice(),
                        limit,
                        control,
                    )?;
                }
                return Err(e);
            }
        }
    }
    drop(passage_groups);
    if resolution.next_pending_passage_req_idx().is_some() {
        let access = match &record_access {
            Some(access) => access.clone(),
            None => {
                let access = resolution.logical_access(main, logical, read)?;
                record_access = Some(access.clone());
                access
            }
        };
        while let Some(pending_req_idx) = resolution.next_pending_passage_req_idx() {
            control.check()?;
            let outcome = resolution.verify_next_pending_passage(main, logical, access.clone());
            if let Err(e) = outcome.result {
                let limit = match e.code() {
                    "span-mismatch" | "evidence-unavailable" | "immutable-record-conflict" => {
                        Some(pending_req_idx)
                    }
                    "incomplete-evidence" => Some(if outcome.shape_confirmed {
                        usize::MAX
                    } else {
                        pending_req_idx
                    }),
                    _ => None,
                };
                if let Some(limit) = limit {
                    check_deferred_literal_shapes(
                        &mut resolution,
                        main,
                        logical,
                        access,
                        &mut [],
                        structured_groups.as_mut_slice(),
                        limit,
                        control,
                    )?;
                }
                return Err(e);
            }
        }
    }
    if !passage_refs.is_empty() {
        let access = match &record_access {
            Some(access) => access.clone(),
            None => {
                let access = resolution.logical_access(main, logical, read)?;
                record_access = Some(access.clone());
                access
            }
        };
        let mut checked_passage_refs: AdmittedSet<(&NamedNode, &NamedNode, &NamedNode)> =
            match AdmittedSet::new(&prep, &admit) {
                Ok(set) => set,
                Err(e) => {
                    check_deferred_literal_shapes(
                        &mut resolution,
                        main,
                        logical,
                        access,
                        &mut [],
                        structured_groups.as_mut_slice(),
                        usize::MAX,
                        control,
                    )?;
                    return Err(e);
                }
            };
        for (ref_key, passage, expected_rev, expected_rend) in passage_refs.as_slice() {
            control.check()?;
            let inserted = match checked_passage_refs
                .insert((*passage, *expected_rev, *expected_rend), &admit)
            {
                Ok(inserted) => inserted,
                Err(e) => {
                    drop(checked_passage_refs);
                    check_deferred_literal_shapes(
                        &mut resolution,
                        main,
                        logical,
                        access,
                        &mut [],
                        structured_groups.as_mut_slice(),
                        usize::MAX,
                        control,
                    )?;
                    return Err(e);
                }
            };
            if inserted
                && let Err(e) = resolution.verify_passage_reference(
                    ref_key,
                    main,
                    logical,
                    access.clone(),
                    passage,
                    expected_rev,
                    expected_rend,
                )
            {
                if matches!(
                    e.code(),
                    "invalid"
                        | "span-mismatch"
                        | "immutable-record-conflict"
                        | "incomplete-evidence"
                ) {
                    drop(checked_passage_refs);
                    check_deferred_literal_shapes(
                        &mut resolution,
                        main,
                        logical,
                        access,
                        &mut [],
                        structured_groups.as_mut_slice(),
                        usize::MAX,
                        control,
                    )?;
                }
                return Err(e);
            }
        }
        drop(checked_passage_refs);
    }
    drop(passage_refs);
    if !structured_groups.is_empty() {
        let access = match record_access {
            Some(access) => access,
            None => resolution.logical_access(main, logical, read)?,
        };
        for idx in 0..structured_groups.len() {
            control.check()?;
            let (group_req_idx, revision) = {
                let g = &structured_groups.as_slice()[idx];
                (g.0, g.1)
            };
            let outcome = resolution.verify_structured_references_checked(
                main,
                logical,
                access.clone(),
                revision,
                structured_groups.as_slice()[idx].2.as_slice(),
            );
            if let Err(e) = outcome.result {
                let limit = match e.code() {
                    "span-mismatch" => Some(usize::MAX),
                    "immutable-record-conflict" | "incomplete-evidence" => {
                        Some(if outcome.shape_confirmed {
                            usize::MAX
                        } else {
                            group_req_idx
                        })
                    }
                    "evidence-unavailable" => Some(group_req_idx),
                    _ => None,
                };
                if let Some(limit) = limit {
                    check_deferred_literal_shapes(
                        &mut resolution,
                        main,
                        logical,
                        access,
                        &mut [],
                        &mut structured_groups.as_mut_slice()[idx + 1..],
                        limit,
                        control,
                    )?;
                }
                return Err(e);
            }
        }
    }
    drop(structured_groups);
    Ok(())
}

fn catalog_identity_changed() -> Error {
    managed(
        ManagedErrorKind::EvidenceUnavailable,
        "cross-dataset holds require unchanged, unambiguous dataset registrations in the proposal catalog",
    )
}

fn source_hold_error(error: Error, foreign: bool) -> Error {
    match error {
        Error::HistoryGone(_) => managed(
            ManagedErrorKind::EvidenceUnavailable,
            "a referenced commit is no longer retained",
        ),
        Error::Branch(ref b) if b.code == "no-such-branch" => managed(
            ManagedErrorKind::EvidenceUnavailable,
            "a referenced branch is no longer available",
        ),
        Error::Managed(ref e) if foreign => match e.kind {
            ManagedErrorKind::HoldsLimit => {
                managed(e.kind, "a referenced dataset cannot hold more commits")
            }
            ManagedErrorKind::HoldsUnavailable => {
                managed(e.kind, "a referenced dataset's holds are unavailable")
            }
            ManagedErrorKind::HoldConflict => managed(
                e.kind,
                "a referenced dataset's source hold was released or conflicts with this proposal",
            ),
            _ => error,
        },
        Error::Io(_) if foreign => managed(
            ManagedErrorKind::HoldsUnavailable,
            "a referenced dataset's holds could not be made durable",
        ),
        _ => error,
    }
}

/// Publish `input` as one commit of the evidence owner on `logical`, which is `main` or
/// one of its branches, in the order the [module](self) describes. `read` is asked what
/// the caller may read on a dataset and branch, by their current names, as
/// [`Resolution::all`] describes. Stored facts of this dataset and of other datasets of
/// `catalog` are verified and held under the operation's id. Cross-dataset holds
/// require the proposing dataset and the referenced datasets to be registered by
/// distinct identities in the same catalog and to have the same persistence.
///
/// Refusals: a malformed or inconsistent input is invalid, `unsupported-profile` when
/// the profile is not enabled or a written graph is detached, `head-changed` when
/// `if_head` is not the head, `idempotency-conflict` for a key used with another
/// payload, `immutable-record-conflict` for an identity whose recorded content differs
/// or that is not an authenticated record, the resolution outcomes for references,
/// `hold-conflict` for a hold of the operation that names other commits, and
/// `authority-changed` from the authority fence. A write the caller's grants refuse is
/// not permitted. A refusal publishes no proposal records. A failure after
/// acquisition may leave holds for reconciliation.
pub fn propose(
    catalog: Option<&Catalog>,
    main: &Dataset,
    logical: &Dataset,
    input: &EvidenceInput,
    options: &Options,
    read: &mut dyn FnMut(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<Proposed> {
    control.check()?;
    if main.store().is_branch() || !main.store().shares_family_with(logical.store()) {
        return Err(Error::invalid(
            "a proposal names the dataset's main store and one of its branches",
        ));
    }
    let (settings, settings_bytes) = settings_of(main)?;
    let prepared =
        sparkles_core::evidence::prepare_input(input, control).map_err(prepared_error)?;
    let plan = write::plan(
        &prepared,
        main.dataset_id(),
        &settings,
        &ProposalOptions {
            principal: &options.principal,
            idempotency_key: options.idempotency_key.as_deref(),
            source_graph: options.source_graph.as_ref(),
            proposal_graph: options.proposal_graph.as_ref(),
            identity_graph: options.identity_graph.as_ref(),
        },
        control,
    )
    .map_err(prepared_error)?;
    // Update grants on every written graph, decided from the grants alone.
    if let Some(access) = &options.write {
        for g in &plan.graphs {
            let term = Term::NamedNode(g.clone());
            if !access.writable(Some(&term)) {
                return Err(GraphAccess::refused(Some(&term)));
            }
        }
    }
    let store = logical.store();
    let branch = store.branch_id();
    // From here on a change of the registry refuses the commit. The plan must come from
    // the settings of the registry state the capability is valid for.
    let capability = store.managed_writer(OwnerKind::Evidence, plan.operation)?;
    if capability.settings().as_deref() != Some(&settings_bytes[..]) {
        return Err(Error::Conflict(
            "the evidence settings changed while the proposal was prepared; retry".into(),
        ));
    }
    let targets: Vec<ProtectedGraph> = plan
        .graphs
        .iter()
        .map(|g| (g.clone(), capability.protection(g.as_str())))
        .collect();
    if let Some((g, _)) = targets.iter().find(|(_, n)| n.is_none()) {
        return Err(managed(
            ManagedErrorKind::UnsupportedProfile,
            format!(
                "graph <{}> is not protected by the evidence profile; record the evidence settings again",
                g.as_str()
            ),
        ));
    }
    // Ordinary changes stay outside every managed graph, of any owner, so that the
    // evidence capability carries no write into a graph it does not type.
    let ordinary = options.ordinary.as_ref().filter(|o| !o.is_empty());
    if let Some(o) = ordinary {
        for q in o.delete.iter().chain(&o.insert) {
            let oxrdf::GraphName::NamedNode(g) = &q.graph_name else {
                return Err(Error::invalid(
                    "ordinary changes of a proposal lie in named graphs",
                ));
            };
            if capability.managed_by_any(g.as_str()) {
                return Err(managed(
                    ManagedErrorKind::Protected,
                    format!(
                        "graph <{}> is managed by a profile; a proposal's ordinary changes cannot write it",
                        g.as_str()
                    ),
                ));
            }
            if let Some(access) = &options.write {
                let term = Term::NamedNode(g.clone());
                if !access.writable(Some(&term)) {
                    return Err(GraphAccess::refused(Some(&term)));
                }
            }
        }
    }
    let records: Vec<ProtectedGraph> = record_graphs(&settings)
        .into_iter()
        .map(|g| {
            let n = capability.protection(g.as_str());
            (g, n)
        })
        .collect();
    let hold = (!plan.held_in().is_empty()).then_some(plan.operation);
    // A repeated key answers before anything is resolved, held or locked.
    if options.idempotency_key.is_some() {
        let head = store.snapshot();
        let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
        let mut reads = Reads::new(&preparation, &head);
        if let Some(found) = reads.operation_details(&plan.operation_iri, &records)? {
            return replay(&plan, found, &mut reads, &records, branch, control);
        }
    }
    // Retain the same dataset handles the resolver read. Reopening by UUID could
    // find a dataset restored in the meantime with different history at that commit.
    let mut resolved_datasets = HashMap::new();
    let requirements = prepared.requirements();
    let check_expected_head = || {
        if let Some(expected) = options.if_head {
            let live = store.snapshot().commit;
            if live != expected {
                return Err(managed(
                    ManagedErrorKind::HeadChanged,
                    format!("ifHead: the head of the branch is commit {live}, not {expected}"),
                ));
            }
        }
        Ok(())
    };
    if !requirements.is_empty() {
        check_expected_head()?;
    }
    let verified = verify_requirements(
        catalog,
        main,
        logical,
        input,
        &plan,
        requirements,
        &mut |ds, branch| {
            let granted = read(ds, branch)?;
            if ds.dataset_id() != main.dataset_id() {
                let resolved = resolved_datasets
                    .entry(ds.dataset_id())
                    .or_insert_with(|| ds.clone());
                if !std::ptr::eq(resolved.store(), ds.store()) {
                    return None;
                }
            }
            Some(granted)
        },
        control,
        options.if_head,
    );
    if let Err(e) = verified {
        if matches!(
            &e,
            Error::Managed(m) if matches!(
                m.kind,
                ManagedErrorKind::EvidenceUnavailable | ManagedErrorKind::OriginNotReproduced
            )
        ) {
            check_expected_head()?;
        }
        return Err(e);
    }
    main.store().failpoint("evidence-sources-resolved");
    let foreign: Vec<&Dataset> = resolved_datasets.values().collect();
    let (publication_catalog, source_identities) = if foreign.is_empty() {
        (None, None)
    } else {
        let catalog = catalog.ok_or_else(catalog_identity_changed)?;
        let identities = catalog
            .pin_dataset_identities(
                std::iter::once(main).chain(foreign.iter().copied()),
                control,
                false,
            )?
            .ok_or_else(catalog_identity_changed)?;
        if foreign
            .iter()
            .any(|ds| ds.store().is_persistent() != main.store().is_persistent())
        {
            return Err(managed(
                ManagedErrorKind::EvidenceUnavailable,
                "cross-dataset holds require all sources and the proposal dataset to have the same persistence",
            ));
        }
        (Some(catalog.clone()), Some(identities))
    };
    let source_authority = match &options.authority {
        Some(authority) if !foreign.is_empty() => Some(authority.enter_source_holds(&foreign)?),
        _ => None,
    };
    // The plan is sorted by dataset UUID, which also orders the publication guards.
    let mut held_targets = Vec::new();
    if let Some(id) = hold
        && !options.dry_run
    {
        for commits in plan.held_in().chunk_by(|a, b| a.dataset == b.dataset) {
            control.check()?;
            let dataset_id = commits[0].dataset;
            let (dataset, for_dataset) = if dataset_id == main.dataset_id() {
                (main.clone(), None)
            } else {
                let dataset = resolved_datasets
                    .remove(&dataset_id)
                    .ok_or_else(catalog_identity_changed)?;
                (dataset, Some(main.dataset_id()))
            };
            let commits = commits
                .iter()
                .map(|entry| CommitRef {
                    branch_id: entry.commit.branch,
                    seq: entry.commit.commit,
                })
                .collect::<Vec<_>>();
            dataset
                .store()
                .acquire_hold_for(OwnerKind::Evidence, id, &commits, for_dataset)
                .map_err(|e| source_hold_error(e, for_dataset.is_some()))?;
            if for_dataset.is_some() {
                dataset.store().failpoint("evidence-held-foreign");
            }
            held_targets.push((dataset, commits, for_dataset));
        }
    }
    drop(source_authority);
    drop(source_identities);
    if hold.is_some() && !options.dry_run {
        main.store().failpoint("evidence-held");
    }
    // The hook keeps both the catalog mapping and the holds stable until the
    // commit returns. It cannot wait for a catalog writer under the branch writer.
    let (release, before_publish) = {
        let held = held_targets.clone();
        let main = main.clone();
        let catalog = publication_catalog.clone();
        let control = control.clone();
        publish_hook(options.authority.clone(), move || {
            let Some(id) = hold.filter(|_| !held.is_empty()) else {
                return Ok(None);
            };
            let identities = if let Some(catalog) = &catalog {
                Some(
                    catalog
                        .pin_dataset_identities(
                            std::iter::once(&main).chain(held.iter().map(|(ds, _, _)| ds)),
                            &control,
                            true,
                        )?
                        .ok_or_else(catalog_identity_changed)?,
                )
            } else {
                None
            };
            let mut guards = Vec::with_capacity(held.len());
            for (target_ds, commits, for_dataset) in &held {
                guards.push(
                    target_ds
                        .store()
                        .hold_publication_for(OwnerKind::Evidence, id, commits, *for_dataset)
                        .map_err(|e| source_hold_error(e, for_dataset.is_some()))?,
                );
            }
            Ok(Some(Box::new((guards, identities))))
        })
    };
    let opts = crate::guard::WriteOptions {
        deadline: control.deadline,
        cancel: Some(control.cancel.flag()),
        message: options.message.clone(),
        author: options.author.clone(),
        graphs: options.write.clone(),
        dry_run: options.dry_run.then(crate::preview::DryRun::default),
        before_publish: Some(before_publish),
        ..Default::default()
    };
    let mut t = store.try_write_with(crate::commit::CommitKind::Transaction, opts)?;
    t.set_managed(capability)?;
    if let Some(expected) = options.if_head
        && t.base().commit != expected
    {
        return Err(managed(
            ManagedErrorKind::HeadChanged,
            format!(
                "ifHead: the head of the branch is commit {}, not {expected}",
                t.base().commit
            ),
        ));
    }
    let head = t.base().clone();
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    let mut reads = Reads::new(&preparation, &head);
    if options.idempotency_key.is_some()
        && let Some(found) = reads.operation_details(&plan.operation_iri, &records)?
    {
        return replay(&plan, found, &mut reads, &records, branch, control);
    }
    // An identity already published with equal content is not written again. Any other
    // content under the identity, or content that is not an authenticated record, is
    // an immutable-record conflict.
    let mut skip = Vec::with_capacity(plan.records.len());
    let mut digests = Vec::with_capacity(plan.records.len());
    for r in &plan.records {
        control.check()?;
        let existing = reads.record(&r.iri, &records)?;
        let (reused, recorded_digest) = match existing {
            ExistingRecord::Absent => (false, r.digest),
            ExistingRecord::Present {
                graph,
                digest: Some(recorded),
                authenticated: true,
            } if graph == r.graph && r.matches_recorded_digest(recorded, control)? => {
                (true, recorded)
            }
            _ => {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "{}: <{}> is already recorded with other content",
                        r.key,
                        r.iri.as_str()
                    ),
                ));
            }
        };
        skip.push(reused);
        digests.push(recorded_digest);
    }
    drop(reads);
    let (seq, time_ms) = t.managed_stamp()?;
    let quads = {
        let capability = t.managed().expect("capability set");
        plan.quads(
            &Stamp {
                dataset: main.dataset_id(),
                branch,
                commit: seq,
                time_ms,
            },
            |g| capability.protection(g.as_str()),
            hold,
            &skip,
        )?
    };
    let mut labels = std::collections::HashMap::new();
    for q in &quads {
        let ids = t.encode_quad(q, &mut labels)?;
        t.insert(ids)?;
    }
    if let Some(o) = ordinary {
        for q in &o.delete {
            control.check()?;
            let ids = t.encode_quad(q, &mut labels)?;
            t.delete(ids)?;
        }
        for q in &o.insert {
            control.check()?;
            let ids = t.encode_quad(q, &mut labels)?;
            t.insert(ids)?;
        }
    }
    main.store().failpoint("evidence-before-commit");
    let committed = t.commit();
    // The authority guard is released once the commit published or was refused.
    drop(release);
    let records = result(&plan, &skip, &digests);
    match committed {
        Err(Error::DryRun(preview)) => Ok(Proposed {
            operation: plan.operation_iri.clone(),
            committed: false,
            replayed: false,
            commit: head.commit,
            branch,
            first_available_at: None,
            hold: None,
            records,
            preview: Some(preview),
        }),
        Err(e) => Err(e),
        Ok(receipt) => {
            main.store().failpoint("evidence-committed");
            // All targets share the remaining request time, even after publication.
            let sweep_end = Instant::now() + Duration::from_secs(1);
            let cleanup = Control {
                deadline: Some(control.deadline.map_or(sweep_end, |d| d.min(sweep_end))),
                cancel: control.cancel.clone(),
                ..Control::default()
            };
            let targets = std::iter::once(main).chain(
                held_targets
                    .iter()
                    .filter_map(|(ds, _, origin)| origin.is_some().then_some(ds)),
            );
            for target in targets {
                if cleanup.check().is_err() {
                    break;
                }
                match reconcile_operation_holds_controlled(
                    catalog,
                    target,
                    crate::holds::DEFAULT_GRACE_MS,
                    &cleanup,
                    true,
                ) {
                    Ok(_) | Err(Error::WriterBusy) => {}
                    Err(Error::Timeout | Error::Cancelled) => break,
                    Err(e) => {
                        tracing::warn!(target: "sparkles::evidence", "orphaned evidence holds were not reconciled: {e}")
                    }
                }
            }
            Ok(Proposed {
                operation: plan.operation_iri.clone(),
                committed: receipt.committed,
                replayed: false,
                commit: receipt.commit.seq,
                branch,
                first_available_at: Some(write::date_time(time_ms).value().to_string()),
                hold,
                records,
                preview: None,
            })
        }
    }
}

/// Whether the operation `id` is recorded as an authenticated typed operation in the
/// evidence graphs of `main` or one of its branches, at their current heads.
pub fn operation_recorded(main: &Dataset, id: Uuid, control: &Control) -> Result<bool> {
    let op = operation_iri(id);
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    let mut stores = vec![main.clone()];
    if main.store().branch_set().is_some() {
        for b in main.branches()? {
            stores.push(main.branch(&b.name)?);
        }
    }
    for ds in &stores {
        control.check()?;
        let head = ds.store().snapshot();
        if Reads::new(&preparation, &head).described_anywhere(&op)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The stores to scan in both passes of reconciliation, and the branch identities
/// whose membership must still match when the operation is found absent.
struct OperationStores {
    origin: Dataset,
    listed: Vec<Arc<Store>>,
    branches: Vec<Uuid>,
    upstreams: Vec<Arc<Store>>,
}

impl OperationStores {
    fn open(
        origin: Dataset,
        include_upstreams: bool,
        options: &crate::guard::WriteOptions,
    ) -> Result<Option<Self>> {
        let main = origin.store();
        let branches = match main.reconciliation_branch_ids(options) {
            Err(Error::WriterBusy) => return Ok(None),
            result => result?,
        };
        let mut listed = Vec::with_capacity(branches.len());
        for &id in &branches {
            let Ok(Some(store)) = main.reconciliation_branch_store(id, options) else {
                return Ok(None);
            };
            listed.push(store);
        }
        let mut upstreams = Vec::new();
        if include_upstreams {
            let mut found: HashSet<_> = branches
                .iter()
                .copied()
                .chain([origin.dataset_id()])
                .collect();
            let mut pending: Vec<_> = listed.iter().filter_map(|s| s.branch_from()).collect();
            while let Some(from) = pending.pop() {
                options.check()?;
                if found.insert(from.branch_id) {
                    let Ok(Some(store)) = main.reconciliation_branch_store(from.branch_id, options)
                    else {
                        return Ok(None);
                    };
                    pending.extend(store.branch_from());
                    upstreams.push(store);
                }
            }
        }
        Ok(Some(Self {
            origin,
            listed,
            branches,
            upstreams,
        }))
    }

    fn current_stores(&self) -> impl Iterator<Item = &Store> {
        std::iter::once(self.origin.store()).chain(self.listed.iter().map(Arc::as_ref))
    }

    fn all_stores(&self) -> impl Iterator<Item = &Store> {
        self.current_stores()
            .chain(self.upstreams.iter().map(Arc::as_ref))
    }

    fn current_holds(
        &self,
        preparation: &crate::access::Preparation,
        op: &NamedNode,
        target: Uuid,
        commits: &[CommitRef],
    ) -> Result<bool> {
        for store in self.current_stores() {
            preparation.check()?;
            let head = store.snapshot();
            if Reads::new(preparation, &head).describes_hold(op, target, commits)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A fork can inherit an operation from any retained commit, even after it
    /// disappeared from the current graphs. Limit background work conservatively.
    fn historical_holds(
        &self,
        op: &NamedNode,
        target: Uuid,
        commits: &[CommitRef],
        control: &Control,
        no_wait: bool,
    ) -> Result<bool> {
        let deadline = control
            .deadline
            .or_else(|| Some(Instant::now() + Duration::from_secs(1)));
        let scan_control = Control {
            deadline,
            ..control.clone()
        };
        let preparation =
            crate::access::Preparation::new(scan_control, READS_BYTES)?.with_no_wait(no_wait);
        let mut remaining = 256;
        let mut query = HistoryQuery {
            subjects: vec![op.clone().into()],
            predicates: write::operation_hold_predicates().into(),
            deadline,
            cancel: Some(control.cancel.flag()),
            ..Default::default()
        };
        for store in self.all_stores() {
            control.check()?;
            let ranges = match store.published_history_ranges(&preparation, READS_BYTES) {
                Ok(ranges) => ranges,
                Err(_) => {
                    control.check()?;
                    return Ok(true);
                }
            };
            for (first, last) in ranges {
                control.check()?;
                if preparation.check().is_err() {
                    return Ok(true);
                }
                if remaining == 0 {
                    return Ok(true);
                }
                // A complete log proves that only the range's first state and
                // subsequent changes to the receipt's predicates can matter.
                query.from = Some(HistoryBound::Commit(first.saturating_add(1)));
                query.to = Some(HistoryBound::Commit(last));
                query.limit = remaining;
                let changes = if first < last {
                    let result = if no_wait {
                        store.try_published_history_changes(&query, READS_BYTES)
                    } else {
                        store.published_history_changes(&query, READS_BYTES)
                    };
                    match result {
                        Ok(result)
                            if !result.truncated
                                && result.unrecorded.is_empty()
                                && result.to == last =>
                        {
                            Some(result)
                        }
                        Ok(_) => None,
                        Err(_) => {
                            control.check()?;
                            if preparation.check().is_err() {
                                return Ok(true);
                            }
                            None
                        }
                    }
                } else {
                    None
                };
                let mut states: Box<dyn Iterator<Item = u64>> = match changes.as_ref() {
                    Some(changes) => {
                        let mut commits: Vec<_> = changes
                            .changes
                            .iter()
                            .map(|change| change.commit.seq)
                            .collect();
                        commits.sort_unstable();
                        commits.dedup();
                        Box::new(std::iter::once(first).chain(commits))
                    }
                    None => Box::new(first..=last),
                };
                for seq in states.by_ref().take(remaining) {
                    remaining -= 1;
                    match store.snapshot_at_published(seq, &preparation, READS_BYTES) {
                        Ok(snap) => {
                            store.failpoint("evidence-reconcile-historical-read");
                            match Reads::new(&preparation, &snap)
                                .describes_hold(op, target, commits)
                            {
                                Ok(true) => return Ok(true),
                                Ok(false) => {}
                                Err(_) => {
                                    control.check()?;
                                    return Ok(true);
                                }
                            }
                        }
                        // Without the first state, a complete log still cannot
                        // establish which operation descriptions were inherited.
                        Err(_) => {
                            control.check()?;
                            return Ok(true);
                        }
                    }
                }
                if states.next().is_some() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn keep_if_inconclusive(result: Result<bool>, no_wait: bool) -> Result<bool> {
    match result {
        Err(error) if no_wait && !matches!(error, Error::Cancelled | Error::Timeout) => Ok(true),
        result => result,
    }
}

#[derive(Default)]
struct ReconcileGuards<'a> {
    catalog: Option<ReconcileCatalogIdentities<'a>>,
    origins: HashMap<Uuid, OriginGuards>,
}

struct OriginGuards {
    _branches: ListedBranches,
    _writers: Vec<PublishedWriter>,
}

/// Release the local evidence holds of `main` acquired at least `grace_ms` milliseconds
/// ago whose operation never published, as after a failure or crash between a hold and
/// its commit. Cross-dataset holds (whose `for_dataset` names another dataset) are kept
/// because no catalog is supplied to verify their originating dataset; use
/// [`reconcile_operation_holds_in`] to reconcile those across a catalog.
pub fn reconcile_operation_holds(main: &Dataset, grace_ms: i64) -> Result<Vec<Uuid>> {
    reconcile_operation_holds_in(None, main, grace_ms)
}

/// Release evidence holds of `main` acquired at least `grace_ms` milliseconds ago
/// whose operation is absent from its originating dataset. A foreign hold is kept
/// while either dataset cannot be identified unambiguously among open and closed
/// registrations in `catalog`. The grace
/// period must exceed the longest request deadline. Returns the released ids.
///
/// The first pass reads the listed branches and checks retained history for foreign
/// holds, so a historical fork cannot revive a foreign dependency whose hold was
/// released. An inconclusive historical scan keeps the hold. The second pass pins
/// catalog identities, origin branch membership and persistent writers without
/// waiting. It checks current heads and keeps holds while an origin commit can
/// recover beyond the published head. Every publication renews its hold, so a
/// release selected before that publication leaves the hold in place.
pub fn reconcile_operation_holds_in(
    catalog: Option<&Catalog>,
    main: &Dataset,
    grace_ms: i64,
) -> Result<Vec<Uuid>> {
    reconcile_operation_holds_controlled(catalog, main, grace_ms, &Control::none(), false)
}

fn reconcile_operation_holds_controlled(
    catalog: Option<&Catalog>,
    main: &Dataset,
    grace_ms: i64,
    control: &Control,
    no_wait: bool,
) -> Result<Vec<Uuid>> {
    let options = crate::guard::WriteOptions {
        deadline: control.deadline,
        cancel: Some(control.cancel.flag()),
        no_wait,
        ..Default::default()
    };
    let mut origins = HashMap::new();
    main.store()
        .reconcile_holds_checked_with::<ReconcileGuards<'_>>(
            OwnerKind::Evidence,
            grace_ms,
            &options,
            |info, confirmation| {
                let hold = &info.hold;
                let origin_id = info.for_dataset.unwrap_or_else(|| main.dataset_id());
                let stores = match origins.entry(origin_id) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        let origin = if info.for_dataset.is_none() {
                            Some(main.clone())
                        } else {
                            match catalog {
                                Some(cat) => match cat.try_get_by_id(origin_id, &options) {
                                    Err(Error::WriterBusy) => None,
                                    result => result?,
                                },
                                None => None,
                            }
                        };
                        let stores = match origin {
                            Some(origin) => {
                                OperationStores::open(origin, info.for_dataset.is_some(), &options)?
                            }
                            None => None,
                        };
                        main.store().failpoint("evidence-reconcile-origin-opened");
                        entry.insert(stores)
                    }
                };
                control.check()?;
                let Some(stores) = stores else {
                    return Ok(true);
                };
                let origin = &stores.origin;
                let op = operation_iri(hold.id);
                let read_without_waiting = no_wait || confirmation.is_some();
                let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?
                    .with_no_wait(read_without_waiting);
                let current_holds = || {
                    keep_if_inconclusive(
                        stores.current_holds(&preparation, &op, main.dataset_id(), &hold.commits),
                        read_without_waiting,
                    )
                };
                if let Some(guards) = confirmation {
                    if info.for_dataset.is_some() {
                        if guards.catalog.is_none() {
                            guards.catalog = catalog.and_then(|cat| {
                                cat.try_pin_reconciliation(main, control).ok().flatten()
                            });
                        }
                        if !guards
                            .catalog
                            .as_ref()
                            .is_some_and(|pin| pin.matches(origin))
                        {
                            return Ok(true);
                        }
                    }
                    if let Entry::Vacant(slot) = guards.origins.entry(origin_id) {
                        let Some(branches) = origin.store().pin_listed_branch_ids(&stores.branches)
                        else {
                            return Ok(true);
                        };
                        // A failed sync may leave a sealed commit beyond a published head.
                        // No writer wait is permitted while confirming under a hold lock.
                        let mut writers = Vec::new();
                        if origin.store().is_persistent() {
                            for store in stores.all_stores() {
                                control.check()?;
                                let Some(writer) = store.try_pin_published_writer() else {
                                    return Ok(true);
                                };
                                writers.push(writer);
                            }
                        }
                        slot.insert(OriginGuards {
                            _branches: branches,
                            _writers: writers,
                        });
                    }
                    return current_holds();
                }
                if current_holds()? {
                    return Ok(true);
                }
                if info.for_dataset.is_some() {
                    if !catalog.is_some_and(|cat| {
                        cat.try_pin_reconciliation(main, control)
                            .is_ok_and(|pin| pin.is_some_and(|pin| pin.matches(origin)))
                    }) {
                        return Ok(true);
                    }
                    return keep_if_inconclusive(
                        stores.historical_holds(
                            &op,
                            main.dataset_id(),
                            &hold.commits,
                            control,
                            no_wait,
                        ),
                        no_wait,
                    );
                }
                Ok(false)
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles_core::evidence::{MethodRef, RecordKey, RecordVersion, prepare_input};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn iri(s: &str) -> NamedNode {
        NamedNode::new(s).unwrap()
    }

    fn key(s: &str) -> RecordKey {
        RecordKey::new(s).unwrap()
    }

    fn btree_total_charge<K, V>(n: usize) -> u64 {
        if n == 0 {
            return 0;
        }
        btree_insert_charge::<K, V>(true)
            .saturating_add(((n - 1) as u64).saturating_mul(btree_insert_charge::<K, V>(false)))
    }

    #[test]
    fn admitted_containers_charge_exact_singleton_capacity_before_insert_and_release_on_drop() {
        let admit = |e: Error| e;
        let prep = Preparation::new(Control::none(), 64 * 1024).unwrap();
        let baseline = prep.available_bytes();
        let context_bytes = 64 * 1024 - baseline;
        let digest = Sha256Digest::of_bytes(b"1", &Control::none()).unwrap();
        let slot = std::mem::size_of::<ProposedStructured<'_>>() as u64;

        // Singleton AdmittedVec allocates exact capacity 1 and charges its exact bytes.
        {
            let mut vec: AdmittedVec<ProposedStructured<'_>> =
                AdmittedVec::new(&prep, &admit).unwrap();
            vec.push(
                &prep,
                ProposedStructured {
                    key: "s0",
                    pointer: "/a",
                    value_digest: digest,
                },
                &admit,
            )
            .unwrap();
            assert_eq!(vec.values.capacity(), 1);
            assert_eq!(baseline - prep.available_bytes(), slot);
            // Second push grows capacity 1 -> 2 and settles to 2 slots after dropping old.
            vec.push(
                &prep,
                ProposedStructured {
                    key: "s1",
                    pointer: "/b",
                    value_digest: digest,
                },
                &admit,
            )
            .unwrap();
            assert_eq!(vec.values.capacity(), 2);
            assert_eq!(baseline - prep.available_bytes(), slot * 2);
        }
        assert_eq!(prep.available_bytes(), baseline);

        // Vector growth must admit old + new capacity (1 + 2 = 3 slots) simultaneously:
        // refuse when the new 2-slot buffer fits inside the allowance by itself (3*slot - 1)
        // but old + new exceeds it, and succeed with 1 extra byte (3*slot).
        {
            let tight_peak =
                Preparation::new(Control::none(), context_bytes + slot * 3 - 1).unwrap();
            let mut vec: AdmittedVec<ProposedStructured<'_>> =
                AdmittedVec::new(&tight_peak, &admit).unwrap();
            vec.push(
                &tight_peak,
                ProposedStructured {
                    key: "p0",
                    pointer: "/a",
                    value_digest: digest,
                },
                &admit,
            )
            .unwrap();
            assert_eq!(vec.values.capacity(), 1);
            let before_refusal = tight_peak.available_bytes();
            assert!(matches!(
                vec.push(
                    &tight_peak,
                    ProposedStructured {
                        key: "p1",
                        pointer: "/b",
                        value_digest: digest,
                    },
                    &admit,
                ),
                Err(Error::BudgetExceeded(_))
            ));
            assert_eq!(vec.len(), 1);
            assert_eq!(vec.values.capacity(), 1);
            assert_eq!(tight_peak.available_bytes(), before_refusal);
        }
        {
            let exact_peak = Preparation::new(Control::none(), context_bytes + slot * 3).unwrap();
            let exact_baseline = exact_peak.available_bytes();
            {
                let mut vec: AdmittedVec<ProposedStructured<'_>> =
                    AdmittedVec::new(&exact_peak, &admit).unwrap();
                vec.push(
                    &exact_peak,
                    ProposedStructured {
                        key: "p0",
                        pointer: "/a",
                        value_digest: digest,
                    },
                    &admit,
                )
                .unwrap();
                vec.push(
                    &exact_peak,
                    ProposedStructured {
                        key: "p1",
                        pointer: "/b",
                        value_digest: digest,
                    },
                    &admit,
                )
                .unwrap();
                assert_eq!(vec.len(), 2);
                assert_eq!(vec.values.capacity(), 2);
                assert_eq!(exact_baseline - exact_peak.available_bytes(), slot * 2);
            }
            assert_eq!(exact_peak.available_bytes(), exact_baseline);
        }

        // AdmittedSet and AdmittedMap refuse before inserting when budget is too small.
        let tiny = Preparation::new(Control::none(), context_bytes + 64).unwrap();
        let mut set: AdmittedSet<&str> = AdmittedSet::new(&tiny, &admit).unwrap();
        assert!(matches!(
            set.insert("first", &admit),
            Err(Error::BudgetExceeded(_))
        ));
        assert!(set.values.is_empty());

        let mut map: AdmittedMap<&str, usize> = AdmittedMap::new(&tiny, &admit).unwrap();
        assert!(matches!(
            map.insert("first", 0, &admit),
            Err(Error::BudgetExceeded(_))
        ));
        assert!(map.values.is_empty());
    }

    #[test]
    fn singleton_structured_groups_and_long_input_errors_respect_preparation_budget() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let principal = iri("urn:agent:budget");
        let digest = Sha256Digest::of_bytes(b"1", &Control::none()).unwrap();
        let settings = Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            ..Default::default()
        };
        let opts = ProposalOptions {
            principal: &principal,
            idempotency_key: None,
            source_graph: None,
            proposal_graph: None,
            identity_graph: None,
        };

        // 1. 64 singleton structured groups: compute the exact admitted classification
        //    cost (all 5 populated index/dedup maps + outer vectors + 64 inner singleton
        //    group vectors). Setting the allowance 1 byte below that total admits every
        //    index map, outer vector and the first 63 inner singleton groups, so the 64th
        //    inner singleton group allocation decides refusal (`incomplete-evidence` with
        //    zero store reads). Adding 1 byte completes classification and reaches Phase 1.
        const N: usize = 64;
        let refs: Vec<EvidenceRefInput> = (0..N)
            .map(|i| EvidenceRefInput {
                version: RecordVersion::V1,
                key: key(&format!("sr-{i}")),
                occurrence_key: key(&format!("sr-{i}")),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(iri(&format!("urn:rev:singleton-{i}"))),
                    pointer: "/x".into(),
                    method: MethodRef {
                        method: iri(JSON_LEXICAL_UTF8_V1),
                        version: key(JSON_LEXICAL_UTF8_VERSION),
                    },
                    value_digest: digest,
                },
            })
            .collect();
        let input = EvidenceInput {
            references: refs,
            ..Default::default()
        };
        let prepared = prepare_input(&input, &Control::none()).unwrap();
        let plan = write::plan(
            &prepared,
            ds.dataset_id(),
            &settings,
            &opts,
            &Control::none(),
        )
        .unwrap();
        let context_bytes = 64 * 1024
            - Preparation::new(Control::none(), 64 * 1024)
                .unwrap()
                .available_bytes();
        let index_and_outer_bytes = context_bytes
            + btree_total_charge::<&str, &NamedNode>(N)
            + btree_total_charge::<&str, &EvidenceRefInput>(N)
            + btree_total_charge::<(&NamedNode, u32), ()>(N)
            + (N as u64) * (std::mem::size_of::<GenericReq<'_>>() as u64)
            + btree_total_charge::<&NamedNode, SelectionRequirements>(N)
            + btree_total_charge::<&NamedNode, usize>(N)
            + (N as u64) * (std::mem::size_of::<StructuredGroup<'_>>() as u64);
        let inner_singleton_bytes =
            (N as u64) * (std::mem::size_of::<ProposedStructured<'_>>() as u64);
        let exact_classified_bytes = index_and_outer_bytes + inner_singleton_bytes;

        let reads = AtomicUsize::new(0);
        let err = verify_requirements_with_memory(
            None,
            &ds,
            &ds,
            &input,
            &plan,
            prepared.requirements(),
            &mut |_, _| {
                reads.fetch_add(1, Ordering::Relaxed);
                None
            },
            &Control::none(),
            exact_classified_bytes - 1,
            None,
        )
        .unwrap_err();
        assert_eq!(err.code(), "incomplete-evidence");
        assert_eq!(reads.load(Ordering::Relaxed), 0);

        // One more byte admits the 64th inner singleton group, releases the index maps,
        // and enters Phase 1 (invoking `read` once before refusing with unavailable).
        let exact_reads = AtomicUsize::new(0);
        let exact_err = verify_requirements_with_memory(
            None,
            &ds,
            &ds,
            &input,
            &plan,
            prepared.requirements(),
            &mut |_, _| {
                exact_reads.fetch_add(1, Ordering::Relaxed);
                None
            },
            &Control::none(),
            exact_classified_bytes,
            None,
        )
        .unwrap_err();
        assert_eq!(exact_err.code(), "evidence-unavailable");
        assert_eq!(exact_reads.load(Ordering::Relaxed), 1);

        // 2. Unsupported normalization method carrying a long method IRI fails
        //    immediately with the input-only error without buffering deferred errors
        //    or reading the store, even under a tight 4 KiB budget.
        let long_method = iri(&format!(
            "urn:example:unsupported-{}",
            "m".repeat(16 * 1024)
        ));
        let long_input = EvidenceInput {
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("long-unsup"),
                occurrence_key: key("long-unsup"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(iri("urn:rev:missing")),
                    pointer: "/x".into(),
                    method: MethodRef {
                        method: long_method,
                        version: key("v1"),
                    },
                    value_digest: digest,
                },
            }],
            ..Default::default()
        };
        let long_prepared = prepare_input(&long_input, &Control::none()).unwrap();
        let long_plan = write::plan(
            &long_prepared,
            ds.dataset_id(),
            &settings,
            &opts,
            &Control::none(),
        )
        .unwrap();
        let long_reads = AtomicUsize::new(0);
        let long_err = verify_requirements_with_memory(
            None,
            &ds,
            &ds,
            &long_input,
            &long_plan,
            long_prepared.requirements(),
            &mut |_, _| {
                long_reads.fetch_add(1, Ordering::Relaxed);
                Some(Arc::new(GraphAccess::all()))
            },
            &Control::none(),
            4 * 1024,
            None,
        )
        .unwrap_err();
        assert_eq!(long_err.code(), "incomplete-evidence");
        assert!(long_err.to_string().contains("not supported by this build"));
        assert_eq!(long_reads.load(Ordering::Relaxed), 0);
    }

    fn insert_managed_quads(ds: &Dataset, quads: Vec<oxrdf::Quad>) {
        let store = ds.store();
        let cap = store
            .managed_writer(OwnerKind::Evidence, Uuid::new_v4())
            .unwrap();
        let mut txn = store
            .try_write_with(
                sparkles_core::commit::CommitKind::Transaction,
                Default::default(),
            )
            .unwrap();
        txn.set_managed(cap).unwrap();
        let mut labels = std::collections::HashMap::new();
        for q in &quads {
            let ids = txn.encode_quad(q, &mut labels).unwrap();
            txn.insert(ids).unwrap();
        }
        txn.commit().unwrap();
    }

    fn configure_test_evidence(ds: &Dataset) -> (Settings, Uuid) {
        let settings = Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            ..Default::default()
        };
        crate::evidence::configure(ds, &settings, &Control::none()).unwrap();
        let capability = ds
            .store()
            .managed_writer(OwnerKind::Evidence, Uuid::new_v4())
            .unwrap();
        let nonce = capability
            .protection("urn:ev:sources")
            .expect("sources protection nonce");
        (settings, nonce)
    }

    fn run_verify(
        ds: &Dataset,
        settings: &Settings,
        input: &EvidenceInput,
        memory_bytes: u64,
    ) -> Result<()> {
        let principal = iri("urn:agent:r4-test");
        let opts = ProposalOptions {
            principal: &principal,
            idempotency_key: None,
            source_graph: None,
            proposal_graph: None,
            identity_graph: None,
        };
        let prepared = prepare_input(input, &Control::none()).unwrap();
        let plan = write::plan(
            &prepared,
            ds.dataset_id(),
            settings,
            &opts,
            &Control::none(),
        )
        .unwrap();
        verify_requirements_with_memory(
            None,
            ds,
            ds,
            input,
            &plan,
            prepared.requirements(),
            &mut |_, _| Some(Arc::new(GraphAccess::all())),
            &Control::none(),
            memory_bytes,
            None,
        )
    }

    #[test]
    fn proposal_single_read_for_incompatible_kind_fallback_and_mixed_passages() {
        use base64::Engine as _;
        use oxrdf::{Literal, Term};
        use sparkles_core::evidence::resolve::{
            kind_iri, literal_read_counts, reset_literal_read_counts,
        };

        const EV: &str = "urn:x-sparkles:evidence:";
        const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
        const PROV_DERIVED: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";
        const OA: &str = "http://www.w3.org/ns/oa#";
        const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
        const XSD_BASE64: &str = "http://www.w3.org/2001/XMLSchema#base64Binary";

        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let (settings, nonce) = configure_test_evidence(&ds);
        let sg = oxrdf::GraphName::NamedNode(iri("urn:ev:sources"));
        let q =
            |s: &NamedNode, p: &str, o: Term| oxrdf::Quad::new(s.clone(), iri(p), o, sg.clone());
        let lit = |v: &str| Term::Literal(Literal::new_simple_literal(v));
        let int_lit =
            |n: u64| Term::Literal(Literal::new_typed_literal(n.to_string(), iri(XSD_INTEGER)));

        let op = iri("urn:op:r4-reads");
        let rev_ok = iri("urn:rev:r4-ok");
        let rend_ok = iri("urn:rend:r4-ok");
        let rend_bad_shape = iri("urn:rend:r4-bad-shape");
        let rend_bad_shape_2 = iri("urn:rend:r4-bad-shape-2");
        let pass_ok_1 = iri("urn:pass:r4-ok-1");
        let sel_1 = iri("urn:sel:r4-ok-1");
        let pass_ok_2 = iri("urn:pass:r4-ok-2");
        let sel_2 = iri("urn:sel:r4-ok-2");

        let json = r#"{"score":42}"#;
        let json_digest = Sha256Digest::of_bytes(json.as_bytes(), &Control::none()).unwrap();
        let val_digest = Sha256Digest::of_bytes(b"42", &Control::none()).unwrap();
        let json_b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        let text = "single proposal read check";
        let d_single = Sha256Digest::of_bytes(b"single", &Control::none()).unwrap();
        let d_proposal = Sha256Digest::of_bytes(b"proposal", &Control::none()).unwrap();
        let d_read = Sha256Digest::of_bytes(b"read", &Control::none()).unwrap();

        insert_managed_quads(
            &ds,
            vec![
                q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
                q(
                    &op,
                    &format!("{EV}datasetId"),
                    lit(&ds.dataset_id().to_string()),
                ),
                q(
                    &op,
                    &format!("{EV}branchId"),
                    lit(&ds.store().branch_id().to_string()),
                ),
                q(&op, &format!("{EV}commit"), int_lit(1)),
                q(
                    &op,
                    &format!("{EV}graphProtection"),
                    lit(&nonce.to_string()),
                ),
                // Valid SourceRevision
                q(
                    &rev_ok,
                    RDF_TYPE,
                    iri(kind_iri(RecordKind::SourceRevision)).into(),
                ),
                q(
                    &rev_ok,
                    &format!("{EV}availabilityReceipt"),
                    op.clone().into(),
                ),
                q(
                    &rev_ok,
                    &format!("{EV}graphProtection"),
                    lit(&nonce.to_string()),
                ),
                q(
                    &rev_ok,
                    &format!("{EV}contentDigest"),
                    lit(&json_digest.to_string()),
                ),
                q(
                    &rev_ok,
                    &format!("{EV}retainedBytes"),
                    Term::Literal(Literal::new_typed_literal(json_b64, iri(XSD_BASE64))),
                ),
                // Valid Rendition
                q(
                    &rend_ok,
                    RDF_TYPE,
                    iri(kind_iri(RecordKind::Rendition)).into(),
                ),
                q(
                    &rend_ok,
                    &format!("{EV}availabilityReceipt"),
                    op.clone().into(),
                ),
                q(
                    &rend_ok,
                    &format!("{EV}graphProtection"),
                    lit(&nonce.to_string()),
                ),
                q(&rend_ok, PROV_DERIVED, rev_ok.clone().into()),
                q(&rend_ok, &format!("{EV}normalizedText"), lit(text)),
                // Wrong-shape Rendition (Vocab IRI under ev:normalizedText)
                q(
                    &rend_bad_shape,
                    RDF_TYPE,
                    iri(kind_iri(RecordKind::Rendition)).into(),
                ),
                q(
                    &rend_bad_shape,
                    &format!("{EV}availabilityReceipt"),
                    op.clone().into(),
                ),
                q(
                    &rend_bad_shape,
                    &format!("{EV}graphProtection"),
                    lit(&nonce.to_string()),
                ),
                q(&rend_bad_shape, PROV_DERIVED, rev_ok.clone().into()),
                q(
                    &rend_bad_shape,
                    &format!("{EV}normalizedText"),
                    iri("urn:not:simple-literal").into(),
                ),
                // Second wrong-shape Rendition (for ordering between compatible & incompatible probes)
                q(
                    &rend_bad_shape_2,
                    RDF_TYPE,
                    iri(kind_iri(RecordKind::Rendition)).into(),
                ),
                q(
                    &rend_bad_shape_2,
                    &format!("{EV}availabilityReceipt"),
                    op.clone().into(),
                ),
                q(
                    &rend_bad_shape_2,
                    &format!("{EV}graphProtection"),
                    lit(&nonce.to_string()),
                ),
                q(&rend_bad_shape_2, PROV_DERIVED, rev_ok.clone().into()),
                q(
                    &rend_bad_shape_2,
                    &format!("{EV}normalizedText"),
                    iri("urn:not:simple-literal-2").into(),
                ),
                // Existing passage 1 on rend_ok
                q(
                    &pass_ok_1,
                    RDF_TYPE,
                    iri(kind_iri(RecordKind::Passage)).into(),
                ),
                q(
                    &pass_ok_1,
                    &format!("{EV}availabilityReceipt"),
                    op.clone().into(),
                ),
                q(
                    &pass_ok_1,
                    &format!("{EV}graphProtection"),
                    lit(&nonce.to_string()),
                ),
                q(
                    &pass_ok_1,
                    &format!("{EV}sourceRevision"),
                    rev_ok.clone().into(),
                ),
                q(
                    &pass_ok_1,
                    &format!("{OA}hasSource"),
                    rend_ok.clone().into(),
                ),
                q(
                    &pass_ok_1,
                    &format!("{OA}hasSelector"),
                    sel_1.clone().into(),
                ),
                q(&sel_1, &format!("{OA}start"), int_lit(0)),
                q(&sel_1, &format!("{OA}end"), int_lit(6)),
                q(
                    &pass_ok_1,
                    &format!("{EV}textDigest"),
                    lit(&d_single.to_string()),
                ),
                // Existing passage 2 on rend_ok
                q(
                    &pass_ok_2,
                    RDF_TYPE,
                    iri(kind_iri(RecordKind::Passage)).into(),
                ),
                q(
                    &pass_ok_2,
                    &format!("{EV}availabilityReceipt"),
                    op.clone().into(),
                ),
                q(
                    &pass_ok_2,
                    &format!("{EV}graphProtection"),
                    lit(&nonce.to_string()),
                ),
                q(
                    &pass_ok_2,
                    &format!("{EV}sourceRevision"),
                    rev_ok.clone().into(),
                ),
                q(
                    &pass_ok_2,
                    &format!("{OA}hasSource"),
                    rend_ok.clone().into(),
                ),
                q(
                    &pass_ok_2,
                    &format!("{OA}hasSelector"),
                    sel_2.clone().into(),
                ),
                q(&sel_2, &format!("{OA}start"), int_lit(7)),
                q(&sel_2, &format!("{OA}end"), int_lit(15)),
                q(
                    &pass_ok_2,
                    &format!("{EV}textDigest"),
                    lit(&d_proposal.to_string()),
                ),
            ],
        );
        ds.store().compact().unwrap();

        // 1. Compatible rendition probe (rev_ok, rend_ok) followed by incompatible
        //    (rend_ok, rend_ok): fallback reuses the exact shape already checked by the
        //    incompatible probe -> 1 simple literal read total (not 2).
        let compat_then_incompat = EvidenceInput {
            passages: vec![
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p1"),
                    revision: RecordRef::Existing(rev_ok.clone()),
                    rendition: RecordRef::Existing(rend_ok.clone()),
                    occurrence_key: key("p1"),
                    start: 0,
                    end: 6,
                    text_digest: d_single,
                    quote: Some("single".into()),
                },
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p2"),
                    revision: RecordRef::Existing(rend_ok.clone()),
                    rendition: RecordRef::Existing(rend_ok.clone()),
                    occurrence_key: key("p2"),
                    start: 0,
                    end: 6,
                    text_digest: d_single,
                    quote: Some("single".into()),
                },
            ],
            ..Default::default()
        };
        reset_literal_read_counts();
        let err = run_verify(&ds, &settings, &compat_then_incompat, MEMORY_BYTES).unwrap_err();
        assert_eq!(err.code(), "immutable-record-conflict");
        assert_eq!(literal_read_counts(), (1, 0));

        // 2a. Earlier compatible probe on wrong-shape rendition T, then wrong-shape
        //     rendition B, then incompatible (T, T): carries forward T's exact
        //     Unavailable outcome at T's earlier position before B without rereading T
        //     or reading B.
        let earlier_bad_shape = EvidenceInput {
            passages: vec![
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p-bad-1"),
                    revision: RecordRef::Existing(rev_ok.clone()),
                    rendition: RecordRef::Existing(rend_bad_shape.clone()),
                    occurrence_key: key("p-bad-1"),
                    start: 0,
                    end: 6,
                    text_digest: d_single,
                    quote: None,
                },
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p-bad-mid"),
                    revision: RecordRef::Existing(rev_ok.clone()),
                    rendition: RecordRef::Existing(rend_bad_shape_2.clone()),
                    occurrence_key: key("p-bad-mid"),
                    start: 0,
                    end: 6,
                    text_digest: d_single,
                    quote: None,
                },
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p-bad-2"),
                    revision: RecordRef::Existing(rend_bad_shape.clone()),
                    rendition: RecordRef::Existing(rend_bad_shape.clone()),
                    occurrence_key: key("p-bad-2"),
                    start: 0,
                    end: 6,
                    text_digest: d_single,
                    quote: None,
                },
            ],
            ..Default::default()
        };
        reset_literal_read_counts();
        let err = run_verify(&ds, &settings, &earlier_bad_shape, MEMORY_BYTES).unwrap_err();
        assert_eq!(err.code(), "evidence-unavailable");
        assert!(err.to_string().contains(rend_bad_shape.as_str()));
        assert_eq!(literal_read_counts(), (1, 0));

        // 2b. Earlier compatible probe on valid rendition T, then wrong-shape rendition B,
        //     then incompatible (T, T): fallback skips rereading T (cached Valid) and
        //     reports B's earlier `evidence-unavailable` before T's kind conflict
        //     (2 reads total: 1 for T, 1 for B).
        let valid_then_bad_then_incompat = EvidenceInput {
            passages: vec![
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p-ok-first"),
                    revision: RecordRef::Existing(rev_ok.clone()),
                    rendition: RecordRef::Existing(rend_ok.clone()),
                    occurrence_key: key("p-ok-first"),
                    start: 0,
                    end: 6,
                    text_digest: d_single,
                    quote: Some("single".into()),
                },
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p-bad-second"),
                    revision: RecordRef::Existing(rev_ok.clone()),
                    rendition: RecordRef::Existing(rend_bad_shape.clone()),
                    occurrence_key: key("p-bad-second"),
                    start: 0,
                    end: 6,
                    text_digest: d_single,
                    quote: None,
                },
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p-ok-incompat"),
                    revision: RecordRef::Existing(rend_ok.clone()),
                    rendition: RecordRef::Existing(rend_ok.clone()),
                    occurrence_key: key("p-ok-incompat"),
                    start: 0,
                    end: 6,
                    text_digest: d_single,
                    quote: Some("single".into()),
                },
            ],
            ..Default::default()
        };
        reset_literal_read_counts();
        let err =
            run_verify(&ds, &settings, &valid_then_bad_then_incompat, MEMORY_BYTES).unwrap_err();
        assert_eq!(err.code(), "evidence-unavailable");
        assert!(err.to_string().contains(rend_bad_shape.as_str()));
        assert_eq!(literal_read_counts(), (2, 0));

        // 3. Symmetric structured compatible probe followed by incompatible rendition probe:
        //    exactly 1 typed literal read total.
        let struct_then_incompat = EvidenceInput {
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("s-ok"),
                occurrence_key: key("s-ok"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(rev_ok.clone()),
                    pointer: "/score".into(),
                    method: MethodRef {
                        method: iri(JSON_LEXICAL_UTF8_V1),
                        version: key(JSON_LEXICAL_UTF8_VERSION),
                    },
                    value_digest: val_digest,
                },
            }],
            passages: vec![PassageInput {
                version: RecordVersion::V1,
                key: key("p-rev-as-rend"),
                revision: RecordRef::Existing(rev_ok.clone()),
                rendition: RecordRef::Existing(rev_ok.clone()),
                occurrence_key: key("p-rev-as-rend"),
                start: 0,
                end: 6,
                text_digest: d_single,
                quote: None,
            }],
            ..Default::default()
        };
        reset_literal_read_counts();
        let err = run_verify(&ds, &settings, &struct_then_incompat, MEMORY_BYTES).unwrap_err();
        assert_eq!(err.code(), "immutable-record-conflict");
        assert_eq!(literal_read_counts(), (0, 1));

        // 4. Mixed successful proposal creating a new passage from `rend_ok` AND
        //    referencing two existing passages on `rend_ok`: shares one single
        //    `ev:normalizedText` read across both selection forms.
        let mixed_ok = EvidenceInput {
            passages: vec![PassageInput {
                version: RecordVersion::V1,
                key: key("p-new"),
                revision: RecordRef::Existing(rev_ok.clone()),
                rendition: RecordRef::Existing(rend_ok.clone()),
                occurrence_key: key("p-new"),
                start: 16,
                end: 20,
                text_digest: d_read,
                quote: Some("read".into()),
            }],
            references: vec![
                EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("ref-existing-1"),
                    occurrence_key: key("ref-existing-1"),
                    target: EvidenceRefTarget::Passage {
                        revision: RecordRef::Existing(rev_ok.clone()),
                        rendition: RecordRef::Existing(rend_ok.clone()),
                        passage: RecordRef::Existing(pass_ok_1.clone()),
                    },
                },
                EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("ref-existing-2"),
                    occurrence_key: key("ref-existing-2"),
                    target: EvidenceRefTarget::Passage {
                        revision: RecordRef::Existing(rev_ok.clone()),
                        rendition: RecordRef::Existing(rend_ok.clone()),
                        passage: RecordRef::Existing(pass_ok_2.clone()),
                    },
                },
            ],
            ..Default::default()
        };
        reset_literal_read_counts();
        run_verify(&ds, &settings, &mixed_ok, MEMORY_BYTES).unwrap();
        assert_eq!(literal_read_counts(), (1, 0));
    }

    #[test]
    fn proposal_phase2_budget_and_mixed_batch_wrong_shape_fallback_parity() {
        use base64::Engine as _;
        use oxrdf::{Literal, Term};
        use sparkles_core::evidence::resolve::kind_iri;

        const EV: &str = "urn:x-sparkles:evidence:";
        const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
        const PROV_DERIVED: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";
        const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
        const XSD_BASE64: &str = "http://www.w3.org/2001/XMLSchema#base64Binary";

        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let (settings, nonce) = configure_test_evidence(&ds);
        let sg = oxrdf::GraphName::NamedNode(iri("urn:ev:sources"));
        let q =
            |s: &NamedNode, p: &str, o: Term| oxrdf::Quad::new(s.clone(), iri(p), o, sg.clone());
        let lit = |v: &str| Term::Literal(Literal::new_simple_literal(v));
        let int_lit =
            |n: u64| Term::Literal(Literal::new_typed_literal(n.to_string(), iri(XSD_INTEGER)));

        let op = iri("urn:op:r4-budget");
        let rev_big = iri("urn:rev:r4-big");
        let rev_other = iri("urn:rev:r4-other");
        let rev_bad = iri("urn:rev:r4-wrong-shape");
        let rev_missing = iri("urn:rev:r4-missing");
        let rend_ok = iri("urn:rend:r4-budget-ok");

        // 180 KiB JSON -> ~240 KiB base64 literal (< 256 KiB Vec capacity): leaves a
        // ~52 KiB window where the reconstructed vocab key fits during literal-shape
        // checking (128 + 256 = 384 KiB peak), while the 180 KiB decoded buffer cannot
        // fit simultaneously alongside the 256 KiB live key (256 + 180 = 436 KiB).
        let json = format!(r#"{{"score":1,"pad":"{}"}}"#, "x".repeat(180 * 1024));
        let json_digest = Sha256Digest::of_bytes(json.as_bytes(), &Control::none()).unwrap();
        let val_digest = Sha256Digest::of_bytes(b"1", &Control::none()).unwrap();
        let json_b64 = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        let text = "Release 1.2";
        let d_text = Sha256Digest::of_bytes(text.as_bytes(), &Control::none()).unwrap();

        let mut quads = vec![
            q(&op, RDF_TYPE, iri(&format!("{EV}Operation")).into()),
            q(
                &op,
                &format!("{EV}datasetId"),
                lit(&ds.dataset_id().to_string()),
            ),
            q(
                &op,
                &format!("{EV}branchId"),
                lit(&ds.store().branch_id().to_string()),
            ),
            q(&op, &format!("{EV}commit"), int_lit(1)),
            q(
                &op,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev_big,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(
                &rev_big,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rev_big,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev_big,
                &format!("{EV}contentDigest"),
                lit(&json_digest.to_string()),
            ),
            q(
                &rev_big,
                &format!("{EV}retainedBytes"),
                Term::Literal(Literal::new_typed_literal(json_b64, iri(XSD_BASE64))),
            ),
            q(
                &rev_other,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(
                &rev_other,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rev_other,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev_other,
                &format!("{EV}contentDigest"),
                lit(&json_digest.to_string()),
            ),
            q(
                &rev_bad,
                RDF_TYPE,
                iri(kind_iri(RecordKind::SourceRevision)).into(),
            ),
            q(
                &rev_bad,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rev_bad,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(
                &rev_bad,
                &format!("{EV}contentDigest"),
                lit(&json_digest.to_string()),
            ),
            q(
                &rev_bad,
                &format!("{EV}retainedBytes"),
                iri("urn:not:base64-value").into(),
            ),
            q(
                &rend_ok,
                RDF_TYPE,
                iri(kind_iri(RecordKind::Rendition)).into(),
            ),
            q(
                &rend_ok,
                &format!("{EV}availabilityReceipt"),
                op.clone().into(),
            ),
            q(
                &rend_ok,
                &format!("{EV}graphProtection"),
                lit(&nonce.to_string()),
            ),
            q(&rend_ok, PROV_DERIVED, rev_big.clone().into()),
            q(&rend_ok, &format!("{EV}normalizedText"), lit(text)),
        ];
        // Isolate `rev_big`'s large base64 literal into its own front-coded Vocab block
        // so IRI lookups do not scan past the 240 KiB literal.
        let pad_sub = iri("urn:pad:vocab-blocks");
        for i in 0..32u32 {
            quads.push(q(
                &pad_sub,
                &format!("{EV}padBefore"),
                lit(&format!("a{i:02}")),
            ));
            quads.push(q(
                &pad_sub,
                &format!("{EV}padAfter"),
                lit(&format!("z{i:02}")),
            ));
        }
        insert_managed_quads(&ds, quads);
        ds.store().compact().unwrap();

        let two_struct = |b: &NamedNode| EvidenceInput {
            references: vec![
                EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("s-a"),
                    occurrence_key: key("s-a"),
                    target: EvidenceRefTarget::Structured {
                        revision: RecordRef::Existing(rev_big.clone()),
                        pointer: "/score".into(),
                        method: MethodRef {
                            method: iri(JSON_LEXICAL_UTF8_V1),
                            version: key(JSON_LEXICAL_UTF8_VERSION),
                        },
                        value_digest: val_digest,
                    },
                },
                EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("s-b"),
                    occurrence_key: key("s-b"),
                    target: EvidenceRefTarget::Structured {
                        revision: RecordRef::Existing(b.clone()),
                        pointer: "/score".into(),
                        method: MethodRef {
                            method: iri(JSON_LEXICAL_UTF8_V1),
                            version: key(JSON_LEXICAL_UTF8_VERSION),
                        },
                        value_digest: val_digest,
                    },
                },
            ],
            ..Default::default()
        };

        let scrub = |e: &Error, target: &NamedNode| {
            (
                e.code().to_string(),
                e.to_string().replace(target.as_str(), "TARGET"),
            )
        };

        // Find a budget in [4 KiB, 512 KiB] where:
        // - `(rev_big, rev_big)` fails in Phase 2 decoding with `incomplete-evidence`
        //   naming `rev_big`, AND
        // - `(rev_big, rev_missing)` succeeds at `rev_big`'s literal-shape check and
        //   returns `evidence-unavailable` naming `rev_missing`.
        let valid_pair = two_struct(&rev_big);
        let absent_pair = two_struct(&rev_missing);
        let wrong_shape_pair = two_struct(&rev_bad);

        let mut decode_refusal_budget = None;
        let mut key_refusal_budget = None;
        for kib in (128u64..=768).step_by(8) {
            let cap = kib * 1024;
            let absent_res = run_verify(&ds, &settings, &absent_pair, cap);
            let valid_res = run_verify(&ds, &settings, &valid_pair, cap);
            if let (Err(a_err), Err(v_err)) = (&absent_res, &valid_res) {
                if a_err.code() == "incomplete-evidence"
                    && a_err.to_string().contains(rev_big.as_str())
                    && key_refusal_budget.is_none()
                {
                    key_refusal_budget = Some(cap);
                }
                if a_err.code() == "evidence-unavailable"
                    && a_err.to_string().contains(rev_missing.as_str())
                    && v_err.code() == "incomplete-evidence"
                    && v_err.to_string().contains(rev_big.as_str())
                {
                    decode_refusal_budget = Some(cap);
                    break;
                }
            }
        }
        let decode_budget = decode_refusal_budget
            .expect("expected a budget admitting A's shape check but refusing A's decode buffer");
        let key_budget =
            key_refusal_budget.expect("expected a tighter budget refusing A's vocab key itself");

        // 1. Under `decode_budget`, Phase 2 decode refusal on A must still check B's
        //    deferred literal shape and match absent B byte for byte.
        let absent_err = run_verify(&ds, &settings, &absent_pair, decode_budget).unwrap_err();
        let bad_shape_err =
            run_verify(&ds, &settings, &wrong_shape_pair, decode_budget).unwrap_err();
        assert_eq!(absent_err.code(), "evidence-unavailable");
        assert_eq!(
            scrub(&bad_shape_err, &rev_bad),
            scrub(&absent_err, &rev_missing)
        );

        // 2. Under `key_budget` where A's vocab key itself does not fit, both absent B
        //    and wrong-shape B return identical `incomplete-evidence` naming A.
        let absent_key_err = run_verify(&ds, &settings, &absent_pair, key_budget).unwrap_err();
        let bad_key_err = run_verify(&ds, &settings, &wrong_shape_pair, key_budget).unwrap_err();
        assert_eq!(absent_key_err.code(), "incomplete-evidence");
        assert_eq!(bad_key_err.code(), "incomplete-evidence");
        assert_eq!(bad_key_err.to_string(), absent_key_err.to_string());

        // 3. Mixed-batch Phase 2 fallback dispatch: an earlier proposed passage that
        //    fails with `span-mismatch` (mismatched quote) or `invalid` (wrong revision)
        //    still runs `check_deferred_literal_shapes` on a later wrong-shape target B
        //    and matches absent B byte for byte.
        let mixed_bad_quote = |b: &NamedNode| EvidenceInput {
            passages: vec![PassageInput {
                version: RecordVersion::V1,
                key: key("p-bad-quote"),
                revision: RecordRef::Existing(rev_big.clone()),
                rendition: RecordRef::Existing(rend_ok.clone()),
                occurrence_key: key("p-bad-quote"),
                start: 0,
                end: 11,
                text_digest: d_text,
                quote: Some("Wrong quote".into()),
            }],
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("s-after-quote"),
                occurrence_key: key("s-after-quote"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(b.clone()),
                    pointer: "/score".into(),
                    method: MethodRef {
                        method: iri(JSON_LEXICAL_UTF8_V1),
                        version: key(JSON_LEXICAL_UTF8_VERSION),
                    },
                    value_digest: val_digest,
                },
            }],
            ..Default::default()
        };
        let quote_absent =
            run_verify(&ds, &settings, &mixed_bad_quote(&rev_missing), MEMORY_BYTES).unwrap_err();
        let quote_bad =
            run_verify(&ds, &settings, &mixed_bad_quote(&rev_bad), MEMORY_BYTES).unwrap_err();
        assert_eq!(quote_absent.code(), "evidence-unavailable");
        assert_eq!(
            scrub(&quote_bad, &rev_bad),
            scrub(&quote_absent, &rev_missing)
        );

        let mixed_bad_rev = |b: &NamedNode| EvidenceInput {
            passages: vec![PassageInput {
                version: RecordVersion::V1,
                key: key("p-bad-rev"),
                revision: RecordRef::Existing(rev_other.clone()),
                rendition: RecordRef::Existing(rend_ok.clone()),
                occurrence_key: key("p-bad-rev"),
                start: 0,
                end: 11,
                text_digest: d_text,
                quote: Some(text.into()),
            }],
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("s-after-rev"),
                occurrence_key: key("s-after-rev"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(b.clone()),
                    pointer: "/score".into(),
                    method: MethodRef {
                        method: iri(JSON_LEXICAL_UTF8_V1),
                        version: key(JSON_LEXICAL_UTF8_VERSION),
                    },
                    value_digest: val_digest,
                },
            }],
            ..Default::default()
        };
        let rev_absent =
            run_verify(&ds, &settings, &mixed_bad_rev(&rev_missing), MEMORY_BYTES).unwrap_err();
        let rev_bad_err =
            run_verify(&ds, &settings, &mixed_bad_rev(&rev_bad), MEMORY_BYTES).unwrap_err();
        assert_eq!(rev_absent.code(), "evidence-unavailable");
        assert_eq!(
            scrub(&rev_bad_err, &rev_bad),
            scrub(&rev_absent, &rev_missing)
        );

        // 4. Phase 1 fallback dispatch: wrong-shape target B before missing target M
        //    returns `evidence-unavailable` naming B (not M).
        let bad_before_missing = EvidenceInput {
            references: vec![
                EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("s-first-bad"),
                    occurrence_key: key("s-first-bad"),
                    target: EvidenceRefTarget::Structured {
                        revision: RecordRef::Existing(rev_bad.clone()),
                        pointer: "/score".into(),
                        method: MethodRef {
                            method: iri(JSON_LEXICAL_UTF8_V1),
                            version: key(JSON_LEXICAL_UTF8_VERSION),
                        },
                        value_digest: val_digest,
                    },
                },
                EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("s-second-missing"),
                    occurrence_key: key("s-second-missing"),
                    target: EvidenceRefTarget::Structured {
                        revision: RecordRef::Existing(rev_missing.clone()),
                        pointer: "/score".into(),
                        method: MethodRef {
                            method: iri(JSON_LEXICAL_UTF8_V1),
                            version: key(JSON_LEXICAL_UTF8_VERSION),
                        },
                        value_digest: val_digest,
                    },
                },
            ],
            ..Default::default()
        };
        let err = run_verify(&ds, &settings, &bad_before_missing, MEMORY_BYTES).unwrap_err();
        assert_eq!(err.code(), "evidence-unavailable");
        assert!(err.to_string().contains(rev_bad.as_str()));
    }
}
