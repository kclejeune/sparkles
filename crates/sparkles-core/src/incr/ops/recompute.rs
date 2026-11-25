//! A subtree outside the incremental fragment (spec F13 §3.4). It is evaluated by the
//! query engine at the new snapshot of every pass whose changes match its footprint, and
//! its change is the difference from its previous output, which it keeps. The rest of
//! the circuit stays incremental, and the node is exact.

use super::super::footprint::{Footprint, Resolved};
use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::prov::{AChanges, Ann};
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
        let mut tracked = self.bytes(schema.len());
        let mark = p.output_lease_mark();
        let rows = p.evaluate(&self.pattern, schema, self.dataset.as_ref())?;
        let rows_leases = p.take_output_leases_since(mark);
        self.prev = ZSet::new();
        p.sync_op_state(&mut tracked, 0)?;
        let per_row = super::super::zset::row_bytes(schema.len());
        for (r, w) in &rows {
            p.tick()?;
            if *w != 0 && self.prev.get(r) == 0 {
                let next = (self.prev.len() as u64 + 1).saturating_mul(per_row);
                if next > tracked {
                    p.sync_op_state(&mut tracked, next)?;
                }
            }
            self.prev.add(r.clone(), *w);
        }
        p.sync_op_state(&mut tracked, self.bytes(schema.len()))?;
        for lease in rows_leases {
            p.push_output_lease(lease);
        }
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
        let mut tracked = self.bytes(schema.len());
        let mark = p.output_lease_mark();
        let raw = p.evaluate(&self.pattern, schema, self.dataset.as_ref())?;
        let raw_leases = p.take_output_leases_since(mark);
        let per_row = super::super::zset::row_bytes(schema.len());
        let mut now_lease = p.scratch_lease();
        let mut now = ZSet::new();
        for (r, w) in raw {
            p.tick()?;
            if w != 0 && now.get(&r) == 0 {
                let next = (now.len() as u64 + 1).saturating_mul(per_row);
                if next > now_lease.bytes() {
                    now_lease.try_set(next)?;
                }
            }
            now.add(r, w);
        }
        drop(raw_leases);
        let mut out_lease = p.scratch_lease();
        let mut d = Changes::new();
        for (r, w) in now.iter() {
            p.tick()?;
            let diff = w - self.prev.get(r);
            if diff != 0 {
                p.preadmit_changes_push(&d, schema.len(), &mut out_lease)?;
                d.push((r.clone(), diff));
            }
        }
        for (r, w) in self.prev.iter() {
            p.tick()?;
            if now.get(r) == 0 {
                p.preadmit_changes_push(&d, schema.len(), &mut out_lease)?;
                d.push((r.clone(), -w));
            }
        }
        let old_prev = std::mem::replace(&mut self.prev, now);
        drop(old_prev);
        let next_bytes = self.bytes(schema.len());
        if next_bytes > tracked {
            let need = next_bytes - tracked;
            let moved = p.move_scratch_to_pass(&mut now_lease, need);
            tracked = tracked.saturating_add(moved);
            if next_bytes > tracked {
                p.sync_op_state(&mut tracked, next_bytes)?;
            }
        } else {
            p.sync_op_state(&mut tracked, next_bytes)?;
        }
        drop(now_lease);
        p.push_output_lease(out_lease);
        Ok(d)
    }

    /// [`init`](Self::init) with annotations. The query engine does not report a row's
    /// derivations, so every derivation's atoms are unknown.
    pub fn init_a(&mut self, p: &mut Pass, schema: &[u32]) -> Result<AChanges> {
        let mode = p.prov;
        Ok(self
            .init(p, schema)?
            .into_iter()
            .map(|(r, w)| (r, w, Ann::unknown(mode, w)))
            .collect())
    }

    pub fn delta_a(&mut self, p: &mut Pass, schema: &[u32]) -> Result<AChanges> {
        let mode = p.prov;
        Ok(self
            .delta(p, schema)?
            .into_iter()
            .map(|(r, w)| (r, w, Ann::unknown(mode, w)))
            .collect())
    }

    pub fn bytes(&self, width: usize) -> u64 {
        self.prev.bytes(width)
    }

    pub fn clear(&mut self) {
        self.prev = ZSet::new();
        self.resolved = None;
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
