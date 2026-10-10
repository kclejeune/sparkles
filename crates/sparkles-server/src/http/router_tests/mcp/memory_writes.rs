//! `assert_facts` and the branch tools of C17 Phases 1b and 1c through `/$/mcp`: the
//! acceptance examples A6–A10, A14, A16 and A19, the checks of §5.6, and the access
//! rules of §5.7 and §6 for each caller, including the agent grants of C18 §8.6.

use super::*;

/// The `mem` dataset of C17 §14 at commit 1, before the stand-up notes of 2026-10-08.
const MEM: &str = r#"@prefix ex:     <http://example.org/> .
@prefix schema: <http://schema.org/> .
@prefix org:    <http://www.w3.org/ns/org#> .
@prefix rdf:    <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs:   <http://www.w3.org/2000/01/rdf-schema#> .
@prefix prov:   <http://www.w3.org/ns/prov#> .
@prefix xsd:    <http://www.w3.org/2001/XMLSchema#> .
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

const NOTES_1008: &str = "https://example.org/notes/2026-10-08";

fn attach_mem(st: &AppState) {
    let ds = st.attach("mem", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            MEM.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    #[cfg(feature = "text")]
    ds.store
        .enable_text(sparkles::text::TextConfig::default())
        .unwrap();
}

/// An open server with `mem` and the MCP flags `flags`.
fn mem_server(flags: &[&str]) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.mcp = Some(conf(&st, flags));
    let state = Arc::new(st);
    attach_mem(&state);
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

fn head(s: &Server) -> u64 {
    s.state.get("mem").unwrap().store.head_commit().seq
}

/// The rows of a SELECT over the head of `mem` (or of its branch `branch`), each
/// term in N-Triples form.
fn select(s: &Server, branch: Option<&str>, q: &str) -> Vec<Vec<String>> {
    let ds = s.state.get("mem").unwrap();
    let ds = match branch {
        Some(b) => s.state.branch_dataset(&ds, b).unwrap(),
        None => ds,
    };
    sparkles::sparql::query(
        ds.store.snapshot(),
        q,
        &sparkles::sparql::QueryOptions::default(),
    )
    .unwrap()
    .rows()
    .into_iter()
    .map(|r| {
        r.into_iter()
            .map(|t| t.map(|t| t.to_string()).unwrap_or_default())
            .collect()
    })
    .collect()
}

const PREFIXES: &str = "PREFIX ex: <http://example.org/> PREFIX org: <http://www.w3.org/ns/org#> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX prov: <http://www.w3.org/ns/prov#> PREFIX spk: <urn:x-sparkles:> PREFIX schema: <http://schema.org/> ";

/// The arguments of A6 and A7.
fn standup(dry_run: bool) -> J {
    json!({
        "dataset": "mem",
        "source": {"iri": NOTES_1008, "title": "Stand-up notes"},
        "entities": [{"key": "_:pay", "label": "Payments team", "types": ["org:OrganizationalUnit"]}],
        "facts": [
            {"s": "_:pay", "p": "org:unitOf", "o": "ex:acme"},
            {"s": "ex:ana", "p": "org:memberOf", "o": "_:pay", "mode": "replace",
             "confidence": 0.9, "quote": "Ana moved to the payments team this week."}
        ],
        "replaceScope": "writable",
        "idempotencyKey": "standup-1008",
        "message": "Stand-up notes of 2026-10-08",
        "agent": {"name": "notes-bot", "model": "m-1"},
        "dryRun": dry_run
    })
}

/// The `data` of a failed call: its errors' codes.
fn error_codes(r: &J) -> Vec<String> {
    r["_meta"]["io.github.kclejeune.sparkles/error"]["data"]["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("{r}"))
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .collect()
}

fn error_data(r: &J) -> J {
    r["_meta"]["io.github.kclejeune.sparkles/error"]["data"].clone()
}

/// A6, A7 and A8: a dry run, the write with its supersession, the retry, and the
/// duplicate check against the unit the write minted.
#[tokio::test(flavor = "multi_thread")]
async fn a6_to_a8_assert_supersede_and_retry() {
    let s = mem_server(&["--mcp-allow-update"]);
    assert_eq!(head(&s), 1);
    // A6
    let r = tool(&s.app, "assert_facts", standup(true), &[]).await;
    assert_eq!(r["isError"], false, "{r}");
    let out = &r["structuredContent"];
    assert_eq!(out["committed"], false, "{out}");
    assert_eq!(out["wouldCommit"], true, "{out}");
    assert_eq!(out["head"], 1, "{out}");
    let pay = out["minted"]["_:pay"].as_str().unwrap().to_string();
    assert!(pay.starts_with("<urn:uuid:"), "{out}");
    assert_eq!(out["superseded"][0]["reifier"], "<urn:uuid:r1>", "{out}");
    assert_eq!(
        out["superseded"][0]["graph"], "<https://example.org/notes/2026-10-01>",
        "{out}"
    );
    assert_eq!(out["conflicts"], json!([]));
    assert!(out["dryRun"]["outcome"] == "commit", "{out}");
    assert_eq!(head(&s), 1);
    // A7
    let mut args = standup(false);
    args["ifHead"] = 1.into();
    let r = tool(&s.app, "assert_facts", args.clone(), &[]).await;
    assert_eq!(r["isError"], false, "{r}");
    let out = &r["structuredContent"];
    assert_eq!(
        (&out["committed"], &out["commit"]),
        (&json!(true), &json!(2)),
        "{out}"
    );
    assert_eq!(out["minted"]["_:pay"], pay.as_str());
    assert_eq!(out["deleted"], 1, "{out}");
    let activity = out["activity"].as_str().unwrap().to_string();
    assert_eq!(head(&s), 2);
    let units = select(
        &s,
        None,
        &format!("{PREFIXES} SELECT ?t WHERE {{ GRAPH ?g {{ ex:ana org:memberOf ?t }} }}"),
    );
    assert_eq!(units, [[pay.clone()]]);
    let old = select(
        &s,
        None,
        &format!(
            "{PREFIXES} SELECT ?r WHERE {{ GRAPH ?g {{ ?r rdf:reifies <<( ex:ana org:memberOf ex:platform )>> ; prov:wasInvalidatedBy ?a ; prov:invalidatedAtTime ?t }} }}"
        ),
    );
    assert_eq!(old, [["<urn:uuid:r1>".to_string()]]);
    // the new fact's reifier, its provenance and the activity share the graph
    let rows = select(
        &s,
        None,
        &format!(
            "{PREFIXES} SELECT ?a ?src ?c ?q ?prev ?who ?label ?key ?bot WHERE {{ GRAPH <{NOTES_1008}> {{
               ?r rdf:reifies <<( ex:ana org:memberOf {pay} )>> ; prov:wasGeneratedBy ?a ;
                  prov:wasDerivedFrom ?src ; spk:confidence ?c ; spk:quote ?q ; prov:wasRevisionOf ?prev ;
                  prov:generatedAtTime ?at .
               ?a a prov:Activity ; prov:wasAssociatedWith ?who ; rdfs:label ?label ; spk:idempotencyKey ?key ;
                  prov:wasAssociatedWith ?bot .
               ?bot a prov:SoftwareAgent ; prov:actedOnBehalfOf ?who ; spk:model \"m-1\" .
               {pay} a org:OrganizationalUnit ; rdfs:label \"Payments team\" ; org:unitOf ex:acme . }} }}"
        ),
    );
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row[0], activity);
    assert_eq!(row[1], format!("<{NOTES_1008}>"));
    assert_eq!(
        row[2],
        "\"0.9\"^^<http://www.w3.org/2001/XMLSchema#decimal>"
    );
    assert_eq!(row[4], "<urn:uuid:r1>");
    assert!(row[5].starts_with("<urn:x-sparkles:principal:"), "{row:?}");
    assert_eq!(row[7], "\"standup-1008\"");
    // the source's title, and the commit's message
    assert_eq!(
        select(
            &s,
            None,
            &format!(
                "{PREFIXES} SELECT ?t WHERE {{ GRAPH <{NOTES_1008}> {{ <{NOTES_1008}> rdfs:label ?t }} }}"
            )
        ),
        [["\"Stand-up notes\"".to_string()]]
    );
    // the retry writes nothing
    let r = tool(&s.app, "assert_facts", args, &[]).await;
    let out = &r["structuredContent"];
    assert_eq!(out["alreadyApplied"], true, "{r}");
    assert_eq!(out["committed"], false);
    assert_eq!(out["activity"], activity.as_str());
    assert_eq!(out["minted"]["_:pay"], pay.as_str());
    assert_eq!(head(&s), 2);
    // a stale ifHead fails under the writer lock
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008, "facts": [{"s": "ex:ana2", "p": "org:memberOf", "o": "ex:platform"}], "ifHead": 1}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "precondition-failed");
    assert_eq!(head(&s), 2);
    // A8: a new unit whose label matches the minted one
    #[cfg(feature = "text")]
    {
        let dup = |distinct: Option<&str>| {
            let mut e = json!({"key": "_:p2", "label": "payments team", "types": ["org:OrganizationalUnit"]});
            if let Some(d) = distinct {
                e["distinctFrom"] = json!([d]);
            }
            json!({
                "graph": "https://example.org/notes/2026-10-09",
                "entities": [e],
                "facts": [{"s": "_:p2", "p": "org:unitOf", "o": "ex:acme"}],
                "idempotencyKey": "a8"
            })
        };
        let r = tool(&s.app, "assert_facts", dup(None), &[]).await;
        assert_eq!(tool_error(&r), "possible-duplicate", "{r}");
        assert_eq!(error_data(&r)["errors"][0]["candidates"][0], pay.as_str());
        assert_eq!(head(&s), 2);
        let r = tool(&s.app, "assert_facts", dup(Some(&pay)), &[]).await;
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(head(&s), 3);
    }
}

/// A9 and the other checks of §5.6: every failure of one call is reported together,
/// and nothing is written.
#[tokio::test(flavor = "multi_thread")]
async fn a9_checks_report_every_problem() {
    let s = mem_server(&["--mcp-allow-update"]);
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008, "facts": [
            {"s": "ex:ana", "p": "org:memberOf", "o": "ex:paymnts"},
            {"s": "ex:ana", "p": "org:memberof", "o": "ex:platform"}
        ]}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "invalid-facts", "{r}");
    let data = error_data(&r);
    let mut codes = error_codes(&r);
    codes.sort();
    assert_eq!(codes, ["unknown-entity", "unknown-predicate"], "{data}");
    let p = data["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["code"] == "unknown-predicate")
        .unwrap();
    assert_eq!(p["term"], "org:memberof");
    assert_eq!(p["at"], "facts[1].p");
    assert_eq!(p["suggestions"][0]["term"], "org:memberOf", "{p}");
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown-entity")
    );
    // allowUnknownIris accepts the invented IRI, but never the predicate
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008, "allowUnknownIris": true, "facts": [
            {"s": "ex:ana", "p": "org:memberof", "o": "ex:paymnts"}
        ]}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "unknown-predicate");
    // an unknown class, keys that are undeclared or unused, a bad literal
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008,
               "entities": [{"key": "_:u", "label": "Unused", "types": ["org:Organisation"]}],
               "facts": [{"s": "_:x", "p": "rdfs:label", "o": "\"unterminated"}]}),
        &[],
    )
    .await;
    let mut codes = error_codes(&r);
    codes.sort();
    assert_eq!(
        codes,
        ["invalid-term", "undeclared-entity", "unused-entity"],
        "{r}"
    );
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008,
               "entities": [{"key": "_:u", "label": "Unit", "types": ["org:Organisation"]}],
               "facts": [{"s": "_:u", "p": "org:unitOf", "o": "ex:acme"}]}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "unknown-class", "{r}");
    assert_eq!(
        error_data(&r)["errors"][0]["suggestions"][0]["term"],
        "org:Organization"
    );
    // a literal of another datatype than the predicate's objects is a warning only
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008, "facts": [{"s": "ex:ana2", "p": "rdfs:label", "o": "\"Ana S.\""}], "dryRun": true}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        r["structuredContent"]["warnings"][0]["code"], "language-tag",
        "{r}"
    );
    // arguments
    for bad in [
        json!({"facts": [{"s": "ex:ana", "p": "rdfs:label", "o": "\"x\""}]}),
        json!({"graph": "default", "facts": [{"s": "ex:ana", "p": "rdfs:label", "o": "\"x\""}]}),
        json!({"graph": NOTES_1008}),
        json!({"graph": NOTES_1008, "facts": [{"s": "ex:ana", "p": "rdfs:label", "o": "\"x\""}], "changes": 1}),
        json!({"graph": NOTES_1008, "facts": [{"s": "ex:ana", "p": "rdfs:label", "o": "\"x\""}], "idempotencyKey": "k".repeat(129)}),
        json!({"graph": NOTES_1008, "facts": (0..501).map(|_| json!({"s": "ex:ana", "p": "rdfs:label", "o": "\"x\""})).collect::<Vec<_>>()}),
        json!({"graph": NOTES_1008, "facts": [{"s": "ex:ana", "p": "rdfs:label", "o": "x".repeat(1 << 20)}]}),
    ] {
        let r = tool(&s.app, "assert_facts", bad.clone(), &[]).await;
        assert_eq!(tool_error(&r), "bad-argument", "{r}");
    }
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008, "facts": [{"s": "ex:ana", "p": "rdfs:label", "o": "\"x\"", "quote": "q".repeat(1001)}]}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "invalid-term", "{r}");
    assert_eq!(head(&s), 1);
}

/// Retractions by reifier and by fact, a fact asserted twice, and a fact without a
/// reifier that a supersession gives one (§3.3).
#[tokio::test(flavor = "multi_thread")]
async fn retractions_keep_the_record() {
    let s = mem_server(&["--mcp-allow-update"]);
    // a fact already asserted in the graph gets a second reifier only
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": "https://example.org/notes/2026-10-01",
               "facts": [{"s": "ex:ana", "p": "org:memberOf", "o": "ex:platform", "quote": "again"}]}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let reifiers = format!(
        "{PREFIXES} SELECT ?r WHERE {{ GRAPH ?g {{ ?r rdf:reifies <<( ex:ana org:memberOf ex:platform )>> FILTER NOT EXISTS {{ ?r prov:wasInvalidatedBy ?a }} }} }}"
    );
    assert_eq!(select(&s, None, &reifiers).len(), 2);
    // retract by reifier: the fact goes, and both of its reifiers record it
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008, "retract": ["<urn:uuid:r1>"]}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let out = &r["structuredContent"];
    assert_eq!(out["retracted"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(out["deleted"], 1, "{out}");
    assert!(select(&s, None, &reifiers).is_empty());
    assert!(
        select(
            &s,
            None,
            &format!("{PREFIXES} SELECT * WHERE {{ GRAPH ?g {{ ex:ana org:memberOf ?t }} }}")
        )
        .is_empty()
    );
    // the same retraction again: nothing is asserted any more
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008, "retract": [{"s": "ex:ana", "p": "org:memberOf", "o": "ex:platform", "graph": "https://example.org/notes/2026-10-01"}]}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "not-asserted", "{r}");
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008, "retract": ["<urn:uuid:nothing>"]}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "unknown-reifier", "{r}");
    // retract by fact: a fact that came in without a reifier gets one for its record
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"graph": NOTES_1008, "retract": [{"s": "ex:acme", "p": "schema:name", "o": "\"Acme Corp\"", "graph": "https://example.org/hr"}]}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let rows = select(
        &s,
        None,
        &format!(
            "{PREFIXES} SELECT ?r WHERE {{ GRAPH <https://example.org/hr> {{ ?r rdf:reifies <<( ex:acme schema:name \"Acme Corp\" )>> ; prov:wasInvalidatedBy ?a }} }}"
        ),
    );
    assert_eq!(rows.len(), 1, "{rows:?}");
}

/// A10: the dataset's guard checks the write, and its dry run reports the same
/// outcome.
#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn a10_the_guard_validates_the_write() {
    let s = mem_server(&["--mcp-allow-update"]);
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> .\n@prefix org: <http://www.w3.org/ns/org#> .\n@prefix ex: <http://example.org/> .\nex:Unit a sh:NodeShape ; sh:targetClass org:OrganizationalUnit ; sh:property [ sh:path org:unitOf ; sh:minCount 1 ; sh:maxCount 1 ] .\n";
    let cfg = json!({"mode": "reject", "dataGraph": "union", "shapes": {"inline": shapes}});
    let put = Request::put("/$/validation/mem")
        .header("content-type", "application/json")
        .body(Body::from(cfg.to_string()))
        .unwrap();
    let r = send(&s.app, put).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let before = head(&s);
    let args = |dry: bool| {
        json!({"graph": NOTES_1008,
               "entities": [{"key": "_:sec", "label": "Security guild", "types": ["org:OrganizationalUnit"]}],
               "facts": [{"s": "ex:ana", "p": "org:memberOf", "o": "_:sec"}],
               "dryRun": dry})
    };
    let r = tool(&s.app, "assert_facts", args(true), &[]).await;
    assert_eq!(r["isError"], false, "{r}");
    let out = &r["structuredContent"];
    assert_eq!(out["validation"]["status"], "rejected", "{out}");
    assert_eq!(
        out["validation"]["results"][0]["sourceConstraintComponent"]["value"],
        "http://www.w3.org/ns/shacl#MinCountConstraintComponent"
    );
    assert_eq!(out["dryRun"]["outcome"], "rejected", "{out}");
    let r = tool(&s.app, "assert_facts", args(false), &[]).await;
    assert_eq!(tool_error(&r), "validation-failed", "{r}");
    assert!(
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("blocking result"),
        "{r}"
    );
    assert_eq!(head(&s), before);
}

/// A16: the write tools are listed only with `--mcp-allow-update`, and
/// `--mcp-disable-tool` leaves out one of them.
#[tokio::test(flavor = "multi_thread")]
async fn a16_tool_listing() {
    const WRITES: [&str; 4] = [
        "assert_facts",
        "create_branch",
        "merge_branch",
        "delete_branch",
    ];
    let s = mem_server(&[]);
    let names = tool_names(&s.app, &[]).await;
    for t in [
        "check_query",
        "similar_queries",
        "link_entities",
        "recall",
        "list_branches",
    ] {
        assert!(names.contains(&t.to_string()), "{t}");
    }
    for t in WRITES {
        assert!(!names.contains(&t.to_string()), "{t}");
    }
    let r = modern(
        &s.app,
        "tools/call",
        json!({"name": "assert_facts", "arguments": {"graph": NOTES_1008}}),
        &[],
    )
    .await;
    assert_eq!(r.rpc()["error"]["code"], -32602, "{}", r.body);
    let s = mem_server(&["--mcp-allow-update", "--mcp-disable-tool", "sparql_update"]);
    let r = modern(&s.app, "tools/list", json!({}), &[]).await;
    let tools = r.rpc()["result"]["tools"].as_array().unwrap().clone();
    let find = |n: &str| tools.iter().find(|t| t["name"] == n).cloned();
    assert!(find("sparql_update").is_none());
    for t in WRITES {
        assert!(find(t).is_some(), "{t}");
    }
    assert_eq!(
        find("assert_facts").unwrap()["annotations"],
        json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false})
    );
    assert_eq!(
        find("merge_branch").unwrap()["annotations"]["destructiveHint"],
        true
    );
    assert_eq!(
        find("delete_branch").unwrap()["annotations"]["destructiveHint"],
        true
    );
    assert_eq!(
        find("create_branch").unwrap()["annotations"]["destructiveHint"],
        false
    );
    // the read tools take `branch`, the branch tools name branches themselves
    let q = find("sparql_query").unwrap();
    assert!(q["inputSchema"]["properties"]["branch"].is_object());
    assert!(find("assert_facts").unwrap()["inputSchema"]["properties"]["branch"].is_object());
    assert!(
        find("list_branches").unwrap()["inputSchema"]["properties"]
            .get("branch")
            .is_none()
    );
    // the instructions gain the writing step, and the agent_memory prompt sets out the
    // loop of §2
    let (_, init) = initialize(&s.app, &[]).await;
    let text = init.rpc()["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.ends_with(
            "Before writing, call link_entities, then write with assert_facts and dryRun first."
        ),
        "{text}"
    );
    let p = modern(
        &s.app,
        "prompts/get",
        json!({"name": "agent_memory", "arguments": {"dataset": "mem"}}),
        &[],
    )
    .await;
    let text = p.rpc()["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    for t in [
        "recall",
        "similar_queries",
        "check_query",
        "link_entities",
        "assert_facts",
        "ifHead",
        "create_branch",
    ] {
        assert!(text.contains(t), "{t}: {text}");
    }
    let s = mem_server(&[]);
    let (_, init) = initialize(&s.app, &[]).await;
    let text = init.rpc()["result"]["instructions"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!text.contains("assert_facts"), "{text}");
    // never on a read-only server
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.read_only = true;
    st.mcp = Some(conf(&st, &["--mcp-allow-update"]));
    let app = router(Arc::new(st));
    let names = tool_names(&app, &[]).await;
    for t in WRITES {
        assert!(!names.contains(&t.to_string()), "{t}");
    }
}

/// A19: an idle scratch branch expires, a branch made over HTTP never does.
#[tokio::test(flavor = "multi_thread")]
async fn a19_idle_scratch_branches_expire() {
    let s = mem_server(&["--mcp-allow-update", "--mcp-scratch-branch-ttl", "1h"]);
    let r = tool(
        &s.app,
        "create_branch",
        json!({"name": "scratch-old", "note": "a test"}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["scratch"], true);
    assert!(r["structuredContent"]["expires"].is_string(), "{r}");
    let req = Request::post("/$/branches/mem")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"kept"}"#))
        .unwrap();
    let r = send(&s.app, req).await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = tool(&s.app, "list_branches", json!({}), &[]).await;
    let list = r["structuredContent"]["branches"]
        .as_array()
        .unwrap()
        .clone();
    let b = |n: &str| list.iter().find(|b| b["name"] == n).cloned().unwrap();
    assert_eq!(b("main")["scratch"], false);
    assert_eq!(b("kept")["scratch"], false);
    assert!(b("kept").get("expires").is_none());
    assert_eq!(b("scratch-old")["scratch"], true);
    assert_eq!(b("scratch-old")["creator"], "local");
    assert!(b("scratch-old")["expires"].is_string());
    assert_eq!(b("scratch-old")["note"], "a test");
    // an hour from now nothing is old enough; two hours from now the scratch branch is
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let ttl = Duration::from_secs(3600);
    assert!(crate::mcp::branches::sweep(&s.state, ttl, now + 1000).is_empty());
    let before = crate::mcp::branches::expired_total();
    crate::mcp::branches::spawn_expiry(s.state.clone(), ttl);
    let gone = crate::mcp::branches::sweep(&s.state, ttl, now + 2 * 3600 * 1000);
    assert_eq!(gone, ["mem@scratch-old"]);
    assert!(crate::mcp::branches::expired_total() > before);
    let r = tool(&s.app, "list_branches", json!({}), &[]).await;
    let names: Vec<&str> = r["structuredContent"]["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["main", "kept"]);
    let mut m = String::new();
    crate::mcp::branches::metrics(&mut m);
    assert!(
        m.contains("sparkles_mcp_scratch_branches_expired_total "),
        "{m}"
    );
    // the flag needs a duration
    let args = ["serve", "--mcp", "--mcp-scratch-branch-ttl", "0s"];
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    assert!(Flags::parse_from(args).mcp.conf(&st).is_err());
}

/// The `branch` argument of the read tools: a branch's data, an unknown branch, and an
/// invalid name.
#[tokio::test(flavor = "multi_thread")]
async fn tools_read_and_write_branches() {
    let s = mem_server(&["--mcp-allow-update"]);
    let r = tool(&s.app, "create_branch", json!({"name": "try"}), &[]).await;
    assert_eq!(r["isError"], false, "{r}");
    let r = tool(
        &s.app,
        "sparql_update",
        json!({"branch": "try", "update": "PREFIX ex: <http://example.org/> INSERT DATA { GRAPH ex:g { ex:x ex:y ex:z } }"}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["branch"], "try", "{r}");
    let ask = |b: Option<&str>| {
        let mut a = json!({"query": "ASK { GRAPH ?g { <http://example.org/x> ?p ?o } }"});
        if let Some(b) = b {
            a["branch"] = b.into();
        }
        a
    };
    let r = tool(&s.app, "sparql_query", ask(Some("try")), &[]).await;
    assert!(
        r["content"][0]["text"].as_str().unwrap().contains("true"),
        "{r}"
    );
    let r = tool(&s.app, "sparql_query", ask(None), &[]).await;
    assert!(
        r["content"][0]["text"].as_str().unwrap().contains("false"),
        "{r}"
    );
    let r = tool(&s.app, "sparql_query", ask(Some("main")), &[]).await;
    assert!(
        r["content"][0]["text"].as_str().unwrap().contains("false"),
        "{r}"
    );
    let r = tool(&s.app, "sparql_query", ask(Some("nope")), &[]).await;
    assert_eq!(tool_error(&r), "no-such-branch", "{r}");
    let r = tool(&s.app, "sparql_query", ask(Some("a/b")), &[]).await;
    assert_eq!(tool_error(&r), "invalid-branch", "{r}");
    let r = tool(&s.app, "list_branches", json!({"branch": "try"}), &[]).await;
    assert_eq!(tool_error(&r), "bad-argument", "{r}");
    // the merge: a preview with the heads to expect, then the merge with them
    let r = tool(
        &s.app,
        "merge_branch",
        json!({"source": "try", "changes": 5}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let out = r["structuredContent"].clone();
    assert_eq!(
        (&out["committed"], &out["wouldCommit"], &out["mergeable"]),
        (&json!(false), &json!(true), &json!(true)),
        "{out}"
    );
    assert_eq!(out["changes"]["total"], 1, "{out}");
    let expect = out["expect"].clone();
    let r = tool(
        &s.app,
        "merge_branch",
        json!({"source": "try", "dryRun": false}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "bad-argument", "{r}");
    // a stale expectation is refused
    let stale =
        json!({"source": expect["source"], "target": expect["target"].as_u64().unwrap() + 7});
    let r = tool(
        &s.app,
        "merge_branch",
        json!({"source": "try", "dryRun": false, "expect": stale}),
        &[],
    )
    .await;
    assert_eq!(tool_error(&r), "head-moved", "{r}");
    let r = tool(
        &s.app,
        "merge_branch",
        json!({"source": "try", "dryRun": false, "expect": expect, "message": "merge try"}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["committed"], true, "{r}");
    let r = tool(&s.app, "sparql_query", ask(None), &[]).await;
    assert!(
        r["content"][0]["text"].as_str().unwrap().contains("true"),
        "{r}"
    );
    let r = tool(&s.app, "delete_branch", json!({"name": "try"}), &[]).await;
    assert_eq!(r["isError"], false, "{r}");
    let r = tool(&s.app, "delete_branch", json!({"name": "main"}), &[]).await;
    assert_eq!(tool_error(&r), "invalid-branch", "{r}");
    let r = tool(&s.app, "sparql_query", ask(Some("try")), &[]).await;
    assert_eq!(tool_error(&r), "no-such-branch", "{r}");
}

#[cfg(feature = "auth")]
mod auth {
    use super::*;
    use crate::auth::{Auth, hash_password_with};
    use crate::http::router_tests::auth::b;

    /// `owner` administers `mem`; `agent-7` reads it and writes the notes graphs (C17
    /// §14); `tmpl` has the agent grants of C18 §8.6, with `proposals-tmpl-*` for its
    /// proposal branches; `reader` only reads.
    fn users() -> String {
        let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
        format!(
            r#"
version = 1

[[users]]
name = "owner"
password = "{owner}"
datasets = {{ mem = "admin" }}

[[users]]
name = "agent-7"
password = "{agent}"
[[users.grants]]
dataset = "mem"
level = "read"
[[users.grants]]
dataset = "mem"
level = "write"
graphs = ["https://example.org/notes/*"]

[[users]]
name = "tmpl"
password = "{tmpl}"
[[users.grants]]
dataset = "mem"
level = "read"
[[users.grants]]
dataset = "mem"
level = "write"
graphs = ["https://example.org/memory/agents/tmpl/*"]
branches = ["main", "proposals-tmpl-*"]
endpoints = ["query", "update", "gsp-rw", "info", "branches"]
[[users.grants]]
dataset = "mem"
level = "write"
graphs = ["https://example.org/hr"]
branches = ["proposals-tmpl-*"]
endpoints = ["query", "update", "gsp-rw", "info", "branches"]

[[users]]
name = "reader"
password = "{reader}"
datasets = {{ mem = "read" }}
"#,
            owner = h("owner-pw"),
            agent = h("agent-7-pw"),
            tmpl = h("tmpl-pw"),
            reader = h("reader-pw"),
        )
    }

    fn authed() -> Server {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("auth.toml");
        std::fs::write(&config, users()).unwrap();
        let mut st =
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
        st.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
        st.mcp = Some(conf(&st, &["--mcp-allow-update"]));
        let state = Arc::new(st);
        attach_mem(&state);
        state.set_phase(crate::obs::Phase::Ready);
        let app = router(state.clone());
        Server {
            _dir: dir,
            state,
            app,
        }
    }

    async fn call(s: &Server, user: &str, name: &str, args: J) -> J {
        tool(&s.app, name, args, &[("authorization", &b(user))]).await
    }

    /// A14 and the access rules of §5.7 and §6.
    #[tokio::test(flavor = "multi_thread")]
    async fn a14_scratch_branches_and_grants() {
        let s = authed();
        // an unrestricted principal: a scratch branch, a write on it, recall on it
        let r = call(&s, "owner", "create_branch", json!({"name": "scratch-s1"})).await;
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(r["structuredContent"]["creator"], "user:owner", "{r}");
        let r = call(
            &s,
            "owner",
            "assert_facts",
            json!({"branch": "scratch-s1", "graph": NOTES_1008,
                   "facts": [{"s": "ex:ana2", "p": "org:memberOf", "o": "ex:platform"}]}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(r["structuredContent"]["branch"], "scratch-s1");
        let on = |b: Option<&str>| {
            let mut a = json!({"seeds": ["ex:ana2"], "hops": 0});
            if let Some(b) = b {
                a["branch"] = b.into();
            }
            a
        };
        let r = call(&s, "owner", "recall", on(Some("scratch-s1"))).await;
        let t = r["content"][0]["text"].as_str().unwrap();
        assert!(t.contains("ex:ana2 org:memberOf ex:platform"), "{t}");
        let r = call(&s, "owner", "recall", on(None)).await;
        let t = r["content"][0]["text"].as_str().unwrap();
        assert!(!t.contains("ex:ana2 org:memberOf ex:platform"), "{t}");
        let r = call(&s, "owner", "merge_branch", json!({"source": "scratch-s1"})).await;
        assert_eq!(r["isError"], false, "{r}");
        let out = &r["structuredContent"];
        assert_eq!(out["wouldCommit"], true, "{out}");
        assert!(out["merge"].is_object(), "{out}");
        assert!(out["expect"]["source"].is_u64(), "{out}");
        // agent-7: its own scratch branch, a write on it, and a merge into main
        let r = call(
            &s,
            "agent-7",
            "create_branch",
            json!({"name": "scratch-s2"}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(r["structuredContent"]["creator"], "user:agent-7");
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            json!({"branch": "scratch-s2", "graph": "https://example.org/notes/2026-10-09",
                   "facts": [{"s": "ex:ana", "p": "org:memberOf", "o": "ex:platform"}],
                   "idempotencyKey": "s2"}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let r = call(
            &s,
            "agent-7",
            "merge_branch",
            json!({"source": "scratch-s2"}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let expect = r["structuredContent"]["expect"].clone();
        let r = call(
            &s,
            "agent-7",
            "merge_branch",
            json!({"source": "scratch-s2", "dryRun": false, "expect": expect}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(r["structuredContent"]["committed"], true, "{r}");
        assert_eq!(
            select(
                &s,
                None,
                &format!("{PREFIXES} SELECT ?a WHERE {{ GRAPH <https://example.org/notes/2026-10-09> {{ ?a a prov:Activity ; prov:wasAssociatedWith <urn:x-sparkles:principal:user:agent-7> }} }}")
            )
            .len(),
            1
        );
        // once another principal writes a graph agent-7 may not write, the merge is
        // refused
        let r = call(
            &s,
            "owner",
            "sparql_update",
            json!({"branch": "scratch-s2", "update": "INSERT DATA { GRAPH <https://example.org/hr> { <http://example.org/ana2> <http://schema.org/name> \"Ana S\" } }"}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let r = call(
            &s,
            "agent-7",
            "merge_branch",
            json!({"source": "scratch-s2"}),
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden", "{r}");
        // agent-7 may not merge, delete or write through another's scratch branch
        let r = call(
            &s,
            "agent-7",
            "merge_branch",
            json!({"source": "scratch-s1"}),
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden", "{r}");
        let r = call(
            &s,
            "agent-7",
            "delete_branch",
            json!({"name": "scratch-s1"}),
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden", "{r}");
        // on a branch its grants are what they are on main
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            json!({"branch": "scratch-s1", "graph": "https://example.org/hr",
                   "facts": [{"s": "ex:ana2", "p": "org:memberOf", "o": "ex:platform"}]}),
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden", "{r}");
        // its own branch it may delete; the owner deletes the other
        let r = call(
            &s,
            "agent-7",
            "delete_branch",
            json!({"name": "scratch-s2"}),
        )
        .await;
        assert_eq!(tool_error(&r), "unmerged", "{r}");
        let r = call(
            &s,
            "agent-7",
            "delete_branch",
            json!({"name": "scratch-s2", "force": true}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let r = call(
            &s,
            "owner",
            "delete_branch",
            json!({"name": "scratch-s1", "force": true}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let r = call(&s, "owner", "list_branches", json!({})).await;
        assert_eq!(
            r["structuredContent"]["branches"].as_array().unwrap().len(),
            1,
            "{r}"
        );
        // the HTTP routes keep F09 §6.1: agent-7 cannot create a branch there
        let req = Request::post("/$/branches/mem")
            .header("content-type", "application/json")
            .header("authorization", b("agent-7"))
            .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())))
            .body(Body::from(r#"{"name":"http-try"}"#))
            .unwrap();
        assert_eq!(send(&s.app, req).await.status, StatusCode::FORBIDDEN);
    }

    /// §6 for `assert_facts`: the target graph and every graph a supersession or
    /// retraction changes need `write`; the checks read the caller's view.
    #[tokio::test(flavor = "multi_thread")]
    async fn assert_facts_follows_graph_grants() {
        let s = authed();
        let before = head(&s);
        // a graph agent-7 may not write
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            json!({"graph": "https://example.org/hr",
                   "facts": [{"s": "ex:ana2", "p": "org:memberOf", "o": "ex:platform"}]}),
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden", "{r}");
        // a replace whose old value is in a graph it may only read leaves that value
        // and reports a conflict
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            json!({"graph": NOTES_1008, "replaceScope": "writable",
                   "facts": [{"s": "ex:ana", "p": "schema:email", "o": "\"ana@acme.example\"", "mode": "replace"}]}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let out = &r["structuredContent"];
        assert_eq!(out["conflicts"][0]["reason"], "not-writable", "{out}");
        assert_eq!(out["conflicts"][0]["graph"], "<https://example.org/hr>");
        assert_eq!(out["superseded"], json!([]));
        assert_eq!(
            select(
                &s,
                None,
                &format!(
                    "{PREFIXES} SELECT ?e WHERE {{ GRAPH <https://example.org/hr> {{ ex:ana schema:email ?e }} }}"
                )
            ),
            [["\"ana@example.org\"".to_string()]]
        );
        // a retraction in a graph it may only read
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            json!({"graph": NOTES_1008, "retract": [{"s": "ex:acme", "p": "schema:name", "o": "\"Acme Corp\"", "graph": "https://example.org/hr"}]}),
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden", "{r}");
        // a reader writes nothing and creates no branch
        for (name, args) in [
            (
                "assert_facts",
                json!({"graph": NOTES_1008, "facts": [{"s": "ex:ana2", "p": "org:memberOf", "o": "ex:platform"}]}),
            ),
            ("create_branch", json!({"name": "reader-try"})),
        ] {
            let r = call(&s, "reader", name, args).await;
            assert_eq!(tool_error(&r), "forbidden", "{name}: {r}");
        }
        let names = tool_names(&s.app, &[("authorization", &b("reader"))]).await;
        assert!(names.contains(&"list_branches".to_string()));
        assert!(!names.contains(&"assert_facts".to_string()), "{names:?}");
        let names = tool_names(&s.app, &[("authorization", &b("agent-7"))]).await;
        assert!(names.contains(&"assert_facts".to_string()), "{names:?}");
        assert_eq!(head(&s), before + 1);
    }

    /// The agent grants of C18 §8.6: the agent's own graphs on main, curated graphs only
    /// on its proposal branches, and never a merge.
    #[tokio::test(flavor = "multi_thread")]
    async fn c18_agent_grants() {
        let s = authed();
        let own = "https://example.org/memory/agents/tmpl/s1";
        let fact = |b: Option<&str>, g: &str| {
            let mut a = json!({"graph": g, "facts": [{"s": "ex:ana2", "p": "org:memberOf", "o": "ex:platform"}]});
            if let Some(b) = b {
                a["branch"] = b.into();
            }
            a
        };
        let r = call(&s, "tmpl", "assert_facts", fact(None, own)).await;
        assert_eq!(r["isError"], false, "{r}");
        let r = call(
            &s,
            "tmpl",
            "assert_facts",
            fact(None, "https://example.org/hr"),
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden", "{r}");
        let r = call(
            &s,
            "tmpl",
            "create_branch",
            json!({"name": "proposals-tmpl-1"}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        // a branch name outside its grants
        let r = call(&s, "tmpl", "create_branch", json!({"name": "elsewhere"})).await;
        assert_eq!(tool_error(&r), "forbidden", "{r}");
        let r = call(
            &s,
            "tmpl",
            "assert_facts",
            fact(Some("proposals-tmpl-1"), "https://example.org/hr"),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        // no merge endpoint in any grant: no merge and no preview
        let r = call(
            &s,
            "tmpl",
            "merge_branch",
            json!({"source": "proposals-tmpl-1"}),
        )
        .await;
        assert_eq!(tool_error(&r), "forbidden", "{r}");
        // its own proposal branch it may delete
        let r = call(
            &s,
            "tmpl",
            "delete_branch",
            json!({"name": "proposals-tmpl-1", "force": true}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
    }
}
