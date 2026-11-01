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
//! [`persist`](super::persist) under the magic `SPKCIRC\0` and format
//! [`INTERNAL_FORMAT`](super::persist::INTERNAL_FORMAT). The body is the circuit's
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
use std::collections::{BTreeMap, VecDeque};
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
}

impl InternalSpec {
    /// A circuit that keeps the rows of `query`.
    pub fn rows(id: impl Into<String>, query: impl Into<String>) -> InternalSpec {
        InternalSpec {
            id: id.into(),
            query: query.into(),
            sink: Sink::Rows,
            max_memory_bytes: super::views::DEFAULT_MEMORY_BYTES,
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
        }
    }

    /// The same spec with a budget of `bytes`.
    pub fn with_memory(mut self, bytes: u64) -> InternalSpec {
        self.max_memory_bytes = bytes;
        self
    }
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
    keys: Vec<Vec<Option<Term>>>,
    index: FxHashMap<Vec<Option<Term>>, u32>,
    /// `(commit, key, count)` in commit order
    entries: VecDeque<(u64, u32, u64)>,
    /// the memory of `keys` and `index` together ([`key_bytes`])
    key_bytes: u64,
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

/// The approximate memory of one key, held once in the list of keys and once in the
/// index: each term's vocabulary key and the term's own size, and the map's slot.
fn key_bytes(key: &[Option<Term>]) -> u64 {
    let terms: u64 = key
        .iter()
        .map(|t| {
            t.as_ref()
                .map_or(0, |t| crate::id::term_key(t).len() as u64)
                + 32
        })
        .sum();
    2 * (terms + 24) + 16
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
        self.entries.capacity() as u64 * ENTRY_BYTES
            + self.key_bytes
            + self.unknown.capacity() as u64 * RANGE_BYTES
    }

    fn intern(&mut self, key: Vec<Option<Term>>) -> u32 {
        if let Some(&n) = self.index.get(&key) {
            return n;
        }
        let n = self.keys.len() as u32;
        self.key_bytes += key_bytes(&key);
        self.keys.push(key.clone());
        self.index.insert(key, n);
        n
    }

    /// Count the change `c` of the circuit's result in `commit`: the absolute weight of
    /// each row, which holds the key and the fact, summed per key. `term` decodes an id
    /// of the change.
    pub(crate) fn add(&mut self, commit: u64, c: &Changes, term: impl Fn(Id) -> Option<Term>) {
        let mut per: FxHashMap<Box<[Id]>, u64> = FxHashMap::default();
        for (row, w) in c {
            let key: Box<[Id]> = self.key_cols.iter().map(|&i| row[i]).collect();
            *per.entry(key).or_insert(0) += w.unsigned_abs();
        }
        let mut counted: Vec<(u32, u64)> = per
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(k, n)| {
                let key = k.iter().map(|&i| term(i)).collect();
                (self.intern(key), n)
            })
            .collect();
        counted.sort_unstable();
        for (k, n) in counted {
            self.entries.push_back((commit, k, n));
        }
        #[cfg(test)]
        {
            self.peak = self.peak.max(self.bytes());
        }
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
        self.since = commit;
        while self.entries.front().is_some_and(|e| e.0 <= commit) {
            self.entries.pop_front();
        }
        self.unknown.retain(|r| r.1 > commit);
        if self.entries.capacity() > self.entries.len() + self.entries.len() / 4 + 64 {
            self.entries.shrink_to_fit();
        }
        if self.unknown.capacity() > self.unknown.len() + self.unknown.len() / 4 + 16 {
            self.unknown.shrink_to_fit();
        }
        self.compact_keys();
    }

    /// Drop the oldest commits' counts until the counts take at most `bytes`, keys and
    /// unknown ranges they alone hold included. Whether any were dropped.
    pub(crate) fn shrink_to(&mut self, bytes: u64) -> bool {
        if self.bytes() <= bytes {
            return false;
        }
        let mut dropped = false;
        // the counts may take seven eighths of the budget, and the rest is the storage's
        // room to grow; a drop goes a quarter below the budget, so that counts at the
        // budget are dropped in batches and not one commit after each commit
        if self.used() > bytes - bytes / 8 {
            let target = bytes - bytes / 4;
            #[cfg(test)]
            {
                self.drops += 1;
            }
            loop {
                let through = self.through_for(target);
                if through <= self.since {
                    break;
                }
                self.discard_through(through);
                dropped = true;
                if self.used() <= target {
                    break;
                }
            }
        }
        self.fit(bytes);
        dropped
    }

    /// The memory the counts hold, without the storage's spare capacity.
    fn used(&self) -> u64 {
        self.entries.len() as u64 * ENTRY_BYTES
            + self.key_bytes
            + self.unknown.len() as u64 * RANGE_BYTES
    }

    /// Fit the storage to `bytes`: the counts' storage keeps room to grow within it, an
    /// eighth of it left for new keys, so that counts arriving after a drop need no
    /// reallocation until the budget is reached again.
    fn fit(&mut self, bytes: u64) {
        #[cfg(test)]
        {
            self.resizes += 1;
        }
        self.unknown.shrink_to_fit();
        self.entries.shrink_to_fit();
        let fixed = self.key_bytes + self.unknown.capacity() as u64 * RANGE_BYTES;
        let room = (bytes.saturating_sub(bytes / 8).saturating_sub(fixed) / ENTRY_BYTES) as usize;
        if room > self.entries.len() {
            self.entries.reserve_exact(room - self.entries.len());
            if self.bytes() > bytes {
                self.entries.shrink_to_fit();
            }
        }
    }

    /// The commit through which dropping the counts leaves at most `bytes`, keys and
    /// unknown ranges that only those counts hold included.
    fn through_for(&self, bytes: u64) -> u64 {
        // the commit of each key's latest count: dropping the counts through that
        // commit frees the key
        let mut last = vec![0u64; self.keys.len()];
        for &(c, k, _) in &self.entries {
            last[k as usize] = c;
        }
        let mut freed_by: BTreeMap<u64, u64> = BTreeMap::new();
        for (k, &c) in last.iter().enumerate() {
            *freed_by.entry(c).or_insert(0) += key_bytes(&self.keys[k]);
        }
        for &(c, _, _) in &self.entries {
            *freed_by.entry(c).or_insert(0) += ENTRY_BYTES;
        }
        for &(_, end) in &self.unknown {
            *freed_by.entry(end).or_insert(0) += RANGE_BYTES;
        }
        // what stays once the discard has shrunk the storage to what it holds
        let mut left = self.entries.len() as u64 * ENTRY_BYTES
            + self.key_bytes
            + self.unknown.len() as u64 * RANGE_BYTES;
        let mut through = self.since;
        for (c, b) in freed_by {
            left = left.saturating_sub(b);
            through = through.max(c);
            if left <= bytes {
                break;
            }
        }
        through
    }

    /// Forget the keys no entry names.
    fn compact_keys(&mut self) {
        let mut used = vec![u32::MAX; self.keys.len()];
        let mut keys = Vec::new();
        for e in self.entries.iter_mut() {
            let n = &mut used[e.1 as usize];
            if *n == u32::MAX {
                *n = keys.len() as u32;
                keys.push(std::mem::take(&mut self.keys[e.1 as usize]));
            }
            e.1 = *n;
        }
        self.index = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), i as u32))
            .collect();
        self.key_bytes = keys.iter().map(|k| key_bytes(k)).sum();
        keys.shrink_to_fit();
        self.keys = keys;
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
                .map(|(k, n)| (self.keys[k as usize].clone(), n))
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
            out.u64(k.len() as u64);
            for t in k {
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

    pub(crate) fn decode(key_cols: Vec<usize>, d: &mut Dec<'_>) -> Result<Tally> {
        let width = key_cols.len();
        let mut t = Tally::new(key_cols, d.u64()?);
        for _ in 0..d.count()? {
            let a = d.u64()?;
            let b = d.u64()?;
            t.unknown.push((a, b));
        }
        for _ in 0..d.count()? {
            let n = d.count()?;
            if n != width {
                return Err(corrupt("a key of another width"));
            }
            let mut key = Vec::with_capacity(n);
            for _ in 0..n {
                let len = d.u64()? as usize;
                key.push(if len == 0 {
                    None
                } else {
                    Some(crate::id::key_to_term(d.raw(len - 1)?))
                });
            }
            let n = t.keys.len() as u32;
            t.key_bytes += key_bytes(&key);
            t.index.insert(key.clone(), n);
            t.keys.push(key);
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
            t.entries.push_back((c, k as u32, n));
        }
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
        t.keys = vec![vec![iri("a")], vec![iri("b")]];
        t.index = t
            .keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.clone(), i as u32))
            .collect();
        t.entries = [(4, 0, 2), (5, 1, 1), (7, 0, 1)].into_iter().collect();
        t.key_bytes = t.keys.iter().map(|k| key_bytes(k)).sum();
        t
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
        t.keys.push(vec![None]);
        t.index.insert(vec![None], 2);
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
