//! Louvain and Leiden community detection (spec F14 §4).
//!
//! The graph is read as undirected and weighted (parallel edges add their weights, a
//! self-loop is ignored at the first level). Each level moves nodes between communities
//! while modularity improves, then aggregates the communities into the nodes of the next
//! level. Leiden adds a refinement step before the aggregation, which merges nodes only
//! into well-connected parts of their community, so every aggregated node is connected,
//! and reports each level with its communities split into connected pieces.
//!
//! Nodes move in index order and ties keep the current community, so a run is
//! deterministic for a seed. The moving phases are sequential.

use super::memory::{Buffer, Growing, Work};
use super::{Control, Csr, Graph, Rng, bad};
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct Params {
    pub resolution: f64,
    pub max_levels: u32,
    /// the modularity gain of a level below which the run stops
    pub tolerance: f64,
    pub leiden: bool,
    /// Leiden's randomness in choosing a community to refine into
    pub randomness: f64,
    pub seed: u64,
    /// the most sweeps over the nodes in one local-moving phase
    pub max_sweeps: u32,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            resolution: 1.0,
            max_levels: 10,
            tolerance: 1e-4,
            leiden: false,
            randomness: 0.01,
            seed: 0,
            max_sweeps: 100,
        }
    }
}

pub struct Output {
    /// for each level, from the finest, the community of each node, numbered by the
    /// first node in each
    pub levels: Vec<Vec<u32>>,
    /// the modularity of each level
    pub modularity: Vec<f64>,
}

/// A weighted graph of one level: rows without self-loops, with the weight of the
/// self-loops of each node held apart. Every array is charged to the call and given
/// back when the level drops.
struct Level {
    off: Buffer<usize>,
    adj: Growing<u32>,
    w: Growing<f64>,
    selfw: Buffer<f64>,
    /// the weighted degree: row sum plus twice the self-loop weight
    k: Buffer<f64>,
    /// the sum of the degrees
    m2: f64,
}

impl Level {
    fn n(&self) -> usize {
        self.k.len()
    }

    fn row(&self, u: usize) -> (&[u32], &[f64]) {
        let r = self.off[u]..self.off[u + 1];
        (&self.adj[r.clone()], &self.w[r])
    }

    fn from_csr(c: &Csr, ctl: &Control) -> Result<Level> {
        let n = c.n();
        let mut work = Work::new(ctl)?;
        let off = Buffer::collect(
            "community-offsets",
            n + 1,
            c.off.iter().map(|&o| o as usize),
            ctl,
        )?;
        let mut adj = Growing::new("community-edges", ctl)?;
        adj.reserve(c.adj.len())?;
        for row in c.adj.chunks(super::CHECK_EVERY) {
            work.tick_n(row.len(), "community-edges")?;
            for &v in row {
                adj.push(v)?;
            }
        }
        let mut w = Growing::new("community-weights", ctl)?;
        w.reserve(c.adj.len())?;
        for i in 0..c.adj.len() {
            work.tick("community-weights")?;
            w.push(c.w.as_ref().map_or(1.0, |w| w[i] as f64))?;
        }
        let mut k = Buffer::<f64>::zeroed("community-degrees", n, ctl)?;
        for u in 0..n {
            work.tick_n(off[u + 1] - off[u] + 1, "community-degrees")?;
            k[u] = w[off[u]..off[u + 1]].iter().sum();
        }
        let m2 = k.iter().sum();
        Ok(Level {
            off,
            adj,
            w,
            selfw: Buffer::zeroed("community-self-loops", n, ctl)?,
            k,
            m2,
        })
    }

    /// The modularity of a partition, with community numbers below `n`.
    fn modularity(&self, comm: &[u32], res: f64, ctl: &Control) -> Result<f64> {
        if self.m2 <= 0.0 {
            return Ok(0.0);
        }
        let n = self.n();
        let mut inside = Buffer::<f64>::zeroed("community-inside", n, ctl)?;
        let mut tot = Buffer::<f64>::zeroed("community-totals", n, ctl)?;
        let mut work = Work::new(ctl)?;
        for u in 0..n {
            let c = comm[u] as usize;
            tot[c] += self.k[u];
            inside[c] += 2.0 * self.selfw[u];
            let (r, w) = self.row(u);
            work.tick_n(r.len() + 1, "community-modularity")?;
            for (i, &v) in r.iter().enumerate() {
                if comm[v as usize] as usize == c {
                    inside[c] += w[i];
                }
            }
        }
        Ok((0..n)
            .map(|c| inside[c] / self.m2 - res * (tot[c] / self.m2).powi(2))
            .sum())
    }

    /// Merge the nodes of each community into one node. `comm` has dense numbers.
    fn aggregate(&self, comm: &[u32], count: usize, ctl: &Control) -> Result<Level> {
        let n = self.n();
        let mut work = Work::new(ctl)?;
        // members by community
        let mut start = Buffer::<usize>::zeroed("community-start", count + 1, ctl)?;
        for &c in comm {
            work.tick("community-start")?;
            start[c as usize + 1] += 1;
        }
        for c in 0..count {
            work.tick("community-start")?;
            start[c + 1] += start[c];
        }
        let mut at = Buffer::collect("community-cursor", count + 1, start.iter().copied(), ctl)?;
        let mut members = Buffer::<u32>::zeroed("community-members", n, ctl)?;
        for u in 0..n {
            work.tick("community-members")?;
            members[at[comm[u] as usize]] = u as u32;
            at[comm[u] as usize] += 1;
        }
        drop(at);
        let mut off = Buffer::<usize>::zeroed("community-offsets", count + 1, ctl)?;
        let mut adj = Growing::new("community-edges", ctl)?;
        let mut w = Growing::new("community-weights", ctl)?;
        let mut selfw = Buffer::<f64>::zeroed("community-self-loops", count, ctl)?;
        let mut k = Buffer::<f64>::zeroed("community-degrees", count, ctl)?;
        let mut acc = Buffer::<f64>::zeroed("community-accumulator", count, ctl)?;
        let mut stamp = Buffer::splat("community-stamp", count, usize::MAX, ctl)?;
        let mut touched: Growing<u32> = Growing::new("community-touched", ctl)?;
        for c in 0..count {
            touched.clear();
            let mut internal = 0.0;
            for &u in &members[start[c]..start[c + 1]] {
                let u = u as usize;
                selfw[c] += self.selfw[u];
                k[c] += self.k[u];
                let (r, ws) = self.row(u);
                work.tick_n(r.len() + 1, "community-aggregate")?;
                for (i, &v) in r.iter().enumerate() {
                    let d = comm[v as usize] as usize;
                    if d == c {
                        internal += ws[i];
                    } else {
                        if stamp[d] != c {
                            stamp[d] = c;
                            acc[d] = 0.0;
                            touched.push(d as u32)?;
                        }
                        acc[d] += ws[i];
                    }
                }
            }
            // an internal edge is seen from both ends
            selfw[c] += internal / 2.0;
            touched.sort_unstable();
            adj.reserve(touched.len())?;
            w.reserve(touched.len())?;
            for &d in touched.iter() {
                adj.push(d)?;
                w.push(acc[d as usize])?;
            }
            off[c + 1] = adj.len();
        }
        Ok(Level {
            off,
            adj,
            w,
            selfw,
            k,
            m2: self.m2,
        })
    }
}

/// Number the communities by first appearance. Returns the count.
fn renumber(comm: &mut [u32], ctl: &Control) -> Result<usize> {
    let mut map = Buffer::splat("community-renumber", comm.len(), u32::MAX, ctl)?;
    let mut work = Work::new(ctl)?;
    let mut next = 0u32;
    for c in comm.iter_mut() {
        work.tick("community-renumber")?;
        if map[*c as usize] == u32::MAX {
            map[*c as usize] = next;
            next += 1;
        }
        *c = map[*c as usize];
    }
    Ok(next as usize)
}

/// Move nodes to the neighbouring community of the best modularity gain until a sweep
/// moves none. Returns whether any node moved.
fn local_moving(
    lv: &Level,
    comm: &mut [u32],
    res: f64,
    max_sweeps: u32,
    ctl: &Control,
) -> Result<bool> {
    let n = lv.n();
    let mut tot = Buffer::<f64>::zeroed("community-totals", n, ctl)?;
    let mut work = Work::new(ctl)?;
    for u in 0..n {
        work.tick("community-totals")?;
        tot[comm[u] as usize] += lv.k[u];
    }
    let mut acc = Buffer::<f64>::zeroed("community-accumulator", n, ctl)?;
    let mut stamp = Buffer::splat("community-stamp", n, usize::MAX, ctl)?;
    let mut touched: Growing<u32> = Growing::new("community-touched", ctl)?;
    let mut any = false;
    for _ in 0..max_sweeps {
        ctl.check()?;
        let mut moved = false;
        for u in 0..n {
            let (r, ws) = lv.row(u);
            work.tick_n(r.len() + 1, "community-local-moving")?;
            if r.is_empty() {
                continue;
            }
            touched.clear();
            for (i, &v) in r.iter().enumerate() {
                let c = comm[v as usize] as usize;
                if stamp[c] != u {
                    stamp[c] = u;
                    acc[c] = 0.0;
                    touched.push(c as u32)?;
                }
                acc[c] += ws[i];
            }
            let cu = comm[u] as usize;
            let ku = lv.k[u];
            tot[cu] -= ku;
            let own = if stamp[cu] == u { acc[cu] } else { 0.0 };
            let mut best = cu;
            let mut best_gain = own - res * tot[cu] * ku / lv.m2;
            for &c in touched.iter() {
                let c = c as usize;
                let gain = acc[c] - res * tot[c] * ku / lv.m2;
                if gain > best_gain + 1e-12 {
                    best = c;
                    best_gain = gain;
                }
            }
            tot[best] += ku;
            if best != cu {
                comm[u] = best as u32;
                moved = true;
            }
        }
        if !moved {
            break;
        }
        any = true;
    }
    Ok(any)
}

/// Leiden's refinement: inside each community, singletons merge into well-connected
/// parts of it. Returns the refined partition with dense numbers, and its count.
///
/// A singleton chooses among the parts it may join with probability proportional to
/// `exp(gain / theta)`. The gain is measured in units of edge weight, so multiplying
/// every weight by a factor has the effect of dividing `theta` by it.
fn refine(
    lv: &Level,
    comm: &[u32],
    res: f64,
    theta: f64,
    rng: &mut Rng,
    ctl: &Control,
) -> Result<(Buffer<u32>, usize)> {
    let n = lv.n();
    let mut work = Work::new(ctl)?;
    let mut refined = Buffer::collect("leiden-refined", n, 0..n as u32, ctl)?;
    let mut size = Buffer::splat("leiden-size", n, 1u32, ctl)?;
    // K_R: the degree of each refined community
    let mut kr = Buffer::collect("leiden-degrees", n, lv.k.iter().copied(), ctl)?;
    // the degree of each community of the partition
    let mut kc = Buffer::<f64>::zeroed("leiden-community-degrees", n, ctl)?;
    for u in 0..n {
        work.tick("leiden-community-degrees")?;
        kc[comm[u] as usize] += lv.k[u];
    }
    // E(R, C - R): the weight between a refined community and the rest of its community
    let mut ext = Buffer::<f64>::zeroed("leiden-external", n, ctl)?;
    for u in 0..n {
        let (r, ws) = lv.row(u);
        work.tick_n(r.len() + 1, "leiden-external")?;
        for (i, &v) in r.iter().enumerate() {
            if comm[v as usize] == comm[u] {
                ext[u] += ws[i];
            }
        }
    }
    let mut order = Buffer::collect("leiden-order", n, 0..n as u32, ctl)?;
    for i in (1..n).rev() {
        work.tick("leiden-order")?;
        order.swap(i, rng.below(i + 1));
    }
    let mut acc = Buffer::<f64>::zeroed("leiden-accumulator", n, ctl)?;
    let mut stamp = Buffer::splat("leiden-stamp", n, usize::MAX, ctl)?;
    let mut touched: Growing<u32> = Growing::new("leiden-touched", ctl)?;
    let mut cand: Growing<(u32, f64)> = Growing::new("leiden-candidates", ctl)?;
    let mut weights: Growing<f64> = Growing::new("leiden-weights", ctl)?;
    for (step, &v) in order.iter().enumerate() {
        let v = v as usize;
        work.tick("leiden-refine")?;
        if size[refined[v] as usize] != 1 {
            continue;
        }
        let c = comm[v];
        let kv = lv.k[v];
        // v must be well connected to its community
        if ext[v] < res * kv * (kc[c as usize] - kv) / lv.m2 {
            continue;
        }
        touched.clear();
        let (r, ws) = lv.row(v);
        work.tick_n(r.len(), "leiden-refine")?;
        for (i, &x) in r.iter().enumerate() {
            if comm[x as usize] != c {
                continue;
            }
            let rr = refined[x as usize] as usize;
            if stamp[rr] != step {
                stamp[rr] = step;
                acc[rr] = 0.0;
                touched.push(rr as u32)?;
            }
            acc[rr] += ws[i];
        }
        cand.clear();
        let mut top = 0.0f64;
        for &rr in touched.iter() {
            let rr = rr as usize;
            if rr == refined[v] as usize {
                continue;
            }
            // the part must itself be well connected to the community
            if ext[rr] < res * kr[rr] * (kc[c as usize] - kr[rr]) / lv.m2 {
                continue;
            }
            // the gain of the move in units of edge weight, as local moving measures it
            let gain = acc[rr] - res * kr[rr] * kv / lv.m2;
            if gain >= 0.0 {
                top = top.max(gain);
                cand.push((rr as u32, gain))?;
            }
        }
        if cand.is_empty() {
            continue;
        }
        // staying alone has gain 0 and is a choice too
        weights.clear();
        for c in cand.iter() {
            weights.push(((c.1 - top) / theta).exp())?;
        }
        let stay = ((0.0 - top) / theta).exp();
        let total: f64 = weights.iter().sum::<f64>() + stay;
        let mut pick = rng.next_f64() * total;
        let mut chosen = None;
        for (i, wgt) in weights.iter().enumerate() {
            if pick < *wgt {
                chosen = Some(i);
                break;
            }
            pick -= wgt;
        }
        let Some(i) = chosen else { continue };
        let (target, _) = cand[i];
        let target = target as usize;
        let old = refined[v] as usize;
        let to_target = acc[target];
        refined[v] = target as u32;
        size[old] = 0;
        size[target] += 1;
        kr[target] += kv;
        ext[target] = ext[target] + ext[old] - 2.0 * to_target;
    }
    let count = renumber(&mut refined, ctl)?;
    Ok((refined, count))
}

/// Split each community into its connected pieces in `g`.
fn split_connected(g: &Csr, labels: &[u32], ctl: &Control) -> Result<Buffer<u32>> {
    let n = g.n();
    let mut out = Buffer::splat("community-pieces", n, u32::MAX, ctl)?;
    let mut work = Work::new(ctl)?;
    let mut next = 0u32;
    let mut stack: Growing<u32> = Growing::new("community-piece-stack", ctl)?;
    for s in 0..n {
        work.tick("community-pieces")?;
        if out[s] != u32::MAX {
            continue;
        }
        out[s] = next;
        stack.push(s as u32)?;
        while let Some(u) = stack.pop() {
            let row = g.row(u);
            work.tick_n(row.len() + 1, "community-pieces")?;
            for &v in row {
                if out[v as usize] == u32::MAX && labels[v as usize] == labels[s] {
                    out[v as usize] = next;
                    stack.push(v)?;
                }
            }
        }
        next += 1;
    }
    Ok(out)
}

pub fn run(g: &Graph, p: &Params, ctl: &Control) -> Result<Output> {
    if p.resolution <= 0.0 {
        return Err(bad("louvain", "resolution must be above 0"));
    }
    let sym = g.weighted_sym(ctl)?;
    let n = sym.n();
    let mut levels: Vec<Vec<u32>> = Vec::new();
    let mut modularity: Vec<f64> = Vec::new();
    if n == 0 {
        return Ok(Output { levels, modularity });
    }
    let base = Level::from_csr(&sym, ctl)?;
    let mut rng = Rng::new(p.seed);
    let mut lv = Level::from_csr(&sym, ctl)?;
    let mut comm = Buffer::collect("community-membership", n, 0..n as u32, ctl)?;
    // the node of the current level that each original node is in
    let mut node_of = Buffer::collect("community-node-of", n, 0..n as u32, ctl)?;
    let mut last_q = f64::NEG_INFINITY;
    for level in 0..p.max_levels.max(1) {
        ctl.check()?;
        local_moving(&lv, &mut comm, p.resolution, p.max_sweeps, ctl)?;
        let mut dense = Buffer::collect("community-dense", comm.len(), comm.iter().copied(), ctl)?;
        let count = renumber(&mut dense, ctl)?;
        let mut flat = Buffer::collect(
            "community-level",
            n,
            node_of.iter().map(|&x| dense[x as usize]),
            ctl,
        )?;
        if p.leiden {
            // Leiden reports each community split into its connected pieces
            flat = split_connected(&sym, &flat, ctl)?;
        }
        renumber(&mut flat, ctl)?;
        let q = base.modularity(&flat, p.resolution, ctl)?;
        // a later level that changes nothing ends the run
        if level > 0 && levels.last().is_some_and(|l| l[..] == flat[..]) {
            break;
        }
        let gain = q - last_q;
        levels.push(flat.into_output());
        modularity.push(q);
        last_q = q;
        if level > 0 && gain < p.tolerance {
            break;
        }
        if level + 1 >= p.max_levels.max(1) || count == lv.n() {
            break;
        }
        // aggregate: by the communities (Louvain) or by the refinement of them (Leiden)
        let (parts, pcount) = if p.leiden {
            refine(
                &lv,
                &dense,
                p.resolution,
                p.randomness.max(1e-9),
                &mut rng,
                ctl,
            )?
        } else {
            let copy = Buffer::collect("community-parts", dense.len(), dense.iter().copied(), ctl)?;
            (copy, count)
        };
        if pcount == lv.n() {
            // nothing merged: another level would be the same
            break;
        }
        let next = lv.aggregate(&parts, pcount, ctl)?;
        // each aggregated node starts in the community its members were in
        let mut start = Buffer::<u32>::zeroed("community-membership", pcount, ctl)?;
        for u in 0..lv.n() {
            start[parts[u] as usize] = dense[u];
        }
        let mut work = Work::new(ctl)?;
        for x in node_of.iter_mut() {
            work.tick("community-node-of")?;
            *x = parts[*x as usize];
        }
        lv = next;
        comm = start;
    }
    Ok(Output { levels, modularity })
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    fn two_cliques() -> Graph {
        let mut e = Vec::new();
        for a in 0..5u32 {
            for b in a + 1..5 {
                e.push((a, b));
                e.push((a + 5, b + 5));
            }
        }
        e.push((4, 5));
        ugraph(10, &e)
    }

    #[test]
    fn louvain_finds_two_cliques() {
        let o = run(&two_cliques(), &Params::default(), &Control::unlimited()).unwrap();
        let top = o.levels.last().unwrap();
        assert!(top[..5].iter().all(|&c| c == top[0]));
        assert!(top[5..].iter().all(|&c| c == top[5]));
        assert_ne!(top[0], top[5]);
        assert!(*o.modularity.last().unwrap() > 0.4);
    }

    fn connected(g: &Graph, labels: &[u32]) -> bool {
        let s = g.simple(&Control::unlimited()).unwrap();
        split_connected(&s, labels, &Control::unlimited())
            .unwrap()
            .iter()
            .max()
            == labels.iter().max()
    }

    #[test]
    fn leiden_on_the_karate_club() {
        let g = ugraph(34, &karate());
        assert_eq!(g.m(), 156);
        let p = Params {
            leiden: true,
            seed: 1,
            ..Params::default()
        };
        let a = run(&g, &p, &Control::unlimited()).unwrap();
        let b = run(&g, &p, &Control::unlimited()).unwrap();
        assert_eq!(a.levels, b.levels, "same seed, same result");
        let q = *a.modularity.last().unwrap();
        assert!(q >= 0.41, "modularity {q}");
        for l in &a.levels {
            assert!(connected(&g, l));
        }
        let lou = run(&g, &Params::default(), &Control::unlimited()).unwrap();
        assert!(*lou.modularity.last().unwrap() >= 0.38);
    }

    /// Planted groups with noisy links between them, from a fixed generator.
    fn planted(n: u32, groups: u32, seed: u64) -> Graph {
        let mut x = seed | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut e = Vec::new();
        for a in 0..n {
            for _ in 0..4 {
                let same = next() % 10 < 7;
                let b = if same {
                    (next() % (n / groups) as u64) as u32 * groups + a % groups
                } else {
                    (next() % n as u64) as u32
                };
                if a != b && b < n {
                    e.push((a.min(b), a.max(b)));
                }
            }
        }
        e.sort_unstable();
        e.dedup();
        ugraph(n, &e)
    }

    #[test]
    fn leiden_is_connected_and_no_worse_than_louvain_on_random_graphs() {
        for (n, groups, seed) in [(200, 5, 11), (300, 8, 12), (400, 4, 13), (150, 3, 14)] {
            let g = planted(n, groups, seed);
            let lou = run(&g, &Params::default(), &Control::unlimited()).unwrap();
            let lei = run(
                &g,
                &Params {
                    leiden: true,
                    seed: 3,
                    ..Params::default()
                },
                &Control::unlimited(),
            )
            .unwrap();
            let (ql, qe) = (
                *lou.modularity.last().unwrap(),
                *lei.modularity.last().unwrap(),
            );
            // Leiden refines each level, so it may only differ from Louvain by a small margin
            assert!(qe >= ql - 0.01, "n={n}: leiden {qe} against louvain {ql}");
            for l in &lei.levels {
                assert!(connected(&g, l), "n={n}: a community is split");
            }
        }
    }

    #[test]
    fn isolated_nodes_and_empty_graph() {
        let g = ugraph(3, &[]);
        let o = run(&g, &Params::default(), &Control::unlimited()).unwrap();
        assert_eq!(o.levels[0], vec![0, 1, 2]);
        let g = ugraph(0, &[]);
        assert!(
            run(&g, &Params::default(), &Control::unlimited())
                .unwrap()
                .levels
                .is_empty()
        );
    }
}
