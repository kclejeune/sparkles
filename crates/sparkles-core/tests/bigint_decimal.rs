//! Regression tests for roadmap item 1065: arbitrary-precision `xsd:integer`,
//! derived integer types, and `xsd:decimal` beyond fixed-width `i64`/`Decimal`.

use oxrdf::vocab::xsd;
use oxrdf::{NamedNode, Term};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{
    CursorOptions, CursorStatus, FallbackPolicy, QueryOptions, query, select_cursor,
};
use sparkles_core::store::{Store, StoreOptions};
use std::fs;

const PREFIXES: &str = "PREFIX ex: <http://example.org/> \
                        PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> \
                        PREFIX fn: <http://www.w3.org/2005/xpath-functions#> \
                        PREFIX afn: <http://jena.apache.org/ARQ/function#> ";

fn run_eager(store: &Store, sparql: &str) -> Vec<Vec<Option<Term>>> {
    let q = format!("{PREFIXES}{sparql}");
    let res = query(store.snapshot(), &q, &QueryOptions::default()).expect("eager query");
    res.rows()
        .expect("select rows")
        .iter()
        .map(|row| row.values().to_vec())
        .collect()
}

fn run_streaming(store: &Store, sparql: &str, batch_rows: usize) -> Vec<Vec<Option<Term>>> {
    let copts = CursorOptions {
        batch_rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let q = format!("{PREFIXES}{sparql}");
    let mut cursor = select_cursor(store.snapshot(), &q, &QueryOptions::default(), &copts)
        .expect("streaming query");
    let mut answer = Vec::new();
    while let Some(batch) = cursor.next_batch().expect("next batch") {
        for row in 0..batch.len() {
            answer.push(batch.row(row).expect("row").values().to_vec());
        }
    }
    assert_eq!(cursor.status(), CursorStatus::Complete);
    answer
}

/// Run a SELECT query in both eager and streaming (batch size 2 and default) modes and
/// assert identical results.
fn run_both(store: &Store, sparql: &str) -> Vec<Vec<Option<Term>>> {
    let eager = run_eager(store, sparql);
    let stream_small = run_streaming(store, sparql, 2);
    let stream_default = run_streaming(store, sparql, 4096);
    assert_eq!(eager, stream_small, "eager vs streaming(2) for: {sparql}");
    assert_eq!(
        eager, stream_default,
        "eager vs streaming(4096) for: {sparql}"
    );
    eager
}

fn store_from_ttl(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        ttl.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .expect("load ttl");
    s
}

fn lit_val(row: &[Option<Term>], col: usize) -> Option<(String, String)> {
    match row[col].as_ref()? {
        Term::Literal(l) => Some((l.value().to_string(), l.datatype().as_str().to_string())),
        other => panic!("expected literal, got {other:?}"),
    }
}

#[test]
fn range_filters_and_compacted_vocab_keep_large_integers_and_decimals() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let mut ttl = String::from(
        "@prefix ex: <http://example.org/> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    // Add > 1200 ordinary rows so rangemask and exprcache paths activate.
    for i in 0..1200 {
        ttl.push_str(&format!("ex:n{i} ex:v {i} .\n"));
    }
    ttl.push_str(
        r#"
        ex:non_inline_int ex:v "42"^^xsd:int .
        ex:big_int ex:v "99999999999999999999999"^^xsd:integer .
        ex:big_neg ex:v "-99999999999999999999999"^^xsd:integer .
        ex:big_nonneg ex:v "100000000000000000000000"^^xsd:nonNegativeInteger .
        ex:big_pos ex:v "100000000000000000000001"^^xsd:positiveInteger .
        ex:big_nonpos ex:v "-100000000000000000000000"^^xsd:nonPositiveInteger .
        ex:big_negint ex:v "-100000000000000000000001"^^xsd:negativeInteger .
        ex:max_ulong ex:v "18446744073709551615"^^xsd:unsignedLong .
        ex:big_dec ex:v "999999999999999999999999999999.5"^^xsd:decimal .
        ex:tiny_dec ex:v "0.0000000000000000001"^^xsd:decimal .
        ex:bad_nonneg ex:v "-5"^^xsd:nonNegativeInteger .
        ex:bad_ulong ex:v "18446744073709551616"^^xsd:unsignedLong .
        ex:bad_long ex:v "9223372036854775808"^^xsd:long .
        ex:bad_pos ex:v "0"^^xsd:positiveInteger .
        ex:str ex:v "99999999999999999999999" .
    "#,
    );
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    // Compact so `vocab.num` (SPKVNUM4) is built and tested alongside RangeScan.
    s.compact().unwrap();
    assert!(s.snapshot().generation.vocab.numeric().is_some());

    // `?v > 150000` must return all positive big integers, max_ulong, and big_dec,
    // and exclude negatives, tiny_dec, ill-typed derived integers, and strings.
    let rows = run_both(
        &s,
        "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v > 150000) } ORDER BY ?s",
    );
    let subjects: Vec<String> = rows
        .iter()
        .map(|r| r[0].as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        subjects,
        vec![
            "<http://example.org/big_dec>",
            "<http://example.org/big_int>",
            "<http://example.org/big_nonneg>",
            "<http://example.org/big_pos>",
            "<http://example.org/max_ulong>",
        ]
    );

    // Filter with a big integer constant on the right-hand side.
    let rows = run_both(
        &s,
        "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v >= 99999999999999999999999) } ORDER BY ?s",
    );
    let subjects: Vec<String> = rows
        .iter()
        .map(|r| r[0].as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        subjects,
        vec![
            "<http://example.org/big_dec>",
            "<http://example.org/big_int>",
            "<http://example.org/big_nonneg>",
            "<http://example.org/big_pos>",
        ]
    );

    // Negative range filter.
    let rows = run_both(
        &s,
        "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v < -150000) } ORDER BY ?s",
    );
    let subjects: Vec<String> = rows
        .iter()
        .map(|r| r[0].as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        subjects,
        vec![
            "<http://example.org/big_neg>",
            "<http://example.org/big_negint>",
            "<http://example.org/big_nonpos>",
        ]
    );

    // Tiny >18-fractional-digit decimal is strictly between 0 and 1e-18.
    let rows = run_both(
        &s,
        "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v > 0 && ?v < 0.000000000000000001) }",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0][0].as_ref().unwrap().to_string(),
        "<http://example.org/tiny_dec>"
    );

    // `isNumeric` distinguishes valid large/derived numbers from ill-typed ones.
    let rows = run_both(
        &s,
        "SELECT ?s (isNumeric(?v) AS ?n) WHERE { \
            ?s ex:v ?v \
            VALUES ?s { ex:big_int ex:big_nonneg ex:big_pos ex:big_nonpos ex:big_negint \
                        ex:max_ulong ex:big_dec ex:tiny_dec \
                        ex:bad_nonneg ex:bad_ulong ex:bad_long ex:bad_pos ex:str } \
         } ORDER BY ?s",
    );
    let flags: Vec<(String, String)> = rows
        .iter()
        .map(|r| (r[0].as_ref().unwrap().to_string(), lit_val(r, 1).unwrap().0))
        .collect();
    assert_eq!(
        flags,
        vec![
            ("<http://example.org/bad_long>".into(), "false".into()),
            ("<http://example.org/bad_nonneg>".into(), "false".into()),
            ("<http://example.org/bad_pos>".into(), "false".into()),
            ("<http://example.org/bad_ulong>".into(), "false".into()),
            ("<http://example.org/big_dec>".into(), "true".into()),
            ("<http://example.org/big_int>".into(), "true".into()),
            ("<http://example.org/big_negint>".into(), "true".into()),
            ("<http://example.org/big_nonneg>".into(), "true".into()),
            ("<http://example.org/big_nonpos>".into(), "true".into()),
            ("<http://example.org/big_pos>".into(), "true".into()),
            ("<http://example.org/max_ulong>".into(), "true".into()),
            ("<http://example.org/str>".into(), "false".into()),
            ("<http://example.org/tiny_dec>".into(), "true".into()),
        ]
    );
}

#[test]
fn mixed_comparisons_and_order_by_including_topk() {
    let s = store_from_ttl(
        r#"
        @prefix ex: <http://example.org/> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        ex:neg_inf ex:v "-INF"^^xsd:double .
        ex:neg_big ex:v "-99999999999999999999999"^^xsd:integer .
        ex:neg_small ex:v -10 .
        ex:zero ex:v 0 .
        ex:tiny ex:v "0.0000000000000000001"^^xsd:decimal .
        ex:small ex:v 150000 .
        ex:flt ex:v "1.0e20"^^xsd:float .
        ex:big1 ex:v "99999999999999999999998"^^xsd:integer .
        ex:big2 ex:v "99999999999999999999999"^^xsd:integer .
        ex:big2_dec ex:v "99999999999999999999999.5"^^xsd:decimal .
        ex:huge ex:v "99999999999999999999999999999999999999999999999999"^^xsd:integer .
        ex:pos_inf ex:v "INF"^^xsd:double .
    "#,
    );

    let ordered = run_both(&s, "SELECT ?s WHERE { ?s ex:v ?v } ORDER BY ?v ?s");
    let got: Vec<String> = ordered
        .iter()
        .map(|r| r[0].as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        got,
        vec![
            "<http://example.org/neg_inf>",
            "<http://example.org/neg_big>",
            "<http://example.org/neg_small>",
            "<http://example.org/zero>",
            "<http://example.org/tiny>",
            "<http://example.org/small>",
            "<http://example.org/flt>",
            "<http://example.org/big1>",
            "<http://example.org/big2>",
            "<http://example.org/big2_dec>",
            "<http://example.org/huge>",
            "<http://example.org/pos_inf>",
        ]
    );

    // Top-k descending (exercises top-k heap / prefilter paths).
    let top3 = run_both(
        &s,
        "SELECT ?s WHERE { ?s ex:v ?v } ORDER BY DESC(?v) ?s LIMIT 3",
    );
    let top3_s: Vec<String> = top3
        .iter()
        .map(|r| r[0].as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        top3_s,
        vec![
            "<http://example.org/pos_inf>",
            "<http://example.org/huge>",
            "<http://example.org/big2_dec>",
        ]
    );

    // Mixed comparisons: =, !=, <, <=, >, >=, sameTerm, NaN, INF, -INF.
    let cmp_rows = run_both(
        &s,
        r#"SELECT
            ("99999999999999999999999"^^xsd:integer = "99999999999999999999999.0"^^xsd:decimal AS ?eq_dec)
            (sameTerm("99999999999999999999999"^^xsd:integer, "99999999999999999999999.0"^^xsd:decimal) AS ?same_dec)
            ("99999999999999999999999"^^xsd:integer != "99999999999999999999998"^^xsd:integer AS ?ne_big)
            ("99999999999999999999999"^^xsd:integer < "INF"^^xsd:double AS ?lt_inf)
            ("99999999999999999999999"^^xsd:integer > "-INF"^^xsd:double AS ?gt_ninf)
            ("99999999999999999999999"^^xsd:integer = "NaN"^^xsd:double AS ?eq_nan)
            ("99999999999999999999999"^^xsd:integer != "NaN"^^xsd:double AS ?ne_nan)
            ("99999999999999999999999"^^xsd:integer < "NaN"^^xsd:double AS ?lt_nan)
            ("99999999999999999999999"^^xsd:integer > "1.0e10"^^xsd:float AS ?gt_flt)
            ("0.0000000000000000001"^^xsd:decimal > 0 AS ?tiny_gt_0)
            ("-0.0000000000000000001"^^xsd:decimal < 0 AS ?ntiny_lt_0)
        WHERE {}"#,
    );
    let bools: Vec<&str> = (0..11)
        .map(|i| match cmp_rows[0][i].as_ref().unwrap() {
            Term::Literal(l) => l.value(),
            _ => panic!("expected bool"),
        })
        .collect();
    assert_eq!(
        bools,
        vec![
            "true", "false", "true", "true", "true", "false", "true", "false", "true", "true",
            "true"
        ]
    );
}

#[test]
fn arithmetic_unary_rounding_casts_and_ebv() {
    let s = Store::in_memory(StoreOptions::default());

    let rows = run_both(
        &s,
        r#"SELECT
            ("99999999999999999999999"^^xsd:integer + 1 AS ?add)
            ("99999999999999999999999"^^xsd:integer - "99999999999999999999990"^^xsd:integer AS ?sub_small)
            (9223372036854775807 + 1 AS ?i64_ovf)
            (1000000000000 * 1000000000000 AS ?mul_ovf)
            ("100000000000000000000000"^^xsd:integer / 4 AS ?div_int)
            (-("99999999999999999999999"^^xsd:integer) AS ?neg)
            (+("99999999999999999999999"^^xsd:integer) AS ?pos)
            (ABS("-99999999999999999999999"^^xsd:integer) AS ?abs_big)
            (ABS(-9223372036854775808) AS ?abs_min)
            (ROUND("99999999999999999999999.5"^^xsd:decimal) AS ?rnd_pos)
            (ROUND("-99999999999999999999999.5"^^xsd:decimal) AS ?rnd_neg)
            (CEIL("99999999999999999999999.1"^^xsd:decimal) AS ?ceil_pos)
            (FLOOR("-99999999999999999999999.1"^^xsd:decimal) AS ?floor_neg)
            (fn:numeric-integer-divide("100000000000000000000000"^^xsd:integer, 3) AS ?idiv)
            (fn:numeric-mod("100000000000000000000000"^^xsd:integer, 3) AS ?imod)
            (afn:evenInteger("100000000000000000000000"^^xsd:integer) AS ?even)
        WHERE {}"#,
    );
    let r = &rows[0];
    assert_eq!(
        lit_val(r, 0),
        Some((
            "100000000000000000000000".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 1),
        Some(("9".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(r, 2),
        Some(("9223372036854775808".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(r, 3),
        Some((
            "1000000000000000000000000".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 4),
        Some((
            "25000000000000000000000".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 5),
        Some((
            "-99999999999999999999999".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 6),
        Some((
            "99999999999999999999999".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 7),
        Some((
            "99999999999999999999999".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 8),
        Some(("9223372036854775808".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(r, 9),
        Some((
            "100000000000000000000000".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 10),
        Some((
            "-99999999999999999999999".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 11),
        Some((
            "100000000000000000000000".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 12),
        Some((
            "-100000000000000000000000".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 13),
        Some((
            "33333333333333333333333".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(r, 14),
        Some(("1".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(r, 15),
        Some(("true".into(), xsd::BOOLEAN.as_str().into()))
    );

    // Casts & EBV (including tiny decimals that would underflow f64 to 0.0).
    let tiny400 = format!("0.{}1", "0".repeat(399));
    let cast_q = format!(
        r#"SELECT
            (xsd:integer("99999999999999999999999.9"^^xsd:decimal) AS ?dec_to_int)
            (xsd:decimal("99999999999999999999999"^^xsd:integer) AS ?int_to_dec)
            (xsd:string("99999999999999999999999"^^xsd:integer) AS ?int_to_str)
            (xsd:boolean("99999999999999999999999"^^xsd:integer) AS ?int_to_bool)
            (xsd:boolean("{tiny400}"^^xsd:decimal) AS ?tiny_to_bool)
            (xsd:double("100000000000000000000000"^^xsd:integer) AS ?int_to_dbl)
            (xsd:float("100000000000000000000000"^^xsd:integer) AS ?int_to_flt)
            (xsd:nonNegativeInteger("99999999999999999999999") AS ?str_to_nn)
            (xsd:unsignedLong("18446744073709551615") AS ?str_to_ul)
            (xsd:unsignedLong("18446744073709551616") AS ?bad_ul)
            (xsd:integer(1.0e25) AS ?dbl_to_int)
            (xsd:decimal(1.0e-25) AS ?dbl_to_dec)
        WHERE {{}}"#
    );
    let crows = run_both(&s, &cast_q);
    let c = &crows[0];
    assert_eq!(
        lit_val(c, 0),
        Some((
            "99999999999999999999999".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(c, 1),
        Some((
            "99999999999999999999999".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(c, 2),
        Some((
            "99999999999999999999999".into(),
            xsd::STRING.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(c, 3),
        Some(("true".into(), xsd::BOOLEAN.as_str().into()))
    );
    assert_eq!(
        lit_val(c, 4),
        Some(("true".into(), xsd::BOOLEAN.as_str().into()))
    );
    assert_eq!(
        lit_val(c, 5).map(|(_, dt)| dt),
        Some(xsd::DOUBLE.as_str().into())
    );
    assert_eq!(
        lit_val(c, 6).map(|(_, dt)| dt),
        Some(xsd::FLOAT.as_str().into())
    );
    assert_eq!(
        lit_val(c, 7),
        Some((
            "99999999999999999999999".into(),
            "http://www.w3.org/2001/XMLSchema#nonNegativeInteger".into()
        ))
    );
    assert_eq!(
        lit_val(c, 8),
        Some((
            "18446744073709551615".into(),
            "http://www.w3.org/2001/XMLSchema#unsignedLong".into()
        ))
    );
    assert!(c[9].is_none(), "overflowing unsignedLong cast must error");
    assert_eq!(
        lit_val(c, 10),
        Some((
            "10000000000000000905969664".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(c, 11),
        Some((
            "0.0000000000000000000000001".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
}

#[test]
fn decimal_avg_matches_arq_in_eager_and_streaming_groups() {
    let store = Store::in_memory(StoreOptions::default());
    for (values, expected) in [
        ("1.0 0 0", "0.333333333333333333333333"),
        ("-1.0 -1 0", "-0.666666666666666666666667"),
        ("1 0 0", "0.333333333333333333333333"),
        (
            "0.000000000000000000000000001 0",
            "0.0000000000000000000000000005",
        ),
        ("0.000000000000000000000000001 0 0", "0"),
        (
            "100000000000000000000.0 100000000000000000000.0 0",
            "66666666666666666666.666666666666666666666667",
        ),
    ] {
        let query = format!("SELECT (AVG(?v) AS ?avg) {{ VALUES ?v {{ {values} }} }}");
        let answer = run_both(&store, &query);
        assert_eq!(answer.len(), 1, "{values}");
        assert_eq!(
            lit_val(&answer[0], 0),
            Some((expected.into(), xsd::DECIMAL.as_str().into())),
            "{values}"
        );
    }

    let answer = run_both(
        &store,
        "SELECT (AVG(DISTINCT ?v) AS ?avg) { VALUES ?v { 0 0 2 -1.0 } }",
    );
    assert_eq!(
        lit_val(&answer[0], 0),
        Some((
            "0.333333333333333333333333".into(),
            xsd::DECIMAL.as_str().into(),
        ))
    );
}

#[test]
fn aggregates_sum_avg_min_max_on_big_numbers() {
    let s = store_from_ttl(
        r#"
        @prefix ex: <http://example.org/> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        ex:g1 ex:v "99999999999999999999999"^^xsd:integer .
        ex:g1 ex:v 1 .
        ex:g2 ex:v "99999999999999999999999999999.25"^^xsd:decimal .
        ex:g2 ex:v "0.75"^^xsd:decimal .
        ex:g3 ex:item ex:i1, ex:i2 .
        ex:i1 ex:v 9223372036854775807 .
        ex:i2 ex:v 9223372036854775807 .
    "#,
    );

    let rows = run_both(
        &s,
        "SELECT ?g (SUM(?v) AS ?sum) (AVG(?v) AS ?avg) (MIN(?v) AS ?min) (MAX(?v) AS ?max) \
         WHERE { { ?g ex:v ?v FILTER(?g != ex:i1 && ?g != ex:i2) } UNION { ?g ex:item ?item . ?item ex:v ?v } } GROUP BY ?g ORDER BY ?g",
    );
    assert_eq!(rows.len(), 3);
    // g1: 99999999999999999999999 + 1 = 100000000000000000000000, avg = 50000000000000000000000
    assert_eq!(
        lit_val(&rows[0], 1),
        Some((
            "100000000000000000000000".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(&rows[0], 2),
        Some((
            "50000000000000000000000".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(&rows[0], 3),
        Some(("1".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(&rows[0], 4),
        Some((
            "99999999999999999999999".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    // g2: big decimal + small decimal = 100000000000000000000000000000, avg = 50000000000000000000000000000
    assert_eq!(
        lit_val(&rows[1], 1),
        Some((
            "100000000000000000000000000000".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(&rows[1], 2),
        Some((
            "50000000000000000000000000000".into(),
            xsd::DECIMAL.as_str().into()
        ))
    );
    // g3: two i64::MAX values sum without overflowing to unbound
    assert_eq!(
        lit_val(&rows[2], 1),
        Some(("18446744073709551614".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(&rows[2], 2),
        Some(("9223372036854775807".into(), xsd::DECIMAL.as_str().into()))
    );
}

#[test]
fn dos_bounds_reject_pathological_digit_counts_cleanly() {
    let s = Store::in_memory(StoreOptions::default());
    let huge_lit = "9".repeat(5000);
    let q = format!(
        "SELECT (isNumeric(\"{huge_lit}\"^^xsd:integer) AS ?is_int) \
                (isNumeric(\"0.{huge_lit}\"^^xsd:decimal) AS ?is_dec) \
         WHERE {{}}"
    );
    let rows = run_both(&s, &q);
    assert_eq!(lit_val(&rows[0], 0).unwrap().0, "false");
    assert_eq!(lit_val(&rows[0], 1).unwrap().0, "false");

    // Repeated squaring that exceeds MAX_DIGITS yields an expression error (unbound),
    // never panicking or exhausting memory.
    let big_3000 = format!("1{}", "0".repeat(2500));
    let q_mul = format!(
        "SELECT (\"{big_3000}\"^^xsd:integer * \"{big_3000}\"^^xsd:integer AS ?p) WHERE {{}}"
    );
    let rows = run_both(&s, &q_mul);
    assert!(rows[0][0].is_none());
}

#[test]
fn legacy_spkvnum3_file_is_ignored_so_big_integers_are_not_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let ttl = r#"
        @prefix ex: <http://example.org/> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        ex:a ex:v "99999999999999999999999"^^xsd:integer .
        ex:b ex:v "42"^^xsd:int .
    "#;
    s.load(&[Source::from_bytes(
        ttl.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    drop(s);

    // Overwrite vocab.num magic from SPKVNUM4 to SPKVNUM3 to simulate an older generation
    // whose numeric column marked out-of-range integers as NOT_NUMERIC.
    let mut patched = false;
    for entry in fs::read_dir(dir.path()).unwrap() {
        let p = entry.unwrap().path().join("vocab.num");
        if p.exists() {
            let mut bytes = fs::read(&p).unwrap();
            assert_eq!(&bytes[..8], b"SPKVNUM4");
            bytes[..8].copy_from_slice(b"SPKVNUM3");
            fs::write(&p, bytes).unwrap();
            patched = true;
        }
    }
    assert!(patched, "expected vocab.num in compacted generation");

    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    assert!(
        s.snapshot().generation.vocab.numeric().is_none(),
        "stale SPKVNUM3 vocab.num must be ignored"
    );
    let rows = run_both(&s, "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v > 150000) }");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0][0].as_ref().unwrap(),
        &Term::NamedNode(NamedNode::new_unchecked("http://example.org/a"))
    );

    // `Store::add_numeric_column` (`sparkles vocab-index`) must upgrade a stale SPKVNUM3
    // file to SPKVNUM4 rather than refusing because the stale file exists on disk.
    let rebuilt = s.add_numeric_column().unwrap();
    assert!(rebuilt.is_some(), "stale SPKVNUM3 should be rebuilt");
    assert_eq!(s.add_numeric_column().unwrap(), None);
    drop(s);
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    assert!(
        s.snapshot().generation.vocab.numeric().is_some(),
        "rebuilt SPKVNUM4 vocab.num should load on reopen"
    );
    let rows = run_both(&s, "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v > 150000) }");
    assert_eq!(rows.len(), 1);
}

#[test]
fn knuth_algorithm_d_qhat_base_plus_one_sparql_idiv_and_mod() {
    let s = Store::in_memory(StoreOptions::default());
    let rows = run_both(
        &s,
        "SELECT ?q ?r WHERE { \
           BIND(fn:numeric-integer-divide(\"500000000999999998000000000\"^^xsd:integer, \
                                          \"500000000999999999\"^^xsd:integer) AS ?q) \
           BIND(fn:numeric-mod(\"500000000999999998000000000\"^^xsd:integer, \
                               \"500000000999999999\"^^xsd:integer) AS ?r) \
         }",
    );
    assert_eq!(
        lit_val(&rows[0], 0),
        Some(("999999999".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(&rows[0], 1),
        Some(("499999999999999999".into(), xsd::INTEGER.as_str().into()))
    );
}

#[test]
fn order_by_top_k_is_transitive_with_repeated_mixed_numeric_keys() {
    let s = Store::in_memory(StoreOptions::default());
    // Repeated keys in sequence C, A, B, A across deduplicated key column:
    // A = 10^22 - 1 (integer), B = 10^22.0 (decimal), C = 1e22 (double).
    let ttl = r#"
        @prefix ex: <http://example.org/> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        ex:r1 ex:k "1e22"^^xsd:double .
        ex:r2 ex:k "9999999999999999999999"^^xsd:integer .
        ex:r3 ex:k "10000000000000000000000.0"^^xsd:decimal .
        ex:r4 ex:k "9999999999999999999999"^^xsd:integer .
        ex:r5 ex:k "1e22"^^xsd:double .
        ex:r6 ex:k "10000000000000000000000.0"^^xsd:decimal .
    "#;
    s.load(&[Source::from_bytes(
        ttl.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();

    // Ascending LIMIT 1 must pick A (10^22 - 1), matching full sort.
    let top1 = run_both(
        &s,
        "SELECT ?k WHERE { ?s ex:k ?k } ORDER BY ASC(?k) LIMIT 1",
    );
    assert_eq!(
        lit_val(&top1[0], 0),
        Some((
            "9999999999999999999999".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    let full = run_both(&s, "SELECT ?k WHERE { ?s ex:k ?k } ORDER BY ASC(?k)");
    assert_eq!(full[0][0], top1[0][0]);
    assert_eq!(full[1][0], top1[0][0]);

    // FILTER equality still uses XPath numeric promotion (A = C is true).
    let eq = run_both(
        &s,
        "SELECT ?eq WHERE { \
           BIND((\"9999999999999999999999\"^^xsd:integer = \"1e22\"^^xsd:double) AS ?eq) \
         }",
    );
    assert_eq!(
        lit_val(&eq[0], 0),
        Some(("true".into(), xsd::BOOLEAN.as_str().into()))
    );
}

#[test]
fn terminating_and_bounded_extreme_decimal_division_mod_and_idiv() {
    let s = Store::in_memory(StoreOptions::default());
    // 1 / 2^100 preserves exact scale 100.
    let r = run_both(
        &s,
        "SELECT ?prod WHERE { \
           BIND((1 / \"1267650600228229401496703205376\"^^xsd:integer) AS ?q) \
           BIND((?q * \"1267650600228229401496703205376\"^^xsd:integer) AS ?prod) \
         }",
    );
    assert_eq!(
        lit_val(&r[0], 0),
        Some(("1".into(), xsd::DECIMAL.as_str().into()))
    );

    // Fixed-width i64 operands with terminating quotient beyond 18 fractional digits: 1 / 2^60.
    let r = run_both(
        &s,
        "SELECT ?q ?prod WHERE { \
           BIND((1 / 1152921504606846976) AS ?q) \
           BIND((?q * 1152921504606846976) AS ?prod) \
         }",
    );
    assert_ne!(lit_val(&r[0], 0).unwrap().0, "0");
    assert_eq!(
        lit_val(&r[0], 1),
        Some(("1".into(), xsd::DECIMAL.as_str().into()))
    );

    // 10^-18 / 2 preserves exact 0.0000000000000000005.
    let r = run_both(
        &s,
        "SELECT ?q WHERE { BIND((\"0.000000000000000001\"^^xsd:decimal / 2) AS ?q) }",
    );
    assert_eq!(
        lit_val(&r[0], 0),
        Some(("0.0000000000000000005".into(), xsd::DECIMAL.as_str().into()))
    );

    // 10^-4096 / 1 succeeds, 100 / (3 + 10^-4095) succeeds, and 10^4095 mod 10^-4096 == 0,
    // 10^-4096 idiv 10^4095 == 0.
    let tiny = format!("0.{}1", "0".repeat(4_095));
    let big = format!("1{}", "0".repeat(4_095));
    let div_hi = format!("3.{}1", "0".repeat(4_094));
    let q = format!(
        "SELECT ?t ?h ?m ?id WHERE {{ \
           BIND((\"{tiny}\"^^xsd:decimal / 1) AS ?t) \
           BIND((100 / \"{div_hi}\"^^xsd:decimal) AS ?h) \
           BIND(fn:numeric-mod(\"{big}\"^^xsd:decimal, \"{tiny}\"^^xsd:decimal) AS ?m) \
           BIND(fn:numeric-integer-divide(\"{tiny}\"^^xsd:decimal, \"{big}\"^^xsd:decimal) AS ?id) \
         }}"
    );
    let r = run_both(&s, &q);
    assert_eq!(
        lit_val(&r[0], 0),
        Some((tiny, xsd::DECIMAL.as_str().into()))
    );
    assert_eq!(
        lit_val(&r[0], 1),
        Some(("33.333333333333333333".into(), xsd::DECIMAL.as_str().into()))
    );
    assert_eq!(
        lit_val(&r[0], 2),
        Some(("0".into(), xsd::DECIMAL.as_str().into()))
    );
    assert_eq!(
        lit_val(&r[0], 3),
        Some(("0".into(), xsd::INTEGER.as_str().into()))
    );
}

#[test]
fn sum_and_avg_enforce_cap_after_cancellation_and_averaging() {
    let s = Store::in_memory(StoreOptions::default());
    let x = "9".repeat(4_096);
    let ttl = format!(
        "@prefix ex: <http://example.org/> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
         ex:a1 ex:g ex:cancel1 ; ex:v \"{x}\"^^xsd:integer .\n\
         ex:a2 ex:g ex:cancel1 ; ex:v \"{x}\"^^xsd:integer .\n\
         ex:a3 ex:g ex:cancel1 ; ex:v \"-{x}\"^^xsd:integer .\n\
         ex:b1 ex:g ex:cancel2 ; ex:v \"{x}\"^^xsd:integer .\n\
         ex:b2 ex:g ex:cancel2 ; ex:v \"-{x}\"^^xsd:integer .\n\
         ex:b3 ex:g ex:cancel2 ; ex:v \"{x}\"^^xsd:integer .\n\
         ex:c1 ex:g ex:avg_ok ; ex:v \"{x}\"^^xsd:integer .\n\
         ex:c2 ex:g ex:avg_ok ; ex:v \"{x}\"^^xsd:integer .\n"
    );
    s.load(&[Source::from_bytes(
        ttl.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();

    let rows = run_both(
        &s,
        "SELECT ?g (SUM(?v) AS ?sum) (AVG(?v) AS ?avg) \
         WHERE { ?s ex:g ?g ; ex:v ?v } GROUP BY ?g ORDER BY ?g",
    );
    assert_eq!(rows.len(), 3);
    // avg_ok: SUM(X, X) exceeds 4096 digits -> unbound; AVG(X, X) == X!
    assert!(rows[0][1].is_none(), "SUM(X, X) should exceed cap");
    assert_eq!(
        lit_val(&rows[0], 2),
        Some((x.clone(), xsd::DECIMAL.as_str().into()))
    );
    // cancel1 and cancel2: SUM(X, X, -X) == X regardless of row order.
    assert_eq!(
        lit_val(&rows[1], 1),
        Some((x.clone(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(&rows[2], 1),
        Some((x, xsd::INTEGER.as_str().into()))
    );
}

#[test]
fn round_extreme_precision_and_direct_float_to_integer_truncation() {
    let s = Store::in_memory(StoreOptions::default());
    let rows = run_both(
        &s,
        "SELECT ?r1 ?r2 ?r3 ?c1 ?c2 ?c3 WHERE { \
           BIND(fn:round-half-to-even(\"99999999999999999999999\"^^xsd:integer, 9223372036854775807) AS ?r1) \
           BIND(fn:round(\"99999999999999999999999\"^^xsd:integer, -9223372036854775808) AS ?r2) \
           BIND(fn:round-half-to-even(42, 9223372036854775807) AS ?r3) \
           BIND(xsd:integer(\"1000000000000000100\"^^xsd:double) AS ?c1) \
           BIND(xsd:integer(\"-9223372036854775808.0\"^^xsd:double) AS ?c2) \
           BIND(xsd:integer(\"9223372036854775808.0\"^^xsd:double) AS ?c3) \
         }",
    );
    assert_eq!(
        lit_val(&rows[0], 0),
        Some((
            "99999999999999999999999".into(),
            xsd::INTEGER.as_str().into()
        ))
    );
    assert_eq!(
        lit_val(&rows[0], 1),
        Some(("0".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(&rows[0], 2),
        Some(("42".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(&rows[0], 3),
        Some(("1000000000000000128".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(&rows[0], 4),
        Some(("-9223372036854775808".into(), xsd::INTEGER.as_str().into()))
    );
    assert_eq!(
        lit_val(&rows[0], 5),
        Some(("9223372036854775808".into(), xsd::INTEGER.as_str().into()))
    );
}
