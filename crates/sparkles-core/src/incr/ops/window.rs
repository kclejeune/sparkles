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

/// A test hook: a count of steps and the flag it raises when the count reaches zero.
#[cfg(test)]
type Hook = std::cell::RefCell<Option<(u64, std::sync::Arc<std::sync::atomic::AtomicBool>)>>;

#[cfg(test)]
thread_local! {
    /// the comparisons of [`Window::cmp`] on this thread, for tests of the work
    static CMPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// the most rows a refill's reconciliation tracked on this thread, for tests of
    /// its memory
    static RECONCILED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// a flag that [`Window::key_of`] raises once it has computed this many keys, for
    /// tests of a cancellation that arrives during a phase
    pub(crate) static CANCEL_AFTER: Hook =
        const { std::cell::RefCell::new(None) };
    /// the same for the rows that [`Window::place`] and [`Window::reconcile`] install
    pub(crate) static CANCEL_PLACING: Hook =
        const { std::cell::RefCell::new(None) };
}

/// Count one step of a test hook down and raise its flag at zero.
#[cfg(test)]
fn count_down(hook: &'static std::thread::LocalKey<Hook>) {
    hook.with(|c| {
        if let Some((n, flag)) = c.borrow_mut().as_mut() {
            *n = n.saturating_sub(1);
            if *n == 0 {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    });
}

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
        #[cfg(test)]
        count_down(&CANCEL_AFTER);
        self.keys
            .iter()
            .map(|&(c, _)| SortKey::new(p.value(r[c])))
            .collect()
    }

    fn cmp(&self, a: &[SortKey], b: &[SortKey]) -> Ordering {
        #[cfg(test)]
        CMPS.with(|c| c.set(c.get() + 1));
        cmp_keys(a, b, &self.asc)
    }

    fn next_serial(&mut self) -> u64 {
        self.serial += 1;
        self.serial
    }

    /// Compute the keys of rows loaded from a checkpoint. The rows stay stale until
    /// every key is computed.
    fn refresh(&mut self, p: &mut Pass) -> Result<()> {
        if !self.stale {
            return Ok(());
        }
        p.control.check()?;
        let base_state = self.bytes();
        let mut used = base_state;
        let mut keys = Vec::with_capacity(self.kept.len());
        for e in &self.kept {
            p.tick()?;
            let k = self.key_of(p, &e.row);
            // `bytes` counts each row's key slots already, so only the strings are new
            used = used.saturating_add(k.iter().map(SortKey::payload_bytes).sum::<u64>());
            charge(p, used, base_state)?;
            keys.push(k);
        }
        p.control.check()?;
        for (e, k) in self.kept.iter_mut().zip(keys) {
            e.keys = k;
        }
        p.commit_window_state(self.bytes(), base_state)?;
        self.partial = self
            .kept
            .iter()
            .any(|e| e.keys.iter().any(|k| !k.totally_ordered()));
        self.stale = false;
        Ok(())
    }

    /// The changes with their rows' keys.
    #[allow(clippy::type_complexity)]
    fn keyed(&self, p: &mut Pass, c: Changes) -> Result<Vec<(Row, i64, Vec<SortKey>)>> {
        p.control.check()?;
        let width = self.inner.schema.len();
        let slot = std::mem::size_of::<(Row, i64, Vec<SortKey>)>() as u64;
        let base_state = self.bytes();
        let mut used = base_state.saturating_add((c.len() as u64).saturating_mul(slot));
        charge(p, used, base_state)?;
        let mut out = Vec::with_capacity(c.len());
        for (r, w) in c {
            p.tick()?;
            let k = self.key_of(p, &r);
            used = used.saturating_add(heap_bytes(&k, width));
            charge(p, used, base_state)?;
            out.push((r, w, k));
        }
        p.control.check()?;
        Ok(out)
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
    ///
    /// Only the kept rows are reconciled, so the bookkeeping is bounded by the window and
    /// not by the input. The heap, its keys' strings, the kept rows a refill still holds
    /// and the reconciliation are charged to the pass's memory budget at every row. A
    /// pass that fails while the rows are placed leaves the window empty with the input's
    /// total, a consistent state, and the runtime then fails the view, which a rebuild
    /// starts again from `init`.
    fn fill(&mut self, p: &mut Pass, rows: Changes, keep: bool) -> Result<()> {
        p.control.check()?;
        let cap = usize::try_from(self.cap()).unwrap_or(usize::MAX);
        let width = self.inner.schema.len();
        let mut top: TopK<(Row, i64, Option<usize>)> = TopK::new(cap, self.asc.clone());
        let mut partial = false;
        let mut total = 0i64;
        // the window's rows stay allocated until the new ones replace them
        let per = std::mem::size_of::<(&[crate::id::Id], usize)>() as u64 + 16;
        let base_state = self.bytes();
        let keep_len = if keep { self.kept.len() } else { 0 };
        let est_index_cap = sparkles_budget::sizes::table_capacity(keep_len) as u64;
        let est_index_bytes = est_index_cap.saturating_mul(per);
        let counts_bytes = (keep_len as u64).saturating_mul(std::mem::size_of::<i64>() as u64);
        let base = base_state
            .saturating_add(est_index_bytes)
            .saturating_add(counts_bytes);
        if keep_len > 0 {
            charge(p, base, base_state)?;
        }
        // a refill tracks each kept row's copies in the input, and nothing else
        let index: FxHashMap<&[crate::id::Id], usize> = if keep {
            self.kept
                .iter()
                .enumerate()
                .map(|(i, e)| (&*e.row, i))
                .collect()
        } else {
            FxHashMap::default()
        };
        let mut counts = vec![0i64; keep_len];
        let base = base_state
            .saturating_add((index.capacity() as u64).saturating_mul(per))
            .saturating_add(counts_bytes);
        let slot = std::mem::size_of::<HeapEntry>() as u64;
        let mut payloads = 0u64;
        for (r, w) in rows {
            total = total.saturating_add(w);
            if w <= 0 {
                continue;
            }
            p.tick()?;
            let keys = self.key_of(p, &r);
            partial |= keys.iter().any(|k| !k.totally_ordered());
            let at = index.get(&*r).copied();
            if let Some(i) = at {
                counts[i] = counts[i].saturating_add(w);
            }
            let entry_bytes = heap_bytes(&keys, width);
            let next_kept = top.kept().saturating_add(1).min(cap);
            let next_slots = if next_kept == 0 {
                0
            } else {
                (next_kept.next_power_of_two() as u64).saturating_mul(slot)
            };
            charge(
                p,
                base.saturating_add(payloads)
                    .saturating_add(entry_bytes)
                    .saturating_add(next_slots),
                base_state,
            )?;
            payloads = payloads.saturating_add(entry_bytes);
            if let Some(out) = top.offer(keys, (r, w, at)) {
                payloads = payloads.saturating_sub(heap_bytes(&out.keys, width));
            }
        }
        drop(index);
        #[cfg(test)]
        RECONCILED.with(|c| c.set(c.get().max(counts.len())));
        // the sort's positions and slots, and the empty input's state
        let slots = if top.kept() == 0 {
            0
        } else {
            (top.kept().next_power_of_two() as u64).saturating_mul(slot)
        };
        let sorting = (top.kept() as u64).saturating_mul(slot + 24);
        charge(
            p,
            base.saturating_add(payloads)
                .saturating_add(slots)
                .saturating_add(sorting),
            base_state,
        )?;
        p.control.check()?;
        let sorted = top.into_sorted();
        p.control.check()?;
        let placed = if keep && !partial {
            self.reconcile(p, sorted, &counts)
        } else {
            self.kept.clear();
            self.held = 0;
            self.place(p, sorted)
        };
        if let Err(e) = placed {
            self.kept.clear();
            self.held = 0;
            self.total = total;
            return Err(e);
        }
        self.total = total;
        self.partial = partial;
        self.trim();
        p.commit_window_state(self.bytes(), base_state)?;
        Ok(())
    }

    /// The heap's rows in order as the kept rows, each with all its copies.
    fn place(&mut self, p: &mut Pass, sorted: Vec<HeapEntry>) -> Result<()> {
        let mut kept = Vec::with_capacity(sorted.len());
        let mut held = 0i64;
        for e in sorted {
            p.tick()?;
            #[cfg(test)]
            count_down(&CANCEL_PLACING);
            let (row, w, _) = e.payload;
            held = held.saturating_add(w);
            kept.push(Entry {
                row,
                w,
                keys: e.keys,
                serial: self.next_serial(),
            });
        }
        p.control.check()?;
        self.kept = kept;
        self.held = held;
        Ok(())
    }

    /// Merge the heap's rows into the kept rows that the input still holds, whose copies
    /// in the input are `counts`. A kept row keeps at most its input's copies, its other
    /// copies in the heap join its entry, and the heap's other rows take their places
    /// after the kept rows they tie with.
    fn reconcile(&mut self, p: &mut Pass, sorted: Vec<HeapEntry>, counts: &[i64]) -> Result<()> {
        let mut extra = vec![0i64; counts.len()];
        let mut fresh = Vec::new();
        for e in sorted {
            p.tick()?;
            let (row, full, at) = e.payload;
            match at {
                Some(i) => {
                    let retained = counts[i].min(self.kept[i].w);
                    extra[i] = extra[i].saturating_add(full - retained);
                }
                None => fresh.push((row, full, e.keys)),
            }
        }
        let old = std::mem::take(&mut self.kept);
        self.held = 0;
        let mut kept = Vec::with_capacity(old.len().saturating_add(fresh.len()));
        let mut held = 0i64;
        let mut fresh = fresh.into_iter().peekable();
        for (i, mut e) in old.into_iter().enumerate() {
            let w = counts[i].min(e.w).saturating_add(extra[i]);
            if w <= 0 {
                continue;
            }
            while let Some(f) = fresh.next_if(|f| self.cmp(&f.2, &e.keys) == Ordering::Less) {
                p.tick()?;
                #[cfg(test)]
                count_down(&CANCEL_PLACING);
                held = held.saturating_add(f.1);
                kept.push(Entry {
                    row: f.0,
                    w: f.1,
                    keys: f.2,
                    serial: self.next_serial(),
                });
            }
            p.tick()?;
            #[cfg(test)]
            count_down(&CANCEL_PLACING);
            e.w = w;
            held = held.saturating_add(w);
            kept.push(e);
        }
        for f in fresh {
            p.tick()?;
            #[cfg(test)]
            count_down(&CANCEL_PLACING);
            held = held.saturating_add(f.1);
            kept.push(Entry {
                row: f.0,
                w: f.1,
                keys: f.2,
                serial: self.next_serial(),
            });
        }
        p.control.check()?;
        self.kept = kept;
        self.held = held;
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
        let mark = p.output_lease_mark();
        let rows = self.inner_init(p)?;
        let rows_leases = p.take_output_leases_since(mark);
        self.fill(p, rows, false)?;
        drop(rows_leases);
        Ok(self.rows())
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        self.moves = None;
        let mark = p.output_lease_mark();
        let c = self.inner_delta(p)?;
        let c_leases = p.take_output_leases_since(mark);
        if c.is_empty() {
            drop(c_leases);
            if self.positions {
                self.moves = Some(Moves::default());
            }
            return Ok(Vec::new());
        }
        self.refresh(p)?;
        let base_state = self.bytes();
        let mut old_lease = p.scratch_lease();
        let width = self.inner.schema.len();
        let seg_est = (self.kept.len().min(self.limit) as u64)
            .saturating_mul(std::mem::size_of::<Segment>() as u64 + row_bytes(width));
        old_lease.try_reserve(seg_est)?;
        let old = self.segments();
        let keyed = self.keyed(p, c)?;
        drop(c_leases);
        let partial = self.partial
            || keyed
                .iter()
                .any(|(_, _, k)| k.iter().any(|k| !k.totally_ordered()));
        if partial {
            // no sorted vector without a total order: the input's rows again
            drop(keyed);
            p.clear_window_scratch();
            let rmark = p.output_lease_mark();
            let rows = self.inner_init(p)?;
            let r_leases = p.take_output_leases_since(rmark);
            self.fill(p, rows, false)?;
            drop(r_leases);
            let diff = self.diff(old);
            drop(old_lease);
            return Ok(diff);
        }
        // Pre-admit the split remove/add vectors before partitioning `keyed`.
        let split_slot = std::mem::size_of::<(Row, i64, Vec<SortKey>)>() as u64;
        let mut split_lease = p.scratch_lease();
        split_lease.try_reserve((keyed.len() as u64).saturating_mul(split_slot))?;
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
        drop(split_lease);
        p.commit_window_state(self.bytes(), base_state)?;
        if self.held < self.need() && self.total > self.held {
            // the reserve ran out: the input's own operators give its rows again
            self.refills += 1;
            let rmark = p.output_lease_mark();
            let rows = self.inner_init(p)?;
            let r_leases = p.take_output_leases_since(rmark);
            self.fill(p, rows, true)?;
            drop(r_leases);
        }
        let diff = self.diff(old);
        drop(old_lease);
        Ok(diff)
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

    /// Whether the keys are not computed.
    #[cfg(test)]
    pub(crate) fn stale(&self) -> bool {
        self.stale
    }

    /// Mark the keys as not computed, as a load does.
    #[cfg(test)]
    pub(crate) fn mark_stale(&mut self) {
        self.stale = true;
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

    pub fn clear(&mut self) {
        self.kept = Vec::new();
        self.held = 0;
        self.total = 0;
        self.stale = false;
        self.partial = false;
        self.moves = None;
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
        let per = self.per_row();
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
            r.charge_state(per)?;
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
        let mut seen = rustc_hash::FxHashSet::default();
        if !kept.iter().all(|e| seen.insert(&*e.row)) {
            return Err(crate::error::Error::Corrupt(
                "a view's window holds a row twice".into(),
            ));
        }
        drop(seen);
        self.kept = kept;
        self.held = held;
        self.total = total;
        self.stale = true;
        self.moves = None;
        Ok(())
    }
}

/// A row in the heap of a fill: the row, its copies in the input and its index among
/// the kept rows.
type HeapEntry = crate::sparql::sortkey::Entry<(Row, i64, Option<usize>)>;

/// The memory a heap entry holds besides its slot: its keys and their strings and its
/// row's ids.
fn heap_bytes(keys: &[SortKey], width: usize) -> u64 {
    key_bytes(keys).saturating_add(row_bytes(width))
}

/// The memory of a row's keys: their vector and their strings.
fn key_bytes(keys: &[SortKey]) -> u64 {
    let strings: u64 = keys.iter().map(SortKey::payload_bytes).sum();
    (std::mem::size_of_val(keys) as u64).saturating_add(strings)
}

/// Fail with a memory budget error when `used` passes the pass's budget or parent pool.
fn charge(p: &mut Pass, used: u64, base_state: u64) -> Result<()> {
    p.charge_window(used, base_state)
}

fn strip(c: AChanges) -> Changes {
    consolidate(c.into_iter().map(|(r, w, _)| (r, w)).collect())
}

#[cfg(test)]
#[path = "window_unit_tests.rs"]
mod tests;
