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
//! by name fails when it is planned. A call through `fn:apply` or `afn:eval`, whose
//! function the planner cannot see, is an error of the expression and removes nothing,
//! with the right or without it. Neither function reaches another caller's entries,
//! since both compare the entry's scope with the caller's as a whole.

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

/// Refuse an expression that calls `se:cacheRm` with the wrong number of arguments, or
/// in a query without the right to. The planner asks this of every expression it
/// compiles.
pub(super) fn admit(ctx: &Ctx, e: &spargebra::algebra::Expression) -> Result<()> {
    use spargebra::algebra::{Expression, Function};
    if let Expression::FunctionCall(Function::Custom(n), args) = e
        && n.as_str() == CACHE_RM
    {
        check_rm(ctx)?;
        if args.len() > 1 {
            return Err(Error::invalid(
                "se:cacheRm takes no argument or an entry's id",
            ));
        }
    }
    Ok(())
}

/// `se:cacheRm()` with `id` `None`, `se:cacheRm(id)` otherwise: the entries removed.
pub(super) fn cache_rm(ctx: &Ctx, id: Option<i64>) -> u64 {
    let id = match id {
        Some(i) => match u64::try_from(i) {
            Ok(i) => Some(i),
            Err(_) => return 0,
        },
        None => None,
    };
    ctx.snap
        .results
        .service
        .remove_in_scope(&ctx.service_scope, id)
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
    // the listing and the table are admitted as they grow
    let charge = ctx.charge(0)?;
    let listed = ctx
        .snap
        .results
        .service
        .list_admitted(&ctx.service_scope, &|n| charge.add(n))?;
    let row_bytes = (vars.len() * std::mem::size_of::<Id>()) as u64;
    let mut t = Table::new(vars.clone());
    let mut row = vec![Id::UNDEF; vars.len()];
    for l in listed {
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
            t.push_row(&row);
        }
    }
    let est = t.len() as f64;
    let desc = format!("{LS_DESC} {} entries of the caller", t.len());
    Ok(Node::leaf(Kind::Values(t), vars, est, desc))
}
