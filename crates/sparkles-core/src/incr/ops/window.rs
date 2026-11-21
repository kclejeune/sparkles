//! An ordered window: `ORDER BY` with `LIMIT k OFFSET m` (spec F13 §4.4).
//!
//! The operator keeps the best `k + m + r` rows of its input in order, where `r`, the
//! reserve, is `max(k, 100)` rows, and emits the change of the rows at positions
//! `m..m + k`. Every row it does not keep orders at or after every row it keeps, and it
//! may keep only some copies of a row. A row enters the kept rows when it orders no later
//! than the last of them, or when they hold the whole input. A change that removes a row
//! from the window promotes the next kept row. When the kept rows fall below `k + m` while
//! the input holds more, the input is evaluated again by its own operators and the best
//! rows refill the reserve, so a run of deletions costs one evaluation per `r` of them.
//!
//! Rows the order does not tell apart keep the order in which they entered the kept rows,
//! which a checkpoint and a generation switch keep, since the rows are saved in order and
//! the order depends on the terms and not on their ids. Keys whose order is not total,
//! such as dates with and without a timezone or NaN, make the operator rebuild its rows
//! from its input at each pass that changes it, since a sorted vector needs a total order.
//!
//! The operator at the root of a SELECT also reports the rows that enter and leave the
//! window with their positions ([`Moves`]). Each kept row has a serial, and its copies are
//! numbered, so a copy that stays in the window is recognised however far it moves. The
//! rows that stay keep their relative order.

use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::prov::{AChanges, Ann};
use super::super::zset::{Changes, Row, consolidate, row_bytes};
use super::Node;
use crate::error::Result;
use crate::sparql::sortkey::{SortKey, TopK, cmp_keys};
use rustc_hash::FxHashMap;
use std::cmp::Ordering;

/// The least reserve of a window, in rows.
pub const MIN_RESERVE: usize = 100;

/// Rows of the window that entered or left it in a pass, each with the position of its
/// first copy (0 is the first row the query returns) and the number of copies, which
/// stand at consecutive positions. Removed rows carry their position before the pass and
/// added rows their position after it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moves {
    pub removed: Vec<(u64, Row, u64)>,
    pub added: Vec<(u64, Row, u64)>,
}

#[derive(Clone, Debug)]
struct Entry {
    row: Row,
    /// the copies kept, at least 1
    w: i64,
    keys: Vec<SortKey>,
    serial: u64,
}

/// The copies `lo..hi` of the entry `serial` stand at the window positions from `pos`.
#[derive(Clone, Debug)]
struct Segment {
    serial: u64,
    lo: i64,
    hi: i64,
    pos: u64,
    row: Row,
}

#[derive(Clone, Debug)]
pub struct Window {
    pub inner: Node,
    /// the key columns of the input, with `true` for ascending
    pub keys: Vec<(usize, bool)>,
    pub limit: usize,
    pub offset: usize,
    /// whether the pass records [`Moves`] (the window of a SELECT's result)
    pub positions: bool,
    asc: Vec<bool>,
    kept: Vec<Entry>,
    /// the copies `kept` holds
    held: i64,
    /// the input's total weight
    total: i64,
    /// the keys are not computed (after a load)
    stale: bool,
    /// a kept key is not totally ordered: each changing pass rebuilds the rows
    partial: bool,
    serial: u64,
    /// the moves of the latest pass, when `positions` is set
    pub moves: Option<Moves>,
    /// the evaluations of the input that refilled the reserve, since the operator was
    /// made
    pub refills: u64,
}

impl Window {
    pub fn new(inner: Node, keys: Vec<(usize, bool)>, limit: usize, offset: usize) -> Window {
        let asc = keys.iter().map(|k| k.1).collect();
        Window {
            inner,
            keys,
            limit,
            offset,
            positions: false,
            asc,
            kept: Vec::new(),
            held: 0,
            total: 0,
            stale: false,
            partial: false,
            serial: 0,
            moves: None,
            refills: 0,
        }
    }

    /// The most copies the operator keeps: the window, the rows before it and the reserve.
    pub fn cap(&self) -> i64 {
        let n = self
            .limit
            .saturating_add(self.offset)
            .saturating_add(self.limit.max(MIN_RESERVE));
        i64::try_from(n).unwrap_or(i64::MAX)
    }

    fn need(&self) -> i64 {
        i64::try_from(self.limit.saturating_add(self.offset)).unwrap_or(i64::MAX)
    }

    fn key_of(&self, p: &Pass, r: &[crate::id::Id]) -> Vec<SortKey> {
        self.keys
            .iter()
            .map(|&(c, _)| SortKey::new(p.value(r[c])))
            .collect()
    }

    fn cmp(&self, a: &[SortKey], b: &[SortKey]) -> Ordering {
        cmp_keys(a, b, &self.asc)
    }

    fn next_serial(&mut self) -> u64 {
        self.serial += 1;
        self.serial
    }

    /// Compute the keys of rows loaded from a checkpoint.
    fn refresh(&mut self, p: &Pass) {
        if !self.stale {
            return;
        }
        let keys: Vec<Vec<SortKey>> = self.kept.iter().map(|e| self.key_of(p, &e.row)).collect();
        for (e, k) in self.kept.iter_mut().zip(keys) {
            e.keys = k;
        }
        self.partial = self
            .kept
            .iter()
            .any(|e| e.keys.iter().any(|k| !k.totally_ordered()));
        self.stale = false;
    }

    /// The index of `row`, whose keys are `keys`, among the kept rows.
    fn find(&self, row: &[crate::id::Id], keys: &[SortKey]) -> Option<usize> {
        let lo = self
            .kept
            .partition_point(|e| self.cmp(&e.keys, keys) == Ordering::Less);
        self.kept[lo..]
            .iter()
            .take_while(|e| self.cmp(&e.keys, keys) == Ordering::Equal)
            .position(|e| *e.row == *row)
            .map(|i| lo + i)
    }

    /// The window as segments of the kept rows.
    fn segments(&self) -> Vec<Segment> {
        let big = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        let (start, end) = (
            big(self.offset),
            big(self.offset).saturating_add(big(self.limit)),
        );
        let mut out = Vec::new();
        let mut at: i64 = 0;
        for e in &self.kept {
            if at >= end {
                break;
            }
            let next = at + e.w;
            let (a, b) = (at.max(start), next.min(end));
            if a < b {
                out.push(Segment {
                    serial: e.serial,
                    lo: a - at,
                    hi: b - at,
                    pos: (a - start) as u64,
                    row: e.row.clone(),
                });
            }
            at = next;
        }
        out
    }

    /// The rows of the window in order, each with its copies.
    /// The slots of the kept rows' vector, for tests of its memory.
    #[cfg(test)]
    pub fn slots(&self) -> usize {
        self.kept.capacity()
    }

    pub fn rows(&self) -> Vec<(Row, i64)> {
        self.segments()
            .into_iter()
            .map(|s| (s.row, s.hi - s.lo))
            .collect()
    }

    /// The copies of `old` that are not in `new`, as runs with their positions.
    fn leaving(old: &[Segment], new: &FxHashMap<u64, (i64, i64)>) -> Vec<(u64, Row, u64)> {
        let mut out = Vec::new();
        for s in old {
            // the copies of [lo, hi) outside the entry's copies in the other window
            let parts = match new.get(&s.serial) {
                Some(&(nlo, nhi)) => [(s.lo, s.hi.min(nlo)), (s.lo.max(nhi), s.hi)],
                None => [(s.lo, s.hi), (0, 0)],
            };
            for (a, b) in parts {
                if a < b {
                    out.push((s.pos + (a - s.lo) as u64, s.row.clone(), (b - a) as u64));
                }
            }
        }
        out
    }

    /// The change of the window from `old` to the current rows, and its moves.
    fn diff(&mut self, old: Vec<Segment>) -> Changes {
        let new = self.segments();
        let idx = |v: &[Segment]| -> FxHashMap<u64, (i64, i64)> {
            v.iter().map(|s| (s.serial, (s.lo, s.hi))).collect()
        };
        let (oi, ni) = (idx(&old), idx(&new));
        let removed = Self::leaving(&old, &ni);
        let added = Self::leaving(&new, &oi);
        let mut out = Changes::new();
        for (_, r, n) in &removed {
            out.push((r.clone(), -(*n as i64)));
        }
        for (_, r, n) in &added {
            out.push((r.clone(), *n as i64));
        }
        if self.positions {
            self.moves = Some(Moves { removed, added });
        }
        consolidate(out)
    }

    /// Drop copies from the end until the kept rows fit the cap.
    fn trim(&mut self) {
        let cap = self.cap();
        if self.held <= cap {
            return;
        }
        while self.held > cap {
            let excess = self.held - cap;
            let last = self.kept.last_mut().expect("held copies");
            if last.w <= excess {
                self.held -= last.w;
                self.kept.pop();
            } else {
                last.w -= excess;
                self.held -= excess;
            }
        }
        if self.kept.capacity() > 2 * self.kept.len() + 64 {
            self.kept.shrink_to_fit();
        }
    }

    /// The best rows of `rows`, the input's whole result. With `keep`, the kept rows that
    /// the input still holds stay in their order, clipped to the input's copies, since
    /// an operator evaluated again can give other rows than its maintained state (a
    /// `SAMPLE` keeps its value while the group holds it, and a fresh one takes the
    /// smallest). The other rows of the best `cap` then join them at their places, and
    /// copies of a row that is kept already join its entry.
    fn fill(&mut self, p: &mut Pass, rows: Changes, keep: bool) -> Result<()> {
        self.total = rows.iter().map(|(_, w)| *w).sum();
        let cap = usize::try_from(self.cap()).unwrap_or(usize::MAX);
        let mut top: TopK<(Row, i64)> = TopK::new(cap, self.asc.clone());
        let mut partial = false;
        let mut left: FxHashMap<Row, i64> = FxHashMap::default();
        let per = self.per_row();
        for (i, (r, w)) in rows.into_iter().enumerate() {
            if w <= 0 {
                continue;
            }
            p.tick()?;
            // the heap is the operator's state to be, charged as it grows
            if i % 4096 == 4095
                && let Some(max) = p.max_memory_bytes
            {
                let used = (top.kept() as u64).saturating_mul(per);
                if used > max {
                    return Err(crate::error::Error::BudgetExceeded(crate::error::Budget {
                        kind: crate::error::BudgetKind::Memory,
                        limit: max,
                        requested: used,
                    }));
                }
            }
            let keys = self.key_of(p, &r);
            partial |= keys.iter().any(|k| !k.totally_ordered());
            if keep {
                left.insert(r.clone(), w);
            }
            top.offer(keys, (r, w));
        }
        if keep && !partial {
            let mut kept = std::mem::take(&mut self.kept);
            self.held = 0;
            kept.retain_mut(|e| {
                let n = left.get_mut(&e.row).map_or(0, |n| {
                    let k = (*n).min(e.w);
                    *n -= k;
                    k
                });
                e.w = n;
                self.held += n;
                n > 0
            });
            self.kept = kept;
        } else {
            self.kept.clear();
            self.held = 0;
            left.clear();
        }
        for e in top.into_sorted() {
            let (row, full) = e.payload;
            let w = if keep && !partial {
                left.remove(&row).unwrap_or(0)
            } else {
                full
            };
            if w <= 0 {
                continue;
            }
            self.held += w;
            if let Some(i) = self.find(&row, &e.keys) {
                self.kept[i].w += w;
                continue;
            }
            let at = self
                .kept
                .partition_point(|k| self.cmp(&k.keys, &e.keys) != Ordering::Greater);
            let serial = self.next_serial();
            self.kept.insert(
                at,
                Entry {
                    row,
                    w,
                    keys: e.keys,
                    serial,
                },
            );
        }
        self.partial = partial;
        self.trim();
        Ok(())
    }

    /// The memory of one kept row besides its keys' strings.
    fn per_row(&self) -> u64 {
        (std::mem::size_of::<Entry>() + self.keys.len() * std::mem::size_of::<SortKey>()) as u64
            + row_bytes(self.inner.schema.len())
    }

    fn inner_init(&mut self, p: &mut Pass) -> Result<Changes> {
        if p.prov.is_none() {
            return Ok(consolidate(self.inner.init(p)?));
        }
        Ok(strip(self.inner.init_a(p)?))
    }

    fn inner_delta(&mut self, p: &mut Pass) -> Result<Changes> {
        if p.prov.is_none() {
            return Ok(consolidate(self.inner.delta(p)?));
        }
        Ok(strip(self.inner.delta_a(p)?))
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        self.moves = None;
        self.stale = false;
        let rows = self.inner_init(p)?;
        self.fill(p, rows, false)?;
        Ok(self.rows())
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        self.moves = None;
        let c = self.inner_delta(p)?;
        if c.is_empty() {
            if self.positions {
                self.moves = Some(Moves::default());
            }
            return Ok(Vec::new());
        }
        self.refresh(p);
        let old = self.segments();
        let keyed: Vec<(Row, i64, Vec<SortKey>)> = c
            .into_iter()
            .map(|(r, w)| {
                let k = self.key_of(p, &r);
                (r, w, k)
            })
            .collect();
        let partial = self.partial
            || keyed
                .iter()
                .any(|(_, _, k)| k.iter().any(|k| !k.totally_ordered()));
        if partial {
            // no sorted vector without a total order: the input's rows again
            let rows = self.inner_init(p)?;
            self.fill(p, rows, false)?;
            return Ok(self.diff(old));
        }
        // removals first, so that an insert sees the kept rows without them
        let (rem, add): (Vec<_>, Vec<_>) = keyed.into_iter().partition(|(_, w, _)| *w < 0);
        for (r, w, k) in rem {
            p.tick()?;
            self.total += w;
            if let Some(i) = self.find(&r, &k) {
                let e = &mut self.kept[i];
                let take = e.w.min(-w);
                e.w -= take;
                self.held -= take;
                if e.w == 0 {
                    self.kept.remove(i);
                }
            }
        }
        for (r, w, k) in add {
            p.tick()?;
            let complete = self.held == self.total;
            self.total += w;
            if let Some(i) = self.find(&r, &k) {
                self.kept[i].w += w;
                self.held += w;
                continue;
            }
            let enters = complete
                || self
                    .kept
                    .last()
                    .is_some_and(|l| self.cmp(&k, &l.keys) != Ordering::Greater);
            if !enters {
                continue;
            }
            let at = self
                .kept
                .partition_point(|e| self.cmp(&e.keys, &k) != Ordering::Greater);
            let serial = self.next_serial();
            self.held += w;
            self.kept.insert(
                at,
                Entry {
                    row: r,
                    w,
                    keys: k,
                    serial,
                },
            );
            // a large insert into a window that held its whole input stays bounded
            if self.held > self.cap() {
                self.trim();
            }
        }
        self.trim();
        if self.held < self.need() && self.total > self.held {
            // the reserve ran out: the input's own operators give its rows again
            self.refills += 1;
            let rows = self.inner_init(p)?;
            self.fill(p, rows, true)?;
        }
        Ok(self.diff(old))
    }

    /// [`init`](Self::init) with annotations. The window's rows carry unknown
    /// derivations, as a `recompute` node's do.
    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        Ok(self
            .init(p)?
            .into_iter()
            .map(|(r, w)| (r, w, Ann::unknown(mode, w)))
            .collect())
    }

    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        Ok(self
            .delta(p)?
            .into_iter()
            .map(|(r, w)| (r, w, Ann::unknown(mode, w)))
            .collect())
    }

    pub fn describe(&self, vars: &[String]) -> String {
        let keys: Vec<String> = self
            .keys
            .iter()
            .map(|&(c, asc)| {
                let v = &vars[self.inner.schema[c] as usize];
                if asc {
                    format!("?{v}")
                } else {
                    format!("DESC(?{v})")
                }
            })
            .collect();
        format!(
            "ORDER BY {} LIMIT {} OFFSET {}, reserve {}",
            keys.join(" "),
            self.limit,
            self.offset,
            self.limit.max(MIN_RESERVE)
        )
    }

    /// Whether the window holds a key without a total order and so rebuilds its rows.
    #[cfg(test)]
    pub(crate) fn partial(&self) -> bool {
        self.partial
    }

    pub fn bytes(&self) -> u64 {
        // the vector's slots, whether used or not, then each row's ids and keys
        let slots = self.kept.capacity() as u64 * std::mem::size_of::<Entry>() as u64;
        let per = self.per_row() - std::mem::size_of::<Entry>() as u64;
        slots
            + self
                .kept
                .iter()
                .map(|e| per + e.keys.iter().map(SortKey::payload_bytes).sum::<u64>())
                .sum::<u64>()
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.i64(self.total);
        w.u64(self.kept.len() as u64);
        for e in &self.kept {
            w.row(&e.row)?;
            w.i64(e.w);
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        let total = r.i64()?;
        let n = r.u64()?;
        let width = self.inner.schema.len();
        let mut kept = Vec::new();
        let mut held = 0i64;
        for _ in 0..n {
            let row = r.row()?;
            let w = r.i64()?;
            if row.len() != width || w <= 0 {
                return Err(crate::error::Error::Corrupt(
                    "a view's window holds a row of another width or weight".into(),
                ));
            }
            held = held.checked_add(w).ok_or_else(|| {
                crate::error::Error::Corrupt("a view's window holds too many rows".into())
            })?;
            self.serial += 1;
            kept.push(Entry {
                row,
                w,
                keys: Vec::new(),
                serial: self.serial,
            });
        }
        if held > total {
            return Err(crate::error::Error::Corrupt(
                "a view's window holds more rows than its input".into(),
            ));
        }
        self.kept = kept;
        self.held = held;
        self.total = total;
        self.stale = true;
        self.moves = None;
        Ok(())
    }
}

fn strip(c: AChanges) -> Changes {
    consolidate(c.into_iter().map(|(r, w, _)| (r, w)).collect())
}
