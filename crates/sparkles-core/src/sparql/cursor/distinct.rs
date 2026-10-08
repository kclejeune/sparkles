//! Incremental DISTINCT with a charged set of previously emitted ID rows.
use super::Buffer;
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use rustc_hash::FxHashSet;

pub(super) struct Distinct {
    seen: FxHashSet<Vec<Id>>,
    charge: Option<RetainedCharge>,
    bytes: u64,
}

impl Distinct {
    pub(super) fn new(ctx: &Ctx) -> Result<Self> {
        Ok(Self {
            seen: FxHashSet::default(),
            charge: ctx.retained_charge(256)?,
            bytes: 256,
        })
    }

    pub(super) fn apply(&mut self, ctx: &Ctx, buffer: &mut Buffer) -> Result<()> {
        let width = buffer.table.width();
        let _scratch = ctx.charge(width as u64 * 8 + 64)?;
        let mut key = Vec::with_capacity(width);
        let mut output = 0;
        for row in 0..buffer.table.len {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            key.clear();
            key.extend(buffer.table.cols.iter().map(|column| column[row]));
            if self.seen.contains(&key) {
                continue;
            }
            // Covers key payloads, hash bucket capacity and simultaneous old/new
            // bucket allocations during growth. Reserve before cloning/inserting.
            let bytes = self.bytes.saturating_add(width as u64 * 16 + 192);
            if let Some(charge) = &mut self.charge {
                charge.resize(bytes)?;
            }
            self.bytes = bytes;
            self.seen.insert(key.clone());
            for column in &mut buffer.table.cols {
                column[output] = column[row];
            }
            output += 1;
        }
        for column in &mut buffer.table.cols {
            column.truncate(output);
        }
        buffer.table.len = output;
        buffer.reconcile()
    }
}
