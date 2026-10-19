//! Build one input, then resume compatible probe rows without materializing the
//! join's output or a Cartesian vector of matching row pairs.
//!
//! The build rows are grouped by key into one flat array, so that a key's rows are one
//! slice in build order. A probe batch joins in two passes: the first finds the pairs of
//! probe and build rows, up to the output's room, and the second gathers each output
//! column from the pairs. An OPTIONAL condition is evaluated once over the gathered
//! candidate rows of a chunk, as eager left joins do.
//!
//! A query that may spill partitions both inputs to disk once the build input outgrows
//! its share of memory, and joins the partitions one pair at a time ([`super::joinspill`],
//! X08 §6.3).
use super::joinspill::JoinSpill;
use super::{Buffer, CursorOptions, Operator, merge};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::exec;
use crate::sparql::expr::Expr;
use crate::sparql::plan::{JoinAlgo, Kind, Node};
use crate::sparql::table::{Table, VarId};
use rustc_hash::FxHashMap;
use std::sync::Arc;

pub(super) fn eligible(node: &Node) -> bool {
    node.children.len() == 2
        && matches!(
            node.kind,
            Kind::Join { .. } | Kind::HalfJoin { .. } | Kind::Minus | Kind::LeftJoin { .. }
        )
}

/// The input that is streamed. It matches the planner's hash probe order: the larger
/// input of an inner hash join, and child zero of every other join.
pub(super) fn probe_side(node: &Node) -> usize {
    usize::from(
        matches!(
            node.kind,
            Kind::Join {
                algo: JoinAlgo::Hash,
                ..
            }
        ) && node.children[0].est < node.children[1].est,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Inner,
    Optional,
    Semi,
    Anti,
    Minus,
}

pub(super) struct Binary {
    pub(super) mode: Mode,
    pub(super) probe_side: usize,
    pub(super) columns: Vec<(Option<usize>, Option<usize>)>,
    pub(super) shared: Vec<(usize, usize)>,
    /// Probe columns in order of preference for the hash key: certain on the probe
    /// side first. The build data decides which one is used.
    pub(super) preferred: Vec<(usize, usize)>,
    pub(super) index: Option<HashIndex>,
    pub(super) build: Option<Buffer>,
    pub(super) probe: Option<Buffer>,
    /// The probe row being joined.
    pub(super) row: usize,
    /// How many of the row's candidates have been visited.
    pub(super) offset: usize,
    /// Whether the row has met a matching build row so far.
    pub(super) matched: bool,
    pub(super) expression: Option<Expr>,
    /// Whether a semi, anti or MINUS join lists the probe rows that a build row
    /// matches, whatever the mode keeps, because the build input is joined in several
    /// chunks and a probe row's verdict waits for the last one.
    pub(super) deferred: bool,
    /// Whether the output must keep the probe input's order if the join spills,
    /// because an operator above relies on it.
    pub(super) ordered: bool,
    /// The variables of the probe and build inputs.
    pub(super) probe_vars: Vec<VarId>,
    pub(super) build_vars: Vec<VarId>,
    /// The planner's estimate of the build input's rows.
    pub(super) build_est: f64,
    /// The partitioned join, once the build input has spilled.
    pub(super) spill: Option<Box<JoinSpill>>,
    _charge: Option<RetainedCharge>,
}

/// The build row of a pair that has none: the OPTIONAL extension of a probe row.
pub(super) const NO_ROW: u32 = u32::MAX;

/// Build rows grouped on one shared column. A bound key's rows are one slice of `rows`,
/// in build order. Rows that leave the key unbound are compatible with every probe
/// value, so every bound probe also visits them, and an unbound probe visits every
/// build row.
pub(super) struct HashIndex {
    /// The probe column that carries the key.
    probe: usize,
    /// Where each key's rows start in `rows`, and how many there are.
    slots: FxHashMap<Id, (u32, u32)>,
    rows: Vec<u32>,
    unbound: Vec<u32>,
    _charge: Option<RetainedCharge>,
}

/// The bytes a hash map of `capacity` entries of `entry` bytes allocates.
fn map_bytes(capacity: usize, entry: u64) -> u64 {
    let buckets = (capacity.saturating_mul(8) / 7).next_power_of_two() as u64;
    buckets.saturating_mul(entry + 1)
}

impl HashIndex {
    /// Index `build` on its column `column`. Row numbers are 32 bits wide, and the
    /// grouped rows, the unbound rows and the key table are charged before they grow.
    pub(super) fn new(ctx: &Ctx, build: &Table, column: usize, probe: usize) -> Result<Self> {
        let rows = u32::try_from(build.len)
            .ok()
            .filter(|&n| n < NO_ROW)
            .ok_or_else(|| Error::invalid("a hash join build input has too many rows"))?;
        let n = rows as usize;
        // The slot of every row and the grouped rows coexist with a key table that may
        // hold a key per row, and with its old buckets while it grows.
        let mut charge =
            ctx.retained_charge((n as u64) * 12 + map_bytes(n, 16).saturating_mul(3) / 2 + 1024)?;
        let keys = &build.cols[column];
        // First each key's slot and its count of rows, then where its rows start.
        // Sized for a key per row, so that it never grows while it fills.
        let mut slots: FxHashMap<Id, (u32, u32)> =
            FxHashMap::with_capacity_and_hasher(n, Default::default());
        let mut slot_of: Vec<u32> = Vec::with_capacity(n);
        let mut counts: Vec<u32> = Vec::new();
        for (row, &key) in keys.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            if key.is_undef() {
                slot_of.push(NO_ROW);
                continue;
            }
            let next = counts.len() as u32;
            let slot = slots.entry(key).or_insert((next, 0)).0;
            if slot == next {
                counts.push(0);
            }
            counts[slot as usize] += 1;
            slot_of.push(slot);
        }
        let mut at = 0u32;
        let mut fill = Vec::with_capacity(counts.len());
        for &count in &counts {
            fill.push(at);
            at += count;
        }
        let mut grouped = vec![0u32; at as usize];
        let mut unbound = Vec::with_capacity(n - at as usize);
        for (row, &slot) in slot_of.iter().enumerate() {
            if slot == NO_ROW {
                unbound.push(row as u32);
            } else {
                let next = &mut fill[slot as usize];
                grouped[*next as usize] = row as u32;
                *next += 1;
            }
        }
        // A key's entry holds where its rows start and how many there are, so that a
        // probe finds them with one lookup.
        for (slot, range) in slots.values_mut() {
            let count = counts[*slot as usize];
            *range = count;
            *slot = fill[*slot as usize] - count;
        }
        drop((slot_of, counts, fill));
        if let Some(charge) = &mut charge {
            charge.resize(
                (grouped.capacity() + unbound.capacity()) as u64 * 4
                    + map_bytes(slots.capacity(), 16)
                    + 1024,
            )?;
        }
        Ok(Self {
            probe,
            slots,
            rows: grouped,
            unbound,
            _charge: charge,
        })
    }

    /// The build rows that hold `key`.
    #[inline]
    fn keyed(&self, key: Id) -> &[u32] {
        match self.slots.get(&key) {
            Some(&(start, count)) => &self.rows[start as usize..(start + count) as usize],
            None => &[],
        }
    }
}

/// The build rows a probe row may match: every build row, or the rows of its key and
/// then the rows that leave the key unbound.
#[derive(Clone, Copy)]
enum Candidates<'a> {
    All(usize),
    Keyed(&'a [u32], &'a [u32]),
}

impl Candidates<'_> {
    #[inline]
    fn len(&self) -> usize {
        match self {
            Candidates::All(n) => *n,
            Candidates::Keyed(keyed, unbound) => keyed.len() + unbound.len(),
        }
    }

    #[inline]
    fn get(&self, k: usize) -> u32 {
        match self {
            Candidates::All(_) => k as u32,
            Candidates::Keyed(keyed, unbound) => match keyed.get(k) {
                Some(&row) => row,
                None => unbound[k - keyed.len()],
            },
        }
    }
}

impl Binary {
    /// `ordered` tells that an operator above relies on the probe input's order, which
    /// a spilled join then restores.
    pub(super) fn new(ctx: &Ctx, node: &Node, ordered: bool) -> Result<Self> {
        let mode = match &node.kind {
            Kind::LeftJoin { .. } => Mode::Optional,
            Kind::HalfJoin { anti: false } => Mode::Semi,
            Kind::HalfJoin { anti: true } => Mode::Anti,
            Kind::Minus => Mode::Minus,
            _ => Mode::Inner,
        };
        let probe_side = probe_side(node);
        let (probe, build) = (&node.children[probe_side], &node.children[1 - probe_side]);
        let charge = ctx.retained_charge(node.vars.len() as u64 * 96 + 1024)?;
        let columns = node
            .vars
            .iter()
            .map(|v| {
                (
                    probe.vars.iter().position(|x| x == v),
                    build.vars.iter().position(|x| x == v),
                )
            })
            .collect();
        let shared: Vec<_> = probe
            .vars
            .iter()
            .enumerate()
            .filter_map(|(p, v)| build.vars.iter().position(|x| x == v).map(|b| (p, b)))
            .collect();
        let mut preferred = shared.clone();
        preferred.sort_by_key(|&(p, _)| !probe.certain.contains(&probe.vars[p]));
        let expression = match &node.kind {
            Kind::LeftJoin { expr } => expr.clone(),
            _ => None,
        };
        Ok(Self {
            mode,
            probe_side,
            columns,
            shared,
            preferred,
            index: None,
            build: None,
            probe: None,
            row: 0,
            offset: 0,
            matched: false,
            expression,
            deferred: false,
            ordered,
            probe_vars: probe.vars.clone(),
            build_vars: build.vars.clone(),
            build_est: build.est,
            spill: None,
            _charge: charge,
        })
    }

    fn ensure_probe(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        cap: usize,
    ) -> Result<bool> {
        if self.probe.as_ref().is_some_and(|b| self.row < b.table.len) {
            return Ok(true);
        }
        self.probe = None;
        self.advance();
        self.row = 0;
        self.probe = children[self.probe_side].next(ctx, options, cap)?;
        Ok(self.probe.is_some())
    }

    fn build(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
    ) -> Result<()> {
        let child = &mut children[1 - self.probe_side];
        let mut build = Buffer::new(ctx, &child.vars, 0)?;
        // A query that may spill keeps the build input to half the room it had, so that
        // the probe batches and the output still fit when the join must partition.
        let threshold = ctx
            .spill
            .is_some()
            .then(|| (ctx.memory_room() / 2).max(super::joinspill::MIN_BUDGET));
        while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
            let n = batch.table.len;
            // The next batch, and what the input's operators hold to make it.
            let next_batch = super::capacity_bytes(&batch.table).saturating_mul(4);
            let grown = threshold.filter(|&threshold| {
                build
                    .charge
                    .bytes()
                    .saturating_add(super::capacity_bytes(&batch.table))
                    > threshold
                    && build.table.len > 0
            });
            let appended = match grown {
                Some(threshold) => Err(ctx.memory_exceeded(threshold)),
                None => merge::append(ctx, &mut build, &batch.table, 0..n),
            };
            match appended {
                // The build also spills when what else the query holds leaves less than a
                // quarter of the room it had, or less than a few of the input's batches, so
                // that the input can still give its next batch.
                Ok(()) => {
                    if let Some(threshold) = threshold
                        && ctx.memory_room() < (threshold / 2).max(next_batch)
                    {
                        let error = ctx.memory_exceeded(threshold);
                        return self.start_spill(ctx, children, options, build, None, error);
                    }
                }
                Err(error) if spills(ctx, &error) => {
                    return self.start_spill(ctx, children, options, build, Some(batch), error);
                }
                Err(error) => return Err(error),
            }
        }
        // Key on the shared column that the build data leaves unbound least often.
        // A column that is unbound in every build row selects nothing.
        let Some((p, b)) = self.key_column(ctx, &build.table)? else {
            self.build = Some(build);
            return Ok(());
        };
        if build.table.len > 1 {
            match HashIndex::new(ctx, &build.table, b, p) {
                Ok(index) => self.index = Some(index),
                Err(error) if spills(ctx, &error) => {
                    return self.start_spill(ctx, children, options, build, None, error);
                }
                Err(error) => return Err(error),
            }
        }
        self.build = Some(build);
        Ok(())
    }

    /// The shared column to key on: the one that `table`, a build table, leaves unbound
    /// least often, as (probe column, build column), unless every row leaves each
    /// shared column unbound.
    pub(super) fn key_column(&self, ctx: &Ctx, table: &Table) -> Result<Option<(usize, usize)>> {
        let unbound = |column: usize| table.cols[column].iter().filter(|x| x.is_undef()).count();
        let mut best: Option<((usize, usize), usize)> = None;
        for &(p, b) in &self.preferred {
            ctx.check()?;
            let n = unbound(b);
            if n < table.len && best.is_none_or(|(_, m)| n < m) {
                best = Some(((p, b), n));
            }
            if n == 0 {
                break;
            }
        }
        Ok(best.map(|(key, _)| key))
    }

    /// Partition the build input to disk: the rows read so far in `build`, the batch
    /// `pending` that did not fit, and the rest of the input. A join without a key to
    /// partition on fails with `error`, the budget error that started the spill.
    fn start_spill(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        build: Buffer,
        pending: Option<Buffer>,
        error: Error,
    ) -> Result<()> {
        self.index = None;
        let Some(key) = self.key_column(ctx, &build.table)? else {
            return Err(error);
        };
        let child = &mut children[1 - self.probe_side];
        let spill = JoinSpill::start(ctx, self, key, build, pending, child, options, error)?;
        self.spill = Some(Box::new(spill));
        Ok(())
    }

    fn advance(&mut self) {
        self.row += 1;
        self.offset = 0;
        self.matched = false;
    }

    /// Whether a probe row's candidates need a test of the shared variables: when
    /// another shared variable than the key may disagree, or when there is no key.
    fn checks(&self) -> bool {
        !self.shared.is_empty() && (self.shared.len() > 1 || self.index.is_none())
    }

    /// Whether probe row `p` and build row `b` agree on their shared variables, and for
    /// MINUS, whether they also bind a shared variable both.
    #[inline]
    fn compatible(&self, probe: &Table, p: usize, build: &Table, b: usize) -> bool {
        let agree = self.shared.iter().all(|&(pc, bc)| {
            let (x, y) = (probe.cols[pc][p], build.cols[bc][b]);
            x == y || x.is_undef() || y.is_undef()
        });
        agree
            && (self.mode != Mode::Minus
                || self.shared.iter().any(|&(pc, bc)| {
                    !probe.cols[pc][p].is_undef() && !build.cols[bc][b].is_undef()
                }))
    }

    fn candidates<'a>(
        index: Option<&'a HashIndex>,
        probe: &Table,
        row: usize,
        all: usize,
    ) -> Candidates<'a> {
        match index {
            Some(index) => {
                let key = probe.cols[index.probe][row];
                if key.is_undef() {
                    Candidates::All(all)
                } else {
                    Candidates::Keyed(index.keyed(key), &index.unbound)
                }
            }
            None => Candidates::All(all),
        }
    }

    /// Inner and OPTIONAL joins without a condition: the pairs of the current probe
    /// batch from the current row, at most `room` of them. An unmatched OPTIONAL row
    /// pairs with [`NO_ROW`].
    pub(super) fn pairs(
        &mut self,
        ctx: &Ctx,
        room: usize,
        lp: &mut Vec<u32>,
        rp: &mut Vec<u32>,
    ) -> Result<()> {
        let probe = &self.probe.as_ref().expect("available probe").table;
        let build = &self.build.as_ref().expect("captured build").table;
        let optional = self.mode == Mode::Optional;
        let checks = self.checks();
        if !checks
            && self.offset == 0
            && let Some(index) = &self.index
            && index.unbound.is_empty()
        {
            // Every build row binds the key, so a bound probe row matches exactly the rows
            // of its key. Whole rows go into the pairs while they fit, written in place.
            let keys = &probe.cols[index.probe];
            let mut w = lp.len();
            lp.resize(room, 0);
            rp.resize(room, 0);
            let mut row = self.row;
            while row < probe.len {
                if row.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let key = keys[row];
                if key.is_undef() {
                    break;
                }
                let keyed = index.keyed(key);
                if keyed.is_empty() {
                    if optional {
                        if w == room {
                            break;
                        }
                        lp[w] = row as u32;
                        rp[w] = NO_ROW;
                        w += 1;
                    }
                } else {
                    if keyed.len() > room - w {
                        break;
                    }
                    for (i, &other) in keyed.iter().enumerate() {
                        lp[w + i] = row as u32;
                        rp[w + i] = other;
                    }
                    w += keyed.len();
                }
                row += 1;
            }
            lp.truncate(w);
            rp.truncate(w);
            self.row = row;
        }
        let mut steps = 0usize;
        while self.row < probe.len && lp.len() < room {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            let row = self.row;
            let candidates = Self::candidates(self.index.as_ref(), probe, row, build.len);
            let n = candidates.len();
            if !checks && self.offset == 0 && n <= room - lp.len() {
                // Every candidate matches and the whole row fits.
                match candidates {
                    Candidates::Keyed(keyed, unbound) => {
                        // Most keys have a row or two, which a copy call costs more for.
                        for &other in keyed.iter().chain(unbound) {
                            rp.push(other);
                        }
                    }
                    Candidates::All(all) => rp.extend(0..all as u32),
                }
                lp.extend(std::iter::repeat_n(row as u32, n));
                if n == 0 && optional {
                    if lp.len() == room {
                        break;
                    }
                    lp.push(row as u32);
                    rp.push(NO_ROW);
                }
                self.row += 1;
                self.offset = 0;
                self.matched = false;
                continue;
            }
            while self.offset < n && lp.len() < room {
                if self.offset.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let other = candidates.get(self.offset);
                self.offset += 1;
                if checks && !self.compatible(probe, row, build, other as usize) {
                    continue;
                }
                self.matched = true;
                lp.push(row as u32);
                rp.push(other);
            }
            if self.offset < n {
                break;
            }
            if optional && !self.matched {
                if lp.len() == room {
                    break;
                }
                lp.push(row as u32);
                rp.push(NO_ROW);
            }
            self.row += 1;
            self.offset = 0;
            self.matched = false;
        }
        Ok(())
    }

    /// OPTIONAL with a condition: collect the compatible pairs of a chunk of probe rows,
    /// test the condition on their merged rows at once, then keep the pairs that pass and
    /// extend each finished row that none of its pairs passed. A row's output is at most
    /// the larger of its visited candidates and one, which bounds the chunk by `room`.
    pub(super) fn condition(
        &mut self,
        ctx: &Ctx,
        vars: &[VarId],
        room: usize,
        lp: &mut Vec<u32>,
        rp: &mut Vec<u32>,
    ) -> Result<()> {
        let probe = &self.probe.as_ref().expect("available probe").table;
        let build = &self.build.as_ref().expect("captured build").table;
        let checks = self.checks();
        // The candidate pairs, the rows of the chunk and the merged candidate rows.
        let _scratch = ctx.charge(room as u64 * (vars.len() as u64 * 8 + 24) + 256)?;
        // The rows of the chunk: the row, its first pair, and whether all its candidates
        // were visited.
        let mut rows: Vec<(u32, u32, bool)> = Vec::new();
        let (mut cl, mut cr): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        let carried = self.matched;
        let first_row = self.row;
        let mut used = 0usize;
        let mut steps = 0usize;
        'rows: while self.row < probe.len && used < room {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            let row = self.row;
            used += 1;
            let candidates = Self::candidates(self.index.as_ref(), probe, row, build.len);
            let n = candidates.len();
            let begin = cl.len() as u32;
            let mut visited = 0usize;
            while self.offset < n {
                if self.offset.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let other = candidates.get(self.offset);
                if checks && !self.compatible(probe, row, build, other as usize) {
                    self.offset += 1;
                    continue;
                }
                if visited >= 1 {
                    if used >= room {
                        rows.push((row as u32, begin, false));
                        break 'rows;
                    }
                    used += 1;
                }
                visited += 1;
                self.offset += 1;
                cl.push(row as u32);
                cr.push(other);
            }
            rows.push((row as u32, begin, true));
            self.row += 1;
            self.offset = 0;
        }
        // The merged candidate rows, and which of them pass the condition.
        let mut merged = Table::new(vars.to_vec());
        gather(&mut merged, &self.columns, probe, build, &cl, &cr);
        let expression = self.expression.as_ref().expect("an OPTIONAL condition");
        let pass = exec::filter_mask(ctx, &merged, std::slice::from_ref(expression))?;
        drop(merged);
        for (i, &(row, begin, complete)) in rows.iter().enumerate() {
            let end = rows.get(i + 1).map_or(cl.len() as u32, |next| next.1);
            let mut matched = carried && row as usize == first_row;
            for k in begin..end {
                if pass[k as usize] {
                    matched = true;
                    lp.push(row);
                    rp.push(cr[k as usize]);
                }
            }
            if complete {
                if !matched {
                    lp.push(row);
                    rp.push(NO_ROW);
                }
                self.matched = false;
            } else {
                // The row continues in the next chunk.
                self.matched = matched;
            }
        }
        Ok(())
    }

    /// Semi, anti and MINUS joins: the probe rows that pass, at most `room` of them. A
    /// row passes a semi join when a build row matches it, and the others when none does.
    /// A deferred join lists the rows that a build row matches.
    pub(super) fn filter(&mut self, ctx: &Ctx, room: usize, lp: &mut Vec<u32>) -> Result<()> {
        let probe = &self.probe.as_ref().expect("available probe").table;
        let build = &self.build.as_ref().expect("captured build").table;
        let keep_matched = self.mode == Mode::Semi || self.deferred;
        let tests = self.checks() || self.mode == Mode::Minus;
        let simple = !tests
            || (self.mode == Mode::Minus
                && self.shared.len() == 1
                && self.index.as_ref().is_some_and(|i| i.unbound.is_empty()));
        let mut steps = 0usize;
        while self.row < probe.len && lp.len() < room {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            let row = self.row;
            let candidates = Self::candidates(self.index.as_ref(), probe, row, build.len);
            let found = match candidates {
                // Every keyed build row binds the key, so it matches a bound probe key
                // and, for MINUS, shares that bound variable.
                Candidates::Keyed(keyed, unbound) if simple => {
                    !keyed.is_empty() || (!tests && !unbound.is_empty())
                }
                _ if !tests => candidates.len() > 0,
                _ => (0..candidates.len())
                    .any(|k| self.compatible(probe, row, build, candidates.get(k) as usize)),
            };
            if found == keep_matched {
                lp.push(row as u32);
            }
            self.row += 1;
            self.offset = 0;
            self.matched = false;
        }
        Ok(())
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if let Some(mut spill) = self.spill.take() {
            let batch = spill.next(self, ctx, children, options, vars, cap);
            self.spill = Some(spill);
            return batch;
        }
        if !self.ensure_probe(ctx, children, options, cap)? {
            return Ok(None);
        }
        if self.build.is_none() && self.spill.is_none() {
            self.build(ctx, children, options)?;
            if self.spill.is_some() {
                return self.next(ctx, children, options, vars, cap);
            }
        }
        let mut output = Buffer::new(ctx, vars, cap)?;
        let _pairs = ctx.charge(cap as u64 * 8 + ctx.nvars() as u64 * 16 + 256)?;
        let mut lp: Vec<u32> = Vec::with_capacity(cap);
        let mut rp: Vec<u32> = Vec::with_capacity(cap);
        while output.table.len < cap {
            ctx.check()?;
            if !self.ensure_probe(ctx, children, options, cap)? {
                break;
            }
            let room = cap - output.table.len;
            lp.clear();
            rp.clear();
            match self.mode {
                Mode::Semi | Mode::Anti | Mode::Minus => self.filter(ctx, room, &mut lp)?,
                Mode::Optional if self.expression.is_some() => {
                    self.condition(ctx, vars, room, &mut lp, &mut rp)?
                }
                Mode::Inner | Mode::Optional => self.pairs(ctx, room, &mut lp, &mut rp)?,
            }
            let probe = &self.probe.as_ref().expect("available probe").table;
            let build = &self.build.as_ref().expect("captured build").table;
            if rp.is_empty() && !lp.is_empty() {
                // Rows of the probe input alone.
                rp.resize(lp.len(), NO_ROW);
            }
            gather(&mut output.table, &self.columns, probe, build, &lp, &rp);
        }
        output.reconcile()?;
        Ok((!output.table.is_empty()).then_some(output))
    }
}

/// Whether `error` is a memory budget that a query with spill answers by spilling.
pub(super) fn spills(ctx: &Ctx, error: &Error) -> bool {
    ctx.spill.is_some()
        && matches!(
            error,
            Error::BudgetExceeded(b)
                if matches!(b.kind, crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory)
        )
}

/// Append the rows of the pairs `lp` and `rp` to `out`, one column at a time. A probe
/// value that is unbound takes the build row's value, and [`NO_ROW`] binds none.
pub(super) fn gather(
    out: &mut Table,
    columns: &[(Option<usize>, Option<usize>)],
    probe: &Table,
    build: &Table,
    lp: &[u32],
    rp: &[u32],
) {
    for (column, &(p, b)) in out.cols.iter_mut().zip(columns) {
        match (p, b) {
            (Some(p), Some(b)) => {
                let (pc, bc) = (&probe.cols[p], &build.cols[b]);
                column.extend(lp.iter().zip(rp).map(|(&l, &r)| {
                    let value = pc[l as usize];
                    if value.is_undef() && r != NO_ROW {
                        bc[r as usize]
                    } else {
                        value
                    }
                }));
            }
            (Some(p), None) => {
                let pc = &probe.cols[p];
                column.extend(lp.iter().map(|&l| pc[l as usize]));
            }
            (None, Some(b)) => {
                let bc = &build.cols[b];
                column.extend(rp.iter().map(|&r| {
                    if r == NO_ROW {
                        Id::UNDEF
                    } else {
                        bc[r as usize]
                    }
                }));
            }
            (None, None) => column.extend(std::iter::repeat_n(Id::UNDEF, lp.len())),
        }
    }
    out.len += lp.len();
}
