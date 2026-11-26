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
use crate::access::{GraphAccess, Preparation, PreparedView, Reservation};
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

/// Per-entry overhead for retained `BTreeMap`, `BTreeSet` and `VecDeque` bookkeeping
/// beside owned IRI string bytes.
const MAP_ENTRY_BYTES: u64 = 192;
const SET_ENTRY_BYTES: u64 = 128;
const DEPENDENT_OVERHEAD: u64 = std::mem::size_of::<Dependent>() as u64 + MAP_ENTRY_BYTES;
const REPORT_DEPENDENT_OVERHEAD: u64 =
    std::mem::size_of::<ReportDependent>() as u64 + MAP_ENTRY_BYTES;

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
/// may read there, `None` for everything. Unrestricted policies (`None` or
/// [`GraphAccess::reads_everything`]) may read guarded reports graphs; restricted
/// policies must also carry a snapshot-bound guard (such as the C22 §6 disclosure
/// guard) whose pattern matches and exempts each guarded reports graph. Undecided read
/// guards and triple protections on `access` are bound against the snapshot being read
/// under the shared preparation budget; an unproven or unreadable graph is not searched,
/// and a record or activity any of whose triples the caller's view hides is neither
/// reported, counted against the bound nor followed, so the answer discloses nothing
/// hidden. The bound counts only the visible candidate records the caller's view shows.
/// Each dependent is reported once, at its smallest depth, sorted by depth and IRI.
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
/// Any undecided read guard on `access` is bound against the view's exact snapshot.
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
    dependents_in_with_budget(
        settings,
        nonce,
        head,
        changed,
        transitive,
        access,
        control,
        READS_BYTES,
    )
}

#[allow(clippy::too_many_arguments)]
fn dependents_in_with_budget(
    settings: &Settings,
    nonce: &dyn Fn(&str) -> Option<Uuid>,
    head: DependentsHead<'_>,
    changed: &[NamedNode],
    transitive: bool,
    access: Option<Arc<GraphAccess>>,
    control: &Control,
    memory_bytes: u64,
) -> Result<Dependents> {
    control.check()?;
    if !settings.enabled {
        return Err(ManagedError::error(
            ManagedErrorKind::UnsupportedProfile,
            "the evidence profile is not enabled in this dataset",
        ));
    }
    if changed.is_empty() || changed.len() > MAX_CHANGED {
        return Err(Error::invalid(format!(
            "changed: a request names 1 to {MAX_CHANGED} identities"
        )));
    }
    if let Some(a) = access.as_deref() {
        head.validate_access(a)?;
    }
    let preparation = Preparation::new(control.clone(), memory_bytes)?;
    // Bind any undecided read guard and prepare any triple mask against this exact
    // snapshot under the shared preparation budget.
    let prepared = match access.as_ref() {
        Some(a) if a.hides_triples() || a.read.undecided() => {
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
    let allows_reports_read = |iri: &str| -> bool {
        if let Some(v) = prepared.as_ref() {
            v.allows_guarded_read_iri(iri)
        } else if let Some(a) = access.as_deref() {
            a.allows_guarded_read_iri(iri)
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
    let skip_masked = state.seen.is_some();
    // Declared before the retained traversal and output maps so those drop before credit
    // releases back to `preparation`.
    let mut result_held = preparation.reserve(0)?;
    let mut found: BTreeMap<NamedNode, Dependent> = BTreeMap::new();
    let mut found_reports: BTreeMap<NamedNode, ReportDependent> = BTreeMap::new();
    let mut asked: BTreeSet<Arc<NamedNode>> = BTreeSet::new();
    let mut queue: VecDeque<(Arc<NamedNode>, u32)> = VecDeque::new();
    let mut expanded_edits: BTreeSet<(Arc<NamedNode>, Arc<NamedNode>)> = BTreeSet::new();
    for c in changed {
        if !asked.contains(c) {
            result_held.grow_by((c.as_str().len() as u64).saturating_add(SET_ENTRY_BYTES * 2))?;
            let node = Arc::new(c.clone());
            asked.insert(Arc::clone(&node));
            queue.push_back((node, 0));
        }
    }
    let mut read = 0usize;
    let activity_type = Term::NamedNode(NamedNode::new_unchecked(PROV_ACTIVITY));
    let report_type = Term::NamedNode(NamedNode::new_unchecked(GR_REPORT));
    while let Some((on, depth)) = queue.pop_front() {
        control.check()?;
        let _on_term_held = preparation.reserve(on.as_str().len() as u64)?;
        let o = Term::NamedNode((*on).clone());
        // 1. Authenticated assessments in readable proposal graphs.
        for g in &graphs {
            for (p, dependence) in [
                (super::ev("target"), Dependence::Target),
                (PROV_USED.to_string(), Dependence::Used),
            ] {
                let remaining = MAX_EVIDENCE.saturating_sub(read);
                // Candidates come from the caller's view with partially masked subjects
                // skipped before counting, so the bound counts only visible records.
                let scan = match state.seen.as_mut() {
                    Some(seen) => {
                        seen.visible_subjects_up_to(&p, &o, g.0.as_str(), remaining, true)?
                    }
                    None => state.reads.visible_subjects_up_to(
                        &p,
                        &o,
                        g.0.as_str(),
                        remaining,
                        false,
                    )?,
                };
                read += scan.items.len();
                if scan.more || read > MAX_EVIDENCE {
                    return Err(super::incomplete("the dependents of the changed records"));
                }
                for a in &scan.items {
                    if found.contains_key(a)
                        || state.masked(a, &g.0)?
                        || !state.typed(a, g, RecordKind::Assessment)?
                    {
                        continue;
                    }
                    let bytes = (a.as_str().len() as u64)
                        .saturating_mul(2)
                        .saturating_add(g.0.as_str().len() as u64)
                        .saturating_add(on.as_str().len() as u64)
                        .saturating_add(DEPENDENT_OVERHEAD);
                    result_held.grow_by(bytes)?;
                    found.insert(
                        a.clone(),
                        Dependent {
                            assessment: a.clone(),
                            graph: g.0.clone(),
                            on: (*on).clone(),
                            dependence,
                            depth,
                        },
                    );
                    if transitive && !asked.contains(a) {
                        result_held.grow_by(
                            (a.as_str().len() as u64).saturating_add(SET_ENTRY_BYTES * 2),
                        )?;
                        let node = Arc::new(a.clone());
                        asked.insert(Arc::clone(&node));
                        queue.push_back((node, depth + 1));
                    }
                }
            }
        }
        // 2. Community reports in readable guarded reports graphs whose generating
        //    activity recorded `prov:used <on>` (plus human edits via `gr:editOf`).
        let remaining = MAX_EVIDENCE.saturating_sub(read);
        let mut act_scan = match state.seen.as_mut() {
            Some(seen) => seen.subjects_in_graphs_up_to(PROV_USED, &o, remaining, true, |g| {
                is_reports_graph(g) && allows_reports_read(g)
            })?,
            None => state
                .reads
                .subjects_in_graphs_up_to(PROV_USED, &o, remaining, false, |g| {
                    is_reports_graph(g) && allows_reports_read(g)
                })?,
        };
        read += act_scan.items.len();
        if act_scan.more || read > MAX_EVIDENCE {
            return Err(super::incomplete("the dependents of the changed records"));
        }
        act_scan
            .items
            .sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0)));
        for (act, g) in &act_scan.items {
            control.check()?;
            if state.masked(act, g)? {
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
            let _act_term_held = preparation.reserve(act.as_str().len() as u64)?;
            let act_term = Term::NamedNode(act.clone());
            let mut rep_scan = match state.seen.as_mut() {
                Some(seen) => seen.visible_subjects_up_to(
                    PROV_WAS_GENERATED_BY,
                    &act_term,
                    g.as_str(),
                    remaining,
                    true,
                )?,
                None => state.reads.visible_subjects_up_to(
                    PROV_WAS_GENERATED_BY,
                    &act_term,
                    g.as_str(),
                    remaining,
                    false,
                )?,
            };
            read += rep_scan.items.len();
            if rep_scan.more || read > MAX_EVIDENCE {
                return Err(super::incomplete("the dependents of the changed records"));
            }
            rep_scan.items.sort();
            for rep in &rep_scan.items {
                record_report_and_edits(
                    &mut state,
                    &preparation,
                    &mut result_held,
                    rep,
                    g,
                    &on,
                    depth,
                    transitive,
                    skip_masked,
                    &report_type,
                    &mut read,
                    &mut found_reports,
                    &mut asked,
                    &mut queue,
                    &mut expanded_edits,
                    control,
                )?;
            }
        }
        drop(act_scan);
        // 3. Reports that directly edited `<on>` via `gr:editOf`, only when `<on>` is
        //    itself a visible, unmasked `gr:Report` in that proven reports graph and
        //    its `gr:editOf` chain has not already been expanded.
        let mut report_graphs = match state.seen.as_mut() {
            Some(seen) => {
                seen.graphs_with_subject_type(on.as_str(), RDF_TYPE, GR_REPORT, true, |g| {
                    is_reports_graph(g) && allows_reports_read(g)
                })?
            }
            None => state.reads.graphs_with_subject_type(
                on.as_str(),
                RDF_TYPE,
                GR_REPORT,
                false,
                |g| is_reports_graph(g) && allows_reports_read(g),
            )?,
        };
        report_graphs.items.sort();
        for g in &report_graphs.items {
            control.check()?;
            let key = (Arc::clone(&on), Arc::clone(g));
            if expanded_edits.contains(&key) {
                continue;
            }
            result_held.grow_by((g.as_str().len() as u64).saturating_add(SET_ENTRY_BYTES))?;
            expanded_edits.insert(key);
            let remaining = MAX_EVIDENCE.saturating_sub(read);
            let mut edit_scan = match state.seen.as_mut() {
                Some(seen) => {
                    seen.visible_subjects_up_to(GR_EDIT_OF, &o, g.as_str(), remaining, true)?
                }
                None => state.reads.visible_subjects_up_to(
                    GR_EDIT_OF,
                    &o,
                    g.as_str(),
                    remaining,
                    false,
                )?,
            };
            read += edit_scan.items.len();
            if edit_scan.more || read > MAX_EVIDENCE {
                return Err(super::incomplete("the dependents of the changed records"));
            }
            edit_scan.items.sort();
            for edit in &edit_scan.items {
                control.check()?;
                record_report_and_edits(
                    &mut state,
                    &preparation,
                    &mut result_held,
                    edit,
                    g,
                    &on,
                    depth,
                    transitive,
                    skip_masked,
                    &report_type,
                    &mut read,
                    &mut found_reports,
                    &mut asked,
                    &mut queue,
                    &mut expanded_edits,
                    control,
                )?;
            }
        }
        drop(report_graphs);
    }
    let final_vec_bytes = (found.len() as u64)
        .saturating_mul(std::mem::size_of::<Dependent>() as u64)
        .saturating_add(
            (found_reports.len() as u64)
                .saturating_mul(std::mem::size_of::<ReportDependent>() as u64),
        );
    result_held.grow_by(final_vec_bytes)?;
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
    preparation: &Preparation,
    result_held: &mut Reservation,
    report: &NamedNode,
    graph: &Arc<NamedNode>,
    on: &Arc<NamedNode>,
    depth: u32,
    transitive: bool,
    skip_masked: bool,
    report_type: &Term,
    read: &mut usize,
    found_reports: &mut BTreeMap<NamedNode, ReportDependent>,
    asked: &mut BTreeSet<Arc<NamedNode>>,
    queue: &mut VecDeque<(Arc<NamedNode>, u32)>,
    expanded_edits: &mut BTreeSet<(Arc<NamedNode>, Arc<NamedNode>)>,
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
    let community_of = |state: &mut State<'_>,
                        r: &NamedNode,
                        held: &mut Reservation|
     -> Result<Option<NamedNode>> {
        match state.seen.as_mut() {
            Some(seen) => seen.single_iri_admitted(r.as_str(), GR_COMMUNITY, graph.as_str(), held),
            None => state
                .reads
                .single_iri_admitted(r.as_str(), GR_COMMUNITY, graph.as_str(), held),
        }
    };
    let community = community_of(state, report, result_held)?;
    let entry_bytes = (report.as_str().len() as u64)
        .saturating_mul(3)
        .saturating_add(graph.as_str().len() as u64)
        .saturating_add(on.as_str().len() as u64)
        .saturating_add(REPORT_DEPENDENT_OVERHEAD)
        .saturating_add(SET_ENTRY_BYTES);
    result_held.grow_by(entry_bytes)?;
    let report_arc = Arc::new(report.clone());
    found_reports.insert(
        report.clone(),
        ReportDependent {
            report: report.clone(),
            community,
            graph: (**graph).clone(),
            on: (**on).clone(),
            dependence: Dependence::Used,
            depth,
        },
    );
    if transitive && !asked.contains(report) {
        result_held.grow_by(SET_ENTRY_BYTES * 2)?;
        asked.insert(Arc::clone(&report_arc));
        queue.push_back((Arc::clone(&report_arc), depth + 1));
    }
    // Person-edited reports (`gr:editOf`) carry no generating activity of their own;
    // their provenance resolves through `gr:editOf+` to the edited report's activity.
    let mut edit_held = preparation.reserve(SET_ENTRY_BYTES)?;
    let mut edit_queue = VecDeque::from([report_arc]);
    while let Some(prev) = edit_queue.pop_front() {
        control.check()?;
        let key = (Arc::clone(&prev), Arc::clone(graph));
        if expanded_edits.contains(&key) {
            continue;
        }
        result_held.grow_by((graph.as_str().len() as u64).saturating_add(SET_ENTRY_BYTES))?;
        expanded_edits.insert(key);
        let remaining = MAX_EVIDENCE.saturating_sub(*read);
        let _prev_term_held = preparation.reserve(prev.as_str().len() as u64)?;
        let prev_term = Term::NamedNode((*prev).clone());
        let mut edit_scan = match state.seen.as_mut() {
            Some(seen) => seen.visible_subjects_up_to(
                GR_EDIT_OF,
                &prev_term,
                graph.as_str(),
                remaining,
                skip_masked,
            )?,
            None => state.reads.visible_subjects_up_to(
                GR_EDIT_OF,
                &prev_term,
                graph.as_str(),
                remaining,
                false,
            )?,
        };
        *read += edit_scan.items.len();
        if edit_scan.more || *read > MAX_EVIDENCE {
            return Err(super::incomplete("the dependents of the changed records"));
        }
        edit_scan.items.sort();
        for edit in &edit_scan.items {
            if found_reports.contains_key(edit)
                || state.masked(edit, graph)?
                || !is_report(state, edit)?
            {
                continue;
            }
            let community = community_of(state, edit, result_held)?;
            let edit_bytes = (edit.as_str().len() as u64)
                .saturating_mul(3)
                .saturating_add(graph.as_str().len() as u64)
                .saturating_add(on.as_str().len() as u64)
                .saturating_add(REPORT_DEPENDENT_OVERHEAD)
                .saturating_add(SET_ENTRY_BYTES);
            result_held.grow_by(edit_bytes)?;
            let edit_arc = Arc::new(edit.clone());
            found_reports.insert(
                edit.clone(),
                ReportDependent {
                    report: edit.clone(),
                    community,
                    graph: (**graph).clone(),
                    on: (**on).clone(),
                    dependence: Dependence::Used,
                    depth,
                },
            );
            if transitive && !asked.contains(edit) {
                result_held.grow_by(SET_ENTRY_BYTES * 2)?;
                asked.insert(Arc::clone(&edit_arc));
                queue.push_back((Arc::clone(&edit_arc), depth + 1));
            }
            edit_held.grow_by(SET_ENTRY_BYTES)?;
            edit_queue.push_back(edit_arc);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Budget, BudgetKind};

    /// A long stored input IRI repeated across reports is charged before cloning `on`
    /// into retained results, so a tight preparation allowance refuses cleanly instead
    /// of allocating unbounded copies.
    #[test]
    fn tight_budget_refuses_repeated_long_input_iri_copies() {
        let ds = Dataset::memory();
        let settings = Settings {
            enabled: true,
            ..Settings::default()
        };
        let reports = "urn:x-sparkles:graphrag:reports";
        let long_input = format!("urn:input:{}", "a".repeat(16 * 1024));
        let mut trig = format!("GRAPH <{reports}> {{\n");
        trig.push_str(&format!(
            "<urn:act:0> <{RDF_TYPE}> <{PROV_ACTIVITY}> ; <{PROV_USED}> <{long_input}> .\n"
        ));
        for i in 0..20 {
            trig.push_str(&format!(
                "<urn:rep:{i}> <{RDF_TYPE}> <{GR_REPORT}> ; <{PROV_WAS_GENERATED_BY}> <urn:act:0> .\n"
            ));
        }
        trig.push_str("}\n");
        ds.update(&format!("INSERT DATA {{ {trig} }}")).unwrap();

        let head = ds.store().snapshot();
        let changed = [NamedNode::new(long_input).unwrap()];
        let no_nonce = |_: &str| None;
        let err = dependents_in_with_budget(
            &settings,
            &no_nonce,
            DependentsHead::Snapshot(&head),
            &changed,
            false,
            None,
            &Control::none(),
            128 << 10,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                Error::BudgetExceeded(Budget {
                    kind: BudgetKind::Memory,
                    ..
                })
            ),
            "{err:?}"
        );
        // Adequate budget succeeds with all 20 reports.
        let ok = dependents_in_with_budget(
            &settings,
            &no_nonce,
            DependentsHead::Snapshot(&head),
            &changed,
            false,
            None,
            &Control::none(),
            2 << 20,
        )
        .unwrap();
        assert_eq!(ok.reports.len(), 20);
    }
}
