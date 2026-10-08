//! Incremental aggregate state over input batches. These operators consume all
//! input before output; they retain groups and aggregates, not all source rows.
use super::{Buffer, CursorOptions, Operator};
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::exec::{AggState, incremental_group_ok};
use crate::sparql::expr::Expr;
use crate::sparql::plan::{Agg, Kind, Node};
use crate::sparql::table::VarId;
use rustc_hash::FxHashMap;
use spargebra::algebra::AggregateFunction;
use std::sync::Arc;

pub(super) fn eligible(node: &Node) -> bool {
    let Kind::Group { keys, aggs } = &node.kind else {
        return false;
    };
    node.children.len() == 1 && incremental_group_ok(keys, aggs, &node.children[0].vars)
}

pub(super) struct Group {
    key: Option<usize>,
    columns: Vec<Option<usize>>,
    aggs: Vec<Agg>,
    index: FxHashMap<Id, usize>,
    order: Vec<Id>,
    states: Vec<AggState>,
    payload: Vec<u64>,
    loaded: bool,
    at: usize,
    charge: Option<RetainedCharge>,
    bytes: u64,
}

impl Group {
    pub(super) fn new(ctx: &Ctx, node: &Node) -> Result<Self> {
        let Kind::Group { keys, aggs } = &node.kind else {
            unreachable!()
        };
        let input = &node.children[0].vars;
        let bytes = aggs.len() as u64 * 256 + 1024;
        let charge = ctx.retained_charge(bytes)?;
        let key = keys.first().and_then(|k| input.iter().position(|v| v == k));
        let columns = aggs
            .iter()
            .map(|(_, agg)| match &agg.expr {
                Some(Expr::Var(v)) => input.iter().position(|x| x == v),
                _ => None,
            })
            .collect();
        let mut group = Self {
            key,
            columns,
            aggs: aggs.iter().map(|(_, a)| a.clone()).collect(),
            index: Default::default(),
            order: Vec::new(),
            states: Vec::new(),
            payload: Vec::new(),
            loaded: false,
            at: 0,
            charge,
            bytes,
        };
        if key.is_none() {
            group.insert(ctx, Id::UNDEF)?;
        }
        Ok(group)
    }

    fn insert(&mut self, ctx: &Ctx, key: Id) -> Result<usize> {
        let at = self.order.len();
        reserve_group(ctx, at, self.aggs.len(), &mut self.charge, &mut self.bytes)?;
        self.index.insert(key, at);
        self.order.push(key);
        self.states.extend(self.aggs.iter().map(AggState::new));
        self.payload.extend(std::iter::repeat_n(0, self.aggs.len()));
        Ok(at)
    }

    fn load(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
    ) -> Result<()> {
        let count_all = self.key.is_none()
            && self
                .aggs
                .iter()
                .all(|agg| agg.expr.is_none() && matches!(agg.func, AggregateFunction::Count));
        if count_all && let Some(count) = child.count_input(ctx, options)? {
            for state in &mut self.states {
                if let AggState::Count(n) = state {
                    *n += count;
                }
            }
            self.loaded = true;
            return Ok(());
        }
        let key_column = self.key;
        let aggs = &self.aggs;
        let columns = &self.columns;
        let na = aggs.len();
        let index = &mut self.index;
        let order = &mut self.order;
        let states = &mut self.states;
        let payload = &mut self.payload;
        let bytes = &mut self.bytes;
        let charge = &mut self.charge;
        let retain_payload = aggs
            .iter()
            .any(|agg| matches!(agg.func, AggregateFunction::Min | AggregateFunction::Max));
        // Every input row contributes even when a parent requests a tiny prefix.
        while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
            if key_column.is_none()
                && aggs
                    .iter()
                    .all(|agg| agg.expr.is_none() && matches!(agg.func, AggregateFunction::Count))
            {
                for state in &mut *states {
                    if let AggState::Count(count) = state {
                        *count += batch.table.len as u64;
                    }
                }
                continue;
            }
            for row in 0..batch.table.len {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let group = match key_column {
                    None => 0,
                    Some(column) => {
                        let key = batch.table.cols[column][row];
                        match index.entry(key) {
                            std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                            std::collections::hash_map::Entry::Vacant(entry) => {
                                let at = order.len();
                                reserve_group(ctx, at, na, charge, bytes)?;
                                order.push(key);
                                states.extend(aggs.iter().map(AggState::new));
                                payload.extend(std::iter::repeat_n(0, na));
                                entry.insert(at);
                                at
                            }
                        }
                    }
                };
                for (a, (agg, column)) in aggs.iter().zip(columns).enumerate() {
                    let at = group * na + a;
                    let id = column.map(|column| batch.table.cols[column][row]);
                    if retain_payload
                        && matches!(agg.func, AggregateFunction::Min | AggregateFunction::Max)
                        && let Some(id) = id
                    {
                        let decoded_bytes = ctx.decoded_bytes(id)?;
                        if decoded_bytes > payload[at] {
                            let retained = bytes.saturating_add(decoded_bytes - payload[at]);
                            if let Some(charge) = charge {
                                charge.resize(retained)?;
                            }
                            *bytes = retained;
                            payload[at] = decoded_bytes;
                        }
                    }
                    states[at].add(ctx, agg, id);
                }
            }
        }
        ctx.check()?;
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
        if !self.loaded {
            self.load(ctx, child, options)?;
        }
        let n = cap.min(self.order.len() - self.at);
        if n == 0 {
            return Ok(None);
        }
        let mut output = Buffer::new(ctx, vars, n)?;
        for row in self.at..self.at + n {
            ctx.check()?;
            let offset = usize::from(self.key.is_some());
            if offset == 1 {
                output.table.cols[0].push(self.order[row]);
            }
            for (a, agg) in self.aggs.iter().enumerate() {
                let state = std::mem::replace(
                    &mut self.states[row * self.aggs.len() + a],
                    AggState::Count(0),
                );
                output.table.cols[offset + a].push(state.finish(ctx, agg));
            }
            output.table.len += 1;
        }
        self.at += n;
        output.reconcile()?;
        Ok(Some(output))
    }
}

fn reserve_group(
    ctx: &Ctx,
    rows: usize,
    aggregates: usize,
    charge: &mut Option<RetainedCharge>,
    bytes: &mut u64,
) -> Result<()> {
    ctx.check_rows(rows.saturating_add(1))?;
    let retained = bytes.saturating_add(
        512 + aggregates as u64 * (std::mem::size_of::<AggState>() as u64 * 4 + 128),
    );
    if let Some(charge) = charge {
        charge.resize(retained)?;
    }
    *bytes = retained;
    Ok(())
}
