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

use super::resolve::{Reference, Resolution};
use crate::access::GraphAccess;
use crate::managed::{ManagedError, ManagedErrorKind, OwnerKind};
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
    // The caller's reads decide every reference, before anything is written.
    let refs = references(input, &plan, prepared.requirements())?;
    if !refs.is_empty() {
        let mut resolution = Resolution::new(control.clone())?;
        resolution.all(catalog, main, logical, &refs, read)?;
    }
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
