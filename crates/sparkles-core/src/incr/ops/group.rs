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
use crate::sparql::plan::Agg;
use crate::sparql::table::Table;
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::{AggregateFunction, Expression};
use std::fmt;

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
    index: FxHashMap<Row, u32>,
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
            index: FxHashMap::default(),
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
        self.index = FxHashMap::default();
        self.slots = Vec::new();
        for acc in &mut self.accs {
            acc.clear();
        }
    }

    fn shrink_unused(&mut self) {
        const SHRINK_MIN_CAP: usize = 64;
        if self.slots.is_empty() {
            self.slots = Vec::new();
            self.index = FxHashMap::default();
            for acc in &mut self.accs {
                acc.clear();
            }
            return;
        }
        if self.slots.capacity() >= SHRINK_MIN_CAP && self.slots.len() * 4 <= self.slots.capacity()
        {
            self.slots.shrink_to(self.slots.len() * 2);
        }
        if self.index.capacity() >= SHRINK_MIN_CAP && self.index.len() * 4 <= self.index.capacity()
        {
            self.index.shrink_to(self.index.len() * 2);
        }
        for acc in &mut self.accs {
            acc.shrink_unused();
        }
    }

    #[cfg(test)]
    pub(crate) fn backing_capacities(&self) -> (usize, usize) {
        (self.slots.capacity(), self.index.capacity())
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
            prov::absorb_row(&mut gann, &k, a, mode);
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
        let mut pos_idx: Vec<usize> = Vec::new();
        let mut pos_slots: Vec<u32> = Vec::new();
        let mut pos_weights: Vec<i64> = Vec::new();
        let mut neg_idx: Vec<usize> = Vec::new();
        let mut neg_slots: Vec<u32> = Vec::new();
        let mut neg_weights: Vec<i64> = Vec::new();
        for (i, (r, w)) in c.iter().enumerate() {
            if i.is_multiple_of(1024) {
                p.control.check()?;
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
        let mut out = Changes::new();
        let aggs = &self.aggs;
        let accs = &mut self.accs;
        let index = &mut self.index;
        let slots = &mut self.slots;
        let implicit = self.implicit;
        let mut removed_any = false;
        p.with_agg_ctx(|ctx| {
            // Fold positive weights first, then retract negative weights, so intermediate
            // multiplicities stay non-negative and SAMPLE stays stable on replacements.
            for (ai, acc) in accs.iter_mut().enumerate() {
                if !pos_slots.is_empty() {
                    let pos_col: Vec<Id> = match &args[ai] {
                        Some(col) => pos_idx.iter().map(|&i| col[i]).collect(),
                        None => Vec::new(),
                    };
                    let arg = match (&aggs[ai].expr, aggs[ai].column) {
                        (None, _) => Arg::Star,
                        (_, Some(None)) => Arg::Unbound,
                        _ => Arg::Column(&pos_col),
                    };
                    let batch = Batch {
                        table: &pos_table,
                        arg,
                        rows: None,
                        weights: Some(&pos_weights),
                        base: 0,
                    };
                    acc.add_batch(ctx, &batch, &pos_slots)?;
                }
                if !neg_slots.is_empty() {
                    let neg_col: Vec<Id> = match &args[ai] {
                        Some(col) => neg_idx.iter().map(|&i| col[i]).collect(),
                        None => Vec::new(),
                    };
                    let arg = match (&aggs[ai].expr, aggs[ai].column) {
                        (None, _) => Arg::Star,
                        (_, Some(None)) => Arg::Unbound,
                        _ => Arg::Column(&neg_col),
                    };
                    let batch = Batch {
                        table: &neg_table,
                        arg,
                        rows: None,
                        weights: Some(&neg_weights),
                        base: 0,
                    };
                    debug_assert!(acc.retractable());
                    if !acc.retract(ctx, &batch, &neg_slots)? {
                        return Err(Error::Corrupt(
                            "incremental view accumulator refused retraction".into(),
                        ));
                    }
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
        Ok(out)
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        self.clear_state();
        let c = consolidate(self.inner.init(p)?);
        let mut out = self.apply(p, c)?;
        if self.implicit && self.slots.is_empty() {
            let slot = self.push_group(Row::default()) as usize;
            let row: Row = self.read_slot(p, slot)?.into();
            self.slots[slot].out = Some(row.clone());
            out.push((row, 1));
        }
        Ok(out)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = consolidate(self.inner.delta(p)?);
        if c.is_empty() {
            return Ok(c);
        }
        self.apply(p, c)
    }

    pub fn bytes(&self) -> u64 {
        let key_row_bytes = self.keys.len() as u64 * 8;
        let out_row_bytes = (self.keys.len() + self.aggs.len()) as u64 * 8;
        let live_rows = self.slots.len() as u64 * (key_row_bytes * 2 + out_row_bytes);
        let slots_buf = self.slots.capacity() as u64 * std::mem::size_of::<GroupMeta>() as u64;
        let index_buf =
            self.index.capacity() as u64 * (std::mem::size_of::<(Row, u32)>() as u64 + 8);
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
        let n = r.u64()?;
        for _ in 0..n {
            let key = r.row()?;
            let rows = r.i64()?;
            let out = match r.u64()? {
                0 => None,
                _ => Some(r.row()?),
            };
            let slot = self.push_group(key) as usize;
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
