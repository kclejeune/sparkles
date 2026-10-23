//! The community hierarchy of a build (spec C22 §3.1): F14's Leiden adapted into nested
//! depths from the coarsest.
//!
//! `community::run` reports its levels from the finest. The adapter runs it to natural
//! convergence under a finite level cap, reverses the levels and keeps the coarsest
//! `levels + 1` of them, so `levels` is the deepest zero-based depth kept. A run that
//! reaches the cap is refused as incomplete rather than presented as the coarsest
//! hierarchy, and a run stopped by its deadline or memory limit fails as a whole, since
//! `community::run` returns no partial output.
//!
//! Leiden's levels need not nest: aggregation by the refined partition can move a part
//! of one community into another at the next level, and each level is split into
//! connected pieces. The reports form a tree (`skos:broader`), so each depth below the
//! coarsest is intersected with its parent depth and split again into connected pieces
//! of the projection. Every community is then a connected subset of its parent.
//!
//! A community smaller than `minCommunitySize` is folded into its nearest kept ancestor.
//! Since a child is a subset of its parent, the ancestor already holds its members, and
//! the folded members are listed on the ancestor so that its report input can include
//! them. Members of a small community at depth 0 have no community.

use sparkles::algo::{Control, Csr, Graph, community};
use std::collections::VecDeque;

/// The most native levels a run may produce before it counts as incomplete.
pub const NATIVE_LEVEL_CAP: u32 = 32;
/// The fixed seed of every build, so that a build is reproducible.
pub const LEIDEN_SEED: u64 = 0x6772_6170_6872_6167;

/// One kept community.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Community {
    /// node indexes of the projection, ascending
    pub members: Vec<u32>,
    /// the index of the parent in the depth above, `None` at depth 0
    pub parent: Option<usize>,
    /// members that no kept child holds, because their child was folded into this
    /// community; empty at the deepest depth kept
    pub folded: Vec<u32>,
}

/// The kept depths, coarsest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hierarchy {
    pub depths: Vec<Vec<Community>>,
    /// nodes whose depth-0 community was too small to keep
    pub unassigned: Vec<u32>,
    /// the levels Leiden produced before the adaptation
    pub native_levels: usize,
}

#[derive(Debug)]
pub enum HierarchyError {
    /// the run reached [`NATIVE_LEVEL_CAP`] without converging
    Incomplete { levels: usize },
    /// the algorithm failed, including by its deadline or memory limit
    Algo(String),
    /// the native levels do not describe the projection's nodes
    Malformed(String),
}

impl std::fmt::Display for HierarchyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HierarchyError::Incomplete { levels } => write!(
                f,
                "the community hierarchy did not converge within {levels} levels, so its coarsest level is not known"
            ),
            HierarchyError::Algo(m) => write!(f, "community detection failed: {m}"),
            HierarchyError::Malformed(m) => write!(f, "malformed community levels: {m}"),
        }
    }
}

impl std::error::Error for HierarchyError {}

/// Run Leiden on the projection and adapt its levels.
pub fn compute(
    g: &Graph,
    levels: u32,
    resolution: f64,
    min_size: u32,
    ctl: &Control,
) -> Result<Hierarchy, HierarchyError> {
    let p = community::Params {
        resolution,
        max_levels: NATIVE_LEVEL_CAP,
        leiden: true,
        seed: LEIDEN_SEED,
        ..community::Params::default()
    };
    let out = community::run(g, &p, ctl).map_err(|e| HierarchyError::Algo(e.to_string()))?;
    if out.levels.len() >= NATIVE_LEVEL_CAP as usize {
        return Err(HierarchyError::Incomplete {
            levels: out.levels.len(),
        });
    }
    let adj = g
        .simple(ctl)
        .map_err(|e| HierarchyError::Algo(e.to_string()))?;
    adapt(
        &out.levels,
        &adj,
        levels as usize + 1,
        min_size as usize,
        ctl,
    )
}

/// Estimated bytes per node and kept depth of the adaptation's workspace: the labels,
/// the groups with their vector headers, the kept indexes, and the kept communities'
/// member lists with their headers.
const BYTES_PER_NODE_AND_DEPTH: u64 = 128;
/// Steps between checks of the control.
const CHECK_EVERY: usize = 4096;

/// Memory charged to a control, given back when dropped.
struct Charged<'a> {
    ctl: &'a Control,
    bytes: u64,
}

impl Drop for Charged<'_> {
    fn drop(&mut self) {
        self.ctl.release(self.bytes);
    }
}

/// Counts steps and checks the control every [`CHECK_EVERY`] of them.
struct Ticker<'a> {
    ctl: &'a Control,
    steps: usize,
}

impl Ticker<'_> {
    fn tick(&mut self, n: usize) -> Result<(), HierarchyError> {
        self.steps += n;
        if self.steps >= CHECK_EVERY {
            self.steps = 0;
            self.ctl
                .check()
                .map_err(|e| HierarchyError::Algo(e.to_string()))?;
        }
        Ok(())
    }
}

/// Adapt native levels, finest first, over the undirected simple rows `adj`, keeping at
/// most `keep` depths. The workspace is charged to `ctl` before it is allocated, and
/// the loops check `ctl`, so a cancellation, a deadline or the memory limit fails the
/// adaptation as a whole.
pub fn adapt(
    native: &[Vec<u32>],
    adj: &Csr,
    keep: usize,
    min_size: usize,
    ctl: &Control,
) -> Result<Hierarchy, HierarchyError> {
    let algo = |e: sparkles::error::Error| HierarchyError::Algo(e.to_string());
    let n = adj.n();
    if let Some(l) = native.iter().find(|l| l.len() != n) {
        return Err(HierarchyError::Malformed(format!(
            "a level has {} entries for {n} nodes",
            l.len()
        )));
    }
    let mut h = Hierarchy {
        native_levels: native.len(),
        ..Hierarchy::default()
    };
    ctl.check().map_err(algo)?;
    if n == 0 || native.is_empty() || keep == 0 {
        return Ok(h);
    }
    let depths = keep.min(native.len()) as u64;
    let bytes = (n as u64)
        .saturating_mul(depths)
        .saturating_mul(BYTES_PER_NODE_AND_DEPTH)
        .saturating_add(4 * n as u64);
    ctl.charge(bytes).map_err(algo)?;
    let _charged = Charged { ctl, bytes };
    let mut tick = Ticker { ctl, steps: 0 };
    // labels per depth, coarsest first, each nested in the one above
    let mut labels: Vec<Vec<u32>> = Vec::new();
    for lv in native.iter().rev().take(keep) {
        let parent = labels.last();
        let l = pieces(
            adj,
            |u, v| lv[u] == lv[v] && parent.is_none_or(|p| p[u] == p[v]),
            &mut tick,
        )?;
        labels.push(l);
    }
    // communities per depth, ordered by their smallest member
    let mut groups: Vec<Vec<Vec<u32>>> = Vec::with_capacity(labels.len());
    for l in &labels {
        let count = l.iter().map(|&x| x as usize + 1).max().unwrap_or(0);
        let mut g: Vec<Vec<u32>> = vec![Vec::new(); count];
        for (u, &c) in l.iter().enumerate() {
            tick.tick(1)?;
            g[c as usize].push(u as u32);
        }
        groups.push(g);
    }
    // kept index of each native group, per depth
    let mut kept_index: Vec<Vec<Option<usize>>> = Vec::with_capacity(groups.len());
    for (d, gs) in groups.iter().enumerate() {
        let mut idx = vec![None; gs.len()];
        let mut out = Vec::new();
        for (c, members) in gs.iter().enumerate() {
            tick.tick(members.len() + 1)?;
            if members.len() < min_size.max(1) {
                continue;
            }
            let parent = if d == 0 {
                None
            } else {
                let pl = labels[d - 1][members[0] as usize] as usize;
                // a kept child's parent is a superset, so it is kept as well
                Some(kept_index[d - 1][pl].ok_or_else(|| {
                    HierarchyError::Malformed("a kept community has a folded parent".into())
                })?)
            };
            idx[c] = Some(out.len());
            out.push(Community {
                members: members.clone(),
                parent,
                folded: Vec::new(),
            });
        }
        kept_index.push(idx);
        h.depths.push(out);
    }
    // members of folded children, listed on their parent
    for d in 0..h.depths.len().saturating_sub(1) {
        for u in 0..n {
            tick.tick(1)?;
            let child = labels[d + 1][u] as usize;
            if kept_index[d + 1][child].is_none()
                && let Some(p) = kept_index[d][labels[d][u] as usize]
            {
                h.depths[d][p].folded.push(u as u32);
            }
        }
    }
    h.unassigned = (0..n as u32)
        .filter(|&u| kept_index[0][labels[0][u as usize] as usize].is_none())
        .collect();
    Ok(h)
}

/// Connected pieces of the graph under the edges `same` accepts, numbered by their
/// smallest node.
fn pieces(
    adj: &Csr,
    same: impl Fn(usize, usize) -> bool,
    tick: &mut Ticker<'_>,
) -> Result<Vec<u32>, HierarchyError> {
    let n = adj.n();
    let mut out = vec![u32::MAX; n];
    let mut next = 0u32;
    let mut queue = VecDeque::new();
    for s in 0..n {
        if out[s] != u32::MAX {
            continue;
        }
        out[s] = next;
        queue.push_back(s as u32);
        while let Some(u) = queue.pop_front() {
            tick.tick(adj.deg(u) + 1)?;
            for &v in adj.row(u) {
                if out[v as usize] == u32::MAX && same(u as usize, v as usize) {
                    out[v as usize] = next;
                    queue.push_back(v);
                }
            }
        }
        next += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::algo::{Aggregate, Par, RawEdges};

    fn ugraph(n: u32, edges: &[(u32, u32)]) -> Graph {
        let mut src = Vec::new();
        let mut dst = Vec::new();
        for &(a, b) in edges {
            src.extend([a as u64, b as u64]);
            dst.extend([b as u64, a as u64]);
        }
        let raw = RawEdges {
            nodes: (0..n as u64).collect(),
            src,
            dst,
            w: None,
            symmetric: true,
        };
        Graph::build(raw, Aggregate::None, &Par::single(), &Control::unlimited()).unwrap()
    }

    fn csr(n: u32, edges: &[(u32, u32)]) -> Csr {
        ugraph(n, edges).simple(&Control::unlimited()).unwrap()
    }

    fn clique(e: &mut Vec<(u32, u32)>, from: u32, size: u32) {
        for a in from..from + size {
            for b in a + 1..from + size {
                e.push((a, b));
            }
        }
    }

    /// Two dense clusters joined by one edge make two communities (C22 A1).
    #[test]
    fn two_clusters_make_two_communities() {
        let mut e = Vec::new();
        clique(&mut e, 0, 6);
        clique(&mut e, 6, 6);
        e.push((5, 6));
        let g = ugraph(12, &e);
        let h = compute(&g, 3, 1.0, 3, &Control::unlimited()).unwrap();
        let finest = h.depths.last().unwrap();
        assert_eq!(finest.len(), 2, "{h:?}");
        assert_eq!(finest[0].members, (0..6).collect::<Vec<_>>());
        assert_eq!(finest[1].members, (6..12).collect::<Vec<_>>());
        assert!(h.unassigned.is_empty());
        // the same input gives the same hierarchy
        assert_eq!(h, compute(&g, 3, 1.0, 3, &Control::unlimited()).unwrap());
    }

    #[test]
    fn an_empty_projection_has_no_depths() {
        let g = ugraph(0, &[]);
        let h = compute(&g, 3, 1.0, 3, &Control::unlimited()).unwrap();
        assert!(h.depths.is_empty());
        assert!(h.unassigned.is_empty());
    }

    #[test]
    fn levels_keep_the_coarsest_depths() {
        // three native levels, finest first: singletons, pairs, one group
        let adj = csr(4, &[(0, 1), (1, 2), (2, 3)]);
        let native = vec![vec![0, 1, 2, 3], vec![0, 0, 1, 1], vec![0, 0, 0, 0]];
        let h = adapt(&native, &adj, 2, 1, &Control::unlimited()).unwrap();
        assert_eq!(h.depths.len(), 2);
        assert_eq!(h.depths[0].len(), 1);
        assert_eq!(h.depths[1].len(), 2);
        assert_eq!(h.depths[1][1].parent, Some(0));
        // early convergence keeps the levels there are
        let h = adapt(&native[2..], &adj, 4, 1, &Control::unlimited()).unwrap();
        assert_eq!(h.depths.len(), 1);
        assert_eq!(h.native_levels, 1);
    }

    #[test]
    fn finer_depths_nest_in_their_parents() {
        // the finer native level crosses the coarse boundary between 1 and 2
        let adj = csr(4, &[(0, 1), (1, 2), (2, 3)]);
        let native = vec![vec![0, 1, 1, 2], vec![0, 0, 1, 1]];
        let h = adapt(&native, &adj, 2, 1, &Control::unlimited()).unwrap();
        let fine: Vec<Vec<u32>> = h.depths[1].iter().map(|c| c.members.clone()).collect();
        assert_eq!(fine, vec![vec![0], vec![1], vec![2], vec![3]]);
        for c in &h.depths[1] {
            let p = &h.depths[0][c.parent.unwrap()];
            assert!(c.members.iter().all(|m| p.members.contains(m)));
        }
    }

    #[test]
    fn disconnected_intersections_are_split() {
        // native fine community {0, 2} is not connected inside its parent {0, 1, 2}
        let adj = csr(3, &[(0, 1), (1, 2)]);
        let native = vec![vec![0, 1, 0], vec![0, 0, 0]];
        let h = adapt(&native, &adj, 2, 1, &Control::unlimited()).unwrap();
        assert_eq!(h.depths[1].len(), 3);
    }

    #[test]
    fn small_communities_fold_into_their_parent() {
        let adj = csr(7, &[(0, 1), (1, 2), (2, 3), (3, 4), (5, 6)]);
        // coarse: {0..4}, {5, 6}; fine: {0, 1, 2}, {3, 4}, {5, 6}
        let native = vec![vec![0, 0, 0, 1, 1, 2, 2], vec![0, 0, 0, 0, 0, 1, 1]];
        let h = adapt(&native, &adj, 2, 3, &Control::unlimited()).unwrap();
        assert_eq!(h.depths[0].len(), 1);
        assert_eq!(h.depths[0][0].members, vec![0, 1, 2, 3, 4]);
        assert_eq!(h.depths[0][0].folded, vec![3, 4]);
        assert_eq!(h.depths[1].len(), 1);
        assert_eq!(h.depths[1][0].members, vec![0, 1, 2]);
        assert!(h.depths[1][0].folded.is_empty());
        assert_eq!(h.unassigned, vec![5, 6]);
    }

    #[test]
    fn malformed_levels_are_refused() {
        let adj = csr(3, &[(0, 1)]);
        assert!(matches!(
            adapt(&[vec![0, 0]], &adj, 2, 1, &Control::unlimited()),
            Err(HierarchyError::Malformed(_))
        ));
    }

    /// The adaptation after Leiden obeys the control too (review finding 3).
    #[test]
    fn the_adaptation_obeys_its_control() {
        let adj = csr(4, &[(0, 1), (1, 2), (2, 3)]);
        let native = vec![vec![0, 0, 1, 1], vec![0, 0, 0, 0]];
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctl = Control::new(cancel, None, u64::MAX);
        assert!(matches!(
            adapt(&native, &adj, 2, 1, &ctl),
            Err(HierarchyError::Algo(_))
        ));
        // a memory limit below the workspace refuses before allocating
        let tiny = Control::new(Default::default(), None, 64);
        assert!(matches!(
            adapt(&native, &adj, 2, 1, &tiny),
            Err(HierarchyError::Algo(_))
        ));
        let ctl = Control::unlimited();
        adapt(&native, &adj, 2, 1, &ctl).unwrap();
        assert_eq!(
            ctl.remaining(),
            Control::unlimited().remaining(),
            "released"
        );
    }

    #[test]
    fn a_cancelled_run_fails_as_a_whole() {
        let mut e = Vec::new();
        clique(&mut e, 0, 8);
        let g = ugraph(8, &e);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ctl = Control::new(cancel, None, u64::MAX);
        assert!(matches!(
            compute(&g, 3, 1.0, 1, &ctl),
            Err(HierarchyError::Algo(_))
        ));
    }
}
