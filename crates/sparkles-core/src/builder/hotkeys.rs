//! The keys that recur across batches, kept once for the whole load.
//!
//! Each batch writes its distinct keys as a partial vocabulary, so a key that many
//! batches use is written, read back and merged once per batch. On the full DBpedia
//! load the partial vocabularies hold about three times the bytes of their distinct
//! keys. Most of the repeats are some tens of millions of IRIs that appear in tens to
//! hundreds of batches each.
//!
//! When a batch is written, each of its keys is looked up here first. A key that is
//! already hot gets its hot id. A key that an earlier batch wrote is made hot while
//! the memory budget allows. Other keys are recorded as seen and stay in the batch.
//! The batch's quads refer to a hot key by its hot id after the ranks of the batch's
//! own keys. The hot keys are written once, as one more partial vocabulary, when the
//! parse ends.
//!
//! Whether an earlier batch wrote a key is answered by a Bloom filter. A false
//! positive makes a key hot that did not need to be, which costs memory but changes
//! nothing else. Every key gets the same global id whichever partial vocabulary holds
//! it, so the index does not depend on which keys are hot.
//!
//! The hot keys and their table cost about the budget (`BuildOptions::hot_key_bytes`),
//! and the filter takes [`SEEN_BYTES`]. Both are held until the parse ends, which is
//! when a load's memory peaks. This trades that memory for fewer bytes of partial
//! vocabularies and a shorter vocabulary merge.

use super::KeySet;
use crate::error::{Error, Result};
use parking_lot::Mutex;
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// The hot keys are split into this many shards, each behind a lock of its own.
const SHARDS: usize = 256;
/// The bytes of the Bloom filter of seen keys, over all shards.
pub(super) const SEEN_BYTES: usize = 256 << 20;
/// The words of the Bloom filter of a shard.
const SEEN_WORDS: usize = SEEN_BYTES / 8 / SHARDS;
/// The bytes a hot key costs on top of its own: its start, its hot id, its slot in the
/// table, and the growth of these vectors.
const ENTRY_COST: u64 = 48;

pub(super) struct HotKeys {
    budget: u64,
    used: AtomicU64,
    /// set once the budget is spent, after which no key is made hot or recorded
    full: AtomicBool,
    next: AtomicU32,
    shards: Box<[Mutex<Shard>]>,
}

#[derive(Default)]
struct Shard {
    keys: KeySet,
    /// the hot id of each key of `keys`
    ids: Vec<u32>,
    /// the Bloom filter of the keys seen in this shard, allocated at its first use
    seen: Vec<u64>,
}

impl HotKeys {
    pub(super) fn new(budget: u64) -> HotKeys {
        HotKeys {
            budget,
            used: AtomicU64::new(0),
            full: AtomicBool::new(budget == 0),
            next: AtomicU32::new(0),
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
        }
    }

    /// The number of hot keys.
    pub(super) fn len(&self) -> u32 {
        self.next.load(Ordering::Relaxed)
    }

    /// The hot id of `key`, if it is hot or is made hot now. A key that stays in its
    /// batch is recorded as seen.
    pub(super) fn classify(&self, key: &[u8]) -> Option<u32> {
        let hash = KeySet::hash(key);
        // the table of a shard uses the low and the top bits of `hash`, so the shard
        // and the filter take theirs from a remix of it
        let mut m = (hash ^ (hash >> 29)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        m ^= m >> 32;
        let mut shard = self.shards[(m >> 56) as usize].lock();
        if let Some(i) = shard.keys.find(hash, key) {
            return Some(shard.ids[i as usize]);
        }
        if self.full.load(Ordering::Relaxed) {
            return None;
        }
        if shard.seen.is_empty() {
            shard.seen = vec![0; SEEN_WORDS];
        }
        let word = (m >> 12) as usize % SEEN_WORDS;
        let bits = (1 << (m & 63)) | (1 << ((m >> 6) & 63));
        if shard.seen[word] & bits != bits {
            shard.seen[word] |= bits;
            return None;
        }
        let cost = key.len() as u64 + ENTRY_COST;
        if self.used.fetch_add(cost, Ordering::Relaxed) + cost > self.budget {
            self.full.store(true, Ordering::Relaxed);
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        shard.keys.insert(hash, key);
        shard.ids.push(id);
        Some(id)
    }

    /// Write the hot keys with `write`, sorted, and free them. Returns what `write`
    /// returns and the hot id of each key in sorted order.
    pub(super) fn write<T>(
        self,
        write: impl FnOnce(&mut dyn Iterator<Item = &[u8]>) -> Result<T>,
    ) -> Result<(T, Vec<u32>)> {
        let shards: Vec<Shard> = self
            .shards
            .into_vec()
            .into_iter()
            .map(|s| {
                let mut s = s.into_inner();
                s.seen = Vec::new();
                s
            })
            .collect();
        let n = self.next.into_inner() as usize;
        // the shard and the index in it of each hot id
        let mut at = vec![(u32::MAX, 0u32); n];
        for (si, s) in shards.iter().enumerate() {
            for (i, &id) in s.ids.iter().enumerate() {
                let slot = at
                    .get_mut(id as usize)
                    .ok_or_else(|| Error::Corrupt("hot key id".into()))?;
                *slot = (si as u32, i as u32);
            }
        }
        let key = |id: u32| {
            let (s, i) = at[id as usize];
            shards[s as usize].keys.key(i)
        };
        let mut order: Vec<u32> = (0..n as u32).collect();
        order.par_sort_unstable_by(|&a, &b| key(a).cmp(key(b)));
        let out = write(&mut order.iter().map(|&id| key(id)))?;
        Ok((out, order))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_seen_twice_is_made_hot() {
        let hot = HotKeys::new(1 << 20);
        assert_eq!(hot.classify(b"a"), None);
        assert_eq!(hot.classify(b"b"), None);
        assert_eq!(hot.classify(b"a"), Some(0));
        assert_eq!(hot.classify(b"a"), Some(0));
        assert_eq!(hot.classify(b"b"), Some(1));
        let ((), order) = hot
            .write(|keys| {
                assert_eq!(keys.collect::<Vec<_>>(), [&b"a"[..], b"b"]);
                Ok(())
            })
            .unwrap();
        assert_eq!(order, [0, 1]);
    }

    #[test]
    fn no_key_is_made_hot_past_the_budget() {
        let hot = HotKeys::new(ENTRY_COST + 1);
        for k in [b"a", b"b", b"a", b"b"] {
            hot.classify(k);
        }
        assert_eq!(hot.len(), 1);
        assert_eq!(hot.classify(b"a"), Some(0));
        assert_eq!(hot.classify(b"b"), None);
        assert!(HotKeys::new(0).classify(b"a").is_none());
    }
}
