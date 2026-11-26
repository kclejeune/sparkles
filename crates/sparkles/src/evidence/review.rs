//! Review decisions, accepted projections and withdrawals (spec C25 §5 and §6).
//!
//! [`review`] appends immutable decisions about exact claim and assessment revisions
//! under one named review policy. An acceptance of a positive claim that the policy
//! admits records an acceptance support in the policy's supports graph and projects the
//! claim's proposition into the policy's output graph. [`withdraw`] records that claim
//! occurrences are withdrawn. Neither edits a record in place.
//!
//! A support is live while its decision is current, which means no authenticated
//! decision supersedes it, and its claim has no authenticated withdrawal. A projected
//! triple stays in the output graph while one live support owns it, so two occurrences
//! of one proposition each own one support, and withdrawing or reversing one of them
//! leaves the triple to the other.
//!
//! Every read of an operation happens at one head: the head must be the expected one
//! before the reads start, and the branch writer refuses the commit unless that head is
//! still its base, so the reads and the commit see the same state. The operation is one
//! commit of the evidence owner, made as [`super::propose`] makes it, with the caller's
//! authority checked right before publication. A review or withdrawal records no new
//! stored-fact reference, so it takes no source hold.

use super::propose::{AuthorityFence, READS_BYTES, managed, publish_hook, settings_of};
use super::resolve::{RecordRequest, Resolution, unavailable};
use crate::access::GraphAccess;
use crate::managed::{ManagedErrorKind, ManagedWrite, OwnerKind};
use crate::task::Control;
use crate::{Catalog, Dataset, Error, Result};
use oxrdf::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use sparkles_core::evidence::review::{
    self as rv, Batch, DecisionInput, Disposition, MAX_DECISIONS, MAX_EVIDENCE, ProjectionNonces,
    WithdrawalInput,
};
use sparkles_core::evidence::write::{Reads, Stamp, operation_id, operation_iri};
use sparkles_core::evidence::{
    BlankAnchor, ClaimKind, ConflictMode, GraphIdentity, Policy, RecordKind, Settings,
    StoredFactRef,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use uuid::Uuid;

pub use sparkles_core::evidence::review::{POLICY_PREFIX, policy_iri};

#[path = "claims.rs"]
pub mod claims;
#[path = "dependents.rs"]
pub mod dependents;
#[path = "identity.rs"]
pub mod identity;

const EV: &str = "urn:x-sparkles:evidence:";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const PROV_WAS_REVISION_OF: &str = "http://www.w3.org/ns/prov#wasRevisionOf";
const PROV_WAS_GENERATED_BY: &str = "http://www.w3.org/ns/prov#wasGeneratedBy";
const PROV_USED: &str = "http://www.w3.org/ns/prov#used";

fn ev(local: &str) -> String {
    format!("{EV}{local}")
}

fn conflict_mode_iri(mode: ConflictMode) -> String {
    ev(match mode {
        ConflictMode::Refuse => "Refuse",
        ConflictMode::AdmitLabelled => "AdmitLabelled",
    })
}

/// What a review or withdrawal shares with a proposal: the caller and the commit.
#[derive(Clone)]
pub struct Options {
    /// The authenticated principal, the reviewer or withdrawer of record.
    pub principal: NamedNode,
    /// Makes a retry with the same payload return the first receipt.
    pub idempotency_key: Option<String>,
    /// Publish only while this commit is the branch head. A review requires it.
    pub if_head: Option<u64>,
    /// Check and preview the write without committing it.
    pub dry_run: bool,
    pub message: Option<Arc<str>>,
    pub author: Option<Arc<str>>,
    /// What the caller may write on the branch, `None` for every graph.
    pub write: Option<Arc<GraphAccess>>,
    /// The caller's authority, checked right before publication.
    pub authority: Option<Arc<dyn AuthorityFence>>,
}

impl Options {
    pub fn new(principal: NamedNode) -> Options {
        Options {
            principal,
            idempotency_key: None,
            if_head: None,
            dry_run: false,
            message: None,
            author: None,
            write: None,
            authority: None,
        }
    }
}

/// A projected triple added to or removed from an output graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionChange {
    pub output: NamedNode,
    pub triple: Triple,
}

/// The identities an operation changed, for consumers that track dependent results.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    /// Supports this operation recorded.
    pub supports_added: Vec<NamedNode>,
    /// Supports that stopped being live because a decision was superseded or a claim
    /// withdrawn.
    pub supports_ended: Vec<NamedNode>,
    /// Triples projected into an output graph.
    pub inserted: Vec<ProjectionChange>,
    /// Triples removed from an output graph because no live support owns them, or
    /// because a resolution map no longer maps an entity so.
    pub removed: Vec<ProjectionChange>,
    /// The new revision of a resolution map, when a review of identity proposals
    /// changed which decisions the map follows.
    pub map: Option<MapChange>,
}

impl Changed {
    /// The record identities whose dependents may need review: the supports that
    /// ended, and for a new map revision the revision it follows and the decisions
    /// whose effect ended. Claims an operation withdrew and decisions it superseded are
    /// named in its own result. The result is sorted and has no duplicates, ready for
    /// [`dependents::dependents`].
    pub fn identities(&self) -> Vec<NamedNode> {
        let mut out: BTreeSet<NamedNode> = self.supports_ended.iter().cloned().collect();
        if let Some(map) = &self.map {
            out.extend(map.previous.iter().cloned());
            out.extend(map.decisions_ended.iter().cloned());
        }
        out.into_iter().collect()
    }
}

/// A new revision of a matching policy's resolution map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapChange {
    pub policy: NamedNode,
    pub revision: NamedNode,
    /// The revision's number, from 1.
    pub number: u64,
    /// The revision it follows, whose consumers may need review.
    pub previous: Option<NamedNode>,
    /// The accepted decisions that became effective.
    pub decisions_added: Vec<NamedNode>,
    /// The accepted decisions whose effect ended.
    pub decisions_ended: Vec<NamedNode>,
    /// The entities whose selected entity changed, sorted.
    pub entities: Vec<NamedNode>,
}

/// One recorded decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decided {
    pub key: String,
    pub iri: NamedNode,
    pub target: NamedNode,
    pub disposition: Disposition,
    pub supersedes: Option<NamedNode>,
    /// The acceptance support the decision recorded, when it projected its claim.
    pub support: Option<NamedNode>,
    /// Why an acceptance did not project, `None` when it projected or is no acceptance.
    pub not_projected: Option<&'static str>,
    /// The contrary links and claims an `admitLabelled` policy admitted it against.
    pub conflicts: Vec<NamedNode>,
}

/// The outcome of [`review`].
#[derive(Debug)]
pub struct Reviewed {
    pub operation: NamedNode,
    pub committed: bool,
    pub replayed: bool,
    pub commit: u64,
    pub branch: Uuid,
    pub decisions: Vec<Decided>,
    /// What changed. A replay reports nothing here.
    pub changed: Changed,
    /// The resolution map revision the operation published, also for a replay.
    pub map_revision: Option<NamedNode>,
    pub preview: Option<Box<crate::preview::Preview>>,
}

/// One withdrawal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WithdrawnClaim {
    pub key: String,
    /// The withdrawal record, `None` when the claim was already withdrawn.
    pub iri: Option<NamedNode>,
    pub target: NamedNode,
    /// Whether an earlier withdrawal had already withdrawn the claim.
    pub existing: bool,
}

/// The outcome of [`withdraw`].
#[derive(Debug)]
pub struct Withdrawn {
    pub operation: NamedNode,
    pub committed: bool,
    pub replayed: bool,
    pub commit: u64,
    pub branch: Uuid,
    pub withdrawals: Vec<WithdrawnClaim>,
    pub changed: Changed,
    pub preview: Option<Box<crate::preview::Preview>>,
}

/// The outcome of [`reconcile_projection`].
#[derive(Debug)]
pub struct ReconciledProjection {
    pub operation: NamedNode,
    pub committed: bool,
    pub replayed: bool,
    pub commit: u64,
    pub branch: Uuid,
    pub policy: NamedNode,
    pub policy_version: u32,
    /// The output graph recorded in the projection record.
    pub output: NamedNode,
    /// The active projection record that carries the policy's current protections, or the
    /// projection record selected by the original operation on a replay.
    pub projection: NamedNode,
    /// The live authenticated acceptance supports vouched for under the current
    /// protections, sorted by IRI. Empty on a replay.
    pub vouched_supports: Vec<NamedNode>,
    /// What changed in the output graph. A replay reports nothing here.
    pub changed: Changed,
    pub preview: Option<Box<crate::preview::Preview>>,
}

// ------------------------------------------------------------------- reads -------

/// A graph and its current protection nonce.
type G = (NamedNode, Option<Uuid>);

/// A resolution outcome about a record the caller must be able to read answers as its
/// decision's target being unavailable, so that it discloses nothing about the record.
fn hidden(key: &str, e: Error) -> Error {
    match &e {
        Error::Managed(m)
            if matches!(
                m.code,
                "evidence-unavailable"
                    | "origin-not-reproduced"
                    | "immutable-record-conflict"
                    | "span-mismatch"
                    | "unanchored-evidence"
            ) =>
        {
            unavailable(key)
        }
        _ => e,
    }
}

fn incomplete(what: &str) -> Error {
    managed(
        ManagedErrorKind::IncompleteEvidence,
        format!("{what} exceed {MAX_EVIDENCE} records, so nothing was written"),
    )
}

/// The derived state an operation reads at its head.
struct State<'a> {
    reads: Reads<'a>,
    /// The caller's view of the head when its access hides triples.
    seen: Option<Reads<'a>>,
    /// The proposal graphs, where claims and their withdrawals are.
    claim_graphs: Vec<G>,
    /// Decisions this operation supersedes.
    superseded: BTreeSet<NamedNode>,
    /// Claims this operation withdraws.
    withdrawn: BTreeSet<NamedNode>,
}

impl State<'_> {
    fn typed(&mut self, s: &NamedNode, g: &G, kind: RecordKind) -> Result<bool> {
        Ok(self.reads.contains(
            s.as_str(),
            RDF_TYPE,
            &Term::NamedNode(NamedNode::new_unchecked(super::resolve::kind_iri(kind))),
            g.0.as_str(),
        )? && self.reads.authenticated(s.as_str(), g.0.as_str(), g.1)?)
    }

    fn subjects(&mut self, p: &str, o: &Term, g: &G, what: &str) -> Result<Vec<NamedNode>> {
        let (v, more) = self
            .reads
            .subjects_up_to(p, o, g.0.as_str(), MAX_EVIDENCE)?;
        if more {
            return Err(incomplete(what));
        }
        Ok(v)
    }

    fn subjects_held(
        &mut self,
        p: &str,
        o: &Term,
        g: &G,
        what: &str,
        held: &mut crate::access::Reservation,
    ) -> Result<Vec<NamedNode>> {
        let (v, more) = self
            .reads
            .subjects_up_to_held(p, o, g.0.as_str(), MAX_EVIDENCE, held)?;
        if more {
            return Err(incomplete(what));
        }
        Ok(v)
    }

    /// Up to [`MAX_EVIDENCE`] subjects of `?s p o` in `g`, and whether more exist. The
    /// caller resolves them in order before it reports the bound, so that a hidden
    /// record is refused before a count that includes it is reported.
    fn candidates(&mut self, p: &str, o: &Term, g: &G) -> Result<(Vec<NamedNode>, bool)> {
        self.reads.subjects_up_to(p, o, g.0.as_str(), MAX_EVIDENCE)
    }

    /// Refuse as `key`'s unavailable target unless every triple of `s` in `g` is in the
    /// caller's view, which hides none when its access has no triple protections.
    fn visible(&mut self, key: &str, s: &NamedNode, g: &NamedNode) -> Result<()> {
        if self.masked(s, g)? {
            return Err(unavailable(key));
        }
        Ok(())
    }

    /// Whether the caller's view hides a triple of `s` in `g`.
    fn masked(&mut self, s: &NamedNode, g: &NamedNode) -> Result<bool> {
        let Some(seen) = self.seen.as_mut() else {
            return Ok(false);
        };
        const LIMIT: usize = 1 << 20;
        let all = self.reads.subject_triples(s.as_str(), g.as_str(), LIMIT)?;
        Ok(seen.subject_triples(s.as_str(), g.as_str(), LIMIT)? != all)
    }

    fn iri(&mut self, s: &NamedNode, p: &str, g: &G) -> Result<Option<NamedNode>> {
        Ok(match self.reads.single(s.as_str(), p, g.0.as_str())? {
            Some(Term::NamedNode(n)) => Some(n),
            _ => None,
        })
    }

    fn text(&mut self, s: &NamedNode, p: &str, g: &G) -> Result<Option<String>> {
        Ok(match self.reads.single(s.as_str(), p, g.0.as_str())? {
            Some(Term::Literal(l)) => Some(l.value().to_string()),
            _ => None,
        })
    }

    /// Whether an authenticated decision in `decisions` supersedes `d`, or this
    /// operation does.
    fn is_superseded(&mut self, d: &NamedNode, decisions: &G) -> Result<bool> {
        if self.superseded.contains(d) {
            return Ok(true);
        }
        for later in self.subjects(
            &ev("supersedes"),
            &Term::NamedNode(d.clone()),
            decisions,
            "decisions",
        )? {
            if self.typed(&later, decisions, RecordKind::ReviewDecision)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The current decisions of `target` under `policy` in `decisions`: authenticated
    /// decisions that nothing supersedes. A target has at most one.
    fn current(
        &mut self,
        target: &NamedNode,
        policy: &NamedNode,
        decisions: &G,
    ) -> Result<Vec<NamedNode>> {
        let mut out = Vec::new();
        for d in self.subjects(
            &ev("target"),
            &Term::NamedNode(target.clone()),
            decisions,
            "decisions",
        )? {
            if self.typed(&d, decisions, RecordKind::ReviewDecision)?
                && self.reads.contains(
                    d.as_str(),
                    &ev("reviewPolicy"),
                    &Term::NamedNode(policy.clone()),
                    decisions.0.as_str(),
                )?
                && !self.is_superseded(&d, decisions)?
            {
                out.push(d);
            }
        }
        Ok(out)
    }

    fn disposition(&mut self, d: &NamedNode, decisions: &G) -> Result<Option<Disposition>> {
        Ok(self
            .iri(d, &ev("disposition"), decisions)?
            .and_then(|i| Disposition::from_iri(i.as_str())))
    }

    /// The authenticated withdrawal of `claim` in its graph `g`, if any.
    fn withdrawal(&mut self, claim: &NamedNode, g: &G) -> Result<Option<NamedNode>> {
        for w in self.subjects(
            &ev("withdraws"),
            &Term::NamedNode(claim.clone()),
            g,
            "withdrawals",
        )? {
            if self.typed(&w, g, RecordKind::Withdrawal)? {
                return Ok(Some(w));
            }
        }
        Ok(None)
    }

    /// Whether `claim` is withdrawn in one of the proposal graphs, also by this
    /// operation.
    fn is_withdrawn(&mut self, claim: &NamedNode) -> Result<bool> {
        if self.withdrawn.contains(claim) {
            return Ok(true);
        }
        for g in self.claim_graphs.clone() {
            let g = &g;
            if self.withdrawal(claim, g)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether `claim` authenticates as an `ev:Claim` in one of the configured proposal
    /// graphs under that graph's current protection.
    fn claim_authenticated(&mut self, claim: &NamedNode) -> Result<bool> {
        for g in self.claim_graphs.clone() {
            if self.typed(claim, &g, RecordKind::Claim)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the support `s` in `p.supports` is live. Requires the supported claim
    /// itself to authenticate in a configured proposal graph under its current protection
    /// so a proposal-graph detach and re-protection cannot invalidate an earlier
    /// withdrawal and revive a withdrawn claim during ordinary writes or reconciliation.
    fn live(&mut self, s: &NamedNode, p: &PolicyGraphs) -> Result<bool> {
        if !self.typed(s, &p.supports, RecordKind::AcceptanceSupport)? {
            return Ok(false);
        }
        let Some(d) = self.iri(s, &ev("decision"), &p.supports)? else {
            return Ok(false);
        };
        if !self.typed(&d, &p.decisions, RecordKind::ReviewDecision)?
            || self.is_superseded(&d, &p.decisions)?
        {
            return Ok(false);
        }
        let Some(claim) = self.iri(s, &ev("claim"), &p.supports)? else {
            return Ok(false);
        };
        if self.withdrawn.contains(&claim) {
            return Ok(false);
        }
        if !self.claim_authenticated(&claim)? {
            return Ok(false);
        }
        Ok(!self.is_withdrawn(&claim)?)
    }

    /// The supported triple of the support `s` when it projects into `p.output`.
    fn supported(&mut self, s: &NamedNode, p: &PolicyGraphs) -> Result<Option<Triple>> {
        if !self.reads.contains(
            s.as_str(),
            &ev("output"),
            &Term::NamedNode(p.output.0.clone()),
            p.supports.0.as_str(),
        )? {
            return Ok(None);
        }
        Ok(
            match self
                .reads
                .single(s.as_str(), &ev("supportedTriple"), p.supports.0.as_str())?
            {
                Some(Term::Triple(t)) => Some(*t),
                _ => None,
            },
        )
    }

    /// The authenticated supports in `p.supports` whose `predicate` is `object`.
    fn supports_by(
        &mut self,
        predicate: &str,
        object: &NamedNode,
        p: &PolicyGraphs,
    ) -> Result<Vec<NamedNode>> {
        let mut out = Vec::new();
        for s in self.subjects(
            predicate,
            &Term::NamedNode(object.clone()),
            &p.supports,
            "supports",
        )? {
            if self.typed(&s, &p.supports, RecordKind::AcceptanceSupport)? {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// Whether a live support in `p.supports` owns `triple` in `p.output`.
    fn owned(&mut self, triple: &Triple, p: &PolicyGraphs) -> Result<bool> {
        for s in self.subjects(
            &ev("supportedTriple"),
            &Term::Triple(Box::new(triple.clone())),
            &p.supports,
            "supports",
        )? {
            if self.supported(&s, p)?.as_ref() == Some(triple) && self.live(&s, p)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// All `ev:Projection` records of `p.policy_iri` in `p.supports`, and the subset
    /// superseded through `prov:wasRevisionOf` by a currently authenticated projection
    /// record of the same policy. Retained record IRIs are charged to `held`.
    fn policy_projections_held(
        &mut self,
        p: &PolicyGraphs,
        held: &mut crate::access::Reservation,
    ) -> Result<(Vec<NamedNode>, BTreeSet<NamedNode>)> {
        let policy = Term::NamedNode(p.policy_iri.clone());
        let class = Term::NamedNode(NamedNode::new_unchecked(ev("Projection")));
        let mut all_held = self.reads.preparation().reserve(0)?;
        let all =
            self.subjects_held(RDF_TYPE, &class, &p.supports, "projections", &mut all_held)?;
        let mut records = Vec::new();
        for r in all {
            if self.reads.contains(
                r.as_str(),
                &ev("reviewPolicy"),
                &policy,
                p.supports.0.as_str(),
            )? {
                held.grow_by((r.as_str().len() as u64).saturating_add(64))?;
                records.push(r);
            }
        }
        drop(all_held);
        let mut superseded = BTreeSet::new();
        for r in &records {
            if !self
                .reads
                .authenticated(r.as_str(), p.supports.0.as_str(), p.supports.1)?
            {
                continue;
            }
            let mut priors_held = self.reads.preparation().reserve(0)?;
            let (priors, more) = self.reads.objects_up_to_held(
                r.as_str(),
                PROV_WAS_REVISION_OF,
                p.supports.0.as_str(),
                MAX_EVIDENCE,
                &mut priors_held,
            )?;
            if more {
                return Err(incomplete("projections"));
            }
            for prior in priors {
                if let Term::NamedNode(n) = prior
                    && !superseded.contains(&n)
                {
                    held.grow_by((n.as_str().len() as u64).saturating_add(96))?;
                    superseded.insert(n);
                }
            }
        }
        Ok((records, superseded))
    }

    /// All `ev:Projection` records of `p.policy_iri` in `p.supports`, and the subset
    /// superseded through `prov:wasRevisionOf` by a currently authenticated projection
    /// record of the same policy.
    fn policy_projections(
        &mut self,
        p: &PolicyGraphs,
    ) -> Result<(Vec<NamedNode>, BTreeSet<NamedNode>)> {
        let mut held = self.reads.preparation().reserve(0)?;
        self.policy_projections_held(p, &mut held)
    }

    /// Whether projection record `r` authenticates under `p.supports`'s current
    /// protection and records the current protection nonces of `p`'s decisions,
    /// supports and output graphs.
    fn projection_matches_nonces(&mut self, r: &NamedNode, p: &PolicyGraphs) -> Result<bool> {
        let Some(now) = p.nonces().map(|n| [n.decisions, n.supports, n.output]) else {
            return Ok(false);
        };
        if !self
            .reads
            .authenticated(r.as_str(), p.supports.0.as_str(), p.supports.1)?
        {
            return Ok(false);
        }
        for (predicate, expected) in rv::NONCE_PREDICATES.iter().zip(now) {
            let recorded = self
                .text(r, predicate, &p.supports)?
                .and_then(|v| Uuid::parse_str(&v).ok());
            if recorded != Some(expected) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether projection record `r` matches `p`'s output graph and conflict mode.
    fn projection_matches_policy(&mut self, r: &NamedNode, p: &PolicyGraphs) -> Result<bool> {
        Ok(
            self.iri(r, &ev("output"), &p.supports)? == Some(p.output.0.clone())
                && self
                    .iri(r, &ev("conflictMode"), &p.supports)?
                    .is_some_and(|m| m.as_str() == conflict_mode_iri(p.mode)),
        )
    }

    /// Whether active projection record `r` authenticates under `p`'s current supports
    /// protection at `version` while recording a different output graph or conflict mode.
    /// That signals a same-version policy change (including swapping continuously
    /// protected outputs or `(decisions, output)` pairs between policies) rather than a
    /// detach and new protection.
    fn projection_same_version_drift(
        &mut self,
        r: &NamedNode,
        p: &PolicyGraphs,
        version: u32,
    ) -> Result<bool> {
        let Some(sup_nonce) = p.supports.1 else {
            return Ok(false);
        };
        if !self
            .reads
            .authenticated(r.as_str(), p.supports.0.as_str(), p.supports.1)?
        {
            return Ok(false);
        }
        let recorded_sup = self
            .text(r, rv::NONCE_PREDICATES[1], &p.supports)?
            .and_then(|v| Uuid::parse_str(&v).ok());
        if recorded_sup != Some(sup_nonce) {
            return Ok(false);
        }
        if self.text(r, &ev("policyVersion"), &p.supports)?.as_deref()
            != Some(version.to_string().as_str())
        {
            return Ok(false);
        }
        Ok(!self.projection_matches_policy(r, p)?)
    }

    /// Refuse when any continuously protected managed graph holds an active,
    /// authenticated `ev:Projection` record showing same-version drift for `p`
    /// (either `p` moved supports without raising its version, including across
    /// graph role changes that retain protection, or another currently configured
    /// review policy at its current version still owns `p.output` under unchanged
    /// protections).
    fn check_same_version_drift(
        &self,
        p: &PolicyGraphs,
        policy_version: u32,
        settings: &Settings,
        managed_graphs: &[(&str, Uuid)],
    ) -> Result<()> {
        let rival_owned: Vec<(NamedNode, u32)> = settings
            .policies
            .iter()
            .filter(|(name, _)| name.as_str() != p.name.as_str())
            .map(|(name, other)| (rv::policy_iri(name), other.version))
            .collect();
        let rivals: Vec<(&str, u32)> = rival_owned
            .iter()
            .map(|(iri, ver)| (iri.as_str(), *ver))
            .collect();
        if self.reads.has_same_version_projection_drift(
            p.policy_iri.as_str(),
            policy_version,
            p.supports.0.as_str(),
            p.output.0.as_str(),
            p.output.1,
            managed_graphs,
            &rivals,
        )? {
            return Err(same_version_policy_changed(&p.name));
        }
        Ok(())
    }

    /// Refuse when an active projection record of the policy was written under
    /// protections that are no longer the current ones, so that its supports no longer
    /// authenticate while its output still holds their triples, or when the output
    /// holds triples and no active projection record of the policy exists. Returns
    /// whether an active record of `p`'s version already exists with the content
    /// `planned` would give it.
    fn check_projection(
        &mut self,
        p: &PolicyGraphs,
        planned: Option<(u32, &sparkles_core::evidence::write::PlannedRecord)>,
    ) -> Result<bool> {
        let (records, superseded) = self.policy_projections(p)?;
        let active: Vec<NamedNode> = records
            .iter()
            .filter(|r| !superseded.contains(*r))
            .cloned()
            .collect();
        if !records.is_empty() && active.is_empty() {
            return Err(projection_unavailable(&p.name));
        }
        let mut existing = false;
        for r in &active {
            if let Some((version, _)) = planned
                && self.projection_same_version_drift(r, p, version)?
            {
                return Err(same_version_policy_changed(&p.name));
            }
            if !self.projection_matches_nonces(r, p)? {
                return Err(projection_unavailable(&p.name));
            }
            if let Some((version, planned_rec)) = planned {
                if *r == planned_rec.iri {
                    let digest = self
                        .text(r, &ev("recordDigest"), &p.supports)?
                        .and_then(|d| d.parse().ok());
                    if digest != Some(planned_rec.digest) {
                        return Err(same_version_policy_changed(&p.name));
                    }
                    existing = true;
                } else if self.text(r, &ev("policyVersion"), &p.supports)?.as_deref()
                    == Some(version.to_string().as_str())
                {
                    if !self.projection_matches_policy(r, p)? {
                        return Err(same_version_policy_changed(&p.name));
                    }
                    existing = true;
                }
            }
        }
        // An output that holds triples without a verifiable projection lost its
        // lineage, as when the supports graph was detached and cleared.
        if active.is_empty() && self.reads.graph_nonempty(p.output.0.as_str())? {
            return Err(projection_unavailable(&p.name));
        }
        Ok(existing)
    }
}

fn projection_unavailable(policy: &str) -> Error {
    managed(
        ManagedErrorKind::ProjectionUnavailable,
        format!(
            "policy {policy}: a graph of its projection was detached and protected again, so its supports no longer authenticate; the projection must be reconciled first"
        ),
    )
}

fn same_version_policy_changed(policy: &str) -> Error {
    managed(
        ManagedErrorKind::ProjectionUnavailable,
        format!(
            "policy {policy}: its supports, output or conflict mode changed without a new version; raise the policy's version"
        ),
    )
}

/// One policy's graphs and their current nonces.
struct PolicyGraphs {
    name: String,
    policy_iri: NamedNode,
    mode: ConflictMode,
    decisions: G,
    supports: G,
    output: G,
    boundary: Vec<G>,
}

impl PolicyGraphs {
    fn new(name: &str, policy: &Policy, cap: &ManagedWrite) -> Result<Self> {
        let g = |iri: &str| -> Result<G> {
            let n = NamedNode::new(iri).map_err(|e| Error::invalid(format!("graph: {e}")))?;
            let nonce = cap.protection(iri);
            Ok((n, nonce))
        };
        Ok(PolicyGraphs {
            name: name.to_string(),
            policy_iri: rv::policy_iri(name),
            mode: policy.conflicts,
            decisions: g(&policy.decisions)?,
            supports: g(&policy.supports)?,
            output: g(&policy.output)?,
            boundary: policy
                .boundary
                .iter()
                .map(|b| g(b))
                .collect::<Result<_>>()?,
        })
    }

    /// The graphs of `policy` with the nonces `nonce` gives, as an evidence view's
    /// configuration records them.
    fn with_nonces(
        name: &str,
        policy: &Policy,
        nonce: &dyn Fn(&str) -> Option<Uuid>,
    ) -> Result<Self> {
        let g = |iri: &str| -> Result<G> {
            let n = NamedNode::new(iri).map_err(|e| Error::invalid(format!("graph: {e}")))?;
            Ok((n, nonce(iri)))
        };
        Ok(PolicyGraphs {
            name: name.to_string(),
            policy_iri: rv::policy_iri(name),
            mode: policy.conflicts,
            decisions: g(&policy.decisions)?,
            supports: g(&policy.supports)?,
            output: g(&policy.output)?,
            boundary: policy
                .boundary
                .iter()
                .map(|b| g(b))
                .collect::<Result<_>>()?,
        })
    }

    fn nonces(&self) -> Option<ProjectionNonces> {
        Some(ProjectionNonces {
            decisions: self.decisions.1?,
            supports: self.supports.1?,
            output: self.output.1?,
        })
    }

    /// Refuse unless the decisions, supports and output graphs are protected.
    fn protected(&self) -> Result<ProjectionNonces> {
        for (g, n) in [&self.decisions, &self.supports, &self.output] {
            if n.is_none() {
                return Err(managed(
                    ManagedErrorKind::UnsupportedProfile,
                    format!(
                        "graph <{}> is not protected by the evidence profile; record the evidence settings again",
                        g.as_str()
                    ),
                ));
            }
        }
        Ok(self.nonces().expect("protected"))
    }
}

/// Refuse unless `access` lets the caller write `g`.
fn writable(access: Option<&Arc<GraphAccess>>, g: &NamedNode) -> Result<()> {
    let term = Term::NamedNode(g.clone());
    match access {
        Some(a) if !a.writable(Some(&term)) => Err(GraphAccess::refused(Some(&term))),
        _ => Ok(()),
    }
}

/// Refuse unless `access` lets the caller read `g`.
fn readable(access: Option<&Arc<GraphAccess>>, g: &NamedNode) -> Result<()> {
    let term = Term::NamedNode(g.clone());
    match access {
        Some(a) if a.readable(Some(&term)) => Ok(()),
        _ => Err(Error::NotPermitted(format!(
            "read access to graph {term} required"
        ))),
    }
}

/// The current name of `logical`'s branch.
fn branch_name(main: &Dataset, logical: &Dataset) -> Result<Option<String>> {
    if logical.store().is_branch() {
        main.store()
            .listed_branch_name(logical.store().branch_id(), &Default::default())
    } else {
        Ok(Some(crate::branch::MAIN.to_string()))
    }
}

/// The stored fact an evidence reference record `r` in `g` names, if it names one.
fn stored_fact_of(state: &mut State<'_>, r: &NamedNode, g: &G) -> Result<Option<StoredFactRef>> {
    let Some(dataset) = state.text(r, &ev("datasetId"), g)? else {
        return Ok(None);
    };
    let bad = || {
        Error::invalid(format!(
            "<{}> is not a well-formed stored-fact reference",
            r.as_str()
        ))
    };
    let uuid = |v: Option<String>| v.and_then(|v| Uuid::parse_str(&v).ok()).ok_or_else(bad);
    let dataset = uuid(Some(dataset))?;
    let branch = uuid(state.text(r, &ev("branchId"), g)?)?;
    let commit = state
        .text(r, &ev("commit"), g)?
        .and_then(|c| c.parse().ok())
        .ok_or_else(bad)?;
    let graph = match state.iri(r, &ev("graphKind"), g)? {
        Some(k) if k.as_str() == ev("DefaultGraph") => GraphIdentity::Default,
        Some(k) if k.as_str() == ev("NamedGraph") => {
            GraphIdentity::Named(state.iri(r, &ev("graph"), g)?.ok_or_else(bad)?)
        }
        _ => return Err(bad()),
    };
    let expected = match state
        .reads
        .single(r.as_str(), &ev("expectedTriple"), g.0.as_str())?
    {
        Some(Term::Triple(t)) => *t,
        _ => return Err(bad()),
    };
    let anchor = match (
        state.iri(r, &ev("reifier"), g)?,
        state.iri(r, &ev("skolemMap"), g)?,
    ) {
        (Some(x), None) => Some(BlankAnchor::Reifier(x)),
        (None, Some(x)) => Some(BlankAnchor::ImportMap(x)),
        _ => None,
    };
    Ok(Some(StoredFactRef {
        dataset,
        branch,
        commit,
        graph,
        expected,
        anchor,
    }))
}

/// The resolution of the records a decision depends on, within one evidence budget.
struct Evidence<'r, 'c> {
    resolution: &'r mut Resolution,
    catalog: Option<&'c Catalog>,
    main: &'c Dataset,
    logical: &'c Dataset,
    access: Option<Arc<GraphAccess>>,
    read: &'r mut dyn FnMut(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    count: usize,
}

impl Evidence<'_, '_> {
    fn charge(&mut self) -> Result<()> {
        self.count += 1;
        if self.count > MAX_EVIDENCE {
            return Err(incomplete("the evidence of the decisions"));
        }
        Ok(())
    }

    /// The record `iri` of one of `kinds`, readable by the caller.
    fn record(
        &mut self,
        key: &str,
        iri: &NamedNode,
        kinds: &[RecordKind],
    ) -> Result<super::resolve::VerifiedRecord> {
        self.charge()?;
        self.resolution.record(
            key,
            self.main,
            self.logical,
            self.access.clone(),
            &RecordRequest {
                iri: iri.clone(),
                kinds: kinds.to_vec(),
                content_digest: None,
            },
        )
    }

    fn fact(&mut self, key: &str, fact: &StoredFactRef) -> Result<()> {
        self.charge()?;
        let read = &mut *self.read;
        match self.catalog {
            Some(c) if fact.dataset != self.main.dataset_id() => {
                self.resolution.stored_fact(key, c, fact, read)?;
            }
            _ => {
                self.resolution.stored_fact_in(key, self.main, fact, read)?;
            }
        }
        Ok(())
    }

    /// Every authenticated evidence link of `target` in `boundary`, which the caller
    /// must be able to read with its reference and what that reference names. Returns
    /// the links whose relation is `ev:Contradicts`. A record the caller cannot read
    /// answers as `key`'s target being unavailable.
    fn links(
        &mut self,
        state: &mut State<'_>,
        key: &str,
        target: &NamedNode,
        boundary: &[G],
    ) -> Result<Vec<NamedNode>> {
        let mut contrary = Vec::new();
        for g in boundary {
            let (candidates, more) =
                state.candidates(&ev("target"), &Term::NamedNode(target.clone()), g)?;
            for link in candidates {
                if !state.typed(&link, g, RecordKind::EvidenceLink)? {
                    continue;
                }
                self.record(key, &link, &[RecordKind::EvidenceLink])
                    .map_err(|e| hidden(key, e))?;
                state.visible(key, &link, &g.0)?;
                if state
                    .iri(&link, &ev("relation"), g)?
                    .is_some_and(|r| r.as_str() == ev("Contradicts"))
                {
                    contrary.push(link.clone());
                }
                let Some(reference) = state.iri(&link, &ev("evidence"), g)? else {
                    return Err(unavailable(key));
                };
                let found = self
                    .record(key, &reference, &[RecordKind::EvidenceRef])
                    .map_err(|e| hidden(key, e))?;
                state.visible(key, &reference, &found.graph)?;
                let rg: G = (found.graph.clone(), None);
                if let Some(fact) = stored_fact_of(state, &reference, &rg)? {
                    self.fact(key, &fact).map_err(|e| hidden(key, e))?;
                }
                for (p, kind) in [
                    ("sourceRevision", RecordKind::SourceRevision),
                    ("rendition", RecordKind::Rendition),
                    ("passage", RecordKind::Passage),
                ] {
                    if let Some(r) = state.iri(&reference, &ev(p), &rg)? {
                        let named = self.record(key, &r, &[kind]).map_err(|e| hidden(key, e))?;
                        state.visible(key, &r, &named.graph)?;
                    }
                }
            }
            if more {
                return Err(incomplete("the evidence links of a target"));
            }
        }
        Ok(contrary)
    }
}

/// What the store records about a claim that the projection rules read.
struct ClaimFacts {
    proposition: Option<Triple>,
    kind: Option<ClaimKind>,
    positive: bool,
    negative: bool,
}

fn claim_facts(state: &mut State<'_>, claim: &NamedNode, g: &G) -> Result<ClaimFacts> {
    let proposition = match state
        .reads
        .single(claim.as_str(), RDF_REIFIES, g.0.as_str())?
    {
        Some(Term::Triple(t)) => Some(*t),
        _ => None,
    };
    let kind = state.iri(claim, &ev("claimKind"), g)?.and_then(|k| {
        [
            ("Reported", ClaimKind::Reported),
            ("Observation", ClaimKind::Observation),
            ("Hypothesis", ClaimKind::Hypothesis),
            ("Preference", ClaimKind::Preference),
        ]
        .into_iter()
        .find(|(n, _)| ev(n) == k.as_str())
        .map(|(_, k)| k)
    });
    let polarity = state.iri(claim, &ev("polarity"), g)?;
    Ok(ClaimFacts {
        proposition,
        kind,
        positive: polarity
            .as_ref()
            .is_some_and(|p| p.as_str() == ev("Positive")),
        negative: polarity.is_some_and(|p| p.as_str() == ev("Negative")),
    })
}

/// The output-graph quads of `triple`.
fn output_quad(triple: &Triple, output: &NamedNode) -> Quad {
    Quad::new(
        triple.subject.clone(),
        triple.predicate.clone(),
        triple.object.clone(),
        GraphName::NamedNode(output.clone()),
    )
}

// ------------------------------------------------------------------ commit -------

struct Commit {
    commit: u64,
    committed: bool,
    preview: Option<Box<crate::preview::Preview>>,
}

/// Publish `batch` and the output changes as one commit of the evidence owner, from
/// the head `expected` that the operation read. When `expected_generation` is given,
/// the base snapshot's generation UID must also match so physical IDs in `deletes`
/// remain valid across concurrent compaction.
#[allow(clippy::too_many_arguments)]
fn publish(
    main: &Dataset,
    logical: &Dataset,
    capability: ManagedWrite,
    expected: u64,
    expected_generation: Option<u64>,
    batch: &Batch,
    skip: &[bool],
    changes: &[(Quad, bool)],
    deletes: &[[sparkles_core::id::Id; 4]],
    options: &Options,
    control: &Control,
) -> Result<Commit> {
    debug_assert!(deletes.is_empty() || expected_generation.is_some());
    let store = logical.store();
    let (release, before_publish) = publish_hook(options.authority.clone(), || Ok(None));
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
    // The reads were made at `expected`; any later commit could change their answers.
    if t.base().commit != expected {
        return Err(managed(
            ManagedErrorKind::HeadChanged,
            format!(
                "the head of the branch moved from commit {expected} to {} while the operation read it; retry",
                t.base().commit
            ),
        ));
    }
    if let Some(expected_gen) = expected_generation
        && t.base().generation.uid != expected_gen
    {
        return Err(managed(
            ManagedErrorKind::HeadChanged,
            "the store compacted while the operation read it; retry",
        ));
    }
    let head = t.base().commit;
    let (seq, time_ms) = t.managed_stamp()?;
    let quads = {
        let capability = t.managed().expect("capability set");
        batch.quads(
            &Stamp {
                dataset: main.dataset_id(),
                branch: store.branch_id(),
                commit: seq,
                time_ms,
            },
            |g| capability.protection(g.as_str()),
            skip,
        )?
    };
    let mut labels = std::collections::HashMap::new();
    for (i, q) in quads.iter().enumerate() {
        control.cancel.check()?;
        if i & 0xff == 0 {
            control.check()?;
        }
        let ids = t.encode_quad(q, &mut labels)?;
        t.insert(ids)?;
    }
    for (i, (q, insert)) in changes.iter().enumerate() {
        control.cancel.check()?;
        if i & 0xff == 0 {
            control.check()?;
        }
        let ids = t.encode_quad(q, &mut labels)?;
        if *insert {
            t.insert(ids)?;
        } else {
            t.delete(ids)?;
        }
    }
    for (i, ids) in deletes.iter().enumerate() {
        control.cancel.check()?;
        if i & 0xff == 0 {
            control.check()?;
        }
        if !t.delete(*ids)? {
            return Err(managed(
                ManagedErrorKind::HeadChanged,
                "the output graph changed while the operation prepared deletions; retry",
            ));
        }
        store.failpoint("evidence-reconcile-after-delete");
    }
    let committed = t.commit();
    drop(release);
    match committed {
        Err(Error::DryRun(preview)) => Ok(Commit {
            commit: head,
            committed: false,
            preview: Some(preview),
        }),
        Err(e) => Err(e),
        Ok(receipt) => Ok(Commit {
            commit: receipt.commit.seq,
            committed: receipt.committed,
            preview: None,
        }),
    }
}

/// The operation identity of a request and whether a key names it.
fn operation(main: &Dataset, options: &Options) -> Result<Uuid> {
    match options.idempotency_key.as_deref() {
        Some(k) if k.is_empty() || k.len() > 256 => {
            Err(Error::invalid("idempotencyKey has 1 to 256 bytes"))
        }
        Some(k) => Ok(operation_id(main.dataset_id(), &options.principal, k)),
        None => Ok(Uuid::new_v4()),
    }
}

/// What a repeated key means: the first receipt, or a conflict.
fn replayed(
    found: Option<sparkles_core::evidence::write::RecordedOperation>,
    batch: &Batch,
) -> Result<Option<(u64, Option<Uuid>)>> {
    let Some(found) = found else {
        return Ok(None);
    };
    if !found.authenticated {
        return Err(managed(
            ManagedErrorKind::ImmutableRecordConflict,
            format!(
                "operation <{}> was recorded outside the evidence profile's protection",
                batch.operation_iri.as_str()
            ),
        ));
    }
    if found.payload_digest != Some(batch.payload_digest) {
        return Err(managed(
            ManagedErrorKind::IdempotencyConflict,
            "this idempotency key was used with a different payload",
        ));
    }
    Ok(Some((found.commit.unwrap_or_default(), found.branch)))
}

fn check_store(main: &Dataset, logical: &Dataset) -> Result<()> {
    if main.store().is_branch() || logical.store().owner_dataset_id() != main.dataset_id() {
        return Err(Error::invalid(
            "an operation names the dataset's main store and one of its branches",
        ));
    }
    Ok(())
}

/// The capability, checked against the settings the operation planned with.
fn capability(logical: &Dataset, operation: Uuid, settings_bytes: &[u8]) -> Result<ManagedWrite> {
    let capability = logical
        .store()
        .managed_writer(OwnerKind::Evidence, operation)?;
    if capability.settings().as_deref() != Some(settings_bytes) {
        return Err(Error::Conflict(
            "the evidence settings changed while the operation was prepared; retry".into(),
        ));
    }
    Ok(capability)
}

fn unique<'a>(what: &str, items: impl Iterator<Item = (&'a str, &'a NamedNode)>) -> Result<()> {
    let mut keys = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for (i, (key, target)) in items.enumerate() {
        if key.is_empty() || key.len() > 256 {
            return Err(Error::invalid(format!(
                "{what}[{i}].key has 1 to 256 bytes"
            )));
        }
        if !keys.insert(key) {
            return Err(Error::invalid(format!(
                "{what}[{i}].key: the key is used twice"
            )));
        }
        if !targets.insert(target.as_str()) {
            return Err(Error::invalid(format!(
                "{what}[{i}].target: the request names the target twice"
            )));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ review -------

/// The review state of one decision, between its checks and its records.
struct Pending {
    is_claim: bool,
    facts: Option<ClaimFacts>,
    not_projected: Option<&'static str>,
    /// Contrary links and the negative claims of the same proposition with their graphs.
    contrary_links: Vec<NamedNode>,
    negative_claims: Vec<NamedNode>,
}

/// Append `decisions` under the review policy `policy` of the evidence settings, as one
/// commit of the evidence owner on `logical`, which is `main` or one of its branches.
/// `read` is asked what the caller may read on a dataset and branch, by their current
/// names, as [`Resolution::all`] describes.
///
/// `options.if_head` is required, and every read is made at that head. The caller needs
/// update and read grants on the policy's decisions, supports and output graphs, read
/// access to each target and, for an acceptance, to every evidence link of the target
/// with its reference and what that names, and to every negative claim of the same
/// proposition in the policy's evidence boundary. Such a record it cannot read answers
/// as the target being unavailable.
///
/// Refusals, which write nothing: invalid input, `policy-changed` for another policy
/// version, `head-changed`, `evidence-unavailable` for a target that is unreadable,
/// missing or outside the policy's boundary, `target-changed` for another target digest,
/// `decision-superseded` when `supersedes` is not the target's current decision,
/// `target-withdrawn` for an acceptance of a withdrawn claim, `evidence-conflict` for an
/// acceptance a `refuse` policy meets with contrary evidence, `projection-unavailable`
/// when a protection of the policy's projection changed, `idempotency-conflict` and
/// `authority-changed`.
#[allow(clippy::too_many_arguments)]
pub fn review(
    catalog: Option<&Catalog>,
    main: &Dataset,
    logical: &Dataset,
    policy_name: &str,
    policy_version: u32,
    decisions: &[DecisionInput],
    options: &Options,
    read: &mut dyn FnMut(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<Reviewed> {
    control.check()?;
    check_store(main, logical)?;
    let Some(if_head) = options.if_head else {
        return Err(Error::invalid(
            "ifHead: a review names the branch head it was prepared at",
        ));
    };
    if decisions.is_empty() || decisions.len() > MAX_DECISIONS {
        return Err(Error::invalid(format!(
            "decisions: a review holds 1 to {MAX_DECISIONS} decisions"
        )));
    }
    unique(
        "decisions",
        decisions.iter().map(|d| (d.key.as_str(), &d.target)),
    )?;
    let (settings, settings_bytes) = settings_of(main)?;
    if !settings.enabled {
        return Err(managed(
            ManagedErrorKind::UnsupportedProfile,
            "the evidence profile is not enabled in this dataset",
        ));
    }
    if let Some(matching) = settings.matching_policies.get(policy_name) {
        return identity::review(
            catalog,
            main,
            logical,
            identity::Request {
                name: policy_name,
                policy: matching,
                settings: &settings,
                settings_bytes: &settings_bytes,
                version: policy_version,
                if_head,
                candidates: MAX_EVIDENCE,
            },
            decisions,
            options,
            read,
            control,
        );
    }
    let Some(policy) = settings.policies.get(policy_name) else {
        return Err(Error::invalid(format!(
            "policy: the evidence settings name no review policy {policy_name}"
        )));
    };
    if policy.version != policy_version {
        return Err(managed(
            ManagedErrorKind::PolicyChanged,
            format!(
                "policyVersion: policy {policy_name} is at version {}, not {policy_version}",
                policy.version
            ),
        ));
    }
    let operation = operation(main, options)?;
    let payload = rv::review_payload_digest(policy_name, policy_version, decisions, control)?;
    let access = match branch_name(main, logical)? {
        Some(b) => read(main, &b),
        None => None,
    };
    for g in [&policy.decisions, &policy.supports, &policy.output] {
        let g = NamedNode::new(g.as_str()).map_err(|e| Error::invalid(e.to_string()))?;
        writable(options.write.as_ref(), &g)?;
        readable(access.as_ref(), &g)?;
    }
    let capability = capability(logical, operation, &settings_bytes)?;
    let p = PolicyGraphs::new(policy_name, policy, &capability)?;
    let nonces = p.protected()?;
    let store = logical.store();
    let branch = store.branch_id();
    let head = store.snapshot();
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    // The caller's masked view is prepared only after a replay was ruled out, under
    // the resolver's allowance.
    let mask_preparation =
        crate::access::Preparation::new(control.clone(), super::resolve::MEMORY_BYTES)?;
    // The preconditions read the stored decisions; this operation's supersessions
    // count once they have all been checked.
    let mut state = State {
        reads: Reads::new(&preparation, &head),
        seen: None,
        claim_graphs: claim_graphs(&settings, &capability),
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    let records = record_graph_nonces(&settings, &capability);
    let decided_iri = |d: &DecisionInput| -> Result<NamedNode> {
        Ok(rv::decision_record(
            main.dataset_id(),
            &options.principal,
            operation,
            policy_name,
            policy_version,
            d,
            None,
            &p.decisions.0,
            control,
        )?
        .iri)
    };
    // A repeated key answers the first receipt before anything else is read.
    if options.idempotency_key.is_some() {
        let probe = Batch {
            operation,
            operation_iri: operation_iri(operation),
            payload_digest: payload,
            principal: options.principal.clone(),
            idempotency_key: options.idempotency_key.clone(),
            used: None,
            graphs: Vec::new(),
            records: Vec::new(),
        };
        let found = state.reads.operation(&probe.operation_iri, &records)?;
        if let Some((commit, recorded_branch)) = replayed(found, &probe)? {
            let mut out = Vec::new();
            for d in decisions {
                let iri = decided_iri(d)?;
                let support =
                    rv::support_iri(main.dataset_id(), &options.principal, &iri, control)?;
                let present = matches!(
                    state
                        .reads
                        .record(&support, std::slice::from_ref(&p.supports))?,
                    sparkles_core::evidence::write::ExistingRecord::Present { .. }
                );
                // the first answer's metadata, from the records it wrote
                let not_projected = state
                    .text(&iri, &ev("projectionNote"), &p.decisions)?
                    .and_then(|n| NOTES.iter().copied().find(|k| *k == n));
                let conflicts = if present {
                    let (v, _) = state.reads.objects_up_to(
                        support.as_str(),
                        &ev("conflictsWith"),
                        p.supports.0.as_str(),
                        MAX_EVIDENCE,
                    )?;
                    canonical(
                        v.into_iter()
                            .filter_map(|t| match t {
                                Term::NamedNode(n) => Some(n),
                                _ => None,
                            })
                            .collect(),
                    )
                } else {
                    Vec::new()
                };
                out.push(Decided {
                    key: d.key.clone(),
                    iri,
                    target: d.target.clone(),
                    disposition: d.disposition,
                    supersedes: d.supersedes.clone(),
                    support: present.then_some(support),
                    not_projected,
                    conflicts,
                });
            }
            return Ok(Reviewed {
                operation: probe.operation_iri,
                committed: false,
                replayed: true,
                commit,
                branch: recorded_branch.unwrap_or(branch),
                decisions: out,
                changed: Changed::default(),
                map_revision: None,
                preview: None,
            });
        }
    }
    if head.commit != if_head {
        return Err(managed(
            ManagedErrorKind::HeadChanged,
            format!(
                "ifHead: the head of the branch is commit {}, not {if_head}",
                head.commit
            ),
        ));
    }
    let masked = caller_view(access.as_ref(), &head, &mask_preparation)?;
    state.seen = masked
        .as_ref()
        .map(|v| Reads::from_prepared(&mask_preparation, v));
    state.check_projection(&p, None)?;
    let mut resolution = Resolution::new(control.clone())?;
    let mut evidence = Evidence {
        resolution: &mut resolution,
        catalog,
        main,
        logical,
        access: access.clone(),
        read,
        count: 0,
    };
    let retained = crate::access::Preparation::new(control.clone(), super::resolve::MEMORY_BYTES)?;
    let mut held = Vec::new();
    let mut pending = Vec::with_capacity(decisions.len());
    for d in decisions {
        control.check()?;
        let target = evidence.record(
            &d.key,
            &d.target,
            &[RecordKind::Claim, RecordKind::Assessment],
        )?;
        let Some(tg) = p.boundary.iter().find(|(g, _)| *g == target.graph).cloned() else {
            return Err(unavailable(&d.key));
        };
        if target.record_digest != Some(d.target_digest) {
            return Err(managed(
                ManagedErrorKind::TargetChanged,
                format!(
                    "{}: the target's record digest is not the one the review names",
                    d.key
                ),
            ));
        }
        let current = state.current(&d.target, &p.policy_iri, &p.decisions)?;
        if current.len() > 1 {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!("{}: the target has more than one current decision", d.key),
            ));
        }
        if current.first() != d.supersedes.as_ref() {
            return Err(managed(
                ManagedErrorKind::DecisionSuperseded,
                format!(
                    "{}: supersedes does not name the target's current decision under policy {policy_name}",
                    d.key
                ),
            ));
        }
        let is_claim = target.kind == RecordKind::Claim;
        let accepted = d.disposition == Disposition::Accepted;
        if accepted {
            state.visible(&d.key, &d.target, &tg.0)?;
        }
        if accepted && is_claim && state.withdrawal(&d.target, &tg)?.is_some() {
            return Err(managed(
                ManagedErrorKind::TargetWithdrawn,
                format!("{}: the claim occurrence was withdrawn", d.key),
            ));
        }
        let mut item = Pending {
            is_claim,
            facts: None,
            not_projected: None,
            contrary_links: Vec::new(),
            negative_claims: Vec::new(),
        };
        if accepted {
            item.contrary_links = evidence.links(&mut state, &d.key, &d.target, &p.boundary)?;
        }
        if accepted && is_claim {
            let facts = claim_facts(&mut state, &d.target, &tg)?;
            item.not_projected = if !facts.positive {
                Some(NOTES[0])
            } else if !facts
                .kind
                .is_some_and(|k| policy.admitted_kinds.contains(&k))
            {
                Some(NOTES[1])
            } else if facts.proposition.is_none() {
                Some(NOTES[2])
            } else {
                let mut missing = None;
                for predicate in &policy.required_scope {
                    let (v, _) = state.reads.objects_up_to(
                        d.target.as_str(),
                        predicate,
                        tg.0.as_str(),
                        1,
                    )?;
                    if v.is_empty() {
                        missing = Some(NOTES[3]);
                        break;
                    }
                }
                missing
            };
            if item.not_projected.is_none() {
                // every negative occurrence of the same proposition in the boundary
                // can change the answer, so the reviewer must be able to read it
                let tt = Term::Triple(Box::new(facts.proposition.clone().expect("checked")));
                for g in &p.boundary {
                    let (candidates, more) = state.candidates(RDF_REIFIES, &tt, g)?;
                    for other in candidates {
                        if other == d.target || !state.typed(&other, g, RecordKind::Claim)? {
                            continue;
                        }
                        if !claim_facts(&mut state, &other, g)?.negative {
                            continue;
                        }
                        evidence
                            .record(&d.key, &other, &[RecordKind::Claim])
                            .map_err(|e| hidden(&d.key, e))?;
                        state.visible(&d.key, &other, &g.0)?;
                        if state.withdrawal(&other, g)?.is_none() {
                            item.negative_claims.push(other);
                        }
                    }
                    if more {
                        return Err(incomplete("the claims of a proposition"));
                    }
                }
            }
            if let Some(t) = &facts.proposition {
                // the proposition is kept until the commit, and copied for its support
                // and output changes
                let bytes = (t.to_string().len() as u64).saturating_mul(4);
                held.push(
                    retained
                        .reserve(bytes)
                        .map_err(|_| incomplete("the propositions of the acceptances"))?,
                );
            }
            item.facts = Some(facts);
        } else if accepted {
            item.not_projected = Some(NOTES[4]);
        }
        pending.push(item);
    }
    drop(evidence);
    state.superseded = decisions
        .iter()
        .filter_map(|d| d.supersedes.clone())
        .collect();
    // Conflicts are decided against the batch's final decisions, so the order of the
    // decisions in the request cannot change the answer.
    let in_batch: BTreeMap<&NamedNode, Disposition> = decisions
        .iter()
        .map(|d| (&d.target, d.disposition))
        .collect();
    let mut conflicts: Vec<Vec<NamedNode>> = Vec::with_capacity(decisions.len());
    for (d, item) in decisions.iter().zip(&pending) {
        let mut found = item.contrary_links.clone();
        if d.disposition == Disposition::Accepted && item.is_claim && item.not_projected.is_none() {
            for other in &item.negative_claims {
                let rejected = match in_batch.get(other) {
                    Some(disposition) => *disposition == Disposition::Rejected,
                    None => {
                        let current = state.current(other, &p.policy_iri, &p.decisions)?;
                        match current.first() {
                            Some(c) => {
                                state.disposition(c, &p.decisions)? == Some(Disposition::Rejected)
                            }
                            None => false,
                        }
                    }
                };
                if !rejected {
                    found.push(other.clone());
                }
            }
        } else {
            found.clear();
        }
        if !found.is_empty() && p.mode == ConflictMode::Refuse {
            return Err(managed(
                ManagedErrorKind::EvidenceConflict,
                format!(
                    "{}: contrary evidence meets the acceptance, and policy {policy_name} refuses conflicts",
                    d.key
                ),
            ));
        }
        conflicts.push(canonical(found));
    }
    // The records.
    let mut batch = Batch {
        operation,
        operation_iri: operation_iri(operation),
        payload_digest: payload,
        principal: options.principal.clone(),
        idempotency_key: options.idempotency_key.clone(),
        used: None,
        graphs: vec![p.decisions.0.clone()],
        records: Vec::new(),
    };
    let mut decided = Vec::with_capacity(decisions.len());
    let mut changed = Changed::default();
    let mut touched: Vec<Triple> = Vec::new();
    let mut owned_now: HashSet<Triple> = HashSet::new();
    for ((d, item), found) in decisions.iter().zip(&pending).zip(conflicts) {
        let note = item
            .not_projected
            .filter(|_| d.disposition == Disposition::Accepted);
        let record = rv::decision_record(
            main.dataset_id(),
            &options.principal,
            operation,
            policy_name,
            policy_version,
            d,
            note,
            &p.decisions.0,
            control,
        )?;
        let iri = record.iri.clone();
        batch.records.push(record);
        // the supports of the superseded decision end with it
        if let Some(prior) = &d.supersedes {
            for s in state.supports_by(&ev("decision"), prior, &p)? {
                let Some(t) = state.supported(&s, &p)? else {
                    continue;
                };
                // live apart from this operation's supersessions
                let mut superseded = std::mem::take(&mut state.superseded);
                let before = state.live(&s, &p);
                std::mem::swap(&mut state.superseded, &mut superseded);
                if before? {
                    changed.supports_ended.push(s);
                    touched.push(t);
                }
            }
        }
        let mut support = None;
        let projected =
            d.disposition == Disposition::Accepted && item.is_claim && item.not_projected.is_none();
        if projected {
            let triple = item
                .facts
                .as_ref()
                .and_then(|f| f.proposition.clone())
                .expect("checked");
            let s = rv::support_record(
                main.dataset_id(),
                &options.principal,
                &format!("{}.support", d.key),
                &iri,
                &d.target,
                policy_name,
                &p.output.0,
                &triple,
                &found,
                &p.supports.0,
                control,
            )?;
            support = Some(s.iri.clone());
            changed.supports_added.push(s.iri.clone());
            batch.records.push(s);
            owned_now.insert(triple.clone());
            touched.push(triple);
        }
        decided.push(Decided {
            key: d.key.clone(),
            iri,
            target: d.target.clone(),
            disposition: d.disposition,
            supersedes: d.supersedes.clone(),
            support,
            not_projected: if d.disposition == Disposition::Accepted {
                item.not_projected
            } else {
                None
            },
            conflicts: if projected { found } else { Vec::new() },
        });
    }
    let mut skip = vec![false; batch.records.len()];
    if !changed.supports_added.is_empty() {
        let record = rv::projection_record(
            main.dataset_id(),
            policy_name,
            policy_version,
            &p.output.0,
            p.mode,
            nonces,
            &p.supports.0,
            control,
        )?;
        let protected_graphs = protected_managed_graphs(&settings, &capability);
        let protected_refs: Vec<(&str, Uuid)> = protected_graphs
            .iter()
            .map(|(g, n)| (g.as_str(), *n))
            .collect();
        state.check_same_version_drift(&p, policy_version, &settings, &protected_refs)?;
        let existing = state.check_projection(&p, Some((policy_version, &record)))?;
        batch.records.push(record);
        skip.push(existing);
        batch.graphs.push(p.supports.0.clone());
        batch.graphs.sort();
        batch.graphs.dedup();
    }
    let changes = output_changes(&mut state, &p, touched, &owned_now, &mut changed)?;
    check_identities(&mut state, &batch, &records, &skip)?;
    drop(state);
    let committed = publish(
        main,
        logical,
        capability,
        if_head,
        None,
        &batch,
        &skip,
        &changes,
        &[],
        options,
        control,
    )?;
    drop(held);
    Ok(Reviewed {
        operation: batch.operation_iri.clone(),
        committed: committed.committed,
        replayed: false,
        commit: committed.commit,
        branch,
        decisions: decided,
        changed,
        map_revision: None,
        preview: committed.preview,
    })
}

/// `conflicts` sorted by IRI without repeats, the order a recorded support's
/// `ev:conflictsWith` is answered in.
fn canonical(mut conflicts: Vec<NamedNode>) -> Vec<NamedNode> {
    conflicts.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    conflicts.dedup();
    conflicts
}

/// The reasons an acceptance does not project its claim, as `ev:projectionNote` records
/// them.
const NOTES: [&str; 5] = [
    "the claim is not positive",
    "the policy does not admit the claim's kind",
    "the claim reifies no ground triple",
    "the claim lacks a scope the policy requires",
    "only claims are projected",
];

/// The caller's controlled view of `head` when its access hides triples, `None` when
/// the unmasked head shows exactly what it may read in a readable graph.
fn caller_view(
    access: Option<&Arc<GraphAccess>>,
    head: &Arc<crate::store::Snapshot>,
    preparation: &crate::access::Preparation,
) -> Result<Option<crate::access::PreparedView>> {
    match access.filter(|a| a.hides_triples()) {
        None => Ok(None),
        Some(a) => a
            .masked_controlled(head, preparation)
            .map(Some)
            .map_err(|e| match e {
                Error::Unsupported(m) => managed(ManagedErrorKind::IncompleteEvidence, m),
                e => e,
            }),
    }
}

/// The caller's controlled view over an evidence view when its access hides triples.
fn caller_view_evidence(
    access: Option<&Arc<GraphAccess>>,
    view: &sparkles_core::store::BoundEvidenceView,
    preparation: &crate::access::Preparation,
) -> Result<Option<crate::access::PreparedView>> {
    match access.filter(|a| a.hides_triples()) {
        None => Ok(None),
        Some(a) => view
            .prepare_view(a.clone(), preparation)
            .map(Some)
            .map_err(|e| match e {
                Error::Unsupported(m) => managed(ManagedErrorKind::IncompleteEvidence, m),
                e => e,
            }),
    }
}

/// The proposal graphs of `settings`, where claims and their withdrawals are.
fn claim_graphs(settings: &Settings, capability: &ManagedWrite) -> Vec<G> {
    settings
        .proposals
        .iter()
        .filter_map(|g| NamedNode::new(g.as_str()).ok())
        .map(|g| {
            let n = capability.protection(g.as_str());
            (g, n)
        })
        .collect()
}

fn protected_managed_graphs(settings: &Settings, capability: &ManagedWrite) -> Vec<(String, Uuid)> {
    settings
        .managed_graphs()
        .into_iter()
        .filter_map(|g| capability.protection(&g).map(|n| (g, n)))
        .collect()
}

/// Refuse when a record identity the operation mints already has content in a record
/// graph, unless `skip` leaves it out as an equal authenticated record, so that content
/// planted under a predictable identity is never authenticated by a later write.
fn check_identities(
    state: &mut State<'_>,
    batch: &Batch,
    records: &[G],
    skip: &[bool],
) -> Result<()> {
    use sparkles_core::evidence::write::ExistingRecord;
    for (i, r) in batch.records.iter().enumerate() {
        if skip.get(i).copied().unwrap_or(false) {
            continue;
        }
        if !matches!(state.reads.record(&r.iri, records)?, ExistingRecord::Absent) {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "{}: <{}> is already recorded with other content",
                    r.key,
                    r.iri.as_str()
                ),
            ));
        }
    }
    Ok(())
}

/// The insertions and deletions in `p.output` that leave exactly the triples a live
/// support owns among `touched`, given that this operation's new supports own
/// `owned_now`.
fn output_changes(
    state: &mut State<'_>,
    p: &PolicyGraphs,
    mut touched: Vec<Triple>,
    owned_now: &HashSet<Triple>,
    changed: &mut Changed,
) -> Result<Vec<(Quad, bool)>> {
    // a stable order, so that a preview and the changes list are deterministic
    touched.sort_by_cached_key(|t| t.to_string());
    touched.dedup();
    let mut out = Vec::new();
    for t in touched {
        let owned = owned_now.contains(&t) || state.owned(&t, p)?;
        let NamedOrBlankNode::NamedNode(s) = &t.subject else {
            continue;
        };
        let present = state.reads.contains(
            s.as_str(),
            t.predicate.as_str(),
            &t.object,
            p.output.0.as_str(),
        )?;
        let change = ProjectionChange {
            output: p.output.0.clone(),
            triple: t.clone(),
        };
        if owned && !present {
            out.push((output_quad(&t, &p.output.0), true));
            changed.inserted.push(change);
        } else if !owned && present {
            out.push((output_quad(&t, &p.output.0), false));
            changed.removed.push(change);
        }
    }
    Ok(out)
}

/// The record-role graphs of `settings` with their nonces under `capability`.
fn record_graph_nonces(settings: &Settings, capability: &ManagedWrite) -> Vec<G> {
    super::propose::record_graphs(settings)
        .into_iter()
        .map(|g| {
            let n = capability.protection(g.as_str());
            (g, n)
        })
        .collect()
}

// ---------------------------------------------------------------- withdraw -------

/// Withdraw the claim occurrences `withdrawals` as one commit of the evidence owner on
/// `logical`. Each withdrawal is a record in the claim's own graph with
/// `prov:invalidatedAtTime`, the commit's time, and makes no contrary claim. The
/// supports of the claims stop being live, and each projected triple left without a
/// live support leaves its output graph. A claim that is already withdrawn is reported
/// as existing and not withdrawn again.
///
/// The caller needs update grants on each claim's graph, read access to each claim, and
/// read and update grants on the output graph and read grants on the decisions and
/// supports graphs of every review policy, since a support admitted under an earlier
/// policy version ends with its claim whatever the boundary is now.
/// `options.if_head` is optional, and the head the reads were made at must still be the
/// head when the commit is made, or the answer is `head-changed`.
///
/// Refusals, which write nothing: invalid input, `head-changed`,
/// `evidence-unavailable`, `target-changed`, `projection-unavailable`,
/// `idempotency-conflict` and `authority-changed`.
pub fn withdraw(
    main: &Dataset,
    logical: &Dataset,
    withdrawals: &[WithdrawalInput],
    options: &Options,
    read: &mut dyn FnMut(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<Withdrawn> {
    control.check()?;
    check_store(main, logical)?;
    if withdrawals.is_empty() || withdrawals.len() > MAX_DECISIONS {
        return Err(Error::invalid(format!(
            "withdrawals: a request holds 1 to {MAX_DECISIONS} withdrawals"
        )));
    }
    unique(
        "withdrawals",
        withdrawals.iter().map(|w| (w.key.as_str(), &w.target)),
    )?;
    let (settings, settings_bytes) = settings_of(main)?;
    if !settings.enabled {
        return Err(managed(
            ManagedErrorKind::UnsupportedProfile,
            "the evidence profile is not enabled in this dataset",
        ));
    }
    let operation = operation(main, options)?;
    let payload = rv::withdrawal_payload_digest(withdrawals, control)?;
    let access = match branch_name(main, logical)? {
        Some(b) => read(main, &b),
        None => None,
    };
    let capability = capability(logical, operation, &settings_bytes)?;
    let store = logical.store();
    let branch = store.branch_id();
    let head = store.snapshot();
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    let mut state = State {
        reads: Reads::new(&preparation, &head),
        seen: None,
        claim_graphs: claim_graphs(&settings, &capability),
        superseded: BTreeSet::new(),
        withdrawn: withdrawals.iter().map(|w| w.target.clone()).collect(),
    };
    let records = record_graph_nonces(&settings, &capability);
    let mut resolution = Resolution::new(control.clone())?;
    // Each claim, readable by the caller and of the digest the request names.
    let mut graphs: BTreeMap<String, NamedNode> = BTreeMap::new();
    let mut targets = Vec::with_capacity(withdrawals.len());
    for w in withdrawals {
        control.check()?;
        let found = resolution.record(
            &w.key,
            main,
            logical,
            access.clone(),
            &RecordRequest {
                iri: w.target.clone(),
                kinds: vec![RecordKind::Claim],
                content_digest: None,
            },
        )?;
        if found.record_digest != Some(w.target_digest) {
            return Err(managed(
                ManagedErrorKind::TargetChanged,
                format!(
                    "{}: the claim's record digest is not the one the withdrawal names",
                    w.key
                ),
            ));
        }
        writable(options.write.as_ref(), &found.graph)?;
        graphs.insert(found.graph.as_str().to_string(), found.graph.clone());
        targets.push(found.graph);
    }
    // Every policy, decided statically: a support admitted under an earlier version
    // whose boundary no longer holds the claim's graph still ends with the claim.
    let mut policies = Vec::new();
    for (name, policy) in &settings.policies {
        let p = PolicyGraphs::new(name, policy, &capability)?;
        writable(options.write.as_ref(), &p.output.0)?;
        for g in [&p.decisions.0, &p.supports.0, &p.output.0] {
            readable(access.as_ref(), g)?;
        }
        policies.push(p);
    }
    let mut batch = Batch {
        operation,
        operation_iri: operation_iri(operation),
        payload_digest: payload,
        principal: options.principal.clone(),
        idempotency_key: options.idempotency_key.clone(),
        used: None,
        graphs: graphs.values().cloned().collect(),
        records: Vec::new(),
    };
    if options.idempotency_key.is_some() {
        let found = state.reads.operation(&batch.operation_iri, &records)?;
        if let Some((commit, recorded_branch)) = replayed(found, &batch)? {
            let mut out = Vec::new();
            for (w, g) in withdrawals.iter().zip(&targets) {
                let r = rv::withdrawal_record(
                    main.dataset_id(),
                    &options.principal,
                    operation,
                    w,
                    g,
                    control,
                )?;
                let ours = matches!(
                    state
                        .reads
                        .record(&r.iri, &[(g.clone(), capability.protection(g.as_str()))])?,
                    sparkles_core::evidence::write::ExistingRecord::Present { .. }
                );
                out.push(WithdrawnClaim {
                    key: w.key.clone(),
                    iri: ours.then_some(r.iri),
                    target: w.target.clone(),
                    existing: !ours,
                });
            }
            return Ok(Withdrawn {
                operation: batch.operation_iri,
                committed: false,
                replayed: true,
                commit,
                branch: recorded_branch.unwrap_or(branch),
                withdrawals: out,
                changed: Changed::default(),
                preview: None,
            });
        }
    }
    if let Some(expected) = options.if_head
        && head.commit != expected
    {
        return Err(managed(
            ManagedErrorKind::HeadChanged,
            format!(
                "ifHead: the head of the branch is commit {}, not {expected}",
                head.commit
            ),
        ));
    }
    for p in &policies {
        p.protected()?;
        state.check_projection(p, None)?;
    }
    for g in &batch.graphs {
        if capability.protection(g.as_str()).is_none() {
            return Err(managed(
                ManagedErrorKind::UnsupportedProfile,
                format!(
                    "graph <{}> is not protected by the evidence profile; record the evidence settings again",
                    g.as_str()
                ),
            ));
        }
    }
    let mut out = Vec::with_capacity(withdrawals.len());
    for (w, g) in withdrawals.iter().zip(&targets) {
        let protected: G = (g.clone(), capability.protection(g.as_str()));
        if state.withdrawal(&w.target, &protected)?.is_some() {
            out.push(WithdrawnClaim {
                key: w.key.clone(),
                iri: None,
                target: w.target.clone(),
                existing: true,
            });
            continue;
        }
        let r = rv::withdrawal_record(
            main.dataset_id(),
            &options.principal,
            operation,
            w,
            g,
            control,
        )?;
        out.push(WithdrawnClaim {
            key: w.key.clone(),
            iri: Some(r.iri.clone()),
            target: w.target.clone(),
            existing: false,
        });
        batch.records.push(r);
    }
    // The supports that were live before, which the withdrawals end.
    let mut changed = Changed::default();
    let mut changes = Vec::new();
    for p in &policies {
        let mut touched = Vec::new();
        for w in out.iter().filter(|w| !w.existing) {
            for s in state.supports_by(&ev("claim"), &w.target, p)? {
                let Some(t) = state.supported(&s, p)? else {
                    continue;
                };
                // live apart from this operation's withdrawals
                let before = {
                    let mut withdrawn = std::mem::take(&mut state.withdrawn);
                    let live = state.live(&s, p);
                    std::mem::swap(&mut state.withdrawn, &mut withdrawn);
                    live?
                };
                if before {
                    changed.supports_ended.push(s);
                    touched.push(t);
                }
            }
        }
        changes.extend(output_changes(
            &mut state,
            p,
            touched,
            &HashSet::new(),
            &mut changed,
        )?);
    }
    let skip = vec![false; batch.records.len()];
    check_identities(&mut state, &batch, &records, &skip)?;
    drop(state);
    let committed = publish(
        main,
        logical,
        capability,
        head.commit,
        None,
        &batch,
        &skip,
        &changes,
        &[],
        options,
        control,
    )?;
    Ok(Withdrawn {
        operation: batch.operation_iri.clone(),
        committed: committed.committed,
        replayed: false,
        commit: committed.commit,
        branch,
        withdrawals: out,
        changed,
        preview: committed.preview,
    })
}

// ------------------------------------------------------ reconcile projection -----

fn readable_optional(access: Option<&Arc<GraphAccess>>, g: &NamedNode) -> Result<()> {
    let term = Term::NamedNode(g.clone());
    match access {
        Some(a) if !a.readable(Some(&term)) => Err(Error::NotPermitted(format!(
            "read access to graph {term} required"
        ))),
        _ => Ok(()),
    }
}

fn budget_to_incomplete(e: Error) -> Error {
    match e {
        Error::BudgetExceeded(b) if b.kind != crate::error::BudgetKind::ServerMemory => managed(
            ManagedErrorKind::IncompleteEvidence,
            "the projection's records exceed the memory allowance, so nothing was written",
        ),
        other => other,
    }
}

fn cmp_subject(a: &NamedOrBlankNode, b: &NamedOrBlankNode) -> std::cmp::Ordering {
    match (a, b) {
        (NamedOrBlankNode::NamedNode(a), NamedOrBlankNode::NamedNode(b)) => {
            a.as_str().cmp(b.as_str())
        }
        (NamedOrBlankNode::NamedNode(_), NamedOrBlankNode::BlankNode(_)) => {
            std::cmp::Ordering::Less
        }
        (NamedOrBlankNode::BlankNode(_), NamedOrBlankNode::NamedNode(_)) => {
            std::cmp::Ordering::Greater
        }
        (NamedOrBlankNode::BlankNode(a), NamedOrBlankNode::BlankNode(b)) => {
            a.as_str().cmp(b.as_str())
        }
    }
}

fn cmp_literal(a: &oxrdf::Literal, b: &oxrdf::Literal) -> std::cmp::Ordering {
    fn dir_tag(l: &oxrdf::Literal) -> u8 {
        match l.direction() {
            None => 0,
            Some(oxrdf::BaseDirection::Ltr) => 1,
            Some(oxrdf::BaseDirection::Rtl) => 2,
        }
    }
    a.value()
        .cmp(b.value())
        .then_with(|| a.language().cmp(&b.language()))
        .then_with(|| dir_tag(a).cmp(&dir_tag(b)))
        .then_with(|| a.datatype().as_str().cmp(b.datatype().as_str()))
}

fn cmp_term(a: &Term, b: &Term) -> std::cmp::Ordering {
    fn tag(t: &Term) -> u8 {
        match t {
            Term::Literal(_) => 0,
            Term::NamedNode(_) => 1,
            Term::Triple(_) => 2,
            Term::BlankNode(_) => 3,
        }
    }
    tag(a).cmp(&tag(b)).then_with(|| match (a, b) {
        (Term::Literal(a), Term::Literal(b)) => cmp_literal(a, b),
        (Term::NamedNode(a), Term::NamedNode(b)) => a.as_str().cmp(b.as_str()),
        (Term::Triple(a), Term::Triple(b)) => cmp_triple(a, b),
        (Term::BlankNode(a), Term::BlankNode(b)) => a.as_str().cmp(b.as_str()),
        _ => std::cmp::Ordering::Equal,
    })
}

fn cmp_triple(a: &Triple, b: &Triple) -> std::cmp::Ordering {
    cmp_subject(&a.subject, &b.subject)
        .then_with(|| a.predicate.as_str().cmp(b.predicate.as_str()))
        .then_with(|| cmp_term(&a.object, &b.object))
}

/// Reconcile a review policy's accepted projection after a detach and new protection of
/// its decisions, supports or output graph, as one commit of the evidence owner on
/// `logical` (`main` or one of its branches).
///
/// Rebuilds the policy's output graph to hold exactly the triples owned by live
/// authenticated acceptance supports of the policy whose claims authenticate in a
/// configured proposal graph and have no authenticated withdrawal, or empties it when
/// none vouch. Records the current protection nonces in a fresh `ev:Projection` record
/// in the policy's supports graph that supersedes any prior unsuperseded projection
/// records of the policy, or reuses the active projection record (linking the operation
/// through `prov:used` for keyed replays) when the protections and output already match.
///
/// `policy_version` is optional; when supplied it must equal the policy's current
/// version or the operation refuses with `policy-changed`. `options.if_head` is
/// optional, and the head read must still be the writer's base when committing. The
/// caller needs update grants on the policy's supports and output graphs and unmasked
/// read grants on the policy's decisions, supports and output graphs and on every
/// configured proposal graph.
///
/// Refusals, which write nothing: invalid input (including a matching policy), missing
/// or masked grants, `unsupported-profile` when the profile is disabled or one of the
/// policy's graphs is still detached, `policy-changed`, `head-changed`,
/// `idempotency-conflict`, `immutable-record-conflict`, `incomplete-evidence`,
/// `projection-unavailable` when the policy's output or conflict mode changed without
/// raising its version under unchanged protections, and `authority-changed`.
pub fn reconcile_projection(
    main: &Dataset,
    logical: &Dataset,
    policy_name: &str,
    policy_version: Option<u32>,
    options: &Options,
    access: Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<ReconciledProjection> {
    reconcile_projection_with_budget(
        main,
        logical,
        policy_name,
        policy_version,
        options,
        access,
        control,
        super::resolve::MEMORY_BYTES,
    )
}

#[allow(clippy::too_many_arguments)]
fn reconcile_projection_with_budget(
    main: &Dataset,
    logical: &Dataset,
    policy_name: &str,
    policy_version: Option<u32>,
    options: &Options,
    access: Option<Arc<GraphAccess>>,
    control: &Control,
    memory_bytes: u64,
) -> Result<ReconciledProjection> {
    control.check()?;
    let preparation = crate::access::Preparation::new(control.clone(), memory_bytes)
        .map_err(budget_to_incomplete)?;
    let (reconciled, _response_held) = reconcile_projection_prepared(
        main,
        logical,
        policy_name,
        policy_version,
        options,
        access,
        &preparation,
        control,
    )?;
    Ok(reconciled)
}

/// Like [`reconcile_projection`], sharing a caller-owned [`crate::access::Preparation`]
/// (which may carry a server memory-pool grant) and returning the reservation that covers
/// the returned outcome and its formatted JSON response so HTTP callers keep it charged
/// until serialization finishes.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_projection_prepared(
    main: &Dataset,
    logical: &Dataset,
    policy_name: &str,
    policy_version: Option<u32>,
    options: &Options,
    access: Option<Arc<GraphAccess>>,
    preparation: &crate::access::Preparation,
    control: &Control,
) -> Result<(ReconciledProjection, crate::access::Reservation)> {
    control.check()?;
    check_store(main, logical)?;
    let (settings, settings_bytes) = settings_of(main)?;
    if !settings.enabled {
        return Err(managed(
            ManagedErrorKind::UnsupportedProfile,
            "the evidence profile is not enabled in this dataset",
        ));
    }
    if settings.matching_policies.contains_key(policy_name) {
        return Err(Error::invalid(format!(
            "policy: {policy_name} is a matching policy, not a review policy"
        )));
    }
    let Some(policy) = settings.policies.get(policy_name) else {
        return Err(Error::invalid(format!(
            "policy: the evidence settings name no review policy {policy_name}"
        )));
    };
    if let Some(expected) = policy_version
        && policy.version != expected
    {
        return Err(managed(
            ManagedErrorKind::PolicyChanged,
            format!(
                "policyVersion: policy {policy_name} is at version {}, not {expected}",
                policy.version
            ),
        ));
    }
    let operation = operation(main, options)?;
    let payload = rv::reconcile_payload_digest(policy_name, policy_version, control)?;
    if access.as_ref().is_some_and(|a| a.hides_triples()) {
        return Err(Error::NotPermitted(
            "reconciling a projection requires unmasked access to its graphs".into(),
        ));
    }
    for g in [&policy.supports, &policy.output] {
        let g = NamedNode::new(g.as_str()).map_err(|e| Error::invalid(e.to_string()))?;
        writable(options.write.as_ref(), &g)?;
    }
    for g in [&policy.decisions, &policy.supports, &policy.output]
        .into_iter()
        .chain(settings.proposals.iter())
    {
        let g = NamedNode::new(g.as_str()).map_err(|e| Error::invalid(e.to_string()))?;
        readable_optional(access.as_ref(), &g)?;
    }
    let capability = capability(logical, operation, &settings_bytes)?;
    let p = PolicyGraphs::new(policy_name, policy, &capability)?;
    let nonces = p.protected()?;
    let store = logical.store();
    let branch = store.branch_id();
    let head = store.snapshot();

    struct ReconcileReady {
        batch: Batch,
        projection: NamedNode,
        vouched_supports: Vec<NamedNode>,
        changed: Changed,
        insert_quads: Vec<(Quad, bool)>,
        delete_ids: Vec<[sparkles_core::id::Id; 4]>,
        _vouched_held: crate::access::Reservation,
        _output_held: crate::access::Reservation,
        _diff_held: crate::access::Reservation,
        _batch_held: crate::access::Reservation,
        response_held: crate::access::Reservation,
    }

    enum ReconcilePlan {
        Replayed(Box<ReconciledProjection>, crate::access::Reservation),
        Ready(Box<ReconcileReady>),
    }

    let plan = (|| -> Result<ReconcilePlan> {
        let mut state = State {
            reads: Reads::new(preparation, &head),
            seen: None,
            claim_graphs: claim_graphs(&settings, &capability),
            superseded: BTreeSet::new(),
            withdrawn: BTreeSet::new(),
        };
        let records = record_graph_nonces(&settings, &capability);
        let op_iri = operation_iri(operation);
        if options.idempotency_key.is_some() {
            let probe = Batch {
                operation,
                operation_iri: op_iri.clone(),
                payload_digest: payload,
                principal: options.principal.clone(),
                idempotency_key: options.idempotency_key.clone(),
                used: None,
                graphs: Vec::new(),
                records: Vec::new(),
            };
            let found = state.reads.operation(&probe.operation_iri, &records)?;
            if let Some((commit, recorded_branch)) = replayed(found, &probe)? {
                // Preserve keyed refusal semantics above, then enforce caller read
                // authorization on every graph that recorded the replayed operation
                // before exposing any metadata from it.
                for (g, _) in &records {
                    if state.reads.has_operation(op_iri.as_str(), g.as_str())? {
                        readable_optional(access.as_ref(), g)?;
                    }
                }
                let mut search_graphs: Vec<G> = vec![p.supports.clone()];
                for g in settings.managed_graphs() {
                    if g != p.supports.0.as_str()
                        && let Ok(node) = NamedNode::new(g.as_str())
                    {
                        let nonce = capability.protection(g.as_str());
                        search_graphs.push((node, nonce));
                    }
                }
                let mut linked: Option<(NamedNode, G)> = None;
                'find_linked: for g in &search_graphs {
                    let mut gen_held = preparation.reserve(0)?;
                    for r in state.subjects_held(
                        PROV_WAS_GENERATED_BY,
                        &Term::NamedNode(op_iri.clone()),
                        g,
                        "projections",
                        &mut gen_held,
                    )? {
                        if state.typed(&r, g, RecordKind::Projection)?
                            && state.reads.contains(
                                r.as_str(),
                                &ev("reviewPolicy"),
                                &Term::NamedNode(p.policy_iri.clone()),
                                g.0.as_str(),
                            )?
                        {
                            linked = Some((r, g.clone()));
                            break 'find_linked;
                        }
                    }
                    if let Some(r) = state.iri(&op_iri, PROV_USED, g)?
                        && state.typed(&r, g, RecordKind::Projection)?
                        && state.reads.contains(
                            r.as_str(),
                            &ev("reviewPolicy"),
                            &Term::NamedNode(p.policy_iri.clone()),
                            g.0.as_str(),
                        )?
                    {
                        linked = Some((r, g.clone()));
                        break 'find_linked;
                    }
                }
                let (projection, proj_graph) = match linked {
                    Some((r, g)) => {
                        readable_optional(access.as_ref(), &g.0)?;
                        (r, g)
                    }
                    None => {
                        let mut proj_held = preparation.reserve(0)?;
                        let (all_proj, superseded_proj) =
                            state.policy_projections_held(&p, &mut proj_held)?;
                        let active_proj: Vec<NamedNode> = all_proj
                            .into_iter()
                            .filter(|r| !superseded_proj.contains(r))
                            .collect();
                        let mut chosen = None;
                        for r in &active_proj {
                            if state.text(r, &ev("policyVersion"), &p.supports)?.as_deref()
                                == Some(policy.version.to_string().as_str())
                            {
                                chosen = Some(r.clone());
                                break;
                            }
                        }
                        let proj = chosen.or_else(|| active_proj.into_iter().next()).unwrap_or(
                            rv::reconciled_projection_iri(
                                main.dataset_id(),
                                policy_name,
                                policy.version,
                                nonces,
                                operation,
                                control,
                            )?,
                        );
                        (proj, p.supports.clone())
                    }
                };
                let recorded_version = state
                    .text(&projection, &ev("policyVersion"), &proj_graph)?
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(policy.version);
                let recorded_output = state
                    .iri(&projection, &ev("output"), &proj_graph)?
                    .unwrap_or_else(|| p.output.0.clone());
                let replay_resp_bytes = (op_iri.as_str().len() as u64)
                    .saturating_add(p.policy_iri.as_str().len() as u64)
                    .saturating_add(recorded_output.as_str().len() as u64)
                    .saturating_add(projection.as_str().len() as u64)
                    .saturating_mul(2)
                    .saturating_add(1024);
                let response_held = preparation.reserve(replay_resp_bytes)?;
                return Ok(ReconcilePlan::Replayed(
                    Box::new(ReconciledProjection {
                        operation: op_iri,
                        committed: false,
                        replayed: true,
                        commit,
                        branch: recorded_branch.unwrap_or(branch),
                        policy: p.policy_iri.clone(),
                        policy_version: recorded_version,
                        output: recorded_output,
                        projection,
                        vouched_supports: Vec::new(),
                        changed: Changed::default(),
                        preview: None,
                    }),
                    response_held,
                ));
            }
        }
        if let Some(expected) = options.if_head
            && head.commit != expected
        {
            return Err(managed(
                ManagedErrorKind::HeadChanged,
                format!(
                    "ifHead: the head of the branch is commit {}, not {expected}",
                    head.commit
                ),
            ));
        }
        // Catch same-version supports/output swaps and role changes across all
        // continuously protected managed graphs before scanning or planning changes.
        let protected_graphs = protected_managed_graphs(&settings, &capability);
        let protected_refs: Vec<(&str, Uuid)> = protected_graphs
            .iter()
            .map(|(g, n)| (g.as_str(), *n))
            .collect();
        state.check_same_version_drift(&p, policy.version, &settings, &protected_refs)?;
        // Existing projection records of this policy, and which remain unsuperseded.
        let mut proj_held = preparation.reserve(0)?;
        let (existing_projections, superseded_projections) =
            state.policy_projections_held(&p, &mut proj_held)?;
        let mut active_projections: Vec<NamedNode> = existing_projections
            .into_iter()
            .filter(|r| !superseded_projections.contains(r))
            .collect();
        let version_str = policy.version.to_string();
        let mut all_active_valid = !active_projections.is_empty();
        let mut current_for_version = None;
        for r in &active_projections {
            if state.projection_same_version_drift(r, &p, policy.version)? {
                return Err(same_version_policy_changed(&p.name));
            }
            if state.projection_matches_nonces(r, &p)? {
                if state.text(r, &ev("policyVersion"), &p.supports)?.as_deref()
                    == Some(version_str.as_str())
                {
                    current_for_version = Some(r.clone());
                }
            } else {
                all_active_valid = false;
            }
        }
        let already_valid_current = if all_active_valid {
            current_for_version
        } else {
            None
        };

        // Collect every live authenticated acceptance support for this policy and output.
        let mut vouched_held = preparation.reserve(0)?;
        let mut vouched_supports = Vec::new();
        let mut vouched_triples: Vec<Triple> = Vec::new();
        let mut vouched_set: HashSet<Triple> = HashSet::new();
        {
            let mut candidates_held = preparation.reserve(0)?;
            let candidates = state.subjects_held(
                &ev("output"),
                &Term::NamedNode(p.output.0.clone()),
                &p.supports,
                "supports",
                &mut candidates_held,
            )?;
            for s in candidates {
                control.check()?;
                if !state.reads.contains(
                    s.as_str(),
                    &ev("reviewPolicy"),
                    &Term::NamedNode(p.policy_iri.clone()),
                    p.supports.0.as_str(),
                )? {
                    continue;
                }
                // `State::live` enforces current claim authentication and withdrawal
                // absence consistently across reconciliation and ordinary writes.
                if !state.live(&s, &p)? {
                    continue;
                }
                let Some(d) = state.iri(&s, &ev("decision"), &p.supports)? else {
                    continue;
                };
                if !state.reads.contains(
                    d.as_str(),
                    &ev("reviewPolicy"),
                    &Term::NamedNode(p.policy_iri.clone()),
                    p.decisions.0.as_str(),
                )? || state.disposition(&d, &p.decisions)? != Some(Disposition::Accepted)
                {
                    continue;
                }
                let Some(triple) = state.supported(&s, &p)? else {
                    continue;
                };
                if !matches!(triple.subject, NamedOrBlankNode::NamedNode(_)) {
                    continue;
                }
                let triple_est = sparkles_core::evidence::write::triple_bytes(&triple);
                let bytes = (s.as_str().len() as u64)
                    .saturating_add(160)
                    .saturating_add(triple_est.saturating_mul(2));
                vouched_held.grow_by(bytes)?;
                vouched_supports.push(s);
                if vouched_set.insert(triple.clone()) {
                    vouched_triples.push(triple);
                }
            }
        }
        vouched_supports.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        vouched_supports.dedup();
        vouched_triples.sort_by(cmp_triple);

        // Compare the output graph's current quads against the vouched triples.
        let mut output_held = preparation.reserve(0)?;
        let (existing_output, more_output) = state.reads.graph_quads_up_to(
            p.output.0.as_str(),
            claims::MAX_CLAIM_SCAN,
            &mut output_held,
        )?;
        if more_output {
            return Err(managed(
                ManagedErrorKind::IncompleteEvidence,
                format!(
                    "the output graph holds more than {} triples, so nothing was written",
                    claims::MAX_CLAIM_SCAN
                ),
            ));
        }
        let mut diff_held = preparation.reserve(0)?;
        let output_len = p.output.0.as_str().len() as u64;
        let base_resp = (op_iri.as_str().len() as u64)
            .saturating_add(p.policy_iri.as_str().len() as u64)
            .saturating_add(output_len)
            .saturating_mul(2)
            .saturating_add(2048);
        let mut response_held = preparation.reserve(base_resp)?;
        for s in &vouched_supports {
            // Rust NamedNode plus JSON string entry in `vouchedSupports`.
            response_held.grow_by(
                (s.as_str().len() as u64)
                    .saturating_mul(2)
                    .saturating_add(64),
            )?;
        }
        let mut max_nt_scratch: u64 = 0;
        let mut present_vouched: HashSet<Triple> = HashSet::new();
        let mut to_delete: Vec<([sparkles_core::id::Id; 4], Triple)> = Vec::new();
        for (ids, triple) in existing_output {
            control.check()?;
            if vouched_set.contains(&triple) {
                diff_held.grow_by(
                    (std::mem::size_of::<Triple>() as u64)
                        .saturating_mul(2)
                        .saturating_add(64),
                )?;
                present_vouched.insert(triple);
            } else {
                let triple_est = sparkles_core::evidence::write::triple_bytes(&triple);
                let nt_len = sparkles_core::evidence::write::ntriples_triple_bytes(&triple);
                let json_len =
                    sparkles_core::evidence::write::json_escaped_ntriples_triple_bytes(&triple);
                if nt_len > max_nt_scratch {
                    response_held.grow_by(nt_len - max_nt_scratch)?;
                    max_nt_scratch = nt_len;
                }
                let staging_bytes = (std::mem::size_of::<([sparkles_core::id::Id; 4], Triple)>()
                    as u64)
                    .saturating_mul(2)
                    .saturating_add(std::mem::size_of::<[sparkles_core::id::Id; 4]>() as u64)
                    .saturating_add(64);
                diff_held.grow_by(staging_bytes)?;
                // Retained `ProjectionChange` plus serialized JSON entry `{"output":"...","triple":"..."}`.
                let resp_bytes = (std::mem::size_of::<ProjectionChange>() as u64)
                    .saturating_add(triple_est)
                    .saturating_add(json_len)
                    .saturating_add(output_len.saturating_mul(2))
                    .saturating_add(96);
                response_held.grow_by(resp_bytes)?;
                to_delete.push((ids, triple));
            }
        }
        control.check()?;
        to_delete.sort_by(|a, b| cmp_triple(&a.1, &b.1).then_with(|| a.0.cmp(&b.0)));

        let mut to_insert: Vec<Triple> = Vec::new();
        for triple in &vouched_triples {
            control.check()?;
            if !present_vouched.contains(triple) {
                let triple_est = sparkles_core::evidence::write::triple_bytes(triple);
                let nt_len = sparkles_core::evidence::write::ntriples_triple_bytes(triple);
                let json_len =
                    sparkles_core::evidence::write::json_escaped_ntriples_triple_bytes(triple);
                if nt_len > max_nt_scratch {
                    response_held.grow_by(nt_len - max_nt_scratch)?;
                    max_nt_scratch = nt_len;
                }
                let ins_bytes = (std::mem::size_of::<Triple>() as u64)
                    .saturating_add(std::mem::size_of::<(Quad, bool)>() as u64)
                    .saturating_add(triple_est.saturating_mul(2))
                    .saturating_add(output_len.saturating_mul(2))
                    .saturating_add(64);
                diff_held.grow_by(ins_bytes)?;
                let resp_bytes = (std::mem::size_of::<ProjectionChange>() as u64)
                    .saturating_add(triple_est)
                    .saturating_add(json_len)
                    .saturating_add(output_len.saturating_mul(2))
                    .saturating_add(96);
                response_held.grow_by(resp_bytes)?;
                to_insert.push(triple.clone());
            }
        }

        let mut changed = Changed::default();
        let mut insert_quads = Vec::with_capacity(to_insert.len());
        for t in to_insert {
            control.check()?;
            insert_quads.push((output_quad(&t, &p.output.0), true));
            changed.inserted.push(ProjectionChange {
                output: p.output.0.clone(),
                triple: t,
            });
        }
        let mut delete_ids = Vec::with_capacity(to_delete.len());
        for (ids, t) in to_delete {
            control.check()?;
            delete_ids.push(ids);
            changed.removed.push(ProjectionChange {
                output: p.output.0.clone(),
                triple: t,
            });
        }

        let mut batch = Batch {
            operation,
            operation_iri: op_iri,
            payload_digest: payload,
            principal: options.principal.clone(),
            idempotency_key: options.idempotency_key.clone(),
            used: None,
            graphs: vec![p.supports.0.clone()],
            records: Vec::new(),
        };
        let mut batch_held = preparation.reserve(0)?;
        let projection = if let Some(existing_proj) = already_valid_current
            && insert_quads.is_empty()
            && delete_ids.is_empty()
        {
            batch.used = Some(existing_proj.clone());
            existing_proj
        } else {
            active_projections.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            active_projections.dedup();
            // Charge both the owned `PlannedRecord` triples (plus `record_digest`
            // scratch) and the duplicate `Vec<Quad>` that `publish` materializes via
            // `batch.quads(...)` before allocating them.
            let supports_len = p.supports.0.as_str().len() as u64;
            batch_held.grow_by(supports_len.saturating_mul(24).saturating_add(8192))?;
            for pred in &active_projections {
                let pred_bytes = (pred.as_str().len() as u64)
                    .saturating_mul(2)
                    .saturating_add(supports_len)
                    .saturating_add((std::mem::size_of::<Triple>() as u64).saturating_mul(2))
                    .saturating_add((std::mem::size_of::<Quad>() as u64).saturating_mul(2))
                    .saturating_add(512);
                batch_held.grow_by(pred_bytes)?;
            }
            let record = rv::reconciled_projection_record(
                main.dataset_id(),
                policy_name,
                policy.version,
                &p.output.0,
                p.mode,
                nonces,
                operation,
                &active_projections,
                &p.supports.0,
                control,
            )?;
            let iri = record.iri.clone();
            batch.records.push(record);
            let skip = [false];
            check_identities(&mut state, &batch, &records, &skip)?;
            iri
        };
        drop(active_projections);
        drop(proj_held);
        // Preadmit the projection IRI's Rust and JSON bytes before publication so
        // committed responses never need another fallible reservation after publish.
        response_held.grow_by((projection.as_str().len() as u64).saturating_mul(2))?;
        Ok(ReconcilePlan::Ready(Box::new(ReconcileReady {
            batch,
            projection,
            vouched_supports,
            changed,
            insert_quads,
            delete_ids,
            _vouched_held: vouched_held,
            _output_held: output_held,
            _diff_held: diff_held,
            _batch_held: batch_held,
            response_held,
        })))
    })()
    .map_err(budget_to_incomplete)?;

    let ReconcileReady {
        batch,
        projection,
        vouched_supports,
        changed,
        insert_quads,
        delete_ids,
        _vouched_held,
        _output_held,
        _diff_held,
        _batch_held,
        response_held,
    } = match plan {
        ReconcilePlan::Ready(ready) => *ready,
        ReconcilePlan::Replayed(replayed, response_held) => return Ok((*replayed, response_held)),
    };
    let skip = vec![false; batch.records.len()];
    logical
        .store()
        .failpoint("evidence-reconcile-before-publish");
    let committed = publish(
        main,
        logical,
        capability,
        head.commit,
        Some(head.generation.uid),
        &batch,
        &skip,
        &insert_quads,
        &delete_ids,
        options,
        control,
    )?;
    logical
        .store()
        .failpoint("evidence-reconcile-after-publish");
    drop(_batch_held);
    drop(_diff_held);
    drop(_output_held);
    drop(_vouched_held);
    Ok((
        ReconciledProjection {
            operation: batch.operation_iri,
            committed: committed.committed,
            replayed: false,
            commit: committed.commit,
            branch,
            policy: p.policy_iri,
            policy_version: policy.version,
            output: p.output.0,
            projection,
            vouched_supports,
            changed,
            preview: committed.preview,
        },
        response_held,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence;

    #[test]
    fn reconcile_maps_raw_read_budget_exhaustion_to_incomplete_evidence() {
        let ds = Dataset::memory();
        let settings = Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            decisions: vec!["urn:ev:decisions".into()],
            supports: vec!["urn:ev:supports".into()],
            projections: vec!["urn:ev:accepted".into()],
            policies: [(
                "main".to_string(),
                Policy {
                    version: 1,
                    admitted_kinds: vec![ClaimKind::Reported],
                    required_scope: Vec::new(),
                    conflicts: ConflictMode::Refuse,
                    boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
                    decisions: "urn:ev:decisions".into(),
                    supports: "urn:ev:supports".into(),
                    output: "urn:ev:accepted".into(),
                },
            )]
            .into_iter()
            .collect(),
            ..Settings::default()
        };
        evidence::configure(&ds, &settings, &Control::none()).unwrap();
        // Detach SUPPORTS and insert 80 unauthenticated long-IRI candidate subjects
        // (~80 KiB total) so candidate scanning exhausts a 48 KiB budget AFTER
        // Preparation::new succeeds (CONTEXT_BYTES = 256).
        evidence::detach(&ds, "urn:ev:supports").unwrap();
        let mut nq = String::new();
        for i in 0..80 {
            nq.push_str(&format!(
                "<urn:sup:{i}:{}> <{EV}output> <urn:ev:accepted> <urn:ev:supports> .\n",
                "s".repeat(1024)
            ));
        }
        ds.load_str(&nq, oxrdfio::RdfFormat::NQuads).unwrap();
        evidence::configure(&ds, &settings, &Control::none()).unwrap();

        let budget: u64 = 48 << 10;
        assert!(
            crate::access::Preparation::new(Control::none(), budget).is_ok(),
            "test budget must admit the Preparation constructor so State reads hit it"
        );
        let opts = Options::new(NamedNode::new("urn:reviewer").unwrap());
        let err = reconcile_projection_with_budget(
            &ds,
            &ds,
            "main",
            Some(1),
            &opts,
            None,
            &Control::none(),
            budget,
        )
        .unwrap_err();
        assert_eq!(err.code(), "incomplete-evidence", "{err}");
    }

    #[test]
    fn reconcile_charges_supersession_lineage_record_and_publication_copies() {
        let ds = Dataset::memory();
        let settings = Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            decisions: vec!["urn:ev:decisions".into()],
            supports: vec!["urn:ev:supports".into()],
            projections: vec!["urn:ev:accepted".into()],
            policies: [(
                "main".to_string(),
                Policy {
                    version: 1,
                    admitted_kinds: vec![ClaimKind::Reported],
                    required_scope: Vec::new(),
                    conflicts: ConflictMode::Refuse,
                    boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
                    decisions: "urn:ev:decisions".into(),
                    supports: "urn:ev:supports".into(),
                    output: "urn:ev:accepted".into(),
                },
            )]
            .into_iter()
            .collect(),
            ..Settings::default()
        };
        evidence::configure(&ds, &settings, &Control::none()).unwrap();
        // Detach SUPPORTS and plant 100 unauthenticated long-IRI projection predecessors
        // (~102 KiB of IRI text) so the initial scan fits in 440 KiB, while building the
        // `prov:wasRevisionOf` record and reserving its publication `Vec<Quad>` copy
        // exceeds 440 KiB and refuses cleanly before writing.
        evidence::detach(&ds, "urn:ev:supports").unwrap();
        let policy_iri = rv::policy_iri("main");
        let mut nq = String::new();
        for i in 0..100 {
            let pred = format!("urn:proj:{i}:{}", "p".repeat(1024));
            nq.push_str(&format!(
                "<{pred}> <{RDF_TYPE}> <{EV}Projection> <urn:ev:supports> .\n\
                 <{pred}> <{EV}reviewPolicy> <{}> <urn:ev:supports> .\n",
                policy_iri.as_str()
            ));
        }
        ds.load_str(&nq, oxrdfio::RdfFormat::NQuads).unwrap();
        evidence::configure(&ds, &settings, &Control::none()).unwrap();

        let tight: u64 = 440 << 10;
        // Confirm the projection scan alone fits inside `tight`, proving the refusal below
        // comes from charging the supersession lineage record and publication copies.
        {
            let (_, settings_bytes) = settings_of(&ds).unwrap();
            let cap = capability(&ds, Uuid::new_v4(), &settings_bytes).unwrap();
            let p = PolicyGraphs::new("main", &settings.policies["main"], &cap).unwrap();
            let snap = ds.store().snapshot();
            let prep = crate::access::Preparation::new(Control::none(), tight).unwrap();
            let mut st = State {
                reads: Reads::new(&prep, &snap),
                seen: None,
                claim_graphs: claim_graphs(&settings, &cap),
                superseded: BTreeSet::new(),
                withdrawn: BTreeSet::new(),
            };
            let mut proj_held = prep.reserve(0).unwrap();
            let (recs, _) = st.policy_projections_held(&p, &mut proj_held).unwrap();
            assert_eq!(recs.len(), 100);
        }

        let head = ds.store().head_commit().seq;
        let opts = Options::new(NamedNode::new("urn:reviewer").unwrap());
        let err = reconcile_projection_with_budget(
            &ds,
            &ds,
            "main",
            Some(1),
            &opts,
            None,
            &Control::none(),
            tight,
        )
        .unwrap_err();
        assert_eq!(err.code(), "incomplete-evidence", "{err}");
        assert_eq!(ds.store().head_commit().seq, head);

        // With enough allowance, reconciliation succeeds and supersedes all 100 predecessors.
        let rec = reconcile_projection_with_budget(
            &ds,
            &ds,
            "main",
            Some(1),
            &opts,
            None,
            &Control::none(),
            2 << 20,
        )
        .unwrap();
        assert!(rec.committed);
    }
}
