//! Join estimates from probing the values of a small input: VALUES tables and small
//! patterns joined with larger ones are estimated from counts of the actual values, and
//! answers never change.

use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX ex: <http://ex.org/> ";

fn off() -> Optimizations {
    Optimizations {
        probed_keys: false,
        ..Optimizations::ALL
    }
}

fn opts(opt: Optimizations) -> QueryOptions {
    QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    }
}

fn load(ttl: String) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// `n` subjects with a name and an age; every fourth has three tags, and every tenth is
/// of class `ex:C`.
fn people(n: usize) -> String {
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..n {
        t.push_str(&format!("ex:s{i} ex:name \"n{i}\" ; ex:age {} .\n", i % 70));
        if i % 4 == 0 {
            t.push_str(&format!("ex:s{i} ex:tag ex:t1, ex:t2, ex:t3 .\n"));
        }
        if i % 10 == 0 {
            t.push_str(&format!("ex:s{i} a ex:C .\n"));
        }
    }
    t
}

fn root_est(snap: &Arc<crate::store::Snapshot>, q: &str, opt: Optimizations) -> f64 {
    let (_, info) = explain(snap.clone(), &format!("{PREFIXES}{q}"), &opts(opt))
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    // below the projection
    let mut p = &info;
    while p.operator == "Project" {
        p = &p.children[0];
    }
    p.estimated_rows
}

fn answer(snap: &Arc<crate::store::Snapshot>, q: &str, opt: Optimizations) -> Vec<String> {
    let r = query(snap.clone(), &format!("{PREFIXES}{q}"), &opts(opt))
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut rows: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    rows.sort();
    rows
}

/// VALUES of subjects that all have the pattern: the estimate is the number of rows,
/// where the estimate from distinct values takes off 30%.
#[test]
fn values_joined_with_patterns_are_estimated_from_their_keys() {
    let s = load(people(4000));
    let snap = s.snapshot();
    let keys: Vec<String> = (0..400).map(|i| format!("ex:s{}", i * 8)).collect();
    let values = format!("VALUES ?s {{ {} }}", keys.join(" "));
    for (q, rows) in [
        (
            format!("SELECT * WHERE {{ {values} ?s ex:name ?n }}"),
            400.0,
        ),
        // every eighth subject has the tags
        (
            format!("SELECT * WHERE {{ {values} ?s ex:tag ?t }}"),
            1200.0,
        ),
        (
            format!("SELECT * WHERE {{ {values} ?s ex:name ?n ; ex:age ?a }}"),
            400.0,
        ),
    ] {
        assert_eq!(
            answer(&snap, &q, Optimizations::ALL).len() as f64,
            rows,
            "{q}"
        );
        let est = root_est(&snap, &q, Optimizations::ALL);
        assert!(
            (est - rows).abs() <= rows * 0.05,
            "{q}: estimated {est} for {rows}"
        );
        let old = root_est(&snap, &q, off());
        assert!(old < rows * 0.75, "{q}: {old} without probes");
    }
}

/// A small pattern drives the join: `?s a ex:C` holds every tenth subject, of which
/// those divisible by 20 have tags.
#[test]
fn small_patterns_are_estimated_from_their_keys() {
    let s = load(people(4000));
    let snap = s.snapshot();
    let q = "SELECT * WHERE { ?s a ex:C ; ex:tag ?t }";
    assert_eq!(answer(&snap, q, Optimizations::ALL).len(), 600);
    let est = root_est(&snap, q, Optimizations::ALL);
    assert!((est - 600.0).abs() <= 120.0, "estimated {est} for 600");
}

/// Random joins of VALUES and small patterns with larger ones: the same solutions with
/// the estimates on and off (a debug build also checks that the join ordering's
/// summaries match the plans built from them).
#[test]
fn probed_estimates_never_change_answers() {
    let s = load(people(600));
    let snap = s.snapshot();
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let pats = [
        "?s ex:name ?n",
        "?s ex:age ?a",
        "?s ex:tag ?t",
        "?s a ex:C",
        "?s ex:age 7",
        "?s ex:tag ex:t2",
    ];
    for round in 0..50 {
        let mut q = String::from("SELECT * WHERE { ");
        if round % 2 == 0 {
            let k = 1 + next() % 40;
            let keys: Vec<String> = (0..k).map(|_| format!("ex:s{}", next() % 700)).collect();
            q.push_str(&format!("VALUES ?s {{ {} }} ", keys.join(" ")));
        }
        let n = 1 + next() as usize % 4;
        for _ in 0..n {
            q.push_str(pats[next() as usize % pats.len()]);
            q.push_str(" . ");
        }
        if round % 5 == 0 {
            q.push_str("?t ex:x ?y . ");
        }
        q.push('}');
        assert_eq!(
            answer(&snap, &q, Optimizations::ALL),
            answer(&snap, &q, off()),
            "{q}"
        );
    }
}
