//! From a query's algebra to a circuit of incremental operators (spec F13 §3.2, §4).
//!
//! The compiler works on the parsed algebra rather than on the planner's tree, because
//! the planner fuses operators (index joins, stars, key filters) whose boundaries the
//! incremental rules need. Joins of triple patterns, also under `GRAPH` and in simple
//! paths, become one basic graph pattern node, whose own probes choose their order.
//! Every subtree outside the fragment of §4.1 becomes a `recompute` node, and features
//! without a value per snapshot are refused (§4.5).

use super::footprint::{Entry, FGraph, Footprint};
use super::ops::bgp::{Bgp, GSlot, Pat, Slot};
use super::ops::distinct::Distinct;
use super::ops::group::{AggKind, AggSpec, Group};
use super::ops::join::Join;
use super::ops::linear::{Extend, Filter, Project, Union, Values};
use super::ops::matching::{MatchMode, MatchOp};
use super::ops::recompute::Recompute;
use super::ops::template::{TSlot, Template};
use super::ops::{Node, Op};
use super::zset::ZSet;
use crate::error::{ComponentError, Error, Result};
use oxrdf::{NamedNode, Term, Variable};
use rustc_hash::FxHashMap;
use spargebra::Query;
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, Function, GraphPattern as GP,
    OrderExpression, PropertyPathExpression as PPE, QueryDataset,
};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

/// The prefix of the variables the compiler adds for the inner nodes of paths.
const HIDDEN: &str = "__incr";
/// The most alternatives a path expands to before it is recomputed instead.
const MAX_PATH_ALTERNATIVES: usize = 16;

/// What a circuit's result is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Kind {
    Select,
    Ask,
    Construct,
}

/// A compiled query.
#[derive(Clone, Debug)]
pub struct Compiled {
    pub kind: Kind,
    /// every variable of the circuit, by number
    pub vars: Vec<String>,
    pub root: Node,
    pub footprint: Footprint,
    /// the query's base IRI
    pub base: Option<oxiri::Iri<String>>,
    /// the query's pattern and dataset clause, which a circuit over inferred data reads
    /// its footprint from
    pub pattern: GP,
    pub dataset: Option<QueryDataset>,
}

/// The footprint of `gp` read in the default graph.
pub(crate) fn pattern_footprint(gp: &GP) -> Footprint {
    let mut fp = Footprint::default();
    footprint_of(gp, &Scope::Default, &mut fp);
    fp
}

/// The error of a query that cannot be a view or a live query (`400 not-incremental`).
pub fn not_incremental(feature: &str) -> Error {
    Error::Component(Box::new(ComponentError::new(
        "views",
        "not-incremental",
        format!("{feature} cannot be used in a view or a live query"),
    )))
}

/// The active graph of a subtree.
#[derive(Clone, Debug)]
enum Scope {
    Default,
    Graph(NamedNode),
    Var(Variable),
}

impl Scope {
    fn fgraph(&self) -> FGraph {
        match self {
            Scope::Default => FGraph::Default,
            Scope::Graph(n) => FGraph::Graph(Term::NamedNode(n.clone())),
            Scope::Var(_) => FGraph::Named,
        }
    }

    fn wrap(&self, gp: GP) -> GP {
        match self {
            Scope::Default => gp,
            Scope::Graph(n) => GP::Graph {
                name: NamedNodePattern::NamedNode(n.clone()),
                inner: Box::new(gp),
            },
            Scope::Var(v) => GP::Graph {
                name: NamedNodePattern::Variable(v.clone()),
                inner: Box::new(gp),
            },
        }
    }
}

/// The feature that makes a query unfit for a view, if any (spec F13 §4.5).
pub fn refused(q: &Query) -> Option<String> {
    let (pattern, construct) = match q {
        Query::Select { pattern, .. } | Query::Ask { pattern, .. } => (pattern, None),
        Query::Construct {
            pattern,
            graph_templates,
            ..
        } => {
            if !graph_templates.is_empty() {
                return Some("a GRAPH block in a CONSTRUCT template".into());
            }
            (pattern, Some(()))
        }
        Query::Describe { .. } => return Some("DESCRIBE".into()),
    };
    let _ = construct;
    let mut found = None;
    refused_gp(pattern, &mut found);
    found
}

fn refused_expr(e: &Expression, found: &mut Option<String>) {
    if found.is_some() {
        return;
    }
    use Expression as E;
    match e {
        E::FunctionCall(f, args) => {
            let name = match f {
                Function::Now => Some("NOW"),
                Function::Rand => Some("RAND"),
                Function::Uuid => Some("UUID"),
                Function::StrUuid => Some("STRUUID"),
                Function::BNode => Some("BNODE"),
                _ => None,
            };
            if let Some(n) = name {
                *found = Some(n.into());
                return;
            }
            for a in args {
                refused_expr(a, found);
            }
        }
        E::Exists(p) => refused_gp(p, found),
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
            refused_expr(a, found);
            refused_expr(b, found);
        }
        E::In(a, l) => {
            refused_expr(a, found);
            for x in l {
                refused_expr(x, found);
            }
        }
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => refused_expr(a, found),
        E::If(a, b, c) => {
            refused_expr(a, found);
            refused_expr(b, found);
            refused_expr(c, found);
        }
        E::Coalesce(l) => {
            for x in l {
                refused_expr(x, found);
            }
        }
        _ => {}
    }
}

/// Whether `iri` names a property function, which computes its answers instead of
/// matching stored triples.
fn property_function(iri: &str) -> bool {
    iri == crate::sparql::arqpf::RDFS_MEMBER
        || crate::sparql::catalog::property_functions()
            .iter()
            .any(|f| f == iri)
}

/// The search property functions, which read indexes built after the commit.
fn search_function(iri: &str) -> Option<&'static str> {
    if iri == crate::sparql::textpf::TEXT_QUERY {
        Some("text:query")
    } else if iri == crate::vector::VECTOR_SEARCH {
        Some("spk:vectorSearch")
    } else if iri == crate::sparql::hybrid::HYBRID_SEARCH {
        Some("spk:hybridSearch")
    } else if iri.starts_with("http://jena.apache.org/spatial#") {
        Some("spatial property functions")
    } else {
        None
    }
}

fn refused_gp(gp: &GP, found: &mut Option<String>) {
    if found.is_some() {
        return;
    }
    match gp {
        GP::Bgp { patterns } => {
            for t in patterns {
                if let NamedNodePattern::NamedNode(p) = &t.predicate
                    && let Some(f) = search_function(p.as_str())
                {
                    *found = Some(f.into());
                    return;
                }
            }
        }
        GP::Service { name, .. } => {
            let what = match name {
                NamedNodePattern::NamedNode(n) if n.as_str() == super::VIEW_READ => {
                    "SERVICE view:read".to_string()
                }
                NamedNodePattern::NamedNode(n) if crate::sparql::history_svc::is_changes(name) => {
                    let _ = n;
                    "SERVICE hist:changes".to_string()
                }
                NamedNodePattern::NamedNode(_) if crate::sparql::pathsearch::is_search(name) => {
                    "SERVICE path:search".to_string()
                }
                _ => "SERVICE".to_string(),
            };
            *found = Some(what);
        }
        GP::Filter { expr, inner } => {
            refused_expr(expr, found);
            refused_gp(inner, found);
        }
        GP::Extend {
            inner, expression, ..
        }
        | GP::Assign {
            inner, expression, ..
        }
        | GP::Unfold {
            inner, expression, ..
        } => {
            refused_expr(expression, found);
            refused_gp(inner, found);
        }
        GP::LeftJoin {
            left,
            right,
            expression,
        } => {
            if let Some(e) = expression {
                refused_expr(e, found);
            }
            refused_gp(left, found);
            refused_gp(right, found);
        }
        GP::Join { left, right }
        | GP::Lateral { left, right }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right } => {
            refused_gp(left, found);
            refused_gp(right, found);
        }
        GP::OrderBy { inner, expression } => {
            for o in expression {
                let e = match o {
                    spargebra::algebra::OrderExpression::Asc(e)
                    | spargebra::algebra::OrderExpression::Desc(e) => e,
                };
                refused_expr(e, found);
            }
            refused_gp(inner, found);
        }
        GP::Group {
            inner, aggregates, ..
        } => {
            for (_, a) in aggregates {
                match a {
                    AggregateExpression::FunctionCall { expr, .. } => refused_expr(expr, found),
                    AggregateExpression::Fold { expr, value, .. } => {
                        refused_expr(expr, found);
                        if let Some(v) = value {
                            refused_expr(v, found);
                        }
                    }
                    AggregateExpression::CountSolutions { .. } => {}
                }
            }
            refused_gp(inner, found);
        }
        GP::Graph { inner, .. }
        | GP::Project { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. } => refused_gp(inner, found),
        GP::Path { .. } | GP::Values { .. } => {}
    }
}

/// The footprint entries of a subtree read in `scope`.
fn footprint_of(gp: &GP, scope: &Scope, fp: &mut Footprint) {
    fn term(t: &TermPattern) -> Option<Term> {
        match t {
            TermPattern::NamedNode(n) => Some(Term::NamedNode(n.clone())),
            TermPattern::Literal(l) => Some(Term::Literal(l.clone())),
            _ => None,
        }
    }
    /// Whether a path matches the empty path, which joins every node of the graph
    /// to itself.
    fn nullable(p: &PPE) -> bool {
        match p {
            PPE::ZeroOrMore(_) | PPE::ZeroOrOne(_) => true,
            PPE::Range { min, path, .. } => *min == 0 || nullable(path),
            PPE::Reverse(a) | PPE::Distinct(a) | PPE::Multi(a) | PPE::Shortest(a) => nullable(a),
            PPE::OneOrMore(a) => nullable(a),
            PPE::Sequence(a, b) => nullable(a) && nullable(b),
            PPE::Alternative(a, b) => nullable(a) || nullable(b),
            PPE::NamedNode(_) | PPE::NegatedPropertySet(_) => false,
        }
    }
    fn any_quad(scope: &Scope, fp: &mut Footprint) {
        fp.add(Entry {
            g: scope.fgraph(),
            s: None,
            p: None,
            o: None,
        });
    }
    fn path(p: &PPE, scope: &Scope, fp: &mut Footprint) {
        match p {
            PPE::NamedNode(n) => fp.add(Entry {
                g: scope.fgraph(),
                s: None,
                p: Some(Term::NamedNode(n.clone())),
                o: None,
            }),
            PPE::Reverse(a)
            | PPE::ZeroOrMore(a)
            | PPE::OneOrMore(a)
            | PPE::ZeroOrOne(a)
            | PPE::Distinct(a)
            | PPE::Multi(a)
            | PPE::Shortest(a)
            | PPE::Range { path: a, .. } => path(a, scope, fp),
            PPE::Sequence(a, b) | PPE::Alternative(a, b) => {
                path(a, scope, fp);
                path(b, scope, fp);
            }
            PPE::NegatedPropertySet(_) => fp.add(Entry {
                g: scope.fgraph(),
                s: None,
                p: None,
                o: None,
            }),
        }
    }
    fn expr(e: &Expression, scope: &Scope, fp: &mut Footprint) {
        use Expression as E;
        match e {
            E::Exists(p) => footprint_of(p, scope, fp),
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
                expr(a, scope, fp);
                expr(b, scope, fp);
            }
            E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expr(a, scope, fp),
            E::In(a, l) => {
                expr(a, scope, fp);
                l.iter().for_each(|x| expr(x, scope, fp));
            }
            E::If(a, b, c) => {
                expr(a, scope, fp);
                expr(b, scope, fp);
                expr(c, scope, fp);
            }
            E::Coalesce(l) | E::FunctionCall(_, l) => l.iter().for_each(|x| expr(x, scope, fp)),
            _ => {}
        }
    }
    match gp {
        GP::Bgp { patterns } => {
            for t in patterns {
                // a property function reads triples its pattern does not name
                if let NamedNodePattern::NamedNode(n) = &t.predicate
                    && property_function(n.as_str())
                {
                    fp.all = true;
                }
                fp.add(Entry {
                    g: scope.fgraph(),
                    s: term(&t.subject),
                    p: match &t.predicate {
                        NamedNodePattern::NamedNode(n) => Some(Term::NamedNode(n.clone())),
                        NamedNodePattern::Variable(_) => None,
                    },
                    o: term(&t.object),
                });
            }
        }
        GP::Path { path: p, .. } => {
            path(p, scope, fp);
            if nullable(p) {
                any_quad(scope, fp);
            }
        }
        GP::Graph { name, inner } => {
            let s = match name {
                NamedNodePattern::NamedNode(n) => Scope::Graph(n.clone()),
                NamedNodePattern::Variable(v) => Scope::Var(v.clone()),
            };
            let before = (fp.entries.len(), fp.all);
            footprint_of(inner, &s, fp);
            // a group that reads no triple still depends on which graphs exist
            if (fp.entries.len(), fp.all) == before {
                any_quad(&s, fp);
            }
            // the special graphs of ARQ read other graphs
            if let NamedNodePattern::NamedNode(n) = name
                && n.as_str().starts_with("urn:x-arq:")
            {
                footprint_of(inner, &Scope::Var(Variable::new_unchecked("g")), fp);
                footprint_of(inner, &Scope::Default, fp);
            }
        }
        GP::Service { .. } => fp.all = true,
        GP::Filter { expr: e, inner } => {
            expr(e, scope, fp);
            footprint_of(inner, scope, fp);
        }
        GP::Extend {
            inner, expression, ..
        }
        | GP::Assign {
            inner, expression, ..
        }
        | GP::Unfold {
            inner, expression, ..
        } => {
            expr(expression, scope, fp);
            footprint_of(inner, scope, fp);
        }
        GP::LeftJoin {
            left,
            right,
            expression,
        } => {
            if let Some(e) = expression {
                expr(e, scope, fp);
            }
            footprint_of(left, scope, fp);
            footprint_of(right, scope, fp);
        }
        GP::Join { left, right }
        | GP::Lateral { left, right }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right } => {
            footprint_of(left, scope, fp);
            footprint_of(right, scope, fp);
        }
        GP::Group {
            inner, aggregates, ..
        } => {
            for (_, a) in aggregates {
                if let AggregateExpression::FunctionCall { expr: e, .. } = a {
                    expr(e, scope, fp);
                }
            }
            footprint_of(inner, scope, fp);
        }
        GP::OrderBy { inner, expression } => {
            for o in expression {
                match o {
                    OrderExpression::Asc(e) | OrderExpression::Desc(e) => expr(e, scope, fp),
                }
            }
            footprint_of(inner, scope, fp);
        }
        GP::Project { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. } => footprint_of(inner, scope, fp),
        GP::Values { .. } => {}
    }
}

/// The variables a subtree binds, in a stable order: a projection's own order, else the
/// order of first appearance.
fn scope_vars(gp: &GP) -> Vec<String> {
    match gp {
        GP::Project { variables, .. } => variables.iter().map(|v| v.as_str().to_string()).collect(),
        GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::OrderBy { inner, .. } => scope_vars(inner),
        _ => {
            let mut out: Vec<String> = Vec::new();
            gp.on_in_scope_variable(|v| {
                if !out.iter().any(|x| x == v.as_str()) {
                    out.push(v.as_str().to_string());
                }
            });
            out
        }
    }
}

/// The variables a subtree binds in every solution, for the join keys of a `recompute`
/// node's parents.
fn certain_of(gp: &GP) -> Vec<String> {
    fn tp_vars(t: &TermPattern, out: &mut Vec<String>) {
        if let TermPattern::Variable(v) = t {
            out.push(v.as_str().to_string());
        }
    }
    let inter = |a: Vec<String>, b: Vec<String>| a.into_iter().filter(|x| b.contains(x)).collect();
    match gp {
        GP::Bgp { patterns } => {
            let mut out = Vec::new();
            for t in patterns {
                tp_vars(&t.subject, &mut out);
                if let NamedNodePattern::Variable(v) = &t.predicate {
                    out.push(v.as_str().to_string());
                }
                tp_vars(&t.object, &mut out);
            }
            out
        }
        GP::Path {
            subject, object, ..
        } => {
            let mut out = Vec::new();
            tp_vars(subject, &mut out);
            tp_vars(object, &mut out);
            out
        }
        GP::Join { left, right } | GP::Lateral { left, right } => {
            let mut a = certain_of(left);
            a.extend(certain_of(right));
            a
        }
        GP::LeftJoin { left, .. }
        | GP::Minus { left, .. }
        | GP::SemiJoin { left, .. }
        | GP::AntiJoin { left, .. } => certain_of(left),
        GP::Union { left, right } => inter(certain_of(left), certain_of(right)),
        GP::Graph { name, inner } => {
            let mut a = certain_of(inner);
            if let NamedNodePattern::Variable(v) = name {
                a.push(v.as_str().to_string());
            }
            a
        }
        GP::Filter { inner, .. }
        | GP::Extend { inner, .. }
        | GP::Assign { inner, .. }
        | GP::Unfold { inner, .. }
        | GP::OrderBy { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. } => certain_of(inner),
        GP::Project { inner, variables } => {
            let vs: Vec<String> = variables.iter().map(|v| v.as_str().to_string()).collect();
            inter(certain_of(inner), vs)
        }
        GP::Values {
            variables,
            bindings,
        } => variables
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                bindings
                    .iter()
                    .all(|b| b.get(*i).is_some_and(|x| x.is_some()))
            })
            .map(|(_, v)| v.as_str().to_string())
            .collect(),
        GP::Group {
            inner,
            variables,
            aggregates,
        } => {
            let c = certain_of(inner);
            let mut out: Vec<String> = variables
                .iter()
                .map(|v| v.as_str().to_string())
                .filter(|v| c.contains(v))
                .collect();
            for (v, a) in aggregates {
                let counts = matches!(
                    a,
                    AggregateExpression::CountSolutions { .. }
                        | AggregateExpression::FunctionCall {
                            name: AggregateFunction::Count,
                            ..
                        }
                );
                if counts {
                    out.push(v.as_str().to_string());
                }
            }
            out
        }
        GP::Service { .. } => Vec::new(),
    }
}

/// Every variable a subtree mentions, its expressions' included.
fn mentioned(gp: &GP, out: &mut Vec<String>) {
    fn expr(e: &Expression, out: &mut Vec<String>) {
        use Expression as E;
        match e {
            E::Variable(v) | E::Bound(v) => out.push(v.as_str().to_string()),
            E::Exists(p) => mentioned(p, out),
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
                expr(a, out);
                expr(b, out);
            }
            E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expr(a, out),
            E::In(a, l) => {
                expr(a, out);
                l.iter().for_each(|x| expr(x, out));
            }
            E::If(a, b, c) => {
                expr(a, out);
                expr(b, out);
                expr(c, out);
            }
            E::Coalesce(l) | E::FunctionCall(_, l) => l.iter().for_each(|x| expr(x, out)),
            _ => {}
        }
    }
    gp.on_in_scope_variable(|v| out.push(v.as_str().to_string()));
    match gp {
        GP::Filter { expr: e, inner } => {
            expr(e, out);
            mentioned(inner, out);
        }
        GP::Extend {
            inner, expression, ..
        } => {
            expr(expression, out);
            mentioned(inner, out);
        }
        GP::LeftJoin {
            left,
            right,
            expression,
        } => {
            if let Some(e) = expression {
                expr(e, out);
            }
            mentioned(left, out);
            mentioned(right, out);
        }
        GP::Join { left, right }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::Lateral { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right } => {
            mentioned(left, out);
            mentioned(right, out);
        }
        GP::Group {
            inner, aggregates, ..
        } => {
            for (_, a) in aggregates {
                if let AggregateExpression::FunctionCall { expr: e, .. } = a {
                    expr(e, out);
                }
            }
            mentioned(inner, out);
        }
        GP::OrderBy { inner, .. }
        | GP::Graph { inner, .. }
        | GP::Project { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::Assign { inner, .. }
        | GP::Unfold { inner, .. }
        | GP::Service { inner, .. } => mentioned(inner, out),
        _ => {}
    }
}

fn has_exists(e: &Expression) -> bool {
    use Expression as E;
    match e {
        E::Exists(_) => true,
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
        | E::Divide(a, b) => has_exists(a) || has_exists(b),
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => has_exists(a),
        E::In(a, l) => has_exists(a) || l.iter().any(has_exists),
        E::If(a, b, c) => has_exists(a) || has_exists(b) || has_exists(c),
        E::Coalesce(l) | E::FunctionCall(_, l) => l.iter().any(has_exists),
        _ => false,
    }
}

fn conjuncts(e: &Expression, out: &mut Vec<Expression>) {
    match e {
        Expression::And(a, b) => {
            conjuncts(a, out);
            conjuncts(b, out);
        }
        e => out.push(e.clone()),
    }
}

/// Whether a subtree under `GRAPH ?g` binds `?g` in every row by its own patterns, so
/// that the incremental operators evaluate it graph by graph.
fn graph_var_ok(gp: &GP) -> bool {
    match gp {
        GP::Bgp { patterns } => !patterns.is_empty(),
        GP::Path { .. } => true,
        GP::Join { left, right } | GP::Union { left, right } => {
            graph_var_ok(left) && graph_var_ok(right)
        }
        GP::LeftJoin {
            left,
            right,
            expression,
        } => {
            graph_var_ok(left)
                && graph_var_ok(right)
                && expression.as_ref().is_none_or(|e| !has_exists(e))
        }
        GP::Filter { expr, inner } => !has_exists(expr) && graph_var_ok(inner),
        GP::Extend {
            inner, expression, ..
        } => !has_exists(expression) && graph_var_ok(inner),
        _ => false,
    }
}

struct C {
    vars: Vec<String>,
    index: FxHashMap<String, u32>,
    hidden: usize,
}

impl C {
    fn var(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let i = self.vars.len() as u32;
        self.vars.push(name.to_string());
        self.index.insert(name.to_string(), i);
        i
    }

    fn vars_of(&mut self, names: &[String]) -> Vec<u32> {
        names.iter().map(|n| self.var(n)).collect()
    }

    fn fresh(&mut self) -> Variable {
        self.hidden += 1;
        Variable::new_unchecked(format!("{HIDDEN}{}", self.hidden))
    }

    fn recompute(
        &mut self,
        gp: &GP,
        scope: &Scope,
        reason: &str,
        dataset: Option<&QueryDataset>,
    ) -> Node {
        let pattern = scope.wrap(gp.clone());
        let names = scope_vars(&pattern);
        let certain_names = certain_of(&pattern);
        let schema = self.vars_of(&names);
        let certain = schema
            .iter()
            .zip(&names)
            .filter(|(_, n)| certain_names.contains(n))
            .map(|(v, _)| *v)
            .collect();
        let mut fp = Footprint::default();
        footprint_of(&pattern, &Scope::Default, &mut fp);
        if dataset.is_some() {
            fp.all = true;
        }
        let mut r = Recompute::new(pattern, fp, reason.to_string());
        r.dataset = dataset.cloned();
        Node {
            op: Op::Recompute(Box::new(r)),
            schema,
            certain,
        }
    }

    /// The paths of a simple property path as alternatives of pattern sequences, or
    /// `None` for `*`, `+`, `?` and negated property sets (recomputed until Phase 3).
    fn expand_path(
        &mut self,
        s: &TermPattern,
        p: &PPE,
        o: &TermPattern,
    ) -> Option<Vec<Vec<TriplePattern>>> {
        match p {
            PPE::NamedNode(n) => Some(vec![vec![TriplePattern {
                subject: s.clone(),
                predicate: NamedNodePattern::NamedNode(n.clone()),
                object: o.clone(),
            }]]),
            PPE::Reverse(a) => self.expand_path(o, a, s),
            PPE::Sequence(a, b) => {
                let h = TermPattern::Variable(self.fresh());
                let x = self.expand_path(s, a, &h)?;
                let y = self.expand_path(&h, b, o)?;
                if x.len() * y.len() > MAX_PATH_ALTERNATIVES {
                    return None;
                }
                let mut out = Vec::new();
                for a in &x {
                    for b in &y {
                        let mut v = a.clone();
                        v.extend(b.iter().cloned());
                        out.push(v);
                    }
                }
                Some(out)
            }
            PPE::Alternative(a, b) => {
                let mut x = self.expand_path(s, a, o)?;
                x.extend(self.expand_path(s, b, o)?);
                (x.len() <= MAX_PATH_ALTERNATIVES).then_some(x)
            }
            _ => None,
        }
    }

    /// The triple patterns of a subtree made only of joins of patterns (under `GRAPH`
    /// too, and with paths of one alternative), with their scopes.
    fn factors(&mut self, gp: &GP, scope: &Scope, out: &mut Vec<(TriplePattern, Scope)>) -> bool {
        match gp {
            GP::Bgp { patterns } => {
                for t in patterns {
                    let triple_term = matches!(t.subject, TermPattern::Triple(_))
                        || matches!(t.object, TermPattern::Triple(_));
                    let pf = match &t.predicate {
                        NamedNodePattern::NamedNode(p) => property_function(p.as_str()),
                        NamedNodePattern::Variable(_) => false,
                    };
                    if triple_term || pf {
                        return false;
                    }
                    out.push((t.clone(), scope.clone()));
                }
                true
            }
            GP::Join { left, right } => {
                self.factors(left, scope, out) && self.factors(right, scope, out)
            }
            GP::Graph {
                name: NamedNodePattern::NamedNode(n),
                inner,
            } if !n.as_str().starts_with("urn:x-arq:") => {
                self.factors(inner, &Scope::Graph(n.clone()), out)
            }
            GP::Graph {
                name: NamedNodePattern::Variable(v),
                inner,
            } => {
                let before = out.len();
                let ok = self.factors(inner, &Scope::Var(v.clone()), out);
                // an empty group binds nothing per graph
                ok && out.len() > before
            }
            GP::Path {
                subject,
                path,
                object,
            } => match self.expand_path(subject, path, object) {
                Some(alts) if alts.len() == 1 => {
                    for t in alts.into_iter().next().expect("one alternative") {
                        out.push((t, scope.clone()));
                    }
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn bgp(&mut self, factors: Vec<(TriplePattern, Scope)>) -> Node {
        struct Slots {
            names: Vec<String>,
            visible: Vec<bool>,
        }
        impl Slots {
            fn get(&mut self, name: String, vis: bool) -> usize {
                match self.names.iter().position(|s| *s == name) {
                    Some(i) => i,
                    None => {
                        self.names.push(name);
                        self.visible.push(vis);
                        self.names.len() - 1
                    }
                }
            }
            fn pos(&mut self, tp: &TermPattern) -> Slot {
                match tp {
                    TermPattern::Variable(v) => {
                        let hidden = v.as_str().starts_with(HIDDEN);
                        Slot::Var(self.get(v.as_str().to_string(), !hidden))
                    }
                    TermPattern::BlankNode(b) => {
                        Slot::Var(self.get(format!("_:{}", b.as_str()), false))
                    }
                    TermPattern::NamedNode(n) => Slot::Const(Term::NamedNode(n.clone())),
                    TermPattern::Literal(l) => Slot::Const(Term::Literal(l.clone())),
                    TermPattern::Triple(_) => unreachable!("factors refuse triple terms"),
                }
            }
        }
        let mut sl = Slots {
            names: Vec::new(),
            visible: Vec::new(),
        };
        let mut pats = Vec::new();
        for (t, scope) in &factors {
            let s = sl.pos(&t.subject);
            let p = match &t.predicate {
                NamedNodePattern::NamedNode(n) => Slot::Const(Term::NamedNode(n.clone())),
                NamedNodePattern::Variable(v) => Slot::Var(sl.get(v.as_str().to_string(), true)),
            };
            let o = sl.pos(&t.object);
            let g = match scope {
                Scope::Default => GSlot::Default,
                Scope::Graph(n) => GSlot::Const(Term::NamedNode(n.clone())),
                Scope::Var(v) => GSlot::Var(sl.get(v.as_str().to_string(), true)),
            };
            pats.push(Pat { s, p, o, g });
        }
        let (slots, visible) = (sl.names, sl.visible);
        let out_slots: Vec<usize> = (0..slots.len()).filter(|&i| visible[i]).collect();
        let names: Vec<String> = out_slots.iter().map(|&i| slots[i].clone()).collect();
        let schema = self.vars_of(&names);
        // the patterns as algebra, grouped by scope
        let mut groups: Vec<(String, Scope, Vec<TriplePattern>)> = Vec::new();
        for (t, scope) in factors {
            let key = format!("{scope:?}");
            match groups.iter_mut().find(|g| g.0 == key) {
                Some(g) => g.2.push(t),
                None => groups.push((key, scope, vec![t])),
            }
        }
        let mut source: Option<GP> = None;
        for (_, scope, patterns) in groups {
            let gp = scope.wrap(GP::Bgp { patterns });
            source = Some(match source {
                None => gp,
                Some(l) => GP::Join {
                    left: Box::new(l),
                    right: Box::new(gp),
                },
            });
        }
        let display = slots
            .iter()
            .map(|s| {
                if s.starts_with("_:") {
                    s.clone()
                } else {
                    format!("?{s}")
                }
            })
            .collect();
        Node {
            op: Op::Bgp(Box::new(Bgp {
                pats,
                slots: slots.len(),
                out: out_slots,
                names: display,
                source: source.unwrap_or(GP::Bgp { patterns: vec![] }),
            })),
            certain: schema.clone(),
            schema,
        }
    }

    /// The node of one empty solution, the identity of joins.
    fn unit() -> Node {
        Node {
            op: Op::Values(Box::new(Values { rows: vec![vec![]] })),
            schema: vec![],
            certain: vec![],
        }
    }

    fn is_unit(n: &Node) -> bool {
        matches!(&n.op, Op::Values(v) if v.rows.len() == 1 && n.schema.is_empty())
    }

    fn compile(&mut self, gp: &GP, scope: &Scope) -> Node {
        if matches!(
            gp,
            GP::Bgp { .. } | GP::Join { .. } | GP::Graph { .. } | GP::Path { .. }
        ) {
            let mut f = Vec::new();
            if self.factors(gp, scope, &mut f) {
                if f.is_empty() {
                    return match scope {
                        Scope::Var(_) | Scope::Graph(_) => {
                            self.recompute(gp, scope, "an empty group under GRAPH", None)
                        }
                        Scope::Default => C::unit(),
                    };
                }
                return self.bgp(f);
            }
        }
        match gp {
            GP::Bgp { .. } => {
                self.recompute(gp, scope, "a triple term or a property function", None)
            }
            GP::Path {
                subject,
                path,
                object,
            } => match self.expand_path(subject, path, object) {
                Some(alts) => {
                    let mut nodes: Vec<Node> = alts
                        .into_iter()
                        .map(|a| self.bgp(a.into_iter().map(|t| (t, scope.clone())).collect()))
                        .collect();
                    let first = nodes.remove(0);
                    nodes.into_iter().fold(first, |l, r| self.union(l, r))
                }
                None => self.recompute(gp, scope, "a property path with *, + or ? (Phase 3)", None),
            },
            GP::Join { left, right } => {
                let l = self.compile(left, scope);
                let r = self.compile(right, scope);
                if C::is_unit(&l) {
                    return r;
                }
                if C::is_unit(&r) {
                    return l;
                }
                let j = Join::new(l, r);
                let mut certain = j.left.certain.clone();
                for v in &j.right.certain {
                    if !certain.contains(v) {
                        certain.push(*v);
                    }
                }
                Node {
                    schema: j.m.out.clone(),
                    certain,
                    op: Op::Join(Box::new(j)),
                }
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } => {
                if expression.as_ref().is_some_and(has_exists) {
                    return self.recompute(gp, scope, "EXISTS in an OPTIONAL filter", None);
                }
                let l = self.compile(left, scope);
                let r = self.compile(right, scope);
                let certain = l.certain.clone();
                let m = MatchOp::new(MatchMode::LeftJoin, l, r, expression.clone());
                Node {
                    schema: m.m.out.clone(),
                    certain,
                    op: Op::Match(Box::new(m)),
                }
            }
            GP::Minus { left, right } => {
                if matches!(scope, Scope::Var(_)) {
                    return self.recompute(gp, scope, "MINUS under GRAPH ?g", None);
                }
                let l = self.compile(left, scope);
                let r = self.compile(right, scope);
                let (schema, certain) = (l.schema.clone(), l.certain.clone());
                Node {
                    op: Op::Match(Box::new(MatchOp::new(MatchMode::Minus, l, r, None))),
                    schema,
                    certain,
                }
            }
            GP::Filter { expr, inner } => self.filter(gp, expr, inner, scope),
            GP::Union { left, right } => {
                let l = self.compile(left, scope);
                let r = self.compile(right, scope);
                self.union(l, r)
            }
            GP::Graph { name, inner } => match name {
                NamedNodePattern::NamedNode(n) if !n.as_str().starts_with("urn:x-arq:") => {
                    self.compile(inner, &Scope::Graph(n.clone()))
                }
                NamedNodePattern::Variable(v) if graph_var_ok(inner) => {
                    self.compile(inner, &Scope::Var(v.clone()))
                }
                NamedNodePattern::Variable(_) => self.recompute(
                    gp,
                    scope,
                    "a group under GRAPH ?g that its patterns do not bind graph by graph",
                    None,
                ),
                NamedNodePattern::NamedNode(_) => {
                    self.recompute(gp, scope, "an ARQ special graph", None)
                }
            },
            GP::Extend {
                inner,
                variable,
                expression,
            } => {
                if has_exists(expression) {
                    return self.recompute(gp, scope, "EXISTS in BIND", None);
                }
                let i = self.compile(inner, scope);
                let var = self.var(variable.as_str());
                let mut schema = i.schema.clone();
                schema.push(var);
                let certain = i.certain.clone();
                Node {
                    op: Op::Extend(Box::new(Extend {
                        inner: i,
                        var,
                        expr: expression.clone(),
                    })),
                    schema,
                    certain,
                }
            }
            GP::Values {
                variables,
                bindings,
            } => {
                if matches!(scope, Scope::Var(_)) {
                    return self.recompute(gp, scope, "VALUES under GRAPH ?g", None);
                }
                let names: Vec<String> = variables.iter().map(|v| v.as_str().to_string()).collect();
                let schema = self.vars_of(&names);
                let certain = self.vars_of(&certain_of(gp));
                let rows = bindings
                    .iter()
                    .map(|b| {
                        b.iter()
                            .map(|t| {
                                t.as_ref().map(|t| match t {
                                    GroundTerm::NamedNode(n) => Term::NamedNode(n.clone()),
                                    GroundTerm::Literal(l) => Term::Literal(l.clone()),
                                    GroundTerm::Triple(t) => {
                                        Term::Triple(Box::new((**t).clone().into()))
                                    }
                                })
                            })
                            .collect()
                    })
                    .collect();
                Node {
                    op: Op::Values(Box::new(Values { rows })),
                    schema,
                    certain,
                }
            }
            GP::OrderBy { inner, .. } => self.compile(inner, scope),
            GP::Project { inner, variables } => {
                if matches!(scope, Scope::Var(_)) {
                    return self.recompute(gp, scope, "a subquery under GRAPH ?g", None);
                }
                let i = self.compile(inner, scope);
                let names: Vec<String> = variables.iter().map(|v| v.as_str().to_string()).collect();
                let schema = self.vars_of(&names);
                let cols = schema
                    .iter()
                    .map(|v| i.schema.iter().position(|x| x == v))
                    .collect();
                let certain = schema
                    .iter()
                    .filter(|v| i.certain.contains(v))
                    .copied()
                    .collect();
                Node {
                    op: Op::Project(Box::new(Project { inner: i, cols })),
                    schema,
                    certain,
                }
            }
            GP::Distinct { inner } | GP::Reduced { inner } => {
                if matches!(scope, Scope::Var(_)) {
                    return self.recompute(gp, scope, "DISTINCT under GRAPH ?g", None);
                }
                let i = self.compile(inner, scope);
                let (schema, certain) = (i.schema.clone(), i.certain.clone());
                Node {
                    op: Op::Distinct(Box::new(Distinct {
                        inner: i,
                        weights: ZSet::new(),
                    })),
                    schema,
                    certain,
                }
            }
            GP::Slice { .. } => self.recompute(gp, scope, "LIMIT and OFFSET (Phase 2)", None),
            GP::Group {
                inner,
                variables,
                aggregates,
            } => self.group(gp, inner, variables, aggregates, scope),
            GP::Lateral { .. } => self.recompute(gp, scope, "LATERAL", None),
            GP::SemiJoin { .. } | GP::AntiJoin { .. } => {
                self.recompute(gp, scope, "a semi-join", None)
            }
            GP::Assign { .. } | GP::Unfold { .. } => {
                self.recompute(gp, scope, "an ARQ extension", None)
            }
            GP::Service { .. } => self.recompute(gp, scope, "SERVICE", None),
        }
    }

    fn union(&mut self, l: Node, r: Node) -> Node {
        let mut schema = l.schema.clone();
        for v in &r.schema {
            if !schema.contains(v) {
                schema.push(*v);
            }
        }
        let lmap = schema
            .iter()
            .map(|v| l.schema.iter().position(|x| x == v))
            .collect();
        let rmap = schema
            .iter()
            .map(|v| r.schema.iter().position(|x| x == v))
            .collect();
        let certain = l
            .certain
            .iter()
            .filter(|v| r.certain.contains(v))
            .copied()
            .collect();
        Node {
            op: Op::Union(Box::new(Union {
                left: l,
                right: r,
                lmap,
                rmap,
            })),
            schema,
            certain,
        }
    }

    fn filter(&mut self, gp: &GP, expr: &Expression, inner: &GP, scope: &Scope) -> Node {
        let mut cs = Vec::new();
        conjuncts(expr, &mut cs);
        let mut plain: Vec<Expression> = Vec::new();
        let mut exists: Vec<(GP, MatchMode)> = Vec::new();
        for c in cs {
            match &c {
                Expression::Exists(p) => exists.push(((**p).clone(), MatchMode::Exists)),
                Expression::Not(e) if matches!(**e, Expression::Exists(_)) => {
                    let Expression::Exists(p) = &**e else {
                        unreachable!()
                    };
                    exists.push(((**p).clone(), MatchMode::NotExists));
                }
                e if has_exists(e) => {
                    return self.recompute(gp, scope, "EXISTS inside an expression", None);
                }
                _ => plain.push(c),
            }
        }
        let mut node = self.compile(inner, scope);
        if let Some(first) = plain.first().cloned() {
            let e = plain[1..]
                .iter()
                .cloned()
                .fold(first, |a, b| Expression::And(Box::new(a), Box::new(b)));
            let (schema, certain) = (node.schema.clone(), node.certain.clone());
            node = Node {
                op: Op::Filter(Box::new(Filter {
                    inner: node,
                    expr: e,
                })),
                schema,
                certain,
            };
        }
        for (p, mode) in exists {
            // join semantics equal substitution only when the pattern shares nothing but
            // variables both sides always bind, and its expressions read only its own
            let outer: Vec<String> = node
                .schema
                .iter()
                .map(|&v| self.vars[v as usize].clone())
                .collect();
            let mut own: Vec<String> = Vec::new();
            p.on_in_scope_variable(|v| own.push(v.as_str().to_string()));
            let mut all = Vec::new();
            mentioned(&p, &mut all);
            let free_outer = all.iter().any(|v| !own.contains(v) && outer.contains(v));
            let right = self.compile(&p, scope);
            let shared_ok = right.schema.iter().all(|v| {
                !node.schema.contains(v) || (node.certain.contains(v) && right.certain.contains(v))
            });
            if free_outer || !shared_ok || right.has_recompute() && matches!(scope, Scope::Var(_)) {
                return self.recompute(gp, scope, "a correlated EXISTS (Phase 2)", None);
            }
            let (schema, certain) = (node.schema.clone(), node.certain.clone());
            node = Node {
                op: Op::Match(Box::new(MatchOp::new(mode, node, right, None))),
                schema,
                certain,
            };
        }
        node
    }

    fn group(
        &mut self,
        gp: &GP,
        inner: &GP,
        variables: &[Variable],
        aggregates: &[(Variable, AggregateExpression)],
        scope: &Scope,
    ) -> Node {
        if matches!(scope, Scope::Var(_)) {
            return self.recompute(gp, scope, "GROUP BY under GRAPH ?g", None);
        }
        let mut specs: Vec<(Variable, AggKind, bool, Option<Expression>)> = Vec::new();
        for (v, a) in aggregates {
            match a {
                AggregateExpression::CountSolutions { distinct } => {
                    specs.push((v.clone(), AggKind::CountAll, *distinct, None))
                }
                AggregateExpression::FunctionCall {
                    name,
                    expr,
                    distinct,
                } => {
                    let kind = match name {
                        AggregateFunction::Count => AggKind::Count,
                        AggregateFunction::Sum => AggKind::Sum,
                        AggregateFunction::Avg => AggKind::Avg,
                        AggregateFunction::Min | AggregateFunction::Max => {
                            return self.recompute(gp, scope, "MIN and MAX (Phase 2)", None);
                        }
                        AggregateFunction::GroupConcat { .. } => {
                            return self.recompute(gp, scope, "GROUP_CONCAT (Phase 2)", None);
                        }
                        AggregateFunction::Sample => {
                            return self.recompute(gp, scope, "SAMPLE (Phase 2)", None);
                        }
                        AggregateFunction::Custom(_) => {
                            return self.recompute(gp, scope, "a custom aggregate", None);
                        }
                    };
                    if has_exists(expr) {
                        return self.recompute(gp, scope, "EXISTS in an aggregate", None);
                    }
                    specs.push((v.clone(), kind, *distinct, Some(expr.clone())));
                }
                AggregateExpression::Fold { .. } => {
                    return self.recompute(gp, scope, "FOLD", None);
                }
            }
        }
        let i = self.compile(inner, scope);
        let key_vars: Vec<u32> = variables.iter().map(|v| self.var(v.as_str())).collect();
        let keys: Vec<Option<usize>> = key_vars
            .iter()
            .map(|v| i.schema.iter().position(|x| x == v))
            .collect();
        let mut schema = key_vars.clone();
        let mut certain: Vec<u32> = key_vars
            .iter()
            .filter(|v| i.certain.contains(v))
            .copied()
            .collect();
        let mut aggs = Vec::new();
        for (v, kind, distinct, expr) in specs {
            let var = self.var(v.as_str());
            schema.push(var);
            if matches!(kind, AggKind::CountAll | AggKind::Count) {
                certain.push(var);
            }
            let column = match &expr {
                Some(Expression::Variable(x)) => {
                    let xv = self.var(x.as_str());
                    Some(i.schema.iter().position(|s| *s == xv))
                }
                _ => None,
            };
            aggs.push(AggSpec {
                kind,
                distinct,
                expr,
                column,
            });
        }
        let implicit = variables.is_empty();
        Node {
            op: Op::Group(Box::new(Group::new(i, keys, aggs, implicit))),
            schema,
            certain,
        }
    }
}

/// Compile a parsed query. `whole` names a reason to evaluate the whole query again on
/// every relevant commit (protections that depend on the data, or inference).
pub fn compile(q: &Query, whole: Option<&str>) -> Result<Compiled> {
    if let Some(f) = refused(q) {
        return Err(not_incremental(&f));
    }
    let (kind, pattern, dataset) = match q {
        Query::Select {
            pattern, dataset, ..
        } => (Kind::Select, pattern, dataset.as_ref()),
        Query::Ask {
            pattern, dataset, ..
        } => (Kind::Ask, pattern, dataset.as_ref()),
        Query::Construct {
            pattern, dataset, ..
        } => (Kind::Construct, pattern, dataset.as_ref()),
        Query::Describe { .. } => return Err(not_incremental("DESCRIBE")),
    };
    let mut c = C {
        vars: Vec::new(),
        index: FxHashMap::default(),
        hidden: 0,
    };
    let mut footprint = Footprint::default();
    footprint_of(pattern, &Scope::Default, &mut footprint);
    let mut root = match (whole, dataset) {
        (Some(reason), _) => {
            footprint.all = true;
            let mut n = c.recompute(pattern, &Scope::Default, reason, dataset);
            if let Op::Recompute(r) = &mut n.op {
                r.footprint.all = true;
            }
            n
        }
        (None, Some(_)) => {
            footprint.all = true;
            c.recompute(pattern, &Scope::Default, "a dataset clause (FROM)", dataset)
        }
        (None, None) => c.compile(pattern, &Scope::Default),
    };
    match (kind, q) {
        (Kind::Ask, _) => {
            root = Node {
                op: Op::Project(Box::new(Project {
                    inner: root,
                    cols: vec![],
                })),
                schema: vec![],
                certain: vec![],
            };
        }
        (Kind::Construct, Query::Construct { template, .. }) => {
            let slot = |c: &mut C, root: &Node, t: &TermPattern| -> TSlot {
                match t {
                    TermPattern::Variable(v) => {
                        let x = c.var(v.as_str());
                        TSlot::Var(root.schema.iter().position(|s| *s == x))
                    }
                    TermPattern::BlankNode(b) => TSlot::Blank(b.as_str().to_string()),
                    TermPattern::NamedNode(n) => TSlot::Const(Term::NamedNode(n.clone())),
                    TermPattern::Literal(l) => TSlot::Const(Term::Literal(l.clone())),
                    TermPattern::Triple(_) => TSlot::Var(None),
                }
            };
            let mut triples = Vec::new();
            for t in template {
                let s = slot(&mut c, &root, &t.subject);
                let p = match &t.predicate {
                    NamedNodePattern::NamedNode(n) => TSlot::Const(Term::NamedNode(n.clone())),
                    NamedNodePattern::Variable(v) => {
                        let x = c.var(v.as_str());
                        TSlot::Var(root.schema.iter().position(|s| *s == x))
                    }
                };
                let o = slot(&mut c, &root, &t.object);
                triples.push([s, p, o]);
            }
            let schema = vec![c.var("subject"), c.var("predicate"), c.var("object")];
            let t = Node {
                op: Op::Template(Box::new(Template {
                    inner: root,
                    triples,
                    seed: String::new(),
                    counts: ZSet::new(),
                })),
                schema: schema.clone(),
                certain: schema.clone(),
            };
            root = Node {
                op: Op::Distinct(Box::new(Distinct {
                    inner: t,
                    weights: ZSet::new(),
                })),
                schema: schema.clone(),
                certain: schema,
            };
        }
        _ => {}
    }
    let base = match q {
        Query::Select { base_iri, .. }
        | Query::Ask { base_iri, .. }
        | Query::Construct { base_iri, .. }
        | Query::Describe { base_iri, .. } => base_iri.clone(),
    };
    Ok(Compiled {
        kind,
        vars: c.vars,
        root,
        footprint,
        base,
        pattern: pattern.clone(),
        dataset: dataset.cloned(),
    })
}
