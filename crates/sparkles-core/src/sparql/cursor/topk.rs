//! Streaming ORDER BY with LIMIT: a bounded heap of `offset + limit` rows.
//!
//! The operator reads its input batch by batch and offers each row to the same
//! [`TopK`](crate::sparql::sortkey::TopK) heap as eager execution, in the same order, so
//! both modes end with the same rows in the same order at any batch size. It keeps at
//! most `k + 1` rows of IDs and their sort keys, charged to the query budget, instead of
//! the whole input. Output starts once the input is exhausted.
use super::{Buffer, CursorOptions, Operator, copy_rows};
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::exec::{self, HeapRows};
use crate::sparql::expr::Expr;
use crate::sparql::exprcache::Report;
use crate::sparql::plan::{Kind, Node};
use crate::sparql::sortkey::{Entry, SortKey, TopK as Heap};
use crate::sparql::table::VarId;
use std::sync::Arc;

/// Whether the operator for `node` is a streaming top-k.
pub(super) fn eligible(ctx: &Ctx, node: &Node) -> bool {
    matches!(&node.kind, Kind::OrderBy { keys, limit: Some(k) }
        if *k > 0 && !keys.iter().any(|(key, _)| key.has_exists()))
        && node.children.len() == 1
        && exec::heap_eligible(ctx)
}

/// The kept rows' IDs, one slot of `width` IDs per row, with a spare slot for the row
/// being offered when the heap is full.
struct Slots {
    vars: Vec<VarId>,
    ids: Vec<Id>,
    free: Vec<u32>,
    /// The batch being offered and its column for each of `vars`.
    batch: Vec<Option<usize>>,
    key_bytes: u64,
}

impl Slots {
    fn width(&self) -> usize {
        self.vars.len()
    }

    fn bytes(&self, entries: usize, keys: usize) -> u64 {
        (self.ids.capacity() as u64 * 8)
            .saturating_add(self.free.capacity() as u64 * 4)
            .saturating_add(self.key_bytes)
            .saturating_add(
                entries as u64
                    * (std::mem::size_of::<Entry<u32>>() as u64
                        + keys as u64 * std::mem::size_of::<SortKey>() as u64),
            )
    }
}

struct Offer<'a> {
    slots: &'a mut Slots,
    table: &'a crate::sparql::table::Table,
}

impl HeapRows<u32> for Offer<'_> {
    fn keep(&mut self, i: usize, keys: &[SortKey]) -> Result<u32> {
        let w = self.slots.width();
        let slot = match self.slots.free.pop() {
            Some(slot) => slot,
            None => {
                let slot = (self.slots.ids.len() / w.max(1)) as u32;
                self.slots.ids.resize(self.slots.ids.len() + w, Id::UNDEF);
                slot
            }
        };
        let at = slot as usize * w;
        for (j, col) in self.slots.batch.iter().enumerate() {
            self.slots.ids[at + j] = col.map_or(Id::UNDEF, |c| self.table.cols[c][i]);
        }
        self.slots.key_bytes = self
            .slots
            .key_bytes
            .saturating_add(keys.iter().map(SortKey::payload_bytes).sum::<u64>());
        Ok(slot)
    }

    fn release(&mut self, entry: Entry<u32>) -> Result<()> {
        self.slots.key_bytes = self
            .slots
            .key_bytes
            .saturating_sub(entry.keys.iter().map(SortKey::payload_bytes).sum::<u64>());
        self.slots.free.push(entry.payload);
        Ok(())
    }
}

pub(super) struct TopK {
    keys: Vec<(Expr, bool)>,
    heap: Option<Heap<u32>>,
    slots: Slots,
    charge: OwnedCharge,
    loaded: Option<Buffer>,
    at: usize,
}

impl TopK {
    pub(super) fn new(ctx: &Arc<Ctx>, node: &Node, vars: &[VarId]) -> Result<Self> {
        let Kind::OrderBy {
            keys,
            limit: Some(k),
        } = &node.kind
        else {
            unreachable!("a top-k node")
        };
        Ok(Self {
            heap: Some(Heap::new(*k, keys.iter().map(|(_, asc)| *asc).collect())),
            keys: keys.clone(),
            slots: Slots {
                vars: vars.to_vec(),
                ids: Vec::new(),
                free: Vec::new(),
                batch: Vec::new(),
                key_bytes: 0,
            },
            charge: OwnedCharge::new(ctx, 1024)?,
            loaded: None,
            at: 0,
        })
    }

    pub(super) fn done(&self) -> bool {
        self.loaded.as_ref().is_some_and(|b| self.at == b.table.len)
    }

    fn load(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
    ) -> Result<()> {
        let Self {
            keys,
            heap,
            slots,
            charge,
            ..
        } = self;
        let mut report = Report::default();
        let width = slots.width();
        // Input demand is independent of the requested output prefix.
        while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
            let heap = heap.as_mut().expect("an unloaded top-k");
            let table = &batch.table;
            slots.batch.clear();
            slots
                .batch
                .extend(slots.vars.iter().map(|v| table.col_of(*v)));
            // Reserve the slots this batch may add (k + 1 at most, counting the spare
            // one) before offering its rows, then settle on the keys they kept.
            let used = slots.ids.len() / width.max(1);
            let room = (heap.k().saturating_add(1).saturating_sub(used)).min(table.len);
            slots.ids.reserve(room * width);
            charge.resize(1024 + slots.bytes(heap.kept() + room, keys.len()))?;
            let mut rows = Offer {
                slots: &mut *slots,
                table,
            };
            exec::offer_rows(ctx, heap, table, keys, &mut report, &mut rows)?;
            charge.resize(1024 + slots.bytes(heap.kept(), keys.len()))?;
        }
        ctx.check()?;
        let entries = heap.take().expect("an unloaded top-k").into_sorted();
        let mut out = Buffer::new(ctx, &slots.vars, entries.len())?;
        for (j, col) in out.table.cols.iter_mut().enumerate() {
            col.extend(
                entries
                    .iter()
                    .map(|e| slots.ids[e.payload as usize * width + j]),
            );
        }
        out.table.len = entries.len();
        out.reconcile()?;
        drop(entries);
        *slots = Slots {
            vars: Vec::new(),
            ids: Vec::new(),
            free: Vec::new(),
            batch: Vec::new(),
            key_bytes: 0,
        };
        charge.resize(0)?;
        self.loaded = Some(out);
        Ok(())
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if self.loaded.is_none() {
            self.load(ctx, child, options)?;
        }
        let table = &self.loaded.as_ref().expect("loaded top-k").table;
        let batch = copy_rows(ctx, table, self.at, cap)?;
        self.at += batch.as_ref().map_or(0, |b| b.table.len);
        Ok(batch)
    }
}
