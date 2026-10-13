//! The cache of built projections (spec F14 §2.4).
//!
//! Building a projection reads the index ranges of its predicates once, which takes
//! seconds for tens of millions of edges. Calls on the same data and the same projection
//! share one build. The key names the data (generation, commit, the graphs the caller
//! sees) and the projection, so a call at a new commit builds a new projection and the
//! older one leaves the cache when it is the least recently used. The cache is bounded by
//! bytes, and a build larger than the bound is not kept.

use super::Graph;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static LIMIT: AtomicU64 = AtomicU64::new(256 << 20);

/// Set the most bytes of projections the cache keeps (`--algo-cache-mb`). Zero turns the
/// cache off.
pub fn set_limit(bytes: u64) {
    LIMIT.store(bytes, Ordering::Relaxed);
    let mut c = CACHE.lock();
    c.shrink(bytes);
}

struct Entry {
    graph: Arc<Graph>,
    bytes: u64,
    used: u64,
}

#[derive(Default)]
struct Cache {
    map: HashMap<String, Entry>,
    bytes: u64,
    tick: u64,
}

impl Cache {
    fn shrink(&mut self, limit: u64) {
        while self.bytes > limit {
            let Some(k) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(e) = self.map.remove(&k) {
                self.bytes -= e.bytes;
            }
        }
    }
}

static CACHE: std::sync::LazyLock<Mutex<Cache>> =
    std::sync::LazyLock::new(|| Mutex::new(Cache::default()));

/// The cached projection under `key`.
pub fn get(key: &str) -> Option<Arc<Graph>> {
    let mut c = CACHE.lock();
    c.tick += 1;
    let tick = c.tick;
    c.map.get_mut(key).map(|e| {
        e.used = tick;
        e.graph.clone()
    })
}

/// Keep `graph` under `key`, evicting the least recently used projections to make room.
pub fn put(key: String, graph: Arc<Graph>) {
    let limit = LIMIT.load(Ordering::Relaxed);
    // the in-edges that an algorithm may build later are about as large as the out-edges
    let bytes = graph.bytes()
        + if graph.symmetric {
            0
        } else {
            graph.out.bytes()
        };
    if bytes > limit {
        return;
    }
    let mut c = CACHE.lock();
    c.tick += 1;
    let tick = c.tick;
    if let Some(old) = c.map.insert(
        key,
        Entry {
            graph,
            bytes,
            used: tick,
        },
    ) {
        c.bytes -= old.bytes;
    }
    c.bytes += bytes;
    c.shrink(limit);
}

/// The bytes and the number of projections the cache holds.
pub fn stats() -> (u64, usize) {
    let c = CACHE.lock();
    (c.bytes, c.map.len())
}

/// Drop every cached projection.
pub fn clear() {
    let mut c = CACHE.lock();
    c.map.clear();
    c.bytes = 0;
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn evicts_the_least_recently_used() {
        clear();
        let g = Arc::new(digraph(100, &[(0, 1)]));
        let one = g.bytes() + g.out.bytes();
        set_limit(one * 2 + 1);
        put("a".into(), g.clone());
        put("b".into(), g.clone());
        assert!(get("a").is_some());
        put("c".into(), g);
        assert!(get("a").is_some());
        assert!(get("b").is_none(), "b was the least recently used");
        set_limit(256 << 20);
        clear();
    }
}
