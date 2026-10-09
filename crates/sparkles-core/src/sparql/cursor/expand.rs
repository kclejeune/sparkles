//! Operators that map each input row to any number of output rows on their own: index
//! joins, LET, UNFOLD and triple-term decomposition in input order, and transitive
//! paths from the start nodes of their input. Each input
//! batch runs through the eager kernel. Its output is charged state that later pulls
//! resume, so one batch that expands into many solutions leaves the following batches
//! bounded by the caller's cap. The input demand follows the expansion seen so far.
use super::{Buffer, CursorOptions, Operator, copy_rows, exec};
use crate::error::Result;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::expr::Expr;
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::VarId;
use std::sync::Arc;

/// Whether a plan node runs as an expanding operator over its input's batches.
pub(super) fn eligible(node: &Node) -> bool {
    node.children.len() == 1
        && match &node.kind {
            Kind::IndexJoin(spec) => spec
                .probes
                .iter()
                .all(|probe| !probe.filter.iter().any(Expr::has_exists)),
            Kind::Assign(_, expr) => !expr.has_exists(),
            Kind::Unfold { expr, .. } => !expr.has_exists(),
            Kind::Unpack { .. } => true,
            // A path from the start nodes of its input, without an edge input.
            Kind::Path {
                bound_from_left: true,
                ..
            } => true,
            _ => false,
        }
}

/// The variables an expanding operator may bind in a row whose input leaves them
/// unbound, which moves the row out of its input's order on them.
pub(super) fn fills(node: &Node) -> Vec<VarId> {
    match &node.kind {
        Kind::IndexJoin(spec) => spec
            .probes
            .iter()
            .flat_map(|probe| probe.scan.cols.iter().map(|&(_, v)| v))
            .collect(),
        Kind::Assign(v, _) => vec![*v],
        Kind::Unfold { var, second, .. } => std::iter::once(*var).chain(*second).collect(),
        Kind::Unpack { parts, .. } => parts
            .iter()
            .filter_map(|p| match p {
                crate::sparql::plan::PathEnd::Var(v) => Some(*v),
                crate::sparql::plan::PathEnd::Const(_) => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub(super) struct Expand {
    node: Node,
    pending: Option<Buffer>,
    at: usize,
    /// Output rows per input row in the batches so far, at least one.
    ratio: f64,
}

impl Expand {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            pending: None,
            at: 0,
            ratio: 1.0,
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
        vars: &[VarId],
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
                if self.at >= buffer.table.len {
                    self.pending = None;
                    self.at = 0;
                }
                return Ok(batch);
            }
            if child.done {
                return Ok(None);
            }
            // Ask for about as many input rows as fill the cap at the expansion seen
            // so far, so that the caller's demand bounds the input too.
            let want = ((cap as f64 / self.ratio) as usize).clamp(1, cap);
            let Some(input) = child.next(ctx, options, want)? else {
                return Ok(None);
            };
            let rows = input.table.len;
            let Buffer { table, charge } = input;
            let mut input = Some(table);
            // The kernel holds its input under its own charge while it runs.
            drop(charge);
            let info = child.info.clone();
            let (table, _) = exec::execute_with_inputs(ctx, &self.node, &mut |_| {
                Ok((
                    input.take().expect("one input table per batch"),
                    info.clone(),
                ))
            })?;
            let mut output = Buffer {
                charge: OwnedCharge::new(ctx, super::capacity_bytes(&table))?,
                table,
            };
            output.project(vars)?;
            if rows > 0 {
                self.ratio = (output.table.len as f64 / rows as f64).max(1.0);
            }
            if !output.table.is_empty() {
                self.pending = Some(output);
                self.at = 0;
            }
        }
    }
}
