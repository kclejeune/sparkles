//! Executor optimizations: each test checks that the optimized operator is chosen (by its
//! EXPLAIN name) and that it returns exactly the answer of the generic operators.

use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

fn load(s: &Store, text: &str, format: RdfFormat) {
    s.load(&[Source::from_bytes(text.as_bytes().to_vec(), format, None)])
        .unwrap();
}

fn update(s: &Store, text: &str) {
    super::update::update(s, &format!("{PREFIXES}{text}"), &QueryOptions::default()).unwrap();
}

fn run(s: &Store, text: &str, opt: Optimizations) -> QueryResult {
    let opts = QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    };
    let text = format!("{PREFIXES}{text}");
    query(s.snapshot(), &text, &opts).unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// Solutions as exact RDF terms, sorted (a multiset).
fn solutions(r: &QueryResult) -> Vec<String> {
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    v.sort();
    v
}

fn has_op(p: &PlanInfo, op: &str) -> bool {
    p.operator == op || p.children.iter().any(|c| has_op(c, op))
}

fn has_desc(p: &PlanInfo, needle: &str) -> bool {
    p.description.contains(needle) || p.children.iter().any(|c| has_desc(c, needle))
}

/// Run `text` with every optimization and with none; the answers must be equal and the
/// optimized plan must contain `op` (an operator name, or `desc:` + description text).
fn same_answer(s: &Store, text: &str, op: &str) -> Vec<String> {
    let fast = run(s, text, Optimizations::ALL);
    let slow = run(s, text, Optimizations::NONE);
    let (a, b) = (solutions(&fast), solutions(&slow));
    assert_eq!(a, b, "{text}");
    let taken = match op.strip_prefix("desc:") {
        Some(d) => has_desc(&fast.plan, d),
        None => has_op(&fast.plan, op),
    };
    assert!(taken, "{text}: optimized plan lacks {op}: {:#?}", fast.plan);
    let generic = match op.strip_prefix("desc:") {
        Some(d) => has_desc(&slow.plan, d),
        None => has_op(&slow.plan, op),
    };
    assert!(!generic, "{text}: generic plan uses {op}");
    a
}

/// Rows of `text` with every optimization and with none, without checking the plan.
fn same_rows(s: &Store, text: &str) -> Vec<String> {
    let a = solutions(&run(s, text, Optimizations::ALL));
    assert_eq!(a, solutions(&run(s, text, Optimizations::NONE)), "{text}");
    a
}

// ------------------------------------------------------------ range pushdown ------

/// Values of every kind, including every inline numeric encoding and its boundaries,
/// non-canonical numerals (vocabulary literals) and non-numeric terms.
fn mixed_values() -> Vec<String> {
    let mut v: Vec<String> = [
        "0",
        "-0",
        "1",
        "-1",
        "7",
        "150000",
        "149999",
        "150001",
        "-150000",
        "576460752303423487",
        "-576460752303423488",
        "576460752303423488",
        "-576460752303423489",
        "99999999999999999999999",
        "\"0012\"^^xsd:integer",
        "\"+5\"^^xsd:integer",
        "1.5",
        "-1.5",
        "150000.5",
        "149999.99",
        "150000.01",
        "0.000001",
        "-0.000001",
        "123456789012.123",
        "\"150000.50\"^^xsd:decimal",
        "\"150000.0\"^^xsd:decimal",
        "\"1.50\"^^xsd:decimal",
        "\"-2.10\"^^xsd:decimal",
        "1.5e0",
        "-2.5e3",
        "1.5e5",
        "\"NaN\"^^xsd:double",
        "\"INF\"^^xsd:double",
        "\"-INF\"^^xsd:double",
        "\"0.1\"^^xsd:double",
        "\"2.5\"^^xsd:float",
        "\"150000\"^^xsd:int",
        "\"-3\"^^xsd:short",
        "\"12\"^^xsd:nonNegativeInteger",
        "\"abc\"",
        "\"150001\"",
        "\"5\"@en",
        "true",
        "false",
        "\"2024-01-01\"^^xsd:date",
        "\"2024-01-01T00:00:00Z\"^^xsd:dateTime",
        "\"x\"^^xsd:integer",
        "ex:iri",
        "[]",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // decimals of every inline scale
    for scale in 0..16 {
        v.push(format!("{}.{}1", 150000 - scale, "0".repeat(scale)));
        v.push(format!("-{}.{}3", scale, "0".repeat(scale)));
    }
    v
}

fn range_store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for (i, v) in mixed_values().iter().enumerate() {
        ttl.push_str(&format!("ex:s{i} ex:v {v} .\n"));
    }
    // enough plain integers and decimals for several blocks
    for i in 0..70_000 {
        ttl.push_str(&format!(
            "ex:n{i} ex:v {} .\n",
            (i * 37) % 300_001 - 150_000
        ));
        if i % 3 == 0 {
            ttl.push_str(&format!("ex:n{i} ex:v {}.{} .\n", i * 5, i % 97 + 1));
        }
    }
    load(&s, &ttl, RdfFormat::Turtle);
    s
}

const THRESHOLDS: &[&str] = &[
    "150000",
    "-150000",
    "0",
    "-1",
    "150000.5",
    "150000.005",
    "1.5e5",
    "\"2.5\"^^xsd:float",
    "\"NaN\"^^xsd:double",
    "\"INF\"^^xsd:double",
    "576460752303423487",
    "-576460752303423488",
    "99999999999999999999999",
    "\"150000.50\"^^xsd:decimal",
];

/// Same answer with and without optimizations; whether the range scan was chosen (the
/// planner keeps the plain scan when the range covers most rows).
fn range_check(s: &Store, text: &str) -> bool {
    let fast = run(s, text, Optimizations::ALL);
    assert_eq!(
        solutions(&fast),
        solutions(&run(s, text, Optimizations::NONE)),
        "{text}"
    );
    has_op(&fast.plan, "IndexRangeScan")
}

#[test]
fn range_filters_read_only_matching_id_ranges() {
    let s = range_store();
    let mut pushed = 0;
    let mut total = 0;
    for c in THRESHOLDS {
        for op in [">", ">=", "<", "<=", "="] {
            let text = format!("SELECT ?s ?v WHERE {{ ?s ex:v ?v FILTER(?v {op} {c}) }}");
            pushed += range_check(&s, &text) as usize;
            // constant on the left
            let text = format!("SELECT ?s ?v WHERE {{ ?s ex:v ?v FILTER({c} {op} ?v) }}");
            pushed += range_check(&s, &text) as usize;
            total += 2;
        }
    }
    assert!(
        pushed * 2 > total,
        "range scan chosen {pushed} of {total} times"
    );
    // selective ranges are always pushed down
    for text in [
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v > 150000) }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v <= -149999.5) }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v = 7) }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(-150000 >= ?v) }",
    ] {
        same_answer(&s, text, "IndexRangeScan");
    }
    // two-sided ranges, with a conjunct that is not pushed
    let rows = same_answer(
        &s,
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v > -2 && ?v <= 1.5 && ?v != 0) }",
        "IndexRangeScan",
    );
    assert!(!rows.is_empty());
    same_answer(
        &s,
        "SELECT ?v WHERE { ?s ex:v ?v FILTER(?v >= 149999.99 && ?v < 150001) } ORDER BY DESC(?v) LIMIT 7",
        "IndexRangeScan",
    );
}

#[test]
fn range_filters_see_updates() {
    let s = range_store();
    update(
        &s,
        "INSERT DATA { ex:new1 ex:v 150002 . ex:new2 ex:v \"150003.0\"^^xsd:decimal . ex:new3 ex:v 2.25 . ex:new4 ex:v \"zzz\" }",
    );
    update(&s, "DELETE DATA { ex:s5 ex:v 150000 . ex:n3 ex:v 14889 }");
    for c in THRESHOLDS {
        for op in [">", "<", "="] {
            range_check(
                &s,
                &format!("SELECT ?s ?v WHERE {{ ?s ex:v ?v FILTER(?v {op} {c}) }}"),
            );
        }
    }
    let rows = same_answer(
        &s,
        "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v > 150001) }",
        "IndexRangeScan",
    );
    assert!(rows.iter().any(|r| r.contains("new1")));
    assert!(rows.iter().any(|r| r.contains("new2")));
    let rows = same_answer(
        &s,
        "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v = 150000) }",
        "IndexRangeScan",
    );
    assert!(!rows.iter().any(|r| r.contains("<http://ex.org/s5>")));
}

#[test]
fn range_filters_in_joins_and_graphs() {
    let s = Store::in_memory(StoreOptions::default());
    let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..5000 {
        trig.push_str(&format!(
            "ex:g{} {{ ex:p{i} ex:age {} ; ex:name \"n{i}\" . }}\n",
            i % 3,
            i % 90
        ));
        trig.push_str(&format!("ex:p{i} ex:age {} .\n", i % 70));
    }
    load(&s, &trig, RdfFormat::TriG);
    same_answer(
        &s,
        "SELECT ?p ?a WHERE { ?p ex:age ?a FILTER(?a >= 85) }",
        "IndexRangeScan",
    );
    same_rows(
        &s,
        "SELECT ?p ?a ?n WHERE { GRAPH ex:g1 { ?p ex:age ?a ; ex:name ?n FILTER(?a < 3) } }",
    );
    same_answer(
        &s,
        "SELECT ?p ?a WHERE { GRAPH ex:g1 { ?p ex:age ?a FILTER(?a < 3) } }",
        "IndexRangeScan",
    );
    same_rows(
        &s,
        "SELECT ?g (COUNT(*) AS ?c) WHERE { GRAPH ?g { ?p ex:age ?a } FILTER(?a > 80) } GROUP BY ?g",
    );
    same_rows(
        &s,
        "SELECT ?p WHERE { GRAPH <urn:x-arq:UnionGraph> { ?p ex:age ?a } FILTER(?a > 88) }",
    );
}

#[test]
fn non_numeric_range_filters_are_not_pushed() {
    let s = range_store();
    let r = run(
        &s,
        "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v > \"abc\") }",
        Optimizations::ALL,
    );
    assert!(!has_op(&r.plan, "IndexRangeScan"));
    let r = run(
        &s,
        "SELECT ?s WHERE { ?s ex:v ?v FILTER(?v > \"2020-01-01\"^^xsd:date) }",
        Optimizations::ALL,
    );
    assert!(!has_op(&r.plan, "IndexRangeScan"));
}

/// Pseudo-random ranges over pseudo-random mixed values.
#[test]
fn range_filters_match_the_generic_filter_on_random_data() {
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 0..20_000 {
        let r = next();
        let v = match r % 6 {
            0 => format!("{}", (r >> 8) as i64 % 1000),
            1 => format!("{}.{}", (r >> 8) as i64 % 1000, (r >> 40) % 1000),
            2 => format!("\"{}.{}0\"^^xsd:decimal", (r >> 8) % 1000, (r >> 40) % 10),
            3 => format!("{}e-1", (r >> 8) as i64 % 10_000),
            4 => format!("\"{}\"^^xsd:long", (r >> 8) as i64 % 1000),
            _ => format!("\"{}\"", (r >> 8) % 1000),
        };
        ttl.push_str(&format!("ex:s{i} ex:v {v} .\n"));
    }
    load(&s, &ttl, RdfFormat::Turtle);
    for _ in 0..25 {
        let (a, b) = ((next() % 2000) as i64 - 1000, (next() % 2000) as i64 - 1000);
        let text = format!(
            "SELECT ?s ?v WHERE {{ ?s ex:v ?v FILTER(?v >= {}.{} && ?v < {b}) }}",
            a.min(b),
            next() % 100
        );
        same_answer(&s, &text, "IndexRangeScan");
    }
}

// ------------------------------------------------------ incremental grouping ------

fn group_store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 0..20_000 {
        ttl.push_str(&format!("ex:p{i} ex:org ex:o{} .\n", i % 37));
        match i % 11 {
            0 => {} // no age: unbound under OPTIONAL
            1 => ttl.push_str(&format!("ex:p{i} ex:age {}.5 .\n", i % 60)),
            2 => ttl.push_str(&format!("ex:p{i} ex:age \"{}\"^^xsd:double .\n", i % 60)),
            3 => ttl.push_str(&format!("ex:p{i} ex:age \"0{}\"^^xsd:integer .\n", i % 60)),
            _ => ttl.push_str(&format!("ex:p{i} ex:age {} .\n", i % 60)),
        }
        // a few non-numeric values in some groups (SUM / AVG become errors there)
        if i % 1000 == 7 {
            ttl.push_str(&format!("ex:p{i} ex:age \"old\" .\n"));
        }
        // large integers: sums overflow i64 in their group
        if i % 37 == 5 && i < 3000 {
            ttl.push_str(&format!("ex:p{i} ex:big 576460752303423000 .\n"));
        }
        if i % 37 == 6 {
            ttl.push_str(&format!("ex:p{i} ex:big {} .\n", i));
        }
    }
    load(&s, &ttl, RdfFormat::Turtle);
    s
}

#[test]
fn incremental_grouping_matches_the_generic_aggregates() {
    let s = group_store();
    for q in [
        "SELECT ?o (AVG(?a) AS ?avg) (COUNT(?p) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o",
        "SELECT ?o (SUM(?a) AS ?s) (COUNT(?a) AS ?c) (COUNT(*) AS ?n) (MIN(?a) AS ?lo) (MAX(?a) AS ?hi) (SAMPLE(?p) AS ?x) WHERE { ?p ex:org ?o OPTIONAL { ?p ex:age ?a } } GROUP BY ?o",
        "SELECT ?o (SUM(?b) AS ?s) (AVG(?b) AS ?m) WHERE { ?p ex:org ?o ; ex:big ?b } GROUP BY ?o",
        "SELECT (SUM(?a) AS ?s) (AVG(?a) AS ?m) (COUNT(*) AS ?n) (MAX(?a) AS ?hi) WHERE { ?p ex:age ?a }",
        "SELECT (SUM(?a) AS ?s) (AVG(?a) AS ?m) (COUNT(*) AS ?n) (MIN(?a) AS ?lo) WHERE { ?p ex:age ?a FILTER(?a > 1000) }",
        "SELECT ?a (COUNT(?p) AS ?n) (MAX(?p) AS ?m) WHERE { ?p ex:age ?a } GROUP BY ?a",
        "SELECT ?o (AVG(?a) AS ?avg) WHERE { ?p ex:org ?o OPTIONAL { ?p ex:age ?a } } GROUP BY ?o HAVING (COUNT(?a) > 400)",
    ] {
        same_answer(&s, q, "desc:[incremental]");
    }
    // not admitted: DISTINCT, expressions, GROUP_CONCAT, several keys
    for q in [
        "SELECT ?o (COUNT(DISTINCT ?a) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o",
        "SELECT ?o (SUM(?a * 2) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o",
        "SELECT ?o (GROUP_CONCAT(?a) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o",
        "SELECT ?o ?a (COUNT(*) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o ?a",
    ] {
        let r = run(&s, q, Optimizations::ALL);
        assert!(!has_desc(&r.plan, "[incremental]"), "{q}");
        same_rows(&s, q);
    }
}

// --------------------------------------------------------- count join runs ------

#[test]
fn count_joins_from_key_runs() {
    let s = Store::in_memory(StoreOptions::default());
    let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..3000 {
        trig.push_str(&format!(
            "ex:a{i} ex:knows ex:a{} , ex:a{} .\n",
            (i * 7) % 3000,
            (i + 1) % 3000
        ));
        if i % 5 == 0 {
            // the same triple in two named graphs and the default graph
            trig.push_str(&format!(
                "ex:g1 {{ ex:a{i} ex:knows ex:a{} . }}\n",
                (i + 1) % 3000
            ));
            trig.push_str(&format!(
                "ex:g2 {{ ex:a{i} ex:knows ex:a{} . }}\n",
                (i + 1) % 3000
            ));
        }
        if i % 97 == 0 {
            trig.push_str(&format!("ex:a{i} ex:knows ex:a{i} .\n"));
        }
    }
    load(&s, &trig, RdfFormat::TriG);
    let two_hop = "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:knows ?b . ?b ex:knows ?c }";
    let n = same_answer(&s, two_hop, "CountJoinFromRuns");
    assert_eq!(n.len(), 1);
    for q in [
        "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:knows ?b . ?c ex:knows ?b }",
        "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:knows ?a . ?a ex:knows ?c }",
        "SELECT (COUNT(*) AS ?n) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?a ex:knows ?b . ?b ex:knows ?c } }",
        "SELECT (COUNT(*) AS ?n) WHERE { GRAPH ex:g1 { ?a ex:knows ?b . ?b ex:knows ?c } }",
    ] {
        same_answer(&s, q, "CountJoinFromRuns");
    }
    // a join on two variables, or with a third shared variable, is not admitted
    let q = "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:knows ?b . ?b ex:knows ?a }";
    assert!(!has_op(
        &run(&s, q, Optimizations::ALL).plan,
        "CountJoinFromRuns"
    ));
    same_rows(&s, q);
    // updates are visible
    update(
        &s,
        "INSERT DATA { ex:x ex:knows ex:a1 . ex:a2 ex:knows ex:y }",
    );
    update(&s, "DELETE DATA { ex:a3 ex:knows ex:a4 }");
    same_answer(&s, two_hop, "CountJoinFromRuns");
}

// ---------------------------------------------------------- metadata counts ------

#[test]
fn class_counts_from_statistics_only_when_exact() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..5000 {
        ttl.push_str(&format!("ex:s{i} a ex:C{} .\n", i % 13));
        if i % 4 == 0 {
            ttl.push_str(&format!("ex:s{i} a ex:Extra .\n"));
        }
    }
    load(&s, &ttl, RdfFormat::Turtle);
    let q = "SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t ORDER BY DESC(?c) ?t";
    let before = same_answer(&s, q, "GroupCountFromMetadata");
    same_answer(
        &s,
        "SELECT ?t (COUNT(*) AS ?c) WHERE { ?s rdf:type ?t } GROUP BY ?t",
        "GroupCountFromMetadata",
    );
    // a delta makes the statistics stale: the index runs are counted instead
    update(&s, "INSERT DATA { ex:new a ex:C1 }");
    let r = run(&s, q, Optimizations::ALL);
    assert!(!has_op(&r.plan, "GroupCountFromMetadata"));
    assert!(has_op(&r.plan, "GroupCountFromIndex"));
    let after = same_rows(&s, q);
    assert_ne!(before, after);
    // after compaction they are exact again
    s.compact().unwrap();
    assert_eq!(same_answer(&s, q, "GroupCountFromMetadata"), after);
    // named graphs: a subject typed in two graphs has two rows in a union
    update(
        &s,
        "INSERT DATA { GRAPH ex:g { ex:s1 a ex:C1 . ex:t a ex:C2 } }",
    );
    s.compact().unwrap();
    let r = run(&s, q, Optimizations::ALL);
    assert!(!has_op(&r.plan, "GroupCountFromMetadata"));
    same_rows(&s, q);
    same_rows(
        &s,
        "SELECT ?t (COUNT(?s) AS ?c) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s a ?t } } GROUP BY ?t",
    );
}

#[test]
fn optimizations_can_be_disabled_by_name() {
    let o = Optimizations::ALL
        .disable("range_pushdown, metadata_counts")
        .unwrap();
    assert!(!o.range_pushdown && !o.metadata_counts && o.incremental_group);
    assert_eq!(
        Optimizations::ALL.disable("all").unwrap(),
        Optimizations::NONE
    );
    assert!(Optimizations::ALL.disable("nope").is_err());
}

// ------------------------------------------------------------ top-k prefilter ------

#[test]
fn numeric_top_k_prefilter_keeps_the_exact_order() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 0..20_000 {
        // many ties, equal values of different types and lexical forms, huge integers
        let v = match i % 7 {
            0 => format!("{}", i % 500),
            1 => format!("{}.0e0", i % 500),
            2 => format!("\"{}.50\"^^xsd:decimal", i % 500),
            3 => format!("{}.5", i % 500),
            4 => format!("\"{}\"^^xsd:int", i % 500),
            5 => format!("9007199254740{}", 990 + i % 9),
            _ => format!("\"0{}\"^^xsd:integer", i % 500),
        };
        ttl.push_str(&format!("ex:s{i} ex:v {v} .\n"));
    }
    load(&s, &ttl, RdfFormat::Turtle);
    for q in [
        "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(?v) LIMIT 10",
        "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v LIMIT 25",
        "SELECT ?v WHERE { ?s ex:v ?v } ORDER BY DESC(?v) LIMIT 1",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v < 3) } ORDER BY DESC(?v) LIMIT 5",
    ] {
        // row order matters here, not just the multiset; ties keep their input order, so
        // compare with the same plan minus the prefilter
        let fast = run(&s, q, Optimizations::ALL);
        let without = Optimizations {
            topk_prefilter: false,
            ..Optimizations::ALL
        };
        let slow = run(&s, q, without);
        assert_eq!(fast.rows(), slow.rows(), "{q}");
        assert!(has_desc(&fast.plan, "numeric prefilter"), "{q}");
    }
    // a non-numeric value disables it
    update(&s, "INSERT DATA { ex:x ex:v \"text\" }");
    let q = "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(?v) LIMIT 10";
    let fast = run(&s, q, Optimizations::ALL);
    assert!(!has_desc(&fast.plan, "numeric prefilter"));
    assert_eq!(
        solutions(&fast),
        solutions(&run(&s, q, Optimizations::NONE))
    );
}
