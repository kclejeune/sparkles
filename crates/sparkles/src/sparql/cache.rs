//! Query (sub)result cache (QLever `QueryResultCache`).
//!
//! Executed operator subtrees are cached under a canonical key built from the plan
//! (operators, descriptions, variable *names*, constants) plus the snapshot version and
//! the query dataset, so a later query containing the same subtree — even with different
//! variable ids — reuses the result. Results containing query-local terms, and subtrees
//! with non-deterministic functions or SERVICE, are never cached.

use super::ctx::Ctx;
use super::expr::{Expr, Func};
use super::plan::{Kind, Node};
use super::table::{Table, VarId};
use crate::id::{Id, Tag};
use spargebra::algebra::Function;
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A cached result with variables stored by name.
pub struct Entry {
    vars: Vec<String>,
    sorted: Vec<String>,
    cols: Arc<Vec<Vec<Id>>>,
    len: usize,
}

impl Entry {
    fn bytes(&self) -> u64 {
        (self.len * self.cols.len() * 8 + 64) as u64
    }
}

#[derive(Clone)]
struct Weighter;
impl quick_cache::Weighter<String, Arc<Entry>> for Weighter {
    fn weight(&self, k: &String, v: &Arc<Entry>) -> u64 {
        v.bytes() + k.len() as u64
    }
}

pub struct ResultCache {
    cache: Option<quick_cache::sync::Cache<String, Arc<Entry>, Weighter>>,
    max_entry_bytes: u64,
    /// minimum computation time (ms) for a result to be worth caching
    pub min_ms: f64,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl ResultCache {
    pub fn new(bytes: u64, min_ms: f64) -> ResultCache {
        ResultCache {
            min_ms,
            cache: (bytes > 0)
                .then(|| quick_cache::sync::Cache::with_weighter(10_000, bytes, Weighter)),
            max_entry_bytes: bytes / 8,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cache.is_some()
    }

    pub fn get(&self, key: &str, ctx: &Ctx) -> Option<Table> {
        let c = self.cache.as_ref()?;
        match c.get(key) {
            Some(e) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                let vars: Vec<VarId> = e.vars.iter().map(|n| ctx.var(n)).collect();
                let sorted: Vec<VarId> = e.sorted.iter().map(|n| ctx.var(n)).collect();
                Some(Table {
                    vars,
                    cols: (*e.cols).clone(),
                    len: e.len,
                    sorted,
                })
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Cache a result unless it holds query-local terms or is too large.
    pub fn put(&self, key: String, t: &Table, ctx: &Ctx) {
        let Some(c) = &self.cache else { return };
        let bytes = (t.len() * t.width() * 8) as u64;
        if bytes > self.max_entry_bytes {
            return;
        }
        let local = t.cols.iter().flatten().any(|id| {
            id.tag() == Tag::Local
                || (id.tag() == Tag::BNode && id.payload() & Id::LOCAL_BNODE_BIT != 0)
        });
        if local {
            return;
        }
        c.insert(
            key,
            Arc::new(Entry {
                vars: t.vars.iter().map(|v| ctx.var_name(*v)).collect(),
                sorted: t.sorted.iter().map(|v| ctx.var_name(*v)).collect(),
                cols: Arc::new(t.cols.clone()),
                len: t.len(),
            }),
        );
    }

    pub fn clear(&self) {
        if let Some(c) = &self.cache {
            c.clear();
        }
    }
    pub fn entries(&self) -> usize {
        self.cache.as_ref().map_or(0, |c| c.len())
    }
    pub fn bytes(&self) -> u64 {
        self.cache.as_ref().map_or(0, |c| c.weight())
    }
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
}

fn deterministic(e: &Expr) -> bool {
    match e {
        Expr::Call(Func::Builtin(f), args) => {
            !matches!(
                f,
                Function::Rand
                    | Function::Now
                    | Function::Uuid
                    | Function::StrUuid
                    | Function::BNode
            ) && args.iter().all(deterministic)
        }
        Expr::Call(Func::Ext(name), args) => {
            !name.ends_with("#now") && args.iter().all(deterministic)
        }
        Expr::Call(_, l) | Expr::Coalesce(l) => l.iter().all(deterministic),
        Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Eq(a, b)
        | Expr::SameTerm(a, b)
        | Expr::Cmp(a, b, _)
        | Expr::Arith(a, b, _) => deterministic(a) && deterministic(b),
        Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => deterministic(a),
        Expr::In(a, l) => deterministic(a) && l.iter().all(deterministic),
        Expr::If(a, b, c) => deterministic(a) && deterministic(b) && deterministic(c),
        // EXISTS results are memoized per query; the pattern text is not in the key
        Expr::Exists(_) => false,
        Expr::Const(_) | Expr::Var(_) | Expr::Bound(_) => true,
    }
}

/// Canonical cache key of a subtree, or `None` if it must not be cached.
pub fn key(n: &Node, ctx: &Ctx) -> Option<String> {
    let mut s = String::with_capacity(256);
    let ds = &ctx.dataset;
    let _ = write!(
        s,
        "v{}|{}|{:?}|{:?}|{}|",
        ctx.snap.version, ctx.snap.generation.name, ds.default, ds.named, ds.union_default
    );
    write_node(n, ctx, &mut s).then_some(s)
}

fn write_node(n: &Node, ctx: &Ctx, s: &mut String) -> bool {
    let names = |vs: &[VarId]| {
        vs.iter()
            .map(|v| ctx.var_name(*v))
            .collect::<Vec<_>>()
            .join(",")
    };
    let _ = write!(s, "({} [{}] {{{}}}", n.operator(), n.desc, names(&n.vars));
    let ok = match &n.kind {
        Kind::Service { .. } => false,
        Kind::Values(t) => {
            // include the actual rows (local ids make it uncacheable)
            if t.cols.iter().flatten().any(|id| id.tag() == Tag::Local) {
                false
            } else {
                for i in 0..t.len().min(10_000) {
                    for c in &t.cols {
                        let _ = write!(s, "{:x},", c[i].0);
                    }
                    s.push(';');
                }
                t.len() <= 10_000
            }
        }
        Kind::Scan(spec)
        | Kind::CountScan { spec, .. }
        | Kind::CountDistinctScan { spec, .. }
        | Kind::GroupCountScan { spec, .. } => {
            let _ = write!(s, "{:?}{:?}{:?}", spec.prefix, spec.graph, spec.eqs);
            true
        }
        Kind::Filter(es) => es.iter().all(deterministic),
        Kind::Extend(_, e) => deterministic(e),
        Kind::LeftJoin { expr } => expr.as_ref().is_none_or(deterministic),
        Kind::OrderBy { keys, limit } => {
            let _ = write!(s, "limit={limit:?}");
            keys.iter().all(|(e, _)| deterministic(e))
        }
        Kind::Group { aggs, .. } => {
            for (v, a) in aggs {
                let _ = write!(
                    s,
                    "{}={:?}/{}/{};",
                    ctx.var_name(*v),
                    a.func,
                    a.distinct,
                    a.expr.as_ref().map(|e| e.display(ctx)).unwrap_or_default()
                );
            }
            aggs.iter()
                .all(|(_, a)| a.expr.as_ref().is_none_or(deterministic))
        }
        Kind::Path {
            spec,
            bound_from_left,
        } => {
            let _ = write!(
                s,
                "{:?}{:?}{}{}{:?}{}",
                spec.subj, spec.obj, spec.min, spec.max_one, spec.simple, bound_from_left
            );
            true
        }
        _ => true,
    };
    if !ok {
        return false;
    }
    for c in &n.children {
        if !write_node(c, ctx, s) {
            return false;
        }
    }
    s.push(')');
    true
}
