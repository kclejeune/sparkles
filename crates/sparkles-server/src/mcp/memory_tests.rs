//! The agent-memory read tools of C17 Phase 1a (`check_query`, `similar_queries`,
//! `link_entities` and `recall`) over the same JSON-RPC client as the other tools, with
//! the acceptance examples of C17 §14 that Phase 1a covers.

use super::*;

/// The `mem` dataset of C17 §14 before the stand-up notes of 2026-10-08 (commit 1).
const MEM: &str = r#"@prefix ex:     <http://example.org/> .
@prefix schema: <http://schema.org/> .
@prefix org:    <http://www.w3.org/ns/org#> .
@prefix foaf:   <http://xmlns.com/foaf/0.1/> .
@prefix rdf:    <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs:   <http://www.w3.org/2000/01/rdf-schema#> .
@prefix prov:   <http://www.w3.org/ns/prov#> .
@prefix xsd:    <http://www.w3.org/2001/XMLSchema#> .
@prefix spk:    <urn:x-sparkles:> .
<https://example.org/hr> {
  ex:ana a schema:Person ; rdfs:label "Ana Lima"@en ; schema:email "ana@example.org" .
  ex:ana2 a schema:Person ; rdfs:label "Ana Souza"@en .
  ex:platform a org:OrganizationalUnit ; rdfs:label "Platform team"@en ; org:unitOf ex:acme .
  ex:acme a org:Organization ; schema:name "Acme Corp" .
}
<https://example.org/notes/2026-10-01> {
  ex:ana org:memberOf ex:platform .
  <urn:uuid:r1> rdf:reifies <<( ex:ana org:memberOf ex:platform )>> ;
      prov:wasGeneratedBy <urn:uuid:a0> ;
      prov:generatedAtTime "2026-10-01T10:02:11Z"^^xsd:dateTime .
}
"#;

/// The state after A7 (commit 2): `r1` superseded by `r2`, which reifies Ana's
/// membership of the new payments team.
const A7: &str = r#"PREFIX ex:     <http://example.org/>
PREFIX org:    <http://www.w3.org/ns/org#>
PREFIX rdf:    <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs:   <http://www.w3.org/2000/01/rdf-schema#>
PREFIX prov:   <http://www.w3.org/ns/prov#>
PREFIX xsd:    <http://www.w3.org/2001/XMLSchema#>
PREFIX spk:    <urn:x-sparkles:>
DELETE DATA { GRAPH <https://example.org/notes/2026-10-01> { ex:ana org:memberOf ex:platform } } ;
INSERT DATA {
  GRAPH <https://example.org/notes/2026-10-01> {
    <urn:uuid:r1> prov:wasInvalidatedBy <urn:uuid:a1> ;
        prov:invalidatedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime .
  }
  GRAPH <https://example.org/notes/2026-10-08> {
    ex:ana org:memberOf <urn:uuid:pay> .
    <urn:uuid:r2> rdf:reifies <<( ex:ana org:memberOf <urn:uuid:pay> )>> ;
        prov:wasGeneratedBy <urn:uuid:a1> ;
        prov:wasDerivedFrom <https://example.org/notes/2026-10-08> ;
        prov:generatedAtTime "2026-10-08T09:14:03Z"^^xsd:dateTime ;
        prov:wasRevisionOf <urn:uuid:r1> ;
        spk:confidence 0.9 ;
        spk:quote "Ana moved to the payments team this week." .
    <urn:uuid:a1> a prov:Activity ;
        prov:wasAssociatedWith <urn:x-sparkles:principal:agent-7> .
    <urn:uuid:pay> a org:OrganizationalUnit ; rdfs:label "Payments team"@en ;
        org:unitOf ex:acme .
  }
}"#;

fn load_trig(ds: &crate::state::Dataset, trig: &str) {
    ds.store
        .load(&[Source::from_bytes(
            trig.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
}

/// The `mem` dataset at commit 2, with a full-text index when `text`.
fn mem(text: bool) -> McpServer {
    let server = server_with(&[], McpConfig::default());
    let ds = server.state.attach("mem", DbType::Mem, None).unwrap();
    load_trig(&ds, MEM);
    // commit 1 stays readable
    ds.store
        .create_snapshot_opts(
            "before",
            &sparkles::history::At::Commit(1),
            &Default::default(),
        )
        .unwrap();
    sparkles::sparql::update::update(&ds.store, A7, &sparkles::sparql::QueryOptions::default())
        .unwrap();
    assert_eq!(head(&server, "mem"), 2);
    #[cfg(feature = "text")]
    if text {
        ds.store
            .enable_text(sparkles::text::TextConfig::default())
            .unwrap();
    }
    #[cfg(not(feature = "text"))]
    let _ = text;
    server
}

fn issues(s: &Value) -> Vec<(String, String)> {
    s["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["code"].as_str().unwrap().to_string(),
                i["severity"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// A1–A3, the parse issues and the other checks of `check_query`.
#[tokio::test(flavor = "multi_thread")]
async fn check_query() {
    let mut c = Client::start(mem(false));
    // A1: a predicate of another vocabulary
    let s = c
        .structured(
            "check_query",
            json!({"query": "SELECT ?n WHERE { ?p a schema:Person ; foaf:name ?n }"}),
        )
        .await;
    assert_eq!(s["ok"], false, "{s}");
    assert_eq!(s["commit"], 2);
    assert_eq!(
        issues(&s),
        [("unknown-predicate".into(), "error".into())],
        "{s}"
    );
    let i = &s["issues"][0];
    assert_eq!(i["term"], "foaf:name");
    assert_eq!(i["suggestions"][0]["term"], "schema:name", "{s}");
    assert_eq!(i["suggestions"][0]["why"], "same-local-name");
    assert_eq!(i["line"], 1, "{s}");
    // A2: a simple literal against tagged labels
    let s = c
        .structured(
            "check_query",
            json!({"query": "SELECT ?p WHERE { ?p rdfs:label \"Ana Lima\" }"}),
        )
        .await;
    assert_eq!(s["ok"], true, "{s}");
    assert_eq!(
        issues(&s),
        [("language-tag".into(), "warning".into())],
        "{s}"
    );
    assert_eq!(
        s["issues"][0]["suggestions"][0]["term"], "\"Ana Lima\"@en",
        "{s}"
    );
    // A3: a misspelt class, and the plan's estimate on request
    let q = "SELECT ?u WHERE { ?u a org:Organisation } LIMIT 10";
    let s = c.structured("check_query", json!({"query": q})).await;
    assert_eq!(
        issues(&s),
        [("unknown-class".into(), "error".into())],
        "{s}"
    );
    let sug = &s["issues"][0]["suggestions"][0];
    assert_eq!(sug["term"], "org:Organization", "{s}");
    assert_eq!(sug["why"], "edit-distance");
    assert!(s.get("estimatedRows").is_none());
    let s = c
        .structured("check_query", json!({"query": q, "explain": true}))
        .await;
    assert!(s["estimatedRows"].is_number(), "{s}");
    // a query that checks clean
    let s = c
        .structured(
            "check_query",
            json!({"query": "SELECT ?n WHERE { ?p a schema:Person ; rdfs:label ?n } LIMIT 5"}),
        )
        .await;
    assert_eq!(s["ok"], true);
    assert_eq!(s["issues"], json!([]), "{s}");
    // syntax, with the line and column
    let s = c
        .structured(
            "check_query",
            json!({"query": "SELECT ?x\nWHERE { ?x ?p }"}),
        )
        .await;
    assert_eq!(issues(&s), [("syntax".into(), "error".into())], "{s}");
    assert_eq!(s["issues"][0]["line"], 2, "{s}");
    // an undefined prefix suggests the known ones that resemble it
    let s = c
        .structured(
            "check_query",
            json!({"query": "SELECT ?n WHERE { ?p schem:name ?n }"}),
        )
        .await;
    assert_eq!(s["issues"][0]["code"], "syntax", "{s}");
    let sugs = s["issues"][0]["suggestions"].to_string();
    assert!(sugs.contains("schema"), "{s}");
    // an update is not a query (C18 A3)
    let s = c
        .structured(
            "check_query",
            json!({"query": "INSERT DATA { ex:a ex:b ex:c }"}),
        )
        .await;
    assert_eq!(issues(&s), [("not-a-query".into(), "error".into())], "{s}");
    // an unknown term, a projection the pattern never binds, a datatype mismatch
    let s = c
        .structured(
            "check_query",
            json!({"query": "SELECT ?p ?nope WHERE { ?p org:memberOf ex:nowhere ; schema:email 42 }"}),
        )
        .await;
    let got = issues(&s);
    for want in ["unknown-term", "unbound-projection", "datatype-mismatch"] {
        assert!(
            got.contains(&(want.into(), "warning".into())),
            "{want}: {s}"
        );
    }
    assert_eq!(s["ok"], true);
    // a class whose instances never have the predicate
    let s = c
        .structured(
            "check_query",
            json!({"query": "SELECT ?e WHERE { ?o a org:Organization ; schema:email ?e }"}),
        )
        .await;
    assert!(
        issues(&s).contains(&("class-mismatch".into(), "warning".into())),
        "{s}"
    );
    // maxSuggestions bounds the suggestions
    let s = c
        .structured(
            "check_query",
            json!({"query": "SELECT ?n WHERE { ?p foaf:name ?n }", "maxSuggestions": 0}),
        )
        .await;
    assert!(s["issues"][0].get("suggestions").is_none(), "{s}");
    let (_, e) = c
        .error(
            "check_query",
            json!({"query": "ASK {}", "maxSuggestions": 11}),
        )
        .await;
    assert_eq!(e["code"], "bad-argument");
}

/// The `mem` dataset of [`mem`] on a server whose tools read schema reports from the
/// index statistics at any size (`--schema-statistics-above 0`), or never.
fn mem_with_statistics(on: bool) -> McpServer {
    let cfg = McpConfig::default();
    let mut st = AppState::standalone(StoreOptions::default(), cfg.max_timeout);
    st.read_only = true;
    st.limits.query_memory_bytes = cfg.query_memory_bytes;
    st.limits.max_result_bytes = None;
    st.schema_statistics_above = on.then_some(0);
    let st = Arc::new(st);
    let ds = st.attach("mem", DbType::Mem, None).unwrap();
    load_trig(&ds, MEM);
    sparkles::sparql::update::update(&ds.store, A7, &sparkles::sparql::QueryOptions::default())
        .unwrap();
    McpServer::new(st, cfg)
}

/// A report measured from the index statistics gives `check_query` the answers a full
/// report gives, and a commit replaces it.
#[tokio::test(flavor = "multi_thread")]
async fn check_query_from_index_statistics() {
    let queries = [
        "SELECT ?n WHERE { ?p a schema:Person ; foaf:name ?n }",
        "SELECT ?p WHERE { ?p rdfs:label \"Ana Lima\" }",
        "SELECT ?p WHERE { ?p rdfs:label \"Ana Lima\"@en }",
        "SELECT ?u WHERE { ?u a org:Organisation } LIMIT 10",
        "SELECT ?n WHERE { ?p a schema:Person ; rdfs:label ?n } LIMIT 5",
        "SELECT ?p ?nope WHERE { ?p org:memberOf ex:nowhere ; schema:email 42 }",
        "SELECT ?e WHERE { ?o a org:Organization ; schema:email ?e }",
        "SELECT ?x WHERE { ?x spk:confidence ?c FILTER(?c > \"high\") }",
    ];
    let mut exact = Client::start(mem_with_statistics(false));
    let server = mem_with_statistics(true);
    let state = server.state.clone();
    let mut stats = Client::start(server);
    for q in queries {
        let args = json!({"query": q, "terms": true});
        let a = exact.structured("check_query", args.clone()).await;
        let b = stats.structured("check_query", args).await;
        assert_eq!(a["ok"], b["ok"], "{q}");
        assert_eq!(a["issues"], b["issues"], "{q}");
        // the counts of the terms list are quads, which equal triples here
        assert_eq!(a["terms"], b["terms"], "{q}");
    }
    let d = stats
        .structured(
            "describe_schema",
            json!({"section": "summary", "graph": "union"}),
        )
        .await;
    assert_eq!(d["basis"], "statistics", "{d}");
    let label = d["predicates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["iri"] == "rdfs:label")
        .unwrap()
        .clone();
    assert_eq!(label["objects"], json!(["rdf:langString@en 4"]), "{label}");
    assert!(label.get("maxPerSubject").is_none(), "{label}");
    // the output schema of describe_schema admits the statistics report: no member it
    // requires of a predicate is missing, and it names the ones this report adds
    let r = stats.request(90, "tools/list", json!({"_meta": m()})).await;
    let tool = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "describe_schema")
        .unwrap()
        .clone();
    let out = &tool["outputSchema"];
    let item = &out["properties"]["predicates"]["items"];
    for p in d["predicates"].as_array().unwrap() {
        for k in item["required"].as_array().unwrap() {
            assert!(p.get(k.as_str().unwrap()).is_some(), "{k} missing in {p}");
        }
    }
    assert!(out["properties"].get("basis").is_some(), "{out}");
    assert!(item["properties"].get("objectsSampled").is_some(), "{item}");
    let d = exact
        .structured(
            "describe_schema",
            json!({"section": "summary", "graph": "union"}),
        )
        .await;
    assert!(d.get("basis").is_none(), "{d}");
    // a commit adds a predicate: the next check reads a report of the new state
    let q = json!({"query": "SELECT ?x WHERE { ?x ex:newPredicate ?y }"});
    let s = stats.structured("check_query", q.clone()).await;
    assert_eq!(
        issues(&s),
        [("unknown-predicate".into(), "error".into())],
        "{s}"
    );
    let ds = state.get("mem").unwrap();
    sparkles::sparql::update::update(
        &ds.store,
        "INSERT DATA { <http://example.org/z> <http://example.org/newPredicate> 1 }",
        &sparkles::sparql::QueryOptions::default(),
    )
    .unwrap();
    let s = stats.structured("check_query", q).await;
    assert_eq!(s["ok"], true, "{s}");
    assert_eq!(s["issues"], json!([]), "{s}");
}

fn put_query(server: &McpServer, ds: &str, name: &str, def: Value) {
    let ds = server.state.datasets()[ds].clone();
    let d: sparkles::stored::Definition = serde_json::from_value(def).unwrap();
    ds.queries
        .put(name, d, sparkles::stored::Change::default())
        .unwrap();
}

/// The stored queries of A4.
fn with_queries(server: &McpServer) {
    put_query(
        server,
        "mem",
        "team_members",
        json!({
            "query": "PREFIX org: <http://www.w3.org/ns/org#>\nSELECT ?m WHERE { ?m org:memberOf ?team } LIMIT 100",
            "description": "Members of a team",
            "parameters": {"team": {"type": "iri", "description": "The team"}},
            "questions": ["Who is on the payments team?"]
        }),
    );
    put_query(
        server,
        "mem",
        "org_units",
        json!({
            "query": "PREFIX org: <http://www.w3.org/ns/org#>\nSELECT ?u WHERE { ?u org:unitOf ?org } LIMIT 100",
            "description": "Units of an organization"
        }),
    );
    put_query(
        server,
        "mem",
        "payments_secret",
        json!({
            "query": "ASK { ?s ?p ?o }",
            "description": "Who works in payments, hidden from MCP",
            "mcp": false
        }),
    );
}

/// A4: example questions rank a stored query first; `mcp: false` hides one.
#[tokio::test(flavor = "multi_thread")]
async fn similar_queries() {
    let server = mem(false);
    with_queries(&server);
    let mut c = Client::start(server);
    let s = c
        .structured(
            "similar_queries",
            json!({"question": "who works in payments"}),
        )
        .await;
    assert_eq!(s["ranking"], "text", "{s}");
    let q = &s["queries"][0];
    assert_eq!(q["name"], "team_members", "{s}");
    assert_eq!(q["tool"], "mem__team_members");
    assert_eq!(q["matchedBy"], json!(["text"]));
    assert_eq!(q["questions"], json!(["Who is on the payments team?"]));
    assert_eq!(
        q["parameters"],
        json!([{"name": "team", "type": "iri", "required": true, "description": "The team"}])
    );
    assert!(q["query"].as_str().unwrap().contains("org:memberOf"));
    assert!(!s.to_string().contains("payments_secret"), "{s}");
    // without the text, and with k
    let s = c
        .structured(
            "similar_queries",
            json!({"question": "units of the organization", "withText": false, "k": 1}),
        )
        .await;
    assert_eq!(s["queries"].as_array().unwrap().len(), 1, "{s}");
    assert_eq!(s["queries"][0]["name"], "org_units");
    assert!(s["queries"][0].get("query").is_none());
    // nothing matches
    let s = c
        .structured("similar_queries", json!({"question": "zebra"}))
        .await;
    assert_eq!(s["queries"], json!([]));
    let (_, e) = c
        .error("similar_queries", json!({"question": "x".repeat(2001)}))
        .await;
    assert_eq!(e["code"], "bad-argument");
}

/// The `questions` of a stored query are checked like the rest of its definition.
#[test]
fn questions_are_checked() {
    let def = |q: Value| -> sparkles::stored::Definition {
        serde_json::from_value(json!({"query": "ASK {}", "questions": q})).unwrap()
    };
    assert!(def(json!(["Who?"])).check().is_ok());
    assert!(def(json!([""])).check().is_err());
    assert!(def(json!(["x".repeat(501)])).check().is_err());
    let many: Vec<String> = (0..21).map(|i| format!("q{i}")).collect();
    assert!(def(json!(many)).check().is_err());
    // an empty list is not serialized
    let d = def(json!([]));
    assert!(!serde_json::to_string(&d).unwrap().contains("questions"));
}

fn verdict(s: &Value, i: usize) -> &str {
    s["mentions"][i]["verdict"].as_str().unwrap()
}

fn candidates(s: &Value, i: usize) -> Vec<String> {
    s["mentions"][i]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["iri"].as_str().unwrap().to_string())
        .collect()
}

/// A5 and A18: exact, ambiguous and missing mentions, with and without a text index.
#[tokio::test(flavor = "multi_thread")]
async fn link_entities() {
    // a build without `text` has no full-text index to turn on
    let with_text: &[bool] = if cfg!(feature = "text") {
        &[true, false]
    } else {
        &[false]
    };
    for &text in with_text {
        let mut c = Client::start(mem(text));
        let s = c
            .structured(
                "link_entities",
                json!({"atCommit": 2, "mentions": [
                    {"text": "Ana Lima", "types": ["schema:Person"]},
                    {"text": "Ana"},
                    {"text": "Platform  TEAM"},
                    {"text": "Zebra"}
                ]}),
            )
            .await;
        assert_eq!(s["search"], json!({"text": text, "vector": false}), "{s}");
        assert_eq!(verdict(&s, 0), "exact", "{s}");
        assert_eq!(candidates(&s, 0)[0], "ex:ana");
        let ana = &s["mentions"][0]["candidates"][0];
        assert_eq!(ana["label"], "Ana Lima");
        assert_eq!(ana["types"], json!(["schema:Person"]));
        assert_eq!(ana["typeMatch"], true);
        assert!(
            ana["matchedBy"]
                .as_array()
                .unwrap()
                .contains(&json!("exact"))
        );
        assert!(!ana["triples"].as_array().unwrap().is_empty(), "{ana}");
        if text {
            assert_eq!(verdict(&s, 1), "ambiguous", "{s}");
            let mut got = candidates(&s, 1);
            got.sort();
            assert_eq!(got, ["ex:ana", "ex:ana2"], "{s}");
            for cand in s["mentions"][1]["candidates"].as_array().unwrap() {
                assert!(!cand["triples"].as_array().unwrap().is_empty(), "{cand}");
            }
            // the normalized form matches through the phrase query
            assert_eq!(verdict(&s, 2), "exact", "{s}");
            assert_eq!(candidates(&s, 2)[0], "ex:platform");
        } else {
            assert_eq!(verdict(&s, 1), "none", "{s}");
        }
        assert_eq!(verdict(&s, 3), "none", "{s}");
        assert_eq!(s["mentions"][3]["candidates"], json!([]));
    }
    // A5's "Payments team" before the notes of 2026-10-08: never exact
    let mut c = Client::start(mem(true));
    let s = c
        .structured(
            "link_entities",
            json!({"atCommit": 1, "mentions": [{"text": "Payments team"}]}),
        )
        .await;
    assert_ne!(verdict(&s, 0), "exact", "{s}");
    // a type that does not match comes after the others
    let s = c
        .structured(
            "link_entities",
            json!({"mentions": [{"text": "Ana Lima", "types": ["org:Organization"]}]}),
        )
        .await;
    assert_eq!(verdict(&s, 0), "candidates", "{s}");
    assert_eq!(s["mentions"][0]["candidates"][0]["typeMatch"], false);
    // graphs limit the search
    let s = c
        .structured(
            "link_entities",
            json!({"graphs": ["https://example.org/notes/2026-10-08"], "mentions": [{"text": "Ana Lima"}]}),
        )
        .await;
    assert_eq!(verdict(&s, 0), "none", "{s}");
    for bad in [
        json!({"mentions": []}),
        json!({"mentions": [{"text": "x".repeat(201)}]}),
        json!({"mentions": [{"text": "a"}], "k": 21}),
        json!({"mentions": [{"text": "a", "types": ["a:b", "a:c", "a:d", "a:e", "a:f", "a:g"]}]}),
    ] {
        let (_, e) = c.error("link_entities", bad.clone()).await;
        assert_eq!(e["code"], "bad-argument", "{bad}");
    }
}

/// A11, A12, A15 and A17: facts with citations, superseded facts, the caps and
/// escaping. A query finds its seeds through the full-text index.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn recall() {
    let mut c = Client::start(mem(true));
    // A11
    let t = c.text("recall", json!({"query": "payments team"})).await;
    let first = t.lines().next().unwrap();
    assert!(first.starts_with("# dataset=mem commit=2 seeds="), "{t}");
    assert!(first.ends_with("truncated=false"), "{t}");
    assert!(
        t.lines()
            .any(|l| l
                .starts_with("## <urn:uuid:pay> \"Payments team\" (org:OrganizationalUnit) seed=")),
        "{t}"
    );
    let fact = t
        .lines()
        .find(|l| l.starts_with("ex:ana org:memberOf <urn:uuid:pay> ["))
        .unwrap_or_else(|| panic!("{t}"));
    let n = fact.rsplit_once('[').unwrap().1.trim_end_matches(']');
    let citation = t
        .lines()
        .find(|l| l.starts_with(&format!("[{n}] ")))
        .unwrap_or_else(|| panic!("{t}"));
    for part in [
        "graph=<https://example.org/notes/2026-10-08>",
        "reifier=<urn:uuid:r2>",
        "source=<https://example.org/notes/2026-10-08>",
        "at=2026-10-08T09:14:03Z",
        "agent-7",
        "confidence=0.9",
        "quote=\"Ana moved to the payments team this week.\"",
    ] {
        assert!(citation.contains(part), "{part} in {citation}");
    }
    assert!(!t.contains("org:memberOf ex:platform"), "{t}");
    assert!(!t.contains("# superseded"), "{t}");
    // a fact without a reifier is cited by its graph alone
    assert!(
        t.lines()
            .any(|l| l.starts_with('[') && l.ends_with("] graph=<https://example.org/hr>")),
        "{t}"
    );
    let prefixes = t.lines().last().unwrap();
    assert!(prefixes.starts_with("# prefixes "), "{t}");
    assert!(prefixes.contains("org: <http://www.w3.org/ns/org#>"), "{t}");
    // A11 with the superseded facts
    let t = c
        .text(
            "recall",
            json!({"query": "payments team", "includeSuperseded": true}),
        )
        .await;
    let sup = t
        .lines()
        .skip_while(|l| *l != "# superseded")
        .nth(1)
        .unwrap_or_else(|| panic!("{t}"));
    assert!(
        sup.starts_with("ex:ana org:memberOf ex:platform graph=<https://example.org/notes/2026-10-01> reifier=<urn:uuid:r1>"),
        "{sup}"
    );
    assert!(sup.contains("invalidated=2026-10-08T09:14:03Z"), "{sup}");
    // A12: the earlier state, from a seed
    let t = c
        .text("recall", json!({"seeds": ["ex:ana"], "at": 1, "hops": 0}))
        .await;
    assert!(t.starts_with("# dataset=mem commit=1 seeds=1 "), "{t}");
    assert!(
        t.lines()
            .any(|l| l.starts_with("ex:ana org:memberOf ex:platform [")),
        "{t}"
    );
    assert!(
        t.contains("## ex:ana \"Ana Lima\" (schema:Person) seed=1"),
        "{t}"
    );
    // A15: maxTriples cuts the facts and says so
    let t = c
        .text("recall", json!({"query": "payments team", "maxTriples": 5}))
        .await;
    assert!(
        t.lines().next().unwrap().contains("facts=5 truncated=true"),
        "{t}"
    );
    let facts = t
        .lines()
        .take_while(|l| *l != "# citations")
        .filter(|l| !l.starts_with('#'))
        .count();
    assert_eq!(facts, 5, "{t}");
    // maxBytes too
    let t = c
        .text(
            "recall",
            json!({"query": "payments team", "maxBytes": 1024, "hops": 2}),
        )
        .await;
    assert!(t.len() <= 1024, "{}", t.len());
    assert!(t.lines().next().unwrap().ends_with("truncated=true"), "{t}");
    // the JSON format has the same content
    let j: Value = serde_json::from_str(
        &c.text(
            "recall",
            json!({"query": "payments team", "format": "json"}),
        )
        .await,
    )
    .unwrap();
    assert_eq!(j["commit"], 2);
    assert_eq!(j["truncated"], false);
    let pay = j["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["iri"] == "<urn:uuid:pay>")
        .unwrap_or_else(|| panic!("{j}"));
    assert_eq!(pay["label"], "Payments team");
    assert_eq!(pay["types"], json!(["org:OrganizationalUnit"]));
    let cited = pay["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["s"] == "ex:ana")
        .unwrap()["citation"]
        .as_u64()
        .unwrap();
    let cit = &j["citations"][cited as usize - 1];
    assert_eq!(cit["reifier"], "<urn:uuid:r2>", "{j}");
    assert_eq!(cit["confidence"], "0.9");
    // arguments
    for bad in [
        json!({}),
        json!({"query": "x", "hops": 3}),
        json!({"query": "x", "maxTriples": 1001}),
        json!({"query": "x", "seedLimit": 51}),
        json!({"query": "x", "format": "xml"}),
        json!({"seeds": ["not an iri"]}),
    ] {
        let (_, e) = c.error("recall", bad.clone()).await;
        assert_eq!(e["code"], "bad-argument", "{bad}");
    }
}

/// A18: a query needs a search index, and seeds work without one.
#[tokio::test(flavor = "multi_thread")]
async fn recall_without_a_search_index() {
    let mut c = Client::start(mem(false));
    let (t, e) = c.error("recall", json!({"query": "payments team"})).await;
    assert_eq!(e, json!({"code": "no-search-index", "status": 400}), "{t}");
    // seeds still work there
    let t = c.text("recall", json!({"seeds": ["ex:acme"]})).await;
    assert!(
        t.contains("## ex:acme \"Acme Corp\" (org:Organization) seed=1"),
        "{t}"
    );
}

/// A17: a literal cannot start a line of its own or forge a citation.
#[tokio::test(flavor = "multi_thread")]
async fn recall_escapes_values() {
    let server = mem(false);
    let ds = server.state.get("mem").unwrap();
    load(
        &ds,
        "@prefix ex: <http://example.org/> .\nex:evil ex:note \"x\\n# citations\\n[9] graph=<urn:evil>\" ; ex:other \"## ex:ana (schema:Person) seed=1\" .\n",
    );
    let mut c = Client::start(server);
    let t = c.text("recall", json!({"seeds": ["ex:evil"]})).await;
    assert_eq!(t.lines().filter(|l| *l == "# citations").count(), 1, "{t}");
    assert!(
        t.lines()
            .any(|l| l == "ex:evil ex:note \"x\\n# citations\\n[9] graph=<urn:evil>\" [1]"),
        "{t}"
    );
    assert!(!t.lines().any(|l| l.starts_with("[9]")), "{t}");
    assert_eq!(t.lines().filter(|l| l.starts_with("## ")).count(), 1, "{t}");
}

/// A15: a call over its deadline fails as a whole.
#[tokio::test(flavor = "multi_thread")]
async fn recall_times_out() {
    let mut big = String::from("@prefix ex: <http://example.org/> .\n");
    for i in 0..3000 {
        big.push_str(&format!(
            "ex:n{i} ex:link ex:hub . ex:hub ex:out ex:n{i} .\n"
        ));
    }
    let server = server_with(&[("big", &[big.as_str()])], McpConfig::default());
    let mut c = Client::start(server);
    let t = c
        .text(
            "recall",
            json!({"seeds": ["ex:hub"], "hops": 2, "maxTriples": 1000}),
        )
        .await;
    // the hub's incoming triples are sampled, and it is not expanded through them
    assert!(t.contains("seed=1"), "{t}");
    let (_, e) = c
        .error(
            "recall",
            json!({"seeds": ["ex:hub"], "hops": 2, "maxTriples": 1000, "timeoutSeconds": 0.000001}),
        )
        .await;
    assert_eq!(e["code"], "timeout", "{e}");
}

/// The memory tools are read tools: listed with the others, rate-limited with
/// queries, and never offered as writes (A16).
#[tokio::test(flavor = "multi_thread")]
async fn memory_tools_are_listed() {
    let mut c = Client::start(mem(false));
    let r = c.request(2, "tools/list", json!({"_meta": m()})).await;
    let names: Vec<&str> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in ["check_query", "similar_queries", "link_entities", "recall"] {
        assert!(names.contains(&t), "{t}");
    }
    assert!(!names.contains(&"sparql_update"));
    let p = c
        .request(
            3,
            "prompts/get",
            json!({"_meta": m(), "name": "answer_question", "arguments": {"dataset": "mem", "question": "Who is Ana?"}}),
        )
        .await;
    let text = p["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    for t in ["recall", "similar_queries", "check_query"] {
        assert!(text.contains(t), "{t}: {text}");
    }
}

/// With an embedding index: hybrid ranking, vector candidates, and the fallback to
/// text when the endpoint fails (C17 §8).
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn memory_tools_with_embeddings() {
    use sparkles::vector::embed::Environment;
    use sparkles::vector::embed::mock::MockProvider;
    let mock = MockProvider::start(8);
    let server = mem(true);
    with_queries(&server);
    let ds = server.state.get("mem").unwrap();
    // the provider's client blocks
    tokio::task::block_in_place(|| {
        ds.dataset
            .indexes()
            .vector()
            .set_embedding_environment(Some(Environment {
                outbound: sparkles::outbound::OutboundPolicy {
                    allow_private: true,
                    ..Default::default()
                },
                ..Default::default()
            }));
        let cfg: sparkles::vector::VectorIndexConfig = serde_json::from_value(json!({
            "predicate": "urn:x-sparkles:test:emb",
            "dimension": 8,
            "embedding": {
                "url": mock.url(),
                "model": "mock",
                "predicates": ["http://www.w3.org/2000/01/rdf-schema#label"]
            }
        }))
        .unwrap();
        ds.dataset.indexes().vector().put("labels", cfg).unwrap();
        ds.dataset
            .indexes()
            .vector()
            .embed_until_idle(Duration::from_secs(30))
            .unwrap();
    });
    let mut c = Client::start(server);
    let s = c
        .structured(
            "similar_queries",
            json!({"question": "who works in payments"}),
        )
        .await;
    assert_eq!(s["ranking"], "hybrid", "{s}");
    assert_eq!(s["queries"][0]["name"], "team_members", "{s}");
    let s = c
        .structured(
            "link_entities",
            json!({"mentions": [{"text": "Ana Souza"}]}),
        )
        .await;
    assert_eq!(s["search"], json!({"text": true, "vector": true}), "{s}");
    assert_eq!(verdict(&s, 0), "exact", "{s}");
    let m = s["mentions"][0]["candidates"][0]["matchedBy"].to_string();
    assert!(m.contains("vector") && m.contains("exact"), "{s}");
    let t = c.text("recall", json!({"query": "Ana Souza"})).await;
    assert!(t.contains("## ex:ana2 \"Ana Souza\""), "{t}");
    // the endpoint fails: text alone
    mock.state().fail = Some(500);
    let s = c
        .structured(
            "similar_queries",
            json!({"question": "who works in the payments group"}),
        )
        .await;
    assert_eq!(s["ranking"], "text", "{s}");
    assert_eq!(s["queries"][0]["name"], "team_members", "{s}");
    let s = c
        .structured(
            "link_entities",
            json!({"mentions": [{"text": "Ana Lima", "context": "a new phrase"}]}),
        )
        .await;
    assert_eq!(s["search"], json!({"text": true, "vector": false}), "{s}");
    assert_eq!(verdict(&s, 0), "exact", "{s}");
    let t = c.text("recall", json!({"query": "payments team"})).await;
    assert!(t.contains("## <urn:uuid:pay>"), "{t}");
}

/// Two graphs disagree on a predicate the guard limits to one value: `recall` marks
/// both facts and picks no winner (C17 §5.5).
#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn recall_marks_conflicts() {
    let server = mem(false);
    let ds = server.state.get("mem").unwrap();
    load_trig(
        &ds,
        "<https://example.org/hr> { <http://example.org/ana> <http://www.w3.org/ns/org#memberOf> <http://example.org/platform> . }",
    );
    let cfg: sparkles_shacl::guard::ValidationConfig = serde_json::from_value(json!({
        "format": 2,
        "language": "shacl",
        "mode": "warn",
        "dataGraph": "union",
        "shapes": {"inline": "@prefix sh: <http://www.w3.org/ns/shacl#> .
            @prefix schema: <http://schema.org/> . @prefix org: <http://www.w3.org/ns/org#> .
            <urn:s:Person> a sh:NodeShape ; sh:targetClass schema:Person ;
              sh:property [ sh:path org:memberOf ; sh:maxCount 1 ] ."}
    }))
    .unwrap();
    tokio::task::block_in_place(|| ds.dataset.validation().guard().set_shacl(cfg).unwrap());
    let mut c = Client::start(server);
    let t = c
        .text("recall", json!({"seeds": ["ex:ana"], "hops": 0}))
        .await;
    let marked: Vec<&str> = t.lines().filter(|l| l.ends_with(" conflict")).collect();
    assert_eq!(marked.len(), 2, "{t}");
    assert!(
        marked.iter().all(|l| l.starts_with("ex:ana org:memberOf ")),
        "{t}"
    );
    let j: Value = serde_json::from_str(
        &c.text(
            "recall",
            json!({"seeds": ["ex:ana"], "hops": 0, "format": "json"}),
        )
        .await,
    )
    .unwrap();
    assert_eq!(j["conflicts"][0]["p"], "org:memberOf", "{j}");
    assert_eq!(j["conflicts"][0]["values"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn link_evidence_exposes_retained_details_beyond_the_display() {
    let ttl = r#"@prefix ex: <http://example.org/> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix skos: <http://www.w3.org/2004/02/skos/core#> .
        ex:item a ex:A, ex:B, ex:C, ex:WikimediaDisambiguation ;
          rdfs:label "Berlin"@en ; skos:altLabel "Alias1", "Alias2", "Alias3", "Alias4" ."#;
    let mut c = Client::start(server_with(&[("t", &[ttl])], McpConfig::default()));
    let plain = c
        .structured("link_entities", json!({"mentions":[{"text":"Berlin"}]}))
        .await;
    assert!(plain["mentions"][0].get("evidence").is_none());
    assert!(
        plain["mentions"][0]["candidates"][0]
            .get("evidence")
            .is_none()
    );
    let result = c
        .structured(
            "link_entities",
            json!({"mentions":[{"text":"Berlin"}],"evidence":true}),
        )
        .await;
    let mention = &result["mentions"][0];
    assert_eq!(mention["verdict"], plain["mentions"][0]["verdict"]);
    assert_eq!(mention["evidence"]["version"], 1);
    assert_eq!(
        mention["evidence"]["exactStringLabelsComplete"], true,
        "{result}"
    );
    assert_eq!(mention["evidence"]["candidateUniverseComplete"], false);
    assert_eq!(mention["evidence"]["normalizedLabelsComplete"], false);
    let candidate = &mention["candidates"][0];
    assert_eq!(candidate["types"].as_array().unwrap().len(), 3);
    assert_eq!(candidate["altLabels"].as_array().unwrap().len(), 3);
    let evidence = &candidate["evidence"];
    assert_eq!(evidence["version"], 1);
    assert_eq!(evidence["types"].as_array().unwrap().len(), 4);
    assert!(
        evidence["types"]
            .to_string()
            .contains("WikimediaDisambiguation")
    );
    assert_eq!(evidence["typesComplete"], true);
    assert_eq!(evidence["labelsComplete"], true);
    assert_eq!(evidence["labels"].as_array().unwrap().len(), 5);
    for alias in ["Alias1", "Alias2", "Alias3", "Alias4"] {
        assert!(evidence["labels"].to_string().contains(alias));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn link_evidence_retains_full_terms_beyond_presentation_character_limits() {
    let entity_iri = format!("urn:entity:{}", "z".repeat(400));
    let type_iri = format!("urn:type:{}", "x".repeat(400));
    let label = format!("{}\"\nend", "a".repeat(400));
    let ttl = format!(
        "<{entity_iri}> a <{type_iri}> ; <http://www.w3.org/2000/01/rdf-schema#label> \"Berlin\"@en ; <http://www.w3.org/2004/02/skos/core#altLabel> {} .",
        oxrdf::Literal::new_simple_literal(&label)
    );
    let mut c = Client::start(server_with(&[("t", &[&ttl])], McpConfig::default()));
    let result = c
        .structured(
            "link_entities",
            json!({"mentions":[{"text":"Berlin"}],"evidence":true}),
        )
        .await;
    let candidate = &result["mentions"][0]["candidates"][0];
    assert!(candidate["types"][0].as_str().unwrap().contains("…"));
    assert!(candidate["iri"].as_str().unwrap().contains("…"));
    assert_eq!(candidate["evidence"]["iri"], entity_iri);
    assert_eq!(candidate["evidence"]["types"][0], type_iri);
    assert_eq!(candidate["evidence"]["typesComplete"], true);
    assert_eq!(candidate["evidence"]["labelsComplete"], true);
    assert!(
        candidate["evidence"]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["term"] == oxrdf::Literal::new_simple_literal(&label).to_string())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn link_evidence_distinguishes_display_and_exact_lookup_caps() {
    for (entities, k, exact_complete) in [(4, 3, true), (51, 2, false)] {
        let mut ttl = String::new();
        for n in 0..entities {
            ttl.push_str(&format!(
                "<urn:item{n:03}> <http://www.w3.org/2000/01/rdf-schema#label> \"Berlin\"@en .\n"
            ));
        }
        let mut c = Client::start(server_with(&[("t", &[&ttl])], McpConfig::default()));
        let result = c
            .structured(
                "link_entities",
                json!({"mentions":[{"text":"Berlin"}],"k":k,"evidence":true}),
            )
            .await;
        let mention = &result["mentions"][0];
        assert_eq!(mention["verdict"], "ambiguous");
        assert_eq!(mention["candidates"].as_array().unwrap().len(), k);
        assert_eq!(mention["evidence"]["observedCandidates"], entities.min(50));
        assert_eq!(mention["evidence"]["displayTruncated"], true);
        assert_eq!(
            mention["evidence"]["exactStringLabelsComplete"], exact_complete,
            "{result}"
        );
        assert_eq!(mention["evidence"]["candidateUniverseComplete"], false);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn link_evidence_marks_per_subject_and_global_detail_caps() {
    for (type_count, label_count, global_cap) in [(25, 25, false), (41, 100, true)] {
        let mut ttl = String::from(
            "<urn:a> <http://www.w3.org/2000/01/rdf-schema#label> \"Berlin\"@en .\n<urn:b> <http://www.w3.org/2000/01/rdf-schema#label> \"Berlin\"@en ; a <urn:TypeB> .\n",
        );
        for n in 0..type_count {
            ttl.push_str(&format!("<urn:a> a <urn:Type{n:03}> .\n"));
        }
        for n in 0..label_count {
            ttl.push_str(&format!(
                "<urn:a> <http://www.w3.org/2000/01/rdf-schema#label> \"Alias{n:03}\" .\n"
            ));
        }
        let mut c = Client::start(server_with(&[("t", &[&ttl])], McpConfig::default()));
        let result = c
            .structured(
                "link_entities",
                json!({"mentions":[{"text":"Berlin"}],"evidence":true}),
            )
            .await;
        let candidates = result["mentions"][0]["candidates"].as_array().unwrap();
        let a = candidates
            .iter()
            .find(|c| c["iri"].as_str().unwrap().contains("urn:a"))
            .unwrap();
        let b = candidates
            .iter()
            .find(|c| c["iri"].as_str().unwrap().contains("urn:b"))
            .unwrap();
        assert_eq!(a["evidence"]["types"].as_array().unwrap().len(), 20);
        assert_eq!(a["evidence"]["typesComplete"], false);
        assert_eq!(a["evidence"]["labels"].as_array().unwrap().len(), 20);
        assert_eq!(a["evidence"]["labelsComplete"], false);
        assert_eq!(b["evidence"]["typesComplete"], !global_cap, "{result}");
        assert_eq!(b["evidence"]["labelsComplete"], !global_cap, "{result}");
        if b["evidence"]["types"].as_array().unwrap().is_empty() {
            assert_eq!(
                b["evidence"]["typesComplete"], false,
                "an empty starved lookup is not absence"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn link_evidence_does_not_certify_unsupported_label_forms() {
    let ttl = "<urn:item> <http://www.w3.org/2000/01/rdf-schema#label> 7 .";
    let mut c = Client::start(server_with(&[("t", &[ttl])], McpConfig::default()));
    let result = c
        .structured(
            "link_entities",
            json!({"mentions":[{"text":"7"}],"evidence":true}),
        )
        .await;
    assert_eq!(
        result["mentions"][0]["evidence"]["exactStringLabelsComplete"],
        false
    );
    assert_eq!(
        result["mentions"][0]["evidence"]["candidateUniverseComplete"],
        false
    );
}

#[test]
fn link_evidence_counts_and_details_remain_in_the_query_view() {
    let server = server_with(
        &[(
            "t",
            &[
                "<urn:visible> a <urn:PublicType> ; <http://www.w3.org/2000/01/rdf-schema#label> \"Berlin\"@en .",
            ],
        )],
        McpConfig::default(),
    );
    server.state.datasets()["t"].store.load(&[Source::from_bytes(
        b"<urn:hiddenEntity> a <urn:SecretType> ; <http://www.w3.org/2000/01/rdf-schema#label> \"Berlin\"@en .".to_vec(),
        RdfFormat::Turtle, Some(oxrdf::NamedNode::new("urn:hiddenGraph").unwrap()),
    )]).unwrap();
    let read = |principal| {
        let call = Call {
            arrived: Instant::now(),
            cancel: Arc::new(AtomicBool::new(false)),
            request_id: "link-evidence".into(),
            principal,
            headers: None,
            held: None,
            grant: None,
        };
        let args = json!({"dataset":"t","mentions":[{"text":"Berlin"}],"evidence":true});
        match server
            .run_now("link_entities", args.as_object().unwrap().clone(), &call)
            .unwrap()
        {
            Outcome::Structured(value) => value,
            Outcome::Text(_) => panic!("expected structured link result"),
        }
    };
    let full = read(Principal::local());
    assert_eq!(full["mentions"][0]["evidence"]["observedCandidates"], 2);
    let restricted = read(Principal::local().hiding_graphs(&["urn:hiddenGraph".into()]));
    assert_eq!(
        restricted["mentions"][0]["evidence"]["observedCandidates"],
        1
    );
    assert!(
        !restricted.to_string().contains("hiddenEntity"),
        "{restricted}"
    );
    assert!(
        !restricted.to_string().contains("SecretType"),
        "{restricted}"
    );
    assert_eq!(read(Principal::local()), full);
}
