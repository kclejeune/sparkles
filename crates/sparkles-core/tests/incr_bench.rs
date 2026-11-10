//! Measurements of incremental views (spec F13 §11): pass time against re-evaluation,
//! commit latency with views defined, and the commit path without views. Run in
//! release mode, alone on a quiet machine:
//!
//! ```text
//! cargo test --release -p sparkles-core --test incr_bench -- --ignored --nocapture --test-threads 1
//! ```
//!
//! `INCR_BENCH_PEOPLE` sets the size (200,000 people, about 1.2M quads, by default).
//! `INCR_BENCH_JSON=path` also appends each measured value as a JSON line, which
//! `scripts/bench-gate.py metrics` turns into a gate record. `mise run bench:incr` runs
//! every test this way (ROADMAP item 471, docs/benchmarks/writes/README.md).

use sparkles_core::incr::{
    InternalAccess, InternalSpec, LiveOptions, OpenHost, Provenance, Runtime, RuntimeOptions,
    ViewDef,
};
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

/// Append one value to `INCR_BENCH_JSON`, when it is set. `better` is `lower` or `higher`.
fn metric(key: &str, value: f64, unit: &str, better: &str) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("INCR_BENCH_JSON") else {
        return;
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    let line = serde_json::json!({ "key": key, "value": value, "unit": unit, "better": better });
    writeln!(f, "{line}").unwrap();
}

/// Append one check to `INCR_BENCH_JSON`: a condition the timings depend on, such as a
/// maintained view equal to evaluating its query again.
fn check(name: &str, ok: bool, detail: String) {
    check_with(name, ok, detail, None);
}

/// `check` with a fingerprint, which a comparison requires to equal the baseline's.
fn check_with(name: &str, ok: bool, detail: String, fingerprint: Option<String>) {
    use std::io::Write as _;
    if !ok {
        eprintln!("check failed: {name}: {detail}");
    }
    let Some(path) = std::env::var_os("INCR_BENCH_JSON") else {
        assert!(ok, "{name}: {detail}");
        return;
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    let mut line = serde_json::json!({ "check": name, "ok": ok, "detail": detail });
    if let Some(fp) = fingerprint {
        line["fingerprint"] = fp.into();
    }
    writeln!(f, "{line}").unwrap();
}

/// Rows as sorted strings, each repeated by its multiplicity. A row of negative weight,
/// which a correct maintained result never holds, is kept as a marked row so that the
/// answer cannot match a query's.
fn rows_of(rows: &[(Vec<Option<oxrdf::Term>>, i64)]) -> Vec<String> {
    let mut out: Vec<String> = rows
        .iter()
        .flat_map(|(r, w)| {
            let s = r
                .iter()
                .map(|t| t.as_ref().map_or("UNDEF".into(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ");
            if *w < 0 {
                return vec![format!("weight {w}: {s}")];
            }
            vec![s; *w as usize]
        })
        .collect();
    out.sort();
    out
}

#[test]
fn negative_weights_are_kept() {
    let row = vec![Some(oxrdf::NamedNode::new_unchecked("http://ex/a").into())];
    let good = rows_of(&[(row.clone(), 1)]);
    assert_ne!(good, rows_of(&[(row.clone(), 1), (row, -1)]));
}

/// Check that the view `name` is at the store's head commit and holds the rows of
/// evaluating `text` again with `opts`, outside every timed interval.
fn check_view(rt: &Runtime, s: &Store, key: &str, name: &str, text: &str, opts: QueryOptions) {
    let snap = s.snapshot();
    let read = rt.read(&sparkles_core::sparql::viewsvc::ViewRead {
        name,
        at: snap.commit,
        snap: Some(&snap),
        allow_stale: false,
        caller: None,
        deadline: None,
    });
    let fresh = sparql::query(
        s.snapshot(),
        text,
        &QueryOptions {
            no_cache: true,
            ..opts
        },
    )
    .unwrap();
    let want = rows_of(&fresh.rows().into_iter().map(|r| (r, 1)).collect::<Vec<_>>());
    // the evaluated answer's variables and rows, which stay the same across builds, so that
    // a change that breaks the view and its query alike still differs from the baseline
    let fingerprint = answer_fingerprint(&fresh.vars, &want);
    let (ok, detail) = match read {
        Ok(r) if r.commit != snap.commit => (
            false,
            format!("the view is at commit {} of {}", r.commit, snap.commit),
        ),
        Ok(r) if r.columns != fresh.vars => (
            false,
            format!(
                "the view's columns are {:?}, the query's {:?}",
                r.columns, fresh.vars
            ),
        ),
        Ok(r) => {
            let got = rows_of(&r.rows);
            (
                got == want,
                format!(
                    "{} maintained rows, {} evaluated rows",
                    got.len(),
                    want.len()
                ),
            )
        }
        Err(e) => (false, e.to_string()),
    };
    check_with(&format!("{key}/answer"), ok, detail, Some(fingerprint));
}

/// The row count and an FNV-1a digest of the variables and the sorted rows, which stay
/// the same across builds.
fn answer_fingerprint(vars: &[String], rows: &[String]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for row in std::iter::once(&vars.join(" ")).chain(rows) {
        for b in row.bytes().chain(*b"\n") {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{} rows, {h:016x}", rows.len())
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

/// The workload's version, which a change to the commits a test makes bumps, so that a
/// comparison with an older baseline fails.
const WORKLOAD: &str = "incr_bench v1";

/// A check whose fingerprint covers the data, the views and the workload's version, so
/// that a comparison with a baseline of another workload fails even when every view
/// matches its query.
#[test]
#[ignore]
fn workload() {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let views: Vec<&str> = VIEWS.iter().flat_map(|(n, q)| [*n, *q]).collect();
    for p in [data(people()).as_str(), &views.join("\n"), WORKLOAD] {
        for b in p.bytes().chain([0]) {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    let Some(path) = std::env::var_os("INCR_BENCH_JSON") else {
        return;
    };
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    let line = serde_json::json!({ "check": "workload", "ok": true,
        "fingerprint": format!("{h:016x}"), "detail": format!("{} people", people()) });
    writeln!(f, "{line}").unwrap();
}

/// §11: a one- or two-quad commit's pass against evaluating the query again.
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
        check_view(
            &rt,
            &s,
            &format!("pass/{name}"),
            name,
            &text,
            QueryOptions::default(),
        );
        let info = rt.get(&s, name).unwrap();
        let qm = pct(&mut query, 0.5);
        let pm = pct(&mut passes, 0.5);
        let p95 = pct(&mut passes, 0.95);
        let state = info.state_bytes as f64 / (1 << 20) as f64;
        println!(
            "| {name} | {build:.1} | {qm:.1} | {pm:.3} | {p95:.3} | {:.0} | {state:.1} |",
            qm / pm,
        );
        metric(&format!("pass/{name}/build"), build, "ms", "lower");
        metric(&format!("pass/{name}/query"), qm, "ms", "lower");
        metric(&format!("pass/{name}/pass-median"), pm, "ms", "lower");
        metric(&format!("pass/{name}/pass-p95"), p95, "ms", "lower");
        metric(&format!("pass/{name}/state"), state, "MiB", "lower");
        rt.delete(name).unwrap();
        rt.round(&s);
    }
}

/// One commit of the pass benchmarks' mix: new members, new knows edges both ways,
/// an edge to a new person, and a member who leaves.
fn mix(s: &Store, i: usize, n: usize) {
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
    upd(s, &u);
}

/// §5: the cost of each provenance annotation. Each view's query runs as an internal
/// circuit that keeps its rows, once per setting, through the same 200 commits as
/// `pass_against_recompute`. `INCR_BENCH_MODES` names the settings to run, by default
/// `none,count,lineage,why`, and `INCR_BENCH_VIEWS` the views.
#[test]
#[ignore]
fn annotation_cost() {
    let n = people();
    let t0 = Instant::now();
    let s = store(n, None);
    eprintln!("loaded {n} people in {:.1?}", t0.elapsed());
    let modes: Vec<String> = std::env::var("INCR_BENCH_MODES")
        .unwrap_or_else(|_| "none,count,lineage,why".into())
        .split(',')
        .map(str::to_string)
        .collect();
    let only: Option<Vec<String>> = std::env::var("INCR_BENCH_VIEWS")
        .ok()
        .map(|v| v.split(',').map(str::to_string).collect());
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    println!("| view | provenance | build ms | pass median ms | pass p95 ms | state MiB |");
    println!("|---|---|---|---|---|---|");
    for (name, q) in VIEWS {
        if only.as_ref().is_some_and(|o| !o.iter().any(|x| x == name)) {
            continue;
        }
        let text = format!("{PREFIX}{q}");
        for mode in &modes {
            let p = match mode.as_str() {
                "none" => Provenance::None,
                "count" => Provenance::Count,
                "lineage" => Provenance::Lineage,
                "why" => Provenance::why(),
                m => panic!("unknown provenance {m}"),
            };
            let t = Instant::now();
            let c = rt
                .attach_internal(
                    &s,
                    InternalSpec::rows(format!("{name}-{mode}"), text.clone())
                        .with_provenance(p)
                        .with_memory(16 << 30),
                    InternalAccess::everything(),
                )
                .unwrap();
            let build = t.elapsed().as_secs_f64() * 1e3;
            let mut passes = Vec::new();
            for i in 0..200 {
                mix(&s, i, n);
                let t = Instant::now();
                rt.round(&s);
                passes.push(t.elapsed().as_secs_f64() * 1e3);
            }
            assert_eq!(c.commit(), s.snapshot().commit);
            let bytes = c.bytes();
            println!(
                "| {name} | {mode} | {build:.1} | {:.3} | {:.3} | {:.1} |",
                pct(&mut passes, 0.5),
                pct(&mut passes, 0.95),
                bytes as f64 / (1 << 20) as f64
            );
            drop(c);
            rt.round(&s);
        }
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

/// §11: write latency with 0, 10 and 100 async views on a persistent store with the
/// default store options, and how long the views take to catch up after the writes.
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
        let mut caught_up = true;
        while rt.stats(&s).views.iter().any(|v| v.lag_commits > 0) {
            std::thread::sleep(Duration::from_millis(1));
            if t.elapsed() > Duration::from_secs(600) {
                caught_up = false;
                break;
            }
        }
        let catch_up = t.elapsed().as_secs_f64() * 1e3;
        check(
            &format!("commits/{views}-views/caught-up"),
            caught_up,
            format!("the views caught up in {catch_up:.0} ms"),
        );
        // a few views against their queries evaluated again
        for v in (0..views).step_by(7) {
            let (_, q) = VIEWS[v % VIEWS.len()];
            let q = q.replace("WHERE {", &format!("WHERE {{ VALUES ?vx {{ {v} }}"));
            check_view(
                &rt,
                &s,
                &format!("commits/{views}-views/v{v}"),
                &format!("v{v}"),
                &format!("{PREFIX}{q}"),
                QueryOptions::default(),
            );
        }
        println!("| {views} | {med:.3} | {p99:.3} | {rate:.0} | {catch_up:.0} |");
        metric(&format!("commits/{views}-views/median"), med, "ms", "lower");
        metric(&format!("commits/{views}-views/p99"), p99, "ms", "lower");
        metric(
            &format!("commits/{views}-views/rate"),
            rate,
            "commits/s",
            "higher",
        );
        metric(
            &format!("commits/{views}-views/catch-up"),
            catch_up,
            "ms",
            "lower",
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
    // the median over the rounds of each value, for the gate record
    let rounds = |case: &str, all: &mut Vec<(f64, f64, f64)>| {
        let mut med: Vec<f64> = all.iter().map(|r| r.0).collect();
        let mut p99: Vec<f64> = all.iter().map(|r| r.1).collect();
        let mut rate: Vec<f64> = all.iter().map(|r| r.2).collect();
        metric(
            &format!("commit-path/{case}/median"),
            pct(&mut med, 0.5),
            "ms",
            "lower",
        );
        metric(
            &format!("commit-path/{case}/p99"),
            pct(&mut p99, 0.5),
            "ms",
            "lower",
        );
        metric(
            &format!("commit-path/{case}/rate"),
            pct(&mut rate, 0.5),
            "commits/s",
            "higher",
        );
    };
    let start = s.snapshot().commit;
    let quads = s.snapshot().len();
    let mut all = Vec::new();
    for round in 0..3 {
        let (med, p99, rate) = commits(&s, 3000);
        println!(
            "no views, round {round}: median {med:.3} ms, p99 {p99:.3} ms, {rate:.0} commits/s"
        );
        all.push((med, p99, rate));
    }
    rounds("no-views", &mut all);
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
    let mut all = Vec::new();
    for round in 0..3 {
        let (med, p99, rate) = commits(&s, 3000);
        println!(
            "ring filled, round {round}: median {med:.3} ms, p99 {p99:.3} ms, {rate:.0} commits/s"
        );
        all.push((med, p99, rate));
    }
    rounds("live-query", &mut all);
    let head = s.snapshot().commit;
    check(
        "commit-path/commits",
        head == start + 18_000,
        format!("{} commits from {start}", head - start),
    );
    // every commit wrote two new quads, a team and an age of a new :w subject
    let written = sparql::query(
        s.snapshot(),
        &format!("{PREFIX}SELECT ?w ?m ?a WHERE {{ ?w :memberOf ?m ; :age ?a FILTER(STRSTARTS(STR(?w), \"http://ex/w\")) }}"),
        &QueryOptions {
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let added = s.snapshot().len() - quads;
    let vars = written.vars.clone();
    let rows = rows_of(
        &written
            .rows()
            .into_iter()
            .map(|r| (r, 1))
            .collect::<Vec<_>>(),
    );
    // the i-th commit of a round gave its subject the team t(i % 50) and the age i % 60
    let shaped = rows.iter().all(|r| {
        let f: Vec<&str> = r.split(' ').collect();
        f.len() == 3 && f[1].starts_with("<http://ex/t")
    });
    check_with(
        "commit-path/written",
        added == 36_000 && rows.len() == 18_000 && shaped,
        format!(
            "{added} quads added, {} subjects with a team and an age",
            rows.len()
        ),
        Some(answer_fingerprint(&vars, &rows)),
    );
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
        check(
            &format!("breakdown/{name}/applied"),
            c.applied == s.snapshot().commit,
            format!(
                "the circuit applied commit {} of {}",
                c.applied,
                s.snapshot().commit
            ),
        );
        // the circuit's own answer, outside the timed intervals
        let fresh = sparql::query(
            s.snapshot(),
            &format!("{PREFIX}{q}"),
            &QueryOptions {
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        let want = rows_of(&fresh.rows().into_iter().map(|r| (r, 1)).collect::<Vec<_>>());
        let got = rows_of(&c.rows());
        check_with(
            &format!("breakdown/{name}/answer"),
            c.columns() == fresh.vars && got == want,
            format!(
                "columns {:?} against {:?}, {} rows against {}",
                c.columns(),
                fresh.vars,
                got.len(),
                want.len()
            ),
            Some(answer_fingerprint(&fresh.vars, &want)),
        );
        let (r, o, a) = (pct(&mut read, 0.5), pct(&mut old, 0.5), pct(&mut adv, 0.5));
        println!("| {name} | {r:.1} | {o:.1} | {a:.1} |");
        metric(&format!("breakdown/{name}/read"), r, "µs", "lower");
        metric(&format!("breakdown/{name}/old"), o, "µs", "lower");
        metric(&format!("breakdown/{name}/advance"), a, "µs", "lower");
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
        let key = name.replace(' ', "-");
        check_view(
            &rt,
            &s,
            &format!("inferred/{key}"),
            "v",
            &text,
            QueryOptions {
                rdfs: inf.rdfs.clone(),
                default_graph_extra: inf.default_graph_extra.clone(),
                ..Default::default()
            },
        );
        let info = rt.get(&s, "v").unwrap();
        let qm = pct(&mut query, 0.5);
        let pm = pct(&mut passes, 0.5);
        let p95 = pct(&mut passes, 0.95);
        let skip = pct(&mut skips, 0.5);
        println!(
            "| {name} | {qm:.1} | {pm:.1} | {p95:.1} | {:.2} | {skip:.3} | {} | {} |",
            pm / qm,
            info.passes,
            info.skipped
        );
        metric(&format!("inferred/{key}/query"), qm, "ms", "lower");
        metric(&format!("inferred/{key}/round-median"), pm, "ms", "lower");
        metric(&format!("inferred/{key}/round-p95"), p95, "ms", "lower");
        metric(
            &format!("inferred/{key}/unrelated-commit"),
            skip,
            "ms",
            "lower",
        );
        rt.close(&s);
    }
}

/// The views of `path_pass_against_recompute` (spec F13 §4.3): a closure from one person
/// over the knows graph, which is one strongly connected component, the closure of a unit
/// hierarchy, the units under its root, and a count of the people under each unit.
const PATH_VIEWS: &[(&str, &str)] = &[
    ("reach-one", "SELECT ?q WHERE { :p0 :knows+ ?q }"),
    ("unit-ancestors", "SELECT ?u ?a WHERE { ?u :partOf+ ?a }"),
    ("under-root", "SELECT ?u WHERE { ?u :partOf* :u0 }"),
    (
        "people-under",
        "SELECT ?a (COUNT(?p) AS ?n) WHERE { ?p :inUnit ?u . ?u :partOf+ ?a } GROUP BY ?a",
    ),
];

/// The path workload's version, as [`WORKLOAD`] is the other tests'.
const PATH_WORKLOAD: &str = "incr_bench paths v1";

/// The units of the path workload: a tree of fan-out 4 over a tenth of the people, each
/// person in one unit.
fn units(n: usize) -> usize {
    (n / 10).max(16)
}

/// The people data with the unit tree and each person's unit.
fn path_data(n: usize) -> String {
    let mut s = data(n);
    let u = units(n);
    for i in 1..u {
        s.push_str(&format!(
            "<http://ex/u{i}> <http://ex/partOf> <http://ex/u{}> .\n",
            (i - 1) / 4
        ));
    }
    for i in 0..n {
        s.push_str(&format!(
            "<http://ex/p{i}> <http://ex/inUnit> <http://ex/u{}> .\n",
            (i * 31) % u
        ));
    }
    s
}

/// One commit of the path mix: a knows edge to a new person, the removal of a knows edge
/// of the component, a unit moved under another unit of a lower number (so the hierarchy
/// stays a tree), a new unit with a member, and an unrelated commit.
fn path_mix(s: &Store, i: usize, n: usize) {
    let f = fresh();
    let who = (f * 7919) % n;
    let u = units(n);
    let k = 1 + (f * 104_729) % (u - 1);
    let u = match i % 5 {
        0 => format!("INSERT DATA {{ :p{who} :knows :x{f} }}"),
        1 => format!("DELETE DATA {{ :p{who} :knows :p{} }}", (who * 7 + 1) % n),
        2 => format!(
            "DELETE WHERE {{ :u{k} :partOf ?x }} ; INSERT DATA {{ :u{k} :partOf :u{} }}",
            (f * 13) % k
        ),
        3 => format!("INSERT DATA {{ :nu{f} :partOf :u{k} . :x{f} :inUnit :nu{f} }}"),
        _ => format!("INSERT DATA {{ :new{f} :memberOf :t{} }}", who % 50),
    };
    upd(s, &u);
}

/// §4.3: each path view's pass against evaluating its query again, through 200 commits
/// of the path mix. A build without the fixed-point operator evaluates the paths again in
/// every pass that touches them, which `query ms` approximates.
#[test]
#[ignore]
fn path_pass_against_recompute() {
    let n = people();
    let t0 = Instant::now();
    let text = path_data(n);
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let views: Vec<&str> = PATH_VIEWS.iter().flat_map(|(n, q)| [*n, *q]).collect();
    for p in [text.as_str(), &views.join("\n"), PATH_WORKLOAD] {
        for b in p.bytes().chain([0]) {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    check_with(
        "paths/workload",
        true,
        format!("{n} people, {} units", units(n)),
        Some(format!("{h:016x}")),
    );
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        text.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    eprintln!(
        "loaded {n} people and {} units in {:.1?}",
        units(n),
        t0.elapsed()
    );
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    println!(
        "| path view | build ms | query ms | pass median ms | pass p95 ms | query / pass | state MiB |"
    );
    println!("|---|---|---|---|---|---|---|");
    for (name, q) in PATH_VIEWS {
        let text = format!("{PREFIX}{q}");
        let t = Instant::now();
        let mut def = ViewDef::table(text.clone());
        def.memory_bytes = 4 << 30;
        rt.put(&s, name, def).unwrap();
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
            path_mix(&s, i, n);
            let t = Instant::now();
            rt.round(&s);
            passes.push(t.elapsed().as_secs_f64() * 1e3);
        }
        check_view(
            &rt,
            &s,
            &format!("paths/{name}"),
            name,
            &text,
            QueryOptions::default(),
        );
        let info = rt.get(&s, name).unwrap();
        let qm = pct(&mut query, 0.5);
        let pm = pct(&mut passes, 0.5);
        let p95 = pct(&mut passes, 0.95);
        let state = info.state_bytes as f64 / (1 << 20) as f64;
        println!(
            "| {name} | {build:.1} | {qm:.1} | {pm:.3} | {p95:.3} | {:.0} | {state:.1} |",
            qm / pm,
        );
        metric(&format!("paths/{name}/build"), build, "ms", "lower");
        metric(&format!("paths/{name}/query"), qm, "ms", "lower");
        metric(&format!("paths/{name}/pass-median"), pm, "ms", "lower");
        metric(&format!("paths/{name}/pass-p95"), p95, "ms", "lower");
        metric(&format!("paths/{name}/state"), state, "MiB", "lower");
        rt.delete(name).unwrap();
        rt.round(&s);
    }
}

/// The views of item 418: the aggregates of spec F13 §4.2, `HAVING` and a correlated
/// `EXISTS`, over the people data. Each has one answer per snapshot: every team has at
/// most one label, so its `GROUP_CONCAT` and `SAMPLE` do not depend on the order of its
/// rows.
const AGG_VIEWS: &[(&str, &str)] = &[
    (
        "team-age-range",
        "SELECT ?t (MIN(?a) AS ?lo) (MAX(?a) AS ?hi) WHERE { ?p :memberOf ?t . ?p :age ?a } GROUP BY ?t",
    ),
    (
        "full-teams",
        "SELECT ?t (COUNT(?p) AS ?n) WHERE { ?p :memberOf ?t } GROUP BY ?t HAVING (COUNT(?p) >= 200)",
    ),
    (
        "team-labels",
        "SELECT ?t (GROUP_CONCAT(?l; SEPARATOR=\", \") AS ?ls) (SAMPLE(?l) AS ?one) WHERE { ?p :memberOf ?t OPTIONAL { ?t rdfs:label ?l } } GROUP BY ?t",
    ),
    (
        "knows-older",
        "SELECT ?p ?a WHERE { ?p :age ?a FILTER EXISTS { ?p :knows ?q . ?q :age ?b FILTER(?b > ?a) } }",
    ),
];

const AGG_WORKLOAD: &str = "incr_bench aggregates v1";

/// §4.2: a one- or two-quad commit's pass of the item 418 views against evaluating the
/// query again, through the commit mix of `pass_against_recompute`. An age insert
/// touches the `?q :age ?b` pattern of `knows-older`, which names no outer variable,
/// so that commit evaluates the condition again for every row, and the p95 shows it.
#[test]
#[ignore]
fn aggregate_pass_against_recompute() {
    let n = people();
    let t0 = Instant::now();
    let s = store(n, None);
    eprintln!("loaded {n} people in {:.1?}", t0.elapsed());
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let views: Vec<&str> = AGG_VIEWS.iter().flat_map(|(n, q)| [*n, *q]).collect();
    for p in [data(n).as_str(), &views.join("\n"), AGG_WORKLOAD] {
        for b in p.bytes().chain([0]) {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    check_with(
        "aggregates/workload",
        true,
        format!("{n} people"),
        Some(format!("{h:016x}")),
    );
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    println!(
        "| view | build ms | query ms | pass median ms | pass p95 ms | query / pass | state MiB |"
    );
    println!("|---|---|---|---|---|---|---|");
    for (name, q) in AGG_VIEWS {
        let text = format!("{PREFIX}{q}");
        let t = Instant::now();
        let mut def = ViewDef::table(text.clone());
        def.memory_bytes = 4 << 30;
        rt.put(&s, name, def).unwrap();
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
            mix(&s, i, n);
            let t = Instant::now();
            rt.round(&s);
            passes.push(t.elapsed().as_secs_f64() * 1e3);
        }
        check_view(
            &rt,
            &s,
            &format!("aggregates/{name}"),
            name,
            &text,
            QueryOptions::default(),
        );
        let info = rt.get(&s, name).unwrap();
        assert!(
            !info.plan.as_ref().is_some_and(|p| p.has_recompute()),
            "{name} has a recompute node: {:#?}",
            info.plan
        );
        let qm = pct(&mut query, 0.5);
        let pm = pct(&mut passes, 0.5);
        let p95 = pct(&mut passes, 0.95);
        let state = info.state_bytes as f64 / (1 << 20) as f64;
        println!(
            "| {name} | {build:.1} | {qm:.1} | {pm:.3} | {p95:.3} | {:.0} | {state:.1} |",
            qm / pm,
        );
        metric(&format!("aggregates/{name}/build"), build, "ms", "lower");
        metric(&format!("aggregates/{name}/query"), qm, "ms", "lower");
        metric(&format!("aggregates/{name}/pass-median"), pm, "ms", "lower");
        metric(&format!("aggregates/{name}/pass-p95"), p95, "ms", "lower");
        metric(&format!("aggregates/{name}/state"), state, "MiB", "lower");
        rt.delete(name).unwrap();
        rt.round(&s);
    }
}
