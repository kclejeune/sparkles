//! node2vec embeddings (spec F14 §4; Grover and Leskovec, KDD 2016).
//!
//! Biased second-order random walks feed a skip-gram model with negative sampling. A walk
//! at node `v` that came from `t` picks a neighbour `x` with probability proportional to
//! the edge weight times `1/p` when `x` is `t`, 1 when `x` is a neighbour of `t`, and
//! `1/q` otherwise. Training is sequential over the walks in a fixed order, so a seed gives
//! one result. The `concurrency` parameter does not apply to it.

use super::{CHECK_EVERY, Control, Graph, Rng, bad};
use crate::error::Result;

#[derive(Clone, Debug)]
pub struct Params {
    pub dimension: usize,
    pub walk_length: usize,
    pub walks_per_node: usize,
    pub window: usize,
    /// p: the weight of returning to the previous node is `1 / return_factor`
    pub return_factor: f64,
    /// q: the weight of moving away is `1 / in_out_factor`
    pub in_out_factor: f64,
    pub negative_samples: usize,
    pub epochs: usize,
    pub learning_rate: f64,
    pub seed: u64,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            dimension: 128,
            walk_length: 80,
            walks_per_node: 10,
            window: 10,
            return_factor: 1.0,
            in_out_factor: 1.0,
            negative_samples: 5,
            epochs: 1,
            learning_rate: 0.025,
            seed: 0,
        }
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x.clamp(-30.0, 30.0)).exp())
}

/// One step of a walk from `cur`, which it reached from `prev` (`None` at the start).
fn step(
    g: &Graph,
    p: &Params,
    prev: Option<u32>,
    cur: u32,
    rng: &mut Rng,
    buf: &mut Vec<f64>,
) -> Option<u32> {
    let row = g.out.row(cur);
    if row.is_empty() {
        return None;
    }
    let w = g.out.wrow(cur);
    buf.clear();
    let mut total = 0.0;
    for (i, &x) in row.iter().enumerate() {
        let mut a = w.map_or(1.0, |w| w[i] as f64);
        if let Some(t) = prev {
            a *= if x == t {
                1.0 / p.return_factor
            } else if g.out.has(t, x) {
                1.0
            } else {
                1.0 / p.in_out_factor
            };
        }
        total += a;
        buf.push(total);
    }
    if total <= 0.0 {
        return None;
    }
    let r = rng.next_f64() * total;
    let i = buf.partition_point(|&c| c <= r).min(row.len() - 1);
    Some(row[i])
}

/// The embeddings, `dimension` values for each node, in node order.
pub fn run(g: &Graph, p: &Params, ctl: &Control) -> Result<Vec<f32>> {
    let d = p.dimension;
    if d == 0 {
        return Err(bad("node2vec", "dimension must be above 0"));
    }
    if p.return_factor <= 0.0 || p.in_out_factor <= 0.0 {
        return Err(bad(
            "node2vec",
            "returnFactor and inOutFactor must be above 0",
        ));
    }
    if p.walk_length < 2 {
        return Err(bad("node2vec", "walkLength must be at least 2"));
    }
    let n = g.n();
    ctl.charge(n as u64 * d as u64 * 8 + n as u64 * 8)?;
    let mut syn0 = vec![0f32; n * d];
    for u in 0..n {
        let mut rng = Rng::for_item(p.seed, u as u64);
        for x in &mut syn0[u * d..(u + 1) * d] {
            *x = ((rng.next_f64() - 0.5) / d as f64) as f32;
        }
    }
    let mut syn1 = vec![0f32; n * d];
    // the noise distribution: degree to the 3/4
    let inn = g.inn();
    let mut cum: Vec<f64> = Vec::with_capacity(n);
    let mut acc = 0.0;
    for u in 0..n as u32 {
        acc += ((g.out.deg(u) + if g.symmetric { 0 } else { inn.deg(u) }) as f64 + 1.0).powf(0.75);
        cum.push(acc);
    }
    let total_steps = (p.epochs * p.walks_per_node * n).max(1);
    let mut done = 0usize;
    let mut walk: Vec<u32> = Vec::with_capacity(p.walk_length);
    let mut buf: Vec<f64> = Vec::new();
    let mut neu1e = vec![0f32; d];
    let mut order: Vec<u32> = (0..n as u32).collect();
    let mut shuffle = Rng::new(p.seed ^ 0xABCD);
    for epoch in 0..p.epochs {
        for rep in 0..p.walks_per_node {
            // a new order of starting nodes for each pass
            for i in (1..n).rev() {
                order.swap(i, shuffle.below(i + 1));
            }
            for (k, &start) in order.iter().enumerate() {
                if k % 256 == 0 || k % CHECK_EVERY == 0 {
                    ctl.check()?;
                }
                let item = ((epoch * p.walks_per_node + rep) * n) as u64 + start as u64;
                let mut rng = Rng::for_item(p.seed ^ 0x5555, item);
                walk.clear();
                walk.push(start);
                let mut prev = None;
                let mut cur = start;
                while walk.len() < p.walk_length {
                    match step(g, p, prev, cur, &mut rng, &mut buf) {
                        Some(next) => {
                            walk.push(next);
                            prev = Some(cur);
                            cur = next;
                        }
                        None => break,
                    }
                }
                if walk.len() < 2 {
                    done += 1;
                    continue;
                }
                let alpha = (p.learning_rate * (1.0 - done as f64 / total_steps as f64))
                    .max(p.learning_rate * 1e-4) as f32;
                done += 1;
                for i in 0..walk.len() {
                    let center = walk[i] as usize;
                    // a window that shrinks at random, as word2vec does
                    let b = rng.below(p.window.max(1));
                    let reach = p.window.max(1) - b;
                    let lo = i.saturating_sub(reach);
                    let hi = (i + reach + 1).min(walk.len());
                    for (j, &ctx_node) in walk.iter().enumerate().take(hi).skip(lo) {
                        if j == i {
                            continue;
                        }
                        // the center word's input vector learns to predict the context
                        let l1 = center * d;
                        neu1e.fill(0.0);
                        for s in 0..=p.negative_samples {
                            let (target, label) = if s == 0 {
                                (ctx_node as usize, 1.0f32)
                            } else {
                                let r = rng.next_f64() * acc;
                                let t = cum.partition_point(|&c| c <= r).min(n - 1);
                                if t == ctx_node as usize {
                                    continue;
                                }
                                (t, 0.0)
                            };
                            let l2 = target * d;
                            let mut dot = 0f32;
                            for c in 0..d {
                                dot += syn0[l1 + c] * syn1[l2 + c];
                            }
                            let gr = (label - sigmoid(dot)) * alpha;
                            for c in 0..d {
                                neu1e[c] += gr * syn1[l2 + c];
                                syn1[l2 + c] += gr * syn0[l1 + c];
                            }
                        }
                        for c in 0..d {
                            syn0[l1 + c] += neu1e[c];
                        }
                    }
                }
            }
        }
    }
    Ok(syn0)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (na * nb)
    }

    #[test]
    fn deterministic_and_separates_two_cliques() {
        let mut e = Vec::new();
        for a in 0..6u32 {
            for b in a + 1..6 {
                e.push((a, b));
                e.push((a + 6, b + 6));
            }
        }
        e.push((5, 6));
        let g = ugraph(12, &e);
        let p = Params {
            dimension: 16,
            walk_length: 20,
            walks_per_node: 20,
            window: 4,
            epochs: 2,
            seed: 5,
            ..Params::default()
        };
        let a = run(&g, &p, &Control::unlimited()).unwrap();
        let b = run(&g, &p, &Control::unlimited()).unwrap();
        assert_eq!(a, b);
        let v = |i: usize| &a[i * 16..(i + 1) * 16];
        let within =
            (cosine(v(0), v(1)) + cosine(v(2), v(3)) + cosine(v(8), v(9)) + cosine(v(10), v(11)))
                / 4.0;
        let across =
            (cosine(v(0), v(11)) + cosine(v(1), v(10)) + cosine(v(2), v(9)) + cosine(v(3), v(8)))
                / 4.0;
        assert!(within > across, "within {within} across {across}");
    }
}
