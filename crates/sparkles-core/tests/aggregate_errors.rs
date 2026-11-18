//! The three readings of errors in aggregates (roadmap item 1064), on every aggregate
//! with and without DISTINCT, over a variable and over an expression, for groups with
//! an unbound value, a value of the wrong type, only unbound values and no rows.
//!
//! The `jena` expectations are the output of Jena 6.2.0's `arq --results=tsv`, which
//! `fixtures/aggregate-errors/jena.sh` writes to `fixtures/aggregate-errors/jena-6.2.0.tsv`.

use oxrdf::Term;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{AggregateErrors, Optimizations, QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const JENA: &str = include_str!("fixtures/aggregate-errors/jena-6.2.0.tsv");

const VALUES: &str = r#"VALUES (?g ?v) {
 ("good" 1) ("good" 2) ("good" 2)
 ("unb" 1) ("unb" UNDEF) ("unb" 3)
 ("allunb" UNDEF) ("allunb" UNDEF)
 ("str" 1) ("str" "x") ("str" 3)
 ("iri" <http://ex/i>) ("iri" 2)
 ("lang" "a"@en) ("lang" "b")
}"#;

/// A term as ARQ's TSV writes it: a number by its lexical form, a string quoted, an IRI
/// in angle brackets and an unbound value as nothing.
fn tsv(t: &Option<Term>) -> String {
    match t {
        None => String::new(),
        Some(Term::NamedNode(n)) => format!("<{}>", n.as_str()),
        Some(Term::Literal(l)) => {
            let dt = l.datatype().as_str();
            if let Some(lang) = l.language() {
                format!("\"{}\"@{lang}", l.value())
            } else if dt == "http://www.w3.org/2001/XMLSchema#string" {
                format!("\"{}\"", l.value())
            } else {
                l.value().to_string()
            }
        }
        Some(other) => other.to_string(),
    }
}

/// The aggregate's value for each group, as `(group, cell)`, `"empty"` for the group of
/// a query without rows.
fn answers(s: &Store, aggregate: &str, opts: &QueryOptions) -> Vec<(String, String)> {
    let grouped =
        format!("SELECT ?g ({aggregate} AS ?r) WHERE {{ {VALUES} }} GROUP BY ?g ORDER BY ?g");
    let empty = format!("SELECT ({aggregate} AS ?r) WHERE {{ VALUES ?v {{ 1 }} FILTER(false) }}");
    let mut out = Vec::new();
    for row in query(s.snapshot(), &grouped, opts)
        .unwrap()
        .rows()
        .unwrap()
        .iter()
    {
        let v = row.values();
        out.push((tsv(&v[0]), tsv(&v[1])));
    }
    for row in query(s.snapshot(), &empty, opts)
        .unwrap()
        .rows()
        .unwrap()
        .iter()
    {
        out.push(("\"empty\"".to_string(), tsv(&row.values()[0])));
    }
    out
}

/// The fixture's rows as `(aggregate, group, cell)`.
fn jena() -> Vec<(String, String, String)> {
    JENA.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut c = l.split('\t');
            let a = c.next().unwrap().to_string();
            let g = c.next().unwrap().to_string();
            let v = c.next().unwrap_or("").to_string();
            (a, g, v)
        })
        .collect()
}

/// The options of a query under `mode`, with the executor's optimizations as given.
fn under(mode: AggregateErrors, optimizations: Option<Optimizations>) -> QueryOptions {
    QueryOptions {
        aggregate_errors: Some(mode),
        optimizations,
        ..Default::default()
    }
}

/// Every way to run the aggregates: the running states, the per-group kernel that reads
/// each group's rows, and no optimizations at all.
fn executors() -> [Option<Optimizations>; 3] {
    let rows = Optimizations {
        incremental_group: false,
        ..Optimizations::default()
    };
    [None, Some(rows), Some(Optimizations::NONE)]
}

/// Whether two TSV cells hold the same answer: equal, or the same decimal to 1e-15,
/// since Jena divides decimals to more digits than the engine.
fn same(got: &str, want: &str) -> bool {
    got == want
        || matches!((got.parse::<f64>(), want.parse::<f64>()),
            (Ok(g), Ok(w)) if got.contains('.') && (g - w).abs() <= 1e-15 * w.abs().max(1.0))
}

#[test]
fn the_jena_reading_answers_as_arq() {
    let s = Store::in_memory(StoreOptions::default());
    let want = jena();
    let mut aggs: Vec<&str> = want.iter().map(|(a, _, _)| a.as_str()).collect();
    aggs.dedup();
    assert_eq!(aggs.len(), 28);
    for opt in executors() {
        for a in &aggs {
            let got = answers(&s, a, &under(AggregateErrors::Jena, opt));
            let rows: Vec<_> = want.iter().filter(|(wa, _, _)| wa == a).collect();
            assert_eq!(got.len(), rows.len(), "{a}: {got:?}");
            for (g, v) in got {
                let w = &rows.iter().find(|(_, wg, _)| *wg == g).expect("a group").2;
                // SAMPLE may pick any value, so only whether it is bound is compared
                if a.starts_with("SAMPLE") {
                    assert_eq!(v.is_empty(), w.is_empty(), "{a} {g}: {v} against {w}");
                } else {
                    assert!(same(&v, w), "{a} {g} {opt:?}: {v} against Jena's {w}");
                }
            }
        }
    }
}

/// The value of `aggregate` for `group` under `mode`, the same on every executor.
fn one(s: &Store, mode: AggregateErrors, aggregate: &str, group: &str) -> String {
    let mut seen: Option<String> = None;
    for opt in executors() {
        let got = answers(s, aggregate, &under(mode, opt));
        let v = got
            .into_iter()
            .find(|(g, _)| g == &format!("\"{group}\""))
            .unwrap_or_else(|| panic!("{aggregate}: no group {group}"))
            .1;
        if let Some(prev) = &seen {
            assert_eq!(prev, &v, "{mode} {aggregate} {group} differs on {opt:?}");
        }
        seen = Some(v);
    }
    seen.unwrap()
}

#[test]
fn the_three_readings_of_min_and_max() {
    let s = Store::in_memory(StoreOptions::default());
    use AggregateErrors::*;
    for arg in ["?v", "DISTINCT ?v", "(?v * 1)"] {
        for (mode, min, max) in [(Jena, "", ""), (Sparkles, "1", "3"), (Spec, "", "3")] {
            assert_eq!(
                one(&s, mode, &format!("MIN({arg})"), "unb"),
                min,
                "{mode} {arg}"
            );
            assert_eq!(
                one(&s, mode, &format!("MAX({arg})"), "unb"),
                max,
                "{mode} {arg}"
            );
        }
        // only errors leave nothing to choose from
        for mode in AggregateErrors::ALL {
            assert_eq!(one(&s, mode, &format!("MIN({arg})"), "allunb"), "");
            assert_eq!(one(&s, mode, &format!("MAX({arg})"), "allunb"), "");
            assert_eq!(one(&s, mode, &format!("MIN({arg})"), "empty"), "");
        }
    }
    // a value of another type is no error for MIN and MAX, and is one for `* 1`
    assert_eq!(one(&s, Spec, "MAX((?v * 1))", "str"), "3");
    assert_eq!(one(&s, Spec, "MIN((?v * 1))", "str"), "");
    assert_eq!(one(&s, Jena, "MAX(?v)", "str"), "3");
}

#[test]
fn the_three_readings_of_avg() {
    let s = Store::in_memory(StoreOptions::default());
    use AggregateErrors::*;
    for arg in ["?v", "DISTINCT ?v", "(?v * 1)"] {
        let avg = format!("AVG({arg})");
        assert_eq!(one(&s, Jena, &avg, "allunb"), "", "{arg}");
        assert_eq!(one(&s, Sparkles, &avg, "allunb"), "", "{arg}");
        // Sum over Count, where Count leaves the errors out
        assert_eq!(one(&s, Spec, &avg, "allunb"), "0", "{arg}");
        for mode in AggregateErrors::ALL {
            // an error among values makes Sum an error in every reading
            assert_eq!(one(&s, mode, &avg, "unb"), "", "{mode} {arg}");
            assert_eq!(one(&s, mode, &avg, "empty"), "0", "{mode} {arg}");
        }
    }
    // a value that is not a number is no evaluation error of the argument, so the
    // spec's Count counts it and Sum fails on it
    assert_eq!(one(&s, Spec, "AVG(?v)", "str"), "");
}

#[test]
fn the_three_readings_of_group_concat() {
    let s = Store::in_memory(StoreOptions::default());
    use AggregateErrors::*;
    for arg in ["?v", "DISTINCT ?v"] {
        let gc = format!("GROUP_CONCAT({arg})");
        for mode in [Jena, Sparkles] {
            assert_eq!(
                one(&s, mode, &gc, "iri"),
                "\"http://ex/i 2\"",
                "{mode} {arg}"
            );
            assert_eq!(one(&s, mode, &gc, "str"), "\"1 x 3\"", "{mode} {arg}");
        }
        // CONCAT takes only strings
        assert_eq!(one(&s, Spec, &gc, "iri"), "", "{arg}");
        assert_eq!(one(&s, Spec, &gc, "good"), "", "{arg}");
        assert_eq!(one(&s, Spec, &gc, "str"), "", "{arg}");
        // a language-tagged string is a string, and the result is a simple literal
        for mode in AggregateErrors::ALL {
            assert_eq!(one(&s, mode, &gc, "lang"), "\"a b\"", "{mode} {arg}");
            assert_eq!(one(&s, mode, &gc, "unb"), "", "{mode} {arg}");
            // Jena makes GROUP_CONCAT(DISTINCT …) over no rows unbound
            let nothing = if mode == Jena && arg.starts_with("DISTINCT") {
                ""
            } else {
                "\"\""
            };
            assert_eq!(one(&s, mode, &gc, "empty"), nothing, "{mode} {arg}");
        }
    }
}

/// Stored data read through OPTIONAL, and a variable the pattern never binds, which the
/// grouping takes as an argument unbound in every row.
#[test]
fn stored_data_and_a_variable_never_bound() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        br#"@prefix : <http://ex/> .
:a :g "a" ; :v 1 . :b :g "a" ; :v 3 . :c :g "a" .
:d :g "b" ; :v 2 .
"#
        .to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let q = |mode, agg: &str| -> Vec<String> {
        let mut out = Vec::new();
        for opt in executors() {
            let text = format!(
                "PREFIX : <http://ex/> SELECT ?g ({agg} AS ?r) WHERE {{ ?s :g ?g OPTIONAL {{ ?s :v ?v }} }} GROUP BY ?g ORDER BY ?g"
            );
            let rows: Vec<String> = query(s.snapshot(), &text, &under(mode, opt))
                .unwrap()
                .rows()
                .unwrap()
                .iter()
                .map(|r| tsv(&r.values()[1]))
                .collect();
            if !out.is_empty() {
                assert_eq!(out, rows, "{mode} {agg} {opt:?}");
            }
            out = rows;
        }
        out
    };
    use AggregateErrors::*;
    assert_eq!(q(Jena, "MIN(?v)"), ["", "2"]);
    assert_eq!(q(Sparkles, "MIN(?v)"), ["1", "2"]);
    assert_eq!(q(Spec, "MIN(?v)"), ["", "2"]);
    assert_eq!(q(Spec, "MAX(?v)"), ["3", "2"]);
    assert_eq!(q(Jena, "MAX(?v)"), ["", "2"]);
    for mode in AggregateErrors::ALL {
        let want: [&str; 2] = if mode == Spec { ["0", "0"] } else { ["", ""] };
        assert_eq!(q(mode, "AVG(?never)"), want, "{mode}");
        assert_eq!(q(mode, "MAX(?never)"), ["", ""], "{mode}");
    }
}

/// A store's reading applies to the queries that do not choose one, and a query's own
/// choice comes first.
#[test]
fn the_store_reading_is_the_default() {
    let s = Store::in_memory(StoreOptions {
        aggregate_errors: AggregateErrors::Sparkles,
        ..Default::default()
    });
    let mode = |opts: &QueryOptions| answers(&s, "MIN(?v)", opts);
    let unb = |a: Vec<(String, String)>| a.into_iter().find(|(g, _)| g == "\"unb\"").unwrap().1;
    assert_eq!(unb(mode(&QueryOptions::default())), "1");
    assert_eq!(unb(mode(&under(AggregateErrors::Jena, None))), "");
}

/// The result cache keeps the readings apart.
#[test]
fn the_result_cache_keys_on_the_reading() {
    let s = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    for _ in 0..2 {
        for (mode, want) in [
            (AggregateErrors::Sparkles, "1"),
            (AggregateErrors::Jena, ""),
            (AggregateErrors::Spec, ""),
        ] {
            let got = answers(&s, "MIN(?v)", &under(mode, None));
            let v = got.into_iter().find(|(g, _)| g == "\"unb\"").unwrap().1;
            assert_eq!(v, want, "{mode}");
        }
    }
}

/// The result cache keeps the readings apart for an aggregate inside a correlated
/// LATERAL, whose right side is planned while the query runs, and when the dataset's
/// setting changes without a commit.
#[test]
fn the_result_cache_keys_a_lateral_on_the_reading() {
    let s = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    let text = "SELECT ?x ?m WHERE { VALUES ?x { 7 } LATERAL { \
        SELECT ?x (MIN(?v) AS ?m) WHERE { VALUES ?v { 1 UNDEF } } GROUP BY ?x } }";
    let m = |opts: &QueryOptions| {
        let r = query(s.snapshot(), text, opts).unwrap().rows().unwrap();
        r.iter()
            .map(|row| tsv(&row.values()[1]))
            .collect::<Vec<_>>()
    };
    assert_eq!(m(&under(AggregateErrors::Sparkles, None)), ["1"]);
    assert_eq!(m(&under(AggregateErrors::Jena, None)), [""]);
    // the same requests again are answered from the cache
    let hits = s.result_cache().hits();
    assert_eq!(m(&under(AggregateErrors::Sparkles, None)), ["1"]);
    assert_eq!(m(&under(AggregateErrors::Jena, None)), [""]);
    assert!(s.result_cache().hits() > hits, "the cache answered");
    assert_eq!(m(&QueryOptions::default()), [""]);
    s.set_aggregate_errors_setting(Some(AggregateErrors::Sparkles))
        .unwrap();
    assert_eq!(m(&QueryOptions::default()), ["1"]);
    s.set_aggregate_errors_setting(None).unwrap();
    assert_eq!(m(&QueryOptions::default()), [""]);
}

/// EXPLAIN names a reading other than the default on the aggregates it changes.
#[test]
fn the_plan_names_the_reading() {
    let s = Store::in_memory(StoreOptions::default());
    let plan = |mode, agg: &str| {
        let text = format!("SELECT ?g ({agg} AS ?r) WHERE {{ {VALUES} }} GROUP BY ?g");
        let r = query(s.snapshot(), &text, &under(mode, None)).unwrap();
        format!("{:?}", r.plan())
    };
    assert!(plan(AggregateErrors::Spec, "MIN(?v)").contains("[aggregate errors: spec]"));
    assert!(plan(AggregateErrors::Sparkles, "AVG(?v)").contains("[aggregate errors: sparkles]"));
    assert!(!plan(AggregateErrors::Jena, "MIN(?v)").contains("aggregate errors"));
    assert!(!plan(AggregateErrors::Spec, "COUNT(?v)").contains("aggregate errors"));
}

/// The WHERE clause of an update reads aggregates as the request chooses, else as the
/// dataset's own setting says, else as the store's option says.
#[test]
fn an_update_reads_aggregates_as_the_request_or_the_dataset_says() {
    let insert = r#"INSERT { <http://ex/r> <http://ex/min> ?m } WHERE {
        SELECT (MIN(?v) AS ?m) WHERE { VALUES ?v { 3 UNDEF 1 } } }"#;
    let written = |s: &Store| {
        let r = query(
            s.snapshot(),
            "SELECT ?m WHERE { <http://ex/r> <http://ex/min> ?m }",
            &QueryOptions::default(),
        )
        .unwrap()
        .rows()
        .unwrap();
        let cells: Vec<String> = r.iter().map(|row| tsv(&row.values()[0])).collect();
        sparkles_core::sparql::update::update(
            s,
            "DELETE WHERE { <http://ex/r> ?p ?o }",
            &QueryOptions::default(),
        )
        .unwrap();
        cells
    };
    let run = |s: &Store, opts: &QueryOptions| {
        sparkles_core::sparql::update::update(s, insert, opts).unwrap();
        written(s)
    };
    let s = Store::in_memory(StoreOptions::default());
    // jena: MIN over an unbound value is unbound, so the template writes nothing
    assert!(run(&s, &QueryOptions::default()).is_empty());
    assert_eq!(run(&s, &under(AggregateErrors::Sparkles, None)), ["1"]);
    s.set_aggregate_errors_setting(Some(AggregateErrors::Sparkles))
        .unwrap();
    assert_eq!(run(&s, &QueryOptions::default()), ["1"]);
    assert!(run(&s, &under(AggregateErrors::Jena, None)).is_empty());
    let mut txn = s.write();
    sparkles_core::sparql::update::update_in(&mut txn, insert, &QueryOptions::default()).unwrap();
    txn.commit().unwrap();
    assert_eq!(written(&s), ["1"]);
}

/// A new branch keeps its upstream's reading, in memory and on disk.
#[test]
fn a_branch_keeps_the_upstream_reading() {
    let dir = tempfile::tempdir().unwrap();
    for s in [
        Store::in_memory(StoreOptions::default()),
        Store::open(dir.path(), StoreOptions::default()).unwrap(),
    ] {
        s.set_aggregate_errors_setting(Some(AggregateErrors::Sparkles))
            .unwrap();
        s.create_branch("b", &Default::default()).unwrap();
        let b = s.branch("b").unwrap();
        assert_eq!(
            b.aggregate_errors_setting(),
            Some(AggregateErrors::Sparkles)
        );
    }
}
