//! Timings of the graph algorithms on a synthetic social graph, by thread count (spec F14
//! §9). Ignored by default:
//!
//! ```text
//! ALGO_NODES=1000000 ALGO_EDGES=5000000 cargo test --release -p sparkles-core \
//!     --test graph_algorithms_scale -- --ignored --nocapture
//! ```

use sparkles_core::algo::{
    Aggregate, Control, Graph, Par, RawEdges, community, components, kcore, labelprop, pagerank,
    triangles,
};
use std::time::Instant;

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A graph with a skewed degree distribution: each edge picks its ends by squaring a
/// uniform draw, so low node numbers are hubs.
fn graph(n: usize, m: usize, par: &Par) -> Graph {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut src = Vec::with_capacity(m * 2);
    let mut dst = Vec::with_capacity(m * 2);
    for _ in 0..m {
        let a = ((next() >> 11) as f64 / (1u64 << 53) as f64).powi(2) * n as f64;
        let b = (next() % n as u64) as f64;
        let (a, b) = (a as u64, b as u64);
        src.push(a);
        dst.push(b);
        src.push(b);
        dst.push(a);
    }
    let raw = RawEdges {
        nodes: (0..n as u64).collect(),
        src,
        dst,
        w: None,
        symmetric: true,
    };
    Graph::build(raw, Aggregate::None, par, &Control::unlimited()).unwrap()
}

/// The cost of polling cancellation: each algorithm runs `ALGO_ROUNDS` times on
/// `ALGO_THREADS` threads under a control with a far deadline, as a query's call has,
/// and the fastest round is printed. Run it on two builds and compare. Ignored by
/// default:
///
/// ```text
/// ALGO_NODES=1000000 ALGO_EDGES=5000000 ALGO_ROUNDS=5 cargo test --release \
///     -p sparkles-core --test graph_algorithms_scale polling_cost -- --ignored --nocapture
/// ```
#[test]
#[ignore]
fn polling_cost() {
    use sparkles_core::algo::{betweenness, fastrp, traverse};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    let n = env("ALGO_NODES", 200_000);
    let m = env("ALGO_EDGES", 1_000_000);
    let rounds = env("ALGO_ROUNDS", 5);
    let threads = env("ALGO_THREADS", 1);
    let g = graph(n, m, &Par::with_threads(8));
    let ctl = Control::new(
        Arc::new(AtomicBool::new(false)),
        Some(Instant::now() + Duration::from_secs(24 * 3600)),
        u64::MAX,
    );
    let par = Par::with_threads(threads);
    let only = std::env::var("ALGO_ONLY").ok();
    let time = |name: &str, f: &dyn Fn()| {
        if only
            .as_deref()
            .is_some_and(|o| !o.split(',').any(|o| o == name))
        {
            return;
        }
        let mut best = f64::INFINITY;
        for _ in 0..rounds {
            let t = Instant::now();
            f();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        println!("POLL {name:<12} {best:>9.1} ms");
    };
    time("pageRank", &|| {
        pagerank::run(&g, &pagerank::Params::default(), &par, &ctl).unwrap();
    });
    time("wcc", &|| {
        components::wcc(&g, &par, &ctl).unwrap();
    });
    time("scc", &|| {
        components::scc(&g, &ctl).unwrap();
    });
    time("bfs", &|| {
        traverse::bfs(&g, &[0], &par, &ctl).unwrap();
    });
    time("labelProp", &|| {
        labelprop::run(&g, 10, &par, &ctl).unwrap();
    });
    time("triangles", &|| {
        triangles::count(&g, &par, &ctl).unwrap();
    });
    time("kCore", &|| {
        kcore::run(&g, &ctl).unwrap();
    });
    time("betweenness", &|| {
        let p = betweenness::Params {
            samples: 16,
            seed: 1,
        };
        betweenness::run(&g, &p, &par, &ctl).unwrap();
    });
    time("fastRP", &|| {
        let p = fastrp::Params {
            dimension: 64,
            ..fastrp::Params::default()
        };
        fastrp::run(&g, &p, &par, &ctl).unwrap();
    });
    if std::env::var_os("ALGO_COMMUNITY").is_some() {
        time("louvain", &|| {
            community::run(&g, &community::Params::default(), &ctl).unwrap();
        });
    }
}

#[test]
#[ignore]
fn scaling() {
    let n = env("ALGO_NODES", 200_000);
    let m = env("ALGO_EDGES", 1_000_000);
    let ctl = Control::unlimited();
    let t = Instant::now();
    let g = graph(n, m, &Par::with_threads(8));
    println!(
        "projection: {n} nodes, {} stored edges, built in {:.2?} ({:.1} M edges/s), {:.1} MiB",
        g.m(),
        t.elapsed(),
        g.m() as f64 / t.elapsed().as_secs_f64() / 1e6,
        g.bytes() as f64 / (1 << 20) as f64
    );
    println!("{:<12} {:>8} {:>8} {:>8} {:>8}", "", "1", "2", "4", "8");
    let row = |name: &str, f: &dyn Fn(&Par)| {
        let mut cells = Vec::new();
        for threads in [1, 2, 4, 8] {
            let par = Par::with_threads(threads);
            let t = Instant::now();
            f(&par);
            cells.push(format!("{:>7.0}ms", t.elapsed().as_secs_f64() * 1000.0));
        }
        println!("{name:<12} {}", cells.join(" "));
    };
    row("pageRank", &|par| {
        pagerank::run(&g, &pagerank::Params::default(), par, &ctl).unwrap();
    });
    row("wcc", &|par| {
        components::wcc(&g, par, &ctl).unwrap();
    });
    row("labelProp", &|par| {
        labelprop::run(&g, 10, par, &ctl).unwrap();
    });
    row("triangles", &|par| {
        triangles::count(&g, par, &ctl).unwrap();
    });
    // the moving phases of Louvain and Leiden are sequential
    let t = Instant::now();
    kcore::run(&g, &ctl).unwrap();
    println!("kCore        {:.2?} (sequential)", t.elapsed());
    for leiden in [false, true] {
        let t = Instant::now();
        let p = community::Params {
            leiden,
            ..Default::default()
        };
        let o = community::run(&g, &p, &ctl).unwrap();
        println!(
            "{:<12} {:.2?} (sequential), modularity {:.4}",
            if leiden { "leiden" } else { "louvain" },
            t.elapsed(),
            o.modularity.last().copied().unwrap_or(0.0)
        );
    }
}
