//! Claim occurrences and their review state under a named policy at an evidence view
//! (spec C25, "Recall and Ask with claims").
//!
//! A [`PolicyClaims`] reads one [`BoundEvidenceView`] with the policy and protections
//! that the view's configuration records, never the current settings. A state is
//! decided from the authenticated records of the unmasked view, with the liveness rules
//! of reviews and withdrawals. An occurrence is reported only when the caller's view
//! holds every record that decides it: the claim, its current decision and any
//! withdrawal that ends it. Otherwise it answers as missing, so a reader learns nothing
//! about hidden records, and an older decision never stands in for a hidden current one.

use super::{G, PolicyGraphs, State, caller_view, caller_view_evidence, check_store};
use crate::access::GraphAccess;
use crate::evidence::propose::READS_BYTES;
use crate::managed::{ManagedErrorKind, OwnerKind};
use crate::task::Control;
use crate::{Dataset, Error, Result};
use oxrdf::{NamedNode, Term, Triple};
use sparkles_core::evidence::review::{Disposition, MAX_EVIDENCE};
use sparkles_core::evidence::write::Reads;
use sparkles_core::evidence::{ClaimKind, Polarity, RecordKind, Settings};
use sparkles_core::store::BoundEvidenceView;
use std::collections::BTreeSet;
use std::sync::Arc;
use uuid::Uuid;

const EV: &str = "urn:x-sparkles:evidence:";
const PROV: &str = "http://www.w3.org/ns/prov#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
/// The class of imported instruction documents, such as `AGENTS.md`.
pub const MEM_INSTRUCTIONS: &str = "urn:x-sparkles:mem:Instructions";

/// The most quads of one proposal graph that [`PolicyClaims::about`] reads.
pub const MAX_CLAIM_SCAN: usize = 1_000_000;
/// The most claims one call of [`PolicyClaims::states`] or [`PolicyClaims::about`]
/// considers.
pub const MAX_CLAIMS: usize = MAX_EVIDENCE;

fn ev(local: &str) -> String {
    format!("{EV}{local}")
}

/// The state of a claim occurrence under one review policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClaimState {
    Accepted,
    Rejected,
    NeedsEvidence,
    Unreviewed,
    Withdrawn,
}

impl ClaimState {
    /// `accepted`, `rejected`, `needs-evidence`, `unreviewed` or `withdrawn`.
    pub fn name(self) -> &'static str {
        match self {
            ClaimState::Accepted => "accepted",
            ClaimState::Rejected => "rejected",
            ClaimState::NeedsEvidence => "needs-evidence",
            ClaimState::Unreviewed => "unreviewed",
            ClaimState::Withdrawn => "withdrawn",
        }
    }
}

/// Where a claim's source sits for the reader: an instruction document or other data.
/// Neither grants authority. Every claim is source data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClaimOrigin {
    Instructions,
    Source,
}

impl ClaimOrigin {
    pub fn name(self) -> &'static str {
        match self {
            ClaimOrigin::Instructions => "instructions",
            ClaimOrigin::Source => "source",
        }
    }
}

/// One claim occurrence the caller may see, with its state under the policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimOccurrence {
    pub claim: NamedNode,
    /// The proposal graph that holds it.
    pub graph: NamedNode,
    pub state: ClaimState,
    /// The current decision under the policy, when one exists.
    pub decision: Option<NamedNode>,
    /// The policy version that decision was made under.
    pub decision_version: Option<u32>,
    pub kind: ClaimKind,
    pub polarity: Polarity,
    pub proposition: Triple,
    /// The source revision the claim derives from, when the caller can see it.
    pub revision: Option<NamedNode>,
    /// That revision's stable source, when the caller can see it.
    pub source: Option<NamedNode>,
    /// The attributed author.
    pub by: Option<NamedNode>,
    pub origin: ClaimOrigin,
}

/// The evidence settings an evidence view's configuration records, or the default
/// settings when it records none.
pub(super) fn view_settings(view: &BoundEvidenceView) -> Result<Settings> {
    match view.owner_settings(OwnerKind::Evidence) {
        Some(bytes) => super::super::recorded(bytes),
        None => Ok(Settings::default()),
    }
}

/// The evidence settings that `view`'s configuration records, or the default settings
/// when it records none. A reader compares them with the current settings to notice a
/// configuration change during a long read.
pub fn recorded_settings(view: &BoundEvidenceView) -> Result<Settings> {
    view_settings(view)
}

/// The refusal of a review policy the caller cannot use: unknown at the view, a
/// matching policy, or one whose graphs it may not read. The answer is the same in
/// each case, so a caller cannot tell a hidden policy from a missing one.
pub fn unknown_policy(name: &str) -> Error {
    Error::invalid(format!(
        "policy: no review policy {name} is available to the caller at this view"
    ))
}

/// The refusal of a matching policy the caller cannot use, with the same rule.
pub fn unknown_matching_policy(name: &str) -> Error {
    Error::invalid(format!(
        "policy: no matching policy {name} is available to the caller at this view"
    ))
}

/// Whether `e` is the refusal of a target whose records exceed [`MAX_EVIDENCE`].
fn beyond_record_bound(e: &Error) -> bool {
    matches!(e, Error::Managed(m) if m.kind == ManagedErrorKind::IncompleteEvidence)
}

fn can_read(access: Option<&Arc<GraphAccess>>, g: &str) -> bool {
    let term = Term::NamedNode(NamedNode::new_unchecked(g));
    access.is_none_or(|a| a.readable(Some(&term)))
}

/// The version of the matching policy `name` at `view`. Refuses with
/// [`unknown_matching_policy`] unless the view's configuration names that matching
/// policy and the caller may read its decisions, maps and output graphs and the
/// identity graphs of its boundary.
pub fn check_matching_policy(
    view: &BoundEvidenceView,
    name: &str,
    access: Option<&Arc<GraphAccess>>,
) -> Result<u32> {
    if let Some(a) = access {
        view.validate_access(a)?;
    }
    let settings = view_settings(view)?;
    let Some(p) = settings
        .matching_policies
        .get(name)
        .filter(|_| settings.enabled)
    else {
        return Err(unknown_matching_policy(name));
    };
    if !super::identity::required_graphs(&settings, p)
        .iter()
        .all(|g| can_read(access, g))
    {
        return Err(unknown_matching_policy(name));
    }
    Ok(p.version)
}

/// The claims of one review policy at one evidence view, as one caller may see them.
pub struct PolicyClaims<'a> {
    state: State<'a>,
    access: Option<Arc<GraphAccess>>,
    graphs: PolicyGraphs,
    settings: Settings,
    version: u32,
    control: Control,
}

/// The policy a [`PolicyClaims`] reads, as the view records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyRef {
    pub name: String,
    pub iri: NamedNode,
    pub version: u32,
    /// The output graph of its accepted projection.
    pub output: NamedNode,
}

/// Read the claims of the review policy `policy` at `view` with the caller's `access`,
/// `None` for everything, inside `f`. Refuses with [`unknown_policy`] when the view's
/// configuration names no such review policy or the caller may not read its decisions,
/// supports and output graphs, and as `projection-unavailable` when the policy's
/// projection no longer authenticates.
pub fn with_policy_claims<R>(
    view: &BoundEvidenceView,
    policy: &str,
    access: Option<Arc<GraphAccess>>,
    control: &Control,
    f: impl FnOnce(&mut PolicyClaims<'_>) -> Result<R>,
) -> Result<R> {
    control.check()?;
    if let Some(a) = access.as_deref() {
        view.validate_access(a)?;
    }
    let settings = view_settings(view)?;
    let Some(p) = settings.policies.get(policy).filter(|_| settings.enabled) else {
        return Err(unknown_policy(policy));
    };
    for g in [&p.decisions, &p.supports, &p.output] {
        if !can_read(access.as_ref(), g) {
            return Err(unknown_policy(policy));
        }
    }
    let owner = view.owner(OwnerKind::Evidence);
    let nonce = |g: &str| -> Option<Uuid> {
        owner
            .filter(|e| e.graphs.iter().any(|x| x == g))
            .and_then(|e| e.protections.get(g).copied())
    };
    let graphs = PolicyGraphs::with_nonces(policy, p, &nonce)?;
    let claim_graphs: Vec<G> = settings
        .proposals
        .iter()
        .map(|g| (NamedNode::new_unchecked(g), nonce(g)))
        .collect();
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    let all = access
        .clone()
        .or_else(|| Some(Arc::new(GraphAccess::all())));
    let seen = caller_view_evidence(all.as_ref(), view, &preparation)?;
    let state = State {
        reads: Reads::from_evidence(&preparation, view),
        seen: seen.as_ref().map(|v| Reads::from_prepared(&preparation, v)),
        claim_graphs,
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    let mut claims = PolicyClaims {
        state,
        access,
        graphs,
        version: p.version,
        settings: settings.clone(),
        control: control.clone(),
    };
    claims.state.check_projection(&claims.graphs, None)?;
    f(&mut claims)
}

/// The states of `claims` under the review policy `policy` at `view`, as the caller
/// with `access` may see them: one entry per claim, `None` for a claim that is not an
/// authenticated claim occurrence or that the caller may not see.
pub fn claim_states(
    view: &BoundEvidenceView,
    policy: &str,
    claims: &[NamedNode],
    access: Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<Vec<Option<ClaimOccurrence>>> {
    with_policy_claims(view, policy, access, control, |c| c.states(claims))
}

/// The states of `claims` under the review policy `policy` at the live head of `logical`
/// (with settings and protections from `main`), for datasets whose configuration history
/// is not activated. Captures settings and graph protection nonces from a single registry
/// snapshot and verifies that the registry revision remains stable across data snapshot
/// capture. Returns the read commit sequence alongside one entry per claim.
pub(crate) fn claim_states_live(
    main: &Dataset,
    logical: &Dataset,
    policy: &str,
    claims: &[NamedNode],
    access: Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<(u64, Vec<Option<ClaimOccurrence>>)> {
    claim_states_live_with_hook(main, logical, policy, claims, access, control, |_| {})
}

fn claim_states_live_with_hook(
    main: &Dataset,
    logical: &Dataset,
    policy: &str,
    claims: &[NamedNode],
    access: Option<Arc<GraphAccess>>,
    control: &Control,
    mut between_registry_and_head: impl FnMut(usize),
) -> Result<(u64, Vec<Option<ClaimOccurrence>>)> {
    const MAX_ATTEMPTS: usize = 3;
    control.check()?;
    check_store(main, logical)?;
    let attempts = if access.is_some() { 1 } else { MAX_ATTEMPTS };
    let mut selected = None;
    for attempt in 0..attempts {
        control.check()?;
        let registry = main.store().managed_snapshot()?;
        if let Some(reason) = registry.as_ref().and_then(|s| s.unavailable.as_ref()) {
            return Err(crate::managed::ManagedError::error(
                ManagedErrorKind::Unavailable,
                reason.clone(),
            ));
        }
        let rev_before = registry.as_ref().map_or(0, |s| s.graphs.revision);
        between_registry_and_head(attempt);
        let head = logical.store().snapshot();
        let rev_after = main.store().managed_revision()?;
        if rev_after == rev_before {
            selected = Some((registry, head));
            break;
        }
    }
    let Some((registry, head)) = selected else {
        return Err(crate::managed::ManagedError::error(
            ManagedErrorKind::Unavailable,
            "configuration changed during evidence selection",
        ));
    };
    if let Some(a) = access.as_deref() {
        a.validate_bound_snapshot(&head)?;
    }
    let Some(registry) = registry else {
        return Err(unknown_policy(policy));
    };
    let Some(bytes) = registry.settings.get(&OwnerKind::Evidence) else {
        return Err(unknown_policy(policy));
    };
    let settings = crate::evidence::recorded(bytes)?;
    let Some(p) = settings.policies.get(policy).filter(|_| settings.enabled) else {
        return Err(unknown_policy(policy));
    };
    for g in [&p.decisions, &p.supports, &p.output] {
        if !can_read(access.as_ref(), g) {
            return Err(unknown_policy(policy));
        }
    }
    let nonce = |g: &str| -> Option<Uuid> {
        registry
            .graphs
            .owners
            .get(&OwnerKind::Evidence)
            .filter(|e| e.graphs.iter().any(|x| x == g))
            .and_then(|e| e.protections.get(g).copied())
    };
    let graphs = PolicyGraphs::with_nonces(policy, p, &nonce)?;
    let claim_graphs: Vec<G> = settings
        .proposals
        .iter()
        .map(|g| (NamedNode::new_unchecked(g), nonce(g)))
        .collect();
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    let all = access
        .clone()
        .or_else(|| Some(Arc::new(GraphAccess::all())));
    let seen = caller_view(all.as_ref(), &head, &preparation)?;
    let state = State {
        reads: Reads::new(&preparation, &head),
        seen: seen.as_ref().map(|v| Reads::from_prepared(&preparation, v)),
        claim_graphs,
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    let mut policy_claims = PolicyClaims {
        state,
        access,
        graphs,
        version: p.version,
        settings,
        control: control.clone(),
    };
    policy_claims
        .state
        .check_projection(&policy_claims.graphs, None)?;
    let occurrences = policy_claims.states(claims)?;
    Ok((head.commit, occurrences))
}

impl PolicyClaims<'_> {
    /// The policy, as the view records it.
    pub fn policy(&self) -> PolicyRef {
        PolicyRef {
            name: self.graphs.name.clone(),
            iri: self.graphs.policy_iri.clone(),
            version: self.version,
            output: self.graphs.output.0.clone(),
        }
    }

    fn readable(&self, g: &NamedNode) -> bool {
        can_read(self.access.as_ref(), g.as_str())
    }

    /// Whether the caller sees `s` in `g`: it may read `g` and its view hides no triple
    /// of `s` there.
    fn sees(&mut self, s: &NamedNode, g: &NamedNode) -> Result<bool> {
        Ok(self.readable(g) && !self.state.masked(s, g)?)
    }

    /// The states of `claims`, one entry per claim, `None` for one the caller may not
    /// see or that is not an authenticated claim occurrence.
    pub fn states(&mut self, claims: &[NamedNode]) -> Result<Vec<Option<ClaimOccurrence>>> {
        if claims.len() > MAX_CLAIMS {
            return Err(super::incomplete("claims"));
        }
        let mut out = Vec::with_capacity(claims.len());
        for c in claims {
            self.control.check()?;
            out.push(self.state_of(c)?);
        }
        Ok(out)
    }

    /// The occurrence `claim` as the caller may see it. A claim whose own records, such
    /// as its decisions, withdrawals or supports, exceed the record bound is read as a
    /// hidden one, so that hidden records never surface as an `incomplete-evidence`
    /// refusal.
    fn state_of(&mut self, claim: &NamedNode) -> Result<Option<ClaimOccurrence>> {
        match self.state_of_bounded(claim) {
            Err(e) if beyond_record_bound(&e) => Ok(None),
            r => r,
        }
    }

    fn state_of_bounded(&mut self, claim: &NamedNode) -> Result<Option<ClaimOccurrence>> {
        // the proposal graph that holds the claim as an authenticated record
        let mut home = None;
        for g in self.state.claim_graphs.clone() {
            if self.state.typed(claim, &g, RecordKind::Claim)? {
                home = Some(g);
                break;
            }
        }
        let Some(g) = home else {
            return Ok(None);
        };
        if !self.sees(claim, &g.0)? {
            return Ok(None);
        }
        let facts = super::claim_facts(&mut self.state, claim, &g)?;
        let (Some(proposition), Some(kind)) = (facts.proposition, facts.kind) else {
            return Ok(None);
        };
        let polarity = if facts.positive {
            Polarity::Positive
        } else if facts.negative {
            Polarity::Negative
        } else {
            Polarity::Uncertain
        };
        // a withdrawal in any configured proposal graph ends the occurrence
        let mut withdrawn = false;
        for wg in self.state.claim_graphs.clone() {
            if let Some(w) = self.state.withdrawal(claim, &wg)? {
                if !self.sees(&w, &wg.0)? {
                    return Ok(None);
                }
                withdrawn = true;
                break;
            }
        }
        let decisions = self.graphs.decisions.clone();
        let mut current = self
            .state
            .current(claim, &self.graphs.policy_iri, &decisions)?;
        current.sort();
        let decision = current.into_iter().next();
        let mut decision_version = None;
        let mut disposition = None;
        if let Some(d) = &decision {
            if self.state.masked(d, &decisions.0)? {
                return Ok(None);
            }
            disposition = self.state.disposition(d, &decisions)?;
            decision_version = self
                .state
                .text(d, &ev("policyVersion"), &decisions)?
                .and_then(|v| v.parse().ok());
        }
        if !self.supports_seen(claim)? {
            return Ok(None);
        }
        let state = match (withdrawn, disposition) {
            (true, _) => ClaimState::Withdrawn,
            (false, Some(Disposition::Accepted)) => ClaimState::Accepted,
            (false, Some(Disposition::Rejected)) => ClaimState::Rejected,
            (false, Some(Disposition::NeedsEvidence)) => ClaimState::NeedsEvidence,
            (false, None) => ClaimState::Unreviewed,
        };
        let by = self
            .state
            .iri(claim, &format!("{PROV}wasAttributedTo"), &g)?;
        let mut revision = self
            .state
            .iri(claim, &format!("{PROV}wasDerivedFrom"), &g)?;
        let mut source = None;
        if let Some(r) = revision.clone() {
            let mut visible = false;
            for sg in self.settings.source_records.clone() {
                let sg = NamedNode::new_unchecked(sg);
                if self
                    .state
                    .reads
                    .subject_triples(r.as_str(), sg.as_str(), 1)?
                    == 0
                {
                    continue;
                }
                if !self.sees(&r, &sg)? {
                    continue;
                }
                visible = true;
                source =
                    self.state
                        .iri(&r, &format!("{PROV}specializationOf"), &(sg.clone(), None))?;
                break;
            }
            if !visible {
                revision = None;
            }
        }
        let mut origin = ClaimOrigin::Source;
        for x in revision.iter().chain(source.iter()) {
            if self.typed_instructions(x)? {
                origin = ClaimOrigin::Instructions;
                break;
            }
        }
        Ok(Some(ClaimOccurrence {
            claim: claim.clone(),
            graph: g.0,
            state,
            decision,
            decision_version,
            kind,
            polarity,
            proposition,
            revision,
            source,
            by,
            origin,
        }))
    }

    /// Whether a triple in the caller's view types `x` as `mem:Instructions`, in any
    /// graph the caller may read, the default graph included. A hidden or unreadable
    /// typing triple is not consulted and takes no part of any bound.
    fn typed_instructions(&mut self, x: &NamedNode) -> Result<bool> {
        let class = Term::NamedNode(NamedNode::new_unchecked(MEM_INSTRUCTIONS));
        let access = self.access.clone();
        let reads = match self.state.seen.as_mut() {
            Some(seen) => seen,
            None => &mut self.state.reads,
        };
        reads.in_some_graph(x.as_str(), RDF_TYPE, &class, |snap, g| {
            access.as_ref().is_none_or(|a| a.readable_id(snap, g))
        })
    }

    /// Whether the caller sees every live acceptance support of the policy that cites
    /// `claim`. An occurrence whose citing support is hidden is itself hidden.
    fn supports_seen(&mut self, claim: &NamedNode) -> Result<bool> {
        let supports = self.graphs.supports.clone();
        for s in self.state.subjects(
            &ev("claim"),
            &Term::NamedNode(claim.clone()),
            &supports,
            "supports",
        )? {
            if !self.state.live(&s, &self.graphs)? {
                continue;
            }
            if !self.sees(&s, &supports.0)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// For each of `triples`, the claims of the policy's live acceptance supports that
    /// project it into the output graph, sorted. A support the caller's view hides is
    /// left out. The claims still need [`Self::states`] before they are shown.
    pub fn supports(&mut self, triples: &[Triple]) -> Result<Vec<Vec<NamedNode>>> {
        let mut out = Vec::with_capacity(triples.len());
        for t in triples {
            self.control.check()?;
            out.push(self.supports_of(t)?);
        }
        Ok(out)
    }

    /// The claims of `t`'s live supports the caller sees. A triple whose supports
    /// exceed the record bound cites none, and a support whose own records, or its
    /// claim's, exceed it is left out without affecting the others.
    fn supports_of(&mut self, t: &Triple) -> Result<Vec<NamedNode>> {
        let supports = self.graphs.supports.clone();
        let listed = match self.state.subjects(
            &ev("supportedTriple"),
            &Term::Triple(Box::new(t.clone())),
            &supports,
            "supports",
        ) {
            Err(e) if beyond_record_bound(&e) => return Ok(Vec::new()),
            r => r?,
        };
        let mut claims = BTreeSet::new();
        for s in listed {
            self.control.check()?;
            match self.support_claim(&s, t) {
                Err(e) if beyond_record_bound(&e) => {}
                r => claims.extend(r?),
            }
        }
        Ok(claims.into_iter().collect())
    }

    /// The claim of the support `s` when it is live, projects `t` and is not hidden.
    fn support_claim(&mut self, s: &NamedNode, t: &Triple) -> Result<Option<NamedNode>> {
        let supports = self.graphs.supports.clone();
        if self.state.supported(s, &self.graphs)?.as_ref() != Some(t)
            || !self.state.live(s, &self.graphs)?
            || self.state.masked(s, &supports.0)?
        {
            return Ok(None);
        }
        self.state.iri(s, &ev("claim"), &supports)
    }

    /// The occurrences the caller may see in the readable proposal graphs of the
    /// policy's boundary whose proposition has one of `entities` as its subject or
    /// object, in claim order. Each candidate passes [`Self::states`]' visibility check
    /// before it is counted, so a hidden claim takes no place in the bound. Refuses as
    /// `incomplete-evidence` when a graph holds more than [`MAX_CLAIM_SCAN`] quads or the
    /// visible occurrences exceed [`MAX_CLAIMS`].
    pub fn about(&mut self, entities: &[NamedNode]) -> Result<Vec<ClaimOccurrence>> {
        let wanted: BTreeSet<String> = entities.iter().map(|e| e.as_str().to_string()).collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let boundary: Vec<G> = self
            .graphs
            .boundary
            .iter()
            .filter(|(g, _)| self.settings.proposals.iter().any(|p| p == g.as_str()))
            .cloned()
            .collect();
        // the identities found stay charged until the occurrences are read
        let mut held = self.state.reads.preparation().reserve(0)?;
        let mut found = BTreeSet::new();
        for (g, _) in boundary {
            self.control.check()?;
            if !self.readable(&g) {
                continue;
            }
            let (rows, more) = self.state.reads.reifiers_about(
                RDF_REIFIES,
                g.as_str(),
                &wanted,
                MAX_CLAIM_SCAN,
                &mut held,
            )?;
            if more {
                return Err(crate::managed::ManagedError::error(
                    ManagedErrorKind::IncompleteEvidence,
                    format!(
                        "a proposal graph holds more than {MAX_CLAIM_SCAN} quads, so its claims about the result were not all read"
                    ),
                ));
            }
            found.extend(rows);
        }
        let mut out = Vec::new();
        for c in found {
            self.control.check()?;
            if let Some(o) = self.state_of(&c)? {
                if out.len() == MAX_CLAIMS {
                    return Err(super::incomplete("claims"));
                }
                out.push(o);
            }
        }
        Ok(out)
    }

    /// The occurrences among `claims` the caller may see, in the order given, with a
    /// claim named twice listed once. Unlike [`Self::states`], only visible occurrences
    /// count toward [`MAX_CLAIMS`].
    pub fn visible(&mut self, claims: &[NamedNode]) -> Result<Vec<ClaimOccurrence>> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for c in claims {
            self.control.check()?;
            if !seen.insert(c.as_str()) {
                continue;
            }
            if let Some(o) = self.state_of(c)? {
                if out.len() == MAX_CLAIMS {
                    return Err(super::incomplete("claims"));
                }
                out.push(o);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target beyond the record bound is the one refusal a claim read turns into a
    /// hidden occurrence; any other error still refuses.
    #[test]
    fn only_the_record_bound_reads_as_hidden() {
        assert!(beyond_record_bound(&super::super::incomplete("decisions")));
        assert!(!beyond_record_bound(&unknown_policy("main")));
        assert!(!beyond_record_bound(&crate::managed::ManagedError::error(
            ManagedErrorKind::ProjectionUnavailable,
            "x"
        )));
    }

    #[test]
    fn graph_only_stale_bound_policy_is_refused_by_evidence_reads() {
        use crate::access::{
            Decider, Exemptions, GraphAccess, Graphs, GuardDecision, GuardSnapshot, Preparation,
        };
        use crate::evidence::{MatchingPolicy, configure, resolve_entities_at};
        use crate::store::Snapshot;
        struct CommitGuard;
        impl GuardDecision for CommitGuard {
            fn exempt(&self, snap: &Arc<Snapshot>) -> Vec<String> {
                if snap.commit <= 2 {
                    vec!["urn:ev:decisions".into()]
                } else {
                    Vec::new()
                }
            }
            fn exempt_controlled(
                &self,
                snap: &GuardSnapshot<'_>,
                preparation: &Preparation,
            ) -> Result<Exemptions> {
                let mut out = Exemptions::new(preparation)?;
                if snap.commit() <= 2 {
                    out.push("urn:ev:decisions")?;
                }
                Ok(out)
            }
        }
        let ds = crate::Dataset::memory();
        let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
        ds.store()
            .activate_memory_configuration_history(&prep)
            .unwrap();
        let settings = Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            decisions: vec!["urn:ev:decisions".into()],
            supports: vec!["urn:ev:supports".into()],
            identity: vec!["urn:ev:id".into(), "urn:ev:maps".into()],
            projections: vec!["urn:ev:accepted".into(), "urn:ev:map".into()],
            policies: [(
                "main".into(),
                super::super::Policy {
                    version: 1,
                    admitted_kinds: vec![ClaimKind::Reported],
                    required_scope: vec![],
                    conflicts: super::super::ConflictMode::AdmitLabelled,
                    boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
                    decisions: "urn:ev:decisions".into(),
                    supports: "urn:ev:supports".into(),
                    output: "urn:ev:accepted".into(),
                },
            )]
            .into_iter()
            .collect(),
            matching_policies: [(
                "people".into(),
                MatchingPolicy {
                    version: 1,
                    boundary: vec!["urn:ev:id".into()],
                    decisions: "urn:ev:decisions".into(),
                    maps: "urn:ev:maps".into(),
                    output: "urn:ev:map".into(),
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        configure(&ds, &settings, &Control::none()).unwrap();
        let snap_before = ds.store().snapshot();
        let unbound = Arc::new(GraphAccess::graphs(
            Graphs::All.guarded_by("urn:ev:decisions", Decider(Arc::new(CommitGuard))),
            Graphs::none(),
        ));
        let bound_before = unbound.bound(&snap_before);
        assert!(bound_before.read.allows_iri("urn:ev:decisions"));

        // Advance commit so CommitGuard would no longer exempt urn:ev:decisions.
        ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
        let view = ds
            .store()
            .evidence_view(&crate::history::At::Head, &prep)
            .unwrap();
        assert!(matches!(
            with_policy_claims(
                &view,
                "main",
                Some(bound_before.clone()),
                &Control::none(),
                |_| Ok(()),
            ),
            Err(Error::Invalid(ref m)) if m.contains("different snapshot")
        ));
        assert!(matches!(
            check_matching_policy(&view, "people", Some(&bound_before)),
            Err(Error::Invalid(ref m)) if m.contains("different snapshot")
        ));
        assert!(matches!(
            resolve_entities_at(&view, "people", &[], Some(bound_before), &Control::none()),
            Err(Error::Invalid(ref m)) if m.contains("different snapshot")
        ));
    }

    #[test]
    fn claim_states_live_rejects_torn_registry_and_head() {
        use crate::evidence::{Policy, binding, configure, detach};

        let ds = crate::Dataset::memory();
        let mut settings = Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            decisions: vec!["urn:ev:decisions".into(), "urn:ev:decisions2".into()],
            supports: vec!["urn:ev:supports".into(), "urn:ev:supports2".into()],
            projections: vec!["urn:ev:accepted".into(), "urn:ev:accepted2".into()],
            policies: [
                (
                    "review-v1".into(),
                    Policy {
                        version: 1,
                        admitted_kinds: vec![ClaimKind::Reported],
                        required_scope: vec![],
                        conflicts: super::super::ConflictMode::AdmitLabelled,
                        boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
                        decisions: "urn:ev:decisions".into(),
                        supports: "urn:ev:supports".into(),
                        output: "urn:ev:accepted".into(),
                    },
                ),
                (
                    "review-v2".into(),
                    Policy {
                        version: 1,
                        admitted_kinds: vec![ClaimKind::Reported],
                        required_scope: vec![],
                        conflicts: super::super::ConflictMode::AdmitLabelled,
                        boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
                        decisions: "urn:ev:decisions2".into(),
                        supports: "urn:ev:supports2".into(),
                        output: "urn:ev:accepted2".into(),
                    },
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        configure(&ds, &settings, &Control::none()).unwrap();

        // Publish one accepted claim so review-v1 has a live Projection record with nonces.
        let prop_req = serde_json::to_vec(&serde_json::json!({
            "revisions": [{
                "key": "r1",
                "source": "https://example.org/src/1",
                "sourceKey": "s1",
                "content": { "text": "fact" },
            }],
            "claims": [{
                "key": "c1",
                "occurrenceKey": "o1",
                "revision": { "key": "r1" },
                "proposition": "<https://example.org/a> <https://example.org/b> <https://example.org/c>",
                "kind": "reported",
                "polarity": "positive",
                "author": "https://example.org/org/a",
            }],
        }))
        .unwrap();
        let proposed =
            binding::propose_bytes(&ds, None, None, None, &prop_req, &Control::none()).unwrap();
        let claim_rec = proposed["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == "c1")
            .unwrap();
        let claim_iri = NamedNode::new(claim_rec["iri"].as_str().unwrap()).unwrap();
        let claim_digest = claim_rec["recordDigest"].as_str().unwrap();
        let rev_req = serde_json::to_vec(&serde_json::json!({
            "policy": "review-v1",
            "policyVersion": 1,
            "ifHead": ds.head_commit().seq,
            "decisions": [{
                "key": "d1",
                "target": claim_iri.as_str(),
                "targetDigest": claim_digest,
                "disposition": "accepted",
            }],
        }))
        .unwrap();
        binding::review_bytes(&ds, None, None, None, &rev_req, &Control::none()).unwrap();

        // Sanity: coherent live read observes the accepted claim.
        let (_, states) = claim_states_live(
            &ds,
            &ds,
            "review-v1",
            std::slice::from_ref(&claim_iri),
            None,
            &Control::none(),
        )
        .unwrap();
        assert_eq!(states[0].as_ref().unwrap().state, ClaimState::Accepted);

        // 1. A one-shot detach between registry snapshot and head capture triggers retry
        // and observes the coherent post-detach state (projection-unavailable), never
        // old settings paired with new protections.
        let mut did_detach = false;
        let err = claim_states_live_with_hook(
            &ds,
            &ds,
            "review-v1",
            std::slice::from_ref(&claim_iri),
            None,
            &Control::none(),
            |_| {
                if !did_detach {
                    did_detach = true;
                    detach(&ds, "urn:ev:accepted").unwrap();
                }
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "projection-unavailable", "{err:?}");

        // Re-enable protection.
        configure(&ds, &settings, &Control::none()).unwrap();

        // 2. Persistent registry churn across every attempt fails explicitly with
        // managed-unavailable rather than pairing an older head with a newer policy.
        let err = claim_states_live_with_hook(
            &ds,
            &ds,
            "review-v1",
            std::slice::from_ref(&claim_iri),
            None,
            &Control::none(),
            |attempt| {
                let (d1, s1, o1, d2, s2, o2) = if attempt % 2 == 0 {
                    (
                        "urn:ev:decisions2",
                        "urn:ev:supports2",
                        "urn:ev:accepted2",
                        "urn:ev:decisions",
                        "urn:ev:supports",
                        "urn:ev:accepted",
                    )
                } else {
                    (
                        "urn:ev:decisions",
                        "urn:ev:supports",
                        "urn:ev:accepted",
                        "urn:ev:decisions2",
                        "urn:ev:supports2",
                        "urn:ev:accepted2",
                    )
                };
                let p1 = settings.policies.get_mut("review-v1").unwrap();
                p1.decisions = d1.into();
                p1.supports = s1.into();
                p1.output = o1.into();
                let p2 = settings.policies.get_mut("review-v2").unwrap();
                p2.decisions = d2.into();
                p2.supports = s2.into();
                p2.output = o2.into();
                configure(&ds, &settings, &Control::none()).unwrap();
            },
        )
        .unwrap_err();
        assert_eq!(err.code(), "managed-unavailable", "{err:?}");
    }
}
