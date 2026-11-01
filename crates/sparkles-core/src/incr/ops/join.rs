//! Join: `Δ(A ⋈ B) = ΔA ⋈ B_old + A_new ⋈ ΔB`.
//!
//! A side that is a basic graph pattern is probed at the snapshot the term needs. A side
//! that is anything else keeps an arrangement of its rows by the join key, the shared
//! variables both sides always bind. Shared variables that a side may leave unbound are
//! checked row by row, as SPARQL's compatible mappings ask.

use super::super::pass::{Pass, Which};
use super::super::persist::{Reader, Writer};
use super::super::prov::{self, AChanges, AnnMap, Provenance};
use super::super::zset::{Changes, consolidate};
use super::{Arrangement, Merge, Node, absorb_side, side_matches, side_matches_a};
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
        self.lann = self.larr.is_some().then(AnnMap::default);
        self.rann = self.rarr.is_some().then(AnnMap::default);
    }

    /// [`init`](Self::init) with annotations: a merged row's annotation is the product
    /// of its sides' annotations as their states keep them.
    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        let l = prov::consolidate(self.left.init_a(p)?);
        let r = prov::consolidate(self.right.init_a(p)?);
        let l = match (&mut self.larr, &mut self.lann) {
            (Some(a), Some(m)) => {
                a.clear();
                m.clear();
                absorb_side(a, m, l, mode)
            }
            _ => l,
        };
        let r = match (&mut self.rarr, &mut self.rann) {
            (Some(a), Some(m)) => {
                a.clear();
                m.clear();
                absorb_side(a, m, r, mode)
            }
            _ => r,
        };
        let mut by_key: FxHashMap<_, Vec<usize>> = FxHashMap::default();
        for (i, (row, _, _)) in r.iter().enumerate() {
            by_key.entry(self.m.key_right(row)).or_default().push(i);
        }
        let mut out = AChanges::new();
        for (a, wa, aa) in &l {
            p.tick()?;
            if let Some(is) = by_key.get(&self.m.key_left(a)) {
                for &i in is {
                    let (b, wb, ab) = &r[i];
                    if self.m.compatible(a, b) {
                        out.push((self.m.merge(a, b), wa * wb, aa.mul(ab)));
                    }
                }
            }
        }
        Ok(out)
    }

    /// [`delta`](Self::delta) with annotations. Each side's change is first absorbed
    /// into its state, and the effective change joins, so the output's change is the
    /// change of the product of the states.
    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        let dl = prov::consolidate(self.left.delta_a(p)?);
        let dr = prov::consolidate(self.right.delta_a(p)?);
        let dl = match (&mut self.larr, &mut self.lann) {
            (Some(a), Some(m)) => absorb_side(a, m, dl, mode),
            _ => dl,
        };
        let mut out = AChanges::new();
        // ΔA ⋈ B_old
        for (a, wa, aa) in &dl {
            let key = self.m.key_left(a);
            let seed = self.m.seed_right(a);
            for (b, wb, ab) in side_matches_a(
                p,
                &self.right,
                self.rarr.as_ref(),
                self.rann.as_ref(),
                Which::Old,
                &key,
                &seed,
            )? {
                if self.m.compatible(a, &b) {
                    out.push((self.m.merge(a, &b), wa * wb, aa.mul(&ab)));
                }
            }
        }
        let dr = match (&mut self.rarr, &mut self.rann) {
            (Some(a), Some(m)) => absorb_side(a, m, dr, mode),
            _ => dr,
        };
        // A_new ⋈ ΔB
        for (b, wb, ab) in &dr {
            let key = self.m.key_right(b);
            let seed = self.m.seed_left(b);
            for (a, wa, aa) in side_matches_a(
                p,
                &self.left,
                self.larr.as_ref(),
                self.lann.as_ref(),
                Which::New,
                &key,
                &seed,
            )? {
                if self.m.compatible(&a, b) {
                    out.push((self.m.merge(&a, b), wa * wb, aa.mul(ab)));
                }
            }
        }
        Ok(out)
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        let l = consolidate(self.left.init(p)?);
        let r = consolidate(self.right.init(p)?);
        // a rebuild starts from nothing: the old rows hold ids of the old term space
        if let Some(a) = &mut self.larr {
            a.clear();
            a.apply(&l);
        }
        if let Some(a) = &mut self.rarr {
            a.clear();
            a.apply(&r);
        }
        let mut by_key: FxHashMap<_, Vec<usize>> = FxHashMap::default();
        for (i, (row, _)) in r.iter().enumerate() {
            by_key.entry(self.m.key_right(row)).or_default().push(i);
        }
        let mut out = Changes::new();
        for (a, wa) in &l {
            p.tick()?;
            if let Some(is) = by_key.get(&self.m.key_left(a)) {
                for &i in is {
                    let (b, wb) = &r[i];
                    if self.m.compatible(a, b) {
                        out.push((self.m.merge(a, b), wa * wb));
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let dl = consolidate(self.left.delta(p)?);
        let dr = consolidate(self.right.delta(p)?);
        let mut out = Changes::new();
        // ΔA ⋈ B_old
        for (a, wa) in &dl {
            let key = self.m.key_left(a);
            let seed = self.m.seed_right(a);
            for (b, wb) in
                side_matches(p, &self.right, self.rarr.as_ref(), Which::Old, &key, &seed)?
            {
                if self.m.compatible(a, &b) {
                    out.push((self.m.merge(a, &b), wa * wb));
                }
            }
        }
        if let Some(arr) = &mut self.larr {
            arr.apply(&dl);
        }
        // A_new ⋈ ΔB
        for (b, wb) in &dr {
            let key = self.m.key_right(b);
            let seed = self.m.seed_left(b);
            for (a, wa) in side_matches(p, &self.left, self.larr.as_ref(), Which::New, &key, &seed)?
            {
                if self.m.compatible(&a, b) {
                    out.push((self.m.merge(&a, b), wa * wb));
                }
            }
        }
        if let Some(arr) = &mut self.rarr {
            arr.apply(&dr);
        }
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
