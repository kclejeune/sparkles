//! Resumable merge joins over certainly-bound, equally ordered keys. Equal-key
//! runs may grow; their capacities stay on the query budget across output pulls.
use super::{Buffer, CursorOptions, Operator, capacity_bytes};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::plan::{JoinAlgo, Kind, Node};
use crate::sparql::table::{Table, VarId};
use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

fn key(node: &Node) -> Option<VarId> {
    match &node.kind {
        Kind::Join {
            algo: JoinAlgo::Merge,
            keys,
        } => keys.first().copied(),
        Kind::LeftJoin { expr: None } => node.children.first()?.sorted.first().copied(),
        // MINUS and semi and anti joins compare the left row with the right input on
        // their shared variables. With one shared variable that both inputs bind on
        // every row, that comparison is equality of the key.
        Kind::Minus | Kind::HalfJoin { .. } => {
            let [left, right] = &node.children[..] else {
                return None;
            };
            let mut shared = left.vars.iter().filter(|v| right.vars.contains(v));
            let key = shared.next().copied();
            if shared.next().is_some() { None } else { key }
        }
        _ => None,
    }
}
pub(super) fn eligible(node: &Node) -> bool {
    let Some(key) = key(node) else { return false };
    node.children.len() == 2
        && node
            .children
            .iter()
            .all(|child| child.sorted.first() == Some(&key) && child.certain.contains(&key))
}

#[derive(Default)]
struct Input {
    buffer: Option<Buffer>,
    at: usize,
}
struct Run {
    owned: Option<Buffer>,
    rows: Range<usize>,
}
struct Pair {
    left: Run,
    right: Run,
    li: usize,
    ri: usize,
    matched: bool,
}

pub(super) struct Merge {
    inputs: [Input; 2],
    keys: [Vec<usize>; 2],
    columns: Vec<(Option<usize>, Option<usize>)>,
    pair: Option<Pair>,
    optional: bool,
    /// For MINUS and semi and anti joins, whether a left row passes when the right
    /// input has its key (a semi join) or when it lacks it.
    filter: Option<bool>,
    /// Whether the right input of a filtering join reads only the key yet.
    narrowed: bool,
    _charge: Option<RetainedCharge>,
}

/// Nearby unmatched keys are common in interleaved graph relations. Probe a
/// bounded neighborhood before bisecting a long gap; the current key is known
/// to precede `key` and is included in the returned count.
#[inline]
fn before(column: &[Id], at: usize, key: Id) -> usize {
    let stop = at.saturating_add(8).min(column.len());
    let mut next = at + 1;
    while next < stop {
        if column[next] >= key {
            return next - at;
        }
        next += 1;
    }
    next + column[next..].partition_point(|id| *id < key) - at
}

impl Input {
    fn ensure(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
        cap: usize,
    ) -> Result<bool> {
        if self.buffer.as_ref().is_some_and(|b| self.at < b.table.len) {
            return Ok(true);
        }
        self.buffer = None;
        self.at = 0;
        if child.done {
            return Ok(false);
        }
        self.buffer = child.next(ctx, options, cap)?;
        Ok(self.buffer.is_some())
    }

    fn table(&self) -> &Table {
        &self.buffer.as_ref().expect("available merge input").table
    }

    fn end(&self, ctx: &Ctx, columns: &[usize]) -> Result<usize> {
        let table = self.table();
        let mut end = self.at + 1;
        while end < table.len
            && columns
                .iter()
                .all(|&c| table.cols[c][self.at] == table.cols[c][end])
        {
            if (end - self.at).is_multiple_of(1024) {
                ctx.check()?;
            }
            end += 1;
        }
        Ok(end)
    }

    fn run(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
        cap: usize,
        columns: &[usize],
    ) -> Result<Run> {
        let end = self.end(ctx, columns)?;
        let range = self.at..end;
        if end < self.table().len || child.done {
            self.at = end;
            return Ok(Run {
                owned: None,
                rows: range,
            });
        }
        // Only a run touching an unproven batch boundary needs a copy. Ordinary
        // runs borrow the already charged input batch, including across pulls.
        let _key_charge = ctx.charge(columns.len() as u64 * 8 + 64)?;
        let key: Vec<Id> = columns
            .iter()
            .map(|&c| self.table().cols[c][self.at])
            .collect();
        let mut held = Buffer::new(ctx, &self.table().vars, range.len())?;
        append(ctx, &mut held, self.table(), range)?;
        self.at = end;
        loop {
            if !self.ensure(ctx, child, options, cap)? {
                break;
            }
            if columns
                .iter()
                .zip(&key)
                .any(|(&c, &key)| self.table().cols[c][self.at] != key)
            {
                break;
            }
            let end = self.end(ctx, columns)?;
            let range = self.at..end;
            append(ctx, &mut held, self.table(), range)?;
            self.at = end;
            if end < self.table().len || child.done {
                break;
            }
        }
        let n = held.table.len;
        Ok(Run {
            owned: Some(held),
            rows: 0..n,
        })
    }
}

pub(super) fn append(
    ctx: &Arc<Ctx>,
    held: &mut Buffer,
    table: &Table,
    rows: Range<usize>,
) -> Result<()> {
    let len = held
        .table
        .len
        .checked_add(rows.len())
        .ok_or_else(|| Error::invalid("merge run length overflow"))?;
    ctx.check_rows(len)?;
    // Old and new allocations can coexist while reserve_exact moves a column.
    let capacity = if held.table.cols.iter().any(|column| column.capacity() < len) {
        len.checked_next_power_of_two().unwrap_or(len)
    } else {
        len
    };
    held.charge
        .resize(capacity_bytes(&held.table).saturating_add(
            capacity as u64 * table.width() as u64 * 8 + table.width() as u64 * 40 + 128,
        ))?;
    for (to, from) in held.table.cols.iter_mut().zip(&table.cols) {
        to.reserve_exact(capacity - to.len());
        to.extend_from_slice(&from[rows.clone()]);
    }
    held.table.len = len;
    held.reconcile()?;
    Ok(())
}

impl Run {
    fn table<'a>(&'a self, input: &'a Input) -> &'a Table {
        self.owned
            .as_ref()
            .map_or_else(|| input.table(), |b| &b.table)
    }
}

impl Merge {
    pub(super) fn new(ctx: &Ctx, node: &Node) -> Result<Self> {
        let keys = [key(node).expect("eligible merge key")];
        let charge =
            ctx.retained_charge(node.vars.len() as u64 * 64 + keys.len() as u64 * 32 + 1024)?;
        let columns = node
            .vars
            .iter()
            .map(|v| {
                (
                    node.children[0].vars.iter().position(|x| x == v),
                    node.children[1].vars.iter().position(|x| x == v),
                )
            })
            .collect();
        let key_columns = |side: usize| {
            keys[..1]
                .iter()
                .map(|key| {
                    node.children[side]
                        .vars
                        .iter()
                        .position(|v| v == key)
                        .expect("certain merge key")
                })
                .collect()
        };
        Ok(Self {
            inputs: Default::default(),
            keys: [key_columns(0), key_columns(1)],
            columns,
            pair: None,
            optional: matches!(node.kind, Kind::LeftJoin { .. }),
            filter: match node.kind {
                Kind::Minus => Some(false),
                Kind::HalfJoin { anti } => Some(!anti),
                _ => None,
            },
            narrowed: false,
            _charge: charge,
        })
    }

    fn unmatched(&mut self, out: &mut Buffer, cap: usize, ctx: &Ctx) -> Result<()> {
        let end = self.inputs[0]
            .end(ctx, &self.keys[0])?
            .min(self.inputs[0].at + cap - out.table.len);
        for row in self.inputs[0].at..end {
            for (column, &(left, _)) in out.table.cols.iter_mut().zip(&self.columns) {
                column.push(left.map_or(Id::UNDEF, |c| self.inputs[0].table().cols[c][row]));
            }
            out.table.len += 1;
        }
        self.inputs[0].at = end;
        Ok(())
    }

    /// Pass up to `cap` left rows of a MINUS, semi or anti join into `out`, or count
    /// them when `out` is None. A left row passes when the right input has its key
    /// and `keep_matched`, or lacks it and not `keep_matched`. Both inputs are ordered
    /// on the key, so the right input only moves forward and no run is held.
    #[allow(clippy::too_many_arguments)]
    fn filter(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        cap: usize,
        pull: usize,
        keep_matched: bool,
        mut out: Option<&mut Buffer>,
    ) -> Result<usize> {
        let mut passed = 0usize;
        let mut steps = 0usize;
        while passed < cap {
            ctx.check()?;
            if !self.inputs[0].ensure(ctx, &mut children[0], options, pull)? {
                break;
            }
            let more = self.inputs[1].ensure(ctx, &mut children[1], options, pull)?;
            if !more && keep_matched {
                break;
            }
            let [left, right] = &self.inputs;
            let table = left.table();
            let lk = &table.cols[self.keys[0][0]];
            let rk: &[Id] = if more {
                &right.table().cols[self.keys[1][0]]
            } else {
                &[]
            };
            let (mut li, mut ri) = (left.at, right.at);
            let mut emit = |rows: Range<usize>| {
                if let Some(out) = out.as_deref_mut() {
                    for (column, &(l, _)) in out.table.cols.iter_mut().zip(&self.columns) {
                        let l = l.expect("filtering joins output left columns");
                        column.extend_from_slice(&table.cols[l][rows.clone()]);
                    }
                    out.table.len += rows.len();
                }
            };
            while li < lk.len() && passed < cap {
                steps += 1;
                if steps.is_multiple_of(1024) {
                    ctx.check()?;
                }
                if ri == rk.len() {
                    if more {
                        // The next right batch decides the rest of this one.
                        break;
                    }
                    let end = lk.len().min(li.saturating_add(cap - passed));
                    emit(li..end);
                    passed += end - li;
                    li = end;
                    continue;
                }
                match lk[li].cmp(&rk[ri]) {
                    Ordering::Less => {
                        let n = before(lk, li, rk[ri]);
                        if keep_matched {
                            li += n;
                        } else {
                            let end = li + n.min(cap - passed);
                            emit(li..end);
                            passed += end - li;
                            li = end;
                        }
                    }
                    Ordering::Greater => ri += before(rk, ri, lk[li]),
                    Ordering::Equal => {
                        if keep_matched {
                            emit(li..li + 1);
                            passed += 1;
                        }
                        li += 1;
                    }
                }
            }
            self.inputs[0].at = li;
            if more {
                self.inputs[1].at = ri;
            }
        }
        Ok(passed)
    }

    pub(super) fn supports_count(&self) -> bool {
        self.columns
            .iter()
            .filter(|(l, r)| l.is_some() && r.is_some())
            .count()
            == 1
    }

    /// Stay inside already loaded batches when the certainly-bound merge key
    /// is the only shared variable. Lookahead proves that a run ends inside its
    /// batch, and such runs join directly. Runs that touch a batch boundary, or
    /// whose product does not fit the output, return to the resumable general path.
    fn interior(
        &mut self,
        ctx: &Ctx,
        out: &mut Buffer,
        cap: usize,
        steps: &mut usize,
    ) -> Result<bool> {
        let (left, right) = (self.inputs[0].table(), self.inputs[1].table());
        let (lk, rk) = (&left.cols[self.keys[0][0]], &right.cols[self.keys[1][0]]);
        let (mut li, mut ri) = (self.inputs[0].at, self.inputs[1].at);
        let original = (li, ri);
        while li + 1 < left.len && ri + 1 < right.len && out.table.len < cap {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            *steps += 1;
            match lk[li].cmp(&rk[ri]) {
                Ordering::Less if !self.optional => {
                    li += before(lk, li, rk[ri]);
                }
                Ordering::Greater => {
                    ri += before(rk, ri, lk[li]);
                }
                Ordering::Less => {
                    for (column, &(l, _)) in out.table.cols.iter_mut().zip(&self.columns) {
                        column.push(l.map_or(Id::UNDEF, |c| left.cols[c][li]));
                    }
                    out.table.len += 1;
                    li += 1;
                }
                Ordering::Equal => {
                    // Both runs must end inside their batches, which the next
                    // different key proves, and their product must fit the output.
                    let key = lk[li];
                    let le = li + 1 + lk[li + 1..].iter().take_while(|&&k| k == key).count();
                    let re = ri + 1 + rk[ri + 1..].iter().take_while(|&&k| k == key).count();
                    if le == lk.len()
                        || re == rk.len()
                        || (le - li).saturating_mul(re - ri) > cap - out.table.len
                    {
                        break;
                    }
                    for row in li..le {
                        for (column, &(l, r)) in out.table.cols.iter_mut().zip(&self.columns) {
                            match (l, r) {
                                (Some(c), _) => {
                                    column.extend(std::iter::repeat_n(left.cols[c][row], re - ri));
                                }
                                (None, Some(c)) => column.extend_from_slice(&right.cols[c][ri..re]),
                                (None, None) => {
                                    column.extend(std::iter::repeat_n(Id::UNDEF, re - ri));
                                }
                            }
                        }
                    }
                    out.table.len += (le - li) * (re - ri);
                    li = le;
                    ri = re;
                }
            }
        }
        self.inputs[0].at = li;
        self.inputs[1].at = ri;
        Ok(original != (li, ri))
    }

    /// Count compatible run multiplicities without constructing join output.
    /// The first key is bound on both sides and is the only shared variable.
    pub(super) fn count(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
    ) -> Result<u64> {
        debug_assert!(self.supports_count());
        for (side, child) in children.iter_mut().enumerate() {
            let key = child.vars[self.keys[side][0]];
            child.restrict_count_scan(ctx, key);
            self.keys[side][0] = child.vars.iter().position(|v| *v == key).unwrap();
        }
        if let Some(keep_matched) = self.filter {
            self.narrowed = true;
            let count = self.filter(
                ctx,
                children,
                options,
                usize::MAX,
                options.batch_rows,
                keep_matched,
                None,
            )?;
            ctx.check_rows(count)?;
            ctx.check()?;
            return Ok(count as u64);
        }
        let mut count = 0u64;
        let mut steps = 0usize;
        loop {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            if !self.inputs[0].ensure(ctx, &mut children[0], options, options.batch_rows)? {
                break;
            }
            if !self.inputs[1].ensure(ctx, &mut children[1], options, options.batch_rows)? {
                if !self.optional {
                    break;
                }
                let input = &mut self.inputs[0];
                count = count
                    .checked_add((input.table().len - input.at) as u64)
                    .ok_or_else(|| Error::invalid("join count overflow"))?;
                input.at = input.table().len;
            } else {
                // Hold the column slices across an interior merge, rather than
                // repeating input/option/key lookups for every matching row.
                // Leave runs touching either batch boundary for reconciliation.
                let [left, right] = &mut self.inputs;
                let (lc, rc) = (
                    &left.table().cols[self.keys[0][0]],
                    &right.table().cols[self.keys[1][0]],
                );
                let (mut li, mut ri) = (left.at, right.at);
                while li + 1 < lc.len() && ri + 1 < rc.len() {
                    if steps.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    steps += 1;
                    let increment = match lc[li].cmp(&rc[ri]) {
                        Ordering::Less => {
                            let n = before(lc, li, rc[ri]);
                            li += n;
                            if self.optional { n as u64 } else { 0 }
                        }
                        Ordering::Greater => {
                            ri += before(rc, ri, lc[li]);
                            0
                        }
                        Ordering::Equal => {
                            let le = if lc[li + 1] != lc[li] {
                                li + 1
                            } else {
                                li + lc[li..].partition_point(|id| *id == lc[li])
                            };
                            let re = if rc[ri + 1] != rc[ri] {
                                ri + 1
                            } else {
                                ri + rc[ri..].partition_point(|id| *id == rc[ri])
                            };
                            if le == lc.len() || re == rc.len() {
                                break;
                            }
                            let n = ((le - li) as u64)
                                .checked_mul((re - ri) as u64)
                                .ok_or_else(|| Error::invalid("join count overflow"))?;
                            li = le;
                            ri = re;
                            n
                        }
                    };
                    count = count
                        .checked_add(increment)
                        .ok_or_else(|| Error::invalid("join count overflow"))?;
                }
                if li != left.at || ri != right.at {
                    left.at = li;
                    right.at = ri;
                    ctx.check_rows(
                        usize::try_from(count)
                            .map_err(|_| Error::invalid("join row count overflow"))?,
                    )?;
                    continue;
                }
                let left = self.inputs[0].table().cols[self.keys[0][0]][self.inputs[0].at];
                let right = self.inputs[1].table().cols[self.keys[1][0]][self.inputs[1].at];
                match left.cmp(&right) {
                    Ordering::Less => {
                        let input = &mut self.inputs[0];
                        let n = before(&input.table().cols[self.keys[0][0]], input.at, right);
                        if self.optional {
                            count = count
                                .checked_add(n as u64)
                                .ok_or_else(|| Error::invalid("join count overflow"))?;
                        }
                        input.at += n;
                    }
                    Ordering::Greater => {
                        let input = &mut self.inputs[1];
                        input.at += before(&input.table().cols[self.keys[1][0]], input.at, left);
                    }
                    Ordering::Equal => {
                        // Proven unique interior runs need no Run construction or
                        // boundary reconciliation. Boundary/duplicate runs retain
                        // the normal path so multiplicities span batches correctly.
                        let unique = self.inputs.iter().zip(&self.keys).all(|(input, keys)| {
                            let column = &input.table().cols[keys[0]];
                            input.at + 1 < column.len() && column[input.at + 1] != column[input.at]
                        });
                        if unique {
                            self.inputs[0].at += 1;
                            self.inputs[1].at += 1;
                            count = count
                                .checked_add(1)
                                .ok_or_else(|| Error::invalid("join count overflow"))?;
                            ctx.check_rows(
                                usize::try_from(count)
                                    .map_err(|_| Error::invalid("join row count overflow"))?,
                            )?;
                            continue;
                        }
                        let left = self.inputs[0].run(
                            ctx,
                            &mut children[0],
                            options,
                            options.batch_rows,
                            &self.keys[0],
                        )?;
                        let right = self.inputs[1].run(
                            ctx,
                            &mut children[1],
                            options,
                            options.batch_rows,
                            &self.keys[1],
                        )?;
                        let n = (left.rows.len() as u64)
                            .checked_mul(right.rows.len() as u64)
                            .ok_or_else(|| Error::invalid("join count overflow"))?;
                        count = count
                            .checked_add(n)
                            .ok_or_else(|| Error::invalid("join count overflow"))?;
                    }
                }
            }
            ctx.check_rows(
                usize::try_from(count).map_err(|_| Error::invalid("join row count overflow"))?,
            )?;
        }
        ctx.check()?;
        Ok(count)
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let mut out = Buffer::new(ctx, vars, cap)?;
        if let Some(keep_matched) = self.filter {
            if !self.narrowed {
                // The output takes only left columns, so the right input needs its key alone.
                let child = &mut children[1];
                let key = child.vars[self.keys[1][0]];
                child.restrict_count_scan(ctx, key);
                self.keys[1][0] = child.vars.iter().position(|v| *v == key).unwrap();
                self.narrowed = true;
            }
            self.filter(
                ctx,
                children,
                options,
                cap,
                cap,
                keep_matched,
                Some(&mut out),
            )?;
            out.reconcile()?;
            return Ok((!out.table.is_empty()).then_some(out));
        }
        let mut steps = 0usize;
        while out.table.len < cap {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            if self.pair.is_none() {
                // Preserve empty-left short circuit and callback behavior.
                if !self.inputs[0].ensure(ctx, &mut children[0], options, cap)? {
                    break;
                }
                if !self.inputs[1].ensure(ctx, &mut children[1], options, cap)? {
                    if !self.optional {
                        break;
                    }
                    self.unmatched(&mut out, cap, ctx)?;
                    continue;
                }
                if self.supports_count() && self.interior(ctx, &mut out, cap, &mut steps)? {
                    continue;
                }
                let cmp = self.keys[0]
                    .iter()
                    .zip(&self.keys[1])
                    .map(|(&l, &r)| {
                        self.inputs[0].table().cols[l][self.inputs[0].at]
                            .cmp(&self.inputs[1].table().cols[r][self.inputs[1].at])
                    })
                    .find(|o| *o != Ordering::Equal)
                    .unwrap_or(Ordering::Equal);
                match cmp {
                    Ordering::Less => {
                        if self.optional {
                            self.unmatched(&mut out, cap, ctx)?;
                            continue;
                        }
                        self.inputs[0].at = self.inputs[0].end(ctx, &self.keys[0])?;
                        continue;
                    }
                    Ordering::Greater => {
                        self.inputs[1].at = self.inputs[1].end(ctx, &self.keys[1])?;
                        continue;
                    }
                    Ordering::Equal => {}
                }
                // Single-row runs wholly proven by lookahead can be copied
                // directly. Duplicate/boundary runs keep the resumable pair path.
                let single = (0..2).all(|side| {
                    let input = &self.inputs[side];
                    let end = input.at + 1;
                    (end == input.table().len && children[side].done)
                        || (end < input.table().len
                            && self.keys[side].iter().any(|&column| {
                                input.table().cols[column][input.at]
                                    != input.table().cols[column][end]
                            }))
                });
                if single {
                    let left = self.inputs[0].table();
                    let right = self.inputs[1].table();
                    let (li, ri) = (self.inputs[0].at, self.inputs[1].at);
                    let compatible = self.columns.iter().all(|&(l, r)| match (l, r) {
                        (Some(l), Some(r)) => {
                            let (l, r) = (left.cols[l][li], right.cols[r][ri]);
                            l == r || l.is_undef() || r.is_undef()
                        }
                        _ => true,
                    });
                    if compatible || self.optional {
                        for (col, &(l, r)) in out.table.cols.iter_mut().zip(&self.columns) {
                            let value = l.map_or(Id::UNDEF, |c| left.cols[c][li]);
                            col.push(if value.is_undef() && compatible {
                                r.map_or(value, |c| right.cols[c][ri])
                            } else {
                                value
                            });
                        }
                        out.table.len += 1;
                    }
                    self.inputs[0].at += 1;
                    self.inputs[1].at += 1;
                    continue;
                }
                let left =
                    self.inputs[0].run(ctx, &mut children[0], options, cap, &self.keys[0])?;
                let right =
                    self.inputs[1].run(ctx, &mut children[1], options, cap, &self.keys[1])?;
                let (li, ri) = (left.rows.start, right.rows.start);
                self.pair = Some(Pair {
                    left,
                    right,
                    li,
                    ri,
                    matched: false,
                });
            }
            // With the certainly bound key as the only shared variable, every pair of
            // the two runs is compatible, so a left row joins a slice of right rows.
            let key_only = self.supports_count();
            let pair = self.pair.as_mut().expect("matched runs");
            let left = pair.left.table(&self.inputs[0]);
            let right = pair.right.table(&self.inputs[1]);
            while pair.li < pair.left.rows.end && out.table.len < cap {
                if steps.is_multiple_of(1024) {
                    ctx.check()?;
                }
                steps += 1;
                if pair.ri == pair.right.rows.end {
                    if self.optional && !pair.matched {
                        for (col, &(l, _)) in out.table.cols.iter_mut().zip(&self.columns) {
                            col.push(l.map_or(Id::UNDEF, |c| left.cols[c][pair.li]));
                        }
                        out.table.len += 1;
                    }
                    pair.ri = pair.right.rows.start;
                    pair.li += 1;
                    pair.matched = false;
                    continue;
                }
                if key_only {
                    let n = (pair.right.rows.end - pair.ri).min(cap - out.table.len);
                    let rows = pair.ri..pair.ri + n;
                    for (col, &(l, r)) in out.table.cols.iter_mut().zip(&self.columns) {
                        match (l, r) {
                            (Some(l), _) => {
                                col.extend(std::iter::repeat_n(left.cols[l][pair.li], n));
                            }
                            (None, Some(r)) => col.extend_from_slice(&right.cols[r][rows.clone()]),
                            (None, None) => col.extend(std::iter::repeat_n(Id::UNDEF, n)),
                        }
                    }
                    out.table.len += n;
                    pair.matched = true;
                    pair.ri += n;
                    continue;
                }
                let compatible = self.columns.iter().all(|&(l, r)| match (l, r) {
                    (Some(l), Some(r)) => {
                        let (l, r) = (left.cols[l][pair.li], right.cols[r][pair.ri]);
                        l == r || l.is_undef() || r.is_undef()
                    }
                    _ => true,
                });
                if compatible {
                    pair.matched = true;
                    for (col, &(l, r)) in out.table.cols.iter_mut().zip(&self.columns) {
                        let l = l.map_or(Id::UNDEF, |c| left.cols[c][pair.li]);
                        col.push(if l.is_undef() {
                            r.map_or(l, |c| right.cols[c][pair.ri])
                        } else {
                            l
                        });
                    }
                    out.table.len += 1;
                }
                pair.ri += 1;
            }
            if pair.li == pair.left.rows.end {
                self.pair = None;
            }
        }
        out.reconcile()?;
        Ok((!out.table.is_empty()).then_some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sparql::{cursor::FallbackPolicy, exec};
    use crate::store::{Store, StoreOptions};

    fn context(limit: u64) -> Arc<Ctx> {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        for name in ["key", "shared", "left", "right"] {
            ctx.var(name);
        }
        ctx.mem_limit = limit;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_cursor_owner();
        ctx
    }

    fn values(vars: Vec<VarId>, rows: &[Vec<i64>]) -> Node {
        let mut table = Table::new(vars.clone());
        for row in rows {
            table.push_row(
                &row.iter()
                    .map(|&x| {
                        if x == -1 {
                            Id::UNDEF
                        } else {
                            Id::from_i64(x).unwrap()
                        }
                    })
                    .collect::<Vec<_>>(),
            );
        }
        table.sorted = vec![vars[0]];
        let mut node = Node::leaf(Kind::Values(table), vars, rows.len() as f64, String::new());
        node.sorted = vec![node.vars[0]];
        node.certain = vec![node.vars[0]];
        node
    }

    fn join(left: Node, right: Node) -> Node {
        let mut vars = left.vars.clone();
        for &var in &right.vars {
            if !vars.contains(&var) {
                vars.push(var);
            }
        }
        let keys = left
            .vars
            .iter()
            .filter(|v| right.vars.contains(v))
            .copied()
            .collect();
        let mut node = Node::leaf(
            Kind::Join {
                algo: JoinAlgo::Merge,
                keys,
            },
            vars,
            100.0,
            String::new(),
        );
        node.sorted = vec![left.vars[0]];
        node.children = vec![left, right];
        node
    }

    fn rows(table: &Table) -> Vec<Vec<Id>> {
        let mut rows = (0..table.len)
            .map(|i| table.cols.iter().map(|c| c[i]).collect())
            .collect::<Vec<Vec<_>>>();
        rows.sort();
        rows
    }

    #[test]
    fn merge_runs_resume_with_duplicates_unbound_shared_columns_and_empty_inputs() {
        let l = [
            vec![0, 0, 10],
            vec![0, -1, 11],
            vec![1, 1, 12],
            vec![2, 0, 13],
            vec![2, 1, 14],
            vec![2, -1, 15],
            vec![4, 0, 16],
        ];
        let r = [
            vec![0, 0, 20],
            vec![0, 1, 21],
            vec![0, -1, 22],
            vec![1, 1, 23],
            vec![2, 0, 24],
            vec![2, 1, 25],
            vec![3, 0, 26],
        ];
        for (left, right) in [(&l[..], &r[..]), (&l[..], &[][..]), (&[][..], &r[..])] {
            for cap in [1, 2, 3, 4096] {
                let ctx = context(1 << 20);
                let node = join(values(vec![0, 1, 2], left), values(vec![0, 1, 3], right));
                let expected = rows(&exec::execute(&ctx, &node).unwrap().0);
                let mut op =
                    Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
                assert!(!op.materializes);
                let options = CursorOptions {
                    batch_rows: cap,
                    ..Default::default()
                };
                let mut result = Vec::new();
                while let Some(batch) = op.next(&ctx, &options, cap).unwrap() {
                    assert!(batch.table.len <= cap);
                    result.extend(rows(&batch.table));
                }
                result.sort();
                assert_eq!(result, expected, "batch cap {cap}");
            }
        }
    }

    /// With the key as the only shared variable, runs of duplicates on both sides join
    /// inside a batch or across its boundary, for inner and OPTIONAL merges alike.
    #[test]
    fn key_only_merges_join_duplicate_runs_at_every_batch_size() {
        let l = [
            vec![0, 10],
            vec![0, 11],
            vec![1, 12],
            vec![2, 13],
            vec![2, 14],
            vec![2, 15],
            vec![4, 16],
            vec![5, 17],
            vec![5, 18],
        ];
        let r = [
            vec![0, 20],
            vec![0, 21],
            vec![0, 22],
            vec![2, 23],
            vec![3, 24],
            vec![5, 25],
            vec![5, 26],
            vec![6, 27],
        ];
        for optional in [false, true] {
            for cap in [1, 2, 3, 4, 5, 4096] {
                let ctx = context(1 << 20);
                let mut node = join(values(vec![0, 2], &l), values(vec![0, 3], &r));
                if optional {
                    node.kind = Kind::LeftJoin { expr: None };
                }
                assert!(eligible(&node));
                let expected = rows(&exec::execute(&ctx, &node).unwrap().0);
                let mut op =
                    Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
                assert!(matches!(op.state, super::super::State::Merge(_)));
                let options = CursorOptions {
                    batch_rows: cap,
                    ..Default::default()
                };
                let mut result = Vec::new();
                while let Some(batch) = op.next(&ctx, &options, cap).unwrap() {
                    assert!(batch.table.len <= cap);
                    result.extend(rows(&batch.table));
                }
                result.sort();
                assert_eq!(result, expected, "optional {optional}, batch cap {cap}");
            }
        }
    }

    /// MINUS and semi and anti joins on one ordered, certainly bound key pass left rows
    /// by the presence of their key on the right, across duplicates on both sides,
    /// batch boundaries and empty inputs, and count without building rows.
    #[test]
    fn filtering_merges_match_eager_at_every_batch_size() {
        let l = [
            vec![0, 10],
            vec![0, 11],
            vec![1, 12],
            vec![2, 13],
            vec![2, 14],
            vec![4, 15],
            vec![5, 16],
            vec![9, 17],
        ];
        let r = [
            vec![0, 20],
            vec![0, 21],
            vec![2, 22],
            vec![3, 23],
            vec![3, 24],
            vec![5, 25],
        ];
        let kinds = [
            Kind::Minus,
            Kind::HalfJoin { anti: false },
            Kind::HalfJoin { anti: true },
        ];
        for (k, kind) in kinds.into_iter().enumerate() {
            for (left, right) in [(&l[..], &r[..]), (&l[..], &[][..]), (&[][..], &r[..])] {
                for cap in [1, 2, 3, 4096] {
                    let ctx = context(1 << 20);
                    let left = values(vec![0, 1], left);
                    let mut node = Node::leaf(kind.clone(), left.vars.clone(), 8.0, String::new());
                    node.sorted = vec![0];
                    node.children = vec![left, values(vec![0, 2], right)];
                    assert!(eligible(&node));
                    let expected = rows(&exec::execute(&ctx, &node).unwrap().0);
                    let mut op =
                        Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization)
                            .unwrap();
                    assert!(matches!(op.state, super::super::State::Merge(_)));
                    let options = CursorOptions {
                        batch_rows: cap,
                        ..Default::default()
                    };
                    let mut result = Vec::new();
                    while let Some(batch) = op.next(&ctx, &options, cap).unwrap() {
                        assert!(batch.table.len <= cap);
                        result.extend(rows(&batch.table));
                    }
                    result.sort();
                    assert_eq!(result, expected, "kind {k}, batch cap {cap}");
                    let mut op =
                        Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
                    let counted = op.count_input(&ctx, &options).unwrap();
                    assert_eq!(counted, Some(expected.len() as u64), "kind {k} count");
                }
            }
        }
    }

    #[test]
    fn uncertain_keys_use_compatibility_join_and_cross_batch_runs_fail_under_budget() {
        let mut node = join(
            values(vec![0, 1], &[vec![0, 1]]),
            values(vec![0, 2], &[vec![0, 2]]),
        );
        node.children[0].certain.clear();
        assert!(!eligible(&node));
        let op = Operator::build(
            &context(1 << 20),
            node,
            FallbackPolicy::RejectMaterialization,
        )
        .unwrap();
        assert!(matches!(op.state, super::super::State::Binary(_)));
        let many = vec![vec![0, 1]; 10_000];
        let ctx = context(32 << 10);
        let node = join(values(vec![0, 1], &many), values(vec![0, 2], &[vec![0, 2]]));
        let mut op = Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
        let error = match op.next(&ctx, &CursorOptions::default(), 1) {
            Err(error) => error,
            Ok(_) => panic!("growing run exceeded the budget without failing"),
        };
        assert!(matches!(error, Error::BudgetExceeded(_)));
        assert!(ctx.mem_peak() <= 32 << 10);
    }

    /// A planner claim that an input is sorted is not enough: the merge join needs the
    /// order its input operator keeps. Concatenated UNION arms keep none, so the hash
    /// join takes over and still answers correctly.
    #[test]
    fn inputs_without_a_kept_order_use_the_hash_join() {
        let arm = |rows: &[Vec<i64>]| values(vec![0, 1], rows);
        let mut union = Node::leaf(Kind::Union, vec![0, 1], 4.0, String::new());
        union.children = vec![
            arm(&[vec![2, 1], vec![3, 1]]),
            arm(&[vec![0, 2], vec![1, 2]]),
        ];
        union.sorted = vec![0];
        union.certain = vec![0, 1];
        let node = join(
            union,
            values(vec![0, 2], &[vec![0, 5], vec![1, 6], vec![3, 7]]),
        );
        assert!(eligible(&node));
        let ctx = context(1 << 20);
        let expected = rows(&exec::execute(&ctx, &node).unwrap().0);
        let mut op = Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
        assert!(matches!(op.state, super::super::State::Binary(_)));
        let mut result = Vec::new();
        while let Some(batch) = op.next(&ctx, &CursorOptions::default(), 1).unwrap() {
            result.extend(rows(&batch.table));
        }
        result.sort();
        assert_eq!(result, expected);
        assert_eq!(result.len(), 3);
    }

    /// Debug builds check that output batches keep the order their operator claims.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "out of order")]
    fn a_false_order_claim_fails_the_debug_check() {
        let ctx = context(1 << 20);
        let node = values(vec![0, 1], &[vec![2, 0], vec![1, 0]]);
        let mut op = Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
        let _ = op.next(&ctx, &CursorOptions::default(), 4096);
    }
}
