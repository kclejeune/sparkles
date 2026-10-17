//! Every long loop over nodes, rows or heap entries of every algorithm polls the cancel flag and the deadline (spec F14
//! §5). Each test cancels the call from inside one loop, on a graph where that loop
//! alone holds more than [`CHECK_EVERY`] operations, and expects the call to stop
//! before the loop ends, within one polling interval.

use super::memory::probe;
use super::testutil::*;
use super::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// More edges than one polling interval, so a loop over them must poll inside.
const BIG: u32 = 3 * CHECK_EVERY as u32;

/// A star: node 0 linked with every other node, read both ways.
fn star() -> Graph {
    let edges: Vec<(u32, u32)> = (1..BIG).map(|v| (0, v)).collect();
    ugraph(BIG, &edges)
}

/// A directed path 0 → 1 → … that one depth-first search walks whole.
fn path() -> Graph {
    let edges: Vec<(u32, u32)> = (0..BIG - 1).map(|v| (v, v + 1)).collect();
    digraph(BIG, &edges)
}

/// A star with edge weights, directed out of node 0.
fn weighted_star() -> Graph {
    let raw = RawEdges {
        nodes: (0..BIG as u64).collect(),
        src: (1..BIG as u64).map(|_| 0).collect(),
        dst: (1..BIG as u64).collect(),
        w: Some((1..BIG).map(|v| (v % 7 + 1) as f32).collect()),
        symmetric: false,
    };
    Graph::build(raw, Aggregate::None, &Par::single(), &Control::unlimited()).unwrap()
}

/// A small row followed by a hub row in the same block: node 0 has one neighbour and
/// node 1 has `BIG - 2`, so the rows of the first block hold more than one interval.
fn small_then_hub() -> Graph {
    let edges: Vec<(u32, u32)> = std::iter::once((0, 2))
        .chain((2..BIG).map(|v| (1, v)))
        .collect();
    ugraph(BIG, &edges)
}

/// The units a hub row of [`small_then_hub`] counts at once: its edges and one more.
const HUB_ROW: usize = BIG as usize - 1;

/// What the probe saw of one cancelled call.
struct Watched {
    /// The units of work that ran after the count that cancelled the call.
    after: usize,
    /// The most units of `phase` counted at once.
    largest: usize,
}

/// Run `f` with a control that is cancelled at the first count of `phase` of at least
/// `from` units, and expect the call to fail as cancelled.
fn watch(
    phase: &'static str,
    from: usize,
    f: impl FnOnce(&Control) -> crate::error::Result<()>,
) -> Watched {
    let flag = Arc::new(AtomicBool::new(false));
    let set = flag.clone();
    let seen = Rc::new(Cell::new(false));
    let after = Rc::new(Cell::new(0usize));
    let largest = Rc::new(Cell::new(0usize));
    let (s, a, l) = (seen.clone(), after.clone(), largest.clone());
    let hook = probe::install(
        move |point| {
            if let probe::Point::Work(p, units) = point {
                if p == phase {
                    l.set(l.get().max(units));
                }
                if s.get() {
                    a.set(a.get() + units);
                } else if p == phase && units >= from {
                    s.set(true);
                    set.store(true, Ordering::Relaxed);
                }
            }
        },
        None,
    );
    let ctl = Control::new(flag, None, u64::MAX);
    let result = f(&ctl);
    drop(hook);
    assert!(seen.get(), "{phase}: the loop was not reached");
    assert!(
        matches!(result, Err(crate::Error::Cancelled)),
        "{phase}: {result:?}"
    );
    Watched {
        after: after.get(),
        largest: largest.get(),
    }
}

/// Run `f` with a control that is cancelled at the first count of `phase`, and check
/// that less than one polling interval of work runs after that count.
fn stops_in(phase: &'static str, f: impl FnOnce(&Control) -> crate::error::Result<()>) {
    let w = watch(phase, 0, f);
    assert!(
        w.after < CHECK_EVERY,
        "{phase}: {} units of work after the cancel",
        w.after
    );
}

/// Check that a loop over a block holding a small row and then a hub row counts the
/// rows one at a time: no count of `phase` covers more than the hub row, and the call
/// stops within one interval of the cancel.
fn counts_rows_of_a_large_block(
    phase: &'static str,
    f: impl FnOnce(&Control) -> crate::error::Result<()>,
) {
    let w = watch(phase, 0, f);
    assert!(
        w.largest <= HUB_ROW,
        "{phase}: {} units counted at once for a block whose largest row holds {HUB_ROW}",
        w.largest
    );
    assert!(
        w.after < CHECK_EVERY,
        "{phase}: {} units of work after the cancel",
        w.after
    );
}

#[test]
fn weakly_connected_components_count_each_row_of_a_large_block() {
    let g = small_then_hub();
    counts_rows_of_a_large_block("wcc-union", |ctl| {
        components::wcc(&g, &Par::single(), ctl).map(drop)
    });
}

#[test]
fn breadth_first_search_counts_each_row_of_a_large_block() {
    let g = small_then_hub();
    counts_rows_of_a_large_block("bfs-frontier", |ctl| {
        traverse::bfs(&g, &[0, 1], &Par::single(), ctl).map(drop)
    });
}

#[test]
fn label_propagation_counts_each_row_of_a_large_block() {
    let g = small_then_hub();
    counts_rows_of_a_large_block("labelprop-node", |ctl| {
        labelprop::run(&g, 1, &Par::single(), ctl).map(drop)
    });
}

#[test]
fn the_simple_view_counts_each_row_of_a_large_block() {
    let g = small_then_hub();
    counts_rows_of_a_large_block("simple-rows", |ctl| g.simple(ctl).map(drop));
}

#[test]
fn strongly_connected_components_split_a_hub_row() {
    // the last node links to every other node, all of them seen by the time the search
    // reaches it, so its scan finds no child and reads the whole row
    let edges: Vec<(u32, u32)> = (0..BIG - 1).map(|v| (BIG - 1, v)).collect();
    let g = digraph(BIG, &edges);
    let w = watch("scc-dfs", CHECK_EVERY, |ctl| {
        components::scc(&g, ctl).map(drop)
    });
    assert!(
        w.largest <= CHECK_EVERY + 1,
        "scc-dfs: {} units counted at once",
        w.largest
    );
    assert!(w.after < CHECK_EVERY, "scc-dfs: {} units after", w.after);
}

#[test]
fn strongly_connected_components_poll_inside_one_search() {
    let g = path();
    stops_in("scc-dfs", |ctl| components::scc(&g, ctl).map(drop));
}

#[test]
fn weakly_connected_components_poll_inside_a_hub_row() {
    let g = star();
    stops_in("wcc-union", |ctl| {
        components::wcc(&g, &Par::single(), ctl).map(drop)
    });
}

#[test]
fn breadth_first_search_polls_inside_one_level() {
    let g = star();
    stops_in("bfs-frontier", |ctl| {
        traverse::bfs(&g, &[0], &Par::single(), ctl).map(drop)
    });
}

#[test]
fn shortest_paths_poll_inside_a_hub_row() {
    let g = weighted_star();
    stops_in("sssp-relax", |ctl| {
        traverse::sssp(&g, &[0], &Par::single(), ctl).map(drop)
    });
}

#[test]
fn label_propagation_polls_inside_one_iteration() {
    let g = star();
    stops_in("labelprop-node", |ctl| {
        labelprop::run(&g, 1, &Par::single(), ctl).map(drop)
    });
}

#[test]
fn betweenness_polls_inside_one_source() {
    let g = star();
    let p = betweenness::Params {
        samples: 2,
        seed: 1,
    };
    for phase in ["betweenness-bfs", "betweenness-accumulate"] {
        stops_in(phase, |ctl| {
            betweenness::run(&g, &p, &Par::single(), ctl).map(drop)
        });
    }
    let g = weighted_star();
    stops_in("betweenness-dijkstra", |ctl| {
        betweenness::run(&g, &p, &Par::single(), ctl).map(drop)
    });
}

#[test]
fn fastrp_polls_inside_one_propagation() {
    let g = star();
    let p = fastrp::Params {
        dimension: 8,
        ..fastrp::Params::default()
    };
    stops_in("fastrp-propagate", |ctl| {
        fastrp::run(&g, &p, &Par::single(), ctl).map(drop)
    });
}

#[test]
fn community_detection_polls_inside_its_phases() {
    let g = star();
    for (phase, leiden) in [
        ("weighted-rows", false),
        ("community-edges", false),
        ("community-local-moving", false),
        ("community-modularity", false),
        ("leiden-external", true),
    ] {
        let p = community::Params {
            leiden,
            ..community::Params::default()
        };
        stops_in(phase, |ctl| community::run(&g, &p, ctl).map(drop));
    }
}

#[test]
fn core_numbers_and_triangles_poll_inside_a_hub_row() {
    let g = star();
    stops_in("simple-rows", |ctl| kcore::run(&g, ctl).map(drop));
    stops_in("kcore-peel", |ctl| kcore::run(&g, ctl).map(drop));
    stops_in("triangles-orient", |ctl| {
        triangles::count(&g, &Par::single(), ctl).map(drop)
    });
    stops_in("clustering-neighbours", |ctl| {
        triangles::clustering_graphalytics(&g, &Par::single(), ctl).map(drop)
    });
}

/// Small nodes count their merges together after each node, and a node whose merges
/// can pass the interval counts each merge before it starts.
#[test]
fn triangle_merges_poll_for_small_and_large_nodes() {
    // disjoint triangles: many nodes with short merges
    let edges: Vec<(u32, u32)> = (0..BIG / 3)
        .flat_map(|t| {
            let a = 3 * t;
            [(a, a + 1), (a + 1, a + 2), (a, a + 2)]
        })
        .collect();
    let g = ugraph(BIG, &edges);
    stops_in("triangles-node", |ctl| {
        triangles::count(&g, &Par::single(), ctl).map(drop)
    });
    // a clique: its first nodes have long lists of higher neighbours
    let k = 400u32;
    let edges: Vec<(u32, u32)> = (0..k)
        .flat_map(|a| (a + 1..k).map(move |b| (a, b)))
        .collect();
    let g = ugraph(k, &edges);
    stops_in("triangles-intersect", |ctl| {
        triangles::count(&g, &Par::single(), ctl).map(drop)
    });
}

#[test]
fn pagerank_polls_inside_a_hub_row() {
    let g = star();
    stops_in("pagerank-pull", |ctl| {
        pagerank::run(&g, &pagerank::Params::default(), &Par::single(), ctl).map(drop)
    });
}

/// The heap of SSSP holds an entry for each improved distance. Its capacity is charged
/// as it grows, so a budget that covers the distances but not the heap stops the call.
#[test]
fn the_shortest_path_heap_is_charged() {
    let g = weighted_star();
    let n = g.n() as u64;
    let distances = n * 8;
    let tight = Control::new(Default::default(), None, distances + n + 4096);
    assert!(matches!(
        traverse::sssp(&g, &[0], &Par::single(), &tight),
        Err(crate::Error::BudgetExceeded(_))
    ));
    let roomy = Control::new(Default::default(), None, u64::MAX);
    let d = traverse::sssp(&g, &[0], &Par::single(), &roomy).unwrap();
    assert_eq!(d[0], 0.0);
    // one 16-byte entry for every leaf at once
    assert!(roomy.peak() >= distances + (n - 1) * 16, "{}", roomy.peak());
}

/// A workspace array is charged before it is allocated, so a budget below one stops
/// the call with a budget error and leaves nothing charged behind.
#[test]
fn workspaces_are_charged_and_given_back() {
    let g = star();
    let n = g.n() as u64;
    for (name, run) in [
        (
            "scc",
            Box::new(|ctl: &Control| components::scc(&g, ctl).map(drop))
                as Box<dyn Fn(&Control) -> crate::error::Result<()>>,
        ),
        (
            "wcc",
            Box::new(|ctl: &Control| components::wcc(&g, &Par::single(), ctl).map(drop)),
        ),
        (
            "bfs",
            Box::new(|ctl: &Control| traverse::bfs(&g, &[0], &Par::single(), ctl).map(drop)),
        ),
        (
            "labelPropagation",
            Box::new(|ctl: &Control| labelprop::run(&g, 2, &Par::single(), ctl).map(drop)),
        ),
    ] {
        // the arrays of n nodes the algorithm needs at least
        let ctl = Control::new(Default::default(), None, n * 4);
        assert!(
            matches!(run(&ctl), Err(crate::Error::BudgetExceeded(_))),
            "{name}"
        );
        assert_eq!(
            ctl.remaining(),
            n * 4,
            "{name}: the failed call kept a charge"
        );
        // a call that fails later, after it holds some workspaces, gives them all back
        for blocks in 2..=16 {
            let limit = n * 4 * blocks;
            let ctl = Control::new(Default::default(), None, limit);
            if run(&ctl).is_err() {
                assert_eq!(
                    ctl.remaining(),
                    limit,
                    "{name}: the call that failed at a budget of {limit} kept a charge"
                );
            }
        }
        let ctl = Control::new(Default::default(), None, u64::MAX);
        run(&ctl).unwrap();
        assert!(ctl.peak() >= n * 8, "{name}: peak {}", ctl.peak());
    }
}
