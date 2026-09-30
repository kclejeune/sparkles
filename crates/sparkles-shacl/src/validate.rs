//! The validation engine (SHACL §3–4).

use crate::data::DataGraph;
use crate::path::{CPath, PropertyPath};
use crate::report::{ValidationReport, ValidationResult};
use crate::shapes::{Constraint, Qualified, ShapeId, Shapes, Target};
use crate::xsd;
use anyhow::{Result, bail};
use oxrdf::{Literal, NamedNode, Term};
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use sparkles::id::Id;
use sparkles::sparql::value;
use sparkles::store::Snapshot;
use std::cmp::Ordering;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
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
    /// Validate focus nodes in parallel (rayon).
    pub parallel: bool,
    pub timeout: Option<Duration>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for ValidateOptions {
    fn default() -> Self {
        ValidateOptions {
            data_graph: None,
            extra_graphs: Vec::new(),
            parallel: true,
            timeout: None,
            cancel: None,
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
        shapes,
    )?;
    let focus = data.resolve(node, shapes.bnodes_in_store);
    let engine = Engine::new(shapes, &data, ids, opts)?;
    engine.run(Some(focus))
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
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<AtomicBool>>,
}

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
            deadline: opts.timeout.map(|t| Instant::now() + t),
            cancel: opts.cancel.clone(),
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
        let mut results = Vec::new();
        for (si, shape) in self.shapes.shapes.iter().enumerate() {
            if shape.targets.is_empty() || shape.deactivated {
                continue;
            }
            self.check_limits()?;
            let focus = match only {
                None => self.focus_nodes(si)?,
                Some(f) => {
                    if self.is_target(si, f)? {
                        vec![f]
                    } else {
                        continue;
                    }
                }
            };
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
                let parts: Vec<Result<Vec<ValidationResult>>> =
                    focus.par_chunks(256).map(chunk).collect();
                for p in parts {
                    results.extend(p?);
                }
            } else {
                results.extend(chunk(&focus)?);
            }
        }
        Ok(ValidationReport {
            conforms: results.is_empty(),
            results,
        })
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
            };
            if hit {
                return Ok(true);
            }
        }
        Ok(false)
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
        });
        Ok(())
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
