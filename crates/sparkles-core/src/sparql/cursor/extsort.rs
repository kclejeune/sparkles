//! External merge sort for a cursor's blocking ORDER BY and Sort (X08 §6.4).
//!
//! The sort reads its input into a charged buffer. When the buffer reaches its share of
//! the memory budget, or a charge for it fails, the buffer is sorted with the in-memory
//! sort and written to a spill file as a run. The merge evaluates an ORDER BY key that
//! is a pure expression again on each frame it reads, so its computed values are never
//! interned and need no memory beyond the frame. An impure key, such as one that calls
//! RAND, is evaluated once before its run is sorted into a hidden key column that the
//! run keeps, so the merge compares the values the sort compared.
//! Runs are merged at most [`FAN_IN`] at a time, each with one frame in memory, and
//! more runs merge in passes. Equal keys keep their input order: a run holds earlier
//! input than every later run, and the merge takes the earliest run on a tie. An input
//! that fits never touches the disk and sorts as before.
use super::{Buffer, capacity_bytes, merge};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::exec;
use crate::sparql::expr::Expr;
use crate::sparql::exprcache::Report;
use crate::sparql::plan::{Kind, Node};
use crate::sparql::sortkey::SortKey;
use crate::sparql::table::VarId;
use crate::spill::{QuerySpill, SpillKind, SpillReader, SpillRun};
use std::cmp::Ordering;
use std::sync::Arc;

/// The most runs merged at once.
pub(super) const FAN_IN: usize = 64;
/// The least buffer that a run holds before it is written.
const MIN_RUN_BYTES: u64 = 64 << 10;
/// The fewest rows a run holds unless its input ends.
const MIN_PIECE_ROWS: usize = 256;
/// The fewest rows a run's frames hold.
const MIN_FRAME_ROWS: usize = 64;

/// Whether a blocking node can sort externally: a Sort, or an ORDER BY without LIMIT.
pub(super) fn eligible(node: &Node) -> bool {
    matches!(node.kind, Kind::Sort(_) | Kind::OrderBy { limit: None, .. })
        && node.children.len() == 1
}

/// How two rows of a run compare.
enum Keys {
    /// Raw id order on these columns, for a Sort.
    Raw(Vec<usize>),
    /// ORDER BY: each key and its direction, over a run's columns `vars`.
    Values {
        keys: Vec<(Key, bool)>,
        vars: Vec<VarId>,
    },
}

/// Where an ORDER BY key's values come from in a run.
enum Key {
    /// The value of the id in this column.
    Column(usize),
    /// A pure expression over the run's columns.
    Expr(Expr),
}

pub(super) struct External {
    query: Arc<QuerySpill>,
    /// The input's variables and then the hidden key columns.
    columns: Vec<VarId>,
    /// How many of `columns` are the input's.
    width: usize,
    /// The expressions of the hidden key columns.
    hidden: Vec<Expr>,
    /// The node that sorts one run in memory, on the key columns.
    sort: Node,
    keys: Keys,
    runs: Vec<Arc<SpillRun>>,
    /// The sort footprint at which a run is written.
    threshold: u64,
    /// The rows of a run's frames, which let [`FAN_IN`] frames share a quarter of the
    /// room the query had when the sort started.
    frame_rows: usize,
    merge: Option<Merger>,
    /// What the plan reports.
    pub(super) written: u64,
    pub(super) run_count: usize,
    pub(super) passes: usize,
}

impl External {
    /// An external sort for the blocking `node`, or `None` when the query has no spill.
    pub(super) fn new(ctx: &Ctx, node: &Node, input: &[VarId]) -> Option<Self> {
        let query = ctx.spill.clone()?;
        let mut columns = input.to_vec();
        let mut hidden = Vec::new();
        let mut sort = node.clone();
        sort.children.clear();
        let keys = match &node.kind {
            Kind::Sort(vars) => Keys::Raw(
                vars.iter()
                    .filter_map(|v| input.iter().position(|x| x == v))
                    .collect(),
            ),
            Kind::OrderBy { keys, .. } => {
                let mut sorted = Vec::with_capacity(keys.len());
                let mut rewritten = Vec::with_capacity(keys.len());
                for (i, (expr, asc)) in keys.iter().enumerate() {
                    let key = match expr {
                        Expr::Var(v) if input.contains(v) => {
                            Key::Column(input.iter().position(|x| x == v).expect("an input column"))
                        }
                        _ if crate::sparql::exprcache::impurity(expr).is_none() => {
                            Key::Expr(expr.clone())
                        }
                        _ => {
                            let var = ctx.var(&format!("\u{1}spill key {i}"));
                            columns.push(var);
                            hidden.push(expr.clone());
                            Key::Column(columns.len() - 1)
                        }
                    };
                    rewritten.push(match &key {
                        Key::Column(c) => (Expr::Var(columns[*c]), *asc),
                        Key::Expr(e) => (e.clone(), *asc),
                    });
                    sorted.push((key, *asc));
                }
                sort.kind = Kind::OrderBy {
                    keys: rewritten,
                    limit: None,
                };
                Keys::Values {
                    keys: sorted,
                    vars: columns.clone(),
                }
            }
            _ => return None,
        };
        sort.vars = columns.clone();
        Some(Self {
            query,
            width: input.len(),
            columns,
            hidden,
            sort,
            keys,
            runs: Vec::new(),
            threshold: 0,
            frame_rows: crate::spill::FRAME_ROWS,
            merge: None,
            written: 0,
            run_count: 0,
            passes: 0,
        })
    }

    pub(super) fn spilled(&self) -> bool {
        !self.runs.is_empty() || self.merge.is_some()
    }

    /// Fix the memory of a run's sort from the room the query has when its input
    /// starts. Half of the room is left for the operators around the sort.
    pub(super) fn start(&mut self, ctx: &Ctx) {
        if self.threshold == 0 {
            let room = ctx.memory_room();
            self.threshold = (room / 2).max(MIN_RUN_BYTES);
            let per_head = (room / 4) / (FAN_IN as u64 * self.row_bytes());
            self.frame_rows = usize::try_from(per_head).map_or(crate::spill::FRAME_ROWS, |rows| {
                rows.clamp(MIN_FRAME_ROWS, crate::spill::FRAME_ROWS)
            });
        }
    }

    /// The memory sorting `input` in memory takes. The in-memory sort holds the
    /// buffer and the hidden key columns, their reordered copies and row indices, and
    /// for each key a decoded value and a slot per row, which the sort charges at up to
    /// 136 bytes a row.
    fn footprint(&self, input: &Buffer) -> u64 {
        let rows = input.table.len as u64;
        capacity_bytes(&input.table)
            .saturating_mul(3)
            .saturating_add(
                rows.saturating_mul(
                    32 + 24 * self.hidden.len() as u64 + 160 * self.key_count() as u64,
                ),
            )
    }

    /// The bytes a decoded row of a run takes in the merge, with its key values.
    fn row_bytes(&self) -> u64 {
        self.columns.len() as u64 * 8 + self.key_count() as u64 * 64 + 8
    }

    /// Whether `input` should be written as a run now.
    pub(super) fn full(&self, input: &Buffer) -> bool {
        self.footprint(input) >= self.threshold
    }

    /// Sort `input` and write it as runs. The rows are sorted in pieces that the room
    /// left now should hold, and a piece that still runs out of memory is tried again
    /// at half its size, while the input stays in its buffer.
    pub(super) fn write_run(&mut self, ctx: &Arc<Ctx>, input: Buffer) -> Result<()> {
        let Buffer {
            mut table,
            mut charge,
        } = input;
        if table.is_empty() {
            return Ok(());
        }
        let mut report = Report::default();
        // Impure keys are evaluated once on the whole buffer, before the rows move.
        for (expr, &var) in self.hidden.iter().zip(&self.columns[self.width..]) {
            charge.resize(capacity_bytes(&table).saturating_add(table.len as u64 * 8 + 64))?;
            let column = exec::compute_column(ctx, &table, expr, &mut report)?;
            table.vars.push(var);
            table.cols.push(column);
        }
        charge.resize(capacity_bytes(&table))?;
        let per_row = (self.columns.len() as u64 * 24 + 32 + 160 * self.key_count() as u64).max(1);
        let mut piece = usize::try_from(ctx.memory_room() / 2 / per_row)
            .unwrap_or(usize::MAX)
            .clamp(MIN_PIECE_ROWS, table.len.max(MIN_PIECE_ROWS));
        let mut at = 0;
        while at < table.len {
            let n = piece.min(table.len - at);
            match self.sort_piece(ctx, &table, at..at + n, &mut report) {
                Ok(()) => at += n,
                Err(Error::BudgetExceeded(b))
                    if matches!(
                        b.kind,
                        crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
                    ) && n > MIN_PIECE_ROWS
                        && ctx.check_cursor_memory().is_ok() =>
                {
                    piece = (n / 2).max(MIN_PIECE_ROWS);
                }
                Err(error) => return Err(error),
            }
        }
        drop(charge);
        Ok(())
    }

    /// Copy `rows` of `table`, sort them in memory and write them as one run.
    fn sort_piece(
        &mut self,
        ctx: &Arc<Ctx>,
        table: &crate::sparql::table::Table,
        rows: std::ops::Range<usize>,
        report: &mut Report,
    ) -> Result<()> {
        let n = rows.len();
        let mut piece = Buffer::new(ctx, &table.vars, n)?;
        for (to, from) in piece.table.cols.iter_mut().zip(&table.cols) {
            to.extend_from_slice(&from[rows.clone()]);
        }
        piece.table.len = n;
        piece.reconcile()?;
        let Buffer {
            table: piece,
            charge,
        } = piece;
        // The in-memory sort charges its own index, copy and key values.
        let sorted = exec::apply_blocking(ctx, &self.sort, piece, report)?;
        let columns: Vec<&[Id]> = self
            .columns
            .iter()
            .map(|v| {
                let c = sorted.col_of(*v).expect("a run column");
                sorted.cols[c].as_slice()
            })
            .collect();
        let mut writer = self.query.create(SpillKind::SortRun, self.columns.len())?;
        writer.set_frame_rows(self.frame_rows);
        let mut at = 0;
        while at < sorted.len {
            ctx.check()?;
            let n = (sorted.len - at).min(crate::spill::FRAME_ROWS * 16);
            let part: Vec<&[Id]> = columns.iter().map(|c| &c[at..at + n]).collect();
            writer.write(&part, n)?;
            at += n;
        }
        let run = writer.finish()?;
        self.written = self.written.saturating_add(run.bytes());
        self.run_count += 1;
        self.runs.push(Arc::new(run));
        drop(charge);
        Ok(())
    }

    /// Merge the runs down to [`FAN_IN`] or to what memory holds, then start the
    /// final merge.
    pub(super) fn finish(&mut self, ctx: &Arc<Ctx>) -> Result<()> {
        let frame = (self.frame_rows as u64)
            .saturating_mul(self.row_bytes())
            .saturating_add(4096);
        let fan_in = ((ctx.memory_room() / 2) / frame.max(1)).clamp(2, FAN_IN as u64) as usize;
        while self.runs.len() > fan_in {
            ctx.check()?;
            let group: Vec<Arc<SpillRun>> = self.runs.drain(..fan_in).collect();
            let mut merger = Merger::new(ctx, group, &self.keys, self.columns.len())?;
            let mut writer = self.query.create(SpillKind::SortRun, self.columns.len())?;
            writer.set_frame_rows(self.frame_rows);
            let mut out: Vec<Vec<Id>> = vec![Vec::new(); self.columns.len()];
            loop {
                for c in &mut out {
                    c.clear();
                }
                let n = merger.next(ctx, &self.keys, &mut out, self.frame_rows)?;
                if n == 0 {
                    break;
                }
                let part: Vec<&[Id]> = out.iter().map(|c| c.as_slice()).collect();
                writer.write(&part, n)?;
            }
            drop(merger);
            let run = writer.finish()?;
            self.written = self.written.saturating_add(run.bytes());
            self.passes += 1;
            // The merged runs held the earliest input, so the new run goes first.
            self.runs.insert(0, Arc::new(run));
        }
        let runs = std::mem::take(&mut self.runs);
        self.merge = Some(Merger::new(ctx, runs, &self.keys, self.columns.len())?);
        self.passes += 1;
        Ok(())
    }

    fn key_count(&self) -> usize {
        match &self.keys {
            Keys::Raw(c) => c.len(),
            Keys::Values { keys, .. } => keys.len(),
        }
    }

    /// The next sorted rows of the input's columns, at most `cap`.
    pub(super) fn next(&mut self, ctx: &Arc<Ctx>, cap: usize) -> Result<Option<Buffer>> {
        let merger = self.merge.as_mut().expect("a started merge");
        // The merged rows are copied once more into the batch, so a batch takes at most
        // a quarter of the room left (item 85).
        let row = self.columns.len() as u64 * 8 + 8;
        let fit = usize::try_from(ctx.memory_room() / 4 / row).unwrap_or(usize::MAX);
        let cap = cap.min(fit.max(MIN_FRAME_ROWS));
        let mut out: Vec<Vec<Id>> = vec![Vec::new(); self.columns.len()];
        let n = merger.next(ctx, &self.keys, &mut out, cap)?;
        if n == 0 {
            return Ok(None);
        }
        let mut buffer = Buffer::new(ctx, &self.columns[..self.width], n)?;
        for (to, from) in buffer.table.cols.iter_mut().zip(&out) {
            to.extend_from_slice(from);
        }
        buffer.table.len = n;
        buffer.reconcile()?;
        Ok(Some(buffer))
    }
}

/// One run's place in the merge: its current frame and the frame's key values.
struct Head {
    reader: SpillReader,
    columns: Vec<Vec<Id>>,
    keys: Vec<Vec<SortKey>>,
    at: usize,
    len: usize,
    _charge: Option<OwnedCharge>,
}

impl Head {
    /// Read the next frame, or return false at the end of the run.
    fn load(&mut self, ctx: &Arc<Ctx>, keys: &Keys) -> Result<bool> {
        self._charge = None;
        let Some((columns, len)) = self.reader.next_frame()? else {
            self.columns.clear();
            self.keys.clear();
            self.len = 0;
            self.at = 0;
            return Ok(false);
        };
        let mut bytes = columns.len() as u64 * len as u64 * 8 + 256;
        let (columns, decoded) = match keys {
            Keys::Raw(_) => (columns, Vec::new()),
            Keys::Values { keys, vars } => {
                let mut decoded = Vec::with_capacity(keys.len());
                let mut columns = Some(columns);
                for (key, _) in keys {
                    let column: Vec<SortKey> = match key {
                        Key::Column(c) => columns.as_ref().expect("frame columns")[*c]
                            .iter()
                            .map(|&id| SortKey::new(ctx.value(id)))
                            .collect(),
                        Key::Expr(e) => {
                            let mut table = crate::sparql::table::Table::new(vars.clone());
                            table.cols = columns.take().expect("frame columns");
                            table.len = len;
                            let values = exec::key_values(ctx, &table, e);
                            columns = Some(std::mem::take(&mut table.cols));
                            values?.into_iter().map(SortKey::new).collect()
                        }
                    };
                    bytes = bytes.saturating_add(
                        column
                            .iter()
                            .map(|k| k.payload_bytes() + size_of::<SortKey>() as u64)
                            .sum::<u64>(),
                    );
                    decoded.push(column);
                }
                (columns.expect("frame columns"), decoded)
            }
        };
        self._charge = Some(OwnedCharge::new(ctx, bytes)?);
        self.columns = columns;
        self.keys = decoded;
        self.len = len;
        self.at = 0;
        Ok(true)
    }
}

/// A k-way merge of sorted runs. The heap holds the runs that have rows, ordered by
/// their current row and then by run, so that equal keys come from earlier runs first.
struct Merger {
    heads: Vec<Head>,
    heap: Vec<usize>,
    _runs: Vec<Arc<SpillRun>>,
}

fn compare(keys: &Keys, a: &Head, b: &Head) -> Ordering {
    match keys {
        Keys::Raw(columns) => {
            for &c in columns {
                match a.columns[c][a.at].cmp(&b.columns[c][b.at]) {
                    Ordering::Equal => continue,
                    o => return o,
                }
            }
            Ordering::Equal
        }
        Keys::Values { keys, .. } => {
            for (k, &(_, asc)) in keys.iter().enumerate() {
                let o = a.keys[k][a.at].cmp(&b.keys[k][b.at]);
                let o = if asc { o } else { o.reverse() };
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        }
    }
}

impl Merger {
    fn new(ctx: &Arc<Ctx>, runs: Vec<Arc<SpillRun>>, keys: &Keys, width: usize) -> Result<Self> {
        let mut heads = Vec::with_capacity(runs.len());
        for run in &runs {
            let mut head = Head {
                reader: run.reader()?,
                columns: Vec::with_capacity(width),
                keys: Vec::new(),
                at: 0,
                len: 0,
                _charge: None,
            };
            head.load(ctx, keys)?;
            heads.push(head);
        }
        let mut merger = Merger {
            heap: (0..heads.len()).filter(|&i| heads[i].len > 0).collect(),
            heads,
            _runs: runs,
        };
        for i in (0..merger.heap.len() / 2).rev() {
            merger.sift_down(keys, i);
        }
        Ok(merger)
    }

    fn less(&self, keys: &Keys, a: usize, b: usize) -> bool {
        match compare(keys, &self.heads[a], &self.heads[b]) {
            Ordering::Less => true,
            Ordering::Greater => false,
            Ordering::Equal => a < b,
        }
    }

    fn sift_down(&mut self, keys: &Keys, mut i: usize) {
        let n = self.heap.len();
        loop {
            let (l, r) = (2 * i + 1, 2 * i + 2);
            let mut least = i;
            if l < n && self.less(keys, self.heap[l], self.heap[least]) {
                least = l;
            }
            if r < n && self.less(keys, self.heap[r], self.heap[least]) {
                least = r;
            }
            if least == i {
                return;
            }
            self.heap.swap(i, least);
            i = least;
        }
    }

    /// Append the next rows, at most `cap`, to `out`, and return how many.
    fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        keys: &Keys,
        out: &mut [Vec<Id>],
        cap: usize,
    ) -> Result<usize> {
        let mut n = 0;
        while n < cap && !self.heap.is_empty() {
            if n.is_multiple_of(1024) {
                ctx.check()?;
            }
            let top = self.heap[0];
            if self.heap.len() == 1 {
                // The last run copies its frame without comparing.
                let head = &mut self.heads[top];
                let take = (head.len - head.at).min(cap - n);
                for (o, c) in out.iter_mut().zip(&head.columns) {
                    o.extend_from_slice(&c[head.at..head.at + take]);
                }
                head.at += take;
                n += take;
            } else {
                let head = &mut self.heads[top];
                for (o, c) in out.iter_mut().zip(&head.columns) {
                    o.push(c[head.at]);
                }
                head.at += 1;
                n += 1;
            }
            let head = &mut self.heads[top];
            if head.at == head.len && !head.load(ctx, keys)? {
                let last = self.heap.len() - 1;
                self.heap.swap(0, last);
                self.heap.pop();
            }
            if !self.heap.is_empty() {
                self.sift_down(keys, 0);
            }
        }
        Ok(n)
    }
}

/// Read the whole input of a blocking sort, spilling sorted runs when it outgrows its
/// share of memory. Returns the in-memory input when no run was written.
pub(super) fn load(
    ctx: &Arc<Ctx>,
    external: &mut External,
    child: &mut super::Operator,
    options: &super::CursorOptions,
) -> Result<Option<Buffer>> {
    let mut input = Buffer::new(ctx, &child.vars, 0)?;
    input.table.sorted.clone_from(&child.order);
    external.start(ctx);
    while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
        match merge::append(ctx, &mut input, &batch.table, 0..batch.table.len) {
            Ok(()) => {}
            // A charge that fails for want of memory writes what the buffer holds and
            // starts a new one.
            Err(Error::BudgetExceeded(b))
                if matches!(
                    b.kind,
                    crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
                ) && !input.table.is_empty() =>
            {
                let full = std::mem::replace(&mut input, Buffer::new(ctx, &child.vars, 0)?);
                // Later runs aim lower, since something else holds the memory.
                external.threshold = (external.threshold / 2).max(MIN_RUN_BYTES);
                external.write_run(ctx, full)?;
                merge::append(ctx, &mut input, &batch.table, 0..batch.table.len)?;
            }
            // With nothing buffered, the batch itself, which its charge already holds,
            // becomes a run (item 85).
            Err(Error::BudgetExceeded(b))
                if matches!(
                    b.kind,
                    crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
                ) && ctx.spill.is_some() =>
            {
                external.threshold = (external.threshold / 2).max(MIN_RUN_BYTES);
                external.write_run(ctx, batch)?;
                continue;
            }
            Err(error) => return Err(error),
        }
        if external.full(&input) {
            let full = std::mem::replace(&mut input, Buffer::new(ctx, &child.vars, 0)?);
            external.write_run(ctx, full)?;
        }
    }
    if !external.spilled() {
        return Ok(Some(input));
    }
    external.write_run(ctx, input)?;
    external.finish(ctx)?;
    Ok(None)
}
