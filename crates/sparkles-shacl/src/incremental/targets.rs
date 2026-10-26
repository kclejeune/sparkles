//! Complete bounded target admission for affected form checks.

use super::bounded::{self, Work};
use crate::data::DataGraph;
use crate::nodeeval::Scope;
use crate::nodeexpr::{Expr, ExprId};
use crate::shapes::{Candidates, Constraint, ShapeId, Shapes, Target};
use crate::validate::{Cx, Engine};
use sparkles_core::error::{Error, Result};
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::Perm;

/// Admit graph-free expression producers before invoking the existing evaluator.
/// Shared subexpressions count each time, so doubling expressions cannot evade it.
pub(super) fn static_expression(
    shapes: &Shapes,
    e: ExprId,
    depth: usize,
    work: &mut Work<'_>,
) -> Result<()> {
    work.charge()?;
    if depth > 64 {
        return Err(Error::unsupported(
            "form target expression exceeds bounded discovery depth",
        ));
    }
    let next = depth + 1;
    match &shapes.exprs[e] {
        Expr::Const(_) | Expr::Empty | Expr::Var(_) => {}
        Expr::List(nodes) => {
            for _ in nodes {
                work.charge()?;
            }
        }
        Expr::Concat(xs) | Expr::Intersection(xs) => {
            for &x in xs {
                static_expression(shapes, x, next, work)?;
            }
        }
        Expr::Distinct(x)
        | Expr::Exists(x)
        | Expr::Count(x)
        | Expr::Min(x)
        | Expr::Max(x)
        | Expr::Sum(x)
        | Expr::Limit { nodes: x, .. }
        | Expr::Offset { nodes: x, .. } => static_expression(shapes, *x, next, work)?,
        Expr::Remove { nodes, remove } => {
            static_expression(shapes, *nodes, next, work)?;
            static_expression(shapes, *remove, next, work)?;
        }
        Expr::If { cond, then, els } => {
            static_expression(shapes, *cond, next, work)?;
            for &x in [then, els].into_iter().flatten() {
                static_expression(shapes, x, next, work)?;
            }
        }
        _ => {
            return Err(Error::unsupported(
                "form target expression requires an early-bounded data-reading producer",
            ));
        }
    }
    Ok(())
}

/// Filters that inspect only their input node may use the existing evaluator.
/// Graph-reading filters need an early-bounded data producer before form admission.
pub(super) fn local_filter(
    shapes: &Shapes,
    shape: ShapeId,
    depth: usize,
    work: &mut Work<'_>,
) -> Result<()> {
    work.charge()?;
    if depth > 64 {
        return Err(Error::unsupported(
            "form target filter exceeds bounded discovery depth",
        ));
    }
    let shape = &shapes.shapes[shape];
    if shape.deactivated {
        return Ok(());
    }
    if shape.path.is_some()
        || shape.values.is_some()
        || shape.default_value.is_some()
        || shape.deactivated_expr.is_some()
    {
        return Err(Error::unsupported(
            "form target filter requires an early-bounded data-reading producer",
        ));
    }
    for constraint in &shape.constraints {
        work.charge()?;
        match constraint {
            Constraint::Datatype(_)
            | Constraint::NodeKind(_)
            | Constraint::MinCount(_)
            | Constraint::MaxCount(_)
            | Constraint::MinExclusive(..)
            | Constraint::MinInclusive(..)
            | Constraint::MaxExclusive(..)
            | Constraint::MaxInclusive(..)
            | Constraint::MinLength(_)
            | Constraint::MaxLength(_)
            | Constraint::Pattern(_)
            | Constraint::UniqueLang
            | Constraint::HasValue(_) => {}
            Constraint::LanguageIn(xs) => {
                for _ in xs {
                    work.charge()?;
                }
            }
            Constraint::In(xs) => {
                for _ in xs {
                    work.charge()?;
                }
            }
            Constraint::Not(s) | Constraint::Node(s) | Constraint::Property(s) => {
                local_filter(shapes, *s, depth + 1, work)?
            }
            Constraint::And(ss) | Constraint::Or(ss) | Constraint::Xone(ss) => {
                for &s in ss {
                    local_filter(shapes, s, depth + 1, work)?;
                }
            }
            _ => {
                return Err(Error::unsupported(
                    "form target filter requires an early-bounded data-reading producer",
                ));
            }
        }
    }
    Ok(())
}

pub(super) struct Targets<'a, 'b> {
    pub(super) data: &'a DataGraph,
    pub(super) engine: &'b Engine<'a>,
    filters: std::cell::RefCell<rustc_hash::FxHashSet<ShapeId>>,
    expressions: std::cell::RefCell<std::collections::BTreeMap<(ShapeId, ExprId), Vec<Id>>>,
}

impl<'a, 'b> Targets<'a, 'b> {
    pub(super) fn new(data: &'a DataGraph, engine: &'b Engine<'a>) -> Self {
        Self {
            data,
            engine,
            expressions: Default::default(),
            filters: Default::default(),
        }
    }

    fn expression(
        &self,
        e: ExprId,
        shape: ShapeId,
        work: &mut Work<'_>,
    ) -> anyhow::Result<Vec<Id>> {
        if let Some(nodes) = self.expressions.borrow().get(&(shape, e)) {
            for _ in nodes {
                work.charge()?;
            }
            return Ok(nodes.clone());
        }
        static_expression(self.engine.shapes, e, 0, work)?;
        let focus = self.engine.lit_id(&self.engine.shapes.shapes[shape].node);
        let nodes = self
            .engine
            .eval(e, focus, &Scope::default(), &mut Cx::default())?;
        for _ in &nodes {
            work.charge()?;
        }
        self.expressions
            .borrow_mut()
            .insert((shape, e), nodes.clone());
        Ok(nodes)
    }

    fn admit_filter(&self, shape: ShapeId, work: &mut Work<'_>) -> Result<()> {
        work.charge()?;
        if !self.filters.borrow().contains(&shape) {
            local_filter(self.engine.shapes, shape, 0, work)?;
            work.charge()?;
            self.filters.borrow_mut().insert(shape);
        }
        Ok(())
    }

    fn where_candidates(&self, shape: ShapeId, work: &mut Work<'_>) -> anyhow::Result<Vec<Id>> {
        Ok(match self.engine.shapes.candidates(shape) {
            Candidates::All => bounded::project(self.data, Perm::Spo, &[], &[0, 2], work)?,
            Candidates::Instances(c) => bounded::instances(self.data, self.engine.id(c), work)?,
            Candidates::Terms(terms) => {
                let mut out = Vec::new();
                for t in terms {
                    work.charge()?;
                    let n = self.engine.id(t);
                    if self.is_node(n, work)? {
                        out.push(n);
                    }
                }
                out
            }
            Candidates::SubjectsOf(p) => match self.data.snap.lookup_iri(p.as_str()) {
                Some(p) => bounded::project(self.data, Perm::Pso, &[p.0], &[1], work)?,
                None => Vec::new(),
            },
            Candidates::ObjectsOf(p) => match self.data.snap.lookup_iri(p.as_str()) {
                Some(p) => bounded::project(self.data, Perm::Pos, &[p.0], &[1], work)?,
                None => Vec::new(),
            },
        })
    }

    fn is_node(&self, node: Id, work: &mut Work<'_>) -> anyhow::Result<bool> {
        if matches!(node.tag(), Tag::Local | Tag::Undef) {
            return Ok(false);
        }
        Ok(bounded::exists(self.data, Perm::Spo, &[node.0], work)?
            || bounded::exists(self.data, Perm::Osp, &[node.0], work)?)
    }

    pub(super) fn contains(
        &self,
        shape: ShapeId,
        focus: Id,
        work: &mut Work<'_>,
    ) -> anyhow::Result<bool> {
        for target in &self.engine.shapes.shapes[shape].targets {
            work.charge()?;
            let hit = match *target {
                Target::Node(t) => focus == self.engine.id(t),
                Target::Class(c) => {
                    let path = super::CPath::Seq(vec![
                        super::CPath::Pred(self.data.rdf_type),
                        super::CPath::ZeroOrMore(Box::new(super::CPath::Pred(
                            self.data.sub_class_of,
                        ))),
                    ]);
                    bounded::eval(&path, self.data, focus, false, work)?
                        .contains(&self.engine.id(c))
                }
                Target::SubjectsOf(p) => {
                    bounded::exists(self.data, Perm::Spo, &[focus.0, self.engine.id(p).0], work)?
                }
                Target::ObjectsOf(p) => {
                    bounded::exists(self.data, Perm::Pos, &[self.engine.id(p).0, focus.0], work)?
                }
                Target::Expr(e) => self.expression(e, shape, work)?.contains(&focus),
                Target::Where(w) => {
                    self.admit_filter(w, work)?;
                    let narrowed = !matches!(
                        self.engine.shapes.candidates(w),
                        Candidates::All | Candidates::Terms(_)
                    );
                    (narrowed || self.is_node(focus, work)?)
                        && self.engine.conforms(w, focus, &mut Cx::default())?
                }
            };
            if hit {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn visit_all<'w>(
        &self,
        shape: ShapeId,
        work: &mut Work<'w>,
        emit: &mut dyn FnMut(Id, &mut Work<'w>) -> Result<()>,
    ) -> anyhow::Result<()> {
        for target in &self.engine.shapes.shapes[shape].targets {
            work.charge()?;
            match *target {
                Target::Node(t) => emit(self.engine.id(t), work)?,
                Target::Class(c) => {
                    bounded::instances_visit(self.data, self.engine.id(c), work, emit)?
                }
                Target::SubjectsOf(p) => bounded::project_visit(
                    self.data,
                    Perm::Pso,
                    &[self.engine.id(p).0],
                    &[1],
                    work,
                    emit,
                )?,
                Target::ObjectsOf(p) => bounded::project_visit(
                    self.data,
                    Perm::Pos,
                    &[self.engine.id(p).0],
                    &[1],
                    work,
                    emit,
                )?,
                Target::Expr(e) => {
                    for n in self.expression(e, shape, work)? {
                        emit(n, work)?;
                    }
                }
                Target::Where(w) => {
                    self.admit_filter(w, work)?;
                    for n in self.where_candidates(w, work)? {
                        work.charge()?;
                        if self.engine.conforms(w, n, &mut Cx::default())? {
                            emit(n, work)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
