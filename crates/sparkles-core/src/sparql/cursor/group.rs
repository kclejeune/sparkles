//! Incremental aggregate state over input batches. These operators consume all
//! input before output; they retain groups and aggregates, not all source rows.
//!
//! A query that may spill aggregates in passes (X08 §6.5). Once the groups of a pass
//! hold half the memory the query had when the grouping started, or a new group does
//! not fit, the pass takes no new groups. Rows of groups it already holds are still
//! aggregated, and rows of other groups are written in input order to one of
//! [`PARTITIONS`] spill files chosen by a hash of their keys. After the pass's groups
//! are output and freed, each file is read by a pass of its own, which partitions what
//! it passes on with another hash. Every row of a group lands in the same pass, in
//! input order, so each aggregate sees exactly the rows it would see in memory, and
//! each pass takes at least one group.
use super::{Buffer, CursorOptions, Operator};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, OwnedCharge, RetainedCharge};
use crate::sparql::exec::{AggState, compute_column, stat_aggregate};
use crate::sparql::expr::Expr;
use crate::sparql::exprcache::Report;
use crate::sparql::plan::{Agg, Kind, Node};
use crate::sparql::table::{Table, VarId};
use crate::sparql::value::Value;
use crate::spill::{QuerySpill, SpillKind, SpillReader, SpillRun, SpillWriter};
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::AggregateFunction;
use std::sync::Arc;

/// Whether an aggregate keeps running state: COUNT(*), COUNT(DISTINCT *), or COUNT,
/// SUM, AVG, MIN, MAX, SAMPLE, GROUP_CONCAT or an ARQ statistics aggregate of an
/// expression, with or without DISTINCT. FOLD orders its group's rows and registered
/// aggregates run one group's accumulator at a time, so both need their group's rows,
/// and so do the other custom aggregates.
fn admitted(agg: &Agg) -> bool {
    agg.registered.is_none()
        && agg.fold.is_none()
        && match &agg.expr {
            None => matches!(agg.func, AggregateFunction::Count),
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

/// An input row that a COUNT(DISTINCT *) has counted: its group, its aggregate and
/// every value of the row.
type SeenRow = (u32, u32, Box<[Id]>);

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
    /// The input rows each COUNT(DISTINCT *) has seen, by group and aggregate.
    rows_seen: Option<FxHashSet<SeenRow>>,
    loaded: bool,
    at: usize,
    charge: Option<RetainedCharge>,
    bytes: u64,
    /// The bytes the charge holds, at least `bytes`.
    reserved: u64,
    /// The state's bytes before its first group.
    base: u64,
    /// The input's variables, which spilled rows keep.
    input: Vec<VarId>,
    /// Where rows of groups that do not fit go, when the query may spill.
    query: Option<Arc<QuerySpill>>,
    /// The state's bytes at which a pass takes no new groups, half the room the query
    /// had when the grouping started.
    threshold: u64,
    /// Whether this pass takes no new groups.
    frozen: bool,
    /// The rows this pass passes on, by partition.
    writers: Vec<Option<SpillWriter>>,
    /// The partitions later passes read, each with how many times its rows were
    /// partitioned, which seeds the hash of its own partitions.
    pending: Vec<(Arc<SpillRun>, u64)>,
    /// How many times the rows of this pass were partitioned.
    depth: u64,
    /// The rows this pass reads, after the first.
    source: Option<SpillReader>,
    /// What the plan reports.
    pub(super) spilled: u64,
    pub(super) passes: usize,
}

/// The slot of a row whose group this pass does not hold.
const PASSED: u32 = u32::MAX;
/// The spill files one pass divides the rows it passes on among.
const PARTITIONS: usize = 16;
/// The least state a pass holds before it takes no new groups.
const MIN_PASS_BYTES: u64 = 256 << 10;

/// The least step by which the retained charge of the group state grows.
const RETAIN_STEP: u64 = 64 << 10;

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
                .any(|(_, agg)| agg.distinct && agg.expr.is_some())
                .then(Default::default),
            rows_seen: aggs
                .iter()
                .any(|(_, agg)| agg.distinct && agg.expr.is_none())
                .then(Default::default),
            loaded: false,
            at: 0,
            charge,
            bytes,
            reserved: bytes,
            base: bytes,
            input: input.clone(),
            query: None,
            threshold: 0,
            frozen: false,
            writers: Vec::new(),
            pending: Vec::new(),
            depth: 0,
            source: None,
            spilled: 0,
            passes: 0,
        };
        if group.keys.is_empty() {
            group.insert(ctx, &[])?;
        } else {
            group.query = ctx.spill.clone();
        }
        Ok(group)
    }

    /// Add a group with these keys and return its number.
    fn insert(&mut self, ctx: &Ctx, key: &[Id]) -> Result<usize> {
        let at = self.groups;
        let na = self.aggs.len();
        ctx.check_rows(at.saturating_add(1))?;
        // The index entry, twice while the index grows, a boxed key for several key
        // columns, the key in `order`, and each aggregate's state, twice while `states`
        // grows, with its payload slot. Text and values that states own are charged as
        // they grow.
        let nk = key.len() as u64;
        let entry = if nk == 1 { 17 } else { 8 * nk + 16 + 25 };
        self.retain(entry * 2 + nk * 8 + na as u64 * (size_of::<State>() as u64 * 2 + 8) + 16)?;
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

    /// Count `bytes` more of retained state. The charge grows ahead in steps of at
    /// least 64 KiB, still before the state does, so that adding many small groups does
    /// not update the memory ledger for each of them. It overstates the state by less
    /// than one step.
    fn retain(&mut self, bytes: u64) -> Result<()> {
        let retained = self.bytes.saturating_add(bytes);
        if retained > self.reserved {
            let reserved = retained.saturating_add((retained / 8).max(RETAIN_STEP));
            if let Some(charge) = &mut self.charge {
                if charge.resize(reserved).is_err() {
                    // Near the budget, reserve only what the state needs.
                    charge.resize(retained)?;
                    self.reserved = retained;
                } else {
                    self.reserved = reserved;
                }
            } else {
                self.reserved = reserved;
            }
        }
        self.bytes = retained;
        Ok(())
    }

    /// The slot of the group with these keys, adding the group unless this pass takes
    /// no new groups, when the slot is [`PASSED`].
    fn find(&mut self, ctx: &Ctx, key: &[Id]) -> Result<u32> {
        let found = match &self.index {
            Index::One(index) => index.get(&key[0]).copied(),
            Index::Many(index) => index.get(key).copied(),
        };
        match found {
            Some(group) => Ok(group as u32),
            None => self.place(ctx, key),
        }
    }

    /// Add a group with these keys, or pass its rows on when the pass is full.
    fn place(&mut self, ctx: &Ctx, key: &[Id]) -> Result<u32> {
        // A pass also stops taking groups when what else the query holds leaves less
        // than a quarter of the room it had, so that the batches in flight still fit.
        if self.query.is_some()
            && self.groups > 0
            && (self.bytes >= self.threshold || ctx.memory_room() < self.threshold / 2)
        {
            self.frozen = true;
        }
        if self.frozen {
            return Ok(PASSED);
        }
        match self.insert(ctx, key) {
            Ok(group) => Ok(group as u32),
            // A pass that holds a group passes on the groups that do not fit.
            Err(Error::BudgetExceeded(b))
                if self.query.is_some()
                    && self.groups > 0
                    && matches!(
                        b.kind,
                        crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
                    ) =>
            {
                self.frozen = true;
                Ok(PASSED)
            }
            Err(error) => Err(error),
        }
    }

    /// Write the rows of `table` whose slot is [`PASSED`] to the partitions of later
    /// passes.
    fn pass_on(&mut self, ctx: &Ctx, table: &Table, slots: &[u32], rows: usize) -> Result<()> {
        use std::hash::{Hash, Hasher};
        let query = self.query.clone().expect("a spilling grouping");
        let _gather = ctx.charge(rows as u64 * (table.cols.len() as u64 * 8 + 8) + 256)?;
        let seed = self.depth.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5bd1_e995;
        let mut parts: Vec<Vec<usize>> = vec![Vec::new(); PARTITIONS];
        for (row, _) in slots
            .iter()
            .enumerate()
            .filter(|&(_, &slot)| slot == PASSED)
        {
            let mut hasher = rustc_hash::FxHasher::default();
            for key in &self.keys {
                key.map_or(Id::UNDEF, |c| table.cols[c][row])
                    .hash(&mut hasher);
            }
            let hash = super::joinspill::mix(hasher.finish() ^ seed);
            parts[(hash >> 60) as usize % PARTITIONS].push(row);
        }
        if self.writers.is_empty() {
            self.writers.resize_with(PARTITIONS, || None);
        }
        for (part, rows) in parts.iter().enumerate() {
            if rows.is_empty() {
                continue;
            }
            let columns: Vec<Vec<Id>> = table
                .cols
                .iter()
                .map(|column| rows.iter().map(|&row| column[row]).collect())
                .collect();
            let writer = &mut self.writers[part];
            if writer.is_none() {
                *writer = Some(query.create(SpillKind::GroupPartition, table.cols.len())?);
            }
            let slices: Vec<&[Id]> = columns.iter().map(Vec::as_slice).collect();
            writer
                .as_mut()
                .expect("an open spill file")
                .write(&slices, rows.len())?;
        }
        Ok(())
    }

    /// Forget the groups of a finished pass, keeping the aggregates' configuration.
    fn reset(&mut self) -> Result<()> {
        self.index = if self.keys.len() == 1 {
            Index::One(Default::default())
        } else {
            Index::Many(Default::default())
        };
        self.order = Vec::new();
        self.states = Vec::new();
        self.payload = Vec::new();
        self.groups = 0;
        self.at = 0;
        self.seen = self
            .aggs
            .iter()
            .any(|agg| agg.distinct && agg.expr.is_some())
            .then(Default::default);
        self.rows_seen = self
            .aggs
            .iter()
            .any(|agg| agg.distinct && agg.expr.is_none())
            .then(Default::default);
        self.loaded = false;
        self.frozen = false;
        self.bytes = self.base;
        self.reserved = self.base;
        if let Some(charge) = &mut self.charge {
            charge.resize(self.base)?;
        }
        Ok(())
    }

    fn only_counts_rows(&self) -> bool {
        self.keys.is_empty()
            && self.args.iter().all(|arg| matches!(arg, Arg::Star))
            && self.rows_seen.is_none()
    }

    /// Whether `row` of `table` is new to COUNT(DISTINCT *) aggregate `a` of `group`,
    /// remembering it under a charge when it is. A row is all of its input's values.
    fn new_row(&mut self, group: usize, a: usize, table: &Table, row: usize) -> Result<bool> {
        let key: Box<[Id]> = table.cols.iter().map(|c| c[row]).collect();
        let entry = (group as u32, a as u32, key);
        let seen = self.rows_seen.as_ref().expect("a DISTINCT row set");
        if seen.contains(&entry) {
            return Ok(false);
        }
        // The boxed row, the set's entry and room for its table to grow.
        self.retain(table.cols.len() as u64 * 8 + 96)?;
        let seen = self.rows_seen.as_mut().expect("a DISTINCT row set");
        seen.insert(entry);
        Ok(true)
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
        if self.query.is_some() && self.threshold == 0 {
            self.threshold = (ctx.memory_room() / 2).max(MIN_PASS_BYTES);
        }
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
            let table = &table;
            if self.only_counts_rows() {
                for state in &mut self.states {
                    if let State::Agg(AggState::Count(count)) = state {
                        *count += table.len as u64;
                    }
                }
                continue;
            }
            // The group of every row of the batch, then each aggregate over the whole
            // batch, so that the kind of an aggregate is matched once per batch.
            let _slots = ctx.charge(table.len as u64 * 4 + 64)?;
            let mut slots: Vec<u32> = Vec::with_capacity(table.len);
            if self.keys.is_empty() {
                slots.resize(table.len, 0);
            } else if let ([Some(column)], Index::One(_)) = (self.keys.as_slice(), &self.index) {
                let column = &table.cols[*column];
                for (row, &id) in column.iter().enumerate() {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let found = match &self.index {
                        Index::One(index) => index.get(&id).copied(),
                        Index::Many(_) => unreachable!("a one-key index"),
                    };
                    slots.push(match found {
                        Some(group) => group as u32,
                        None => self.place(ctx, &[id])?,
                    });
                }
            } else {
                for row in 0..table.len {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    for (k, column) in key.iter_mut().zip(&self.keys) {
                        *k = column.map_or(Id::UNDEF, |c| table.cols[c][row]);
                    }
                    slots.push(self.find(ctx, &key)?);
                }
            }
            // Rows of groups this pass does not hold go to the next pass.
            let passed = if self.frozen {
                slots.iter().filter(|&&slot| slot == PASSED).count()
            } else {
                0
            };
            let kept;
            let table = if passed > 0 {
                self.pass_on(ctx, table, &slots, passed)?;
                let _kept =
                    ctx.charge((table.len - passed) as u64 * table.cols.len() as u64 * 8 + 256)?;
                let mut rows = Table::new(table.vars.clone());
                for (to, from) in rows.cols.iter_mut().zip(&table.cols) {
                    to.extend(
                        from.iter()
                            .zip(&slots)
                            .filter(|&(_, &slot)| slot != PASSED)
                            .map(|(&id, _)| id),
                    );
                }
                rows.len = table.len - passed;
                slots.retain(|&slot| slot != PASSED);
                kept = (rows, _kept);
                &kept.0
            } else {
                table
            };
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
            for (a, values) in computed.iter().enumerate() {
                ctx.check()?;
                let na = self.aggs.len();
                let agg = &self.aggs[a];
                let column = match (&self.args[a], values) {
                    (_, Some(values)) => Some(values.as_slice()),
                    (Arg::Column(Some(c)), _) => Some(table.cols[*c].as_slice()),
                    _ => None,
                };
                let unbound = matches!(self.args[a], Arg::Column(None));
                if !agg.distinct && !matches!(agg.func, AggregateFunction::GroupConcat { .. }) {
                    // Running states that need no payload: counts and sums.
                    match (&agg.func, column) {
                        (AggregateFunction::Count, None) if !unbound => {
                            for &slot in &slots {
                                if let State::Agg(AggState::Count(n)) =
                                    &mut self.states[slot as usize * na + a]
                                {
                                    *n += 1;
                                }
                            }
                            continue;
                        }
                        (AggregateFunction::Count, Some(values)) => {
                            for (&slot, id) in slots.iter().zip(values) {
                                if let State::Agg(AggState::Count(n)) =
                                    &mut self.states[slot as usize * na + a]
                                {
                                    *n += u64::from(!id.is_undef());
                                }
                            }
                            continue;
                        }
                        (AggregateFunction::Sum | AggregateFunction::Avg, Some(values)) => {
                            for (&slot, &id) in slots.iter().zip(values) {
                                if let State::Agg(state) = &mut self.states[slot as usize * na + a]
                                {
                                    state.add(ctx, agg, Some(id));
                                }
                            }
                            continue;
                        }
                        _ => {}
                    }
                }
                for (row, &slot) in slots.iter().enumerate() {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let group = slot as usize;
                    if matches!(self.args[a], Arg::Star)
                        && self.aggs[a].distinct
                        && !self.new_row(group, a, table, row)?
                    {
                        continue;
                    }
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
        for writer in std::mem::take(&mut self.writers).into_iter().flatten() {
            let run = writer.finish()?;
            self.spilled = self.spilled.saturating_add(run.bytes());
            self.pending.push((Arc::new(run), self.depth + 1));
        }
        self.source = None;
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
        let n = loop {
            if !self.loaded {
                self.load(ctx, child, options)?;
                // The value sets are not needed for output.
                self.seen = None;
                self.rows_seen = None;
            }
            let n = cap.min(self.groups - self.at);
            if n > 0 {
                break n;
            }
            // The pass is output: free it and read the rows it passed on.
            let Some((run, depth)) = self.pending.pop() else {
                return Ok(None);
            };
            self.reset()?;
            self.depth = depth;
            self.source = Some(run.reader()?);
            self.passes += 1;
        };
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
