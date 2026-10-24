//! Two-dimensional projections of a vector index (spec U11 §3.3).
//!
//! * **PCA.** The two leading principal components are found from a sample of up to
//!   [`MAX_SAMPLE`] vectors by randomized subspace iteration: a block of
//!   [`BLOCK`] directions is multiplied by the sample's covariance a few times and
//!   orthonormalized, and the small matrix of the block is then diagonalized exactly
//!   with Jacobi rotations. Every vector of the index is projected on the two components.
//!   Under the cosine metric the vectors are normalized first, so that direction alone
//!   places them.
//! * **Entities.** A subject with several vectors under the predicate, in several graphs or
//!   with several values, is placed at the mean of their projections.
//! * **Neighbour layout.** The index's HNSW graph links each vector to its near neighbours
//!   on its bottom layer. The layout starts from the PCA positions and moves each entity
//!   toward its neighbours and away from crowded cells of a grid, a simplified form of
//!   ForceAtlas2's attraction and repulsion. The steps are computed from the positions of
//!   the step before, so the result depends only on the seed.
//! * **Answers.** A projection keeps, per entity, its subject id, its position, its class
//!   and a sampling priority, 24 bytes in all. A region's points are those of lowest
//!   priority inside it, so zooming in keeps the points already shown and adds others.

use super::index::Segment;
use super::{Metric, from_key};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::Perm;
use crate::store::Chunk;
use crate::store::Snapshot;
use rayon::prelude::*;
use rustc_hash::FxHashMap;

/// The most vectors the PCA learns its components from.
pub const MAX_SAMPLE: usize = 100_000;
/// The most points an answer holds.
pub const MAX_POINTS: usize = 50_000;
/// The cells of a side of the density grid.
pub const GRID: usize = 256;
/// The directions of the subspace iteration: the two components and six more, which make
/// the two leading ones converge in a few rounds.
const BLOCK: usize = 8;
/// Rounds of subspace iteration.
const ROUNDS: usize = 8;
/// No class, in [`Projection::classes`].
pub const NO_CLASS: u32 = u32::MAX;
/// Bytes a projection keeps per entity: subject, position, class and priority.
pub const BYTES_PER_POINT: u64 = 24;

/// The bottom-layer neighbours of a graph node, written to the vector given.
pub type Links<'a> = &'a (dyn Fn(u32, &mut Vec<u32>) + Sync);

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// How a projection places the vectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectionMethod {
    /// the two leading principal components
    Pca,
    /// a force layout of the index's neighbour graph, seeded by the PCA
    Neighbours,
}

impl ProjectionMethod {
    pub fn name(self) -> &'static str {
        match self {
            ProjectionMethod::Pca => "pca",
            ProjectionMethod::Neighbours => "neighbours",
        }
    }

    pub fn parse(s: &str) -> Option<ProjectionMethod> {
        match s {
            "pca" => Some(ProjectionMethod::Pca),
            "neighbours" | "neighbors" => Some(ProjectionMethod::Neighbours),
            _ => None,
        }
    }
}

/// What a projection computes.
#[derive(Clone, Debug)]
pub struct ProjectionConfig {
    pub method: ProjectionMethod,
    /// vectors the PCA learns from, at most [`MAX_SAMPLE`]
    pub sample: usize,
    /// the seed of the sample, the first directions and the layout
    pub seed: u64,
    /// steps of the neighbour layout
    pub iterations: usize,
}

impl Default for ProjectionConfig {
    fn default() -> Self {
        ProjectionConfig {
            method: ProjectionMethod::Pca,
            sample: MAX_SAMPLE,
            seed: 0x5eed,
            iterations: 100,
        }
    }
}

/// A two-dimensional projection of an index's entities.
#[derive(Clone)]
pub struct Projection {
    pub index: String,
    pub method: ProjectionMethod,
    /// the snapshot the projection read, whose dictionary names its subjects
    pub snapshot: Snapshot,
    /// vectors (rows) projected
    pub vectors: u64,
    /// the dimension of the vectors
    pub dimension: usize,
    /// the subject of each point, sorted
    pub subjects: Vec<u64>,
    pub xy: Vec<[f32; 2]>,
    /// the index into `class_ids` of each point's class, or [`NO_CLASS`]
    pub classes: Vec<u32>,
    /// the classes, by index
    pub class_ids: Vec<u64>,
    /// points per class, by index
    pub class_counts: Vec<u64>,
    /// the sampling priority of each point: lower is shown first
    pub priority: Vec<u32>,
    /// `[min x, min y, max x, max y]`
    pub bounds: [f32; 4],
    /// the share of the sample's variance on each component (PCA)
    pub explained: [f64; 2],
    pub built_ms: f64,
}

impl Projection {
    pub fn len(&self) -> usize {
        self.subjects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.subjects.is_empty()
    }

    pub fn commit(&self) -> u64 {
        self.snapshot.commit
    }

    pub fn generation(&self) -> &str {
        &self.snapshot.generation.name
    }

    /// The bytes it keeps, besides the snapshot it shares with the store.
    pub fn bytes(&self) -> u64 {
        self.len() as u64 * BYTES_PER_POINT + self.class_ids.len() as u64 * 16
    }

    /// The point of `subject`, if it has one.
    pub fn point_of(&self, subject: u64) -> Option<usize> {
        self.subjects.binary_search(&subject).ok()
    }

    /// The points inside `bbox` (`[min x, min y, max x, max y]`, all of them when `None`),
    /// and how many there are: at most `max`, those of lowest priority, in point order.
    pub fn points_in(&self, bbox: Option<[f32; 4]>, max: usize) -> (Vec<usize>, usize) {
        let inside = |p: &[f32; 2]| match bbox {
            None => true,
            Some([x0, y0, x1, y1]) => p[0] >= x0 && p[0] <= x1 && p[1] >= y0 && p[1] <= y1,
        };
        let mut all: Vec<usize> = (0..self.len()).filter(|&i| inside(&self.xy[i])).collect();
        let total = all.len();
        if all.len() > max {
            if max == 0 {
                return (Vec::new(), total);
            }
            all.select_nth_unstable_by_key(max - 1, |&i| (self.priority[i], i));
            all.truncate(max);
            all.sort_unstable();
        }
        (all, total)
    }

    /// The counts of points in a `GRID` by `GRID` grid over `bbox` (the bounds when
    /// `None`), row by row from the minimum y.
    pub fn density(&self, bbox: Option<[f32; 4]>) -> Vec<u32> {
        let [x0, y0, x1, y1] = bbox.unwrap_or(self.bounds);
        let (w, h) = (
            (x1 - x0).max(f32::MIN_POSITIVE),
            (y1 - y0).max(f32::MIN_POSITIVE),
        );
        let mut grid = vec![0u32; GRID * GRID];
        for p in &self.xy {
            if p[0] < x0 || p[0] > x1 || p[1] < y0 || p[1] > y1 {
                continue;
            }
            let cx = (((p[0] - x0) / w) * GRID as f32) as usize;
            let cy = (((p[1] - y0) / h) * GRID as f32) as usize;
            grid[cy.min(GRID - 1) * GRID + cx.min(GRID - 1)] += 1;
        }
        grid
    }
}

/// One vector of the index to project: its subject and its values.
struct Row<'a> {
    s: u64,
    v: std::borrow::Cow<'a, [f32]>,
}

/// Compute the projection of `seg` (the base rows of index `name` over predicate `pred`)
/// and the vectors that `snap` inserted, minus those it deleted. `graph_links` gives the
/// bottom-layer neighbours of each graph node and `node_rows` the first row of each node,
/// for the neighbour layout. `check` is called now and then, and its error stops the work;
/// `progress` hears the share done.
#[allow(clippy::too_many_arguments)]
pub fn project(
    snap: &Snapshot,
    name: &str,
    pred: u64,
    seg: &Segment,
    metric: Metric,
    cfg: &ProjectionConfig,
    neighbours: Option<Links<'_>>,
    node_rows: &[u32],
    check: &(dyn Fn() -> Result<()> + Sync),
    progress: &(dyn Fn(f64) + Sync),
) -> Result<Projection> {
    let started = std::time::Instant::now();
    if cfg.sample == 0 || cfg.sample > MAX_SAMPLE {
        return Err(Error::invalid(format!(
            "sample must be from 1 to {MAX_SAMPLE}"
        )));
    }
    let dim = seg.dim;
    let pi = Perm::Pso.index();
    let lo = [pred, 0, 0, 0];
    let hi = [pred, u64::MAX, u64::MAX, u64::MAX];
    let deleted: rustc_hash::FxHashSet<[u64; 3]> = snap.delta.del[pi]
        .range(lo..=hi)
        .map(|k| [k[1], k[2], k[3]])
        .collect();
    // the base rows that are still there, and the inserted ones of the index's dimension
    let mut rows: Vec<Row<'_>> = Vec::with_capacity(seg.rows());
    for i in 0..seg.rows() {
        if !deleted.contains(&seg.ids[i]) {
            rows.push(Row {
                s: seg.ids[i][0],
                v: std::borrow::Cow::Borrowed(seg.row(i)),
            });
        }
    }
    let mut based: Vec<usize> = Vec::new();
    if !node_rows.is_empty() {
        // the row index of each base row kept, for the graph's nodes
        based = vec![usize::MAX; seg.rows()];
        let mut k = 0;
        for (i, b) in based.iter_mut().enumerate() {
            if !deleted.contains(&seg.ids[i]) {
                *b = k;
                k += 1;
            }
        }
    }
    for k in snap.delta.ins[pi].range(lo..=hi) {
        if let Some(v) = snap.key(Id(k[2])).and_then(|key| from_key(&key))
            && v.len() == dim
        {
            rows.push(Row {
                s: k[1],
                v: std::borrow::Cow::Owned(v),
            });
        }
    }
    if dim == 0 || rows.is_empty() {
        return Err(Error::invalid(format!(
            "vector index {name} has no vectors to project"
        )));
    }
    let unit = metric == Metric::Cosine;
    let value = |r: &Row<'_>, out: &mut [f64]| {
        let n = if unit {
            let n =
                r.v.iter()
                    .map(|x| (*x as f64) * (*x as f64))
                    .sum::<f64>()
                    .sqrt();
            if n > 0.0 { n } else { 1.0 }
        } else {
            1.0
        };
        for (o, x) in out.iter_mut().zip(r.v.iter()) {
            *o = *x as f64 / n;
        }
    };
    check()?;
    // the sample: evenly spaced rows
    let n = rows.len();
    let m = cfg.sample.min(n);
    let mut sample = vec![0f64; m * dim];
    sample
        .par_chunks_mut(dim)
        .enumerate()
        .for_each(|(j, out)| value(&rows[(j as u128 * n as u128 / m as u128) as usize], out));
    let mut mean = vec![0f64; dim];
    for row in sample.chunks(dim) {
        for (a, x) in mean.iter_mut().zip(row) {
            *a += x;
        }
    }
    mean.iter_mut().for_each(|a| *a /= m as f64);
    sample.par_chunks_mut(dim).for_each(|row| {
        for (x, a) in row.iter_mut().zip(&mean) {
            *x -= a;
        }
    });
    let (components, explained) = principal(&sample, dim, cfg.seed, check)?;
    drop(sample);
    progress(0.3);
    check()?;
    // every row on the two components, then the mean per subject
    let proj: Vec<[f64; 2]> = rows
        .par_iter()
        .map(|r| {
            let mut buf = vec![0f64; dim];
            value(r, &mut buf);
            let mut p = [0f64; 2];
            for (k, c) in components.iter().enumerate() {
                p[k] = buf
                    .iter()
                    .zip(&mean)
                    .zip(c)
                    .map(|((x, a), w)| (x - a) * w)
                    .sum();
            }
            p
        })
        .collect();
    check()?;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| rows[i].s);
    let mut subjects: Vec<u64> = Vec::new();
    let mut sums: Vec<[f64; 3]> = Vec::new();
    let mut point_of_row = vec![0u32; n];
    for &i in &order {
        let s = rows[i].s;
        if subjects.last() != Some(&s) {
            subjects.push(s);
            sums.push([0.0; 3]);
        }
        let last = sums.last_mut().expect("pushed");
        last[0] += proj[i][0];
        last[1] += proj[i][1];
        last[2] += 1.0;
        point_of_row[i] = (subjects.len() - 1) as u32;
    }
    let mut xy: Vec<[f32; 2]> = sums
        .iter()
        .map(|s| [(s[0] / s[2]) as f32, (s[1] / s[2]) as f32])
        .collect();
    progress(0.4);
    if cfg.method == ProjectionMethod::Neighbours {
        let links = neighbours.ok_or_else(|| {
            Error::invalid(format!(
                "vector index {name} has no neighbour graph ready: the neighbour layout needs its HNSW graph"
            ))
        })?;
        // the entity edges of the graph's bottom layer
        let mut edges: Vec<(u32, u32)> = Vec::new();
        let mut out = Vec::new();
        for (x, &row) in node_rows.iter().enumerate() {
            let Some(&a) = based.get(row as usize).filter(|&&k| k != usize::MAX) else {
                continue;
            };
            out.clear();
            links(x as u32, &mut out);
            for &y in &out {
                let Some(&b) = node_rows
                    .get(y as usize)
                    .and_then(|&r| based.get(r as usize))
                    .filter(|&&k| k != usize::MAX)
                else {
                    continue;
                };
                let (pa, pb) = (point_of_row[a], point_of_row[b]);
                if pa != pb {
                    edges.push((pa.min(pb), pa.max(pb)));
                }
            }
        }
        edges.sort_unstable();
        edges.dedup();
        layout(&mut xy, &edges, cfg, check, progress)?;
    }
    progress(0.9);
    let classes_of = classes(snap, &subjects, check)?;
    let (classes, class_ids, class_counts) = classes_of;
    let priority: Vec<u32> = subjects
        .iter()
        .map(|&s| (mix(s ^ cfg.seed) >> 32) as u32)
        .collect();
    let bounds = bounds_of(&xy);
    Ok(Projection {
        index: name.to_string(),
        method: cfg.method,
        snapshot: snap.clone(),
        vectors: n as u64,
        dimension: dim,
        subjects,
        xy,
        classes,
        class_ids,
        class_counts,
        priority,
        bounds,
        explained,
        built_ms: started.elapsed().as_secs_f64() * 1e3,
    })
}

fn bounds_of(xy: &[[f32; 2]]) -> [f32; 4] {
    if xy.is_empty() {
        return [0.0, 0.0, 1.0, 1.0];
    }
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for p in xy {
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    }
    b
}

/// splitmix64
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The two leading principal directions of the centred `sample` (rows of `dim`), and the
/// share of the variance on each.
pub fn principal(
    sample: &[f64],
    dim: usize,
    seed: u64,
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<([Vec<f64>; 2], [f64; 2])> {
    let m = sample.len() / dim.max(1);
    let k = BLOCK.min(dim);
    let total: f64 = sample.iter().map(|x| x * x).sum::<f64>() / m.max(1) as f64;
    // the first directions: a seeded pseudo-random block
    let mut q: Vec<Vec<f64>> = (0..k)
        .map(|j| {
            (0..dim)
                .map(|d| {
                    let z = mix(seed ^ ((j as u64) << 32) ^ d as u64);
                    (z >> 11) as f64 / (1u64 << 53) as f64 - 0.5
                })
                .collect()
        })
        .collect();
    orthonormalize(&mut q);
    // Z = Xᵀ X Q, a pass over the sample per round
    let times = |q: &[Vec<f64>]| -> Vec<Vec<f64>> {
        sample
            .par_chunks(dim)
            .fold(
                || vec![vec![0f64; dim]; q.len()],
                |mut acc, row| {
                    for (a, c) in acc.iter_mut().zip(q) {
                        let y: f64 = row.iter().zip(c).map(|(x, w)| x * w).sum();
                        for (t, x) in a.iter_mut().zip(row) {
                            *t += y * x;
                        }
                    }
                    acc
                },
            )
            .reduce(
                || vec![vec![0f64; dim]; q.len()],
                |mut a, b| {
                    for (x, y) in a.iter_mut().zip(b) {
                        for (s, t) in x.iter_mut().zip(y) {
                            *s += t;
                        }
                    }
                    a
                },
            )
    };
    for _ in 0..ROUNDS {
        check()?;
        q = times(&q);
        orthonormalize(&mut q);
    }
    // B = Qᵀ (Xᵀ X / m) Q, then its eigenvectors
    let z = times(&q);
    let mut b = vec![vec![0f64; k]; k];
    for i in 0..k {
        for j in 0..k {
            b[i][j] = q[i].iter().zip(&z[j]).map(|(x, y)| x * y).sum::<f64>() / m.max(1) as f64;
        }
    }
    let (values, vectors) = jacobi(b);
    let mut idx: Vec<usize> = (0..k).collect();
    idx.sort_by(|&a, &c| values[c].total_cmp(&values[a]));
    let mut comps: [Vec<f64>; 2] = [vec![0f64; dim], vec![0f64; dim]];
    let mut explained = [0f64; 2];
    for (slot, &e) in idx.iter().take(2).enumerate() {
        for (d, x) in comps[slot].iter_mut().enumerate() {
            *x = (0..k).map(|i| vectors[i][e] * q[i][d]).sum();
        }
        // a fixed sign: the largest coordinate is positive
        let big = comps[slot]
            .iter()
            .copied()
            .max_by(|a, c| a.abs().total_cmp(&c.abs()))
            .unwrap_or(0.0);
        if big < 0.0 {
            comps[slot].iter_mut().for_each(|x| *x = -*x);
        }
        explained[slot] = if total > 0.0 {
            values[e].max(0.0) / total
        } else {
            0.0
        };
    }
    Ok((comps, explained))
}

/// Gram-Schmidt, twice for accuracy; a direction that vanishes becomes zero.
fn orthonormalize(q: &mut [Vec<f64>]) {
    for _ in 0..2 {
        for i in 0..q.len() {
            for j in 0..i {
                let (a, b) = q.split_at_mut(i);
                let d: f64 = b[0].iter().zip(&a[j]).map(|(x, y)| x * y).sum();
                for (x, y) in b[0].iter_mut().zip(&a[j]) {
                    *x -= d * y;
                }
            }
            let n = q[i].iter().map(|x| x * x).sum::<f64>().sqrt();
            if n > 1e-300 {
                q[i].iter_mut().for_each(|x| *x /= n);
            } else {
                q[i].iter_mut().for_each(|x| *x = 0.0);
            }
        }
    }
}

/// The eigenvalues and eigenvectors (columns) of the symmetric matrix `a`, by cyclic
/// Jacobi rotations.
#[allow(clippy::needless_range_loop)]
pub fn jacobi(mut a: Vec<Vec<f64>>) -> (Vec<f64>, Vec<Vec<f64>>) {
    let n = a.len();
    let mut v = vec![vec![0f64; n]; n];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for _ in 0..100 {
        let off: f64 = (0..n)
            .flat_map(|i| (0..n).filter(move |&j| j != i).map(move |j| (i, j)))
            .map(|(i, j)| a[i][j] * a[i][j])
            .sum();
        if off < 1e-24 {
            break;
        }
        for p in 0..n {
            for r in p + 1..n {
                if a[p][r].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[r][r] - a[p][p]) / (2.0 * a[p][r]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (akp, akr) = (a[k][p], a[k][r]);
                    a[k][p] = c * akp - s * akr;
                    a[k][r] = s * akp + c * akr;
                }
                for k in 0..n {
                    let (apk, ark) = (a[p][k], a[r][k]);
                    a[p][k] = c * apk - s * ark;
                    a[r][k] = s * apk + c * ark;
                }
                for row in v.iter_mut() {
                    let (vp, vr) = (row[p], row[r]);
                    row[p] = c * vp - s * vr;
                    row[r] = s * vp + c * vr;
                }
            }
        }
    }
    ((0..n).map(|i| a[i][i]).collect(), v)
}

/// A force layout of `edges` from the positions `xy`: each step moves an entity toward
/// the mean of its neighbours and away from the mass of its grid cell's neighbourhood,
/// with a step size that cools linearly.
fn layout(
    xy: &mut [[f32; 2]],
    edges: &[(u32, u32)],
    cfg: &ProjectionConfig,
    check: &(dyn Fn() -> Result<()> + Sync),
    progress: &(dyn Fn(f64) + Sync),
) -> Result<()> {
    let n = xy.len();
    if n < 2 {
        return Ok(());
    }
    // the positions in a unit square, kept there
    let b = bounds_of(xy);
    let span = (b[2] - b[0]).max(b[3] - b[1]).max(f32::MIN_POSITIVE);
    let mut pos: Vec<[f32; 2]> = xy
        .iter()
        .map(|p| [(p[0] - b[0]) / span, (p[1] - b[1]) / span])
        .collect();
    // adjacency in compressed rows
    let mut deg = vec![0u32; n + 1];
    for &(a, c) in edges {
        deg[a as usize + 1] += 1;
        deg[c as usize + 1] += 1;
    }
    for i in 0..n {
        deg[i + 1] += deg[i];
    }
    let mut adj = vec![0u32; deg[n] as usize];
    let mut fill = deg.clone();
    for &(a, c) in edges {
        adj[fill[a as usize] as usize] = c;
        fill[a as usize] += 1;
        adj[fill[c as usize] as usize] = a;
        fill[c as usize] += 1;
    }
    let g = ((n as f64).sqrt() / 2.0).clamp(4.0, 512.0) as usize;
    let iterations = cfg.iterations.clamp(1, 1000);
    for it in 0..iterations {
        check()?;
        let cool = 1.0 - it as f32 / iterations as f32;
        // the mass and centre of each cell
        let mut mass = vec![0f32; g * g];
        let mut centre = vec![[0f32; 2]; g * g];
        let cell = |p: &[f32; 2]| {
            let cx = ((p[0].clamp(0.0, 1.0)) * g as f32) as usize;
            let cy = ((p[1].clamp(0.0, 1.0)) * g as f32) as usize;
            (cx.min(g - 1), cy.min(g - 1))
        };
        for p in &pos {
            let (cx, cy) = cell(p);
            mass[cy * g + cx] += 1.0;
            centre[cy * g + cx][0] += p[0];
            centre[cy * g + cx][1] += p[1];
        }
        for (c, m) in centre.iter_mut().zip(&mass) {
            if *m > 0.0 {
                c[0] /= m;
                c[1] /= m;
            }
        }
        let crowd = n as f32 / (g * g) as f32;
        let next: Vec<[f32; 2]> = (0..n)
            .into_par_iter()
            .map(|i| {
                let p = pos[i];
                let mut f = [0f32; 2];
                // attraction toward the neighbours' mean
                let (lo, hi) = (deg[i] as usize, deg[i + 1] as usize);
                if hi > lo {
                    let mut m = [0f32; 2];
                    for &j in &adj[lo..hi] {
                        m[0] += pos[j as usize][0];
                        m[1] += pos[j as usize][1];
                    }
                    let k = (hi - lo) as f32;
                    f[0] += (m[0] / k - p[0]) * 0.5;
                    f[1] += (m[1] / k - p[1]) * 0.5;
                }
                // repulsion from the crowded cells around
                let (cx, cy) = cell(&p);
                for dy in -1i64..=1 {
                    for dx in -1i64..=1 {
                        let (x, y) = (cx as i64 + dx, cy as i64 + dy);
                        if x < 0 || y < 0 || x >= g as i64 || y >= g as i64 {
                            continue;
                        }
                        let c = y as usize * g + x as usize;
                        if mass[c] <= crowd {
                            continue;
                        }
                        let (ddx, ddy) = (p[0] - centre[c][0], p[1] - centre[c][1]);
                        let d2 = ddx * ddx + ddy * ddy + 1e-6;
                        let push = (mass[c] - crowd) / (n as f32) * 0.05 / d2.sqrt();
                        f[0] += ddx / d2.sqrt() * push;
                        f[1] += ddy / d2.sqrt() * push;
                    }
                }
                let step = 0.1 * cool;
                let len = (f[0] * f[0] + f[1] * f[1]).sqrt();
                let scale = if len > step { step / len } else { 1.0 };
                [
                    (p[0] + f[0] * scale).clamp(0.0, 1.0),
                    (p[1] + f[1] * scale).clamp(0.0, 1.0),
                ]
            })
            .collect();
        pos = next;
        progress(0.4 + 0.5 * (it + 1) as f64 / iterations as f64);
    }
    for (o, p) in xy.iter_mut().zip(pos) {
        *o = p;
    }
    Ok(())
}

/// The class of each subject (its first `rdf:type` in key order), the classes by index
/// and their counts.
fn classes(
    snap: &Snapshot,
    subjects: &[u64],
    check: &(dyn Fn() -> Result<()> + Sync),
) -> Result<(Vec<u32>, Vec<u64>, Vec<u64>)> {
    let mut of = vec![NO_CLASS; subjects.len()];
    let mut ids: Vec<u64> = Vec::new();
    let mut counts: Vec<u64> = Vec::new();
    let mut index: FxHashMap<u64, u32> = FxHashMap::default();
    let Some(ty) = snap.lookup_iri(RDF_TYPE) else {
        return Ok((of, ids, counts));
    };
    let mut seen = 0u64;
    let mut visit = |k: &[u64; 4]| {
        let (s, o) = (k[1], k[2]);
        if let Ok(i) = subjects.binary_search(&s)
            && of[i] == NO_CLASS
        {
            let c = *index.entry(o).or_insert_with(|| {
                ids.push(o);
                counts.push(0);
                (ids.len() - 1) as u32
            });
            of[i] = c;
            counts[c as usize] += 1;
        }
    };
    snap.scan(Perm::Pso, &[ty.0], |c| {
        match c {
            Chunk::Block(b, lo, hi) => (lo..hi).for_each(|i| visit(&b.key(i))),
            Chunk::Row(k) => visit(&k),
        }
        seen += 1;
        if seen.is_multiple_of(64) {
            check()?;
        }
        Ok(true)
    })?;
    Ok((of, ids, counts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Result<()> {
        Ok(())
    }

    #[test]
    fn subspace_iteration_matches_the_exact_eigenvectors() {
        // 2,000 points in 6 dimensions with known variances along mixed axes
        let dim = 6;
        let mut sample = Vec::new();
        for i in 0..2000u64 {
            let r = |k: u64| (mix(i * 31 + k) >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
            let (a, b, c) = (r(1) * 10.0, r(2) * 4.0, r(3) * 0.5);
            let v = [a + b, a - b, c, b * 0.5, a * 0.2 + c, c - b * 0.1];
            sample.extend_from_slice(&v);
        }
        let m = 2000.0;
        let mut mean = vec![0f64; dim];
        for row in sample.chunks(dim) {
            for (a, x) in mean.iter_mut().zip(row) {
                *a += x / m;
            }
        }
        for row in sample.chunks_mut(dim) {
            for (x, a) in row.iter_mut().zip(&mean) {
                *x -= a;
            }
        }
        let (comps, explained) = principal(&sample, dim, 7, &none).unwrap();
        // the reference: the exact covariance's eigenvectors
        let mut cov = vec![vec![0f64; dim]; dim];
        for row in sample.chunks(dim) {
            for i in 0..dim {
                for j in 0..dim {
                    cov[i][j] += row[i] * row[j] / m;
                }
            }
        }
        let total: f64 = (0..dim).map(|i| cov[i][i]).sum();
        let (values, vectors) = jacobi(cov);
        let mut idx: Vec<usize> = (0..dim).collect();
        idx.sort_by(|&a, &b| values[b].total_cmp(&values[a]));
        for (slot, &e) in idx.iter().take(2).enumerate() {
            let dot: f64 = (0..dim).map(|d| vectors[d][e] * comps[slot][d]).sum();
            assert!(
                dot.abs() > 0.9999,
                "component {slot}: |cos| = {}",
                dot.abs()
            );
            assert!((explained[slot] - values[e] / total).abs() < 1e-6);
        }
    }

    #[test]
    fn jacobi_diagonalizes_a_small_matrix() {
        let (values, v) = jacobi(vec![vec![2.0, 1.0], vec![1.0, 2.0]]);
        let mut sorted = values.clone();
        sorted.sort_by(f64::total_cmp);
        assert!((sorted[0] - 1.0).abs() < 1e-12 && (sorted[1] - 3.0).abs() < 1e-12);
        let dot = v[0][0] * v[0][1] + v[1][0] * v[1][1];
        assert!(dot.abs() < 1e-12);
    }
}
