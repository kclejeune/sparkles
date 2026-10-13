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

use super::{CHECK_EVERY, Control, Csr, Graph, Rng, bad};
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
/// self-loops of each node held apart.
struct Level {
    off: Vec<usize>,
    adj: Vec<u32>,
    w: Vec<f64>,
    selfw: Vec<f64>,
    /// the weighted degree: row sum plus twice the self-loop weight
    k: Vec<f64>,
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

    fn from_csr(c: &Csr) -> Level {
        let n = c.n();
        let off: Vec<usize> = c.off.iter().map(|&o| o as usize).collect();
        let w: Vec<f64> = match &c.w {
            Some(w) => w.iter().map(|&x| x as f64).collect(),
            None => vec![1.0; c.adj.len()],
        };
        let k: Vec<f64> = (0..n).map(|u| w[off[u]..off[u + 1]].iter().sum()).collect();
        let m2 = k.iter().sum();
        Level {
            off,
            adj: c.adj.clone(),
            w,
            selfw: vec![0.0; n],
            k,
            m2,
        }
    }

    /// The modularity of a partition, with community numbers below `n`.
    fn modularity(&self, comm: &[u32], res: f64) -> f64 {
        if self.m2 <= 0.0 {
            return 0.0;
        }
        let n = self.n();
        let mut inside = vec![0f64; n];
        let mut tot = vec![0f64; n];
        for u in 0..n {
            let c = comm[u] as usize;
            tot[c] += self.k[u];
            inside[c] += 2.0 * self.selfw[u];
            let (r, w) = self.row(u);
            for (i, &v) in r.iter().enumerate() {
                if comm[v as usize] as usize == c {
                    inside[c] += w[i];
                }
            }
        }
        (0..n)
            .map(|c| inside[c] / self.m2 - res * (tot[c] / self.m2).powi(2))
            .sum()
    }

    /// Merge the nodes of each community into one node. `comm` has dense numbers.
    fn aggregate(&self, comm: &[u32], count: usize) -> Level {
        let n = self.n();
        // members by community
        let mut start = vec![0usize; count + 1];
        for &c in comm {
            start[c as usize + 1] += 1;
        }
        for c in 0..count {
            start[c + 1] += start[c];
        }
        let mut at = start.clone();
        let mut members = vec![0u32; n];
        for u in 0..n {
            members[at[comm[u] as usize]] = u as u32;
            at[comm[u] as usize] += 1;
        }
        let mut off = vec![0usize; count + 1];
        let mut adj = Vec::new();
        let mut w = Vec::new();
        let mut selfw = vec![0f64; count];
        let mut k = vec![0f64; count];
        let mut acc = vec![0f64; count];
        let mut stamp = vec![usize::MAX; count];
        let mut touched: Vec<u32> = Vec::new();
        for c in 0..count {
            touched.clear();
            let mut internal = 0.0;
            for &u in &members[start[c]..start[c + 1]] {
                let u = u as usize;
                selfw[c] += self.selfw[u];
                k[c] += self.k[u];
                let (r, ws) = self.row(u);
                for (i, &v) in r.iter().enumerate() {
                    let d = comm[v as usize] as usize;
                    if d == c {
                        internal += ws[i];
                    } else {
                        if stamp[d] != c {
                            stamp[d] = c;
                            acc[d] = 0.0;
                            touched.push(d as u32);
                        }
                        acc[d] += ws[i];
                    }
                }
            }
            // an internal edge is seen from both ends
            selfw[c] += internal / 2.0;
            touched.sort_unstable();
            for &d in &touched {
                adj.push(d);
                w.push(acc[d as usize]);
            }
            off[c + 1] = adj.len();
        }
        Level {
            off,
            adj,
            w,
            selfw,
            k,
            m2: self.m2,
        }
    }
}

/// Number the communities by first appearance. Returns the count.
fn renumber(comm: &mut [u32]) -> usize {
    let mut map = vec![u32::MAX; comm.len()];
    let mut next = 0u32;
    for c in comm.iter_mut() {
        if map[*c as usize] == u32::MAX {
            map[*c as usize] = next;
            next += 1;
        }
        *c = map[*c as usize];
    }
    next as usize
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
    let mut tot = vec![0f64; n];
    for u in 0..n {
        tot[comm[u] as usize] += lv.k[u];
    }
    let mut acc = vec![0f64; n];
    let mut stamp = vec![usize::MAX; n];
    let mut touched: Vec<u32> = Vec::new();
    let mut any = false;
    for _ in 0..max_sweeps {
        ctl.check()?;
        let mut moved = false;
        for u in 0..n {
            if u % CHECK_EVERY == 0 {
                ctl.check()?;
            }
            let (r, ws) = lv.row(u);
            if r.is_empty() {
                continue;
            }
            touched.clear();
            for (i, &v) in r.iter().enumerate() {
                let c = comm[v as usize] as usize;
                if stamp[c] != u {
                    stamp[c] = u;
                    acc[c] = 0.0;
                    touched.push(c as u32);
                }
                acc[c] += ws[i];
            }
            let cu = comm[u] as usize;
            let ku = lv.k[u];
            tot[cu] -= ku;
            let own = if stamp[cu] == u { acc[cu] } else { 0.0 };
            let mut best = cu;
            let mut best_gain = own - res * tot[cu] * ku / lv.m2;
            for &c in &touched {
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
fn refine(lv: &Level, comm: &[u32], res: f64, theta: f64, rng: &mut Rng) -> (Vec<u32>, usize) {
    let n = lv.n();
    let mut refined: Vec<u32> = (0..n as u32).collect();
    let mut size = vec![1u32; n];
    // K_R: the degree of each refined community
    let mut kr: Vec<f64> = lv.k.clone();
    // the degree of each community of the partition
    let mut kc = vec![0f64; n];
    for u in 0..n {
        kc[comm[u] as usize] += lv.k[u];
    }
    // E(R, C - R): the weight between a refined community and the rest of its community
    let mut ext = vec![0f64; n];
    for u in 0..n {
        let (r, ws) = lv.row(u);
        for (i, &v) in r.iter().enumerate() {
            if comm[v as usize] == comm[u] {
                ext[u] += ws[i];
            }
        }
    }
    let mut order: Vec<u32> = (0..n as u32).collect();
    for i in (1..n).rev() {
        order.swap(i, rng.below(i + 1));
    }
    let mut acc = vec![0f64; n];
    let mut stamp = vec![usize::MAX; n];
    let mut touched: Vec<u32> = Vec::new();
    let mut cand: Vec<(u32, f64)> = Vec::new();
    for (step, &v) in order.iter().enumerate() {
        let v = v as usize;
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
        for (i, &x) in r.iter().enumerate() {
            if comm[x as usize] != c {
                continue;
            }
            let rr = refined[x as usize] as usize;
            if stamp[rr] != step {
                stamp[rr] = step;
                acc[rr] = 0.0;
                touched.push(rr as u32);
            }
            acc[rr] += ws[i];
        }
        cand.clear();
        let mut top = 0.0f64;
        for &rr in &touched {
            let rr = rr as usize;
            if rr == refined[v] as usize {
                continue;
            }
            // the part must itself be well connected to the community
            if ext[rr] < res * kr[rr] * (kc[c as usize] - kr[rr]) / lv.m2 {
                continue;
            }
            let gain = acc[rr] - res * kr[rr] * kv / lv.m2;
            if gain >= 0.0 {
                top = top.max(gain);
                cand.push((rr as u32, gain));
            }
        }
        if cand.is_empty() {
            continue;
        }
        // staying alone has gain 0 and is a choice too
        let weights: Vec<f64> = cand.iter().map(|c| ((c.1 - top) / theta).exp()).collect();
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
    let count = renumber(&mut refined);
    (refined, count)
}

/// Split each community into its connected pieces in `g`.
fn split_connected(g: &Csr, labels: &[u32]) -> Vec<u32> {
    let n = g.n();
    let mut out = vec![u32::MAX; n];
    let mut next = 0u32;
    let mut stack: Vec<u32> = Vec::new();
    for s in 0..n {
        if out[s] != u32::MAX {
            continue;
        }
        out[s] = next;
        stack.push(s as u32);
        while let Some(u) = stack.pop() {
            for &v in g.row(u) {
                if out[v as usize] == u32::MAX && labels[v as usize] == labels[s] {
                    out[v as usize] = next;
                    stack.push(v);
                }
            }
        }
        next += 1;
    }
    out
}

pub fn run(g: &Graph, p: &Params, ctl: &Control) -> Result<Output> {
    if p.resolution <= 0.0 {
        return Err(bad("louvain", "resolution must be above 0"));
    }
    let sym = g.weighted_sym(ctl)?;
    let n = sym.n();
    ctl.charge(sym.bytes() * 3 + n as u64 * 48)?;
    let base = Level::from_csr(&sym);
    let mut levels: Vec<Vec<u32>> = Vec::new();
    let mut modularity: Vec<f64> = Vec::new();
    if n == 0 {
        return Ok(Output { levels, modularity });
    }
    let mut rng = Rng::new(p.seed);
    let mut lv = Level::from_csr(&sym);
    let mut comm: Vec<u32> = (0..n as u32).collect();
    // the node of the current level that each original node is in
    let mut node_of: Vec<u32> = (0..n as u32).collect();
    let mut last_q = f64::NEG_INFINITY;
    for level in 0..p.max_levels.max(1) {
        ctl.check()?;
        local_moving(&lv, &mut comm, p.resolution, p.max_sweeps, ctl)?;
        let mut dense = comm.clone();
        let count = renumber(&mut dense);
        let mut flat: Vec<u32> = node_of.iter().map(|&x| dense[x as usize]).collect();
        if p.leiden {
            flat = split_connected(&sym, &flat);
        }
        renumber(&mut flat);
        let q = base.modularity(&flat, p.resolution);
        // a later level that changes nothing ends the run
        if level > 0 && levels.last() == Some(&flat) {
            break;
        }
        let gain = q - last_q;
        levels.push(flat);
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
            refine(&lv, &dense, p.resolution, p.randomness.max(1e-9), &mut rng)
        } else {
            (dense.clone(), count)
        };
        if pcount == lv.n() {
            // nothing merged: another level would be the same
            break;
        }
        let next = lv.aggregate(&parts, pcount);
        // each aggregated node starts in the community its members were in
        let mut start = vec![0u32; pcount];
        for u in 0..lv.n() {
            start[parts[u] as usize] = dense[u];
        }
        for x in node_of.iter_mut() {
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
        split_connected(&s, labels).iter().max() == labels.iter().max()
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
