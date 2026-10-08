//! Pure scan predicates can amortize dictionary work over a larger, charged
//! input buffer without increasing the caller's output batch or demand prefix.
use super::{Buffer, CursorOptions, Operator, State, copy_rows, exec};
use crate::error::Result;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::exprcache::Report;
use crate::sparql::plan::{Kind, Node};
use std::sync::Arc;

const INPUT_ROWS: usize = 32_768;

pub(super) fn eligible(node: &Node, children: &[Operator]) -> bool {
    let Kind::Filter(exprs) = &node.kind else {
        return false;
    };
    // Never evaluate an application callback or impure input beyond demand.
    if !matches!(children, [Operator { state: State::Scan(scan), .. }] if scan.filter.is_empty()) {
        return false;
    }
    let refs = exprs.iter().collect::<Vec<_>>();
    let Ok(Some(variable)) = crate::sparql::exprcache::input(&refs) else {
        return false;
    };
    crate::sparql::keyfilter::KeyFilter::cursor_scratch_bytes(exprs, variable, INPUT_ROWS).is_some()
}

pub(super) struct Filter {
    node: Node,
    pending: Option<Buffer>,
    at: usize,
}

impl Filter {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            pending: None,
            at: 0,
        }
    }

    pub(super) fn done(&self, child_done: bool) -> bool {
        child_done && self.pending.is_none()
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        loop {
            ctx.check()?;
            if let Some(buffer) = &self.pending {
                if self.at == 0 && buffer.table.len <= cap {
                    return Ok(self.pending.take());
                }
                let batch = copy_rows(ctx, &buffer.table, self.at, cap)?;
                self.at += batch.as_ref().map_or(0, |b| b.table.len);
                if self.at == buffer.table.len {
                    self.pending = None;
                    self.at = 0;
                }
                return Ok(batch);
            }
            let row_bytes = (child.vars.len() as u64 * 16).saturating_add(192);
            // Include simultaneous input, pending/output copies, key-test reuse,
            // and the decoded block. Every actual allocation is also charged.
            let headroom = (INPUT_ROWS as u64)
                .saturating_mul(row_bytes)
                .saturating_add(crate::index::BLOCK_ROWS as u64 * 32 + 8192);
            let enlarge = cap >= 4096
                && ctx.max_rows_produced == u64::MAX
                && ctx.max_rows.saturating_sub(child.rows) >= INPUT_ROWS
                && ctx.memory_remaining() >= headroom;
            let mut input_options = options.clone();
            let demand = if enlarge { INPUT_ROWS.max(cap) } else { cap };
            input_options.batch_rows = demand.max(options.batch_rows);
            let Some(mut buffer) = child.next(ctx, &input_options, demand)? else {
                return Ok(None);
            };
            let scratch = OwnedCharge::new(
                ctx,
                buffer.table.len as u64 * 16 + ctx.nvars() as u64 * 16 + 128,
            )?;
            buffer.table =
                exec::apply_unary(ctx, &self.node, buffer.table, &mut Report::default())?;
            buffer.reconcile()?;
            drop(scratch);
            if !buffer.table.is_empty() {
                self.pending = Some(buffer);
            }
        }
    }
}
