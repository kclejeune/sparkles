//! A text search with a constant query and no limit, read a page of hits at a time
//! (item 829).
//!
//! The search's hits are bounded by the index's `maxHits`, so it is a leaf under
//! decision 2 of the X14 plan. The operator holds the charged hit list that
//! [`crate::text::Reader`] keeps, and decodes rows from it as batches are pulled, rather
//! than a table of every row. A search with a limit keeps the bounded leaf it has, since
//! whether a limit is above `maxHits` is known only once the index is read.
//!
//! Nothing happens before the first pull, because operators are also built only to see
//! whether an EXISTS pattern streams. The checks, the search and its errors come at the
//! first pull, as they come when an eager run reaches the node.
use super::Buffer;
use crate::error::Result;
use crate::sparql::ctx::Ctx;
use crate::sparql::plan::{Kind, Node};
use std::sync::Arc;

/// Whether a plan node runs as a text reader: a search without a limit that reads no
/// input, in a build with full-text search.
pub(super) fn eligible(node: &Node) -> bool {
    cfg!(feature = "text")
        && matches!(&node.kind, Kind::TextSearch(spec) if spec.limit.is_none())
        && node.children.is_empty()
}

/// What handing out a batch charges for its header, with room to spare (192 bytes on a
/// 64-bit target).
#[cfg_attr(not(feature = "text"), allow(dead_code))]
const PUBLISH: usize = 256;

pub(super) struct Text {
    #[cfg_attr(not(feature = "text"), allow(dead_code))]
    node: Node,
    opened: bool,
    #[cfg(feature = "text")]
    reader: Option<(crate::text::Reader, crate::sparql::ctx::OwnedCharge)>,
}

impl Text {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            opened: false,
            #[cfg(feature = "text")]
            reader: None,
        }
    }

    /// Drop the hit list and its charge, once nothing reads it any more.
    pub(super) fn release(&mut self) {
        self.opened = true;
        #[cfg(feature = "text")]
        {
            self.reader = None;
        }
    }

    /// Whether the hits have been read to their end.
    pub(super) fn done(&self) -> bool {
        #[cfg(feature = "text")]
        {
            self.opened && self.reader.is_none()
        }
        #[cfg(not(feature = "text"))]
        {
            self.opened
        }
    }

    /// The next batch of at most `cap` rows, or `None` once the hits have ended.
    #[cfg(feature = "text")]
    pub(super) fn next(&mut self, ctx: &Arc<Ctx>, cap: usize) -> Result<Option<Buffer>> {
        use crate::sparql::ctx::OwnedCharge;
        let Kind::TextSearch(spec) = &self.node.kind else {
            unreachable!("a text search node")
        };
        if !self.opened {
            self.opened = true;
            let mut charge = OwnedCharge::new(ctx, 0)?;
            let reader =
                crate::text::Reader::open(ctx, spec, &self.node.vars, &mut |b| charge.resize(b))?;
            self.reader = reader.map(|r| (r, charge));
        }
        let Some((reader, _)) = &mut self.reader else {
            return Ok(None);
        };
        // The hits left bound the batch, and none left ends the answer before a batch.
        // Every hit left goes in a last batch when its buffer fits in what the hit list
        // leaves of the budget, since the hit list is released before the batch is
        // handed out. Any other batch also leaves room for the header it is handed out
        // in, and is not cut when it fits; a larger one is cut to half of what fits, so
        // that an operator above may hold a copy of it.
        let row = self.node.vars.len().max(1) * 8;
        // what `Buffer::new` charges beside its rows
        let buffer = self.node.vars.len() * 40 + 128;
        let within = ctx.rows_within_budget(self.node.vars.len());
        let remaining = reader.remaining();
        let rows = if remaining <= cap && remaining <= within.saturating_sub(buffer.div_ceil(row)) {
            remaining
        } else {
            let fit = within.saturating_sub((buffer + PUBLISH).div_ceil(row));
            let want = cap.min(remaining);
            if want <= fit {
                want
            } else {
                want.min((fit / 2).max(1))
            }
        };
        if rows == 0 {
            self.reader = None;
            return Ok(None);
        }
        let mut b = Buffer::new(ctx, &self.node.vars, rows)?;
        let ended = match reader.next(ctx, rows, &mut b.table) {
            Ok(ended) => ended,
            Err(e) => {
                self.reader = None;
                return Err(e);
            }
        };
        if ended {
            self.reader = None;
        }
        b.reconcile()?;
        Ok((b.table.len > 0).then_some(b))
    }

    #[cfg(not(feature = "text"))]
    pub(super) fn next(&mut self, _: &Arc<Ctx>, _: usize) -> Result<Option<Buffer>> {
        unreachable!("a text reader needs the text feature")
    }
}
