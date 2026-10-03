//! A measurement of the adapter on the benchmark data (`scripts/gen-data.py`, as
//! `scripts/bench.sh` generates it), not a test: for each document, the median time of
//! the GraphQL request, of its groups' SPARQL run directly, and of one hand-written
//! SPARQL query that answers the same question. The result cache is off throughout.
//!
//! `SPARKLES_GRAPHQL_BENCH_DATA=~/.cache/sparkles-bench/1m/data.nt cargo test --release
//! -p sparkles-graphql --test bench -- --ignored --nocapture`

use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::QueryOptions;
use sparkles::store::{Store, StoreOptions};
use sparkles_graphql::{Compiled, Config, Options, Request, execute_on};
use std::time::Instant;

const SDL: &str = r#"extend schema
  @prefix(name: "ex", iri: "http://example.org/")
  @prefix(name: "foaf", iri: "http://xmlns.com/foaf/0.1/")
type Person @rdf(iri: "ex:Person") {
  name: String @rdf(iri: "foaf:name")
  age: Int @rdf(iri: "foaf:age")
  salary: Decimal @rdf(iri: "ex:salary")
  knows: [Person!]! @rdf(iri: "foaf:knows")
  worksFor: Organization @rdf(iri: "ex:worksFor")
  authorOf: [Document!]! @rdf(iri: "ex:authorOf")
}
type Organization @rdf(iri: "ex:Organization") { name: String @rdf(iri: "foaf:name") city: String @rdf(iri: "ex:city") }
type Document @rdf(iri: "ex:Document") { title: String @rdf(iri: "ex:title") year: Int @rdf(iri: "ex:year") }
"#;

const P: &str = "PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

fn cases() -> Vec<(&'static str, &'static str, String)> {
    vec![
        (
            "lookup",
            r#"{ person(id: "http://example.org/person/1") { name age knows { name } } }"#,
            format!(
                "{P}SELECT ?name ?age ?kn WHERE {{ VALUES ?p {{ <http://example.org/person/1> }} ?p a/rdfs:subClassOf* ex:Person OPTIONAL {{ ?p foaf:name ?name }} OPTIONAL {{ ?p foaf:age ?age }} OPTIONAL {{ ?p foaf:knows ?k OPTIONAL {{ ?k foaf:name ?kn }} }} }}"
            ),
        ),
        (
            "page-100",
            "{ allPerson(first: 100) { nodes { id name age } } }",
            format!(
                "{P}SELECT ?p ?name ?age WHERE {{ {{ SELECT DISTINCT ?p WHERE {{ ?p a/rdfs:subClassOf* ex:Person }} ORDER BY ?p LIMIT 100 }} OPTIONAL {{ ?p foaf:name ?name }} OPTIONAL {{ ?p foaf:age ?age }} }} ORDER BY ?p"
            ),
        ),
        (
            "nested-1000",
            "{ allPerson(first: 1000) { nodes { name knows(first: 10) { name } } } }",
            format!(
                "{P}SELECT ?p ?name ?k ?kn WHERE {{ {{ SELECT DISTINCT ?p WHERE {{ ?p a/rdfs:subClassOf* ex:Person }} ORDER BY ?p LIMIT 1000 }} OPTIONAL {{ ?p foaf:name ?name }} OPTIONAL {{ ?p foaf:knows ?k OPTIONAL {{ ?k foaf:name ?kn }} }} }} ORDER BY ?p ?k"
            ),
        ),
        (
            "filter-order",
            "{ allPerson(first: 50, filter: { age: { gt: 60 } }, orderBy: [AGE_DESC]) { totalCount nodes { name age worksFor { name } } } }",
            format!(
                "{P}SELECT ?p ?name ?age ?on WHERE {{ {{ SELECT DISTINCT ?p ?age WHERE {{ ?p a/rdfs:subClassOf* ex:Person ; foaf:age ?age FILTER(?age > 60) }} ORDER BY DESC(?age) ?p LIMIT 50 }} OPTIONAL {{ ?p foaf:name ?name }} OPTIONAL {{ ?p ex:worksFor ?o OPTIONAL {{ ?o foaf:name ?on }} }} }} ORDER BY DESC(?age) ?p"
            ),
        ),
        (
            "sibling-lists",
            "{ allPerson(first: 1000) { nodes { name knows(first: 40) { name } authorOf(first: 40) { title } } } }",
            format!(
                "{P}SELECT ?p ?name ?kn ?t WHERE {{ {{ SELECT DISTINCT ?p WHERE {{ ?p a/rdfs:subClassOf* ex:Person }} ORDER BY ?p LIMIT 1000 }} OPTIONAL {{ ?p foaf:name ?name }} OPTIONAL {{ ?p foaf:knows ?k OPTIONAL {{ ?k foaf:name ?kn }} }} OPTIONAL {{ ?p ex:authorOf ?d OPTIONAL {{ ?d ex:title ?t }} }} }}"
            ),
        ),
    ]
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

#[test]
#[ignore]
fn bench() {
    let Ok(path) = std::env::var("SPARKLES_GRAPHQL_BENCH_DATA") else {
        eprintln!("set SPARKLES_GRAPHQL_BENCH_DATA to the benchmark's data.nt");
        return;
    };
    let store = Store::in_memory(StoreOptions::default());
    let t = Instant::now();
    store
        .load(&[Source::from_bytes(
            std::fs::read(&path).unwrap(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    eprintln!("loaded in {:.1} s", t.elapsed().as_secs_f64());
    let (c, _) = Compiled::new(Config::new(SDL), 1, &|_, _, _| true)
        .unwrap_or_else(|e| panic!("{}", e.message()));
    let qopts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let opts = Options {
        query: qopts.clone(),
        ..Default::default()
    };
    let runs: usize = std::env::var("BENCH_RUNS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(21);
    let explain = Options {
        explain: true,
        ..opts.clone()
    };
    println!(
        "| document | groups | GraphQL ms | groups as SPARQL ms | adapter ms (parse, plan, assemble) | one SPARQL query ms | rows of the one query |"
    );
    println!("|---|---|---|---|---|---|---|");
    for (name, doc, sparql) in cases() {
        let req = Request {
            query: doc.into(),
            ..Default::default()
        };
        let explained = execute_on(&c, store.snapshot(), &req, &explain);
        assert!(
            explained.body.get("errors").is_none(),
            "{name}: {:#}",
            explained.body
        );
        let groups: Vec<String> = explained.body["extensions"]["sparkles"]["plan"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|g| g["skipped"] != true)
            .map(|g| g["sparql"].as_str().unwrap().to_string())
            .collect();
        // the three measurements alternate, so that the machine's load affects each alike
        let (mut gql, mut adapter, mut direct, mut one) = (vec![], vec![], vec![], vec![]);
        let mut rows = 0;
        for i in 0..runs + 3 {
            let t = Instant::now();
            let r = execute_on(&c, store.snapshot(), &req, &opts);
            let g = t.elapsed().as_secs_f64() * 1000.0;
            assert!(r.body.get("errors").is_none());
            let r = execute_on(&c, store.snapshot(), &req, &explain);
            let tm = &r.body["extensions"]["sparkles"]["timing"];
            let own: f64 = ["parseMs", "planMs", "assembleMs"]
                .iter()
                .map(|k| tm[k].as_f64().unwrap())
                .sum();
            // the groups' own text, run directly (with their parent ids in VALUES)
            let t = Instant::now();
            for q in &groups {
                sparkles::sparql::query(store.snapshot(), q, &qopts).unwrap();
            }
            let d = t.elapsed().as_secs_f64() * 1000.0;
            let t = Instant::now();
            rows = sparkles::sparql::query(store.snapshot(), &sparql, &qopts)
                .unwrap()
                .len();
            let o = t.elapsed().as_secs_f64() * 1000.0;
            if i >= 3 {
                gql.push(g);
                adapter.push(own);
                direct.push(d);
                one.push(o);
            }
        }
        println!(
            "| {name} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {rows} |",
            groups.len(),
            median(gql),
            median(direct),
            median(adapter),
            median(one)
        );
    }
}
