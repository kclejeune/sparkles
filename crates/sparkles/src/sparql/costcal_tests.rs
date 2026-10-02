//! Calibration of the planner's cost model on a loaded store. These tests are ignored;
//! they time operators and whole queries and print tab-separated lines for fitting:
//!
//! ```text
//! SPARKLES_CAL_DB=<store> cargo test --release -p sparkles costcal -- --ignored --nocapture
//! ```
//!
//! `cal_queries` runs every `.rq` file of `SPARKLES_CAL_QUERIES` with batched joins on and
//! off and prints the median execution time of each and the self time of every operator.
//! `cal_probes` reads one pattern for `K` keys of its subjects, spread at random or
//! contiguous, by a forced index join and by the plan chosen without batched joins.
//! `cal_stars` does the same for a star of three patterns, read by subject runs or per
//! pattern. `cal_drives` joins selective patterns, sorted on their subject, with one
//! more pattern each. All three also run the plan the planner chooses (`auto`).
//! `cal_tables` times hash joins, merge joins and sorts of generated tables.
//! `SPARKLES_CAL_MODES` limits the modes that run, and `SPARKLES_CAL_RUNS` sets the timed
//! runs (default 7) after one warm-up.
//!
//! The server allocates with mimalloc, and test binaries with the system allocator. Run
//! them with mimalloc preloaded (`LD_PRELOAD=…/libmimalloc.so`) to time what the server
//! does: with glibc's allocator, scans that fill large new columns run up to four times
//! slower depending on what the plan freed before them.

use super::indexjoin::{FORCE_INDEX_JOIN, FORCE_WALK};
use super::*;
use crate::store::{Store, StoreOptions};
use std::path::Path;

const PREFIXES: &str = "PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> \
     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

fn store() -> Store {
    let p = std::env::var_os("SPARKLES_CAL_DB").expect("SPARKLES_CAL_DB names a store");
    Store::open(Path::new(&p), StoreOptions::default()).expect("open the store")
}

fn runs() -> usize {
    std::env::var("SPARKLES_CAL_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7)
}

fn opts(batched: bool) -> QueryOptions {
    QueryOptions {
        optimizations: Some(Optimizations {
            batched_join: batched,
            ..Optimizations::ALL
        }),
        no_cache: true,
        ..Default::default()
    }
}

/// One operator of an executed plan: its path in the tree, name, description, estimates,
/// rows, self time (its time less its children's) and its children's rows.
struct Op {
    path: String,
    op: String,
    desc: String,
    est_rows: f64,
    est_cost: f64,
    rows: i64,
    self_ms: f64,
    child_rows: Vec<i64>,
    counters: String,
}

fn flatten(info: &PlanInfo, path: String, out: &mut Vec<Op>) {
    let kids: f64 = info.children.iter().map(|c| c.time_ms).sum();
    out.push(Op {
        path: path.clone(),
        op: info.operator.clone(),
        desc: info.description.replace(['\t', '\n'], " "),
        est_rows: info.estimated_rows,
        est_cost: info.estimated_cost,
        rows: info.actual_rows,
        self_ms: (info.time_ms - kids).max(0.0),
        child_rows: info.children.iter().map(|c| c.actual_rows).collect(),
        counters: info
            .counters
            .as_ref()
            .map(|c| serde_json::Value::Object(c.clone()).to_string())
            .unwrap_or_default(),
    });
    for (i, c) in info.children.iter().enumerate() {
        flatten(c, format!("{path}.{i}"), out);
    }
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(f64::total_cmp);
    xs[xs.len() / 2]
}

/// Median execution time of `q` and the operators of its plan with their median self
/// times, forcing index joins (and a star's reading) when `force` says so.
fn measure(
    snap: &Arc<crate::store::Snapshot>,
    q: &str,
    o: &QueryOptions,
    force: Option<Option<bool>>,
) -> (f64, f64, usize, Vec<Op>) {
    if let Some(walk) = force {
        FORCE_INDEX_JOIN.with(|f| f.set(true));
        FORCE_WALK.with(|f| f.set(walk));
    }
    let parsed = parse_query(q, None, &[]).unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut exec = Vec::new();
    let mut plan = Vec::new();
    let mut per_op: Vec<Vec<f64>> = Vec::new();
    let mut ops = Vec::new();
    let mut rows = 0;
    for i in 0..=runs() {
        let r = execute_query(snap.clone(), &parsed, o, 0.0).unwrap_or_else(|e| panic!("{q}: {e}"));
        if i == 0 {
            continue;
        }
        exec.push(r.timing.exec_ms);
        plan.push(r.timing.plan_ms);
        rows = r.table.len();
        let mut f = Vec::new();
        flatten(&r.plan, "0".into(), &mut f);
        if per_op.is_empty() {
            per_op = vec![Vec::new(); f.len()];
        }
        for (k, op) in f.iter().enumerate() {
            if let Some(v) = per_op.get_mut(k) {
                v.push(op.self_ms);
            }
        }
        ops = f;
    }
    FORCE_INDEX_JOIN.with(|f| f.set(false));
    FORCE_WALK.with(|f| f.set(None));
    for (op, ts) in ops.iter_mut().zip(per_op) {
        op.self_ms = median(ts);
    }
    (median(exec), median(plan), rows, ops)
}

fn print_ops(tag: &str, ops: &[Op]) {
    for o in ops {
        println!(
            "OP\t{tag}\t{}\t{}\t{:.0}\t{:.0}\t{}\t{:.4}\t{}\t{}\t{}",
            o.path,
            o.op,
            o.est_rows,
            o.est_cost,
            o.rows,
            o.self_ms,
            o.child_rows
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(","),
            o.counters,
            o.desc
        );
    }
}

#[test]
#[ignore]
fn cal_queries() {
    let s = store();
    let snap = s.snapshot();
    let dir =
        std::env::var("SPARKLES_CAL_QUERIES").expect("SPARKLES_CAL_QUERIES names a directory");
    let only = std::env::var("SPARKLES_CAL_ONLY").ok();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rq"))
        .collect();
    files.sort();
    for f in files {
        let name = f.file_stem().unwrap().to_string_lossy().to_string();
        if name.starts_with('_')
            || only
                .as_ref()
                .is_some_and(|o| !o.split(',').any(|x| x == name))
        {
            continue;
        }
        let q = std::fs::read_to_string(&f).unwrap();
        for (mode, batched) in [("on", true), ("off", false)] {
            if !runs_mode(mode) {
                continue;
            }
            let (exec, plan, rows, ops) = measure(&snap, &q, &opts(batched), None);
            println!(
                "Q\t{name}\t{mode}\t{exec:.3}\t{plan:.3}\t{rows}\t{:.0}\t{}",
                ops[0].est_cost,
                ops.iter()
                    .filter(|o| o.op.contains("Join") || o.op.contains("Star"))
                    .map(|o| format!("{} {}", o.op, o.desc))
                    .collect::<Vec<_>>()
                    .join(" / ")
            );
            print_ops(&format!("{name}\t{mode}"), &ops);
        }
    }
}

/// The subjects of `pred` in id order, as IRIs.
fn subjects(snap: &Arc<crate::store::Snapshot>, pred: &str) -> Vec<String> {
    let q = format!("{PREFIXES}SELECT DISTINCT ?s WHERE {{ ?s {pred} ?o }} ORDER BY ?s");
    let r = query(snap.clone(), &q, &opts(true)).unwrap();
    let mut ids: Vec<(Id, String)> = r.table.cols[0]
        .iter()
        .zip(r.rows())
        .filter_map(|(id, row)| match &row[0] {
            Some(oxrdf::Term::NamedNode(n)) => Some((*id, format!("<{}>", n.as_str()))),
            _ => None,
        })
        .collect();
    ids.sort();
    ids.into_iter().map(|x| x.1).collect()
}

/// `k` of `all`: a random sample (in id order) or a contiguous run at a random offset.
fn pick(all: &[String], k: usize, dense: bool, seed: u64) -> Vec<String> {
    let mut x = seed | 1;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let k = k.min(all.len());
    if dense {
        let start = (next() as usize) % (all.len() - k + 1);
        return all[start..start + k].to_vec();
    }
    // Floyd's sample, then id order
    let mut chosen = rustc_hash::FxHashSet::default();
    for j in all.len() - k..all.len() {
        let t = (next() as usize) % (j + 1);
        if !chosen.insert(t) {
            chosen.insert(j);
        }
    }
    let mut idx: Vec<usize> = chosen.into_iter().collect();
    idx.sort_unstable();
    idx.into_iter().map(|i| all[i].clone()).collect()
}

/// Whether `mode` runs: `SPARKLES_CAL_MODES` lists the modes to run (all by default).
fn runs_mode(mode: &str) -> bool {
    std::env::var("SPARKLES_CAL_MODES").map_or(true, |m| m.split(',').any(|x| x == mode))
}

/// Measure `q` in `mode` and print its total and operators.
fn report(
    snap: &Arc<crate::store::Snapshot>,
    tag: &str,
    mode: &str,
    q: &str,
    batched: bool,
    force: Option<Option<bool>>,
) {
    if !runs_mode(mode) {
        return;
    }
    let (t, _, rows, ops) = measure(snap, q, &opts(batched), force);
    println!("P\t{tag}\t{mode}\t{t:.3}\t{rows}");
    print_ops(&format!("{tag}\t{mode}"), &ops);
}

fn ks() -> Vec<usize> {
    std::env::var("SPARKLES_CAL_KS")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_else(|| vec![10, 100, 1000, 3000, 10_000, 30_000, 100_000, 300_000])
}

#[test]
#[ignore]
fn cal_probes() {
    let s = store();
    let snap = s.snapshot();
    let people = subjects(&snap, "foaf:name");
    for (pred, obj) in [
        ("foaf:name", "?o"),
        ("foaf:age", "?o"),
        ("foaf:knows", "?o"),
        ("rdf:type", "?o"),
        ("rdf:type", "ex:Researcher"),
        ("ex:worksFor", "?o"),
        ("ex:authorOf", "?o"),
    ] {
        for k in ks() {
            if k > people.len() {
                continue;
            }
            for dense in [false, true] {
                let keys = pick(&people, k, dense, 0x9e37_79b9 ^ k as u64);
                let q = format!(
                    "{PREFIXES}SELECT * WHERE {{ VALUES ?s {{ {} }} ?s {pred} {obj} }}",
                    keys.join(" ")
                );
                let tag = format!(
                    "{pred} {obj}\t{k}\t{}",
                    if dense { "dense" } else { "random" }
                );
                report(&snap, &tag, "index", &q, true, Some(None));
                report(&snap, &tag, "off", &q, false, None);
                report(&snap, &tag, "auto", &q, true, None);
            }
        }
    }
}

#[test]
#[ignore]
fn cal_stars() {
    let s = store();
    let snap = s.snapshot();
    let people = subjects(&snap, "foaf:name");
    for k in ks() {
        if k > people.len() {
            continue;
        }
        for dense in [false, true] {
            let keys = pick(&people, k, dense, 0x51_7cc1 ^ k as u64);
            let q = format!(
                "{PREFIXES}SELECT * WHERE {{ VALUES ?s {{ {} }} ?s foaf:name ?n ; foaf:age ?a ; a ?t }}",
                keys.join(" ")
            );
            let tag = format!("star3\t{k}\t{}", if dense { "dense" } else { "random" });
            report(&snap, &tag, "walk", &q, true, Some(Some(true)));
            report(&snap, &tag, "per", &q, true, Some(Some(false)));
            report(&snap, &tag, "off", &q, false, None);
            report(&snap, &tag, "auto", &q, true, None);
        }
    }
}

/// Selective patterns sorted on their subject (`?s`), joined with one more pattern on
/// it: the index join against the merge join the planner would otherwise choose.
#[test]
#[ignore]
fn cal_drives() {
    let s = store();
    let snap = s.snapshot();
    for drive in [
        "?s ex:worksFor <http://example.org/org/7>",
        "?s foaf:age 30",
        "?s a ex:Manager",
        "?s a ex:Researcher",
    ] {
        for pred in [
            "foaf:name",
            "foaf:knows",
            "ex:salary",
            "ex:authorOf",
            "ex:worksFor",
        ] {
            if drive.contains(pred) {
                continue;
            }
            let q = format!("{PREFIXES}SELECT * WHERE {{ {drive} . ?s {pred} ?o }}");
            let tag = format!("{drive}\t{pred}\t-");
            report(&snap, &tag, "index", &q, true, Some(None));
            report(&snap, &tag, "off", &q, false, None);
            report(&snap, &tag, "auto", &q, true, None);
        }
    }
}

/// Hash joins, merge joins and sorts of generated tables: a probe side of `p` rows with
/// distinct keys and a build side of `b` of those keys (so `b` output rows), timed apart
/// from any scan.
#[test]
#[ignore]
fn cal_tables() {
    let s = Store::in_memory(StoreOptions::default());
    let ctx = Ctx::new(s.snapshot());
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let time = |f: &mut dyn FnMut()| {
        let mut ts: Vec<f64> = (0..=runs())
            .map(|_| {
                let t0 = std::time::Instant::now();
                f();
                t0.elapsed().as_secs_f64() * 1000.0
            })
            .skip(1)
            .collect();
        ts.sort_by(f64::total_cmp);
        ts[ts.len() / 2]
    };
    for p in [100_000usize, 1_000_000] {
        let keys: Vec<Id> = (0..p).map(|_| Id(next() >> 8 | 1)).collect();
        let mut probe = Table::new(vec![0, 1]);
        probe.cols[0] = keys.clone();
        probe.cols[1] = (0..p).map(|i| Id(i as u64 + 1)).collect();
        probe.len = p;
        let mut sorted_probe = probe.clone();
        sorted_probe.sort_by_vars(&[0]);
        for b in [1_000usize, 10_000, 100_000, 1_000_000] {
            if b > p {
                continue;
            }
            let mut build = Table::new(vec![0, 2]);
            build.cols[0] = (0..b).map(|_| keys[next() as usize % p]).collect();
            build.cols[1] = (0..b).map(|i| Id(i as u64 + 1)).collect();
            build.len = b;
            let hash = time(&mut || {
                super::exec::join_tables(&ctx, &probe, &build, &[0], false).unwrap();
            });
            let mut sorted_build = build.clone();
            sorted_build.sort_by_vars(&[0]);
            let merge = time(&mut || {
                super::exec::join_tables(&ctx, &sorted_probe, &sorted_build, &[0], true).unwrap();
            });
            let sort = time(&mut || {
                let mut t = build.clone();
                t.sort_by_vars(&[0]);
            });
            let copy = time(&mut || {
                let _t = build.clone();
            });
            let out = super::exec::join_tables(&ctx, &probe, &build, &[0], false)
                .unwrap()
                .len();
            println!("T\thash\t{b}\t{p}\t{out}\t{hash:.4}");
            println!("T\tmerge\t{b}\t{p}\t{out}\t{merge:.4}");
            println!("T\tsort\t{b}\t0\t0\t{:.4}", (sort - copy).max(0.0));
        }
    }
}
