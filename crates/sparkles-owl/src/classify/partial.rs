//! Unified, strictly pre-admitted dependency recording for partial classification answers
//! across EL (`Plain`, `Full`), Horn (`Horn`, `HornFull`), and Disjunctive (`Disj`) tiers
//! (roadmap item 714).
//!
//! All scratch storage is pre-sized and admitted against the caller's budget callback
//! before allocation using a compact `BitSet` and a two-pass flat CSR reverse dependency
//! graph. If admission or allocation fails at any step, the pass releases all scratch and
//! falls back cleanly to `(all non-TOP roots, Consistency::Unknown)` without allocating
//! un-admitted memory.

use super::Consistency;
use super::dl_rules;
use super::index::{Ex, Index, Node, TOP};
use super::saturate::{Contexts, Ctx, Dynamic};
use rustc_hash::FxHashSet;
use std::sync::atomic::Ordering;

/// Compact bitset over dense context IDs `0..total` backed by a pre-admitted `Vec<u64>`.
struct BitSet {
    words: Vec<u64>,
    len: usize,
}

impl BitSet {
    fn byte_len(len: usize) -> Option<usize> {
        len.div_ceil(64).checked_mul(std::mem::size_of::<u64>())
    }

    fn try_new(len: usize) -> Option<Self> {
        let words_len = len.div_ceil(64);
        let mut words = Vec::new();
        words.try_reserve_exact(words_len).ok()?;
        words.resize(words_len, 0);
        Some(Self { words, len })
    }

    #[inline]
    fn contains(&self, idx: usize) -> bool {
        if idx >= self.len {
            return false;
        }
        let word = idx >> 6;
        let bit = 1u64 << (idx & 63);
        (self.words[word] & bit) != 0
    }

    #[inline]
    fn insert(&mut self, idx: usize) -> bool {
        if idx >= self.len {
            return false;
        }
        let word = idx >> 6;
        let bit = 1u64 << (idx & 63);
        let slot = &mut self.words[word];
        let was = (*slot & bit) != 0;
        *slot |= bit;
        !was
    }
}

/// Which tier and phase stopped and requires dependency recording.
pub(super) enum RecordKind<'a> {
    ElHorn {
        individuals: bool,
        has_universal_or_keys: bool,
        horn: bool,
        phase2_started: bool,
        individuals_list: &'a [Ex],
        nominals: &'a [Ex],
    },
    Disj {
        rooted: bool,
        refuted: &'a FxHashSet<(Ex, Ex)>,
    },
}

#[inline]
fn all_open(roots: &[Ex]) -> (Vec<Ex>, Consistency) {
    (
        roots.iter().copied().filter(|&r| r != TOP).collect(),
        Consistency::Unknown,
    )
}

/// Visits every forward dependency edge `e -> to` stored on `st` that is not already
/// mirrored in `to.st.back` (or that may target an uncreated/unprocessed context).
#[inline]
fn for_each_forward_edge(
    ix: &Index,
    st: &super::saturate::State,
    kind: &RecordKind<'_>,
    mut f: impl FnMut(Ex),
) {
    for &d in &st.subs {
        if let Node::Exist(..) = ix.head(d).node {
            f(ix.head(d).succ);
        }
    }
    for &(_, target) in &st.fwd.list {
        f(target);
    }
    match kind {
        RecordKind::ElHorn {
            individuals, horn, ..
        } => {
            if let Some(x) = &st.extra {
                if *horn {
                    for &(_, succ) in &x.horn.succ.list {
                        f(succ);
                    }
                    if *individuals {
                        for &partner in &x.horn.same.list {
                            f(partner);
                        }
                    }
                }
                if *individuals {
                    for &nom in &x.noms {
                        f(nom);
                    }
                }
            }
        }
        RecordKind::Disj { .. } => {
            if let Some(d) = dl_rules::dl(st) {
                for &(_, v) in d.edges() {
                    f(v);
                }
            }
        }
    }
}

/// Computes `(incomplete_roots, consistency)` for a classification stopped in Phase 1 or
/// Phase 2, pre-admitting all scratch buffers against `admit` before allocation.
///
/// Never mutates or clears context `todo` queues so callers retain exact control over
/// pending-message accounting and teardown.
pub(super) fn record(
    ix: &Index,
    ctx: &Contexts,
    dynamic: Option<&Dynamic>,
    roots: &[Ex],
    kind: RecordKind<'_>,
    admit: &mut dyn FnMut(usize) -> bool,
) -> (Vec<Ex>, Consistency) {
    let n = ctx.len();
    debug_assert!(dynamic.is_none_or(|d| d.base() as usize == n));
    let total = n + dynamic.map_or(0, |d| d.len());
    let get = |e: Ex| -> Option<&Ctx> {
        let idx = e as usize;
        if idx < n {
            ctx[idx].get().map(|c| &**c)
        } else if let Some(d) = dynamic {
            if d.holds(e) { d.get_ctx(e) } else { None }
        } else {
            None
        }
    };

    // Pass 0 (zero allocations): check whether inconsistency is already proved, and handle
    // regimes that require a conservative `(all_open, Unknown)` answer without a CSR walk.
    if matches!(kind, RecordKind::Disj { rooted: true, .. }) {
        // Rooted Tier D cannot track work across the root context's propagations, so it
        // always leaves every class open and consistency unknown.
        return all_open(roots);
    }

    let is_bottom = |e: Ex| get(e).is_some_and(|c| c.st.lock().bottom);
    let inconsistent = match &kind {
        RecordKind::Disj { rooted: false, .. } => is_bottom(TOP),
        RecordKind::ElHorn {
            individuals,
            individuals_list,
            nominals,
            ..
        } => {
            is_bottom(TOP)
                || (*individuals
                    && (individuals_list.iter().any(|&i| is_bottom(i))
                        || nominals.iter().any(|&i| is_bottom(i))
                        || (0..total as Ex).any(|e| {
                            get(e).is_some_and(|c| {
                                let st = c.st.lock();
                                st.real && st.bottom
                            })
                        })))
        }
        RecordKind::Disj { rooted: true, .. } => unreachable!(),
    };
    if inconsistent {
        return (Vec::new(), Consistency::Inconsistent);
    }

    if let RecordKind::ElHorn {
        has_universal_or_keys: true,
        ..
    } = &kind
    {
        // Pending universal-role facts or key merges can affect any context or cause
        // inconsistency until Phase 2 reaches a fixpoint.
        return all_open(roots);
    }

    // Step 1 admission: `bad_bits` (dense BitSet) + CSR `offsets` (length `total + 1`).
    let Some(bits_bytes) = BitSet::byte_len(total) else {
        return all_open(roots);
    };
    let Some(offsets_len) = total.checked_add(1) else {
        return all_open(roots);
    };
    let Some(offsets_bytes) = offsets_len.checked_mul(std::mem::size_of::<u32>()) else {
        return all_open(roots);
    };
    let Some(step1_bytes) = bits_bytes.checked_add(offsets_bytes) else {
        return all_open(roots);
    };
    if !admit(step1_bytes) {
        return all_open(roots);
    }

    let Some(mut bad_bits) = BitSet::try_new(total) else {
        return all_open(roots);
    };
    let mut offsets: Vec<u32> = Vec::new();
    if offsets.try_reserve_exact(offsets_len).is_err() {
        return all_open(roots);
    }
    offsets.resize(offsets_len, 0);

    // Uncreated roots are locally open.
    for &r in roots {
        if (r as usize) < total && get(r).is_none() {
            bad_bits.insert(r as usize);
        }
    }

    // In `HornFull` (Phase 1 or 2) and `Full` Phase 2, every mandatory context must have
    // completed `Real` marking to be considered closed.
    if let RecordKind::ElHorn {
        individuals: true,
        horn,
        phase2_started,
        individuals_list,
        nominals,
        ..
    } = &kind
        && (*horn || *phase2_started)
    {
        for m in std::iter::once(TOP)
            .chain(individuals_list.iter().copied())
            .chain(nominals.iter().copied())
        {
            if (m as usize) < total && get(m).is_none_or(|c| !c.st.lock().real) {
                bad_bits.insert(m as usize);
            }
        }
    }

    // Pass 1: identify locally open contexts and count incoming reverse edges per target.
    let mut count_overflow = false;
    for e in 0..total as Ex {
        let Some(c) = get(e) else {
            continue;
        };
        let st = c.st.lock();
        let started = c.started.load(Ordering::Acquire) && st.init;
        let has_todo = !c.todo.lock().is_empty();
        let tier_open = match &kind {
            RecordKind::ElHorn {
                individuals, horn, ..
            } => {
                let horn_open = *horn && st.extra.as_ref().is_some_and(|x| !x.horn.is_idle());
                let witness_open = *individuals
                    && !st.real
                    && (!st.noms().is_empty()
                        || (*horn && st.extra.as_ref().is_some_and(|x| x.horn.counted)));
                horn_open || witness_open
            }
            RecordKind::Disj { .. } => dl_rules::dl(&st).is_some_and(|d| !d.is_idle()),
        };
        if !started || st.interrupted || has_todo || tier_open {
            bad_bits.insert(e as usize);
        }
        for_each_forward_edge(ix, &st, &kind, |to| {
            let ti = to as usize;
            if ti >= total {
                return;
            }
            if get(to).is_none() {
                bad_bits.insert(ti);
            }
            let slot = &mut offsets[ti + 1];
            match slot.checked_add(1) {
                Some(next) => *slot = next,
                None => count_overflow = true,
            }
        });
        if count_overflow {
            return all_open(roots);
        }
    }

    // Prefix-sum `offsets` in place.
    for i in 1..offsets_len {
        let Some(sum) = offsets[i - 1].checked_add(offsets[i]) else {
            return all_open(roots);
        };
        offsets[i] = sum;
    }
    let edge_count = offsets[total] as usize;

    // Step 2 admission: `rev_edges` (`edge_count * 4`) + `stack` (`total * 4`).
    let Some(edges_bytes) = edge_count.checked_mul(std::mem::size_of::<Ex>()) else {
        return all_open(roots);
    };
    let Some(stack_bytes) = total.checked_mul(std::mem::size_of::<Ex>()) else {
        return all_open(roots);
    };
    let Some(step2_bytes) = step1_bytes
        .checked_add(edges_bytes)
        .and_then(|b| b.checked_add(stack_bytes))
    else {
        return all_open(roots);
    };
    if !admit(step2_bytes) {
        return all_open(roots);
    }

    let mut rev_edges: Vec<Ex> = Vec::new();
    if rev_edges.try_reserve_exact(edge_count).is_err() {
        return all_open(roots);
    }
    rev_edges.resize(edge_count, 0);

    let mut stack: Vec<Ex> = Vec::new();
    if stack.try_reserve_exact(total).is_err() {
        return all_open(roots);
    }

    // Pass 2: populate CSR `rev_edges` using `offsets[to]` as the write cursor.
    for e in 0..total as Ex {
        let Some(c) = get(e) else {
            continue;
        };
        let st = c.st.lock();
        for_each_forward_edge(ix, &st, &kind, |to| {
            let ti = to as usize;
            if ti >= total {
                return;
            }
            let pos = offsets[ti] as usize;
            rev_edges[pos] = e;
            offsets[ti] += 1;
        });
    }
    // Restore `offsets` in place by shifting right by 1 (`offsets[0] = 0`).
    offsets.copy_within(0..total, 1);
    offsets[0] = 0;

    // Seed `stack` with all initially open contexts.
    for e in 0..total {
        if bad_bits.contains(e) {
            stack.push(e as Ex);
        }
    }

    // Backward reachability over CSR reverse edges + in-place `st.back` (zero allocations).
    while let Some(e) = stack.pop() {
        let ei = e as usize;
        let start = offsets[ei] as usize;
        let end = offsets[ei + 1] as usize;
        for &p in &rev_edges[start..end] {
            if bad_bits.insert(p as usize) {
                stack.push(p);
            }
        }
        if let Some(c) = get(e) {
            let st = c.st.lock();
            for (_, srcs) in &st.back {
                for &p in &srcs.list {
                    let pi = p as usize;
                    if pi < total && bad_bits.insert(pi) {
                        stack.push(p);
                    }
                }
            }
        }
    }

    drop(rev_edges);
    drop(offsets);
    drop(stack);

    // Consistency check over mandatory contexts.
    let mandatory_open = match &kind {
        RecordKind::Disj { .. } => bad_bits.contains(TOP as usize),
        RecordKind::ElHorn {
            individuals,
            individuals_list,
            nominals,
            ..
        } => {
            bad_bits.contains(TOP as usize)
                || (*individuals
                    && (individuals_list
                        .iter()
                        .any(|&i| (i as usize) >= total || bad_bits.contains(i as usize))
                        || nominals
                            .iter()
                            .any(|&i| (i as usize) >= total || bad_bits.contains(i as usize))))
        }
    };
    if mandatory_open {
        return all_open(roots);
    }

    // Ontology is proved consistent; collect incomplete non-TOP roots.
    let incomplete: Vec<Ex> = match &kind {
        RecordKind::ElHorn { .. } => roots
            .iter()
            .copied()
            .filter(|&r| r != TOP && ((r as usize) >= total || bad_bits.contains(r as usize)))
            .collect(),
        RecordKind::Disj { refuted, .. } => {
            // Re-account `bad_bits` live while checking per-root pending candidates.
            let mut cand_admit = |cand_scratch: usize| {
                bits_bytes
                    .checked_add(cand_scratch)
                    .is_some_and(&mut *admit)
            };
            roots
                .iter()
                .copied()
                .filter(|&r| {
                    if r == TOP {
                        return false;
                    }
                    if (r as usize) >= total || bad_bits.contains(r as usize) {
                        return true;
                    }
                    dl_rules::has_pending_candidate(ix, ctx, r, refuted, &mut cand_admit)
                })
                .collect()
        }
    };

    (incomplete, Consistency::Consistent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitset_insert_and_contains_roundtrip() {
        let mut bs = BitSet::try_new(130).unwrap();
        assert!(!bs.contains(0));
        assert!(!bs.contains(63));
        assert!(!bs.contains(64));
        assert!(!bs.contains(129));
        assert!(!bs.contains(130));
        assert!(bs.insert(0));
        assert!(!bs.insert(0));
        assert!(bs.insert(63));
        assert!(bs.insert(64));
        assert!(bs.insert(129));
        assert!(!bs.insert(130));
        assert!(bs.contains(0));
        assert!(bs.contains(63));
        assert!(bs.contains(64));
        assert!(bs.contains(129));
        assert!(!bs.contains(1));
    }
}
