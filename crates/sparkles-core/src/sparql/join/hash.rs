//! The hash join kernel: one flat table of the build rows grouped by key, and one probe
//! loop that takes a probe batch and an output room and returns pairs of rows.
//!
//! The build rows are grouped by key into one flat array, so that a key's rows are one
//! slice in build order. A build side of [`PART_MIN`] rows or more is split by bits of a
//! hash of the key into partitions of about [`PART_ROWS`] rows, whose tables stay in the
//! CPU caches while they are built, in parallel. A probe joins in two passes: the first
//! finds the pairs of probe and build rows, up to the output's room, and the second
//! gathers each output column from the pairs. An OPTIONAL condition is evaluated once
//! over the gathered candidate rows of a chunk.
//!
//! The kernel has the five join modes and knows nothing about who runs it. The eager
//! executor probes its whole input in parallel pieces, and the cursor probes the
//! batches it pulls and resumes where the room ran out. A probe row whose key is unbound
//! visits every build row, and build rows that leave the key unbound are visited by
//! every probe row, which is the UNDEF-compatible nested loop for the keys the hash
//! cannot take.
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::eqkey::{EqKey, eq_key};
use crate::sparql::exec;
use crate::sparql::expr::Expr;
use crate::sparql::table::{Table, VarId};
use rayon::prelude::*;
use rustc_hash::FxHashMap;

/// The build row of a pair that has none: the OPTIONAL extension of a probe row.
pub(crate) const NO_ROW: u32 = u32::MAX;

/// What a join keeps of the pairs its probe finds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Inner,
    Optional,
    Semi,
    Anti,
    Minus,
}

/// Rows per partition of a hash join's build side: a partition's table of about 24 bytes
/// per row fits in the per-core cache.
const PART_ROWS: usize = 8192;

/// Build sides with fewer rows have one partition, built on one thread: their table fits
/// in the caches anyway, and handing pieces to other threads costs more than it saves.
const PART_MIN: usize = 1 << 16;

/// Probe rows per parallel piece of an eager join, and the pairs one call of the probe
/// loop returns to it before the output is checked against the budget.
pub(crate) const PROBE_PIECE: usize = 1 << 15;

/// Build rows grouped on one shared column. A bound key's rows are one slice of its
/// partition's `rows`, in build order. Rows that leave the key unbound are compatible
/// with every probe value, so every bound probe also visits them, and an unbound probe
/// visits every build row.
pub(crate) struct HashIndex {
    /// The probe column that carries the key.
    probe: usize,
    /// The partition of a key is its mixed hash shifted right by this (64: one).
    shift: u32,
    parts: Vec<Part>,
    unbound: Vec<u32>,
    _charge: Option<RetainedCharge>,
}

struct Part {
    /// Where each key's rows start in `rows`, and how many there are.
    slots: FxHashMap<Id, (u32, u32)>,
    rows: Vec<u32>,
}

/// The bytes a hash map of `capacity` entries of `entry` bytes allocates.
pub(crate) fn map_bytes(capacity: usize, entry: u64) -> u64 {
    let buckets = (capacity.saturating_mul(8) / 7).next_power_of_two() as u64;
    buckets.saturating_mul(entry + 1)
}

/// A hash of an id for choosing its partition, unlike the hash tables' own hash (whose
/// high bits pick the slots' tags).
#[inline]
fn mix(k: u64) -> u64 {
    (k ^ (k >> 29)).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

impl HashIndex {
    /// Index `build` on its column `column`, which probe column `probe` is compared
    /// with. Row numbers are 32 bits wide, and the grouped rows, the unbound rows and
    /// the key tables are charged before they grow.
    pub(crate) fn new(ctx: &Ctx, build: &Table, column: usize, probe: usize) -> Result<Self> {
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
        let bits = if n < PART_MIN {
            0
        } else {
            (n / PART_ROWS).next_power_of_two().trailing_zeros().min(12)
        };
        let shift = 64 - bits;
        let mut unbound = Vec::new();
        let parts = if bits == 0 {
            let mut bound = Vec::with_capacity(n);
            for (row, key) in keys.iter().enumerate() {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                if key.is_undef() {
                    unbound.push(row as u32);
                } else {
                    bound.push(row as u32);
                }
            }
            vec![Part::build(keys, &bound)]
        } else {
            // The bound rows of each partition, in row order (a counting sort on the
            // partition).
            let part_of = |k: Id| (mix(k.0) >> shift) as usize;
            let mut count = vec![0u32; 1 << bits];
            for (row, key) in keys.iter().enumerate() {
                if row.is_multiple_of(4096) {
                    ctx.check()?;
                }
                if key.is_undef() {
                    unbound.push(row as u32);
                } else {
                    count[part_of(*key)] += 1;
                }
            }
            let mut at: Vec<u32> = Vec::with_capacity(count.len() + 1);
            let mut sum = 0u32;
            at.push(0);
            for c in &count {
                sum += c;
                at.push(sum);
            }
            let mut next = at.clone();
            let mut order = vec![0u32; sum as usize];
            for (row, key) in keys.iter().enumerate() {
                if key.is_undef() {
                    continue;
                }
                let p = &mut next[part_of(*key)];
                order[*p as usize] = row as u32;
                *p += 1;
            }
            ctx.check()?;
            (0..count.len())
                .into_par_iter()
                .map(|p| Part::build(keys, &order[at[p] as usize..at[p + 1] as usize]))
                .collect()
        };
        unbound.shrink_to_fit();
        if let Some(charge) = &mut charge {
            let held: u64 = parts
                .iter()
                .map(|part| part.rows.capacity() as u64 * 4 + map_bytes(part.slots.capacity(), 16))
                .sum();
            charge.resize(held + unbound.capacity() as u64 * 4 + 1024)?;
        }
        Ok(Self {
            probe,
            shift,
            parts,
            unbound,
            _charge: charge,
        })
    }

    /// The build rows that hold `key`, in build order (none for a key not in the table).
    #[inline]
    pub(crate) fn keyed(&self, key: Id) -> &[u32] {
        let part = if self.shift == 64 {
            &self.parts[0]
        } else {
            &self.parts[(mix(key.0) >> self.shift) as usize]
        };
        match part.slots.get(&key) {
            Some(&(start, count)) => &part.rows[start as usize..(start + count) as usize],
            None => &[],
        }
    }

    /// The build rows that leave the key unbound, in build order.
    #[cfg(test)]
    pub(crate) fn unbound(&self) -> &[u32] {
        &self.unbound
    }

    /// How many distinct bound keys the build rows hold.
    pub(crate) fn keys(&self) -> usize {
        self.parts.iter().map(|part| part.slots.len()).sum()
    }

    /// How many partitions the table was built in.
    pub(crate) fn partitions(&self) -> usize {
        self.parts.len()
    }
}

impl Part {
    /// The groups of the bound rows `rows` (in row order) of the keys `keys`.
    fn build(keys: &[Id], rows: &[u32]) -> Part {
        // First each key's slot and its count of rows, then where its rows start. Sized
        // for a key per row, so that it never grows while it fills.
        let mut slots: FxHashMap<Id, (u32, u32)> =
            FxHashMap::with_capacity_and_hasher(rows.len(), Default::default());
        let mut slot_of: Vec<u32> = Vec::with_capacity(rows.len());
        let mut counts: Vec<u32> = Vec::new();
        for &row in rows {
            let next = counts.len() as u32;
            let slot = slots.entry(keys[row as usize]).or_insert((next, 0)).0;
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
        for (&row, &slot) in rows.iter().zip(&slot_of) {
            let next = &mut fill[slot as usize];
            grouped[*next as usize] = row;
            *next += 1;
        }
        // A key's entry holds where its rows start and how many there are, so that a
        // probe finds them with one lookup.
        for (slot, range) in slots.values_mut() {
            let count = counts[*slot as usize];
            *range = count;
            *slot = fill[*slot as usize] - count;
        }
        Part {
            slots,
            rows: grouped,
        }
    }
}

/// The build rows of a keyed join by the value of its equality's build variable. Each
/// value has a candidate key ([`crate::sparql::eqkey`]): a probe value meets the build
/// rows with the same key, a loose value meets every loose build value, and an unbound
/// value meets none, since `=` then raises an error. The condition is tested on every
/// candidate.
pub(crate) struct EqualityIndex {
    /// The build rows by key, of which a probe reads only the rows of its own key.
    by_key: HashIndex,
    /// The build rows whose value is loose.
    loose: Vec<u32>,
    _charge: Option<RetainedCharge>,
}

impl EqualityIndex {
    /// Index `build` on the values of its column `column`, which probe column `probe` is
    /// compared with.
    pub(crate) fn new(ctx: &Ctx, build: &Table, column: usize, probe: usize) -> Result<Self> {
        let values = &build.cols[column];
        // The key of each row, unbound for a loose or unbound value, and the loose rows.
        let mut charge = ctx.retained_charge(values.len() as u64 * 12 + 1024)?;
        let mut keys = Vec::with_capacity(values.len());
        // Reserve exactly the worst-case number of loose rows. Geometric growth
        // can otherwise exceed the four bytes per row reserved alongside keys.
        let mut loose = Vec::with_capacity(values.len());
        for (row, &value) in values.iter().enumerate() {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            keys.push(match eq_key(ctx, value)? {
                EqKey::Key(k) => k,
                EqKey::Loose => {
                    loose.push(row as u32);
                    Id::UNDEF
                }
                EqKey::Unbound => Id::UNDEF,
            });
        }
        debug_assert!(charge.as_ref().is_none_or(|charge| {
            (keys.capacity() as u64 * 8 + loose.capacity() as u64 * 4) <= charge.bytes()
        }));
        let keyed = Table {
            vars: vec![0],
            cols: vec![keys],
            len: values.len(),
            sorted: Vec::new(),
        };
        let by_key = HashIndex::new(ctx, &keyed, 0, probe)?;
        // Free the temporary keys before a shrink can hold old and new loose
        // allocations together. Both fit within the original twelve-byte charge.
        drop(keyed);
        loose.shrink_to_fit();
        if let Some(charge) = &mut charge {
            charge.resize(loose.capacity() as u64 * 4 + 1024)?;
        }
        Ok(Self {
            by_key,
            loose,
            _charge: charge,
        })
    }

    /// The build rows that probe value `value` may equal.
    #[inline]
    fn candidates<'a>(&'a self, ctx: &Ctx, value: Id) -> Result<Candidates<'a>> {
        Ok(match eq_key(ctx, value)? {
            EqKey::Unbound => Candidates::Keyed(&[], &[]),
            EqKey::Key(k) => Candidates::Keyed(self.by_key.keyed(k), &[]),
            EqKey::Loose => Candidates::Keyed(&[], &self.loose),
        })
    }

    /// The build rows whose value is loose.
    #[cfg(test)]
    pub(crate) fn loose(&self) -> &[u32] {
        &self.loose
    }

    #[cfg(test)]
    pub(crate) fn charge(&self) -> Option<&RetainedCharge> {
        self._charge.as_ref()
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

/// Where a probe stands in its batch: the row being joined, the row the probe ends
/// before, how many of the row's candidates have been visited, and whether the row has
/// met a matching build row so far. A probe loop resumes from it when its room ran out.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Probe {
    pub row: usize,
    pub end: usize,
    pub offset: usize,
    pub matched: bool,
}

impl Probe {
    pub(crate) fn new(rows: std::ops::Range<usize>) -> Self {
        Self {
            row: rows.start,
            end: rows.end,
            offset: 0,
            matched: false,
        }
    }

    #[inline]
    fn advance(&mut self) {
        self.row += 1;
        self.offset = 0;
        self.matched = false;
    }
}

/// One probe batch against one build table: the mode, the shared variables as (probe
/// column, build column), and the index the build rows are found through, if any.
pub(crate) struct Join<'a> {
    pub mode: Mode,
    pub probe: &'a Table,
    pub build: &'a Table,
    pub shared: &'a [(usize, usize)],
    pub index: Option<&'a HashIndex>,
    /// A keyed join's equality: the probe column and the index of the build values.
    pub equality: Option<(usize, &'a EqualityIndex)>,
}

impl Join<'_> {
    /// Whether a probe row's candidates need a test of the shared variables: when
    /// another shared variable than the key may disagree, or when there is no key.
    #[inline]
    fn checks(&self) -> bool {
        !self.shared.is_empty() && (self.shared.len() > 1 || self.index.is_none())
    }

    /// Whether probe row `p` and build row `b` agree on their shared variables, and for
    /// MINUS, whether they also bind a shared variable both.
    #[inline]
    pub(crate) fn compatible(&self, p: usize, b: usize) -> bool {
        let (probe, build) = (self.probe, self.build);
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

    #[inline]
    fn candidates(&self, ctx: &Ctx, row: usize) -> Result<Candidates<'_>> {
        if let Some((p, equality)) = self.equality {
            return equality.candidates(ctx, self.probe.cols[p][row]);
        }
        Ok(match self.index {
            Some(index) => {
                let key = self.probe.cols[index.probe][row];
                if key.is_undef() {
                    Candidates::All(self.build.len)
                } else {
                    Candidates::Keyed(index.keyed(key), &index.unbound)
                }
            }
            None => Candidates::All(self.build.len),
        })
    }

    /// Inner and OPTIONAL joins without a condition: the pairs of the probe rows from
    /// `at`, at most `room` of them appended to `lp` and `rp`. An unmatched OPTIONAL row
    /// pairs with [`NO_ROW`]. `at` says where the next call continues.
    pub(crate) fn pairs(
        &self,
        ctx: &Ctx,
        at: &mut Probe,
        room: usize,
        lp: &mut Vec<u32>,
        rp: &mut Vec<u32>,
    ) -> Result<()> {
        let probe = self.probe;
        let optional = self.mode == Mode::Optional;
        let checks = self.checks();
        let limit = lp.len().saturating_add(room);
        if !checks
            && at.offset == 0
            && let Some(index) = self.index
            && index.unbound.is_empty()
        {
            // Every build row binds the key, so a bound probe row matches exactly the rows
            // of its key. Whole rows go into the pairs while they fit, written in place.
            let keys = &probe.cols[index.probe];
            let mut w = lp.len();
            lp.resize(limit, 0);
            rp.resize(limit, 0);
            let mut row = at.row;
            while row < at.end {
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
                        if w == limit {
                            break;
                        }
                        lp[w] = row as u32;
                        rp[w] = NO_ROW;
                        w += 1;
                    }
                } else {
                    if keyed.len() > limit - w {
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
            at.row = row;
        }
        let mut steps = 0usize;
        while at.row < at.end && lp.len() < limit {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            let row = at.row;
            let candidates = self.candidates(ctx, row)?;
            let n = candidates.len();
            if !checks && at.offset == 0 && n <= limit - lp.len() {
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
                    if lp.len() == limit {
                        break;
                    }
                    lp.push(row as u32);
                    rp.push(NO_ROW);
                }
                at.advance();
                continue;
            }
            while at.offset < n && lp.len() < limit {
                if at.offset.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let other = candidates.get(at.offset);
                at.offset += 1;
                if checks && !self.compatible(row, other as usize) {
                    continue;
                }
                at.matched = true;
                lp.push(row as u32);
                rp.push(other);
            }
            if at.offset < n {
                break;
            }
            if optional && !at.matched {
                if lp.len() == limit {
                    break;
                }
                lp.push(row as u32);
                rp.push(NO_ROW);
            }
            at.advance();
        }
        Ok(())
    }

    /// Joins with a condition: collect the compatible pairs of a chunk of probe rows,
    /// test the condition on their merged rows at once, then keep the pairs that pass
    /// and, in an OPTIONAL, extend each finished row that none of its pairs passed. A
    /// row's output is at most the larger of its visited candidates and one, which
    /// bounds the chunk by `room`. `columns` maps each of the merged row's variables
    /// `vars` to its probe and build columns. Returns how many candidate pairs the
    /// condition was tested on.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn condition(
        &self,
        ctx: &Ctx,
        at: &mut Probe,
        columns: &[(Option<usize>, Option<usize>)],
        vars: &[VarId],
        expression: &Expr,
        room: usize,
        lp: &mut Vec<u32>,
        rp: &mut Vec<u32>,
    ) -> Result<usize> {
        let checks = self.checks();
        // The candidate pairs, the rows of the chunk and the merged candidate rows.
        let _scratch = ctx.charge(room as u64 * (vars.len() as u64 * 8 + 24) + 256)?;
        // The rows of the chunk: the row, its first pair, and whether all its candidates
        // were visited.
        let mut rows: Vec<(u32, u32, bool)> = Vec::new();
        let (mut cl, mut cr): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
        let carried = at.matched;
        let first_row = at.row;
        let mut used = 0usize;
        let mut steps = 0usize;
        'rows: while at.row < at.end && used < room {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            let row = at.row;
            used += 1;
            let candidates = self.candidates(ctx, row)?;
            let n = candidates.len();
            let begin = cl.len() as u32;
            let mut visited = 0usize;
            while at.offset < n {
                if at.offset.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let other = candidates.get(at.offset);
                if checks && !self.compatible(row, other as usize) {
                    at.offset += 1;
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
                at.offset += 1;
                cl.push(row as u32);
                cr.push(other);
            }
            rows.push((row as u32, begin, true));
            at.row += 1;
            at.offset = 0;
        }
        // The merged candidate rows, and which of them pass the condition.
        let mut merged = Table::new(vars.to_vec());
        gather(&mut merged, columns, self.probe, self.build, &cl, &cr);
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
                if !matched && self.mode == Mode::Optional {
                    lp.push(row);
                    rp.push(NO_ROW);
                }
                at.matched = false;
            } else {
                // The row continues in the next chunk.
                at.matched = matched;
            }
        }
        Ok(cl.len())
    }

    /// Semi, anti and MINUS joins: the probe rows from `at` that pass, at most `room` of
    /// them appended to `lp`. A row passes a semi join when a build row matches it, and
    /// the others when none does. A `deferred` join lists the rows that a build row
    /// matches whatever the mode keeps, because the build input is joined in several
    /// chunks and a probe row's verdict waits for the last one.
    pub(crate) fn filter(
        &self,
        ctx: &Ctx,
        at: &mut Probe,
        room: usize,
        deferred: bool,
        lp: &mut Vec<u32>,
    ) -> Result<()> {
        let keep_matched = self.mode == Mode::Semi || deferred;
        let tests = self.checks() || self.mode == Mode::Minus;
        let simple = !tests
            || (self.mode == Mode::Minus
                && self.shared.len() == 1
                && self.index.is_some_and(|i| i.unbound.is_empty()));
        let limit = lp.len().saturating_add(room);
        let mut steps = 0usize;
        while at.row < at.end && lp.len() < limit {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            let row = at.row;
            let candidates = self.candidates(ctx, row)?;
            let found = match candidates {
                // Every keyed build row binds the key, so it matches a bound probe key
                // and, for MINUS, shares that bound variable.
                Candidates::Keyed(keyed, unbound) if simple => {
                    !keyed.is_empty() || (!tests && !unbound.is_empty())
                }
                _ if !tests => candidates.len() > 0,
                _ => {
                    (0..candidates.len()).any(|k| self.compatible(row, candidates.get(k) as usize))
                }
            };
            if found == keep_matched {
                lp.push(row as u32);
            }
            at.advance();
        }
        Ok(())
    }
}

/// Append the rows of the pairs `lp` and `rp` to `out`, one column at a time. A probe
/// value that is unbound takes the build row's value, and [`NO_ROW`] binds none.
pub(crate) fn gather(
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

/// The columns of `vars` on a probe and a build side, for [`gather`].
pub(crate) fn columns(
    vars: &[VarId],
    probe: &[VarId],
    build: &[VarId],
) -> Vec<(Option<usize>, Option<usize>)> {
    vars.iter()
        .map(|v| {
            (
                probe.iter().position(|x| x == v),
                build.iter().position(|x| x == v),
            )
        })
        .collect()
}

/// The build column to key on, as (probe column, build column): the shared column that
/// the probe binds in every row and the build leaves unbound least often, or the one
/// the build leaves unbound least often when the probe leaves every shared column
/// unbound somewhere. `None` when every build row leaves each shared column unbound, or
/// when no variable is shared, so that every probe row visits every build row.
pub(crate) fn key_column(
    ctx: &Ctx,
    probe: &Table,
    build: &Table,
    shared: &[(usize, usize)],
) -> Result<Option<(usize, usize)>> {
    let unbound = |t: &Table, c: usize| t.cols[c].iter().filter(|x| x.is_undef()).count();
    let mut best: Option<((usize, usize), (bool, usize))> = None;
    for &(p, b) in shared {
        ctx.check()?;
        let n = unbound(build, b);
        if n == build.len {
            continue;
        }
        let rank = (unbound(probe, p) > 0, n);
        if best.is_none_or(|(_, r)| rank < r) {
            best = Some(((p, b), rank));
        }
        if rank == (false, 0) {
            break;
        }
    }
    Ok(best.map(|(key, _)| key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoreOptions};
    use std::sync::Arc;

    #[test]
    fn equality_index_covers_loose_rows_across_vector_growth_boundaries() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 16 << 20;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_owner();
        for len in [65_536, 65_537] {
            let mut build = Table::new(vec![0]);
            for row in 0..len {
                build.push_row(&[Id::from_i64(row as i64).unwrap()]);
            }
            // Building the index also checks that every temporary allocation
            // fits its charge before the temporary keys have been dropped.
            let index = EqualityIndex::new(&ctx, &build, 0, 0).unwrap();
            assert_eq!(index.loose().len(), len);
            assert_eq!(index.charge().unwrap().bytes(), len as u64 * 4 + 1024);
        }
    }

    /// A partitioned table finds the same rows as a table of one partition, with the
    /// unbound rows kept apart in build order.
    #[test]
    fn partitioned_tables_find_every_key_and_keep_unbound_rows_apart() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let n = PART_MIN + 1000;
        let mut build = Table::new(vec![0]);
        for row in 0..n {
            let key = if row % 97 == 0 {
                Id::UNDEF
            } else {
                Id::from_i64((row % 5000) as i64).unwrap()
            };
            build.push_row(&[key]);
        }
        let index = HashIndex::new(&ctx, &build, 0, 0).unwrap();
        assert!(index.partitions() > 1);
        assert_eq!(index.keys(), 5000);
        let unbound: Vec<u32> = (0..n as u32).filter(|r| r % 97 == 0).collect();
        assert_eq!(index.unbound(), &unbound[..]);
        for key in [0i64, 1, 4999, 5000, 12345] {
            let expected: Vec<u32> = (0..n as u32)
                .filter(|&r| r % 97 != 0 && (r % 5000) as i64 == key)
                .collect();
            assert_eq!(
                index.keyed(Id::from_i64(key).unwrap()),
                &expected[..],
                "{key}"
            );
        }
    }
}
