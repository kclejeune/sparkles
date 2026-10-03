//! Jena ARQ's statistical aggregates (`MEDIAN`, `MODE`, `STDEV`, `STDEV_SAMP`,
//! `STDEV_POP`, `VARIANCE`, `VAR_SAMP`, `VAR_POP`), compared with ARQ's answers.
//!
//! The expected rows are the output of Jena 6.2.0's `arq --results=tsv` for the same
//! data and query, with `|` between the cells: `2.5` is an `xsd:decimal`, `1.25e0` an `xsd:double`, `0` an
//! `xsd:integer` and an empty cell unbound. Doubles are compared to a relative 1e-9,
//! since ARQ's shifted sums depend on the order of a group's rows.

use oxrdf::Term;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const DATA: &str = r#"
@prefix : <http://ex/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
:a1 :g "a" ; :v 1 . :a2 :g "a" ; :v 2 . :a3 :g "a" ; :v 3 . :a4 :g "a" ; :v 4 .
:b1 :g "b" ; :v 1 . :b2 :g "b" ; :v 2 . :b3 :g "b" ; :v 2 . :b4 :g "b" ; :v 7 .
:c1 :g "c" ; :v 5 . :c2 :g "c" ; :v 5 .
:d1 :g "d" ; :v 1.5 . :d2 :g "d" ; :v "2"^^xsd:float . :d3 :g "d" ; :v 3e0 . :d4 :g "d" ; :v 1.5 .
:e1 :g "e" ; :v 1 . :e2 :g "e" ; :v "x" . :e3 :g "e" ; :v 1 .
:f1 :g "f" .
:h1 :g "h" ; :v 10 . :h2 :g "h" ; :v -3 . :h3 :g "h" ; :v 0.25 . :h4 :g "h" ; :v -3 . :h5 :g "h" ; :v "7"^^xsd:short .
:i1 :g "i" ; :v 1000000 . :i2 :g "i" ; :v 0.001 . :i3 :g "i" ; :v 123456.789 . :i4 :g "i" ; :v 1000000 . :i5 :g "i" ; :v 2.5e-3 .
:j1 :g "j" ; :v 9 .
"#;

const PREFIXES: &str = "PREFIX : <http://ex/>
PREFIX agg: <http://jena.apache.org/ARQ/function/aggregate#>
PREFIX afn: <http://jena.apache.org/ARQ/function#>
";

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn run(s: &Store, q: &str) -> Vec<Vec<Option<Term>>> {
    let text = format!("{PREFIXES}{q}");
    query(s.snapshot(), &text, &QueryOptions::default())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .rows()
}

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Whether a Sparkles value is the value ARQ printed as `want` in TSV.
fn matches(got: &Option<Term>, want: &str) -> bool {
    let Some(got) = got else {
        return want.is_empty();
    };
    if let Some(s) = want.strip_prefix('"').and_then(|w| w.strip_suffix('"')) {
        return matches!(got, Term::Literal(l) if l.value() == s);
    }
    let Term::Literal(l) = got else { return false };
    let dt = if want.contains(['e', 'E']) {
        "double"
    } else if want.contains('.') {
        "decimal"
    } else {
        "integer"
    };
    if l.datatype().as_str() != format!("{XSD}{dt}") {
        return false;
    }
    let (Ok(g), Ok(w)) = (l.value().parse::<f64>(), want.parse::<f64>()) else {
        return false;
    };
    g == w || (g - w).abs() <= 1e-9 * w.abs().max(g.abs())
}

/// The query's rows, in order, against ARQ's TSV rows (cells separated by `|`).
fn check(s: &Store, q: &str, want: &str) {
    let got = run(s, q);
    let want: Vec<Vec<&str>> = want
        .trim_matches('\n')
        .lines()
        .map(|l| l.trim_start().split('|').collect())
        .collect();
    assert_eq!(got.len(), want.len(), "{q}\n{got:?}");
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g.len(), w.len(), "{q}");
        for (gc, wc) in g.iter().zip(w) {
            assert!(matches(gc, wc), "{q}\ngot {g:?}\nwant {w:?}");
        }
    }
}

#[test]
fn statistical_aggregates_match_arq() {
    let s = store();
    check(
        &s,
        "SELECT ?g (MEDIAN(?v) AS ?med) (STDEV(?v) AS ?sd) (STDEV_SAMP(?v) AS ?sds) \
         (STDEV_POP(?v) AS ?sdp) (VARIANCE(?v) AS ?var) (VAR_SAMP(?v) AS ?vs) (VAR_POP(?v) AS ?vp) \
         WHERE { ?s :g ?g OPTIONAL { ?s :v ?v } } GROUP BY ?g ORDER BY ?g",
        r#"
"a"|2.5|1.2909944487358056e0|1.2909944487358056e0|1.118033988749895e0|1.6666666666666667e0|1.6666666666666667e0|1.25e0
"b"|2.0|2.70801280154532e0|2.70801280154532e0|2.345207879911715e0|7.333333333333333e0|7.333333333333333e0|5.5e0
"c"|5.0|0.0e0|0.0e0|0.0e0|0.0e0|0.0e0|0.0e0
"d"|1.75|0.7071067811865476e0|0.7071067811865476e0|0.6123724356957945e0|0.5e0|0.5e0|0.375e0
"e"|||||||
"f"|||||||
"h"|0.25|5.952940449895329e0|5.952940449895329e0|5.324471804789654e0|35.4375e0|35.4375e0|28.35e0
"i"|123456.789|527595.4484326303e0|527595.4484326303e0|471895.71492593846e0|2.7835695720682825E11|2.7835695720682825E11|2.226855657654626E11
"j"|9.0|||0.0e0|||0.0e0
"#,
    );
}

#[test]
fn distinct_and_expression_arguments_match_arq() {
    let s = store();
    check(
        &s,
        "SELECT ?g (MEDIAN(DISTINCT ?v) AS ?med) (STDEV(DISTINCT ?v) AS ?sd) \
         (STDEV_POP(DISTINCT ?v) AS ?sdp) (VAR_SAMP(DISTINCT ?v) AS ?vs) (VAR_POP(DISTINCT ?v) AS ?vp) \
         (median(?v * 2) AS ?m2) \
         WHERE { ?s :g ?g OPTIONAL { ?s :v ?v } } GROUP BY ?g ORDER BY ?g",
        r#"
"a"|2.5|1.2909944487358056e0|1.118033988749895e0|1.6666666666666667e0|1.25e0|5.0
"b"|2.0|3.2145502536643185e0|2.6246692913372702e0|10.333333333333334e0|6.888888888888889e0|4.0
"c"|5.0||0.0e0||0.0e0|10.0
"d"|2.0|0.7637626158259734e0|0.6236095644623235e0|0.5833333333333334e0|0.3888888888888889e0|3.5
"e"||||||
"f"||||||
"h"|3.625|5.980436856952843e0|5.1792102438499255e0|35.765625e0|26.82421875e0|0.5
"i"|61728.39575|482943.3326304421e0|418241.1946462811e0|2.3323426253219785E11|1.7492569689914838E11|246913.578
"j"|9.0||0.0e0||0.0e0|18.0
"#,
    );
}

#[test]
fn mode_matches_arq() {
    let s = store();
    // ARQ fails the whole query when a group has no repeated value, so the groups here
    // all have one
    check(
        &s,
        r#"SELECT ?g (MODE(?v) AS ?mode)
           WHERE { ?s :g ?g OPTIONAL { ?s :v ?v } FILTER(?g IN ("b","c","d","e","f","h","i")) }
           GROUP BY ?g ORDER BY ?g"#,
        r#"
"b"|2.0
"c"|5.0
"d"|1.5
"e"|
"f"|
"h"|-3.0
"i"|1000000.0
"#,
    );
    // where ARQ fails: no value repeats, so every value reaches the highest count (one)
    // and the first in row order is the mode
    let rows = run(
        &s,
        r#"SELECT (MODE(?v) AS ?m) (MODE(DISTINCT ?v) AS ?d) WHERE { ?s :g "j" ; :v ?v }"#,
    );
    assert!(matches(&rows[0][0], "9.0") && matches(&rows[0][1], "9.0"));
}

#[test]
fn empty_group_matches_arq() {
    let s = store();
    // MEDIAN and MODE over no rows are 0, the variance aggregates unbound
    check(
        &s,
        "SELECT (MEDIAN(?v) AS ?m) (MODE(?v) AS ?mo) (STDEV(?v) AS ?s) (VAR_POP(?v) AS ?vp) \
         (COUNT(*) AS ?c) WHERE { ?s :v ?v FILTER(false) }",
        "0|0|||0",
    );
    check(
        &s,
        r#"SELECT (MEDIAN(?v) AS ?m) (STDEV(?v) AS ?s) (VAR_POP(?v) AS ?vp) (COUNT(*) AS ?c)
           WHERE { ?s :g "a" ; :v ?v }"#,
        "2.5|1.2909944487358056e0|1.25e0|4",
    );
}

#[test]
fn iri_forms_match_arq() {
    let s = store();
    // ARQ registers the variance and deviation aggregates under agg: and afn:, and
    // `AGG <iri>(…)` calls any custom aggregate; the keywords are case-insensitive
    check(
        &s,
        r#"SELECT ?g (agg:stdev(?v) AS ?a) (afn:stdev_pop(?v) AS ?b) (AGG agg:var_samp(DISTINCT ?v) AS ?c)
             (afn:variance(?v) AS ?d) (AGG <http://jena.apache.org/ARQ/function#var_pop>(?v) AS ?e)
             (vAr_PoP(?v) AS ?f) (STDEV(?v) + 1 AS ?x)
           WHERE { ?s :g ?g ; :v ?v FILTER(?g IN ("a", "h")) }
           GROUP BY ?g HAVING (MEDIAN(?v) > 0) ORDER BY DESC(MEDIAN(?v))"#,
        r#"
"a"|1.2909944487358056e0|1.118033988749895e0|1.6666666666666667e0|1.6666666666666667e0|1.25e0|1.25e0|2.2909944487358054e0
"h"|5.952940449895329e0|5.324471804789654e0|35.765625e0|35.4375e0|28.35e0|28.35e0|6.952940449895329e0
"#,
    );
    // Sparkles also reads agg:median and agg:mode as the aggregates (ARQ has no IRI for
    // them and calls an unknown function)
    check(
        &s,
        r#"SELECT (agg:median(?v) AS ?m) (agg:mode(?v) AS ?o) WHERE { ?s :g "b" ; :v ?v }"#,
        "2.0|2.0",
    );
}

#[test]
fn keyword_forms_print_and_parse_back() {
    let q = spargebra::SparqlParser::new()
        .parse_query(
            "SELECT (MEDIAN(?v) AS ?m) (stdev_pop(DISTINCT ?v) AS ?s) WHERE { ?x <http://ex/v> ?v }",
        )
        .unwrap();
    let text = q.to_string();
    assert!(
        text.contains("MEDIAN(?v)") && text.contains("STDEV_POP(DISTINCT ?v)"),
        "{text}"
    );
    let again = spargebra::SparqlParser::new().parse_query(&text).unwrap();
    assert!(again.to_string().contains("STDEV_POP(DISTINCT ?v)"));
    // a prefix named like a keyword is still a prefix
    for ok in [
        "PREFIX median: <http://ex/> SELECT * WHERE { ?s median:p ?o }",
        "PREFIX agg: <http://ex/> SELECT (agg:f(?o) AS ?x) WHERE { ?s ?p ?o }",
        "PREFIX : <http://ex/> PREFIX agg: <http://ex/a#> SELECT (agg:f(?o) AS ?x) WHERE { ?s ?p ?o }",
    ] {
        let q = spargebra::SparqlParser::new().parse_query(ok).unwrap();
        assert!(!q.to_string().contains("AGG"), "{ok}");
    }
    // aggregates only where aggregates may be
    assert!(
        spargebra::SparqlParser::new()
            .parse_query("SELECT * WHERE { ?s ?p ?o FILTER(MEDIAN(?o) > 1) }")
            .is_err()
    );
}
