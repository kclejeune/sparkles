//! SP2Bench v1.01 q6 must not form the product of its OPTIONAL's two sides, which share
//! no variable and are linked only by the condition's `?author = ?author2`.
//! The fixture keeps the query's nine triple patterns and its exact pinned text.
use oxrdf::{Literal, NamedNode, Term};
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{Optimizations, QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::BTreeMap;

const Q6: &str = include_str!("fixtures/sp2b-q6.rq");

const AUTHORS: usize = 24;
const DOCS: usize = 6;

/// A document: its IRI, its class, its year (if issued) and its authors.
struct Doc {
    iri: String,
    class: &'static str,
    year: Option<i64>,
    authors: Vec<usize>,
}

fn docs() -> Vec<Doc> {
    let mut docs = Vec::new();
    for a in 0..AUTHORS {
        for k in 0..DOCS {
            // repeated years make ties for an author's first year
            let year = 1950 + ((a * 7 + k * 3) % 5) as i64;
            docs.push(Doc {
                iri: format!("urn:doc:{a}:{k}"),
                class: if k % 2 == 0 {
                    "bench:Article"
                } else {
                    "bench:Inproceedings"
                },
                // one document of every fourth author is never issued
                year: (k != 5 || a % 4 != 0).then_some(year),
                // a coauthor joins every third document
                authors: if k % 3 == 0 {
                    vec![a, (a + 1) % AUTHORS]
                } else {
                    vec![a]
                },
            });
        }
    }
    // a document of a class outside foaf:Document matches neither side
    docs.push(Doc {
        iri: "urn:doc:other".into(),
        class: "bench:Other",
        year: Some(1900),
        authors: vec![0],
    });
    docs
}

/// Every third author is a blank node, as SP2Bench writes most persons.
fn author(a: usize) -> String {
    if a % 3 == 0 {
        format!("_:p{a}")
    } else {
        format!("<urn:person:{a}>")
    }
}

fn fixture() -> Store {
    let store = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    let mut data = String::from(
        "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         @prefix foaf: <http://xmlns.com/foaf/0.1/> .\n\
         @prefix bench: <http://localhost/vocabulary/bench/> .\n\
         @prefix dc: <http://purl.org/dc/elements/1.1/> .\n\
         @prefix dcterms: <http://purl.org/dc/terms/> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
         bench:Article rdfs:subClassOf foaf:Document .\n\
         bench:Inproceedings rdfs:subClassOf foaf:Document .\n",
    );
    for a in 0..AUTHORS {
        data.push_str(&format!("{} foaf:name \"Person {a}\" .\n", author(a)));
    }
    for d in docs() {
        data.push_str(&format!("<{}> rdf:type {} .\n", d.iri, d.class));
        if let Some(y) = d.year {
            data.push_str(&format!(
                "<{}> dcterms:issued \"{y}\"^^xsd:integer .\n",
                d.iri
            ));
        }
        for a in &d.authors {
            data.push_str(&format!("<{}> dc:creator {} .\n", d.iri, author(*a)));
        }
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    store
}

/// The rows of q6 computed from the fixture's description: each issued document of a
/// foaf:Document class with each of its authors, where no issued document of that
/// author has an earlier year. Blank nodes print as `_`, as [`bag`] writes them.
fn oracle() -> Vec<String> {
    let docs = docs();
    let documents = |d: &&Doc| d.class != "bench:Other" && d.year.is_some();
    let mut first: BTreeMap<usize, i64> = BTreeMap::new();
    for d in docs.iter().filter(documents) {
        for a in &d.authors {
            let y = d.year.unwrap();
            first
                .entry(*a)
                .and_modify(|f| *f = (*f).min(y))
                .or_insert(y);
        }
    }
    let mut rows = Vec::new();
    for d in docs.iter().filter(documents) {
        for a in &d.authors {
            if d.year == first.get(a).copied() {
                let row = vec![
                    Some(Term::Literal(Literal::new_typed_literal(
                        d.year.unwrap().to_string(),
                        oxrdf::vocab::xsd::INTEGER,
                    ))),
                    Some(Term::Literal(Literal::new_simple_literal(format!(
                        "Person {a}"
                    )))),
                    Some(Term::NamedNode(NamedNode::new(d.iri.clone()).unwrap())),
                ];
                rows.push(format!("{row:?}"));
            }
        }
    }
    rows.sort();
    rows
}

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut result = rows
        .into_iter()
        .map(|r| format!("{r:?}"))
        .collect::<Vec<_>>();
    result.sort();
    result
}

fn options(max_rows: usize, keyed: bool) -> QueryOptions {
    QueryOptions {
        max_rows: Some(max_rows),
        no_cache: true,
        optimizations: Some(Optimizations {
            keyed_left_join: keyed,
            ..Optimizations::ALL
        }),
        ..Default::default()
    }
}

fn keyed_candidates(p: &sparkles_core::sparql::PlanInfo) -> Vec<String> {
    let mut todo = vec![p];
    let mut notes = Vec::new();
    while let Some(p) = todo.pop() {
        if p.description.contains("candidate pairs") {
            notes.push(p.description.clone());
        }
        todo.extend(&p.children);
    }
    notes
}

#[test]
fn q6_fixture_matches_an_independent_oracle_with_and_without_the_key() {
    let store = fixture();
    let expected = oracle();
    assert!(expected.len() > AUTHORS / 2, "{expected:?}");
    // the generic product fits this limit, so both plans can answer
    for keyed in [true, false] {
        let r = query(store.snapshot(), Q6, &options(1_000_000, keyed)).unwrap();
        assert_eq!(bag(r.rows()), expected, "keyed {keyed}");
    }
}

#[test]
fn q6_pairs_by_the_condition_equality_instead_of_the_product() {
    let store = fixture();
    // Both sides hold about 190 rows, so their product of about 36,000 rows exceeds
    // this limit. The pairs of rows with the same author and the result do not.
    let limit = 4096;
    let r = query(store.snapshot(), Q6, &options(limit, true)).unwrap();
    assert_eq!(bag(r.rows()), oracle());
    let notes = keyed_candidates(&r.plan);
    assert_eq!(notes.len(), 1, "{notes:?}");
    let generic = query(store.snapshot(), Q6, &options(limit, false));
    assert!(generic.is_err(), "the product should exceed {limit} rows");
}

/// A store whose values of ?x and ?y mix IRIs, blank nodes, numbers that are equal by
/// value with different ids, strings, language strings and unbound values.
fn mixed() -> Store {
    let store = Store::in_memory(StoreOptions::default());
    let data = r#"
        @prefix ex: <http://example.org/> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        ex:l1 ex:v ex:a .
        ex:l2 ex:v _:b .
        ex:l3 ex:v "1"^^xsd:integer .
        ex:l4 ex:v "01"^^xsd:integer .
        ex:l5 ex:v "1.0"^^xsd:decimal .
        ex:l6 ex:v "a" .
        ex:l7 ex:v "a"@en .
        ex:l8 ex:v "x"^^ex:unknown .
        ex:l9 ex:other ex:a .
        ex:r1 ex:w ex:a .
        ex:r2 ex:w _:b .
        ex:r3 ex:w "1"^^xsd:integer .
        ex:r4 ex:w "1"^^xsd:double .
        ex:r5 ex:w "a" .
        ex:r6 ex:w "a"@EN .
        ex:r7 ex:w "x"^^ex:unknown .
        ex:r8 ex:w "y"^^ex:unknown .
        ex:r9 ex:w ex:a .
    "#;
    store
        .load(&[Source::from_bytes(
            data.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    store
}

#[test]
fn keyed_optional_keeps_value_equality_errors_and_unbound_values() {
    let store = mixed();
    for (condition, guarded) in [
        ("?x = ?y", false),
        ("?y = ?x", false),
        ("?x = ?y && !sameTerm(?l, ex:l3)", false),
        ("(?x = ?y) && (?r != ex:r9)", false),
        // ?x is unbound on the left row without ex:v
        ("?x = ?y || true", true),
    ] {
        let text = format!(
            "PREFIX ex: <http://example.org/> SELECT ?l ?r WHERE {{ \
             ?l ?p ?any OPTIONAL {{ ?l ex:v ?x }} \
             OPTIONAL {{ ?r ex:w ?y FILTER({condition}) }} }}"
        );
        let run = |keyed| {
            let r = query(store.snapshot(), &text, &options(1_000_000, keyed)).unwrap();
            (bag(r.rows()), keyed_candidates(&r.plan).len())
        };
        let (on, used) = run(true);
        let (off, unused) = run(false);
        assert_eq!(on, off, "{condition}");
        assert_eq!(unused, 0, "{condition}");
        assert_eq!(used, usize::from(!guarded), "{condition}");
    }
}
