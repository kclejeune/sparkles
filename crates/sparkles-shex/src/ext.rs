//! Matching a shape that extends others (ShEx 2.2).
//!
//! The arcs of a node are partitioned over the shape's own triple expression and the
//! triple expressions of its ancestors, each ancestor once. Every part is matched by its
//! shape's expression alone; the other conjuncts of an ancestor's declaration must hold
//! for the arcs of its part and of its own ancestors. What no part takes is the
//! remainder: an outgoing arc that no triple constraint of any part has a predicate for
//! is forbidden when the extending shape is CLOSED, and an arc that a constraint could
//! match is left over only for an EXTRA predicate that no constraint matches for sure.
//! Leaving such an arc over is itself undecided, because it is only valid when the arc does
//! not match, so every partition that does so is at best `Unknown`.
//!
//! The partitions are searched depth first over the arcs that have a choice, each leaf
//! charged to the partition budget. A leaf is a conjunction of readings of the typing, so
//! the three-valued result of the search (the disjunction of its leaves) is monotone in
//! it, as [`crate::typing`] needs. A conjunct is satisfied by a subset of the arcs
//! without reading the typing for the node itself: references are unfolded in place (a
//! revisit of the same reference and arcs counts as satisfied, the greatest fixed point),
//! shapes are matched against a neighbourhood made of the subset, and only the values of
//! triple constraints read the typing, as everywhere else.

use crate::ir::{Dir, Frame, PairKind, Se, SeId, ShapeId, TcId, Tri};
use crate::matcher::{self, Budget};
use crate::neigh::Neigh;
use crate::semact::ActCtx;
use crate::typing::{Env, Read, Worker};
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use sparkles_core::id::Id;
use std::cell::RefCell;

/// How deep conjuncts may nest (references and shapes in conjuncts of ancestors).
const MAX_DEPTH: usize = 256;

/// An arc of the node: an outgoing one (`value` is the object) or an incoming one (the
/// subject).
#[derive(Clone, Copy, Debug)]
struct ArcAt {
    dir: Dir,
    pred: Id,
    value: Id,
}

/// Where an arc may go.
#[derive(Clone, Copy, Debug)]
enum Opt {
    /// part, and the entry of the part's shape (for the cap)
    Part(usize, usize),
    Remainder,
}

struct Choice {
    arc: u32,
    opts: SmallVec<[Opt; 4]>,
}

/// The state of one search over the partitions of a frame.
struct Search<'f> {
    frame: &'f Frame,
    /// the own shape, then the shape of each part of the frame
    shapes: Vec<ShapeId>,
    /// the arcs of each part
    assign: Vec<Vec<u32>>,
    counts: Vec<Vec<u64>>,
    caps: Vec<Vec<Option<u64>>>,
    choices: Vec<Choice>,
}

impl<'a> Env<'a> {
    /// The incoming arcs the extension machinery fetches: the inverse predicates of the
    /// schema's triple constraints.
    pub(crate) fn inverse_predicates(
        ir: &crate::ir::Ir,
        snap: &sparkles_core::store::Snapshot,
    ) -> Vec<Id> {
        let mut out: Vec<Id> = ir
            .shapes
            .iter()
            .flat_map(|s| s.tcs.iter())
            .filter(|tc| tc.dir == Dir::In)
            .filter_map(|tc| snap.lookup_iri(&tc.pred))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Does `node` match the shape `sid` that extends others, under the typing `read`?
    pub(crate) fn ext_shape(
        &self,
        sid: ShapeId,
        node: Id,
        read: &Read<'_>,
        w: &mut Worker<'_>,
    ) -> anyhow::Result<Tri> {
        let mut arcs = Vec::new();
        self.data.scan_out_edges(node, |p, o| {
            arcs.push(ArcAt {
                dir: Dir::Out,
                pred: p,
                value: o,
            });
            true
        })?;
        for &p in &self.ext_inverse {
            self.data.scan_subjects(p, node, |s| {
                arcs.push(ArcAt {
                    dir: Dir::In,
                    pred: p,
                    value: s,
                });
                true
            })?;
        }
        let all: Vec<u32> = (0..arcs.len() as u32).collect();
        let mut cx = Cx {
            env: self,
            node,
            arcs,
            read,
            acts: &mut w.cx,
            budget: Budget::new(self.max_partitions),
            error: RefCell::new(None),
            parts: FxHashMap::default(),
            cons: FxHashMap::default(),
            active: Vec::new(),
            depth: 0,
        };
        let r = cx.shape_in(sid, &all);
        match cx.error.take() {
            Some(e) => Err(e),
            None => r,
        }
    }
}

struct Cx<'e, 'a, 'w> {
    env: &'e Env<'a>,
    node: Id,
    arcs: Vec<ArcAt>,
    read: &'e Read<'e>,
    acts: &'e mut ActCtx<'w>,
    budget: Budget,
    /// the first error a value read hit (the read itself answers `False`)
    error: RefCell<Option<anyhow::Error>>,
    /// matches of a shape's own expression against a set of arcs (closed or not)
    parts: FxHashMap<(ShapeId, Vec<u32>, bool), Tri>,
    /// conjuncts against a set of arcs
    cons: FxHashMap<(SeId, Vec<u32>), Tri>,
    /// references being unfolded
    active: Vec<(PairKind, Vec<u32>)>,
    depth: usize,
}

impl Cx<'_, '_, '_> {
    /// Does shape `sid` match the node's arcs `set`?
    fn shape_in(&mut self, sid: ShapeId, set: &[u32]) -> anyhow::Result<Tri> {
        let env = self.env;
        match &env.ir.shapes[sid.index()].ext {
            None => self.plain(sid, set, true),
            Some(plan) => {
                let mut res = Tri::False;
                for frame in &plan.frames {
                    res = res.or(self.frame(sid, frame, set)?);
                    if res == Tri::True {
                        break;
                    }
                }
                Ok(res)
            }
        }
    }

    /// Does the expression of shape `sid` alone match `set`; `closed` applies the shape's
    /// CLOSED to the arcs of `set`.
    fn plain(&mut self, sid: ShapeId, set: &[u32], closed: bool) -> anyhow::Result<Tri> {
        let key = (sid, set.to_vec(), closed);
        if let Some(&t) = self.parts.get(&key) {
            return Ok(t);
        }
        let env = self.env;
        let shape = &env.ir.shapes[sid.index()];
        let ids = &env.plan.preds[sid.index()];
        let mut neigh = Neigh {
            node: self.node,
            preds: ids.iter().map(|i| i.unwrap_or(Id::UNDEF)).collect(),
            values: vec![Vec::new(); shape.preds.len()],
            closed_violation: None,
        };
        let closed = closed && shape.closed;
        for &a in set {
            let arc = self.arcs[a as usize];
            let mut hit = false;
            for (e, (_, dir, _)) in shape.preds.iter().enumerate() {
                if *dir == arc.dir && neigh.preds[e] == arc.pred && arc.pred != Id::UNDEF {
                    neigh.values[e].push(arc.value);
                    hit = true;
                }
            }
            if closed && !hit && arc.dir == Dir::Out && neigh.closed_violation.is_none() {
                neigh.closed_violation = Some((arc.pred, arc.value));
            }
        }
        let read = self.read;
        let error = &self.error;
        let value = |v: Id, tc: TcId| env.value(shape, tc, v, read, error);
        let t = matcher::matches(shape, &neigh, &value, &mut self.budget, self.acts)?;
        self.parts.insert(key, t);
        Ok(t)
    }

    /// The entry of `shape` that `arc` can match, and whether its value satisfies the
    /// entry's constraints (the best of them).
    fn entry(&self, sid: ShapeId, arc: &ArcAt) -> Option<(usize, Tri)> {
        let env = self.env;
        let shape = &env.ir.shapes[sid.index()];
        let ids = &env.plan.preds[sid.index()];
        for (e, (_, dir, tcs)) in shape.preds.iter().enumerate() {
            if *dir == arc.dir && ids[e] == Some(arc.pred) {
                let mut t = Tri::False;
                for &tc in tcs {
                    let c = if shape.tcs[tc.index()].value.is_none() {
                        Tri::True
                    } else {
                        env.value(shape, tc, arc.value, self.read, &self.error)
                    };
                    t = t.or(c);
                }
                return Some((e, t));
            }
        }
        None
    }

    /// Does `sid`, extending others, match `set` under one reading of its ancestors?
    fn frame(&mut self, sid: ShapeId, frame: &Frame, set: &[u32]) -> anyhow::Result<Tri> {
        let env = self.env;
        let own = &env.ir.shapes[sid.index()];
        let mut shapes = vec![sid];
        shapes.extend(frame.parts.iter().map(|p| p.shape));
        let extra: Vec<Id> = own
            .extra
            .iter()
            .filter_map(|p| env.snap.lookup_iri(p))
            .collect();
        let caps: Vec<Vec<Option<u64>>> = shapes
            .iter()
            .map(|&s| {
                let shape = &env.ir.shapes[s.index()];
                let ids = &env.plan.preds[s.index()];
                shape
                    .preds
                    .iter()
                    .enumerate()
                    .map(|(e, (_, _, tcs))| {
                        if ids[e].is_some_and(|id| extra.contains(&id)) {
                            return None;
                        }
                        tcs.iter().try_fold(0u64, |sum, tc| {
                            shape.max_occ[tc.index()].map(|m| sum.saturating_add(m))
                        })
                    })
                    .collect()
            })
            .collect();
        let mut st = Search {
            frame,
            shapes,
            assign: vec![Vec::new(); frame.parts.len() + 1],
            counts: caps.iter().map(|c| vec![0; c.len()]).collect(),
            caps,
            choices: Vec::new(),
        };
        for &a in set {
            let arc = self.arcs[a as usize];
            let mut opts: SmallVec<[Opt; 4]> = SmallVec::new();
            let (mut matchable, mut sure) = (false, false);
            for (p, &s) in st.shapes.iter().enumerate() {
                if let Some((e, t)) = self.entry(s, &arc) {
                    matchable = true;
                    if t != Tri::False {
                        opts.push(Opt::Part(p, e));
                        sure |= t == Tri::True;
                    }
                }
            }
            if !matchable {
                if own.closed && arc.dir == Dir::Out {
                    return Ok(Tri::False);
                }
                continue;
            }
            let extra = extra.contains(&arc.pred);
            if opts.is_empty() {
                if extra {
                    continue;
                }
                return Ok(Tri::False);
            }
            if extra && !sure {
                opts.push(Opt::Remainder);
            }
            match opts[..] {
                [Opt::Part(p, e)] => {
                    if over(&st.caps, &st.counts, p, e) {
                        return Ok(Tri::False);
                    }
                    st.counts[p][e] += 1;
                    st.assign[p].push(a);
                }
                _ => st.choices.push(Choice { arc: a, opts }),
            }
        }
        if let Some(e) = self.error.borrow_mut().take() {
            return Err(e);
        }
        self.dfs(&mut st)
    }

    /// The disjunction of the leaves under the arcs that have a choice, found depth first
    /// with an explicit stack, since a node can have tens of thousands of such arcs. The
    /// first descent is free. Each leaf, each option abandoned on backtracking and each
    /// option that its part's cap rules out is charged to the partition budget, so a
    /// search that never reaches a leaf is bounded as well, and the timeout and
    /// cancellation are checked as it goes.
    fn dfs(&mut self, st: &mut Search<'_>) -> anyhow::Result<Tri> {
        /// Steps between two checks of the timeout and cancellation.
        const CHECK_EVERY: usize = 4096;
        /// The state of the search at one choice.
        struct Level {
            /// the next option of the choice to try
            next: usize,
            /// the entry of the part the current option filled
            applied: Option<(usize, usize)>,
            /// whether the current option leaves the arc over
            remainder: bool,
        }
        let n = st.choices.len();
        if n == 0 {
            self.budget.charge(1)?;
            return self.leaf(st);
        }
        let mut res = Tri::False;
        let mut stack = vec![Level {
            next: 0,
            applied: None,
            remainder: false,
        }];
        // the arcs the current partition leaves over: a leaf under one is undecided at
        // best, because the arc may match after all
        let mut left_over = 0usize;
        let mut steps = 0usize;
        while !stack.is_empty() {
            steps += 1;
            if steps.is_multiple_of(CHECK_EVERY) {
                self.env.limits.check()?;
            }
            let i = stack.len() - 1;
            let level = &mut stack[i];
            // back out of the option this level tried last
            if let Some((p, e)) = level.applied.take() {
                st.counts[p][e] -= 1;
                st.assign[p].pop();
                self.budget.charge(1)?;
            }
            if std::mem::take(&mut level.remainder) {
                left_over -= 1;
                self.budget.charge(1)?;
            }
            let Some(&opt) = st.choices[i].opts.get(level.next) else {
                stack.pop();
                continue;
            };
            level.next += 1;
            match opt {
                Opt::Part(p, e) => {
                    if over(&st.caps, &st.counts, p, e) {
                        self.budget.charge(1)?;
                        continue;
                    }
                    st.counts[p][e] += 1;
                    st.assign[p].push(st.choices[i].arc);
                    level.applied = Some((p, e));
                }
                Opt::Remainder => {
                    left_over += 1;
                    level.remainder = true;
                }
            }
            if i + 1 < n {
                stack.push(Level {
                    next: 0,
                    applied: None,
                    remainder: false,
                });
                continue;
            }
            self.budget.charge(1)?;
            let mut leaf = self.leaf(st)?;
            if left_over > 0 {
                leaf = leaf.and(Tri::Unknown);
            }
            res = res.or(leaf);
            if res == Tri::True {
                break;
            }
        }
        Ok(res)
    }

    /// One partition: every part matches, and every ancestor's other conjuncts hold for
    /// its arcs and its ancestors'.
    fn leaf(&mut self, st: &Search<'_>) -> anyhow::Result<Tri> {
        let sets: Vec<Vec<u32>> = st
            .assign
            .iter()
            .map(|v| {
                let mut v = v.clone();
                v.sort_unstable();
                v
            })
            .collect();
        let mut r = Tri::True;
        for (i, set) in sets.iter().enumerate() {
            r = r.and(self.plain(st.shapes[i], set, false)?);
            if r == Tri::False {
                return Ok(r);
            }
        }
        for (i, part) in st.frame.parts.iter().enumerate() {
            if part.constraints.is_empty() {
                continue;
            }
            let mut sub = sets[i + 1].clone();
            for &j in &part.below {
                sub.extend(&sets[j + 1]);
            }
            sub.sort_unstable();
            sub.dedup();
            for &c in &part.constraints {
                r = r.and(self.sat(c, &sub)?);
                if r == Tri::False {
                    return Ok(r);
                }
            }
        }
        Ok(r)
    }

    /// Does the node satisfy `se` when its neighbourhood is the arcs `set`?
    fn sat(&mut self, se: SeId, set: &[u32]) -> anyhow::Result<Tri> {
        let key = (se, set.to_vec());
        if let Some(&t) = self.cons.get(&key) {
            return Ok(t);
        }
        if self.depth >= MAX_DEPTH {
            anyhow::bail!("shapes extend each other too deeply to match");
        }
        self.depth += 1;
        let r = self.sat_inner(se, set);
        self.depth -= 1;
        let t = r?;
        // a result that rests on an assumption about a reference being unfolded is not
        // final
        if self.active.is_empty() {
            self.cons.insert(key, t);
        }
        Ok(t)
    }

    fn sat_inner(&mut self, se: SeId, set: &[u32]) -> anyhow::Result<Tri> {
        let env = self.env;
        Ok(match &env.ir.ses[se.index()] {
            Se::Nc(n) => Tri::from(env.nc_check(*n, self.node)?),
            Se::And(xs) => {
                let mut r = Tri::True;
                for &x in xs {
                    r = r.and(self.sat(x, set)?);
                    if r == Tri::False {
                        break;
                    }
                }
                r
            }
            Se::Or(xs) => {
                let mut r = Tri::False;
                for &x in xs {
                    r = r.or(self.sat(x, set)?);
                    if r == Tri::True {
                        break;
                    }
                }
                r
            }
            Se::Not(x) => !self.sat(*x, set)?,
            Se::Ref(k) => {
                if self.active.iter().any(|(a, s)| a == k && s == set) {
                    return Ok(Tri::True);
                }
                self.active.push((*k, set.to_vec()));
                let r = self.sat(env.ir.pairs[k.index()].se, set);
                self.active.pop();
                r?
            }
            Se::Shape(s) => self.shape_in(*s, set)?,
            Se::External => anyhow::bail!("an external shape has no definition"),
        })
    }
}

/// Is entry `e` of part `p` full?
fn over(caps: &[Vec<Option<u64>>], counts: &[Vec<u64>], p: usize, e: usize) -> bool {
    caps[p][e].is_some_and(|c| counts[p][e] >= c)
}
