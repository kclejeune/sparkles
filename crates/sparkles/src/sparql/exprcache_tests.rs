//! Expressions evaluated per distinct value: random FILTER / BIND / ORDER BY / aggregate
//! queries over random data give the same solutions with the cache on and off, and the
//! cache runs where it should (and only there).

use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> PREFIX afn: <http://jena.apache.org/ARQ/function#> ";

fn without() -> Optimizations {
    Optimizations {
        expr_cache: false,
        ..Optimizations::ALL
    }
}

fn run_on(snap: Arc<crate::store::Snapshot>, text: &str, opt: Optimizations) -> QueryResult {
    let opts = QueryOptions {
        optimizations: Some(opt),
        no_cache: true,
        ..Default::default()
    };
    let text = format!("{PREFIXES}{text}");
    query(snap, &text, &opts).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn show(r: &QueryResult) -> Vec<String> {
    r.rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".to_string(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn notes(p: &PlanInfo, out: &mut Vec<String>) {
    if let Some(i) = p.description.find("[expr cache:") {
        out.push(p.description[i..].to_string());
    }
    for c in &p.children {
        notes(c, out);
    }
}

/// Sum of a counter over the plan.
fn counter(p: &PlanInfo, name: &str) -> u64 {
    p.counters
        .as_ref()
        .and_then(|c| c.get(name))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        + p.children.iter().map(|c| counter(c, name)).sum::<u64>()
}

/// Same solutions with and without the cache (in order when the query orders them);
/// whether the cache evaluated some expression per value.
fn check(snap: &Arc<crate::store::Snapshot>, q: &str) -> bool {
    let on = run_on(snap.clone(), q, Optimizations::ALL);
    let off = run_on(snap.clone(), q, without());
    let (mut a, mut b) = (show(&on), show(&off));
    if !q.contains("ORDER BY") {
        a.sort();
        b.sort();
    }
    assert_eq!(a, b, "{q}");
    let mut n = Vec::new();
    notes(&off.plan, &mut n);
    assert!(n.is_empty(), "{q}: cache ran while off: {n:?}");
    counter(&on.plan, "exprCacheMisses") > 0
}

fn xorshift(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// A value from a small domain of every kind of term: inline and vocabulary numerals
/// (canonical and not), strings, language-tagged strings (tags in either case, with a
/// base direction), IRIs, blank nodes, dates, booleans, ill-typed and custom literals.
fn value(r: u64) -> String {
    let k = (r >> 8) % 12;
    match r % 16 {
        0 | 1 => format!("{}", k as i64 - 4),
        2 => format!("{}.5", k),
        3 => format!("\"0{k}\"^^xsd:integer"),
        4 => format!("{k}.0e0"),
        5 => format!("\"s{k}\""),
        6 => format!("\"Ab{k}\"@en"),
        7 => format!("\"ab{k}\"@EN-gb"),
        8 => format!("\"r{k}\"@ar--rtl"),
        9 => format!("ex:i{k}"),
        10 => format!("_:b{}", k % 3),
        11 => format!("\"2024-0{}-01\"^^xsd:date", 1 + k % 9),
        12 => ["true", "false"][(k % 2) as usize].to_string(),
        13 => format!("\"x{k}\"^^xsd:integer"),
        14 => format!("\"{k}\"^^ex:dt"),
        _ => format!("\"{}\"", "a".repeat(k as usize)),
    }
}

/// `n` random `ex:v` values (repeating), some subjects with several, in the default
/// graph and two named graphs; `ex:w` on some subjects only (OPTIONAL leaves it unbound).
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
        let subj = format!("ex:s{}", i % (n / 3 + 1));
        let triple = format!("{subj} ex:v {} .", value(r));
        match (r >> 40) % 8 {
            0..=4 => trig.push_str(&format!("{triple}\n")),
            5 => trig.push_str(&format!("ex:g1 {{ {triple} }}\n")),
            6 => trig.push_str(&format!("ex:g2 {{ {triple} }}\n")),
            _ => trig.push_str(&format!("{triple}\nex:g1 {{ {triple} }}\n")),
        }
        if (r >> 50).is_multiple_of(3) {
            trig.push_str(&format!("{subj} ex:w {} .\n", value(r >> 20)));
        }
    }
    s.load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    s
}

fn update(s: &Store, text: &str) {
    super::update::update(s, &format!("{PREFIXES}{text}"), &QueryOptions::default()).unwrap();
}

/// Expressions over `?v` (the input) of every kind: type errors on some values, string,
/// numeric, date, language and IRI functions, constant regexes, NOW, the base IRI.
const EXPRS: &[&str] = &[
    "STRLEN(STR(?v))",
    "UCASE(STR(?v))",
    "LANG(?v)",
    "DATATYPE(?v)",
    "?v + 1",
    "?v * 2 - 3",
    "ABS(?v)",
    "isNumeric(?v)",
    "STRSTARTS(STR(?v), \"a\")",
    "REGEX(STR(?v), \"^[a-r]\", \"i\")",
    "REPLACE(STR(?v), \"[0-9]\", \"#\")",
    "CONCAT(STR(?v), \"-\", LANG(?v))",
    "IF(isLiteral(?v), STRLEN(STR(?v)), -1)",
    "COALESCE(?v + 0, \"none\")",
    "YEAR(?v)",
    "IRI(CONCAT(\"rel/\", ENCODE_FOR_URI(STR(?v))))",
    "SHA1(STR(?v))",
    "afn:localname(?v)",
    "STRDT(STR(?v), xsd:string)",
    "?v IN (1, \"s3\", ex:i2, 2.5)",
    "YEAR(NOW()) > 2000 && BOUND(?v)",
    "LANGMATCHES(LANG(?v), \"en\")",
    "CONTAINS(STR(?v), \"b\")",
];

/// Query shapes: `{E}` is an expression, `{P}` a graph pattern binding ?s and ?v.
const SHAPES: &[&str] = &[
    "SELECT ?s ?v ?x WHERE { {P} BIND({E} AS ?x) }",
    "SELECT ?s ?v WHERE { {P} FILTER({E}) }",
    "SELECT ?s ?v WHERE { {P} FILTER({E} && ?s != ex:s1) }",
    "SELECT ?s ?v WHERE { {P} OPTIONAL { ?s ex:w ?w } FILTER({E} || BOUND(?w)) }",
    "SELECT ?s ?v ?x WHERE { {P} OPTIONAL { ?s ex:w ?w } BIND(COALESCE(?w, {E}) AS ?x) }",
    "SELECT ?s ?v WHERE { {P} } ORDER BY ({E}) ?s ?v",
    "SELECT ?s ?v WHERE { {P} } ORDER BY DESC({E}) STR(?v) ?s",
    "SELECT ?x (COUNT(*) AS ?n) WHERE { {P} } GROUP BY ({E} AS ?x)",
    "SELECT ?s (COUNT({E}) AS ?n) (SAMPLE(?v) AS ?any) WHERE { {P} } GROUP BY ?s",
    "SELECT ?s (GROUP_CONCAT(STR({E}); separator=\",\") AS ?gc) WHERE { {P} } GROUP BY ?s",
    "SELECT (COUNT(DISTINCT {E}) AS ?n) (MAX({E}) AS ?m) (MIN({E}) AS ?l) WHERE { {P} }",
    "SELECT (SUM({E}) AS ?n) (AVG({E}) AS ?a) WHERE { {P} }",
    "SELECT ?s ?v WHERE { {P} FILTER({E}) } LIMIT 37",
    "SELECT ?s ?v WHERE { {P} FILTER({E} && EXISTS { ?s ex:w ?w }) }",
];

const PATTERNS: &[&str] = &[
    "?s ex:v ?v",
    "GRAPH ?g { ?s ex:v ?v }",
    "GRAPH ex:g1 { ?s ex:v ?v }",
    "GRAPH <urn:x-arq:UnionGraph> { ?s ex:v ?v }",
    "?s ex:v ?v . ?s ex:v ?u",
    "{ ?s ex:v ?v } UNION { ?s ex:w ?v }",
];

#[test]
fn random_queries_agree_with_and_without_the_cache() {
    let mut next = xorshift(0x9e37_79b9_7f4a_7c15);
    for round in 0..4 {
        let s = store(next(), 2500 + (next() % 3000) as usize, round % 2 == 1);
        let before = s.snapshot();
        let queries: Vec<String> = (0..60)
            .map(|_| {
                let shape = SHAPES[(next() % SHAPES.len() as u64) as usize];
                let e = EXPRS[(next() % EXPRS.len() as u64) as usize];
                let p = PATTERNS[(next() % PATTERNS.len() as u64) as usize];
                shape.replace("{E}", e).replace("{P}", p)
            })
            .collect();
        let mut cached = 0;
        for q in &queries {
            cached += check(&before, q) as usize;
        }
        // the rest read two variables or too few rows
        assert!(cached * 3 >= queries.len(), "{cached} of {}", queries.len());
        // deltas that are not compacted: new terms (delta ids), deletions
        update(
            &s,
            "INSERT DATA { ex:n1 ex:v \"new\"@en . ex:n2 ex:v 12345 . ex:n3 ex:v \"0012\"^^xsd:integer . \
             ex:n4 ex:v ex:fresh . GRAPH ex:g1 { ex:n5 ex:v \"zz\" } } ; \
             DELETE WHERE { ?s ex:v \"s3\" } ; DELETE WHERE { ?s ex:v 2 }",
        );
        let after = s.snapshot();
        for q in &queries {
            check(&after, q);
        }
        // the snapshot before the updates answers the same way
        for q in queries.iter().take(10) {
            check(&before, q);
        }
    }
}

/// The cache runs on repeated values, says how often it reused a result, and steps
/// aside for nearly distinct inputs and impure expressions.
#[test]
fn admission_and_counters() {
    let s = store(0x2545_f491_4f6c_dd1d, 4000, false);
    let snap = s.snapshot();
    let rows = run_on(
        snap.clone(),
        "SELECT ?s ?v WHERE { ?s ex:v ?v }",
        Optimizations::ALL,
    )
    .len() as u64;
    let r = run_on(
        snap.clone(),
        "SELECT ?s ?x WHERE { ?s ex:v ?v BIND(UCASE(STR(?v)) AS ?x) }",
        Optimizations::ALL,
    );
    let (hits, misses) = (
        counter(&r.plan, "exprCacheHits"),
        counter(&r.plan, "exprCacheMisses"),
    );
    assert_eq!(hits + misses, rows, "{:#?}", r.plan);
    assert!(misses < 200 && hits > rows / 2, "{hits} {misses}");
    let mut n = Vec::new();
    notes(&r.plan, &mut n);
    assert!(n[0].contains("once per"), "{n:?}");

    // values that hardly repeat: row by row, with the reason
    let ids: String = (0..3000)
        .map(|i| {
            format!(
                "<http://ex.org/u{i}> <http://ex.org/id> \"id{}\" .\n",
                i % 2900
            )
        })
        .collect();
    s.load(&[Source::from_bytes(
        ids.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let r = run_on(
        s.snapshot(),
        "SELECT ?s ?x WHERE { ?s ex:id ?v BIND(UCASE(?v) AS ?x) }",
        Optimizations::ALL,
    );
    let mut n = Vec::new();
    notes(&r.plan, &mut n);
    assert!(n.len() == 1 && n[0].contains("row by row"), "{n:?}");
    assert_eq!(counter(&r.plan, "exprCacheSkipped"), 1);

    // impure expressions are evaluated on every row
    for (q, col) in [
        (
            "SELECT ?v ?x WHERE { ?s ex:v ?v BIND(RAND() + STRLEN(STR(?v)) AS ?x) }",
            1,
        ),
        ("SELECT ?v ?x WHERE { ?s ex:v ?v BIND(STRUUID() AS ?x) }", 1),
        (
            "SELECT ?v ?x WHERE { ?s ex:v ?v BIND(BNODE(STR(?v)) AS ?x) }",
            1,
        ),
    ] {
        let r = run_on(snap.clone(), q, Optimizations::ALL);
        let mut n = Vec::new();
        notes(&r.plan, &mut n);
        assert!(n.is_empty(), "{q}: {n:?}");
        let distinct: std::collections::HashSet<_> =
            r.rows().into_iter().map(|row| row[col].clone()).collect();
        // one value per solution (BNODE: per solution and string)
        assert!(distinct.len() as u64 > rows / 2, "{q}: {}", distinct.len());
    }

    // a constant expression is evaluated once
    let r = run_on(
        snap.clone(),
        "SELECT ?s ?x WHERE { ?s ex:v ?v BIND(CONCAT(\"a\", STR(YEAR(NOW()))) AS ?x) }",
        Optimizations::ALL,
    );
    assert_eq!(counter(&r.plan, "exprCacheMisses"), 1, "{:#?}", r.plan);

    // the base IRI resolves relative IRIs the same way for every row
    let q =
        "BASE <http://base.org/dir/> SELECT ?v ?x WHERE { ?s ex:v ?v BIND(IRI(STR(?v)) AS ?x) }";
    assert!(check(&snap, q));

    // conjuncts over different variables are cached separately; EXISTS runs per row
    let q = "SELECT ?s ?v WHERE { ?s ex:v ?v FILTER(STRLEN(STR(?v)) > 1 && ?s != ex:s3 && EXISTS { ?s ex:w ?w }) }";
    let r = run_on(snap.clone(), q, Optimizations::ALL);
    let mut n = Vec::new();
    notes(&r.plan, &mut n);
    assert!(n.iter().any(|n| n.contains("STRLEN")), "{n:?}");
    assert!(!n.iter().any(|n| n.contains("EXISTS")), "{n:?}");
    check(&snap, q);

    // the switch
    let o = Optimizations::ALL.disable("expr_cache").unwrap();
    assert!(!o.expr_cache);
}
