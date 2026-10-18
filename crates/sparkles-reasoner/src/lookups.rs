//! Lookups of the base tier that a kept closure remembers between incremental runs.
//!
//! The backward search and the insertion of an incremental run look the input graphs up
//! with a few bound positions each, and most lookups find a handful of facts or none, or
//! stop at the first fact they find. Read from the store, every such lookup searches a
//! decoded block of 32,768 rows, and the first read of a block decodes it. [`Lookups`]
//! keeps the answer of each lookup, keyed by its bound positions: the whole answer when it
//! has at most [`MAX_FACTS`] facts, and otherwise its first facts, as many as the reader
//! took, up to [`MAX_FACTS`]. A later lookup with the same bindings, in the same run or a
//! later one, reads the kept facts first and the blocks only when it wants more than they
//! hold.
//!
//! The answers of earlier runs are in a table that the run's threads read without a
//! lock, and the answers a run finds go to a table of shards behind locks. Between runs,
//! when the closure moves to the next snapshot, [`Lookups::freeze`] moves the run's
//! answers to the first table.
//!
//! The answers belong to the base tier of one snapshot. When a closure moves to a later
//! snapshot of the same generation, [`Lookups::update`] takes in the triples the input
//! graphs gained and lost: each changes the answers of the eight patterns made by leaving
//! out some of its positions, and with one input graph the answers stay in scan order.
//! With several input graphs, [`Lookups::forget`] drops those answers instead. A closure
//! never moves to another generation: a closure of another generation is dropped with its
//! lookups. Lookups with a term the store does not know (a local id) are not kept, since
//! local ids are not stable between runs.
//!
//! The answers are charged to the run's budget before they are kept. When the budget has
//! no room, or the lookups reached their own limit, the answer is not kept and the next
//! lookup reads the blocks again, so the lookups change speed and never results. Past the
//! limit, a shard of the run's table is emptied before it takes a new answer, and the
//! answers of earlier runs are dropped when the run's answers join them.

use crate::budget::{Budget, MemoryExceeded};
use crate::graph::Triple;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use sparkles_core::id::{Id, Tag};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The most facts an answer may have to be kept.
pub(crate) const MAX_FACTS: usize = 64;

/// Answers with at most this many facts are copied out when read.
pub(crate) const COPY_FACTS: usize = 8;

/// The bytes of one kept answer besides its facts: the table slot with its control byte
/// and the table's load, the shared slice's counts.
const ENTRY: usize = 96;

const SHARDS: usize = 64;

/// Past this many changed triples, and more than the answers kept, the answers are
/// dropped instead of brought up to date: each change costs seven table lookups, which a
/// bulk load of millions of triples would spend before the run's first check of its
/// cancel flag.
const BULK_CHANGES: usize = 1 << 16;

/// The default limit of the lookups of one closure.
pub(crate) const DEFAULT_MAX_BYTES: usize = 64 << 20;

/// A lookup's bound positions, 0 for an unbound one.
pub(crate) type Pattern = [u64; 3];

fn shard(k: &Pattern) -> usize {
    let h = k[0].wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(17)
        ^ k[1].wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ k[2].wrapping_mul(0x1656_67B1_9E37_79F9).rotate_left(31);
    (h >> 58) as usize % SHARDS
}

fn bytes_of(facts: usize) -> usize {
    ENTRY + facts * size_of::<Triple>()
}

/// The patterns a triple matches, with at least one position bound.
fn patterns(t: &Triple) -> impl Iterator<Item = Pattern> {
    let t = *t;
    (1u8..8).map(move |mask| {
        [
            if mask & 1 != 0 { t[0] } else { 0 },
            if mask & 2 != 0 { t[1] } else { 0 },
            if mask & 4 != 0 { t[2] } else { 0 },
        ]
    })
}

/// The kept answer of a lookup: its first facts, in the order a scan of the base tier
/// gives them, and whether they are all of them.
#[derive(Clone)]
pub(crate) struct Answer {
    pub facts: Arc<[Triple]>,
    pub complete: bool,
}

/// A kept answer as [`Lookups::read`] gives it: an answer of an earlier run, which no
/// one changes during a run, or one of this run, copied out, as the number of facts copied
/// and whether they are all of them.
pub(crate) enum Read<'a> {
    Frozen(&'a Answer),
    Copied(usize, bool),
}

pub(crate) struct Lookups {
    /// the answers of earlier runs, read without a lock
    frozen: FxHashMap<Pattern, Answer>,
    /// the answers this run found
    hot: Box<[Mutex<FxHashMap<Pattern, Answer>>]>,
    /// the bytes charged for the answers of both tables
    bytes: AtomicUsize,
    max_bytes: usize,
    budget: Arc<Budget>,
    /// lookups answered from the tables and lookups not answered, counted in tests only:
    /// shared counters would cost the parallel searches more than the lookups save
    hits: AtomicUsize,
    misses: AtomicUsize,
}

impl std::fmt::Debug for Lookups {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lookups")
            .field("bytes", &self.bytes())
            .field("answers", &self.len())
            .finish()
    }
}

impl Lookups {
    pub fn new(budget: &Arc<Budget>, max_bytes: usize) -> Lookups {
        Lookups {
            frozen: FxHashMap::default(),
            hot: (0..SHARDS).map(|_| Mutex::default()).collect(),
            bytes: AtomicUsize::new(0),
            max_bytes,
            budget: budget.clone(),
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
        }
    }

    /// The key of a lookup, or `None` when it is not kept: one with no bound position,
    /// or with a local id.
    #[inline]
    pub fn pattern(s: Option<u64>, p: Option<u64>, o: Option<u64>) -> Option<Pattern> {
        let k = [s.unwrap_or(0), p.unwrap_or(0), o.unwrap_or(0)];
        if k == [0; 3] || k.iter().any(|&x| x != 0 && Id(x).tag() == Tag::Local) {
            return None;
        }
        Some(k)
    }

    #[inline]
    fn count(&self, found: bool) {
        if !cfg!(test) {
            return;
        }
        if found {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The kept answer of a lookup: one of an earlier run as it is, without a lock, and
    /// otherwise this run's first facts, copied into `buf` under the shard's lock. At most
    /// [`COPY_FACTS`] facts are copied, and a longer answer of this run is read from the
    /// blocks again. This run's answer is preferred to an earlier run's first facts that
    /// it extends, which takes the lock only for answers that stopped short of
    /// [`MAX_FACTS`].
    #[inline]
    pub fn read<'a>(&'a self, k: &Pattern, buf: &mut [Triple; COPY_FACTS]) -> Option<Read<'a>> {
        let frozen = self.frozen.get(k);
        if let Some(a) = frozen
            && (a.complete || a.facts.len() >= MAX_FACTS)
        {
            self.count(true);
            return Some(Read::Frozen(a));
        }
        let kept = frozen.map_or(0, |a| a.facts.len());
        let copied = self.hot[shard(k)].lock().get(k).and_then(|a| {
            let n = a.facts.len().min(COPY_FACTS);
            let complete = a.complete && n == a.facts.len();
            // the copy serves when it says more than the earlier run's facts
            (frozen.is_none() || complete || n > kept).then(|| {
                buf[..n].copy_from_slice(&a.facts[..n]);
                Read::Copied(n, complete)
            })
        });
        let r = copied.or(frozen.map(Read::Frozen));
        self.count(r.is_some());
        r
    }

    /// Whether the kept answer of a lookup has a fact, and whether it is whole.
    #[inline]
    pub fn peek(&self, k: &Pattern) -> Option<(bool, bool)> {
        let a = |a: &Answer| (!a.facts.is_empty(), a.complete);
        let r = match self.frozen.get(k) {
            Some(f) => Some(a(f)),
            None => self.hot[shard(k)].lock().get(k).map(a),
        };
        self.count(r.is_some());
        r
    }

    /// Keep the first facts of a lookup's answer, `complete` when they are all of them,
    /// when they fit the limit and the budget and say more than what is kept already.
    pub fn put(&self, k: Pattern, facts: &[Triple], complete: bool) {
        if facts.len() > MAX_FACTS {
            return;
        }
        let more = |a: &Answer| !a.complete && (complete || a.facts.len() < facts.len());
        if self.frozen.get(&k).is_some_and(|a| !more(a)) {
            return;
        }
        let n = bytes_of(facts.len());
        let mut sh = self.hot[shard(&k)].lock();
        let old = match sh.get(&k) {
            Some(a) if !more(a) => return,
            Some(a) => bytes_of(a.facts.len()),
            None => 0,
        };
        if self.bytes.load(Ordering::Relaxed) + n > self.max_bytes + old {
            // start this shard over
            let freed: usize = sh.values().map(|v| bytes_of(v.facts.len())).sum();
            sh.clear();
            sh.shrink_to_fit();
            self.release(freed);
            if self.bytes.load(Ordering::Relaxed) + n > self.max_bytes {
                return;
            }
        }
        if self.budget.try_charge(n).is_err() {
            return;
        }
        self.bytes.fetch_add(n, Ordering::Relaxed);
        let answer = Answer {
            facts: Arc::from(facts),
            complete,
        };
        if let Some(a) = sh.insert(k, answer) {
            drop(sh);
            self.release(bytes_of(a.facts.len()));
        }
    }

    fn release(&self, n: usize) {
        if n > 0 {
            self.bytes.fetch_sub(n, Ordering::Relaxed);
            self.budget.release(n);
        }
    }

    /// Move the answers this run found to the table of earlier runs, where they replace
    /// the answers they extend. When the answers together pass the limit, which the
    /// growth of [`update`](Self::update) may cause, the earlier ones are dropped.
    pub fn freeze(&mut self) {
        if self.bytes.load(Ordering::Relaxed) > self.max_bytes {
            let freed: usize = self.frozen.values().map(|v| bytes_of(v.facts.len())).sum();
            self.frozen = FxHashMap::default();
            self.release(freed);
        }
        let hot: usize = self.hot.iter_mut().map(|s| s.get_mut().len()).sum();
        if hot == 0 {
            return;
        }
        self.frozen.reserve(hot);
        let mut freed = 0;
        for sh in self.hot.iter_mut() {
            let sh = sh.get_mut();
            for (k, a) in sh.drain() {
                if let Some(old) = self.frozen.insert(k, a) {
                    freed += bytes_of(old.facts.len());
                }
            }
            sh.shrink_to_fit();
        }
        self.release(freed);
    }

    /// Bring the kept answers from one snapshot of the base tier to a later one, given
    /// the triples the tier gained (`added`) and lost (`removed`) in between. `rank` gives
    /// the place of a triple in the scan of a pattern, as a key that sorts in scan order;
    /// it must not depend on anything but the pattern and the triple, which holds with one
    /// input graph (see [`crate::facts::rank`]). A lost triple leaves the answers it
    /// is in. A gained triple joins a whole answer at its place, and the first facts of an
    /// answer when it comes before the last of them; an answer that grows past
    /// [`MAX_FACTS`] keeps its first facts. An answer whose growth the budget refuses is
    /// dropped, and a bulk change drops every answer.
    pub fn update(
        &mut self,
        added: &[Triple],
        removed: &[Triple],
        rank: impl Fn(&Pattern, &Triple) -> Triple,
    ) {
        self.freeze();
        if self.frozen.is_empty() {
            return;
        }
        if self.bulk(added.len() + removed.len()) {
            self.clear();
            return;
        }
        let (mut grown, mut freed) = (0, 0);
        for (t, gained) in removed
            .iter()
            .map(|t| (t, false))
            .chain(added.iter().map(|t| (t, true)))
        {
            for k in patterns(t) {
                let Some(a) = self.frozen.get(&k) else {
                    continue;
                };
                let mut facts: Vec<Triple> = a.facts.to_vec();
                let mut complete = a.complete;
                if gained {
                    let r = rank(&k, t);
                    let at = facts.partition_point(|x| rank(&k, x) < r);
                    if (at == facts.len() && !complete) || facts.get(at) == Some(t) {
                        // after the kept first facts, or kept already
                        continue;
                    }
                    facts.insert(at, *t);
                    if facts.len() > MAX_FACTS {
                        facts.truncate(MAX_FACTS);
                        complete = false;
                    }
                } else {
                    let Some(at) = facts.iter().position(|x| x == t) else {
                        continue;
                    };
                    facts.remove(at);
                }
                let (old, new) = (bytes_of(a.facts.len()), bytes_of(facts.len()));
                if new > old {
                    if self.budget.try_charge(new - old).is_err() {
                        self.frozen.remove(&k);
                        freed += old;
                        continue;
                    }
                    grown += new - old;
                } else {
                    freed += old - new;
                }
                self.frozen.insert(
                    k,
                    Answer {
                        facts: Arc::from(facts),
                        complete,
                    },
                );
            }
        }
        self.bytes.fetch_add(grown, Ordering::Relaxed);
        self.release(freed);
    }

    /// Drop the answers that a change of the triples `ts` may make wrong: those of every
    /// pattern each triple matches, or all of them for a bulk change.
    pub fn forget<'a>(&mut self, ts: impl IntoIterator<Item = &'a Triple>) {
        self.freeze();
        let ts = ts.into_iter();
        if self.bulk(ts.size_hint().0) {
            self.clear();
            return;
        }
        let mut freed = 0;
        for t in ts {
            for k in patterns(t) {
                if let Some(v) = self.frozen.remove(&k) {
                    freed += bytes_of(v.facts.len());
                }
            }
        }
        self.release(freed);
    }

    /// Whether `changes` triples are too many to bring the answers up to date with.
    fn bulk(&self, changes: usize) -> bool {
        changes > BULK_CHANGES && changes > self.frozen.len()
    }

    /// Drop every answer.
    pub fn clear(&mut self) {
        self.freeze();
        let freed: usize = self.frozen.values().map(|v| bytes_of(v.facts.len())).sum();
        self.frozen = FxHashMap::default();
        self.release(freed);
    }

    /// Charge the answers to `budget` from now on, or fail when they do not fit.
    pub fn set_budget(&mut self, budget: &Arc<Budget>) -> Result<(), MemoryExceeded> {
        if Arc::ptr_eq(&self.budget, budget) {
            return Ok(());
        }
        let n = self.bytes.load(Ordering::Relaxed);
        budget.try_charge(n)?;
        self.budget.release(n);
        self.budget = budget.clone();
        Ok(())
    }

    /// The bytes charged for the answers.
    pub fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    /// The number of answers kept.
    pub fn len(&self) -> usize {
        let hot: usize = self.hot.iter().map(|s| s.lock().len()).sum();
        self.frozen.len() + hot
    }

    /// Every kept answer, of both tables.
    #[cfg(test)]
    pub fn answers(&self) -> Vec<(Pattern, Vec<Triple>, bool)> {
        let mut out: Vec<(Pattern, Vec<Triple>, bool)> = self
            .frozen
            .iter()
            .map(|(k, a)| (*k, a.facts.to_vec(), a.complete))
            .collect();
        for sh in self.hot.iter() {
            out.extend(
                sh.lock()
                    .iter()
                    .map(|(k, a)| (*k, a.facts.to_vec(), a.complete)),
            );
        }
        out
    }

    /// (lookups answered from the tables, lookups not answered)
    #[cfg(test)]
    pub fn counts(&self) -> (usize, usize) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}

impl Drop for Lookups {
    fn drop(&mut self) {
        let n = *self.bytes.get_mut();
        if n > 0 {
            self.budget.release(n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(i: u64) -> u64 {
        Id::vocab(i).0
    }

    /// A rank that sorts by subject, then object, as a scan of PSO does for a pattern
    /// with only the predicate bound.
    fn by_subject(_: &Pattern, t: &Triple) -> Triple {
        [t[0], t[2], t[1]]
    }

    fn answer(l: &Lookups, k: &Pattern) -> Option<(Vec<Triple>, bool)> {
        let mut buf = [[0; 3]; COPY_FACTS];
        l.read(k, &mut buf).map(|r| match r {
            Read::Frozen(a) => (a.facts.to_vec(), a.complete),
            Read::Copied(n, c) => (buf[..n].to_vec(), c),
        })
    }

    #[test]
    fn patterns_keep_bound_stable_terms_only() {
        assert_eq!(Lookups::pattern(None, None, None), None);
        assert_eq!(Lookups::pattern(None, Some(v(1)), None), Some([0, v(1), 0]));
        assert_eq!(
            Lookups::pattern(Some(Id::local(3).0), Some(v(1)), None),
            None
        );
        // a triple matches the seven patterns with a bound position
        let t = [v(1), v(2), v(3)];
        let ps: Vec<Pattern> = patterns(&t).collect();
        assert_eq!(ps.len(), 7);
        assert!(
            ps.iter()
                .all(|k| k.iter().zip(&t).all(|(a, b)| *a == 0 || a == b))
        );
    }

    #[test]
    fn answers_say_more_or_are_not_kept() {
        let budget = Budget::unlimited();
        let l = Lookups::new(&budget, DEFAULT_MAX_BYTES);
        let k = [0, v(1), 0];
        let (a, b, c) = ([v(2), v(1), v(9)], [v(3), v(1), v(9)], [v(4), v(1), v(9)]);
        l.put(k, &[a], false);
        assert_eq!(answer(&l, &k), Some((vec![a], false)));
        // fewer first facts say less
        l.put(k, &[], false);
        assert_eq!(answer(&l, &k), Some((vec![a], false)));
        l.put(k, &[a, b, c], true);
        assert_eq!(answer(&l, &k), Some((vec![a, b, c], true)));
        // a whole answer is never replaced
        l.put(k, &[a], false);
        assert_eq!(answer(&l, &k), Some((vec![a, b, c], true)));
        assert_eq!(l.peek(&k), Some((true, true)));
        assert_eq!(l.peek(&[v(7), 0, 0]), None);
        // an answer too long to keep is not
        let long: Vec<Triple> = (0..MAX_FACTS as u64 + 1)
            .map(|i| [v(i), v(1), v(9)])
            .collect();
        l.put([0, 0, v(9)], &long, true);
        assert_eq!(l.peek(&[0, 0, v(9)]), None);
        assert_eq!(l.counts(), (5, 2));
        assert_eq!(l.bytes(), bytes_of(3));
        assert_eq!(budget.used(), l.bytes());
        drop(l);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn answers_of_this_run_are_copied_up_to_a_bound() {
        let budget = Budget::unlimited();
        let mut l = Lookups::new(&budget, DEFAULT_MAX_BYTES);
        let k = [0, v(1), 0];
        let facts: Vec<Triple> = (0..COPY_FACTS as u64 + 2)
            .map(|i| [v(10 + i), v(1), v(2)])
            .collect();
        l.put(k, &facts, true);
        // the copy holds the first facts and is not whole
        assert_eq!(answer(&l, &k), Some((facts[..COPY_FACTS].to_vec(), false)));
        l.freeze();
        assert_eq!(answer(&l, &k), Some((facts.clone(), true)));
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn the_budget_and_the_limit_bound_the_answers() {
        // a budget with room for one answer of one fact
        let budget = Budget::new(Some(bytes_of(1) + bytes_of(1) / 2));
        let l = Lookups::new(&budget, DEFAULT_MAX_BYTES);
        l.put([v(1), 0, 0], &[[v(1), v(2), v(3)]], true);
        l.put([v(4), 0, 0], &[[v(4), v(2), v(3)]], true);
        assert_eq!(l.len(), 1);
        assert_eq!(budget.used(), bytes_of(1));
        // a refused charge leaves no peak behind
        assert_eq!(budget.peak(), bytes_of(1));

        // past their own limit, a shard starts over
        let budget = Budget::unlimited();
        let l = Lookups::new(&budget, 2 * bytes_of(1));
        let keys: Vec<Pattern> = (0..200).map(|i| [v(i), 0, 0]).collect();
        for k in &keys {
            l.put(*k, &[[k[0], v(1), v(2)]], true);
            assert!(l.bytes() <= 2 * bytes_of(1));
        }
        assert!(l.len() <= 2);
        assert_eq!(budget.used(), l.bytes());
    }

    #[test]
    fn freezing_past_the_limit_drops_the_older_answers() {
        let budget = Budget::unlimited();
        let mut l = Lookups::new(&budget, 3 * bytes_of(1));
        l.put([v(1), 0, 0], &[[v(1), v(2), v(3)]], true);
        l.put([v(2), 0, 0], &[[v(2), v(2), v(3)]], true);
        l.freeze();
        l.put([v(3), 0, 0], &[[v(3), v(2), v(3)]], true);
        // a run's answers never pass the limit
        l.put([v(4), 0, 0], &[[v(4), v(2), v(3)]], true);
        assert!(l.bytes() <= 3 * bytes_of(1));
        assert_eq!(l.peek(&[v(1), 0, 0]), Some((true, true)));
        // an update may: the next freeze drops the older answers, also with no new ones
        l.freeze();
        l.update(&[[v(1), v(5), v(6)], [v(1), v(7), v(8)]], &[], |_, t| *t);
        assert!(l.bytes() > 3 * bytes_of(1));
        l.freeze();
        assert_eq!((l.len(), l.bytes()), (0, 0));
        l.put([v(4), 0, 0], &[[v(4), v(2), v(3)]], true);
        assert_eq!(l.peek(&[v(4), 0, 0]), Some((true, true)));
        assert_eq!(budget.used(), l.bytes());
    }

    #[test]
    fn updates_keep_scan_order_and_prefixes() {
        let budget = Budget::unlimited();
        let mut l = Lookups::new(&budget, DEFAULT_MAX_BYTES);
        let p = v(1);
        let f = |s: u64| [v(s), p, v(100)];
        let whole = [0, p, 0];
        let first = [0, 0, v(100)];
        l.put(whole, &[f(10), f(30)], true);
        // the first two facts of a longer answer
        l.put(first, &[f(10), f(30)], false);
        l.freeze();
        l.update(&[f(20), f(40)], &[f(10)], by_subject);
        // a gained fact joins a whole answer at its place, and the first facts of
        // one when it comes before their last
        assert_eq!(answer(&l, &whole), Some((vec![f(20), f(30), f(40)], true)));
        assert_eq!(answer(&l, &first), Some((vec![f(20), f(30)], false)));
        assert_eq!(budget.used(), l.bytes());
        assert_eq!(l.bytes(), bytes_of(3) + bytes_of(2));
        // removing every fact leaves an empty whole answer
        l.update(&[], &[f(20), f(30), f(40)], by_subject);
        assert_eq!(answer(&l, &whole), Some((vec![], true)));
        // a whole answer that grows past the bound keeps its first facts
        let many: Vec<Triple> = (0..MAX_FACTS as u64 + 1).map(|i| f(1000 + i)).collect();
        l.update(&many, &[], by_subject);
        let (facts, complete) = answer(&l, &whole).unwrap();
        assert_eq!(facts, many[..MAX_FACTS].to_vec());
        assert!(!complete);
        assert_eq!(budget.used(), l.bytes());
    }

    #[test]
    fn an_update_the_budget_refuses_drops_the_answer() {
        let budget = Budget::new(Some(bytes_of(1) + 8));
        let mut l = Lookups::new(&budget, DEFAULT_MAX_BYTES);
        let k = [0, v(1), 0];
        l.put(k, &[[v(5), v(1), v(2)]], true);
        l.freeze();
        l.update(&[[v(6), v(1), v(2)]], &[], by_subject);
        assert_eq!(l.peek(&k), None);
        assert_eq!(l.bytes(), 0);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn forget_drops_the_answers_a_triple_may_change() {
        let budget = Budget::unlimited();
        let mut l = Lookups::new(&budget, DEFAULT_MAX_BYTES);
        let t = [v(1), v(2), v(3)];
        l.put([v(1), 0, 0], &[t], true);
        l.put([0, v(2), v(3)], &[t], true);
        l.put([v(9), 0, 0], &[], true);
        l.forget([&t]);
        assert_eq!(l.len(), 1);
        assert_eq!(l.peek(&[v(9), 0, 0]), Some((false, true)));
        l.clear();
        assert_eq!((l.len(), l.bytes(), budget.used()), (0, 0, 0));
    }

    /// A bulk change drops the answers instead of bringing them up to date, and a smaller
    /// one updates them.
    #[test]
    fn bulk_changes_drop_the_answers() {
        let budget = Budget::unlimited();
        let mut l = Lookups::new(&budget, DEFAULT_MAX_BYTES);
        let k = [0, v(1), 0];
        l.put(k, &[[v(5), v(1), v(2)]], true);
        l.freeze();
        let small: Vec<Triple> = (0..1000).map(|i| [v(100 + i), v(9), v(2)]).collect();
        l.update(&small, &[], by_subject);
        assert_eq!(l.len(), 1);
        let many: Vec<Triple> = (0..BULK_CHANGES as u64 + 1)
            .map(|i| [v(100 + i), v(9), v(2)])
            .collect();
        l.update(&many, &[], by_subject);
        assert_eq!((l.len(), l.bytes(), budget.used()), (0, 0, 0));
        l.put(k, &[[v(5), v(1), v(2)]], true);
        l.freeze();
        l.forget(&many);
        assert_eq!(l.len(), 0);
    }

    /// An earlier run's first facts give way to a longer answer of this run.
    #[test]
    fn a_longer_answer_of_this_run_is_read_first() {
        let budget = Budget::unlimited();
        let mut l = Lookups::new(&budget, DEFAULT_MAX_BYTES);
        let k = [0, v(1), 0];
        let (a, b) = ([v(2), v(1), v(9)], [v(3), v(1), v(9)]);
        l.put(k, &[a], false);
        l.freeze();
        l.put(k, &[a, b], true);
        assert_eq!(answer(&l, &k), Some((vec![a, b], true)));
        // a whole answer of an earlier run is read without the lock
        l.freeze();
        assert_eq!(answer(&l, &k), Some((vec![a, b], true)));
    }

    #[test]
    fn the_charge_moves_to_a_new_budget() {
        let first = Budget::unlimited();
        let mut l = Lookups::new(&first, DEFAULT_MAX_BYTES);
        l.put([v(1), 0, 0], &[[v(1), v(2), v(3)]], true);
        let small = Budget::new(Some(8));
        assert!(l.set_budget(&small).is_err());
        assert_eq!(first.used(), l.bytes());
        let second = Budget::unlimited();
        l.set_budget(&second).unwrap();
        assert_eq!((first.used(), second.used()), (0, l.bytes()));
        drop(l);
        assert_eq!(second.used(), 0);
    }
}
