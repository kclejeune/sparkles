//! Operators that map each input row to any number of output rows on their own: index
//! joins, LET, UNFOLD and triple-term decomposition in input order, transitive paths
//! from the start nodes of their input, LATERAL, ARQ property functions, registered
//! property functions that declare a prefix output over an input, and text, hybrid,
//! history and spatial calls whose arguments their input binds. Each input
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
        // LATERAL evaluates its right side for each left row, and an ARQ property
        // function solves each input row, so the output of an input batch is the
        // output of those rows, and a prefix of the input gives a subset of the
        // output.
        // A remote LATERAL sends the groups of a batch in bulk requests, so each input
        // batch is a whole number of its own requests.
        Kind::Lateral(_) => node.children.len() == 1,
        // A text or hybrid search over its group searches once per distinct query value of
        // a row and joins the hits to that row. A hybrid search whose vector candidates
        // come from its input ranks them all at once, so it reads its whole input.
        Kind::TextSearch(spec) => spec.needs_input() && node.children.len() == 1,
        Kind::HybridSearch(spec) => {
            spec.needs_input()
                && !spec.vector.as_ref().is_some_and(|v| v.candidates)
                && node.children.len() == 1
        }
        // History changes over the group look up the changes of each row's bound values.
        Kind::HistoryChanges(spec) => spec.needs_input() && node.children.len() == 1,
        // A spatial call whose arguments the group binds tests each row's arguments.
        Kind::SpatialPf(spec) => spec.needs_input() && node.children.len() == 1,
        // A call whose arguments are all constants is one call, as over a single empty
        // input row.
        Kind::PropertyFn(_) => node.children.len() <= 1,
        // A registered property function only when its registration declares that a
        // prefix of its input gives a prefix of its output.
        Kind::RegisteredProperty(spec) => spec.prefix_output() && node.children.len() == 1,
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
    /// The kernel's counters, added up over the batches it ran.
    counters: Option<serde_json::Map<String, serde_json::Value>>,
    /// Whether an operator without input has made its one call.
    called: bool,
}

impl Expand {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            pending: None,
            at: 0,
            ratio: 1.0,
            edges: None,
            counters: None,
            called: false,
        }
    }

    /// The kernel's counters over the batches so far, as one eager run reports them.
    pub(super) fn counters(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        self.counters.as_ref()
    }

    /// Add one batch's counters: numbers add up, and other values keep the latest.
    fn add_counters(&mut self, batch: Option<serde_json::Map<String, serde_json::Value>>) {
        let Some(batch) = batch else { return };
        let total = self.counters.get_or_insert_with(Default::default);
        for (key, value) in batch {
            let sum = match (total.get(&key).and_then(|v| v.as_u64()), value.as_u64()) {
                (Some(a), Some(b)) => Some(a.saturating_add(b).into()),
                _ => None,
            };
            total.insert(key, sum.unwrap_or(value));
        }
    }

    /// Serve the answer of an operator without input, which calls its kernel once.
    fn call(&mut self, ctx: &Arc<Ctx>, vars: &[VarId], cap: usize) -> Result<Option<Buffer>> {
        if !self.called {
            self.called = true;
            let (mut output, ran) = exec::execute_with_inputs(ctx, &self.node, &mut |_| {
                Err(crate::Error::invalid("a call without input reads no input"))
            })?;
            self.add_counters(ran.counters);
            let charge = match output.lease.is_some() {
                true => OwnedCharge::adopt(ctx, &mut output.lease)?,
                false => OwnedCharge::new(ctx, super::capacity_bytes(&output.table))?,
            };
            let mut output = Buffer {
                table: output.table,
                charge,
            };
            output.project(vars)?;
            if !output.table.is_empty() {
                self.pending = Some(output);
                self.at = 0;
            }
        }
        let Some(buffer) = &self.pending else {
            return Ok(None);
        };
        if self.at == 0 && buffer.table.len <= cap {
            return Ok(self.pending.take());
        }
        let batch = copy_rows(ctx, &buffer.table, self.at, cap)?;
        self.at += batch.as_ref().map_or(0, |b| b.table.len);
        if self.at >= buffer.table.len {
            self.pending = None;
            self.at = 0;
        }
        Ok(batch)
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
            [] => return self.call(ctx, vars, cap),
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
            let want = if matches!(&self.node.kind, Kind::Lateral(spec) if spec.service.is_some()) {
                // A remote LATERAL's requests carry many groups each, so it reads whole
                // batches and keeps what the caller did not take for the next pulls.
                options.batch_rows.max(1)
            } else {
                ((cap as f64 / self.ratio) as usize).clamp(1, cap)
            };
            let Some(input) = child.next(ctx, options, want)? else {
                return Ok(None);
            };
            let rows = input.table.len;
            let Buffer { table, charge } = input;
            let mut input = Some(crate::sparql::table::Materialized::with_owned(
                table, charge,
            )?);
            let mut output = match (&self.edges, &self.node.kind) {
                (Some((edges, _)), Kind::Path { spec, .. }) => {
                    let input = input.take().expect("one input table per batch");
                    let table = exec::path_batch(ctx, spec, edges, &input.table, &self.node.vars)?;
                    // Keep the actual input allocation paid through output admission.
                    // The path kernel's own output admission remains a separate edge.
                    let charge = OwnedCharge::new(ctx, super::capacity_bytes(&table))?;
                    Buffer { table, charge }
                }
                _ => {
                    let info = child.info.clone();
                    let (mut output, ran) =
                        exec::execute_with_inputs(ctx, &self.node, &mut |_| {
                            Ok((
                                input.take().expect("one input table per batch"),
                                info.clone(),
                            ))
                        })?;
                    self.add_counters(ran.counters);
                    if output.lease.is_none() {
                        return Err(crate::Error::invalid(
                            "expansion output lost its admitted input credit",
                        ));
                    }
                    let charge = OwnedCharge::adopt(ctx, &mut output.lease)?;
                    Buffer {
                        table: output.table,
                        charge,
                    }
                }
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
