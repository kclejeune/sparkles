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
#![allow(dead_code)] // the cursor's history reader uses this module (item 829)
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
        let (from, to, _) = self.range(&q, upto)?;
        // an empty range reads nothing and holds no pin
        let pin = (from <= to).then(|| self.pin(from)).flatten();
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
    /// the page stopped at its limit with changes left
    pub(crate) truncated: bool,
}

/// The bytes a page holds, as admitted: its changes, the record being read, and what a
/// read-only cursor keeps between pages.
pub(super) struct Held<'a, 'b> {
    page: Cell<u64>,
    record: Cell<u64>,
    kept: Cell<u64>,
    /// the changes in the page
    pub(super) len: Cell<usize>,
    admit: std::cell::RefCell<&'a mut Admit<'b>>,
}

impl Held<'_, '_> {
    fn admit(&self) -> Result<()> {
        let total = self.page.get() + self.record.get() + self.kept.get();
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

impl HistoryPages {
    /// Whether the range has been read to its end.
    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// The next page, of at least `budget` changes unless the range ends, and at most
    /// `limit`: a page that reaches `limit` with changes left is `truncated`. `admit`
    /// is told the bytes the page holds before each allocation. An empty page that is
    /// not truncated ends the read, and the pin goes with it.
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
            kept: Cell::new(self.ro.kept_bytes()),
            len: Cell::new(0),
            admit: std::cell::RefCell::new(admit),
        };
        let q = &self.q;
        let access = q.access.as_ref().filter(|a| !a.reads_everything());
        let mut readable: FxHashMap<Arc<[u8]>, bool> = FxHashMap::default();
        let mut changes: Vec<HistoryChange> = Vec::new();
        let mut truncated = false;
        let mut current: Option<u64> = None;
        let mut boundary: Option<u64> = None;
        let mut visit = |c: &Arc<ChangeCommit>, add: bool, k: &QuadKey| -> Result<bool> {
            if current != Some(c.seq) {
                if current.is_some() && changes.len() >= budget {
                    boundary = current;
                    return Ok(false);
                }
                current = Some(c.seq);
            }
            let op = if add { DiffOp::Add } else { DiffOp::Remove };
            if q.op.is_some_and(|o| o != op) {
                return Ok(true);
            }
            if access.is_some_and(|a| !visible_key(a, &mut readable, k)) {
                return Ok(true);
            }
            if changes.len() >= limit {
                truncated = true;
                return Ok(false);
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
            if changes.len() == changes.capacity() {
                let more = changes.capacity().max(64);
                add_bytes += (more * std::mem::size_of::<HistoryChange>()) as u64;
                held.page.set(held.page.get() + add_bytes);
                held.admit()?;
                changes.reserve_exact(more);
            } else {
                held.page.set(held.page.get() + add_bytes);
                held.admit()?;
            }
            if let Some(quad) = key_quad(k) {
                changes.push(HistoryChange {
                    commit: c.clone(),
                    op,
                    quad,
                });
                held.len.set(changes.len());
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
                Some(&mut admit_record),
                &mut visit,
            )?;
            false
        };
        drop(visit);
        let ended = if self.log.readonly.is_some() {
            ro_ended
        } else {
            match boundary {
                Some(seq) if q.descending => {
                    self.to = seq - 1;
                    false
                }
                Some(seq) => {
                    self.from = seq + 1;
                    false
                }
                None => true,
            }
        };
        if ended && !truncated {
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

    #[test]
    fn a_read_only_log_pages_as_its_scan_in_both_orders() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), options()).unwrap();
        fill(&s);
        s.flush_change_log().unwrap();
        let normal = s.change_log().unwrap().clone();
        assert!(normal.status().segments > 3, "{:?}", normal.status());
        let history = HistoryOptions::default();
        for through in [7, 20, 36] {
            let mut budget = ColdBudget::new(ReadLimit {
                options: &history,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            let ro = ChangeLog::capture_read_only(
                dir.path(),
                normal.dataset_id,
                through,
                36,
                normal.limits,
                normal.settings(),
                &mut budget,
            )
            .unwrap()
            .value;
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
        for persistent in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = match persistent {
                true => Store::open(dir.path(), options()).unwrap(),
                false => Store::in_memory(options()),
            };
            fill(&s);
            let log = s.change_log().unwrap().clone();
            let mut pages = log.pages(HistoryQuery::default(), 36).unwrap();
            let mut last = 0;
            let page = pages
                .next(64, usize::MAX, &mut |b| {
                    last = b;
                    Ok(())
                })
                .unwrap();
            let floor = page.changes.capacity() * std::mem::size_of::<HistoryChange>();
            assert!(last >= floor as u64, "{last} < {floor}");
            let mut pages = log.pages(HistoryQuery::default(), 36).unwrap();
            let refused = pages.next(64, usize::MAX, &mut |b| {
                if b > 2048 {
                    Err(Error::Invalid("refused".into()))
                } else {
                    Ok(())
                }
            });
            assert!(matches!(refused, Err(Error::Invalid(_))));
        }
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
