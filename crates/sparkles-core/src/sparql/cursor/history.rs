//! A history query without a limit and without input, read a page of whole commits at a
//! time (item 829).
//!
//! The reader holds one page of the change log's matching changes, at least
//! [`PAGE`] of them unless the range ends, and turns it into rows a batch at a time, so
//! it never holds a table of every row. Each page, and each record read for it, is
//! charged before it is allocated (see [`crate::store::HistoryPages`]). The changes are
//! counted as they are read, and the query fails once they pass the row budget, as an
//! eager run fails once it has read one more change than the budget.
//!
//! The reader pins the change log at the first commit of its range until the range ends
//! or the reader is released, so retention keeps the commits it has still to read and the
//! log is not turned off under it. A slow reader therefore keeps segments that the size
//! limit would drop.
//!
//! Nothing happens before the first pull, because operators are also built only to see
//! whether an EXISTS pattern streams. The checks, the pin and their errors come at the
//! first pull, as they come when an eager run reaches the node.
use super::Buffer;
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::history_svc::{self, RowMaker};
use crate::sparql::plan::{Kind, Node};
use crate::store::{HistoryChange, HistoryPages};
use std::sync::Arc;

/// The changes a page holds at least, unless the range ends first. A commit is never
/// split, so a page holds more when its last commit does.
const PAGE: usize = 4096;

/// Whether a plan node runs as a history reader: a call without a limit that reads no
/// input. A call with a limit keeps its bounded leaf, and an attached call expands its
/// input's rows.
pub(super) fn eligible(node: &Node) -> bool {
    matches!(&node.kind, Kind::HistoryChanges(spec) if spec.limit.is_none())
        && node.children.is_empty()
}

pub(super) struct History {
    node: Node,
    opened: bool,
    rows: Option<RowMaker>,
    /// the pages left of the range and the charge of the page held; `None` once the range
    /// has ended or the reader is released
    pages: Option<(HistoryPages, OwnedCharge)>,
    page: Vec<HistoryChange>,
    /// the next change of `page` to turn into a row
    at: usize,
    /// the changes read so far, which the row budget bounds
    total: usize,
}

impl History {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            opened: false,
            rows: None,
            pages: None,
            page: Vec::new(),
            at: 0,
            total: 0,
        }
    }

    /// Drop the page, its charge and the pin, once nothing reads them any more.
    pub(super) fn release(&mut self) {
        self.opened = true;
        self.page = Vec::new();
        self.at = 0;
        self.pages = None;
    }

    /// Whether the range has been read to its end.
    pub(super) fn done(&self) -> bool {
        self.opened && self.pages.is_none()
    }

    /// The next batch of at most `cap` rows, or `None` once the range has ended.
    pub(super) fn next(&mut self, ctx: &Arc<Ctx>, cap: usize) -> Result<Option<Buffer>> {
        let r = self.pull(ctx, cap);
        if r.is_err() {
            self.release();
        }
        r
    }

    fn pull(&mut self, ctx: &Arc<Ctx>, cap: usize) -> Result<Option<Buffer>> {
        let Kind::HistoryChanges(spec) = &self.node.kind else {
            unreachable!("a history node")
        };
        if !self.opened {
            self.opened = true;
            history_svc::range_vars_bound(ctx, spec)?;
            let log = history_svc::change_log(ctx)?;
            let q = history_svc::history_query(ctx, spec);
            if q.access.as_ref().is_some_and(|a| !a.reads_everything())
                && ctx.snap.has_evidence_projection_or_unavailable()
            {
                return Err(crate::Error::NotPermitted(
                    "history queries are not available to a caller whose view is subject to \
                     support-aware projection visibility"
                        .into(),
                ));
            }
            let charge = OwnedCharge::new(ctx, 0)?;
            let pages = log.pages_checked(q, ctx.snap.commit, &|| ctx.check())?;
            self.rows = Some(RowMaker::new(spec, &self.node.vars));
            self.pages = Some((pages, charge));
        }
        loop {
            if self.at < self.page.len() {
                let n = cap.max(1).min(self.page.len() - self.at);
                let rows = self.rows.as_ref().expect("opened");
                let mut b = Buffer::new(ctx, &self.node.vars, n)?;
                let mut row = vec![Id::UNDEF; self.node.vars.len()];
                for c in &self.page[self.at..self.at + n] {
                    ctx.check()?;
                    if rows.fill(ctx, c, &mut row) {
                        b.table.push_row(&row);
                    }
                    // a term that could not be interned within the budget fails here
                    ctx.check()?;
                }
                self.at += n;
                if self.at == self.page.len() {
                    self.drop_page()?;
                }
                b.reconcile()?;
                if b.table.len > 0 {
                    // An eager call counts its rows once itself and once more as the
                    // executor's node, which the cursor counts above; the reader counts
                    // them as eager does.
                    ctx.produced(b.table.len)?;
                    return Ok(Some(b));
                }
                // every change of the batch was refused by a variable used twice
                continue;
            }
            let Some((pages, charge)) = &mut self.pages else {
                return Ok(None);
            };
            if pages.done() {
                self.release();
                return Ok(None);
            }
            // one more change than the row budget fails, as it fails an eager run
            let limit = ctx
                .max_rows
                .saturating_add(1)
                .saturating_sub(self.total)
                .max(1);
            let page = pages.next_checked(PAGE, limit, Some(&|| ctx.check()), &mut |bytes| {
                charge.resize(bytes)?;
                #[cfg(test)]
                tests::observe_admission(bytes);
                Ok(())
            })?;
            self.total = self.total.saturating_add(page.changes.len());
            ctx.check_rows(self.total)?;
            // a page reaches its limit only with one change more than the budget, and
            // stops there, so the refusal comes before any later record is read
            debug_assert!(!page.truncated, "a truncated page passes the row budget");
            if page.changes.is_empty() {
                self.release();
                return Ok(None);
            }
            self.page = page.changes;
            self.at = 0;
        }
    }

    /// Drop a page whose rows are all out, and its charge. What the pages keep between
    /// pages stays charged.
    fn drop_page(&mut self) -> Result<()> {
        self.page = Vec::new();
        self.at = 0;
        match &mut self.pages {
            Some((pages, charge)) => pages.page_dropped(&mut |b| charge.resize(b)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Error;
    use crate::access::{GraphAccess, Preparation};
    use crate::error::Result;
    use crate::sparql::update::update;
    use crate::sparql::{
        CursorOptions, FallbackPolicy, QueryCursor, QueryOptions, select_source_cursor,
    };
    use crate::store::{ChangeLogSettings, Store};
    use crate::task::Control;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    const QUERY: &str = "PREFIX hist: <urn:x-sparkles:history#>
        SELECT ?s ?p ?o ?op ?c { SERVICE hist:changes {
            << ?s ?p ?o >> hist:op ?op ; hist:commit ?c
        } }";

    type AdmissionObserver = Box<dyn FnMut(u64)>;

    thread_local! {
        static ADMISSION_OBSERVER: RefCell<Option<AdmissionObserver>> = const { RefCell::new(None) };
    }

    pub(super) fn observe_admission(bytes: u64) {
        ADMISSION_OBSERVER.with_borrow_mut(|observer| {
            if let Some(observer) = observer {
                observer(bytes);
            }
        });
    }

    struct ObserveAdmission;

    impl ObserveAdmission {
        fn new(observer: impl FnMut(u64) + 'static) -> Self {
            ADMISSION_OBSERVER.with_borrow_mut(|slot| {
                assert!(slot.is_none());
                *slot = Some(Box::new(observer));
            });
            Self
        }
    }

    impl Drop for ObserveAdmission {
        fn drop(&mut self) {
            ADMISSION_OBSERVER.with_borrow_mut(|slot| *slot = None);
        }
    }

    fn fill_log(store: &Store) {
        store.change_log().unwrap().set_background(false);
        let body: String = (0..4_200)
            .map(|j| format!("<urn:s> <urn:p> {j} . "))
            .collect();
        update(
            store,
            &format!("INSERT DATA {{ {body} }}"),
            &Default::default(),
        )
        .unwrap();
        store.flush_change_log().unwrap();
    }

    fn strict() -> CursorOptions {
        CursorOptions {
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        }
    }

    fn pull_with_stop(
        cursor: &mut QueryCursor,
        stop: impl FnOnce() + 'static,
    ) -> (Result<Option<usize>>, usize) {
        let admissions_after_stop = Rc::new(Cell::new(None));
        let count = admissions_after_stop.clone();
        let mut stop = Some(stop);
        let observer = ObserveAdmission::new(move |bytes| {
            if let Some(n) = count.get() {
                count.set(Some(n + 1));
            } else if bytes != 0 {
                count.set(Some(0));
                stop.take().unwrap()();
            }
        });
        let result = cursor
            .next_batch()
            .map(|batch| batch.map(|batch| batch.len()));
        drop(observer);
        let admissions_after_stop = admissions_after_stop
            .get()
            .expect("the first nonzero admission must occur during the pull");
        assert_eq!(cursor.stats().emitted_rows, 0);
        assert!(cursor.next_batch().unwrap().is_none());
        (result, admissions_after_stop)
    }

    fn disable_log(store: &Store) {
        store
            .set_change_log_settings(ChangeLogSettings {
                enabled: Some(false),
                ..Default::default()
            })
            .expect("the failed pull must have released the writable pin");
    }

    #[test]
    fn checked_history_cursor_stops_during_record_admission() {
        let mut results = Vec::new();
        for persistent in [false, true] {
            for max_rows in [None, Some(1)] {
                let dir = tempfile::tempdir().unwrap();
                let store = if persistent {
                    Store::open(dir.path(), Default::default()).unwrap()
                } else {
                    Store::in_memory(Default::default())
                };
                fill_log(&store);
                let flag = Arc::new(AtomicBool::new(false));
                let preparation =
                    Preparation::new(Control::with_cancel(flag.clone()), 64 << 20).unwrap();
                let policy = Arc::new(GraphAccess::all());
                let view = store.prepare_read(policy.clone(), &preparation).unwrap();
                let baseline = preparation.inner_used();
                let mut cursor = select_source_cursor(
                    view.read_source(),
                    &store,
                    QUERY,
                    &QueryOptions {
                        graphs: Some(policy),
                        max_rows,
                        ..Default::default()
                    },
                    &strict(),
                )
                .unwrap();
                let (result, later_admissions) =
                    pull_with_stop(&mut cursor, move || flag.store(true, Ordering::Relaxed));
                disable_log(&store);
                drop(cursor);
                assert_eq!(preparation.inner_used(), baseline);
                results.push((persistent, max_rows, result, later_admissions));
            }
        }
        assert!(
            results.iter().all(
                |(_, _, result, later)| matches!(result, Err(Error::Cancelled)) && *later == 0
            ),
            "each case must cancel without admitting more bytes: {results:?}"
        );
    }

    #[test]
    fn checked_history_cursor_keeps_source_deadline_during_page_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        fill_log(&store);
        let deadline = Instant::now() + Duration::from_secs(5);
        let preparation = Preparation::new(
            Control {
                deadline: Some(deadline),
                ..Control::none()
            },
            64 << 20,
        )
        .unwrap();
        let policy = Arc::new(GraphAccess::all());
        let view = store.prepare_read(policy.clone(), &preparation).unwrap();
        let baseline = preparation.inner_used();
        let mut cursor = select_source_cursor(
            view.read_source(),
            &store,
            QUERY,
            &QueryOptions {
                graphs: Some(policy),
                max_rows: Some(1),
                deadline: Some(deadline + Duration::from_secs(60)),
                ..Default::default()
            },
            &strict(),
        )
        .unwrap();
        cursor
            .deadline_pause()
            .store(60_000_000_000, Ordering::Relaxed);
        let source = preparation.clone();
        let (result, later_admissions) = pull_with_stop(&mut cursor, move || {
            source
                .check()
                .expect("the deadline must still be live at admission");
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            assert!(matches!(source.check(), Err(Error::Timeout)));
        });
        disable_log(&store);
        drop(cursor);
        assert_eq!(preparation.inner_used(), baseline);
        assert!(
            matches!(result, Err(Error::Timeout)) && later_admissions == 0,
            "the source deadline must stop admission: {result:?}, {later_admissions} later admissions"
        );
    }
}
