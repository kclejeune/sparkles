//! Shared controls and isolated states for entailment diffs and justifications.

use super::memory::Memory;
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
    witnesses: std::sync::OnceLock<Arc<sparkles_reasoner::analysis::WitnessMap>>,
    witness_memory: Option<usize>,
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
            witnesses: Default::default(),
            witness_memory: options.max_memory.map(|bytes| bytes / 8),
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
            // the rewrite below drops the included sets, which the program's check reads
            sparkles_reasoner::parser::check_program_includes(&rules)
                .map_err(|error| Error::invalid(format!("{error:#}")))?;
            let mut changed = false;
            for rule in &mut rules {
                changed |= forward(rule);
            }
            if changed {
                // with the declarations of the relations, so that a rule program reads back
                program.profile = Profile::Rules(sparkles_reasoner::parser::rules_text(&rules));
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

pub(crate) fn input_graphs_controlled(
    snapshot: &Arc<Snapshot>,
    program: &AnalysisProgram,
    options: &AnalysisOptions,
    control: &AnalysisControl,
    max_memory: Option<usize>,
    memory_parent: Option<sparkles_reasoner::ParentHandle>,
) -> Result<(Vec<crate::id::Id>, usize)> {
    let bound = options
        .access
        .as_ref()
        .filter(|access| !access.reads_all())
        .map(|access| access.bound(snapshot));
    let mut check = || control.check().map_err(anyhow::Error::new);
    sparkles_reasoner::inputs::resolve_ids_controlled_with_parent(
        snapshot,
        &program.inputs,
        bound.as_deref(),
        max_memory,
        memory_parent,
        &mut check,
    )
    .map_err(|error| materialization_error(control, error))
}

pub(crate) fn graph_refs_controlled(
    snapshot: &Snapshot,
    graphs: &[crate::id::Id],
    extra: &[sparkles_reasoner::GraphRef],
    max_memory: Option<usize>,
    memory_parent: Option<sparkles_reasoner::ParentHandle>,
    control: &AnalysisControl,
) -> Result<(Vec<sparkles_reasoner::GraphRef>, usize)> {
    let mut check = || control.check().map_err(anyhow::Error::new);
    sparkles_reasoner::inputs::graph_refs_controlled_with_parent(
        snapshot,
        graphs,
        extra,
        max_memory,
        memory_parent,
        &mut check,
    )
    .map_err(|error| materialization_error(control, error))
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
    _memory: Memory,
    _diagnostics_memory: sparkles_reasoner::diagnostics::DiagnosticMemory,
}

fn hold_state(
    limit: Option<usize>,
    bytes: &mut usize,
    memory: &mut Memory,
    additional: usize,
) -> Result<()> {
    hold(limit, bytes, additional)?;
    memory.grow(additional as u64)
}

fn literal_typing_bytes(typings: &sparkles_reasoner::diagnostics::GuardedLiteralTypings) -> usize {
    typings
        .facts
        .iter()
        .map(|(literal, datatype)| {
            128 + literal.value().len() * 2
                + literal.language().map_or(0, str::len) * 2
                + literal.datatype().as_str().len() * 2
                + datatype.as_str().len() * 2
        })
        .sum()
}

fn diagnostic_bytes(finding: &sparkles_reasoner::diagnostics::Finding) -> usize {
    use sparkles_reasoner::diagnostics::Evidence;
    256 + finding.message.len()
        + crate::explain::term_bytes(&finding.focus)
        + finding
            .evidence
            .iter()
            .map(|(name, evidence)| {
                name.len()
                    + match evidence {
                        Evidence::One(term) => crate::explain::term_bytes(term),
                        Evidence::Many(terms) => terms.iter().map(crate::explain::term_bytes).sum(),
                    }
            })
            .sum::<usize>()
}

fn decode_fact(
    snapshot: &Snapshot,
    quad: &[crate::id::Id; 4],
    control: &AnalysisControl,
    scratch: &mut Memory,
) -> Result<[oxrdf::Term; 3]> {
    let mut term = |i| {
        super::memory::term(snapshot, quad[i], scratch, &mut || control.check())
            .map_err(share_error)?
            .ok_or_else(|| Error::Corrupt("an entailment fact has an unknown term".into()))
    };
    let triple = [term(0)?, term(1)?, term(2)?];
    scratch
        .grow(triple.iter().map(crate::explain::term_bytes).sum::<usize>() as u64 * 2)
        .map_err(share_error)?;
    Ok(triple)
}

/// A decoding scratch that passes the limit of its analysis share has passed the
/// analysis's memory limit. A refusal of the shared server memory stays as it is.
pub(crate) fn share_error(error: Error) -> Error {
    match error {
        Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory => budget(format!(
            "the entailment analysis passed its memory limit: {b}"
        )),
        error => error,
    }
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

/// Read exact stored terms when the caller has proved both materializations fresh.
/// Optional closure caching only controls whether generalized diagnostics need a
/// private recomputation; it never changes the stored graph being compared.
pub(crate) fn stored(
    dataset: &Dataset,
    snapshot: &Arc<Snapshot>,
    program: &AnalysisProgram,
    options: &AnalysisOptions,
    control: &AnalysisControl,
    retained: usize,
) -> Result<State> {
    use sparkles_reasoner::diagnostics::{DiagnoseOptions, DiagnosticControls, MAX_LIMIT};
    let pool = dataset.state().pool.clone();
    let parent = pool
        .as_ref()
        .map(crate::sparql::pool::QueryPool::work_parent);
    let mut memory = Memory::new(None, parent.clone());
    let cache = &dataset.state().closure;
    let share = options.max_memory.map(|limit| {
        limit
            .saturating_sub(retained)
            .saturating_sub(control.witness_memory.unwrap_or(0))
            / 8
    });
    let typings = sparkles_reasoner::literal_typings_controlled_guarded(
        dataset.store(),
        snapshot.commit,
        Some(cache),
        &DiagnosticControls {
            deadline: control.deadline,
            cancel: Some(control.cancel.clone()),
            max_memory: share.map(|bytes| bytes as u64),
            access: None,
            memory_parent: pool.as_ref().map(|p| p.work_parent()),
        },
    )
    .map_err(|error| materialization_error(control, error))?;
    let summary_complete = typings.is_some();
    let typings = typings.unwrap_or_default();
    let mut asserted = Facts::new();
    let mut closure = Facts::new();
    let typings_bytes = literal_typing_bytes(&typings);
    let mut bytes = 0;
    hold(share, &mut bytes, typings_bytes)?;
    let (graphs, graphs_bytes) = input_graphs_controlled(
        snapshot,
        program,
        options,
        control,
        share.map(|limit| limit.saturating_sub(bytes)),
        pool.as_ref().map(|p| p.work_parent()),
    )?;
    hold_state(share, &mut bytes, &mut memory, graphs_bytes)?;
    let inferred = snapshot.lookup_iri(super::INFERRED_GRAPH);
    snapshot.for_each_quad(|quad| {
        control.check()?;
        let input = graphs.contains(&quad[3]);
        if !input && Some(quad[3]) != inferred {
            return Ok(());
        }
        let mut scratch = Memory::new(share.map(|n| n as u64), parent.clone());
        let triple = decode_fact(snapshot, quad, control, &mut scratch)?;
        let key = fact_key(&triple);
        if input && !asserted.contains_key(&key) {
            hold_state(share, &mut bytes, &mut memory, fact_bytes(&key, &triple))?;
            asserted.insert(key.clone(), triple.clone());
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = closure.entry(key) {
            hold_state(
                share,
                &mut bytes,
                &mut memory,
                fact_bytes(entry.key(), &triple),
            )?;
            entry.insert(triple);
        }
        Ok(())
    })?;
    let (graph_refs, names_bytes) = graph_refs_controlled(
        snapshot,
        &graphs,
        &[],
        share.map(|limit| limit.saturating_sub(bytes)),
        pool.as_ref().map(|p| p.work_parent()),
        control,
    )?;
    hold_state(share, &mut bytes, &mut memory, names_bytes)?;
    let mut names: Vec<_> = graph_refs
        .into_iter()
        .map(|graph| match graph {
            sparkles_reasoner::GraphRef::Default => "default".to_string(),
            sparkles_reasoner::GraphRef::Named(name) => name,
        })
        .collect();
    if names.is_empty() {
        // A protocol dataset containing a graph absent from this immutable view is
        // empty; an empty list would instead restore the actual default graph.
        loop {
            let name = format!("urn:x-sparkles:empty-analysis:{}", uuid::Uuid::new_v4());
            if snapshot.lookup_iri(&name).is_none() {
                names.push(name);
                break;
            }
        }
    }
    let (mut diagnostics, diagnostics_memory) =
        sparkles_reasoner::diagnostics::diagnose_controlled_guarded(
            snapshot.clone(),
            &DiagnoseOptions {
                graphs: names,
                inferences: true,
                limit: MAX_LIMIT,
                timeout: control.remaining(),
                grant: pool.as_ref().map(|p| Arc::new(p.empty_grant())),
                ..Default::default()
            },
            Some(&typings),
            &DiagnosticControls {
                deadline: control.deadline,
                cancel: Some(control.cancel.clone()),
                max_memory: share.map(|bytes| bytes as u64 / 2),
                access: options.access.clone(),
                memory_parent: pool.as_ref().map(|p| p.work_parent()),
            },
        )
        .map_err(|error| diagnostic_error(control, error))?;
    if !summary_complete
        && let Some(check) = diagnostics
            .checks
            .iter_mut()
            .find(|check| check.id == sparkles_reasoner::diagnostics::LITERAL_NOT_IN_DATATYPE)
    {
        check.status = sparkles_reasoner::diagnostics::CheckStatus::Error;
        check.error = Some(
            "the selected materialization has no available generalized diagnostic summary".into(),
        );
        if diagnostics.findings.is_empty() {
            diagnostics.status = sparkles_reasoner::diagnostics::ReportStatus::Incomplete;
        }
    }
    for finding in &diagnostics.findings {
        hold(share, &mut bytes, diagnostic_bytes(finding))?;
    }
    drop(typings);
    Ok(State {
        asserted,
        closure,
        diagnostics,
        bytes: bytes - typings_bytes,
        warnings: Vec::new(),
        _memory: memory,
        _diagnostics_memory: diagnostics_memory,
    })
}

fn materialization_error(control: &AnalysisControl, error: anyhow::Error) -> Error {
    if let Err(stopped) = control.check() {
        return stopped;
    }
    if let Some(error) = error.downcast_ref::<sparkles_reasoner::MemoryLimitExceeded>() {
        budget(error.to_string())
    } else if let Some(error) = error.downcast_ref::<sparkles_reasoner::InferredLimitExceeded>() {
        budget(error.to_string())
    } else {
        control.error(crate::handles::from_anyhow(error))
    }
}

/// A diagnostic report that passes the limit of its analysis share, either its own
/// local limit or an enclosing memory share, has passed the analysis's memory limit.
/// Cancellation, the deadline and a refusal of the shared server memory or of graph
/// ownership keep their own errors.
pub(crate) fn diagnostic_error(control: &AnalysisControl, error: anyhow::Error) -> Error {
    if let Err(stopped) = control.check() {
        return stopped;
    }
    if let Some(error) = error.downcast_ref::<sparkles_reasoner::MemoryLimitExceeded>() {
        return budget(error.to_string());
    }
    control.error(share_error(crate::handles::from_anyhow(error)))
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
    analyze_with_cache_limit(
        dataset, snapshot, program, options, control, retained, subset, checks, None,
    )
}

#[allow(clippy::too_many_arguments)]
fn analyze_with_cache_limit(
    dataset: &Dataset,
    snapshot: &Arc<Snapshot>,
    program: &AnalysisProgram,
    options: &AnalysisOptions,
    control: &AnalysisControl,
    retained: usize,
    subset: Option<&std::collections::HashSet<[crate::id::Id; 4]>>,
    checks: &[String],
    cache_limit: Option<usize>,
) -> Result<State> {
    use sparkles_reasoner::{GraphRef, ImportMode};
    control.check()?;
    let snapshot = view(snapshot, options)?;
    let available = options
        .max_memory
        .map(|limit| {
            limit
                .checked_sub(retained)
                .and_then(|left| left.checked_sub(control.witness_memory.unwrap_or(0)))
                .filter(|left| *left > 0)
                .ok_or_else(|| budget("the retained analysis result exhausts its memory limit"))
        })
        .transpose()?;
    let pool = dataset.state().pool.clone();
    let parent = pool
        .as_ref()
        .map(crate::sparql::pool::QueryPool::work_parent);
    let mut memory = Memory::new(None, parent.clone());
    // The data copy, reasoner, diagnostic query and retained output each have a
    // separate share. Their overlapping lifetimes stay inside one request grant.
    let share = available.map(|bytes| bytes / 8);
    let (graphs, graphs_bytes) = input_graphs_controlled(
        &snapshot,
        program,
        options,
        control,
        share,
        pool.as_ref().map(|p| p.work_parent()),
    )?;
    let copied = dataset
        .store()
        .copy_view_to_memory_with_parent(
            &snapshot,
            crate::store::StoreOptions {
                max_memory_bytes: share.map(|bytes| bytes as u64),
                result_cache_bytes: 0,
                service_cache_bytes: 0,
                union_default_graph: false,
                ..Default::default()
            },
            &control.write_options(),
            parent.clone(),
            |quad| {
                Ok(graphs.contains(&quad[3]) && subset.is_none_or(|subset| subset.contains(quad)))
            },
        )
        .map_err(|error| control.error(error))?;
    let ordinal =
        copied.reserve_simulation_blank_range(control.witnesses.get().map(|map| map.ordinal()))?;
    let witnesses = control
        .witnesses
        .get_or_init(|| {
            sparkles_reasoner::analysis::WitnessMap::with_memory_parent(
                ordinal,
                control.witness_memory,
                pool.as_ref().map(|p| p.work_parent()),
                control.cancel.clone(),
                control.deadline,
            )
        })
        .clone();
    let context = sparkles_reasoner::analysis::AnalysisContext::new(
        witnesses,
        share,
        format!(
            "{}\n{:?}\n{:?}",
            program.profile.text(),
            program.extras,
            program.inputs
        ),
    )
    .map_err(|error| materialization_error(control, error))?;
    let copied = Dataset::from_store_with(
        copied,
        crate::DatasetOptions {
            closure_cache_triples: cache_limit.unwrap_or(0),
            ..Default::default()
        },
    );
    let before = copied.store().snapshot();
    let mut asserted = Facts::new();
    let mut bytes = 0;
    hold_state(share, &mut bytes, &mut memory, graphs_bytes)?;
    before.for_each_quad(|quad| {
        control.check()?;
        let mut scratch = Memory::new(share.map(|n| n as u64), parent.clone());
        let triple = decode_fact(&before, quad, control, &mut scratch)?;
        let key = fact_key(&triple);
        if let std::collections::btree_map::Entry::Vacant(entry) = asserted.entry(key) {
            hold_state(
                share,
                &mut bytes,
                &mut memory,
                fact_bytes(entry.key(), &triple),
            )?;
            entry.insert(triple);
        }
        Ok(())
    })?;
    drop(before);
    let (mut selected, selected_bytes) = graph_refs_controlled(
        &snapshot,
        &graphs,
        &[],
        share.map(|limit| limit.saturating_sub(bytes)),
        pool.as_ref().map(|p| p.work_parent()),
        control,
    )?;
    hold_state(share, &mut bytes, &mut memory, selected_bytes)?;
    if selected.is_empty() {
        selected.push(GraphRef::Default);
    }
    let mut input_names = Vec::new();
    for graph in &selected {
        control.check()?;
        let name = graph.as_str();
        hold_state(
            share,
            &mut bytes,
            &mut memory,
            std::mem::size_of::<String>() + name.len(),
        )?;
        input_names.push(name.to_string());
    }
    let request = sparkles_reasoner::ReasonOptions {
        analysis: Some(context.clone()),
        inputs: Inputs {
            data_graphs: selected,
            ontology_graphs: Vec::new(),
            imports: ImportMode::None,
            location_mapping: Default::default(),
        },
        max_memory: share,
        memory_parent: pool.as_ref().map(|p| p.work_parent()),
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
            cache: cache_limit
                .filter(|limit| *limit > 0)
                .map(|_| &copied.state().closure),
        },
        &request,
    )
    .map_err(|error| materialization_error(control, error))?;
    control.check()?;
    let snapshot = copied.store().snapshot();
    let mut closure = Facts::new();
    snapshot.for_each_quad(|quad| {
        control.check()?;
        let mut scratch = Memory::new(share.map(|n| n as u64), parent.clone());
        let triple = decode_fact(&snapshot, quad, control, &mut scratch)?;
        let key = fact_key(&triple);
        if let std::collections::btree_map::Entry::Vacant(entry) = closure.entry(key) {
            hold_state(
                share,
                &mut bytes,
                &mut memory,
                fact_bytes(entry.key(), &triple),
            )?;
            entry.insert(triple);
        }
        Ok(())
    })?;
    let typings = context
        .take_literal_typings_guarded(snapshot.commit)
        .ok_or_else(|| budget("the analysis did not retain its generalized diagnostics"))?;
    let typings_bytes = literal_typing_bytes(&typings);
    hold(share, &mut bytes, typings_bytes)?;
    let (diagnostics, diagnostics_memory) =
        sparkles_reasoner::diagnostics::diagnose_controlled_guarded(
            snapshot,
            &sparkles_reasoner::diagnostics::DiagnoseOptions {
                checks: checks.to_vec(),
                inferences: true,
                graphs: input_names,
                limit: sparkles_reasoner::diagnostics::MAX_LIMIT,
                timeout: control.remaining(),
                prefixes: dataset.store().prefixes().into_iter().collect(),
                grant: pool.as_ref().map(|p| Arc::new(p.empty_grant())),
                ..Default::default()
            },
            Some(&typings),
            &sparkles_reasoner::diagnostics::DiagnosticControls {
                deadline: control.deadline,
                cancel: Some(control.cancel.clone()),
                max_memory: available.map(|bytes| (bytes / 16) as u64),
                access: None,
                memory_parent: pool.as_ref().map(|p| p.work_parent()),
            },
        )
        .map_err(|error| diagnostic_error(control, error))?;
    control.check()?;
    for finding in &diagnostics.findings {
        hold(share, &mut bytes, diagnostic_bytes(finding))?;
    }
    drop(typings);
    Ok(State {
        asserted,
        closure,
        diagnostics,
        bytes: bytes - typings_bytes,
        warnings: outcome.warnings,
        _memory: memory,
        _diagnostics_memory: diagnostics_memory,
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

#[cfg(test)]
mod review_regressions {
    use super::*;
    #[test]
    fn declined_optional_cache_does_not_erase_generalized_diagnostics() {
        let pool = crate::sparql::pool::QueryPool::new(16 << 20);
        let dataset = Dataset::from_store_with(
            crate::store::Store::in_memory(Default::default()),
            crate::DatasetOptions {
                pool: Some(pool.clone()),
                ..Default::default()
            },
        );
        dataset
            .update("INSERT DATA {<urn:s> <urn:p> <urn:o> . <urn:literal> <urn:value> \"abc\"}")
            .unwrap();
        let program=AnalysisProgram {profile:Profile::Rules("[typing: (<urn:s> <urn:p> <urn:o>) -> (\"abc\" <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2001/XMLSchema#integer>)]".into()),extras:Default::default(),inputs:Default::default()};
        let options = AnalysisOptions::default();
        let control = AnalysisControl::new(&options).unwrap();
        let state = analyze_with_cache_limit(
            &dataset,
            &dataset.store().snapshot(),
            &program,
            &options,
            &control,
            0,
            None,
            &["literal-not-in-datatype".into()],
            Some(0),
        )
        .unwrap();
        assert!(
            state
                .diagnostics
                .findings
                .iter()
                .any(|finding| finding.check == "literal-not-in-datatype"),
            "optional caching lost an entailed literal typing conflict"
        );
        let diagnostics = state
            .diagnostics
            .findings
            .iter()
            .map(diagnostic_bytes)
            .sum::<usize>();
        assert_eq!(state.bytes, state._memory.bytes() as usize + diagnostics);
        // The grounded and generalized versions have identical retained RDF facts.
        // The generalized literal typing must not get a second state reservation.
        let plain = analyze(
            &dataset,
            &dataset.store().snapshot(),
            &AnalysisProgram {
                profile: Profile::Rules(
                    "[identity: (<urn:s> <urn:p> <urn:o>) -> (<urn:s> <urn:p> <urn:o>)]".into(),
                ),
                ..program
            },
            &options,
            &control,
            0,
            None,
            &[],
        )
        .unwrap();
        assert_eq!(state._memory.bytes(), plain._memory.bytes());
    }
    #[test]
    fn cancellation_during_diagnostic_capture_is_not_a_memory_refusal() {
        let caller = Arc::new(AtomicBool::new(false));
        let control = AnalysisControl::new(&AnalysisOptions {
            cancel: Some(caller.clone()),
            ..Default::default()
        })
        .unwrap();
        caller.store(true, Ordering::Relaxed);
        let capture = sparkles_reasoner::MemoryLimitExceeded {
            limit: 1024,
            context: " while stopping diagnostic capture".into(),
        };
        let error = materialization_error(&control, anyhow::Error::new(capture));
        assert!(matches!(error, Error::Cancelled), "{error}");
    }

    #[test]
    fn diagnostic_refusals_keep_cancellation_and_shared_memory_and_map_local_memory() {
        let refusal = |kind| {
            anyhow::Error::new(Error::BudgetExceeded(crate::Budget {
                kind,
                limit: 1024,
                requested: 2048,
            }))
        };
        let is_budget = |error: &Error| matches!(error, Error::Component(c) if c.code == super::super::explain::REASONING_BUDGET);
        let control = AnalysisControl::new(&AnalysisOptions::default()).unwrap();
        let local = sparkles_reasoner::MemoryLimitExceeded {
            limit: 1024,
            context: " while retaining the report scope".into(),
        };
        let error = diagnostic_error(&control, anyhow::Error::new(local.clone()));
        assert!(is_budget(&error), "{error:?}");
        let error = diagnostic_error(&control, refusal(crate::BudgetKind::Memory));
        assert!(is_budget(&error), "{error:?}");
        let error = diagnostic_error(&control, refusal(crate::BudgetKind::ServerMemory));
        assert!(
            matches!(&error, Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::ServerMemory),
            "{error:?}"
        );
        let caller = Arc::new(AtomicBool::new(false));
        let control = AnalysisControl::new(&AnalysisOptions {
            cancel: Some(caller.clone()),
            ..Default::default()
        })
        .unwrap();
        caller.store(true, Ordering::Relaxed);
        let error = diagnostic_error(&control, anyhow::Error::new(local));
        assert!(matches!(error, Error::Cancelled), "{error:?}");
    }
}
