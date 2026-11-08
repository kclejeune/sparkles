//! Zero-or-more and one-or-more property paths count their closure state against the
//! query memory budget, stop at the deadline, and walk an alternative of predicates
//! from a bound end by index lookups, in eager and cursor execution alike.
use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    ExecutionMode, QueryExecution, QueryKind, QueryOptions, query_execution,
};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

const MODES: [ExecutionMode; 3] = [
    ExecutionMode::Eager,
    ExecutionMode::Streaming,
    ExecutionMode::Auto,
];

fn store(data: &str) -> Store {
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            data.as_bytes().to_vec(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    store
}

fn run(store: &Store, q: &str, opts: &QueryOptions, mode: ExecutionMode) -> Result<Rows, Error> {
    let text = |row: Vec<Option<Term>>| -> Vec<String> {
        row.into_iter()
            .map(|t| t.map(|t| t.to_string()).unwrap_or_default())
            .collect()
    };
    let mut rows = match query_execution(store.snapshot(), q, opts, &Default::default(), mode)? {
        QueryExecution::Ask(a) => return Ok(Rows::Ask(a.result().boolean)),
        QueryExecution::Eager(r) if r.result().kind == QueryKind::Ask => {
            return Ok(Rows::Ask(r.result().boolean));
        }
        QueryExecution::Eager(r) => r.into_result().rows().into_iter().map(text).collect(),
        QueryExecution::Select(mut c) => {
            let mut rows = Vec::new();
            while let Some(b) = c.next_batch()? {
                for i in 0..b.len() {
                    rows.push(text(b.row(i).unwrap().values().to_vec()));
                }
            }
            rows
        }
        QueryExecution::Graph(_) => unreachable!("no graph queries here"),
    };
    rows.sort();
    Ok(Rows::Select(rows))
}

#[derive(Debug, PartialEq)]
enum Rows {
    Ask(bool),
    Select(Vec<Vec<String>>),
}

fn memory_exceeded(r: &Result<Rows, Error>) -> bool {
    matches!(r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
}

/// A star of `n` leaves under `<urn:root>` over `<urn:b>`.
fn star(n: usize) -> Store {
    let mut data = String::new();
    for i in 0..n {
        data.push_str(&format!("<urn:leaf:{i}> <urn:b> <urn:root> .\n"));
    }
    store(&data)
}

#[test]
fn closure_state_counts_against_the_memory_budget() {
    // The closure from the root reaches 200,000 nodes. Its visited set and frontier
    // take several times the bytes of the one-column result, and the budget sits
    // between the two, so only a closure whose state is charged fails.
    let store = star(200_000);
    let q = "SELECT (COUNT(*) AS ?n) WHERE { ?leaf <urn:b>* <urn:root> }";
    let tight = QueryOptions {
        max_memory_bytes: Some(4 << 20),
        no_cache: true,
        ..Default::default()
    };
    for mode in MODES {
        let r = run(&store, q, &tight, mode);
        assert!(memory_exceeded(&r), "{mode:?}: {r:?}");
    }
    let ample = QueryOptions {
        max_memory_bytes: Some(256 << 20),
        no_cache: true,
        ..Default::default()
    };
    for mode in MODES {
        let r = run(&store, q, &ample, mode).unwrap();
        let Rows::Select(rows) = r else { panic!() };
        assert!(rows[0][0].starts_with("\"200001\""), "{mode:?}: {rows:?}");
    }
}

#[test]
fn edge_relation_of_a_closure_counts_against_the_memory_budget() {
    // Two unbound ends over an alternative need the whole edge relation, which is
    // charged as it is built: about 190 bytes for each edge of a chain against the
    // 16 of each edge row and result row.
    let mut data = String::new();
    for i in 0..20_000 {
        let p = if i % 2 == 0 { "r" } else { "d" };
        data.push_str(&format!("<urn:hub> <urn:{p}> <urn:n:{i}> .\n"));
    }
    let store = store(&data);
    let q = "SELECT (COUNT(*) AS ?n) WHERE { ?s (<urn:r>|<urn:d>)+ ?t }";
    let tight = QueryOptions {
        max_memory_bytes: Some(2 << 20),
        no_cache: true,
        ..Default::default()
    };
    for mode in MODES {
        let r = run(&store, q, &tight, mode);
        assert!(memory_exceeded(&r), "{mode:?}: {r:?}");
    }
    let ample = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    for mode in MODES {
        let Rows::Select(rows) = run(&store, q, &ample, mode).unwrap() else {
            panic!()
        };
        assert!(rows[0][0].starts_with("\"20000\""), "{mode:?}: {rows:?}");
    }
}

/// A chain of `n` nodes over `<urn:broader>`, from `<urn:c:0>` up.
fn chain(n: usize) -> Store {
    let mut data = String::new();
    for i in 0..n - 1 {
        data.push_str(&format!("<urn:c:{i}> <urn:broader> <urn:c:{}> .\n", i + 1));
    }
    store(&data)
}

#[test]
fn ask_over_a_closure_with_unbound_ends_meets_its_deadline() {
    // Every one of the 30,000 nodes reaches all the nodes above it, about 450 million
    // pairs in all, so no evaluation of the whole closure meets the deadline. ASK needs
    // one pair, which the walk from the first start node gives.
    let store = chain(30_000);
    let opts = QueryOptions {
        timeout: Some(Duration::from_secs(5)),
        max_memory_bytes: Some(64 << 20),
        no_cache: true,
        ..Default::default()
    };
    for q in [
        "ASK { ?a <urn:broader>* ?b }",
        "ASK { ?a <urn:broader>+ ?b }",
        "SELECT ?a ?b WHERE { ?a <urn:broader>* ?b } LIMIT 3",
    ] {
        for mode in MODES {
            let started = Instant::now();
            let r = run(&store, q, &opts, mode);
            let took = started.elapsed();
            match r {
                Ok(Rows::Ask(b)) => assert!(b, "{q} {mode:?}"),
                Ok(Rows::Select(rows)) => assert_eq!(rows.len(), 3, "{q} {mode:?}"),
                Err(e) => panic!("{q} {mode:?}: {e} after {took:?}"),
            }
        }
    }
}

#[test]
fn a_closure_with_unbound_ends_stops_at_the_deadline() {
    // The whole closure of the chain is far more work than the deadline allows: every
    // mode fails with the timeout, at the deadline rather than after the closure.
    let store = chain(30_000);
    let opts = QueryOptions {
        timeout: Some(Duration::from_millis(300)),
        no_cache: true,
        ..Default::default()
    };
    let q = "SELECT (COUNT(*) AS ?n) WHERE { ?a <urn:broader>* ?b }";
    for mode in MODES {
        let started = Instant::now();
        let r = run(&store, q, &opts, mode);
        let took = started.elapsed();
        assert!(matches!(r, Err(Error::Timeout)), "{mode:?}: {r:?}");
        assert!(took < Duration::from_secs(10), "{mode:?}: {took:?}");
    }
}

/// A graph with cycles and hubs over `<urn:r>` and `<urn:d>`, with `<urn:other>`
/// edges that the paths must not follow.
fn tangle() -> (Store, Vec<(u32, &'static str, u32)>) {
    let mut edges = Vec::new();
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |m: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % m) as u32
    };
    for i in 0..400u32 {
        // a ring, chords and two hubs
        edges.push((i, "r", (i + 1) % 400));
        edges.push((i, if i % 3 == 0 { "d" } else { "r" }, next(400)));
        if i % 7 == 0 {
            edges.push((0, "d", i));
            edges.push((i, "r", 1));
        }
        edges.push((i, "other", next(400)));
    }
    // a component the seeds do not reach
    for i in 1000..1010u32 {
        edges.push((i, "d", i + 1));
    }
    let mut data = String::new();
    for (s, p, o) in &edges {
        data.push_str(&format!("<urn:n:{s}> <urn:{p}> <urn:n:{o}> .\n"));
    }
    (store(&data), edges)
}

/// The nodes reached from `seed` over the edges of `preds` (`rev`: backwards).
fn oracle(
    edges: &[(u32, &str, u32)],
    preds: &[(&str, bool)],
    seed: u32,
    min: usize,
) -> BTreeSet<u32> {
    let mut adj: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &(s, p, o) in edges {
        for &(q, rev) in preds {
            if p == q {
                let (a, b) = if rev { (o, s) } else { (s, o) };
                adj.entry(a).or_default().push(b);
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    if min == 0 {
        seen.insert(seed);
    }
    queue.push_back(seed);
    while let Some(x) = queue.pop_front() {
        for &y in adj.get(&x).into_iter().flatten() {
            if seen.insert(y) {
                queue.push_back(y);
            }
        }
    }
    seen
}

#[test]
fn bound_seed_alternative_closure_returns_the_same_answers() {
    let (store, edges) = tangle();
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let seeds = [0u32, 5, 399, 1005];
    let values = seeds
        .iter()
        .map(|s| format!("<urn:n:{s}>"))
        .collect::<Vec<_>>()
        .join(" ");
    type Case<'a> = (&'a str, &'a [(&'a str, bool)], usize);
    let cases: [Case; 5] = [
        ("(<urn:r>|<urn:d>)*", &[("r", false), ("d", false)], 0),
        ("(<urn:r>|<urn:d>)+", &[("r", false), ("d", false)], 1),
        ("(<urn:d>|^<urn:r>)*", &[("d", false), ("r", true)], 0),
        ("(<urn:d>|<urn:missing>)*", &[("d", false)], 0),
        ("(<urn:r>|<urn:d>)?", &[], 0),
    ];
    for (path, preds, min) in cases {
        let mut want: Vec<Vec<String>> = Vec::new();
        for &s in &seeds {
            let reached = if preds.is_empty() {
                // zero or one step over r or d
                let mut r: BTreeSet<u32> = edges
                    .iter()
                    .filter(|(a, p, _)| *a == s && (*p == "r" || *p == "d"))
                    .map(|(_, _, b)| *b)
                    .collect();
                r.insert(s);
                r
            } else {
                oracle(&edges, preds, s, min)
            };
            for t in reached {
                want.push(vec![format!("<urn:n:{s}>"), format!("<urn:n:{t}>")]);
            }
        }
        want.sort();
        let bound = format!("SELECT ?s ?t WHERE {{ VALUES ?s {{ {values} }} ?s {path} ?t }}");
        // The same closure with both ends unbound reads the whole edge relation.
        let unbound = format!(
            "SELECT ?s ?t WHERE {{ ?s {path} ?t FILTER(?s IN ({})) }}",
            seeds
                .iter()
                .map(|s| format!("<urn:n:{s}>"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for mode in MODES {
            for q in [&bound, &unbound] {
                let Rows::Select(rows) = run(&store, q, &opts, mode).unwrap() else {
                    panic!()
                };
                assert_eq!(rows, want, "{q} {mode:?}");
            }
        }
        // From a constant end, in both directions.
        for &s in &seeds {
            let q = format!("SELECT ?t WHERE {{ <urn:n:{s}> {path} ?t }}");
            let back = format!("SELECT ?t WHERE {{ ?t ^({path}) <urn:n:{s}> }}");
            let mut want: Vec<Vec<String>> = want
                .iter()
                .filter(|r| r[0] == format!("<urn:n:{s}>"))
                .map(|r| vec![r[1].clone()])
                .collect();
            want.sort();
            for mode in MODES {
                for q in [&q, &back] {
                    let Rows::Select(rows) = run(&store, q, &opts, mode).unwrap() else {
                        panic!()
                    };
                    assert_eq!(rows, want, "{q} {mode:?}");
                }
            }
        }
    }
}

#[test]
fn bound_seed_alternative_closure_reads_no_edge_relation() {
    // A seed bound by VALUES walks by index lookups, so the closure of a few seeds
    // fits a budget far below the bytes of the alternative's edge relation.
    let mut data = String::new();
    for i in 0..100_000 {
        let p = if i % 2 == 0 { "r" } else { "d" };
        data.push_str(&format!("<urn:a:{i}> <urn:{p}> <urn:b:{i}> .\n"));
    }
    data.push_str("<urn:seed> <urn:r> <urn:x> .\n<urn:x> <urn:d> <urn:seed> .\n");
    let store = store(&data);
    let opts = QueryOptions {
        max_memory_bytes: Some(1 << 20),
        no_cache: true,
        ..Default::default()
    };
    let q = "SELECT ?t WHERE { VALUES ?s { <urn:seed> } ?s (<urn:r>|<urn:d>)* ?t }";
    for mode in MODES {
        let Rows::Select(rows) = run(&store, q, &opts, mode).unwrap() else {
            panic!()
        };
        assert_eq!(
            rows,
            vec![vec!["<urn:seed>".to_string()], vec!["<urn:x>".to_string()]],
            "{mode:?}"
        );
    }
}

#[test]
fn one_hub_lookup_is_charged_as_it_is_read() {
    // One lookup returns all 300,000 neighbours of the hub. The neighbours are charged
    // as they are read, so the query fails near its budget rather than after holding
    // them all, and the closure state for them, uncharged.
    let mut data = String::new();
    for i in 0..300_000 {
        let p = if i % 2 == 0 { "p" } else { "q" };
        data.push_str(&format!("<urn:hub> <urn:{p}> <urn:n:{i}> .\n"));
    }
    let store = store(&data);
    let limit = 1u64 << 20;
    let opts = QueryOptions {
        max_memory_bytes: Some(limit),
        no_cache: true,
        ..Default::default()
    };
    for q in [
        "SELECT (COUNT(*) AS ?n) WHERE { <urn:hub> (<urn:p>|<urn:q>)* ?t }",
        "SELECT (COUNT(*) AS ?n) WHERE { <urn:hub> <urn:p>* ?t }",
    ] {
        for mode in MODES {
            match run(&store, q, &opts, mode) {
                Err(Error::BudgetExceeded(b)) => {
                    assert_eq!(b.kind, BudgetKind::Memory, "{q} {mode:?}");
                    assert!(b.requested < 4 * limit, "{q} {mode:?}: {b:?}");
                }
                r => panic!("{q} {mode:?}: {r:?}"),
            }
        }
    }
}

#[test]
fn neighbours_of_many_alternative_predicates_stay_charged_together() {
    // Each of the 64 lookups fits the budget, and their neighbours together do not.
    let mut data = String::new();
    for p in 0..64 {
        for i in 0..10_000 {
            data.push_str(&format!("<urn:hub> <urn:p:{p}> <urn:n:{p}:{i}> .\n"));
        }
    }
    let store = store(&data);
    let alternative = (0..64)
        .map(|p| format!("<urn:p:{p}>"))
        .collect::<Vec<_>>()
        .join("|");
    let q = format!("SELECT (COUNT(*) AS ?n) WHERE {{ <urn:hub> ({alternative})+ ?t }}");
    let limit = 1u64 << 20;
    let opts = QueryOptions {
        max_memory_bytes: Some(limit),
        no_cache: true,
        ..Default::default()
    };
    for mode in MODES {
        match run(&store, &q, &opts, mode) {
            Err(Error::BudgetExceeded(b)) => {
                assert_eq!(b.kind, BudgetKind::Memory, "{mode:?}");
                assert!(b.requested < 4 * limit, "{mode:?}: {b:?}");
            }
            r => panic!("{mode:?}: {r:?}"),
        }
    }
}

#[test]
fn a_sweep_charges_only_the_capacity_it_adds() {
    // The second level of 256 nodes is expanded by one sweep over the 65,536 edges
    // between the two layers. The neighbour buffer the closure already holds is not
    // charged a second time, so the closure fits a budget it fits without the sweep.
    let mut data = String::new();
    for a in 0..256 {
        data.push_str(&format!("<urn:root> <urn:p> <urn:a:{a}> .\n"));
        for b in 0..256 {
            data.push_str(&format!("<urn:a:{a}> <urn:p> <urn:b:{b}> .\n"));
        }
    }
    for b in 0..256 {
        data.push_str(&format!("<urn:b:{b}> <urn:p> <urn:end> .\n"));
    }
    let store = store(&data);
    let q = "SELECT (COUNT(*) AS ?n) WHERE { <urn:root> <urn:p>* ?t }";
    let opts = QueryOptions {
        max_memory_bytes: Some(700 << 10),
        no_cache: true,
        ..Default::default()
    };
    for mode in MODES {
        let Rows::Select(rows) = run(&store, q, &opts, mode).unwrap() else {
            panic!()
        };
        assert!(rows[0][0].starts_with("\"514\""), "{mode:?}: {rows:?}");
    }
}

#[test]
fn growth_stays_charged_from_one_predicate_to_the_next() {
    // Each predicate's 20,000 neighbours fit the budget, and the buffer that holds
    // both does not. The query fails while it expands, not after the closure has
    // built its visited set and frontier for both.
    let mut data = String::new();
    for p in ["p", "q"] {
        for i in 0..20_000 {
            data.push_str(&format!("<urn:hub> <urn:{p}> <urn:n:{p}:{i}> .\n"));
        }
    }
    let store = store(&data);
    let limit = 300u64 << 10;
    let opts = QueryOptions {
        max_memory_bytes: Some(limit),
        no_cache: true,
        ..Default::default()
    };
    let q = "SELECT (COUNT(*) AS ?n) WHERE { <urn:hub> (<urn:p>|<urn:q>)+ ?t }";
    for mode in MODES {
        match run(&store, q, &opts, mode) {
            Err(Error::BudgetExceeded(b)) => {
                assert_eq!(b.kind, BudgetKind::Memory, "{mode:?}");
                assert!(b.requested < 4 * limit, "{mode:?}: {b:?}");
            }
            r => panic!("{mode:?}: {r:?}"),
        }
    }
}
