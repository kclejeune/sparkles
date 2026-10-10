//! Incremental DISTINCT with a charged set of previously emitted ID rows.
use super::Buffer;
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use rustc_hash::FxHashSet;

/// The rows emitted so far. A single column keeps its IDs, which avoids a heap key and
/// its hashing per row.
enum Seen {
    One(FxHashSet<Id>),
    Many(FxHashSet<Vec<Id>>),
}

/// The retained reservation of the set of emitted rows.
struct Reserved {
    charge: Option<RetainedCharge>,
    bytes: u64,
}

impl Reserved {
    /// Reserve room for one more emitted row of `width` IDs before it is inserted.
    /// The charge covers the key, hash bucket capacity and the old and new buckets
    /// that coexist while the set grows.
    fn row(&mut self, width: usize) -> Result<()> {
        let bytes = self.bytes.saturating_add(width as u64 * 16 + 192);
        if let Some(charge) = &mut self.charge {
            charge.resize(bytes)?;
        }
        self.bytes = bytes;
        Ok(())
    }
}

pub(super) struct Distinct {
    seen: Option<Seen>,
    reserved: Reserved,
}

impl Distinct {
    pub(super) fn new(ctx: &Ctx) -> Result<Self> {
        Ok(Self {
            seen: None,
            reserved: Reserved {
                charge: ctx.retained_charge(256)?,
                bytes: 256,
            },
        })
    }

    pub(super) fn apply(&mut self, ctx: &Ctx, buffer: &mut Buffer) -> Result<()> {
        let width = buffer.table.width();
        let seen = self.seen.get_or_insert_with(|| {
            if width == 1 {
                Seen::One(FxHashSet::default())
            } else {
                Seen::Many(FxHashSet::default())
            }
        });
        let mut output = 0;
        match seen {
            Seen::One(ids) => {
                let column = &mut buffer.table.cols[0];
                for row in 0..buffer.table.len {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let id = column[row];
                    if ids.contains(&id) {
                        continue;
                    }
                    self.reserved.row(1)?;
                    ids.insert(id);
                    column[output] = id;
                    output += 1;
                }
            }
            Seen::Many(rows) => {
                let _scratch = ctx.charge(width as u64 * 8 + 64)?;
                let mut key = Vec::with_capacity(width);
                for row in 0..buffer.table.len {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    key.clear();
                    key.extend(buffer.table.cols.iter().map(|column| column[row]));
                    if rows.contains(&key) {
                        continue;
                    }
                    self.reserved.row(width)?;
                    rows.insert(key.clone());
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
}
