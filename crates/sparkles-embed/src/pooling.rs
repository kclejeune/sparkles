//! Pooling of token states into one vector, normalization and truncation, on plain
//! `f32` buffers. The definitions follow sentence-transformers' `models.Pooling`.

use serde::{Deserialize, Serialize};

/// How token states become one vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pooling {
    /// The first token's state (`[CLS]`).
    Cls,
    /// The mean of the states of the tokens the mask keeps.
    Mean,
    /// The element-wise maximum over the kept tokens.
    Max,
    /// The sum of the kept states divided by the square root of their count.
    #[serde(rename = "mean_sqrt_len")]
    MeanSqrtLen,
    /// The state of the last kept token (decoder models).
    #[serde(rename = "lasttoken")]
    LastToken,
}

/// Pool `states`, a row-major `(batch, len, dim)` buffer, under `mask`, a row-major
/// `(batch, len)` buffer of 0 and 1. A row without kept tokens pools to zeros.
pub fn pool(
    pooling: Pooling,
    states: &[f32],
    mask: &[u32],
    batch: usize,
    len: usize,
    dim: usize,
) -> Vec<Vec<f32>> {
    assert_eq!(states.len(), batch * len * dim);
    assert_eq!(mask.len(), batch * len);
    let tok = |b: usize, t: usize| &states[(b * len + t) * dim..(b * len + t + 1) * dim];
    (0..batch)
        .map(|b| {
            let m = &mask[b * len..(b + 1) * len];
            let kept: Vec<usize> = (0..len).filter(|&t| m[t] != 0).collect();
            let mut out = vec![0f32; dim];
            match pooling {
                Pooling::Cls => {
                    if let Some(&t) = kept.first() {
                        out.copy_from_slice(tok(b, t));
                    }
                }
                Pooling::LastToken => {
                    if let Some(&t) = kept.last() {
                        out.copy_from_slice(tok(b, t));
                    }
                }
                Pooling::Mean | Pooling::MeanSqrtLen => {
                    for &t in &kept {
                        for (o, x) in out.iter_mut().zip(tok(b, t)) {
                            *o += *x;
                        }
                    }
                    let n = kept.len().max(1) as f32;
                    let d = if pooling == Pooling::Mean {
                        n
                    } else {
                        n.sqrt()
                    };
                    for o in &mut out {
                        *o /= d;
                    }
                }
                Pooling::Max => {
                    if !kept.is_empty() {
                        out.fill(f32::NEG_INFINITY);
                        for &t in &kept {
                            for (o, x) in out.iter_mut().zip(tok(b, t)) {
                                *o = o.max(*x);
                            }
                        }
                    }
                }
            }
            out
        })
        .collect()
}

/// Scale `v` to unit length. A zero vector stays zero.
pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v {
            *x /= n;
        }
    }
}

/// Cosine similarity.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Matryoshka truncation: keep the first `dim` components, then renormalize when the
/// model's vectors are normalized.
pub fn truncate(v: &mut Vec<f32>, dim: usize, renormalize: bool) {
    if dim < v.len() {
        v.truncate(dim);
        if renormalize {
            normalize(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // two rows of three tokens, dimension 2; row 0 has a padded last token, row 1 a
    // padded first token (left padding)
    const S: [f32; 12] = [
        1., 2., 3., 4., 100., 100., /* row 1 */ -9., -9., 5., 1., 7., 3.,
    ];
    const M: [u32; 6] = [1, 1, 0, 0, 1, 1];

    #[test]
    fn modes() {
        let p = |m| pool(m, &S, &M, 2, 3, 2);
        assert_eq!(p(Pooling::Cls), vec![vec![1., 2.], vec![5., 1.]]);
        assert_eq!(p(Pooling::LastToken), vec![vec![3., 4.], vec![7., 3.]]);
        assert_eq!(p(Pooling::Mean), vec![vec![2., 3.], vec![6., 2.]]);
        assert_eq!(p(Pooling::Max), vec![vec![3., 4.], vec![7., 3.]]);
        let s = 2f32.sqrt();
        assert_eq!(
            p(Pooling::MeanSqrtLen),
            vec![vec![4. / s, 6. / s], vec![12. / s, 4. / s]]
        );
    }

    #[test]
    fn norms() {
        let mut v = vec![3., 4., 12.];
        normalize(&mut v);
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-6);
        assert!((v[0] - 3. / 13.).abs() < 1e-6);
        let mut w = vec![3., 4., 12.];
        truncate(&mut w, 2, true);
        assert_eq!(w, vec![0.6, 0.8]);
        let mut z = vec![0., 0.];
        normalize(&mut z);
        assert_eq!(z, vec![0., 0.]);
        assert!((cosine(&[1., 0.], &[1., 1.]) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }
}
