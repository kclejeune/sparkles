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

/// Run `f` with a control that is cancelled at the first operation of `phase`, and
/// check that the call stops within one polling interval of it.
fn stops_in(phase: &'static str, f: impl FnOnce(&Control) -> crate::error::Result<()>) {
    let flag = Arc::new(AtomicBool::new(false));
    let set = flag.clone();
    let seen = Rc::new(Cell::new(false));
    let since = Rc::new(Cell::new(0usize));
    let (s, c) = (seen.clone(), since.clone());
    let hook = probe::install(
        move |point| {
            if let probe::Point::Work(p) = point {
                if p == phase && !s.get() {
                    s.set(true);
                    set.store(true, Ordering::Relaxed);
                }
                if s.get() {
                    c.set(c.get() + 1);
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
    assert!(
        since.get() <= CHECK_EVERY + 1,
        "{phase}: {} operations after the cancel",
        since.get()
    );
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
        let ctl = Control::new(Default::default(), None, u64::MAX);
        run(&ctl).unwrap();
        assert!(ctl.peak() >= n * 8, "{name}: peak {}", ctl.peak());
    }
}
