//! Join: `Δ(A ⋈ B) = ΔA ⋈ B_old + A_new ⋈ ΔB`.
//!
//! A side that is a basic graph pattern is probed at the snapshot the term needs. A side
//! that is anything else keeps an arrangement of its rows by the join key, the shared
//! variables both sides always bind. Shared variables that a side may leave unbound are
//! checked row by row, as SPARQL's compatible mappings ask.

use super::super::pass::{Pass, Which};
use super::super::persist::{Reader, Writer};
use super::super::zset::{Changes, consolidate};
use super::{Arrangement, Merge, Node, side_matches};
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
        }
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
        self.larr.as_ref().map_or(0, |a| a.bytes()) + self.rarr.as_ref().map_or(0, |a| a.bytes())
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        for a in [&self.larr, &self.rarr].into_iter().flatten() {
            a.save(w)?;
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        for a in [&mut self.larr, &mut self.rarr].into_iter().flatten() {
            a.load(r)?;
        }
        Ok(())
    }
}
