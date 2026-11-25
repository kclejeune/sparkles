//! Distinct: a row's output weight is 1 while its input weight is positive, so the
//! operator emits `+1` or `−1` when the input weight crosses zero. `REDUCED` is
//! maintained as `DISTINCT`, and a CONSTRUCT template's triples pass through it because
//! a graph is a set.

use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::prov::AChanges;
use super::super::zset::{Changes, ZSet};
use super::Node;
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct Distinct {
    pub inner: Node,
    pub weights: ZSet,
}

impl Distinct {
    fn apply(&mut self, p: &mut Pass, c: Changes) -> Result<Changes> {
        let mut out = Changes::new();
        let mut tracked = self.bytes();
        for (i, (r, w)) in c.into_iter().enumerate() {
            let (old, new) = self.weights.add(r.clone(), w);
            match (old > 0, new > 0) {
                (false, true) => out.push((r, 1)),
                (true, false) => out.push((r, -1)),
                _ => {}
            }
            if i % 1024 == 1023 {
                p.tick()?;
                p.sync_op_state(&mut tracked, self.bytes())?;
            }
        }
        p.sync_op_state(&mut tracked, self.bytes())?;
        Ok(out)
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        self.weights = ZSet::new();
        let c = self.inner.init(p)?;
        self.apply(p, c)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.delta(p)?;
        self.apply(p, c)
    }

    /// [`apply`](Self::apply) with annotations: a row's annotation is the sum of its
    /// input rows' annotations, so every annotation change passes, with the weight's
    /// crossing when there is one.
    fn apply_a(&mut self, p: &mut Pass, c: AChanges) -> Result<AChanges> {
        let mut out = AChanges::new();
        let mut tracked = self.bytes();
        for (i, (r, w, a)) in c.into_iter().enumerate() {
            let (old, new) = self.weights.add(r.clone(), w);
            let d = match (old > 0, new > 0) {
                (false, true) => 1,
                (true, false) => -1,
                _ => 0,
            };
            if d != 0 || !a.is_zero() {
                out.push((r, d, a));
            }
            if i % 1024 == 1023 {
                p.tick()?;
                p.sync_op_state(&mut tracked, self.bytes())?;
            }
        }
        p.sync_op_state(&mut tracked, self.bytes())?;
        Ok(out)
    }

    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        self.weights = ZSet::new();
        let c = self.inner.init_a(p)?;
        self.apply_a(p, c)
    }

    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let c = self.inner.delta_a(p)?;
        self.apply_a(p, c)
    }

    pub fn bytes(&self) -> u64 {
        self.weights.bytes(self.inner.schema.len())
    }

    pub fn clear(&mut self) {
        self.weights = ZSet::new();
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.zset(&self.weights)
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.weights = r.zset()?;
        Ok(())
    }
}
