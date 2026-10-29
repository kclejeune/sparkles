//! Versioned offline interpretation, never authenticated admission or publication.
use super::model::*;
use super::resolution::{Disposition, terminal_decisions};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Selection<T> {
    Known(T),
    Unknown(&'static str),
}

/// The supported fixture rule admits exact occurrences only when all selected
/// unsuperseded decisions agree on acceptance. A withdrawal is not a contrary fact.
/// The policy IRI/version is supplied explicitly, not inferred from a projection.
pub(crate) fn admitted_occurrences(
    graph: &Graph,
    view: &str,
    declared_policy: &str,
    policy_version: u32,
    operation: &Operation<'_>,
) -> ReplayResult<Selection<Vec<String>>> {
    operation.check()?;
    if policy_version != 1 {
        return Err(Failure::UnsupportedProfile);
    }
    let manifest = graph.one_iri(view, &format!("{SR}inputManifest"))?;
    let ids: BTreeSet<_> = graph
        .iris(&manifest, &format!("{SR}inputId"))?
        .into_iter()
        .collect();
    if !ids.contains(declared_policy) {
        return Err(Failure::IncompleteInput);
    }
    let decisions: Vec<_> = ids
        .iter()
        .filter(|id| graph.has_type(id, &format!("{EV}ReviewDecision")))
        .cloned()
        .collect();
    let statements = graph.records.values().map(|r| r.len()).sum::<usize>();
    let _workspace = operation.charge(
        statements
            .checked_mul(256)
            .and_then(|n| n.checked_add(4096))
            .ok_or(Failure::ReplayLimit)?,
    )?;
    let terminal = terminal_decisions(graph, &decisions, Some(declared_policy), operation)?;
    let mut admitted = Vec::new();
    for (target, supports) in terminal {
        operation.check()?;
        if !ids.contains(&target) || !graph.has_type(&target, &format!("{EV}Claim")) {
            return Err(Failure::UnresolvedReference);
        }
        let dispositions: BTreeSet<_> = supports.iter().map(|s| s.disposition).collect();
        if dispositions.len() > 1 {
            return Ok(Selection::Unknown("ConflictingTerminalDecisions"));
        }
        if dispositions.contains(&Disposition::Accepted) {
            admitted.push(target);
        }
    }
    operation.check()?;
    Ok(Selection::Known(admitted))
}

/// Reject cycles and ambiguity using the declared revision graph rather than
/// publication time, lexical ID order or the numerical size of an observation.
pub(crate) fn terminal_revision(
    selected: &[String],
    successors: &[(String, String)],
    operation: &Operation<'_>,
) -> ReplayResult<Selection<String>> {
    let _workspace = operation.charge(
        selected.iter().map(|s| s.len() + 128).sum::<usize>()
            + successors
                .iter()
                .map(|(a, b)| a.len() + b.len() + 128)
                .sum::<usize>(),
    )?;
    let selected: BTreeSet<_> = selected.iter().map(String::as_str).collect();
    if selected.is_empty() {
        return Ok(Selection::Unknown("NoSelectedCandidate"));
    }
    let mut old = BTreeSet::new();
    let mut edges = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for (revision, prior) in successors {
        operation.check()?;
        if selected.contains(revision.as_str()) && selected.contains(prior.as_str()) {
            old.insert(prior.as_str());
            edges.entry(revision).or_default().push(prior);
        }
    }
    let mut colours = std::collections::BTreeMap::new();
    for start in &selected {
        let mut stack = vec![(*start, false)];
        while let Some((id, exit)) = stack.pop() {
            operation.check()?;
            if exit {
                colours.insert(id, 2);
                continue;
            }
            match colours.get(id) {
                Some(1) => return Ok(Selection::Unknown("RevisionCycle")),
                Some(2) => continue,
                _ => {}
            }
            colours.insert(id, 1);
            stack.push((id, true));
            for prior in edges.get(id).into_iter().flatten() {
                stack.push((*prior, false));
            }
        }
    }
    let current: Vec<_> = selected.difference(&old).copied().collect();
    Ok(match current.as_slice() {
        [id] => Selection::Known((*id).to_owned()),
        [] => Selection::Unknown("NoSelectedCandidate"),
        _ => Selection::Unknown("ConflictingTerminalRevisions"),
    })
}
