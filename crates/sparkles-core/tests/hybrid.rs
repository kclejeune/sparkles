//! `spk:hybridSearch`: a `text:query` ranking and a `spk:vectorSearch` ranking fused by
//! reciprocal rank fusion.
#![cfg(feature = "text")]

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::text::TextConfig;

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix spk: <urn:x-sparkles:> .
ex:a a ex:Doc ; rdfs:label "brown fox" ; ex:emb "[1, 0]"^^spk:vector .
ex:b a ex:Doc ; rdfs:label "brown bear" ; ex:emb "[0.8, 0.6]"^^spk:vector .
ex:c rdfs:label "red fox" ; ex:emb "[0, 1]"^^spk:vector .
ex:d a ex:Doc ; ex:emb "[0.6, 0.8]"^^spk:vector .
ex:e rdfs:label "brown" .
ex:g1 { ex:x rdfs:label "brown" ; ex:emb "[1, 0]"^^spk:vector . }
ex:g2 { ex:x rdfs:label "brown fox" . }
"#;

const P: &str = "PREFIX ex: <http://example.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX spk: <urn:x-sparkles:> ";

fn store(cfg: Option<TextConfig>) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    if let Some(cfg) = cfg {
        s.enable_text(cfg).unwrap();
    }
    s
}

/// Rows in result order as "name value …", numbers to 6 digits.
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

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn err(s: &Store, q: &str) -> String {
    query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default())
        .err()
        .unwrap_or_else(|| panic!("{q}: no error"))
        .to_string()
}

fn r6(x: f64) -> String {
    format!("{}", (x * 1e6).round() / 1e6)
}

/// A hybrid call in the default graph with the given text list, vector list and rest.
fn hybrid(text: &str, vector: &str, rest: &str) -> String {
    format!(
        "SELECT ?s ?score ?tr ?vr {{ (?s ?score ?tr ?vr) spk:hybridSearch (({text}) ({vector}) {rest}) }} ORDER BY DESC(?score) ?s"
    )
}

#[test]
fn fuses_both_rankings_with_missing_items() {
    let s = store(Some(TextConfig::default()));
    // text "brown": e first (the shortest literal), a and b tie; vector [1,0]: a, b, d, c
    let r = rows(
        &s,
        &hybrid("rdfs:label \"brown\"", "ex:emb \"[1,0]\"^^spk:vector", ""),
    );
    let k = 60.0;
    assert_eq!(
        r,
        [
            format!("a {} 2 1", r6(1.0 / (k + 2.0) + 1.0 / (k + 1.0))),
            format!("b {} 2 2", r6(2.0 / (k + 2.0))),
            // only in the text ranking, or only in the vector ranking: one rank unbound
            format!("e {} 1 -", r6(1.0 / (k + 1.0))),
            format!("d {} - 3", r6(1.0 / (k + 3.0))),
            format!("c {} - 4", r6(1.0 / (k + 4.0))),
        ]
    );
    // the result joins like any other pattern
    assert_eq!(
        rows(
            &s,
            "SELECT ?s { ?s a ex:Doc . (?s ?score) spk:hybridSearch ((rdfs:label \"brown\") (ex:emb \"[1,0]\"^^spk:vector)) } ORDER BY DESC(?score)"
        ),
        ["a", "b", "d"]
    );
    // a plain query string is the text list without predicates
    assert_eq!(
        rows(
            &s,
            "SELECT ?s { (?s ?score) spk:hybridSearch (\"brown\" (ex:emb \"[1,0]\"^^spk:vector) 2) } ORDER BY DESC(?score)"
        ),
        ["a", "b"]
    );
    // the plan names both searches and the fusion
    let (_, info) = sparkles_core::sparql::explain(
        s.snapshot(),
        &format!(
            "{P}{}",
            hybrid("rdfs:label \"brown\"", "ex:emb \"[1,0]\"^^spk:vector", "")
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    let plan = serde_json::to_string(&info).unwrap();
    assert!(plan.contains("HybridSearch"), "{plan}");
    assert!(plan.contains("brown") && plan.contains("rrf 60"), "{plan}");
    assert!(!plan.contains("spkhybrid"), "{plan}");
    // inside OPTIONAL
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?tr { ?s a ex:Doc OPTIONAL { (?s ?score ?tr) spk:hybridSearch ((rdfs:label \"bear\") (ex:emb \"[0,1]\"^^spk:vector 1)) } } ORDER BY ?s"
        ),
        ["a -", "b 1", "d -"]
    );
}

#[test]
fn ties_share_ranks_and_break_by_term() {
    let s = store(Some(TextConfig::default()));
    // vector [0,1]: c, d, b, a; text "brown": e, then a and b tied
    let q = hybrid("rdfs:label \"brown\"", "ex:emb \"[0,1]\"^^spk:vector", "");
    let r = rows(&s, &q);
    assert_eq!(r[0], format!("b {} 2 3", r6(1.0 / 62.0 + 1.0 / 63.0)));
    assert_eq!(r[1], format!("a {} 2 4", r6(1.0 / 62.0 + 1.0 / 64.0)));
    // c (vector rank 1) and e (text rank 1) tie on the fused score
    assert!(
        r[2..4].iter().any(|x| x.starts_with("c ")) && r[2..4].iter().any(|x| x.starts_with("e ")),
        "{r:?}"
    );
    assert_eq!(r[2].split(' ').nth(1), r[3].split(' ').nth(1));
    // a limit that cuts between them keeps the same one every time
    let cut = |s: &Store| {
        rows(
            s,
            "SELECT ?s { (?s ?score) spk:hybridSearch ((rdfs:label \"brown\") (ex:emb \"[0,1]\"^^spk:vector) 3) }",
        )
    };
    let first = cut(&s);
    assert_eq!(first.len(), 3);
    for _ in 0..5 {
        assert_eq!(cut(&s), first);
    }
}

#[test]
fn depths_limits_and_options() {
    let s = store(Some(TextConfig::default()));
    // the text list's limit and the vector list's k are their depths
    assert_eq!(
        sorted(rows(
            &s,
            &hybrid(
                "rdfs:label \"brown\" 1",
                "ex:emb \"[1,0]\"^^spk:vector 2",
                ""
            )
        )),
        [
            format!("a {} - 1", r6(1.0 / 61.0)),
            format!("b {} - 2", r6(1.0 / 62.0)),
            format!("e {} 1 -", r6(1.0 / 61.0)),
        ]
    );
    // the limit counts subjects
    let q = hybrid("rdfs:label \"brown\"", "ex:emb \"[1,0]\"^^spk:vector", "2");
    assert_eq!(rows(&s, &q).len(), 2);
    // rrf:0 scores 1 / rank; a weight of 0 keeps the list's subjects, with nothing from it
    assert_eq!(
        rows(
            &s,
            &hybrid(
                "rdfs:label \"brown\"",
                "ex:emb \"[1,0]\"^^spk:vector",
                "\"rrf:0\" \"weights:0,2\""
            )
        ),
        [
            "a 2 2 1".to_string(),
            "b 1 2 2".into(),
            format!("d {} - 3", r6(2.0 / 3.0)),
            "c 0.5 - 4".into(),
            "e 0 1 -".into(),
        ]
    );
    // euclidean: a lower distance ranks first
    assert_eq!(
        rows(
            &s,
            &hybrid(
                "rdfs:label \"nothing\"",
                "ex:emb \"[1,0]\"^^spk:vector 4 \"metric:euclidean\"",
                ""
            )
        )
        .iter()
        .map(|r| r.split(' ').next().unwrap().to_string())
        .collect::<Vec<_>>(),
        ["a", "b", "d", "c"]
    );
    // a constant subject keeps its row of the fused ranking
    assert_eq!(
        rows(
            &s,
            "SELECT ?score ?tr ?vr { (ex:b ?score ?tr ?vr) spk:hybridSearch ((rdfs:label \"brown\") (ex:emb \"[1,0]\"^^spk:vector) 1) }"
        ),
        [format!("{} 2 2", r6(2.0 / 62.0))]
    );
    // an entity query, and options of the vector list
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?vr { (?s ?score [] ?vr) spk:hybridSearch ((rdfs:label \"zebra\") (ex:emb ex:a 2 \"exact:true\")) } ORDER BY ?vr"
        ),
        ["a 1", "b 2"]
    );
}

#[test]
fn several_hits_of_a_subject_and_graphs() {
    let s = store(Some(TextConfig::default()));
    // under GRAPH ?g the rankings are per subject and graph
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?g ?tr ?vr { GRAPH ?g { (?s ?score ?tr ?vr) spk:hybridSearch ((rdfs:label \"brown\") (ex:emb \"[1,0]\"^^spk:vector)) } } ORDER BY DESC(?score)"
        ),
        ["x g1 1 1", "x g2 2 -"]
    );
    // a subject with several matching literals takes its best rank, once
    let multi = Store::in_memory(StoreOptions::default());
    multi
        .load(&[Source::from_bytes(
            br#"@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix spk: <urn:x-sparkles:> .
ex:m rdfs:label "brown" ; rdfs:comment "a brown and brown bear in brown woods" ; ex:emb "[0, 1]"^^spk:vector .
ex:n rdfs:label "brown dog" ; ex:emb "[1, 0]"^^spk:vector .
"#
            .to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    multi.enable_text(TextConfig::default()).unwrap();
    let r = rows(
        &multi,
        &hybrid("\"brown\"", "ex:emb \"[1,0]\"^^spk:vector", ""),
    );
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(
        r.contains(&format!("m {} 1 2", r6(1.0 / 61.0 + 1.0 / 62.0))),
        "{r:?}"
    );
    assert!(
        r.contains(&format!("n {} 2 1", r6(1.0 / 62.0 + 1.0 / 61.0))),
        "{r:?}"
    );
}

#[test]
fn budgets_and_errors() {
    // a depth above maxHits counts as none: more hits than maxHits is a budget error
    let s = store(Some(TextConfig {
        max_hits: 2,
        ..Default::default()
    }));
    let e = err(
        &s,
        &hybrid("rdfs:label \"brown\"", "ex:emb \"[1,0]\"^^spk:vector", ""),
    );
    assert!(e.contains("budget") || e.contains("limit"), "{e}");
    assert_eq!(
        rows(
            &s,
            &hybrid("rdfs:label \"brown\" 2", "ex:emb \"[1,0]\"^^spk:vector", "")
        )
        .len(),
        5,
        "e and one of a and b by text, a to d by vector"
    );
    let s = store(Some(TextConfig::default()));
    let v = "ex:emb \"[1,0]\"^^spk:vector";
    for (q, msg) in [
        (hybrid("\"brown\"", v, "0"), "limit"),
        (hybrid("\"brown\"", v, "10001"), "limit"),
        (
            hybrid("\"brown\"", "ex:emb \"[1,0]\"^^spk:vector 10001", ""),
            "k must be",
        ),
        (hybrid("\"brown\"", v, "\"rrf:-1\""), "rrf"),
        (hybrid("\"brown\"", v, "\"weights:1\""), "weights"),
        (hybrid("\"brown\"", v, "\"weights:0,0\""), "weights"),
        (hybrid("\"brown\"", v, "\"what:1\""), "unexpected"),
        (hybrid("\"brown\"", v, "3 3"), "twice"),
        (hybrid("\"brown\" \"highlight:\"", v, ""), "highlight"),
        (
            hybrid(
                "\"brown\"",
                "ex:emb \"[1,0]\"^^spk:vector \"candidates:join\"",
                "",
            ),
            "candidates:join",
        ),
        (hybrid("\"brown\"", "ex:emb", ""), "vector list"),
        (hybrid("\"brown\" ?x", v, ""), "constants"),
        (
            "SELECT ?s { ?d ex:emb ?q . (?s ?score) spk:hybridSearch ((\"brown\") (ex:emb ?q)) }"
                .into(),
            "vector literal or an entity",
        ),
        (
            "SELECT ?s { (?s ?score) spk:hybridSearch (\"brown\" \"x\") }".into(),
            "expected",
        ),
        (
            "SELECT ?s { (?s 1) spk:hybridSearch (\"brown\" (ex:emb \"[1,0]\"^^spk:vector)) }"
                .into(),
            "must be a variable",
        ),
    ] {
        let e = err(&s, &q);
        assert!(e.contains(msg), "{q}: {e}");
    }
    // without a full-text index
    let plain = store(None);
    let e = err(&plain, &hybrid("\"brown\"", v, ""));
    assert!(e.contains("no full-text index"), "{e}");
}
