//! A history query read a page of whole commits at a time (item 829), for the cursor's
//! history reader. Each page scans what is left of the query's range of commits and
//! stops at the first commit boundary after a budget of changes, so a commit is never
//! split across pages and a commit larger than the budget is held whole, as an eager
//! read holds it. Every allocation a page makes is admitted before it is made: the
//! frame and the decoded form of each record read, the vector of the page's changes and
//! each change's quad, and for a read-only log read newest first the offsets of one
//! segment's records.
//!
//! The reader pins the log at the first commit of its range ([`LogPin`]), so retention
//! keeps the commits it has still to read and the log cannot be turned off under it. A
//! read-only log has neither retention nor a switch and is not pinned; its pages resume
//! from the position where the previous page stopped.
use super::readonly::RoCursor;
use super::*;
use crate::store::diff::{key_quad, visible_key};
use crate::store::history_query::{HistoryChange, HistoryQuery};
use std::cell::Cell;

/// A history reader's hold on the commits from `at` on (see [`ChangeLog`]'s `pins`).
pub(crate) struct LogPin {
    log: Arc<ChangeLog>,
    at: u64,
}

impl Drop for LogPin {
    fn drop(&mut self) {
        let mut pins = self.log.pins.lock();
        if let Some(n) = pins.get_mut(&self.at) {
            *n -= 1;
            if *n == 0 {
                pins.remove(&self.at);
            }
        }
    }
}

impl ChangeLog {
    /// Pin the commits from `at` on. `None` for a read-only log, which needs no pin.
    /// The caller checks that the log is on once the pin is held.
    fn pin(self: &Arc<Self>, at: u64) -> Option<LogPin> {
        if self.readonly.is_some() {
            return None;
        }
        *self.pins.lock().entry(at).or_insert(0) += 1;
        Some(LogPin {
            log: self.clone(),
            at,
        })
    }

    /// Start reading `q` in pages over the commits up to `upto`. The checks, the flush
    /// and the resolution of the range are those of [`ChangeLog::query`], made once.
    pub(crate) fn pages(self: &Arc<Self>, q: HistoryQuery, upto: u64) -> Result<HistoryPages> {
        self.pages_hooked(q, upto, &mut || Ok(()))
    }

    /// [`ChangeLog::pages`], calling `after_range` once the range is resolved and before
    /// it is pinned, which a test uses to run retention there.
    fn pages_hooked(
        self: &Arc<Self>,
        q: HistoryQuery,
        upto: u64,
        after_range: &mut dyn FnMut() -> Result<()>,
    ) -> Result<HistoryPages> {
        // Every commit is pinned while the range is resolved, so retention cannot drop
        // a commit of the range between its resolution and its pin. The pin is taken
        // under `inner` (before `pins`, the lock order), which retention holds while it
        // reads the pins and drops segments: a retention pass that read them before
        // has finished, and every later one sees the pin.
        let hold = {
            let _inner = self.inner.lock();
            self.pin(0)
        };
        let (from, to, _) = self.range(&q, upto)?;
        after_range()?;
        // an empty range reads nothing and holds no pin
        let pin = (from <= to).then(|| self.pin(from)).flatten();
        drop(hold);
        if !self.is_enabled() {
            return Err(Error::HistoryUnsupported(
                "the change log of this dataset is off; history queries need it".into(),
            ));
        }
        Ok(HistoryPages {
            log: self.clone(),
            filter: q.filter(),
            q,
            from,
            to,
            done: from > to,
            ro: RoCursor::default(),
            pin,
        })
    }
}

/// A history query read in pages (see the module documentation).
pub(crate) struct HistoryPages {
    log: Arc<ChangeLog>,
    q: HistoryQuery,
    filter: LogFilter,
    /// what is left of the range: the next page reads commits `from` through `to`
    from: u64,
    to: u64,
    done: bool,
    /// where a read-only log's next page starts
    ro: RoCursor,
    pin: Option<LogPin>,
}

/// One page: the matching changes of whole commits, in the order of the query.
pub(crate) struct Page {
    pub(crate) changes: Vec<HistoryChange>,
    /// the page reached its limit, which ends the read
    pub(crate) truncated: bool,
}

/// The bytes a page holds, as admitted: its changes, the record being read, the
/// access-control memo of the graphs it has read, and what a read-only cursor keeps
/// between pages.
pub(super) struct Held<'a, 'b> {
    page: Cell<u64>,
    record: Cell<u64>,
    memo: Cell<u64>,
    kept: Cell<u64>,
    /// the changes in the page
    pub(super) len: Cell<usize>,
    admit: std::cell::RefCell<&'a mut Admit<'b>>,
}

impl Held<'_, '_> {
    fn admit(&self) -> Result<()> {
        let total = self.page.get() + self.record.get() + self.memo.get() + self.kept.get();
        (self.admit.borrow_mut())(total)
    }
    pub(super) fn record(&self, bytes: u64) -> Result<()> {
        self.record.set(bytes);
        self.admit()
    }
    pub(super) fn kept(&self, bytes: u64) -> Result<()> {
        self.kept.set(bytes);
        self.admit()
    }
}

/// The bytes a quad decoded from `k` can allocate, at most: each term's text is no
/// longer than its key, a blank node's label is at most 64 bytes, and a triple term
/// boxes its parts at each level of nesting.
fn quad_bytes(k: &QuadKey) -> u64 {
    k.iter()
        .map(|t| {
            let nested = if t.first() == Some(&b'(') {
                t.len() as u64 * 64
            } else {
                0
            };
            t.len() as u64 + 64 + nested
        })
        .sum()
}

/// The bytes the table of a hash map from graph keys to a flag allocates for
/// `capacity` entries: its buckets of entries and their control bytes, as the standard
/// map sizes them.
fn memo_table_bytes(capacity: usize) -> u64 {
    if capacity == 0 {
        return 0;
    }
    let buckets = match capacity {
        0..4 => 4,
        4..8 => 8,
        n => (n.saturating_mul(8) / 7).next_power_of_two(),
    } as u64;
    buckets * (std::mem::size_of::<(Arc<[u8]>, bool)>() as u64 + 1) + 16
}

impl HistoryPages {
    /// Whether the range has been read to its end.
    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// What the reader keeps allocated between pages: for a read-only log read newest
    /// first, the offsets of a segment's records. The caller keeps it charged.
    pub(crate) fn kept_bytes(&self) -> u64 {
        self.ro.kept_bytes()
    }

    /// The next page, of at least `budget` changes unless the range ends, and at most
    /// `limit`: a page that reaches `limit` is `truncated`, and ends the read. `admit`
    /// is told the bytes the page holds before each allocation. The page stops before
    /// it reads a record of a commit after the budget is met. An empty page ends the
    /// read, and the pin goes with the read's end.
    pub(crate) fn next(
        &mut self,
        budget: usize,
        limit: usize,
        admit: &mut Admit<'_>,
    ) -> Result<Page> {
        if self.done {
            return Ok(Page {
                changes: Vec::new(),
                truncated: false,
            });
        }
        let held = Held {
            page: Cell::new(0),
            record: Cell::new(0),
            memo: Cell::new(0),
            kept: Cell::new(self.ro.kept_bytes()),
            len: Cell::new(0),
            admit: std::cell::RefCell::new(admit),
        };
        let q = &self.q;
        let access = q.access.as_ref().filter(|a| !a.reads_everything());
        let mut readable: FxHashMap<Arc<[u8]>, bool> = FxHashMap::default();
        let mut changes: Vec<HistoryChange> = Vec::new();
        let mut truncated = false;
        // the commit whose changes were visited last, and the commit the next page
        // starts at
        let current: Cell<Option<u64>> = Cell::new(None);
        let resume: Cell<Option<u64>> = Cell::new(None);
        let mut full = |seq: u64| {
            let stop = held.len.get() >= budget && current.get().is_some_and(|c| c != seq);
            if stop {
                resume.set(Some(seq));
            }
            stop
        };
        let mut visit = |c: &Arc<ChangeCommit>, add: bool, k: &QuadKey| -> Result<bool> {
            if current.get() != Some(c.seq) {
                if current.get().is_some() && changes.len() >= budget {
                    resume.set(Some(c.seq));
                    return Ok(false);
                }
                current.set(Some(c.seq));
            }
            let op = if add { DiffOp::Add } else { DiffOp::Remove };
            if q.op.is_some_and(|o| o != op) {
                return Ok(true);
            }
            if let Some(a) = access {
                // The memo keeps each graph key it has seen, which holds the key's
                // text after its record is released, and grows its table: both are
                // admitted before they are made.
                if !a.read.is_all() && !readable.contains_key(&k[0]) {
                    let mut memo = held.memo.get() + arc_slice_bytes(k[0].len());
                    if readable.len() == readable.capacity() {
                        let old = memo_table_bytes(readable.capacity());
                        let new = memo_table_bytes(readable.capacity() + 1);
                        held.memo.set(memo + new);
                        held.admit()?;
                        readable.try_reserve(1).map_err(|_| {
                            Error::HistoryUnsupported(
                                "history access memo allocation was refused".into(),
                            )
                        })?;
                        memo = memo - old + memo_table_bytes(readable.capacity());
                    }
                    held.memo.set(memo);
                    held.admit()?;
                }
                if !visible_key(a, &mut readable, k) {
                    return Ok(true);
                }
            }
            let mut add_bytes = quad_bytes(k);
            if changes.last().is_none_or(|l| !Arc::ptr_eq(&l.commit, c)) {
                // the commit's metadata stays with the page once its record is gone
                add_bytes += arc_slice_bytes(std::mem::size_of::<ChangeCommit>())
                    + [&c.author, &c.message]
                        .into_iter()
                        .flatten()
                        .map(|s| arc_slice_bytes(s.len()))
                        .sum::<u64>();
            }
            let page = held.page.get() + add_bytes;
            if changes.len() == changes.capacity() {
                // a reallocation holds the old vector beside the new one
                let size = std::mem::size_of::<HistoryChange>() as u64;
                let old = changes.capacity() as u64 * size;
                let more = changes.capacity().max(64);
                let page = page + more as u64 * size;
                held.page.set(page + old);
                held.admit()?;
                changes.try_reserve_exact(more).map_err(|_| {
                    Error::HistoryUnsupported("history page allocation was refused".into())
                })?;
                held.page.set(page);
            } else {
                held.page.set(page);
            }
            held.admit()?;
            if let Some(quad) = key_quad(k) {
                changes.push(HistoryChange {
                    commit: c.clone(),
                    op,
                    quad,
                });
                held.len.set(changes.len());
            }
            // a page that reaches its limit stops at once: the caller refuses it
            if changes.len() >= limit {
                truncated = true;
                return Ok(false);
            }
            Ok(true)
        };
        let mut check = || q.check();
        let ro_ended = if let Some(reader) = &self.log.readonly {
            reader.page(
                &mut self.ro,
                self.from,
                self.to,
                &self.filter,
                q.descending,
                budget,
                &mut check,
                &held,
                &mut visit,
            )?
        } else {
            let mut admit_record = |b: u64| held.record(b);
            self.log.scan(
                self.from,
                self.to,
                &self.filter,
                q.descending,
                false,
                &mut check,
                Some(&mut PageHooks {
                    admit: &mut admit_record,
                    full: &mut full,
                }),
                &mut visit,
            )?;
            false
        };
        drop(visit);
        // the memo goes with the page's read
        drop(readable);
        held.memo.set(0);
        let ended = if self.log.readonly.is_some() {
            ro_ended
        } else {
            match resume.get() {
                Some(seq) if q.descending => {
                    self.to = seq;
                    false
                }
                Some(seq) => {
                    self.from = seq;
                    false
                }
                None => true,
            }
        };
        if ended || truncated {
            self.done = true;
            self.pin = None;
        }
        let _ = held.record(0);
        Ok(Page { changes, truncated })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{HistoryOptions, ReadLimit};
    use crate::store::cold::ColdBudget;
    use crate::store::history_query::HistoryBound;
    use crate::store::{Store, StoreOptions};

    fn upd(s: &Store, u: &str) {
        crate::sparql::update::update(s, u, &Default::default()).unwrap();
    }

    fn options() -> StoreOptions {
        StoreOptions {
            change_log_segment_bytes: 4096,
            ..Default::default()
        }
    }

    /// 36 commits over several segments: the `i`th insertion of 30 adds `i % 7 + 1`
    /// quads and the tenth 40 (commit 11), and every fifth is followed by a removal.
    fn fill(s: &Store) {
        if let Some(log) = s.change_log() {
            log.set_background(false);
        }
        for i in 1..=30u32 {
            let n = if i == 10 { 40 } else { i % 7 + 1 };
            let body: String = (0..n)
                .map(|j| {
                    let v = "v".repeat(((i * 31 + j * 17) % 300) as usize);
                    format!("<urn:s{i}> <urn:p{}> \"{v}{j}\" . ", j % 3)
                })
                .collect();
            upd(s, &format!("INSERT DATA {{ {body} }}"));
            if i % 5 == 0 {
                let v = "v".repeat(((i * 31) % 300) as usize);
                upd(
                    s,
                    &format!("DELETE DATA {{ <urn:s{i}> <urn:p0> \"{v}0\" }}"),
                );
            }
        }
        assert_eq!(s.head_commit().seq, 36);
        s.flush_change_log().unwrap();
    }

    type Key = (u64, DiffOp, String);

    fn key(c: &HistoryChange) -> Key {
        (c.commit.seq, c.op, c.quad.to_string())
    }

    /// Read `q` to its end in pages of `budget`, checking that each page holds whole
    /// commits and stops at the first commit boundary after the budget.
    fn paged(log: &Arc<ChangeLog>, q: &HistoryQuery, upto: u64, budget: usize) -> Vec<Vec<Key>> {
        let mut pages = log.pages(q.clone(), upto).unwrap();
        let mut out: Vec<Vec<Key>> = Vec::new();
        loop {
            let page = pages.next(budget, usize::MAX, &mut |_| Ok(())).unwrap();
            assert!(!page.truncated);
            if page.changes.is_empty() {
                assert!(pages.done(), "an empty page ends the read");
                break;
            }
            let keys: Vec<Key> = page.changes.iter().map(key).collect();
            let last = keys.last().unwrap().0;
            let before_last = keys.iter().take_while(|k| k.0 != last).count();
            assert!(
                before_last < budget,
                "a page stops after its budget: {keys:?}"
            );
            if let Some(prev) = out.last() {
                assert_ne!(
                    prev.last().unwrap().0,
                    keys[0].0,
                    "a commit split across pages"
                );
            }
            out.push(keys);
        }
        out
    }

    fn queries() -> Vec<HistoryQuery> {
        let p0 = oxrdf::NamedNode::new("urn:p0").unwrap();
        vec![
            HistoryQuery::default(),
            HistoryQuery {
                predicates: vec![p0],
                ..Default::default()
            },
            HistoryQuery {
                op: Some(DiffOp::Remove),
                ..Default::default()
            },
            HistoryQuery {
                from: Some(HistoryBound::Commit(9)),
                to: Some(HistoryBound::Commit(20)),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn pages_put_together_are_the_query_in_both_orders() {
        for persistent in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = match persistent {
                true => Store::open(dir.path(), options()).unwrap(),
                false => Store::in_memory(options()),
            };
            fill(&s);
            let log = s.change_log().unwrap().clone();
            let head = s.head_commit().seq;
            for q in queries() {
                for descending in [false, true] {
                    let q = HistoryQuery {
                        descending,
                        ..q.clone()
                    };
                    let want: Vec<Key> = log
                        .query(&q, head)
                        .unwrap()
                        .changes
                        .iter()
                        .map(key)
                        .collect();
                    assert!(!want.is_empty());
                    for budget in [1, 3, 16, 10_000] {
                        let got = paged(&log, &q, head, budget);
                        assert_eq!(
                            got.concat(),
                            want,
                            "persistent={persistent} descending={descending} budget={budget}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_commit_larger_than_the_budget_comes_whole() {
        let s = Store::in_memory(options());
        fill(&s);
        let log = s.change_log().unwrap().clone();
        for descending in [false, true] {
            let q = HistoryQuery {
                descending,
                ..Default::default()
            };
            let pages = paged(&log, &q, 36, 3);
            let big: Vec<&Vec<Key>> = pages
                .iter()
                .filter(|p| p.iter().any(|k| k.0 == 11))
                .collect();
            assert_eq!(big.len(), 1);
            assert_eq!(big[0].iter().filter(|k| k.0 == 11).count(), 40);
        }
    }

    /// The read-only log of the commits through `through` of the persistent store at
    /// `dir`, whose head is 36 unless `head` says otherwise.
    fn read_only(
        dir: &std::path::Path,
        normal: &ChangeLog,
        through: u64,
        head: u64,
    ) -> Arc<ChangeLog> {
        let history = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &history,
            memory: Some(16 << 20),
            held: 0,
        })
        .unwrap();
        ChangeLog::capture_read_only(
            dir,
            normal.dataset_id,
            through,
            head,
            normal.limits,
            normal.settings(),
            &mut budget,
        )
        .unwrap()
        .value
    }

    /// Read `q` to its end in pages of `budget`, returning the largest admission each
    /// page made and the commits it holds.
    fn peaks(log: &Arc<ChangeLog>, q: &HistoryQuery, budget: usize) -> Vec<(u64, Vec<u64>)> {
        let mut pages = log.pages(q.clone(), u64::MAX).unwrap();
        let mut out = Vec::new();
        loop {
            let mut peak = 0;
            let page = pages
                .next(budget, usize::MAX, &mut |b| {
                    peak = peak.max(b);
                    Ok(())
                })
                .unwrap();
            if page.changes.is_empty() {
                break;
            }
            let mut commits: Vec<u64> = page.changes.iter().map(|c| c.commit.seq).collect();
            commits.dedup();
            out.push((peak, commits));
        }
        out
    }

    /// A full page stops before it reads the record of the next commit, or the index or
    /// records of the next segment: the page before a large commit never admits it.
    #[test]
    fn a_full_page_reads_nothing_of_the_next_commit() {
        let dir = tempfile::tempdir().unwrap();
        for persistent in [false, true] {
            let root = dir.path().join(format!("db{persistent}"));
            let s = match persistent {
                true => Store::open(&root, options()).unwrap(),
                false => Store::in_memory(options()),
            };
            s.change_log().unwrap().set_background(false);
            // three small commits, one of 40 quads of 3,000 bytes each, three small
            for i in 1..=7u32 {
                let body: String = if i == 4 {
                    (0..40)
                        .map(|j| format!("<urn:big> <urn:p> \"{}{j}\" . ", "v".repeat(3000)))
                        .collect()
                } else {
                    format!("<urn:s{i}> <urn:p> \"{i}\" . ")
                };
                upd(&s, &format!("INSERT DATA {{ {body} }}"));
            }
            s.flush_change_log().unwrap();
            let normal = s.change_log().unwrap().clone();
            let mut logs = vec![("normal", normal.clone())];
            if persistent {
                logs.push(("read-only", read_only(&root, &normal, 7, 7)));
            }
            for (name, log) in logs {
                for descending in [false, true] {
                    let q = HistoryQuery {
                        descending,
                        ..Default::default()
                    };
                    let pages = peaks(&log, &q, 1);
                    assert_eq!(pages.len(), 7, "{name}: {pages:?}");
                    let before = if descending { 5 } else { 3 };
                    let (peak, commits) = pages.iter().find(|p| p.1 == [before]).unwrap();
                    assert!(
                        *peak < 100_000,
                        "{name} persistent={persistent} descending={descending}: the page of \
                         commit {commits:?} admitted {peak} bytes"
                    );
                    let big = pages.iter().find(|p| p.1 == [4]).unwrap();
                    assert!(big.0 > 120_000, "{name}: {big:?}");
                }
            }
        }
    }

    /// Retention that runs between the resolution of the range and the reader's pin
    /// keeps the range: every commit is pinned while the range is resolved.
    #[test]
    fn retention_while_the_range_is_resolved_keeps_the_range() {
        for persistent in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = match persistent {
                true => Store::open(dir.path(), options()).unwrap(),
                false => Store::in_memory(options()),
            };
            fill(&s);
            let log = s.change_log().unwrap().clone();
            let all = HistoryQuery::default();
            let want: Vec<Key> = log
                .query(&all, 36)
                .unwrap()
                .changes
                .iter()
                .map(key)
                .collect();
            let oldest = || log.inner.lock().segs[0].first;
            let first = oldest();
            let mut pages = log
                .pages_hooked(all.clone(), 36, &mut || {
                    s.set_change_log_settings(ChangeLogSettings {
                        keep_commits: Some(2),
                        ..Default::default()
                    })
                    .map(|_| ())
                })
                .unwrap();
            assert_eq!(oldest(), first, "persistent={persistent}");
            let mut got: Vec<Key> = Vec::new();
            loop {
                let page = pages.next(3, usize::MAX, &mut |_| Ok(())).unwrap();
                if page.changes.is_empty() {
                    break;
                }
                got.extend(page.changes.iter().map(key));
            }
            assert_eq!(got, want, "persistent={persistent}");
            drop(pages);
            log.enforce(None).unwrap();
            assert!(oldest() > first, "persistent={persistent}");
        }
    }

    #[test]
    fn a_read_only_log_pages_as_its_scan_in_both_orders() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), options()).unwrap();
        fill(&s);
        s.flush_change_log().unwrap();
        let normal = s.change_log().unwrap().clone();
        assert!(normal.status().segments > 3, "{:?}", normal.status());
        for through in [7, 20, 36] {
            let ro = read_only(dir.path(), &normal, through, 36);
            for q in queries() {
                for descending in [false, true] {
                    let q = HistoryQuery {
                        descending,
                        ..q.clone()
                    };
                    let want: Vec<Key> = normal
                        .query(&q, through)
                        .unwrap()
                        .changes
                        .iter()
                        .map(key)
                        .collect();
                    for budget in [1, 3, 16, 10_000] {
                        let got = paged(&ro, &q, 36, budget);
                        assert_eq!(
                            got.concat(),
                            want,
                            "through={through} descending={descending} budget={budget}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_page_admits_what_it_holds_and_a_refusal_fails_the_page() {
        let size = std::mem::size_of::<HistoryChange>() as u64;
        for persistent in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = match persistent {
                true => Store::open(dir.path(), options()).unwrap(),
                false => Store::in_memory(options()),
            };
            fill(&s);
            let normal = s.change_log().unwrap().clone();
            let mut logs = vec![normal.clone()];
            if persistent {
                logs.push(read_only(dir.path(), &normal, 36, 36));
            }
            for log in logs {
                let ro = log.readonly.is_some();
                for descending in [false, true] {
                    let q = HistoryQuery {
                        descending,
                        ..Default::default()
                    };
                    let mut pages = log.pages(q.clone(), 36).unwrap();
                    let mut seen: Vec<u64> = Vec::new();
                    let page = pages
                        .next(100, usize::MAX, &mut |b| {
                            seen.push(b);
                            Ok(())
                        })
                        .unwrap();
                    assert!(page.changes.len() > 64, "{}", page.changes.len());
                    let last = *seen.last().unwrap();
                    let floor = page.changes.capacity() as u64 * size;
                    assert!(last >= floor, "{last} < {floor}");
                    // the vector grew from 64 changes to 128 with the old one admitted
                    // beside the new one, and released after
                    assert!(
                        seen.windows(2)
                            .any(|w| w[0].checked_sub(w[1]) == Some(64 * size)),
                        "ro={ro} descending={descending}"
                    );
                    if ro && descending {
                        // the offsets of a segment's records stay with the reader
                        let kept = pages.kept_bytes();
                        assert!(kept > 0 && kept <= last, "{kept} {last}");
                    }
                    let mut pages = log.pages(q, 36).unwrap();
                    let refused = pages.next(64, usize::MAX, &mut |b| {
                        if b > 2048 {
                            Err(Error::Invalid("refused".into()))
                        } else {
                            Ok(())
                        }
                    });
                    assert!(
                        matches!(refused, Err(Error::Invalid(_))),
                        "ro={ro} descending={descending}"
                    );
                }
            }
        }
    }

    /// The access-control memo of the graphs a page reads is admitted with the page and
    /// released after it.
    #[test]
    fn the_access_memo_is_admitted() {
        use crate::access::{GraphAccess, GraphRule, Graphs};
        let s = Store::in_memory(options());
        s.change_log().unwrap().set_background(false);
        for i in 0..40u32 {
            upd(
                &s,
                &format!(
                    "INSERT DATA {{ GRAPH <urn:g{}> {{ <urn:s{i}> <urn:p> {i} }} }}",
                    i % 20
                ),
            );
        }
        s.flush_change_log().unwrap();
        let log = s.change_log().unwrap().clone();
        let names: Vec<String> = (0..20).map(|i| format!("urn:g{i}")).collect();
        let access = Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new(&names, &[] as &[&str])),
            Graphs::none(),
        ));
        let read = |access: Option<Arc<GraphAccess>>| {
            let mut pages = log
                .pages(
                    HistoryQuery {
                        access,
                        ..Default::default()
                    },
                    40,
                )
                .unwrap();
            let (mut peak, mut last) = (0, 0);
            let page = pages
                .next(10_000, usize::MAX, &mut |b| {
                    peak = peak.max(b);
                    last = b;
                    Ok(())
                })
                .unwrap();
            (page.changes.len(), peak, last)
        };
        let (n, peak, last) = read(None);
        let (n_memo, peak_memo, last_memo) = read(Some(access));
        assert_eq!((n, n_memo), (40, 40));
        // twenty graph keys and a table of at least twenty entries
        let memo = 20 * 16 + memo_table_bytes(20);
        assert!(peak_memo >= peak + memo, "{peak_memo} < {peak} + {memo}");
        assert_eq!(last_memo, last, "the memo goes with the page");
    }

    #[test]
    fn a_page_stops_at_its_limit_with_changes_left() {
        let s = Store::in_memory(options());
        fill(&s);
        let log = s.change_log().unwrap().clone();
        let mut pages = log.pages(HistoryQuery::default(), 36).unwrap();
        let page = pages.next(10_000, 5, &mut |_| Ok(())).unwrap();
        assert!(page.truncated);
        assert_eq!(page.changes.len(), 5);
        // a page that reaches its limit ends the read and its pin
        assert!(pages.done());
        assert!(
            pages
                .next(10_000, 5, &mut |_| Ok(()))
                .unwrap()
                .changes
                .is_empty()
        );
        turn_off(&s).unwrap();
        drop(pages);
    }

    fn turn_off(s: &Store) -> Result<()> {
        s.set_change_log_settings(ChangeLogSettings {
            enabled: Some(false),
            ..Default::default()
        })
        .map(|_| ())
    }

    #[test]
    fn a_pin_refuses_turning_the_log_off_and_writes_no_settings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, options()).unwrap();
        fill(&s);
        let log = s.change_log().unwrap().clone();
        let settings = || std::fs::read(root.join(CHANGE_LOG_FILE)).ok();
        let before = settings();
        let first = log.pages(HistoryQuery::default(), 36).unwrap();
        let mut second = log
            .pages(
                HistoryQuery {
                    from: Some(HistoryBound::Commit(30)),
                    ..Default::default()
                },
                36,
            )
            .unwrap();
        assert!(matches!(turn_off(&s), Err(Error::Conflict(_))));
        assert_eq!(settings(), before, "a refused change writes no settings");
        assert!(log.is_enabled());
        // the pins are independent: reading one to its end releases only its own
        while !second
            .next(4, usize::MAX, &mut |_| Ok(()))
            .unwrap()
            .changes
            .is_empty()
        {}
        assert!(second.done());
        assert!(matches!(turn_off(&s), Err(Error::Conflict(_))));
        drop(first);
        drop(s);
        // the log is on after a reopen, with every commit
        let s = Store::open(&root, options()).unwrap();
        assert_eq!(
            s.history_changes(&HistoryQuery::default())
                .unwrap()
                .changes
                .len(),
            log.query(&HistoryQuery::default(), 36)
                .unwrap()
                .changes
                .len()
        );
        drop(log);
        drop(second);
        turn_off(&s).unwrap();
        assert!(!s.change_log().unwrap().is_enabled());
    }

    #[test]
    fn a_pin_goes_when_the_range_ends_or_is_empty() {
        let s = Store::in_memory(options());
        fill(&s);
        let log = s.change_log().unwrap().clone();
        let empty = log
            .pages(
                HistoryQuery {
                    from: Some(HistoryBound::Commit(100)),
                    ..Default::default()
                },
                36,
            )
            .unwrap();
        assert!(empty.done());
        let mut read = log.pages(HistoryQuery::default(), 36).unwrap();
        while !read
            .next(1000, usize::MAX, &mut |_| Ok(()))
            .unwrap()
            .changes
            .is_empty()
        {}
        // both readers are alive, and neither pins the log
        turn_off(&s).unwrap();
        drop((empty, read));
    }

    #[test]
    fn retention_keeps_what_a_pin_holds_and_resumes_after_it_goes() {
        for persistent in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = match persistent {
                true => Store::open(dir.path(), options()).unwrap(),
                false => Store::in_memory(options()),
            };
            fill(&s);
            let log = s.change_log().unwrap().clone();
            assert!(log.status().segments > 3, "{:?}", log.status());
            let all = HistoryQuery::default();
            let want: Vec<Key> = log
                .query(&all, 36)
                .unwrap()
                .changes
                .iter()
                .map(key)
                .collect();
            // the segments, oldest first, and the first commit retention keeps when the
            // oldest pin is `pin`: two commits are kept, so a segment ending before 35 goes
            let segs: Vec<(u64, u64)> = log
                .inner
                .lock()
                .segs
                .iter()
                .map(|g| (g.first, g.last))
                .collect();
            let kept = |pin: u64| segs.iter().find(|g| g.1 >= pin.min(35)).unwrap().0;
            let oldest = || log.inner.lock().segs[0].first;
            assert!(kept(25) > segs[0].0 && kept(35) > kept(25), "{segs:?}");
            let mut pages = log.pages(all.clone(), 36).unwrap();
            let mut got: Vec<Key> = pages
                .next(3, usize::MAX, &mut |_| Ok(()))
                .unwrap()
                .changes
                .iter()
                .map(key)
                .collect();
            // a later reader's pin keeps less than the first's
            let later = log
                .pages(
                    HistoryQuery {
                        from: Some(HistoryBound::Commit(25)),
                        ..Default::default()
                    },
                    36,
                )
                .unwrap();
            s.set_change_log_settings(ChangeLogSettings {
                keep_commits: Some(2),
                ..Default::default()
            })
            .unwrap();
            assert_eq!(
                oldest(),
                segs[0].0,
                "the first reader's pin keeps every segment"
            );
            loop {
                let page = pages.next(3, usize::MAX, &mut |_| Ok(())).unwrap();
                if page.changes.is_empty() {
                    break;
                }
                got.extend(page.changes.iter().map(key));
            }
            assert_eq!(got, want, "persistent={persistent}");
            // the first pin went with the end of its range; the later one still holds
            log.enforce(None).unwrap();
            assert_eq!(oldest(), kept(25), "{segs:?}");
            drop(later);
            log.enforce(None).unwrap();
            assert_eq!(oldest(), kept(35), "{segs:?}");
        }
    }
}
