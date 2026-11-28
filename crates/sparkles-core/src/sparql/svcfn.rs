//! The management functions of Jena's service enhancer over the cache of remote SERVICE
//! results (spec G10, roadmap item 38).
//!
//! `?id se:cacheLs (?service ?query ?binding ?rows ?bytes ?ageSeconds)` lists the
//! entries of the caller's own scope. The subject is the entry's id, and the object list
//! takes up to six positions, of which every one is optional and each may be a variable
//! or a term that the entry must match. The first three are Jena's, the endpoint, the
//! pattern and the values substituted into it, and the last three are Sparkles' own, the
//! entry's solutions, its bytes and its age in whole seconds. The endpoint is listed
//! without the user name, password, query string and fragment of its URL.
//!
//! `se:cacheRm()` removes every entry of the caller's scope and `se:cacheRm(?id)` the
//! entry with that id, each returning how many it removed. Removal changes what other
//! queries of the same caller fetch, so a query may call it only when its options grant
//! it ([`QueryOptions::service_cache_mgmt`](super::QueryOptions::service_cache_mgmt)),
//! which the server does for a caller with `admin` on the dataset when management is
//! turned on, as Jena's `enableMgmt` context symbol does. Any other query that calls it
//! by name fails when it is planned, including a call inside an EXISTS pattern that no
//! row reaches. A call inside a SERVICE to a remote endpoint is the endpoint's and is
//! not refused. A call through `fn:apply` or `afn:eval`, whose function the planner
//! cannot see, is an error of the expression and removes nothing, with the right or
//! without it. Neither function reaches another caller's entries, since both compare the
//! entry's scope with the caller's as a whole. A removal whose files cannot be synced
//! fails the query with that error once the expression has run.

use super::ctx::Ctx;
use super::plan::{Kind, Node, PT, Planner};
use super::table::{Table, VarId};
use crate::error::{Error, Result};
use crate::id::Id;
use oxrdf::{Literal, NamedNode, Term};
use spargebra::term::TermPattern;

/// `se:cacheLs`.
pub const CACHE_LS: &str = "http://jena.apache.org/service-enhancer#cacheLs";
/// `se:cacheRm`.
pub const CACHE_RM: &str = "http://jena.apache.org/service-enhancer#cacheRm";

/// How the leaf of an `se:cacheLs` call starts its description, by which the query
/// result cache knows to keep none of its results.
pub(super) const LS_DESC: &str = "se:cacheLs:";

/// The positions of the object list of `se:cacheLs`.
const POSITIONS: usize = 6;

/// Refuse a query that calls `se:cacheRm` without the right to.
pub(super) fn check_rm(ctx: &Ctx) -> Result<()> {
    if ctx.service_cache_mgmt {
        Ok(())
    } else {
        Err(Error::NotPermitted(
            "se:cacheRm needs admin on the dataset with the service enhancer's management \
             turned on (serve --service-cache-mgmt)"
                .into(),
        ))
    }
}

/// Walk every sub-expression of `e` and run the admission check on each. The planner's
/// `compile` calls [`admit`] one expression at a time, but a deferred OPTIONAL condition
/// is not compiled until execution, so this walk catches a `se:cacheRm` call buried in
/// the tree before the condition is deferred.
pub(super) fn admit_tree(ctx: &Ctx, e: &spargebra::algebra::Expression) -> Result<()> {
    use spargebra::algebra::Expression as E;
    admit(ctx, e)?;
    match e {
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
            admit_tree(ctx, a)?;
            admit_tree(ctx, b)?;
        }
        E::Not(a) | E::UnaryPlus(a) | E::UnaryMinus(a) => admit_tree(ctx, a)?,
        E::In(a, list) => {
            admit_tree(ctx, a)?;
            for x in list {
                admit_tree(ctx, x)?;
            }
        }
        E::If(a, b, c) => {
            admit_tree(ctx, a)?;
            admit_tree(ctx, b)?;
            admit_tree(ctx, c)?;
        }
        E::Coalesce(args) => {
            for a in args {
                admit_tree(ctx, a)?;
            }
        }
        E::FunctionCall(_, args) => {
            for a in args {
                admit_tree(ctx, a)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Refuse an expression that calls `se:cacheRm` with the wrong number of arguments, or
/// in a query without the right to. The planner asks this of every expression it
/// compiles. An EXISTS is compiled before any row reaches it and its pattern is planned
/// only then, so the calls in its pattern that run here are checked now too.
pub(super) fn admit(ctx: &Ctx, e: &spargebra::algebra::Expression) -> Result<()> {
    use spargebra::algebra::{Expression, Function};
    match e {
        Expression::FunctionCall(Function::Custom(n), args) if n.as_str() == CACHE_RM => {
            check_rm(ctx)?;
            if args.len() > 1 {
                return Err(Error::invalid(
                    "se:cacheRm takes no argument or an entry's id",
                ));
            }
        }
        Expression::Exists(p) if !ctx.service_cache_mgmt && calls_rm(ctx, p)? => check_rm(ctx)?,
        _ => {}
    }
    Ok(())
}

/// Whether `root` calls `se:cacheRm` by name where this query runs it: anywhere but
/// inside a SERVICE that the planner sends elsewhere, which is every SERVICE but one
/// over the dataset itself. A remote endpoint answers its pattern as it likes, and one
/// of the process's own endpoints plans it as a query of its own without the right to
/// remove. The walk keeps its own stack, so a deep pattern does not exhaust the thread's.
fn calls_rm(ctx: &Ctx, root: &spargebra::algebra::GraphPattern) -> Result<bool> {
    use spargebra::algebra::{
        AggregateExpression as A, Expression as E, Function, GraphPattern as GP,
        OrderExpression as O,
    };
    enum Item<'a> {
        P(&'a GP),
        E(&'a E),
    }
    fn order(o: &O) -> &E {
        match o {
            O::Asc(e) | O::Desc(e) => e,
        }
    }
    let mut stack = vec![Item::P(root)];
    while let Some(item) = stack.pop() {
        ctx.check()?;
        match item {
            Item::P(gp) => match gp {
                GP::Bgp { .. } | GP::Path { .. } | GP::Values { .. } => {}
                GP::Extend {
                    inner, expression, ..
                }
                | GP::Assign {
                    inner, expression, ..
                }
                | GP::Unfold {
                    inner, expression, ..
                } => stack.extend([Item::P(inner), Item::E(expression)]),
                GP::Join { left, right }
                | GP::Lateral { left, right }
                | GP::Union { left, right }
                | GP::Minus { left, right }
                | GP::SemiJoin { left, right }
                | GP::AntiJoin { left, right } => stack.extend([Item::P(left), Item::P(right)]),
                GP::LeftJoin {
                    left,
                    right,
                    expression,
                } => {
                    stack.extend([Item::P(left), Item::P(right)]);
                    stack.extend(expression.iter().map(Item::E));
                }
                GP::Filter { inner, expr } => stack.extend([Item::P(inner), Item::E(expr)]),
                GP::OrderBy { inner, expression } => {
                    stack.push(Item::P(inner));
                    stack.extend(expression.iter().map(|o| Item::E(order(o))));
                }
                GP::Group {
                    inner, aggregates, ..
                } => {
                    stack.push(Item::P(inner));
                    for (_, a) in aggregates {
                        match a {
                            A::CountSolutions { .. } => {}
                            A::FunctionCall { expr, .. } => stack.push(Item::E(expr)),
                            A::Fold {
                                expr,
                                value,
                                order: o,
                                ..
                            } => {
                                stack.push(Item::E(expr));
                                stack.extend(value.iter().map(Item::E));
                                stack.extend(o.iter().map(|o| Item::E(order(o))));
                            }
                        }
                    }
                }
                GP::Service {
                    name,
                    inner,
                    silent,
                } => {
                    if matches!(
                        super::enhancer::effective(name, inner, *silent),
                        Ok(Some(e)) if e.target == super::enhancer::Target::SelfDataset
                    ) {
                        stack.push(Item::P(inner));
                    }
                }
                GP::Graph { inner, .. }
                | GP::Project { inner, .. }
                | GP::Distinct { inner }
                | GP::Reduced { inner }
                | GP::Slice { inner, .. } => stack.push(Item::P(inner)),
            },
            Item::E(e) => match e {
                E::FunctionCall(Function::Custom(n), _) if n.as_str() == CACHE_RM => {
                    return Ok(true);
                }
                E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => {}
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
                | E::Divide(a, b) => stack.extend([Item::E(a), Item::E(b)]),
                E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => stack.push(Item::E(a)),
                E::In(a, l) => {
                    stack.push(Item::E(a));
                    stack.extend(l.iter().map(Item::E));
                }
                E::If(a, b, c) => stack.extend([Item::E(a), Item::E(b), Item::E(c)]),
                E::Coalesce(l) | E::FunctionCall(_, l) => stack.extend(l.iter().map(Item::E)),
                E::Exists(p) => stack.push(Item::P(p)),
            },
        }
    }
    Ok(false)
}

/// `se:cacheRm()` with `id` `None`, `se:cacheRm(id)` otherwise: the entries removed. A
/// removal whose files cannot be synced records that failure, which the query's next
/// check returns.
pub(super) fn cache_rm(ctx: &Ctx, id: Option<i64>) -> u64 {
    let id = match id {
        Some(i) => match u64::try_from(i) {
            Ok(i) => Some(i),
            Err(_) => return 0,
        },
        None => None,
    };
    match ctx
        .snap
        .results
        .service
        .remove_in_scope(&ctx.service_scope, id)
    {
        Ok(n) => n,
        Err(e) => {
            ctx.fail_side_effect(&e);
            0
        }
    }
}

/// The leaf of one `se:cacheLs` call: a table of the caller's entries, as the call's
/// slots bind them.
pub(super) fn cache_ls_leaf(
    p: &Planner<'_>,
    subjects: Vec<TermPattern>,
    args: Vec<TermPattern>,
) -> Result<Node> {
    let bad = |m: &str| Error::invalid(format!("se:cacheLs: {m}"));
    let [subject] = <[TermPattern; 1]>::try_from(subjects)
        .map_err(|_| bad("the subject is one variable or id"))?;
    // `()` is no positions, and an object that is not a list is the first one
    let args = match args.as_slice() {
        [TermPattern::NamedNode(n)] if n.as_str() == oxrdf::vocab::rdf::NIL.as_str() => Vec::new(),
        _ => args,
    };
    if args.len() > POSITIONS {
        return Err(bad("the object list has at most six positions"));
    }
    let ctx = p.ctx;
    let slots: Vec<PT> = std::iter::once(&subject)
        .chain(&args)
        .map(|t| p.term_pattern(t))
        .collect();
    let mut vars: Vec<VarId> = Vec::new();
    for s in &slots {
        if let PT::V(v) = s
            && !vars.contains(v)
        {
            vars.push(*v);
        }
    }
    let int = |n: u64| {
        let n = i64::try_from(n).unwrap_or(i64::MAX);
        ctx.intern_term(&Term::Literal(Literal::from(n)))
    };
    // the listing, the terms it interns and the table are admitted as they grow, and
    // what the leaf keeps, its terms and its table, stays charged with the plan
    let charge = ctx.charge(0)?;
    let mut kept = 0u64;
    let listed = ctx
        .snap
        .results
        .service
        .list_admitted(&ctx.service_scope, &|n| charge.add(n))?;
    let row_bytes = (vars.len() * std::mem::size_of::<Id>()) as u64;
    let mut t = Table::new(vars.clone());
    let mut row = vec![Id::UNDEF; vars.len()];
    for l in listed {
        let interned = (l.endpoint.len() + l.pattern.len() + l.binding.len()) as u64 + 3 * 64;
        charge.add(interned)?;
        kept = kept.saturating_add(interned);
        let endpoint = match NamedNode::new(l.endpoint.clone()) {
            Ok(n) => Term::NamedNode(n),
            Err(_) => Term::Literal(Literal::new_simple_literal(l.endpoint.clone())),
        };
        let values = [
            int(l.id),
            ctx.intern_term(&endpoint),
            ctx.intern_term(&Term::Literal(Literal::new_simple_literal(l.pattern))),
            ctx.intern_term(&Term::Literal(Literal::new_simple_literal(l.binding))),
            int(l.rows as u64),
            int(l.bytes),
            int(l.age.as_secs()),
        ];
        row.fill(Id::UNDEF);
        let mut ok = true;
        for (slot, value) in slots.iter().zip(values) {
            match slot {
                PT::C(c) => ok &= *c == value,
                PT::V(v) => {
                    let c = vars.iter().position(|x| x == v).expect("a slot's variable");
                    if row[c] != Id::UNDEF && row[c] != value {
                        ok = false;
                    }
                    row[c] = value;
                }
            }
        }
        if ok {
            ctx.check()?;
            charge.add(row_bytes)?;
            kept = kept.saturating_add(row_bytes);
            t.push_row(&row);
        }
    }
    charge.resize(kept)?;
    p.retain_plan_scratch(Some(charge))?;
    let est = t.len() as f64;
    let desc = format!("{LS_DESC} {} entries of the caller", t.len());
    Ok(Node::leaf(Kind::Values(t), vars, est, desc))
}
