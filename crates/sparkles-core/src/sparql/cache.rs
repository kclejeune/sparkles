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
use super::svccache::{NsKey, NsKeyRef, ServiceStore};
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
impl quick_cache::Weighter<NsKey, Arc<Entry>> for Weighter {
    fn weight(&self, k: &NsKey, v: &Arc<Entry>) -> u64 {
        v.bytes() + k.key.len() as u64
    }
}

/// The storage of cached results that stores can share, with the storage of remote
/// SERVICE results. A server holds one for all its datasets.
pub struct ResultStore {
    cache: Option<quick_cache::sync::Cache<NsKey, Arc<Entry>, Weighter>>,
    max_entry_bytes: u64,
    service: Arc<ServiceStore>,
}

impl ResultStore {
    /// Storage of at most `bytes` for results (0 turns it off) and `service_bytes` for
    /// remote SERVICE results.
    pub fn new(bytes: u64, service_bytes: u64) -> ResultStore {
        Self::with_shards(bytes, service_bytes, super::svccache::host_shards())
    }

    /// [`new`](Self::new) asking for `shards` shards, of which the cache keeps at most
    /// [`MAX_SHARDS`](super::svccache::MAX_SHARDS), so an entry up to an eighth of the
    /// budget always fits its shard.
    fn with_shards(bytes: u64, service_bytes: u64, shards: usize) -> ResultStore {
        ResultStore {
            cache: (bytes > 0).then(|| super::svccache::bounded_cache(bytes, shards, Weighter)),
            max_entry_bytes: bytes / 8,
            service: Arc::new(ServiceStore::new(service_bytes)),
        }
    }

    pub fn capacity(&self) -> u64 {
        self.cache.as_ref().map_or(0, |c| c.capacity())
    }

    pub fn bytes(&self) -> u64 {
        self.cache.as_ref().map_or(0, |c| c.weight())
    }

    pub fn entries(&self) -> usize {
        self.cache.as_ref().map_or(0, |c| c.len())
    }

    /// The remote SERVICE results' storage.
    pub fn service(&self) -> &Arc<ServiceStore> {
        &self.service
    }

    /// Bytes and entries by namespace, in one pass.
    pub fn usage(&self) -> std::collections::HashMap<u32, crate::index::NamespaceUsage> {
        let mut out: std::collections::HashMap<u32, crate::index::NamespaceUsage> =
            Default::default();
        if let Some(c) = &self.cache {
            for (k, v) in c.iter() {
                let u = out.entry(k.ns).or_default();
                u.bytes += v.bytes() + k.key.len() as u64;
                u.entries += 1;
            }
        }
        out
    }
}

/// A store's view of the cache of query results. Hits and misses are the view's own.
pub struct ResultCache {
    /// the cache of remote SERVICE results, cleared with this one
    pub service: super::svccache::ServiceCache,
    store: Arc<ResultStore>,
    ns: u32,
    /// whether other views share the storage
    shared: bool,
    /// minimum computation time (ms) for a result to be worth caching
    pub min_ms: f64,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl ResultCache {
    pub fn new(bytes: u64, min_ms: f64) -> ResultCache {
        ResultCache::with_service(bytes, min_ms, 0)
    }

    /// A result cache of its own with a cache of remote SERVICE results of
    /// `service_bytes`.
    pub fn with_service(bytes: u64, min_ms: f64, service_bytes: u64) -> ResultCache {
        let store = Arc::new(ResultStore::new(bytes, service_bytes));
        ResultCache {
            service: super::svccache::ServiceCache::view(store.service().clone(), 0, false),
            store,
            ns: 0,
            shared: false,
            min_ms,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// A view of `store` for namespace `ns`.
    pub fn attach(store: &Arc<ResultStore>, ns: u32, min_ms: f64) -> ResultCache {
        ResultCache {
            service: super::svccache::ServiceCache::attach(store.service(), ns),
            store: store.clone(),
            ns,
            shared: true,
            min_ms,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.store.cache.is_some()
    }

    /// The namespace of this view's entries.
    pub fn namespace(&self) -> u32 {
        self.ns
    }

    /// Look up a cached result. The request's row and memory budgets apply before the
    /// columns are copied, exactly as they would to recomputing the result.
    pub fn get(&self, key: &CacheKey, ctx: &Ctx) -> crate::error::Result<Option<Table>> {
        let Some(c) = self.store.cache.as_ref() else {
            return Ok(None);
        };
        let lookup = NsKeyRef {
            ns: self.ns,
            key: &key.key,
        };
        Ok(match c.get(&lookup) {
            Some(e) => {
                ctx.check_output(e.len, e.cols.len())?;
                self.hits.fetch_add(1, Ordering::Relaxed);
                let vars: Vec<VarId> = e.vars.iter().map(|n| ctx.var(key.resolve(n))).collect();
                let sorted: Vec<VarId> = e.sorted.iter().map(|n| ctx.var(key.resolve(n))).collect();
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
        })
    }

    /// Only algorithm-owned execution uses this private clone boundary. Entries stay
    /// plain data; the caller acquires its own capacity before copying any columns.
    pub(super) fn get_owned(
        &self,
        key: &CacheKey,
        ctx: &Ctx,
    ) -> crate::error::Result<Option<super::table::Materialized>> {
        use super::table::Materialized;
        use crate::algo::memory;
        use crate::id::Id;
        let Some(c) = self.store.cache.as_ref() else {
            return Ok(None);
        };
        let lookup = NsKeyRef {
            ns: self.ns,
            key: &key.key,
        };
        let Some(e) = c.get(&lookup) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        };
        ctx.check_output_rows(e.len)?;
        let data = e.cols.iter().try_fold(0u64, |n, c| {
            memory::sum(&[n, memory::bytes::<Id>(c.len())?])
        })?;
        let bytes = memory::sum(&[
            data.max(if e.cols.is_empty() {
                memory::bytes::<Id>(e.len)?
            } else {
                0
            }),
            memory::bytes::<VarId>(e.vars.len())?,
            memory::bytes::<VarId>(e.sorted.len())?,
            memory::bytes::<Vec<Id>>(e.cols.len())?,
            128,
        ])?;
        let mut out = Materialized {
            table: Table::empty(Vec::new()),
            lease: Some(ctx.output_lease(bytes)?),
        };
        self.hits.fetch_add(1, Ordering::Relaxed);
        out.table.vars = e.vars.iter().map(|n| ctx.var(key.resolve(n))).collect();
        out.table.sorted = e.sorted.iter().map(|n| ctx.var(key.resolve(n))).collect();
        out.table.cols = (*e.cols).clone();
        out.table.len = e.len;
        if super::algosvc::table_capacity(&out.table)? != bytes {
            return Err(crate::error::Error::invalid(
                "algorithm result clone exceeded its admitted capacity",
            ));
        }
        Ok(Some(out))
    }

    /// Cache a result unless it holds query-local terms or is too large.
    pub fn put(&self, key: CacheKey, t: &Table, ctx: &Ctx) {
        let Some(c) = &self.store.cache else { return };
        let bytes = (t.len() * t.width() * 8) as u64;
        if bytes > self.store.max_entry_bytes {
            return;
        }
        let local = t.cols.iter().flatten().any(|id| {
            id.tag() == Tag::Local
                || (id.tag() == Tag::BNode && id.payload() & Id::LOCAL_BNODE_BIT != 0)
        });
        if local {
            return;
        }
        let store = |v: &VarId| key.canonical(&ctx.var_name(*v));
        c.insert(
            NsKey {
                ns: self.ns,
                key: key.key.clone(),
            },
            Arc::new(Entry {
                vars: t.vars.iter().map(store).collect(),
                sorted: t.sorted.iter().map(store).collect(),
                cols: Arc::new(t.cols.clone()),
                len: t.len(),
            }),
        );
    }

    /// Drop this view's entries. Another namespace's stay.
    pub fn clear(&self) {
        let Some(c) = &self.store.cache else { return };
        if self.shared {
            let ns = self.ns;
            c.retain(|k, _| k.ns != ns);
        } else {
            c.clear();
        }
    }
    /// The entries of this view's namespace.
    pub fn entries(&self) -> usize {
        match &self.store.cache {
            Some(c) if self.shared => c.iter().filter(|(k, _)| k.ns == self.ns).count(),
            Some(c) => c.len(),
            None => 0,
        }
    }
    /// The bytes of this view's namespace.
    pub fn bytes(&self) -> u64 {
        match &self.store.cache {
            Some(c) if self.shared => c
                .iter()
                .filter(|(k, _)| k.ns == self.ns)
                .map(|(k, v)| v.bytes() + k.key.len() as u64)
                .sum(),
            Some(c) => c.weight(),
            None => 0,
        }
    }
    /// The bytes the storage may hold, shared with other views or not.
    pub fn capacity(&self) -> u64 {
        self.store.capacity()
    }
    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }
    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
}

/// Same result for the same variable bindings across queries, without side effects.
pub(crate) fn deterministic(e: &Expr) -> bool {
    // The shared classifier is query-local. Cached results also outlive a query's
    // clock, so check NOW separately in a second linear walk.
    super::exprcache::impurity(e).is_none() && timeless(e)
}

fn timeless(e: &Expr) -> bool {
    match e {
        Expr::Call(Func::Builtin(Function::Now), _) => false,
        Expr::Call(Func::Ext(name), _) if name.ends_with("#now") => false,
        Expr::Call(_, l) | Expr::Coalesce(l) => l.iter().all(timeless),
        Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Eq(a, b)
        | Expr::SameTerm(a, b)
        | Expr::Cmp(a, b, _)
        | Expr::Arith(a, b, _) => timeless(a) && timeless(b),
        Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => timeless(a),
        Expr::In(a, l) => timeless(a) && l.iter().all(timeless),
        Expr::If(a, b, c) => timeless(a) && timeless(b) && timeless(c),
        Expr::Exists(_) => false,
        Expr::Const(_) | Expr::Lit(..) | Expr::Var(_) | Expr::Bound(_) => true,
    }
}

/// A cache key plus the generated variable names it abstracts over.
///
/// Generated variables (spargebra's random names for aggregates and anonymous
/// patterns, the planner's hidden `" _N"` / blank-node variables) differ on every parse;
/// they are replaced by positional placeholders so identical queries share entries.
pub struct CacheKey {
    pub key: String,
    anon: Vec<String>,
}

impl CacheKey {
    fn canonical(&self, name: &str) -> String {
        match self.anon.iter().position(|a| a == name) {
            Some(i) => format!("\u{a7}{i}"),
            None => name.to_string(),
        }
    }
    fn resolve<'a>(&'a self, name: &'a str) -> &'a str {
        match name
            .strip_prefix('\u{a7}')
            .and_then(|i| i.parse::<usize>().ok())
        {
            Some(i) => &self.anon[i],
            None => name,
        }
    }
}

fn anon_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // `?` + a generated variable name, not followed by further name characters.
    // spargebra formats a random u128 with `{:x}` (no zero padding), so the hex names
    // are up to 32 digits long; ≤ 16 digits has probability 2^-64.
    RE.get_or_init(|| {
        regex::Regex::new(r"\?([0-9a-f]{17,32}| _\d+| bn\d+_[A-Za-z0-9_.-]+)([^A-Za-z0-9_]|$)")
            .unwrap()
    })
}

/// The most variables the nodes of a subtree with a cache key may have, summed over its
/// nodes. A key writes the variables of every node of its subtree, so the keys of a
/// subtree's nodes grow with the cube of its depth: the eager run of a chain of 2000 triple
/// patterns spent minutes writing them. A larger subtree is not cached. A key of this many
/// variables is about 100 KB, and the subtrees of ordinary queries stay far below it.
const KEY_MAX_VARS: usize = 16_384;

/// Whether the variables of the nodes of `n` are within [`KEY_MAX_VARS`], read without
/// walking further than that.
fn small_enough(n: &Node) -> bool {
    let mut left = KEY_MAX_VARS;
    let mut todo = vec![n];
    while let Some(n) = todo.pop() {
        match left.checked_sub(n.vars.len() + 1) {
            Some(l) => left = l,
            None => return false,
        }
        todo.extend(&n.children);
    }
    true
}

/// Canonical cache key of a subtree, or `None` if it must not be cached.
pub fn key(n: &Node, ctx: &Ctx) -> Option<CacheKey> {
    if !small_enough(n) {
        return None;
    }
    let raw = raw_key(n, ctx)?;
    let mut anon: Vec<String> = Vec::new();
    let key = anon_regex()
        .replace_all(&raw, |c: &regex::Captures<'_>| {
            let tok = c[1].to_string();
            // only rename actual variables of this query (never IRI / literal text)
            if !ctx.has_var(&tok) {
                return c[0].to_string();
            }
            let i = match anon.iter().position(|a| *a == tok) {
                Some(i) => i,
                None => {
                    anon.push(tok);
                    anon.len() - 1
                }
            };
            format!("?\u{a7}{i}{}", &c[2])
        })
        .into_owned();
    Some(CacheKey { key, anon })
}

/// What the data a query reads is a function of: the generation instance and the commit
/// (a compaction makes a new instance; a past state shares its generation's instance),
/// the graphs of the request, and the mask of a view that hides triples.
pub(super) fn view_key(ctx: &Ctx) -> String {
    let mut s = String::with_capacity(64);
    let _ = write_view_key(ctx, &mut s);
    s
}

/// The shared encoding, also used by pre-admitted algorithm key storage.
pub(super) fn write_view_key(ctx: &Ctx, mut s: impl std::fmt::Write) -> std::fmt::Result {
    let ds = &ctx.dataset;
    write!(
        s,
        "g{}|c{}|{:?}|{:?}|{}|{}|",
        ctx.snap.generation.uid,
        ctx.snap.commit,
        ds.default,
        ds.named,
        ds.union_default,
        // a request may override the store's union default graph
        ctx.snap.union_default_graph
    )?;
    // a view that hides triples reads other data than the commit holds
    if let Some(m) = &ctx.snap.mask {
        write!(s, "m{}|", m.key)?;
    }
    Ok(())
}

fn raw_key(n: &Node, ctx: &Ctx) -> Option<String> {
    let mut s = view_key(ctx);
    s.reserve(256);
    write_node(n, ctx, &mut s).then_some(s)
}

/// The spatial index's epoch and configuration hash as the snapshot sees them.
fn geo_view_key(ctx: &Ctx) -> (u64, u64) {
    ctx.snap
        .geo
        .as_ref()
        .map_or((0, 0), |v| (v.epoch, v.config.hash()))
}

fn write_node(n: &Node, ctx: &Ctx, s: &mut String) -> bool {
    let names = |vs: &[VarId]| {
        vs.iter()
            .map(|v| format!("?{}", ctx.var_name(*v)))
            .collect::<Vec<_>>()
            .join(",")
    };
    let _ = write!(s, "({} [{}] {{{}}}", n.operator(), n.desc, names(&n.vars));
    let ok = match &n.kind {
        Kind::Service { .. } | Kind::VectorSearch(_) | Kind::HybridSearch(_) => false,
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
        Kind::CountFilterScan { spec, filter, .. } => {
            let _ = write!(s, "{:?}{:?}{:?}", spec.prefix, spec.graph, spec.eqs);
            filter.iter().all(deterministic)
        }
        Kind::RangeScan(spec, range) => {
            let _ = write!(s, "{:?}{:?}{:?}", spec.prefix, spec.graph, spec.eqs);
            range.filter.iter().all(deterministic)
        }
        Kind::OrderedTopK(t) => {
            let _ = write!(
                s,
                "{:?}{:?}{:?}{:?}",
                t.scan.prefix, t.scan.graph, t.scan.eqs, t.pieces
            );
            t.filter.iter().chain(&t.range_filter).all(deterministic)
        }
        Kind::SpatialScan(sp) => {
            // the constant geometries, tests and windows, and the index's epoch and
            // configuration (a rebuild or reconfiguration changes the candidates' source)
            let (epoch, config) = geo_view_key(ctx);
            let _ = write!(
                s,
                "{:?}{:?}{:?}k{:x}e{epoch}h{config:x}",
                sp.scan.prefix, sp.scan.graph, sp.scan.eqs, sp.key
            );
            sp.filter.iter().all(deterministic)
        }
        Kind::SpatialJoin(sp) => {
            // the sides' scans and plans are part of `key` and of the children
            let (epoch, config) = geo_view_key(ctx);
            let _ = write!(s, "k{:x}e{epoch}h{config:x}", sp.key);
            sp.filter.iter().all(deterministic)
        }
        Kind::SpatialKnn(sp) => {
            let (epoch, config) = geo_view_key(ctx);
            let _ = write!(
                s,
                "{:?}{:?}{:?}k{:x}e{epoch}h{config:x}",
                sp.scan.prefix, sp.scan.graph, sp.scan.eqs, sp.key
            );
            true
        }
        Kind::SpatialRelate(sp) => {
            let (epoch, config) = geo_view_key(ctx);
            let _ = write!(
                s,
                "{:?}{:?}k{:x}e{epoch}h{config:x}",
                sp.graph, sp.graph_var, sp.key
            );
            true
        }
        Kind::SpatialPf(sp) => {
            let (epoch, config) = geo_view_key(ctx);
            let _ = write!(
                s,
                "{:?}{:?}{:?}k{:x}e{epoch}h{config:x}",
                sp.graph, sp.graph_var, sp.subject, sp.key
            );
            true
        }
        // the change log grows and retention trims it: not cached
        Kind::HistoryChanges(_) => false,
        // views move with their own passes: not cached
        Kind::ViewRead(_) => false,
        Kind::AlgoCall(spec) => {
            // every parameter is in the spec, ids and variables included
            let _ = write!(s, "{spec:?}");
            true
        }
        Kind::PathSearch(spec) => {
            // every parameter is in the spec, ids and variables included
            let _ = write!(s, "{spec:?}");
            true
        }
        Kind::TextSearch(t) => {
            // An immutable unavailable view must never reuse a ready view's result,
            // even when its generation, commit and epoch otherwise match.
            #[cfg(feature = "text")]
            if ctx.snap.text.as_ref().is_some_and(|v| !v.is_available()) {
                return false;
            }
            // the view's epoch changes with every rebuild of the index
            let epoch = ctx.snap.text.as_ref().map_or(0, |v| v.epoch);
            #[cfg(feature = "text")]
            let _ = write!(
                s,
                "owner:{};",
                ctx.snap.text.as_ref().map_or(0, |v| v.owner)
            );
            let _ = write!(s, "{:?}{:?}{:?}{}", t.graph, t.graph_var, t.subject, epoch);
            true
        }
        Kind::IndexJoin(j) => {
            for p in &j.probes {
                let _ = write!(
                    s,
                    "{:?}{:?}{:?}{:?};",
                    p.scan.perm, p.scan.prefix, p.scan.graph, p.scan.eqs
                );
            }
            j.probes.iter().all(|p| p.filter.iter().all(deterministic))
        }
        Kind::Lateral(l) => {
            // the right side's text and the constants in force; its blank node labels
            // and generated names are local to it
            let _ = write!(
                s,
                "{}|{}|{:?}|{:?}|{:?}|{:?}",
                l.pattern,
                l.unscoped,
                match &l.graph {
                    super::plan::ActiveGraph::Var(v) => format!("?{}", ctx.var_name(*v)),
                    g => format!("{g:?}"),
                },
                l.keys.iter().map(|k| ctx.var_name(*k)).collect::<Vec<_>>(),
                l.outer
                    .iter()
                    .map(|(v, id)| (ctx.var_name(*v), id.0))
                    .collect::<Vec<_>>(),
                l.outer_scoped
                    .iter()
                    .map(|v| ctx.var_name(*v))
                    .collect::<Vec<_>>(),
            );
            // a remote loop's results are the endpoint's
            l.service.is_none() && super::exists::pure_pattern(&l.pattern)
        }
        Kind::RegisteredProperty(_) => false,
        Kind::PropertyFn(spec) => {
            let _ = write!(s, "{:?}", spec);
            true
        }
        Kind::Filter(es) => es.iter().all(deterministic),
        Kind::Extend(_, e) | Kind::Assign(_, e) | Kind::Unfold { expr: e, .. } => deterministic(e),
        Kind::LeftJoin { expr } => expr.as_ref().is_none_or(deterministic),
        Kind::KeyedLeftJoin { expr, .. } | Kind::KeyedJoin { expr, .. } => deterministic(expr),
        Kind::OrderBy { keys, limit } => {
            let _ = write!(s, "limit={limit:?}");
            keys.iter().all(|(e, _)| deterministic(e))
        }
        Kind::Group { aggs, .. } => {
            for (v, a) in aggs {
                let _ = write!(
                    s,
                    "?{}={:?}/{}/{};",
                    ctx.var_name(*v),
                    a.func,
                    a.distinct,
                    a.expr.as_ref().map(|e| e.display(ctx)).unwrap_or_default()
                );
                if let Some(f) = &a.fold {
                    let _ = write!(
                        s,
                        "fold/{}/{};",
                        f.value.as_ref().map(|e| e.display(ctx)).unwrap_or_default(),
                        f.order
                            .iter()
                            .map(|(e, asc)| format!("{asc}:{}", e.display(ctx)))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                }
            }
            aggs.iter().all(|(_, a)| {
                a.registered.is_none()
                    && a.expr.as_ref().is_none_or(deterministic)
                    && a.fold.as_ref().is_none_or(|f| {
                        f.value.as_ref().is_none_or(deterministic)
                            && f.order.iter().all(|(e, _)| deterministic(e))
                    })
            })
        }
        Kind::Path {
            spec,
            bound_from_left,
        } => {
            let _ = write!(
                s,
                "{:?}{:?}{}{}{:?}{}{:?}{:?}",
                spec.subj,
                spec.obj,
                spec.min,
                spec.max_one,
                spec.simple,
                bound_from_left,
                spec.graph,
                spec.count
            );
            if let Some(g) = spec.graph_var {
                let _ = write!(s, " graph=?{}", ctx.var_name(g));
            }
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

#[cfg(test)]
mod tests {
    use super::{Expr, Func, anon_regex, deterministic};

    #[test]
    fn volatile_aliases_and_dynamic_calls_are_not_deterministic() {
        for name in [
            "http://jena.apache.org/ARQ/function#now",
            "http://jena.apache.org/ARQ/function#uuid",
            "http://jena.apache.org/ARQ/function#struuid",
            "http://jena.apache.org/ARQ/function#eval",
            "http://jena.apache.org/ARQ/function#wait",
            "http://jena.apache.org/ARQ/function#print",
            "http://jena.apache.org/ARQ/function#execTime",
            "http://www.w3.org/2005/xpath-functions#apply",
        ] {
            assert!(
                !deterministic(&Expr::Call(Func::Ext(name.into()), vec![])),
                "{name}"
            );
        }
        for name in [
            "http://jena.apache.org/ARQ/function#strlen",
            "http://www.opengis.net/def/function/geosparql/metricDistance",
        ] {
            assert!(
                deterministic(&Expr::Call(Func::Ext(name.into()), vec![])),
                "{name}"
            );
        }
    }

    #[test]
    fn cache_purity_keeps_effects_and_query_time_out_when_nested() {
        use spargebra::algebra::Function;

        for local in ["wait", "print", "execTime"] {
            let effect = Expr::Call(
                Func::Ext(format!("http://jena.apache.org/ARQ/function#{local}")),
                vec![Expr::Var(0)],
            );
            let nested = Expr::Call(Func::Builtin(Function::StrLen), vec![effect]);
            assert!(!deterministic(&nested), "{local}");
        }
        let now = Expr::Call(Func::Builtin(Function::Now), vec![]);
        assert!(super::super::exprcache::impurity(&now).is_none());
        assert!(!deterministic(&now));
        assert!(!deterministic(&Expr::Coalesce(vec![Expr::Var(0), now])));
        assert!(deterministic(&Expr::Call(
            Func::Ext("http://jena.apache.org/ARQ/function#strlen".into()),
            vec![Expr::Var(0)],
        )));
    }

    #[test]
    fn generated_names_without_zero_padding_are_recognized() {
        // `format!("{:x}", u128)` drops leading zeros: 1 in 16 names is shorter than 32
        for name in [
            "747f263623102c9077294cf98085481f",
            "4b0701a3941a6fac1867cf67729c4f",
            "1234567890abcdef1",
        ] {
            let text = format!("(Bind [?c := ?{name}] {{?t}})");
            let c = anon_regex().captures(&text).expect(name);
            assert_eq!(&c[1], name);
        }
        // ordinary short variable names are left alone
        assert!(anon_regex().captures("(Bind [?c := ?abc] {?t})").is_none());
    }
}

#[cfg(test)]
mod shard_tests {
    use super::*;

    /// The largest entry `put` accepts is kept, whatever number of shards the host asks
    /// for. The default asks for four per CPU, so 64 on a host with 16 CPUs.
    #[test]
    fn largest_accepted_result_is_kept_on_any_host() {
        let budget = 64u64 << 20;
        for shards in [1, 4, 16, 64, 128, 256, 1024] {
            let store = ResultStore::with_shards(budget, 0, shards);
            let c = store.cache.as_ref().unwrap();
            let key = NsKey {
                ns: 0,
                key: "k0".to_string(),
            };
            let rows = ((store.max_entry_bytes - 64 - key.key.len() as u64) / 8) as usize;
            let entry = Arc::new(Entry {
                vars: vec!["x".into()],
                sorted: vec![],
                cols: Arc::new(vec![vec![Id(0); rows]]),
                len: rows,
            });
            c.insert(key.clone(), entry);
            assert!(
                c.get(&NsKeyRef {
                    ns: 0,
                    key: &key.key
                })
                .is_some(),
                "{shards} shards ({} of {} bytes) dropped an entry that put accepts",
                c.num_shards(),
                c.shard_capacity()
            );
        }
    }
}
