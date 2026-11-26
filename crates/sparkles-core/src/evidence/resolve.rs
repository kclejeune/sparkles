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
use crate::{Error, Result};
use oxrdf::{NamedNode, Term, Triple};
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

/// The memory allowance of one resolution operation (spec C25 §8).
pub const MEMORY_BYTES: u64 = 64 << 20;

/// The most references one operation resolves.
pub const MAX_REFERENCES: usize = 10_000;

const EV_AVAILABILITY_RECEIPT: &str = "urn:x-sparkles:evidence:availabilityReceipt";
const EV_DATASET_ID: &str = "urn:x-sparkles:evidence:datasetId";
const EV_BRANCH_ID: &str = "urn:x-sparkles:evidence:branchId";
const EV_COMMIT: &str = "urn:x-sparkles:evidence:commit";
const EV_OPERATION: &str = "urn:x-sparkles:evidence:Operation";
const EV_GRAPH_PROTECTION: &str = "urn:x-sparkles:evidence:graphProtection";
const EV_CONTENT_DIGEST: &str = "urn:x-sparkles:evidence:contentDigest";
const EV_RECORD_DIGEST: &str = "urn:x-sparkles:evidence:recordDigest";
const EV_TEXT_DIGEST: &str = "urn:x-sparkles:evidence:textDigest";
const EV_NORMALIZED_TEXT: &str = "urn:x-sparkles:evidence:normalizedText";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const OA_HAS_SOURCE: &str = "http://www.w3.org/ns/oa#hasSource";
const OA_HAS_SELECTOR: &str = "http://www.w3.org/ns/oa#hasSelector";
const OA_START: &str = "http://www.w3.org/ns/oa#start";
const OA_END: &str = "http://www.w3.org/ns/oa#end";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

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

const KINDS: [RecordKind; 16] = [
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
    /// Optional `(dataset_id, branch_id, commit)` pinning `record` and graph helpers to
    /// one branch commit across the resolution.
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
        self.record_charged(
            key,
            main,
            logical,
            access,
            &request.iri,
            &request.kinds,
            request.content_digest.as_ref(),
        )
        .map(|(verified, _held)| verified)
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
        let (view_key, index) = self.logical_view(key, main, logical, access)?;
        let cached = &self.views[&view_key][index];
        let record_graphs = self.record_graphs.as_ref().expect("prepared");
        let all_graphs = &record_graphs.graphs;
        let protections = &record_graphs.protections;
        // The readable record graphs are borrowed from the charged list.
        let mut graphs: Vec<&NamedNode> = Vec::new();
        let _filtered = self
            .preparation
            .reserve((all_graphs.len() as u64).saturating_mul(8))
            .map_err(|e| classify(key, e))?;
        graphs
            .try_reserve_exact(all_graphs.len())
            .map_err(|_| classify(key, self.preparation.exhausted()))?;
        let bound = cached.view.access();
        for g in all_graphs {
            self.preparation.check()?;
            if bound.readable(Some(&Term::NamedNode(g.clone()))) {
                graphs.push(g);
            }
        }
        if graphs.is_empty() {
            return Err(unavailable(key));
        }
        let Some(iri_id) = self
            .preparation
            .lookup_iri(cached.view.snapshot(), iri.as_str())
            .map_err(|e| classify(key, e))?
        else {
            return Err(unavailable(key));
        };
        let mut lookups =
            Lookups::new(&self.preparation, &cached.view).map_err(|e| classify(key, e))?;
        let Some((graph_idx, graph_id, kind)) = find_record_id(&mut lookups, &graphs, iri_id, key)?
        else {
            return Err(unavailable(key));
        };
        if !kinds.is_empty() && !kinds.contains(&kind) {
            return Err(conflict(
                key,
                "the record exists with another kind than the reference expects",
            ));
        }
        let graph_node = graphs[graph_idx];
        let mut result_held = self.preparation.reserve(0).map_err(|e| classify(key, e))?;
        let receipt = verify_receipt(
            &mut lookups,
            graph_id,
            iri_id,
            main,
            key,
            protections.get(graph_node.as_str()).copied(),
            &mut result_held,
        )?;
        let content_digest = single_digest(&mut lookups, graph_id, iri_id, EV_CONTENT_DIGEST, key)?;
        if let Some(expected) = expected_content_digest
            && content_digest.as_ref() != Some(expected)
        {
            return Err(conflict(
                key,
                "the record's content digest differs from the expected digest",
            ));
        }
        let record_digest = single_digest(&mut lookups, graph_id, iri_id, EV_RECORD_DIGEST, key)?;
        let span = if kind == RecordKind::Passage {
            Some(verify_passage(
                &mut lookups,
                &graphs,
                protections,
                graph_id,
                iri_id,
                main,
                key,
                &mut result_held,
            )?)
        } else {
            None
        };
        let graph_str = graph_node.as_str();
        drop(lookups);
        drop(graphs);
        drop(_filtered);
        let verified_iri = NamedNode::new_unchecked(
            self.preparation
                .copy_string(iri.as_str(), &mut result_held)
                .map_err(|e| classify(key, e))?,
        );
        let verified_graph = NamedNode::new_unchecked(
            self.preparation
                .copy_string(graph_str, &mut result_held)
                .map_err(|e| classify(key, e))?,
        );
        result_held
            .grow_by(std::mem::size_of::<VerifiedRecord>() as u64)
            .map_err(|e| classify(key, e))?;
        Ok((
            VerifiedRecord {
                iri: verified_iri,
                kind,
                graph: verified_graph,
                receipt,
                content_digest,
                record_digest,
                span,
            },
            result_held,
        ))
    }

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

    /// Whether `s p o` is in readable graph `g`.
    fn has_id(&mut self, s: Id, p: &str, o: &str, g: Id) -> Result<bool> {
        let (Some(p), Some(o)) = (self.iri(p)?, self.iri(o)?) else {
            return Ok(false);
        };
        self.snap().contains(&[s, p, o, g])
    }

    /// The one object id of `s p ?o` in readable graph `g`.
    fn one_id(&mut self, s: Id, p: &str, g: Id) -> Result<OneId> {
        let Some(p) = self.iri(p)? else {
            return Ok(OneId::None);
        };
        let mut first: Option<Id> = None;
        let mut many = false;
        self.preparation
            .scan(self.snap(), Perm::Gspo, &[g.0, s.0, p.0], |q| {
                if first.is_none() {
                    first = Some(q[2]);
                    Ok(true)
                } else {
                    many = true;
                    Ok(false)
                }
            })?;
        Ok(if many {
            OneId::Many
        } else if let Some(id) = first {
            OneId::Some(id)
        } else {
            OneId::None
        })
    }

    /// Borrow the one decoded object term of `s p ?o` in readable graph `g` without
    /// retaining an uncharged copy past `each`.
    fn with_one_term<R>(
        &mut self,
        s: Id,
        p: &str,
        g: Id,
        each: impl FnOnce(Option<&Term>) -> Result<R>,
    ) -> Result<OneValue<R>> {
        match self.one_id(s, p, g)? {
            OneId::None => Ok(OneValue::None),
            OneId::Many => Ok(OneValue::Many),
            OneId::Some(id) => self
                .preparation
                .with_term(self.snap(), id, |t| Ok(OneValue::Some(each(t)?))),
        }
    }

    /// The one object of `s p ?o` in readable graph `g`, read as a stored simple literal's
    /// text without decoding it as a term.
    fn one_text_id<R>(
        &mut self,
        s: Id,
        p: &str,
        g: Id,
        each: impl FnOnce(&str) -> Result<R>,
    ) -> Result<Text<R>> {
        match self.one_id(s, p, g)? {
            OneId::None => Ok(Text::None),
            OneId::Some(id) => self
                .preparation
                .with_simple_literal(self.snap(), id, |text| match text {
                    Some(text) => Ok(Text::Some(each(text)?)),
                    None => Ok(Text::None),
                }),
            OneId::Many => Ok(Text::Many),
        }
    }
}

enum OneId {
    None,
    Some(Id),
    Many,
}

enum OneValue<R> {
    None,
    Some(R),
    Many,
}

enum Text<R> {
    None,
    Some(R),
    Many,
}

/// The readable graph index, graph id, and kind of `iri_id` among `graphs`, when it is a
/// profile record there.
fn find_record_id(
    lookups: &mut Lookups<'_>,
    graphs: &[&NamedNode],
    iri_id: Id,
    key: &str,
) -> Result<Option<(usize, Id, RecordKind)>> {
    let mut found: Option<(usize, Id, RecordKind)> = None;
    for (idx, graph) in graphs.iter().enumerate() {
        let Some(g_id) = lookups.iri(graph.as_str()).map_err(|e| classify(key, e))? else {
            continue;
        };
        for kind in KINDS {
            if lookups
                .has_id(iri_id, RDF_TYPE, kind_iri(kind), g_id)
                .map_err(|e| classify(key, e))?
            {
                if found.is_some() {
                    return Err(conflict(
                        key,
                        "the record is stored more than once or with more than one kind",
                    ));
                }
                found = Some((idx, g_id, kind));
            }
        }
    }
    Ok(found)
}

/// Verify the record's one publication receipt in its graph against `nonce` and `main`,
/// checking authentication before decoding the receipt IRI itself. When `copy_iri` is
/// `Some`, copies the authenticated receipt IRI into that reservation and returns `Receipt`.
#[allow(clippy::too_many_arguments)]
fn verify_receipt_inner(
    lookups: &mut Lookups<'_>,
    graph_id: Id,
    iri_id: Id,
    main: &Store,
    key: &str,
    nonce: Option<Uuid>,
    copy_iri: Option<&mut Reservation>,
) -> Result<Option<Receipt>> {
    // The operation marker of decision 1 of the resolution step.
    let Some(nonce) = nonce else {
        return Err(origin(key));
    };
    let receipt_id = match lookups
        .one_id(iri_id, EV_AVAILABILITY_RECEIPT, graph_id)
        .map_err(|e| classify(key, e))?
    {
        OneId::Some(id) => id,
        _ => return Err(origin(key)),
    };
    // The record and its operation both carry the nonce. Check the nonce before decoding
    // the receipt IRI or any other receipt field so unauthenticated records cannot retain
    // or decode oversized receipt strings.
    for subject_id in [iri_id, receipt_id] {
        let matched = lookups
            .with_one_term(subject_id, EV_GRAPH_PROTECTION, graph_id, |term| {
                Ok(matches!(
                    term,
                    Some(Term::Literal(l))
                        if l.datatype().as_str() == XSD_STRING
                            && Uuid::parse_str(l.value()).ok() == Some(nonce)
                ))
            })
            .map_err(|e| classify(key, e))?;
        if !matches!(matched, OneValue::Some(true)) {
            return Err(origin(key));
        }
    }
    if !lookups
        .has_id(receipt_id, RDF_TYPE, EV_OPERATION, graph_id)
        .map_err(|e| classify(key, e))?
    {
        return Err(origin(key));
    }
    let mut uuid_of = |p: &str| -> Result<Option<Uuid>> {
        match lookups
            .with_one_term(receipt_id, p, graph_id, |term| {
                Ok(match term {
                    Some(Term::Literal(l)) if l.datatype().as_str() == XSD_STRING => {
                        Uuid::parse_str(l.value()).ok()
                    }
                    _ => None,
                })
            })
            .map_err(|e| classify(key, e))?
        {
            OneValue::Some(u) => Ok(u),
            _ => Ok(None),
        }
    };
    let (Some(dataset), Some(branch)) = (uuid_of(EV_DATASET_ID)?, uuid_of(EV_BRANCH_ID)?) else {
        return Err(origin(key));
    };
    let commit = match lookups
        .with_one_term(receipt_id, EV_COMMIT, graph_id, |term| {
            Ok(match term {
                Some(Term::Literal(l)) => integer(l),
                _ => None,
            })
        })
        .map_err(|e| classify(key, e))?
    {
        OneValue::Some(c) => c,
        _ => None,
    };
    let Some(commit) = commit else {
        return Err(origin(key));
    };
    if dataset != main.dataset_id()
        || !main
            .branch_known_prepared(branch, lookups.preparation)
            .map_err(|e| classify(key, e))?
    {
        return Err(origin(key));
    }
    match copy_iri {
        Some(held) => {
            let receipt_iri = lookups
                .preparation
                .with_term(lookups.snap(), receipt_id, |term| match term {
                    Some(Term::NamedNode(n)) => {
                        let s = lookups.preparation.copy_string(n.as_str(), held)?;
                        Ok(Some(NamedNode::new_unchecked(s)))
                    }
                    _ => Ok(None),
                })
                .map_err(|e| classify(key, e))?
                .ok_or_else(|| origin(key))?;
            Ok(Some(Receipt {
                iri: receipt_iri,
                dataset,
                branch,
                commit,
            }))
        }
        None => {
            let is_named = lookups
                .preparation
                .with_term(lookups.snap(), receipt_id, |term| {
                    Ok(matches!(term, Some(Term::NamedNode(_))))
                })
                .map_err(|e| classify(key, e))?;
            if !is_named {
                return Err(origin(key));
            }
            Ok(None)
        }
    }
}

/// The record's one publication receipt in its graph, charging its IRI copy to `held`.
#[allow(clippy::too_many_arguments)]
fn verify_receipt(
    lookups: &mut Lookups<'_>,
    graph_id: Id,
    iri_id: Id,
    main: &Store,
    key: &str,
    nonce: Option<Uuid>,
    held: &mut Reservation,
) -> Result<Receipt> {
    Ok(
        verify_receipt_inner(lookups, graph_id, iri_id, main, key, nonce, Some(held))?
            .expect("receipt returned when copy_iri is Some"),
    )
}

/// A non-negative `xsd:integer` value, also from a stored inline literal.
fn integer(l: &oxrdf::Literal) -> Option<u64> {
    let datatype = l.datatype().as_str();
    if datatype != XSD_INTEGER {
        return None;
    }
    l.value().trim_start_matches('+').parse::<u64>().ok()
}

/// The record's one digest under `predicate`, refusing more than one value.
fn single_digest(
    lookups: &mut Lookups<'_>,
    graph_id: Id,
    iri_id: Id,
    predicate: &str,
    key: &str,
) -> Result<Option<Sha256Digest>> {
    match lookups
        .with_one_term(iri_id, predicate, graph_id, |term| {
            Ok(match term {
                Some(Term::Literal(l)) => match l.value().parse() {
                    Ok(d) => Ok(Some(d)),
                    Err(_) => Err("the record's digest is not a SHA-256 digest"),
                },
                Some(_) => Err("the record's digest is not a SHA-256 digest"),
                None => Ok(None),
            })
        })
        .map_err(|e| classify(key, e))?
    {
        OneValue::None => Ok(None),
        OneValue::Some(Ok(d)) => Ok(d),
        OneValue::Some(Err(why)) => Err(conflict(key, why)),
        OneValue::Many => Err(conflict(key, "the record has more than one digest")),
    }
}

/// Verify a passage's selector and text digest against its rendition's retained text.
#[allow(clippy::too_many_arguments)]
fn verify_passage(
    lookups: &mut Lookups<'_>,
    graphs: &[&NamedNode],
    protections: &BTreeMap<String, Uuid>,
    graph_id: Id,
    iri_id: Id,
    main: &Store,
    key: &str,
    result_held: &mut Reservation,
) -> Result<VerifiedSpan> {
    let rendition_id = match lookups
        .one_id(iri_id, OA_HAS_SOURCE, graph_id)
        .map_err(|e| classify(key, e))?
    {
        OneId::Some(id) => {
            let is_named = lookups
                .preparation
                .with_term(lookups.snap(), id, |t| {
                    Ok(matches!(t, Some(Term::NamedNode(_))))
                })
                .map_err(|e| classify(key, e))?;
            if !is_named {
                return Err(span(key, "the passage names no rendition"));
            }
            id
        }
        OneId::Many => return Err(conflict(key, "the passage names more than one rendition")),
        OneId::None => return Err(span(key, "the passage names no rendition")),
    };
    let selector_id = match lookups
        .one_id(iri_id, OA_HAS_SELECTOR, graph_id)
        .map_err(|e| classify(key, e))?
    {
        OneId::Some(id) => {
            let is_node = lookups
                .preparation
                .with_term(lookups.snap(), id, |t| {
                    Ok(matches!(t, Some(Term::NamedNode(_) | Term::BlankNode(_))))
                })
                .map_err(|e| classify(key, e))?;
            if !is_node {
                return Err(span(key, "the passage has no selector"));
            }
            id
        }
        OneId::Many => return Err(conflict(key, "the passage has more than one selector")),
        OneId::None => return Err(span(key, "the passage has no selector")),
    };
    let position = |lookups: &mut Lookups<'_>, p: &str| -> Result<Option<u64>> {
        match lookups.with_one_term(selector_id, p, graph_id, |t| {
            Ok(match t {
                Some(Term::Literal(l)) => integer(l),
                _ => None,
            })
        })? {
            OneValue::Some(v) => Ok(v),
            _ => Ok(None),
        }
    };
    let start = position(lookups, OA_START).map_err(|e| classify(key, e))?;
    let end = position(lookups, OA_END).map_err(|e| classify(key, e))?;
    let (Some(start), Some(end)) = (start, end) else {
        return Err(span(
            key,
            "the passage selector has no single start and end",
        ));
    };
    if end < start {
        return Err(span(key, "the passage selector ends before it starts"));
    }
    let Some(text_digest) = single_digest(lookups, graph_id, iri_id, EV_TEXT_DIGEST, key)? else {
        return Err(span(key, "the passage has no text digest"));
    };
    // The rendition is itself an authenticated record of one of the record graphs.
    let Some((rendition_graph_idx, rendition_graph_id, kind)) =
        find_record_id(lookups, graphs, rendition_id, key)?
    else {
        return Err(unavailable(key));
    };
    if kind != RecordKind::Rendition {
        return Err(conflict(key, "the passage's source is not a rendition"));
    }
    verify_receipt_inner(
        lookups,
        rendition_graph_id,
        rendition_id,
        main,
        key,
        protections
            .get(graphs[rendition_graph_idx].as_str())
            .copied(),
        None,
    )?;
    let control = lookups.preparation.control();
    let checked = lookups
        .one_text_id(
            rendition_id,
            EV_NORMALIZED_TEXT,
            rendition_graph_id,
            |text| {
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
    let rendition = lookups
        .preparation
        .with_term(lookups.snap(), rendition_id, |t| match t {
            Some(Term::NamedNode(n)) => {
                let s = lookups.preparation.copy_string(n.as_str(), result_held)?;
                Ok(Some(NamedNode::new_unchecked(s)))
            }
            _ => Ok(None),
        })
        .map_err(|e| classify(key, e))?
        .ok_or_else(|| span(key, "the passage names no rendition"))?;
    Ok(VerifiedSpan {
        rendition,
        start,
        end,
        text_digest,
    })
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
