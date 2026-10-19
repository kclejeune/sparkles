//! Hash joins whose build input outgrows memory (X08 §6.3).
//!
//! A join of a query that may spill keeps its build input to half the room the query
//! had when the build started. When the build input passes that, or a charge for it or
//! for its hash table fails, the join picks its key column as the in-memory join does
//! and writes the build rows to [`JoinSpill::fanout`] files by a seeded hash of the key.
//! Build rows that leave the key unbound are compatible with every probe row, so they
//! stay in memory. The probe input is then written to files by the same hash, and probe
//! rows that leave the key unbound go to a file of their own.
//!
//! Each partition is then joined in turn by the in-memory kernels of [`Binary`]: its
//! build file, with the unbound build rows, is read into a table with a hash index, and
//! its probe file streams through it. A build file that does not fit is partitioned
//! again with the next seed, up to [`MAX_LEVEL`] levels. A partition that holds nine
//! tenths of its parent's build rows after that holds a heavy key, and it joins as a
//! block nested loop: the build file is read in chunks that fit, the probe file is read
//! once per chunk, and a bitmap of the probe rows that matched decides, after the last
//! chunk, which rows OPTIONAL, semi, anti and MINUS joins emit alone. The unbound probe
//! rows are joined in the same way against every build file.
//!
//! Output order changes when a join spills. An operator above may rely on the order of
//! the probe input, which the cursor fixes when it is built, so such a join numbers its
//! probe rows, writes the output of every pass with the probe row's number, and merges
//! those files by number at the end, which restores the probe input's order. A join
//! whose order nothing above relies on emits each partition's output as it is made.
use super::binary::{Binary, HashIndex, Mode, NO_ROW, gather};
use super::{Buffer, CursorOptions, Operator, merge};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::table::{Table, VarId};
use crate::spill::{QuerySpill, SpillKind, SpillReader, SpillRun, SpillWriter};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// The least memory a spilled join plans for, whatever room the query has left.
pub(super) const MIN_BUDGET: u64 = 64 << 10;
/// The fewest and most files one partitioning writes.
const MIN_FANOUT: usize = 8;
const MAX_FANOUT: usize = 64;
/// How many times a partition's rows may be partitioned again before it joins as a
/// block nested loop.
pub(super) const MAX_LEVEL: u64 = 3;
/// The fewest rows of a frame, whatever the memory.
const MIN_FRAME_ROWS: usize = 64;
/// The most files an ordered join's output merge reads at once.
const FAN_IN: usize = 64;
/// The fewest rows of an output file's frames.
const MIN_SPOOL_ROWS: usize = 16;

/// The partition of `key` at partitioning level `level`, among `fanout` partitions.
fn partition(key: Id, level: u64, fanout: usize) -> usize {
    let mut hasher = rustc_hash::FxHasher::default();
    (level.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2545_f491_4f6c_dd1d).hash(&mut hasher);
    key.hash(&mut hasher);
    // The high bits mix best.
    (hasher.finish() >> 58) as usize % fanout
}

/// The bytes a table of `rows` rows of `width` columns and a hash index over it may
/// take while they are built: the columns, the index's grouped rows and slots, and its
/// key table with the old buckets of a growth.
fn chunk_row_bytes(width: usize) -> u64 {
    width as u64 * 8 + 12 + 51
}

/// Rows buffered for one spill file until they fill a frame.
struct Part {
    columns: Vec<Vec<Id>>,
    writer: Option<SpillWriter>,
}

/// The files one partitioning writes, each fed whole frames.
struct Parts {
    query: Arc<QuerySpill>,
    kind: SpillKind,
    width: usize,
    frame_rows: usize,
    parts: Vec<Part>,
    _charge: Option<RetainedCharge>,
}

impl Parts {
    fn new(
        ctx: &Ctx,
        query: &Arc<QuerySpill>,
        width: usize,
        count: usize,
        frame_rows: usize,
    ) -> Result<Self> {
        // Each file buffers a frame of rows, and its writer an encoded frame.
        let charge = ctx.retained_charge(
            count as u64 * (frame_rows as u64 * width.max(1) as u64 * 8 * 2 + 1024),
        )?;
        Ok(Self {
            query: query.clone(),
            kind: SpillKind::JoinPartition,
            width,
            frame_rows,
            parts: (0..count)
                .map(|_| Part {
                    columns: vec![Vec::new(); width],
                    writer: None,
                })
                .collect(),
            _charge: charge,
        })
    }

    /// Add row `row` of `columns`, and `extra` after its columns, to file `part`.
    fn push(
        &mut self,
        part: usize,
        columns: &[Vec<Id>],
        row: usize,
        extra: Option<Id>,
    ) -> Result<()> {
        let target = &mut self.parts[part];
        for (to, from) in target.columns.iter_mut().zip(columns) {
            to.push(from[row]);
        }
        if let Some(extra) = extra {
            target.columns[columns.len()].push(extra);
        }
        if target.columns[0].len() >= self.frame_rows {
            self.flush(part)?;
        }
        Ok(())
    }

    fn flush(&mut self, part: usize) -> Result<()> {
        let target = &mut self.parts[part];
        let rows = target.columns.first().map_or(0, Vec::len);
        if rows == 0 {
            return Ok(());
        }
        if target.writer.is_none() {
            let mut writer = self.query.create(self.kind, self.width)?;
            writer.set_frame_rows(self.frame_rows);
            target.writer = Some(writer);
        }
        let slices: Vec<&[Id]> = target.columns.iter().map(Vec::as_slice).collect();
        target
            .writer
            .as_mut()
            .expect("an open spill file")
            .write(&slices, rows)?;
        for column in &mut target.columns {
            column.clear();
        }
        Ok(())
    }

    /// The finished files, `None` for a partition that received no row, and the bytes
    /// they take.
    fn finish(mut self) -> Result<(Vec<Option<Arc<SpillRun>>>, u64)> {
        let mut runs = Vec::with_capacity(self.parts.len());
        let mut bytes = 0;
        for part in 0..self.parts.len() {
            self.flush(part)?;
            let run = match self.parts[part].writer.take() {
                Some(writer) => {
                    let run = writer.finish()?;
                    bytes += run.bytes();
                    Some(Arc::new(run))
                }
                None => None,
            };
            runs.push(run);
        }
        Ok((runs, bytes))
    }
}

/// One partition's join: probe rows and the build files whose rows are their
/// candidates.
struct Job {
    builds: Vec<Arc<SpillRun>>,
    probe: Arc<SpillRun>,
    /// How many times the rows were partitioned, which seeds the next partitioning.
    level: u64,
    /// Whether the probe rows bind the key, so that a hash index on it finds their
    /// candidates and the job may partition again. The unbound probe rows meet every
    /// build row.
    keyed: bool,
}

impl Job {
    fn build_rows(&self) -> u64 {
        self.builds.iter().map(|run| run.rows()).sum()
    }
}

/// The build rows of a job, read in chunks.
struct Chunks {
    runs: Vec<Arc<SpillRun>>,
    at: usize,
    reader: Option<SpillReader>,
    /// Whether the first chunk was read, which carries the unbound build rows.
    started: bool,
}

/// A job being joined: its current chunk of build rows sits in the [`Binary`] kernel.
struct Pass {
    job: Job,
    chunks: Chunks,
    /// Whether the build rows take more than one chunk, so that probe rows' verdicts
    /// wait for the last chunk.
    deferred: bool,
    /// The probe rows that a build row matched, by row of the probe file, when the
    /// verdicts wait.
    matched: Vec<u64>,
    _matched_charge: Option<RetainedCharge>,
    /// The probe file's reader for the current chunk, or for the last pass that emits
    /// the rows whose verdict waited.
    reader: Option<SpillReader>,
    /// The probe file's row of the current probe batch's first row.
    base: u64,
    /// The rows of the probe file read so far in this pass.
    read: u64,
    /// The probe row numbers of the current probe batch, when the output keeps order.
    seq: Vec<Id>,
    /// Whether the last pass, which emits the rows whose verdict waited, is running.
    finishing: bool,
}

/// Output files of an ordered join, each ascending in the probe row number of its
/// last column.
struct Spooler {
    query: Arc<QuerySpill>,
    out: Buffer,
    seq: Vec<Id>,
    writer: Option<SpillWriter>,
    runs: Vec<Arc<SpillRun>>,
    frame_rows: usize,
    width: usize,
    bytes: u64,
}

impl Spooler {
    fn room(&self) -> usize {
        self.frame_rows.saturating_sub(self.out.table.len).max(1)
    }

    fn flush(&mut self) -> Result<()> {
        let rows = self.out.table.len;
        if rows == 0 {
            return Ok(());
        }
        if self.writer.is_none() {
            let mut writer = self
                .query
                .create(SpillKind::JoinPartition, self.width + 1)?;
            writer.set_frame_rows(self.frame_rows);
            self.writer = Some(writer);
        }
        let mut slices: Vec<&[Id]> = self.out.table.cols.iter().map(Vec::as_slice).collect();
        slices.push(&self.seq);
        self.writer
            .as_mut()
            .expect("an open spill file")
            .write(&slices, rows)?;
        for column in &mut self.out.table.cols {
            column.clear();
        }
        self.out.table.len = 0;
        self.seq.clear();
        Ok(())
    }

    /// End the current file: rows that follow may have smaller numbers.
    fn boundary(&mut self) -> Result<()> {
        self.flush()?;
        if let Some(writer) = self.writer.take() {
            let run = writer.finish()?;
            self.bytes += run.bytes();
            self.runs.push(Arc::new(run));
        }
        Ok(())
    }
}

/// A merge of output files by the probe row number in their last column.
struct SeqMerge {
    inputs: Vec<MergeInput>,
    heap: BinaryHeap<Reverse<(u64, usize)>>,
    width: usize,
    _charge: Option<RetainedCharge>,
}

struct MergeInput {
    reader: SpillReader,
    frame: Vec<Vec<Id>>,
    len: usize,
    at: usize,
}

impl MergeInput {
    /// Move to the next row, reading a frame when the current one is spent. False at
    /// the end of the file.
    fn advance(&mut self, ctx: &Ctx) -> Result<bool> {
        self.at += 1;
        while self.at >= self.len {
            ctx.check()?;
            match self.reader.next_frame()? {
                Some((frame, len)) => {
                    self.frame = frame;
                    self.len = len;
                    self.at = 0;
                }
                None => return Ok(false),
            }
        }
        Ok(true)
    }

    fn seq(&self) -> u64 {
        self.frame.last().expect("a number column")[self.at].0
    }
}

impl SeqMerge {
    fn new(ctx: &Ctx, runs: &[Arc<SpillRun>], width: usize, frame_rows: usize) -> Result<Self> {
        let charge = ctx.retained_charge(
            runs.len() as u64 * (frame_rows as u64 * (width as u64 + 1) * 8 * 2 + 1024),
        )?;
        let mut inputs = Vec::with_capacity(runs.len());
        let mut heap = BinaryHeap::with_capacity(runs.len());
        for run in runs {
            let mut input = MergeInput {
                reader: run.reader()?,
                frame: Vec::new(),
                len: 0,
                at: 0,
            };
            if input.advance_first(ctx)? {
                heap.push(Reverse((input.seq(), inputs.len())));
                inputs.push(input);
            }
        }
        Ok(Self {
            inputs,
            heap,
            width,
            _charge: charge,
        })
    }

    /// Append at most `room` rows in number order to `out`, and their numbers to `seq`
    /// when it is given. False once every input is spent.
    fn pull(
        &mut self,
        ctx: &Ctx,
        out: &mut Table,
        mut seq: Option<&mut Vec<Id>>,
        room: usize,
    ) -> Result<bool> {
        let mut n = 0;
        while n < room {
            let Some(Reverse((number, i))) = self.heap.pop() else {
                return Ok(false);
            };
            let input = &mut self.inputs[i];
            for (to, from) in out.cols.iter_mut().zip(&input.frame[..self.width]) {
                to.push(from[input.at]);
            }
            if let Some(seq) = seq.as_deref_mut() {
                seq.push(Id(number));
            }
            out.len += 1;
            n += 1;
            if input.advance(ctx)? {
                self.heap.push(Reverse((input.seq(), i)));
            }
        }
        Ok(!self.heap.is_empty())
    }
}

impl MergeInput {
    fn advance_first(&mut self, ctx: &Ctx) -> Result<bool> {
        loop {
            ctx.check()?;
            match self.reader.next_frame()? {
                Some((frame, len)) if len > 0 => {
                    self.frame = frame;
                    self.len = len;
                    self.at = 0;
                    return Ok(true);
                }
                Some(_) => {}
                None => return Ok(false),
            }
        }
    }
}

/// What the plan reports of a spilled join.
#[derive(Default)]
pub(super) struct Stats {
    pub(super) written: u64,
    pub(super) partitions: usize,
    pub(super) levels: u64,
    pub(super) chunks: u64,
    pub(super) merge_passes: u64,
}

pub(super) struct JoinSpill {
    query: Arc<QuerySpill>,
    /// The probe column and the build column of the key.
    key: (usize, usize),
    fanout: usize,
    /// The memory the join plans for once it has spilled.
    budget: u64,
    /// The rows of a partition file's frames.
    frame_rows: usize,
    /// Build rows that leave the key unbound, which every probe row may match.
    unbound: Buffer,
    /// The build files of the first partitioning, until the probe input is partitioned.
    builds: Vec<Option<Arc<SpillRun>>>,
    /// The next probe row's number, when the output keeps the probe input's order.
    seq: u64,
    probed: bool,
    jobs: Vec<Job>,
    pass: Option<Pass>,
    spooler: Option<Spooler>,
    /// Whether a pass started reading its probe file again since the output was last
    /// spooled.
    restarted: bool,
    merge: Option<SeqMerge>,
    pub(super) stats: Stats,
}

impl JoinSpill {
    /// Partition the build input of `binary` on `key`: the rows of `build`, the batch
    /// `pending` and what `child` still gives.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        ctx: &Arc<Ctx>,
        binary: &mut Binary,
        key: (usize, usize),
        build: Buffer,
        pending: Option<Buffer>,
        child: &mut Operator,
        options: &CursorOptions,
        error: Error,
    ) -> Result<Self> {
        let query = ctx.spill.clone().ok_or(error)?;
        let budget = (build.charge.bytes().max(ctx.memory_room() / 2)).max(MIN_BUDGET);
        let width = binary.build_vars.len();
        // The estimated build bytes over the memory for one partition's table, as a
        // power of two between the bounds, fewer when their buffers would not fit.
        let row = chunk_row_bytes(width);
        let estimate = (binary.build_est.max(0.0) as u64)
            .max(build.table.len as u64 * 2 + pending.as_ref().map_or(0, |p| p.table.len as u64))
            .saturating_mul(row);
        let mut fanout = (estimate / (budget / 2).max(1))
            .max(1)
            .next_power_of_two()
            .clamp(MIN_FANOUT as u64, MAX_FANOUT as u64) as usize;
        let frame_rows = |fanout: usize| {
            // Buffers of the partition files, with the unbound probe file, may take a
            // quarter of the budget.
            ((budget / 4) / ((fanout as u64 + 1) * (width as u64 + 2) * 16))
                .clamp(MIN_FRAME_ROWS as u64, crate::spill::FRAME_ROWS as u64) as usize
        };
        while fanout > MIN_FANOUT
            && (fanout as u64 + 1) * frame_rows(fanout) as u64 * (width as u64 + 2) * 16
                > budget / 4
        {
            fanout /= 2;
        }
        let frame_rows = frame_rows(fanout);
        let mut spill = Self {
            query: query.clone(),
            key,
            fanout,
            budget,
            frame_rows,
            unbound: Buffer::new(ctx, &binary.build_vars, 0)?,
            builds: Vec::new(),
            seq: 0,
            probed: false,
            jobs: Vec::new(),
            pass: None,
            spooler: None,
            restarted: false,
            merge: None,
            stats: Stats {
                partitions: fanout,
                levels: 1,
                ..Default::default()
            },
        };
        let mut parts = Parts::new(ctx, &query, width, fanout, frame_rows)?;
        spill.route_build(ctx, &mut parts, &build.table)?;
        drop(build);
        if let Some(pending) = pending {
            spill.route_build(ctx, &mut parts, &pending.table)?;
        }
        while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
            spill.route_build(ctx, &mut parts, &batch.table)?;
        }
        let (builds, bytes) = parts.finish()?;
        spill.stats.written += bytes;
        spill.builds = builds;
        Ok(spill)
    }

    /// Write the rows of a build batch to their partitions, keeping those that leave
    /// the key unbound.
    fn route_build(&mut self, ctx: &Arc<Ctx>, parts: &mut Parts, table: &Table) -> Result<()> {
        ctx.check()?;
        let keys = &table.cols[self.key.1];
        let mut unbound = Vec::new();
        for (row, &key) in keys.iter().enumerate() {
            if key.is_undef() {
                unbound.push(row);
            } else {
                parts.push(partition(key, 0, self.fanout), &table.cols, row, None)?;
            }
        }
        if !unbound.is_empty() {
            let mut rows = Table::new(table.vars.clone());
            for (to, from) in rows.cols.iter_mut().zip(&table.cols) {
                to.extend(unbound.iter().map(|&row| from[row]));
            }
            rows.len = unbound.len();
            merge::append(ctx, &mut self.unbound, &rows, 0..unbound.len())?;
        }
        Ok(())
    }

    /// Partition the probe input, the batch `binary` holds and the rest of the input,
    /// and plan the partitions' joins.
    fn partition_probe(
        &mut self,
        binary: &mut Binary,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
    ) -> Result<()> {
        let width = binary.probe_vars.len() + usize::from(binary.ordered);
        let mut parts = Parts::new(ctx, &self.query, width, self.fanout + 1, self.frame_rows)?;
        if let Some(batch) = binary.probe.take() {
            self.route_probe(ctx, binary, &mut parts, &batch.table, binary.row)?;
        }
        let child = &mut children[binary.probe_side];
        while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
            self.route_probe(ctx, binary, &mut parts, &batch.table, 0)?;
        }
        let (mut probes, bytes) = parts.finish()?;
        self.stats.written += bytes;
        let unbound_probe = probes.pop().flatten();
        let builds = std::mem::take(&mut self.builds);
        // A probe row without candidates still passes an OPTIONAL, anti or MINUS join.
        let alone = matches!(binary.mode, Mode::Optional | Mode::Anti | Mode::Minus);
        let has_unbound = self.unbound.table.len > 0;
        for (probe, build) in probes.into_iter().zip(&builds) {
            let Some(probe) = probe else { continue };
            if build.is_none() && !has_unbound && !alone {
                continue;
            }
            self.jobs.push(Job {
                builds: build.iter().cloned().collect(),
                probe,
                level: 0,
                keyed: true,
            });
        }
        // The unbound probe rows meet every build file, before the partitions' jobs
        // free them.
        if let Some(probe) = unbound_probe {
            let all: Vec<_> = builds.into_iter().flatten().collect();
            if !all.is_empty() || has_unbound || alone {
                self.jobs.push(Job {
                    builds: all,
                    probe,
                    level: MAX_LEVEL,
                    keyed: false,
                });
            }
        }
        binary.row = 0;
        binary.offset = 0;
        binary.matched = false;
        self.probed = true;
        Ok(())
    }

    fn route_probe(
        &mut self,
        ctx: &Ctx,
        binary: &Binary,
        parts: &mut Parts,
        table: &Table,
        from: usize,
    ) -> Result<()> {
        ctx.check()?;
        let keys = &table.cols[self.key.0];
        for (row, &key) in keys.iter().enumerate().skip(from) {
            let part = if key.is_undef() {
                self.fanout
            } else {
                partition(key, 0, self.fanout)
            };
            let seq = binary.ordered.then(|| {
                self.seq += 1;
                Id(self.seq)
            });
            parts.push(part, &table.cols, row, seq)?;
        }
        Ok(())
    }

    /// The rows one chunk of build rows may hold.
    fn chunk_rows(&self, width: usize) -> u64 {
        ((self.budget / 2) / chunk_row_bytes(width)).max(MIN_FRAME_ROWS as u64)
    }

    /// Partition a job's build and probe files again with the next seed.
    fn repartition(&mut self, ctx: &Arc<Ctx>, binary: &Binary, job: Job) -> Result<()> {
        let level = job.level + 1;
        self.stats.levels = self.stats.levels.max(level + 1);
        let parent = job.build_rows();
        let build_width = binary.build_vars.len();
        let mut builds = Parts::new(ctx, &self.query, build_width, self.fanout, self.frame_rows)?;
        for run in &job.builds {
            let mut reader = run.reader()?;
            while let Some((columns, rows)) = reader.next_frame()? {
                ctx.check()?;
                for row in 0..rows {
                    let part = partition(columns[self.key.1][row], level, self.fanout);
                    builds.push(part, &columns, row, None)?;
                }
            }
        }
        let (builds, bytes) = builds.finish()?;
        self.stats.written += bytes;
        let probe_width = binary.probe_vars.len() + usize::from(binary.ordered);
        let mut probes = Parts::new(ctx, &self.query, probe_width, self.fanout, self.frame_rows)?;
        let mut reader = job.probe.reader()?;
        while let Some((columns, rows)) = reader.next_frame()? {
            ctx.check()?;
            for row in 0..rows {
                let part = partition(columns[self.key.0][row], level, self.fanout);
                probes.push(part, &columns, row, None)?;
            }
        }
        drop(reader);
        drop(job);
        let (probes, bytes) = probes.finish()?;
        self.stats.written += bytes;
        let alone = matches!(binary.mode, Mode::Optional | Mode::Anti | Mode::Minus);
        let has_unbound = self.unbound.table.len > 0;
        for (probe, build) in probes.into_iter().zip(builds) {
            let Some(probe) = probe else { continue };
            if build.is_none() && !has_unbound && !alone {
                continue;
            }
            let rows = build.as_ref().map_or(0, |b| b.rows());
            self.jobs.push(Job {
                builds: build.into_iter().collect(),
                probe,
                // A partition that did not shrink holds a heavy key, which another
                // partitioning would not divide.
                level: if rows * 10 > parent * 9 {
                    MAX_LEVEL
                } else {
                    level
                },
                keyed: true,
            });
        }
        Ok(())
    }

    /// Read the next chunk of a job's build rows into the kernel, with the unbound build
    /// rows in the first. False when the job's build rows are spent.
    fn load_chunk(&mut self, ctx: &Arc<Ctx>, binary: &mut Binary) -> Result<bool> {
        let pass = self.pass.as_mut().expect("a running pass");
        let width = binary.build_vars.len();
        let limit =
            ((self.budget / 2) / chunk_row_bytes(width)).max(MIN_FRAME_ROWS as u64) as usize;
        let first = !pass.chunks.started;
        pass.chunks.started = true;
        binary.index = None;
        binary.build = None;
        let unbound = if first { self.unbound.table.len } else { 0 };
        let left: u64 = pass.chunks.runs[pass.chunks.at.min(pass.chunks.runs.len())..]
            .iter()
            .map(|run| run.rows())
            .sum();
        let want = (left as usize).min(limit).saturating_add(unbound);
        let mut table = Buffer::new(ctx, &binary.build_vars, want.min(limit + unbound))?;
        let mut rows = 0usize;
        while rows < limit {
            if pass.chunks.reader.is_none() {
                let Some(run) = pass.chunks.runs.get(pass.chunks.at) else {
                    break;
                };
                pass.chunks.reader = Some(run.reader()?);
            }
            ctx.check()?;
            match pass
                .chunks
                .reader
                .as_mut()
                .expect("a build reader")
                .next_frame()?
            {
                Some((columns, n)) => {
                    let len = table.table.len + n;
                    ctx.check_rows(len)?;
                    if table.table.cols.iter().any(|c| c.capacity() < len) {
                        table.charge.resize(
                            super::capacity_bytes(&table.table).saturating_add(
                                (len as u64).next_power_of_two() * width as u64 * 8,
                            ),
                        )?;
                    }
                    for (to, from) in table.table.cols.iter_mut().zip(&columns) {
                        to.extend_from_slice(from);
                    }
                    table.table.len = len;
                    table.reconcile()?;
                    rows += n;
                }
                None => {
                    pass.chunks.reader = None;
                    pass.chunks.at += 1;
                }
            }
        }
        if rows == 0 && !first {
            return Ok(false);
        }
        if unbound > 0 {
            let n = self.unbound.table.len;
            merge::append(ctx, &mut table, &self.unbound.table, 0..n)?;
        }
        if pass.job.keyed && table.table.len > 1 {
            binary.index = Some(HashIndex::new(ctx, &table.table, self.key.1, self.key.0)?);
        }
        binary.build = Some(table);
        self.stats.chunks += u64::from(pass.deferred);
        Ok(true)
    }

    /// Start the next job, partitioning it again first when its build rows do not fit.
    /// False when no job is left.
    fn start_pass(&mut self, ctx: &Arc<Ctx>, binary: &mut Binary) -> Result<bool> {
        loop {
            let Some(job) = self.jobs.pop() else {
                return Ok(false);
            };
            let width = binary.build_vars.len();
            let fits = job.build_rows() + self.unbound.table.len as u64 <= self.chunk_rows(width);
            if !fits && job.keyed && job.level < MAX_LEVEL {
                self.repartition(ctx, binary, job)?;
                continue;
            }
            // The verdicts of an inner join need no bitmap.
            let deferred = !fits;
            let probe_rows = job.probe.rows();
            let needs_bits = deferred && binary.mode != Mode::Inner;
            let words = if needs_bits {
                probe_rows.div_ceil(64) as usize
            } else {
                0
            };
            let charge = ctx.retained_charge(words as u64 * 8 + 64)?;
            self.pass = Some(Pass {
                chunks: Chunks {
                    runs: job.builds.clone(),
                    at: 0,
                    reader: None,
                    started: false,
                },
                job,
                deferred,
                matched: vec![0; words],
                _matched_charge: charge,
                reader: None,
                base: 0,
                read: 0,
                seq: Vec::new(),
                finishing: false,
            });
            binary.deferred = deferred;
            if !self.load_chunk(ctx, binary)? {
                // A job always has a first chunk.
                unreachable!("a first chunk");
            }
            self.restart_probe(binary)?;
            return Ok(true);
        }
    }

    /// Read the current pass's probe file again from its start. Output that follows may
    /// have smaller probe row numbers than output before it.
    fn restart_probe(&mut self, binary: &mut Binary) -> Result<()> {
        self.restarted = true;
        let pass = self.pass.as_mut().expect("a running pass");
        pass.reader = Some(pass.job.probe.reader()?);
        pass.base = 0;
        pass.read = 0;
        binary.probe = None;
        binary.row = 0;
        binary.offset = 0;
        binary.matched = false;
        Ok(())
    }

    /// The next probe batch of the current pass into the kernel. False at the end of the
    /// probe file.
    fn next_probe(&mut self, ctx: &Arc<Ctx>, binary: &mut Binary) -> Result<bool> {
        let pass = self.pass.as_mut().expect("a running pass");
        ctx.check()?;
        let Some((mut columns, n)) = pass.reader.as_mut().expect("a probe reader").next_frame()?
        else {
            binary.probe = None;
            return Ok(false);
        };
        pass.seq = if binary.ordered {
            columns.pop().expect("a number column")
        } else {
            Vec::new()
        };
        let mut buffer = Buffer::new(ctx, &binary.probe_vars, n)?;
        for (to, from) in buffer.table.cols.iter_mut().zip(&columns) {
            to.extend_from_slice(from);
        }
        buffer.table.len = n;
        buffer.reconcile()?;
        pass.base = pass.read;
        pass.read += n as u64;
        binary.probe = Some(buffer);
        binary.row = 0;
        binary.offset = 0;
        binary.matched = false;
        Ok(true)
    }

    /// Join more of the spilled partitions: append at most `room` rows to `out`, with
    /// their probe row numbers in `seq` when the output keeps order. False once every
    /// job is done.
    #[allow(clippy::too_many_arguments)]
    fn step(
        &mut self,
        binary: &mut Binary,
        ctx: &Arc<Ctx>,
        vars: &[VarId],
        room: usize,
        out: &mut Table,
        seq: Option<&mut Vec<Id>>,
        lp: &mut Vec<u32>,
        rp: &mut Vec<u32>,
    ) -> Result<bool> {
        if self.pass.is_none() {
            // A new job's rows may come before rows already given, so its first rows
            // come in the next step.
            return self.start_pass(ctx, binary);
        }
        let probe_live = binary
            .probe
            .as_ref()
            .is_some_and(|b| binary.row < b.table.len);
        if !probe_live && !self.next_probe(ctx, binary)? {
            self.end_of_probe(ctx, binary)?;
            return Ok(true);
        }
        let pass = self.pass.as_mut().expect("a running pass");
        lp.clear();
        rp.clear();
        if pass.finishing {
            // The rows whose verdict waited: unmatched rows of OPTIONAL, anti and MINUS
            // joins, matched rows of a semi join.
            let probe = &binary.probe.as_ref().expect("a probe batch").table;
            let keep_matched = binary.mode == Mode::Semi;
            while binary.row < probe.len && lp.len() < room {
                let at = pass.base + binary.row as u64;
                let matched = pass.matched[(at / 64) as usize] >> (at % 64) & 1 == 1;
                if matched == keep_matched {
                    lp.push(binary.row as u32);
                }
                binary.row += 1;
            }
        } else {
            match binary.mode {
                Mode::Semi | Mode::Anti | Mode::Minus => binary.filter(ctx, room, lp)?,
                Mode::Optional if binary.expression.is_some() => {
                    binary.condition(ctx, vars, room, lp, rp)?
                }
                Mode::Inner | Mode::Optional => binary.pairs(ctx, room, lp, rp)?,
            }
            if pass.deferred && binary.mode != Mode::Inner {
                // Record the probe rows a build row matched, and leave their verdicts
                // to the last pass.
                let mut kept = 0;
                for i in 0..lp.len() {
                    let row = lp[i];
                    let paired = rp.get(i).is_some_and(|&r| r != NO_ROW);
                    let matched = match binary.mode {
                        Mode::Optional => paired,
                        _ => true,
                    };
                    if matched {
                        let at = pass.base + u64::from(row);
                        pass.matched[(at / 64) as usize] |= 1 << (at % 64);
                    }
                    if binary.mode == Mode::Optional && paired {
                        lp[kept] = row;
                        rp[kept] = rp[i];
                        kept += 1;
                    }
                }
                lp.truncate(kept);
                rp.truncate(kept);
            }
        }
        if lp.is_empty() {
            return Ok(true);
        }
        if rp.len() < lp.len() {
            rp.resize(lp.len(), NO_ROW);
        }
        let probe = &binary.probe.as_ref().expect("a probe batch").table;
        let empty = Table::new(binary.build_vars.clone());
        let build = binary.build.as_ref().map_or(&empty, |b| &b.table);
        gather(out, &binary.columns, probe, build, lp, rp);
        if let Some(seq) = seq {
            seq.extend(lp.iter().map(|&l| pass.seq[l as usize]));
        }
        Ok(true)
    }

    /// The current pass read its whole probe file: join the next chunk, run the last
    /// pass of the rows whose verdict waited, or end the job.
    fn end_of_probe(&mut self, ctx: &Arc<Ctx>, binary: &mut Binary) -> Result<()> {
        let pass = self.pass.as_ref().expect("a running pass");
        let (finishing, deferred) = (pass.finishing, pass.deferred);
        if !finishing && deferred && self.load_chunk(ctx, binary)? {
            return self.restart_probe(binary);
        }
        let pass = self.pass.as_mut().expect("a running pass");
        if !pass.finishing && pass.deferred && binary.mode != Mode::Inner {
            pass.finishing = true;
            binary.index = None;
            binary.build = None;
            return self.restart_probe(binary);
        }
        // The job is done, and its files close.
        self.pass = None;
        binary.index = None;
        binary.build = None;
        binary.probe = None;
        binary.deferred = false;
        Ok(())
    }

    pub(super) fn next(
        &mut self,
        binary: &mut Binary,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if !self.probed {
            self.partition_probe(binary, ctx, children, options)?;
        }
        let mut lp: Vec<u32> = Vec::new();
        let mut rp: Vec<u32> = Vec::new();
        // The output takes at most an eighth of the join's memory, beside its tables.
        let cap = cap.min(
            ((self.budget / 8) / (vars.len().max(1) as u64 * 8)).max(MIN_FRAME_ROWS as u64)
                as usize,
        );
        if binary.ordered {
            if self.merge.is_none() {
                self.spool_all(binary, ctx, vars, &mut lp, &mut rp)?;
            }
            let mut out = Buffer::new(ctx, vars, cap)?;
            let merge = self.merge.as_mut().expect("an output merge");
            merge.pull(ctx, &mut out.table, None, cap)?;
            out.reconcile()?;
            return Ok((!out.table.is_empty()).then_some(out));
        }
        let _pairs = ctx.charge(cap as u64 * 8 + 256)?;
        let mut out = Buffer::new(ctx, vars, cap)?;
        while out.table.len < cap {
            ctx.check()?;
            let room = cap - out.table.len;
            if !self.step(
                binary,
                ctx,
                vars,
                room,
                &mut out.table,
                None,
                &mut lp,
                &mut rp,
            )? {
                break;
            }
        }
        out.reconcile()?;
        Ok((!out.table.is_empty()).then_some(out))
    }

    /// Run every job into output files numbered by probe row, then merge them by number
    /// in passes of at most [`FAN_IN`] files, fewer when memory is short.
    fn spool_all(
        &mut self,
        binary: &mut Binary,
        ctx: &Arc<Ctx>,
        vars: &[VarId],
        lp: &mut Vec<u32>,
        rp: &mut Vec<u32>,
    ) -> Result<()> {
        let width = vars.len();
        // The merge holds a frame of each file it reads, in a quarter of the budget: up to
        // [`FAN_IN`] frames of at least [`MIN_SPOOL_ROWS`] rows, and fewer files at once
        // when those do not fit.
        let row = (width as u64 + 1) * 16;
        let frame_rows = ((self.budget / 4) / (FAN_IN as u64 * row))
            .clamp(MIN_SPOOL_ROWS as u64, crate::spill::FRAME_ROWS as u64)
            as usize;
        let fan_in =
            ((self.budget / 4) / (frame_rows as u64 * row + 1024)).clamp(2, FAN_IN as u64) as usize;
        self.spooler = Some(Spooler {
            query: self.query.clone(),
            out: Buffer::new(ctx, vars, frame_rows)?,
            seq: Vec::with_capacity(frame_rows),
            writer: None,
            runs: Vec::new(),
            frame_rows,
            width,
            bytes: 0,
        });
        loop {
            let spooler = self.spooler.as_mut().expect("a spooler");
            let room = spooler.room();
            let mut out = std::mem::replace(&mut spooler.out.table, Table::new(Vec::new()));
            let mut seq = std::mem::take(&mut spooler.seq);
            self.restarted = false;
            let more = self.step(binary, ctx, vars, room, &mut out, Some(&mut seq), lp, rp);
            let spooler = self.spooler.as_mut().expect("a spooler");
            spooler.out.table = out;
            spooler.seq = seq;
            spooler.out.reconcile()?;
            if !more? {
                break;
            }
            if self.restarted {
                // The step that restarts a probe file adds no row, so the rows held are
                // all from before it.
                spooler.boundary()?;
            } else if spooler.out.table.len >= spooler.frame_rows {
                spooler.flush()?;
            }
        }
        let mut spooler = self.spooler.take().expect("a spooler");
        spooler.boundary()?;
        self.stats.written += spooler.bytes;
        let mut runs = std::mem::take(&mut spooler.runs);
        drop(spooler);
        // Merge passes until the files fit one merge.
        while runs.len() > fan_in {
            self.stats.merge_passes += 1;
            let rest = runs.split_off(fan_in);
            let mut merge = SeqMerge::new(ctx, &runs, width, frame_rows)?;
            let mut spooler = Spooler {
                query: self.query.clone(),
                out: Buffer::new(ctx, vars, frame_rows)?,
                seq: Vec::with_capacity(frame_rows),
                writer: None,
                runs: Vec::new(),
                frame_rows,
                width,
                bytes: 0,
            };
            loop {
                let room = spooler.room();
                let more = merge.pull(ctx, &mut spooler.out.table, Some(&mut spooler.seq), room)?;
                spooler.out.reconcile()?;
                spooler.flush()?;
                if !more {
                    break;
                }
            }
            drop(merge);
            spooler.boundary()?;
            self.stats.written += spooler.bytes;
            runs = rest;
            runs.append(&mut spooler.runs);
        }
        self.stats.merge_passes += 1;
        self.merge = Some(SeqMerge::new(ctx, &runs, width, frame_rows)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FallbackPolicy, State};
    use super::*;
    use crate::sparql::exec;
    use crate::sparql::expr::{CmpOp, Expr};
    use crate::sparql::plan::{JoinAlgo, Kind, Node};
    use crate::store::{Store, StoreOptions};

    const KEY: VarId = 0;
    const SHARED: VarId = 1;
    const LEFT: VarId = 2;
    const RIGHT: VarId = 3;

    fn context(limit: u64, spill: Option<&Arc<crate::spill::Spill>>) -> Arc<Ctx> {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        for name in ["key", "shared", "left", "right"] {
            ctx.var(name);
        }
        ctx.mem_limit = limit;
        ctx.spill = spill.map(|s| Arc::new(s.query(None)));
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_cursor_owner();
        ctx
    }

    /// `n` rows over `vars`, the first the key: one key in a hundred unbound, and with
    /// `heavy` most keys one value. Other columns are unbound one time in ten.
    fn values(vars: Vec<VarId>, n: usize, seed: u64, heavy: bool, ordered: bool) -> Node {
        let mut state = seed;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state >> 33
        };
        let mut table = Table::new(vars.clone());
        for _ in 0..n {
            let row: Vec<Id> = (0..vars.len())
                .map(|c| {
                    let x = next();
                    match c {
                        0 if x % 100 == 0 => Id::UNDEF,
                        0 if heavy && x % 10 != 1 => Id::from_i64(7).unwrap(),
                        0 => Id::from_i64((x % 400) as i64).unwrap(),
                        _ if x % 10 == 0 => Id::UNDEF,
                        1 => Id::from_i64((x % 3) as i64).unwrap(),
                        _ => Id::from_i64((x % 1000) as i64).unwrap(),
                    }
                })
                .collect();
            table.push_row(&row);
        }
        let mut node = Node::leaf(Kind::Empty, vars.clone(), n as f64, String::new());
        // Every column has unbound values.
        node.certain.clear();
        if ordered {
            // Rows in raw order of every column, which the cursor then claims.
            let mut rows: Vec<Vec<Id>> = (0..table.len)
                .map(|i| table.cols.iter().map(|c| c[i]).collect())
                .collect();
            rows.sort();
            let mut sorted = Table::new(vars.clone());
            for row in rows {
                sorted.push_row(&row);
            }
            sorted.sorted = vars.clone();
            table = sorted;
            node.sorted = vars.clone();
        }
        node.kind = Kind::Values(table);
        node
    }

    fn binary(kind: Kind, left: Node, right: Node) -> Node {
        let mut vars = left.vars.clone();
        if !matches!(kind, Kind::HalfJoin { .. } | Kind::Minus) {
            for &var in &right.vars {
                if !vars.contains(&var) {
                    vars.push(var);
                }
            }
        }
        let mut node = Node::leaf(kind, vars, 100.0, String::new());
        node.children = vec![left, right];
        node
    }

    fn rows(table: &Table) -> Vec<Vec<Id>> {
        (0..table.len)
            .map(|i| table.cols.iter().map(|c| c[i]).collect())
            .collect()
    }

    /// Every join mode, with one and two shared variables, unbound keys on both sides,
    /// ordered and unordered probe input and a heavy key, spills under a small budget
    /// and gives the bag that eager execution gives, at batch caps 1, 7 and 512. The
    /// heavy key, whose output is large, runs at 512 only.
    #[test]
    fn spilled_join_modes_match_eager_execution() {
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
            Box::new(Expr::Var(LEFT)),
            Box::new(Expr::Var(RIGHT)),
            CmpOp::Lt,
        );
        let kinds = [
            Kind::Join {
                algo: JoinAlgo::Hash,
                keys: vec![KEY],
            },
            Kind::LeftJoin { expr: None },
            Kind::LeftJoin {
                expr: Some(condition),
            },
            Kind::HalfJoin { anti: false },
            Kind::HalfJoin { anti: true },
            Kind::Minus,
        ];
        let mut spilled = 0;
        let mut cases = 0;
        for (heavy, n) in [(false, 3000), (true, 1500)] {
            for right in [vec![KEY, RIGHT], vec![KEY, SHARED, RIGHT]] {
                for ordered in [false, true] {
                    for kind in &kinds {
                        let node = binary(
                            kind.clone(),
                            values(vec![KEY, SHARED, LEFT], n, 1, heavy, ordered),
                            values(right.clone(), n, 2, heavy, false),
                        );
                        let eager = context(u64::MAX, None);
                        let mut expected = rows(&exec::execute(&eager, &node).unwrap().0);
                        expected.sort();
                        let caps: &[usize] = if heavy { &[512] } else { &[1, 7, 512] };
                        for &cap in caps {
                            let ctx =
                                context(if heavy { 96 << 10 } else { 128 << 10 }, Some(&spill));
                            let mut op = Operator::build(
                                &ctx,
                                node.clone(),
                                FallbackPolicy::RejectMaterialization,
                            )
                            .unwrap();
                            let options = CursorOptions {
                                batch_rows: cap,
                                ..Default::default()
                            };
                            let mut result = Vec::new();
                            let case = format!(
                                "{} heavy {heavy} right {right:?} ordered {ordered} cap {cap}",
                                node.operator()
                            );
                            while let Some(batch) = op
                                .next(&ctx, &options, cap)
                                .unwrap_or_else(|e| panic!("{case}: {e}"))
                            {
                                assert!(batch.table.len <= cap);
                                result.extend(rows(&batch.table));
                            }
                            cases += 1;
                            if matches!(&op.state, State::Binary(b) if b.spill.is_some()) {
                                spilled += 1;
                            }
                            result.sort();
                            let case = format!(
                                "{} heavy {heavy} right {right:?} ordered {ordered} cap {cap}",
                                node.operator()
                            );
                            assert_eq!(result.len(), expected.len(), "{case}");
                            assert_eq!(result, expected, "{case}");
                        }
                    }
                }
            }
        }
        assert!(spilled * 10 > cases * 9, "{spilled} of {cases} spilled");
        assert_eq!(spill.reserved_bytes(), 0);
    }
}
