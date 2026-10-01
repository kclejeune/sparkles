//! validate_shacl and validate_shex, over the same JSON-RPC client as the other tools.

use super::*;

/// The ShEx acceptance data: alice and bob conform, carol has no name and is 200,
/// acme has an arc its CLOSED shape does not allow.
const PEOPLE: &str = r#"@prefix ex:   <http://ex.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a ex:Person ; foaf:name "Alice" ; foaf:age 30 ; foaf:knows ex:bob .
ex:bob   a ex:Person ; foaf:name "Bob" ; foaf:knows ex:alice .
ex:carol a ex:Person ; foaf:age 200 .
ex:acme  a ex:Org ; foaf:name "ACME" ; ex:city "Paris" ; ex:mayor ex:bob .
"#;

fn people_server() -> McpServer {
    server_with(&[("ds", &[PEOPLE])], McpConfig::default())
}

fn update(s: &McpServer, ds: &str, u: &str) {
    sparkles::sparql::update::update(
        &s.state.get(ds).unwrap().store,
        u,
        &sparkles::sparql::QueryOptions::default(),
    )
    .unwrap();
}

/// The `key` field of each result.
#[cfg(feature = "shex")]
fn nodes(r: &Value, key: &str) -> Vec<String> {
    r["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x[key].as_str().unwrap().to_string())
        .collect()
}

// ------------------------------------------------------------------- ShEx ------

#[cfg(feature = "shex")]
const SCHEMA: &str = r#"PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
start = @ex:Person
ex:Person EXTRA a { a [ex:Person] ; foaf:name xsd:string ; foaf:age xsd:integer MAXINCLUSIVE 150 ? ; foaf:knows @ex:Person * }
ex:Org CLOSED { a [ex:Org] ; foaf:name . ; ex:city ["Paris" "Kyoto"] }
"#;

#[cfg(feature = "shex")]
const MAP: &str = "{FOCUS a ex:Person}@ex:Person,ex:acme@ex:Org";

#[cfg(feature = "shex")]
#[tokio::test(flavor = "multi_thread")]
async fn shex_report() {
    let server = people_server();
    let mut c = Client::start(server.clone());
    let r = c
        .structured("validate_shex", json!({"schema": SCHEMA, "shapeMap": MAP}))
        .await;
    assert_eq!(r["dataset"], "ds");
    assert_eq!(r["commit"], head(&server, "ds"));
    assert_eq!(r["reasoning"], false);
    assert_eq!(r["conforms"], false);
    assert_eq!(r["counts"], json!({"conformant": 2, "nonconformant": 2}));
    assert_eq!(r["truncated"], false);
    assert_eq!(r["warnings"], json!([]));
    // only the nonconformant results, in map order, in the dataset's prefixes
    assert_eq!(nodes(&r, "node"), ["ex:carol", "ex:acme"]);
    let carol = &r["results"][0];
    assert_eq!(carol["shape"], "ex:Person");
    assert_eq!(carol["status"], "nonconformant");
    assert!(
        carol["reason"].as_str().is_some_and(|s| !s.is_empty()),
        "{carol}"
    );
    let failures = carol["failures"].as_array().unwrap();
    assert!(
        failures.contains(&json!({"kind": "cardinality", "predicate": "foaf:name", "inverse": false, "min": 1, "max": 1, "count": 0})),
        "{carol}"
    );
    assert!(
        failures
            .contains(&json!({"kind": "facet", "constraint": "MAXINCLUSIVE 150", "value": "200"})),
        "{carol}"
    );
    let acme = &r["results"][1];
    assert_eq!(acme["shape"], "ex:Org");
    assert!(
        acme["failures"]
            .as_array()
            .unwrap()
            .contains(&json!({"kind": "closed", "predicate": "ex:mayor", "value": "ex:bob"})),
        "{acme}"
    );
    assert_eq!(
        r["prefixes"],
        json!({"ex": "http://ex.org/", "foaf": "http://xmlns.com/foaf/0.1/"})
    );
    // every result, in map order
    let r = c
        .structured(
            "validate_shex",
            json!({"schema": SCHEMA, "shapeMap": MAP, "onlyNonconformant": false}),
        )
        .await;
    assert_eq!(
        nodes(&r, "node"),
        ["ex:alice", "ex:bob", "ex:carol", "ex:acme"]
    );
    assert_eq!(
        nodes(&r, "status"),
        ["conformant", "conformant", "nonconformant", "nonconformant"]
    );
    assert!(r["results"][0].get("failures").is_none(), "{r}");
    // ShExJ, a START association, and a map that uses the dataset's prefixes only
    let shexj = sparkles_shex::Schema::parse_shexc(SCHEMA, None)
        .unwrap()
        .to_shexj()
        .to_string();
    let r = c
        .structured(
            "validate_shex",
            json!({"schema": shexj, "shapeMap": "ex:carol@START,<http://ex.org/bob>@<http://ex.org/Person>"}),
        )
        .await;
    assert_eq!(r["counts"], json!({"conformant": 1, "nonconformant": 1}));
    assert_eq!(r["results"][0]["shape"], "START");
}

#[cfg(feature = "shex")]
#[tokio::test(flavor = "multi_thread")]
async fn shex_max_results_and_snapshots() {
    let server = people_server();
    let mut c = Client::start(server.clone());
    let r = c
        .structured(
            "validate_shex",
            json!({"schema": SCHEMA, "shapeMap": MAP, "maxResults": 1}),
        )
        .await;
    assert_eq!(nodes(&r, "node"), ["ex:carol"]);
    assert_eq!(r["truncated"], true);
    assert_eq!(r["counts"]["nonconformant"], 2);
    let first = r["commit"].as_u64().unwrap();
    // fix carol and acme; the earlier commit still reads as before
    update(
        &server,
        "ds",
        "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/>
         DELETE DATA { ex:carol foaf:age 200 . ex:acme ex:mayor ex:bob } ;
         INSERT DATA { ex:carol foaf:name \"Carol\" }",
    );
    let r = c
        .structured("validate_shex", json!({"schema": SCHEMA, "shapeMap": MAP}))
        .await;
    assert_eq!(r["conforms"], true);
    assert_eq!(r["results"], json!([]));
    assert_eq!(r["commit"], first + 1);
    let r = c
        .structured(
            "validate_shex",
            json!({"schema": SCHEMA, "shapeMap": MAP, "atCommit": first}),
        )
        .await;
    assert_eq!(r["counts"]["nonconformant"], 2);
    assert_eq!(r["commit"], first);
}

#[cfg(feature = "shex")]
#[tokio::test(flavor = "multi_thread")]
async fn shex_errors() {
    let mut c = Client::start(people_server());
    let err = |schema: &str, map: &str| json!({"schema": schema, "shapeMap": map});
    // imports are refused
    let (t, e) = c
        .error(
            "validate_shex",
            err(
                &format!("IMPORT <http://ex.org/common.shex>\n{SCHEMA}"),
                MAP,
            ),
        )
        .await;
    assert_eq!(e, json!({"code": "bad-argument", "status": 400}));
    assert!(t.contains("does not resolve imports"), "{t}");
    assert!(t.contains("<http://ex.org/common.shex>"), "{t}");
    // SPARQL selectors are refused until the engine runs them
    if !super::super::validate::SPARQL_SELECTORS {
        let (t, e) = c
            .error(
                "validate_shex",
                err(
                    SCHEMA,
                    "SPARQL \"\"\"SELECT ?focus { ?focus a ex:Person }\"\"\"@ex:Person",
                ),
            )
            .await;
        assert_eq!(e, json!({"code": "unsupported", "status": 501}));
        assert!(t.contains("SPARQL node selectors"), "{t}");
    }
    // syntax errors name the line and column
    let (t, e) = c
        .error(
            "validate_shex",
            err("PREFIX ex: <http://ex.org/>\nex:S { ex:p @@ }", MAP),
        )
        .await;
    assert_eq!(e, json!({"code": "syntax", "status": 400}));
    assert!(
        t.starts_with("schema syntax error at line 2, column "),
        "{t}"
    );
    let (t, e) = c.error("validate_shex", err(SCHEMA, "ex:alice@")).await;
    assert_eq!(e, json!({"code": "syntax", "status": 400}));
    assert!(t.starts_with("shape map syntax error at line 1"), "{t}");
    // a schema that parses but cannot be used, and a label it does not define
    let (t, e) = c
        .error(
            "validate_shex",
            err(
                "PREFIX ex: <http://ex.org/>\nex:S { ex:p @ex:Missing }",
                "ex:alice@ex:S",
            ),
        )
        .await;
    assert_eq!(e, json!({"code": "invalid-schema", "status": 400}));
    assert!(t.contains("Missing"), "{t}");
    let (t, e) = c
        .error("validate_shex", err(SCHEMA, "ex:alice@ex:Nothing"))
        .await;
    assert_eq!(e, json!({"code": "invalid-schema", "status": 400}));
    assert!(t.contains("Nothing"), "{t}");
    // arguments
    for (args, needle) in [
        (json!({"schema": SCHEMA}), "missing field `shapeMap`"),
        (err(" ", MAP), "schema must not be empty"),
        (
            json!({"schema": SCHEMA, "shapeMap": MAP, "maxResults": 1001}),
            "maxResults must be ≤ 1000",
        ),
        (
            json!({"schema": SCHEMA, "shapeMap": MAP, "map": MAP}),
            "unknown field `map`",
        ),
        (
            json!({"schema": SCHEMA, "shapeMap": MAP, "graph": "not an iri"}),
            "invalid IRI",
        ),
    ] {
        let (t, e) = c.error("validate_shex", args.clone()).await;
        assert_eq!(e, json!({"code": "bad-argument", "status": 400}), "{args}");
        assert!(t.contains(needle), "{args}: {t}");
    }
}

#[cfg(feature = "shex")]
#[tokio::test(flavor = "multi_thread")]
async fn shex_budget_and_timeout() {
    // 640 bytes of memory: at most 10 typing pairs
    let cfg = McpConfig {
        query_memory_bytes: Some(640),
        ..McpConfig::default()
    };
    let mut c = Client::start(server_with(&[("t", &[&numbers(100)])], cfg));
    let schema = "PREFIX ex: <http://ex.org/>\nex:S { ex:v . }";
    let (t, e) = c
        .error(
            "validate_shex",
            json!({"schema": schema, "shapeMap": "{FOCUS ex:v _}@ex:S"}),
        )
        .await;
    assert_eq!(
        e,
        json!({"code": "budget-validation-work", "status": 507, "budget": "validation-work"})
    );
    assert!(t.contains("fewer focus nodes"), "{t}");
    let mut c = Client::start(server_with(
        &[("t", &[&numbers(3000)])],
        McpConfig::default(),
    ));
    let (t, e) = c
        .error(
            "validate_shex",
            json!({"schema": schema, "shapeMap": "{FOCUS ex:v _}@ex:S", "timeoutSeconds": 0.000001}),
        )
        .await;
    assert_eq!(e, json!({"code": "timeout", "status": 408}));
    assert!(
        t.starts_with("validation exceeded the 0.000001 s timeout"),
        "{t}"
    );
    assert!(t.contains("max 60"), "{t}");
}

// ------------------------------------------------------------------ SHACL ------

#[cfg(feature = "shacl")]
const SHAPES: &str = r#"@prefix sh:   <http://www.w3.org/ns/shacl#> .
@prefix ex:   <http://ex.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix xsd:  <http://www.w3.org/2001/XMLSchema#> .
ex:PersonShape a sh:NodeShape ;
  sh:targetClass ex:Person ;
  sh:property ex:NameShape ;
  sh:property [ sh:path foaf:age ; sh:maxInclusive 150 ; sh:severity sh:Warning ;
                sh:message "too old" ] .
ex:NameShape sh:path foaf:name ; sh:minCount 1 ; sh:datatype xsd:string .
"#;

#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn shacl_report() {
    let server = people_server();
    let mut c = Client::start(server.clone());
    let r = c
        .structured("validate_shacl", json!({"shapes": SHAPES}))
        .await;
    assert_eq!(r["dataset"], "ds");
    assert_eq!(r["commit"], head(&server, "ds"));
    assert_eq!(r["conforms"], false);
    assert_eq!(r["total"], 2);
    assert_eq!(
        r["bySeverity"],
        json!({"violation": 1, "warning": 1, "info": 0})
    );
    assert_eq!(r["truncated"], false);
    // most severe first
    let v = &r["results"][0];
    assert_eq!(v["focus"], "ex:carol");
    assert_eq!(v["path"], "foaf:name");
    assert_eq!(v["shape"], "ex:NameShape");
    assert_eq!(v["constraint"], "sh:MinCountConstraintComponent");
    assert_eq!(v["severity"], "Violation");
    assert!(v.get("value").is_none(), "{v}");
    let w = &r["results"][1];
    assert_eq!(
        (
            &w["focus"],
            &w["path"],
            &w["value"],
            &w["severity"],
            &w["message"]
        ),
        (
            &json!("ex:carol"),
            &json!("foaf:age"),
            &json!("200"),
            &json!("Warning"),
            &json!("too old")
        )
    );
    assert_eq!(w["constraint"], "sh:MaxInclusiveConstraintComponent");
    // a blank-node property shape keeps its label
    assert!(w["shape"].as_str().unwrap().starts_with("_:"), "{w}");
    assert_eq!(r["prefixes"]["sh"], "http://www.w3.org/ns/shacl#");
    // maxResults
    let r = c
        .structured("validate_shacl", json!({"shapes": SHAPES, "maxResults": 1}))
        .await;
    assert_eq!(r["total"], 2);
    assert_eq!(r["results"].as_array().unwrap().len(), 1);
    assert_eq!(r["results"][0]["severity"], "Violation");
    assert_eq!(r["truncated"], true);
    // a conforming dataset
    update(
        &server,
        "ds",
        "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/>
         DELETE DATA { ex:carol foaf:age 200 } ; INSERT DATA { ex:carol foaf:name \"Carol\" }",
    );
    let r = c
        .structured("validate_shacl", json!({"shapes": SHAPES}))
        .await;
    assert_eq!(
        (&r["conforms"], &r["total"], &r["results"]),
        (&json!(true), &json!(0), &json!([]))
    );
}

#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn shacl_graphs() {
    let server = people_server();
    let ds = server.state.get("ds").unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"<http://ex.org/g> { <http://ex.org/dave> a <http://ex.org/Person> . }".to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let mut c = Client::start(server);
    let r = c
        .structured("validate_shacl", json!({"shapes": SHAPES, "graph": "ex:g"}))
        .await;
    assert_eq!(r["total"], 1);
    assert_eq!(r["results"][0]["focus"], "ex:dave");
    let r = c
        .structured(
            "validate_shacl",
            json!({"shapes": SHAPES, "graph": "union"}),
        )
        .await;
    assert_eq!(r["total"], 3);
    let (t, e) = c
        .error(
            "validate_shacl",
            json!({"shapes": SHAPES, "graph": "<http://ex.org/none>"}),
        )
        .await;
    assert_eq!(e, json!({"code": "unknown-graph", "status": 404}));
    assert!(t.contains("graph=union"), "{t}");
}

#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn shacl_errors() {
    let mut c = Client::start(people_server());
    let (t, e) = c
        .error("validate_shacl", json!({"shapes": "ex:a ex:b"}))
        .await;
    assert_eq!(e, json!({"code": "syntax", "status": 400}));
    assert!(t.starts_with("shapes: "), "{t}");
    for (args, needle) in [
        (json!({}), "missing field `shapes`"),
        (json!({"shapes": ""}), "shapes must not be empty"),
        (
            json!({"shapes": SHAPES, "maxResults": 0}),
            "maxResults must be ≥ 1",
        ),
        (
            json!({"shapes": SHAPES, "timeoutSeconds": 61}),
            "timeoutSeconds must be > 0 and ≤ 60",
        ),
    ] {
        let (t, e) = c.error("validate_shacl", args.clone()).await;
        assert_eq!(e, json!({"code": "bad-argument", "status": 400}), "{args}");
        assert!(t.contains(needle), "{args}: {t}");
    }
}

#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn shacl_budget_and_timeout() {
    let shapes = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ex: <http://ex.org/> .
ex:S a sh:NodeShape ; sh:targetSubjectsOf ex:v ;
  sh:property [ sh:path ex:w ; sh:minCount 1 ] .
"#;
    // 1 MiB of memory: at most 2048 results at 512 bytes each
    let cfg = McpConfig {
        query_memory_bytes: Some(1 << 20),
        ..McpConfig::default()
    };
    let mut c = Client::start(server_with(&[("t", &[&numbers(3000)])], cfg));
    let (t, e) = c.error("validate_shacl", json!({"shapes": shapes})).await;
    assert_eq!(
        e,
        json!({"code": "budget-memory", "status": 507, "budget": "memory"})
    );
    assert!(t.contains("more than 2048 results"), "{t}");
    let (t, e) = c
        .error(
            "validate_shacl",
            json!({"shapes": shapes, "timeoutSeconds": 0.000001}),
        )
        .await;
    assert_eq!(e, json!({"code": "timeout", "status": 408}));
    assert!(t.starts_with("validation exceeded the"), "{t}");
    // the result's size stays within the server's maxBytes
    let cfg = McpConfig {
        max_bytes: 4096,
        ..McpConfig::default()
    };
    let mut c = Client::start(server_with(&[("t", &[&numbers(300)])], cfg));
    let r = c
        .structured(
            "validate_shacl",
            json!({"shapes": shapes, "maxResults": 300}),
        )
        .await;
    assert_eq!(r["total"], 300);
    assert_eq!(r["truncated"], true);
    let n = r["results"].as_array().unwrap().len();
    assert!(n > 0 && n < 300, "{n}");
    assert!(r["results"].to_string().len() <= 4096);
}

// -------------------------------------------------------------- tool set ------

#[tokio::test(flavor = "multi_thread")]
async fn disable_validation_tool() {
    let cfg = McpConfig {
        disabled: ["validate_shex".to_string()].into(),
        ..McpConfig::default()
    };
    let mut c = Client::start(server_with(&[("ds", &[PEOPLE])], cfg));
    let r = c.request(2, "tools/list", json!({"_meta": m()})).await;
    let names: Vec<&str> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"validate_shex"), "{names:?}");
    assert_eq!(names.contains(&"validate_shacl"), cfg!(feature = "shacl"));
    let r = c
        .request(
            3,
            "tools/call",
            json!({"_meta": m(), "name": "validate_shex", "arguments": {"schema": "x", "shapeMap": "x"}}),
        )
        .await;
    assert_eq!(r["error"]["code"], -32602);
    assert_eq!(r["error"]["message"], "Unknown tool: validate_shex");
}
