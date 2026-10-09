//! Executor optimizations: each test checks that the optimized operator is chosen (by its
//! EXPLAIN name) and that it returns exactly the answer of the generic operators.

use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

pub(super) const PREFIXES: &str = "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

pub(super) fn load(s: &Store, text: &str, format: RdfFormat) {
    s.load(&[Source::from_bytes(text.as_bytes().to_vec(), format, None)])
        .unwrap();
}

pub(super) fn update(s: &Store, text: &str) {
    super::update::update(s, &format!("{PREFIXES}{text}"), &QueryOptions::default()).unwrap();
}

pub(super) fn run(s: &Store, text: &str, opt: Optimizations) -> QueryResult {
    let opts = QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    };
    let text = format!("{PREFIXES}{text}");
    query(s.snapshot(), &text, &opts).unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// Solutions as exact RDF terms, sorted (a multiset).
pub(super) fn solutions(r: &QueryResult) -> Vec<String> {
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

pub(super) fn has_op(p: &PlanInfo, op: &str) -> bool {
    p.operator == op || p.children.iter().any(|c| has_op(c, op))
}

pub(super) fn has_desc(p: &PlanInfo, needle: &str) -> bool {
    p.description.contains(needle) || p.children.iter().any(|c| has_desc(c, needle))
}

/// Run `text` with every optimization and with none; the answers must be equal and the
/// optimized plan must contain `op` (an operator name, or `desc:` + description text).
pub(super) fn same_answer(s: &Store, text: &str, op: &str) -> Vec<String> {
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
pub(super) fn same_rows(s: &Store, text: &str) -> Vec<String> {
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
        "SELECT ?o (SUM(?a) AS ?s) (COUNT(?a) AS ?c) (COUNT(*) AS ?n) (MIN(?a) AS ?lo) (MAX(?a) AS ?hi) WHERE { ?p ex:org ?o OPTIONAL { ?p ex:age ?a } } GROUP BY ?o",
        "SELECT ?o (SUM(?b) AS ?s) (AVG(?b) AS ?m) WHERE { ?p ex:org ?o ; ex:big ?b } GROUP BY ?o",
        "SELECT (SUM(?a) AS ?s) (AVG(?a) AS ?m) (COUNT(*) AS ?n) (MAX(?a) AS ?hi) WHERE { ?p ex:age ?a }",
        "SELECT (SUM(?a) AS ?s) (AVG(?a) AS ?m) (COUNT(*) AS ?n) (MIN(?a) AS ?lo) WHERE { ?p ex:age ?a FILTER(?a > 1000) }",
        "SELECT ?a (COUNT(?p) AS ?n) (MAX(?p) AS ?m) WHERE { ?p ex:age ?a } GROUP BY ?a",
        "SELECT ?o (AVG(?a) AS ?avg) WHERE { ?p ex:org ?o OPTIONAL { ?p ex:age ?a } } GROUP BY ?o HAVING (COUNT(?a) > 400)",
        "SELECT ?o (STDEV(?a) AS ?sd) (VAR_POP(?a) AS ?v) (COUNT(*) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a FILTER(isNumeric(?a)) } GROUP BY ?o",
        "SELECT ?o (STDEV_POP(?b) AS ?sd) (VARIANCE(?b) AS ?v) WHERE { ?p ex:org ?o OPTIONAL { ?p ex:big ?b } } GROUP BY ?o",
        "SELECT (STDEV_SAMP(?a) AS ?sd) (VAR_SAMP(?a) AS ?v) WHERE { ?p ex:age ?a FILTER(?a > 1000) }",
    ] {
        same_answer(&s, q, "desc:[incremental]");
    }
    // SAMPLE takes a row that depends on the input's order, which other operators
    // change (a merge left join keeps the left side's order, a hash left join puts the
    // unmatched rows last): compared with only the incremental grouping off
    let q = "SELECT ?o (COUNT(*) AS ?n) (SAMPLE(?p) AS ?x) (SAMPLE(?a) AS ?y) \
             WHERE { ?p ex:org ?o OPTIONAL { ?p ex:age ?a } } GROUP BY ?o";
    let fast = run(&s, q, Optimizations::ALL);
    let slow = Optimizations {
        incremental_group: false,
        ..Optimizations::ALL
    };
    let slow = run(&s, q, slow);
    assert_eq!(solutions(&fast), solutions(&slow), "{q}");
    assert!(has_desc(&fast.plan, "[incremental]") && !has_desc(&slow.plan, "[incremental]"));
    // not admitted: DISTINCT, expressions, GROUP_CONCAT, several keys
    for q in [
        "SELECT ?o (COUNT(DISTINCT ?a) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o",
        "SELECT ?o (SUM(?a * 2) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o",
        "SELECT ?o (GROUP_CONCAT(?a) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o",
        "SELECT ?o (MEDIAN(?a) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o",
        "SELECT ?o (STDEV(DISTINCT ?a) AS ?n) WHERE { ?p ex:org ?o ; ex:age ?a } GROUP BY ?o",
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

// ---------------------------------------------------------------- anti-join ------

#[test]
fn minus_on_one_bound_variable_is_an_anti_join() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..4000 {
        ttl.push_str(&format!("ex:s{i} a ex:C{} .\n", i % 3));
        if i % 5 != 0 {
            ttl.push_str(&format!("ex:s{i} ex:wrote ex:d{} .\n", i % 50));
        }
        if i % 7 == 0 {
            ttl.push_str(&format!("ex:s{i} ex:age {} .\n", i % 90));
        }
        if i % 7 == 0 && i < 140 {
            ttl.push_str(&format!("ex:d{} ex:by ex:s{i} .\n", i % 50));
        }
        ttl.push_str(&format!("ex:s{i} ex:v {} .\n", i % 11 * 10));
    }
    load(&s, &ttl, RdfFormat::Turtle);
    for q in [
        // both sides sorted on ?p: merge
        "SELECT ?p WHERE { ?p a ex:C1 MINUS { ?p ex:wrote ?d } }",
        "SELECT (COUNT(*) AS ?c) WHERE { ?p a ex:C0 MINUS { ?p ex:wrote ?d } }",
        // the right side sorted on another variable: hash probe
        "SELECT ?p ?d WHERE { ?p ex:wrote ?d MINUS { ?d ex:by ?q } }",
        "SELECT ?d WHERE { ?p ex:wrote ?d MINUS { ?x ex:by ?p } }",
        // literals and duplicates on the left
        "SELECT ?v WHERE { ?p ex:v ?v MINUS { ?q ex:age ?v } }",
    ] {
        let a = same_answer(&s, q, "desc:anti-join on");
        assert!(!a.is_empty(), "{q}");
        let r = run(&s, q, Optimizations::ALL);
        let mut stack = vec![&r.plan];
        while let Some(p) = stack.pop() {
            if p.operator == "Minus" {
                let how = if q.contains("ex:by") || q.contains("ex:age") {
                    "hash"
                } else {
                    "merge"
                };
                assert!(p.description.contains(how), "{q}: {}", p.description);
            }
            stack.extend(&p.children);
        }
    }
    // unbound shared variables, several shared variables, none: the generic MINUS
    for q in [
        "SELECT ?p ?a WHERE { ?p a ex:C2 OPTIONAL { ?p ex:age ?a } MINUS { ?q ex:age ?a } }",
        "SELECT ?p WHERE { ?p a ex:C2 MINUS { { ?p ex:age ?a } UNION { ?x ex:by ?y } } }",
        "SELECT ?p ?d WHERE { ?p ex:wrote ?d MINUS { ?d ex:by ?p } }",
        "SELECT ?p WHERE { ?p a ex:C2 MINUS { ?x ex:by ?y } }",
    ] {
        let r = run(&s, q, Optimizations::ALL);
        assert!(!has_desc(&r.plan, "anti-join on"), "{q}");
        same_rows(&s, q);
    }
}

#[test]
fn optimizations_can_be_disabled_by_name() {
    let o = Optimizations::ALL
        .disable("range_pushdown, metadata_counts")
        .unwrap();
    assert!(!o.range_pushdown && !o.metadata_counts && o.incremental_group);
    let o = Optimizations::ALL.disable("ordered_topk").unwrap();
    assert!(!o.ordered_topk && o.topk_prefilter);
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
        let with = Optimizations {
            ordered_topk: false,
            topk_heap: false,
            ..Optimizations::ALL
        };
        let fast = run(&s, q, with);
        let without = Optimizations {
            topk_prefilter: false,
            ..with
        };
        let slow = run(&s, q, without);
        assert_eq!(fast.rows(), slow.rows(), "{q}");
        assert!(has_desc(&fast.plan, "numeric prefilter"), "{q}");
        let heap = run(
            &s,
            q,
            Optimizations {
                topk_heap: true,
                ..with
            },
        );
        assert_eq!(heap.rows(), slow.rows(), "{q}");
        assert!(has_desc(&heap.plan, "top-k heap"), "{q}");
    }
    // a non-numeric value disables it
    update(&s, "INSERT DATA { ex:x ex:v \"text\" }");
    let q = "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(?v) LIMIT 10";
    let fast = run(
        &s,
        q,
        Optimizations {
            topk_heap: false,
            ..Optimizations::ALL
        },
    );
    assert!(!has_desc(&fast.plan, "numeric prefilter"));
    assert_eq!(
        solutions(&fast),
        solutions(&run(&s, q, Optimizations::NONE))
    );
}

// ------------------------------------------------------- ordered scan top-k ------

fn without_ordered_topk() -> Optimizations {
    Optimizations {
        ordered_topk: false,
        ..Optimizations::ALL
    }
}

/// Run `q` with the ordered top-k scan (chosen whenever it applies) and without it: the
/// rows must be equal in order, ties included. Returns the optimized result.
fn topk_check(s: &Store, q: &str) -> QueryResult {
    super::plan::FORCE_ORDERED_TOPK.with(|f| f.set(true));
    let fast = run(s, q, Optimizations::ALL);
    super::plan::FORCE_ORDERED_TOPK.with(|f| f.set(false));
    let slow = run(s, q, without_ordered_topk());
    assert_eq!(fast.rows(), slow.rows(), "{q}");
    assert!(!has_op(&slow.plan, "IndexTopK"), "{q}");
    fast
}

/// A pseudo-random object of every kind: inline integers, decimals of several scales
/// and doubles of both signs (with their boundaries), non-canonical numerals and other
/// numeric types from the vocabulary, strings, IRIs, blank nodes and booleans; small
/// ranges, so that many values tie.
fn topk_value(r: u64) -> String {
    let small = (r >> 8) as i64 % 40 - 20;
    let frac = (r >> 20) % 1000;
    match r % 17 {
        0 | 1 => format!("{small}"),
        2 => format!("{small}.{}", frac % 10),
        3 => format!("{small}.{frac:03}"),
        4 => format!("\"{small}.{}0\"^^xsd:decimal", frac % 10),
        5 => format!("{small}.5e0"),
        6 => [
            "0.0e0",
            "-0.0e0",
            "\"INF\"^^xsd:double",
            "\"-INF\"^^xsd:double",
            "1.0e300",
        ][(r >> 8) as usize % 5]
            .to_string(),
        7 => format!("\"0{}\"^^xsd:integer", small.abs()),
        8 => format!("\"{small}\"^^xsd:int"),
        9 => format!("\"{small}.25\"^^xsd:float"),
        10 => format!("\"s{}\"", small.abs()),
        11 => format!("\"l{}\"@en", small.abs() % 5),
        12 => format!("ex:o{}", small.abs() % 7),
        13 => format!("_:b{}", small.abs() % 4),
        14 => ["true", "false"][(r >> 8) as usize % 2].to_string(),
        15 => [
            "576460752303423487",
            "-576460752303423488",
            "576460752303423488",
            "99999999999999999999",
            "-0.000000000000001",
            "123456789.123456789",
        ][(r >> 8) as usize % 6]
            .to_string(),
        _ => format!("{}", small * 1000),
    }
}

/// `n` pseudo-random values of `ex:v`, some subjects with several, in the default graph
/// and two named graphs (some triples in several graphs); `ex:w` holds integers only.
fn topk_store(seed: u64, n: usize, opts: StoreOptions) -> Store {
    let mut x = seed;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let s = Store::in_memory(opts);
    let mut trig = String::from(
        "@prefix ex: <http://ex.org/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 0..n {
        let r = next();
        let subj = format!("ex:s{}", i % (n / 2 + 1));
        let triple = format!("{subj} ex:v {} .", topk_value(r));
        match (r >> 40) % 10 {
            0..=5 => trig.push_str(&format!("{triple}\n")),
            6 | 7 => trig.push_str(&format!("ex:g1 {{ {triple} }}\n")),
            8 => trig.push_str(&format!("ex:g2 {{ {triple} }}\n")),
            _ => trig.push_str(&format!(
                "{triple}\nex:g1 {{ {triple} }}\nex:g2 {{ {triple} }}\n"
            )),
        }
        trig.push_str(&format!("{subj} ex:w {} .\n", (r >> 16) as i64 % 500 - 250));
    }
    load(&s, &trig, RdfFormat::TriG);
    s
}

/// ORDER BY + LIMIT queries the ordered scan applies to.
fn topk_queries() -> Vec<String> {
    let mut out = Vec::new();
    let patterns = [
        "SELECT ?s ?v WHERE { ?s ex:v ?v }",
        "SELECT ?v ?s WHERE { ?s ex:v ?v FILTER(?v > 0) }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v >= -10 && ?v < 12.5) }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v <= 3 && ?s != ex:s3) }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(isNumeric(?v)) }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(!isNumeric(?v)) }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(STRSTARTS(STR(?s), \"http://ex.org/s1\")) }",
        "SELECT ?s ?v ?g WHERE { GRAPH ?g { ?s ex:v ?v } }",
        "SELECT ?s ?v WHERE { GRAPH ex:g1 { ?s ex:v ?v } }",
        "SELECT ?s ?v WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s ex:v ?v } }",
        "SELECT ?s ?v WHERE { ?s ex:w ?v }",
        "SELECT * WHERE { ?s ex:w ?v FILTER(?v > 100) }",
        "SELECT * WHERE { ?s ?p ?v }",
    ];
    for (i, p) in patterns.iter().enumerate() {
        for (dir, k, off) in [
            ("DESC", 10, 0),
            ("ASC", 10, 0),
            ("DESC", 1, 0),
            ("ASC", 3, 7),
            ("DESC", 37, 5),
            ("ASC", 250, 0),
        ] {
            let limit = k + i;
            out.push(format!("{p} ORDER BY {dir}(?v) LIMIT {limit} OFFSET {off}"));
        }
    }
    // other order variables: the scan is re-targeted to a permutation sorted on them
    out.push("SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(?s) LIMIT 7".into());
    out.push("SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v > 3) } ORDER BY ?s LIMIT 4".into());
    out.push("SELECT ?s ?v ?g WHERE { GRAPH ?g { ?s ex:v ?v } } ORDER BY ?s LIMIT 9".into());
    out
}

#[test]
fn first_key_prefilter_keeps_the_exact_order() {
    let s = topk_store(0x5851_f42d_4c95_7f2d, 6000, StoreOptions::default());
    let with = Optimizations {
        topk_heap: false,
        ..Optimizations::ALL
    };
    let without = Optimizations {
        topk_first_key: false,
        ..with
    };
    let mut kept = 0;
    let queries = [
        "SELECT ?s ?w WHERE { ?s ex:w ?w } ORDER BY DESC(ABS(?w - 50)) ?s LIMIT 10",
        "SELECT ?s ?w WHERE { ?s ex:w ?w } ORDER BY (?w / 50) DESC(?s) LIMIT 37 OFFSET 5",
        "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v ?s LIMIT 10",
        "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(?v) DESC(?s) LIMIT 3",
        // errors and unbound keys sort first
        "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ABS(?v) ?s LIMIT 20",
        "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(ABS(?v)) ?s LIMIT 20",
        "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(STR(?v)) ?s ?v LIMIT 5",
        "SELECT ?s ?v ?w WHERE { ?s ex:v ?v OPTIONAL { ?s ex:w ?w FILTER(?w > 200) } } ORDER BY ?w ?v ?s LIMIT 12",
        "SELECT ?s ?v ?w WHERE { ?s ex:v ?v OPTIONAL { ?s ex:w ?w FILTER(?w > 200) } } ORDER BY DESC(?w) ?v ?s LIMIT 12",
        // the first key ties on every row: nothing to drop
        "SELECT ?s ?w WHERE { ?s ex:w ?w } ORDER BY STRLEN(\"x\") ?w ?s LIMIT 10",
    ];
    for q in queries {
        let fast = run(&s, q, with);
        let slow = run(&s, q, without);
        assert_eq!(fast.rows(), slow.rows(), "{q}");
        assert!(!has_desc(&slow.plan, "first-key prefilter"), "{q}");
        let heap = run(&s, q, Optimizations::ALL);
        assert_eq!(heap.rows(), slow.rows(), "{q}");
        assert!(has_desc(&heap.plan, "top-k heap"), "{q}");
        assert_eq!(
            solutions(&fast),
            solutions(&run(&s, q, Optimizations::NONE)),
            "{q}"
        );
        kept += has_desc(&fast.plan, "first-key prefilter") as usize;
    }
    assert!(kept >= queries.len() - 2, "{kept} of {}", queries.len());
}

#[test]
fn top_k_heap_matches_the_full_sort_on_random_data() {
    let s = topk_store(0x6a09_e667_f3bc_c908, 6000, StoreOptions::default());
    let heap = Optimizations {
        ordered_topk: false,
        ..Optimizations::ALL
    };
    // the full sort and selection, without any top-k shortcut
    let sort = Optimizations {
        topk_heap: false,
        topk_prefilter: false,
        topk_first_key: false,
        ..heap
    };
    let mut queries = topk_queries();
    queries.extend(
        [
            "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(?v) ?s LIMIT 10",
            "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY STR(?v) DESC(?s) LIMIT 25 OFFSET 3",
            "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ABS(?v) LIMIT 40",
            "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(LANG(?v)) DATATYPE(?v) ?v ?s LIMIT 30",
            "SELECT ?s ?w WHERE { ?s ex:w ?w } ORDER BY DESC(?w * 0) ?s LIMIT 15",
            "SELECT ?s ?v ?w WHERE { ?s ex:v ?v OPTIONAL { ?s ex:w ?w FILTER(?w > 200) } } ORDER BY DESC(?w) ?v LIMIT 12",
            "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v LIMIT 5999",
            "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v LIMIT 1 OFFSET 5998",
        ]
        .map(String::from),
    );
    let mut used = 0;
    for q in &queries {
        let fast = run(&s, q, heap);
        let slow = run(&s, q, sort);
        assert_eq!(fast.rows(), slow.rows(), "{q}");
        used += has_desc(&fast.plan, "top-k heap") as usize;
    }
    assert!(
        used * 10 >= queries.len() * 8,
        "{used} of {}",
        queries.len()
    );
}

#[test]
fn ordered_top_k_matches_the_sort_on_random_data() {
    for (seed, union) in [
        (0x2545_f491_4f6c_dd1d, false),
        (0x9e37_79b9_7f4a_7c15, true),
    ] {
        let s = topk_store(
            seed,
            6000,
            StoreOptions {
                union_default_graph: union,
                ..Default::default()
            },
        );
        let before = s.snapshot();
        let queries = topk_queries();
        let mut chosen = 0;
        let mut answers = Vec::new();
        for q in &queries {
            let r = topk_check(&s, q);
            chosen += has_op(&r.plan, "IndexTopK") as usize;
            answers.push(r.rows());
        }
        assert!(
            chosen * 10 >= queries.len() * 9,
            "{chosen} of {}",
            queries.len()
        );
        // updates after the base build: inserts at both ends and in the middle (new
        // inline values and new vocabulary terms), deletes of extreme rows
        update(
            &s,
            "INSERT DATA { ex:n1 ex:v 99 . ex:n2 ex:v -99.5 . ex:n3 ex:v \"0099\"^^xsd:integer . \
             ex:n4 ex:v \"zz\" . ex:n5 ex:v 1.5e10 . ex:n6 ex:v ex:new . ex:n7 ex:v 3 . \
             ex:n8 ex:v \"-7.70\"^^xsd:decimal . ex:n9 ex:w 1000 . \
             GRAPH ex:g1 { ex:n1 ex:v 99 . ex:n10 ex:v -1000 } }",
        );
        update(
            &s,
            "DELETE WHERE { ?s ex:v 1.0e300 } ; DELETE WHERE { ?s ex:v \"INF\"^^xsd:double } ; \
             DELETE WHERE { ?s ex:w 249 }",
        );
        update(&s, "DELETE DATA { ex:n7 ex:v 3 }");
        for q in &queries {
            topk_check(&s, q);
        }
        // the snapshot taken before the updates still answers as before
        let opts = QueryOptions {
            optimizations: Some(Optimizations::ALL),
            no_cache: true,
            ..Default::default()
        };
        for (q, rows) in queries.iter().zip(&answers) {
            super::plan::FORCE_ORDERED_TOPK.with(|f| f.set(true));
            let r = query(before.clone(), &format!("{PREFIXES}{q}"), &opts).unwrap();
            super::plan::FORCE_ORDERED_TOPK.with(|f| f.set(false));
            assert_eq!(&r.rows(), rows, "{q}");
        }
    }
}

/// Many small random stores, random directions, limits and offsets.
#[test]
fn ordered_top_k_matches_the_sort_on_random_limits() {
    let mut x: u64 = 0x5851_f42d_4c95_7f2d;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let patterns = [
        "SELECT ?s ?v WHERE { ?s ex:v ?v }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v < 5) }",
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v > -3 && ?s != ex:s1) }",
        "SELECT ?s ?v ?g WHERE { GRAPH ?g { ?s ex:v ?v } }",
        "SELECT ?s ?v WHERE { ?s ex:w ?v FILTER(STRENDS(STR(?s), \"7\")) }",
    ];
    for round in 0..8 {
        let opts = StoreOptions {
            union_default_graph: round % 2 == 1,
            ..Default::default()
        };
        let s = topk_store(next(), 300 + (next() % 1500) as usize, opts);
        if round % 3 == 2 {
            update(
                &s,
                "INSERT DATA { ex:d1 ex:v 7 . ex:d2 ex:v \"07\"^^xsd:integer . ex:d3 ex:v 7.0 } ; \
                 DELETE WHERE { ?s ex:v 0 }",
            );
        }
        for p in patterns {
            let dir = ["ASC", "DESC"][(next() % 2) as usize];
            let (limit, offset) = (1 + next() % 60, next() % 20);
            topk_check(
                &s,
                &format!("{p} ORDER BY {dir}(?v) LIMIT {limit} OFFSET {offset}"),
            );
        }
    }
}

#[test]
fn ordered_top_k_falls_back_on_values_that_are_not_totally_ordered() {
    let s = topk_store(0x1234_5678_9abc_def1, 2000, StoreOptions::default());
    update(
        &s,
        "INSERT DATA { ex:x1 ex:v \"NaN\"^^xsd:double . ex:x2 ex:w \"2024-01-01\"^^xsd:date }",
    );
    for q in [
        "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(?v) LIMIT 5",
        "SELECT ?s ?v WHERE { ?s ex:w ?v } ORDER BY ?v LIMIT 5",
    ] {
        let r = topk_check(&s, q);
        assert!(has_desc(&r.plan, "ran the generic plan"), "{q}");
    }
    // NaN fails the range filter: the ordered scan decides
    let r = topk_check(
        &s,
        "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v > 2) } ORDER BY DESC(?v) LIMIT 5",
    );
    assert!(has_desc(&r.plan, "[read "), "{:#?}", r.plan);
}

/// A piece whose best rows mostly fail the filter is read on, step by step, while its
/// unread values can still beat the k-th candidate from the other pieces; ties at the
/// cut stay.
#[test]
fn ordered_top_k_reads_a_piece_until_it_cannot_win() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 1..=3000 {
        // integers and negative doubles: only every 50th subject passes the filter
        let tag = if i % 50 == 0 { "keep" } else { "drop" };
        ttl.push_str(&format!("ex:{tag}{i} ex:u {i} , -{i}.0e0 .\n"));
        // decimals below the kept integers, and a tie run on 2000
        ttl.push_str(&format!("ex:keep_d{i} ex:u {}.5 .\n", i % 1500));
        if i % 7 == 0 {
            ttl.push_str(&format!("ex:keep_t{i} ex:u 2000 , -2000.0e0 .\n"));
        }
    }
    // the second row is an inline 12 that ties with a vocabulary "012" read earlier and
    // comes first in the scan: the piece must be read up to and including 12
    ttl.push_str("ex:keep_a ex:t 12 . ex:keep_b ex:t \"012\"^^xsd:integer . ex:keep_c ex:t 50 .\n");
    for i in 51..=100 {
        ttl.push_str(&format!("ex:drop_t{i} ex:t {i} .\n"));
    }
    for i in 0..1000 {
        ttl.push_str(&format!("ex:drop_u{i} ex:t 13 .\n"));
    }
    load(&s, &ttl, RdfFormat::Turtle);
    let r = topk_check(
        &s,
        "SELECT ?s WHERE { ?s ex:t ?v FILTER(STRSTARTS(STR(?s), \"http://ex.org/keep\")) } ORDER BY DESC(?v) LIMIT 2",
    );
    assert!(has_op(&r.plan, "IndexTopK"));
    assert_eq!(
        solutions(&r),
        ["<http://ex.org/keep_a>", "<http://ex.org/keep_c>"]
    );
    for q in [
        "SELECT ?s ?v WHERE { ?s ex:u ?v FILTER(STRSTARTS(STR(?s), \"http://ex.org/keep\")) } ORDER BY DESC(?v) LIMIT 30",
        "SELECT ?s ?v WHERE { ?s ex:u ?v FILTER(STRSTARTS(STR(?s), \"http://ex.org/keep\")) } ORDER BY ?v LIMIT 30 OFFSET 4",
        "SELECT ?s ?v WHERE { ?s ex:u ?v FILTER(STRSTARTS(STR(?s), \"http://ex.org/keep\") && ?v > 10) } ORDER BY DESC(?v) LIMIT 50",
        "SELECT ?s ?v WHERE { ?s ex:u ?v } ORDER BY DESC(?v) LIMIT 300",
        "SELECT ?s ?v WHERE { ?s ex:u ?v } ORDER BY ?v LIMIT 320",
    ] {
        let r = topk_check(&s, q);
        assert!(has_op(&r.plan, "IndexTopK"), "{q}");
    }
}

/// `OFFSET n LIMIT u64::MAX` saturates instead of overflowing, through the plain slice,
/// the limited execution of a slice, the top-k sort and the ordered top-k scan.
#[test]
fn the_largest_limit_and_offset_saturate() {
    const MAX: u64 = u64::MAX;
    let s = Store::in_memory(StoreOptions::default());
    load(
        &s,
        "@prefix ex: <http://ex.org/> . ex:a ex:v 1 . ex:b ex:v 2 . ex:c ex:v 3 .",
        RdfFormat::Turtle,
    );
    for opt in [Optimizations::ALL, Optimizations::NONE] {
        let rows = |q: &str| solutions(&run(&s, q, opt));
        // the plain slice
        assert_eq!(
            rows(&format!(
                "SELECT ?x {{ VALUES ?x {{ 1 2 }} }} OFFSET 1 LIMIT {MAX}"
            )),
            ["\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"]
        );
        // top-k: ORDER BY with a LIMIT
        assert_eq!(
            rows(&format!(
                "SELECT ?x {{ VALUES ?x {{ 2 1 }} }} ORDER BY ?x OFFSET 1 LIMIT {MAX}"
            )),
            ["\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"]
        );
        // a slice over a scan, executed with a row budget
        assert_eq!(
            rows(&format!("SELECT ?s {{ ?s ex:v ?v }} OFFSET 2 LIMIT {MAX}")).len(),
            1
        );
        assert!(
            rows(&format!(
                "SELECT ?s {{ ?s ex:v ?v }} OFFSET {MAX} LIMIT {MAX}"
            ))
            .is_empty()
        );
        assert!(
            rows(&format!(
                "SELECT ?x {{ VALUES ?x {{ 1 2 }} }} ORDER BY ?x OFFSET {MAX} LIMIT {MAX}"
            ))
            .is_empty()
        );
    }
    // the ordered top-k scan
    let r = topk_check(
        &s,
        &format!("SELECT ?s ?v {{ ?s ex:v ?v }} ORDER BY DESC(?v) OFFSET 1 LIMIT {MAX}"),
    );
    assert!(has_op(&r.plan, "IndexTopK"), "{:#?}", r.plan);
    assert_eq!(
        solutions(&r),
        [
            "<http://ex.org/a> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            "<http://ex.org/b> \"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        ]
    );
}

/// The `[read N rows]` note of the ordered scan in an executed plan.
fn topk_rows_read(p: &PlanInfo) -> Option<u64> {
    if p.operator == "IndexTopK" {
        let n = p.description.split("[read ").nth(1)?;
        return n[..n.find(' ')?].parse().ok();
    }
    p.children.iter().find_map(topk_rows_read)
}

/// Salaries as in the benchmark: inline decimals, a tenth of them non-canonical
/// (vocabulary literals). The planner picks the ordered scan on its own, and it reads
/// far fewer rows than the plain scan.
#[test]
fn ordered_top_k_is_chosen_when_it_reads_less() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 0..120_000u64 {
        let cents = (i * 7919) % 10_000_000;
        let v = if i % 10 == 0 {
            format!("\"{}.{:02}0\"^^xsd:decimal", cents / 100, cents % 100)
        } else {
            format!("{}.{:02}", cents / 100, cents % 100)
        };
        ttl.push_str(&format!("ex:p{i} ex:salary {v} .\n"));
    }
    load(&s, &ttl, RdfFormat::Turtle);
    for q in [
        "SELECT ?p ?s WHERE { ?p ex:salary ?s FILTER(?s > 15000) } ORDER BY DESC(?s) LIMIT 10",
        "SELECT ?p ?s WHERE { ?p ex:salary ?s } ORDER BY ?s LIMIT 10 OFFSET 3",
    ] {
        let fast = run(&s, q, Optimizations::ALL);
        let slow = run(&s, q, without_ordered_topk());
        assert_eq!(fast.rows(), slow.rows(), "{q}");
        assert_eq!(fast.rows().len(), 10);
        let read = topk_rows_read(&fast.plan);
        assert!(read.is_some_and(|n| n < 60_000), "{q}: {:#?}", fast.plan);
    }
    // ordering by a column of IRIs reads every row: the plain sort stays
    let r = run(
        &s,
        "SELECT ?p ?s WHERE { ?p ex:salary ?s } ORDER BY DESC(?p) LIMIT 10",
        Optimizations::ALL,
    );
    assert!(!has_op(&r.plan, "IndexTopK"));
}

// ---------------------------------------------------------- batched frontiers ------

#[test]
fn transitive_paths_expand_large_frontiers_by_sweeps() {
    let s = Store::in_memory(StoreOptions::default());
    let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
    let n = 60_000;
    for i in 0..n {
        // a sparse random-ish graph with average out-degree 2
        trig.push_str(&format!(
            "ex:n{i} ex:k ex:n{} , ex:n{} .\n",
            (i * 7 + 3) % n,
            (i * 13 + 11) % n
        ));
    }
    // hubs with runs longer than an index block (32k rows): runs span chunk boundaries
    for i in 0..40_000 {
        trig.push_str(&format!(
            "ex:n1 ex:k ex:h{i} .\nex:h{i} ex:k ex:n{} .\n",
            (i * 31) % n
        ));
    }
    // edges only in a named graph
    for i in 0..500 {
        trig.push_str(&format!(
            "ex:g {{ ex:n{i} ex:k ex:x{i} . ex:x{i} ex:k ex:y{i} . }}\n"
        ));
    }
    load(&s, &trig, RdfFormat::TriG);
    let queries = [
        "SELECT ?x WHERE { ex:n0 ex:k* ?x }",
        "SELECT ?x WHERE { ex:n5 ex:k+ ?x }",
        "SELECT ?x WHERE { ?x ex:k* ex:n9 }",
        "SELECT ?x WHERE { ?x ex:k+ ex:n1 }",
        "SELECT ?x WHERE { ex:n0 ^ex:k* ?x }",
    ];
    // the named graph alone only has short chains
    same_rows(
        &s,
        "SELECT ?x WHERE { GRAPH <urn:x-arq:UnionGraph> { ex:n0 ex:k* ?x } }",
    );
    for q in queries {
        let rows = same_answer(&s, q, "desc:frontier levels expanded by index sweeps");
        assert!(rows.len() > 1000, "{q}: {}", rows.len());
    }
    // the delta: inserted edges (rows between base rows of a run) and deleted ones
    update(
        &s,
        "INSERT DATA { ex:n1 ex:k ex:new1 . ex:new1 ex:k ex:new2 . ex:h7 ex:k ex:new3 } ; DELETE DATA { ex:h8 ex:k ex:n248 . ex:n3 ex:k ex:n24 }",
    );
    for q in queries {
        same_answer(&s, q, "desc:frontier levels expanded by index sweeps");
    }
    // small frontiers keep per-node seeks
    let r = run(&s, "SELECT ?x WHERE { ex:y3 ex:k* ?x }", Optimizations::ALL);
    assert!(!has_desc(&r.plan, "index sweeps"));
}

// ------------------------------------------------------- selective decoding ------

#[test]
fn scans_decode_only_the_columns_they_read() {
    let build = || {
        let s = Store::in_memory(StoreOptions::default());
        let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
        for i in 0..80_000 {
            trig.push_str(&format!(
                "ex:s{i} ex:p {} ; ex:q ex:o{} .\n",
                i % 1000,
                i % 77
            ));
            if i % 10 == 0 {
                trig.push_str(&format!("ex:g {{ ex:s{i} ex:p {} }}\n", i % 5));
            }
        }
        load(&s, &trig, RdfFormat::TriG);
        s
    };
    let queries = [
        ("SELECT ?s ?o WHERE { ?s ex:p ?o }", "[decodes SOG]"),
        (
            "SELECT ?s ?o WHERE { GRAPH ?g { ?s ex:p ?o } }",
            "[decodes SOG]",
        ),
        (
            "SELECT ?s WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s ex:q ex:o3 } }",
            "[decodes SG]",
        ),
        ("SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 70000", ""),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:q ?b . ?c ex:q ?b }",
            "",
        ),
        ("SELECT (COUNT(DISTINCT ?o) AS ?n) WHERE { ?s ex:p ?o }", ""),
        (
            "SELECT ?o (COUNT(?s) AS ?n) WHERE { ?s ex:q ?o } GROUP BY ?o",
            "",
        ),
    ];
    let without = Optimizations {
        selective_columns: false,
        ..Optimizations::ALL
    };
    let (a, b) = (build(), build());
    for (q, note) in queries {
        let fast = run(&a, q, Optimizations::ALL);
        if !note.is_empty() {
            assert!(has_desc(&fast.plan, note), "{q}: {:#?}", fast.plan);
        }
    }
    // the same work on a second store with whole blocks caches more bytes
    for (q, _) in queries {
        run(&b, q, without);
    }
    assert!(
        a.cache().bytes() < b.cache().bytes(),
        "{} vs {}",
        a.cache().bytes(),
        b.cache().bytes()
    );
    for (q, _) in queries {
        assert_eq!(
            solutions(&run(&a, q, Optimizations::ALL)),
            solutions(&run(&a, q, without)),
            "{q}"
        );
    }
    // updates: the delta forces full-key merges in the touched ranges
    update(
        &a,
        "INSERT DATA { ex:s5 ex:p 7 . ex:new ex:q ex:o3 } ; DELETE DATA { ex:s9 ex:p 9 }",
    );
    for (q, _) in queries {
        assert_eq!(
            solutions(&run(&a, q, Optimizations::ALL)),
            solutions(&run(&a, q, without)),
            "{q}"
        );
    }
}

// ------------------------------------------------------------ memory budget ------

/// Every optimized operator checks the memory budget: a query needs exactly its
/// reported peak (it runs under a budget of the peak and fails one byte below it), and
/// the optimized operator is the one that ran.
#[test]
fn optimized_operators_obey_the_memory_budget() {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..4000 {
        ttl.push_str(&format!(
            "ex:p{i} ex:v {} ; ex:org ex:o{} ; ex:knows ex:p{} .\n",
            (i * 37) % 4001,
            i % 23,
            (i * 7 + 1) % 4000
        ));
    }
    // a wide frontier for the path sweep
    for i in 0..200 {
        ttl.push_str(&format!("ex:root ex:k ex:h{i} . ex:h{i} ex:k ex:m{i} .\n"));
    }
    load(&s, &ttl, RdfFormat::Turtle);
    let budget = |limit: Option<u64>| QueryOptions {
        max_memory_bytes: limit,
        no_cache: true,
        ..Default::default()
    };
    for (q, op) in [
        (
            "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v > 3900) }",
            "IndexRangeScan",
        ),
        (
            "SELECT ?o (COUNT(*) AS ?n) (MAX(?v) AS ?m) WHERE { ?p ex:org ?o ; ex:v ?v } GROUP BY ?o",
            "desc:[incremental]",
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?a ex:knows ?b . ?b ex:knows ?c }",
            "CountJoinFromRuns",
        ),
        (
            "SELECT ?x WHERE { ex:root ex:k* ?x }",
            "desc:frontier levels expanded by index sweeps",
        ),
        (
            "SELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY DESC(?v) LIMIT 5",
            "desc:numeric prefilter",
        ),
        (
            "SELECT * WHERE { ?a ex:org ?o . ?b ex:org ?o }",
            "MergeJoin",
        ),
        (
            "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(?v > 100) } ORDER BY ?v LIMIT 5",
            "IndexTopK",
        ),
    ] {
        // the data is too small for the ordered scan to pay off
        super::plan::FORCE_ORDERED_TOPK.with(|f| f.set(op == "IndexTopK"));
        let text = format!("{PREFIXES}{q}");
        let full = query(s.snapshot(), &text, &budget(None)).unwrap();
        let taken = match op.strip_prefix("desc:") {
            Some(d) => has_desc(&full.plan, d),
            None => has_op(&full.plan, op),
        };
        assert!(taken, "{q}: plan lacks {op}: {:#?}", full.plan);
        let peak = full.mem_peak_bytes;
        assert!(peak > 0, "{q}");
        let exact =
            query(s.snapshot(), &text, &budget(Some(peak))).unwrap_or_else(|e| panic!("{q}: {e}"));
        assert_eq!(solutions(&exact), solutions(&full), "{q}");
        assert_eq!(exact.mem_peak_bytes, peak, "{q}");
        match query(s.snapshot(), &text, &budget(Some(peak - 1))) {
            Err(crate::Error::BudgetExceeded(b)) => {
                assert_eq!(b.kind, crate::BudgetKind::Memory, "{q}");
                assert_eq!(b.limit, peak - 1, "{q}");
                assert!(b.requested >= peak, "{q}");
            }
            r => panic!("{q}: {:?}", r.map(|r| r.len())),
        }
    }
    super::plan::FORCE_ORDERED_TOPK.with(|f| f.set(false));
}
