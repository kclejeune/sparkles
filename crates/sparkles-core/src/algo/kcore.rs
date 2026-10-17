//! k-core decomposition by Batagelj and Zaversnik's bucket algorithm (spec F14 §4).
//! Linear in the number of edges. The graph is read as undirected and simple.

use super::memory::{Buffer, Work};
use super::{Control, Graph};
use crate::error::Result;

/// The core number of each node.
pub fn run(g: &Graph, ctl: &Control) -> Result<Vec<u32>> {
    let s = g.simple(ctl)?;
    let n = s.n();
    let mut work = Work::new(ctl)?;
    let mut deg = Buffer::collect(
        "kcore-degree",
        n,
        (0..n as u32).map(|u| s.deg(u) as u32),
        ctl,
    )?;
    let max_deg = deg.iter().copied().max().unwrap_or(0) as usize;
    // bin[d] = start of the nodes of degree d in `vert`
    let mut bin = Buffer::<usize>::zeroed("kcore-bins", max_deg + 2, ctl)?;
    for &d in deg.iter() {
        work.tick("kcore-bins")?;
        bin[d as usize + 1] += 1;
    }
    for d in 0..=max_deg {
        work.tick("kcore-bins")?;
        bin[d + 1] += bin[d];
    }
    let mut pos = Buffer::<usize>::zeroed("kcore-position", n, ctl)?;
    let mut vert = Buffer::<u32>::zeroed("kcore-order", n, ctl)?;
    {
        let mut at = Buffer::collect("kcore-cursor", bin.len(), bin.iter().copied(), ctl)?;
        for v in 0..n {
            work.tick("kcore-order")?;
            let d = deg[v] as usize;
            pos[v] = at[d];
            vert[at[d]] = v as u32;
            at[d] += 1;
        }
    }
    for i in 0..n {
        let v = vert[i];
        let row = s.row(v);
        work.tick_n(row.len() + 1, "kcore-peel")?;
        for &u in row {
            let (du, dv) = (deg[u as usize], deg[v as usize]);
            if du > dv {
                // move u to the front of its bin and shrink its degree
                let pu = pos[u as usize];
                let pw = bin[du as usize];
                let w = vert[pw];
                if u != w {
                    pos[u as usize] = pw;
                    pos[w as usize] = pu;
                    vert[pu] = w;
                    vert[pw] = u;
                }
                bin[du as usize] += 1;
                deg[u as usize] -= 1;
            }
        }
    }
    ctl.check()?;
    Ok(deg.into_output())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn triangle_with_a_tail() {
        // a b c triangle, c-d, d-e
        let g = ugraph(5, &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4)]);
        assert_eq!(run(&g, &Control::unlimited()).unwrap(), vec![2, 2, 2, 1, 1]);
    }

    #[test]
    fn matches_repeated_peeling() {
        let mut x = 5u64;
        let mut rnd = |m: u32| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32 % m
        };
        for _ in 0..30 {
            let n = 25u32;
            let e: Vec<(u32, u32)> = (0..50).map(|_| (rnd(n), rnd(n))).collect();
            let g = ugraph(n, &e);
            let got = run(&g, &Control::unlimited()).unwrap();
            // reference: for k = 1.., peel nodes of degree < k until stable
            let mut adj = vec![std::collections::BTreeSet::new(); n as usize];
            for &(a, b) in &e {
                if a != b {
                    adj[a as usize].insert(b);
                    adj[b as usize].insert(a);
                }
            }
            let mut core = vec![0u32; n as usize];
            for k in 1..=n {
                let mut alive = vec![true; n as usize];
                loop {
                    let mut changed = false;
                    for v in 0..n as usize {
                        if alive[v]
                            && (adj[v].iter().filter(|&&u| alive[u as usize]).count() as u32) < k
                        {
                            alive[v] = false;
                            changed = true;
                        }
                    }
                    if !changed {
                        break;
                    }
                }
                for v in 0..n as usize {
                    if alive[v] {
                        core[v] = k;
                    }
                }
            }
            assert_eq!(got, core);
        }
    }
}
