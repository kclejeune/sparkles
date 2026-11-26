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
//! 5. The stored facts of this dataset are held durably under a hold named by the
//!    operation, before the commit that refers to them.
//! 6. Under the branch writer the expected head, the idempotency key again and the
//!    identity of every record are checked, the commit is stamped, the server sets the
//!    receipt, the first availability, each record digest and the operation marker,
//!    and right before publication the caller's authority and the hold are checked.
//!
//! A failure after step 5 leaves the hold without an operation that records it. It is
//! an orphan that [`reconcile_operation_holds`] releases once its grace period has
//! passed, which every published proposal runs, and a retry with the same idempotency
//! key acquires the same hold again.

use super::resolve::{
    MAX_REFERENCES, MEMORY_BYTES, ProposedPassage, ProposedStructured, Resolution,
    SelectionRequirements, unavailable,
};
use crate::access::GraphAccess;
use crate::managed::{ManagedError, ManagedErrorKind, OwnerKind};
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
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
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
    /// The source hold of the operation's stored facts in this dataset.
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

fn result(plan: &Plan, skip: &[bool]) -> Vec<ProposedRecord> {
    plan.records
        .iter()
        .enumerate()
        .map(|(i, r)| ProposedRecord {
            key: r.key.clone(),
            kind: r.kind,
            iri: r.iri.clone(),
            graph: r.graph.clone(),
            digest: r.digest,
            existing: skip.get(i).copied().unwrap_or(false),
        })
        .collect()
}

/// What a repeated key means: the first receipt, or a conflict.
fn replay(
    plan: &Plan,
    found: write::RecordedOperation,
    reads: &mut Reads<'_>,
    branch: Uuid,
    hold: Option<Uuid>,
) -> Result<Proposed> {
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
    // A record whose receipt is another operation was already published when the
    // first operation ran, which reported it as existing.
    let mut existing = Vec::with_capacity(plan.records.len());
    for r in &plan.records {
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
        hold: (!plan.held.is_empty()).then_some(hold).flatten(),
        records: result(plan, &existing),
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
    )
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
    let prep = resolution.preparation().clone();
    enum GenericReq<'a> {
        Record(&'a NamedNode, &'a [RecordKind], SelectionRequirements),
        Fact(&'a str, &'a StoredFactRef),
    }
    let mut generic_count = 0usize;
    let mut generic_reqs: AdmittedVec<GenericReq<'_>> = AdmittedVec::new(&prep, &admit)?;
    let mut passage_groups: AdmittedVec<(&NamedNode, AdmittedVec<ProposedPassage<'_>>)> =
        AdmittedVec::new(&prep, &admit)?;
    let mut passage_refs: AdmittedVec<(&str, &NamedNode, &NamedNode, &NamedNode)> =
        AdmittedVec::new(&prep, &admit)?;
    let mut structured_groups: AdmittedVec<(&NamedNode, AdmittedVec<ProposedStructured<'_>>)> =
        AdmittedVec::new(&prep, &admit)?;
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
                                        passage_groups.push(&prep, (rendition, group), &admit)?;
                                        Ok(i)
                                    },
                                    &admit,
                                )?;
                                passage_groups.as_mut_slice()[idx].1.push(
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
                                        passage_groups.push(&prep, (rendition, group), &admit)?;
                                        Ok(i)
                                    },
                                    &admit,
                                )?;
                                passage_groups.as_mut_slice()[idx].1.push(
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
                                    structured_groups.push(&prep, (rev_iri, group), &admit)?;
                                    Ok(i)
                                },
                                &admit,
                            )?;
                            structured_groups.as_mut_slice()[idx].1.push(
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
        for req in generic_reqs.as_mut_slice() {
            control.check()?;
            if let GenericReq::Record(iri, _, selection) = req
                && let Some(needs) = selection_needs.get(iri)
            {
                *selection = *needs;
            }
        }
    }
    // Phase 1: resolve every ExistingRecord (with any selection-target availability
    // requirements attached) and every StoredFact first so missing, graph-denied,
    // metadata-only and triple-masked targets always share identical precedence before
    // any identity or content comparison runs.
    let mut record_access: Option<Option<Arc<GraphAccess>>> = None;
    for req in generic_reqs.as_slice() {
        control.check()?;
        match req {
            GenericReq::Fact(key, fact) => {
                let _ = match catalog {
                    Some(catalog) if fact.dataset != main.dataset_id() => {
                        resolution.stored_fact(key, catalog, fact, &mut *read)?
                    }
                    _ => resolution.stored_fact_in(key, main, fact, &mut *read)?,
                };
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
                resolution.verify_record_with_selection(
                    iri.as_str(),
                    main,
                    logical,
                    granted,
                    iri,
                    kinds,
                    *selection,
                )?;
            }
        }
    }
    drop(generic_reqs);
    // Phase 2: run content and identity comparisons, dropping each phase's admitted
    // buffer as soon as it finishes so its reservation is released before the next.
    if !passage_groups.is_empty() {
        let access = match &record_access {
            Some(access) => access.clone(),
            None => {
                let access = resolution.logical_access(main, logical, read)?;
                record_access = Some(access.clone());
                access
            }
        };
        for (rendition, passages) in passage_groups.as_slice() {
            control.check()?;
            resolution.verify_proposed_passages(
                main,
                logical,
                access.clone(),
                rendition,
                passages.as_slice(),
            )?;
        }
    }
    drop(passage_groups);
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
            AdmittedSet::new(&prep, &admit)?;
        for (ref_key, passage, expected_rev, expected_rend) in passage_refs.as_slice() {
            control.check()?;
            if checked_passage_refs.insert((*passage, *expected_rev, *expected_rend), &admit)? {
                resolution.verify_passage_reference(
                    ref_key,
                    main,
                    logical,
                    access.clone(),
                    passage,
                    expected_rev,
                    expected_rend,
                )?;
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
        for (revision, structured) in structured_groups.as_slice() {
            control.check()?;
            resolution.verify_structured_references(
                main,
                logical,
                access.clone(),
                revision,
                structured.as_slice(),
            )?;
        }
    }
    drop(structured_groups);
    Ok(())
}

/// Publish `input` as one commit of the evidence owner on `logical`, which is `main` or
/// one of its branches, in the order the [module](self) describes. `read` is asked what
/// the caller may read on a dataset and branch, by their current names, as
/// [`Resolution::all`] describes. Stored facts in other datasets of `catalog` are
/// verified but not held.
///
/// Refusals: a malformed or inconsistent input is invalid, `unsupported-profile` when
/// the profile is not enabled or a written graph is detached, `head-changed` when
/// `if_head` is not the head, `idempotency-conflict` for a key used with another
/// payload, `immutable-record-conflict` for an identity whose recorded content differs
/// or that is not an authenticated record, the resolution outcomes for references,
/// `hold-conflict` for a hold of the operation that names other commits, and
/// `authority-changed` from the authority fence. A write the caller's grants refuse is
/// not permitted. Nothing is written on a refusal.
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
    if main.store().is_branch() || logical.store().owner_dataset_id() != main.dataset_id() {
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
    let hold = (!plan.held.is_empty()).then_some(plan.operation);
    // A repeated key answers before anything is resolved, held or locked.
    if options.idempotency_key.is_some() {
        let head = store.snapshot();
        let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
        let mut reads = Reads::new(&preparation, &head);
        if let Some(found) = reads.operation(&plan.operation_iri, &records)? {
            return replay(&plan, found, &mut reads, branch, hold);
        }
    }
    // The caller's reads decide every reference and existing-record selection before
    // anything is held or written.
    verify_requirements(
        catalog,
        main,
        logical,
        input,
        &plan,
        prepared.requirements(),
        read,
        control,
    )?;
    // The hold is durable before the commit that records it.
    let commits: Vec<crate::branch::CommitRef> = plan
        .held
        .iter()
        .map(|h| crate::branch::CommitRef {
            branch_id: h.branch,
            seq: h.commit,
        })
        .collect();
    if let Some(id) = hold
        && !options.dry_run
    {
        main.store()
            .acquire_hold(OwnerKind::Evidence, id, &commits)
            .map_err(|e| match e {
                Error::HistoryGone(_) => managed(
                    ManagedErrorKind::EvidenceUnavailable,
                    "a referenced commit is no longer retained",
                ),
                e => e,
            })?;
        main.store().failpoint("evidence-held");
    }
    // The hook runs on this thread, inside the commit, and keeps the authority guard
    // in this thread's slot until the commit returns.
    let (release, before_publish) = {
        let held_by = main.clone();
        let dry_run = options.dry_run;
        publish_hook(options.authority.clone(), move || {
            // An administrator's release leaves the references unheld: refuse rather
            // than record a hold that no longer protects them. The guard keeps every
            // release waiting until this commit has published or been refused.
            match hold.filter(|_| !dry_run) {
                Some(id) => Ok(Some(Box::new(held_by.store().hold_publication(
                    OwnerKind::Evidence,
                    id,
                    &commits,
                )?))),
                None => Ok(None),
            }
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
        && let Some(found) = reads.operation(&plan.operation_iri, &records)?
    {
        return replay(&plan, found, &mut reads, branch, hold);
    }
    // An identity already published with equal content is not written again. Any other
    // content under the identity, or content that is not an authenticated record, is
    // an immutable-record conflict.
    let mut skip = Vec::with_capacity(plan.records.len());
    for r in &plan.records {
        control.check()?;
        let existing = reads.record(&r.iri, &records)?;
        skip.push(match existing {
            ExistingRecord::Absent => false,
            ExistingRecord::Present {
                graph,
                digest,
                authenticated: true,
            } if graph == r.graph && digest == Some(r.digest) => true,
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
        });
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
    let records = result(&plan, &skip);
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
            // Orphaned holds of earlier operations past their grace period are released
            // after a publication. This housekeeping never fails the proposal.
            if let Err(e) = reconcile_operation_holds(main, crate::holds::DEFAULT_GRACE_MS) {
                tracing::warn!(target: "sparkles::evidence", "orphaned evidence holds were not reconciled: {e}");
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

/// Release the evidence holds acquired at least `grace_ms` milliseconds ago whose
/// operation never published, as after a failure or crash between a hold and its
/// commit. The grace period must exceed the longest request deadline, so that an
/// operation still on its way to its commit keeps its hold. Returns the released ids.
///
/// The stores are found at the first question, after the store chose its candidates,
/// so nothing is opened when no hold is old enough. A commit that passed its hold
/// check before then published on one of them, and one that passes it later renews
/// the hold, which the store then does not release. The store asks again under the
/// lock that every release takes, and that second answer reads only current
/// snapshots of those stores and takes no writer or branch table, since a commit
/// waiting for the lock may hold its writer. A branch that is renamed, deleted or
/// replaced under its name while the stores are found keeps every hold.
pub fn reconcile_operation_holds(main: &Dataset, grace_ms: i64) -> Result<Vec<Uuid>> {
    let control = Control::none();
    let mut stores: Option<Vec<Dataset>> = None;
    // A branch renamed while it is found may open another branch under its name, so
    // the stores found may miss one: then every hold is kept.
    let mut unsure = false;
    super::reconcile_holds(main, grace_ms, &mut |hold| {
        let stores = match &mut stores {
            Some(s) => s,
            None => {
                // the listed branches, found without their heads, so without writers
                let mut found = vec![main.clone()];
                for id in main.store().listed_branch_ids() {
                    let name = main.store().listed_branch_name(id, &Default::default())?;
                    match name.map(|n| main.branch(&n)) {
                        Some(Ok(ds)) if ds.store().branch_id() == id => found.push(ds),
                        _ => unsure = true,
                    }
                }
                stores.insert(found)
            }
        };
        if unsure {
            return Ok(true);
        }
        let op = operation_iri(hold.id);
        let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
        for ds in stores.iter() {
            let head = ds.store().snapshot();
            if Reads::new(&preparation, &head).described_anywhere(&op)? {
                return Ok(true);
            }
        }
        Ok(false)
    })
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

    #[test]
    fn admitted_containers_charge_exact_singleton_capacity_before_insert_and_release_on_drop() {
        let admit = |e: Error| e;
        let prep = Preparation::new(Control::none(), 64 * 1024).unwrap();
        let baseline = prep.available_bytes();
        let digest = Sha256Digest::of_bytes(b"1", &Control::none()).unwrap();

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
            assert_eq!(
                baseline - prep.available_bytes(),
                std::mem::size_of::<ProposedStructured<'_>>() as u64
            );
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
            assert_eq!(
                baseline - prep.available_bytes(),
                (std::mem::size_of::<ProposedStructured<'_>>() * 2) as u64
            );
        }
        assert_eq!(prep.available_bytes(), baseline);

        // AdmittedSet and AdmittedMap refuse before inserting when budget is too small.
        let tiny = Preparation::new(Control::none(), 320).unwrap();
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

        // 1. 64 singleton structured groups require admitting outer Vec, 64 inner
        //    singleton Vecs, index maps, and generic_reqs; a tight 8 KiB budget refuses
        //    with `incomplete-evidence` before any store access runs.
        let refs: Vec<EvidenceRefInput> = (0..64)
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
                Some(Arc::new(GraphAccess::all()))
            },
            &Control::none(),
            8 * 1024,
        )
        .unwrap_err();
        assert_eq!(err.code(), "incomplete-evidence");
        assert_eq!(reads.load(Ordering::Relaxed), 0);

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
        )
        .unwrap_err();
        assert_eq!(long_err.code(), "incomplete-evidence");
        assert!(long_err.to_string().contains("not supported by this build"));
        assert_eq!(long_reads.load(Ordering::Relaxed), 0);
    }
}
