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
        let pin = self.pin(from);
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
