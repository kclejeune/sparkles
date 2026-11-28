//! Per-query execution context: snapshot, local vocabulary, decode cache,
//! cancellation and dataset description.

use super::pool::PoolGrant;
use super::query_memory::{PRIMARY, QueryMemoryLease, QueryMemoryLedger};
use super::table::VarId;
use super::value::Value;
use crate::access::{Preparation, PreparationText, Reservation};
use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::{self, Id, Tag};
use crate::store::{Snapshot, bnode_for, parse_bnode_label};
use crate::vocab::AppendVocab;
use oxrdf::Term;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

#[derive(Default)]
struct PreparedVariables {
    text: Option<Reservation>,
    vector: Option<Reservation>,
}

/// Jena's IRI for the default graph (`Quad.defaultGraphIRI`).
pub const DEFAULT_GRAPH_IRI: &str = "urn:x-arq:DefaultGraph";
/// Jena's name for the default graph in quads it builds, such as a CONSTRUCT template's
/// triples outside `GRAPH` (`Quad.defaultGraphNodeGenerated`).
pub const DEFAULT_GRAPH_NODE_IRI: &str = "urn:x-arq:DefaultGraphNode";
/// Jena's IRI for the union of all named graphs (`Quad.unionGraph`).
pub const UNION_GRAPH_IRI: &str = "urn:x-arq:UnionGraph";

/// RDF dataset for a query (`FROM` / `FROM NAMED` or protocol parameters).
#[derive(Clone, Debug, Default)]
pub struct DatasetSpec {
    /// Graphs merged into the default graph; `None` = the store's default graph
    /// (or union of named graphs if the store uses a union default graph).
    pub default: Option<Vec<Id>>,
    /// Named graphs; `None` = all named graphs in the store.
    pub named: Option<Vec<Id>>,
    /// default graph is the union of all named graphs
    pub union_default: bool,
}

impl DatasetSpec {
    /// Limit the dataset of a query on `snap` to the graphs `access` may read: hidden
    /// graphs leave `FROM` and `FROM NAMED` (as if they did not exist), the default graph
    /// is empty when it is hidden, and the union of named graphs (the store's union
    /// default graph, or `urn:x-arq:UnionGraph`) becomes the visible named graphs.
    pub fn restrict(
        &mut self,
        snap: &Snapshot,
        access: &crate::access::GraphAccess,
        term: &dyn Fn(Id) -> Option<Term>,
    ) -> Result<()> {
        self.restrict_checked(snap, access, &|id| Ok(term(id)))
    }

    /// Checked variant of [`DatasetSpec::restrict`] that propagates decoding and request
    /// control failures instead of treating a refused graph IRI as absent.
    pub fn restrict_checked(
        &mut self,
        snap: &Snapshot,
        access: &crate::access::GraphAccess,
        term: &dyn Fn(Id) -> Result<Option<Term>>,
    ) -> Result<()> {
        if access.reads_all() {
            return Ok(());
        }
        let (visible, every) = access.visible_named_all(snap)?;
        let allowed = |g: &Id| -> Result<bool> {
            if *g == Id::DEFAULT_GRAPH {
                Ok(access.read.default_graph())
            } else if visible.binary_search(g).is_ok() {
                Ok(true)
            } else {
                // a graph without quads (or a query-local term): by its name
                match g.tag() {
                    id::Tag::BNode | id::Tag::Undef | id::Tag::Special => Ok(false),
                    _ => Ok(access.readable(term(*g)?.as_ref())),
                }
            }
        };
        let filter_graphs = |graphs: &[Id]| -> Result<Vec<Id>> {
            let mut kept = Vec::with_capacity(graphs.len());
            for &g in graphs {
                if allowed(&g)? {
                    kept.push(g);
                }
            }
            Ok(kept)
        };
        let union = || visible.to_vec();
        let new_default = match self.default.as_deref() {
            // every named graph is visible: the union of them all, with its fast paths
            None if every && (self.union_default || snap.union_default_graph) => None,
            _ if self.union_default => Some(union()),
            None if snap.union_default_graph => Some(union()),
            // the store's default graph, as before (which keeps its fast paths)
            None if access.read.default_graph() => None,
            None => Some(Vec::new()),
            Some(l) => Some(filter_graphs(l)?),
        };
        let new_union_default = if new_default.is_some() {
            false
        } else {
            self.union_default
        };
        let new_named = match self.named.as_deref() {
            None if every => None,
            None => Some(union()),
            Some(l) => Some(filter_graphs(l)?),
        };
        self.default = new_default;
        self.union_default = new_union_default;
        self.named = new_named;
        Ok(())
    }
}

const VALUE_SHARDS: usize = 64;
/// decoded values kept per shard before the shard is cleared
const VALUE_SHARD_CAP: usize = 1 << 16;

/// Bytes of query state that a context charges in steps and keeps charged until
/// it drops: its decoded value cache, its query-local terms and memos and its parsed
/// geometries. The state counts its bytes itself, and [`Ctx::grow_reservation`] grows
/// the reservation to cover them, so that most additions take one atomic load.
#[derive(Default)]
pub(crate) struct StepReservation {
    /// the bytes charged
    reserved: AtomicU64,
    /// serializes growth, with the owner the charges went to
    grow: parking_lot::Mutex<Option<u64>>,
}

impl StepReservation {
    /// Whether the reservation covers `bytes` already.
    #[inline]
    pub(crate) fn covers(&self, bytes: u64) -> bool {
        bytes <= self.reserved.load(Ordering::Acquire)
    }

    /// The bytes charged.
    pub(crate) fn reserved(&self) -> u64 {
        self.reserved.load(Ordering::Acquire)
    }
}

/// The query-local terms and memos of a context: the terms its expressions compute and
/// the blank nodes it mints, charged in both accounting modes (see [`Ctx::retain_bytes`]).
#[derive(Default)]
struct RetainedTerms {
    /// the estimated bytes retained
    used: AtomicU64,
    charge: StepReservation,
}

/// The most the decoded value cache of one query holds, before the memory budget's
/// share (see [`ValueCache::cap`]).
const VALUE_CACHE_BYTES: u64 = 64 << 20;

/// The query's cache of decoded dictionary values, in shards that a hash of the id
/// picks. It is charged to the query in both accounting modes: the cache reserves
/// [`EAGER_STEP`]s from the query's account as its entries grow, and a value whose
/// reservation is refused is returned without being cached. The cache holds at most
/// [`VALUE_CACHE_BYTES`] or an eighth of the memory budget, whichever is smaller, and a
/// shard that reaches its share of that is cleared and freed. The reservation is kept
/// until the context drops.
pub(crate) struct ValueCache {
    /// Made on the first value cached or looked up. Most short queries decode nothing
    /// through the cache and skip the 64 shards' allocation, setup and drop.
    shards: std::sync::OnceLock<Box<[RwLock<ValueShard>]>>,
    /// the estimated bytes of the entries held now
    used: AtomicU64,
    /// the bytes charged for the cache, never fewer than `used`
    charge: StepReservation,
}

#[derive(Default)]
struct ValueShard {
    map: FxHashMap<Id, Value>,
    /// the estimated bytes of `map`'s entries
    bytes: u64,
}

#[cfg(test)]
thread_local! {
    /// growths of a step reservation on this thread
    static VALUE_GROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Default for ValueCache {
    fn default() -> Self {
        Self {
            shards: std::sync::OnceLock::new(),
            used: AtomicU64::new(0),
            charge: Default::default(),
        }
    }
}

impl ValueCache {
    /// The estimated bytes one cached entry keeps: its slot, counted twice since a map
    /// grows by doubling, and the heap storage of its text.
    pub(crate) fn entry_bytes(v: &Value) -> u64 {
        fn text(s: &str) -> u64 {
            s.len() as u64 + 16
        }
        let heap = match v {
            Value::Iri(a) | Value::BNode(a) | Value::Str(a) => text(a),
            Value::Lang(a, b) | Value::LangDir(a, b, _) => text(a) + text(b),
            Value::Other { lex, dt } => text(lex) + text(dt),
            Value::BigInteger(i) => i.heap_bytes() + 16,
            Value::BigDecimal(d) => d.heap_bytes() + 16,
            Value::Triple(t) => 64 + super::graph_triple_bytes(t),
            _ => 0,
        };
        2 * (std::mem::size_of::<(Id, Value)>() as u64 + 1) + heap
    }

    /// The most the cache holds under a memory budget of `mem_limit` bytes.
    fn cap(mem_limit: u64) -> u64 {
        VALUE_CACHE_BYTES.min(mem_limit / 8)
    }

    #[inline]
    fn index(id: Id) -> usize {
        (id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) as usize % VALUE_SHARDS
    }

    #[inline]
    fn shard(&self, id: Id) -> &RwLock<ValueShard> {
        let shards = self
            .shards
            .get_or_init(|| (0..VALUE_SHARDS).map(|_| Default::default()).collect());
        &shards[Self::index(id)]
    }

    #[inline]
    fn get(&self, id: Id) -> Option<Value> {
        let shards = self.shards.get()?;
        shards[Self::index(id)].read().map.get(&id).cloned()
    }

    /// The bytes charged for the cache.
    #[cfg(test)]
    fn reserved(&self) -> u64 {
        self.charge.reserved()
    }
}

/// The most the string floor cache of one query holds, before the memory budget's share
/// (see [`FloorCache::cap`]).
const FLOOR_CACHE_BYTES: u64 = 64 << 20;

/// The default slots of the floor table a query starts with, 512 KiB of words.
pub(super) const FLOOR_TABLE_FIRST: usize = 1 << 16;

/// The fewest slots a floor table is worth allocating.
pub(super) const FLOOR_TABLE_FEWEST: usize = 1 << 10;

/// A floor table that grows is replaced by one this many times larger.
const FLOOR_TABLE_GROWTH: usize = 8;

/// A floor table grows once the floors it evicted that had been read again number at
/// least its slots divided by this.
const FLOOR_TABLE_EVICTIONS: usize = 32;

/// Minimum lookups in one floor-cached mask or part before evaluating its hit rate
/// against [`FLOOR_BYPASS_HIT_DENOM`].
const FLOOR_BYPASS_MIN_LOOKUPS: usize = 2048;

/// A floor pass whose hits are fewer than `lookups / FLOOR_BYPASS_HIT_DENOM` (12.5%)
/// pauses floor-table use until the next probe mask.
pub(super) const FLOOR_BYPASS_HIT_DENOM: usize = 8;

/// While bypassing floors due to low hit rate, every `FLOOR_BYPASS_PROBE_INTERVAL`-th
/// mask runs on floors to populate the table and test whether cross-batch reuse has
/// risen above the threshold.
const FLOOR_BYPASS_PROBE_INTERVAL: u32 = 4;

/// Worker-local reusable key reconstruction buffers for floor lookups and direct string
/// comparisons. Both buffers start empty and allocate only on first use after
/// decoding-envelope admission.
#[derive(Default)]
pub(super) struct FloorScratch {
    key: Vec<u8>,
    other: Vec<u8>,
}

/// How [`super::exec::string_order_mask`] evaluates the next mask or part.
pub(super) enum FloorMaskPlan {
    /// Use the shared floor table (or uncached floors when `None` because the memory
    /// budget refused a table).
    Floors(Option<Arc<FloorTable>>),
    /// Bypass the floor table for this mask and compare keys directly.
    Direct,
}

/// Result of committing a mask whose sample validated via direct key comparison.
pub(super) enum DirectCommitPlan {
    /// Stay on direct key comparison without allocating or growing a floor table.
    Direct,
    /// Committing would consider optional table growth or enter `Floors`. `FloorTables`
    /// was left unchanged so the caller can run a non-sticky floor trial on the validated
    /// sample before optional table memory is spent.
    NeedsFloorTrial(Option<Arc<FloorTable>>),
}

/// Result of looking up a string floor in [`FloorMap::try_floor`].
pub(super) enum FloorLookup {
    /// Base-vocabulary simple literal with floor `u64`.
    Floor(u64),
    /// Not a base-vocabulary simple literal (evaluate via generic SPARQL comparison).
    NotSimple,
    /// Computing the floor refused decoding-envelope admission without recording a sticky
    /// memory failure (caller may fall back to direct key comparison).
    Refused,
}

/// The query's cache of string floors ([`Ctx::string_floor`]). It lasts the query, so the
/// batches of a cursor share it as the rows of an eager table do, and every worker of a
/// mask reads and writes the same [`FloorTable`] without a lock.
///
/// The first table is sized from an estimate of the distinct repeating strings in the
/// first eligible mask (or [`FLOOR_TABLE_FIRST`] for direct callers), clamped to the
/// vocabulary's strings and the cache's cap. A mask asks before each part of its rows
/// whether the table should grow or pause ([`Ctx::begin_floor_mask`]), growing when its
/// estimate of the floors it evicted that had been read again passes
/// [`FLOOR_TABLE_EVICTIONS`]. A query whose strings rarely repeat, whose evictions cost
/// nothing, therefore keeps its first table, and while the table's hit rate stays below
/// `1 / FLOOR_BYPASS_HIT_DENOM` masks bypass the table for direct key comparison,
/// probing periodically so inputs that repeat only across later batches re-enable floors.
/// The new table is [`FLOOR_TABLE_GROWTH`] times larger and receives the live entries
/// copied from the old table before it is published, so lookups probe only the current
/// table. Each table is charged to the query before it is allocated, through a
/// reservation that grows in steps as the value cache's does, and old tables stay charged
/// until the context drops since an in-flight mask can still hold one. If an oversized
/// initial table does not fit the remaining memory budget, allocation steps down by
/// halves to [`FLOOR_TABLE_FEWEST`] before giving up. A refused charge, or one past the
/// cache's cap, keeps the table the query has, or leaves it without one, and the floors
/// it does not hold are computed each time, which costs about what a direct comparison of
/// the two keys costs.
#[derive(Default)]
pub(super) struct FloorCache {
    /// the table the masks read and write
    tables: parking_lot::Mutex<FloorTables>,
    /// whether the cache has already installed a first table or deferred it in bypass mode
    initialized: AtomicBool,
    /// the bytes of every table allocated
    used: AtomicU64,
    /// the bytes charged for the cache, never fewer than `used`
    charge: StepReservation,
    /// the floors computed (for tests that the cache lasts the query)
    #[cfg(test)]
    computed: std::sync::atomic::AtomicUsize,
    /// the slots of the first table, when a test sets them
    #[cfg(test)]
    first_slots: std::sync::atomic::AtomicUsize,
    /// masks or parts bypassed to direct key comparison (for tests)
    #[cfg(test)]
    bypassed: std::sync::atomic::AtomicUsize,
    /// the threads that looked up floors (for tests that a mask spreads over the pool)
    #[cfg(test)]
    threads: parking_lot::Mutex<std::collections::HashSet<std::thread::ThreadId>>,
}

#[derive(Default)]
struct FloorTables {
    cur: Option<Arc<FloorTable>>,
    /// a table was refused, so the cache takes no other
    full: bool,
    /// saved initial slot estimate when the first eligible mask deferred table allocation
    deferred_slots: usize,
    /// remaining lookups before a freshly pre-sized table (`len > FLOOR_TABLE_FIRST`)
    /// is evaluated for low-hit-rate bypass
    warmup_lookups: usize,
    /// whether low hit rate has currently paused floor-table use
    bypass: bool,
    /// consecutive bypassed masks or parts since the last floor probe
    bypass_streak: u32,
}

/// A direct-mapped table of string floors shared by the workers of a query. Slot
/// `p & mask` holds one word for base id `p`: from the top, `(p >> bits) + 1`, which
/// tells the ids of a slot apart and is never zero, so an empty slot holds none; then the
/// distance from `p` down to its floor, all ones for an id that is not a simple literal;
/// then a bit set once the floor is read again. A floor farther away than the distance
/// bits can hold is not kept. Each word is written and read whole, so a reader sees a
/// word some worker wrote for an id of that slot, or zero, and a word whose top part
/// matches is the floor of that id, which does not change while the query reads one
/// snapshot. Two workers that compute the same floor store the same word, and a store
/// that replaces another id's word only loses that floor. The words therefore need no
/// ordering with other memory.
pub(super) struct FloorTable {
    slots: Box<[AtomicU64]>,
    /// log2 of the slots
    bits: u32,
    /// the shift of the top part, above the distance and the read bit
    shift: u32,
    /// the distance that marks an id that is not a simple literal
    none: u64,
    /// an estimate of the floors evicted that had been read again, counted by the maps
    /// that end against the word each read before its store, which another worker may
    /// have replaced in between
    evicted: std::sync::atomic::AtomicUsize,
    /// total lookups performed against this table since the last hit-rate check
    lookups: std::sync::atomic::AtomicUsize,
    /// cache hits observed against this table since the last hit-rate check
    hits: std::sync::atomic::AtomicUsize,
}

impl FloorTable {
    /// A table of `slots`, a power of two, for base ids below `first_triple`.
    fn new(slots: usize, first_triple: u64) -> Self {
        debug_assert!(slots.is_power_of_two());
        let bits = slots.trailing_zeros();
        // The top part reaches `(first_triple - 1 >> bits) + 1`, which needs one bit more
        // than `first_triple >> bits` takes. Base ids have at most `PAYLOAD_BITS`, so the
        // distance keeps at least two bits.
        let top_bits = 64 - (first_triple >> bits).leading_zeros() + 1;
        let delta_bits = 64 - 1 - top_bits;
        debug_assert!(delta_bits >= 2);
        FloorTable {
            slots: (0..slots).map(|_| AtomicU64::new(0)).collect(),
            bits,
            shift: delta_bits + 1,
            none: (1 << delta_bits) - 1,
            evicted: Default::default(),
            lookups: Default::default(),
            hits: Default::default(),
        }
    }

    /// The bytes of a table of `slots`: its words, the table and the shared counts of
    /// its `Arc`.
    pub(super) fn bytes(slots: usize) -> u64 {
        (slots as u64)
            .saturating_mul(8)
            .saturating_add(Self::OVERHEAD)
    }

    /// The bytes of a table besides its words.
    pub(super) const OVERHEAD: u64 =
        (std::mem::size_of::<FloorTable>() + 2 * std::mem::size_of::<usize>()) as u64;

    fn len(&self) -> usize {
        self.slots.len()
    }

    #[inline]
    fn slot(&self, p: u64) -> &AtomicU64 {
        &self.slots[(p as usize) & (self.slots.len() - 1)]
    }

    #[inline]
    fn top(&self, p: u64) -> u64 {
        (p >> self.bits) + 1
    }

    /// The floor of `p` in `word`, when the word is `p`'s: `Some(None)` for an id that is
    /// not a simple literal.
    #[inline]
    fn decode(&self, p: u64, word: u64) -> Option<Option<u64>> {
        if word >> self.shift != self.top(p) {
            return None;
        }
        let delta = (word >> 1) & self.none;
        Some((delta != self.none).then(|| p - delta))
    }

    /// Read-only cache lookup: `Some(Some(floor))` on a simple-literal hit, `Some(None)`
    /// on a cached non-simple literal, `None` on a miss.
    #[inline]
    pub(super) fn get_cached(&self, p: u64) -> Option<Option<u64>> {
        self.decode(p, self.slot(p).load(Ordering::Relaxed))
    }

    /// The floor of `p`, when the table holds it (for tests).
    #[cfg(test)]
    #[inline]
    fn get(&self, p: u64) -> Option<Option<u64>> {
        self.decode(p, self.slot(p).load(Ordering::Relaxed))
    }

    /// The word of `p` with floor `floor`, unless the floor is too far to hold.
    #[inline]
    fn encode(&self, p: u64, floor: Option<u64>, read: bool) -> Option<u64> {
        let delta = floor.map_or(self.none, |f| p - f);
        (floor.is_none() || delta < self.none)
            .then(|| (self.top(p) << self.shift) | (delta << 1) | u64::from(read))
    }

    /// Keep the floor of `p`, unless it is too far to hold. Whether it was kept.
    #[inline]
    fn put(&self, p: u64, floor: Option<u64>, read: bool) -> bool {
        let Some(word) = self.encode(p, floor, read) else {
            return false;
        };
        self.slot(p).store(word, Ordering::Relaxed);
        true
    }

    /// Copy every live floor held by this table into a larger table `dst` before `dst` is
    /// published. Each non-zero slot in `self` recovers its exact base id `p` from its
    /// slot index and top tag, and distinct slots of `self` map to distinct slots of `dst`.
    fn copy_into(&self, dst: &mut FloorTable) {
        debug_assert!(dst.len() > self.len());
        debug_assert!(dst.bits > self.bits);
        let dst_mask = dst.slots.len() - 1;
        for (idx, slot) in self.slots.iter().enumerate() {
            let word = slot.load(Ordering::Relaxed);
            if word == 0 {
                continue;
            }
            let top = word >> self.shift;
            let p = ((top - 1) << self.bits) | (idx as u64);
            let delta = (word >> 1) & self.none;
            let floor = (delta != self.none).then(|| p - delta);
            let read = (word & 1) != 0;
            if let Some(encoded) = dst.encode(p, floor, read) {
                *dst.slots[(p as usize) & dst_mask].get_mut() = encoded;
            }
        }
    }

    /// The words that hold a floor (for tests).
    #[cfg(test)]
    fn entries(&self) -> usize {
        self.slots
            .iter()
            .filter(|w| w.load(Ordering::Relaxed) != 0)
            .count()
    }
}

impl FloorCache {
    /// The most the cache holds under a memory budget of `mem_limit` bytes.
    fn cap(mem_limit: u64) -> u64 {
        FLOOR_CACHE_BYTES.min(mem_limit / 16)
    }

    /// The bytes charged for the cache.
    #[cfg(test)]
    pub(super) fn reserved(&self) -> u64 {
        self.charge.reserved()
    }

    /// The floors computed for the cache, whether or not it kept them.
    #[cfg(test)]
    pub(super) fn computed(&self) -> usize {
        self.computed.load(Ordering::Relaxed)
    }

    /// The floors its current table holds.
    #[cfg(test)]
    pub(super) fn entries(&self) -> usize {
        self.tables.lock().cur.as_ref().map_or(0, |t| t.entries())
    }

    /// The bytes of every table allocated.
    #[cfg(test)]
    pub(super) fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    /// The slots of its current table.
    #[cfg(test)]
    pub(super) fn slots(&self) -> usize {
        self.tables.lock().cur.as_ref().map_or(0, |t| t.len())
    }

    /// Start the cache with a table of `slots`.
    #[cfg(test)]
    pub(super) fn set_first_slots(&self, slots: usize) {
        self.first_slots.store(slots, Ordering::Relaxed);
    }

    /// The threads that have looked up floors.
    #[cfg(test)]
    pub(super) fn threads(&self) -> usize {
        self.threads.lock().len()
    }

    /// Masks or parts bypassed to direct key comparison due to low floor hit rate.
    #[cfg(test)]
    pub(super) fn bypassed(&self) -> usize {
        self.bypassed.load(Ordering::Relaxed)
    }
}

enum FloorTableRef<'a> {
    #[cfg(test)]
    Owned(Arc<FloorTable>),
    Borrowed(&'a FloorTable),
}

impl FloorTableRef<'_> {
    #[inline]
    fn as_ref(&self) -> &FloorTable {
        match self {
            #[cfg(test)]
            FloorTableRef::Owned(arc) => arc,
            FloorTableRef::Borrowed(table) => table,
        }
    }
}

/// A worker's view of the query's [`FloorCache`]: the table it reads and writes, its
/// reusable key reconstruction buffers, and the counts it adds to the cache when it ends.
pub(super) struct FloorMap<'a> {
    ctx: &'a Ctx,
    cur: Option<FloorTableRef<'a>>,
    first_triple: u64,
    scratch: FloorScratch,
    /// the floors that had been read again that this map's stores evicted
    evicted: usize,
    /// lookups performed against `cur`
    lookups: usize,
    /// cache hits observed against `cur`
    hits: usize,
    #[cfg(test)]
    computed: usize,
}

impl Drop for FloorMap<'_> {
    fn drop(&mut self) {
        if let Some(cur) = self.cur.as_ref().map(FloorTableRef::as_ref) {
            if self.evicted > 0 {
                cur.evicted.fetch_add(self.evicted, Ordering::Relaxed);
            }
            if self.lookups > 0 {
                cur.lookups.fetch_add(self.lookups, Ordering::Relaxed);
            }
            if self.hits > 0 {
                cur.hits.fetch_add(self.hits, Ordering::Relaxed);
            }
        }
        #[cfg(test)]
        {
            let cache = &self.ctx.floors;
            cache.computed.fetch_add(self.computed, Ordering::Relaxed);
            cache.threads.lock().insert(std::thread::current().id());
        }
    }
}

impl FloorMap<'_> {
    /// [`Ctx::string_floor`] of `id`, computed once while the table holds it.
    #[cfg(test)]
    #[inline]
    pub(super) fn floor(&mut self, id: Id) -> Option<u64> {
        match self.try_floor(id) {
            FloorLookup::Floor(f) => Some(f),
            FloorLookup::NotSimple => None,
            FloorLookup::Refused => self.ctx.string_floor_in(id, &mut self.scratch),
        }
    }

    /// Look up `id`'s string floor without recording a sticky memory failure on a
    /// decoding-envelope refusal, so a caller with direct key comparison available can
    /// decline optional floor-only work (such as a block-start predecessor decode) and
    /// fall back to [`Ctx::simple_literal_cmp_in`] using [`FloorMap::scratch_mut`].
    #[inline]
    pub(super) fn try_floor(&mut self, id: Id) -> FloorLookup {
        let p = id.payload();
        if id.tag() != Tag::Vocab || p >= self.first_triple {
            return FloorLookup::NotSimple;
        }
        let Some(cur) = self.cur.as_ref().map(FloorTableRef::as_ref) else {
            return self.try_compute(id);
        };
        self.lookups += 1;
        let slot = cur.slot(p);
        let word = slot.load(Ordering::Relaxed);
        if let Some(floor) = cur.decode(p, word) {
            self.hits += 1;
            if word & 1 == 0 {
                slot.store(word | 1, Ordering::Relaxed);
            }
            return match floor {
                Some(f) => FloorLookup::Floor(f),
                None => FloorLookup::NotSimple,
            };
        }
        // The slot held another id's floor, which had been read again when its bit is set.
        let evicts = usize::from(word & 1 == 1);
        #[cfg(test)]
        {
            self.computed += 1;
        }
        match self.ctx.try_string_floor_in(id, &mut self.scratch) {
            Ok(floor) => {
                if cur.put(p, floor, false) {
                    self.evicted += evicts;
                }
                match floor {
                    Some(f) => FloorLookup::Floor(f),
                    None => FloorLookup::NotSimple,
                }
            }
            // Do not cache a refusal as a non-simple literal or record a sticky failure;
            // the caller falls back to direct comparison, which records any required refusal.
            Err(_) => FloorLookup::Refused,
        }
    }

    /// Reusable key-reconstruction scratch owned by this worker map, available for a
    /// per-row [`Ctx::simple_literal_cmp_in`] fallback after [`FloorLookup::Refused`].
    #[inline]
    pub(super) fn scratch_mut(&mut self) -> &mut FloorScratch {
        &mut self.scratch
    }

    /// Record a floor already computed by [`Ctx::string_floor`] for a base-vocabulary simple
    /// literal `id`. Preserves lookup, hit, read-bit, eviction and test `computed` accounting
    /// identically to [`FloorMap::try_floor`] without recomputing the floor.
    #[inline]
    pub(super) fn record_floor(&mut self, id: Id, floor: u64, computed: bool) {
        let p = id.payload();
        debug_assert!(id.tag() == Tag::Vocab && p < self.first_triple);
        #[cfg(test)]
        if computed {
            self.computed += 1;
        }
        #[cfg(not(test))]
        let _ = computed;
        let Some(cur) = self.cur.as_ref().map(FloorTableRef::as_ref) else {
            return;
        };
        self.lookups += 1;
        let slot = cur.slot(p);
        let word = slot.load(Ordering::Relaxed);
        if cur.decode(p, word).is_some() {
            self.hits += 1;
            if word & 1 == 0 {
                slot.store(word | 1, Ordering::Relaxed);
            }
            return;
        }
        let evicts = usize::from(word & 1 == 1);
        if cur.put(p, Some(floor), false) {
            self.evicted += evicts;
        }
    }

    #[inline]
    fn try_compute(&mut self, id: Id) -> FloorLookup {
        #[cfg(test)]
        {
            self.computed += 1;
        }
        match self.ctx.try_string_floor_in(id, &mut self.scratch) {
            Ok(Some(f)) => FloorLookup::Floor(f),
            Ok(None) => FloorLookup::NotSimple,
            Err(_) => FloorLookup::Refused,
        }
    }
}

/// Executor optimizations that can be switched off for diagnosis (every one of them has
/// a generic fallback with the same results). `SPARKLES_DISABLE_OPTIMIZATIONS` holds a
/// comma-separated list of names that are off by default in a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Optimizations {
    /// numeric range FILTERs on a scan's sorted column read only the matching id ranges
    pub range_pushdown: bool,
    /// aggregates that can keep one running state per group do, rather than reading
    /// their group's rows, which is the diagnostic path when this is off
    pub incremental_group: bool,
    /// COUNT(*) over a join of two scans counts per-key runs instead of reading rows
    pub count_join_runs: bool,
    /// per-class counts from index statistics when they are exact
    pub metadata_counts: bool,
    /// property paths expand a whole frontier per index sweep
    pub batched_paths: bool,
    /// ORDER BY one numeric variable with LIMIT ranks rounded keys first
    pub topk_prefilter: bool,
    /// ORDER BY a scan's variable with LIMIT reads the scan in value order and stops once
    /// the first rows are proven
    pub ordered_topk: bool,
    /// spatial FILTERs on an indexed predicate's object search the spatial index
    pub spatial_pushdown: bool,
    /// `FILTER (NOT) EXISTS` over a join group probes a key set built once from the
    /// pattern, instead of evaluating the substituted pattern per outer row
    pub decorrelate_exists: bool,
    /// a join with a selective input reads a triple pattern only for the input's distinct
    /// keys, by clustered seeks over a permutation sorted on the key
    pub batched_join: bool,
    /// index joins on one subject over constant predicates are read together, walking
    /// each subject's run once when that touches fewer blocks
    pub star_fusion: bool,
    /// a spatial FILTER between the geometries of two join components joins them
    pub spatial_join: bool,
    /// ORDER BY a distance to a constant with LIMIT searches the nearest geometries
    pub spatial_knn: bool,
    /// pure expressions over one variable (FILTER, BIND, ORDER BY keys, aggregate
    /// arguments) are evaluated once per distinct value
    pub expr_cache: bool,
    /// MINUS on one variable that both sides always bind removes rows by a merge or a
    /// probe of single ids instead of hashing a key per row
    pub anti_join: bool,
    /// ORDER BY several keys with LIMIT evaluates the later keys only on the rows that
    /// the first key does not rule out
    pub topk_first_key: bool,
    /// COUNT(*) over a FILTER on one variable of a single scan tests the filter once per
    /// run of the variable in a permutation sorted on it and sums the run lengths
    pub count_filter_runs: bool,
    /// a FILTER over a scan sorted on a variable it tests reads the runs of that
    /// variable, tests each value once and copies only the rows of the values that pass
    pub filter_scan_runs: bool,
    /// index joins find each key's rows by galloping from the previous key's position
    /// and keep the current block, instead of a binary search and a scan per cluster
    pub gallop_index_join: bool,
    /// OPTIONAL on one variable, with both sides sorted on it and always binding it,
    /// runs as a merge in the left side's order
    pub merge_left_join: bool,
    /// a streamed merge OPTIONAL with a condition tests it on the pairs of the left rows
    /// that a loaded right batch decides before it copies any output row, instead of
    /// copying every pair and extension and testing the batch afterwards
    pub merge_pair_conditions: bool,
    /// OPTIONAL whose right side is a triple pattern sorted on a variable that every row
    /// of the evaluated left side binds reads the pattern only for the left side's
    /// distinct values, when probing them costs less than reading the pattern
    pub probed_optional: bool,
    /// an OPTIONAL whose sides share no variable and whose condition equates a variable
    /// of each side pairs the rows by that equality instead of forming their product
    pub keyed_left_join: bool,
    /// two parts of a group that share no variable, linked by a FILTER conjunct that
    /// equates a variable of each, are joined by that equality instead of their product
    pub keyed_join: bool,
    /// a triple pattern with one variable of which the store holds one row binds the
    /// variable to that value for the rest of its group at planning time
    pub point_patterns: bool,
    /// a FILTER conjunct over the variables of one triple pattern is tested on a sample
    /// of the pattern's rows, whose share that passes is the planner's estimate of the
    /// share of its input it keeps (instead of 30%)
    pub sampled_filters: bool,
    /// joins on a subject variable of patterns with constant predicates are estimated
    /// from the characteristic sets in the statistics
    pub characteristic_sets: bool,
    /// a join of a small input (VALUES, or a pattern of few rows) with a pattern is
    /// estimated by counting the pattern's rows for a sample of the input's values
    pub probed_keys: bool,
    /// the characteristic sets estimate stars over the subjects of the sets the
    /// statistics do not keep from their remainder, instead of using only the predicates
    /// that the kept sets cover
    pub set_remainders: bool,
    /// joins on a variable that is the object of a pattern of its group are estimated
    /// from the node sets of the statistics (incoming and outgoing predicates per node)
    pub node_sets: bool,
    /// a join on several keys combines their distinct counts by exponential back-off
    /// instead of dividing by the largest alone
    pub key_backoff: bool,
    /// a join against a pattern that restricts the other column by a constant other than
    /// a class divides by the predicate's distinct values of the key column at least
    pub domain_estimates: bool,
    /// index joins that a fused star reads together are costed for the keys of the
    /// star's input, which a fused star probes in every pattern
    pub fused_star_costs: bool,
    /// an index join with few keys asks the kernel to read every block it will visit
    /// before it decodes the first, so a cold server reads them in parallel
    #[cfg(test)]
    pub prefetch_blocks: bool,
    /// a hash join whose right side is a `text:query` call without a limit searches only
    /// the subjects of a small left side
    pub text_subject_pushdown: bool,
    /// ORDER BY DESC(spk:cosine(?v, C)) LIMIT k (or spk:dot) over one pattern reads the
    /// k best rows by an exact vector search
    pub vector_topk: bool,
    /// ORDER BY with LIMIT keeps the best rows in a bounded heap of offset + limit rows,
    /// evaluating the later keys only for rows whose first key can still enter
    pub topk_heap: bool,
    /// an ORDER BY key equal to the expression of a BIND or SELECT expression directly
    /// below it orders by that expression's variable instead of evaluating it again
    pub bound_order_keys: bool,
    /// the vocabulary groups that hold a query's constants are requested together before
    /// the planner looks the constants up one after the other
    #[cfg(test)]
    pub prefetch_constants: bool,
    /// a FILTER, BIND or ORDER BY over a table of fewer than 4,096 rows asks for the
    /// vocabulary pages of the terms it evaluates before the first row
    #[cfg(test)]
    pub prefetch_terms: bool,
    /// the blocks of a plan's scans with few blocks are requested before any operator runs
    #[cfg(test)]
    pub prefetch_first_scan: bool,
    /// the columns of the blocks that an index join has asked the kernel for are decoded
    /// in parallel on a pool of the cache's own, before the reader gets to them.
    /// Integration tests can disable this to check foreground decoding.
    #[doc(hidden)]
    pub parallel_decode: bool,
    /// a streamed result writer decodes the terms of the next batch on the Rayon pool
    /// while it writes the current one. Integration tests can disable this to check
    /// sequential output.
    #[doc(hidden)]
    pub decode_ahead: bool,
    /// cursor operators that read their whole input before output, and filters without
    /// effects whose rows do not pass, read larger inputs as a long input goes on
    pub grow_input: bool,
    /// the predicates of a set of subjects, with the subjects having each, and
    /// `spk:hasPredicate` are read from the generation's predicate patterns
    pub predicate_patterns: bool,
}

impl Optimizations {
    pub const NAMES: [&str; 39] = [
        "range_pushdown",
        "incremental_group",
        "count_join_runs",
        "metadata_counts",
        "batched_paths",
        "topk_prefilter",
        "ordered_topk",
        "spatial_pushdown",
        "decorrelate_exists",
        "batched_join",
        "star_fusion",
        "spatial_join",
        "spatial_knn",
        "expr_cache",
        "anti_join",
        "topk_first_key",
        "count_filter_runs",
        "filter_scan_runs",
        "gallop_index_join",
        "merge_left_join",
        "merge_pair_conditions",
        "probed_optional",
        "keyed_left_join",
        "keyed_join",
        "point_patterns",
        "sampled_filters",
        "characteristic_sets",
        "probed_keys",
        "set_remainders",
        "node_sets",
        "key_backoff",
        "domain_estimates",
        "fused_star_costs",
        "text_subject_pushdown",
        "vector_topk",
        "topk_heap",
        "bound_order_keys",
        "grow_input",
        "predicate_patterns",
    ];

    /// Everything on.
    pub const ALL: Optimizations = Optimizations {
        range_pushdown: true,
        incremental_group: true,
        count_join_runs: true,
        metadata_counts: true,
        batched_paths: true,
        topk_prefilter: true,
        ordered_topk: true,
        spatial_pushdown: true,
        decorrelate_exists: true,
        batched_join: true,
        star_fusion: true,
        spatial_join: true,
        spatial_knn: true,
        expr_cache: true,
        anti_join: true,
        topk_first_key: true,
        count_filter_runs: true,
        filter_scan_runs: true,
        gallop_index_join: true,
        merge_left_join: true,
        merge_pair_conditions: true,
        probed_optional: true,
        keyed_left_join: true,
        keyed_join: true,
        point_patterns: true,
        sampled_filters: true,
        characteristic_sets: true,
        probed_keys: true,
        set_remainders: true,
        node_sets: true,
        key_backoff: true,
        domain_estimates: true,
        fused_star_costs: true,
        #[cfg(test)]
        prefetch_blocks: true,
        text_subject_pushdown: true,
        vector_topk: true,
        topk_heap: true,
        bound_order_keys: true,
        #[cfg(test)]
        prefetch_constants: true,
        #[cfg(test)]
        prefetch_terms: true,
        #[cfg(test)]
        prefetch_first_scan: true,
        parallel_decode: true,
        decode_ahead: true,
        grow_input: true,
        predicate_patterns: true,
    };

    /// All named optimizations off, using the generic operators.
    pub const NONE: Optimizations = Optimizations {
        range_pushdown: false,
        incremental_group: false,
        count_join_runs: false,
        metadata_counts: false,
        batched_paths: false,
        topk_prefilter: false,
        ordered_topk: false,
        spatial_pushdown: false,
        decorrelate_exists: false,
        batched_join: false,
        star_fusion: false,
        spatial_join: false,
        spatial_knn: false,
        expr_cache: false,
        anti_join: false,
        topk_first_key: false,
        count_filter_runs: false,
        filter_scan_runs: false,
        gallop_index_join: false,
        merge_left_join: false,
        merge_pair_conditions: false,
        probed_optional: false,
        keyed_left_join: false,
        keyed_join: false,
        point_patterns: false,
        sampled_filters: false,
        characteristic_sets: false,
        probed_keys: false,
        set_remainders: false,
        node_sets: false,
        key_backoff: false,
        domain_estimates: false,
        fused_star_costs: false,
        #[cfg(test)]
        prefetch_blocks: true,
        text_subject_pushdown: false,
        vector_topk: false,
        topk_heap: false,
        bound_order_keys: false,
        #[cfg(test)]
        prefetch_constants: true,
        #[cfg(test)]
        prefetch_terms: true,
        #[cfg(test)]
        prefetch_first_scan: true,
        parallel_decode: true,
        decode_ahead: true,
        grow_input: false,
        predicate_patterns: false,
    };

    fn flag(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "range_pushdown" => &mut self.range_pushdown,
            "incremental_group" => &mut self.incremental_group,
            "count_join_runs" => &mut self.count_join_runs,
            "metadata_counts" => &mut self.metadata_counts,
            "batched_paths" => &mut self.batched_paths,
            "topk_prefilter" => &mut self.topk_prefilter,
            "ordered_topk" => &mut self.ordered_topk,
            "spatial_pushdown" => &mut self.spatial_pushdown,
            "decorrelate_exists" => &mut self.decorrelate_exists,
            "batched_join" => &mut self.batched_join,
            "star_fusion" => &mut self.star_fusion,
            "spatial_join" => &mut self.spatial_join,
            "spatial_knn" => &mut self.spatial_knn,
            "expr_cache" => &mut self.expr_cache,
            "anti_join" => &mut self.anti_join,
            "topk_first_key" => &mut self.topk_first_key,
            "count_filter_runs" => &mut self.count_filter_runs,
            "filter_scan_runs" => &mut self.filter_scan_runs,
            "gallop_index_join" => &mut self.gallop_index_join,
            "merge_left_join" => &mut self.merge_left_join,
            "merge_pair_conditions" => &mut self.merge_pair_conditions,
            "probed_optional" => &mut self.probed_optional,
            "keyed_left_join" => &mut self.keyed_left_join,
            "keyed_join" => &mut self.keyed_join,
            "point_patterns" => &mut self.point_patterns,
            "sampled_filters" => &mut self.sampled_filters,
            "characteristic_sets" => &mut self.characteristic_sets,
            "probed_keys" => &mut self.probed_keys,
            "set_remainders" => &mut self.set_remainders,
            "node_sets" => &mut self.node_sets,
            "key_backoff" => &mut self.key_backoff,
            "domain_estimates" => &mut self.domain_estimates,
            "fused_star_costs" => &mut self.fused_star_costs,
            "text_subject_pushdown" => &mut self.text_subject_pushdown,
            "vector_topk" => &mut self.vector_topk,
            "topk_heap" => &mut self.topk_heap,
            "bound_order_keys" => &mut self.bound_order_keys,
            "grow_input" => &mut self.grow_input,
            "predicate_patterns" => &mut self.predicate_patterns,
            _ => return None,
        })
    }

    /// Switch off optimizations by name (`all` selects [`Self::NONE`]). Unknown names
    /// are returned as an error.
    pub fn disable(mut self, names: &str) -> std::result::Result<Optimizations, String> {
        for name in Self::split_names(names) {
            self.disable_name(name)?;
        }
        Ok(self)
    }

    fn split_names(names: &str) -> impl Iterator<Item = &str> {
        names
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    fn disable_name(&mut self, name: &str) -> std::result::Result<(), String> {
        if name == "all" {
            *self = Self::NONE;
        } else {
            *self.flag(name).ok_or_else(|| {
                format!(
                    "unknown optimization {name:?} (known: {})",
                    Self::NAMES.join(", ")
                )
            })? = false;
        }
        Ok(())
    }
}

impl Default for Optimizations {
    /// Everything on, except what `SPARKLES_DISABLE_OPTIMIZATIONS` lists.
    /// Retired names are ignored and unknown names are logged.
    fn default() -> Optimizations {
        static DEFAULT: std::sync::OnceLock<Optimizations> = std::sync::OnceLock::new();
        *DEFAULT.get_or_init(|| {
            let mut optimizations = Self::ALL;
            if let Ok(names) = std::env::var("SPARKLES_DISABLE_OPTIMIZATIONS") {
                for name in Self::split_names(&names) {
                    if matches!(
                        name,
                        "prefetch_blocks" | "prefetch_constants" | "prefetch_terms"
                            | "prefetch_first_scan" | "parallel_decode" | "decode_ahead"
                    ) {
                        continue;
                    }
                    if let Err(error) = optimizations.disable_name(name) {
                        tracing::warn!(target: "sparkles::sparql::ctx", "SPARKLES_DISABLE_OPTIMIZATIONS: {error}");
                    }
                }
            }
            optimizations
        })
    }
}

/// A note about a plan for its reader (explain output, MCP): something did not run the
/// way the query suggests, though the answer is the same.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PlanWarning {
    /// stable identifier (`geo-not-pushed`, `geo-index-building`, `geo-not-built`, …)
    pub code: &'static str,
    pub message: String,
}

/// Query-local vocabulary backing: either an owned [`AppendVocab`] for ordinary queries
/// or an incremental circuit's [`Terms`](crate::incr::terms::Terms) borrowed for the
/// duration of an accumulator call without copying historical keys.
enum LocalVocab {
    Owned(AppendVocab),
    Circuit(crate::incr::terms::Terms),
}

impl Default for LocalVocab {
    fn default() -> Self {
        Self::Owned(AppendVocab::default())
    }
}

impl LocalVocab {
    fn len(&self) -> u64 {
        match self {
            Self::Owned(v) => v.len(),
            Self::Circuit(t) => t.key_count() as u64,
        }
    }

    fn get(&self, id: u64) -> Option<&[u8]> {
        match self {
            Self::Owned(v) => v.get(id),
            Self::Circuit(t) => t.key_bytes(id as usize),
        }
    }

    fn find(&self, key: &[u8]) -> Option<u64> {
        match self {
            Self::Owned(v) => v.find(key),
            Self::Circuit(t) => t.find_local_key(key),
        }
    }

    fn insert(&mut self, key: &[u8]) -> (u64, bool) {
        match self {
            Self::Owned(v) => v.insert(key),
            Self::Circuit(t) => {
                let before = t.key_count();
                let id = t.push_local_key(key);
                (id.payload(), t.key_count() > before)
            }
        }
    }

    fn clone_owned(&self) -> AppendVocab {
        match self {
            Self::Owned(v) => v.clone(),
            Self::Circuit(t) => {
                let mut v = AppendVocab::default();
                for i in 0..t.key_count() {
                    if let Some(k) = t.key_bytes(i) {
                        v.insert(k);
                    }
                }
                v
            }
        }
    }

    fn append_only_clone_bytes(&self) -> u64 {
        match self {
            Self::Owned(v) => v.append_only_clone_bytes(),
            Self::Circuit(t) => t.bytes(),
        }
    }

    fn physical_bytes(&self) -> u64 {
        match self {
            Self::Owned(v) => v.physical_bytes(),
            Self::Circuit(t) => t.bytes(),
        }
    }

    fn insert_peak_bytes(&self, key_len: usize) -> Option<u64> {
        match self {
            Self::Owned(v) => v.insert_peak_bytes(key_len),
            Self::Circuit(t) => t.insert_peak_bytes(key_len),
        }
    }
}

pub struct Ctx {
    pub extensions: Option<Arc<super::extensions::ExtensionRegistry>>,
    /// what query-time reasoning derived for this query (see [`super::reasoning`])
    pub reasoning: Option<Arc<super::reasoning::Applied>>,
    /// The snapshot holds facts with query-local terms, which query-time reasoning
    /// derived (spec F17). A pattern constant the store does not have may then match.
    pub local_facts: bool,
    /// The memory the derived facts of query-time reasoning hold in the snapshot's
    /// overlay, charged to the query until the context drops.
    pub(super) reasoning_memory: Vec<QueryMemoryLease>,
    pub snap: Arc<Snapshot>,
    local: RwLock<LocalVocab>,
    /// Bytes of `local` currently covered by retained context credit (mutated under
    /// `local.write()`).
    local_retained: AtomicU64,
    values: ValueCache,
    floors: FloorCache,
    next_bnode: AtomicU64,
    bnode_memo: parking_lot::Mutex<FxHashMap<(Vec<Id>, String), Id>>,
    /// blank nodes given to the query from outside (initial bindings) that are not
    /// stored ones: a fresh blank node per label
    outside_bnodes: parking_lot::Mutex<FxHashMap<String, Id>>,
    /// the blank nodes that labels inside the composite literals of the query text name
    /// (see [`Ctx::query_literal`])
    literal_bnodes: parking_lot::Mutex<FxHashMap<String, Id>>,
    /// The query's deadline, without the time the query waited for its consumer. Code
    /// that hands a deadline to other work reads [`Ctx::current_deadline`], which adds
    /// [`Ctx::deadline_pause`].
    pub deadline: Option<Instant>,
    /// Nanoseconds the query spent waiting for its consumer, which the deadline does not
    /// count. The thread that drives the cursor adds to it, after each stretch of waiting
    /// for its consumer, and the same thread runs every check, so a check never misses a
    /// wait that is still in progress. A background reader of the deadline would need to
    /// credit a wait before it starts.
    pub deadline_pause: Arc<AtomicU64>,
    /// Whether the query runs as a cursor under the strict policy. An operator that
    /// decides at its first pull how to run, such as an in-process SERVICE call, reads
    /// it, since the operators of an EXISTS pattern are built under the permissive
    /// policy once their strict build has passed.
    pub(crate) strict_cursor: AtomicBool,
    pub cancel: Arc<AtomicBool>,
    pub cancellation: Option<crate::task::Cancellation>,
    pub dataset: DatasetSpec,
    /// A validation selector has only its selected default data, even for ARQ graph aliases.
    pub(super) selector_default_only: bool,
    /// the request's graph view when it does not read every graph: `dataset` is already
    /// limited to it (see [`DatasetSpec::restrict`]), and plans are redacted
    pub graphs: Option<Arc<crate::access::GraphAccess>>,
    /// The query's `NOW()`, read from the clock on first use so that queries that never
    /// call it skip the clock and the calendar conversion. One value serves the whole
    /// query, as SPARQL requires (see [`Ctx::now`]).
    now: std::sync::OnceLock<oxsdatatypes::DateTime>,
    pub base_iri: Option<oxiri::Iri<String>>,
    pub var_names: RwLock<Vec<String>>,
    prepared_var_names: parking_lot::Mutex<PreparedVariables>,
    /// Maximum number of rows any intermediate result may have (memory guard).
    pub max_rows: usize,
    /// Budget for the estimated bytes of intermediate results alive at once
    /// (`u64::MAX`: unlimited). See [`Ctx::charge`] and [`Ctx::check_output`].
    pub mem_limit: u64,
    /// The query's disk reservation for spill files, when spill is on.
    pub(crate) spill: Option<Arc<crate::spill::QuerySpill>>,
    /// Budget for the rows all operators produce together (`u64::MAX`: unlimited). See
    /// [`Ctx::produced`].
    pub max_rows_produced: u64,
    /// rows produced so far, summed over operators (shared by the WHERE clauses of one
    /// update)
    pub rows_produced: Arc<AtomicU64>,
    pub allow_service: bool,
    /// SERVICE fails with [`crate::Error::NotPermitted`] (see
    /// [`QueryOptions::forbid_service`](super::QueryOptions::forbid_service))
    pub forbid_service: bool,
    /// network policy of SERVICE
    pub outbound: crate::outbound::OutboundPolicy,
    /// what the SERVICE calls (and, in an update, the LOADs) of the request have spent
    /// of the policy's totals
    pub outbound_budget: Arc<crate::outbound::RequestBudget>,
    /// consult / fill the store's result cache
    pub use_cache: bool,
    /// name the query's patterns and variables in the plan (see
    /// [`QueryOptions::no_plan_patterns`](super::QueryOptions::no_plan_patterns))
    pub plan_patterns: bool,
    /// the caller's scope in the cache of remote SERVICE results (see
    /// [`QueryOptions::service_scope`](super::QueryOptions::service_scope))
    pub service_scope: Arc<str>,
    /// whether the query may call `se:cacheRm` (see
    /// [`QueryOptions::service_cache_mgmt`](super::QueryOptions::service_cache_mgmt))
    pub service_cache_mgmt: bool,
    /// the process's own query endpoints (see [`super::LocalEndpoints`])
    pub local_endpoints: Option<Arc<dyn super::LocalEndpoints>>,
    /// how many in-process SERVICE calls this query runs inside
    pub service_depth: u32,
    /// the dataset's named projections (see [`crate::algo::catalog`])
    pub algo_projections: Option<Arc<crate::algo::catalog::Catalog>>,
    /// the dataset's views, for `SERVICE view:read`
    pub views: Option<Arc<dyn super::viewsvc::ViewSource>>,
    /// RDFS on read (see [`super::rdfs`])
    pub rdfs: Option<Arc<super::rdfs::RdfsSchema>>,
    pub opt: Optimizations,
    /// How the query's aggregates treat errors and values that are not strings: the
    /// request's choice, else the snapshot's (see [`super::AggregateErrors`])
    pub aggregate_errors: super::AggregateErrors,
    /// the I/O counters when the query began, taken only under a profile (see
    /// [`Ctx::io_stats`]); boxed, so that a context without a profile stays small
    io_start: Option<Box<crate::ioacct::IoSnapshot>>,
    /// parsed geometries of this query
    pub geo: crate::geo::memo::GeoMemo,
    /// notes for the plan's reader, without duplicates (see [`Ctx::warn`])
    warnings: parking_lot::Mutex<Vec<PlanWarning>>,
    /// the planner met a `SERVICE` of the graph-algorithm namespace (see
    /// [`Ctx::calls_algorithm`])
    algorithmic: AtomicBool,
    /// The plan of the operator that failed, as far as it ran, keyed by the address of
    /// its node, until its parent takes it (see [`Ctx::take_failed`]). Execution state:
    /// a plan shared between executions must not keep it.
    failed: parking_lot::Mutex<Option<(usize, super::exec::PlanInfo)>>,
    /// FILTER selectivities measured on samples for this query, by conjunct text
    sampled: parking_lot::Mutex<FxHashMap<String, super::sample::Sampled>>,
    /// the star predicates registered per subject variable (see [`super::charsets`])
    pub(super) stars: parking_lot::Mutex<FxHashMap<VarId, super::charsets::StarVar>>,
    /// the small input of a variable and the patterns probed with its values (see
    /// [`super::keyprobe`])
    pub(super) probes: parking_lot::Mutex<FxHashMap<VarId, super::keyprobe::VarProbe>>,
    // Keep callback bookkeeping after the ordinary execution fields. The registry
    // stays first so its user-owned callbacks keep their existing drop order.
    pub(crate) extension_failure: Option<super::extensions::Failure>,
    pub(crate) calls_extensions: bool,
    pub(crate) extension_families: Vec<uuid::Uuid>,
    pub(crate) extension_ancestors: Vec<super::extensions::Failure>,
    pub(crate) extension_writers: Vec<(uuid::Uuid, super::extensions::Failure)>,
    callback_bnodes: parking_lot::Mutex<FxHashMap<String, Id>>,
    callback_terms: parking_lot::Mutex<FxHashMap<Id, u64>>,
    /// A budget refusal in infallible code, which fails the next check (see
    /// [`Ctx::fail_memory`]).
    memory_failure: MemoryFailure,
    /// the envelope of transient dictionary decoding
    decode: DecodeEnvelope,
    /// the charge of the query-local terms and memos
    retained: RetainedTerms,
    /// the context itself, once it is shared, which retained charges release through
    /// (see [`Ctx::attach_owner`])
    owner: parking_lot::Mutex<Weak<Ctx>>,
    /// the query's compiled regular expressions, charged in both accounting modes
    regex: super::query_regex::RegexCache,
    // Opt-in cursor accounting stays after the ordinary execution fields.
    /// whether the query streams through pull cursors, whose grant follows their live
    /// bytes (see [`Ctx::streams`])
    streams: bool,
    /// active scoped strict-decode guards (such as constant folding) that refuse eager
    /// decode envelope overflows instead of silently dropping them
    strict_decode: AtomicU32,
    // Data and weak retained caches drop before final ledger ownership.
    fast: FastMemory,
    plan_scratch_leases: parking_lot::Mutex<Vec<QueryMemoryLease>>,
    memory: std::sync::OnceLock<Arc<QueryMemoryLedger>>,
    // All ordinary payloads, including graphs and ledger callbacks, retire before
    // their actual sources. A new mask can share its original source's backing.
    pub(super) source_view: Option<crate::access::PreparedView>,
    pub(super) read_source: Option<crate::store::BoundReadSource>,
    pub(super) view_source_controls: Option<crate::access::Preparation>,
    pub(super) source_preparation: Option<crate::access::Preparation>,
    pub(super) source_setup: Option<crate::access::Reservation>,
    #[cfg(test)]
    pub(super) chunk_hook: parking_lot::Mutex<Option<ChunkHook>>,
}

/// A test observer of the chunks a retained output takes.
#[cfg(test)]
pub(super) type ChunkHook = Arc<dyn Fn(&'static str, usize) + Send + Sync>;

// Deterministic test trigger to expire any query deadline at a chosen checkpoint.
#[cfg(test)]
thread_local! {
    pub(super) static TEST_DEADLINE_EXPIRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Set in [`FastMemory::live`] once the ledger has taken the account over.
const PROMOTED: u64 = 1 << 63;

/// The step in which the fast account admits its eager contribution to the grant. A
/// plain query's estimate grows by a few bytes at a time, and each growth past the
/// contribution takes the account's lock and the grant's counters, so the account
/// admits up to the next step at once. The contribution may then exceed the highest
/// estimate by less than one step.
const EAGER_STEP: u64 = 64 << 10;

/// How far above what a finished eager result keeps its pool contribution may stay
/// before [`Ctx::shrink_retained`] lowers it. Below this the ledger's lock costs more
/// than the memory it would return.
const RETAIN_SLACK: u64 = 1 << 20;

/// The memory account of an eager context until something needs the query ledger: a
/// graph algorithm call, a lease that outlives an operator, cursor accounting or a
/// change of the grant. Plain queries charge and check it with a few atomic operations
/// and never create the ledger. [`Ctx::memory`] hands the account to the ledger as its
/// [`PRIMARY`] owner, after which every charge goes through the ledger.
#[derive(Default)]
struct FastMemory {
    /// live bytes, with [`PROMOTED`] set once the ledger took the account over
    live: AtomicU64,
    /// the highest estimate seen while the account was fast
    peak: AtomicU64,
    /// the account's admitted contribution to `grant`, kept until the context drops
    eager: AtomicU64,
    /// The grant fixed before the first charge. A promoted account owns its credit
    /// through the ledger, which shares this grant.
    grant: Option<Arc<PoolGrant>>,
    /// serializes growth of `eager` with the hand-over to the ledger
    resize: parking_lot::Mutex<()>,
}

impl FastMemory {
    #[inline]
    fn note_peak(&self, bytes: u64) {
        if bytes > self.peak.load(Ordering::Relaxed) {
            self.peak.fetch_max(bytes, Ordering::Relaxed);
        }
    }

    /// Give back `bytes` of the account's live estimate. `false` once the ledger owns
    /// the account.
    #[inline]
    fn release(&self, bytes: u64) -> bool {
        let mut cur = self.live.load(Ordering::Acquire);
        loop {
            if cur & PROMOTED != 0 {
                return false;
            }
            let next = cur.checked_sub(bytes).expect("query live bytes");
            match self
                .live
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(c) => cur = c,
            }
        }
    }
}

impl Drop for FastMemory {
    /// An account the ledger never took over returns its eager contribution here.
    fn drop(&mut self) {
        let eager = *self.eager.get_mut();
        if eager != 0
            && *self.live.get_mut() & PROMOTED == 0
            && let Some(grant) = &self.grant
        {
            grant.release(eager);
        }
    }
}

/// How a context accounts its memory, the one place that reads the mode (see
/// [`Ctx::accounting`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Accounting {
    /// Eager execution, which collects whole tables. Its pool contribution stays at
    /// the admitted high point until the result is dropped.
    Collect,
    /// Pull cursors, whose grant follows their live bytes.
    Cursor,
}

/// The first supported execution refusal that infallible code met. Scalar evaluation
/// records it here so the next check fails with it even when the expression falls back.
#[derive(Default)]
struct MemoryFailure {
    failure: parking_lot::Mutex<Option<ResourceRefusal>>,
    failed: AtomicBool,
    /// The open [`Ctx::optional_eval`] scopes on this context, across threads, that a
    /// memory refusal has met. While it is zero, [`Ctx::has_memory_failure`] reads no
    /// thread-local state.
    refused_scopes: AtomicUsize,
}

thread_local! {
    /// The optional evaluation this thread runs, as the address of its context and
    /// whether a memory refusal met it (see [`Ctx::optional_eval`]).
    static OPTIONAL_EVAL: std::cell::Cell<Option<(usize, bool)>> =
        const { std::cell::Cell::new(None) };
}

/// Whether `error` refuses query or server memory, which an optional cache yields to
/// the ordinary evaluation instead of failing the query with it. A memory parent's
/// limit gives one of these. Its other refusals, such as an overflow or a foreign
/// parent, are ownership errors that ordinary evaluation meets as well, so they fail
/// the query.
pub(crate) fn is_memory_refusal(error: &Error) -> bool {
    matches!(
        error,
        Error::BudgetExceeded(budget)
            if matches!(budget.kind, BudgetKind::Memory | BudgetKind::ServerMemory)
    )
}

/// Resource and ownership refusals, the unconfigured extension refusal and the strict
/// cursor policy's refusals become sticky. Ordinary expression and SERVICE errors keep
/// their existing semantics, so a failed SERVICE inside an EXISTS is still false.
#[derive(Clone)]
enum ResourceRefusal {
    Budget(Budget),
    Parent {
        component: &'static str,
        code: String,
        message: String,
        source: sparkles_budget::ParentRefused,
    },
    Cancelled,
    Timeout,
    /// The strict cursor policy's refusal of an eager fallback, met where the operator
    /// decides how to run at its first pull, such as an in-process SERVICE call inside
    /// an EXISTS. It fails the query, as the same refusal does outside an expression.
    Refused(String),
    /// The strict cursor policy's refusal of a SERVICE response it cannot stream, such as
    /// one that gives its results before its head. It keeps its SERVICE error.
    ServiceRefused(String),
    UnconfiguredExtension,
    Corrupt(String),
    /// A side effect of an expression that failed, such as a removal from the SERVICE
    /// cache whose files could not be synced, which only its call site records.
    Io(String),
}

impl ResourceRefusal {
    /// The refusal `error` records, if any. A SERVICE refusal is recognised by its fixed
    /// beginning, which no endpoint controls (see [`super::cursor::LATE_HEAD`]), and only
    /// under a strict cursor, the only policy that makes it.
    fn from_error(error: &Error, strict: bool) -> Option<Self> {
        match error {
            Error::Service(message) if strict && message.starts_with(super::cursor::LATE_HEAD) => {
                Some(Self::ServiceRefused(message.clone()))
            }
            Error::BudgetExceeded(budget) => Some(Self::Budget(*budget)),
            Error::Cancelled => Some(Self::Cancelled),
            Error::Timeout => Some(Self::Timeout),
            Error::Unsupported(message) if message.starts_with(super::cursor::REFUSAL) => {
                Some(Self::Refused(message.clone()))
            }
            Error::Corrupt(message) => Some(Self::Corrupt(message.clone())),
            error if super::extensions::is_unconfigured(error) => Some(Self::UnconfiguredExtension),
            Error::Component(component) => component
                .source
                .as_ref()
                .and_then(|source| source.downcast_ref::<sparkles_budget::ParentRefused>())
                .map(|source| Self::Parent {
                    component: component.component,
                    code: component.code.clone(),
                    message: component.message.clone(),
                    source: *source,
                }),
            _ => None,
        }
    }

    fn into_error(self) -> Error {
        match self {
            Self::Budget(budget) => Error::BudgetExceeded(budget),
            Self::Cancelled => Error::Cancelled,
            Self::Timeout => Error::Timeout,
            Self::Refused(message) => Error::Unsupported(message),
            Self::ServiceRefused(message) => Error::Service(message),
            Self::UnconfiguredExtension => super::extensions::unconfigured(),
            Self::Corrupt(message) => Error::Corrupt(message),
            Self::Io(message) => Error::Io(std::io::Error::other(message)),
            Self::Parent {
                component,
                code,
                message,
                source,
            } => Error::Component(Box::new(
                crate::error::ComponentError::new(component, code, message).with_source(source),
            )),
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum DecodeGrowPoint {
    BeforeLock,
    BeforeEagerCharge,
    AfterLedgerSnapshot,
}

#[cfg(test)]
type DecodeGrowHook = Arc<dyn Fn(DecodeGrowPoint) + Send + Sync>;

/// The high-water envelope that transient dictionary decoding reserves in both
/// accounting modes: a key of the largest size seen so far on every worker, charged once
/// when it grows and released when the work ends (see [`Ctx::reserve_decode_checked`]).
#[derive(Default)]
struct DecodeEnvelope {
    /// the reserved bytes, with the `(owner, bytes)` charges backing the envelope
    state: parking_lot::Mutex<(u64, Vec<(u64, u64)>)>,
    /// the reserved bytes, read without the lock
    reserved: AtomicU64,
    #[cfg(test)]
    grow_hook: parking_lot::Mutex<Option<DecodeGrowHook>>,
}

#[cfg(test)]
impl DecodeEnvelope {
    fn notify_grow_hook(&self, point: DecodeGrowPoint) {
        let hook = self.grow_hook.lock().clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// [`Ctx::check`] calls on this thread (for tests of per-query overhead)
    pub(crate) static CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// growths of a fast account's grant contribution on this thread
    static FAST_GROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Ctx {
    /// The query's memory ledger, made on first use from the fast account (see
    /// [`FastMemory`]).
    pub(crate) fn memory(&self) -> &Arc<QueryMemoryLedger> {
        self.memory.get_or_init(|| self.promote())
    }

    #[cold]
    #[inline(never)]
    fn promote(&self) -> Arc<QueryMemoryLedger> {
        let fast = &self.fast;
        // No eager growth is in flight, and none starts on the fast account after this.
        let _resize = fast.resize.lock();
        let live = fast.live.fetch_or(PROMOTED, Ordering::AcqRel);
        debug_assert_eq!(live & PROMOTED, 0, "one hand-over");
        QueryMemoryLedger::seeded(
            fast.grant.clone(),
            live,
            fast.eager.load(Ordering::Acquire),
            fast.peak.load(Ordering::Acquire),
        )
    }

    pub(super) fn owns_ledger(&self, ledger: &Arc<QueryMemoryLedger>) -> bool {
        self.memory
            .get()
            .is_some_and(|own| Arc::ptr_eq(own, ledger))
    }

    /// A distinct reasoning child sharing this query's live total and original
    /// backing. Construct it only when reasoning starts; ordinary execution keeps
    /// its existing lazy fast account. The child retains accounting, not this Ctx.
    pub fn memory_parent(&self) -> Result<sparkles_budget::ParentHandle> {
        self.memory()
            .child_parent(self.fast.grant.clone(), self.mem_limit)
    }

    /// Attach a seed subcontext to its outer query before any charge. It retains
    /// its own ledger/data lifetime and never reuses the outer primary owner id.
    pub(crate) fn set_memory_parent(
        &mut self,
        parent: sparkles_budget::ParentHandle,
    ) -> Result<()> {
        if self.memory.get().is_some()
            || self.fast.live.load(Ordering::Relaxed) != 0
            || self.fast.eager.load(Ordering::Relaxed) != 0
            || self.fast.peak.load(Ordering::Relaxed) != 0
        {
            return Err(Error::invalid(
                "upstream query memory is set before the first charge",
            ));
        }
        let factory = self
            .grant()
            .map_or(sparkles_budget::MemoryDomain::unpooled(), |g| {
                g.memory_domain()
            });
        if factory.id() != parent.domain() {
            return Err(Error::Component(Box::new(
                crate::error::ComponentError::new(
                    "query-memory",
                    "foreign-domain",
                    "upstream memory belongs to another accounting domain",
                ),
            )));
        }
        let ledger = QueryMemoryLedger::nested(self.fast.grant.clone(), parent);
        self.memory
            .set(ledger)
            .map_err(|_| Error::invalid("query memory already initialized"))?;
        self.fast.live.store(PROMOTED, Ordering::Release);
        Ok(())
    }

    /// The grant the query charges, if it has one.
    pub fn grant(&self) -> Option<&Arc<PoolGrant>> {
        self.fast.grant.as_ref()
    }

    /// Set the grant before any charge. The fast account borrows this same reference
    /// on each charge and returns its contribution before dropping it.
    pub(crate) fn set_grant(&mut self, grant: Option<Arc<PoolGrant>>) {
        debug_assert!(
            self.memory.get().is_none()
                && self.fast.live.load(Ordering::Relaxed) == 0
                && self.fast.eager.load(Ordering::Relaxed) == 0
                && self.fast.peak.load(Ordering::Relaxed) == 0,
            "the grant is set before the first charge"
        );
        self.fast.grant = grant;
    }

    /// Preserve the old fast account in the ledger before testing a change of grant.
    #[cfg(test)]
    pub(crate) fn reconfigure_grant(&mut self, grant: Option<Arc<PoolGrant>>) {
        if (self.fast.live.load(Ordering::Relaxed) != 0
            || self.fast.eager.load(Ordering::Relaxed) != 0
            || self.fast.peak.load(Ordering::Relaxed) != 0)
            && !super::query_memory::same(self.grant(), grant.as_ref())
        {
            self.memory();
        }
        self.fast.grant = grant;
    }

    /// Admit `bytes` more live estimate (`add`) or check a transient output of
    /// `bytes` on the fast account, with the ledger's semantics for a single eager
    /// owner. `None` when the ledger must take the request.
    #[inline]
    fn fast_admit(&self, bytes: u64, add: bool) -> Option<Result<()>> {
        if self.memory.get().is_some() {
            return None;
        }
        let grant = self.grant();
        let fast = &self.fast;
        let mut cur = fast.live.load(Ordering::Acquire);
        loop {
            if cur & PROMOTED != 0 {
                return None;
            }
            let need = cur.checked_add(bytes).filter(|n| *n < PROMOTED)?;
            if !add {
                // Like the ledger, an output check records the attempted estimate.
                fast.note_peak(need);
            }
            if need > self.mem_limit {
                return Some(Err(self.memory_exceeded(need)));
            }
            if let Some(grant) = grant
                && need > fast.eager.load(Ordering::Acquire)
            {
                return self.fast_grow(grant, bytes, add);
            }
            if !add {
                return Some(Ok(()));
            }
            match fast
                .live
                .compare_exchange_weak(cur, need, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    fast.note_peak(need);
                    return Some(Ok(()));
                }
                Err(c) => cur = c,
            }
        }
    }

    /// [`Ctx::fast_admit`] when the estimate passes the eager contribution: grow it
    /// from the grant first, so a refused grant changes nothing.
    #[cold]
    #[inline(never)]
    fn fast_grow(&self, grant: &Arc<PoolGrant>, bytes: u64, add: bool) -> Option<Result<()>> {
        #[cfg(test)]
        FAST_GROWS.with(|n| n.set(n.get() + 1));
        let fast = &self.fast;
        let _resize = fast.resize.lock();
        let mut cur = fast.live.load(Ordering::Acquire);
        loop {
            if cur & PROMOTED != 0 {
                return None;
            }
            let need = cur.checked_add(bytes).filter(|n| *n < PROMOTED)?;
            if need > self.mem_limit {
                return Some(Err(self.memory_exceeded(need)));
            }
            let eager = fast.eager.load(Ordering::Acquire);
            if need > eager {
                // Admit a step ahead, within the local budget, so that the next small
                // increases stay on the lock-free path. When the pool cannot give the
                // step, admit exactly what the estimate needs.
                let ahead = need
                    .next_multiple_of(EAGER_STEP)
                    .min(self.mem_limit)
                    .max(need);
                let admitted = match grant.charge(ahead - eager) {
                    Ok(()) => ahead,
                    Err(_) if ahead > need => match grant.charge(need - eager) {
                        Ok(()) => need,
                        Err(error) => return Some(Err(error)),
                    },
                    Err(error) => return Some(Err(error)),
                };
                fast.eager.store(admitted, Ordering::Release);
            }
            if !add {
                return Some(Ok(()));
            }
            // Lock-free admissions in between stayed within the old contribution.
            match fast
                .live
                .compare_exchange(cur, need, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    fast.note_peak(need);
                    return Some(Ok(()));
                }
                Err(c) => cur = c,
            }
        }
    }

    /// [`Ctx::live_bytes`] for the tests of other modules.
    #[cfg(all(test, feature = "geo"))]
    pub(crate) fn live_bytes_for_tests(&self) -> u64 {
        self.live_bytes()
    }

    #[cfg(test)]
    pub(super) fn decode_state_unlocked(&self) -> bool {
        self.decode.state.try_lock().is_some()
    }

    #[cfg(test)]
    pub(super) fn intern_local_raw_for_test(&self, key: &[u8]) -> Id {
        Id::local(self.local.write().insert(key).0)
    }

    /// The query's live estimate, from the ledger once it exists.
    #[inline]
    pub(crate) fn live_bytes(&self) -> u64 {
        match self.memory.get() {
            Some(ledger) => ledger.live.load(Ordering::Relaxed),
            None => self.fast.live.load(Ordering::Relaxed) & !PROMOTED,
        }
    }

    pub fn new(snap: Arc<Snapshot>) -> Ctx {
        let aggregate_errors = snap.aggregate_errors;
        Ctx {
            reasoning: None,
            local_facts: false,
            reasoning_memory: Vec::new(),
            extension_families: Vec::new(),
            extension_writers: Vec::new(),
            extension_ancestors: Vec::new(),
            extensions: None,
            extension_failure: Default::default(),
            calls_extensions: false,
            callback_bnodes: Default::default(),
            callback_terms: Default::default(),
            memory_failure: Default::default(),
            decode: Default::default(),
            retained: Default::default(),
            owner: Default::default(),
            regex: Default::default(),
            streams: false,
            strict_decode: AtomicU32::new(0),
            fast: Default::default(),
            plan_scratch_leases: Default::default(),
            memory: Default::default(),
            snap,
            read_source: None,
            source_view: None,
            view_source_controls: None,
            source_preparation: None,
            source_setup: None,
            #[cfg(test)]
            chunk_hook: parking_lot::Mutex::new(None),
            local: RwLock::new(LocalVocab::default()),
            local_retained: AtomicU64::new(0),
            values: Default::default(),
            floors: Default::default(),
            next_bnode: AtomicU64::new(0),
            bnode_memo: Default::default(),
            outside_bnodes: Default::default(),
            literal_bnodes: Default::default(),
            deadline: None,
            deadline_pause: Arc::new(AtomicU64::new(0)),
            strict_cursor: AtomicBool::new(false),
            cancel: Arc::new(AtomicBool::new(false)),
            cancellation: None,
            dataset: DatasetSpec::default(),
            selector_default_only: false,
            graphs: None,
            now: std::sync::OnceLock::new(),
            base_iri: None,
            var_names: RwLock::new(Vec::new()),
            prepared_var_names: Default::default(),
            max_rows: 200_000_000,
            mem_limit: u64::MAX,
            spill: None,
            max_rows_produced: u64::MAX,
            rows_produced: Arc::new(AtomicU64::new(0)),
            allow_service: true,
            forbid_service: false,
            outbound: Default::default(),
            outbound_budget: crate::outbound::RequestBudget::new(&Default::default()),
            use_cache: true,
            plan_patterns: true,
            service_scope: Arc::from(""),
            service_cache_mgmt: false,
            local_endpoints: None,
            service_depth: 0,
            algo_projections: None,
            views: None,
            rdfs: None,
            opt: Optimizations::default(),
            aggregate_errors,
            io_start: crate::ioacct::profiling()
                .then(|| Box::new(crate::ioacct::IoSnapshot::now())),
            geo: Default::default(),
            warnings: Default::default(),
            algorithmic: AtomicBool::new(false),
            failed: Default::default(),
            sampled: Default::default(),
            stars: Default::default(),
            probes: Default::default(),
        }
    }

    /// What the read path counted since this context was made: index columns decoded,
    /// bytes asked of the kernel and vocabulary lookups (see [`crate::ioacct`]). The
    /// counters are process-wide, so reads of queries that run at the same time are
    /// included. The starting counters are taken only while a [`Profile`](crate::ioacct::Profile)
    /// is alive, so the result is meaningful only for a query made under one: otherwise it
    /// holds everything the process has counted.
    pub fn io_stats(&self) -> crate::ioacct::IoSnapshot {
        let now = crate::ioacct::IoSnapshot::now();
        match &self.io_start {
            Some(start) => now.since(start),
            None => now.since(&Default::default()),
        }
    }

    /// The query's `NOW()`: the clock at its first call, the same value at every later
    /// call of this context.
    pub fn now(&self) -> oxsdatatypes::DateTime {
        *self.now.get_or_init(oxsdatatypes::DateTime::now)
    }

    pub(crate) fn fail_extension(&self, error: super::extensions::ScalarError) {
        let error = error.bounded();
        if !matches!(error, super::extensions::ScalarError::Expression) {
            if let Some(failure) = &self.extension_failure {
                failure.lock().get_or_insert(error.clone());
            }
            for ancestor in &self.extension_ancestors {
                ancestor.lock().get_or_insert(error.clone());
            }
            for (_, writer) in self
                .extension_writers
                .iter()
                .filter(|(id, _)| *id == self.snap.dataset_id)
            {
                writer.lock().get_or_insert(error.clone());
            }
        }
    }

    pub(crate) fn configure_extensions(&mut self, pattern: &spargebra::algebra::GraphPattern) {
        self.calls_extensions = self
            .extensions
            .as_ref()
            .is_some_and(|r| !r.is_empty() && r.references(pattern));
        if self.calls_extensions {
            self.extension_failure = Some(Default::default());
            self.extension_families = super::extensions::captured_families(self.snap.dataset_id);
            self.extension_writers = super::extensions::captured_writers();
            self.extension_ancestors = super::extensions::captured_ancestors();
            self.use_cache = false;
            self.opt.expr_cache = false;
            self.opt.sampled_filters = false;
            self.opt.decorrelate_exists = false;
            self.opt.count_filter_runs = false;
            self.opt.filter_scan_runs = false;
            // Ranking the first key separately can evaluate it twice and omit calls
            // from later keys on discarded rows.
            self.opt.topk_first_key = false;
        }
    }

    pub(crate) fn retain_callback_bytes(&self, id: Id, bytes: u64) -> Result<()> {
        let mut retained = self.callback_terms.lock();
        if retained.contains_key(&id) {
            return Ok(());
        }
        let charge = self.charge(bytes)?;
        retained.insert(id, bytes);
        // This vocabulary belongs to Ctx and is released with it; unlike temporary
        // callback batches it must remain charged for the rest of query execution.
        std::mem::forget(charge);
        Ok(())
    }

    /// Callback blank nodes may refer back to nodes it received, but arbitrary labels
    /// cannot forge stored identities. Newly returned labels live in this query only.
    pub(crate) fn intern_callback_term(&self, term: &Term, arguments: &[Term]) -> Result<Id> {
        self.check()?;
        fn permit(label: &str, out: &mut FxHashMap<String, Id>, charge: &Charge<'_>) -> Result<()> {
            if let Some(payload) = id::parse_bnode_payload(label)
                && !out.contains_key(label)
            {
                charge.add(label.len() as u64 + 96)?;
                out.insert(label.to_owned(), Id::bnode(payload));
            }
            Ok(())
        }
        fn collect(
            term: &Term,
            out: &mut FxHashMap<String, Id>,
            charge: &Charge<'_>,
        ) -> Result<()> {
            match term {
                Term::BlankNode(b) => permit(b.as_str(), out, charge)?,
                Term::Triple(t) => {
                    collect(&t.subject.clone().into(), out, charge)?;
                    collect(&t.object, out, charge)?;
                }
                _ => {}
            }
            Ok(())
        }
        let allowed_charge = self.charge(0)?;
        let mut allowed = FxHashMap::default();
        for term in arguments {
            collect(term, &mut allowed, &allowed_charge)?;
            let mut failure = None;
            let _parsed = super::cdt::callback_relabel_term(self, term, &mut |b| {
                if failure.is_none() {
                    failure = permit(b, &mut allowed, &allowed_charge).err();
                }
                b.to_owned()
            })?;
            if let Some(error) = failure {
                return Err(error);
            }
        }
        let mut failure = None;
        let mut choose = |b: &str| {
            if let Some(id) = allowed.get(b) {
                return *id;
            }
            let mut nodes = self.callback_bnodes.lock();
            if let Some(id) = nodes.get(b) {
                return *id;
            }
            // Keep label-map ownership charged independently of returned values.
            // A failed conversion can never allocate new unaccounted identities.
            if failure.is_some() {
                return Id::bnode(0);
            }
            match self.charge(b.len() as u64 + 96) {
                Ok(charge) => {
                    let id = self.fresh_bnode();
                    nodes.insert(b.to_owned(), id);
                    std::mem::forget(charge);
                    id
                }
                Err(error) => {
                    failure = Some(error);
                    Id::bnode(0)
                }
            }
        };
        let _mapped_charge =
            self.charge(super::extensions::term_bytes(term).map_err(|e| e.engine())?)?;
        let term = self.map_bnodes(term, &mut choose);
        let relabeled = super::cdt::callback_relabel_term(self, &term, &mut |b| {
            id::bnode_label(choose(b).payload())
        })?;
        if let Some(error) = failure {
            return Err(error);
        }
        let mapped = relabeled.term.as_ref().unwrap_or(&term);
        let bytes = super::extensions::term_bytes(mapped).map_err(|e| e.engine())?;
        self.check()?;
        let _output = self.charge(bytes)?;
        let id = self.intern_term(mapped);
        if id.tag() == Tag::Local
            || (id.tag() == Tag::BNode && id.payload() & Id::LOCAL_BNODE_BIT != 0)
        {
            self.retain_callback_bytes(id, bytes)?;
        }
        Ok(id)
    }

    /// Record a warning for the plan (once, however often it is raised).
    pub fn warn(&self, w: PlanWarning) {
        self.try_warn(w);
    }

    /// [`Ctx::warn`], returning whether the plan holds the warning. It does not when
    /// the budget refused the warning's bytes, which inside [`Ctx::optional_eval`]
    /// leaves the query running, so a caller that warns once per key marks the key
    /// only on success and warns again on a later evaluation.
    pub(crate) fn try_warn(&self, w: PlanWarning) -> bool {
        let mut ws = self.warnings.lock();
        if ws.contains(&w) {
            return true;
        }
        if self.retain_bytes(w.message.len() as u64 + 128) {
            ws.push(w);
            return true;
        }
        false
    }

    /// Keep `info` as the plan of the failed node `node` (see [`Ctx::take_failed`]).
    pub(super) fn set_failed(&self, node: &super::plan::Node, info: super::exec::PlanInfo) {
        *self.failed.lock() = Some((node as *const _ as usize, info));
    }

    /// The plan of the failed node `node`, when the failure came from it.
    /// Forget the plan of a failure that the executor recovered from, such as a LIMIT's
    /// whole run that a budget refused before the LIMIT pulled its input instead.
    pub(super) fn clear_failed(&self) {
        *self.failed.lock() = None;
    }

    pub(super) fn take_failed(&self, node: &super::plan::Node) -> Option<super::exec::PlanInfo> {
        let mut f = self.failed.lock();
        match &*f {
            Some((at, _)) if *at == node as *const _ as usize => f.take().map(|(_, i)| i),
            _ => None,
        }
    }

    /// The plan of the operator tree that failed, as far as it ran, when a failure
    /// recorded one.
    pub fn failed_plan(&self) -> Option<super::exec::PlanInfo> {
        self.failed.lock().take().map(|(_, i)| i)
    }

    /// Whether the parsed query calls a graph algorithm, that is, has a `SERVICE` whose
    /// IRI is in the algorithm namespace once prefixes and the base are resolved. The
    /// planner decides it from the algebra, so a prefixed name counts like the full IRI.
    pub fn calls_algorithm(&self) -> bool {
        self.algorithmic.load(Ordering::Relaxed)
    }

    pub(super) fn note_algorithm_call(&self) {
        self.algorithmic.store(true, Ordering::Relaxed);
    }

    /// The warnings recorded so far.
    pub fn warnings(&self) -> Vec<PlanWarning> {
        self.warnings.lock().clone()
    }

    /// The selectivity measured on a sample for the FILTER conjunct shown as `text`, if
    /// one was measured while planning this query.
    pub(super) fn sampled(&self, text: &str) -> Option<super::sample::Sampled> {
        let m = self.sampled.lock();
        if m.is_empty() {
            return None;
        }
        m.get(text).copied()
    }

    /// Whether some selectivity was measured on a sample while planning this query, so
    /// that [`Ctx::sampled`] can find one.
    pub(super) fn has_sampled(&self) -> bool {
        !self.sampled.lock().is_empty()
    }

    /// Keep the selectivity measured for the conjunct shown as `text` (the first one
    /// measured stays, so that every plan compared sees the same).
    pub(super) fn set_sampled(
        &self,
        scratch: Option<Charge<'_>>,
        text: String,
        s: super::sample::Sampled,
    ) {
        use sparkles_budget::sizes::{table_bytes, table_grown};
        let mut samples = self.sampled.lock();
        if samples.contains_key(&text) {
            return;
        }
        if let Some(held) = &scratch {
            let cap = samples.capacity();
            let grown = table_grown(cap, samples.len(), 1);
            let elem = size_of::<(String, super::sample::Sampled)>();
            let new_table = if grown == cap {
                0
            } else {
                table_bytes(grown, elem)
            };
            let text_bytes = text.capacity();
            if held
                .resize(text_bytes.saturating_add(new_table) as u64)
                .is_err()
            {
                return;
            }
            samples.insert(text, s);
            let retired = if grown == cap {
                0
            } else {
                table_bytes(cap, elem)
            };
            let _ = held.resize(text_bytes.saturating_add(new_table - retired) as u64);
            // The query owns the sample text and its table until the context drops.
            held.disarm();
        } else {
            samples.insert(text, s);
        }
    }

    #[inline]
    pub fn check(&self) -> Result<()> {
        #[cfg(test)]
        CHECKS.with(|n| n.set(n.get() + 1));
        self.check_memory_failure()?;
        if let Some(preparation) = self
            .source_preparation
            .as_ref()
            .or(self.view_source_controls.as_ref())
        {
            // Both source and query absolute controls apply during preparation
            // and execution, without cursor wait extensions or renewed deadlines.
            preparation.check()?;
        } else if let Some(source) = &self.read_source {
            // Source preparation has its original absolute deadline. Cursor wait
            // pauses never extend it, and no cancellation watcher is required.
            source.query_input().2.check()?;
        }
        if self.calls_extensions
            && let Some(state) = &self.extension_failure
            && let Some(failure) = state.lock().as_ref()
        {
            return Err(failure.engine());
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        if self.past_deadline() {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    /// Whether the deadline has passed, not counting the time the query spent waiting
    /// for its consumer. The counter is read only once the raw deadline has passed, which
    /// keeps the atomic load off the common path.
    pub fn past_deadline(&self) -> bool {
        let Some(d) = self.deadline else {
            return false;
        };
        #[cfg(test)]
        if TEST_DEADLINE_EXPIRED.with(std::cell::Cell::get) {
            return true;
        }
        let now = Instant::now();
        if now < d {
            return false;
        }
        let paused = std::time::Duration::from_nanos(self.deadline_pause.load(Ordering::Relaxed));
        d.checked_add(paused).is_some_and(|end| now >= end)
    }

    /// The deadline as it stands now, moved back by the time the query has waited for
    /// its consumer so far. Work that copies the deadline, such as a SERVICE call or an
    /// algorithm run, takes this value when it starts. A deadline too far out to
    /// represent is none.
    pub fn current_deadline(&self) -> Option<Instant> {
        let d = self.deadline?;
        let paused = std::time::Duration::from_nanos(self.deadline_pause.load(Ordering::Relaxed));
        d.checked_add(paused)
    }

    pub(super) fn enable_cursor_accounting(&mut self) -> Result<()> {
        self.memory().check_cursor_transition()?;
        // A cursor reserves its decoding on the ledger, so the eager envelope goes.
        self.release_decode_envelope();
        // The decoded value cache, the query-local terms and the parsed geometries are
        // charged already, and the ledger carries their reservations over, so the
        // cursor keeps them.
        self.memory().cursor_mode()?;
        self.streams = true;
        // The result cache and sampled filters stay as the query set them, so that a
        // query plans alike in every mode (item 827). The cache serves the subtrees a
        // cursor runs whole, and sampled filters take their samples once per plan.
        Ok(())
    }

    /// How this context accounts its memory.
    #[inline]
    pub(super) fn accounting(&self) -> Accounting {
        if self.streams {
            Accounting::Cursor
        } else {
            Accounting::Collect
        }
    }

    /// Whether the query streams through pull cursors. Both modes charge the same state
    /// and refuse decode-envelope overflows alike, so only the routing of a query and the
    /// accumulation of an EXISTS key set across batches read it.
    #[inline]
    pub(super) fn streams(&self) -> bool {
        self.accounting() == Accounting::Cursor
    }

    /// Let retained charges release through this shared context, in both accounting
    /// modes. Query preparation attaches every context it shares.
    ///
    /// Regular expressions compiled before, such as those of sampled filters in eager
    /// planning, are uncharged, so the first attachment drops them and execution
    /// compiles them again with their charges.
    pub(super) fn attach_owner(self: &Arc<Self>) {
        let mut owner = self.owner.lock();
        let first = owner.strong_count() == 0;
        *owner = Arc::downgrade(self);
        drop(owner);
        if first {
            self.regex.clear();
        }
    }

    /// This context in an `Arc` that holds a weak reference to it, as query preparation
    /// shares every context it runs. A caller that plans and executes a node itself
    /// shares its context with this once planning is done, since a LIMIT without ORDER
    /// BY, an ASK and an EXISTS pattern pull their input through cursor operators only
    /// to the rows they need in a shared context, and a context that was never shared
    /// runs that input whole (item 980). Retained state, such as the EXISTS memo, is
    /// charged to the query's memory budget from then on.
    pub fn into_shared(self) -> Arc<Ctx> {
        let ctx = Arc::new(self);
        ctx.attach_owner();
        ctx
    }

    /// The shared context, for an eager join that runs the cursor's partitioned join
    /// over its inputs when its build does not fit. `None` for a context that was never
    /// shared, such as one a test builds alone.
    pub(super) fn shared(&self) -> Option<Arc<Ctx>> {
        self.owner.lock().upgrade()
    }

    /// The cursor's own context, for state that runs a cursor operator tree of its own
    /// (the key set of an EXISTS pattern). `None` in eager execution.
    pub(super) fn cursor_owner(&self) -> Option<Arc<Ctx>> {
        if !self.streams {
            return None;
        }
        self.owner.lock().upgrade()
    }

    /// Weak owner for retained state, or `Ok(None)` when an unattached eager context
    /// keeps retained state uncharged.
    fn retained_owner(&self) -> Result<Option<Weak<Ctx>>> {
        let owner = self.owner.lock().clone();
        if owner.strong_count() == 0 {
            if !self.streams {
                return Ok(None);
            }
            return Err(Error::invalid("cursor context owner has not been attached"));
        }
        Ok(Some(owner))
    }

    /// Charge `bytes` of state that outlives the call that builds it, such as a cache
    /// that belongs to the plan, in both accounting modes. The charge holds a weak owner,
    /// which avoids a plan and context reference cycle. A context that was never shared
    /// has no owner to release through, so an eager one keeps such state uncharged, as
    /// `None` says, while a cursor always has one.
    pub(crate) fn retained_charge(&self, bytes: u64) -> Result<Option<RetainedCharge>> {
        let Some(owner) = self.retained_owner()? else {
            return Ok(None);
        };
        let charge = self.charge(bytes)?;
        let id = charge.into_disarmed();
        Ok(Some(RetainedCharge { owner, id, bytes }))
    }

    /// Split `bytes` out of a live [`QueryMemoryLease`] belonging to this attached context
    /// into a [`RetainedCharge`] without releasing or re-acquiring from the pool. If the
    /// lease holds fewer than `bytes`, only the shortfall is reserved.
    pub(super) fn retained_charge_from_lease(
        &self,
        lease: &mut QueryMemoryLease,
        bytes: u64,
    ) -> Result<Option<RetainedCharge>> {
        if lease.service.is_some() || !self.owns_ledger(&lease.ledger) {
            return Err(Error::invalid(
                "admission lease belongs to another query account",
            ));
        }
        let Some(owner) = self.retained_owner()? else {
            return Ok(None);
        };
        if bytes == 0 {
            return Ok(Some(RetainedCharge {
                owner,
                id: None,
                bytes: 0,
            }));
        }
        if bytes <= lease.bytes {
            lease.bytes -= bytes;
        } else {
            let extra = bytes - lease.bytes;
            lease
                .ledger
                .reserve(&lease.owner, self.mem_limit, extra, false)?;
            lease.bytes = 0;
        }
        Ok(Some(RetainedCharge {
            owner,
            id: Some(lease.owner.id),
            bytes,
        }))
    }

    /// The query's program of a regular expression, compiled and charged on first use.
    /// `Ok(None)` for a pattern or flags that do not compile.
    pub(super) fn regex(
        &self,
        pattern: &str,
        flags: &str,
    ) -> Result<Option<Arc<super::query_regex::RegexState>>> {
        self.regex.get(self, pattern, flags)
    }

    /// Run `f` on the query's program of a regular expression without taking a
    /// reference to it, which is the per-row path of `REGEX` and `REPLACE`.
    #[inline]
    pub(super) fn with_regex<R>(
        &self,
        pattern: &str,
        flags: &str,
        f: impl FnOnce(&super::query_regex::RegexState) -> R,
    ) -> Result<Option<R>> {
        self.regex.with(self, pattern, flags, f)
    }

    /// Give back `bytes` of estimated memory, and the pool's granules the query no
    /// longer needs.
    #[inline]
    fn release_live(&self, owner: Option<u64>, bytes: u64) {
        match owner {
            Some(PRIMARY) if self.fast.release(bytes) => {}
            Some(id) => self.memory().release(id, bytes),
            None => {
                debug_assert_eq!(bytes, 0);
                // Only cursor accounting trims, and it always has a ledger. An eager
                // query that a pulled LIMIT gave a ledger does not take its lock here.
                if self.streams
                    && let Some(ledger) = self.memory.get()
                {
                    ledger.trim_zero(self.grant());
                }
            }
        }
    }

    /// The bytes this query holds in the server's query memory pool (0 without a pool).
    pub fn granted_bytes(&self) -> u64 {
        self.grant().map_or(0, |g| g.granted())
    }

    pub(super) fn memory_remaining(&self) -> u64 {
        self.mem_limit.saturating_sub(self.live_bytes())
    }

    /// The bytes this query could still charge: what its own budget leaves, capped by
    /// what the server pool has free plus the unused part of the query's grant.
    pub(super) fn memory_room(&self) -> u64 {
        let own = self.memory_remaining();
        match self.grant() {
            Some(grant) => {
                let pool = grant.pool();
                let free = pool.capacity().saturating_sub(pool.granted());
                let spare = grant.granted().saturating_sub(grant.used());
                own.min(free.saturating_add(spare))
            }
            None => own,
        }
    }

    /// Release the state that only running work needs: the decode envelope and the
    /// query's regular expressions. Execution ends with it in both accounting modes.
    pub(super) fn release_work(&self) {
        self.regex.clear();
        self.release_decode_envelope();
    }

    /// [`Ctx::release_work`] under the name the cursor executor calls.
    pub(super) fn release_cursor_work(&self) {
        self.release_work();
    }

    fn release_decode_envelope(&self) {
        let envelope = &self.decode;
        let mut state = envelope.state.lock();
        let (bytes, owners) = std::mem::take(&mut *state);
        envelope.reserved.store(0, Ordering::Release);
        drop(state);
        if bytes != 0 {
            for (owner, owned) in owners {
                self.release_live(Some(owner), owned);
            }
        }
        if let Some(ledger) = self.memory.get()
            && ledger.has_envelope()
        {
            ledger.release_decode();
        }
    }

    /// Lightweight relaxed probe for a latched fatal refusal, or for a memory refusal
    /// that met the [`Ctx::optional_eval`] scope this thread runs on this context.
    /// Catching and short-circuiting expressions give up when it is true, so a refused
    /// optional evaluation takes no arm that the ordinary evaluation would skip.
    #[inline]
    pub(crate) fn has_memory_failure(&self) -> bool {
        let failure = &self.memory_failure;
        failure.failed.load(Ordering::Relaxed)
            || (failure.refused_scopes.load(Ordering::Relaxed) != 0 && self.optional_refused())
    }

    /// Whether the optional evaluation of this context that this thread runs, if any,
    /// has met a memory refusal.
    #[cold]
    #[inline(never)]
    fn optional_refused(&self) -> bool {
        let me = std::ptr::from_ref(self) as usize;
        OPTIONAL_EVAL.with(|scope| scope.get() == Some((me, true)))
    }

    /// Fail with the refusal that infallible code recorded (see
    /// [`Ctx::fail_memory`]).
    #[inline]
    pub(super) fn check_memory_failure(&self) -> Result<()> {
        let failure = &self.memory_failure;
        if failure.failed.load(Ordering::Acquire) {
            return self.recorded_failure();
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn recorded_failure(&self) -> Result<()> {
        let failure = self.memory_failure.failure.lock().clone();
        match failure {
            Some(failure) => Err(failure.into_error()),
            None => Ok(()),
        }
    }

    /// Record the first supported execution refusal met by infallible code, so
    /// the next check preserves its original typed outcome in both modes. A refusal of
    /// memory inside [`Ctx::optional_eval`] on this context is the one exception, which
    /// the optional evaluation reports to its caller instead, and which
    /// [`Ctx::has_memory_failure`] reports for the rest of the scope. Every state that
    /// the refused code leaves behind holds correct values, such as interned terms,
    /// decoded values, compiled regular expressions, parsed geometries and warnings
    /// marked as raised only once the plan holds them, so the ordinary evaluation that
    /// follows can reuse it.
    pub(crate) fn fail_memory(&self, error: &Error) {
        if self.memory_failure.failed.load(Ordering::Acquire) {
            return;
        }
        if is_memory_refusal(error) && self.note_optional_refusal() {
            return;
        }
        let strict = self.strict_cursor.load(Ordering::Relaxed);
        if let Some(refusal) = ResourceRefusal::from_error(error, strict) {
            let failure = &self.memory_failure;
            failure.failure.lock().get_or_insert(refusal);
            failure.failed.store(true, Ordering::Release);
        }
    }

    /// Record that a side effect of an expression failed with `error`, such as a removal
    /// from the SERVICE cache whose files could not be synced, so the query's next check
    /// fails with it. The expression itself can only give a type error, and no other
    /// error is recorded this way.
    pub(crate) fn fail_side_effect(&self, error: &Error) {
        if self.memory_failure.failed.load(Ordering::Acquire) {
            return;
        }
        let failure = &self.memory_failure;
        failure
            .failure
            .lock()
            .get_or_insert(ResourceRefusal::Io(error.to_string()));
        failure.failed.store(true, Ordering::Release);
    }

    /// Run `f` as optional evaluation on this thread, such as the misses of an optional
    /// cache, and return its value with whether a refusal of memory met it. Inside `f`, a
    /// memory refusal that infallible code meets on this context sets that result and
    /// does not fail the query, so the caller must discard the value of `f` when it is
    /// set and evaluate the same input the ordinary way. From the refusal on,
    /// [`Ctx::has_memory_failure`] is true inside `f` as it is after a latched refusal,
    /// so expressions that catch errors give up instead of evaluating arms that the
    /// ordinary evaluation skips. The ordinary evaluation of that input then meets the
    /// query's budget as it would without the cache, provided the caller has released
    /// its optional state and does not evaluate again an input whose earlier evaluation
    /// finished, which would meet that evaluation's transient memory beside the terms
    /// that later inputs retained, except when the unfinished input itself interned an
    /// intermediate term after an earlier transient allocation and then refused later in
    /// the same expression (roadmap item 1322). That holds only for evaluations that keep no
    /// charge past their value, so callers keep out geometry constructors, whose charge
    /// lasts until the query ends. Every other refusal, and refusals met on other
    /// contexts or other threads, still fail the next check. A scope opened inside a
    /// refused scope of the same context starts refused. `f` must not hand work to the
    /// pool, where another job of the query could run inside the scope.
    pub(crate) fn optional_eval<T>(&self, f: impl FnOnce() -> T) -> (T, bool) {
        struct Restore<'a> {
            prev: Option<(usize, bool)>,
            refused_scopes: &'a AtomicUsize,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                let closed = OPTIONAL_EVAL.with(|scope| scope.replace(self.prev));
                // Only a scope that `note_optional_refusal` turned refused counted
                // itself, and a scope that started refused did not.
                if let Some((me, true)) = closed
                    && self.prev != Some((me, true))
                {
                    self.refused_scopes.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
        let me = std::ptr::from_ref(self) as usize;
        let prev = OPTIONAL_EVAL.with(|scope| {
            let prev = scope.get();
            scope.set(Some((me, prev == Some((me, true)))));
            prev
        });
        let _restore = Restore {
            prev,
            refused_scopes: &self.memory_failure.refused_scopes,
        };
        let value = f();
        let refused = OPTIONAL_EVAL.with(|scope| scope.get().is_some_and(|(_, refused)| refused));
        (value, refused)
    }

    /// Record a memory refusal for the optional evaluation of this context that this
    /// thread runs, if any, and say whether there was one. The first refusal of a scope
    /// counts it in [`MemoryFailure::refused_scopes`] until the scope closes.
    #[cold]
    fn note_optional_refusal(&self) -> bool {
        let me = std::ptr::from_ref(self) as usize;
        OPTIONAL_EVAL.with(|scope| match scope.get() {
            Some((ctx, refused)) if ctx == me => {
                if !refused {
                    self.memory_failure
                        .refused_scopes
                        .fetch_add(1, Ordering::Relaxed);
                    scope.set(Some((ctx, true)));
                }
                true
            }
            _ => false,
        })
    }

    /// The open optional evaluation scopes on this context that a refusal has met.
    #[cfg(test)]
    pub(super) fn refused_scopes_for_test(&self) -> usize {
        self.memory_failure.refused_scopes.load(Ordering::Relaxed)
    }

    /// Build a disposable trial context for optional planning estimation (such as FILTER
    /// selectivity sampling) bounded by the remaining memory of `self`. All decode
    /// envelopes, value caches, local terms, compiled regexes, and parsed geometries
    /// live only on the trial context and are released when it drops.
    fn sampling_trial(&self) -> Result<Arc<Ctx>> {
        self.check()?;
        let mut trial = Ctx::new(self.snap.clone());
        trial.mem_limit = self.memory_remaining();
        trial.set_grant(self.fast.grant.clone());
        if self.has_ledger() {
            trial.set_memory_parent(self.memory_parent()?)?;
            trial.enable_cursor_accounting()?;
        }
        trial.source_preparation = self.source_preparation.clone();
        trial.read_source = self.read_source.clone();
        trial.deadline = self.deadline;
        trial.deadline_pause = self.deadline_pause.clone();
        trial.cancel = self.cancel.clone();
        trial.cancellation = self.cancellation.clone();
        trial.graphs = self.graphs.clone();
        if let Some(now) = self.now.get() {
            let _ = trial.now.set(*now);
        }
        trial.opt = self.opt;
        trial.aggregate_errors = self.aggregate_errors;
        let graph_bytes = [&self.dataset.default, &self.dataset.named]
            .into_iter()
            .flatten()
            .fold(0u64, |bytes, graphs| {
                bytes.saturating_add(
                    (graphs.len() as u64).saturating_mul(std::mem::size_of::<Id>() as u64),
                )
            });
        let base_bytes = self
            .base_iri
            .as_ref()
            .map_or(0, |iri| iri.as_str().len() as u64);
        let fixed_bytes = graph_bytes.saturating_add(base_bytes);
        let mut admitted = 0;
        loop {
            let names = self.var_names.read();
            let local = self.local.read();
            let name_bytes = names.iter().fold(0u64, |bytes, name| {
                bytes.saturating_add(name.len() as u64 + std::mem::size_of::<String>() as u64)
            });
            let local_bytes = local.append_only_clone_bytes();
            let bytes = fixed_bytes
                .saturating_add(name_bytes)
                .saturating_add(local_bytes);
            if bytes <= admitted {
                *trial.var_names.get_mut() = names.clone();
                *trial.local.get_mut() = LocalVocab::Owned(local.clone_owned());
                trial.local_retained.store(local_bytes, Ordering::Release);
                break;
            }
            // Admission may wake a pool entrant. Keep both vocabulary and
            // variable-name locks free, then recheck the size before copying.
            drop(local);
            drop(names);
            if !trial.retain_bytes(bytes - admitted) {
                trial.check()?;
            }
            admitted = bytes;
        }
        trial.dataset = self.dataset.clone();
        trial.base_iri = self.base_iri.clone();
        trial
            .next_bnode
            .store(self.next_bnode.load(Ordering::Relaxed), Ordering::Relaxed);
        Ok(trial.into_shared())
    }

    /// Run an optional planning estimator on a disposable trial context so a declined
    /// sample leaves zero decode-envelope, value-cache, local-term, regex, or geometry
    /// state on `self`. Pre-existing failures, cancellation, and timeout always propagate.
    pub(super) fn with_sampling_trial<T>(
        &self,
        f: impl FnOnce(&Ctx) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        self.check()?;
        let res = self.sampling_trial().and_then(|trial| {
            let result = f(&trial).and_then(|value| {
                trial.check()?;
                Ok(value)
            });
            trial.release_work();
            result
        });
        match res {
            Ok(v) => {
                self.check()?;
                Ok(v)
            }
            Err(Error::BudgetExceeded(b))
                if matches!(b.kind, BudgetKind::Memory | BudgetKind::ServerMemory) =>
            {
                self.check()?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Fallible variant of [`Ctx::retain_bytes`]: charges `bytes` more of query-local
    /// terms and memos without latching [`Ctx::memory_failure`] on refusal, so callers
    /// with a recovery path (such as parallel fallback or `group_or_spill`) can handle
    /// the memory error directly.
    pub(crate) fn try_retain_bytes(&self, bytes: u64) -> Result<()> {
        self.check_memory_failure()?;
        let retained = &self.retained;
        let used = retained.used.fetch_add(bytes, Ordering::AcqRel) + bytes;
        if retained.charge.covers(used) {
            return Ok(());
        }
        match self.grow_reservation(&retained.charge, used, u64::MAX, true) {
            Ok(()) => Ok(()),
            Err(error) => {
                retained.used.fetch_sub(bytes, Ordering::AcqRel);
                Err(error)
            }
        }
    }

    /// Retain `bytes` of query-local state by consuming spare `self.retained` headroom
    /// first and keeping any remainder permanently charged directly out of `source`
    /// without releasing it back to the pool. Never advances `self.retained.charge.reserved`,
    /// preserving the `StepReservation` invariant (`reserved >= used`) against concurrent
    /// `retain_bytes` callers. Any excess above `bytes` stays on `source` to be released
    /// once temporary growth buffers have freed.
    fn retain_from_charge(&self, bytes: u64, source: &Charge<'_>) {
        if bytes == 0 {
            return;
        }
        debug_assert!(source.belongs_to(self));
        debug_assert!(source.bytes() >= bytes);
        let from_spare = {
            let _grow = self.retained.charge.grow.lock();
            let reserved = self.retained.charge.reserved.load(Ordering::Acquire);
            let mut claimed = 0u64;
            if self
                .retained
                .used
                .try_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                    let take = bytes.min(reserved.saturating_sub(cur));
                    if take == 0 {
                        None
                    } else {
                        claimed = take;
                        Some(cur + take)
                    }
                })
                .is_err()
            {
                claimed = 0;
            }
            claimed
        };
        let direct = bytes - from_spare;
        if direct > 0 {
            let remaining = source.bytes.get() - direct;
            source.bytes.set(remaining);
            if remaining == 0 {
                source.disarm();
            }
        }
    }

    /// Charge `bytes` more of query-local terms and memos, which stay until the context
    /// drops, in both accounting modes. Infallible expression interning must not turn a
    /// resource failure into an ordinary unbound BIND, so a refusal is recorded and fails
    /// the next check, or goes to the caller of [`Ctx::optional_eval`], and `false` tells
    /// the caller not to allocate.
    pub(crate) fn retain_bytes(&self, bytes: u64) -> bool {
        match self.try_retain_bytes(bytes) {
            Ok(()) => true,
            Err(error) => {
                self.fail_memory(&error);
                false
            }
        }
    }

    #[inline]
    fn decode_workers() -> u64 {
        if rayon::current_thread_index().is_some() {
            rayon::current_num_threads() as u64 + 1
        } else {
            1
        }
    }

    #[inline]
    fn decode_need(bytes: usize) -> u64 {
        (bytes as u64)
            .saturating_mul(8)
            .saturating_add(1024)
            .saturating_mul(Self::decode_workers())
    }

    /// Lock-free check whether the decode envelope already covers `bytes` with no
    /// latched failure. Used under vocabulary read locks so the hot path never
    /// double-locks and the cold growth/refusal path always runs after unlocking.
    #[inline]
    fn decode_envelope_covers(&self, bytes: usize) -> bool {
        !self.memory_failure.failed.load(Ordering::Acquire)
            && self.decode.reserved.load(Ordering::Acquire) >= Self::decode_need(bytes)
    }

    /// Admit optional vocabulary prefetch scratch. In cursor mode, prefetch reuses the
    /// query's paid decode envelope and admits any extra optional headroom on the ledger
    /// as reclaimable bytes (skipping ledger entry when the additional bytes cannot fit
    /// current local headroom). The returned guard pins those bytes while the scratch
    /// vectors are live; once it drops, any required charge that would fail with
    /// `BudgetExceeded` reclaims the idle optional excess first. In eager mode, prefetch
    /// charges and releases directly on the fast account.
    pub(super) fn admit_prefetch_scratch(
        &self,
        min_bytes: u64,
        eager_bytes: u64,
    ) -> Result<PrefetchScratch<'_>> {
        let Some(ledger) = self.memory.get() else {
            return Ok(PrefetchScratch::Eager(self.charge(min_bytes)?));
        };
        self.check_memory_failure()?;
        let workers = Self::decode_workers();
        let min_need = min_bytes.saturating_mul(workers);
        let eager_need = eager_bytes.saturating_mul(workers);
        ledger.begin_prefetch();
        let covered = ledger.envelope_reserved();
        if covered >= min_need {
            if eager_need > covered {
                let extra = eager_need - covered;
                if extra <= self.memory_remaining() {
                    let _ = ledger.grow_active_prefetch(self.grant(), self.mem_limit, eager_need);
                }
            }
            return Ok(PrefetchScratch::Cursor {
                ctx: self,
                ledger,
                workers,
            });
        }
        ledger.end_prefetch();

        let remaining = self.memory_remaining();
        if eager_need > min_need && eager_need - covered <= remaining {
            if ledger
                .admit_prefetch(self.grant(), self.mem_limit, eager_need)
                .is_ok()
            {
                return Ok(PrefetchScratch::Cursor {
                    ctx: self,
                    ledger,
                    workers,
                });
            }
            let covered = ledger.envelope_reserved();
            if covered < min_need && min_need - covered > self.memory_remaining() {
                return Err(
                    self.memory_exceeded(self.live_bytes().saturating_add(min_need - covered))
                );
            }
        } else if min_need - covered > remaining {
            return Err(self.memory_exceeded(self.live_bytes().saturating_add(min_need - covered)));
        }
        ledger.admit_prefetch(self.grant(), self.mem_limit, min_need)?;
        Ok(PrefetchScratch::Cursor {
            ctx: self,
            ledger,
            workers,
        })
    }

    /// Admit transient decoding of a key of `bytes` bytes for an explicitly fallible
    /// operation. A refused sort trial can release its storage and try a smaller piece,
    /// so it must not poison the context as an infallible scalar evaluation would. The
    /// envelope covers a key of the largest size seen so far on every worker, so the
    /// common path is one atomic load.
    #[inline]
    pub(super) fn reserve_decode_checked(&self, bytes: usize) -> Result<()> {
        self.check_memory_failure()?;
        let need = Self::decode_need(bytes);
        if self.decode.reserved.load(Ordering::Acquire) >= need {
            return Ok(());
        }
        self.grow_decode(need)
    }

    /// Whether a scoped strict-decode guard is active on this context (for example,
    /// constant folding during planning), requiring decodes to check live controls
    /// during vocabulary reconstruction.
    #[inline]
    fn is_strict_decode(&self) -> bool {
        self.strict_decode.load(Ordering::Relaxed) != 0
    }

    /// Enable strict decoding for the lifetime of the returned guard so evaluator term
    /// and value decodes check live controls during vocabulary reconstruction.
    pub(super) fn strict_decode_guard(&self) -> StrictDecodeGuard<'_> {
        self.strict_decode.fetch_add(1, Ordering::AcqRel);
        StrictDecodeGuard { ctx: self }
    }

    #[cold]
    #[inline(never)]
    fn grow_decode(&self, need: u64) -> Result<()> {
        let envelope = &self.decode;
        // A refused charge is dropped after the guard, since even an empty cursor
        // charge can synchronously wake pool waiters.
        let charge = self.charge(0)?;
        #[cfg(test)]
        envelope.notify_grow_hook(DecodeGrowPoint::BeforeLock);
        let ledger_target = {
            let mut state = envelope.state.lock();
            if need <= state.0 {
                drop(state);
                drop(charge);
                return self.check_memory_failure();
            }
            if let Some(ledger) = self.memory.get() {
                let eager_paid: u64 = state.1.iter().map(|&(_, owned)| owned).sum();
                Some((ledger, need.saturating_sub(eager_paid)))
            } else {
                #[cfg(test)]
                envelope.notify_grow_hook(DecodeGrowPoint::BeforeEagerCharge);
                let (peak, owners) = &mut *state;
                let delta = need - *peak;
                charge.add(delta)?;
                if let Some(id) = charge.disarm() {
                    if let Some((_, owned)) = owners.iter_mut().find(|(owner, _)| *owner == id) {
                        *owned += delta;
                    } else {
                        owners.push((id, delta));
                    }
                }
                *peak = need;
                envelope.reserved.store(need, Ordering::Release);
                None
            }
        };
        if let Some((ledger, target)) = ledger_target {
            #[cfg(test)]
            envelope.notify_grow_hook(DecodeGrowPoint::AfterLedgerSnapshot);
            // Reserve decode bytes directly on the ledger's decode envelope while
            // `envelope.state` is unlocked so synchronous pool wakes can re-enter safely
            // and optional cursor prefetch shares the envelope with zero extra ledger ops.
            let account = ledger.owner(self.grant())?;
            ledger.reserve_decode_to(&account, self.mem_limit, target)?;
            let mut state = envelope.state.lock();
            if need > state.0 {
                state.0 = need;
                envelope.reserved.store(need, Ordering::Release);
            }
        }
        drop(charge);
        self.check_memory_failure()
    }

    /// Reserve the same envelope for an infallible scalar caller. Its failure stays
    /// sticky so an expression cannot become unbound, except inside
    /// [`Ctx::optional_eval`], whose caller discards the expression's value.
    #[inline]
    pub(super) fn reserve_decode(&self, bytes: usize) -> bool {
        match self.reserve_decode_checked(bytes) {
            Ok(()) => true,
            Err(error) => {
                self.fail_memory(&error);
                false
            }
        }
    }

    /// `f` of the key of base vocabulary id `id`, with its reconstruction and its
    /// decoding inside the envelope. A refusal records a failure and gives `None`.
    #[inline]
    fn with_vocab_key<T>(&self, id: u64, mut f: impl FnMut(&[u8]) -> T) -> Option<T> {
        let decode = |key: &[u8]| -> Result<Option<T>> {
            id::validate_key_framing(key)?;
            Ok(self.reserve_decode(key.len()).then(|| f(key)))
        };
        if self.is_strict_decode() {
            return match self.snap.generation.vocab.get_with_controlled(
                id,
                |need| self.reserve_decode_checked(need),
                |_| self.check(),
                decode,
            ) {
                Ok(Some(Ok(found))) => found,
                Ok(None) => None,
                Ok(Some(Err(error))) | Err(error) => {
                    self.fail_memory(&error);
                    None
                }
            };
        }
        if self.check_memory_failure().is_err() {
            return None;
        }
        match self.snap.generation.vocab.get_with_checked(
            id,
            |need| self.reserve_decode_checked(need),
            decode,
        ) {
            Ok(Some(Ok(found))) => found,
            Ok(None) => None,
            Ok(Some(Err(error))) | Err(error) => {
                self.fail_memory(&error);
                None
            }
        }
    }

    /// [`Ctx::with_vocab_key`] reusing the caller's scratch buffer across calls.
    #[inline]
    fn with_vocab_key_in<T>(
        &self,
        id: u64,
        key: &mut Vec<u8>,
        mut f: impl FnMut(&[u8]) -> T,
    ) -> Option<T> {
        if self.is_strict_decode() {
            return self.with_vocab_key(id, f);
        }
        if self.check_memory_failure().is_err() {
            return None;
        }
        let decode = |k: &[u8]| -> Result<Option<T>> {
            id::validate_key_framing(k)?;
            Ok(self.reserve_decode(k.len()).then(|| f(k)))
        };
        match self.snap.generation.vocab.get_with_checked_in(
            id,
            key,
            |need| self.reserve_decode_checked(need),
            decode,
        ) {
            Ok(Some(Ok(found))) => found,
            Ok(None) => None,
            Ok(Some(Err(error))) | Err(error) => {
                self.fail_memory(&error);
                None
            }
        }
    }

    /// [`Ctx::with_vocab_key_in`] without recording a sticky memory failure on refusal.
    #[inline]
    fn try_with_vocab_key_in<T>(
        &self,
        id: u64,
        key: &mut Vec<u8>,
        mut f: impl FnMut(&[u8]) -> T,
    ) -> Result<Option<T>> {
        self.check_memory_failure()?;
        if self.is_strict_decode() {
            self.check()?;
            return self
                .snap
                .generation
                .vocab
                .get_with_controlled(
                    id,
                    |need| self.reserve_decode_checked(need),
                    |_| self.check(),
                    |k| {
                        id::validate_key_framing(k)?;
                        Ok(f(k))
                    },
                )
                .and_then(|opt| opt.transpose());
        }
        self.snap
            .generation
            .vocab
            .get_with_checked_in(
                id,
                key,
                |need| self.reserve_decode_checked(need),
                |k| {
                    id::validate_key_framing(k)?;
                    Ok(f(k))
                },
            )
            .and_then(|opt| opt.transpose())
    }

    /// The row limit of intermediate results.
    #[inline]
    pub fn check_rows(&self, n: usize) -> Result<()> {
        if n > self.max_rows {
            return Err(self.row_budget_error(Budget {
                kind: BudgetKind::Rows,
                limit: self.max_rows as u64,
                requested: n as u64,
            }));
        }
        Ok(())
    }

    /// An operator finished with `rows` rows: count them as produced, and fail once the
    /// total passes [`Ctx::max_rows_produced`]. One atomic update per operator, in both
    /// accounting modes. The counter never wraps: a total past `u64::MAX` fails the
    /// query and leaves the counter as it was.
    #[inline]
    pub fn produced(&self, rows: usize) -> Result<()> {
        let rows = rows as u64;
        let prior = self
            .rows_produced
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                n.checked_add(rows)
            })
            .map_err(|_| self.produced_exceeded(u64::MAX))?;
        let total = prior + rows;
        if total > self.max_rows_produced {
            return Err(self.produced_exceeded(total));
        }
        Ok(())
    }

    /// The rows produced by all operators so far.
    pub fn rows_produced(&self) -> u64 {
        self.rows_produced.load(Ordering::Relaxed)
    }

    fn produced_exceeded(&self, requested: u64) -> Error {
        self.row_budget_error(Budget {
            kind: BudgetKind::RowsProduced,
            limit: self.max_rows_produced,
            requested,
        })
    }

    #[cold]
    fn row_budget_error(&self, budget: Budget) -> Error {
        // Row accounting can see a fallback row from a refused expression.
        if matches!(
            self.memory_failure.failure.lock().as_ref(),
            Some(ResourceRefusal::UnconfiguredExtension)
        ) {
            super::extensions::unconfigured()
        } else {
            Error::BudgetExceeded(budget)
        }
    }

    // --------------------------------------------------------------- memory ------

    /// Before an operator produces (or grows its output to) `rows` rows of `width`
    /// columns: the row limit, and the memory budget for the tables held now plus that
    /// output. Runs before the output is allocated where the size is known up front, so
    /// a query over budget fails fast. Records the peak estimate.
    #[inline]
    pub fn check_output(&self, rows: usize, width: usize) -> Result<()> {
        self.check_output_rows(rows)?;
        self.check_output_bytes(rows, width)
    }

    /// The memory half of [`Ctx::check_output`]: admit the bytes of `rows` rows of
    /// `width` columns, such as capacity reserved ahead of the rows, without counting
    /// them against the row limits.
    pub(super) fn check_output_bytes(&self, rows: usize, width: usize) -> Result<()> {
        let bytes = table_bytes(rows, width);
        if let Some(checked) = self.fast_admit(bytes, false) {
            return checked;
        }
        let owner = self.memory().owner(self.grant())?;
        self.memory().check_output(&owner, self.mem_limit, bytes)
    }

    pub(crate) fn check_output_rows(&self, rows: usize) -> Result<()> {
        self.check_rows(rows)?;
        if self.max_rows_produced != u64::MAX {
            let total = self
                .rows_produced
                .load(Ordering::Relaxed)
                .saturating_add(rows as u64);
            if total > self.max_rows_produced {
                return Err(self.produced_exceeded(total));
            }
        }
        Ok(())
    }

    /// Hold `bytes` against the local budget until the guard drops. An eager
    /// context also retains its admitted pool peak until the context drops.
    pub(super) fn output_lease(&self, bytes: u64) -> Result<QueryMemoryLease> {
        let ledger = self.memory();
        let owner = ledger.owner(self.grant())?;
        ledger.reserve(&owner, self.mem_limit, bytes, false)?;
        Ok(QueryMemoryLease {
            ledger: ledger.clone(),
            owner,
            bytes,
            service: None,
        })
    }

    pub(super) fn retain_plan_lease(&self, lease: QueryMemoryLease) {
        self.plan_scratch_leases.lock().push(lease);
    }

    pub fn charge(&self, bytes: u64) -> Result<Charge<'_>> {
        let c = Charge {
            ctx: self,
            bytes: std::cell::Cell::new(0),
            owner: std::cell::Cell::new(None),
        };
        c.add(bytes)?;
        Ok(c)
    }

    /// Lower a finished eager query's retained pool contribution to what its result
    /// keeps: `result` bytes of allocated output columns and its live charges, which
    /// include the state the context keeps, its value cache, terms, memos and
    /// geometries (X08-P1-retained-results). The contribution otherwise stays at the
    /// query's highest admitted estimate, including build and transient state, until the
    /// context drops. A fast account within [`RETAIN_SLACK`] of what the result keeps is
    /// left alone, so that a small query never takes the ledger.
    pub(super) fn shrink_retained(&self, result: u64) -> Result<()> {
        if self.streams {
            return Ok(());
        }
        let fast = match self.memory.get() {
            Some(_) => None,
            None if self.grant().is_some() => Some(self.fast.eager.load(Ordering::Acquire)),
            None => return Ok(()),
        };
        // The output columns hold no lease, so the result keeps them on top of the
        // live charges.
        let keep = result.saturating_add(self.live_bytes());
        // A contribution within the slack of what the result keeps is left alone.
        if fast.is_some_and(|admitted| admitted <= keep.saturating_add(RETAIN_SLACK)) {
            return Ok(());
        }
        self.memory().shrink_eager(result)
    }

    pub fn mem_peak(&self) -> u64 {
        let fast = self.fast.peak.load(Ordering::Relaxed);
        self.memory
            .get()
            .map_or(fast, |ledger| fast.max(ledger.peak.load(Ordering::Relaxed)))
    }

    /// How many rows of `width` columns fit in what is left of the memory budget (for
    /// capping up-front reservations; the output is still checked as it grows).
    pub fn rows_within_budget(&self, width: usize) -> usize {
        if self.mem_limit == u64::MAX {
            return usize::MAX;
        }
        let left = self.mem_limit.saturating_sub(self.live_bytes());
        usize::try_from(left / table_bytes(1, width)).unwrap_or(usize::MAX)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn memory_exceeded(&self, requested: u64) -> Error {
        Error::BudgetExceeded(Budget {
            kind: BudgetKind::Memory,
            limit: self.mem_limit,
            requested,
        })
    }

    // ------------------------------------------------------------ variables ------

    pub fn var(&self, name: &str) -> VarId {
        if let Some(i) = self.var_names.read().iter().position(|n| n == name) {
            return i as VarId;
        }
        let mut w = self.var_names.write();
        if let Some(i) = w.iter().position(|n| n == name) {
            return i as VarId;
        }
        w.push(name.to_string());
        (w.len() - 1) as VarId
    }

    /// Intern a checked planner variable with its query-owned vector and text.
    pub(super) fn var_prepared(&self, name: &str, preparation: &Preparation) -> Result<VarId> {
        let find = |names: &[String]| -> Result<Option<VarId>> {
            for (start, part) in names.chunks(256).enumerate() {
                preparation.check()?;
                if let Some(offset) = part.iter().position(|current| current == name) {
                    return Ok(Some((start * 256 + offset) as VarId));
                }
            }
            Ok(None)
        };
        if let Some(id) = find(&self.var_names.read())? {
            return Ok(id);
        }
        let mut names = self.var_names.write();
        if let Some(id) = find(&names)? {
            return Ok(id);
        }

        let id = VarId::try_from(names.len()).map_err(|_| preparation.exhausted())?;
        let mut text = PreparationText::new(preparation)?;
        text.push_str(name)?;
        let mut paid = self.prepared_var_names.lock();
        if names.len() == names.capacity() {
            let capacity = names
                .capacity()
                .checked_mul(2)
                .map(|capacity| capacity.max(4))
                .ok_or_else(|| preparation.exhausted())?;
            let bytes = capacity
                .checked_mul(size_of::<String>())
                .ok_or_else(|| preparation.exhausted())?;
            let vector_credit = preparation.reserve(bytes as u64)?;
            let mut replacement = Vec::new();
            replacement
                .try_reserve_exact(capacity)
                .map_err(|_| preparation.exhausted())?;
            // Reallocation is committed as a unit so cancellation cannot remove
            // names already in use by another part of the query.
            replacement.append(&mut names);
            let old = std::mem::replace(&mut *names, replacement);
            drop(old);
            paid.vector = Some(vector_credit);
        }
        let (name, text_credit) = text.finish()?;
        names.push(name);
        if let Some(held) = &mut paid.text {
            held.merge(text_credit);
        } else {
            paid.text = Some(text_credit);
        }
        preparation.check()?;
        Ok(id)
    }

    /// A fresh variable that can never clash with user variables.
    pub fn fresh_var(&self) -> VarId {
        let mut w = self.var_names.write();
        let n = w.len();
        w.push(format!(" _{n}"));
        n as VarId
    }

    /// Is `name` a variable of this query (without creating it)?
    pub fn has_var(&self, name: &str) -> bool {
        self.var_names.read().iter().any(|n| n == name)
    }

    pub fn var_name(&self, v: VarId) -> String {
        self.var_names.read()[v as usize].clone()
    }

    pub fn nvars(&self) -> usize {
        self.var_names.read().len()
    }

    // ---------------------------------------------------------------- terms ------

    /// Fallible variant of [`Ctx::intern_term`] that returns memory refusals without
    /// latching [`Ctx::memory_failure`].
    pub(crate) fn try_intern_term(&self, t: &Term) -> Result<Id> {
        if let Term::BlankNode(b) = t {
            return match id::parse_bnode_payload(b.as_str()) {
                Some(p) => Ok(Id::bnode(p)),
                None => self.try_outside_bnode(b.as_str()),
            };
        }
        if let Some(id) = id::inline_id(t) {
            return Ok(id);
        }
        let key = id::term_key(t);
        self.try_intern_key(&key)
    }

    /// The id of a term this query decoded from one of its own ids: stored id if the
    /// term exists in the store, else a local id. A blank node label written by
    /// [`bnode_for`] maps back to its id, a stored node's or one this query minted.
    pub fn intern_term(&self, t: &Term) -> Id {
        match self.try_intern_term(t) {
            Ok(id) => id,
            Err(error) => {
                self.fail_memory(&error);
                Id::UNDEF
            }
        }
    }

    /// A literal written in the query text. A blank node label inside a composite literal
    /// (`cdt:List`, `cdt:Map`) names a blank node of this query, the same one for the
    /// same label in all the query's literals, as Jena has it. It never names a stored
    /// node, also when it looks like a stored node's label. The literal is written again
    /// with the labels of those nodes, so the functions that read it find them.
    pub fn query_literal<'l>(&self, l: &'l oxrdf::Literal) -> std::borrow::Cow<'l, oxrdf::Literal> {
        match super::cdt::relabel_literal(l, &mut |b| {
            let mut memo = self.literal_bnodes.lock();
            let id = match memo.get(b) {
                Some(id) => *id,
                None if self.retain_bytes(b.len() as u64 + 128) => {
                    let id = self.fresh_bnode();
                    memo.insert(b.to_string(), id);
                    id
                }
                None => Id::UNDEF,
            };
            id::bnode_label(id.payload())
        }) {
            Some(l) => std::borrow::Cow::Owned(l),
            None => std::borrow::Cow::Borrowed(l),
        }
    }

    /// [`Ctx::intern_term`] for a literal written in the query text (see
    /// [`Ctx::query_literal`]).
    pub fn intern_query_literal(&self, l: &oxrdf::Literal) -> Id {
        self.intern_term(&Term::Literal(self.query_literal(l).into_owned()))
    }

    /// The id of a term given to the query from outside, such as an initial binding. A
    /// blank node, also inside a triple term, names a stored node only by that node's
    /// label (see [`parse_bnode_label`]). Any other label, a minted node's label from
    /// an earlier result included, is a new blank node of this query, the same one for
    /// the same label.
    pub fn intern_outside_term(&self, t: &Term) -> Id {
        self.intern_term(&self.map_bnodes(t, &mut |b| {
            parse_bnode_label(b).unwrap_or_else(|| self.outside_bnode(b))
        }))
    }

    /// The id of a term from a SERVICE result. Its blank nodes are the remote endpoint's,
    /// so every label is a new blank node of this query, the same one for the same label
    /// within `scope` (one result set).
    pub fn intern_remote_term(&self, t: &Term, scope: &mut FxHashMap<String, Id>) -> Id {
        self.intern_term(&self.map_bnodes(t, &mut |b| {
            if let Some(&id) = scope.get(b) {
                return id;
            }
            if !self.retain_bytes(b.len() as u64 + 128) {
                return Id::UNDEF;
            }
            let id = self.fresh_bnode();
            scope.insert(b.to_string(), id);
            id
        }))
    }

    /// `t` with each blank node, also inside triple terms, replaced by the node `f` picks
    /// for its label.
    fn map_bnodes(&self, t: &Term, f: &mut dyn FnMut(&str) -> Id) -> Term {
        match t {
            Term::BlankNode(b) => Term::BlankNode(bnode_for(f(b.as_str()))),
            Term::Triple(tr) => {
                let s = match self.map_bnodes(&tr.subject.clone().into(), f) {
                    Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                    Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                    _ => unreachable!("a subject stays a subject"),
                };
                let o = self.map_bnodes(&tr.object, f);
                Term::Triple(Box::new(oxrdf::Triple::new(s, tr.predicate.clone(), o)))
            }
            t => t.clone(),
        }
    }

    /// The label of the outside blank node that [`Ctx::intern_outside_term`] mapped to the
    /// query's blank node labelled `label`, if it is one. The memo holds the outside nodes
    /// of the query's arguments, so the scan is short.
    pub fn outside_label(&self, label: &str) -> Option<String> {
        self.outside_bnodes
            .lock()
            .iter()
            .find(|(_, v)| id::bnode_label(v.payload()) == label)
            .map(|(k, _)| k.clone())
    }

    /// Fallible variant of [`Ctx::outside_bnode`].
    fn try_outside_bnode(&self, label: &str) -> Result<Id> {
        let mut memo = self.outside_bnodes.lock();
        if let Some(&id) = memo.get(label) {
            return Ok(id);
        }
        self.try_retain_bytes(label.len() as u64 + 128)?;
        let id = self.fresh_bnode();
        memo.insert(label.to_string(), id);
        Ok(id)
    }

    /// A blank node of this query for a label from outside it.
    fn outside_bnode(&self, label: &str) -> Id {
        match self.try_outside_bnode(label) {
            Ok(id) => id,
            Err(error) => {
                self.fail_memory(&error);
                Id::UNDEF
            }
        }
    }

    /// Id for a graph name, mapping Jena's special default-graph IRI.
    pub fn graph_id(&self, iri: &str) -> Id {
        if iri == DEFAULT_GRAPH_IRI {
            Id::DEFAULT_GRAPH
        } else {
            self.intern_term(&Term::NamedNode(oxrdf::NamedNode::new_unchecked(iri)))
        }
    }

    /// Whether `id` is a constant no fact of the snapshot holds: a query-local term,
    /// unless query-time reasoning derived facts with such terms.
    pub fn unknown_const(&self, id: Id) -> bool {
        id.tag() == Tag::Local && !self.local_facts
    }

    /// Fallible variant of [`Ctx::intern_key`] that returns memory refusals without
    /// latching [`Ctx::memory_failure`].
    pub(crate) fn try_intern_key(&self, key: &[u8]) -> Result<Id> {
        if let Some(id) = self.snap.lookup_key(key) {
            return Ok(id);
        }
        if let Some(i) = self.local.read().find(key) {
            return Ok(Id::local(i));
        }
        let mut local = self.local.write();
        if let Some(i) = local.find(key) {
            return Ok(Id::local(i));
        }
        let add = key.len() as u64 + 96;
        self.try_retain_bytes(add)?;
        let id = Id::local(local.insert(key).0);
        let covered = self.local_retained.load(Ordering::Acquire);
        self.local_retained
            .store(covered.saturating_add(add), Ordering::Release);
        Ok(id)
    }

    pub fn intern_key(&self, key: &[u8]) -> Id {
        match self.try_intern_key(key) {
            Ok(id) => id,
            Err(error) => {
                self.fail_memory(&error);
                Id::UNDEF
            }
        }
    }

    /// Look up or intern an already-encoded vocabulary key, pre-admitting decompression
    /// scratch and local-vocabulary growth peaks (simultaneous old and new hash tables or
    /// key-pointer buffers during resize) and reconciling against retained physical
    /// capacities afterward.
    pub(crate) fn intern_key_checked(&self, key: &[u8]) -> Result<Id> {
        self.check()?;
        let vocab_scratch = self.charge(0)?;
        if let Ok(i) = self.snap.generation.vocab.find_with_checked(key, |need| {
            let bytes = u64::try_from(need)
                .ok()
                .and_then(|n| n.checked_mul(2))
                .ok_or_else(|| self.memory_exceeded(u64::MAX))?;
            vocab_scratch.resize(bytes)
        })? {
            return Ok(Id::vocab(i));
        }
        drop(vocab_scratch);
        if let Some(i) = self
            .snap
            .generation
            .dvocab
            .find(key)
            .filter(|&i| i < self.snap.dvocab_len)
        {
            return Ok(Id::delta(i));
        }
        if let Some(i) = self.local.read().find(key) {
            return Ok(Id::local(i));
        }
        let mut local = self.local.write();
        if let Some(i) = local.find(key) {
            return Ok(Id::local(i));
        }
        let covered = self.local_retained.load(Ordering::Acquire);
        let peak = local
            .insert_peak_bytes(key.len())
            .ok_or_else(|| self.memory_exceeded(u64::MAX))?;
        let transient = peak.saturating_sub(covered);
        let growth_charge = if transient > 0 {
            Some(self.charge(transient)?)
        } else {
            None
        };
        let (id, _) = local.insert(key);
        let after = local.physical_bytes();
        let retain_delta = after.saturating_sub(covered);
        if retain_delta > 0 {
            let from_source = growth_charge
                .as_ref()
                .map_or(0, |charge| charge.bytes().min(retain_delta));
            if from_source > 0 {
                self.retain_from_charge(
                    from_source,
                    growth_charge
                        .as_ref()
                        .expect("pre-admitted growth charge for retained transfer"),
                );
            }
            let rem = retain_delta - from_source;
            debug_assert_eq!(
                rem, 0,
                "insert_peak_bytes must cover post-insert physical_bytes"
            );
            if rem > 0 {
                drop(growth_charge);
                if !self.retain_bytes(rem) {
                    self.local_retained
                        .store(covered.saturating_add(from_source), Ordering::Release);
                    return Err(self.check_memory_failure().err().unwrap_or_else(|| {
                        self.memory_exceeded(self.live_bytes().saturating_add(rem))
                    }));
                }
                self.local_retained
                    .store(covered.saturating_add(retain_delta), Ordering::Release);
                return Ok(Id::local(id));
            }
            self.local_retained
                .store(covered.saturating_add(retain_delta), Ordering::Release);
        }
        drop(growth_charge);
        Ok(Id::local(id))
    }

    /// Intern a simple `xsd:string` literal directly from a borrowed lexical value,
    /// pre-admitting the temporary key buffer before encoding so no intermediate
    /// `Arc<str>` or `oxrdf::Literal` is allocated outside the query budget.
    pub(crate) fn intern_simple_str_checked(&self, s: &str) -> Result<Id> {
        let key_len = s
            .len()
            .checked_add(2)
            .ok_or_else(|| self.memory_exceeded(u64::MAX))?;
        let _scratch = self.charge(key_len as u64)?;
        let mut key = Vec::with_capacity(key_len);
        id::write_literal_ref_key(oxrdf::LiteralRef::new_simple_literal(s), &mut key);
        debug_assert_eq!(key.len(), key_len);
        self.intern_key_checked(&key)
    }

    /// Intern an IRI directly from a borrowed string, pre-admitting the temporary key
    /// buffer before encoding.
    pub(crate) fn intern_iri_checked(&self, iri: &str) -> Result<Id> {
        let key_len = iri
            .len()
            .checked_add(1)
            .ok_or_else(|| self.memory_exceeded(u64::MAX))?;
        let _scratch = self.charge(key_len as u64)?;
        let mut key = Vec::with_capacity(key_len);
        key.push(b'<');
        key.extend_from_slice(iri.as_bytes());
        self.intern_key_checked(&key)
    }

    /// SERVICE-created encoded terms retain the existing key-byte estimate in eager
    /// and cursor execution. This is not an allocator/RSS bound for AppendVocab.
    pub(super) fn intern_algorithm_key(
        &self,
        key: &[u8],
        ctl: &crate::algo::Control,
    ) -> Result<Id> {
        let mut scratch = crate::algo::memory::Reservation::new(ctl, 0)?;
        if let Ok(i) = self.snap.generation.vocab.find_with_checked(key, |need| {
            scratch.grow_to(
                crate::algo::memory::bytes::<u8>(need)?
                    .checked_mul(2)
                    .ok_or_else(|| self.memory_exceeded(u64::MAX))?,
            )
        })? {
            return Ok(Id::vocab(i));
        }
        if let Some(i) = self
            .snap
            .generation
            .dvocab
            .find(key)
            .filter(|&i| i < self.snap.dvocab_len)
        {
            return Ok(Id::delta(i));
        }
        if let Some(i) = self.local.read().find(key) {
            return Ok(Id::local(i));
        }
        let estimate = u64::try_from(key.len())
            .ok()
            .and_then(|n| n.checked_add(96))
            .ok_or_else(|| self.memory_exceeded(u64::MAX))?;
        let charge = self.charge(estimate)?;
        self.check()?;
        let mut local = self.local.write();
        if let Some(i) = local.find(key) {
            return Ok(Id::local(i));
        }
        let id = Id::local(local.insert(key).0);
        std::mem::forget(charge);
        let covered = self.local_retained.load(Ordering::Acquire);
        self.local_retained
            .store(covered.saturating_add(estimate), Ordering::Release);
        Ok(id)
    }

    /// Fallible variant of [`Ctx::intern_value`] that returns memory refusals without
    /// latching [`Ctx::memory_failure`].
    pub(crate) fn try_intern_value(&self, v: &Value) -> Result<Id> {
        match v {
            Value::Bool(b) => Ok(Id::from_bool(*b)),
            Value::Integer(i) => match Id::from_i64(i64::from(*i)) {
                Some(id) => Ok(id),
                None => self.try_intern_term(&v.to_term()),
            },
            // The canonical form of a decimal value parses back to the same value, so
            // its inline id needs neither the text nor the parse of `intern_term`.
            Value::Decimal(d) => match id::pack_decimal(*d) {
                Some(p) => Ok(Id::new(Tag::Decimal, p)),
                None => self.try_intern_term(&v.to_term()),
            },
            _ => self.try_intern_term(&v.to_term()),
        }
    }

    pub fn intern_value(&self, v: &Value) -> Id {
        match self.try_intern_value(v) {
            Ok(id) => id,
            Err(error) => {
                self.fail_memory(&error);
                Id::UNDEF
            }
        }
    }

    /// Take the local terms and minted blank nodes of `other`, so that this context reads
    /// the ids `other` gave them (a query that reads a view with derived facts).
    pub(crate) fn share_local(&self, other: &Ctx) {
        let local = other.local.read().clone_owned();
        // The copy is this context's own, so it is charged here, and a refusal fails the
        // next check.
        let bytes = local.bytes() as u64;
        self.retain_bytes(bytes);
        let mut dst = self.local.write();
        *dst = LocalVocab::Owned(local);
        self.local_retained.store(bytes, Ordering::Release);
        self.next_bnode
            .store(other.next_bnode.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    pub fn fresh_bnode(&self) -> Id {
        Id::bnode(Id::LOCAL_BNODE_BIT | self.next_bnode.fetch_add(1, Ordering::Relaxed))
    }

    /// `BNODE(str)`: one blank node per (solution, string).
    pub fn bnode_for_row(&self, row: Vec<Id>, s: &str) -> Id {
        let mut memo = self.bnode_memo.lock();
        let key = (row, s.to_string());
        if let Some(id) = memo.get(&key) {
            return *id;
        }
        if !self.retain_bytes(key.0.len() as u64 * 8 + s.len() as u64 + 128) {
            return Id::UNDEF;
        }
        let id = self.fresh_bnode();
        memo.insert(key, id);
        id
    }

    /// A term for plan text, which no answer depends on. It is decoded without the
    /// decode envelope in both accounting modes, as eager decoding always was, so that
    /// describing a plan, such as a constant the planner substituted for a variable,
    /// never fails the query it describes.
    pub(super) fn plan_term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map(id::key_to_term),
            Tag::BNode => Some(Term::BlankNode(bnode_for(id))),
            _ => self.snap.term(id),
        }
    }

    fn decode_local_infallible<T>(
        &self,
        payload: u64,
        mut decode: impl FnMut(&[u8]) -> T,
    ) -> Option<T> {
        if self.is_strict_decode()
            && let Err(error) = self.check()
        {
            self.fail_memory(&error);
            return None;
        }
        enum Fast<T> {
            Done(T),
            NeedGrow(usize),
            Corrupt(Error),
        }
        let fast = {
            let local = self.local.read();
            let key = local.get(payload)?;
            if self.decode_envelope_covers(key.len()) {
                match id::validate_key_framing(key) {
                    Ok(()) => Fast::Done(decode(key)),
                    Err(error) => Fast::Corrupt(error),
                }
            } else {
                Fast::NeedGrow(key.len())
            }
        };
        match fast {
            Fast::Done(val) => Some(val),
            Fast::Corrupt(error) => {
                self.fail_memory(&error);
                None
            }
            Fast::NeedGrow(len) => {
                if !self.reserve_decode(len) {
                    return None;
                }
                let res = {
                    let local = self.local.read();
                    let key = local.get(payload)?;
                    id::validate_key_framing(key).map(|()| decode(key))
                };
                match res {
                    Ok(val) => Some(val),
                    Err(error) => {
                        self.fail_memory(&error);
                        None
                    }
                }
            }
        }
    }

    fn decode_delta_infallible<T>(
        &self,
        payload: u64,
        mut decode: impl FnMut(&[u8]) -> T,
    ) -> Option<T> {
        if payload >= self.snap.dvocab_len {
            return None;
        }
        if self.is_strict_decode()
            && let Err(error) = self.check()
        {
            self.fail_memory(&error);
            return None;
        }
        enum Fast<T> {
            Done(T),
            NeedGrow(usize),
            Corrupt(Error),
        }
        let fast = self.snap.generation.dvocab.with(|vocab| {
            let key = vocab.get(payload)?;
            Some(if self.decode_envelope_covers(key.len()) {
                match id::validate_key_framing(key) {
                    Ok(()) => Fast::Done(decode(key)),
                    Err(error) => Fast::Corrupt(error),
                }
            } else {
                Fast::NeedGrow(key.len())
            })
        })?;
        match fast {
            Fast::Done(val) => Some(val),
            Fast::Corrupt(error) => {
                self.fail_memory(&error);
                None
            }
            Fast::NeedGrow(len) => {
                if !self.reserve_decode(len) {
                    return None;
                }
                let res = self.snap.generation.dvocab.with(|vocab| {
                    let key = vocab.get(payload)?;
                    Some(id::validate_key_framing(key).map(|()| decode(key)))
                })?;
                match res {
                    Ok(val) => Some(val),
                    Err(error) => {
                        self.fail_memory(&error);
                        None
                    }
                }
            }
        }
    }

    pub fn term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Local => self.decode_local_infallible(id.payload(), copied_output_key),
            Tag::Vocab => self.with_vocab_key(id.payload(), copied_output_key),
            Tag::Delta => self.decode_delta_infallible(id.payload(), copied_output_key),
            Tag::BNode => Some(Term::BlankNode(bnode_for(id))),
            _ => self.snap.term(id),
        }
    }

    #[inline]
    fn checked_decode_bytes(len: usize) -> u64 {
        (len as u64).saturating_mul(8).saturating_add(128)
    }

    /// Decode an id into an RDF term, checking live request controls and charging the
    /// owned decoded bytes onto `credit` so output credit stays alive with its owner.
    /// Vocabulary reconstruction scratch is charged separately while the key is rebuilt.
    pub(crate) fn term_checked_into(&self, id: Id, credit: &Charge<'_>) -> Result<Option<Term>> {
        assert!(
            credit.belongs_to(self),
            "decode credit belongs to its query"
        );
        self.check()?;
        match id.tag() {
            Tag::Undef | Tag::Special => Ok(None),
            Tag::Local => {
                let len = self
                    .local
                    .read()
                    .get(id.payload())
                    .map(|key| id::validate_key_framing(key).map(|()| key.len()))
                    .transpose()?;
                let Some(len) = len else {
                    return Ok(None);
                };
                credit.add(Self::checked_decode_bytes(len))?;
                self.check()?;
                Ok(self.local.read().get(id.payload()).map(copied_output_key))
            }
            Tag::Vocab => self.with_controlled_vocab_key(id.payload(), |key| {
                id::validate_key_framing(key)?;
                credit.add(Self::checked_decode_bytes(key.len()))?;
                self.check()?;
                Ok(copied_output_key(key))
            }),
            Tag::Delta => {
                if id.payload() >= self.snap.dvocab_len {
                    return Ok(None);
                }
                let len = self
                    .snap
                    .generation
                    .dvocab
                    .with(|vocab| {
                        vocab
                            .get(id.payload())
                            .map(|key| id::validate_key_framing(key).map(|()| key.len()))
                    })
                    .transpose()?;
                let Some(len) = len else {
                    return Ok(None);
                };
                credit.add(Self::checked_decode_bytes(len))?;
                self.check()?;
                Ok(self
                    .snap
                    .generation
                    .dvocab
                    .with(|vocab| vocab.get(id.payload()).map(copied_output_key)))
            }
            Tag::BNode => {
                credit.add(Self::checked_decode_bytes(0))?;
                Ok(Some(Term::BlankNode(bnode_for(id))))
            }
            _ => {
                let term = self.snap.term(id);
                if term.is_some() {
                    credit.add(Self::checked_decode_bytes(0))?;
                }
                Ok(term)
            }
        }
    }

    /// Decode one transient RDF term under live request controls and the query's memory
    /// budget, propagating any memory or control refusal with its original error instead
    /// of returning `None`. Callers that retain or decode multiple terms together should
    /// pass a live [`Charge`] to [`Ctx::term_checked_into`].
    pub fn term_checked(&self, id: Id) -> Result<Option<Term>> {
        let decode = self.charge(0)?;
        self.term_checked_into(id, &decode)
    }

    /// Decode an id into a value, checking live request controls and charging the owned
    /// decoded bytes onto `credit` so output credit stays alive with its owner.
    /// Vocabulary reconstruction scratch and optional value-cache admission stay separate.
    pub(crate) fn value_checked_into(&self, id: Id, credit: &Charge<'_>) -> Result<Option<Value>> {
        assert!(
            credit.belongs_to(self),
            "decode credit belongs to its query"
        );
        self.check()?;
        match id.tag() {
            Tag::Undef | Tag::Special => Ok(None),
            Tag::Int => Ok(Some(Value::Integer(id.as_i64().into()))),
            Tag::Bool => Ok(Some(Value::Bool(id.as_bool()))),
            Tag::Double => Ok(Some(Value::Double(id.as_f64().into()))),
            Tag::Decimal => Ok(Some(Value::Decimal(id::unpack_decimal(id.payload())))),
            Tag::DateTime | Tag::Date => {
                Ok(id::inline_to_literal(id).map(|l| Value::from_literal(&l)))
            }
            Tag::BNode => {
                credit.add(Self::checked_decode_bytes(0))?;
                Ok(Some(Value::BNode(bnode_for(id).as_str().into())))
            }
            _ => {
                if id.tag() == Tag::Vocab
                    && let Some(v) = self.column_value(id.payload())
                {
                    return Ok(Some(v));
                }
                if let Some(v) = self.values.get(id) {
                    return Ok(Some(v));
                }
                let v = match id.tag() {
                    Tag::Vocab => self.with_controlled_vocab_key(id.payload(), |key| {
                        id::validate_key_framing(key)?;
                        credit.add(Self::checked_decode_bytes(key.len()))?;
                        self.check()?;
                        Ok(decode_value_key(key))
                    })?,
                    Tag::Delta => {
                        if id.payload() >= self.snap.dvocab_len {
                            return Ok(None);
                        }
                        let len = self
                            .snap
                            .generation
                            .dvocab
                            .with(|v| {
                                v.get(id.payload())
                                    .map(|key| id::validate_key_framing(key).map(|()| key.len()))
                            })
                            .transpose()?;
                        match len {
                            Some(len) => {
                                credit.add(Self::checked_decode_bytes(len))?;
                                self.check()?;
                                self.snap
                                    .generation
                                    .dvocab
                                    .with(|v| v.get(id.payload()).map(decode_value_key))
                            }
                            None => None,
                        }
                    }
                    Tag::Local => {
                        let len = self
                            .local
                            .read()
                            .get(id.payload())
                            .map(|key| id::validate_key_framing(key).map(|()| key.len()))
                            .transpose()?;
                        match len {
                            Some(len) => {
                                credit.add(Self::checked_decode_bytes(len))?;
                                self.check()?;
                                self.local.read().get(id.payload()).map(decode_value_key)
                            }
                            None => None,
                        }
                    }
                    _ => unreachable!("dictionary value tag"),
                };
                if let Some(v) = v {
                    self.cache_value(id, &v);
                    Ok(Some(v))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Decode one transient value under live request controls and the query's memory
    /// budget, propagating any memory or control refusal with its original error instead
    /// of returning `None`. Callers that retain or decode multiple values together should
    /// pass a live [`Charge`] to [`Ctx::value_checked_into`].
    pub(crate) fn value_checked(&self, id: Id) -> Result<Option<Value>> {
        let decode = self.charge(0)?;
        self.value_checked_into(id, &decode)
    }

    /// Decode one retained output cell after admitting its owned payload. Unlike the
    /// public compatibility decoder, this route cannot turn a refusal into UNDEF.
    /// The charge outlives every returned term; no owning value escapes it. Retained
    /// pages and single terms use this route, so a simple literal takes over its
    /// reconstructed vocabulary key's allocation. The streaming writer reuses one
    /// scratch row through [`Ctx::output_term_into`] instead.
    pub(super) fn output_term(&self, id: Id, row: &dyn TermCredit) -> Result<Option<Term>> {
        assert!(
            std::ptr::eq(self, row.ctx()),
            "output credit belongs to its query"
        );
        self.check_memory_failure()?;
        let term = match id.tag() {
            Tag::Undef => None,
            Tag::Local => {
                // Keys are append-only. Read the estimate, leave the vocabulary lock,
                // then admit before taking the lock again to allocate the term.
                let bytes = self
                    .local
                    .read()
                    .get(id.payload())
                    .map(|key| output_key_bytes(self, key))
                    .transpose()?;
                match bytes {
                    None => None,
                    Some(bytes) => {
                        row.add(bytes)?;
                        self.local.read().get(id.payload()).map(copied_output_key)
                    }
                }
            }
            Tag::Delta => {
                let bytes = self
                    .snap
                    .generation
                    .dvocab
                    .with(|vocab| {
                        vocab
                            .get(id.payload())
                            .map(|key| output_key_bytes(self, key))
                    })
                    .transpose()?;
                match bytes {
                    None => None,
                    Some(bytes) => {
                        row.add(bytes)?;
                        self.snap
                            .generation
                            .dvocab
                            .with(|vocab| vocab.get(id.payload()).map(copied_output_key))
                    }
                }
            }
            Tag::Vocab => self.output_vocab_term(id.payload(), row)?,
            // Inline numbers/calendar values and blank-node labels have bounded
            // formatted text. This covers their strings and construction overlap.
            _ => {
                row.add(256)?;
                self.snap.term(id)
            }
        };
        self.check_memory_failure()?;
        Ok(term)
    }

    /// Decode one streaming writer cell into the writer's reused scratch, for tests.
    #[cfg(test)]
    pub(super) fn output_scratch_term(
        &self,
        id: Id,
        row: &RowCredit<'_>,
        keys: &RowCredit<'_>,
        key: &mut Vec<u8>,
    ) -> Result<Option<Term>> {
        let mut out = Vec::with_capacity(1);
        self.output_term_into(id, row, keys, key, &mut out)?;
        Ok(out.pop().flatten())
    }

    /// Decode one streaming writer cell after admitting its owned payload, pushing it
    /// onto `out`. Unlike the public compatibility decoder, this route cannot turn a
    /// refusal into UNDEF. The row's credit outlives every pushed term, and the
    /// writer drops the row's terms before it decodes the next row. `key` is the
    /// writer's reused reconstruction buffer, and `keys` admits its growth before it
    /// is allocated and holds that credit until the buffer is freed. A decoded term
    /// is built in place in the row rather than returned through the vocabulary's
    /// callback and its nested results, whose copies of each term cost a large
    /// answer about a tenth of its writing time.
    ///
    /// No term escapes the scratch row, so this route copies each key out of the
    /// reused buffer instead of moving the key's allocation into the term as
    /// [`Ctx::output_term`] does. Both physical allocations stay admitted: the
    /// buffer by `keys` and the copy by `row`.
    #[inline]
    pub(super) fn output_term_into(
        &self,
        id: Id,
        row: &RowCredit<'_>,
        keys: &RowCredit<'_>,
        key: &mut Vec<u8>,
        out: &mut Vec<Option<Term>>,
    ) -> Result<()> {
        assert!(
            std::ptr::eq(self, row.charge.ctx) && std::ptr::eq(self, keys.charge.ctx),
            "output credit belongs to its query"
        );
        match id.tag() {
            Tag::Undef => out.push(None),
            Tag::Local => {
                // Keys are append-only. Read the estimate, leave the vocabulary lock,
                // then admit before taking the lock again to allocate the term.
                let bytes = self
                    .local
                    .read()
                    .get(id.payload())
                    .map(|key| output_key_bytes(self, key))
                    .transpose()?;
                if let Some(bytes) = bytes {
                    row.add(bytes)?;
                }
                out.push(
                    bytes.and_then(|_| self.local.read().get(id.payload()).map(copied_output_key)),
                );
            }
            Tag::Delta => {
                let bytes = self
                    .snap
                    .generation
                    .dvocab
                    .with(|vocab| {
                        vocab
                            .get(id.payload())
                            .map(|key| output_key_bytes(self, key))
                    })
                    .transpose()?;
                if let Some(bytes) = bytes {
                    row.add(bytes)?;
                }
                out.push(bytes.and_then(|_| {
                    self.snap
                        .generation
                        .dvocab
                        .with(|vocab| vocab.get(id.payload()).map(copied_output_key))
                }));
            }
            // Reconstruction scratch is query-owned even when the vocabulary is mapped.
            // Twice the requested size covers old and new buffers during reallocation.
            // The writer reuses one buffer for every cell, and its high-water credit
            // admits growth without an account update per cell. A fresh buffer per
            // cell reallocated while rebuilding nearly every key and made large
            // answers about a third slower to write.
            Tag::Vocab => {
                let found = self.snap.generation.vocab.get_with_checked_in(
                    id.payload(),
                    key,
                    |need| keys.reserve((need as u64).saturating_mul(2)),
                    |key| -> Result<()> {
                        out.push(Some(output_key_term(self, key, row)?));
                        Ok(())
                    },
                )?;
                match found {
                    Some(pushed) => pushed?,
                    None => out.push(None),
                }
            }
            // Inline numbers/calendar values and blank-node labels have bounded
            // formatted text. This covers their strings and construction overlap.
            _ => {
                row.add(256)?;
                out.push(self.snap.term(id));
            }
        }
        Ok(())
    }

    /// A vocabulary key and its final simple literal share one physical allocation.
    /// Every capacity change stays on the actual output account, including realloc
    /// overlap. Other key kinds keep the admitted copy path while scratch is alive.
    fn output_vocab_term(&self, id: u64, row: &dyn TermCredit) -> Result<Option<Term>> {
        let base = row.bytes();
        let total = |old: usize, new: usize| {
            base.checked_add(old as u64)
                .and_then(|n| n.checked_add(new as u64))
                .ok_or_else(|| self.memory_exceeded(u64::MAX))
        };
        self.snap.generation.vocab.get_owned_with_controlled(
            id,
            |old, new| row.resize(total(old, new)?),
            |capacity| row.resize(total(0, capacity)?),
            |_| self.check(),
            |key| self.output_owned_key(key, row),
        )
    }

    fn output_owned_key(&self, mut key: Vec<u8>, row: &dyn TermCredit) -> Result<Term> {
        self.check()?;
        let capacity = key.capacity() as u64;
        #[cfg(test)]
        {
            self.chunk_event("owned-key-pointer", key.as_ptr() as usize);
            self.chunk_event("owned-key-capacity", key.capacity());
        }
        self.check()?;
        let term = if key.first() == Some(&b'"')
            && key.last() == Some(&id::KEY_SEP)
            && std::str::from_utf8(&key[1..key.len() - 1]).is_ok()
        {
            #[cfg(test)]
            {
                use crate::algo::memory::probe;
                probe::at(probe::Point::Allocation("chunk-term", key.len()));
                if probe::fails("chunk-term") {
                    return Err(Error::invalid("injected chunk term allocation failure"));
                }
            }
            // Move lexical bytes over the marker without allocating or shrinking
            // capacity. Both markers' capacity stays paid until the literal drops.
            let end = key.len() - 1;
            key.copy_within(1..end, 0);
            key.truncate(end - 1);
            let value = String::from_utf8(key)
                .map_err(|_| Error::Corrupt("invalid simple literal key".into()))?;
            Term::Literal(oxrdf::Literal::new_simple_literal(value))
        } else {
            let term = self.output_key(&key, row)?;
            drop(key);
            row.resize(
                row.bytes()
                    .checked_sub(capacity)
                    .ok_or_else(|| Error::invalid("missing output vocabulary scratch credit"))?,
            )?;
            term
        };
        if let Err(error) = self.check() {
            drop(term);
            return Err(error);
        }
        Ok(term)
    }

    #[cfg(test)]
    pub(super) fn chunk_event(&self, stage: &'static str, count: usize) {
        let hook = self.chunk_hook.lock().clone();
        if let Some(hook) = hook {
            hook(stage, count);
        }
    }

    /// The sorted bulk decoder already borrows a reconstructed key. Admit its
    /// actual owned representation before copying, under the same real context.
    pub(super) fn output_key(&self, key: &[u8], storage: &dyn TermCredit) -> Result<Term> {
        if !std::ptr::eq(self, storage.ctx()) {
            return Err(Error::invalid(
                "bulk output belongs to another query account",
            ));
        }
        let decode = || {
            self.check()?;
            storage.add(output_key_bytes(self, key)?)?;
            #[cfg(test)]
            {
                use crate::algo::memory::probe;
                probe::at(probe::Point::Allocation("chunk-term", key.len()));
                if probe::fails("chunk-term") {
                    return Err(Error::invalid("injected chunk term allocation failure"));
                }
            }
            self.check()?;
            let term = id::key_to_term(key);
            #[cfg(test)]
            self.chunk_event("term-copied", key.len());
            if let Err(error) = self.check() {
                drop(term);
                return Err(error);
            }
            Ok(term)
        };
        if key.first() == Some(&b'(') {
            super::depth::with_stack(crate::nesting::MAX_TRIPLE_TERMS, decode)
        } else {
            decode()
        }
    }

    /// Reconstruction scratch is query-owned even when the vocabulary is mapped.
    /// Twice the requested size covers old and new buffers during reallocation.
    pub(super) fn with_cursor_vocab_key<T>(
        &self,
        id: u64,
        f: impl FnMut(&[u8]) -> T,
    ) -> Result<Option<T>> {
        let scratch = self.charge(0)?;
        let mut reserved = 0;
        self.snap.generation.vocab.get_with_checked(
            id,
            |need| {
                let bytes = (need as u64).saturating_mul(2);
                scratch.add(bytes.saturating_sub(reserved))?;
                reserved = bytes;
                Ok(())
            },
            f,
        )
    }

    /// Reconstruct one base vocabulary key with query-owned scratch credit and live
    /// request-control checkpoints. Twice the requested size covers old and new buffers
    /// during reallocation.
    pub(super) fn with_controlled_vocab_key<T>(
        &self,
        id: u64,
        mut f: impl FnMut(&[u8]) -> Result<T>,
    ) -> Result<Option<T>> {
        self.check()?;
        let scratch = self.charge(0)?;
        let mut reserved = 0;
        self.snap
            .generation
            .vocab
            .get_with_controlled(
                id,
                |need| {
                    let bytes = (need as u64).saturating_mul(2);
                    scratch.add(bytes.saturating_sub(reserved))?;
                    reserved = bytes;
                    Ok(())
                },
                |_| self.check(),
                &mut f,
            )?
            .transpose()
    }

    /// Inspect the borrowed vocabulary key of `id` across base, delta, and query-local
    /// vocabularies with live request controls and query-owned scratch admission.
    /// `f` must only inspect the borrowed bytes and must not call `ctx.charge()` or
    /// `ctx.check()`, because delta and local closures run under vocabulary locks.
    pub(crate) fn with_controlled_key<T>(
        &self,
        id: Id,
        mut f: impl FnMut(&[u8]) -> Result<T>,
    ) -> Result<Option<T>> {
        self.check()?;
        match id.tag() {
            Tag::Vocab => self.with_controlled_vocab_key(id.payload(), f),
            Tag::Delta => self.snap.generation.dvocab.with(|v| {
                if id.payload() >= self.snap.dvocab_len {
                    return Ok(None);
                }
                v.get(id.payload()).map(&mut f).transpose()
            }),
            Tag::Local => self.with_local_key(id.payload(), &mut f).transpose(),
            _ => Ok(None),
        }
    }

    /// Algorithm-owned key reconstruction never populates the eager value cache.
    pub(super) fn with_algo_key<T>(
        &self,
        id: Id,
        ctl: &crate::algo::Control,
        mut f: impl FnMut(&[u8]) -> Result<T>,
    ) -> Result<Option<T>> {
        use crate::algo::memory::{Reservation, bytes};
        ctl.check()?;
        match id.tag() {
            Tag::Vocab => {
                let mut scratch = Reservation::new(ctl, 0)?;
                self.snap
                    .generation
                    .vocab
                    .get_with_fallible(
                        id.payload(),
                        |need| {
                            let bound = bytes::<u8>(need)?
                                .checked_mul(2)
                                .ok_or_else(|| Error::invalid("algorithm key scratch overflows"))?;
                            scratch.grow_to(bound)
                        },
                        &mut f,
                    )?
                    .transpose()
            }
            Tag::Delta => self.snap.generation.dvocab.with(|v| {
                if id.payload() >= self.snap.dvocab_len {
                    return Ok(None);
                }
                v.get(id.payload()).map(&mut f).transpose()
            }),
            Tag::Local => self.with_local_key(id.payload(), &mut f).transpose(),
            Tag::BNode => {
                let mut key = [b'_'; 9];
                key[1..].copy_from_slice(&id.payload().to_be_bytes());
                f(&key).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn with_local_key<T>(&self, id: u64, f: impl FnOnce(&[u8]) -> T) -> Option<T> {
        self.local.read().get(id).map(f)
    }

    /// The term of an id as eager execution decodes it, without charging the scratch of
    /// a vocabulary block or the term, for a caller that drops the term at once.
    pub(super) fn transient_batch_term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map(id::key_to_term),
            Tag::Undef | Tag::Special => None,
            _ => self.snap.term(id),
        }
    }

    /// The term of an id as a cursor batch resolves it, for a caller that has already
    /// reserved [`Ctx::decoded_bytes`] for the term, as the result writers do for a
    /// row's cells. Batch resolution is independent of a producer's sticky failure, so
    /// previously returned ids remain valid.
    /// It takes no reservation of its own, which cost an inline number one pass through
    /// the memory ledger per cell.
    pub(super) fn reserved_batch_term(&self, id: Id) -> Result<Option<Term>> {
        let decode = |key: &[u8]| Some(id::key_to_term(key));
        Ok(match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).and_then(decode),
            Tag::Vocab => self.with_cursor_vocab_key(id.payload(), decode)?.flatten(),
            Tag::Delta => self
                .snap
                .generation
                .dvocab
                .with(|v| v.get(id.payload()).and_then(decode)),
            Tag::Undef | Tag::Special => None,
            _ => self.snap.term(id),
        })
    }

    /// The term of an id as a cursor batch resolves it. It asks `cover` for the
    /// [`Ctx::decoded_bytes`] of the term before it builds it, so that a result writer can hold one reservation for its row
    /// and decode each key once.
    pub(super) fn covered_batch_term(
        &self,
        id: Id,
        mut cover: impl FnMut(u64) -> Result<()>,
    ) -> Result<Option<Term>> {
        let mut decode = |key: &[u8]| -> Result<Option<Term>> {
            cover(self.batch_key_decode_bytes(key)?)?;
            Ok(Some(copied_output_key(key)))
        };
        match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map_or(Ok(None), decode),
            Tag::Vocab => self
                .with_cursor_vocab_key(id.payload(), decode)?
                .unwrap_or(Ok(None)),
            Tag::Delta => self
                .snap
                .generation
                .dvocab
                .with(|v| v.get(id.payload()).map_or(Ok(None), &mut decode)),
            Tag::Undef | Tag::Special => Ok(None),
            _ => {
                cover(128)?;
                Ok(self.snap.term(id))
            }
        }
    }

    fn batch_key_decode_bytes(&self, key: &[u8]) -> Result<u64> {
        let envelope = (key.len() as u64).saturating_mul(8).saturating_add(128);
        if id::is_key_triple(key) {
            // Compact triple keys can hide a component vector and boxed triple
            // at each level, even when there is very little lexical text, while
            // wide triple terms still need the legacy eightfold envelope floor.
            Ok(output_key_bytes(self, key)?.max(envelope))
        } else {
            Ok(envelope)
        }
    }

    pub(super) fn decoded_bytes(&self, id: Id) -> Result<u64> {
        self.check()?;
        let size = |key: &[u8]| self.batch_key_decode_bytes(key);
        let bytes = match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map(size),
            Tag::Vocab => self.with_controlled_vocab_key(id.payload(), |key| Ok(size(key)))?,
            Tag::Delta => {
                if id.payload() >= self.snap.dvocab_len {
                    None
                } else {
                    self.snap
                        .generation
                        .dvocab
                        .with(|v| v.get(id.payload()).map(size))
                }
            }
            _ => None,
        };
        bytes.unwrap_or(Ok(128))
    }

    /// The value of a base vocabulary id from the numeric column, when it holds one.
    /// Kept out of line so that [`Ctx::value`] stays small for the ids it decodes.
    #[inline(never)]
    fn column_value(&self, payload: u64) -> Option<Value> {
        self.snap.generation.vocab.numeric()?.get(payload).value()
    }

    /// Decode an ORDER value without latching a recoverable trial refusal.
    /// Inline and numeric-column values keep their existing paths. Dictionary
    /// reconstruction and admitted decoding retain the same owner and charges.
    pub(super) fn cursor_value_checked(&self, id: Id) -> Result<Option<Value>> {
        self.check()?;
        match id.tag() {
            Tag::Undef | Tag::Special => Ok(None),
            Tag::Int => Ok(Some(Value::Integer(id.as_i64().into()))),
            Tag::Bool => Ok(Some(Value::Bool(id.as_bool()))),
            Tag::Double => Ok(Some(Value::Double(id.as_f64().into()))),
            Tag::Decimal => Ok(Some(Value::Decimal(id::unpack_decimal(id.payload())))),
            Tag::DateTime | Tag::Date => {
                Ok(id::inline_to_literal(id).map(|l| Value::from_literal(&l)))
            }
            Tag::BNode => Ok(Some(Value::BNode(bnode_for(id).as_str().into()))),
            Tag::Vocab => {
                if let Some(value) = self.column_value(id.payload()) {
                    return Ok(Some(value));
                }
                self.with_controlled_vocab_key(id.payload(), |key| {
                    id::validate_key_framing(key)?;
                    self.reserve_decode_checked(key.len())?;
                    self.check()?;
                    Ok(decode_value_key(key))
                })
            }
            Tag::Delta => {
                if id.payload() >= self.snap.dvocab_len {
                    return Ok(None);
                }
                let len = self
                    .snap
                    .generation
                    .dvocab
                    .with(|v| {
                        v.get(id.payload())
                            .map(|key| id::validate_key_framing(key).map(|()| key.len()))
                    })
                    .transpose()?;
                let Some(len) = len else {
                    return Ok(None);
                };
                self.reserve_decode_checked(len)?;
                self.check()?;
                Ok(self
                    .snap
                    .generation
                    .dvocab
                    .with(|v| v.get(id.payload()).map(decode_value_key)))
            }
            Tag::Local => {
                let len = self
                    .local
                    .read()
                    .get(id.payload())
                    .map(|key| id::validate_key_framing(key).map(|()| key.len()))
                    .transpose()?;
                let Some(len) = len else {
                    return Ok(None);
                };
                self.reserve_decode_checked(len)?;
                self.check()?;
                Ok(self.local.read().get(id.payload()).map(decode_value_key))
            }
        }
    }

    /// Decode an id into a value, with a per-query cache.
    pub fn value(&self, id: Id) -> Option<Value> {
        match id.tag() {
            Tag::Undef | Tag::Special => None,
            Tag::Int => Some(Value::Integer(id.as_i64().into())),
            Tag::Bool => Some(Value::Bool(id.as_bool())),
            Tag::Double => Some(Value::Double(id.as_f64().into())),
            Tag::Decimal => Some(Value::Decimal(id::unpack_decimal(id.payload()))),
            Tag::DateTime | Tag::Date => id::inline_to_literal(id).map(|l| Value::from_literal(&l)),
            Tag::BNode => Some(Value::BNode(bnode_for(id).as_str().into())),
            _ => {
                // a number of the base vocabulary from its numeric column, without
                // decoding the key (the same value, see `crate::vocab::numeric`)
                if id.tag() == Tag::Vocab
                    && let Some(v) = self.column_value(id.payload())
                {
                    return Some(v);
                }
                if let Some(v) = self.values.get(id) {
                    return Some(v);
                }
                let v = match id.tag() {
                    Tag::Vocab => self.with_vocab_key(id.payload(), decode_value_key),
                    Tag::Delta => self.decode_delta_infallible(id.payload(), decode_value_key),
                    Tag::Local => self.decode_local_infallible(id.payload(), decode_value_key),
                    _ => unreachable!("dictionary value tag"),
                }?;
                self.cache_value(id, &v);
                Some(v)
            }
        }
    }

    /// Keep `v` as the decoded value of `id`, when the cache's reservation can grow to
    /// hold it (see [`ValueCache`]). A refused reservation leaves the value uncached and
    /// records no failure, since the cache is optional.
    fn cache_value(&self, id: Id, v: &Value) {
        let cache = &self.values;
        let cap = ValueCache::cap(self.mem_limit);
        let share = cap / VALUE_SHARDS as u64;
        let entry = ValueCache::entry_bytes(v);
        if entry > share {
            return;
        }
        let mut shard = cache.shard(id).write();
        if shard.map.contains_key(&id) {
            return;
        }
        if shard.bytes + entry > share || shard.map.len() > VALUE_SHARD_CAP {
            // The map is freed with its slots, so that the entries' estimate, slots
            // included, leaves the cache with them.
            cache.used.fetch_sub(shard.bytes, Ordering::AcqRel);
            shard.bytes = 0;
            shard.map = FxHashMap::default();
        }
        let used = cache.used.fetch_add(entry, Ordering::AcqRel) + entry;
        if !cache.charge.covers(used)
            && self
                .grow_reservation(&cache.charge, used, cap, false)
                .is_err()
        {
            cache.used.fetch_sub(entry, Ordering::AcqRel);
            return;
        }
        shard.bytes += entry;
        shard.map.insert(id, v.clone());
    }

    /// Grow `reservation` to cover `used` bytes, a step ahead within `cap`. The step is
    /// [`EAGER_STEP`], or a sixty-fourth of a smaller memory budget. When the
    /// budget or the pool cannot give the step, state that the query needs (`exact`)
    /// takes exactly what it needs, while an optional cache declines, so that it never
    /// takes the last room an operator needs. The charge stays with the context until it
    /// drops. A refusal changes nothing.
    /// The step in which state charged a value at a time grows its reservation:
    /// [`EAGER_STEP`], or a sixty-fourth of a smaller memory budget and at least 256
    /// bytes. A small budget takes small steps, so that the slack of the steps never
    /// holds more than a sixty-fourth of it.
    #[inline]
    pub(crate) fn reservation_step(&self) -> u64 {
        (self.mem_limit / 64).clamp(256, EAGER_STEP)
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn grow_reservation(
        &self,
        reservation: &StepReservation,
        used: u64,
        cap: u64,
        exact: bool,
    ) -> Result<()> {
        #[cfg(test)]
        VALUE_GROWS.with(|n| n.set(n.get() + 1));
        let mut owner = reservation.grow.lock();
        let reserved = reservation.reserved.load(Ordering::Acquire);
        if used <= reserved {
            return Ok(());
        }
        let need = used - reserved;
        let step = self.reservation_step();
        // When a required (`exact`) reservation grows under tight remaining budget, cap
        // speculative lookahead slack so a small interning charge cannot swallow the
        // headroom left for batch publication or row decoding.
        let ahead = if exact && self.mem_limit < u64::MAX {
            let avail = self.mem_limit.saturating_sub(self.live_bytes());
            let slack = step.min(avail.saturating_sub(need) / 4);
            used.next_multiple_of(step)
                .min(used.saturating_add(slack))
                .min(cap)
                .max(used)
        } else {
            used.next_multiple_of(step).min(cap).max(used)
        };
        let (charge, admitted) = match self.charge(ahead - reserved) {
            Ok(charge) => (charge, ahead),
            Err(_) if exact && ahead > used => (self.charge(need)?, used),
            Err(error) => return Err(error),
        };
        let id = charge.disarm();
        debug_assert!(
            owner.is_none() || id.is_none() || *owner == id,
            "one owner of a reservation"
        );
        if owner.is_none() {
            *owner = id;
        }
        reservation.reserved.store(admitted, Ordering::Release);
        Ok(())
    }

    /// Is the id an IRI / blank node / literal (without full decoding where possible)?
    pub fn kind(&self, id: Id) -> TermKind {
        match id.tag() {
            Tag::Undef | Tag::Special => TermKind::None,
            Tag::Bool | Tag::Int | Tag::Double | Tag::Decimal | Tag::DateTime | Tag::Date => {
                TermKind::Literal
            }
            Tag::BNode => TermKind::BNode,
            Tag::Vocab => {
                let v = &self.snap.generation.vocab;
                if v.is_iri(id.payload()) {
                    TermKind::Iri
                } else if v.is_triple(id.payload()) {
                    TermKind::Triple
                } else {
                    TermKind::Literal
                }
            }
            Tag::Delta => self
                .snap
                .generation
                .dvocab
                .with(|d| d.get(id.payload()).map_or(TermKind::None, key_kind)),
            Tag::Local => self
                .local
                .read()
                .get(id.payload())
                .map_or(TermKind::None, key_kind),
        }
    }

    /// The order of two base-vocabulary simple literals (`xsd:string`) by their lexical
    /// forms, compared on their vocabulary keys without decoding either term. A simple
    /// literal's key is `"` lexical 0xFF, and UTF-8 byte order is code point order.
    /// `None` when either id is another kind of term.
    pub(super) fn simple_literal_cmp(&self, a: Id, b: Id) -> Option<std::cmp::Ordering> {
        self.simple_literal_cmp_in(a, b, &mut FloorScratch::default())
    }

    /// [`Ctx::simple_literal_cmp`] reusing the caller's scratch buffers across calls.
    /// Checks and decodes `a` before `b` so a cursor's memory refusal on the left operand
    /// is recorded even when the right operand is not a base-vocabulary simple literal.
    #[inline]
    pub(super) fn simple_literal_cmp_in(
        &self,
        a: Id,
        b: Id,
        scratch: &mut FloorScratch,
    ) -> Option<std::cmp::Ordering> {
        if a.tag() != Tag::Vocab {
            return None;
        }
        let v = &self.snap.generation.vocab;
        let first_triple = v.first_triple();
        let x = a.payload();
        // the literals come before the triple terms and the IRIs
        if x >= first_triple {
            return None;
        }
        let lexical = |k: &[u8]| -> Option<std::ops::Range<usize>> {
            (k.len() >= 2 && k[0] == b'"' && k[k.len() - 1] == id::KEY_SEP).then(|| 1..k.len() - 1)
        };
        let FloorScratch { key, other } = scratch;
        self.with_vocab_key_in(x, key, |kx| {
            let rx = lexical(kx)?;
            if a == b {
                return Some(std::cmp::Ordering::Equal);
            }
            if b.tag() != Tag::Vocab {
                return None;
            }
            let y = b.payload();
            if y >= first_triple {
                return None;
            }
            self.with_vocab_key_in(y, other, |ky| {
                let ry = lexical(ky)?;
                Some(kx[rx.clone()].cmp(&ky[ry]))
            })
            .flatten()
        })
        .flatten()
    }

    /// For a base-vocabulary simple literal, the first base id whose key starts with its
    /// lexical form, which is the first of the strings it is a proper prefix of, or its
    /// own id when it is a prefix of none. `None` for any other id.
    ///
    /// The vocabulary sorts a string's key `"` lexical 0xFF after the keys of its
    /// extensions, so id order is lexical order except for those. For simple literals
    /// `a < b` by id, the lexical form of `a` is less than that of `b` exactly when `a`
    /// is below `b`'s floor, since the keys between `b`'s floor and `b` all start with
    /// `b`'s lexical form.
    ///
    /// Key reconstruction and the search for the floor use the decoding envelope
    /// ([`Ctx::reserve_decode`]), so a cursor reserves them on the ledger. A refusal
    /// records a failure, which the next check reports, and gives `None`.
    #[cfg(test)]
    pub(super) fn string_floor(&self, id: Id) -> Option<u64> {
        self.string_floor_in(id, &mut FloorScratch::default())
    }

    /// Compute [`Ctx::string_floor`] reusing the caller's scratch buffers without recording
    /// a sticky memory failure on decoding-envelope refusal. Returns `Ok(Some(floor))` for a
    /// base-vocabulary simple literal, `Ok(None)` for any other id, or `Err` when the
    /// cursor's decoding envelope refuses reconstruction or a prior memory failure is
    /// already recorded.
    #[inline]
    pub(super) fn try_string_floor_in(
        &self,
        id: Id,
        scratch: &mut FloorScratch,
    ) -> Result<Option<u64>> {
        if id.tag() != Tag::Vocab {
            return Ok(None);
        }
        let v = &self.snap.generation.vocab;
        let p = id.payload();
        if p >= v.first_triple() {
            return Ok(None);
        }
        let FloorScratch { key, other } = scratch;
        let Some(res) =
            self.try_with_vocab_key_prefix_in(p, key, |shared, key| -> Result<Option<u64>> {
                if key.len() < 2 || key[0] != b'"' || key[key.len() - 1] != id::KEY_SEP {
                    return Ok(None);
                }
                let prefix = &key[..key.len() - 1];
                // Within a block, the stored common prefix tells whether the preceding
                // key extends this simple literal. Across blocks, compare the keys.
                let extended = match shared {
                    Some(len) => len >= prefix.len(),
                    None if p == 0 => false,
                    None => {
                        match self.try_with_vocab_key_in(p - 1, other, |k| k.starts_with(prefix))? {
                            Some(ext) => ext,
                            None => return Ok(None),
                        }
                    }
                };
                if !extended {
                    return Ok(Some(p));
                }
                self.try_vocab_lower_bound(prefix).map(Some)
            })?
        else {
            return Ok(None);
        };
        res
    }

    /// [`Ctx::string_floor`] reusing the caller's scratch buffers across calls.
    #[cfg(test)]
    #[inline]
    pub(super) fn string_floor_in(&self, id: Id, scratch: &mut FloorScratch) -> Option<u64> {
        match self.try_string_floor_in(id, scratch) {
            Ok(floor) => floor,
            Err(error) => {
                self.fail_memory(&error);
                None
            }
        }
    }

    /// Read a vocabulary key and, within its block, its common prefix length with
    /// the preceding key, reusing `key` across calls. Admits scratch through the
    /// decoding envelope before allocation and returns any refusal without recording it.
    #[inline]
    fn try_with_vocab_key_prefix_in<T>(
        &self,
        id: u64,
        key: &mut Vec<u8>,
        f: impl FnOnce(Option<usize>, &[u8]) -> T,
    ) -> Result<Option<T>> {
        self.check_memory_failure()?;
        let vocab = &self.snap.generation.vocab;
        if self.is_strict_decode() {
            self.check()?;
            let mut f = Some(f);
            return vocab
                .get_with_controlled(
                    id,
                    |need| self.reserve_decode_checked(need),
                    |_| self.check(),
                    |k| {
                        id::validate_key_framing(k)?;
                        Ok(f.take().map(|f| f(None, k)))
                    },
                )
                .and_then(|opt| opt.transpose())
                .map(Option::flatten);
        }
        vocab.get_with_shared_prefix_checked_in(
            id,
            key,
            |need| self.reserve_decode_checked(need),
            f,
        )
    }

    /// The query's floor cache.
    #[cfg(test)]
    pub(super) fn floor_cache(&self) -> &FloorCache {
        &self.floors
    }

    /// First non-literal base vocabulary id.
    #[inline]
    pub(super) fn first_triple(&self) -> u64 {
        self.snap.generation.vocab.first_triple()
    }

    /// Whether `id` is a base-vocabulary literal id (before triple terms and IRIs), which
    /// may be a simple literal with a cached string floor.
    #[inline]
    pub(super) fn may_be_base_literal(&self, id: Id) -> bool {
        id.tag() == Tag::Vocab && id.payload() < self.snap.generation.vocab.first_triple()
    }

    /// Whether the floor cache has not yet attempted to allocate its first table.
    #[inline]
    pub(super) fn floor_cache_needs_init(&self) -> bool {
        !self.floors.initialized.load(Ordering::Acquire)
    }

    /// A map of the query's floor cache for one worker of a mask (see [`FloorCache`]),
    /// which allocates the cache's first table at [`FLOOR_TABLE_FIRST`] when it has none.
    #[cfg(test)]
    pub(super) fn floor_map(&self) -> FloorMap<'_> {
        self.floor_map_for(FLOOR_TABLE_FIRST)
    }

    /// A map of the query's floor cache for one worker of a mask, sizing the first table
    /// to at most `first_slots` when no table has been installed yet.
    #[cfg(test)]
    pub(super) fn floor_map_for(&self, first_slots: usize) -> FloorMap<'_> {
        let mut tables = self.floors.tables.lock();
        if tables.cur.is_none() && !tables.full {
            let first = match self.floors.first_slots.load(Ordering::Relaxed) {
                0 => first_slots,
                slots => slots,
            };
            self.install_floor_table(&mut tables, first);
            self.floors.initialized.store(true, Ordering::Release);
        }
        FloorMap {
            ctx: self,
            cur: tables.cur.clone().map(FloorTableRef::Owned),
            first_triple: self.snap.generation.vocab.first_triple(),
            scratch: FloorScratch::default(),
            evicted: 0,
            lookups: 0,
            hits: 0,
            #[cfg(test)]
            computed: 0,
        }
    }

    /// A worker's map borrowing a `FloorTable` snapshot taken once for the whole mask or
    /// part, avoiding per-worker mutex acquisitions and `Arc` refcount traffic.
    #[inline]
    pub(super) fn floor_map_borrowed<'a>(
        &'a self,
        table: Option<&'a FloorTable>,
        first_triple: u64,
    ) -> FloorMap<'a> {
        FloorMap {
            ctx: self,
            cur: table.map(FloorTableRef::Borrowed),
            first_triple,
            scratch: FloorScratch::default(),
            evicted: 0,
            lookups: 0,
            hits: 0,
            #[cfg(test)]
            computed: 0,
        }
    }

    /// Initialize the floor cache from the first eligible mask's slot and reuse estimate.
    /// When `worth_caching` is false, defers table allocation and enters bypass mode so
    /// low-repeat streams compare keys directly until the periodic probe mask runs.
    pub(super) fn init_floor_cache(&self, slots: usize, worth_caching: bool) -> FloorMaskPlan {
        let mut tables = self.floors.tables.lock();
        if !self.floors.initialized.load(Ordering::Relaxed) && tables.cur.is_none() && !tables.full
        {
            #[cfg(test)]
            let first = match self.floors.first_slots.load(Ordering::Relaxed) {
                0 => slots,
                s => s,
            };
            #[cfg(not(test))]
            let first = slots;
            if worth_caching {
                self.install_floor_table(&mut tables, first);
                if let Some(cur) = &tables.cur
                    && cur.len() > FLOOR_TABLE_FIRST
                {
                    tables.warmup_lookups = cur.len() / 2;
                }
                self.floors.initialized.store(true, Ordering::Release);
                return FloorMaskPlan::Floors(tables.cur.clone());
            }
            tables.deferred_slots = first;
            tables.bypass = true;
            tables.bypass_streak = 1;
            self.floors.initialized.store(true, Ordering::Release);
            #[cfg(test)]
            self.floors.bypassed.fetch_add(1, Ordering::Relaxed);
            return FloorMaskPlan::Direct;
        }
        self.begin_floor_mask_locked(&mut tables)
    }

    /// Defer floor-table allocation and enter bypass mode when the first eligible mask's
    /// sample refused floor-only decoding. If another mask initialized the cache first,
    /// commits this mask directly without allocating or growing a table.
    pub(super) fn init_floor_cache_after_refused_floor_trial(&self, slots: usize) -> FloorMaskPlan {
        let mut tables = self.floors.tables.lock();
        if !self.floors.initialized.load(Ordering::Relaxed) && tables.cur.is_none() && !tables.full
        {
            #[cfg(test)]
            let first = match self.floors.first_slots.load(Ordering::Relaxed) {
                0 => slots,
                s => s,
            };
            #[cfg(not(test))]
            let first = slots;
            tables.deferred_slots = first;
            tables.bypass = true;
            tables.bypass_streak = 1;
            self.floors.initialized.store(true, Ordering::Release);
            #[cfg(test)]
            self.floors.bypassed.fetch_add(1, Ordering::Relaxed);
            return FloorMaskPlan::Direct;
        }
        self.begin_floor_mask_after_refused_floor_trial_locked(&mut tables)
    }

    /// Whether the current floor table has enough low-hit lookups past its warmup budget
    /// to enter direct bypass if table growth does not happen first.
    fn low_hit_bypass_pending(tables: &FloorTables, cur: &FloorTable) -> bool {
        let lookups = cur.lookups.load(Ordering::Relaxed);
        if lookups < FLOOR_BYPASS_MIN_LOOKUPS {
            return false;
        }
        let hits = cur.hits.load(Ordering::Relaxed);
        let low_hit = hits.saturating_mul(FLOOR_BYPASS_HIT_DENOM) < lookups;
        low_hit && tables.warmup_lookups <= lookups
    }

    /// Preview the floor mode and table used to validate the next mask's sample without
    /// mutating [`FloorCache`] state. Whenever bypass is active (including on a scheduled
    /// periodic probe) or a low-hit bypass can take effect (including when table growth is
    /// also pending and could still be declined after sample decoding enlarges the decode
    /// envelope), this selects [`FloorMaskPlan::Direct`] so sample validation compares keys
    /// directly and never triggers a speculative floor-only predecessor decode before
    /// [`Ctx::begin_floor_mask`] commits the mode.
    pub(super) fn peek_floor_plan(&self) -> FloorMaskPlan {
        let tables = self.floors.tables.lock();
        if tables.bypass {
            return FloorMaskPlan::Direct;
        }
        if let Some(cur) = &tables.cur
            && Self::low_hit_bypass_pending(&tables, cur)
        {
            return FloorMaskPlan::Direct;
        }
        FloorMaskPlan::Floors(tables.cur.clone())
    }

    /// Choose whether the next mask or eager part runs against the shared floor table
    /// (growing it when read-again evictions crossed the threshold) or bypasses the table
    /// for direct key comparison while its hit rate stays low.
    pub(super) fn begin_floor_mask(&self) -> FloorMaskPlan {
        let mut tables = self.floors.tables.lock();
        self.begin_floor_mask_locked(&mut tables)
    }

    /// Commit a mask whose sample validated through direct key comparison. If the mask
    /// commits `Direct` without allocating or growing a floor table, applies the state
    /// transition and returns [`DirectCommitPlan::Direct`]. Otherwise leaves `FloorTables`
    /// unchanged and returns [`DirectCommitPlan::NeedsFloorTrial`] so the caller can account
    /// for the sample's floor decodes non-stickily before optional table memory is spent.
    pub(super) fn begin_floor_mask_after_direct(&self) -> DirectCommitPlan {
        let mut tables = self.floors.tables.lock();
        if !tables.bypass {
            if let Some(cur) = &tables.cur {
                let growth_pending = !tables.full
                    && cur.evicted.load(Ordering::Relaxed) >= cur.len() / FLOOR_TABLE_EVICTIONS;
                if growth_pending && self.can_grow_floor_table_locked(cur) {
                    return DirectCommitPlan::NeedsFloorTrial(Some(cur.clone()));
                }
                if Self::low_hit_bypass_pending(&tables, cur) {
                    let _ = cur.lookups.swap(0, Ordering::Relaxed);
                    let _ = cur.hits.swap(0, Ordering::Relaxed);
                    if growth_pending {
                        tables.full = true;
                    }
                    tables.warmup_lookups = 0;
                    tables.bypass = true;
                    tables.bypass_streak = 1;
                    #[cfg(test)]
                    self.floors.bypassed.fetch_add(1, Ordering::Relaxed);
                    return DirectCommitPlan::Direct;
                }
            }
            return DirectCommitPlan::NeedsFloorTrial(tables.cur.clone());
        }
        if tables.bypass_streak + 1 < FLOOR_BYPASS_PROBE_INTERVAL {
            tables.bypass_streak += 1;
            #[cfg(test)]
            self.floors.bypassed.fetch_add(1, Ordering::Relaxed);
            return DirectCommitPlan::Direct;
        }
        DirectCommitPlan::NeedsFloorTrial(tables.cur.clone())
    }

    /// Commit a mask to direct comparison after a fallible floor trial on its validated
    /// sample refused decoding-envelope admission, skipping optional table growth and
    /// deferred table allocation so the validated direct path succeeds cleanly.
    pub(super) fn begin_floor_mask_after_refused_floor_trial(&self) -> FloorMaskPlan {
        let mut tables = self.floors.tables.lock();
        self.begin_floor_mask_after_refused_floor_trial_locked(&mut tables)
    }

    fn begin_floor_mask_after_refused_floor_trial_locked(
        &self,
        tables: &mut FloorTables,
    ) -> FloorMaskPlan {
        if !tables.bypass {
            let bypass = tables.cur.as_ref().is_some_and(|cur| {
                if Self::low_hit_bypass_pending(tables, cur) {
                    let _ = cur.lookups.swap(0, Ordering::Relaxed);
                    let _ = cur.hits.swap(0, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            });
            if bypass {
                tables.warmup_lookups = 0;
                tables.bypass = true;
                tables.bypass_streak = 1;
            }
        } else if tables.bypass_streak + 1 < FLOOR_BYPASS_PROBE_INTERVAL {
            tables.bypass_streak += 1;
        } else {
            tables.bypass_streak = 1;
        }
        #[cfg(test)]
        self.floors.bypassed.fetch_add(1, Ordering::Relaxed);
        FloorMaskPlan::Direct
    }

    fn begin_floor_mask_locked(&self, tables: &mut FloorTables) -> FloorMaskPlan {
        if !tables.bypass {
            if tables.cur.is_some() {
                let grew = self.maybe_grow_floor_table(tables);
                if grew {
                    tables.warmup_lookups = 0;
                } else if let Some(cur) = &tables.cur {
                    let lookups = cur.lookups.swap(0, Ordering::Relaxed);
                    let hits = cur.hits.swap(0, Ordering::Relaxed);
                    if lookups >= FLOOR_BYPASS_MIN_LOOKUPS {
                        let low_hit = hits.saturating_mul(FLOOR_BYPASS_HIT_DENOM) < lookups;
                        if !low_hit {
                            tables.warmup_lookups = 0;
                        } else if tables.warmup_lookups > lookups {
                            tables.warmup_lookups -= lookups;
                        } else {
                            tables.warmup_lookups = 0;
                            tables.bypass = true;
                            tables.bypass_streak = 1;
                            #[cfg(test)]
                            self.floors.bypassed.fetch_add(1, Ordering::Relaxed);
                            return FloorMaskPlan::Direct;
                        }
                    }
                }
            }
            return FloorMaskPlan::Floors(tables.cur.clone());
        }
        if tables.bypass_streak + 1 < FLOOR_BYPASS_PROBE_INTERVAL {
            tables.bypass_streak += 1;
            #[cfg(test)]
            self.floors.bypassed.fetch_add(1, Ordering::Relaxed);
            return FloorMaskPlan::Direct;
        }
        tables.bypass = false;
        tables.bypass_streak = 0;
        if tables.cur.is_none() && !tables.full {
            let slots = if tables.deferred_slots > 0 {
                tables.deferred_slots
            } else {
                FLOOR_TABLE_FIRST
            };
            #[cfg(test)]
            let slots = match self.floors.first_slots.load(Ordering::Relaxed) {
                0 => slots,
                s => s,
            };
            self.install_floor_table(tables, slots);
        }
        if let Some(cur) = &tables.cur {
            cur.lookups.store(0, Ordering::Relaxed);
            cur.hits.store(0, Ordering::Relaxed);
        }
        FloorMaskPlan::Floors(tables.cur.clone())
    }

    /// Replace the floor cache's table by a larger one when the floors it evicted that
    /// had been read again pass [`FLOOR_TABLE_EVICTIONS`] (see [`FloorCache`]). A mask
    /// asks before each part of its rows, so that the workers of one part share a table.
    #[cfg(test)]
    pub(super) fn grow_floor_cache(&self) {
        let mut tables = self.floors.tables.lock();
        let _ = self.maybe_grow_floor_table(&mut tables);
    }

    fn clamped_floor_slots(&self, requested: usize, cap: u64) -> Option<usize> {
        let first_triple = self.snap.generation.vocab.first_triple();
        let max_pow2 = 1usize << (usize::BITS - 1);
        let slots = requested
            .max(1)
            .checked_next_power_of_two()
            .unwrap_or(max_pow2);
        let most = usize::try_from(first_triple)
            .unwrap_or(usize::MAX)
            .checked_next_power_of_two()
            .unwrap_or(max_pow2);
        let room =
            usize::try_from(cap.saturating_sub(FloorTable::OVERHEAD) / 8).unwrap_or(usize::MAX);
        // the largest power of two of words that fits the cap with the table's overhead
        let room = if room == 0 { 0 } else { 1 << room.ilog2() };
        let wanted = slots.min(most);
        if room < wanted && room < FLOOR_TABLE_FEWEST {
            return None;
        }
        Some(wanted.min(room))
    }

    fn can_grow_floor_table_locked(&self, cur: &FloorTable) -> bool {
        let cap = FloorCache::cap(self.mem_limit);
        let requested = cur.len().saturating_mul(FLOOR_TABLE_GROWTH);
        let Some(slots) = self.clamped_floor_slots(requested, cap) else {
            return false;
        };
        if slots <= cur.len() {
            return false;
        }
        let step = self.reservation_step();
        let used = self
            .floors
            .used
            .load(Ordering::Acquire)
            .saturating_add(FloorTable::bytes(slots));
        let ahead = used.next_multiple_of(step).min(cap).max(used);
        let delta = ahead.saturating_sub(self.floors.charge.reserved());
        used <= cap && (delta == 0 || delta <= self.memory_room() / 2)
    }

    fn maybe_grow_floor_table(&self, tables: &mut FloorTables) -> bool {
        let Some(cur) = &tables.cur else {
            return false;
        };
        if tables.full || cur.evicted.load(Ordering::Relaxed) < cur.len() / FLOOR_TABLE_EVICTIONS {
            return false;
        }
        let prev_len = cur.len();
        let slots = prev_len.saturating_mul(FLOOR_TABLE_GROWTH);
        self.install_floor_table(tables, slots);
        tables.cur.as_ref().is_some_and(|t| t.len() > prev_len)
    }

    /// Make a table of at most `slots` the floor cache's current one, charged first, with
    /// live entries from any previous table copied in before publication. The slots are
    /// no more than the vocabulary's strings need, since past that no two ids share a
    /// slot, and no more than the cap holds. A cap that allows fewer than
    /// [`FLOOR_TABLE_FEWEST`] slots leaves the cache full, unless the vocabulary's strings
    /// need no more than the cap allows. When an initial table does not fit the remaining
    /// budget, it steps down by halves to [`FLOOR_TABLE_FEWEST`] before leaving the cache
    /// full.
    #[cold]
    #[inline(never)]
    fn install_floor_table(&self, tables: &mut FloorTables, slots: usize) {
        let cache = &self.floors;
        let first_triple = self.snap.generation.vocab.first_triple();
        let cap = FloorCache::cap(self.mem_limit);
        let Some(mut slots) = self.clamped_floor_slots(slots, cap) else {
            tables.full = true;
            return;
        };
        let step = self.reservation_step();
        let base_used = cache.used.load(Ordering::Acquire);
        let reserved = cache.charge.reserved();
        loop {
            if tables.cur.as_ref().is_some_and(|cur| slots <= cur.len()) {
                // the table is as large as the strings or the cap allow
                tables.full = true;
                return;
            }
            let bytes = FloorTable::bytes(slots);
            // Every table allocated stays charged, since a mask may still read an old one.
            let used = base_used.saturating_add(bytes);
            let ahead = used.next_multiple_of(step).min(cap).max(used);
            let delta = ahead.saturating_sub(reserved);
            // Keep at least half the remaining room for required query state (such as
            // decoding envelopes) when admitting an optional floor table.
            let fits = used <= cap
                && (delta == 0
                    || (delta <= self.memory_room() / 2
                        && self
                            .grow_reservation(&cache.charge, used, cap, false)
                            .is_ok()));
            if fits {
                cache.used.store(used, Ordering::Release);
                break;
            }
            if tables.cur.is_none() && slots > FLOOR_TABLE_FEWEST {
                slots /= 2;
                continue;
            }
            tables.full = true;
            return;
        }
        tracing::debug!(target: "sparkles::sparql::exec", "string floor table of {slots} slots");
        let mut table = FloorTable::new(slots, first_triple);
        if let Some(old) = &tables.cur {
            old.copy_into(&mut table);
        }
        tables.cur = Some(Arc::new(table));
    }

    /// The first base id whose key is not below `key`, checking the query's controls
    /// in strict decode mode.
    fn try_vocab_lower_bound(&self, key: &[u8]) -> Result<u64> {
        let v = &self.snap.generation.vocab;
        self.check_memory_failure()?;
        if self.is_strict_decode() {
            self.check()?;
            let (Ok(i) | Err(i)) = v.find_exact_controlled(key, |_| self.check())?;
            return Ok(i);
        }
        let (Ok(i) | Err(i)) =
            v.find_with_checked(key, |need| self.reserve_decode_checked(need))?;
        Ok(i)
    }

    pub fn local_len(&self) -> u64 {
        self.local.read().len()
    }

    /// Back this context's query-local vocabulary directly with an incremental circuit's
    /// [`Terms`](crate::incr::terms::Terms) so `Tag::Local` ids resolve and intern on
    /// demand without copying historical keys.
    pub(crate) fn swap_circuit_terms(
        &self,
        terms: crate::incr::terms::Terms,
    ) -> crate::incr::terms::Terms {
        let mut guard = self.local.write();
        let bytes = terms.bytes();
        let prev = match std::mem::replace(&mut *guard, LocalVocab::Circuit(terms)) {
            LocalVocab::Circuit(prev) => prev,
            LocalVocab::Owned(_) => crate::incr::terms::Terms::default(),
        };
        self.local_retained.store(bytes, Ordering::Release);
        prev
    }

    /// Take back the [`Terms`](crate::incr::terms::Terms) previously attached with
    /// [`Ctx::swap_circuit_terms`], restoring an empty owned vocabulary.
    pub(crate) fn take_circuit_terms(&self) -> crate::incr::terms::Terms {
        let mut guard = self.local.write();
        let prev = match std::mem::take(&mut *guard) {
            LocalVocab::Circuit(terms) => terms,
            LocalVocab::Owned(_) => crate::incr::terms::Terms::default(),
        };
        self.local_retained.store(0, Ordering::Release);
        prev
    }

    #[cfg(test)]
    pub(crate) fn circuit_terms_backing_ptrs(&self) -> Option<(usize, Option<usize>)> {
        match &*self.local.read() {
            LocalVocab::Circuit(terms) => Some(terms.backing_ptrs()),
            LocalVocab::Owned(_) => None,
        }
    }
}

/// The term of one decoded vocabulary key, admitted on `row` before it is built.
/// An IRI or literal key is split and its text checked once, and the term is built
/// from the checked parts with the charge [`output_key_bytes`] gives for valid text.
/// Sizing and building it in two passes repeated the separator scan and the UTF-8
/// checks, a measurable share of the time to write a large answer.
#[inline]
fn output_key_term(ctx: &Ctx, key: &[u8], row: &RowCredit<'_>) -> Result<Term> {
    if let Some(text) = id::TextKey::parse(key) {
        let tag = match text {
            id::TextKey::Lang(_, tag) => tag.len() as u64,
            _ => 0,
        };
        row.add(
            (key.len() as u64)
                .checked_add(64 + tag)
                .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?,
        )?;
        return Ok(text.into_term());
    }
    row.add(output_key_bytes(ctx, key)?)?;
    Ok(copied_output_key(key))
}

/// Heap and construction overlap of one decoded vocabulary key, excluding the
/// outer Term slot already covered by its row. Ordinary lexical bytes are charged
/// once, independently of the existing reconstruction envelope's eightfold policy.
/// RDF-star nodes also cover their temporary component Vec and final boxed Triple.
fn copied_output_key(key: &[u8]) -> Term {
    if key.first() == Some(&b'(') {
        super::depth::with_stack(crate::nesting::MAX_TRIPLE_TERMS, || id::key_to_term(key))
    } else {
        id::key_to_term(key)
    }
}

fn decode_value_key(key: &[u8]) -> Value {
    if key.first() == Some(&b'(') {
        super::depth::with_stack(crate::nesting::MAX_TRIPLE_TERMS, || Value::from_key(key))
    } else {
        Value::from_key(key)
    }
}

fn output_key_bytes(ctx: &Ctx, key: &[u8]) -> Result<u64> {
    id::key_decode_bytes_with(key, || ctx.memory_exceeded(u64::MAX))
}

/// Estimated bytes of `rows` rows of `width` ids (a row of a zero-width table still
/// counts as one id).
#[inline]
pub fn table_bytes(rows: usize, width: usize) -> u64 {
    (rows as u64).saturating_mul(width.max(1) as u64 * 8)
}

/// RAII guard returned by [`Ctx::strict_decode_guard`].
pub(super) struct StrictDecodeGuard<'a> {
    ctx: &'a Ctx,
}

impl Drop for StrictDecodeGuard<'_> {
    fn drop(&mut self) {
        self.ctx.strict_decode.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Estimated memory held by a running operator (its input tables), released when the
/// guard drops, also while an error unwinds.
pub struct Charge<'a> {
    ctx: &'a Ctx,
    bytes: std::cell::Cell<u64>,
    owner: std::cell::Cell<Option<u64>>,
}
/// What [`Ctx::output_term`] admits a decoded term's bytes on before it allocates them.
/// It counts what the terms use, which [`TermCredit::resize`] sets, as a reused key
/// buffer comes and goes.
pub(super) trait TermCredit {
    fn ctx(&self) -> &Ctx;
    fn add(&self, bytes: u64) -> Result<()>;
    /// What the terms use.
    fn bytes(&self) -> u64;
    /// Set what the terms use, admitting growth before it is allocated.
    fn resize(&self, bytes: u64) -> Result<()>;
}

impl TermCredit for Charge<'_> {
    fn ctx(&self) -> &Ctx {
        self.ctx
    }
    fn add(&self, bytes: u64) -> Result<()> {
        Charge::add(self, bytes)
    }
    fn bytes(&self) -> u64 {
        Charge::bytes(self)
    }
    fn resize(&self, bytes: u64) -> Result<()> {
        Charge::resize(self, bytes)
    }
}

impl Ctx {
    /// Whether the query keeps its memory on the shared ledger, whose every charge
    /// takes a lock, rather than on its fast account.
    pub(super) fn has_ledger(&self) -> bool {
        self.memory.get().is_some()
    }
}

impl Charge<'_> {
    pub(super) fn bytes(&self) -> u64 {
        self.bytes.get()
    }

    /// An empty child charge sharing this charge's owner on the same context, suitable
    /// for later adoption via [`Charge::adopt_charge`] without an owner mismatch.
    pub(super) fn part(&self) -> Charge<'_> {
        Charge {
            ctx: self.ctx,
            bytes: std::cell::Cell::new(0),
            owner: std::cell::Cell::new(self.owner.get()),
        }
    }

    /// Resize this live charge to `bytes` and transfer ownership into a [`RetainedCharge`]
    /// without releasing or double-charging.
    pub(super) fn into_retained(self, bytes: u64) -> Result<Option<RetainedCharge>> {
        Self::transfer_retained(&mut Some(self), bytes)
    }

    /// Transfer a charge without consuming it if admission fails.
    pub(super) fn transfer_retained(
        source: &mut Option<Self>,
        bytes: u64,
    ) -> Result<Option<RetainedCharge>> {
        let held = source.as_ref().expect("live charge to transfer");
        let Some(owner) = held.ctx.retained_owner()? else {
            return Ok(None);
        };
        held.resize(bytes)?;
        let id = source.take().expect("admitted live charge").disarm();
        Ok(Some(RetainedCharge {
            owner,
            id: (bytes > 0).then_some(id).flatten(),
            bytes,
        }))
    }

    pub(super) fn belongs_to(&self, ctx: &Ctx) -> bool {
        std::ptr::eq(self.ctx, ctx)
    }

    /// Reconcile retained physical storage without moving it to a ledger account.
    /// Callers admit growth before allocating and release a shrink after dropping it.
    pub(super) fn resize(&self, bytes: u64) -> Result<()> {
        let current = self.bytes.get();
        if bytes > current {
            self.add(bytes - current)?;
        } else if bytes < current {
            self.ctx.release_live(self.owner.get(), current - bytes);
            self.bytes.set(bytes);
        }
        Ok(())
    }

    /// Consume another live charge on this exact context without a release interval.
    /// Every check precedes mutation, including for an empty but owned source.
    pub(super) fn adopt_charge(&self, source: &mut Option<Charge<'_>>) -> Result<()> {
        let part = source
            .as_ref()
            .ok_or_else(|| Error::invalid("missing materialized input credit"))?;
        if !std::ptr::eq(self.ctx, part.ctx)
            || matches!((self.owner.get(), part.owner.get()), (Some(a), Some(b)) if a != b)
            || (self.bytes.get() != 0 && self.owner.get().is_none())
            || (part.bytes.get() != 0 && part.owner.get().is_none())
        {
            return Err(Error::invalid(
                "materialized input belongs to another query account",
            ));
        }
        let bytes = self
            .bytes
            .get()
            .checked_add(part.bytes.get())
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        let owner = self.owner.get().or(part.owner.get());
        let part = source.take().expect("checked live input credit");
        part.into_disarmed();
        self.owner.set(owner);
        self.bytes.set(bytes);
        Ok(())
    }

    pub fn add(&self, bytes: u64) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        let owned = self
            .bytes
            .get()
            .checked_add(bytes)
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        if self.owner.get().is_none_or(|id| id == PRIMARY)
            && let Some(admitted) = self.ctx.fast_admit(bytes, true)
        {
            admitted?;
            self.owner.set(Some(PRIMARY));
            self.bytes.set(owned);
            return Ok(());
        }
        let ledger = self.ctx.memory();
        let owner = match self.owner.get() {
            Some(id) => ledger.owner_by_id(id),
            None => ledger.owner(self.ctx.grant())?,
        };
        ledger.reserve(&owner, self.ctx.mem_limit, bytes, false)?;
        self.owner.set(Some(owner.id));
        self.bytes.set(owned);
        Ok(())
    }
    /// Grow the charge to cover `bytes`, `step` bytes ahead when the budget and the pool
    /// allow it and exactly otherwise.
    pub(crate) fn cover(&self, bytes: u64, step: u64) -> Result<()> {
        let current = self.bytes.get();
        if bytes <= current {
            return Ok(());
        }
        match self.add(bytes.saturating_add(step).saturating_sub(current)) {
            Err(Error::BudgetExceeded(_)) if step > 0 => self.add(bytes - current),
            result => result,
        }
    }
    fn disarm(&self) -> Option<u64> {
        self.bytes.set(0);
        self.owner.take()
    }
    /// Transfer this charge's ownership without running `Drop` (and without invoking
    /// `trim_zero` or pool wakers when nothing was released).
    fn into_disarmed(self) -> Option<u64> {
        let owner = self.owner.take();
        self.bytes.set(0);
        std::mem::forget(self);
        owner
    }
    /// Transfer this already admitted physical allocation to an immutable output.
    /// The receiving context is the actual account owner, not a domain token.
    pub(super) fn into_output(self, ctx: &Arc<Ctx>) -> OutputCredit {
        assert!(
            std::ptr::eq(self.ctx, Arc::as_ptr(ctx)),
            "output account owner"
        );
        let bytes = self.bytes.get();
        let owner = self.into_disarmed();
        OutputCredit {
            ctx: ctx.clone(),
            owner,
            bytes,
            exact_receiving: false,
        }
    }

    /// Move query-only input ownership into the returned backing without a release
    /// interval. Existing nonalgorithm kernels retain their estimate policy.
    pub(crate) fn output_lease(&self, bytes: u64) -> Result<QueryMemoryLease> {
        if bytes > self.bytes.get() {
            self.add(bytes - self.bytes.get())?;
        } else {
            self.ctx
                .release_live(self.owner.get(), self.bytes.get() - bytes);
            self.bytes.set(bytes);
        }
        let ledger = self.ctx.memory();
        let owner = self
            .owner
            .get()
            .map(|id| ledger.owner_by_id(id))
            .map_or_else(|| ledger.owner(self.ctx.grant()), Ok)?;
        self.disarm();
        Ok(QueryMemoryLease {
            ledger: ledger.clone(),
            owner,
            bytes,
            service: None,
        })
    }
    pub(super) fn adopt(&self, slot: &mut Option<QueryMemoryLease>) -> Result<()> {
        let lease = slot.as_ref().expect("admitted input");
        if lease.service.is_some()
            || !Arc::ptr_eq(self.ctx.memory(), &lease.ledger)
            || self.owner.get().is_some_and(|id| id != lease.owner.id)
        {
            return Err(Error::invalid(
                "algorithm input belongs to another query owner",
            ));
        }
        let bytes = self
            .bytes
            .get()
            .checked_add(lease.bytes)
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        self.owner.set(Some(lease.owner.id));
        self.bytes.set(bytes);
        slot.take().expect("admitted input").bytes = 0;
        Ok(())
    }
}
/// Credit of a reused output row. The writer frees each row's terms before it decodes
/// the next, so live output is at most the largest row so far. The credit keeps that
/// high-water mark admitted and adds to the query's account only when a row exceeds it,
/// which leaves one shared account update per growth instead of one per cell.
pub(super) struct RowCredit<'a> {
    charge: Charge<'a>,
    used: std::cell::Cell<u64>,
}
impl<'a> RowCredit<'a> {
    pub(super) fn new(ctx: &'a Ctx) -> Self {
        Self {
            charge: Charge {
                ctx,
                bytes: std::cell::Cell::new(0),
                owner: std::cell::Cell::new(None),
            },
            used: std::cell::Cell::new(0),
        }
    }
    /// Admit `bytes` more for the current row before they are allocated.
    pub(super) fn add(&self, bytes: u64) -> Result<()> {
        let used = self
            .used
            .get()
            .checked_add(bytes)
            .ok_or_else(|| self.charge.ctx.memory_exceeded(u64::MAX))?;
        let held = self.charge.bytes.get();
        if used > held {
            self.charge.add(used - held)?;
        }
        self.used.set(used);
        Ok(())
    }
    /// Hold at least `bytes` in total, for scratch whose size replaces rather than
    /// adds to what was held before.
    pub(super) fn reserve(&self, bytes: u64) -> Result<()> {
        let held = self.charge.bytes.get();
        if bytes > held {
            self.charge.add(bytes - held)?;
        }
        Ok(())
    }
    /// Start a new row. The caller has already dropped every term of the last one.
    pub(super) fn reset(&self) {
        self.used.set(0);
    }
}

impl Drop for Charge<'_> {
    fn drop(&mut self) {
        self.ctx.release_live(self.owner.get(), self.bytes.get());
    }
}

/// Guard for optional vocabulary prefetch scratch admitted by
/// [`Ctx::admit_prefetch_scratch`]. In eager mode it holds a direct [`Charge`]; in
/// cursor mode it pins the ledger's active prefetch scope until dropped so idle
/// optional headroom can be reclaimed by required charges once scratch vectors are
/// freed.
pub(super) enum PrefetchScratch<'a> {
    Eager(Charge<'a>),
    Cursor {
        ctx: &'a Ctx,
        ledger: &'a QueryMemoryLedger,
        workers: u64,
    },
}

impl PrefetchScratch<'_> {
    pub(super) fn grow(&mut self, total_bytes: u64) -> Result<()> {
        match self {
            Self::Eager(charge) => charge.resize(total_bytes),
            Self::Cursor {
                ctx,
                ledger,
                workers,
            } => {
                ctx.check_memory_failure()?;
                let need = total_bytes.saturating_mul(*workers);
                let covered = ledger.envelope_reserved();
                if covered >= need {
                    return Ok(());
                }
                let extra = need - covered;
                if extra > ctx.memory_remaining() {
                    return Err(ctx.memory_exceeded(ctx.live_bytes().saturating_add(extra)));
                }
                ledger.grow_active_prefetch(ctx.grant(), ctx.mem_limit, need)
            }
        }
    }
}

impl Drop for PrefetchScratch<'_> {
    fn drop(&mut self) {
        if let Self::Cursor { ledger, .. } = self {
            ledger.end_prefetch();
        }
    }
}

/// Final physical output credit. Retaining the real context keeps its original
/// fast account or promoted ledger alive. Moving a paid charge here never returns
/// and reacquires its credit and never forces fast eager queries onto the ledger.
pub(super) struct OutputCredit {
    ctx: Arc<Ctx>,
    owner: Option<u64>,
    bytes: u64,
    exact_receiving: bool,
}
impl OutputCredit {
    pub(super) fn receiving(ctx: &Arc<Ctx>, bytes: u64) -> Result<Self> {
        // Ordinary parentless eager outputs keep their original lock-free account.
        if ctx.source_preparation.is_none() {
            return Ok(ctx.charge(bytes)?.into_output(ctx));
        }
        let ledger = ctx.memory();
        let owner = ledger.owner(ctx.grant())?;
        ledger.reserve_receiving(&owner, ctx.mem_limit, bytes)?;
        Ok(Self {
            ctx: ctx.clone(),
            owner: Some(owner.id),
            bytes,
            exact_receiving: true,
        })
    }
    /// Move a worker's already paid payload contribution without releasing it.
    /// Metadata buffers remain with their own original guard until they are freed.
    pub(super) fn absorb_charge(&mut self, slot: &mut Option<Charge<'_>>) -> Result<()> {
        let part = slot
            .as_ref()
            .ok_or_else(|| Error::invalid("missing worker credit"))?;
        if self.exact_receiving
            || !std::ptr::eq(Arc::as_ptr(&self.ctx), part.ctx)
            || matches!((self.owner, part.owner.get()), (Some(a), Some(b)) if a != b)
            || (part.bytes.get() != 0 && part.owner.get().is_none())
        {
            return Err(Error::invalid(
                "worker payload belongs to another query account",
            ));
        }
        let bytes = self
            .bytes
            .checked_add(part.bytes.get())
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        let part = slot.take().expect("validated worker credit");
        let owner = part.into_disarmed();
        self.owner = self.owner.or(owner);
        self.bytes = bytes;
        Ok(())
    }

    pub(super) fn belongs_to(&self, ctx: &Ctx) -> bool {
        std::ptr::eq(Arc::as_ptr(&self.ctx), ctx)
    }
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(super) fn check(&self) -> Result<()> {
        self.ctx.check()
    }
    pub(super) fn resize(&mut self, bytes: u64) -> Result<()> {
        if self.exact_receiving {
            let ledger = self.ctx.memory();
            let id = self.owner.expect("receiving owner");
            if bytes > self.bytes {
                ledger.reserve_receiving(
                    &ledger.owner_by_id(id),
                    self.ctx.mem_limit,
                    bytes - self.bytes,
                )?;
            } else if bytes < self.bytes {
                ledger.release_receiving(id, self.bytes - bytes);
            }
            self.bytes = bytes;
            return Ok(());
        }
        let held = Charge {
            ctx: &self.ctx,
            bytes: std::cell::Cell::new(self.bytes),
            owner: std::cell::Cell::new(self.owner),
        };
        let result = held.resize(bytes);
        let bytes = held.bytes();
        let owner = held.into_disarmed();
        self.bytes = bytes;
        self.owner = owner;
        result
    }
}
impl Drop for OutputCredit {
    fn drop(&mut self) {
        if self.exact_receiving {
            self.ctx
                .memory()
                .release_receiving(self.owner.expect("receiving owner"), self.bytes);
        } else {
            self.ctx.release_live(self.owner, self.bytes);
        }
    }
}

/// Weak retained state never owns the context, grant or ledger.
pub(crate) struct RetainedCharge {
    owner: Weak<Ctx>,
    id: Option<u64>,
    bytes: u64,
}
impl RetainedCharge {
    pub(super) fn owner(&self) -> Option<Arc<Ctx>> {
        self.owner.upgrade()
    }
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Transfer a previously validated live physical owner without returning its
    /// bytes. The caller must validate the actual context before consuming it.
    pub(super) fn into_output(mut self, ctx: &Arc<Ctx>) -> OutputCredit {
        assert!(
            std::ptr::eq(self.owner.as_ptr(), Arc::as_ptr(ctx)),
            "retained output account owner"
        );
        let bytes = std::mem::take(&mut self.bytes);
        let owner = self.id.take();
        self.owner = Weak::new();
        OutputCredit {
            ctx: ctx.clone(),
            owner,
            bytes,
            exact_receiving: false,
        }
    }
    pub(crate) fn resize(&mut self, bytes: u64) -> Result<()> {
        // A size that does not change takes neither the owner nor an account. A sort key
        // column of numbers resized its charge on every row this way.
        if bytes == self.bytes {
            return Ok(());
        }
        let ctx = self
            .owner
            .upgrade()
            .ok_or_else(|| Error::invalid("cursor context was released"))?;
        if bytes > self.bytes {
            // A charge on the same owner, which an eager context's lock-free account
            // takes without the ledger.
            let charge = Charge {
                ctx: &ctx,
                bytes: std::cell::Cell::new(0),
                owner: std::cell::Cell::new(self.id),
            };
            charge.add(bytes - self.bytes)?;
            self.id = charge.into_disarmed();
        } else {
            ctx.release_live(self.id, self.bytes - bytes);
        }
        self.bytes = bytes;
        if bytes == 0 {
            self.id = None;
        }
        Ok(())
    }

    /// Grow the charge to cover `bytes`, `step` bytes ahead when the budget and the pool
    /// allow it and exactly otherwise, so that state admitted one value at a time takes a
    /// charge once per step rather than once per value. The charge never falls below what
    /// it covers and holds at most `step` bytes more. A refusal changes nothing.
    pub(super) fn cover(&mut self, bytes: u64, step: u64) -> Result<()> {
        if bytes <= self.bytes {
            return Ok(());
        }
        match self.resize(bytes.saturating_add(step)) {
            Err(Error::BudgetExceeded(_)) if step > 0 => self.resize(bytes),
            result => result,
        }
    }

    /// An empty charge on this charge's owner in `ctx`, the context the charge belongs
    /// to, which a parallel piece of work grows on its own thread and
    /// [`RetainedCharge::absorb`] takes back.
    pub(super) fn part<'a>(&self, ctx: &'a Ctx) -> Charge<'a> {
        debug_assert!(
            std::ptr::eq(self.owner.as_ptr(), ctx),
            "the charge's context"
        );
        Charge {
            ctx,
            bytes: std::cell::Cell::new(0),
            owner: std::cell::Cell::new(self.id),
        }
    }

    /// Keep the bytes of `part`, a charge made by [`RetainedCharge::part`], with this
    /// charge, without releasing and charging them again.
    pub(super) fn absorb(&mut self, part: Charge<'_>) {
        let bytes = part.bytes.get();
        let id = part.into_disarmed();
        if bytes == 0 {
            return;
        }
        debug_assert!(self.id.is_none() || self.id == id, "one owner of a charge");
        if self.id.is_none() {
            self.id = id;
        }
        self.bytes = self.bytes.saturating_add(bytes);
    }

    /// Split up to `take` already-admitted bytes from this charge into a [`Charge`] on
    /// the same owner and admit any remaining shortfall up to `bytes`, without releasing
    /// or re-acquiring the reused bytes. If admitting the shortfall fails, nothing changes.
    pub(crate) fn split<'a>(&mut self, ctx: &'a Ctx, take: u64, bytes: u64) -> Result<Charge<'a>> {
        debug_assert!(
            std::ptr::eq(self.owner.as_ptr(), ctx),
            "the charge's context"
        );
        let take = take.min(self.bytes).min(bytes);
        let extra = bytes - take;
        let charge = self.part(ctx);
        if extra > 0 {
            charge.add(extra)?;
        }
        if take > 0 {
            self.bytes -= take;
            if self.bytes == 0 {
                self.id = None;
            }
            charge.bytes.set(charge.bytes.get() + take);
        }
        Ok(charge)
    }
}
impl Drop for RetainedCharge {
    fn drop(&mut self) {
        if let Some(ctx) = self.owner.upgrade() {
            ctx.release_live(self.id, self.bytes);
        }
    }
}

/// Cursor ownership can adopt an admitted algorithm lease without release/recharge.
pub(crate) struct OwnedCharge {
    ctx: Arc<Ctx>,
    lease: QueryMemoryLease,
}
impl OwnedCharge {
    pub(crate) fn bytes(&self) -> u64 {
        self.lease.bytes
    }
    pub(crate) fn new(ctx: &Arc<Ctx>, bytes: u64) -> Result<Self> {
        let ledger = ctx.memory();
        let owner = ledger.owner(ctx.grant())?;
        ledger.reserve(&owner, ctx.mem_limit, bytes, false)?;
        Ok(Self {
            ctx: ctx.clone(),
            lease: QueryMemoryLease {
                ledger: ledger.clone(),
                owner,
                bytes,
                service: None,
            },
        })
    }
    pub(super) fn adopt(ctx: &Arc<Ctx>, slot: &mut Option<QueryMemoryLease>) -> Result<Self> {
        let lease = slot.as_ref().expect("admitted output");
        if !Arc::ptr_eq(ctx.memory(), &lease.ledger) {
            return Err(Error::invalid("algorithm output belongs to another query"));
        }
        Ok(Self {
            ctx: ctx.clone(),
            lease: slot.take().expect("admitted output"),
        })
    }
    pub(crate) fn resize(&mut self, bytes: u64) -> Result<()> {
        self.lease.resize(self.ctx.mem_limit, bytes)
    }
    pub(super) fn cover(&mut self, bytes: u64, step: u64) -> Result<()> {
        if bytes <= self.lease.bytes {
            return Ok(());
        }
        let target = bytes.max(self.lease.bytes.saturating_add(step));
        if self.resize(target).is_err() {
            self.resize(bytes)?;
        }
        Ok(())
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.ctx.check()
    }
    /// Adopt the actual predecoded payload guard; the existing cursor ledger is
    /// required and checked before the source is consumed.
    pub(super) fn adopt_charge(&mut self, source: &mut Option<Charge<'_>>) -> Result<()> {
        let held = source
            .as_ref()
            .ok_or_else(|| Error::invalid("missing graph payload credit"))?;
        if !held.belongs_to(&self.ctx)
            || held.owner.get().is_some_and(|id| id != self.lease.owner.id)
            || (held.bytes() != 0 && held.owner.get().is_none())
        {
            return Err(Error::invalid(
                "graph payload belongs to another query account",
            ));
        }
        let bytes = self
            .lease
            .bytes
            .checked_add(held.bytes())
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        let held = source.take().expect("validated graph payload");
        held.into_disarmed();
        self.lease.bytes = bytes;
        Ok(())
    }
    pub(super) fn into_parts(self) -> (Arc<Ctx>, QueryMemoryLease) {
        (self.ctx, self.lease)
    }
}

fn key_kind(k: &[u8]) -> TermKind {
    if id::is_key_iri(k) {
        TermKind::Iri
    } else if id::is_key_triple(k) {
        TermKind::Triple
    } else {
        TermKind::Literal
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TermKind {
    None,
    Iri,
    BNode,
    Literal,
    /// RDF 1.2 triple term
    Triple,
}

#[cfg(test)]
mod charge_tests {
    use super::*;
    use crate::builder::{BuildOptions, VocabCodec};
    use crate::store::{Store, StoreOptions};

    /// Item 293: optional evaluation keeps a memory refusal of its own context out of the
    /// query's latch, and every other refusal, other contexts and the code after the
    /// scope still latch theirs.
    #[test]
    fn optional_eval_keeps_only_its_own_memory_refusals_out_of_the_latch() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let other = Ctx::new(store.snapshot());
        let memory = ctx.memory_exceeded(64);

        let ((), refused) = ctx.optional_eval(|| ctx.fail_memory(&memory));
        assert!(refused);
        ctx.check().unwrap();

        let ((), refused) = ctx.optional_eval(|| ());
        assert!(!refused);

        // A nested scope restores the outer one, which still sees its own refusal.
        let ((), refused) = ctx.optional_eval(|| {
            let ((), inner) = other.optional_eval(|| ());
            assert!(!inner);
            ctx.fail_memory(&memory);
        });
        assert!(refused);
        ctx.check().unwrap();

        let ((), refused) = ctx.optional_eval(|| other.fail_memory(&memory));
        assert!(!refused);
        assert!(matches!(
            other.check(),
            Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                ..
            }))
        ));
        ctx.check().unwrap();

        let ((), refused) = ctx.optional_eval(|| ctx.fail_memory(&Error::Cancelled));
        assert!(!refused);
        assert!(matches!(ctx.check(), Err(Error::Cancelled)));

        let fresh = Ctx::new(store.snapshot());
        let ((), refused) = fresh.optional_eval(|| ());
        assert!(!refused);
        fresh.fail_memory(&memory);
        assert!(matches!(
            fresh.check(),
            Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                ..
            }))
        ));
    }

    /// A panic inside optional evaluation restores the scope it replaced, so a later
    /// refusal outside any scope latches and one inside the outer scope stays there.
    #[test]
    fn optional_eval_restores_its_scope_when_the_evaluation_unwinds() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let memory = ctx.memory_exceeded(64);

        let ((), refused) = ctx.optional_eval(|| {
            let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ctx.optional_eval::<()>(|| panic!("optional evaluation unwinds"))
            }));
            assert!(unwound.is_err());
            ctx.fail_memory(&memory);
        });
        assert!(
            refused,
            "the outer scope is restored after the inner one unwinds"
        );
        ctx.check().unwrap();

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ctx.optional_eval::<()>(|| panic!("optional evaluation unwinds"))
        }));
        assert!(unwound.is_err());
        ctx.check().unwrap();
        ctx.fail_memory(&memory);
        assert!(matches!(
            ctx.check(),
            Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                ..
            }))
        ));
    }

    /// The scope belongs to the thread that opened it, so a refusal of the same context
    /// on another thread still latches.
    #[test]
    fn optional_eval_latches_refusals_of_its_context_on_other_threads() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let memory = ctx.memory_exceeded(64);

        let ((), refused) = ctx.optional_eval(|| {
            std::thread::scope(|scope| {
                scope.spawn(|| ctx.fail_memory(&memory)).join().unwrap();
            });
        });
        assert!(!refused, "another thread's refusal is not this scope's");
        assert!(matches!(
            ctx.check(),
            Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                ..
            }))
        ));
    }

    /// Item 293 round 4: a memory refusal that a scope keeps out of the latch still makes
    /// [`Ctx::has_memory_failure`] true for the rest of the scope on its thread, so
    /// catching expressions give up there, and false after the scope, on other threads
    /// and for other contexts.
    #[test]
    fn a_refused_optional_scope_reports_a_memory_failure_on_its_thread() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let other = Ctx::new(store.snapshot());
        let memory = ctx.memory_exceeded(64);

        let (inside, refused) = ctx.optional_eval(|| {
            assert!(!ctx.has_memory_failure());
            ctx.fail_memory(&memory);
            let elsewhere = std::thread::scope(|scope| {
                scope.spawn(|| ctx.has_memory_failure()).join().unwrap()
            });
            assert!(!elsewhere, "another thread has no refused scope");
            assert!(!other.has_memory_failure(), "another context has none");
            ctx.has_memory_failure()
        });
        assert!(refused);
        assert!(inside, "the refused scope reports its refusal");
        assert!(
            !ctx.has_memory_failure(),
            "the scope's refusal ends with it"
        );
        ctx.check().unwrap();

        // A scope of the same context inside a refused one starts refused.
        let ((inner, inner_refused), refused) = ctx.optional_eval(|| {
            ctx.fail_memory(&memory);
            ctx.optional_eval(|| ctx.has_memory_failure())
        });
        assert!(refused && inner && inner_refused);
        assert!(!ctx.has_memory_failure());

        // A scope of another context inside a refused one reports only its own.
        let ((inner, inner_refused), refused) = ctx.optional_eval(|| {
            ctx.fail_memory(&memory);
            other.optional_eval(|| (other.has_memory_failure(), ctx.has_memory_failure()))
        });
        assert!(refused && !inner_refused);
        assert_eq!(inner, (false, false));
        assert!(!ctx.has_memory_failure());
        ctx.check().unwrap();
        other.check().unwrap();
    }

    /// The count of refused scopes that gates the thread-local read of
    /// [`Ctx::has_memory_failure`] returns to zero when scopes close, unwind or nest.
    #[test]
    fn refused_optional_scopes_are_counted_until_they_close() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let other = Ctx::new(store.snapshot());
        let memory = ctx.memory_exceeded(64);

        let ((), refused) = ctx.optional_eval(|| ());
        assert!(!refused);
        assert_eq!(ctx.refused_scopes_for_test(), 0);

        let ((), refused) = ctx.optional_eval(|| {
            ctx.fail_memory(&memory);
            ctx.fail_memory(&memory);
            assert_eq!(ctx.refused_scopes_for_test(), 1, "a scope counts once");
        });
        assert!(refused);
        assert_eq!(ctx.refused_scopes_for_test(), 0);

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ctx.optional_eval::<()>(|| {
                ctx.fail_memory(&memory);
                panic!("optional evaluation unwinds")
            })
        }));
        assert!(unwound.is_err());
        assert_eq!(ctx.refused_scopes_for_test(), 0);
        assert!(!ctx.has_memory_failure());

        // Nested scopes of the same context: an inner scope that refuses on its own
        // counts itself, and one that starts refused does not count again.
        let ((), refused) = ctx.optional_eval(|| {
            let ((), inner) = ctx.optional_eval(|| ctx.fail_memory(&memory));
            assert!(inner);
            assert_eq!(ctx.refused_scopes_for_test(), 0);
            assert!(!ctx.has_memory_failure(), "the outer scope has not refused");
            ctx.fail_memory(&memory);
            let ((), inner) = ctx.optional_eval(|| {
                ctx.fail_memory(&memory);
                assert_eq!(ctx.refused_scopes_for_test(), 1);
            });
            assert!(inner);
            assert_eq!(ctx.refused_scopes_for_test(), 1);
            let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ctx.optional_eval::<()>(|| panic!("optional evaluation unwinds"))
            }));
            assert!(unwound.is_err());
            assert_eq!(ctx.refused_scopes_for_test(), 1);
            assert!(ctx.has_memory_failure());
        });
        assert!(refused);
        assert_eq!(ctx.refused_scopes_for_test(), 0);

        // Nested scopes of different contexts count on their own contexts.
        let ((), refused) = ctx.optional_eval(|| {
            ctx.fail_memory(&memory);
            let ((), inner) = other.optional_eval(|| {
                other.fail_memory(&memory);
                assert_eq!(other.refused_scopes_for_test(), 1);
                assert_eq!(ctx.refused_scopes_for_test(), 1);
            });
            assert!(inner);
            assert_eq!(other.refused_scopes_for_test(), 0);
            assert!(ctx.has_memory_failure());
        });
        assert!(refused);
        assert_eq!(ctx.refused_scopes_for_test(), 0);
        assert_eq!(other.refused_scopes_for_test(), 0);
        ctx.check().unwrap();
        other.check().unwrap();
    }

    #[test]
    fn eager_charge_transfer_keeps_the_actual_fast_credit_at_the_limit() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 12_288;
        let output = ctx.charge(4096).unwrap();
        let mut input = Some(ctx.charge(8192).unwrap());
        assert_eq!(ctx.live_bytes(), ctx.mem_limit);
        output.adopt_charge(&mut input).unwrap();
        assert!(input.is_none());
        assert_eq!(output.bytes.get(), 12_288);
        assert_eq!(ctx.live_bytes(), ctx.mem_limit);
        assert!(ctx.memory.get().is_none(), "transfer must not promote");
        output.resize(4096).unwrap();
        assert_eq!(ctx.live_bytes(), 4096);
        output.resize(8192).unwrap();
        assert_eq!(ctx.live_bytes(), 8192);
        drop(output);
        assert_eq!(ctx.live_bytes(), 0);
        assert!(ctx.memory.get().is_none());
    }

    #[test]
    fn eager_charge_transfer_rejects_a_different_context_on_the_same_grant() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(32 << 20);
        let grant = Arc::new(pool.empty_grant());
        let one = pooled(&store, &grant);
        let two = pooled(&store, &grant);
        let output = one.charge(4096).unwrap();
        let mut input = Some(two.charge(8192).unwrap());
        let used = grant.used();
        assert!(output.adopt_charge(&mut input).is_err());
        assert_eq!(output.bytes.get(), 4096);
        assert_eq!(input.as_ref().unwrap().bytes.get(), 8192);
        assert_eq!(one.live_bytes(), 4096);
        assert_eq!(two.live_bytes(), 8192);
        assert_eq!(grant.used(), used);
        assert!(one.memory.get().is_none() && two.memory.get().is_none());
    }

    #[test]
    fn eager_charge_transfer_refuses_empty_foreign_accounts_and_overflow_unchanged() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let output = ctx.charge(4096).unwrap();
        // These states exercise validation only; no fictitious credit is released.
        let foreign = Charge {
            ctx: &ctx,
            bytes: std::cell::Cell::new(0),
            owner: std::cell::Cell::new(Some(PRIMARY + 1)),
        };
        let mut input = Some(foreign);
        let refused = output.adopt_charge(&mut input);
        let foreign_owner = input.as_ref().unwrap().owner.get();
        input.as_ref().unwrap().disarm();
        assert!(refused.is_err());
        assert_eq!(foreign_owner, Some(PRIMARY + 1));
        assert_eq!(output.bytes.get(), 4096);
        let part = input.as_ref().unwrap();
        part.owner.set(Some(PRIMARY));
        part.bytes.set(u64::MAX);
        let refused = output.adopt_charge(&mut input);
        let source_bytes = input.as_ref().unwrap().bytes.get();
        input.as_ref().unwrap().disarm();
        assert!(refused.is_err());
        assert_eq!(source_bytes, u64::MAX);
        assert_eq!(output.bytes.get(), 4096);
        assert_eq!(ctx.live_bytes(), 4096);
        assert!(ctx.memory.get().is_none());
    }

    #[test]
    fn eager_charge_resize_refusal_preserves_the_admitted_storage() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 8192;
        let charge = ctx.charge(4096).unwrap();
        assert!(charge.resize(8193).is_err());
        assert_eq!(charge.bytes.get(), 4096);
        assert_eq!(ctx.live_bytes(), 4096);
        charge.resize(0).unwrap();
        assert_eq!(ctx.live_bytes(), 0);
        let mut empty = Some(ctx.charge(0).unwrap());
        charge.adopt_charge(&mut empty).unwrap();
        assert!(empty.is_none());
        assert!(ctx.memory.get().is_none());
    }

    /// The one-pass output decode builds the same term as `key_to_term` and admits
    /// the same bytes as `output_key_bytes`, for valid, damaged and special keys.
    #[test]
    fn one_pass_output_keys_match_the_two_pass_term_and_charge() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let sep = id::KEY_SEP;
        let mut keys: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"<".to_vec(),
            b"<http://example.org/a".to_vec(),
            "<http://example.org/\u{e9}".as_bytes().to_vec(),
            b"<http://example.org/\xff\xfe".to_vec(),
            b"\"".to_vec(),
            b"\"plain".to_vec(),
            b"x-bare".to_vec(),
            b"\xc3".to_vec(),
            b"_short".to_vec(),
        ];
        for lex in [&b"lex"[..], b"", "caf\u{e9}".as_bytes(), b"bad\xc3("] {
            let with = |suffix: &[u8]| {
                let mut key = vec![b'"'];
                key.extend_from_slice(lex);
                key.push(sep);
                key.extend_from_slice(suffix);
                key
            };
            keys.push(with(b""));
            keys.push(with(b"@en"));
            keys.push(with(b"@en--ltr"));
            keys.push(with(b"@ar--rtl"));
            keys.push(with(b"@e\xffn"));
            keys.push(with(b"^http://www.w3.org/2001/XMLSchema#integer"));
            keys.push(with(b"^http://example.org/\xc3"));
            keys.push(with("\u{e9}http://example.org/t".as_bytes()));
            keys.push(with(b"\xe9x"));
        }
        let mut bnode = vec![b'_'];
        bnode.extend_from_slice(&7u64.to_be_bytes());
        keys.push(bnode);
        let mut triple = vec![b'('];
        for part in [
            &b"<http://example.org/s"[..],
            b"<http://example.org/p",
            b"\"o",
        ] {
            triple.push(part.len() as u8);
            triple.extend_from_slice(part);
        }
        keys.push(triple);
        // The one-pass route takes every valid IRI and literal form, so that a fallback
        // cannot hide a disabled fast path, and leaves the others to the two-pass route.
        let one_pass = |key: &[u8]| id::TextKey::parse(key).is_some();
        let lit = |suffix: &[u8]| [&b"\"lex"[..], &[sep], suffix].concat();
        for key in [
            &b""[..],
            b"<http://example.org/a",
            "<http://example.org/\u{e9}".as_bytes(),
            b"x-bare",
            b"\"plain",
            &lit(b""),
            &lit(b"@en"),
            &lit(b"@ar--rtl"),
            &lit(b"^http://www.w3.org/2001/XMLSchema#integer"),
        ] {
            assert!(one_pass(key), "{key:?}");
        }
        for key in [
            &b"<http://example.org/\xff"[..],
            b"\xc3",
            b"_short",
            &keys[keys.len() - 2],
            &keys[keys.len() - 1],
            &lit(b"@e\xffn"),
            &lit(b"^http://example.org/\xc3"),
            &lit("\u{e9}http://example.org/t".as_bytes()),
            &[&b"\"bad\xc3("[..], &[sep]].concat(),
        ] {
            assert!(!one_pass(key), "{key:?}");
        }
        for key in &keys {
            let row = RowCredit::new(&ctx);
            let term = output_key_term(&ctx, key, &row).unwrap();
            assert_eq!(term, id::key_to_term(key), "{key:?}");
            assert_eq!(
                row.used.get(),
                output_key_bytes(&ctx, key).unwrap(),
                "{key:?}"
            );
        }
    }

    /// A decimal value gets the id of its canonical literal, inline or query-local.
    #[test]
    fn decimal_values_intern_as_their_canonical_literal() {
        use oxsdatatypes::Decimal;
        let store = Store::in_memory(StoreOptions::default());
        let context = Ctx::new(store.snapshot());
        let mut bits: Vec<i128> = vec![
            0,
            1,
            -1,
            1_950_000_000_000_000_000_000,
            -5_000_000_000_000_000_000,
            123_456_789,
            i128::MAX,
            i128::MIN,
            1 << 100,
        ];
        for scale in 0..=38 {
            let p = 10i128.pow(scale);
            bits.extend([p, -p, 7 * (p / 10)]);
            bits.extend(
                [1i128 << 55, (1 << 55) - 1, -(1 << 55), -(1 << 55) - 1]
                    .iter()
                    .filter_map(|m| m.checked_mul(p / 10)),
            );
        }
        let mut inline = 0;
        for v in bits {
            let value = Value::Decimal(Decimal::from_be_bytes(v.to_be_bytes()));
            let id = context.intern_value(&value);
            assert_eq!(id, context.intern_term(&value.to_term()), "{value:?}");
            inline += usize::from(id.tag() == Tag::Decimal);
            assert_eq!(context.term(id), Some(value.to_term()));
        }
        assert!(inline > 20, "most of the values are inline: {inline}");
    }

    /// The executors read the accounting mode in at most this many places outside tests.
    /// Stage 4 of item 453 lowered it as each twin merged, item 827 took the planning
    /// reads out so that a query plans alike in every mode, and item 1308 Wave 2 removed
    /// the eager decoding-envelope bypass. The routing of `query_execution_with_plan` and
    /// the EXISTS key set accumulated across batches remain, with the four reads of the
    /// flag that decide whether a cursor owner exists, whether a context without an owner
    /// may keep retained state uncharged, whether an empty release trims the ledger and
    /// whether a finished query's pool contribution shrinks.
    const MODE_CHECKS: usize = 6;

    /// Counts the reads of the accounting mode in the non-test sources of the SPARQL
    /// engine, and fails when they exceed [`MODE_CHECKS`] (decision D9 of item 453). A
    /// read is a call of `streams()` or `accounting()`, a use of `Accounting::Cursor` or
    /// `Accounting::Collect`, or a direct read of the `streams` field. The bodies of the
    /// two accessors and the one write of the field are not reads.
    #[test]
    fn the_accounting_mode_is_read_in_a_bounded_number_of_places() {
        fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    sources(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        /// Whether `line` reads the mode, as a word match so that `streams` inside a
        /// longer name does not count.
        fn reads_mode(line: &str) -> bool {
            let code = line.split("//").next().unwrap_or("");
            if code.contains(".streams()")
                || code.contains(".accounting()")
                || code.contains("Accounting::Cursor")
                || code.contains("Accounting::Collect")
            {
                return true;
            }
            let mut rest = code;
            while let Some(at) = rest.find(".streams") {
                let after = &rest[at + ".streams".len()..];
                let next = after.chars().next();
                let word_end = next.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                // `.streams = ` writes the flag, and `.streams()` was counted above
                let writes =
                    after.trim_start().starts_with("= ") && !after.trim_start().starts_with("==");
                if word_end && next != Some('(') && !writes {
                    return true;
                }
                rest = after;
            }
            false
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sparql");
        let mut files = Vec::new();
        sources(&root, &mut files);
        let mut sites = Vec::new();
        for file in files {
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            if name == "tests.rs" || name.ends_with("_tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            // a file's unit tests follow its code
            let code = text
                .find("\n#[cfg(test)]\nmod ")
                .map_or(text.as_str(), |end| &text[..end]);
            // the body of `fn accounting` and of `fn streams`, up to their closing brace
            let mut accessor = false;
            for (line, content) in code.lines().enumerate() {
                if name == "ctx.rs" {
                    if content.starts_with("    pub(super) fn streams(")
                        || content.starts_with("    pub(super) fn accounting(")
                    {
                        accessor = true;
                    }
                    if accessor {
                        accessor = content != "    }";
                        continue;
                    }
                }
                if reads_mode(content) {
                    sites.push(format!("{name}:{}", line + 1));
                }
            }
        }
        assert!(
            sites.len() <= MODE_CHECKS,
            "{} reads of the accounting mode, more than {MODE_CHECKS}: {sites:?}",
            sites.len()
        );
    }

    /// A program compiled before the context has an owner, as in eager planning, is
    /// uncharged, so the first attachment drops it and the next use charges it.
    #[test]
    fn regex_programs_compiled_before_the_owner_are_compiled_again_with_charges() {
        let store = Store::in_memory(StoreOptions::default());
        let context = Arc::new(Ctx::new(store.snapshot()));
        let before = context.live_bytes();
        let planned = context.regex("^ab+c$", "").unwrap().unwrap();
        assert_eq!(context.live_bytes(), before, "no owner, no retained charge");
        context.attach_owner();
        let executed = context.regex("^ab+c$", "").unwrap().unwrap();
        assert!(!Arc::ptr_eq(&planned, &executed));
        assert!(context.live_bytes() > before);
        let charged = context.live_bytes();
        // a second attachment keeps the charged programs
        context.attach_owner();
        let again = context.regex("^ab+c$", "").unwrap().unwrap();
        assert!(Arc::ptr_eq(&executed, &again));
        assert_eq!(context.live_bytes(), charged);
        drop((planned, executed, again));
        context.release_work();
        assert_eq!(context.live_bytes(), before);
    }

    #[test]
    fn retained_cursor_charges_release_and_do_not_keep_context_alive() {
        let store = Store::in_memory(StoreOptions::default());
        let mut context = Ctx::new(store.snapshot());
        context.enable_cursor_accounting().unwrap();
        let context = Arc::new(context);
        context.attach_owner();
        let before = context.memory().live.load(Ordering::Relaxed);
        let mut retained = context.retained_charge(4096).unwrap().unwrap();
        assert_eq!(context.memory().live.load(Ordering::Relaxed), before + 4096);
        retained.resize(2048).unwrap();
        assert_eq!(context.memory().live.load(Ordering::Relaxed), before + 2048);
        drop(retained);
        assert_eq!(context.memory().live.load(Ordering::Relaxed), before);
        let retained = context.retained_charge(4096).unwrap();
        let weak = Arc::downgrade(&context);
        drop(context);
        assert!(weak.upgrade().is_none());
        drop(retained);
    }

    /// A retained charge resized to its own size touches neither its owner nor an
    /// account, and one that covers values a step at a time grows a step ahead, takes the
    /// exact bytes when the step does not fit, and changes nothing when even those do not.
    #[test]
    fn retained_charges_cover_values_in_steps() {
        let store = Store::in_memory(StoreOptions::default());
        let mut context = Ctx::new(store.snapshot());
        context.mem_limit = 1 << 20;
        let context = Arc::new(context);
        context.attach_owner();
        let step = context.reservation_step();
        assert_eq!(step, (1 << 20) / 64);
        let base = context.live_bytes();
        let mut retained = context.retained_charge(100).unwrap().unwrap();
        retained.cover(60, step).unwrap();
        assert_eq!(retained.bytes(), 100);
        retained.cover(200, step).unwrap();
        assert_eq!(retained.bytes(), 200 + step);
        assert_eq!(context.live_bytes(), base + 200 + step);
        retained.cover(200 + step, step).unwrap();
        assert_eq!(retained.bytes(), 200 + step);
        // without room for the step, the exact bytes
        let room = context.memory_remaining();
        let storage = context.charge(room - 1000).unwrap();
        let held = retained.bytes();
        retained.cover(held + 500, step).unwrap();
        assert_eq!(retained.bytes(), held + 500);
        // without room for those, nothing changes
        assert!(matches!(
            retained.cover(held + 2000, step),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(retained.bytes(), held + 500);
        drop(storage);
        // a part grown on another charge joins it without a second charge
        let part = retained.part(&context);
        part.add(300).unwrap();
        let live = context.live_bytes();
        retained.absorb(part);
        assert_eq!(retained.bytes(), held + 800);
        assert_eq!(context.live_bytes(), live);
        // the same size needs no owner, even one that is gone
        let bytes = retained.bytes();
        let weak = Arc::downgrade(&context);
        drop(context);
        assert!(weak.upgrade().is_none());
        retained.resize(bytes).unwrap();
        drop(retained);
    }

    #[test]
    fn rejected_finite_and_unlimited_overflow_leave_accounting_reusable() {
        for limit in [100, u64::MAX] {
            let store = Store::in_memory(StoreOptions::default());
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = limit;
            let charge = ctx.charge(80).unwrap();
            assert!(charge.add(u64::MAX).is_err());
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 80);
            assert_eq!(charge.bytes.get(), 80);
            charge.add(20).unwrap();
            drop(charge);
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
            // A different guard can overflow the global count while its own count
            // is representable. Failure must not corrupt either guard's release.
            let first = ctx.charge(limit).unwrap();
            assert!(ctx.charge(1).is_err());
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), limit);
            drop(first);
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
            drop(ctx.charge(1).unwrap());
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn concurrent_rejections_do_not_wrap_or_steal_live_charges() {
        for limit in [100, u64::MAX] {
            let store = Store::in_memory(StoreOptions::default());
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = limit;
            let owner = ctx.charge(limit - 10).unwrap();
            let barrier = std::sync::Barrier::new(8);
            std::thread::scope(|scope| {
                for _ in 0..8 {
                    let ctx = &ctx;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        for _ in 0..500 {
                            assert!(ctx.charge(11).is_err());
                            assert!(ctx.charge(u64::MAX).is_err());
                            drop(ctx.charge(1).unwrap());
                        }
                    });
                }
            });
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), limit - 10);
            drop(owner);
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
        }
    }

    const MIB: u64 = 1 << 20;

    fn pooled(store: &Store, grant: &Arc<super::super::pool::PoolGrant>) -> Ctx {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.set_grant(Some(grant.clone()));
        ctx
    }

    #[test]
    fn a_zero_charge_context_keeps_one_grant_reference_and_does_not_touch_its_credit() {
        use super::super::pool::{GRANT_RELEASES, QueryPool};
        let store = Store::in_memory(StoreOptions::default());
        let pool = QueryPool::new(64 << 10);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = Ctx::new(store.snapshot());
        let releases = GRANT_RELEASES.with(|n| n.get());
        let grows = super::FAST_GROWS.with(|n| n.get());
        let ledger_ops = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        let (_, stats) = crate::test_alloc::record_alloc_stats(|| {
            ctx.set_grant(Some(grant.clone()));
            assert_eq!(Arc::strong_count(&grant), 2);
            drop(ctx.charge(0).unwrap());
            ctx.check_output(0, 0).unwrap();
            drop(ctx);
        });
        assert_eq!(stats.total_alloc, 0);
        assert_eq!(super::FAST_GROWS.with(|n| n.get()), grows);
        assert_eq!(GRANT_RELEASES.with(|n| n.get()), releases);
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            ledger_ops
        );
        assert_eq!((grant.used(), pool.granted()), (0, 0));
    }

    /// A plain query's estimate grows a few bytes at a time. The fast account admits a
    /// step from the grant at once, so the next small increases take neither its lock
    /// nor the grant's counters.
    #[test]
    fn small_eager_growth_admits_one_step_from_the_grant() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.try_grant(16 * MIB).unwrap());
        let ctx = pooled(&store, &grant);
        let before = super::FAST_GROWS.with(|n| n.get());
        let a = ctx.charge(8).unwrap();
        let b = ctx.charge(24).unwrap();
        ctx.check_output(100, 2).unwrap();
        assert_eq!(super::FAST_GROWS.with(|n| n.get()) - before, 1);
        assert_eq!(grant.used(), EAGER_STEP);
        let c = ctx.charge(EAGER_STEP).unwrap();
        assert_eq!(grant.used(), 2 * EAGER_STEP);
        assert_eq!(super::FAST_GROWS.with(|n| n.get()) - before, 2);
        assert_eq!(ctx.mem_peak(), 32 + EAGER_STEP);
        drop((a, b, c));
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// The step never takes the contribution past the local budget, and a pool that
    /// cannot give the step still admits the exact estimate.
    #[test]
    fn the_eager_step_stays_within_the_budget_and_the_pool() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 1000;
        ctx.set_grant(Some(grant.clone()));
        drop(ctx.charge(10).unwrap());
        assert_eq!(grant.used(), 1000);
        drop(ctx);
        assert_eq!(grant.used(), 0);

        let small = super::super::pool::QueryPool::new(100_000);
        let grant = Arc::new(small.empty_grant());
        let ctx = pooled(&store, &grant);
        let held = ctx.charge(70_000).unwrap();
        assert_eq!(grant.used(), 70_000);
        assert!(matches!(ctx.charge(40_000), Err(Error::BudgetExceeded(b))
            if b.kind == BudgetKind::ServerMemory));
        assert_eq!(grant.used(), 70_000);
        drop(held);
        drop(ctx);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(small.granted(), 0);
    }

    #[test]
    fn a_fitting_eager_output_remains_owned_until_context_drop() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.try_grant(100 * MIB).unwrap());
        let ctx = pooled(&store, &grant);
        let held = ctx.charge(80 * MIB).unwrap();
        ctx.check_output((20 * MIB / 8) as usize, 1).unwrap();
        drop(held);
        assert_eq!(grant.used(), 100 * MIB);
        assert!(grant.granted() >= 100 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
        assert_eq!(grant.granted(), 16 * MIB);
    }

    /// `n` distinct query-local simple literals and their texts.
    fn local_literals(ctx: &Ctx, n: usize) -> Vec<(Id, String)> {
        (0..n)
            .map(|i| {
                let text = format!("literal number {i}");
                let term = Term::Literal(oxrdf::Literal::new_simple_literal(text.clone()));
                let id = ctx.intern_term(&term);
                assert_eq!(id.tag(), Tag::Local);
                (id, text)
            })
            .collect()
    }

    fn assert_text(value: Option<Value>, text: &str) {
        match value {
            Some(Value::Str(s)) => assert_eq!(&*s, text),
            other => panic!("{other:?} for {text:?}"),
        }
    }

    /// Cached values are charged to the query, and a finished eager result keeps
    /// their reservation in its pool contribution.
    #[test]
    fn cached_values_are_charged_and_kept_by_a_finished_result() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(256 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let ctx = pooled(&store, &grant);
        let literals = local_literals(&ctx, 2_000);
        for (id, text) in &literals {
            assert_text(ctx.value(*id), text);
        }
        let reserved = ctx.values.reserved();
        assert!(reserved >= 2_000 * 40, "{reserved} bytes reserved");
        assert!(ctx.live_bytes() >= reserved);
        assert!(grant.used() >= reserved);
        ctx.shrink_retained(0).unwrap();
        assert!(grant.used() >= reserved, "{} of {reserved}", grant.used());
        // a second read hits the cache and reserves nothing more
        for (id, text) in &literals {
            assert_text(ctx.value(*id), text);
        }
        assert_eq!(ctx.values.reserved(), reserved);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// A cursor keeps the values cached before its transition and the charge of
    /// them, and caches repeated values with one reservation step per 64 KiB.
    #[test]
    fn a_cursor_caches_repeated_values_in_steps() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(256 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = pooled(&store, &grant);
        ctx.mem_limit = 64 * MIB;
        let literals = local_literals(&ctx, 1_000);
        assert_text(ctx.value(literals[0].0), &literals[0].1);
        let prepared = ctx.values.reserved();
        assert!(prepared > 0);
        ctx.enable_cursor_accounting().unwrap();
        assert_eq!(
            ctx.values.reserved(),
            prepared,
            "the transition keeps the cache"
        );
        assert!(ctx.memory().live.load(Ordering::SeqCst) >= prepared);
        let before = super::VALUE_GROWS.with(|n| n.get());
        for i in 0..100_000 {
            let (id, text) = &literals[i % literals.len()];
            assert_text(ctx.value(*id), text);
        }
        let grows = super::VALUE_GROWS.with(|n| n.get()) - before;
        let reserved = ctx.values.reserved();
        assert!(
            grows as u64 <= reserved / EAGER_STEP + 1,
            "{grows} growths for {reserved} bytes"
        );
        assert!(reserved <= ValueCache::cap(ctx.mem_limit));
        assert!(ctx.live_bytes() <= ctx.mem_limit);
        assert!(ctx.check().is_ok());
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// A budget that cannot hold the cache's reservation leaves every value bound and
    /// uncached, in both accounting modes, and records no failure.
    #[test]
    fn a_refused_cache_reservation_leaves_values_bound() {
        let store = Store::in_memory(StoreOptions::default());
        for cursor in [false, true] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = MIB;
            let literals = local_literals(&ctx, 200);
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let held = ctx.charge(MIB - ctx.live_bytes() - 10_000).unwrap();
            for (id, text) in &literals {
                assert_text(ctx.value(*id), text);
            }
            assert_eq!(ctx.values.reserved(), 0, "cursor {cursor}");
            assert!(ctx.check().is_ok(), "cursor {cursor}");
            drop(held);
        }
    }

    /// A required (`exact`) reservation under tight remaining budget caps its speculative
    /// lookahead slack so a small retained charge leaves headroom for batch publication
    /// and row decoding instead of taking a full `reservation_step()`.
    #[test]
    fn tight_retained_reservation_caps_speculative_slack() {
        let store = Store::in_memory(StoreOptions::default());
        for cursor in [false, true] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = 1100 << 10;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let step = ctx.reservation_step();
            let headroom = step + 800;
            let held = ctx
                .charge(ctx.mem_limit - ctx.live_bytes() - headroom)
                .unwrap();
            assert!(ctx.retain_bytes(148), "cursor {cursor}");
            let remaining = ctx.mem_limit - ctx.live_bytes();
            assert!(
                remaining >= headroom / 2,
                "cursor {cursor}: remaining {remaining} must keep at least half of {headroom}"
            );
            assert!(ctx.check().is_ok(), "cursor {cursor}");
            drop(held);
        }
    }

    /// Decoding a term reserves the decode envelope in both accounting modes, and the end
    /// of the work releases it. A cursor refuses a decode that the budget cannot hold,
    /// gives no term and fails the next check, so a refused decode never becomes an
    /// unbound value. Eager callers cannot report a refusal, so an eager context decodes
    /// without the envelope then.
    #[test]
    fn refused_decode_envelopes_fail_the_next_check() {
        let store = Store::in_memory(StoreOptions::default());
        for cursor in [false, true] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = MIB;
            let literals = local_literals(&ctx, 2);
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let live = ctx.live_bytes();
            assert!(ctx.term(literals[0].0).is_some(), "cursor {cursor}");
            assert!(
                ctx.live_bytes() > live,
                "cursor {cursor}: the envelope is charged"
            );
            ctx.release_work();
            assert_eq!(ctx.live_bytes(), live, "cursor {cursor}: and released");
            let held = ctx.charge(MIB - live - 100).unwrap();
            assert_eq!(ctx.term(literals[1].0), None, "cursor {cursor}");
            assert!(
                matches!(ctx.check(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
                "cursor {cursor}"
            );
            drop(held);
        }
    }

    /// A floor table's words tell apart the ids that share a slot, mark an id that is not
    /// a simple literal, hold a floor as far as their distance bits reach and no farther,
    /// and keep the bit of a floor read again only for that floor.
    #[test]
    fn floor_table_words_hold_one_id_of_a_slot() {
        // 16 slots below 2^59 leave the top part 57 bits and the distance 6, so a floor
        // 62 ids down is the farthest kept
        let first_triple = 1u64 << 59;
        let t = FloorTable::new(16, first_triple);
        assert_eq!(t.none, 63);
        let p = first_triple - 1;
        let q = p - 16;
        assert_eq!(t.get(p), None);
        assert!(t.put(p, Some(p - 62), false));
        assert_eq!(t.get(p), Some(Some(p - 62)));
        assert_eq!(t.get(q), None, "the other id of the slot");
        assert!(!t.put(q, Some(q - 63), false), "too far to hold");
        assert_eq!(t.get(p), Some(Some(p - 62)));
        assert!(t.put(q, None, true));
        assert_eq!(t.get(q), Some(None));
        assert_eq!(t.get(p), None, "evicted");
        let word = t.slot(q).load(Ordering::Relaxed);
        assert_eq!(word & 1, 1, "read again");
        assert!(t.put(q, Some(q), false));
        assert_eq!(t.get(q), Some(Some(q)));
        assert_eq!(t.entries(), 1);
        // a hit's store of its read bit that lands after another id's store puts back the
        // first id's word, which is still that id's floor
        let a = p;
        let b = p - 16;
        assert!(t.put(a, Some(a - 5), false));
        let seen = t.slot(a).load(Ordering::Relaxed);
        assert!(t.put(b, Some(b - 7), false));
        t.slot(a).store(seen | 1, Ordering::Relaxed);
        assert_eq!(t.get(a), Some(Some(a - 5)));
        assert_eq!(t.get(b), None);
        // the widest ids keep a distance of at least two bits in any table, and a floor
        // read from one table is kept by a larger one with wider distances
        let top = (1u64 << crate::id::PAYLOAD_BITS) - 1;
        for log in [0, 1, 4, 10, 16, 23] {
            let t = FloorTable::new(1 << log, top);
            assert!(t.none >= 3, "{log}");
            let p = top - 1;
            assert!(t.put(p, Some(p - 2), true));
            assert_eq!(t.get(p), Some(Some(p - 2)), "{log}");
            assert!(t.put(p, None, false));
            assert_eq!(t.get(p), Some(None), "{log}");
        }
        let (small, large) = (
            FloorTable::new(16, first_triple),
            FloorTable::new(1 << 20, first_triple),
        );
        assert!(large.none > small.none);
        let far = small.none - 1;
        assert!(small.put(p, Some(p - far), false));
        let floor = small.get(p).unwrap();
        assert!(large.put(p, floor, true));
        assert_eq!(large.get(p), Some(Some(p - far)));
        // a small vocabulary leaves the top part one bit, as every id is below the slots
        let t = FloorTable::new(1 << 10, 1000);
        assert_eq!(t.none, (1 << 62) - 1);
        for p in [0, 1, 999] {
            assert!(t.put(p, Some(0), false));
            assert_eq!(t.get(p), Some(Some(0)));
        }
        // copying a smaller table into a larger one preserves every floor, non-literal
        // marker and read bit under the wider slot layout
        let small = FloorTable::new(16, first_triple);
        let samples = [
            (0u64, Some(0), false),
            (17, Some(15), true),
            (35, None, true),
            (
                first_triple - 1,
                Some(first_triple - 1 - (small.none - 1)),
                false,
            ),
            (first_triple - 5, None, false),
        ];
        for &(id, floor, read) in &samples {
            assert!(small.put(id, floor, read));
        }
        let mut larger = FloorTable::new(128, first_triple);
        small.copy_into(&mut larger);
        assert_eq!(larger.entries(), samples.len());
        for &(id, floor, read) in &samples {
            assert_eq!(larger.get(id), Some(floor), "id {id}");
            let word = larger.slot(id).load(Ordering::Relaxed);
            assert_eq!(word & 1 == 1, read, "read bit for id {id}");
        }
    }

    /// The floor of every stored string is the same in both modes and is the first id
    /// whose key starts with its lexical form. Both modes reserve the keys a floor reads
    /// and the search for it, so a long string refuses once the budget is held, and the
    /// refusal fails the next check.
    #[test]
    fn string_floors_agree_across_modes_and_reserve_their_reads_in_both_modes() {
        use crate::io::Source;
        use oxrdfio::RdfFormat;
        let store = Store::in_memory(StoreOptions::default());
        let long = "a".repeat(256 << 10);
        let mut nt = String::new();
        for (i, w) in ["", "a", "ab", "abc", "b", "ba", "é", "z"]
            .into_iter()
            .chain([long.as_str(), "abé"])
            .enumerate()
        {
            nt.push_str(&format!("<urn:s:{i}> <urn:p> \"{w}\" .\n"));
        }
        nt.push_str("<urn:s:n> <urn:p> \"5\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n");
        nt.push_str("<urn:s:l> <urn:p> \"ab\"@en .\n");
        store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let v = &snap.generation.vocab;
        let ids: Vec<u64> = (0..v.first_triple()).collect();
        let expected: Vec<Option<u64>> = ids
            .iter()
            .map(|&p| {
                let key = v.get(p).unwrap();
                if key.len() < 2 || key[0] != b'"' || key[key.len() - 1] != id::KEY_SEP {
                    return None;
                }
                let prefix = &key[..key.len() - 1];
                ids.iter()
                    .copied()
                    .find(|&q| v.get(q).unwrap().starts_with(prefix))
            })
            .collect();
        assert!(
            expected
                .iter()
                .zip(&ids)
                .any(|(f, p)| f.is_some_and(|f| f < *p)),
            "a string extends another"
        );
        for cursor in [false, true] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = 64 * MIB;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let floors: Vec<Option<u64>> = ids
                .iter()
                .map(|&p| ctx.string_floor(Id::vocab(p)))
                .collect();
            assert_eq!(floors, expected, "cursor {cursor}");
            assert!(ctx.check().is_ok(), "cursor {cursor}");
        }
        let long_id = ids
            .iter()
            .copied()
            .find(|&p| v.get(p).unwrap().len() > 1000)
            .unwrap();
        for cursor in [false, true] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = MIB;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let held = ctx.charge(MIB - ctx.live_bytes() - 100).unwrap();
            let floor = ctx.string_floor(Id::vocab(long_id));
            assert_eq!(floor, None, "cursor {cursor}");
            assert!(
                matches!(ctx.check(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
                "cursor {cursor}"
            );
            drop(held);
        }
    }

    fn store_with_floor_literals(literals: &[oxrdf::Term], codec: VocabCodec) -> Store {
        use std::fmt::Write;

        let store = Store::in_memory(StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(codec),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut nt = String::new();
        for literal in literals {
            writeln!(nt, "<urn:s> <urn:p> {literal} .").unwrap();
        }
        store
            .load(&[crate::io::Source::from_bytes(
                nt.into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        assert_eq!(store.snapshot().generation.vocab_codec(), codec);
        store
    }

    #[test]
    fn a_cursor_floor_reads_a_long_literal_without_reading_its_huge_predecessor() {
        let literals = [
            oxrdf::Literal::new_simple_literal(format!("a{}", "x".repeat(1 << 20))).into(),
            oxrdf::Literal::new_simple_literal(format!("b{}", "y".repeat(5 << 10))).into(),
        ];
        for codec in [VocabCodec::Front, VocabCodec::Fsst] {
            let store = store_with_floor_literals(&literals, codec);
            let snap = store.snapshot();
            let [huge, long] = literals
                .each_ref()
                .map(|term| snap.lookup_term(term).unwrap());
            assert_eq!(long.payload(), huge.payload() + 1);
            assert!(!(long.payload() as usize).is_multiple_of(crate::vocab::FC_BLOCK));

            let mut ctx = Ctx::new(snap);
            ctx.mem_limit = 2 * MIB;
            ctx.enable_cursor_accounting().unwrap();
            assert_eq!(ctx.string_floor(long), Some(long.payload()), "{codec:?}");
            assert!(ctx.check().is_ok());
            assert_eq!(ctx.with_vocab_key(huge.payload(), |_| ()), None);
            assert!(
                matches!(ctx.check(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
            );
        }
    }

    #[test]
    fn string_floors_use_adjacent_prefixes_in_both_codecs() {
        use oxrdf::{BaseDirection, Literal, NamedNode};

        let literals = [
            Literal::new_language_tagged_literal("ab", "en")
                .unwrap()
                .into(),
            Literal::new_simple_literal("a").into(),
            Literal::new_directional_language_tagged_literal("bb", "ar", BaseDirection::Rtl)
                .unwrap()
                .into(),
            Literal::new_simple_literal("b").into(),
            Literal::new_typed_literal("cb", NamedNode::new("urn:dt").unwrap()).into(),
            Literal::new_simple_literal("c").into(),
            Literal::new_simple_literal("dè").into(),
            Literal::new_simple_literal("dé").into(),
        ];
        for codec in [VocabCodec::Front, VocabCodec::Fsst] {
            let store = store_with_floor_literals(&literals, codec);
            let snap = store.snapshot();
            let ids = literals
                .each_ref()
                .map(|term| snap.lookup_term(term).unwrap());
            for pair in ids.as_chunks::<2>().0 {
                assert_eq!(pair[1].payload(), pair[0].payload() + 1);
                assert!(!(pair[1].payload() as usize).is_multiple_of(crate::vocab::FC_BLOCK));
            }
            for cursor in [false, true] {
                let mut ctx = Ctx::new(snap.clone());
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                for pair in ids[..6].as_chunks::<2>().0 {
                    assert_eq!(ctx.string_floor(pair[0]), None);
                    assert_eq!(ctx.string_floor(pair[1]), Some(pair[0].payload()));
                }
                // These keys share the first byte of è and é, but neither extends the other.
                assert_eq!(ctx.string_floor(ids[6]), Some(ids[6].payload()));
                assert_eq!(ctx.string_floor(ids[7]), Some(ids[7].payload()));
                assert_eq!(
                    ctx.string_floor(Id::vocab(snap.generation.vocab.len())),
                    None
                );
                assert!(ctx.check().is_ok(), "{codec:?} cursor {cursor}");
            }
        }
    }

    #[test]
    fn reused_floor_and_direct_cmp_scratch_matches_single_shot_and_refuses_strictly() {
        use oxrdf::{Literal, NamedNode};

        let long = format!("z{}", "a".repeat(256 << 10));
        let mut literals: Vec<oxrdf::Term> = Vec::new();
        for i in 0..40 {
            literals.push(Literal::new_simple_literal(format!("w{i:02}")).into());
            literals.push(Literal::new_simple_literal(format!("w{i:02}ext")).into());
        }
        literals.push(
            Literal::new_language_tagged_literal("w05", "en")
                .unwrap()
                .into(),
        );
        literals.push(Literal::new_typed_literal("w06", NamedNode::new_unchecked("urn:dt")).into());
        literals.push(Literal::new_simple_literal(&long).into());

        for codec in [VocabCodec::Front, VocabCodec::Fsst] {
            let store = store_with_floor_literals(&literals, codec);
            let snap = store.snapshot();
            let ids: Vec<Id> = literals
                .iter()
                .map(|t| snap.lookup_term(t).unwrap())
                .collect();
            let long_id = *ids.last().unwrap();
            let lang_id = ids[ids.len() - 3];
            let typed_id = ids[ids.len() - 2];
            let iri_id = snap
                .lookup_term(&NamedNode::new_unchecked("urn:s").into())
                .unwrap();

            for cursor in [false, true] {
                let mut ctx = Ctx::new(snap.clone());
                ctx.mem_limit = 64 * MIB;
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                let mut scratch = FloorScratch::default();
                for &id in &ids {
                    assert_eq!(
                        ctx.string_floor_in(id, &mut scratch),
                        ctx.string_floor(id),
                        "{codec:?} cursor {cursor}"
                    );
                }
                for &a in &ids[..12] {
                    assert_eq!(
                        ctx.simple_literal_cmp_in(a, a, &mut scratch),
                        Some(std::cmp::Ordering::Equal)
                    );
                    for &b in &ids[..12] {
                        assert_eq!(
                            ctx.simple_literal_cmp_in(a, b, &mut scratch),
                            ctx.simple_literal_cmp(a, b),
                            "{codec:?} cursor {cursor}"
                        );
                    }
                }
                assert_eq!(
                    ctx.simple_literal_cmp_in(lang_id, lang_id, &mut scratch),
                    None
                );
                assert_eq!(
                    ctx.simple_literal_cmp_in(typed_id, typed_id, &mut scratch),
                    None
                );
                assert_eq!(
                    ctx.simple_literal_cmp_in(iri_id, iri_id, &mut scratch),
                    None
                );
                assert_eq!(
                    ctx.simple_literal_cmp_in(Id::UNDEF, ids[0], &mut scratch),
                    None
                );
                assert!(ctx.check().is_ok());
            }

            // Strict left-to-right memory refusal in cursor mode.
            let mut ctx = Ctx::new(snap.clone());
            ctx.mem_limit = MIB;
            ctx.enable_cursor_accounting().unwrap();
            let held = ctx.charge(MIB - ctx.live_bytes() - 4096).unwrap();
            let mut scratch = FloorScratch::default();
            // Ineligible left literal does not decode the budget-exceeding right literal.
            assert_eq!(
                ctx.simple_literal_cmp_in(lang_id, long_id, &mut scratch),
                None
            );
            assert!(
                ctx.check().is_ok(),
                "{codec:?}: right operand must not be decoded"
            );
            // Budget-exceeding left literal records its refusal even when right is an IRI.
            assert_eq!(
                ctx.simple_literal_cmp_in(long_id, iri_id, &mut scratch),
                None
            );
            assert!(
                matches!(ctx.check(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
                "{codec:?}: left decode refusal must be recorded"
            );
            drop(held);
        }
    }

    #[test]
    fn cursor_fsst_lookup_succeeds_when_optional_room_is_refused() {
        use oxrdf::Literal;

        let mid_pad = "alpha_beta_gamma_delta_".repeat(5);
        let long_pad = "alpha_beta_gamma_delta_".repeat(8);
        let mut literals: Vec<oxrdf::Term> = (0..160u32)
            .map(|i| {
                Literal::new_simple_literal(format!("m-{mid_pad}delta_tail_{i:04}_alpha_beta"))
                    .into()
            })
            .collect();
        for i in 0..32u32 {
            literals.push(
                Literal::new_simple_literal(format!("z-{long_pad}delta_tail_{i:02}_alpha_beta"))
                    .into(),
            );
        }
        let store = store_with_floor_literals(&literals, VocabCodec::Fsst);
        let snap = store.snapshot();
        assert!(snap.generation.vocab.is_fsst());

        // Pick a verified FSST-coded simple literal with at most 8 suffix codes (`fast_room`)
        // whose full vocab key fits in 192 bytes while `key.len() + 64` exceeds 192 bytes.
        let (target_term, target_id) = literals
            .iter()
            .find_map(|term| {
                let id = snap.lookup_term(term).unwrap();
                let p = id.payload();
                let key = snap.generation.vocab.get(p).unwrap();
                (key.len() > 128
                    && key.len() <= 192
                    && !(p as usize).is_multiple_of(crate::vocab::FC_BLOCK)
                    && snap
                        .generation
                        .vocab
                        .entry_coded_len(p)
                        .is_some_and(|codes| codes <= 8))
                .then(|| (term.clone(), id))
            })
            .expect(
                "fixture must contain a verified FSST-coded simple literal in (128, 192] with <= 8 codes",
            );

        // Pick a verified FSST-coded simple literal longer than 192 bytes with at most 8 codes.
        let (grown_id, grown_len) = literals
            .iter()
            .find_map(|term| {
                let id = snap.lookup_term(term).unwrap();
                let p = id.payload();
                let key = snap.generation.vocab.get(p).unwrap();
                (key.len() > 192
                    && !(p as usize).is_multiple_of(crate::vocab::FC_BLOCK)
                    && snap
                        .generation
                        .vocab
                        .entry_coded_len(p)
                        .is_some_and(|codes| codes <= 8))
                .then_some((id, key.len()))
            })
            .expect(
                "fixture must contain a verified FSST-coded simple literal > 192 bytes with <= 8 codes",
            );

        // Outside Rayon, `Ctx::decode_workers() == 1`, so `n` bytes needs
        // `(n * 8 + 1024)` bytes while optional 64-byte FSST headroom exceeds it.
        let room_for = |bytes: usize| (bytes as u64 * 8 + 1024) * Ctx::decode_workers();
        let room_for_192 = room_for(192);
        // (a) `string_floor_in` via `try_with_vocab_key_prefix_in`
        {
            let mut ctx = Ctx::new(snap.clone());
            ctx.mem_limit = MIB;
            ctx.enable_cursor_accounting().unwrap();
            let held = ctx.charge(MIB - ctx.live_bytes() - room_for_192).unwrap();
            let mut scratch = FloorScratch::default();
            assert_eq!(
                ctx.string_floor_in(target_id, &mut scratch),
                Some(target_id.payload()),
                "string_floor_in must succeed when 192-byte scratch fits and extra FSST headroom is refused"
            );
            assert!(
                ctx.check().is_ok(),
                "string_floor_in must not record a memory failure when required capacity fits"
            );
            drop(held);
        }
        // (b) `simple_literal_cmp_in` via `with_vocab_key_in` (`total <= 192`, `fast_room` refused)
        {
            let mut ctx = Ctx::new(snap.clone());
            ctx.mem_limit = MIB;
            ctx.enable_cursor_accounting().unwrap();
            let held = ctx.charge(MIB - ctx.live_bytes() - room_for_192).unwrap();
            let mut scratch = FloorScratch::default();
            assert_eq!(
                ctx.simple_literal_cmp_in(target_id, target_id, &mut scratch),
                Some(std::cmp::Ordering::Equal),
                "simple_literal_cmp_in must succeed when 192-byte scratch fits and fast_room is refused"
            );
            assert!(
                ctx.check().is_ok(),
                "simple_literal_cmp_in must not record a memory failure"
            );
            drop(held);
        }
        // (c) scalar `term` via fresh-buffer `with_vocab_key`
        {
            let mut ctx = Ctx::new(snap.clone());
            ctx.mem_limit = MIB;
            ctx.enable_cursor_accounting().unwrap();
            let held = ctx.charge(MIB - ctx.live_bytes() - room_for_192).unwrap();
            assert_eq!(
                ctx.term(target_id),
                Some(target_term.clone()),
                "scalar term lookup must succeed when 192-byte scratch fits"
            );
            assert!(
                ctx.check().is_ok(),
                "scalar term lookup must not record a memory failure"
            );
            drop(held);
        }
        // (d) `simple_literal_cmp_in` via `with_vocab_key_in` (`total > 192`, `fast_room` refused,
        // fallback grows to required `grown_len`)
        {
            let mut ctx = Ctx::new(snap.clone());
            ctx.mem_limit = MIB;
            ctx.enable_cursor_accounting().unwrap();
            let held = ctx
                .charge(MIB - ctx.live_bytes() - room_for(grown_len))
                .unwrap();
            let mut scratch = FloorScratch::default();
            assert_eq!(
                ctx.simple_literal_cmp_in(grown_id, grown_id, &mut scratch),
                Some(std::cmp::Ordering::Equal),
                "simple_literal_cmp_in must grow to required length after fast_room refusal"
            );
            assert!(
                ctx.check().is_ok(),
                "simple_literal_cmp_in must not record a memory failure after optional headroom fallback"
            );
            drop(held);
        }
    }

    /// A finished result keeps its unleased output columns on top of the charged state
    /// the context retains, such as its value cache.
    #[test]
    fn a_finished_result_keeps_its_output_and_its_charged_state() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(512 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let ctx = pooled(&store, &grant);
        for (id, text) in &local_literals(&ctx, 2_000) {
            assert_text(ctx.value(*id), text);
        }
        let state = ctx.live_bytes();
        assert!(state > 0);
        ctx.check_output((200 * MIB / 8) as usize, 1).unwrap();
        ctx.shrink_retained(32 * MIB).unwrap();
        assert!(
            grant.used() >= state + 32 * MIB,
            "{} of {}",
            grant.used(),
            state + 32 * MIB
        );
        assert!(grant.used() < 200 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// A shard that reaches its share of the cache is freed with its slots, so that the
    /// slots of cleared shards never pile up outside the reservation. The test watches
    /// one shard through its evictions: a map cleared in place would keep the capacity
    /// it had before the eviction.
    #[test]
    fn a_cleared_value_shard_frees_its_slots() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 8 * MIB;
        let watched = ctx.values.shard(Id(0)) as *const _;
        let literals: Vec<_> = local_literals(&ctx, 40_000)
            .into_iter()
            .filter(|(id, _)| std::ptr::eq(ctx.values.shard(*id), watched))
            .collect();
        let share = ValueCache::cap(ctx.mem_limit) / VALUE_SHARDS as u64;
        let (mut before, mut evictions) = (0, 0);
        for (id, text) in &literals {
            assert_text(ctx.value(*id), text);
            let shard = ctx.values.shard(*id).read();
            if shard.bytes < before {
                evictions += 1;
                assert_eq!(shard.map.len(), 1);
                assert!(
                    shard.map.capacity() <= 4,
                    "a cleared shard keeps {} slots",
                    shard.map.capacity()
                );
            }
            assert!(shard.bytes <= share);
            before = shard.bytes;
        }
        assert!(evictions >= 2, "{evictions} evictions of the watched shard");
        assert!(ctx.values.reserved() <= ValueCache::cap(ctx.mem_limit));
    }

    #[test]
    fn fitting_eager_contexts_own_distinct_contributions() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.try_grant(100 * MIB).unwrap());
        let one = pooled(&store, &grant);
        let two = pooled(&store, &grant);
        one.check_output((40 * MIB / 8) as usize, 1).unwrap();
        two.check_output((24 * MIB / 8) as usize, 1).unwrap();
        assert_eq!(grant.used(), 64 * MIB);
        drop(one);
        assert_eq!(grant.used(), 24 * MIB);
        drop(two);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn eager_live_and_output_estimates_do_not_count_live_bytes_twice() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(64 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let ctx = pooled(&store, &grant);
        let held = ctx.charge(40 * MIB).unwrap();
        ctx.check_output((20 * MIB / 8) as usize, 1).unwrap();
        assert_eq!(grant.used(), 60 * MIB);
        ctx.check_output((10 * MIB / 8) as usize, 1).unwrap();
        assert_eq!(grant.used(), 60 * MIB);
        drop(held);
        assert_eq!(grant.used(), 60 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn rejected_eager_estimates_preserve_the_successful_contribution() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(32 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = pooled(&store, &grant);
        ctx.mem_limit = 48 * MIB;
        drop(ctx.charge(24 * MIB).unwrap());
        assert!(matches!(ctx.charge(40 * MIB), Err(Error::BudgetExceeded(b))
            if b.kind == BudgetKind::ServerMemory));
        assert!(matches!(ctx.check_output((56 * MIB / 8) as usize, 1),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory));
        assert_eq!(ctx.memory().live.load(Ordering::SeqCst), 0);
        assert_eq!(grant.used(), 24 * MIB);
        drop(ctx.charge(0).unwrap());
        ctx.check_output(0, 0).unwrap();
        assert_eq!(grant.used(), 24 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn concurrent_eager_peaks_reserve_only_the_local_maximum() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(64 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let ctx = pooled(&store, &grant);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for bytes in [24 * MIB, 40 * MIB] {
                let (ctx, barrier) = (&ctx, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    ctx.check_output((bytes / 8) as usize, 1).unwrap();
                });
            }
        });
        assert_eq!(grant.used(), 40 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn eager_and_cursor_ownership_share_a_grant_without_pinning_cursor_peaks() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let eager = pooled(&store, &grant);
        eager.check_output((24 * MIB / 8) as usize, 1).unwrap();
        let mut cursor = pooled(&store, &grant);
        cursor.enable_cursor_accounting().unwrap();
        let held = cursor.charge(48 * MIB).unwrap();
        assert_eq!(grant.used(), 72 * MIB);
        drop(held);
        assert_eq!(grant.used(), 24 * MIB);
        assert!(grant.granted() <= 32 * MIB);
        drop(eager);
        assert_eq!(grant.used(), 0);
        drop(cursor);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn cursor_transition_reconciles_prepared_local_terms_and_eager_peaks() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = pooled(&store, &grant);
        ctx.check_output((64 * MIB / 8) as usize, 1).unwrap();
        let term = Term::Literal(oxrdf::Literal::new_simple_literal("x".repeat(MIB as usize)));
        let id = ctx.intern_term(&term);
        assert!(ctx.local_len() > 0);
        ctx.enable_cursor_accounting().unwrap();
        let live = ctx.memory().live.load(Ordering::SeqCst);
        assert!(live >= MIB);
        assert_eq!(grant.used(), live);
        assert!(grant.granted() <= 16 * MIB);
        assert_eq!(ctx.term(id), Some(term));
        drop(ctx);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn a_grant_shared_by_two_contexts_follows_their_sum() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(256 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let one = pooled(&store, &grant);
        let two = pooled(&store, &grant);
        let held = one.charge(50 * MIB).unwrap();
        drop(two.charge(40 * MIB).unwrap());
        assert!(grant.granted() >= 50 * MIB, "{}", grant.granted());
        drop(held);
    }

    #[test]
    fn a_trim_never_unbacks_an_accepted_charge() {
        use std::sync::atomic::AtomicBool;
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(64 * MIB);
        let g1 = Arc::new(pool.empty_grant());
        let g2 = Arc::new(pool.empty_grant());
        let ctx1 = pooled(&store, &g1);
        let ctx2 = pooled(&store, &g2);
        let accepted = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        let worker = |ctx: &Ctx, bytes: u64, hold: bool| {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(charge) = ctx.charge(bytes) {
                    let now = accepted.fetch_add(bytes, Ordering::SeqCst) + bytes;
                    assert!(now <= 64 * MIB, "accepted {now} bytes in a 64 MiB pool");
                    if hold {
                        std::thread::yield_now();
                    }
                    accepted.fetch_sub(bytes, Ordering::SeqCst);
                    drop(charge);
                }
            }
        };
        std::thread::scope(|s| {
            s.spawn(|| worker(&ctx1, 48 * MIB, true));
            s.spawn(|| worker(&ctx1, MIB, false));
            s.spawn(|| worker(&ctx2, 48 * MIB, true));
            std::thread::sleep(std::time::Duration::from_secs(2));
            stop.store(true, Ordering::Relaxed);
        });
        drop((ctx1, ctx2));
        assert_eq!(accepted.load(Ordering::SeqCst), 0);
        drop((g1, g2));
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn grow_decode_serializes_mode_selection_across_ledger_promotion() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc;
        use std::time::Duration;

        fn recv_bounded<T>(rx: &mpsc::Receiver<T>, msg: &str) -> T {
            rx.recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|err| panic!("{msg}: {err}"))
        }

        let store = Store::in_memory(StoreOptions::default());

        // Case 1: Decoder 1 pauses before acquiring `decode.state`, `memory_parent()`
        // promotes the context to a ledger, and Decoder 2 enters the ledger path and
        // pauses after snapshotting `eager_paid == 0`. When Decoder 1 resumes, it must
        // observe the promoted ledger under `decode.state` instead of taking a stale
        // eager branch and double-charging the envelope.
        {
            let need = Ctx::decode_need(128);
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = need;
            let ctx = ctx.into_shared();

            let before_lock_calls = Arc::new(AtomicUsize::new(0));
            let after_snap_calls = Arc::new(AtomicUsize::new(0));
            let (t1_paused_tx, t1_paused_rx) = mpsc::channel::<()>();
            let (t1_go_tx, t1_go_rx) = mpsc::channel::<()>();
            let (t2_paused_tx, t2_paused_rx) = mpsc::channel::<()>();
            let (t2_go_tx, t2_go_rx) = mpsc::channel::<()>();
            let t1_paused_tx = Arc::new(parking_lot::Mutex::new(Some(t1_paused_tx)));
            let t1_go_rx = Arc::new(parking_lot::Mutex::new(Some(t1_go_rx)));
            let t2_paused_tx = Arc::new(parking_lot::Mutex::new(Some(t2_paused_tx)));
            let t2_go_rx = Arc::new(parking_lot::Mutex::new(Some(t2_go_rx)));

            let bl_calls = before_lock_calls.clone();
            let as_calls = after_snap_calls.clone();
            *ctx.decode.grow_hook.lock() = Some(Arc::new(move |point| match point {
                DecodeGrowPoint::BeforeLock => {
                    if bl_calls.fetch_add(1, Ordering::AcqRel) == 0 {
                        t1_paused_tx.lock().take().unwrap().send(()).unwrap();
                        let rx = t1_go_rx.lock().take().unwrap();
                        recv_bounded(&rx, "decoder 1 was not released before lock");
                    }
                }
                DecodeGrowPoint::AfterLedgerSnapshot => {
                    if as_calls.fetch_add(1, Ordering::AcqRel) == 0 {
                        t2_paused_tx.lock().take().unwrap().send(()).unwrap();
                        let rx = t2_go_rx.lock().take().unwrap();
                        recv_bounded(&rx, "decoder 2 was not released after snapshot");
                    }
                }
                DecodeGrowPoint::BeforeEagerCharge => {
                    panic!("neither decoder should take the eager branch after promotion");
                }
            }));

            let (r1_tx, r1_rx) = mpsc::channel();
            let ctx1 = ctx.clone();
            let t1 = std::thread::spawn(move || {
                let _ = r1_tx.send(ctx1.reserve_decode_checked(128));
            });

            recv_bounded(&t1_paused_rx, "decoder 1 did not pause before decode lock");

            // Promote to the shared ledger while decoder 1 is paused before the lock.
            let _parent = ctx.memory_parent().unwrap();

            let (r2_tx, r2_rx) = mpsc::channel();
            let ctx2 = ctx.clone();
            let t2 = std::thread::spawn(move || {
                let _ = r2_tx.send(ctx2.reserve_decode_checked(128));
            });

            recv_bounded(
                &t2_paused_rx,
                "decoder 2 did not pause after ledger snapshot",
            );

            // Release decoder 1 first; it must enter the ledger path and fund `need` once.
            t1_go_tx.send(()).unwrap();
            let r1 = recv_bounded(&r1_rx, "decoder 1 did not finish");
            r1.expect("decoder 1 must succeed on the promoted ledger path");

            // Release decoder 2; it must reuse the already-funded ledger decode target.
            t2_go_tx.send(()).unwrap();
            let r2 = recv_bounded(&r2_rx, "decoder 2 did not finish");
            r2.expect("decoder 2 must succeed without double-charging the envelope");

            t1.join().unwrap();
            t2.join().unwrap();
            *ctx.decode.grow_hook.lock() = None;

            assert_eq!(ctx.live_bytes(), need);
            assert!(ctx.decode.state.lock().1.is_empty());
            assert!(ctx.check().is_ok());
            ctx.release_work();
            assert_eq!(ctx.live_bytes(), 0);
        }

        // Case 2: Decoder 1 acquires `decode.state` before `memory_parent()` promotes
        // the ledger, then falls back to `Charge::add` on the promoted ledger while
        // still holding `decode.state`. Decoder 2 requests a larger envelope and must
        // wait until Decoder 1 records its eager-paid owner bytes, then reserve only
        // the remaining delta on the ledger.
        {
            let need1 = Ctx::decode_need(128);
            let need2 = Ctx::decode_need(256);
            assert!(need2 > need1);
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = need2;
            let ctx = ctx.into_shared();

            let before_lock_calls = Arc::new(AtomicUsize::new(0));
            let (t1_eager_tx, t1_eager_rx) = mpsc::channel::<()>();
            let (t1_go_tx, t1_go_rx) = mpsc::channel::<()>();
            let (t2_before_lock_tx, t2_before_lock_rx) = mpsc::channel::<()>();
            let t1_eager_tx = Arc::new(parking_lot::Mutex::new(Some(t1_eager_tx)));
            let t1_go_rx = Arc::new(parking_lot::Mutex::new(Some(t1_go_rx)));
            let t2_before_lock_tx = Arc::new(parking_lot::Mutex::new(Some(t2_before_lock_tx)));

            let bl_calls = before_lock_calls.clone();
            *ctx.decode.grow_hook.lock() = Some(Arc::new(move |point| match point {
                DecodeGrowPoint::BeforeLock => {
                    if bl_calls.fetch_add(1, Ordering::AcqRel) == 1 {
                        t2_before_lock_tx.lock().take().unwrap().send(()).unwrap();
                    }
                }
                DecodeGrowPoint::BeforeEagerCharge => {
                    t1_eager_tx.lock().take().unwrap().send(()).unwrap();
                    let rx = t1_go_rx.lock().take().unwrap();
                    recv_bounded(&rx, "decoder 1 was not released in eager branch");
                }
                DecodeGrowPoint::AfterLedgerSnapshot => {}
            }));

            let (r1_tx, r1_rx) = mpsc::channel();
            let ctx1 = ctx.clone();
            let t1 = std::thread::spawn(move || {
                let _ = r1_tx.send(ctx1.reserve_decode_checked(128));
            });

            recv_bounded(&t1_eager_rx, "decoder 1 did not enter locked eager branch");

            let _parent = ctx.memory_parent().unwrap();

            let (r2_tx, r2_rx) = mpsc::channel();
            let ctx2 = ctx.clone();
            let t2 = std::thread::spawn(move || {
                let _ = r2_tx.send(ctx2.reserve_decode_checked(256));
            });

            recv_bounded(&t2_before_lock_rx, "decoder 2 did not reach decode lock");
            assert!(
                !ctx.decode_state_unlocked(),
                "decoder 1 must hold decode.state across its eager fallback charge"
            );

            t1_go_tx.send(()).unwrap();
            let r1 = recv_bounded(&r1_rx, "decoder 1 did not finish eager fallback");
            let r2 = recv_bounded(&r2_rx, "decoder 2 did not finish ledger delta");
            t1.join().unwrap();
            t2.join().unwrap();
            *ctx.decode.grow_hook.lock() = None;

            r1.expect("decoder 1 must succeed");
            r2.expect("decoder 2 must succeed by charging only need2 - need1");
            assert_eq!(ctx.live_bytes(), need2);
            let eager_paid: u64 = ctx.decode.state.lock().1.iter().map(|&(_, b)| b).sum();
            assert_eq!(eager_paid, need1);
            assert!(ctx.check().is_ok());
            ctx.release_work();
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    /// Checked local-vocabulary insertion transfers the retained portion of its
    /// pre-admitted growth charge directly into `self.retained` before releasing the
    /// transient excess, so a contending waiter on the shared `QueryPool` cannot steal
    /// the retained granules during handoff and leave an inserted entry unaccounted.
    #[test]
    fn intern_key_checked_transfers_retained_credit_without_pool_release() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(48 * MIB);
        let grant = Arc::new(pool.try_grant(16 * MIB).unwrap());
        let other = pool.try_grant(24 * MIB).unwrap();
        let mut ctx = pooled(&store, &grant);
        ctx.mem_limit = 64 * MIB;
        ctx.enable_cursor_accounting().unwrap();

        let ticket_slot: Arc<parking_lot::Mutex<Option<super::super::pool::Ticket>>> =
            Default::default();
        let stolen: Arc<parking_lot::Mutex<Option<super::super::pool::PoolGrant>>> =
            Default::default();
        let ticket_for_wake = ticket_slot.clone();
        let stolen_for_wake = stolen.clone();
        let ticket = pool.enter(
            8 * MIB,
            Some(Arc::new(move || {
                if let Some(ticket) = ticket_for_wake.lock().as_mut()
                    && let Some(g) = ticket.poll()
                {
                    *stolen_for_wake.lock() = Some(g);
                }
            })),
        );
        *ticket_slot.lock() = Some(ticket);

        // `growth_charge` grows `grant` from 16 MiB to 24 MiB (consuming the last 8 MiB
        // of the 48 MiB pool while staying within the 24 MiB waiting-pool half share).
        // If `growth_charge` were dropped before retaining the 22 MiB entry, `grant`
        // would trim back to 16 MiB, wake the 8 MiB waiter synchronously, and fail the
        // subsequent `retain_bytes` call with `ServerMemory`.
        let key = vec![b'k'; (22 * MIB) as usize];
        let id = ctx
            .intern_key_checked(&key)
            .expect("retained credit must stay pinned across handoff under pool contention");
        assert_eq!(id.tag(), Tag::Local);
        assert!(
            stolen.lock().is_none(),
            "retained granules must not be released to a contending pool waiter"
        );
        let physical = ctx.local.read().physical_bytes();
        assert_eq!(ctx.local_retained.load(Ordering::Acquire), physical);
        assert!(
            ctx.live_bytes() >= physical,
            "live bytes ({}) must cover retained local vocabulary ({physical})",
            ctx.live_bytes()
        );

        drop(ticket_slot.lock().take());
        drop(stolen.lock().take());
        drop(other);
        drop(ctx);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    /// `retain_from_charge` never advances `self.retained.charge.reserved`, so concurrent
    /// `retain_bytes` calls cannot mistake directly-retained bytes for `StepReservation`
    /// headroom and skip charging past `mem_limit`.
    #[test]
    fn retain_from_charge_preserves_step_reservation_invariant_under_concurrency() {
        let store = Store::in_memory(StoreOptions::default());

        // 1. Deterministic A < B interleaving (16 KiB retain_bytes pauses before
        // `grow_reservation`, 60 KiB `retain_from_charge` completes under a 64 KiB cap):
        // `grow_reservation` must refuse rather than observing `used_a <= reserved`.
        {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = 64 << 10;
            let used_a = ctx.retained.used.fetch_add(16 << 10, Ordering::AcqRel) + (16 << 10);
            assert!(!ctx.retained.charge.covers(used_a));

            let src = ctx.charge(60 << 10).unwrap();
            ctx.retain_from_charge(60 << 10, &src);
            drop(src);

            let res = ctx.grow_reservation(&ctx.retained.charge, used_a, u64::MAX, true);
            assert!(
                res.is_err(),
                "16 KiB retain_bytes must refuse when 60 KiB is already retained under 64 KiB cap"
            );
            ctx.retained.used.fetch_sub(16 << 10, Ordering::AcqRel);
            assert!(
                ctx.retained.charge.reserved() >= ctx.retained.used.load(Ordering::Acquire),
                "reserved ({}) must stay >= used ({})",
                ctx.retained.charge.reserved(),
                ctx.retained.used.load(Ordering::Acquire)
            );
            assert_eq!(ctx.live_bytes(), 60 << 10);
        }

        // 2. Deterministic A > B interleaving (60 KiB retain_bytes pauses before
        // `grow_reservation`, 16 KiB `retain_from_charge` completes): under 64 KiB cap it
        // must refuse; under 128 KiB cap it must charge all 60 KiB on top of the 16 KiB.
        for (limit, expect_ok) in [(64 << 10, false), (128 << 10, true)] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = limit;
            let used_a = ctx.retained.used.fetch_add(60 << 10, Ordering::AcqRel) + (60 << 10);

            let src = ctx.charge(16 << 10).unwrap();
            ctx.retain_from_charge(16 << 10, &src);
            drop(src);

            let res = ctx.grow_reservation(&ctx.retained.charge, used_a, u64::MAX, true);
            assert_eq!(res.is_ok(), expect_ok);
            if res.is_err() {
                ctx.retained.used.fetch_sub(60 << 10, Ordering::AcqRel);
            } else {
                assert!(
                    ctx.live_bytes() >= (76 << 10),
                    "live bytes ({}) must cover both 60 KiB and 16 KiB",
                    ctx.live_bytes()
                );
            }
            assert!(ctx.retained.charge.reserved() >= ctx.retained.used.load(Ordering::Acquire));
            assert!(ctx.live_bytes() <= limit);
        }

        // 3. Existing spare headroom in `self.retained` is reused without changing
        // `reserved`, while only the overflow stays permanently charged out of `source`.
        {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = 16 << 20;
            assert!(ctx.retain_bytes(16 << 10));
            let reserved_before = ctx.retained.charge.reserved();
            assert!(reserved_before > (16 << 10));
            let spare = reserved_before - (16 << 10);
            let want = spare + (12 << 10);
            let src = ctx.charge(want + (4 << 10)).unwrap();
            ctx.retain_from_charge(want, &src);
            drop(src);
            assert_eq!(ctx.retained.charge.reserved(), reserved_before);
            assert_eq!(ctx.retained.used.load(Ordering::Acquire), reserved_before);
            assert_eq!(ctx.live_bytes(), reserved_before + (12 << 10));
        }

        // 4. Multi-threaded stress test: concurrent `retain_bytes` and
        // `intern_key_checked` under a tight budget preserve `reserved >= used` and
        // `live_bytes <= mem_limit`.
        for trial in 0..8usize {
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 96 << 10;
            let ctx = Arc::new(raw);
            let barrier = Arc::new(std::sync::Barrier::new(2));
            std::thread::scope(|s| {
                let c1 = ctx.clone();
                let b1 = barrier.clone();
                s.spawn(move || {
                    b1.wait();
                    for _ in 0..32 {
                        if !c1.retain_bytes(4 << 10) {
                            break;
                        }
                    }
                });
                let c2 = ctx.clone();
                let b2 = barrier.clone();
                s.spawn(move || {
                    b2.wait();
                    for i in 0..32usize {
                        let key = format!("<urn:t{trial}:{i}:{}>", "x".repeat(2048));
                        if c2.intern_key_checked(key.as_bytes()).is_err() {
                            break;
                        }
                    }
                });
            });
            assert!(
                ctx.retained.charge.reserved() >= ctx.retained.used.load(Ordering::Acquire),
                "trial {trial}: reserved ({}) < used ({})",
                ctx.retained.charge.reserved(),
                ctx.retained.used.load(Ordering::Acquire)
            );
            assert!(
                ctx.live_bytes() <= ctx.mem_limit,
                "trial {trial}: live bytes ({}) exceeded mem_limit ({})",
                ctx.live_bytes(),
                ctx.mem_limit
            );
        }
    }

    #[test]
    fn circuit_vocab_checked_insert_covers_long_keys_and_refuses_before_mutation() {
        let store = Store::in_memory(StoreOptions::default());
        let long_key = format!("\"{}\x00", "a".repeat(256));
        let expected_retained =
            crate::incr::terms::Terms::key_retained_bytes(long_key.len()).unwrap();
        assert!(expected_retained > long_key.len() as u64 + 96);

        // 1. Direct checked insertion into an attached empty circuit term table with a key
        // longer than 48 bytes must cover `Terms::bytes()` without tripping `rem == 0`.
        {
            let ctx = Ctx::new(store.snapshot());
            ctx.swap_circuit_terms(crate::incr::terms::Terms::default());
            let id = ctx
                .intern_key_checked(long_key.as_bytes())
                .expect("long key insertion must fit");
            assert_eq!(id.tag(), Tag::Local);
            assert_eq!(
                ctx.local_retained.load(Ordering::Acquire),
                expected_retained
            );
            let terms = ctx.take_circuit_terms();
            assert_eq!(terms.key_count(), 1);
            assert_eq!(terms.bytes(), expected_retained);
        }

        // 2. Budget strictly between `key_len + 96` and `2 * key_len + 48` must refuse
        // BEFORE mutating the attached circuit term table.
        {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = long_key.len() as u64 + 96 + 16;
            assert!(ctx.mem_limit < expected_retained);
            ctx.swap_circuit_terms(crate::incr::terms::Terms::default());
            let err = ctx
                .intern_key_checked(long_key.as_bytes())
                .expect_err("long key insertion must refuse before mutating terms");
            assert!(
                matches!(
                    err,
                    crate::error::Error::BudgetExceeded(b)
                        if b.kind == crate::BudgetKind::Memory
                ),
                "expected BudgetKind::Memory, got {err:?}"
            );
            let terms = ctx.take_circuit_terms();
            assert_eq!(
                terms.key_count(),
                0,
                "refused checked insertion must not mutate attached circuit terms"
            );
            assert_eq!(terms.bytes(), 0);
        }
    }
}
