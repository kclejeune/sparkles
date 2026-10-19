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
use crate::sparql::exec;
use crate::sparql::plan::{JoinAlgo, Kind, Node};
use crate::sparql::table::{Table, VarId};
use crate::sparql::zipper;
use crate::spill::{SpillKind, SpillReader, SpillRun, SpillWriter};
use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;

/// No right row: the OPTIONAL extension of an unmatched left row.
const NO_ROW: u32 = u32::MAX;

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
    inputs: [Input; 2],
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
                match reader.as_mut().expect("a started run").next_frame()? {
                    Some((columns, n)) => {
                        *frame = columns;
                        Ok(Some(n))
                    }
                    None => {
                        frame.clear();
                        Ok(None)
                    }
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
    fn new(ctx: &Arc<Ctx>, left: Run, right: Run, inputs: &[Input; 2]) -> Result<Self> {
        let mut left = Side::new(ctx, left, &inputs[0])?;
        let mut right = Side::new(ctx, right, &inputs[1])?;
        left.restart()?;
        right.restart()?;
        // A frame of each file, decoded and as read.
        let frame = |side: &Side, width: usize| match side {
            Side::File { run, .. } => {
                run.rows().div_ceil(run.frames().max(1)) * width as u64 * 8 * 2
            }
            Side::Rows(_) => 0,
        };
        let charge = ctx.retained_charge(
            frame(&left, inputs[0].table().width())
                + frame(&right, inputs[1].table().width())
                + 1024,
        )?;
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
                if optional && !self.matched {
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
    pub(super) fn new(ctx: &Ctx, node: &Node) -> Result<Self> {
        let keys = [key(node).expect("eligible merge key")];
        let charge =
            ctx.retained_charge(node.vars.len() as u64 * 64 + keys.len() as u64 * 32 + 1024)?;
        let (left, right) = (
            positions(&node.vars, &node.children[0].vars),
            positions(&node.vars, &node.children[1].vars),
        );
        let columns = left.into_iter().zip(right).collect();
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
                    let first = zipper::lower_bounds(ctx, &lk[li..end], &rk[ri..])?;
                    // Which left rows pass, decided without a branch per row, since
                    // interleaved keys make the outcome unpredictable.
                    let right = &rk[ri..];
                    let keep: Vec<bool> = first
                        .iter()
                        .zip(&lk[li..end])
                        .map(|(&at, &key)| {
                            let at = at as usize;
                            let hit = at < right.len() && right[at.min(right.len() - 1)] == key;
                            hit == keep_matched
                        })
                        .collect();
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

    pub(super) fn supports_count(&self) -> bool {
        self.columns
            .iter()
            .filter(|(l, r)| l.is_some() && r.is_some())
            .count()
            == 1
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
    ) -> Result<bool> {
        let (left, right) = (self.inputs[0].table(), self.inputs[1].table());
        let (lk, rk) = (&left.cols[self.keys[0][0]], &right.cols[self.keys[1][0]]);
        let (li, ri) = (self.inputs[0].at, self.inputs[1].at);
        let last = rk[rk.len() - 1];
        let covered = if right_done {
            lk.len()
        } else {
            li + lk[li..].partition_point(|k| *k < last)
        };
        let room = cap - out.table.len;
        let end = covered.min(li.saturating_add(room));
        let (mut next_li, mut next_ri) = (li, ri);
        if end > li && room > 0 {
            let _scratch = ctx.charge((end - li) as u64 * 4 + room as u64 * 8 + 128)?;
            let first = zipper::lower_bounds(ctx, &lk[li..end], &rk[ri..])?;
            let rkeys = &rk[ri..];
            let mut lrows: Vec<u32> = Vec::with_capacity(room);
            let mut rrows: Vec<u32> = Vec::with_capacity(room);
            let mut run = (usize::MAX, 0usize);
            let mut done = 0;
            for (j, (&at, &key)) in first.iter().zip(&lk[li..end]).enumerate() {
                if j.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let at = at as usize;
                let row = (li + j) as u32;
                if at < rkeys.len() && rkeys[at] == key {
                    if run.0 != at {
                        run = (at, exec::run_end(rkeys, at));
                    }
                    let n = run.1 - at;
                    if lrows.len() + n > room {
                        break;
                    }
                    lrows.extend(std::iter::repeat_n(row, n));
                    rrows.extend((ri + at) as u32..(ri + run.1) as u32);
                } else if self.optional {
                    if lrows.len() == room {
                        break;
                    }
                    lrows.push(row);
                    rrows.push(NO_ROW);
                }
                done = j + 1;
            }
            if done > 0 {
                for (column, &(l, r)) in out.table.cols.iter_mut().zip(&self.columns) {
                    match (l, r) {
                        (Some(c), _) => {
                            let source = &left.cols[c];
                            column.extend(lrows.iter().map(|&i| source[i as usize]));
                        }
                        (None, Some(c)) => {
                            let source = &right.cols[c];
                            column.extend(
                                rrows
                                    .iter()
                                    .map(|&j| source.get(j as usize).copied().unwrap_or(Id::UNDEF)),
                            );
                        }
                        (None, None) => column.extend(std::iter::repeat_n(Id::UNDEF, lrows.len())),
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
        Ok((li, ri) != (next_li, next_ri))
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
                    let _scratch =
                        ctx.charge((covered - li) as u64 * 4 + rkeys.len() as u64 * 4 + 128)?;
                    let first = zipper::lower_bounds(ctx, &lc[li..covered], rkeys)?;
                    // The length of the run of equal keys that starts at each right row.
                    let mut runs = vec![1u32; rkeys.len() + 1];
                    runs[rkeys.len()] = 0;
                    for j in (0..rkeys.len().saturating_sub(1)).rev() {
                        runs[j] = 1 + u32::from(rkeys[j] == rkeys[j + 1]) * runs[j + 1];
                    }
                    let unmatched = u64::from(self.optional);
                    let mut n = 0u64;
                    for (&at, &key) in first.iter().zip(&lc[li..covered]) {
                        let at = at as usize;
                        let hit = at < rkeys.len() && rkeys[at.min(rkeys.len() - 1)] == key;
                        n += u64::from(hit) * u64::from(runs[at]) + u64::from(!hit) * unmatched;
                    }
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
            if let Some(spooled) = &mut self.spooled {
                spooled.fill(ctx, &self.columns, self.optional, &mut out, cap)?;
                if spooled.done {
                    self.spooled = None;
                }
                continue;
            }
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
                if self.supports_count() && self.interior(ctx, &mut out, cap, children[1].done)? {
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
                if left.spooled.is_some() || right.spooled.is_some() {
                    for run in [&left, &right] {
                        if let Some(file) = &run.spooled {
                            self.spilled += file.bytes();
                            self.spooled_runs += 1;
                        }
                    }
                    let spooled = SpooledPair::new(ctx, left, right, &self.inputs)?;
                    self.spooled = Some(Box::new(spooled));
                    continue;
                }
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
            for optional in [false, true] {
                let mut node = join(values(vec![0, 1, 2], left), values(vec![0, 1, 3], right));
                if optional {
                    node.kind = Kind::LeftJoin { expr: None };
                }
                let expected = rows(&exec::execute(&context(u64::MAX), &node).unwrap().0);
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
                        ctx.attach_cursor_owner();
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
                        "optional {optional} cap {cap}"
                    );
                    assert_eq!(result, expected, "optional {optional} cap {cap}");
                }
            }
        }
        assert!(spooled >= 12, "{spooled} runs spooled");
        assert_eq!(spill.reserved_bytes(), 0);
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
