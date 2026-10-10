//! Transitive paths without inputs, walked one start node at a time. The reached
//! nodes of one start are charged state that later pulls resume, and the start nodes
//! of the graph being walked are charged while they are kept. A path over a sequence
//! or alternative first reads its edge plan's batches into a charged edge relation,
//! which it keeps while it walks.
use super::{Buffer, CursorOptions, Operator, copy_rows, exec};
use crate::error::Result;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::VarId;
use std::sync::Arc;

pub(super) fn eligible(node: &Node) -> bool {
    match &node.kind {
        Kind::Path {
            bound_from_left: false,
            spec,
        } => match node.children.len() {
            0 => true,
            1 => spec.edge_vars.is_some(),
            _ => false,
        },
        _ => false,
    }
}

/// Read every batch of an edge plan into a charged edge relation. The charge grows
/// before each batch is added and is kept with the relation.
pub(super) fn read_edges(
    ctx: &Arc<Ctx>,
    spec: &crate::sparql::plan::PathSpec,
    child: &mut Operator,
    options: &CursorOptions,
) -> Result<(exec::PathEdges, OwnedCharge)> {
    let mut edges = exec::PathEdges::default();
    let mut charge = OwnedCharge::new(ctx, edges.bytes_with(0))?;
    while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
        charge.resize(edges.bytes_with(batch.table.len))?;
        edges.add(spec, &batch.table)?;
        charge.resize(edges.bytes_with(0))?;
    }
    Ok((edges, charge))
}

/// The variable a walk's output is in order of, if any.
pub(super) fn order(node: &Node) -> Vec<VarId> {
    match &node.kind {
        Kind::Path { spec, .. } => exec::PathWalk::ordered_on(spec).into_iter().collect(),
        _ => Vec::new(),
    }
}

pub(super) struct Walk {
    walk: exec::PathWalk,
    pending: Option<Buffer>,
    at: usize,
    finished: bool,
    starts: Option<OwnedCharge>,
    /// The edge relation's charge, once its edge plan has been read.
    edges: Option<OwnedCharge>,
    spec: crate::sparql::plan::PathSpec,
}

impl Walk {
    pub(super) fn new(ctx: &Ctx, node: &Node) -> Result<Self> {
        let Kind::Path { spec, .. } = &node.kind else {
            unreachable!("a walk is a path")
        };
        Ok(Self {
            walk: exec::PathWalk::new(ctx, spec, &node.vars)?,
            pending: None,
            at: 0,
            finished: false,
            starts: None,
            edges: None,
            spec: (**spec).clone(),
        })
    }

    pub(super) fn done(&self) -> bool {
        self.finished && self.pending.is_none()
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if let [child] = children
            && self.edges.is_none()
        {
            let (edges, charge) = read_edges(ctx, &self.spec, child, options)?;
            self.walk.set_edges(edges);
            self.edges = Some(charge);
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
            if self.finished {
                return Ok(None);
            }
            let next = self.walk.next(ctx)?;
            let starts = self.walk.retained() as u64 * 8;
            match &mut self.starts {
                Some(charge) => charge.resize(starts)?,
                None => self.starts = Some(OwnedCharge::new(ctx, starts)?),
            }
            let Some(table) = next else {
                self.finished = true;
                return Ok(None);
            };
            let mut output = Buffer {
                charge: OwnedCharge::new(ctx, super::capacity_bytes(&table))?,
                table,
            };
            output.project(vars)?;
            self.pending = Some(output);
            self.at = 0;
        }
    }
}
