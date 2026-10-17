//! Incremental DISTINCT. Input ordered on every output column drops a row equal to the
//! one before it and keeps only the last row. Other input keeps a charged set of the
//! rows emitted so far.
use super::Buffer;
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::sparql::table::VarId;
use rustc_hash::{FxBuildHasher, FxHashSet};
use std::hash::BuildHasher;

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
}

impl Distinct {
    pub(super) fn new(ctx: &Ctx, vars: &[VarId], input_order: &[VarId]) -> Result<Self> {
        Ok(Self {
            seen: None,
            adjacent: !vars.is_empty() && vars.iter().all(|v| input_order.contains(v)),
            last: None,
            emitted_empty: false,
            charge: ctx.retained_charge(256 + vars.len() as u64 * 8)?,
            bytes: 256 + vars.len() as u64 * 8,
        })
    }

    fn retain(&mut self, bytes: u64) -> Result<()> {
        if let Some(charge) = &mut self.charge {
            charge.resize(bytes)?;
        }
        self.bytes = bytes;
        Ok(())
    }

    pub(super) fn apply(&mut self, ctx: &Ctx, buffer: &mut Buffer) -> Result<()> {
        let width = buffer.table.width();
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
        let seen = self.seen.get_or_insert_with(|| {
            if width == 1 {
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
        if needed > capacity {
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
            self.retain(during)?;
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
                    key.extend(buffer.table.cols.iter().map(|column| column[row]));
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
        let mut keep = vec![false; len];
        keep[0] = self
            .last
            .as_ref()
            .is_none_or(|last| table.cols.iter().zip(last).any(|(c, &v)| c[0] != v));
        for row in 1..len {
            keep[row] = table.cols.iter().any(|c| c[row] != c[row - 1]);
        }
        let last: Vec<Id> = table.cols.iter().map(|c| c[len - 1]).collect();
        table.filter_rows(&keep);
        self.last = Some(last);
        buffer.reconcile()
    }
}
