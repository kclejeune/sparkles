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
    let bindings = d.bind(&given(g), &prefixes()).unwrap();
    let opts = QueryOptions {
        initial_bindings: bindings.initial,
        list_bindings: bindings.lists,
        ..Default::default()
    };
    let r = query(s.snapshot(), &d.query, &opts).unwrap();
    r.test_application_rows()
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
    assert!(names(&s, &d, json!({ "name": "Ann\" } UNION { ?p ?q ?r } #" })).is_empty());
    assert_eq!(
        d.bind(&given(json!({ "name": "x\" }" })), &prefixes())
            .unwrap()
            .initial[0]
            .1,
        Term::Literal(Literal::new_simple_literal("x\" }"))
    );
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
            .unwrap()
            .initial[0]
            .1,
        Term::NamedNode(NamedNode::new_unchecked("http://ex.org/a"))
    );
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
    assert_eq!(b.initial.len(), 4);
    assert!(
        b.initial
            .iter()
            .any(|(n, t)| n == "t" && t.to_string() == "\"hi\"@en")
    );
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
            "queries",
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
            Some("ann"),
            true,
        )
        .unwrap();
    assert!(s1.changed && s1.created);
    assert_eq!(s1.stored.version.version, 1);
    assert_eq!(s1.stored.version.parent, None);
    let again = c
        .put("adults", d1.clone(), Change::default(), None, true)
        .unwrap();
    assert!(!again.changed);
    assert_eq!(again.stored.version.version, 1);
    let mut d2 = d1.clone();
    d2.description = Some("People of age".into());
    let e = c
        .put(
            "adults",
            d2.clone(),
            Change {
                if_version: Some(0),
                ..Default::default()
            },
            None,
            true,
        )
        .unwrap_err();
    assert!(matches!(e, Error::PreconditionFailed(_)), "{e}");
    let s2 = c
        .put("adults", d2.clone(), Change::default(), None, true)
        .unwrap();
    assert_eq!(s2.stored.version.version, 2);
    assert_eq!(s2.stored.version.parent, Some(1));
    assert_ne!(s2.stored.version.digest, s1.stored.version.digest);
    assert!(c
        .put("bad name", d1.clone(), Change::default(), None, true)
        .is_err());
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
    assert!(c2.delete("adults", Some(1), None, true).is_err());
    assert!(c2.delete("adults", None, None, true).unwrap());
    assert!(!c2.delete("adults", None, None, true).unwrap());
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
        c.put("q", d, Change::default(), None, true).unwrap();
    }
    let v = c.versions("q").unwrap();
    assert_eq!(v.len(), MAX_VERSIONS);
    assert_eq!(v[0].version, MAX_VERSIONS as u64 + 5);
    assert!(c.get("q", Some(1)).is_none());
}

#[test]
fn stored_updates_are_checked() {
    let u: Definition = serde_json::from_value(json!({
        "kind": "update",
        "query": "DELETE { ?s <http://ex.org/status> ?o } INSERT { ?s <http://ex.org/status> \"done\" } WHERE { ?s <http://ex.org/status> ?o FILTER(?s = ?item) }",
        "parameters": { "item": { "type": "iri" } }
    }))
    .unwrap();
    assert_eq!(u.check().unwrap(), Kind::Update);

    let bad_query_as_update: Definition = serde_json::from_value(json!({
        "kind": "update",
        "query": "SELECT * WHERE { ?s ?p ?o }",
    }))
    .unwrap();
    let e = bad_query_as_update.check().unwrap_err().to_string();
    assert!(e.contains("updates"), "{e}");

    let query_as_update: Definition = serde_json::from_value(json!({
        "kind": "query",
        "query": "INSERT DATA { <a:a> <a:b> <a:c> }",
    }))
    .unwrap();
    let e = query_as_update.check().unwrap_err().to_string();
    assert!(e.contains("queries"), "{e}");

    let data_only_with_params: Definition = serde_json::from_value(json!({
        "kind": "update",
        "query": "INSERT DATA { <a:a> <a:b> <a:c> }",
        "parameters": { "x": { "type": "iri" } }
    }))
    .unwrap();
    let e = data_only_with_params.check().unwrap_err().to_string();
    assert!(e.contains("WHERE"), "{e}");

    let update_no_results: Definition = serde_json::from_value(json!({
        "kind": "update",
        "query": "INSERT DATA { <a:a> <a:b> <a:c> }",
        "results": "json"
    }))
    .unwrap();
    let e = update_no_results.check().unwrap_err().to_string();
    assert!(e.contains("result format"), "{e}");
}

#[test]
fn list_parameters() {
    let d = def(
        "SELECT ?name WHERE { ?p <http://ex.org/name> ?name ; <http://ex.org/age> ?age FILTER(?p = ?who) } ORDER BY ?name",
        json!({ "who": { "type": "iri", "list": true } }),
    );
    assert_eq!(d.check().unwrap(), Kind::Select);
    let s = store();
    let b = d
        .bind(
            &given(json!({ "who": ["<http://ex.org/a>", "<http://ex.org/c>"] })),
            &prefixes(),
        )
        .unwrap();
    assert!(b.initial.is_empty());
    assert_eq!(b.lists.len(), 1);
    assert_eq!(b.lists[0].0, "who");
    assert_eq!(b.lists[0].1.len(), 2);

    let opts = QueryOptions {
        list_bindings: b.lists,
        ..Default::default()
    };
    let r = query(s.snapshot(), &d.query, &opts).unwrap();
    let names: Vec<String> = r
        .test_application_rows()
        .into_iter()
        .map(|row| match &row[0] {
            Some(Term::Literal(l)) => l.value().to_string(),
            t => format!("{t:?}"),
        })
        .collect();
    assert_eq!(names, ["Ann", "Cy"]);

    let too_many = d.bind(
        &given(json!({ "who": (0..1001).map(|i| format!("<http://ex.org/{i}>")).collect::<Vec<_>>() })),
        &prefixes(),
    );
    assert!(too_many.is_err());

    let non_list_array = def(
        PEOPLE,
        json!({ "min": { "type": "integer" } }),
    );
    let e = non_list_array
        .bind(&given(json!({ "min": [1, 2] })), &prefixes())
        .unwrap_err()
        .to_string();
    assert!(e.contains("list"), "{e}");
}

#[test]
fn private_queries() {
    let c = Catalog::open(None).unwrap();
    let mut priv_def = def(PEOPLE, json!({ "min": { "type": "integer", "default": 18 } }));
    priv_def.visibility = Visibility::Private;
    priv_def.owner = Some("alice".into());
    c.put("my-query", priv_def.clone(), Change::default(), Some("alice"), false)
        .unwrap();
    assert_eq!(c.list_visible(Some("alice"), false).len(), 1);
    assert_eq!(c.list_visible(Some("bob"), false).len(), 0);
    assert_eq!(c.list_visible(None, true).len(), 1);
    assert!(c.get_visible("my-query", None, Some("alice"), false).is_some());
    assert!(c.get_visible("my-query", None, Some("bob"), false).is_none());
    assert!(c.get_visible("my-query", None, None, true).is_some());

    let e = c
        .delete("my-query", None, Some("bob"), false)
        .unwrap();
    assert!(!e);

    assert!(c.delete("my-query", None, Some("alice"), false).unwrap());

    let mut shared = def(PEOPLE, json!({}));
    shared.visibility = Visibility::Dataset;
    let e = c
        .put("shared", shared, Change::default(), Some("alice"), false)
        .unwrap_err()
        .to_string();
    assert!(e.contains("admin"), "{e}");
}

#[test]
fn format_1_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = r#"{"format":1,"queries":{"q":{"versions":[{"version":1,"created":"2026-01-01T00:00:00Z","digest":"abc","definition":{"query":"SELECT * WHERE { ?s ?p ?o }"}}]}}}"#;
    std::fs::write(dir.path().join(FILE), v1).unwrap();
    let c = Catalog::open(Some(dir.path())).unwrap();
    let s = c.get("q", None).unwrap();
    assert_eq!(s.definition.kind, DefKind::Query);
    assert_eq!(s.definition.visibility, Visibility::Dataset);
    assert!(s.definition.owner.is_none());
    assert!(!s.definition.parameters.values().any(|p| p.list));
}
