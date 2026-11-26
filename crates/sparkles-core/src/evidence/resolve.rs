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

/// A verified passage span cached for one bound `(ViewKey, view_index)` under one
/// installed `RecordGraphs` state.
struct CachedPassage {
    graph: NamedNode,
    rendition_graph: NamedNode,
    span: VerifiedSpan,
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
    _held: Reservation,
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
}

impl Resolver {
    /// A resolver with `memory_bytes` of allowance under `control`.
    pub fn new(control: Control, memory_bytes: u64) -> Result<Self> {
        Ok(Self {
            preparation: Preparation::new(control, memory_bytes)?,
            views: BTreeMap::new(),
            record_graphs: None,
        })
    }

    /// The operation's preparation context.
    pub fn preparation(&self) -> &Preparation {
        &self.preparation
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
        let live = logical.snapshot();
        let view_key = ViewKey {
            dataset: main.dataset_id(),
            branch: logical.branch_id(),
            commit: live.commit,
        };
        let (_, index) = self
            .view(view_key, access, |_| {
                Ok((live.clone(), logical.branch_id(), SnapshotCharge::shared()))
            })
            .map_err(|e| classify(key, e))?;
        Ok((view_key, index))
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
        self.record_in_view(
            key,
            view_key,
            index,
            main,
            &request.iri,
            &request.kinds,
            request.content_digest.as_ref(),
            SelectionRequirements::default(),
        )
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
        let _ = self.record_in_view(key, view_key, index, main, iri, kinds, None, selection)?;
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
    ) -> Result<VerifiedRecord> {
        let (auth, cached_span) = {
            let cached = &self.views[&view_key][index];
            let rg = self.record_graphs.as_ref().expect("prepared");
            let (graphs, _filtered) =
                readable_record_graphs(&self.preparation, &cached.view, &rg.graphs, key)?;
            let mut lookups = Lookups::new(&self.preparation, &cached.view);
            let auth = authenticate_record(
                &mut lookups,
                &graphs,
                &rg.protections,
                iri,
                kinds,
                "the record exists with another kind than the reference expects",
                main,
                key,
            )?;
            if let Some(expected) = expected_content_digest
                && auth.content_digest.as_ref() != Some(expected)
            {
                return Err(conflict(
                    key,
                    "the record's content digest differs from the expected digest",
                ));
            }
            let cached_span = if auth.kind == RecordKind::Passage {
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
                    Some((None, hit.span.clone()))
                } else {
                    let (rendition_graph, span) = verify_passage(
                        &mut lookups,
                        &graphs,
                        &rg.protections,
                        &auth.graph,
                        iri,
                        main,
                        key,
                        selection.passage_lineage,
                    )?;
                    Some((Some(rendition_graph), span))
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
                    match lookups
                        .one_text(
                            iri.as_str(),
                            &format!("{EV}normalizedText"),
                            &auth.graph,
                            |_| Ok(()),
                        )
                        .map_err(|e| classify(key, e))?
                    {
                        Text::Some(()) => {}
                        Text::Many => {
                            return Err(conflict(
                                key,
                                "the rendition has more than one normalized text",
                            ));
                        }
                        Text::None => return Err(unavailable(key)),
                    }
                }
                if auth.kind == RecordKind::SourceRevision && selection.structured_content {
                    if auth.content_digest.is_none() {
                        return Err(unavailable(key));
                    }
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
                        Text::Some(()) => {}
                        Text::Many => {
                            return Err(conflict(
                                key,
                                "the revision has more than one retained bytes value",
                            ));
                        }
                        Text::None => return Err(unavailable(key)),
                    }
                }
                None
            };
            (auth, cached_span)
        };
        let span = match cached_span {
            Some((Some(rendition_graph), span)) => {
                self.cache_verified_passage(
                    key,
                    view_key,
                    index,
                    iri,
                    &auth.graph,
                    &rendition_graph,
                    &span,
                )?;
                Some(span)
            }
            Some((None, span)) => Some(span),
            None => None,
        };
        Ok(VerifiedRecord {
            iri: iri.clone(),
            kind: auth.kind,
            graph: auth.graph,
            receipt: auth.receipt,
            content_digest: auth.content_digest,
            record_digest: auth.record_digest,
            span,
        })
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
        if passages.is_empty() {
            return Ok(());
        }
        // Validate caller-input-only span bounds before touching the store so malformed
        // input answers identically whether or not the rendition exists.
        for p in passages {
            self.preparation.check()?;
            if p.start >= p.end {
                return Err(span(p.key, "passage must be nonempty"));
            }
        }
        let target_key = rendition.as_str();
        let (view_key, index) = self.prepare_record_view(target_key, main, logical, access)?;
        let cached = &self.views[&view_key][index];
        let rg = self.record_graphs.as_ref().expect("prepared");
        let (graphs, _filtered) =
            readable_record_graphs(&self.preparation, &cached.view, &rg.graphs, target_key)?;
        let protections = &rg.protections;
        let mut lookups = Lookups::new(&self.preparation, &cached.view);
        let auth = authenticate_record(
            &mut lookups,
            &graphs,
            protections,
            rendition,
            &[RecordKind::Rendition],
            "the passage's source is not a rendition",
            main,
            target_key,
        )?;
        let rendition_graph = auth.graph;
        let actual_rev = match lookups
            .one(rendition.as_str(), PROV_WAS_DERIVED_FROM, &rendition_graph)
            .map_err(|e| classify(target_key, e))?
        {
            One::Some(Term::NamedNode(actual_rev)) => actual_rev,
            One::Many => {
                return Err(conflict(
                    target_key,
                    "the rendition names more than one source revision",
                ));
            }
            One::None | One::Some(_) => return Err(unavailable(target_key)),
        };
        enum PassageOutcome<'a> {
            Ok,
            Invalid(&'a str),
            Span(&'a str, &'static str),
        }
        // Establish that `ev:normalizedText` is present and caller-visible before
        // comparing `actual_rev` against each passage's revision, so a masked or
        // metadata-only rendition answers `evidence-unavailable` identically to an
        // absent rendition even when the supplied revision differs.
        let preparation = &self.preparation;
        let checked = lookups
            .one_text(
                rendition.as_str(),
                &format!("{EV}normalizedText"),
                &rendition_graph,
                |text| {
                    let control = preparation.control();
                    for p in passages {
                        control.check()?;
                        if p.revision != &actual_rev {
                            return Ok(PassageOutcome::Invalid(p.key));
                        }
                    }
                    let mut offsets: Vec<u64> = Vec::new();
                    let offset_cap = passages.len().saturating_mul(2);
                    let _offsets_hold = preparation.reserve(
                        (offset_cap as u64).saturating_mul(std::mem::size_of::<u64>() as u64),
                    )?;
                    offsets
                        .try_reserve_exact(offset_cap)
                        .map_err(|_| preparation.exhausted())?;
                    for p in passages {
                        offsets.push(p.start);
                        offsets.push(p.end);
                    }
                    offsets.sort_unstable();
                    offsets.dedup();
                    let mut positions: Vec<usize> = Vec::new();
                    let _pos_hold = preparation.reserve(
                        (offsets.len() as u64).saturating_mul(std::mem::size_of::<usize>() as u64),
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
                                p.key,
                                "the passage selector exceeds the rendition's normalized text",
                            ));
                        };
                        let selected = &text[start..end];
                        if p.quote.is_some_and(|quote| quote != selected) {
                            return Ok(PassageOutcome::Span(
                                p.key,
                                "the passage's quote does not match the rendition's normalized text",
                            ));
                        }
                        let digest = match digests.entry((start, end)) {
                            std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                digest_hold.grow_by(128)?;
                                *entry.insert(Sha256Digest::of_bytes(selected.as_bytes(), control)?)
                            }
                        };
                        if digest != p.text_digest {
                            return Ok(PassageOutcome::Span(
                                p.key,
                                "the passage's text digest does not match the selected text",
                            ));
                        }
                    }
                    Ok(PassageOutcome::Ok)
                },
            )
            .map_err(|e| classify(target_key, e))?;
        match checked {
            Text::Some(PassageOutcome::Ok) => Ok(()),
            Text::Some(PassageOutcome::Invalid(k)) => Err(Error::invalid(format!(
                "{k}: the passage and its rendition name different revisions"
            ))),
            Text::Some(PassageOutcome::Span(k, why)) => Err(span(k, why)),
            Text::Many => Err(conflict(
                target_key,
                "the rendition has more than one normalized text",
            )),
            Text::None => Err(unavailable(target_key)),
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
            let mut lookups = Lookups::new(&self.preparation, &cached.view);
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
        let Some(first) = references.first() else {
            return Ok(());
        };
        // Validate RFC 6901 pointer syntax from caller input before reading the store
        // so malformed pointers always answer Invalid (400) rather than span-mismatch.
        for reference in references {
            super::structured::validate_pointer(reference.pointer, self.preparation.control())?;
        }
        let target_key = revision.as_str();
        let first_key = first.key;
        let (view_key, index) = self.prepare_record_view(target_key, main, logical, access)?;
        let (expected_digest, bytes, _decoded_hold) = {
            let cached = &self.views[&view_key][index];
            let rg = self.record_graphs.as_ref().expect("prepared");
            let (graphs, _filtered) =
                readable_record_graphs(&self.preparation, &cached.view, &rg.graphs, target_key)?;
            let protections = &rg.protections;
            let mut lookups = Lookups::new(&self.preparation, &cached.view);
            let auth = authenticate_record(
                &mut lookups,
                &graphs,
                protections,
                revision,
                &[RecordKind::SourceRevision],
                "the structured reference's source is not a source revision",
                main,
                target_key,
            )?;
            let rev_graph = auth.graph;
            let Some(expected_digest) = auth.content_digest else {
                return Err(unavailable(target_key));
            };
            enum DecodedBytes {
                Ok(Vec<u8>, Reservation),
                InvalidBase64,
            }
            // Keep only base64 decoding inside `one_typed_literal` so the vocabulary
            // workspace and any `dvocab` read lock are released before SHA-256 hashing
            // and JSON validation/traversal run.
            let preparation = &self.preparation;
            let decoded = lookups
                .one_typed_literal(
                    revision.as_str(),
                    &format!("{EV}retainedBytes"),
                    XSD_BASE64_BINARY,
                    &rev_graph,
                    |b64| {
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
                .map_err(|e| classify(target_key, e))?;
            let (bytes, decoded_hold) = match decoded {
                Text::Some(DecodedBytes::Ok(bytes, hold)) => (bytes, hold),
                Text::Some(DecodedBytes::InvalidBase64) => {
                    return Err(conflict(
                        target_key,
                        "the revision's retained bytes are not valid base64",
                    ));
                }
                Text::Many => {
                    return Err(conflict(
                        target_key,
                        "the revision has more than one retained bytes value",
                    ));
                }
                Text::None => return Err(unavailable(target_key)),
            };
            (expected_digest, bytes, decoded_hold)
        };
        let control = self.preparation.control();
        if Sha256Digest::of_bytes(&bytes, control)? != expected_digest {
            return Err(span(
                first_key,
                "the revision's retained bytes do not match its content digest",
            ));
        }
        let Ok(json) = std::str::from_utf8(&bytes) else {
            return Err(span(
                first_key,
                "the revision's retained bytes are not valid UTF-8 JSON",
            ));
        };
        let mut pointers: Vec<&str> = Vec::new();
        let _ptrs_hold = self
            .preparation
            .reserve((references.len() as u64).saturating_mul(std::mem::size_of::<&str>() as u64))
            .map_err(|e| classify(target_key, e))?;
        pointers
            .try_reserve_exact(references.len())
            .map_err(|_| classify(target_key, self.preparation.exhausted()))?;
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
                if let Some(why) = msg.strip_prefix("span-mismatch: ") {
                    return Err(span(first_key, why));
                }
                return Err(Error::Invalid(msg));
            }
            Err(e) => return Err(classify(target_key, e)),
        };
        for (reference, value) in references.iter().zip(selected) {
            control.check()?;
            let Some(value) = value else {
                return Err(span(
                    reference.key,
                    "JSON Pointer does not select a retained value",
                ));
            };
            if value.digest != reference.value_digest {
                return Err(span(
                    reference.key,
                    "the selected JSON value's digest does not match valueDigest",
                ));
            }
        }
        Ok(())
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
struct Lookups<'a> {
    preparation: &'a Preparation,
    view: &'a PreparedView,
    ids: BTreeMap<String, Option<Id>>,
}

impl<'a> Lookups<'a> {
    fn new(preparation: &'a Preparation, view: &'a PreparedView) -> Self {
        Self {
            preparation,
            view,
            ids: BTreeMap::new(),
        }
    }

    fn snap(&self) -> &'a Snapshot {
        self.view.snapshot()
    }

    fn iri(&mut self, iri: &str) -> Result<Option<Id>> {
        if let Some(id) = self.ids.get(iri) {
            return Ok(*id);
        }
        let id = self.preparation.lookup_iri(self.snap(), iri)?;
        self.ids.insert(iri.to_string(), id);
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
            [id] => self
                .preparation
                .with_simple_literal(self.snap(), *id, |text| match text {
                    Some(text) => Ok(Text::Some(each(text)?)),
                    None => Ok(Text::None),
                }),
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
            [id] => self
                .preparation
                .with_typed_literal(self.snap(), *id, datatype, |text| match text {
                    Some(text) => Ok(Text::Some(each(text)?)),
                    None => Ok(Text::None),
                }),
            _ => Ok(Text::Many),
        }
    }

    fn objects(&mut self, s: &str, p: &str, graph: &NamedNode) -> Result<Vec<Term>> {
        if !self.readable(graph) {
            return Ok(Vec::new());
        }
        let (Some(s), Some(p), Some(g)) = (self.iri(s)?, self.iri(p)?, self.iri(graph.as_str())?)
        else {
            return Ok(Vec::new());
        };
        let ids = self.object_ids(s, p, g)?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(term) = self
                .preparation
                .with_term(self.snap(), id, |t| Ok(t.cloned()))?
            {
                out.push(term);
            }
        }
        Ok(out)
    }

    /// The one object of `s p ?o` in `graph`; `Ok(None)` when there is none, and
    /// `Err(())` inside `Ok` when there is more than one.
    fn one(&mut self, s: &str, p: &str, graph: &NamedNode) -> Result<One> {
        let mut values = self.objects(s, p, graph)?;
        Ok(match values.len() {
            0 => One::None,
            1 => One::Some(values.pop().expect("one value")),
            _ => One::Many,
        })
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
) -> Result<Receipt> {
    let receipt = match lookups
        .one(iri.as_str(), &format!("{EV}availabilityReceipt"), graph)
        .map_err(|e| classify(key, e))?
    {
        One::Some(Term::NamedNode(r)) => r,
        _ => return Err(origin(key)),
    };
    let mut uuid_of = |p: &str| -> Result<Option<Uuid>> {
        Ok(
            match lookups
                .one(receipt.as_str(), &format!("{EV}{p}"), graph)
                .map_err(|e| classify(key, e))?
            {
                One::Some(Term::Literal(l))
                    if l.datatype().as_str() == "http://www.w3.org/2001/XMLSchema#string" =>
                {
                    Uuid::parse_str(l.value()).ok()
                }
                _ => None,
            },
        )
    };
    let (Some(dataset), Some(branch)) = (uuid_of("datasetId")?, uuid_of("branchId")?) else {
        return Err(origin(key));
    };
    let commit = match lookups
        .one(receipt.as_str(), &format!("{EV}commit"), graph)
        .map_err(|e| classify(key, e))?
    {
        One::Some(Term::Literal(l)) => integer(&l),
        _ => None,
    };
    let Some(commit) = commit else {
        return Err(origin(key));
    };
    // The operation marker of decision 1 of the resolution step.
    let Some(nonce) = nonce else {
        return Err(origin(key));
    };
    if !lookups
        .has(receipt.as_str(), RDF_TYPE, &format!("{EV}Operation"), graph)
        .map_err(|e| classify(key, e))?
    {
        return Err(origin(key));
    }
    // The record and its operation both carry the nonce. A record planted while the
    // graph was detached cannot, even when it names an operation that a later typed
    // write records with the current nonce.
    for subject in [receipt.as_str(), iri.as_str()] {
        match lookups
            .one(subject, &format!("{EV}graphProtection"), graph)
            .map_err(|e| classify(key, e))?
        {
            One::Some(Term::Literal(l))
                if l.datatype().as_str() == "http://www.w3.org/2001/XMLSchema#string"
                    && Uuid::parse_str(l.value()).ok() == Some(nonce) => {}
            _ => return Err(origin(key)),
        }
    }
    Ok(Receipt {
        iri: receipt,
        dataset,
        branch,
        commit,
    })
}

/// A non-negative `xsd:integer` value, also from a stored inline literal.
fn integer(l: &oxrdf::Literal) -> Option<u64> {
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
    let rec = receipt(
        lookups,
        &graph,
        iri,
        key,
        protections.get(graph.as_str()).copied(),
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
    verify_with_selector(
        lookups,
        graphs,
        protections,
        graph,
        iri,
        &rendition,
        selector,
        main,
        key,
        require_rendition_revision,
    )
}

#[allow(clippy::too_many_arguments)]
fn verify_with_selector(
    lookups: &mut Lookups<'_>,
    graphs: &[&NamedNode],
    protections: &BTreeMap<String, Uuid>,
    graph: &NamedNode,
    iri: &NamedNode,
    rendition: &NamedNode,
    selector: Term,
    main: &Store,
    key: &str,
    require_rendition_revision: bool,
) -> Result<(NamedNode, VerifiedSpan)> {
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
    let Some((rendition_graph, kind)) = find_record(lookups, graphs, rendition, key)? else {
        return Err(unavailable(key));
    };
    if kind != RecordKind::Rendition {
        return Err(conflict(key, "the passage's source is not a rendition"));
    }
    let receipt = receipt(
        lookups,
        &rendition_graph,
        rendition,
        key,
        protections.get(rendition_graph.as_str()).copied(),
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
    let control = lookups.preparation.control();
    let checked = lookups
        .one_text(
            rendition.as_str(),
            &format!("{EV}normalizedText"),
            &rendition_graph,
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
    Ok((
        rendition_graph,
        VerifiedSpan {
            rendition: rendition.clone(),
            start,
            end,
            text_digest,
        },
    ))
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
