//! Vector similarity: `spk:vector` literals, functions and exact `spk:vectorSearch`.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix spk: <urn:x-sparkles:> .
ex:a ex:emb "[1, 0, 0]"^^spk:vector ; a ex:Doc .
ex:b ex:emb "[0.8, 0.6, 0]"^^spk:vector ; a ex:Doc .
ex:c ex:emb "[0, 1, 0]"^^spk:vector ; a ex:Img .
ex:d ex:emb "[-1, 0, 0]"^^spk:vector .
ex:e ex:emb "[0, 0, 0]"^^spk:vector .
ex:f ex:emb "[1, 2]"^^spk:vector .
ex:g ex:emb "[1, NaN, 0]"^^spk:vector .
ex:g1 { ex:h ex:emb "[0.6, 0.8, 0]"^^spk:vector . ex:a ex:emb "[1, 0, 0]"^^spk:vector . }
ex:g2 { ex:a ex:emb "[1, 0, 0]"^^spk:vector . }
"#;

const P: &str = "PREFIX ex: <http://example.org/> PREFIX spk: <urn:x-sparkles:> ";

fn store(opts: StoreOptions) -> Store {
    let s = Store::in_memory(opts);
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s
}

/// Rows as "local-name value …" strings, numbers rounded to 6 digits.
fn rows(s: &Store, q: &str) -> Vec<String> {
    let r = query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default())
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    r.rows()
        .unwrap()
        .iter()
        .map(|row| {
            row.values()
                .iter()
                .map(|t| match t {
                    None => "-".to_string(),
                    Some(oxrdf::Term::NamedNode(n)) => {
                        n.as_str().rsplit('/').next().unwrap().into()
                    }
                    Some(oxrdf::Term::Literal(l)) => match l.value().parse::<f64>() {
                        Ok(x) => format!("{}", (x * 1e6).round() / 1e6),
                        Err(_) => l.value().to_string(),
                    },
                    Some(t) => t.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn err(s: &Store, q: &str) -> String {
    query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default())
        .err()
        .unwrap_or_else(|| panic!("{q}: no error"))
        .to_string()
}

#[test]
fn search_semantics() {
    let s = store(StoreOptions::default());
    // 1: default graph; zero norm, other dimension, malformed and named-graph rows excluded
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 3) } ORDER BY DESC(?score)"
        ),
        ["a 1", "b 0.8", "c 0"]
    );
    // 2: named graphs, one row per graph
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?g ?score { GRAPH ?g { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 5) } } ORDER BY DESC(?score) ?g"
        ),
        ["a g1 1", "a g2 1", "h g1 0.6"]
    );
    // 4: the join happens after top-k
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?score { ?s a ex:Doc . (?s ?score) spk:vectorSearch (ex:emb \"[0,1,0]\"^^spk:vector 2) }"
        ),
        ["b 0.6"]
    );
    // 5: euclidean distance, lower is better
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 3 \"metric:euclidean\") } ORDER BY ?score"
        ),
        ["a 0", "b 0.632456", "e 1"]
    );
    // 6: another dimension
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb \"[1,0]\"^^spk:vector) }"
        ),
        ["f 0.447214"]
    );
    let e = err(
        &s,
        "SELECT ?s { ?s spk:vectorSearch (ex:emb \"[1,0,0,0]\"^^spk:vector) }",
    );
    assert!(
        e.contains("dimension mismatch") && e.contains("2, 3") && e.contains("has 4"),
        "{e}"
    );
    // 7: malformed query, bad k, unknown option
    assert!(
        err(
            &s,
            "SELECT ?s { ?s spk:vectorSearch (ex:emb \"[1, x]\"^^spk:vector) }"
        )
        .contains("offset 4")
    );
    assert!(
        err(
            &s,
            "SELECT ?s { ?s spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 0) }"
        )
        .contains("k must be")
    );
    assert!(
        err(
            &s,
            "SELECT ?s { ?s spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 10 \"foo:bar\") }"
        )
        .contains("unknown option")
    );
    assert!(
        err(
            &s,
            "SELECT ?s { ?s spk:vectorSearch (ex:emb \"[0,0,0]\"^^spk:vector) }"
        )
        .contains("zero query vector")
    );
    // 8: entity queries
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb ex:b 2) } ORDER BY DESC(?score)"
        ),
        ["b 1", "a 0.8"]
    );
    assert!(rows(&s, "SELECT ?s { ?s spk:vectorSearch (ex:emb ex:zzz 2) }").is_empty());
    // the matched vector is the stored literal, byte for byte
    assert_eq!(
        rows(
            &s,
            "SELECT ?v { (ex:b ?sc ?v) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 3) }"
        ),
        ["[0.8, 0.6, 0]"]
    );
    // an unknown predicate has no rows
    assert!(
        rows(
            &s,
            "SELECT ?s { ?s spk:vectorSearch (ex:nope \"[1,0,0]\"^^spk:vector) }"
        )
        .is_empty()
    );
}

#[test]
fn merged_default_graphs_count_duplicates_once() {
    let s = store(StoreOptions {
        union_default_graph: true,
        ..Default::default()
    });
    // 3: (a, "[1,0,0]") in g1 and g2 is one solution, so it does not crowd out h
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 2) } ORDER BY DESC(?score)"
        ),
        ["a 1", "h 0.6"]
    );
}

#[test]
fn functions() {
    let s = store(StoreOptions::default());
    let one = |e: &str| rows(&s, &format!("SELECT ?x {{ BIND({e} AS ?x) }}"))[0].clone();
    assert_eq!(
        one("spk:dot(\"[1,2,3]\"^^spk:vector, \"[4,5,6]\"^^spk:vector)"),
        "32"
    );
    assert_eq!(
        one("spk:euclidean(\"[0,0]\"^^spk:vector, \"[3,4]\"^^spk:vector)"),
        "5"
    );
    assert_eq!(one("spk:dimension(\"[1,2,3]\"^^spk:vector)"), "3");
    assert_eq!(
        one("spk:cosine(\"[1,0]\"^^spk:vector, \"[0,0]\"^^spk:vector)"),
        "-"
    );
    assert_eq!(
        one("spk:cosine(\"[1,0]\"^^spk:vector, \"[1,0,0]\"^^spk:vector)"),
        "-"
    );
    assert_eq!(one("spk:cosine(\"[1,0]\", \"[1,0]\"^^spk:vector)"), "-");
    assert_eq!(
        one("spk:cosine(\"[1,0]\"^^spk:vector, \"[1,0]\"^^spk:vector)"),
        "1"
    );
    // over stored data
    assert_eq!(
        rows(
            &s,
            "SELECT ?s { ?s ex:emb ?v FILTER(spk:cosine(?v, \"[1,0,0]\"^^spk:vector) > 0.9) } ORDER BY ?s"
        ),
        ["a"]
    );
}

#[test]
fn updates_and_snapshots() {
    let s = store(StoreOptions::default());
    let q = "SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 2) } ORDER BY DESC(?score) ?s";
    // 10: inserted rows are searched; a tie breaks by id (base before delta)
    update(
        &s,
        &format!("{P}INSERT DATA {{ ex:z ex:emb \"[1.0, 0.0,0]\"^^spk:vector }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(rows(&s, q), ["a 1", "z 1"]);
    assert_eq!(rows(&s, "SELECT ?v { ex:z ex:emb ?v }"), ["[1.0, 0.0,0]"]);
    let before = s.snapshot();
    update(
        &s,
        &format!("{P}DELETE DATA {{ ex:a ex:emb \"[1, 0, 0]\"^^spk:vector }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(rows(&s, q), ["z 1", "b 0.8"]);
    // a reader of the earlier snapshot still sees a
    let r = query(before, &format!("{P}{q}"), &QueryOptions::default()).unwrap();
    assert_eq!(r.len(), 2);
    // 11: compaction changes nothing
    s.compact().unwrap();
    assert_eq!(rows(&s, q), ["z 1", "b 0.8"]);
}

#[test]
fn search_is_exact_on_larger_data() {
    // compare with a brute-force ranking by the scoring function
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from("@prefix spk: <urn:x-sparkles:> .\n");
    let mut x: u64 = 0x9e3779b97f4a7c15;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % 2000) as f32 / 1000.0 - 1.0
    };
    for i in 0..20_000 {
        let v: Vec<String> = (0..16).map(|_| format!("{}", rnd())).collect();
        ttl.push_str(&format!(
            "<urn:n{i}> <urn:emb> \"[{}]\"^^spk:vector .\n",
            v.join(",")
        ));
    }
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let qv: Vec<String> = (0..16)
        .map(|i| format!("{}", (i as f32 / 8.0) - 1.0))
        .collect();
    let qv = format!("\"[{}]\"^^spk:vector", qv.join(","));
    for metric in ["cosine", "dot", "euclidean"] {
        let order = if metric == "euclidean" {
            "?sc"
        } else {
            "DESC(?sc)"
        };
        let fast = rows(
            &s,
            &format!(
                "SELECT ?s ?sc {{ (?s ?sc) spk:vectorSearch (<urn:emb> {qv} 25 \"metric:{metric}\") }} ORDER BY {order} ?s"
            ),
        );
        let slow = rows(
            &s,
            &format!(
                "SELECT ?s ?sc {{ ?s <urn:emb> ?v BIND(spk:{metric}(?v, {qv}) AS ?sc) }} ORDER BY {order} ?s LIMIT 25"
            ),
        );
        assert_eq!(fast, slow, "{metric}");
    }
}

#[test]
fn searches_count_toward_the_memory_budget() {
    let s = store(StoreOptions::default());
    let opts = QueryOptions {
        max_memory_bytes: Some(8),
        ..Default::default()
    };
    let q = format!("{P}SELECT ?s {{ ?s spk:vectorSearch (ex:emb ex:a 3) }}");
    let Err(e) = query(s.snapshot(), &q, &opts) else {
        panic!("over budget");
    };
    assert!(
        matches!(e, sparkles_core::Error::BudgetExceeded(b) if b.kind == sparkles_core::BudgetKind::Memory),
        "{e}"
    );
}

#[test]
fn bound_queries_and_options() {
    let s = store(StoreOptions::default());
    // 4: the join after top-k, and candidates:join before it
    let q = "SELECT ?s ?score { ?s a ex:Doc . (?s ?score) spk:vectorSearch (ex:emb \"[0,1,0]\"^^spk:vector 2 OPTS) } ORDER BY DESC(?score) ?s";
    assert_eq!(rows(&s, &q.replace("OPTS", "")), ["b 0.6"]);
    assert_eq!(
        rows(&s, &q.replace("OPTS", "\"candidates:join\"")),
        ["b 0.6", "a 0"]
    );
    // a variable query: one search per bound entity or vector
    assert_eq!(
        rows(
            &s,
            "SELECT ?q ?s ?score { VALUES ?q { ex:a ex:c } (?s ?score) spk:vectorSearch (ex:emb ?q 2) } ORDER BY ?q DESC(?score)"
        ),
        ["a a 1", "a b 0.8", "c c 1", "c b 0.6"]
    );
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?score { BIND(\"[0,1,0]\"^^spk:vector AS ?v) (?s ?score) spk:vectorSearch (ex:emb ?v 1) }"
        ),
        ["c 1"]
    );
    // the variable must be bound by the rest of the group
    assert!(
        err(
            &s,
            "SELECT ?s { (?s ?score) spk:vectorSearch (ex:emb ?q 2) }"
        )
        .contains("not bound")
    );
    // a variable query and candidates together: per query, among the bound subjects
    assert_eq!(
        rows(
            &s,
            "SELECT ?q ?s { VALUES (?q ?s) { (ex:c ex:a) (ex:c ex:b) } (?s ?score) spk:vectorSearch (ex:emb ?q 1 \"candidates:join\") }"
        ),
        ["c b"]
    );
    // distinct:subject: at most one row per entity
    update(
        &s,
        &format!("{P}INSERT DATA {{ ex:b ex:emb \"[0.9, 0.1, 0]\"^^spk:vector }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let q = "SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 3 OPTS) } ORDER BY DESC(?score)";
    assert_eq!(
        rows(&s, &q.replace("OPTS", "")),
        ["a 1", "b 0.993884", "b 0.8"]
    );
    assert_eq!(
        rows(&s, &q.replace("OPTS", "\"distinct:subject\"")),
        ["a 1", "b 0.993884", "c 0"]
    );
    let named = rows(
        &s,
        "SELECT ?s ?score { GRAPH ?g { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 5 \"distinct:subject\") } } ORDER BY DESC(?score)",
    );
    assert_eq!(named, ["a 1", "h 0.6"]);
}

/// Query options with the ORDER BY rewrite to a vector search on or off.
fn topk_opts(on: bool) -> QueryOptions {
    let mut o = sparkles_core::sparql::Optimizations::ALL;
    o.vector_topk = on;
    QueryOptions {
        optimizations: Some(o),
        no_cache: true,
        ..Default::default()
    }
}

fn plan_has(p: &sparkles_core::sparql::PlanInfo, needle: &str) -> bool {
    p.description.contains(needle) || p.children.iter().any(|c| plan_has(c, needle))
}

/// The plan of a query with the rewrite on, whose rows are checked against the rows
/// with it off.
fn same_both_ways(s: &Store, q: &str) -> sparkles_core::sparql::PlanInfo {
    let run = |on: bool| {
        query(s.snapshot(), &format!("{P}{q}"), &topk_opts(on))
            .unwrap_or_else(|e| panic!("{q}: {e}"))
    };
    let (on, off) = (run(true), run(false));
    let on_rows = on.rows().unwrap();
    let off_rows = off.rows().unwrap();
    assert_eq!(
        on_rows
            .iter()
            .map(|row| row.values().to_vec())
            .collect::<Vec<_>>(),
        off_rows
            .iter()
            .map(|row| row.values().to_vec())
            .collect::<Vec<_>>(),
        "{q}"
    );
    on.plan().clone()
}

#[test]
fn order_by_similarity_with_limit_is_a_vector_search() {
    let s = store(StoreOptions::default());
    const BEST: &str = "the best rows of ORDER BY";
    let c = "\"[1,0,0]\"^^spk:vector";
    for q in [
        format!("SELECT ?s ?v {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, {c})) LIMIT 3"),
        format!("SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine({c}, ?v)) LIMIT 2"),
        format!(
            "SELECT ?s ?sc {{ ?s ex:emb ?v BIND(spk:cosine(?v, {c}) AS ?sc) }} ORDER BY DESC(?sc) LIMIT 2"
        ),
        format!("SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:dot(?v, {c})) LIMIT 2"),
        format!(
            "SELECT ?s {{ GRAPH ex:g1 {{ ?s ex:emb ?v }} }} ORDER BY DESC(spk:cosine(?v, {c})) LIMIT 1"
        ),
    ] {
        let plan = same_both_ways(&s, &q);
        assert!(plan_has(&plan, BEST), "{q}");
        assert!(!plan_has(&plan, "ran the generic plan"), "{q}");
    }
    // fewer rows with a score than the limit: the generic plan, with the error rows last
    let q = format!("SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, {c})) LIMIT 6");
    let plan = same_both_ways(&s, &q);
    assert!(plan_has(&plan, "ran the generic plan"), "{plan:#?}");
    // shapes that keep the generic plan
    for q in [
        format!("SELECT ?s {{ ?s ex:emb ?v }} ORDER BY ASC(spk:cosine(?v, {c})) LIMIT 2"),
        format!("SELECT ?s {{ ?s ex:emb ?v }} ORDER BY ASC(spk:euclidean(?v, {c})) LIMIT 2"),
        format!(
            "SELECT ?s {{ ?s ex:emb ?v FILTER(?s != ex:a) }} ORDER BY DESC(spk:cosine(?v, {c})) LIMIT 2"
        ),
        format!(
            "SELECT ?s {{ ?s ex:emb ?v ; a ex:Doc }} ORDER BY DESC(spk:cosine(?v, {c})) LIMIT 1"
        ),
        format!("SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, {c})) ?s LIMIT 2"),
        format!("SELECT ?v {{ ex:b ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, {c})) LIMIT 1"),
        "SELECT ?s { ?s ex:emb ?v } ORDER BY DESC(spk:cosine(?v, \"[0,0,0]\"^^spk:vector)) LIMIT 2"
            .to_string(),
        format!(
            "SELECT ?s ?g {{ GRAPH ?g {{ ?s ex:emb ?v }} }} ORDER BY DESC(spk:cosine(?v, {c})) LIMIT 2"
        ),
    ] {
        let plan = same_both_ways(&s, &q);
        assert!(!plan_has(&plan, BEST), "{q}");
    }
}

/// The rewrite on random vectors, with malformed ones and other dimensions among them,
/// against the generic plan, also after updates that the search overlays.
#[test]
fn order_by_similarity_on_random_vectors() {
    let s = Store::in_memory(StoreOptions::default());
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % 2_000_001) as f64 / 1_000_000.0 - 1.0
    };
    let mut ttl =
        String::from("@prefix ex: <http://example.org/> . @prefix spk: <urn:x-sparkles:> .\n");
    for i in 0..3000 {
        let v = match i % 97 {
            0 => "[1, 2".to_string(),
            1 => format!("[{}, {}]", next(), next()),
            _ => format!(
                "[{}]",
                (0..8)
                    .map(|_| next().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        ttl.push_str(&format!("ex:n{i} ex:emb \"{v}\"^^spk:vector .\n"));
    }
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let c = format!(
        "\"[{}]\"^^spk:vector",
        (0..8)
            .map(|_| next().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for f in ["cosine", "dot"] {
        for k in [1, 10, 250] {
            let q = format!(
                "SELECT ?s ?v {{ ?s ex:emb ?v }} ORDER BY DESC(spk:{f}(?v, {c})) LIMIT {k}"
            );
            let plan = same_both_ways(&s, &q);
            assert!(plan_has(&plan, "the best rows of ORDER BY"), "{q}");
        }
    }
    // the snapshot's changes since the vectors were packed
    update(
        &s,
        &format!("{P}DELETE WHERE {{ ex:n5 ex:emb ?v }} ; INSERT DATA {{ ex:new ex:emb {c} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let q = format!("SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, {c})) LIMIT 20");
    let plan = same_both_ways(&s, &q);
    assert!(plan_has(&plan, "the best rows of ORDER BY"));
}

/// The compact datatype `spk:vectorB64` is a vector wherever `spk:vector` is: in the
/// functions, as stored vectors (also next to JSON ones under one predicate), as a
/// query, and in the ORDER BY rewrite.
#[test]
fn compact_vectors() {
    use sparkles_core::vector::{canonical_b64, parse_b64};
    let b64 = |v: &[f32]| format!("\"{}\"^^spk:vectorB64", canonical_b64(v));
    let s = Store::in_memory(StoreOptions::default());
    let ttl = format!(
        "@prefix ex: <http://example.org/> . @prefix spk: <urn:x-sparkles:> .\n\
         ex:a ex:emb {} . ex:b ex:emb \"[0.8, 0.6, 0]\"^^spk:vector . ex:c ex:emb {} .\n\
         ex:d ex:emb \"AAAA\"^^spk:vectorB64 . ex:e ex:emb {} .",
        b64(&[1.0, 0.0, 0.0]),
        b64(&[0.0, 1.0, 0.0]),
        b64(&[1.0, 2.0]),
    );
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    assert_eq!(
        parse_b64(&canonical_b64(&[0.5, -1.5])).unwrap(),
        [0.5, -1.5]
    );
    // the functions read both forms alike
    let one = b64(&[1.0, 0.0, 0.0]);
    assert_eq!(
        rows(
            &s,
            &format!(
                "SELECT (spk:cosine({one}, \"[0.8,0.6,0]\"^^spk:vector) AS ?c) (spk:dimension({one}) AS ?d) {{}}"
            )
        ),
        ["0.8 3"]
    );
    assert_eq!(
        rows(
            &s,
            "SELECT (spk:dimension(\"AAAA\"^^spk:vectorB64) AS ?d) {}"
        ),
        ["-"]
    );
    // a search over both forms, from either form
    for q in ["\"[1,0,0]\"^^spk:vector".to_string(), one.clone()] {
        assert_eq!(
            rows(
                &s,
                &format!(
                    "SELECT ?s ?score {{ (?s ?score) spk:vectorSearch (ex:emb {q} 5) }} ORDER BY DESC(?score)"
                )
            ),
            ["a 1", "b 0.8", "c 0"],
            "{q}"
        );
    }
    // the ORDER BY rewrite, from a compact constant
    let q = format!("SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, {one})) LIMIT 2");
    let plan = same_both_ways(&s, &q);
    assert!(plan_has(&plan, "the best rows of ORDER BY"));
    assert_eq!(rows(&s, &q), ["a", "b"]);
    // a malformed compact query is refused like a malformed JSON one
    let e = err(
        &s,
        "SELECT ?s { ?s spk:vectorSearch (ex:emb \"AAA=\"^^spk:vectorB64) }",
    );
    assert!(e.contains("spk:vectorB64"), "{e}");
}

/// Vector searches run as cursor operators (item 829). A search with a constant vector is
/// a leaf bounded by its k. A search whose candidates or query values come from its input
/// reads the whole input before it ranks, so k of one over candidates split across input
/// batches of one row still returns the one global winner, as eager execution does.
#[test]
fn vector_searches_run_as_cursor_operators() {
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    let s = store(StoreOptions::default());
    let strict = |rows: usize| CursorOptions {
        batch_rows: rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let bag = |mut c: sparkles_core::sparql::QueryCursor| {
        let mut out = Vec::new();
        while let Some(b) = c.next_batch().unwrap() {
            for r in 0..b.len() {
                out.push(format!("{:?}", b.row(r).unwrap().values()));
            }
        }
        out.sort();
        out
    };
    let eager = |q: &str| {
        let r = query(s.snapshot(), q, &QueryOptions::default()).unwrap();
        let mut out: Vec<String> = r
            .rows()
            .unwrap()
            .iter()
            .map(|row| format!("{:?}", row.values()))
            .collect();
        out.sort();
        out
    };
    for q in [
        "SELECT ?s ?score { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 3) }",
        "SELECT ?s ?score { VALUES ?s { ex:a ex:b ex:c ex:d } (?s ?score) spk:vectorSearch (ex:emb \"[0,1,0]\"^^spk:vector 1 \"candidates:join\") }",
        "SELECT ?s ?score { ?s a ex:Doc . (?s ?score) spk:vectorSearch (ex:emb \"[0,1,0]\"^^spk:vector 1 \"candidates:join\") }",
        "SELECT ?q ?s ?score { VALUES ?q { ex:a ex:c } (?s ?score) spk:vectorSearch (ex:emb ?q 2) }",
    ] {
        let q = format!("{P}{q}");
        let expected = eager(&q);
        assert!(!expected.is_empty(), "{q}");
        for rows in [1, 2, 4096] {
            let c = select_cursor(s.snapshot(), &q, &QueryOptions::default(), &strict(rows))
                .unwrap_or_else(|e| panic!("{q}: {e}"));
            assert!(!c.plan().has_materialization(), "{q}");
            assert_eq!(bag(c), expected, "{q}; {rows}");
        }
    }
    // the global winner of the split candidates
    let q = format!(
        "{P}SELECT ?s {{ VALUES ?s {{ ex:a ex:c }} (?s ?score) spk:vectorSearch (ex:emb \"[0,1,0]\"^^spk:vector 1 \"candidates:join\") }}"
    );
    let c = select_cursor(s.snapshot(), &q, &QueryOptions::default(), &strict(1)).unwrap();
    assert_eq!(bag(c).len(), 1);
    assert_eq!(eager(&q).len(), 1);
}

/// A vector search whose query vector its group binds reports the counters of one eager
/// search however its input splits into batches: work such as `scored` and `searches`
/// adds up, and properties of the index such as `rows` keep their value (Sol review of
/// item 829, finding 4).
#[test]
fn streamed_vector_search_counters_match_one_search_over_the_input() {
    use sparkles_core::sparql::{CursorOptions, CursorPlan, FallbackPolicy, select_cursor};
    let s = store(StoreOptions::default());
    fn find(plan: &CursorPlan) -> Option<&CursorPlan> {
        if plan.operator.operator == "VectorSearch" {
            return Some(plan);
        }
        plan.children.iter().find_map(find)
    }
    let q = format!(
        "{P}SELECT ?q ?s ?score {{ VALUES ?q {{ ex:a ex:c }} (?s ?score) spk:vectorSearch (ex:emb ?q 2) }}"
    );
    let counters = |rows: usize| {
        let o = CursorOptions {
            batch_rows: rows,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut c = select_cursor(s.snapshot(), &q, &QueryOptions::default(), &o).unwrap();
        while c.next_batch().unwrap().is_some() {}
        find(c.plan())
            .and_then(|p| p.operator.counters.clone())
            .expect("vector search counters")
    };
    let one = counters(4096);
    assert_eq!(
        one.get("searches").and_then(|v| v.as_u64()),
        Some(2),
        "{one:?}"
    );
    for rows in [1, 2] {
        assert_eq!(counters(rows), one, "{rows}");
    }
}

/// Vector searches as cursor operators count the rows they produce against the work
/// limit as eager runs do: both modes count the same rows, and a limit one row short of
/// that count stops both (item 829).
#[test]
fn streamed_vector_searches_count_their_rows_as_eager_does() {
    use sparkles_core::BudgetKind;
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    let s = store(StoreOptions::default());
    for q in [
        "SELECT ?s ?score { VALUES ?s { ex:a ex:b ex:c ex:d } (?s ?score) spk:vectorSearch (ex:emb \"[0,1,0]\"^^spk:vector 2 \"candidates:join\") }",
        "SELECT ?q ?s ?score { VALUES ?q { ex:a ex:c } (?s ?score) spk:vectorSearch (ex:emb ?q 2) }",
    ] {
        let q = format!("{P}{q}");
        let with = |limit: Option<u64>, work: Option<Arc<AtomicU64>>| QueryOptions {
            no_cache: true,
            max_rows_produced: limit,
            work,
            ..Default::default()
        };
        let produced = query(s.snapshot(), &q, &with(None, None))
            .unwrap()
            .rows_produced;
        let streamed = |limit: Option<u64>, rows: usize| -> sparkles_core::Result<u64> {
            let work = Arc::new(AtomicU64::new(0));
            let o = CursorOptions {
                batch_rows: rows,
                fallback: FallbackPolicy::RejectMaterialization,
                ..Default::default()
            };
            let mut c = select_cursor(s.snapshot(), &q, &with(limit, Some(work.clone())), &o)?;
            while c.next_batch()?.is_some() {}
            Ok(work.load(Ordering::Relaxed))
        };
        for rows in [1, 4096] {
            assert_eq!(streamed(None, rows).unwrap(), produced, "{q}; {rows}");
            assert!(
                matches!(streamed(Some(produced - 1), rows), Err(sparkles_core::Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced),
                "{q}; {rows}"
            );
        }
        assert!(matches!(
            query(s.snapshot(), &q, &with(Some(produced - 1), None)),
            Err(sparkles_core::Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced
        ));
    }
}
