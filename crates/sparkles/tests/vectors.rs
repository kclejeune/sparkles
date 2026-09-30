//! Vector similarity: `spk:vector` literals, functions and exact `spk:vectorSearch`.

use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::update::update;
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};

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
        .into_iter()
        .map(|row| {
            row.into_iter()
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
