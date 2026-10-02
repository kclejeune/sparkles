//! String and language filters over one variable of a triple pattern: counts from the
//! runs of a permutation (`CountFilterFromRuns`) and filtered scans give the solutions of
//! the generic operators on random data with every kind of term, in named graphs, with
//! the union default graph and with updates that are not compacted.

use super::opt_tests::{has_op, run, solutions};
use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

fn xorshift(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// Value `k` of a domain of every kind of term: inline and vocabulary numerals, simple,
/// `xsd:string`, language-tagged (tags in either case, with a base direction), empty,
/// non-ASCII and escaped strings, IRIs, blank nodes, dates, booleans, ill-typed and
/// custom literals.
fn value(k: u64) -> String {
    let j = k / 20;
    match k % 20 {
        0 => format!("{}", j as i64 - 4),
        1 => format!("{j}.5"),
        2 => format!("\"0{j}\"^^xsd:integer"),
        3 => format!("{j}.0e0"),
        4 => format!("\"s{j}\""),
        5 => format!("\"Ab{j}\"@en"),
        6 => format!("\"ab{j}\"@EN-gb"),
        7 => format!("\"r{j}\"@ar--rtl"),
        8 => format!("ex:i{j}"),
        9 => format!("_:b{}", j % 3),
        10 => format!("\"2024-0{}-01\"^^xsd:date", 1 + j % 9),
        11 => ["true", "false"][(j % 2) as usize].to_string(),
        12 => format!("\"x{j}\"^^xsd:integer"),
        13 => format!("\"{j}\"^^ex:dt"),
        14 => format!("\"{}\"", "a".repeat((j % 7) as usize)),
        15 => format!("\"Ada {j}\"^^xsd:string"),
        16 => format!("\"ünï{j}cödé\""),
        17 => format!("\"Ada{j}\"@en-US--ltr"),
        18 => "\"\"".to_string(),
        _ => format!("\"b\\\"q{j}\\n\""),
    }
}

/// `n` triples `ex:v` with values from a domain of 400 (long runs that cross index
/// blocks), in the default graph, two named graphs or both; `ex:w` on some subjects.
fn store(seed: u64, n: usize, union: bool) -> Store {
    let mut next = xorshift(seed);
    let s = Store::in_memory(StoreOptions {
        union_default_graph: union,
        ..Default::default()
    });
    let mut trig = String::from(
        "@prefix ex: <http://ex.org/> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 0..n {
        let r = next();
        let triple = format!("ex:s{} ex:v {} .", i % (n / 2), value(r % 400));
        match (r >> 40) % 8 {
            0..=4 => trig.push_str(&format!("{triple}\n")),
            5 => trig.push_str(&format!("ex:g1 {{ {triple} }}\n")),
            6 => trig.push_str(&format!("ex:g2 {{ {triple} }}\n")),
            _ => trig.push_str(&format!("{triple}\nex:g1 {{ {triple} }}\n")),
        }
        if (r >> 50).is_multiple_of(5) {
            trig.push_str(&format!(
                "ex:s{} ex:w {} .\n",
                i % 97,
                value(r >> 20 & 0xff)
            ));
        }
    }
    s.load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    s
}

/// Filters over `{X}`: on vocabulary keys (string, regex and language tests) and others
/// (generic expressions, a conjunct that is not a key test).
const FILTERS: &[&str] = &[
    "CONTAINS({X}, \"b\")",
    "CONTAINS({X}, \"Ada\")",
    "CONTAINS(STR({X}), \"1\")",
    "CONTAINS({X}, \"\")",
    "CONTAINS({X}, \"b\"@en)",
    "CONTAINS({X}, \"b\"@EN-GB)",
    "CONTAINS({X}, \"b\"@fr)",
    "CONTAINS({X}, \"ï1\")",
    "CONTAINS({X}, \"\\\"q\")",
    "STRSTARTS({X}, \"Ab\")",
    "STRSTARTS(STR({X}), \"http://ex.org/i1\")",
    "STRSTARTS(STR({X}), \"1\")",
    "STRENDS(STR({X}), \"2\")",
    "STRENDS({X}, \"dé\")",
    "REGEX(STR({X}), \"^[a-r]\", \"i\")",
    "REGEX({X}, \"d.?[0-9]$\")",
    "REGEX(STR({X}), \"s1[0-9]$\")",
    "LANGMATCHES(LANG({X}), \"en\")",
    "LANGMATCHES(LANG({X}), \"*\")",
    "LANGMATCHES(LANG({X}), \"ar\")",
    "CONTAINS({X}, \"b\") && LANGMATCHES(LANG({X}), \"en\")",
    "STRLEN(STR({X})) > 3",
    "isIRI({X}) || isBlank({X})",
    "CONTAINS({X}, \"a\") && STRLEN({X}) < 4",
    "{X} = \"s3\" || {X} = ex:i2",
];

const SHAPES: &[&str] = &[
    "SELECT (COUNT(*) AS ?c) WHERE { {P} FILTER({E}) }",
    "SELECT (COUNT(?s) AS ?c) WHERE { {P} FILTER({E}) }",
    "SELECT (COUNT(DISTINCT {X}) AS ?c) WHERE { {P} FILTER({E}) }",
];

const PATTERNS: &[&str] = &[
    "?s ex:v ?v",
    "GRAPH ?g { ?s ex:v ?v }",
    "GRAPH ex:g1 { ?s ex:v ?v }",
    "GRAPH <urn:x-arq:UnionGraph> { ?s ex:v ?v }",
    "?s ?p ?v",
];

fn without() -> Optimizations {
    Optimizations::ALL.disable("count_filter_runs").unwrap()
}

/// Same solutions with every optimization, without this one and with none; whether the
/// count was taken from the runs.
fn check(s: &Store, q: &str) -> bool {
    let fast = run(s, q, Optimizations::ALL);
    let a = solutions(&fast);
    assert_eq!(a, solutions(&run(s, q, without())), "{q}");
    assert_eq!(a, solutions(&run(s, q, Optimizations::NONE)), "{q}");
    has_op(&fast.plan, "CountFilterFromRuns")
}

fn queries(next: &mut impl FnMut() -> u64, n: usize) -> Vec<String> {
    (0..n)
        .map(|_| {
            let shape = SHAPES[(next() % SHAPES.len() as u64) as usize];
            let f = FILTERS[(next() % FILTERS.len() as u64) as usize];
            let p = PATTERNS[(next() % PATTERNS.len() as u64) as usize];
            let x = if next().is_multiple_of(4) { "?s" } else { "?v" };
            shape.replace("{E}", f).replace("{P}", p).replace("{X}", x)
        })
        .collect()
}

#[test]
fn counts_from_runs_match_the_generic_filter() {
    let mut next = xorshift(0x51f1_7e25_c0ff_ee11);
    for round in 0..2 {
        let s = store(next(), 40_000 + (next() % 20_000) as usize, round == 1);
        let before = s.snapshot();
        let qs = queries(&mut next, 70);
        let mut used = 0;
        for q in &qs {
            used += check(&s, q) as usize;
        }
        assert!(used * 2 >= qs.len(), "{used} of {}", qs.len());
        // an update of another predicate: the runs are still read in parallel
        super::opt_tests::update(&s, "INSERT DATA { ex:n0 ex:w \"Ada\" }");
        for q in qs.iter().take(20) {
            check(&s, q);
        }
        // new terms (delta ids), new triples of known terms and deletions in the range
        super::opt_tests::update(
            &s,
            "INSERT DATA { ex:n1 ex:v \"new b\"@en . ex:n2 ex:v 12345 . ex:n3 ex:v \"Ada 7\"^^xsd:string . \
             ex:n4 ex:v ex:fresh1 . ex:n5 ex:v \"b\\\"q1\\n\" . GRAPH ex:g1 { ex:n6 ex:v \"ünï1zz\" } } ; \
             DELETE WHERE { ?s ex:v \"s3\" } ; DELETE WHERE { ?s ex:v \"Ab1\"@en } ; DELETE WHERE { ?s ex:v 2 }",
        );
        for q in &qs {
            check(&s, q);
        }
        // the snapshot before the updates is unchanged
        for q in qs.iter().take(10) {
            let a = solutions(
                &super::query(
                    before.clone(),
                    &format!("{}{q}", super::opt_tests::PREFIXES),
                    &QueryOptions {
                        no_cache: true,
                        ..Default::default()
                    },
                )
                .unwrap(),
            );
            let b = solutions(
                &super::query(
                    before.clone(),
                    &format!("{}{q}", super::opt_tests::PREFIXES),
                    &QueryOptions {
                        optimizations: Some(Optimizations::NONE),
                        no_cache: true,
                        ..Default::default()
                    },
                )
                .unwrap(),
            );
            assert_eq!(a, b, "{q}");
        }
    }
}

#[test]
fn counts_from_runs_are_chosen_and_explained() {
    let s = store(7, 5000, false);
    let q = "SELECT (COUNT(*) AS ?c) WHERE { ?s ex:v ?v FILTER(CONTAINS(?v, \"b\")) }";
    let r = run(&s, q, Optimizations::ALL);
    assert!(has_op(&r.plan, "CountFilterFromRuns"), "{:#?}", r.plan);
    assert!(
        super::opt_tests::has_desc(&r.plan, "values tested on vocabulary keys"),
        "{:#?}",
        r.plan
    );
    // a generic expression is evaluated per value
    let q = "SELECT (COUNT(*) AS ?c) WHERE { ?s ex:v ?v FILTER(STRLEN(STR(?v)) > 3) }";
    let r = run(&s, q, Optimizations::ALL);
    assert!(
        super::opt_tests::has_desc(&r.plan, "values tested, "),
        "{:#?}",
        r.plan
    );
    // not over two variables, an impure expression or EXISTS, nor when switched off
    for q in [
        "SELECT (COUNT(*) AS ?c) WHERE { ?s ex:v ?v FILTER(CONTAINS(?v, \"b\") && ?s != ex:s1) }",
        "SELECT (COUNT(*) AS ?c) WHERE { ?s ex:v ?v FILTER(RAND() < 2 && CONTAINS(?v, \"b\")) }",
        "SELECT (COUNT(*) AS ?c) WHERE { ?s ex:v ?v FILTER(EXISTS { ?s ex:w ?v }) }",
        "SELECT (COUNT(DISTINCT ?s) AS ?c) WHERE { ?s ex:v ?v FILTER(CONTAINS(?v, \"b\")) }",
    ] {
        let r = run(&s, q, Optimizations::ALL);
        assert!(!has_op(&r.plan, "CountFilterFromRuns"), "{q}");
        assert_eq!(
            solutions(&r),
            solutions(&run(&s, q, Optimizations::NONE)),
            "{q}"
        );
    }
    let r = run(
        &s,
        "SELECT (COUNT(*) AS ?c) WHERE { ?s ex:v ?v FILTER(CONTAINS(?v, \"b\")) }",
        without(),
    );
    assert!(!has_op(&r.plan, "CountFilterFromRuns"));
}
