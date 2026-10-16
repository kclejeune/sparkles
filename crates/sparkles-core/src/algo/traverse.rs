//! Breadth-first depths and single-source shortest paths (spec F14 §4).

use super::memory::{Buffer, Growing, Heap, Work};
use super::{BLOCK, CHECK_EVERY, Control, Graph, Par, bad};
use crate::error::Result;
use std::cmp::Reverse;
use std::sync::atomic::{AtomicU32, Ordering};

pub const UNREACHED: u32 = u32::MAX;

/// The depth of each node from the nearest source (`UNREACHED` when no source reaches it).
/// Levels are expanded in parallel, and the depths do not depend on the order within one.
pub fn bfs(g: &Graph, sources: &[u32], par: &Par, ctl: &Control) -> Result<Vec<u32>> {
    let n = g.n();
    let depth = Buffer::collect(
        "bfs-depth",
        n,
        (0..n).map(|_| AtomicU32::new(UNREACHED)),
        ctl,
    )?;
    let mut frontier = Buffer::new("bfs-frontier", sources.len(), ctl)?;
    let mut work = Work::new(ctl)?;
    for &s in sources {
        work.tick("bfs-sources")?;
        if depth[s as usize]
            .compare_exchange(UNREACHED, 0, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            frontier.push(s)?;
        }
    }
    let mut level = 0u32;
    while !frontier.is_empty() {
        ctl.check()?;
        level += 1;
        let parts = par.blocks(frontier.len(), BLOCK / 8, |lo, hi| {
            let mut work = Work::new(ctl)?;
            let mut next = Growing::new("bfs-next", ctl)?;
            for &u in &frontier[lo..hi] {
                let row = g.out.row(u);
                work.tick_n(row.len() + 1, "bfs-frontier")?;
                // room for every neighbour, charged before the row is read
                next.reserve(row.len())?;
                for &v in row {
                    if depth[v as usize]
                        .compare_exchange(UNREACHED, level, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                    {
                        next.push_reserved(v);
                    }
                }
            }
            Ok(next)
        })?;
        let total = parts.iter().map(|p| p.len()).sum();
        // the old frontier goes before the new one is charged
        drop(frontier);
        frontier = Buffer::new("bfs-frontier", total, ctl)?;
        for part in parts {
            ctl.check()?;
            frontier.extend_within(part.iter().copied())?;
        }
    }
    drop(frontier);
    let out = Buffer::collect(
        "bfs-output",
        n,
        depth.iter().map(|d| d.load(Ordering::Relaxed)),
        ctl,
    )?;
    Ok(out.into_output())
}

/// The distance of each node from the nearest source: Dijkstra's algorithm over the edge
/// weights, or breadth-first levels when the graph has none. Unreached nodes get
/// infinity. A negative weight fails the call. The heap holds one entry per improved
/// distance, up to one per edge, and its capacity is charged to the call as it grows.
pub fn sssp(g: &Graph, sources: &[u32], par: &Par, ctl: &Control) -> Result<Vec<f64>> {
    let n = g.n();
    let Some(w) = &g.out.w else {
        let depth = bfs(g, sources, par, ctl)?;
        let mut work = Work::new(ctl)?;
        let mut out = Buffer::new("sssp-output", n, ctl)?;
        for d in depth {
            work.tick("sssp-output")?;
            out.push(if d == UNREACHED {
                f64::INFINITY
            } else {
                d as f64
            })?;
        }
        return Ok(out.into_output());
    };
    for chunk in w.chunks(CHECK_EVERY) {
        ctl.check()?;
        if chunk.iter().any(|&x| x < 0.0 || x.is_nan()) {
            return Err(bad("sssp", "a negative edge weight"));
        }
    }
    let mut dist = Buffer::splat("sssp-distance", n, f64::INFINITY, ctl)?;
    let mut heap: Heap<Reverse<(OrdF64, u32)>> = Heap::new(ctl)?;
    for &s in sources {
        dist[s as usize] = 0.0;
        heap.push(Reverse((OrdF64(0.0), s)))?;
    }
    let mut work = Work::new(ctl)?;
    while let Some(Reverse((OrdF64(d), u))) = heap.pop() {
        if d > dist[u as usize] {
            work.tick("sssp-stale")?;
            continue;
        }
        let (row, ws) = (g.out.row(u), g.out.wrow(u).unwrap_or_default());
        work.tick_n(row.len() + 1, "sssp-relax")?;
        for (i, &v) in row.iter().enumerate() {
            let nd = d + ws[i] as f64;
            if nd < dist[v as usize] {
                dist[v as usize] = nd;
                heap.push(Reverse((OrdF64(nd), v)))?;
            }
        }
    }
    drop(heap);
    Ok(dist.into_output())
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct OrdF64(pub f64);
impl Eq for OrdF64 {}
impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for OrdF64 {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::super::{Aggregate, RawEdges};
    use super::*;

    #[test]
    fn bfs_depths() {
        let g = digraph(8, &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 3), (5, 6)]);
        for par in [Par::single(), Par::with_threads(3)] {
            let d = bfs(&g, &[0], &par, &Control::unlimited()).unwrap();
            assert_eq!(&d[..5], &[0, 1, 2, 3, 4]);
            assert_eq!(d[5], UNREACHED);
        }
    }

    #[test]
    fn sssp_uses_weights() {
        let raw = RawEdges {
            nodes: (0..4).collect(),
            src: vec![0, 0, 1, 2],
            dst: vec![1, 2, 3, 3],
            w: Some(vec![1.0, 5.0, 10.0, 1.0]),
            symmetric: false,
        };
        let g = Graph::build(raw, Aggregate::None, &Par::single(), &Control::unlimited()).unwrap();
        let d = sssp(&g, &[0], &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(d, vec![0.0, 1.0, 5.0, 6.0]);
    }

    #[test]
    fn sssp_without_weights_is_bfs() {
        let g = digraph(3, &[(0, 1), (1, 2)]);
        let d = sssp(&g, &[0], &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(d, vec![0.0, 1.0, 2.0]);
    }
}
