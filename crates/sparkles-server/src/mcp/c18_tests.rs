//! The MCP tools and HTTP routes of C18 Phase 1: `share_query`, `why_empty`, the terms
//! list of `check_query`, and `/{ds}/check`, `/{ds}/recall` and
//! `/{ds}/sparql/diagnose`, with the acceptance examples A1 to A4.

use super::*;
use base64::Engine;

/// A small `org` graph in the namespaces of C18 §15.
const ORG: &str = r#"@prefix ex:   <http://example.org/ontology#> .
@prefix res:  <http://example.org/resource/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Person a <http://www.w3.org/2002/07/owl#Class> ; rdfs:label "Person"@en .
ex:Team rdfs:label "Team"@en .
res:payments a ex:Team ; rdfs:label "Payments team"@en .
res:platform a ex:Team ; rdfs:label "Platform team"@en .
res:ana a ex:Person ; foaf:name "Ana Lima"@en ; ex:memberOf res:payments .
res:bo a ex:Person ; foaf:name "Bo Chen"@en ; ex:memberOf res:platform .
"#;

const PREFIXES: &str = "PREFIX ex: <http://example.org/ontology#>\nPREFIX res: <http://example.org/resource/>\nPREFIX foaf: <http://xmlns.com/foaf/0.1/>\n";

fn org(cfg: McpConfig) -> McpServer {
    server_with(&[("org", &[ORG])], cfg)
}

fn decode_link(url: &str) -> Value {
    let (_, frag) = url.split_once("#ask=").expect("an ask fragment");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(frag)
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// A1, A2: the link carries the question and the query; an update is refused; the tool
/// is listed only when the server knows the UI's address.
#[tokio::test(flavor = "multi_thread")]
async fn share_query() {
    let mut c = Client::start(org(McpConfig::default()));
    let r = c.request(2, "tools/list", json!({"_meta": m()})).await;
    let names: Vec<&str> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"share_query"), "{names:?}");
    assert!(names.contains(&"why_empty"));

    let cfg = McpConfig {
        ui_url: Some("https://sparql.example.org/".into()),
        ..McpConfig::default()
    };
    let mut c = Client::start(org(cfg));
    let r = c.request(2, "tools/list", json!({"_meta": m()})).await;
    let tool = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "share_query")
        .cloned()
        .expect("share_query is listed with --ui-url");
    assert_eq!(
        tool["annotations"],
        json!({"readOnlyHint": true, "openWorldHint": false})
    );
    let query = format!("{PREFIXES}SELECT ?p WHERE {{ ?p ex:memberOf res:payments }}");
    let s = c
        .structured(
            "share_query",
            json!({"query": query, "question": "Who is on the payments team?", "explanation": "Members of the payments team.", "assumptions": ["team means ex:Team"], "atCommit": 1}),
        )
        .await;
    assert_eq!(s["ok"], true, "{s}");
    let url = s["url"].as_str().unwrap();
    assert!(
        url.starts_with("https://sparql.example.org/ui/query?ds=org#ask="),
        "{url}"
    );
    let p = decode_link(url);
    assert_eq!(p["question"], "Who is on the payments team?");
    assert_eq!(p["query"], query);
    assert_eq!(p["dataset"], "org");
    assert_eq!(p["atCommit"], 1);
    assert_eq!(p["assumptions"], json!(["team means ex:Team"]));
    // A2
    let (text, meta) = c
        .error(
            "share_query",
            json!({"query": "INSERT DATA { <urn:a> <urn:b> <urn:c> }"}),
        )
        .await;
    assert_eq!(meta["code"], "not-a-query", "{text}");
    // the limits
    let (_, meta) = c
        .error(
            "share_query",
            json!({"query": "ASK {}", "explanation": "x".repeat(401)}),
        )
        .await;
    assert_eq!(meta["code"], "bad-argument");
    let long = format!("ASK {{}} # {}", "y".repeat(40_000));
    let (_, meta) = c.error("share_query", json!({ "query": long })).await;
    assert_eq!(meta["code"], "too-large");
}

/// The `ask_graph` prompt: the steps of C18 §4.1 as rules, with the dataset's prefixes
/// and the question, and nothing from the data.
#[tokio::test(flavor = "multi_thread")]
async fn ask_graph_prompt() {
    let mut c = Client::start(org(McpConfig::default()));
    let r = c.request(2, "prompts/list", json!({"_meta": m()})).await;
    assert!(
        r["result"]["prompts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "ask_graph"),
        "{r}"
    );
    let p = c
        .request(
            3,
            "prompts/get",
            json!({"_meta": m(), "name": "ask_graph", "arguments": {"dataset": "org", "question": "Who is on the payments team?"}}),
        )
        .await;
    let text = p["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    for t in [
        "Who is on the payments team?",
        "describe_schema",
        "similar_queries",
        "link_entities",
        "check_query",
        "sparql_query",
        "why_empty",
        "at most twice",
        "share_query",
        "unreviewed",
        "PREFIX",
    ] {
        assert!(text.contains(t), "{t}: {text}");
    }
    assert!(!text.contains("Ana Lima"), "no data in prompt text");
    let p = c
        .request(
            4,
            "prompts/get",
            json!({"_meta": m(), "name": "ask_graph", "arguments": {"dataset": "org"}}),
        )
        .await;
    assert!(p.get("error").is_some(), "{p}");
}

/// A3 and the terms list.
#[tokio::test(flavor = "multi_thread")]
async fn check_terms() {
    let mut c = Client::start(org(McpConfig::default()));
    let s = c
        .structured("check_query", json!({"query": "DELETE WHERE { ?s ?p ?o }"}))
        .await;
    assert_eq!(s["ok"], false);
    assert_eq!(s["issues"][0]["code"], "not-a-query", "{s}");
    let q = format!(
        "{PREFIXES}SELECT ?p WHERE {{ ?p a ex:Person ; ex:memberOf res:payments ; ex:nope res:nobody }}"
    );
    let s = c
        .structured("check_query", json!({"query": q, "terms": true}))
        .await;
    let terms = s["terms"].as_array().unwrap();
    let by = |t: &str| {
        terms
            .iter()
            .find(|x| x["term"] == t)
            .unwrap_or_else(|| panic!("{t} in {s}"))
            .clone()
    };
    let member = by("ex:memberOf");
    assert_eq!(
        (
            member["kind"].clone(),
            member["count"].clone(),
            member["occurs"].clone()
        ),
        (json!("property"), json!(2), json!(true)),
        "{member}"
    );
    let person = by("ex:Person");
    assert_eq!(person["kind"], "class");
    assert_eq!(person["count"], 2);
    assert_eq!(person["label"], "Person");
    let pay = by("res:payments");
    assert_eq!(pay["kind"], "entity");
    assert_eq!(pay["label"], "Payments team");
    assert_eq!(pay["types"], json!(["ex:Team"]));
    assert_eq!(by("res:nobody")["occurs"], false);
    assert_eq!(by("ex:nope")["occurs"], false);
    // without the option there is no list
    let s = c.structured("check_query", json!({"query": q})).await;
    assert!(s.get("terms").is_none());
}

/// A4: the first pattern without solutions, with the language-tag warning.
#[tokio::test(flavor = "multi_thread")]
async fn why_empty() {
    let mut c = Client::start(org(McpConfig::default()));
    let q = format!(
        "{PREFIXES}SELECT ?p {{ ?p a ex:Person ; ex:memberOf res:payments ;\n  foaf:name \"Ana Lima\" }}"
    );
    let s = c.structured("why_empty", json!({"query": q})).await;
    assert_eq!(s["empty"], true, "{s}");
    let f = &s["first"];
    assert_eq!(f["kind"], "pattern", "{s}");
    assert_eq!(f["text"], "?p foaf:name \"Ana Lima\"", "{s}");
    assert_eq!(f["line"], 5, "{s}");
    let codes: Vec<&str> = f["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["language-tag"], "{s}");
    assert!(
        f["constants"]
            .as_array()
            .unwrap()
            .contains(&json!({"term": "\"Ana Lima\"", "occurs": false})),
        "{s}"
    );
    assert_eq!(s["complete"], true);
    // A37: a check issue explains the empty pattern
    assert_eq!(s["verdict"], "query", "{s}");
    // every pattern matches alone, but not together
    let q = format!(
        "{PREFIXES}SELECT ?p {{ ?p ex:memberOf res:payments . ?p foaf:name \"Bo Chen\"@en }}"
    );
    let s = c.structured("why_empty", json!({"query": q})).await;
    assert_eq!(s["first"]["kind"], "join", "{s}");
    assert_eq!(s["first"]["text"], "?p foaf:name \"Bo Chen\"@en");
    assert_eq!(s["steps"].as_array().unwrap().len(), 3, "{s}");
    // A37: the query is well formed for the data, which holds no match
    assert_eq!(s["verdict"], "data", "{s}");
    // a term that occurs nowhere and has no suggestion looks like a hidden one
    let q =
        format!("{PREFIXES}SELECT ?p {{ ?p ex:memberOf <http://example.org/resource/zzz-none> }}");
    let s = c.structured("why_empty", json!({"query": q})).await;
    assert_eq!(s["first"]["kind"], "pattern", "{s}");
    assert_eq!(s["verdict"], "data", "{s}");
    // a filter that removes everything
    let q = format!("{PREFIXES}SELECT ?n {{ ?p foaf:name ?n FILTER(STRSTARTS(?n, \"Z\")) }}");
    let s = c.structured("why_empty", json!({"query": q})).await;
    assert_eq!(s["first"]["kind"], "filter", "{s}");
    // a query with solutions
    let q = format!("{PREFIXES}SELECT ?p {{ ?p a ex:Person }}");
    let s = c.structured("why_empty", json!({"query": q})).await;
    assert_eq!(s["empty"], false, "{s}");
    assert!(s.get("first").is_none());
    // OPTIONAL never empties a result; MINUS is not cut
    let q = format!("{PREFIXES}SELECT ?p {{ ?p a ex:Person MINUS {{ ?p ex:memberOf ?t }} }}");
    let s = c.structured("why_empty", json!({"query": q})).await;
    assert_eq!(s["empty"], true);
    assert_eq!(s["complete"], false, "{s}");
    assert_eq!(s["unchecked"], json!(["MINUS"]));
    let (_, meta) = c
        .error("why_empty", json!({"query": "DELETE WHERE { ?s ?p ?o }"}))
        .await;
    assert_eq!(meta["code"], "not-a-query");
}

/// A28: an agent's session fact is unreviewed, left out with `statuses: ["reviewed"]`,
/// and its seed counts for less; a superseded fact names its replacement.
#[tokio::test(flavor = "multi_thread")]
async fn recall_statuses() {
    let server = org(McpConfig::default());
    let ds = server.state.get("org").unwrap();
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            br#"<https://example.org/memory/agents/agent-7/sessions/s1> {
  <http://example.org/resource/ana> <http://example.org/ontology#leads> <http://example.org/resource/payments> .
  <http://example.org/resource/ana> <http://example.org/ontology#memberOf> <http://example.org/resource/payments> .
}
<https://example.org/memory/agents/agent-7/sessions/s0> {
  <urn:r1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( <http://example.org/resource/ana> <http://example.org/ontology#memberOf> <http://example.org/resource/platform> )>> ;
     <http://www.w3.org/ns/prov#wasInvalidatedBy> <urn:a1> .
  <urn:r2> <http://www.w3.org/ns/prov#wasRevisionOf> <urn:r1> .
}"#
            .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let mut c = Client::start(server.clone());
    let ana = "http://example.org/resource/ana";
    // without memory settings, no status
    let s: Value = serde_json::from_str(
        &c.text(
            "recall",
            json!({"seeds": [ana], "format": "json", "hops": 0}),
        )
        .await,
    )
    .unwrap();
    assert!(s["entities"][0]["facts"][0].get("status").is_none(), "{s}");
    // the fixture is a read-only server, which refuses settings writes through the API
    crate::settings::store_runtime(
        &server.state,
        &ds,
        &crate::settings::MEMORY,
        &json!({"agentGraphs": ["https://example.org/memory/agents/*"]}),
    )
    .unwrap();
    let s: Value = serde_json::from_str(
        &c.text(
            "recall",
            json!({"seeds": [ana], "format": "json", "hops": 0, "includeSuperseded": true}),
        )
        .await,
    )
    .unwrap();
    let facts = s["entities"][0]["facts"].as_array().unwrap();
    let of = |p: &str| {
        facts
            .iter()
            .filter(|f| f["p"] == p)
            .map(|f| f["status"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(of("ex:leads"), ["unreviewed"], "{s}");
    // asserted in the default graph too: reviewed in both graphs
    assert!(of("ex:memberOf").iter().all(|x| x == "reviewed"), "{s}");
    assert!(
        s["citations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["status"] == "unreviewed"),
        "{s}"
    );
    assert_eq!(s["superseded"][0]["replacedBy"], json!(["<urn:r2>"]), "{s}");
    let s: Value = serde_json::from_str(
        &c.text(
            "recall",
            json!({"seeds": [ana], "format": "json", "hops": 0, "statuses": ["reviewed"]}),
        )
        .await,
    )
    .unwrap();
    let facts = s["entities"][0]["facts"].as_array().unwrap();
    assert!(facts.iter().all(|f| f["status"] == "reviewed"), "{s}");
    assert!(!facts.iter().any(|f| f["p"] == "ex:leads"), "{s}");
    let text = c.text("recall", json!({"seeds": [ana], "hops": 0})).await;
    assert!(
        text.contains("ex:leads res:payments [") && text.contains(" unreviewed\n"),
        "{text}"
    );
    let (_, meta) = c
        .error("recall", json!({"seeds": [ana], "unreviewedWeight": 2}))
        .await;
    assert_eq!(meta["code"], "bad-argument");
}

/// C18 §8.4 (Phase 5): with `recency`, found seeds rank by the age of their newest
/// fact and by how many graphs assert their facts; a bad half-life is refused.
#[cfg(feature = "text")]
#[tokio::test(flavor = "multi_thread")]
async fn recall_recency() {
    let server = org(McpConfig::default());
    let ds = server.state.get("org").unwrap();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let fact = |g: &str, s: &str, label: &str, at: &str| {
        format!(
            r#"<https://example.org/memory/agents/a/sessions/{g}> {{
  <http://example.org/resource/{s}> <http://www.w3.org/2000/01/rdf-schema#label> "{label}" .
  <urn:r-{g}-{s}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( <http://example.org/resource/{s}> <http://www.w3.org/2000/01/rdf-schema#label> "{label}" )>> ;
     <http://www.w3.org/ns/prov#generatedAtTime> "{at}"^^<http://www.w3.org/2001/XMLSchema#dateTime> .
}}
"#
        )
    };
    let trig = [
        fact("s1", "aaa", "zephyr project aaa", "2020-01-01T00:00:00Z"),
        fact("s2", "bbb", "zephyr project bbb", &now),
        fact("s3", "ccc", "zephyr project ccc", &now),
        fact("s4", "ccc", "zephyr project ccc", &now),
    ]
    .concat();
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            trig.into_bytes(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    ds.store
        .enable_text(sparkles::text::TextConfig::default())
        .unwrap();
    let mut c = Client::start(server.clone());
    let order = |s: &Value| -> Vec<String> {
        let mut seeds: Vec<(u64, String)> = s["entities"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| Some((e["seed"].as_u64()?, e["iri"].as_str()?.to_string())))
            .collect();
        seeds.sort();
        seeds.into_iter().map(|(_, i)| i).collect()
    };
    let plain: Value = serde_json::from_str(
        &c.text(
            "recall",
            json!({"query": "zephyr", "format": "json", "hops": 0}),
        )
        .await,
    )
    .unwrap();
    assert_eq!(order(&plain), ["res:aaa", "res:bbb", "res:ccc"], "{plain}");
    let ranked: Value = serde_json::from_str(
        &c.text(
            "recall",
            json!({"query": "zephyr", "format": "json", "hops": 0, "recency": "30d"}),
        )
        .await,
    )
    .unwrap();
    // two graphs first, then the recent fact, then the one from 2020
    assert_eq!(
        order(&ranked),
        ["res:ccc", "res:bbb", "res:aaa"],
        "{ranked}"
    );
    for bad in ["90", "x", "-3d", "0d"] {
        let (_, e) = c
            .error("recall", json!({"query": "zephyr", "recency": bad}))
            .await;
        assert_eq!(e["code"], "bad-argument", "{bad}");
    }
}

async fn post(app: &axum::Router, path: &str, body: &str) -> (u16, Value) {
    use tower::ServiceExt;
    let req = axum::http::Request::post(path)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&b)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&b).into())),
    )
}

/// `/{ds}/check`, `/{ds}/recall` and `/{ds}/sparql/diagnose` run the tools.
#[tokio::test(flavor = "multi_thread")]
async fn http_routes() {
    let server = org(McpConfig::default());
    let app = crate::http::router(server.state.clone());
    let q = format!("{PREFIXES}SELECT ?p {{ ?p a ex:Person ; foaf:name \"Ana Lima\" }}");
    let (s, v) = post(
        &app,
        "/org/check",
        &json!({"query": q, "terms": true}).to_string(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["dataset"], "org");
    assert!(v["terms"].as_array().unwrap().len() >= 2, "{v}");
    let (s, v) = post(
        &app,
        "/org/sparql/diagnose",
        &json!({"query": q}).to_string(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["first"]["text"], "?p foaf:name \"Ana Lima\"", "{v}");
    let (s, v) = post(
        &app,
        "/org/recall",
        &json!({"seeds": ["http://example.org/resource/ana"]}).to_string(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["entities"][0]["iri"], "res:ana", "{v}");
    // the body never names the dataset, recall is always JSON
    let (s, v) = post(
        &app,
        "/org/check",
        &json!({"query": q, "dataset": "x"}).to_string(),
    )
    .await;
    assert_eq!((s, v["code"].clone()), (400, json!("bad-argument")));
    let (s, _) = post(
        &app,
        "/org/recall",
        &json!({"seeds": ["http://example.org/resource/ana"], "format": "text"}).to_string(),
    )
    .await;
    assert_eq!(s, 400);
    // tool errors keep their code and status
    let (s, v) = post(
        &app,
        "/org/sparql/diagnose",
        &json!({"query": "DELETE WHERE { ?s ?p ?o }"}).to_string(),
    )
    .await;
    assert_eq!((s, v["code"].clone()), (400, json!("not-a-query")), "{v}");
    let (s, v) = post(&app, "/org/check", "[1]").await;
    assert_eq!(s, 400, "{v}");
    let (s, _) = post(&app, "/nope/check", &json!({"query": q}).to_string()).await;
    assert_eq!(s, 404);
    // another branch is refused
    let (s, v) = post(
        &app,
        "/org/check?branch=dev",
        &json!({"query": q}).to_string(),
    )
    .await;
    assert_eq!(s, 400, "{v}");
}
