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
    Operation, PlannedRecord, Record, Stamp, ev, no_blank, prov, publish_quads, record_digest,
    string,
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
/// policy and the operation, so a replayed operation names the same decision.
#[allow(clippy::too_many_arguments)]
pub fn decision_record(
    dataset: Uuid,
    principal: &NamedNode,
    operation: Uuid,
    policy: &str,
    version: u32,
    d: &DecisionInput,
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
    r.add(ev("target"), w.target.clone());
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

/// The records of one review or withdrawal operation.
#[derive(Clone, Debug)]
pub struct Batch {
    pub operation: Uuid,
    pub operation_iri: NamedNode,
    pub payload_digest: Sha256Digest,
    pub principal: NamedNode,
    pub idempotency_key: Option<String>,
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
        publish_quads(
            &Operation {
                iri: &self.operation_iri,
                principal: &self.principal,
                payload_digest: self.payload_digest,
                idempotency_key: self.idempotency_key.as_deref(),
                hold: None,
            },
            &self.graphs,
            &self.records,
            stamp,
            &protection,
            skip,
        )
    }
}
