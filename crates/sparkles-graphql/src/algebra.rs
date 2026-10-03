//! The SPARQL algebra of each fetch group (§6.2), built as `spargebra` values, never as
//! text: membership of a type, filters (§5.5), orders (§5.6), pages, and the columns of
//! single-valued fields, membership flags and parent ids.

use crate::error::{Code, GqlError};
use crate::mapping::{FieldMap, Mapping, Target};
use crate::plan::{Group, GroupKind, OrderKey};
use crate::scalars::{RDF_TYPE, RDFS_SUBCLASS_OF, Scalar, input_term};
use oxrdf::{Literal, NamedNode, Term, Variable};
use serde_json::{Map, Value as J};
use spargebra::Query;
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression as E, Function, GraphPattern as P,
    OrderExpression, PropertyPathExpression,
};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};

pub fn var(s: &str) -> Variable {
    Variable::new_unchecked(s)
}

/// Fresh variables for one query.
#[derive(Default)]
pub struct Fresh(usize);

impl Fresh {
    pub fn next(&mut self, stem: &str) -> Variable {
        self.0 += 1;
        var(&format!("{stem}{}", self.0))
    }
}

fn tp(s: impl Into<TermPattern>, p: &NamedNode, o: impl Into<TermPattern>) -> TriplePattern {
    TriplePattern {
        subject: s.into(),
        predicate: NamedNodePattern::NamedNode(p.clone()),
        object: o.into(),
    }
}

fn bgp(patterns: Vec<TriplePattern>) -> P {
    P::Bgp { patterns }
}

fn join(a: P, b: P) -> P {
    match (&a, &b) {
        (P::Bgp { patterns }, _) if patterns.is_empty() => b,
        (_, P::Bgp { patterns }) if patterns.is_empty() => a,
        _ => P::Join {
            left: Box::new(a),
            right: Box::new(b),
        },
    }
}

fn left_join(a: P, b: P) -> P {
    P::LeftJoin {
        left: Box::new(a),
        right: Box::new(b),
        expression: None,
    }
}

fn filter(inner: P, e: E) -> P {
    P::Filter {
        expr: e,
        inner: Box::new(inner),
    }
}

fn and(a: E, b: E) -> E {
    E::And(Box::new(a), Box::new(b))
}

fn and_all(mut es: Vec<E>) -> Option<E> {
    let first = es.pop()?;
    Some(es.into_iter().rev().fold(first, |acc, e| and(e, acc)))
}

fn or_all(mut es: Vec<E>) -> E {
    match es.pop() {
        None => E::Literal(Literal::from(false)),
        Some(first) => es
            .into_iter()
            .rev()
            .fold(first, |acc, e| E::Or(Box::new(e), Box::new(acc))),
    }
}

fn not(e: E) -> E {
    E::Not(Box::new(e))
}

fn exists(p: P) -> E {
    E::Exists(Box::new(p))
}

fn call(f: Function, args: Vec<E>) -> E {
    E::FunctionCall(f, args)
}

fn term_expr(t: &Term) -> E {
    match t {
        Term::NamedNode(n) => E::NamedNode(n.clone()),
        Term::Literal(l) => E::Literal(l.clone()),
        t => E::Literal(Literal::new_simple_literal(t.to_string())),
    }
}

/// The members of a mapped type (§5.1): `?n rdf:type/rdfs:subClassOf* C`, as a join of
/// a type triple and a path, or `?n rdf:type C` without subclasses.
pub fn members(m: &Mapping, ty: &str, n: &Variable, fresh: &mut Fresh) -> P {
    let Some(t) = m.ty(ty) else {
        // an unmapped type has no members
        return P::Values {
            variables: vec![n.clone()],
            bindings: Vec::new(),
        };
    };
    let rdf_type = NamedNode::new_unchecked(RDF_TYPE);
    if !t.subclasses {
        return bgp(vec![tp(n.clone(), &rdf_type, t.class.clone())]);
    }
    let c = fresh.next("_c");
    join(
        bgp(vec![tp(n.clone(), &rdf_type, c.clone())]),
        P::Path {
            subject: c.into(),
            path: PropertyPathExpression::ZeroOrMore(Box::new(PropertyPathExpression::NamedNode(
                NamedNode::new_unchecked(RDFS_SUBCLASS_OF),
            ))),
            object: t.class.clone().into(),
        },
    )
}

fn bad(msg: impl Into<String>) -> GqlError {
    GqlError::new(Code::BadUserInput, msg)
}

/// The values of a field of node `n` as a pattern binding `x`.
fn field_values(f: &FieldMap, n: &Variable, x: &Variable) -> P {
    if f.inverse {
        bgp(vec![tp(x.clone(), &f.predicate, n.clone())])
    } else {
        bgp(vec![tp(n.clone(), &f.predicate, x.clone())])
    }
}

/// The comparison of one filter operator on value `x` of a field of `scalar` (or of an
/// enum).
fn op_expr(
    m: &Mapping,
    f: &FieldMap,
    op: &str,
    v: &J,
    x: &Variable,
) -> Result<Option<E>, GqlError> {
    let what = || format!("filter on {}: {op}", f.name);
    let one = |v: &J| -> Result<E, GqlError> {
        match &f.target {
            Target::Enum(e) => {
                let name = v.as_str().unwrap_or_default();
                m.enum_(e)
                    .and_then(|en| en.iri(name))
                    .map(|i| E::NamedNode(i.clone()))
                    .ok_or_else(|| bad(format!("{}: {name} is not a value of {e}", what())))
            }
            Target::Scalar(s) => input_term(v, *s, &m.prefixes)
                .map(|t| term_expr(&t))
                .map_err(|e| bad(format!("{}: {e}", what()))),
            Target::Object(_) => Err(bad(what())),
        }
    };
    let text = matches!(f.target, Target::Scalar(s) if s.is_text());
    // string comparisons read the lexical form of literals
    let lhs = || {
        if text {
            call(Function::Str, vec![E::Variable(x.clone())])
        } else {
            E::Variable(x.clone())
        }
    };
    let list = |v: &J| -> Result<Vec<E>, GqlError> {
        v.as_array()
            .ok_or_else(|| bad(format!("{} takes a list", what())))?
            .iter()
            .map(one)
            .collect()
    };
    let e = match op {
        "eq" => E::Equal(Box::new(lhs()), Box::new(one(v)?)),
        "ne" => not(E::Equal(Box::new(lhs()), Box::new(one(v)?))),
        "lt" => E::Less(Box::new(lhs()), Box::new(one(v)?)),
        "lte" => E::LessOrEqual(Box::new(lhs()), Box::new(one(v)?)),
        "gt" => E::Greater(Box::new(lhs()), Box::new(one(v)?)),
        "gte" => E::GreaterOrEqual(Box::new(lhs()), Box::new(one(v)?)),
        "in" => E::In(Box::new(lhs()), list(v)?),
        "notIn" => not(E::In(Box::new(lhs()), list(v)?)),
        "startsWith" => call(Function::StrStarts, vec![lhs(), one(v)?]),
        "contains" => call(Function::Contains, vec![lhs(), one(v)?]),
        "regex" => {
            // the flags are added by the caller
            call(Function::Regex, vec![lhs(), one(v)?])
        }
        "lang" => {
            let r = v.as_str().unwrap_or_default();
            call(
                Function::LangMatches,
                vec![
                    call(Function::Lang, vec![E::Variable(x.clone())]),
                    E::Literal(Literal::new_simple_literal(r)),
                ],
            )
        }
        "flags" | "exists" => return Ok(None),
        _ => return Err(bad(format!("unknown filter operator {op}"))),
    };
    // string operators apply to literals only
    Ok(Some(if text {
        and(call(Function::IsLiteral, vec![E::Variable(x.clone())]), e)
    } else {
        e
    }))
}

/// What a filter compiles to: patterns joined to the node's pattern (the fields of a
/// top-level `and`, where "some value satisfies" is a join) and expressions.
#[derive(Default)]
pub struct Compiled {
    pub joins: Vec<P>,
    pub exprs: Vec<E>,
}

/// Compile a `TFilter` on node `n` of type `ty`. `top` allows joins.
pub fn compile_filter(
    m: &Mapping,
    ty: &str,
    flt: &J,
    n: &Variable,
    fresh: &mut Fresh,
    top: bool,
    out: &mut Compiled,
) -> Result<(), GqlError> {
    let Some(obj) = flt.as_object() else {
        return Ok(());
    };
    let t = m.ty(ty).ok_or_else(|| bad(format!("no filter on {ty}")))?;
    for (k, v) in obj {
        if v.is_null() {
            continue;
        }
        match k.as_str() {
            "and" => {
                for f in v.as_array().into_iter().flatten() {
                    compile_filter(m, ty, f, n, fresh, top, out)?;
                }
            }
            "or" => {
                let mut alts = Vec::new();
                for f in v.as_array().into_iter().flatten() {
                    alts.push(filter_expr(m, ty, f, n, fresh)?);
                }
                out.exprs.push(or_all(alts));
            }
            "not" => out.exprs.push(not(filter_expr(m, ty, v, n, fresh)?)),
            "id" => {
                let o = v.as_object().cloned().unwrap_or_default();
                for (op, val) in o {
                    if val.is_null() {
                        continue;
                    }
                    let id = |v: &J| {
                        input_term(v, Scalar::Id, &m.prefixes)
                            .map(|t| term_expr(&t))
                            .map_err(|e| bad(format!("filter on id: {e}")))
                    };
                    let list = |v: &J| -> Result<Vec<E>, GqlError> {
                        v.as_array()
                            .ok_or_else(|| bad("filter on id: in takes a list"))?
                            .iter()
                            .map(id)
                            .collect()
                    };
                    let nv = E::Variable(n.clone());
                    out.exprs.push(match op.as_str() {
                        "eq" => E::SameTerm(Box::new(nv), Box::new(id(&val)?)),
                        "in" => E::In(Box::new(nv), list(&val)?),
                        "notIn" => not(E::In(Box::new(nv), list(&val)?)),
                        o => return Err(bad(format!("unknown id filter operator {o}"))),
                    });
                }
            }
            name => {
                let f = t
                    .field(name)
                    .ok_or_else(|| bad(format!("{ty} has no field {name}")))?;
                field_filter(m, f, v, n, fresh, top, out)?;
            }
        }
    }
    Ok(())
}

fn field_filter(
    m: &Mapping,
    f: &FieldMap,
    v: &J,
    n: &Variable,
    fresh: &mut Fresh,
    top: bool,
    out: &mut Compiled,
) -> Result<(), GqlError> {
    let x = fresh.next("_x");
    if let Target::Object(target) = &f.target {
        // some value satisfies the nested filter
        let mut inner = Compiled::default();
        compile_filter(m, target, v, &x, fresh, top, &mut inner)?;
        let mut p = field_values(f, n, &x);
        for j in inner.joins {
            p = join(p, j);
        }
        if let Some(e) = and_all(inner.exprs) {
            p = filter(p, e);
        }
        if top {
            out.joins.push(p);
        } else {
            out.exprs.push(exists(p));
        }
        return Ok(());
    }
    let ops = v
        .as_object()
        .ok_or_else(|| bad(format!("filter on {}: expected an object", f.name)))?;
    let mut conds = Vec::new();
    let mut want_exists = None;
    for (op, val) in ops {
        if val.is_null() {
            continue;
        }
        if op == "exists" {
            want_exists = val.as_bool();
            continue;
        }
        if let Some(mut e) = op_expr(m, f, op, val, &x)? {
            if op == "regex"
                && let Some(fl) = ops.get("flags").and_then(J::as_str)
                && let E::And(_, r) = &mut e
                && let E::FunctionCall(Function::Regex, args) = r.as_mut()
            {
                args.push(E::Literal(Literal::new_simple_literal(fl)));
            }
            conds.push(e);
        }
    }
    let values = field_values(f, n, &x);
    match want_exists {
        Some(false) => out.exprs.push(not(exists(values.clone()))),
        Some(true) if conds.is_empty() => {
            if top {
                out.joins.push(values.clone());
            } else {
                out.exprs.push(exists(values.clone()));
            }
        }
        _ => {}
    }
    if let Some(c) = and_all(conds) {
        let p = filter(values, c);
        if top {
            out.joins.push(p);
        } else {
            out.exprs.push(exists(p));
        }
    }
    Ok(())
}

/// A filter as one expression on `n` (inside `or` and `not`).
fn filter_expr(
    m: &Mapping,
    ty: &str,
    flt: &J,
    n: &Variable,
    fresh: &mut Fresh,
) -> Result<E, GqlError> {
    let mut c = Compiled::default();
    compile_filter(m, ty, flt, n, fresh, false, &mut c)?;
    Ok(and_all(c.exprs).unwrap_or(E::Literal(Literal::from(true))))
}

/// `pattern` with a filter on node `n` of type `ty` applied.
fn with_filter(
    m: &Mapping,
    pattern: P,
    ty: &str,
    flt: Option<&J>,
    n: &Variable,
    fresh: &mut Fresh,
) -> Result<P, GqlError> {
    let Some(flt) = flt else { return Ok(pattern) };
    let mut c = Compiled::default();
    compile_filter(m, ty, flt, n, fresh, true, &mut c)?;
    let mut p = pattern;
    for j in c.joins {
        p = join(p, j);
    }
    if let Some(e) = and_all(c.exprs) {
        p = filter(p, e);
    }
    Ok(p)
}

/// The ids of parent nodes as a pattern binding `p`: a `VALUES` table of the IRIs, and
/// one branch per blank node, bound through the query's initial bindings (blank nodes
/// cannot be written in `VALUES`).
pub fn seed(p: &Variable, ids: &[Term], bindings: &mut Vec<(String, Term)>) -> P {
    let mut rows = Vec::new();
    let mut blanks = Vec::new();
    for t in ids {
        match t {
            Term::NamedNode(n) => rows.push(vec![Some(GroundTerm::NamedNode(n.clone()))]),
            Term::BlankNode(_) => blanks.push(t.clone()),
            _ => {}
        }
    }
    let mut out = P::Values {
        variables: vec![p.clone()],
        bindings: rows,
    };
    for b in blanks {
        let name = format!("_b{}", bindings.len());
        bindings.push((name.clone(), b));
        let branch = P::Extend {
            inner: Box::new(bgp(Vec::new())),
            variable: p.clone(),
            expression: E::Variable(var(&name)),
        };
        out = P::Union {
            left: Box::new(out),
            right: Box::new(branch),
        };
    }
    out
}

fn order_exprs(keys: &[OrderKey], cols: &[Option<Variable>], n: &Variable) -> Vec<OrderExpression> {
    keys.iter()
        .zip(cols)
        .map(|(k, c)| {
            let e = E::Variable(c.clone().unwrap_or_else(|| n.clone()));
            if k.desc {
                OrderExpression::Desc(e)
            } else {
                OrderExpression::Asc(e)
            }
        })
        .collect()
}

/// The ordered (and, for a page, sliced) nodes of `base`: `?n` alone, or with `?p` for
/// child groups. Keys with a predicate sort by the smallest value (the largest for a
/// descending key), so a node with several values appears once.
fn ordered(
    base: P,
    keys: &[OrderKey],
    n: &Variable,
    parent: Option<&Variable>,
    slice: Option<(usize, Option<usize>)>,
    fresh: &mut Fresh,
) -> (P, Vec<Option<Variable>>) {
    let mut group_vars: Vec<Variable> = parent.into_iter().cloned().collect();
    group_vars.push(n.clone());
    let mut cols = Vec::new();
    let mut inner = base;
    let mut aggs = Vec::new();
    for k in keys {
        match &k.pred {
            None => cols.push(None),
            Some(p) => {
                let kv = fresh.next("_k");
                let o = fresh.next("_o");
                inner = left_join(inner, bgp(vec![tp(n.clone(), p, kv.clone())]));
                aggs.push((
                    o.clone(),
                    AggregateExpression::FunctionCall {
                        name: if k.desc {
                            AggregateFunction::Max
                        } else {
                            AggregateFunction::Min
                        },
                        expr: E::Variable(kv),
                        distinct: false,
                    },
                ));
                cols.push(Some(o));
            }
        }
    }
    let mut out_vars = group_vars.clone();
    out_vars.extend(cols.iter().flatten().cloned());
    let grouped = if aggs.is_empty() {
        P::Distinct {
            inner: Box::new(P::Project {
                inner: Box::new(inner),
                variables: group_vars.clone(),
            }),
        }
    } else {
        P::Group {
            inner: Box::new(inner),
            variables: group_vars.clone(),
            aggregates: aggs,
        }
    };
    let mut order = Vec::new();
    if let Some(p) = parent {
        order.push(OrderExpression::Asc(E::Variable(p.clone())));
    }
    order.extend(order_exprs(keys, &cols, n));
    let mut p = P::Project {
        inner: Box::new(P::OrderBy {
            inner: Box::new(grouped),
            expression: order,
        }),
        variables: out_vars,
    };
    if let Some((start, length)) = slice {
        p = P::Slice {
            inner: Box::new(p),
            start,
            length,
        };
    }
    (p, cols)
}

/// A built group query, with the initial bindings its blank parent ids need and the
/// names of its columns.
pub struct Built {
    pub query: Query,
    pub bindings: Vec<(String, Term)>,
    /// the node (`n`), parent (`p`), single (`v0…`) and flag (`t0…`) variables
    pub singles: Vec<Variable>,
    pub flags: Vec<Variable>,
}

pub fn select(pattern: P) -> Query {
    Query::Select {
        dataset: None,
        pattern,
        base_iri: None,
    }
}

/// The query of a group that yields nodes (lookup, node, collection, child). `ids` are
/// the parent nodes of a child group; `slice` the page of a collection.
pub fn node_query(
    m: &Mapping,
    g: &Group,
    ids: &[Term],
    slice: Option<(usize, Option<usize>)>,
) -> Result<Built, GqlError> {
    let n = var("n");
    let p = var("p");
    let mut fresh = Fresh::default();
    let mut bindings = Vec::new();
    let (page, order_cols, keys, parent): (P, Vec<Option<Variable>>, Vec<OrderKey>, bool) =
        match &g.kind {
            GroupKind::Lookup { ty, id } => {
                let base = match id {
                    Term::BlankNode(_) => seed(&n, std::slice::from_ref(id), &mut bindings),
                    _ => seed(&n, std::slice::from_ref(id), &mut bindings),
                };
                let mem = members(m, ty, &n, &mut fresh);
                (filter(base, exists(mem)), Vec::new(), Vec::new(), false)
            }
            GroupKind::Node { id } => {
                let base = seed(&n, std::slice::from_ref(id), &mut bindings);
                let (a, b) = (fresh.next("_a"), fresh.next("_b"));
                let any = P::Union {
                    left: Box::new(bgp(vec![TriplePattern {
                        subject: n.clone().into(),
                        predicate: NamedNodePattern::Variable(a.clone()),
                        object: b.clone().into(),
                    }])),
                    right: Box::new(bgp(vec![TriplePattern {
                        subject: b.into(),
                        predicate: NamedNodePattern::Variable(a),
                        object: n.clone().into(),
                    }])),
                };
                (filter(base, exists(any)), Vec::new(), Vec::new(), false)
            }
            GroupKind::Collection {
                ty,
                filter: flt,
                order,
                ..
            } => {
                let base = members(m, ty, &n, &mut fresh);
                let base = with_filter(m, base, ty, flt.as_ref(), &n, &mut fresh)?;
                let (pg, cols) = ordered(base, order, &n, None, slice, &mut fresh);
                (pg, cols, order.clone(), false)
            }
            GroupKind::Child {
                pred,
                inverse,
                filter: flt,
                order,
            } => {
                let s = seed(&p, ids, &mut bindings);
                let edge = if *inverse {
                    bgp(vec![tp(n.clone(), pred, p.clone())])
                } else {
                    bgp(vec![tp(p.clone(), pred, n.clone())])
                };
                let mut base = join(s, edge);
                if let Some((ty, f)) = flt {
                    base = with_filter(m, base, ty, Some(f), &n, &mut fresh)?;
                }
                let (pg, cols) = ordered(base, order, &n, Some(&p), None, &mut fresh);
                (pg, cols, order.clone(), true)
            }
            GroupKind::Count { .. } | GroupKind::Values => {
                return Err(GqlError::new(Code::Internal, "not a node group"));
            }
        };
    // single-valued fields as OPTIONALs, membership flags as EXISTS
    let mut pat = page;
    let mut singles = Vec::new();
    for (i, pr) in g.singles.iter().enumerate() {
        let v = var(&format!("v{i}"));
        pat = left_join(pat, bgp(vec![tp(n.clone(), pr, v.clone())]));
        singles.push(v);
    }
    let mut flags = Vec::new();
    for (i, ty) in g.flags.iter().enumerate() {
        let t = var(&format!("t{i}"));
        pat = P::Extend {
            inner: Box::new(pat),
            variable: t.clone(),
            expression: exists(members(m, ty, &n, &mut fresh)),
        };
        flags.push(t);
    }
    let mut vars = Vec::new();
    if parent {
        vars.push(p.clone());
    }
    vars.push(n.clone());
    vars.extend(singles.iter().cloned());
    vars.extend(flags.iter().cloned());
    let has_order = !keys.is_empty();
    let mut order = Vec::new();
    if parent {
        order.push(OrderExpression::Asc(E::Variable(p.clone())));
    }
    if has_order {
        order.extend(order_exprs(&keys, &order_cols, &n));
    }
    let mut body = pat;
    if !order.is_empty() {
        body = P::OrderBy {
            inner: Box::new(body),
            expression: order,
        };
    }
    Ok(Built {
        query: select(P::Project {
            inner: Box::new(body),
            variables: vars,
        }),
        bindings,
        singles,
        flags,
    })
}

/// The values group of a node group: one `UNION` branch per field, tagged with its
/// number.
pub fn values_query(vfields: &[NamedNode], ids: &[Term]) -> (Query, Vec<(String, Term)>) {
    let (p, f, v) = (var("p"), var("f"), var("v"));
    let mut bindings = Vec::new();
    let s = seed(&p, ids, &mut bindings);
    let mut branches: Option<P> = None;
    for (i, pr) in vfields.iter().enumerate() {
        let b = P::Extend {
            inner: Box::new(bgp(vec![tp(p.clone(), pr, v.clone())])),
            variable: f.clone(),
            expression: E::Literal(Literal::from(i as i64)),
        };
        branches = Some(match branches {
            None => b,
            Some(acc) => P::Union {
                left: Box::new(acc),
                right: Box::new(b),
            },
        });
    }
    let body = join(s, branches.unwrap_or_else(|| bgp(Vec::new())));
    let q = select(P::Distinct {
        inner: Box::new(P::Project {
            inner: Box::new(P::OrderBy {
                inner: Box::new(body),
                expression: vec![
                    OrderExpression::Asc(E::Variable(p.clone())),
                    OrderExpression::Asc(E::Variable(f.clone())),
                    OrderExpression::Asc(E::Variable(v.clone())),
                ],
            }),
            variables: vec![p, f, v],
        }),
    });
    (q, bindings)
}

/// `totalCount`: the distinct members that match the filter.
pub fn count_query(m: &Mapping, ty: &str, flt: Option<&J>) -> Result<Query, GqlError> {
    let n = var("n");
    let mut fresh = Fresh::default();
    let base = members(m, ty, &n, &mut fresh);
    let base = with_filter(m, base, ty, flt, &n, &mut fresh)?;
    let c = var("c");
    Ok(select(P::Project {
        inner: Box::new(P::Group {
            inner: Box::new(base),
            variables: Vec::new(),
            aggregates: vec![(
                c.clone(),
                AggregateExpression::FunctionCall {
                    name: AggregateFunction::Count,
                    expr: E::Variable(n),
                    distinct: true,
                },
            )],
        }),
        variables: vec![c],
    }))
}

/// Unused filter arguments are refused before a group runs.
pub fn check_filter(m: &Mapping, ty: &str, flt: &Map<String, J>) -> Result<(), GqlError> {
    let mut fresh = Fresh::default();
    let mut c = Compiled::default();
    compile_filter(
        m,
        ty,
        &J::Object(flt.clone()),
        &var("n"),
        &mut fresh,
        true,
        &mut c,
    )
}
