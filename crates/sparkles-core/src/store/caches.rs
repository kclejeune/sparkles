//! Caches that several stores share (spec X08 §3).
//!
//! A server or an embedding catalog builds one [`SharedCaches`] and gives each dataset a
//! [`CacheScope`] of it through [`StoreOptions::shared`](super::StoreOptions::shared).
//! The block, result and SERVICE caches are then sized once for the process and every
//! store reads through a view that counts its own hits, misses and bytes. The history
//! cache is a list of materialized states per store, so the stores share a
//! [`ByteBudget`] instead.

use crate::index::SharedBlocks;
use crate::sparql::cache::ResultStore;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// A byte total with a capacity that several owners charge. The budget does not evict
/// anything. An owner that finds it over capacity gives back its own bytes.
#[derive(Debug)]
pub struct ByteBudget {
    capacity: AtomicU64,
    used: AtomicU64,
}

impl ByteBudget {
    pub fn new(capacity: u64) -> ByteBudget {
        ByteBudget {
            capacity: AtomicU64::new(capacity),
            used: AtomicU64::new(0),
        }
    }
    pub fn capacity(&self) -> u64 {
        self.capacity.load(Ordering::Relaxed)
    }
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }
    pub fn over(&self) -> bool {
        self.used() > self.capacity()
    }
    /// Move the charge of one owner from `from` to `to` bytes.
    pub(crate) fn settle(&self, from: u64, to: u64) {
        if to >= from {
            self.used.fetch_add(to - from, Ordering::Relaxed);
        } else {
            self.used.fetch_sub(from - to, Ordering::Relaxed);
        }
    }
}

/// The sizes of the caches a server shares, in bytes. 0 disables the result and SERVICE
/// caches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheSizes {
    pub blocks: u64,
    pub results: u64,
    pub service: u64,
    pub history: u64,
}

impl Default for CacheSizes {
    /// The default memory profile (spec X08 §3.3).
    fn default() -> Self {
        CacheSizes {
            blocks: 1 << 30,
            results: 256 << 20,
            service: 64 << 20,
            history: 512 << 20,
        }
    }
}

impl CacheSizes {
    /// The `small` memory profile, for constrained hosts.
    pub fn small() -> CacheSizes {
        CacheSizes {
            blocks: 256 << 20,
            results: 64 << 20,
            service: 16 << 20,
            history: 128 << 20,
        }
    }
}

struct Inner {
    sizes: CacheSizes,
    blocks: SharedBlocks,
    results: Arc<ResultStore>,
    history: Arc<ByteBudget>,
    next_ns: AtomicU32,
}

/// The block, result, SERVICE and history caches of a whole server or catalog.
#[derive(Clone)]
pub struct SharedCaches {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SharedCaches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedCaches")
            .field("sizes", &self.inner.sizes)
            .finish()
    }
}

impl SharedCaches {
    pub fn new(sizes: CacheSizes) -> SharedCaches {
        SharedCaches {
            inner: Arc::new(Inner {
                sizes,
                blocks: SharedBlocks::new(sizes.blocks),
                results: Arc::new(ResultStore::new(sizes.results, sizes.service)),
                history: Arc::new(ByteBudget::new(sizes.history)),
                next_ns: AtomicU32::new(1),
            }),
        }
    }

    pub fn sizes(&self) -> CacheSizes {
        self.inner.sizes
    }
    pub fn blocks(&self) -> &SharedBlocks {
        &self.inner.blocks
    }
    pub fn results(&self) -> &Arc<ResultStore> {
        &self.inner.results
    }
    pub fn history(&self) -> &Arc<ByteBudget> {
        &self.inner.history
    }

    /// A scope with a namespace of its own, for one dataset and its branches.
    pub fn scope(&self) -> CacheScope {
        // the block cache keeps 24 bits of namespace, so the numbers wrap after 16M
        // datasets opened by one process, which only blurs the accounting of old ones
        let ns = self.inner.next_ns.fetch_add(1, Ordering::Relaxed) & 0xff_ffff;
        CacheScope {
            caches: self.clone(),
            ns: ns.max(1),
        }
    }
}

/// A dataset's share of [`SharedCaches`]: the caches and the namespace its entries use.
#[derive(Clone, Debug)]
pub struct CacheScope {
    pub caches: SharedCaches,
    pub ns: u32,
}

impl CacheScope {
    pub(crate) fn block_cache(&self) -> Arc<crate::index::BlockCache> {
        Arc::new(crate::index::BlockCache::attach(
            self.caches.blocks(),
            self.ns,
        ))
    }
    pub(crate) fn result_cache(&self, min_ms: f64) -> Arc<crate::sparql::cache::ResultCache> {
        Arc::new(crate::sparql::cache::ResultCache::attach(
            self.caches.results(),
            self.ns,
            min_ms,
        ))
    }
}
