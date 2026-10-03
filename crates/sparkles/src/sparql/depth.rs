//! How deeply a parsed query or update nests, and the stack to plan and evaluate it on.
//!
//! spargebra refuses a text whose brackets nest deeper than
//! [`spargebra::nesting::MAX_NESTING`] or whose algebra could nest deeper than
//! [`spargebra::nesting::MAX_DEPTH`] levels, counted from the text before parsing (its
//! parser recurses once per bracket). The planner, the evaluator and the code that clones
//! or drops the algebra recurse once per level of the algebra. [`check_query`] and
//! [`check_update`] measure the parsed algebra itself, never recursing deeper than
//! [`MAX_ALGEBRA_DEPTH`], so that bound holds whatever the text count misses; and
//! [`with_stack`] gives the planner and the evaluator a stack that fits the depth they
//! measured, whatever thread they run on.

use crate::error::{Error, Result};
use oxrdf::Term;
use spargebra::algebra::{
    AggregateExpression, Expression, GraphPattern, OrderExpression, PropertyPathExpression,
};
use spargebra::term::{GroundTerm, GroundTermPattern, TermPattern, TriplePattern};
use spargebra::{GraphUpdateOperation, Query, Update};

/// The deepest algebra the engine plans and evaluates. A level of the text may be more
/// than one level of the algebra (a subquery is a projection, a slice and a group, a
/// filter an `Exists` and its group), so this is twice spargebra's bound on the text,
/// with room for the levels a query form adds.
pub const MAX_ALGEBRA_DEPTH: usize = 2 * spargebra::nesting::MAX_DEPTH + 64;

/// The stack each level of the algebra may take while it is planned and evaluated, with
/// room to spare: in a release build the executor takes about 7 KiB a level and the
/// planner 4 KiB; a debug build takes several times more.
const STACK_PER_LEVEL: usize = if cfg!(debug_assertions) {
    128 << 10
} else {
    16 << 10
};

/// The stack for everything around the recursion.
const STACK_BASE: usize = 256 << 10;

/// The largest stack [`with_stack`] asks for.
const STACK_MAX: usize = 1 << 30;

/// Refuse a query whose algebra nests deeper than [`MAX_ALGEBRA_DEPTH`]; else the depth
/// of its plan, for [`with_stack`].
pub fn check_query(q: &Query) -> Result<usize> {
    limit(Walk::new(MAX_ALGEBRA_DEPTH, false).query(q))?;
    Ok(Walk::new(usize::MAX, true).query(q))
}

/// Refuse an update whose algebra nests deeper than [`MAX_ALGEBRA_DEPTH`]; else the depth
/// of its plans, for [`with_stack`].
pub fn check_update(u: &Update) -> Result<usize> {
    limit(Walk::new(MAX_ALGEBRA_DEPTH, false).update(u))?;
    Ok(Walk::new(usize::MAX, true).update(u))
}

fn limit(depth: usize) -> Result<usize> {
    if depth > MAX_ALGEBRA_DEPTH {
        return Err(Error::invalid(format!(
            "query algebra nested deeper than {MAX_ALGEBRA_DEPTH} levels"
        )));
    }
    Ok(depth)
}

/// Run `f`, which plans or evaluates a plan `depth` levels deep, with the stack that takes:
/// on this thread's stack when enough of it is left, else on a new stack segment of this
/// thread (`stacker`). A spawned thread (2 MiB by default) or a rayon worker has room for
/// a few hundred levels.
pub fn with_stack<R>(depth: usize, f: impl FnOnce() -> R) -> R {
    let need = depth
        .saturating_mul(STACK_PER_LEVEL)
        .saturating_add(STACK_BASE)
        .min(STACK_MAX);
    stacker::maybe_grow(need, need, f)
}

/// The depth of a query's algebra, or `limit + 1` once it is deeper than `limit`.
pub fn query_depth(q: &Query, limit: usize) -> usize {
    Walk::new(limit, false).query(q)
}

/// The depth of an update's algebra, or `limit + 1` once it is deeper than `limit`.
pub fn update_depth(u: &Update, limit: usize) -> usize {
    Walk::new(limit, false).update(u)
}

/// A walk that never goes deeper than `limit + 1` levels. Each method takes the depth `d`
/// of the node it is given and returns the depth of its deepest descendant. With `bgp`, a
/// basic graph pattern counts one level per triple pattern, as the plan that joins them
/// nests (the depth the planner and the executor recurse to).
struct Walk {
    limit: usize,
    bgp: bool,
}

impl Walk {
    fn new(limit: usize, bgp: bool) -> Self {
        Self { limit, bgp }
    }

    fn query(&self, q: &Query) -> usize {
        match q {
            Query::Select { pattern, .. }
            | Query::Ask { pattern, .. }
            | Query::Describe { pattern, .. } => self.pattern(pattern, 1),
            Query::Construct {
                template,
                graph_templates,
                pattern,
                ..
            } => template
                .iter()
                .chain(graph_templates.iter().flat_map(|g| &g.triples))
                .map(|t| self.triple(t, 1))
                .fold(self.pattern(pattern, 1), usize::max),
        }
    }

    fn update(&self, u: &Update) -> usize {
        let mut depth = 0;
        for op in &u.operations {
            let d = match op {
                GraphUpdateOperation::InsertData { data } => data
                    .iter()
                    .map(|q| self.rdf(&q.object, 1))
                    .max()
                    .unwrap_or(0),
                GraphUpdateOperation::DeleteData { data } => data
                    .iter()
                    .map(|q| self.ground(&q.object, 1))
                    .max()
                    .unwrap_or(0),
                GraphUpdateOperation::DeleteInsert {
                    delete,
                    insert,
                    pattern,
                    ..
                } => {
                    let deleted = delete.iter().map(|q| {
                        self.ground_pattern(&q.subject, 1)
                            .max(self.ground_pattern(&q.object, 1))
                    });
                    let inserted = insert
                        .iter()
                        .map(|q| self.term(&q.subject, 1).max(self.term(&q.object, 1)));
                    deleted
                        .chain(inserted)
                        .fold(self.pattern(pattern, 1), usize::max)
                }
                _ => 0,
            };
            depth = depth.max(d);
        }
        depth
    }

    fn pattern(&self, p: &GraphPattern, d: usize) -> usize {
        if d > self.limit {
            return d;
        }
        let n = d + 1;
        match p {
            GraphPattern::Bgp { patterns } => {
                let terms = patterns
                    .iter()
                    .map(|t| self.triple(t, n))
                    .max()
                    .unwrap_or(d);
                if self.bgp {
                    terms.saturating_add(patterns.len())
                } else {
                    terms
                }
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => self
                .term(subject, n)
                .max(self.path(path, n))
                .max(self.term(object, n)),
            GraphPattern::Join { left, right }
            | GraphPattern::Lateral { left, right }
            | GraphPattern::Union { left, right }
            | GraphPattern::Minus { left, right } => {
                self.pattern(left, n).max(self.pattern(right, n))
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => expression.iter().map(|e| self.expr(e, n)).fold(
                self.pattern(left, n).max(self.pattern(right, n)),
                usize::max,
            ),
            GraphPattern::Filter { expr, inner } => self.expr(expr, n).max(self.pattern(inner, n)),
            GraphPattern::Extend {
                inner, expression, ..
            }
            | GraphPattern::Assign {
                inner, expression, ..
            }
            | GraphPattern::Unfold {
                inner, expression, ..
            } => self.pattern(inner, n).max(self.expr(expression, n)),
            GraphPattern::Values { bindings, .. } => bindings
                .iter()
                .flatten()
                .flatten()
                .map(|t| self.ground(t, n))
                .fold(d, usize::max),
            GraphPattern::OrderBy { inner, expression } => expression
                .iter()
                .map(|e| match e {
                    OrderExpression::Asc(e) | OrderExpression::Desc(e) => self.expr(e, n),
                })
                .fold(self.pattern(inner, n), usize::max),
            GraphPattern::Group {
                inner, aggregates, ..
            } => aggregates
                .iter()
                .map(|(_, a)| match a {
                    AggregateExpression::CountSolutions { .. } => n,
                    AggregateExpression::FunctionCall { expr, .. } => self.expr(expr, n),
                    AggregateExpression::Fold {
                        expr, value, order, ..
                    } => value
                        .iter()
                        .chain(order.iter().map(|o| match o {
                            OrderExpression::Asc(e) | OrderExpression::Desc(e) => e,
                        }))
                        .map(|e| self.expr(e, n))
                        .fold(self.expr(expr, n), usize::max),
                })
                .fold(self.pattern(inner, n), usize::max),
            GraphPattern::Graph { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::Service { inner, .. } => self.pattern(inner, n),
        }
    }

    fn expr(&self, e: &Expression, d: usize) -> usize {
        if d > self.limit {
            return d;
        }
        let n = d + 1;
        match e {
            Expression::NamedNode(_)
            | Expression::Literal(_)
            | Expression::Variable(_)
            | Expression::Bound(_) => d,
            Expression::Or(a, b)
            | Expression::And(a, b)
            | Expression::Equal(a, b)
            | Expression::SameTerm(a, b)
            | Expression::Greater(a, b)
            | Expression::GreaterOrEqual(a, b)
            | Expression::Less(a, b)
            | Expression::LessOrEqual(a, b)
            | Expression::Add(a, b)
            | Expression::Subtract(a, b)
            | Expression::Multiply(a, b)
            | Expression::Divide(a, b) => self.expr(a, n).max(self.expr(b, n)),
            Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
                self.expr(a, n)
            }
            Expression::In(a, list) => list
                .iter()
                .map(|e| self.expr(e, n))
                .fold(self.expr(a, n), usize::max),
            Expression::If(a, b, c) => self.expr(a, n).max(self.expr(b, n)).max(self.expr(c, n)),
            Expression::Coalesce(list) | Expression::FunctionCall(_, list) => {
                list.iter().map(|e| self.expr(e, n)).fold(d, usize::max)
            }
            Expression::Exists(p) => self.pattern(p, n),
        }
    }

    fn path(&self, p: &PropertyPathExpression, d: usize) -> usize {
        if d > self.limit {
            return d;
        }
        let n = d + 1;
        match p {
            PropertyPathExpression::NamedNode(_)
            | PropertyPathExpression::NegatedPropertySet(_) => d,
            PropertyPathExpression::Reverse(a)
            | PropertyPathExpression::ZeroOrMore(a)
            | PropertyPathExpression::OneOrMore(a)
            | PropertyPathExpression::ZeroOrOne(a) => self.path(a, n),
            PropertyPathExpression::Sequence(a, b) | PropertyPathExpression::Alternative(a, b) => {
                self.path(a, n).max(self.path(b, n))
            }
            // the planner unrolls up to 32 steps of a range into a chain of joins
            PropertyPathExpression::Range { path, min, .. } => {
                self.path(path, n).saturating_add((*min).min(32) as usize)
            }
        }
    }

    fn triple(&self, t: &TriplePattern, d: usize) -> usize {
        self.term(&t.subject, d).max(self.term(&t.object, d))
    }

    fn term(&self, t: &TermPattern, d: usize) -> usize {
        match t {
            TermPattern::Triple(t) if d <= self.limit => self.triple(t, d + 1),
            _ => d,
        }
    }

    fn ground(&self, t: &GroundTerm, d: usize) -> usize {
        match t {
            GroundTerm::Triple(t) if d <= self.limit => self.ground(&t.object, d + 1),
            _ => d,
        }
    }

    fn ground_pattern(&self, t: &GroundTermPattern, d: usize) -> usize {
        match t {
            GroundTermPattern::Triple(t) if d <= self.limit => self
                .ground_pattern(&t.subject, d + 1)
                .max(self.ground_pattern(&t.object, d + 1)),
            _ => d,
        }
    }

    fn rdf(&self, t: &Term, d: usize) -> usize {
        match t {
            Term::Triple(t) if d <= self.limit => self.rdf(&t.object, d + 1),
            _ => d,
        }
    }
}
