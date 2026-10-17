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
        let total: f64 = match out.wrow(u) {
            Some(w) => {
                for &weight in w {
                    work.tick("pagerank-weight-validation")?;
                    if !weight.is_finite() || weight < 0.0 {
                        return Err(bad(
                            "pageRank",
                            "edge weights must be finite and non-negative",
                        ));
                    }
                }
                w.iter().map(|&x| x as f64).sum()
            }
            None => out.deg(u) as f64,
        };
        if !total.is_finite() {
            return Err(bad("pageRank", "outgoing weight total is not finite"));
        }
        outw.push(total)?;
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
        #[cfg(test)]
        c1_probe::at(c1_probe::Event::Iteration, ctl);
        // The passes check the control at the start of each block of BLOCK nodes, and
        // the pass over the incoming rows also every CHECK_EVERY edges, so a hub row is
        // polled too. A check per node cost a clock read per node under a deadline.
        // the rank of the nodes with no out-edges, which is spread over the teleport set
        par.chunks_mut(&mut partials, 1, |lo, c| {
            ctl.check()?;
            let start = lo * BLOCK;
            c[0] = (start..(start + BLOCK).min(n))
                .filter(|&u| {
                    #[cfg(test)]
                    c1_probe::at(c1_probe::Event::Node(0), ctl);
                    outw[u] <= 0.0
                })
                .map(|u| score[u])
                .sum::<f64>();
            Ok(())
        })?;
        ctl.check()?;
        let dangling: f64 = partials.iter().copied().sum();
        if !dangling.is_finite() {
            return Err(bad("pageRank", "dangling rank total is not finite"));
        }
        // what each node sends along one unit of edge weight
        par.chunks_mut(&mut contrib, BLOCK, |lo, c| {
            ctl.check()?;
            for (i, x) in c.iter_mut().enumerate() {
                let u = lo + i;
                #[cfg(test)]
                c1_probe::at(c1_probe::Event::Node(1), ctl);
                *x = if outw[u] > 0.0 {
                    score[u] / outw[u]
                } else {
                    0.0
                };
                if !x.is_finite() {
                    return Err(bad("pageRank", "rank contribution is not finite"));
                }
            }
            Ok(())
        })?;
        ctl.check()?;
        {
            let (contrib_ref, tele_ref) = (&contrib, &tele);
            par.chunks_mut(&mut next, BLOCK, |lo, c| {
                let mut work = Work::new(ctl)?;
                for (i, x) in c.iter_mut().enumerate() {
                    let v = lo + i;
                    #[cfg(test)]
                    c1_probe::at(c1_probe::Event::Node(2), ctl);
                    let (r, w) = (inn.row(v as u32), inn.wrow(v as u32));
                    work.tick_n(r.len() + 1, "pagerank-pull")?;
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
                    if !x.is_finite() {
                        return Err(bad("pageRank", "rank is not finite"));
                    }
                }
                Ok(())
            })?;
        }
        ctl.check()?;
        par.chunks_mut(&mut partials, 1, |lo, c| {
            ctl.check()?;
            let start = lo * BLOCK;
            c[0] = (start..(start + BLOCK).min(n))
                .map(|v| {
                    #[cfg(test)]
                    c1_probe::at(c1_probe::Event::Node(3), ctl);
                    (next[v] - score[v]).abs()
                })
                .sum::<f64>();
            Ok(())
        })?;
        ctl.check()?;
        delta = partials.iter().copied().sum();
        if !delta.is_finite() {
            return Err(bad("pageRank", "rank change is not finite"));
        }
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
    // no iteration ran: the change of the last one is not known, and JSON has no
    // infinity, so the summary leaves `delta` out
    if iterations > 0 {
        stats.insert("delta".into(), serde_json::json!(delta));
    }
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

    fn r12_graph(weights: [f32; 2]) -> Graph {
        Graph::build(
            super::super::RawEdges {
                nodes: vec![0, 1, 2, 3],
                src: vec![0, 0],
                dst: vec![1, 2],
                w: Some(weights.to_vec()),
                ..Default::default()
            },
            super::super::Aggregate::None,
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap()
    }

    #[test]
    fn r12_native_invalid_weights_are_typed_and_release_workspace() {
        let mut accepted = Vec::new();
        for weights in [
            [-10.0, 11.0],
            [f32::NAN, 1.0],
            [f32::INFINITY, 1.0],
            [f32::NEG_INFINITY, 1.0],
        ] {
            let g = r12_graph(weights);
            let ctl = Control::new(Default::default(), None, 1024 * 1024);
            let result = run(&g, &Params::default(), &Par::single(), &ctl);
            let invalid = matches!(result, Err(crate::Error::Invalid(_)));
            eprintln!("weights={weights:?} typed_invalid={invalid}");
            drop(result);
            if invalid {
                assert_eq!(ctl.remaining(), 1024 * 1024);
            }
            if !invalid {
                accepted.push(weights);
            }
        }
        assert!(
            accepted.is_empty(),
            "invalid weights accepted: {accepted:?}"
        );
    }

    #[test]
    fn r12_zero_weights_and_isolated_nodes_remain_probabilities() {
        for weights in [[0.0, 0.0], [0.0, 3.0], [2.0, 3.0]] {
            let g = r12_graph(weights);
            let p = Params {
                max_iterations: 100,
                ..Params::default()
            };
            let output = run(&g, &p, &Par::single(), &Control::unlimited()).unwrap();
            assert!(output.score.iter().all(|v| v.is_finite() && *v >= 0.0));
            assert!((output.score.iter().sum::<f64>() - 1.0).abs() < 1e-12);
            assert!(output.score[3] > 0.0, "isolated node retains teleport mass");
            if weights == [0.0, 0.0] {
                assert_eq!(output.score, vec![0.25; 4]);
            }
        }
    }

    #[test]
    fn r12_positive_reference_bits_threads_one_two_four() {
        let g = r12_graph([2.0, 3.0]);
        let p = Params {
            max_iterations: 20,
            fixed: true,
            ..Params::default()
        };
        // Independent four-node recurrence, recorded before the domain repair.
        let mut expected = [0.25f64; 4];
        for _ in 0..20 {
            let dangling = expected[1] + expected[2] + expected[3];
            let contribution = expected[0] / 5.0;
            expected = [
                (1.0 - 0.85) * 0.25 + 0.85 * (dangling * 0.25),
                (1.0 - 0.85) * 0.25 + 0.85 * (contribution * 2.0 + dangling * 0.25),
                (1.0 - 0.85) * 0.25 + 0.85 * (contribution * 3.0 + dangling * 0.25),
                (1.0 - 0.85) * 0.25 + 0.85 * (dangling * 0.25),
            ];
        }
        for par in [Par::single(), Par::with_threads(2), Par::with_threads(4)] {
            let output = run(&g, &p, &par, &Control::unlimited()).unwrap();
            assert_eq!(
                output.score.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                expected.map(f64::to_bits)
            );
        }
    }

    #[test]
    fn r12_weight_validation_cancellation_and_deadline_release() {
        use crate::algo::memory::probe::{self, Point};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        let edges = 2 * super::super::CHECK_EVERY;
        let limit = 8 * 1024 * 1024;
        let g = Graph::build(
            super::super::RawEdges {
                nodes: vec![0, 1],
                src: vec![0; edges],
                dst: vec![1; edges],
                w: Some(vec![1.0; edges]),
                ..Default::default()
            },
            super::super::Aggregate::None,
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let visits = Arc::new(AtomicUsize::new(0));
        let ctl = Control::new(cancel.clone(), None, limit);
        let v = visits.clone();
        let _scope = probe::install(
            move |point| {
                if matches!(point, Point::Work("pagerank-weight-validation", _)) {
                    v.fetch_add(1, Ordering::Relaxed);
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            None,
        );
        assert!(matches!(
            run(&g, &Params::default(), &Par::single(), &ctl),
            Err(crate::Error::Cancelled)
        ));
        let visits = visits.load(Ordering::Relaxed);
        eprintln!("validation visits before cancellation: {visits}");
        assert!((1..=super::super::CHECK_EVERY).contains(&visits));
        assert_eq!(ctl.remaining(), limit);
        let expired = Control::new(Default::default(), Some(std::time::Instant::now()), limit);
        assert!(matches!(
            run(&g, &Params::default(), &Par::single(), &expired),
            Err(crate::Error::Timeout)
        ));
        assert_eq!(expired.remaining(), limit);
    }

    fn a1() -> Graph {
        // a b c d e f g = 0..7, h = 7
        digraph(8, &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 3), (5, 6)])
    }

    fn pr(g: &Graph, p: &Params, par: &Par) -> Vec<f64> {
        run(g, p, par, &Control::unlimited()).unwrap().score
    }

    /// With no iteration the change is unknown. JSON has no infinity, so the summary
    /// leaves `delta` out instead of writing null.
    #[test]
    fn zero_iterations_report_no_delta() {
        let p = Params {
            max_iterations: 0,
            ..Params::default()
        };
        let out = run(&a1(), &p, &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(out.stats["iterations"], 0);
        assert!(!out.stats.contains_key("delta"), "{:?}", out.stats);
        let p = Params {
            max_iterations: 1,
            ..Params::default()
        };
        let out = run(&a1(), &p, &Par::single(), &Control::unlimited()).unwrap();
        assert!(out.stats["delta"].as_f64().is_some_and(f64::is_finite));
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

#[cfg(test)]
mod c1_probe;
#[cfg(test)]
mod polling_tests;
