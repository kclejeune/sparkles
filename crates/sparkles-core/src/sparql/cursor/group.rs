//! Incremental aggregate state over input batches. These operators consume all
//! input before output; they retain groups and aggregates, not all source rows.
use super::{Buffer, CursorOptions, Operator};
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::exec::{AggState, compute_column, stat_aggregate};
use crate::sparql::expr::Expr;
use crate::sparql::exprcache::Report;
use crate::sparql::plan::{Agg, Kind, Node};
use crate::sparql::table::VarId;
use crate::sparql::value::Value;
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::AggregateFunction;
use std::sync::Arc;

/// Whether an aggregate keeps running state: COUNT(*), or COUNT, SUM, AVG, MIN, MAX,
/// SAMPLE, GROUP_CONCAT or an ARQ statistics aggregate of an expression, with or
/// without DISTINCT. FOLD and registered aggregates need their group's
/// rows, and so do the other custom aggregates.
fn admitted(agg: &Agg) -> bool {
    agg.registered.is_none()
        && agg.fold.is_none()
        && match &agg.expr {
            None => matches!(agg.func, AggregateFunction::Count) && !agg.distinct,
            Some(_) => {
                matches!(
                    agg.func,
                    AggregateFunction::Count
                        | AggregateFunction::Sum
                        | AggregateFunction::Avg
                        | AggregateFunction::Min
                        | AggregateFunction::Max
                        | AggregateFunction::Sample
                        | AggregateFunction::GroupConcat { .. }
                ) || stat_aggregate(&agg.func).is_some()
            }
        }
}

pub(super) fn eligible(node: &Node) -> bool {
    let Kind::Group { aggs, .. } = &node.kind else {
        return false;
    };
    node.children.len() == 1 && aggs.iter().all(|(_, agg)| admitted(agg))
}

/// Where an aggregate's argument comes from in an input batch.
enum Arg {
    Star,
    Column(Option<usize>),
    Expr(Expr),
}

/// The running state of one aggregate of one group.
enum State {
    Agg(AggState),
    /// GROUP_CONCAT: the text so far, and whether every value had a lexical form.
    Concat {
        text: String,
        ok: bool,
        empty: bool,
    },
}

impl State {
    fn new(agg: &Agg) -> Self {
        match agg.func {
            AggregateFunction::GroupConcat { .. } => State::Concat {
                text: String::new(),
                ok: true,
                empty: true,
            },
            _ => State::Agg(AggState::new(agg)),
        }
    }
}

/// Groups found by their keys, in order of first appearance.
enum Index {
    One(FxHashMap<Id, usize>),
    Many(FxHashMap<Box<[Id]>, usize>),
}

pub(super) struct Group {
    keys: Vec<Option<usize>>,
    args: Vec<Arg>,
    aggs: Vec<Agg>,
    index: Index,
    /// The keys of every group, `keys.len()` per group.
    order: Vec<Id>,
    groups: usize,
    states: Vec<State>,
    payload: Vec<u64>,
    /// The values each DISTINCT aggregate has seen, by group and aggregate.
    seen: Option<FxHashSet<(u32, u32, Id)>>,
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
        let bytes = aggs.len() as u64 * 256 + keys.len() as u64 * 64 + 1024;
        let charge = ctx.retained_charge(bytes)?;
        let keys: Vec<Option<usize>> = keys
            .iter()
            .map(|k| input.iter().position(|v| v == k))
            .collect();
        let args = aggs
            .iter()
            .map(|(_, agg)| match &agg.expr {
                None => Arg::Star,
                Some(Expr::Var(v)) => Arg::Column(input.iter().position(|x| x == v)),
                Some(expr) => Arg::Expr(expr.clone()),
            })
            .collect();
        let mut group = Self {
            index: if keys.len() == 1 {
                Index::One(Default::default())
            } else {
                Index::Many(Default::default())
            },
            keys,
            args,
            aggs: aggs.iter().map(|(_, a)| a.clone()).collect(),
            order: Vec::new(),
            groups: 0,
            states: Vec::new(),
            payload: Vec::new(),
            seen: aggs
                .iter()
                .any(|(_, agg)| agg.distinct)
                .then(Default::default),
            loaded: false,
            at: 0,
            charge,
            bytes,
        };
        if group.keys.is_empty() {
            group.insert(ctx, &[])?;
        }
        Ok(group)
    }

    /// Add a group with these keys and return its number.
    fn insert(&mut self, ctx: &Ctx, key: &[Id]) -> Result<usize> {
        let at = self.groups;
        let na = self.aggs.len();
        ctx.check_rows(at.saturating_add(1))?;
        self.retain(
            512 + key.len() as u64 * 24 + na as u64 * (size_of::<State>() as u64 * 4 + 128),
        )?;
        match &mut self.index {
            Index::One(index) => {
                index.insert(key[0], at);
            }
            Index::Many(index) => {
                index.insert(key.into(), at);
            }
        }
        self.order.extend_from_slice(key);
        self.states.extend(self.aggs.iter().map(State::new));
        self.payload.extend(std::iter::repeat_n(0, na));
        self.groups += 1;
        Ok(at)
    }

    fn retain(&mut self, bytes: u64) -> Result<()> {
        let retained = self.bytes.saturating_add(bytes);
        if let Some(charge) = &mut self.charge {
            charge.resize(retained)?;
        }
        self.bytes = retained;
        Ok(())
    }

    fn find(&mut self, ctx: &Ctx, key: &[Id]) -> Result<usize> {
        let found = match &self.index {
            Index::One(index) => index.get(&key[0]).copied(),
            Index::Many(index) => index.get(key).copied(),
        };
        match found {
            Some(group) => Ok(group),
            None => self.insert(ctx, key),
        }
    }

    fn only_counts_rows(&self) -> bool {
        self.keys.is_empty() && self.args.iter().all(|arg| matches!(arg, Arg::Star))
    }

    fn load(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
    ) -> Result<()> {
        if self.only_counts_rows()
            && let Some(count) = child.count_input(ctx, options)?
        {
            for state in &mut self.states {
                if let State::Agg(AggState::Count(n)) = state {
                    *n += count;
                }
            }
            self.loaded = true;
            return Ok(());
        }
        let retain_payload = self
            .aggs
            .iter()
            .any(|agg| matches!(agg.func, AggregateFunction::Min | AggregateFunction::Max));
        let mut key = vec![Id::UNDEF; self.keys.len()];
        let mut report = Report::default();
        // Every input row contributes even when a parent requests a tiny prefix.
        while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
            let table = &batch.table;
            if self.only_counts_rows() {
                for state in &mut self.states {
                    if let State::Agg(AggState::Count(count)) = state {
                        *count += table.len as u64;
                    }
                }
                continue;
            }
            // Expression arguments are evaluated once per batch into charged columns.
            let _scratch = ctx.charge(
                self.args
                    .iter()
                    .filter(|arg| matches!(arg, Arg::Expr(_)))
                    .count() as u64
                    * (table.len as u64 * 8 + 64),
            )?;
            let computed = self
                .args
                .iter()
                .map(|arg| match arg {
                    Arg::Expr(expr) => compute_column(ctx, table, expr, &mut report).map(Some),
                    _ => Ok(None),
                })
                .collect::<Result<Vec<Option<Vec<Id>>>>>()?;
            for row in 0..table.len {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                for (k, column) in key.iter_mut().zip(&self.keys) {
                    *k = column.map_or(Id::UNDEF, |c| table.cols[c][row]);
                }
                let group = if self.keys.is_empty() {
                    0
                } else {
                    self.find(ctx, &key)?
                };
                for (a, values) in computed.iter().enumerate() {
                    let id = match (&self.args[a], values) {
                        (_, Some(values)) => Some(values[row]),
                        (Arg::Column(column), _) => {
                            Some(column.map_or(Id::UNDEF, |c| table.cols[c][row]))
                        }
                        _ => None,
                    };
                    self.add(ctx, group, a, id, retain_payload)?;
                }
            }
        }
        ctx.check()?;
        self.loaded = true;
        Ok(())
    }

    /// Add one row's value to aggregate `a` of `group`.
    fn add(
        &mut self,
        ctx: &Ctx,
        group: usize,
        a: usize,
        id: Option<Id>,
        retain_payload: bool,
    ) -> Result<()> {
        let at = group * self.aggs.len() + a;
        let agg = &self.aggs[a];
        // An error is never a duplicate of another value.
        if agg.distinct
            && let (Some(seen), Some(id)) = (&mut self.seen, id)
            && !id.is_undef()
        {
            if seen.contains(&(group as u32, a as u32, id)) {
                return Ok(());
            }
            // The set's entry, with room for its table to grow.
            self.retain(64)?;
            let seen = self.seen.as_mut().expect("a DISTINCT value set");
            seen.insert((group as u32, a as u32, id));
        }
        let agg = &self.aggs[a];
        if retain_payload
            && matches!(agg.func, AggregateFunction::Min | AggregateFunction::Max)
            && let Some(id) = id
        {
            let decoded = ctx.decoded_bytes(id)?;
            if decoded > self.payload[at] {
                let more = decoded - self.payload[at];
                self.retain(more)?;
                self.payload[at] = decoded;
            }
        }
        let agg = &self.aggs[a];
        match &mut self.states[at] {
            State::Agg(state) => state.add(ctx, agg, id),
            State::Concat { text, ok, empty } => {
                if !*ok {
                    return Ok(());
                }
                let id = id.unwrap_or(Id::UNDEF);
                if id.is_undef() {
                    *ok = false;
                    return Ok(());
                }
                let Some(value) = ctx.value(id) else {
                    return Ok(());
                };
                let Ok(lexical) = value.lexical() else {
                    *ok = false;
                    return Ok(());
                };
                let separator = match &agg.func {
                    AggregateFunction::GroupConcat { separator } => {
                        separator.as_deref().unwrap_or(" ")
                    }
                    _ => unreachable!("a GROUP_CONCAT state"),
                };
                let before = text.capacity();
                if !*empty {
                    text.push_str(separator);
                }
                text.push_str(&lexical);
                *empty = false;
                let grown = text.capacity().saturating_sub(before) as u64;
                // The string and its reallocation coexist while it grows.
                self.retain(grown.saturating_mul(2))?;
            }
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
        if !self.loaded {
            self.load(ctx, child, options)?;
            // The value sets are not needed for output.
            self.seen = None;
        }
        let n = cap.min(self.groups - self.at);
        if n == 0 {
            return Ok(None);
        }
        let nk = self.keys.len();
        let na = self.aggs.len();
        let mut output = Buffer::new(ctx, vars, n)?;
        for row in self.at..self.at + n {
            ctx.check()?;
            for k in 0..nk {
                output.table.cols[k].push(self.order[row * nk + k]);
            }
            for (a, agg) in self.aggs.iter().enumerate() {
                let state = std::mem::replace(
                    &mut self.states[row * na + a],
                    State::Agg(AggState::Count(0)),
                );
                output.table.cols[nk + a].push(match state {
                    State::Agg(state) => state.finish(ctx, agg),
                    State::Concat { ok: false, .. } => Id::UNDEF,
                    // SPARQL 1.1: the result is a simple literal.
                    State::Concat { text, .. } => ctx.intern_value(&Value::Str(text.into())),
                });
            }
            output.table.len += 1;
        }
        self.at += n;
        output.reconcile()?;
        Ok(Some(output))
    }
}
