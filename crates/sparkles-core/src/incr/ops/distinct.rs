//! Distinct: a row's output weight is 1 while its input weight is positive, so the
//! operator emits `+1` or `−1` when the input weight crosses zero. `REDUCED` is
//! maintained as `DISTINCT`, and a CONSTRUCT template's triples pass through it because
//! a graph is a set.

use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::zset::{Changes, ZSet};
use super::Node;
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct Distinct {
    pub inner: Node,
    pub weights: ZSet,
}

impl Distinct {
    fn apply(&mut self, c: Changes) -> Changes {
        let mut out = Changes::new();
        for (r, w) in c {
            let (old, new) = self.weights.add(r.clone(), w);
            match (old > 0, new > 0) {
                (false, true) => out.push((r, 1)),
                (true, false) => out.push((r, -1)),
                _ => {}
            }
        }
        out
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        self.weights = ZSet::new();
        let c = self.inner.init(p)?;
        Ok(self.apply(c))
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let c = self.inner.delta(p)?;
        Ok(self.apply(c))
    }

    pub fn bytes(&self) -> u64 {
        self.weights.bytes(self.inner.schema.len())
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.zset(&self.weights)
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.weights = r.zset()?;
        Ok(())
    }
}
