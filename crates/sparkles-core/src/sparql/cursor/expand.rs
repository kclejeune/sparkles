//! Operators that map each input row to any number of output rows on their own: index
//! joins, LET, UNFOLD and triple-term decomposition in input order, and transitive
//! paths from the start nodes of their input. Each input
//! batch runs through the eager kernel. A path over a sequence or alternative first
//! reads its edge plan into a charged edge relation, which every batch then walks. Its output is charged state that later pulls
//! resume, so one batch that expands into many solutions leaves the following batches
//! bounded by the caller's cap. The input demand follows the expansion seen so far.
use super::{Buffer, CursorOptions, Operator, copy_rows, exec};
use crate::error::Result;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::VarId;
use std::sync::Arc;

/// Whether a plan node runs as an expanding operator over its input's batches.
pub(super) fn eligible(node: &Node) -> bool {
    match &node.kind {
        Kind::IndexJoin(_) | Kind::Assign(..) | Kind::Unfold { .. } | Kind::Unpack { .. } => {
            node.children.len() == 1
        }
        // A path from the start nodes of its input (the last child), after the edge
        // plan when it has one.
        Kind::Path {
            bound_from_left: true,
            spec,
        } => node.children.len() == 1 + usize::from(spec.edge_vars.is_some()),
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
    /// A path's edge relation and its charge, once its edge plan has been read.
    edges: Option<(crate::sparql::exec::PathEdges, OwnedCharge)>,
}

impl Expand {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            pending: None,
            at: 0,
            ratio: 1.0,
            edges: None,
        }
    }

    pub(super) fn done(&self, child_done: bool) -> bool {
        child_done && self.pending.is_none()
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let (child, edge_plan) = match children {
            [edges, input] => (input, Some(edges)),
            [input] => (input, None),
            _ => unreachable!("an expanding operator has one input"),
        };
        if let (Some(edge_plan), Kind::Path { spec, .. }) = (edge_plan, &self.node.kind)
            && self.edges.is_none()
        {
            self.edges = Some(super::walk::read_edges(ctx, spec, edge_plan, options)?);
        }
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
            let table = match (&self.edges, &self.node.kind) {
                (Some((edges, _)), Kind::Path { spec, .. }) => {
                    let input = input.take().expect("one input table per batch");
                    // The kernel holds its input while it runs, as eager execution does.
                    let _held = OwnedCharge::new(ctx, super::capacity_bytes(&input))?;
                    exec::path_batch(ctx, spec, edges, &input, &self.node.vars)?
                }
                _ => {
                    let info = child.info.clone();
                    exec::execute_with_inputs(ctx, &self.node, &mut |_| {
                        Ok((
                            input.take().expect("one input table per batch"),
                            info.clone(),
                        ))
                    })?
                    .0
                }
            };
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
