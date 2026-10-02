//! Incremental against full materialization on the benchmark data, in a persistent
//! dataset: the time to bring the inferences up to date after inserting or deleting 1,
//! 100 and 10,000 triples.
//!
//! ```sh
//! cargo run --release -p sparkles-reasoner --example incremental -- DIR [triples] [profile]
//! ```
//!
//! Each change is measured three ways: with the closure kept in memory (a server), with
//! the closure read back from the dataset (`sparkles infer`), and as a full run. The data
//! is put back after each measurement, with an untimed incremental run.

use oxrdf::{GraphName, NamedNode, Quad, Triple};
use sparkles::io::{RdfFormat, Source};
use sparkles::store::{Store, StoreOptions};
use sparkles_reasoner::{
    Cache, Extras, Incremental, Method, Profile, ReasonOptions, ReasonReport, infer,
    materialize_incremental,
};
use std::collections::BTreeSet;
use std::time::Instant;

#[path = "common/generate.rs"]
mod data;

const EX: &str = "http://bench.example.org/";

fn change(s: &Store, add: &[Triple], del: &[Triple]) {
    let mut txn = s.write();
    let mut labels = Default::default();
    let q = |t: &Triple| {
        Quad::new(
            t.subject.clone(),
            t.predicate.clone(),
            t.object.clone(),
            GraphName::DefaultGraph,
        )
    };
    for t in del {
        let x = txn.encode_quad(&q(t), &mut labels).unwrap();
        txn.delete(x).unwrap();
    }
    for t in add {
        let x = txn.encode_quad(&q(t), &mut labels).unwrap();
        txn.insert(x).unwrap();
    }
    txn.commit().unwrap();
}

fn run(s: &Store, p: &Profile, since: Option<u64>, cache: Option<&Cache>) -> ReasonReport {
    materialize_incremental(
        s,
        p,
        &Extras::default(),
        Incremental { since, cache },
        &ReasonOptions::default(),
    )
    .unwrap()
}

/// `k` new triples about new instances: a leaf type and property assertions.
fn new_triples(k: usize, round: usize) -> Vec<Triple> {
    let n = |s: String| NamedNode::new_unchecked(s);
    let ty = n("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".into());
    (0..k)
        .map(|j| {
            let s = n(format!("{EX}new{round}-{}", j / 4));
            if j % 4 == 0 {
                Triple::new(s, ty.clone(), n(format!("{EX}C{}", 341 + j % 1024)))
            } else {
                Triple::new(
                    s,
                    n(format!("{EX}p{}", j % 40)),
                    n(format!("{EX}i{}", j * 7)),
                )
            }
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Way {
    Memory,
    Store,
    Full,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(args.get(1).expect("a scratch directory"));
    let n: usize = args
        .get(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_000_000);
    let profile: Profile = args.get(3).map_or(Profile::Rdfs, |p| p.parse().unwrap());
    // `memory` measures the closure kept in memory only
    let ways: Vec<Way> = match args.get(4).map(String::as_str) {
        Some("memory") => vec![Way::Memory],
        _ => vec![Way::Memory, Way::Store, Way::Full],
    };
    let path = dir.join(format!("db-{n}-{}", profile.name()));
    let _ = std::fs::remove_dir_all(&path);
    let s = Store::open(&path, StoreOptions::default()).unwrap();
    let text = data::generate(n);
    let existing: Vec<Triple> = oxttl::NTriplesParser::new()
        .for_slice(text.as_bytes())
        .map(|t| t.unwrap())
        .filter(|t| t.subject.to_string().contains("/i"))
        .collect();
    let t = Instant::now();
    s.load(&[Source::from_bytes(
        text.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    println!(
        "{} triples loaded in {} ms",
        s.snapshot().len(),
        t.elapsed().as_millis()
    );
    let cache = Cache::default();
    let r = run(&s, &profile, None, Some(&cache));
    println!(
        "{}: first full run {} inferred in {} ms",
        profile.name(),
        r.inferred,
        r.millis
    );
    let mut since = r.receipt.unwrap().commit.seq;
    // a second run puts the closure in memory (the first rebuilt the generation)
    let r = run(&s, &profile, Some(since), Some(&cache));
    since = r.receipt.unwrap().commit.seq;
    println!(
        "{}: no change, {} in {} ms ({:?})",
        profile.name(),
        r.method.as_str(),
        r.millis,
        r.changes.map(|c| c.source)
    );
    let mut round = 0;
    let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
    println!("change           memory ms  store ms  full ms  removed  derived  checked");
    for k in [1usize, 100, 10_000] {
        for kind in ["insert", "delete"] {
            let mut times = [Vec::new(), Vec::new(), Vec::new()];
            let mut counts = (0, 0, 0);
            for rep in 0..3 {
                for (wi, &way) in ways.iter().enumerate() {
                    round += 1;
                    let batch: Vec<Triple> = if kind == "insert" {
                        new_triples(k, round)
                    } else {
                        let mut v = BTreeSet::new();
                        while v.len() < k {
                            rng ^= rng << 13;
                            rng ^= rng >> 7;
                            rng ^= rng << 17;
                            v.insert((rng % existing.len() as u64) as usize);
                        }
                        v.into_iter().map(|i| existing[i].clone()).collect()
                    };
                    let (add, del) = if kind == "insert" {
                        (batch.clone(), Vec::new())
                    } else {
                        (Vec::new(), batch.clone())
                    };
                    change(&s, &add, &del);
                    let r = match way {
                        Way::Memory => run(&s, &profile, Some(since), Some(&cache)),
                        Way::Store => run(&s, &profile, Some(since), None),
                        Way::Full => run(&s, &profile, None, Some(&cache)),
                    };
                    let want = if way == Way::Full {
                        Method::Full
                    } else {
                        Method::Incremental
                    };
                    assert_eq!(r.method, want, "{:?}", r.fallback);
                    if way == Way::Memory {
                        assert_eq!(r.changes.as_ref().unwrap().source, "memory");
                    }
                    if let (0, Some(c)) = (rep, &r.changes) {
                        counts = (c.removed, c.derived, c.checked);
                    }
                    times[wi].push(r.millis);
                    since = r.receipt.unwrap().commit.seq;
                    // put the data back
                    change(&s, &del, &add);
                    let r = run(&s, &profile, Some(since), Some(&cache));
                    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
                    since = r.receipt.unwrap().commit.seq;
                }
            }
            let med = |v: &mut Vec<u128>| {
                v.sort_unstable();
                v.get(v.len() / 2).copied().unwrap_or(0)
            };
            let mut times = times.map(|v| v.into_iter().map(u128::from).collect::<Vec<_>>());
            println!(
                "{kind:>6} {k:>6}   {:>9}  {:>8}  {:>7}  {:>7}  {:>7}  {:>7}",
                med(&mut times[0]),
                med(&mut times[1]),
                med(&mut times[2]),
                counts.0,
                counts.1,
                counts.2
            );
        }
    }
    // the incremental result is the full one
    let got: BTreeSet<String> = {
        let snap = s.snapshot();
        let g = snap.lookup_iri(sparkles_reasoner::INFERRED_GRAPH).unwrap();
        snap.scan_keys(sparkles::index::Perm::Gspo, &[g.0])
            .unwrap()
            .iter()
            .map(|k| {
                let q = snap
                    .quad_to_terms(&sparkles::index::Perm::Gspo.to_quad(k))
                    .unwrap();
                Triple::new(q.subject, q.predicate, q.object).to_string()
            })
            .collect()
    };
    let (want, _) = infer(s.snapshot(), &profile, &ReasonOptions::default()).unwrap();
    let want: BTreeSet<String> = want.iter().map(|t| t.to_string()).collect();
    assert_eq!(got, want, "incremental and full results differ");
    println!(
        "final inferred graph equals a full materialization ({} triples)",
        got.len()
    );
}
