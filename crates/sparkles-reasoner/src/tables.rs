//! The cache of derived tables of query-time reasoning (spec F17 §6.1).
//!
//! A table is what one demand of a query derived: the facts of the session that answered
//! it, which hold every fact the demand matches and possibly more, all entailed. The cache
//! keys a table by the view it was derived from (the generation instance, the commit, the
//! mask of a view that hides triples and the input graphs the caller reads), the digest
//! of the rules, the demand with its constants, and a hash of its sorted seeds. A later
//! query on the same view with the same demand reads the table instead of deriving it.
//!
//! The cache is process-wide and in memory, bounded by a retained-table estimate
//! ([`set_capacity`]), and
//! evicts the table used longest ago. A table carries the keys of the IRIs and literals
//! its facts hold that the store does not have, so that a later session gives them its
//! own ids. Tables whose facts hold a blank node a rule made are not cached, since such a
//! node belongs to the session that made it.

use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The cache's capacity by default: 256 MiB of retained-table estimates.
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
    #[cfg(test)]
    before_insert_lock: Option<Arc<dyn Fn() + Send + Sync>>,
}

static CACHE: std::sync::LazyLock<Cache> =
    std::sync::LazyLock::new(|| Cache::new(DEFAULT_CAPACITY));

/// A coherent snapshot of the process-wide cache's retained-table accounting.
///
/// The estimate counts a shared chunk for each table retaining it. It is neither
/// allocator usage nor RSS, and excludes tables kept alive only by a caller.
/// Each table contributes 64 bytes, each chunk contributes 24 bytes per fact plus
/// 64 bytes, and each local term contributes its byte length plus 40 bytes. This
/// weighting does not count spare vector capacity, cache-key strings, hash-table
/// capacity or exact allocator metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheStats {
    pub capacity_bytes: u64,
    pub estimated_bytes: u64,
    pub tables: usize,
    /// Successful lookups since process start, including subsequently evicted entries.
    pub hits: u64,
    /// Unsuccessful lookups since process start, including while retention is disabled.
    pub misses: u64,
}

impl Cache {
    fn new(capacity: u64) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            capacity: AtomicU64::new(capacity),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            #[cfg(test)]
            before_insert_lock: None,
        }
    }

    fn set_capacity(&self, bytes: u64) {
        let mut inner = self.inner.lock();
        self.capacity.store(bytes, Ordering::Relaxed);
        evict(&mut inner, bytes);
    }

    fn snapshot(&self) -> CacheStats {
        let inner = self.inner.lock();
        CacheStats {
            capacity_bytes: self.capacity.load(Ordering::Relaxed),
            estimated_bytes: inner.bytes,
            tables: inner.map.len(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    fn put(&self, key: Key, table: Arc<Table>) {
        let bytes = table.bytes();
        #[cfg(test)]
        if let Some(before_lock) = &self.before_insert_lock {
            before_lock();
        }
        let mut inner = self.inner.lock();
        // A shrink that completed while this insertion waited must win.
        let cap = self.capacity.load(Ordering::Relaxed);
        if bytes > cap {
            return;
        }
        inner.tick += 1;
        let used = inner.tick;
        if let Some(old) = inner.map.insert(key, Entry { table, bytes, used }) {
            inner.bytes -= old.bytes;
        }
        inner.bytes += bytes;
        evict(&mut inner, cap);
    }
}

/// Set the cache's capacity in bytes (0 turns it off and empties it).
pub fn set_capacity(bytes: u64) {
    CACHE.set_capacity(bytes);
}

/// The cache's capacity in bytes.
pub fn capacity() -> u64 {
    CACHE.capacity.load(Ordering::Relaxed)
}

/// Statistics of the cache: (tables, bytes, hits, misses).
pub fn stats() -> (usize, u64, u64, u64) {
    let s = snapshot();
    (s.tables, s.estimated_bytes, s.hits, s.misses)
}

/// Capacity, retained estimates and lookup counters from one cache observation.
pub fn snapshot() -> CacheStats {
    CACHE.snapshot()
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
    CACHE.put(key, table);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn key(name: &str) -> Key {
        Key {
            view: "test-view".into(),
            digest: 1,
            demand: name.into(),
            seeds: (0, 0),
        }
    }

    fn table(value: u64) -> Arc<Table> {
        Arc::new(Table {
            chunks: vec![Arc::new(vec![[value, 2, 3]])],
            keys: Vec::new(),
        })
    }

    #[test]
    fn shrinking_and_disabling_evict_retained_tables_but_not_borrowers() {
        let first = table(1);
        let second = table(4);
        let bytes = first.bytes();
        let cache = Cache::new(bytes * 2);
        cache.put(key("first"), first);
        cache.put(key("second"), second.clone());
        assert_eq!(cache.snapshot().estimated_bytes, bytes * 2);
        cache.set_capacity(bytes);
        let s = cache.snapshot();
        assert_eq!(
            (s.capacity_bytes, s.estimated_bytes, s.tables),
            (bytes, bytes, 1)
        );
        assert!(cache.inner.lock().map.contains_key(&key("second")));
        cache.set_capacity(0);
        let s = cache.snapshot();
        assert_eq!((s.capacity_bytes, s.estimated_bytes, s.tables), (0, 0, 0));
        assert_eq!(second.facts().copied().collect::<Vec<_>>(), [[4, 2, 3]]);
        cache.put(key("disabled"), second);
        assert_eq!(cache.snapshot().tables, 0);
    }

    #[test]
    fn an_insertion_paused_before_lock_obeys_a_completed_shrink() {
        let (entered, observe) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let proceed = Mutex::new(proceed);
        let mut cache = Cache::new(4096);
        cache.before_insert_lock = Some(Arc::new(move || {
            entered.send(()).unwrap();
            proceed
                .lock()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }));
        let cache = Arc::new(cache);
        let writer = cache.clone();
        let insertion = std::thread::spawn(move || writer.put(key("waited"), table(1)));
        observe.recv_timeout(Duration::from_secs(10)).unwrap();
        cache.set_capacity(0);
        release.send(()).unwrap();
        insertion.join().unwrap();
        let s = cache.snapshot();
        assert_eq!((s.capacity_bytes, s.estimated_bytes, s.tables), (0, 0, 0));
    }

    #[test]
    fn oversized_tables_are_not_admitted_and_replacement_is_counted_once() {
        let t = table(1);
        let bytes = t.bytes();
        let cache = Cache::new(bytes - 1);
        cache.put(key("large"), t.clone());
        assert_eq!(cache.snapshot().tables, 0);
        cache.set_capacity(bytes);
        cache.put(key("same"), t.clone());
        cache.put(key("same"), t);
        let s = cache.snapshot();
        assert_eq!(
            (s.capacity_bytes, s.estimated_bytes, s.tables),
            (bytes, bytes, 1)
        );
    }
}
