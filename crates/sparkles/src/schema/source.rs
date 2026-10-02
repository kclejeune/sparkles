//! Where the passes of a report read index keys from: the snapshot's permutation
//! indexes, or, when the selection is a few small graphs, their quads copied out of GSPO
//! and sorted in memory.
//!
//! A pass over `PSO[p]` reads every graph's rows of `p` and drops those outside the
//! selection, so a report of a 1,000-quad named graph in a 10M-quad store would read the
//! whole store. When every graph filter of a report is a set of graphs whose quads add up
//! to at most [`SMALL_SELECTION_MAX`] and to at most an eighth of the store, the quads of
//! those graphs are read once from GSPO instead. Each permutation the passes ask for is
//! then sorted once, on first use, and scanned with binary searches.

use super::{Budget, GraphFilter};
use crate::index::{Key, Perm};
use crate::store::{Chunk, Snapshot};
use std::sync::OnceLock;

/// The most quads a selection may hold to be read from GSPO into memory.
pub const SMALL_SELECTION_MAX: u64 = 1 << 20;

/// Index keys of one snapshot, read from its indexes or from a copy of a small selection.
pub(super) struct Src<'s> {
    snap: &'s Snapshot,
    mem: Option<Mem>,
}

/// The quads of the selected graphs (`[s, p, o, g]`), and each permutation's keys once
/// sorted.
struct Mem {
    quads: Vec<[u64; 4]>,
    perms: [OnceLock<Vec<Key>>; 7],
}

impl std::ops::Deref for Src<'_> {
    type Target = Snapshot;

    fn deref(&self) -> &Snapshot {
        self.snap
    }
}

impl<'s> Src<'s> {
    /// Read the snapshot's indexes.
    pub(super) fn direct(snap: &'s Snapshot) -> Src<'s> {
        Src { snap, mem: None }
    }

    /// Copy the selected graphs' quads when every filter is a set of graphs and they are
    /// small (see the module documentation); otherwise read the indexes.
    pub(super) fn for_filters(
        snap: &'s Snapshot,
        filters: &[&GraphFilter],
        budget: &Budget,
    ) -> crate::Result<Src<'s>> {
        let mut graphs: Vec<u64> = Vec::new();
        for f in filters {
            match f {
                GraphFilter::Set(s) => graphs.extend(s.iter().copied()),
                _ => return Ok(Src::direct(snap)),
            }
        }
        graphs.sort_unstable();
        graphs.dedup();
        let mut total = 0u64;
        for &g in &graphs {
            total += snap.count(Perm::Gspo, &[g])?;
        }
        if total > SMALL_SELECTION_MAX || total.saturating_mul(8) > snap.len() {
            return Ok(Src::direct(snap));
        }
        let mut quads = Vec::with_capacity(total as usize);
        for &g in &graphs {
            super::for_each_key(snap, Perm::Gspo, &[g], budget, |k| {
                quads.push(Perm::Gspo.to_quad(k).map(|i| i.0));
            })?;
        }
        Ok(Src {
            snap,
            mem: Some(Mem {
                quads,
                perms: Default::default(),
            }),
        })
    }

    /// Whether the keys come from a copy of the selection.
    #[cfg(test)]
    pub(super) fn is_copied(&self) -> bool {
        self.mem.is_some()
    }

    fn sorted(&self, perm: Perm) -> Option<&[Key]> {
        let m = self.mem.as_ref()?;
        Some(m.perms[perm.index()].get_or_init(|| {
            let mut keys: Vec<Key> = m
                .quads
                .iter()
                .map(|q| perm.to_key(&q.map(crate::id::Id)))
                .collect();
            keys.sort_unstable();
            keys
        }))
    }

    /// Visit every key with a prefix in key order, checking the budget as
    /// [`super::for_each_key`] does.
    pub(super) fn for_each_key(
        &self,
        perm: Perm,
        prefix: &[u64],
        budget: &Budget,
        mut f: impl FnMut(&Key),
    ) -> crate::Result<()> {
        let Some(keys) = self.sorted(perm) else {
            return super::for_each_key(self.snap, perm, prefix, budget, f);
        };
        budget.check()?;
        let n = prefix.len();
        let start = keys.partition_point(|k| k[..n] < *prefix);
        for (i, k) in keys[start..].iter().enumerate() {
            if k[..n] != *prefix {
                break;
            }
            if i % 4096 == 4095 {
                budget.check()?;
            }
            f(k);
        }
        Ok(())
    }

    /// Visit keys with a prefix until `f` returns `false`.
    pub(super) fn scan_until(
        &self,
        perm: Perm,
        prefix: &[u64],
        mut f: impl FnMut(&Key) -> bool,
    ) -> crate::Result<()> {
        let Some(keys) = self.sorted(perm) else {
            return self.snap.scan(perm, prefix, |c| {
                Ok(match c {
                    Chunk::Block(b, s, e) => (s..e).all(|i| f(&b.key(i))),
                    Chunk::Row(k) => f(&k),
                })
            });
        };
        let n = prefix.len();
        let start = keys.partition_point(|k| k[..n] < *prefix);
        for k in &keys[start..] {
            if k[..n] != *prefix || !f(k) {
                break;
            }
        }
        Ok(())
    }

    /// The distinct values of the first key column.
    pub(super) fn distinct_first(&self, perm: Perm) -> crate::Result<Vec<u64>> {
        let Some(keys) = self.sorted(perm) else {
            return self.snap.distinct_first(perm);
        };
        let mut out: Vec<u64> = keys.iter().map(|k| k[0]).collect();
        out.dedup();
        Ok(out)
    }
}
