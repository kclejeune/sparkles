use super::*;
use serde_json::json;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:Employee rdfs:subClassOf ex:Person .
ex:p1 a ex:Person ; ex:name "Ann" ; ex:age 31 ; ex:email "ann@a.org", "ann@b.org" ;
  ex:knows ex:p2, ex:p3 ; ex:worksFor ex:o1 ;
  rdfs:label "Ann"@en, "Anne"@fr .
ex:p2 a ex:Employee ; ex:name "Bob" ; ex:age 25 ; ex:knows ex:p1 ; rdfs:label "Bob"@en .
ex:p3 a ex:Person ; ex:name "Cy" ; ex:age 45 ; ex:knows ex:p1 ; ex:worksFor ex:o1 .
ex:o1 a ex:Org ; ex:name "Acme" ; ex:population 3000000000 .
"#;

pub(crate) const SDL: &str = r#"
schema @rdf(vocab: "http://example.org/") @prefix(name: "ex", iri: "http://example.org/")
  @prefix(name: "rdfs", iri: "http://www.w3.org/2000/01/rdf-schema#") { query: Query }
type Person @rdf(iri: "ex:Person") {
  name: String
  age: Int
  email: [String!]!
  knows: [Person!]!
  knownBy: [Person!]! @rdf(iri: "ex:knows", inverse: true)
  worksFor: Org
  label: String @rdf(iri: "rdfs:label")
}
type Employee @rdf(iri: "ex:Employee") { name: String }
type Org { name: String population: Integer }
"#;

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn compiled(sdl: &str) -> Compiled {
    // without a Query type, the server generates one
    let sdl = sdl.replace(" { query: Query }", "");
    let sdl = if sdl.contains("extend schema") {
        sdl
    } else {
        sdl.replacen("schema @rdf", "extend schema @rdf", 1)
    };
    match Compiled::new(Config::new(sdl), 1, &|_, _, _| false) {
        Ok((c, _)) => c,
        Err(e) => panic!("{}", e.message()),
    }
}

fn run(c: &Compiled, s: &Store, q: &str) -> J {
    let r = execute_on(
        c,
        s.snapshot(),
        &Request {
            query: q.into(),
            ..Default::default()
        },
        &Options {
            explain: true,
            ..Default::default()
        },
    );
    r.body
}

#[test]
fn smoke() {
    let c = compiled(SDL);
    let s = store();
    let b = run(
        &c,
        &s,
        r#"{ person(id: "ex:p1") { id name age email knows { name } worksFor { name population } } }"#,
    );
    assert_eq!(
        b["data"],
        json!({ "person": {
            "id": "http://example.org/p1", "name": "Ann", "age": 31,
            "email": ["ann@a.org", "ann@b.org"],
            "knows": [{ "name": "Bob" }, { "name": "Cy" }],
            "worksFor": { "name": "Acme", "population": "3000000000" }
        }}),
        "{b:#}"
    );
}

fn data(b: &J) -> &J {
    assert!(b.get("errors").is_none(), "{b:#}");
    &b["data"]
}

fn codes(b: &J) -> Vec<String> {
    b["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| e["extensions"]["code"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn connections_and_pages() {
    let c = compiled(SDL);
    let s = store();
    let b = run(
        &c,
        &s,
        "{ allPerson(first: 2) { totalCount nodes { id } pageInfo { hasNextPage hasPreviousPage endCursor } } }",
    );
    let d = data(&b);
    assert_eq!(d["allPerson"]["totalCount"], 3, "{b:#}");
    assert_eq!(
        d["allPerson"]["nodes"],
        json!([{ "id": "http://example.org/p1" }, { "id": "http://example.org/p2" }])
    );
    assert_eq!(d["allPerson"]["pageInfo"]["hasNextPage"], true);
    assert_eq!(d["allPerson"]["pageInfo"]["hasPreviousPage"], false);
    let end = d["allPerson"]["pageInfo"]["endCursor"]
        .as_str()
        .unwrap()
        .to_string();
    let b = run(
        &c,
        &s,
        &format!(
            "{{ allPerson(first: 2, after: \"{end}\") {{ nodes {{ id }} pageInfo {{ hasNextPage hasPreviousPage }} }} }}"
        ),
    );
    let d = data(&b);
    assert_eq!(
        d["allPerson"]["nodes"],
        json!([{ "id": "http://example.org/p3" }])
    );
    assert_eq!(d["allPerson"]["pageInfo"]["hasNextPage"], false);
    assert_eq!(d["allPerson"]["pageInfo"]["hasPreviousPage"], true);
    // last without before reads the count
    let b = run(
        &c,
        &s,
        "{ allPerson(last: 1) { edges { node { id } cursor } } }",
    );
    assert_eq!(
        data(&b)["allPerson"]["edges"][0]["node"]["id"],
        "http://example.org/p3"
    );
    // a cursor of another field is refused
    let b = run(
        &c,
        &s,
        &format!("{{ allOrg(after: \"{end}\") {{ nodes {{ id }} }} }}"),
    );
    assert_eq!(codes(&b), ["CURSOR_INVALID"], "{b:#}");
}

#[test]
fn filters_and_orders() {
    let c = compiled(SDL);
    let s = store();
    let b = run(
        &c,
        &s,
        r#"{ allPerson(filter: { age: { gt: 30 }, knows: { name: { eq: "Ann" } } }) { nodes { name } } }"#,
    );
    assert_eq!(
        data(&b)["allPerson"]["nodes"],
        json!([{ "name": "Cy" }]),
        "{b:#}"
    );
    let b = run(
        &c,
        &s,
        r#"{ allPerson(filter: { name: { eq: "Ann\" } UNION { ?s ?p ?o } #" } }) { nodes { name } } }"#,
    );
    assert_eq!(data(&b)["allPerson"]["nodes"], json!([]));
    let b = run(
        &c,
        &s,
        r#"{ allPerson(orderBy: [AGE_DESC]) { nodes { name } } }"#,
    );
    assert_eq!(
        data(&b)["allPerson"]["nodes"],
        json!([{ "name": "Cy" }, { "name": "Ann" }, { "name": "Bob" }])
    );
    let b = run(
        &c,
        &s,
        r#"{ allPerson(filter: { or: [{ name: { startsWith: "B" } }, { not: { age: { lt: 40 } } }] }) { nodes { name } } }"#,
    );
    assert_eq!(
        data(&b)["allPerson"]["nodes"],
        json!([{ "name": "Bob" }, { "name": "Cy" }]),
        "{b:#}"
    );
    let b = run(
        &c,
        &s,
        r#"{ allPerson(filter: { not: { worksFor: {} } }) { nodes { name } } }"#,
    );
    assert_eq!(
        data(&b)["allPerson"]["nodes"],
        json!([{ "name": "Bob" }]),
        "{b:#}"
    );
    let b = run(
        &c,
        &s,
        r#"{ person(id: "ex:p1") { knows(orderBy: [NAME_DESC], first: 1) { name } knownBy { name } email(orderBy: DESC, first: 1) } }"#,
    );
    assert_eq!(
        data(&b)["person"],
        json!({ "knows": [{ "name": "Cy" }], "knownBy": [{ "name": "Bob" }, { "name": "Cy" }], "email": ["ann@b.org"] }),
        "{b:#}"
    );
}

#[test]
fn languages_numbers_and_lookups() {
    let c = compiled(SDL);
    let s = store();
    let b = run(
        &c,
        &s,
        r#"{ allPerson { nodes { fr: label(lang: ["fr", "en"]) de: label(lang: ["de"]) } } }"#,
    );
    assert_eq!(
        data(&b)["allPerson"]["nodes"],
        json!([{ "fr": "Anne", "de": null }, { "fr": "Bob", "de": null }, { "fr": null, "de": null }]),
        "{b:#}"
    );
    // a lookup of a node of another type, or of none, is null
    let b = run(
        &c,
        &s,
        r#"{ a: person(id: "ex:o1") { id } b: person(id: "ex:nobody") { id } c: employee(id: "ex:p2") { name } d: person(id: "ex:p2") { name } }"#,
    );
    assert_eq!(
        data(&b),
        &json!({ "a": null, "b": null, "c": { "name": "Bob" }, "d": { "name": "Bob" } })
    );
    let b = run(&c, &s, r#"{ person(id: "nope") { id } }"#);
    assert_eq!(codes(&b), ["BAD_USER_INPUT"]);
    let b = run(
        &c,
        &s,
        r#"{ node(id: "ex:o1") { __typename id ... on Org { name } } x: node(id: "ex:Employee") { __typename ... on Resource { _types } } }"#,
    );
    assert_eq!(
        data(&b),
        &json!({ "node": { "__typename": "Org", "id": "http://example.org/o1", "name": "Acme" },
                 "x": { "__typename": "Resource", "_types": [] } }),
        "{b:#}"
    );
}

#[test]
fn multiple_values_and_errors() {
    let c = compiled(SDL);
    let s = store();
    sparkles_core::sparql::update::update(
        &s,
        "INSERT DATA { <http://example.org/p3> <http://example.org/name> \"Cyrus\" }",
        &Default::default(),
    )
    .unwrap();
    let b = run(&c, &s, "{ allPerson { nodes { name } } }");
    assert_eq!(codes(&b), ["MULTIPLE_VALUES"], "{b:#}");
    assert_eq!(
        b["errors"][0]["path"],
        json!(["allPerson", "nodes", 2, "name"])
    );
    assert_eq!(b["data"]["allPerson"]["nodes"][2]["name"], J::Null);
    assert_eq!(b["data"]["allPerson"]["nodes"][0]["name"], "Ann");
    let strict = compiled(&SDL.replace("  name: String\n  age", "  name: String!\n  age"));
    let b = run(&strict, &s, "{ allPerson { nodes { name } } }");
    assert_eq!(codes(&b), ["MULTIPLE_VALUES"], "{b:#}");
    assert_eq!(b["data"]["allPerson"], J::Null, "{b:#}");
    let min = compiled(&SDL.replace(
        "  name: String\n  age",
        "  name: String @single(onMany: MIN)\n  age",
    ));
    let b = run(&min, &s, "{ person(id: \"ex:p3\") { name } }");
    assert_eq!(data(&b)["person"]["name"], "Cy");
    // Int over a value outside 32 bits
    let big = compiled(&SDL.replace("population: Integer", "population: Int"));
    let b = run(&big, &s, "{ allOrg { nodes { population } } }");
    assert_eq!(codes(&b), ["INVALID_VALUE"], "{b:#}");
}

#[test]
fn limits() {
    let c = compiled(SDL);
    let s = store();
    let b = run(
        &c,
        &s,
        "{ allPerson(first: 1000) { nodes { knows(first: 1000) { name } } } }",
    );
    assert_eq!(codes(&b), ["QUERY_TOO_COMPLEX"], "{b:#}");
    assert_eq!(b["errors"][0]["extensions"]["estimate"], 1001000);
    assert_eq!(b["errors"][0]["extensions"]["limit"], 100000);
    assert!(b.get("data").is_none());
    let b = run(&c, &s, "{ allPerson(first: -1) { nodes { name } } }");
    assert_eq!(codes(&b), ["BAD_USER_INPUT"]);
    let b = run(&c, &s, "mutation { x }");
    assert_eq!(codes(&b), ["GRAPHQL_VALIDATION_FAILED"]);
    let b = run(&c, &s, "{ allPerson { ");
    assert_eq!(codes(&b), ["GRAPHQL_PARSE_FAILED"]);
}

fn groups(c: &Compiled, s: &Store, q: &str) -> usize {
    let r = execute_on(
        c,
        s.snapshot(),
        &Request {
            query: q.into(),
            ..Default::default()
        },
        &Options {
            // the estimate of A5 is 1,000 + 1,000 × 100 = 101,000 nodes
            limits: plan::Limits {
                max_nodes: 200_000,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(r.body.get("errors").is_none(), "{:#}", r.body);
    r.groups
}

#[test]
fn groups_do_not_grow_with_nodes() {
    let c = compiled(SDL);
    let s = store();
    let q = "{ allPerson(first: 1000) { nodes { name knows { name } } } }";
    assert_eq!(groups(&c, &s, q), 2);
    let q = "{ allPerson(first: 1000) { nodes { name email knows { name } } } }";
    assert_eq!(groups(&c, &s, q), 3);
}

#[test]
fn explained_sparql_returns_the_group_rows() {
    let c = compiled(SDL);
    let s = store();
    let b = run(
        &c,
        &s,
        r#"{ allPerson(filter: { age: { gte: 25 } }, orderBy: [NAME_ASC]) { totalCount nodes { name email knows(first: 1) { name label(lang: ["en"]) } } } }"#,
    );
    let plan = b["extensions"]["sparkles"]["plan"]
        .as_array()
        .unwrap()
        .clone();
    assert!(plan.len() >= 4, "{b:#}");
    for g in plan {
        let text = g["sparql"].as_str().unwrap();
        let r = sparkles_core::sparql::query(s.snapshot(), text, &Default::default())
            .unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(r.len() as u64, g["rows"].as_u64().unwrap(), "{text}");
    }
}

#[test]
fn introspection() {
    let c = compiled(SDL);
    let s = store();
    let b = run(
        &c,
        &s,
        "{ __schema { queryType { name } types { name } } __type(name: \"Person\") { fields { name } } }",
    );
    let d = data(&b);
    assert_eq!(d["__schema"]["queryType"]["name"], "Query");
    assert!(d["__type"]["fields"].as_array().unwrap().len() >= 7);
    let mut cfg = c.config.clone();
    cfg.introspection = false;
    let off = Compiled::new(cfg, 2, &|_, _, _| true).unwrap().0;
    let reader = Options {
        admin: false,
        ..Default::default()
    };
    let ask = |q: &str| {
        execute_on(
            &off,
            s.snapshot(),
            &Request {
                query: q.into(),
                ..Default::default()
            },
            &reader,
        )
        .body
    };
    assert_eq!(
        codes(&ask("{ __schema { queryType { name } } }")),
        ["GRAPHQL_VALIDATION_FAILED"]
    );
    assert_eq!(ask("{ __typename }")["data"]["__typename"], "Query");
}

const ABSTRACT_DATA: &str = r#"
@prefix ex: <http://example.org/> .
ex:p1 a ex:Person ; ex:name "Ann" ; ex:member ex:o1, ex:p2 ;
  ex:address [ a ex:Address ; ex:city "Paris" ; ex:country ex:fr ] .
ex:p2 a ex:Person ; ex:name "Bob" .
ex:o1 a ex:Org ; ex:name "Acme" .
ex:fr a ex:Country ; ex:name "France" .
ex:x ex:member ex:thing .
ex:thing a ex:Unmapped .
"#;

const ABSTRACT_SDL: &str = r#"
extend schema @rdf(vocab: "http://example.org/") @prefix(name: "ex", iri: "http://example.org/")
interface Agent { name: String }
type Person implements Agent { name: String member: [Member!]! agents: [Agent!]! @rdf(iri: "ex:member") address: Address }
type Org implements Agent { name: String }
union Member = Person | Org
type Address { city: String country: Country }
type Country { name: String }
"#;

#[test]
fn interfaces_unions_and_blank_nodes() {
    let c = compiled(ABSTRACT_SDL);
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        ABSTRACT_DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let b = run(
        &c,
        &s,
        r#"{ person(id: "ex:p1") {
             member { __typename ... on Person { name } ... on Org { oname: name } }
             agents { __typename name }
             address { id city country { name } } } }"#,
    );
    let d = data(&b);
    assert_eq!(
        d["person"]["member"],
        json!([{ "__typename": "Org", "oname": "Acme" }, { "__typename": "Person", "name": "Bob" }]),
        "{b:#}"
    );
    assert_eq!(
        d["person"]["agents"],
        json!([{ "__typename": "Org", "name": "Acme" }, { "__typename": "Person", "name": "Bob" }])
    );
    assert_eq!(d["person"]["address"]["country"]["name"], "France");
    // a blank node is looked up by its label
    let id = d["person"]["address"]["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("_:"), "{id}");
    let b = run(
        &c,
        &s,
        &format!("{{ address(id: \"{id}\") {{ city }} node(id: \"{id}\") {{ __typename }} }}"),
    );
    assert_eq!(
        data(&b),
        &json!({ "address": { "city": "Paris" }, "node": { "__typename": "Address" } })
    );
    // a node of no possible type is an error under a union
    let b = run(&c, &s, r#"{ node(id: "ex:thing") { __typename } }"#);
    assert_eq!(data(&b)["node"]["__typename"], "Resource");
}

#[test]
fn mapping_errors_name_their_lines() {
    let bad = "type Person { name: String }\ntype Org @rdf(iri: \"foo:Org\") { name: String }\n";
    let e = Compiled::new(Config::new(bad), 1, &|_, _, _| true)
        .err()
        .unwrap();
    let msg = e.message();
    assert!(msg.contains("line 1") && msg.contains("vocab"), "{msg}");
    assert!(
        msg.contains("line 2") && msg.contains("undeclared prefix"),
        "{msg}"
    );
    let bad = "extend schema @rdf(vocab: \"http://x.org/\")\ntype PersonFilter { a: String }\ntype Person { id: ID! @rdf(iri: \"http://x.org/id\") b(x: Int): String c: Int @rdf(inverse: true) }\n";
    let msg = Compiled::new(Config::new(bad), 1, &|_, _, _| true)
        .err()
        .unwrap()
        .message();
    assert!(msg.contains("PersonFilter"), "{msg}");
    let bad = "extend schema @rdf(vocab: \"http://x.org/\")\ntype Person { id: ID! @rdf(iri: \"http://x.org/id\") b(x: Int): String c: Int @rdf(inverse: true) }\n";
    let msg = Compiled::new(Config::new(bad), 1, &|_, _, _| true)
        .err()
        .unwrap()
        .message();
    assert!(msg.contains("Person.id"), "{msg}");
    assert!(msg.contains("declares arguments"), "{msg}");
    assert!(msg.contains("inverse"), "{msg}");
    // a non-null field the guard does not back is a warning
    let ok =
        "extend schema @rdf(vocab: \"http://x.org/\")\ntype Person { name: String! age: Int! }\n";
    let (_, warnings) = Compiled::new(Config::new(ok), 1, &|_, p, _| p.ends_with("name")).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].starts_with("Person.age"), "{warnings:?}");
}
