//! Streaming ORDER BY with LIMIT: a bounded heap of `offset + limit` rows.
//!
//! The operator reads its input batch by batch and offers each row to the same
//! [`TopK`](crate::sparql::sortkey::TopK) heap as eager execution, in the same order, so
//! both modes end with the same rows in the same order at any batch size. It keeps at
//! most `k + 1` rows of IDs and their sort keys, charged to the query budget, instead of
//! the whole input. Output starts once the input is exhausted.
use super::{Buffer, CursorOptions, CursorPlan, FallbackPolicy, Operator, copy_rows};
use crate::error::Result;
use crate::id::Id;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::exec::{self, HeapRows};
use crate::sparql::expr::Expr;
use crate::sparql::exprcache::Report;
use crate::sparql::plan::{Kind, Node};
use crate::sparql::sortkey::{Entry, SortKey, TopK as Heap};
use crate::sparql::table::{Table, VarId};
use std::sync::Arc;

/// The largest input batch the operator asks for when its reads grow.
const MAX_INPUT_ROWS: usize = 1 << 16;

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

    fn bytes(&self, entries: usize, key_slots: u64) -> u64 {
        (self.ids.capacity() as u64 * 8)
            .saturating_add(self.free.capacity() as u64 * 4)
            .saturating_add(self.key_bytes)
            .saturating_add(entries as u64 * (std::mem::size_of::<Entry<u32>>() as u64 + key_slots))
    }
}

struct Offer<'a> {
    slots: &'a mut Slots,
    table: &'a Table,
    charge: &'a mut OwnedCharge,
    key_slots: u64,
    step: u64,
    base_entries: usize,
    remaining_entries: usize,
}

impl Offer<'_> {
    fn base_bytes(&self) -> u64 {
        1024 + self.slots.bytes(self.base_entries, self.key_slots)
    }
}

impl HeapRows<u32> for Offer<'_> {
    fn reserve_key_slots(&mut self, bytes: u64) -> Result<()> {
        // Once the heap is full, the candidate's vector lives alongside all kept
        // vectors until the offer returns one to drop.
        if self.remaining_entries == 0 {
            let need = self.base_bytes().saturating_add(bytes);
            if need > self.charge.bytes() {
                self.charge.resize(need)?;
            }
        }
        Ok(())
    }

    fn keep(&mut self, i: usize, keys: &[SortKey]) -> Result<u32> {
        let added = keys.iter().map(SortKey::payload_bytes).sum::<u64>();
        // The single-key path calls keep before allocating its vector. For a
        // multi-key offer, the same reservation already covers the vector.
        let key_slots = if self.remaining_entries == 0 {
            self.key_slots
        } else {
            0
        };
        let need = self
            .base_bytes()
            .saturating_add(key_slots)
            .saturating_add(added);
        self.charge.cover(need, self.step.min(need / 4))?;
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
        self.slots.key_bytes = self.slots.key_bytes.saturating_add(added);
        self.remaining_entries = self.remaining_entries.saturating_sub(1);
        Ok(slot)
    }

    fn release(&mut self, entry: Entry<u32>) -> Result<()> {
        let freed = entry.keys.iter().map(SortKey::payload_bytes).sum::<u64>();
        let slot = entry.payload;
        // Drop evicted sort keys before shrinking key_bytes / credit.
        drop(entry);
        self.slots.key_bytes = self.slots.key_bytes.saturating_sub(freed);
        self.slots.free.push(slot);
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

    fn offer_batch(&mut self, ctx: &Ctx, table: &Table, report: &mut Report) -> Result<()> {
        let heap = self.heap.as_mut().expect("an unloaded top-k");
        let slots = &mut self.slots;
        let width = slots.width();
        slots.batch.clear();
        slots
            .batch
            .extend(slots.vars.iter().map(|v| table.col_of(*v)));
        // Reserve row IDs including the spare slot, and key storage for the
        // entries the heap can fill. A full heap needs a separate key vector
        // only for a row that passes screening.
        let used = slots.ids.len() / width.max(1);
        let room = (heap.k().saturating_add(1).saturating_sub(used)).min(table.len);
        slots.ids.reserve(room * width);
        let remaining_entries = (heap.k() - heap.kept()).min(table.len);
        let base_entries = heap.kept() + remaining_entries;
        let key_slots = self.keys.len() as u64 * std::mem::size_of::<SortKey>() as u64;
        self.charge
            .resize(1024 + slots.bytes(base_entries, key_slots))?;
        let mut rows = Offer {
            slots,
            table,
            charge: &mut self.charge,
            key_slots,
            step: ctx.reservation_step(),
            base_entries,
            remaining_entries,
        };
        exec::offer_rows(ctx, heap, table, &self.keys, report, &mut rows)?;
        self.charge
            .resize(1024 + slots.bytes(heap.kept(), key_slots))
    }

    fn load(
        &mut self,
        ctx: &Arc<Ctx>,
        child: &mut Operator,
        options: &CursorOptions,
    ) -> Result<()> {
        let mut report = Report::default();
        // Input demand is independent of the requested output prefix. With `grow_input`
        // it doubles with each read up to MAX_INPUT_ROWS, as long as the rows fit the
        // budget with room to spare, so that the first key is evaluated and screened over
        // larger batches.
        let mut demand = options.batch_rows;
        let mut input_options = options.clone();
        loop {
            input_options.batch_rows = demand;
            // A grown read also raises the byte cap, which would otherwise hold it to the
            // caller's batch. A read of the caller's size keeps the caller's byte cap.
            if demand > options.batch_rows {
                input_options.batch_bytes = options
                    .batch_bytes
                    .max(demand.saturating_mul(child.vars.len().max(1) * 8));
            }
            let Some(batch) = child.next(ctx, &input_options, demand)? else {
                break;
            };
            let next = demand.saturating_mul(2).min(MAX_INPUT_ROWS);
            if ctx.opt.grow_input
                && next > demand
                && ctx.rows_within_budget(child.vars.len() + self.keys.len() * 4) / 4 >= next
            {
                demand = next;
            }
            self.offer_batch(ctx, &batch.table, &mut report)?;
        }
        ctx.check()?;
        let entries = self.heap.take().expect("an unloaded top-k").into_sorted();
        let slots = &mut self.slots;
        let width = slots.width();
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
        self.charge.resize(0)?;
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

/// An ordered top-k over a scan (`IndexTopK`). It reads the pieces of the scan whose ids
/// are in value order from their best end, and the other pieces a morsel at a time into a
/// heap of its best k rows ([`exec::ordered_topk`]), so it holds one morsel and k rows.
/// When a value of those pieces is not totally ordered, the generic plan, a streaming
/// top-k over the scan, answers instead, as it does eagerly. The operator builds that
/// plan as its child only then.
pub(super) struct IndexTopK {
    node: Node,
    fallback: FallbackPolicy,
    loaded: Option<Buffer>,
    at: usize,
    /// the described generic plan, once it has been built
    generic: Option<Box<CursorPlan>>,
}

impl IndexTopK {
    pub(super) fn new(node: Node, fallback: FallbackPolicy) -> Self {
        Self {
            node,
            fallback,
            loaded: None,
            at: 0,
            generic: None,
        }
    }

    pub(super) fn generic_plan(&self) -> Option<&CursorPlan> {
        self.generic.as_deref()
    }

    pub(super) fn release(&mut self) {
        self.loaded = None;
    }

    pub(super) fn done(&self, children: &[Operator]) -> bool {
        if self.generic.is_some() {
            return children[0].done;
        }
        self.loaded.as_ref().is_some_and(|b| self.at == b.table.len)
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut Vec<Operator>,
        options: &CursorOptions,
        info: &mut crate::sparql::PlanInfo,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if self.loaded.is_none() && self.generic.is_none() {
            let Kind::OrderedTopK(spec) = &self.node.kind else {
                unreachable!("an ordered top-k node")
            };
            match exec::ordered_topk_morsels_charged(ctx, spec, &self.node.vars)? {
                Some((table, charge, read)) => {
                    info.description = format!("{} [read {read} rows]", info.description);
                    self.loaded = Some(Buffer { table, charge });
                }
                None => {
                    let child =
                        Operator::build(ctx, (*spec.fallback).clone(), self.fallback.at_pull(ctx))?;
                    self.generic = Some(Box::new(child.describe(ctx)));
                    children.push(child);
                }
            }
        }
        if self.generic.is_some() {
            return children[0].next(ctx, options, cap);
        }
        let table = &self.loaded.as_ref().expect("loaded ordered top-k").table;
        let batch = copy_rows(ctx, table, self.at, cap)?;
        self.at += batch.as_ref().map_or(0, |b| b.table.len);
        Ok(batch)
    }
}

/// Whether `node` is a vector ORDER BY search with a generic scan fallback.
pub(super) fn vector_order_eligible(node: &Node) -> bool {
    matches!(&node.kind, Kind::VectorSearch(spec) if spec.order_fallback)
        && node.children.len() == 1
}

/// A vector search below an `ORDER BY DESC(spk:cosine/dot) LIMIT k` (item 829).
///
/// On first pull it asks the vector index for the `k` best rows. When at least `k` rows
/// have a score, the generic scan child is marked skipped and the charged top-`k` answer
/// is served batch by batch. Otherwise the generic scan child streams its rows instead,
/// carrying the same plan note as eager execution.
pub(super) struct VectorOrder {
    node: Node,
    opened: bool,
    fallback: bool,
    loaded: Option<Buffer>,
    at: usize,
    pub(super) counters: Option<serde_json::Map<String, serde_json::Value>>,
    note: Option<String>,
}

impl VectorOrder {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            opened: false,
            fallback: false,
            loaded: None,
            at: 0,
            counters: None,
            note: None,
        }
    }

    pub(super) fn release(&mut self) {
        self.opened = true;
        self.loaded = None;
    }

    pub(super) fn description(&self) -> Option<String> {
        self.note
            .as_ref()
            .map(|note| format!("{} {note}", self.node.desc))
    }

    pub(super) fn done(&self, children: &[Operator]) -> bool {
        if !self.opened {
            return false;
        }
        if self.fallback {
            children[0].done
        } else {
            self.loaded.is_none()
        }
    }

    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        children: &mut [Operator],
        options: &CursorOptions,
        vars: &[VarId],
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if !self.opened {
            self.opened = true;
            let Kind::VectorSearch(spec) = &self.node.kind else {
                unreachable!("a vector order node")
            };
            let searched = match exec::vector_search_order_charged(ctx, spec, &self.node.vars, vars)
            {
                Ok(Some((table, charge, counters))) => Some((table, charge, counters)),
                Ok(None) => None,
                Err(crate::error::Error::BudgetExceeded(_) | crate::error::Error::Invalid(_)) => {
                    None
                }
                Err(e) => return Err(e),
            };
            match searched {
                Some((table, charge, counters)) => {
                    children[0].mark_skipped("the vector index answered");
                    self.counters = Some(counters);
                    if !table.is_empty() {
                        self.loaded = Some(Buffer { table, charge });
                    }
                }
                None => {
                    self.fallback = true;
                    self.note = Some(format!(
                        "[fewer than {} rows have a score: ran the generic plan]",
                        spec.k
                    ));
                }
            }
        }
        if self.fallback {
            let Some(mut batch) = children[0].next(ctx, options, cap)? else {
                return Ok(None);
            };
            if batch.table.vars != vars {
                batch.project(vars)?;
            }
            return Ok(Some(batch));
        }
        let Some(loaded) = &self.loaded else {
            return Ok(None);
        };
        if self.at == 0 && loaded.table.len <= cap {
            return Ok(self.loaded.take());
        }
        let batch = copy_rows(ctx, &loaded.table, self.at, cap)?;
        self.at += batch.as_ref().map_or(0, |b| b.table.len);
        if self.at >= loaded.table.len {
            self.loaded = None;
        }
        Ok(batch)
    }
}

#[cfg(test)]
#[test]
fn topk_cursor_admits_key_slots_once() {
    use crate::sparql::value::Value;
    use std::mem::size_of;

    let store = crate::store::Store::in_memory(Default::default());
    let mut ctx = Ctx::new(store.snapshot());
    let var = ctx.var("v");
    let mut table = Table::new(vec![var]);
    table.push_row(&[Id::from_i64(32).unwrap()]);
    let keys = vec![(Expr::Var(var), true); 16];
    let mut slots = Slots {
        vars: vec![var],
        ids: Vec::with_capacity(1),
        free: Vec::new(),
        batch: vec![Some(0)],
        key_bytes: 0,
    };
    let key_slots = keys.len() as u64 * size_of::<SortKey>() as u64;
    let reserved = 1024 + slots.bytes(1, key_slots);
    // The first key's one-row sort column and its retained value column.
    let first_keys = size_of::<SortKey>() as u64 + size_of::<Option<Value>>() as u64 + 256;
    let map = ctx.nvars() as u64 * 16 + 128;
    ctx.mem_limit = reserved + first_keys + map;
    ctx.enable_cursor_accounting().unwrap();
    let ctx = ctx.into_shared();
    let mut charge = OwnedCharge::new(&ctx, reserved).unwrap();
    let mut heap = Heap::new(1, vec![true; keys.len()]);
    let mut rows = Offer {
        slots: &mut slots,
        table: &table,
        charge: &mut charge,
        key_slots,
        step: ctx.reservation_step(),
        base_entries: 1,
        remaining_entries: 1,
    };

    assert_eq!(
        exec::offer_rows(
            &ctx,
            &mut heap,
            &table,
            &keys,
            &mut Report::default(),
            &mut rows
        )
        .unwrap(),
        1,
    );
    assert_eq!(ctx.mem_peak(), ctx.mem_limit);
    assert_eq!(ctx.live_bytes(), reserved);
    assert_eq!(heap.kept(), 1);
    drop(heap);
    drop(slots);
    drop(charge);
    assert_eq!(ctx.live_bytes(), 0);
}

#[cfg(test)]
#[test]
fn topk_cursor_admits_recycled_key_slots_after_screening() {
    use crate::error::{BudgetKind, Error};
    use crate::sparql::opt_tests::load;
    use crate::sparql::value::Value;
    use oxrdf::NamedNode;
    use std::mem::size_of;

    let store = crate::store::Store::in_memory(Default::default());
    load(
        &store,
        "<urn:a> <urn:p> <urn:b> .",
        crate::io::RdfFormat::Turtle,
    );
    store.compact().unwrap();
    let snapshot = store.snapshot();
    let [a, b] = ["urn:a", "urn:b"].map(|iri| {
        snapshot
            .lookup_term(&NamedNode::new_unchecked(iri).into())
            .unwrap()
    });
    let rank = Id::from_i64(2).unwrap();
    let better = Id::from_i64(1).unwrap();
    let key_size = size_of::<SortKey>() as u64;

    // Use three keys for the surviving candidate so its vector needs more
    // credit than the first-key evaluator releases before the offer.
    for (input, key_count, candidate_credit, kept_slot) in [
        ([rank, b], 2, 0, Some(1)),
        ([better, Id::UNDEF], 3, 3 * key_size - 1, None),
        ([better, Id::UNDEF], 3, 3 * key_size, Some(0)),
    ] {
        let mut ctx = Ctx::new(snapshot.clone());
        let vars = [ctx.var("rank"), ctx.var("iri")];
        let mut keys = vec![(Expr::Var(vars[0]), true); key_count];
        keys[1] = (Expr::Var(vars[1]), true);
        let key_slots = key_count as u64 * key_size;
        let mut heap = Heap::new(1, vec![true; key_count]);
        let first = SortKey::new(ctx.value(rank));
        let iri = SortKey::new(Some(Value::Iri("urn:a".into()))).with_base_iri(a.payload());
        let key_bytes = iri.payload_bytes();
        let mut kept = vec![first; key_count];
        kept[1] = iri;
        assert!(heap.offer(kept, 1).is_none());
        // Model a full heap after its first replacement freed ID slot 0.
        let slots = Slots {
            vars: vars.to_vec(),
            ids: vec![rank, b, rank, a],
            free: vec![0],
            batch: Vec::new(),
            key_bytes,
        };
        let reserved = 1024 + slots.bytes(1, key_slots);
        let first_keys = key_size + size_of::<Option<Value>>() as u64 + 256;
        let map = ctx.nvars() as u64 * 16 + 128;
        ctx.mem_limit = reserved + first_keys + map.max(candidate_credit);
        ctx.enable_cursor_accounting().unwrap();
        let ctx = ctx.into_shared();
        let mut topk = TopK {
            keys,
            heap: Some(heap),
            slots,
            charge: OwnedCharge::new(&ctx, reserved).unwrap(),
            loaded: None,
            at: 0,
        };
        let mut table = Table::new(vars.to_vec());
        table.push_row(&input);
        let result = topk.offer_batch(&ctx, &table, &mut Report::default());
        if let Some(slot) = kept_slot {
            result.unwrap();
            assert_eq!(ctx.mem_peak(), ctx.mem_limit);
            let retained = if slot == 0 {
                reserved - key_bytes
            } else {
                reserved
            };
            assert_eq!(ctx.live_bytes(), retained);
            let heap = topk.heap.take().unwrap();
            assert_eq!(heap.into_sorted()[0].payload, slot);
        } else {
            assert!(matches!(result, Err(Error::BudgetExceeded(b))
                    if b.kind == BudgetKind::Memory && b.requested == b.limit + 1));
            assert_eq!(ctx.live_bytes(), reserved);
        }
        drop(topk);
        assert_eq!(ctx.live_bytes(), 0);
    }
}
