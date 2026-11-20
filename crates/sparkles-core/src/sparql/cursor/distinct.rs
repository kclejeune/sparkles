//! Incremental DISTINCT. Input ordered on every output column drops a row equal to the
//! one before it and keeps only the last row. Input ordered on some output columns
//! keeps a charged set of the rows of the current run of its first order column, since
//! equal rows lie in one such run, and empties it when the run ends (item 93). Other
//! input keeps a charged set of the rows emitted so far.
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
use crate::sparql::charged::{Charged, Rows};
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::table::VarId;
use crate::spill::{QuerySpill, SpillKind, SpillReader, SpillRun, SpillWriter};
use rustc_hash::FxBuildHasher;
use std::sync::Arc;

/// A buffer of the set, charged on the query's retained account (item 1183).
type Held<V> = Charged<V, Option<RetainedCharge>>;

/// The rows emitted so far. A single column keeps its IDs, which avoids a heap key and
/// its hashing per row. Wider rows live in one flat arena of IDs, and a table holds
/// their row numbers, both on one charge of their allocations.
enum Seen {
    One(Held<hashbrown::HashSet<Id, FxBuildHasher>>),
    Many(Held<Rows>),
}

pub(super) struct Distinct {
    seen: Option<Seen>,
    /// Whether the input is ordered on every output column, so that equal rows are next
    /// to each other.
    adjacent: bool,
    /// The last row of the previous batch of ordered input.
    last: Option<Vec<Id>>,
    /// The column on which input that is not ordered on every output column is
    /// ordered, whose runs of one value hold every copy of a row (item 93).
    segment: Option<usize>,
    /// The value of the run the set holds.
    run: Option<Id>,
    /// Zero-column input: whether its one distinct row went out.
    emitted_empty: bool,
    /// The bytes of the empty state. The set's buffers charge their own.
    _charge: Option<RetainedCharge>,
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

/// Whether `error` is a refusal of the query's or the server's memory, which a spilling
/// DISTINCT meets by holding less.
fn memory_refusal(error: &Error) -> bool {
    matches!(
        error,
        Error::BudgetExceeded(b)
            if matches!(b.kind, crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory)
    )
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
        // The first column of the input's order divides it into runs of one value, and
        // equal rows lie in one run. A numbered DISTINCT's order does not matter.
        let segment = (!adjacent && !numbered)
            .then(|| input_order.first())
            .flatten()
            .and_then(|first| keys.iter().position(|v| v == first));
        let base = 256 + vars.len() as u64 * 8;
        Ok(Self {
            seen: None,
            adjacent,
            last: None,
            segment,
            run: None,
            emitted_empty: false,
            _charge: ctx.retained_charge(base)?,
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

    /// A later partition may precede rows already emitted in the input's order.
    pub(super) fn may_reorder(&self) -> bool {
        self.query.is_some()
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
            // The pass sizes its set from the room it has when it starts.
            self.threshold = 0;
            self.depth = depth;
            self.source = Some(run.reader()?);
            self.passes += 1;
        }
    }

    /// The bytes [`Self::pass_on`] gathers for `rows` rows of `width` columns.
    fn gather_bytes(rows: usize, width: usize) -> u64 {
        rows as u64 * (width as u64 * 8 + 8) + 256
    }

    /// Write the rows of `buffer` marked in `passed` to the partitions of later passes,
    /// under a charge of [`Self::gather_bytes`] that the caller holds.
    fn pass_on(&mut self, buffer: &Buffer, passed: &[usize]) -> Result<()> {
        let query = self.query.clone().expect("a spilling DISTINCT");
        let table = &buffer.table;
        let mut parts: Vec<Vec<usize>> = vec![Vec::new(); PARTITIONS];
        let width = table.width() - usize::from(self.numbered);
        for &row in passed {
            let hash = super::joinspill::row_hash(
                self.depth,
                table.cols[..width].iter().map(|column| column[row]),
            );
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
        let output = if let Some(column) = self.segment {
            self.apply_segmented(ctx, buffer, column)?
        } else {
            if !self.reserve(ctx, width, len)? {
                return self.apply_frozen(ctx, buffer);
            }
            self.insert(ctx, buffer, 0..len, 0)?
        };
        for column in &mut buffer.table.cols {
            column.truncate(output);
        }
        buffer.table.len = output;
        buffer.reconcile()
    }

    /// Input ordered on a column: equal rows have equal values there and lie in one run of
    /// them, so the set holds one run at a time and starts again at each new value. The
    /// rows that pass keep the input's order. Returns how many rows pass, compacted to the
    /// front of the batch.
    fn apply_segmented(&mut self, ctx: &Ctx, buffer: &mut Buffer, column: usize) -> Result<usize> {
        let width = buffer.table.width() - usize::from(self.numbered);
        let len = buffer.table.len;
        let mut output = 0;
        let mut start = 0;
        while start < len {
            let id = buffer.table.cols[column][start];
            let mut end = start + 1;
            while end < len && buffer.table.cols[column][end] == id {
                end += 1;
            }
            if self.run != Some(id) {
                self.restart()?;
                self.run = Some(id);
            }
            // A query without spill never freezes the set.
            let fits = self.reserve(ctx, width, end - start)?;
            debug_assert!(fits, "a segmented DISTINCT does not spill");
            output = self.insert(ctx, buffer, start..end, output)?;
            start = end;
        }
        Ok(output)
    }

    /// Empty the set for the next run of a segmented DISTINCT. A table whose capacity a
    /// long run left far larger than what it holds is freed, so that many short runs
    /// after a long one do not each clear its whole capacity.
    fn restart(&mut self) -> Result<()> {
        let oversized = match &self.seen {
            None => return Ok(()),
            Some(Seen::One(ids)) => ids.capacity() > 1024 && ids.len() * 8 < ids.capacity(),
            Some(Seen::Many(set)) => set.capacity() > 1024 && set.len() * 8 < set.capacity(),
        };
        if oversized {
            self.seen = None;
            return Ok(());
        }
        match self.seen.as_mut().expect("distinct state") {
            Seen::One(ids) => ids.clear(),
            Seen::Many(set) => set.clear(),
        }
        Ok(())
    }

    /// Room in the set for `len` more rows of `width` key columns. A one-column set
    /// grows on its charge, which admits its old and new allocations together, and a
    /// wider set admits its table's and its arena's growth as one before either
    /// allocates. Returns false when the set of a query that may spill stops growing
    /// instead, which it does before a growth that would take its predicted size to the
    /// pass's threshold, or when a growth is refused.
    fn reserve(&mut self, ctx: &Ctx, width: usize, len: usize) -> Result<bool> {
        if self.seen.is_none() {
            let account = ctx.retained_charge(0)?;
            self.seen = Some(if width == 1 && !self.numbered {
                Seen::One(Charged::empty(account)?)
            } else {
                Seen::Many(Charged::rows(width, account)?)
            });
        }
        if self.frozen {
            return Ok(false);
        }
        // What the set holds once it grows now, or zero when it has room, and the bytes
        // of the new allocations the growth makes.
        let (size, predicted, allocations) = match self.seen.as_ref().expect("distinct state") {
            Seen::One(ids) => (ids.len(), ids.growth(len), ids.growth(len)),
            Seen::Many(set) => (set.len(), set.growth(len), set.allocations(len)),
        };
        if predicted == 0 {
            return Ok(true);
        }
        let spills = self.query.is_some() && size > 0;
        if self.query.is_some() && self.threshold == 0 {
            self.threshold = (ctx.memory_room() / 2).max(MIN_PASS_BYTES);
        }
        // A set that holds rows also stops growing when the room is less than the
        // growth's new allocations taken whole. The growth itself needs less, since a
        // wider set frees its old table before its arena grows, but it would leave the
        // operators above the set, such as the sort above a numbered DISTINCT, too
        // little to work in.
        let room = ctx.memory_room();
        if spills
            && (predicted >= self.threshold || room < self.threshold / 2 || room < allocations)
        {
            self.frozen = true;
            return Ok(false);
        }
        let grown = match self.seen.as_mut().expect("distinct state") {
            Seen::One(ids) => ids.reserve(len),
            Seen::Many(set) => set.reserve(len),
        };
        match grown {
            Ok(()) => Ok(true),
            // A set that holds rows stops growing when it no longer fits, and a refusal
            // left its buffers as they were.
            Err(error) if spills && memory_refusal(&error) => {
                self.frozen = true;
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Insert the rows `range` of `buffer` into the set, whose room [`Self::reserve`]
    /// made, and move the new ones to the front from row `output` on. Returns the
    /// number of rows at the front after them.
    fn insert(
        &mut self,
        ctx: &Ctx,
        buffer: &mut Buffer,
        range: std::ops::Range<usize>,
        mut output: usize,
    ) -> Result<usize> {
        let width = buffer.table.width() - usize::from(self.numbered);
        let seen = self.seen.as_mut().expect("distinct state");
        match seen {
            Seen::One(ids) => {
                let column = &mut buffer.table.cols[0];
                for row in range {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let id = column[row];
                    if ids.insert_reserved(id) {
                        column[output] = id;
                        output += 1;
                    }
                }
            }
            Seen::Many(set) => {
                let mut key = Vec::with_capacity(width);
                for row in range {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    key.clear();
                    key.extend(buffer.table.cols[..width].iter().map(|column| column[row]));
                    if !set.insert_reserved(&key, Rows::hash(&key)) {
                        continue;
                    }
                    for column in &mut buffer.table.cols {
                        column[output] = column[row];
                    }
                    output += 1;
                }
            }
        }
        Ok(output)
    }

    /// A set that no longer grows: rows in it are dropped and the others go to the
    /// partitions of later passes, so none is emitted now. The batch goes in ranges whose
    /// scratch fits, halving a range whose scratch is refused before any of its rows are
    /// written, so the rows of each partition keep their order. A refusal of the scratch
    /// of one row is the query's.
    fn apply_frozen(&mut self, ctx: &Ctx, buffer: &mut Buffer) -> Result<()> {
        let len = buffer.table.len;
        let (mut start, mut step) = (0, len.max(1));
        while start < len {
            let end = start + step.min(len - start);
            match self.freeze_range(ctx, buffer, start..end) {
                Ok(()) => start = end,
                Err(error) if end - start > 1 && memory_refusal(&error) => {
                    step = (end - start) / 2;
                }
                Err(error) => return Err(error),
            }
        }
        for column in &mut buffer.table.cols {
            column.clear();
        }
        buffer.table.len = 0;
        buffer.reconcile()
    }

    /// Pass on the rows `range` of `buffer` that the set does not hold. Its scratch is
    /// charged before any row is written, so a refusal of it writes nothing.
    fn freeze_range(
        &mut self,
        ctx: &Ctx,
        buffer: &Buffer,
        range: std::ops::Range<usize>,
    ) -> Result<()> {
        let width = buffer.table.width() - usize::from(self.numbered);
        let _rows = ctx.charge(range.len() as u64 * 8 + 64)?;
        let mut passed = Vec::with_capacity(range.len());
        {
            let seen = self.seen.as_ref().expect("distinct state");
            let mut key = Vec::with_capacity(width);
            for row in range {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let found = match seen {
                    Seen::One(ids) => ids.contains(&buffer.table.cols[0][row]),
                    Seen::Many(set) => {
                        key.clear();
                        key.extend(buffer.table.cols[..width].iter().map(|column| column[row]));
                        set.contains(&key, Rows::hash(&key))
                    }
                };
                if !found {
                    passed.push(row);
                }
            }
        }
        if passed.is_empty() {
            return Ok(());
        }
        let _gather = ctx.charge(Self::gather_bytes(passed.len(), buffer.table.cols.len()))?;
        self.pass_on(buffer, &passed)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoreOptions};

    /// The bytes and capacities of the set's table and row arena.
    fn sizes(distinct: &Distinct) -> (u64, u64, usize, usize) {
        match distinct.seen.as_ref().expect("distinct state") {
            Seen::Many(set) => (
                set.table_bytes(),
                set.arena_bytes(),
                set.capacity(),
                set.arena_capacity(),
            ),
            Seen::One(_) => panic!("these cases keep a row arena"),
        }
    }

    fn lens(distinct: &Distinct) -> (usize, usize) {
        match distinct.seen.as_ref().expect("distinct state") {
            Seen::Many(set) => (set.len(), set.arena_len()),
            Seen::One(_) => unreachable!(),
        }
    }

    /// What the second batch of [`growth`] should do.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Expect {
        /// The table doubles and the arena grows to the table's room.
        Grows,
        /// The arena's growth to the table's room does not fit, and it grows to what
        /// it needs.
        Exact,
        /// Not even that fits.
        Refused,
        /// The set of a spilling query stops growing.
        Freezes,
    }

    /// A DISTINCT over 32 rows and then 40 more, of which eight repeat earlier rows, under
    /// `limit`. Hashbrown rounds 32 entries to room for 56 and the arena grows to match,
    /// so the second batch grows both. They grow one after the other on one charge, which
    /// admits the larger of the two growths before either, and not both.
    /// Returns the room the batch needs with the arena at the table's room and with the
    /// arena at what it needs, read from the buffers' sizes.
    fn growth(limit: u64, numbered: bool, spill_on: bool, expect: Expect) -> (u64, u64) {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.var("value");
        ctx.var("second");
        ctx.mem_limit = limit;
        if spill_on {
            ctx.spill = Some(Arc::new(spill.query(None)));
        }
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_owner();
        let baseline = ctx.live_bytes();
        let mut distinct = Distinct::new(&ctx, &[0, 1], &[], false, numbered).unwrap();
        let batch = |first: i64, len: usize| {
            let mut buffer = Buffer::new(&ctx, &[0, 1], len).unwrap();
            for i in first..first + len as i64 {
                buffer.table.cols[0].push(Id::from_i64(i).unwrap());
                let second = if numbered {
                    // The second batch repeats eight keys with later input numbers.
                    i + if first == 24 { 8 } else { 0 }
                } else {
                    i * 2
                };
                buffer.table.cols[1].push(Id::from_i64(second).unwrap());
            }
            buffer.table.len = len;
            buffer.reconcile().unwrap();
            buffer
        };
        let collect = |buffer: &Buffer| -> Vec<(Id, Id)> {
            buffer.table.cols[0]
                .iter()
                .copied()
                .zip(buffer.table.cols[1].iter().copied())
                .collect()
        };
        let mut first = batch(0, 32);
        distinct.apply(&ctx, &mut first).unwrap();
        let mut output = collect(&first);
        drop(first);
        let width = if numbered { 1 } else { 2 };
        let (old_table, old_arena, hash, arena) = sizes(&distinct);
        assert_eq!(
            (hash, arena),
            (56, 56 * width),
            "the arena keeps step with the table"
        );
        let rest = ctx.live_bytes() - old_table - old_arena;
        let neighbor = ctx
            .retained_charge(if expect == Expect::Freezes {
                800 << 10
            } else {
                4096
            })
            .unwrap();
        let mut next = batch(24, 40);
        let other = ctx.live_bytes() - old_table - old_arena;
        let result = distinct.apply(&ctx, &mut next);
        let (table, arena_bytes, hash_now, arena_now) = sizes(&distinct);
        match expect {
            Expect::Refused => {
                assert!(matches!(result, Err(Error::BudgetExceeded(_))));
                // The pair is refused before either buffer allocates, so the table, the
                // arena and the rows are as they were.
                assert_eq!((table, hash_now), (old_table, hash));
                assert_eq!((arena_bytes, arena_now), (old_arena, arena));
                assert_eq!(lens(&distinct), (32, 32 * width));
            }
            Expect::Freezes => {
                result.unwrap();
                assert!(distinct.frozen, "a growth must be a freeze point");
                assert_eq!((hash_now, arena_now), (hash, arena));
                assert_eq!(next.table.len, 0);
                assert!(distinct.has_more());
            }
            Expect::Grows | Expect::Exact => {
                result.unwrap();
                assert_eq!(hash_now, 112);
                // the exact need is room for every row of the batch
                let grown = if expect == Expect::Grows { 112 } else { 72 };
                assert_eq!(arena_now, grown * width);
                assert_eq!(lens(&distinct), (64, 64 * width));
                let needed =
                    other + (old_table + table + old_arena).max(table + old_arena + arena_bytes);
                assert!(
                    ctx.mem_peak() >= needed,
                    "peak {} missed the growths, which need {needed}",
                    ctx.mem_peak()
                );
            }
        }
        let (table, arena_bytes, _, _) = sizes(&distinct);
        let first_growth = old_table + table + old_arena;
        let full = other + first_growth.max(table + old_arena + (112 * width) as u64 * 8);
        let exact = other + first_growth.max(table + old_arena + (72 * width) as u64 * 8);
        if expect != Expect::Refused {
            output.extend(collect(&next));
        }
        drop(next);
        drop(neighbor);
        assert_eq!(
            ctx.live_bytes() - table - arena_bytes,
            rest,
            "the set charges what it holds"
        );
        if expect != Expect::Refused {
            while let Some(buffer) = distinct.drain(&ctx, &[0, 1]).unwrap() {
                output.extend(collect(&buffer));
            }
            // Both spill modes must match the eager first-occurrence result,
            // including the row numbers carried by numbered DISTINCT.
            output.sort_unstable();
            let expected: Vec<_> = (0..64)
                .map(|i| {
                    (
                        Id::from_i64(i).unwrap(),
                        Id::from_i64(if numbered {
                            i + if i >= 32 { 8 } else { 0 }
                        } else {
                            i * 2
                        })
                        .unwrap(),
                    )
                })
                .collect();
            assert_eq!(output, expected);
        }
        drop(distinct);
        assert_eq!(ctx.live_bytes(), baseline);
        assert_eq!(spill.reserved_bytes(), 0);
        (full, exact)
    }

    #[test]
    fn set_growth_is_charged_and_denied_before_allocation() {
        for numbered in [false, true] {
            let (full, exact) = growth(1 << 20, numbered, false, Expect::Grows);
            assert!(exact < full);
            growth(full - 1, numbered, false, Expect::Exact);
            growth(exact - 1, numbered, false, Expect::Refused);
            growth(1 << 20, numbered, true, Expect::Grows);
        }
    }

    #[test]
    fn set_growth_can_freeze_and_drain() {
        for numbered in [false, true] {
            growth(1 << 20, numbered, true, Expect::Freezes);
        }
    }

    /// A one-column set of a query that may spill stops growing once the table it would
    /// grow to reaches the pass's threshold, which is predicted from the table's own
    /// bound and not from row bytes the set does not keep. The rows it passes on come
    /// back from later passes, each once.
    #[test]
    fn a_one_column_set_freezes_at_its_predicted_table() {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.var("value");
        ctx.mem_limit = 1 << 20;
        ctx.spill = Some(Arc::new(spill.query(None)));
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_owner();
        let baseline = ctx.live_bytes();
        let mut distinct = Distinct::new(&ctx, &[0], &[], false, false).unwrap();
        let (rows, batch) = (100_000i64, 1000i64);
        let mut output = Vec::new();
        let mut largest = 0;
        for first in (0..rows).step_by(batch as usize) {
            let mut buffer = Buffer::new(&ctx, &[0], batch as usize).unwrap();
            // every value twice, half the input apart
            for i in first..first + batch {
                buffer.table.cols[0].push(Id::from_i64(i % (rows / 2)).unwrap());
            }
            buffer.table.len = batch as usize;
            buffer.reconcile().unwrap();
            distinct.apply(&ctx, &mut buffer).unwrap();
            output.extend(buffer.table.cols[0].iter().copied());
            if let Some(Seen::One(ids)) = &distinct.seen {
                largest = largest.max(ids.bytes());
                assert_eq!(ids.bytes(), ids.allocation_size() as u64);
            }
        }
        assert!(distinct.frozen, "the set must stop growing");
        // the threshold is half the room the query had, at least 256 KiB
        assert!(
            largest < distinct.threshold,
            "{largest} {}",
            distinct.threshold
        );
        while let Some(buffer) = distinct.drain(&ctx, &[0]).unwrap() {
            output.extend(buffer.table.cols[0].iter().copied());
        }
        assert!(distinct.passes > 0 && distinct.spilled > 0);
        output.sort_unstable();
        let expected: Vec<Id> = (0..rows / 2).map(|i| Id::from_i64(i).unwrap()).collect();
        assert_eq!(output, expected);
        drop(distinct);
        assert_eq!(ctx.live_bytes(), baseline);
        assert_eq!(spill.reserved_bytes(), 0);
    }

    /// `count` batches of `rows` new rows each, of `width` columns, under a budget that
    /// sets the pass's threshold at its least and leaves `room` bytes when the second
    /// batch arrives. Every row comes out once, and every charge returns. Returns
    /// whether the set stopped growing at the second batch and the sizes of its
    /// buffers after it.
    fn batches(width: usize, rows: usize, room: u64, count: usize) -> (bool, (u64, u64)) {
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.var("value");
        ctx.var("second");
        // the room at the first growth is below twice the least pass, so the threshold
        // is the least pass
        ctx.mem_limit = 512 << 10;
        ctx.spill = Some(Arc::new(spill.query(None)));
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_owner();
        let baseline = ctx.live_bytes();
        let vars: Vec<VarId> = (0..width as VarId).collect();
        let mut distinct = Distinct::new(&ctx, &vars, &[], false, false).unwrap();
        let value = |i: usize, c: usize| Id::from_i64((i * (c + 1)) as i64).unwrap();
        let collect = |buffer: &Buffer, output: &mut Vec<Vec<Id>>| {
            for row in 0..buffer.table.len {
                output.push(buffer.table.cols.iter().map(|c| c[row]).collect());
            }
        };
        let mut output = Vec::new();
        let mut neighbor = None;
        let mut froze = false;
        let mut after = (0, 0);
        for b in 0..count {
            let mut buffer = Buffer::new(&ctx, &vars, rows).unwrap();
            for (c, column) in buffer.table.cols.iter_mut().enumerate() {
                column.extend((b * rows..(b + 1) * rows).map(|i| value(i, c)));
            }
            buffer.table.len = rows;
            buffer.reconcile().unwrap();
            if b == 1 {
                let free = ctx.memory_room();
                assert!(free > room, "{free} bytes of room");
                neighbor = ctx.retained_charge(free - room).unwrap();
                assert_eq!(ctx.memory_room(), room);
            }
            distinct
                .apply(&ctx, &mut buffer)
                .unwrap_or_else(|e| panic!("batch {b} with {room} bytes of room: {e}"));
            if b == 1 {
                froze = distinct.frozen;
                after = match distinct.seen.as_ref().expect("distinct state") {
                    Seen::One(ids) => (ids.bytes(), 0),
                    Seen::Many(set) => (set.table_bytes(), set.arena_bytes()),
                };
            }
            collect(&buffer, &mut output);
        }
        while let Some(buffer) = distinct.drain(&ctx, &vars).unwrap() {
            collect(&buffer, &mut output);
        }
        output.sort_unstable();
        let expected: Vec<Vec<Id>> = (0..count * rows)
            .map(|i| (0..width).map(|c| value(i, c)).collect())
            .collect();
        assert_eq!(output.len(), expected.len(), "every row once");
        assert_eq!(output, expected);
        drop(neighbor);
        drop(distinct);
        assert_eq!(ctx.live_bytes(), baseline);
        assert_eq!(spill.reserved_bytes(), 0);
        (froze, after)
    }

    /// The table and the arena of a two-column set grow one after the other, and their
    /// pair is admitted before either allocates. Here the table's growth fits and the
    /// pair's does not, so neither grows and the frozen batch has the room for its
    /// scratch that a table grown alone would have taken.
    #[test]
    fn a_pair_growth_that_does_not_fit_leaves_both_buffers() {
        let (froze, after) = batches(2, 3584, 132_000, 2);
        assert!(froze, "the set must stop growing");
        assert_eq!(after, (20_496, 57_344), "neither buffer grows");
    }

    /// The pair's growth at the second batch admits 135,184 bytes, and its new table
    /// and arena take 155,680 bytes whole. With room between the two the set stops
    /// growing, as it did before its buffers were charged, and leaves that room to
    /// the operators above it.
    #[test]
    fn a_growth_leaves_the_room_of_its_new_allocations() {
        let (froze, after) = batches(2, 3584, 145_000, 2);
        assert!(froze, "the set must stop growing");
        assert_eq!(after, (20_496, 57_344), "neither buffer grows");
        let (froze, _) = batches(2, 3584, 155_680, 2);
        assert!(!froze, "the new allocations fit");
    }

    /// A set that grows where a formula of its final size would have frozen it holds
    /// more when it stops growing later, and its frozen batch passes rows on in
    /// smaller ranges where its scratch does not fit at once.
    #[test]
    fn a_later_freeze_passes_rows_on_in_ranges_that_fit() {
        let (froze, _) = batches(2, 3584, 160_000, 3);
        assert!(!froze, "the pair fits at the second batch");
        let (froze, _) = batches(1, 7168, 180_000, 3);
        assert!(!froze, "the table fits at the second batch");
    }
}
