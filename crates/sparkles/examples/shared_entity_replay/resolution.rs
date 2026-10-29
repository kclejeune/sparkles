//! Set-based replay of explicitly supplied offline review declarations.
//! Authored sr:Assignment/sr:Conflict records are deliberately never consumed.
use super::model::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Disposition {
    Accepted,
    Rejected,
    NeedsEvidence,
}
pub(crate) struct Decision {
    pub id: String,
    pub target: String,
    pub disposition: Disposition,
    pub supersedes: Vec<String>,
    digest: String,
}

/// Validate the selected immutable history and return all unsuperseded terminal
/// supports, without discarding equal outcomes or choosing by arrival/time.
pub(crate) fn terminal_decisions(
    graph: &Graph,
    selected: &[String],
    policy: Option<&str>,
    operation: &Operation<'_>,
) -> ReplayResult<BTreeMap<String, Vec<Decision>>> {
    let mut decisions = BTreeMap::new();
    for id in selected {
        operation.check()?;
        if !graph.has_type(id, &format!("{EV}ReviewDecision")) {
            return Err(Failure::UnresolvedReference);
        }
        if let Some(policy) = policy
            && (graph.one_iri(id, &format!("{EV}reviewPolicy"))? != policy
                || graph.literal(id, &format!("{EV}policyVersion"))? != "1")
        {
            return Err(Failure::UnsupportedProfile);
        }
        let disposition = match graph.one_iri(id, &format!("{EV}disposition"))?.as_str() {
            "urn:x-sparkles:evidence:Accepted" => Disposition::Accepted,
            "urn:x-sparkles:evidence:Rejected" => Disposition::Rejected,
            "urn:x-sparkles:evidence:NeedsEvidence" => Disposition::NeedsEvidence,
            _ => return Err(Failure::InvalidDomainRecord),
        };
        let digest = graph.literal(id, &format!("{EV}targetDigest"))?;
        if digest.len() != 71
            || !digest.starts_with("sha256:")
            || !digest[7..]
                .bytes()
                .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
        {
            return Err(Failure::InvalidDomainRecord);
        }
        decisions.insert(
            id.clone(),
            Decision {
                id: id.clone(),
                target: graph.one_iri(id, &format!("{EV}target"))?,
                disposition,
                supersedes: graph.iris(id, PROV_REVISION)?,
                digest,
            },
        );
    }
    // Iterative DFS avoids stack growth for a caller-sized successor chain.
    let mut colour = BTreeMap::<&str, u8>::new();
    let mut superseded = BTreeSet::new();
    for id in decisions.keys() {
        let mut work = vec![(id.as_str(), false)];
        while let Some((id, exiting)) = work.pop() {
            operation.check()?;
            if exiting {
                colour.insert(id, 2);
                continue;
            }
            match colour.get(id) {
                Some(1) => return Err(Failure::DecisionCycle),
                Some(2) => continue,
                _ => {}
            }
            colour.insert(id, 1);
            work.push((id, true));
            let decision = &decisions[id];
            for parent in &decision.supersedes {
                operation.check()?;
                let old = decisions.get(parent).ok_or(Failure::UnresolvedReference)?;
                if old.target != decision.target || old.digest != decision.digest {
                    return Err(Failure::InvalidDomainRecord);
                }
                superseded.insert(parent.clone());
                work.push((parent, false));
            }
        }
    }
    let mut result = BTreeMap::<String, Vec<Decision>>::new();
    for (id, decision) in decisions {
        operation.check()?;
        if !superseded.contains(id.as_str()) {
            result
                .entry(decision.target.clone())
                .or_default()
                .push(decision);
        }
    }
    Ok(result)
}

struct Union {
    parent: Vec<usize>,
}
impl Union {
    fn root(&mut self, mut value: usize, operation: &Operation<'_>) -> ReplayResult<usize> {
        while self.parent[value] != value {
            operation.check()?;
            self.parent[value] = self.parent[self.parent[value]];
            value = self.parent[value];
        }
        Ok(value)
    }
    fn join(&mut self, a: usize, b: usize, operation: &Operation<'_>) -> ReplayResult<()> {
        let a = self.root(a, operation)?;
        let b = self.root(b, operation)?;
        if a != b {
            self.parent[a.max(b)] = a.min(b);
        }
        Ok(())
    }
}
struct Proposal {
    members: Vec<usize>,
    positive: bool,
    separate: bool,
    contradictory: bool,
    decisions: Vec<String>,
    accepted_decisions: Vec<String>,
}

fn profile(graph: &Graph, id: &str) -> ReplayResult<()> {
    if graph.literal(id, &format!("{SR}profileVersion"))? != "1" {
        return Err(Failure::UnsupportedProfile);
    }
    Ok(())
}
fn policy(graph: &Graph, id: &str) -> ReplayResult<()> {
    profile(graph, id)?;
    for (predicate, expected) in [
        ("positiveMatchRule", "AcceptedPossibleMatch"),
        ("separationRule", "PairwiseSeparation"),
        ("conflictRule", "WholeComponentConflict"),
        ("kindRule", "ExactKind"),
        ("representativeRule", "LeastUtf8Iri"),
        ("stringRule", "ScalarJson1"),
    ] {
        if graph.one_iri(id, &format!("{SR}{predicate}"))? != format!("{SR}{expected}") {
            return Err(Failure::UnsupportedProfile);
        }
    }
    Ok(())
}

pub(crate) fn resolve(
    graph: &Graph,
    view: &str,
    operation: &Operation<'_>,
) -> ReplayResult<ComputedComparison> {
    operation.check()?;
    // One conservative workspace admission covers temporary indexes and property
    // copies. Per-assignment output support lists are charged separately below.
    let count = graph.records.values().try_fold(0usize, |n, r| {
        n.checked_add(r.len()).ok_or(Failure::ReplayLimit)
    })?;
    let _workspace = operation.charge(
        count
            .checked_mul(512)
            .and_then(|v| v.checked_add(4096))
            .ok_or(Failure::ReplayLimit)?,
    )?;
    graph.validate_domain(operation)?;
    if !graph.has_type(view, &format!("{SR}ResolutionView")) {
        return Err(Failure::UnresolvedReference);
    }
    profile(graph, view)?;
    let scope = graph.one_iri(view, &format!("{SR}domainScope"))?;
    let matching = graph.one_iri(view, &format!("{SR}matchingPolicy"))?;
    policy(graph, &matching)?;
    let manifest = graph.one_iri(view, &format!("{SR}inputManifest"))?;
    if !graph.has_type(&manifest, &format!("{SR}InputManifest")) {
        return Err(Failure::UnresolvedReference);
    }
    profile(graph, &manifest)?;
    graph.verify_manifest(&manifest, operation)?;
    let ids: BTreeSet<_> = graph
        .iris(&manifest, &format!("{SR}inputId"))?
        .into_iter()
        .collect();
    if ids.contains(&manifest) {
        return Err(Failure::InvalidDomainRecord);
    }
    let mut external = BTreeSet::new();
    for field in [
        "ontologyVersion",
        "mappingVersion",
        "normalizationVersion",
        "recordedView",
    ] {
        let id = graph.one_iri(view, &format!("{SR}{field}"))?;
        if !ids.contains(&id) {
            return Err(Failure::IncompleteInput);
        }
        external.insert(id);
    }
    if !ids.contains(&matching) {
        return Err(Failure::IncompleteInput);
    }
    for id in &ids {
        operation.check()?;
        if !graph.records.contains_key(id) && !external.contains(id) {
            return Err(Failure::IncompleteInput);
        }
    }
    let mut entity_ids = Vec::new();
    let mut kinds = Vec::new();
    for id in &ids {
        operation.check()?;
        if graph.has_type(id, &format!("{SR}Entity")) {
            profile(graph, id)?;
            if graph.one_iri(id, &format!("{SR}domainScope"))? != scope {
                return Err(Failure::ScopeMismatch);
            }
            entity_ids.push(id.clone());
            kinds.push(graph.one_iri(id, &format!("{SR}entityKind"))?);
        }
    }
    if entity_ids.len() > operation.limits.entities {
        return Err(Failure::ReplayLimit);
    }
    let index: BTreeMap<_, _> = entity_ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    let decisions = graph.iris(view, &format!("{SR}decision"))?;
    if decisions.len() > operation.limits.decisions {
        return Err(Failure::ReplayLimit);
    }
    if decisions.iter().any(|id| !ids.contains(id)) {
        return Err(Failure::IncompleteInput);
    }
    let mut terminal = terminal_decisions(graph, &decisions, Some(&matching), operation)?;
    let proposal_ids = graph.iris(view, &format!("{SR}proposal"))?;
    let mut unknowns = Vec::new();
    let mut proposals = Vec::new();
    let mut union = Union {
        parent: (0..entity_ids.len()).collect(),
    };
    for id in proposal_ids {
        operation.check()?;
        if !ids.contains(&id) || !graph.has_type(&id, &format!("{EV}IdentityProposal")) {
            return Err(Failure::UnresolvedReference);
        }
        let members = graph
            .iris(&id, &format!("{EV}entity"))?
            .into_iter()
            .map(|id| {
                if let Some(index) = index.get(id.as_str()) {
                    return Ok(*index);
                }
                if !graph.records.contains_key(&id) {
                    return Err(Failure::UnresolvedReference);
                }
                if graph.one_iri(&id, &format!("{SR}domainScope"))? != scope {
                    return Err(Failure::ScopeMismatch);
                }
                Err(Failure::IncompleteInput)
            })
            .collect::<ReplayResult<Vec<_>>>()?;
        if members.len() < 2 {
            return Err(Failure::InvalidDomainRecord);
        }
        let relation = graph.one_iri(&id, &format!("{EV}relation"))?;
        if relation != format!("{EV}PossibleMatch") && relation != format!("{EV}DistinctEntities") {
            return Err(Failure::InvalidDomainRecord);
        }
        let outcomes = terminal.remove(&id).unwrap_or_default();
        let dispositions: BTreeSet<_> = outcomes.iter().map(|d| d.disposition).collect();
        let accepted = dispositions.contains(&Disposition::Accepted);
        let positive = relation == format!("{EV}PossibleMatch") && accepted;
        let separate = relation == format!("{EV}DistinctEntities") && accepted;
        if (outcomes.is_empty() || dispositions == BTreeSet::from([Disposition::NeedsEvidence]))
            && relation == format!("{EV}PossibleMatch")
        {
            unknowns.push(Unknown {
                record: id.clone(),
                code: UnknownCode::PossibleMatch,
            });
        }
        if positive {
            for member in &members[1..] {
                union.join(members[0], *member, operation)?;
            }
        }
        let accepted_decisions = outcomes
            .iter()
            .filter(|d| d.disposition == Disposition::Accepted)
            .map(|d| d.id.clone())
            .collect();
        proposals.push(Proposal {
            members,
            positive,
            separate,
            contradictory: dispositions.len() > 1,
            decisions: outcomes.into_iter().map(|d| d.id).collect(),
            accepted_decisions,
        });
    }
    if !terminal.is_empty() {
        return Err(Failure::UnresolvedReference);
    }
    let mut components = BTreeMap::<usize, Vec<usize>>::new();
    for i in 0..entity_ids.len() {
        let root = union.root(i, operation)?;
        components.entry(root).or_default().push(i);
    }
    let mut positive = BTreeMap::<usize, BTreeSet<String>>::new();
    let mut causes = BTreeMap::<(usize, ConflictCode), BTreeSet<String>>::new();
    for proposal in proposals {
        operation.check()?;
        let mut touched = BTreeMap::<usize, usize>::new();
        for member in proposal.members {
            *touched.entry(union.root(member, operation)?).or_default() += 1;
        }
        for (root, count) in touched {
            operation.check()?;
            if proposal.positive {
                positive
                    .entry(root)
                    .or_default()
                    .extend(proposal.accepted_decisions.iter().cloned());
            }
            if proposal.contradictory {
                causes
                    .entry((root, ConflictCode::Decision))
                    .or_default()
                    .extend(proposal.decisions.iter().cloned());
            }
            if proposal.separate && count > 1 {
                causes
                    .entry((root, ConflictCode::Distinct))
                    .or_default()
                    .extend(proposal.accepted_decisions.iter().cloned());
            }
        }
    }
    for (root, members) in &components {
        operation.check()?;
        if members.iter().any(|i| kinds[*i] != kinds[members[0]]) {
            causes.entry((*root, ConflictCode::Kind)).or_default();
        }
    }
    for ((root, _), decisions) in &mut causes {
        if let Some(connecting) = positive.get(root) {
            decisions.extend(connecting.iter().cloned());
        }
    }
    let mut assignments = Vec::new();
    let mut conflicts = Vec::new();
    let mut charges = Vec::new();
    for (root, members) in components {
        operation.check()?;
        let codes: Vec<_> = causes
            .range((root, ConflictCode::Decision)..=(root, ConflictCode::Kind))
            .map(|((_, code), d)| (*code, d))
            .collect();
        let unresolved = !codes.is_empty();
        let support: BTreeSet<_> = if unresolved {
            codes
                .iter()
                .flat_map(|(_, ids)| ids.iter().cloned())
                .collect()
        } else {
            positive.get(&root).cloned().unwrap_or_default()
        };
        let support_bytes = support.iter().try_fold(0usize, |n, s| {
            n.checked_add(s.len() + 64).ok_or(Failure::ReplayLimit)
        })?;
        for member in &members {
            operation.check()?;
            charges.push(
                operation.charge(
                    support_bytes
                        .checked_add(entity_ids[*member].len() + entity_ids[members[0]].len() + 256)
                        .ok_or(Failure::ReplayLimit)?,
                )?,
            );
            assignments.push(Assignment {
                entity: entity_ids[*member].clone(),
                status: if unresolved {
                    Status::Unresolved
                } else {
                    Status::Resolved
                },
                representative: (!unresolved).then(|| entity_ids[members[0]].clone()),
                decisions: support.iter().cloned().collect(),
            });
        }
        for (code, decisions) in codes {
            let bytes = members
                .iter()
                .map(|i| entity_ids[*i].len() + 64)
                .sum::<usize>()
                .checked_add(decisions.iter().map(|s| s.len() + 64).sum::<usize>())
                .ok_or(Failure::ReplayLimit)?;
            charges.push(operation.charge(bytes + 256)?);
            conflicts.push(Conflict {
                code,
                entities: members.iter().map(|i| entity_ids[*i].clone()).collect(),
                decisions: decisions.iter().cloned().collect(),
            });
        }
    }
    let mut input_ids: Vec<_> = ids.into_iter().collect();
    input_ids.push(manifest);
    charges.push(operation.charge(
        input_ids.iter().map(|s| s.len() + 64).sum::<usize>()
            + unknowns.iter().map(|s| s.record.len() + 128).sum::<usize>()
            + scope.len()
            + 256,
    )?);
    operation.check()?;
    Ok(ComputedComparison {
        comparison: Comparison {
            profile_version: 1,
            scope,
            input_ids,
            assignments,
            conflicts,
            unknowns,
        },
        charges,
    })
}

/// A separately selected public/private input scope cannot borrow the result of
/// a differently scoped complete view. This is offline isolation, not real auth.
pub(crate) fn resolve_in_scope(
    graph: &Graph,
    view: &str,
    requested_scope: &str,
    operation: &Operation<'_>,
) -> ReplayResult<ComputedComparison> {
    operation.check()?;
    if graph.one_iri(view, &format!("{SR}domainScope"))? != requested_scope {
        return Err(Failure::ScopeMismatch);
    }
    resolve(graph, view, operation)
}
