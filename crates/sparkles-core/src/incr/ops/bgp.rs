//! Basic graph patterns: joins of triple patterns, maintained without state.
//!
//! The change of `P1 ⋈ … ⋈ Pn` is the sum over `i` of `P1..P(i−1)` at the new snapshot,
//! joined with the change of `Pi`, joined with `P(i+1)..Pn` at the old snapshot. Each
//! term starts from the changed quads that match `Pi` and probes the other patterns
//! through the store's permutations, choosing at each step the pattern with the most
//! positions bound. A pattern's matches are never copied.
//!
//! Blank nodes of the patterns and the inner variables of rewritten paths are hidden
//! slots: they join like variables and are projected away, so a solution's weight
//! counts its ways of matching.

use super::super::pass::{GraphSel, Pass, Which};
use super::super::prov::{AChanges, Ann};
use super::super::zset::{Changes, Row};
use crate::error::Result;
use crate::id::Id;
use oxrdf::Term;
use spargebra::algebra::GraphPattern;

/// A position of a pattern.
#[derive(Clone, Debug)]
pub enum Slot {
    Var(usize),
    Const(Term),
}

/// The graph of a pattern.
#[derive(Clone, Debug)]
pub enum GSlot {
    Default,
    Const(Term),
    Var(usize),
}

#[derive(Clone, Debug)]
pub struct Pat {
    pub s: Slot,
    pub p: Slot,
    pub o: Slot,
    pub g: GSlot,
}

#[derive(Clone, Debug)]
pub struct Bgp {
    pub pats: Vec<Pat>,
    /// the number of slots (variables, blank nodes and hidden variables)
    pub slots: usize,
    /// the slot of each schema column
    pub out: Vec<usize>,
    /// the slot names, for EXPLAIN
    pub names: Vec<String>,
    /// the patterns as algebra, which the query engine evaluates to build the node
    pub source: GraphPattern,
}

/// A position resolved at one pass.
#[derive(Clone, Copy, Debug)]
enum R {
    Var(usize),
    Id(Id),
    /// a constant no quad holds
    Never,
}

#[derive(Clone, Copy, Debug)]
enum RG {
    Default,
    Union,
    Id(Id),
    Var(usize),
    Never,
}

#[derive(Clone, Copy, Debug)]
struct RPat {
    s: R,
    p: R,
    o: R,
    g: RG,
}

impl RPat {
    fn never(&self) -> bool {
        matches!(self.s, R::Never)
            || matches!(self.p, R::Never)
            || matches!(self.o, R::Never)
            || matches!(self.g, RG::Never)
    }

    /// The positions bound under binding `b`.
    fn bound(&self, b: &[Id]) -> usize {
        let v = |r: R| match r {
            R::Id(_) | R::Never => 1,
            R::Var(s) => !b[s].is_undef() as usize,
        };
        let g = match self.g {
            RG::Id(_) => 1,
            RG::Var(s) => !b[s].is_undef() as usize,
            _ => 0,
        };
        // a bound subject or object narrows more than a bound predicate
        v(self.s) * 3 + v(self.o) * 3 + v(self.p) * 2 + g
    }

    /// Bind quad `q` into `b`; `false` when it does not match.
    fn bind(&self, q: &[Id; 4], b: &mut [Id]) -> bool {
        let mut one = |r: R, x: Id| -> bool {
            match r {
                R::Never => false,
                R::Id(c) => c == x,
                R::Var(s) => {
                    if b[s].is_undef() {
                        b[s] = x;
                        true
                    } else {
                        b[s] == x
                    }
                }
            }
        };
        if !(one(self.s, q[0]) && one(self.p, q[1]) && one(self.o, q[2])) {
            return false;
        }
        match self.g {
            RG::Never => false,
            RG::Default | RG::Union => q[3] == Id::DEFAULT_GRAPH,
            RG::Id(g) => q[3] == g,
            RG::Var(s) => {
                if q[3] == Id::DEFAULT_GRAPH {
                    false
                } else if b[s].is_undef() {
                    b[s] = q[3];
                    true
                } else {
                    b[s] == q[3]
                }
            }
        }
    }
}

impl Bgp {
    fn resolve(&self, p: &Pass) -> Vec<RPat> {
        let r = |s: &Slot| match s {
            Slot::Var(v) => R::Var(*v),
            Slot::Const(t) => p.constant(t).map_or(R::Never, R::Id),
        };
        let union = p.new.union_default_graph;
        self.pats
            .iter()
            .map(|pt| RPat {
                s: r(&pt.s),
                p: r(&pt.p),
                o: r(&pt.o),
                g: match &pt.g {
                    GSlot::Default if union => RG::Union,
                    GSlot::Default => RG::Default,
                    GSlot::Const(t) => p.constant(t).map_or(RG::Never, RG::Id),
                    GSlot::Var(v) => RG::Var(*v),
                },
            })
            .collect()
    }

    /// Visit the quads of snapshot `w` that match `rp` under binding `b`; in the union
    /// default graph, each triple once with the default graph's id.
    fn scan(
        p: &Pass,
        w: Which,
        rp: &RPat,
        b: &[Id],
        f: &mut dyn FnMut([Id; 4]) -> Result<()>,
    ) -> Result<()> {
        if rp.never() {
            return Ok(());
        }
        let val = |r: R| match r {
            R::Id(c) => Some(c),
            R::Var(s) if !b[s].is_undef() => Some(b[s]),
            _ => None,
        };
        let (s, pr, o) = (val(rp.s), val(rp.p), val(rp.o));
        match rp.g {
            RG::Never => Ok(()),
            RG::Union => {
                let mut seen = rustc_hash::FxHashSet::default();
                p.scan(w, s, pr, o, GraphSel::Named, &mut |q| {
                    if seen.insert([q[0], q[1], q[2]]) {
                        f([q[0], q[1], q[2], Id::DEFAULT_GRAPH])?;
                    }
                    Ok(true)
                })
            }
            g => {
                let sel = match g {
                    RG::Default => GraphSel::Default,
                    RG::Id(x) => GraphSel::Exact(x),
                    RG::Var(v) if !b[v].is_undef() => GraphSel::Exact(b[v]),
                    _ => GraphSel::Named,
                };
                p.scan(w, s, pr, o, sel, &mut |q| {
                    f(q)?;
                    Ok(true)
                })
            }
        }
    }

    /// Extend binding `b` by the patterns in `rest`, each read at `which(j)`.
    #[allow(clippy::too_many_arguments)]
    fn extend(
        &self,
        p: &mut Pass,
        rps: &[RPat],
        which: &dyn Fn(usize) -> Which,
        rest: &mut Vec<usize>,
        b: &[Id],
        w: i64,
        out: &mut Changes,
    ) -> Result<()> {
        p.tick()?;
        if rest.is_empty() {
            let row: Row = self.out.iter().map(|&s| b[s]).collect();
            out.push((row, w));
            return Ok(());
        }
        let (k, _) = rest
            .iter()
            .enumerate()
            .max_by_key(|(_, j)| rps[**j].bound(b))
            .expect("rest is not empty");
        let j = rest.swap_remove(k);
        let mut found: Vec<Vec<Id>> = Vec::new();
        Self::scan(p, which(j), &rps[j], b, &mut |q| {
            let mut nb = b.to_vec();
            if rps[j].bind(&q, &mut nb) {
                found.push(nb);
            }
            Ok(())
        })?;
        for nb in found {
            self.extend(p, rps, which, rest, &nb, w, out)?;
        }
        rest.push(j);
        let last = rest.len() - 1;
        rest.swap(k, last);
        Ok(())
    }

    /// Visit the matches of `rp` under `b` at snapshot `w` with the stored quads behind
    /// each: the quad itself, or for the union default graph every named-graph quad that
    /// holds the triple, so that a witness names stored quads (spec F13 §5.1).
    fn scan_sources(
        p: &Pass,
        w: Which,
        rp: &RPat,
        b: &[Id],
        f: &mut dyn FnMut([Id; 4], Vec<[Id; 4]>),
    ) -> Result<()> {
        if !matches!(rp.g, RG::Union) {
            return Self::scan(p, w, rp, b, &mut |q| {
                f(q, vec![q]);
                Ok(())
            });
        }
        if rp.never() {
            return Ok(());
        }
        let val = |r: R| match r {
            R::Id(c) => Some(c),
            R::Var(s) if !b[s].is_undef() => Some(b[s]),
            _ => None,
        };
        let mut index: rustc_hash::FxHashMap<[Id; 3], usize> = Default::default();
        let mut groups: Vec<([Id; 3], Vec<[Id; 4]>)> = Vec::new();
        p.scan(
            w,
            val(rp.s),
            val(rp.p),
            val(rp.o),
            GraphSel::Named,
            &mut |q| {
                let t = [q[0], q[1], q[2]];
                match index.get(&t) {
                    Some(&i) => groups[i].1.push(q),
                    None => {
                        index.insert(t, groups.len());
                        groups.push((t, vec![q]));
                    }
                }
                Ok(true)
            },
        )?;
        for (t, qs) in groups {
            f([t[0], t[1], t[2], Id::DEFAULT_GRAPH], qs);
        }
        Ok(())
    }

    /// [`extend`](Self::extend) that keeps the stored quads each derivation matched, for
    /// its annotation (spec F13 §5). `w` is the derivation's weight and `sign` the sign
    /// of its annotation, which differ only for a quad of the union default graph whose
    /// triple stays present.
    #[allow(clippy::too_many_arguments)]
    fn extend_a(
        &self,
        p: &mut Pass,
        rps: &[RPat],
        which: &dyn Fn(usize) -> Which,
        rest: &mut Vec<usize>,
        b: &[Id],
        w: i64,
        sign: i64,
        trail: &mut Vec<Vec<[Id; 4]>>,
        out: &mut AChanges,
    ) -> Result<()> {
        p.tick()?;
        if rest.is_empty() {
            let row: Row = self.out.iter().map(|&s| b[s]).collect();
            let a = if trail.iter().all(|e| e.len() == 1) {
                let flat: Vec<[Id; 4]> = trail.iter().map(|e| e[0]).collect();
                p.fact_ann(&flat)
            } else {
                // a triple of the union default graph counts each graph that holds it
                let mut acc = Ann::one(p.prov);
                for e in trail.iter() {
                    let mut sum = Ann::zero(p.prov);
                    for q in e {
                        sum.add_assign(&p.fact_ann(&[*q]));
                    }
                    acc = acc.mul(&sum);
                }
                acc
            };
            out.push((row, w, a.scaled(sign)));
            return Ok(());
        }
        let (k, _) = rest
            .iter()
            .enumerate()
            .max_by_key(|(_, j)| rps[**j].bound(b))
            .expect("rest is not empty");
        let j = rest.swap_remove(k);
        let mut found: Vec<(Vec<Id>, Vec<[Id; 4]>)> = Vec::new();
        Self::scan_sources(p, which(j), &rps[j], b, &mut |q, src| {
            let mut nb = b.to_vec();
            if rps[j].bind(&q, &mut nb) {
                found.push((nb, src));
            }
        })?;
        for (nb, src) in found {
            trail.push(src);
            self.extend_a(p, rps, which, rest, &nb, w, sign, trail, out)?;
            trail.pop();
        }
        rest.push(j);
        let last = rest.len() - 1;
        rest.swap(k, last);
        Ok(())
    }

    /// [`init`](Self::init) with annotations. The query engine does not report the
    /// quads behind a row, so the patterns are matched here, at the new snapshot.
    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let rps = self.resolve(p);
        let mut out = AChanges::new();
        let mut rest: Vec<usize> = (0..rps.len()).collect();
        let b = vec![Id::UNDEF; self.slots];
        self.extend_a(
            p,
            &rps,
            &|_| Which::New,
            &mut rest,
            &b,
            1,
            1,
            &mut Vec::new(),
            &mut out,
        )?;
        Ok(out)
    }

    /// [`delta`](Self::delta) with annotations: each derivation's annotation names the
    /// changed quad and the quads it joined. A pattern over the union default graph is
    /// seeded by every changed named-graph quad, whose annotation moves, and the triple's
    /// change of presence carries the weight on the first such quad.
    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mut out = AChanges::new();
        if p.added.is_empty() && p.removed.is_empty() {
            return Ok(out);
        }
        let rps = self.resolve(p);
        let n = rps.len();
        let union_seeds: Vec<([Id; 4], i64, i64)> = if rps.iter().any(|r| matches!(r.g, RG::Union))
        {
            let mut flips: rustc_hash::FxHashMap<[Id; 3], i64> =
                p.union_changes()?.iter().copied().collect();
            p.added
                .iter()
                .map(|q| (*q, 1))
                .chain(p.removed.iter().map(|q| (*q, -1)))
                .filter(|(q, _)| q[3] != Id::DEFAULT_GRAPH)
                .map(|(q, s)| {
                    let t = [q[0], q[1], q[2]];
                    let w = match flips.get(&t) {
                        Some(&f) if f == s => {
                            flips.remove(&t);
                            s
                        }
                        _ => 0,
                    };
                    (q, w, s)
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut trail = Vec::with_capacity(n);
        for i in 0..n {
            if rps[i].never() {
                continue;
            }
            let seeds: Vec<([Id; 4], [Id; 4], i64, i64)> = if matches!(rps[i].g, RG::Union) {
                union_seeds
                    .iter()
                    .map(|(q, w, s)| ([q[0], q[1], q[2], Id::DEFAULT_GRAPH], *q, *w, *s))
                    .collect()
            } else {
                p.added
                    .iter()
                    .map(|q| (*q, *q, 1, 1))
                    .chain(p.removed.iter().map(|q| (*q, *q, -1, -1)))
                    .collect()
            };
            let mut rest: Vec<usize> = (0..n).filter(|&j| j != i).collect();
            let which = |j: usize| if j < i { Which::New } else { Which::Old };
            for (q, src, w, s) in seeds {
                let mut b = vec![Id::UNDEF; self.slots];
                if rps[i].bind(&q, &mut b) {
                    trail.clear();
                    trail.push(vec![src]);
                    self.extend_a(p, &rps, &which, &mut rest, &b, w, s, &mut trail, &mut out)?;
                }
            }
        }
        Ok(out)
    }

    /// [`probe`](Self::probe) with annotations.
    pub fn probe_a(&self, p: &mut Pass, w: Which, seed: &[(usize, Id)]) -> Result<AChanges> {
        let rps = self.resolve(p);
        let mut b = vec![Id::UNDEF; self.slots];
        for &(pos, v) in seed {
            b[self.out[pos]] = v;
        }
        let mut out = AChanges::new();
        let mut rest: Vec<usize> = (0..rps.len()).collect();
        self.extend_a(
            p,
            &rps,
            &|_| w,
            &mut rest,
            &b,
            1,
            1,
            &mut Vec::new(),
            &mut out,
        )?;
        Ok(out)
    }

    /// The stored quads of snapshot `w` that some pattern matches with its constants,
    /// each visited once per pattern; for resolving fact keys. A pattern over the union
    /// default graph visits the named-graph quads that hold its triples. `f` returns
    /// `false` to stop.
    pub fn visit(
        &self,
        p: &Pass,
        w: Which,
        f: &mut dyn FnMut([Id; 4]) -> Result<bool>,
    ) -> Result<()> {
        let rps = self.resolve(p);
        let b = vec![Id::UNDEF; self.slots];
        for rp in &rps {
            if rp.never() {
                continue;
            }
            let mut go = true;
            if matches!(rp.g, RG::Union) {
                let c = |r: R| match r {
                    R::Id(c) => Some(c),
                    _ => None,
                };
                p.scan(w, c(rp.s), c(rp.p), c(rp.o), GraphSel::Named, &mut |q| {
                    go = f(q)?;
                    Ok(go)
                })?;
            } else {
                let mut stop: Result<()> = Ok(());
                Self::scan(p, w, rp, &b, &mut |q| {
                    if go {
                        match f(q) {
                            Ok(g) => go = g,
                            Err(e) => {
                                go = false;
                                stop = Err(e);
                            }
                        }
                    }
                    Ok(())
                })?;
                stop?;
            }
            if !go {
                break;
            }
        }
        Ok(())
    }

    pub fn init(&mut self, p: &mut Pass, schema: &[u32]) -> Result<Changes> {
        p.evaluate(&self.source, schema, None)
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let mut out = Changes::new();
        if p.added.is_empty() && p.removed.is_empty() {
            return Ok(out);
        }
        let rps = self.resolve(p);
        let n = rps.len();
        let union_changes: Vec<([Id; 3], i64)> = if rps.iter().any(|r| matches!(r.g, RG::Union)) {
            p.union_changes()?.to_vec()
        } else {
            Vec::new()
        };
        for i in 0..n {
            if rps[i].never() {
                continue;
            }
            let seeds: Vec<([Id; 4], i64)> = if matches!(rps[i].g, RG::Union) {
                union_changes
                    .iter()
                    .map(|(t, w)| ([t[0], t[1], t[2], Id::DEFAULT_GRAPH], *w))
                    .collect()
            } else {
                p.added
                    .iter()
                    .map(|q| (*q, 1))
                    .chain(p.removed.iter().map(|q| (*q, -1)))
                    .collect()
            };
            let mut rest: Vec<usize> = (0..n).filter(|&j| j != i).collect();
            let which = |j: usize| if j < i { Which::New } else { Which::Old };
            for (q, w) in seeds {
                let mut b = vec![Id::UNDEF; self.slots];
                if rps[i].bind(&q, &mut b) {
                    self.extend(p, &rps, &which, &mut rest, &b, w, &mut out)?;
                }
            }
        }
        Ok(out)
    }

    /// The node's rows at snapshot `w` that agree with `seed` (schema positions and
    /// their values).
    pub fn probe(&self, p: &mut Pass, w: Which, seed: &[(usize, Id)]) -> Result<Changes> {
        let rps = self.resolve(p);
        let mut b = vec![Id::UNDEF; self.slots];
        for &(pos, v) in seed {
            b[self.out[pos]] = v;
        }
        let mut out = Changes::new();
        let mut rest: Vec<usize> = (0..rps.len()).collect();
        self.extend(p, &rps, &|_| w, &mut rest, &b, 1, &mut out)?;
        Ok(out)
    }

    pub fn describe(&self) -> String {
        let slot = |s: &Slot| match s {
            Slot::Var(v) => self.names[*v].clone(),
            Slot::Const(t) => t.to_string(),
        };
        self.pats
            .iter()
            .map(|pt| {
                let t = format!("{} {} {}", slot(&pt.s), slot(&pt.p), slot(&pt.o));
                match &pt.g {
                    GSlot::Default => t,
                    GSlot::Const(g) => format!("GRAPH {g} {{ {t} }}"),
                    GSlot::Var(v) => format!("GRAPH {} {{ {t} }}", self.names[*v]),
                }
            })
            .collect::<Vec<_>>()
            .join(" . ")
    }
}
