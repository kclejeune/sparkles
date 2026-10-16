//! Weakly and strongly connected components (spec F14 §4).
//!
//! Components are numbered from 0 in the order of their smallest node, so the numbers
//! are stable for a commit and a projection.

use super::memory::{Buffer, Growing, Work};
use super::{BLOCK, Control, Graph, Par};
use crate::error::Result;
use std::sync::atomic::{AtomicU32, Ordering};

pub struct Components {
    /// the component of each node
    pub comp: Vec<u32>,
    pub count: u32,
}

/// Number the components by the smallest node: `root(v)` is a representative of `v`'s
/// component, and any representative works.
fn number(n: usize, mut root: impl FnMut(usize) -> u32, ctl: &Control) -> Result<Components> {
    let mut first = Buffer::splat("components-first", n, u32::MAX, ctl)?;
    let mut comp = Buffer::zeroed("components-output", n, ctl)?;
    let mut work = Work::new(ctl)?;
    let mut count = 0u32;
    for v in 0..n {
        work.tick("components-number")?;
        let r = root(v) as usize;
        if first[r] == u32::MAX {
            first[r] = count;
            count += 1;
        }
        comp[v] = first[r];
    }
    ctl.check()?;
    Ok(Components {
        comp: comp.into_output(),
        count,
    })
}

/// Weakly connected components: union-find with path halving, parallel over nodes. The
/// larger root always links under the smaller one, so a component's root is its smallest
/// node whatever the order of the unions.
pub fn wcc(g: &Graph, par: &Par, ctl: &Control) -> Result<Components> {
    let n = g.n();
    let parent = Buffer::collect("wcc-parent", n, (0..n as u32).map(AtomicU32::new), ctl)?;
    let find = |mut x: u32| -> u32 {
        loop {
            let p = parent[x as usize].load(Ordering::Relaxed);
            if p == x {
                return x;
            }
            let gp = parent[p as usize].load(Ordering::Relaxed);
            // path halving
            let _ = parent[x as usize].compare_exchange_weak(
                p,
                gp,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            x = gp;
        }
    };
    par.blocks(n, BLOCK, |lo, hi| {
        let mut work = Work::new(ctl)?;
        for u in lo..hi {
            let row = g.out.row(u as u32);
            work.tick_n(row.len() + 1, "wcc-union")?;
            for &v in row {
                let (mut a, mut b) = (find(u as u32), find(v));
                while a != b {
                    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
                    match parent[hi as usize].compare_exchange(
                        hi,
                        lo,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(_) => {
                            a = find(a);
                            b = find(b);
                        }
                    }
                }
            }
        }
        Ok(())
    })?;
    number(n, |v| find(v as u32), ctl)
}

/// Strongly connected components: Tarjan's algorithm with an explicit stack.
pub fn scc(g: &Graph, ctl: &Control) -> Result<Components> {
    let n = g.n();
    const UNSEEN: u32 = u32::MAX;
    let mut index = Buffer::splat("scc-index", n, UNSEEN, ctl)?;
    let mut low = Buffer::<u32>::zeroed("scc-low", n, ctl)?;
    let mut on_stack = Buffer::splat("scc-on-stack", n, false, ctl)?;
    let mut root = Buffer::splat("scc-root", n, UNSEEN, ctl)?;
    let mut stack: Growing<u32> = Growing::new("scc-stack", ctl)?;
    let mut call: Growing<(u32, u32)> = Growing::new("scc-call", ctl)?;
    let mut work = Work::new(ctl)?;
    let mut next = 0u32;
    for s in 0..n as u32 {
        work.tick("scc-start")?;
        if index[s as usize] != UNSEEN {
            continue;
        }
        call.push((s, 0))?;
        index[s as usize] = next;
        low[s as usize] = next;
        next += 1;
        stack.push(s)?;
        on_stack[s as usize] = true;
        while let Some(&(u, i)) = call.last() {
            // one step reads one edge or finishes one node
            work.tick("scc-dfs")?;
            let row = g.out.row(u);
            if (i as usize) < row.len() {
                let v = row[i as usize];
                if let Some(top) = call.last_mut() {
                    top.1 += 1;
                }
                if index[v as usize] == UNSEEN {
                    index[v as usize] = next;
                    low[v as usize] = next;
                    next += 1;
                    stack.push(v)?;
                    on_stack[v as usize] = true;
                    call.push((v, 0))?;
                } else if on_stack[v as usize] {
                    low[u as usize] = low[u as usize].min(index[v as usize]);
                }
            } else {
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    low[parent as usize] = low[parent as usize].min(low[u as usize]);
                }
                if low[u as usize] == index[u as usize] {
                    // u is the root of a component: pop it
                    while let Some(w) = stack.pop() {
                        on_stack[w as usize] = false;
                        root[w as usize] = u;
                        if w == u {
                            break;
                        }
                    }
                }
            }
        }
    }
    drop((index, low, on_stack, stack, call));
    number(n, |v| root[v], ctl)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    fn a1() -> Graph {
        digraph(8, &[(0, 1), (1, 2), (2, 0), (2, 3), (3, 4), (4, 3), (5, 6)])
    }

    #[test]
    fn wcc_finds_three_and_h() {
        let c = wcc(&a1(), &Par::single(), &Control::unlimited()).unwrap();
        assert_eq!(c.count, 3);
        assert_eq!(c.comp, vec![0, 0, 0, 0, 0, 1, 1, 2]);
    }

    #[test]
    fn scc_splits_the_chain() {
        let c = scc(&a1(), &Control::unlimited()).unwrap();
        // {a b c}, {d e}, {f}, {g}, {h}
        assert_eq!(c.count, 5);
        assert_eq!(c.comp, vec![0, 0, 0, 1, 1, 2, 3, 4]);
    }

    #[test]
    fn wcc_matches_repeated_search_on_random_graphs() {
        let mut x = 99u64;
        let mut rnd = |m: u32| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32 % m
        };
        for _ in 0..20 {
            let n = 60;
            let e: Vec<(u32, u32)> = (0..50).map(|_| (rnd(n), rnd(n))).collect();
            let g = digraph(n, &e);
            let one = wcc(&g, &Par::single(), &Control::unlimited()).unwrap();
            let four = wcc(&g, &Par::with_threads(4), &Control::unlimited()).unwrap();
            assert_eq!(one.comp, four.comp);
            // reference: repeated label sweeps
            let mut lab: Vec<u32> = (0..n).collect();
            loop {
                let mut changed = false;
                for &(a, b) in &e {
                    let m = lab[a as usize].min(lab[b as usize]);
                    if lab[a as usize] != m || lab[b as usize] != m {
                        lab[a as usize] = m;
                        lab[b as usize] = m;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            let reference = number(lab.len(), |v| lab[v], &Control::unlimited()).unwrap();
            assert_eq!(one.comp, reference.comp);
        }
    }

    #[test]
    fn scc_matches_reachability_on_random_graphs() {
        let mut x = 7u64;
        let mut rnd = |m: u32| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u32 % m
        };
        for _ in 0..20 {
            let n = 30u32;
            let e: Vec<(u32, u32)> = (0..45).map(|_| (rnd(n), rnd(n))).collect();
            let g = digraph(n, &e);
            let c = scc(&g, &Control::unlimited()).unwrap();
            // reach[a][b] by Floyd-Warshall
            let mut reach = vec![vec![false; n as usize]; n as usize];
            for i in 0..n as usize {
                reach[i][i] = true;
            }
            for &(a, b) in &e {
                reach[a as usize][b as usize] = true;
            }
            for k in 0..n as usize {
                for i in 0..n as usize {
                    for j in 0..n as usize {
                        if reach[i][k] && reach[k][j] {
                            reach[i][j] = true;
                        }
                    }
                }
            }
            for a in 0..n as usize {
                for b in 0..n as usize {
                    assert_eq!(
                        c.comp[a] == c.comp[b],
                        reach[a][b] && reach[b][a],
                        "nodes {a} and {b}"
                    );
                }
            }
        }
    }
}
