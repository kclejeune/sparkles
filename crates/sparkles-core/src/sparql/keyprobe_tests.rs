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
        .test_application_rows()
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

/// A sample that finds no value of a pattern, among values it did not all probe, says
/// only that few match: the join is estimated with the rule of succession and the
/// pattern's rows per value. Probing every value keeps an exact empty join.
#[test]
fn a_sample_without_hits_is_not_an_empty_join() {
    use super::keyprobe::{Measure, est};
    let sampled = Measure {
        m: 0.0,
        f: 0.0,
        rows: 300.0,
        d: 10.0,
        keys: 32,
        complete: false,
    };
    // 1,000 rows, 1/34 of them matching 30 rows each
    let e = est(1000.0, 300.0, &sampled);
    assert!((e - 1000.0 / 34.0 * 30.0).abs() < 1e-6, "{e}");
    let complete = Measure {
        complete: true,
        ..sampled
    };
    assert_eq!(est(1000.0, 300.0, &complete), 1.0);
    // a hit keeps the measured mean
    let hit = Measure {
        m: 2.0,
        f: 0.5,
        ..sampled
    };
    assert_eq!(est(1000.0, 300.0, &hit), 2000.0);
}

/// A small class whose few members a pattern points at: LUBM's `?u a University . ?d
/// subOrganizationOf ?u`, where only ten of a thousand universities have departments.
#[test]
fn few_matching_values_of_a_small_input_are_not_estimated_at_one_row() {
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..1000 {
        t.push_str(&format!("ex:u{i} a ex:U .\n"));
    }
    for d in 0..300 {
        t.push_str(&format!("ex:d{d} ex:sub ex:u{} .\n", d % 10 * 97));
    }
    let s = load(t);
    let snap = s.snapshot();
    let q = "SELECT * WHERE { ?u a ex:U . ?d ex:sub ?u }";
    // the node sets aside, so that the probes estimate the join
    let probes = Optimizations {
        node_sets: false,
        ..Optimizations::ALL
    };
    assert_eq!(answer(&snap, q, probes).len(), 300);
    // the first query reads the blocks that later ones probe
    let _ = answer(&snap, q, probes);
    let est = root_est(&snap, q, probes);
    assert!(est >= 30.0, "estimated {est} for 300");
    let with_sets = root_est(&snap, q, Optimizations::ALL);
    assert!(
        (with_sets - 300.0).abs() <= 1.0,
        "estimated {with_sets} for 300 from the node sets"
    );
}

/// Probed keys cover a small input only when its values were read whole: a scan that
/// stopped at its row limit gave the values of its first rows, and a value after them was
/// never probed.
#[test]
fn keys_cover_a_source_only_when_it_was_read_whole() {
    use super::keyprobe::covers_every_value;
    let ids = |v: &[u64]| v.iter().map(|&x| crate::id::Id(x)).collect::<Vec<_>>();
    let vals = ids(&[1, 1, 2, 3, 3]);
    assert!(covers_every_value(&vals, &ids(&[1, 2, 3]), true));
    assert!(!covers_every_value(&vals, &ids(&[1, 3]), true));
    assert!(!covers_every_value(&vals, &ids(&[1, 2, 3]), false));
}
