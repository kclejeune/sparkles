//! The first `reasoning=demand` query after a commit, on LUBM-style data with the 14
//! LUBM queries (spec F17 §6.2).
//!
//! ```sh
//! cargo run --release -p sparkles-reasoner --example table_reuse -- 10 5
//! ```
//!
//! The arguments are the universities (default 10) and the rounds (default 5). For the
//! rdfs and owl-rl profiles, one in-memory store holds the ontology and the universities.
//! Each query runs once to fill the cache. Then every round, before each query, commits a
//! triple that no rule reads or gives, a literal of a predicate the ontology does not
//! name, and times the query, and then does the same with a triple that the rules read,
//! the type of a new graduate student. Every answer is checked against the same state
//! read as a view that no cache serves, so that the check derives from nothing. The output gives the median and range of each query, profile
//! and kind of commit, in milliseconds, with the tables the query read from the cache.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, QueryResult, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, Inputs, Profile};
use std::time::Instant;

#[path = "common/lubm.rs"]
mod lubm;

const MAX_DERIVED: usize = 1 << 32;
const QUERIES: &str = include_str!("../../../scripts/bench-lubm/queries.txt");
const UB: &str = "http://swat.engr.uky.edu/~zhp2/univ-bench.owl#";

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

fn commit(s: &Store, triple: &str) {
    s.load(&[Source::from_bytes(
        triple.as_bytes().to_vec(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
}

fn rows(r: &QueryResult) -> Vec<String> {
    let mut v: Vec<String> = r
        .rows()
        .unwrap()
        .iter()
        .map(|row| {
            row.values()
                .iter()
                .map(|t| t.as_ref().map_or("UNDEF".into(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    v.sort_unstable();
    v
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let universities: usize = args.next().map_or(10, |a| a.parse().unwrap());
    let rounds: usize = args.next().map_or(5, |a| a.parse().unwrap());
    let qs = queries();
    println!(
        "LUBM-style data of {universities} universities, {} threads, {rounds} rounds",
        rayon::current_num_threads()
    );
    for profile in [Profile::Rdfs, Profile::OwlRl] {
        let s = store(universities);
        let qr = QueryReasoner::new(profile.clone(), &Extras::default(), Inputs::default())
            .unwrap()
            .with_max_derived(MAX_DERIVED);
        let opts = QueryOptions {
            reasoning: Some(qr.provider()),
            ..Default::default()
        };
        for (_, q) in &qs {
            query(s.snapshot(), q, &opts).unwrap();
        }
        // per query and kind of commit: the times, and the cached and used tables
        let mut times = vec![[Vec::new(), Vec::new()]; qs.len()];
        let mut cached = vec![[(0u64, 0u64); 2]; qs.len()];
        for round in 0..rounds {
            for kind in 0..2 {
                for (i, (name, q)) in qs.iter().enumerate() {
                    // a commit of its kind before each query, so each run is the first
                    // after one
                    let triple = if kind == 0 {
                        format!("<urn:x:note{round}x{i}> <urn:x:says> \"round {round}\" .\n")
                    } else {
                        format!(
                            "<urn:x:student{round}x{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{UB}GraduateStudent> .\n"
                        )
                    };
                    commit(&s, &triple);
                    let t = Instant::now();
                    let r = query(s.snapshot(), q, &opts).unwrap();
                    let ms = t.elapsed().as_secs_f64() * 1e3;
                    // the same state read as a view that no cache serves, so that the
                    // check derives from nothing
                    let mut cold = (*s.snapshot()).clone();
                    cold.cacheable = false;
                    let expected = query(
                        std::sync::Arc::new(cold),
                        q,
                        &QueryOptions {
                            no_cache: true,
                            ..opts.clone()
                        },
                    )
                    .unwrap();
                    assert_eq!(
                        rows(&r),
                        rows(&expected),
                        "{} {name} after {triple}",
                        profile.name()
                    );
                    times[i][kind].push(ms);
                    if let Some(a) = r.reasoning() {
                        cached[i][kind].0 += a.report.cached;
                        cached[i][kind].1 += a.report.tables;
                    }
                }
            }
        }
        for (i, (name, _)) in qs.iter().enumerate() {
            for (kind, label) in ["unrelated", "relevant"].into_iter().enumerate() {
                let v = &mut times[i][kind];
                let (lo, hi) = (
                    v.iter().copied().fold(f64::INFINITY, f64::min),
                    v.iter().copied().fold(0.0, f64::max),
                );
                let m = median(v);
                let (c, t) = cached[i][kind];
                println!(
                    "{} {name} {label}: {m:.2} ms ({lo:.2} to {hi:.2}), cached {c} of {t} tables",
                    profile.name()
                );
            }
        }
    }
}
