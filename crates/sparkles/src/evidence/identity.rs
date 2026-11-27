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
    branch_name, caller_view, caller_view_evidence, capability, check_identities, check_store,
    claim_graphs, hidden, incomplete, operation, publish, readable, record_graph_nonces, replayed,
    writable,
};
use crate::access::GraphAccess;
use crate::evidence::propose::{READS_BYTES, managed, settings_of};
use crate::evidence::resolve::{Resolution, unavailable};
use crate::managed::ManagedErrorKind;
use crate::task::Control;
use crate::{Catalog, Dataset, Error, Result};
use oxrdf::vocab::xsd;
use oxrdf::{GraphName, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use sparkles_core::evidence::review::{
    self as rv, BOUNDARY_PROTECTION, Batch, DecisionInput, Disposition, MAP_NONCE_PREDICATES,
    MAX_DECISIONS, MAX_EVIDENCE, MapNonces, MapRevision,
};
use sparkles_core::evidence::write::{ExistingRecord, Reads, operation_iri};
use sparkles_core::evidence::{IdentityRelation, MatchingPolicy, RecordKind, Settings};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::Arc;
use uuid::Uuid;

const PROV: &str = "http://www.w3.org/ns/prov#";
/// The most existing triples of a policy's output graph that [`rebuild`] reads.
const MAX_OUTPUT_TRIPLES: usize = 1_000_000;

fn ev(local: &str) -> String {
    super::ev(local)
}

fn map_unavailable(policy: &str) -> Error {
    managed(
        ManagedErrorKind::ProjectionUnavailable,
        format!(
            "policy {policy}: its resolution map is unavailable because a graph protection or boundary changed or the latest revision cannot be verified; the resolution map must be rebuilt first"
        ),
    )
}

fn parse_map_revision_literal(term: &Term) -> Option<u64> {
    match term {
        Term::Literal(l) if l.datatype() == xsd::INTEGER => {
            l.value().parse::<u64>().ok().filter(|&n| n >= 1)
        }
        _ => None,
    }
}

fn map_revision_number(state: &mut State<'_>, s: &NamedNode, g: &G) -> Result<Option<u64>> {
    Ok(state
        .reads
        .single(s.as_str(), &ev("mapRevision"), g.0.as_str())?
        .as_ref()
        .and_then(parse_map_revision_literal))
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

/// The latest revision present in `m.maps`, found by galloping and binary search over the
/// minted revision identities, together with its number and whether it authenticates under
/// the current `maps` protection nonce.
fn latest_present(
    state: &mut State<'_>,
    dataset: Uuid,
    m: &MapGraphs,
    control: &Control,
) -> Result<Option<(NamedNode, u64, bool)>> {
    let probe = |n: u64, state: &mut State<'_>| -> Result<Option<(NamedNode, bool)>> {
        let iri = rv::map_iri(dataset, &m.name, n, control)?;
        match state.reads.record(&iri, std::slice::from_ref(&m.maps))? {
            ExistingRecord::Absent => Ok(None),
            ExistingRecord::Present { authenticated, .. } => Ok(Some((iri, authenticated))),
            ExistingRecord::Many => Err(map_unavailable(&m.name)),
        }
    };
    let Some((first, first_auth)) = probe(1, state)? else {
        return Ok(None);
    };
    // the largest present revision lies in [lo, hi)
    let (mut lo, mut lo_iri, mut lo_auth) = (1u64, first, first_auth);
    let mut hi = 2u64;
    loop {
        control.check()?;
        match probe(hi, state)? {
            Some((iri, auth)) => {
                lo = hi;
                lo_iri = iri;
                lo_auth = auth;
                hi = hi.checked_mul(2).ok_or_else(|| map_unavailable(&m.name))?;
            }
            None => break,
        }
    }
    while hi - lo > 1 {
        control.check()?;
        let mid = lo + (hi - lo) / 2;
        match probe(mid, state)? {
            Some((iri, auth)) => {
                lo = mid;
                lo_iri = iri;
                lo_auth = auth;
            }
            None => hi = mid,
        }
    }
    Ok(Some((lo_iri, lo, lo_auth)))
}

/// The latest revision of `m`'s map, found by galloping and binary search over the
/// minted revision identities, which a typed writer publishes in order. Refuses as
/// `projection-unavailable` when the latest revision in the maps graph does not
/// authenticate or was written under other protections than the current ones, and
/// when the output holds triples while no revision exists.
fn latest(
    state: &mut State<'_>,
    dataset: Uuid,
    m: &MapGraphs,
    masked_view: bool,
    control: &Control,
) -> Result<Option<Latest>> {
    let Some((lo_iri, lo, lo_auth)) = latest_present(state, dataset, m, control)? else {
        if state.reads.graph_nonempty(m.output.0.as_str())? {
            return Err(map_unavailable(&m.name));
        }
        return Ok(None);
    };
    if !lo_auth || !state.typed(&lo_iri, &m.maps, RecordKind::ResolutionMap)? {
        return Err(map_unavailable(&m.name));
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
        return Err(map_unavailable(&m.name));
    }
    // Every identity graph the map was built from must still be in the boundary under
    // the same protection, or matches the map follows would no longer be read. A graph
    // added to the boundary after the latest revision is accepted for an unmasked caller
    // only when it holds no authenticated proposal with a current accepted decision under
    // this policy (re-adding a previously removed graph with live accepted decisions
    // requires a rebuild). A caller whose view masks triples receives
    // `projection-unavailable` whenever the boundary contains an added graph not yet
    // recorded on the latest revision, so the status depends only on caller-visible
    // boundary metadata and never discloses whether a masked acceptance exists.
    let current_entries: Vec<(G, String)> = m
        .proposals
        .iter()
        .filter_map(|(g, n)| n.map(|n| ((g.clone(), Some(n)), rv::boundary_protection(g, n))))
        .collect();
    let current: BTreeSet<&str> = current_entries.iter().map(|(_, s)| s.as_str()).collect();
    let (built, more) = state.reads.objects_up_to(
        lo_iri.as_str(),
        BOUNDARY_PROTECTION,
        m.maps.0.as_str(),
        MAX_BOUNDARY,
    )?;
    if more {
        return Err(map_unavailable(&m.name));
    }
    let mut built_set: BTreeSet<String> = BTreeSet::new();
    for t in built {
        match t {
            Term::Literal(l) if current.contains(l.value()) => {
                built_set.insert(l.value().to_string());
            }
            _ => return Err(map_unavailable(&m.name)),
        }
    }
    let added: Vec<G> = current_entries
        .into_iter()
        .filter(|(_, entry)| !built_set.contains(entry))
        .map(|(g, _)| g)
        .collect();
    if !added.is_empty()
        && (masked_view
            || state
                .reads
                .has_live_accepted_in_graphs(&added, &m.decisions, &m.policy_iri)?)
    {
        return Err(map_unavailable(&m.name));
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
    let masked;
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
            used: None,
            graphs: Vec::new(),
            records: Vec::new(),
        };
        let found = state.reads.operation(&probe.operation_iri, &records)?;
        if let Some((commit, recorded_branch)) = replayed(found, &probe)? {
            let mut out = Vec::with_capacity(decisions.len());
            for d in decisions {
                let iri = decided_iri(d)?;
                match state.reads.record(&iri, &records)? {
                    sparkles_core::evidence::write::ExistingRecord::Present {
                        graph,
                        authenticated: true,
                        ..
                    } if graph == m.decisions.0
                        && state.typed(&iri, &m.decisions, RecordKind::ReviewDecision)? => {}
                    _ => {
                        return Err(managed(
                            ManagedErrorKind::ImmutableRecordConflict,
                            format!(
                                "{}: decision <{}> from replayed operation <{}> is not authenticated",
                                d.key,
                                iri.as_str(),
                                probe.operation_iri.as_str()
                            ),
                        ));
                    }
                }
                out.push(Decided {
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
            // the revision the first answer published, recorded with its receipt
            let (published, _) = state.reads.subjects_up_to(
                &ev("availabilityReceipt"),
                &Term::NamedNode(probe.operation_iri.clone()),
                m.maps.0.as_str(),
                MAX_DECISIONS + 2,
            )?;
            let map_class = Term::NamedNode(NamedNode::new_unchecked(
                sparkles_core::evidence::resolve::kind_iri(RecordKind::ResolutionMap),
            ));
            let mut map_revision = None;
            for s in published {
                if state.reads.contains(
                    s.as_str(),
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                    &map_class,
                    m.maps.0.as_str(),
                )? {
                    if !state
                        .reads
                        .authenticated(s.as_str(), m.maps.0.as_str(), m.maps.1)?
                    {
                        return Err(managed(
                            ManagedErrorKind::ImmutableRecordConflict,
                            format!(
                                "map revision <{}> from replayed operation <{}> is not authenticated",
                                s.as_str(),
                                probe.operation_iri.as_str()
                            ),
                        ));
                    }
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
    if let Some(a) = access.as_deref() {
        a.validate_bound_snapshot(&head)?;
    }
    let hides_triples = access.as_deref().is_some_and(GraphAccess::hides_triples);
    let previous = latest(&mut state, main.dataset_id(), &m, hides_triples, control)?;
    masked = caller_view(access.as_ref(), &head, &mask_preparation)?;
    state.seen = masked
        .as_ref()
        .map(|v| Reads::from_prepared(&mask_preparation, v));
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
        used: None,
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
        main,
        logical,
        capability,
        r.if_head,
        None,
        &batch,
        &skip,
        &[],
        &changes,
        options,
        control,
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

// ------------------------------------------------------------------ rebuild -------

/// The outcome of [`rebuild`].
#[derive(Debug)]
pub struct RebuiltMap {
    pub operation: NamedNode,
    pub committed: bool,
    pub replayed: bool,
    pub commit: u64,
    pub branch: Uuid,
    pub policy: NamedNode,
    pub policy_version: u32,
    /// The resolution map revision the rebuild published, also for a replay.
    pub revision: NamedNode,
    /// Its number, from 1.
    pub number: u64,
    /// The previous authenticated revision it follows, `None` when none existed or when
    /// earlier revisions in the maps graph no longer authenticate.
    pub previous: Option<NamedNode>,
    /// The protected identity graphs of the boundary recorded on the revision, sorted.
    pub boundary: Vec<NamedNode>,
    /// What changed. A replay reports nothing here.
    pub changed: Changed,
    pub preview: Option<Box<crate::preview::Preview>>,
}

/// Refuse with `incomplete-evidence` naming the exact record or entry bound `limit`.
fn incomplete_limit(what: &str, limit: usize) -> Error {
    managed(
        ManagedErrorKind::IncompleteEvidence,
        format!("{what} exceed {limit} records, so nothing was written"),
    )
}

/// Map local preparation memory exhaustion to `incomplete-evidence` (HTTP 422), while
/// preserving server-pool exhaustion, cancellation, timeout and all other errors.
fn map_local_budget<T>(result: Result<T>) -> Result<T> {
    match result {
        Err(Error::BudgetExceeded(b)) if b.kind == sparkles_core::BudgetKind::Memory => {
            Err(managed(
                ManagedErrorKind::IncompleteEvidence,
                format!(
                    "the rebuild exceeded its memory allowance of {} bytes, so nothing was written",
                    b.limit
                ),
            ))
        }
        other => other,
    }
}

/// Zero-allocation deterministic ordering over `oxrdf::Triple` values.
fn cmp_triple(a: &Triple, b: &Triple) -> std::cmp::Ordering {
    fn cmp_subject(a: &NamedOrBlankNode, b: &NamedOrBlankNode) -> std::cmp::Ordering {
        match (a, b) {
            (NamedOrBlankNode::NamedNode(x), NamedOrBlankNode::NamedNode(y)) => {
                x.as_str().cmp(y.as_str())
            }
            (NamedOrBlankNode::BlankNode(x), NamedOrBlankNode::BlankNode(y)) => {
                x.as_str().cmp(y.as_str())
            }
            (NamedOrBlankNode::NamedNode(_), NamedOrBlankNode::BlankNode(_)) => {
                std::cmp::Ordering::Less
            }
            (NamedOrBlankNode::BlankNode(_), NamedOrBlankNode::NamedNode(_)) => {
                std::cmp::Ordering::Greater
            }
        }
    }
    fn cmp_term(a: &Term, b: &Term) -> std::cmp::Ordering {
        fn rank(t: &Term) -> u8 {
            match t {
                Term::NamedNode(_) => 0,
                Term::BlankNode(_) => 1,
                Term::Literal(_) => 2,
                Term::Triple(_) => 3,
            }
        }
        match (a, b) {
            (Term::NamedNode(x), Term::NamedNode(y)) => x.as_str().cmp(y.as_str()),
            (Term::BlankNode(x), Term::BlankNode(y)) => x.as_str().cmp(y.as_str()),
            (Term::Literal(x), Term::Literal(y)) => x
                .value()
                .cmp(y.value())
                .then_with(|| x.datatype().as_str().cmp(y.datatype().as_str()))
                .then_with(|| x.language().cmp(&y.language())),
            (Term::Triple(x), Term::Triple(y)) => cmp_triple(x, y),
            _ => rank(a).cmp(&rank(b)),
        }
    }
    cmp_subject(&a.subject, &b.subject)
        .then_with(|| a.predicate.as_str().cmp(b.predicate.as_str()))
        .then_with(|| cmp_term(&a.object, &b.object))
}

/// Verify that `m.maps` has no numbering gap in `1..=max` when recovering from an absent
/// or unauthenticated/untyped head, and that no minted slot `latest_present` will probe
/// after publishing `next_number` holds a stray or partial subject.
fn verify_maps_for_rebuild(
    state: &mut State<'_>,
    dataset: Uuid,
    m: &MapGraphs,
    prev_number: u64,
    prev_auth_typed: bool,
    next_number: u64,
    control: &Control,
) -> Result<()> {
    if !prev_auth_typed {
        let policy_term = Term::NamedNode(m.policy_iri.clone());
        let (revs, more) = state.reads.subjects_up_to(
            &ev("reviewPolicy"),
            &policy_term,
            m.maps.0.as_str(),
            MAX_EVIDENCE,
        )?;
        if more {
            return Err(incomplete("resolution map revisions"));
        }
        let class = Term::NamedNode(NamedNode::new_unchecked(ev("ResolutionMap")));
        let mut max_rev = prev_number;
        let mut revision_subjects = Vec::new();
        for r in revs {
            control.check()?;
            let is_map_type =
                state
                    .reads
                    .contains(r.as_str(), super::RDF_TYPE, &class, m.maps.0.as_str())?;
            let (rev_objs, rev_more) =
                state
                    .reads
                    .objects_up_to(r.as_str(), &ev("mapRevision"), m.maps.0.as_str(), 2)?;
            if !is_map_type && rev_objs.is_empty() {
                continue;
            }
            if rev_more || rev_objs.len() != 1 {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "policy {}: maps graph holds an invalid revision number on <{}>",
                        m.name,
                        r.as_str()
                    ),
                ));
            }
            let Some(n) = parse_map_revision_literal(&rev_objs[0]) else {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "policy {}: maps graph holds an invalid revision number on <{}>",
                        m.name,
                        r.as_str()
                    ),
                ));
            };
            max_rev = max_rev.max(n);
            revision_subjects.push(r);
        }
        if max_rev > MAX_EVIDENCE as u64 {
            // `!more` means at most MAX_EVIDENCE subjects exist, so 1..=max_rev has a gap.
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "policy {}: maps graph holds revision {max_rev} past a numbering gap",
                    m.name
                ),
            ));
        }
        let mut canonical_iris = HashSet::with_capacity(max_rev as usize);
        for k in 1..=max_rev {
            control.check()?;
            let iri = rv::map_iri(dataset, &m.name, k, control)?;
            match state.reads.record(&iri, std::slice::from_ref(&m.maps))? {
                ExistingRecord::Present { .. } => {
                    let has_type = state.reads.contains(
                        iri.as_str(),
                        super::RDF_TYPE,
                        &class,
                        m.maps.0.as_str(),
                    )?;
                    let has_policy = state.iri(&iri, &ev("reviewPolicy"), &m.maps)?.as_ref()
                        == Some(&m.policy_iri);
                    let has_number = map_revision_number(state, &iri, &m.maps)? == Some(k);
                    if !has_type || !has_policy || !has_number {
                        return Err(managed(
                            ManagedErrorKind::ImmutableRecordConflict,
                            format!(
                                "policy {}: maps graph has a missing or malformed revision record at slot {k} before revision {max_rev}",
                                m.name
                            ),
                        ));
                    }
                    canonical_iris.insert(iri);
                }
                ExistingRecord::Absent => {
                    return Err(managed(
                        ManagedErrorKind::ImmutableRecordConflict,
                        format!(
                            "policy {}: maps graph has a numbering gap at revision {k} before revision {max_rev}",
                            m.name
                        ),
                    ));
                }
                ExistingRecord::Many => {
                    return Err(managed(
                        ManagedErrorKind::ImmutableRecordConflict,
                        format!(
                            "policy {}: maps graph has conflicting records at revision {k}",
                            m.name
                        ),
                    ));
                }
            }
        }
        for r in revision_subjects {
            if !canonical_iris.contains(&r) {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "policy {}: maps graph holds revision record <{}> outside contiguous range 1..={max_rev}",
                        m.name,
                        r.as_str()
                    ),
                ));
            }
        }
    }
    // Check `next_number + 1` and every minted slot `latest_present` will probe after
    // writing `next_number`, so no stray or partial subject can break `latest()`.
    let mut probe_slots: BTreeSet<u64> = BTreeSet::new();
    if let Some(after_next) = next_number.checked_add(1) {
        probe_slots.insert(after_next);
        let mut hi = 2u64;
        while hi <= next_number {
            hi = hi.checked_mul(2).ok_or_else(|| map_unavailable(&m.name))?;
        }
        probe_slots.insert(hi);
        let mut lo = hi / 2;
        let mut cur_hi = hi;
        while cur_hi - lo > 1 {
            let mid = lo + (cur_hi - lo) / 2;
            if mid > next_number {
                probe_slots.insert(mid);
                cur_hi = mid;
            } else {
                lo = mid;
            }
        }
    }
    for slot in probe_slots {
        control.check()?;
        let iri = rv::map_iri(dataset, &m.name, slot, control)?;
        if !matches!(
            state.reads.record(&iri, std::slice::from_ref(&m.maps))?,
            ExistingRecord::Absent
        ) {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "policy {}: maps graph holds a record at revision slot {slot} past revision {prev_number}",
                    m.name
                ),
            ));
        }
    }
    Ok(())
}

/// Fold the authenticated revision chain ending at `head_rev` to obtain the set of
/// accepted decisions effective at `head_rev`.
fn fold_previous_effective(
    state: &mut State<'_>,
    m: &MapGraphs,
    head_rev: &NamedNode,
    retained: &mut usize,
    retained_limit: usize,
    control: &Control,
) -> Result<BTreeSet<NamedNode>> {
    fold_previous_effective_bounded(
        state,
        m,
        head_rev,
        retained,
        retained_limit,
        MAX_EVIDENCE,
        control,
    )
}

fn fold_previous_effective_bounded(
    state: &mut State<'_>,
    m: &MapGraphs,
    head_rev: &NamedNode,
    retained: &mut usize,
    retained_limit: usize,
    max_effective: usize,
    control: &Control,
) -> Result<BTreeSet<NamedNode>> {
    const MAX_FOLD_ENTRIES: usize = MAX_EVIDENCE * 10;
    let mut chain = Vec::new();
    let mut visited = BTreeSet::new();
    let mut visited_bytes = 0usize;
    let mut cursor = Some(head_rev.clone());
    while let Some(rev) = cursor {
        control.check()?;
        if visited.contains(&rev) {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "policy {}: resolution map revision chain contains a cycle at <{}>",
                    m.name,
                    rev.as_str()
                ),
            ));
        }
        if !state.typed(&rev, &m.maps, RecordKind::ResolutionMap)? {
            break;
        }
        if chain.len() == MAX_EVIDENCE {
            return Err(incomplete("resolution map revisions"));
        }
        let len = rev.as_str().len();
        retain(retained, retained_limit, len.saturating_mul(2))?;
        visited_bytes = visited_bytes.saturating_add(len);
        visited.insert(rev.clone());
        let next = state.iri(&rev, &format!("{PROV}wasRevisionOf"), &m.maps)?;
        chain.push(rev);
        cursor = next;
    }
    drop(visited);
    *retained = retained.saturating_sub(visited_bytes);
    chain.reverse();
    let mut effective = BTreeSet::new();
    let mut scanned = 0usize;
    for rev in &chain {
        control.check()?;
        let (adds, more_a) = state.reads.objects_up_to(
            rev.as_str(),
            &ev("adds"),
            m.maps.0.as_str(),
            MAX_EVIDENCE,
        )?;
        if more_a {
            return Err(incomplete("resolution map decisions"));
        }
        let (ends, more_e) = state.reads.objects_up_to(
            rev.as_str(),
            &ev("ends"),
            m.maps.0.as_str(),
            MAX_EVIDENCE,
        )?;
        if more_e {
            return Err(incomplete("resolution map decisions"));
        }
        // Apply endings before additions so a replacement transition between two valid
        // states below `max_effective` does not fail on their transient union. Track
        // ended decisions so any overlap in a revision preserves `adds`-then-`ends`
        // semantics.
        let mut ended: BTreeSet<&NamedNode> = BTreeSet::new();
        for t in &ends {
            if let Term::NamedNode(d) = t {
                scanned += 1;
                if scanned > MAX_FOLD_ENTRIES {
                    return Err(incomplete_limit(
                        "resolution map revision entries",
                        MAX_FOLD_ENTRIES,
                    ));
                }
                ended.insert(d);
                if effective.remove(d) {
                    *retained = retained.saturating_sub(d.as_str().len());
                }
            }
        }
        for t in adds {
            if let Term::NamedNode(d) = t {
                scanned += 1;
                if scanned > MAX_FOLD_ENTRIES {
                    return Err(incomplete_limit(
                        "resolution map revision entries",
                        MAX_FOLD_ENTRIES,
                    ));
                }
                if ended.contains(&d) {
                    continue;
                }
                if !effective.contains(&d) {
                    if effective.len() == max_effective {
                        return Err(incomplete_limit("resolution map decisions", max_effective));
                    }
                    retain(retained, retained_limit, d.as_str().len())?;
                    effective.insert(d);
                }
            }
        }
    }
    Ok(effective)
}

/// Administered rebuild of a matching policy's resolution map on `logical` (`main` or one
/// of its branches) after a boundary or protection change.
///
/// Recomputes the clusters of the policy's current authenticated accepted decisions whose
/// targets are authenticated identity proposals in the current protected identity graphs
/// of the policy's boundary, validates all current effective separations, rewrites the
/// policy's output graph atomically to match those clusters, and appends a fresh
/// `ev:ResolutionMap` revision recording the current graph protections and boundary
/// protections.
///
/// Refusals, which write nothing: invalid input or unknown matching policy,
/// `unsupported-profile` when the evidence profile is disabled or a policy graph is
/// detached, `policy-changed` when `policy_version` does not match on first execution,
/// `head-changed`, `identity-conflict` when an effective separation joins one cluster,
/// `immutable-record-conflict`, `idempotency-conflict`, `incomplete-evidence` beyond the
/// operation's record or local memory bounds, `NotPermitted` when `options.write` lacks
/// access or masks triples, and `authority-changed`.
pub fn rebuild(
    main: &Dataset,
    logical: &Dataset,
    policy_name: &str,
    policy_version: Option<u32>,
    options: &Options,
    control: &Control,
) -> Result<RebuiltMap> {
    rebuild_bounded(
        main,
        logical,
        policy_name,
        policy_version,
        options,
        control,
        READS_BYTES,
        RETAINED_BYTES,
    )
}

#[cfg(test)]
#[derive(Default)]
struct RebuildHooks<'a> {
    on_after_diff: Option<&'a mut dyn FnMut()>,
    on_before_publish: Option<&'a mut dyn FnMut()>,
}

#[allow(clippy::too_many_arguments)]
fn rebuild_bounded(
    main: &Dataset,
    logical: &Dataset,
    policy_name: &str,
    policy_version: Option<u32>,
    options: &Options,
    control: &Control,
    reads_bytes: u64,
    retained_limit: usize,
) -> Result<RebuiltMap> {
    rebuild_inner(
        main,
        logical,
        policy_name,
        policy_version,
        options,
        control,
        reads_bytes,
        retained_limit,
        #[cfg(test)]
        RebuildHooks::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn rebuild_inner(
    main: &Dataset,
    logical: &Dataset,
    policy_name: &str,
    policy_version: Option<u32>,
    options: &Options,
    control: &Control,
    reads_bytes: u64,
    retained_limit: usize,
    #[cfg(test)] mut hooks: RebuildHooks<'_>,
) -> Result<RebuiltMap> {
    control.check()?;
    check_store(main, logical)?;
    let (settings, settings_bytes) = settings_of(main)?;
    if !settings.enabled {
        return Err(managed(
            ManagedErrorKind::UnsupportedProfile,
            "the evidence profile is not enabled in this dataset",
        ));
    }
    let Some(policy) = settings.matching_policies.get(policy_name) else {
        return Err(Error::invalid(format!(
            "policy: the evidence settings name no matching policy {policy_name}"
        )));
    };
    let operation = operation(main, options)?;
    let payload = rv::rebuild_map_payload_digest(policy_name, policy_version, control)?;
    if let Some(access) = options.write.as_ref() {
        if access.hides_triples() {
            return Err(Error::NotPermitted(
                "rebuilding a resolution map requires an unmasked view of the policy's graphs"
                    .into(),
            ));
        }
        for g in [&policy.maps, &policy.output] {
            let g = NamedNode::new(g.as_str()).map_err(|e| Error::invalid(e.to_string()))?;
            writable(Some(access), &g)?;
        }
        for g in required_graphs(&settings, policy) {
            let g = NamedNode::new(g.as_str()).map_err(|e| Error::invalid(e.to_string()))?;
            readable(Some(access), &g)?;
        }
    }
    let capability = capability(logical, operation, &settings_bytes)?;
    let m = MapGraphs::new(policy_name, policy, &settings, &|g| {
        capability.protection(g)
    })?;
    let nonces = m.protected()?;
    let store = logical.store();
    let branch = store.branch_id();
    let head = store.snapshot();
    let preparation = map_local_budget(crate::access::Preparation::new(
        control.clone(),
        reads_bytes,
    ))?;
    let records = record_graph_nonces(&settings, &capability);
    let mut state = State {
        reads: Reads::new(&preparation, &head),
        seen: None,
        claim_graphs: Vec::new(),
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    // A repeated idempotency key answers the first receipt before checking current
    // execution preconditions such as policyVersion or ifHead.
    let replayed_receipt = map_local_budget((|| -> Result<Option<RebuiltMap>> {
        if options.idempotency_key.is_none() {
            return Ok(None);
        }
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
        let Some((commit, recorded_branch)) = replayed(found, &probe)? else {
            return Ok(None);
        };
        let (published, _) = state.reads.subjects_up_to(
            &ev("availabilityReceipt"),
            &Term::NamedNode(probe.operation_iri.clone()),
            m.maps.0.as_str(),
            MAX_DECISIONS + 2,
        )?;
        let mut found_rev = None;
        for s in published {
            if state.typed(&s, &m.maps, RecordKind::ResolutionMap)?
                && state.reads.contains(
                    s.as_str(),
                    &ev("reviewPolicy"),
                    &Term::NamedNode(m.policy_iri.clone()),
                    m.maps.0.as_str(),
                )?
            {
                found_rev = Some(s);
                break;
            }
        }
        let Some(revision) = found_rev else {
            return Err(managed(
                ManagedErrorKind::ImmutableRecordConflict,
                format!(
                    "operation <{}> has no authenticated resolution map revision in <{}>",
                    probe.operation_iri.as_str(),
                    m.maps.0.as_str()
                ),
            ));
        };
        let number = map_revision_number(&mut state, &revision, &m.maps)?
            .ok_or_else(|| map_unavailable(&m.name))?;
        let previous = state.iri(&revision, &format!("{PROV}wasRevisionOf"), &m.maps)?;
        let recorded_version = state
            .text(&revision, &ev("policyVersion"), &m.maps)?
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(m.version);
        let (built, _) = state.reads.objects_up_to(
            revision.as_str(),
            BOUNDARY_PROTECTION,
            m.maps.0.as_str(),
            MAX_BOUNDARY,
        )?;
        let mut boundary: Vec<NamedNode> = built
            .into_iter()
            .filter_map(|t| match t {
                Term::Literal(l) => l
                    .value()
                    .split_once(' ')
                    .and_then(|(_, iri)| NamedNode::new(iri).ok()),
                _ => None,
            })
            .collect();
        boundary.sort();
        boundary.dedup();
        Ok(Some(RebuiltMap {
            operation: probe.operation_iri,
            committed: false,
            replayed: true,
            commit,
            branch: recorded_branch.unwrap_or(branch),
            policy: m.policy_iri.clone(),
            policy_version: recorded_version,
            revision,
            number,
            previous,
            boundary,
            changed: Changed::default(),
            preview: None,
        }))
    })())?;
    if let Some(receipt) = replayed_receipt {
        return Ok(receipt);
    }
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
    struct RebuildPlan {
        batch: Batch,
        raw_deletes: Vec<[sparkles_core::id::Id; 4]>,
        changes: Vec<(Quad, bool)>,
        changed: Changed,
        revision: NamedNode,
        number: u64,
        previous: Option<NamedNode>,
        boundary: Vec<NamedNode>,
        output_held: sparkles_core::access::Reservation,
    }
    let plan = map_local_budget((|| -> Result<RebuildPlan> {
        let prev_present = latest_present(&mut state, main.dataset_id(), &m, control)?;
        let prev_number = prev_present.as_ref().map_or(0, |(_, n, _)| *n);
        let prev_auth_typed = match &prev_present {
            Some((iri, _, true)) => state.typed(iri, &m.maps, RecordKind::ResolutionMap)?,
            _ => false,
        };
        let next_number = prev_number
            .checked_add(1)
            .ok_or_else(|| map_unavailable(&m.name))?;
        verify_maps_for_rebuild(
            &mut state,
            main.dataset_id(),
            &m,
            prev_number,
            prev_auth_typed,
            next_number,
            control,
        )?;
        let previous =
            prev_auth_typed.then(|| prev_present.as_ref().expect("prev_present").0.clone());
        let mut retained = 0usize;
        let prev_effective = match &previous {
            Some(prev_iri) => fold_previous_effective(
                &mut state,
                &m,
                prev_iri,
                &mut retained,
                retained_limit,
                control,
            )?,
            None => BTreeSet::new(),
        };
        // Collect all current authenticated accepted decisions under this matching policy
        // whose targets are authenticated identity proposals in the current protected
        // identity graphs of the boundary.
        let (decision_candidates, more) = state.reads.subjects_up_to(
            &ev("reviewPolicy"),
            &Term::NamedNode(m.policy_iri.clone()),
            m.decisions.0.as_str(),
            MAX_EVIDENCE,
        )?;
        if more {
            return Err(incomplete("decisions of the matching policy"));
        }
        let mut seen_current_target: BTreeSet<NamedNode> = BTreeSet::new();
        let mut effective_decisions: BTreeSet<NamedNode> = BTreeSet::new();
        let mut matches: Vec<(NamedNode, Facts)> = Vec::new();
        let mut distincts: Vec<(NamedNode, Facts)> = Vec::new();
        for d in decision_candidates {
            control.check()?;
            if !state.typed(&d, &m.decisions, RecordKind::ReviewDecision)? {
                continue;
            }
            if state.is_superseded(&d, &m.decisions)? {
                continue;
            }
            let Some(target) = state.iri(&d, &ev("target"), &m.decisions)? else {
                continue;
            };
            let Some(disp) = state.disposition(&d, &m.decisions)? else {
                continue;
            };
            if seen_current_target.contains(&target) {
                return Err(managed(
                    ManagedErrorKind::ImmutableRecordConflict,
                    format!(
                        "<{}> has more than one current decision under policy {}",
                        target.as_str(),
                        m.name
                    ),
                ));
            }
            retain(&mut retained, retained_limit, target.as_str().len())?;
            seen_current_target.insert(target.clone());
            if disp != Disposition::Accepted {
                continue;
            }
            let mut found_graph: Option<G> = None;
            for g in &m.proposals {
                if g.1.is_none() {
                    continue;
                }
                if state.typed(&target, g, RecordKind::IdentityProposal)? {
                    if found_graph.is_some() {
                        return Err(managed(
                            ManagedErrorKind::ImmutableRecordConflict,
                            format!(
                                "<{}> is recorded in more than one identity graph of policy {}",
                                target.as_str(),
                                m.name
                            ),
                        ));
                    }
                    found_graph = Some(g.clone());
                }
            }
            let Some(tg) = found_graph else {
                // Target left the boundary or its identity graph was detached/re-protected.
                continue;
            };
            let f = facts(&mut state, &target, &tg)?;
            let Some(relation) = f.relation else {
                continue;
            };
            retain(
                &mut retained,
                retained_limit,
                d.as_str().len() + target.as_str().len() + fact_bytes(&f),
            )?;
            effective_decisions.insert(d);
            match relation {
                IdentityRelation::PossibleMatch => matches.push((target, f)),
                IdentityRelation::DistinctEntities => distincts.push((target, f)),
            }
        }
        // Recompute connected components from all effective PossibleMatch proposals.
        let mut entity_to_matches: BTreeMap<NamedNode, Vec<usize>> = BTreeMap::new();
        for (idx, (_, f)) in matches.iter().enumerate() {
            control.check()?;
            for e in &f.entities {
                entity_to_matches.entry(e.clone()).or_default().push(idx);
            }
        }
        let mut visited: BTreeSet<NamedNode> = BTreeSet::new();
        let mut desired: BTreeMap<NamedNode, NamedNode> = BTreeMap::new();
        for seed in entity_to_matches.keys() {
            if visited.contains(seed) {
                continue;
            }
            let mut cluster = Cluster::default();
            cluster.entities.insert(seed.clone());
            visited.insert(seed.clone());
            let mut queue = VecDeque::from([seed.clone()]);
            while let Some(e) = queue.pop_front() {
                control.check()?;
                if let Some(indices) = entity_to_matches.get(&e) {
                    for &idx in indices {
                        let (_, f) = &matches[idx];
                        if let Some(pref) = &f.preferred {
                            cluster.preferred.insert(pref.clone());
                        }
                        for x in &f.entities {
                            if cluster.entities.insert(x.clone()) {
                                visited.insert(x.clone());
                                queue.push_back(x.clone());
                            }
                        }
                    }
                }
            }
            if let Some(rep) = cluster.representative().cloned() {
                for x in cluster.entities {
                    retain(
                        &mut retained,
                        retained_limit,
                        x.as_str().len() + rep.as_str().len(),
                    )?;
                    desired.insert(x, rep.clone());
                }
            }
        }
        // Validate all effective DistinctEntities constraints against the recomputed clusters.
        for (_, f) in &distincts {
            control.check()?;
            let mut seen_reps: BTreeSet<&NamedNode> = BTreeSet::new();
            for e in &f.entities {
                if let Some(rep) = desired.get(e)
                    && !seen_reps.insert(rep)
                {
                    return Err(managed(
                        ManagedErrorKind::IdentityConflict,
                        format!(
                            "policy {}: an accepted separation keeps two entities of one cluster apart; reverse the match or the separation",
                            m.name
                        ),
                    ));
                }
            }
        }
        // Diff the desired resolvesTo triples against ALL existing quads in the output graph
        // using physical term IDs so blank-node and detached-epoch quads are stripped cleanly
        // under the shared preparation budget without decoding unchanged output triples.
        let mut output_held = state.reads.preparation().reserve(0)?;
        let mut keep: HashSet<[sparkles_core::id::Id; 3]> = HashSet::new();
        let mut desired_ids: Vec<(&NamedNode, &NamedNode, Option<[sparkles_core::id::Id; 3]>)> =
            Vec::new();
        let resolves_to_id = state.reads.iri_id(RESOLVES_TO)?;
        if !desired.is_empty() {
            let slot_bytes = desired.len() as u64
                * std::mem::size_of::<(&NamedNode, &NamedNode, Option<[sparkles_core::id::Id; 3]>)>(
                ) as u64;
            output_held.grow_by(slot_bytes)?;
            desired_ids.reserve_exact(desired.len());
            if let Some(p_id) = resolves_to_id {
                let keep_bytes = 256u64
                    + desired.len() as u64
                        * (std::mem::size_of::<[sparkles_core::id::Id; 3]>() as u64 + 8)
                        * 2;
                output_held.grow_by(keep_bytes)?;
                keep.reserve(desired.len());
                for (e, rep) in &desired {
                    control.check()?;
                    let triple_id = match (
                        state.reads.iri_id(e.as_str())?,
                        state.reads.iri_id(rep.as_str())?,
                    ) {
                        (Some(s_id), Some(o_id)) => {
                            let id = [s_id, p_id, o_id];
                            keep.insert(id);
                            Some(id)
                        }
                        _ => None,
                    };
                    desired_ids.push((e, rep, triple_id));
                }
            } else {
                for (e, rep) in &desired {
                    control.check()?;
                    desired_ids.push((e, rep, None));
                }
            }
        }
        let diff = state.reads.graph_quads_diff_up_to(
            m.output.0.as_str(),
            &keep,
            MAX_OUTPUT_TRIPLES,
            &mut output_held,
        )?;
        let mut removed = diff.removed;
        let present = diff.present;
        if diff.more {
            return Err(incomplete_limit(
                "triples of the output graph",
                MAX_OUTPUT_TRIPLES,
            ));
        }
        drop(keep);
        #[cfg(test)]
        if let Some(hook) = hooks.on_after_diff.as_deref_mut() {
            hook();
        }
        control.check()?;
        // Physical quads from `Gspo` are distinct, so an in-place unstable sort
        // yields the same deterministic order without allocating scratch storage.
        removed.sort_unstable_by(|a, b| cmp_triple(&a.1, &b.1));
        control.check()?;
        const MOVED_START: u64 = 1024;
        const MOVED_ENTRY: u64 = 128;
        let mut raw_deletes: Vec<[sparkles_core::id::Id; 4]> = Vec::new();
        let mut changes: Vec<(Quad, bool)> = Vec::new();
        let mut changed = Changed::default();
        let mut moved_set: BTreeSet<NamedNode> = BTreeSet::new();
        if !removed.is_empty() {
            let del_bytes = removed.len() as u64
                * (std::mem::size_of::<[sparkles_core::id::Id; 4]>() as u64
                    + std::mem::size_of::<ProjectionChange>() as u64
                    + m.output.0.as_str().len() as u64);
            output_held.grow_by(del_bytes)?;
            raw_deletes.reserve_exact(removed.len());
            changed.removed.reserve_exact(removed.len());
            for (ids, t) in removed {
                control.check()?;
                if t.predicate.as_str() == RESOLVES_TO
                    && let (NamedOrBlankNode::NamedNode(s), Term::NamedNode(_)) =
                        (&t.subject, &t.object)
                    && !moved_set.contains(s)
                {
                    let base = if moved_set.is_empty() { MOVED_START } else { 0 };
                    output_held.grow_by(base + MOVED_ENTRY + s.as_str().len() as u64)?;
                    moved_set.insert(s.clone());
                }
                raw_deletes.push(ids);
                changed.removed.push(ProjectionChange {
                    output: m.output.0.clone(),
                    triple: t,
                });
            }
        }
        let insert_count = desired_ids
            .iter()
            .filter(|(_, _, triple_id)| !triple_id.is_some_and(|id| present.contains(&id)))
            .count();
        if insert_count > 0 {
            let vec_bytes = insert_count as u64
                * (std::mem::size_of::<(Quad, bool)>() as u64
                    + std::mem::size_of::<ProjectionChange>() as u64);
            output_held.grow_by(vec_bytes)?;
            changes.reserve_exact(insert_count);
            changed.inserted.reserve_exact(insert_count);
        }
        for (e, rep, triple_id) in desired_ids {
            control.check()?;
            if triple_id.is_some_and(|id| present.contains(&id)) {
                continue;
            }
            let str_bytes = (e.as_str().len()
                + RESOLVES_TO.len()
                + rep.as_str().len()
                + m.output.0.as_str().len()) as u64
                * 2;
            output_held.grow_by(str_bytes)?;
            let t = resolves(e, rep);
            changes.push((quad(&t, &m.output.0), true));
            changed.inserted.push(ProjectionChange {
                output: m.output.0.clone(),
                triple: t,
            });
            if !moved_set.contains(e) {
                let base = if moved_set.is_empty() { MOVED_START } else { 0 };
                output_held.grow_by(base + MOVED_ENTRY + e.as_str().len() as u64)?;
                moved_set.insert(e.clone());
            }
        }
        drop(present);
        let mut moved: Vec<NamedNode> = Vec::new();
        if !moved_set.is_empty() {
            output_held
                .grow_by(moved_set.len() as u64 * std::mem::size_of::<NamedNode>() as u64)?;
            moved.reserve_exact(moved_set.len());
            moved.extend(moved_set);
        }
        let adds: Vec<NamedNode> = effective_decisions
            .difference(&prev_effective)
            .cloned()
            .collect();
        let ends: Vec<NamedNode> = prev_effective
            .difference(&effective_decisions)
            .cloned()
            .collect();
        let boundary_protections = m.boundary_protections();
        let record = rv::map_record(
            main.dataset_id(),
            &MapRevision {
                policy: policy_name,
                version: m.version,
                number: next_number,
                previous: previous.as_ref(),
                output: &m.output.0,
                adds: &adds,
                ends: &ends,
                nonces,
                boundary: &boundary_protections,
            },
            &m.maps.0,
            control,
        )?;
        let revision = record.iri.clone();
        changed.map = Some(MapChange {
            policy: m.policy_iri.clone(),
            revision: revision.clone(),
            number: next_number,
            previous: previous.clone(),
            decisions_added: adds,
            decisions_ended: ends,
            entities: moved,
        });
        let batch = Batch {
            operation,
            operation_iri: operation_iri(operation),
            payload_digest: payload,
            principal: options.principal.clone(),
            idempotency_key: options.idempotency_key.clone(),
            graphs: vec![m.maps.0.clone()],
            records: vec![record],
        };
        let skip = vec![false];
        check_identities(&mut state, &batch, &records, &skip)?;
        let boundary = boundary_protections.into_iter().map(|(g, _)| g).collect();
        Ok(RebuildPlan {
            batch,
            raw_deletes,
            changes,
            changed,
            revision,
            number: next_number,
            previous,
            boundary,
            output_held,
        })
    })())?;
    drop(state);
    #[cfg(test)]
    if let Some(hook) = hooks.on_before_publish.as_deref_mut() {
        hook();
    }
    let skip = vec![false];
    let committed = publish(
        main,
        logical,
        capability,
        head.commit,
        (!plan.raw_deletes.is_empty()).then_some(head.generation.uid),
        &plan.batch,
        &skip,
        &plan.raw_deletes,
        &plan.changes,
        options,
        control,
    )?;
    drop(plan.raw_deletes);
    drop(plan.changes);
    drop(plan.output_held);
    Ok(RebuiltMap {
        operation: plan.batch.operation_iri,
        committed: committed.committed,
        replayed: false,
        commit: committed.commit,
        branch,
        policy: m.policy_iri,
        policy_version: m.version,
        revision: plan.revision,
        number: plan.number,
        previous: plan.previous,
        boundary: plan.boundary,
        changed: plan.changed,
        preview: committed.preview,
    })
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
        ResolveHead::Snapshot(&head),
        policy,
        entities,
        access,
        control,
        decision_limit,
        READS_BYTES,
    )
}

#[cfg(test)]
fn resolve_with_reads_bytes(
    main: &Dataset,
    logical: &Dataset,
    policy: &str,
    entities: &[NamedNode],
    access: Option<Arc<GraphAccess>>,
    control: &Control,
    reads_bytes: u64,
) -> Result<EntityResolution> {
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
        ResolveHead::Snapshot(&head),
        policy,
        entities,
        access,
        control,
        MAX_EVIDENCE,
        reads_bytes,
    )
}

/// What the resolution map of the matching policy `policy` selects for each of
/// `entities` in an evidence view, with the policy, its graphs and their protections
/// as the view's configuration records them. Refusals are those of [`resolve`], except
/// that an empty `entities` is allowed and reads and checks only the map's revision.
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
        ResolveHead::Evidence(view),
        policy,
        entities,
        access,
        control,
        MAX_EVIDENCE,
        READS_BYTES,
    )
}

#[derive(Clone, Copy)]
enum ResolveHead<'a> {
    Snapshot(&'a Arc<crate::store::Snapshot>),
    Evidence(&'a sparkles_core::store::BoundEvidenceView),
}

impl<'a> ResolveHead<'a> {
    fn validate_access(self, access: &GraphAccess) -> Result<()> {
        match self {
            Self::Snapshot(head) => access.validate_bound_snapshot(head),
            Self::Evidence(view) => view.validate_access(access),
        }
    }

    fn caller_view(
        self,
        access: Option<&Arc<GraphAccess>>,
        preparation: &crate::access::Preparation,
    ) -> Result<Option<crate::access::PreparedView>> {
        match self {
            Self::Snapshot(head) => caller_view(access, head, preparation),
            Self::Evidence(view) => caller_view_evidence(access, view, preparation),
        }
    }

    fn reads(self, preparation: &'a crate::access::Preparation) -> Reads<'a> {
        match self {
            Self::Snapshot(head) => Reads::new(preparation, head),
            Self::Evidence(view) => Reads::from_evidence(preparation, view),
        }
    }

    fn commit(self) -> u64 {
        match self {
            Self::Snapshot(head) => head.commit,
            Self::Evidence(view) => view.commit().seq,
        }
    }
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
    head: ResolveHead<'_>,
    policy: &str,
    entities: &[NamedNode],
    access: Option<Arc<GraphAccess>>,
    control: &Control,
    decision_limit: usize,
    reads_bytes: u64,
) -> Result<EntityResolution> {
    if entities.len() > MAX_RESOLVE {
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
    if let Some(a) = access.as_deref() {
        head.validate_access(a)?;
    }
    for (g, _) in [&m.decisions, &m.maps, &m.output]
        .into_iter()
        .chain(m.proposals.iter())
    {
        readable(access.as_ref(), g)?;
    }
    let preparation = crate::access::Preparation::new(control.clone(), reads_bytes)?;
    let hides_triples = access.as_deref().is_some_and(GraphAccess::hides_triples);
    // Records are read at the unmasked head with the caller's view beside it. The map's
    // consistency is checked first using `hides_triples` before materializing the masked
    // view, so a boundary refusal cannot be preempted by data-dependent mask limits. A
    // revision the caller's view masks is then refused rather than read, any proposal or
    // decision the view hides any triple of takes no part in a cluster, and the output
    // graph is read in the caller's view.
    let view;
    let mut state = State {
        reads: head.reads(&preparation),
        seen: None,
        claim_graphs: Vec::new(),
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    let latest = latest(&mut state, dataset_id, &m, hides_triples, control)?;
    view = head.caller_view(access.as_ref(), &preparation)?;
    state.seen = view.as_ref().map(|v| Reads::from_prepared(&preparation, v));
    if let Some(l) = &latest
        && state.masked(&l.iri, &m.maps.0)?
    {
        return Err(map_unavailable(&m.name));
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
        commit: head.commit(),
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

    #[test]
    fn rebuild_local_budget_maps_to_incomplete_evidence_and_preserves_other_errors() {
        // Local preparation memory exhaustion maps to `incomplete-evidence` (HTTP 422)
        // with a memory-specific message, while server-pool exhaustion, cancellation and
        // timeout stay distinct.
        let local_err: Result<()> = Err(Error::BudgetExceeded(sparkles_core::Budget {
            kind: sparkles_core::BudgetKind::Memory,
            limit: 1024,
            requested: 2048,
        }));
        let mapped = map_local_budget(local_err).unwrap_err();
        assert_eq!(mapped.code(), "incomplete-evidence");
        assert!(
            mapped
                .to_string()
                .contains("memory allowance of 1024 bytes"),
            "{mapped}"
        );
        assert!(
            !mapped.to_string().contains("records"),
            "memory exhaustion must not report a record-count limit: {mapped}"
        );
        assert!(matches!(
            mapped,
            Error::Managed(ref m) if m.kind == ManagedErrorKind::IncompleteEvidence && m.status() == 422
        ));

        let fold_err = incomplete_limit("resolution map revision entries", MAX_EVIDENCE * 10);
        assert_eq!(fold_err.code(), "incomplete-evidence");
        assert!(
            fold_err.to_string().contains("100000 records"),
            "{fold_err}"
        );
        let output_err = incomplete_limit("triples of the output graph", MAX_OUTPUT_TRIPLES);
        assert_eq!(output_err.code(), "incomplete-evidence");
        assert!(
            output_err.to_string().contains("1000000 records"),
            "{output_err}"
        );

        let server_err: Result<()> = Err(Error::BudgetExceeded(sparkles_core::Budget {
            kind: sparkles_core::BudgetKind::ServerMemory,
            limit: 1024,
            requested: 2048,
        }));
        assert!(matches!(
            map_local_budget(server_err),
            Err(Error::BudgetExceeded(b)) if b.kind == sparkles_core::BudgetKind::ServerMemory
        ));
        assert!(matches!(
            map_local_budget::<()>(Err(Error::Cancelled)),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            map_local_budget::<()>(Err(Error::Timeout)),
            Err(Error::Timeout)
        ));

        // Detached output graph with a large literal that exceeds a small local preparation
        // allowance must refuse with `incomplete-evidence` without committing.
        let ds = configured();
        let ab = propose(&ds, IDENTITY, "ab", &["urn:a", "urn:b"]);
        decide(&ds, &ab, Disposition::Accepted, None);
        crate::evidence::detach(&ds, "urn:ev:map").unwrap();
        let big = "x".repeat(32 * 1024);
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <urn:ev:map> {{ <urn:z> <urn:big> \"{big}\" }} }}"
        ))
        .unwrap();
        let (settings, _) = settings_of(&ds).unwrap();
        crate::evidence::configure(&ds, &settings, &Control::none()).unwrap();
        let head = ds.store().head_commit().seq;
        let admin = Options::new(n("urn:principal:admin"));

        // Under a 16 KiB local preparation cap, decoding the 32 KiB literal refuses before
        // retaining it, returning `incomplete-evidence` (HTTP 422) and writing nothing.
        let err = rebuild_bounded(
            &ds,
            &ds,
            "people",
            None,
            &admin,
            &Control::none(),
            16 * 1024,
            RETAINED_BYTES,
        )
        .unwrap_err();
        assert_eq!(err.code(), "incomplete-evidence", "{err}");
        assert!(err.to_string().contains("memory allowance"), "{err}");
        assert_eq!(ds.store().head_commit().seq, head);

        // Pre-cancelled entry check returns Error::Cancelled with no commit.
        let cancelled = Control::none();
        cancelled.cancel.cancel();
        let err = rebuild_bounded(
            &ds,
            &ds,
            "people",
            None,
            &admin,
            &cancelled,
            READS_BYTES,
            RETAINED_BYTES,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert_eq!(ds.store().head_commit().seq, head);

        // Cancellation triggered mid-planning (after output diff completes, before staging)
        // must hit the staging checkpoint and return Error::Cancelled with no commit.
        let mid_cancel = Control::none();
        let cancel_handle = mid_cancel.cancel.clone();
        let mut diff_reached = false;
        let mut on_after_diff = || {
            diff_reached = true;
            cancel_handle.cancel();
        };
        let err = rebuild_inner(
            &ds,
            &ds,
            "people",
            None,
            &admin,
            &mid_cancel,
            READS_BYTES,
            RETAINED_BYTES,
            RebuildHooks {
                on_after_diff: Some(&mut on_after_diff),
                on_before_publish: None,
            },
        )
        .unwrap_err();
        assert!(
            diff_reached,
            "cancellation must occur after planning/diff started"
        );
        assert!(matches!(err, Error::Cancelled), "{err:?}");
        assert_eq!(ds.store().head_commit().seq, head);

        // Under the normal allowance the rebuild succeeds and strips the large literal.
        let ok = rebuild_bounded(
            &ds,
            &ds,
            "people",
            None,
            &admin,
            &Control::none(),
            READS_BYTES,
            RETAINED_BYTES,
        )
        .unwrap();
        assert!(ok.committed);
        assert_eq!(ok.changed.removed.len(), 1);
    }

    /// Compaction switches the store generation at the same commit and renumbers delta
    /// and base vocabulary IDs. Fencing `publish` on `generation.uid` refuses stale
    /// physical `raw_deletes` with retryable `head-changed` and writes nothing, and a
    /// retry after compaction strips all detached quads cleanly.
    #[test]
    fn rebuild_refuses_compaction_generation_switch_before_publish() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
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
        let ab = propose(&ds, IDENTITY, "ab", &["urn:a", "urn:b"]);
        decide(&ds, &ab, Disposition::Accepted, None);
        crate::evidence::detach(&ds, "urn:ev:map").unwrap();
        ds.update(
            "INSERT DATA { GRAPH <urn:ev:map> { \
                <urn:z> <urn:bogus> \"junk\" . \
                _:b0 <urn:bogus> \"bnode-subj\" . \
                <urn:z> <urn:bogus> _:b1 . \
                <urn:z> <urn:reifies> <<( _:b2 <urn:p> \"v\" )>> \
            } }",
        )
        .unwrap();
        crate::evidence::configure(&ds, &settings, &Control::none()).unwrap();
        let head_before = ds.store().snapshot();
        let admin = Options::new(n("urn:principal:admin"));

        let mut compacted_at_same_commit = false;
        let mut on_before_publish = || {
            ds.store().compact().unwrap();
            let after = ds.store().snapshot();
            assert_eq!(after.commit, head_before.commit);
            assert_ne!(after.generation.uid, head_before.generation.uid);
            compacted_at_same_commit = true;
        };
        let err = rebuild_inner(
            &ds,
            &ds,
            "people",
            None,
            &admin,
            &Control::none(),
            READS_BYTES,
            RETAINED_BYTES,
            RebuildHooks {
                on_after_diff: None,
                on_before_publish: Some(&mut on_before_publish),
            },
        )
        .unwrap_err();
        assert!(compacted_at_same_commit);
        assert_eq!(err.code(), "head-changed", "{err}");
        assert_eq!(ds.store().head_commit().seq, head_before.commit);

        // Retrying on the compacted generation succeeds and strips all 4 detached quads.
        let ok = rebuild(&ds, &ds, "people", None, &admin, &Control::none()).unwrap();
        assert!(ok.committed);
        assert_eq!(ok.changed.removed.len(), 4);
        let remaining = ds
            .construct("CONSTRUCT { ?s ?p ?o } WHERE { GRAPH <urn:ev:map> { ?s ?p ?o } }")
            .unwrap()
            .len();
        assert_eq!(
            remaining, 2,
            "retry after compaction must leave only the 2 recomputed resolvesTo triples"
        );
    }

    /// Output diff staging (removal vectors, unsorted removal sort, insertion vectors,
    /// moved-entity set and vector conversion) is charged to the shared preparation
    /// reservation: a cap that admits decoding the diff still rejects unadmitted staging
    /// for both removal-heavy and insertion-heavy rebuilds.
    #[test]
    fn rebuild_staging_budget_rejects_unadmitted_removals_and_insertions() {
        let admin = Options::new(n("urn:principal:admin"));

        // 1. Removal-heavy path with unsorted detached resolvesTo quads.
        let ds = configured();
        let ab = propose(&ds, IDENTITY, "ab", &["urn:a", "urn:b"]);
        decide(&ds, &ab, Disposition::Accepted, None);
        crate::evidence::detach(&ds, "urn:ev:map").unwrap();
        let mut insert_triples = String::new();
        for i in (0..80).rev() {
            let pad = "p".repeat(120);
            insert_triples.push_str(&format!(
                "<urn:stale:{pad}:{i:03}> <{RESOLVES_TO}> <urn:rep:{pad}:{i:03}> .\n"
            ));
        }
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <urn:ev:map> {{ {insert_triples} }} }}"
        ))
        .unwrap();
        let (settings, _) = settings_of(&ds).unwrap();
        crate::evidence::configure(&ds, &settings, &Control::none()).unwrap();
        let head = ds.store().head_commit().seq;

        // Find the smallest cap (in 4 KiB steps) that admits diff decoding (`diff_reached`),
        // and verify staging still refuses under that tight cap with no commit.
        let mut saw_staging_refusal = false;
        for step in 1..=64u64 {
            let cap = step * 4096;
            let mut diff_reached = false;
            let mut on_after_diff = || {
                diff_reached = true;
            };
            let res = rebuild_inner(
                &ds,
                &ds,
                "people",
                None,
                &admin,
                &Control::none(),
                cap,
                RETAINED_BYTES,
                RebuildHooks {
                    on_after_diff: Some(&mut on_after_diff),
                    on_before_publish: None,
                },
            );
            if diff_reached {
                let err = res.expect_err("tight cap after diff must reject removal staging");
                assert_eq!(err.code(), "incomplete-evidence", "{err}");
                assert!(err.to_string().contains("memory allowance"), "{err}");
                assert_eq!(ds.store().head_commit().seq, head);
                saw_staging_refusal = true;
                break;
            }
            assert_eq!(res.unwrap_err().code(), "incomplete-evidence");
            assert_eq!(ds.store().head_commit().seq, head);
        }
        assert!(
            saw_staging_refusal,
            "expected a cap that admits diff decoding and rejects removal staging"
        );
        let ok = rebuild(&ds, &ds, "people", None, &admin, &Control::none()).unwrap();
        assert!(ok.committed);
        assert_eq!(ok.changed.removed.len(), 80);

        // 2. Insertion-heavy path: cleared output graph with many desired resolvesTo triples.
        let ds2 = configured();
        let pad = "q".repeat(120);
        for i in 0..40 {
            let p = propose(
                &ds2,
                IDENTITY,
                &format!("ins{i}"),
                &[
                    &format!("urn:ins:{pad}:a:{i:03}"),
                    &format!("urn:ins:{pad}:b:{i:03}"),
                ],
            );
            decide(&ds2, &p, Disposition::Accepted, None);
        }
        crate::evidence::detach(&ds2, "urn:ev:map").unwrap();
        ds2.update("CLEAR GRAPH <urn:ev:map>").unwrap();
        let (settings2, _) = settings_of(&ds2).unwrap();
        crate::evidence::configure(&ds2, &settings2, &Control::none()).unwrap();
        let head2 = ds2.store().head_commit().seq;

        let mut saw_insert_staging_refusal = false;
        for step in 1..=64u64 {
            let cap = step * 4096;
            let mut diff_reached = false;
            let mut on_after_diff = || {
                diff_reached = true;
            };
            let res = rebuild_inner(
                &ds2,
                &ds2,
                "people",
                None,
                &admin,
                &Control::none(),
                cap,
                RETAINED_BYTES,
                RebuildHooks {
                    on_after_diff: Some(&mut on_after_diff),
                    on_before_publish: None,
                },
            );
            if diff_reached {
                let err = res.expect_err("tight cap after diff must reject insertion staging");
                assert_eq!(err.code(), "incomplete-evidence", "{err}");
                assert!(err.to_string().contains("memory allowance"), "{err}");
                assert_eq!(ds2.store().head_commit().seq, head2);
                saw_insert_staging_refusal = true;
                break;
            }
            assert_eq!(res.unwrap_err().code(), "incomplete-evidence");
            assert_eq!(ds2.store().head_commit().seq, head2);
        }
        assert!(
            saw_insert_staging_refusal,
            "expected a cap that admits empty output diff and rejects insertion staging"
        );
        let ok2 = rebuild(&ds2, &ds2, "people", None, &admin, &Control::none()).unwrap();
        assert!(ok2.committed);
        assert_eq!(ok2.changed.inserted.len(), 80);
    }

    /// A replacement revision that ends `k` prior acceptances and adds `k` disjoint
    /// acceptances stays within `max_effective` even when the transient union `2k`
    /// exceeds `max_effective`, while a state whose post-transition size exceeds
    /// `max_effective` is still refused.
    #[test]
    fn fold_previous_effective_allows_replacement_transition_at_cap() {
        let ds = configured();
        let mut prior = Vec::new();
        let mut set_a = BTreeSet::new();
        for i in 0..3 {
            let p = propose(
                &ds,
                IDENTITY,
                &format!("a{i}"),
                &[&format!("urn:a{i}"), &format!("urn:b{i}")],
            );
            let d = decide(&ds, &p, Disposition::Accepted, None);
            set_a.insert(d.clone());
            prior.push((p, d));
        }
        let mut next_targets = Vec::new();
        for i in 0..3 {
            next_targets.push(propose(
                &ds,
                IDENTITY,
                &format!("b{i}"),
                &[&format!("urn:c{i}"), &format!("urn:d{i}")],
            ));
        }
        // One review batch supersedes all 3 prior acceptances (ending them) and accepts
        // 3 disjoint new proposals, publishing revision 4 with ends=3 and adds=3.
        let (settings, settings_bytes) = settings_of(&ds).unwrap();
        let mut batch_inputs = Vec::with_capacity(6);
        for (i, (p, prev_d)) in prior.iter().enumerate() {
            batch_inputs.push(DecisionInput {
                key: format!("end{i}"),
                target: p.0.clone(),
                target_digest: p.1,
                disposition: Disposition::Rejected,
                supersedes: Some(prev_d.clone()),
            });
        }
        for (i, p) in next_targets.iter().enumerate() {
            batch_inputs.push(DecisionInput {
                key: format!("add{i}"),
                target: p.0.clone(),
                target_digest: p.1,
                disposition: Disposition::Accepted,
                supersedes: None,
            });
        }
        let mut opts = Options::new(n("urn:principal:reviewer"));
        opts.if_head = Some(ds.store().head_commit().seq);
        let reviewed = review(
            None,
            &ds,
            &ds,
            Request {
                name: "people",
                policy: settings.matching_policies.get("people").unwrap(),
                settings: &settings,
                settings_bytes: &settings_bytes,
                version: 1,
                if_head: ds.store().head_commit().seq,
                candidates: MAX_EVIDENCE,
            },
            &batch_inputs,
            &opts,
            &mut |_, _| Some(Arc::new(GraphAccess::all())),
            &Control::none(),
        )
        .unwrap();
        let rev4 = reviewed.map_revision.clone().unwrap();
        let map_change = reviewed.changed.map.as_ref().unwrap();
        assert_eq!(map_change.number, 4);
        assert_eq!(map_change.decisions_ended.len(), 3);
        assert_eq!(map_change.decisions_added.len(), 3);
        let set_b: BTreeSet<NamedNode> = map_change.decisions_added.iter().cloned().collect();

        // Fold revision 4 under cap 4 (before=3, after=3, transient union=6 > 4).
        let op = Uuid::new_v4();
        let cap = capability(&ds, op, &settings_bytes).unwrap();
        let m = MapGraphs::new(
            "people",
            settings.matching_policies.get("people").unwrap(),
            &settings,
            &|g| cap.protection(g),
        )
        .unwrap();
        let head = ds.store().snapshot();
        let prep = crate::access::Preparation::new(Control::none(), READS_BYTES).unwrap();
        let mut state = State {
            reads: Reads::new(&prep, &head),
            seen: None,
            claim_graphs: Vec::new(),
            superseded: BTreeSet::new(),
            withdrawn: BTreeSet::new(),
        };
        let mut retained = 0usize;
        let folded = fold_previous_effective_bounded(
            &mut state,
            &m,
            &rev4,
            &mut retained,
            RETAINED_BYTES,
            4,
            &Control::none(),
        )
        .unwrap();
        assert_eq!(folded, set_b);
        assert!(folded.is_disjoint(&set_a));

        // Publish revision 5 from the admissible 3-decision state at rev4: end 1 decision
        // from `set_b` and accept 3 new proposals so the post-state alone has 5 effective
        // decisions (exceeding cap 4 while rev1..rev4 all stay within cap 4).
        let mut third_targets = Vec::new();
        for i in 0..3 {
            third_targets.push(propose(
                &ds,
                IDENTITY,
                &format!("c{i}"),
                &[&format!("urn:e{i}"), &format!("urn:f{i}")],
            ));
        }
        let ended_b0 = map_change.decisions_added[0].clone();
        let mut batch5 = vec![DecisionInput {
            key: "end_b0".into(),
            target: next_targets[0].0.clone(),
            target_digest: next_targets[0].1,
            disposition: Disposition::Rejected,
            supersedes: Some(ended_b0.clone()),
        }];
        for (i, p) in third_targets.iter().enumerate() {
            batch5.push(DecisionInput {
                key: format!("add_c{i}"),
                target: p.0.clone(),
                target_digest: p.1,
                disposition: Disposition::Accepted,
                supersedes: None,
            });
        }
        let mut opts5 = Options::new(n("urn:principal:reviewer"));
        opts5.if_head = Some(ds.store().head_commit().seq);
        let reviewed5 = review(
            None,
            &ds,
            &ds,
            Request {
                name: "people",
                policy: settings.matching_policies.get("people").unwrap(),
                settings: &settings,
                settings_bytes: &settings_bytes,
                version: 1,
                if_head: ds.store().head_commit().seq,
                candidates: MAX_EVIDENCE,
            },
            &batch5,
            &opts5,
            &mut |_, _| Some(Arc::new(GraphAccess::all())),
            &Control::none(),
        )
        .unwrap();
        let rev5 = reviewed5.map_revision.clone().unwrap();
        let map_change5 = reviewed5.changed.map.as_ref().unwrap();
        assert_eq!(map_change5.number, 5);
        assert_eq!(map_change5.decisions_ended.len(), 1);
        assert_eq!(map_change5.decisions_added.len(), 3);
        let mut set_c = set_b.clone();
        assert!(set_c.remove(&ended_b0));
        set_c.extend(map_change5.decisions_added.iter().cloned());
        assert_eq!(set_c.len(), 5);

        let head5 = ds.store().snapshot();
        let mut state5 = State {
            reads: Reads::new(&prep, &head5),
            seen: None,
            claim_graphs: Vec::new(),
            superseded: BTreeSet::new(),
            withdrawn: BTreeSet::new(),
        };
        // Under cap 4, rev1..rev4 are admissible, and rev5's post-state of 5 must refuse.
        let mut retained5_fail = 0usize;
        let err5 = fold_previous_effective_bounded(
            &mut state5,
            &m,
            &rev5,
            &mut retained5_fail,
            RETAINED_BYTES,
            4,
            &Control::none(),
        )
        .unwrap_err();
        assert_eq!(err5.code(), "incomplete-evidence", "{err5}");
        assert!(err5.to_string().contains("4 records"), "{err5}");

        // Under cap 5, rev5 folds to the exact 5 effective decisions.
        let mut retained5_ok = 0usize;
        let folded5 = fold_previous_effective_bounded(
            &mut state5,
            &m,
            &rev5,
            &mut retained5_ok,
            RETAINED_BYTES,
            5,
            &Control::none(),
        )
        .unwrap();
        assert_eq!(folded5, set_c);

        // Under cap 2, rev3 already exceeds the cap before reaching rev4.
        let mut retained2 = 0usize;
        let err = fold_previous_effective_bounded(
            &mut state,
            &m,
            &rev4,
            &mut retained2,
            RETAINED_BYTES,
            2,
            &Control::none(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "incomplete-evidence", "{err}");
        assert!(err.to_string().contains("2 records"), "{err}");
    }

    /// Scanning newly added boundary graphs with many unreviewed proposals streams over
    /// physical IDs without growing `Reads.ids`, so multiple added graphs never exhaust
    /// a small preparation reads cache (preventing spurious HTTP 507).
    #[test]
    fn added_boundary_graphs_do_not_exhaust_reads_cache() {
        let extras = ["urn:ev:extra-1", "urn:ev:extra-2", "urn:ev:extra-3"];
        let ds = Dataset::memory();
        let base_settings = |boundary: Vec<String>| Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            decisions: vec!["urn:ev:decisions".into()],
            identity: vec![
                IDENTITY.into(),
                extras[0].into(),
                extras[1].into(),
                extras[2].into(),
                "urn:ev:maps".into(),
            ],
            projections: vec!["urn:ev:map".into()],
            matching_policies: [(
                "people".to_string(),
                MatchingPolicy {
                    version: 1,
                    boundary,
                    decisions: "urn:ev:decisions".into(),
                    maps: "urn:ev:maps".into(),
                    output: "urn:ev:map".into(),
                },
            )]
            .into_iter()
            .collect(),
            ..Settings::default()
        };
        crate::evidence::configure(&ds, &base_settings(vec![IDENTITY.into()]), &Control::none())
            .unwrap();
        let ab = propose(&ds, IDENTITY, "ab", &["urn:a", "urn:b"]);
        decide(&ds, &ab, Disposition::Accepted, None);

        // Populate 3 extra identity graphs with 100 unreviewed proposals each (300 total).
        for (gi, g) in extras.iter().enumerate() {
            let mut o = ProposeOptions::new(n("urn:principal:matcher"));
            o.identity_graph = Some(n(g));
            let mut identities = Vec::with_capacity(100);
            for i in 0..100 {
                identities.push(IdentityInput {
                    version: RecordVersion::V1,
                    key: RecordKey::new(format!("p{i}")).unwrap(),
                    occurrence_key: RecordKey::new(format!("g{gi}-{i}")).unwrap(),
                    entities: vec![
                        n(&format!("urn:u:{gi}:{i}:a")),
                        n(&format!("urn:u:{gi}:{i}:b")),
                    ],
                    relation: IdentityRelation::PossibleMatch,
                    preferred: None,
                    attributed_author: n("urn:matcher"),
                    method: Some(MethodRef {
                        method: n("urn:method"),
                        version: RecordKey::new("1").unwrap(),
                    }),
                });
            }
            crate::evidence::propose(
                None,
                &ds,
                &ds,
                &EvidenceInput {
                    identities,
                    ..Default::default()
                },
                &o,
                &mut |_, _| Some(Arc::new(GraphAccess::all())),
                &Control::none(),
            )
            .unwrap();
        }
        // Add all 3 graphs to the boundary.
        crate::evidence::configure(
            &ds,
            &base_settings(vec![
                IDENTITY.into(),
                extras[0].into(),
                extras[1].into(),
                extras[2].into(),
            ]),
            &Control::none(),
        )
        .unwrap();

        // A tight 16 KiB preparation budget would overflow `Reads.ids` if each proposal
        // were cached, returning BudgetExceeded (HTTP 507); streaming physical IDs keeps
        // the map available.
        let res = resolve_with_reads_bytes(
            &ds,
            &ds,
            "people",
            &[n("urn:a")],
            Some(Arc::new(GraphAccess::all())),
            &Control::none(),
            16 * 1024,
        )
        .unwrap();
        assert_eq!(res.entities[0].representative, n("urn:a"));
        assert_eq!(res.entities[0].members, vec![n("urn:a"), n("urn:b")]);
    }
}
