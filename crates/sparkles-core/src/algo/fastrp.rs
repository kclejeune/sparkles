//! FastRP node embeddings by very sparse random projection (spec F14 §4; Chen et al.,
//! CIKM 2019).
//!
//! Every node starts with a random vector whose entries are `+√3`, `-√3` or 0 with
//! probabilities 1/6, 1/6 and 2/3, drawn from a generator seeded by the run's seed and the
//! node, so the vector does not depend on the number of threads. Round `k` replaces each
//! node's vector with the weighted mean of its neighbours' vectors from round `k - 1`.
//! The embedding is the sum of the L2-normalized rounds, each times its iteration weight,
//! normalized once more.

use super::memory::Buffer;
use super::{BLOCK, Control, Graph, Par, Rng, bad};
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct Params {
    pub dimension: usize,
    /// the weight of round 0, 1, ...: round 0 is the random vectors themselves
    pub iteration_weights: Vec<f64>,
    /// the random vector of a node is scaled by its degree to this power
    pub normalization_strength: f64,
    pub seed: u64,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            dimension: 128,
            iteration_weights: vec![0.0, 1.0, 1.0],
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
    if p.iteration_weights.is_empty() {
        return Err(bad("fastRP", "iterationWeights must not be empty"));
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
    add(&mut sum, &prev, p.iteration_weights[0], par)?;
    let mut cur = Buffer::<f32>::zeroed("fastrp-next", len, ctl)?;
    for &weight in &p.iteration_weights[1..] {
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
