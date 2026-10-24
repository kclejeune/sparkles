//! Hash joins whose build input outgrows memory (X08 §6.3).
//!
//! A join of a query that may spill keeps its build input to half the room the query
//! had when the build started. When the build input passes that, or a charge for it or
//! for its hash table fails, the join picks its key column as the in-memory join does
//! and writes the build rows to [`JoinSpill::fanout`] files by a seeded hash of the key.
//! Build rows that leave the key unbound are compatible with every probe row. They stay
//! in memory while they fill at most a quarter of a chunk, and go to a file of their own
//! once they are more. The probe input is then written to files by the same
//! hash, and probe rows that leave the key unbound go to a file of their own.
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
//! Unbound build rows on disk are not part of any partition, so they never count toward
//! the decision to partition again and are never copied into the files of a deeper
//! level. An inner join meets them in one job of their own, which reads every probe file
//! against them in chunks. The other joins decide a probe row from all its candidates at
//! once, so each of their jobs reads the unbound build file as further chunks after its
//! partition's build rows.
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
/// The total finished output files retained across all merge levels.
const MAX_OUTPUT_RUNS: usize = 2 * FAN_IN;
/// The fewest rows of an output file's frames.
const MIN_SPOOL_ROWS: usize = 16;

/// The partition of `key` at partitioning level `level`, among `fanout` partitions.
/// Each level mixes the key with its own seed, so that the rows of one partition spread
/// evenly over the partitions of the next level.
fn partition(key: Id, level: u64, fanout: usize) -> usize {
    (mix(key.0 ^ (level + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)) >> 58) as usize % fanout
}

/// The SplitMix64 finalizer, whose output bits each depend on every input bit. Spilled
/// joins, groupings and DISTINCT pick a row's partition from its high bits with a seed
/// per level. A multiplicative hash such as FxHash alone kept most of a partition's rows
/// together at the next level, since its high bits change little with the seed.
pub(super) fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A hash of a row's key for the partitions of a spilled grouping or DISTINCT, seeded
/// by the partitioning depth. Each column is mixed into a state that starts from the
/// seed, so two different keys share a partition at one depth and at the next only by
/// chance. A seeded mix of the row's FxHash kept rows whose FxHash values collide in one
/// partition at every depth, since the seed cannot separate equal hashes.
pub(super) fn row_hash(depth: u64, key: impl IntoIterator<Item = Id>) -> u64 {
    let mut state = mix(depth.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5bd1_e995);
    for id in key {
        state = mix(state ^ id.0);
    }
    state
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
    /// The probe files, read one after the other. Only the job of an inner join's
    /// unbound build rows has more than one.
    probes: Vec<Arc<SpillRun>>,
    /// Whether the unbound build rows on disk follow `builds` as further chunks.
    unbound: bool,
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

    fn probe_rows(&self) -> u64 {
        self.probes.iter().map(|run| run.rows()).sum()
    }
}

/// What reading the next probe batch of a pass found.
enum Probe {
    Batch,
    /// The pass moved on to its next probe file. Rows that follow may have smaller probe
    /// row numbers than rows before.
    NextFile,
    End,
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
    /// The probe file `reader` reads.
    probe_at: usize,
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
struct SpoolRun {
    run: Arc<SpillRun>,
    level: u64,
}

struct Spooler {
    query: Arc<QuerySpill>,
    out: Buffer,
    seq: Vec<Id>,
    writer: Option<SpillWriter>,
    runs: Vec<SpoolRun>,
    frame_rows: usize,
    fan_in: usize,
    width: usize,
    bytes: u64,
    merge_passes: u64,
    _charge: Option<RetainedCharge>,
}

impl Spooler {
    /// Sequence IDs, the encoded payload and the writer's value, varint and
    /// compressed-column scratch coexist. Allow allocation growth and headers.
    fn writer_bytes(width: usize, frame_rows: usize) -> u64 {
        frame_rows as u64 * ((width as u64 + 1) * 24 + 64) + 1024
    }

    fn new(
        ctx: &Arc<Ctx>,
        query: &Arc<QuerySpill>,
        vars: &[VarId],
        frame_rows: usize,
        fan_in: usize,
    ) -> Result<Self> {
        // The output buffer charges its columns; this covers sequence numbers, the
        // encoded writer frame and the bounded retained-run directory.
        let charge = ctx.retained_charge(
            Self::writer_bytes(vars.len(), frame_rows)
                + (MAX_OUTPUT_RUNS * std::mem::size_of::<SpoolRun>()) as u64,
        )?;
        Ok(Self {
            query: query.clone(),
            out: Buffer::new(ctx, vars, frame_rows)?,
            seq: Vec::with_capacity(frame_rows),
            writer: None,
            runs: Vec::with_capacity(MAX_OUTPUT_RUNS),
            frame_rows,
            fan_in,
            width: vars.len(),
            bytes: 0,
            merge_passes: 0,
            _charge: charge,
        })
    }
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
    fn boundary(&mut self, ctx: &Arc<Ctx>) -> Result<()> {
        self.flush()?;
        if let Some(writer) = self.writer.take() {
            let run = writer.finish()?;
            self.bytes += run.bytes();
            self.runs.push(SpoolRun {
                run: Arc::new(run),
                level: 0,
            });
        }
        loop {
            // Merge like-sized generations, rather than rewriting a growing run for
            // each new small run. The aggregate cap also bounds handles when many
            // generations coexist. In that case combine the smallest files first.
            let same_level = self.runs.iter().find_map(|run| {
                let indices: Vec<_> = self
                    .runs
                    .iter()
                    .enumerate()
                    .filter(|(_, other)| other.level == run.level)
                    .take(self.fan_in)
                    .map(|(i, _)| i)
                    .collect();
                (indices.len() == self.fan_in).then_some(indices)
            });
            let indices = if let Some(indices) = same_level {
                indices
            } else if self.runs.len() >= MAX_OUTPUT_RUNS {
                self.smallest(self.fan_in)
            } else {
                break;
            };
            self.combine(ctx, indices)?;
        }
        Ok(())
    }

    fn smallest(&self, count: usize) -> Vec<usize> {
        let mut indices: Vec<_> = (0..self.runs.len()).collect();
        indices.sort_unstable_by_key(|&i| self.runs[i].run.bytes());
        indices.truncate(count);
        indices
    }

    /// Merge a bounded set while retaining its inputs until the output is admitted
    /// and finished. SpillWriter charges temporary disk overlap; SeqMerge charges
    /// reader frames, and this method charges its additional output and writer.
    fn combine(&mut self, ctx: &Arc<Ctx>, mut indices: Vec<usize>) -> Result<()> {
        indices.sort_unstable();
        let mut selected = Vec::with_capacity(indices.len());
        let mut level = 0;
        for i in indices.into_iter().rev() {
            let run = self.runs.remove(i);
            level = level.max(run.level + 1);
            selected.push(run.run);
        }
        let mut merge = SeqMerge::new(ctx, &selected, self.width, self.frame_rows)?;
        let _charge = ctx.retained_charge(Self::writer_bytes(self.width, self.frame_rows))?;
        let mut out = Buffer::new(ctx, &self.out.table.vars, self.frame_rows)?;
        let mut seq = Vec::with_capacity(self.frame_rows);
        debug_assert!(self.writer.is_none());
        // Selected readers share their runs' handles; only the writer adds one.
        let output_handles = self.runs.len() + selected.len() + 1;
        debug_assert!(output_handles <= MAX_OUTPUT_RUNS + 1);
        let mut writer = self
            .query
            .create(SpillKind::JoinPartition, self.width + 1)?;
        writer.set_frame_rows(self.frame_rows);
        loop {
            let more = merge.pull(ctx, &mut out.table, Some(&mut seq), self.frame_rows)?;
            out.reconcile()?;
            if out.table.len > 0 {
                let mut slices: Vec<&[Id]> = out.table.cols.iter().map(Vec::as_slice).collect();
                slices.push(&seq);
                writer.write(&slices, out.table.len)?;
            }
            for col in &mut out.table.cols {
                col.clear();
            }
            out.table.len = 0;
            seq.clear();
            if !more {
                break;
            }
        }
        let run = writer.finish()?;
        self.bytes += run.bytes();
        self.merge_passes += 1;
        self.runs.push(SpoolRun {
            run: Arc::new(run),
            level,
        });
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
    /// Build rows that leave the key unbound, which every probe row may match, while
    /// they number at most [`Self::unbound_rows`].
    unbound: Buffer,
    /// The most unbound build rows held in memory: a quarter of a chunk, so that they
    /// leave most of every chunk to the partition's own rows.
    unbound_rows: u64,
    /// The unbound build rows once they outgrew memory, while the build input is read,
    /// buffered into whole frames as the partition files are.
    unbound_writer: Option<Parts>,
    /// The unbound build rows on disk.
    unbound_run: Option<Arc<SpillRun>>,
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
        // Half of the room the join has once the build rows it holds are on disk.
        let held = build.charge.bytes() + pending.as_ref().map_or(0, |p| p.charge.bytes());
        let budget = ((held + ctx.memory_room()) / 2).max(MIN_BUDGET);
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
            unbound_rows: ((budget / 2) / chunk_row_bytes(width) / 4).max(1),
            unbound_writer: None,
            unbound_run: None,
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
        if let Some(writer) = spill.unbound_writer.take() {
            let (mut runs, bytes) = writer.finish()?;
            spill.stats.written += bytes;
            spill.unbound_run = runs.pop().flatten();
        }
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
        if unbound.is_empty() {
            return Ok(());
        }
        let held = self.unbound.table.len as u64 + unbound.len() as u64;
        if self.unbound_writer.is_none() && held > self.unbound_rows {
            // The unbound build rows outgrow their share of memory: they continue on
            // disk, and the rows held so far go first.
            let mut writer = Parts::new(ctx, &self.query, table.width(), 1, self.frame_rows)?;
            let held = std::mem::replace(&mut self.unbound, Buffer::new(ctx, &table.vars, 0)?);
            for row in 0..held.table.len {
                writer.push(0, &held.table.cols, row, None)?;
            }
            self.unbound_writer = Some(writer);
        }
        match &mut self.unbound_writer {
            Some(writer) => {
                for &row in &unbound {
                    writer.push(0, &table.cols, row, None)?;
                }
            }
            None => {
                let mut rows = Table::new(table.vars.clone());
                for (to, from) in rows.cols.iter_mut().zip(&table.cols) {
                    to.extend(unbound.iter().map(|&row| from[row]));
                }
                rows.len = unbound.len();
                merge::append(ctx, &mut self.unbound, &rows, 0..rows.len)?;
            }
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
        // An inner join meets the unbound build rows on disk in a job of their own.
        let separate = self.unbound_run.is_some() && binary.mode == Mode::Inner;
        let in_jobs = self.unbound_run.is_some() && !separate;
        let has_unbound = self.unbound.table.len > 0 || in_jobs;
        let mut every_probe = Vec::new();
        for (probe, build) in probes.into_iter().zip(&builds) {
            let Some(probe) = probe else { continue };
            if separate {
                every_probe.push(probe.clone());
            }
            if build.is_none() && !has_unbound && !alone {
                continue;
            }
            self.jobs.push(Job {
                builds: build.iter().cloned().collect(),
                probes: vec![probe],
                unbound: in_jobs,
                level: 0,
                keyed: true,
            });
        }
        // The unbound probe rows meet every build file, before the partitions' jobs
        // free them.
        if let Some(probe) = unbound_probe {
            if separate {
                every_probe.push(probe.clone());
            }
            let all: Vec<_> = builds.into_iter().flatten().collect();
            if !all.is_empty() || has_unbound || alone {
                self.jobs.push(Job {
                    builds: all,
                    probes: vec![probe],
                    unbound: in_jobs,
                    level: MAX_LEVEL,
                    keyed: false,
                });
            }
        }
        if separate && !every_probe.is_empty() {
            self.jobs.push(Job {
                builds: self.unbound_run.iter().cloned().collect(),
                probes: every_probe,
                unbound: false,
                level: MAX_LEVEL,
                keyed: false,
            });
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

    /// The rows one chunk of build rows may hold: half of the join's budget, or a quarter
    /// of the room left with no chunk held when that is more.
    fn chunk_rows(&self, ctx: &Ctx, width: usize) -> u64 {
        let bytes = (self.budget / 2).max(ctx.memory_room() / 4);
        (bytes / chunk_row_bytes(width)).max(MIN_FRAME_ROWS as u64)
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
        for run in &job.probes {
            let mut reader = run.reader()?;
            while let Some((columns, rows)) = reader.next_frame()? {
                ctx.check()?;
                for row in 0..rows {
                    let part = partition(columns[self.key.0][row], level, self.fanout);
                    probes.push(part, &columns, row, None)?;
                }
            }
        }
        let unbound = job.unbound;
        drop(job);
        let (probes, bytes) = probes.finish()?;
        self.stats.written += bytes;
        let alone = matches!(binary.mode, Mode::Optional | Mode::Anti | Mode::Minus);
        let has_unbound = self.unbound.table.len > 0 || unbound;
        for (probe, build) in probes.into_iter().zip(builds) {
            let Some(probe) = probe else { continue };
            if build.is_none() && !has_unbound && !alone {
                continue;
            }
            let rows = build.as_ref().map_or(0, |b| b.rows());
            self.jobs.push(Job {
                builds: build.into_iter().collect(),
                probes: vec![probe],
                unbound,
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
        binary.index = None;
        binary.build = None;
        let width = binary.build_vars.len();
        let limit = self.chunk_rows(ctx, width) as usize;
        let pass = self.pass.as_mut().expect("a running pass");
        let first = !pass.chunks.started;
        pass.chunks.started = true;
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
            binary.index = None;
            binary.build = None;
            let chunk = self.chunk_rows(ctx, width);
            // The rows of the partition and the unbound rows in memory: only these may
            // shrink by partitioning again.
            let partitioned = job.build_rows() + self.unbound.table.len as u64;
            let on_disk = match &self.unbound_run {
                Some(run) if job.unbound => run.rows(),
                _ => 0,
            };
            let fits = partitioned + on_disk <= chunk;
            if partitioned > chunk && job.keyed && job.level < MAX_LEVEL {
                self.repartition(ctx, binary, job)?;
                continue;
            }
            // The verdicts of an inner join need no bitmap.
            let deferred = !fits;
            let probe_rows = job.probe_rows();
            let needs_bits = deferred && binary.mode != Mode::Inner;
            let words = if needs_bits {
                probe_rows.div_ceil(64) as usize
            } else {
                0
            };
            let charge = ctx.retained_charge(words as u64 * 8 + 64)?;
            self.pass = Some(Pass {
                chunks: Chunks {
                    runs: job
                        .builds
                        .iter()
                        .chain(self.unbound_run.iter().filter(|_| job.unbound))
                        .cloned()
                        .collect(),
                    at: 0,
                    reader: None,
                    started: false,
                },
                job,
                deferred,
                matched: vec![0; words],
                _matched_charge: charge,
                reader: None,
                probe_at: 0,
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
        pass.reader = Some(pass.job.probes[0].reader()?);
        pass.probe_at = 0;
        pass.base = 0;
        pass.read = 0;
        binary.probe = None;
        binary.row = 0;
        binary.offset = 0;
        binary.matched = false;
        Ok(())
    }

    /// The next probe batch of the current pass into the kernel, or the start of its next
    /// probe file.
    fn next_probe(&mut self, ctx: &Arc<Ctx>, binary: &mut Binary) -> Result<Probe> {
        let pass = self.pass.as_mut().expect("a running pass");
        ctx.check()?;
        let Some((mut columns, n)) = pass.reader.as_mut().expect("a probe reader").next_frame()?
        else {
            binary.probe = None;
            if pass.probe_at + 1 < pass.job.probes.len() {
                pass.probe_at += 1;
                pass.reader = Some(pass.job.probes[pass.probe_at].reader()?);
                self.restarted = true;
                return Ok(Probe::NextFile);
            }
            return Ok(Probe::End);
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
        Ok(Probe::Batch)
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
        if !probe_live {
            match self.next_probe(ctx, binary)? {
                Probe::Batch => {}
                // The new file's rows come in the next step, after the spooler saw the
                // boundary.
                Probe::NextFile => return Ok(true),
                Probe::End => {
                    self.end_of_probe(ctx, binary)?;
                    return Ok(true);
                }
            }
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
        self.spooler = Some(Spooler::new(ctx, &self.query, vars, frame_rows, fan_in)?);
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
                spooler.boundary(ctx)?;
            } else if spooler.out.table.len >= spooler.frame_rows {
                spooler.flush()?;
            }
        }
        let mut spooler = self.spooler.take().expect("a spooler");
        spooler.boundary(ctx)?;
        // Finish with bounded fan-in, taking the smallest files first rather than
        // repeatedly merging a large prefix with each new file.
        while spooler.runs.len() > fan_in {
            let indices = spooler.smallest(fan_in);
            spooler.combine(ctx, indices)?;
        }
        self.stats.written += spooler.bytes;
        self.stats.merge_passes += spooler.merge_passes;
        let runs: Vec<_> = std::mem::take(&mut spooler.runs)
            .into_iter()
            .map(|run| run.run)
            .collect();
        drop(spooler);
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

    /// The keys of one partition spread evenly over the partitions of the next level,
    /// both for dictionary ids that differ in their low bits and for inline integers.
    #[test]
    fn partitioning_again_divides_a_partition() {
        let dictionary = (0..200_000u64).map(|i| Id(i * 3 + 17));
        let inline = (0..200_000i64).map(|i| Id::from_i64(i).unwrap());
        for keys in [dictionary.collect::<Vec<_>>(), inline.collect()] {
            for level in 0..MAX_LEVEL {
                let first: Vec<Id> = keys
                    .iter()
                    .copied()
                    .filter(|&key| partition(key, level, 8) == 0)
                    .collect();
                let mut counts = [0usize; 8];
                for &key in &first {
                    counts[partition(key, level + 1, 8)] += 1;
                }
                let even = first.len() / 8;
                assert!(
                    counts.iter().all(|&c| c > even * 3 / 4 && c < even * 5 / 4),
                    "level {level}: {counts:?}"
                );
            }
        }
    }

    /// `n` different two-column rows whose FxHash values are all equal: FxHash adds
    /// each word to its state and multiplies by a constant, so rows `(a, c - a K)`
    /// collide. The values of `a` are scattered, so the rows compress no better than
    /// other rows.
    fn fx_colliding_rows(n: u64) -> Vec<[Id; 2]> {
        use std::hash::{Hash, Hasher};
        const K: u64 = 0xf135_7aea_2e62_a9c5;
        let c = 0x1234_5678_9abc_def0u64;
        let rows: Vec<[Id; 2]> = (1..=n)
            .map(|i| {
                let a = mix(i);
                [Id(a), Id(c.wrapping_sub(a.wrapping_mul(K)))]
            })
            .collect();
        let fx = |row: &[Id; 2]| {
            let mut hasher = rustc_hash::FxHasher::default();
            row[0].hash(&mut hasher);
            row[1].hash(&mut hasher);
            hasher.finish()
        };
        assert!(
            rows.iter().all(|row| fx(row) == fx(&rows[0])),
            "rows must collide"
        );
        assert!(
            rows.iter()
                .all(|row| !row[0].is_undef() && !row[1].is_undef())
        );
        let mut firsts: Vec<Id> = rows.iter().map(|row| row[0]).collect();
        firsts.sort_unstable();
        firsts.dedup();
        assert_eq!(firsts.len() as u64, n, "rows must differ");
        rows
    }

    /// Rows whose FxHash values collide spread over the partitions of a spilled grouping
    /// or DISTINCT at every depth, independently of the depth before.
    #[test]
    fn colliding_rows_spread_over_the_partitions_at_every_depth() {
        let rows = fx_colliding_rows(64_000);
        for depth in 0..4u64 {
            let first: Vec<_> = rows
                .iter()
                .filter(|row| row_hash(depth, row.iter().copied()) >> 60 == 0)
                .collect();
            let mut counts = [0usize; 16];
            for row in &first {
                counts[(row_hash(depth + 1, row.iter().copied()) >> 60) as usize] += 1;
            }
            let even = first.len() / 16;
            assert!(
                counts.iter().all(|&c| c > even * 3 / 4 && c < even * 5 / 4),
                "depth {depth}: {counts:?}"
            );
        }
    }

    /// A spilled DISTINCT over rows whose FxHash values collide writes about what it
    /// writes for other rows. Before the fix every pass passed all the rows it did not
    /// keep to one partition, so the passes and the bytes grew with the square of the
    /// input.
    #[test]
    fn spilled_distinct_of_colliding_rows_is_not_amplified() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 34,
            query_bytes: 1 << 34,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        const N: u64 = 120_000;
        let colliding = fx_colliding_rows(N);
        // The same number of rows with ordinary values.
        let ordinary: Vec<[Id; 2]> = (1..=N).map(|i| [Id(mix(i)), Id(mix(i + N))]).collect();
        let mut measured = Vec::new();
        for rows in [&ordinary, &colliding] {
            let mut table = Table::new(vec![KEY, LEFT]);
            for row in rows.iter().chain(rows.iter().step_by(7)) {
                table.push_row(row);
            }
            let mut input = Node::leaf(
                Kind::Empty,
                vec![KEY, LEFT],
                table.len as f64,
                String::new(),
            );
            input.certain.clear();
            input.kind = Kind::Values(table);
            let mut node = Node::leaf(Kind::Distinct, vec![KEY, LEFT], N as f64, String::new());
            node.certain.clear();
            node.children = vec![input];
            let ctx = context(1 << 20, Some(&spill));
            let mut op =
                Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
            let options = CursorOptions::default();
            let mut out = 0;
            while let Some(batch) = op.next(&ctx, &options, 4096).unwrap() {
                out += batch.table.len as u64;
            }
            assert_eq!(out, N);
            let mut plan = op.describe(&ctx);
            op.update_plan(&mut plan);
            let counters = plan.operator.counters.clone().unwrap_or_default();
            let counter = |name: &str| counters.get(name).and_then(serde_json::Value::as_u64);
            let bytes = counter("spilledBytes").unwrap_or(0);
            let passes = counter("spillPasses").unwrap_or(0);
            assert!(bytes > 0, "{counters:?}");
            eprintln!("distinct: {bytes} bytes spilled in {passes} passes");
            measured.push((bytes, passes));
            drop(op);
            drop(ctx);
            assert_eq!(spill.reserved_bytes(), 0);
        }
        let [
            (ordinary_bytes, ordinary_passes),
            (colliding_bytes, colliding_passes),
        ] = measured[..]
        else {
            unreachable!()
        };
        assert!(
            colliding_bytes * 2 <= ordinary_bytes * 3,
            "colliding rows wrote {colliding_bytes} bytes, ordinary rows {ordinary_bytes}"
        );
        // Colliding rows used to need fewer passes than ordinary rows, each rewriting all
        // the rows it did not keep, so only the bytes show the amplification.
        let _ = (ordinary_passes, colliding_passes);
    }

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

    /// A first build batch that does not fit has no rows in front of it. Its own
    /// shared columns choose the partitioning key, so the join spills instead of
    /// failing with the memory error that started the spill.
    #[test]
    fn a_first_build_batch_that_does_not_fit_spills() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 34,
            query_bytes: 1 << 34,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let int = |i: i64| Id::from_i64(i).unwrap();
        let table = |vars: Vec<VarId>, rows: Vec<Vec<Id>>| {
            let mut table = Table::new(vars.clone());
            for row in &rows {
                table.push_row(row);
            }
            let mut node = Node::leaf(Kind::Empty, vars, rows.len() as f64, String::new());
            node.certain.clear();
            node.kind = Kind::Values(table);
            node
        };
        let left = table(
            vec![KEY, LEFT],
            (0..10).map(|i| vec![int(i), int(i)]).collect(),
        );
        let right = table(
            vec![KEY, SHARED, RIGHT],
            (0..8192)
                .map(|i| vec![int(i % 10), int(i), int(i * 3)])
                .collect(),
        );
        let node = binary(Kind::LeftJoin { expr: None }, left, right);
        let mut expected = rows(&exec::execute(&context(u64::MAX, None), &node).unwrap().0);
        expected.sort();
        let ctx = context(160 << 10, Some(&spill));
        let before = spill.written_bytes();
        let mut op = Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
        let options = CursorOptions {
            batch_rows: 4096,
            ..Default::default()
        };
        let mut out = Vec::new();
        while let Some(batch) = op.next(&ctx, &options, 4096).unwrap() {
            out.extend(rows(&batch.table));
        }
        out.sort();
        assert_eq!(out, expected);
        assert!(spill.written_bytes() > before, "the join did not spill");
    }

    #[test]
    fn keyless_builds_above_the_proactive_threshold_still_fit() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        for unbound_shared in [false, true] {
            let mut left = values(
                if unbound_shared {
                    vec![KEY, LEFT]
                } else {
                    vec![LEFT]
                },
                1,
                1,
                false,
                false,
            );
            left.est = 8000.0;
            let mut right = values(vec![KEY, SHARED, RIGHT], 5000, 2, false, false);
            if unbound_shared && let Kind::Values(table) = &mut right.kind {
                table.cols[0].fill(Id::UNDEF);
            }
            let node = binary(
                Kind::Join {
                    algo: JoinAlgo::Hash,
                    keys: if unbound_shared { vec![KEY] } else { vec![] },
                },
                left,
                right,
            );
            let eager = context(u64::MAX, None);
            let mut expected = rows(&exec::execute(&eager, &node).unwrap().0);
            expected.sort();
            for place in [None, Some(&spill)] {
                let ctx = context(352 << 10, place);
                let mut op =
                    Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization)
                        .unwrap();
                let options = CursorOptions {
                    batch_rows: 128,
                    ..Default::default()
                };
                let mut actual = Vec::new();
                while let Some(batch) = op.next(&ctx, &options, 128).unwrap_or_else(|e| {
                    panic!("unbound={unbound_shared}, spill={}: {e:?}", place.is_some())
                }) {
                    actual.extend(rows(&batch.table));
                }
                actual.sort();
                assert_eq!(actual, expected, "shared but unbound: {unbound_shared}");
                assert!(ctx.mem_peak() <= 352 << 10);
            }
        }
        assert_eq!(spill.reserved_bytes(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn ordered_output_spooling_bounds_open_handles() {
        const CHILD: &str = "SPARKLES_ORDERED_SPOOL_FD_CHILD";
        if std::env::var_os(CHILD).is_none() {
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args([
                "--exact",
                "sparql::cursor::joinspill::tests::ordered_output_spooling_bounds_open_handles",
                "--nocapture",
            ]);
            command.env(CHILD, "1");
            // SAFETY: only async-signal-safe resource-limit syscalls run before exec.
            unsafe {
                command.pre_exec(|| {
                    let mut limit = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    limit.rlim_cur = limit.rlim_cur.min(384);
                    if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let output = command.output().unwrap();
            assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let ctx = context(16 << 20, Some(&spill));
        // Keep the handles of a full 64-way build/probe partitioning alive.
        let mut partitions = Vec::new();
        for i in 0..2 * FAN_IN {
            let mut writer = ctx
                .spill
                .as_ref()
                .unwrap()
                .create(SpillKind::JoinPartition, 1)
                .unwrap();
            let id = Id::from_i64(i as i64).unwrap();
            writer.write(&[&[id]], 1).unwrap();
            partitions.push(Arc::new(writer.finish().unwrap()));
        }
        for distinct_levels in [false, true] {
            let mut spooler =
                Spooler::new(&ctx, ctx.spill.as_ref().unwrap(), &[LEFT], 16, FAN_IN).unwrap();
            for i in (0..512).rev() {
                if distinct_levels {
                    for (level, run) in spooler.runs.iter_mut().enumerate() {
                        run.level = level as u64 + 1;
                    }
                }
                let id = Id::from_i64(i).unwrap();
                spooler.out.table.cols[0].push(id);
                spooler.out.table.len = 1;
                spooler.seq.push(id);
                spooler.boundary(&ctx).unwrap();
                assert!(spooler.runs.len() < MAX_OUTPUT_RUNS);
            }
            assert!(spooler.merge_passes > 0);
            let runs: Vec<_> = spooler.runs.iter().map(|run| run.run.clone()).collect();
            let mut merge = SeqMerge::new(&ctx, &runs, 1, 16).unwrap();
            let mut out = Table::new(vec![LEFT]);
            while merge.pull(&ctx, &mut out, None, 512).unwrap() {}
            let expected: Vec<_> = (0..512).map(|i| Id::from_i64(i).unwrap()).collect();
            assert_eq!(out.cols[0], expected);
            drop(merge);
            drop(runs);
            drop(spooler);
        }
        drop(partitions);
        drop(ctx);
        assert_eq!(spill.reserved_bytes(), 0);
    }

    /// An order-independent digest of a bag of rows and its size, so that large
    /// results compare without holding them as row vectors.
    fn bag_digest<'a>(rows: impl Iterator<Item = &'a [Id]>) -> (u64, u64, u64) {
        let (mut count, mut sum, mut xor) = (0u64, 0u64, 0u64);
        for row in rows {
            let h = row_hash(99, row.iter().copied());
            count += 1;
            sum = sum.wrapping_add(h);
            xor ^= mix(h);
        }
        (count, sum, xor)
    }

    fn table_digest(table: &Table) -> (u64, u64, u64) {
        let rows: Vec<Vec<Id>> = rows(table);
        bag_digest(rows.iter().map(Vec::as_slice))
    }

    /// Build rows that leave the key unbound and outgrow a chunk go to a file of their
    /// own. They neither make every partition partition again to the last level nor
    /// stay in memory, and every join mode still gives eager execution's bag. Ordered
    /// probe input keeps its order, including through the inner join's job of the
    /// unbound build rows, which reads several probe files. Before the fix the join
    /// failed its memory budget, and with the unbound rows in memory but past a chunk,
    /// every job partitioned again to the last level.
    #[test]
    fn unbound_build_rows_do_not_partition_every_job_again() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 34,
            query_bytes: 1 << 34,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let int = |i: i64| Id::from_i64(i).unwrap();
        let table = |vars: Vec<VarId>, rows: Vec<Vec<Id>>, est: f64, order: Option<VarId>| {
            let mut table = Table::new(vars.clone());
            for row in &rows {
                table.push_row(row);
            }
            let mut node = Node::leaf(Kind::Empty, vars.clone(), est, String::new());
            node.certain.clear();
            if let Some(var) = order {
                table.sorted = vec![var];
                node.sorted = vec![var];
                node.certain = vec![var];
            }
            node.kind = Kind::Values(table);
            node
        };
        let kinds = [
            Kind::Join {
                algo: JoinAlgo::Hash,
                keys: vec![KEY],
            },
            Kind::LeftJoin { expr: None },
            Kind::HalfJoin { anti: false },
            Kind::HalfJoin { anti: true },
            Kind::Minus,
        ];
        // Under the larger budget the unbound build rows fit in memory but not in a chunk.
        // Under the smaller one they outgrow memory. One build row in ten unbound fills a
        // chunk without filling an eighth of the budget.
        for (build_rows, limit, every) in [
            (20_000, 768u64 << 10, 2),
            (20_000, 768 << 10, 10),
            (6000, 192 << 10, 2),
        ] {
            for ordered in [false, true] {
                for kind in &kinds {
                    // Probe rows, a few with the key unbound, in the order of their
                    // own column when `ordered`.
                    let left = table(
                        vec![KEY, LEFT],
                        (0..160)
                            .map(|i| {
                                let key = if i % 37 == 0 {
                                    Id::UNDEF
                                } else {
                                    int(i * 5 % 900)
                                };
                                vec![key, int(i)]
                            })
                            .collect(),
                        1e6,
                        ordered.then_some(LEFT),
                    );
                    // One build row in `every` leaves the key unbound.
                    let right = table(
                        vec![KEY, RIGHT],
                        (0..build_rows)
                            .map(|i| {
                                let key = if i % every == 0 {
                                    Id::UNDEF
                                } else {
                                    int(i % 900)
                                };
                                vec![key, int(i % 50)]
                            })
                            .collect(),
                        build_rows as f64,
                        None,
                    );
                    let node = binary(kind.clone(), left, right);
                    let expected =
                        table_digest(&exec::execute(&context(u64::MAX, None), &node).unwrap().0);
                    let ctx = context(limit, Some(&spill));
                    let mut op =
                        Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization)
                            .unwrap();
                    let case = format!(
                        "{} ordered {ordered} build {build_rows} unbound 1/{every}",
                        node.operator()
                    );
                    let probe_order = op.order.first() == Some(&LEFT);
                    assert_eq!(probe_order, ordered, "{case}: order kept");
                    let options = CursorOptions {
                        batch_rows: 512,
                        ..Default::default()
                    };
                    let left_column = node.vars.iter().position(|&v| v == LEFT).unwrap();
                    let (mut count, mut sum, mut xor) = (0u64, 0u64, 0u64);
                    let mut last: Option<Id> = None;
                    while let Some(batch) = op
                        .next(&ctx, &options, 512)
                        .unwrap_or_else(|e| panic!("{case}: {e}"))
                    {
                        let rows = rows(&batch.table);
                        if ordered {
                            for row in &rows {
                                let value = row[left_column];
                                assert!(last.is_none_or(|l| l <= value), "{case}: out of order");
                                last = Some(value);
                            }
                        }
                        let (c, s, x) = bag_digest(rows.iter().map(Vec::as_slice));
                        count += c;
                        sum = sum.wrapping_add(s);
                        xor ^= x;
                    }
                    assert_eq!((count, sum, xor), expected, "{case}");
                    let mut plan = op.describe(&ctx);
                    op.update_plan(&mut plan);
                    let counters = plan.operator.counters.clone().unwrap_or_default();
                    let counter =
                        |name: &str| counters.get(name).and_then(serde_json::Value::as_u64);
                    eprintln!("{case}: {counters:?}");
                    assert!(
                        counter("spilledBytes").unwrap_or(0) > 0,
                        "{case}: {counters:?}"
                    );
                    assert_eq!(counter("spillLevels"), Some(1), "{case}: {counters:?}");
                    if ordered {
                        assert!(
                            counter("spillMergePasses").unwrap_or(0) > 0,
                            "{case}: the ordered output was not spooled: {counters:?}"
                        );
                    }
                    assert!(ctx.mem_peak() <= limit, "{case}");
                    drop(op);
                    drop(ctx);
                    assert_eq!(spill.reserved_bytes(), 0, "{case}");
                }
            }
        }
    }

    /// Unbound build rows on disk fill whole frames whatever the input's batch size, so
    /// one-row batches write about what large batches write. Writing each batch as a
    /// frame of its own made the file many times larger at batch size 1.
    #[test]
    fn unbound_build_rows_fill_whole_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 34,
            query_bytes: 1 << 34,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let int = |i: i64| Id::from_i64(i).unwrap();
        let values = |vars: Vec<VarId>, rows: Vec<Vec<Id>>, est: f64| {
            let mut table = Table::new(vars.clone());
            for row in &rows {
                table.push_row(row);
            }
            let mut node = Node::leaf(Kind::Empty, vars, est, String::new());
            node.certain.clear();
            node.kind = Kind::Values(table);
            node
        };
        let node = binary(
            Kind::Join {
                algo: JoinAlgo::Hash,
                keys: vec![KEY],
            },
            values(
                vec![KEY, LEFT],
                (0..64).map(|i| vec![int(i * 7 % 900), int(i)]).collect(),
                1e6,
            ),
            values(
                vec![KEY, RIGHT],
                (0..40_000)
                    .map(|i| {
                        let key = if i % 2 == 0 { Id::UNDEF } else { int(i % 900) };
                        vec![key, int(i % 50)]
                    })
                    .collect(),
                40_000.0,
            ),
        );
        let mut written = Vec::new();
        for batch_rows in [1, 512] {
            let ctx = context(192 << 10, Some(&spill));
            let mut op =
                Operator::build(&ctx, node.clone(), FallbackPolicy::RejectMaterialization).unwrap();
            let options = CursorOptions {
                batch_rows,
                ..Default::default()
            };
            while op.next(&ctx, &options, 4096).unwrap().is_some() {}
            let mut plan = op.describe(&ctx);
            op.update_plan(&mut plan);
            let bytes = plan
                .operator
                .counters
                .as_ref()
                .and_then(|c| c.get("spilledBytes"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            eprintln!("batch {batch_rows}: {bytes} bytes spilled");
            written.push(bytes);
            drop(op);
            drop(ctx);
            assert_eq!(spill.reserved_bytes(), 0);
        }
        assert!(written[1] > 0);
        assert!(
            written[0] * 2 <= written[1] * 3,
            "batch size 1 wrote {} bytes, 512 wrote {}",
            written[0],
            written[1]
        );
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
                            let mut plan = op.describe(&ctx);
                            op.update_plan(&mut plan);
                            assert!(plan.complete, "{case}");
                            assert!(plan.full_input_before_output, "{case}");
                            assert_eq!(plan.operator.actual_rows, result.len() as i64, "{case}");
                            assert!(
                                matches!(&op.state, State::Binary(b)
                                    if b.index.is_none() && b.build.is_none()
                                        && b.probe.is_none() && b.spill.is_none()),
                                "{case}: exhausted join retains its working state",
                            );
                            if plan
                                .operator
                                .counters
                                .as_ref()
                                .and_then(|c| c.get("spilledBytes"))
                                .and_then(serde_json::Value::as_u64)
                                .is_some_and(|bytes| bytes > 0)
                            {
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
