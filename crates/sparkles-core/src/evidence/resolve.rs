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
use crate::store::{Snapshot, Store};
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

const EV: &str = "urn:x-sparkles:evidence:";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const OA: &str = "http://www.w3.org/ns/oa#";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

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
                let (snapshot, resolved, served_by) = main.snapshot_of_branch_limited(
                    fact.branch,
                    &At::Commit(fact.commit),
                    &preparation.history_options(),
                    preparation.room(),
                )?;
                if resolved.commit.seq != fact.commit {
                    return Err(Error::NotFound("commit".into()));
                }
                Ok((snapshot, served_by))
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
        let live = logical.snapshot();
        let view_key = ViewKey {
            dataset: main.dataset_id(),
            branch: logical.branch_id(),
            commit: live.commit,
        };
        let (_, index) = self
            .view(view_key, access, |_| {
                Ok((live.clone(), logical.branch_id()))
            })
            .map_err(|e| classify(key, e))?;
        let cached = &self.views[&view_key][index];
        let all_graphs = &self.record_graphs.as_ref().expect("prepared").graphs;
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
        let mut lookups = Lookups::new(&self.preparation, &cached.view);
        let found = find_record(&mut lookups, &graphs, &request.iri, key)?;
        let Some((graph, kind)) = found else {
            return Err(unavailable(key));
        };
        if !request.kinds.is_empty() && !request.kinds.contains(&kind) {
            return Err(conflict(
                key,
                "the record exists with another kind than the reference expects",
            ));
        }
        let protections = &self.record_graphs.as_ref().expect("prepared").protections;
        let receipt = receipt(
            &mut lookups,
            &graph,
            &request.iri,
            key,
            protections.get(graph.as_str()).copied(),
        )?;
        if receipt.dataset != main.dataset_id()
            || !main
                .branch_known_prepared(receipt.branch, lookups.preparation)
                .map_err(|e| classify(key, e))?
        {
            return Err(origin(key));
        }
        let content_digest =
            single_digest(&mut lookups, &graph, &request.iri, "contentDigest", key)?;
        if let Some(expected) = &request.content_digest
            && content_digest.as_ref() != Some(expected)
        {
            return Err(conflict(
                key,
                "the record's content digest differs from the expected digest",
            ));
        }
        let record_digest = single_digest(&mut lookups, &graph, &request.iri, "recordDigest", key)?;
        let span = if kind == RecordKind::Passage {
            Some(verify_passage(
                &mut lookups,
                &graphs,
                protections,
                &graph,
                &request.iri,
                main,
                key,
            )?)
        } else {
            None
        };
        Ok(VerifiedRecord {
            iri: request.iri.clone(),
            kind,
            graph,
            receipt,
            content_digest,
            record_digest,
            span,
        })
    }

    /// Prepare, or reuse, the controlled view of `key` under `access`, and return the
    /// serving branch and the view's index among the views of `key`.
    fn view(
        &mut self,
        key: ViewKey,
        access: &Arc<GraphAccess>,
        read: impl FnOnce(&Preparation) -> Result<(Arc<Snapshot>, Uuid)>,
    ) -> Result<(Uuid, usize)> {
        if let Some(views) = self.views.get(&key)
            && let Some(index) = views
                .iter()
                .position(|c| Arc::ptr_eq(&c.access, access) || *c.access == **access)
        {
            return Ok((views[index].served_by, index));
        }
        let (snapshot, served_by) = read(&self.preparation)?;
        // The read was checked against the remaining allowance; the snapshot is
        // retained while the view lives, so its state stays charged.
        let retained = self
            .preparation
            .reserve(snapshot.retained_preparation_bytes())?;
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
fn verify_passage(
    lookups: &mut Lookups<'_>,
    graphs: &[&NamedNode],
    protections: &BTreeMap<String, Uuid>,
    graph: &NamedNode,
    iri: &NamedNode,
    main: &Store,
    key: &str,
) -> Result<VerifiedSpan> {
    let rendition = match lookups
        .one(iri.as_str(), &format!("{OA}hasSource"), graph)
        .map_err(|e| classify(key, e))?
    {
        One::Some(Term::NamedNode(r)) => r,
        One::Many => return Err(conflict(key, "the passage names more than one rendition")),
        _ => return Err(span(key, "the passage names no rendition")),
    };
    let selector = match lookups
        .one(iri.as_str(), &format!("{OA}hasSelector"), graph)
        .map_err(|e| classify(key, e))?
    {
        One::Some(Term::NamedNode(s)) => s.as_str().to_string(),
        One::Some(Term::BlankNode(b)) => {
            // A blank selector of the stored record is addressed by its stored label.
            return verify_with_selector(
                lookups,
                graphs,
                protections,
                graph,
                iri,
                &rendition,
                Term::BlankNode(b),
                main,
                key,
            );
        }
        One::Many => return Err(conflict(key, "the passage has more than one selector")),
        _ => return Err(span(key, "the passage has no selector")),
    };
    let selector = NamedNode::new_unchecked(selector);
    verify_with_selector(
        lookups,
        graphs,
        protections,
        graph,
        iri,
        &rendition,
        Term::NamedNode(selector),
        main,
        key,
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
) -> Result<VerifiedSpan> {
    let position = |lookups: &mut Lookups<'_>, p: &str| -> Result<Option<u64>> {
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
            return Ok(None);
        };
        let ids = lookups.object_ids(s, p, g)?;
        let [id] = ids.as_slice() else {
            return Ok(None);
        };
        preparation.with_term(snap, *id, |t| {
            Ok(match t {
                Some(Term::Literal(l)) => integer(l),
                _ => None,
            })
        })
    };
    let start = position(lookups, "start").map_err(|e| classify(key, e))?;
    let end = position(lookups, "end").map_err(|e| classify(key, e))?;
    let (Some(start), Some(end)) = (start, end) else {
        return Err(span(
            key,
            "the passage selector has no single start and end",
        ));
    };
    if end < start {
        return Err(span(key, "the passage selector ends before it starts"));
    }
    let Some(text_digest) = single_digest(lookups, graph, iri, "textDigest", key)? else {
        return Err(span(key, "the passage has no text digest"));
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
    let control = lookups.preparation.control();
    let checked = lookups
        .one_text(
            rendition.as_str(),
            &format!("{EV}normalizedText"),
            &rendition_graph,
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
    Ok(VerifiedSpan {
        rendition: rendition.clone(),
        start,
        end,
        text_digest,
    })
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
