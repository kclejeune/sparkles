//! Cooperative payload operations for freshly constructed cold dictionaries.
use crate::{error::Result, store::cold::ColdBudget};
use std::{collections::BTreeMap, hash::Hasher, sync::Arc};

const CHUNK: usize = 64 << 10;
// A fixed-scalar BTreeMap node holds eleven pairs and at most twelve edges in
// the pinned standard library. This leaves room for sixteen pairs/edges/header,
// separately from the collision vectors' admitted backing storage.
const NODE_BYTES: u64 =
    (16 * (std::mem::size_of::<(u64, Vec<u64>)>() + std::mem::size_of::<usize>())) as u64;

#[derive(Clone, Default)]
pub(super) struct Index {
    buckets: BTreeMap<u64, Vec<u64>>,
    bytes: u64,
}

fn hash_checked(key: &[u8], mut check: impl FnMut(usize) -> Result<()>) -> Result<u64> {
    let mut hash = rustc_hash::FxHasher::default();
    for (index, chunk) in key.chunks(CHUNK).enumerate() {
        check(index * CHUNK)?;
        hash.write(chunk);
    }
    check(key.len())?;
    Ok(hash.finish())
}

pub(super) fn fingerprint(key: &[u8]) -> u64 {
    hash_checked(key, |_| Ok(())).expect("the unchecked fingerprint cannot fail")
}

pub(super) fn compare_controlled(
    left: &[u8],
    right: &[u8],
    budget: &ColdBudget<'_>,
) -> Result<std::cmp::Ordering> {
    let common = left.len().min(right.len());
    for (index, (a, b)) in left[..common]
        .chunks(CHUNK)
        .zip(right[..common].chunks(CHUNK))
        .enumerate()
    {
        budget.key_step("sparse compare", index * CHUNK)?;
        let order = a.cmp(b);
        if order != std::cmp::Ordering::Equal {
            budget.check()?;
            return Ok(order);
        }
    }
    budget.key_step("sparse compare", common)?;
    Ok(left.len().cmp(&right.len()))
}

fn equal_checked(
    left: &[u8],
    right: &[u8],
    mut check: impl FnMut(usize) -> Result<()>,
) -> Result<bool> {
    check(0)?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (index, (a, b)) in left.chunks(CHUNK).zip(right.chunks(CHUNK)).enumerate() {
        check(index * CHUNK)?;
        if a != b {
            return Ok(false);
        }
    }
    check(left.len())?;
    Ok(true)
}

pub(super) fn copy_controlled(key: &[u8], budget: &ColdBudget<'_>) -> Result<Arc<[u8]>> {
    budget.key_step("copy", 0)?;
    let mut owned = Arc::<[u8]>::new_uninit_slice(key.len());
    let output = Arc::get_mut(&mut owned).expect("a new uninitialized Arc is uniquely owned");
    for (index, (target, source)) in output.chunks_mut(CHUNK).zip(key.chunks(CHUNK)).enumerate() {
        budget.key_step("copy", index * CHUNK)?;
        for (target, byte) in target.iter_mut().zip(source) {
            target.write(*byte);
        }
    }
    budget.key_step("copy", key.len())?;
    // SAFETY: the Arc stayed uniquely owned, every byte of its exact-length slice
    // was initialized by the safe chunk loop, and no reference to uninitialized
    // contents escaped. Any earlier error drops Arc<[MaybeUninit<u8>]> directly.
    Ok(unsafe { owned.assume_init() })
}

impl Index {
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn find(&self, keys: &[Arc<[u8]>], key: &[u8]) -> Option<u64> {
        self.find_hashed(keys, key, fingerprint(key))
    }

    pub(super) fn find_hashed(&self, keys: &[Arc<[u8]>], key: &[u8], hash: u64) -> Option<u64> {
        self.buckets
            .get(&hash)?
            .iter()
            .copied()
            .find(|&id| keys[id as usize].as_ref() == key)
    }

    pub(super) fn find_controlled(
        &self,
        keys: &[Arc<[u8]>],
        key: &[u8],
        budget: &ColdBudget<'_>,
    ) -> Result<(u64, Option<u64>)> {
        let hash = hash_checked(key, |at| budget.key_step("hash", at))?;
        if let Some(bucket) = self.buckets.get(&hash) {
            for &id in bucket {
                budget.check()?;
                if equal_checked(&keys[id as usize], key, |at| budget.key_step("compare", at))? {
                    return Ok((hash, Some(id)));
                }
            }
        }
        budget.check()?;
        Ok((hash, None))
    }

    pub(super) fn insert_controlled(
        &mut self,
        hash: u64,
        id: u64,
        budget: &mut ColdBudget<'_>,
    ) -> Result<()> {
        budget.check()?;
        if let Some(bucket) = self.buckets.get_mut(&hash) {
            let before = bucket.capacity();
            let need = bucket.len() + 1;
            budget.reserve(bucket, need)?;
            self.bytes = self
                .bytes
                .saturating_add((bucket.capacity() - before) as u64 * 8);
            bucket.push(id);
        } else {
            let mut bucket = Vec::new();
            budget.reserve(&mut bucket, 1)?;
            bucket.push(id);
            let levels = (self.buckets.len() as u64).saturating_add(1).ilog2() as u64 + 2;
            let workspace = levels.saturating_mul(NODE_BYTES);
            budget.charge(NODE_BYTES.saturating_add(workspace))?;
            // Only fixed-width fingerprints and IDs participate in the balanced
            // insertion. No payload hashing or copying can occur inside it.
            self.buckets.insert(hash, bucket);
            self.bytes = self.bytes.saturating_add(NODE_BYTES + 8);
            budget.release(workspace);
        }
        // The owner appends its preadmitted key immediately, then checks control.
        // Do not fail here with an index ID whose key has not been appended yet.
        Ok(())
    }

    pub(super) fn insert(&mut self, hash: u64, id: u64) {
        let is_new = !self.buckets.contains_key(&hash);
        let bucket = self.buckets.entry(hash).or_default();
        let before = bucket.capacity();
        bucket.push(id);
        self.bytes = self
            .bytes
            .saturating_add((bucket.capacity() - before) as u64 * 8)
            .saturating_add(if is_new { NODE_BYTES } else { 0 });
    }

    pub(super) fn truncate(&mut self, end: u64) {
        self.buckets.retain(|_, bucket| {
            bucket.retain(|&id| id < end);
            !bucket.is_empty()
        });
        if self.buckets.is_empty() {
            self.buckets = BTreeMap::new();
        }
        self.bytes = (self.buckets.len() as u64)
            .saturating_mul(NODE_BYTES)
            .saturating_add(
                self.buckets
                    .values()
                    .map(|v| v.capacity() as u64 * 8)
                    .sum::<u64>(),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Error,
        history::{HistoryOptions, ReadLimit},
        vocab::AppendVocab,
    };
    use std::{
        cell::Cell,
        sync::atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn large_key_hash_compare_and_copy_stop_after_processing_has_started() {
        for stage in ["hash", "copy", "compare"] {
            let key = vec![b'x'; CHUNK * 4 + 19];
            let cancel = Arc::new(AtomicBool::new(false));
            let options = HistoryOptions {
                cancel: Some(cancel.clone()),
                deadline: None,
            };
            let reached = Cell::new(0);
            let observer = |current: &str, at: usize| {
                if current == stage && at >= CHUNK && at < key.len() {
                    reached.set(at);
                    cancel.store(true, Ordering::Relaxed);
                }
            };
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            let mut vocab = AppendVocab::default();
            if stage == "compare" {
                vocab.push_controlled(&key, false, &mut budget).unwrap();
            }
            let before = vocab.len();
            budget.observe_keys(&observer);
            let result = vocab.push_controlled(&key, false, &mut budget);
            assert!(
                matches!(result, Err(Error::Cancelled)),
                "stage={stage}: {result:?}"
            );
            assert!(
                reached.get() >= CHUNK && reached.get() < key.len(),
                "stage={stage}"
            );
            assert_eq!(
                vocab.len(),
                before,
                "cancelled payload escaped its private builder"
            );
        }
    }

    #[test]
    fn cold_index_preserves_duplicate_ids_native_mutation_and_truncation() {
        let options = HistoryOptions::default();
        let mut vocab = AppendVocab::default();
        {
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            for id in 0..2000u64 {
                let key = format!("<urn:key:{id:08}>");
                assert_eq!(
                    vocab
                        .push_controlled(key.as_bytes(), false, &mut budget)
                        .unwrap(),
                    (id, true)
                );
                assert_eq!(vocab.find(key.as_bytes()), Some(id));
                assert_eq!(vocab.get(id), Some(key.as_bytes()));
            }
            assert!(
                vocab.map.is_empty(),
                "cold construction must not rehash byte payloads"
            );
            assert_eq!(vocab.cold_bytes(), budget.used());
            assert_eq!(
                vocab
                    .push_controlled(b"<urn:key:00000000>", false, &mut budget)
                    .unwrap(),
                (0, false)
            );
            assert_eq!(
                vocab
                    .push_controlled(b"<urn:key:00000000>", true, &mut budget)
                    .unwrap(),
                (2000, true)
            );
            assert_eq!(vocab.find(b"<urn:key:00000000>"), Some(0));
            assert_eq!(vocab.cold_bytes(), budget.used());
        }
        vocab.truncate(2000);
        assert_eq!(vocab.insert(b"<urn:later>"), (2000, true));
        assert_eq!(vocab.insert(b"<urn:later>"), (2000, false));
        vocab.push_dup(b"<urn:later>");
        assert_eq!(vocab.find(b"<urn:later>"), Some(2000));
        let cloned = vocab.clone();
        assert_eq!(cloned.find(b"<urn:later>"), Some(2000));
        vocab.truncate(2000);
        assert_eq!(vocab.find(b"<urn:later>"), None);
        vocab.truncate(0);
        assert_eq!(vocab.cold.as_ref().unwrap().bytes(), 0);
    }

    #[test]
    fn fingerprint_collisions_compare_real_bytes_before_reusing_an_id() {
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(16 << 20),
            held: 0,
        })
        .unwrap();
        let mut first = vec![b'x'; CHUNK * 2 + 17];
        *first.last_mut().unwrap() = b'a';
        let mut second = first.clone();
        *second.last_mut().unwrap() = b'b';
        let mut vocab = AppendVocab::default();
        vocab.push_controlled(&first, false, &mut budget).unwrap();
        // Force a collision with a valid key ID without relying on a hash preimage.
        let hash = fingerprint(&second);
        vocab
            .cold
            .as_mut()
            .unwrap()
            .insert_controlled(hash, 0, &mut budget)
            .unwrap();
        assert_eq!(
            vocab.push_controlled(&second, false, &mut budget).unwrap(),
            (1, true)
        );
        assert_eq!(
            vocab.push_controlled(&second, false, &mut budget).unwrap(),
            (1, false)
        );
        assert_eq!(vocab.find(&first), Some(0));
        assert_eq!(vocab.find(&second), Some(1));
        assert_eq!(vocab.cold_bytes(), budget.used());
    }

    #[test]
    fn recycled_unadmitted_capacity_is_not_a_fresh_cold_dictionary() {
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        })
        .unwrap();
        let mut vocab = AppendVocab::default();
        vocab.insert(b"<urn:old>");
        vocab.truncate(0);
        assert!(vocab.keys.capacity() > 0 && vocab.map.capacity() > 0);
        assert!(matches!(
            vocab.push_controlled(b"<urn:new>", false, &mut budget),
            Err(Error::Invalid(_))
        ));
        assert_eq!(budget.used(), 0);
        assert!(vocab.cold.is_none());
    }
}
