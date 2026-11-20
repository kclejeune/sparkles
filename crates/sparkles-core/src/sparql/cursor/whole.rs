//! Operators that read all of their input before they answer, and whose answer is
//! bounded: a vector or hybrid search that ranks the candidate subjects of its whole
//! input against its global `k` or limit, a path search over its group under a limit,
//! which takes its paths in one order across all of its sources, and an algorithm call
//! over its group, which runs once from all of its sources, and a count over a merge
//! join on two or more variables, whose one row the eager kernel counts (item 1129). A path search under a limit
//! or an algorithm call whose edges come from a nested pattern reads that pattern whole
//! in the same way, also when its sources are constants.
//!
//! The operator reads each input cursor's batches into a charged table, so a budget
//! that cannot hold the input refuses the query as a sort without spill does. When the
//! input has ended it calls the kernel once, charges the answer and serves it batch by
//! batch. It calls the kernel functions itself rather than the eager executor's node
//! dispatch.
use super::{Buffer, CursorOptions, Operator, capacity_bytes, copy_rows, merge};
use crate::error::Result;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::{Table, VarId};
use std::sync::Arc;

/// Whether a plan node runs as a whole-input operator.
pub(super) fn eligible(node: &Node) -> bool {
    match &node.kind {
        Kind::VectorSearch(spec) => {
            spec.candidates && !spec.order_fallback && node.children.len() == 1
        }
        Kind::HybridSearch(spec) => {
            spec.vector.as_ref().is_some_and(|v| v.candidates) && node.children.len() == 1
        }
        // A search or call whose edges come from a nested pattern reads that pattern
        // whole too, as its last child, whether or not it also reads its group.
        Kind::PathSearch(spec) => !node.children.is_empty() && spec.limit.is_some(),
        // Decision 2 of the X14 plan counts an algorithm call as bounded.
        Kind::AlgoCall(_) => !node.children.is_empty(),
        // A count over a merge join on two or more variables has one row.
        Kind::CountJoin { .. } => super::countjoin::eager_kernel(node),
        _ => false,
    }
}

/// What a whole-input operator reads before it answers, for its plan's reason: its
/// group, or only the nested pattern of its edges.
pub(super) fn reads(node: &Node) -> &'static str {
    match super::leaf::reads_group(node) {
        true => "all of its input",
        false => "all of its edge pattern",
    }
}

/// The bound on the answer of a whole-input operator, for its plan's reason.
pub(super) fn bound(node: &Node) -> Option<String> {
    let per_query = |var: bool, k: usize| match var {
        true => format!("at most {k} rows per distinct query value"),
        false => format!("at most {k} rows"),
    };
    match &node.kind {
        Kind::VectorSearch(spec) => Some(per_query(
            matches!(spec.query, crate::sparql::plan::VectorQuery::Var(_)),
            spec.k,
        )),
        Kind::HybridSearch(spec) => Some(format!(
            "at most {} rows per pair of query values",
            spec.limit
        )),
        Kind::PathSearch(spec) => spec.limit.map(|k| per_query(false, k)),
        _ => None,
    }
}

pub(super) struct Whole {
    node: Node,
    answer: Option<Buffer>,
    at: usize,
    /// The kernel's counters and the note of its plan node, once it has run.
    pub(super) counters: Option<serde_json::Map<String, serde_json::Value>>,
    note: Option<String>,
}

impl Whole {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            answer: None,
            at: 0,
            counters: None,
            note: None,
        }
    }

    /// The description of the plan node with the kernel's note, once it has run.
    pub(super) fn description(&self) -> Option<String> {
        self.note
            .as_ref()
            .map(|note| format!("{} {note}", self.node.desc))
    }

    pub(super) fn done(&self) -> bool {
        self.answer.is_none() && self.counters.is_some()
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        order: &[VarId],
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if self.counters.is_none() {
            let mut inputs = Vec::with_capacity(children.len());
            let count = matches!(self.node.kind, Kind::CountJoin { .. });
            for child in children.iter_mut() {
                // A count over a join with an empty left input reads nothing of its right
                // input, as the eager count does.
                if count && inputs.first().is_some_and(|l: &Buffer| l.table.is_empty()) {
                    child.mark_skipped(crate::sparql::exec::LEFT_EMPTY);
                    inputs.push(Buffer::new(ctx, &child.vars, 0)?);
                    continue;
                }
                let mut input = read(ctx, child, options)?;
                // The count merges its inputs when they are sorted on a shared key, as
                // the eager tables say they are.
                if count {
                    input.set_order(&child.order)?;
                }
                inputs.push(input);
            }
            let mut answer = self.run(ctx, &mut inputs)?;
            drop(inputs);
            if !answer.table.sorted.starts_with(order) {
                // Row indices and the reordered copy coexist with the answer.
                answer.charge.resize(
                    capacity_bytes(&answer.table)
                        .saturating_mul(2)
                        .saturating_add(answer.table.len as u64 * 8),
                )?;
                answer.table.sort_by_vars(order);
                ctx.check()?;
                answer.charge.resize(capacity_bytes(&answer.table))?;
            }
            answer.project(vars)?;
            if !answer.table.is_empty() {
                self.answer = Some(answer);
            }
        }
        let Some(answer) = &self.answer else {
            return Ok(None);
        };
        if self.at == 0 && answer.table.len <= cap {
            return Ok(self.answer.take());
        }
        let batch = copy_rows(ctx, &answer.table, self.at, cap)?;
        self.at += batch.as_ref().map_or(0, |b| b.table.len);
        if self.at >= answer.table.len {
            self.answer = None;
        }
        Ok(batch)
    }

    /// Run the kernel once over the charged input tables, which it takes.
    fn run(&mut self, ctx: &Arc<Ctx>, inputs: &mut [Buffer]) -> Result<Buffer> {
        let last = inputs.len().saturating_sub(1);
        // The group is the first child when the kernel reads one, and the edge pattern
        // the last.
        let group = super::leaf::reads_group(&self.node);
        let mut take =
            |i: usize| -> Table { std::mem::replace(&mut inputs[i].table, Table::new(Vec::new())) };
        let vars = &self.node.vars;
        let (table, counters, lease) = match &self.node.kind {
            Kind::VectorSearch(spec) => {
                let (t, c) = crate::sparql::exec::vector_search(ctx, spec, Some(take(0)), vars)?;
                (t, c, None)
            }
            Kind::HybridSearch(spec) => {
                let input = take(0);
                let mut note = None;
                let (t, c) = crate::sparql::hybrid::search_bound(ctx, spec, &input, &mut note)?;
                self.note = note;
                (t.project(vars), c, None)
            }
            Kind::PathSearch(spec) => {
                let edges = spec.edge_pattern.is_some().then(|| take(last));
                let input = group.then(|| take(0));
                let (t, c) = crate::sparql::pathsearch::run(ctx, spec, input, edges, vars)?;
                (t, c, None)
            }
            Kind::AlgoCall(spec) => {
                let edges = spec.proj.pattern.is_some().then(|| take(last));
                let input = group.then(|| take(0));
                let (t, c) = crate::sparql::algosvc::run(ctx, spec, input, edges, vars)?;
                (t.table, c, t.lease)
            }
            Kind::CountJoin { algo, var, .. } => {
                let (l, r) = (take(0), take(1));
                let c = match l.is_empty() || r.is_empty() {
                    true => 0,
                    false => crate::sparql::exec::join_count(
                        ctx,
                        &l,
                        &r,
                        *algo == crate::sparql::plan::JoinAlgo::Merge,
                    )?,
                };
                let mut t = Table::new(vec![*var]);
                t.push_row(&[crate::id::Id::from_i64(c as i64).unwrap_or(crate::id::Id::UNDEF)]);
                (t, serde_json::Map::new(), None)
            }
            _ => unreachable!("{} is not a whole-input operator", self.node.operator()),
        };
        self.counters = Some(counters);
        let mut lease = lease;
        let charge = match lease.is_some() {
            true => OwnedCharge::adopt(ctx, &mut lease)?,
            false => OwnedCharge::new(ctx, capacity_bytes(&table))?,
        };
        Ok(Buffer { table, charge })
    }
}

/// Read every batch of `child` into one charged table.
fn read(ctx: &Arc<Ctx>, child: &mut Operator, options: &CursorOptions) -> Result<Buffer> {
    let mut input = Buffer::new(ctx, &child.vars, 0)?;
    while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
        merge::append(ctx, &mut input, &batch.table, 0..batch.table.len)?;
    }
    Ok(input)
}
