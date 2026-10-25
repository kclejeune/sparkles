//! Shared controls and isolated states for entailment diffs and justifications.

use crate::Dataset;
use crate::access::GraphAccess;
use crate::error::{ComponentError, Error, Result};
use crate::guard::WriteOptions;
use crate::store::Snapshot;
use sparkles_reasoner::{Extras, Inputs, Profile};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The same program is applied to every compared or minimized state.
#[derive(Clone, Debug)]
pub struct AnalysisProgram {
    pub profile: Profile,
    pub extras: Extras,
    pub inputs: Inputs,
}

/// Resource and visibility controls shared by one complete analysis request.
#[derive(Clone, Default)]
pub struct AnalysisOptions {
    pub program: Option<AnalysisProgram>,
    pub access: Option<Arc<GraphAccess>>,
    pub timeout: Option<Duration>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub max_memory: Option<usize>,
    pub max_inferred: Option<usize>,
    pub threads: Option<usize>,
}

pub(crate) fn budget(message: impl Into<String>) -> Error {
    let message: String = message.into();
    Error::Component(Box::new(ComponentError::new(
        "reasoner",
        super::explain::REASONING_BUDGET,
        message,
    )))
}

/// Relay deadlines to the existing reasoner cancellation checks without changing the
/// caller's cancellation flag. Every exit, including unwinding, joins the monitor.
pub(crate) struct AnalysisControl {
    pub deadline: Option<Instant>,
    pub cancel: Arc<AtomicBool>,
    caller: Option<Arc<AtomicBool>>,
    stop: Option<mpsc::Sender<()>>,
    monitor: Option<std::thread::JoinHandle<()>>,
}

impl Drop for AnalysisControl {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
    }
}

impl AnalysisControl {
    pub fn new(options: &AnalysisOptions) -> Result<Self> {
        let deadline =
            Instant::now().checked_add(options.timeout.unwrap_or(Duration::from_secs(30)));
        let cancel = Arc::new(AtomicBool::new(false));
        let caller = options.cancel.clone();
        let mut control = Self {
            deadline,
            cancel,
            caller,
            stop: None,
            monitor: None,
        };
        control.check()?;
        let (send, receive) = mpsc::channel();
        let (stop_flag, caller) = (control.cancel.clone(), control.caller.clone());
        let monitor = std::thread::spawn(move || {
            loop {
                match receive.recv_timeout(Duration::from_millis(5)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if caller
                            .as_ref()
                            .is_some_and(|flag| flag.load(Ordering::Relaxed))
                            || deadline.is_some_and(|limit| Instant::now() >= limit)
                        {
                            stop_flag.store(true, Ordering::Relaxed);
                            break;
                        }
                    }
                }
            }
        });
        control.stop = Some(send);
        control.monitor = Some(monitor);
        Ok(control)
    }

    pub fn check(&self) -> Result<()> {
        if self
            .caller
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(Error::Cancelled);
        }
        if self.deadline.is_some_and(|limit| Instant::now() >= limit) {
            return Err(budget("the entailment analysis passed its timeout"));
        }
        Ok(())
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|limit| limit.saturating_duration_since(Instant::now()))
    }

    pub fn write_options(&self) -> WriteOptions {
        WriteOptions {
            cancel: Some(self.cancel.clone()),
            deadline: self.deadline,
            ..Default::default()
        }
    }

    pub fn error(&self, error: Error) -> Error {
        if let Err(stopped) = self.check() {
            return stopped;
        }
        match error {
            Error::StorageFull(message) => budget(message),
            error => error,
        }
    }
}

pub(crate) fn program(dataset: &Dataset, options: &AnalysisOptions) -> Result<AnalysisProgram> {
    if let Some(program) = &options.program {
        program
            .inputs
            .validate()
            .map_err(crate::handles::from_anyhow)?;
        let mut program = program.clone();
        // An explicitly supplied query-time program includes backward Horn rules.
        // The full-closure fallback evaluates their semantic body/head orientation.
        // Recorded materialization programs retain the materializer's own semantics.
        if let Profile::Rules(_) = &program.profile {
            fn forward(rule: &mut sparkles_reasoner::Rule) -> bool {
                let mut changed = rule.direction == sparkles_reasoner::Direction::Backward;
                rule.direction = sparkles_reasoner::Direction::Forward;
                for clause in rule.body.iter_mut().chain(&mut rule.head) {
                    if let sparkles_reasoner::Clause::Rule(nested) = clause {
                        changed |= forward(nested);
                    }
                }
                changed
            }
            let mut rules = program
                .profile
                .rules()
                .map_err(|error| Error::invalid(error.to_string()))?;
            let mut changed = false;
            for rule in &mut rules {
                changed |= forward(rule);
            }
            if changed {
                program.profile = Profile::Rules(
                    rules
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        }
        return Ok(program);
    }
    let record = dataset.reasoning().record().ok_or_else(|| {
        Error::invalid(
            "entailment analysis needs an explicit program or a recorded materialization",
        )
    })?;
    Ok(AnalysisProgram {
        profile: record.profile()?,
        extras: record.extras()?,
        inputs: record.run_inputs()?,
    })
}

pub(crate) fn view(snapshot: &Arc<Snapshot>, options: &AnalysisOptions) -> Result<Arc<Snapshot>> {
    match &options.access {
        Some(access) => access.bound(snapshot).masked(snapshot),
        None => Ok(snapshot.clone()),
    }
}

pub(crate) fn input_graphs(
    snapshot: &Arc<Snapshot>,
    program: &AnalysisProgram,
    options: &AnalysisOptions,
) -> Vec<crate::id::Id> {
    let readable = |graph| {
        options
            .access
            .as_ref()
            .is_none_or(|access| access.bound(snapshot).readable_id(snapshot, graph))
    };
    sparkles_reasoner::inputs::resolve_readable(snapshot, &program.inputs, &readable)
        .ids(snapshot)
        .into_iter()
        .filter(|&graph| readable(graph))
        .collect()
}

pub(crate) type Facts = std::collections::BTreeMap<String, [oxrdf::Term; 3]>;

pub(crate) fn fact_key(triple: &[oxrdf::Term; 3]) -> String {
    format!("{} {} {}", triple[0], triple[1], triple[2])
}

pub(crate) fn fact_bytes(key: &str, triple: &[oxrdf::Term; 3]) -> usize {
    96 + key.len() + triple.iter().map(crate::explain::term_bytes).sum::<usize>()
}

pub(crate) struct State {
    pub asserted: Facts,
    pub closure: Facts,
    pub diagnostics: sparkles_reasoner::diagnostics::DiagnosticsReport,
    pub bytes: usize,
    pub warnings: Vec<String>,
}

pub(crate) fn hold(limit: Option<usize>, bytes: &mut usize, additional: usize) -> Result<()> {
    let next = bytes
        .checked_add(additional)
        .ok_or_else(|| budget("the entailment analysis memory size overflowed"))?;
    if limit.is_some_and(|limit| next > limit) {
        return Err(budget("the entailment analysis passed its memory limit"));
    }
    *bytes = next;
    Ok(())
}

/// Full-closure fallback on a private writable copy. Data and blank identities come
/// from the immutable caller view; only the resolved visible inputs reach the copy.
#[allow(clippy::too_many_arguments)]
pub(crate) fn analyze(
    dataset: &Dataset,
    snapshot: &Arc<Snapshot>,
    program: &AnalysisProgram,
    options: &AnalysisOptions,
    control: &AnalysisControl,
    retained: usize,
    subset: Option<&std::collections::HashSet<[crate::id::Id; 4]>>,
    checks: &[String],
) -> Result<State> {
    use crate::id::Id;
    use sparkles_reasoner::{GraphRef, ImportMode};
    control.check()?;
    let snapshot = view(snapshot, options)?;
    let graphs = input_graphs(&snapshot, program, options);
    let available = options
        .max_memory
        .map(|limit| {
            limit
                .checked_sub(retained)
                .filter(|left| *left > 0)
                .ok_or_else(|| budget("the retained analysis result exhausts its memory limit"))
        })
        .transpose()?;
    // The data copy, reasoner, diagnostic query and retained output each have a
    // separate share. Their overlapping lifetimes stay inside one request grant.
    let share = available.map(|bytes| bytes / 8);
    let copied = dataset
        .store()
        .copy_view_to_memory(
            &snapshot,
            crate::store::StoreOptions {
                max_memory_bytes: share.map(|bytes| bytes as u64),
                result_cache_bytes: 0,
                service_cache_bytes: 0,
                union_default_graph: false,
                ..Default::default()
            },
            &control.write_options(),
            |quad| {
                Ok(graphs.contains(&quad[3]) && subset.is_none_or(|subset| subset.contains(quad)))
            },
        )
        .map_err(|error| control.error(error))?;
    let copied = Dataset::from_store(copied);
    let before = copied.store().snapshot();
    let mut asserted = Facts::new();
    let mut bytes = 0;
    before.for_each_quad(|quad| {
        control.check()?;
        let triple = [
            before.term(quad[0]).unwrap(),
            before.term(quad[1]).unwrap(),
            before.term(quad[2]).unwrap(),
        ];
        let key = fact_key(&triple);
        if let std::collections::btree_map::Entry::Vacant(entry) = asserted.entry(key) {
            hold(share, &mut bytes, fact_bytes(entry.key(), &triple))?;
            entry.insert(triple);
        }
        Ok(())
    })?;
    drop(before);
    let mut selected: Vec<_> = graphs
        .iter()
        .filter_map(|graph| {
            if *graph == Id::DEFAULT_GRAPH {
                Some(GraphRef::Default)
            } else {
                match snapshot.term(*graph) {
                    Some(oxrdf::Term::NamedNode(node)) => Some(GraphRef::Named(node.into_string())),
                    _ => None,
                }
            }
        })
        .collect();
    if selected.is_empty() {
        selected.push(GraphRef::Default);
    }
    let input_names: Vec<_> = selected
        .iter()
        .map(|graph| graph.as_str().to_string())
        .collect();
    let request = sparkles_reasoner::ReasonOptions {
        inputs: Inputs {
            data_graphs: selected,
            ontology_graphs: Vec::new(),
            imports: ImportMode::None,
            location_mapping: Default::default(),
        },
        max_memory: share,
        max_inferred: options
            .max_inferred
            .unwrap_or(sparkles_reasoner::DEFAULT_MAX_INFERRED),
        threads: options.threads,
        cancel: Some(control.cancel.clone()),
        pool: super::reason_pool(),
        ..Default::default()
    };
    let outcome = sparkles_reasoner::materialize_incremental(
        copied.store(),
        &program.profile,
        &program.extras,
        sparkles_reasoner::Incremental {
            since: None,
            cache: Some(&copied.state().closure),
        },
        &request,
    )
    .map_err(|error| {
        if let Some(error) = error.downcast_ref::<sparkles_reasoner::MemoryLimitExceeded>() {
            budget(error.to_string())
        } else if let Some(error) = error.downcast_ref::<sparkles_reasoner::InferredLimitExceeded>()
        {
            budget(error.to_string())
        } else {
            control.error(crate::handles::from_anyhow(error))
        }
    })?;
    control.check()?;
    let snapshot = copied.store().snapshot();
    let mut closure = Facts::new();
    snapshot.for_each_quad(|quad| {
        control.check()?;
        let triple = [
            snapshot.term(quad[0]).unwrap(),
            snapshot.term(quad[1]).unwrap(),
            snapshot.term(quad[2]).unwrap(),
        ];
        let key = fact_key(&triple);
        if let std::collections::btree_map::Entry::Vacant(entry) = closure.entry(key) {
            hold(share, &mut bytes, fact_bytes(entry.key(), &triple))?;
            entry.insert(triple);
        }
        Ok(())
    })?;
    let typings = sparkles_reasoner::literal_typings(
        copied.store(),
        snapshot.commit,
        Some(&copied.state().closure),
    )
    .map_err(crate::handles::from_anyhow)?;
    let diagnostics = sparkles_reasoner::diagnostics::diagnose_controlled(
        snapshot,
        &sparkles_reasoner::diagnostics::DiagnoseOptions {
            checks: checks.to_vec(),
            inferences: true,
            graphs: input_names,
            limit: sparkles_reasoner::diagnostics::MAX_LIMIT,
            timeout: control.remaining(),
            prefixes: dataset.store().prefixes().into_iter().collect(),
            ..Default::default()
        },
        typings.as_ref(),
        &sparkles_reasoner::diagnostics::DiagnosticControls {
            deadline: control.deadline,
            cancel: Some(control.cancel.clone()),
            max_memory: available.map(|bytes| (bytes / 16) as u64),
        },
    )
    .map_err(|error| control.error(crate::handles::from_anyhow(error)))?;
    control.check()?;
    for finding in &diagnostics.findings {
        let mut size = 256 + finding.message.len() + crate::explain::term_bytes(&finding.focus);
        for (name, evidence) in &finding.evidence {
            size += name.len();
            size += match evidence {
                sparkles_reasoner::diagnostics::Evidence::One(term) => {
                    crate::explain::term_bytes(term)
                }
                sparkles_reasoner::diagnostics::Evidence::Many(terms) => {
                    terms.iter().map(crate::explain::term_bytes).sum()
                }
            };
        }
        hold(share, &mut bytes, size)?;
    }
    Ok(State {
        asserted,
        closure,
        diagnostics,
        bytes,
        warnings: outcome.warnings,
    })
}

impl AnalysisOptions {
    /// Parse controls and an optional explicit program from a public JSON request.
    /// Timeouts use seconds, as explanation and diagnostic requests do.
    pub fn from_json(value: &serde_json::Value) -> Result<Self> {
        use serde_json::Value;
        let object = value
            .as_object()
            .ok_or_else(|| Error::invalid("the analysis request must be an object"))?;
        let mut options = Self::default();
        if let Some(timeout) = object.get("timeout").filter(|value| !value.is_null()) {
            options.timeout = Some(
                timeout
                    .as_f64()
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .and_then(|value| Duration::try_from_secs_f64(value).ok())
                    .ok_or_else(|| {
                        Error::invalid("timeout must be a positive number of seconds")
                    })?,
            );
        }
        for (name, destination) in [
            ("maxMemory", &mut options.max_memory),
            ("maxInferred", &mut options.max_inferred),
            ("threads", &mut options.threads),
        ] {
            if let Some(value) = object.get(name).filter(|value| !value.is_null()) {
                *destination = Some(
                    value
                        .as_u64()
                        .and_then(|value| usize::try_from(value).ok())
                        .filter(|value| *value > 0)
                        .ok_or_else(|| {
                            Error::invalid(format!("{name} must be a positive integer"))
                        })?,
                );
            }
        }
        if [
            "profile",
            "rules",
            "inputs",
            "vocabularies",
            "geoDefaultGeometry",
        ]
        .iter()
        .any(|name| object.get(*name).is_some_and(|value| !value.is_null()))
        {
            let profile = match object.get("profile").and_then(Value::as_str) {
                Some("rules") | None
                    if object.get("rules").is_some_and(|value| !value.is_null()) =>
                {
                    Profile::Rules(
                        object
                            .get("rules")
                            .and_then(Value::as_str)
                            .ok_or_else(|| Error::invalid("rules must be rule text"))?
                            .to_string(),
                    )
                }
                Some(profile) => {
                    profile
                        .parse()
                        .map_err(|error: sparkles_reasoner::UnknownProfile| {
                            Error::invalid(error.to_string())
                        })?
                }
                None => {
                    return Err(Error::invalid(
                        "an explicit analysis program needs profile or rules",
                    ));
                }
            };
            if object.get("rules").is_some_and(|value| !value.is_null())
                && !matches!(profile, Profile::Rules(_))
            {
                return Err(Error::invalid("rules applies only to the rules profile"));
            }
            let inputs = object
                .get("inputs")
                .filter(|value| !value.is_null())
                .map(|value| serde_json::from_value::<Inputs>(value.clone()))
                .transpose()
                .map_err(|error| Error::invalid(format!("invalid inputs: {error}")))?
                .unwrap_or_default();
            inputs.validate().map_err(crate::handles::from_anyhow)?;
            let vocabularies = object
                .get("vocabularies")
                .filter(|value| !value.is_null())
                .map(|value| serde_json::from_value::<Vec<String>>(value.clone()))
                .transpose()
                .map_err(|error| Error::invalid(format!("invalid vocabularies: {error}")))?
                .unwrap_or_default();
            let geo = match object
                .get("geoDefaultGeometry")
                .filter(|value| !value.is_null())
            {
                None => false,
                Some(Value::Bool(value)) => *value,
                _ => return Err(Error::invalid("geoDefaultGeometry must be a boolean")),
            };
            let extras = Extras::parse(&vocabularies, geo)
                .map_err(|error| Error::invalid(error.to_string()))?;
            options.program = Some(AnalysisProgram {
                profile,
                extras,
                inputs,
            });
        }
        Ok(options)
    }
}

pub(crate) fn positive(
    value: &serde_json::Value,
    name: &str,
    default: usize,
    maximum: usize,
) -> Result<usize> {
    value
        .get(name)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value > 0 && *value <= maximum)
                .ok_or_else(|| {
                    Error::invalid(format!("{name} must be an integer from 1 to {maximum}"))
                })
        })
        .unwrap_or(Ok(default))
}
