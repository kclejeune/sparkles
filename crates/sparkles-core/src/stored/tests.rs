use super::*;
use crate::io::{RdfFormat, Source};
use crate::sparql::{QueryOptions, query};
use crate::store::{Store, StoreOptions};
use serde_json::json;

fn def(q: &str, params: Value) -> Definition {
    serde_json::from_value(json!({ "query": q, "parameters": params })).unwrap()
}

fn prefixes() -> BTreeMap<String, String> {
    BTreeMap::from([("ex".to_string(), "http://ex.org/".to_string())])
}

fn given(v: Value) -> BTreeMap<String, Value> {
    serde_json::from_value(v).unwrap()
}

const PEOPLE: &str = "SELECT ?name WHERE { ?p <http://ex.org/name> ?name ; <http://ex.org/age> ?age FILTER(?age >= ?min) } ORDER BY ?name";

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex.org/> .
ex:a ex:name \"Ann\" ; ex:age 30 .
ex:b ex:name \"Bob\" ; ex:age 17 .
ex:c ex:name \"Cy\" ; ex:age 45 ; ex:knows ex:a ."
            .to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn names(s: &Store, d: &Definition, g: Value) -> Vec<String> {
    let opts = QueryOptions {
        initial_bindings: d.bind(&given(g), &prefixes()).unwrap(),
        ..Default::default()
    };
    let r = query(s.snapshot(), &d.query, &opts).unwrap();
    r.rows()
        .into_iter()
        .map(|row| match &row[0] {
            Some(Term::Literal(l)) => l.value().to_string(),
            t => format!("{t:?}"),
        })
        .collect()
}

#[test]
fn parameters_bind_as_terms() {
    let d = def(
        PEOPLE,
        json!({ "min": { "type": "integer", "default": 18 } }),
    );
    assert_eq!(d.check().unwrap(), Kind::Select);
    let s = store();
    assert_eq!(names(&s, &d, json!({})), ["Ann", "Cy"]);
    assert_eq!(names(&s, &d, json!({ "min": "40" })), ["Cy"]);
    assert_eq!(names(&s, &d, json!({ "min": 0 })), ["Ann", "Bob", "Cy"]);
}

#[test]
fn values_cannot_inject_text() {
    let d = def(
        "SELECT ?p WHERE { ?p <http://ex.org/name> ?name }",
        json!({ "name": { "type": "string" } }),
    );
    let s = store();
    // a string is one literal, whatever it contains
    assert!(names(&s, &d, json!({ "name": "Ann\" } UNION { ?p ?q ?r } #" })).is_empty());
    assert_eq!(
        d.bind(&given(json!({ "name": "x\" }" })), &prefixes())
            .unwrap()[0]
            .1,
        Term::Literal(Literal::new_simple_literal("x\" }"))
    );
    // a term parameter takes exactly one term
    let t = def(
        "SELECT ?p WHERE { ?p <http://ex.org/knows> ?who }",
        json!({ "who": { "type": "term" } }),
    );
    for bad in [
        "ex:a } UNION { ?p ?q ?r",
        "ex:a ex:b",
        "?x",
        "_:b0",
        "",
        "{ }",
    ] {
        assert!(
            t.bind(&given(json!({ "who": bad })), &prefixes()).is_err(),
            "{bad}"
        );
    }
    assert_eq!(
        t.bind(&given(json!({ "who": "ex:a" })), &prefixes())
            .unwrap()[0]
            .1,
        Term::NamedNode(NamedNode::new_unchecked("http://ex.org/a"))
    );
    // an IRI parameter takes an IRI only
    let i = def(
        "SELECT ?n WHERE { ?who <http://ex.org/name> ?n }",
        json!({ "who": { "type": "iri" } }),
    );
    for bad in ["not an iri", "<http://ex.org/a> } #", "\"x\""] {
        assert!(
            i.bind(&given(json!({ "who": bad })), &prefixes()).is_err(),
            "{bad}"
        );
    }
    assert_eq!(
        names(&s, &i, json!({ "who": "<http://ex.org/b>" })),
        ["Bob"]
    );
}

#[test]
fn types_are_checked() {
    let d = def(
        "SELECT * WHERE { ?s ?p ?o FILTER(?o = ?v || ?o = ?d || ?o = ?b || ?o = ?l || ?o = ?t) }",
        json!({
            "v": { "type": "integer", "required": false },
            "d": { "type": "date", "required": false },
            "b": { "type": "boolean", "required": false },
            "l": { "type": "literal", "datatype": "http://www.w3.org/2001/XMLSchema#gYear", "required": false },
            "t": { "type": "string", "language": "en", "required": false },
        }),
    );
    d.check().unwrap();
    let p = prefixes();
    assert!(d.bind(&given(json!({ "v": "1.5" })), &p).is_err());
    assert!(d.bind(&given(json!({ "v": "x" })), &p).is_err());
    assert!(d.bind(&given(json!({ "d": "2020-13-01" })), &p).is_err());
    assert!(d.bind(&given(json!({ "b": "yes" })), &p).is_err());
    assert!(d.bind(&given(json!({ "l": "20x" })), &p).is_err());
    let b = d
        .bind(
            &given(json!({ "v": 7, "b": true, "l": "2020", "t": "hi" })),
            &p,
        )
        .unwrap();
    assert_eq!(b.len(), 4);
    assert!(
        b.iter()
            .any(|(n, t)| n == "t" && t.to_string() == "\"hi\"@en")
    );
    // unknown and missing names
    assert!(d.bind(&given(json!({ "zzz": 1 })), &p).is_err());
    let r = def(PEOPLE, json!({ "min": { "type": "integer" } }));
    let e = r.bind(&given(json!({})), &p).unwrap_err().to_string();
    assert!(e.contains("missing parameter 'min'"), "{e}");
}

#[test]
fn allowed_values() {
    let d = def(
        PEOPLE,
        json!({ "min": { "type": "integer", "enum": [18, 21], "default": 18 } }),
    );
    d.check().unwrap();
    assert!(d.bind(&given(json!({ "min": 21 })), &prefixes()).is_ok());
    assert!(d.bind(&given(json!({ "min": "21" })), &prefixes()).is_ok());
    assert!(d.bind(&given(json!({ "min": 30 })), &prefixes()).is_err());
}

#[test]
fn definitions_are_checked() {
    let bad = [
        (
            def("INSERT DATA { <a:a> <a:b> <a:c> }", json!({})),
            "updates",
        ),
        (def("SELECT ?x {", json!({})), "syntax"),
        (
            def(PEOPLE, json!({ "nope": { "type": "string" } })),
            "no variable ?nope",
        ),
        (
            def(
                "SELECT ?x WHERE { BIND(1 AS ?x) }",
                json!({ "x": { "type": "integer" } }),
            ),
            "assigns",
        ),
        (
            def(
                "SELECT ?x WHERE { VALUES ?x { 1 } }",
                json!({ "x": { "type": "integer" } }),
            ),
            "assigns",
        ),
        (
            def(
                "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
                json!({ "n": { "type": "integer" } }),
            ),
            "assigns",
        ),
        (
            def(
                "SELECT ?timeout WHERE { ?s ?p ?timeout }",
                json!({ "timeout": { "type": "integer" } }),
            ),
            "reserved",
        ),
        (
            def(
                PEOPLE,
                json!({ "min": { "type": "integer", "default": "old" } }),
            ),
            "default",
        ),
        (
            def(
                PEOPLE,
                json!({ "min": { "type": "integer", "language": "en" } }),
            ),
            "language",
        ),
    ];
    for (d, want) in bad {
        let e = d.check().unwrap_err().to_string();
        assert!(e.contains(want), "{want}: {e}");
    }
    // a variable whose name extends the parameter's is not the parameter
    let d = def(
        "SELECT ?minimum WHERE { ?s ?p ?minimum }",
        json!({ "min": { "type": "integer" } }),
    );
    assert!(d.check().is_err());
    let mut d = def(PEOPLE, json!({}));
    d.results = Some("turtle".into());
    assert!(d.check().is_err());
    d.results = Some("csv".into());
    assert!(d.check().is_ok());
}

#[test]
fn versions_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let c = Catalog::open(Some(dir.path())).unwrap();
    assert!(c.list().is_empty());
    let d1 = def(
        PEOPLE,
        json!({ "min": { "type": "integer", "default": 18 } }),
    );
    let s1 = c
        .put(
            "adults",
            d1.clone(),
            Change {
                author: Some("ann".into()),
                message: Some("first".into()),
                dataset_commit: Some(4),
                if_version: Some(0),
            },
        )
        .unwrap();
    assert!(s1.changed && s1.created);
    assert_eq!(s1.stored.version.version, 1);
    assert_eq!(s1.stored.version.parent, None);
    // the same definition adds no version
    let again = c.put("adults", d1.clone(), Change::default()).unwrap();
    assert!(!again.changed);
    assert_eq!(again.stored.version.version, 1);
    let mut d2 = d1.clone();
    d2.description = Some("People of age".into());
    // a stale version is refused
    let e = c
        .put(
            "adults",
            d2.clone(),
            Change {
                if_version: Some(0),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(e, Error::PreconditionFailed(_)), "{e}");
    let s2 = c.put("adults", d2.clone(), Change::default()).unwrap();
    assert_eq!(s2.stored.version.version, 2);
    assert_eq!(s2.stored.version.parent, Some(1));
    assert_ne!(s2.stored.version.digest, s1.stored.version.digest);
    assert!(c.put("bad name", d1.clone(), Change::default()).is_err());
    // reopened from the file
    let c2 = Catalog::open(Some(dir.path())).unwrap();
    assert_eq!(c2.get("adults", None).unwrap().definition, d2);
    assert_eq!(c2.get("adults", Some(1)).unwrap().definition, d1);
    let v: Vec<u64> = c2
        .versions("adults")
        .unwrap()
        .iter()
        .map(|v| v.version)
        .collect();
    assert_eq!(v, [2, 1]);
    assert_eq!(
        c2.get("adults", Some(1)).unwrap().version.author.as_deref(),
        Some("ann")
    );
    assert!(c2.delete("adults", Some(1)).is_err());
    assert!(c2.delete("adults", None).unwrap());
    assert!(!c2.delete("adults", None).unwrap());
    assert!(!dir.path().join(FILE).exists());
}

#[test]
fn old_versions_are_dropped() {
    let c = Catalog::open(None).unwrap();
    for i in 0..(MAX_VERSIONS + 5) {
        let mut d = def(
            PEOPLE,
            json!({ "min": { "type": "integer", "default": 18 } }),
        );
        d.description = Some(format!("v{i}"));
        c.put("q", d, Change::default()).unwrap();
    }
    let v = c.versions("q").unwrap();
    assert_eq!(v.len(), MAX_VERSIONS);
    assert_eq!(v[0].version, MAX_VERSIONS as u64 + 5);
    assert!(c.get("q", Some(1)).is_none());
}
