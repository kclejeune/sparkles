//! Latency of `reasoning=auto` against materialized reasoning and derivation on demand
//! (spec F17 §4.3 and §13) on LUBM-style data.
//!
//! ```text
//! auto_latency [universities] [rounds] [reps] [metrics.jsonl]
//! ```
//!
//! The arguments are the universities (default 10), the rounds (default 5), the timed
//! runs of each query and mode in a round (default 3) and an optional file that receives
//! one JSON line per metric, as `scripts/bench-gate.py metrics` reads them.
//!
//! For each of the `rdfs` and `owl-rl` profiles the harness builds two persistent
//! datasets, materialized with the profile. One stays fresh. The other receives a triple
//! outside every query's cone after the run, so its materialization is stale while its
//! answers do not change. Each of the 14 LUBM queries then runs in these modes:
//!
//! - `materialized`: the inferred graph, on the fresh dataset;
//! - `demand` and `demand-cached`: derivation on demand with `nocache` on and off, on the
//!   fresh dataset, whose inferred graph the modes do not read;
//! - `auto-fresh`: `auto` on the fresh dataset, which reads the inferred graph;
//! - `auto-stale` and `auto-stale-cached`: `auto` on the stale dataset, which derives.
//!
//! With `AUTO_LATENCY_RAW` set, a `demand-raw` mode adds the query-time reasoner made
//! without the dataset's record, as `crates/sparkles-reasoner/examples/
//! reasoning_latency.rs` makes it, and each checked run prints its derivation counters.
//!
//! The result cache is off in every mode. Each mode's answer is checked against the
//! materialized answer of its profile before anything is timed, and the harness checks
//! that `auto-fresh` read every pattern and that `auto-stale` derived. Each mode and
//! query gets one untimed run first, then the rounds visit the modes in a rotated order.
//! The table gives the median of the timed runs in milliseconds.

#![cfg(feature = "reasoning")]

#[path = "../../sparkles-reasoner/examples/common/lubm.rs"]
mod lubm;

use sparkles::Dataset;
use sparkles::io::{RdfFormat, Source};
use sparkles::reasoning::reasoner::Profile;
use sparkles::reasoning::{Demand, ReasonRequest, ReasoningMode};
use sparkles::sparql::{QueryOptions, QueryResult};
use std::io::Write as _;
use std::time::Instant;

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

fn dataset(dir: &std::path::Path, universities: usize, profile: &Profile, stale: bool) -> Dataset {
    let ds = Dataset::open(dir).unwrap();
    let mut data = String::new();
    for u in 0..universities {
        data.push_str(&lubm::university(u));
    }
    ds.load_sources_receipt(vec![
        Source::from_bytes(lubm::ontology().into_bytes(), RdfFormat::Turtle, None),
        Source::from_bytes(data.into_bytes(), RdfFormat::NTriples, None),
    ])
    .unwrap();
    let t = Instant::now();
    let r = ds
        .reasoning()
        .run(&ReasonRequest {
            profile: profile.clone(),
            ..Default::default()
        })
        .unwrap();
    println!(
        "{}{}: {} triples, {} inferred in {:.1} s",
        profile.name(),
        if stale { " (stale)" } else { "" },
        ds.snapshot().len(),
        r.report.inferred,
        t.elapsed().as_secs_f64()
    );
    if stale {
        // a predicate no rule and no query reads: the materialization is stale, the
        // answers are not
        ds.update(
            "INSERT DATA { <http://ex.org/harness> <http://ex.org/touched> <http://ex.org/it> }",
        )
        .unwrap();
    }
    ds.clear_cache();
    ds
}

/// The row count and an order-independent digest of the rows.
fn answer(r: &QueryResult) -> (usize, u64) {
    let mut rows: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".into(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    if r.kind == sparkles::sparql::QueryKind::Ask {
        rows = vec![r.boolean.to_string()];
    }
    rows.sort();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for row in &rows {
        for b in row.bytes().chain(*b"\n") {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    (rows.len(), h)
}

struct Mode {
    profile: &'static str,
    name: &'static str,
    dataset: usize,
    options: QueryOptions,
    /// what `auto` must have done: `Some(true)` read every pattern, `Some(false)`
    /// derived at least one
    auto_read: Option<bool>,
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
    println!("LUBM-style data of {universities} universities");
    let dir = tempfile::tempdir().unwrap();
    let mut datasets = Vec::new();
    for profile in [Profile::Rdfs, Profile::OwlRl] {
        for stale in [false, true] {
            let name = format!(
                "{}-{}",
                profile.name(),
                if stale { "stale" } else { "fresh" }
            );
            datasets.push(dataset(
                &dir.path().join(name),
                universities,
                &profile,
                stale,
            ));
        }
    }
    let base = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let view = |ds: &Dataset, mode: ReasoningMode, no_cache: bool| {
        let mut o = ds.inference_view(mode).unwrap().query_options();
        o.no_cache = no_cache;
        let _ = &base;
        o
    };
    let demand = |p: &Profile| Demand {
        profile: Some(p.clone()),
        max_derived: Some(usize::MAX),
    };
    let mut modes = Vec::new();
    for (pi, profile) in [Profile::Rdfs, Profile::OwlRl].into_iter().enumerate() {
        let (fresh, stale) = (pi * 2, pi * 2 + 1);
        let name: &'static str = if pi == 0 { "rdfs" } else { "owl-rl" };
        modes.push(Mode {
            profile: name,
            name: "materialized",
            dataset: fresh,
            options: view(&datasets[fresh], ReasoningMode::Materialized, true),
            auto_read: None,
        });
        modes.push(Mode {
            profile: name,
            name: "demand",
            dataset: fresh,
            options: view(
                &datasets[fresh],
                ReasoningMode::Demand(demand(&profile)),
                true,
            ),
            auto_read: None,
        });
        if std::env::var_os("AUTO_LATENCY_RAW").is_some() {
            // the reasoner without the dataset's record, as the Phase 1 harness makes it
            let raw = sparkles_reasoner::query::QueryReasoner::new(
                profile.clone(),
                &Default::default(),
                Default::default(),
            )
            .unwrap()
            .with_max_derived(usize::MAX)
            .provider();
            let mut o = view(&datasets[fresh], ReasoningMode::Off, true);
            o.reasoning = Some(raw);
            o.default_graph_extra.clear();
            modes.push(Mode {
                profile: name,
                name: "demand-raw",
                dataset: fresh,
                options: o,
                auto_read: None,
            });
        }
        modes.push(Mode {
            profile: name,
            name: "demand-cached",
            dataset: fresh,
            options: view(
                &datasets[fresh],
                ReasoningMode::Demand(demand(&profile)),
                false,
            ),
            auto_read: None,
        });
        modes.push(Mode {
            profile: name,
            name: "auto-fresh",
            dataset: fresh,
            options: view(
                &datasets[fresh],
                ReasoningMode::Auto(demand(&profile)),
                true,
            ),
            auto_read: Some(true),
        });
        modes.push(Mode {
            profile: name,
            name: "auto-stale",
            dataset: stale,
            options: view(
                &datasets[stale],
                ReasoningMode::Auto(demand(&profile)),
                true,
            ),
            auto_read: Some(false),
        });
        modes.push(Mode {
            profile: name,
            name: "auto-stale-cached",
            dataset: stale,
            options: view(
                &datasets[stale],
                ReasoningMode::Auto(demand(&profile)),
                false,
            ),
            auto_read: Some(false),
        });
    }
    let qs = queries();
    assert_eq!(qs.len(), 14, "the LUBM query file has 14 queries");

    // the answers, and one untimed run of each mode and query
    let mut expected: Vec<Vec<Option<(usize, u64)>>> = vec![vec![None; qs.len()]; 2];
    let mut ok = vec![vec![true; qs.len()]; modes.len()];
    let mut decisions: Vec<Vec<String>> = vec![vec![String::new(); qs.len()]; modes.len()];
    for (mi, m) in modes.iter().enumerate() {
        let pi = usize::from(m.profile == "owl-rl");
        for (qi, (q, text)) in qs.iter().enumerate() {
            let r = match datasets[m.dataset].query_with(text, &m.options) {
                Ok(r) => r,
                Err(e) => {
                    println!("{}/{}/{q}: {e}", m.profile, m.name);
                    ok[mi][qi] = false;
                    continue;
                }
            };
            let a = answer(&r);
            match &expected[pi][qi] {
                None if m.name == "materialized" => expected[pi][qi] = Some(a),
                Some(e) if *e != a => {
                    println!(
                        "{}/{}/{q}: {} rows, materialized gave {}",
                        m.profile, m.name, a.0, e.0
                    );
                    ok[mi][qi] = false;
                }
                _ => {}
            }
            if let Some(applied) = &r.ctx.reasoning {
                let d = format!(
                    "read={} derived={} tables={} cached={}",
                    applied.read,
                    applied.report.derived,
                    applied.report.tables,
                    applied.report.cached
                );
                if let Some(read_all) = m.auto_read {
                    let did = if read_all {
                        applied.report.tables == 0
                    } else {
                        applied.read == 0
                    };
                    if !did {
                        println!("{}/{}/{q}: auto chose otherwise: {d}", m.profile, m.name);
                        ok[mi][qi] = false;
                    }
                }
                if std::env::var_os("AUTO_LATENCY_RAW").is_some() {
                    println!(
                        "{}/{}/{q}: {d}; warnings {:?}",
                        m.profile, m.name, applied.report.warnings
                    );
                }
                decisions[mi][qi] = d;
            }
        }
    }

    let mut samples: Vec<Vec<Vec<f64>>> = vec![vec![Vec::new(); qs.len()]; modes.len()];
    for round in 0..rounds {
        for (qi, (_, text)) in qs.iter().enumerate() {
            for k in 0..modes.len() {
                let mi = (k + round + qi) % modes.len();
                let m = &modes[mi];
                if !ok[mi][qi] {
                    continue;
                }
                for _ in 0..reps {
                    let t = Instant::now();
                    let r = datasets[m.dataset].query_with(text, &m.options);
                    let ms = t.elapsed().as_secs_f64() * 1e3;
                    if r.is_err() {
                        ok[mi][qi] = false;
                        break;
                    }
                    samples[mi][qi].push(ms);
                }
            }
        }
    }
    let median = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(f64::total_cmp);
        if v.is_empty() {
            f64::NAN
        } else {
            v[v.len() / 2]
        }
    };
    for profile in ["rdfs", "owl-rl"] {
        let ms: Vec<usize> = (0..modes.len())
            .filter(|&i| modes[i].profile == profile)
            .collect();
        println!("\n### {profile}\n");
        println!(
            "| Query | {} |",
            ms.iter()
                .map(|&i| format!("{} ms", modes[i].name))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        println!("|---|{}|", "--:|".repeat(ms.len()));
        for (qi, (q, _)) in qs.iter().enumerate() {
            let cells: Vec<String> = ms
                .iter()
                .map(|&i| {
                    if ok[i][qi] {
                        format!("{:.3}", median(&samples[i][qi]))
                    } else {
                        "failed".to_string()
                    }
                })
                .collect();
            println!("| {q} | {} |", cells.join(" | "));
            for &i in &ms {
                line(serde_json::json!({
                    "key": format!("{profile}/{}/{q}", modes[i].name),
                    "value": median(&samples[i][qi]), "unit": "ms", "better": "lower",
                    "detail": decisions[i][qi],
                }));
                line(serde_json::json!({
                    "check": format!("{profile}/{}/{q}/answer", modes[i].name), "ok": ok[i][qi],
                    "detail": decisions[i][qi],
                }));
            }
        }
        println!();
        for &i in &ms {
            if modes[i].auto_read.is_some() {
                println!(
                    "{}: {}",
                    modes[i].name,
                    (0..qs.len())
                        .map(|qi| format!("{} {}", qs[qi].0, decisions[i][qi]))
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
        }
    }
}
