//! Jena's service enhancer: options written at the front of a SERVICE IRI.
//!
//! `SERVICE <loop:bulk+10:cache:https://example.org/sparql> { … }` reads as the options
//! `loop`, `bulk+10` and `cache` followed by the endpoint. The options are
//! `:`-separated keys, each with an optional `+value`, and the first key that is not an
//! option starts the endpoint's IRI. Without an IRI after the options, or with
//! `urn:x-arq:self`, the service is the dataset the query runs on.
//!
//! * `loop` evaluates the SERVICE once per solution of the patterns before it, with that
//!   solution's values substituted into the pattern, as `LATERAL` does. Unlike
//!   `LATERAL`, the substitution also reaches the variables of sub-selects that do not
//!   project them, which is Jena's behaviour.
//! * `bulk` and `bulk+n` send the solutions of a loop to a remote endpoint `n` at a time
//!   in one request (see [`values_request`] and [`union_request`]).
//! * `cache`, `cache+clear` and `cache+off` read and write the store's cache of remote
//!   results ([`super::svccache`]) per input solution.
//!
//! The options are IRIs with a scheme that is neither `http` nor `https`, so no query
//! that could reach an endpoint before means something else now.

use super::ctx::{Charge, Ctx, TermKind};
use super::exec::{join_tables, left_join};
use super::lateral::LateralSpec;
use super::plan::{ActiveGraph, Kind, Node, Planner, collect_pattern_vars};
use super::query_memory::QueryMemoryLease;
use super::svccache::{self, Rows};
use super::table::{Table, VarId};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::outbound::RequestBudget;
use oxrdf::{Literal, NamedNode, Term, Variable};
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};
use std::borrow::Cow;
use std::fmt::Write;
use std::sync::Arc;

/// The IRI of the dataset the query runs on.
pub const SELF_IRI: &str = "urn:x-arq:self";

/// The index of the end marker in a bulk request: higher than any input's index.
pub const END_MARKER: i64 = 1_000_000_000;

/// How a SERVICE uses the cache of remote results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CacheMode {
    /// no `cache` option
    #[default]
    None,
    /// `cache`, `cache+default`: read the cache and write what is fetched
    Default,
    /// `cache+clear`: drop the entries of the request's inputs, fetch and write them
    Clear,
    /// `cache+off`: neither read nor write
    Off,
}

impl CacheMode {
    fn reads(self) -> bool {
        self == CacheMode::Default
    }

    fn writes(self) -> bool {
        matches!(self, CacheMode::Default | CacheMode::Clear)
    }
}

/// The options of a SERVICE.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub looped: bool,
    /// `None` without `bulk`, `Some(None)` for `bulk` and `Some(Some(n))` for `bulk+n`
    pub bulk: Option<Option<usize>>,
    pub cache: CacheMode,
    /// `None` without `parallel`, `Some(None)` for `parallel` and `Some(Some(n))` for
    /// `parallel+n`: the requests of a remote loop in flight at once
    pub parallel: Option<Option<usize>>,
}

/// Where a SERVICE with options runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// the dataset the query runs on (`urn:x-arq:self`)
    SelfDataset,
    Remote(NamedNode),
}

/// A SERVICE as its options make it.
pub struct Effective<'a> {
    pub opts: Options,
    pub target: Target,
    pub inner: &'a GraphPattern,
    pub silent: bool,
}

const KNOWN: [&str; 5] = ["loop", "bulk", "cache", "optimize", "parallel"];

/// The options at the front of `iri` and the rest of it.
pub fn parse_options(iri: &str) -> (Vec<(&str, Option<&str>)>, &str) {
    fn option(seg: &str) -> Option<(&str, Option<&str>)> {
        let (k, v) = match seg.split_once('+') {
            Some((k, v)) => (k, Some(v)),
            None => (seg, None),
        };
        KNOWN.contains(&k).then_some((k, v))
    }
    let mut opts = Vec::new();
    let mut rest = iri;
    loop {
        match rest.find(':') {
            Some(i) => match option(&rest[..i]) {
                Some(o) => {
                    opts.push(o);
                    rest = &rest[i + 1..];
                }
                None => break,
            },
            // the last option may stand without its `:` (`<bulk:loop>`)
            None => {
                if !opts.is_empty()
                    && let Some(o) = option(rest)
                {
                    opts.push(o);
                    rest = "";
                }
                break;
            }
        }
    }
    (opts, rest)
}

/// Add `opts` to `o`; an option already set keeps its first value, as in Jena.
fn apply(
    o: &mut Options,
    opts: &[(&str, Option<&str>)],
    seen: &mut FxHashSet<String>,
) -> Result<()> {
    for (k, v) in opts {
        if !seen.insert((*k).to_string()) {
            continue;
        }
        match *k {
            "loop" => o.looped = true,
            "bulk" => {
                o.bulk = Some(match v {
                    None => None,
                    Some(n) => {
                        Some(n.parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| {
                            Error::invalid(format!(
                                "SERVICE option bulk+{n}: not a positive number"
                            ))
                        })?)
                    }
                })
            }
            "parallel" => {
                o.parallel = Some(match v {
                    None => None,
                    Some(n) => {
                        Some(n.parse::<usize>().ok().filter(|n| *n > 0).ok_or_else(|| {
                            Error::invalid(format!(
                                "SERVICE option parallel+{n}: not a positive number"
                            ))
                        })?)
                    }
                })
            }
            "cache" => {
                o.cache = match v.unwrap_or("default") {
                    "default" => CacheMode::Default,
                    "clear" => CacheMode::Clear,
                    "off" => CacheMode::Off,
                    x => {
                        return Err(Error::invalid(format!(
                            "SERVICE option cache+{x}: use cache, cache+clear or cache+off"
                        )));
                    }
                }
            }
            // Jena's `optimize` has no effect here
            _ => {}
        }
    }
    Ok(())
}

/// The SERVICE `name { inner }` with its options, or `None` for a plain SERVICE (no
/// options, and not the dataset itself).
pub fn effective<'a>(
    name: &NamedNodePattern,
    inner: &'a GraphPattern,
    silent: bool,
) -> Result<Option<Effective<'a>>> {
    let NamedNodePattern::NamedNode(n) = name else {
        return Ok(None);
    };
    let (opts, rest) = parse_options(n.as_str());
    if opts.is_empty() {
        return Ok((rest == SELF_IRI).then(|| Effective {
            opts: Options::default(),
            target: Target::SelfDataset,
            inner,
            silent,
        }));
    }
    let mut o = Options::default();
    let mut seen = FxHashSet::default();
    apply(&mut o, &opts, &mut seen)?;
    if rest.is_empty() {
        // options alone over a SERVICE directly inside: the inner one with both options
        if let GraphPattern::Service {
            name: n2,
            inner: i2,
            silent: s2,
        } = inner
            && let NamedNodePattern::NamedNode(nn) = n2
        {
            let (opts2, rest2) = parse_options(nn.as_str());
            apply(&mut o, &opts2, &mut seen)?;
            let target = if rest2.is_empty() || rest2 == SELF_IRI {
                Target::SelfDataset
            } else {
                Target::Remote(
                    NamedNode::new(rest2)
                        .map_err(|e| Error::invalid(format!("SERVICE <{}>: {e}", nn.as_str())))?,
                )
            };
            return Ok(Some(Effective {
                opts: o,
                target,
                inner: i2,
                silent: *s2,
            }));
        }
        return Ok(Some(Effective {
            opts: o,
            target: Target::SelfDataset,
            inner,
            silent,
        }));
    }
    let target = if rest == SELF_IRI {
        Target::SelfDataset
    } else {
        Target::Remote(
            NamedNode::new(rest)
                .map_err(|e| Error::invalid(format!("SERVICE <{}>: {e}", n.as_str())))?,
        )
    };
    Ok(Some(Effective {
        opts: o,
        target,
        inner,
        silent,
    }))
}

/// Whether `gp` is a SERVICE with the `loop` option.
pub(super) fn is_loop(gp: &GraphPattern) -> bool {
    match gp {
        GraphPattern::Service {
            name,
            inner,
            silent,
        } => matches!(effective(name, inner, *silent), Ok(Some(e)) if e.opts.looped),
        _ => false,
    }
}

// ------------------------------------------------------------ variables ------

fn expr_walk(e: &Expression, f: &mut dyn FnMut(&Variable), p: &mut dyn FnMut(&GraphPattern)) {
    use Expression as E;
    match e {
        E::Variable(v) | E::Bound(v) => f(v),
        E::NamedNode(_) | E::Literal(_) => {}
        E::Or(a, b)
        | E::And(a, b)
        | E::Equal(a, b)
        | E::SameTerm(a, b)
        | E::Greater(a, b)
        | E::GreaterOrEqual(a, b)
        | E::Less(a, b)
        | E::LessOrEqual(a, b)
        | E::Add(a, b)
        | E::Subtract(a, b)
        | E::Multiply(a, b)
        | E::Divide(a, b) => {
            expr_walk(a, f, p);
            expr_walk(b, f, p);
        }
        E::In(a, bs) => {
            expr_walk(a, f, p);
            for b in bs {
                expr_walk(b, f, p);
            }
        }
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expr_walk(a, f, p),
        E::Exists(g) => p(g),
        E::If(a, b, c) => {
            expr_walk(a, f, p);
            expr_walk(b, f, p);
            expr_walk(c, f, p);
        }
        E::Coalesce(es) | E::FunctionCall(_, es) => {
            for a in es {
                expr_walk(a, f, p);
            }
        }
    }
}

fn term_vars(t: &TermPattern, f: &mut dyn FnMut(&Variable)) {
    match t {
        TermPattern::Variable(v) => f(v),
        TermPattern::Triple(tp) => {
            term_vars(&tp.subject, f);
            if let NamedNodePattern::Variable(v) = &tp.predicate {
                f(v);
            }
            term_vars(&tp.object, f);
        }
        _ => {}
    }
}

/// Every variable `gp` mentions, in sub-selects, expressions and `EXISTS` too.
pub(super) fn all_vars(gp: &GraphPattern) -> Vec<String> {
    let mut out = FxHashSet::default();
    walk_all(gp, &mut |v| {
        out.insert(v.as_str().to_string());
    });
    let mut v: Vec<String> = out.into_iter().collect();
    v.sort_unstable();
    v
}

fn walk_all(gp: &GraphPattern, f: &mut dyn FnMut(&Variable)) {
    use GraphPattern as GP;
    let exprs = |e: &Expression, f: &mut dyn FnMut(&Variable)| {
        let mut pats = Vec::new();
        expr_walk(e, f, &mut |g| pats.push(g.clone()));
        for g in &pats {
            walk_all(g, f);
        }
    };
    match gp {
        GP::Bgp { patterns } => {
            for t in patterns {
                term_vars(&t.subject, f);
                if let NamedNodePattern::Variable(v) = &t.predicate {
                    f(v);
                }
                term_vars(&t.object, f);
            }
        }
        GP::Path {
            subject, object, ..
        } => {
            term_vars(subject, f);
            term_vars(object, f);
        }
        GP::Join { left, right }
        | GP::Lateral { left, right }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right } => {
            walk_all(left, f);
            walk_all(right, f);
        }
        GP::LeftJoin {
            left,
            right,
            expression,
        } => {
            walk_all(left, f);
            walk_all(right, f);
            if let Some(e) = expression {
                exprs(e, f);
            }
        }
        GP::Filter { expr, inner } => {
            exprs(expr, f);
            walk_all(inner, f);
        }
        GP::Graph { name, inner } | GP::Service { name, inner, .. } => {
            if let NamedNodePattern::Variable(v) = name {
                f(v);
            }
            walk_all(inner, f);
        }
        GP::Extend {
            inner,
            variable,
            expression,
        }
        | GP::Assign {
            inner,
            variable,
            expression,
        } => {
            f(variable);
            exprs(expression, f);
            walk_all(inner, f);
        }
        GP::Unfold {
            inner,
            expression,
            variable,
            second,
        } => {
            f(variable);
            if let Some(s) = second {
                f(s);
            }
            exprs(expression, f);
            walk_all(inner, f);
        }
        GP::Values { variables, .. } => variables.iter().for_each(&mut *f),
        GP::OrderBy { inner, expression } => {
            for o in expression {
                let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = o;
                exprs(e, f);
            }
            walk_all(inner, f);
        }
        GP::Project { inner, variables } => {
            variables.iter().for_each(&mut *f);
            walk_all(inner, f);
        }
        GP::Distinct { inner } | GP::Reduced { inner } | GP::Slice { inner, .. } => {
            walk_all(inner, f)
        }
        GP::Group {
            inner,
            variables,
            aggregates,
        } => {
            variables.iter().for_each(&mut *f);
            for (v, a) in aggregates {
                f(v);
                match a {
                    AggregateExpression::CountSolutions { .. } => {}
                    AggregateExpression::FunctionCall { expr, .. } => exprs(expr, f),
                    AggregateExpression::Fold {
                        expr, value, order, ..
                    } => {
                        exprs(expr, f);
                        if let Some(v) = value {
                            exprs(v, f);
                        }
                        for o in order {
                            let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = o;
                            exprs(e, f);
                        }
                    }
                }
            }
            walk_all(inner, f);
        }
    }
}

/// The variables `gp` assigns (`BIND`, `LET`, `UNFOLD`, `VALUES`, aggregates): they
/// are never substituted, since a value cannot stand where a variable is assigned.
fn assigned_vars(gp: &GraphPattern, out: &mut FxHashSet<String>) {
    use GraphPattern as GP;
    let sub = |g: &GraphPattern, out: &mut FxHashSet<String>| assigned_vars(g, out);
    match gp {
        GP::Bgp { .. } | GP::Path { .. } => {}
        GP::Join { left, right }
        | GP::Lateral { left, right }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right }
        | GP::LeftJoin { left, right, .. } => {
            sub(left, out);
            sub(right, out);
        }
        GP::Extend {
            inner, variable, ..
        }
        | GP::Assign {
            inner, variable, ..
        } => {
            out.insert(variable.as_str().to_string());
            sub(inner, out);
        }
        GP::Unfold {
            inner,
            variable,
            second,
            ..
        } => {
            out.insert(variable.as_str().to_string());
            if let Some(s) = second {
                out.insert(s.as_str().to_string());
            }
            sub(inner, out);
        }
        GP::Values { variables, .. } => {
            out.extend(variables.iter().map(|v| v.as_str().to_string()));
        }
        GP::Group {
            inner, aggregates, ..
        } => {
            out.extend(aggregates.iter().map(|(v, _)| v.as_str().to_string()));
            sub(inner, out);
        }
        GP::Filter { inner, .. }
        | GP::Graph { inner, .. }
        | GP::Service { inner, .. }
        | GP::OrderBy { inner, .. }
        | GP::Project { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. } => sub(inner, out),
    }
}

// --------------------------------------------------------- substitution ------

/// Values to put in place of variables in a SERVICE pattern.
#[derive(Default)]
pub(super) struct Subst {
    /// IRIs and literals by variable name
    pub map: FxHashMap<String, Term>,
    /// the variables of `map` that a sub-select hides unless it projects them (the
    /// substitutions of `LATERAL`; a loop's substitute everywhere, as in Jena)
    pub hidden: FxHashSet<String>,
}

impl Subst {
    /// Add `v = t` if `t` can stand in a query (an IRI or a literal).
    pub fn add(&mut self, v: &str, t: Term) {
        if matches!(t, Term::NamedNode(_) | Term::Literal(_)) {
            self.map.insert(v.to_string(), t);
        }
    }
}

/// `gp` with the values of `s` in place of its variables. A variable that `gp` assigns
/// is not replaced, nor is one in the subject or predicate position of a triple pattern
/// whose value is a literal (such a pattern cannot match, and the join with the input
/// still compares the value). The variables of `GROUP BY` keep their name and get
/// their value by a `BIND` under the grouping.
pub(super) fn substitute(gp: &GraphPattern, s: &Subst) -> GraphPattern {
    let mut assigned = FxHashSet::default();
    assigned_vars(gp, &mut assigned);
    let map: FxHashMap<String, Term> = s
        .map
        .iter()
        .filter(|(k, _)| !assigned.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    sub_gp(gp, &map, &s.hidden)
}

fn term_expr(t: &Term) -> Expression {
    match t {
        Term::NamedNode(n) => Expression::NamedNode(n.clone()),
        Term::Literal(l) => Expression::Literal(l.clone()),
        _ => unreachable!("only IRIs and literals are substituted"),
    }
}

fn sub_term(t: &TermPattern, m: &FxHashMap<String, Term>, object: bool) -> TermPattern {
    match t {
        TermPattern::Variable(v) => match m.get(v.as_str()) {
            Some(Term::NamedNode(n)) => TermPattern::NamedNode(n.clone()),
            Some(Term::Literal(l)) if object => TermPattern::Literal(l.clone()),
            _ => t.clone(),
        },
        TermPattern::Triple(tp) => TermPattern::Triple(Box::new(sub_triple(tp, m))),
        _ => t.clone(),
    }
}

fn sub_named(n: &NamedNodePattern, m: &FxHashMap<String, Term>) -> NamedNodePattern {
    match n {
        NamedNodePattern::Variable(v) => match m.get(v.as_str()) {
            Some(Term::NamedNode(nn)) => NamedNodePattern::NamedNode(nn.clone()),
            _ => n.clone(),
        },
        n => n.clone(),
    }
}

fn sub_triple(t: &TriplePattern, m: &FxHashMap<String, Term>) -> TriplePattern {
    TriplePattern {
        subject: sub_term(&t.subject, m, false),
        predicate: sub_named(&t.predicate, m),
        object: sub_term(&t.object, m, true),
    }
}

fn sub_expr(e: &Expression, m: &FxHashMap<String, Term>, h: &FxHashSet<String>) -> Expression {
    use Expression as E;
    let b = |x: &Expression| Box::new(sub_expr(x, m, h));
    match e {
        E::Variable(v) => m.get(v.as_str()).map_or_else(|| e.clone(), term_expr),
        E::Bound(v) if m.contains_key(v.as_str()) => E::Literal(Literal::from(true)),
        E::Bound(_) | E::NamedNode(_) | E::Literal(_) => e.clone(),
        E::Or(x, y) => E::Or(b(x), b(y)),
        E::And(x, y) => E::And(b(x), b(y)),
        E::Equal(x, y) => E::Equal(b(x), b(y)),
        E::SameTerm(x, y) => E::SameTerm(b(x), b(y)),
        E::Greater(x, y) => E::Greater(b(x), b(y)),
        E::GreaterOrEqual(x, y) => E::GreaterOrEqual(b(x), b(y)),
        E::Less(x, y) => E::Less(b(x), b(y)),
        E::LessOrEqual(x, y) => E::LessOrEqual(b(x), b(y)),
        E::Add(x, y) => E::Add(b(x), b(y)),
        E::Subtract(x, y) => E::Subtract(b(x), b(y)),
        E::Multiply(x, y) => E::Multiply(b(x), b(y)),
        E::Divide(x, y) => E::Divide(b(x), b(y)),
        E::In(x, ys) => E::In(b(x), ys.iter().map(|y| sub_expr(y, m, h)).collect()),
        E::UnaryPlus(x) => E::UnaryPlus(b(x)),
        E::UnaryMinus(x) => E::UnaryMinus(b(x)),
        E::Not(x) => E::Not(b(x)),
        E::Exists(g) => E::Exists(Box::new(sub_gp(g, m, h))),
        E::If(x, y, z) => E::If(b(x), b(y), b(z)),
        E::Coalesce(xs) => E::Coalesce(xs.iter().map(|x| sub_expr(x, m, h)).collect()),
        E::FunctionCall(f, xs) => {
            E::FunctionCall(f.clone(), xs.iter().map(|x| sub_expr(x, m, h)).collect())
        }
    }
}

fn sub_order(
    o: &OrderExpression,
    m: &FxHashMap<String, Term>,
    h: &FxHashSet<String>,
) -> OrderExpression {
    match o {
        OrderExpression::Asc(e) => OrderExpression::Asc(sub_expr(e, m, h)),
        OrderExpression::Desc(e) => OrderExpression::Desc(sub_expr(e, m, h)),
    }
}

fn sub_gp(gp: &GraphPattern, m: &FxHashMap<String, Term>, h: &FxHashSet<String>) -> GraphPattern {
    use GraphPattern as GP;
    if m.is_empty() {
        return gp.clone();
    }
    let b = |x: &GraphPattern| Box::new(sub_gp(x, m, h));
    let ex = |x: &Expression| sub_expr(x, m, h);
    match gp {
        GP::Bgp { patterns } => GP::Bgp {
            patterns: patterns.iter().map(|t| sub_triple(t, m)).collect(),
        },
        GP::Path {
            subject,
            path,
            object,
        } => GP::Path {
            subject: sub_term(subject, m, false),
            path: path.clone(),
            object: sub_term(object, m, true),
        },
        GP::Join { left, right } => GP::Join {
            left: b(left),
            right: b(right),
        },
        GP::Lateral { left, right } => GP::Lateral {
            left: b(left),
            right: b(right),
        },
        GP::Union { left, right } => GP::Union {
            left: b(left),
            right: b(right),
        },
        GP::Minus { left, right } => GP::Minus {
            left: b(left),
            right: b(right),
        },
        GP::SemiJoin { left, right } => GP::SemiJoin {
            left: b(left),
            right: b(right),
        },
        GP::AntiJoin { left, right } => GP::AntiJoin {
            left: b(left),
            right: b(right),
        },
        GP::LeftJoin {
            left,
            right,
            expression,
        } => GP::LeftJoin {
            left: b(left),
            right: b(right),
            expression: expression.as_ref().map(ex),
        },
        GP::Filter { expr, inner } => GP::Filter {
            expr: ex(expr),
            inner: b(inner),
        },
        GP::Graph { name, inner } => GP::Graph {
            name: sub_named(name, m),
            inner: b(inner),
        },
        GP::Service {
            name,
            inner,
            silent,
        } => GP::Service {
            name: sub_named(name, m),
            inner: b(inner),
            silent: *silent,
        },
        GP::Extend {
            inner,
            variable,
            expression,
        } => GP::Extend {
            inner: b(inner),
            variable: variable.clone(),
            expression: ex(expression),
        },
        GP::Assign {
            inner,
            variable,
            expression,
        } => GP::Assign {
            inner: b(inner),
            variable: variable.clone(),
            expression: ex(expression),
        },
        GP::Unfold {
            inner,
            expression,
            variable,
            second,
        } => GP::Unfold {
            inner: b(inner),
            expression: ex(expression),
            variable: variable.clone(),
            second: second.clone(),
        },
        GP::Values { .. } => gp.clone(),
        GP::OrderBy { inner, expression } => GP::OrderBy {
            inner: b(inner),
            expression: expression.iter().map(|o| sub_order(o, m, h)).collect(),
        },
        GP::Project { inner, variables } => {
            // a sub-select hides the scoped variables it does not project
            let m2: Cow<'_, FxHashMap<String, Term>> = if h.is_empty() {
                Cow::Borrowed(m)
            } else {
                Cow::Owned(
                    m.iter()
                        .filter(|(k, _)| {
                            !h.contains(*k) || variables.iter().any(|v| v.as_str() == *k)
                        })
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )
            };
            GP::Project {
                inner: Box::new(sub_gp(inner, &m2, h)),
                variables: variables.clone(),
            }
        }
        GP::Distinct { inner } => GP::Distinct { inner: b(inner) },
        GP::Reduced { inner } => GP::Reduced { inner: b(inner) },
        GP::Slice {
            inner,
            start,
            length,
        } => GP::Slice {
            inner: b(inner),
            start: *start,
            length: *length,
        },
        GP::Group {
            inner,
            variables,
            aggregates,
        } => {
            let mut i = sub_gp(inner, m, h);
            for v in variables {
                if let Some(t) = m.get(v.as_str()) {
                    i = GP::Extend {
                        inner: Box::new(i),
                        variable: v.clone(),
                        expression: term_expr(t),
                    };
                }
            }
            GP::Group {
                inner: Box::new(i),
                variables: variables.clone(),
                aggregates: aggregates
                    .iter()
                    .map(|(v, a)| {
                        let a = match a {
                            AggregateExpression::CountSolutions { .. } => a.clone(),
                            AggregateExpression::FunctionCall {
                                name,
                                expr,
                                distinct,
                            } => AggregateExpression::FunctionCall {
                                name: name.clone(),
                                expr: ex(expr),
                                distinct: *distinct,
                            },
                            AggregateExpression::Fold {
                                expr,
                                value,
                                distinct,
                                order,
                            } => AggregateExpression::Fold {
                                expr: ex(expr),
                                value: value.as_ref().map(ex),
                                distinct: *distinct,
                                order: order.iter().map(|o| sub_order(o, m, h)).collect(),
                            },
                        };
                        (v.clone(), a)
                    })
                    .collect(),
            }
        }
    }
}

// ------------------------------------------------------------- requests ------

/// `gp` without its outermost slice, and that slice, which the cache of remote results
/// keeps as the bound of an entry rather than in its key (roadmap item 38).
pub(super) fn outer_slice(gp: &GraphPattern) -> (&GraphPattern, svccache::Slice) {
    match gp {
        GraphPattern::Slice {
            inner,
            start,
            length,
        } => (
            inner,
            svccache::Slice {
                offset: *start,
                limit: *length,
            },
        ),
        _ => (gp, svccache::Slice::ALL),
    }
}

/// The query sent for one input: `SELECT * WHERE { R }`, with the input's values in `R`.
pub(super) fn single_request(inner: &GraphPattern) -> String {
    format!("SELECT * WHERE {{ {inner} }}")
}

/// A variable named after `base` that `gp` does not mention.
fn fresh_var(gp: &GraphPattern, base: &str) -> String {
    let used: FxHashSet<String> = all_vars(gp).into_iter().collect();
    let mut name = base.to_string();
    let mut i = 1;
    while used.contains(&name) {
        name = format!("{base}{i}");
        i += 1;
    }
    name
}

fn ground(t: &Option<Term>) -> Option<GroundTerm> {
    match t {
        Some(Term::NamedNode(n)) => Some(GroundTerm::NamedNode(n.clone())),
        Some(Term::Literal(l)) => Some(GroundTerm::Literal(l.clone())),
        _ => None,
    }
}

/// The bulk request of a pattern whose solutions with an input's values are its
/// solutions that agree with them: one VALUES block with the inputs, numbered by
/// `?idx`, joined with the pattern, and an end marker.
///
/// ```sparql
/// SELECT * WHERE {
///   { VALUES (?k ?idx) { (<a> 0) (<b> 1) } { R } }
///   UNION { BIND(1000000000 AS ?idx) }
/// } ORDER BY ?idx
/// ```
///
/// The marker sorts last, so a response that an endpoint cut short lacks it.
pub(super) fn values_request(
    inner: &GraphPattern,
    keys: &[String],
    tuples: &[&[Option<Term>]],
    idx: &str,
) -> String {
    let mut variables: Vec<Variable> = keys.iter().map(Variable::new_unchecked).collect();
    variables.push(Variable::new_unchecked(idx));
    let bindings = tuples
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut row: Vec<Option<GroundTerm>> = t.iter().map(ground).collect();
            row.push(Some(GroundTerm::Literal(Literal::from(i as i64))));
            row
        })
        .collect();
    let values = GraphPattern::Values {
        variables,
        bindings,
    };
    format!(
        "SELECT * WHERE {{ {{ {values} {{ {inner} }} }} UNION {{ BIND({END_MARKER} AS ?{idx}) }} }} ORDER BY ?{idx}"
    )
}

/// The bulk request of any other pattern, as Jena's service enhancer writes it: the
/// pattern with each input's values substituted, numbered by `?idx`, in a UNION with
/// an end marker.
///
/// ```sparql
/// SELECT * WHERE {
///   { { R[μ0] } BIND(0 AS ?idx) } UNION { { R[μ1] } BIND(1 AS ?idx) }
///   UNION { BIND(1000000000 AS ?idx) }
/// } ORDER BY ?idx
/// ```
pub(super) fn union_request(members: &[GraphPattern], idx: &str) -> String {
    let mut s = String::from("SELECT * WHERE { ");
    for (i, m) in members.iter().enumerate() {
        let _ = write!(s, "{{ {{ {m} }} BIND({i} AS ?{idx}) }} UNION ");
    }
    let _ = write!(s, "{{ BIND({END_MARKER} AS ?{idx}) }} }} ORDER BY ?{idx}");
    s
}

/// Whether the VALUES form of a bulk request gives each input the solutions that
/// substitution would: the pattern is built from basic graph patterns, paths that
/// cannot match a zero-length path, joins, `GRAPH` and deterministic FILTERs, and no
/// key is used by a FILTER over a pattern that may leave it unbound (the analysis of
/// `LATERAL`'s join test).
fn values_form(ctx: &Ctx, inner: &GraphPattern, keys: &[VarId]) -> bool {
    let mut risky = FxHashSet::default();
    match super::exists::certain(ctx, inner, &mut risky) {
        Ok(_) => keys.iter().all(|k| !risky.contains(k)),
        Err(_) => false,
    }
}

// -------------------------------------------------------------- planning ------

/// A loop over a remote endpoint, run by the `Lateral` operator.
#[derive(Clone, Debug)]
pub struct RemoteLoop {
    pub endpoint: NamedNode,
    pub silent: bool,
    /// inputs per request
    pub bulk: usize,
    pub cache: CacheMode,
    /// whether bulk requests take the VALUES form
    pub values: bool,
    /// the name of the input index in bulk requests
    pub idx: String,
    /// the requests in flight at once (`parallel+n`)
    pub parallel: usize,
}

/// The plan of `left` joined (or, with `optional`, left-joined) with the SERVICE
/// `right`, which has the `loop` option.
pub(super) fn plan_loop(
    p: &mut Planner<'_>,
    left: Node,
    right: &GraphPattern,
    g: &ActiveGraph,
    optional: Option<Option<Expression>>,
) -> Result<Node> {
    let GraphPattern::Service {
        name,
        inner,
        silent,
    } = right
    else {
        unreachable!("a loop is a SERVICE");
    };
    let Some(e) = effective(name, inner, *silent)? else {
        unreachable!("a loop has options");
    };
    let Target::Remote(endpoint) = e.target else {
        // the dataset itself: LATERAL's operator, substituting through sub-selects
        return super::lateral::plan_loop_self(p, left, e.inner, optional);
    };
    let ctx = p.ctx;
    let keys: Vec<VarId> = all_vars(e.inner)
        .iter()
        .map(|n| ctx.var(n))
        .filter(|v| left.vars.contains(v))
        .collect();
    let mut names = Vec::new();
    collect_pattern_vars(e.inner, &mut names);
    let mut vars = left.vars.clone();
    for n in names {
        let v = ctx.var(&n);
        if !vars.contains(&v) {
            vars.push(v);
        }
    }
    let policy = &ctx.outbound;
    let bulk = match e.opts.bulk {
        None => 1,
        Some(None) => policy.service_bulk_size,
        Some(Some(n)) => n,
    }
    .clamp(1, policy.service_bulk_max.max(1));
    let parallel = match e.opts.parallel {
        None => 1,
        Some(None) => policy.service_parallel_max,
        Some(Some(n)) => n,
    }
    .clamp(1, policy.service_parallel_max.max(1));
    let values = values_form(ctx, e.inner, &keys);
    let idx = fresh_var(e.inner, "__idx__");
    let est = left.est.max(1.0);
    let requests = (est / bulk as f64).ceil().max(1.0) / parallel as f64;
    let cost = left.cost + requests * 100_000.0 + est;
    let dist = vars
        .iter()
        .map(|v| (*v, left.dist.get(v).copied().unwrap_or(est).min(est)))
        .collect();
    let desc = format!(
        "SERVICE <{}> per binding on {}{}{}",
        endpoint.as_str(),
        keys.iter()
            .map(|k| format!("?{}", ctx.var_name(*k)))
            .collect::<Vec<_>>()
            .join(" "),
        if bulk > 1 {
            format!(
                ", {bulk} per request as {}",
                if values { "VALUES" } else { "UNION" }
            )
        } else {
            String::new()
        },
        match e.opts.cache {
            CacheMode::Default => ", cached",
            CacheMode::Clear => ", cache cleared",
            _ => "",
        }
    ) + &if parallel > 1 {
        format!(", {parallel} requests at a time")
    } else {
        String::new()
    };
    let mut outer: Vec<(VarId, Id)> = p.subst.iter().map(|(v, id)| (*v, *id)).collect();
    outer.sort_unstable_by_key(|(v, _)| *v);
    let mut outer_scoped: Vec<VarId> = p.scoped.iter().copied().collect();
    outer_scoped.sort_unstable();
    let certain = left.certain.clone();
    let spec = LateralSpec {
        pattern: e.inner.clone(),
        graph: g.clone(),
        keys,
        outer,
        outer_scoped,
        optional,
        unscoped: true,
        service: Some(Box::new(RemoteLoop {
            endpoint,
            silent: e.silent,
            bulk,
            cache: e.opts.cache,
            values,
            idx,
            parallel,
        })),
    };
    Ok(Node {
        kind: Kind::Lateral(Box::new(spec)),
        children: vec![left],
        vars,
        certain,
        sorted: Vec::new(),
        est,
        cost,
        dist,
        desc,
    })
}

/// The substitutions of the planner's constants (initial bindings, the row of an
/// enclosing `EXISTS` or `LATERAL`) into a SERVICE pattern, together with the decode
/// credit that must stay alive until the returned map is dropped.
pub(super) fn planner_subst<'a>(
    p: &Planner<'a>,
    inner: &GraphPattern,
) -> Result<(Subst, Charge<'a>)> {
    let decode = p.ctx.charge(0)?;
    let mut s = Subst::default();
    if p.subst.is_empty() {
        return Ok((s, decode));
    }
    for n in all_vars(inner) {
        let v = p.ctx.var(&n);
        if let Some(id) = p.subst.get(&v)
            && let Some(t) = p.ctx.term_checked_into(*id, &decode)?
        {
            if p.scoped.contains(&v) {
                s.hidden.insert(n.clone());
            }
            s.add(&n, t);
        }
    }
    Ok((s, decode))
}

// ------------------------------------------------------------- execution ------

/// What a remote loop did, for EXPLAIN.
#[derive(Default)]
pub(super) struct LoopStats {
    pub inputs: usize,
    pub requests: usize,
    pub cache_hits: usize,
    /// bulk requests the endpoint cut short, sent again one input at a time
    pub retried: usize,
    pub solutions: usize,
}

impl LoopStats {
    /// The note of the loop's plan node.
    pub(super) fn note(&self) -> String {
        format!(
            "[{} inputs, {} requests, {} from the cache{}]",
            self.inputs,
            self.requests,
            self.cache_hits,
            if self.retried > 0 {
                format!(", {} bulk requests cut short and sent again", self.retried)
            } else {
                String::new()
            }
        )
    }

    /// The counters of the loop's plan node.
    pub(super) fn counters(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut c = serde_json::Map::new();
        c.insert("serviceInputs".into(), self.inputs.into());
        c.insert("serviceRequests".into(), self.requests.into());
        c.insert("serviceCacheHits".into(), self.cache_hits.into());
        c.insert("serviceRetried".into(), self.retried.into());
        c.insert("serviceSolutions".into(), self.solutions.into());
        c
    }
}

/// Fail unless the request may call remote services.
pub(super) fn check_allowed(ctx: &Ctx) -> Result<()> {
    if !ctx.allow_service {
        return Err(Error::Service("SERVICE is disabled".into()));
    }
    if ctx.forbid_service {
        return Err(Error::NotPermitted(
            "SERVICE requires the federate permission".into(),
        ));
    }
    Ok(())
}

/// The solutions of one response, as terms by variable name, with each solution
/// admitted on `charge` before it is kept, for the eager SERVICE without `loop`, which
/// holds the rows on `charge` until it has made its table of them.
pub(super) fn fetch_rows_charged(
    ctx: &Ctx,
    url: &NamedNode,
    query: &str,
    charge: &Charge<'_>,
) -> Result<Rows> {
    fetch_rows_within(ctx, url, query, None, Some(charge))
}

/// [`fetch_rows_charged`] drawing on `share` in place of the request's outbound budget,
/// with what each solution adds admitted on `charge` before it is copied: the variable
/// names with the first one, the row with its terms, and the growth of the rows'
/// vector. The query's cancellation and deadline are polled before each copy.
fn fetch_rows_within(
    ctx: &Ctx,
    url: &NamedNode,
    query: &str,
    share: Option<&Arc<RequestBudget>>,
    charge: Option<&Charge<'_>>,
) -> Result<Rows> {
    let mut vars: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Option<Term>>> = Vec::new();
    let admit = |n: u64| charge.map_or(Ok(()), |c| c.add(n));
    super::exec::fetch_within(ctx, url, query, share, &mut |sol| {
        ctx.check()?;
        if vars.is_empty() && sol.variables().len() != 0 {
            let names = sol.variables().len() * size_of::<String>()
                + sol.variables().map(str::len).sum::<usize>();
            admit(names as u64)?;
            vars = sol.variables().map(str::to_owned).collect();
        }
        let terms = vars.iter().filter_map(|v| sol.get(v.as_str()));
        let mut bytes = Rows::retained_slots_bytes(vars.len(), terms);
        if rows.len() == rows.capacity() {
            let grown = rows.capacity().max(4);
            bytes = bytes.saturating_add((grown * size_of::<Vec<Option<Term>>>()) as u64);
            admit(bytes)?;
            rows.reserve_exact(grown);
        } else {
            admit(bytes)?;
        }
        rows.push(
            vars.iter()
                .map(|v| sol.get(v.as_str()).cloned())
                .collect::<Vec<_>>(),
        );
        ctx.check()
    })?;
    Ok(Rows { vars, rows })
}

/// One answer of [`fetch_all`]: the rows, with the charge that admitted them, which the
/// caller holds until it has admitted them itself.
type Fetched<'c> = Result<(Rows, Charge<'c>)>;

/// The answers to `queries`, sent to `url` at once, in the order of `queries`. Each
/// request reserves an equal share of the time the request's outbound budget has left
/// before any is sent ([`RequestBudget::share`]), so the requests in flight together,
/// and any other request of the query that draws on the budget meanwhile, cannot take
/// more than the budget, which counts every request's own time. A request whose share
/// cannot be reserved is not sent and fails. The budget's bytes are counted as
/// responses arrive, so they bound the requests in flight together as they bound one.
/// Each solution is admitted on its request's charge before it is kept, and every
/// solution checks for cancellation and the deadline, as does every dispatch. Each
/// request runs in the caller's span.
///
/// The time a wave cannot reserve is the outbound budget's, spent: when the time left
/// does not give every request of the wave a share, or when a request fails once its
/// share's time is spent, `fetch_all` fails as a whole, so SILENT does not hide it and
/// no later wave goes out, as one request out of the budget's time fails the query.
/// A wave of one request draws on the budget directly, as every other outbound request
/// of the query does.
fn fetch_all<'c>(ctx: &'c Ctx, url: &NamedNode, queries: &[String]) -> Result<Vec<Fetched<'c>>> {
    let one = |q: &str, share: Option<&Arc<RequestBudget>>| -> Fetched<'c> {
        // the last poll before the request is built and sent
        ctx.check()?;
        let charge = ctx.charge(0)?;
        let rows = fetch_rows_within(ctx, url, q, share, Some(&charge))?;
        Ok((rows, charge))
    };
    if queries.len() <= 1 {
        return Ok(queries.iter().map(|q| one(q, None)).collect());
    }
    let spent = || {
        Error::Service(format!(
            "<{}>: the outbound requests of this request have no time left",
            url.as_str()
        ))
    };
    let n = u32::try_from(queries.len()).unwrap_or(u32::MAX);
    let each = ctx.outbound_budget.remaining_time() / n;
    if each.is_zero() {
        return Err(spent());
    }
    let mut shares = Vec::with_capacity(queries.len());
    for _ in queries {
        ctx.check()?;
        shares.push(ctx.outbound_budget.share(each).ok_or_else(spent)?);
    }
    let span = tracing::Span::current();
    let answers: Vec<(Fetched<'c>, bool)> = std::thread::scope(|scope| {
        let handles: Vec<_> = queries
            .iter()
            .zip(shares)
            .map(|(q, share)| {
                let span = span.clone();
                let one = &one;
                scope.spawn(move || -> (Fetched<'c>, bool) {
                    let _entered = span.enter();
                    let answer = one(q, Some(&share));
                    // a failure once the share's time is spent is the budget's
                    let out = answer.is_err() && share.remaining_time().is_zero();
                    (answer, out)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    let mut out = Vec::with_capacity(answers.len());
    for (answer, spent_share) in answers {
        if spent_share {
            return Err(answer.err().unwrap_or_else(spent));
        }
        out.push(answer);
    }
    Ok(out)
}

/// The rows of one input, as a table of `vars`, leaving out the variables in `skip`
/// (substituted ones, whose value the input has).
fn to_table(ctx: &Ctx, rows: &Rows, vars: &[VarId], skip: &FxHashSet<VarId>) -> Table {
    let tvars: Vec<VarId> = vars.iter().copied().filter(|v| !skip.contains(v)).collect();
    let cols: Vec<Option<usize>> = tvars
        .iter()
        .map(|v| {
            let n = ctx.var_name(*v);
            rows.vars.iter().position(|x| *x == n)
        })
        .collect();
    let mut t = Table::new(tvars);
    let mut bnodes = FxHashMap::default();
    let mut row = vec![Id::UNDEF; cols.len()];
    for r in &rows.rows {
        for (o, c) in row.iter_mut().zip(&cols) {
            *o = c
                .and_then(|c| r[c].as_ref())
                .map_or(Id::UNDEF, |term| ctx.intern_remote_term(term, &mut bnodes));
        }
        t.push_row(&row);
    }
    t
}

/// One distinct input of a loop: the values substituted for its keys.
struct Input {
    /// by key, `None` where the key is unbound or its value cannot be sent (a blank
    /// node)
    values: Vec<Option<Term>>,
    key: Option<String>,
    rows: Option<Arc<Rows>>,
}

/// Run a loop over a remote endpoint for the left side's rows `l`.
pub(super) fn run_remote(
    ctx: &Ctx,
    spec: &LateralSpec,
    rl: &RemoteLoop,
    l: &Table,
    vars: &[VarId],
) -> Result<(Table, LoopStats)> {
    let mut run = RemoteRun::new(ctx, spec, rl)?;
    run.add(ctx, spec, rl, l)?;
    // The eager loop admits its answers as they arrive and holds them on one charge
    // until it has joined them.
    let held = ctx.charge(0)?;
    run.send(ctx, spec, rl, true, &mut |bytes| held.resize(bytes))?;
    let t = run.join(ctx, spec, l, vars)?;
    drop(held);
    Ok((t, run.stats))
}

/// A loop over a remote endpoint that takes its left rows in parts. It keeps the
/// distinct inputs it has seen with their answers, and sends a bulk request only once
/// it holds a full request of unanswered inputs or the left side has ended, so its
/// requests are the ones one call over all of the rows sends.
pub(super) struct RemoteRun {
    names: Vec<String>,
    /// the constants in force where the loop was planned
    outer: Subst,
    pattern: GraphPattern,
    /// the pattern without its outermost slice, which the cache keys
    text: String,
    /// the outermost slice of the pattern, which bounds the cache's entries
    slice: svccache::Slice,
    /// the parts of every input's cache key before its values
    parts: svccache::Parts,
    caching: bool,
    inputs: Vec<Input>,
    by_values: FxHashMap<Vec<Option<Term>>, usize>,
    /// the input of each distinct key of the left rows, and whether its solutions have
    /// been counted
    by_key: FxHashMap<Vec<Id>, (usize, bool)>,
    /// the inputs that the cache did not have and no request has answered, in the order
    /// they first appeared
    missing: Vec<usize>,
    /// the bytes of the answers the inputs hold, summed as they arrive
    held: u64,
    pub stats: LoopStats,
    /// retained credit for decoded outer constants and substituted pattern, dropped after
    /// `outer` and `pattern`
    _outer_lease: Option<QueryMemoryLease>,
    /// retained credit for distinct keys, inputs and cache keys, reconciled continuously
    /// from each row's decode charge and dropped after `inputs`, `by_values` and `by_key`
    kept_lease: Option<QueryMemoryLease>,
}

/// The bytes one distinct key of `width` identifiers holds in `by_key`: the key, its
/// vector header and value, and the hash table's slot and control byte.
fn key_bytes(width: usize) -> u64 {
    (width * size_of::<Id>() + size_of::<(Vec<Id>, (usize, bool))>() + 1) as u64
}

/// The bytes one distinct input holds: its values in `inputs` and again as the key of
/// `by_values`, the slot of each, its place in `missing` and its cache key.
fn input_bytes(input: &Input) -> u64 {
    let values = input.values.len() * size_of::<Option<Term>>();
    let terms: u64 = input
        .values
        .iter()
        .flatten()
        .map(|t| super::output::term_bytes(t).unwrap_or(u64::MAX / 4))
        .sum();
    let slots = size_of::<Input>() + size_of::<(Vec<Option<Term>>, usize)>() + 1;
    let cache_key = input.key.as_ref().map_or(0, String::capacity);
    (2 * values + slots + size_of::<usize>() + cache_key) as u64 + 2 * terms
}

impl RemoteRun {
    pub(super) fn new(ctx: &Ctx, spec: &LateralSpec, rl: &RemoteLoop) -> Result<Self> {
        check_allowed(ctx)?;
        let names: Vec<String> = spec.keys.iter().map(|k| ctx.var_name(*k)).collect();
        let decode = ctx.charge(0)?;
        let mut outer = Subst::default();
        for (v, id) in &spec.outer {
            if spec.keys.contains(v) {
                continue;
            }
            let n = ctx.var_name(*v);
            if let Some(t) = ctx.term_checked_into(*id, &decode)? {
                if spec.outer_scoped.contains(v) {
                    outer.hidden.insert(n.clone());
                }
                outer.add(&n, t);
            }
        }
        let pattern = substitute(&spec.pattern, &outer);
        let _outer_lease = if decode.bytes() > 0 {
            Some(decode.output_lease(decode.bytes())?)
        } else {
            None
        };
        let (base, slice) = outer_slice(&spec.pattern);
        let text = base.to_string();
        let parts = svccache::Parts::of(&ctx.service_scope, rl.endpoint.as_str(), &text);
        let cache = &ctx.snap.results.service;
        let caching = rl.cache != CacheMode::None
            && rl.cache != CacheMode::Off
            && ctx.use_cache
            && cache.enabled()
            && !super::exec::is_local(ctx, rl.endpoint.as_str());
        Ok(Self {
            names,
            outer,
            pattern,
            text,
            slice,
            parts,
            caching,
            inputs: Vec::new(),
            by_values: FxHashMap::default(),
            by_key: FxHashMap::default(),
            missing: Vec::new(),
            held: 0,
            stats: LoopStats::default(),
            _outer_lease,
            kept_lease: None,
        })
    }

    /// The values of the keys in row `i` of `l`.
    fn key(spec: &LateralSpec, l: &Table, i: usize) -> Vec<Id> {
        spec.keys
            .iter()
            .map(|k| l.col_of(*k).map_or(Id::UNDEF, |c| l.cols[c][i]))
            .collect()
    }

    /// Take the left rows `l`: their new distinct inputs, answered from the cache where
    /// it has them. Reconciles each row's transient decode charge continuously into
    /// `kept_lease`, releasing credit for filtered or deduplicated terms immediately.
    pub(super) fn add(
        &mut self,
        ctx: &Ctx,
        spec: &LateralSpec,
        rl: &RemoteLoop,
        l: &Table,
    ) -> Result<()> {
        let cache = &ctx.snap.results.service;
        for i in 0..l.len() {
            let key = Self::key(spec, l, i);
            if self.by_key.contains_key(&key) {
                continue;
            }
            ctx.check()?;
            let row_decode = ctx.charge(0)?;
            let mut values = Vec::with_capacity(key.len());
            for id in &key {
                let term = if id.is_undef() {
                    None
                } else {
                    ctx.check()?;
                    match ctx.kind(*id) {
                        TermKind::Iri | TermKind::Literal => {
                            let prev = row_decode.bytes();
                            match ctx.term_checked_into(*id, &row_decode)? {
                                Some(t) if matches!(t, Term::NamedNode(_) | Term::Literal(_)) => {
                                    Some(t)
                                }
                                discarded => {
                                    drop(discarded);
                                    row_decode.resize(prev)?;
                                    None
                                }
                            }
                        }
                        _ => None,
                    }
                };
                values.push(term);
            }
            let key_cost = key_bytes(key.len());
            if let Some(&existing) = self.by_values.get(&values) {
                // Duplicate values (including filtered terms that collapsed to an existing
                // entry): drop the decoded terms and settle credit down to the key alone.
                drop(values);
                let mut lease = Some(row_decode.output_lease(key_cost)?);
                match &mut self.kept_lease {
                    Some(held) => held.merge(&mut lease)?,
                    None => self.kept_lease = lease,
                }
                self.by_key.insert(key, (existing, false));
                continue;
            }
            let mut input = Input {
                values,
                key: None,
                rows: None,
            };
            let base_kept = key_cost.saturating_add(input_bytes(&input));
            if self.caching {
                let mut kv: Vec<(&str, &Term)> = self
                    .outer
                    .map
                    .iter()
                    .map(|(k, v)| (k.as_str(), v))
                    .collect();
                for (n, t) in self.names.iter().zip(&input.values) {
                    if let Some(t) = t {
                        kv.push((n.as_str(), t));
                    }
                }
                input.key = Some(svccache::try_key(
                    &ctx.service_scope,
                    rl.endpoint.as_str(),
                    &self.text,
                    &kv,
                    |cap| -> Result<()> {
                        let needed = base_kept.saturating_add(cap as u64);
                        if needed > row_decode.bytes() {
                            row_decode.add(needed - row_decode.bytes())?;
                        }
                        Ok(())
                    },
                )?);
            }
            let kept_cost =
                base_kept.saturating_add(input.key.as_ref().map_or(0, |k| k.capacity() as u64));
            // Admit any growth above the transient decode charge before cloning `values`
            // into `by_values` or mutating state, then settle continuously into `kept_lease`.
            if kept_cost > row_decode.bytes() {
                row_decode.add(kept_cost - row_decode.bytes())?;
            }
            let mut kept_cost = kept_cost;
            if let Some(k) = &input.key {
                if rl.cache.reads() {
                    // what the lookup allocates is admitted before it is, on the
                    // charge that settles into the kept lease below, which keeps the
                    // answer charged for as long as the input holds it
                    input.rows = cache.get_admitted(k, self.slice, &|n| row_decode.add(n))?;
                    if let Some(rows) = &input.rows {
                        self.stats.cache_hits += 1;
                        kept_cost = kept_cost.saturating_add(rows.retained_bytes());
                        if kept_cost > row_decode.bytes() {
                            row_decode.add(kept_cost - row_decode.bytes())?;
                        }
                    }
                } else {
                    cache.remove(k);
                }
            }
            let n = self.inputs.len();
            self.by_values.insert(input.values.clone(), n);
            if input.rows.is_none() {
                self.missing.push(n);
            }
            self.inputs.push(input);
            self.by_key.insert(key, (n, false));
            let mut lease = Some(row_decode.output_lease(kept_cost)?);
            match &mut self.kept_lease {
                Some(held) => held.merge(&mut lease)?,
                None => self.kept_lease = lease,
            }
        }
        self.stats.inputs = self.inputs.len();
        Ok(())
    }

    /// Send the requests for the unanswered inputs: the full bulk requests among them,
    /// and with `all` the last partial one too. After each request it passes `admit` the
    /// answer bytes the loop then holds ([`RemoteRun::answer_bytes`]), so a caller that
    /// charges them refuses the loop before it sends the next request. Keys and inputs
    /// are already covered by `self.kept_lease`.
    pub(super) fn send(
        &mut self,
        ctx: &Ctx,
        spec: &LateralSpec,
        rl: &RemoteLoop,
        all: bool,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        let bulk = rl.bulk.max(1);
        let ready = match all {
            true => self.missing.len(),
            false => self.missing.len() / bulk * bulk,
        };
        if ready == 0 {
            return Ok(());
        }
        let missing: Vec<usize> = self.missing.drain(..ready).collect();
        let mut held = self.held;
        let (names, outer, pattern) = (&self.names, &self.outer, &self.pattern);
        let (key_parts, slice) = (self.parts, self.slice);
        let (inputs, stats) = (&mut self.inputs, &mut self.stats);
        let unit = || {
            Arc::new(Rows {
                vars: Vec::new(),
                rows: vec![Vec::new()],
            })
        };
        // the request of one input alone
        let single = |input: &Input| -> String {
            let mut s = Subst {
                hidden: outer.hidden.clone(),
                ..Subst::default()
            };
            for (n, t) in names.iter().zip(&input.values) {
                if let Some(t) = t {
                    s.add(n, t.clone());
                }
            }
            s.map
                .extend(outer.map.iter().map(|(k, v)| (k.clone(), v.clone())));
            single_request(&substitute(&spec.pattern, &s))
        };
        // one input's request, its solutions admitted as they arrive on a charge that
        // the caller drops once it has admitted them itself
        let one = |input: &Input, stats: &mut LoopStats| {
            stats.requests += 1;
            let charge = ctx.charge(0)?;
            let rows = fetch_rows_within(ctx, &rl.endpoint, &single(input), None, Some(&charge))?;
            Ok::<_, Error>((rows, charge))
        };
        // the request of a chunk of inputs: one input's alone, or a bulk request
        let request = |chunk: &[usize], inputs: &[Input]| -> String {
            if chunk.len() == 1 {
                return single(&inputs[chunk[0]]);
            }
            if rl.values {
                let tuples: Vec<&[Option<Term>]> =
                    chunk.iter().map(|i| &inputs[*i].values[..]).collect();
                values_request(pattern, names, &tuples, &rl.idx)
            } else {
                let members: Vec<GraphPattern> = chunk
                    .iter()
                    .map(|i| {
                        let mut s = Subst::default();
                        for (n, t) in names.iter().zip(&inputs[*i].values) {
                            if let Some(t) = t {
                                s.add(n, t.clone());
                            }
                        }
                        substitute(pattern, &s)
                    })
                    .collect();
                union_request(&members, &rl.idx)
            }
        };
        let cache = &ctx.snap.results.service;
        let chunks: Vec<&[usize]> = missing.chunks(bulk).collect();
        // an in-process endpoint answers one request at a time
        let width = if super::exec::is_local(ctx, rl.endpoint.as_str()) {
            1
        } else {
            rl.parallel.max(1)
        };
        // Up to `width` requests go out at once. Their answers are taken in the order of
        // the inputs, so neither the answer nor the requests that follow depend on
        // which response came first, and no wave starts after a failure.
        let arc_counters = (2 * std::mem::size_of::<usize>()) as u64;
        let answer_slot = std::mem::size_of::<(usize, Arc<Rows>, bool)>() as u64;
        let per_answer_overhead = arc_counters + answer_slot;
        for wave in chunks.chunks(width) {
            ctx.check()?;
            let queries: Vec<String> = wave.iter().map(|c| request(c, inputs)).collect();
            stats.requests += queries.len();
            let answers = fetch_all(ctx, &rl.endpoint, &queries)?;
            for (chunk, answer) in wave.iter().copied().zip(answers) {
                // the charges that admitted the answers, held until `admit` has them
                let mut charges = Vec::new();
                let fetched: Result<Vec<(usize, Arc<Rows>, bool)>> = if chunk.len() == 1 {
                    answer.and_then(|(r, c)| {
                        c.add(per_answer_overhead)?;
                        charges.push(c);
                        Ok(vec![(chunk[0], Arc::new(r), true)])
                    })
                } else {
                    match answer {
                        Ok((r, c)) => {
                            let has_marker = has_end_marker(&r, &rl.idx);
                            if !has_marker {
                                drop(r);
                                drop(c);
                                stats.retried += 1;
                                let mut out = Vec::new();
                                let mut err = None;
                                let mut retried = held;
                                for i in chunk {
                                    match ctx
                                        .check()
                                        .and_then(|()| one(&inputs[*i], stats))
                                        .and_then(|(r, c)| {
                                            drop(c);
                                            retried = retried
                                                .saturating_add(r.retained_bytes())
                                                .saturating_add(per_answer_overhead);
                                            admit(retried).map(|()| r)
                                        }) {
                                        Ok(r) => out.push((*i, Arc::new(r), true)),
                                        Err(e) => {
                                            err = Some(e);
                                            break;
                                        }
                                    }
                                }
                                match err {
                                    Some(e) => Err(e),
                                    None => Ok(out),
                                }
                            } else {
                                let arc_inner = (std::mem::size_of::<Rows>()
                                    + 2 * std::mem::size_of::<usize>())
                                    as u64;
                                let schema_bytes: u64 = r
                                    .vars
                                    .iter()
                                    .filter(|v| *v != rl.idx.as_str())
                                    .map(|v| (v.len() + std::mem::size_of::<String>()) as u64)
                                    .sum();
                                let per_part = schema_bytes + arc_inner + answer_slot;
                                let counts_overhead =
                                    std::mem::size_of_val(chunk) as u64;
                                let split_overhead =
                                    chunk.len() as u64 * per_part + counts_overhead;
                                let admitted = c
                                    .add(r.retained_bytes().saturating_add(split_overhead))
                                    .map(|()| {
                                        charges.push(c);
                                        split_arc(&r, &rl.idx, chunk)
                                    });
                                match admitted {
                                    Err(e) => Err(e),
                                    Ok(parts) => Ok(parts),
                                }
                            }
                        }
                        Err(e) => Err(e),
                    }
                };
                match fetched {
                    Ok(answered) => {
                        for (i, rows, complete) in answered {
                            if self.caching
                                && complete
                                && rl.cache.writes()
                                && let Some(k) = &inputs[i].key
                            {
                                cache.put(k.clone(), key_parts, slice, rows.clone());
                            }
                            inputs[i].rows = Some(rows);
                        }
                    }
                    Err(e) => failed(ctx, e, rl, chunk, inputs, unit)?,
                }
                // release the per-fetch charges before admitting the held total, so
                // the same rows are not counted twice against the budget
                drop(charges);
                let answered: u64 = chunk
                    .iter()
                    .filter_map(|&i| inputs[i].rows.as_ref())
                    .map(|rows| rows.retained_bytes().saturating_add(per_answer_overhead))
                    .sum();
                held = held.saturating_add(answered);
                self.held = held;
                admit(held)?;
            }
        }
        Ok(())
    }

    /// How many of the first rows of `l` have answered inputs. The unanswered inputs are
    /// the last to appear, so every row before the first unanswered one is answered.
    pub(super) fn answered(&self, spec: &LateralSpec, l: &Table) -> usize {
        (0..l.len())
            .find(|&i| {
                self.by_key
                    .get(&Self::key(spec, l, i))
                    .is_none_or(|(input, _)| self.inputs[*input].rows.is_none())
            })
            .unwrap_or(l.len())
    }

    /// The bytes of the fetched answers the loop holds. Answers from the cache, with the
    /// distinct keys and inputs, are charged separately on `self.kept_lease`.
    pub(super) fn answer_bytes(&self) -> u64 {
        self.held
    }

    /// Join the left rows `l`, whose inputs all have answers, with their solutions.
    pub(super) fn join(
        &mut self,
        ctx: &Ctx,
        spec: &LateralSpec,
        l: &Table,
        vars: &[VarId],
    ) -> Result<Table> {
        // the left rows by their values of the keys, in the order they first appear
        let mut groups: FxHashMap<Vec<Id>, Vec<usize>> = FxHashMap::default();
        let mut order: Vec<Vec<Id>> = Vec::new();
        for i in 0..l.len() {
            let key = Self::key(spec, l, i);
            if let Some(g) = groups.get_mut(&key) {
                g.push(i);
                continue;
            }
            order.push(key.clone());
            groups.insert(key, vec![i]);
        }
        let mut rvars: Vec<VarId> = vars
            .iter()
            .copied()
            .filter(|v| l.col_of(*v).is_none() || spec.keys.contains(v))
            .collect();
        rvars.dedup();
        let expr = match &spec.optional {
            Some(Some(e)) => Some(Planner::new(ctx).compile(e, &spec.graph)?),
            _ => None,
        };
        let mut out = Table::new(vars.to_vec());
        for key in &order {
            ctx.check()?;
            let idx = &groups[key];
            let Some((input, counted)) = self.by_key.get_mut(key) else {
                return Err(Error::invalid("a remote loop row has no input"));
            };
            let input = &self.inputs[*input];
            let Some(rows) = &input.rows else { continue };
            let skip: FxHashSet<VarId> = spec
                .keys
                .iter()
                .zip(&input.values)
                .filter(|(_, v)| v.is_some())
                .map(|(k, _)| *k)
                .collect();
            let r = to_table(ctx, rows, &rvars, &skip);
            // one eager call counts the solutions of each key once
            if !std::mem::replace(counted, true) {
                self.stats.solutions += r.len();
            }
            let left_rows = l.take_rows(idx);
            let joined = match &spec.optional {
                Some(_) => {
                    let mut note = None;
                    left_join(ctx, &left_rows, &r, expr.as_ref(), &mut note)?
                }
                None if r.is_empty() => continue,
                None => join_tables(ctx, &left_rows, &r, &[], false)?,
            };
            ctx.check_output(out.len() + joined.len(), vars.len())?;
            out.append(joined.project(vars));
        }
        Ok(out)
    }
}

/// A failed request: a refusal and a spent budget always fail the query, and so does a
/// failure once the request's outbound time is spent; with SILENT, any other failure
/// gives each of the chunk's inputs one empty solution.
fn failed(
    ctx: &Ctx,
    e: Error,
    rl: &RemoteLoop,
    chunk: &[usize],
    inputs: &mut [Input],
    unit: impl Fn() -> Arc<Rows>,
) -> Result<()> {
    match e {
        e @ (Error::NotPermitted(_)
        | Error::BudgetExceeded(_)
        | Error::Timeout
        | Error::Cancelled) => Err(e),
        e if ctx.outbound_budget.remaining_time().is_zero() => Err(e),
        _ if rl.silent => {
            for i in chunk {
                inputs[*i].rows = Some(unit());
            }
            Ok(())
        }
        e => Err(e),
    }
}

/// Whether the bulk response contains the end marker row.
fn has_end_marker(r: &Rows, idx: &str) -> bool {
    let Some(ic) = r.vars.iter().position(|v| v == idx) else {
        return false;
    };
    r.rows.iter().any(|row| {
        matches!(&row[ic], Some(Term::Literal(i)) if i.value().parse::<i64>().ok() == Some(END_MARKER))
    })
}

/// Split a bulk response into per-input `Arc<Rows>`, paired with the chunk index and a
/// completeness flag. The caller has already verified the end marker is present.
fn split_arc(r: &Rows, idx: &str, chunk: &[usize]) -> Vec<(usize, Arc<Rows>, bool)> {
    let n = chunk.len();
    let ic = r.vars.iter().position(|v| v == idx).expect("index column");
    let vars: Vec<String> = r.vars.iter().filter(|v| *v != idx).cloned().collect();
    let mut counts = vec![0usize; n];
    for row in &r.rows {
        let Some(Term::Literal(i)) = &row[ic] else {
            continue;
        };
        let Ok(i) = i.value().parse::<i64>() else {
            continue;
        };
        if i == END_MARKER {
            continue;
        }
        if let Some(c) = usize::try_from(i).ok().and_then(|i| counts.get_mut(i)) {
            *c += 1;
        }
    }
    let mut out: Vec<(usize, Arc<Rows>, bool)> = chunk
        .iter()
        .copied()
        .zip(&counts)
        .map(|(i, &c)| {
            (
                i,
                Arc::new(Rows {
                    vars: vars.clone(),
                    rows: Vec::with_capacity(c),
                }),
                true,
            )
        })
        .collect();
    for row in &r.rows {
        let Some(Term::Literal(i)) = &row[ic] else {
            continue;
        };
        let Ok(i) = i.value().parse::<i64>() else {
            continue;
        };
        if i == END_MARKER {
            continue;
        }
        let Some(p) = usize::try_from(i).ok().and_then(|i| out.get_mut(i)) else {
            continue;
        };
        Arc::get_mut(&mut p.1).unwrap().rows.push(
            row.iter()
                .enumerate()
                .filter(|(c, _)| *c != ic)
                .map(|(_, t)| t.clone())
                .collect(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(iri: &str) -> (Vec<(&str, Option<&str>)>, &str) {
        parse_options(iri)
    }

    #[test]
    fn options_and_endpoint() {
        assert_eq!(
            opts("loop:bulk+10:cache:https://query.wikidata.org/sparql"),
            (
                vec![("loop", None), ("bulk", Some("10")), ("cache", None)],
                "https://query.wikidata.org/sparql"
            )
        );
        assert_eq!(opts("cache:"), (vec![("cache", None)], ""));
        assert_eq!(
            opts("bulk:loop"),
            (vec![("bulk", None), ("loop", None)], "")
        );
        assert_eq!(
            opts("loop:urn:x-arq:self"),
            (vec![("loop", None)], "urn:x-arq:self")
        );
        assert_eq!(
            opts("http://localhost:3030/ds/sparql"),
            (vec![], "http://localhost:3030/ds/sparql")
        );
        assert_eq!(
            opts("cache+clear:http://e/s"),
            (vec![("cache", Some("clear"))], "http://e/s")
        );
        // `loop` alone is a relative IRI's text, not an option
        assert_eq!(opts("loop"), (vec![], "loop"));
    }

    fn eff(q: &str) -> Result<Option<(Options, Target)>> {
        let parsed = spargebra::SparqlParser::new().parse_query(q).unwrap();
        let spargebra::Query::Select { pattern, .. } = parsed else {
            unreachable!()
        };
        fn find(gp: &GraphPattern) -> Option<&GraphPattern> {
            match gp {
                GraphPattern::Service { .. } => Some(gp),
                GraphPattern::Project { inner, .. } | GraphPattern::Filter { inner, .. } => {
                    find(inner)
                }
                GraphPattern::Join { left, right } => find(right).or_else(|| find(left)),
                _ => None,
            }
        }
        let Some(GraphPattern::Service {
            name,
            inner,
            silent,
        }) = find(&pattern)
        else {
            unreachable!()
        };
        Ok(effective(name, inner, *silent)?.map(|e| (e.opts, e.target)))
    }

    #[test]
    fn effective_options() {
        let wd = NamedNode::new("https://query.wikidata.org/sparql").unwrap();
        let (o, t) = eff(
            "SELECT * { SERVICE <cache:loop:bulk+5:https://query.wikidata.org/sparql> { ?s ?p ?o } }",
        )
        .unwrap()
        .unwrap();
        assert!(o.looped);
        assert_eq!(o.bulk, Some(Some(5)));
        assert_eq!(o.cache, CacheMode::Default);
        assert_eq!(t, Target::Remote(wd.clone()));
        // options over a SERVICE directly inside merge, the outer ones first
        let (o, t) = eff(
            "SELECT * { SERVICE <loop:cache+off:> { SERVICE <cache:https://query.wikidata.org/sparql> { ?s ?p ?o } } }",
        )
        .unwrap()
        .unwrap();
        assert!(o.looped);
        assert_eq!(o.cache, CacheMode::Off);
        assert_eq!(t, Target::Remote(wd));
        let (o, t) = eff("SELECT * { SERVICE <loop:> { ?s ?p ?o } }")
            .unwrap()
            .unwrap();
        assert!(o.looped);
        assert_eq!(t, Target::SelfDataset);
        let (o, t) = eff("SELECT * { SERVICE <urn:x-arq:self> { ?s ?p ?o } }")
            .unwrap()
            .unwrap();
        assert_eq!(o, Options::default());
        assert_eq!(t, Target::SelfDataset);
        assert!(
            eff("SELECT * { SERVICE <http://e/s> { ?s ?p ?o } }")
                .unwrap()
                .is_none()
        );
        assert!(eff("SELECT * { SERVICE <bulk+x:http://e/s> { ?s ?p ?o } }").is_err());
        assert!(eff("SELECT * { SERVICE <cache+maybe:http://e/s> { ?s ?p ?o } }").is_err());
        // parallel, with and without a count
        let (o, _) = eff("SELECT * { SERVICE <loop:parallel+3:http://e/s> { ?s ?p ?o } }")
            .unwrap()
            .unwrap();
        assert_eq!(o.parallel, Some(Some(3)));
        let (o, _) = eff("SELECT * { SERVICE <parallel:loop:http://e/s> { ?s ?p ?o } }")
            .unwrap()
            .unwrap();
        assert_eq!((o.parallel, o.looped), (Some(None), true));
        assert!(eff("SELECT * { SERVICE <parallel+0:http://e/s> { ?s ?p ?o } }").is_err());
    }

    /// The outermost slice of a pattern is split off for the cache's key, and a pattern
    /// without one has every solution.
    #[test]
    fn outer_slices() {
        let q = |s: &str| pattern(&format!("SELECT * {{ SERVICE <http://e/s> {{ {s} }} }}"));
        let inner = |gp: &GraphPattern| -> GraphPattern {
            let mut found = None;
            let _ = super::super::walk::visit_graph_patterns(gp, |p| {
                if let GraphPattern::Service { inner, .. } = p {
                    found.get_or_insert_with(|| (**inner).clone());
                }
                Ok::<(), ()>(())
            });
            found.expect("a SERVICE")
        };
        let gp = inner(&q("SELECT ?p { ?s ?p ?o } ORDER BY ?p OFFSET 2 LIMIT 3"));
        let (base, slice) = outer_slice(&gp);
        assert_eq!(
            slice,
            svccache::Slice {
                offset: 2,
                limit: Some(3)
            }
        );
        assert!(!matches!(base, GraphPattern::Slice { .. }));
        let gp = inner(&q("?s ?p ?o"));
        assert_eq!(outer_slice(&gp).1, svccache::Slice::ALL);
        assert_eq!(
            svccache::Slice::ALL.limited(4).limited(9),
            svccache::Slice {
                offset: 0,
                limit: Some(4)
            }
        );
    }

    fn pattern(q: &str) -> GraphPattern {
        match spargebra::SparqlParser::new().parse_query(q).unwrap() {
            spargebra::Query::Select { pattern, .. } => pattern,
            _ => unreachable!(),
        }
    }

    fn subst(pairs: &[(&str, Term)]) -> Subst {
        let mut s = Subst::default();
        for (k, v) in pairs {
            s.add(k, v.clone());
        }
        s
    }

    #[test]
    fn substitution_reaches_sub_selects() {
        let s: Term = NamedNode::new("urn:s").unwrap().into();
        let p = pattern("SELECT ?s { ?s ?p ?o { SELECT ?p { ?s ?p ?o } } }");
        let out = substitute(&p, &subst(&[("s", s.clone())])).to_string();
        // the projection keeps the name; the patterns have the value
        assert_eq!(out.matches("?s").count(), 1, "{out}");
        // with the LATERAL scope, a sub-select that does not project ?s hides it
        let mut sc = subst(&[("s", s)]);
        sc.hidden.insert("s".into());
        let out = substitute(&p, &sc).to_string();
        assert_eq!(out.matches("?s").count(), 2, "{out}");
    }

    #[test]
    fn substitution_of_groups_and_filters() {
        let p: Term = NamedNode::new("urn:p").unwrap().into();
        let q = pattern(
            "SELECT ?p (COUNT(*) AS ?c) { ?s ?p ?o FILTER(BOUND(?p) && ?p != <urn:x>) } GROUP BY ?p",
        );
        let out = substitute(&q, &subst(&[("p", p)])).to_string();
        // grouped by the BIND of the value, filters see it
        assert!(out.contains("BIND(<urn:p> AS ?p)"), "{out}");
        assert!(out.contains("!(<urn:p> = <urn:x>)"), "{out}");
        assert!(out.contains("GROUP BY ?p"), "{out}");
        // a literal never stands as a subject; an assigned variable is kept
        let l: Term = Literal::from(1).into();
        let q = pattern("SELECT * { ?x <urn:p> ?y BIND(2 AS ?z) }");
        let out =
            substitute(&q, &subst(&[("x", l.clone()), ("y", l.clone()), ("z", l)])).to_string();
        assert!(out.contains(r#"?x <urn:p> "1"^^"#), "{out}");
        assert!(out.contains("AS ?z"), "{out}");
    }

    #[test]
    fn request_shapes() {
        let inner = pattern("SELECT * { ?s <urn:p> ?o }");
        let a: Term = NamedNode::new("urn:a").unwrap().into();
        let t1 = [Some(a)];
        let t2 = [None];
        let q = values_request(&inner, &["s".into()], &[&t1, &t2], "__idx__");
        assert!(
            q.contains(r#"VALUES ( ?s ?__idx__ ) { ( <urn:a> "0"^^"#),
            "{q}"
        );
        assert!(q.contains(r#"( UNDEF "1"^^"#), "{q}");
        assert!(
            q.ends_with("UNION { BIND(1000000000 AS ?__idx__) } } ORDER BY ?__idx__"),
            "{q}"
        );
        spargebra::SparqlParser::new().parse_query(&q).unwrap();
        let q = union_request(&[inner.clone(), inner], "__idx__");
        spargebra::SparqlParser::new().parse_query(&q).unwrap();
        assert!(q.contains("BIND(1 AS ?__idx__)"), "{q}");
    }

    #[test]
    fn bulk_responses_split_by_index() {
        let lit = |i: i64| Some(Term::Literal(Literal::from(i)));
        let x = |s: &str| Some(Term::NamedNode(NamedNode::new(s).unwrap()));
        let r = Rows {
            vars: vec!["o".into(), "__idx__".into()],
            rows: vec![
                vec![x("urn:a"), lit(0)],
                vec![x("urn:b"), lit(1)],
                vec![x("urn:c"), lit(1)],
                vec![None, lit(END_MARKER)],
            ],
        };
        assert!(has_end_marker(&r, "__idx__"));
        let chunk = [0, 1, 2];
        let parts = split_arc(&r, "__idx__", &chunk);
        assert_eq!(
            parts
                .iter()
                .map(|(_, p, _)| p.rows.len())
                .collect::<Vec<_>>(),
            [1, 2, 0]
        );
        assert_eq!(parts[0].1.vars, ["o"]);
        let cut = Rows {
            vars: r.vars.clone(),
            rows: r.rows[..2].to_vec(),
        };
        assert!(!has_end_marker(&cut, "__idx__"));
    }
}
