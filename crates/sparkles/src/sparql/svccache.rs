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

#[derive(Clone)]
struct Weighter;
impl quick_cache::Weighter<String, Arc<Rows>> for Weighter {
    fn weight(&self, k: &String, v: &Arc<Rows>) -> u64 {
        v.bytes() + k.len() as u64
    }
}

/// A store's cache of remote SERVICE results.
pub struct ServiceCache {
    cache: Option<quick_cache::sync::Cache<String, Arc<Rows>, Weighter>>,
    max_entry_bytes: u64,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl ServiceCache {
    /// A cache of at most `bytes` (0 turns it off).
    pub fn new(bytes: u64) -> ServiceCache {
        ServiceCache {
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

    pub fn get(&self, key: &str) -> Option<Arc<Rows>> {
        let c = self.cache.as_ref()?;
        let r = c.get(key);
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
        let Some(c) = &self.cache else { return };
        if rows.bytes() + key.len() as u64 > self.max_entry_bytes {
            return;
        }
        c.insert(key, rows);
    }

    pub fn remove(&self, key: &str) {
        if let Some(c) = &self.cache {
            c.remove(key);
        }
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

    pub fn capacity(&self) -> u64 {
        self.cache.as_ref().map_or(0, |c| c.capacity())
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
