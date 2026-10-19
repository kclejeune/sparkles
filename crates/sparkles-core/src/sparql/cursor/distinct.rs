//! Incremental DISTINCT. Input ordered on every output column drops a row equal to the
//! one before it and keeps only the last row. Other input keeps a charged set of the
//! rows emitted so far.
//!
//! Unordered input of a query that may spill deduplicates in passes (X08 §6.5). Once
//! the set of rows emitted would hold half the room the query had when the operator
//! started, it stops growing. Rows already in it are still dropped, and other rows are
//! written to one of [`PARTITIONS`] spill files chosen by a hash of the whole row. Once
//! the input ends the set is freed, and each file is deduplicated by a pass of its own,
//! which partitions what it passes on with another hash. Equal rows always land in the
//! same file, so each distinct row is emitted once. Rows from the files come after the
//! input's other rows, so this needs input with no order to keep.
//!
//! A numbered DISTINCT deduplicates below an ORDER BY whose keys it carries (item 85).
//! It adds to each input row its number in the input, deduplicates on the other
//! columns and keeps each row's first occurrence with its number. The sort above it
//! orders on its keys and then on that number, which gives the order that sorting
//! first and deduplicating after would give, whichever pass emitted the row.
use super::Buffer;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::table::VarId;
use crate::spill::{QuerySpill, SpillKind, SpillReader, SpillRun, SpillWriter};
use rustc_hash::{FxBuildHasher, FxHashSet};
use std::hash::BuildHasher;
use std::sync::Arc;

/// The rows emitted so far. A single column keeps its IDs, which avoids a heap key and
/// its hashing per row. Wider rows live in one flat arena of IDs, and the table holds
/// their row numbers.
enum Seen {
    One(FxHashSet<Id>),
    Many {
        table: hashbrown::HashTable<u32>,
        rows: Vec<Id>,
    },
}

/// The bytes a hash table of `capacity` entries of `entry` bytes allocates: buckets of a
/// power of two at most seven eighths full, each with one control byte.
fn table_bytes(capacity: usize, entry: usize) -> u64 {
    if capacity == 0 {
        return 0;
    }
    let buckets = (capacity.saturating_mul(8) / 7).next_power_of_two();
    (buckets as u64).saturating_mul(entry as u64 + 1) + 64
}

pub(super) struct Distinct {
    seen: Option<Seen>,
    /// Whether the input is ordered on every output column, so that equal rows are next
    /// to each other.
    adjacent: bool,
    /// The last row of the previous batch of ordered input.
    last: Option<Vec<Id>>,
    /// Zero-column input: whether its one distinct row went out.
    emitted_empty: bool,
    charge: Option<RetainedCharge>,
    bytes: u64,
    /// The bytes of the empty state.
    base: u64,
    /// Where rows go once the set stops growing, when the query may spill.
    query: Option<Arc<QuerySpill>>,
    /// The set's bytes at which it stops growing.
    threshold: u64,
    /// Whether the set has stopped growing in this pass.
    frozen: bool,
    /// The rows this pass passes on, by partition.
    writers: Vec<Option<SpillWriter>>,
    /// The partitions later passes read, each with its partitioning depth.
    pending: Vec<(Arc<SpillRun>, u64)>,
    /// How many times the rows of this pass were partitioned.
    depth: u64,
    /// The partition this pass reads.
    source: Option<SpillReader>,
    /// Whether the last column numbers the input rows and is not part of the key.
    numbered: bool,
    /// The number of the next input row.
    next_number: i64,
    /// What the plan reports.
    pub(super) spilled: u64,
    pub(super) passes: usize,
}

/// The spill files one pass divides the rows it passes on among.
const PARTITIONS: usize = 16;
/// The least set a pass keeps before it stops growing.
const MIN_PASS_BYTES: u64 = 256 << 10;

impl Distinct {
    /// `value_ordered` tells that the input is in an order that operator orders do not
    /// record, such as an ORDER BY's, which forbids passing rows to later passes.
    /// `numbered` tells that the last of `vars` numbers the input rows, which the
    /// operator adds, and that the order the operator gives does not matter.
    pub(super) fn new(
        ctx: &Ctx,
        vars: &[VarId],
        input_order: &[VarId],
        value_ordered: bool,
        numbered: bool,
    ) -> Result<Self> {
        let keys = &vars[..vars.len() - usize::from(numbered)];
        let adjacent = !keys.is_empty() && keys.iter().all(|v| input_order.contains(v));
        let base = 256 + vars.len() as u64 * 8;
        Ok(Self {
            seen: None,
            adjacent,
            last: None,
            emitted_empty: false,
            charge: ctx.retained_charge(base)?,
            bytes: base,
            base,
            // Rows read back from disk come last, which only unordered input allows.
            query: if adjacent
                || keys.is_empty()
                || (!numbered && (!input_order.is_empty() || value_ordered))
            {
                None
            } else {
                ctx.spill.clone()
            },
            threshold: 0,
            frozen: false,
            writers: Vec::new(),
            pending: Vec::new(),
            depth: 0,
            source: None,
            numbered,
            next_number: 0,
            spilled: 0,
            passes: 0,
        })
    }

    /// Add each input row's number as the last column, when the operator numbers rows.
    pub(super) fn number(&mut self, buffer: &mut Buffer, var: VarId) -> Result<()> {
        if !self.numbered {
            return Ok(());
        }
        let len = buffer.table.len;
        let first = self.next_number;
        self.next_number += len as i64;
        let column: Vec<Id> = (first..first + len as i64)
            .map(|n| Id::from_i64(n).ok_or_else(|| Error::invalid("too many DISTINCT rows")))
            .collect::<Result<_>>()?;
        buffer.table.vars.push(var);
        buffer.table.cols.push(column);
        buffer.reconcile()
    }

    /// Whether rows passed to later passes are still to be read.
    pub(super) fn has_more(&self) -> bool {
        self.source.is_some()
            || !self.pending.is_empty()
            || self.writers.iter().any(Option::is_some)
    }

    /// The next rows of the partitions that passes passed on, deduplicated, once the
    /// input has ended.
    pub(super) fn drain(&mut self, ctx: &Arc<Ctx>, vars: &[VarId]) -> Result<Option<Buffer>> {
        loop {
            if let Some(reader) = &mut self.source {
                if let Some((columns, len)) = reader.next_frame()? {
                    let mut buffer = Buffer::new(ctx, vars, len)?;
                    for (to, from) in buffer.table.cols.iter_mut().zip(&columns) {
                        to.extend_from_slice(from);
                    }
                    buffer.table.len = len;
                    buffer.reconcile()?;
                    drop(columns);
                    self.apply(ctx, &mut buffer)?;
                    if buffer.table.len > 0 {
                        return Ok(Some(buffer));
                    }
                    continue;
                }
                self.source = None;
            }
            for writer in std::mem::take(&mut self.writers).into_iter().flatten() {
                let run = writer.finish()?;
                self.spilled = self.spilled.saturating_add(run.bytes());
                self.pending.push((Arc::new(run), self.depth + 1));
            }
            let Some((run, depth)) = self.pending.pop() else {
                return Ok(None);
            };
            // The previous pass's rows went out: free its set and read the next file.
            self.seen = None;
            self.frozen = false;
            self.retain(self.base)?;
            self.depth = depth;
            self.source = Some(run.reader()?);
            self.passes += 1;
        }
    }

    /// Write the rows of `buffer` marked in `passed` to the partitions of later passes.
    fn pass_on(&mut self, ctx: &Ctx, buffer: &Buffer, passed: &[usize]) -> Result<()> {
        use std::hash::{Hash, Hasher};
        let query = self.query.clone().expect("a spilling DISTINCT");
        let table = &buffer.table;
        let _gather = ctx.charge(passed.len() as u64 * (table.cols.len() as u64 * 8 + 8) + 256)?;
        let seed = self.depth.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5bd1_e995;
        let mut parts: Vec<Vec<usize>> = vec![Vec::new(); PARTITIONS];
        let width = table.width() - usize::from(self.numbered);
        for &row in passed {
            let mut hasher = rustc_hash::FxHasher::default();
            for column in &table.cols[..width] {
                column[row].hash(&mut hasher);
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
                *writer = Some(query.create(SpillKind::DistinctPartition, table.cols.len())?);
            }
            let slices: Vec<&[Id]> = columns.iter().map(Vec::as_slice).collect();
            writer
                .as_mut()
                .expect("an open spill file")
                .write(&slices, rows.len())?;
        }
        Ok(())
    }

    fn retain(&mut self, bytes: u64) -> Result<()> {
        if let Some(charge) = &mut self.charge {
            charge.resize(bytes)?;
        }
        self.bytes = bytes;
        Ok(())
    }

    pub(super) fn apply(&mut self, ctx: &Ctx, buffer: &mut Buffer) -> Result<()> {
        // The key's width: a row's number is not part of it.
        let width = buffer.table.width() - usize::from(self.numbered);
        let len = buffer.table.len;
        if width == 0 {
            let keep = usize::from(len > 0 && !self.emitted_empty);
            self.emitted_empty |= keep == 1;
            buffer.table.len = keep;
            return buffer.reconcile();
        }
        if self.adjacent {
            return self.apply_adjacent(ctx, buffer);
        }
        // Room for every row of the batch to be new, reserved before the set grows. While
        // a table grows, its old and new allocations coexist.
        let numbered = self.numbered;
        let seen = self.seen.get_or_insert_with(|| {
            if width == 1 && !numbered {
                Seen::One(FxHashSet::default())
            } else {
                Seen::Many {
                    table: hashbrown::HashTable::new(),
                    rows: Vec::new(),
                }
            }
        });
        let (size, capacity, entry, arena) = match seen {
            Seen::One(ids) => (ids.len(), ids.capacity(), 8, 0),
            Seen::Many { table, rows } => (table.len(), table.capacity(), 4, rows.capacity()),
        };
        let needed = size.saturating_add(len);
        if self.query.is_some() && self.threshold == 0 {
            self.threshold = (ctx.memory_room() / 2).max(MIN_PASS_BYTES);
        }
        if self.query.is_some()
            && !self.frozen
            && size > 0
            && needed > capacity
            && (table_bytes(needed.max(capacity.saturating_mul(2)), entry)
                .saturating_add(needed.max(capacity.saturating_mul(2)) as u64 * width as u64 * 8)
                >= self.threshold
                || ctx.memory_room() < self.threshold / 2)
        {
            self.frozen = true;
        }
        if needed > capacity && !self.frozen {
            let target = needed.max(capacity.saturating_mul(2));
            let old = table_bytes(capacity, entry);
            let new = table_bytes(target, entry);
            let arena_old = arena as u64 * 8;
            let arena_new = if width > 1 {
                (target.saturating_mul(width) as u64).saturating_mul(8)
            } else {
                0
            };
            let during = old + new + arena_old + arena_new + 256;
            match self.retain(during) {
                Ok(()) => {}
                // A set that holds rows stops growing when it no longer fits.
                Err(Error::BudgetExceeded(b))
                    if self.query.is_some()
                        && size > 0
                        && matches!(
                            b.kind,
                            crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
                        ) =>
                {
                    self.frozen = true;
                    return self.apply(ctx, buffer);
                }
                Err(error) => return Err(error),
            }
            let seen = self.seen.as_mut().expect("distinct state");
            match seen {
                Seen::One(ids) => ids.reserve(target - ids.len()),
                Seen::Many { table, rows } => {
                    table.reserve(target - table.len(), |&r| {
                        let r = r as usize;
                        FxBuildHasher.hash_one(&rows[r * width..(r + 1) * width])
                    });
                    rows.reserve_exact(target * width - rows.len());
                }
            }
            let (capacity, arena) = match seen {
                Seen::One(ids) => (ids.capacity(), 0),
                Seen::Many { table, rows } => (table.capacity(), rows.capacity()),
            };
            self.retain(table_bytes(capacity, entry) + arena as u64 * 8 + 256)?;
        }
        if self.frozen {
            return self.apply_frozen(ctx, buffer);
        }
        let seen = self.seen.as_mut().expect("distinct state");
        let mut output = 0;
        match seen {
            Seen::One(ids) => {
                let column = &mut buffer.table.cols[0];
                for row in 0..len {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let id = column[row];
                    if ids.insert(id) {
                        column[output] = id;
                        output += 1;
                    }
                }
            }
            Seen::Many { table, rows } => {
                let mut key = Vec::with_capacity(width);
                for row in 0..len {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    key.clear();
                    key.extend(buffer.table.cols[..width].iter().map(|column| column[row]));
                    let hash = FxBuildHasher.hash_one(key.as_slice());
                    let found = table
                        .find(hash, |&r| {
                            let r = r as usize;
                            rows[r * width..(r + 1) * width] == key[..]
                        })
                        .is_some();
                    if found {
                        continue;
                    }
                    let number = (rows.len() / width) as u32;
                    rows.extend_from_slice(&key);
                    table.insert_unique(hash, number, |&r| {
                        let r = r as usize;
                        FxBuildHasher.hash_one(&rows[r * width..(r + 1) * width])
                    });
                    for column in &mut buffer.table.cols {
                        column[output] = column[row];
                    }
                    output += 1;
                }
            }
        }
        for column in &mut buffer.table.cols {
            column.truncate(output);
        }
        buffer.table.len = output;
        buffer.reconcile()
    }

    /// A set that no longer grows: rows in it are dropped and the others go to the
    /// partitions of later passes, so none is emitted now.
    fn apply_frozen(&mut self, ctx: &Ctx, buffer: &mut Buffer) -> Result<()> {
        let len = buffer.table.len;
        let width = buffer.table.width() - usize::from(self.numbered);
        let _rows = ctx.charge(len as u64 * 8 + 64)?;
        let mut passed = Vec::new();
        {
            let seen = self.seen.as_ref().expect("distinct state");
            let mut key = Vec::with_capacity(width);
            for row in 0..len {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let found = match seen {
                    Seen::One(ids) => ids.contains(&buffer.table.cols[0][row]),
                    Seen::Many { table, rows } => {
                        key.clear();
                        key.extend(buffer.table.cols[..width].iter().map(|column| column[row]));
                        let hash = FxBuildHasher.hash_one(key.as_slice());
                        table
                            .find(hash, |&r| {
                                let r = r as usize;
                                rows[r * width..(r + 1) * width] == key[..]
                            })
                            .is_some()
                    }
                };
                if !found {
                    passed.push(row);
                }
            }
        }
        if !passed.is_empty() {
            self.pass_on(ctx, buffer, &passed)?;
        }
        for column in &mut buffer.table.cols {
            column.clear();
        }
        buffer.table.len = 0;
        buffer.reconcile()
    }

    /// Ordered input: a row passes when it differs from the row before it, which for the
    /// first row of a batch is the last row of the previous batch.
    fn apply_adjacent(&mut self, ctx: &Ctx, buffer: &mut Buffer) -> Result<()> {
        let table = &mut buffer.table;
        let len = table.len;
        if len == 0 {
            return Ok(());
        }
        ctx.check()?;
        let _scratch = ctx.charge(len as u64 + table.width() as u64 * 8 + 64)?;
        let keys = &table.cols[..table.width() - usize::from(self.numbered)];
        let mut keep = vec![false; len];
        keep[0] = self
            .last
            .as_ref()
            .is_none_or(|last| keys.iter().zip(last).any(|(c, &v)| c[0] != v));
        for row in 1..len {
            keep[row] = keys.iter().any(|c| c[row] != c[row - 1]);
        }
        let last: Vec<Id> = keys.iter().map(|c| c[len - 1]).collect();
        table.filter_rows(&keep);
        self.last = Some(last);
        buffer.reconcile()
    }
}
