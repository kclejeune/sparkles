//! The cache of derived tables of query-time reasoning (spec F17 §6.1).
//!
//! A table is what one demand of a query derived: the facts of the session that answered
//! it, which hold every fact the demand matches and possibly more, all entailed. The cache
//! keys a table by the view it was derived from (the generation instance, the commit, the
//! mask of a view that hides triples and the input graphs the caller reads), the digest
//! of the rules, the demand with its constants, and a hash of its sorted seeds. A later
//! query on the same view with the same demand reads the table instead of deriving it.
//!
//! The cache is process-wide and in memory, bounded by a calibrated retained-byte estimate
//! ([`set_capacity`]), and evicts entries when occupancy exceeds the effective budget.
//! Retained accounting tracks deduplicated `Arc<Table>` and `Chunk` allocations (including
//! spare `Vec` capacity), local-term key buffers and their capacities, cache-key `String`
//! capacities, hash-table bucket capacities, and conservative per-allocation allocator
//! metadata. Eviction prefers unborrowed entries so memory is reclaimed immediately,
//! shrinks oversized hash tables when that alone restores the budget, and counts evicted
//! tables or chunks still pinned by callers against the eviction allowance until those
//! callers drop them. A table carries the keys of the IRIs and literals its facts hold
//! that the store does not have, so that a later session gives them its own ids. Tables
//! whose facts hold a blank node a rule made are not cached, since such a node belongs to
//! the session that made it.

use hashbrown::HashMap;
use parking_lot::Mutex;
use rustc_hash::{FxBuildHasher, FxHashSet};
use sparkles_budget::sizes::{table_bytes_for, table_capacity};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

/// The cache's capacity by default: 256 MiB of retained-table estimates.
pub const DEFAULT_CAPACITY: u64 = 256 << 20;

/// Conservative allocator metadata and alignment overhead charged per non-empty heap
/// allocation (covers glibc chunk headers/alignment and typical small-class allocator
/// metadata).
const ALLOC_METADATA_BYTES: u64 = 16;

/// Exact layout of `std::sync::Arc`'s heap allocation (`ArcInner<T>`).
#[repr(C)]
struct ArcInnerLayout<T> {
    strong: AtomicUsize,
    weak: AtomicUsize,
    data: T,
}

#[inline]
fn alloc_bytes(requested: usize) -> u64 {
    if requested == 0 {
        0
    } else {
        (requested as u64).saturating_add(ALLOC_METADATA_BYTES)
    }
}

#[inline]
fn vec_bytes<T>(cap: usize) -> u64 {
    alloc_bytes(cap.saturating_mul(std::mem::size_of::<T>()))
}

#[inline]
fn arc_box_bytes<T>() -> u64 {
    alloc_bytes(std::mem::size_of::<ArcInnerLayout<T>>())
}

#[inline]
fn min_map_bytes<K, V>(len: usize) -> u64 {
    alloc_bytes(table_bytes_for::<(K, V)>(table_capacity(len)))
}

type CacheMap<K, V> = HashMap<K, V, FxBuildHasher>;

/// A chunk of a session's facts, shared by the tables made from the session.
pub(crate) type Chunk = Arc<Vec<[u64; 3]>>;

/// Estimated retained heap of one shared [`Chunk`]: its `Arc` allocation plus the backing
/// `Vec<[u64; 3]>` capacity (including spare capacity) and allocator metadata.
fn chunk_bytes(chunk: &Chunk) -> u64 {
    arc_box_bytes::<Vec<[u64; 3]>>().saturating_add(vec_bytes::<[u64; 3]>(chunk.capacity()))
}

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

    /// Estimated retained heap directly owned by this `Arc<Table>`, excluding the payloads
    /// of its shared [`Chunk`]s (counted separately with deduplication).
    fn own_bytes(&self) -> u64 {
        let key_bufs = self
            .keys
            .iter()
            .map(|(_, k)| vec_bytes::<u8>(k.capacity()))
            .fold(0u64, u64::saturating_add);
        arc_box_bytes::<Table>()
            .saturating_add(vec_bytes::<Chunk>(self.chunks.capacity()))
            .saturating_add(vec_bytes::<(u64, Vec<u8>)>(self.keys.capacity()))
            .saturating_add(key_bufs)
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

impl Key {
    fn heap_bytes(&self) -> u64 {
        vec_bytes::<u8>(self.view.capacity())
            .saturating_add(vec_bytes::<u8>(self.demand.capacity()))
    }
}

struct Entry {
    table: Arc<Table>,
    key_bytes: u64,
    used: u64,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
struct Victim {
    borrowed: bool,
    used: u64,
}

struct SharedHold {
    refs: usize,
    bytes: u64,
}

struct EvictedHold<T> {
    weak: Weak<T>,
    bytes: u64,
}

/// Every live allocation has at most one weak record and is charged in either the
/// active or the evicted maps. The Weak reserves the address while it is indexed.
struct Evicted<T> {
    holds: CacheMap<usize, EvictedHold<T>>,
    payload_bytes: u64,
}

impl<T> Default for Evicted<T> {
    fn default() -> Self {
        Self {
            holds: CacheMap::default(),
            payload_bytes: 0,
        }
    }
}

impl<T> Evicted<T> {
    fn bytes(&self) -> u64 {
        self.payload_bytes
            .saturating_add(alloc_bytes(self.holds.allocation_size()))
    }

    fn min_bytes(&self) -> u64 {
        self.payload_bytes
            .saturating_add(min_map_bytes::<usize, EvictedHold<T>>(self.holds.len()))
    }

    fn remember(&mut self, weak: Weak<T>, bytes: u64) {
        let ptr = weak.as_ptr() as usize;
        if weak.strong_count() > 0
            && let hashbrown::hash_map::Entry::Vacant(entry) = self.holds.entry(ptr)
        {
            entry.insert(EvictedHold { weak, bytes });
            self.payload_bytes = self.payload_bytes.saturating_add(bytes);
        }
    }

    fn forget(&mut self, ptr: usize) {
        if let Some(hold) = self.holds.remove(&ptr) {
            self.payload_bytes = self.payload_bytes.saturating_sub(hold.bytes);
            self.compact();
        }
    }

    fn prune(&mut self) {
        self.holds.retain(|_, hold| {
            if hold.weak.strong_count() > 0 {
                true
            } else {
                self.payload_bytes = self.payload_bytes.saturating_sub(hold.bytes);
                false
            }
        });
        self.compact();
    }

    fn compact(&mut self) {
        if alloc_bytes(self.holds.allocation_size())
            > min_map_bytes::<usize, EvictedHold<T>>(self.holds.len()).saturating_mul(4)
        {
            self.holds.shrink_to_fit();
        }
    }

    fn shrink_to_fit(&mut self) {
        self.holds.shrink_to_fit();
    }
}

#[derive(Default)]
struct Inner {
    map: CacheMap<Key, Entry>,
    /// Distinct `Arc<Table>` allocations retained by `map`, keyed by `Arc::as_ptr as usize`.
    tables: CacheMap<usize, SharedHold>,
    /// Distinct `Chunk` allocations retained by `tables`, keyed by `Arc::as_ptr as usize`.
    chunks: CacheMap<usize, SharedHold>,
    /// Sum of `Key::heap_bytes` across entries currently in `map`.
    key_bytes: u64,
    /// Sum of `Table::own_bytes` across distinct tables currently in `tables`.
    table_bytes: u64,
    /// Sum of `chunk_bytes` across distinct chunks currently in `chunks`.
    chunk_bytes: u64,
    /// Evicted tables and chunks still kept alive by external callers.
    evicted_tables: Evicted<Table>,
    evicted_chunks: Evicted<Vec<[u64; 3]>>,
    tick: u64,
}

impl Inner {
    fn payload_bytes(&self) -> u64 {
        self.key_bytes
            .saturating_add(self.table_bytes)
            .saturating_add(self.chunk_bytes)
    }

    fn map_overhead_bytes(&self) -> u64 {
        alloc_bytes(self.map.allocation_size())
            .saturating_add(alloc_bytes(self.tables.allocation_size()))
            .saturating_add(alloc_bytes(self.chunks.allocation_size()))
    }

    fn min_map_overhead_bytes(&self) -> u64 {
        min_map_bytes::<Key, Entry>(self.map.len())
            .saturating_add(min_map_bytes::<usize, SharedHold>(self.tables.len()))
            .saturating_add(min_map_bytes::<usize, SharedHold>(self.chunks.len()))
    }

    fn retained_bytes(&self) -> u64 {
        if self.map.is_empty() {
            return 0;
        }
        self.payload_bytes()
            .saturating_add(self.map_overhead_bytes())
    }

    fn shrink_maps(&mut self) {
        self.map.shrink_to_fit();
        self.tables.shrink_to_fit();
        self.chunks.shrink_to_fit();
    }

    fn reset_maps(&mut self) {
        self.map = CacheMap::default();
        self.tables = CacheMap::default();
        self.chunks = CacheMap::default();
        self.key_bytes = 0;
        self.table_bytes = 0;
        self.chunk_bytes = 0;
    }

    fn clear_all(&mut self) {
        self.reset_maps();
        self.evicted_tables = Evicted::default();
        self.evicted_chunks = Evicted::default();
    }

    fn retain_table(&mut self, table: &Arc<Table>) {
        let tptr = Arc::as_ptr(table) as usize;
        if let Some(hold) = self.tables.get_mut(&tptr) {
            hold.refs += 1;
            return;
        }
        self.evicted_tables.forget(tptr);
        let own = table.own_bytes();
        self.tables.insert(
            tptr,
            SharedHold {
                refs: 1,
                bytes: own,
            },
        );
        self.table_bytes = self.table_bytes.saturating_add(own);
        for chunk in &table.chunks {
            let cptr = Arc::as_ptr(chunk) as usize;
            if let Some(chold) = self.chunks.get_mut(&cptr) {
                chold.refs += 1;
            } else {
                self.evicted_chunks.forget(cptr);
                let cb = chunk_bytes(chunk);
                self.chunks.insert(cptr, SharedHold { refs: 1, bytes: cb });
                self.chunk_bytes = self.chunk_bytes.saturating_add(cb);
            }
        }
    }

    fn release_table(&mut self, table: Arc<Table>) {
        let tptr = Arc::as_ptr(&table) as usize;
        let hold = self.tables.get_mut(&tptr).expect("retained table");
        hold.refs -= 1;
        if hold.refs > 0 {
            return;
        }
        let own = hold.bytes;
        self.tables.remove(&tptr);
        self.table_bytes = self.table_bytes.saturating_sub(own);

        let table_held = Arc::strong_count(&table) > 1;
        let table_weak = table_held.then(|| Arc::downgrade(&table));
        let mut candidate_chunks = smallvec::SmallVec::<[_; 8]>::new();
        for chunk in &table.chunks {
            let cptr = Arc::as_ptr(chunk) as usize;
            let hold = self.chunks.get_mut(&cptr).expect("retained chunk");
            hold.refs -= 1;
            if hold.refs == 0 {
                let bytes = hold.bytes;
                self.chunks.remove(&cptr);
                self.chunk_bytes = self.chunk_bytes.saturating_sub(bytes);
                if table_held || Arc::strong_count(chunk) > 1 {
                    candidate_chunks.push((Arc::downgrade(chunk), bytes));
                }
            }
        }
        // Check the weak counts after dropping the cache's last strong table reference.
        drop(table);
        if let Some(weak) = table_weak {
            self.evicted_tables.remember(weak, own);
        }
        for (weak, bytes) in candidate_chunks {
            self.evicted_chunks.remember(weak, bytes);
        }
    }

    fn is_externally_held(
        &self,
        table: &Arc<Table>,
        admission: Option<&AdmissionFootprint>,
    ) -> bool {
        let tptr = Arc::as_ptr(table) as usize;
        let incoming_table = usize::from(admission.is_some_and(|plan| plan.incoming_table == tptr));
        if Arc::strong_count(table) > self.tables[&tptr].refs + incoming_table {
            return true;
        }
        table.chunks.iter().any(|chunk| {
            let ptr = Arc::as_ptr(chunk) as usize;
            let external = Arc::strong_count(chunk).saturating_sub(self.chunks[&ptr].refs);
            external > 0
                && external
                    > admission
                        .and_then(|plan| plan.chunks.get(&ptr))
                        .copied()
                        .unwrap_or(0)
        })
    }

    fn prune_evicted_held(&mut self) {
        self.evicted_tables.prune();
        self.evicted_chunks.prune();
    }

    fn caller_held_evicted_bytes(&self) -> u64 {
        self.evicted_tables
            .bytes()
            .saturating_add(self.evicted_chunks.bytes())
    }

    fn total_bytes(&self) -> u64 {
        self.retained_bytes()
            .saturating_add(self.caller_held_evicted_bytes())
    }

    /// Minimum total charge with the candidate and every other borrowed entry. Compute
    /// this before changing any ownership, including when an equal key is replaced.
    fn admission_victims(
        &self,
        key: &Key,
        table: &Arc<Table>,
        cap: u64,
    ) -> Option<Vec<Reverse<Victim>>> {
        let replacement = self.map.get(key);
        let mut footprint = AdmissionFootprint {
            incoming_table: Arc::as_ptr(table) as usize,
            ..AdmissionFootprint::default()
        };
        footprint.add_entry(
            replacement.map_or_else(|| key.heap_bytes(), |e| e.key_bytes),
            table,
        );
        // The incoming handle becomes a cached reference, as do its chunk
        // references when this is the first cached key for its table.
        if !self.tables.contains_key(&footprint.incoming_table) {
            for chunk in &table.chunks {
                *footprint
                    .chunks
                    .get_mut(&(Arc::as_ptr(chunk) as usize))
                    .expect("candidate chunk") += 1;
            }
        }
        let mut victims = Vec::new();
        for entry in self.map.values() {
            if replacement.is_some_and(|replaced| entry.used == replaced.used) {
                continue;
            }
            if self.is_externally_held(&entry.table, Some(&footprint)) {
                footprint.add_entry(entry.key_bytes, &entry.table);
            } else {
                victims.push(Reverse(Victim {
                    borrowed: false,
                    used: entry.used,
                }));
            }
        }
        footprint.bytes = footprint
            .bytes
            .saturating_add(min_map_bytes::<Key, Entry>(footprint.entries))
            .saturating_add(min_map_bytes::<usize, SharedHold>(footprint.tables.len()))
            .saturating_add(min_map_bytes::<usize, SharedHold>(footprint.chunks.len()));

        // Charge evicted allocations only if the candidate does not move them back into
        // the cache. Include any new caller holds caused by displacing the existing entry.
        let mut evicted_tables = 0usize;
        let mut evicted_chunks = 0usize;
        for (&ptr, hold) in &self.evicted_tables.holds {
            if footprint.tables.insert(ptr) {
                footprint.bytes = footprint.bytes.saturating_add(hold.bytes);
                evicted_tables += 1;
            }
        }
        for (&ptr, hold) in &self.evicted_chunks.holds {
            if footprint.charge_chunk(ptr) {
                footprint.bytes = footprint.bytes.saturating_add(hold.bytes);
                evicted_chunks += 1;
            }
        }
        if let Some(entry) = replacement {
            let old = &entry.table;
            let ptr = Arc::as_ptr(old) as usize;
            let table_held = Arc::strong_count(old) > self.tables[&ptr].refs;
            if table_held && footprint.tables.insert(ptr) {
                footprint.bytes = footprint.bytes.saturating_add(self.tables[&ptr].bytes);
                evicted_tables += 1;
            }
            for chunk in &old.chunks {
                let ptr = Arc::as_ptr(chunk) as usize;
                let hold = &self.chunks[&ptr];
                if (table_held || Arc::strong_count(chunk) > hold.refs)
                    && footprint.charge_chunk(ptr)
                {
                    footprint.bytes = footprint.bytes.saturating_add(hold.bytes);
                    evicted_chunks += 1;
                }
            }
        }
        let minimum = footprint
            .bytes
            .saturating_add(min_map_bytes::<usize, EvictedHold<Table>>(evicted_tables))
            .saturating_add(min_map_bytes::<usize, EvictedHold<Vec<[u64; 3]>>>(
                evicted_chunks,
            ));
        (minimum <= cap).then_some(victims)
    }

    fn remove_at_tick(&mut self, tick: u64) {
        let (_, entry) = self
            .map
            .extract_if(|_, entry| entry.used == tick)
            .next()
            .expect("victim is cached");
        self.key_bytes = self.key_bytes.saturating_sub(entry.key_bytes);
        self.release_table(entry.table);
    }
}

#[derive(Default)]
struct AdmissionFootprint {
    incoming_table: usize,
    entries: usize,
    bytes: u64,
    tables: FxHashSet<usize>,
    /// Distinct chunk addresses and the number of new cache references to each.
    chunks: CacheMap<usize, usize>,
}

impl AdmissionFootprint {
    fn charge_chunk(&mut self, ptr: usize) -> bool {
        if let hashbrown::hash_map::Entry::Vacant(entry) = self.chunks.entry(ptr) {
            entry.insert(0);
            true
        } else {
            false
        }
    }

    fn add_entry(&mut self, key_bytes: u64, table: &Arc<Table>) {
        self.entries += 1;
        self.bytes = self.bytes.saturating_add(key_bytes);
        if self.tables.insert(Arc::as_ptr(table) as usize) {
            self.bytes = self.bytes.saturating_add(table.own_bytes());
            for chunk in &table.chunks {
                if self.charge_chunk(Arc::as_ptr(chunk) as usize) {
                    self.bytes = self.bytes.saturating_add(chunk_bytes(chunk));
                }
            }
        }
    }
}

/// Minimum retained bytes for a single entry, excluding the key's string buffers.
fn standalone_table_bytes(table: &Table) -> u64 {
    let mut distinct_chunks = 0u64;
    let mut chunk_count = 0usize;
    if table.chunks.len() <= 8 {
        for (i, c) in table.chunks.iter().enumerate() {
            let ptr = Arc::as_ptr(c) as usize;
            if !table.chunks[..i]
                .iter()
                .any(|prev| Arc::as_ptr(prev) as usize == ptr)
            {
                distinct_chunks = distinct_chunks.saturating_add(chunk_bytes(c));
                chunk_count += 1;
            }
        }
    } else {
        let mut seen: FxHashSet<usize> = FxHashSet::default();
        for c in &table.chunks {
            if seen.insert(Arc::as_ptr(c) as usize) {
                distinct_chunks = distinct_chunks.saturating_add(chunk_bytes(c));
                chunk_count += 1;
            }
        }
    }
    let min_maps = min_map_bytes::<Key, Entry>(1)
        .saturating_add(min_map_bytes::<usize, SharedHold>(1))
        .saturating_add(min_map_bytes::<usize, SharedHold>(chunk_count));
    table
        .own_bytes()
        .saturating_add(distinct_chunks)
        .saturating_add(min_maps)
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
/// The estimate deduplicates shared `Arc<Table>` and `Chunk` allocations across cached
/// entries and accounts for fact-chunk `Vec` capacity (including spare capacity),
/// local-term key buffers and vector capacities, cache-key `String` capacities,
/// hash-table bucket capacities, and 16 bytes of allocator metadata per non-empty heap
/// allocation. It is neither exact allocator usage nor RSS, and excludes tables kept alive
/// only by a caller (which reduce the cache's eviction allowance until dropped).
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
        inner.prune_evicted_held();
        evict(&mut inner, bytes, None);
    }

    fn snapshot(&self) -> CacheStats {
        let mut inner = self.inner.lock();
        inner.prune_evicted_held();
        CacheStats {
            capacity_bytes: self.capacity.load(Ordering::Relaxed),
            estimated_bytes: inner.retained_bytes(),
            tables: inner.map.len(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    fn put(&self, key: Key, table: Arc<Table>) {
        let table_bytes = standalone_table_bytes(&table);
        #[cfg(test)]
        if let Some(before_lock) = &self.before_insert_lock {
            before_lock();
        }
        let mut inner = self.inner.lock();
        // A shrink that completed while this insertion waited must win.
        let cap = self.capacity.load(Ordering::Relaxed);
        if cap == 0 || table_bytes > cap {
            return;
        }
        let replacement = inner.map.get(&key);
        let key_bytes = replacement.map_or_else(|| key.heap_bytes(), |entry| entry.key_bytes);
        let standalone = table_bytes.saturating_add(key_bytes);
        if standalone > cap {
            return;
        }
        let replacing = replacement.is_some();
        inner.prune_evicted_held();
        let fits_without_growth = !replacing
            && inner.map.len() < inner.map.capacity()
            && inner.tables.len() < inner.tables.capacity()
            && table.chunks.len() <= inner.chunks.capacity() - inner.chunks.len()
            && inner.total_bytes().saturating_add(standalone) <= cap;
        let fits_empty = inner.map.is_empty() && inner.caller_held_evicted_bytes() == 0;
        let victims = if fits_without_growth || fits_empty {
            Vec::new()
        } else if let Some(victims) = inner.admission_victims(&key, &table, cap) {
            victims
        } else {
            return;
        };
        inner.tick += 1;
        let used = inner.tick;
        if let Some(existing) = inner.map.get_mut(&key) {
            let old_table = std::mem::replace(&mut existing.table, table);
            let new_table = existing.table.clone();
            existing.used = used;
            inner.retain_table(&new_table);
            drop(new_table);
            inner.release_table(old_table);
        } else {
            inner.retain_table(&table);
            inner.key_bytes = inner.key_bytes.saturating_add(key_bytes);
            inner.map.insert(
                key,
                Entry {
                    table,
                    key_bytes,
                    used,
                },
            );
        }
        evict(&mut inner, cap, Some(victims));
    }

    #[cfg(test)]
    fn get(&self, key: &Key) -> Option<Arc<Table>> {
        let mut inner = self.inner.lock();
        inner.tick += 1;
        let tick = inner.tick;
        match inner.map.get_mut(key) {
            Some(e) => {
                e.used = tick;
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(e.table.clone())
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }
}

/// Set the cache's capacity in bytes (0 turns it off and empties it).
pub fn set_capacity(bytes: u64) {
    CACHE.set_capacity(bytes);
    // no table outlives a disabled cache, so neither does an anchor and its delta
    if bytes == 0 {
        crate::reuse::clear();
    }
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
    {
        let mut inner = CACHE.inner.lock();
        inner.clear_all();
    }
    crate::reuse::clear();
    crate::qlrewrite::clear();
}

fn evict(inner: &mut Inner, cap: u64, victims: Option<Vec<Reverse<Victim>>>) {
    if cap == 0 {
        inner.clear_all();
        return;
    }
    let mut pending = victims;
    let mut queue = None;
    loop {
        if inner.total_bytes() <= cap {
            if inner.map.is_empty() {
                inner.reset_maps();
            }
            return;
        }
        // Evicting a borrowed entry can grow its weak map. Recompute the minimum
        // for both sets of maps before deciding that another entry must go.
        if inner.map.is_empty()
            || inner
                .payload_bytes()
                .saturating_add(inner.min_map_overhead_bytes())
                .saturating_add(inner.evicted_tables.min_bytes())
                .saturating_add(inner.evicted_chunks.min_bytes())
                <= cap
        {
            break;
        }

        // A put already identified the unborrowed victims before changing ownership.
        // For a resize, classify once and use borrowed victims only as a last resort.
        let queue = queue.get_or_insert_with(|| {
            BinaryHeap::from(pending.take().unwrap_or_else(|| {
                inner
                    .map
                    .values()
                    .map(|entry| {
                        Reverse(Victim {
                            borrowed: inner.is_externally_held(&entry.table, None),
                            used: entry.used,
                        })
                    })
                    .collect()
            }))
        });
        let Some(Reverse(victim)) = queue.pop() else {
            break;
        };
        inner.remove_at_tick(victim.used);
    }
    inner.shrink_maps();
    inner.evicted_tables.shrink_to_fit();
    inner.evicted_chunks.shrink_to_fit();
    debug_assert!(inner.map.is_empty() || inner.total_bytes() <= cap);
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

/// Whether the cache holds a table for `key`, without counting a hit or a miss and
/// without marking the table used: the cost estimate of `auto` asks before a derivation
/// decides whether to read the table (spec F17 §4.3).
pub(crate) fn peek(key: &Key) -> bool {
    CACHE.inner.lock().map.contains_key(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc_probe;
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

    fn standalone_entry_bytes(key: &Key, table: &Table) -> u64 {
        standalone_table_bytes(table).saturating_add(key.heap_bytes())
    }

    /// Legacy uncalibrated estimator (item 192) for side-by-side calibration diagnostics.
    fn legacy_table_bytes(table: &Table) -> u64 {
        table
            .chunks
            .iter()
            .map(|c| (c.len() * std::mem::size_of::<[u64; 3]>()) as u64 + 64)
            .sum::<u64>()
            + table
                .keys
                .iter()
                .map(|(_, k)| k.len() as u64 + 40)
                .sum::<u64>()
            + 64
    }

    /// Exact number of distinct non-empty heap allocations retained by `inner`.
    fn live_allocation_count(inner: &Inner) -> u64 {
        if inner.map.is_empty() {
            return 0;
        }
        let mut count = 0u64;
        if inner.map.allocation_size() > 0 {
            count += 1;
        }
        if inner.tables.allocation_size() > 0 {
            count += 1;
        }
        if inner.chunks.allocation_size() > 0 {
            count += 1;
        }
        for k in inner.map.keys() {
            if k.view.capacity() > 0 {
                count += 1;
            }
            if k.demand.capacity() > 0 {
                count += 1;
            }
        }
        let mut seen_tables = FxHashSet::default();
        let mut seen_chunks = FxHashSet::default();
        for entry in inner.map.values() {
            let tptr = Arc::as_ptr(&entry.table) as usize;
            if seen_tables.insert(tptr) {
                count += 1; // ArcInner<Table>
                if entry.table.chunks.capacity() > 0 {
                    count += 1;
                }
                if entry.table.keys.capacity() > 0 {
                    count += 1;
                }
                for (_, k) in &entry.table.keys {
                    if k.capacity() > 0 {
                        count += 1;
                    }
                }
                for c in &entry.table.chunks {
                    let cptr = Arc::as_ptr(c) as usize;
                    if seen_chunks.insert(cptr) {
                        count += 1; // ArcInner<Vec<[u64; 3]>>
                        if c.capacity() > 0 {
                            count += 1;
                        }
                    }
                }
            }
        }
        count
    }

    #[test]
    fn shrinking_and_disabling_evict_retained_tables_but_not_borrowers() {
        let first = table(1);
        let second = table(4);
        let k1 = key("first");
        let k2 = key("second");
        let one_cap = standalone_entry_bytes(&k2, &second);
        let cache = Cache::new(one_cap * 4);
        cache.put(k1.clone(), first);
        cache.put(k2.clone(), second.clone());
        let two_bytes = cache.snapshot().estimated_bytes;
        assert!(two_bytes > one_cap);
        cache.set_capacity(one_cap);
        let s = cache.snapshot();
        assert_eq!(
            (s.capacity_bytes, s.estimated_bytes, s.tables),
            (one_cap, one_cap, 1)
        );
        assert!(cache.inner.lock().map.contains_key(&k2));
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
        let k_large = key("large");
        let k_same = key("same");
        let large_bytes = standalone_entry_bytes(&k_large, &t);
        let same_bytes = standalone_entry_bytes(&k_same, &t);
        let cache = Cache::new(large_bytes - 1);
        cache.put(k_large, t.clone());
        assert_eq!(cache.snapshot().tables, 0);
        cache.set_capacity(same_bytes);
        cache.put(k_same.clone(), t.clone());
        cache.put(k_same, t);
        let s = cache.snapshot();
        assert_eq!(
            (s.capacity_bytes, s.estimated_bytes, s.tables),
            (same_bytes, same_bytes, 1)
        );
    }

    #[test]
    fn oversized_publication_skips_pruning_and_admission_planning() {
        let candidate = table_with_facts(1000);
        let candidate_key = key("oversized");
        let cap = standalone_entry_bytes(&candidate_key, &candidate) - 1;
        let cache = Cache::new(cap);
        for i in 0..8 {
            cache.put(key(&format!("warm-{i}")), table(i));
        }
        let stale = table(0);
        cache
            .inner
            .lock()
            .evicted_tables
            .remember(Arc::downgrade(&stale), stale.own_bytes());
        drop(stale);

        // The standalone charge for one chunk needs no scratch. Building the
        // admission footprint or collecting victims would allocate under the lock.
        let ((), _, largest) =
            alloc_probe::peak_and_largest_of(|| cache.put(candidate_key, candidate));
        let inner = cache.inner.lock();
        assert_eq!(largest, 0);
        assert_eq!(inner.map.len(), 8);
        assert_eq!(inner.evicted_tables.holds.len(), 1);
    }

    #[test]
    fn replacement_size_limit_uses_the_stored_key_capacity() {
        for stored_padded in [false, true] {
            let mut stored = key("replacement");
            let mut incoming = stored.clone();
            if stored_padded {
                stored.demand.reserve(1024);
            } else {
                incoming.demand.reserve(1024);
            }
            let old = table(1);
            let replacement = table_with_facts(1000);
            let cap = standalone_entry_bytes(&incoming, &replacement)
                .min(standalone_entry_bytes(&stored, &replacement));
            let lookup = incoming.clone();
            let cache = Cache::new(cap);
            cache.put(stored, old);
            cache.put(incoming, replacement.clone());
            assert_eq!(
                Arc::ptr_eq(&cache.get(&lookup).unwrap(), &replacement),
                !stored_padded
            );
        }
    }

    #[test]
    fn calibrated_estimator_matches_measured_heap_across_spare_capacity_keys_and_maps() {
        // Measure live heap delta directly on this thread via alloc_probe across five
        // representative retained-allocation shapes and confirm the calibrated estimator
        // equals measured requested bytes plus ALLOC_METADATA_BYTES per live allocation.
        type CalibrationCase = (&'static str, Box<dyn FnOnce(&Cache) -> u64>);
        let cases: Vec<CalibrationCase> = vec![
            (
                "empty_table_long_key_strings",
                Box::new(|cache| {
                    let mut view = String::with_capacity(512);
                    view.push_str("gen-uuid:commit-42:mask-0:default");
                    let mut demand = String::with_capacity(1024);
                    demand.push_str("Demand(http://example.org/p^bf)");
                    let t = Arc::new(Table {
                        chunks: Vec::new(),
                        keys: Vec::new(),
                    });
                    let legacy = legacy_table_bytes(&t);
                    cache.put(
                        Key {
                            view,
                            digest: 7,
                            demand,
                            seeds: (0, 0),
                        },
                        t,
                    );
                    legacy
                }),
            ),
            (
                "spare_fact_vec_and_local_key_capacity",
                Box::new(|cache| {
                    let mut facts = Vec::with_capacity(2048);
                    for i in 0..256u64 {
                        facts.push([i, i + 1, i + 2]);
                    }
                    let mut local_keys = Vec::with_capacity(128);
                    for i in 0..16u64 {
                        let mut buf = Vec::with_capacity(256);
                        buf.extend_from_slice(b"http://example.org/term");
                        local_keys.push((i, buf));
                    }
                    let mut chunks = Vec::with_capacity(16);
                    chunks.push(Arc::new(facts));
                    let t = Arc::new(Table {
                        chunks,
                        keys: local_keys,
                    });
                    let legacy = legacy_table_bytes(&t);
                    cache.put(key("spare-vecs"), t);
                    legacy
                }),
            ),
            (
                "hash_table_growth_64_entries",
                Box::new(|cache| {
                    let mut legacy = 0u64;
                    for i in 0..64u64 {
                        let t = table(i);
                        legacy += legacy_table_bytes(&t);
                        cache.put(key(&format!("demand-{i:04}")), t);
                    }
                    legacy
                }),
            ),
            (
                "shared_chunks_across_progressive_session_tables",
                Box::new(|cache| {
                    let c1 = Arc::new(vec![[1, 2, 3]; 1_000]);
                    let c2 = Arc::new(vec![[4, 5, 6]; 500]);
                    let c3 = Arc::new(vec![[7, 8, 9]; 250]);
                    let t1 = Arc::new(Table {
                        chunks: vec![c1.clone()],
                        keys: Vec::new(),
                    });
                    let t2 = Arc::new(Table {
                        chunks: vec![c1.clone(), c2.clone()],
                        keys: Vec::new(),
                    });
                    let t3 = Arc::new(Table {
                        chunks: vec![c1, c2, c3],
                        keys: Vec::new(),
                    });
                    let legacy =
                        legacy_table_bytes(&t1) + legacy_table_bytes(&t2) + legacy_table_bytes(&t3);
                    cache.put(key("step-1"), t1);
                    cache.put(key("step-2"), t2);
                    cache.put(key("step-3"), t3);
                    legacy
                }),
            ),
            (
                "shared_table_across_multiple_keys",
                Box::new(|cache| {
                    let shared = Arc::new(Table {
                        chunks: vec![Arc::new(vec![[10, 20, 30]; 500])],
                        keys: vec![(1, b"http://example.org/local".to_vec())],
                    });
                    let legacy = legacy_table_bytes(&shared) * 5;
                    for i in 0..5 {
                        cache.put(key(&format!("alias-{i}")), shared.clone());
                    }
                    legacy
                }),
            ),
        ];

        for (label, populate) in cases {
            let before = alloc_probe::live_bytes();
            let cache = Cache::new(64 << 20);
            let legacy = populate(&cache);
            let after = alloc_probe::live_bytes();
            let measured = (after - before).max(0) as u64;
            let s = cache.snapshot();
            let allocs = live_allocation_count(&cache.inner.lock());
            let expected = measured + allocs * ALLOC_METADATA_BYTES;
            assert_eq!(
                s.estimated_bytes, expected,
                "{label}: calibrated estimate must equal measured heap ({measured}) + {allocs}*{ALLOC_METADATA_BYTES}"
            );
            assert!(
                s.estimated_bytes >= measured,
                "{label}: calibrated estimate must not undercount measured heap"
            );
            drop(cache);
            let after_drop = alloc_probe::live_bytes();
            assert_eq!(
                after_drop, before,
                "{label}: dropping cache must release all measured heap"
            );
            eprintln!(
                "case={label}: measured={measured}B allocs={allocs} calibrated={}B legacy={legacy}B",
                s.estimated_bytes
            );
        }
    }

    #[test]
    fn tombstones_do_not_reduce_the_measured_map_allocation() {
        use std::hash::BuildHasher;

        let before = alloc_probe::live_bytes();
        let cache = Cache::new(1 << 20);
        let shared = Arc::new(Table {
            chunks: Vec::new(),
            keys: Vec::new(),
        });
        // Fill consecutive control slots, then evict interior entries. Hashbrown
        // leaves tombstones there because a full probe group spans each removal.
        for bucket in 0..56 {
            let mut k = key("collision-layout");
            while FxBuildHasher.hash_one(&k) & 63 != bucket {
                k.seeds.1 += 1;
            }
            cache.put(k, shared.clone());
        }
        drop(shared);
        let mut inner = cache.inner.lock();
        let bytes = inner.map.allocation_size();
        for tick in 15..=42 {
            inner.remove_at_tick(tick);
        }
        assert_eq!(inner.map.len(), 28);
        assert_eq!(inner.map.allocation_size(), bytes);
        let inferred = alloc_bytes(table_bytes_for::<(Key, Entry)>(inner.map.capacity()));
        assert!(inferred < alloc_bytes(bytes));
        let measured = (alloc_probe::live_bytes() - before) as u64;
        assert_eq!(
            inner.retained_bytes(),
            measured + live_allocation_count(&inner) * ALLOC_METADATA_BYTES
        );
        // A cap the old estimate would have accepted now shrinks the map.
        let cap = inner.retained_bytes() - alloc_bytes(bytes) + inferred;
        drop(inner);
        cache.set_capacity(cap);
        let stats = cache.snapshot();
        assert_eq!(stats.tables, 28);
        assert!(stats.estimated_bytes <= cap);
        drop(cache);
        assert_eq!(alloc_probe::live_bytes(), before);
    }

    #[test]
    fn resize_shrinks_a_grown_caller_hold_map_before_evicting_again() {
        use std::hash::BuildHasher;

        let empty_table = || {
            Arc::new(Table {
                chunks: Vec::new(),
                keys: Vec::new(),
            })
        };
        let cache = Cache::new(1 << 20);
        let older = empty_table();
        let newer = empty_table();
        let old_key = key("older");
        let new_key = key("newer");
        cache.put(old_key.clone(), older.clone());
        cache.put(new_key.clone(), newer.clone());

        let mut inner = cache.inner.lock();
        let holds = &mut inner.evicted_tables;
        holds.holds.reserve(56);
        let original_allocation = holds.holds.allocation_size();
        let old_ptr = Arc::as_ptr(&older) as usize;
        let old_bucket = FxBuildHasher.hash_one(old_ptr) & 63;
        let mut callers = Vec::new();
        // Use fixture keys to fill consecutive buckets regardless of allocator
        // addresses. Leave the real older table's home bucket empty so inserting
        // it cannot reuse a tombstone. Pruning the middle creates tombstones.
        for offset in 1..=56 {
            let bucket = (old_bucket + offset) & 63;
            let mut fixture_key = 0;
            while FxBuildHasher.hash_one(fixture_key) & 63 != bucket {
                fixture_key += 1;
            }
            let caller = empty_table();
            holds.holds.insert(
                fixture_key,
                EvictedHold {
                    weak: Arc::downgrade(&caller),
                    bytes: caller.own_bytes(),
                },
            );
            holds.payload_bytes += caller.own_bytes();
            if !(18..=38).contains(&offset) {
                callers.push(caller);
            }
        }
        holds.prune();
        assert_eq!(holds.holds.len(), 35);
        assert_eq!(holds.holds.capacity(), 35);
        assert_eq!(holds.holds.allocation_size(), original_allocation);
        let cap = standalone_entry_bytes(&new_key, &newer) + holds.bytes() + older.own_bytes();
        drop(inner);

        cache.set_capacity(cap);
        let inner = cache.inner.lock();
        assert!(!inner.map.contains_key(&old_key));
        assert!(inner.map.contains_key(&new_key));
        assert_eq!(inner.evicted_tables.holds.len(), 36);
        assert_eq!(
            inner.evicted_tables.holds.allocation_size(),
            original_allocation
        );
        assert_eq!(inner.total_bytes(), cap);
    }

    fn table_with_facts(facts: usize) -> Arc<Table> {
        Arc::new(Table {
            chunks: vec![Arc::new(vec![[1, 2, 3]; facts])],
            keys: Vec::new(),
        })
    }

    #[test]
    fn rejected_reinsertion_preserves_evicted_table_and_shared_chunk_charges() {
        let cache = Cache::new(1 << 20);
        let evicted = table_with_facts(300);
        cache.put(key("old"), evicted.clone());
        cache.set_capacity(1);
        let held = cache.inner.lock().caller_held_evicted_bytes();
        assert!(held > 300 * 24);

        let pinned = table_with_facts(600);
        let pin_key = key("pinned");
        let cap = standalone_entry_bytes(&pin_key, &pinned) + held;
        cache.set_capacity(cap);
        cache.put(pin_key.clone(), pinned.clone());

        let mut padded_key = key("same-table");
        padded_key.view.reserve(512);
        let shared = Arc::new(Table {
            chunks: vec![evicted.chunks[0].clone()],
            keys: vec![(1, vec![1; 512])],
        });
        for (candidate_key, candidate) in
            [(padded_key, evicted.clone()), (key("shared-chunk"), shared)]
        {
            // The candidate fits by itself but the combined footprint cannot fit
            // with the pinned table. Keys and local terms make overlap insufficient.
            assert!(standalone_entry_bytes(&candidate_key, &candidate) <= cap);
            let lookup_key = candidate_key.clone();
            cache.put(candidate_key, candidate);
            let inner = cache.inner.lock();
            assert_eq!(inner.map.len(), 1);
            assert!(inner.map.contains_key(&pin_key));
            assert!(!inner.map.contains_key(&lookup_key));
            assert_eq!(inner.caller_held_evicted_bytes(), held);
        }
    }

    #[test]
    fn disjoint_candidate_that_cannot_fit_beside_evicted_holds_preserves_warm_entry() {
        let cache = Cache::new(1 << 20);
        let held = table_with_facts(400);
        cache.put(key("evicted"), held.clone());
        cache.set_capacity(1);
        let charge = cache.inner.lock().caller_held_evicted_bytes();
        let warm = table_with_facts(100);
        let warm_key = key("warm");
        let cap = standalone_entry_bytes(&warm_key, &warm) + charge;
        cache.set_capacity(cap);
        cache.put(warm_key.clone(), warm);

        let candidate = table_with_facts(200);
        let candidate_key = key("candidate");
        assert!(standalone_entry_bytes(&candidate_key, &candidate) <= cap);
        cache.put(candidate_key.clone(), candidate.clone());
        let inner = cache.inner.lock();
        assert_eq!(inner.map.len(), 1);
        assert!(inner.map.contains_key(&warm_key));
        assert!(!inner.map.contains_key(&candidate_key));
    }

    #[test]
    fn borrowed_candidate_that_cannot_fit_beside_cached_pins_preserves_warm_entries() {
        let cache = Cache::new(1 << 20);
        let pinned = table_with_facts(400);
        cache.put(key("pinned"), pinned.clone());
        cache.put(key("warm-1"), table_with_facts(100));
        cache.put(key("warm-2"), table_with_facts(100));
        let full = cache.snapshot();
        cache.set_capacity(full.estimated_bytes);

        let candidate = table_with_facts(300);
        let candidate_key = key("candidate");
        assert!(standalone_entry_bytes(&candidate_key, &candidate) <= full.estimated_bytes);
        cache.put(candidate_key.clone(), candidate.clone());
        let inner = cache.inner.lock();
        assert_eq!(inner.map.len(), 3);
        assert!(inner.map.contains_key(&key("warm-1")));
        assert!(inner.map.contains_key(&key("warm-2")));
        assert!(!inner.map.contains_key(&candidate_key));
    }

    #[test]
    fn admitting_a_shared_allocation_can_evict_the_previous_unborrowed_alias() {
        for same_table in [false, true] {
            let cache = Cache::new(1 << 20);
            let old_key = key("old");
            let new_key = key("new");
            cache.put(old_key.clone(), table_with_facts(200));
            let old = cache.get(&old_key).unwrap();
            let candidate = if same_table {
                old
            } else {
                let chunks = old.chunks.clone();
                drop(old);
                Arc::new(Table {
                    chunks,
                    keys: Vec::new(),
                })
            };
            let cap = standalone_entry_bytes(&new_key, &candidate);
            cache.set_capacity(cap);
            cache.put(new_key.clone(), candidate);
            let inner = cache.inner.lock();
            assert_eq!(inner.map.len(), 1);
            assert!(inner.map.contains_key(&new_key));
            assert_eq!(inner.caller_held_evicted_bytes(), 0);
        }
    }

    #[test]
    fn declined_replacement_keeps_old_binding_and_successful_replacement_tracks_its_hold() {
        let cache = Cache::new(1 << 20);
        let old = table_with_facts(300);
        let replacement = table_with_facts(300);
        let k = key("replace");
        let one = standalone_entry_bytes(&k, &old);
        cache.put(k.clone(), old.clone());
        cache.set_capacity(one);
        cache.put(k.clone(), replacement.clone());
        assert!(Arc::ptr_eq(&cache.get(&k).unwrap(), &old));

        cache.set_capacity(one * 3);
        cache.put(k.clone(), replacement.clone());
        assert!(Arc::ptr_eq(&cache.get(&k).unwrap(), &replacement));
        assert!(cache.inner.lock().caller_held_evicted_bytes() > 300 * 24);
    }

    #[test]
    fn caller_hold_tracking_compacts_and_uses_no_scratch_allocations_to_total() {
        let cache = Cache::new(1 << 20);
        let mut callers = Vec::new();
        for i in 0..64 {
            let borrowed = table(i);
            cache.put(key(&format!("pinned-{i}")), borrowed.clone());
            callers.push(borrowed);
        }
        cache.set_capacity(1);
        let (table_alloc, chunk_alloc) = {
            let inner = cache.inner.lock();
            assert_eq!(inner.evicted_tables.holds.len(), 64);
            assert_eq!(inner.evicted_chunks.holds.len(), 64);
            (
                inner.evicted_tables.holds.allocation_size(),
                inner.evicted_chunks.holds.allocation_size(),
            )
        };

        let shared_chunk = callers[0].chunks[0].clone();
        callers.truncate(1);
        cache.snapshot();
        let mut inner = cache.inner.lock();
        assert_eq!(inner.evicted_tables.holds.len(), 1);
        assert_eq!(inner.evicted_chunks.holds.len(), 1);
        assert!(inner.evicted_tables.holds.allocation_size() < table_alloc);
        assert!(inner.evicted_chunks.holds.allocation_size() < chunk_alloc);
        drop(callers);
        inner.prune_evicted_held();
        assert_eq!(inner.evicted_tables.holds.allocation_size(), 0);

        let (charge, peak) = alloc_probe::peak_of(|| {
            (0..64)
                .map(|_| inner.caller_held_evicted_bytes())
                .sum::<u64>()
        });
        assert_eq!(
            charge,
            64 * (chunk_bytes(&shared_chunk)
                + alloc_bytes(inner.evicted_chunks.holds.allocation_size()))
        );
        assert_eq!(peak, 0);
        drop(shared_chunk);
        inner.prune_evicted_held();
        assert_eq!(inner.evicted_chunks.holds.allocation_size(), 0);
    }

    #[test]
    fn shared_chunks_deduplicate_and_retain_until_last_owner_evicts() {
        let big_chunk = Arc::new(vec![[1, 2, 3]; 2_000]); // 48,000 B payload
        let small_chunk = Arc::new(vec![[4, 5, 6]; 10]);
        let t1 = Arc::new(Table {
            chunks: vec![big_chunk.clone()],
            keys: Vec::new(),
        });
        let t2 = Arc::new(Table {
            chunks: vec![big_chunk.clone(), small_chunk],
            keys: Vec::new(),
        });
        // Repeated chunk inside one table is also deduplicated.
        let t3 = Arc::new(Table {
            chunks: vec![big_chunk.clone(), big_chunk],
            keys: Vec::new(),
        });

        let cache = Cache::new(1 << 20);
        cache.put(key("t1"), t1);
        let one = cache.snapshot().estimated_bytes;
        cache.put(key("t2"), t2);
        let two = cache.snapshot().estimated_bytes;
        cache.put(key("t3"), t3);
        let three = cache.snapshot().estimated_bytes;

        // Adding t2 and t3 must not re-charge the 48 KiB shared chunk.
        let delta_t2 = two - one;
        let delta_t3 = three - two;
        assert!(
            delta_t2 < 2_048,
            "t2 should only add its own shell, key, and small chunk, got {delta_t2}"
        );
        assert!(
            delta_t3 < 1_024,
            "t3 should only add its own shell and key, got {delta_t3}"
        );

        // Shrink so only t2 + t3 fit (t1 evicted). Because t2 and t3 still retain big_chunk,
        // evicting t1 frees only t1's key + table shell, keeping big_chunk accounted.
        let target_two = three - 1;
        cache.set_capacity(target_two);
        let s = cache.snapshot();
        assert_eq!(s.tables, 2);
        assert!(s.estimated_bytes <= target_two);
        assert!(s.estimated_bytes > 48_000);
        assert!(cache.get(&key("t1")).is_none());
        assert!(cache.get(&key("t2")).is_some());
        assert!(cache.get(&key("t3")).is_some());

        // Shrink below big_chunk's size: both remaining sharers must evict and release all maps.
        cache.set_capacity(10_000);
        let s = cache.snapshot();
        assert_eq!((s.tables, s.estimated_bytes), (0, 0));
    }

    #[test]
    fn hash_table_shrinks_after_eviction_instead_of_dropping_fitting_entry() {
        let cache = Cache::new(1 << 20);
        for i in 0..200u64 {
            cache.put(key(&format!("entry-{i:04}")), table(i));
        }
        let peak_cap = cache.inner.lock().map.capacity();
        assert!(peak_cap >= 200);
        let last_key = key("entry-0199");
        let last_table = table(199);
        let one_entry_budget = standalone_entry_bytes(&last_key, &last_table);

        // Shrink to fit exactly the single most-recent entry; hash maps must shrink so that
        // entry survives instead of being evicted by leftover bucket capacity.
        cache.set_capacity(one_entry_budget);
        let s = cache.snapshot();
        assert_eq!(s.tables, 1);
        assert_eq!(s.estimated_bytes, one_entry_budget);
        assert!(cache.inner.lock().map.capacity() < peak_cap);
        assert!(cache.get(&last_key).is_some());
    }

    #[test]
    fn eviction_prefers_unborrowed_entries_over_caller_held_tables() {
        let t_a = Arc::new(Table {
            chunks: vec![Arc::new(vec![[1, 2, 3]; 200])],
            keys: Vec::new(),
        });
        let t_b = Arc::new(Table {
            chunks: vec![Arc::new(vec![[4, 5, 6]; 200])],
            keys: Vec::new(),
        });
        let t_c = Arc::new(Table {
            chunks: vec![Arc::new(vec![[7, 8, 9]; 200])],
            keys: Vec::new(),
        });

        let cache = Cache::new(1 << 20);
        cache.put(key("a"), t_a);
        cache.put(key("b"), t_b);
        let two_entries = cache.snapshot().estimated_bytes;
        cache.set_capacity(two_entries);

        // Caller borrows the older entry `a` (leaving `b` unborrowed).
        // Touch `b` so `a` stays older by LRU tick, yet `a` is caller-held.
        let held_a = cache.get(&key("a")).expect("a cached");
        let _ = cache.get(&key("b")).expect("b cached");
        // Drop the temporary handle from `get("b")`, keeping only `held_a`.
        cache.put(key("c"), t_c);

        let s = cache.snapshot();
        assert_eq!(s.tables, 2);
        assert!(s.estimated_bytes <= two_entries);
        // `b` (unborrowed) was evicted to reclaim memory; `a` (caller-held) and `c` stay cached.
        assert!(cache.inner.lock().map.contains_key(&key("a")));
        assert!(!cache.inner.lock().map.contains_key(&key("b")));
        assert!(cache.inner.lock().map.contains_key(&key("c")));
        drop(held_a);
    }

    #[test]
    fn caller_held_evicted_tables_reduce_eviction_allowance_until_dropped() {
        let large = Arc::new(Table {
            chunks: vec![Arc::new(vec![[1, 2, 3]; 500])], // ~12 KiB
            keys: Vec::new(),
        });
        let medium = Arc::new(Table {
            chunks: vec![Arc::new(vec![[4, 5, 6]; 200])], // ~4.8 KiB
            keys: Vec::new(),
        });
        let k_large = key("large");
        let k_med = key("medium");
        let large_only = standalone_entry_bytes(&k_large, &large);
        let med_only = standalone_entry_bytes(&k_med, &medium);

        let cache = Cache::new(large_only);
        cache.put(k_large.clone(), large);
        let borrowed = cache.get(&k_large).expect("large cached");

        // Shrink capacity below `large` while caller still holds it. `large` is evicted from
        // `map` (reported estimated_bytes == 0), yet its live bytes remain charged against
        // the eviction allowance (`med_only + 1024 < large_only`).
        let reduced_cap = med_only + 1024;
        assert!(reduced_cap < large_only);
        cache.set_capacity(reduced_cap);
        let s = cache.snapshot();
        assert_eq!((s.tables, s.estimated_bytes), (0, 0));
        assert!(cache.inner.lock().caller_held_evicted_bytes() >= 12_000);

        // While the caller still pins `large` (> reduced_cap), `medium` is refused so total
        // live memory does not pile on top of the caller-held evicted table.
        cache.put(k_med.clone(), medium.clone());
        assert_eq!(cache.snapshot().tables, 0);

        // Once the caller drops `borrowed`, the eviction allowance recovers and `medium` fits.
        drop(borrowed);
        cache.put(k_med.clone(), medium);
        let s = cache.snapshot();
        assert_eq!(s.tables, 1);
        assert!(s.estimated_bytes <= reduced_cap);
        assert_eq!(cache.inner.lock().caller_held_evicted_bytes(), 0);
        assert!(cache.get(&k_med).is_some());
    }

    #[test]
    fn caller_held_chunk_pin_and_key_replacement_stay_exact() {
        let shared_chunk = Arc::new(vec![[9, 8, 7]; 400]);
        let t = Arc::new(Table {
            chunks: vec![shared_chunk.clone()],
            keys: Vec::new(),
        });
        let k_exact = key("rep");
        let cap = standalone_entry_bytes(&k_exact, &t);
        let cache = Cache::new(cap);
        cache.put(k_exact.clone(), t);

        // Shrink below `t` while an external holder (`shared_chunk`) still pins the chunk
        // (matching an active Session::chunks holding a chunk after Table drops).
        let small_cap = cap - 1;
        cache.set_capacity(small_cap);
        assert_eq!(cache.snapshot().tables, 0);
        assert!(cache.inner.lock().caller_held_evicted_bytes() >= 400 * 24);
        drop(shared_chunk);
        assert_eq!(cache.snapshot().tables, 0);
        assert_eq!(cache.inner.lock().caller_held_evicted_bytes(), 0);

        // Replacing an equal key whose new String has different capacity must keep key_bytes
        // synced with the key actually retained by the HashMap.
        let mut padded_view = String::with_capacity(256);
        padded_view.push_str("v1");
        let k_padded = Key {
            view: padded_view,
            digest: 1,
            demand: "d1".into(),
            seeds: (0, 0),
        };
        let k_tight = Key {
            view: "v1".into(),
            digest: 1,
            demand: "d1".into(),
            seeds: (0, 0),
        };
        cache.set_capacity(1 << 20);
        cache.put(k_padded, table(1));
        let before_key_bytes = cache.inner.lock().key_bytes;
        cache.put(k_tight, table(2));
        let inner = cache.inner.lock();
        assert_eq!(inner.key_bytes, before_key_bytes);
        let stored_key = inner.map.keys().next().unwrap();
        assert_eq!(inner.key_bytes, stored_key.heap_bytes());
    }

    #[test]
    fn reinserted_caller_held_table_is_not_double_counted_and_pinned_cache_is_not_thrashed() {
        let t1 = Arc::new(Table {
            chunks: vec![Arc::new(vec![[1, 2, 3]; 300])],
            keys: Vec::new(),
        });
        let t2 = Arc::new(Table {
            chunks: vec![Arc::new(vec![[4, 5, 6]; 300])],
            keys: Vec::new(),
        });
        let k1 = key("pinned-1");
        let k2 = key("pinned-2");
        let one_cap = standalone_entry_bytes(&k1, &t1);
        let cache = Cache::new(one_cap + 1024);

        cache.put(k1.clone(), t1.clone());
        // Both t1 (in cache) and t2 (being inserted) are externally held, and together
        // exceed capacity; t2 is declined without evicting t1 or polluting evicted_held.
        cache.put(k2.clone(), t2.clone());
        assert_eq!(cache.snapshot().tables, 1);
        assert!(cache.inner.lock().map.contains_key(&k1));
        assert_eq!(cache.inner.lock().caller_held_evicted_bytes(), 0);

        // Shrink below t1 so t1 moves to evicted_held, then restore capacity and re-insert
        // t1: caller_held_evicted_bytes must not double-count t1 while it is back in cache.
        cache.set_capacity(one_cap - 1);
        assert_eq!(cache.snapshot().tables, 0);
        assert!(cache.inner.lock().caller_held_evicted_bytes() > 0);
        cache.set_capacity(one_cap);
        cache.put(k1.clone(), t1.clone());
        assert_eq!(cache.snapshot().tables, 1);
        assert_eq!(cache.snapshot().estimated_bytes, one_cap);
        assert_eq!(cache.inner.lock().caller_held_evicted_bytes(), 0);
        drop((t1, t2));
    }
}
