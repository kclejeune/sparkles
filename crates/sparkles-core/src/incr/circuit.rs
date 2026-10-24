//! A compiled circuit with its state: the result it keeps current, the commit it
//! reflects, and the passes that move it (spec F13 §3.3, §3.4).

use super::compile::{Compiled, Kind};
use super::footprint::{Footprint, Resolved};
use super::ops::{Node, PlanNode};
use super::pass::{Pass, PassStats, Vis};
use super::persist::{Reader, Writer};
use super::terms::Terms;
use super::zset::{Changes, Row, ZSet, consolidate};
use crate::access::GraphAccess;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::store::{ChangesOptions, Snapshot, Store};
use crate::task::Control;
use oxrdf::Term;
use rustc_hash::FxHashMap;
use std::sync::{Arc, OnceLock};

/// The most net changes a pass applies; more are cheaper to evaluate from scratch.
pub const MAX_PASS_CHANGES: usize = 1_000_000;

/// The net changes from one commit to a later one, as ids of the later snapshot.
pub struct DeltaSet {
    pub after: u64,
    /// the snapshot the changes lead to
    pub snapshot: Arc<Snapshot>,
    pub added: Vec<[Id; 4]>,
    pub removed: Vec<[Id; 4]>,
    /// `false` when some commit's changes are not known (a large bulk commit, a gap the
    /// change feed cannot bridge, terms the generation lacks): passes evaluate again
    pub complete: bool,
    old: OnceLock<std::result::Result<Arc<Snapshot>, String>>,
}

impl DeltaSet {
    /// The net changes after commit `after` up to the store's head.
    pub fn read(store: &Store, after: u64, control: &Control) -> DeltaSet {
        let snap = store.snapshot();
        let mut d = DeltaSet {
            after,
            snapshot: snap.clone(),
            added: Vec::new(),
            removed: Vec::new(),
            complete: true,
            old: OnceLock::new(),
        };
        if after >= snap.commit {
            return d;
        }
        let mut net: FxHashMap<[Id; 4], i64> = FxHashMap::default();
        let mut at = after;
        let o = ChangesOptions {
            max_commits: 1000,
            cancel: Some(control.cancel.flag()),
            deadline: control.deadline,
            ..Default::default()
        };
        while at < snap.commit {
            let page = match store.commit_deltas(at, &o) {
                Ok(p) => p,
                Err(_) => {
                    d.complete = false;
                    break;
                }
            };
            if page.snapshot.generation.uid != snap.generation.uid || page.commits.is_empty() {
                d.complete = false;
                break;
            }
            for c in &page.commits {
                if c.seq > snap.commit {
                    break;
                }
                if !c.complete {
                    d.complete = false;
                }
                for q in &c.removed {
                    *net.entry(*q).or_insert(0) -= 1;
                }
                for q in &c.added {
                    *net.entry(*q).or_insert(0) += 1;
                }
                at = c.seq;
            }
            if !d.complete || net.len() > MAX_PASS_CHANGES {
                d.complete = false;
                break;
            }
        }
        if d.complete {
            for (q, n) in net {
                match n.cmp(&0) {
                    std::cmp::Ordering::Greater => d.added.push(q),
                    std::cmp::Ordering::Less => d.removed.push(q),
                    std::cmp::Ordering::Equal => {}
                }
            }
        }
        d
    }

    /// A set without changes that leads to `snap` (a build).
    pub fn at(snap: Arc<Snapshot>) -> DeltaSet {
        DeltaSet {
            after: snap.commit,
            snapshot: snap,
            added: Vec::new(),
            removed: Vec::new(),
            complete: true,
            old: OnceLock::new(),
        }
    }

    /// The snapshot before the changes, built on first use.
    pub fn old(&self) -> Result<Arc<Snapshot>> {
        self.old
            .get_or_init(|| {
                self.snapshot
                    .before_changes(&self.added, &self.removed, self.after)
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            })
            .clone()
            .map_err(Error::Corrupt)
    }
}

/// What a pass did to a circuit.
pub enum Advance {
    /// the changes do not touch the circuit's footprint (or there were none)
    Skipped,
    /// the circuit's result changed by these rows (possibly none)
    Changed(Changes),
    /// the circuit was evaluated again; its result changed by these rows
    Reset(Changes),
}

#[derive(Clone)]
pub struct Circuit {
    pub kind: Kind,
    pub vars: Vec<String>,
    pub root: Node,
    pub footprint: Footprint,
    resolved: Option<Resolved>,
    pub terms: Terms,
    /// the result, with multiplicities
    pub result: ZSet,
    /// the commit the result reflects
    pub applied: u64,
    /// the snapshot of the latest pass
    pub snap: Option<Arc<Snapshot>>,
    /// whose view of the dataset the circuit reads
    pub access: Option<Arc<GraphAccess>>,
    /// counters of the latest pass
    pub last: PassStats,
    pub max_memory_bytes: Option<u64>,
}

impl Circuit {
    pub fn new(c: Compiled, access: Option<Arc<GraphAccess>>) -> Circuit {
        Circuit {
            kind: c.kind,
            vars: c.vars,
            root: c.root,
            footprint: c.footprint,
            resolved: None,
            terms: Terms::default(),
            result: ZSet::new(),
            applied: 0,
            snap: None,
            access,
            last: PassStats::default(),
            max_memory_bytes: None,
        }
    }

    /// The names of the result's columns.
    pub fn columns(&self) -> Vec<String> {
        self.root
            .schema
            .iter()
            .map(|&v| self.vars[v as usize].clone())
            .collect()
    }

    pub fn plan(&self) -> PlanNode {
        self.root.plan(&self.vars)
    }

    /// The approximate memory of the state and the result.
    pub fn bytes(&self) -> u64 {
        self.root.bytes() + self.result.bytes(self.root.schema.len()) + self.terms.bytes()
    }

    fn pass<'a>(
        &'a mut self,
        snap: Arc<Snapshot>,
        old: Option<Arc<Snapshot>>,
        added: &'a [[Id; 4]],
        removed: &'a [[Id; 4]],
        vis: &'a Vis,
        control: &'a Control,
    ) -> (Pass<'a>, &'a mut Node) {
        let mut p = Pass::new(
            snap,
            old,
            added,
            removed,
            &mut self.terms,
            vis,
            self.access.clone(),
            control,
            &self.vars,
        );
        p.max_memory_bytes = self.max_memory_bytes;
        (p, &mut self.root)
    }

    /// Evaluate the circuit from scratch at `snap`.
    pub fn build(&mut self, snap: Arc<Snapshot>, control: &Control) -> Result<()> {
        self.terms = Terms::default();
        let vis = Vis::new(self.access.as_ref(), &snap);
        let (mut p, root) = self.pass(snap.clone(), None, &[], &[], &vis, control);
        let out = root.init(&mut p)?;
        let stats = p.stats;
        drop(p);
        self.result = out.into_iter().collect();
        self.applied = snap.commit;
        self.snap = Some(snap);
        self.last = stats;
        Ok(())
    }

    /// Move the state to the id space of `to` (a new generation, or terms the store
    /// now holds that the circuit held as local ids), through the checkpoint encoding.
    fn remap(&mut self, from: &Arc<Snapshot>, to: &Arc<Snapshot>) -> Result<()> {
        let body = self.encode(from)?;
        self.decode(&body, to)
    }

    /// The state as checkpoint bytes, its terms read from `snap`.
    pub fn encode(&self, snap: &Snapshot) -> Result<Vec<u8>> {
        let mut w = Writer::new(snap, &self.terms);
        w.zset(&self.result)?;
        self.root.save(&mut w)?;
        Ok(w.finish())
    }

    /// Load a state written by [`encode`](Self::encode) into the id space of `snap`.
    pub fn decode(&mut self, body: &[u8], snap: &Arc<Snapshot>) -> Result<()> {
        let mut terms = Terms::default();
        let mut r = Reader::new(body, snap, &mut terms)?;
        self.result = r.zset()?;
        self.root.load(&mut r)?;
        if !r.done() {
            return Err(Error::Corrupt("a view's state has trailing bytes".into()));
        }
        self.terms = terms;
        self.snap = Some(snap.clone());
        self.resolved = None;
        Ok(())
    }

    /// Evaluate again at `d.snapshot` and return the change of the result.
    pub fn reset(&mut self, d: &DeltaSet, control: &Control) -> Result<Changes> {
        let snap = d.snapshot.clone();
        // the old result in the new id space, to compare with
        if let Some(prev) = self.snap.clone()
            && (prev.generation.uid != snap.generation.uid || !self.terms.is_empty())
        {
            self.remap(&prev, &snap)?;
        }
        let before = std::mem::take(&mut self.result);
        let old_terms = self.terms.clone();
        self.build(snap.clone(), control)?;
        // local ids of the old result must mean what the new ones mean
        let mut before_keys: ZSet = ZSet::new();
        for (r, w) in before.iter() {
            let row: Row = r
                .iter()
                .map(|&i| {
                    if i.tag() == crate::id::Tag::Local {
                        old_terms
                            .term(&snap, i)
                            .map_or(Id::UNDEF, |t| self.terms.intern(&snap, &t))
                    } else {
                        i
                    }
                })
                .collect();
            before_keys.add(row, w);
        }
        Ok(self.result.minus(&before_keys))
    }

    /// Apply the net changes `d` (whose `after` must be the circuit's commit).
    pub fn advance(&mut self, d: &DeltaSet, control: &Control) -> Result<Advance> {
        let snap = d.snapshot.clone();
        if !d.complete {
            return Ok(Advance::Reset(self.reset(d, control)?));
        }
        if let Some(prev) = self.snap.clone() {
            if prev.generation.uid != snap.generation.uid {
                self.remap(&prev, &snap)?;
            } else if snap.dvocab_len > prev.dvocab_len
                && !self.terms.absorbed(&snap, prev.dvocab_len).is_empty()
            {
                // the store now holds terms the circuit kept as local ids
                self.remap(&prev, &snap)?;
            }
        }
        let vis = Vis::new(self.access.as_ref(), &snap);
        let (added, removed): (Vec<[Id; 4]>, Vec<[Id; 4]>) = if vis.everything() {
            (d.added.clone(), d.removed.clone())
        } else {
            (
                d.added
                    .iter()
                    .filter(|q| vis.visible(&snap, q))
                    .copied()
                    .collect(),
                d.removed
                    .iter()
                    .filter(|q| vis.visible(&snap, q))
                    .copied()
                    .collect(),
            )
        };
        if !self.resolved.as_ref().is_some_and(|r| r.current(&snap)) {
            self.resolved = Some(self.footprint.resolve(&snap));
        }
        let touched = self
            .resolved
            .as_ref()
            .expect("resolved above")
            .touches(added.iter().chain(removed.iter()));
        if !touched {
            self.applied = snap.commit;
            self.snap = Some(snap);
            return Ok(Advance::Skipped);
        }
        let old = d.old()?;
        let (mut p, root) = self.pass(snap.clone(), Some(old), &added, &removed, &vis, control);
        let out = root.delta(&mut p)?;
        let stats = p.stats;
        drop(p);
        let out = consolidate(out);
        self.result.apply(&out);
        self.applied = snap.commit;
        self.snap = Some(snap);
        self.last = stats;
        Ok(Advance::Changed(out))
    }

    /// The terms of rows of the result.
    pub fn decode_rows(&self, rows: &[(Row, i64)]) -> Vec<(Vec<Option<Term>>, i64)> {
        let Some(snap) = &self.snap else {
            return Vec::new();
        };
        rows.iter()
            .map(|(r, w)| (r.iter().map(|&i| self.terms.term(snap, i)).collect(), *w))
            .collect()
    }

    /// The whole result as terms.
    pub fn rows(&self) -> Vec<(Vec<Option<Term>>, i64)> {
        let rows: Vec<(Row, i64)> = self.result.iter().map(|(r, w)| (r.clone(), w)).collect();
        self.decode_rows(&rows)
    }
}
