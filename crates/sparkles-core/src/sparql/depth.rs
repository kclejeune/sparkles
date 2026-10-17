//! How deeply a parsed query or update nests, and the stack to plan and evaluate it on.
//!
//! spargebra refuses a text whose brackets nest deeper than
//! [`spargebra::nesting::MAX_NESTING`] or whose algebra could nest deeper than
//! [`spargebra::nesting::MAX_DEPTH`] levels, counted from the text before parsing (its
//! parser recurses once per bracket). The planner, the evaluator and the code that clones
//! or drops the algebra recurse once per level of the algebra. [`check_query`] and
//! [`check_update`] measure the parsed algebra itself, never recursing deeper than
//! [`MAX_ALGEBRA_DEPTH`], so that bound holds whatever the text count misses. They also
//! refuse a plan deeper than [`MAX_PLAN_DEPTH`], which a large basic graph pattern
//! reaches without nesting at all, and [`with_stack`] gives the planner and the
//! evaluator a stack that fits the depth they measured, whatever thread they run on.
//! The count of the parsed query is an upper bound on the plan, so [`check_plan`] checks
//! the plan the planner built as well, and the evaluator's stack is sized from its depth.

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

/// The deepest plan the engine builds. The planner joins the triple patterns of a basic
/// graph pattern one after the other and puts a sort before a join, so the walk counts
/// two levels per triple pattern, and the unrolled steps of a path range. A text far
/// below the body limit could otherwise ask for more stack than [`STACK_MAX`]. The plan,
/// its description and its JSON form are all recursive, and at this depth they fit the
/// stack of any thread the server runs.
pub const MAX_PLAN_DEPTH: usize = 4000;

/// The stack allowance per plan level. Measured on a thread with a painted stack, a level
/// of the executor in a release build touches 4.7 KB in the eager path and up to 10.7 KB in
/// the cursor path, and 16 KiB covers both. A debug build touches up to 70 KB per level in
/// the eager path, and 256 KiB covers that. These are virtual stack requests, not resident
/// or query-budget memory. [`with_stack`] caps requests at [`STACK_MAX`].
const STACK_PER_LEVEL: usize = if cfg!(debug_assertions) {
    256 << 10
} else {
    16 << 10
};

/// The stack for everything around the recursion.
const STACK_BASE: usize = 256 << 10;

/// The largest stack [`with_stack`] asks for.
const STACK_MAX: usize = 1 << 30;

// a plan within the bound always gets all the stack it needs: this holds because the walk
// counts the levels of the plan and not the triple patterns of the query
const _: () = assert!(STACK_BASE + MAX_PLAN_DEPTH * STACK_PER_LEVEL <= STACK_MAX);

/// Refuse a query whose algebra nests deeper than [`MAX_ALGEBRA_DEPTH`] or whose plan
/// nests deeper than [`MAX_PLAN_DEPTH`]; else the depth of its plan, for [`with_stack`].
pub fn check_query(q: &Query) -> Result<usize> {
    limit(Walk::new(MAX_ALGEBRA_DEPTH, false).query(q))?;
    plan_limit(Walk::new(MAX_PLAN_DEPTH, true).query(q))
}

/// Refuse an update whose algebra nests deeper than [`MAX_ALGEBRA_DEPTH`] or whose plans
/// nest deeper than [`MAX_PLAN_DEPTH`]; else the depth of its plans, for [`with_stack`].
pub fn check_update(u: &Update) -> Result<usize> {
    limit(Walk::new(MAX_ALGEBRA_DEPTH, false).update(u))?;
    plan_limit(Walk::new(MAX_PLAN_DEPTH, true).update(u))
}

/// [`check_query`] for a query the caller gives up when it is refused: a refused query is
/// taken apart without recursion (see [`discard_query`]).
pub fn checked_query(q: Query) -> Result<(Query, usize)> {
    match check_query(&q) {
        Ok(depth) => Ok((q, depth)),
        Err(e) => {
            discard_query(q);
            Err(e)
        }
    }
}

/// [`check_update`] for an update the caller gives up when it is refused, which is taken
/// apart without recursion.
pub fn checked_update(u: Update) -> Result<(Update, usize)> {
    match check_update(&u) {
        Ok(depth) => Ok((u, depth)),
        Err(e) => {
            discard_update(u);
            Err(e)
        }
    }
}

/// Drop a query without recursion. Dropping the algebra recurses once per level, and the
/// walk of [`check_query`] stops at its limit, so a refused query can nest deeper than any
/// stack holds when the count of its text missed how it nests.
pub fn discard_query(q: Query) {
    let mut t = Teardown::default();
    match q {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Describe { pattern, .. } => t.patterns.push(pattern),
        Query::Construct {
            template,
            graph_templates,
            pattern,
            ..
        } => {
            t.patterns.push(pattern);
            t.triples.extend(template);
            t.triples
                .extend(graph_templates.into_iter().flat_map(|g| g.triples));
        }
    }
    t.run();
}

/// Drop an update without recursion, as [`discard_query`] drops a query.
pub fn discard_update(u: Update) {
    let mut t = Teardown::default();
    for op in u.operations {
        if let GraphUpdateOperation::DeleteInsert { pattern, .. } = op {
            t.patterns.push(*pattern);
        }
    }
    t.run();
}

/// The parts of an algebra still to take apart. Each is moved out of its parent before
/// the parent is dropped, so no drop recurses into a child that still has children.
#[derive(Default)]
struct Teardown {
    patterns: Vec<GraphPattern>,
    exprs: Vec<Expression>,
    paths: Vec<PropertyPathExpression>,
    triples: Vec<TriplePattern>,
}

impl Teardown {
    fn run(&mut self) {
        loop {
            if let Some(p) = self.patterns.pop() {
                self.pattern(p);
            } else if let Some(e) = self.exprs.pop() {
                self.expr(e);
            } else if let Some(p) = self.paths.pop() {
                self.path(p);
            } else if let Some(t) = self.triples.pop() {
                self.term(t.subject);
                self.term(t.object);
            } else {
                return;
            }
        }
    }

    fn term(&mut self, t: TermPattern) {
        if let TermPattern::Triple(t) = t {
            self.triples.push(*t);
        }
    }

    fn order(&mut self, o: OrderExpression) {
        match o {
            OrderExpression::Asc(e) | OrderExpression::Desc(e) => self.exprs.push(e),
        }
    }

    fn pattern(&mut self, p: GraphPattern) {
        match p {
            GraphPattern::Bgp { patterns } => self.triples.extend(patterns),
            GraphPattern::Path {
                subject,
                path,
                object,
            } => {
                self.term(subject);
                self.paths.push(path);
                self.term(object);
            }
            GraphPattern::Join { left, right }
            | GraphPattern::Lateral { left, right }
            | GraphPattern::Union { left, right }
            | GraphPattern::Minus { left, right }
            | GraphPattern::SemiJoin { left, right }
            | GraphPattern::AntiJoin { left, right } => {
                self.patterns.push(*left);
                self.patterns.push(*right);
            }
            GraphPattern::LeftJoin {
                left,
                right,
                expression,
            } => {
                self.patterns.push(*left);
                self.patterns.push(*right);
                self.exprs.extend(expression);
            }
            GraphPattern::Filter { expr, inner } => {
                self.exprs.push(expr);
                self.patterns.push(*inner);
            }
            GraphPattern::Extend {
                inner, expression, ..
            }
            | GraphPattern::Assign {
                inner, expression, ..
            }
            | GraphPattern::Unfold {
                inner, expression, ..
            } => {
                self.patterns.push(*inner);
                self.exprs.push(expression);
            }
            GraphPattern::Values { .. } => {}
            GraphPattern::OrderBy { inner, expression } => {
                self.patterns.push(*inner);
                for o in expression {
                    self.order(o);
                }
            }
            GraphPattern::Group {
                inner, aggregates, ..
            } => {
                self.patterns.push(*inner);
                for (_, a) in aggregates {
                    match a {
                        AggregateExpression::CountSolutions { .. } => {}
                        AggregateExpression::FunctionCall { expr, .. } => self.exprs.push(expr),
                        AggregateExpression::Fold {
                            expr, value, order, ..
                        } => {
                            self.exprs.push(expr);
                            self.exprs.extend(value);
                            for o in order {
                                self.order(o);
                            }
                        }
                    }
                }
            }
            GraphPattern::Graph { inner, .. }
            | GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner }
            | GraphPattern::Slice { inner, .. }
            | GraphPattern::Service { inner, .. } => self.patterns.push(*inner),
        }
    }

    fn expr(&mut self, e: Expression) {
        match e {
            Expression::NamedNode(_)
            | Expression::Literal(_)
            | Expression::Variable(_)
            | Expression::Bound(_) => {}
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
            | Expression::Divide(a, b) => {
                self.exprs.push(*a);
                self.exprs.push(*b);
            }
            Expression::UnaryPlus(a) | Expression::UnaryMinus(a) | Expression::Not(a) => {
                self.exprs.push(*a);
            }
            Expression::In(a, list) => {
                self.exprs.push(*a);
                self.exprs.extend(list);
            }
            Expression::If(a, b, c) => {
                self.exprs.push(*a);
                self.exprs.push(*b);
                self.exprs.push(*c);
            }
            Expression::Coalesce(list) | Expression::FunctionCall(_, list) => {
                self.exprs.extend(list);
            }
            Expression::Exists(p) => self.patterns.push(*p),
        }
    }

    fn path(&mut self, p: PropertyPathExpression) {
        use PropertyPathExpression as PP;
        match p {
            PP::NamedNode(_) | PP::NegatedPropertySet(_) => {}
            PP::Reverse(a)
            | PP::ZeroOrMore(a)
            | PP::OneOrMore(a)
            | PP::ZeroOrOne(a)
            | PP::Distinct(a)
            | PP::Multi(a)
            | PP::Shortest(a)
            | PP::Range { path: a, .. } => self.paths.push(*a),
            PP::Sequence(a, b) | PP::Alternative(a, b) => {
                self.paths.push(*a);
                self.paths.push(*b);
            }
        }
    }
}

/// Refuse a plan the planner built that nests deeper than [`MAX_PLAN_DEPTH`]; else its
/// depth, for [`with_stack`]. The count of the parsed query bounds the plan, and this
/// makes the bound true of the plan whatever the count misses.
pub fn check_plan(depth: usize) -> Result<usize> {
    #[cfg(test)]
    if let Some(limit) = probe::PLAN_LIMIT.get() {
        return if depth > limit {
            Err(Error::invalid(format!(
                "query plan nested deeper than {MAX_PLAN_DEPTH} levels; a basic graph \
                 pattern counts two levels per triple pattern"
            )))
        } else {
            Ok(depth)
        };
    }
    plan_limit(depth)
}

/// Knobs for the tests of this module's callers. The text of a query cannot build a plan
/// deeper than its walk allows, so a test lowers the limit [`check_plan`] applies, and
/// reads the depths [`with_stack`] was asked for.
#[cfg(test)]
pub(super) mod probe {
    use std::cell::{Cell, RefCell};

    thread_local! {
        /// The limit [`check_plan`](super::check_plan) applies on this thread, when set.
        pub static PLAN_LIMIT: Cell<Option<usize>> = const { Cell::new(None) };
        /// The depths [`with_stack`](super::with_stack) was called with on this thread.
        pub static STACKS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    }
}

fn limit(depth: usize) -> Result<usize> {
    if depth > MAX_ALGEBRA_DEPTH {
        return Err(Error::invalid(format!(
            "query algebra nested deeper than {MAX_ALGEBRA_DEPTH} levels"
        )));
    }
    Ok(depth)
}

fn plan_limit(depth: usize) -> Result<usize> {
    if depth > MAX_PLAN_DEPTH {
        return Err(Error::invalid(format!(
            "query plan nested deeper than {MAX_PLAN_DEPTH} levels; a basic graph pattern \
             counts two levels per triple pattern"
        )));
    }
    Ok(depth)
}

/// Run `f`, which plans or evaluates a plan `depth` levels deep, with the stack that takes:
/// on this thread's stack when enough of it is left, else on a new stack segment of this
/// thread (`stacker`). A spawned thread (2 MiB by default) or a rayon worker has room for
/// a few hundred levels.
pub fn with_stack<R>(depth: usize, f: impl FnOnce() -> R) -> R {
    #[cfg(test)]
    probe::STACKS.with_borrow_mut(|v| v.push(depth));
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
/// of the node it is given and returns the depth of its deepest descendant. With `bgp`, it
/// counts the levels the planner can build and the executor recurses through: a join and a
/// sort per triple pattern of a basic graph pattern, the unrolled steps of a path range,
/// the join of a VALUES block and the filters placed between joins. The plan of a join's
/// right operand stacks on the plan of its left one, since the planner flattens joins
/// into one join tree. The count is an upper bound, since the plan depends on statistics.
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
                // a group with no triple pattern is still a leaf of the plan, which the
                // planner makes a unit
                let terms = patterns
                    .iter()
                    .map(|t| self.triple(t, n))
                    .max()
                    .unwrap_or(if self.bgp { n } else { d });
                if self.bgp {
                    // a join, and the sort before it, per triple pattern; a blank node
                    // is a variable to the planner
                    terms.saturating_add(patterns.len().saturating_mul(2))
                } else {
                    terms
                }
            }
            GraphPattern::Path {
                subject,
                path,
                object,
            } => {
                let terms = self.term(subject, n).max(self.term(object, n));
                if self.bgp {
                    let levels = d.saturating_add(self.path_levels(path));
                    terms.max(levels.min(self.limit + 1))
                } else {
                    terms.max(self.path(path, n))
                }
            }
            // the planner flattens joins into one join tree, so the plan of the right
            // operand can stack on top of the plan of the left one
            GraphPattern::Join { left, right } | GraphPattern::Lateral { left, right }
                if self.bgp =>
            {
                let l = self.pattern(left, n);
                self.pattern(right, l.max(n))
            }
            GraphPattern::Join { left, right }
            | GraphPattern::Lateral { left, right }
            | GraphPattern::Union { left, right }
            | GraphPattern::Minus { left, right }
            | GraphPattern::SemiJoin { left, right }
            | GraphPattern::AntiJoin { left, right } => {
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
            // the planner puts a filter between joins where its variables become bound, so
            // with `bgp` the plan of the inner pattern stacks on the conjuncts
            GraphPattern::Filter { expr, inner } if self.bgp => {
                self.pattern(inner, self.expr(expr, n))
            }
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
                // the planner joins a VALUES block, with a sort
                .fold(if self.bgp { d.saturating_add(2) } else { d }, usize::max),
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
            PropertyPathExpression::Distinct(a)
            | PropertyPathExpression::Multi(a)
            | PropertyPathExpression::Shortest(a) => self.path(a, n),
            // the planner unrolls up to 32 steps of a range into a chain of joins
            PropertyPathExpression::Range { path, min, .. } => {
                self.path(path, n).saturating_add((*min).min(32) as usize)
            }
        }
    }

    /// The levels of the plan a property path makes, or `limit + 1` once it is over. This
    /// mirrors `Planner::collect_path`: a change there must change this.
    fn path_levels(&self, p: &PropertyPathExpression) -> usize {
        use PropertyPathExpression as PP;
        let over = self.limit + 1;
        let cap = |n: usize| n.min(over);
        match p {
            // a join and a sort, as for a triple pattern
            PP::NamedNode(_) => 2,
            PP::Reverse(a) => self.path_levels(a),
            PP::Sequence(a, b) => cap(self.path_levels(a).saturating_add(self.path_levels(b))),
            // a join, a sort, a union and a branch
            PP::Alternative(a, b) => {
                cap(3usize.saturating_add(self.path_levels(a).max(self.path_levels(b))))
            }
            // a join, a sort, the path operator and the projection of the inner group
            PP::ZeroOrMore(a)
            | PP::OneOrMore(a)
            | PP::ZeroOrOne(a)
            | PP::Distinct(a)
            | PP::Multi(a)
            | PP::Shortest(a) => cap(4usize.saturating_add(self.path_levels(a))),
            // a join, a sort, the projection, a filter and a scan
            PP::NegatedPropertySet(_) => 5,
            PP::Range { path, min, max } => {
                let inner = self.path_levels(path);
                // the first `min` steps of a range are unrolled into joins, and the rest
                // of it runs in the path operator
                if (1..=32).contains(min) {
                    let steps = (*min as usize).saturating_mul(inner);
                    let rest = if *max == Some(*min) {
                        0
                    } else {
                        4usize.saturating_add(inner)
                    };
                    cap(steps.saturating_add(rest))
                } else {
                    cap(4usize.saturating_add(inner))
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_plan_is_checked_against_the_plan_limit() {
        assert_eq!(check_plan(MAX_PLAN_DEPTH).unwrap(), MAX_PLAN_DEPTH);
        let e = check_plan(MAX_PLAN_DEPTH + 1).unwrap_err().to_string();
        assert!(
            e.contains(&format!(
                "query plan nested deeper than {MAX_PLAN_DEPTH} levels"
            )),
            "{e}"
        );
        assert!(e.contains("two levels per triple pattern"), "{e}");
    }
}
