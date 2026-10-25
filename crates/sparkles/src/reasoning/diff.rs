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
        let mut text = String::new();
        for (operation, rows) in [("D", &self.removed_triples), ("A", &self.added_triples)] {
            for [s, p, o] in rows {
                text.push_str(&format!(
                    "{operation} {s} {p} {o} <{}> .\n",
                    super::INFERRED_GRAPH
                ));
            }
        }
        Ok(text)
    }
}

pub(crate) fn resolve(
    dataset: &Dataset,
    selector: &str,
    control: &AnalysisControl,
) -> Result<Arc<Snapshot>> {
    use crate::history::{At, HistoryOptions};
    let options = HistoryOptions {
        cancel: Some(control.cancel.clone()),
        deadline: control.deadline,
    };
    let resolved = if let Some((branch, at)) = selector.rsplit_once('@') {
        if branch.is_empty() || at.is_empty() {
            return Err(crate::Error::invalid("a branch state needs branch@commit"));
        }
        dataset
            .store()
            .branch_snapshot_at(branch, &at.parse::<At>()?, &options)
    } else if let Ok(at) = selector.parse::<At>() {
        dataset.store().snapshot_at(&at, &options)
    } else {
        dataset
            .store()
            .branch_snapshot_at(selector, &At::Head, &options)
    };
    resolved
        .map(|(snapshot, _)| snapshot)
        .map_err(|error| control.error(error))
}

pub fn diff(dataset: &Dataset, request: &DiffRequest) -> Result<EntailmentDiff> {
    let control = AnalysisControl::new(&request.analysis)?;
    let from = resolve(dataset, &request.from, &control)?;
    let to = resolve(dataset, &request.to, &control)?;
    compare(dataset, &from, &to, request, &control)
}

/// Compare actual retained candidates; no reconstruction from a truncated preview patch.
pub(crate) fn compare(
    dataset: &Dataset,
    from: &Arc<Snapshot>,
    to: &Arc<Snapshot>,
    request: &DiffRequest,
    control: &AnalysisControl,
) -> Result<EntailmentDiff> {
    let program = analysis::program(dataset, &request.analysis)?;
    let mut from_options = request.analysis.clone();
    from_options.access = request.from_access.clone().or(from_options.access);
    let mut to_options = request.analysis.clone();
    to_options.access = request.to_access.clone().or(to_options.access);
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
