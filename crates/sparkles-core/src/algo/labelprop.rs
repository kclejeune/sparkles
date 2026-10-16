//! Label propagation (spec F14 §4), with the community detection rule of LDBC
//! Graphalytics: synchronous updates, and a tie goes to the smallest label.
//!
//! A node takes the label that most of its neighbours had in the last round, or the
//! label of the largest total edge weight. A directed projection counts the labels of
//! the in-neighbours as well, once for each edge.

use super::memory::{Buffer, Growing, Work};
use super::{BLOCK, CHECK_EVERY, Control, Graph, Par};
use crate::error::Result;

pub struct Output {
    /// the community of each node: the index of one of its members
    pub label: Vec<u32>,
    pub iterations: u32,
    pub converged: bool,
}

pub fn run(g: &Graph, max_iterations: u32, par: &Par, ctl: &Control) -> Result<Output> {
    let n = g.n();
    let mut label = Buffer::collect("labelprop-label", n, 0..n as u32, ctl)?;
    let mut next = Buffer::<u32>::zeroed("labelprop-next", n, ctl)?;
    let incoming = g.inn(ctl)?;
    let inn = &*incoming;
    let mut iterations = 0;
    let mut converged = false;
    for it in 0..max_iterations {
        ctl.check()?;
        let prev = &label;
        par.chunks_mut(&mut next, BLOCK, |lo, chunk| {
            let mut work = Work::new(ctl)?;
            // the labels around one node, charged as the largest row needs
            let mut seen: Growing<(u32, f64)> = Growing::new("labelprop-seen", ctl)?;
            for (i, slot) in chunk.iter_mut().enumerate() {
                let u = (lo + i) as u32;
                seen.clear();
                let sides: &[&super::Csr] = if g.symmetric {
                    &[&g.out]
                } else {
                    &[&g.out, inn]
                };
                for csr in sides {
                    let (r, w) = (csr.row(u), csr.wrow(u));
                    work.tick_n(r.len() + 1, "labelprop-node")?;
                    seen.reserve(r.len())?;
                    for (k, &v) in r.iter().enumerate() {
                        seen.push((prev[v as usize], w.map_or(1.0, |w| w[k] as f64)))?;
                    }
                }
                if seen.is_empty() {
                    *slot = prev[u as usize];
                    continue;
                }
                seen.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
                let mut best = (prev[u as usize], -1.0f64);
                let mut k = 0;
                while k < seen.len() {
                    let l = seen[k].0;
                    let mut total = 0.0;
                    while k < seen.len() && seen[k].0 == l {
                        total += seen[k].1;
                        k += 1;
                    }
                    // labels come in increasing order, so a tie keeps the smaller one
                    if total > best.1 {
                        best = (l, total);
                    }
                }
                *slot = best.0;
            }
            Ok(())
        })?;
        iterations = it + 1;
        let mut same = true;
        for (a, b) in label.chunks(CHECK_EVERY).zip(next.chunks(CHECK_EVERY)) {
            ctl.check()?;
            if a != b {
                same = false;
                break;
            }
        }
        std::mem::swap(&mut label, &mut next);
        if same {
            converged = true;
            break;
        }
    }
    Ok(Output {
        label: label.into_output(),
        iterations,
        converged,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn two_cliques_get_two_labels() {
        let mut e = Vec::new();
        for a in 0..4u32 {
            for b in a + 1..4 {
                e.push((a, b));
                e.push((a + 4, b + 4));
            }
        }
        e.push((3, 4));
        let g = ugraph(8, &e);
        let o = run(&g, 10, &Par::single(), &Control::unlimited()).unwrap();
        assert!(o.label[..4].iter().all(|&l| l == o.label[0]));
        assert!(o.label[4..].iter().all(|&l| l == o.label[4]));
    }

    #[test]
    fn tie_goes_to_the_smallest_label() {
        // node 1 sees labels 0 and 2 once each
        let g = ugraph(3, &[(0, 1), (1, 2)]);
        let o = run(&g, 1, &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(o.label[1], 0);
    }
}
