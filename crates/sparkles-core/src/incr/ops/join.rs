//! Join: `Δ(A ⋈ B) = ΔA ⋈ B_old + A_new ⋈ ΔB`.
//!
//! A side that is a basic graph pattern is probed at the snapshot the term needs. A side
//! that is anything else keeps an arrangement of its rows by the join key, the shared
//! variables both sides always bind. Shared variables that a side may leave unbound are
//! checked row by row, as SPARQL's compatible mappings ask.

use super::super::pass::{Pass, Which};
use super::super::persist::{Reader, Writer};
use super::super::prov::{self, AChanges, AnnMap, Provenance};
use super::super::zset::{Changes, consolidate, row_bytes};
use super::{Arrangement, Merge, Node, for_each_side_match, for_each_side_match_a};
use crate::error::Result;
use rustc_hash::FxHashMap;

#[derive(Clone, Debug)]
pub struct Join {
    pub left: Node,
    pub right: Node,
    pub m: Merge,
    /// the left's rows, unless the left is a pattern
    pub larr: Option<Arrangement>,
    pub rarr: Option<Arrangement>,
    /// the annotations of the arranged rows, when the circuit keeps them (spec F13 §5)
    pub lann: Option<AnnMap>,
    pub rann: Option<AnnMap>,
    prov: Provenance,
}

impl Join {
    pub fn new(left: Node, right: Node) -> Join {
        let m = Merge::new(&left, &right);
        let larr = left
            .as_bgp()
            .is_none()
            .then(|| Arrangement::new(m.key.iter().map(|k| k.0).collect(), left.schema.len()));
        let rarr = right
            .as_bgp()
            .is_none()
            .then(|| Arrangement::new(m.key.iter().map(|k| k.1).collect(), right.schema.len()));
        Join {
            left,
            right,
            m,
            larr,
            rarr,
            lann: None,
            rann: None,
            prov: Provenance::None,
        }
    }

    pub fn enable_prov(&mut self, mode: Provenance) {
        self.prov = mode;
        let on = !mode.is_none();
        self.lann = (on && self.larr.is_some()).then(AnnMap::default);
        self.rann = (on && self.rarr.is_some()).then(AnnMap::default);
    }

    /// [`init`](Self::init) with annotations: a merged row's annotation is the product
    /// of its sides' annotations as their states keep them.
    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        let l_mark = p.output_lease_mark();
        let l_raw = self.left.init_a(p)?;
        let l_lease = p.take_output_leases_since(l_mark);
        let l = prov::consolidate(l_raw);
        let r_mark = p.output_lease_mark();
        let r_raw = self.right.init_a(p)?;
        let r_lease = p.take_output_leases_since(r_mark);
        let r = prov::consolidate(r_raw);
        let mut tracked = self.bytes();
        let lw = self.left.schema.len();
        let rw = self.right.schema.len();
        let key_w = self.m.key.len();
        let l = if let (Some(larr), Some(lann)) = (&mut self.larr, &mut self.lann) {
            let other = self.rarr.as_ref().map_or(0, |a| a.bytes())
                + self.rann.as_ref().map_or(0, |m| prov::map_bytes(m, rw));
            larr.clear();
            lann.clear();
            p.sync_op_state(&mut tracked, other)?;
            let per_row = row_bytes(lw) * 2 + row_bytes(key_w);
            let mut eff = AChanges::new();
            let mut eff_lease = p.scratch_lease();
            let mut eff_ann_bytes = 0u64;
            for (idx, (row, w, a)) in l.into_iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + larr.bytes() + prov::map_bytes(lann, lw) + per_row + a.bytes();
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                larr.add(&row, w);
                let e = prov::absorb_row(lann, &row, a, mode);
                p.preadmit_achanges_push(&eff, lw, &mut eff_ann_bytes, e.bytes(), &mut eff_lease)?;
                eff.push((row, w, e));
            }
            p.sync_op_state(
                &mut tracked,
                other + larr.bytes() + prov::map_bytes(lann, lw),
            )?;
            drop(l_lease);
            (eff, vec![eff_lease])
        } else {
            (l, l_lease)
        };
        let (l, l_lease) = l;
        let r = if let (Some(rarr), Some(rann)) = (&mut self.rarr, &mut self.rann) {
            let other = self.larr.as_ref().map_or(0, |a| a.bytes())
                + self.lann.as_ref().map_or(0, |m| prov::map_bytes(m, lw));
            rarr.clear();
            rann.clear();
            p.sync_op_state(&mut tracked, other)?;
            let per_row = row_bytes(rw) * 2 + row_bytes(key_w);
            let mut eff = AChanges::new();
            let mut eff_lease = p.scratch_lease();
            let mut eff_ann_bytes = 0u64;
            for (idx, (row, w, a)) in r.into_iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + rarr.bytes() + prov::map_bytes(rann, rw) + per_row + a.bytes();
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                rarr.add(&row, w);
                let e = prov::absorb_row(rann, &row, a, mode);
                p.preadmit_achanges_push(&eff, rw, &mut eff_ann_bytes, e.bytes(), &mut eff_lease)?;
                eff.push((row, w, e));
            }
            p.sync_op_state(
                &mut tracked,
                other + rarr.bytes() + prov::map_bytes(rann, rw),
            )?;
            drop(r_lease);
            (eff, vec![eff_lease])
        } else {
            (r, r_lease)
        };
        let (r, r_lease) = r;
        let mut by_key_lease = p.scratch_lease();
        let key_w = self.m.key.len();
        by_key_lease.try_reserve((r.len() as u64).saturating_mul(row_bytes(key_w) + 32))?;
        let mut by_key: FxHashMap<_, Vec<usize>> = FxHashMap::default();
        for (i, (row, _, _)) in r.iter().enumerate() {
            by_key.entry(self.m.key_right(row)).or_default().push(i);
        }
        let out_width = self.m.out.len();
        let mut out = AChanges::new();
        let mut out_lease = p.scratch_lease();
        let mut out_ann_bytes = 0u64;
        for (a, wa, aa) in &l {
            p.tick()?;
            if let Some(is) = by_key.get(&self.m.key_left(a)) {
                for &i in is {
                    let (b, wb, ab) = &r[i];
                    if self.m.compatible(a, b) {
                        let merged_ann = aa.mul(ab);
                        p.preadmit_achanges_push(
                            &out,
                            out_width,
                            &mut out_ann_bytes,
                            merged_ann.bytes(),
                            &mut out_lease,
                        )?;
                        out.push((self.m.merge(a, b), wa * wb, merged_ann));
                    }
                }
            }
        }
        drop(by_key);
        drop(by_key_lease);
        drop(r);
        drop(r_lease);
        drop(l);
        drop(l_lease);
        p.push_output_lease(out_lease);
        Ok(out)
    }

    /// [`delta`](Self::delta) with annotations. Each side's change is first absorbed
    /// into its state, and the effective change joins, so the output's change is the
    /// change of the product of the states.
    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        let l_mark = p.output_lease_mark();
        let dl_raw = self.left.delta_a(p)?;
        let dl_lease = p.take_output_leases_since(l_mark);
        let dl = prov::consolidate(dl_raw);
        let r_mark = p.output_lease_mark();
        let dr_raw = self.right.delta_a(p)?;
        let dr_lease = p.take_output_leases_since(r_mark);
        let dr = prov::consolidate(dr_raw);
        let mut tracked = self.bytes();
        let lw = self.left.schema.len();
        let rw = self.right.schema.len();
        let key_w = self.m.key.len();
        let (dl, dl_lease) = if let (Some(larr), Some(lann)) = (&mut self.larr, &mut self.lann) {
            let other = self.rarr.as_ref().map_or(0, |a| a.bytes())
                + self.rann.as_ref().map_or(0, |m| prov::map_bytes(m, rw));
            let per_row = row_bytes(lw) * 2 + row_bytes(key_w);
            let mut eff = AChanges::new();
            let mut eff_lease = p.scratch_lease();
            let mut eff_ann_bytes = 0u64;
            for (idx, (row, w, a)) in dl.into_iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + larr.bytes() + prov::map_bytes(lann, lw) + per_row + a.bytes();
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                larr.add(&row, w);
                let e = prov::absorb_row(lann, &row, a, mode);
                p.preadmit_achanges_push(&eff, lw, &mut eff_ann_bytes, e.bytes(), &mut eff_lease)?;
                eff.push((row, w, e));
            }
            p.sync_op_state(
                &mut tracked,
                other + larr.bytes() + prov::map_bytes(lann, lw),
            )?;
            drop(dl_lease);
            (eff, vec![eff_lease])
        } else {
            (dl, dl_lease)
        };
        let out_width = self.m.out.len();
        let mut out = AChanges::new();
        let mut out_lease = p.scratch_lease();
        let mut out_ann_bytes = 0u64;
        // ΔA ⋈ B_old
        for (a, wa, aa) in &dl {
            p.tick()?;
            let key = self.m.key_left(a);
            let seed = self.m.seed_right(a);
            for_each_side_match_a(
                p,
                &self.right,
                self.rarr.as_ref(),
                self.rann.as_ref(),
                Which::Old,
                &key,
                &seed,
                |b, wb, ab, p| {
                    if self.m.compatible(a, b) {
                        let merged_ann = aa.mul(ab);
                        p.preadmit_achanges_push(
                            &out,
                            out_width,
                            &mut out_ann_bytes,
                            merged_ann.bytes(),
                            &mut out_lease,
                        )?;
                        out.push((self.m.merge(a, b), wa * wb, merged_ann));
                    }
                    Ok(())
                },
            )?;
        }
        drop(dl);
        drop(dl_lease);
        let (dr, dr_lease) = if let (Some(rarr), Some(rann)) = (&mut self.rarr, &mut self.rann) {
            let other = self.larr.as_ref().map_or(0, |a| a.bytes())
                + self.lann.as_ref().map_or(0, |m| prov::map_bytes(m, lw));
            let per_row = row_bytes(rw) * 2 + row_bytes(key_w);
            let mut eff = AChanges::new();
            let mut eff_lease = p.scratch_lease();
            let mut eff_ann_bytes = 0u64;
            for (idx, (row, w, a)) in dr.into_iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + rarr.bytes() + prov::map_bytes(rann, rw) + per_row + a.bytes();
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                rarr.add(&row, w);
                let e = prov::absorb_row(rann, &row, a, mode);
                p.preadmit_achanges_push(&eff, rw, &mut eff_ann_bytes, e.bytes(), &mut eff_lease)?;
                eff.push((row, w, e));
            }
            p.sync_op_state(
                &mut tracked,
                other + rarr.bytes() + prov::map_bytes(rann, rw),
            )?;
            drop(dr_lease);
            (eff, vec![eff_lease])
        } else {
            (dr, dr_lease)
        };
        // A_new ⋈ ΔB
        for (b, wb, ab) in &dr {
            p.tick()?;
            let key = self.m.key_right(b);
            let seed = self.m.seed_left(b);
            for_each_side_match_a(
                p,
                &self.left,
                self.larr.as_ref(),
                self.lann.as_ref(),
                Which::New,
                &key,
                &seed,
                |a, wa, aa, p| {
                    if self.m.compatible(a, b) {
                        let merged_ann = aa.mul(ab);
                        p.preadmit_achanges_push(
                            &out,
                            out_width,
                            &mut out_ann_bytes,
                            merged_ann.bytes(),
                            &mut out_lease,
                        )?;
                        out.push((self.m.merge(a, b), wa * wb, merged_ann));
                    }
                    Ok(())
                },
            )?;
        }
        drop(dr);
        drop(dr_lease);
        p.push_output_lease(out_lease);
        Ok(out)
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let l_mark = p.output_lease_mark();
        let l_raw = self.left.init(p)?;
        let l_lease = p.take_output_leases_since(l_mark);
        let l = consolidate(l_raw);
        let r_mark = p.output_lease_mark();
        let r_raw = self.right.init(p)?;
        let r_lease = p.take_output_leases_since(r_mark);
        let r = consolidate(r_raw);
        let mut tracked = self.bytes();
        // a rebuild starts from nothing: the old rows hold ids of the old term space
        let ann_other = self
            .lann
            .as_ref()
            .map_or(0, |m| prov::map_bytes(m, self.left.schema.len()))
            + self
                .rann
                .as_ref()
                .map_or(0, |m| prov::map_bytes(m, self.right.schema.len()));
        let key_w = self.m.key.len();
        if let Some(larr) = &mut self.larr {
            let other = self.rarr.as_ref().map_or(0, |a| a.bytes()) + ann_other;
            let per_row = row_bytes(self.left.schema.len()) + row_bytes(key_w);
            larr.clear();
            p.sync_op_state(&mut tracked, other)?;
            for (idx, (row, w)) in l.iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + larr.bytes() + per_row;
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                larr.add(row, *w);
            }
            p.sync_op_state(&mut tracked, other + larr.bytes())?;
        }
        if let Some(rarr) = &mut self.rarr {
            let other = self.larr.as_ref().map_or(0, |a| a.bytes()) + ann_other;
            let per_row = row_bytes(self.right.schema.len()) + row_bytes(key_w);
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
        let mut by_key_lease = p.scratch_lease();
        let key_w = self.m.key.len();
        by_key_lease.try_reserve((r.len() as u64).saturating_mul(row_bytes(key_w) + 32))?;
        let mut by_key: FxHashMap<_, Vec<usize>> = FxHashMap::default();
        for (i, (row, _)) in r.iter().enumerate() {
            by_key.entry(self.m.key_right(row)).or_default().push(i);
        }
        let out_width = self.m.out.len();
        let mut out = Changes::new();
        let mut out_lease = p.scratch_lease();
        for (a, wa) in &l {
            p.tick()?;
            if let Some(is) = by_key.get(&self.m.key_left(a)) {
                for &i in is {
                    let (b, wb) = &r[i];
                    if self.m.compatible(a, b) {
                        p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                        out.push((self.m.merge(a, b), wa * wb));
                    }
                }
            }
        }
        drop(by_key);
        drop(by_key_lease);
        drop(r);
        drop(r_lease);
        drop(l);
        drop(l_lease);
        p.push_output_lease(out_lease);
        Ok(out)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let l_mark = p.output_lease_mark();
        let dl_raw = self.left.delta(p)?;
        let dl_lease = p.take_output_leases_since(l_mark);
        let dl = consolidate(dl_raw);
        let r_mark = p.output_lease_mark();
        let dr_raw = self.right.delta(p)?;
        let dr_lease = p.take_output_leases_since(r_mark);
        let dr = consolidate(dr_raw);
        let mut tracked = self.bytes();
        let out_width = self.m.out.len();
        let mut out = Changes::new();
        let mut out_lease = p.scratch_lease();
        // ΔA ⋈ B_old
        for (a, wa) in &dl {
            p.tick()?;
            let key = self.m.key_left(a);
            let seed = self.m.seed_right(a);
            for_each_side_match(
                p,
                &self.right,
                self.rarr.as_ref(),
                Which::Old,
                &key,
                &seed,
                |b, wb, p| {
                    if self.m.compatible(a, b) {
                        p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                        out.push((self.m.merge(a, b), wa * wb));
                    }
                    Ok(())
                },
            )?;
        }
        let ann_other = self
            .lann
            .as_ref()
            .map_or(0, |m| prov::map_bytes(m, self.left.schema.len()))
            + self
                .rann
                .as_ref()
                .map_or(0, |m| prov::map_bytes(m, self.right.schema.len()));
        let key_w = self.m.key.len();
        if let Some(larr) = &mut self.larr {
            let other = self.rarr.as_ref().map_or(0, |a| a.bytes()) + ann_other;
            let per_row = row_bytes(self.left.schema.len()) + row_bytes(key_w);
            for (idx, (row, w)) in dl.iter().enumerate() {
                if idx % 1024 == 1023 {
                    p.tick()?;
                }
                let est = other + larr.bytes() + per_row;
                if est > tracked {
                    p.sync_op_state(&mut tracked, est)?;
                }
                larr.add(row, *w);
            }
            p.sync_op_state(&mut tracked, other + larr.bytes())?;
        }
        drop(dl);
        drop(dl_lease);
        // A_new ⋈ ΔB
        for (b, wb) in &dr {
            p.tick()?;
            let key = self.m.key_right(b);
            let seed = self.m.seed_left(b);
            for_each_side_match(
                p,
                &self.left,
                self.larr.as_ref(),
                Which::New,
                &key,
                &seed,
                |a, wa, p| {
                    if self.m.compatible(a, b) {
                        p.preadmit_changes_push(&out, out_width, &mut out_lease)?;
                        out.push((self.m.merge(a, b), wa * wb));
                    }
                    Ok(())
                },
            )?;
        }
        if let Some(rarr) = &mut self.rarr {
            let other = self.larr.as_ref().map_or(0, |a| a.bytes()) + ann_other;
            let per_row = row_bytes(self.right.schema.len()) + row_bytes(key_w);
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
            p.sync_op_state(&mut tracked, other + rarr.bytes())?;
        }
        drop(dr);
        drop(dr_lease);
        p.push_output_lease(out_lease);
        Ok(out)
    }

    pub fn bytes(&self) -> u64 {
        self.larr.as_ref().map_or(0, |a| a.bytes())
            + self.rarr.as_ref().map_or(0, |a| a.bytes())
            + self
                .lann
                .as_ref()
                .map_or(0, |m| prov::map_bytes(m, self.left.schema.len()))
            + self
                .rann
                .as_ref()
                .map_or(0, |m| prov::map_bytes(m, self.right.schema.len()))
    }

    pub fn clear(&mut self) {
        if let Some(a) = &mut self.larr {
            a.clear();
        }
        if let Some(a) = &mut self.rarr {
            a.clear();
        }
        if let Some(m) = &mut self.lann {
            *m = AnnMap::default();
        }
        if let Some(m) = &mut self.rann {
            *m = AnnMap::default();
        }
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        for a in [&self.larr, &self.rarr].into_iter().flatten() {
            a.save(w)?;
        }
        for m in [&self.lann, &self.rann].into_iter().flatten() {
            prov::save_map(w, m)?;
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        for a in [&mut self.larr, &mut self.rarr].into_iter().flatten() {
            a.load(r)?;
        }
        let mode = self.prov;
        for m in [&mut self.lann, &mut self.rann].into_iter().flatten() {
            *m = prov::load_map(r, mode)?;
        }
        Ok(())
    }
}
