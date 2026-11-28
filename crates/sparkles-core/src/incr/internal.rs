//! Internal circuits (spec F13 §3.6): circuits that server code attaches and no user
//! defined.
//!
//! An internal circuit has no `view.json`, no name in the list of views and no
//! principal. It reads through an [`InternalAccess`], which only server code
//! constructs: no view definition, route, binding or caller credential converts into
//! one, and nothing that lists, reads, rebuilds or deletes views reaches an internal
//! circuit. Server code holds an [`InternalCircuit`] handle, and the circuit is
//! maintained while a handle lives.
//!
//! Its sink ([`Sink`]) is either the circuit's rows, or an accumulator that keeps a
//! count of changed facts per key and not the rows. Changes cancel inside a Z-set, both
//! across commits and within one commit, so the accumulator's circuit keeps each
//! changed fact's identity to the sink: the key columns and the fact's own columns. The
//! runtime runs an accumulator's circuit one commit at a time
//! ([`DeltaSet::steps`](super::circuit::DeltaSet::steps)), and for each commit the
//! sink sums the absolute weight of each (key, fact) row of the circuit's change and
//! totals it per key, before the changes of a later commit can cancel it. A fact added
//! by one commit and removed by the next counts 2, a deletion counts 1, and a
//! replacement within one commit counts 2.
//!
//! The accumulator keeps its counts per commit, so [`InternalCircuit::counts`] gives
//! the exact totals after any commit it still covers, and
//! [`InternalCircuit::discard_through`] lets server code drop the counts it has acted
//! on. A commit whose changes are not known per key, such as a bulk load the change
//! feed lists with counts only, is recorded as unknown, and a read that spans it is not
//! complete.
//!
//! **Checkpoint.** `<root>/circuits/{id}/main/state-{commit}.bin` with the header of
//! [`persist`](super::persist) under the magic `SPKCIRC\0`. Format 1 holds a circuit
//! without annotations, and format 2 one that keeps them: its circuit state ends with
//! the annotations of each operator's state and of the result (spec F13 §5). Formats 3
//! and 4 ([`INTERNAL_FORMAT`](super::persist::INTERNAL_FORMAT)) hold the same for a
//! circuit with a fixed-point operator (spec F13 §4.3). A circuit writes the lowest
//! format that holds its state, and reads only that format. The body is the circuit's
//! state as a view checkpoint holds it, as a length and its bytes, then a sink byte
//! (0 rows, 1 accumulator) and, for an accumulator, the commit its counts start after,
//! the unknown ranges (count, then each range's first and last commit), the keys
//! (count, then each key's width and each term as a length and its vocabulary key, 0
//! for unbound) and the counts (count, then each count's commit, key number and
//! value), all as LEB128 numbers. A reader that does not know the format refuses the
//! file, leaves it in place and builds the circuit from the data, and an accumulator
//! built so counts from its build.

use super::zset::Changes;
use crate::access::GraphAccess;
use crate::error::{Error, Result};
use crate::id::Id;
use oxrdf::Term;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::sync::Arc;

/// The directory of internal circuits' checkpoints under a dataset's root.
pub const DIR: &str = "circuits";

/// The access an internal circuit reads with. Only server code constructs one: it is
/// not serializable, has no default and converts from nothing a caller supplies, so a
/// view, a live query, a route or a binding never holds one.
pub struct InternalAccess {
    pub(crate) graphs: Option<Arc<GraphAccess>>,
}

impl InternalAccess {
    /// Every quad of the dataset, including the graphs that a disclosure guard
    /// protects from callers.
    pub fn everything() -> InternalAccess {
        InternalAccess { graphs: None }
    }

    /// What `graphs` lets a reader see, such as a configured principal's view that
    /// server code resolved. The access is fixed when the circuit attaches: server code
    /// attaches the circuit again to follow a policy reload.
    pub fn reading(graphs: Arc<GraphAccess>) -> InternalAccess {
        InternalAccess {
            graphs: Some(graphs),
        }
    }
}

impl std::fmt::Debug for InternalAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.graphs.is_none() {
            "InternalAccess(everything)"
        } else {
            "InternalAccess(restricted)"
        })
    }
}

/// Where an internal circuit's changes go.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Sink {
    /// The circuit keeps its rows, which [`InternalCircuit::rows`] reads.
    Rows,
    /// The circuit counts changed facts per key. `key` names the result's key columns,
    /// and every other column belongs to the fact's identity.
    Accumulator { key: Vec<String> },
}

/// What server code asks an internal circuit to maintain.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct InternalSpec {
    /// A stable name that server code chooses, which keeps the circuit's checkpoints:
    /// letters, digits, `_` and `-`, starting with a letter or digit, at most 64
    /// characters. It is not a view name, and views of the same name are other objects.
    pub id: String,
    /// A SELECT query (an ASK or CONSTRUCT query for the rows sink), with full IRIs:
    /// the dataset's prefixes do not apply.
    pub query: String,
    pub sink: Sink,
    /// The budget of the circuit's state and, for an accumulator, of its counts.
    pub max_memory_bytes: u64,
    /// The annotation the circuit keeps for each row (spec F13 §5), which
    /// [`InternalCircuit::annotated_rows`] reads. Only the rows sink keeps one.
    pub provenance: super::prov::Provenance,
}

impl InternalSpec {
    /// A circuit that keeps the rows of `query`.
    pub fn rows(id: impl Into<String>, query: impl Into<String>) -> InternalSpec {
        InternalSpec {
            id: id.into(),
            query: query.into(),
            sink: Sink::Rows,
            max_memory_bytes: super::views::DEFAULT_MEMORY_BYTES,
            provenance: super::prov::Provenance::None,
        }
    }

    /// A circuit that counts the changed facts of `query` per value of the `key`
    /// columns.
    pub fn accumulator<K: Into<String>>(
        id: impl Into<String>,
        query: impl Into<String>,
        key: impl IntoIterator<Item = K>,
    ) -> InternalSpec {
        InternalSpec {
            id: id.into(),
            query: query.into(),
            sink: Sink::Accumulator {
                key: key.into_iter().map(Into::into).collect(),
            },
            max_memory_bytes: super::views::DEFAULT_MEMORY_BYTES,
            provenance: super::prov::Provenance::None,
        }
    }

    /// The same spec with a budget of `bytes`.
    pub fn with_memory(mut self, bytes: u64) -> InternalSpec {
        self.max_memory_bytes = bytes;
        self
    }

    /// The same spec keeping annotation `p` for each row.
    pub fn with_provenance(mut self, p: super::prov::Provenance) -> InternalSpec {
        self.provenance = p;
        self
    }
}

/// The original read owns its admission. An explicit caller clone is independently owned.
#[derive(Debug, Default)]
pub(crate) struct RowsMemory {
    pub(crate) _lease: Option<sparkles_budget::MemoryLease>,
}

impl Clone for RowsMemory {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for RowsMemory {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for RowsMemory {}

/// An internal circuit's rows with their annotations
/// ([`InternalCircuit::annotated_rows`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotatedRows {
    pub commit: u64,
    pub columns: Vec<String>,
    /// each row's terms with its multiplicity and its annotation
    pub rows: Vec<(Vec<Option<Term>>, u64, super::prov::Annotation)>,
    pub(crate) _memory: RowsMemory,
}

/// The changed facts per key after a commit ([`InternalCircuit::counts`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counts {
    /// the commit the counts start after, as asked
    pub after: u64,
    /// the commit the counts run to: the circuit's commit
    pub through: u64,
    /// why some commit in `after..=through` is not counted per key, when one is not:
    /// the accumulator started or dropped its counts later, or a commit's changes were
    /// not known. The keys then hold only the commits that were counted.
    pub unknown: Option<String>,
    /// each key's terms (`None` for unbound) with its count, in no order
    pub keys: Vec<(Vec<Option<Term>>, u64)>,
}

impl Counts {
    /// Whether every commit after `after` up to `through` is counted.
    pub fn complete(&self) -> bool {
        self.unknown.is_none()
    }

    /// The count of `key` (0 when it has none).
    pub fn get(&self, key: &[Option<Term>]) -> u64 {
        self.keys
            .iter()
            .find(|(k, _)| k.as_slice() == key)
            .map_or(0, |(_, n)| *n)
    }
}

/// An internal circuit's rows at its commit ([`InternalCircuit::rows`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternalRows {
    pub commit: u64,
    pub columns: Vec<String>,
    /// each row's terms with its multiplicity
    pub rows: Vec<(Vec<Option<Term>>, u64)>,
    pub(crate) _memory: RowsMemory,
}

/// The counts of an accumulator, one entry per commit and key.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tally {
    /// the result columns of the key, by position
    pub(crate) key_cols: Vec<usize>,
    /// the counts cover the commits after this one
    pub(crate) since: u64,
    /// ranges `(first, last)` of commits whose changes were not counted per key
    pub(crate) unknown: Vec<(u64, u64)>,
    keys: Vec<TallyKey>,
    index: FxHashMap<Vec<Option<Term>>, u32>,
    /// `(commit, key, count)` in commit order
    entries: VecDeque<(u64, u32, u64)>,
    /// The heap owned by the two copies of each key, excluding their backing tables.
    key_bytes: u64,
    /// The index allocation can outlive deletions that lower its reported capacity.
    index_capacity: usize,
    /// the most memory the counts took right after a commit was counted
    #[cfg(test)]
    pub(crate) peak: u64,
    /// the drops that the budget made
    #[cfg(test)]
    pub(crate) drops: u64,
    /// the times the budget fitted the storage
    #[cfg(test)]
    pub(crate) resizes: u64,
}

/// The approximate memory of one entry and of one unknown range.
const ENTRY_BYTES: u64 = 24;
const RANGE_BYTES: u64 = 16;

const KEY_SLOT: usize = std::mem::size_of::<TallyKey>();
const INDEX_SLOT: usize = std::mem::size_of::<(Vec<Option<Term>>, u32)>();
const TERM_SLOT: usize = std::mem::size_of::<Option<Term>>();
const UNUSED_KEY: u32 = u32::MAX;

#[derive(Clone, Debug, PartialEq)]
struct TallyKey {
    terms: Vec<Option<Term>>,
    bytes: u64,
    /// Used during pruning to mark live keys and remap their old indices in place.
    remap: u32,
}

fn key_copy_bytes(key: &[Option<Term>]) -> u64 {
    use oxrdf::TermRef;
    fn heap_bytes(term: TermRef<'_>) -> u64 {
        match term {
            TermRef::NamedNode(n) => n.as_str().len() as u64,
            TermRef::BlankNode(b) => b.as_str().len() as u64,
            TermRef::Literal(l) => {
                let suffix = l.language().map_or_else(
                    || {
                        if l.datatype() == oxrdf::vocab::xsd::STRING {
                            0
                        } else {
                            l.datatype().as_str().len()
                        }
                    },
                    str::len,
                );
                (l.value().len() + suffix) as u64
            }
            TermRef::Triple(t) => {
                std::mem::size_of::<oxrdf::Triple>() as u64
                    + heap_bytes(t.subject.as_ref().into())
                    + heap_bytes(t.predicate.as_ref().into())
                    + heap_bytes(t.object.as_ref())
            }
        }
    }
    std::mem::size_of_val(key) as u64
        + key
            .iter()
            .flatten()
            .map(|term| heap_bytes(term.as_ref()))
            .sum::<u64>()
}

impl Tally {
    pub(crate) fn new(key_cols: Vec<usize>, since: u64) -> Tally {
        Tally {
            key_cols,
            since,
            ..Default::default()
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.buffer_bytes() + self.key_bytes
    }

    fn buffer_bytes(&self) -> u64 {
        self.entries.capacity() as u64 * ENTRY_BYTES
            + self.unknown.capacity() as u64 * RANGE_BYTES
            + (self.keys.capacity() * KEY_SLOT) as u64
            + sparkles_budget::sizes::table_bytes(self.index_capacity, INDEX_SLOT) as u64
    }

    fn key_slot_growth(&self, add: usize) -> u64 {
        use sparkles_budget::sizes::{table_bytes, table_grown, vec_grown};
        let cap = self.keys.capacity();
        let keys = vec_grown(cap, self.keys.len(), add, KEY_SLOT).saturating_sub(cap) * KEY_SLOT;
        let want = self.index.len() + add;
        let index_growth = if want <= self.index.capacity() || want <= self.index_capacity / 2 {
            0
        } else {
            // Include the replacement allocation while the old table is still alive.
            // Reserve against the backing capacity because tombstones can make the
            // table report fewer usable slots than it actually allocated.
            let cap = table_grown(
                self.index_capacity,
                self.index_capacity,
                want.saturating_sub(self.index_capacity).max(1),
            );
            table_bytes(cap, INDEX_SLOT) as u64
        };
        keys as u64 + index_growth
    }

    fn intern(&mut self, key: Vec<Option<Term>>) -> u32 {
        if let Some(&n) = self.index.get(&key) {
            return n;
        }
        let n = self.keys.len() as u32;
        let bytes = 2 * key_copy_bytes(&key);
        self.key_bytes += bytes;
        self.index.insert(key.clone(), n);
        self.index_capacity = self.index_capacity.max(self.index.capacity());
        self.keys.push(TallyKey {
            terms: key,
            bytes,
            remap: UNUSED_KEY,
        });
        n
    }

    /// Release all entries, unknown ranges, keys, and retained backing capacity so
    /// `self.bytes() == 0` while keeping `key_cols` and `since`.
    pub(crate) fn clear_storage(&mut self) {
        self.entries = VecDeque::new();
        self.unknown = Vec::new();
        self.keys = Vec::new();
        self.index = FxHashMap::default();
        self.key_bytes = 0;
        self.index_capacity = 0;
    }

    /// Release spare backing capacity without dropping any commits' counts.
    /// Returns whether `self.bytes()` decreased.
    pub(crate) fn shrink_spare_capacity(&mut self) -> bool {
        let before = self.bytes();
        self.entries.shrink_to_fit();
        self.unknown.shrink_to_fit();
        self.keys.shrink_to_fit();
        use sparkles_budget::sizes::{buckets, table_capacity};
        let slots = table_capacity(self.index.len());
        if buckets(slots) < buckets(self.index_capacity) {
            // Drop the table and its key copies before reusing their existing credit.
            // The replacement uses fewer buckets and copies only the surviving keys.
            drop(std::mem::take(&mut self.index));
            self.index.reserve(self.keys.len());
            for (i, key) in self.keys.iter().enumerate() {
                self.index.insert(key.terms.clone(), i as u32);
            }
            self.index_capacity = self.index.capacity();
        }
        self.bytes() < before
    }

    /// Reclaim under the budget mutex without growing or reallocating any buffers.
    /// Evict a prefix to free key payloads. If the backing buffers alone would exceed
    /// the target, drop the storage as a unit and mark its range unavailable.
    pub(crate) fn evict_for_reclaim(&mut self, needed: u64) -> (u64, bool) {
        let before = self.bytes();
        if before == 0 || needed == 0 {
            return (0, false);
        }
        let old_since = self.since;
        let target = before.saturating_sub(needed);
        if let Some(payload) = target.checked_sub(self.buffer_bytes()) {
            let through = self.through_for(payload - payload / 4, false);
            self.discard_prefix(through);
        }
        if self.bytes() > target {
            self.since = self.since.max(self.last_counted_commit());
            self.clear_storage();
        }
        #[cfg(test)]
        {
            self.drops += u64::from(self.since > old_since);
        }
        (before - self.bytes(), self.since > old_since)
    }

    fn last_counted_commit(&self) -> u64 {
        self.entries
            .back()
            .map_or(self.since, |e| e.0)
            .max(self.unknown.last().map_or(self.since, |r| r.1))
    }

    /// Count one commit with the compact input and decoded keys admitted separately.
    /// Budget calls happen outside the tally lock, so they can reclaim old counts.
    pub(crate) fn add_budgeted_charged_shared(
        tally: &parking_lot::Mutex<Tally>,
        commit: u64,
        c: Changes,
        c_lease: super::budget::ScratchLease,
        term: impl Fn(Id, &mut super::budget::ScratchLease) -> Result<Option<Term>>,
        circuit: Option<&super::circuit::Circuit>,
        dirty: &mut bool,
    ) -> Result<()> {
        let result = Self::try_count(tally, commit, c, c_lease, term, circuit);
        if matches!(result, Err(Error::BudgetExceeded(_))) {
            {
                let mut t = tally.lock();
                t.since = t.since.max(commit);
                t.clear_storage();
            }
            if let Some(circuit) = circuit {
                circuit.set_external_best_effort(0);
            }
            *dirty = true;
            return Ok(());
        }
        result
    }

    fn try_count(
        tally: &parking_lot::Mutex<Tally>,
        commit: u64,
        c: Changes,
        c_lease: super::budget::ScratchLease,
        term: impl Fn(Id, &mut super::budget::ScratchLease) -> Result<Option<Term>>,
        circuit: Option<&super::circuit::Circuit>,
    ) -> Result<()> {
        use super::circuit::reserve_vec_slot;
        use sparkles_budget::sizes::{table_bytes, table_grown, vec_grown};

        if c.is_empty() {
            drop(c);
            return Ok(());
        }
        let budget = circuit.map(|c| c.budget_handle());
        let mut compact = super::budget::ScratchLease::new(budget.clone());
        let cols_bytes = (tally.lock().key_cols.len() * std::mem::size_of::<usize>()) as u64;
        compact.try_reserve(cols_bytes)?;
        let key_cols = tally.lock().key_cols.clone();
        let key_payload = (key_cols.len() * std::mem::size_of::<Id>()) as u64;
        let slot = std::mem::size_of::<(Box<[Id]>, u64)>();
        let mut per = FxHashMap::<Box<[Id]>, u64>::default();
        let mut projected = compact.sibling();
        for (row, w) in &c {
            projected.try_set(key_payload)?;
            let key: Box<[Id]> = key_cols.iter().map(|&i| row[i]).collect();
            if let Some(n) = per.get_mut(&key) {
                *n += w.unsigned_abs();
            } else {
                let old_cap = per.capacity();
                let new_cap = table_grown(old_cap, per.len(), 1);
                if new_cap > old_cap {
                    compact.try_reserve(table_bytes(new_cap, slot) as u64)?;
                }
                projected.transfer_to(&mut compact, key_payload);
                per.insert(key, w.unsigned_abs());
                if new_cap > old_cap {
                    compact.release(table_bytes(old_cap, slot) as u64);
                }
            }
        }
        drop(projected);
        drop(c);
        drop(c_lease);

        let mut decoded_lease = super::budget::ScratchLease::new(budget);
        let mut decoded = Vec::new();
        for (ids, n) in per {
            if n == 0 {
                continue;
            }
            reserve_vec_slot(&decoded, &mut decoded_lease)?;
            decoded_lease.try_reserve((ids.len() * TERM_SLOT) as u64)?;
            let mut key = Vec::with_capacity(ids.len());
            for &i in &ids {
                key.push(term(i, &mut decoded_lease)?);
            }
            decoded.push((key, n));
        }
        drop(key_cols);
        drop(compact);
        if decoded.is_empty() {
            return Ok(());
        }

        let counted_bytes = (decoded.len() * std::mem::size_of::<(u32, u64)>()) as u64;
        let mut retained = decoded_lease.sibling();
        let final_bytes = loop {
            let mut t = tally.lock();
            let mut new_keys = 0;
            let mut copy_bytes = 0;
            for (key, _) in &decoded {
                if !t.index.contains_key(key) {
                    new_keys += 1;
                    copy_bytes += key_copy_bytes(key);
                }
            }
            let cap = t.entries.capacity();
            let next = vec_grown(cap, t.entries.len(), decoded.len(), ENTRY_BYTES as usize);
            let need = copy_bytes
                + t.key_slot_growth(new_keys)
                + (next.saturating_sub(cap) as u64) * ENTRY_BYTES
                + counted_bytes;
            if need > retained.bytes() {
                // Admission can evict counts. Inspect the remaining keys again before
                // allocating or interning anything.
                drop(t);
                retained.try_set(need)?;
                continue;
            }
            let mut counted = Vec::with_capacity(decoded.len());
            // Reserve the batch as estimated above. Repeated pushes could otherwise
            // take a different capacity path than one reservation.
            t.entries.reserve(decoded.len());
            t.keys.reserve(new_keys);
            t.index.reserve(new_keys);
            for (key, n) in decoded {
                counted.push((t.intern(key), n));
            }
            counted.sort_unstable();
            for (k, n) in counted {
                t.entries.push_back((commit, k, n));
            }
            let bytes = t.bytes();
            #[cfg(test)]
            {
                t.peak = t.peak.max(bytes);
            }
            break bytes;
        };
        decoded_lease.transfer_to(&mut retained, decoded_lease.bytes());
        if let Some(circuit) = circuit {
            circuit.commit_external_from_scratch(final_bytes, &mut retained)?;
        }
        Ok(())
    }

    /// Count a change in the in-memory tally fixtures.
    #[cfg(test)]
    pub(crate) fn add(&mut self, commit: u64, c: &Changes, term: impl Fn(Id) -> Option<Term>) {
        let mut dirty = false;
        let tally = parking_lot::Mutex::new(std::mem::replace(self, Self::new(Vec::new(), 0)));
        Self::add_budgeted_charged_shared(
            &tally,
            commit,
            c.clone(),
            super::budget::ScratchLease::default(),
            |i, _| Ok(term(i)),
            None,
            &mut dirty,
        )
        .unwrap();
        *self = tally.into_inner();
    }

    pub(crate) fn bytes_after_loss(&self, first: u64, last: u64) -> u64 {
        let merges = self
            .unknown
            .last()
            .is_some_and(|r| first <= r.1.saturating_add(1) && last.saturating_add(1) >= r.0);
        let capacity = self.unknown.capacity();
        let next = sparkles_budget::sizes::vec_grown(
            capacity,
            self.unknown.len(),
            usize::from(!merges),
            RANGE_BYTES as usize,
        );
        self.bytes() + next.saturating_sub(capacity) as u64 * RANGE_BYTES
    }

    /// Record that the commits `first..=last` were not counted per key.
    pub(crate) fn lose(&mut self, first: u64, last: u64) {
        if first > last {
            return;
        }
        // losses come in commit order: one that meets or overlaps the latest joins it
        match self.unknown.last_mut() {
            Some(r) if first <= r.1.saturating_add(1) && last.saturating_add(1) >= r.0 => {
                r.0 = r.0.min(first);
                r.1 = r.1.max(last);
            }
            _ => self.unknown.push((first, last)),
        }
    }

    /// Drop the counts of the commits up to `commit`.
    pub(crate) fn discard_through(&mut self, commit: u64) {
        if commit <= self.since {
            return;
        }
        self.discard_prefix(commit);
        if self.entries.capacity() > self.entries.len() + self.entries.len() / 4 + 64
            || self.unknown.capacity() > self.unknown.len() + self.unknown.len() / 4 + 16
            || self.keys.capacity() > self.keys.len() + self.keys.len() / 4 + 16
        {
            self.shrink_spare_capacity();
        }
    }

    fn discard_prefix(&mut self, commit: u64) {
        if commit <= self.since {
            return;
        }
        self.since = commit;
        while self.entries.front().is_some_and(|e| e.0 <= commit) {
            self.entries.pop_front();
        }
        self.unknown.retain(|r| r.1 > commit);
        if self.entries.is_empty() {
            self.entries = VecDeque::new();
            self.keys = Vec::new();
            self.index = FxHashMap::default();
            self.key_bytes = 0;
            self.index_capacity = 0;
        } else {
            self.compact_keys();
        }
        if self.unknown.is_empty() {
            self.unknown = Vec::new();
        }
    }

    /// Drop the oldest commits' counts until the counts take at most `bytes`, keys and
    /// unknown ranges they alone hold included. Whether any were dropped.
    pub(crate) fn shrink_to(&mut self, bytes: u64) -> bool {
        if self.bytes() <= bytes {
            return false;
        }
        let old_since = self.since;
        // Leave room for future counts so that eviction works in batches.
        let target = bytes - bytes / 4;
        if self.used() > bytes - bytes / 8 {
            let through = self.through_for(target, true);
            self.discard_prefix(through);
        }
        self.fit(bytes);
        if self.bytes() > bytes {
            self.since = self.since.max(self.last_counted_commit());
            self.clear_storage();
        }
        #[cfg(test)]
        {
            self.drops += u64::from(self.since > old_since);
        }
        self.since > old_since
    }

    /// The estimated bytes once the backing tables have been compacted.
    fn used(&self) -> u64 {
        self.entries.len() as u64 * ENTRY_BYTES
            + self.key_bytes
            + self.unknown.len() as u64 * RANGE_BYTES
            + (self.keys.len() * KEY_SLOT) as u64
            + sparkles_budget::sizes::table_bytes(
                sparkles_budget::sizes::table_capacity(self.index.len()),
                INDEX_SLOT,
            ) as u64
    }

    /// Fit the storage to `bytes`, keeping room for incoming counts.
    fn fit(&mut self, bytes: u64) {
        #[cfg(test)]
        {
            self.resizes += 1;
        }
        let admitted = self.entries.capacity();
        self.shrink_spare_capacity();
        let fixed = self.bytes() - self.entries.capacity() as u64 * ENTRY_BYTES;
        let room = ((bytes.saturating_sub(bytes / 8).saturating_sub(fixed) / ENTRY_BYTES) as usize)
            .min(admitted);
        // Growing entry capacity is only useful when there are retained entries and
        // the allocation is smaller than the capacity we admitted before shrinking.
        if room > self.entries.len() {
            self.entries.reserve_exact(room - self.entries.len());
        }
    }

    /// Walk complete commits from the newest back, finding the longest suffix that
    /// fits. The reclaim hook counts only key payloads since its backing allocations
    /// stay as they are. Normal pruning also estimates the compacted buffers.
    fn through_for(&mut self, limit: u64, compact_buffers: bool) -> u64 {
        for key in &mut self.keys {
            key.remap = UNUSED_KEY;
        }
        let mut entries = self.entries.iter().rev().peekable();
        let mut unknown = self.unknown.iter().rev().peekable();
        let mut payload = 0u64;
        let (mut keys, mut counts, mut ranges) = (0, 0, 0);
        loop {
            let commit = entries
                .peek()
                .map_or(self.since, |e| e.0)
                .max(unknown.peek().map_or(self.since, |r| r.1));
            if commit <= self.since {
                return self.since;
            }
            while let Some(&(_, k, _)) = entries.next_if(|e| e.0 == commit) {
                counts += 1;
                let key = &mut self.keys[k as usize];
                if key.remap == UNUSED_KEY {
                    key.remap = 0;
                    payload += key.bytes;
                    keys += 1;
                }
            }
            ranges += u64::from(unknown.next_if(|r| r.1 == commit).is_some());
            let mut bytes = payload;
            if compact_buffers {
                use sparkles_budget::sizes::{table_bytes, table_capacity};
                bytes += counts * ENTRY_BYTES
                    + ranges * RANGE_BYTES
                    + (keys * KEY_SLOT + table_bytes(table_capacity(keys), INDEX_SLOT)) as u64;
            }
            if bytes > limit {
                return commit;
            }
        }
    }

    /// Forget keys no retained entry names, updating the stored indices in place.
    fn compact_keys(&mut self) {
        for key in &mut self.keys {
            key.remap = UNUSED_KEY;
        }
        for &(_, k, _) in &self.entries {
            self.keys[k as usize].remap = 0;
        }
        let mut next = 0;
        self.key_bytes = 0;
        for key in &mut self.keys {
            if key.remap != UNUSED_KEY {
                key.remap = next;
                next += 1;
                self.key_bytes += key.bytes;
            }
        }
        for (_, k, _) in &mut self.entries {
            *k = self.keys[*k as usize].remap;
        }
        self.index.retain(|_, k| {
            *k = self.keys[*k as usize].remap;
            *k != UNUSED_KEY
        });
        self.keys.retain(|key| key.remap != UNUSED_KEY);
        for key in &mut self.keys {
            key.remap = UNUSED_KEY;
        }
    }

    /// The totals per key of the commits after `after` up to `through`.
    pub(crate) fn counts(&self, after: u64, through: u64) -> Counts {
        let unknown = if after < self.since {
            Some(format!(
                "the counts start after commit {}, later than commit {after}",
                self.since
            ))
        } else {
            self.unknown
                .iter()
                .find(|r| r.1 > after && r.0 <= through)
                .map(|r| {
                    if r.0 == r.1 {
                        format!("the changes of commit {} are not known per key", r.0)
                    } else {
                        format!(
                            "the changes of commits {} to {} are not known per key",
                            r.0, r.1
                        )
                    }
                })
        };
        let mut totals: FxHashMap<u32, u64> = FxHashMap::default();
        for &(c, k, n) in &self.entries {
            if c > after && c <= through {
                *totals.entry(k).or_insert(0) += n;
            }
        }
        Counts {
            after,
            through,
            unknown,
            keys: totals
                .into_iter()
                .map(|(k, n)| (self.keys[k as usize].terms.clone(), n))
                .collect(),
        }
    }

    pub(crate) fn encode(&self, out: &mut Enc) {
        out.u64(self.since);
        out.u64(self.unknown.len() as u64);
        for &(a, b) in &self.unknown {
            out.u64(a);
            out.u64(b);
        }
        out.u64(self.keys.len() as u64);
        for k in &self.keys {
            out.u64(k.terms.len() as u64);
            for t in &k.terms {
                match t {
                    None => out.u64(0),
                    Some(t) => {
                        let key = crate::id::term_key(t);
                        out.u64(key.len() as u64 + 1);
                        out.raw(&key);
                    }
                }
            }
        }
        out.u64(self.entries.len() as u64);
        for &(c, k, n) in &self.entries {
            out.u64(c);
            out.u64(k as u64);
            out.u64(n);
        }
    }

    #[cfg(test)]
    pub(crate) fn decode(key_cols: Vec<usize>, d: &mut Dec<'_>) -> Result<Tally> {
        let mut lease = super::budget::ScratchLease::default();
        Self::decode_budgeted(key_cols, d, &mut lease)
    }

    pub(crate) fn decode_budgeted(
        key_cols: Vec<usize>,
        d: &mut Dec<'_>,
        lease: &mut super::budget::ScratchLease,
    ) -> Result<Tally> {
        let width = key_cols.len();
        let mut t = Tally::new(key_cols, d.u64()?);
        for _ in 0..d.count()? {
            let a = d.u64()?;
            let b = d.u64()?;
            let next_cap = sparkles_budget::sizes::vec_grown(
                t.unknown.capacity(),
                t.unknown.len(),
                1,
                RANGE_BYTES as usize,
            );
            let need = (next_cap as u64) * RANGE_BYTES + t.bytes()
                - t.unknown.capacity() as u64 * RANGE_BYTES;
            if need > lease.bytes() {
                lease.try_set(need)?;
            }
            t.unknown.push((a, b));
        }
        for _ in 0..d.count()? {
            let n = d.count()?;
            if n != width {
                return Err(corrupt("a key of another width"));
            }
            let before = t.bytes();
            let mut decode_bytes = (n * TERM_SLOT) as u64;
            lease.try_set(before.saturating_add(decode_bytes))?;
            let mut key = Vec::with_capacity(n);
            for _ in 0..n {
                let len = d.u64()? as usize;
                key.push(if len == 0 {
                    None
                } else {
                    let encoded = d.raw(len - 1)?;
                    decode_bytes += super::terms::decoded_key_heap_bytes(encoded);
                    lease.try_set(before.saturating_add(decode_bytes))?;
                    Some(crate::id::key_to_term(encoded))
                });
            }
            if t.index.contains_key(&key) {
                return Err(corrupt("a duplicate key"));
            }
            lease.try_set(before + decode_bytes + key_copy_bytes(&key) + t.key_slot_growth(1))?;
            t.intern(key);
        }
        let mut last = 0;
        for _ in 0..d.count()? {
            let c = d.u64()?;
            let k = d.u64()?;
            let n = d.u64()?;
            if k as usize >= t.keys.len() || c < last {
                return Err(corrupt("a count out of order or of no key"));
            }
            last = c;
            let next_cap = sparkles_budget::sizes::vec_grown(
                t.entries.capacity(),
                t.entries.len(),
                1,
                ENTRY_BYTES as usize,
            );
            let need = (next_cap as u64) * ENTRY_BYTES + t.bytes()
                - t.entries.capacity() as u64 * ENTRY_BYTES;
            if need > lease.bytes() {
                lease.try_set(need)?;
            }
            t.entries.push_back((c, k as u32, n));
        }
        lease.try_set(t.bytes())?;
        Ok(t)
    }
}

fn corrupt(what: &str) -> Error {
    Error::Corrupt(format!("an internal circuit's state has {what}"))
}

/// LEB128 numbers and raw bytes.
#[derive(Default)]
pub(crate) struct Enc(pub(crate) Vec<u8>);

impl Enc {
    pub(crate) fn u64(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }

    pub(crate) fn raw(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
}

pub(crate) struct Dec<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) at: usize,
}

impl<'a> Dec<'a> {
    pub(crate) fn u64(&mut self) -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = *self.b.get(self.at).ok_or_else(|| corrupt("an early end"))?;
            self.at += 1;
            if shift >= 64 {
                return Err(corrupt("a malformed number"));
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
            shift += 7;
        }
    }

    /// A count of items that each take at least one byte.
    fn count(&mut self) -> Result<usize> {
        let n = self.u64()? as usize;
        if n > self.b.len() - self.at {
            return Err(corrupt("an impossible count"));
        }
        Ok(n)
    }

    pub(crate) fn raw(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| corrupt("an early end"))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }

    pub(crate) fn done(&self) -> bool {
        self.at == self.b.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::NamedNode;

    fn iri(s: &str) -> Option<Term> {
        Some(Term::NamedNode(
            NamedNode::new(format!("http://ex/{s}")).unwrap(),
        ))
    }

    fn tally() -> Tally {
        let mut t = Tally::new(vec![0], 3);
        t.intern(vec![iri("a")]);
        t.intern(vec![iri("b")]);
        t.entries = [(4, 0, 2), (5, 1, 1), (7, 0, 1)].into_iter().collect();
        t
    }

    #[test]
    fn reclaim_does_not_allocate_with_full_credit_and_many_keys() {
        use super::super::budget::ViewBudget;
        use std::sync::Arc;

        let mut t = Tally::new(vec![0], 0);
        let payload = "p".repeat(256);
        for commit in 1..=2048 {
            let key = t.intern(vec![iri(&format!("{payload}{commit}"))]);
            t.entries.push_back((commit, key, 1));
        }
        t.shrink_spare_capacity();
        let full = t.bytes();
        let tally = Arc::new(parking_lot::Mutex::new(t));
        let budget = ViewBudget::new(Some(full), None);
        budget.try_set_external(full).unwrap();
        budget.set_reclaim_external(Some(Arc::new({
            let tally = tally.clone();
            move |needed| tally.lock().evict_for_reclaim(needed).0
        })));
        let previous = budget.handle().remaining_local();
        assert_eq!(previous, Some(0));
        let mut admitted = None;
        let largest = crate::test_alloc::record_largest(|| {
            admitted = Some(budget.handle().try_reserve_pass(full / 8));
        });
        admitted.unwrap().unwrap();
        assert_eq!(largest, 0, "reclaim allocated a {largest}-byte buffer");
        let tally = tally.lock();
        assert!(tally.since > 0 && tally.since < 2048);
        assert!(tally.counts(tally.since, 2048).complete());
        assert_eq!(
            tally
                .counts(2047, 2048)
                .get(&[iri(&format!("{payload}2048"))]),
            1
        );
    }

    #[test]
    fn tally_compaction_frees_the_old_index_before_rebuilding_at_a_full_budget() {
        use super::super::budget::ViewBudget;
        use sparkles_budget::sizes::{table_bytes, table_capacity};

        let mut t = Tally::new(vec![0], 0);
        for commit in 1..=512 {
            let key = t.intern(vec![iri(&format!("key{commit}"))]);
            t.entries.push_back((commit, key, 1));
        }
        t.discard_prefix(256);
        t.entries.shrink_to_fit();
        t.keys.shrink_to_fit();

        let original = table_bytes(table_capacity(t.index.len() + 4096), INDEX_SLOT) as u64;
        let full = t.bytes() - table_bytes(t.index_capacity, INDEX_SLOT) as u64 + original;
        let budget = ViewBudget::new(Some(full), None);
        let peak = crate::test_alloc::record_peak(|| {
            // The old index is allocated inside the observation so the peak includes it.
            t.index.reserve(4096);
            t.index_capacity = t.index.capacity();
            assert_eq!(t.bytes(), full);
            budget.try_set_external(full).unwrap();
            assert_eq!(budget.handle().remaining_local(), Some(0));

            assert!(t.shrink_spare_capacity());
            assert!(t.bytes() < full);
        });
        assert!(
            peak <= original,
            "index compaction peaked at {peak} from {original}"
        );
        let counts = t.counts(256, 512);
        assert!(counts.complete());
        assert_eq!(counts.get(&[iri("key512")]), 1);
        assert_eq!(t.index.get(&vec![iri("key512")]), Some(&255));
    }

    #[test]
    fn empty_accumulator_change_keeps_exact_counts_at_a_full_budget() {
        use super::super::budget::ScratchLease;
        use super::super::circuit::Circuit;
        use super::super::compile::compile;

        let query = crate::sparql::parse_query("SELECT ?s WHERE { ?s ?p ?o }", None, &[]).unwrap();
        let mut circuit = Circuit::new(compile(&query, None).unwrap(), None);
        let t = tally();
        let full = t.bytes();
        circuit.max_memory_bytes = Some(full);
        circuit.try_set_external(full).unwrap();
        assert_eq!(circuit.budget_handle().remaining_local(), Some(0));
        let tally = parking_lot::Mutex::new(t);
        let mut dirty = false;

        Tally::add_budgeted_charged_shared(
            &tally,
            8,
            Changes::new(),
            ScratchLease::default(),
            |_, _| panic!("empty changes have no key to decode"),
            Some(&circuit),
            &mut dirty,
        )
        .unwrap();
        assert!(!dirty);
        assert_eq!(circuit.budget_handle().remaining_local(), Some(0));
        let t = tally.lock();
        assert_eq!(t.bytes(), full);
        assert!(t.counts(7, 8).complete());
        assert!(t.counts(3, 8).complete());
        assert_eq!(t.counts(3, 8).get(&[iri("a")]), 3);
    }

    #[test]
    fn pruning_releases_the_counts_storage() {
        let mut t = Tally::new(vec![0], 0);
        for i in 0..100_000u64 {
            let k = t.intern(vec![iri(&format!("k{i}"))]);
            t.entries.push_back((1, k, 1));
        }
        let k = t.intern(vec![iri("last")]);
        t.entries.push_back((2, k, 1));
        assert!(t.bytes() > 3 << 20, "{}", t.bytes());
        assert!(t.shrink_to(8 << 10));
        assert!(
            t.entries.capacity() as u64 * ENTRY_BYTES <= 8 << 10,
            "{}",
            t.entries.capacity()
        );
        assert!(t.bytes() <= 8 << 10, "{}", t.bytes());
        assert_eq!(t.since, 1);
    }

    #[test]
    fn a_small_drop_meets_the_budget() {
        let mut t = Tally::new(vec![0], 0);
        let k = t.intern(vec![iri("a")]);
        for c in 1..=4 {
            t.entries.push_back((c, k, 1));
        }
        // A one-entry reduction can only force eviction if there is no spare capacity.
        t.shrink_spare_capacity();
        let budget = t.bytes() - ENTRY_BYTES;
        assert!(t.shrink_to(budget));
        assert!(t.bytes() <= budget, "{} over {budget}", t.bytes());
        assert!(t.since >= 1);
        assert!(t.counts(t.since, 4).complete());
    }

    #[test]
    fn counts_at_the_budget_reallocate_in_batches() {
        // one key changed by every commit, held to the budget after each, as a run does
        let budget = 1 << 20;
        let mut t = Tally::new(vec![0], 0);
        let k = t.intern(vec![iri("a")]);
        for c in 1..=50_000 {
            t.entries.push_back((c, k, 1));
            t.shrink_to(budget);
            assert!(t.bytes() <= budget, "{} at commit {c}", t.bytes());
        }
        assert!(t.drops >= 1);
        assert!(t.resizes <= 10, "{} resizes", t.resizes);
        // new keys after a drop fit the room left for them
        let mut t = Tally::new(vec![0], 0);
        for c in 1..=20_000u64 {
            let k = t.intern(vec![iri(&format!("k{c}"))]);
            t.entries.push_back((c, k, 1));
            t.shrink_to(budget);
            assert!(t.bytes() <= budget, "{} at commit {c}", t.bytes());
        }
        assert!(t.resizes <= 200, "{} resizes", t.resizes);
    }

    #[test]
    fn discarded_ranges_release_their_storage() {
        let mut t = Tally::new(vec![0], 0);
        for c in 0..100_000u64 {
            t.lose(2 * c + 1, 2 * c + 1);
        }
        assert!(t.bytes() >= 100_000 * RANGE_BYTES);
        t.discard_through(u64::MAX - 1);
        assert!(t.unknown.capacity() < 32, "{}", t.unknown.capacity());
        assert!(t.bytes() < 1 << 10, "{}", t.bytes());
    }

    #[test]
    fn large_keys_and_unknown_ranges_are_charged() {
        let mut t = Tally::new(vec![0], 0);
        let big = Some(Term::Literal(oxrdf::Literal::new_simple_literal(
            "x".repeat(1 << 20),
        )));
        let n = t.intern(vec![big]);
        t.entries.push_back((1, n, 1));
        assert!(t.bytes() >= 2 << 20, "{}", t.bytes());
        // dropping the only count that names the key frees it
        assert!(t.shrink_to(1 << 10));
        assert!(t.bytes() < 1 << 10, "{}", t.bytes());
        // adjacent and overlapping losses join one range
        for c in 2..1000 {
            t.lose(c, c);
        }
        t.lose(500, 1200);
        assert_eq!(t.unknown, vec![(2, 1200)]);
        t.lose(1300, 1300);
        assert_eq!(t.unknown.len(), 2);
    }

    #[test]
    fn counts_sum_the_commits_after_a_commit() {
        let t = tally();
        let c = t.counts(3, 7);
        assert!(c.complete());
        assert_eq!(c.get(&[iri("a")]), 3);
        assert_eq!(c.get(&[iri("b")]), 1);
        let c = t.counts(4, 7);
        assert_eq!(c.get(&[iri("a")]), 1);
        let c = t.counts(2, 7);
        assert!(!c.complete(), "{c:?}");
        // the counts through an earlier commit leave the later ones out
        assert_eq!(t.counts(3, 5).get(&[iri("a")]), 2);
    }

    #[test]
    fn unknown_commits_make_spanning_counts_incomplete() {
        let mut t = tally();
        t.lose(6, 6);
        assert!(t.counts(3, 7).unknown.is_some());
        assert!(t.counts(6, 7).complete());
        assert!(t.counts(3, 5).complete());
        t.discard_through(6);
        assert!(t.unknown.is_empty());
        assert!(t.counts(6, 7).complete());
        assert_eq!(t.counts(6, 7).get(&[iri("a")]), 1);
        assert_eq!(t.keys.len(), 1, "keys no count names are dropped");
    }

    #[test]
    fn the_encoding_round_trips() {
        let mut t = tally();
        t.lose(6, 6);
        t.intern(vec![None]);
        t.entries.push_back((8, 2, 5));
        let mut e = Enc::default();
        t.encode(&mut e);
        let mut d = Dec { b: &e.0, at: 0 };
        let back = Tally::decode(vec![0], &mut d).unwrap();
        assert!(d.done());
        assert_eq!(back.since, t.since);
        assert_eq!(back.unknown, t.unknown);
        assert_eq!(back.keys, t.keys);
        assert_eq!(back.entries, t.entries);
        // a key of another width is refused
        let mut d = Dec { b: &e.0, at: 0 };
        assert!(Tally::decode(vec![0, 1], &mut d).is_err());
    }

    #[test]
    fn shrinking_drops_the_oldest_commits() {
        let mut t = tally();
        let all = t.bytes();
        assert!(t.shrink_to(all - 1));
        assert!(t.since >= 4, "{}", t.since);
        assert!(!t.counts(3, 7).complete());
        assert!(t.counts(t.since, 7).complete());
        assert!(!t.shrink_to(all));
    }
}
