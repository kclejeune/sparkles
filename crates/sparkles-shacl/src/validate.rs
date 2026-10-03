//! The validation engine (SHACL §3–4).

use crate::data::DataGraph;
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
    pub cancel: Option<Arc<AtomicBool>>,
    /// Stop with [`TooManyResults`] once the report would hold more results than this.
    pub max_results: Option<usize>,
}

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
            cancel: None,
            max_results: None,
        }
    }
}

/// Validate the data graph of a snapshot against a shapes graph.
pub fn validate(
    snap: &Arc<Snapshot>,
    shapes: &Shapes,
    opts: &ValidateOptions,
) -> Result<ValidationReport> {
    let (data, ids) = DataGraph::new(
        snap.clone(),
        opts.data_graph.as_deref(),
        &opts.extra_graphs,
        &opts.exclude_graphs,
        shapes,
    )?;
    let engine = Engine::new(shapes, &data, ids, opts)?;
    engine.run(None)
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
    let focus = data.resolve(node, shapes.bnodes_in_store);
    let engine = Engine::new(shapes, &data, ids, opts)?;
    engine.run(Some(focus))
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
            if let Sel::Nodes(nodes) = s {
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
}

pub(crate) struct Engine<'a> {
    pub shapes: &'a Shapes,
    pub data: &'a DataGraph,
    /// store id of each term of the shapes' term table
    pub ids: Vec<Id>,
    paths: Vec<Option<CPath>>,
    parallel: bool,
    pool: Option<Arc<rayon::ThreadPool>>,
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<AtomicBool>>,
    max_results: Option<usize>,
    /// results collected so far (checked against `max_results`)
    collected: AtomicUsize,
}

/// Members past which a list counts as ill-formed (a hostile list cannot exhaust memory).
const MAX_LIST_MEMBERS: usize = 1 << 24;

/// A one-element result message.
fn msg(s: String) -> Vec<Literal> {
    vec![Literal::new_simple_literal(s)]
}

impl<'a> Engine<'a> {
    fn new(
        shapes: &'a Shapes,
        data: &'a DataGraph,
        ids: Vec<Id>,
        opts: &ValidateOptions,
    ) -> Result<Engine<'a>> {
        let paths = shapes
            .shapes
            .iter()
            .map(|s| {
                s.path.as_ref().map(|p| {
                    CPath::compile(p, &mut |n| {
                        data.snap
                            .lookup_iri(n.as_str())
                            .unwrap_or(Id::local(u64::MAX >> 8))
                    })
                })
            })
            .collect();
        Ok(Engine {
            shapes,
            data,
            ids,
            paths,
            parallel: opts.parallel,
            pool: opts.pool.clone(),
            deadline: opts.timeout.map(|t| Instant::now() + t),
            cancel: opts.cancel.clone(),
            max_results: opts.max_results,
            collected: AtomicUsize::new(0),
        })
    }

    fn check_limits(&self) -> Result<()> {
        if let Some(c) = &self.cancel
            && c.load(AtomicOrdering::Relaxed)
        {
            bail!("validation cancelled");
        }
        if let Some(d) = self.deadline
            && Instant::now() > d
        {
            bail!("validation timed out");
        }
        Ok(())
    }

    /// Count `n` more results against `max_results`.
    fn count_results(&self, n: usize) -> Result<()> {
        let Some(limit) = self.max_results else {
            return Ok(());
        };
        if n > 0 && self.collected.fetch_add(n, AtomicOrdering::Relaxed) + n > limit {
            return Err(TooManyResults { limit }.into());
        }
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
        let sel: Vec<Sel> = (0..self.shapes.shapes.len())
            .map(|_| match only {
                None => Sel::All,
                Some(f) => Sel::Nodes(vec![f]),
            })
            .collect();
        let results: Vec<ValidationResult> = self
            .run_shapes(&sel)?
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
                    let before = out.results.len();
                    self.validate_focus(si, f, &mut out, &mut cx)?;
                    self.count_results(out.results.len() - before)?;
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
    fn focus_nodes(&self, si: ShapeId) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        for t in &self.shapes.shapes[si].targets {
            let nodes = match *t {
                Target::Node(t) => vec![self.id(t)],
                Target::Class(c) => self.data.instances(self.id(c))?,
                Target::SubjectsOf(p) => self.data.subjects_of(self.id(p))?,
                Target::ObjectsOf(p) => self.data.objects_of(self.id(p))?,
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
                if seen.insert(n) {
                    out.push(n);
                }
            }
        }
        Ok(out)
    }

    fn is_target(&self, si: ShapeId, f: Id) -> Result<bool> {
        for t in &self.shapes.shapes[si].targets {
            let hit = match *t {
                Target::Node(t) => self.id(t) == f,
                Target::Class(c) => self.data.is_instance(f, self.id(c))?,
                Target::SubjectsOf(p) => !self.data.objects(f, self.id(p))?.is_empty(),
                Target::ObjectsOf(p) => !self.data.subjects(self.id(p), f)?.is_empty(),
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
    fn candidates(&self, si: ShapeId) -> Result<Vec<Id>> {
        let pred = |p: &NamedNode| {
            self.data
                .snap
                .lookup_iri(p.as_str())
                .unwrap_or(Id::local(u64::MAX >> 8))
        };
        Ok(match self.shapes.candidates(si) {
            Candidates::All => self.data.nodes()?,
            Candidates::Instances(c) => self.data.instances(self.id(c))?,
            Candidates::Terms(ts) => {
                let mut out = Vec::new();
                for t in ts {
                    let id = self.id(t);
                    if self.is_node(id)? && !out.contains(&id) {
                        out.push(id);
                    }
                }
                out
            }
            Candidates::SubjectsOf(p) => self.data.subjects_of(pred(&p))?,
            Candidates::ObjectsOf(p) => self.data.objects_of(pred(&p))?,
        })
    }

    /// Whether `n` is a node of the data graph (the subject or object of a triple).
    fn is_node(&self, n: Id) -> Result<bool> {
        if matches!(
            n.tag(),
            sparkles_core::id::Tag::Local | sparkles_core::id::Tag::Undef
        ) {
            return Ok(false);
        }
        let mut found = false;
        self.data.scan_out_edges(n, |_, _| {
            found = true;
            false
        })?;
        if !found {
            found = self.data.has_in_edge(n)?;
        }
        Ok(found)
    }

    /// Value nodes of a shape for a focus node.
    pub fn value_nodes(&self, si: ShapeId, focus: Id) -> Result<Vec<Id>> {
        match &self.paths[si] {
            Some(p) => p.eval(self.data, focus),
            None => Ok(vec![focus]),
        }
    }

    pub(crate) fn validate_focus(
        &self,
        si: ShapeId,
        focus: Id,
        out: &mut Out,
        cx: &mut Cx,
    ) -> Result<()> {
        let shape = &self.shapes.shapes[si];
        if shape.deactivated {
            return Ok(());
        }
        let values = self.value_nodes(si, focus)?;
        for c in &shape.constraints {
            self.check(si, c, focus, &values, out, cx)?;
            if out.stop() {
                break;
            }
        }
        Ok(())
    }

    /// Does `v` conform to shape `si`?
    pub(crate) fn conforms(&self, si: ShapeId, v: Id, cx: &mut Cx) -> Result<bool> {
        if self.shapes.shapes[si].deactivated || cx.stack.contains(&(si, v)) {
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
        let r = self.validate_focus(si, v, &mut out, cx);
        cx.stack.pop();
        r?;
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
        out.results.push(ValidationResult {
            focus_node: focus,
            result_path,
            value,
            source_shape: shape.node.clone(),
            source_constraint_component: component,
            source_constraint,
            severity: shape.severity.clone(),
            messages,
            details: Vec::new(),
        });
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
            let firsts = self.data.objects(n, first)?;
            let rests = self.data.objects(n, rest)?;
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

    fn show(&self, id: Id) -> String {
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
                    self.data.is_instance(v, cid)?,
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
                let others = self.data.objects(focus, self.id(*p))?;
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
                let others = self.data.objects(focus, self.id(*p))?;
                let pt = self.shapes.term(*p);
                each_value!(
                    v,
                    !others.contains(&v),
                    format!("Disjoint[{pt}]: not disjoint: {} is in {pt}", self.show(v))
                );
            }
            Constraint::LessThan(p) | Constraint::LessThanOrEquals(p) => {
                let or_eq = matches!(c, Constraint::LessThanOrEquals(_));
                let others = self.data.objects(focus, self.id(*p))?;
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
                    for (p, o) in self.data.out_edges(v)? {
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
