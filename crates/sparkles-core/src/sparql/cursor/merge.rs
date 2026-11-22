//! Resumable merge joins over certainly-bound, equally ordered keys. Equal-key
//! runs may grow; their capacities stay on the query budget across output pulls.
//!
//! A query that may spill writes a run of equal keys that outgrows a quarter of the room
//! it had when the run started, or whose charge fails, to a spill file (X08 §6.3). The
//! pair of runs then joins from the file: the left run is read once, and the right run
//! is read again for each left row, so the output keeps its left-major order.
use super::{Buffer, CursorOptions, Operator, capacity_bytes};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::expr::Expr;
use crate::sparql::join::{gallop, merge as kernel};
use crate::sparql::plan::{JoinAlgo, Kind, Node};
use crate::sparql::table::{Table, VarId};
use crate::spill::{SpillKind, SpillReader, SpillRun, SpillWriter};
use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

/// No right row: the OPTIONAL extension of an unmatched left row.
const NO_ROW: u32 = u32::MAX;

fn key(ctx: &Ctx, node: &Node) -> Option<VarId> {
    match &node.kind {
        Kind::Join {
            algo: JoinAlgo::Merge,
            keys,
        } => keys.first().copied(),
        // An OPTIONAL merges on the key both inputs are sorted on when the switch that
        // selects the merge OPTIONAL in eager execution is on, so that EXPLAIN names
        // the same algorithm in both modes.
        Kind::LeftJoin { .. } if ctx.opt.merge_left_join => {
            node.children.first()?.sorted.first().copied()
        }
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
pub(super) fn eligible(ctx: &Ctx, node: &Node) -> bool {
    let Some(key) = key(ctx, node) else {
        return false;
    };
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
    /// The run's rows on disk, when they outgrew memory. `rows` then counts them.
    spooled: Option<Arc<SpillRun>>,
}
struct Pair {
    left: Run,
    right: Run,
    li: usize,
    ri: usize,
    matched: bool,
}

pub(super) struct Merge {
    /// The variable the inputs merge on.
    key: VarId,
    inputs: [Input; 2],
    /// Child schemas survive a final run whose input has already reached EOF.
    widths: [usize; 2],
    keys: [Vec<usize>; 2],
    columns: Vec<(Option<usize>, Option<usize>)>,
    pair: Option<Pair>,
    /// A pair of runs of which at least one was spooled to disk.
    spooled: Option<Box<SpooledPair>>,
    /// What the plan reports: the bytes and runs spooled.
    pub(super) spilled: u64,
    pub(super) spooled_runs: u64,
    optional: bool,
    /// For MINUS and semi and anti joins, whether a left row passes when the right
    /// input has its key (a semi join) or when it lacks it.
    filter: Option<bool>,
    /// Whether the right input of a filtering join reads only the key yet.
    narrowed: bool,
    /// The condition of an OPTIONAL. The merge then ends the output of every left row
    /// with its extension, and a last output column, bound only on pairs, tells the
    /// two apart. [`Merge::next`] tests the condition on the pairs of each batch and
    /// keeps a left row's extension only when none of its pairs passed.
    condition: Option<Arc<Expr>>,
    /// Whether the order or the grouping of the pairs that the condition is tested on
    /// can be observed ([`crate::sparql::exprcache::observable`]): the query references
    /// a registered extension anywhere, or the condition holds a call with side effects
    /// or a value that differs from call to call. Such a condition is always tested
    /// after the batch, on the same pairs in the same order and batches as with
    /// `merge_pair_conditions` off.
    observable: bool,
    /// The output variables with the marker column, which every pull under a condition
    /// asks [`Merge::merged`] for. It is built on the first pull.
    staged: Vec<VarId>,
    /// Whether a pair of the left row whose output the last batch left open passed.
    passed: bool,
    _charge: Option<RetainedCharge>,
}

/// How many rows of `column` from `at` precede `key`: the current key is known to
/// precede `key` and is included in the count.
#[inline]
fn before(column: &[Id], at: usize, key: Id) -> usize {
    gallop(column, at + 1, |id| *id < key) - at
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
                spooled: None,
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
        // A query that may spill writes a run that outgrows a quarter of its room.
        let threshold = ctx
            .spill
            .is_some()
            .then(|| (ctx.memory_room() / 4).max(SPOOL_MIN_BYTES));
        let mut spool: Option<SpillWriter> = None;
        let mut spooled_rows = 0usize;
        self.hold(
            ctx,
            &mut held,
            &mut spool,
            &mut spooled_rows,
            threshold,
            range,
        )?;
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
            self.hold(
                ctx,
                &mut held,
                &mut spool,
                &mut spooled_rows,
                threshold,
                range,
            )?;
            self.at = end;
            if end < self.table().len || child.done {
                break;
            }
        }
        if let Some(writer) = spool {
            let run = writer.finish()?;
            return Ok(Run {
                owned: None,
                rows: 0..spooled_rows,
                spooled: Some(Arc::new(run)),
            });
        }
        let n = held.table.len;
        Ok(Run {
            owned: Some(held),
            rows: 0..n,
            spooled: None,
        })
    }

    /// Pass over the run of equal keys at the current row and return its length. A count
    /// needs only the length, so no row is held or spooled, however long the run.
    fn skip_run(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
        cap: usize,
        columns: &[usize],
    ) -> Result<u64> {
        let end = self.end(ctx, columns)?;
        let mut rows = (end - self.at) as u64;
        if end < self.table().len || child.done {
            self.at = end;
            return Ok(rows);
        }
        let _key_charge = ctx.charge(columns.len() as u64 * 8 + 64)?;
        let key: Vec<Id> = columns
            .iter()
            .map(|&c| self.table().cols[c][self.at])
            .collect();
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
            rows += (end - self.at) as u64;
            self.at = end;
            if end < self.table().len || child.done {
                break;
            }
        }
        Ok(rows)
    }

    /// Add `range` of the loaded batch to a run: to `held` while it fits, and to the
    /// spill file `spool` once it does not.
    fn hold(
        &self,
        ctx: &Arc<Ctx>,
        held: &mut Buffer,
        spool: &mut Option<SpillWriter>,
        spooled_rows: &mut usize,
        threshold: Option<u64>,
        range: Range<usize>,
    ) -> Result<()> {
        let table = self.table();
        if spool.is_none() {
            let over = threshold.filter(|&threshold| {
                held.charge
                    .bytes()
                    .saturating_add(range.len() as u64 * table.width() as u64 * 8)
                    > threshold
            });
            let held_now = match over {
                Some(threshold) => Err(ctx.memory_exceeded(threshold)),
                None => append(ctx, held, table, range.clone()),
            };
            match held_now {
                Ok(()) => return Ok(()),
                Err(error) if super::binary::spills(ctx, &error) => {
                    let query = ctx.spill.clone().expect("a query that may spill");
                    let mut writer = query.create(SpillKind::JoinPartition, table.width())?;
                    // Frames that a quarter of the run's room holds a few of.
                    let room = threshold.unwrap_or(SPOOL_MIN_BYTES);
                    writer.set_frame_rows(
                        ((room / 4) / (table.width().max(1) as u64 * 16)).max(64) as usize,
                    );
                    let slices: Vec<&[Id]> = held.table.cols.iter().map(Vec::as_slice).collect();
                    writer.write(&slices, held.table.len)?;
                    *spooled_rows += held.table.len;
                    for column in &mut held.table.cols {
                        *column = Vec::new();
                    }
                    held.table.len = 0;
                    held.reconcile()?;
                    *spool = Some(writer);
                }
                Err(error) => return Err(error),
            }
        }
        ctx.check()?;
        let writer = spool.as_mut().expect("a spooled run");
        let slices: Vec<&[Id]> = table.cols.iter().map(|c| &c[range.clone()]).collect();
        writer.write(&slices, range.len())?;
        *spooled_rows += range.len();
        Ok(())
    }
}

/// The least room a run may take before it is spooled to disk.
const SPOOL_MIN_BYTES: u64 = 16 << 10;

/// One run of a spooled pair: its rows in memory, or a spill file read frame by frame.
enum Side {
    Rows(Buffer),
    File {
        run: Arc<SpillRun>,
        reader: Option<SpillReader>,
        frame: Vec<Vec<Id>>,
    },
}

impl Side {
    fn new(ctx: &Arc<Ctx>, run: Run, input: &Input) -> Result<Self> {
        Ok(match run {
            Run {
                spooled: Some(run), ..
            } => Side::File {
                run,
                reader: None,
                frame: Vec::new(),
            },
            Run {
                owned: Some(held), ..
            } => Side::Rows(held),
            Run { rows, .. } => {
                let mut held = Buffer::new(ctx, &input.table().vars, rows.len())?;
                append(ctx, &mut held, input.table(), rows)?;
                Side::Rows(held)
            }
        })
    }

    /// Start reading the run again from its first row.
    fn restart(&mut self) -> Result<()> {
        if let Side::File { run, reader, frame } = self {
            *reader = Some(run.reader()?);
            frame.clear();
        }
        Ok(())
    }

    /// The next frame's length, or `None` at the end. A run in memory is one frame.
    fn next_frame(&mut self, ctx: &Ctx, started: bool) -> Result<Option<usize>> {
        match self {
            Side::Rows(held) => Ok((!started).then_some(held.table.len)),
            Side::File { reader, frame, .. } => {
                ctx.check()?;
                // The consumed frame and its column headers go before the next is
                // decoded, so one decoded frame and its encoded payload are live at a
                // time.
                drop(std::mem::take(frame));
                match reader.as_mut().expect("a started run").next_frame()? {
                    Some((columns, n)) => {
                        *frame = columns;
                        Ok(Some(n))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    fn cols(&self) -> &[Vec<Id>] {
        match self {
            Side::Rows(held) => &held.table.cols,
            Side::File { frame, .. } => frame,
        }
    }
}

/// Two runs of equal keys, at least one of them on disk, joined left row by left row:
/// every left row meets the whole right run, which a file reads again for each.
struct SpooledPair {
    left: Side,
    right: Side,
    /// The current left frame's length, the left row in it, and whether a frame was
    /// read.
    llen: usize,
    li: usize,
    lstarted: bool,
    rlen: usize,
    ri: usize,
    rstarted: bool,
    matched: bool,
    done: bool,
    _charge: Option<RetainedCharge>,
}

impl SpooledPair {
    fn new(
        ctx: &Arc<Ctx>,
        left: Run,
        right: Run,
        inputs: &[Input; 2],
        widths: [usize; 2],
    ) -> Result<Self> {
        let mut left = Side::new(ctx, left, &inputs[0])?;
        let mut right = Side::new(ctx, right, &inputs[1])?;
        left.restart()?;
        right.restart()?;
        // A frame of each file, decoded and as read, with the decoded frame's column
        // headers and the decoder's scratch for one column.
        let frame = |side: &Side, width: usize| match side {
            Side::File { run, .. } => {
                run.max_frame_rows() as u64 * (width as u64 * 8 * 2 + 8) + width as u64 * 24
            }
            Side::Rows(_) => 0,
        };
        let charge =
            ctx.retained_charge(frame(&left, widths[0]) + frame(&right, widths[1]) + 1024)?;
        Ok(Self {
            left,
            right,
            llen: 0,
            li: 0,
            lstarted: false,
            rlen: 0,
            ri: 0,
            rstarted: false,
            matched: false,
            done: false,
            _charge: charge,
        })
    }

    /// Append at most `cap` rows of output in total to `out`, ending the pair when its
    /// left run is spent.
    fn fill(
        &mut self,
        ctx: &Ctx,
        columns: &[(Option<usize>, Option<usize>)],
        optional: bool,
        extend: bool,
        out: &mut Buffer,
        cap: usize,
    ) -> Result<()> {
        let mut steps = 0usize;
        while out.table.len < cap {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            if self.li >= self.llen {
                match self.left.next_frame(ctx, self.lstarted)? {
                    Some(n) => {
                        self.lstarted = true;
                        self.llen = n;
                        self.li = 0;
                        continue;
                    }
                    None => {
                        self.done = true;
                        break;
                    }
                }
            }
            if self.ri >= self.rlen {
                if let Some(n) = self.right.next_frame(ctx, self.rstarted)? {
                    self.rstarted = true;
                    self.rlen = n;
                    self.ri = 0;
                    continue;
                }
                // The right run is spent for this left row.
                let left = self.left.cols();
                if optional && (extend || !self.matched) {
                    for (col, &(l, _)) in out.table.cols.iter_mut().zip(columns) {
                        col.push(l.map_or(Id::UNDEF, |c| left[c][self.li]));
                    }
                    out.table.len += 1;
                }
                self.li += 1;
                self.matched = false;
                self.right.restart()?;
                self.rstarted = false;
                self.rlen = 0;
                self.ri = 0;
                continue;
            }
            let (left, right) = (self.left.cols(), self.right.cols());
            let (li, ri) = (self.li, self.ri);
            let compatible = columns.iter().all(|&(l, r)| match (l, r) {
                (Some(l), Some(r)) => {
                    let (l, r) = (left[l][li], right[r][ri]);
                    l == r || l.is_undef() || r.is_undef()
                }
                _ => true,
            });
            if compatible {
                self.matched = true;
                for (col, &(l, r)) in out.table.cols.iter_mut().zip(columns) {
                    let value = l.map_or(Id::UNDEF, |c| left[c][li]);
                    col.push(if value.is_undef() {
                        r.map_or(value, |c| right[c][ri])
                    } else {
                        value
                    });
                }
                out.table.len += 1;
            }
            self.ri += 1;
        }
        Ok(())
    }
}

/// The pairs `lrows` and `rrows` of a merge, with an extension after the pairs of every
/// left row that has any, for the condition of an OPTIONAL to choose from.
fn with_extensions(lrows: &[u32], rrows: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut l = Vec::with_capacity(lrows.len() * 2);
    let mut r = Vec::with_capacity(lrows.len() * 2);
    for (k, (&i, &j)) in lrows.iter().zip(rrows).enumerate() {
        l.push(i);
        r.push(j);
        if j != NO_ROW && lrows.get(k + 1) != Some(&i) {
            l.push(i);
            r.push(NO_ROW);
        }
    }
    (l, r)
}

/// What [`Merge::interior`] did with the loaded batches.
enum Step {
    /// It decided left rows or passed over right rows.
    Advanced,
    /// It decided nothing, and the general path takes the next run.
    Stuck,
    /// The next left row's pairs do not fit the rest of the batch, which ends here.
    Full,
}

/// The scratch bytes that [`Merge::interior`] takes for each pair it may decide, for
/// output rows of `width` variables without the marker: the lower bound of a left row
/// and the two row numbers of the pair, and the pair's row of the table that the
/// condition reads, with its verdict. The size of a batch and the charge both come from
/// this expression, so the batch leaves the room that the charge takes.
fn decide_row(width: usize) -> u64 {
    12 + width as u64 * 8 + 1
}

/// The scratch bytes that deciding takes whatever the number of pairs: the slack of the
/// bounds and pairs, the column headers and variables of the pair table, and the
/// variable map that evaluating the condition builds.
fn decide_fixed(ctx: &Ctx, width: usize) -> u64 {
    128 + width as u64 * 28 + ctx.nvars() as u64 * 16 + 256
}

/// Candidates scanned between cancellation and deadline checks in the deciding path.
const DECIDE_POLL: usize = 1024;

/// Test the condition of an OPTIONAL on the pairs `lrows` and `rrows` that the merge
/// kernel gave for whole left rows, before any output row is copied: the pairs that pass
/// stay, and a left row none of whose pairs passed stays once with no right row, as in
/// the eager merge OPTIONAL. The pairs are rows of `left` and `right` from `li` and
/// `ri`, and `vars` are the output variables that `columns` fill, without the marker.
/// The caller holds the charge for the scratch, [`decide_row`] for each pair and
/// [`decide_fixed`].
fn decide_pairs(
    ctx: &Ctx,
    condition: &Expr,
    vars: &[VarId],
    columns: &[(Option<usize>, Option<usize>)],
    (left, right, li, ri): (&Table, &Table, usize, usize),
    lrows: &mut Vec<u32>,
    rrows: &mut Vec<u32>,
) -> Result<()> {
    ctx.check()?;
    let mut pairs = 0usize;
    for chunk in rrows.chunks(DECIDE_POLL) {
        pairs += chunk.iter().filter(|j| **j != NO_ROW).count();
        ctx.check()?;
    }
    if pairs == 0 {
        ctx.check()?;
        return Ok(());
    }
    let mut matched = Table::new(vars.to_vec());
    for (to, &(l, r)) in matched.cols.iter_mut().zip(columns) {
        to.reserve_exact(pairs);
        match (l, r) {
            (Some(c), _) => {
                let source = &left.cols[c];
                for (lc, rc) in lrows.chunks(DECIDE_POLL).zip(rrows.chunks(DECIDE_POLL)) {
                    let rows = lc.iter().zip(rc.iter()).filter(|(_, j)| **j != NO_ROW);
                    to.extend(rows.map(|(&i, _)| source[li + i as usize]));
                    ctx.check()?;
                }
            }
            (None, Some(c)) => {
                let source = &right.cols[c];
                for rc in rrows.chunks(DECIDE_POLL) {
                    let rows = rc.iter().filter(|j| **j != NO_ROW);
                    to.extend(rows.map(|&j| source[ri + j as usize]));
                    ctx.check()?;
                }
            }
            (None, None) => {
                let mut left_pairs = pairs;
                while left_pairs > 0 {
                    let step = left_pairs.min(DECIDE_POLL);
                    to.extend(std::iter::repeat_n(Id::UNDEF, step));
                    left_pairs -= step;
                    ctx.check()?;
                }
            }
        }
    }
    matched.len = pairs;
    let pass = super::super::exec::condition_mask(ctx, &matched, condition)?;
    drop(matched);
    ctx.check()?;
    let mut verdicts = pass.into_iter();
    // The rows kept are no more than the rows read, so they are written in place.
    let (mut w, mut k) = (0, 0);
    while k < lrows.len() {
        let row = lrows[k];
        let start = w;
        while k < lrows.len() && lrows[k] == row {
            if rrows[k] != NO_ROW && verdicts.next().unwrap_or(false) {
                lrows[w] = row;
                rrows[w] = rrows[k];
                w += 1;
            }
            k += 1;
            if k.is_multiple_of(DECIDE_POLL) {
                ctx.check()?;
            }
        }
        if w == start {
            lrows[w] = row;
            rrows[w] = NO_ROW;
            w += 1;
        }
    }
    lrows.truncate(w);
    rrows.truncate(w);
    ctx.check()?;
    Ok(())
}

/// Append the rows of `src` whose `keep` is set to `out`, writing every row and
/// advancing by the flag, which costs no branch per row.
pub(super) fn compact(out: &mut Vec<Id>, src: &[Id], keep: &[bool]) {
    let base = out.len();
    out.resize(base + src.len(), Id::UNDEF);
    let dst = &mut out[base..];
    let mut w = 0;
    for (&value, &k) in src.iter().zip(keep) {
        dst[w] = value;
        w += k as usize;
    }
    out.truncate(base + w);
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

/// Where each of `vars` is among `of`. By a map when both are long, since a join near the
/// root of a large plan has thousands of variables.
fn positions(vars: &[VarId], of: &[VarId]) -> Vec<Option<usize>> {
    if vars.len() > 32 && of.len() > 32 {
        let at: rustc_hash::FxHashMap<VarId, usize> =
            of.iter().enumerate().rev().map(|(i, &v)| (v, i)).collect();
        vars.iter().map(|v| at.get(v).copied()).collect()
    } else {
        vars.iter()
            .map(|v| of.iter().position(|x| x == v))
            .collect()
    }
}

impl Merge {
    /// The merge takes the condition of an OPTIONAL out of `node`, which the operator
    /// does not keep, so building it copies no expression.
    pub(super) fn new(ctx: &Ctx, node: &mut Node) -> Result<Self> {
        let keys = [key(ctx, node).expect("eligible merge key")];
        let charge =
            ctx.retained_charge(node.vars.len() as u64 * 64 + keys.len() as u64 * 32 + 1024)?;
        let (left, right) = (
            positions(&node.vars, &node.children[0].vars),
            positions(&node.vars, &node.children[1].vars),
        );
        let mut columns: Vec<(Option<usize>, Option<usize>)> =
            left.into_iter().zip(right).collect();
        let condition = match &mut node.kind {
            Kind::LeftJoin { expr } => expr.take().map(Arc::new),
            _ => None,
        };
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
        if condition.is_some() {
            // The right key, which the right input binds on every row, marks the pairs.
            let marker = node.children[1].vars.iter().position(|v| *v == keys[0]);
            columns.push((None, Some(marker.expect("certain merge key"))));
        }
        Ok(Self {
            key: keys[0],
            inputs: Default::default(),
            widths: [node.children[0].vars.len(), node.children[1].vars.len()],
            keys: [key_columns(0), key_columns(1)],
            columns,
            pair: None,
            spooled: None,
            spilled: 0,
            spooled_runs: 0,
            optional: matches!(node.kind, Kind::LeftJoin { .. }),
            filter: match node.kind {
                Kind::Minus => Some(false),
                Kind::HalfJoin { anti } => Some(!anti),
                _ => None,
            },
            narrowed: false,
            observable: condition
                .as_deref()
                .is_some_and(|e| crate::sparql::exprcache::observable(ctx, e).is_some()),
            condition,
            staged: Vec::new(),
            passed: false,
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
            if !more {
                // The right input is exhausted, so the rest of the left rows pass.
                let end = lk.len().min(li.saturating_add(cap - passed));
                if let Some(out) = out.as_deref_mut() {
                    for (column, &(l, _)) in out.table.cols.iter_mut().zip(&self.columns) {
                        let l = l.expect("filtering joins output left columns");
                        column.extend_from_slice(&table.cols[l][li..end]);
                    }
                    out.table.len += end - li;
                }
                passed += end - li;
                li = end;
            } else if ri < rk.len() {
                // The loaded right batch decides every left row up to its last key.
                // Their lower bounds in it come from the branch-free zipper of eager
                // MINUS, because a compare per row mispredicts on interleaved keys.
                let last = rk[rk.len() - 1];
                let covered = li + lk[li..].partition_point(|k| *k <= last);
                let end = covered.min(li.saturating_add(cap - passed));
                if end > li {
                    let _scratch = ctx.charge((end - li) as u64 * 5 + 64)?;
                    let first = kernel::bounds(ctx, &lk[li..end], &rk[ri..])?;
                    // Which left rows pass, decided without a branch per row, since
                    // interleaved keys make the outcome unpredictable.
                    let keep = kernel::filter(&lk[li..end], &rk[ri..], &first, keep_matched);
                    let n = keep.iter().filter(|&&k| k).count();
                    if let Some(out) = out.as_deref_mut() {
                        for (column, &(l, _)) in out.table.cols.iter_mut().zip(&self.columns) {
                            let l = l.expect("filtering joins output left columns");
                            compact(column, &table.cols[l][li..end], &keep);
                        }
                        out.table.len += n;
                    }
                    passed += n;
                    // Later left keys are not less than the last one decided here.
                    ri += first[first.len() - 1] as usize;
                    li = end;
                }
                if li == covered && covered < lk.len() {
                    // Every remaining left key follows this right batch.
                    ri = rk.len();
                }
            }
            self.inputs[0].at = li;
            if more {
                self.inputs[1].at = ri;
            }
        }
        Ok(passed)
    }

    /// The variable the inputs merge on.
    pub(super) fn key(&self) -> VarId {
        self.key
    }

    /// Whether the key is the only shared variable, so that every pair of a key's runs
    /// is compatible.
    fn key_only(&self) -> bool {
        self.columns
            .iter()
            .filter(|(l, r)| l.is_some() && r.is_some())
            .count()
            == 1
    }

    /// Whether a parent may count the output with [`Merge::count`], which takes the key
    /// alone and so cannot test an OPTIONAL condition.
    pub(super) fn supports_count(&self) -> bool {
        self.key_only() && self.condition.is_none()
    }

    /// Join the left rows whose right run lies wholly inside the loaded right batch,
    /// when the certainly-bound merge key is the only shared variable. Every pair of a
    /// key's runs is then compatible, so a left row joins the right run of its key
    /// whichever left batch it is in. The lower bounds of the left keys come from the
    /// branch-free zipper, and the output columns are gathered from the pairs one column
    /// at a time, as eager merge joins do. A left row is decided when its key is below
    /// the last key of the right batch, or when the right input has no more batches.
    /// Runs at the end of the right batch, and a run whose product does not fit the
    /// output, are left to the resumable general path.
    fn interior(
        &mut self,
        ctx: &Ctx,
        out: &mut Buffer,
        cap: usize,
        right_done: bool,
        decide: bool,
    ) -> Result<Step> {
        let (left, right) = (self.inputs[0].table(), self.inputs[1].table());
        let (lk, rk) = (&left.cols[self.keys[0][0]], &right.cols[self.keys[1][0]]);
        let (li, ri) = (self.inputs[0].at, self.inputs[1].at);
        let last = rk[rk.len() - 1];
        let covered = if right_done {
            lk.len()
        } else {
            li + lk[li..].partition_point(|k| *k < last)
        };
        let extend = self.condition.is_some();
        // With `decide` the condition is tested on the pairs here, before any output
        // row is copied, so a left row takes no more output rows than it has pairs.
        // Otherwise every left row also takes its extension for [`Merge::test`] to
        // choose from, which at most doubles its output.
        let mut room = (cap - out.table.len) >> usize::from(extend && !decide);
        let width = out.table.width().saturating_sub(1);
        if decide {
            // The batch began with room for the scratch of every pair, but a run held
            // or an input batch loaded since may have taken it. No more pairs are
            // decided than the room left holds; when none fit and the batch already
            // holds rows, flush those rows rather than staging pairs into a tight
            // batch, and leave an empty batch to the general path.
            let fit =
                ctx.memory_room().saturating_sub(decide_fixed(ctx, width)) / decide_row(width);
            room = room.min(usize::try_from(fit).unwrap_or(usize::MAX));
        }
        let end = covered.min(li.saturating_add(room));
        let (mut next_li, mut next_ri) = (li, ri);
        let mut full = decide && out.table.len > 0 && covered > li && room == 0;
        if end > li && room > 0 {
            let scratch = if decide {
                // the bounds, the pairs and what [`decide_pairs`] builds from them
                room as u64 * decide_row(width) + decide_fixed(ctx, width)
            } else {
                // the bounds and the pairs, and for [`Merge::settle`] the pairs with
                // extensions
                let pairs = if extend { 24 } else { 8 };
                (end - li) as u64 * 4 + room as u64 * pairs + 128
            };
            let _scratch = ctx.charge(scratch)?;
            let first = kernel::bounds(ctx, &lk[li..end], &rk[ri..])?;
            let mut lrows: Vec<u32> = Vec::with_capacity(room);
            let mut rrows: Vec<u32> = Vec::with_capacity(room);
            let done = kernel::expand(
                ctx,
                &lk[li..end],
                &rk[ri..],
                &first,
                0,
                self.optional,
                room,
                |_, _| true,
                &mut lrows,
                &mut rrows,
            )?;
            if decide {
                // A run that does not fit the rest of a batch that already has rows
                // ends the batch, so that the next one takes it here and not through a
                // pair of held runs, whose leftover would make the batch after it short.
                full = done == 0 && out.table.len > 0;
                if let Some(condition) = self.condition.as_deref()
                    && done > 0
                {
                    let vars = &out.table.vars[..out.table.vars.len() - 1];
                    let sides = (left, right, li, ri);
                    decide_pairs(
                        ctx,
                        condition,
                        vars,
                        &self.columns,
                        sides,
                        &mut lrows,
                        &mut rrows,
                    )?;
                }
            } else if extend && done > 0 {
                (lrows, rrows) = with_extensions(&lrows, &rrows);
            }
            if done > 0 {
                if decide {
                    ctx.check()?;
                    let marker = self.columns.len() - 1;
                    for (at, (column, &(l, r))) in
                        out.table.cols.iter_mut().zip(&self.columns).enumerate()
                    {
                        if at == marker {
                            // Decided rows carry no marker, so the later test keeps them as
                            // it keeps the extension of a left row none of whose pairs passed.
                            let mut left_rows = lrows.len();
                            while left_rows > 0 {
                                let step = left_rows.min(DECIDE_POLL);
                                column.extend(std::iter::repeat_n(Id::UNDEF, step));
                                left_rows -= step;
                                ctx.check()?;
                            }
                            continue;
                        }
                        match (l, r) {
                            (Some(c), _) => {
                                let source = &left.cols[c];
                                for chunk in lrows.chunks(DECIDE_POLL) {
                                    column.extend(chunk.iter().map(|&i| source[li + i as usize]));
                                    ctx.check()?;
                                }
                            }
                            (None, Some(c)) => {
                                let source = &right.cols[c];
                                for chunk in rrows.chunks(DECIDE_POLL) {
                                    column.extend(chunk.iter().map(|&j| {
                                        if j == NO_ROW {
                                            Id::UNDEF
                                        } else {
                                            source[ri + j as usize]
                                        }
                                    }));
                                    ctx.check()?;
                                }
                            }
                            (None, None) => {
                                let mut left_rows = lrows.len();
                                while left_rows > 0 {
                                    let step = left_rows.min(DECIDE_POLL);
                                    column.extend(std::iter::repeat_n(Id::UNDEF, step));
                                    left_rows -= step;
                                    ctx.check()?;
                                }
                            }
                        }
                    }
                    ctx.check()?;
                } else {
                    for (column, &(l, r)) in out.table.cols.iter_mut().zip(&self.columns) {
                        match (l, r) {
                            (Some(c), _) => {
                                let source = &left.cols[c];
                                column.extend(lrows.iter().map(|&i| source[li + i as usize]));
                            }
                            (None, Some(c)) => {
                                let source = &right.cols[c];
                                column.extend(rrows.iter().map(|&j| {
                                    if j == NO_ROW {
                                        Id::UNDEF
                                    } else {
                                        source[ri + j as usize]
                                    }
                                }));
                            }
                            (None, None) => {
                                column.extend(std::iter::repeat_n(Id::UNDEF, lrows.len()));
                            }
                        }
                    }
                }
                out.table.len += lrows.len();
                next_li = li + done;
                // Later left keys are not less than the last one decided here.
                next_ri = ri + first[done - 1] as usize;
            }
        }
        if !right_done && next_li == covered && covered < lk.len() {
            // The remaining left keys are at least the right batch's last key, so the
            // right rows below the next left key cannot match.
            next_ri += rk[next_ri..].partition_point(|k| *k < lk[covered]);
        }
        self.inputs[0].at = next_li;
        self.inputs[1].at = next_ri;
        Ok(if (li, ri) != (next_li, next_ri) {
            Step::Advanced
        } else if full {
            Step::Full
        } else {
            Step::Stuck
        })
    }

    /// Count compatible run multiplicities without constructing join output.
    /// The first key is bound on both sides and is the only shared variable.
    pub(super) fn count(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        limit_rows: bool,
    ) -> Result<u64> {
        debug_assert!(self.supports_count());
        // The pairs a count over a join counts are rows no eager operator holds, so
        // the row limit applies to them only when the count stands for a join's rows.
        let check_rows = |n: usize| match limit_rows {
            true => ctx.check_rows(n),
            false => Ok(()),
        };
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
            check_rows(count)?;
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
                // Count the left rows whose right run lies wholly inside the loaded right
                // batch without a branch per row: their lower bounds come from the
                // branch-free zipper, and each adds its run's length when the key is
                // there and, in an OPTIONAL join, one when it is not. The branches of a
                // compare per row mispredict when the two sides' keys interleave. Rows at
                // the end of the right batch are left for the general step below.
                let right_done = children[1].done;
                let [left, right] = &mut self.inputs;
                let (lc, rc) = (
                    &left.table().cols[self.keys[0][0]],
                    &right.table().cols[self.keys[1][0]],
                );
                let (mut li, mut ri) = (left.at, right.at);
                let last = rc[rc.len() - 1];
                let covered = if right_done {
                    lc.len()
                } else {
                    li + lc[li..].partition_point(|k| *k < last)
                };
                if covered > li {
                    let rkeys = &rc[ri..];
                    // the lower bounds; the kernel charges the run lengths it counts with
                    let _scratch = ctx.charge((covered - li) as u64 * 4 + 128)?;
                    let first = kernel::bounds(ctx, &lc[li..covered], rkeys)?;
                    let n = kernel::count(ctx, &lc[li..covered], rkeys, &first, self.optional)?;
                    count = count
                        .checked_add(n)
                        .ok_or_else(|| Error::invalid("join count overflow"))?;
                    ri += first[first.len() - 1] as usize;
                    li = covered;
                }
                if !right_done && li == covered && covered < lc.len() {
                    // The remaining left keys are at least the right batch's last key.
                    ri += rc[ri..].partition_point(|k| *k < lc[covered]);
                }
                if li != left.at || ri != right.at {
                    left.at = li;
                    right.at = ri;
                    check_rows(
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
                            check_rows(
                                usize::try_from(count)
                                    .map_err(|_| Error::invalid("join row count overflow"))?,
                            )?;
                            continue;
                        }
                        // Only the runs' lengths count, so neither run is held.
                        let left = self.inputs[0].skip_run(
                            ctx,
                            &mut children[0],
                            options,
                            options.batch_rows,
                            &self.keys[0],
                        )?;
                        let right = self.inputs[1].skip_run(
                            ctx,
                            &mut children[1],
                            options,
                            options.batch_rows,
                            &self.keys[1],
                        )?;
                        let n = left
                            .checked_mul(right)
                            .ok_or_else(|| Error::invalid("join count overflow"))?;
                        count = count
                            .checked_add(n)
                            .ok_or_else(|| Error::invalid("join count overflow"))?;
                    }
                }
            }
            check_rows(
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
        // An Arc, so a pull copies no expression.
        let Some(condition) = self.condition.clone() else {
            return self.merged(ctx, children, options, vars, cap, None);
        };
        if self.staged.len() != vars.len() + 1 {
            // The staged schema is built once per operator and kept beside its state.
            self.staged = vars.iter().copied().chain([VarId::MAX]).collect();
        }
        // Taken out for the pull, since `merged` borrows the operator mutably.
        let staged = std::mem::take(&mut self.staged);
        let result = loop {
            let mut out = match self.merged(ctx, children, options, &staged, cap, Some(&condition))
            {
                Ok(Some(out)) => out,
                other => break other,
            };
            if let Err(e) = self.test(ctx, &mut out, &condition) {
                break Err(e);
            }
            if !out.table.is_empty() {
                break Ok(Some(out));
            }
        };
        self.staged = staged;
        result
    }

    /// Test the condition on the staged pairs of a batch that [`Merge::merged`] produced
    /// under a condition, keep the pairs that pass and the extension of each left row
    /// none of whose pairs passed, and drop the marker column. A left row's output ends
    /// with its extension, so a left row whose pairs fill one batch is decided in a
    /// later one.
    fn test(&mut self, ctx: &Ctx, out: &mut Buffer, condition: &Expr) -> Result<()> {
        self.settle(ctx, &mut out.table, condition)?;
        // The marker is freed before the batch's charge falls to the columns it keeps.
        drop(out.table.cols.pop());
        out.table.vars.pop();
        out.reconcile()
    }

    /// Test the condition on the pairs that `table` stages, which its last column, the
    /// marker, binds, and keep the rows as [`Merge::test`] describes.
    fn settle(&mut self, ctx: &Ctx, table: &mut Table, condition: &Expr) -> Result<()> {
        let rows = table.len;
        let width = table.width() - 1;
        let (marker, cols) = table.cols.split_last_mut().expect("a marker column");
        let pairs = marker.iter().filter(|id| !id.is_undef()).count();
        if pairs == 0 && !self.passed && ctx.opt.merge_pair_conditions {
            // Every row is the extension of a left row with no pair to test, or a row
            // that [`Merge::interior`] decided, and no left row is open with a pair
            // that passed, so every row stays. With the switch off the test reserves
            // its scratch for such a batch too, as it did before the switch.
            return Ok(());
        }
        // the merged pairs with their column headers and variables, their verdicts, the
        // rows kept and the variable map that evaluating the condition builds
        let _scratch = ctx.charge(
            pairs as u64 * (width as u64 * 8 + 1)
                + rows as u64
                + width as u64 * 28
                + ctx.nvars() as u64 * 16
                + 256,
        )?;
        let mut merged = Table::new(table.vars[..width].to_vec());
        for (to, from) in merged.cols.iter_mut().zip(cols.iter()) {
            to.reserve_exact(pairs);
            to.extend(
                from.iter()
                    .zip(marker.iter())
                    .filter(|(_, m)| !m.is_undef())
                    .map(|(id, _)| *id),
            );
        }
        merged.len = pairs;
        let pass = super::super::exec::condition_mask(ctx, &merged, condition)?;
        drop(merged);
        let mut keep = Vec::with_capacity(rows);
        let mut verdicts = pass.into_iter();
        for m in marker.iter() {
            if m.is_undef() {
                keep.push(!self.passed);
                self.passed = false;
            } else {
                let pass = verdicts.next().unwrap_or(false);
                self.passed |= pass;
                keep.push(pass);
            }
        }
        for column in cols.iter_mut() {
            let mut w = 0;
            for i in 0..rows {
                column[w] = column[i];
                w += keep[i] as usize;
            }
            column.truncate(w);
        }
        table.len = keep.iter().filter(|k| **k).count();
        Ok(())
    }

    /// The output of the merge, which under a condition holds every left row's
    /// extension and the marker column that [`Merge::test`] reads.
    fn merged(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
        condition: Option<&Expr>,
    ) -> Result<Option<Buffer>> {
        let extend = condition.is_some();
        let pull = cap;
        if self.filter.is_some() && !self.narrowed {
            // The output takes only left columns, so the right input needs its key alone.
            let child = &mut children[1];
            let key = child.vars[self.keys[1][0]];
            child.restrict_count_scan(ctx, key);
            self.keys[1][0] = child.vars.iter().position(|v| *v == key).unwrap();
            self.narrowed = true;
        }
        // Initialize children before reserving an empty ancestor output. A batch
        // cap is an upper bound, and a few loaded rows need not hold that capacity.
        let cap = if self.spooled.is_some() {
            cap
        } else if let Some(pair) = &self.pair {
            cap.min(
                (pair.left.rows.end - pair.li)
                    .saturating_mul(pair.right.rows.len().max(1) + usize::from(extend))
                    .max(1),
            )
        } else {
            if !self.inputs[0].ensure(ctx, &mut children[0], options, pull)? {
                return Ok(None);
            }
            let right = self.inputs[1].ensure(ctx, &mut children[1], options, pull)?;
            if !right && !(self.optional || self.filter == Some(false)) {
                return Ok(None);
            }
            let left_rows = self.inputs[0].table().len - self.inputs[0].at;
            let rows = if self.filter.is_some() || !right {
                left_rows
            } else {
                left_rows.saturating_mul(
                    self.inputs[1].table().len - self.inputs[1].at + usize::from(extend),
                )
            };
            cap.min(rows.max(1))
        };
        let room = ctx.memory_room();
        // The rows that the budget holds with these bytes of scratch for each row and
        // for the batch.
        let fits = |scratch_row: u64, scratch_fixed: u64| {
            let fixed = (vars.len() as u64 * 40)
                .saturating_add(128)
                .saturating_add(scratch_fixed);
            let row = super::super::ctx::table_bytes(1, vars.len()).saturating_add(scratch_row);
            usize::try_from(room.saturating_sub(fixed) / row).unwrap_or(usize::MAX)
        };
        // Whether [`Merge::interior`] tests the condition on the pairs it decides in
        // this batch. Holding the pairs with the table that the condition reads takes
        // more scratch for a row than the test after the batch, so a budget that does
        // not hold the whole batch with that scratch gets the batch that the switch-off
        // path builds. A condition whose evaluation can be observed, which includes
        // every condition of a query that references a registered extension, keeps
        // that path at every budget, since deciding tests other pairs together and,
        // beside staged pairs, in another order.
        let decide =
            extend && ctx.opt.merge_pair_conditions && !self.observable && self.key_only() && {
                let width = vars.len() - 1;
                fits(decide_row(width), decide_fixed(ctx, width)) >= cap
            };
        let (scratch_row, scratch_fixed) = if self.filter.is_some() {
            (5, 64)
        } else if self.key_only() {
            (12, 128)
        } else {
            (0, 0)
        };
        let cap = cap.min(fits(scratch_row, scratch_fixed).max(1));
        // Under a condition a one-row batch may take a pair and its extension before the
        // test keeps one of them, so it reserves and charges room for both.
        let mut out = Buffer::new(ctx, vars, if extend { cap.max(2) } else { cap })?;
        if let Some(keep_matched) = self.filter {
            self.filter(
                ctx,
                children,
                options,
                cap,
                pull,
                keep_matched,
                Some(&mut out),
            )?;
            out.reconcile()?;
            return Ok((!out.table.is_empty()).then_some(out));
        }
        let mut staged = self.passed;
        let mut steps = 0usize;
        while out.table.len < cap {
            if steps.is_multiple_of(1024) {
                ctx.check()?;
            }
            steps += 1;
            if let Some(spooled) = &mut self.spooled {
                let before = out.table.len;
                spooled.fill(ctx, &self.columns, self.optional, extend, &mut out, cap)?;
                staged |= extend && out.table.len > before;
                if spooled.done {
                    self.spooled = None;
                }
                continue;
            }
            if self.pair.is_none() {
                // Preserve empty-left short circuit and callback behavior.
                if !self.inputs[0].ensure(ctx, &mut children[0], options, pull)? {
                    break;
                }
                if !self.inputs[1].ensure(ctx, &mut children[1], options, pull)? {
                    if !self.optional {
                        break;
                    }
                    self.unmatched(&mut out, cap, ctx)?;
                    continue;
                }
                if self.key_only() {
                    match self.interior(ctx, &mut out, cap, children[1].done, decide)? {
                        Step::Advanced => continue,
                        Step::Full => break,
                        Step::Stuck => {}
                    }
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
                    if extend && compatible {
                        // The pair, then the extension. An empty batch takes both even
                        // at a cap of one row, because the condition keeps one of them.
                        if out.table.len > 0 && cap - out.table.len < 2 {
                            break;
                        }
                        for (col, &(l, r)) in out.table.cols.iter_mut().zip(&self.columns) {
                            let value = l.map_or(Id::UNDEF, |c| left.cols[c][li]);
                            col.push(if value.is_undef() {
                                r.map_or(value, |c| right.cols[c][ri])
                            } else {
                                value
                            });
                        }
                        out.table.len += 1;
                        staged = true;
                    }
                    if compatible || self.optional {
                        for (col, &(l, r)) in out.table.cols.iter_mut().zip(&self.columns) {
                            let value = l.map_or(Id::UNDEF, |c| left.cols[c][li]);
                            col.push(if value.is_undef() && compatible && !extend {
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
                if decide && staged && out.table.len > 0 {
                    // End a deciding batch before collecting another general run when it
                    // already holds staged pairs or an open passed row.
                    break;
                }
                let left =
                    self.inputs[0].run(ctx, &mut children[0], options, pull, &self.keys[0])?;
                let right =
                    self.inputs[1].run(ctx, &mut children[1], options, pull, &self.keys[1])?;
                if left.spooled.is_some() || right.spooled.is_some() {
                    for run in [&left, &right] {
                        if let Some(file) = &run.spooled {
                            self.spilled += file.bytes();
                            self.spooled_runs += 1;
                        }
                    }
                    let spooled = SpooledPair::new(ctx, left, right, &self.inputs, self.widths)?;
                    self.spooled = Some(Box::new(spooled));
                    if decide && out.table.len > 0 {
                        // Flush already-decided rows without settlement scratch before
                        // streaming the newly spooled run.
                        break;
                    }
                    continue;
                }
                let rows = (left.rows.end - left.rows.start).saturating_mul(
                    (right.rows.end - right.rows.start).saturating_add(usize::from(extend)),
                );
                let (li, ri) = (left.rows.start, right.rows.start);
                self.pair = Some(Pair {
                    left,
                    right,
                    li,
                    ri,
                    matched: false,
                });
                if decide && out.table.len > 0 && rows > cap - out.table.len {
                    // Flush already-decided rows (which settle without scratch) before
                    // emitting a run that would stay held across [`Merge::settle`].
                    break;
                }
            }
            // With the certainly bound key as the only shared variable, every pair of
            // the two runs is compatible, so a left row joins a slice of right rows.
            let key_only = self.key_only();
            let pair = self.pair.as_mut().expect("matched runs");
            let left = pair.left.table(&self.inputs[0]);
            let right = pair.right.table(&self.inputs[1]);
            let before = out.table.len;
            while pair.li < pair.left.rows.end && out.table.len < cap {
                if steps.is_multiple_of(1024) {
                    ctx.check()?;
                }
                steps += 1;
                if pair.ri == pair.right.rows.end {
                    if self.optional && (extend || !pair.matched) {
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
            staged |= extend && out.table.len > before;
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
        context_with_spill(limit, None)
    }

    fn context_with_spill(limit: u64, spill: Option<&Arc<crate::spill::Spill>>) -> Arc<Ctx> {
        context_with(limit, spill, true)
    }

    /// A context whose merge tests an OPTIONAL condition on the pairs before it copies
    /// output rows when `pairs` is set, and on each batch after it otherwise.
    fn context_with(limit: u64, spill: Option<&Arc<crate::spill::Spill>>, pairs: bool) -> Arc<Ctx> {
        context_set(limit, spill, pairs, |_| ())
    }

    /// [`context_with`] after `set` has changed the context.
    fn context_set(
        limit: u64,
        spill: Option<&Arc<crate::spill::Spill>>,
        pairs: bool,
        set: impl FnOnce(&mut Ctx),
    ) -> Arc<Ctx> {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.opt.merge_pair_conditions = pairs;
        for name in ["key", "shared", "left", "right"] {
            ctx.var(name);
        }
        ctx.mem_limit = limit;
        ctx.spill = spill.map(|s| Arc::new(s.query(None)));
        // Where a registered scalar that a condition calls reports a failure.
        ctx.extension_failure = Some(Default::default());
        set(&mut ctx);
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_owner();
        ctx
    }

    #[test]
    fn sorted_batch_metadata_growth_is_admitted_before_replacement() {
        for rows in [0, 1] {
            let ctx = context(64 << 10);
            let mut batch = Buffer::new(&ctx, &[0, 1, 2], rows).unwrap();
            batch.set_order(&[0]).unwrap();
            assert_eq!(batch.charge.bytes(), capacity_bytes(&batch.table));
            let before = batch.charge.bytes();
            let old_capacity = batch.table.sorted.capacity();
            let replacement = 3 * 4;
            let held = ctx
                .charge(ctx.memory_remaining() - replacement + 1)
                .unwrap();
            assert!(ctx.memory_remaining() >= replacement - old_capacity as u64 * 4);
            assert!(ctx.memory_remaining() < replacement);
            assert!(matches!(
                batch.set_order(&[0, 1, 2]),
                Err(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
            ));
            assert_eq!(batch.table.sorted, [0]);
            assert_eq!(batch.table.sorted.capacity(), old_capacity);
            assert_eq!(batch.charge.bytes(), before);
            drop(held);
            batch.set_order(&[0, 1, 2]).unwrap();
            assert_eq!(batch.table.sorted, [0, 1, 2]);
            assert!(ctx.mem_peak() <= 64 << 10);
            assert_eq!(batch.charge.bytes(), capacity_bytes(&batch.table));
            batch.set_order(&[]).unwrap();
            assert!(batch.table.sorted.is_empty());
            assert_eq!(batch.charge.bytes(), capacity_bytes(&batch.table));
        }
    }

    #[test]
    fn nested_optional_chunks_preserve_matches_and_unbound_extensions() {
        let optional = |left: Node, right: Node| {
            let certain = left.certain.clone();
            let mut node = join(left, right);
            node.kind = Kind::LeftJoin { expr: None };
            node.certain = certain;
            node
        };
        let mut left: Vec<_> = (0..24).map(|i| vec![0, i]).collect();
        left.extend([vec![1, 99], vec![2, 100]]);
        let mut middle: Vec<_> = (0..24).map(|i| vec![0, i + 100]).collect();
        middle.push(vec![2, 200]);
        let right: Vec<_> = (0..24).map(|i| vec![0, i + 1000]).collect();
        let node = optional(
            values(vec![0, 1], &left),
            optional(values(vec![0, 2], &middle), values(vec![0, 3], &right)),
        );
        let expected = rows(
            &exec::execute_materialized(&context(u64::MAX), &node)
                .unwrap()
                .0
                .table,
        );
        assert_eq!(expected.len(), 24 * 24 * 24 + 2);
        for encrypted in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
                dir: dir.path().to_owned(),
                max_bytes: 1 << 30,
                query_bytes: 1 << 30,
                min_free_bytes: 0,
                encrypted,
            })
            .unwrap();
            for cap in [1, 7, 4096] {
                let ctx = context_with_spill(1 << 20, Some(&spill));
                let mut op =
                    Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization)
                        .unwrap();
                assert!(matches!(op.state, super::super::State::Merge(_)));
                assert!(matches!(
                    op.children[1].state,
                    super::super::State::Merge(_)
                ));
                let options = CursorOptions {
                    batch_rows: cap,
                    ..Default::default()
                };
                let (mut actual, mut batches) = (Vec::new(), 0);
                while let Some(batch) = op.next(&ctx, &options, cap).unwrap() {
                    assert!(batch.table.len <= cap);
                    assert!(batch.charge.bytes() >= capacity_bytes(&batch.table));
                    actual.extend(rows(&batch.table));
                    batches += 1;
                }
                actual.sort();
                assert_eq!(actual, expected, "encrypted={encrypted}, cap={cap}");
                assert!(batches > 1);
                assert!(ctx.mem_peak() <= 1 << 20);
                drop(op);
                assert_eq!(spill.reserved_bytes(), 0);
            }
        }
    }

    /// Counting a merge join needs only the lengths of its equal-key runs, so a run
    /// longer than memory is passed over rather than spooled. Before the fix both runs
    /// of the heavy key went to disk only to be counted.
    #[test]
    fn counting_long_equal_key_runs_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let side = |offset: i64| -> Vec<Vec<i64>> {
            let mut rows: Vec<Vec<i64>> = (0..4000).map(|i| vec![7, i + offset]).collect();
            rows.extend((0..50).map(|i| vec![8 + i, offset]));
            rows.insert(0, vec![1, offset]);
            rows
        };
        let node = join(
            values(vec![0, 1], &side(0)),
            values(vec![0, 2], &side(100_000)),
        );
        let expected = exec::execute_materialized(&context(u64::MAX), &node)
            .unwrap()
            .0
            .table
            .len as u64;
        assert_eq!(expected, 4000 * 4000 + 51);
        for cap in [64, 1000] {
            let ctx = context_with_spill(128 << 10, Some(&spill));
            let mut op =
                Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization).unwrap();
            let options = CursorOptions {
                batch_rows: cap,
                ..Default::default()
            };
            let before = spill.written_bytes();
            let count = op.count_input(&ctx, &options).unwrap();
            assert_eq!(count, Some(expected), "cap {cap}");
            assert_eq!(
                spill.written_bytes(),
                before,
                "cap {cap}: a counted run spilled"
            );
            assert!(ctx.mem_peak() <= 128 << 10);
            drop(op);
            drop(ctx);
            assert_eq!(spill.reserved_bytes(), 0);
        }
    }

    #[test]
    fn a_spooled_final_run_from_a_nested_merge_survives_eof() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let input: Vec<_> = (0..10_000).map(|i| vec![7, 11, i]).collect();
        for nested_on_left in [true, false] {
            let nested = join(values(vec![0, 1, 2], &input), values(vec![0], &[vec![7]]));
            let other = values(vec![0, 1], &[vec![7, 11]]);
            let widths = if nested_on_left { [3, 2] } else { [2, 3] };
            let node = if nested_on_left {
                join(nested, other)
            } else {
                join(other, nested)
            };
            let eager = context(u64::MAX);
            let expected = rows(&exec::execute_materialized(&eager, &node).unwrap().0.table);
            for cap in [32, 127] {
                let ctx = context_with_spill(128 << 10, Some(&spill));
                let mut op =
                    Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization)
                        .unwrap();
                let options = CursorOptions {
                    batch_rows: cap,
                    ..Default::default()
                };
                let mut actual = Vec::new();
                let mut checked_reservation = false;
                while let Some(batch) = op.next(&ctx, &options, cap).unwrap() {
                    actual.extend(rows(&batch.table));
                    if let super::super::State::Merge(merge) = &op.state
                        && let Some(pair) = &merge.spooled
                    {
                        let frame_bytes = |side: &Side, width: u64| match side {
                            Side::File { run, .. } => {
                                run.max_frame_rows() as u64 * (width * 16 + 8) + width * 24
                            }
                            Side::Rows(_) => 0,
                        };
                        let required = frame_bytes(&pair.left, widths[0])
                            + frame_bytes(&pair.right, widths[1])
                            + 1024;
                        assert!(
                            pair._charge.as_ref().unwrap().bytes() >= required,
                            "nested_left={nested_on_left}, cap={cap}, widths={widths:?}"
                        );
                        checked_reservation = true;
                    }
                }
                actual.sort();
                assert_eq!(actual, expected);
                assert!(spill.written_bytes() > 0);
                assert!(checked_reservation);
                drop(op);
                drop(ctx);
                assert_eq!(spill.reserved_bytes(), 0);
            }
        }
    }

    #[test]
    fn spooled_pair_reserves_the_largest_frame() {
        for encrypted in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
                dir: tmp.path().to_path_buf(),
                max_bytes: 1 << 30,
                query_bytes: 1 << 30,
                min_free_bytes: 0,
                encrypted,
            })
            .unwrap();
            let query = Arc::new(spill.query(None));
            let ctx = context(64 << 10);
            let empty = query
                .create(crate::spill::SpillKind::JoinPartition, 1)
                .unwrap()
                .finish()
                .unwrap();
            assert_eq!(empty.max_frame_rows(), 0);
            drop(empty);
            let mut writer = query
                .create(crate::spill::SpillKind::JoinPartition, 1)
                .unwrap();
            writer.set_frame_rows(64);
            let column: Vec<_> = (0..65).map(|i| Id::from_i64(i).unwrap()).collect();
            writer.write(&[&column[..64]], 64).unwrap();
            writer.set_frame_rows(1);
            writer.write(&[&column[64..]], 1).unwrap();
            let run = Arc::new(writer.finish().unwrap());
            assert_eq!(run.max_frame_rows(), 64);
            let inputs = [
                Input {
                    buffer: Some(Buffer::new(&ctx, &[0], 0).unwrap()),
                    at: 0,
                },
                Input {
                    buffer: Some(Buffer::new(&ctx, &[0], 0).unwrap()),
                    at: 0,
                },
            ];
            let left = Run {
                owned: None,
                rows: 0..65,
                spooled: Some(run),
            };
            let right = Run {
                owned: Some(Buffer::new(&ctx, &[0], 1).unwrap()),
                rows: 0..1,
                spooled: None,
            };
            let mut pair = SpooledPair::new(&ctx, left, right, &inputs, [1, 1]).unwrap();
            assert_eq!(pair.left.next_frame(&ctx, false).unwrap(), Some(64));
            assert!(
                pair._charge.as_ref().unwrap().bytes() >= 64 * 8 * 2 + 1024,
                "reservation {} does not cover the largest frame",
                pair._charge.as_ref().unwrap().bytes()
            );
            drop(pair);
            drop(inputs);
            drop(query);
            assert_eq!(spill.reserved_bytes(), 0);
        }
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
                let expected = rows(&exec::execute_materialized(&ctx, &node).unwrap().0.table);
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
                assert!(eligible(&ctx, &node));
                let expected = rows(&exec::execute_materialized(&ctx, &node).unwrap().0.table);
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
                // The branch-free count decides the same rows without building them.
                let mut node = join(values(vec![0, 2], &l), values(vec![0, 3], &r));
                if optional {
                    node.kind = Kind::LeftJoin { expr: None };
                }
                let mut op =
                    Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
                let counted = op.count_input(&ctx, &options).unwrap();
                assert_eq!(counted, Some(expected.len() as u64), "count at cap {cap}");
            }
        }
    }

    /// An OPTIONAL with a condition keeps the pairs that pass it and extends a left row
    /// none of whose pairs passed, as eager execution does. Left rows whose failing pairs
    /// span several batches, unmatched rows, a right value the condition cannot compare
    /// and a second shared variable are covered at every batch size, and a conditional
    /// merge is not counted from its keys.
    #[test]
    fn conditional_optional_merges_match_eager_at_every_batch_size() {
        let (left, right) = (2, 3);
        let mut l: Vec<Vec<i64>> = Vec::new();
        let mut r: Vec<Vec<i64>> = Vec::new();
        // Key 0 has three left rows and 40 right rows of which only the last passes the
        // first left row, and key 9 a single pair that fails.
        for i in 0..3 {
            l.push(vec![0, i % 2, [45, 70, 10][i as usize]]);
        }
        for j in 0..40 {
            r.push(vec![0, j % 2, if j == 39 { 50 } else { j }]);
        }
        for key in 1..9 {
            for i in 0..(key % 4) {
                l.push(vec![key, i % 2, (key * 7 + i * 13) % 50]);
            }
            for j in 0..(key % 3) {
                let value = if (key + j) % 5 == 0 {
                    -1
                } else {
                    (key * 11 + j * 17) % 50
                };
                r.push(vec![key, j % 2, value]);
            }
        }
        l.push(vec![9, 0, 30]);
        r.push(vec![9, 0, 10]);
        l.push(vec![10, 1, 5]);
        let condition = Expr::Cmp(
            Box::new(Expr::Var(right)),
            Box::new(Expr::Var(left)),
            crate::sparql::expr::CmpOp::Gt,
        );
        let project = |rows: &[Vec<i64>]| -> Vec<Vec<i64>> {
            rows.iter().map(|row| vec![row[0], row[2]]).collect()
        };
        for shared in [false, true] {
            let (lv, rv, lr, rr) = if shared {
                (vec![0, 1, left], vec![0, 1, right], l.clone(), r.clone())
            } else {
                (vec![0, left], vec![0, right], project(&l), project(&r))
            };
            let mut node = join(values(lv, &lr), values(rv, &rr));
            node.kind = Kind::LeftJoin {
                expr: Some(condition.clone()),
            };
            let ctx = context(1 << 20);
            assert!(eligible(&ctx, &node));
            let expected = rows(&exec::execute_materialized(&ctx, &node).unwrap().0.table);
            let passing = expected.iter().filter(|row| !row[row.len() - 1].is_undef());
            assert!(passing.count() > 0 && expected.len() > lr.len() / 2);
            for cap in [1, 2, 3, 4, 7, 4096] {
                let ctx = context(1 << 20);
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
                    assert!(batch.table.len <= cap && batch.table.len > 0);
                    assert_eq!(batch.table.width(), node.vars.len());
                    result.extend(rows(&batch.table));
                }
                result.sort();
                assert_eq!(result, expected, "shared {shared}, batch cap {cap}");
                let mut op =
                    Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization)
                        .unwrap();
                assert_eq!(op.count_input(&ctx, &options).unwrap(), None);
            }
        }
    }

    /// With the key as the only shared variable, the merge tests the condition on the
    /// pairs before it copies output rows (`merge_pair_conditions`). The rows it decides
    /// there share batches with the rows of held and spooled runs, which the test after
    /// the merge decides, and the bag is the eager one at every batch size with the
    /// switch on and off. Short batches can end at a general run boundary, when the
    /// next left row's pairs exceed the available slots, or when deciding scratch
    /// exhausts the available room.
    #[test]
    fn pair_conditions_match_the_staged_test_beside_held_and_spooled_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 34,
            query_bytes: 1 << 34,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let (left, right) = (2, 3);
        // The condition passes a pair whose right value is above its left value.
        let condition = Expr::Cmp(
            Box::new(Expr::Var(right)),
            Box::new(Expr::Var(left)),
            crate::sparql::expr::CmpOp::Gt,
        );
        let mut l: Vec<Vec<i64>> = Vec::new();
        let mut r: Vec<Vec<i64>> = Vec::new();
        for key in 0..600i64 {
            // One to three left rows, some of them equal, and no to five right rows.
            for i in 0..(1 + key % 3) {
                l.push(vec![key, (key * 7 + i / 2 * 13) % 50]);
            }
            for j in 0..(key % 6) {
                r.push(vec![key, (key * 11 + j * 17) % 60]);
            }
        }
        let fan = l.len();
        // A run that every pair of fails, one that every pair of passes, and a run of
        // left rows, each long enough to be held across batches and to spool.
        for j in 0..1500 {
            r.push(vec![700, j % 40]);
            r.push(vec![701, 100 + j]);
        }
        l.push(vec![700, 45]);
        l.push(vec![700, 20]);
        l.push(vec![701, 3]);
        for i in 0..1500 {
            l.push(vec![702, i % 50]);
        }
        r.sort_by_key(|row| row[0]);
        for j in 0..3 {
            r.push(vec![702, 20 + j]);
        }
        l.push(vec![703, 1]);
        let mut spooled = 0;
        for with_long in [false, true] {
            let lr = if with_long { &l[..] } else { &l[..fan] };
            let rr: Vec<Vec<i64>> = r
                .iter()
                .filter(|row| with_long || row[0] < 700)
                .cloned()
                .collect();
            let mut node = join(values(vec![0, left], lr), values(vec![0, right], &rr));
            node.kind = Kind::LeftJoin {
                expr: Some(condition.clone()),
            };
            let expected = rows(
                &exec::execute_materialized(&context(u64::MAX), &node)
                    .unwrap()
                    .0
                    .table,
            );
            let unextended = expected.iter().filter(|row| !row[row.len() - 1].is_undef());
            assert!(unextended.count() > lr.len() / 4 && expected.len() > lr.len());
            for (pairs, spills) in [(true, false), (false, false), (true, true), (false, true)] {
                for cap in [1, 2, 3, 4, 7, 64, 4096] {
                    if spills && cap > 64 {
                        // Two input batches of this size outgrow the small budget.
                        continue;
                    }
                    let ctx = if spills {
                        context_with(128 << 10, Some(&spill), pairs)
                    } else {
                        context_with(8 << 20, None, pairs)
                    };
                    let mut op =
                        Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization)
                            .unwrap();
                    assert!(matches!(op.state, super::super::State::Merge(_)));
                    let options = CursorOptions {
                        batch_rows: cap,
                        ..Default::default()
                    };
                    let mut result = Vec::new();
                    let mut sizes = Vec::new();
                    while let Some(batch) = op.next(&ctx, &options, cap).unwrap() {
                        assert!(batch.table.len <= cap && batch.table.len > 0);
                        assert_eq!(batch.table.width(), node.vars.len());
                        assert!(batch.charge.bytes() >= capacity_bytes(&batch.table));
                        sizes.push(batch.table.len);
                        result.extend(rows(&batch.table));
                    }
                    if let super::super::State::Merge(merge) = &op.state {
                        spooled += merge.spooled_runs;
                    }
                    // The merge keeps the left order, so only rows of one key reorder.
                    let keys: Vec<Id> = result.iter().map(|row| row[0]).collect();
                    assert!(keys.is_sorted());
                    result.sort();
                    let case = format!("long {with_long} pairs {pairs} spills {spills} cap {cap}");
                    assert_eq!(result.len(), expected.len(), "{case}");
                    assert_eq!(result, expected, "{case}");
                    if pairs && !with_long && cap >= 64 {
                        // A left row has at most five pairs. Only a run that crosses
                        // a batch of the right input, which the test after the merge
                        // decides, can leave a batch shorter than that.
                        let short = sizes[..sizes.len() - 1].iter().filter(|n| **n + 12 <= cap);
                        assert!(short.count() * 4 <= sizes.len(), "{case}: {sizes:?}");
                    }
                }
            }
        }
        assert!(spooled > 0, "{spooled} runs spooled");
        assert_eq!(spill.reserved_bytes(), 0);
    }

    #[test]
    fn pair_conditions_flush_decided_rows_before_streaming_a_new_spool() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 34,
            query_bytes: 1 << 34,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let condition = Expr::Cmp(
            Box::new(Expr::Var(2)),
            Box::new(Expr::Var(1)),
            crate::sparql::expr::CmpOp::Gt,
        );
        let right: Vec<_> = [vec![0, 1]]
            .into_iter()
            .chain((1..=4096).map(|value| vec![15, value]))
            .chain([vec![16, 1]])
            .collect();
        let mut node = join(
            values(vec![0, 1], &[vec![0, 0], vec![15, 0], vec![16, 0]]),
            values(vec![0, 2], &right),
        );
        node.kind = Kind::LeftJoin {
            expr: Some(condition.clone()),
        };
        let expected = rows(
            &exec::execute_materialized(&context(u64::MAX), &node)
                .unwrap()
                .0
                .table,
        );
        let ctx = context_with_spill(128 << 10, Some(&spill));
        let mut op =
            Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization).unwrap();
        let super::super::State::Merge(merge) = &mut op.state else {
            unreachable!()
        };
        let cap = 8;
        let options = CursorOptions {
            batch_rows: cap,
            ..Default::default()
        };
        let staged = [0, 1, 2, VarId::MAX];
        let mut first = merge
            .merged(
                &ctx,
                &mut op.children,
                &options,
                &staged,
                cap,
                Some(&condition),
            )
            .unwrap()
            .expect("decided prefix");
        assert!(merge.spooled.is_some(), "the crossing run should spool");
        assert_eq!(first.table.cols[0], [Id::from_i64(0).unwrap()]);
        assert_eq!(first.table.cols[3], [Id::UNDEF]);
        merge.test(&ctx, &mut first, &condition).unwrap();

        let mut result = rows(&first.table);
        let next = merge
            .next(&ctx, &mut op.children, &options, &node.vars, cap)
            .unwrap()
            .expect("first spooled pairs while the decided prefix is held");
        assert!(
            merge.passed,
            "the spooled left row continues in the next batch"
        );
        result.extend(rows(&next.table));
        drop(next);
        drop(first);
        while let Some(batch) = merge
            .next(&ctx, &mut op.children, &options, &node.vars, cap)
            .unwrap()
        {
            result.extend(rows(&batch.table));
        }
        result.sort();
        assert_eq!(result, expected);
        assert_eq!(spill.reserved_bytes(), 0);
    }

    /// A conditional OPTIONAL whose left rows each meet a long right run of failing pairs,
    /// and single-row runs whose pair and extension a one-row batch takes together, stays
    /// within its budget at small batch caps, charges every batch for its rows and
    /// returns batches of at most the cap after the condition.
    #[test]
    fn conditional_optional_merges_stay_within_budget_at_small_caps() {
        let (left, right) = (2, 3);
        let condition = Expr::Cmp(
            Box::new(Expr::Var(right)),
            Box::new(Expr::Var(left)),
            crate::sparql::expr::CmpOp::Gt,
        );
        for shared in [false, true] {
            let mut l: Vec<Vec<i64>> = (0..20).map(|i| vec![0, i % 2, 10_000 + i]).collect();
            let mut r: Vec<Vec<i64>> = (0..3000).map(|j| vec![0, j % 2, j]).collect();
            for key in 1..400 {
                l.push(vec![key, key % 2, key % 7]);
                r.push(vec![key, key % 2, key % 5]);
            }
            if !shared {
                for row in l.iter_mut().chain(r.iter_mut()) {
                    row.remove(1);
                }
            }
            let (lv, rv) = if shared {
                (vec![0, 1, left], vec![0, 1, right])
            } else {
                (vec![0, left], vec![0, right])
            };
            let mut node = join(values(lv, &l), values(rv, &r));
            node.kind = Kind::LeftJoin {
                expr: Some(condition.clone()),
            };
            let expected = rows(
                &exec::execute_materialized(&context(u64::MAX), &node)
                    .unwrap()
                    .0
                    .table,
            );
            assert_eq!(expected.len(), 20 + 399);
            for cap in [1, 2, 3] {
                let ctx = context(512 << 10);
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
                    assert!(batch.table.len <= cap && batch.table.len > 0);
                    assert!(batch.charge.bytes() >= capacity_bytes(&batch.table));
                    result.extend(rows(&batch.table));
                }
                result.sort();
                assert_eq!(result, expected, "shared {shared}, cap {cap}");
                assert!(ctx.mem_peak() <= 512 << 10, "shared {shared}, cap {cap}");
            }
        }
    }

    /// The rows of a conditional merge OPTIONAL at batch size `cap` under `limit` bytes,
    /// with the condition tested on the pairs when `pairs` is set, or the budget error.
    fn conditional(
        node: &Node,
        cap: usize,
        limit: u64,
        pairs: bool,
        spill: Option<&Arc<crate::spill::Spill>>,
    ) -> Result<(Vec<Vec<Id>>, u64)> {
        conditional_batches(node, cap, limit, pairs, spill).map(|(rows, runs, _)| (rows, runs))
    }

    /// [`conditional`] with the number of rows in each batch.
    fn conditional_batches(
        node: &Node,
        cap: usize,
        limit: u64,
        pairs: bool,
        spill: Option<&Arc<crate::spill::Spill>>,
    ) -> Result<(Vec<Vec<Id>>, u64, Vec<usize>)> {
        conditional_on(&context_with(limit, spill, pairs), node, cap)
    }

    /// The memory bound of `merge_pair_conditions` at batch size `cap`: an output batch
    /// with its marker column, as [`Merge::merged`] admits it, and the scratch that
    /// [`Merge::interior`] charges to decide it. A conditional merge OPTIONAL that fits a
    /// budget with the switch off fits it, or a budget at most this much larger, with the
    /// switch on.
    fn pair_bound(node: &Node, cap: usize) -> u64 {
        let ctx = context(u64::MAX);
        let (vars, rows) = (node.vars.len() as u64 + 1, cap.max(2) as u64);
        let width = node.vars.len();
        crate::sparql::ctx::table_bytes(rows as usize, vars as usize)
            + vars * 40
            + 128
            + rows * decide_row(width)
            + decide_fixed(&ctx, width)
    }

    /// The rows of a streamed merge over `node` in output order, pulled at the caps of
    /// `caps` in turn under `limit` bytes, with the condition tested on the pairs when
    /// `pairs` is set, and the length of each batch, or the error the merge failed with.
    /// With `hold` the caller keeps each batch until the next pull returns, so the batch
    /// stays charged while the merge builds the next one.
    fn pulled(
        snap: &Arc<crate::store::Snapshot>,
        node: &Node,
        caps: &[usize],
        limit: u64,
        pairs: bool,
        hold: bool,
    ) -> Result<(Vec<Vec<Id>>, Vec<usize>)> {
        let mut ctx = Ctx::new(Arc::clone(snap));
        ctx.opt.merge_pair_conditions = pairs;
        for name in ["key", "shared", "left", "right"] {
            ctx.var(name);
        }
        ctx.mem_limit = limit;
        ctx.extension_failure = Some(Default::default());
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_owner();
        let mut op = Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization)?;
        assert!(matches!(op.state, super::super::State::Merge(_)));
        let (mut out, mut sizes, mut previous) = (Vec::new(), Vec::new(), None);
        for pull in 0.. {
            let cap = caps[pull % caps.len()];
            let options = CursorOptions {
                batch_rows: cap,
                ..Default::default()
            };
            let Some(batch) = op.next(&ctx, &options, cap)? else {
                break;
            };
            assert!(batch.table.len <= cap && batch.table.len > 0);
            sizes.push(batch.table.len);
            let table = &batch.table;
            out.extend((0..table.len).map(|i| table.cols.iter().map(|c| c[i]).collect()));
            if hold {
                // The batch before this one is released only now, after this pull.
                previous = Some(batch);
            }
        }
        drop(previous);
        Ok((out, sizes))
    }

    /// [`conditional_batches`] on a context.
    fn conditional_on(
        ctx: &Arc<Ctx>,
        node: &Node,
        cap: usize,
    ) -> Result<(Vec<Vec<Id>>, u64, Vec<usize>)> {
        let mut op = Operator::build(ctx, node.clone(), FallbackPolicy::RejectMaterialization)?;
        assert!(matches!(op.state, super::super::State::Merge(_)));
        let options = CursorOptions {
            batch_rows: cap,
            ..Default::default()
        };
        let mut result = Vec::new();
        let mut sizes = Vec::new();
        while let Some(batch) = op.next(ctx, &options, cap)? {
            assert!(batch.table.len <= cap && batch.table.len > 0);
            sizes.push(batch.table.len);
            result.extend(rows(&batch.table));
        }
        let super::super::State::Merge(merge) = &op.state else {
            unreachable!()
        };
        Ok((result, merge.spooled_runs, sizes))
    }

    /// A condition that calls a registered scalar is tested after the batch whether
    /// `merge_pair_conditions` is on or off, so the scalar is called for the same pairs
    /// in the same batches, and where one thread evaluates a batch in the same sequence.
    /// The data has left rows that a merge with the switch on would decide beside held,
    /// single-row and spooled runs.
    #[test]
    fn a_condition_that_calls_a_registered_scalar_keeps_the_test_after_the_batch() {
        use crate::sparql::expr::Func;
        use crate::sparql::extensions::{ScalarContext, ScalarDescriptor, ScalarResult};
        use std::sync::Mutex;
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 34,
            query_bytes: 1 << 34,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let (left, right) = (2, 3);
        let seen: Arc<Mutex<Vec<(i64, i64)>>> = Arc::default();
        let log = seen.clone();
        let value = |term: &oxrdf::Term| match term {
            oxrdf::Term::Literal(l) => l.value().parse::<i64>().unwrap(),
            other => panic!("{other}"),
        };
        // The scalar passes a pair whose right value is above its left value.
        let descriptor = ScalarDescriptor::new(
            "urn:above",
            2..=2,
            move |_: &ScalarContext<'_>, args: &[oxrdf::Term]| -> ScalarResult {
                let (l, r) = (value(&args[0]), value(&args[1]));
                log.lock().unwrap().push((l, r));
                Ok(oxrdf::Literal::from(r > l).into())
            },
        )
        .unwrap();
        let condition = Expr::Call(
            Func::Registered(Arc::new(descriptor)),
            vec![Expr::Var(left), Expr::Var(right)],
        );
        // At four rows a batch, the run of key 5 crosses the right input's batches and is
        // held, and its three rows leave room in the batch for the pair of key 6.
        let crossing = (
            vec![vec![5, 1], vec![6, 1]],
            (0..3)
                .map(|j| vec![0, j])
                .chain([vec![5, 3], vec![5, 4], vec![6, 5], vec![7, 6], vec![8, 7]])
                .collect::<Vec<_>>(),
        );
        let mut l: Vec<Vec<i64>> = Vec::new();
        let mut r: Vec<Vec<i64>> = Vec::new();
        for key in 0..120i64 {
            for i in 0..(1 + key % 3) {
                l.push(vec![key, (key * 7 + i / 2 * 13) % 50]);
            }
            for j in 0..(key % 6) {
                r.push(vec![key, (key * 11 + j * 17) % 60]);
            }
        }
        // Runs long enough to be held across batches and to spool, between keys whose
        // rows the merge decides.
        for j in 0..1500 {
            r.push(vec![700, j % 40]);
        }
        l.push(vec![700, 45]);
        l.push(vec![700, 20]);
        for i in 0..1500 {
            l.push(vec![702, i % 50]);
        }
        for j in 0..3 {
            r.push(vec![702, 20 + j]);
        }
        for key in 703..760i64 {
            l.push(vec![key, key % 9]);
            for j in 0..(key % 4) {
                r.push(vec![key, (key + j * 5) % 12]);
            }
        }
        let mut spooled = 0;
        for (lr, rr, long) in [(crossing.0, crossing.1, false), (l, r, true)] {
            // One call for each pair of a left row and a right row of its key.
            let mut every = Vec::new();
            for row in &lr {
                every.extend(rr.iter().filter(|x| x[0] == row[0]).map(|x| (row[1], x[1])));
            }
            assert!(!every.is_empty());
            every.sort();
            let mut node = join(values(vec![0, left], &lr), values(vec![0, right], &rr));
            node.kind = Kind::LeftJoin {
                expr: Some(condition.clone()),
            };
            for spills in [false, true] {
                // A batch of up to 64 rows is evaluated on one thread. A larger one may
                // go to the pool, where the calls of a batch have no order. With the
                // switch on, a merge that decided pairs would test up to 1024 of them
                // in one call where the test after the batch has 512.
                for cap in [1, 2, 3, 4, 5, 7, 64, 1024] {
                    if spills && (!long || cap > 64) {
                        continue;
                    }
                    let mut runs_of = Vec::new();
                    for pairs in [false, true] {
                        seen.lock().unwrap().clear();
                        let (limit, spill) = if spills {
                            (128 << 10, Some(&spill))
                        } else {
                            (8 << 20, None)
                        };
                        let (mut bag, runs, sizes) =
                            conditional_batches(&node, cap, limit, pairs, spill).unwrap();
                        spooled += runs;
                        let mut calls = seen.lock().unwrap().clone();
                        if cap > 64 {
                            calls.sort();
                        }
                        bag.sort();
                        runs_of.push((calls, sizes, bag));
                    }
                    let case = format!("long {long} spills {spills} cap {cap}");
                    let (off, on) = (&runs_of[0], &runs_of[1]);
                    let mut called = off.0.clone();
                    called.sort();
                    assert_eq!(called, every, "{case}");
                    if let Some(at) =
                        (0..off.0.len().max(on.0.len())).find(|&i| off.0.get(i) != on.0.get(i))
                    {
                        panic!(
                            "{case}: call {at} was for {:?} with the switch off and {:?} with it on",
                            off.0.get(at),
                            on.0.get(at)
                        );
                    }
                    assert_eq!(off.1, on.1, "{case}: rows in each batch");
                    assert_eq!(off.2, on.2, "{case}");
                }
            }
        }
        assert!(spooled > 0, "{spooled} runs spooled");
    }

    /// The merge decides pairs for a condition without an observable call, an EXISTS
    /// included, and not for one that holds such a call beside or inside an EXISTS. At
    /// three pairs for a left row, deciding fits more left rows in a batch.
    #[test]
    fn pair_conditions_decide_only_without_an_observable_call() {
        use crate::sparql::expr::{ExistsSpec, Func};
        use spargebra::algebra::{Expression, Function, GraphPattern};
        let (left, right) = (2, 3);
        let above = Expr::Cmp(
            Box::new(Expr::Var(right)),
            Box::new(Expr::Var(left)),
            crate::sparql::expr::CmpOp::Gt,
        );
        let exists = |filter: Option<Expression>| {
            let bgp = GraphPattern::Bgp {
                patterns: Vec::new(),
            };
            let pattern = match filter {
                Some(expr) => GraphPattern::Filter {
                    expr,
                    inner: Box::new(bgp),
                },
                None => bgp,
            };
            Expr::Exists(Arc::new(ExistsSpec::new(
                pattern,
                crate::sparql::plan::ActiveGraph::Default,
                vec![left],
                Vec::new(),
            )))
        };
        let rand = Expr::Call(Func::Builtin(Function::Rand), vec![]);
        let l: Vec<Vec<i64>> = (0..400).map(|k| vec![k, 5]).collect();
        let r: Vec<Vec<i64>> = (0..1200).map(|j| vec![j / 3, j % 9]).collect();
        let node = |condition: Expr| {
            let mut node = join(values(vec![0, left], &l), values(vec![0, right], &r));
            node.kind = Kind::LeftJoin {
                expr: Some(condition),
            };
            node
        };
        let observable = |condition: Expr| {
            let ctx = context(8 << 20);
            let op = Operator::build(&ctx, node(condition), FallbackPolicy::RejectMaterialization)
                .unwrap();
            let super::super::State::Merge(merge) = &op.state else {
                unreachable!()
            };
            merge.observable
        };
        assert!(!observable(above.clone()));
        assert!(!observable(exists(None)));
        assert!(!observable(Expr::And(
            Box::new(exists(None)),
            Box::new(above.clone())
        )));
        assert!(observable(Expr::Or(
            Box::new(exists(None)),
            Box::new(rand.clone())
        )));
        assert!(observable(exists(Some(Expression::FunctionCall(
            Function::Rand,
            Vec::new()
        )))));
        // RAND() < 2 holds for every pair, so the bag is the same on every path.
        let always = Expr::Cmp(
            Box::new(rand),
            Box::new(Expr::Const(Id::from_i64(2).unwrap())),
            crate::sparql::expr::CmpOp::Lt,
        );
        // A query that references a registration keeps the test after the batch for a
        // condition that calls nothing, since the flag is the registry's answer for the
        // whole query pattern.
        let flagged = |pairs: bool| {
            let ctx = context_set(8 << 20, None, pairs, |ctx| ctx.calls_extensions = true);
            let (mut bag, _, sizes) = conditional_on(&ctx, &node(above.clone()), 64).unwrap();
            bag.sort();
            (bag, sizes)
        };
        assert_eq!(flagged(false), flagged(true));
        for (name, condition, decides) in [("pure", above, true), ("RAND", always, false)] {
            let node = node(condition);
            let batches = |pairs: bool| {
                let (mut bag, _, sizes) =
                    conditional_batches(&node, 64, 8 << 20, pairs, None).unwrap();
                bag.sort();
                (bag, sizes)
            };
            let (off, on) = (batches(false), batches(true));
            assert_eq!(off.0, on.0, "{name}");
            if decides {
                assert!(on.1.len() < off.1.len(), "{name}: {:?}", on.1);
            } else {
                assert_eq!(off.1, on.1, "{name}");
            }
        }
    }

    /// A conditional merge OPTIONAL that fits a memory budget with the condition tested
    /// after the batch fits with the condition tested on the pairs too, with at most
    /// one more output batch and the scratch for deciding it, and returns the same bag.
    /// The two paths end their batches at different left rows, so a run is held or an
    /// input batch loaded beside different buffers, and the same budget does not
    /// always fit both. The fit is not monotone in the budget either, because the merge
    /// derives each batch's size from the room it finds, so the budgets are swept and
    /// not bisected. The bound is on the merge's own buffers: the condition here charges
    /// nothing for its evaluation, and one that charges by the pairs in a call is
    /// outside it, since a call can hold more pairs with the switch on.
    #[test]
    fn pair_conditions_fit_within_a_batch_of_every_budget_that_the_staged_test_fits() {
        let (left, right) = (2, 3);
        let condition = Expr::Cmp(
            Box::new(Expr::Var(right)),
            Box::new(Expr::Var(left)),
            crate::sparql::expr::CmpOp::Gt,
        );
        // 64 one-to-one matches that pass, then pairs that fail and pass at fan-out 3,
        // left rows without a pair, and a run of 40 pairs that no batch of 4 holds.
        let one: Vec<Vec<i64>> = (0..64).map(|k| vec![k, 1]).collect();
        type Rows = Vec<Vec<i64>>;
        let shapes: Vec<(&str, Rows, Rows)> = vec![
            (
                "one to one",
                one.clone(),
                (0..64).map(|k| vec![k, 2]).collect(),
            ),
            (
                "fan-out 3",
                (0..32).map(|k| vec![k, 5]).collect(),
                (0..96).map(|j| vec![j / 3, j % 9]).collect(),
            ),
            (
                "unmatched",
                one.clone(),
                (0..64).map(|k| vec![100 + k, 2]).collect(),
            ),
            (
                "held run",
                (0..24).map(|k| vec![k / 2, 5 + k % 2]).collect(),
                (0..40)
                    .map(|j| vec![3, j % 11])
                    .chain((4..12).map(|k| vec![k, 9]))
                    .collect(),
            ),
            // The last left row meets a run that crosses the right input's batches of
            // 64 rows. With pair conditions the batch before it holds more rows when
            // the run is held.
            (
                "long last run",
                (1..=32).chain([64]).map(|k| vec![k, 1]).collect(),
                (1..=63)
                    .map(|k| vec![k, 2])
                    .chain((0..129).map(|j| vec![64, 2 + j]))
                    .collect(),
            ),
        ];
        let mut worst = 0;
        for (name, l, r) in &shapes {
            let mut node = join(values(vec![0, left], l), values(vec![0, right], r));
            node.kind = Kind::LeftJoin {
                expr: Some(condition.clone()),
            };
            let mut expected = conditional(&node, 4096, u64::MAX, false, None).unwrap().0;
            expected.sort();
            for cap in [1, 2, 4, 64] {
                let bound = pair_bound(&node, cap);
                let (mut fits, mut fails) = (0, 0);
                for limit in (1024..40_000u64).step_by(16) {
                    let budget = |e: &Error| matches!(e, Error::BudgetExceeded(_));
                    match conditional(&node, cap, limit, false, None) {
                        Ok((mut bag, _)) => {
                            fits += 1;
                            bag.sort();
                            assert_eq!(bag, expected, "{name}, cap {cap}, {limit} bytes");
                            let more = (0..=bound).step_by(16).find(|more| {
                                match conditional(&node, cap, limit + more, true, None) {
                                    Ok((mut bag, _)) => {
                                        bag.sort();
                                        assert_eq!(bag, expected, "{name}, cap {cap}, {limit}");
                                        true
                                    }
                                    Err(e) => {
                                        assert!(budget(&e), "{name}, cap {cap}, {limit}: {e}");
                                        false
                                    }
                                }
                            });
                            let more = more.unwrap_or_else(|| {
                                panic!(
                                    "{name}, cap {cap}: fits {limit} bytes with the staged test, \
                                     and with pair conditions not within {bound} bytes more"
                                )
                            });
                            if more > worst {
                                worst = more;
                                println!(
                                    "{name}, cap {cap}, {limit} bytes: {more} of {bound} more"
                                );
                            }
                        }
                        Err(e) => {
                            assert!(budget(&e), "{name}, cap {cap}, {limit} bytes: {e}");
                            fails += 1;
                        }
                    }
                }
                // The sweep crosses the budgets at which the staged test starts to fit.
                assert!(
                    fits > 0 && fails > 0,
                    "{name}, cap {cap}: {fits} fit, {fails} fail"
                );
            }
        }
        // The long last run needs more with pair conditions at some budgets, so the
        // sweep exercises the bound and not only equal budgets.
        assert!(worst > 0);
    }

    /// Small conditional merge OPTIONALs whose runs cross batches, pulled at small and
    /// changing caps, each with the caps to pull it at. The shapes are those that found
    /// over-budget output buffers and run copies in the streamed merge's batch ends: one
    /// pair, a right side of keys only, caps that change between pulls, a copy at a batch
    /// boundary, runs of one to six, a long stretch without a match, and a left run of
    /// one row more than a power of two beside a short right run. Each has a condition
    /// that some pairs pass and some fail.
    fn pair_condition_fixtures() -> Vec<(String, Node, Vec<usize>)> {
        let (left, right) = (2, 3);
        // Values that mix passing and failing pairs under `?right > ?left`.
        let keyed = |keys: &[i64], step: i64| -> Vec<Vec<i64>> {
            keys.iter()
                .enumerate()
                .map(|(i, &k)| vec![k, i as i64 * step % 100])
                .collect()
        };
        let repeat = |runs: &[(i64, usize)]| -> Vec<i64> {
            runs.iter()
                .flat_map(|&(k, n)| std::iter::repeat_n(k, n))
                .collect()
        };
        let above = |a: VarId, b: VarId| {
            Expr::Cmp(
                Box::new(Expr::Var(a)),
                Box::new(Expr::Var(b)),
                crate::sparql::expr::CmpOp::Gt,
            )
        };
        let optional = |l: Node, r: Node, condition: Expr| {
            let mut node = join(l, r);
            node.kind = Kind::LeftJoin {
                expr: Some(condition),
            };
            node
        };
        let mut cases = Vec::new();
        let mut add = |name: &str, node: Node, caps: &[usize]| {
            for &cap in caps {
                cases.push((format!("{name} cap {cap}"), node.clone(), vec![cap]));
            }
            if caps.len() > 1 {
                cases.push((format!("{name} caps {caps:?}"), node, caps.to_vec()));
            }
        };
        let two = |l: &[i64], r: &[i64]| {
            optional(
                values(vec![0, left], &keyed(l, 37)),
                values(vec![0, right], &keyed(r, 53)),
                above(right, left),
            )
        };
        add("one pair", two(&[0], &[0, 0]), &[2]);
        // The right side has the key only, so the condition reads the left value.
        let rows: Vec<Vec<i64>> = repeat(&[(0, 2), (1, 8), (2, 5), (3, 1)])
            .into_iter()
            .map(|k| vec![k])
            .collect();
        let node = optional(
            values(vec![0, left], &keyed(&repeat(&[(0, 3), (2, 5)]), 37)),
            values(vec![0], &rows),
            above(left, 0),
        );
        add("key-only right", node, &[5]);
        add(
            "changing caps",
            two(
                &repeat(&[(0, 1), (1, 1), (3, 4)]),
                &repeat(&[(0, 5), (1, 1), (2, 6), (3, 1), (4, 42)]),
            ),
            &[6, 43],
        );
        add("boundary copy", two(&[2, 2, 5], &[2, 2, 2, 5, 6]), &[4]);
        let l: Vec<i64> = (0..40)
            .flat_map(|k| std::iter::repeat_n(k, 1 + (k % 3) as usize))
            .collect();
        let r: Vec<i64> = (0..40)
            .filter(|k| k % 5 != 2)
            .flat_map(|k| std::iter::repeat_n(k, 1 + (k * 7 % 6) as usize))
            .collect();
        add("runs of one to six", two(&l, &r), &[1, 2, 3, 5, 7]);
        let l: Vec<i64> = (0..300).chain((300..310).flat_map(|k| [k, k])).collect();
        let r: Vec<i64> = (300..310).flat_map(|k| [k, k, k]).collect();
        add("unmatched stretch", two(&l, &r), &[1, 3]);
        // A left run of 2^k + 1 rows, whose copy grows its buffer to twice the rows it
        // holds, beside a right run of two. With one column a side the condition reads
        // the key alone and every pair passes.
        let one = |keys: &[i64]| -> Node {
            let rows: Vec<Vec<i64>> = keys.iter().map(|&k| vec![k]).collect();
            values(vec![0], &rows)
        };
        let any = Expr::Cmp(
            Box::new(Expr::Var(0)),
            Box::new(Expr::Const(Id::from_i64(-1).unwrap())),
            crate::sparql::expr::CmpOp::Gt,
        );
        for n in [128, 129, 130, 257] {
            let l: Vec<i64> = [0].into_iter().chain(std::iter::repeat_n(3, n)).collect();
            let r = [0, 1, 2, 3, 3];
            for caps in [&[3, 1][..], &[1, 3], &[3, 3, 1], &[64, 1]] {
                cases.push((
                    format!("left run {n}, one column, caps {caps:?}"),
                    optional(one(&l), one(&r), any.clone()),
                    caps.to_vec(),
                ));
                cases.push((
                    format!("left run {n}, two columns, caps {caps:?}"),
                    two(&l, &r),
                    caps.to_vec(),
                ));
            }
        }
        cases
    }

    /// The fixtures of [`pair_condition_fixtures`] under a pointwise budget sweep. At
    /// every budget that a merge fits with the condition tested after the batch, it fits
    /// with `merge_pair_conditions` at that budget or within [`pair_bound`] above it, at
    /// the largest of its caps, and gives the same bag with the left order. The fit is
    /// not monotone in the budget, so no limit is bisected: every limit from 2 KiB below
    /// the smallest that the switch-off merge was found to fit at, to 4 KiB or twice the
    /// bound above it, is tried in steps of 8 bytes, for a caller that releases each
    /// batch before the next pull and for one that holds it across the pull.
    #[test]
    fn pair_conditions_stay_within_the_bound_on_the_full_batch_fixtures() {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let budget = |e: &Error| matches!(e, Error::BudgetExceeded(_));
        let (mut failed, mut differ, mut worst) = (Vec::new(), 0, (0u64, String::new()));
        let cases = pair_condition_fixtures();
        for (name, node, caps) in &cases {
            let bound = pair_bound(node, caps.iter().copied().max().unwrap());
            let (mut expected, off_sizes) =
                pulled(&snap, node, caps, u64::MAX, false, false).unwrap();
            let keys: Vec<Id> = expected.iter().map(|row| row[0]).collect();
            assert!(keys.is_sorted(), "{name}");
            expected.sort();
            let (_, on_sizes) = pulled(&snap, node, caps, u64::MAX, true, false).unwrap();
            differ += usize::from(on_sizes != off_sizes);
            for hold in [false, true] {
                let fits =
                    |limit: u64, pairs: bool| match pulled(&snap, node, caps, limit, pairs, hold) {
                        Ok((mut bag, _)) => {
                            let keys: Vec<Id> = bag.iter().map(|row| row[0]).collect();
                            assert!(keys.is_sorted(), "{name} at {limit} bytes, pairs {pairs}");
                            bag.sort();
                            assert_eq!(bag, expected, "{name} at {limit} bytes, pairs {pairs}");
                            true
                        }
                        Err(e) => {
                            assert!(budget(&e), "{name} at {limit} bytes, pairs {pairs}: {e}");
                            false
                        }
                    };
                // A first smallest fit with the switch off, by bisection, as the centre
                // of the pointwise range only.
                let (mut lo, mut hi) = (0u64, 1u64 << 20);
                assert!(fits(hi, false), "{name}");
                while hi - lo > 8 {
                    let mid = lo + (hi - lo) / 2;
                    if fits(mid, false) {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                let (mut tried, mut more) = (0, 0);
                for limit in (hi.saturating_sub(2048)..hi + 4096.max(2 * bound)).step_by(8) {
                    if !fits(limit, false) {
                        continue;
                    }
                    tried += 1;
                    if fits(limit, true) {
                        continue;
                    }
                    let Some(extra) = (8..=bound).step_by(8).find(|m| fits(limit + m, true)) else {
                        failed.push(format!(
                            "{name}, hold {hold}: fits {limit} bytes with the staged test, and \
                             with pair conditions not within {bound} bytes more"
                        ));
                        break;
                    };
                    more = more.max(extra);
                }
                assert!(tried > 0, "{name}, hold {hold}");
                if more > worst.0 {
                    worst = (
                        more,
                        format!("{name}, hold {hold}: {more} of {bound} bytes"),
                    );
                }
            }
        }
        println!("PAIRBOUND worst {}", worst.1);
        println!(
            "PAIRBOUND {differ} of {} cases end batches elsewhere",
            cases.len()
        );
        assert!(failed.is_empty(), "{}", failed.join("\n"));
        // The switch changes where batches end on most fixtures, so the sweep compares
        // two different paths and not one path with itself.
        assert!(differ * 2 > cases.len(), "{differ} of {}", cases.len());
    }

    /// With the switch off, the test after the batch reserves its scratch, which holds
    /// the variable map, for a batch without a pair too, as it did before the switch.
    #[test]
    fn the_staged_test_reserves_its_scratch_for_a_batch_without_pairs() {
        let (left, right) = (2, 3);
        let condition = Expr::Cmp(
            Box::new(Expr::Var(right)),
            Box::new(Expr::Var(left)),
            crate::sparql::expr::CmpOp::Gt,
        );
        let l: Vec<Vec<i64>> = (0..8).map(|k| vec![k, 1]).collect();
        let r: Vec<Vec<i64>> = (0..8).map(|k| vec![100 + k, 2]).collect();
        let mut node = join(values(vec![0, left], &l), values(vec![0, right], &r));
        node.kind = Kind::LeftJoin {
            expr: Some(condition),
        };
        let peak = |pairs: bool| {
            let store = Store::in_memory(StoreOptions::default());
            let mut ctx = Ctx::new(store.snapshot());
            ctx.opt.merge_pair_conditions = pairs;
            // The variable map takes 16 bytes a variable, so it stands out in the peak.
            for i in 0..4096 {
                ctx.var(&format!("v{i}"));
            }
            ctx.mem_limit = 8 << 20;
            ctx.enable_cursor_accounting().unwrap();
            let ctx = Arc::new(ctx);
            ctx.attach_owner();
            let nvars = ctx.nvars() as u64;
            let mut op =
                Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization).unwrap();
            let options = CursorOptions::default();
            let mut rows = 0;
            while let Some(batch) = op.next(&ctx, &options, 4096).unwrap() {
                assert!(batch.table.cols[2].iter().all(|id| id.is_undef()));
                rows += batch.table.len;
            }
            assert_eq!(rows, 8);
            (ctx.mem_peak(), nvars)
        };
        // The peak without the scratch is a few kilobytes.
        let (off, nvars) = peak(false);
        assert!(
            off >= nvars * 16,
            "{off} bytes at the peak, {nvars} variables"
        );
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
                    assert!(eligible(&ctx, &node));
                    let expected = rows(&exec::execute_materialized(&ctx, &node).unwrap().0.table);
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
        assert!(!eligible(&context(1 << 20), &node));
        let op = Operator::build(
            &context(1 << 20),
            node,
            FallbackPolicy::RejectMaterialization,
        )
        .unwrap();
        assert!(matches!(op.state, super::super::State::Binary(_)));
        // A second shared variable keeps the join off the key-only path, which joins a
        // left row with the right run of its key and holds no left run at all.
        let many = vec![vec![0, 1]; 10_000];
        let ctx = context(32 << 10);
        let node = join(values(vec![0, 1], &many), values(vec![0, 1], &[vec![0, 1]]));
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
        let ctx = context(1 << 20);
        assert!(eligible(&ctx, &node));
        let expected = rows(&exec::execute_materialized(&ctx, &node).unwrap().0.table);
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
    /// A run of equal keys larger than a quarter of the room spools to disk, on either
    /// side or both, and the join still gives the eager bag, with an OPTIONAL's
    /// unmatched rows and the order the merge claims.
    #[test]
    fn long_runs_spool_to_disk_and_join_as_in_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 34,
            query_bytes: 1 << 34,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let run = |key: i64, n: usize, shared: usize, base: i64| -> Vec<Vec<i64>> {
            (0..n)
                .map(|i| {
                    let s = if i % 11 == 0 { -1 } else { (i % shared) as i64 };
                    vec![key, s, base + i as i64]
                })
                .collect()
        };
        let long = [run(0, 6, 3, 0), run(1, 1500, 5, 100), run(3, 3, 2, 9000)].concat();
        let short = [
            run(1, 150, 7, 20_000),
            run(2, 5, 2, 30_000),
            run(3, 2, 2, 40_000),
        ]
        .concat();
        let mut spooled = 0;
        let half = long[..1000].to_vec();
        for (left, right) in [(&long, &short), (&short, &long), (&long, &half)] {
            for (optional, condition) in [(false, false), (true, false), (true, true)] {
                let mut node = join(values(vec![0, 1, 2], left), values(vec![0, 1, 3], right));
                if optional {
                    // The condition passes every pair when the long run is on the left
                    // and fails every pair when it is on the right, so the left rows of
                    // a spooled run keep either their pairs or their extension.
                    let expr = condition.then(|| {
                        Expr::Cmp(
                            Box::new(Expr::Var(3)),
                            Box::new(Expr::Var(2)),
                            crate::sparql::expr::CmpOp::Gt,
                        )
                    });
                    node.kind = Kind::LeftJoin { expr };
                }
                let expected = rows(
                    &exec::execute_materialized(&context(u64::MAX), &node)
                        .unwrap()
                        .0
                        .table,
                );
                for cap in [7, 512] {
                    let ctx = {
                        let store = Store::in_memory(StoreOptions::default());
                        let mut ctx = Ctx::new(store.snapshot());
                        for name in ["key", "shared", "left", "right"] {
                            ctx.var(name);
                        }
                        ctx.mem_limit = 128 << 10;
                        ctx.spill = Some(Arc::new(spill.query(None)));
                        ctx.enable_cursor_accounting().unwrap();
                        let ctx = Arc::new(ctx);
                        ctx.attach_owner();
                        ctx
                    };
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
                        result.extend(rows(&batch.table));
                    }
                    if let super::super::State::Merge(merge) = &op.state {
                        spooled += merge.spooled_runs;
                    }
                    result.sort();
                    assert_eq!(
                        result.len(),
                        expected.len(),
                        "optional {optional} condition {condition} cap {cap}"
                    );
                    assert_eq!(
                        result, expected,
                        "optional {optional} condition {condition} cap {cap}"
                    );
                }
            }
        }
        assert!(spooled >= 18, "{spooled} runs spooled");
        assert_eq!(spill.reserved_bytes(), 0);
    }

    /// A deciding merge ends its batch before collecting another general run when it
    /// already holds staged pairs or an open passed row.
    #[test]
    fn pair_conditions_flush_before_collecting_another_run() {
        use crate::sparql::expr::Func;
        use spargebra::algebra::Function;

        let cap = 8usize;
        let condition = Expr::Cmp(
            Box::new(Expr::Var(1)),
            Box::new(Expr::Const(Id::from_i64(0).unwrap())),
            crate::sparql::expr::CmpOp::Gt,
        );
        let left_keys: Vec<i64> = (0..=7)
            .chain([12])
            .chain(std::iter::repeat_n(15, 16))
            .chain([16])
            .collect();
        let right_keys: Vec<i64> = [0, 1, 2, 8, 9, 10, 11]
            .into_iter()
            .chain(std::iter::repeat_n(12, 6))
            .chain([13, 14])
            .chain(std::iter::repeat_n(15, 16))
            .chain([16])
            .collect();
        let left_rows: Vec<Vec<i64>> = left_keys.iter().map(|&k| vec![k, 1]).collect();
        let right_rows: Vec<Vec<i64>> = right_keys
            .iter()
            .enumerate()
            .map(|(j, &k)| vec![k, 100 + j as i64])
            .collect();
        let make_node = |expr: Expr| {
            let mut node = join(
                values(vec![0, 1], &left_rows),
                values(vec![0, 2], &right_rows),
            );
            node.kind = Kind::LeftJoin { expr: Some(expr) };
            node
        };
        let node = make_node(condition.clone());
        assert_eq!(node.vars, vec![0, 1, 2]);
        let expected = rows(
            &exec::execute_materialized(&context(u64::MAX), &node)
                .unwrap()
                .0
                .table,
        );

        // Inspect raw staged output and pending run state across the crossing runs.
        let ctx = context(8 << 20);
        let mut op =
            Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization).unwrap();
        let super::super::State::Merge(merge) = &mut op.state else {
            unreachable!()
        };
        let options = CursorOptions {
            batch_rows: cap,
            ..Default::default()
        };
        let staged = vec![0, 1, 2, VarId::MAX];
        let mut first = merge
            .merged(
                &ctx,
                &mut op.children,
                &options,
                &staged,
                cap,
                Some(&condition),
            )
            .unwrap()
            .expect("first deciding batch");
        assert_eq!(first.table.len, 8);
        assert!(first.table.cols[3].iter().all(|id| id.is_undef()));
        merge.test(&ctx, &mut first, &condition).unwrap();
        assert_eq!(first.table.len, 8);
        assert_eq!(first.table.width(), 3);

        // Hold `first` across the second pull, where key 12 crosses the right batch as
        // 1 + 5 and stages six pairs plus its extension ahead of key 15's general run.
        let mut second = merge
            .merged(
                &ctx,
                &mut op.children,
                &options,
                &staged,
                cap,
                Some(&condition),
            )
            .unwrap()
            .expect("second batch staging crossing run 12");
        assert_eq!(second.table.len, 7);
        let staged_pairs = second.table.cols[3]
            .iter()
            .filter(|id| !id.is_undef())
            .count();
        assert_eq!(staged_pairs, 6);
        assert!(
            merge.pair.is_none(),
            "key 15 run must not be collected before settling staged key 12"
        );
        assert!(merge.spooled.is_none());
        assert_eq!(
            merge.inputs[0].table().cols[0][merge.inputs[0].at],
            Id::from_i64(15).unwrap()
        );
        assert_eq!(
            merge.inputs[1].table().cols[0][merge.inputs[1].at],
            Id::from_i64(15).unwrap()
        );

        merge.test(&ctx, &mut second, &condition).unwrap();
        assert_eq!(second.table.len, 6);
        assert_eq!(second.table.width(), 3);
        let mut expected_key12: Vec<Vec<Id>> = (0..6)
            .map(|j| {
                vec![
                    Id::from_i64(12).unwrap(),
                    Id::from_i64(1).unwrap(),
                    Id::from_i64(107 + j).unwrap(),
                ]
            })
            .collect();
        expected_key12.sort();
        assert_eq!(rows(&second.table), expected_key12);

        let collect_in_order = |table: &Table, out: &mut Vec<Vec<Id>>| {
            out.extend((0..table.len).map(|i| table.cols.iter().map(|c| c[i]).collect()));
        };
        let mut drained = Vec::new();
        collect_in_order(&first.table, &mut drained);
        collect_in_order(&second.table, &mut drained);
        drop(first);
        drop(second);
        while let Some(batch) = merge
            .next(&ctx, &mut op.children, &options, &node.vars, cap)
            .unwrap()
        {
            assert!(batch.table.len <= cap && batch.table.len > 0);
            collect_in_order(&batch.table, &mut drained);
        }
        let keys: Vec<Id> = drained.iter().map(|row| row[0]).collect();
        assert!(keys.is_sorted());
        drained.sort();
        assert_eq!(drained, expected);

        // Normal `next` path across switch settings and hold/release consumers.
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        for pairs in [false, true] {
            for hold in [false, true] {
                let (mut bag, _) = pulled(&snap, &node, &[cap], 8 << 20, pairs, hold).unwrap();
                let keys: Vec<Id> = bag.iter().map(|row| row[0]).collect();
                assert!(keys.is_sorted(), "pairs {pairs} hold {hold}");
                bag.sort();
                assert_eq!(bag, expected, "pairs {pairs} hold {hold}");
            }
        }

        // A first general run on an empty output still proceeds and drains cleanly.
        let first_run_left: Vec<Vec<i64>> = (0..16).map(|_| vec![15, 1]).collect();
        let first_run_right: Vec<Vec<i64>> = (0..16).map(|j| vec![15, 200 + j]).collect();
        let mut first_run_node = join(
            values(vec![0, 1], &first_run_left),
            values(vec![0, 2], &first_run_right),
        );
        first_run_node.kind = Kind::LeftJoin {
            expr: Some(condition.clone()),
        };
        let first_run_expected = rows(
            &exec::execute_materialized(&context(u64::MAX), &first_run_node)
                .unwrap()
                .0
                .table,
        );
        let (mut first_run_bag, _) =
            pulled(&snap, &first_run_node, &[cap], 8 << 20, true, true).unwrap();
        first_run_bag.sort();
        assert_eq!(first_run_bag, first_run_expected);

        // An observable condition keeps the exact switch-off batches.
        let observable = Expr::Cmp(
            Box::new(Expr::Call(Func::Builtin(Function::Rand), vec![])),
            Box::new(Expr::Const(Id::from_i64(2).unwrap())),
            crate::sparql::expr::CmpOp::Lt,
        );
        let obs_node = make_node(observable);
        let (off_bag, off_sizes) = pulled(&snap, &obs_node, &[cap], 8 << 20, false, true).unwrap();
        let (on_bag, on_sizes) = pulled(&snap, &obs_node, &[cap], 8 << 20, true, true).unwrap();
        assert_eq!(on_sizes, off_sizes);
        assert_eq!(on_bag, off_bag);
    }

    /// When remaining memory leaves zero deciding room beside already-decided rows,
    /// [`Merge::interior`] returns [`Step::Full`] so those rows flush without charging
    /// settlement scratch, while an empty batch still returns [`Step::Stuck`] and
    /// `covered == li` preserves right-only advancement.
    #[test]
    fn pair_conditions_flush_when_deciding_room_is_zero() {
        let condition = Expr::Cmp(
            Box::new(Expr::Var(3)),
            Box::new(Expr::Var(2)),
            crate::sparql::expr::CmpOp::Gt,
        );
        let make_merge = |ctx: &Arc<Ctx>, left_rows: &[Vec<i64>], right_rows: &[Vec<i64>]| {
            let mut node = join(
                values(vec![0, 2], left_rows),
                values(vec![0, 3], right_rows),
            );
            node.kind = Kind::LeftJoin {
                expr: Some(condition.clone()),
            };
            let mut op =
                Operator::build(ctx, node.clone(), FallbackPolicy::RejectMaterialization).unwrap();
            let options = CursorOptions {
                batch_rows: 4096,
                ..Default::default()
            };
            let super::super::State::Merge(merge) = &mut op.state else {
                unreachable!()
            };
            assert!(
                merge.inputs[0]
                    .ensure(ctx, &mut op.children[0], &options, 4096)
                    .unwrap()
            );
            assert!(
                merge.inputs[1]
                    .ensure(ctx, &mut op.children[1], &options, 4096)
                    .unwrap()
            );
            (op, node, options)
        };

        let ctx = context(1 << 20);
        let left_rows = [vec![10, 5], vec![11, 5]];
        let right_rows = [vec![10, 1], vec![10, 9], vec![11, 2]];
        let (mut op, node, options) = make_merge(&ctx, &left_rows, &right_rows);
        let super::super::State::Merge(merge) = &mut op.state else {
            unreachable!()
        };
        assert_eq!(merge.inputs[0].at, 0);
        assert_eq!(merge.inputs[1].at, 0);
        assert!(!merge.passed);

        let staged = [0, 2, 3, VarId::MAX];
        let mut out = Buffer::new(&ctx, &staged, 4096).unwrap();
        for k in 0..200i64 {
            out.table.cols[0].push(Id::from_i64(k).unwrap());
            out.table.cols[1].push(Id::from_i64(1).unwrap());
            out.table.cols[2].push(Id::from_i64(2).unwrap());
            out.table.cols[3].push(Id::UNDEF);
        }
        out.table.len = 200;

        let fixed = decide_fixed(&ctx, 3);
        let row = decide_row(3);
        for leave in [0, fixed - 1, fixed + row - 1] {
            let held = ctx.charge(ctx.memory_room() - leave).unwrap();
            assert_eq!(ctx.memory_room(), leave);
            let before = (merge.inputs[0].at, merge.inputs[1].at);
            let step = merge.interior(&ctx, &mut out, 4096, false, true).unwrap();
            assert!(
                matches!(step, Step::Full),
                "zero deciding room (leave {leave}) must flush already-decided rows"
            );
            assert_eq!((merge.inputs[0].at, merge.inputs[1].at), before);
            assert_eq!(out.table.len, 200);
            assert!(out.table.cols[3].iter().all(|id| id.is_undef()));

            // Settling the decided-only table succeeds without taking scratch or dropping rows.
            merge.settle(&ctx, &mut out.table, &condition).unwrap();
            assert_eq!(ctx.memory_room(), leave);
            assert_eq!(out.table.len, 200);
            assert!(!merge.passed);
            assert!(out.table.cols[3].iter().all(|id| id.is_undef()));
            drop(held);
        }

        // One byte higher permits fit == 1, which still cannot hold key 10's two-row run.
        let one_more = fixed + row;
        let held = ctx.charge(ctx.memory_room() - one_more).unwrap();
        let before = (merge.inputs[0].at, merge.inputs[1].at);
        let step = merge.interior(&ctx, &mut out, 4096, false, true).unwrap();
        assert!(matches!(step, Step::Full));
        assert_eq!((merge.inputs[0].at, merge.inputs[1].at), before);
        assert_eq!(out.table.len, 200);
        drop(held);

        // With an empty output, zero deciding room still returns Step::Stuck for progress.
        let mut empty = Buffer::new(&ctx, &staged, 4096).unwrap();
        let held = ctx.charge(ctx.memory_room() - (fixed + row - 1)).unwrap();
        let step = merge.interior(&ctx, &mut empty, 4096, false, true).unwrap();
        assert!(matches!(step, Step::Stuck));
        assert_eq!((merge.inputs[0].at, merge.inputs[1].at), (0, 0));
        assert_eq!(empty.table.len, 0);
        drop(held);
        drop(empty);

        // When `covered == li`, zero deciding room keeps right-only advancement intact.
        {
            let ctx_cov = context(1 << 20);
            let (mut cov_op, _, _) = make_merge(
                &ctx_cov,
                &[vec![11, 5], vec![12, 5]],
                &[vec![9, 1], vec![10, 2], vec![11, 3]],
            );
            let super::super::State::Merge(cov_merge) = &mut cov_op.state else {
                unreachable!()
            };
            let mut cov_out = Buffer::new(&ctx_cov, &staged, 4096).unwrap();
            for k in 0..200i64 {
                cov_out.table.cols[0].push(Id::from_i64(k).unwrap());
                cov_out.table.cols[1].push(Id::from_i64(1).unwrap());
                cov_out.table.cols[2].push(Id::from_i64(2).unwrap());
                cov_out.table.cols[3].push(Id::UNDEF);
            }
            cov_out.table.len = 200;
            let held = ctx_cov
                .charge(ctx_cov.memory_room() - (decide_fixed(&ctx_cov, 3) + row - 1))
                .unwrap();
            let step = cov_merge
                .interior(&ctx_cov, &mut cov_out, 4096, false, true)
                .unwrap();
            assert!(matches!(step, Step::Advanced));
            assert_eq!(cov_merge.inputs[0].at, 0);
            assert_eq!(cov_merge.inputs[1].at, 2);
            let step2 = cov_merge
                .interior(&ctx_cov, &mut cov_out, 4096, false, true)
                .unwrap();
            assert!(matches!(step2, Step::Stuck));
            drop(held);
        }

        // Release the synthetic buffer and resume normally to check exact pairs and extensions.
        drop(out);
        let mut resumed = Vec::new();
        while let Some(batch) = merge
            .next(&ctx, &mut op.children, &options, &node.vars, 4096)
            .unwrap()
        {
            resumed.extend(
                (0..batch.table.len)
                    .map(|i| batch.table.cols.iter().map(|c| c[i]).collect::<Vec<_>>()),
            );
        }
        assert_eq!(
            resumed,
            vec![
                vec![
                    Id::from_i64(10).unwrap(),
                    Id::from_i64(5).unwrap(),
                    Id::from_i64(9).unwrap(),
                ],
                vec![
                    Id::from_i64(11).unwrap(),
                    Id::from_i64(5).unwrap(),
                    Id::UNDEF
                ],
            ]
        );
    }

    /// [`decide_pairs`] polls cancellation periodically even when every candidate is an
    /// unmatched `NO_ROW` extension and the condition evaluator never runs.
    #[test]
    fn pair_conditions_check_unmatched_decisions_for_cancellation() {
        let n = 8192usize;
        let ctx = context(8 << 20);
        let mut left = Table::new(vec![0, 2]);
        for i in 0..n as i64 {
            left.push_row(&[Id::from_i64(i).unwrap(), Id::from_i64(1).unwrap()]);
        }
        let right = Table::new(vec![0, 3]);
        let vars = [0, 2, 3];
        let columns = [(Some(0), Some(0)), (Some(1), None), (None, Some(1))];
        let condition = Expr::Cmp(
            Box::new(Expr::Var(3)),
            Box::new(Expr::Var(2)),
            crate::sparql::expr::CmpOp::Gt,
        );

        let mut lrows: Vec<u32> = (0..n as u32).collect();
        let mut rrows: Vec<u32> = vec![NO_ROW; n];
        ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let err = decide_pairs(
            &ctx,
            &condition,
            &vars,
            &columns,
            (&left, &right, 0, 0),
            &mut lrows,
            &mut rrows,
        );
        assert!(
            matches!(err, Err(Error::Cancelled)),
            "expected Err(Error::Cancelled), got {err:?}"
        );

        ctx.cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let expected_l = lrows.clone();
        let expected_r = rrows.clone();
        let before = crate::sparql::ctx::CHECKS.with(|c| c.get());
        decide_pairs(
            &ctx,
            &condition,
            &vars,
            &columns,
            (&left, &right, 0, 0),
            &mut lrows,
            &mut rrows,
        )
        .unwrap();
        let checks = crate::sparql::ctx::CHECKS.with(|c| c.get()) - before;
        assert!(
            checks >= 8,
            "expected at least 8 cancellation checks for {n} unmatched rows, saw {checks}"
        );
        assert_eq!(lrows, expected_l);
        assert_eq!(rrows, expected_r);
    }

    /// [`decide_pairs`] observes an expired deadline on all-`NO_ROW` inputs and follows
    /// its entry/exit stop policy on zero-length inputs too.
    #[test]
    fn pair_conditions_check_decisions_for_deadline() {
        let n = 8192usize;
        let expired = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
        let ctx = context_set(8 << 20, None, true, |ctx| {
            ctx.deadline = Some(expired);
        });
        let mut left = Table::new(vec![0, 2]);
        for i in 0..n as i64 {
            left.push_row(&[Id::from_i64(i).unwrap(), Id::from_i64(1).unwrap()]);
        }
        let right = Table::new(vec![0, 3]);
        let vars = [0, 2, 3];
        let columns = [(Some(0), Some(0)), (Some(1), None), (None, Some(1))];
        let condition = Expr::Cmp(
            Box::new(Expr::Var(3)),
            Box::new(Expr::Var(2)),
            crate::sparql::expr::CmpOp::Gt,
        );

        let mut lrows: Vec<u32> = (0..n as u32).collect();
        let mut rrows: Vec<u32> = vec![NO_ROW; n];
        let err = decide_pairs(
            &ctx,
            &condition,
            &vars,
            &columns,
            (&left, &right, 0, 0),
            &mut lrows,
            &mut rrows,
        );
        assert!(
            matches!(err, Err(Error::Timeout)),
            "expected Err(Error::Timeout), got {err:?}"
        );

        // Zero-length inputs still observe entry/exit stop checks.
        let mut empty_l = Vec::new();
        let mut empty_r = Vec::new();
        let err = decide_pairs(
            &ctx,
            &condition,
            &vars,
            &columns,
            (&left, &right, 0, 0),
            &mut empty_l,
            &mut empty_r,
        );
        assert!(
            matches!(err, Err(Error::Timeout)),
            "zero-length input must observe expired deadline, got {err:?}"
        );

        let live = context(8 << 20);
        live.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let err = decide_pairs(
            &live,
            &condition,
            &vars,
            &columns,
            (&left, &right, 0, 0),
            &mut empty_l,
            &mut empty_r,
        );
        assert!(matches!(err, Err(Error::Cancelled)));

        live.cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let before = crate::sparql::ctx::CHECKS.with(|c| c.get());
        decide_pairs(
            &live,
            &condition,
            &vars,
            &columns,
            (&left, &right, 0, 0),
            &mut empty_l,
            &mut empty_r,
        )
        .unwrap();
        let checks = crate::sparql::ctx::CHECKS.with(|c| c.get()) - before;
        assert!(
            checks >= 1,
            "zero-length decide_pairs must check at entry/exit, saw {checks}"
        );
    }

    /// On a single-threaded Rayon pool, [`decide_pairs`] and the deciding output copy in
    /// [`Merge::interior`] add periodic stop checks beyond condition evaluation across
    /// all-pass, all-fail and alternating verdicts, including a single left row with a
    /// long right run, while preserving exact compacted pairs and unmatched extensions.
    #[test]
    fn pair_conditions_check_long_decision_passes() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.install(|| {
            let long_run = 4096usize;
            let singles = 4096usize;
            let total = long_run + singles;
            let vars = [0, 2, 3];
            let columns = [(Some(0), Some(0)), (Some(1), None), (None, Some(1))];
            let condition = Expr::Cmp(
                Box::new(Expr::Var(3)),
                Box::new(Expr::Var(2)),
                crate::sparql::expr::CmpOp::Gt,
            );
            let ctx = context(64 << 20);

            for mode in ["all-pass", "all-fail", "alternating"] {
                let left_val = match mode {
                    "all-pass" => 0i64,
                    "all-fail" => 10,
                    "alternating" => 5,
                    _ => unreachable!(),
                };
                let mut left = Table::new(vec![0, 2]);
                left.push_row(&[Id::from_i64(0).unwrap(), Id::from_i64(left_val).unwrap()]);
                for k in 1..=singles as i64 {
                    left.push_row(&[Id::from_i64(k).unwrap(), Id::from_i64(left_val).unwrap()]);
                }
                let mut right = Table::new(vec![0, 3]);
                for j in 0..total {
                    let key = if j < long_run {
                        0i64
                    } else {
                        (j - long_run + 1) as i64
                    };
                    let rv = match mode {
                        "all-pass" => 10i64,
                        "all-fail" => 0,
                        "alternating" => {
                            if j.is_multiple_of(2) {
                                10
                            } else {
                                0
                            }
                        }
                        _ => unreachable!(),
                    };
                    right.push_row(&[Id::from_i64(key).unwrap(), Id::from_i64(rv).unwrap()]);
                }

                let mut lrows = Vec::with_capacity(total);
                let mut rrows = Vec::with_capacity(total);
                for j in 0..long_run {
                    lrows.push(0u32);
                    rrows.push(j as u32);
                }
                for i in 0..singles {
                    lrows.push((i + 1) as u32);
                    rrows.push((long_run + i) as u32);
                }
                let mut matched = Table::new(vars.to_vec());
                for (&li_row, &ri_row) in lrows.iter().zip(&rrows) {
                    matched.push_row(&[
                        left.cols[0][li_row as usize],
                        left.cols[1][li_row as usize],
                        right.cols[1][ri_row as usize],
                    ]);
                }

                let before_eval = crate::sparql::ctx::CHECKS.with(|c| c.get());
                let pass = exec::condition_mask(&ctx, &matched, &condition).unwrap();
                let eval_checks = crate::sparql::ctx::CHECKS.with(|c| c.get()) - before_eval;
                assert_eq!(pass.len(), total);

                let before_decide = crate::sparql::ctx::CHECKS.with(|c| c.get());
                decide_pairs(
                    &ctx,
                    &condition,
                    &vars,
                    &columns,
                    (&left, &right, 0, 0),
                    &mut lrows,
                    &mut rrows,
                )
                .unwrap();
                let decide_checks = crate::sparql::ctx::CHECKS.with(|c| c.get()) - before_decide;
                let chunks = total.div_ceil(1024);
                // Count and compact the pairs once each, gather every matched column,
                // and check at entry, after condition evaluation, and at exit.
                let expected_checks = eval_checks + chunks * (2 + columns.len()) + 3;
                assert!(
                    decide_checks >= expected_checks,
                    "{mode}: decide_pairs made {decide_checks} checks, expected at least {expected_checks}"
                );

                // Verify exact compacted pairs and unmatched extensions.
                let mut expected_l = Vec::new();
                let mut expected_r = Vec::new();
                match mode {
                    "all-pass" => {
                        for j in 0..long_run {
                            expected_l.push(0u32);
                            expected_r.push(j as u32);
                        }
                        for i in 0..singles {
                            expected_l.push((i + 1) as u32);
                            expected_r.push((long_run + i) as u32);
                        }
                    }
                    "all-fail" => {
                        for i in 0..=(singles as u32) {
                            expected_l.push(i);
                            expected_r.push(NO_ROW);
                        }
                    }
                    "alternating" => {
                        for j in (0..long_run).step_by(2) {
                            expected_l.push(0u32);
                            expected_r.push(j as u32);
                        }
                        for i in 0..singles {
                            let ri = long_run + i;
                            expected_l.push((i + 1) as u32);
                            expected_r.push(if ri.is_multiple_of(2) {
                                ri as u32
                            } else {
                                NO_ROW
                            });
                        }
                    }
                    _ => unreachable!(),
                }
                assert_eq!(lrows, expected_l, "{mode}: compacted left rows");
                assert_eq!(rrows, expected_r, "{mode}: compacted right rows");
            }

            // Exercise the subsequent decided output copy in Merge::interior above 1,024 rows.
            let copy_rows = 4096usize;
            let l_data: Vec<Vec<i64>> = (0..copy_rows as i64).map(|k| vec![k, 0]).collect();
            let r_data: Vec<Vec<i64>> = (0..copy_rows as i64).map(|k| vec![k, 10]).collect();
            let mut node = join(values(vec![0, 2], &l_data), values(vec![0, 3], &r_data));
            node.kind = Kind::LeftJoin {
                expr: Some(condition.clone()),
            };
            let mut op =
                Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
            let options = CursorOptions {
                batch_rows: copy_rows,
                ..Default::default()
            };
            let super::super::State::Merge(merge) = &mut op.state else {
                unreachable!()
            };
            assert!(
                merge.inputs[0]
                    .ensure(&ctx, &mut op.children[0], &options, copy_rows)
                    .unwrap()
            );
            assert!(
                merge.inputs[1]
                    .ensure(&ctx, &mut op.children[1], &options, copy_rows)
                    .unwrap()
            );

            // Measure pre-copy work (bounds + expand + decide_pairs) on the same prebuilt inputs.
            let before_pre = crate::sparql::ctx::CHECKS.with(|c| c.get());
            let (lt, rt) = (merge.inputs[0].table(), merge.inputs[1].table());
            let first = kernel::bounds(&ctx, &lt.cols[0], &rt.cols[0]).unwrap();
            let mut tl = Vec::with_capacity(copy_rows);
            let mut tr = Vec::with_capacity(copy_rows);
            let done = kernel::expand(
                &ctx,
                &lt.cols[0],
                &rt.cols[0],
                &first,
                0,
                true,
                copy_rows,
                |_, _| true,
                &mut tl,
                &mut tr,
            )
            .unwrap();
            assert_eq!(done, copy_rows);
            decide_pairs(
                &ctx,
                &condition,
                &vars,
                &merge.columns,
                (lt, rt, 0, 0),
                &mut tl,
                &mut tr,
            )
            .unwrap();
            let pre_checks = crate::sparql::ctx::CHECKS.with(|c| c.get()) - before_pre;

            let staged = [0, 2, 3, VarId::MAX];
            let mut out = Buffer::new(&ctx, &staged, copy_rows).unwrap();
            let before_interior = crate::sparql::ctx::CHECKS.with(|c| c.get());
            let step = merge.interior(&ctx, &mut out, copy_rows, true, true).unwrap();
            let interior_checks = crate::sparql::ctx::CHECKS.with(|c| c.get()) - before_interior;
            assert!(matches!(step, Step::Advanced));
            assert_eq!(out.table.len, copy_rows);
            assert!(out.table.cols[3].iter().all(|id| id.is_undef()));
            let copy_checks = copy_rows.div_ceil(1024) * staged.len() + 2;
            assert!(
                interior_checks >= pre_checks + copy_checks,
                "decided output copy made {} extra checks, expected at least {copy_checks}",
                interior_checks.saturating_sub(pre_checks),
            );
            for i in 0..copy_rows {
                assert_eq!(out.table.cols[0][i], Id::from_i64(i as i64).unwrap());
                assert_eq!(out.table.cols[1][i], Id::from_i64(0).unwrap());
                assert_eq!(out.table.cols[2][i], Id::from_i64(10).unwrap());
            }
        });
    }

    /// Wide width-31 variant of the crossing-run fixture with all 31 variables
    /// registered and a 512-row crossing run, checked across fixed budgets, constant
    /// and changing cap schedules, and held or released consumer batches.
    #[test]
    fn pair_conditions_wide_crossing_runs_stay_within_budget() {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let make_ctx = |limit: u64, pairs: bool| {
            let mut ctx = Ctx::new(Arc::clone(&snap));
            ctx.opt.merge_pair_conditions = pairs;
            for i in 0..31 {
                ctx.var(&format!("v{i}"));
            }
            ctx.mem_limit = limit;
            ctx.extension_failure = Some(Default::default());
            ctx.enable_cursor_accounting().unwrap();
            let ctx = Arc::new(ctx);
            ctx.attach_owner();
            assert_eq!(ctx.nvars(), 31);
            ctx
        };

        let left_vars: Vec<VarId> = (0..16).collect();
        let right_vars: Vec<VarId> = [0].into_iter().chain(16..31).collect();
        let wide_left = |key: i64, v1: i64, tag: i64| -> Vec<i64> {
            let mut row = Vec::with_capacity(16);
            row.push(key);
            row.push(v1);
            for col in 2..16 {
                row.push(tag * 16 + col as i64);
            }
            row
        };
        let wide_right = |key: i64, tag: i64| -> Vec<i64> {
            let mut row = Vec::with_capacity(16);
            row.push(key);
            for col in 1..16 {
                row.push(10_000 + tag * 16 + col as i64);
            }
            row
        };

        // 256 prefix left rows, key 300 crossing right batch 0->1 as 1 + 253,
        // key 400 crossing three batches on both sides with 512 rows each, and key 401.
        let mut left_rows = Vec::with_capacity(770);
        for k in 0..256i64 {
            left_rows.push(wide_left(k, 1, k));
        }
        left_rows.push(wide_left(300, 1, 300));
        for i in 0..512i64 {
            // Two rows pass `?v1 > 0` and 510 rows fail, exercising both passing pairs
            // and unmatched extensions across the 512x512 crossing run.
            let v1 = i64::from(i < 2);
            left_rows.push(wide_left(400, v1, 400 + i));
        }
        left_rows.push(wide_left(401, 1, 999));

        let mut right_rows = Vec::with_capacity(1024);
        for k in 0..253i64 {
            right_rows.push(wide_right(k, k));
        }
        right_rows.push(wide_right(256, 256));
        right_rows.push(wide_right(257, 257));
        for j in 0..254i64 {
            right_rows.push(wide_right(300, 300 + j));
        }
        right_rows.push(wide_right(301, 601));
        right_rows.push(wide_right(302, 602));
        for j in 0..512i64 {
            right_rows.push(wide_right(400, 1000 + j));
        }
        right_rows.push(wide_right(401, 2000));

        let condition = Expr::Cmp(
            Box::new(Expr::Var(1)),
            Box::new(Expr::Const(Id::from_i64(0).unwrap())),
            crate::sparql::expr::CmpOp::Gt,
        );
        let mut node = join(
            values(left_vars, &left_rows),
            values(right_vars, &right_rows),
        );
        node.kind = Kind::LeftJoin {
            expr: Some(condition),
        };
        assert_eq!(node.vars.len(), 31);

        let expected = rows(
            &exec::execute_materialized(&make_ctx(u64::MAX, false), &node)
                .unwrap()
                .0
                .table,
        );
        assert_eq!(expected.len(), 253 + 3 + 254 + 2 * 512 + 510 + 1);

        let schedules: [&[usize]; 2] = [&[256], &[256, 256, 2, 2]];
        for pairs in [false, true] {
            for limit in [512 << 10, 1 << 20, u64::MAX] {
                for caps in schedules {
                    for hold in [false, true] {
                        let ctx = make_ctx(limit, pairs);
                        let mut op = Operator::build(
                            &ctx,
                            node.clone(),
                            FallbackPolicy::RejectMaterialization,
                        )
                        .unwrap();
                        assert!(matches!(op.state, super::super::State::Merge(_)));
                        let (mut out, mut previous) = (Vec::new(), None);
                        for pull in 0.. {
                            let cap = caps[pull % caps.len()];
                            let options = CursorOptions {
                                batch_rows: cap,
                                ..Default::default()
                            };
                            let Some(batch) = op.next(&ctx, &options, cap).unwrap() else {
                                break;
                            };
                            assert!(batch.table.len <= cap && batch.table.len > 0);
                            assert_eq!(batch.table.width(), 31);
                            assert!(batch.charge.bytes() >= capacity_bytes(&batch.table));
                            let table = &batch.table;
                            out.extend(
                                (0..table.len).map(|i| table.cols.iter().map(|c| c[i]).collect()),
                            );
                            if hold {
                                previous = Some(batch);
                            }
                        }
                        drop(previous);
                        let case = format!("pairs {pairs} limit {limit} caps {caps:?} hold {hold}");
                        assert!(
                            ctx.mem_peak() <= limit,
                            "{case}: peak {} > {limit}",
                            ctx.mem_peak()
                        );
                        let keys: Vec<Id> = out.iter().map(|row: &Vec<Id>| row[0]).collect();
                        assert!(keys.is_sorted(), "{case}");
                        out.sort();
                        assert_eq!(out, expected, "{case}");
                    }
                }
            }
        }
    }

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
