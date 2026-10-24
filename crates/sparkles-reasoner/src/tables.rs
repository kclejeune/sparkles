//! The cache of derived tables of query-time reasoning (spec F17 §6.1).
//!
//! A table is what one demand of a query derived: the facts of the session that answered
//! it, which hold every fact the demand matches and possibly more, all entailed. The cache
//! keys a table by the view it was derived from (the generation instance, the commit, the
//! mask of a view that hides triples and the input graphs the caller reads), the digest
//! of the rules, the demand with its constants, and a hash of its sorted seeds. A later
//! query on the same view with the same demand reads the table instead of deriving it.
//!
//! The cache is process-wide and in memory, bounded in bytes ([`set_capacity`]), and
//! evicts the table used longest ago. A table carries the keys of the IRIs and literals
//! its facts hold that the store does not have, so that a later session gives them its
//! own ids. Tables whose facts hold a blank node a rule made are not cached, since such a
//! node belongs to the session that made it.

use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The cache's capacity by default: 256 MiB of facts.
pub const DEFAULT_CAPACITY: u64 = 256 << 20;

/// A chunk of a session's facts, shared by the tables made from the session.
pub(crate) type Chunk = Arc<Vec<[u64; 3]>>;

/// What a table holds: the facts, in chunks.
pub(crate) struct Table {
    pub chunks: Vec<Chunk>,
    /// the session's local ids of the facts' terms the store does not have, with keys
    pub keys: Vec<(u64, Vec<u8>)>,
}

impl Table {
    pub fn facts(&self) -> impl Iterator<Item = &[u64; 3]> {
        self.chunks.iter().flat_map(|c| c.iter())
    }

    fn bytes(&self) -> u64 {
        self.chunks
            .iter()
            .map(|c| (c.len() * std::mem::size_of::<[u64; 3]>()) as u64 + 64)
            .sum::<u64>()
            + self
                .keys
                .iter()
                .map(|(_, k)| k.len() as u64 + 40)
                .sum::<u64>()
            + 64
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    /// the view: generation instance, commit, mask, input graphs
    pub view: String,
    /// the rules
    pub digest: u64,
    /// the demand: its adornment and constants
    pub demand: String,
    /// the sorted seeds: their number and a hash
    pub seeds: (usize, u64),
}

struct Entry {
    table: Arc<Table>,
    bytes: u64,
    used: u64,
}

#[derive(Default)]
struct Inner {
    map: FxHashMap<Key, Entry>,
    bytes: u64,
    tick: u64,
}

struct Cache {
    inner: Mutex<Inner>,
    capacity: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
}

static CACHE: std::sync::LazyLock<Cache> = std::sync::LazyLock::new(|| Cache {
    inner: Mutex::new(Inner::default()),
    capacity: AtomicU64::new(DEFAULT_CAPACITY),
    hits: AtomicU64::new(0),
    misses: AtomicU64::new(0),
});

/// Set the cache's capacity in bytes (0 turns it off and empties it).
pub fn set_capacity(bytes: u64) {
    CACHE.capacity.store(bytes, Ordering::Relaxed);
    let mut inner = CACHE.inner.lock();
    evict(&mut inner, bytes);
}

/// The cache's capacity in bytes.
pub fn capacity() -> u64 {
    CACHE.capacity.load(Ordering::Relaxed)
}

/// Statistics of the cache: (tables, bytes, hits, misses).
pub fn stats() -> (usize, u64, u64, u64) {
    let inner = CACHE.inner.lock();
    (
        inner.map.len(),
        inner.bytes,
        CACHE.hits.load(Ordering::Relaxed),
        CACHE.misses.load(Ordering::Relaxed),
    )
}

/// Empty the cache.
pub fn clear() {
    let mut inner = CACHE.inner.lock();
    inner.map.clear();
    inner.bytes = 0;
}

fn evict(inner: &mut Inner, cap: u64) {
    while inner.bytes > cap && !inner.map.is_empty() {
        let oldest = inner
            .map
            .iter()
            .min_by_key(|(_, e)| e.used)
            .map(|(k, _)| k.clone())
            .expect("not empty");
        if let Some(e) = inner.map.remove(&oldest) {
            inner.bytes -= e.bytes;
        }
    }
}

pub(crate) fn get(key: &Key) -> Option<Arc<Table>> {
    let mut inner = CACHE.inner.lock();
    inner.tick += 1;
    let tick = inner.tick;
    match inner.map.get_mut(key) {
        Some(e) => {
            e.used = tick;
            CACHE.hits.fetch_add(1, Ordering::Relaxed);
            Some(e.table.clone())
        }
        None => {
            CACHE.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

pub(crate) fn put(key: Key, table: Arc<Table>) {
    let cap = capacity();
    let bytes = table.bytes();
    if bytes > cap {
        return;
    }
    let mut inner = CACHE.inner.lock();
    inner.tick += 1;
    let used = inner.tick;
    if let Some(old) = inner.map.insert(key, Entry { table, bytes, used }) {
        inner.bytes -= old.bytes;
    }
    inner.bytes += bytes;
    evict(&mut inner, cap);
}
