//! Exact retained-reference and record resolution (spec C25 §3, §7 and §8).
//!
//! A [`Resolver`] verifies the references of one operation under one
//! [`Preparation`]: one memory allowance, the request's deadline and its cancellation.
//! Each stored-fact reference reads the exact commit of the branch its identity names,
//! through the caller's controlled access view, and never another branch or the head.
//! Each record reference finds an authenticated profile record in the dataset's
//! protected evidence graphs. A verified result is data. It is not a managed-write
//! capability and does not authorize a write.
//!
//! An unreadable reference and a missing one give the same `evidence-unavailable`
//! answer, so a caller cannot learn whether a hidden dataset, branch, commit, graph or
//! fact exists. Access is decided by the caller of this module before a read, and an
//! unreadable graph is refused before its snapshot is prepared.

use super::prepare::{has_blank, locate_boundaries};
use super::{BlankAnchor, GraphIdentity, RecordKind, Sha256Digest, StoredFactRef};
use crate::access::{GraphAccess, Preparation, PreparedView, Reservation};
use crate::history::At;
use crate::id::Id;
use crate::index::Perm;
use crate::managed::{ManagedError, ManagedErrorKind, OwnerKind};
use crate::store::{Snapshot, SnapshotCharge, Store};
use crate::task::Control;
use crate::{Budget, BudgetKind, Error, Result};
use base64::Engine as _;
use oxrdf::{NamedNode, Term, Triple};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

/// The memory allowance of one resolution operation (spec C25 §8).
pub const MEMORY_BYTES: u64 = 64 << 20;

/// The most references one operation resolves.
pub const MAX_REFERENCES: usize = 10_000;

const EV: &str = "urn:x-sparkles:evidence:";
const PROV_WAS_DERIVED_FROM: &str = "http://www.w3.org/ns/prov#wasDerivedFrom";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const OA: &str = "http://www.w3.org/ns/oa#";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_BASE64_BINARY: &str = "http://www.w3.org/2001/XMLSchema#base64Binary";

/// A stored fact verified at its exact retained view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedFact {
    pub dataset: Uuid,
    /// The branch identity the reference names.
    pub branch: Uuid,
    pub commit: u64,
    /// The branch whose history served the commit: `branch`, or the upstream that
    /// serves a commit at or below the branch's starting point.
    pub served_by: Uuid,
    pub graph: GraphIdentity,
    /// The stable reifier that reifies the fact in its graph, when the reference names one.
    pub reifier: Option<NamedNode>,
}

/// What a record reference expects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordRequest {
    pub iri: NamedNode,
    /// The kinds the record may have. Empty accepts any profile record kind.
    pub kinds: Vec<RecordKind>,
    /// The content digest the record must carry, for a source revision or rendition.
    pub content_digest: Option<Sha256Digest>,
}

/// The publication receipt of an authenticated record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub iri: NamedNode,
    pub dataset: Uuid,
    pub branch: Uuid,
    pub commit: u64,
}

/// A passage whose selector was verified against its retained rendition text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSpan {
    pub rendition: NamedNode,
    pub start: u64,
    pub end: u64,
    pub text_digest: Sha256Digest,
}

/// One proposed passage to verify against an existing authenticated rendition.
#[derive(Clone, Copy, Debug)]
pub struct ProposedPassage<'a> {
    pub key: &'a str,
    pub revision: &'a NamedNode,
    pub start: u64,
    pub end: u64,
    pub text_digest: Sha256Digest,
    pub quote: Option<&'a str>,
}

/// One proposed structured reference to verify against an existing source revision's
/// retained JSON bytes under `json-lexical-utf8:1`.
#[derive(Clone, Copy, Debug)]
pub struct ProposedStructured<'a> {
    pub key: &'a str,
    pub pointer: &'a str,
    pub value_digest: Sha256Digest,
}

/// Additional selection-target availability predicates required when a proposal selects
/// content or lineage from an existing record. Checking these alongside generic record
/// verification ensures masked and metadata-only selections have identical precedence to
/// missing records in mixed batches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionRequirements {
    /// Target of a proposed passage: requires visible `prov:wasDerivedFrom` and
    /// `ev:normalizedText` on the rendition.
    pub rendition_content: bool,
    /// Target of an existing-passage reference: requires visible `ev:sourceRevision` on
    /// the passage and `prov:wasDerivedFrom` on its rendition.
    pub passage_lineage: bool,
    /// Target of a supported structured reference: requires visible `ev:contentDigest`
    /// and `ev:retainedBytes` (`xsd:base64Binary`) on the source revision.
    pub structured_content: bool,
}

/// Outcome of probing a record's metadata and selection availability before Phase 2
/// content verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionProbe {
    /// All requested availability checks completed in Phase 1.
    Complete,
    /// A single visible `Vocab`/`Delta` object ID was found for the selected retained
    /// literal (`ev:normalizedText` or `ev:retainedBytes`); its exact literal shape will
    /// be confirmed in one read during Phase 2 (or via
    /// [`Resolver::verify_selection_literal_shape`] if Phase 2 is short-circuited).
    DeferredLiteralShape,
}

/// Outcome of a Phase 2 grouped verification step, carrying enough context for proposal
/// fallback dispatch to preserve exact Phase 1 error precedence.
pub struct Phase2Outcome {
    pub result: Result<()>,
    /// Whether the target's retained literal shape (`ev:normalizedText` or
    /// `ev:retainedBytes`) was confirmed before any error occurred.
    pub shape_confirmed: bool,
    /// When a deferred Phase 1 item (such as an existing passage sharing the same
    /// rendition) produced the error, its Phase 1 requirement index.
    pub failed_req_idx: Option<usize>,
}

/// An authenticated profile record at the resolving view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRecord {
    pub iri: NamedNode,
    pub kind: RecordKind,
    /// The protected evidence graph that holds the record.
    pub graph: NamedNode,
    pub receipt: Receipt,
    pub content_digest: Option<Sha256Digest>,
    /// The digest of the record's immutable content that its typed writer recorded
    /// as `ev:recordDigest` (see [`crate::evidence::write`]).
    pub record_digest: Option<Sha256Digest>,
    /// For a passage, its verified selection.
    pub span: Option<VerifiedSpan>,
}

/// The stable class IRI of a record kind.
pub fn kind_iri(kind: RecordKind) -> &'static str {
    match kind {
        RecordKind::SourceRevision => "urn:x-sparkles:evidence:SourceRevision",
        RecordKind::Rendition => "urn:x-sparkles:evidence:Rendition",
        RecordKind::Passage => "urn:x-sparkles:evidence:Passage",
        RecordKind::EvidenceRef => "urn:x-sparkles:evidence:EvidenceRef",
        RecordKind::Claim => "urn:x-sparkles:evidence:Claim",
        RecordKind::EvidenceLink => "urn:x-sparkles:evidence:EvidenceLink",
        RecordKind::Assessment => "urn:x-sparkles:evidence:Assessment",
        RecordKind::Note => "urn:x-sparkles:evidence:Note",
        RecordKind::Decision => "urn:x-sparkles:evidence:Decision",
        RecordKind::Task => "urn:x-sparkles:evidence:Task",
        RecordKind::ReviewDecision => "urn:x-sparkles:evidence:ReviewDecision",
        RecordKind::IdentityProposal => "urn:x-sparkles:evidence:IdentityProposal",
        RecordKind::AcceptanceSupport => "urn:x-sparkles:evidence:AcceptanceSupport",
        RecordKind::Withdrawal => "urn:x-sparkles:evidence:Withdrawal",
        RecordKind::Projection => "urn:x-sparkles:evidence:Projection",
        RecordKind::ResolutionMap => "urn:x-sparkles:evidence:ResolutionMap",
    }
}

pub(super) const KINDS: [RecordKind; 16] = [
    RecordKind::SourceRevision,
    RecordKind::Rendition,
    RecordKind::Passage,
    RecordKind::EvidenceRef,
    RecordKind::Claim,
    RecordKind::EvidenceLink,
    RecordKind::Assessment,
    RecordKind::Note,
    RecordKind::Decision,
    RecordKind::Task,
    RecordKind::ReviewDecision,
    RecordKind::IdentityProposal,
    RecordKind::AcceptanceSupport,
    RecordKind::Withdrawal,
    RecordKind::Projection,
    RecordKind::ResolutionMap,
];

fn error(kind: ManagedErrorKind, message: String) -> Error {
    ManagedError::error(kind, message)
}

/// Check a stored-fact reference's anchor from the reference alone, before anything,
/// even the dataset, is looked up, so that the answer is the same whether or not the
/// referenced dataset exists. Returns whether the expected triple has blank nodes.
pub fn check_anchor(key: &str, fact: &StoredFactRef) -> Result<bool> {
    let blank = has_blank(&fact.expected);
    match &fact.anchor {
        Some(BlankAnchor::ImportMap(_)) => Err(unanchored(
            key,
            "names a source import mapping, which this build does not verify",
        )),
        None if blank => Err(unanchored(
            key,
            "has blank nodes and no stable reifier; a blank-node label is not a durable identity",
        )),
        _ => Ok(blank),
    }
}

/// The one answer for a reference that is missing, unretained or hidden.
pub fn unavailable(key: &str) -> Error {
    error(
        ManagedErrorKind::EvidenceUnavailable,
        format!("evidence reference `{key}` is unavailable"),
    )
}

fn incomplete(key: &str) -> Error {
    error(
        ManagedErrorKind::IncompleteEvidence,
        format!("evidence reference `{key}` could not be verified within the resolution limit"),
    )
}

fn unanchored(key: &str, why: &str) -> Error {
    error(
        ManagedErrorKind::UnanchoredEvidence,
        format!("evidence reference `{key}` {why}"),
    )
}

fn conflict(key: &str, why: &str) -> Error {
    error(
        ManagedErrorKind::ImmutableRecordConflict,
        format!("evidence reference `{key}`: {why}"),
    )
}

fn origin(key: &str) -> Error {
    error(
        ManagedErrorKind::OriginNotReproduced,
        format!(
            "evidence reference `{key}` names a record whose publication receipt is not part of this dataset's history"
        ),
    )
}

fn span(key: &str, why: &str) -> Error {
    error(
        ManagedErrorKind::SpanMismatch,
        format!("evidence reference `{key}`: {why}"),
    )
}

/// Whether an error is one of the resolution outcomes.
fn outcome(e: &Error) -> bool {
    matches!(e, Error::Managed(m) if matches!(m.kind,
        ManagedErrorKind::EvidenceUnavailable
            | ManagedErrorKind::IncompleteEvidence
            | ManagedErrorKind::UnanchoredEvidence
            | ManagedErrorKind::SpanMismatch
            | ManagedErrorKind::OriginNotReproduced
            | ManagedErrorKind::ImmutableRecordConflict))
}

/// Map a failure of preparation or of a read to its resolution outcome. Cancellation
/// and the request deadline keep the request error contract. The resolution's own
/// allowance, and an access policy this bounded entry point cannot prepare, mean the
/// reference was not verified, never that it is absent. Storage faults pass through.
/// Every other failure reads as unavailable.
fn classify(key: &str, e: Error) -> Error {
    match e {
        e if outcome(&e) => e,
        Error::Cancelled | Error::Timeout => e,
        Error::BudgetExceeded(_) | Error::Unsupported(_) => incomplete(key),
        e @ (Error::Io(_) | Error::Corrupt(_) | Error::StorageFull(_)) => e,
        _ => unavailable(key),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ViewKey {
    dataset: Uuid,
    branch: Uuid,
    commit: u64,
}

struct Cached {
    access: Arc<GraphAccess>,
    view: PreparedView,
    served_by: Uuid,
    // Released after the view, which retains the charged snapshot.
    _retained: Reservation,
}

/// A verified passage span cached for one bound `(ViewKey, view_index)` under one
/// installed `RecordGraphs` state.
struct CachedPassage {
    graph: NamedNode,
    rendition_graph: NamedNode,
    span: VerifiedSpan,
}

/// Exact literal-shape outcome for a selected target in one bound `(ViewKey, view_index)`
/// view so fallback checks never reread a literal already inspected in the same view.
#[derive(Clone, Copy)]
enum CachedLiteralShape {
    Valid,
    Unavailable,
    Conflict(&'static str),
}

/// Outcome of a deferred existing passage's span check once its rendition's
/// `ev:normalizedText` has been scanned.
#[derive(Clone, Copy)]
enum PendingOutcome {
    Valid,
    Unavailable,
    Conflict(&'static str),
    Span(&'static str),
}

/// An existing passage whose metadata and rendition authentication succeeded during
/// Phase 1 probing, with its `ev:normalizedText` scan deferred so it can share one read
/// of that rendition with any other existing or proposed passages targeting it.
struct PendingPassage {
    key: String,
    req_idx: usize,
    passage: NamedNode,
    graph: NamedNode,
    rendition_graph: NamedNode,
    span: VerifiedSpan,
    passage_lineage: bool,
    outcome: Option<PendingOutcome>,
}

/// The record graphs of one dataset, derived from one running managed state. A
/// management change installs a new state, so a list kept for an earlier one is
/// rederived.
struct RecordGraphs {
    dataset: Uuid,
    state: Option<Arc<crate::managed::ManagedState>>,
    graphs: Vec<NamedNode>,
    /// Each record graph's protection nonce, which a typed writer records with the
    /// receipts it publishes there.
    protections: BTreeMap<String, Uuid>,
    /// Verified passage spans keyed by `(view_key, view_index)` and passage IRI.
    /// Invalidated automatically whenever `state` changes and `RecordGraphs` is rebuilt.
    verified_passages: BTreeMap<(ViewKey, usize), BTreeMap<String, CachedPassage>>,
    /// Exact literal-shape outcomes (`ev:normalizedText` or `ev:retainedBytes`) already
    /// established in one bound `(view_key, view_index)` view.
    verified_literal_shapes:
        BTreeMap<(ViewKey, usize), BTreeMap<String, (RecordKind, CachedLiteralShape)>>,
    /// Existing passages deferred during Phase 1 probing for one `(view_key, view_index)`,
    /// stored in ascending Phase-1 `req_idx` order.
    pending_passages: BTreeMap<(ViewKey, usize), Vec<PendingPassage>>,
    _held: Reservation,
}

/// Result of inspecting the object(s) of `s p ?o` in one graph under the caller's access policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SingleObject<T> {
    /// The graph is unreadable or has no visible object triple for `(s, p)`.
    None,
    /// Exactly one visible object triple exists for `(s, p)`, mapped by the caller's parser.
    One(T),
    /// Two or more visible object triples exist for `(s, p)`.
    Many,
}

/// Bounded named-node results from a controlled graph scan, keeping their preparation
/// memory charge live until dropped.
pub struct GraphNodes {
    nodes: Vec<NamedNode>,
    truncated: bool,
    _held: Reservation,
}

impl GraphNodes {
    /// The decoded named nodes retained by this result.
    pub fn nodes(&self) -> &[NamedNode] {
        &self.nodes
    }

    /// Whether the scan stopped at the caller's result limit before exhausting the match.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Whether no named nodes were collected.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Number of named nodes collected.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Iterate over the retained named nodes without releasing their reservation.
    pub fn iter(&self) -> std::slice::Iter<'_, NamedNode> {
        self.nodes.iter()
    }
}

/// One operation's resolution state. Views prepared for one reference are reused by
/// later references to the same commit under the same access, and stay charged to
/// the operation's allowance until the resolver is dropped.
pub struct Resolver {
    preparation: Preparation,
    views: BTreeMap<ViewKey, Vec<Cached>>,
    /// The record graphs of one dataset under one installed registry, charged while
    /// they are kept.
    record_graphs: Option<RecordGraphs>,
    pinned: Option<(Uuid, Uuid, u64)>,
}

impl Resolver {
    /// A resolver with `memory_bytes` of allowance under `control`.
    pub fn new(control: Control, memory_bytes: u64) -> Result<Self> {
        Ok(Self {
            preparation: Preparation::new(control, memory_bytes)?,
            views: BTreeMap::new(),
            record_graphs: None,
            pinned: None,
        })
    }

    /// The operation's preparation context.
    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    /// Pin `record`, `has_triple`, `objects_in_graph`, and `subjects_in_graph` reads of
    /// `(main, logical)` to the exact branch commit `commit`, so every lookup in this
    /// resolution sees one consistent snapshot even if the branch advances concurrently.
    pub fn pin_commit(&mut self, main: &Store, logical: &Store, commit: u64) {
        self.pinned = Some((main.dataset_id(), logical.branch_id(), commit));
    }

    /// Verify a stored-fact reference. `main` is the main store of the dataset the
    /// caller found by the reference's dataset identity, and `access` is what the
    /// caller may read on the referenced branch, `None` when it may read nothing
    /// there, as for a dataset or branch that does not exist.
    pub fn stored_fact(
        &mut self,
        key: &str,
        main: &Store,
        access: Option<&Arc<GraphAccess>>,
        fact: &StoredFactRef,
    ) -> Result<VerifiedFact> {
        self.preparation.check()?;
        let blank = check_anchor(key, fact)?;
        let Some(access) = access else {
            return Err(unavailable(key));
        };
        if main.is_branch() || main.dataset_id() != fact.dataset {
            return Err(unavailable(key));
        }
        let graph_term = match &fact.graph {
            GraphIdentity::Default => None,
            GraphIdentity::Named(n) => Some(Term::NamedNode(n.clone())),
        };
        // Grants that refuse the graph whatever a guard decides refuse it before
        // anything is read; a guard decided at the snapshot is checked once the view is
        // bound.
        if !access.may_read(graph_term.as_ref()) {
            return Err(unavailable(key));
        }
        let view_key = ViewKey {
            dataset: fact.dataset,
            branch: fact.branch,
            commit: fact.commit,
        };
        let (served_by, index) = self
            .view(view_key, access, |preparation| {
                let (snapshot, resolved, served_by, charge) = main
                    .snapshot_of_branch_limited_charged(
                        fact.branch,
                        &At::Commit(fact.commit),
                        &preparation.history_options(),
                        preparation.room(),
                    )?;
                if resolved.commit.seq != fact.commit {
                    return Err(Error::NotFound("commit".into()));
                }
                Ok((snapshot, served_by, charge))
            })
            .map_err(|e| classify(key, e))?;
        let cached = &self.views[&view_key][index];
        let reifier = check_fact(
            &self.preparation,
            &cached.view,
            graph_term.as_ref(),
            fact,
            blank,
        )
        .map_err(|e| match e {
            Missing::Unavailable => unavailable(key),
            Missing::Ambiguous => {
                unanchored(key, "names a reifier that reifies more than one triple")
            }
            Missing::Failed(e) => classify(key, e),
        })?;
        Ok(VerifiedFact {
            dataset: fact.dataset,
            branch: fact.branch,
            commit: fact.commit,
            served_by,
            graph: fact.graph.clone(),
            reifier,
        })
    }

    /// Prepare the running record-graph list and cached live view of `logical`.
    fn prepare_record_view(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
    ) -> Result<(ViewKey, usize)> {
        self.preparation.check()?;
        let Some(access) = access else {
            return Err(unavailable(key));
        };
        if main.is_branch()
            || logical.owner_dataset_id() != main.dataset_id()
            || logical.owner_dataset_id() != main.owner_dataset_id()
        {
            return Err(unavailable(key));
        }
        // The running state is read on every record, so a detach or any other
        // management change since an earlier record is seen.
        let state = main.managed_state_shared();
        let current = self.record_graphs.as_ref().is_some_and(|r| {
            r.dataset == main.dataset_id()
                && match (&r.state, &state) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                }
        });
        if !current {
            self.record_graphs = None;
            let (graphs, protections, held) =
                record_graphs(state.as_deref(), &self.preparation).map_err(|e| classify(key, e))?;
            self.record_graphs = Some(RecordGraphs {
                dataset: main.dataset_id(),
                state,
                graphs,
                protections,
                verified_passages: BTreeMap::new(),
                verified_literal_shapes: BTreeMap::new(),
                pending_passages: BTreeMap::new(),
                _held: held,
            });
        }
        let all_graphs = &self.record_graphs.as_ref().expect("prepared").graphs;
        // Grants that refuse every record graph whatever a guard decides refuse the
        // record before anything is read; a guard decided at the snapshot is checked
        // once the view is bound.
        if !all_graphs
            .iter()
            .any(|g| access.may_read(Some(&Term::NamedNode(g.clone()))))
        {
            return Err(unavailable(key));
        }
        self.logical_view(key, main, logical, access)
    }

    /// Verify a record reference against the current state of `logical`, a store of
    /// the dataset whose main store is `main`, under `access`, what the caller may read
    /// there. The record must be in exactly one graph that the evidence settings give a
    /// role and the evidence owner currently protects, have one of the requested kinds,
    /// and carry one publication receipt that names this dataset and one of its
    /// branches. A passage's selector is verified against its rendition's retained text.
    pub fn record(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        request: &RecordRequest,
    ) -> Result<VerifiedRecord> {
        let (view_key, index) = self.prepare_record_view(key, main, logical, access)?;
        let (verified, _) = self.record_in_view(
            key,
            view_key,
            index,
            main,
            &request.iri,
            &request.kinds,
            request.content_digest.as_ref(),
            SelectionRequirements::default(),
            None,
            None,
        )?;
        Ok(verified)
    }

    /// Verify a record reference as [`Self::record`], borrowing `iri` and returning a
    /// [`Reservation`] that keeps the variable-sized strings inside [`VerifiedRecord`]
    /// charged to this resolver's preparation allowance until dropped.
    #[allow(clippy::too_many_arguments)]
    pub fn record_charged(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        iri: &NamedNode,
        kinds: &[RecordKind],
        expected_content_digest: Option<&Sha256Digest>,
    ) -> Result<(VerifiedRecord, Reservation)> {
        let (view_key, index) = self.prepare_record_view(key, main, logical, access)?;
        let mut held = self
            .preparation
            .reserve(std::mem::size_of::<VerifiedRecord>() as u64)
            .map_err(|e| classify(key, e))?;
        let (record, _) = self.record_in_view(
            key,
            view_key,
            index,
            main,
            iri,
            kinds,
            expected_content_digest,
            SelectionRequirements::default(),
            None,
            Some(&mut held),
        )?;
        Ok((record, held))
    }

    /// Verify a record reference without allocating an owned [`RecordRequest`] or
    /// returning a [`VerifiedRecord`].
    pub fn verify_record(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        iri: &NamedNode,
        kinds: &[RecordKind],
    ) -> Result<()> {
        self.verify_record_with_selection(
            key,
            main,
            logical,
            access,
            iri,
            kinds,
            SelectionRequirements::default(),
        )
    }

    /// Verify a record reference without allocating an owned [`RecordRequest`] or
    /// returning a [`VerifiedRecord`], and also enforce any selection-target availability
    /// predicates in `selection`.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_record_with_selection(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        iri: &NamedNode,
        kinds: &[RecordKind],
        selection: SelectionRequirements,
    ) -> Result<()> {
        let (view_key, index) = self.prepare_record_view(key, main, logical, access)?;
        let _ = self.record_in_view(
            key, view_key, index, main, iri, kinds, None, selection, None, None,
        )?;
        Ok(())
    }

    /// Probe a record reference and its selection-target availability without
    /// reconstructing a compatible retained literal (`ev:normalizedText` or
    /// `ev:retainedBytes`) when a single `Vocab`/`Delta` object ID is present, so a
    /// subsequent Phase 2 verifier reads each retained literal only once. When `kinds`
    /// conflicts with a selected record's actual kind, literal shape is verified
    /// immediately so unavailable or masked selections still precede kind conflicts.
    #[allow(clippy::too_many_arguments)]
    pub fn probe_record_with_selection(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        iri: &NamedNode,
        kinds: &[RecordKind],
        selection: SelectionRequirements,
    ) -> Result<SelectionProbe> {
        self.probe_record_with_selection_at(key, main, logical, access, iri, kinds, selection, 0)
    }

    /// Like [`probe_record_with_selection`](Self::probe_record_with_selection), recording
    /// `req_idx` for any deferred existing passage so fallback checks preserve exact
    /// Phase 1 ordering across proposed passages, existing passages and structured
    /// references.
    #[allow(clippy::too_many_arguments)]
    pub fn probe_record_with_selection_at(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        iri: &NamedNode,
        kinds: &[RecordKind],
        selection: SelectionRequirements,
        req_idx: usize,
    ) -> Result<SelectionProbe> {
        let (view_key, index) = self.prepare_record_view(key, main, logical, access)?;
        let (_, probe) = self.record_in_view(
            key,
            view_key,
            index,
            main,
            iri,
            kinds,
            None,
            selection,
            Some(req_idx),
            None,
        )?;
        Ok(probe)
    }

    /// Smallest Phase 1 requirement index among deferred existing passages that have not
    /// yet been verified or consumed.
    pub fn next_pending_passage_req_idx(&self) -> Option<usize> {
        self.record_graphs
            .as_ref()?
            .pending_passages
            .values()
            .filter_map(|list| list.first().map(|p| p.req_idx))
            .min()
    }

    /// Verify and consume the earliest deferred existing passage (smallest Phase 1
    /// `req_idx`), sharing a single `ev:normalizedText` scan across all pending passages
    /// that target the same rendition.
    pub fn verify_next_pending_passage(
        &mut self,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
    ) -> Phase2Outcome {
        let Some((target_view_key, target_index, front_key, front_passage, front_lineage)) =
            self.record_graphs.as_ref().and_then(|rg| {
                rg.pending_passages
                    .iter()
                    .filter_map(|(&(vk, idx), list)| {
                        list.first().map(|p| {
                            (
                                p.req_idx,
                                vk,
                                idx,
                                p.key.clone(),
                                p.passage.clone(),
                                p.passage_lineage,
                            )
                        })
                    })
                    .min_by_key(|(req_idx, _, _, _, _, _)| *req_idx)
                    .map(|(_, vk, idx, k, p, lin)| (vk, idx, k, p, lin))
            })
        else {
            return Phase2Outcome {
                result: Ok(()),
                shape_confirmed: true,
                failed_req_idx: None,
            };
        };
        let state = main.managed_state_shared();
        let state_current = self.record_graphs.as_ref().is_some_and(|r| {
            r.dataset == main.dataset_id()
                && match (&r.state, &state) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                }
        });
        if !state_current {
            // Management state changed since Phase 1 probing; discard stale pending state
            // and re-verify eagerly under the new state.
            let res = self.verify_record_with_selection(
                &front_key,
                main,
                logical,
                access,
                &front_passage,
                &[RecordKind::Passage],
                SelectionRequirements {
                    passage_lineage: front_lineage,
                    ..Default::default()
                },
            );
            return Phase2Outcome {
                result: res,
                shape_confirmed: false,
                failed_req_idx: None,
            };
        }
        let (view_key, index) = match self.prepare_record_view(&front_key, main, logical, access) {
            Ok(v) => v,
            Err(e) => {
                return Phase2Outcome {
                    result: Err(e),
                    shape_confirmed: false,
                    failed_req_idx: None,
                };
            }
        };
        if (view_key, index) != (target_view_key, target_index) {
            // Snapshot or access changed; remove this pending entry and verify eagerly.
            if let Some(rg) = self.record_graphs.as_mut()
                && let Some(list) = rg
                    .pending_passages
                    .get_mut(&(target_view_key, target_index))
                && !list.is_empty()
            {
                list.remove(0);
            }
            let res = self.verify_record_with_selection(
                &front_key,
                main,
                logical,
                access,
                &front_passage,
                &[RecordKind::Passage],
                SelectionRequirements {
                    passage_lineage: front_lineage,
                    ..Default::default()
                },
            );
            return Phase2Outcome {
                result: res,
                shape_confirmed: false,
                failed_req_idx: None,
            };
        }
        let (already_outcome, rendition, rendition_graph) = {
            let rg = self.record_graphs.as_ref().expect("prepared");
            let p = &rg.pending_passages[&(view_key, index)][0];
            (
                p.outcome,
                p.span.rendition.clone(),
                p.rendition_graph.clone(),
            )
        };
        let mut shape_confirmed = false;
        if already_outcome.is_none() {
            let scan_res = self.scan_pending_passages_for_rendition(
                &front_key,
                view_key,
                index,
                &rendition,
                &rendition_graph,
                &mut shape_confirmed,
            );
            if let Err(e) = scan_res {
                return Phase2Outcome {
                    result: Err(e),
                    shape_confirmed,
                    failed_req_idx: None,
                };
            }
        }
        let consumed = {
            let rg = self.record_graphs.as_mut().expect("prepared");
            let list = rg
                .pending_passages
                .get_mut(&(view_key, index))
                .expect("pending list exists");
            list.remove(0)
        };
        match consumed.outcome.expect("outcome populated by scan") {
            PendingOutcome::Valid => {
                let cache_res = self.cache_verified_passage(
                    &consumed.key,
                    view_key,
                    index,
                    &consumed.passage,
                    &consumed.graph,
                    &consumed.rendition_graph,
                    &consumed.span,
                );
                Phase2Outcome {
                    result: cache_res,
                    shape_confirmed: true,
                    failed_req_idx: None,
                }
            }
            PendingOutcome::Unavailable => Phase2Outcome {
                result: Err(unavailable(&consumed.key)),
                shape_confirmed: false,
                failed_req_idx: None,
            },
            PendingOutcome::Conflict(msg) => Phase2Outcome {
                result: Err(conflict(&consumed.key, msg)),
                shape_confirmed: false,
                failed_req_idx: None,
            },
            PendingOutcome::Span(why) => Phase2Outcome {
                result: Err(span(&consumed.key, why)),
                shape_confirmed: true,
                failed_req_idx: None,
            },
        }
    }

    /// Scan `rendition`'s `ev:normalizedText` once to populate `outcome` for every
    /// unresolved `PendingPassage` targeting `rendition` in `(view_key, index)`.
    fn scan_pending_passages_for_rendition(
        &mut self,
        key: &str,
        view_key: ViewKey,
        index: usize,
        rendition: &NamedNode,
        rendition_graph: &NamedNode,
        shape_confirmed: &mut bool,
    ) -> Result<()> {
        let cached_shape = self
            .record_graphs
            .as_ref()
            .and_then(|rg| rg.verified_literal_shapes.get(&(view_key, index)))
            .and_then(|m| m.get(rendition.as_str()))
            .copied();
        if let Some((RecordKind::Rendition, CachedLiteralShape::Unavailable)) = cached_shape {
            self.set_pending_outcomes_for_rendition(
                view_key,
                index,
                rendition,
                PendingOutcome::Unavailable,
            );
            return Ok(());
        }
        if let Some((RecordKind::Rendition, CachedLiteralShape::Conflict(msg))) = cached_shape {
            self.set_pending_outcomes_for_rendition(
                view_key,
                index,
                rendition,
                PendingOutcome::Conflict(msg),
            );
            return Ok(());
        }
        let outcome = {
            let cached = &self.views[&view_key][index];
            let rg = self.record_graphs.as_ref().expect("prepared");
            let list = rg
                .pending_passages
                .get(&(view_key, index))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let mut lookups =
                Lookups::new(&self.preparation, &cached.view).map_err(|e| classify(key, e))?;
            let preparation = &self.preparation;
            lookups
                .one_text(
                    rendition.as_str(),
                    &format!("{EV}normalizedText"),
                    rendition_graph,
                    |text| {
                        *shape_confirmed = true;
                        verify_pending_spans_in_text(text, list, rendition, preparation)
                    },
                )
                .map_err(|e| classify(key, e))?
        };
        match outcome {
            Text::Some(outcomes) => {
                self.cache_literal_shape(
                    view_key,
                    index,
                    rendition,
                    RecordKind::Rendition,
                    CachedLiteralShape::Valid,
                );
                if let Some(rg) = self.record_graphs.as_mut()
                    && let Some(list) = rg.pending_passages.get_mut(&(view_key, index))
                {
                    for (slot, out) in outcomes {
                        if let Some(entry) = list.get_mut(slot) {
                            entry.outcome = Some(out);
                        }
                    }
                }
            }
            Text::Many => {
                let msg = "the rendition has more than one normalized text";
                self.cache_literal_shape(
                    view_key,
                    index,
                    rendition,
                    RecordKind::Rendition,
                    CachedLiteralShape::Conflict(msg),
                );
                self.set_pending_outcomes_for_rendition(
                    view_key,
                    index,
                    rendition,
                    PendingOutcome::Conflict(msg),
                );
            }
            Text::None => {
                self.cache_literal_shape(
                    view_key,
                    index,
                    rendition,
                    RecordKind::Rendition,
                    CachedLiteralShape::Unavailable,
                );
                self.set_pending_outcomes_for_rendition(
                    view_key,
                    index,
                    rendition,
                    PendingOutcome::Unavailable,
                );
            }
        }
        Ok(())
    }

    fn set_pending_outcomes_for_rendition(
        &mut self,
        view_key: ViewKey,
        index: usize,
        rendition: &NamedNode,
        outcome: PendingOutcome,
    ) {
        if let Some(rg) = self.record_graphs.as_mut()
            && let Some(list) = rg.pending_passages.get_mut(&(view_key, index))
        {
            for p in list.iter_mut() {
                if p.span.rendition == *rendition && p.outcome.is_none() {
                    p.outcome = Some(outcome);
                }
            }
        }
    }

    /// Verify the exact vocabulary literal shape of a selected target whose single
    /// `Vocab`/`Delta` object ID was deferred by [`probe_record_with_selection`](Self::probe_record_with_selection).
    #[allow(clippy::too_many_arguments)]
    pub fn verify_selection_literal_shape(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        iri: &NamedNode,
        selection: SelectionRequirements,
    ) -> Result<()> {
        let (view_key, index) = self.prepare_record_view(key, main, logical, access)?;
        if let Some((cached_kind, cached_shape)) = self
            .record_graphs
            .as_ref()
            .and_then(|rg| rg.verified_literal_shapes.get(&(view_key, index)))
            .and_then(|m| m.get(iri.as_str()))
            .copied()
        {
            let applies = (cached_kind == RecordKind::Rendition && selection.rendition_content)
                || (cached_kind == RecordKind::SourceRevision && selection.structured_content);
            if applies {
                return match cached_shape {
                    CachedLiteralShape::Valid => Ok(()),
                    CachedLiteralShape::Unavailable => Err(unavailable(key)),
                    CachedLiteralShape::Conflict(msg) => Err(conflict(key, msg)),
                };
            }
        }
        let (cache_entry, pending_outcomes) = {
            let cached = &self.views[&view_key][index];
            let rg = self.record_graphs.as_ref().expect("prepared");
            let (graphs, _filtered) =
                readable_record_graphs(&self.preparation, &cached.view, &rg.graphs, key)?;
            let mut lookups =
                Lookups::new(&self.preparation, &cached.view).map_err(|e| classify(key, e))?;
            let Some((graph, kind)) = find_record(&mut lookups, &graphs, iri, key)? else {
                return Err(unavailable(key));
            };
            let mut cache_entry: Option<(RecordKind, CachedLiteralShape)> = None;
            let mut pending_outcomes: Option<Vec<(usize, PendingOutcome)>> = None;
            if kind == RecordKind::Rendition && selection.rendition_content {
                let pending_list = rg
                    .pending_passages
                    .get(&(view_key, index))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let has_pending = pending_list
                    .iter()
                    .any(|p| p.span.rendition == *iri && p.outcome.is_none());
                let preparation = &self.preparation;
                match lookups
                    .one_text(
                        iri.as_str(),
                        &format!("{EV}normalizedText"),
                        &graph,
                        |text| {
                            if has_pending {
                                verify_pending_spans_in_text(text, pending_list, iri, preparation)
                                    .map(Some)
                            } else {
                                Ok(None)
                            }
                        },
                    )
                    .map_err(|e| classify(key, e))?
                {
                    Text::Some(outcomes) => {
                        cache_entry = Some((RecordKind::Rendition, CachedLiteralShape::Valid));
                        pending_outcomes = outcomes;
                    }
                    Text::Many => {
                        let msg = "the rendition has more than one normalized text";
                        cache_entry =
                            Some((RecordKind::Rendition, CachedLiteralShape::Conflict(msg)));
                    }
                    Text::None => {
                        cache_entry =
                            Some((RecordKind::Rendition, CachedLiteralShape::Unavailable));
                    }
                }
            }
            if kind == RecordKind::SourceRevision && selection.structured_content {
                match lookups
                    .one_typed_literal(
                        iri.as_str(),
                        &format!("{EV}retainedBytes"),
                        XSD_BASE64_BINARY,
                        &graph,
                        |_| Ok(()),
                    )
                    .map_err(|e| classify(key, e))?
                {
                    Text::Some(()) => {
                        cache_entry = Some((RecordKind::SourceRevision, CachedLiteralShape::Valid));
                    }
                    Text::Many => {
                        let msg = "the revision has more than one retained bytes value";
                        cache_entry = Some((
                            RecordKind::SourceRevision,
                            CachedLiteralShape::Conflict(msg),
                        ));
                    }
                    Text::None => {
                        cache_entry =
                            Some((RecordKind::SourceRevision, CachedLiteralShape::Unavailable));
                    }
                }
            }
            (cache_entry, pending_outcomes)
        };
        if let Some(outcomes) = pending_outcomes
            && let Some(rg) = self.record_graphs.as_mut()
            && let Some(list) = rg.pending_passages.get_mut(&(view_key, index))
        {
            for (slot, out) in outcomes {
                if let Some(entry) = list.get_mut(slot) {
                    entry.outcome = Some(out);
                }
            }
        }
        if let Some((kind, shape)) = cache_entry {
            self.cache_literal_shape(view_key, index, iri, kind, shape);
            if kind == RecordKind::Rendition {
                match shape {
                    CachedLiteralShape::Unavailable => self.set_pending_outcomes_for_rendition(
                        view_key,
                        index,
                        iri,
                        PendingOutcome::Unavailable,
                    ),
                    CachedLiteralShape::Conflict(msg) => self.set_pending_outcomes_for_rendition(
                        view_key,
                        index,
                        iri,
                        PendingOutcome::Conflict(msg),
                    ),
                    CachedLiteralShape::Valid => {}
                }
            }
            return match shape {
                CachedLiteralShape::Valid => Ok(()),
                CachedLiteralShape::Unavailable => Err(unavailable(key)),
                CachedLiteralShape::Conflict(msg) => Err(conflict(key, msg)),
            };
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_in_view(
        &mut self,
        key: &str,
        view_key: ViewKey,
        index: usize,
        main: &Store,
        iri: &NamedNode,
        kinds: &[RecordKind],
        expected_content_digest: Option<&Sha256Digest>,
        selection: SelectionRequirements,
        defer_req_idx: Option<usize>,
        mut result_held: Option<&mut Reservation>,
    ) -> Result<(VerifiedRecord, SelectionProbe)> {
        enum DeferredWork {
            None,
            CachePassage(NamedNode, VerifiedSpan),
            EnqueuePending(PendingPassage),
            MarkPendingOutcome(NamedNode, PendingOutcome),
        }
        struct InViewState {
            auth: AuthenticatedRecord,
            span: Option<VerifiedSpan>,
            probe: SelectionProbe,
            deferred: DeferredWork,
            shape_to_cache: Option<(NamedNode, RecordKind, CachedLiteralShape)>,
            pending_outcomes: Option<Vec<(usize, PendingOutcome)>>,
            err: Option<Error>,
        }
        let state = {
            let cached = &self.views[&view_key][index];
            let rg = self.record_graphs.as_ref().expect("prepared");
            let (graphs, _filtered) =
                readable_record_graphs(&self.preparation, &cached.view, &rg.graphs, key)?;
            let mut lookups =
                Lookups::new(&self.preparation, &cached.view).map_err(|e| classify(key, e))?;
            let Some((graph, kind)) = find_record(&mut lookups, &graphs, iri, key)? else {
                return Err(unavailable(key));
            };
            let wrong_kind = !kinds.is_empty() && !kinds.contains(&kind);
            let selected_kind = (kind == RecordKind::Rendition && selection.rendition_content)
                || (kind == RecordKind::SourceRevision && selection.structured_content)
                || (kind == RecordKind::Passage && selection.passage_lineage);
            // When the found record is itself a selected target (rendition, structured
            // revision or passage), verify its authentication and selection availability
            // before rejecting an incompatible ordinary kind constraint on the same IRI,
            // so masked or metadata-only selections still match absent/denied targets.
            if wrong_kind && !selected_kind {
                return Err(conflict(
                    key,
                    "the record exists with another kind than the reference expects",
                ));
            }
            let auth = authenticate_record_body(
                &mut lookups,
                graph,
                kind,
                &rg.protections,
                iri,
                main,
                key,
                result_held.as_deref_mut(),
            )?;
            if let Some(expected) = expected_content_digest
                && auth.content_digest.as_ref() != Some(expected)
            {
                if auth.content_digest.is_none()
                    && lookups
                        .has_hidden_spg(iri.as_str(), &format!("{EV}contentDigest"), &auth.graph)
                        .map_err(|e| classify(key, e))?
                {
                    return Err(unavailable(key));
                }
                return Err(conflict(
                    key,
                    "the record's content digest differs from the expected digest",
                ));
            }
            let mut probe = SelectionProbe::Complete;
            let mut deferred = DeferredWork::None;
            let mut shape_to_cache: Option<(NamedNode, RecordKind, CachedLiteralShape)> = None;
            let mut pending_outcomes: Option<Vec<(usize, PendingOutcome)>> = None;
            let mut span: Option<VerifiedSpan> = None;
            let mut err: Option<Error> = None;

            if auth.kind == RecordKind::Passage {
                if selection.passage_lineage {
                    match lookups
                        .one(iri.as_str(), &format!("{EV}sourceRevision"), &auth.graph)
                        .map_err(|e| classify(key, e))?
                    {
                        One::Some(Term::NamedNode(_)) => {}
                        One::Many => {
                            return Err(conflict(
                                key,
                                "the passage names more than one source revision",
                            ));
                        }
                        One::None | One::Some(_) => return Err(unavailable(key)),
                    }
                }
                if let Some(hit) = rg
                    .verified_passages
                    .get(&(view_key, index))
                    .and_then(|m| m.get(iri.as_str()))
                {
                    if selection.passage_lineage {
                        match lookups
                            .one(
                                hit.span.rendition.as_str(),
                                PROV_WAS_DERIVED_FROM,
                                &hit.rendition_graph,
                            )
                            .map_err(|e| classify(key, e))?
                        {
                            One::Some(Term::NamedNode(_)) => {}
                            One::Many => {
                                return Err(conflict(
                                    key,
                                    "the rendition names more than one source revision",
                                ));
                            }
                            One::None | One::Some(_) => return Err(unavailable(key)),
                        }
                    }
                    span = Some(hit.span.clone());
                } else if let Some(req_idx) = defer_req_idx
                    && !wrong_kind
                {
                    let existing_pending = rg
                        .pending_passages
                        .get(&(view_key, index))
                        .and_then(|list| list.iter().find(|p| p.passage == *iri));
                    if let Some(pending) = existing_pending {
                        if selection.passage_lineage {
                            match lookups
                                .one(
                                    pending.span.rendition.as_str(),
                                    PROV_WAS_DERIVED_FROM,
                                    &pending.rendition_graph,
                                )
                                .map_err(|e| classify(key, e))?
                            {
                                One::Some(Term::NamedNode(_)) => {}
                                One::Many => {
                                    return Err(conflict(
                                        key,
                                        "the rendition names more than one source revision",
                                    ));
                                }
                                One::None | One::Some(_) => return Err(unavailable(key)),
                            }
                        }
                        span = Some(pending.span.clone());
                        probe = SelectionProbe::DeferredLiteralShape;
                    } else {
                        let meta = prepare_passage_metadata(
                            &mut lookups,
                            &graphs,
                            &rg.protections,
                            &auth.graph,
                            iri,
                            main,
                            key,
                            selection.passage_lineage,
                        )?;
                        match lookups
                            .one_vocab_object_id(
                                meta.span.rendition.as_str(),
                                &format!("{EV}normalizedText"),
                                &meta.rendition_graph,
                            )
                            .map_err(|e| classify(key, e))?
                        {
                            Text::Some(()) => {
                                if meta.span.end < meta.span.start {
                                    return Err(super::resolve::span(
                                        key,
                                        "the passage selector ends before it starts",
                                    ));
                                }
                                span = Some(meta.span.clone());
                                deferred = DeferredWork::EnqueuePending(PendingPassage {
                                    key: key.to_string(),
                                    req_idx,
                                    passage: iri.clone(),
                                    graph: auth.graph.clone(),
                                    rendition_graph: meta.rendition_graph,
                                    span: meta.span,
                                    passage_lineage: selection.passage_lineage,
                                    outcome: None,
                                });
                                probe = SelectionProbe::DeferredLiteralShape;
                            }
                            Text::Many => {
                                return Err(conflict(
                                    key,
                                    "the rendition has more than one normalized text",
                                ));
                            }
                            Text::None => return Err(unavailable(key)),
                        }
                    }
                } else {
                    match verify_passage(
                        &mut lookups,
                        &graphs,
                        &rg.protections,
                        &auth.graph,
                        iri,
                        main,
                        key,
                        selection.passage_lineage,
                    ) {
                        Ok((rendition_graph, verified_span)) => {
                            if defer_req_idx.is_some() {
                                shape_to_cache = Some((
                                    verified_span.rendition.clone(),
                                    RecordKind::Rendition,
                                    CachedLiteralShape::Valid,
                                ));
                            }
                            if wrong_kind {
                                deferred = DeferredWork::MarkPendingOutcome(
                                    iri.clone(),
                                    PendingOutcome::Valid,
                                );
                            } else {
                                deferred = DeferredWork::CachePassage(
                                    rendition_graph,
                                    verified_span.clone(),
                                );
                            }
                            span = Some(verified_span);
                        }
                        Err(e) => {
                            if wrong_kind && defer_req_idx.is_some() {
                                let outcome = match e.code() {
                                    "evidence-unavailable" => Some(PendingOutcome::Unavailable),
                                    "immutable-record-conflict" => Some(PendingOutcome::Conflict(
                                        "the rendition has more than one normalized text",
                                    )),
                                    "span-mismatch" => {
                                        let text = e.to_string();
                                        let why = if text.contains("ends before it starts") {
                                            "the passage selector ends before it starts"
                                        } else if text
                                            .contains("exceeds the rendition's normalized text")
                                        {
                                            "the passage selector exceeds the rendition's normalized text"
                                        } else {
                                            "the passage's text digest does not match the selected text"
                                        };
                                        Some(PendingOutcome::Span(why))
                                    }
                                    _ => None,
                                };
                                if let Some(outcome) = outcome {
                                    deferred =
                                        DeferredWork::MarkPendingOutcome(iri.clone(), outcome);
                                }
                            }
                            err = Some(e);
                        }
                    }
                }
            } else {
                if auth.kind == RecordKind::Rendition && selection.rendition_content {
                    match lookups
                        .one(iri.as_str(), PROV_WAS_DERIVED_FROM, &auth.graph)
                        .map_err(|e| classify(key, e))?
                    {
                        One::Some(Term::NamedNode(_)) => {}
                        One::Many => {
                            return Err(conflict(
                                key,
                                "the rendition names more than one source revision",
                            ));
                        }
                        One::None | One::Some(_) => return Err(unavailable(key)),
                    }
                    let cached_shape = rg
                        .verified_literal_shapes
                        .get(&(view_key, index))
                        .and_then(|m| m.get(iri.as_str()))
                        .copied();
                    if let Some((RecordKind::Rendition, shape)) = cached_shape {
                        match shape {
                            CachedLiteralShape::Valid => {}
                            CachedLiteralShape::Unavailable => err = Some(unavailable(key)),
                            CachedLiteralShape::Conflict(msg) => err = Some(conflict(key, msg)),
                        }
                    } else if defer_req_idx.is_some() && !wrong_kind {
                        match lookups
                            .one_vocab_object_id(
                                iri.as_str(),
                                &format!("{EV}normalizedText"),
                                &auth.graph,
                            )
                            .map_err(|e| classify(key, e))?
                        {
                            Text::Some(()) => probe = SelectionProbe::DeferredLiteralShape,
                            Text::Many => {
                                return Err(conflict(
                                    key,
                                    "the rendition has more than one normalized text",
                                ));
                            }
                            Text::None => return Err(unavailable(key)),
                        }
                    } else {
                        let pending_list = rg
                            .pending_passages
                            .get(&(view_key, index))
                            .map(Vec::as_slice)
                            .unwrap_or(&[]);
                        let has_pending = pending_list
                            .iter()
                            .any(|p| p.span.rendition == *iri && p.outcome.is_none());
                        let preparation = &self.preparation;
                        match lookups
                            .one_text(
                                iri.as_str(),
                                &format!("{EV}normalizedText"),
                                &auth.graph,
                                |text| {
                                    if has_pending {
                                        verify_pending_spans_in_text(
                                            text,
                                            pending_list,
                                            iri,
                                            preparation,
                                        )
                                        .map(Some)
                                    } else {
                                        Ok(None)
                                    }
                                },
                            )
                            .map_err(|e| classify(key, e))?
                        {
                            Text::Some(outcomes) => {
                                pending_outcomes = outcomes;
                                if defer_req_idx.is_some() {
                                    shape_to_cache = Some((
                                        iri.clone(),
                                        RecordKind::Rendition,
                                        CachedLiteralShape::Valid,
                                    ));
                                }
                            }
                            Text::Many => {
                                let msg = "the rendition has more than one normalized text";
                                if defer_req_idx.is_some() {
                                    shape_to_cache = Some((
                                        iri.clone(),
                                        RecordKind::Rendition,
                                        CachedLiteralShape::Conflict(msg),
                                    ));
                                }
                                err = Some(conflict(key, msg));
                            }
                            Text::None => {
                                if defer_req_idx.is_some() {
                                    shape_to_cache = Some((
                                        iri.clone(),
                                        RecordKind::Rendition,
                                        CachedLiteralShape::Unavailable,
                                    ));
                                }
                                err = Some(unavailable(key));
                            }
                        }
                    }
                }
                if err.is_none()
                    && auth.kind == RecordKind::SourceRevision
                    && selection.structured_content
                {
                    if auth.content_digest.is_none() {
                        return Err(unavailable(key));
                    }
                    let cached_shape = rg
                        .verified_literal_shapes
                        .get(&(view_key, index))
                        .and_then(|m| m.get(iri.as_str()))
                        .copied();
                    if let Some((RecordKind::SourceRevision, shape)) = cached_shape {
                        match shape {
                            CachedLiteralShape::Valid => {}
                            CachedLiteralShape::Unavailable => err = Some(unavailable(key)),
                            CachedLiteralShape::Conflict(msg) => err = Some(conflict(key, msg)),
                        }
                    } else if defer_req_idx.is_some() && !wrong_kind {
                        match lookups
                            .one_vocab_object_id(
                                iri.as_str(),
                                &format!("{EV}retainedBytes"),
                                &auth.graph,
                            )
                            .map_err(|e| classify(key, e))?
                        {
                            Text::Some(()) => probe = SelectionProbe::DeferredLiteralShape,
                            Text::Many => {
                                return Err(conflict(
                                    key,
                                    "the revision has more than one retained bytes value",
                                ));
                            }
                            Text::None => return Err(unavailable(key)),
                        }
                    } else {
                        match lookups
                            .one_typed_literal(
                                iri.as_str(),
                                &format!("{EV}retainedBytes"),
                                XSD_BASE64_BINARY,
                                &auth.graph,
                                |_| Ok(()),
                            )
                            .map_err(|e| classify(key, e))?
                        {
                            Text::Some(()) => {
                                if defer_req_idx.is_some() {
                                    shape_to_cache = Some((
                                        iri.clone(),
                                        RecordKind::SourceRevision,
                                        CachedLiteralShape::Valid,
                                    ));
                                }
                            }
                            Text::Many => {
                                let msg = "the revision has more than one retained bytes value";
                                if defer_req_idx.is_some() {
                                    shape_to_cache = Some((
                                        iri.clone(),
                                        RecordKind::SourceRevision,
                                        CachedLiteralShape::Conflict(msg),
                                    ));
                                }
                                err = Some(conflict(key, msg));
                            }
                            Text::None => {
                                if defer_req_idx.is_some() {
                                    shape_to_cache = Some((
                                        iri.clone(),
                                        RecordKind::SourceRevision,
                                        CachedLiteralShape::Unavailable,
                                    ));
                                }
                                err = Some(unavailable(key));
                            }
                        }
                    }
                }
            }
            if err.is_none() && wrong_kind {
                err = Some(conflict(
                    key,
                    "the record exists with another kind than the reference expects",
                ));
            }
            InViewState {
                auth,
                span,
                probe,
                deferred,
                shape_to_cache,
                pending_outcomes,
                err,
            }
        };

        if let Some(outcomes) = state.pending_outcomes
            && let Some(rg) = self.record_graphs.as_mut()
            && let Some(list) = rg.pending_passages.get_mut(&(view_key, index))
        {
            for (slot, out) in outcomes {
                if let Some(entry) = list.get_mut(slot) {
                    entry.outcome = Some(out);
                }
            }
        }
        if let Some((target_iri, target_kind, shape)) = state.shape_to_cache {
            self.cache_literal_shape(view_key, index, &target_iri, target_kind, shape);
            if target_kind == RecordKind::Rendition {
                match shape {
                    CachedLiteralShape::Unavailable => self.set_pending_outcomes_for_rendition(
                        view_key,
                        index,
                        &target_iri,
                        PendingOutcome::Unavailable,
                    ),
                    CachedLiteralShape::Conflict(msg) => self.set_pending_outcomes_for_rendition(
                        view_key,
                        index,
                        &target_iri,
                        PendingOutcome::Conflict(msg),
                    ),
                    CachedLiteralShape::Valid => {}
                }
            }
        }
        match state.deferred {
            DeferredWork::None => {}
            DeferredWork::CachePassage(rendition_graph, verified_span) => {
                self.cache_verified_passage(
                    key,
                    view_key,
                    index,
                    iri,
                    &state.auth.graph,
                    &rendition_graph,
                    &verified_span,
                )?;
            }
            DeferredWork::EnqueuePending(pending) => {
                self.enqueue_pending_passage(key, view_key, index, pending)?;
            }
            DeferredWork::MarkPendingOutcome(passage_iri, outcome) => {
                if let Some(rg) = self.record_graphs.as_mut()
                    && let Some(list) = rg.pending_passages.get_mut(&(view_key, index))
                    && let Some(entry) = list.iter_mut().find(|p| p.passage == passage_iri)
                {
                    entry.outcome = Some(outcome);
                }
            }
        }
        if let Some(e) = state.err {
            return Err(e);
        }
        let iri = if let Some(held) = result_held {
            if let Some(span) = &state.span {
                held.grow_by(span.rendition.as_str().len() as u64)
                    .map_err(|e| classify(key, e))?;
            }
            NamedNode::new_unchecked(
                self.preparation
                    .copy_string(iri.as_str(), held)
                    .map_err(|e| classify(key, e))?,
            )
        } else {
            iri.clone()
        };
        Ok((
            VerifiedRecord {
                iri,
                kind: state.auth.kind,
                graph: state.auth.graph,
                receipt: state.auth.receipt,
                content_digest: state.auth.content_digest,
                record_digest: state.auth.record_digest,
                span: state.span,
            },
            state.probe,
        ))
    }

    fn cache_literal_shape(
        &mut self,
        view_key: ViewKey,
        index: usize,
        iri: &NamedNode,
        kind: RecordKind,
        shape: CachedLiteralShape,
    ) {
        let Some(rg) = self.record_graphs.as_mut() else {
            return;
        };
        let map = rg
            .verified_literal_shapes
            .entry((view_key, index))
            .or_default();
        if let Some(slot) = map.get_mut(iri.as_str()) {
            *slot = (kind, shape);
            return;
        }
        let charge = (iri.as_str().len() as u64).saturating_add(96);
        if rg._held.grow_by(charge).is_ok() {
            map.insert(iri.as_str().to_string(), (kind, shape));
        }
    }

    fn enqueue_pending_passage(
        &mut self,
        key: &str,
        view_key: ViewKey,
        index: usize,
        pending: PendingPassage,
    ) -> Result<()> {
        let rg = self.record_graphs.as_mut().expect("prepared");
        let list = rg.pending_passages.entry((view_key, index)).or_default();
        if list.iter().any(|p| p.passage == pending.passage) {
            return Ok(());
        }
        let charge = (pending.key.len() as u64)
            .saturating_add(pending.passage.as_str().len() as u64)
            .saturating_add(pending.graph.as_str().len() as u64)
            .saturating_add(pending.rendition_graph.as_str().len() as u64)
            .saturating_add(pending.span.rendition.as_str().len() as u64)
            .saturating_add(256);
        rg._held.grow_by(charge).map_err(|e| classify(key, e))?;
        list.push(pending);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn cache_verified_passage(
        &mut self,
        key: &str,
        view_key: ViewKey,
        index: usize,
        passage: &NamedNode,
        graph: &NamedNode,
        rendition_graph: &NamedNode,
        span: &VerifiedSpan,
    ) -> Result<()> {
        let rg = self.record_graphs.as_mut().expect("prepared");
        let charge = (passage.as_str().len() as u64)
            .saturating_add(graph.as_str().len() as u64)
            .saturating_add(rendition_graph.as_str().len() as u64)
            .saturating_add(span.rendition.as_str().len() as u64)
            .saturating_add(256);
        rg._held.grow_by(charge).map_err(|e| classify(key, e))?;
        rg.verified_passages
            .entry((view_key, index))
            .or_default()
            .insert(
                passage.as_str().to_string(),
                CachedPassage {
                    graph: graph.clone(),
                    rendition_graph: rendition_graph.clone(),
                    span: span.clone(),
                },
            );
        Ok(())
    }

    /// Verify one or more proposed passages against an existing authenticated rendition's
    /// source revision (`prov:wasDerivedFrom`) and retained `ev:normalizedText`, in one
    /// scan of the rendition's text under the shared preparation allowance.
    pub fn verify_proposed_passages(
        &mut self,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        rendition: &NamedNode,
        passages: &[ProposedPassage<'_>],
    ) -> Result<()> {
        self.verify_proposed_passages_checked(
            main,
            logical,
            access,
            rendition,
            passages,
            usize::MAX,
        )
        .result
    }

    /// Like [`verify_proposed_passages`](Self::verify_proposed_passages), sharing the
    /// single `ev:normalizedText` scan with any deferred existing passages targeting
    /// `rendition` and reporting whether the literal shape was confirmed before any error.
    pub fn verify_proposed_passages_checked(
        &mut self,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        rendition: &NamedNode,
        passages: &[ProposedPassage<'_>],
        group_req_idx: usize,
    ) -> Phase2Outcome {
        if passages.is_empty() {
            return Phase2Outcome {
                result: Ok(()),
                shape_confirmed: true,
                failed_req_idx: None,
            };
        }
        // Validate caller-input-only span bounds before touching the store so malformed
        // input answers identically whether or not the rendition exists.
        for p in passages {
            if let Err(e) = self.preparation.check() {
                return Phase2Outcome {
                    result: Err(e),
                    shape_confirmed: false,
                    failed_req_idx: None,
                };
            }
            if p.start >= p.end {
                return Phase2Outcome {
                    result: Err(span(p.key, "passage must be nonempty")),
                    shape_confirmed: false,
                    failed_req_idx: None,
                };
            }
        }
        let target_key = rendition.as_str();
        let (view_key, index) = match self.prepare_record_view(target_key, main, logical, access) {
            Ok(v) => v,
            Err(e) => {
                return Phase2Outcome {
                    result: Err(e),
                    shape_confirmed: false,
                    failed_req_idx: None,
                };
            }
        };
        enum PassageOutcome<'a> {
            Ok(Vec<(usize, PendingOutcome)>),
            PendingFailed(Vec<(usize, PendingOutcome)>, usize, String, &'static str),
            Invalid(Vec<(usize, PendingOutcome)>, &'a str),
            Span(Vec<(usize, PendingOutcome)>, &'a str, &'static str),
        }
        let mut shape_confirmed = false;
        let checked = {
            let cached = &self.views[&view_key][index];
            let rg = self.record_graphs.as_ref().expect("prepared");
            let (graphs, _filtered) = match readable_record_graphs(
                &self.preparation,
                &cached.view,
                &rg.graphs,
                target_key,
            ) {
                Ok(v) => v,
                Err(e) => {
                    return Phase2Outcome {
                        result: Err(e),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    };
                }
            };
            let protections = &rg.protections;
            let mut lookups = match Lookups::new(&self.preparation, &cached.view) {
                Ok(lookups) => lookups,
                Err(e) => {
                    return Phase2Outcome {
                        result: Err(classify(target_key, e)),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    };
                }
            };
            let auth = match authenticate_record(
                &mut lookups,
                &graphs,
                protections,
                rendition,
                &[RecordKind::Rendition],
                "the passage's source is not a rendition",
                main,
                target_key,
            ) {
                Ok(a) => a,
                Err(e) => {
                    return Phase2Outcome {
                        result: Err(e),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    };
                }
            };
            let rendition_graph = auth.graph;
            let actual_rev = match lookups
                .one(rendition.as_str(), PROV_WAS_DERIVED_FROM, &rendition_graph)
                .map_err(|e| classify(target_key, e))
            {
                Ok(One::Some(Term::NamedNode(actual_rev))) => actual_rev,
                Ok(One::Many) => {
                    return Phase2Outcome {
                        result: Err(conflict(
                            target_key,
                            "the rendition names more than one source revision",
                        )),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    };
                }
                Ok(One::None | One::Some(_)) => {
                    return Phase2Outcome {
                        result: Err(unavailable(target_key)),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    };
                }
                Err(e) => {
                    return Phase2Outcome {
                        result: Err(e),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    };
                }
            };
            let pending_list = rg
                .pending_passages
                .get(&(view_key, index))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            // Establish that `ev:normalizedText` is present and caller-visible before
            // comparing `actual_rev` against each passage's revision, so a masked or
            // metadata-only rendition answers `evidence-unavailable` identically to an
            // absent rendition even when the supplied revision differs.
            let preparation = &self.preparation;
            match lookups
                .one_text(
                    rendition.as_str(),
                    &format!("{EV}normalizedText"),
                    &rendition_graph,
                    |text| {
                        shape_confirmed = true;
                        let control = preparation.control();
                        let mut pending_count = 0usize;
                        for p in pending_list {
                            if p.span.rendition == *rendition && p.outcome.is_none() {
                                pending_count += 1;
                            }
                        }
                        if pending_count == 0 {
                            for p in passages {
                                control.check()?;
                                if p.revision != &actual_rev {
                                    return Ok(PassageOutcome::Invalid(Vec::new(), p.key));
                                }
                            }
                        }
                        let mut invalid_proposed: Option<&str> = None;
                        if pending_count > 0 {
                            for p in passages {
                                control.check()?;
                                if p.revision != &actual_rev {
                                    invalid_proposed = Some(p.key);
                                    break;
                                }
                            }
                        }
                        let include_proposed = invalid_proposed.is_none();
                        let proposed_count = if include_proposed { passages.len() } else { 0 };
                        let offset_cap = pending_count
                            .saturating_add(proposed_count)
                            .saturating_mul(2);
                        let mut offsets: Vec<u64> = Vec::new();
                        let _offsets_hold = preparation.reserve(
                            (offset_cap as u64).saturating_mul(std::mem::size_of::<u64>() as u64),
                        )?;
                        offsets
                            .try_reserve_exact(offset_cap)
                            .map_err(|_| preparation.exhausted())?;
                        for p in pending_list {
                            if p.span.rendition == *rendition && p.outcome.is_none() {
                                offsets.push(p.span.start);
                                offsets.push(p.span.end);
                            }
                        }
                        if include_proposed {
                            for p in passages {
                                offsets.push(p.start);
                                offsets.push(p.end);
                            }
                        }
                        offsets.sort_unstable();
                        offsets.dedup();
                        let mut positions: Vec<usize> = Vec::new();
                        let _pos_hold = preparation.reserve(
                            (offsets.len() as u64)
                                .saturating_mul(std::mem::size_of::<usize>() as u64),
                        )?;
                        positions
                            .try_reserve_exact(offsets.len())
                            .map_err(|_| preparation.exhausted())?;
                        let mut count = 0u64;
                        for (byte, _) in text.char_indices() {
                            if count.is_multiple_of(16_384) {
                                control.check()?;
                            }
                            if offsets.get(positions.len()) == Some(&count) {
                                positions.push(byte);
                                if positions.len() == offsets.len() {
                                    break;
                                }
                            }
                            count += 1;
                        }
                        if offsets.get(positions.len()) == Some(&count) {
                            positions.push(text.len());
                        }
                        control.check()?;
                        let mut digests: BTreeMap<(usize, usize), Sha256Digest> = BTreeMap::new();
                        let mut digest_hold = preparation.reserve(0)?;
                        let mut pending_outcomes: Vec<(usize, PendingOutcome)> = Vec::new();
                        if pending_count > 0 {
                            let _pending_hold = preparation.reserve(
                                (pending_count as u64).saturating_mul(
                                    std::mem::size_of::<(usize, PendingOutcome)>() as u64,
                                ),
                            )?;
                            pending_outcomes
                                .try_reserve_exact(pending_count)
                                .map_err(|_| preparation.exhausted())?;
                            let mut first_pending_err: Option<(usize, String, &'static str)> = None;
                            for (slot, p) in pending_list.iter().enumerate() {
                                if p.span.rendition != *rendition || p.outcome.is_some() {
                                    continue;
                                }
                                control.check()?;
                                let out = if p.span.end < p.span.start {
                                    PendingOutcome::Span(
                                        "the passage selector ends before it starts",
                                    )
                                } else {
                                    let start_idx = offsets
                                        .binary_search(&p.span.start)
                                        .expect("start offset was grouped");
                                    let end_idx = offsets
                                        .binary_search(&p.span.end)
                                        .expect("end offset was grouped");
                                    match (positions.get(start_idx), positions.get(end_idx)) {
                                        (Some(&start), Some(&end)) => {
                                            let selected = &text[start..end];
                                            let digest = match digests.entry((start, end)) {
                                                std::collections::btree_map::Entry::Occupied(e) => {
                                                    *e.get()
                                                }
                                                std::collections::btree_map::Entry::Vacant(e) => {
                                                    digest_hold.grow_by(128)?;
                                                    *e.insert(Sha256Digest::of_bytes(
                                                        selected.as_bytes(),
                                                        control,
                                                    )?)
                                                }
                                            };
                                            if digest == p.span.text_digest {
                                                PendingOutcome::Valid
                                            } else {
                                                PendingOutcome::Span(
                                                    "the passage's text digest does not match the selected text",
                                                )
                                            }
                                        }
                                        _ => PendingOutcome::Span(
                                            "the passage selector exceeds the rendition's normalized text",
                                        ),
                                    }
                                };
                                if let PendingOutcome::Span(why) = out
                                    && first_pending_err.is_none()
                                {
                                    first_pending_err = Some((p.req_idx, p.key.clone(), why));
                                }
                                pending_outcomes.push((slot, out));
                            }
                            if let Some((req_idx, key, why)) = first_pending_err {
                                return Ok(PassageOutcome::PendingFailed(
                                    pending_outcomes,
                                    req_idx,
                                    key,
                                    why,
                                ));
                            }
                        }
                        if let Some(k) = invalid_proposed {
                            return Ok(PassageOutcome::Invalid(pending_outcomes, k));
                        }
                        for p in passages {
                            control.check()?;
                            let start_idx = offsets
                                .binary_search(&p.start)
                                .expect("start offset was grouped");
                            let end_idx = offsets
                                .binary_search(&p.end)
                                .expect("end offset was grouped");
                            let (Some(&start), Some(&end)) =
                                (positions.get(start_idx), positions.get(end_idx))
                            else {
                                return Ok(PassageOutcome::Span(
                                    pending_outcomes,
                                    p.key,
                                    "the passage selector exceeds the rendition's normalized text",
                                ));
                            };
                            let selected = &text[start..end];
                            if p.quote.is_some_and(|quote| quote != selected) {
                                return Ok(PassageOutcome::Span(
                                    pending_outcomes,
                                    p.key,
                                    "the passage's quote does not match the rendition's normalized text",
                                ));
                            }
                            let digest = match digests.entry((start, end)) {
                                std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                                std::collections::btree_map::Entry::Vacant(entry) => {
                                    digest_hold.grow_by(128)?;
                                    *entry.insert(Sha256Digest::of_bytes(
                                        selected.as_bytes(),
                                        control,
                                    )?)
                                }
                            };
                            if digest != p.text_digest {
                                return Ok(PassageOutcome::Span(
                                    pending_outcomes,
                                    p.key,
                                    "the passage's text digest does not match the selected text",
                                ));
                            }
                        }
                        Ok(PassageOutcome::Ok(pending_outcomes))
                    },
                )
                .map_err(|e| classify(target_key, e))
            {
                Ok(v) => v,
                Err(e) => {
                    return Phase2Outcome {
                        result: Err(e),
                        shape_confirmed,
                        failed_req_idx: None,
                    };
                }
            }
        };
        let promote_pending = |resolver: &mut Self,
                               outcomes: Vec<(usize, PendingOutcome)>,
                               remove_valid: bool|
         -> Result<()> {
            if outcomes.is_empty() {
                return Ok(());
            }
            let mut to_cache: Vec<(String, NamedNode, NamedNode, NamedNode, VerifiedSpan)> =
                Vec::new();
            if let Some(rg) = resolver.record_graphs.as_mut()
                && let Some(list) = rg.pending_passages.get_mut(&(view_key, index))
            {
                for (slot, out) in &outcomes {
                    if let Some(entry) = list.get_mut(*slot) {
                        entry.outcome = Some(*out);
                        if matches!(out, PendingOutcome::Valid) {
                            to_cache.push((
                                entry.key.clone(),
                                entry.passage.clone(),
                                entry.graph.clone(),
                                entry.rendition_graph.clone(),
                                entry.span.clone(),
                            ));
                        }
                    }
                }
                if remove_valid {
                    list.retain(|entry| {
                        !(entry.span.rendition == *rendition
                            && matches!(entry.outcome, Some(PendingOutcome::Valid)))
                    });
                }
            }
            for (k, passage, graph, rendition_graph, span) in to_cache {
                resolver.cache_verified_passage(
                    &k,
                    view_key,
                    index,
                    &passage,
                    &graph,
                    &rendition_graph,
                    &span,
                )?;
            }
            Ok(())
        };
        match checked {
            Text::Some(PassageOutcome::Ok(outcomes)) => {
                self.cache_literal_shape(
                    view_key,
                    index,
                    rendition,
                    RecordKind::Rendition,
                    CachedLiteralShape::Valid,
                );
                let res = promote_pending(self, outcomes, true);
                Phase2Outcome {
                    result: res,
                    shape_confirmed: true,
                    failed_req_idx: None,
                }
            }
            Text::Some(PassageOutcome::PendingFailed(outcomes, req_idx, key, why)) => {
                self.cache_literal_shape(
                    view_key,
                    index,
                    rendition,
                    RecordKind::Rendition,
                    CachedLiteralShape::Valid,
                );
                let _ = promote_pending(self, outcomes, true);
                if let Some(rg) = self.record_graphs.as_mut()
                    && let Some(list) = rg.pending_passages.get_mut(&(view_key, index))
                {
                    list.retain(|entry| {
                        !(entry.span.rendition == *rendition && entry.req_idx == req_idx)
                    });
                }
                Phase2Outcome {
                    result: Err(span(&key, why)),
                    shape_confirmed: false,
                    failed_req_idx: Some(req_idx),
                }
            }
            Text::Some(PassageOutcome::Invalid(outcomes, k)) => {
                self.cache_literal_shape(
                    view_key,
                    index,
                    rendition,
                    RecordKind::Rendition,
                    CachedLiteralShape::Valid,
                );
                let _ = promote_pending(self, outcomes, true);
                Phase2Outcome {
                    result: Err(Error::invalid(format!(
                        "{k}: the passage and its rendition name different revisions"
                    ))),
                    shape_confirmed: true,
                    failed_req_idx: None,
                }
            }
            Text::Some(PassageOutcome::Span(outcomes, k, why)) => {
                self.cache_literal_shape(
                    view_key,
                    index,
                    rendition,
                    RecordKind::Rendition,
                    CachedLiteralShape::Valid,
                );
                let _ = promote_pending(self, outcomes, true);
                Phase2Outcome {
                    result: Err(span(k, why)),
                    shape_confirmed: true,
                    failed_req_idx: None,
                }
            }
            Text::Many => {
                let msg = "the rendition has more than one normalized text";
                self.cache_literal_shape(
                    view_key,
                    index,
                    rendition,
                    RecordKind::Rendition,
                    CachedLiteralShape::Conflict(msg),
                );
                self.set_pending_outcomes_for_rendition(
                    view_key,
                    index,
                    rendition,
                    PendingOutcome::Conflict(msg),
                );
                let earlier_pending = self
                    .record_graphs
                    .as_ref()
                    .and_then(|rg| rg.pending_passages.get(&(view_key, index)))
                    .and_then(|list| {
                        list.iter()
                            .find(|p| p.span.rendition == *rendition && p.req_idx < group_req_idx)
                            .map(|p| (p.req_idx, p.key.clone()))
                    });
                if let Some((req_idx, key)) = earlier_pending {
                    Phase2Outcome {
                        result: Err(conflict(&key, msg)),
                        shape_confirmed: false,
                        failed_req_idx: Some(req_idx),
                    }
                } else {
                    Phase2Outcome {
                        result: Err(conflict(target_key, msg)),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    }
                }
            }
            Text::None => {
                self.cache_literal_shape(
                    view_key,
                    index,
                    rendition,
                    RecordKind::Rendition,
                    CachedLiteralShape::Unavailable,
                );
                self.set_pending_outcomes_for_rendition(
                    view_key,
                    index,
                    rendition,
                    PendingOutcome::Unavailable,
                );
                let earlier_pending = self
                    .record_graphs
                    .as_ref()
                    .and_then(|rg| rg.pending_passages.get(&(view_key, index)))
                    .and_then(|list| {
                        list.iter()
                            .find(|p| p.span.rendition == *rendition && p.req_idx < group_req_idx)
                            .map(|p| (p.req_idx, p.key.clone()))
                    });
                if let Some((req_idx, key)) = earlier_pending {
                    Phase2Outcome {
                        result: Err(unavailable(&key)),
                        shape_confirmed: false,
                        failed_req_idx: Some(req_idx),
                    }
                } else {
                    Phase2Outcome {
                        result: Err(unavailable(target_key)),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    }
                }
            }
        }
    }

    /// Verify that an existing authenticated passage belongs to `expected_revision` and
    /// `expected_rendition`, and that its own selector still verifies against its
    /// rendition's retained text.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_passage_reference(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        passage: &NamedNode,
        expected_revision: &NamedNode,
        expected_rendition: &NamedNode,
    ) -> Result<()> {
        let target_key = passage.as_str();
        let (view_key, index) = self.prepare_record_view(target_key, main, logical, access)?;
        let to_cache = {
            let cached = &self.views[&view_key][index];
            let rg = self.record_graphs.as_ref().expect("prepared");
            let (graphs, _filtered) =
                readable_record_graphs(&self.preparation, &cached.view, &rg.graphs, target_key)?;
            let mut lookups =
                Lookups::new(&self.preparation, &cached.view).map_err(|e| classify(key, e))?;
            let cached_hit = rg
                .verified_passages
                .get(&(view_key, index))
                .and_then(|m| m.get(passage.as_str()));
            let cold = if cached_hit.is_some() {
                None
            } else {
                let auth = authenticate_record(
                    &mut lookups,
                    &graphs,
                    &rg.protections,
                    passage,
                    &[RecordKind::Passage],
                    "the record exists with another kind than the reference expects",
                    main,
                    target_key,
                )?;
                match lookups
                    .one(
                        passage.as_str(),
                        &format!("{EV}sourceRevision"),
                        &auth.graph,
                    )
                    .map_err(|e| classify(target_key, e))?
                {
                    One::Some(Term::NamedNode(_)) => {}
                    One::Many => {
                        return Err(conflict(
                            target_key,
                            "the passage names more than one source revision",
                        ));
                    }
                    One::None | One::Some(_) => return Err(unavailable(target_key)),
                }
                let (rendition_graph, span) = verify_passage(
                    &mut lookups,
                    &graphs,
                    &rg.protections,
                    &auth.graph,
                    passage,
                    main,
                    target_key,
                    true,
                )?;
                Some((auth.graph, rendition_graph, span))
            };
            let (passage_graph, rendition_graph, span_rendition) = match (&cached_hit, &cold) {
                (Some(hit), None) => (&hit.graph, &hit.rendition_graph, &hit.span.rendition),
                (None, Some((pg, rg_node, span))) => (pg, rg_node, &span.rendition),
                _ => unreachable!(),
            };
            // Establish both lineage links as caller-visible before comparing against
            // expected identities so a masked link answers `evidence-unavailable`.
            let actual_rev = match lookups
                .one(
                    passage.as_str(),
                    &format!("{EV}sourceRevision"),
                    passage_graph,
                )
                .map_err(|e| classify(target_key, e))?
            {
                One::Some(Term::NamedNode(actual_rev)) => actual_rev,
                One::Many => {
                    return Err(conflict(
                        target_key,
                        "the passage names more than one source revision",
                    ));
                }
                One::None | One::Some(_) => return Err(unavailable(target_key)),
            };
            let rend_rev = match lookups
                .one(
                    span_rendition.as_str(),
                    PROV_WAS_DERIVED_FROM,
                    rendition_graph,
                )
                .map_err(|e| classify(target_key, e))?
            {
                One::Some(Term::NamedNode(rend_rev)) => rend_rev,
                One::Many => {
                    return Err(conflict(
                        target_key,
                        "the rendition names more than one source revision",
                    ));
                }
                One::None | One::Some(_) => return Err(unavailable(target_key)),
            };
            if &actual_rev != expected_revision
                || span_rendition != expected_rendition
                || &rend_rev != expected_revision
            {
                return Err(Error::invalid(format!(
                    "{key}: the reference names another revision or rendition than its passage"
                )));
            }
            cold
        };
        if let Some((passage_graph, rendition_graph, verified_span)) = to_cache {
            self.cache_verified_passage(
                target_key,
                view_key,
                index,
                passage,
                &passage_graph,
                &rendition_graph,
                &verified_span,
            )?;
        }
        Ok(())
    }

    /// Verify one or more structured JSON Pointer references (`json-lexical-utf8:1`)
    /// against an existing authenticated source revision's retained `ev:retainedBytes`,
    /// decoding base64 and traversing JSON once under the shared preparation allowance.
    pub fn verify_structured_references(
        &mut self,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        revision: &NamedNode,
        references: &[ProposedStructured<'_>],
    ) -> Result<()> {
        self.verify_structured_references_checked(main, logical, access, revision, references)
            .result
    }

    /// Like [`verify_structured_references`](Self::verify_structured_references), reporting
    /// whether the revision's `ev:retainedBytes` (`xsd:base64Binary`) literal shape was
    /// confirmed before any error occurred.
    pub fn verify_structured_references_checked(
        &mut self,
        main: &Store,
        logical: &Store,
        access: Option<&Arc<GraphAccess>>,
        revision: &NamedNode,
        references: &[ProposedStructured<'_>],
    ) -> Phase2Outcome {
        let Some(first) = references.first() else {
            return Phase2Outcome {
                result: Ok(()),
                shape_confirmed: true,
                failed_req_idx: None,
            };
        };
        // Validate RFC 6901 pointer syntax from caller input before reading the store
        // so malformed pointers always answer Invalid (400) rather than span-mismatch.
        for reference in references {
            if let Err(e) =
                super::structured::validate_pointer(reference.pointer, self.preparation.control())
            {
                return Phase2Outcome {
                    result: Err(e),
                    shape_confirmed: false,
                    failed_req_idx: None,
                };
            }
        }
        let target_key = revision.as_str();
        let first_key = first.key;
        let (view_key, index) = match self.prepare_record_view(target_key, main, logical, access) {
            Ok(v) => v,
            Err(e) => {
                return Phase2Outcome {
                    result: Err(e),
                    shape_confirmed: false,
                    failed_req_idx: None,
                };
            }
        };
        let mut shape_confirmed = false;
        let decoded_step = {
            let cached = &self.views[&view_key][index];
            let rg = self.record_graphs.as_ref().expect("prepared");
            let (graphs, _filtered) = match readable_record_graphs(
                &self.preparation,
                &cached.view,
                &rg.graphs,
                target_key,
            ) {
                Ok(v) => v,
                Err(e) => {
                    return Phase2Outcome {
                        result: Err(e),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    };
                }
            };
            let protections = &rg.protections;
            let mut lookups = match Lookups::new(&self.preparation, &cached.view) {
                Ok(lookups) => lookups,
                Err(e) => {
                    return Phase2Outcome {
                        result: Err(classify(target_key, e)),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    };
                }
            };
            let auth = match authenticate_record(
                &mut lookups,
                &graphs,
                protections,
                revision,
                &[RecordKind::SourceRevision],
                "the structured reference's source is not a source revision",
                main,
                target_key,
            ) {
                Ok(a) => a,
                Err(e) => {
                    return Phase2Outcome {
                        result: Err(e),
                        shape_confirmed: false,
                        failed_req_idx: None,
                    };
                }
            };
            let rev_graph = auth.graph;
            let Some(expected_digest) = auth.content_digest else {
                return Phase2Outcome {
                    result: Err(unavailable(target_key)),
                    shape_confirmed: false,
                    failed_req_idx: None,
                };
            };
            enum DecodedBytes {
                Ok(Vec<u8>, Reservation),
                InvalidBase64,
            }
            // Keep only base64 decoding inside `one_typed_literal` so the vocabulary
            // workspace and any `dvocab` read lock are released before SHA-256 hashing
            // and JSON validation/traversal run.
            let preparation = &self.preparation;
            let decoded = match lookups
                .one_typed_literal(
                    revision.as_str(),
                    &format!("{EV}retainedBytes"),
                    XSD_BASE64_BINARY,
                    &rev_graph,
                    |b64| {
                        shape_confirmed = true;
                        let control = preparation.control();
                        control.check()?;
                        let max_b64 = super::RecordKey::MAX_BYTES.div_ceil(3).saturating_mul(4);
                        let estimated = base64::decoded_len_estimate(b64.len());
                        if b64.len() > max_b64 {
                            return Err(Error::BudgetExceeded(Budget {
                                kind: BudgetKind::DecompressedBytes,
                                limit: super::RecordKey::MAX_BYTES as u64,
                                requested: estimated as u64,
                            }));
                        }
                        let decoded_hold = preparation.reserve(estimated as u64)?;
                        let Some(bytes) = decode_base64_controlled(b64, estimated, preparation)?
                        else {
                            return Ok(DecodedBytes::InvalidBase64);
                        };
                        if bytes.len() > super::RecordKey::MAX_BYTES {
                            return Err(Error::BudgetExceeded(Budget {
                                kind: BudgetKind::DecompressedBytes,
                                limit: super::RecordKey::MAX_BYTES as u64,
                                requested: bytes.len() as u64,
                            }));
                        }
                        Ok(DecodedBytes::Ok(bytes, decoded_hold))
                    },
                )
                .map_err(|e| classify(target_key, e))
            {
                Ok(v) => v,
                Err(e) => {
                    return Phase2Outcome {
                        result: Err(e),
                        shape_confirmed,
                        failed_req_idx: None,
                    };
                }
            };
            match decoded {
                Text::Some(DecodedBytes::Ok(bytes, hold)) => Ok((expected_digest, bytes, hold)),
                Text::Some(DecodedBytes::InvalidBase64) => Err((
                    Some(CachedLiteralShape::Valid),
                    conflict(
                        target_key,
                        "the revision's retained bytes are not valid base64",
                    ),
                )),
                Text::Many => {
                    let msg = "the revision has more than one retained bytes value";
                    Err((
                        Some(CachedLiteralShape::Conflict(msg)),
                        conflict(target_key, msg),
                    ))
                }
                Text::None => Err((
                    Some(CachedLiteralShape::Unavailable),
                    unavailable(target_key),
                )),
            }
        };
        let (expected_digest, bytes, _decoded_hold) = match decoded_step {
            Ok(v) => {
                self.cache_literal_shape(
                    view_key,
                    index,
                    revision,
                    RecordKind::SourceRevision,
                    CachedLiteralShape::Valid,
                );
                v
            }
            Err((shape, err)) => {
                if let Some(shape) = shape {
                    self.cache_literal_shape(
                        view_key,
                        index,
                        revision,
                        RecordKind::SourceRevision,
                        shape,
                    );
                }
                return Phase2Outcome {
                    result: Err(err),
                    shape_confirmed,
                    failed_req_idx: None,
                };
            }
        };
        let control = self.preparation.control();
        let digest_res = match Sha256Digest::of_bytes(&bytes, control) {
            Ok(d) => d,
            Err(e) => {
                return Phase2Outcome {
                    result: Err(e),
                    shape_confirmed: true,
                    failed_req_idx: None,
                };
            }
        };
        if digest_res != expected_digest {
            return Phase2Outcome {
                result: Err(span(
                    first_key,
                    "the revision's retained bytes do not match its content digest",
                )),
                shape_confirmed: true,
                failed_req_idx: None,
            };
        }
        let Ok(json) = std::str::from_utf8(&bytes) else {
            return Phase2Outcome {
                result: Err(span(
                    first_key,
                    "the revision's retained bytes are not valid UTF-8 JSON",
                )),
                shape_confirmed: true,
                failed_req_idx: None,
            };
        };
        let mut pointers: Vec<&str> = Vec::new();
        let _ptrs_hold = match self
            .preparation
            .reserve((references.len() as u64).saturating_mul(std::mem::size_of::<&str>() as u64))
            .map_err(|e| classify(target_key, e))
        {
            Ok(h) => h,
            Err(e) => {
                return Phase2Outcome {
                    result: Err(e),
                    shape_confirmed: true,
                    failed_req_idx: None,
                };
            }
        };
        if pointers.try_reserve_exact(references.len()).is_err() {
            return Phase2Outcome {
                result: Err(classify(target_key, self.preparation.exhausted())),
                shape_confirmed: true,
                failed_req_idx: None,
            };
        }
        pointers.extend(references.iter().map(|r| r.pointer));
        // `bytes` is already charged in `_decoded_hold`, so add `json.len()` back when
        // deriving the cap for `select_json_lexical_utf8_batch_optional`, which starts
        // its own counter at `source.len()`.
        let mem_limit = self
            .preparation
            .room()
            .saturating_add(json.len() as u64)
            .min(super::structured::MEMORY_LIMIT as u64) as usize;
        let selected = match super::structured::select_json_lexical_utf8_batch_optional(
            json, &pointers, mem_limit, control,
        ) {
            Ok(selected) => selected,
            Err(Error::Invalid(msg)) => {
                let err = if let Some(why) = msg.strip_prefix("span-mismatch: ") {
                    span(first_key, why)
                } else {
                    Error::Invalid(msg)
                };
                return Phase2Outcome {
                    result: Err(err),
                    shape_confirmed: true,
                    failed_req_idx: None,
                };
            }
            Err(e) => {
                return Phase2Outcome {
                    result: Err(classify(target_key, e)),
                    shape_confirmed: true,
                    failed_req_idx: None,
                };
            }
        };
        for (reference, value) in references.iter().zip(selected) {
            if let Err(e) = control.check() {
                return Phase2Outcome {
                    result: Err(e),
                    shape_confirmed: true,
                    failed_req_idx: None,
                };
            }
            let Some(value) = value else {
                return Phase2Outcome {
                    result: Err(span(
                        reference.key,
                        "JSON Pointer does not select a retained value",
                    )),
                    shape_confirmed: true,
                    failed_req_idx: None,
                };
            };
            if value.digest != reference.value_digest {
                return Phase2Outcome {
                    result: Err(span(
                        reference.key,
                        "the selected JSON value's digest does not match valueDigest",
                    )),
                    shape_confirmed: true,
                    failed_req_idx: None,
                };
            }
        }
        Phase2Outcome {
            result: Ok(()),
            shape_confirmed: true,
            failed_req_idx: None,
        }
    }

    /// Prepare, or reuse, the controlled view of `key` under `access`, and return the
    /// serving branch and the view's index among the views of `key`.
    /// Prepare or reuse the controlled view of `logical` (at the pinned commit when set,
    /// or the live snapshot otherwise) under `access`.
    fn logical_view(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: &Arc<GraphAccess>,
    ) -> Result<(ViewKey, usize)> {
        let dataset = main.dataset_id();
        let branch = logical.branch_id();
        let (commit, live_opt) = match self.pinned {
            Some((d, b, c)) if d == dataset && b == branch => (c, None),
            _ => {
                let live = logical.snapshot();
                (live.commit, Some(live))
            }
        };
        let view_key = ViewKey {
            dataset,
            branch,
            commit,
        };
        let (_, index) = self
            .view(view_key, access, |preparation| {
                if let Some(live) = live_opt {
                    return Ok((live, branch, SnapshotCharge::shared()));
                }
                let live = logical.snapshot();
                if live.commit == commit {
                    return Ok((live, branch, SnapshotCharge::shared()));
                }
                let (snapshot, resolved, served_by, charge) = main
                    .snapshot_of_branch_limited_charged(
                        branch,
                        &At::Commit(commit),
                        &preparation.history_options(),
                        preparation.room(),
                    )?;
                if resolved.commit.seq != commit {
                    return Err(Error::NotFound("commit".into()));
                }
                Ok((snapshot, served_by, charge))
            })
            .map_err(|e| classify(key, e))?;
        Ok((view_key, index))
    }

    /// Prepare or reuse the controlled view of `logical` under `access` for `graph`,
    /// returning `Ok(None)` when the caller cannot read `graph` or the store pair does
    /// not match.
    fn graph_view(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: &Arc<GraphAccess>,
        graph: &NamedNode,
    ) -> Result<Option<(ViewKey, usize)>> {
        self.preparation.check()?;
        if main.is_branch()
            || logical.owner_dataset_id() != main.dataset_id()
            || logical.owner_dataset_id() != main.owner_dataset_id()
        {
            return Ok(None);
        }
        let graph_term = Term::NamedNode(graph.clone());
        if !access.may_read(Some(&graph_term)) {
            return Ok(None);
        }
        let (view_key, index) = self.logical_view(key, main, logical, access)?;
        let cached = &self.views[&view_key][index];
        if !cached.view.access().readable(Some(&graph_term)) {
            return Ok(None);
        }
        Ok(Some((view_key, index)))
    }

    fn empty_nodes(&self, key: &str) -> Result<GraphNodes> {
        let held = self.preparation.reserve(0).map_err(|e| classify(key, e))?;
        Ok(GraphNodes {
            nodes: Vec::new(),
            truncated: false,
            _held: held,
        })
    }

    fn decode_named_nodes(
        &self,
        key: &str,
        snap: &Snapshot,
        ids: &[Id],
        truncated: bool,
    ) -> Result<GraphNodes> {
        let mut out_held = self.preparation.reserve(0).map_err(|e| classify(key, e))?;
        let mut nodes: Vec<NamedNode> = Vec::new();
        if !ids.is_empty() {
            out_held
                .grow_by((ids.len() as u64).saturating_mul(std::mem::size_of::<NamedNode>() as u64))
                .map_err(|e| classify(key, e))?;
            nodes
                .try_reserve_exact(ids.len())
                .map_err(|_| classify(key, self.preparation.exhausted()))?;
        }
        for &id in ids {
            self.preparation.check()?;
            let node = self
                .preparation
                .with_term(snap, id, |t| match t {
                    Some(Term::NamedNode(n)) => {
                        let s = self.preparation.copy_string(n.as_str(), &mut out_held)?;
                        Ok(Some(NamedNode::new_unchecked(s)))
                    }
                    _ => Ok(None),
                })
                .map_err(|e| classify(key, e))?;
            if let Some(n) = node {
                nodes.push(n);
            }
        }
        Ok(GraphNodes {
            nodes,
            truncated,
            _held: out_held,
        })
    }

    /// Whether `s p o` is visible in `graph` at the resolving state of `logical` under
    /// `access`, bounded by this resolver's preparation allowance and controls.
    #[allow(clippy::too_many_arguments)]
    pub fn has_triple(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: &Arc<GraphAccess>,
        graph: &NamedNode,
        s: &NamedNode,
        p: &NamedNode,
        o: &Term,
    ) -> Result<bool> {
        let Some((view_key, index)) = self.graph_view(key, main, logical, access, graph)? else {
            return Ok(false);
        };
        let snap = self.views[&view_key][index].view.snapshot();
        let (Some(g), Some(s), Some(p), Some(o)) = (
            self.preparation
                .lookup_iri(snap, graph.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_iri(snap, s.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_iri(snap, p.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_term(snap, o)
                .map_err(|e| classify(key, e))?,
        ) else {
            return Ok(false);
        };
        snap.contains(&[s, p, o, g]).map_err(|e| classify(key, e))
    }

    /// Whether at least one triple `s p ?o` is visible in `graph` at the resolving state of
    /// `logical` under `access`, without decoding any object term.
    #[allow(clippy::too_many_arguments)]
    pub fn has_any_object_in_graph(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: &Arc<GraphAccess>,
        graph: &NamedNode,
        s: &NamedNode,
        p: &NamedNode,
    ) -> Result<bool> {
        let Some((view_key, index)) = self.graph_view(key, main, logical, access, graph)? else {
            return Ok(false);
        };
        let snap = self.views[&view_key][index].view.snapshot();
        let (Some(g), Some(s), Some(p)) = (
            self.preparation
                .lookup_iri(snap, graph.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_iri(snap, s.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_iri(snap, p.as_str())
                .map_err(|e| classify(key, e))?,
        ) else {
            return Ok(false);
        };
        let mut found = false;
        self.preparation
            .scan(snap, Perm::Gspo, &[g.0, s.0, p.0], |_| {
                found = true;
                Ok(false)
            })
            .map_err(|e| classify(key, e))?;
        Ok(found)
    }

    /// Inspect the object(s) of `s p ?o` visible in `graph` at the resolving state of
    /// `logical` under `access`. Calls `parse` while borrowing the single decoded term, so
    /// no variable-sized decoded text is retained past the callback.
    #[allow(clippy::too_many_arguments)]
    pub fn single_object_in_graph<T>(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: &Arc<GraphAccess>,
        graph: &NamedNode,
        s: &NamedNode,
        p: &NamedNode,
        parse: impl FnOnce(Option<&Term>) -> Result<T>,
    ) -> Result<SingleObject<T>> {
        let Some((view_key, index)) = self.graph_view(key, main, logical, access, graph)? else {
            return Ok(SingleObject::None);
        };
        let snap = self.views[&view_key][index].view.snapshot();
        let (Some(g), Some(s), Some(p)) = (
            self.preparation
                .lookup_iri(snap, graph.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_iri(snap, s.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_iri(snap, p.as_str())
                .map_err(|e| classify(key, e))?,
        ) else {
            return Ok(SingleObject::None);
        };
        let mut first: Option<Id> = None;
        let mut many = false;
        self.preparation
            .scan(snap, Perm::Gspo, &[g.0, s.0, p.0], |q| {
                if first.is_none() {
                    first = Some(q[2]);
                    Ok(true)
                } else {
                    many = true;
                    Ok(false)
                }
            })
            .map_err(|e| classify(key, e))?;
        if many {
            return Ok(SingleObject::Many);
        }
        let Some(id) = first else {
            return Ok(SingleObject::None);
        };
        self.preparation
            .with_term(snap, id, |t| Ok(SingleObject::One(parse(t)?)))
            .map_err(|e| classify(key, e))
    }

    /// Named-node objects of `s p ?o` visible in `graph` at the resolving state of
    /// `logical`, collecting at most `limit` results and decrementing `remaining_scans`
    /// for each scanned quad in `graph`. The returned [`GraphNodes`] retains its memory
    /// reservation until dropped.
    #[allow(clippy::too_many_arguments)]
    pub fn objects_in_graph(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: &Arc<GraphAccess>,
        graph: &NamedNode,
        s: &NamedNode,
        p: &NamedNode,
        limit: usize,
        remaining_scans: &mut u64,
    ) -> Result<GraphNodes> {
        let Some((view_key, index)) = self.graph_view(key, main, logical, access, graph)? else {
            return self.empty_nodes(key);
        };
        let snap = self.views[&view_key][index].view.snapshot();
        let (Some(g), Some(s), Some(p)) = (
            self.preparation
                .lookup_iri(snap, graph.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_iri(snap, s.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_iri(snap, p.as_str())
                .map_err(|e| classify(key, e))?,
        ) else {
            return self.empty_nodes(key);
        };
        let mut ids_held = self.preparation.reserve(0).map_err(|e| classify(key, e))?;
        let mut ids: Vec<Id> = Vec::new();
        let mut truncated = false;
        self.preparation
            .scan(snap, Perm::Gspo, &[g.0, s.0, p.0], |q| {
                self.preparation.check()?;
                if *remaining_scans == 0 {
                    return Err(incomplete(key));
                }
                *remaining_scans -= 1;
                if ids.len() >= limit {
                    truncated = true;
                    return Ok(false);
                }
                ids_held.grow_by(std::mem::size_of::<Id>() as u64)?;
                ids.try_reserve(1)
                    .map_err(|_| self.preparation.exhausted())?;
                ids.push(q[2]);
                Ok(true)
            })
            .map_err(|e| classify(key, e))?;
        self.decode_named_nodes(key, snap, &ids, truncated)
    }

    /// Named-node subjects of `?s p o` visible in `graph` at the resolving state of
    /// `logical`, collecting at most `limit` results and decrementing `remaining_scans`
    /// only for quads belonging to `graph` (so rows in unreadable graphs cannot alter
    /// the caller-visible scan-budget outcome while physical traversal stays bounded by
    /// the preparation's cancellation and deadline). The returned [`GraphNodes`] retains
    /// its memory reservation until dropped.
    #[allow(clippy::too_many_arguments)]
    pub fn subjects_in_graph(
        &mut self,
        key: &str,
        main: &Store,
        logical: &Store,
        access: &Arc<GraphAccess>,
        graph: &NamedNode,
        p: &NamedNode,
        o: &Term,
        limit: usize,
        remaining_scans: &mut u64,
    ) -> Result<GraphNodes> {
        let Some((view_key, index)) = self.graph_view(key, main, logical, access, graph)? else {
            return self.empty_nodes(key);
        };
        let snap = self.views[&view_key][index].view.snapshot();
        let (Some(g), Some(p), Some(o)) = (
            self.preparation
                .lookup_iri(snap, graph.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_iri(snap, p.as_str())
                .map_err(|e| classify(key, e))?,
            self.preparation
                .lookup_term(snap, o)
                .map_err(|e| classify(key, e))?,
        ) else {
            return self.empty_nodes(key);
        };
        let mut ids_held = self.preparation.reserve(0).map_err(|e| classify(key, e))?;
        let mut ids: Vec<Id> = Vec::new();
        let mut truncated = false;
        self.preparation
            .scan(snap, Perm::Pos, &[p.0, o.0], |q| {
                self.preparation.check()?;
                if q[3] != g {
                    return Ok(true);
                }
                if *remaining_scans == 0 {
                    return Err(incomplete(key));
                }
                *remaining_scans -= 1;
                if ids.len() >= limit {
                    truncated = true;
                    return Ok(false);
                }
                ids_held.grow_by(std::mem::size_of::<Id>() as u64)?;
                ids.try_reserve(1)
                    .map_err(|_| self.preparation.exhausted())?;
                ids.push(q[0]);
                Ok(true)
            })
            .map_err(|e| classify(key, e))?;
        self.decode_named_nodes(key, snap, &ids, truncated)
    }

    /// Prepare, or reuse, the controlled view of `key` under `access`, and return the
    /// serving branch and the view's index among the views of `key`.
    fn view(
        &mut self,
        key: ViewKey,
        access: &Arc<GraphAccess>,
        read: impl FnOnce(&Preparation) -> Result<(Arc<Snapshot>, Uuid, SnapshotCharge)>,
    ) -> Result<(Uuid, usize)> {
        if let Some(views) = self.views.get(&key)
            && let Some(index) = views
                .iter()
                .position(|c| Arc::ptr_eq(&c.access, access) || *c.access == **access)
        {
            return Ok((views[index].served_by, index));
        }
        let (snapshot, served_by, charge) = read(&self.preparation)?;
        // The read was checked against the remaining allowance; what the request
        // retains (a shared handle or size-based delta charge, plus any non-live
        // generation charge) stays charged while the view lives.
        let retained = self.preparation.reserve(charge.total())?;
        let view = access.masked_controlled(&snapshot, &self.preparation)?;
        drop(snapshot);
        let views = self.views.entry(key).or_default();
        views.push(Cached {
            access: access.clone(),
            view,
            served_by,
            _retained: retained,
        });
        Ok((served_by, views.len() - 1))
    }
}

enum Missing {
    Unavailable,
    Ambiguous,
    Failed(Error),
}

impl From<Error> for Missing {
    fn from(e: Error) -> Self {
        Missing::Failed(e)
    }
}

/// Check the fact, and its reifier, in the prepared view.
fn check_fact(
    preparation: &Preparation,
    view: &PreparedView,
    graph: Option<&Term>,
    fact: &StoredFactRef,
    blank: bool,
) -> std::result::Result<Option<NamedNode>, Missing> {
    if !view.access().readable(graph) {
        return Err(Missing::Unavailable);
    }
    let snap = view.snapshot();
    let g = match graph {
        None => Id::DEFAULT_GRAPH,
        Some(Term::NamedNode(n)) => preparation
            .lookup_iri(snap, n.as_str())?
            .ok_or(Missing::Unavailable)?,
        Some(_) => return Err(Missing::Unavailable),
    };
    let reifier = match &fact.anchor {
        Some(BlankAnchor::Reifier(r)) => Some(r.clone()),
        _ => None,
    };
    if !blank {
        let quad =
            ground_quad(preparation, snap, &fact.expected, g)?.ok_or(Missing::Unavailable)?;
        if !snap.contains(&quad)? {
            return Err(Missing::Unavailable);
        }
        if let Some(r) = &reifier {
            let r_id = preparation
                .lookup_iri(snap, r.as_str())?
                .ok_or(Missing::Unavailable)?;
            let reifies = preparation
                .lookup_iri(snap, RDF_REIFIES)?
                .ok_or(Missing::Unavailable)?;
            let tt = preparation
                .lookup_term(snap, &Term::Triple(Box::new(fact.expected.clone())))?
                .ok_or(Missing::Unavailable)?;
            if !snap.contains(&[r_id, reifies, tt, g])? {
                return Err(Missing::Unavailable);
            }
        }
        preparation.check()?;
        return Ok(reifier);
    }
    // A blank-containing fact is found through its stable reifier: the one triple
    // term it reifies in the graph must match the expected triple up to a consistent
    // renaming of blank nodes, and that stored triple must be asserted in the graph.
    let r = reifier.clone().ok_or(Missing::Unavailable)?;
    let r_id = preparation
        .lookup_iri(snap, r.as_str())?
        .ok_or(Missing::Unavailable)?;
    let reifies = preparation
        .lookup_iri(snap, RDF_REIFIES)?
        .ok_or(Missing::Unavailable)?;
    let mut reified: Vec<Id> = Vec::new();
    preparation.scan(snap, Perm::Gspo, &[g.0, r_id.0, reifies.0], |q| {
        reified.push(q[2]);
        Ok(reified.len() < 2)
    })?;
    let tt = match reified.as_slice() {
        [] => return Err(Missing::Unavailable),
        [one] => *one,
        _ => return Err(Missing::Ambiguous),
    };
    let stored = preparation.with_term(snap, tt, |term| match term {
        Some(Term::Triple(t)) => Ok(Some((**t).clone())),
        _ => Ok(None),
    })?;
    let stored = stored.ok_or(Missing::Unavailable)?;
    if !isomorphic(&stored, &fact.expected) {
        return Err(Missing::Unavailable);
    }
    let s = stored_id(preparation, snap, &stored.subject.clone().into())?;
    let p = preparation.lookup_iri(snap, stored.predicate.as_str())?;
    let o = stored_id(preparation, snap, &stored.object)?;
    let (Some(s), Some(p), Some(o)) = (s, p, o) else {
        return Err(Missing::Unavailable);
    };
    if !snap.contains(&[s, p, o, g])? {
        return Err(Missing::Unavailable);
    }
    preparation.check()?;
    Ok(reifier)
}

/// The ids of a ground triple in graph `g`, `None` when a term is not in the view.
fn ground_quad(
    preparation: &Preparation,
    snap: &Snapshot,
    triple: &Triple,
    g: Id,
) -> Result<Option<[Id; 4]>> {
    let subject: Term = triple.subject.clone().into();
    let Some(s) = preparation.lookup_term(snap, &subject)? else {
        return Ok(None);
    };
    let Some(p) = preparation.lookup_iri(snap, triple.predicate.as_str())? else {
        return Ok(None);
    };
    let Some(o) = preparation.lookup_term(snap, &triple.object)? else {
        return Ok(None);
    };
    Ok(Some([s, p, o, g]))
}

/// The id of a term decoded from the view, whose blank nodes carry stored labels.
fn stored_id(preparation: &Preparation, snap: &Snapshot, term: &Term) -> Result<Option<Id>> {
    preparation.check()?;
    match term {
        Term::NamedNode(n) => preparation.lookup_iri(snap, n.as_str()),
        Term::BlankNode(_) | Term::Triple(_) => Ok(snap.lookup_term(term)),
        Term::Literal(_) => preparation.lookup_term(snap, term),
    }
}

/// Whether two triples are equal up to a consistent one-to-one renaming of blank nodes.
fn isomorphic(a: &Triple, b: &Triple) -> bool {
    let mut forward: BTreeMap<String, String> = BTreeMap::new();
    let mut backward: BTreeMap<String, String> = BTreeMap::new();
    fn blank(
        x: &str,
        y: &str,
        f: &mut BTreeMap<String, String>,
        r: &mut BTreeMap<String, String>,
    ) -> bool {
        match (f.get(x), r.get(y)) {
            (Some(fy), Some(rx)) => fy == y && rx == x,
            (None, None) => {
                f.insert(x.to_string(), y.to_string());
                r.insert(y.to_string(), x.to_string());
                true
            }
            _ => false,
        }
    }
    fn term(
        a: &Term,
        b: &Term,
        f: &mut BTreeMap<String, String>,
        r: &mut BTreeMap<String, String>,
        depth: usize,
    ) -> bool {
        if depth > crate::nesting::MAX_TRIPLE_TERMS {
            return false;
        }
        match (a, b) {
            (Term::BlankNode(x), Term::BlankNode(y)) => blank(x.as_str(), y.as_str(), f, r),
            (Term::Triple(x), Term::Triple(y)) => triple(x, y, f, r, depth + 1),
            (Term::BlankNode(_), _) | (_, Term::BlankNode(_)) => false,
            (Term::Triple(_), _) | (_, Term::Triple(_)) => false,
            (x, y) => x == y,
        }
    }
    fn triple(
        a: &Triple,
        b: &Triple,
        f: &mut BTreeMap<String, String>,
        r: &mut BTreeMap<String, String>,
        depth: usize,
    ) -> bool {
        let sa: Term = a.subject.clone().into();
        let sb: Term = b.subject.clone().into();
        a.predicate == b.predicate
            && term(&sa, &sb, f, r, depth)
            && term(&a.object, &b.object, f, r, depth)
    }
    triple(a, b, &mut forward, &mut backward, 0)
}

/// The graphs that hold authenticated records: every graph the evidence settings give
/// a record role, other than the accepted projections, that the evidence owner protects
/// now. None when the profile is not configured or its registry cannot be enforced.
fn record_graphs(
    state: Option<&crate::managed::ManagedState>,
    preparation: &Preparation,
) -> Result<(Vec<NamedNode>, BTreeMap<String, Uuid>, Reservation)> {
    use crate::managed::ManagedState;
    let mut held = preparation.reserve(0)?;
    let mut protections = BTreeMap::new();
    // The registry is borrowed from the running state, never copied.
    let Some(ManagedState::Installed(installed)) = state else {
        return Ok((Vec::new(), protections, held));
    };
    let Some(bytes) = installed.settings.get(&OwnerKind::Evidence) else {
        return Ok((Vec::new(), protections, held));
    };
    // Decoding copies the settings' strings into its own maps and vectors.
    let decoding = preparation.reserve(
        (bytes.len() as u64)
            .saturating_mul(4)
            .saturating_add(64 << 10),
    )?;
    let settings = match super::Settings::from_slice(bytes, preparation.control()) {
        Ok(settings) => settings,
        Err(e @ (Error::Cancelled | Error::Timeout)) => return Err(e),
        // Settings this build cannot read authenticate no record.
        Err(_) => return Ok((Vec::new(), protections, held)),
    };
    if !settings.enabled {
        return Ok((Vec::new(), protections, held));
    }
    let entry = installed.graphs.owners.get(&OwnerKind::Evidence);
    let protected: &[String] = entry.map_or(&[], |e| &e.graphs);
    let mut sorted: Vec<&str> = Vec::new();
    held.grow_by((protected.len() as u64).saturating_mul(16))?;
    sorted
        .try_reserve_exact(protected.len())
        .map_err(|_| preparation.exhausted())?;
    sorted.extend(protected.iter().map(String::as_str));
    crate::access::sort_by_prepared(&mut sorted, preparation, |a, b| preparation.compare(a, b))?;
    let mut out: Vec<NamedNode> = Vec::new();
    for graph in settings
        .source_records
        .iter()
        .chain(&settings.proposals)
        .chain(&settings.decisions)
        .chain(&settings.supports)
        .chain(&settings.identity)
    {
        preparation.check()?;
        let mut failure = None;
        let found = sorted.binary_search_by(|p| match preparation.compare(p, graph) {
            Ok(order) => order,
            Err(e) => {
                failure.get_or_insert(e);
                std::cmp::Ordering::Equal
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        if found.is_ok() && !out.iter().any(|n| n.as_str() == graph) {
            held.grow_by((graph.len() as u64).saturating_mul(2).saturating_add(128))?;
            if let Ok(node) = NamedNode::new(graph.as_str()) {
                out.push(node);
                if let Some(nonce) = entry.and_then(|e| e.protections.get(graph)) {
                    protections.insert(graph.clone(), *nonce);
                }
            }
        }
    }
    out.sort();
    drop(decoding);
    Ok((out, protections, held))
}

/// Filter `all_graphs` to those readable by `view`, charging the borrowed list to
/// `preparation`.
fn readable_record_graphs<'a>(
    preparation: &Preparation,
    view: &PreparedView,
    all_graphs: &'a [NamedNode],
    key: &str,
) -> Result<(Vec<&'a NamedNode>, Reservation)> {
    let mut graphs: Vec<&'a NamedNode> = Vec::new();
    let filtered = preparation
        .reserve((all_graphs.len() as u64).saturating_mul(8))
        .map_err(|e| classify(key, e))?;
    graphs
        .try_reserve_exact(all_graphs.len())
        .map_err(|_| classify(key, preparation.exhausted()))?;
    let bound = view.access();
    for g in all_graphs {
        preparation.check()?;
        if bound.readable(Some(&Term::NamedNode(g.clone()))) {
            graphs.push(g);
        }
    }
    if graphs.is_empty() {
        return Err(unavailable(key));
    }
    Ok((graphs, filtered))
}

#[cfg(test)]
std::thread_local! {
    static PASSAGE_TEXT_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CANCEL_BASE64_AT_CHUNK: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[cfg(any(test, feature = "failpoints"))]
std::thread_local! {
    static SIMPLE_LITERAL_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TYPED_LITERAL_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, feature = "failpoints"))]
#[doc(hidden)]
pub fn reset_literal_read_counts() {
    SIMPLE_LITERAL_READS.with(|c| c.set(0));
    TYPED_LITERAL_READS.with(|c| c.set(0));
}

#[cfg(any(test, feature = "failpoints"))]
#[doc(hidden)]
pub fn literal_read_counts() -> (usize, usize) {
    (
        SIMPLE_LITERAL_READS.with(std::cell::Cell::get),
        TYPED_LITERAL_READS.with(std::cell::Cell::get),
    )
}

/// Decode standard base64 in 64 KiB chunks under `preparation` controls into a
/// pre-reserved buffer. Returns `Ok(None)` on malformed base64.
fn decode_base64_controlled(
    b64: &str,
    estimated: usize,
    preparation: &Preparation,
) -> Result<Option<Vec<u8>>> {
    let raw = b64.as_bytes();
    if !raw.len().is_multiple_of(4) {
        return Ok(None);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(estimated)
        .map_err(|_| preparation.exhausted())?;
    const CHUNK: usize = 64 * 1024;
    let total_chunks = raw.len().div_ceil(CHUNK);
    for (idx, chunk) in raw.chunks(CHUNK).enumerate() {
        #[cfg(test)]
        if CANCEL_BASE64_AT_CHUNK.with(|at| at.get() == Some(idx)) {
            preparation.control().cancel.cancel();
        }
        preparation.check()?;
        if idx + 1 < total_chunks && chunk.ends_with(b"=") {
            return Ok(None);
        }
        if base64::engine::general_purpose::STANDARD
            .decode_vec(chunk, &mut out)
            .is_err()
        {
            return Ok(None);
        }
    }
    preparation.check()?;
    Ok(Some(out))
}

/// Controlled lookups in one prepared view, with the view's graph policy applied.
/// Every memoized IRI key stays charged to `held` for the lifetime of this struct.
struct Lookups<'a> {
    preparation: &'a Preparation,
    view: &'a PreparedView,
    ids: BTreeMap<String, Option<Id>>,
    held: Reservation,
}

impl<'a> Lookups<'a> {
    fn new(preparation: &'a Preparation, view: &'a PreparedView) -> Result<Self> {
        Ok(Self {
            preparation,
            view,
            ids: BTreeMap::new(),
            held: preparation.reserve(0)?,
        })
    }

    fn snap(&self) -> &'a Snapshot {
        self.view.snapshot()
    }

    fn iri(&mut self, iri: &str) -> Result<Option<Id>> {
        if let Some(id) = self.ids.get(iri) {
            return Ok(*id);
        }
        let id = self.preparation.lookup_iri(self.snap(), iri)?;
        let owned = self.preparation.copy_string(iri, &mut self.held)?;
        self.held.grow_by(64)?;
        self.ids.insert(owned, id);
        Ok(id)
    }

    fn readable(&self, graph: &NamedNode) -> bool {
        self.view
            .access()
            .readable(Some(&Term::NamedNode(graph.clone())))
    }

    /// Whether `s p o` is in `graph`.
    fn has(&mut self, s: &str, p: &str, o: &str, graph: &NamedNode) -> Result<bool> {
        if !self.readable(graph) {
            return Ok(false);
        }
        let (Some(s), Some(p), Some(o), Some(g)) = (
            self.iri(s)?,
            self.iri(p)?,
            self.iri(o)?,
            self.iri(graph.as_str())?,
        ) else {
            return Ok(false);
        };
        self.snap().contains(&[s, p, o, g])
    }

    /// Whether the view's triple mask hid at least one quad matching `(s, p, ?, graph)`.
    fn has_hidden_spg(&mut self, s: &str, p: &str, graph: &NamedNode) -> Result<bool> {
        if self.snap().mask.is_none() || !self.readable(graph) {
            return Ok(false);
        }
        let (Some(s), Some(p), Some(g)) = (self.iri(s)?, self.iri(p)?, self.iri(graph.as_str())?)
        else {
            return Ok(false);
        };
        self.has_hidden_object(s, p, g)
    }

    fn has_hidden_object(&self, s: Id, p: Id, g: Id) -> Result<bool> {
        let Some(mask) = self.snap().mask.as_deref() else {
            return Ok(false);
        };
        let start = mask.hidden.partition_point(|k| (k[0], k[1]) < (s.0, p.0));
        for (i, quad) in mask.hidden[start..].iter().enumerate() {
            if i.is_multiple_of(4096) {
                self.preparation.check()?;
            }
            if quad[0] != s.0 || quad[1] != p.0 {
                break;
            }
            if quad[3] == g.0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Up to two objects of `s p ?o` in `graph`, decoded.
    /// Up to two object ids of `s p ?o` in graph `g`.
    fn object_ids(&self, s: Id, p: Id, g: Id) -> Result<Vec<Id>> {
        let mut ids = Vec::new();
        self.preparation
            .scan(self.snap(), Perm::Gspo, &[g.0, s.0, p.0], |q| {
                ids.push(q[2]);
                Ok(ids.len() < 2)
            })?;
        Ok(ids)
    }

    /// Check via the GSPO index alone that `s p ?o` in `graph` has a single object
    /// whose term ID is stored in `Vocab` or `Delta` (the only storage tags that can
    /// hold a simple literal or `xsd:base64Binary` typed literal), without reading or
    /// reconstructing the vocabulary key.
    fn one_vocab_object_id(&mut self, s: &str, p: &str, graph: &NamedNode) -> Result<Text<()>> {
        if !self.readable(graph) {
            return Ok(Text::None);
        }
        let (Some(s), Some(p), Some(g)) = (self.iri(s)?, self.iri(p)?, self.iri(graph.as_str())?)
        else {
            return Ok(Text::None);
        };
        match self.object_ids(s, p, g)?.as_slice() {
            [] => Ok(Text::None),
            [id] => match id.tag() {
                crate::id::Tag::Vocab | crate::id::Tag::Delta => Ok(Text::Some(())),
                _ => Ok(Text::None),
            },
            _ => Ok(Text::Many),
        }
    }

    /// The one object of `s p ?o` in `graph`, read as a stored simple literal's text
    /// without decoding it as a term.
    fn one_text<R>(
        &mut self,
        s: &str,
        p: &str,
        graph: &NamedNode,
        each: impl FnOnce(&str) -> Result<R>,
    ) -> Result<Text<R>> {
        if !self.readable(graph) {
            return Ok(Text::None);
        }
        let (Some(s), Some(p), Some(g)) = (self.iri(s)?, self.iri(p)?, self.iri(graph.as_str())?)
        else {
            return Ok(Text::None);
        };
        match self.object_ids(s, p, g)?.as_slice() {
            [] => Ok(Text::None),
            [id] => {
                #[cfg(any(test, feature = "failpoints"))]
                SIMPLE_LITERAL_READS.with(|count| count.set(count.get() + 1));
                self.preparation
                    .with_simple_literal(self.snap(), *id, |text| match text {
                        Some(text) => Ok(Text::Some(each(text)?)),
                        None => Ok(Text::None),
                    })
            }
            _ => Ok(Text::Many),
        }
    }

    /// The one object of `s p ?o` in `graph`, read as a stored typed literal's lexical
    /// text when its datatype equals `datatype`, without decoding it as a term.
    fn one_typed_literal<R>(
        &mut self,
        s: &str,
        p: &str,
        datatype: &str,
        graph: &NamedNode,
        each: impl FnOnce(&str) -> Result<R>,
    ) -> Result<Text<R>> {
        if !self.readable(graph) {
            return Ok(Text::None);
        }
        let (Some(s), Some(p), Some(g)) = (self.iri(s)?, self.iri(p)?, self.iri(graph.as_str())?)
        else {
            return Ok(Text::None);
        };
        match self.object_ids(s, p, g)?.as_slice() {
            [] => Ok(Text::None),
            [id] => {
                #[cfg(any(test, feature = "failpoints"))]
                TYPED_LITERAL_READS.with(|count| count.set(count.get() + 1));
                self.preparation
                    .with_typed_literal(self.snap(), *id, datatype, |text| match text {
                        Some(text) => Ok(Text::Some(each(text)?)),
                        None => Ok(Text::None),
                    })
            }
            _ => Ok(Text::Many),
        }
    }

    /// Inspect up to two matching object IDs before decoding a single term.
    fn one(&mut self, s: &str, p: &str, graph: &NamedNode) -> Result<One> {
        if !self.readable(graph) {
            return Ok(One::None);
        }
        let (Some(s), Some(p), Some(g)) = (self.iri(s)?, self.iri(p)?, self.iri(graph.as_str())?)
        else {
            return Ok(One::None);
        };
        match self.object_ids(s, p, g)?.as_slice() {
            [] => Ok(One::None),
            [id] => self.preparation.with_term(self.snap(), *id, |term| {
                Ok(term.cloned().map_or(One::None, One::Some))
            }),
            _ => Ok(One::Many),
        }
    }
}

enum One {
    None,
    Some(Term),
    Many,
}

enum Text<R> {
    None,
    Some(R),
    Many,
}

/// The graph and kind of `iri` among `graphs`, when it is a profile record there.
fn find_record(
    lookups: &mut Lookups<'_>,
    graphs: &[&NamedNode],
    iri: &NamedNode,
    key: &str,
) -> Result<Option<(NamedNode, RecordKind)>> {
    let mut found: Option<(NamedNode, RecordKind)> = None;
    for graph in graphs {
        for kind in KINDS {
            if lookups
                .has(iri.as_str(), RDF_TYPE, kind_iri(kind), graph)
                .map_err(|e| classify(key, e))?
            {
                if found.is_some() {
                    return Err(conflict(
                        key,
                        "the record is stored more than once or with more than one kind",
                    ));
                }
                found = Some(((*graph).clone(), kind));
            }
        }
    }
    Ok(found)
}

/// The record's one publication receipt in its graph, which must be the receipt of a
/// typed operation published under the graph's current protection `nonce`: the
/// receipt is an `ev:Operation` whose one `ev:graphProtection` there is the nonce.
/// A record written while its graph was detached cannot carry the nonce of the
/// protection in force now, because that nonce was chosen when the graph was protected
/// again, after which nobody but a typed writer could change the graph.
fn receipt(
    lookups: &mut Lookups<'_>,
    graph: &NamedNode,
    iri: &NamedNode,
    key: &str,
    nonce: Option<Uuid>,
    retained: Option<&mut Reservation>,
) -> Result<Receipt> {
    let classify_error = |e| classify(key, e);
    let (Some(graph_id), Some(iri_id)) = (
        lookups.iri(graph.as_str()).map_err(classify_error)?,
        lookups.iri(iri.as_str()).map_err(classify_error)?,
    ) else {
        return Err(origin(key));
    };
    let receipt_pred = format!("{EV}availabilityReceipt");
    let Some(receipt_pred) = lookups.iri(&receipt_pred).map_err(classify_error)? else {
        return Err(origin(key));
    };
    let receipt_id = match lookups
        .object_ids(iri_id, receipt_pred, graph_id)
        .map_err(classify_error)?
        .as_slice()
    {
        [id] => *id,
        [] if lookups
            .has_hidden_object(iri_id, receipt_pred, graph_id)
            .map_err(classify_error)? =>
        {
            return Err(unavailable(key));
        }
        _ => return Err(origin(key)),
    };

    let uuid = |term: Option<&Term>| match term {
        Some(Term::Literal(l)) if l.datatype().as_str() == XSD_STRING => {
            Uuid::parse_str(l.value()).ok()
        }
        _ => None,
    };
    // Check field visibility without loading their terms. A masked field must be
    // indistinguishable from a miss even when another field fails authentication.
    let mut masked = false;
    let mut field_id = |subject, predicate: &str| -> Result<Option<Id>> {
        let Some(predicate) = lookups.iri(predicate)? else {
            return Ok(None);
        };
        match lookups.object_ids(subject, predicate, graph_id)?.as_slice() {
            [id] => Ok(Some(*id)),
            [] => {
                masked |= lookups.has_hidden_object(subject, predicate, graph_id)?;
                Ok(None)
            }
            _ => Ok(None),
        }
    };
    let protection = format!("{EV}graphProtection");
    let record_nonce = field_id(iri_id, &protection).map_err(classify_error)?;
    let receipt_nonce = field_id(receipt_id, &protection).map_err(classify_error)?;
    let dataset = field_id(receipt_id, &format!("{EV}datasetId")).map_err(classify_error)?;
    let branch = field_id(receipt_id, &format!("{EV}branchId")).map_err(classify_error)?;
    let commit = field_id(receipt_id, &format!("{EV}commit")).map_err(classify_error)?;
    let operation_type = lookups
        .iri(&format!("{EV}Operation"))
        .map_err(classify_error)?;
    let type_pred = lookups.iri(RDF_TYPE).map_err(classify_error)?;
    let is_operation = if let (Some(p), Some(o)) = (type_pred, operation_type) {
        let quad = [receipt_id, p, o, graph_id];
        let visible = lookups.snap().contains(&quad).map_err(classify_error)?;
        masked |= !visible
            && lookups
                .snap()
                .mask
                .as_deref()
                .is_some_and(|m| m.hides(&quad.map(|id| id.0)));
        visible
    } else {
        false
    };
    if masked {
        return Err(unavailable(key));
    }
    let (Some(nonce), Some(record_nonce), Some(receipt_nonce)) =
        (nonce, record_nonce, receipt_nonce)
    else {
        return Err(origin(key));
    };
    let read_uuid = |id| {
        lookups
            .preparation
            .with_term(lookups.snap(), id, |term| Ok(uuid(term)))
            .map_err(classify_error)
    };
    if !is_operation
        || read_uuid(record_nonce)? != Some(nonce)
        || read_uuid(receipt_nonce)? != Some(nonce)
    {
        return Err(origin(key));
    }
    let (Some(dataset), Some(branch), Some(commit)) = (dataset, branch, commit) else {
        return Err(origin(key));
    };
    let dataset = read_uuid(dataset)?.ok_or_else(|| origin(key))?;
    let branch = read_uuid(branch)?.ok_or_else(|| origin(key))?;
    let commit = lookups
        .preparation
        .with_term(lookups.snap(), commit, |term| {
            Ok(match term {
                Some(Term::Literal(l)) => integer(l),
                _ => None,
            })
        })
        .map_err(classify_error)?
        .ok_or_else(|| origin(key))?;

    let mut scratch;
    let held = if let Some(held) = retained {
        held
    } else {
        scratch = lookups.preparation.reserve(0).map_err(classify_error)?;
        &mut scratch
    };
    let receipt_iri = lookups
        .preparation
        .with_iri(lookups.snap(), receipt_id, |text| {
            text.map(|text| lookups.preparation.copy_string(text, held))
                .transpose()
        })
        .map_err(classify_error)?
        .ok_or_else(|| origin(key))?;
    Ok(Receipt {
        iri: NamedNode::new_unchecked(receipt_iri),
        dataset,
        branch,
        commit,
    })
}

/// A non-negative `xsd:integer` value, also from a stored inline literal.
pub(super) fn integer(l: &oxrdf::Literal) -> Option<u64> {
    let datatype = l.datatype().as_str();
    if datatype != XSD_INTEGER {
        return None;
    }
    l.value().trim_start_matches('+').parse::<u64>().ok()
}

/// Shared record authentication metadata validated in a bound view.
struct AuthenticatedRecord {
    graph: NamedNode,
    kind: RecordKind,
    receipt: Receipt,
    content_digest: Option<Sha256Digest>,
    record_digest: Option<Sha256Digest>,
}

/// Authenticate a record in one bound view: check presence, expected kind, publication
/// receipt under the graph's current protection nonce, dataset/branch origin, and
/// well-formed unique `ev:contentDigest` / `ev:recordDigest` values.
#[allow(clippy::too_many_arguments)]
fn authenticate_record(
    lookups: &mut Lookups<'_>,
    graphs: &[&NamedNode],
    protections: &BTreeMap<String, Uuid>,
    iri: &NamedNode,
    kinds: &[RecordKind],
    wrong_kind_msg: &'static str,
    main: &Store,
    key: &str,
) -> Result<AuthenticatedRecord> {
    let Some((graph, kind)) = find_record(lookups, graphs, iri, key)? else {
        return Err(unavailable(key));
    };
    if !kinds.is_empty() && !kinds.contains(&kind) {
        return Err(conflict(key, wrong_kind_msg));
    }
    authenticate_record_body(lookups, graph, kind, protections, iri, main, key, None)
}

#[allow(clippy::too_many_arguments)]
/// Complete record authentication once `find_record` has resolved `(graph, kind)`.
fn authenticate_record_body(
    lookups: &mut Lookups<'_>,
    graph: NamedNode,
    kind: RecordKind,
    protections: &BTreeMap<String, Uuid>,
    iri: &NamedNode,
    main: &Store,
    key: &str,
    mut result_held: Option<&mut Reservation>,
) -> Result<AuthenticatedRecord> {
    if let Some(held) = result_held.as_deref_mut() {
        held.grow_by(graph.as_str().len() as u64)
            .map_err(|e| classify(key, e))?;
    }
    let rec = receipt(
        lookups,
        &graph,
        iri,
        key,
        protections.get(graph.as_str()).copied(),
        result_held,
    )?;
    if rec.dataset != main.dataset_id()
        || !main
            .branch_known_prepared(rec.branch, lookups.preparation)
            .map_err(|e| classify(key, e))?
    {
        return Err(origin(key));
    }
    let content_digest = single_digest(lookups, &graph, iri, "contentDigest", key)?;
    let record_digest = single_digest(lookups, &graph, iri, "recordDigest", key)?;
    Ok(AuthenticatedRecord {
        graph,
        kind,
        receipt: rec,
        content_digest,
        record_digest,
    })
}

/// The record's one digest under `property`, refusing more than one value.
fn single_digest(
    lookups: &mut Lookups<'_>,
    graph: &NamedNode,
    iri: &NamedNode,
    property: &str,
    key: &str,
) -> Result<Option<Sha256Digest>> {
    match lookups
        .one(iri.as_str(), &format!("{EV}{property}"), graph)
        .map_err(|e| classify(key, e))?
    {
        One::None => Ok(None),
        One::Some(Term::Literal(l)) => match l.value().parse() {
            Ok(d) => Ok(Some(d)),
            Err(_) => Err(conflict(key, "the record's digest is not a SHA-256 digest")),
        },
        One::Some(_) => Err(conflict(key, "the record's digest is not a SHA-256 digest")),
        One::Many => Err(conflict(key, "the record has more than one digest")),
    }
}

/// Authenticated passage selector and rendition metadata checked before reading the
/// rendition's `ev:normalizedText`.
struct PreparedPassageMeta {
    rendition_graph: NamedNode,
    span: VerifiedSpan,
}

/// Validate an existing passage's selector, text digest, and authenticated rendition
/// metadata without reading the rendition's `ev:normalizedText`.
#[allow(clippy::too_many_arguments)]
fn prepare_passage_metadata(
    lookups: &mut Lookups<'_>,
    graphs: &[&NamedNode],
    protections: &BTreeMap<String, Uuid>,
    graph: &NamedNode,
    iri: &NamedNode,
    main: &Store,
    key: &str,
    require_rendition_revision: bool,
) -> Result<PreparedPassageMeta> {
    let rendition = match lookups
        .one(iri.as_str(), &format!("{OA}hasSource"), graph)
        .map_err(|e| classify(key, e))?
    {
        One::Some(Term::NamedNode(r)) => r,
        One::Many => return Err(conflict(key, "the passage names more than one rendition")),
        One::None => return Err(unavailable(key)),
        One::Some(_) => return Err(span(key, "the passage names no rendition")),
    };
    let selector = match lookups
        .one(iri.as_str(), &format!("{OA}hasSelector"), graph)
        .map_err(|e| classify(key, e))?
    {
        One::Some(Term::NamedNode(s)) => Term::NamedNode(s),
        // A blank selector of the stored record is addressed by its stored label.
        One::Some(Term::BlankNode(b)) => Term::BlankNode(b),
        One::Many => return Err(conflict(key, "the passage has more than one selector")),
        One::None => return Err(unavailable(key)),
        One::Some(_) => return Err(span(key, "the passage has no selector")),
    };
    enum Position {
        None,
        Some(u64),
        Invalid,
        Many,
    }
    let position = |lookups: &mut Lookups<'_>, p: &str| -> Result<Position> {
        let snap = lookups.snap();
        let preparation = lookups.preparation;
        let s = match &selector {
            Term::NamedNode(n) => lookups.iri(n.as_str())?,
            other => snap.lookup_term(other),
        };
        let (Some(s), Some(p), Some(g)) = (
            s,
            lookups.iri(&format!("{OA}{p}"))?,
            lookups.iri(graph.as_str())?,
        ) else {
            return Ok(Position::None);
        };
        let ids = lookups.object_ids(s, p, g)?;
        match ids.as_slice() {
            [] => Ok(Position::None),
            [id] => preparation.with_term(snap, *id, |t| {
                Ok(match t {
                    Some(Term::Literal(l)) => match integer(l) {
                        Some(n) => Position::Some(n),
                        None => Position::Invalid,
                    },
                    _ => Position::Invalid,
                })
            }),
            _ => Ok(Position::Many),
        }
    };
    let start = position(lookups, "start").map_err(|e| classify(key, e))?;
    let end = position(lookups, "end").map_err(|e| classify(key, e))?;
    let (start, end) = match (start, end) {
        (Position::Many, _) | (_, Position::Many) => {
            return Err(conflict(
                key,
                "the passage selector has more than one start or end",
            ));
        }
        (Position::None, _) | (_, Position::None) => return Err(unavailable(key)),
        (Position::Invalid, _) | (_, Position::Invalid) => {
            return Err(span(
                key,
                "the passage selector has no single start and end",
            ));
        }
        (Position::Some(start), Position::Some(end)) => (start, end),
    };
    let Some(text_digest) = single_digest(lookups, graph, iri, "textDigest", key)? else {
        return Err(unavailable(key));
    };
    // The rendition is itself an authenticated record of one of the record graphs.
    let Some((rendition_graph, kind)) = find_record(lookups, graphs, &rendition, key)? else {
        return Err(unavailable(key));
    };
    if kind != RecordKind::Rendition {
        return Err(conflict(key, "the passage's source is not a rendition"));
    }
    let receipt = receipt(
        lookups,
        &rendition_graph,
        &rendition,
        key,
        protections.get(rendition_graph.as_str()).copied(),
        None,
    )?;
    if receipt.dataset != main.dataset_id()
        || !main
            .branch_known_prepared(receipt.branch, lookups.preparation)
            .map_err(|e| classify(key, e))?
    {
        return Err(origin(key));
    }
    if require_rendition_revision {
        match lookups
            .one(rendition.as_str(), PROV_WAS_DERIVED_FROM, &rendition_graph)
            .map_err(|e| classify(key, e))?
        {
            One::Some(Term::NamedNode(_)) => {}
            One::Many => {
                return Err(conflict(
                    key,
                    "the rendition names more than one source revision",
                ));
            }
            One::None | One::Some(_) => return Err(unavailable(key)),
        }
    }
    Ok(PreparedPassageMeta {
        rendition_graph,
        span: VerifiedSpan {
            rendition,
            start,
            end,
            text_digest,
        },
    })
}

/// Verify all unresolved `pending` passages targeting `rendition` in a single scan of
/// `text`, returning `(slot_index, PendingOutcome)` for each matching entry.
fn verify_pending_spans_in_text(
    text: &str,
    pending: &[PendingPassage],
    rendition: &NamedNode,
    preparation: &Preparation,
) -> Result<Vec<(usize, PendingOutcome)>> {
    let control = preparation.control();
    let pending_count = pending
        .iter()
        .filter(|p| p.span.rendition == *rendition && p.outcome.is_none())
        .count();
    if pending_count == 0 {
        return Ok(Vec::new());
    }
    let offset_cap = pending_count.saturating_mul(2);
    let mut offsets: Vec<u64> = Vec::new();
    let _offsets_hold = preparation
        .reserve((offset_cap as u64).saturating_mul(std::mem::size_of::<u64>() as u64))?;
    offsets
        .try_reserve_exact(offset_cap)
        .map_err(|_| preparation.exhausted())?;
    for p in pending {
        if p.span.rendition == *rendition && p.outcome.is_none() {
            offsets.push(p.span.start);
            offsets.push(p.span.end);
        }
    }
    offsets.sort_unstable();
    offsets.dedup();
    let mut positions: Vec<usize> = Vec::new();
    let _pos_hold = preparation
        .reserve((offsets.len() as u64).saturating_mul(std::mem::size_of::<usize>() as u64))?;
    positions
        .try_reserve_exact(offsets.len())
        .map_err(|_| preparation.exhausted())?;
    #[cfg(test)]
    PASSAGE_TEXT_SCANS.with(|count| count.set(count.get() + 1));
    let mut count = 0u64;
    for (byte, _) in text.char_indices() {
        if count.is_multiple_of(16_384) {
            control.check()?;
        }
        if offsets.get(positions.len()) == Some(&count) {
            positions.push(byte);
            if positions.len() == offsets.len() {
                break;
            }
        }
        count += 1;
    }
    if offsets.get(positions.len()) == Some(&count) {
        positions.push(text.len());
    }
    control.check()?;
    let mut digests: BTreeMap<(usize, usize), Sha256Digest> = BTreeMap::new();
    let mut digest_hold = preparation.reserve(0)?;
    let mut outcomes: Vec<(usize, PendingOutcome)> = Vec::new();
    let _out_hold = preparation.reserve(
        (pending_count as u64)
            .saturating_mul(std::mem::size_of::<(usize, PendingOutcome)>() as u64),
    )?;
    outcomes
        .try_reserve_exact(pending_count)
        .map_err(|_| preparation.exhausted())?;
    for (slot, p) in pending.iter().enumerate() {
        if p.span.rendition != *rendition || p.outcome.is_some() {
            continue;
        }
        control.check()?;
        let out = if p.span.end < p.span.start {
            PendingOutcome::Span("the passage selector ends before it starts")
        } else {
            let start_idx = offsets
                .binary_search(&p.span.start)
                .expect("start offset was grouped");
            let end_idx = offsets
                .binary_search(&p.span.end)
                .expect("end offset was grouped");
            match (positions.get(start_idx), positions.get(end_idx)) {
                (Some(&start), Some(&end)) => {
                    let selected = &text[start..end];
                    let digest = match digests.entry((start, end)) {
                        std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            digest_hold.grow_by(128)?;
                            *entry.insert(Sha256Digest::of_bytes(selected.as_bytes(), control)?)
                        }
                    };
                    if digest == p.span.text_digest {
                        PendingOutcome::Valid
                    } else {
                        PendingOutcome::Span(
                            "the passage's text digest does not match the selected text",
                        )
                    }
                }
                _ => PendingOutcome::Span(
                    "the passage selector exceeds the rendition's normalized text",
                ),
            }
        };
        outcomes.push((slot, out));
    }
    Ok(outcomes)
}

/// Verify a passage's selector and text digest against its rendition's retained text.
#[allow(clippy::too_many_arguments)]
fn verify_passage(
    lookups: &mut Lookups<'_>,
    graphs: &[&NamedNode],
    protections: &BTreeMap<String, Uuid>,
    graph: &NamedNode,
    iri: &NamedNode,
    main: &Store,
    key: &str,
    require_rendition_revision: bool,
) -> Result<(NamedNode, VerifiedSpan)> {
    let meta = prepare_passage_metadata(
        lookups,
        graphs,
        protections,
        graph,
        iri,
        main,
        key,
        require_rendition_revision,
    )?;
    let start = meta.span.start;
    let end = meta.span.end;
    let text_digest = meta.span.text_digest;
    let control = lookups.preparation.control();
    let checked = lookups
        .one_text(
            meta.span.rendition.as_str(),
            &format!("{EV}normalizedText"),
            &meta.rendition_graph,
            |text| {
                if end < start {
                    return Ok(Err("the passage selector ends before it starts"));
                }
                #[cfg(test)]
                PASSAGE_TEXT_SCANS.with(|count| count.set(count.get() + 1));
                let offsets: &[u64] = if start == end {
                    &[start]
                } else {
                    &[start, end]
                };
                let positions = match locate_boundaries(text, offsets, control) {
                    Ok(positions) => positions,
                    Err(e @ (Error::Cancelled | Error::Timeout)) => return Err(e),
                    Err(_) => {
                        return Ok(Err(
                            "the passage selector exceeds the rendition's normalized text",
                        ));
                    }
                };
                let selected = &text[positions[0]..positions[positions.len() - 1]];
                if Sha256Digest::of_bytes(selected.as_bytes(), control)? != text_digest {
                    return Ok(Err(
                        "the passage's text digest does not match the selected text",
                    ));
                }
                Ok(Ok(()))
            },
        )
        .map_err(|e| classify(key, e))?;
    match checked {
        Text::Some(Ok(())) => {}
        Text::Some(Err(why)) => return Err(span(key, why)),
        Text::Many => {
            return Err(conflict(
                key,
                "the rendition has more than one normalized text",
            ));
        }
        Text::None => return Err(unavailable(key)),
    }
    Ok((meta.rendition_graph, meta.span))
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
