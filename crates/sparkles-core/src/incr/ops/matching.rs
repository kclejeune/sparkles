//! Optional, minus and `FILTER (NOT) EXISTS`: operators that keep a left row according to
//! how many right rows match it.
//!
//! The state holds each left row with its weight and its match count `m`, the summed
//! weight of the right rows that are compatible with it (and, for `OPTIONAL`, satisfy
//! its filter, for `MINUS`, share a bound variable with it). A change on the right moves
//! the counts of the left rows it matches. A left row whose count crosses zero moves
//! between the parts of the output: for `OPTIONAL` between the joined rows and the row
//! alone, for `MINUS` and `NOT EXISTS` in or out, and for `EXISTS` out or in.

use super::super::pass::{EffectiveChanges, Pass, Which};
use super::super::persist::{Reader, Writer};
use super::super::prov::{self, AChanges, Ann, AnnMap, Provenance};
use super::super::zset::{Changes, Row, consolidate, row_bytes};
use super::{Arrangement, Merge, Node, for_each_side_match, for_each_side_match_a};
use crate::error::Result;
use crate::id::Id;
use rustc_hash::FxHashMap;
use spargebra::algebra::Expression;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    LeftJoin,
    Minus,
    NotExists,
    Exists,
}

impl MatchMode {
    pub fn name(self) -> &'static str {
        match self {
            MatchMode::LeftJoin => "Optional",
            MatchMode::Minus => "Minus",
            MatchMode::NotExists => "NotExists",
            MatchMode::Exists => "Exists",
        }
    }

    /// Whether a left row with match count `m` is in the output alone.
    fn alone(self, m: i64) -> bool {
        match self {
            MatchMode::Exists => m > 0,
            _ => m == 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MatchOp {
    pub mode: MatchMode,
    pub left: Node,
    pub right: Node,
    /// the filter of an `OPTIONAL`, over the merged row
    pub expr: Option<Expression>,
    pub m: Merge,
    /// left rows by join key: weight and match count
    lstate: FxHashMap<Row, FxHashMap<Row, (i64, i64)>>,
    lrows: usize,
    rarr: Option<Arrangement>,
    /// the annotations of the left rows, when the circuit keeps them (spec F13 §5)
    lann: Option<AnnMap>,
    /// for `EXISTS`, the sum of the annotations of each left row's matches
    sann: Option<AnnMap>,
    rann: Option<AnnMap>,
    /// the number of the operator's negative condition (spec F13 §5.2)
    pub cond: u32,
    prov: Provenance,
}

impl MatchOp {
    pub fn new(mode: MatchMode, left: Node, right: Node, expr: Option<Expression>) -> MatchOp {
        let m = Merge::new(&left, &right);
        let rarr = right
            .as_bgp()
            .is_none()
            .then(|| Arrangement::new(m.key.iter().map(|k| k.1).collect(), right.schema.len()));
        MatchOp {
            mode,
            left,
            right,
            expr,
            m,
            lstate: FxHashMap::default(),
            lrows: 0,
            rarr,
            lann: None,
            sann: None,
            rann: None,
            cond: u32::MAX,
            prov: Provenance::None,
        }
    }

    pub fn enable_prov(&mut self, mode: Provenance, cond: u32) {
        self.prov = mode;
        self.cond = cond;
        let on = !mode.is_none();
        self.lann = on.then(AnnMap::default);
        self.sann = (on && self.mode == MatchMode::Exists).then(AnnMap::default);
        self.rann = (on && self.rarr.is_some()).then(AnnMap::default);
    }

    #[inline]
    fn pair_compatible(&self, a: &[Id], b: &[Id]) -> bool {
        self.m.compatible(a, b) && (self.mode != MatchMode::Minus || self.m.overlap(a, b))
    }

    /// Evaluate `self.expr` on a bounded chunk of compatible `(left, right)` row slices,
    /// pre-admitting the merged buffer on `lease`.
    fn eval_expr_chunk(
        &self,
        p: &mut Pass,
        pairs: &[(&[Id], &[Id])],
        lease: &mut super::super::budget::ScratchLease,
    ) -> Result<Vec<bool>> {
        let Some(e) = &self.expr else {
            return Ok(vec![true; pairs.len()]);
        };
        let need = (pairs.len() as u64).saturating_mul(row_bytes(self.m.out.len()) + 56);
        if need > lease.bytes() {
            lease.try_set(need)?;
        }
        let merged: Vec<Row> = pairs.iter().map(|(a, b)| self.m.merge(a, b)).collect();
        let rows: Vec<&[Id]> = merged.iter().map(|r| &r[..]).collect();
        let vals = p.eval(&self.m.out, &rows, e, true)?;
        Ok(vals
            .into_iter()
            .map(|v| v == Some(Id::from_bool(true)))
            .collect())
    }

    /// Visit right rows at snapshot `w` that satisfy the condition with left row `a`
    /// (annotated), without cloning arrangement buckets or annotations and while keeping
    /// BGP probe leases alive.
    fn for_each_matching_a(
        &self,
        p: &mut Pass,
        w: Which,
        a: &Row,
        mut f: impl FnMut(&Row, i64, &Ann, &Pass) -> Result<()>,
    ) -> Result<()> {
        let key = self.m.key_left(a);
        let seed = self.m.seed_right(a);
        if self.expr.is_none() {
            return for_each_side_match_a(
                p,
                &self.right,
                self.rarr.as_ref(),
                self.rann.as_ref(),
                w,
                &key,
                &seed,
                |b, wb, ab, p| {
                    if self.pair_compatible(a, b) {
                        f(b, wb, ab, p)?;
                    }
                    Ok(())
                },
            );
        }
        let mut expr_lease = p.scratch_lease();
        let mut chunk_lease = p.scratch_lease();
        let mut flush = |p: &mut Pass, chunk: &mut Vec<(&Row, i64, &Ann)>| -> Result<()> {
            if chunk.is_empty() {
                return Ok(());
            }
            // Account for the pair slices before constructing them. The same lease
            // covers the merged rows, expression inputs, and result flags.
            expr_lease.try_set((chunk.len() as u64) * (row_bytes(self.m.out.len()) + 56))?;
            let pairs: Vec<(&[Id], &[Id])> =
                chunk.iter().map(|(b, _, _)| (&a[..], &b[..])).collect();
            let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
            for ((b, wb, ab), k) in chunk.drain(..).zip(keep) {
                if k {
                    f(b, wb, ab, p)?;
                }
            }
            Ok(())
        };
        let mut visit = |p: &mut Pass, rows: &mut dyn Iterator<Item = (&Row, i64, &Ann)>| {
            let mut chunk = Vec::new();
            for (b, wb, ab) in rows {
                if !self.pair_compatible(a, b) {
                    continue;
                }
                crate::incr::circuit::reserve_vec_slot(&chunk, &mut chunk_lease)?;
                chunk.push((b, wb, ab));
                if chunk.len() == 1024 {
                    flush(p, &mut chunk)?;
                }
            }
            flush(p, &mut chunk)
        };
        match (self.rarr.as_ref(), self.rann.as_ref()) {
            (Some(arr), Some(ann_map)) => {
                let zero = Ann::zero(p.prov);
                visit(
                    p,
                    &mut arr
                        .get(&key)
                        .map(|(b, wb)| (b, wb, ann_map.get(b).unwrap_or(&zero))),
                )
            }
            _ => {
                let bgp = self.right.as_bgp().ok_or_else(|| {
                    crate::error::Error::Corrupt(
                        "a join side without annotated state is not a pattern".into(),
                    )
                })?;
                let (probed, probe_lease) = bgp.probe_a_charged(p, w, &seed)?;
                let result = visit(p, &mut probed.iter().map(|(b, wb, ab)| (b, *wb, ab)));
                drop(probed);
                drop(probe_lease);
                result
            }
        }
    }

    /// The annotation of left row `a` alone in the output, from its annotation `la`
    /// and, for `EXISTS`, the sum `sa` of its matches': the product with its matches
    /// for `EXISTS`, and with the condition that nothing matches otherwise.
    fn alone_ann(&self, p: &mut Pass, a: &Row, la: &Ann, sa: &Ann) -> Ann {
        match self.mode {
            MatchMode::Exists => la.mul(sa),
            _ => {
                // an OPTIONAL's filter can read any left variable, so its condition
                // binds them all
                let vals: Vec<(u32, Id)> = if self.expr.is_some() {
                    self.left
                        .schema
                        .iter()
                        .copied()
                        .zip(a.iter().copied())
                        .collect()
                } else {
                    self.m
                        .shared
                        .iter()
                        .map(|&(i, _)| (self.left.schema[i], a[i]))
                        .collect()
                };
                la.mul(&p.absent_ann(self.cond, &vals))
            }
        }
    }

    /// [`init`](Self::init) with annotations.
    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        let l_mark = p.output_lease_mark();
        let l_raw = self.left.init_a(p)?;
        let l_leases = p.take_output_leases_since(l_mark);
        let l = prov::consolidate(l_raw);
        let r_mark = p.output_lease_mark();
        let r_raw = self.right.init_a(p)?;
        let r_leases = p.take_output_leases_since(r_mark);
        let r = prov::consolidate(r_raw);
        let mut tracked = self.bytes();
        let rw = self.right.schema.len();
        let (r, r_leases) = if let (Some(rarr), Some(rann)) = (&mut self.rarr, &mut self.rann) {
            let lw = self.left.schema.len();
            let other = self.lrows as u64 * (row_bytes(lw) + 8)
                + self.lstate.len() as u64 * row_bytes(self.m.key.len())
                + [&self.lann, &self.sann]
                    .into_iter()
                    .flatten()
                    .map(|m| prov::map_bytes(m, lw))
                    .sum::<u64>();
            rarr.clear();
            rann.clear();
            p.sync_op_state(&mut tracked, other)?;
            let per_row = row_bytes(rw) * 2 + row_bytes(self.m.key.len());
            let mut eff = EffectiveChanges::new(p);
            for (idx, (row, w, a)) in r.into_iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + rarr.bytes() + prov::map_bytes(rann, rw) + per_row + a.bytes();
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                rarr.add(&row, w);
                eff.push(p, rw, rann, (row, w, a))?;
            }
            p.sync_op_state(
                &mut tracked,
                other + rarr.bytes() + prov::map_bytes(rann, rw),
            )?;
            drop(r_leases);
            let (eff, eff_lease) = eff.into_parts();
            (eff, vec![eff_lease])
        } else {
            (r, r_leases)
        };
        let mut rindex_lease = p.scratch_lease();
        rindex_lease
            .try_reserve((r.len() as u64).saturating_mul(row_bytes(self.m.key.len()) + 32))?;
        let mut rindex: FxHashMap<Row, Vec<usize>> = FxHashMap::default();
        for (i, (b, _, _)) in r.iter().enumerate() {
            rindex.entry(self.m.key_right(b)).or_default().push(i);
        }
        self.lstate.clear();
        self.lrows = 0;
        let mut lann = std::mem::take(&mut self.lann).unwrap_or_default();
        let mut sann = std::mem::take(&mut self.sann);
        lann.clear();
        if let Some(s) = sann.as_mut() {
            s.clear();
        }
        p.sync_op_state(&mut tracked, self.bytes())?;
        let out_width = if self.mode == MatchMode::LeftJoin {
            self.m.out.len()
        } else {
            self.left.schema.len()
        };
        let lw = self.left.schema.len();
        let per_left = row_bytes(lw) + 8 + row_bytes(self.m.key.len());
        let mut out = AChanges::new();
        let mut out_lease = p.scratch_lease();
        let mut out_ann_bytes = 0u64;
        let mut expr_lease = p.scratch_lease();
        for (idx, (a, wa, aa)) in l.into_iter().enumerate() {
            p.tick()?;
            let ann_est = self.bytes()
                + prov::map_bytes(&lann, lw)
                + sann.as_ref().map_or(0, |s| prov::map_bytes(s, lw))
                + per_left
                + row_bytes(lw)
                + aa.bytes();
            if ann_est > tracked {
                p.sync_op_state(&mut tracked, ann_est)?;
            }
            prov::absorb_row_state(&mut lann, &a, aa, mode);
            if wa == 0 {
                continue;
            }
            let la = prov::get(&lann, &a, mode);
            let mut m = 0;
            let mut sum = Ann::zero(mode);
            if let Some(indices) = rindex.get(&self.m.key_left(&a)) {
                if self.expr.is_none() {
                    for &i in indices {
                        let (b, wb, ab) = &r[i];
                        if !self.pair_compatible(&a, b) {
                            continue;
                        }
                        m += *wb;
                        if self.mode == MatchMode::LeftJoin {
                            let merged_ann = la.mul(ab);
                            p.preadmit_achanges_push(
                                &out,
                                out_width,
                                &mut out_ann_bytes,
                                merged_ann.bytes(),
                                &mut out_lease,
                            )?;
                            out.push((self.m.merge(&a, b), wa * *wb, merged_ann));
                        } else if self.mode == MatchMode::Exists {
                            sum.add_assign(ab);
                        }
                    }
                } else {
                    let mut chunk: Vec<usize> = Vec::new();
                    for &i in indices {
                        let (b, _, _) = &r[i];
                        if !self.pair_compatible(&a, b) {
                            continue;
                        }
                        chunk.push(i);
                        if chunk.len() == 1024 {
                            let pairs: Vec<(&[Id], &[Id])> =
                                chunk.iter().map(|&j| (&a[..], &r[j].0[..])).collect();
                            let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
                            for (j, k) in chunk.drain(..).zip(keep) {
                                if k {
                                    let (b, wb, ab) = &r[j];
                                    m += *wb;
                                    if self.mode == MatchMode::LeftJoin {
                                        let merged_ann = la.mul(ab);
                                        p.preadmit_achanges_push(
                                            &out,
                                            out_width,
                                            &mut out_ann_bytes,
                                            merged_ann.bytes(),
                                            &mut out_lease,
                                        )?;
                                        out.push((self.m.merge(&a, b), wa * *wb, merged_ann));
                                    } else if self.mode == MatchMode::Exists {
                                        sum.add_assign(ab);
                                    }
                                }
                            }
                        }
                    }
                    if !chunk.is_empty() {
                        let pairs: Vec<(&[Id], &[Id])> =
                            chunk.iter().map(|&j| (&a[..], &r[j].0[..])).collect();
                        let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
                        for (j, k) in chunk.drain(..).zip(keep) {
                            if k {
                                let (b, wb, ab) = &r[j];
                                m += *wb;
                                if self.mode == MatchMode::LeftJoin {
                                    let merged_ann = la.mul(ab);
                                    p.preadmit_achanges_push(
                                        &out,
                                        out_width,
                                        &mut out_ann_bytes,
                                        merged_ann.bytes(),
                                        &mut out_lease,
                                    )?;
                                    out.push((self.m.merge(&a, b), wa * *wb, merged_ann));
                                } else if self.mode == MatchMode::Exists {
                                    sum.add_assign(ab);
                                }
                            }
                        }
                    }
                }
            }
            let sa = match sann.as_mut() {
                Some(s) => {
                    let est = self.bytes()
                        + prov::map_bytes(&lann, lw)
                        + prov::map_bytes(s, lw)
                        + row_bytes(lw)
                        + sum.bytes();
                    if est > tracked {
                        p.sync_op_state(&mut tracked, est)?;
                    }
                    prov::absorb_row_state(s, &a, sum, mode);
                    prov::get(s, &a, mode)
                }
                None => sum,
            };
            self.set(&a, (wa, m));
            if idx % 1024 == 1023 {
                let ann_b = prov::map_bytes(&lann, lw)
                    + sann.as_ref().map_or(0, |s| prov::map_bytes(s, lw));
                p.sync_op_state(&mut tracked, self.bytes() + ann_b)?;
            }
            if self.mode.alone(m) {
                let ann = self.alone_ann(p, &a, &la, &sa);
                p.preadmit_achanges_push(
                    &out,
                    out_width,
                    &mut out_ann_bytes,
                    ann.bytes(),
                    &mut out_lease,
                )?;
                out.push((self.out_row(&a), wa, ann));
            }
        }
        drop(expr_lease);
        self.lann = Some(lann);
        self.sann = sann;
        p.sync_op_state(&mut tracked, self.bytes())?;
        drop(rindex);
        drop(rindex_lease);
        drop(r);
        drop(r_leases);
        drop(l_leases);
        p.push_output_lease(out_lease);
        Ok(out)
    }

    /// [`delta`](Self::delta) with annotations. Each input's change is absorbed into
    /// the state that keeps it, and the output's change is the change of what the
    /// states give: the joined rows of an `OPTIONAL` as a join's are, and the rows
    /// alone as the difference of their annotations before and after.
    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        let l_mark = p.output_lease_mark();
        let dl_raw = self.left.delta_a(p)?;
        let dl_leases = p.take_output_leases_since(l_mark);
        let dl = prov::consolidate(dl_raw);
        let r_mark = p.output_lease_mark();
        let dr_raw = self.right.delta_a(p)?;
        let dr_leases = p.take_output_leases_since(r_mark);
        let dr = prov::consolidate(dr_raw);
        let base = self.bytes();
        let mut tracked = base;
        let lj = self.mode == MatchMode::LeftJoin;
        let ex = self.mode == MatchMode::Exists;
        // the left rows each right change matches, by the counts before the changes
        let lw = self.left.schema.len();
        let rw = self.right.schema.len();
        let mut scratch_lease = p.scratch_lease();
        let mut touched: FxHashMap<Row, (i64, i64)> = FxHashMap::default();
        let mut hits: Vec<(Row, usize)> = Vec::new();
        let mut scratch_bytes = 0u64;
        for (i, (b, _, _)) in dr.iter().enumerate() {
            p.tick()?;
            self.for_each_left_matching(p, b, |a, v, _p| {
                scratch_bytes = scratch_bytes.saturating_add(row_bytes(lw) + 16);
                if !touched.contains_key(a) {
                    scratch_bytes = scratch_bytes.saturating_add(row_bytes(lw) + 24);
                }
                if scratch_bytes > scratch_lease.bytes() {
                    scratch_lease.try_set(scratch_bytes)?;
                }
                hits.push((a.clone(), i));
                touched.entry(a.clone()).or_insert(v);
                Ok(())
            })?;
        }
        for (a, _, _) in &dl {
            if !touched.contains_key(a) {
                scratch_bytes = scratch_bytes.saturating_add(row_bytes(lw) + 24);
                if scratch_bytes > scratch_lease.bytes() {
                    scratch_lease.try_set(scratch_bytes)?;
                }
                let v = self.get(a);
                touched.insert(a.clone(), v);
            }
        }
        let mut lann = std::mem::take(&mut self.lann).unwrap_or_default();
        let mut sann = std::mem::take(&mut self.sann);
        // Read the retained sizes before cloning either annotation into scratch.
        let mut before: FxHashMap<Row, (Ann, Ann)> = FxHashMap::default();
        let zero_bytes = Ann::zero_bytes(mode);
        for a in touched.keys() {
            let la = lann.get(a);
            let sa = sann.as_ref().and_then(|s| s.get(a));
            scratch_bytes = scratch_bytes
                .saturating_add(row_bytes(lw))
                .saturating_add(la.map_or(zero_bytes, Ann::bytes))
                .saturating_add(sa.map_or(zero_bytes, Ann::bytes));
            if scratch_bytes > scratch_lease.bytes() {
                scratch_lease.try_set(scratch_bytes)?;
            }
            before.insert(
                a.clone(),
                (
                    la.cloned().unwrap_or_else(|| Ann::zero(mode)),
                    sa.cloned().unwrap_or_else(|| Ann::zero(mode)),
                ),
            );
        }
        let mut dw: FxHashMap<Row, i64> = FxHashMap::default();
        let mut dl_eff = EffectiveChanges::new(p);
        for (a, wa, aa) in dl {
            let est = self.bytes()
                + prov::map_bytes(&lann, lw)
                + sann.as_ref().map_or(0, |s| prov::map_bytes(s, lw))
                + row_bytes(lw)
                + aa.bytes();
            if est > tracked {
                p.sync_op_state(&mut tracked, est)?;
            }
            if !dw.contains_key(&a) {
                scratch_bytes = scratch_bytes.saturating_add(row_bytes(lw) + 16);
                if scratch_bytes > scratch_lease.bytes() {
                    scratch_lease.try_set(scratch_bytes)?;
                }
            }
            *dw.entry(a.clone()).or_insert(0) += wa;
            if lj {
                dl_eff.push(p, lw, &mut lann, (a, wa, aa))?;
            } else {
                prov::absorb_row_state(&mut lann, &a, aa, mode);
            }
        }
        drop(dl_leases);
        let (dl, dl_eff_lease) = dl_eff.into_parts();
        let out_width = if lj {
            self.m.out.len()
        } else {
            self.left.schema.len()
        };
        let mut out = AChanges::new();
        let mut out_lease = p.scratch_lease();
        let mut out_ann_bytes = 0u64;
        // the joined part: ΔL ⋈ R_old
        if lj {
            for (a, wa, aa) in &dl {
                self.for_each_matching_a(p, Which::Old, a, |b, wb, ab, p| {
                    let merged_ann = aa.mul(ab);
                    p.preadmit_achanges_push(
                        &out,
                        out_width,
                        &mut out_ann_bytes,
                        merged_ann.bytes(),
                        &mut out_lease,
                    )?;
                    out.push((self.m.merge(a, b), wa * wb, merged_ann));
                    Ok(())
                })?;
            }
        }
        drop(dl);
        drop(dl_eff_lease);
        let (dr, dr_leases) = if let (Some(rarr), Some(rann)) = (&mut self.rarr, &mut self.rann) {
            let other = self.lrows as u64 * (row_bytes(lw) + 8)
                + self.lstate.len() as u64 * row_bytes(self.m.key.len())
                + prov::map_bytes(&lann, lw)
                + sann.as_ref().map_or(0, |s| prov::map_bytes(s, lw));
            let per_row = row_bytes(rw) * 2 + row_bytes(self.m.key.len());
            let mut eff = EffectiveChanges::new(p);
            for (idx, (row, w, a)) in dr.into_iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + rarr.bytes() + prov::map_bytes(rann, rw) + per_row + a.bytes();
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                rarr.add(&row, w);
                eff.push(p, rw, rann, (row, w, a))?;
            }
            p.sync_op_state(
                &mut tracked,
                (other + rarr.bytes() + prov::map_bytes(rann, rw)).max(base),
            )?;
            drop(dr_leases);
            let (eff, eff_lease) = eff.into_parts();
            (eff, vec![eff_lease])
        } else {
            (dr, dr_leases)
        };
        let mut dm: FxHashMap<Row, i64> = FxHashMap::default();
        let mut ds: FxHashMap<Row, Ann> = FxHashMap::default();
        let hits_bytes = (hits.len() as u64).saturating_mul(row_bytes(lw) + 16);
        let mut dm_bytes = 0u64;
        let mut ds_bytes = 0u64;
        let zero_ann_bytes = Ann::zero_bytes(mode);
        for (a, i) in hits {
            let (_, wb, ab) = &dr[i];
            if !dm.contains_key(&a) {
                let add = row_bytes(lw) + 16;
                dm_bytes = dm_bytes.saturating_add(add);
                scratch_bytes = scratch_bytes.saturating_add(add);
                if scratch_bytes > scratch_lease.bytes() {
                    scratch_lease.try_set(scratch_bytes)?;
                }
            }
            *dm.entry(a.clone()).or_insert(0) += wb;
            if ex {
                let ab_bytes = ab.bytes();
                let is_new = !ds.contains_key(&a);
                let add_est = if is_new {
                    row_bytes(lw)
                        .saturating_add(zero_ann_bytes)
                        .saturating_add(ab_bytes)
                } else {
                    ab_bytes
                };
                scratch_bytes = scratch_bytes.saturating_add(add_est);
                if scratch_bytes > scratch_lease.bytes() {
                    scratch_lease.try_set(scratch_bytes)?;
                }
                let entry = ds.entry(a).or_insert_with(|| Ann::zero(mode));
                let before_b = entry.bytes();
                entry.add_assign(ab);
                let after_b = entry.bytes();
                let actual_add = if is_new {
                    row_bytes(lw).saturating_add(after_b)
                } else {
                    after_b.saturating_sub(before_b)
                };
                ds_bytes = ds_bytes.saturating_add(actual_add);
                scratch_bytes = scratch_bytes
                    .saturating_sub(add_est)
                    .saturating_add(actual_add);
                if scratch_bytes < scratch_lease.bytes() {
                    scratch_lease.release(scratch_lease.bytes() - scratch_bytes);
                }
            }
        }
        // `hits` has been consumed; release its tracked bytes before admitting `new`.
        scratch_bytes = scratch_bytes.saturating_sub(hits_bytes);
        let new_slot = std::mem::size_of::<(Row, (i64, i64), (i64, i64))>() as u64;
        let new_bytes = (touched.len() as u64).saturating_mul(new_slot);
        scratch_bytes = scratch_bytes.saturating_add(new_bytes);
        scratch_lease.try_set(scratch_bytes)?;
        #[allow(clippy::type_complexity)]
        let mut new: Vec<(Row, (i64, i64), (i64, i64))> = Vec::with_capacity(touched.len());
        let dw_bytes = (dw.len() as u64).saturating_mul(row_bytes(lw) + 16);
        let touched_bytes =
            (touched.len() as u64).saturating_mul(row_bytes(lw) + 24 - lw as u64 * 8);
        for (a, (ow, om)) in touched {
            let nw = ow + dw.get(&a).copied().unwrap_or(0);
            let nm = if ow != 0 {
                if let (Some(s), Some(d)) = (sann.as_mut(), ds.remove(&a)) {
                    let d_b = d.bytes();
                    let extra =
                        d_b.saturating_add(if s.contains_key(&a) { 0 } else { row_bytes(lw) });
                    let est =
                        self.bytes() + prov::map_bytes(&lann, lw) + prov::map_bytes(s, lw) + extra;
                    if est > tracked {
                        p.sync_op_state(&mut tracked, est)?;
                    }
                    prov::absorb_row_state(s, &a, d, mode);
                    let drop_b = row_bytes(lw).saturating_add(d_b);
                    ds_bytes = ds_bytes.saturating_sub(drop_b);
                    scratch_bytes = scratch_bytes.saturating_sub(drop_b);
                }
                om + dm.get(&a).copied().unwrap_or(0)
            } else if nw != 0 {
                let mut sum_w = 0i64;
                let mut sum_a = Ann::zero(mode);
                let mut sum_lease = p.scratch_lease();
                let track_s = sann.is_some();
                self.for_each_matching_a(p, Which::New, &a, |_b, wb, ab, _p| {
                    sum_w += wb;
                    if track_s {
                        let est = sum_a.bytes().saturating_add(ab.bytes());
                        if est > sum_lease.bytes() {
                            sum_lease.try_set(est)?;
                        }
                        sum_a.add_assign(ab);
                        let actual = sum_a.bytes();
                        if actual < sum_lease.bytes() {
                            sum_lease.release(sum_lease.bytes() - actual);
                        }
                    }
                    Ok(())
                })?;
                if let Some(s) = sann.as_mut() {
                    let extra = sum_a.bytes().saturating_add(if s.contains_key(&a) {
                        0
                    } else {
                        row_bytes(lw)
                    });
                    let est =
                        self.bytes() + prov::map_bytes(&lann, lw) + prov::map_bytes(s, lw) + extra;
                    if est > tracked {
                        p.sync_op_state(&mut tracked, est)?;
                    }
                    s.remove(&a);
                    prov::absorb_row_state(s, &a, sum_a, mode);
                }
                drop(sum_lease);
                sum_w
            } else {
                0
            };
            if nw == 0
                && let Some(s) = sann.as_mut()
            {
                s.remove(&a);
            }
            new.push((a, (ow, om), (nw, nm)));
        }
        drop(dw);
        drop(dm);
        drop(ds);
        scratch_bytes = scratch_bytes
            .saturating_sub(dw_bytes)
            .saturating_sub(dm_bytes)
            .saturating_sub(ds_bytes)
            .saturating_sub(touched_bytes);
        if scratch_bytes < scratch_lease.bytes() {
            scratch_lease.release(scratch_lease.bytes() - scratch_bytes);
        }
        let per_left = row_bytes(lw) + 8 + row_bytes(self.m.key.len());
        for (idx, (a, _, v)) in new.iter().enumerate() {
            let ann_b =
                prov::map_bytes(&lann, lw) + sann.as_ref().map_or(0, |s| prov::map_bytes(s, lw));
            let est = self.bytes() + ann_b + per_left;
            if est > tracked {
                p.sync_op_state(&mut tracked, est)?;
            }
            self.set(a, *v);
            if idx % 1024 == 1023 {
                p.tick()?;
                p.sync_op_state(&mut tracked, (self.bytes() + ann_b).max(base))?;
            }
        }
        let ann_b =
            prov::map_bytes(&lann, lw) + sann.as_ref().map_or(0, |s| prov::map_bytes(s, lw));
        p.sync_op_state(&mut tracked, (self.bytes() + ann_b).max(base))?;
        // the joined part: L_new ⋈ ΔR
        if lj {
            for (b, wb, ab) in &dr {
                self.for_each_left_matching(p, b, |a, (wa, _), p| {
                    let la = prov::get(&lann, a, mode);
                    let merged_ann = la.mul(ab);
                    p.preadmit_achanges_push(
                        &out,
                        out_width,
                        &mut out_ann_bytes,
                        merged_ann.bytes(),
                        &mut out_lease,
                    )?;
                    out.push((self.m.merge(a, b), wa * wb, merged_ann));
                    Ok(())
                })?;
            }
        }
        drop(dr);
        drop(dr_leases);
        // rows that move in or out of the part without a match, or whose annotation
        // there changes
        for (a, (ow, om), (nw, nm)) in new {
            let was = ow != 0 && self.mode.alone(om);
            let is = nw != 0 && self.mode.alone(nm);
            let before_w = if was { ow } else { 0 };
            let after_w = if is { nw } else { 0 };
            let zero = Ann::zero(mode);
            let (ola, osa) = &before[&a];
            let old = if was {
                self.alone_ann(p, &a, ola, osa)
            } else {
                zero.clone()
            };
            let now = if is {
                let la = prov::get(&lann, &a, mode);
                let sa = sann
                    .as_ref()
                    .map_or_else(|| Ann::zero(mode), |s| prov::get(s, &a, mode));
                self.alone_ann(p, &a, &la, &sa)
            } else {
                zero
            };
            let d = now.sub(&old);
            if before_w != after_w || !d.is_zero() {
                p.preadmit_achanges_push(
                    &out,
                    out_width,
                    &mut out_ann_bytes,
                    d.bytes(),
                    &mut out_lease,
                )?;
                out.push((self.out_row(&a), after_w - before_w, d));
            }
        }
        drop(before);
        drop(scratch_lease);
        self.lann = Some(lann);
        self.sann = sann;
        p.sync_op_state(&mut tracked, self.bytes().max(base))?;
        p.push_output_lease(out_lease);
        Ok(out)
    }

    fn get(&self, a: &Row) -> (i64, i64) {
        self.lstate
            .get(&self.m.key_left(a))
            .and_then(|b| b.get(a))
            .copied()
            .unwrap_or((0, 0))
    }

    fn set(&mut self, a: &Row, v: (i64, i64)) {
        let k = self.m.key_left(a);
        let bucket = self.lstate.entry(k.clone()).or_default();
        let had = bucket.contains_key(a);
        if v.0 == 0 {
            if had {
                bucket.remove(a);
                self.lrows -= 1;
            }
            if bucket.is_empty() {
                self.lstate.remove(&k);
            }
        } else {
            if !had {
                self.lrows += 1;
            }
            bucket.insert(a.clone(), v);
        }
    }

    /// Visit right rows at snapshot `w` that satisfy the condition with left row `a`,
    /// without cloning arrangement buckets and while keeping BGP probe leases alive.
    fn for_each_matching(
        &self,
        p: &mut Pass,
        w: Which,
        a: &Row,
        mut f: impl FnMut(&Row, i64, &Pass) -> Result<()>,
    ) -> Result<()> {
        let key = self.m.key_left(a);
        let seed = self.m.seed_right(a);
        if self.expr.is_none() {
            return for_each_side_match(
                p,
                &self.right,
                self.rarr.as_ref(),
                w,
                &key,
                &seed,
                |b, wb, p| {
                    if self.pair_compatible(a, b) {
                        f(b, wb, p)?;
                    }
                    Ok(())
                },
            );
        }
        let mut expr_lease = p.scratch_lease();
        match self.rarr.as_ref() {
            Some(arr) => {
                let mut chunk: Vec<(&Row, i64)> = Vec::new();
                for (b, wb) in arr.get(&key) {
                    if !self.pair_compatible(a, b) {
                        continue;
                    }
                    chunk.push((b, wb));
                    if chunk.len() == 1024 {
                        let pairs: Vec<(&[Id], &[Id])> =
                            chunk.iter().map(|(b, _)| (&a[..], &b[..])).collect();
                        let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
                        for ((b, wb), k) in chunk.drain(..).zip(keep) {
                            if k {
                                f(b, wb, p)?;
                            }
                        }
                    }
                }
                if !chunk.is_empty() {
                    let pairs: Vec<(&[Id], &[Id])> =
                        chunk.iter().map(|(b, _)| (&a[..], &b[..])).collect();
                    let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
                    for ((b, wb), k) in chunk.drain(..).zip(keep) {
                        if k {
                            f(b, wb, p)?;
                        }
                    }
                }
                drop(chunk);
                drop(expr_lease);
                Ok(())
            }
            None => match self.right.as_bgp() {
                Some(bgp) => {
                    let (probed, probe_lease) = bgp.probe_charged(p, w, &seed)?;
                    let mut chunk: Vec<(&Row, i64)> = Vec::new();
                    for (b, wb) in &probed {
                        if !self.pair_compatible(a, b) {
                            continue;
                        }
                        chunk.push((b, *wb));
                        if chunk.len() == 1024 {
                            let pairs: Vec<(&[Id], &[Id])> =
                                chunk.iter().map(|(b, _)| (&a[..], &b[..])).collect();
                            let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
                            for ((b, wb), k) in chunk.drain(..).zip(keep) {
                                if k {
                                    f(b, wb, p)?;
                                }
                            }
                        }
                    }
                    if !chunk.is_empty() {
                        let pairs: Vec<(&[Id], &[Id])> =
                            chunk.iter().map(|(b, _)| (&a[..], &b[..])).collect();
                        let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
                        for ((b, wb), k) in chunk.drain(..).zip(keep) {
                            if k {
                                f(b, wb, p)?;
                            }
                        }
                    }
                    drop(chunk);
                    drop(expr_lease);
                    drop(probed);
                    drop(probe_lease);
                    Ok(())
                }
                None => Err(crate::error::Error::Corrupt(
                    "a join side without an arrangement is not a pattern".into(),
                )),
            },
        }
    }

    /// Visit left rows in `lstate` that satisfy the condition with right row `b`,
    /// iterating the bucket directly by reference without cloning it into a vector.
    fn for_each_left_matching(
        &self,
        p: &mut Pass,
        b: &Row,
        mut f: impl FnMut(&Row, (i64, i64), &Pass) -> Result<()>,
    ) -> Result<()> {
        let key = self.m.key_right(b);
        let Some(bk) = self.lstate.get(&key) else {
            return Ok(());
        };
        if self.expr.is_none() {
            for (a, &v) in bk {
                if self.pair_compatible(a, b) {
                    f(a, v, p)?;
                }
            }
            return Ok(());
        }
        let mut expr_lease = p.scratch_lease();
        let mut chunk: Vec<(&Row, (i64, i64))> = Vec::new();
        for (a, &v) in bk {
            if !self.pair_compatible(a, b) {
                continue;
            }
            chunk.push((a, v));
            if chunk.len() == 1024 {
                let pairs: Vec<(&[Id], &[Id])> =
                    chunk.iter().map(|(a, _)| (&a[..], &b[..])).collect();
                let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
                for ((a, v), k) in chunk.drain(..).zip(keep) {
                    if k {
                        f(a, v, p)?;
                    }
                }
            }
        }
        if !chunk.is_empty() {
            let pairs: Vec<(&[Id], &[Id])> = chunk.iter().map(|(a, _)| (&a[..], &b[..])).collect();
            let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
            for ((a, v), k) in chunk.drain(..).zip(keep) {
                if k {
                    f(a, v, p)?;
                }
            }
        }
        drop(chunk);
        drop(expr_lease);
        Ok(())
    }

    fn out_row(&self, a: &Row) -> Row {
        match self.mode {
            MatchMode::LeftJoin => self.m.pad_left(a),
            _ => a.clone(),
        }
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let l_mark = p.output_lease_mark();
        let l_raw = self.left.init(p)?;
        let l_leases = p.take_output_leases_since(l_mark);
        let l = consolidate(l_raw);
        let r_mark = p.output_lease_mark();
        let r_raw = self.right.init(p)?;
        let r_leases = p.take_output_leases_since(r_mark);
        let r = consolidate(r_raw);
        let mut tracked = self.bytes();
        let mut rindex_lease = p.scratch_lease();
        let rw = self.right.schema.len();
        rindex_lease.try_reserve(
            (r.len() as u64).saturating_mul(row_bytes(rw) + row_bytes(self.m.key.len())),
        )?;
        let mut rindex = Arrangement::new(self.m.key.iter().map(|k| k.1).collect(), rw);
        rindex.apply(&r);
        if let Some(rarr) = &mut self.rarr {
            let lw = self.left.schema.len();
            let other = self.lrows as u64 * (row_bytes(lw) + 8)
                + self.lstate.len() as u64 * row_bytes(self.m.key.len())
                + [&self.lann, &self.sann]
                    .into_iter()
                    .flatten()
                    .map(|m| prov::map_bytes(m, lw))
                    .sum::<u64>()
                + self.rann.as_ref().map_or(0, |m| prov::map_bytes(m, rw));
            let per_row = row_bytes(rw) + row_bytes(self.m.key.len());
            rarr.clear();
            p.sync_op_state(&mut tracked, other)?;
            for (idx, (row, w)) in r.iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + rarr.bytes() + per_row;
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                rarr.add(row, *w);
            }
            p.sync_op_state(&mut tracked, other + rarr.bytes())?;
        }
        self.lstate.clear();
        self.lrows = 0;
        p.sync_op_state(&mut tracked, self.bytes())?;
        let out_width = if self.mode == MatchMode::LeftJoin {
            self.m.out.len()
        } else {
            self.left.schema.len()
        };
        let lw = self.left.schema.len();
        let per_left = row_bytes(lw) + 8 + row_bytes(self.m.key.len());
        let mut out = Changes::new();
        let mut out_lease = p.scratch_lease();
        let mut expr_lease = p.scratch_lease();
        for (idx, (a, wa)) in l.iter().enumerate() {
            p.tick()?;
            let mut m = 0;
            if self.expr.is_none() {
                for (b, wb) in rindex.get(&self.m.key_left(a)) {
                    if !self.pair_compatible(a, b) {
                        continue;
                    }
                    m += wb;
                    if self.mode == MatchMode::LeftJoin {
                        p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                        out.push((self.m.merge(a, b), wa * wb));
                    }
                }
            } else {
                let mut chunk: Vec<(&Row, i64)> = Vec::new();
                for (b, wb) in rindex.get(&self.m.key_left(a)) {
                    if !self.pair_compatible(a, b) {
                        continue;
                    }
                    chunk.push((b, wb));
                    if chunk.len() == 1024 {
                        let pairs: Vec<(&[Id], &[Id])> =
                            chunk.iter().map(|(b, _)| (&a[..], &b[..])).collect();
                        let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
                        for ((b, wb), k) in chunk.drain(..).zip(keep) {
                            if k {
                                m += wb;
                                if self.mode == MatchMode::LeftJoin {
                                    p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                                    out.push((self.m.merge(a, b), wa * wb));
                                }
                            }
                        }
                    }
                }
                if !chunk.is_empty() {
                    let pairs: Vec<(&[Id], &[Id])> =
                        chunk.iter().map(|(b, _)| (&a[..], &b[..])).collect();
                    let keep = self.eval_expr_chunk(p, &pairs, &mut expr_lease)?;
                    for ((b, wb), k) in chunk.drain(..).zip(keep) {
                        if k {
                            m += wb;
                            if self.mode == MatchMode::LeftJoin {
                                p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                                out.push((self.m.merge(a, b), wa * wb));
                            }
                        }
                    }
                }
            }
            let est = self.bytes() + per_left;
            if est > tracked {
                p.sync_op_state(&mut tracked, est)?;
            }
            self.set(a, (*wa, m));
            if idx % 1024 == 1023 {
                p.sync_op_state(&mut tracked, self.bytes())?;
            }
            if self.mode.alone(m) {
                p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                out.push((self.out_row(a), *wa));
            }
        }
        drop(expr_lease);
        p.sync_op_state(&mut tracked, self.bytes())?;
        drop(rindex);
        drop(rindex_lease);
        drop(r);
        drop(r_leases);
        drop(l);
        drop(l_leases);
        p.push_output_lease(out_lease);
        Ok(out)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let l_mark = p.output_lease_mark();
        let dl_raw = self.left.delta(p)?;
        let dl_leases = p.take_output_leases_since(l_mark);
        let dl = consolidate(dl_raw);
        let r_mark = p.output_lease_mark();
        let dr_raw = self.right.delta(p)?;
        let dr_leases = p.take_output_leases_since(r_mark);
        let dr = consolidate(dr_raw);
        let base = self.bytes();
        let mut tracked = base;
        let lj = self.mode == MatchMode::LeftJoin;
        let out_width = if lj {
            self.m.out.len()
        } else {
            self.left.schema.len()
        };
        let lw = self.left.schema.len();
        let rw = self.right.schema.len();
        let mut out = Changes::new();
        let mut out_lease = p.scratch_lease();
        // the joined part: ΔL ⋈ R_old
        if lj {
            for (a, wa) in &dl {
                self.for_each_matching(p, Which::Old, a, |b, wb, p| {
                    p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                    out.push((self.m.merge(a, b), wa * wb));
                    Ok(())
                })?;
            }
        }
        // the counts of the left rows already present move with the right's changes
        let mut scratch_lease = p.scratch_lease();
        let mut scratch_bytes = 0u64;
        let mut touched: FxHashMap<Row, (i64, i64)> = FxHashMap::default();
        let mut dm: FxHashMap<Row, i64> = FxHashMap::default();
        for (b, wb) in &dr {
            p.tick()?;
            self.for_each_left_matching(p, b, |a, v, _p| {
                if !dm.contains_key(a) {
                    scratch_bytes = scratch_bytes.saturating_add(row_bytes(lw) + 16);
                }
                if !touched.contains_key(a) {
                    scratch_bytes = scratch_bytes.saturating_add(row_bytes(lw) + 24);
                }
                if scratch_bytes > scratch_lease.bytes() {
                    scratch_lease.try_set(scratch_bytes)?;
                }
                *dm.entry(a.clone()).or_insert(0) += *wb;
                touched.entry(a.clone()).or_insert(v);
                Ok(())
            })?;
        }
        if let Some(rarr) = &mut self.rarr {
            let other = self.lrows as u64 * (row_bytes(lw) + 8)
                + self.lstate.len() as u64 * row_bytes(self.m.key.len())
                + [&self.lann, &self.sann]
                    .into_iter()
                    .flatten()
                    .map(|m| prov::map_bytes(m, lw))
                    .sum::<u64>()
                + self.rann.as_ref().map_or(0, |m| prov::map_bytes(m, rw));
            let per_row = row_bytes(rw) + row_bytes(self.m.key.len());
            for (idx, (row, w)) in dr.iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + rarr.bytes() + per_row;
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                rarr.add(row, *w);
            }
            p.sync_op_state(&mut tracked, (other + rarr.bytes()).max(base))?;
        }
        // the left's own changes; a row that appears counts its matches anew
        let mut dw: FxHashMap<Row, i64> = FxHashMap::default();
        for (a, wa) in &dl {
            if !dw.contains_key(a) {
                scratch_bytes = scratch_bytes.saturating_add(row_bytes(lw) + 16);
            }
            if !touched.contains_key(a) {
                scratch_bytes = scratch_bytes.saturating_add(row_bytes(lw) + 24);
            }
            if scratch_bytes > scratch_lease.bytes() {
                scratch_lease.try_set(scratch_bytes)?;
            }
            *dw.entry(a.clone()).or_insert(0) += wa;
            if !touched.contains_key(a) {
                let v = self.get(a);
                touched.insert(a.clone(), v);
            }
        }
        drop(dl);
        drop(dl_leases);
        let new_slot = std::mem::size_of::<(Row, (i64, i64), (i64, i64))>() as u64;
        let new_bytes = (touched.len() as u64).saturating_mul(new_slot);
        let dw_bytes = (dw.len() as u64).saturating_mul(row_bytes(lw) + 16);
        let dm_bytes = (dm.len() as u64).saturating_mul(row_bytes(lw) + 16);
        let touched_bytes =
            (touched.len() as u64).saturating_mul(row_bytes(lw) + 24 - lw as u64 * 8);
        scratch_bytes = scratch_bytes.saturating_add(new_bytes);
        if scratch_bytes > scratch_lease.bytes() {
            scratch_lease.try_set(scratch_bytes)?;
        }
        #[allow(clippy::type_complexity)]
        let mut new: Vec<(Row, (i64, i64), (i64, i64))> = Vec::with_capacity(touched.len());
        for (a, (ow, om)) in touched {
            let nw = ow + dw.get(&a).copied().unwrap_or(0);
            let nm = if ow != 0 {
                om + dm.get(&a).copied().unwrap_or(0)
            } else if nw != 0 {
                let mut sum_w = 0i64;
                self.for_each_matching(p, Which::New, &a, |_b, wb, _p| {
                    sum_w += wb;
                    Ok(())
                })?;
                sum_w
            } else {
                0
            };
            new.push((a, (ow, om), (nw, nm)));
        }
        drop(dw);
        drop(dm);
        scratch_bytes = scratch_bytes
            .saturating_sub(dw_bytes)
            .saturating_sub(dm_bytes)
            .saturating_sub(touched_bytes);
        if scratch_bytes < scratch_lease.bytes() {
            scratch_lease.release(scratch_lease.bytes() - scratch_bytes);
        }
        let per_left = row_bytes(lw) + 8 + row_bytes(self.m.key.len());
        for (idx, (a, _, v)) in new.iter().enumerate() {
            let est = self.bytes() + per_left;
            if est > tracked {
                p.sync_op_state(&mut tracked, est)?;
            }
            self.set(a, *v);
            if idx % 1024 == 1023 {
                p.tick()?;
                p.sync_op_state(&mut tracked, self.bytes().max(base))?;
            }
        }
        p.sync_op_state(&mut tracked, self.bytes().max(base))?;
        // the joined part: L_new ⋈ ΔR
        if lj {
            for (b, wb) in &dr {
                self.for_each_left_matching(p, b, |a, (wa, _), p| {
                    p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                    out.push((self.m.merge(a, b), wa * wb));
                    Ok(())
                })?;
            }
        }
        drop(dr);
        drop(dr_leases);
        // rows that move in or out of the part without a match
        for (a, (ow, om), (nw, nm)) in new {
            let before = if ow != 0 && self.mode.alone(om) {
                ow
            } else {
                0
            };
            let after = if nw != 0 && self.mode.alone(nm) {
                nw
            } else {
                0
            };
            if before != after {
                p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                out.push((self.out_row(&a), after - before));
            }
        }
        drop(scratch_lease);
        p.push_output_lease(out_lease);
        Ok(out)
    }

    pub fn bytes(&self) -> u64 {
        let lw = self.left.schema.len();
        self.lrows as u64 * (row_bytes(lw) + 8)
            + self.lstate.len() as u64 * row_bytes(self.m.key.len())
            + self.rarr.as_ref().map_or(0, |a| a.bytes())
            + [&self.lann, &self.sann]
                .into_iter()
                .flatten()
                .map(|m| prov::map_bytes(m, lw))
                .sum::<u64>()
            + self
                .rann
                .as_ref()
                .map_or(0, |m| prov::map_bytes(m, self.right.schema.len()))
    }

    pub fn clear(&mut self) {
        self.lstate = FxHashMap::default();
        self.lrows = 0;
        if let Some(a) = &mut self.rarr {
            a.clear();
        }
        for m in [&mut self.lann, &mut self.sann, &mut self.rann]
            .into_iter()
            .flatten()
        {
            *m = AnnMap::default();
        }
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.u64(self.lrows as u64);
        for bucket in self.lstate.values() {
            for (a, (n, m)) in bucket {
                w.row(a)?;
                w.i64(*n);
                w.i64(*m);
            }
        }
        if let Some(a) = &self.rarr {
            a.save(w)?;
        }
        for m in [&self.lann, &self.sann, &self.rann].into_iter().flatten() {
            prov::save_map(w, m)?;
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.lstate = FxHashMap::default();
        self.lrows = 0;
        let n = r.u64()?;
        let lw = self.left.schema.len();
        let per = row_bytes(lw) + 8 + row_bytes(self.m.key.len());
        for _ in 0..n {
            let a = r.row()?;
            if a.len() != lw {
                return Err(crate::error::Error::Corrupt(
                    "a view's state holds a row of another width".into(),
                ));
            }
            let w = r.i64()?;
            let m = r.i64()?;
            r.charge_state(per)?;
            self.set(&a, (w, m));
        }
        if let Some(a) = &mut self.rarr {
            a.load(r)?;
        }
        let mode = self.prov;
        for m in [&mut self.lann, &mut self.sann, &mut self.rann]
            .into_iter()
            .flatten()
        {
            *m = prov::load_map(r, mode)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incr::budget::ViewBudget;
    use crate::incr::pass::Vis;
    use crate::incr::prov::{Atom, FactKey, Key128, Witness};
    use crate::incr::terms::Terms;
    use crate::store::{Store, StoreOptions};
    use crate::task::Control;

    #[test]
    fn matching_keeps_effective_why_support_capacity_charged_between_rows() {
        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let vis = Vis::new(None, &snap);
        let control = Control::none();
        let mode = Provenance::Why { max_witnesses: 1 };
        let fact = |n| Atom::Fact(FactKey(Key128::from_u128(n)));
        let old = Ann::derivation(mode, (10..138).map(fact).collect());
        let next = Ann::derivation(mode, vec![fact(1)]);
        let old_witness = old.report(1).witnesses.pop().unwrap();
        let support_slot = std::mem::size_of::<(Witness, i64)>() as u64;
        let live_ann = old.bytes() + next.bytes() - Ann::zero_bytes(mode) + 2 * support_slot;
        let rows = [
            Box::from([Id::from_i64(1).unwrap()]) as Row,
            Box::from([Id::from_i64(2).unwrap()]) as Row,
        ];
        let output_slots = 4 * std::mem::size_of::<(Row, i64, Ann)>() as u64;
        let two_rows = output_slots + 16 + live_ann + next.effective_bytes(Some(&old), 1);

        for headroom in [two_rows - 1, two_rows] {
            let budget = ViewBudget::new(Some(headroom), None);
            let mut terms = Terms::default();
            let mut pass = Pass::new(
                snap.clone(),
                None,
                &[],
                &[],
                &mut terms,
                &vis,
                None,
                &control,
                &[],
            );
            pass.prov = mode;
            pass.budget = Some(budget.handle());
            let mut state = AnnMap::default();
            for row in &rows {
                state.insert(row.clone(), old.clone());
            }
            let mut output = EffectiveChanges::new(&pass);
            output
                .push(&pass, 1, &mut state, (rows[0].clone(), 1, next.clone()))
                .unwrap();
            let first_charge = headroom - budget.handle().remaining_local().unwrap();
            let second = output.push(&pass, 1, &mut state, (rows[1].clone(), 1, next.clone()));
            if headroom < two_rows {
                assert!(matches!(second, Err(crate::Error::BudgetExceeded(_))));
            } else {
                second.unwrap();
            }
            assert_eq!(first_charge, output_slots + 8 + live_ann);
            let (output, lease) = output.into_parts();
            assert_eq!(
                lease.bytes(),
                output_slots + output.len() as u64 * (8 + live_ann)
            );
            for (row, _, effective) in &output {
                let mut prior = old.clone();
                prior.add_assign(effective);
                let actual = prior.report(1);
                assert_eq!(actual.count, 2);
                assert!(actual.truncated);
                assert!(!actual.witnesses.contains(&old_witness));
                assert_eq!(
                    actual.witnesses,
                    state.get(row).unwrap().report(1).witnesses
                );
            }
        }
    }
}
