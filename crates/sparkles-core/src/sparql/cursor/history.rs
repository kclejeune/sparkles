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
            let charge = OwnedCharge::new(ctx, 0)?;
            let pages = log.pages(q, ctx.snap.commit)?;
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
                    if rows.fill(ctx, c, &mut row) {
                        b.table.push_row(&row);
                    }
                }
                self.at += n;
                // a term that could not be interned within the budget fails here
                ctx.check()?;
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
            let page = pages.next(PAGE, limit, &mut |b| charge.resize(b))?;
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
