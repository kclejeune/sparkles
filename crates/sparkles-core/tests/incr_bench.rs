//! Measurements of incremental views (spec F13 §11): pass time against re-evaluation,
//! commit latency with views defined, and the commit path without views. Run in
//! release mode, alone on a quiet machine:
//!
//! ```text
//! cargo test --release -p sparkles-core --test incr_bench -- --ignored --nocapture --test-threads 1
//! ```
//!
//! `INCR_BENCH_PEOPLE` sets the size (200,000 people, about 1.2M quads, by default).

use sparkles_core::incr::{LiveOptions, OpenHost, Runtime, RuntimeOptions, ViewDef};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{self, QueryOptions};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PREFIX: &str = "PREFIX : <http://ex/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

fn people() -> usize {
    std::env::var("INCR_BENCH_PEOPLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000)
}

/// People in teams with ages, labels for some teams, and a knows graph.
fn data(n: usize) -> String {
    let teams = (n / 200).max(10);
    let mut s = String::with_capacity(n * 120);
    for i in 0..n {
        s.push_str(&format!(
            "<http://ex/p{i}> <http://ex/memberOf> <http://ex/t{}> .\n<http://ex/p{i}> <http://ex/age> \"{}\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n<http://ex/p{i}> <http://ex/knows> <http://ex/p{}> .\n<http://ex/p{i}> <http://ex/knows> <http://ex/p{}> .\n",
            i % teams,
            20 + i % 50,
            (i * 7 + 1) % n,
            (i * 13 + 5) % n
        ));
    }
    for t in 0..teams / 2 {
        s.push_str(&format!(
            "<http://ex/t{t}> <http://www.w3.org/2000/01/rdf-schema#label> \"team {t}\" .\n"
        ));
    }
    s
}

fn store(n: usize, dir: Option<&std::path::Path>) -> Store {
    let s = match dir {
        Some(d) => Store::open(d, StoreOptions::default()).unwrap(),
        None => Store::in_memory(StoreOptions::default()),
    };
    s.load(&[Source::from_bytes(
        data(n).into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

fn upd(s: &Store, u: &str) {
    sparql::update::update(s, &format!("{PREFIX}{u}"), &QueryOptions::default()).unwrap();
}

/// A number never used before in this process, for quads no earlier commit wrote.
fn fresh() -> usize {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

const VIEWS: &[(&str, &str)] = &[
    (
        "team-sizes",
        "SELECT ?t (COUNT(?p) AS ?n) WHERE { ?p :memberOf ?t } GROUP BY ?t",
    ),
    (
        "team-ages",
        "SELECT ?t (SUM(?a) AS ?s) (AVG(?a) AS ?m) WHERE { ?p :memberOf ?t . ?p :age ?a } GROUP BY ?t",
    ),
    (
        "colleagues-known",
        "SELECT ?p ?q WHERE { ?p :knows ?q . ?p :memberOf ?t . ?q :memberOf ?t }",
    ),
    (
        "labelled",
        "SELECT ?p ?l WHERE { ?p :memberOf ?t OPTIONAL { ?t rdfs:label ?l } }",
    ),
    ("teams", "SELECT DISTINCT ?t WHERE { ?p :memberOf ?t }"),
];

/// §11: a single-quad commit's pass against evaluating the query again.
#[test]
#[ignore]
fn pass_against_recompute() {
    let n = people();
    let t0 = Instant::now();
    let s = store(n, None);
    eprintln!("loaded {n} people in {:.1?}", t0.elapsed());
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    println!(
        "| view | build ms | query ms | pass median ms | pass p95 ms | query / pass | state MiB |"
    );
    println!("|---|---|---|---|---|---|---|");
    for (name, q) in VIEWS {
        let text = format!("{PREFIX}{q}");
        let t = Instant::now();
        rt.put(&s, name, ViewDef::table(text.clone())).unwrap();
        rt.round(&s);
        let build = t.elapsed().as_secs_f64() * 1e3;
        let mut query = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let r = sparql::query(
                s.snapshot(),
                &text,
                &QueryOptions {
                    no_cache: true,
                    ..Default::default()
                },
            )
            .unwrap();
            std::hint::black_box(r.len());
            query.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let mut passes = Vec::new();
        for i in 0..200 {
            let f = fresh();
            let who = (f * 7919) % n;
            let u = match i % 4 {
                0 => format!(
                    "INSERT DATA {{ :new{f} :memberOf :t{} . :new{f} :age 33 }}",
                    who % 50
                ),
                1 => format!("INSERT DATA {{ :new{f} :knows :p{who} . :p{who} :knows :new{f} }}"),
                2 => format!("INSERT DATA {{ :p{who} :knows :x{f} }}"),
                _ => format!("DELETE WHERE {{ :p{who} :memberOf ?t }}"),
            };
            upd(&s, &u);
            let t = Instant::now();
            rt.round(&s);
            passes.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let info = rt.get(&s, name).unwrap();
        let qm = pct(&mut query, 0.5);
        let pm = pct(&mut passes, 0.5);
        let p95 = pct(&mut passes, 0.95);
        println!(
            "| {name} | {build:.1} | {qm:.1} | {pm:.3} | {p95:.3} | {:.0} | {:.1} |",
            qm / pm,
            info.state_bytes as f64 / (1 << 20) as f64
        );
        rt.delete(name).unwrap();
        rt.round(&s);
    }
}

fn commits(s: &Store, k: usize) -> (f64, f64, f64) {
    let mut lat = Vec::with_capacity(k);
    let t0 = Instant::now();
    for i in 0..k {
        let f = fresh();
        let t = Instant::now();
        upd(
            s,
            &format!(
                "INSERT DATA {{ :w{f} :memberOf :t{} . :w{f} :age {} }}",
                i % 50,
                i % 60
            ),
        );
        lat.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let total = t0.elapsed().as_secs_f64();
    (pct(&mut lat, 0.5), pct(&mut lat, 0.99), k as f64 / total)
}

/// §11: write latency with 0, 10 and 100 async views (persistent store, group commit
/// off), and how long the views take to catch up after the writes.
#[test]
#[ignore]
fn commit_latency_with_views() {
    let n = people() / 4;
    println!("| views | commit median ms | commit p99 ms | commits/s | catch-up ms |");
    println!("|---|---|---|---|---|");
    for views in [0usize, 10, 100] {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(store(n, Some(dir.path())));
        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(OpenHost));
        let weak = Arc::downgrade(&s);
        let h = rt
            .spawn_worker("bench".into(), s.subscribe_commits(), move || {
                weak.upgrade()
            })
            .unwrap();
        for v in 0..views {
            let (_, q) = VIEWS[v % VIEWS.len()];
            // distinct circuits: each view filters on its own team
            let q = q.replace("WHERE {", &format!("WHERE {{ VALUES ?vx {{ {v} }}"));
            rt.put(&s, &format!("v{v}"), ViewDef::table(format!("{PREFIX}{q}")))
                .unwrap();
        }
        for v in 0..views {
            rt.wait_built(&format!("v{v}"), Duration::from_secs(600))
                .unwrap();
        }
        let (med, p99, rate) = commits(&s, 2000);
        let t = Instant::now();
        let head = s.snapshot().commit;
        while rt.stats(&s).views.iter().any(|v| v.lag_commits > 0) {
            std::thread::sleep(Duration::from_millis(1));
            if t.elapsed() > Duration::from_secs(600) {
                break;
            }
        }
        let _ = head;
        println!(
            "| {views} | {med:.3} | {p99:.3} | {rate:.0} | {:.0} |",
            t.elapsed().as_secs_f64() * 1e3
        );
        rt.close(&s);
        drop(rt);
        drop(s);
        let _ = h.join();
    }
}

/// The commit path with nothing subscribed, and with a live query subscribed (the ring
/// of recent changes filled). The first case runs on main as well, for an A/B.
#[test]
#[ignore]
fn commit_path_without_views() {
    let n = people() / 4;
    let dir = tempfile::tempdir().unwrap();
    let s = store(n, Some(dir.path()));
    for round in 0..3 {
        let (med, p99, rate) = commits(&s, 3000);
        println!(
            "no views, round {round}: median {med:.3} ms, p99 {p99:.3} ms, {rate:.0} commits/s"
        );
    }
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    let live = rt
        .live(
            &s,
            &format!("{PREFIX}SELECT ?p WHERE {{ ?p :nothing ?o }}"),
            None,
            &LiveOptions::default(),
        )
        .unwrap();
    for round in 0..3 {
        let (med, p99, rate) = commits(&s, 3000);
        println!(
            "ring filled, round {round}: median {med:.3} ms, p99 {p99:.3} ms, {rate:.0} commits/s"
        );
    }
    drop(live);
}

/// Where a pass's time goes: reading the changes, building the snapshot before them,
/// and the circuit's own work.
#[test]
#[ignore]
fn pass_breakdown() {
    use sparkles_core::incr::{Circuit, DeltaSet, compile};
    use sparkles_core::task::Control;
    let n = people();
    let s = store(n, None);
    println!("| view | read µs | old µs | advance µs |");
    println!("|---|---|---|---|");
    for (name, q) in VIEWS {
        let parsed = sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).unwrap();
        let mut c = Circuit::new(compile(&parsed, None).unwrap(), None);
        c.build(s.snapshot(), &Control::none()).unwrap();
        let _sub = s.subscribe_deltas();
        let (mut read, mut old, mut adv) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..200 {
            let f = fresh();
            let who = (f * 7919) % n;
            let u = match i % 3 {
                0 => format!(
                    "INSERT DATA {{ :new{f} :memberOf :t{} . :new{f} :age 33 }}",
                    who % 50
                ),
                1 => format!("INSERT DATA {{ :new{f} :knows :p{who} . :p{who} :knows :new{f} }}"),
                _ => format!("DELETE WHERE {{ :p{who} :memberOf ?t }}"),
            };
            upd(&s, &u);
            let t = Instant::now();
            let d = DeltaSet::read(&s, c.applied, &Control::none());
            read.push(t.elapsed().as_secs_f64() * 1e6);
            let t = Instant::now();
            d.old().unwrap();
            old.push(t.elapsed().as_secs_f64() * 1e6);
            let t = Instant::now();
            c.advance(&d, &Control::none()).unwrap();
            adv.push(t.elapsed().as_secs_f64() * 1e6);
        }
        println!(
            "| {name} | {:.1} | {:.1} | {:.1} |",
            pct(&mut read, 0.5),
            pct(&mut old, 0.5),
            pct(&mut adv, 0.5)
        );
    }
}

/// Views on a dataset with inference (spec F13 §4.5): RDFS on read, and the graph of
/// materialized inferences in the default graph. Each view evaluates its whole query
/// again with the inference, so a pass that the changes touch costs about one query,
/// and a commit outside the footprint skips the view.
#[test]
#[ignore]
fn inferred_pass_against_query() {
    use sparkles_core::access::GraphAccess;
    use sparkles_core::incr::{Host, Inference};
    use sparkles_core::sparql::rdfs::{RdfsOnRead, SchemaSource};
    struct Inferring(Inference);
    impl Host for Inferring {
        fn access(&self, _: Option<&str>) -> sparkles_core::Result<Option<Arc<GraphAccess>>> {
            Ok(None)
        }
        fn stored_query(&self, _: &str) -> sparkles_core::Result<String> {
            Err(sparkles_core::Error::NotFound("none".into()))
        }
        fn inference(&self) -> Option<Inference> {
            Some(self.0.clone())
        }
    }
    const INFERRED: &str = "urn:x-sparkles:inferred";
    let n = people();
    let s = store(n, None);
    // the schema, and for the materialized case the inferred types of every person
    upd(
        &s,
        "INSERT DATA { :memberOf rdfs:subPropertyOf :belongsTo . :memberOf rdfs:domain :Member . \
         :Member rdfs:subClassOf :Agent }",
    );
    let mut nt = String::with_capacity(n * 80);
    for i in 0..n {
        nt.push_str(&format!(
            "<http://ex/p{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/Agent> .\n"
        ));
    }
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        RdfFormat::NTriples,
        Some(oxrdf::NamedNode::new_unchecked(INFERRED)),
    )])
    .unwrap();
    let cases: &[(&str, Inference, &str)] = &[
        (
            "rdfs team-sizes",
            Inference::new(
                Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
                Vec::new(),
            )
            .unwrap(),
            "SELECT ?t (COUNT(?p) AS ?n) WHERE { ?p :belongsTo ?t } GROUP BY ?t",
        ),
        (
            "rdfs agents",
            Inference::new(
                Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
                Vec::new(),
            )
            .unwrap(),
            "SELECT (COUNT(?p) AS ?n) WHERE { ?p a :Agent }",
        ),
        (
            "materialized agents",
            Inference::new(None, vec![INFERRED.to_string()]).unwrap(),
            "SELECT (COUNT(?p) AS ?n) WHERE { ?p a :Agent }",
        ),
        (
            "materialized team-sizes",
            Inference::new(None, vec![INFERRED.to_string()]).unwrap(),
            "SELECT ?t (COUNT(?p) AS ?n) WHERE { ?p :memberOf ?t } GROUP BY ?t",
        ),
    ];
    println!(
        "| view | query ms | round median ms | round p95 ms | round / query | unrelated commit median ms | passes | skipped |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for (name, inf, q) in cases {
        let text = format!("{PREFIX}{q}");
        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(Inferring(inf.clone())));
        rt.put(&s, "v", ViewDef::table(text.clone())).unwrap();
        rt.round(&s);
        let mut query = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            let r = sparql::query(
                s.snapshot(),
                &text,
                &QueryOptions {
                    no_cache: true,
                    rdfs: inf.rdfs.clone(),
                    default_graph_extra: inf.default_graph_extra.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
            std::hint::black_box(r.len());
            query.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let (mut passes, mut skips) = (Vec::new(), Vec::new());
        for i in 0..100 {
            let f = fresh();
            let who = (f * 7919) % n;
            // a touching commit, then one outside the footprint
            let u = match i % 3 {
                0 => format!(
                    "INSERT DATA {{ :new{f} :memberOf :t{} . GRAPH <{INFERRED}> {{ :new{f} a :Agent }} }}",
                    who % 50
                ),
                1 => format!("DELETE WHERE {{ :p{who} :memberOf ?t }}"),
                _ => format!("INSERT DATA {{ :p{who} :memberOf :t{} }}", f % 50),
            };
            upd(&s, &u);
            let t = Instant::now();
            rt.round(&s);
            passes.push(t.elapsed().as_secs_f64() * 1e3);
            upd(&s, &format!("INSERT DATA {{ :p{who} :knows :x{f} }}"));
            let t = Instant::now();
            rt.round(&s);
            skips.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let info = rt.get(&s, "v").unwrap();
        let qm = pct(&mut query, 0.5);
        let pm = pct(&mut passes, 0.5);
        let p95 = pct(&mut passes, 0.95);
        println!(
            "| {name} | {qm:.1} | {pm:.1} | {p95:.1} | {:.2} | {:.3} | {} | {} |",
            pm / qm,
            pct(&mut skips, 0.5),
            info.passes,
            info.skipped
        );
        rt.close(&s);
    }
}
