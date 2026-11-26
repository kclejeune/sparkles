//! A spill stream of evaluated SUM and AVG arguments that could not join a retained
//! subtotal. The first column orders them by group slot and aggregate. The second
//! contains the original argument IDs, including UNDEF, in arrival order per subtotal.

use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, RetainedCharge};
use crate::spill::{SpillKind, SpillRun, SpillWriter};
use std::sync::Arc;

/// Limit the working set for writing, reading and repartitioning a stream, even when
/// a subtotal has a long suffix.
const FRAME_ROWS: usize = 128;
const WRITER_BYTES: u64 = 12 << 10;
const READER_BYTES: u64 = 8 << 10;
/// A binary split of a u64 range can leave at most 64 pending siblings.
const STACK_BYTES: u64 = 64 * (size_of::<Run>() + size_of::<SpillRun>() + 32) as u64 + 512;

pub(super) fn subtotal(slot: usize, aggregate: usize) -> u64 {
    ((slot as u64) << 32) | u64::from(u32::try_from(aggregate).expect("aggregate index"))
}

struct Run {
    file: Arc<SpillRun>,
    min: u64,
    max: u64,
}

struct Writer {
    file: SpillWriter,
    keys: Vec<Id>,
    args: Vec<Id>,
    min: u64,
    max: u64,
    _charge: Option<RetainedCharge>,
}

impl Writer {
    fn new(ctx: &Ctx) -> Result<Self> {
        let charge = ctx.retained_charge(WRITER_BYTES)?;
        let query = ctx.spill.as_ref().expect("a spilling grouping");
        let mut file = query.create(SpillKind::GroupPartition, 2)?;
        file.set_frame_rows(FRAME_ROWS);
        Ok(Self {
            file,
            keys: Vec::with_capacity(FRAME_ROWS),
            args: Vec::with_capacity(FRAME_ROWS),
            min: u64::MAX,
            max: 0,
            _charge: charge,
        })
    }

    fn push(&mut self, key: u64, id: Id) -> Result<()> {
        self.keys.push(Id(key));
        self.args.push(id);
        self.min = self.min.min(key);
        self.max = self.max.max(key);
        if self.args.len() == FRAME_ROWS {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.file
            .write(&[&self.keys, &self.args], self.args.len())?;
        self.keys.clear();
        self.args.clear();
        Ok(())
    }

    fn finish(mut self) -> Result<Run> {
        self.flush()?;
        Ok(Run {
            file: Arc::new(self.file.finish()?),
            min: self.min,
            max: self.max,
        })
    }
}

struct Buffered {
    rows: std::iter::Peekable<std::vec::IntoIter<(u64, Id)>>,
    _charge: Option<RetainedCharge>,
}

pub(crate) struct Deferred {
    input: Option<Writer>,
    pending: Vec<Run>,
    buffered: Option<Buffered>,
    written: u64,
    _charge: Option<RetainedCharge>,
}

impl Deferred {
    pub(super) fn new(ctx: &Ctx) -> Result<Self> {
        let charge = ctx.retained_charge(STACK_BYTES)?;
        Ok(Self {
            input: Some(Writer::new(ctx)?),
            pending: Vec::with_capacity(64),
            buffered: None,
            written: 0,
            _charge: charge,
        })
    }

    pub(super) fn push(&mut self, key: u64, id: Id) -> Result<()> {
        self.input.as_mut().expect("open arguments").push(key, id)
    }

    fn push_run(&mut self, run: Run) {
        self.written += run.file.bytes();
        self.pending.push(run);
    }

    pub(super) fn seal(&mut self) -> Result<()> {
        if let Some(input) = self.input.take() {
            self.push_run(input.finish()?);
        }
        Ok(())
    }

    pub(super) fn take_written(&mut self) -> u64 {
        std::mem::take(&mut self.written)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.buffered.is_none()
    }

    /// Read subtotals in slot and aggregate order. Split only the next outstanding run,
    /// preserving arrival order on each side. A leaf can replay directly into one
    /// subtotal regardless of the number of arguments it holds.
    pub(super) fn replay(
        &mut self,
        ctx: &Ctx,
        key: u64,
        mut fold: impl FnMut(Id) -> Result<()>,
    ) -> Result<()> {
        let _scratch = ctx.charge(READER_BYTES)?;
        loop {
            if let Some(buffered) = &mut self.buffered {
                if buffered.rows.peek().is_none_or(|&(slot, _)| slot != key) {
                    break;
                }
                while let Some((_, id)) = buffered.rows.next_if(|&(slot, _)| slot == key) {
                    fold(id)?;
                }
                if buffered.rows.peek().is_none() {
                    self.buffered = None;
                }
                return ctx.check();
            }
            let Some(run) = self.pending.pop().filter(|run| run.min == key) else {
                break;
            };
            let mut reader = run.file.reader()?;
            if run.max == key {
                while let Some((columns, _)) = reader.next_frame()? {
                    ctx.check()?;
                    for &id in &columns[1] {
                        fold(id)?;
                    }
                }
                return ctx.check();
            }
            // Small runs fit in a stable in-memory sort. Keep the charge until
            // their last argument is read, and split larger runs on disk.
            let rows = run.file.rows();
            if rows <= (2 * FRAME_ROWS) as u64
                && let Ok(charge) = ctx.retained_charge(rows * 2 * size_of::<(u64, Id)>() as u64)
            {
                let mut args = Vec::with_capacity(rows as usize);
                while let Some((columns, _)) = reader.next_frame()? {
                    args.extend(
                        columns[0]
                            .iter()
                            .zip(&columns[1])
                            .map(|(&slot, &id)| (slot.0, id)),
                    );
                }
                args.sort_by_key(|&(slot, _)| slot);
                self.buffered = Some(Buffered {
                    rows: args.into_iter().peekable(),
                    _charge: charge,
                });
                continue;
            }
            let middle = run.min + (run.max - run.min) / 2;
            let mut low = Writer::new(ctx)?;
            let mut high = Writer::new(ctx)?;
            while let Some((columns, _)) = reader.next_frame()? {
                ctx.check()?;
                for (&slot, &id) in columns[0].iter().zip(&columns[1]) {
                    if slot.0 <= middle {
                        low.push(slot.0, id)?;
                    } else {
                        high.push(slot.0, id)?;
                    }
                }
            }
            drop((reader, run));
            self.push_run(high.finish()?);
            self.push_run(low.finish()?);
        }
        Err(Error::Corrupt(
            "spill subtotal arguments are missing".into(),
        ))
    }
}
