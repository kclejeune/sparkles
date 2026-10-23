//! The pattern answers the UI's unit tests of `lib/pattern.ts` start from, in
//! `ui/src/lib/pattern-fixtures.json`: the module's answer for each query below, so the
//! splice tests run without the built module and still see exactly what it answers.
//! `SPARKLES_FMT_BLESS=1` rewrites the file; otherwise the test fails when it is stale.

use serde_json::{Map, Value};
use std::path::Path;

const QUERIES: &[(&str, &str)] = &[
    (
        "a3",
        "PREFIX ex: <http://example.org/>\nSELECT ?person ?org WHERE {\n  ?person a ex:Person ; ex:worksFor ?org .\n  ?org a ex:Org ; ex:locatedIn ex:lyon .\n}\nLIMIT 1000\n",
    ),
    (
        "bind",
        "PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?person ex:born ?b .\n  BIND(YEAR(NOW()) AS ?y) # year\n  ?person ex:name ?name .\n  FILTER(STRSTARTS(?name, \"A\"))\n}\n",
    ),
    (
        "locked-after",
        "PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?a ex:p ?b .\n  BIND(?b + 1 AS ?c)\n  MINUS { ?a ex:q ?d }\n}\n",
    ),
    (
        "lists",
        "PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:p ?a, ?b , ?c ;\n     ex:q ?d ;\n     ex:r ?e .\n}\n",
    ),
    ("one-line", "SELECT * { ?s <http://x/p> ?o }"),
    ("no-dot", "SELECT * {\n  ?s <http://x/p> ?o # last\n}\n"),
    (
        "optional",
        "PREFIX ex: <http://example.org/>\nSELECT * WHERE {\n  ?org ex:name ?n .\n  OPTIONAL { ?org ex:locatedIn ?c . } .\n  OPTIONAL { ?org ex:size ?z . ?z ex:unit ?u }\n}\n",
    ),
    (
        "crlf",
        "SELECT * {\r\n  ?s <http://x/p> ?o .\r\n  ?o <http://x/q> ?r .\r\n}\r\n",
    ),
    ("empty", "SELECT * WHERE {\n}\n"),
    ("update", "INSERT DATA { <http://x/a> <http://x/b> 1 }"),
    ("broken", "SELECT * WHERE { ?s ?p }"),
    // the regressions of the first review (target/audits/u08-phase1)
    (
        "two",
        "PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:p ?a .\n  ?s ex:q ?b .\n}\n",
    ),
    (
        "two-moved",
        "PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:z ?z .\n  ?s ex:p ?a .\n  ?s ex:q ?b .\n}\n",
    ),
    (
        "optional-iri",
        "PREFIX ex: <http://example.org/>\nSELECT * {\n  ?s ex:p ?o .\n  OPTIONAL { ex:a ex:q ?x }\n}\n",
    ),
    (
        "locked-stmt",
        "SELECT * {\n  ?s <http://p> [ <http://q> 1 ] # c\n}\n",
    ),
    (
        "dot-comment",
        "SELECT * {\n  ?s <http://p> ?v .\n  OPTIONAL { ?s <http://p> ?o } # note\n  . FILTER(?v > 1)\n  .\n}\n",
    ),
    ("describe", "DESCRIBE <http://x/a>"),
    (
        "construct",
        "CONSTRUCT { ?s <http://x/q> ?o } FROM <http://g> WHERE { ?s <http://x/p> ?o }",
    ),
    (
        "base",
        "BASE <http://x/>\nSELECT * { <thing> <p> ?o . ?o <q> <http://x/thing> }",
    ),
    (
        "shared-hull",
        "SELECT * { ?s <http://p> ?o . OPTIONAL { ?s <http://q> ?o } }",
    ),
    (
        "optional-filter",
        "SELECT * { ?s <http://p> ?o . OPTIONAL { ?s <http://q> ?x . FILTER(?o > 2) FILTER(?x > 3) } FILTER(?o < 9) }",
    ),
    (
        "literal",
        "SELECT * { ?person <http://name> \"Ana\" ; <http://knows> ?other }",
    ),
    // the regressions of the second review
    (
        "dups",
        "SELECT * {\n  ?s <http://p> ?o .\n  ?s <http://p> ?o .\n  OPTIONAL { ?s <http://p> ?o }\n}\n",
    ),
    (
        "dups-changed",
        "SELECT * {\n  ?s <http://q> ?o .\n  ?s <http://p> ?o .\n  OPTIONAL { ?s <http://p> ?o }\n}\n",
    ),
    ("undeclared", "SELECT * { ?s ex:p ?o }"),
    // the regressions of the third review
    (
        "optional-undeclared",
        "SELECT * {\n  ?x <http://r> ?y .\n  OPTIONAL { ex:a ex:p ?o }\n}\n",
    ),
    (
        "relative-base",
        "BASE <../>\nSELECT * {\n  ?x <http://r> ?y .\n  OPTIONAL { <g> <http://p> ?o }\n}\n",
    ),
    (
        "literal-renamed",
        "SELECT * { ?someone <http://name> \"Ana\" ; <http://knows> ?other }",
    ),
];

#[test]
fn fixtures_are_the_module_answers() {
    let mut all = Map::new();
    for (name, text) in QUERIES {
        let answer: Value = serde_json::from_str(&sparkles_fmt_wasm::pattern(
            &serde_json::json!({ "text": text }).to_string(),
        ))
        .unwrap();
        all.insert(
            (*name).to_string(),
            serde_json::json!({ "text": text, "answer": answer }),
        );
    }
    let want = Value::Object(all);
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/lib/pattern-fixtures.json");
    if std::env::var_os("SPARKLES_FMT_BLESS").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(&want).unwrap() + "\n").unwrap();
        return;
    }
    // compared as JSON, so the UI's formatter may lay the file out its own way
    let have: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    assert!(
        have == want,
        "{} is stale: rerun with SPARKLES_FMT_BLESS=1",
        path.display()
    );
}
