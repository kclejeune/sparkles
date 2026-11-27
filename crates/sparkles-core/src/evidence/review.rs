//! The record encoding of review decisions, acceptance supports, withdrawals and
//! projection records (spec C25 §5 and §6).
//!
//! A review decision is immutable. A later decision supersedes it by naming it in
//! `ev:supersedes`, and nothing is edited in place. An acceptance support records one
//! admitted claim occurrence that owns a projected triple. Whether a support is live is
//! derived, never recorded: its decision is authenticated and no authenticated decision
//! supersedes it, and its claim has no authenticated withdrawal. A projection record
//! names a policy version, its output graph, its conflict mode and the protection nonces
//! of the policy's decisions, supports and output graphs when the projection was first
//! written, so that a later detach and new protection of one of them is noticed.
//!
//! As in [`super::write`], the functions here hold no lock and authorize nothing.

use super::canonical::FrameHash;
use super::write::{
    Operation, PlannedRecord, Record, Stamp, ev, integer, no_blank, prov, publish_quads,
    record_digest, string,
};
use super::{ConflictMode, IdentityNamespace, RecordKey, RecordKind, Sha256Digest};
use crate::task::Control;
use crate::{Error, Result};
use oxrdf::{NamedNode, Quad, Term, Triple};
use uuid::Uuid;

/// The most decisions or withdrawals one request holds.
pub const MAX_DECISIONS: usize = 1000;
/// The most evidence records one review reads for its acceptances, links, references,
/// facts and contrary claims together.
pub const MAX_EVIDENCE: usize = 10_000;
/// The prefix of a review policy's IRI, followed by its name.
pub const POLICY_PREFIX: &str = "urn:x-sparkles:evidence:policy:";

/// The outcome of a review under its policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Disposition {
    Accepted,
    Rejected,
    NeedsEvidence,
}

impl Disposition {
    fn local(self) -> &'static str {
        match self {
            Disposition::Accepted => "Accepted",
            Disposition::Rejected => "Rejected",
            Disposition::NeedsEvidence => "NeedsEvidence",
        }
    }

    /// The disposition's profile IRI, such as `ev:Accepted`.
    pub fn iri(self) -> NamedNode {
        ev(self.local())
    }

    /// The disposition of a profile IRI.
    pub fn from_iri(iri: &str) -> Option<Disposition> {
        [
            Disposition::Accepted,
            Disposition::Rejected,
            Disposition::NeedsEvidence,
        ]
        .into_iter()
        .find(|d| ev(d.local()).as_str() == iri)
    }

    /// The request name, `accepted`, `rejected` or `needsEvidence`.
    pub fn name(self) -> &'static str {
        match self {
            Disposition::Accepted => "accepted",
            Disposition::Rejected => "rejected",
            Disposition::NeedsEvidence => "needsEvidence",
        }
    }

    pub fn from_name(name: &str) -> Option<Disposition> {
        [
            Disposition::Accepted,
            Disposition::Rejected,
            Disposition::NeedsEvidence,
        ]
        .into_iter()
        .find(|d| d.name() == name)
    }
}

/// One decision of a review request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionInput {
    /// The request's alias for the decision.
    pub key: String,
    /// The reviewed claim or assessment.
    pub target: NamedNode,
    /// The target's `ev:recordDigest` as the reviewer saw it.
    pub target_digest: Sha256Digest,
    pub disposition: Disposition,
    /// The current decision of the target under the policy, which this one supersedes.
    /// `None` when the target has no decision under the policy.
    pub supersedes: Option<NamedNode>,
}

/// One withdrawal of a withdrawal request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WithdrawalInput {
    pub key: String,
    /// The withdrawn claim occurrence.
    pub target: NamedNode,
    /// The claim's `ev:recordDigest` as the caller saw it.
    pub target_digest: Sha256Digest,
}

/// The IRI of the review policy `name`.
pub fn policy_iri(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{POLICY_PREFIX}{name}"))
}

/// The payload digest of a review request, compared when its idempotency key repeats.
pub fn review_payload_digest(
    policy: &str,
    version: u32,
    decisions: &[DecisionInput],
    control: &Control,
) -> Result<Sha256Digest> {
    let mut frame = FrameHash::new(b"sparkles-evidence-review-v1", control)?;
    frame.field(1, policy.as_bytes())?;
    frame.field(2, &version.to_be_bytes())?;
    for d in decisions {
        control.check()?;
        frame.field(3, d.key.as_bytes())?;
        frame.field(4, d.target.as_str().as_bytes())?;
        frame.field(5, d.target_digest.as_bytes())?;
        frame.field(6, d.disposition.name().as_bytes())?;
        frame.field(
            7,
            d.supersedes.as_ref().map_or("", |s| s.as_str()).as_bytes(),
        )?;
    }
    frame.finish()
}

/// The payload digest of a withdrawal request.
pub fn withdrawal_payload_digest(
    withdrawals: &[WithdrawalInput],
    control: &Control,
) -> Result<Sha256Digest> {
    let mut frame = FrameHash::new(b"sparkles-evidence-withdrawal-v1", control)?;
    for w in withdrawals {
        control.check()?;
        frame.field(3, w.key.as_bytes())?;
        frame.field(4, w.target.as_str().as_bytes())?;
        frame.field(5, w.target_digest.as_bytes())?;
    }
    frame.finish()
}

/// The payload digest of a resolution-map rebuild request, compared when its
/// idempotency key repeats.
pub fn rebuild_map_payload_digest(
    policy: &str,
    version: Option<u32>,
    control: &Control,
) -> Result<Sha256Digest> {
    let mut frame = FrameHash::new(b"sparkles-evidence-map-rebuild-v1", control)?;
    frame.field(1, policy.as_bytes())?;
    if let Some(version) = version {
        frame.field(2, &version.to_be_bytes())?;
    }
    frame.finish()
}

/// The payload digest of a projection reconciliation request.
pub fn reconcile_payload_digest(
    policy: &str,
    version: Option<u32>,
    control: &Control,
) -> Result<Sha256Digest> {
    let mut frame = FrameHash::new(b"sparkles-evidence-reconcile-projection-v1", control)?;
    frame.field(1, policy.as_bytes())?;
    if let Some(v) = version {
        frame.field(2, &v.to_be_bytes())?;
    }
    frame.finish()
}

fn planned(
    key: &str,
    kind: RecordKind,
    r: Record,
    graph: &NamedNode,
    control: &Control,
) -> Result<PlannedRecord> {
    let digest = record_digest(kind, &r.triples, control)?;
    Ok(PlannedRecord {
        key: key.to_string(),
        kind,
        iri: r.subject,
        graph: graph.clone(),
        digest,
        triples: r.triples,
    })
}

fn mint(
    dataset: Uuid,
    agent: &NamedNode,
    kind: RecordKind,
    scope: &str,
    occurrence: &str,
    control: &Control,
) -> Result<NamedNode> {
    IdentityNamespace::new(dataset, agent.clone())?.record_id(
        kind,
        &RecordKey::new(scope)?,
        &RecordKey::new(occurrence)?,
        control,
    )
}

/// The decision `d` of the operation `operation` by `principal` under the policy
/// `policy` at `version`, in `graph`. Its identity is minted from the target, the
/// policy and the operation, so a replayed operation names the same decision. An
/// acceptance that does not project its claim records why as `ev:projectionNote`.
#[allow(clippy::too_many_arguments)]
pub fn decision_record(
    dataset: Uuid,
    principal: &NamedNode,
    operation: Uuid,
    policy: &str,
    version: u32,
    d: &DecisionInput,
    projection_note: Option<&str>,
    graph: &NamedNode,
    control: &Control,
) -> Result<PlannedRecord> {
    let iri = mint(
        dataset,
        principal,
        RecordKind::ReviewDecision,
        d.target.as_str(),
        &format!("{policy} {operation}"),
        control,
    )?;
    let mut r = Record::new(&iri, RecordKind::ReviewDecision);
    r.add(ev("target"), d.target.clone());
    r.add(ev("targetDigest"), string(&d.target_digest.to_string()));
    r.add(ev("reviewPolicy"), policy_iri(policy));
    r.add(ev("policyVersion"), string(&version.to_string()));
    r.add(ev("disposition"), d.disposition.iri());
    if let Some(prior) = &d.supersedes {
        r.add(ev("supersedes"), prior.clone());
    }
    if let Some(note) = projection_note {
        r.add(ev("projectionNote"), string(note));
    }
    r.add(prov("wasAttributedTo"), principal.clone());
    planned(&d.key, RecordKind::ReviewDecision, r, graph, control)
}

/// The acceptance support of the claim `claim` admitted by `decision` under `policy`,
/// owning `triple` in `output`, in the supports graph `graph`. `conflicts` names the
/// contrary links and claims an `admitLabelled` policy admitted it against.
#[allow(clippy::too_many_arguments)]
pub fn support_record(
    dataset: Uuid,
    principal: &NamedNode,
    key: &str,
    decision: &NamedNode,
    claim: &NamedNode,
    policy: &str,
    output: &NamedNode,
    triple: &Triple,
    conflicts: &[NamedNode],
    graph: &NamedNode,
    control: &Control,
) -> Result<PlannedRecord> {
    no_blank("supportedTriple", &Term::Triple(Box::new(triple.clone())))?;
    let iri = mint(
        dataset,
        principal,
        RecordKind::AcceptanceSupport,
        decision.as_str(),
        "support",
        control,
    )?;
    let mut r = Record::new(&iri, RecordKind::AcceptanceSupport);
    r.add(ev("claim"), claim.clone());
    r.add(ev("decision"), decision.clone());
    r.add(ev("reviewPolicy"), policy_iri(policy));
    r.add(ev("output"), output.clone());
    r.add(
        ev("supportedTriple"),
        Term::Triple(Box::new(triple.clone())),
    );
    for c in conflicts {
        r.add(ev("conflictsWith"), c.clone());
    }
    planned(key, RecordKind::AcceptanceSupport, r, graph, control)
}

/// The support identity a decision admits, as [`support_record`] mints it.
pub fn support_iri(
    dataset: Uuid,
    principal: &NamedNode,
    decision: &NamedNode,
    control: &Control,
) -> Result<NamedNode> {
    mint(
        dataset,
        principal,
        RecordKind::AcceptanceSupport,
        decision.as_str(),
        "support",
        control,
    )
}

/// The withdrawal `w` of the operation `operation` by `principal`, in the claim's own
/// graph `graph`. The server adds `prov:invalidatedAtTime`, the commit's time.
pub fn withdrawal_record(
    dataset: Uuid,
    principal: &NamedNode,
    operation: Uuid,
    w: &WithdrawalInput,
    graph: &NamedNode,
    control: &Control,
) -> Result<PlannedRecord> {
    let iri = mint(
        dataset,
        principal,
        RecordKind::Withdrawal,
        w.target.as_str(),
        &operation.to_string(),
        control,
    )?;
    let mut r = Record::new(&iri, RecordKind::Withdrawal);
    r.add(ev("withdraws"), w.target.clone());
    r.add(ev("targetDigest"), string(&w.target_digest.to_string()));
    r.add(prov("wasAttributedTo"), principal.clone());
    planned(&w.key, RecordKind::Withdrawal, r, graph, control)
}

/// The protection nonces of a policy's decisions, supports and output graphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProjectionNonces {
    pub decisions: Uuid,
    pub supports: Uuid,
    pub output: Uuid,
}

/// The predicates of a projection record's nonces.
pub const NONCE_PREDICATES: [&str; 3] = [
    "urn:x-sparkles:evidence:decisionsProtection",
    "urn:x-sparkles:evidence:supportsProtection",
    "urn:x-sparkles:evidence:outputProtection",
];

/// The predicate of a resolution map revision's boundary protections. Each value is
/// the literal `"<nonce> <graph IRI>"` of one protected identity graph of the policy's
/// boundary when the revision was written.
pub const BOUNDARY_PROTECTION: &str = "urn:x-sparkles:evidence:boundaryProtection";

/// The value of [`BOUNDARY_PROTECTION`] for `graph` under `nonce`.
pub fn boundary_protection(graph: &NamedNode, nonce: Uuid) -> String {
    format!("{nonce} {}", graph.as_str())
}

/// Predicate linking graph-local, nonce-scoped `ev:Projection` index subjects to their
/// `ev:Projection` record IRI within the same supports graph so drift and supersession
/// checks run strictly on `Perm::Gspo` without scanning foreign graphs.
pub const INDEXED_PROJECTION: &str = "urn:x-sparkles:evidence:indexedProjection";

fn projection_index_iri(kind: &str, domain: &[u8], parts: &[&[u8]]) -> NamedNode {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut h = Sha256::new();
    h.update(domain);
    for part in parts {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part);
    }
    let digest = h.finalize();
    let mut iri = String::with_capacity(32 + kind.len() + 64);
    let _ = write!(iri, "urn:x-sparkles:evidence:index:{kind}:");
    for b in digest {
        let _ = write!(iri, "{b:02x}");
    }
    NamedNode::new_unchecked(iri)
}

/// Graph-local `Perm::Gspo` index subject for `ev:Projection` records of
/// `(policy_iri, policy_version)` under `supports_nonce`.
pub fn projection_policy_index_iri(
    supports_nonce: Uuid,
    policy_iri: &str,
    policy_version: u32,
) -> NamedNode {
    projection_index_iri(
        "policy",
        b"sparkles-projection-policy-index-v1\0",
        &[
            supports_nonce.as_bytes(),
            policy_iri.as_bytes(),
            &policy_version.to_be_bytes(),
        ],
    )
}

/// Graph-local `Perm::Gspo` index subject for `ev:Projection` records targeting
/// `(output_graph, output_nonce)` under `supports_nonce`.
pub fn projection_output_index_iri(
    supports_nonce: Uuid,
    output_nonce: Uuid,
    output_graph: &str,
) -> NamedNode {
    projection_index_iri(
        "output",
        b"sparkles-projection-output-index-v1\0",
        &[
            supports_nonce.as_bytes(),
            output_nonce.as_bytes(),
            output_graph.as_bytes(),
        ],
    )
}

/// Graph-local `Perm::Gspo` index subject for `ev:Projection` records superseding
/// `prior` through `prov:wasRevisionOf` under `supports_nonce`.
pub fn projection_revision_index_iri(supports_nonce: Uuid, prior: &str) -> NamedNode {
    projection_index_iri(
        "revision",
        b"sparkles-projection-revision-index-v1\0",
        &[supports_nonce.as_bytes(), prior.as_bytes()],
    )
}

/// Build the graph-local `Perm::Gspo` index quads for an `ev:Projection` record
/// `proj_iri` in `graph` under `supports_nonce`, `output_nonce`,
/// `(policy_iri, policy_version)`, `output_graph`, and its `prov:wasRevisionOf`
/// predecessors `priors`.
#[allow(clippy::too_many_arguments)]
pub fn projection_index_quads(
    proj_iri: &NamedNode,
    graph: &NamedNode,
    supports_nonce: Uuid,
    output_nonce: Uuid,
    policy_iri: &str,
    policy_version: u32,
    output_graph: &str,
    priors: &[&str],
) -> Vec<Quad> {
    let pred = NamedNode::new_unchecked(INDEXED_PROJECTION);
    let g = oxrdf::GraphName::NamedNode(graph.clone());
    let mut out = Vec::with_capacity(2 + priors.len());
    out.push(Quad::new(
        projection_policy_index_iri(supports_nonce, policy_iri, policy_version),
        pred.clone(),
        proj_iri.clone(),
        g.clone(),
    ));
    out.push(Quad::new(
        projection_output_index_iri(supports_nonce, output_nonce, output_graph),
        pred.clone(),
        proj_iri.clone(),
        g.clone(),
    ));
    for prior in priors {
        out.push(Quad::new(
            projection_revision_index_iri(supports_nonce, prior),
            pred.clone(),
            proj_iri.clone(),
            g.clone(),
        ));
    }
    out
}

fn projection_index_quads_for_record(r: &PlannedRecord) -> Vec<Quad> {
    if r.kind != RecordKind::Projection {
        return Vec::new();
    }
    let review_policy = ev("reviewPolicy");
    let policy_version = ev("policyVersion");
    let output_pred = ev("output");
    let supports_prot = NamedNode::new_unchecked(NONCE_PREDICATES[1]);
    let output_prot = NamedNode::new_unchecked(NONCE_PREDICATES[2]);
    let was_revision_of = prov("wasRevisionOf");

    let mut policy: Option<&str> = None;
    let mut version: Option<u32> = None;
    let mut output: Option<&str> = None;
    let mut sup_nonce: Option<Uuid> = None;
    let mut out_nonce: Option<Uuid> = None;
    let mut priors: Vec<&str> = Vec::new();
    for t in &r.triples {
        if t.predicate == review_policy
            && let Term::NamedNode(n) = &t.object
        {
            policy = Some(n.as_str());
        } else if t.predicate == policy_version
            && let Term::Literal(l) = &t.object
        {
            version = l.value().parse().ok();
        } else if t.predicate == output_pred
            && let Term::NamedNode(n) = &t.object
        {
            output = Some(n.as_str());
        } else if t.predicate == supports_prot
            && let Term::Literal(l) = &t.object
        {
            sup_nonce = Uuid::parse_str(l.value()).ok();
        } else if t.predicate == output_prot
            && let Term::Literal(l) = &t.object
        {
            out_nonce = Uuid::parse_str(l.value()).ok();
        } else if t.predicate == was_revision_of
            && let Term::NamedNode(n) = &t.object
        {
            priors.push(n.as_str());
        }
    }
    let (Some(policy), Some(version), Some(output), Some(sup_nonce), Some(out_nonce)) =
        (policy, version, output, sup_nonce, out_nonce)
    else {
        return Vec::new();
    };
    projection_index_quads(
        &r.iri, &r.graph, sup_nonce, out_nonce, policy, version, output, &priors,
    )
}

/// The projection record of `policy` at `version` in its supports graph `graph`. Its
/// identity is minted in the policy's own namespace from the version, so every
/// reviewer of that version finds the same record.
#[allow(clippy::too_many_arguments)]
pub fn projection_record(
    dataset: Uuid,
    policy: &str,
    version: u32,
    output: &NamedNode,
    mode: ConflictMode,
    nonces: ProjectionNonces,
    graph: &NamedNode,
    control: &Control,
) -> Result<PlannedRecord> {
    let iri = projection_iri(dataset, policy, version, control)?;
    let mut r = Record::new(&iri, RecordKind::Projection);
    r.add(ev("reviewPolicy"), policy_iri(policy));
    r.add(ev("policyVersion"), string(&version.to_string()));
    r.add(ev("output"), output.clone());
    r.add(
        ev("conflictMode"),
        ev(match mode {
            ConflictMode::Refuse => "Refuse",
            ConflictMode::AdmitLabelled => "AdmitLabelled",
        }),
    );
    for (p, n) in NONCE_PREDICATES
        .iter()
        .zip([nonces.decisions, nonces.supports, nonces.output])
    {
        r.add(NamedNode::new_unchecked(*p), string(&n.to_string()));
    }
    planned("projection", RecordKind::Projection, r, graph, control)
}

/// The identity of the projection record of `policy` at `version`.
pub fn projection_iri(
    dataset: Uuid,
    policy: &str,
    version: u32,
    control: &Control,
) -> Result<NamedNode> {
    mint(
        dataset,
        &policy_iri(policy),
        RecordKind::Projection,
        policy,
        &version.to_string(),
        control,
    )
}

/// The identity of a reconciled projection record of `policy` at `version` under the
/// protection nonces `nonces` and `operation`. Including both the three nonces and the
/// operation keeps it distinct from the initial projection record, from earlier
/// protection epochs and from repeated repairs under the same version and nonces.
pub fn reconciled_projection_iri(
    dataset: Uuid,
    policy: &str,
    version: u32,
    nonces: ProjectionNonces,
    operation: Uuid,
    control: &Control,
) -> Result<NamedNode> {
    mint(
        dataset,
        &policy_iri(policy),
        RecordKind::Projection,
        policy,
        &format!(
            "{version} {} {} {} {operation}",
            nonces.decisions, nonces.supports, nonces.output
        ),
        control,
    )
}

/// A reconciled projection record of `policy` at `version` in its supports graph
/// `graph`, carrying the current protection `nonces` and superseding `supersedes`
/// through `prov:wasRevisionOf`.
#[allow(clippy::too_many_arguments)]
pub fn reconciled_projection_record(
    dataset: Uuid,
    policy: &str,
    version: u32,
    output: &NamedNode,
    mode: ConflictMode,
    nonces: ProjectionNonces,
    operation: Uuid,
    supersedes: &[NamedNode],
    graph: &NamedNode,
    control: &Control,
) -> Result<PlannedRecord> {
    let iri = reconciled_projection_iri(dataset, policy, version, nonces, operation, control)?;
    let mut r = Record::new(&iri, RecordKind::Projection);
    r.add(ev("reviewPolicy"), policy_iri(policy));
    r.add(ev("policyVersion"), string(&version.to_string()));
    r.add(ev("output"), output.clone());
    r.add(
        ev("conflictMode"),
        ev(match mode {
            ConflictMode::Refuse => "Refuse",
            ConflictMode::AdmitLabelled => "AdmitLabelled",
        }),
    );
    for (p, n) in NONCE_PREDICATES
        .iter()
        .zip([nonces.decisions, nonces.supports, nonces.output])
    {
        r.add(NamedNode::new_unchecked(*p), string(&n.to_string()));
    }
    let mut prior: Vec<&NamedNode> = supersedes.iter().collect();
    prior.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    prior.dedup();
    r.triples.reserve(7 + prior.len());
    for p in prior {
        r.add(prov("wasRevisionOf"), p.clone());
    }
    planned("projection", RecordKind::Projection, r, graph, control)
}

/// The protection nonces of a matching policy's decisions, maps and output graphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapNonces {
    pub decisions: Uuid,
    pub maps: Uuid,
    pub output: Uuid,
}

/// The predicates of a resolution map revision's nonces.
pub const MAP_NONCE_PREDICATES: [&str; 3] = [
    "urn:x-sparkles:evidence:decisionsProtection",
    "urn:x-sparkles:evidence:mapsProtection",
    "urn:x-sparkles:evidence:outputProtection",
];

/// The identity of revision `number` of the resolution map of the matching policy
/// `policy`. It is minted in the policy's own namespace from the number, so two
/// operations that would publish the same revision mint the same identity.
pub fn map_iri(dataset: Uuid, policy: &str, number: u64, control: &Control) -> Result<NamedNode> {
    mint(
        dataset,
        &policy_iri(policy),
        RecordKind::ResolutionMap,
        policy,
        &format!("revision {number}"),
        control,
    )
}

/// What one resolution map revision records.
#[derive(Clone, Debug)]
pub struct MapRevision<'a> {
    pub policy: &'a str,
    pub version: u32,
    /// The revision's number, from 1.
    pub number: u64,
    /// The revision this one follows, `None` for the first.
    pub previous: Option<&'a NamedNode>,
    pub output: &'a NamedNode,
    /// The accepted decisions this revision makes effective.
    pub adds: &'a [NamedNode],
    /// The accepted decisions whose effect this revision ends, because a decision of its
    /// operation supersedes them.
    pub ends: &'a [NamedNode],
    pub nonces: MapNonces,
    /// The protected identity graphs of the policy's boundary and their nonces, whose
    /// proposals the map was built from.
    pub boundary: &'a [(NamedNode, Uuid)],
}

/// Revision `m.number` of a matching policy's resolution map, in its maps graph
/// `graph`. The map at a revision is the effect of the accepted decisions its chain
/// adds and has not ended, and every revision stays an immutable record.
pub fn map_record(
    dataset: Uuid,
    m: &MapRevision<'_>,
    graph: &NamedNode,
    control: &Control,
) -> Result<PlannedRecord> {
    let iri = map_iri(dataset, m.policy, m.number, control)?;
    let mut r = Record::new(&iri, RecordKind::ResolutionMap);
    r.add(ev("reviewPolicy"), policy_iri(m.policy));
    r.add(ev("policyVersion"), string(&m.version.to_string()));
    r.add(ev("mapRevision"), integer(m.number));
    if let Some(previous) = m.previous {
        r.add(prov("wasRevisionOf"), previous.clone());
    }
    r.add(ev("output"), m.output.clone());
    for d in m.adds {
        r.add(ev("adds"), d.clone());
    }
    for d in m.ends {
        r.add(ev("ends"), d.clone());
    }
    for (p, n) in
        MAP_NONCE_PREDICATES
            .iter()
            .zip([m.nonces.decisions, m.nonces.maps, m.nonces.output])
    {
        r.add(NamedNode::new_unchecked(*p), string(&n.to_string()));
    }
    for (g, n) in m.boundary {
        r.add(
            NamedNode::new_unchecked(BOUNDARY_PROTECTION),
            string(&boundary_protection(g, *n)),
        );
    }
    planned("map", RecordKind::ResolutionMap, r, graph, control)
}

/// The records of one review or withdrawal operation.
#[derive(Clone, Debug)]
pub struct Batch {
    pub operation: Uuid,
    pub operation_iri: NamedNode,
    pub payload_digest: Sha256Digest,
    pub principal: NamedNode,
    pub idempotency_key: Option<String>,
    /// An existing record this operation reused, written as `prov:used` on the
    /// operation so keyed replays recover the first receipt.
    pub used: Option<NamedNode>,
    /// The record graphs that describe the operation, sorted. A projection graph is
    /// never among them.
    pub graphs: Vec<NamedNode>,
    pub records: Vec<PlannedRecord>,
}

impl Batch {
    /// The operation's description in each of its graphs and every record not left
    /// out by `skip`, with the server-owned fields, given the commit's `stamp`.
    pub fn quads(
        &self,
        stamp: &Stamp,
        protection: impl Fn(&NamedNode) -> Option<Uuid>,
        skip: &[bool],
    ) -> Result<Vec<Quad>> {
        if let Some(r) = self
            .records
            .iter()
            .find(|r| !self.graphs.contains(&r.graph))
        {
            return Err(Error::invalid(format!(
                "record {} is in a graph the operation does not describe",
                r.key
            )));
        }
        let mut out = publish_quads(
            &Operation {
                iri: &self.operation_iri,
                principal: &self.principal,
                payload_digest: self.payload_digest,
                idempotency_key: self.idempotency_key.as_deref(),
                hold: None,
                used: self.used.as_ref(),
            },
            &self.graphs,
            &self.records,
            stamp,
            &protection,
            skip,
        )?;
        for (i, r) in self.records.iter().enumerate() {
            if !skip.get(i).copied().unwrap_or(false) && r.kind == RecordKind::Projection {
                out.extend(projection_index_quads_for_record(r));
            }
        }
        Ok(out)
    }
}
