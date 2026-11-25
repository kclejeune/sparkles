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

use super::resolve::{Reference, Resolution};
use crate::access::GraphAccess;
use crate::branch::CommitRef;
use crate::catalog::ReconcileCatalogIdentities;
use crate::managed::{ManagedError, ManagedErrorKind, OwnerKind};
use crate::store::{HistoryBound, HistoryQuery, ListedBranches, PublishedWriter, Store};
use crate::task::Control;
use crate::{Catalog, Dataset, Error, Result};
use oxrdf::{NamedNode, Term};
use sparkles_core::evidence::resolve::RecordRequest;
use sparkles_core::evidence::write::{
    self, ExistingRecord, Plan, ProposalOptions, ProtectedGraph, Reads, Stamp,
};
use sparkles_core::evidence::{
    EvidenceInput, EvidenceRefTarget, RecordKind, RecordRef, RenditionContent,
    ResolutionRequirement, Settings, Sha256Digest,
};
use std::collections::{HashMap, HashSet, hash_map::Entry};
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
    found: write::RecordedOperationDetails,
    reads: &mut Reads<'_>,
    branch: Uuid,
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
        hold: source_hold,
        records: result(plan, &existing),
        preview: None,
    })
}

/// The references a proposal must resolve, and a refusal of a requirement this build
/// cannot verify.
fn references(
    input: &EvidenceInput,
    plan: &Plan,
    requirements: &[ResolutionRequirement],
) -> Result<Vec<(String, Reference)>> {
    let minted = |key: &str| plan.records.iter().find(|r| r.key == key).map(|r| &r.iri);
    let resolve = |r: &RecordRef| match r {
        RecordRef::Local(k) => minted(k.as_str()).cloned(),
        RecordRef::Existing(iri) => Some(iri.clone()),
    };
    let unverifiable = |key: &str, what: &str| {
        managed(
            ManagedErrorKind::IncompleteEvidence,
            format!("{key}: {what} cannot be verified by this build, so nothing was written"),
        )
    };
    let mut out: Vec<(String, Reference)> = Vec::new();
    for requirement in requirements {
        match requirement {
            ResolutionRequirement::ExistingRecord {
                record,
                expected_kinds,
            } => {
                // every use is resolved with its own kinds, and record IRIs never stand
                // for the batch keys of other references
                out.push((
                    record.as_str().to_string(),
                    Reference::Record(RecordRequest {
                        iri: record.clone(),
                        kinds: expected_kinds.clone(),
                        content_digest: None,
                    }),
                ));
            }
            ResolutionRequirement::StoredFact { reference_key } => {
                let fact = input
                    .references
                    .iter()
                    .find(|r| r.key.as_str() == reference_key)
                    .and_then(|r| match &r.target {
                        EvidenceRefTarget::StoredFact(f) => Some(f.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| Error::invalid(format!("{reference_key}: no stored fact")))?;
                out.push((reference_key.clone(), Reference::StoredFact(fact)));
            }
            ResolutionRequirement::PassageIdentity { passage_key } => {
                let passage = input
                    .passages
                    .iter()
                    .find(|p| p.key.as_str() == passage_key)
                    .ok_or_else(|| Error::invalid(format!("{passage_key}: no passage")))?;
                let RecordRef::Local(rendition) = &passage.rendition else {
                    return Err(unverifiable(
                        passage_key,
                        "a passage of an existing rendition",
                    ));
                };
                let rendition = input
                    .renditions
                    .iter()
                    .find(|r| r.key.as_str() == rendition.as_str())
                    .ok_or_else(|| Error::invalid(format!("{passage_key}: no rendition")))?;
                if resolve(&rendition.revision) != resolve(&passage.revision) {
                    return Err(Error::invalid(format!(
                        "{passage_key}: the passage and its rendition name different revisions"
                    )));
                }
            }
            ResolutionRequirement::RetainedPassage { passage_key } => {
                let passage = input
                    .passages
                    .iter()
                    .find(|p| p.key.as_str() == passage_key)
                    .ok_or_else(|| Error::invalid(format!("{passage_key}: no passage")))?;
                let local_text = match &passage.rendition {
                    RecordRef::Local(k) => input.renditions.iter().any(|r| {
                        r.key.as_str() == k.as_str()
                            && matches!(r.content, RenditionContent::NormalizedText(_))
                    }),
                    RecordRef::Existing(_) => false,
                };
                if !local_text {
                    return Err(unverifiable(
                        passage_key,
                        "a passage of a rendition without normalized text in this batch",
                    ));
                }
            }
            ResolutionRequirement::PassageReferenceIdentity { reference_key } => {
                let reference = input
                    .references
                    .iter()
                    .find(|r| r.key.as_str() == reference_key)
                    .ok_or_else(|| Error::invalid(format!("{reference_key}: no reference")))?;
                let EvidenceRefTarget::Passage {
                    revision,
                    rendition,
                    passage,
                } = &reference.target
                else {
                    return Err(Error::invalid(format!("{reference_key}: no passage")));
                };
                let RecordRef::Local(passage) = passage else {
                    return Err(unverifiable(
                        reference_key,
                        "a reference to an existing passage",
                    ));
                };
                let passage = input
                    .passages
                    .iter()
                    .find(|p| p.key.as_str() == passage.as_str())
                    .ok_or_else(|| Error::invalid(format!("{reference_key}: no passage")))?;
                if resolve(revision) != resolve(&passage.revision)
                    || resolve(rendition) != resolve(&passage.rendition)
                {
                    return Err(Error::invalid(format!(
                        "{reference_key}: the reference names another revision or rendition than its passage"
                    )));
                }
            }
            ResolutionRequirement::StructuredValue { reference_key } => {
                return Err(unverifiable(
                    reference_key,
                    "a structured value outside a retained JSON revision of this batch",
                ));
            }
        }
    }
    Ok(out)
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
            return replay(&plan, found, &mut reads, branch);
        }
    }
    // Retain the same dataset handles the resolver read. Reopening by UUID could
    // find a dataset restored in the meantime with different history at that commit.
    let refs = references(input, &plan, prepared.requirements())?;
    let mut resolved_datasets = HashMap::new();
    if !refs.is_empty() {
        Resolution::new(control.clone())?.all(
            catalog,
            main,
            logical,
            &refs,
            &mut |ds, branch| {
                let granted = read(ds, branch)?;
                if ds.dataset_id() != main.dataset_id() {
                    let resolved: &Dataset = resolved_datasets
                        .entry(ds.dataset_id())
                        .or_insert_with(|| ds.clone());
                    if !std::ptr::eq(resolved.store(), ds.store()) {
                        return None;
                    }
                }
                Some(granted)
            },
        )?;
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
        return replay(&plan, found, &mut reads, branch);
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
            let Ok(ranges) = store.published_history_ranges(&preparation, READS_BYTES) else {
                return Ok(true);
            };
            for (first, last) in ranges {
                if remaining == 0 {
                    return Ok(true);
                }
                // A complete log proves that only the range's first state and
                // subsequent changes to the receipt's predicates can matter.
                query.from = Some(HistoryBound::Commit(first.saturating_add(1)));
                query.to = Some(HistoryBound::Commit(last));
                query.limit = remaining;
                let changes = (first < last)
                    .then(|| {
                        if no_wait {
                            store.try_published_history_changes(&query)
                        } else {
                            store.published_history_changes(&query)
                        }
                        .ok()
                    })
                    .flatten()
                    .filter(|result| {
                        !result.truncated && result.unrecorded.is_empty() && result.to == last
                    });
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
                            if Reads::new(&preparation, &snap)
                                .describes_hold(op, target, commits)?
                            {
                                return Ok(true);
                            }
                        }
                        // Without the first state, a complete log still cannot
                        // establish which operation descriptions were inherited.
                        Err(_) => return Ok(true),
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
