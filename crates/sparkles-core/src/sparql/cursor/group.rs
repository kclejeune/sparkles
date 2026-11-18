//! The cursor's drivers of the grouping operator of [`crate::sparql::group`]. These
//! operators consume all input before output, and they retain groups and aggregates, not
//! all source rows.
//!
//! [`Group`] runs a grouping whose aggregates keep running state over the batches it
//! pulls, and reads the partitions that a grouping which spills passes rows to, one
//! pass each, as the operator describes.
//!
//! [`RunGroup`] runs a grouping whose aggregates need a group's rows in input order:
//! FOLD, which orders them itself, and registered aggregates, whose callbacks see one
//! group's rows at a time. The input reaches the grouping ordered on its keys, either
//! because its operator keeps that order or through a stable sort on the keys, which may
//! spill. So each group's rows arrive together and in input order, and the grouping holds
//! the rows of the groups it has not finished. Once its held rows reach a batch and hold
//! a finished group, the operator aggregates the finished groups over whole groups, and
//! their rows are freed. Every aggregate therefore sees the rows that eager execution
//! gives it, in the same order. The cost is that the grouping holds the whole of its
//! largest group, and an ungrouped aggregate holds all of its input, as eager execution
//! does.
use super::{Buffer, CursorOptions, Operator, copy_rows, merge};
use crate::error::Result;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::exprcache::Report;
use crate::sparql::group::{self as grouping, Grouping};
use crate::sparql::plan::{Agg, Kind, Node};
use crate::sparql::table::{Table, VarId};
use crate::spill::SpillReader;
use std::sync::Arc;

/// Whether an aggregate's result is the same whatever the order of its group's rows.
pub(super) fn order_free(ctx: &Ctx, agg: &Agg) -> bool {
    grouping::order_free(ctx, agg)
}

/// Whether a grouping with one input takes its rows in any batches, since every
/// aggregate keeps running state.
pub(super) fn eligible(ctx: &Ctx, node: &Node) -> bool {
    let Kind::Group { aggs, .. } = &node.kind else {
        return false;
    };
    node.children.len() == 1 && grouping::hashable(ctx, aggs)
}

/// Whether a grouping runs over its input's key runs: a grouping with one input whose
/// aggregates need their groups' rows.
pub(super) fn runs_eligible(ctx: &Ctx, node: &Node) -> bool {
    matches!(node.kind, Kind::Group { .. }) && node.children.len() == 1 && !eligible(ctx, node)
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

/// Charge an output table of the grouping as a buffer.
fn buffer(ctx: &Arc<Ctx>, table: Table) -> Result<Buffer> {
    Ok(Buffer {
        charge: OwnedCharge::new(ctx, super::capacity_bytes(&table))?,
        table,
    })
}

pub(super) struct Group {
    grouping: Grouping,
    loaded: bool,
    /// The input's variables, which spilled rows keep.
    input: Vec<VarId>,
    /// The rows this pass reads, after the first.
    source: Option<SpillReader>,
    /// What the plan reports.
    pub(super) spilled: u64,
    pub(super) passes: usize,
}

impl Group {
    pub(super) fn new(ctx: &Ctx, node: &Node) -> Result<Self> {
        let Kind::Group { keys, aggs } = &node.kind else {
            unreachable!("a grouping")
        };
        let input = &node.children[0].vars;
        Ok(Self {
            grouping: Grouping::new(ctx, keys, aggs, input, true, true)?,
            loaded: false,
            input: input.clone(),
            source: None,
            spilled: 0,
            passes: 0,
        })
    }

    fn load(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
    ) -> Result<()> {
        let counts = self.grouping.only_counts_rows();
        if counts
            && self.source.is_none()
            && let Some(count) = child.count_input(ctx, options)?
        {
            self.grouping.count_rows(count);
            self.loaded = true;
            return Ok(());
        }
        let mut report = Report::default();
        self.grouping.start_pass(ctx);
        // Every input row contributes even when a parent requests a tiny prefix.
        loop {
            let (table, _input_charge) = match &mut self.source {
                Some(reader) => match reader.next_frame()? {
                    Some((cols, len)) => {
                        let charge =
                            OwnedCharge::new(ctx, cols.len() as u64 * len as u64 * 8 + 256)?;
                        let mut table = Table::new(self.input.clone());
                        table.cols = cols;
                        table.len = len;
                        (table, charge)
                    }
                    None => break,
                },
                None => match child.next(ctx, options, options.batch_rows)? {
                    Some(Buffer { table, charge }) => (table, charge),
                    None => break,
                },
            };
            if counts {
                self.grouping.count_rows(table.len as u64);
                continue;
            }
            self.grouping.add(ctx, &table, &mut report)?;
        }
        ctx.check()?;
        self.grouping.end_input()?;
        self.spilled = self.grouping.spilled;
        self.source = None;
        self.loaded = true;
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
            if !self.loaded {
                self.load(ctx, child, options)?;
            }
            if let Some(table) = self.grouping.output(ctx, vars, cap)? {
                let mut output = buffer(ctx, table)?;
                output.reconcile()?;
                return Ok(Some(output));
            }
            // The pass is output: free it and read the rows it passed on.
            let Some(reader) = self.grouping.next_pass(ctx)? else {
                return Ok(None);
            };
            self.passes = self.grouping.passes;
            self.source = Some(reader);
            self.loaded = false;
        }
    }
}

pub(super) struct RunGroup {
    keys: Vec<VarId>,
    aggs: Vec<(VarId, Agg)>,
    /// The key columns of the input.
    cols: Vec<usize>,
    /// Input rows of groups not yet aggregated.
    held: Option<Buffer>,
    /// The first held row of the last group, which may not be complete. Each appended
    /// batch moves it by comparing only its own rows, so a group that spans many
    /// batches is read once.
    open: usize,
    /// Aggregated groups not yet returned, and how many of them were.
    output: Option<(Buffer, usize)>,
    input_done: bool,
    /// Whether the operator has run, which an ungrouped aggregate over no rows needs
    /// once.
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
            open: 0,
            output: None,
            input_done: false,
            ran: false,
        }
    }

    pub(super) fn done(&self) -> bool {
        self.input_done && self.held.is_none() && self.output.is_none() && self.ran
    }

    /// Move [`Self::open`] over the held rows from `from` on, which a batch appended.
    fn advance(&mut self, ctx: &Ctx, from: usize) -> Result<()> {
        let Some(held) = &self.held else {
            return Ok(());
        };
        let table = &held.table;
        if self.cols.is_empty() {
            return Ok(());
        }
        for row in from.max(1)..table.len {
            if row.is_multiple_of(4096) {
                ctx.check()?;
            }
            if self
                .cols
                .iter()
                .any(|&c| table.cols[c][row] != table.cols[c][row - 1])
            {
                self.open = row;
            }
        }
        Ok(())
    }

    fn aggregate(&mut self, ctx: &Arc<Ctx>, rows: Buffer, vars: &[VarId]) -> Result<()> {
        self.ran = true;
        let mut report = Report::default();
        // The operator's index arrays and output coexist with its input.
        let scratch = OwnedCharge::new(ctx, rows.table.len as u64 * 16 + 1024)?;
        let table = grouping::whole(
            ctx,
            &rows.table,
            &self.keys,
            &self.aggs,
            true,
            false,
            &mut report,
        )?;
        drop(scratch);
        drop(rows);
        let mut output = buffer(ctx, table)?;
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
            let from = held.table.len;
            merge::append(ctx, held, &batch.table, 0..batch.table.len)?;
            let len = held.table.len;
            drop(batch);
            self.advance(ctx, from)?;
            if self.input_done || len < options.batch_rows {
                continue;
            }
            // Aggregate the finished groups and keep the rows of the last one.
            let start = self.open;
            if start == 0 {
                continue;
            }
            let held = self.held.as_mut().expect("held rows");
            let finished = copy_rows(ctx, &held.table, 0, start)?.expect("finished groups");
            held.slice_in_place(start, len - start)?;
            self.open = 0;
            self.aggregate(ctx, finished, vars)?;
        }
    }
}
