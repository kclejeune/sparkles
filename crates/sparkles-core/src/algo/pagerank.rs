//! PageRank and personalized PageRank (spec F14 §4).
//!
//! Pull-based power iteration, parallel over nodes. The scores are probabilities, so they
//! sum to 1. A node without out-edges spreads its rank evenly over all nodes, as LDBC
//! Graphalytics defines, or over the sources of a personalized run. The sums over a
//! node's in-neighbours run in index order and the sums over nodes add fixed-size blocks
//! in order, so the result does not depend on the number of threads.

use super::memory::{Buffer, Work};
use super::{BLOCK, Control, Graph, Par, Stats, bad};
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct Params {
    pub damping: f64,
    pub max_iterations: u32,
    /// the change of the scores (sum of absolute differences) that ends the run
    pub tolerance: f64,
    /// the sources of a personalized run; empty: every node
    pub sources: Vec<u32>,
    /// run `max_iterations` iterations whatever the change (Graphalytics)
    pub fixed: bool,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            damping: 0.85,
            max_iterations: 20,
            tolerance: 1e-7,
            sources: Vec::new(),
            fixed: false,
        }
    }
}

pub struct Output {
    pub score: Vec<f64>,
    pub stats: Stats,
}

pub fn run(g: &Graph, p: &Params, par: &Par, ctl: &Control) -> Result<Output> {
    if !(0.0..1.0).contains(&p.damping) {
        return Err(bad("pageRank", "dampingFactor must be in [0, 1)"));
    }
    let n = g.n();
    let mut stats = Stats::new();
    if n == 0 {
        stats.insert("iterations".into(), 0.into());
        stats.insert("converged".into(), true.into());
        return Ok(Output {
            score: Vec::new(),
            stats,
        });
    }
    let incoming = g.inn(ctl)?;
    let inn = &*incoming;
    let out = &g.out;
    let weighted = out.w.is_some();
    let mut work = Work::new(ctl)?;
    let mut outw = Buffer::new("pagerank-out-weights", n, ctl)?;
    for u in 0..n as u32 {
        work.tick("pagerank-out-weights")?;
        outw.push(match out.wrow(u) {
            Some(w) => w.iter().map(|&x| x as f64).sum(),
            None => out.deg(u) as f64,
        })?;
    }
    let mut tele = Buffer::filled("pagerank-teleport", n, 0f64, ctl)?;
    if p.sources.is_empty() {
        tele.fill(1.0 / n as f64);
    } else {
        let mut srcs = Buffer::new("pagerank-sources", p.sources.len(), ctl)?;
        for &s in &p.sources {
            work.tick("pagerank-source-copy")?;
            srcs.push(s)?;
        }
        srcs.sort_unstable();
        // Deduplicate in place without changing the capacity charged for the clone.
        let mut kept = 0;
        for i in 0..srcs.len() {
            work.tick("pagerank-source-dedup")?;
            if kept == 0 || srcs[i] != srcs[kept - 1] {
                srcs[kept] = srcs[i];
                kept += 1;
            }
        }
        let share = 1.0 / kept as f64;
        for &s in &srcs[..kept] {
            work.tick("pagerank-personalize")?;
            tele[s as usize] = share;
        }
    }
    let mut score = Buffer::new("pagerank-score", n, ctl)?;
    for &v in &*tele {
        work.tick("pagerank-score-copy")?;
        score.push(v)?;
    }
    let mut contrib = Buffer::filled("pagerank-contribution", n, 0f64, ctl)?;
    let mut next = Buffer::filled("pagerank-next", n, 0f64, ctl)?;
    // Both reductions use the same fixed block boundaries and order as Par::blocks,
    // without an uncharged Result<Vec> collection or overlapping reduction buffers.
    let mut partials = Buffer::filled("pagerank-partials", n.div_ceil(BLOCK), 0f64, ctl)?;
    let d = p.damping;
    let mut iterations = 0u32;
    let mut delta = f64::INFINITY;
    let mut converged = false;
    for it in 0..p.max_iterations {
        ctl.check()?;
        // the rank of the nodes with no out-edges, which is spread over the teleport set
        par.chunks_mut(&mut partials, 1, |lo, c| {
            let start = lo * BLOCK;
            c[0] = (start..(start + BLOCK).min(n))
                .filter(|&u| outw[u] <= 0.0)
                .map(|u| score[u])
                .sum::<f64>();
            Ok(())
        })?;
        let dangling: f64 = partials.iter().copied().sum();
        // what each node sends along one unit of edge weight
        par.chunks_mut(&mut contrib, BLOCK, |lo, c| {
            for (i, x) in c.iter_mut().enumerate() {
                let u = lo + i;
                *x = if outw[u] > 0.0 {
                    score[u] / outw[u]
                } else {
                    0.0
                };
            }
            Ok(())
        })?;
        {
            let (contrib_ref, tele_ref) = (&contrib, &tele);
            par.chunks_mut(&mut next, BLOCK, |lo, c| {
                for (i, x) in c.iter_mut().enumerate() {
                    let v = lo + i;
                    let (r, w) = (inn.row(v as u32), inn.wrow(v as u32));
                    let mut s = 0.0;
                    match (weighted, w) {
                        (true, Some(w)) => {
                            for (k, &u) in r.iter().enumerate() {
                                s += contrib_ref[u as usize] * w[k] as f64;
                            }
                        }
                        _ => {
                            for &u in r {
                                s += contrib_ref[u as usize];
                            }
                        }
                    }
                    let t = tele_ref[v];
                    *x = (1.0 - d) * t + d * (s + dangling * t);
                }
                Ok(())
            })?;
        }
        par.chunks_mut(&mut partials, 1, |lo, c| {
            let start = lo * BLOCK;
            c[0] = (start..(start + BLOCK).min(n))
                .map(|v| (next[v] - score[v]).abs())
                .sum::<f64>();
            Ok(())
        })?;
        delta = partials.iter().copied().sum();
        std::mem::swap(&mut score, &mut next);
        iterations = it + 1;
        if !p.fixed && delta < p.tolerance {
            converged = true;
            break;
        }
    }
    if p.fixed {
        converged = true;
    }
    stats.insert("iterations".into(), iterations.into());
    stats.insert("converged".into(), converged.into());
    stats.insert("delta".into(), serde_json::json!(delta));
    ctl.check()?;
    Ok(Output {
        score: score.into_output(),
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    fn a1() -> Graph {
        // a b c d e f g = 0..7, h = 7
        digraph(8, &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 3), (5, 6)])
    }

    fn pr(g: &Graph, p: &Params, par: &Par) -> Vec<f64> {
        run(g, p, par, &Control::unlimited()).unwrap().score
    }

    #[test]
    fn scores_sum_to_one_and_rank_the_sink_cycle_first() {
        let g = a1();
        let p = Params {
            max_iterations: 100,
            ..Params::default()
        };
        let s = pr(&g, &p, &Par::single());
        assert!((s.iter().sum::<f64>() - 1.0).abs() < 1e-6);
        for x in [3, 4] {
            for y in [0, 1, 2] {
                assert!(s[x] > s[y], "{x} should outrank {y}: {s:?}");
            }
        }
    }

    #[test]
    fn personalized_stays_in_the_component() {
        let g = a1();
        let p = Params {
            max_iterations: 100,
            sources: vec![5],
            ..Params::default()
        };
        let s = pr(&g, &p, &Par::single());
        assert!(s[6] > 0.0);
        for u in [0, 1, 2, 3, 4, 7] {
            assert_eq!(s[u], 0.0);
        }
        assert!((s.iter().sum::<f64>() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn threads_do_not_change_the_result() {
        let n = 20_000u32;
        let mut e = Vec::new();
        let mut x = 12345u64;
        for _ in 0..80_000 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let a = (x >> 33) as u32 % n;
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let b = (x >> 33) as u32 % n;
            e.push((a, b));
        }
        let g = digraph(n, &e);
        let p = Params::default();
        let one = pr(&g, &p, &Par::single());
        let four = pr(&g, &p, &Par::with_threads(4));
        assert_eq!(one, four);
    }

    #[test]
    fn matches_dense_iteration() {
        let edges = [(0, 1), (0, 2), (1, 2), (2, 0), (3, 2)];
        let g = digraph(4, &edges);
        let p = Params {
            max_iterations: 200,
            tolerance: 1e-14,
            ..Params::default()
        };
        let s = pr(&g, &p, &Par::single());
        // dense reference
        let n = 4;
        let mut r = vec![0.25f64; n];
        for _ in 0..200 {
            let mut nr = vec![0.0; n];
            let mut outd = vec![0.0; n];
            for e in &edges {
                outd[e.0 as usize] += 1.0;
            }
            let dang: f64 = (0..n).filter(|&u| outd[u] == 0.0).map(|u| r[u]).sum();
            for e in &edges {
                nr[e.1 as usize] += r[e.0 as usize] / outd[e.0 as usize];
            }
            for v in 0..n {
                nr[v] = 0.15 / n as f64 + 0.85 * (nr[v] + dang / n as f64);
            }
            r = nr;
        }
        for v in 0..n {
            assert!((r[v] - s[v]).abs() < 1e-9);
        }
    }
    #[test]
    fn r7_workspace_fifth_array_is_charged() {
        let g = ugraph(4, &[(0, 1), (1, 2), (2, 3)]);
        let ctl = Control::new(Default::default(), None, 4 * 4 * 8);
        let result = run(&g, &Params::default(), &Par::single(), &ctl);
        assert!(
            matches!(result, Err(crate::Error::BudgetExceeded(_))),
            "expected fifth-array refusal"
        );
    }

    #[test]
    fn r7_workspace_personalized_clone_is_charged() {
        let g = ugraph(2, &[(0, 1)]);
        let ctl = Control::new(Default::default(), None, 88);
        let p = Params {
            sources: vec![0; 1000],
            ..Params::default()
        };
        let result = run(&g, &p, &Par::single(), &ctl);
        assert!(
            matches!(result, Err(crate::Error::BudgetExceeded(_))),
            "expected source-clone refusal"
        );
    }

    #[test]
    fn r7_reduction_workspace_is_charged() {
        let n = 4100;
        let g = ugraph(n, &[(0, 1)]);
        let ctl = Control::new(Default::default(), None, n as u64 * 5 * 8 + 15);
        let result = run(
            &g,
            &Params {
                max_iterations: 1,
                ..Params::default()
            },
            &Par::single(),
            &ctl,
        );
        assert!(
            matches!(result, Err(crate::Error::BudgetExceeded(_))),
            "expected reduction-buffer refusal"
        );
    }

    #[test]
    fn r7_reduction_fixed_bits() {
        use sha2::{Digest, Sha256};
        let n = 4100;
        let edges: Vec<_> = (0..n)
            .flat_map(|u| [(u, (u + 1) % n), (u, (u * 7 + 3) % n)])
            .collect();
        let g = digraph(n, &edges);
        let p = Params {
            max_iterations: 3,
            fixed: true,
            sources: vec![0, 0, 1299, 4099],
            ..Params::default()
        };
        for par in [Par::single(), Par::with_threads(4)] {
            let output = run(&g, &p, &par, &Control::unlimited()).unwrap();
            let mut digest = Sha256::new();
            for v in &output.score {
                digest.update(v.to_bits().to_le_bytes());
            }
            assert_eq!(
                digest
                    .finalize()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
                "d1dbaed2c3118f816315c0be3f20c21902955362fa903ccfda39dcbf6bbe7dd5"
            );
        }
    }
    #[test]
    fn r7_workspace_exact_peak_and_output_lifetime() {
        let g = ugraph(4, &[(0, 1), (1, 2)]);
        // Five four-element f64 arrays and one fixed-block partial.
        let expected = 5 * 4 * 8 + 8;
        let ctl = Control::new(Default::default(), None, expected);
        let output = run(&g, &Params::default(), &Par::single(), &ctl).unwrap();
        assert_eq!(ctl.peak(), expected);
        assert_eq!(ctl.remaining(), expected - 4 * 8);
        drop(output);
        assert_eq!(ctl.remaining(), expected - 4 * 8);
        let short = Control::new(Default::default(), None, expected - 1);
        assert!(matches!(
            run(&g, &Params::default(), &Par::single(), &short),
            Err(crate::Error::BudgetExceeded(_))
        ));
        assert_eq!(short.remaining(), expected - 1);
        let p = Params {
            sources: vec![0; 1000],
            ..Params::default()
        };
        // During source copying, only outw and tele accompany the full clone.
        let peak = 2 * 4 * 8 + 1000 * 4;
        let ctl = Control::new(Default::default(), None, peak);
        run(&g, &p, &Par::single(), &ctl).unwrap();
        assert_eq!(ctl.peak(), peak);
        let short = Control::new(Default::default(), None, peak - 1);
        assert!(matches!(
            run(&g, &p, &Par::single(), &short),
            Err(crate::Error::BudgetExceeded(_))
        ));
        assert_eq!(short.remaining(), peak - 1);
    }

    #[test]
    fn r7_workspace_allocation_failure_and_unwind_rollback() {
        use crate::algo::memory::probe::{self, Point};
        let g = ugraph(4, &[(0, 1)]);
        let p = Params {
            sources: vec![0, 0],
            ..Params::default()
        };
        for name in [
            "pagerank-out-weights",
            "pagerank-teleport",
            "pagerank-sources",
            "pagerank-score",
            "pagerank-contribution",
            "pagerank-next",
            "pagerank-partials",
        ] {
            let ctl = Control::new(Default::default(), None, 10000);
            let _scope = probe::install(|_| {}, Some(name));
            assert!(matches!(
                run(&g, &p, &Par::single(), &ctl),
                Err(crate::Error::Invalid(_))
            ));
            assert_eq!(ctl.remaining(), 10000, "{name}");
        }
        let ctl = Control::new(Default::default(), None, 10000);
        let _scope = probe::install(
            |point| {
                if let Point::Allocation("pagerank-next", _) = point {
                    panic!("test PageRank unwind");
                }
            },
            None,
        );
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(
                &g,
                &p,
                &Par::single(),
                &ctl
            )))
            .is_err()
        );
        assert_eq!(ctl.remaining(), 10000);
    }
}
