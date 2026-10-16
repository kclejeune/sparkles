//! FastRP node embeddings by very sparse random projection (spec F14 §4; Chen et al.,
//! CIKM 2019).
//!
//! Every node starts with a random vector whose entries are `+√3`, `-√3` or 0 with
//! probabilities 1/6, 1/6 and 2/3, drawn from a generator seeded by the run's seed and the
//! node, so the vector does not depend on the number of threads. Round `k` replaces each
//! node's vector with the weighted mean of its neighbours' vectors from round `k - 1`.
//!
//! The parameters mean what they do in Neo4j GDS. The length of `iterationWeights` is the
//! number of propagations, and `iterationWeights[i]` weights round `i + 1`. The random
//! vectors are round 0, and `nodeSelfInfluence` is its weight, so it behaves like an
//! iteration weight for a 0th iteration. The embedding is the sum of the L2-normalized
//! rounds, each times its weight, normalized once more.

use super::memory::Buffer;
use super::{BLOCK, Control, Graph, Par, Rng, bad};
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct Params {
    pub dimension: usize,
    /// the weight of round 1, 2, ...: one propagation for each entry
    pub iteration_weights: Vec<f64>,
    /// the weight of round 0, the random vectors themselves
    pub node_self_influence: f64,
    /// the random vector of a node is scaled by its degree to this power
    pub normalization_strength: f64,
    pub seed: u64,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            dimension: 128,
            iteration_weights: vec![0.0, 1.0, 1.0],
            node_self_influence: 0.0,
            normalization_strength: 0.0,
            seed: 0,
        }
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if norm > 0.0 {
        let inv = (1.0 / norm) as f32;
        for x in v {
            *x *= inv;
        }
    }
}

/// The embeddings, `dimension` values for each node, in node order.
pub fn run(g: &Graph, p: &Params, par: &Par, ctl: &Control) -> Result<Vec<f32>> {
    let d = p.dimension;
    if d == 0 {
        return Err(bad("fastRP", "dimension must be above 0"));
    }
    if p.iteration_weights.is_empty() && p.node_self_influence == 0.0 {
        return Err(bad(
            "fastRP",
            "iterationWeights must not be empty unless nodeSelfInfluence is not zero",
        ));
    }
    if d > crate::vector::MAX_DIM {
        return Err(bad(
            "fastRP",
            format!("dimension must be at most {}", crate::vector::MAX_DIM),
        ));
    }
    let n = g.n();
    let len = n
        .checked_mul(d)
        .filter(|len| len.checked_mul(std::mem::size_of::<f32>() * 3).is_some())
        .ok_or_else(|| bad("fastRP", "the embeddings of this graph are too large"))?;
    // the vectors of the last round, of this round and the sum, each charged to the call
    // before it is allocated
    let sqrt_s = 3f32.sqrt();
    let mut prev = Buffer::<f32>::zeroed("fastrp-round", len, ctl)?;
    par.chunks_mut(&mut prev, (BLOCK / 8).max(1) * d, |lo, chunk| {
        ctl.check()?;
        for (i, v) in chunk.chunks_mut(d).enumerate() {
            let u = lo / d + i;
            let mut rng = Rng::for_item(p.seed, u as u64);
            for x in v.iter_mut() {
                let r = rng.next_f64();
                *x = if r < 1.0 / 6.0 {
                    sqrt_s
                } else if r < 1.0 / 3.0 {
                    -sqrt_s
                } else {
                    0.0
                };
            }
            if p.normalization_strength != 0.0 {
                let deg = (g.out.deg(u as u32) as f64).max(1.0);
                let scale = deg.powf(p.normalization_strength) as f32;
                for x in v.iter_mut() {
                    *x *= scale;
                }
            }
        }
        Ok(())
    })?;
    let mut sum = Buffer::<f32>::zeroed("fastrp-sum", len, ctl)?;
    let add = |sum: &mut [f32], round: &[f32], weight: f64, par: &Par| -> Result<()> {
        if weight == 0.0 {
            return Ok(());
        }
        par.chunks_mut(sum, (BLOCK / 8).max(1) * d, |lo, chunk| {
            for (i, v) in chunk.chunks_mut(d).enumerate() {
                let u = lo / d + i;
                let mut r = round[u * d..(u + 1) * d].to_vec();
                normalize(&mut r);
                for (s, x) in v.iter_mut().zip(&r) {
                    *s += x * weight as f32;
                }
            }
            Ok(())
        })
    };
    add(&mut sum, &prev, p.node_self_influence, par)?;
    // the propagated round needs a second array only when there is a propagation
    let mut cur = Buffer::<f32>::zeroed(
        "fastrp-next",
        if p.iteration_weights.is_empty() {
            0
        } else {
            len
        },
        ctl,
    )?;
    for &weight in &p.iteration_weights {
        ctl.check()?;
        let prev_ref = &*prev;
        par.chunks_mut(&mut cur, (BLOCK / 8).max(1) * d, |lo, chunk| {
            for (i, v) in chunk.chunks_mut(d).enumerate() {
                let u = (lo / d + i) as u32;
                v.fill(0.0);
                let (row, w) = (g.out.row(u), g.out.wrow(u));
                let total: f64 = match w {
                    Some(w) => w.iter().map(|&x| x as f64).sum(),
                    None => row.len() as f64,
                };
                if total <= 0.0 {
                    continue;
                }
                for (k, &x) in row.iter().enumerate() {
                    let share = (w.map_or(1.0, |w| w[k] as f64) / total) as f32;
                    let src = &prev_ref[x as usize * d..(x as usize + 1) * d];
                    for (a, b) in v.iter_mut().zip(src) {
                        *a += b * share;
                    }
                }
            }
            Ok(())
        })?;
        std::mem::swap(&mut prev, &mut cur);
        add(&mut sum, &prev, weight, par)?;
    }
    par.chunks_mut(&mut sum, (BLOCK / 8).max(1) * d, |_, chunk| {
        for v in chunk.chunks_mut(d) {
            normalize(v);
        }
        Ok(())
    })?;
    Ok(sum.into_output())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    fn two_cliques() -> Graph {
        let mut e = Vec::new();
        for a in 0..6u32 {
            for b in a + 1..6 {
                e.push((a, b));
                e.push((a + 6, b + 6));
            }
        }
        e.push((5, 6));
        ugraph(12, &e)
    }

    #[test]
    fn deterministic_and_thread_independent() {
        let g = two_cliques();
        let p = Params {
            dimension: 16,
            seed: 7,
            ..Params::default()
        };
        let a = run(&g, &p, &Par::single(), &Control::unlimited()).unwrap();
        let b = run(&g, &p, &Par::with_threads(3), &Control::unlimited()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 12 * 16);
        let c = run(
            &g,
            &Params { seed: 8, ..p },
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        assert_ne!(a, c);
    }

    #[test]
    fn a_dimension_beyond_a_vector_index_is_refused() {
        let g = two_cliques();
        for dimension in [crate::vector::MAX_DIM + 1, usize::MAX, 1 << 62] {
            let p = Params {
                dimension,
                ..Params::default()
            };
            let e = run(&g, &p, &Par::single(), &Control::unlimited()).unwrap_err();
            assert!(e.to_string().contains("dimension"), "{e}");
        }
    }

    fn path3() -> Graph {
        ugraph(3, &[(0, 1), (1, 2)])
    }

    fn embed(g: &Graph, weights: &[f64], self_influence: f64) -> Vec<f32> {
        let p = Params {
            dimension: 32,
            iteration_weights: weights.to_vec(),
            node_self_influence: self_influence,
            seed: 5,
            ..Params::default()
        };
        run(g, &p, &Par::single(), &Control::unlimited()).unwrap()
    }

    fn close(a: &[f32], b: &[f32]) {
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            assert!((x - y).abs() < 1e-5, "{a:?} != {b:?}");
        }
    }

    /// The length of `iterationWeights` is the number of propagations, as in GDS, so the
    /// random vectors are not weighted by `iterationWeights[0]`.
    #[test]
    fn one_iteration_weight_is_one_propagation() {
        let g = path3();
        let d = 32;
        // the normalized random vectors, which are round 0 alone
        let random = embed(&g, &[], 1.0);
        let one = embed(&g, &[1.0], 0.0);
        // node 0 has the single neighbour 1, so its first round is the random vector of 1
        close(&one[..d], &random[d..2 * d]);
        // node 2 has the single neighbour 1 as well
        close(&one[2 * d..], &random[d..2 * d]);
        // and the old reading, where the first weight scaled the random vectors, is gone
        assert!(
            one[..d]
                .iter()
                .zip(&random[..d])
                .any(|(a, b)| (a - b).abs() > 1e-3)
        );
    }

    #[test]
    fn node_self_influence_weights_the_random_vectors() {
        let g = path3();
        let d = 32;
        let random = embed(&g, &[], 1.0);
        // a lone round 0 is the normalized random vector, whatever its weight
        close(&embed(&g, &[], 3.0), &random);
        // a zero iteration weight adds nothing
        close(&embed(&g, &[0.0], 1.0), &random);
        // both rounds, with equal weights, are not either one alone
        let both = embed(&g, &[1.0], 1.0);
        let one = embed(&g, &[1.0], 0.0);
        assert_ne!(&both[..d], &one[..d]);
        assert_ne!(&both[..d], &random[..d]);
    }

    #[test]
    fn empty_iteration_weights_need_a_node_self_influence() {
        let p = Params {
            iteration_weights: vec![],
            ..Params::default()
        };
        let e = run(&path3(), &p, &Par::single(), &Control::unlimited()).unwrap_err();
        assert!(e.to_string().contains("iterationWeights"), "{e}");
    }

    #[test]
    fn the_default_runs_three_propagations() {
        // the GDS default (0.0, 1.0, 1.0) with no self influence weights A^2 and A^3
        let g = two_cliques();
        let p = Params {
            dimension: 16,
            seed: 2,
            ..Params::default()
        };
        let a = run(&g, &p, &Par::single(), &Control::unlimited()).unwrap();
        let b = run(
            &g,
            &Params {
                iteration_weights: vec![0.0, 1.0, 1.0],
                node_self_influence: 0.0,
                ..p.clone()
            },
            &Par::with_threads(3),
            &Control::unlimited(),
        )
        .unwrap();
        assert_eq!(a, b);
        let two = run(
            &g,
            &Params {
                iteration_weights: vec![0.0, 1.0],
                ..p
            },
            &Par::single(),
            &Control::unlimited(),
        )
        .unwrap();
        assert_ne!(a, two);
    }

    #[test]
    fn nodes_of_a_clique_are_closer() {
        let g = two_cliques();
        let p = Params {
            dimension: 64,
            iteration_weights: vec![0.0, 1.0, 1.0, 1.0],
            seed: 3,
            ..Params::default()
        };
        let e = run(&g, &p, &Par::single(), &Control::unlimited()).unwrap();
        let v = |i: usize| &e[i * 64..(i + 1) * 64];
        // 0 and 1 share a clique, 0 and 11 do not
        assert!(cosine(v(0), v(1)) > cosine(v(0), v(11)));
        assert!(cosine(v(11), v(10)) > cosine(v(11), v(1)));
    }
}
