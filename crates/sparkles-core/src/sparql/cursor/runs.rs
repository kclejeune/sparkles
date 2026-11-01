//! Groupings whose aggregates need a group's rows in input order: FOLD, which orders
//! them itself, and registered aggregates, whose callbacks see one group's rows at a
//! time. The input reaches the grouping ordered on its keys, either because its
//! operator keeps that order or through a stable sort on the keys, which may spill. So
//! each group's rows arrive together and in input order, and the grouping holds the rows
//! of the groups it has not finished. Once its held rows reach a batch and hold a
//! finished group, the eager kernel aggregates the finished groups, and their rows are
//! freed. Every aggregate therefore sees the rows that eager execution gives it, in the
//! same order. The cost is that the grouping holds the whole of its largest group, and
//! an ungrouped aggregate holds all of its input, as eager execution does.
use super::{Buffer, CursorOptions, Operator, copy_rows, exec, merge};
use crate::error::Result;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::exprcache::Report;
use crate::sparql::plan::{Agg, Kind, Node};
use crate::sparql::table::VarId;
use std::sync::Arc;

/// Whether a grouping runs over its input's key runs: a grouping with one input whose
/// aggregates the incremental grouping cannot hold.
pub(super) fn eligible(node: &Node) -> bool {
    matches!(node.kind, Kind::Group { .. })
        && node.children.len() == 1
        && !super::group::eligible(node)
}

/// The keys of a grouping that its input binds, which its input must be ordered on.
/// A key the input does not bind is unbound in every row.
pub(super) fn sort_keys(node: &Node, input: &[VarId]) -> Vec<VarId> {
    let Kind::Group { keys, .. } = &node.kind else {
        return Vec::new();
    };
    let mut present: Vec<VarId> = keys.iter().copied().filter(|k| input.contains(k)).collect();
    present.dedup();
    present
}

/// Whether an input order holds every row of a group together: its first variables are
/// the keys, in any order.
pub(super) fn grouped_by(order: &[VarId], keys: &[VarId]) -> bool {
    order.len() >= keys.len() && keys.iter().all(|k| order[..keys.len()].contains(k))
}

pub(super) struct RunGroup {
    keys: Vec<VarId>,
    aggs: Vec<(VarId, Agg)>,
    /// The key columns of the input.
    cols: Vec<usize>,
    /// Input rows of groups not yet aggregated.
    held: Option<Buffer>,
    /// Aggregated groups not yet returned, and how many of them were.
    output: Option<(Buffer, usize)>,
    input_done: bool,
    /// Whether the kernel has run, which an ungrouped aggregate over no rows needs once.
    ran: bool,
}

impl RunGroup {
    pub(super) fn new(node: &Node, input: &[VarId]) -> Self {
        let Kind::Group { keys, aggs } = &node.kind else {
            unreachable!("a grouping")
        };
        let cols = sort_keys(node, input)
            .iter()
            .filter_map(|k| input.iter().position(|v| v == k))
            .collect();
        Self {
            keys: keys.clone(),
            aggs: aggs.clone(),
            cols,
            held: None,
            output: None,
            input_done: false,
            ran: false,
        }
    }

    pub(super) fn done(&self) -> bool {
        self.input_done && self.held.is_none() && self.output.is_none() && self.ran
    }

    /// The first row of the last group that `table` holds, which may not be complete.
    fn last_start(&self, table: &crate::sparql::table::Table) -> usize {
        if self.cols.is_empty() || table.len == 0 {
            return 0;
        }
        let last = table.len - 1;
        let same = |row: usize| {
            self.cols
                .iter()
                .all(|&c| table.cols[c][row] == table.cols[c][last])
        };
        let mut start = last;
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        start
    }

    fn aggregate(&mut self, ctx: &Arc<Ctx>, rows: Buffer, vars: &[VarId]) -> Result<()> {
        self.ran = true;
        let mut report = Report::default();
        // The kernel's index arrays and output coexist with its input.
        let scratch = OwnedCharge::new(ctx, rows.table.len as u64 * 16 + 1024)?;
        let table = exec::group(ctx, &rows.table, &self.keys, &self.aggs, &mut report)?;
        drop(scratch);
        drop(rows);
        let mut output = Buffer {
            charge: OwnedCharge::new(ctx, super::capacity_bytes(&table))?,
            table,
        };
        output.project(vars)?;
        if !output.table.is_empty() {
            self.output = Some((output, 0));
        }
        Ok(())
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        loop {
            ctx.check()?;
            if let Some((buffer, at)) = &mut self.output {
                let batch = copy_rows(ctx, &buffer.table, *at, cap)?;
                *at += batch.as_ref().map_or(0, |b| b.table.len);
                if *at >= buffer.table.len {
                    self.output = None;
                }
                if batch.is_some() {
                    return Ok(batch);
                }
                continue;
            }
            if self.input_done {
                match self.held.take() {
                    Some(rows) => self.aggregate(ctx, rows, vars)?,
                    // An ungrouped aggregate has one row over no input.
                    None if !self.ran && self.keys.is_empty() => {
                        let rows = Buffer::new(ctx, &child.vars, 0)?;
                        self.aggregate(ctx, rows, vars)?;
                    }
                    None => {
                        self.ran = true;
                        return Ok(None);
                    }
                }
                continue;
            }
            let Some(batch) = child.next(ctx, options, options.batch_rows)? else {
                self.input_done = true;
                continue;
            };
            if child.done {
                self.input_done = true;
            }
            let held = match &mut self.held {
                Some(held) => held,
                None => self.held.insert(Buffer::new(ctx, &child.vars, 0)?),
            };
            merge::append(ctx, held, &batch.table, 0..batch.table.len)?;
            drop(batch);
            if self.input_done || held.table.len < options.batch_rows {
                continue;
            }
            // Aggregate the finished groups and keep the rows of the last one.
            let start = self.last_start(&self.held.as_ref().expect("held rows").table);
            if start == 0 {
                continue;
            }
            let held = self.held.as_mut().expect("held rows");
            let finished = copy_rows(ctx, &held.table, 0, start)?.expect("finished groups");
            let rest = held.table.len - start;
            held.slice_in_place(start, rest)?;
            self.aggregate(ctx, finished, vars)?;
        }
    }
}
