//! Incremental view grouping over the shared `Accumulator` trait: per group, positive
//! change rows fold through `Accumulator::add_batch` and negative change rows retract
//! through `Accumulator::retract` with signed weights, and each changed group's old
//! output row is retracted and its new one emitted.
//!
//! Integer and decimal sums are kept exactly in `acc::RetractableSum`, so removing a
//! value is exact. Float and double values are kept as value counts, and a changed group
//! sums them again. `DISTINCT` aggregates wrap their inner accumulator in
//! `acc::RetractableDistinct`, and `MIN`, `MAX`, `GROUP_CONCAT` and `SAMPLE` run
//! through the ordered-multiset `acc::Ordered` accumulator. All aggregate error
//! readings share the same finalizers as one-off SPARQL grouping.

use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::prov::{self, AChanges, Ann, AnnMap, Provenance};
use super::super::zset::{Changes, Row, consolidate};
use super::Node;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::expr::Expr;
use crate::sparql::group::acc::{self, Accumulator, Arg, Batch};
use crate::sparql::group::op::BatchBudget;
use crate::sparql::plan::Agg;
use crate::sparql::table::Table;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use spargebra::algebra::{AggregateFunction, Expression};
use std::fmt;

type HbMap<K, V> = hashbrown::HashMap<K, V, FxBuildHasher>;

/// Checkpoint encoding version of [`Group`]'s state. Unknown versions are refused with
/// [`Error::Unsupported`].
pub(crate) const GROUP_STATE_VERSION: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggKind {
    /// `COUNT(*)`
    CountAll,
    Count,
    Sum,
    Avg,
    Min,
    Max,
    /// `GROUP_CONCAT`
    Concat,
    Sample,
}

#[derive(Clone, Debug)]
pub struct AggSpec {
    pub kind: AggKind,
    pub distinct: bool,
    pub expr: Option<Expression>,
    /// the expression is this input column (`Some(None)`: a variable the input lacks)
    pub column: Option<Option<usize>>,
    /// the separator of `GROUP_CONCAT` (`None`: a space)
    pub separator: Option<String>,
}

impl AggSpec {
    fn to_agg(&self) -> Agg {
        let func = match self.kind {
            AggKind::CountAll | AggKind::Count => AggregateFunction::Count,
            AggKind::Sum => AggregateFunction::Sum,
            AggKind::Avg => AggregateFunction::Avg,
            AggKind::Min => AggregateFunction::Min,
            AggKind::Max => AggregateFunction::Max,
            AggKind::Concat => AggregateFunction::GroupConcat {
                separator: self.separator.clone(),
            },
            AggKind::Sample => AggregateFunction::Sample,
        };
        Agg {
            func,
            expr: (self.kind != AggKind::CountAll).then_some(Expr::Var(0)),
            distinct: self.distinct,
            fold: None,
            registered: None,
        }
    }
}

#[derive(Clone, Debug)]
struct GroupMeta {
    key: Row,
    rows: i64,
    out: Option<Row>,
}

pub struct Group {
    pub inner: Node,
    /// the position of each key in the input (`None`: never bound)
    pub keys: Vec<Option<usize>>,
    pub aggs: Vec<AggSpec>,
    /// no `GROUP BY`: one group, present even without rows
    pub implicit: bool,
    /// key -> dense slot in `slots` and every accumulator in `accs`
    index: HbMap<Row, u32>,
    slots: Vec<GroupMeta>,
    /// one retractable accumulator per aggregate, sharing slots `0..slots.len()`
    accs: Vec<Box<dyn Accumulator>>,
    /// each group's annotation, the sum of its members', when the circuit keeps them
    /// (spec F13 §5.2)
    gann: Option<AnnMap>,
    /// the number of the condition of an implicit group without rows
    pub cond: u32,
    prov: Provenance,
}

impl Clone for Group {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            keys: self.keys.clone(),
            aggs: self.aggs.clone(),
            implicit: self.implicit,
            index: self.index.clone(),
            slots: self.slots.clone(),
            accs: self
                .accs
                .iter()
                .map(|a| a.clone_box().expect("view accumulators are cloneable"))
                .collect(),
            gann: self.gann.clone(),
            cond: self.cond,
            prov: self.prov,
        }
    }
}

impl fmt::Debug for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Group")
            .field("inner", &self.inner)
            .field("keys", &self.keys)
            .field("aggs", &self.aggs)
            .field("implicit", &self.implicit)
            .field("groups", &self.slots.len())
            .field("cond", &self.cond)
            .field("prov", &self.prov)
            .finish()
    }
}

impl Group {
    pub fn new(inner: Node, keys: Vec<Option<usize>>, aggs: Vec<AggSpec>, implicit: bool) -> Group {
        let row_width = inner.schema.len();
        let accs = aggs
            .iter()
            .map(|spec| {
                acc::retractable(&spec.to_agg(), row_width)
                    .expect("compiled view aggregates have retractable accumulators")
            })
            .collect();
        Group {
            inner,
            keys,
            aggs,
            implicit,
            index: HbMap::default(),
            slots: Vec::new(),
            accs,
            gann: None,
            cond: u32::MAX,
            prov: Provenance::None,
        }
    }

    pub fn enable_prov(&mut self, mode: Provenance, cond: u32) {
        self.prov = mode;
        self.cond = cond;
        self.gann = (!mode.is_none()).then(AnnMap::default);
    }

    fn key_of(&self, r: &[Id]) -> Row {
        self.keys
            .iter()
            .map(|k| k.map_or(Id::UNDEF, |k| r[k]))
            .collect()
    }

    fn clear_state(&mut self) {
        self.index = HbMap::default();
        self.slots = Vec::new();
        for acc in &mut self.accs {
            acc.clear();
        }
        if let Some(m) = self.gann.as_mut() {
            *m = AnnMap::default();
        }
    }

    fn shrink_unused(&mut self) {
        const SHRINK_MIN_CAP: usize = 64;
        if self.slots.is_empty() {
            self.slots = Vec::new();
            self.index = HbMap::default();
            for acc in &mut self.accs {
                acc.clear();
            }
            if let Some(m) = self.gann.as_mut() {
                m.shrink_unused();
            }
            return;
        }
        if self.slots.capacity() >= SHRINK_MIN_CAP && self.slots.len() * 4 <= self.slots.capacity()
        {
            self.slots.shrink_to(self.slots.len() * 2);
        }
        let index_entry_bytes = std::mem::size_of::<(Row, u32)>() + 1;
        let index_alloc = self.index.allocation_size();
        if index_alloc >= SHRINK_MIN_CAP * index_entry_bytes
            && self
                .index
                .len()
                .saturating_mul(index_entry_bytes)
                .saturating_mul(4)
                <= index_alloc
        {
            self.index
                .shrink_to(self.index.len() + self.index.len() / 2);
        }
        for acc in &mut self.accs {
            acc.shrink_unused();
        }
        if let Some(m) = self.gann.as_mut() {
            m.shrink_unused();
        }
    }

    #[cfg(test)]
    pub(crate) fn backing_allocations(&self) -> (usize, usize) {
        (
            self.slots.capacity() * std::mem::size_of::<GroupMeta>(),
            self.index.allocation_size(),
        )
    }

    #[cfg(test)]
    pub(crate) fn gann_allocation_size(&self) -> usize {
        self.gann.as_ref().map_or(0, |m| m.allocation_size())
    }

    fn push_group(&mut self, key: Row) -> u32 {
        let slot = self.slots.len() as u32;
        self.index.insert(key.clone(), slot);
        self.slots.push(GroupMeta {
            key,
            rows: 0,
            out: None,
        });
        for acc in &mut self.accs {
            acc.init(1);
        }
        slot
    }

    fn needs_star_table(&self) -> bool {
        self.aggs
            .iter()
            .any(|a| a.kind == AggKind::CountAll && a.distinct)
    }

    fn build_table(&self, c: &Changes, indices: &[usize]) -> Table {
        if !self.needs_star_table() {
            return Table::new(Vec::new());
        }
        let width = self.inner.schema.len();
        let mut table = Table::new((0..width as u32).collect());
        for col in &mut table.cols {
            col.reserve_exact(indices.len());
        }
        for &i in indices {
            let r = &c[i].0;
            for (col, &id) in table.cols.iter_mut().zip(r.iter()) {
                col.push(id);
            }
        }
        table.len = indices.len();
        table
    }

    /// The annotation of group `key`'s row: its members' sum, or, for an implicit
    /// group without rows, the condition that the pattern has no match.
    fn row_ann(&self, p: &mut Pass, key: &Row) -> Ann {
        let mode = p.prov;
        match self.index.get(key).map(|&s| &self.slots[s as usize]) {
            Some(g) if g.rows == 0 && self.implicit => p.absent_ann(self.cond, &[]),
            Some(_) => self
                .gann
                .as_ref()
                .map_or_else(|| Ann::zero(mode), |m| prov::get(m, key, mode)),
            None => Ann::zero(mode),
        }
    }

    /// [`apply`](Self::apply) with annotations. The weights move the groups as
    /// without them, and the annotations of the rows that the groups retract, emit or
    /// keep follow the groups' annotations before and after.
    fn apply_a(&mut self, p: &mut Pass, c: AChanges) -> Result<AChanges> {
        let mode = p.prov;
        let mut before: FxHashMap<Row, Ann> = FxHashMap::default();
        for (r, _, _) in &c {
            let k = self.key_of(r);
            if let std::collections::hash_map::Entry::Vacant(e) = before.entry(k) {
                let a = self.row_ann(p, e.key());
                e.insert(a);
            }
        }
        let plain = consolidate(c.iter().map(|(r, w, _)| (r.clone(), *w)).collect());
        let mut gann = self.gann.take().unwrap_or_default();
        for (r, _, a) in c {
            let k = self.key_of(&r);
            prov::absorb_row_state(&mut gann, &k, a, mode);
        }
        self.gann = Some(gann);
        let moved = if plain.is_empty() {
            Changes::new()
        } else {
            self.apply(p, plain)?
        };
        let n = self.keys.len();
        let mut out = AChanges::new();
        let mut emitted: FxHashSet<Row> = Default::default();
        for (row, w) in moved {
            let k: Row = row[..n].into();
            let a = if w < 0 {
                before
                    .get(&k)
                    .cloned()
                    .unwrap_or_else(|| Ann::zero(mode))
                    .scaled(w)
            } else {
                self.row_ann(p, &k).scaled(w)
            };
            emitted.insert(k);
            out.push((row, w, a));
        }
        for (k, old) in before {
            if emitted.contains(&k) {
                continue;
            }
            let Some(row) = self
                .index
                .get(&k)
                .and_then(|&s| self.slots[s as usize].out.clone())
            else {
                continue;
            };
            let d = self.row_ann(p, &k).sub(&old);
            if !d.is_zero() {
                out.push((row, 0, d));
            }
        }
        if let Some(m) = self.gann.as_mut() {
            m.retain(|k, _| self.index.contains_key(k));
            m.shrink_unused();
        }
        Ok(out)
    }

    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        self.clear_state();
        if let Some(m) = self.gann.as_mut() {
            *m = AnnMap::default();
        }
        let c = prov::consolidate(self.inner.init_a(p)?);
        let mut out = self.apply_a(p, c)?;
        if self.implicit && self.slots.is_empty() {
            let slot = self.push_group(Row::default()) as usize;
            let row: Row = self.read_slot(p, slot)?.into();
            self.slots[slot].out = Some(row.clone());
            let a = self.row_ann(p, &Row::default());
            out.push((row, 1, a));
        }
        Ok(out)
    }

    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let c = prov::consolidate(self.inner.delta_a(p)?);
        if c.is_empty() {
            return Ok(c);
        }
        self.apply_a(p, c)
    }

    pub fn describe(&self, vars: &[String]) -> String {
        let keys: Vec<String> = self
            .keys
            .iter()
            .map(|k| match k {
                Some(i) => format!("?{}", vars[self.inner.schema[*i] as usize]),
                None => "UNDEF".into(),
            })
            .collect();
        let aggs: Vec<String> = self
            .aggs
            .iter()
            .map(|a| {
                let f = match a.kind {
                    AggKind::CountAll | AggKind::Count => "COUNT",
                    AggKind::Sum => "SUM",
                    AggKind::Avg => "AVG",
                    AggKind::Min => "MIN",
                    AggKind::Max => "MAX",
                    AggKind::Concat => "GROUP_CONCAT",
                    AggKind::Sample => "SAMPLE",
                };
                let d = if a.distinct { "DISTINCT " } else { "" };
                let sep = match &a.separator {
                    Some(s) => format!("; SEPARATOR = {:?}", s),
                    None => String::new(),
                };
                match &a.expr {
                    Some(e) => format!("{f}({d}{e}{sep})"),
                    None => format!("{f}({d}*)"),
                }
            })
            .collect();
        format!("by [{}] {}", keys.join(" "), aggs.join(" "))
    }

    fn read_slot(&mut self, p: &mut Pass, slot: usize) -> Result<Vec<Id>> {
        let accs = &mut self.accs;
        p.with_agg_ctx(|ctx| {
            let mut out = Vec::with_capacity(accs.len());
            for acc in accs.iter_mut() {
                out.push(acc.read(ctx, slot)?);
            }
            ctx.check()?;
            Ok(out)
        })
    }

    fn apply(&mut self, p: &mut Pass, c: Changes) -> Result<Changes> {
        if c.is_empty() {
            return Ok(Changes::new());
        }
        // Evaluate each aggregate's argument for each change row.
        let rows: Vec<&[Id]> = c.iter().map(|(r, _)| &r[..]).collect();
        let mut args: Vec<Option<Vec<Id>>> = Vec::with_capacity(self.aggs.len());
        for a in &self.aggs {
            args.push(match (&a.expr, a.column) {
                (None, _) | (_, Some(None)) => None,
                (_, Some(Some(i))) => Some(rows.iter().map(|r| r[i]).collect()),
                (Some(e), None) => Some(
                    p.eval(&self.inner.schema, &rows, e, false)?
                        .into_iter()
                        .map(|v| v.unwrap_or(Id::UNDEF))
                        .collect(),
                ),
            });
        }
        let mut touched: Vec<Row> = Vec::new();
        let mut seen: FxHashSet<Row> = Default::default();
        let mut tracked = self.bytes();
        let mut pos_idx: Vec<usize> = Vec::new();
        let mut pos_slots: Vec<u32> = Vec::new();
        let mut pos_weights: Vec<i64> = Vec::new();
        let mut neg_idx: Vec<usize> = Vec::new();
        let mut neg_slots: Vec<u32> = Vec::new();
        let mut neg_weights: Vec<i64> = Vec::new();
        for (i, (r, w)) in c.iter().enumerate() {
            if i.is_multiple_of(1024) {
                p.tick()?;
                p.sync_op_state(&mut tracked, self.bytes())?;
            }
            let w = *w;
            if w == 0 {
                continue;
            }
            let key = self.key_of(r);
            let slot = match self.index.get(&key) {
                Some(&slot) => slot,
                None => self.push_group(key.clone()),
            };
            if seen.insert(key.clone()) {
                touched.push(key);
            }
            self.slots[slot as usize].rows += w;
            if w > 0 {
                pos_idx.push(i);
                pos_slots.push(slot);
                pos_weights.push(w);
            } else {
                neg_idx.push(i);
                neg_slots.push(slot);
                neg_weights.push(w);
            }
        }
        p.sync_op_state(&mut tracked, self.bytes())?;
        let pos_table = if pos_idx.is_empty() {
            Table::new(Vec::new())
        } else {
            self.build_table(&c, &pos_idx)
        };
        let neg_table = if neg_idx.is_empty() {
            Table::new(Vec::new())
        } else {
            self.build_table(&c, &neg_idx)
        };
        let pass_budget = p.budget_handle();
        let sync_pass = |tracked: &mut u64, current: u64| -> Result<()> {
            if current > *tracked {
                let delta = current - *tracked;
                if let Some(b) = &pass_budget {
                    b.try_reserve_pass(delta)?;
                }
                *tracked = current;
            } else if current < *tracked {
                let delta = *tracked - current;
                if let Some(b) = &pass_budget {
                    b.release_pass(delta);
                }
                *tracked = current;
            }
            Ok(())
        };
        let mut out = Changes::new();
        let aggs = &self.aggs;
        let accs = &mut self.accs;
        let index = &mut self.index;
        let slots = &mut self.slots;
        let implicit = self.implicit;
        let mut removed_any = false;
        const FOLD_CHUNK: usize = 1024;
        p.with_agg_ctx(|ctx| {
            // Fold positive weights first, then retract negative weights, in bounded
            // chunks with live accumulator retention and per-chunk reconciliation so an
            // update cannot allocate an unbounded delta or hide an add-then-retract peak.
            let mut chunk_col: Vec<Id> = Vec::new();
            for (ai, acc) in accs.iter_mut().enumerate() {
                let other_bytes = tracked.saturating_sub(acc.retained_bytes());
                let mut start = 0;
                while start < pos_slots.len() {
                    ctx.check()?;
                    let end = (start + FOLD_CHUNK).min(pos_slots.len());
                    let arg = match (&aggs[ai].expr, aggs[ai].column) {
                        (None, _) => Arg::Star,
                        (_, Some(None)) => Arg::Unbound,
                        _ => {
                            chunk_col.clear();
                            if let Some(col) = &args[ai] {
                                chunk_col.extend(pos_idx[start..end].iter().map(|&i| col[i]));
                            }
                            Arg::Column(&chunk_col)
                        }
                    };
                    let batch = Batch {
                        table: &pos_table,
                        arg,
                        rows: None,
                        weights: Some(&pos_weights[start..end]),
                        base: start,
                    };
                    let mut retain = |delta: u64| {
                        if delta > 0 {
                            if let Some(b) = &pass_budget {
                                b.try_reserve_pass(delta)?;
                            }
                            tracked = tracked.saturating_add(delta);
                        }
                        Ok(())
                    };
                    let mut budget = BatchBudget::Retain(&mut retain);
                    acc.add_batch(ctx, &batch, &pos_slots[start..end], &mut budget)?;
                    sync_pass(
                        &mut tracked,
                        other_bytes.saturating_add(acc.retained_bytes()),
                    )?;
                    start = end;
                }
                let mut start = 0;
                while start < neg_slots.len() {
                    ctx.check()?;
                    let end = (start + FOLD_CHUNK).min(neg_slots.len());
                    let arg = match (&aggs[ai].expr, aggs[ai].column) {
                        (None, _) => Arg::Star,
                        (_, Some(None)) => Arg::Unbound,
                        _ => {
                            chunk_col.clear();
                            if let Some(col) = &args[ai] {
                                chunk_col.extend(neg_idx[start..end].iter().map(|&i| col[i]));
                            }
                            Arg::Column(&chunk_col)
                        }
                    };
                    let batch = Batch {
                        table: &neg_table,
                        arg,
                        rows: None,
                        weights: Some(&neg_weights[start..end]),
                        base: start,
                    };
                    debug_assert!(acc.retractable());
                    if !acc.retract(ctx, &batch, &neg_slots[start..end])? {
                        return Err(Error::Corrupt(
                            "incremental view accumulator refused retraction".into(),
                        ));
                    }
                    sync_pass(
                        &mut tracked,
                        other_bytes.saturating_add(acc.retained_bytes()),
                    )?;
                    start = end;
                }
            }
            for (ti, key) in touched.into_iter().enumerate() {
                if ti.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let slot = *index.get(&key).expect("touched group exists") as usize;
                let row_count = slots[slot].rows;
                let new = if row_count > 0 || implicit {
                    let mut row = Vec::with_capacity(key.len() + accs.len());
                    row.extend_from_slice(&key);
                    for acc in accs.iter_mut() {
                        row.push(acc.read(ctx, slot)?);
                    }
                    ctx.check()?;
                    Some(row.into_boxed_slice())
                } else {
                    None
                };
                if slots[slot].out != new {
                    if let Some(o) = &slots[slot].out {
                        out.push((o.clone(), -1));
                    }
                    if let Some(n) = &new {
                        out.push((n.clone(), 1));
                    }
                }
                if row_count <= 0 && !implicit {
                    removed_any = true;
                    let removed = slots.swap_remove(slot);
                    index.remove(&removed.key);
                    for acc in accs.iter_mut() {
                        acc.swap_remove(slot);
                    }
                    if slot < slots.len() {
                        let moved_key = slots[slot].key.clone();
                        index.insert(moved_key, slot as u32);
                    }
                } else {
                    slots[slot].out = new;
                }
            }
            Ok(())
        })?;
        if removed_any {
            self.shrink_unused();
        }
        p.sync_op_state(&mut tracked, self.bytes())?;
        Ok(out)
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        self.clear_state();
        let mark = p.output_lease_mark();
        let raw = self.inner.init(p)?;
        let leases = p.take_output_leases_since(mark);
        let c = consolidate(raw);
        let mut out = self.apply(p, c)?;
        drop(leases);
        if self.implicit && self.slots.is_empty() {
            let mut tracked = self.bytes();
            let slot = self.push_group(Row::default()) as usize;
            let row: Row = self.read_slot(p, slot)?.into();
            self.slots[slot].out = Some(row.clone());
            p.sync_op_state(&mut tracked, self.bytes())?;
            out.push((row, 1));
        }
        Ok(out)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let mark = p.output_lease_mark();
        let raw = self.inner.delta(p)?;
        let leases = p.take_output_leases_since(mark);
        let c = consolidate(raw);
        if c.is_empty() {
            drop(leases);
            return Ok(c);
        }
        let out = self.apply(p, c)?;
        drop(leases);
        Ok(out)
    }

    pub fn bytes(&self) -> u64 {
        let key_row_bytes = self.keys.len() as u64 * 8;
        let out_row_bytes = (self.keys.len() + self.aggs.len()) as u64 * 8;
        let live_rows = self.slots.len() as u64 * (key_row_bytes * 2 + out_row_bytes);
        let slots_buf = self.slots.capacity() as u64 * std::mem::size_of::<GroupMeta>() as u64;
        let index_buf = self.index.allocation_size() as u64;
        let accs_bytes: u64 = self.accs.iter().map(|a| a.retained_bytes()).sum();
        live_rows
            + slots_buf
            + index_buf
            + accs_bytes
            + self
                .gann
                .as_ref()
                .map_or(0, |m| prov::map_bytes(m, self.keys.len()))
    }

    pub fn clear(&mut self) {
        self.clear_state();
        if let Some(m) = &mut self.gann {
            *m = AnnMap::default();
        }
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.u64(GROUP_STATE_VERSION);
        w.u64(self.slots.len() as u64);
        for (slot, meta) in self.slots.iter().enumerate() {
            w.row(&meta.key)?;
            w.i64(meta.rows);
            match &meta.out {
                Some(o) => {
                    w.u64(1);
                    w.row(o)?;
                }
                None => w.u64(0),
            }
            for acc in &self.accs {
                acc.save_group(slot, w)?;
            }
        }
        if let Some(m) = &self.gann {
            prov::save_map(w, m)?;
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        let version = r.u64()?;
        if version != GROUP_STATE_VERSION {
            return Err(Error::Unsupported(format!(
                "unsupported group operator checkpoint state version {version}"
            )));
        }
        self.clear_state();
        let key_width = self.keys.len();
        let out_width = self.keys.len() + self.aggs.len();
        let key_row_bytes = key_width as u64 * std::mem::size_of::<Id>() as u64;
        let out_row_bytes = out_width as u64 * std::mem::size_of::<Id>() as u64;
        let live_per_group = key_row_bytes * 2 + out_row_bytes;
        let n = r.u64()?;
        for _ in 0..n {
            let mut row_lease = r.scratch_lease();
            let key = r.row_exact_leased(key_width, &mut row_lease)?;
            if self.index.contains_key(&key) {
                return Err(Error::Corrupt(
                    "a view's state holds a duplicate group key".into(),
                ));
            }
            let rows = r.i64()?;
            let out = match r.u64()? {
                0 => None,
                _ => Some(r.row_exact_leased(out_width, &mut row_lease)?),
            };
            let slots_delta = acc::vec_insert_growth(&self.slots);
            let index_est = acc::hbmap_insert_growth(&self.index, 0);
            let accs_delta: u64 = self.accs.iter().map(|a| a.init_growth(1)).sum();
            let leased = row_lease.bytes();
            let remaining_live = live_per_group.saturating_sub(leased);
            let remaining_est = remaining_live
                .saturating_add(slots_delta)
                .saturating_add(index_est)
                .saturating_add(accs_delta);
            r.charge_state(remaining_est)?;
            r.commit_scratch_to_state(&mut row_lease, leased);
            let index_before = self.index.allocation_size() as u64;
            let slot = self.push_group(key) as usize;
            let index_actual = (self.index.allocation_size() as u64).saturating_sub(index_before);
            if index_est > index_actual {
                r.release_state(index_est - index_actual);
            } else if index_actual > index_est {
                r.charge_state(index_actual - index_est)?;
            }
            self.slots[slot].rows = rows;
            self.slots[slot].out = out;
            for acc in &mut self.accs {
                acc.load_group(slot, r)?;
            }
        }
        let mode = self.prov;
        if let Some(m) = self.gann.as_mut() {
            *m = prov::load_map(r, mode)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incr::compile::compile;
    use crate::incr::ops::Op;
    use crate::incr::pass::{Pass, Vis};
    use crate::incr::terms::Terms;
    use crate::sparql;
    use crate::store::{Store, StoreOptions};
    use crate::task::Control;

    fn find_group_mut(n: &mut Node) -> Option<&mut Group> {
        match &mut n.op {
            Op::Group(g) => Some(g),
            Op::Project(p) => find_group_mut(&mut p.inner),
            Op::Filter(f) => find_group_mut(&mut f.inner),
            Op::Extend(e) => find_group_mut(&mut e.inner),
            Op::Distinct(d) => find_group_mut(&mut d.inner),
            _ => None,
        }
    }

    /// Directly invokes `Group::apply` (bypassing `Node::delta`) to verify both the
    /// operator's own cancellation checks and weighted `COUNT` polling.
    #[test]
    fn group_apply_polls_cancellation_after_entry() {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let parsed = sparql::parse_query(
            "PREFIX : <http://ex/> SELECT ?g (COUNT(*) AS ?c) WHERE { ?x :g ?g } GROUP BY ?g",
            None,
            &[],
        )
        .unwrap();
        let mut plan = compile(&parsed, None).unwrap();
        let group = find_group_mut(&mut plan.root).expect("Group operator");
        let vis = Vis::new(None, &snap);
        let mut terms = Terms::default();

        // 1. Pre-cancelled control aborts inside `Group::apply` itself.
        let ctl = Control::none();
        ctl.cancel.cancel();
        let mut pass = Pass::new(
            snap.clone(),
            None,
            &[],
            &[],
            &mut terms,
            &vis,
            None,
            &ctl,
            &plan.vars,
        );
        let one_row: Changes = vec![(
            vec![Id::from_i64(1).unwrap(); group.inner.schema.len()].into(),
            1,
        )];
        let err = group
            .apply(&mut pass, one_row)
            .expect_err("expected Cancelled inside Group::apply");
        assert!(matches!(err, Error::Cancelled), "got {err:?}");

        // 2. Large weighted COUNT(*) batch polls `ctx.check()` inside weighted add & retract.
        let ok_ctl = Control::none();
        let mut pass = Pass::new(
            snap,
            None,
            &[],
            &[],
            &mut terms,
            &vis,
            None,
            &ok_ctl,
            &plan.vars,
        );
        let pos_rows: Changes = (0..5000i64)
            .map(|i| {
                let mut r = vec![Id::from_i64(i).unwrap(); group.inner.schema.len()];
                if let Some(Some(k)) = group.keys.first() {
                    r[*k] = Id::from_i64(0).unwrap();
                }
                (r.into_boxed_slice(), 1)
            })
            .collect();
        let before_add = crate::sparql::ctx::CHECKS.with(|n| n.get());
        group.apply(&mut pass, pos_rows.clone()).unwrap();
        let add_checks = crate::sparql::ctx::CHECKS.with(|n| n.get()) - before_add;
        assert!(
            add_checks >= 5,
            "expected weighted COUNT(*) add_batch to poll ctx.check(), got {add_checks}"
        );

        let neg_rows: Changes = pos_rows.into_iter().map(|(r, _)| (r, -1)).collect();
        let before_ret = crate::sparql::ctx::CHECKS.with(|n| n.get());
        group.apply(&mut pass, neg_rows).unwrap();
        let ret_checks = crate::sparql::ctx::CHECKS.with(|n| n.get()) - before_ret;
        assert!(
            ret_checks >= 5,
            "expected weighted COUNT(*) retract to poll ctx.check(), got {ret_checks}"
        );
    }

    /// Regression test: a large delta adding many distinct decimals to a single existing
    /// group (including a delta that adds and then retracts those decimals in one `apply`)
    /// must admit accumulator growth during the fold under a tight circuit budget backed
    /// by a parent pool ledger, failing before allocating the full map.
    #[test]
    fn group_apply_admits_accumulator_growth_mid_fold_under_parent_budget() {
        use crate::incr::budget::ViewBudget;
        use crate::sparql::pool::QueryPool;
        use oxrdf::vocab::xsd;
        use oxrdf::{Literal, Term};

        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let parsed = sparql::parse_query(
            "PREFIX : <http://ex/> SELECT ?g (SUM(?v) AS ?s) WHERE { ?x :g ?g ; :v ?v } GROUP BY ?g",
            None,
            &[],
        )
        .unwrap();
        let vis = Vis::new(None, &snap);
        let ctl = Control::none();
        let g_key = Id::from_i64(42).unwrap();

        let make_decimal_id = |terms: &mut Terms, i: i64| -> Id {
            let lit = Literal::new_typed_literal(format!("{i}.1"), xsd::DECIMAL);
            terms.intern(&snap, &Term::Literal(lit))
        };

        // Helper: compile plan and seed one existing group (?g = 42, ?v = 1.1).
        let seed_plan = |terms: &mut Terms| {
            let mut plan = compile(&parsed, None).unwrap();
            let (width, g_col, v_col) = {
                let group = find_group_mut(&mut plan.root).expect("Group operator");
                (
                    group.inner.schema.len(),
                    group.keys[0].expect("group key column"),
                    group.aggs[0].column.flatten().expect("sum arg column"),
                )
            };
            let mut seed_row = vec![Id::from_i64(1).unwrap(); width];
            seed_row[g_col] = g_key;
            seed_row[v_col] = make_decimal_id(terms, 1);
            let mut seed_pass = Pass::new(
                snap.clone(),
                None,
                &[],
                &[],
                terms,
                &vis,
                None,
                &ctl,
                &plan.vars,
            );
            find_group_mut(&mut plan.root)
                .expect("Group operator")
                .apply(&mut seed_pass, vec![(seed_row.into_boxed_slice(), 1)])
                .unwrap();
            plan
        };

        let limit = 16 << 10;

        // 1. Positive-only delta of 4,000 distinct decimals to the single existing group.
        // Without mid-fold accumulator admission, `RetractableSum` allocates the whole >130 KiB
        // hash table before the final `sync_op_state`.
        {
            let mut terms = Terms::default();
            let mut plan = seed_plan(&mut terms);
            let plan_vars = plan.vars.clone();
            let group = find_group_mut(&mut plan.root).expect("Group operator");
            let g_col = group.keys[0].expect("group key column");
            let v_col = group.aggs[0].column.flatten().expect("sum arg column");
            let delta: Changes = (10..4_010i64)
                .map(|i| {
                    let mut r = vec![Id::from_i64(i).unwrap(); group.inner.schema.len()];
                    r[g_col] = g_key;
                    r[v_col] = make_decimal_id(&mut terms, i);
                    (r.into_boxed_slice(), 1)
                })
                .collect();

            let pool = QueryPool::new(limit);
            let budget = ViewBudget::new(Some(limit), Some(pool.work_parent()));
            let initial_bytes = group.bytes();
            budget.try_set_state(initial_bytes).unwrap();

            let mut pass = Pass::new(
                snap.clone(),
                None,
                &[],
                &[],
                &mut terms,
                &vis,
                None,
                &ctl,
                &plan_vars,
            );
            pass.budget = Some(budget.handle());
            pass.pool = Some(pool.clone());
            pass.max_memory_bytes = Some(limit);

            let err = group
                .apply(&mut pass, delta)
                .expect_err("expected tight parent-backed budget refusal mid-fold");
            assert!(
                matches!(
                    err,
                    Error::BudgetExceeded(b)
                        if matches!(b.kind, crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory)
                ),
                "expected Memory/ServerMemory budget error, got {err:?}"
            );
            assert!(
                group.bytes() <= limit,
                "accumulator allocated {} bytes past {limit}-byte budget before refusal",
                group.bytes()
            );
        }

        // 2. Add-then-retract delta in one `apply` call: adds 4,000 distinct decimals (+1)
        // and retracts 3,995 of them (-1). Final state would fit under 1 KiB, so only
        // mid-fold admission / per-chunk reconciliation catches the intermediate peak.
        {
            let mut terms = Terms::default();
            let mut plan = seed_plan(&mut terms);
            let plan_vars = plan.vars.clone();
            let group = find_group_mut(&mut plan.root).expect("Group operator");
            let g_col = group.keys[0].expect("group key column");
            let v_col = group.aggs[0].column.flatten().expect("sum arg column");
            let mut delta: Changes = Vec::with_capacity(7_995);
            for i in 10..4_010i64 {
                let mut r = vec![Id::from_i64(i).unwrap(); group.inner.schema.len()];
                r[g_col] = g_key;
                r[v_col] = make_decimal_id(&mut terms, i);
                delta.push((r.into_boxed_slice(), 1));
            }
            for i in 15..4_010i64 {
                let mut r = vec![Id::from_i64(i).unwrap(); group.inner.schema.len()];
                r[g_col] = g_key;
                r[v_col] = make_decimal_id(&mut terms, i);
                delta.push((r.into_boxed_slice(), -1));
            }

            let pool = QueryPool::new(limit);
            let budget = ViewBudget::new(Some(limit), Some(pool.work_parent()));
            budget.try_set_state(group.bytes()).unwrap();

            let mut pass = Pass::new(
                snap.clone(),
                None,
                &[],
                &[],
                &mut terms,
                &vis,
                None,
                &ctl,
                &plan_vars,
            );
            pass.budget = Some(budget.handle());
            pass.pool = Some(pool.clone());
            pass.max_memory_bytes = Some(limit);

            let err = group
                .apply(&mut pass, delta)
                .expect_err("expected add-then-retract peak to refuse during positive fold");
            assert!(
                matches!(
                    err,
                    Error::BudgetExceeded(b)
                        if matches!(b.kind, crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory)
                ),
                "expected Memory/ServerMemory budget error, got {err:?}"
            );
            assert!(
                group.bytes() <= limit,
                "add-then-retract accumulator allocated {} bytes past {limit}-byte budget",
                group.bytes()
            );
        }
    }

    /// Regression for round-3 P2-1 and P2-3: `Group::load` must reject duplicate group
    /// keys before `push_group` (preventing un-admitted 4->8 bucket growth and duplicate
    /// slots), check encoded key/output row widths before allocating, and pre-admit valid
    /// row buffers before decoding them.
    #[test]
    fn group_load_rejects_duplicate_keys_and_validates_admits_rows() {
        use crate::incr::budget::ViewBudget;
        use crate::sparql::pool::QueryPool;

        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let parsed = sparql::parse_query(
            "PREFIX : <http://ex/> SELECT ?g (COUNT(*) AS ?c) WHERE { ?x :g ?g } GROUP BY ?g",
            None,
            &[],
        )
        .unwrap();

        let make_group = || {
            let mut plan = compile(&parsed, None).unwrap();
            find_group_mut(&mut plan.root)
                .expect("Group operator")
                .clone()
        };

        let g_ids = [
            Id::from_i64(1).unwrap(),
            Id::from_i64(2).unwrap(),
            Id::from_i64(3).unwrap(),
        ];
        let c1 = Id::from_i64(1).unwrap();
        let write_count_group = |w: &mut Writer, key: &[Id], out: Option<&[Id]>| {
            w.row(key).unwrap();
            w.i64(1);
            match out {
                Some(o) => {
                    w.u64(1);
                    w.row(o).unwrap();
                }
                None => w.u64(0),
            }
            // Running::save_group for COUNT(*) writes a single u64 count.
            w.u64(1);
        };

        // 1. Measure exact bytes of 3 distinct groups (fills the 4-bucket index: len == 3, cap == 3).
        let terms = Terms::default();
        let mut w3 = Writer::new(&snap, &terms);
        w3.u64(GROUP_STATE_VERSION);
        w3.u64(3);
        for &g in &g_ids {
            write_count_group(&mut w3, &[g], Some(&[g, c1]));
        }
        let body3 = w3.finish();

        let mut baseline = make_group();
        let mut load_terms = Terms::default();
        let mut r = Reader::new(&body3, &snap, &mut load_terms).unwrap();
        baseline.load(&mut r).unwrap();
        assert_eq!(baseline.slots.len(), 3);
        assert_eq!(baseline.index.len(), 3);
        assert_eq!(baseline.index.capacity(), 3);
        let alloc_4 = baseline.index.allocation_size();
        let bytes_3 = baseline.bytes();

        // 2. Duplicate 4th group key immediately after the 4-bucket index fills, under a
        // budget fitting only the 3-group allocation (plus Reader's id table + one key row).
        let mut w_dup = Writer::new(&snap, &terms);
        w_dup.u64(GROUP_STATE_VERSION);
        w_dup.u64(4);
        for &g in &g_ids {
            write_count_group(&mut w_dup, &[g], Some(&[g, c1]));
        }
        write_count_group(&mut w_dup, &[g_ids[0]], Some(&[g_ids[0], c1]));
        let body_dup = w_dup.finish();

        // Reader id table holds 4 entries (UNDEF + 3 ints) -> Vec cap 4 = 32 B.
        // Budget admits 3 groups + id table + the 8-byte decoded key row, well below an 8-bucket resize.
        let dup_limit = bytes_3 + 32 + 8;
        let pool = QueryPool::new(dup_limit);
        let budget = ViewBudget::new(Some(dup_limit), Some(pool.work_parent()));
        let mut dup_group = make_group();
        let mut load_terms = Terms::default();
        let mut r =
            Reader::with_budget(&body_dup, &snap, &mut load_terms, Some(budget.handle())).unwrap();
        let err = dup_group
            .load(&mut r)
            .expect_err("duplicate group key must be rejected");
        assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
        assert_eq!(
            dup_group.index.allocation_size(),
            alloc_4,
            "duplicate group key must not grow a full 4-bucket index"
        );
        assert_eq!(
            dup_group.slots.len(),
            3,
            "duplicate group key must not append a fourth slot"
        );
        drop(r);
        budget.clear_state();
        drop(budget);
        assert_eq!(pool.granted(), 0);

        // 3. Valid group key row under a budget that refuses before allocating the key row.
        let mut w_one = Writer::new(&snap, &terms);
        w_one.u64(GROUP_STATE_VERSION);
        w_one.u64(1);
        write_count_group(&mut w_one, &[g_ids[0]], Some(&[g_ids[0], c1]));
        let body_one = w_one.finish();

        // Reader id table for 1 unique id needs 32 B; allow 35 B so the 8-byte key row refuses.
        let tight_pool = QueryPool::new(35);
        let tight_budget = ViewBudget::new(Some(35), Some(tight_pool.work_parent()));
        let mut tight_group = make_group();
        let mut load_terms = Terms::default();
        let mut r = Reader::with_budget(
            &body_one,
            &snap,
            &mut load_terms,
            Some(tight_budget.handle()),
        )
        .unwrap();
        let err = tight_group
            .load(&mut r)
            .expect_err("budget below key row size must refuse before allocating row");
        assert!(matches!(err, Error::BudgetExceeded(_)), "got {err:?}");
        assert_eq!(tight_group.slots.len(), 0);
        drop(r);
        tight_budget.clear_state();
        drop(tight_budget);
        assert_eq!(tight_pool.granted(), 0);

        // 4. Wrong-width key row (oversized width 65_536) and wrong-width output row (width 3
        // instead of 2) must fail with Error::Corrupt before budget admission or allocation.
        let mut w_bad_key = Writer::new(&snap, &terms);
        w_bad_key.u64(GROUP_STATE_VERSION);
        w_bad_key.u64(1);
        w_bad_key.u64(65_536);
        let body_bad_key = w_bad_key.finish();

        let tiny_pool = QueryPool::new(64);
        let tiny_budget = ViewBudget::new(Some(64), Some(tiny_pool.work_parent()));
        let mut bad_key_group = make_group();
        let mut load_terms = Terms::default();
        let mut r = Reader::with_budget(
            &body_bad_key,
            &snap,
            &mut load_terms,
            Some(tiny_budget.handle()),
        )
        .unwrap();
        let err = bad_key_group
            .load(&mut r)
            .expect_err("oversized group key width must fail with Corrupt");
        assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
        drop(r);
        tiny_budget.clear_state();
        drop(tiny_budget);
        assert_eq!(tiny_pool.granted(), 0);

        let mut w_bad_out = Writer::new(&snap, &terms);
        w_bad_out.u64(GROUP_STATE_VERSION);
        w_bad_out.u64(1);
        w_bad_out.row(&[g_ids[0]]).unwrap();
        w_bad_out.i64(1);
        w_bad_out.u64(1);
        w_bad_out.row(&[g_ids[0], c1, c1]).unwrap();
        let body_bad_out = w_bad_out.finish();

        let out_pool = QueryPool::new(128);
        let out_budget = ViewBudget::new(Some(128), Some(out_pool.work_parent()));
        let mut bad_out_group = make_group();
        let mut load_terms = Terms::default();
        let mut r = Reader::with_budget(
            &body_bad_out,
            &snap,
            &mut load_terms,
            Some(out_budget.handle()),
        )
        .unwrap();
        let err = bad_out_group
            .load(&mut r)
            .expect_err("wrong-width output row must fail with Corrupt");
        assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
        assert_eq!(bad_out_group.slots.len(), 0);
        drop(r);
        out_budget.clear_state();
        drop(out_budget);
        assert_eq!(out_pool.granted(), 0);
    }
}
