use super::*;
use serde_json::json;
use sparkles::io::{RdfFormat, Source};
use sparkles::store::{Store, StoreOptions};

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
    let sdl = sdl.replacen("schema @rdf", "extend schema @rdf", 1);
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
