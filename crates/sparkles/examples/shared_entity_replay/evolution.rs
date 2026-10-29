//! Versioned offline interpretation, never authenticated admission or publication.
use super::model::*;
use super::resolution::{Disposition, terminal_decisions};
use std::collections::BTreeSet;

#[derive(Debug, PartialEq, Eq)]
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
) -> ReplayResult<Selection<Owned<Vec<String>>>> {
    operation.check()?;
    if policy_version != 1 || declared_policy != "urn:example:a01:A5:simulatedClaimAdmission:1" {
        return Err(Failure::UnsupportedProfile);
    }
    let workspace = operation.charge(
        graph
            .workspace_bytes()?
            .checked_mul(2)
            .ok_or(Failure::ReplayLimit)?,
    )?;
    let manifest = graph.one_iri(view, &format!("{SR}inputManifest"))?;
    graph.verify_manifest(&manifest, operation)?;
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
    Ok(Selection::Known(Owned {
        value: admitted,
        _charges: vec![workspace],
    }))
}

/// Reject cycles and ambiguity using the declared revision graph rather than
/// publication time, lexical ID order or the numerical size of an observation.
pub(crate) fn terminal_revision<'a>(
    selected: &'a [String],
    successors: &[(String, String)],
    operation: &Operation<'_>,
) -> ReplayResult<Selection<&'a str>> {
    operation.check()?;
    if selected.len() > operation.limits.entities || successors.len() > operation.limits.relations {
        return Err(Failure::ReplayLimit);
    }
    let _workspace = operation.charge(
        selected
            .iter()
            .try_fold(4096usize, |n, id| {
                n.checked_add(id.len() + 256).ok_or(Failure::ReplayLimit)
            })?
            .checked_add(successors.iter().try_fold(0usize, |n, (a, b)| {
                n.checked_add(a.len() + b.len() + 256)
                    .ok_or(Failure::ReplayLimit)
            })?)
            .ok_or(Failure::ReplayLimit)?,
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
    let current: Vec<_> = selected
        .iter()
        .filter(|id| !old.contains(*id))
        .copied()
        .collect();
    Ok(match current.as_slice() {
        [id] => Selection::Known(*id),
        [] => Selection::Unknown("NoSelectedCandidate"),
        _ => Selection::Unknown("ConflictingTerminalRevisions"),
    })
}

pub(crate) struct ObservationCandidate {
    pub revision: String,
    pub observation: String,
    pub relation_claim: String,
    pub value_claim: String,
}
pub(crate) struct SelectedObservation {
    pub revision: String,
    pub observation: String,
    pub episode: String,
    pub count: String,
    pub denominator_status: Option<String>,
    pub geometry_version: String,
    _charges: Vec<Charge>,
}

/// One explicitly versioned fixture/application rule, not a general clinical
/// revision policy. Candidate bindings are supplied inputs, never expected answers.
pub(crate) fn select_observation(
    graph: &Graph,
    view: &str,
    selection_profile: &str,
    candidates: &[ObservationCandidate],
    operation: &Operation<'_>,
) -> ReplayResult<Selection<SelectedObservation>> {
    operation.check()?;
    if candidates.len() > operation.limits.entities {
        return Err(Failure::ReplayLimit);
    }
    let _registry = operation.charge(
        graph
            .workspace_bytes()?
            .checked_mul(2)
            .ok_or(Failure::ReplayLimit)?,
    )?;
    let manifest = graph.one_iri(view, &format!("{SR}inputManifest"))?;
    graph.verify_manifest(&manifest, operation)?;
    let inputs: BTreeSet<_> = graph
        .iris(&manifest, &format!("{SR}inputId"))?
        .into_iter()
        .collect();
    if !inputs.contains(selection_profile) {
        return Err(Failure::IncompleteInput);
    }
    let app = "urn:example:a01:application:";
    if graph.one_iri(selection_profile, &format!("{app}revisionSelectionRule"))?
        != format!("{app}UniqueTerminalSelectedSourceRevision")
    {
        return Ok(Selection::Unknown("UnknownSelectionRule"));
    }
    let observations: BTreeSet<_> = graph
        .iris(selection_profile, &format!("{app}seriesObservation"))?
        .into_iter()
        .collect();
    let _workspace = operation.charge(candidates.iter().try_fold(4096usize, |n, c| {
        n.checked_add(
            c.revision.len()
                + c.observation.len()
                + c.relation_claim.len()
                + c.value_claim.len()
                + 1024,
        )
        .ok_or(Failure::ReplayLimit)
    })?)?;
    let mut eligible = std::collections::BTreeMap::new();
    let mut series = None;
    for candidate in candidates {
        operation.check()?;
        if !inputs.contains(&candidate.revision) {
            continue;
        }
        if !observations.contains(&candidate.observation)
            || !inputs.contains(&candidate.relation_claim)
            || !inputs.contains(&candidate.value_claim)
        {
            return Err(Failure::IncompleteInput);
        }
        graph.validate_control(&candidate.observation, "Entity", operation)?;
        let origin = "http://www.w3.org/ns/prov#wasDerivedFrom";
        if graph.one_iri(&candidate.relation_claim, origin)? != candidate.revision
            || graph.one_iri(&candidate.value_claim, origin)? != candidate.revision
        {
            return Err(Failure::InvalidDomainRecord);
        }
        let relationship = claim_triple(graph, &candidate.relation_claim)?;
        let value = claim_triple(graph, &candidate.value_claim)?;
        if relationship.subject.to_string() != format!("<{}>", candidate.observation)
            || relationship.predicate.as_str() != format!("{SR}ObservationAboutEpisode")
            || value.subject.to_string() != relationship.object.to_string()
            || value.predicate.as_str() != "urn:example:a01:reportedCumulativeCases"
        {
            return Err(Failure::InvalidDomainRecord);
        }
        let oxrdf::Term::NamedNode(episode) = &relationship.object else {
            return Err(Failure::InvalidDomainRecord);
        };
        let Some(count) = graph.optional_integer(&candidate.observation, "count")? else {
            return Ok(Selection::Unknown("IncompatibleObservationSeries"));
        };
        let oxrdf::Term::Literal(value) = &value.object else {
            return Err(Failure::InvalidDomainRecord);
        };
        if value.datatype().as_str() != "http://www.w3.org/2001/XMLSchema#integer"
            || value
                .value()
                .strip_prefix('+')
                .unwrap_or(value.value())
                .trim_start_matches('0')
                != count.trim_start_matches('0')
        {
            return Err(Failure::InvalidDomainRecord);
        }
        let mut key = vec![episode.as_str().to_owned()];
        for field in [
            "measure",
            "caseDefinition",
            "countMode",
            "population",
            "place",
            "geometryVersion",
            "reportingAuthority",
            "observedDuring",
        ] {
            key.push(graph.one_iri(&candidate.observation, &format!("{SR}{field}"))?);
        }
        if series.as_ref().is_some_and(|prior| prior != &key) {
            return Ok(Selection::Unknown("IncompatibleObservationSeries"));
        }
        series = Some(key);
        if let Some(old) = eligible.insert(
            candidate.revision.clone(),
            (candidate, episode.as_str().to_owned(), count),
        ) {
            if old.0.observation != candidate.observation
                || old.0.relation_claim != candidate.relation_claim
                || old.0.value_claim != candidate.value_claim
            {
                return Ok(Selection::Unknown("IncompatibleObservationSeries"));
            }
        }
    }
    let revisions: Vec<_> = eligible.keys().cloned().collect();
    let mut edges = Vec::new();
    for revision in &revisions {
        for old in graph.iris(revision, PROV_REVISION)? {
            edges.push((revision.clone(), old));
        }
    }
    let selected = match terminal_revision(&revisions, &edges, operation)? {
        Selection::Known(id) => id,
        Selection::Unknown(code) => return Ok(Selection::Unknown(code)),
    };
    let (candidate, episode, count) = eligible
        .remove(selected)
        .ok_or(Failure::UnresolvedReference)?;
    let denominator = graph.iris(&candidate.observation, &format!("{SR}denominatorStatus"))?;
    let geometry_version =
        graph.one_iri(&candidate.observation, &format!("{SR}geometryVersion"))?;
    let charge = operation.charge(
        candidate.revision.len()
            + candidate.observation.len()
            + episode.len()
            + count.len()
            + geometry_version.len()
            + denominator.iter().map(|v| v.len() + 64).sum::<usize>()
            + 1024,
    )?;
    operation.check()?;
    Ok(Selection::Known(SelectedObservation {
        revision: selected.to_owned(),
        observation: candidate.observation.clone(),
        episode,
        count,
        denominator_status: denominator.into_iter().next(),
        geometry_version,
        _charges: vec![charge],
    }))
}
fn claim_triple<'a>(graph: &'a Graph, id: &str) -> ReplayResult<&'a oxrdf::Triple> {
    if !graph.has_type(id, &format!("{EV}Claim")) {
        return Err(Failure::UnresolvedReference);
    }
    let terms = graph.objects(id, "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies");
    if terms.len() != 1 {
        return Err(Failure::InvalidDomainRecord);
    }
    match terms.into_iter().next().unwrap() {
        oxrdf::Term::Triple(triple) => Ok(triple),
        _ => Err(Failure::InvalidDomainRecord),
    }
}

/// The explicit occurrence IDs remain visible even when several sources share
/// one lineage. No independence score or vote count is inferred.
pub(crate) fn source_lineages(
    graph: &Graph,
    occurrences: &[String],
    operation: &Operation<'_>,
) -> ReplayResult<Selection<Owned<std::collections::BTreeMap<String, Vec<String>>>>> {
    if occurrences.len() > operation.limits.claims {
        return Err(Failure::ReplayLimit);
    }
    let workspace = operation.charge(
        graph
            .workspace_bytes()?
            .checked_mul(2)
            .and_then(|v| v.checked_add(occurrences.iter().map(|id| id.len() + 128).sum::<usize>()))
            .ok_or(Failure::ReplayLimit)?,
    )?;
    let mut groups = std::collections::BTreeMap::<String, Vec<String>>::new();
    for occurrence in occurrences {
        operation.check()?;
        if !graph.has_type(occurrence, &format!("{EV}Claim")) {
            return Err(Failure::UnresolvedReference);
        }
        let mut revision = graph.one_iri(occurrence, "http://www.w3.org/ns/prov#wasDerivedFrom")?;
        let mut visited = BTreeSet::new();
        let mut family = None;
        loop {
            operation.check()?;
            if !visited.insert(revision.clone()) {
                return Ok(Selection::Unknown("LineageCycle"));
            }
            if !graph.has_type(&revision, &format!("{EV}SourceRevision")) {
                return Err(Failure::UnresolvedReference);
            }
            let families = graph.iris(&revision, &format!("{EV}sourceFamily"))?;
            if families.len() > 1 {
                return Err(Failure::InvalidDomainRecord);
            }
            if let Some(current) = families.into_iter().next() {
                if family.as_ref().is_some_and(|old| old != &current) {
                    return Ok(Selection::Unknown("ConflictingSourceFamilies"));
                }
                family = Some(current);
            }
            let upstream = graph.iris(&revision, &format!("{EV}upstreamSource"))?;
            match upstream.as_slice() {
                [] => break,
                [next] => revision = next.clone(),
                _ => return Ok(Selection::Unknown("AmbiguousSourceLineage")),
            }
        }
        groups
            .entry(family.unwrap_or(revision))
            .or_default()
            .push(occurrence.clone());
    }
    for values in groups.values_mut() {
        values.sort();
        values.dedup();
    }
    Ok(Selection::Known(Owned {
        value: groups,
        _charges: vec![workspace],
    }))
}

/// Only this narrow UTC lexical profile is supported. These supplied availability
/// declarations describe the simulated fixture, never server knowledge history.
pub(crate) fn simulated_available(
    graph: &Graph,
    revision: &str,
    cutoff: &str,
    operation: &Operation<'_>,
) -> ReplayResult<Selection<bool>> {
    operation.check()?;
    let valid = |value: &str| {
        value.len() == 20 && value.ends_with('Z') && value.parse::<toml::value::Datetime>().is_ok()
    };
    if !valid(cutoff) {
        return Ok(Selection::Unknown("UnsupportedAvailabilityTime"));
    }
    let terms = graph.objects(revision, &format!("{EV}firstAvailableAt"));
    if terms.is_empty() {
        return Ok(Selection::Unknown("UnknownAvailability"));
    }
    if terms.len() != 1 {
        return Err(Failure::InvalidDomainRecord);
    }
    let oxrdf::Term::Literal(value) = terms.into_iter().next().unwrap() else {
        return Err(Failure::InvalidDomainRecord);
    };
    if value.datatype().as_str() != "http://www.w3.org/2001/XMLSchema#dateTime"
        || !valid(value.value())
    {
        return Ok(Selection::Unknown("UnsupportedAvailabilityTime"));
    }
    Ok(Selection::Known(value.value() <= cutoff))
}

/// Classify an explicitly declared mapping migration over the same retained
/// revisions. This never changes the old view or treats new mapping as correction.
pub(crate) fn mapping_migration(
    graph: &Graph,
    old_view: &str,
    new_view: &str,
    migration: &str,
    operation: &Operation<'_>,
) -> ReplayResult<Selection<bool>> {
    graph.validate_control(migration, "Migration", operation)?;
    if graph.one_iri(migration, &format!("{SR}method"))? != "urn:example:a01:mappingMigration1" {
        return Ok(Selection::Unknown("UnsupportedMapping"));
    }
    let old = graph.one_iri(old_view, &format!("{SR}mappingVersion"))?;
    let new = graph.one_iri(new_view, &format!("{SR}mappingVersion"))?;
    if old == new
        || graph.one_iri(migration, &format!("{SR}fromVersion"))? != old
        || graph.one_iri(migration, &format!("{SR}toVersion"))? != new
    {
        return Err(Failure::InvalidDomainRecord);
    }
    let sources = |view: &str| -> ReplayResult<BTreeSet<String>> {
        let manifest = graph.one_iri(view, &format!("{SR}inputManifest"))?;
        graph.verify_manifest(&manifest, operation)?;
        Ok(graph
            .iris(&manifest, &format!("{SR}inputId"))?
            .into_iter()
            .filter(|id| graph.has_type(id, &format!("{EV}SourceRevision")))
            .collect())
    };
    let old_sources = sources(old_view)?;
    let new_sources = sources(new_view)?;
    if old_sources.is_empty() || old_sources != new_sources {
        return Ok(Selection::Unknown("DifferentSourceRevisions"));
    }
    Ok(Selection::Known(true))
}

pub(crate) struct SoftwareBinding {
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    pub family_id: String,
    pub version_id: String,
}
pub(crate) struct SoftwareInventory {
    pub packages: Vec<(String, String)>,
    pub source_digest: String,
    // This adapter has no release/build/artifact/deployment evidence.
    pub deployed_exposure: Selection<bool>,
    _charges: Vec<Charge>,
}
/// The versioned offline adapter reuses supplied Cargo.lock bytes but requires
/// caller-owned identity bindings. Lock versions cannot establish deployment.
pub(crate) fn software_inventory(
    bytes: &[u8],
    adapter_version: u32,
    bindings: &[SoftwareBinding],
    operation: &Operation<'_>,
) -> ReplayResult<Selection<SoftwareInventory>> {
    operation.check()?;
    if adapter_version != 1 {
        return Ok(Selection::Unknown("UnsupportedMapping"));
    }
    if bytes.len() > operation.limits.source {
        return Err(Failure::ReplayLimit);
    }
    let _workspace = operation.charge(
        bytes
            .len()
            .checked_mul(16)
            .and_then(|v| v.checked_add(8192))
            .ok_or(Failure::ReplayLimit)?,
    )?;
    let table = std::str::from_utf8(bytes)
        .map_err(|_| Failure::InvalidDomainRecord)?
        .parse::<toml::Table>()
        .map_err(|_| Failure::InvalidDomainRecord)?;
    if table.get("version").and_then(toml::Value::as_integer) != Some(4) {
        return Ok(Selection::Unknown("UnsupportedMapping"));
    }
    let packages = table
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or(Failure::InvalidDomainRecord)?;
    if packages.len() > operation.limits.entities {
        return Err(Failure::ReplayLimit);
    }
    let mut result = Vec::new();
    let mut charges = Vec::new();
    for binding in bindings {
        operation.check()?;
        if oxrdf::NamedNode::new(&binding.family_id).is_err()
            || oxrdf::NamedNode::new(&binding.version_id).is_err()
            || binding.family_id == binding.version_id
        {
            return Err(Failure::InvalidDomainRecord);
        }
        let count = packages
            .iter()
            .filter(|p| {
                p.get("name").and_then(toml::Value::as_str) == Some(binding.name.as_str())
                    && p.get("version").and_then(toml::Value::as_str)
                        == Some(binding.version.as_str())
                    && p.get("source").and_then(toml::Value::as_str) == binding.source.as_deref()
            })
            .count();
        if count != 1 {
            return Ok(Selection::Unknown("UnresolvedPackageBinding"));
        }
        charges.push(operation.charge(binding.family_id.len() + binding.version_id.len() + 256)?);
        result.push((binding.family_id.clone(), binding.version_id.clone()));
    }
    result.sort();
    result.dedup();
    let digest = sparkles_core::evidence::Sha256Digest::of_bytes(bytes, operation.control)
        .map_err(|_| Failure::ReplayCancelled)?
        .to_string();
    charges.push(operation.charge(digest.len() + 256)?);
    Ok(Selection::Known(SoftwareInventory {
        packages: result,
        source_digest: digest,
        deployed_exposure: Selection::Unknown("NoDeploymentEvidence"),
        _charges: charges,
    }))
}
