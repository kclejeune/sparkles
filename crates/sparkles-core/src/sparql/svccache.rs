//! The cache of remote SERVICE results (Jena's service enhancer, `SERVICE <cache:…>`).
//!
//! An entry holds the solutions an endpoint returned for one request: the endpoint's
//! URL, the SERVICE pattern and the values substituted into it for one input binding.
//! The solutions are kept as terms, because remote terms have no ids in the store's
//! dictionary. Keys start with the caller's scope (see
//! [`QueryOptions::service_scope`](super::QueryOptions::service_scope)), so callers with
//! different credentials or views never read each other's entries.
//!
//! The cache is bounded by bytes, estimated from the terms' lengths, and an entry larger
//! than an eighth of the budget is not kept. Only complete responses are stored. A bulk
//! response that an endpoint cut short is never split into entries.

use oxrdf::Term;
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The solutions of one request, by variable name.
pub struct Rows {
    pub vars: Vec<String>,
    pub rows: Vec<Vec<Option<Term>>>,
}

impl Rows {
    /// The estimated bytes the entry holds.
    pub fn bytes(&self) -> u64 {
        let terms: usize = self
            .rows
            .iter()
            .flatten()
            .map(|t| t.as_ref().map_or(0, term_bytes) + 16)
            .sum();
        let vars: usize = self.vars.iter().map(|v| v.len() + 24).sum();
        (terms + vars + self.rows.len() * 24 + 64) as u64
    }
}

fn term_bytes(t: &Term) -> usize {
    match t {
        Term::NamedNode(n) => n.as_str().len() + 24,
        Term::BlankNode(b) => b.as_str().len() + 24,
        Term::Literal(l) => {
            l.value().len() + l.language().map_or(0, str::len) + l.datatype().as_str().len() + 48
        }
        Term::Triple(t) => {
            term_bytes(&t.subject.clone().into())
                + t.predicate.as_str().len()
                + term_bytes(&t.object)
                + 24
        }
    }
}

/// A cache key: the namespace of the store that put the entry, then the request's key.
/// A lookup by [`NsKeyRef`] needs no allocation.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct NsKey {
    pub ns: u32,
    pub key: String,
}

/// A borrowed [`NsKey`]. It hashes as the owned key does.
#[derive(Hash)]
pub(crate) struct NsKeyRef<'a> {
    pub ns: u32,
    pub key: &'a str,
}

impl quick_cache::Equivalent<NsKey> for NsKeyRef<'_> {
    fn equivalent(&self, k: &NsKey) -> bool {
        self.ns == k.ns && self.key == k.key
    }
}

#[derive(Clone)]
pub(crate) struct Weighter;
impl quick_cache::Weighter<NsKey, Arc<Rows>> for Weighter {
    fn weight(&self, k: &NsKey, v: &Arc<Rows>) -> u64 {
        v.bytes() + k.key.len() as u64
    }
}

type Inner = quick_cache::sync::Cache<NsKey, Arc<Rows>, Weighter>;

/// The most shards a cache of query results has. quick_cache splits the byte budget
/// evenly between its shards and refuses an entry heavier than 97% of one shard's share.
/// The SERVICE cache and the result cache keep an entry up to an eighth of the whole
/// budget, so every kept entry fits its shard only when a shard holds at least twice
/// that, which means at most four shards. The default of four shards per CPU gave a
/// shard less than an eighth of a small budget on a host with 32 or more CPUs, and the
/// cache dropped entries that `put` had accepted. Four shards cost little, because each
/// lookup takes a read lock and comes once per cached subtree or remote request, not
/// once per row.
pub(crate) const MAX_SHARDS: usize = 4;

/// A cache of at most `bytes` weighed by `weighter`, with `shards` shards but at most
/// [`MAX_SHARDS`].
pub(crate) fn bounded_cache<K, V, W>(
    bytes: u64,
    shards: usize,
    weighter: W,
) -> quick_cache::sync::Cache<K, V, W>
where
    K: Eq + std::hash::Hash,
    V: Clone,
    W: quick_cache::Weighter<K, V> + Clone,
{
    let options = quick_cache::OptionsBuilder::new()
        .estimated_items_capacity(10_000)
        .weight_capacity(bytes)
        .shards(shards.min(MAX_SHARDS))
        .build()
        .expect("the cache options are complete");
    quick_cache::sync::Cache::with_options(
        options,
        weighter,
        Default::default(),
        Default::default(),
    )
}

/// The shards a cache asks for on this host before [`MAX_SHARDS`] applies: four per CPU,
/// quick_cache's default.
pub(crate) fn host_shards() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()) * 4
}

/// The storage of remote SERVICE results that stores can share.
pub struct ServiceStore {
    cache: Option<Inner>,
    max_entry_bytes: u64,
}

impl ServiceStore {
    /// Storage of at most `bytes` (0 turns it off).
    pub fn new(bytes: u64) -> ServiceStore {
        Self::with_shards(bytes, host_shards())
    }

    fn with_shards(bytes: u64, shards: usize) -> ServiceStore {
        ServiceStore {
            cache: (bytes > 0).then(|| bounded_cache(bytes, shards, Weighter)),
            max_entry_bytes: bytes / 8,
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

/// A store's view of the cache of remote SERVICE results.
pub struct ServiceCache {
    store: Arc<ServiceStore>,
    ns: u32,
    /// whether other views share the storage
    shared: bool,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl ServiceCache {
    /// A cache of its own, of at most `bytes` (0 turns it off).
    pub fn new(bytes: u64) -> ServiceCache {
        ServiceCache::view(Arc::new(ServiceStore::new(bytes)), 0, false)
    }

    /// A view of `store` for namespace `ns`.
    pub fn attach(store: &Arc<ServiceStore>, ns: u32) -> ServiceCache {
        ServiceCache::view(store.clone(), ns, true)
    }

    pub(crate) fn view(store: Arc<ServiceStore>, ns: u32, shared: bool) -> ServiceCache {
        ServiceCache {
            store,
            ns,
            shared,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.store.cache.is_some()
    }

    pub fn get(&self, key: &str) -> Option<Arc<Rows>> {
        let c = self.store.cache.as_ref()?;
        let r = c.get(&NsKeyRef { ns: self.ns, key });
        let n = if r.is_some() {
            &self.hits
        } else {
            &self.misses
        };
        n.fetch_add(1, Ordering::Relaxed);
        r
    }

    /// Keep `rows` under `key` unless they are larger than an eighth of the budget.
    pub fn put(&self, key: String, rows: Arc<Rows>) {
        let Some(c) = &self.store.cache else { return };
        if rows.bytes() + key.len() as u64 > self.store.max_entry_bytes {
            return;
        }
        c.insert(NsKey { ns: self.ns, key }, rows);
    }

    pub fn remove(&self, key: &str) {
        if let Some(c) = &self.store.cache {
            c.remove(&NsKeyRef { ns: self.ns, key });
        }
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

/// The key of one request: the caller's scope, the endpoint, the SERVICE pattern as
/// written and the values substituted into it, by variable name.
pub fn key(scope: &str, url: &str, pattern: &str, values: &[(&str, &Term)]) -> String {
    let mut s = String::with_capacity(scope.len() + url.len() + pattern.len() + 64);
    let _ = write!(s, "{scope}\u{0}{url}\u{0}{pattern}\u{0}");
    let mut vs: Vec<_> = values.to_vec();
    vs.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (v, t) in vs {
        let _ = write!(s, "?{v}={t};");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Literal, NamedNode};

    fn rows(n: usize) -> Arc<Rows> {
        Arc::new(Rows {
            vars: vec!["x".into()],
            rows: (0..n)
                .map(|i| vec![Some(Term::Literal(Literal::from(i as i64)))])
                .collect(),
        })
    }

    #[test]
    fn bounded_and_keyed() {
        let c = ServiceCache::new(64 << 10);
        let a = NamedNode::new("http://e/a").unwrap().into();
        let k = key("alice", "http://e/sparql", "?s ?p ?o .", &[("s", &a)]);
        assert!(c.get(&k).is_none());
        c.put(k.clone(), rows(3));
        assert_eq!(c.get(&k).unwrap().rows.len(), 3);
        assert_eq!((c.hits(), c.misses()), (1, 1));
        // another caller's key differs
        let k2 = key("bob", "http://e/sparql", "?s ?p ?o .", &[("s", &a)]);
        assert!(c.get(&k2).is_none());
        // an entry over an eighth of the budget is not kept
        let big = key("alice", "http://e/sparql", "big", &[]);
        c.put(big.clone(), rows(2_000));
        assert!(c.get(&big).is_none());
        c.remove(&k);
        assert!(c.get(&k).is_none());
        assert!(!ServiceCache::new(0).enabled());
    }

    /// The largest entry `put` accepts is kept, whatever number of shards the host asks
    /// for. The default asks for four per CPU, so 128 on a host with 32 CPUs.
    #[test]
    fn entries_up_to_an_eighth_fit_their_shard_on_any_host() {
        let budget = 64u64 << 10;
        let key = "k0".to_string();
        let mut n = 1;
        while rows(n + 1).bytes() + key.len() as u64 <= budget / 8 {
            n += 1;
        }
        for shards in [1, 4, 16, 64, 128, 256, 1024] {
            let store = Arc::new(ServiceStore::with_shards(budget, shards));
            let c = ServiceCache::view(store, 0, false);
            c.put(key.clone(), rows(n));
            assert!(c.get(&key).is_some(), "{shards} shards dropped the entry");
        }
    }

    #[test]
    fn bounded_shared_storage_keeps_namespaces_isolated() {
        for shards in [1, 4, 16, 64, 128, 256, 1024] {
            let store = Arc::new(ServiceStore::with_shards(64 << 10, shards));
            let one = ServiceCache::attach(&store, 11);
            let two = ServiceCache::attach(&store, 22);
            let first = rows(3);
            let second = rows(4);
            one.put("same key".into(), first.clone());
            assert!(two.get("same key").is_none());
            two.put("same key".into(), second.clone());
            assert!(Arc::ptr_eq(&one.get("same key").unwrap(), &first));
            assert!(Arc::ptr_eq(&two.get("same key").unwrap(), &second));
            let usage = store.usage();
            assert_eq!(usage[&11].entries, 1);
            assert_eq!(usage[&22].entries, 1);
            assert_eq!(one.bytes() + two.bytes(), store.bytes());
            one.remove("same key");
            assert!(one.get("same key").is_none());
            assert!(Arc::ptr_eq(&two.get("same key").unwrap(), &second));
            one.put("same key".into(), first);
            one.clear();
            assert_eq!(one.entries(), 0);
            assert_eq!(one.bytes(), 0);
            assert!(Arc::ptr_eq(&two.get("same key").unwrap(), &second));
            assert_eq!(two.entries(), store.entries());
            assert_eq!(two.bytes(), store.bytes());
            assert!(store.bytes() <= store.capacity());
        }
    }

    #[test]
    fn key_ignores_value_order() {
        let a: Term = NamedNode::new("http://e/a").unwrap().into();
        let b: Term = Literal::from(1).into();
        assert_eq!(
            key("", "u", "p", &[("a", &a), ("b", &b)]),
            key("", "u", "p", &[("b", &b), ("a", &a)])
        );
    }
}
