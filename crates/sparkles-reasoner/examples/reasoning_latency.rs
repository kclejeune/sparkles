//! Query latency of derivation on demand and of RDFS on read against materialized
//! inferences, on LUBM-style data with the 14 LUBM queries (ROADMAP item 472, spec F17).
//!
//! ```sh
//! mise run bench:reasoning-latency
//! cargo run --release -p sparkles-reasoner --example reasoning_latency -- 10 5 3 out.jsonl
//! ```
//!
//! The arguments are the universities (default 10, about 1.25 million triples), the
//! rounds (default 5), the timed runs of each query and mode in a round (default 3) and an
//! optional file that gets every measurement and answer check as a JSON line for
//! `scripts/bench-gate.py metrics`. `RAYON_NUM_THREADS` sets the threads.
//!
//! Two in-memory stores hold the ontology and the universities, one materialized with
//! the rdfs profile and one with owl-rl. For each profile a query runs in these modes:
//!
//! * `materialized` reads the store with its inferred graph.
//! * `demand` derives what the query needs from the profile's rules, with `nocache`, so
//!   that its demand tables are made again every time. The schema's table is kept.
//! * `demand-cached` repeats the demand query with its tables in the cache.
//! * `on-read` (rdfs only) matches the RDFS closure at query time with the schema of the
//!   default graph, as `--rdfs-graph default` does.
//!
//! The result cache is off in every mode. Each mode's answer is checked against the
//! materialized answer of its profile before anything is timed. Each mode and query gets
//! one untimed run first, then the rounds visit the modes in a rotated order.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::rdfs::{RdfsOnRead, schema_of_graph};
use sparkles_core::sparql::{QueryOptions, QueryResult, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, INFERRED_GRAPH, Inputs, Profile, ReasonOptions, materialize};
use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

#[path = "common/lubm.rs"]
mod lubm;

/// The derived facts a query may make, far above the server's default of a million, which
/// LUBM's broad queries pass under OWL RL from about ten universities on.
const MAX_DERIVED: usize = 1 << 32;

const QUERIES: &str = include_str!("../../../scripts/bench-lubm/queries.txt");

/// The `## name` blocks of the LUBM query file.
fn queries() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in QUERIES.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            out.push((name.trim().to_string(), String::new()));
        } else if let Some((_, text)) = out.last_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    out
}

fn store(universities: usize) -> Store {
    let s = Store::in_memory(StoreOptions {
        result_cache_bytes: 0,
        ..Default::default()
    });
    let mut data = String::new();
    for u in 0..universities {
        data.push_str(&lubm::university(u));
    }
    s.load(&[
        Source::from_bytes(lubm::ontology().into_bytes(), RdfFormat::Turtle, None),
        Source::from_bytes(data.into_bytes(), RdfFormat::NTriples, None),
    ])
    .unwrap();
    s
}

/// The row count and an order-independent digest of the query kind, the projected
/// variables in order and the rows, FNV-1a over the sorted rows, which stays the same
/// across builds so that a gate can compare it with a baseline. A row of another width
/// than the variables gives a digest of its own.
fn answer(r: &QueryResult) -> (usize, u64) {
    let head = format!("{:?} {}", r.kind, r.variables().join(" "));
    let mut rows: Vec<String> = r
        .rows()
        .unwrap()
        .iter()
        .map(|row| row.values().to_vec())
        .collect::<Vec<_>>()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".into(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    rows.sort_unstable();
    if r.rows()
        .unwrap()
        .iter()
        .map(|row| row.values().to_vec())
        .collect::<Vec<_>>()
        .iter()
        .any(|row| row.len() != r.variables().len())
    {
        rows.push("rows of another width than the variables".into());
    }
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for row in std::iter::once(&head).chain(&rows) {
        for b in row.bytes().chain(*b"\n") {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    (rows.len(), h)
}

struct Mode {
    profile: &'static str,
    name: &'static str,
    store: usize,
    options: QueryOptions,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut num = |default: usize| {
        args.next()
            .map(|a| a.parse().expect("a number"))
            .unwrap_or(default)
    };
    let universities = num(10);
    let rounds = num(5);
    let reps = num(3);
    let json = std::env::args().nth(4);
    let mut out = json.map(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap()
    });
    let mut line = |v: serde_json::Value| {
        if let Some(f) = &mut out {
            writeln!(f, "{v}").unwrap();
        }
    };
    let threads = rayon::current_num_threads();
    println!("LUBM-style data of {universities} universities, {threads} threads");

    let mut stores = Vec::new();
    for profile in [Profile::Rdfs, Profile::OwlRl] {
        let t = Instant::now();
        let s = store(universities);
        let loaded = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let r = materialize(&s, &profile, &ReasonOptions::default()).unwrap();
        println!(
            "{}: {} triples loaded in {loaded:.1} s, {} inferred in {:.1} s",
            profile.name(),
            s.snapshot().len(),
            r.inferred,
            t.elapsed().as_secs_f64()
        );
        line(serde_json::json!({
            "key": format!("{}/materialize", profile.name()),
            "value": t.elapsed().as_secs_f64() * 1e3, "unit": "ms", "better": "lower",
        }));
        line(serde_json::json!({
            "check": format!("{}/materialize/inferred", profile.name()), "ok": r.inferred > 0,
            "detail": format!("{} inferred triples", r.inferred),
            "fingerprint": r.inferred.to_string(),
        }));
        stores.push(s);
    }
    let base = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let demand = |p: Profile| {
        QueryReasoner::new(p, &Extras::default(), Inputs::default())
            .unwrap()
            .with_max_derived(MAX_DERIVED)
            .provider()
    };
    let rdfs = Arc::new(RdfsOnRead::fixed(
        schema_of_graph(&stores[0].snapshot(), None).unwrap(),
    ));
    let rdfs_demand = demand(Profile::Rdfs);
    let owl_demand = demand(Profile::OwlRl);
    let modes = [
        Mode {
            profile: "rdfs",
            name: "materialized",
            store: 0,
            options: QueryOptions {
                default_graph_extra: vec![INFERRED_GRAPH.to_string()],
                ..base.clone()
            },
        },
        Mode {
            profile: "rdfs",
            name: "demand",
            store: 0,
            options: QueryOptions {
                reasoning: Some(rdfs_demand.clone()),
                ..base.clone()
            },
        },
        Mode {
            profile: "rdfs",
            name: "demand-cached",
            store: 0,
            options: QueryOptions {
                reasoning: Some(rdfs_demand),
                no_cache: false,
                ..base.clone()
            },
        },
        Mode {
            profile: "rdfs",
            name: "on-read",
            store: 0,
            options: QueryOptions {
                rdfs: Some(rdfs),
                ..base.clone()
            },
        },
        Mode {
            profile: "owl-rl",
            name: "materialized",
            store: 1,
            options: QueryOptions {
                default_graph_extra: vec![INFERRED_GRAPH.to_string()],
                ..base.clone()
            },
        },
        Mode {
            profile: "owl-rl",
            name: "demand",
            store: 1,
            options: QueryOptions {
                reasoning: Some(owl_demand.clone()),
                ..base.clone()
            },
        },
        Mode {
            profile: "owl-rl",
            name: "demand-cached",
            store: 1,
            options: QueryOptions {
                reasoning: Some(owl_demand),
                no_cache: false,
                ..base.clone()
            },
        },
    ];
    let qs = queries();
    assert_eq!(qs.len(), 14, "the LUBM query file has 14 queries");

    // the answers, and one untimed run of each mode and query; a query that fails is a
    // failed check and is not timed
    let mut reference = std::collections::HashMap::new();
    let mut failed = std::collections::HashSet::new();
    for (mi, m) in modes.iter().enumerate() {
        for (qi, (q, text)) in qs.iter().enumerate() {
            let r = match query(stores[m.store].snapshot(), text, &m.options) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{}/{}/{q}: {e}", m.profile, m.name);
                    line(serde_json::json!({
                        "check": format!("{}/{}/{q}/answer", m.profile, m.name), "ok": false,
                        "detail": e.to_string(),
                    }));
                    failed.insert((mi, qi));
                    continue;
                }
            };
            let a = answer(&r);
            if m.name == "materialized" {
                reference.insert((m.profile, q.as_str()), a);
            }
            let Some(&want) = reference.get(&(m.profile, q.as_str())) else {
                failed.insert((mi, qi));
                continue;
            };
            let ok = a == want;
            if !ok {
                eprintln!(
                    "{}/{}/{q}: {} rows, materialized {} rows",
                    m.profile, m.name, a.0, want.0
                );
            }
            line(serde_json::json!({
                "check": format!("{}/{}/{q}/answer", m.profile, m.name), "ok": ok,
                "detail": format!("{} rows, materialized {} rows", a.0, want.0),
                "fingerprint": format!("{} rows, {:016x}", a.0, a.1),
            }));
        }
    }

    let mut samples: Vec<Vec<Vec<f64>>> = vec![vec![Vec::new(); qs.len()]; modes.len()];
    // the timed runs whose answer differed from the materialized one, checked after the
    // timer stopped, so that a cached path that goes wrong cannot pass as a faster one
    let mut wrong: Vec<Vec<usize>> = vec![vec![0; qs.len()]; modes.len()];
    for round in 0..rounds {
        for (qi, (q, text)) in qs.iter().enumerate() {
            for k in 0..modes.len() {
                let mi = (k + round + qi) % modes.len();
                let m = &modes[mi];
                if failed.contains(&(mi, qi)) {
                    continue;
                }
                for _ in 0..reps {
                    let t = Instant::now();
                    let r = query(stores[m.store].snapshot(), text, &m.options).unwrap();
                    std::hint::black_box(r.len());
                    samples[mi][qi].push(t.elapsed().as_secs_f64() * 1e3);
                    if answer(&r) != reference[&(m.profile, q.as_str())] {
                        wrong[mi][qi] += 1;
                    }
                }
            }
        }
        let (tables, bytes, hits, misses) = sparkles_reasoner::tables::stats();
        eprintln!(
            "round {} of {rounds} done; derived-table cache: {tables} tables, {bytes} bytes, {hits} hits, {misses} misses",
            round + 1
        );
    }

    for (mi, m) in modes.iter().enumerate() {
        for (qi, (q, _)) in qs.iter().enumerate() {
            if failed.contains(&(mi, qi)) {
                continue;
            }
            let n = samples[mi][qi].len();
            line(serde_json::json!({
                "check": format!("{}/{}/{q}/timed-answers", m.profile, m.name),
                "ok": wrong[mi][qi] == 0,
                "detail": format!("{} of {n} timed runs returned another answer", wrong[mi][qi]),
            }));
        }
    }

    let median = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    for profile in ["rdfs", "owl-rl"] {
        let ms: Vec<usize> = (0..modes.len())
            .filter(|&i| modes[i].profile == profile)
            .collect();
        println!();
        println!(
            "| {profile} | {} |",
            ms.iter()
                .map(|&i| format!("{} ms", modes[i].name))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        println!("|---|{}", "--:|".repeat(ms.len()));
        for (qi, (q, _)) in qs.iter().enumerate() {
            let cells: Vec<String> = ms
                .iter()
                .map(|&i| match samples[i][qi].is_empty() {
                    true => "failed".to_string(),
                    false => format!("{:.3}", median(&samples[i][qi])),
                })
                .collect();
            println!("| {q} | {} |", cells.join(" | "));
            for &i in &ms {
                let s = &samples[i][qi];
                if s.is_empty() {
                    continue;
                }
                line(serde_json::json!({
                    "key": format!("{profile}/{}/{q}", modes[i].name),
                    "value": median(s), "unit": "ms", "better": "lower",
                    "samples": s.iter().map(|x| (x * 1e4).round() / 1e4).collect::<Vec<_>>(),
                }));
            }
        }
    }
}
