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
    self as rv, Batch, DecisionInput, Disposition, MAP_NONCE_PREDICATES, MAX_DECISIONS,
    MAX_EVIDENCE, MapNonces, MapRevision,
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
}

impl Clusters<'_> {
    fn charge(&mut self, when: When) -> Result<()> {
        let i = usize::from(when == When::After);
        self.visits[i] += 1;
        if self.visits[i] > MAX_EVIDENCE {
            return Err(incomplete("the entities and proposals of the clusters"));
        }
        Ok(())
    }

    /// The authenticated proposals that name `e`, with their graphs.
    fn naming(
        &mut self,
        state: &mut State<'_>,
        e: &NamedNode,
        when: When,
    ) -> Result<Vec<NamedNode>> {
        let mut out = Vec::new();
        for g in self.m.proposals.clone() {
            let (candidates, more) =
                state.candidates(&ev("entity"), &Term::NamedNode(e.clone()), &g)?;
            if more {
                return Err(incomplete("the proposals of an entity"));
            }
            for p in candidates {
                self.charge(when)?;
                if !self.facts.contains_key(&p) {
                    if !state.typed(&p, &g, RecordKind::IdentityProposal)? {
                        continue;
                    }
                    let f = facts(state, &p, &g)?;
                    self.facts.insert(p.clone(), (g.clone(), f));
                }
                out.push(p);
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
                let current = state.current(p, &self.m.policy_iri, &self.m.decisions);
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
            for p in self.naming(state, &e, when)? {
                if c.matches.contains(&p) || c.distincts.contains(&p) {
                    continue;
                }
                let Some((g, f)) = self.facts.get(&p).cloned() else {
                    continue;
                };
                let Some(relation) = f.relation else {
                    continue;
                };
                if !self.is_effective(state, &p, when)? {
                    continue;
                }
                used(state, &p, &g)?;
                match relation {
                    IdentityRelation::PossibleMatch => {
                        c.matches.insert(p);
                        c.preferred.extend(f.preferred.clone());
                        for x in f.entities {
                            if c.entities.insert(x.clone()) {
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
    let mut clusters = Clusters {
        m: &m,
        batch: decisions
            .iter()
            .map(|d| (d.target.clone(), d.disposition))
            .collect(),
        facts: BTreeMap::new(),
        effective: BTreeMap::new(),
        visits: [0, 0],
    };
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
    control.check()?;
    check_store(main, logical)?;
    if entities.is_empty() || entities.len() > MAX_RESOLVE {
        return Err(Error::invalid(format!(
            "entities: a resolution names 1 to {MAX_RESOLVE} entities"
        )));
    }
    let (settings, _) = settings_of(main)?;
    let Some(p) = settings.matching_policies.get(policy) else {
        return Err(Error::invalid(format!(
            "policy: the evidence settings name no matching policy {policy}"
        )));
    };
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
    let m = MapGraphs::new(policy, p, &settings, &nonce)?;
    let all = Some(Arc::new(GraphAccess::all()));
    let access = access.or(all);
    for (g, _) in [&m.decisions, &m.maps, &m.output]
        .into_iter()
        .chain(m.proposals.iter())
    {
        readable(access.as_ref(), g)?;
    }
    let head = logical.store().snapshot();
    let preparation = crate::access::Preparation::new(control.clone(), READS_BYTES)?;
    let view = caller_view(access.as_ref(), &head, &preparation)?;
    let snap = view.as_ref().map_or(&*head, |v| v.snapshot());
    let mut state = State {
        reads: Reads::new(&preparation, snap),
        seen: None,
        claim_graphs: Vec::new(),
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    let latest = latest(&mut state, main.dataset_id(), &m, control)?;
    let mut clusters = Clusters {
        m: &m,
        batch: BTreeMap::new(),
        facts: BTreeMap::new(),
        effective: BTreeMap::new(),
        visits: [0, 0],
    };
    let mut out = Vec::with_capacity(entities.len());
    for e in entities {
        control.check()?;
        let representative =
            match state
                .reads
                .single(e.as_str(), RESOLVES_TO, m.output.0.as_str())?
            {
                Some(Term::NamedNode(r)) => r,
                _ => e.clone(),
            };
        let (members, complete) = if representative == *e
            && !state.reads.contains(
                e.as_str(),
                RESOLVES_TO,
                &Term::NamedNode(e.clone()),
                m.output.0.as_str(),
            )? {
            (vec![e.clone()], true)
        } else {
            let (mut v, more) = state.reads.subjects_up_to(
                RESOLVES_TO,
                &Term::NamedNode(representative.clone()),
                m.output.0.as_str(),
                MAX_MEMBERS,
            )?;
            v.sort();
            (v, !more)
        };
        let mut decisions = BTreeSet::new();
        if members.len() > 1 {
            let c = clusters.cluster(&mut state, e, When::Before, &mut |_, _, _| Ok(()))?;
            for p in &c.matches {
                let current = state.current(p, &m.policy_iri, &m.decisions)?;
                decisions.extend(current);
            }
        }
        out.push(ResolvedEntity {
            entity: e.clone(),
            representative,
            members,
            members_complete: complete,
            decisions: decisions.into_iter().collect(),
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
