//! A fixed project handover through the supported memory APIs. Expectations live
//! outside the driver and preserve each source occurrence, including copied evidence.

use super::*;
use crate::auth::{Auth, hash_password_with};
use crate::http::router_tests::auth::b;
use std::collections::{BTreeMap, BTreeSet};

const CORPUS: &str = include_str!("../../../../../../testsuite/memory/acceptance/corpus.json");
const EXPECTED: &str = include_str!("../../../../../../testsuite/memory/acceptance/expected.json");
const VOCABULARY: &str = r#"
@prefix ex: <http://example.org/> .
@prefix schema: <http://schema.org/> .
@prefix org: <http://www.w3.org/ns/org#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
<https://example.org/memory/corpus/public/vocabulary> {
    ex:ana a schema:Person ; rdfs:label "Ana Lima" .
    ex:platform a org:OrganizationalUnit .
    ex:payments a org:OrganizationalUnit .
    ex:security a org:OrganizationalUnit .
    ex:research a org:OrganizationalUnit .
    org:memberOf a rdf:Property ; rdfs:label "member of" .
    schema:description a rdf:Property ; rdfs:label "description" .
}
"#;
const PREFIXES: &str = "PREFIX ex: <http://example.org/> PREFIX org: <http://www.w3.org/ns/org#> PREFIX schema: <http://schema.org/> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX prov: <http://www.w3.org/ns/prov#> PREFIX spk: <urn:x-sparkles:> ";

fn corpus_server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let password = |name: &str| hash_password_with(&format!("{name}-pw"), 8, 1, 1).unwrap();
    let config = dir.path().join("auth.toml");
    std::fs::write(
        &config,
        format!(
            r#"version = 1
[[users]]
name = "owner"
password = "{}"
datasets = {{ mem = "admin" }}
[[users]]
name = "public"
password = "{}"
[[users.grants]]
dataset = "mem"
level = "read"
graphs = ["https://example.org/memory/corpus/public/*"]
"#,
            password("owner"),
            password("public"),
        ),
    )
    .unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    state.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
    state.mcp = Some(conf(&state, &["--mcp-allow-update"]));
    let state = Arc::new(state);
    let dataset = state.attach("mem", DbType::Mem, None).unwrap();
    dataset
        .store
        .load(&[Source::from_bytes(
            VOCABULARY.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    // Conflicts are a declared single-value constraint, not a truth verdict.
    let config = serde_json::from_value(json!({
        "format": 2, "language": "shacl", "mode": "warn", "dataGraph": "union",
        "shapes": {"inline": "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix schema: <http://schema.org/> . @prefix org: <http://www.w3.org/ns/org#> . <urn:shape:membership> a sh:NodeShape ; sh:targetClass schema:Person ; sh:property [ sh:path org:memberOf ; sh:maxCount 1 ] ."}
    }))
    .unwrap();
    dataset
        .dataset
        .validation()
        .guard()
        .set_shacl(config)
        .unwrap();
    state.set_phase(crate::obs::Phase::Ready);
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

async fn mcp(s: &Server, user: &str, name: &str, mut arguments: J) -> J {
    arguments["dataset"] = "mem".into();
    let result = tool(&s.app, name, arguments, &[("authorization", &b(user))]).await;
    assert_eq!(result["isError"], false, "{name}: {result}");
    result
}

async fn post(s: &Server, user: &str, path: &str, content_type: &str, body: String) -> Resp {
    send(
        &s.app,
        Request::builder()
            .method("POST")
            .uri(path)
            .header("authorization", b(user))
            .header(header::CONTENT_TYPE, content_type)
            .header(
                header::ACCEPT,
                "application/json, application/sparql-results+json",
            )
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

async fn http_json(s: &Server, user: &str, path: &str, body: J) -> J {
    let response = post(s, user, path, "application/json", body.to_string()).await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "{path}: {}",
        response.text()
    );
    response.json()
}

fn head(s: &Server) -> u64 {
    s.state.get("mem").unwrap().store.head_commit().seq
}

async fn register(s: &Server, source: &J, branch: Option<&str>) -> J {
    let mut arguments = json!({
        "iri": source["iri"], "graph": source["iri"], "text": source["text"],
        "format": "text/plain",
    });
    if let Some(branch) = branch {
        arguments["branch"] = branch.into();
    }
    mcp(s, "owner", "register_source", arguments).await["structuredContent"].clone()
}

fn assertion(source: &J, rendition: &J, branch: Option<&str>) -> J {
    let text = source["text"].as_str().unwrap();
    let quote = source["quote"].as_str().unwrap();
    let offset = text.find(quote).unwrap();
    let start = text[..offset].chars().count();
    let mut arguments = json!({
        "source": {"iri": source["iri"]},
        "graph": source["iri"],
        "idempotencyKey": format!("corpus-{}-{}", source["id"].as_str().unwrap(), rendition.as_str().unwrap()),
        "facts": [{"s": source["s"], "p": source["p"], "o": source["o"],
            "quote": quote, "span": {"rendition": rendition, "start": start,
                "end": start + quote.chars().count()}}]
    });
    if let Some(branch) = branch {
        arguments["branch"] = branch.into();
    }
    arguments
}

async fn recall(s: &Server, user: &str, extra: J, http: bool) -> J {
    let mut arguments =
        json!({"seeds": ["ex:ana"], "hops": 0, "format": "json", "maxTriples": 100});
    for (key, value) in extra.as_object().unwrap() {
        arguments[key] = value.clone();
    }
    if http {
        http_json(s, user, "/mem/recall", arguments).await
    } else {
        let result = mcp(s, user, "recall", arguments).await;
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }
}

/// Source IDs are resolved by exact graph identity, never by counts or labels.
fn tuples(recalled: &J, sources: &[J]) -> BTreeSet<Vec<String>> {
    assert_eq!(recalled["truncated"], false, "{recalled}");
    let citations = recalled["citations"].as_array().unwrap();
    let mut rows = BTreeSet::new();
    for entity in recalled["entities"].as_array().unwrap() {
        for fact in entity["facts"].as_array().unwrap() {
            if !["org:memberOf", "schema:description"].contains(&fact["p"].as_str().unwrap()) {
                continue;
            }
            let citation = citations
                .iter()
                .find(|c| c["id"] == fact["citation"])
                .unwrap();
            let source = sources
                .iter()
                .find(|source| {
                    citation["graph"] == format!("<{}>", source["iri"].as_str().unwrap())
                })
                .unwrap_or_else(|| panic!("unknown citation: {citation}"));
            assert!(
                rows.insert(vec![
                    source["id"].as_str().unwrap().to_string(),
                    fact["s"].as_str().unwrap().to_string(),
                    fact["p"].as_str().unwrap().to_string(),
                    fact["o"].as_str().unwrap().to_string(),
                ]),
                "duplicate occurrence: {fact}"
            );
        }
    }
    rows
}

fn check(recalled: &J, sources: &[J], expected: &J, stage: &str) {
    let expected: BTreeSet<Vec<String>> = serde_json::from_value(expected[stage].clone()).unwrap();
    assert_eq!(tuples(recalled, sources), expected, "{stage}: {recalled}");
}

/// Conflict values preserve cited source occurrences. Three distinct alternatives
/// need not occupy three entries when several sources assert the same object.
fn conflict_occurrences(recalled: &J, sources: &[J], expected: &J) -> Result<(), String> {
    if recalled["truncated"] != false {
        return Err("conflict response was truncated".into());
    }
    let conflicts = recalled["conflicts"]
        .as_array()
        .ok_or("missing conflicts")?;
    if conflicts.len() != 1 {
        return Err(format!("expected one conflict, got {}", conflicts.len()));
    }
    let conflict = &conflicts[0];
    if conflict["s"] != "ex:ana" || conflict["p"] != "org:memberOf" {
        return Err(format!(
            "unexpected conflict subject or predicate: {conflict}"
        ));
    }
    let expected_rows: BTreeSet<Vec<String>> =
        serde_json::from_value(expected["initial"].clone()).map_err(|e| e.to_string())?;
    let expected_rows: BTreeSet<_> = expected_rows
        .into_iter()
        .filter(|row| row[1] == "ex:ana" && row[2] == "org:memberOf")
        .collect();
    let expected_objects: BTreeSet<_> = expected_rows.iter().map(|row| row[3].clone()).collect();
    if expected_objects.len() != 3 {
        return Err("independent oracle must retain three distinct alternatives".into());
    }
    let citations = recalled["citations"]
        .as_array()
        .ok_or("missing citations")?;
    let entities = recalled["entities"].as_array().ok_or("missing entities")?;
    let values = conflict["values"]
        .as_array()
        .ok_or("missing conflict values")?;
    let mut actual = BTreeSet::new();
    for value in values {
        let id = value["citation"]
            .as_u64()
            .ok_or("missing conflict citation ID")?;
        let mut matching = citations.iter().filter(|c| c["id"].as_u64() == Some(id));
        let citation = matching.next().ok_or("unknown conflict citation")?;
        if matching.next().is_some() {
            return Err("ambiguous conflict citation ID".into());
        }
        let mut matching = sources
            .iter()
            .filter(|source| citation["graph"] == format!("<{}>", source["iri"].as_str().unwrap()));
        let source = matching
            .next()
            .ok_or("conflict citation has no corpus source")?;
        if matching.next().is_some() {
            return Err("ambiguous conflict citation graph".into());
        }
        if citation["source"] != format!("<{}>", source["iri"].as_str().unwrap()) {
            return Err("conflict citation names the wrong document".into());
        }
        let facts = entities
            .iter()
            .flat_map(|entity| entity["facts"].as_array().into_iter().flatten());
        if facts
            .filter(|fact| {
                fact["s"] == conflict["s"]
                    && fact["p"] == conflict["p"]
                    && fact["o"] == value["o"]
                    && fact["citation"] == value["citation"]
            })
            .count()
            != 1
        {
            return Err("conflict value does not identify exactly one cited fact".into());
        }
        let row = vec![
            source["id"]
                .as_str()
                .ok_or("missing source ID")?
                .to_string(),
            conflict["s"].as_str().ok_or("missing subject")?.to_string(),
            conflict["p"]
                .as_str()
                .ok_or("missing predicate")?
                .to_string(),
            value["o"].as_str().ok_or("missing object")?.to_string(),
        ];
        if !actual.insert(row) {
            return Err("duplicate conflict source occurrence".into());
        }
    }
    let objects: BTreeSet<_> = actual.iter().map(|row| row[3].clone()).collect();
    if objects.len() != 3 || objects != expected_objects {
        return Err(format!("wrong distinct conflict alternatives: {objects:?}"));
    }
    if actual != expected_rows {
        return Err(format!("wrong conflict source occurrences: {actual:?}"));
    }
    Ok(())
}

/// The exact oracle must reject damaged occurrences even when a raw count could
/// still look right. These checks use the real response, not another engine path.
fn conflict_oracle_negatives(initial: &J, sources: &[J], expected: &J) {
    let vocabulary = initial["citations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["graph"] == "<https://example.org/memory/corpus/public/vocabulary>")
        .unwrap()["id"]
        .clone();
    for case in 0..7 {
        let mut bad = initial.clone();
        match case {
            0 => {
                bad["conflicts"][0]["values"]
                    .as_array_mut()
                    .unwrap()
                    .remove(0);
            }
            1 => {
                let duplicate = bad["conflicts"][0]["values"][0].clone();
                bad["conflicts"][0]["values"]
                    .as_array_mut()
                    .unwrap()
                    .push(duplicate);
            }
            2 => bad["conflicts"][0]["values"][0]["o"] = "ex:research".into(),
            3 => {
                bad["conflicts"][0]["values"][0]["citation"] =
                    bad["conflicts"][0]["values"][1]["citation"].clone();
            }
            4 => bad["conflicts"][0]["values"][0]["citation"] = vocabulary.clone(),
            5 => bad["conflicts"][0]["s"] = "ex:other".into(),
            6 => bad["conflicts"][0]["p"] = "schema:description".into(),
            _ => unreachable!(),
        }
        assert!(
            conflict_occurrences(&bad, sources, expected).is_err(),
            "conflict negative control {case} was accepted: {bad}"
        );
    }
}

/// Cite the actual persisted reifier, tied to the original write's activity. This
/// checks exact evidence identity without predicting generated UUIDs or clock times.
async fn citations(s: &Server, recalled: &J, sources: &[J], activities: &BTreeMap<String, J>) {
    for citation in recalled["citations"].as_array().unwrap() {
        let Some(source) = sources
            .iter()
            .find(|source| citation["graph"] == format!("<{}>", source["iri"].as_str().unwrap()))
        else {
            continue;
        }; // Vocabulary facts have graph-only citations.
        assert_eq!(citation["source"], citation["graph"], "{citation}");
        assert_eq!(
            citation["quote"],
            serde_json::to_string(source["quote"].as_str().unwrap()).unwrap(),
            "{citation}"
        );
        assert_eq!(
            citation["by"], "<urn:x-sparkles:principal:user:owner>",
            "{citation}"
        );
        chrono::DateTime::parse_from_rfc3339(citation["at"].as_str().unwrap()).unwrap();
        let query = format!(
            "{PREFIXES} SELECT ?s ?p ?o WHERE {{ GRAPH {} {{ {} rdf:reifies <<( ?s ?p ?o )>> ; prov:wasGeneratedBy {} ; prov:wasDerivedFrom {} ; spk:quote {} . FILTER(?s = {} && ?p = {} && ?o = {}) }} }}",
            citation["graph"].as_str().unwrap(),
            citation["reifier"].as_str().unwrap(),
            activities[source["id"].as_str().unwrap()].as_str().unwrap(),
            citation["source"].as_str().unwrap(),
            citation["quote"].as_str().unwrap(),
            source["s"].as_str().unwrap(),
            source["p"].as_str().unwrap(),
            source["o"].as_str().unwrap(),
        );
        let response = post(s, "owner", "/mem/sparql", "application/sparql-query", query).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.text());
        let result = response.json();
        let rows = result["results"]["bindings"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{citation}: {result}");
        assert_eq!(rows[0]["s"]["value"], "http://example.org/ana");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_acceptance_project_handover() {
    let corpus: J = serde_json::from_str(CORPUS).unwrap();
    let expected: J = serde_json::from_str(EXPECTED).unwrap();
    let mut sources = corpus["sources"].as_array().unwrap().clone();
    let s = tokio::task::block_in_place(corpus_server);
    let mut activities = BTreeMap::new();
    let mut writes = BTreeMap::new();
    for (index, source) in sources.iter().enumerate() {
        let registered = register(&s, source, None).await;
        let arguments = assertion(source, &registered["rendition"], None);
        // A passage mismatch must refuse the whole write before a fact is stored.
        if index == 0 {
            let before = head(&s);
            let mut bad = arguments.clone();
            bad["dataset"] = "mem".into();
            bad["facts"][0]["quote"] = "Invented quotation".into();
            let result = tool(
                &s.app,
                "assert_facts",
                bad,
                &[("authorization", &b("owner"))],
            )
            .await;
            assert_eq!(tool_error(&result), "span-mismatch", "{result}");
            assert_eq!(head(&s), before);
        }
        let write = if index % 2 == 0 {
            mcp(&s, "owner", "assert_facts", arguments.clone()).await["structuredContent"].clone()
        } else {
            http_json(&s, "owner", "/mem/facts", arguments.clone()).await
        };
        assert_eq!(write["committed"], true, "{write}");
        activities.insert(
            source["id"].as_str().unwrap().to_string(),
            write["activity"].clone(),
        );
        writes.insert(source["id"].as_str().unwrap().to_string(), arguments);
    }
    let original = head(&s);
    // Keep the original state readable after correction through either API.
    let retained = post(
        &s,
        "owner",
        "/$/snapshots/mem",
        "application/json",
        json!({"name": "memory-handover-initial", "at": original}).to_string(),
    )
    .await;
    assert_eq!(retained.status, StatusCode::CREATED, "{}", retained.text());
    let retained = retained.json();
    assert_eq!(retained["seq"], original, "{retained}");
    assert_eq!(retained["reconstructable"], true, "{retained}");
    assert_eq!(head(&s), original);
    let initial = recall(&s, "owner", json!({}), false).await;
    check(&initial, &sources, &expected, "initial");
    citations(&s, &initial, &sources, &activities).await;
    conflict_occurrences(&initial, &sources, &expected)
        .unwrap_or_else(|error| panic!("MCP conflict proof failed: {error}: {initial}"));
    conflict_oracle_negatives(&initial, &sources, &expected);
    let initial_http = recall(&s, "owner", json!({}), true).await;
    check(&initial_http, &sources, &expected, "initial");
    conflict_occurrences(&initial_http, &sources, &expected)
        .unwrap_or_else(|error| panic!("HTTP conflict proof failed: {error}: {initial_http}"));
    let public = recall(&s, "public", json!({}), false).await;
    check(&public, &sources, &expected, "public");
    let public_text = public.to_string();
    for hidden in ["personnel", "ex:security", "Security", "restricted"] {
        assert!(!public_text.contains(hidden), "hidden {hidden}: {public}");
    }
    let text = mcp(
        &s,
        "public",
        "recall",
        json!({"seeds": ["ex:ana"], "hops": 0}),
    )
    .await;
    let text = text["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text.lines().filter(|line| *line == "# citations").count(),
        1,
        "{text}"
    );
    assert!(
        !text.lines().any(|line| line.starts_with("[999] graph=")),
        "{text}"
    );
    assert!(text.contains("urn:untrusted"), "{text}");

    let retry = http_json(&s, "owner", "/mem/facts", writes["roster"].clone()).await;
    assert_eq!(retry["alreadyApplied"], true, "{retry}");
    assert_eq!(retry["activity"], activities["roster"]);
    assert_eq!(head(&s), original);
    assert_eq!(
        recall(&s, "owner", json!({}), false).await["citations"],
        initial["citations"]
    );

    let correction = &corpus["correction"];
    let registered = register(&s, correction, None).await;
    assert!(
        registered["staleFacts"].as_u64().unwrap() > 0,
        "{registered}"
    );
    let mut arguments = assertion(correction, &registered["rendition"], None);
    arguments["retractStale"] = registered["rendition"].clone();
    let write = http_json(&s, "owner", "/mem/facts", arguments).await;
    assert_eq!(write["committed"], true, "{write}");
    activities.insert("handover".to_string(), write["activity"].clone());
    sources[0] = correction.clone();
    let corrected = recall(&s, "owner", json!({}), false).await;
    check(&corrected, &sources, &expected, "corrected");
    citations(&s, &corrected, &sources, &activities).await;
    let historical = recall(&s, "owner", json!({"at": original}), true).await;
    check(&historical, &sources, &expected, "initial");
    assert_eq!(historical["citations"], initial["citations"]);

    let standup = &corpus["sources"][3];
    let result = mcp(&s, "owner", "assert_facts", json!({
        "graph": standup["iri"], "retract": [{"s": standup["s"], "p": standup["p"], "o": standup["o"], "graph": standup["iri"]}]
    })).await;
    assert_eq!(result["structuredContent"]["committed"], true, "{result}");
    check(
        &recall(&s, "owner", json!({}), true).await,
        &sources,
        &expected,
        "withdrawn",
    );
    let before_branch = head(&s);
    mcp(
        &s,
        "owner",
        "create_branch",
        json!({"name": "memory-proposal"}),
    )
    .await;
    let scratch = &corpus["scratch"];
    let registered = register(&s, scratch, Some("memory-proposal")).await;
    mcp(
        &s,
        "owner",
        "assert_facts",
        assertion(scratch, &registered["rendition"], Some("memory-proposal")),
    )
    .await;
    sources.push(scratch.clone());
    check(
        &recall(&s, "owner", json!({"branch": "memory-proposal"}), false).await,
        &sources,
        &expected,
        "scratch",
    );
    check(
        &recall(&s, "owner", json!({}), true).await,
        &sources,
        &expected,
        "withdrawn",
    );
    assert_eq!(head(&s), before_branch);
}
