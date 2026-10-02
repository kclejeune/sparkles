//! Join estimates for subject stars from characteristic sets: the sets a build counts,
//! exact estimates for stars of independent predicates, and the same answers with the
//! estimates on and off.

use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX ex: <http://ex.org/> ";

fn off() -> Optimizations {
    Optimizations {
        characteristic_sets: false,
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

/// `n` subjects: every one has `ex:a`, every second `ex:b`, every third two values of
/// `ex:c`, and every fifth an `ex:d` that links to another subject. `ex:b`, `ex:c` and
/// `ex:d` occur independently of each other.
fn subjects(n: usize) -> String {
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..n {
        t.push_str(&format!("ex:s{i} ex:a {i} .\n"));
        if i % 2 == 0 {
            t.push_str(&format!("ex:s{i} ex:b \"b{i}\" .\n"));
        }
        if i % 3 == 0 {
            t.push_str(&format!("ex:s{i} ex:c 1, 2 .\n"));
        }
        if i % 5 == 0 {
            t.push_str(&format!("ex:s{i} ex:d ex:s{} .\n", (i * 7) % n));
        }
    }
    t
}

/// The joins of a plan: their estimated rows.
fn joins(p: &PlanInfo, out: &mut Vec<f64>) {
    if p.operator.contains("Join") {
        out.push(p.estimated_rows);
    }
    for c in &p.children {
        joins(c, out);
    }
}

fn root_est(snap: &Arc<crate::store::Snapshot>, q: &str, opt: Optimizations) -> f64 {
    let (_, info) = explain(snap.clone(), &format!("{PREFIXES}{q}"), &opts(opt))
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut js = Vec::new();
    joins(&info, &mut js);
    js[0]
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

#[test]
fn builds_count_characteristic_sets() {
    let s = load(subjects(60));
    let snap = s.snapshot();
    let stats = &snap.generation.stats;
    // a, ab, ac, ad, abc, abd, acd, abcd
    assert_eq!(stats.charsets.len(), 8, "{:?}", stats.charsets);
    assert_eq!(stats.charset_others, 0);
    let subjects: u64 = stats.charsets.iter().map(|c| c.subjects).sum();
    assert_eq!(subjects, 60);
    // the 2 subjects divisible by 30 have all four predicates, two values of ex:c each
    let all = stats.charsets.iter().find(|c| c.preds.len() == 4).unwrap();
    assert_eq!(all.subjects, 2);
    assert_eq!(all.triples, vec![2, 2, 4, 2]);
}

/// A star of independent predicates: the estimate from the sets is the number of rows,
/// where the estimate from distinct values assumes every subject with the rarer
/// predicate has the others.
#[test]
fn stars_of_independent_predicates_are_estimated_exactly() {
    let n = 600;
    let s = load(subjects(n));
    let snap = s.snapshot();
    for (q, rows) in [
        ("SELECT * WHERE { ?s ex:b ?b ; ex:c ?c }", 200.0),
        ("SELECT * WHERE { ?s ex:b ?b ; ex:d ?d }", 60.0),
        (
            "SELECT * WHERE { ?s ex:a ?a ; ex:b ?b ; ex:c ?c ; ex:d ?d }",
            40.0,
        ),
    ] {
        assert_eq!(
            answer(&snap, q, Optimizations::ALL).len() as f64,
            rows,
            "{q}"
        );
        let est = root_est(&snap, q, Optimizations::ALL);
        assert!((est - rows).abs() < 0.5, "{q}: estimated {est} for {rows}");
        let old = root_est(&snap, q, off());
        assert!(old > rows * 1.3, "{q}: {old} without the sets");
    }
}

/// Patterns with a constant object, or a predicate that occurs twice, keep the estimate
/// from distinct values.
#[test]
fn other_patterns_keep_the_estimate_from_distinct_values() {
    let s = load(subjects(300));
    let snap = s.snapshot();
    for q in [
        "SELECT * WHERE { ?s ex:b ?b ; ex:c 2 }",
        "SELECT * WHERE { ?s ex:c ?x ; ex:c ?y }",
        "SELECT * WHERE { ?s ex:d ?o . ?o ex:b ?b }",
    ] {
        assert_eq!(
            root_est(&snap, q, Optimizations::ALL),
            root_est(&snap, q, off()),
            "{q}"
        );
    }
}

/// Random stars, chains and filters: the same solutions with the estimates on and off
/// (a debug build also checks that the join ordering's summaries match the plans).
#[test]
fn estimates_from_sets_never_change_answers() {
    let s = load(subjects(240));
    let snap = s.snapshot();
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let preds = ["ex:a", "ex:b", "ex:c", "ex:d"];
    for round in 0..60 {
        let n = 2 + round % 5;
        let mut pats = Vec::new();
        for i in 0..n {
            let s = if i > 0 && next() % 4 == 0 {
                "?o0"
            } else {
                "?s"
            };
            let p = preds[next() as usize % 4];
            let o = match next() % 6 {
                0 => "1".to_string(),
                1 if p == "ex:d" => "?s".to_string(),
                _ => format!("?o{i}"),
            };
            pats.push(format!("{s} {p} {o}"));
        }
        let mut q = format!("SELECT * WHERE {{ {} .", pats.join(" . "));
        if round % 3 == 0 {
            q.push_str(" FILTER(STRLEN(STR(?s)) > 6)");
        }
        q.push_str(" }");
        assert_eq!(
            answer(&snap, &q, Optimizations::ALL),
            answer(&snap, &q, off()),
            "{q}"
        );
    }
}
