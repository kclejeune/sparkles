//! A subtree outside the incremental fragment (spec F13 §3.4). It is evaluated by the
//! query engine at the new snapshot of every pass whose changes match its footprint, and
//! its change is the difference from its previous output, which it keeps. The rest of
//! the circuit stays incremental, and the node is exact.

use super::super::footprint::{Footprint, Resolved};
use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::zset::{Changes, ZSet};
use crate::error::Result;
use spargebra::algebra::{GraphPattern, QueryDataset};

#[derive(Clone, Debug)]
pub struct Recompute {
    /// the subtree, with its active graph
    pub pattern: GraphPattern,
    pub footprint: Footprint,
    /// why the subtree is not incremental, for EXPLAIN
    pub reason: String,
    /// the query's dataset clause, for a whole query evaluated again
    pub dataset: Option<QueryDataset>,
    prev: ZSet,
    resolved: Option<Resolved>,
}

impl Recompute {
    pub fn new(pattern: GraphPattern, footprint: Footprint, reason: String) -> Recompute {
        Recompute {
            pattern,
            footprint,
            reason,
            dataset: None,
            prev: ZSet::new(),
            resolved: None,
        }
    }

    pub fn init(&mut self, p: &mut Pass, schema: &[u32]) -> Result<Changes> {
        let rows = p.evaluate(&self.pattern, schema, self.dataset.as_ref())?;
        self.prev = rows.iter().cloned().collect();
        Ok(rows)
    }

    pub fn delta(&mut self, p: &mut Pass, schema: &[u32]) -> Result<Changes> {
        if !p.full {
            if p.added.is_empty() && p.removed.is_empty() {
                return Ok(Vec::new());
            }
            if !self.resolved.as_ref().is_some_and(|r| r.current(&p.new)) {
                self.resolved = Some(self.footprint.resolve(&p.new));
            }
            let r = self.resolved.as_ref().expect("resolved above");
            if !r.touches(p.added.iter().chain(p.removed.iter())) {
                return Ok(Vec::new());
            }
        }
        let now: ZSet = p
            .evaluate(&self.pattern, schema, self.dataset.as_ref())?
            .into_iter()
            .collect();
        let d = now.minus(&self.prev);
        self.prev = now;
        Ok(d)
    }

    pub fn bytes(&self, width: usize) -> u64 {
        self.prev.bytes(width)
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.zset(&self.prev)
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.prev = r.zset()?;
        self.resolved = None;
        Ok(())
    }
}
