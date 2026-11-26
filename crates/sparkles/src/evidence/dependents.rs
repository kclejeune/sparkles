//! The dependents of changed evidence records (spec C25 §6, "Reading reports", and F22 §5).
//!
//! An assessment depends on the record it evaluates, its `ev:target`, and on every
//! record it names as `prov:used`. A community report in a guarded reports graph
//! depends on every record its generating activity names as `prov:used`, and a person's
//! edit of a report (`gr:editOf`) inherits the edited report's inputs. When a claim is
//! withdrawn, a decision is superseded, a support ends or a resolution map gets a new
//! revision, the operation answers the identities it changed. [`dependents`] finds the
//! authenticated assessments and the readable community reports that depend on them, so
//! that a maintained analysis can be marked for review or recomputation. It decides
//! nothing about their conclusions, which may well still hold.

use super::{RDF_TYPE, State, check_store};
use crate::access::{GraphAccess, Preparation, PreparedView};
use crate::evidence::propose::{READS_BYTES, settings_of};
use crate::managed::{ManagedError, ManagedErrorKind};
use crate::task::Control;
use crate::{Dataset, Error, Result};
use oxrdf::{NamedNode, Term};
use sparkles_core::evidence::review::MAX_EVIDENCE;
use sparkles_core::evidence::write::Reads;
use sparkles_core::evidence::{RecordKind, Settings};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use uuid::Uuid;

const PROV_USED: &str = "http://www.w3.org/ns/prov#used";
const PROV_WAS_GENERATED_BY: &str = "http://www.w3.org/ns/prov#wasGeneratedBy";
const PROV_ACTIVITY: &str = "http://www.w3.org/ns/prov#Activity";
const GR_REPORT: &str = "urn:x-sparkles:graphrag#Report";
const GR_COMMUNITY: &str = "urn:x-sparkles:graphrag#community";
const GR_EDIT_OF: &str = "urn:x-sparkles:graphrag#editOf";
const GUARDED_PREFIX: &str = "urn:x-sparkles:graphrag:";
const DISCLOSURE_GRAPH: &str = "urn:x-sparkles:graphrag:disclosure";
const FOOTPRINT_PREFIX: &str = "urn:x-sparkles:graphrag:footprint:";

/// The most identities one request names.
pub const MAX_CHANGED: usize = 1000;

/// How a dependent depends on a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dependence {
    /// The record is the assessment's `ev:target`.
    Target,
    /// The assessment or report activity names the record as `prov:used`.
    Used,
}

impl Dependence {
    pub fn name(self) -> &'static str {
        match self {
            Dependence::Target => "target",
            Dependence::Used => "used",
        }
    }
}

/// One assessment that depends on a changed record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependent {
    pub assessment: NamedNode,
    pub graph: NamedNode,
    /// The record it depends on: a changed identity, or for a transitive dependent the
    /// assessment it depends on.
    pub on: NamedNode,
    pub dependence: Dependence,
    /// How many intermediate dependents lie between it and a changed identity, 0 for a
    /// direct one.
    pub depth: u32,
}

/// One community report in a guarded reports graph that depends on a changed record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportDependent {
    pub report: NamedNode,
    /// The community the report summarizes (`gr:community`), when recorded.
    pub community: Option<NamedNode>,
    /// The guarded reports graph holding the report.
    pub graph: NamedNode,
    /// The record it depends on: a changed identity, or for a transitive dependent the
    /// assessment or report it depends on.
    pub on: NamedNode,
    pub dependence: Dependence,
    /// How many intermediate dependents lie between it and a changed identity, 0 for a
    /// direct one.
    pub depth: u32,
}

/// The outcome of [`dependents`] and [`dependents_at`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependents {
    /// The commit the records were read at.
    pub commit: u64,
    pub dependents: Vec<Dependent>,
    /// The community reports in readable guarded reports graphs whose activity used a
    /// changed record (or a transitive dependent), each once at its smallest depth.
    pub reports: Vec<ReportDependent>,
}

/// Whether `iri` names a guarded reports graph under `urn:x-sparkles:graphrag:` other
/// than the disclosure footprints graph and footprint resource prefix (C22 §6.1).
fn is_reports_graph(iri: &str) -> bool {
    iri.starts_with(GUARDED_PREFIX)
        && iri.len() > GUARDED_PREFIX.len()
        && iri != DISCLOSURE_GRAPH
        && !iri.starts_with(FOOTPRINT_PREFIX)
}

#[derive(Clone, Copy)]
enum DependentsHead<'a> {
    Snapshot(&'a Arc<crate::store::Snapshot>),
    Evidence(&'a sparkles_core::store::BoundEvidenceView),
}

impl<'a> DependentsHead<'a> {
    fn validate_access(self, access: &GraphAccess) -> Result<()> {
        match self {
            Self::Snapshot(head) => access.validate_bound_snapshot(head),
            Self::Evidence(view) => view.validate_access(access),
        }
    }

    fn prepare_view(
        self,
        access: &Arc<GraphAccess>,
        preparation: &Preparation,
    ) -> Result<PreparedView> {
        let prepared = match self {
            Self::Snapshot(head) => access.masked_controlled(head, preparation),
            Self::Evidence(view) => view.prepare_view(access.clone(), preparation),
        };
        prepared.map_err(|e| match e {
            Error::Unsupported(m) => ManagedError::error(ManagedErrorKind::IncompleteEvidence, m),
            e => e,
        })
    }

    fn reads(self, preparation: &'a Preparation) -> Reads<'a> {
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

/// The authenticated assessments in readable proposal graphs and the community reports
/// in readable guarded reports graphs that depend on any of `changed`, at the head of
/// `logical`, which is `main` or one of its branches. With `transitive`, assessments
/// and reports that depend on a dependent are found too. `access` is what the caller
/// may read there, `None` for everything. Undecided C22 §6 guards on `access` are bound
/// against the snapshot being read under the shared preparation budget; an unproven or
/// unreadable graph is not searched, and a record any of whose triples the caller's
/// view hides is neither reported nor followed, so the answer discloses nothing hidden.
/// The bound counts only the triples the caller's view shows. Each dependent is
/// reported once, at its smallest depth, sorted by depth and IRI.
///
/// Refusals: invalid input, `unsupported-profile` when the profile is not enabled, and
/// `incomplete-evidence` when more than [`MAX_EVIDENCE`] candidates would be read.
pub fn dependents(
    main: &Dataset,
    logical: &Dataset,
    changed: &[NamedNode],
    transitive: bool,
    access: Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<Dependents> {
    control.check()?;
    check_store(main, logical)?;
    let (settings, _) = settings_of(main)?;
    let registry = main.store().managed_snapshot()?;
    let nonce = |g: &str| {
        registry.as_ref().and_then(|s| {
            s.graphs
                .owners
                .get(&crate::managed::OwnerKind::Evidence)
                .filter(|e| e.graphs.iter().any(|x| x == g))
                .and_then(|e| e.protections.get(g).copied())
        })
    };
    let head = logical.store().snapshot();
    dependents_in(
        &settings,
        &nonce,
        DependentsHead::Snapshot(&head),
        changed,
        transitive,
        access,
        control,
    )
}

/// [`dependents`] evaluated against a historical [`sparkles_core::store::BoundEvidenceView`],
/// using the view's recorded evidence configuration, protection nonces and snapshot.
/// Any undecided C22 §6 guard on `access` is bound against the view's exact snapshot.
pub fn dependents_at(
    view: &sparkles_core::store::BoundEvidenceView,
    changed: &[NamedNode],
    transitive: bool,
    access: Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<Dependents> {
    control.check()?;
    let settings = super::claims::view_settings(view)?;
    let owner = view.owner(crate::managed::OwnerKind::Evidence);
    let nonce = |g: &str| -> Option<Uuid> {
        owner
            .filter(|e| e.graphs.iter().any(|x| x == g))
            .and_then(|e| e.protections.get(g).copied())
    };
    dependents_in(
        &settings,
        &nonce,
        DependentsHead::Evidence(view),
        changed,
        transitive,
        access,
        control,
    )
}

#[allow(clippy::too_many_arguments)]
fn dependents_in(
    settings: &Settings,
    nonce: &dyn Fn(&str) -> Option<Uuid>,
    head: DependentsHead<'_>,
    changed: &[NamedNode],
    transitive: bool,
    access: Option<Arc<GraphAccess>>,
    control: &Control,
) -> Result<Dependents> {
    control.check()?;
    if changed.is_empty() || changed.len() > MAX_CHANGED {
        return Err(Error::invalid(format!(
            "changed: a request names 1 to {MAX_CHANGED} identities"
        )));
    }
    if let Some(a) = access.as_deref() {
        head.validate_access(a)?;
    }
    let preparation = Preparation::new(control.clone(), READS_BYTES)?;
    // Bind any undecided C22 §6 guard and prepare any triple mask against this exact
    // snapshot under the shared preparation budget.
    let prepared = match access.as_ref() {
        Some(a) if a.hides_triples() || a.read.undecided() || a.write.undecided() => {
            Some(head.prepare_view(a, &preparation)?)
        }
        _ => None,
    };
    let allows_read = |iri: &str| -> bool {
        if let Some(v) = prepared.as_ref() {
            v.allows_read_iri(iri)
        } else if let Some(a) = access.as_deref() {
            a.read.allows_iri(iri)
        } else {
            true
        }
    };
    let graphs: Vec<(NamedNode, Option<Uuid>)> = settings
        .proposals
        .iter()
        .filter_map(|g| NamedNode::new(g.as_str()).ok())
        .filter(|g| allows_read(g.as_str()))
        .map(|g| {
            let n = nonce(g.as_str());
            (g, n)
        })
        .collect();
    // Records authenticate at the unmasked head, and a record the caller's view hides any
    // triple of is left out, since authentication reads only its receipt and protection.
    let mut state = State {
        reads: head.reads(&preparation),
        seen: prepared
            .as_ref()
            .filter(|v| v.hides_triples())
            .map(|v| Reads::from_prepared(&preparation, v)),
        claim_graphs: Vec::new(),
        superseded: BTreeSet::new(),
        withdrawn: BTreeSet::new(),
    };
    let mut found: BTreeMap<NamedNode, Dependent> = BTreeMap::new();
    let mut found_reports: BTreeMap<NamedNode, ReportDependent> = BTreeMap::new();
    let mut queue: VecDeque<(NamedNode, u32)> = changed.iter().map(|c| (c.clone(), 0)).collect();
    let mut asked: BTreeSet<NamedNode> = changed.iter().cloned().collect();
    let mut read = 0usize;
    let activity_type = Term::NamedNode(NamedNode::new_unchecked(PROV_ACTIVITY));
    let report_type = Term::NamedNode(NamedNode::new_unchecked(GR_REPORT));
    while let Some((on, depth)) = queue.pop_front() {
        control.check()?;
        let o = Term::NamedNode(on.clone());
        // 1. Authenticated assessments in readable proposal graphs.
        for g in &graphs {
            for (p, dependence) in [
                (super::ev("target"), Dependence::Target),
                (PROV_USED.to_string(), Dependence::Used),
            ] {
                let remaining = MAX_EVIDENCE.saturating_sub(read);
                // Candidates come from the caller's view, so the bound counts only triples
                // the caller can read.
                let (candidates, more) = match state.seen.as_mut() {
                    Some(seen) => seen.subjects_up_to(&p, &o, g.0.as_str(), remaining)?,
                    None => state
                        .reads
                        .subjects_up_to(&p, &o, g.0.as_str(), remaining)?,
                };
                read += candidates.len();
                if more || read > MAX_EVIDENCE {
                    return Err(super::incomplete("the dependents of the changed records"));
                }
                for a in candidates {
                    if found.contains_key(&a)
                        || state.masked(&a, &g.0)?
                        || !state.typed(&a, g, RecordKind::Assessment)?
                    {
                        continue;
                    }
                    found.insert(
                        a.clone(),
                        Dependent {
                            assessment: a.clone(),
                            graph: g.0.clone(),
                            on: on.clone(),
                            dependence,
                            depth,
                        },
                    );
                    if transitive && asked.insert(a.clone()) {
                        queue.push_back((a, depth + 1));
                    }
                }
            }
        }
        // 2. Community reports in readable guarded reports graphs whose generating
        //    activity recorded `prov:used <on>` (plus human edits via `gr:editOf`).
        let remaining = MAX_EVIDENCE.saturating_sub(read);
        let (mut activities, more) = match state.seen.as_mut() {
            Some(seen) => seen.subjects_in_graphs_up_to(PROV_USED, &o, remaining, |g| {
                is_reports_graph(g) && allows_read(g)
            })?,
            None => state
                .reads
                .subjects_in_graphs_up_to(PROV_USED, &o, remaining, |g| {
                    is_reports_graph(g) && allows_read(g)
                })?,
        };
        read += activities.len();
        if more || read > MAX_EVIDENCE {
            return Err(super::incomplete("the dependents of the changed records"));
        }
        activities.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
        for (act, g) in activities {
            control.check()?;
            if state.masked(&act, &g)? {
                continue;
            }
            let is_activity = match state.seen.as_mut() {
                Some(seen) => seen.contains(act.as_str(), RDF_TYPE, &activity_type, g.as_str())?,
                None => state
                    .reads
                    .contains(act.as_str(), RDF_TYPE, &activity_type, g.as_str())?,
            };
            if !is_activity {
                continue;
            }
            let remaining = MAX_EVIDENCE.saturating_sub(read);
            let act_term = Term::NamedNode(act);
            let (mut reps, more) = match state.seen.as_mut() {
                Some(seen) => {
                    seen.subjects_up_to(PROV_WAS_GENERATED_BY, &act_term, g.as_str(), remaining)?
                }
                None => state.reads.subjects_up_to(
                    PROV_WAS_GENERATED_BY,
                    &act_term,
                    g.as_str(),
                    remaining,
                )?,
            };
            read += reps.len();
            if more || read > MAX_EVIDENCE {
                return Err(super::incomplete("the dependents of the changed records"));
            }
            reps.sort();
            for rep in reps {
                record_report_and_edits(
                    &mut state,
                    &rep,
                    &g,
                    &on,
                    depth,
                    transitive,
                    &report_type,
                    &mut read,
                    &mut found_reports,
                    &mut asked,
                    &mut queue,
                    control,
                )?;
            }
        }
        // 3. Reports that directly edited `<on>` via `gr:editOf` (for example when
        //    `changed` names a report IRI directly).
        let remaining = MAX_EVIDENCE.saturating_sub(read);
        let (mut direct_edits, more) = match state.seen.as_mut() {
            Some(seen) => seen.subjects_in_graphs_up_to(GR_EDIT_OF, &o, remaining, |g| {
                is_reports_graph(g) && allows_read(g)
            })?,
            None => state
                .reads
                .subjects_in_graphs_up_to(GR_EDIT_OF, &o, remaining, |g| {
                    is_reports_graph(g) && allows_read(g)
                })?,
        };
        read += direct_edits.len();
        if more || read > MAX_EVIDENCE {
            return Err(super::incomplete("the dependents of the changed records"));
        }
        direct_edits.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
        for (edit, g) in direct_edits {
            control.check()?;
            record_report_and_edits(
                &mut state,
                &edit,
                &g,
                &on,
                depth,
                transitive,
                &report_type,
                &mut read,
                &mut found_reports,
                &mut asked,
                &mut queue,
                control,
            )?;
        }
    }
    let mut dependents: Vec<Dependent> = found.into_values().collect();
    dependents.sort_by(|a, b| (a.depth, &a.assessment).cmp(&(b.depth, &b.assessment)));
    let mut reports: Vec<ReportDependent> = found_reports.into_values().collect();
    reports.sort_by(|a, b| (a.depth, &a.report, &a.graph).cmp(&(b.depth, &b.report, &b.graph)));
    Ok(Dependents {
        commit: head.commit(),
        dependents,
        reports,
    })
}

/// Record `report` in `graph` when it is an unmasked `gr:Report`, and follow any
/// `gr:editOf` chain of person-edited reports that inherit its activity at the same
/// depth.
#[allow(clippy::too_many_arguments)]
fn record_report_and_edits(
    state: &mut State<'_>,
    report: &NamedNode,
    graph: &NamedNode,
    on: &NamedNode,
    depth: u32,
    transitive: bool,
    report_type: &Term,
    read: &mut usize,
    found_reports: &mut BTreeMap<NamedNode, ReportDependent>,
    asked: &mut BTreeSet<NamedNode>,
    queue: &mut VecDeque<(NamedNode, u32)>,
    control: &Control,
) -> Result<()> {
    if found_reports.contains_key(report) || state.masked(report, graph)? {
        return Ok(());
    }
    let is_report = |state: &mut State<'_>, r: &NamedNode| -> Result<bool> {
        match state.seen.as_mut() {
            Some(seen) => seen.contains(r.as_str(), RDF_TYPE, report_type, graph.as_str()),
            None => state
                .reads
                .contains(r.as_str(), RDF_TYPE, report_type, graph.as_str()),
        }
    };
    if !is_report(state, report)? {
        return Ok(());
    }
    let community_of = |state: &mut State<'_>, r: &NamedNode| -> Result<Option<NamedNode>> {
        let term = match state.seen.as_mut() {
            Some(seen) => seen.single(r.as_str(), GR_COMMUNITY, graph.as_str())?,
            None => state
                .reads
                .single(r.as_str(), GR_COMMUNITY, graph.as_str())?,
        };
        Ok(match term {
            Some(Term::NamedNode(n)) => Some(n),
            _ => None,
        })
    };
    let community = community_of(state, report)?;
    found_reports.insert(
        report.clone(),
        ReportDependent {
            report: report.clone(),
            community,
            graph: graph.clone(),
            on: on.clone(),
            dependence: Dependence::Used,
            depth,
        },
    );
    if transitive && asked.insert(report.clone()) {
        queue.push_back((report.clone(), depth + 1));
    }
    // Person-edited reports (`gr:editOf`) carry no generating activity of their own;
    // their provenance resolves through `gr:editOf+` to the edited report's activity.
    let mut edit_queue = VecDeque::from([report.clone()]);
    let mut seen_edits = BTreeSet::from([report.clone()]);
    while let Some(prev) = edit_queue.pop_front() {
        control.check()?;
        let remaining = MAX_EVIDENCE.saturating_sub(*read);
        let prev_term = Term::NamedNode(prev);
        let (mut edits, more) = match state.seen.as_mut() {
            Some(seen) => seen.subjects_up_to(GR_EDIT_OF, &prev_term, graph.as_str(), remaining)?,
            None => {
                state
                    .reads
                    .subjects_up_to(GR_EDIT_OF, &prev_term, graph.as_str(), remaining)?
            }
        };
        *read += edits.len();
        if more || *read > MAX_EVIDENCE {
            return Err(super::incomplete("the dependents of the changed records"));
        }
        edits.sort();
        for edit in edits {
            if !seen_edits.insert(edit.clone())
                || found_reports.contains_key(&edit)
                || state.masked(&edit, graph)?
                || !is_report(state, &edit)?
            {
                continue;
            }
            let community = community_of(state, &edit)?;
            found_reports.insert(
                edit.clone(),
                ReportDependent {
                    report: edit.clone(),
                    community,
                    graph: graph.clone(),
                    on: on.clone(),
                    dependence: Dependence::Used,
                    depth,
                },
            );
            if transitive && asked.insert(edit.clone()) {
                queue.push_back((edit.clone(), depth + 1));
            }
            edit_queue.push_back(edit);
        }
    }
    Ok(())
}
