//! Reviews of identity proposals and their resolution maps (spec C25 §6).
//!
//! An identity proposal names two or more exact entity identifiers and proposes that
//! they may denote one thing, `ev:PossibleMatch`, or that they denote different things,
//! `ev:DistinctEntities`. A review under a matching policy of the evidence settings
//! appends immutable decisions about proposals, as a review of claims does, and nothing
//! asserts `owl:sameAs`.
//!
//! A matching policy's effective matches are its authenticated `PossibleMatch`
//! proposals whose current decision under the policy accepts them. They join entities
//! into clusters, the connected components of the entities they name. The policy's
//! output graph holds the current resolution map: `<member> ev:resolvesTo
//! <representative>` for each member of each cluster of two or more entities, the
//! representative included. The representative is the smallest IRI among the preferred
//! entities of the cluster's effective matches, or the smallest member IRI when none
//! names one. An effective `DistinctEntities` proposal keeps its entities in different
//! clusters, and an operation whose final decisions would join them answers
//! `identity-conflict`.
//!
//! Every operation that changes which accepted decisions the map follows writes one
//! immutable `ev:ResolutionMap` revision in the policy's maps graph. It names the
//! revision before it, the decisions it adds and those it ends, and the protection
//! nonces of the policy's graphs. A split or any other reversal is a superseding
//! decision, so proposals, decisions and earlier revisions stay as they were, and
//! accepting a reversed match again restores the earlier map.
//!
//! The output is ordinary RDF that a query joins explicitly. Queries are never
//! rewritten through a map.

use super::{
    Changed, Decided, Evidence, G, MapChange, Options, ProjectionChange, Reviewed, State,
    branch_name, caller_view, capability, check_identities, check_store, claim_graphs, hidden,
    incomplete, operation, projection_unavailable, publish, readable, record_graph_nonces,
    replayed, writable,
};
use crate::access::GraphAccess;
use crate::evidence::propose::{READS_BYTES, managed, settings_of};
use crate::evidence::resolve::{Resolution, unavailable};
use crate::managed::ManagedErrorKind;
use crate::task::Control;
use crate::{Catalog, Dataset, Error, Result};
use oxrdf::{GraphName, NamedNode, Quad, Term, Triple};
use sparkles_core::evidence::review::{
    self as rv, BOUNDARY_PROTECTION, Batch, DecisionInput, Disposition, MAP_NONCE_PREDICATES,
    MAX_DECISIONS, MAX_EVIDENCE, MapNonces, MapRevision,
};
use sparkles_core::evidence::write::{ExistingRecord, Reads, operation_iri};
use sparkles_core::evidence::{IdentityRelation, MatchingPolicy, RecordKind, Settings};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use uuid::Uuid;

fn ev(local: &str) -> String {
    super::ev(local)
}

/// The predicate of the resolution map's output triples.
pub const RESOLVES_TO: &str = "urn:x-sparkles:evidence:resolvesTo";

/// The most entities one resolution read answers.
pub const MAX_RESOLVE: usize = 100;
/// The most members of one cluster a resolution read lists.
pub const MAX_MEMBERS: usize = 1000;
/// The most IRI bytes one operation retains for the proposals and clusters it reads.
pub const RETAINED_BYTES: usize = 64 << 20;
/// Charge `n` bytes to `bytes`, refusing as `incomplete-evidence` beyond `limit`.
fn retain(bytes: &mut usize, limit: usize, n: usize) -> Result<()> {
    *bytes = bytes.saturating_add(n);
    if *bytes > limit {
        return Err(managed(
            ManagedErrorKind::IncompleteEvidence,
            format!(
                "the identifiers of the targets and clusters exceed {} MiB, so nothing was written",
                limit >> 20
            ),
        ));
    }
    Ok(())
}

/// The IRI bytes a proposal's facts retain.
fn fact_bytes(f: &Facts) -> usize {
    f.entities
        .iter()
        .chain(&f.preferred)
        .map(|x| x.as_str().len())
        .sum()
}

/// The most boundary protections a map revision records.
const MAX_BOUNDARY: usize = 4096;

/// One matching policy's graphs and their current nonces.
pub(super) struct MapGraphs {
    pub(super) name: String,
    pub(super) policy_iri: NamedNode,
    pub(super) version: u32,
    pub(super) decisions: G,
    pub(super) maps: G,
    pub(super) output: G,
    /// Every graph of the boundary, where evidence links are looked for.
    pub(super) boundary: Vec<G>,
    /// The identity graphs of the boundary, where proposals are looked for.
    pub(super) proposals: Vec<G>,
}

impl MapGraphs {
    fn new(
        name: &str,
        policy: &MatchingPolicy,
        settings: &Settings,
        nonce: &dyn Fn(&str) -> Option<Uuid>,
    ) -> Result<Self> {
        let g = |iri: &str| -> Result<G> {
            let n = NamedNode::new(iri).map_err(|e| Error::invalid(format!("graph: {e}")))?;
            Ok((n, nonce(iri)))
        };
        let boundary: Vec<G> = policy
            .boundary
            .iter()
            .map(|b| g(b))
            .collect::<Result<_>>()?;
        let proposals = boundary
            .iter()
            .filter(|(b, _)| settings.identity.iter().any(|i| i == b.as_str()))
            .cloned()
            .collect();
        Ok(MapGraphs {
            name: name.to_string(),
            policy_iri: rv::policy_iri(name),
            version: policy.version,
            decisions: g(&policy.decisions)?,
            maps: g(&policy.maps)?,
            output: g(&policy.output)?,
            boundary,
            proposals,
        })
    }

    /// The protected identity graphs of the boundary with their nonces, sorted.
    fn boundary_protections(&self) -> Vec<(NamedNode, Uuid)> {
        let mut out: Vec<(NamedNode, Uuid)> = self
            .proposals
            .iter()
            .filter_map(|(g, n)| n.map(|n| (g.clone(), n)))
            .collect();
        out.sort();
        out
    }

    fn nonces(&self) -> Option<MapNonces> {
        Some(MapNonces {
            decisions: self.decisions.1?,
            maps: self.maps.1?,
            output: self.output.1?,
        })
    }

    /// Refuse unless the decisions, maps and output graphs are protected.
    fn protected(&self) -> Result<MapNonces> {
        for (g, n) in [&self.decisions, &self.maps, &self.output] {
            if n.is_none() {
                return Err(sparkles_core::evidence::write::unprotected(g));
            }
        }
        Ok(self.nonces().expect("protected"))
    }
}

/// The latest revision of a policy's resolution map.
#[derive(Clone, Debug)]
pub(super) struct Latest {
    pub(super) iri: NamedNode,
    pub(super) number: u64,
}

/// The latest revision of `m`'s map, found by galloping and binary search over the
/// minted revision identities, which a typed writer publishes in order. Refuses as
/// `projection-unavailable` when a revision present in the maps graph does not
/// authenticate or was written under other protections than the current ones, and
/// when the output holds triples while no revision exists.
fn latest(
    state: &mut State<'_>,
    dataset: Uuid,
    m: &MapGraphs,
    control: &Control,
) -> Result<Option<Latest>> {
    let probe = |n: u64, state: &mut State<'_>| -> Result<Option<NamedNode>> {
        let iri = rv::map_iri(dataset, &m.name, n, control)?;
        match state.reads.record(&iri, std::slice::from_ref(&m.maps))? {
            ExistingRecord::Absent => Ok(None),
            ExistingRecord::Present {
                authenticated: true,
                ..
            } => Ok(Some(iri)),
            _ => Err(projection_unavailable(&m.name)),
        }
    };
    let Some(first) = probe(1, state)? else {
        if state.reads.graph_nonempty(m.output.0.as_str())? {
            return Err(projection_unavailable(&m.name));
        }
        return Ok(None);
    };
    // the largest present revision lies in [lo, hi)
    let (mut lo, mut lo_iri) = (1u64, first);
    let mut hi = 2u64;
    loop {
        control.check()?;
        match probe(hi, state)? {
            Some(iri) => {
                lo = hi;
                lo_iri = iri;
                hi = hi
                    .checked_mul(2)
                    .ok_or_else(|| projection_unavailable(&m.name))?;
            }
            None => break,
        }
    }
    while hi - lo > 1 {
        control.check()?;
        let mid = lo + (hi - lo) / 2;
        match probe(mid, state)? {
            Some(iri) => {
                lo = mid;
                lo_iri = iri;
            }
            None => hi = mid,
        }
    }
    // the latest revision's protections must be the current ones
    let now = m.nonces().map(|n| [n.decisions, n.maps, n.output]);
    let mut recorded = Vec::new();
    for predicate in MAP_NONCE_PREDICATES {
        recorded.push(
            state
                .text(&lo_iri, predicate, &m.maps)?
                .and_then(|v| Uuid::parse_str(&v).ok()),
        );
    }
    if !now.is_some_and(|n| recorded.iter().zip(n).all(|(a, b)| *a == Some(b))) {
        return Err(projection_unavailable(&m.name));
    }
    // Every identity graph the map was built from must still be in the boundary under
    // the same protection, or matches the map follows would no longer be read. A graph
    // added to the boundary since holds no proposal the policy accepted.
    let current: BTreeSet<String> = m
        .boundary_protections()
        .iter()
        .map(|(g, n)| rv::boundary_protection(g, *n))
        .collect();
    let (built, more) = state.reads.objects_up_to(
        lo_iri.as_str(),
        BOUNDARY_PROTECTION,
        m.maps.0.as_str(),
        MAX_BOUNDARY,
    )?;
    if more
        || !built.iter().all(|t| match t {
            Term::Literal(l) => current.contains(l.value()),
            _ => false,
        })
    {
        return Err(projection_unavailable(&m.name));
    }
    Ok(Some(Latest {
        iri: lo_iri,
        number: lo,
    }))
}

/// What an authenticated identity proposal records.
#[derive(Clone, Debug)]
pub(super) struct Facts {
    pub(super) relation: Option<IdentityRelation>,
    pub(super) entities: Vec<NamedNode>,
    pub(super) preferred: Option<NamedNode>,
}

fn facts(state: &mut State<'_>, p: &NamedNode, g: &G) -> Result<Facts> {
    let relation = state.iri(p, &ev("relation"), g)?.and_then(|r| {
        [
            IdentityRelation::PossibleMatch,
            IdentityRelation::DistinctEntities,
        ]
        .into_iter()
        .find(|k| ev(k.local()) == r.as_str())
    });
    let (v, more) = state.reads.objects_up_to(
        p.as_str(),
        &ev("entity"),
        g.0.as_str(),
        sparkles_core::evidence::MAX_IDENTITY_ENTITIES,
    )?;
    let entities: Vec<NamedNode> = v
        .into_iter()
        .filter_map(|t| match t {
            Term::NamedNode(n) => Some(n),
            _ => None,
        })
        .collect();
    let preferred = state.iri(p, &ev("preferredEntity"), g)?;
    Ok(Facts {
        // a record of more entities than a proposal holds is not one the writer made
        relation: relation.filter(|_| !more && entities.len() >= 2),
        entities,
        preferred,
    })
}

/// Which state of the decisions a cluster is read in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum When {
    /// The stored decisions.
    Before,
    /// The stored decisions with this operation's decisions applied.
    After,
}

/// One cluster of a resolution map.
#[derive(Clone, Debug, Default)]
struct Cluster {
    entities: BTreeSet<NamedNode>,
    /// The effective matches that join it.
    matches: BTreeSet<NamedNode>,
    /// The effective separations that name one of its entities.
    distincts: BTreeSet<NamedNode>,
    preferred: BTreeSet<NamedNode>,
}

impl Cluster {
    /// The representative of a cluster of two or more entities.
    fn representative(&self) -> Option<&NamedNode> {
        if self.entities.len() < 2 {
            return None;
        }
        self.preferred
            .iter()
            .find(|p| self.entities.contains(*p))
            .or_else(|| self.entities.first())
    }
}

/// Reads the clusters of one matching policy, at most [`MAX_EVIDENCE`] entities and
/// proposals for each state of the decisions.
struct Clusters<'m> {
    m: &'m MapGraphs,
    /// The proposals of this operation's decisions and the disposition each gets.
    batch: BTreeMap<NamedNode, Disposition>,
    facts: BTreeMap<NamedNode, (G, Facts)>,
    /// Each proposal's effect in each state.
    effective: BTreeMap<(NamedNode, bool), bool>,
    visits: [usize; 2],
    /// The IRI bytes retained for the operation's targets, facts and clusters, at most
    /// `limit`.
    bytes: usize,
    limit: usize,
    /// The key of the decision whose clusters are read, which a refusal names.
    key: String,
    /// The most proposals naming one entity that are read.
    candidates: usize,
    /// Leave out a proposal, or a proposal whose current decision is a record, that the
    /// caller's view hides any triple of, instead of refusing as an unavailable target.
    /// A read sets it, since authentication reads only a record's receipt and
    /// protection and would otherwise let a masked record join a cluster.
    hide_masked: bool,
    /// The most decisions of one proposal a read takes from the caller's view.
    decision_limit: usize,
}

impl<'m> Clusters<'m> {
    fn new(m: &'m MapGraphs, batch: BTreeMap<NamedNode, Disposition>) -> Self {
        Clusters {
            m,
            batch,
            facts: BTreeMap::new(),
            effective: BTreeMap::new(),
            visits: [0, 0],
            bytes: 0,
            limit: RETAINED_BYTES,
            key: String::new(),
            candidates: MAX_EVIDENCE,
            hide_masked: false,
            decision_limit: MAX_EVIDENCE,
        }
    }

    /// Charge `n` retained bytes against the operation's allowance.
    fn retain(&mut self, n: usize) -> Result<()> {
        retain(&mut self.bytes, self.limit, n)
    }

    fn charge(&mut self, when: When) -> Result<()> {
        let i = usize::from(when == When::After);
        self.visits[i] += 1;
        if self.visits[i] > MAX_EVIDENCE {
            return Err(incomplete("the entities and proposals of the clusters"));
        }
        Ok(())
    }

    /// The effective proposals that name `e` in `when`, after `used` accepted each.
    ///
    /// Candidates are resolved in order before a bound is reported, so a record the
    /// caller cannot read is refused, or left out, before any count that includes it.
    /// A proposal the caller's view masks is left out without being charged when it is
    /// not effective, and an effective one is refused by `used`. Only the facts of
    /// effective proposals are kept.
    fn naming(
        &mut self,
        state: &mut State<'_>,
        e: &NamedNode,
        when: When,
        used: &mut dyn FnMut(&mut State<'_>, &NamedNode, &G) -> Result<()>,
    ) -> Result<Vec<NamedNode>> {
        let mut out = Vec::new();
        for g in self.m.proposals.clone() {
            let o = Term::NamedNode(e.clone());
            // A read takes its candidates from the caller's view, so its bound counts only
            // triples the caller can read.
            let (candidates, more) = match state.seen.as_mut().filter(|_| self.hide_masked) {
                Some(seen) => {
                    seen.subjects_up_to(&ev("entity"), &o, g.0.as_str(), self.candidates)?
                }
                None => {
                    state
                        .reads
                        .subjects_up_to(&ev("entity"), &o, g.0.as_str(), self.candidates)?
                }
            };
            for p in candidates {
                if self.hide_masked && state.masked(&p, &g.0)? {
                    continue;
                }
                let known = self.facts.contains_key(&p);
                if !known && !state.typed(&p, &g, RecordKind::IdentityProposal)? {
                    continue;
                }
                if !self.is_effective(state, &p, when)? {
                    if !state.masked(&p, &g.0)? {
                        self.charge(when)?;
                    }
                    continue;
                }
                if !known {
                    let f = facts(state, &p, &g)?;
                    if f.relation.is_none() {
                        continue;
                    }
                    used(state, &p, &g)?;
                    self.retain(fact_bytes(&f))?;
                    self.facts.insert(p.clone(), (g.clone(), f));
                } else {
                    used(state, &p, &g)?;
                }
                self.charge(when)?;
                out.push(p);
            }
            if more && self.hide_masked {
                return Err(incomplete("the proposals of an entity"));
            }
            if more {
                // When the caller's view shows no more candidates than the bound, the
                // excess is hidden from it, and a hidden one may be effective. The
                // review then answers as its target being unavailable, so the refusal
                // reports no count that includes a hidden record.
                if let Some(seen) = state.seen.as_mut() {
                    let (_, visible_more) = seen.subjects_up_to(
                        &ev("entity"),
                        &Term::NamedNode(e.clone()),
                        g.0.as_str(),
                        self.candidates,
                    )?;
                    if !visible_more {
                        return Err(unavailable(&self.key));
                    }
                }
                return Err(incomplete("the proposals of an entity"));
            }
        }
        Ok(out)
    }

    /// The current decisions of `p` under the policy that the caller's view shows in
    /// full, for a read. Candidates come from the caller's view, so the bound counts
    /// only decision triples the caller can read, and a decision the view hides any
    /// triple of is left out. Authentication and supersession are read at the unmasked
    /// head, so a hidden later decision still ends an earlier visible one.
    fn visible_current(&mut self, state: &mut State<'_>, p: &NamedNode) -> Result<Vec<NamedNode>> {
        let decisions = self.m.decisions.clone();
        let o = Term::NamedNode(p.clone());
        let (candidates, more) = match state.seen.as_mut() {
            Some(seen) => {
                seen.subjects_up_to(&ev("target"), &o, decisions.0.as_str(), self.decision_limit)?
            }
            None => state.reads.subjects_up_to(
                &ev("target"),
                &o,
                decisions.0.as_str(),
                self.decision_limit,
            )?,
        };
        if more {
            return Err(incomplete("decisions"));
        }
        let mut out = Vec::new();
        for d in candidates {
            if state.masked(&d, &decisions.0)? {
                continue;
            }
            if state.typed(&d, &decisions, RecordKind::ReviewDecision)?
                && state.reads.contains(
                    d.as_str(),
                    &ev("reviewPolicy"),
                    &Term::NamedNode(self.m.policy_iri.clone()),
                    decisions.0.as_str(),
                )?
                && !state.is_superseded(&d, &decisions)?
            {
                out.push(d);
            }
        }
        Ok(out)
    }

    /// Whether `p`'s current decision under the policy accepts it, in `when`.
    fn is_effective(&mut self, state: &mut State<'_>, p: &NamedNode, when: When) -> Result<bool> {
        let k = (p.clone(), when == When::After);
        if let Some(v) = self.effective.get(&k) {
            return Ok(*v);
        }
        let v = match (when, self.batch.get(p)) {
            (When::After, Some(d)) => *d == Disposition::Accepted,
            _ => {
                // the stored decisions, without this operation's supersessions, are the
                // state before; after it, a proposal this operation does not decide has
                // the same current decision
                let superseded = std::mem::take(&mut state.superseded);
                let current = if self.hide_masked {
                    self.visible_current(state, p)
                } else {
                    state.current(p, &self.m.policy_iri, &self.m.decisions)
                };
                state.superseded = superseded;
                let current = current?;
                match current.as_slice() {
                    [one] => {
                        state.disposition(one, &self.m.decisions)? == Some(Disposition::Accepted)
                    }
                    [] => false,
                    _ => {
                        return Err(managed(
                            ManagedErrorKind::ImmutableRecordConflict,
                            format!(
                                "<{}> has more than one current decision under policy {}",
                                p.as_str(),
                                self.m.name
                            ),
                        ));
                    }
                }
            }
        };
        self.effective.insert(k, v);
        Ok(v)
    }

    /// The cluster of `seed` in `when`. `used` is called once for each effective
    /// proposal the cluster depends on.
    fn cluster(
        &mut self,
        state: &mut State<'_>,
        seed: &NamedNode,
        when: When,
        used: &mut dyn FnMut(&mut State<'_>, &NamedNode, &G) -> Result<()>,
    ) -> Result<Cluster> {
        let mut c = Cluster::default();
        c.entities.insert(seed.clone());
        let mut queue = VecDeque::from([seed.clone()]);
        while let Some(e) = queue.pop_front() {
            self.charge(when)?;
            for p in self.naming(state, &e, when, used)? {
                if c.matches.contains(&p) || c.distincts.contains(&p) {
                    continue;
                }
                let Some((_, f)) = self.facts.get(&p).cloned() else {
                    continue;
                };
                let Some(relation) = f.relation else {
                    continue;
                };
                match relation {
                    IdentityRelation::PossibleMatch => {
                        c.matches.insert(p);
                        c.preferred.extend(f.preferred.clone());
                        for x in f.entities {
                            if !c.entities.contains(&x) {
                                self.retain(x.as_str().len())?;
                                c.entities.insert(x.clone());
                                queue.push_back(x);
                            }
                        }
                    }
                    IdentityRelation::DistinctEntities => {
                        c.distincts.insert(p);
                    }
                }
            }
        }
        Ok(c)
    }
}

/// The settings a review of identity proposals runs under.
pub(super) struct Request<'a> {
    pub(super) name: &'a str,
    pub(super) policy: &'a MatchingPolicy,
    pub(super) settings: &'a Settings,
    pub(super) settings_bytes: &'a [u8],
    pub(super) version: u32,
    pub(super) if_head: u64,
    /// The most proposals naming one entity that the review reads, [`MAX_EVIDENCE`]
    /// outside tests.
    pub(super) candidates: usize,
}

/// One target of a review, between its checks and its records.
struct Pending {
    facts: Facts,
}

/// Append `decisions` about identity proposals under the matching policy `r.name`, as
/// one commit of the evidence owner, and change the policy's resolution map to follow
/// them. The preconditions and refusals are those of a claim review. The caller also
/// needs update and read grants on the policy's maps and output graphs, and read access
/// to every effective proposal of the clusters the operation reads, and an operation
/// whose final decisions would join two entities that an effective separation keeps
/// apart answers `identity-conflict`.
#[allow(clippy::too_many_arguments)]
pub(super) fn review(
    catalog: Option<&Catalog>,
    main: &Dataset,
    logical: &Dataset,
    r: Request<'_>,
    decisions: &[DecisionInput],
    options: &Options,
    read: &mut dyn FnMut(&Dataset, &str) -> Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<Reviewed> {
    debug_assert!(!decisions.is_empty() && decisions.len() <= MAX_DECISIONS);
    if r.policy.version != r.version {
        return Err(managed(
            ManagedErrorKind::PolicyChanged,
            format!(
                "policyVersion: policy {} is at version {}, not {}",
                r.name, r.policy.version, r.version
            ),
        ));
    }
    let operation = operation(main, options)?;
    let payload = rv::review_payload_digest(r.name, r.version, decisions, control)?;
    let access = match branch_name(main, logical)? {
        Some(b) => read(main, &b),
        None => None,
    };
    for g in [&r.policy.decisions, &r.policy.maps, &r.policy.output] {
        let g = NamedNode::new(g.as_str()).map_err(|e| Error::invalid(e.to_string()))?;
        writable(options.write.as_ref(), &g)?;
        readable(access.as_ref(), &g)?;
    }
    let capability = capability(logical, operation, r.settings_bytes)?;
    let m = MapGraphs::new(r.name, r.policy, r.settings, &|g| capability.protection(g))?;
    // The clusters are read from every identity graph of the boundary, so the caller
    // must be able to read each of them, as for the resolution read.
    for (g, _) in &m.proposals {
        readable(access.as_ref(), g)?;
    }
    let nonces = m.protected()?;
    let store = logical.store();
    let branch = store.branch_id();
    let head = store.snapshot();
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    let mask_preparation =
        crate::access::Preparation::new(control.clone(), super::super::resolve::MEMORY_BYTES)?;
    let mut state = State {
        reads: Reads::new(&preparation, &head),
        seen: None,
        claim_graphs: claim_graphs(r.settings, &capability),
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    let records = record_graph_nonces(r.settings, &capability);
    let decided_iri = |d: &DecisionInput| -> Result<NamedNode> {
        Ok(rv::decision_record(
            main.dataset_id(),
            &options.principal,
            operation,
            r.name,
            r.version,
            d,
            None,
            &m.decisions.0,
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
            graphs: Vec::new(),
            records: Vec::new(),
        };
        let found = state.reads.operation(&probe.operation_iri, &records)?;
        if let Some((commit, recorded_branch)) = replayed(found, &probe)? {
            let mut out = Vec::with_capacity(decisions.len());
            for d in decisions {
                out.push(Decided {
                    key: d.key.clone(),
                    iri: decided_iri(d)?,
                    target: d.target.clone(),
                    disposition: d.disposition,
                    supersedes: d.supersedes.clone(),
                    support: None,
                    not_projected: None,
                    conflicts: Vec::new(),
                });
            }
            // the revision the first answer published, recorded with its receipt
            let (published, _) = state.reads.subjects_up_to(
                &ev("availabilityReceipt"),
                &Term::NamedNode(probe.operation_iri.clone()),
                m.maps.0.as_str(),
                MAX_DECISIONS + 2,
            )?;
            let mut map_revision = None;
            for s in published {
                if state.typed(&s, &m.maps, RecordKind::ResolutionMap)? {
                    map_revision = Some(s);
                }
            }
            return Ok(Reviewed {
                operation: probe.operation_iri,
                committed: false,
                replayed: true,
                commit,
                branch: recorded_branch.unwrap_or(branch),
                decisions: out,
                changed: Changed::default(),
                map_revision,
                preview: None,
            });
        }
    }
    if head.commit != r.if_head {
        return Err(managed(
            ManagedErrorKind::HeadChanged,
            format!(
                "ifHead: the head of the branch is commit {}, not {}",
                head.commit, r.if_head
            ),
        ));
    }
    let masked = caller_view(access.as_ref(), &head, &mask_preparation)?;
    state.seen = masked
        .as_ref()
        .map(|v| Reads::new(&mask_preparation, v.snapshot()));
    let previous = latest(&mut state, main.dataset_id(), &m, control)?;
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
    let mut pending = Vec::with_capacity(decisions.len());
    let mut retained = 0usize;
    for d in decisions {
        control.check()?;
        let target = evidence.record(&d.key, &d.target, &[RecordKind::IdentityProposal])?;
        let Some(tg) = m.boundary.iter().find(|(g, _)| *g == target.graph).cloned() else {
            return Err(unavailable(&d.key));
        };
        if !m.proposals.iter().any(|(g, _)| *g == tg.0) {
            return Err(unavailable(&d.key));
        }
        if target.record_digest != Some(d.target_digest) {
            return Err(managed(
                ManagedErrorKind::TargetChanged,
                format!(
                    "{}: the target's record digest is not the one the review names",
                    d.key
                ),
            ));
        }
        let current = state.current(&d.target, &m.policy_iri, &m.decisions)?;
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
                    "{}: supersedes does not name the target's current decision under policy {}",
                    d.key, r.name
                ),
            ));
        }
        let facts = facts(&mut state, &d.target, &tg)?;
        if facts.relation.is_none() {
            return Err(unavailable(&d.key));
        }
        if d.disposition == Disposition::Accepted {
            state.visible(&d.key, &d.target, &tg.0)?;
            evidence.links(&mut state, &d.key, &d.target, &m.boundary)?;
        }
        retain(&mut retained, RETAINED_BYTES, fact_bytes(&facts))?;
        pending.push(Pending { facts });
    }
    // The decisions the map follows change when an acceptance is added or an accepted
    // decision is superseded.
    let mut adds = Vec::new();
    let mut ends = Vec::new();
    let mut minted = Vec::with_capacity(decisions.len());
    for d in decisions {
        let iri = decided_iri(d)?;
        if d.disposition == Disposition::Accepted {
            adds.push(iri.clone());
        }
        if let Some(prior) = &d.supersedes
            && state.disposition(prior, &m.decisions)? == Some(Disposition::Accepted)
        {
            ends.push(prior.clone());
        }
        minted.push(iri);
    }
    state.superseded = decisions
        .iter()
        .filter_map(|d| d.supersedes.clone())
        .collect();
    let mut clusters = Clusters::new(
        &m,
        decisions
            .iter()
            .map(|d| (d.target.clone(), d.disposition))
            .collect(),
    );
    clusters.bytes = retained;
    clusters.candidates = r.candidates;
    let mut changes = Vec::new();
    let mut changed = Changed::default();
    if !adds.is_empty() || !ends.is_empty() {
        // Every effective proposal a cluster depends on must be readable by the
        // caller, or the review answers as that decision's target being unavailable.
        let mut checked: BTreeSet<NamedNode> = BTreeSet::new();
        let mut before: BTreeMap<NamedNode, Option<NamedNode>> = BTreeMap::new();
        let mut after: BTreeMap<NamedNode, Option<NamedNode>> = BTreeMap::new();
        for (d, item) in decisions.iter().zip(&pending) {
            let mut used = |state: &mut State<'_>, p: &NamedNode, g: &G| -> Result<()> {
                if checked.insert(p.clone()) {
                    evidence
                        .record(&d.key, p, &[RecordKind::IdentityProposal])
                        .map_err(|e| hidden(&d.key, e))?;
                    state.visible(&d.key, p, &g.0)?;
                }
                Ok(())
            };
            clusters.key = d.key.clone();
            for e in &item.facts.entities {
                control.check()?;
                for (when, out) in [(When::Before, &mut before), (When::After, &mut after)] {
                    if out.contains_key(e) {
                        continue;
                    }
                    let c = clusters.cluster(&mut state, e, when, &mut used)?;
                    if when == When::After {
                        separate(&clusters, &c, &d.key)?;
                    }
                    let rep = c.representative().cloned();
                    for x in &c.entities {
                        out.insert(x.clone(), rep.clone());
                    }
                }
            }
        }
        drop(evidence);
        // The output changes: every entity of a cluster before or after maps to its
        // representative after, or to nothing when its cluster is a single entity.
        let entities: BTreeSet<&NamedNode> = before.keys().chain(after.keys()).collect();
        let mut moved = Vec::new();
        for e in entities {
            control.check()?;
            let desired = after.get(e).cloned().flatten();
            let (present, _) =
                state
                    .reads
                    .objects_up_to(e.as_str(), RESOLVES_TO, m.output.0.as_str(), 16)?;
            let present: Vec<NamedNode> = present
                .into_iter()
                .filter_map(|t| match t {
                    Term::NamedNode(n) => Some(n),
                    _ => None,
                })
                .collect();
            for p in &present {
                if Some(p) != desired.as_ref() {
                    let t = resolves(e, p);
                    changes.push((quad(&t, &m.output.0), false));
                    changed.removed.push(ProjectionChange {
                        output: m.output.0.clone(),
                        triple: t,
                    });
                }
            }
            if let Some(rep) = &desired
                && !present.contains(rep)
            {
                let t = resolves(e, rep);
                changes.push((quad(&t, &m.output.0), true));
                changed.inserted.push(ProjectionChange {
                    output: m.output.0.clone(),
                    triple: t,
                });
            }
            if present.first() != desired.as_ref() || present.len() > 1 {
                moved.push(e.clone());
            }
        }
        changed.map = Some(MapChange {
            policy: m.policy_iri.clone(),
            revision: rv::map_iri(
                main.dataset_id(),
                r.name,
                previous.as_ref().map_or(1, |p| p.number + 1),
                control,
            )?,
            number: previous.as_ref().map_or(1, |p| p.number + 1),
            previous: previous.as_ref().map(|p| p.iri.clone()),
            decisions_added: adds.clone(),
            decisions_ended: ends.clone(),
            entities: moved,
        });
    } else {
        drop(evidence);
    }
    // The records.
    let mut batch = Batch {
        operation,
        operation_iri: operation_iri(operation),
        payload_digest: payload,
        principal: options.principal.clone(),
        idempotency_key: options.idempotency_key.clone(),
        graphs: vec![m.decisions.0.clone()],
        records: Vec::new(),
    };
    let mut decided = Vec::with_capacity(decisions.len());
    for (d, iri) in decisions.iter().zip(minted) {
        let record = rv::decision_record(
            main.dataset_id(),
            &options.principal,
            operation,
            r.name,
            r.version,
            d,
            None,
            &m.decisions.0,
            control,
        )?;
        debug_assert_eq!(record.iri, iri);
        batch.records.push(record);
        decided.push(Decided {
            key: d.key.clone(),
            iri,
            target: d.target.clone(),
            disposition: d.disposition,
            supersedes: d.supersedes.clone(),
            support: None,
            not_projected: None,
            conflicts: Vec::new(),
        });
    }
    let mut map_revision = None;
    if let Some(change) = &changed.map {
        let record = rv::map_record(
            main.dataset_id(),
            &MapRevision {
                policy: r.name,
                version: m.version,
                number: change.number,
                previous: change.previous.as_ref(),
                output: &m.output.0,
                adds: &adds,
                ends: &ends,
                nonces,
                boundary: &m.boundary_protections(),
            },
            &m.maps.0,
            control,
        )?;
        map_revision = Some(record.iri.clone());
        batch.records.push(record);
        batch.graphs.push(m.maps.0.clone());
        batch.graphs.sort();
        batch.graphs.dedup();
    }
    let skip = vec![false; batch.records.len()];
    check_identities(&mut state, &batch, &records, &skip)?;
    drop(state);
    let committed = publish(
        main, logical, capability, r.if_head, &batch, &skip, &changes, options, control,
    )?;
    Ok(Reviewed {
        operation: batch.operation_iri.clone(),
        committed: committed.committed,
        replayed: false,
        commit: committed.commit,
        branch,
        decisions: decided,
        changed,
        map_revision,
        preview: committed.preview,
    })
}

/// Refuse as `identity-conflict` when an effective separation names two entities of
/// the cluster `c`.
fn separate(clusters: &Clusters<'_>, c: &Cluster, key: &str) -> Result<()> {
    for p in &c.distincts {
        let Some((_, f)) = clusters.facts.get(p) else {
            continue;
        };
        if f.entities
            .iter()
            .filter(|e| c.entities.contains(*e))
            .count()
            > 1
        {
            return Err(managed(
                ManagedErrorKind::IdentityConflict,
                format!(
                    "{key}: an accepted separation keeps two entities of one cluster apart; reverse the match or the separation"
                ),
            ));
        }
    }
    Ok(())
}

fn resolves(e: &NamedNode, rep: &NamedNode) -> Triple {
    Triple::new(
        e.clone(),
        NamedNode::new_unchecked(RESOLVES_TO),
        rep.clone(),
    )
}

fn quad(t: &Triple, output: &NamedNode) -> Quad {
    Quad::new(
        t.subject.clone(),
        t.predicate.clone(),
        t.object.clone(),
        GraphName::NamedNode(output.clone()),
    )
}

// -------------------------------------------------------------------- reads -------

/// One entity of a resolution read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedEntity {
    /// The entity the request names.
    pub entity: NamedNode,
    /// The entity the map selects for it, the entity itself when the map does not
    /// name it.
    pub representative: NamedNode,
    /// The members of its cluster, sorted, at most [`MAX_MEMBERS`]. The entity alone
    /// when the map does not name it.
    pub members: Vec<NamedNode>,
    /// Whether `members` lists every member.
    pub members_complete: bool,
    /// The current accepted decisions that join the cluster, sorted.
    pub decisions: Vec<NamedNode>,
}

/// The outcome of [`resolve`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityResolution {
    pub policy: NamedNode,
    /// The latest revision of the map, `None` before the first.
    pub revision: Option<NamedNode>,
    /// Its number, 0 before the first.
    pub number: u64,
    /// The commit the map was read at.
    pub commit: u64,
    pub entities: Vec<ResolvedEntity>,
}

/// Read what the current resolution map of the matching policy `policy` selects for
/// each of `entities`, at the head of `logical`, which is `main` or one of its branches.
/// `access` is what the caller may read there, `None` for everything. The caller needs
/// read grants on the policy's decisions, maps and output graphs and on the identity
/// graphs of its boundary. Records it cannot read are not reported.
///
/// Refusals: invalid input, an unknown policy, a graph the caller may not read,
/// `projection-unavailable` when a protection of the policy's graphs changed, and
/// `incomplete-evidence` when a cluster's decisions exceed [`MAX_EVIDENCE`] records.
pub fn resolve(
    main: &Dataset,
    logical: &Dataset,
    policy: &str,
    entities: &[NamedNode],
    access: Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<EntityResolution> {
    resolve_bounded(
        main,
        logical,
        policy,
        entities,
        access,
        control,
        MAX_EVIDENCE,
    )
}

/// [`resolve`] with at most `decision_limit` decisions of one proposal read from the
/// caller's view, [`MAX_EVIDENCE`] outside tests.
fn resolve_bounded(
    main: &Dataset,
    logical: &Dataset,
    policy: &str,
    entities: &[NamedNode],
    access: Option<Arc<GraphAccess>>,
    control: &Control,
    decision_limit: usize,
) -> Result<EntityResolution> {
    control.check()?;
    check_store(main, logical)?;
    if entities.is_empty() || entities.len() > MAX_RESOLVE {
        return Err(Error::invalid(format!(
            "entities: a resolution names 1 to {MAX_RESOLVE} entities"
        )));
    }
    let (settings, _) = settings_of(main)?;
    let registry = main.store().managed_snapshot()?;
    let nonce = |g: &str| -> Option<Uuid> {
        registry.as_ref().and_then(|s| {
            s.graphs
                .owners
                .get(&crate::managed::OwnerKind::Evidence)
                .filter(|e| e.graphs.iter().any(|x| x == g))
                .and_then(|e| e.protections.get(g).copied())
        })
    };
    let head = logical.store().snapshot();
    resolve_in(
        &settings,
        &nonce,
        main.dataset_id(),
        &head,
        policy,
        entities,
        access,
        control,
        decision_limit,
    )
}

/// What the resolution map of the matching policy `policy` selects for each of
/// `entities` in an evidence view, with the policy, its graphs and their protections
/// as the view's configuration records them. Refusals are those of [`resolve`].
pub fn resolve_at(
    view: &sparkles_core::store::BoundEvidenceView,
    policy: &str,
    entities: &[NamedNode],
    access: Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<EntityResolution> {
    control.check()?;
    let settings = super::claims::view_settings(view)?;
    let owner = view.owner(crate::managed::OwnerKind::Evidence);
    let nonce = |g: &str| -> Option<Uuid> {
        owner
            .filter(|e| e.graphs.iter().any(|x| x == g))
            .and_then(|e| e.protections.get(g).copied())
    };
    resolve_in(
        &settings,
        &nonce,
        view.dataset_id(),
        view.shared_snapshot(),
        policy,
        entities,
        access,
        control,
        MAX_EVIDENCE,
    )
}

/// The graphs a caller must be able to read to resolve entities under `policy`: its
/// decisions, maps and output graphs and the identity graphs of its boundary.
pub(super) fn required_graphs(settings: &Settings, policy: &MatchingPolicy) -> Vec<String> {
    let mut out = vec![
        policy.decisions.clone(),
        policy.maps.clone(),
        policy.output.clone(),
    ];
    out.extend(
        policy
            .boundary
            .iter()
            .filter(|b| settings.identity.iter().any(|i| i == *b))
            .cloned(),
    );
    out
}

#[allow(clippy::too_many_arguments)]
fn resolve_in(
    settings: &Settings,
    nonce: &dyn Fn(&str) -> Option<Uuid>,
    dataset_id: Uuid,
    head: &Arc<crate::store::Snapshot>,
    policy: &str,
    entities: &[NamedNode],
    access: Option<Arc<GraphAccess>>,
    control: &Control,
    decision_limit: usize,
) -> Result<EntityResolution> {
    if entities.is_empty() || entities.len() > MAX_RESOLVE {
        return Err(Error::invalid(format!(
            "entities: a resolution names 1 to {MAX_RESOLVE} entities"
        )));
    }
    let Some(p) = settings.matching_policies.get(policy) else {
        return Err(Error::invalid(format!(
            "policy: the evidence settings name no matching policy {policy}"
        )));
    };
    let m = MapGraphs::new(policy, p, settings, nonce)?;
    let all = Some(Arc::new(GraphAccess::all()));
    let access = access.or(all);
    for (g, _) in [&m.decisions, &m.maps, &m.output]
        .into_iter()
        .chain(m.proposals.iter())
    {
        readable(access.as_ref(), g)?;
    }
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    let view = caller_view(access.as_ref(), head, &preparation)?;
    // Records are read at the unmasked head with the caller's view beside it. The map's
    // consistency is checked there, so that a mask cannot hide the metadata that would
    // refuse it, and a revision the caller's view masks is refused rather than read. A
    // proposal or decision the view hides any triple of takes no part in a cluster, and
    // the output graph is read in the caller's view.
    let mut state = State {
        reads: Reads::new(&preparation, head),
        seen: view
            .as_ref()
            .map(|v| Reads::new(&preparation, v.snapshot())),
        claim_graphs: Vec::new(),
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    let latest = latest(&mut state, dataset_id, &m, control)?;
    if let Some(l) = &latest
        && state.masked(&l.iri, &m.maps.0)?
    {
        return Err(projection_unavailable(&m.name));
    }
    let mut clusters = Clusters::new(&m, BTreeMap::new());
    clusters.hide_masked = true;
    clusters.decision_limit = decision_limit;
    // The decisions of each component the caller's view traverses, kept for each of its
    // entities, so that a request answers each entity as a request of it alone does.
    let mut joined: BTreeMap<NamedNode, Arc<Vec<NamedNode>>> = BTreeMap::new();
    let mut out = Vec::with_capacity(entities.len());
    for e in entities {
        control.check()?;
        let seen = match state.seen.as_mut() {
            Some(seen) => seen,
            None => &mut state.reads,
        };
        let representative = match seen.single(e.as_str(), RESOLVES_TO, m.output.0.as_str())? {
            Some(Term::NamedNode(r)) => r,
            _ => e.clone(),
        };
        let (members, complete) = if representative == *e
            && !seen.contains(
                e.as_str(),
                RESOLVES_TO,
                &Term::NamedNode(e.clone()),
                m.output.0.as_str(),
            )? {
            (vec![e.clone()], true)
        } else {
            let (mut v, more) = seen.subjects_up_to(
                RESOLVES_TO,
                &Term::NamedNode(representative.clone()),
                m.output.0.as_str(),
                MAX_MEMBERS,
            )?;
            v.sort();
            (v, !more)
        };
        let mut decisions = Vec::new();
        if members.len() > 1 {
            if let Some(d) = joined.get(e) {
                decisions = d.as_ref().clone();
            } else {
                let c = clusters.cluster(&mut state, e, When::Before, &mut |_, _, _| Ok(()))?;
                let mut found = BTreeSet::new();
                for p in &c.matches {
                    let current = clusters.visible_current(&mut state, p)?;
                    found.extend(current);
                }
                decisions = found.into_iter().collect();
                let shared = Arc::new(decisions.clone());
                for x in c.entities {
                    joined.insert(x, shared.clone());
                }
            }
        }
        out.push(ResolvedEntity {
            entity: e.clone(),
            representative,
            members,
            members_complete: complete,
            decisions,
        });
    }
    Ok(EntityResolution {
        policy: m.policy_iri.clone(),
        revision: latest.as_ref().map(|l| l.iri.clone()),
        number: latest.as_ref().map_or(0, |l| l.number),
        commit: head.commit,
        entities: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
    use crate::access::{GraphRule, Graphs};
    use crate::evidence::{
        EvidenceInput, IdentityInput, MethodRef, ProposeOptions, RecordKey, RecordVersion,
    };

    const IDENTITY: &str = "urn:ev:identity";
    const HIDDEN: &str = "urn:ev:identity-restricted";

    fn n(s: &str) -> NamedNode {
        NamedNode::new_unchecked(s)
    }

    fn configured() -> Dataset {
        let ds = Dataset::memory();
        let settings = Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            decisions: vec!["urn:ev:decisions".into()],
            identity: vec![IDENTITY.into(), HIDDEN.into(), "urn:ev:maps".into()],
            projections: vec!["urn:ev:map".into()],
            matching_policies: [(
                "people".to_string(),
                MatchingPolicy {
                    version: 1,
                    boundary: vec![IDENTITY.into(), HIDDEN.into()],
                    decisions: "urn:ev:decisions".into(),
                    maps: "urn:ev:maps".into(),
                    output: "urn:ev:map".into(),
                },
            )]
            .into_iter()
            .collect(),
            ..Settings::default()
        };
        crate::evidence::configure(&ds, &settings, &Control::none()).unwrap();
        ds
    }

    fn propose(
        ds: &Dataset,
        graph: &str,
        occurrence: &str,
        entities: &[&str],
    ) -> (NamedNode, sparkles_core::evidence::Sha256Digest) {
        let mut o = ProposeOptions::new(n("urn:principal:matcher"));
        o.identity_graph = Some(n(graph));
        let p = crate::evidence::propose(
            None,
            ds,
            ds,
            &EvidenceInput {
                identities: vec![IdentityInput {
                    version: RecordVersion::V1,
                    key: RecordKey::new("p").unwrap(),
                    occurrence_key: RecordKey::new(occurrence).unwrap(),
                    entities: entities.iter().map(|e| n(e)).collect(),
                    relation: IdentityRelation::PossibleMatch,
                    preferred: None,
                    attributed_author: n("urn:matcher"),
                    method: Some(MethodRef {
                        method: n("urn:method"),
                        version: RecordKey::new("1").unwrap(),
                    }),
                }],
                ..Default::default()
            },
            &o,
            &mut |_, _| Some(Arc::new(GraphAccess::all())),
            &Control::none(),
        )
        .unwrap();
        (p.records[0].iri.clone(), p.records[0].digest)
    }

    /// The caller's view hides which entities the restricted graph's proposals name.
    fn masked() -> Option<Arc<GraphAccess>> {
        Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "entities".into(),
                        predicates: Some(vec![ev("entity")]),
                        classes: None,
                        subclasses: false,
                        graphs: Some(Graphs::Only(GraphRule::new([HIDDEN], &[]))),
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        )))
    }

    /// A caller whose view hides `predicate` in `graph`.
    fn hiding(predicate: &str, graph: &str) -> Option<Arc<GraphAccess>> {
        Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "hidden".into(),
                        predicates: Some(vec![predicate.to_string()]),
                        classes: None,
                        subclasses: false,
                        graphs: Some(Graphs::Only(GraphRule::new([graph], &[]))),
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        )))
    }

    /// A decision about `target` under the matching policy.
    fn decide(
        ds: &Dataset,
        target: &(NamedNode, sparkles_core::evidence::Sha256Digest),
        disposition: Disposition,
        supersedes: Option<&NamedNode>,
    ) -> NamedNode {
        let (settings, bytes) = settings_of(ds).unwrap();
        let mut options = Options::new(n("urn:principal:reviewer"));
        options.if_head = Some(ds.store().head_commit().seq);
        review(
            None,
            ds,
            ds,
            Request {
                name: "people",
                policy: settings.matching_policies.get("people").unwrap(),
                settings: &settings,
                settings_bytes: &bytes,
                version: 1,
                if_head: ds.store().head_commit().seq,
                candidates: MAX_EVIDENCE,
            },
            &[DecisionInput {
                key: "x".into(),
                target: target.0.clone(),
                target_digest: target.1,
                disposition,
                supersedes: supersedes.cloned(),
            }],
            &options,
            &mut |_, _| Some(Arc::new(GraphAccess::all())),
            &Control::none(),
        )
        .unwrap()
        .decisions[0]
            .iri
            .clone()
    }

    /// More decisions of a proposal than the read takes, all with their targets hidden
    /// from the caller, are not counted: the read answers without them instead of
    /// refusing with a bound that counts hidden triples. The same store read in full
    /// reaches the bound. A lookup that enumerated decisions at the unmasked head would
    /// refuse in both cases.
    #[test]
    fn hidden_decisions_beyond_the_bound_are_not_counted() {
        let ds = configured();
        let ab = propose(&ds, IDENTITY, "ab", &["urn:a", "urn:b"]);
        let first = decide(&ds, &ab, Disposition::Accepted, None);
        let second = decide(&ds, &ab, Disposition::Rejected, Some(&first));
        decide(&ds, &ab, Disposition::Accepted, Some(&second));
        let read = |access: Option<Arc<GraphAccess>>| {
            resolve_bounded(
                &ds,
                &ds,
                "people",
                &[n("urn:b")],
                access,
                &Control::none(),
                2,
            )
        };
        let hidden = read(hiding(&ev("target"), "urn:ev:decisions")).unwrap();
        assert!(hidden.entities[0].decisions.is_empty(), "{hidden:?}");
        assert_eq!(hidden.entities[0].members.len(), 2);
        assert_eq!(
            read(Some(Arc::new(GraphAccess::all()))).unwrap_err().code(),
            "incomplete-evidence"
        );
    }

    /// The answer of an acceptance of `target` under a bound of `candidates`.
    fn answer(
        ds: &Dataset,
        target: &(NamedNode, sparkles_core::evidence::Sha256Digest),
        candidates: usize,
        access: Option<Arc<GraphAccess>>,
    ) -> Result<Reviewed> {
        let (settings, bytes) = settings_of(ds)?;
        let mut options = Options::new(n("urn:principal:reviewer"));
        options.if_head = Some(ds.store().head_commit().seq);
        review(
            None,
            ds,
            ds,
            Request {
                name: "people",
                policy: settings.matching_policies.get("people").unwrap(),
                settings: &settings,
                settings_bytes: &bytes,
                version: 1,
                if_head: ds.store().head_commit().seq,
                candidates,
            },
            &[DecisionInput {
                key: "x".into(),
                target: target.0.clone(),
                target_digest: target.1,
                disposition: Disposition::Accepted,
                supersedes: None,
            }],
            &options,
            &mut |_, _| access.clone(),
            &Control::none(),
        )
    }

    /// More proposals naming an entity than the bound, hidden from the caller, answer
    /// exactly as an unavailable target, and the same store read in full answers
    /// `incomplete-evidence`. Without the recount in the caller's view the masked review
    /// would answer `incomplete-evidence` too and disclose the hidden count.
    #[test]
    fn hidden_candidates_beyond_the_bound_answer_as_an_unavailable_target() {
        let ds = configured();
        for i in 0..3 {
            propose(
                &ds,
                HIDDEN,
                &format!("h{i}"),
                &["urn:b", &format!("urn:h{i}")],
            );
        }
        let bc = propose(&ds, IDENTITY, "bc", &["urn:b", "urn:c"]);
        let missing = (n("urn:uuid:00000000-0000-4000-8000-000000000000"), bc.1);
        let pair = |r: Result<Reviewed>| {
            let e = r.unwrap_err();
            (e.code().to_string(), e.to_string())
        };
        let hidden = pair(answer(&ds, &bc, 2, masked()));
        assert_eq!(hidden.0, "evidence-unavailable", "{hidden:?}");
        assert_eq!(hidden, pair(answer(&ds, &missing, 2, masked())));
        let all = Some(Arc::new(GraphAccess::all()));
        assert_eq!(
            pair(answer(&ds, &bc, 2, all.clone())).0,
            "incomplete-evidence"
        );
        // within the bound the masked proposals have no effect and are left out
        assert!(answer(&ds, &bc, 10, masked()).unwrap().committed);
    }

    #[test]
    fn retained_identifiers_are_bounded() {
        let g = |s: &str| (NamedNode::new_unchecked(s), None);
        let m = MapGraphs {
            name: "people".into(),
            policy_iri: rv::policy_iri("people"),
            version: 1,
            decisions: g("urn:d"),
            maps: g("urn:m"),
            output: g("urn:o"),
            boundary: Vec::new(),
            proposals: Vec::new(),
        };
        let mut c = Clusters::new(&m, BTreeMap::new());
        assert_eq!(c.limit, RETAINED_BYTES);
        c.limit = 100;
        c.retain(60).unwrap();
        c.retain(40).unwrap();
        let e = c.retain(1).unwrap_err();
        assert_eq!(e.code(), "incomplete-evidence");
        // the targets' facts are charged to the same allowance before the clusters
        let mut bytes = 0;
        retain(&mut bytes, 10, 10).unwrap();
        assert_eq!(
            retain(&mut bytes, 10, 1).unwrap_err().code(),
            "incomplete-evidence"
        );
        let f = Facts {
            relation: Some(IdentityRelation::PossibleMatch),
            entities: vec![
                NamedNode::new_unchecked("urn:a"),
                NamedNode::new_unchecked("urn:bb"),
            ],
            preferred: Some(NamedNode::new_unchecked("urn:a")),
        };
        assert_eq!(fact_bytes(&f), 5 + 6 + 5);
    }
}
