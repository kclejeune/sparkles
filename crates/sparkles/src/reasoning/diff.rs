//! Read-only entailment changes between branches, commits and preview snapshots.

use super::analysis::{self, AnalysisControl, AnalysisOptions, State, fact_bytes, hold};
use crate::store::Snapshot;
use crate::{Dataset, Result};
use oxrdf::Term;
use serde_json::{Value, json};
use sparkles_reasoner::diagnostics::{CheckStatus, DiagnosticsReport, Evidence, Finding};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

/// Two state selectors and one program shared by both states.
#[derive(Clone)]
pub struct DiffRequest {
    pub from: String,
    pub to: String,
    pub analysis: AnalysisOptions,
    /// Optional state-specific graph grants; the shared access applies otherwise.
    pub from_access: Option<Arc<crate::access::GraphAccess>>,
    pub to_access: Option<Arc<crate::access::GraphAccess>>,
    /// Maximum number of triples retained per direction. Counts remain complete.
    pub sample_limit: usize,
    /// Require every changed triple, for an applicable RDF Patch or TriG document.
    pub all_changes: bool,
}

impl DiffRequest {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            analysis: Default::default(),
            from_access: None,
            to_access: None,
            sample_limit: 100,
            all_changes: false,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffState {
    pub selector: String,
    pub commit: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ChangeCounts {
    pub added: usize,
    pub removed: usize,
}

/// Counts cover the complete delta. Samples are bounded independently in each direction.
#[derive(Clone, Debug)]
pub struct EntailmentDiff {
    pub from: DiffState,
    pub to: DiffState,
    pub added: usize,
    pub removed: usize,
    pub added_triples: Vec<[Term; 3]>,
    pub removed_triples: Vec<[Term; 3]>,
    pub by_predicate: BTreeMap<String, ChangeCounts>,
    pub by_class: BTreeMap<String, ChangeCounts>,
    pub new_findings: Vec<Finding>,
    pub findings_complete: bool,
    pub sample_truncated: bool,
    pub warnings: Vec<String>,
}

pub(crate) fn finding_key(finding: &Finding) -> String {
    let mut evidence: Vec<_> = finding
        .evidence
        .iter()
        .map(|(name, values)| {
            let values = match values {
                Evidence::One(term) => vec![term.to_string()],
                Evidence::Many(terms) => {
                    let mut terms: Vec<_> = terms.iter().map(ToString::to_string).collect();
                    terms.sort();
                    terms.dedup();
                    terms
                }
            };
            (name, values)
        })
        .collect();
    evidence.sort();
    // Structured encoding avoids collisions from term strings containing delimiters.
    serde_json::to_string(&(finding.check, finding.focus.to_string(), evidence)).unwrap()
}

pub(crate) fn check_complete(report: &DiagnosticsReport, check: &str) -> bool {
    report
        .checks
        .iter()
        .find(|outcome| outcome.id == check)
        .is_some_and(|outcome| {
            matches!(outcome.status, CheckStatus::None | CheckStatus::Violations)
        })
}

pub(crate) fn finding_json(finding: &Finding) -> Value {
    let evidence: serde_json::Map<_, _> = finding
        .evidence
        .iter()
        .map(|(name, values)| {
            let values = match values {
                Evidence::One(term) => crate::sparql::results::term_json(term),
                Evidence::Many(terms) => Value::Array(
                    terms
                        .iter()
                        .map(crate::sparql::results::term_json)
                        .collect(),
                ),
            };
            (name.to_string(), values)
        })
        .collect();
    json!({"check":finding.check,"rule":finding.rule,"severity":finding.severity.name(),
        "focus":crate::sparql::results::term_json(&finding.focus),"evidence":evidence,
        "basis":finding.basis.name(),"message":finding.message})
}

impl EntailmentDiff {
    pub fn to_json(&self) -> Value {
        let triples = |rows: &[[Term; 3]]| {
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(crate::sparql::results::term_json)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        json!({"entailmentDiffFormat":1,"from":self.from,"to":self.to,
            "added":self.added,"removed":self.removed,"byPredicate":self.by_predicate,"byClass":self.by_class,
            "sample":{"added":triples(&self.added_triples),"removed":triples(&self.removed_triples)},
            "sampleTruncated":self.sample_truncated,"newFindings":self.new_findings.iter().map(finding_json).collect::<Vec<_>>(),
            "findingsComplete":self.findings_complete,"warnings":self.warnings})
    }

    fn complete_changes(&self) -> Result<()> {
        if self.sample_truncated {
            return Err(analysis::budget(
                "the complete entailment delta exceeds the retained sample limit",
            ));
        }
        Ok(())
    }

    /// Every change, grouped in `spk:added` and `spk:removed` TriG graphs.
    pub fn to_trig(&self) -> Result<String> {
        self.complete_changes()?;
        let mut text = String::from("@prefix spk: <urn:x-sparkles:> .\n");
        for (name, rows) in [
            ("added", &self.added_triples),
            ("removed", &self.removed_triples),
        ] {
            text.push_str(&format!("spk:{name} {{\n"));
            for [s, p, o] in rows {
                text.push_str(&format!("  {s} {p} {o} .\n"));
            }
            text.push_str("}\n");
        }
        Ok(text)
    }

    /// Every change as an RDF Patch over the inferred graph.
    pub fn to_patch(&self) -> Result<String> {
        self.complete_changes()?;
        let mut bytes = Vec::new();
        let mut writer = crate::patch::PatchWriter::new(&mut bytes, false);
        for (operation, rows) in [
            (crate::store::DiffOp::Remove, &self.removed_triples),
            (crate::store::DiffOp::Add, &self.added_triples),
        ] {
            for [s, p, o] in rows {
                let subject = oxrdf::NamedOrBlankNode::try_from(s.clone())
                    .map_err(|_| crate::Error::invalid("a patch subject must be an RDF subject"))?;
                let predicate = oxrdf::NamedNode::try_from(p.clone())
                    .map_err(|_| crate::Error::invalid("a patch predicate must be an IRI"))?;
                writer.change(
                    operation,
                    &oxrdf::Quad::new(
                        subject,
                        predicate,
                        o.clone(),
                        oxrdf::NamedNode::new_unchecked(super::INFERRED_GRAPH),
                    ),
                )?;
            }
        }
        Ok(String::from_utf8(bytes).expect("RDF Patch is UTF-8"))
    }
}

struct ResolvedState {
    snapshot: Arc<Snapshot>,
    materialization: Option<Dataset>,
}

fn resolve_state(
    dataset: &Dataset,
    selector: &str,
    control: &AnalysisControl,
) -> Result<ResolvedState> {
    use crate::history::{At, HistoryOptions};
    let options = HistoryOptions {
        cancel: Some(control.cancel.clone()),
        deadline: control.deadline,
    };
    let (selected, at) = if let Some((branch, at)) = selector.rsplit_once('@') {
        if branch.is_empty() || at.is_empty() {
            return Err(crate::Error::invalid("a branch state needs branch@commit"));
        }
        (dataset.branch(branch)?, at.parse::<At>()?)
    } else if let Ok(at) = selector.parse::<At>() {
        (dataset.clone(), at)
    } else {
        (dataset.branch(selector)?, At::Head)
    };
    let (snapshot, _, owner) = dataset
        .store()
        .snapshot_at_with_branch(selected.store(), &at, &options)
        .map_err(|error| control.error(error))?;
    let materialization = (owner == selected.store().branch_id()).then_some(selected);
    Ok(ResolvedState {
        snapshot,
        materialization,
    })
}

pub(crate) fn resolve(
    dataset: &Dataset,
    selector: &str,
    control: &AnalysisControl,
) -> Result<Arc<Snapshot>> {
    Ok(resolve_state(dataset, selector, control)?.snapshot)
}

pub fn diff(dataset: &Dataset, request: &DiffRequest) -> Result<EntailmentDiff> {
    let control = AnalysisControl::new(&request.analysis)?;
    let from = resolve_state(dataset, &request.from, &control)?;
    let to = resolve_state(dataset, &request.to, &control)?;
    compare_resolved(
        dataset,
        &from.snapshot,
        &to.snapshot,
        request,
        &control,
        from.materialization.zip(to.materialization),
    )
}

/// A record proves freshness only for the handle retained with the resolved view.
fn selected_materialization(
    dataset: &Dataset,
    snapshot: &Snapshot,
    program: &analysis::AnalysisProgram,
    options: &AnalysisOptions,
) -> bool {
    if !snapshot.cacheable
        || options
            .access
            .as_ref()
            .is_some_and(|access| !access.reads_everything())
    {
        return false;
    }
    let Some(record) = dataset.reasoning().record() else {
        return false;
    };
    let (Ok(profile), Ok(extras), Ok(inputs)) =
        (record.profile(), record.extras(), record.run_inputs())
    else {
        return false;
    };
    profile == program.profile
        && extras == program.extras
        && inputs == program.inputs
        && super::freshness(&record, dataset.store(), snapshot.commit).stale == Some(false)
}

/// Compare actual retained candidates; no reconstruction from a truncated preview patch.
pub(crate) fn compare(
    dataset: &Dataset,
    from: &Arc<Snapshot>,
    to: &Arc<Snapshot>,
    request: &DiffRequest,
    control: &AnalysisControl,
) -> Result<EntailmentDiff> {
    compare_resolved(dataset, from, to, request, control, None)
}

fn compare_resolved(
    dataset: &Dataset,
    from: &Arc<Snapshot>,
    to: &Arc<Snapshot>,
    request: &DiffRequest,
    control: &AnalysisControl,
    pinned: Option<(Dataset, Dataset)>,
) -> Result<EntailmentDiff> {
    let program = analysis::program(dataset, &request.analysis)?;
    let mut from_options = request.analysis.clone();
    from_options.access = request.from_access.clone().or(from_options.access);
    let mut to_options = request.analysis.clone();
    to_options.access = request.to_access.clone().or(to_options.access);
    let materialized = pinned.filter(|(from_dataset, to_dataset)| {
        selected_materialization(from_dataset, from, &program, &from_options)
            && selected_materialization(to_dataset, to, &program, &to_options)
    });
    let (a, b) = if let Some((from_dataset, to_dataset)) = materialized {
        let a = analysis::stored(&from_dataset, from, &program, &from_options, control, 0)?;
        let b = analysis::stored(&to_dataset, to, &program, &to_options, control, a.bytes)?;
        (a, b)
    } else {
        let a = analysis::analyze(
            dataset,
            from,
            &program,
            &from_options,
            control,
            0,
            None,
            &[],
        )?;
        let b = analysis::analyze(
            dataset,
            to,
            &program,
            &to_options,
            control,
            a.bytes,
            None,
            &[],
        )?;
        (a, b)
    };
    delta(
        a,
        b,
        DiffState {
            selector: request.from.clone(),
            commit: from.commit,
        },
        DiffState {
            selector: request.to.clone(),
            commit: to.commit,
        },
        request,
        control,
    )
}

fn delta(
    a: State,
    b: State,
    from: DiffState,
    to: DiffState,
    request: &DiffRequest,
    control: &AnalysisControl,
) -> Result<EntailmentDiff> {
    let mut retained = a
        .bytes
        .checked_add(b.bytes)
        .ok_or_else(|| analysis::budget("the retained analysis size overflowed"))?;
    let mut result = EntailmentDiff {
        from,
        to,
        added: 0,
        removed: 0,
        added_triples: Vec::new(),
        removed_triples: Vec::new(),
        by_predicate: BTreeMap::new(),
        by_class: BTreeMap::new(),
        new_findings: Vec::new(),
        findings_complete: true,
        sample_truncated: false,
        warnings: a.warnings.clone(),
    };
    result.warnings.extend(b.warnings.iter().cloned());
    result.warnings.sort();
    result.warnings.dedup();
    for (adding, having, other) in [(true, &b, &a), (false, &a, &b)] {
        for (key, triple) in &having.closure {
            control.check()?;
            if other.closure.contains_key(key) || having.asserted.contains_key(key) {
                continue;
            }
            let count = if adding {
                &mut result.added
            } else {
                &mut result.removed
            };
            *count += 1;
            for (is_class, map, term) in [
                (false, &mut result.by_predicate, &triple[1]),
                (true, &mut result.by_class, &triple[2]),
            ] {
                if is_class && triple[1] != oxrdf::vocab::rdf::TYPE.into_owned().into() {
                    continue;
                }
                let name = match term {
                    Term::NamedNode(node) => node.as_str().to_string(),
                    term => term.to_string(),
                };
                if !map.contains_key(&name) {
                    hold(request.analysis.max_memory, &mut retained, 96 + name.len())?;
                }
                let counts = map.entry(name).or_default();
                if adding {
                    counts.added += 1;
                } else {
                    counts.removed += 1;
                }
            }
            let rows = if adding {
                &mut result.added_triples
            } else {
                &mut result.removed_triples
            };
            if request.all_changes && rows.len() >= request.sample_limit {
                return Err(crate::Error::BudgetExceeded(crate::Budget {
                    kind: crate::BudgetKind::Rows,
                    limit: request.sample_limit as u64,
                    requested: rows.len() as u64 + 1,
                }));
            }
            if request.all_changes || rows.len() < request.sample_limit {
                hold(
                    request.analysis.max_memory,
                    &mut retained,
                    fact_bytes(key, triple),
                )?;
                rows.push(triple.clone());
            } else {
                result.sample_truncated = true;
            }
        }
    }
    let known: HashSet<_> = a.diagnostics.findings.iter().map(finding_key).collect();
    for finding in &b.diagnostics.findings {
        control.check()?;
        if known.contains(&finding_key(finding)) {
            continue;
        }
        if check_complete(&a.diagnostics, finding.check) {
            let bytes = serde_json::to_vec(&finding_json(finding)).unwrap().len() * 2 + 256;
            hold(request.analysis.max_memory, &mut retained, bytes)?;
            result.new_findings.push(finding.clone());
        } else {
            result.findings_complete = false;
        }
    }
    result.findings_complete &= a
        .diagnostics
        .checks
        .iter()
        .chain(&b.diagnostics.checks)
        .all(|outcome| matches!(outcome.status, CheckStatus::None | CheckStatus::Violations));
    Ok(result)
}

impl DiffRequest {
    pub fn from_json(value: &Value) -> Result<Self> {
        let from = value
            .get("from")
            .and_then(Value::as_str)
            .ok_or_else(|| crate::Error::invalid("from must be a state selector"))?;
        let to = value.get("to").and_then(Value::as_str).unwrap_or("head");
        let mut request = Self::new(from, to);
        request.analysis = AnalysisOptions::from_json(value)?;
        request.sample_limit = analysis::positive(value, "sampleLimit", 100, 10_000)?;
        request.all_changes = match value.get("allChanges") {
            None => false,
            Some(Value::Bool(value)) => *value,
            _ => return Err(crate::Error::invalid("allChanges must be a boolean")),
        };
        Ok(request)
    }
}

/// Compare the complete views retained by an opt-in write or merge preview.
pub fn preview(
    dataset: &Dataset,
    preview: &crate::preview::Preview,
    request: &DiffRequest,
) -> Result<EntailmentDiff> {
    let states = preview
        .states
        .as_ref()
        .ok_or_else(|| crate::Error::invalid("the preview did not retain its complete states"))?;
    let control = AnalysisControl::new(&request.analysis)?;
    compare(dataset, &states.before, &states.after, request, &control)
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    fn same_snapshot_after_interposed_write(profile: sparkles_reasoner::Profile) -> EntailmentDiff {
        let dataset = Dataset::memory();
        dataset
            .update("INSERT DATA {<urn:trigger> <urn:p> 1}")
            .unwrap();
        let snapshot = dataset.store().snapshot();
        let mut request = DiffRequest::new("head", "head");
        request.analysis.program = Some(analysis::AnalysisProgram {
            profile,
            extras: Default::default(),
            inputs: Default::default(),
        });
        let control = AnalysisControl::new(&request.analysis).unwrap();
        let program = analysis::program(&dataset, &request.analysis).unwrap();
        let a = analysis::analyze(
            &dataset,
            &snapshot,
            &program,
            &request.analysis,
            &control,
            0,
            None,
            &[],
        )
        .unwrap();
        dataset
            .update(&format!(
                "INSERT DATA {{ _:unrelated <urn:p> \"{}\" }}",
                "a".repeat(32768)
            ))
            .unwrap();
        let b = analysis::analyze(
            &dataset,
            &snapshot,
            &program,
            &request.analysis,
            &control,
            a.bytes,
            None,
            &[],
        )
        .unwrap();
        delta(
            a,
            b,
            DiffState {
                selector: "fixed".into(),
                commit: snapshot.commit,
            },
            DiffState {
                selector: "fixed".into(),
                commit: snapshot.commit,
            },
            &request,
            &control,
        )
        .unwrap()
    }
    #[test]
    fn unrelated_blank_allocation_cannot_change_a_fixed_generated_closure() {
        for call in ["makeTemp(?w)", "makeSkolem(?w, ?value)"] {
            let profile = sparkles_reasoner::Profile::Rules(format!(
                "[generated: (<urn:trigger> <urn:p> ?value), {call} -> (?w <urn:q> ?value)]"
            ));
            let diff = same_snapshot_after_interposed_write(profile);
            assert_eq!((diff.added, diff.removed), (0, 0), "{call}");
            assert!(diff.new_findings.is_empty());
        }
    }
    #[test]
    fn program_blank_constants_preserve_identity_after_interposed_allocation() {
        let diff = same_snapshot_after_interposed_write(sparkles_reasoner::Profile::Rules(
            "[constant: (<urn:trigger> <urn:p> ?value) -> (_:w <urn:q> ?value)]".into(),
        ));
        assert_eq!((diff.added, diff.removed), (0, 0));
    }
    #[test]
    fn one_request_uses_one_analysis_clock_for_both_states() {
        let diff=same_snapshot_after_interposed_write(sparkles_reasoner::Profile::Rules("[clock: (<urn:trigger> <urn:p> ?value), now(?time) -> (<urn:trigger> <urn:time> ?time)]".into()));
        assert_eq!((diff.added, diff.removed), (0, 0));
    }
    #[test]
    fn rdf_patch_keeps_jena_blank_labels() {
        let triple = [
            Term::BlankNode(oxrdf::BlankNode::new("b123").unwrap()),
            oxrdf::NamedNode::new("urn:p").unwrap().into(),
            Term::BlankNode(oxrdf::BlankNode::new("b456").unwrap()),
        ];
        let diff = EntailmentDiff {
            from: DiffState {
                selector: "a".into(),
                commit: 1,
            },
            to: DiffState {
                selector: "b".into(),
                commit: 2,
            },
            added: 1,
            removed: 0,
            added_triples: vec![triple],
            removed_triples: vec![],
            by_predicate: Default::default(),
            by_class: Default::default(),
            new_findings: vec![],
            findings_complete: true,
            sample_truncated: false,
            warnings: vec![],
        };
        assert_eq!(
            diff.to_patch().unwrap(),
            "A <_:b123> <urn:p> <_:b456> <urn:x-sparkles:inferred> .\n"
        );
    }
    #[test]
    fn inserted_earlier_bindings_preserve_existing_generated_witnesses() {
        for generator in ["makeTemp(?w)", "makeSkolem(?w, ?s)"] {
            let dataset = Dataset::memory();
            dataset
                .update("INSERT DATA {<urn:z> <urn:p> <urn:t>}")
                .unwrap();
            let before = dataset.store().snapshot();
            dataset
                .update("INSERT DATA {<urn:a> <urn:p> <urn:t>}")
                .unwrap();
            let after = dataset.store().snapshot();
            let mut request = DiffRequest::new("before", "after");
            request.analysis.program = Some(analysis::AnalysisProgram {
                profile: sparkles_reasoner::Profile::Rules(format!(
                    "[r: (?s <urn:p> <urn:t>) {generator} -> (?s <urn:q> ?w)]"
                )),
                extras: Default::default(),
                inputs: Default::default(),
            });
            let control = AnalysisControl::new(&request.analysis).unwrap();
            let diff = compare(&dataset, &before, &after, &request, &control).unwrap();
            assert_eq!((diff.added, diff.removed), (1, 0), "{generator}");
            assert_eq!(
                diff.added_triples[0][0],
                oxrdf::NamedNode::new("urn:a").unwrap().into()
            );
        }
    }

    #[test]
    fn duplicate_rule_labels_keep_distinct_temporary_witnesses() {
        let dataset = Dataset::memory();
        let before = dataset.store().snapshot();
        dataset
            .update("INSERT DATA {<urn:s> <urn:p> <urn:t>}")
            .unwrap();
        let after = dataset.store().snapshot();
        let mut request = DiffRequest::new("before", "after");
        request.analysis.program = Some(analysis::AnalysisProgram { profile: sparkles_reasoner::Profile::Rules("[same: (?s <urn:p> <urn:t>) makeTemp(?w) -> (?s <urn:q> ?w)]\n[same: (?s <urn:p> <urn:t>) makeTemp(?w) -> (?s <urn:q> ?w)]".into()), extras: Default::default(), inputs: Default::default() });
        let control = AnalysisControl::new(&request.analysis).unwrap();
        let diff = compare(&dataset, &before, &after, &request, &control).unwrap();
        assert_eq!((diff.added, diff.removed), (2, 0));
        assert_ne!(diff.added_triples[0][2], diff.added_triples[1][2]);
    }

    #[test]
    fn recursive_generated_inputs_obey_the_inferred_fact_bound() {
        let dataset = Dataset::memory();
        dataset
            .update("INSERT DATA {<urn:s> <urn:p> <urn:t>}")
            .unwrap();
        let snapshot = dataset.store().snapshot();
        let mut request = DiffRequest::new("before", "after");
        request.analysis.max_inferred = Some(32);
        request.analysis.program = Some(analysis::AnalysisProgram {
            profile: sparkles_reasoner::Profile::Rules(
                "[r: (?s <urn:p> <urn:t>) makeTemp(?w) -> (?w <urn:p> <urn:t>)]".into(),
            ),
            extras: Default::default(),
            inputs: Default::default(),
        });
        let control = AnalysisControl::new(&request.analysis).unwrap();
        let program = request.analysis.program.as_ref().unwrap();
        let error = match analysis::analyze(
            &dataset,
            &snapshot,
            program,
            &request.analysis,
            &control,
            0,
            None,
            &[],
        ) {
            Err(error) => error,
            Ok(state) => panic!(
                "recursive closure unexpectedly stopped: {:?}",
                state.closure
            ),
        };
        assert!(
            error.to_string().contains("inferred") || error.to_string().contains("memory"),
            "{error}"
        );
    }
    #[test]
    fn newly_present_configured_graph_keeps_existing_witnesses() {
        let dataset = Dataset::memory();
        dataset
            .update("INSERT DATA {<urn:z> <urn:p> <urn:t>}")
            .unwrap();
        let before = dataset.store().snapshot();
        dataset
            .update("INSERT DATA {GRAPH <urn:later> {<urn:a> <urn:other> <urn:t>}}")
            .unwrap();
        let after = dataset.store().snapshot();
        let mut request = DiffRequest::new("before", "after");
        request.analysis.program = Some(analysis::AnalysisProgram {
            profile: sparkles_reasoner::Profile::Rules(
                "[r: (?s <urn:p> <urn:t>) makeTemp(?w) -> (?s <urn:q> ?w)]".into(),
            ),
            extras: Default::default(),
            inputs: sparkles_reasoner::Inputs {
                data_graphs: vec![
                    sparkles_reasoner::GraphRef::Default,
                    sparkles_reasoner::GraphRef::Named("urn:later".into()),
                ],
                ..Default::default()
            },
        });
        let control = AnalysisControl::new(&request.analysis).unwrap();
        let diff = compare(&dataset, &before, &after, &request, &control).unwrap();
        assert_eq!((diff.added, diff.removed), (0, 0));
    }
    #[test]
    fn retained_views_cannot_take_freshness_from_a_replacement_branch_name() {
        let dataset = Dataset::memory();
        dataset.update("INSERT DATA {<urn:Manager> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:Employee>}").unwrap();
        let run = super::super::ReasonRequest {
            profile: sparkles_reasoner::Profile::RdfsSimple,
            ..Default::default()
        };
        dataset.reasoning().run(&run).unwrap();
        dataset
            .create_branch("moving", &Default::default())
            .unwrap();
        let old = dataset.branch("moving").unwrap();
        old.update("INSERT DATA {<urn:alice> a <urn:Manager>}")
            .unwrap();
        let request = DiffRequest::new("head", "moving");
        let control = AnalysisControl::new(&request.analysis).unwrap();
        let from_state = resolve_state(&dataset, "head", &control).unwrap();
        let to_state = resolve_state(&dataset, "moving", &control).unwrap();
        let from = from_state.snapshot.clone();
        let to = to_state.snapshot.clone();
        dataset.rename_branch("moving", "retained").unwrap();
        dataset
            .create_branch("moving", &Default::default())
            .unwrap();
        let replacement = dataset.branch("moving").unwrap();
        replacement
            .update("INSERT DATA {<urn:unrelated> <urn:p> <urn:o>}")
            .unwrap();
        replacement.reasoning().run(&run).unwrap();
        assert_eq!(replacement.store().snapshot().commit, to.commit);
        assert_eq!(
            replacement.reasoning().status().unwrap().freshness.stale,
            Some(false)
        );
        let diff = compare_resolved(
            &dataset,
            &from,
            &to,
            &request,
            &control,
            from_state.materialization.zip(to_state.materialization),
        )
        .unwrap();
        assert_eq!((diff.added, diff.removed), (1, 0));
        // An unpinned synthetic comparison also cannot reopen selector names.
        let diff = compare(&dataset, &from, &to, &request, &control).unwrap();
        assert_eq!((diff.added, diff.removed), (1, 0));
    }
}
