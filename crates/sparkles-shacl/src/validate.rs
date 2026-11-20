//! The validation engine (SHACL §3–4).

use crate::data::DataGraph;
use crate::nodeeval::Scope;
use crate::path::{CPath, PropertyPath};
use crate::report::{ValidationReport, ValidationResult};
use crate::shapes::{Candidates, Constraint, Qualified, ShapeId, Shapes, Target};
use anyhow::{Result, bail};
use oxrdf::{Literal, NamedNode, Term};
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use sparkles_core::id::Id;
use sparkles_core::sparql::value;
use sparkles_core::store::Snapshot;
use sparkles_core::xsd;
use std::cmp::Ordering;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

/// Validation options.
#[derive(Clone, Debug)]
pub struct ValidateOptions {
    /// The data graph: `None` = the store's default graph (the union of all graphs if
    /// the store uses a union default graph); a named graph IRI; `urn:x-arq:DefaultGraph`
    /// for the default graph or `urn:x-arq:UnionGraph` for the union of all graphs.
    pub data_graph: Option<String>,
    /// Further graphs merged into the data graph (e.g. the reasoner's
    /// `urn:x-sparkles:inferred`). Graphs that do not exist are ignored.
    pub extra_graphs: Vec<String>,
    /// Graphs never part of the data graph, even when it is the union of all graphs
    /// (e.g. the shapes graphs of write-time validation).
    pub exclude_graphs: Vec<String>,
    /// Validate focus nodes in parallel (rayon).
    pub parallel: bool,
    /// The thread pool parallel validation runs in (the global rayon pool if `None`),
    /// to bound the threads one caller's validations may use.
    pub pool: Option<Arc<rayon::ThreadPool>>,
    pub timeout: Option<Duration>,
    /// Absolute bound shared with an enclosing operation, without phase renewal.
    pub deadline: Option<Instant>,
    /// Additional cancellation sources, checked with the existing single flag.
    pub cancellation: Option<sparkles_core::task::Cancellation>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// Stop with [`TooManyResults`] once the report would hold more results than this.
    pub max_results: Option<usize>,
    /// The most units of work one node expression evaluation may take. Each evaluation of a
    /// sub-expression is one unit, and so is each node that a concatenation or a mapping
    /// appends. The nodes that a read of the data returns are not counted, so a large
    /// result is fine. A shared sub-expression counts each time it is evaluated, so a
    /// doubling expression graph stops here and not after exhausting memory. The name
    /// predates the accounting and the field counts units of work, not nodes. At the
    /// default the ids a concatenation appends take 128 MiB, which is a floor, because
    /// the output and the appended vector are both live and each worker thread of a
    /// parallel validation can hold one.
    pub max_expr_nodes: u64,
    /// The memory budget of each SPARQL query that a constraint, a node expression or a
    /// rule runs, in bytes. `None` keeps the query engine's default.
    pub query_memory: Option<u64>,
}

/// The default of [`ValidateOptions::max_expr_nodes`].
pub const DEFAULT_MAX_EXPR_NODES: u64 = 1 << 24;

/// A report that would hold more than [`ValidateOptions::max_results`] results.
#[derive(Debug)]
pub struct TooManyResults {
    pub limit: usize,
}

impl std::fmt::Display for TooManyResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the validation report exceeds {} results", self.limit)
    }
}

impl std::error::Error for TooManyResults {}

impl Default for ValidateOptions {
    fn default() -> Self {
        ValidateOptions {
            data_graph: None,
            extra_graphs: Vec::new(),
            exclude_graphs: Vec::new(),
            parallel: true,
            pool: None,
            timeout: None,
            deadline: None,
            cancellation: None,
            cancel: None,
            max_results: None,
            max_expr_nodes: DEFAULT_MAX_EXPR_NODES,
            query_memory: None,
        }
    }
}

pub(crate) struct ValidationControls {
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub cancellation: Option<sparkles_core::task::Cancellation>,
}

impl ValidationControls {
    fn new(opts: &ValidateOptions) -> Self {
        let relative = opts
            .timeout
            .and_then(|timeout| Instant::now().checked_add(timeout));
        Self {
            deadline: opts.deadline.into_iter().chain(relative).min(),
            cancel: opts.cancel.clone(),
            cancellation: opts.cancellation.clone(),
        }
    }

    fn active(&self) -> bool {
        self.deadline.is_some() || self.cancel.is_some() || self.cancellation.is_some()
    }

    fn check(&self) -> sparkles_core::Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(AtomicOrdering::Relaxed))
        {
            return Err(sparkles_core::Error::Cancelled);
        }
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        crate::data::check_deadline(self.deadline)
    }
}

/// Validate the data graph of a snapshot against a shapes graph.
pub fn validate(
    snap: &Arc<Snapshot>,
    shapes: &Shapes,
    opts: &ValidateOptions,
) -> Result<ValidationReport> {
    validate_impl(snap, shapes, opts, None, None)
}

/// Validate with bounds on embedded SPARQL and estimated retained report memory.
/// From `query`, use the intermediate rows, bytes, produced rows and shared work
/// counter. The validation deadline and data selection are supplied by `opts`.
pub fn validate_controlled(
    snap: &Arc<Snapshot>,
    shapes: &Shapes,
    opts: &ValidateOptions,
    query: &sparkles_core::sparql::QueryOptions,
    max_report_bytes: Option<u64>,
) -> Result<ValidationReport> {
    validate_impl(snap, shapes, opts, Some(query), max_report_bytes)
}

fn validate_impl(
    snap: &Arc<Snapshot>,
    shapes: &Shapes,
    opts: &ValidateOptions,
    query: Option<&sparkles_core::sparql::QueryOptions>,
    max_report_bytes: Option<u64>,
) -> Result<ValidationReport> {
    let controls = ValidationControls::new(opts);
    let (data, ids) = DataGraph::new_checked(
        snap.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
        shapes,
        || controls.check(),
    )?;
    let mut engine = Engine::new_checked(shapes, &data, ids, opts, controls)?;
    engine.query_limits = query.cloned();
    engine.report_bytes = max_report_bytes.map(sparkles_core::validation::ReportBytes::new);
    engine.check_limits()?;
    engine.run(None)
}

/// Validate a set of nodes, each against the shapes whose targets select it, in one
/// report: [`validate_node`] for many nodes over one data graph. Duplicate nodes are
/// validated once.
pub fn validate_nodes(
    snap: &Arc<Snapshot>,
    shapes: &Shapes,
    nodes: &[Term],
    opts: &ValidateOptions,
) -> Result<ValidationReport> {
    let (mut data, ids) = DataGraph::new(
        snap.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
        shapes,
    )?;
    let focus: Vec<Id> = nodes
        .iter()
        .map(|n| resolve_focus(&mut data, n, shapes))
        .collect();
    let engine = Engine::new(shapes, &data, ids, opts)?;
    engine.run_nodes(&focus)
}

/// Validate a single node against the shapes whose targets select it (Jena
/// `ShaclValidator.validate(shapes, graph, node)`).
pub fn validate_node(
    snap: &Arc<Snapshot>,
    shapes: &Shapes,
    node: &Term,
    opts: &ValidateOptions,
) -> Result<ValidationReport> {
    let (mut data, ids) = DataGraph::new(
        snap.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
        shapes,
    )?;
    let focus = resolve_focus(&mut data, node, shapes);
    let engine = Engine::new(shapes, &data, ids, opts)?;
    engine.run_nodes(&[focus])
}

/// The shapes whose targets select `node` in the data graph of `opts`, in shape order:
/// the shapes [`validate_node`] would validate the node against. Deactivated shapes are
/// left out.
pub fn shapes_for_node(
    snap: &Arc<Snapshot>,
    shapes: &Shapes,
    node: &Term,
    opts: &ValidateOptions,
) -> Result<Vec<ShapeId>> {
    let (mut data, ids) = DataGraph::new(
        snap.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
        shapes,
    )?;
    let focus = resolve_focus(&mut data, node, shapes);
    let engine = Engine::new(shapes, &data, ids, opts)?;
    let mut out = Vec::new();
    for (si, shape) in shapes.shapes.iter().enumerate() {
        if shape.targets.is_empty() || shape.deactivated {
            continue;
        }
        engine.check_limits()?;
        if engine.is_target(si, focus)? {
            out.push(si);
        }
    }
    Ok(out)
}

/// The node shapes that would target a new instance of `class`: those with a class
/// target (explicit or implicit) of `class` or of a class `class` is a subclass of in the
/// data graph. Targets by node, by subject or object of a predicate, by expression and
/// `sh:targetWhere` depend on data a new resource does not have yet, so they do not count.
pub fn shapes_for_class(
    snap: &Arc<Snapshot>,
    shapes: &Shapes,
    class: &Term,
    opts: &ValidateOptions,
) -> Result<Vec<ShapeId>> {
    let (mut data, ids) = DataGraph::new(
        snap.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
        shapes,
    )?;
    let cls = data.resolve(class, shapes.bnodes_in_store);
    let engine = Engine::new(shapes, &data, ids, opts)?;
    let mut out = Vec::new();
    for (si, shape) in shapes.shapes.iter().enumerate() {
        if shape.deactivated {
            continue;
        }
        engine.check_limits()?;
        for t in &shape.targets {
            if let Target::Class(c) = t {
                let c = engine.id(*c);
                if c == cls || data.subclasses(c)?.contains(&cls) {
                    out.push(si);
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// The id of a focus node named by a caller: a blank node with the label of a stored one
/// (`b…`) is that stored node, whatever the scope of the shapes' own blank nodes, and any
/// other term resolves as the shapes' terms do.
pub(crate) fn resolve_focus(data: &mut DataGraph, node: &Term, shapes: &Shapes) -> Id {
    if let Term::BlankNode(b) = node
        && let Some(id) = sparkles_core::store::parse_bnode_label(b.as_str())
    {
        return id;
    }
    data.resolve(node, shapes.bnodes_in_store)
}

/// The focus nodes a shape is validated on (see [`validate_selected`]).
#[derive(Clone, Debug)]
pub(crate) enum Sel {
    /// none: the shape is not validated
    Skip,
    /// every focus node of its targets
    All,
    /// those of these nodes that its targets select
    Nodes(Vec<Id>),
    /// Already admitted actual targets; do not enumerate or retest targets.
    Admitted(Vec<Id>),
}

/// The outcome of one shape of [`validate_selected`].
#[derive(Debug, Default)]
pub(crate) struct ShapeRun {
    pub results: Vec<ValidationResult>,
    /// the focus nodes validated
    pub focus: usize,
}

/// Validate each shape (by [`ShapeId`]) on the focus nodes `sel` picks for it, over
/// `data` (with `ids`, the ids of the shapes' terms in it). Ids in `sel` may be ids of
/// `ids_of`, a later state of the same store than `data` (the post-state of a write
/// whose pre-state `data` reads): an id of a term `data` does not have is resolved by
/// its term, so a `sh:targetNode` the write added still matches.
pub(crate) fn validate_selected(
    data: &mut DataGraph,
    ids: &[Id],
    shapes: &Shapes,
    opts: &ValidateOptions,
    sel: &[Sel],
    ids_of: Option<&Snapshot>,
) -> Result<Vec<ShapeRun>> {
    let mut sel = sel.to_vec();
    if let Some(after) = ids_of {
        let known = data.snap.dvocab_len;
        for s in &mut sel {
            if let Sel::Nodes(nodes) | Sel::Admitted(nodes) = s {
                for n in nodes.iter_mut() {
                    // a delta id past the snapshot's vocabulary: a term it does not have
                    let unknown = n.tag() == sparkles_core::id::Tag::Delta && n.payload() >= known;
                    if unknown && let Some(t) = after.term(*n) {
                        *n = data.resolve(&t, shapes.bnodes_in_store);
                    }
                }
            }
        }
    }
    let engine = Engine::new(shapes, data, ids.to_vec(), opts)?;
    engine.run_shapes(&sel)
}

/// Result path of a raw result.
pub(crate) enum RPath {
    None,
    /// the path of the source shape
    Shape,
    Pred(Id),
    Path(PropertyPath),
}

/// Result sink: collects results, or (when probing conformance) stops at the first.
pub(crate) struct Out {
    collect: bool,
    pub results: Vec<ValidationResult>,
    pub failed: bool,
}

impl Out {
    fn collect() -> Out {
        Out {
            collect: true,
            results: Vec::new(),
            failed: false,
        }
    }
    fn probe() -> Out {
        Out {
            collect: false,
            results: Vec::new(),
            failed: false,
        }
    }
    #[inline]
    pub fn stop(&self) -> bool {
        !self.collect && self.failed
    }
}

/// Per-thread evaluation state: the (shape, focus) stack for recursive shapes.
#[derive(Default)]
pub(crate) struct Cx {
    stack: Vec<(ShapeId, Id)>,
    /// nesting of node expression evaluations
    pub depth: usize,
    /// units of work of the top-level evaluation in progress, counted against the budget
    pub expr_work: u64,
    /// nodes that its evaluations produced, which set how often the deadline is checked
    pub expr_seen: u64,
}

pub(crate) struct Engine<'a> {
    pub shapes: &'a Shapes,
    pub data: &'a DataGraph,
    /// store id of each term of the shapes' term table
    pub ids: Vec<Id>,
    paths: Vec<Option<CPath>>,
    /// the compiled path of each `shnex:pathValues` expression
    pub expr_paths: Vec<Option<CPath>>,
    parallel: bool,
    pool: Option<Arc<rayon::ThreadPool>>,
    pub controls: ValidationControls,
    max_results: Option<usize>,
    /// the most nodes one node expression evaluation may work through
    pub max_expr_nodes: u64,
    /// the memory budget of each query ([`ValidateOptions::query_memory`])
    pub query_memory: Option<u64>,
    pub query_limits: Option<sparkles_core::sparql::QueryOptions>,
    report_bytes: Option<sparkles_core::validation::ReportBytes>,
    /// results collected so far (checked against `max_results`)
    collected: AtomicUsize,
    /// the queries node expressions have run, which keep the new blank nodes of each
    /// apart ([`Engine::result_id`])
    pub(crate) query_calls: std::sync::atomic::AtomicU64,
}

/// Members past which a list counts as ill-formed (a hostile list cannot exhaust memory).
const MAX_LIST_MEMBERS: usize = 1 << 24;

/// A one-element result message.
fn msg(s: String) -> Vec<Literal> {
    vec![Literal::new_simple_literal(s)]
}

impl<'a> Engine<'a> {
    pub(crate) fn new_for_expressions(
        shapes: &'a Shapes,
        data: &'a DataGraph,
        ids: Vec<Id>,
        opts: &ValidateOptions,
    ) -> Result<Engine<'a>> {
        Engine::new(shapes, data, ids, opts)
    }

    pub(crate) fn new(
        shapes: &'a Shapes,
        data: &'a DataGraph,
        ids: Vec<Id>,
        opts: &ValidateOptions,
    ) -> Result<Engine<'a>> {
        Self::new_checked(shapes, data, ids, opts, ValidationControls::new(opts))
    }

    fn new_checked(
        shapes: &'a Shapes,
        data: &'a DataGraph,
        ids: Vec<Id>,
        opts: &ValidateOptions,
        controls: ValidationControls,
    ) -> Result<Engine<'a>> {
        let mut lookup = |n: &NamedNode| {
            data.snap
                .lookup_iri(n.as_str())
                .unwrap_or(Id::local(u64::MAX >> 8))
        };
        let check = || controls.check();
        check()?;
        let mut compile = |path: &PropertyPath| {
            if controls.active() {
                CPath::compile_checked(path, &mut lookup, &check)
            } else {
                Ok(CPath::compile(path, &mut lookup))
            }
        };
        let expr_paths = shapes
            .exprs
            .iter()
            .enumerate()
            .map(|(i, e)| {
                if i % 256 == 0 {
                    check()?;
                }
                Ok(match e {
                    crate::nodeexpr::Expr::PathValues { path, .. } => Some(compile(path)?),
                    _ => None,
                })
            })
            .collect::<sparkles_core::Result<Vec<_>>>()?;
        let paths = shapes
            .shapes
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if i % 256 == 0 {
                    check()?;
                }
                s.path.as_ref().map(&mut compile).transpose()
            })
            .collect::<sparkles_core::Result<Vec<_>>>()?;
        check()?;
        Ok(Engine {
            shapes,
            data,
            ids,
            paths,
            expr_paths,
            parallel: opts.parallel,
            pool: opts.pool.clone(),
            controls,
            max_results: opts.max_results,
            max_expr_nodes: opts.max_expr_nodes,
            query_memory: opts.query_memory,
            query_limits: None,
            report_bytes: None,
            collected: AtomicUsize::new(0),
            query_calls: Default::default(),
        })
    }

    pub(crate) fn check_limits(&self) -> Result<()> {
        self.check_data_scan().map_err(Into::into)
    }

    pub(crate) fn check_data_scan(&self) -> sparkles_core::Result<()> {
        self.controls.check()
    }

    /// Admit a report entry or a nested detail before retaining it.
    fn admit_result(&self, result: &ValidationResult) -> Result<()> {
        if let Some(bytes) = &self.report_bytes {
            bytes.admit(result.retained_bytes())?;
        }
        let Some(limit) = self.max_results else {
            return Ok(());
        };
        self.collected
            .try_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |n| {
                (n < limit).then(|| n + 1)
            })
            .map_err(|_| TooManyResults { limit })?;
        Ok(())
    }

    #[inline]
    pub fn id(&self, t: crate::shapes::Tid) -> Id {
        self.ids[t as usize]
    }

    pub fn term(&self, id: Id) -> Result<Term> {
        match self.data.term(id) {
            Some(t) => Ok(t),
            None => bail!("cannot decode id {id:?}"),
        }
    }

    fn run(&self, only: Option<Id>) -> Result<ValidationReport> {
        match only {
            Some(f) => self.run_nodes(&[f]),
            None => self.run_sel(&vec![Sel::All; self.shapes.shapes.len()]),
        }
    }

    /// Validate `nodes` against the shapes whose targets select them.
    fn run_nodes(&self, nodes: &[Id]) -> Result<ValidationReport> {
        self.run_sel(&vec![Sel::Nodes(nodes.to_vec()); self.shapes.shapes.len()])
    }

    fn run_sel(&self, sel: &[Sel]) -> Result<ValidationReport> {
        let results: Vec<ValidationResult> = self
            .run_shapes(sel)?
            .into_iter()
            .flat_map(|r| r.results)
            .collect();
        Ok(ValidationReport {
            conforms: results.is_empty(),
            results,
        })
    }

    /// Validate each shape with targets on the focus nodes `sel` picks for it, in shape
    /// order: the results and the number of focus nodes of every shape.
    fn run_shapes(&self, sel: &[Sel]) -> Result<Vec<ShapeRun>> {
        let mut runs = Vec::with_capacity(self.shapes.shapes.len());
        for (si, shape) in self.shapes.shapes.iter().enumerate() {
            let mut run = ShapeRun::default();
            if shape.targets.is_empty() || shape.deactivated {
                runs.push(run);
                continue;
            }
            self.check_limits()?;
            let focus = match &sel[si] {
                Sel::Skip => {
                    runs.push(run);
                    continue;
                }
                Sel::All => self.focus_nodes(si)?,
                Sel::Admitted(nodes) => nodes.clone(),
                Sel::Nodes(nodes) => {
                    let mut seen = FxHashSet::default();
                    let mut out = Vec::new();
                    for &f in nodes {
                        if seen.insert(f) && self.is_target(si, f)? {
                            out.push(f);
                        }
                    }
                    out
                }
            };
            run.focus = focus.len();
            let chunk = |nodes: &[Id]| -> Result<Vec<ValidationResult>> {
                let mut out = Out::collect();
                let mut cx = Cx::default();
                for (i, &f) in nodes.iter().enumerate() {
                    if i % 256 == 0 {
                        self.check_limits()?;
                    }
                    self.validate_focus(si, f, &mut out, &mut cx)?;
                }
                Ok(out.results)
            };
            if self.parallel && focus.len() >= 512 {
                let run_par = || -> Vec<Result<Vec<ValidationResult>>> {
                    focus.par_chunks(256).map(chunk).collect()
                };
                let parts = match &self.pool {
                    Some(pool) => pool.install(run_par),
                    None => run_par(),
                };
                for p in parts {
                    run.results.extend(p?);
                }
            } else {
                run.results = chunk(&focus)?;
            }
            runs.push(run);
        }
        Ok(runs)
    }

    /// Focus nodes of a shape's targets (distinct, in target order).
    pub(crate) fn focus_nodes(&self, si: ShapeId) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        let check = || self.check_data_scan();
        for t in &self.shapes.shapes[si].targets {
            self.check_limits()?;
            let nodes = match *t {
                Target::Node(t) => vec![self.id(t)],
                Target::Expr(e) => {
                    // the shape is the focus node of its target expression
                    let shape_node = self.shapes.shapes[si].node.clone();
                    let focus = self.lit_id(&shape_node);
                    let mut cx = Cx::default();
                    self.eval(e, focus, &Scope::default(), &mut cx)?
                }
                Target::Class(c) => self.data.instances_checked(self.id(c), check)?,
                Target::SubjectsOf(p) => self.data.subjects_of_checked(self.id(p), check)?,
                Target::ObjectsOf(p) => self.data.objects_of_checked(self.id(p), check)?,
                Target::Where(w) => {
                    let mut cx = Cx::default();
                    let mut out = Vec::new();
                    for (i, n) in self.candidates(w)?.into_iter().enumerate() {
                        if i % 256 == 0 {
                            self.check_limits()?;
                        }
                        if self.conforms(w, n, &mut cx)? {
                            out.push(n);
                        }
                    }
                    out
                }
            };
            for n in nodes {
                self.check_limits()?;
                if seen.insert(n) {
                    out.push(n);
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn is_target(&self, si: ShapeId, f: Id) -> Result<bool> {
        let check = || self.check_data_scan();
        for t in &self.shapes.shapes[si].targets {
            self.check_limits()?;
            let hit = match *t {
                Target::Node(t) => self.id(t) == f,
                Target::Expr(e) => {
                    let shape_node = self.shapes.shapes[si].node.clone();
                    let focus = self.lit_id(&shape_node);
                    self.eval(e, focus, &Scope::default(), &mut Cx::default())?
                        .contains(&f)
                }
                Target::Class(c) => self.data.is_instance_checked(f, self.id(c), check)?,
                Target::SubjectsOf(p) => {
                    !self.data.objects_checked(f, self.id(p), check)?.is_empty()
                }
                Target::ObjectsOf(p) => {
                    !self.data.subjects_checked(self.id(p), f, check)?.is_empty()
                }
                // a node that conforms is one of the candidates; those of a narrowed
                // set are nodes of the data graph
                Target::Where(w) => {
                    let narrowed = !matches!(
                        self.shapes.candidates(w),
                        Candidates::All | Candidates::Terms(_)
                    );
                    (narrowed || self.is_node(f)?) && self.conforms(w, f, &mut Cx::default())?
                }
            };
            if hit {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The nodes that may conform to shape `si` (see [`Shapes::candidates`]).
    pub(crate) fn candidates(&self, si: ShapeId) -> Result<Vec<Id>> {
        let check = || self.check_data_scan();
        let pred = |p: &NamedNode| {
            self.data
                .snap
                .lookup_iri(p.as_str())
                .unwrap_or(Id::local(u64::MAX >> 8))
        };
        Ok(match self.shapes.candidates(si) {
            Candidates::All => self.data.nodes_checked(check)?,
            Candidates::Instances(c) => self.data.instances_checked(self.id(c), check)?,
            Candidates::Terms(ts) => {
                let mut out = Vec::new();
                for t in ts {
                    self.check_limits()?;
                    let id = self.id(t);
                    if self.is_node(id)? && !out.contains(&id) {
                        out.push(id);
                    }
                }
                out
            }
            Candidates::SubjectsOf(p) => self.data.subjects_of_checked(pred(&p), check)?,
            Candidates::ObjectsOf(p) => self.data.objects_of_checked(pred(&p), check)?,
        })
    }

    /// Whether `n` is a node of the data graph (the subject or object of a triple).
    fn is_node(&self, n: Id) -> Result<bool> {
        Ok(self.data.has_node_checked(n, || self.check_data_scan())?)
    }

    /// Value nodes of a shape for a focus node.
    pub fn value_nodes(&self, si: ShapeId, focus: Id) -> Result<Vec<Id>> {
        match &self.paths[si] {
            Some(path) => self.eval_path(path, focus),
            None => Ok(vec![focus]),
        }
    }

    pub(crate) fn eval_path(&self, path: &CPath, node: Id) -> Result<Vec<Id>> {
        if !self.controls.active() {
            return path.eval(self.data, node);
        }
        path.eval_checked(self.data, node, || self.check_data_scan())
    }

    pub(crate) fn validate_focus(
        &self,
        si: ShapeId,
        focus: Id,
        out: &mut Out,
        cx: &mut Cx,
    ) -> Result<()> {
        let shape = &self.shapes.shapes[si];
        if self.deactivated_at(si, focus, cx)? {
            return Ok(());
        }
        let mut values = self.value_nodes(si, focus)?;
        if shape.path.is_some() && (shape.values.is_some() || shape.default_value.is_some()) {
            values = self.extend_values(si, focus, values, cx)?;
        }
        for c in &shape.constraints {
            self.check(si, c, focus, &values, out, cx)?;
            if out.stop() {
                break;
            }
        }
        Ok(())
    }

    fn objects(&self, subject: Id, predicate: Id) -> sparkles_core::Result<Vec<Id>> {
        if self.controls.active() {
            self.data
                .objects_checked(subject, predicate, || self.check_data_scan())
        } else {
            self.data.objects(subject, predicate)
        }
    }

    /// Does `v` conform to shape `si`?
    pub(crate) fn conforms(&self, si: ShapeId, v: Id, cx: &mut Cx) -> Result<bool> {
        self.check_limits()?;
        if cx.stack.contains(&(si, v)) || self.deactivated_at(si, v, cx)? {
            return Ok(true);
        }
        if cx.stack.len() > 512 {
            bail!(
                "shape recursion too deep at {}",
                self.shapes.shapes[si].node
            );
        }
        cx.stack.push((si, v));
        let mut out = Out::probe();
        #[cfg(test)]
        control_tests::observe_conformance_probe();
        let r = self.validate_focus(si, v, &mut out, cx);
        cx.stack.pop();
        r?;
        self.check_limits()?;
        Ok(!out.failed)
    }

    /// Record a result (ids are decoded only when results are collected).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn report(
        &self,
        out: &mut Out,
        si: ShapeId,
        focus: Id,
        path: RPath,
        value: Option<Id>,
        component: NamedNode,
        source_constraint: Option<Term>,
        messages: impl FnOnce() -> Vec<Literal>,
    ) -> Result<()> {
        out.failed = true;
        if !out.collect {
            return Ok(());
        }
        let focus = self.term(focus)?;
        let value = value.map(|v| self.term(v)).transpose()?;
        let shape = &self.shapes.shapes[si];
        let messages = if shape.messages.is_empty() {
            messages()
        } else {
            shape.messages.clone()
        };
        self.push(
            out,
            si,
            focus,
            path,
            value,
            component,
            source_constraint,
            messages,
            String::new,
        )
    }

    /// Record a result given as terms. `messages` take precedence over the shape's
    /// `sh:message`; if both are empty, `default` is used.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push(
        &self,
        out: &mut Out,
        si: ShapeId,
        focus: Term,
        path: RPath,
        value: Option<Term>,
        component: NamedNode,
        source_constraint: Option<Term>,
        messages: Vec<Literal>,
        default: impl FnOnce() -> String,
    ) -> Result<()> {
        out.failed = true;
        if !out.collect {
            return Ok(());
        }
        let shape = &self.shapes.shapes[si];
        let result_path = match path {
            RPath::None => None,
            RPath::Shape => shape.path.clone(),
            RPath::Pred(p) => match self.term(p)? {
                Term::NamedNode(n) => Some(PropertyPath::Predicate(n)),
                _ => None,
            },
            RPath::Path(p) => Some(p),
        };
        let messages = if !messages.is_empty() {
            messages
        } else if !shape.messages.is_empty() {
            shape.messages.clone()
        } else {
            let d = default();
            if d.is_empty() { Vec::new() } else { msg(d) }
        };
        let result = ValidationResult {
            focus_node: focus,
            result_path,
            value,
            source_shape: shape.node.clone(),
            source_constraint_component: component,
            source_constraint,
            severity: shape.severity.clone(),
            messages,
            details: Vec::new(),
        };
        self.admit_result(&result)?;
        out.results.push(result);
        Ok(())
    }

    /// Attach details to the result just recorded (when results are collected).
    fn attach_details(out: &mut Out, details: Vec<ValidationResult>) {
        if out.collect
            && let Some(r) = out.results.last_mut()
        {
            r.details = details;
        }
    }

    /// The members of `head` if it is a SHACL list (SHACL 1.2 Core, "SHACL Lists"):
    /// `rdf:nil` without `rdf:first` or `rdf:rest`, or an IRI or blank node with exactly
    /// one `rdf:first` and one `rdf:rest` whose value is a SHACL list, without a cycle.
    /// `None` when it is not one, or longer than [`MAX_LIST_MEMBERS`].
    pub(crate) fn shacl_list(&self, head: Id) -> Result<Option<Vec<Id>>> {
        if matches!(self.term(head)?, Term::Literal(_) | Term::Triple(_)) {
            return Ok(None);
        }
        let (first, rest, nil) = (self.data.rdf_first, self.data.rdf_rest, self.data.rdf_nil);
        let mut members = Vec::new();
        let mut seen = FxHashSet::default();
        let mut n = head;
        loop {
            if !seen.insert(n) || members.len() > MAX_LIST_MEMBERS {
                return Ok(None);
            }
            if members.len() % 4096 == 4095 {
                self.check_limits()?;
            }
            let firsts = self.objects(n, first)?;
            let rests = self.objects(n, rest)?;
            if n == nil {
                return Ok((firsts.is_empty() && rests.is_empty()).then_some(members));
            }
            let ([f], [r]) = (&firsts[..], &rests[..]) else {
                return Ok(None);
            };
            members.push(*f);
            n = *r;
        }
    }

    /// Report a value node that is not a SHACL list, for a list constraint.
    fn not_a_list(
        &self,
        out: &mut Out,
        si: ShapeId,
        focus: Id,
        path: RPath,
        v: Id,
        component: NamedNode,
    ) -> Result<()> {
        let name = component
            .as_str()
            .strip_prefix(crate::vocab::SH_NS)
            .unwrap_or(component.as_str())
            .trim_end_matches("ConstraintComponent")
            .to_string();
        self.report(out, si, focus, path, Some(v), component, None, || {
            msg(format!("{name}: {} is not a SHACL list", self.show(v)))
        })
    }

    pub(crate) fn show(&self, id: Id) -> String {
        self.data
            .term(id)
            .map(|t| t.to_string())
            .unwrap_or_else(|| format!("{id:?}"))
    }

    fn lexical(&self, id: Id) -> Result<Option<String>> {
        Ok(match self.term(id)? {
            Term::NamedNode(n) => Some(n.into_string()),
            Term::Literal(l) => Some(l.value().to_string()),
            Term::BlankNode(_) | Term::Triple(_) => None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn check(
        &self,
        si: ShapeId,
        c: &Constraint,
        focus: Id,
        values: &[Id],
        out: &mut Out,
        cx: &mut Cx,
    ) -> Result<()> {
        let shape = &self.shapes.shapes[si];
        let path = || {
            if shape.path.is_some() {
                RPath::Shape
            } else {
                RPath::None
            }
        };
        let comp = || c.component();
        // per value node checks
        macro_rules! each_value {
            ($v:ident, $ok:expr, $msg:expr) => {
                for &$v in values {
                    if !$ok {
                        self.report(out, si, focus, path(), Some($v), comp(), None, || msg($msg))?;
                        if out.stop() {
                            return Ok(());
                        }
                    }
                }
            };
        }
        match c {
            Constraint::Class(cls) => {
                let cid = self.id(*cls);
                each_value!(
                    v,
                    if self.controls.active() {
                        self.data
                            .is_instance_checked(v, cid, || self.check_data_scan())?
                    } else {
                        self.data.is_instance(v, cid)?
                    },
                    format!(
                        "ClassConstraint[{}]: Expected class {} for {}",
                        self.shapes.term(*cls),
                        self.shapes.term(*cls),
                        self.show(v)
                    )
                );
            }
            Constraint::Datatype(dt) => {
                each_value!(
                    v,
                    match self.term(v)? {
                        Term::Literal(l) => l.datatype() == dt.as_ref() && xsd::is_valid(&l),
                        _ => false,
                    },
                    format!(
                        "DatatypeConstraint[{dt}]: Expected {dt} : Actual {}",
                        self.show(v)
                    )
                );
            }
            Constraint::NodeKind(k) => {
                each_value!(
                    v,
                    {
                        let t = self.term(v)?;
                        k.matches(
                            matches!(t, Term::NamedNode(_)),
                            matches!(t, Term::BlankNode(_)),
                            matches!(t, Term::Literal(_)),
                        )
                    },
                    format!(
                        "NodeKind[{}] Expected {} for {}",
                        k.iri(),
                        k.iri(),
                        self.show(v)
                    )
                );
            }
            Constraint::MinCount(n) => {
                if (values.len() as u64) < *n {
                    self.report(out, si, focus, path(), None, comp(), None, || {
                        msg(format!(
                            "minCount[{n}]: Invalid cardinality: expected min {n}: Got count = {}",
                            values.len()
                        ))
                    })?;
                }
            }
            Constraint::MaxCount(n) => {
                if (values.len() as u64) > *n {
                    self.report(out, si, focus, path(), None, comp(), None, || {
                        msg(format!(
                            "maxCount[{n}]: Invalid cardinality: expected max {n}: Got count = {}",
                            values.len()
                        ))
                    })?;
                }
            }
            Constraint::MinExclusive(t, p)
            | Constraint::MinInclusive(t, p)
            | Constraint::MaxExclusive(t, p)
            | Constraint::MaxInclusive(t, p) => {
                let (name, accept): (&str, fn(Ordering) -> bool) = match c {
                    Constraint::MinExclusive(..) => ("MinExclusive", |o| o == Ordering::Greater),
                    Constraint::MinInclusive(..) => ("MinInclusive", |o| o != Ordering::Less),
                    Constraint::MaxExclusive(..) => ("MaxExclusive", |o| o == Ordering::Less),
                    _ => ("MaxInclusive", |o| o != Ordering::Greater),
                };
                each_value!(
                    v,
                    match self.data.value(v) {
                        Some(val) => matches!(value::compare(&val, p), Ok(Some(o)) if accept(o)),
                        None => false,
                    },
                    format!("{name}[{t}]: value {} not in range", self.show(v))
                );
            }
            Constraint::MinLength(n) | Constraint::MaxLength(n) => {
                let min = matches!(c, Constraint::MinLength(_));
                each_value!(
                    v,
                    match self.lexical(v)? {
                        Some(s) => {
                            let len = s.chars().count() as u64;
                            if min { len >= *n } else { len <= *n }
                        }
                        None => false,
                    },
                    format!(
                        "{}[{n}]: String too {}: {}",
                        if min {
                            "MinLengthConstraint"
                        } else {
                            "MaxLengthConstraint"
                        },
                        if min { "short" } else { "long" },
                        self.show(v)
                    )
                );
            }
            Constraint::Pattern(p) => {
                each_value!(
                    v,
                    match self.lexical(v)? {
                        Some(s) => p.regex.is_match(&s),
                        None => false,
                    },
                    format!("Pattern[{}]: Does not match: {}", p.pattern, self.show(v))
                );
            }
            Constraint::LanguageIn(langs) => {
                each_value!(
                    v,
                    match self.term(v)? {
                        Term::Literal(l) => match l.language() {
                            Some(tag) => langs.iter().any(|r| lang_matches(tag, r)),
                            None => false,
                        },
                        _ => false,
                    },
                    format!(
                        "LanguageInConstraint[{}]: {} not in allowed languages",
                        langs.join(", "),
                        self.show(v)
                    )
                );
            }
            Constraint::UniqueLang => {
                let mut counts: Vec<(String, usize)> = Vec::new();
                for &v in values {
                    if let Term::Literal(l) = self.term(v)?
                        && let Some(tag) = l.language()
                    {
                        let tag = tag.to_ascii_lowercase();
                        match counts.iter_mut().find(|(t, _)| *t == tag) {
                            Some(e) => e.1 += 1,
                            None => counts.push((tag, 1)),
                        }
                    }
                }
                for (tag, n) in counts {
                    if n > 1 {
                        self.report(out, si, focus, path(), None, comp(), None, || {
                            msg(format!(
                                "UniqueLangConstraint: Multiple values with language tag \"{tag}\""
                            ))
                        })?;
                    }
                }
            }
            Constraint::Equals(p) => {
                let others = self.objects(focus, self.id(*p))?;
                let pt = self.shapes.term(*p);
                for &v in values {
                    if !others.contains(&v) {
                        self.report(out, si, focus, path(), Some(v), comp(), None, || {
                            msg(format!(
                                "Equals[{pt}]: not equal: value node {} is not in {pt}",
                                self.show(v)
                            ))
                        })?;
                    }
                }
                for &o in &others {
                    if !values.contains(&o) {
                        self.report(out, si, focus, path(), Some(o), comp(), None, || {
                            msg(format!(
                                "Equals[{pt}]: not equal: {pt} value {} is not a value node",
                                self.show(o)
                            ))
                        })?;
                    }
                }
            }
            Constraint::Disjoint(p) => {
                let others = self.objects(focus, self.id(*p))?;
                let pt = self.shapes.term(*p);
                each_value!(
                    v,
                    !others.contains(&v),
                    format!("Disjoint[{pt}]: not disjoint: {} is in {pt}", self.show(v))
                );
            }
            Constraint::LessThan(p) | Constraint::LessThanOrEquals(p) => {
                let or_eq = matches!(c, Constraint::LessThanOrEquals(_));
                let others = self.objects(focus, self.id(*p))?;
                let pt = self.shapes.term(*p);
                for &v in values {
                    let vv = self.data.value(v);
                    for &o in &others {
                        let ok = match (&vv, self.data.value(o)) {
                            (Some(a), Some(b)) => match value::compare(a, &b) {
                                Ok(Some(Ordering::Less)) => true,
                                Ok(Some(Ordering::Equal)) => or_eq,
                                _ => false,
                            },
                            _ => false,
                        };
                        if !ok {
                            self.report(out, si, focus, path(), Some(v), comp(), None, || {
                                msg(format!(
                                    "{}[{pt}]: value node {} is not {} {}",
                                    if or_eq {
                                        "LessThanOrEquals"
                                    } else {
                                        "LessThan"
                                    },
                                    self.show(v),
                                    if or_eq {
                                        "less than or equal to"
                                    } else {
                                        "less than"
                                    },
                                    self.show(o)
                                ))
                            })?;
                            if out.stop() {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            Constraint::Not(s) => {
                for &v in values {
                    if self.conforms(*s, v, cx)? {
                        self.report(out, si, focus, path(), Some(v), comp(), None, || {
                            msg(format!(
                                "Not[{}] at focusNode {}",
                                self.shapes.shapes[*s].node,
                                self.show(v)
                            ))
                        })?;
                        if out.stop() {
                            return Ok(());
                        }
                    }
                }
            }
            Constraint::And(ss) | Constraint::Or(ss) | Constraint::Xone(ss) => {
                for &v in values {
                    let mut n = 0usize;
                    for &s in ss {
                        if self.conforms(s, v, cx)? {
                            n += 1;
                        } else if matches!(c, Constraint::And(_)) {
                            break;
                        }
                        if matches!(c, Constraint::Or(_)) && n > 0 {
                            break;
                        }
                    }
                    let (ok, name) = match c {
                        Constraint::And(_) => (n == ss.len(), "And"),
                        Constraint::Or(_) => (n > 0, "Or"),
                        _ => (n == 1, "Xone"),
                    };
                    if !ok {
                        self.report(out, si, focus, path(), Some(v), comp(), None, || {
                            msg(format!("{name} at focusNode {}", self.show(v)))
                        })?;
                        if out.stop() {
                            return Ok(());
                        }
                    }
                }
            }
            Constraint::Node(s) => {
                for &v in values {
                    if !self.conforms(*s, v, cx)? {
                        self.report(out, si, focus, path(), Some(v), comp(), None, || {
                            msg(format!(
                                "Node[{}] at focusNode {}",
                                self.shapes.shapes[*s].node,
                                self.show(v)
                            ))
                        })?;
                        if out.stop() {
                            return Ok(());
                        }
                    }
                }
            }
            Constraint::Property(ps) => {
                for &v in values {
                    if cx.stack.contains(&(*ps, v)) {
                        continue;
                    }
                    cx.stack.push((*ps, v));
                    let r = self.validate_focus(*ps, v, out, cx);
                    cx.stack.pop();
                    r?;
                    if out.stop() {
                        return Ok(());
                    }
                }
            }
            Constraint::QualifiedMin(q) | Constraint::QualifiedMax(q) => {
                let n = self.qualified_count(q, values, cx)?;
                let (fail, text) = match c {
                    Constraint::QualifiedMin(_) => {
                        let m = q.min.unwrap_or(0);
                        (
                            n < m,
                            format!(
                                "QualifiedValueShape[{}]: Expected at least {m} values conforming to the qualified value shape, got {n}",
                                self.shapes.shapes[q.shape].node
                            ),
                        )
                    }
                    _ => {
                        let m = q.max.unwrap_or(u64::MAX);
                        (
                            n > m,
                            format!(
                                "QualifiedValueShape[{}]: Expected at most {m} values conforming to the qualified value shape, got {n}",
                                self.shapes.shapes[q.shape].node
                            ),
                        )
                    }
                };
                if fail {
                    self.report(out, si, focus, path(), None, comp(), None, || msg(text))?;
                }
            }
            Constraint::Closed { allowed, .. } => {
                let allowed: Vec<Id> = allowed.iter().map(|t| self.id(*t)).collect();
                for &v in values {
                    let edges = if self.controls.active() {
                        self.data.out_edges_checked(v, || self.check_data_scan())?
                    } else {
                        self.data.out_edges(v)?
                    };
                    for (p, o) in edges {
                        if !allowed.contains(&p) {
                            self.report(
                                out,
                                si,
                                focus,
                                RPath::Pred(p),
                                Some(o),
                                comp(),
                                None,
                                || {
                                    msg(format!(
                                        "Closed[{}] Property {} : Object {}",
                                        allowed
                                            .iter()
                                            .map(|a| self.show(*a))
                                            .collect::<Vec<_>>()
                                            .join(" "),
                                        self.show(p),
                                        self.show(o)
                                    ))
                                },
                            )?;
                            if out.stop() {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            Constraint::HasValue(t) => {
                let want = self.id(*t);
                if !values.contains(&want) {
                    self.report(out, si, focus, path(), None, comp(), None, || {
                        msg(format!("HasValue[{}]", self.shapes.term(*t)))
                    })?;
                }
            }
            Constraint::In(ts) => {
                let allowed: Vec<Id> = ts.iter().map(|t| self.id(*t)).collect();
                each_value!(
                    v,
                    allowed.contains(&v),
                    format!(
                        "InConstraint[{}] : Expected one of the list : {}",
                        ts.iter()
                            .map(|t| self.shapes.term(*t).to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        self.show(v)
                    )
                );
            }
            Constraint::Sparql(sc) => {
                self.check_sparql(si, sc, focus, out)?;
            }
            Constraint::MemberShape(s) => {
                for &v in values {
                    let Some(members) = self.shacl_list(v)? else {
                        self.not_a_list(out, si, focus, path(), v, comp())?;
                        if out.stop() {
                            return Ok(());
                        }
                        continue;
                    };
                    let mut failing = Vec::new();
                    for &m in &members {
                        if !self.conforms(*s, m, cx)? {
                            failing.push(m);
                            if !out.collect {
                                break;
                            }
                        }
                    }
                    if failing.is_empty() {
                        continue;
                    }
                    let n = failing.len();
                    self.report(out, si, focus, path(), Some(v), comp(), None, || {
                        msg(format!(
                            "MemberShape[{}]: {n} member{} of {} do{} not conform",
                            self.shapes.shapes[*s].node,
                            if n == 1 { "" } else { "s" },
                            self.show(v),
                            if n == 1 { "es" } else { "" },
                        ))
                    })?;
                    if out.stop() {
                        return Ok(());
                    }
                    // the details: each failing member's results against the member shape
                    let mut details = Vec::new();
                    for m in failing {
                        let mut sub = Out::collect();
                        cx.stack.push((*s, m));
                        let r = self.validate_focus(*s, m, &mut sub, cx);
                        cx.stack.pop();
                        r?;
                        details.extend(sub.results);
                    }
                    Self::attach_details(out, details);
                }
            }
            Constraint::MinListLength(n) | Constraint::MaxListLength(n) => {
                let min = matches!(c, Constraint::MinListLength(_));
                for &v in values {
                    let Some(members) = self.shacl_list(v)? else {
                        self.not_a_list(out, si, focus, path(), v, comp())?;
                        if out.stop() {
                            return Ok(());
                        }
                        continue;
                    };
                    let len = members.len() as u64;
                    if (min && len < *n) || (!min && len > *n) {
                        self.report(out, si, focus, path(), Some(v), comp(), None, || {
                            msg(format!(
                                "{}[{n}]: {} has {len} member{}",
                                if min {
                                    "MinListLength"
                                } else {
                                    "MaxListLength"
                                },
                                self.show(v),
                                if len == 1 { "" } else { "s" },
                            ))
                        })?;
                        if out.stop() {
                            return Ok(());
                        }
                    }
                }
            }
            Constraint::UniqueMembers(unique) => {
                for &v in values {
                    let Some(members) = self.shacl_list(v)? else {
                        self.not_a_list(out, si, focus, path(), v, comp())?;
                        if out.stop() {
                            return Ok(());
                        }
                        continue;
                    };
                    if !unique {
                        continue;
                    }
                    // each repeated member once, in the order of its first repeat
                    let mut seen = FxHashSet::default();
                    let mut repeated = Vec::new();
                    for &m in &members {
                        if !seen.insert(m) && !repeated.contains(&m) {
                            repeated.push(m);
                        }
                    }
                    if repeated.is_empty() {
                        continue;
                    }
                    self.report(out, si, focus, path(), Some(v), comp(), None, || {
                        msg(format!(
                            "UniqueMembers: {} repeats {}",
                            self.show(v),
                            repeated
                                .iter()
                                .map(|m| self.show(*m))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })?;
                    if out.stop() {
                        return Ok(());
                    }
                    if out.collect {
                        let mut sub = Out::collect();
                        for m in repeated {
                            self.report(
                                &mut sub,
                                si,
                                focus,
                                path(),
                                Some(m),
                                comp(),
                                None,
                                || {
                                    msg(format!(
                                        "UniqueMembers: {} occurs more than once in {}",
                                        self.show(m),
                                        self.show(v)
                                    ))
                                },
                            )?;
                        }
                        Self::attach_details(out, sub.results);
                    }
                }
            }
            Constraint::Component(cc) => {
                self.check_component(si, cc, focus, values, out)?;
            }
            Constraint::Expression(e) => {
                // for each value node v, the expression evaluated at the focus node with
                // `value` bound to v must give exactly ( true )
                let source = Some(self.shapes.expr_nodes[*e].clone());
                for &v in values {
                    let r = self.eval(*e, focus, &Scope::with_var("value", v), cx)?;
                    if !self.only_true(&r) {
                        self.report(
                            out,
                            si,
                            focus,
                            path(),
                            Some(v),
                            comp(),
                            source.clone(),
                            || {
                                msg(format!(
                                    "Expression {} does not give true for {}",
                                    self.shapes.expr_nodes[*e],
                                    self.show(v)
                                ))
                            },
                        )?;
                        if out.stop() {
                            return Ok(());
                        }
                    }
                }
            }
            Constraint::NodeByExpression(e) => {
                for &v in values {
                    let computed = self.eval(*e, v, &Scope::default(), cx)?;
                    for s in computed {
                        let term = self.term(s)?;
                        // a node no shape declaration describes has no constraints
                        let Some(&target) = self.shapes.by_node.get(&term) else {
                            continue;
                        };
                        if !self.conforms(target, v, cx)? {
                            self.report(
                                out,
                                si,
                                focus,
                                path(),
                                Some(v),
                                comp(),
                                Some(term.clone()),
                                || {
                                    msg(format!(
                                        "NodeByExpression[{term}] at focusNode {}",
                                        self.show(v)
                                    ))
                                },
                            )?;
                            if out.stop() {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn qualified_count(&self, q: &Qualified, values: &[Id], cx: &mut Cx) -> Result<u64> {
        let mut n = 0u64;
        'values: for &v in values {
            if !self.conforms(q.shape, v, cx)? {
                continue;
            }
            if q.disjoint {
                for &s in &q.siblings {
                    if self.conforms(s, v, cx)? {
                        continue 'values;
                    }
                }
            }
            n += 1;
        }
        Ok(n)
    }
}

/// SPARQL `langMatches` (RFC 4647 basic filtering).
pub(crate) fn lang_matches(tag: &str, range: &str) -> bool {
    if range == "*" {
        return !tag.is_empty();
    }
    let (tag, range) = (tag.to_ascii_lowercase(), range.to_ascii_lowercase());
    tag == range || (tag.starts_with(&range) && tag.as_bytes().get(range.len()) == Some(&b'-'))
}

#[cfg(test)]
mod control_tests {
    use super::*;
    use sparkles_core::io::{RdfFormat, Source};
    use sparkles_core::store::{Store, StoreOptions};

    thread_local! {
        static EXPIRE_IN_CONFORMANCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    pub(super) fn observe_conformance_probe() {
        if EXPIRE_IN_CONFORMANCE.replace(false) {
            crate::data::set_deadline_checks_left(Some(16));
        }
    }

    #[test]
    fn conformance_target_probes_poll_excluded_and_deleted_adjacency() {
        use crate::vocab::{SH_NS, SHNEX_NS};
        use sparkles_core::index::Perm;
        use std::fmt::Write;

        let store = Store::in_memory(StoreOptions {
            bulk_threshold: 1,
            ..Default::default()
        });
        let mut input = String::from("<urn:x> <urn:keep> <urn:o> .\n<urn:x> <urn:bad> <urn:o> .\n");
        for i in 0..4096 {
            writeln!(input, "<urn:x> <urn:hidden> <urn:o:{i}> <urn:excluded> .").unwrap();
        }
        store
            .load(&[Source::from_bytes(
                input.into_bytes(),
                RdfFormat::NQuads,
                None,
            )])
            .unwrap();
        let before = store.snapshot();
        let excluded = before.lookup_iri("urn:excluded").unwrap();
        let mut tx = store.write();
        for key in before.scan_keys(Perm::Gspo, &[excluded.0]).unwrap() {
            assert!(tx.delete(Perm::Gspo.to_quad(&key)).unwrap());
        }
        tx.commit().unwrap();
        let after = store.snapshot();
        assert_eq!(after.delta.deletes(), 4096);
        let opts = ValidateOptions {
            data_graph: Some(sparkles_core::sparql::ctx::UNION_GRAPH_IRI.into()),
            exclude_graphs: vec!["urn:excluded".into()],
            deadline: Instant::now().checked_add(Duration::from_secs(60)),
            parallel: false,
            ..Default::default()
        };
        let mut misses = Vec::new();
        for (target, expected_len) in [
            ("sh:targetWhere <urn:probe>", 0),
            ("sh:targetNode [ shnex:nodesMatching <urn:probe> ]", 0),
            (
                "sh:targetNode [ shnex:conformsToShape ( <urn:x> <urn:probe> ) ]",
                1,
            ),
        ] {
            let shapes = Shapes::parse_for_gate(
                &format!(
                    "@prefix sh: <{SH_NS}> . @prefix shnex: <{SHNEX_NS}> .
                     <urn:S> {target} .
                     <urn:probe> sh:hasValue <urn:x> ; sh:closed true ;
                         sh:ignoredProperties ( <urn:keep> ) ."
                ),
                RdfFormat::Turtle,
                None,
                None,
            )
            .unwrap();
            let si = shapes.by_node[&Term::from(NamedNode::new_unchecked("urn:S"))];
            for snap in [&before, &after] {
                let (data, ids) = DataGraph::new(
                    snap.clone(),
                    opts.data_graph.as_deref(),
                    &opts.extra_graphs,
                    &opts.exclude_graphs,
                    &shapes,
                )
                .unwrap();
                let engine = Engine::new(&shapes, &data, ids, &opts).unwrap();
                assert_eq!(engine.focus_nodes(si).unwrap().len(), expected_len);
                let _clock = crate::data::ScopedDeadlineChecks::expire_after(100);
                EXPIRE_IN_CONFORMANCE.set(true);
                let result = engine.focus_nodes(si);
                assert!(
                    !EXPIRE_IN_CONFORMANCE.replace(false),
                    "{target} never entered conforms"
                );
                if !matches!(
                    result.as_ref().err().and_then(|e| e.downcast_ref()),
                    Some(sparkles_core::Error::Timeout)
                ) {
                    misses.push(format!(
                        "{target} with {} tombstones missed timeout: {result:?}",
                        snap.delta.deletes()
                    ));
                }
            }
        }
        assert!(misses.is_empty(), "{}", misses.join("\n"));
    }

    #[test]
    fn controlled_target_probes_check_class_comparison_and_list_reads() {
        use crate::vocab::{SH_NS, SHNEX_NS, rdf};
        use std::fmt::Write;

        for (constraint, predicate) in [
            ("sh:class <urn:MissingClass>", rdf::TYPE.as_str()),
            ("sh:equals <urn:compared>", "urn:compared"),
            ("sh:minListLength 1", rdf::FIRST.as_str()),
        ] {
            let store = Store::in_memory(StoreOptions {
                bulk_threshold: 1,
                ..Default::default()
            });
            let mut input = String::from("<urn:x> <urn:keep> <urn:o> .\n");
            for i in 0..4096 {
                writeln!(input, "<urn:x> <{predicate}> <urn:o:{i}> <urn:hidden> .").unwrap();
            }
            store
                .load(&[Source::from_bytes(
                    input.into_bytes(),
                    RdfFormat::NQuads,
                    None,
                )])
                .unwrap();
            let snap = store.snapshot();
            let shapes = Shapes::parse_for_gate(
                &format!(
                    "@prefix sh: <{SH_NS}> . @prefix shnex: <{SHNEX_NS}> .
                     <urn:S> sh:targetNode [
                         shnex:conformsToShape ( <urn:x> <urn:probe> )
                     ] .
                     <urn:probe> {constraint} ."
                ),
                RdfFormat::Turtle,
                None,
                None,
            )
            .unwrap();
            let si = shapes.by_node[&Term::from(NamedNode::new_unchecked("urn:S"))];
            let opts = ValidateOptions {
                data_graph: Some(sparkles_core::sparql::ctx::UNION_GRAPH_IRI.into()),
                exclude_graphs: vec!["urn:hidden".into()],
                deadline: Instant::now().checked_add(Duration::from_secs(60)),
                ..Default::default()
            };
            let (data, ids) = DataGraph::new(
                snap,
                opts.data_graph.as_deref(),
                &[],
                &opts.exclude_graphs,
                &shapes,
            )
            .unwrap();
            let engine = Engine::new(&shapes, &data, ids, &opts).unwrap();
            let false_node = engine.lit_id(&Literal::from(false).into());
            assert_eq!(
                engine.focus_nodes(si).unwrap(),
                [false_node],
                "{constraint}"
            );
            let _clock = crate::data::ScopedDeadlineChecks::expire_after(100);
            EXPIRE_IN_CONFORMANCE.set(true);
            let result = engine.focus_nodes(si);
            assert!(!EXPIRE_IN_CONFORMANCE.replace(false), "{constraint}");
            assert!(
                matches!(
                    result.as_ref().err().and_then(|e| e.downcast_ref()),
                    Some(sparkles_core::Error::Timeout)
                ),
                "{constraint}: {result:?}"
            );
        }
    }

    #[test]
    fn checked_graph_plans_keep_public_shacl_graph_answers() {
        use sparkles_core::sparql::QueryOptions;

        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:x> <urn:p> 1 .
              <urn:x> { <urn:x> <urn:p> 2 . }
              <urn:excluded> { <urn:x> <urn:p> 3 . }
              _:named { <urn:x> <urn:q> 4 . }"
                    .to_vec(),
                RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let opts = ValidateOptions {
            exclude_graphs: vec!["urn:excluded".into()],
            parallel: false,
            ..Default::default()
        };
        for (query, expected) in [
            ("SELECT $this ?value { GRAPH ?value {} }", 3),
            (
                "SELECT $this ?value { GRAPH ?value { OPTIONAL { $this <urn:p> ?o } FILTER (!BOUND(?value)) } }",
                3,
            ),
            (
                "SELECT $this ?value { GRAPH $this { $this <urn:p> ?value } }",
                1,
            ),
            (
                "SELECT $this ?value { GRAPH <urn:excluded> { $this <urn:p> ?value } }",
                1,
            ),
            (
                "SELECT $this ?value { GRAPH <urn:x-arq:DefaultGraph> { $this <urn:p> ?value } }",
                1,
            ),
            (
                "SELECT $this ?value { GRAPH <urn:x-arq:UnionGraph> { $this <urn:p> ?value } }",
                2,
            ),
        ] {
            let shape = format!(
                "@prefix sh: <http://www.w3.org/ns/shacl#> .
                 <urn:S> sh:targetNode <urn:x> ; sh:sparql [ sh:select '{query}' ] ."
            );
            let shapes = Shapes::parse(&shape, RdfFormat::Turtle, None).unwrap();
            let ordinary = validate(&snap, &shapes, &opts).unwrap();
            assert_eq!(ordinary.results.len(), expected, "{query}");
            let checked =
                validate_controlled(&snap, &shapes, &opts, &QueryOptions::default(), None).unwrap();
            assert_eq!(checked, ordinary, "{query}");
            assert!(
                Shapes::parse_for_gate(&shape, RdfFormat::Turtle, None, None).is_err(),
                "{query}"
            );
        }
    }

    #[test]
    fn validation_setup_uses_the_earlier_deadline_and_preserves_absolute_on_overflow() {
        let now = Instant::now();
        let absolute = now + Duration::from_secs(120);
        let mut opts = ValidateOptions {
            deadline: Some(absolute),
            timeout: Some(Duration::from_secs(60)),
            ..Default::default()
        };
        let deadline = ValidationControls::new(&opts).deadline.unwrap();
        assert!(deadline >= now + Duration::from_secs(60) && deadline < absolute);
        opts.deadline = Some(now);
        assert_eq!(ValidationControls::new(&opts).deadline, Some(now));
        opts.timeout = Some(Duration::MAX);
        assert_eq!(ValidationControls::new(&opts).deadline, Some(now));
    }

    #[test]
    fn controlled_public_named_graph_planning_obeys_memory_cap() {
        use sparkles_core::error::{Budget, BudgetKind};
        use sparkles_core::sparql::{QueryOptions, ctx::DEFAULT_GRAPH_IRI};
        use std::fmt::Write;
        use std::sync::atomic::AtomicU64;

        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::from("<urn:focus> <urn:p> <urn:o> .\n");
        for i in 0..4096 {
            writeln!(data, "<urn:focus> <urn:p> <urn:o> <urn:g:{i}> .").unwrap();
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::NQuads,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let opts = ValidateOptions {
            data_graph: Some(DEFAULT_GRAPH_IRI.into()),
            parallel: false,
            ..Default::default()
        };
        let max_memory_bytes = Some(24 * 1024);
        let baseline = Shapes::parse(
            "@prefix sh: <http://www.w3.org/ns/shacl#> .
             <urn:S> sh:targetNode <urn:focus> ;
                 sh:sparql [ sh:select 'SELECT $this WHERE {} LIMIT 1' ] .",
            RdfFormat::Turtle,
            None,
        )
        .unwrap();
        let baseline_query = QueryOptions {
            max_memory_bytes,
            ..Default::default()
        };
        let control = validate_controlled(&snap, &baseline, &opts, &baseline_query, None).unwrap();
        assert_eq!(
            control.results.len(),
            1,
            "the tight cap must admit checked setup"
        );
        let mut misses = Vec::new();
        for group in ["", "OPTIONAL { <urn:focus> <urn:p> ?o }"] {
            let shapes = Shapes::parse(
                &format!(
                    "@prefix sh: <http://www.w3.org/ns/shacl#> .
                     <urn:S> sh:targetNode <urn:focus> ;
                         sh:sparql [ sh:select 'SELECT $this WHERE {{ GRAPH ?g {{ {group} }} }} LIMIT 1' ] ."
                ),
                RdfFormat::Turtle,
                None,
            )
            .unwrap();
            let generous = QueryOptions {
                max_memory_bytes: Some(64 << 20),
                ..Default::default()
            };
            let result = validate_controlled(&snap, &shapes, &opts, &generous, None).unwrap();
            assert_eq!(result.results.len(), 1, "{group}");
            let work = Arc::new(AtomicU64::new(0));
            let tight = QueryOptions {
                max_memory_bytes,
                max_rows_produced: Some(1 << 20),
                work: Some(work.clone()),
                ..Default::default()
            };
            crate::sparql::take_embedded_plans();
            let result = validate_controlled(&snap, &shapes, &opts, &tight, None);
            let planned = crate::sparql::take_embedded_plans();
            let produced = work.load(AtomicOrdering::Relaxed);
            if !matches!(
                result.as_ref().err().and_then(|e| e.downcast_ref()),
                Some(sparkles_core::Error::BudgetExceeded(Budget {
                    kind: BudgetKind::Memory,
                    ..
                }))
            ) || produced != 0
                || planned != 0
            {
                misses.push(format!(
                    "GRAPH {{ {group} }} did not refuse during preparation: {result:?}, produced {produced}, completed plans {planned}"
                ));
            }
        }
        assert!(misses.is_empty(), "{}", misses.join("\n"));
    }

    #[test]
    fn controlled_public_graph_r12_refuses_wide_exists_during_preparation() {
        use sparkles_core::error::{Budget, BudgetKind};
        use sparkles_core::sparql::QueryOptions;
        use std::sync::atomic::AtomicU64;

        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:focus> <urn:p> <urn:o> .
                  <urn:g:0> { <urn:focus> <urn:p> <urn:o> . }
                  <urn:g:1> { <urn:focus> <urn:p> <urn:o> . }"
                    .to_vec(),
                RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let expr = format!(
            "<urn:missing> <urn:p> \"{}\" . <urn:missing> <urn:q> \"{}\" .",
            "L".repeat(65_521),
            "M".repeat(65_519),
        );
        let shape = format!(
            "@prefix sh: <http://www.w3.org/ns/shacl#> .
             <urn:S> sh:targetNode <urn:focus> ;
             sh:sparql [ sh:select '''SELECT $this WHERE {{
               GRAPH ?g {{ BIND(EXISTS {{ {expr} }} AS ?hit) }}
             }} LIMIT 1''' ] ."
        );
        let snap = store.snapshot();
        let shapes = Shapes::parse(&shape, RdfFormat::Turtle, None).unwrap();
        let opts = ValidateOptions {
            parallel: false,
            ..Default::default()
        };
        let ordinary = validate(&snap, &shapes, &opts).unwrap();
        assert_eq!(ordinary.results.len(), 1);
        let generous = QueryOptions {
            max_memory_bytes: Some(8 << 20),
            ..Default::default()
        };
        let checked = validate_controlled(&snap, &shapes, &opts, &generous, None).unwrap();
        assert_eq!(checked, ordinary);
        let rows = Arc::new(AtomicU64::new(0));
        let tight = QueryOptions {
            max_memory_bytes: Some(24 * 1024),
            max_rows_produced: Some(1 << 20),
            work: Some(rows.clone()),
            ..Default::default()
        };
        crate::sparql::take_embedded_plans();
        let result = validate_controlled(&snap, &shapes, &opts, &tight, None);
        let planned = crate::sparql::take_embedded_plans();
        let produced = rows.load(AtomicOrdering::Relaxed);
        assert!(
            matches!(
                result.as_ref().err().and_then(|error| error.downcast_ref()),
                Some(sparkles_core::Error::BudgetExceeded(Budget {
                    kind: BudgetKind::Memory,
                    ..
                }))
            ) && planned == 0
                && produced == 0,
            "wide GRAPH syntax reached execution: {result:?}, completed plans {planned}, produced {produced}"
        );
    }

    #[test]
    fn controlled_public_setup_observes_cancellation_and_relative_timeout() {
        use sparkles_core::sparql::QueryOptions;
        use std::fmt::Write;

        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let mut input = String::from("@prefix sh: <http://www.w3.org/ns/shacl#> .\n");
        for i in 0..1024 {
            writeln!(input, "<urn:S> sh:targetNode <urn:target:{i}> .").unwrap();
        }
        let shapes = Shapes::parse(&input, RdfFormat::Turtle, None).unwrap();
        assert!(shapes.terms.len() >= 1024);
        let mut misses = Vec::new();
        for source in ["legacy", "linked", "relative"] {
            let flag = Arc::new(AtomicBool::new(source == "legacy"));
            let opts = ValidateOptions {
                cancel: (source == "legacy").then(|| flag.clone()),
                cancellation: (source == "linked")
                    .then(|| sparkles_core::task::Cancellation::from_flags([flag.clone()])),
                timeout: (source == "relative").then_some(Duration::ZERO),
                parallel: false,
                ..Default::default()
            };
            crate::data::observe_mapping_with((source == "linked").then(|| flag.clone()));
            let result = validate_controlled(&snap, &shapes, &opts, &QueryOptions::default(), None);
            let visited = crate::data::mapped_terms();
            crate::data::observe_mapping_with(None);
            let typed = result.as_ref().err().and_then(|e| e.downcast_ref());
            let passed = match source {
                "relative" => matches!(typed, Some(sparkles_core::Error::Timeout)) && visited == 0,
                "legacy" => matches!(typed, Some(sparkles_core::Error::Cancelled)) && visited == 0,
                _ => {
                    matches!(typed, Some(sparkles_core::Error::Cancelled))
                        && (5..=256).contains(&visited)
                }
            };
            if !passed {
                misses.push(format!("{source} mapped {visited} terms: {result:?}"));
            }
        }
        assert!(misses.is_empty(), "{}", misses.join("\n"));
    }

    #[test]
    fn embedded_queries_keep_physical_default_and_named_graph_access() {
        use sparkles_core::sparql::ctx::DEFAULT_GRAPH_IRI;
        let store = Store::in_memory(StoreOptions {
            union_default_graph: true,
            ..Default::default()
        });
        store
            .load(&[Source::from_bytes(
                br#"@prefix sh: <http://www.w3.org/ns/shacl#> .
                    <urn:x> <urn:p> "default" .
                    _:g { <urn:x> <urn:p> "blank", "default" . }
                    <urn:extra> { <urn:x> <urn:p> "extra" . }
                    <urn:shapes> {
                        <urn:x> <urn:p> "shapes" .
                        <urn:S> sh:targetNode <urn:x> ;
                            sh:sparql [
                                sh:select '''SELECT $this ?value {
                                    $this <urn:p> ?value .
                                    GRAPH $shapesGraph { $this <urn:p> "shapes" }
                                }'''
                            ] .
                    }"#
                .to_vec(),
                RdfFormat::TriG,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let shapes = Shapes::from_store(&snap, Some("urn:shapes")).unwrap();
        let queries = sparkles_core::sparql::QueryOptions {
            max_memory_bytes: Some(1 << 20),
            ..Default::default()
        };
        for (selection, extra, exclude, expected) in [
            (
                None,
                vec![],
                vec!["urn:shapes"],
                vec!["blank", "default", "extra"],
            ),
            (Some("urn:extra"), vec![], vec![], vec!["extra"]),
            (
                Some(DEFAULT_GRAPH_IRI),
                vec!["urn:extra"],
                vec![],
                vec!["default", "extra"],
            ),
            (
                Some(DEFAULT_GRAPH_IRI),
                vec![],
                vec![DEFAULT_GRAPH_IRI],
                vec![],
            ),
        ] {
            let opts = ValidateOptions {
                data_graph: selection.map(str::to_owned),
                extra_graphs: extra.into_iter().map(str::to_owned).collect(),
                exclude_graphs: exclude.into_iter().map(str::to_owned).collect(),
                ..Default::default()
            };
            let mut values = validate_controlled(&snap, &shapes, &opts, &queries, None)
                .unwrap()
                .results
                .into_iter()
                .map(|r| match r.value.unwrap() {
                    Term::Literal(literal) => literal.value().to_owned(),
                    term => panic!("expected a literal result, got {term}"),
                })
                .collect::<Vec<_>>();
            values.sort();
            assert_eq!(values, expected, "{selection:?}");
        }
    }

    #[test]
    fn target_scan_expires_inside_excluded_or_deleted_rows() {
        let (_store, before, after, _) = crate::data::scan_fixture();
        for target in ["targetSubjectsOf", "targetObjectsOf"] {
            let shapes = Shapes::parse(
                &format!("<urn:S> <http://www.w3.org/ns/shacl#{target}> <urn:p> ."),
                RdfFormat::Turtle,
                None,
            )
            .unwrap();
            for snap in [&before, &after] {
                let deadline = Instant::now().checked_add(Duration::from_secs(60));
                let opts = ValidateOptions {
                    data_graph: Some(sparkles_core::sparql::ctx::UNION_GRAPH_IRI.into()),
                    exclude_graphs: vec!["urn:shapes".into()],
                    deadline,
                    parallel: false,
                    ..Default::default()
                };
                let (data, ids) = DataGraph::new(
                    snap.clone(),
                    opts.data_graph.as_deref(),
                    &opts.extra_graphs,
                    &opts.exclude_graphs,
                    &shapes,
                )
                .unwrap();
                let engine = Engine::new(&shapes, &data, ids, &opts).unwrap();
                assert!(engine.focus_nodes(0).unwrap().is_empty());
                let _clock = crate::data::ScopedDeadlineChecks::expire_after(8);
                let result = engine.focus_nodes(0);
                assert!(
                    matches!(
                        result.as_ref().err().and_then(|e| e.downcast_ref()),
                        Some(sparkles_core::Error::Timeout)
                    ),
                    "{target} finished without a raw-scan timeout: {result:?}"
                );
            }
        }
    }

    #[test]
    fn target_expression_expires_inside_excluded_and_deleted_rows() {
        use crate::vocab::{SH_NS, SHNEX_NS, rdf};
        use sparkles_core::index::Perm;
        use std::fmt::Write;

        let store = Store::in_memory(StoreOptions {
            bulk_threshold: 1,
            ..Default::default()
        });
        let mut input = String::new();
        for i in 0..4096 {
            writeln!(
                input,
                "<urn:fixed> <urn:p> <urn:o:{i}> <urn:excluded> .
                 <urn:s:{i}> {} <urn:Class> <urn:excluded> .",
                rdf::TYPE
            )
            .unwrap();
        }
        store
            .load(&[Source::from_bytes(
                input.into_bytes(),
                RdfFormat::NQuads,
                None,
            )])
            .unwrap();
        let before = store.snapshot();
        assert_eq!(before.delta.inserts(), 0);
        let graph = before.lookup_iri("urn:excluded").unwrap();
        let mut tx = store.write();
        for key in before.scan_keys(Perm::Gspo, &[graph.0]).unwrap() {
            assert!(tx.delete(Perm::Gspo.to_quad(&key)).unwrap());
        }
        tx.commit().unwrap();
        let after = store.snapshot();
        assert_eq!(after.delta.deletes(), 8192);
        let mut misses = Vec::new();
        for expression in [
            "shnex:pathValues <urn:p> ; shnex:focusNode <urn:fixed>",
            "shnex:instancesOf <urn:Class>",
        ] {
            let shapes = Shapes::parse_for_gate(
                &format!(
                    "@prefix sh: <{SH_NS}> .
                     @prefix shnex: <{SHNEX_NS}> .
                     <urn:S> sh:targetNode [ {expression} ] ."
                ),
                RdfFormat::Turtle,
                None,
                None,
            )
            .unwrap();
            assert!(matches!(
                shapes.shapes[0].targets.as_slice(),
                [Target::Expr(_)]
            ));
            let opts = ValidateOptions {
                data_graph: Some(sparkles_core::sparql::ctx::UNION_GRAPH_IRI.into()),
                exclude_graphs: vec!["urn:excluded".into()],
                deadline: Instant::now().checked_add(Duration::from_secs(60)),
                parallel: false,
                ..Default::default()
            };
            for snap in [&before, &after] {
                let (data, ids) = DataGraph::new(
                    snap.clone(),
                    opts.data_graph.as_deref(),
                    &opts.extra_graphs,
                    &opts.exclude_graphs,
                    &shapes,
                )
                .unwrap();
                let engine = Engine::new(&shapes, &data, ids, &opts).unwrap();
                assert!(engine.focus_nodes(0).unwrap().is_empty());
                let _clock = crate::data::ScopedDeadlineChecks::expire_after(16);
                let result = engine.focus_nodes(0);
                if !matches!(
                    result.as_ref().err().and_then(|e| e.downcast_ref()),
                    Some(sparkles_core::Error::Timeout)
                ) {
                    misses.push(format!(
                        "{expression} with {} tombstones finished without a raw-scan timeout: {result:?}",
                        snap.delta.deletes()
                    ));
                }
            }
        }
        assert!(misses.is_empty(), "{}", misses.join("\n"));
    }

    #[test]
    fn engine_setup_checks_inside_wide_path_translation() {
        use crate::vocab::SH_NS;
        let list = vec!["<urn:p>"; 1024].join(" ");
        let shapes = Shapes::parse_for_gate(
            &format!(
                "@prefix sh: <{SH_NS}> .
                 <urn:S> sh:targetNode <urn:x> ; sh:path ({list}) ."
            ),
            RdfFormat::Turtle,
            None,
            None,
        )
        .unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let (data, ids) = DataGraph::new(store.snapshot(), None, &[], &[], &shapes).unwrap();
        let deadline = Instant::now().checked_add(Duration::from_secs(60));
        let opts = ValidateOptions {
            deadline,
            ..Default::default()
        };
        let engine = Engine::new(&shapes, &data, ids.clone(), &opts).unwrap();
        assert!(matches!(engine.paths[0].as_ref(), Some(CPath::Seq(paths)) if paths.len() == 1024));
        let observed = crate::path::ObservePathCompile::start();
        let result = Engine::new(&shapes, &data, ids, &opts);
        let visited = observed.count();
        assert!(
            matches!(
                result.as_ref().err().and_then(|e| e.downcast_ref()),
                Some(sparkles_core::Error::Timeout)
            ),
            "wide path setup finished after translating {visited} predicates"
        );
        assert!(
            (5..=6).contains(&visited),
            "translated {visited} predicates"
        );
    }

    #[test]
    fn controlled_setup_cancels_during_physical_path_translation() {
        use crate::path::ObservePathCompile;
        use crate::vocab::SH_NS;
        use sparkles_core::sparql::QueryOptions;

        let list = vec!["<urn:p>"; 1024].join(" ");
        let shapes = Shapes::parse(
            &format!(
                "@prefix sh: <{SH_NS}> .
                 <urn:S> sh:targetNode <urn:x> ; sh:path ({list}) ."
            ),
            RdfFormat::Turtle,
            None,
        )
        .unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let flag = Arc::new(AtomicBool::new(false));
        let opts = ValidateOptions {
            cancellation: Some(sparkles_core::task::Cancellation::from_flags(
                [flag.clone()],
            )),
            ..Default::default()
        };
        let observed = ObservePathCompile::cancel_during_compile(flag);
        let result = validate_controlled(
            &store.snapshot(),
            &shapes,
            &opts,
            &QueryOptions::default(),
            None,
        );
        assert!(matches!(
            result.as_ref().err().and_then(|e| e.downcast_ref()),
            Some(sparkles_core::Error::Cancelled)
        ));
        assert_eq!(observed.count(), 5);
    }

    #[test]
    fn operation_controls_reach_engine_and_sparql_context() {
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:focus> <urn:p> <urn:o> .".to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let shapes = Shapes::parse("@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape ; sh:targetNode <urn:focus> ; sh:sparql [ sh:select 'SELECT $this WHERE {}' ] .", RdfFormat::Turtle, None).unwrap();
        let si = shapes
            .shapes
            .iter()
            .position(|s| s.node == Term::NamedNode(NamedNode::new_unchecked("urn:S")))
            .unwrap();
        let constraint = shapes.shapes[si]
            .constraints
            .iter()
            .find_map(|c| match c {
                Constraint::Sparql(c) => Some(c),
                _ => None,
            })
            .unwrap();
        for controlled in [false, true] {
            for source in 0..3 {
                let flag = Arc::new(AtomicBool::new(false));
                let primary = Arc::new(AtomicBool::new(false));
                let opts = ValidateOptions {
                    cancel: Some(primary.clone()),
                    cancellation: Some(sparkles_core::task::Cancellation::from_flags([
                        flag.clone()
                    ])),
                    ..Default::default()
                };
                let (mut data, ids) =
                    DataGraph::new(snap.clone(), None, &[], &[], &shapes).unwrap();
                let focus = data.resolve(
                    &Term::NamedNode(NamedNode::new_unchecked("urn:focus")),
                    false,
                );
                let mut engine = Engine::new(&shapes, &data, ids, &opts).unwrap();
                engine.query_limits = controlled.then(sparkles_core::sparql::QueryOptions::default);
                assert!(engine.check_limits().is_ok());
                match source {
                    0 => flag.store(true, AtomicOrdering::Relaxed),
                    1 => primary.store(true, AtomicOrdering::Relaxed),
                    _ => engine.controls.deadline = Some(Instant::now()),
                }
                let native = engine.check_limits().unwrap_err();
                let query = engine
                    .check_sparql(si, constraint, focus, &mut Out::collect())
                    .unwrap_err();
                for error in [native, query] {
                    let typed = error.downcast_ref::<sparkles_core::Error>();
                    assert!(
                        if source < 2 {
                            matches!(typed, Some(sparkles_core::Error::Cancelled))
                        } else {
                            matches!(typed, Some(sparkles_core::Error::Timeout))
                        },
                        "{error}"
                    );
                }
            }
        }
    }
}
