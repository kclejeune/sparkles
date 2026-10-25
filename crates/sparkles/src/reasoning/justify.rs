//! Bounded black-box contraction and hitting-set enumeration of asserted supports.

use super::analysis::{self, AnalysisControl, AnalysisOptions, AnalysisProgram, hold};
use super::diff::{check_complete, finding_json, finding_key};
use crate::id::Id;
use crate::store::Snapshot;
use crate::{Dataset, Error, Result};
use oxrdf::{GraphName, NamedNode, Quad, Term};
use serde_json::{Value, json};
use sparkles_reasoner::diagnostics::{Evidence, Finding, Severity};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum JustifyTarget {
    Finding(Finding),
    Class(NamedNode),
    AllClasses,
}

#[derive(Clone)]
pub struct JustifyRequest {
    pub target: JustifyTarget,
    pub at: String,
    pub analysis: AnalysisOptions,
    pub max_checks: usize,
    pub max_justifications: usize,
}

impl JustifyRequest {
    pub fn new(target: JustifyTarget) -> Self {
        Self {
            target,
            at: "head".into(),
            analysis: Default::default(),
            max_checks: 500,
            max_justifications: 5,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SupportFact {
    pub quad: Quad,
    /// `schema` for ontology axioms, otherwise `assertion`.
    pub role: &'static str,
}

#[derive(Clone, Debug)]
pub struct Justification {
    pub facts: Vec<SupportFact>,
}

#[derive(Clone, Debug)]
pub struct JustifiedTarget {
    pub class: Option<NamedNode>,
    pub finding: Option<Finding>,
    /// A negative result is conclusive only when `complete` is true.
    pub holds: bool,
    pub justifications: Vec<Justification>,
    pub complete: bool,
}

#[derive(Clone, Debug)]
pub struct JustificationReport {
    pub commit: u64,
    pub results: Vec<JustifiedTarget>,
    pub checks: usize,
    pub complete: bool,
}

impl JustificationReport {
    pub fn to_json(&self) -> Value {
        let term = crate::sparql::results::term_json;
        json!({"justificationFormat":1,"commit":self.commit,"checks":self.checks,"complete":self.complete,
            "results":self.results.iter().map(|result| json!({
                "class":result.class.as_ref().map(NamedNode::as_str),"finding":result.finding.as_ref().map(finding_json),
                "holds":result.holds,"complete":result.complete,
                "justifications":result.justifications.iter().map(|support| json!({"facts":support.facts.iter().map(|fact| {
                    let graph=match &fact.quad.graph_name {GraphName::DefaultGraph=>json!("default"),GraphName::NamedNode(node)=>term(&node.clone().into()),GraphName::BlankNode(node)=>term(&node.clone().into())};
                    json!({"triple":[term(&fact.quad.subject.clone().into()),term(&fact.quad.predicate.clone().into()),term(&fact.quad.object)],"graph":graph,"role":fact.role})
                }).collect::<Vec<_>>() })).collect::<Vec<_>>() })).collect::<Vec<_>>()})
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Truth {
    Yes,
    No,
    Unknown,
}

struct Search<'a> {
    dataset: &'a Dataset,
    snapshot: Arc<Snapshot>,
    program: AnalysisProgram,
    options: &'a AnalysisOptions,
    control: &'a AnalysisControl,
    quads: Vec<[Id; 4]>,
    fixed: HashSet<[Id; 4]>,
    target: Finding,
    variables: HashSet<String>,
    checks: &'a mut usize,
    max_checks: usize,
    bytes: usize,
    stopped: bool,
}

impl Search<'_> {
    fn oracle(&mut self, support: &[usize]) -> Result<Truth> {
        if self.stopped || *self.checks >= self.max_checks {
            self.stopped = true;
            return Ok(Truth::Unknown);
        }
        if let Err(error) = self.control.check() {
            if matches!(error, Error::Cancelled) {
                return Err(error);
            }
            self.stopped = true;
            return Ok(Truth::Unknown);
        }
        *self.checks += 1;
        let mut subset = self.fixed.clone();
        subset.extend(support.iter().map(|&index| self.quads[index]));
        let state = match analysis::analyze(
            self.dataset,
            &self.snapshot,
            &self.program,
            self.options,
            self.control,
            self.bytes,
            Some(&subset),
            &[self.target.check.to_string()],
        ) {
            Ok(state) => state,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => {
                if self.control.check().is_err() {
                    self.stopped = true;
                    return Ok(Truth::Unknown);
                }
                return Err(error);
            }
        };
        let mut matched = false;
        for finding in &state.diagnostics.findings {
            let matches =
                match matches_finding(&self.target, finding, &self.variables, self.control) {
                    Ok(matches) => matches,
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(error) => {
                        if self.control.check().is_err() {
                            self.stopped = true;
                            return Ok(Truth::Unknown);
                        }
                        return Err(error);
                    }
                };
            if matches {
                matched = true;
                break;
            }
        }
        if matched {
            Ok(Truth::Yes)
        } else if check_complete(&state.diagnostics, self.target.check) {
            Ok(Truth::No)
        } else {
            self.stopped = true;
            Ok(Truth::Unknown)
        }
    }

    /// Chunk contraction followed by every single deletion certifies subset minimality
    /// for monotone programs. No support is returned after an unknown check.
    fn contract(&mut self, mut support: Vec<usize>, monotone: bool) -> Result<Option<Vec<usize>>> {
        if !monotone {
            return self.exhaustive(&support);
        }
        match self.oracle(&support)? {
            Truth::Yes => {}
            Truth::No | Truth::Unknown => return Ok(None),
        }
        let mut width = (support.len() / 2).max(1);
        while width > 1 {
            let mut start = 0;
            while start < support.len() {
                let end = (start + width).min(support.len());
                let candidate = support[..start]
                    .iter()
                    .chain(&support[end..])
                    .copied()
                    .collect::<Vec<_>>();
                match self.oracle(&candidate)? {
                    Truth::Yes => support = candidate,
                    Truth::No => start = end,
                    Truth::Unknown => return Ok(None),
                }
            }
            width /= 2;
        }
        let mut index = 0;
        while index < support.len() {
            let candidate = support[..index]
                .iter()
                .chain(&support[index + 1..])
                .copied()
                .collect::<Vec<_>>();
            match self.oracle(&candidate)? {
                Truth::Yes => support = candidate,
                Truth::No => index += 1,
                Truth::Unknown => return Ok(None),
            }
        }
        Ok(Some(support))
    }

    /// Nonmonotone programs require checking all smaller subsets, including when the
    /// whole candidate fails. Cardinality order makes every returned support minimal.
    fn exhaustive(&mut self, allowed: &[usize]) -> Result<Option<Vec<usize>>> {
        for size in 0..=allowed.len() {
            let mut positions: Vec<_> = (0..size).collect();
            loop {
                let support: Vec<_> = positions
                    .iter()
                    .map(|&position| allowed[position])
                    .collect();
                match self.oracle(&support)? {
                    Truth::Yes => return Ok(Some(support)),
                    Truth::Unknown => return Ok(None),
                    Truth::No => {}
                }
                let Some(pivot) = (0..size)
                    .rev()
                    .find(|&index| positions[index] < allowed.len() - size + index)
                else {
                    break;
                };
                positions[pivot] += 1;
                for index in pivot + 1..size {
                    positions[index] = positions[index - 1] + 1;
                }
            }
        }
        Ok(None)
    }

    fn enumerate(&mut self, max: usize, monotone: bool) -> Result<(Vec<Justification>, bool)> {
        if max == 0 {
            return Ok((Vec::new(), false));
        }
        let mut queue = VecDeque::from([BTreeSet::new()]);
        let mut visited = HashSet::new();
        let mut known = HashSet::new();
        let mut results = Vec::new();
        while let Some(forbidden) = queue.pop_front() {
            if let Err(error) = self.control.check() {
                if matches!(error, Error::Cancelled) {
                    return Err(error);
                }
                return Ok((results, false));
            }
            if !visited.insert(forbidden.clone()) {
                continue;
            }
            let allowed = (0..self.quads.len())
                .filter(|index| !forbidden.contains(index))
                .collect();
            let Some(support) = self.contract(allowed, monotone)? else {
                if self.stopped {
                    return Ok((results, false));
                }
                continue;
            };
            // Every other minimal support must exclude at least one element of this
            // support. The empty support therefore closes this whole search tree.
            for &index in &support {
                let mut child = forbidden.clone();
                child.insert(index);
                hold(
                    self.options.max_memory,
                    &mut self.bytes,
                    64 + child.len() * 16,
                )?;
                queue.push_back(child);
            }
            if known.insert(support.clone()) {
                let facts = support
                    .iter()
                    .map(|&index| {
                        let quad = self.snapshot.quad_to_terms(&self.quads[index]).unwrap();
                        SupportFact {
                            role: role(&quad),
                            quad,
                        }
                    })
                    .collect::<Vec<_>>();
                for fact in &facts {
                    hold(
                        self.options.max_memory,
                        &mut self.bytes,
                        256 + fact.quad.to_string().len() * 2,
                    )?;
                }
                results.push(Justification { facts });
                if results.len() >= max {
                    return Ok((results, queue.is_empty()));
                }
            }
        }
        Ok((results, true))
    }
}

fn role(quad: &Quad) -> &'static str {
    let predicate = quad.predicate.as_str();
    if predicate.starts_with("http://www.w3.org/2000/01/rdf-schema#")
        || predicate.starts_with("http://www.w3.org/2002/07/owl#")
        || matches!(
            predicate,
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first"
                | "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest"
        )
    {
        "schema"
    } else {
        "assertion"
    }
}

fn monotone(program: &AnalysisProgram) -> Result<bool> {
    use sparkles_reasoner::Clause;
    fn safe(rule: &sparkles_reasoner::Rule) -> bool {
        rule.body.iter().all(|clause| match clause {
            Clause::Builtin(call) => !matches!(
                call.name.as_str(),
                "noValue" | "now" | "listNotContains" | "listLength" | "listEntry" | "listForAll"
            ),
            Clause::Rule(rule) => safe(rule),
            _ => true,
        }) && rule.head.iter().all(|clause| match clause {
            Clause::Builtin(_) => false,
            Clause::Rule(rule) => safe(rule),
            _ => true,
        })
    }
    Ok(!program.extras.geo_default_geometry
        && program
            .profile
            .rules()
            .map_err(|error| Error::invalid(error.to_string()))?
            .iter()
            .all(safe))
}

#[allow(clippy::too_many_arguments)]
fn finding_support(
    dataset: &Dataset,
    snapshot: Arc<Snapshot>,
    program: AnalysisProgram,
    options: &AnalysisOptions,
    control: &AnalysisControl,
    target: Finding,
    fixed: HashSet<[Id; 4]>,
    checks: &mut usize,
    request: &JustifyRequest,
) -> Result<JustifiedTarget> {
    let view = analysis::view(&snapshot, options)?;
    let graphs = analysis::input_graphs(&view, &program, options);
    let mut bytes = 0;
    let mut quads = Vec::new();
    view.for_each_quad(|quad| {
        control.check()?;
        if graphs.contains(&quad[3]) && !fixed.contains(quad) {
            hold(options.max_memory, &mut bytes, 64)?;
            quads.push(*quad);
        }
        Ok(())
    })?;
    quads.sort();
    // Storage IDs of generated witnesses change when an isolated closure allocates
    // them again. Only blank nodes actually present in the source inferred graph and
    // absent from asserted inputs are existential variables. Asserted blank identities
    // and unknown user-supplied labels remain constants.
    let mut target_blanks = HashSet::new();
    blank_names(&target.focus, &mut target_blanks);
    for (_, evidence) in &target.evidence {
        match evidence {
            Evidence::One(term) => blank_names(term, &mut target_blanks),
            Evidence::Many(terms) => {
                for term in terms {
                    blank_names(term, &mut target_blanks);
                }
            }
        }
    }
    for name in &target_blanks {
        hold(options.max_memory, &mut bytes, 128 + name.len() * 2)?;
    }
    for quad in quads.iter().chain(&fixed) {
        control.check()?;
        let quad = view.quad_to_terms(quad).unwrap();
        let mut names = HashSet::new();
        blank_names(&quad.subject.into(), &mut names);
        blank_names(&quad.object, &mut names);
        target_blanks.retain(|name| !names.contains(name));
    }
    let mut generated_blanks = HashSet::new();
    if !target_blanks.is_empty()
        && let Some(graph) = view.lookup_iri(super::INFERRED_GRAPH)
    {
        view.for_each_quad(|quad| {
            control.check()?;
            if quad[3] == graph {
                let quad = view.quad_to_terms(quad).unwrap();
                let mut names = HashSet::new();
                blank_names(&quad.subject.into(), &mut names);
                blank_names(&quad.object, &mut names);
                generated_blanks.extend(
                    names
                        .into_iter()
                        .filter(|name| target_blanks.contains(name)),
                );
            }
            Ok(())
        })?;
    }
    let monotone = monotone(&program)?;
    let mut search = Search {
        dataset,
        snapshot: view,
        program,
        options,
        control,
        quads,
        fixed,
        target: target.clone(),
        variables: generated_blanks,
        checks,
        max_checks: request.max_checks,
        bytes,
        stopped: false,
    };
    let (justifications, complete) = search.enumerate(request.max_justifications, monotone)?;
    Ok(JustifiedTarget {
        class: None,
        holds: !justifications.is_empty(),
        finding: Some(target),
        justifications,
        complete,
    })
}

#[allow(clippy::too_many_arguments)]
fn class_support(
    dataset: &Dataset,
    snapshot: &Arc<Snapshot>,
    program: &AnalysisProgram,
    options: &AnalysisOptions,
    control: &AnalysisControl,
    class: NamedNode,
    checks: &mut usize,
    request: &JustifyRequest,
) -> Result<JustifiedTarget> {
    let view = analysis::view(snapshot, options)?;
    let graphs = analysis::input_graphs(&view, program, options);
    let copy = dataset.store().copy_view_to_memory(
        &view,
        crate::store::StoreOptions {
            max_memory_bytes: options.max_memory.map(|limit| (limit / 8) as u64),
            result_cache_bytes: 0,
            service_cache_bytes: 0,
            ..Default::default()
        },
        &control.write_options(),
        |quad| Ok(graphs.contains(&quad[3])),
    )?;
    let copied = Dataset::from_store(copy);
    let graph = NamedNode::new(format!(
        "urn:x-sparkles:class-probe:{}",
        uuid::Uuid::new_v4()
    ))
    .unwrap();
    let mut transaction = copied
        .store()
        .write_with(crate::commit::CommitKind::Update, control.write_options());
    let probe = transaction.new_bnode();
    let predicate = transaction.intern(&oxrdf::vocab::rdf::TYPE.into_owned().into())?;
    let object = transaction.intern(&class.clone().into())?;
    let graph_id = transaction.intern(&graph.clone().into())?;
    let probe_quad = [probe, predicate, object, graph_id];
    transaction.insert(probe_quad)?;
    transaction.commit()?;
    let snapshot = copied.store().snapshot();
    let probe = snapshot.term(probe).unwrap();
    let mut program = program.clone();
    program.inputs = InputsForProbe::inputs(&view, &graphs, graph);
    let mut options = options.clone();
    options.access = None;
    if *checks >= request.max_checks {
        return Ok(JustifiedTarget {
            class: Some(class),
            finding: None,
            holds: false,
            justifications: Vec::new(),
            complete: false,
        });
    }
    *checks += 1;
    let state = analysis::analyze(
        &copied,
        &snapshot,
        &program,
        &options,
        control,
        0,
        None,
        &[],
    )?;
    let finding = state
        .diagnostics
        .findings
        .iter()
        .find(|finding| {
            finding.severity == Severity::Inconsistency
                && (finding.focus == probe
                    || finding.evidence.iter().any(|(_, value)| match value {
                        Evidence::One(term) => term == &probe,
                        Evidence::Many(terms) => terms.contains(&probe),
                    }))
        })
        .cloned();
    let complete = state
        .diagnostics
        .checks
        .iter()
        .all(|check| check_complete(&state.diagnostics, check.id));
    drop(state);
    let Some(finding) = finding else {
        return Ok(JustifiedTarget {
            class: Some(class),
            finding: None,
            holds: false,
            justifications: Vec::new(),
            complete,
        });
    };
    let mut result = finding_support(
        &copied,
        snapshot,
        program,
        &options,
        control,
        finding,
        HashSet::from([probe_quad]),
        checks,
        request,
    )?;
    result.class = Some(class);
    Ok(result)
}

struct InputsForProbe;
impl InputsForProbe {
    fn inputs(snapshot: &Snapshot, graphs: &[Id], probe: NamedNode) -> sparkles_reasoner::Inputs {
        use sparkles_reasoner::{GraphRef, ImportMode, Inputs};
        let mut selected: Vec<_> = graphs
            .iter()
            .filter_map(|graph| {
                if *graph == Id::DEFAULT_GRAPH {
                    Some(GraphRef::Default)
                } else {
                    match snapshot.term(*graph) {
                        Some(Term::NamedNode(node)) => Some(GraphRef::Named(node.into_string())),
                        _ => None,
                    }
                }
            })
            .collect();
        selected.push(GraphRef::Named(probe.into_string()));
        Inputs {
            data_graphs: selected,
            ontology_graphs: Vec::new(),
            imports: ImportMode::None,
            location_mapping: Default::default(),
        }
    }
}

pub fn justify(dataset: &Dataset, request: &JustifyRequest) -> Result<JustificationReport> {
    let control = AnalysisControl::new(&request.analysis)?;
    let snapshot = super::diff::resolve(dataset, &request.at, &control)?;
    let program = analysis::program(dataset, &request.analysis)?;
    let mut checks = 0;
    let mut results = Vec::new();
    let mut exhausted = true;
    match &request.target {
        JustifyTarget::Finding(finding) => results.push(finding_support(
            dataset,
            snapshot.clone(),
            program,
            &request.analysis,
            &control,
            finding.clone(),
            HashSet::new(),
            &mut checks,
            request,
        )?),
        JustifyTarget::Class(class) => results.push(class_support(
            dataset,
            &snapshot,
            &program,
            &request.analysis,
            &control,
            class.clone(),
            &mut checks,
            request,
        )?),
        JustifyTarget::AllClasses => {
            if request.max_checks == 0 {
                exhausted = false;
            } else {
                checks += 1;
                let state = analysis::analyze(
                    dataset,
                    &snapshot,
                    &program,
                    &request.analysis,
                    &control,
                    0,
                    None,
                    &["unsatisfiable-class".into()],
                )?;
                let classes = relevant_classes(
                    &state.closure,
                    &control,
                    request.analysis.max_memory,
                    state.bytes,
                )?;
                drop(state);
                for class in classes {
                    if checks >= request.max_checks {
                        exhausted = false;
                        break;
                    }
                    results.push(class_support(
                        dataset,
                        &snapshot,
                        &program,
                        &request.analysis,
                        &control,
                        class,
                        &mut checks,
                        request,
                    )?);
                }
            }
        }
    }
    let complete = exhausted && results.iter().all(|result| result.complete);
    Ok(JustificationReport {
        commit: snapshot.commit,
        results,
        checks,
        complete,
    })
}

impl JustifyRequest {
    pub fn from_json(value: &Value) -> Result<Self> {
        use sparkles_reasoner::diagnostics::{Basis, CHECKS};
        let targets = [
            value.get("finding"),
            value.get("class"),
            value.get("classes"),
        ]
        .into_iter()
        .filter(Option::is_some)
        .count();
        if targets != 1 {
            return Err(Error::invalid(
                "name exactly one finding, class or classes target",
            ));
        }
        let target = if let Some(class) = value.get("class") {
            JustifyTarget::Class(
                NamedNode::new(
                    class
                        .as_str()
                        .ok_or_else(|| Error::invalid("class must be an IRI string"))?,
                )
                .map_err(|error| Error::invalid(error.to_string()))?,
            )
        } else if let Some(classes) = value.get("classes") {
            if classes != "all" {
                return Err(Error::invalid("classes must be all"));
            }
            JustifyTarget::AllClasses
        } else {
            let finding = &value["finding"];
            let check = CHECKS
                .iter()
                .find(|check| Some(check.id) == finding.get("check").and_then(Value::as_str))
                .ok_or_else(|| Error::invalid("finding.check must name a diagnostics check"))?;
            let focus = super::explain::term_from_json(
                finding
                    .get("focus")
                    .ok_or_else(|| Error::invalid("finding.focus is missing"))?,
                "finding.focus",
            )?;
            let object = finding
                .get("evidence")
                .and_then(Value::as_object)
                .ok_or_else(|| Error::invalid("finding.evidence must be an object"))?;
            const KEYS: &[&str] = &[
                "type",
                "classes",
                "axiom",
                "other",
                "property",
                "values",
                "path",
                "restriction",
                "class",
                "value",
                "individuals",
                "properties",
                "target",
                "range",
                "literal",
            ];
            let mut evidence = Vec::new();
            for (key, value) in object {
                let key = *KEYS
                    .iter()
                    .find(|&&known| known == key)
                    .ok_or_else(|| Error::invalid(format!("unknown finding evidence {key}")))?;
                let evidence_value = match value.as_array() {
                    Some(values) => Evidence::Many(
                        values
                            .iter()
                            .map(|value| super::explain::term_from_json(value, key))
                            .collect::<Result<Vec<_>>>()?,
                    ),
                    None => Evidence::One(super::explain::term_from_json(value, key)?),
                };
                evidence.push((key, evidence_value));
            }
            let rule = finding
                .get("rule")
                .and_then(Value::as_str)
                .and_then(|rule| check.rules.iter().find(|&&known| known == rule).copied())
                .unwrap_or(check.rules[0]);
            JustifyTarget::Finding(Finding {
                check: check.id,
                rule,
                severity: check.severity,
                focus,
                evidence,
                basis: Basis::Asserted,
                message: String::new(),
            })
        };
        let mut request = Self::new(target);
        request.analysis = AnalysisOptions::from_json(value)?;
        request.at = value
            .get("at")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| Error::invalid("at must be a state selector"))
            })
            .transpose()?
            .unwrap_or("head")
            .to_string();
        request.max_checks = analysis::positive(value, "maxChecks", 500, 100_000)?;
        request.max_justifications = analysis::positive(value, "maxJustifications", 5, 100)?;
        Ok(request)
    }
}

fn blank_names(term: &Term, names: &mut HashSet<String>) {
    match term {
        Term::BlankNode(node) => {
            names.insert(node.as_str().to_string());
        }
        Term::Triple(triple) => {
            blank_names(&triple.subject.clone().into(), names);
            blank_names(&triple.object, names);
        }
        _ => {}
    }
}

fn matches_finding(
    target: &Finding,
    candidate: &Finding,
    variables: &HashSet<String>,
    control: &AnalysisControl,
) -> Result<bool> {
    use std::collections::HashMap;
    if variables.is_empty() {
        return Ok(finding_key(target) == finding_key(candidate));
    }
    if target.check != candidate.check || target.evidence.len() != candidate.evidence.len() {
        return Ok(false);
    }
    fn term(
        target: &Term,
        candidate: &Term,
        variables: &HashSet<String>,
        mapping: &mut HashMap<String, String>,
    ) -> bool {
        if let Term::BlankNode(node) = target
            && variables.contains(node.as_str())
        {
            let Term::BlankNode(other) = candidate else {
                return false;
            };
            if let Some(previous) = mapping.get(node.as_str()) {
                return previous == other.as_str();
            }
            // Distinct anonymous witnesses remain distinct under the renaming.
            if mapping.values().any(|value| value == other.as_str()) {
                return false;
            }
            mapping.insert(node.as_str().to_string(), other.as_str().to_string());
            return true;
        }
        if let (Term::Triple(target), Term::Triple(candidate)) = (target, candidate) {
            return target.predicate == candidate.predicate
                && term(
                    &target.subject.clone().into(),
                    &candidate.subject.clone().into(),
                    variables,
                    mapping,
                )
                && term(&target.object, &candidate.object, variables, mapping);
        }
        target == candidate
    }
    type Group<'a> = (Vec<&'a Term>, Vec<&'a Term>);
    fn groups_match(
        groups: &[Group<'_>],
        variables: &HashSet<String>,
        mapping: &mut HashMap<String, String>,
        used: &mut [bool],
        group_index: usize,
        term_index: usize,
        control: &AnalysisControl,
    ) -> Result<bool> {
        control.check()?;
        if group_index == groups.len() {
            return Ok(true);
        }
        let (target, candidate) = &groups[group_index];
        if term_index == target.len() {
            let size = groups
                .get(group_index + 1)
                .map_or(0, |(_, terms)| terms.len());
            return groups_match(
                groups,
                variables,
                mapping,
                &mut vec![false; size],
                group_index + 1,
                0,
                control,
            );
        }
        for (position, value) in candidate.iter().enumerate() {
            if used[position] {
                continue;
            }
            let mut next = mapping.clone();
            if term(target[term_index], value, variables, &mut next) {
                used[position] = true;
                if groups_match(
                    groups,
                    variables,
                    &mut next,
                    used,
                    group_index,
                    term_index + 1,
                    control,
                )? {
                    *mapping = next;
                    return Ok(true);
                }
                used[position] = false;
            }
        }
        Ok(false)
    }
    let mut mapping = HashMap::new();
    if !term(&target.focus, &candidate.focus, variables, &mut mapping) {
        return Ok(false);
    }
    let mut groups = Vec::new();
    for (name, evidence) in &target.evidence {
        let Some((_, other)) = candidate.evidence.iter().find(|(key, _)| key == name) else {
            return Ok(false);
        };
        let target: Vec<_> = match evidence {
            Evidence::One(term) => vec![term],
            Evidence::Many(terms) => terms.iter().collect(),
        };
        let candidate: Vec<_> = match other {
            Evidence::One(term) => vec![term],
            Evidence::Many(terms) => terms.iter().collect(),
        };
        if target.len() != candidate.len() {
            return Ok(false);
        }
        groups.push((target, candidate));
    }
    let size = groups.first().map_or(0, |(_, terms)| terms.len());
    groups_match(
        &groups,
        variables,
        &mut mapping,
        &mut vec![false; size],
        0,
        0,
        control,
    )
}

#[derive(Clone, Debug)]
pub struct JustifiedDiagnostics {
    pub diagnostics: super::run::Diagnostics,
    pub supports: JustificationReport,
}

impl JustifiedDiagnostics {
    /// Add each finding's first support and completeness to an existing diagnostics
    /// JSON document, preserving its freshness and scope metadata.
    pub fn attach(&self, document: &mut Value) {
        let supports = self.supports.to_json();
        if let Some(findings) = document.get_mut("findings").and_then(Value::as_array_mut) {
            for (index, finding) in findings.iter_mut().enumerate() {
                let support = &supports["results"][index];
                finding["justification"] = support["justifications"][0].clone();
                finding["justificationComplete"] =
                    support["complete"].as_bool().unwrap_or(false).into();
            }
        }
        document["justificationsComplete"] = self.supports.complete.into();
        document["justificationChecks"] = self.supports.checks.into();
    }
}

/// Diagnostics and their first justifications use one retained state, program,
/// deadline, cancellation flag and oracle-check allowance.
pub fn diagnose(
    dataset: &Dataset,
    options: &sparkles_reasoner::diagnostics::DiagnoseOptions,
    analysis: &AnalysisOptions,
    max_checks: usize,
) -> Result<JustifiedDiagnostics> {
    let mut analysis = analysis.clone();
    analysis.timeout = match (analysis.timeout, options.timeout) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let control = AnalysisControl::new(&analysis)?;
    let snapshot = analysis::view(&dataset.store().snapshot(), &analysis)?;
    let record = dataset.reasoning().record();
    let state = dataset.state();
    let diagnostics = super::run::diagnose_snapshot_with(
        dataset.store(),
        record.as_ref(),
        Some(&state.closure),
        options,
        snapshot.clone(),
        &sparkles_reasoner::diagnostics::DiagnosticControls {
            deadline: control.deadline,
            cancel: Some(control.cancel.clone()),
            max_memory: analysis.max_memory.map(|limit| (limit / 16) as u64),
        },
    )
    .map_err(|error| control.error(error))?;
    let program = if options.inferences && (analysis.program.is_some() || record.is_some()) {
        analysis::program(dataset, &analysis)?
    } else {
        use sparkles_reasoner::{GraphRef, ImportMode, Inputs};
        let graphs = if options.graphs.is_empty() {
            vec![GraphRef::Default]
        } else {
            options
                .graphs
                .iter()
                .map(|graph| GraphRef::parse(graph).map_err(Error::invalid))
                .collect::<Result<Vec<_>>>()?
        };
        AnalysisProgram {
            profile: sparkles_reasoner::Profile::Rules(String::new()),
            extras: Default::default(),
            inputs: Inputs {
                data_graphs: graphs,
                ontology_graphs: Vec::new(),
                imports: ImportMode::None,
                location_mapping: Default::default(),
            },
        }
    };
    let mut results = Vec::new();
    let mut checks = 0;
    let mut retained = 0;
    for finding in &diagnostics.report.findings {
        if checks >= max_checks || control.check().is_err() {
            if matches!(control.check(), Err(Error::Cancelled)) {
                return Err(Error::Cancelled);
            }
            results.push(JustifiedTarget {
                class: None,
                finding: Some(finding.clone()),
                holds: true,
                justifications: Vec::new(),
                complete: false,
            });
            continue;
        }
        let mut available = analysis.clone();
        available.max_memory = analysis
            .max_memory
            .map(|limit| {
                limit
                    .checked_sub(retained)
                    .filter(|left| *left > 0)
                    .ok_or_else(|| {
                        analysis::budget("diagnostic supports exhaust their memory limit")
                    })
            })
            .transpose()?;
        let mut request = JustifyRequest::new(JustifyTarget::Finding(finding.clone()));
        request.max_checks = max_checks;
        request.max_justifications = 1;
        let mut result = finding_support(
            dataset,
            snapshot.clone(),
            program.clone(),
            &available,
            &control,
            finding.clone(),
            HashSet::new(),
            &mut checks,
            &request,
        )?;
        // The endpoint asks for one certified support, not exhaustion of all
        // alternatives. Every returned support has already passed minimality checks.
        result.complete |= !result.justifications.is_empty();
        let size = result
            .justifications
            .iter()
            .flat_map(|support| &support.facts)
            .map(|fact| 256 + fact.quad.to_string().len() * 2)
            .sum();
        hold(analysis.max_memory, &mut retained, size)?;
        results.push(result);
    }
    let complete = results.iter().all(|result| result.complete);
    Ok(JustifiedDiagnostics {
        supports: JustificationReport {
            commit: snapshot.commit,
            results,
            checks,
            complete,
        },
        diagnostics,
    })
}

impl JustificationReport {
    /// Reified asserted quads preserve the support's graph without changing Turtle's
    /// single-graph syntax. Finding check/focus identify the diagnostic being justified.
    pub fn to_turtle(&self) -> String {
        let report = oxrdf::BlankNode::default();
        let literal = |value: &str| oxrdf::Literal::new_simple_literal(value).to_string();
        let mut text = format!(
            "\n{report} a <urn:x-sparkles:JustificationReport> ; <urn:x-sparkles:complete> {} .\n",
            self.complete
        );
        for result in &self.results {
            let target = oxrdf::BlankNode::default();
            text.push_str(&format!("{report} <urn:x-sparkles:target> {target} .\n{target} <urn:x-sparkles:holds> {} ; <urn:x-sparkles:complete> {} .\n",result.holds,result.complete));
            if let Some(class) = &result.class {
                text.push_str(&format!("{target} <urn:x-sparkles:class> {class} .\n"));
            }
            if let Some(finding) = &result.finding {
                text.push_str(&format!(
                    "{target} <urn:x-sparkles:check> {} ; <urn:x-sparkles:focus> {} .\n",
                    literal(finding.check),
                    finding.focus
                ));
            }
            for support in &result.justifications {
                let set = oxrdf::BlankNode::default();
                text.push_str(&format!(
                    "{target} <urn:x-sparkles:justification> {set} .\n"
                ));
                for fact in &support.facts {
                    let node = oxrdf::BlankNode::default();
                    text.push_str(&format!("{set} <urn:x-sparkles:fact> {node} .\n{node} <http://www.w3.org/1999/02/22-rdf-syntax-ns#subject> {} ; <http://www.w3.org/1999/02/22-rdf-syntax-ns#predicate> {} ; <http://www.w3.org/1999/02/22-rdf-syntax-ns#object> {} ; <urn:x-sparkles:role> {} ; <urn:x-sparkles:graph> {} .\n",fact.quad.subject,fact.quad.predicate,fact.quad.object,literal(fact.role),match &fact.quad.graph_name {GraphName::DefaultGraph=>"<urn:x-arq:DefaultGraph>".to_string(),graph=>graph.to_string()}));
                }
            }
        }
        text
    }
}

/// The named classes whose superclass cone reaches a contradiction axiom. Use the
/// computed schema so rules that derive subclass links and intersection classes count.
fn relevant_classes(
    facts: &analysis::Facts,
    control: &AnalysisControl,
    limit: Option<usize>,
    mut bytes: usize,
) -> Result<Vec<NamedNode>> {
    use std::collections::{BTreeMap, HashMap};
    const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    const OWL: &str = "http://www.w3.org/2002/07/owl#";
    let mut reverse: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut seeds = BTreeSet::from(["http://www.w3.org/2002/07/owl#Nothing"]);
    let mut first: HashMap<&Term, Vec<&Term>> = HashMap::new();
    let mut rest: HashMap<&Term, Vec<&Term>> = HashMap::new();
    let mut members: HashMap<&Term, Vec<&Term>> = HashMap::new();
    let mut disjoint = HashSet::new();
    for triple in facts.values() {
        control.check()?;
        if let Term::NamedNode(predicate) = &triple[1] {
            let predicate = predicate.as_str();
            match predicate {
                "http://www.w3.org/2000/01/rdf-schema#subClassOf"
                | "http://www.w3.org/2002/07/owl#equivalentClass" => {
                    if let (Term::NamedNode(sub), Term::NamedNode(sup)) = (&triple[0], &triple[2]) {
                        if reverse
                            .entry(sup.as_str())
                            .or_default()
                            .insert(sub.as_str())
                        {
                            hold(limit, &mut bytes, 96)?;
                        }
                        if predicate.ends_with("equivalentClass")
                            && reverse
                                .entry(sub.as_str())
                                .or_default()
                                .insert(sup.as_str())
                        {
                            hold(limit, &mut bytes, 96)?;
                        }
                    }
                }
                "http://www.w3.org/2002/07/owl#disjointWith"
                | "http://www.w3.org/2002/07/owl#complementOf" => {
                    for term in [&triple[0], &triple[2]] {
                        if let Term::NamedNode(class) = term
                            && seeds.insert(class.as_str())
                        {
                            hold(limit, &mut bytes, 64)?;
                        }
                    }
                }
                _ if predicate.strip_prefix(RDF) == Some("first") => {
                    first.entry(&triple[0]).or_default().push(&triple[2]);
                    hold(limit, &mut bytes, 64)?;
                }
                _ if predicate.strip_prefix(RDF) == Some("rest") => {
                    rest.entry(&triple[0]).or_default().push(&triple[2]);
                    hold(limit, &mut bytes, 64)?;
                }
                _ if predicate.strip_prefix(OWL) == Some("members") => {
                    members.entry(&triple[0]).or_default().push(&triple[2]);
                    hold(limit, &mut bytes, 64)?;
                }
                _ if predicate.strip_prefix(RDF) == Some("type")
                    && matches!(&triple[2],Term::NamedNode(node) if node.as_str().strip_prefix(OWL)==Some("AllDisjointClasses")) =>
                {
                    disjoint.insert(&triple[0]);
                    hold(limit, &mut bytes, 64)?;
                }
                _ => {}
            }
        }
    }
    let mut lists = VecDeque::new();
    for axiom in disjoint {
        if let Some(lists_of) = members.get(axiom) {
            lists.extend(lists_of.iter().copied());
        }
    }
    let mut visited = HashSet::new();
    while let Some(list) = lists.pop_front() {
        control.check()?;
        if !visited.insert(list) {
            continue;
        }
        hold(limit, &mut bytes, 64)?;
        if let Some(values) = first.get(list) {
            for value in values {
                if let Term::NamedNode(class) = value
                    && seeds.insert(class.as_str())
                {
                    hold(limit, &mut bytes, 64)?;
                }
            }
        }
        if let Some(tails) = rest.get(list) {
            lists.extend(tails.iter().copied());
        }
    }
    let mut queue: VecDeque<_> = seeds.iter().copied().collect();
    while let Some(class) = queue.pop_front() {
        control.check()?;
        if let Some(subclasses) = reverse.get(class) {
            for &subclass in subclasses {
                if seeds.insert(subclass) {
                    hold(limit, &mut bytes, 64)?;
                    queue.push_back(subclass);
                }
            }
        }
    }
    let mut classes = Vec::new();
    for name in seeds {
        hold(limit, &mut bytes, 64 + name.len())?;
        classes.push(NamedNode::new_unchecked(name));
    }
    Ok(classes)
}
