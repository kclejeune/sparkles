//! Localizable SHACL-SPARQL (C10 §6.3): which edges a SPARQL-based constraint reads,
//! relative to its focus node, when every triple pattern of its query is anchored there.
//!
//! The patterns of a query form a graph whose nodes are its variables, blank nodes and
//! constants. Starting from the pre-bound `$this` (and `$value`, which the shape's path
//! reaches), each pattern that shares a node with the part reached so far extends it: a
//! pattern `?a p ?b` reached at `?a` over the path `π` reads the `p` edges whose subject
//! is a node `π` reaches from the focus node, and reaches `?b` over `π/p`. Every edge a
//! solution matches is then reached from the focus node over edges the same solution
//! matches, so in the state the solution belongs to, a changed edge it matches is found
//! from the focus node by the reversed path. Those reads become dependencies of the
//! shape, like the reads of its paths ([`crate::incremental`]).
//!
//! A query is not localizable, and its shape is validated in full on every write, when
//! some pattern is not anchored (it may match anywhere), or when it has a subquery,
//! `GRAPH`, `SERVICE`, `VALUES`, `MINUS`, a negated property set inside a longer path,
//! or more than [`MAX_PREFIXES`] ways to reach one node. Negation (`FILTER NOT EXISTS`)
//! and `OPTIONAL` are localizable when their patterns are anchored by the enclosing
//! ones. A variable bound only by one branch of a `UNION`, or only inside an
//! `OPTIONAL`, does not anchor the patterns after it, because a solution may leave it
//! unbound.

use crate::incremental::{nnf, steps};
use crate::path::PropertyPath;
use oxrdf::NamedNode;
use rustc_hash::FxHashMap;
use spargebra::Query;
use spargebra::algebra::{
    AggregateExpression, Expression, GraphPattern, OrderExpression, PropertyPathExpression,
};
use spargebra::term::{NamedNodePattern, TermPattern};

/// Ways to reach one node of a query past which it is not localized.
pub(crate) const MAX_PREFIXES: usize = 16;

/// One read of a query: the edges with predicate `pred` (any, for `None`) whose subject
/// (`out`) or object is a node `prefix` reaches from the focus node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Read {
    pub prefix: Vec<PropertyPath>,
    pub pred: Option<NamedNode>,
    pub out: bool,
}

/// Not localizable.
#[derive(Debug)]
struct Global;

type R<T> = Result<T, Global>;

/// The paths from the focus node to each node reached; `None`: reached over a
/// variable predicate, so no path leads to it.
type Reach = FxHashMap<String, Option<Vec<Vec<PropertyPath>>>>;

/// The edges a query reads, when they are all anchored at the pre-bound variables
/// `anchors` (each with the paths from the focus node to its value).
pub(crate) fn reads(q: &Query, anchors: &[(&str, Vec<PropertyPath>)]) -> Option<Vec<Read>> {
    let mut gp = match q {
        Query::Select { pattern, .. }
        | Query::Ask { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. } => pattern,
    };
    // the solution modifiers of the query itself (a nested one is a subquery)
    loop {
        gp = match gp {
            GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner }
            | GraphPattern::Slice { inner, .. } => inner,
            _ => break,
        };
    }
    let mut reach = Reach::default();
    for (v, prefix) in anchors {
        reach.insert(format!("?{v}"), Some(vec![prefix.clone()]));
    }
    let (_, mut out) = analyze(gp, &reach).ok()?;
    let mut seen = Vec::with_capacity(out.len());
    out.retain(|r| {
        let new = !seen.contains(r);
        if new {
            seen.push(r.clone());
        }
        new
    });
    Some(out)
}

/// A pattern as an edge of the query graph: from `a` to `b` over `path` (`None`: any
/// one predicate).
struct Edge {
    a: String,
    b: String,
    path: Option<PropertyPath>,
}

fn key(t: &TermPattern) -> R<String> {
    Ok(match t {
        TermPattern::Variable(v) => format!("?{}", v.as_str()),
        // a blank node of a pattern is a variable
        TermPattern::BlankNode(b) => format!("?_:{}", b.as_str()),
        TermPattern::NamedNode(n) => n.to_string(),
        TermPattern::Literal(l) => l.to_string(),
        TermPattern::Triple(_) => return Err(Global),
    })
}

/// A property path without negated property sets.
fn path(p: &PropertyPathExpression) -> R<PropertyPath> {
    use PropertyPathExpression as E;
    let b = |x: &E| path(x).map(Box::new);
    Ok(match p {
        E::NamedNode(n) => PropertyPath::Predicate(n.clone()),
        E::Reverse(x) => PropertyPath::Inverse(b(x)?),
        E::Sequence(x, y) => PropertyPath::Sequence(vec![path(x)?, path(y)?]),
        E::Alternative(x, y) => PropertyPath::Alternative(vec![path(x)?, path(y)?]),
        E::ZeroOrMore(x) => PropertyPath::ZeroOrMore(b(x)?),
        E::OneOrMore(x) => PropertyPath::OneOrMore(b(x)?),
        E::ZeroOrOne(x) => PropertyPath::ZeroOrOne(b(x)?),
        E::NegatedPropertySet(_) | E::Range { .. } => return Err(Global),
    })
}

/// The edge of a path pattern: a negated property set alone (or reversed) reads one
/// edge with any predicate.
fn path_edge(s: &TermPattern, p: &PropertyPathExpression, o: &TermPattern) -> R<Edge> {
    let (a, b) = (key(s)?, key(o)?);
    Ok(match p {
        PropertyPathExpression::NegatedPropertySet(_) => Edge { a, b, path: None },
        PropertyPathExpression::Reverse(x)
            if matches!(**x, PropertyPathExpression::NegatedPropertySet(_)) =>
        {
            Edge {
                a: b,
                b: a,
                path: None,
            }
        }
        p => Edge {
            a,
            b,
            path: Some(path(p)?),
        },
    })
}

/// The reads of an edge from its end `from` (its start when `forward`), reached over
/// `prefixes`; `None` when no path leads there.
fn reads_from(
    e: &Edge,
    forward: bool,
    prefixes: &Option<Vec<Vec<PropertyPath>>>,
    out: &mut Vec<Read>,
) -> R<()> {
    let prefixes = prefixes.as_ref().ok_or(Global)?;
    for pre in prefixes {
        match &e.path {
            None => out.push(Read {
                prefix: pre.clone(),
                pred: None,
                out: forward,
            }),
            Some(p) => {
                for (rest, pred, dir) in steps(&nnf(p, !forward)) {
                    let mut prefix = pre.clone();
                    prefix.extend(rest);
                    out.push(Read {
                        prefix,
                        pred: Some(pred),
                        out: dir == crate::incremental::Dir::Out,
                    });
                }
            }
        }
    }
    Ok(())
}

/// The paths to the far end of an edge, from the paths to its near end.
fn extend(
    e: &Edge,
    forward: bool,
    prefixes: &Option<Vec<Vec<PropertyPath>>>,
) -> Option<Vec<Vec<PropertyPath>>> {
    let (p, prefixes) = (e.path.as_ref()?, prefixes.as_ref()?);
    let step = nnf(p, !forward);
    Some(
        prefixes
            .iter()
            .map(|pre| {
                let mut v = pre.clone();
                v.push(step.clone());
                v
            })
            .collect(),
    )
}

/// Reach the edges of a basic graph pattern from `reach`, every one of them.
fn bgp(edges: &[Edge], reach: &Reach) -> R<(Reach, Vec<Read>)> {
    let mut reach = reach.clone();
    let mut out = Vec::new();
    let mut done = vec![false; edges.len()];
    loop {
        let mut progress = false;
        for (i, e) in edges.iter().enumerate() {
            if done[i] {
                continue;
            }
            match (reach.get(&e.a).cloned(), reach.get(&e.b).cloned()) {
                (None, None) => continue,
                (Some(pa), None) => {
                    reads_from(e, true, &pa, &mut out)?;
                    reach.insert(e.b.clone(), extend(e, true, &pa));
                }
                (None, Some(pb)) => {
                    reads_from(e, false, &pb, &mut out)?;
                    reach.insert(e.a.clone(), extend(e, false, &pb));
                }
                // neither path passes through this edge
                (Some(pa), Some(pb)) => {
                    if pa.is_some() {
                        reads_from(e, true, &pa, &mut out)?;
                    } else {
                        reads_from(e, false, &pb, &mut out)?;
                    }
                }
            }
            done[i] = true;
            progress = true;
        }
        if !progress {
            break;
        }
    }
    if done.contains(&false) {
        return Err(Global);
    }
    if reach
        .values()
        .any(|p| p.as_ref().is_some_and(|p| p.len() > MAX_PREFIXES))
    {
        return Err(Global);
    }
    Ok((reach, out))
}

/// The nodes reached on both sides, with the paths of either.
fn both(l: &Reach, r: &Reach) -> Reach {
    let mut out = Reach::default();
    for (k, pl) in l {
        let Some(pr) = r.get(k) else { continue };
        let merged = match (pl, pr) {
            (Some(a), Some(b)) => {
                let mut v = a.clone();
                for p in b {
                    if !v.contains(p) {
                        v.push(p.clone());
                    }
                }
                Some(v)
            }
            _ => None,
        };
        out.insert(k.clone(), merged);
    }
    out
}

fn analyze(gp: &GraphPattern, reach: &Reach) -> R<(Reach, Vec<Read>)> {
    use GraphPattern as G;
    match gp {
        G::Bgp { patterns } => {
            let edges = patterns
                .iter()
                .map(|t| {
                    let (a, b) = (key(&t.subject)?, key(&t.object)?);
                    Ok(match &t.predicate {
                        NamedNodePattern::NamedNode(n) => Edge {
                            a,
                            b,
                            path: Some(PropertyPath::Predicate(n.clone())),
                        },
                        NamedNodePattern::Variable(_) => Edge { a, b, path: None },
                    })
                })
                .collect::<R<Vec<_>>>()?;
            bgp(&edges, reach)
        }
        G::Path {
            subject,
            path,
            object,
        } => bgp(&[path_edge(subject, path, object)?], reach),
        G::Join { left, right } => {
            let ordered = |a: &GraphPattern, b: &GraphPattern| -> R<(Reach, Vec<Read>)> {
                let (r1, mut d1) = analyze(a, reach)?;
                let (r2, d2) = analyze(b, &r1)?;
                d1.extend(d2);
                Ok((r2, d1))
            };
            ordered(left, right).or_else(|_| ordered(right, left))
        }
        G::LeftJoin {
            left,
            right,
            expression,
        } => {
            let (r1, mut d) = analyze(left, reach)?;
            let (r2, d2) = analyze(right, &r1)?;
            d.extend(d2);
            if let Some(e) = expression {
                exists(e, &r2, &mut d)?;
            }
            // the optional part may leave its variables unbound
            Ok((r1, d))
        }
        G::Filter { expr, inner } => {
            let (r, mut d) = analyze(inner, reach)?;
            exists(expr, &r, &mut d)?;
            Ok((r, d))
        }
        G::Union { left, right } => {
            let (r1, mut d) = analyze(left, reach)?;
            let (r2, d2) = analyze(right, reach)?;
            d.extend(d2);
            Ok((both(&r1, &r2), d))
        }
        G::Extend {
            inner,
            variable,
            expression,
        } => {
            let (mut r, mut d) = analyze(inner, reach)?;
            exists(expression, &r, &mut d)?;
            if let Expression::Variable(v) = expression
                && let Some(p) = r.get(&format!("?{}", v.as_str())).cloned()
            {
                r.insert(format!("?{}", variable.as_str()), p);
            }
            Ok((r, d))
        }
        G::OrderBy { inner, expression } => {
            let (r, mut d) = analyze(inner, reach)?;
            for e in expression {
                let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = e;
                exists(e, &r, &mut d)?;
            }
            Ok((r, d))
        }
        G::Group {
            inner,
            variables,
            aggregates,
        } => {
            let (r, mut d) = analyze(inner, reach)?;
            for (_, a) in aggregates {
                match a {
                    AggregateExpression::CountSolutions { .. } => {}
                    AggregateExpression::FunctionCall { expr, .. } => exists(expr, &r, &mut d)?,
                    // ARQ's FOLD reads several expressions in its own order
                    AggregateExpression::Fold { .. } => return Err(Global),
                }
            }
            // only the grouping variables stay in scope
            let kept = variables
                .iter()
                .filter_map(|v| {
                    let k = format!("?{}", v.as_str());
                    r.get(&k).cloned().map(|p| (k, p))
                })
                .chain(reach.iter().map(|(k, p)| (k.clone(), p.clone())))
                .collect();
            Ok((kept, d))
        }
        // subqueries, other graphs, inline data and MINUS
        #[allow(unreachable_patterns)]
        _ => Err(Global),
    }
}

/// The reads of the `EXISTS` patterns of an expression, anchored by `reach`.
fn exists(e: &Expression, reach: &Reach, out: &mut Vec<Read>) -> R<()> {
    use Expression as E;
    match e {
        E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => Ok(()),
        E::Exists(p) => {
            let (_, d) = analyze(p, reach)?;
            out.extend(d);
            Ok(())
        }
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
            exists(a, reach, out)?;
            exists(b, reach, out)
        }
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => exists(a, reach, out),
        E::In(a, xs) => {
            exists(a, reach, out)?;
            xs.iter().try_for_each(|x| exists(x, reach, out))
        }
        E::If(a, b, c) => {
            exists(a, reach, out)?;
            exists(b, reach, out)?;
            exists(c, reach, out)
        }
        E::Coalesce(xs) | E::FunctionCall(_, xs) => {
            xs.iter().try_for_each(|x| exists(x, reach, out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> Query {
        spargebra::SparqlParser::new()
            .with_prefix("ex", "http://ex.org/")
            .unwrap()
            .parse_query(text)
            .unwrap()
    }

    fn reads_of(text: &str) -> Option<Vec<String>> {
        let r = reads(&q(text), &[("this", Vec::new())])?;
        let mut v: Vec<String> = r
            .iter()
            .map(|r| {
                let pre: Vec<String> = r.prefix.iter().map(|p| p.to_sparql()).collect();
                format!(
                    "{} {} {}",
                    pre.join("/"),
                    r.pred.as_ref().map_or("*".into(), |p| p
                        .as_str()
                        .trim_start_matches("http://ex.org/")
                        .to_string()),
                    if r.out { "out" } else { "in" }
                )
            })
            .collect();
        v.sort();
        Some(v)
    }

    #[test]
    fn anchored_queries_are_localized() {
        // uniqueness of a key
        assert_eq!(
            reads_of(
                "SELECT $this WHERE { $this ex:key ?k . ?other ex:key ?k . FILTER(?other != $this) }"
            )
            .unwrap(),
            [" key out", "<http://ex.org/key> key in"]
        );
        // negation anchored by the enclosing pattern, and a path
        assert_eq!(
            reads_of("SELECT $this WHERE { $this ex:a/ex:b ?x FILTER NOT EXISTS { ?x ex:c ?y } }")
                .unwrap(),
            [
                " a out",
                "<http://ex.org/a> b out",
                "<http://ex.org/a>/<http://ex.org/b> c out"
            ]
        );
        // a variable predicate at the focus node
        assert_eq!(
            reads_of("SELECT $this WHERE { $this ?p ?o }").unwrap(),
            [" * out"]
        );
        // aggregates over anchored patterns
        assert!(
            reads_of(
                "SELECT $this (COUNT(?x) AS ?n) WHERE { $this ex:p ?x } GROUP BY $this HAVING (?n > 2)"
            )
            .is_some()
        );
        // a query that reads nothing
        assert_eq!(
            reads_of("SELECT $this WHERE { }").unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn unanchored_queries_are_not() {
        for text in [
            "SELECT $this WHERE { ?x ex:p ?y }",
            "SELECT $this WHERE { $this ex:p ?x . ?y ex:q ?z }",
            "SELECT $this WHERE { FILTER NOT EXISTS { ?y ex:q ?z } }",
            "SELECT $this WHERE { GRAPH ?g { $this ex:p ?x } }",
            "SELECT $this WHERE { { SELECT ?x WHERE { ?x ex:p ?y } } $this ex:q ?x }",
            // ?x may be unbound: the pattern after the union matches anywhere
            "SELECT $this WHERE { { $this ex:p ?x } UNION { $this ex:q ?y } ?x ex:r ?z }",
            // ?x may be unbound after OPTIONAL
            "SELECT $this WHERE { $this ex:p ?y OPTIONAL { $this ex:q ?x } ?x ex:r ?z }",
            // no path reaches ?o over a variable predicate
            "SELECT $this WHERE { $this ?p ?o . ?o ex:r ?z }",
            "SELECT $this WHERE { $this !ex:p/ex:q ?o }",
        ] {
            assert!(reads_of(text).is_none(), "{text}");
        }
        // both branches bind ?x
        assert!(
            reads_of("SELECT $this WHERE { { $this ex:p ?x } UNION { $this ex:q ?x } ?x ex:r ?z }")
                .is_some()
        );
    }
}
