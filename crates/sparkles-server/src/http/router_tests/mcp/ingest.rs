//! C18 Phase 3 through `/$/mcp` and the review routes: `register_source`,
//! `read_chunks`, `list_sources`, `ingest_profile` and spans on `assert_facts` (A7–A9,
//! A11), the branch review with **Use existing** and the merge (A10), the inbox with its
//! signals, promotion and rejection (A29, A32), the `review` policy (A31), and the
//! ingest settings routes.

use super::*;

/// A small `org` dataset: two people labelled alike, a unit, its organization, and the
/// vocabulary of the ingest profile.
const ORG: &str = r#"@prefix ex:     <http://example.org/> .
@prefix schema: <http://schema.org/> .
@prefix org:    <http://www.w3.org/ns/org#> .
@prefix rdfs:   <http://www.w3.org/2000/01/rdf-schema#> .
<https://example.org/hr> {
  ex:ana a schema:Person ; rdfs:label "Ana Lima"@en .
  ex:ana-copy a schema:Person ; rdfs:label "Ana Lima"@en .
  ex:kai a schema:Person ; rdfs:label "Kai Ito"@en ; schema:jobTitle "designer" .
  ex:acme schema:status "active" .
  ex:kai org:memberOf ex:payments .
  ex:payments a org:OrganizationalUnit ; rdfs:label "Payments team"@en ; org:unitOf ex:acme .
  ex:acme a org:Organization ; rdfs:label "Acme Corp"@en .
}
"#;

const NOTES: &str = "https://example.org/notes/standup";
const SESSION: &str = "https://example.org/memory/agents/agent-7/sessions/s1";
const CONSOLIDATED: &str = "https://example.org/memory/consolidated";
const MEMBER_OF: &str = "http://www.w3.org/ns/org#memberOf";

const PREFIXES: &str = "PREFIX ex: <http://example.org/> PREFIX org: <http://www.w3.org/ns/org#> PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX prov: <http://www.w3.org/ns/prov#> PREFIX spk: <urn:x-sparkles:> PREFIX schema: <http://schema.org/> ";

/// The stand-up note: Markdown of exactly 5,120 code points with the sentence of A8.
fn note(with_ana: bool) -> String {
    let mut t = String::from("# Stand-up 2026-10-08\n\n");
    if with_ana {
        t.push_str("Ana moved to the payments team this week.\n\n");
    } else {
        t.push_str("Kai is on leave until Friday.\n\n");
    }
    t.push_str("## Notes\n\n");
    let filler = "The checkout redesign is on track and the review is planned. ";
    while t.chars().count() + filler.len() < 5120 {
        t.push_str(filler);
        if t.chars().count() % 7 == 0 {
            t.push_str("\n\n");
        }
    }
    while t.chars().count() < 5120 {
        t.push('.');
    }
    t
}

fn char_offset(text: &str, needle: &str) -> (usize, usize) {
    let b = text.find(needle).unwrap();
    let a = text[..b].chars().count();
    (a, a + needle.chars().count())
}

fn attach_org(st: &AppState) {
    let ds = st.attach("mem", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            ORG.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
}

fn rows(s: &Server, branch: Option<&str>, q: &str) -> Vec<Vec<String>> {
    let ds = s.state.get("mem").unwrap();
    let ds = match branch {
        Some(b) => s.state.branch_dataset(&ds, b).unwrap(),
        None => ds,
    };
    sparkles::sparql::query(
        ds.store.snapshot(),
        &format!("{PREFIXES}{q}"),
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

fn head(s: &Server, branch: Option<&str>) -> u64 {
    let ds = s.state.get("mem").unwrap();
    match branch {
        Some(b) => {
            s.state
                .branch_dataset(&ds, b)
                .unwrap()
                .store
                .head_commit()
                .seq
        }
        None => ds.store.head_commit().seq,
    }
}

fn code(r: &J) -> String {
    tool_error(r)
}

/// The settings routes, the read tools and `keepText` on an open server.
#[tokio::test(flavor = "multi_thread")]
async fn ingest_settings_and_read_tools() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.mcp = Some(conf(&st, &["--mcp-allow-update"]));
    let state = Arc::new(st);
    attach_org(&state);
    let s = Server {
        _dir: dir,
        state: state.clone(),
        app: router(state),
    };
    let req = |m: &str, uri: &str, body: Option<J>| {
        let b = Request::builder().method(m).uri(uri);
        match body {
            Some(j) => b
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(j.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        }
    };
    // a profile that is not valid is refused, a valid one is stored
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/ingest/mem/profiles/default",
            Some(json!({"predicates": ["not an iri"]})),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/ingest/mem/profiles/default",
            Some(
                json!({"classes": ["http://www.w3.org/ns/org#OrganizationalUnit"],
                        "predicates": [MEMBER_OF, "http://www.w3.org/ns/org#unitOf"],
                        "language": "en"}),
            ),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = send(&s.app, req("GET", "/$/ingest/mem/profiles", None)).await;
    assert_eq!(r.json()["keepText"], true);
    assert!(r.json()["profiles"]["default"].is_object(), "{}", r.text());
    let r = send(&s.app, req("GET", "/$/ingest/mem/profiles/nope", None)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // ingest_profile gives the stored vocabulary and an extraction schema
    let r = tool(&s.app, "ingest_profile", json!({"dataset": "mem"}), &[]).await;
    assert_eq!(r["isError"], false, "{r}");
    let out = &r["structuredContent"];
    let preds: Vec<&str> = out["predicates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["iri"].as_str().unwrap())
        .collect();
    assert!(preds.contains(&"org:memberOf"), "{out}");
    assert!(!preds.iter().any(|p| p.contains("label")), "{out}");
    assert_eq!(out["stored"], true, "{out}");
    assert!(out["schema"].is_object());
    // register, read back and list
    let text = note(true);
    let r = tool(
        &s.app,
        "register_source",
        json!({"dataset": "mem", "iri": NOTES, "graph": NOTES, "title": "Stand-up", "format": "text/markdown", "text": text}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let rend = r["structuredContent"]["rendition"]
        .as_str()
        .unwrap()
        .to_string();
    let r = tool(
        &s.app,
        "read_chunks",
        json!({"dataset": "mem", "rendition": rend, "count": 20}),
        &[],
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let joined: String = r["structuredContent"]["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["text"].as_str().unwrap())
        .collect();
    assert_eq!(joined, text);
    let r = tool(&s.app, "list_sources", json!({"dataset": "mem"}), &[]).await;
    let list = r["structuredContent"]["sources"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(list.len(), 1, "{r}");
    assert_eq!(list[0]["title"], "Stand-up");
    assert_eq!(list[0]["rendition"], rend.as_str());
    // without kept text a source keeps its digest and length only
    let r = send(
        &s.app,
        req(
            "PUT",
            "/$/ingest/mem/settings",
            Some(json!({"keepText": false})),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = tool(
        &s.app,
        "register_source",
        json!({"dataset": "mem", "graph": "https://example.org/notes/other", "text": "Kai is on leave."}),
        &[],
    )
    .await;
    assert_eq!(r["structuredContent"]["textKept"], false, "{r}");
    let other = r["structuredContent"]["rendition"]
        .as_str()
        .unwrap()
        .to_string();
    let r = tool(
        &s.app,
        "read_chunks",
        json!({"dataset": "mem", "rendition": other}),
        &[],
    )
    .await;
    assert_eq!(code(&r), "no-text");
    // a span on a source without text needs the quote
    let r = tool(
        &s.app,
        "assert_facts",
        json!({"dataset": "mem", "graph": "https://example.org/notes/other",
               "facts": [{"s": "ex:kai", "p": "org:memberOf", "o": "ex:payments",
                          "span": {"rendition": other, "start": 0, "end": 3}}]}),
        &[],
    )
    .await;
    assert_eq!(code(&r), "quote-required", "{r}");
    let r = send(
        &s.app,
        req("DELETE", "/$/ingest/mem/profiles/default", None),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

/// A33: a client that declares elicitation chooses between the candidates; one
/// without gets `possible-duplicate`.
#[tokio::test(flavor = "multi_thread")]
async fn a33_elicitation_for_possible_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.mcp = Some(conf(&st, &["--mcp-allow-update"]));
    let state = Arc::new(st);
    attach_org(&state);
    let s = Server {
        _dir: dir,
        state: state.clone(),
        app: router(state),
    };
    let args = json!({"dataset": "mem", "graph": NOTES,
        "entities": [{"key": "_:pay", "label": "Payments team", "types": ["org:OrganizationalUnit"]}],
        "facts": [{"s": "ex:ana", "p": "org:memberOf", "o": "_:pay"}]});
    // without elicitation
    let r = tool(&s.app, "assert_facts", args.clone(), &[]).await;
    assert_eq!(code(&r), "possible-duplicate", "{r}");
    // with elicitation
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {"elicitation": {}}
    });
    let r = modern(
        &s.app,
        "tools/call",
        json!({"name": "assert_facts", "arguments": args, "_meta": meta}),
        &[],
    )
    .await;
    let res = r.rpc()["result"].clone();
    assert_eq!(res["resultType"], "input_required", "{res}");
    let state = res["requestState"].clone();
    let field =
        &res["inputRequests"]["entities"]["params"]["requestedSchema"]["properties"]["entity0"];
    let options: Vec<&str> = field["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["const"].as_str().unwrap())
        .collect();
    assert_eq!(options, ["ex:payments", "new"], "{res}");
    assert_eq!(head(&s, None), 1);
    // the retry with a candidate chosen writes the fact with that IRI
    let r = modern(
        &s.app,
        "tools/call",
        json!({"name": "assert_facts", "arguments": args, "_meta": meta,
               "requestState": state,
               "inputResponses": {"entities": {"action": "accept", "content": {"entity0": "ex:payments"}}}}),
        &[],
    )
    .await;
    let res = r.rpc()["result"].clone();
    assert_eq!(res["isError"], false, "{res}");
    let units = rows(
        &s,
        None,
        &format!("SELECT ?u WHERE {{ GRAPH <{NOTES}> {{ ex:ana org:memberOf ?u }} }}"),
    );
    assert_eq!(units, [["<http://example.org/payments>".to_string()]]);
    let units = rows(
        &s,
        None,
        "SELECT ?u WHERE { GRAPH ?g { ?u a org:OrganizationalUnit } }",
    );
    assert_eq!(units.len(), 1, "{units:?}");
    // an answer that names no candidate leaves the error
    let r = modern(
        &s.app,
        "tools/call",
        json!({"name": "assert_facts", "arguments": {"dataset": "mem", "graph": NOTES,
                 "entities": [{"key": "_:p2", "label": "Payments team", "types": ["org:OrganizationalUnit"]}],
                 "facts": [{"s": "ex:kai", "p": "org:memberOf", "o": "_:p2"}]},
               "_meta": meta, "requestState": "forged",
               "inputResponses": {"entities": {"action": "accept", "content": {"entity0": "ex:acme"}}}}),
        &[],
    )
    .await;
    assert_eq!(code(&r.rpc()["result"]), "possible-duplicate");
}

#[cfg(feature = "auth")]
mod auth {
    use super::*;
    use crate::auth::{Auth, hash_password_with};
    use crate::http::router_tests::auth::b;

    /// `owner` administers `mem`; `agent-7` has the agent grants of §8.6 for its own
    /// graphs and the notes graphs, on `main` and its proposal branches.
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
graphs = ["https://example.org/memory/agents/agent-7/*", "https://example.org/notes/*"]
branches = ["main", "proposals.agent-7.*"]
endpoints = ["query", "update", "gsp-r", "gsp-rw", "info", "branches"]

[[users]]
name = "reader"
password = "{reader}"
datasets = {{ mem = "read" }}
"#,
            owner = h("owner-pw"),
            agent = h("agent-7-pw"),
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
        attach_org(&state);
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

    async fn http(s: &Server, user: &str, method: &str, uri: &str, body: Option<J>) -> Resp {
        let mut r = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, b(user));
        let body = match body {
            Some(j) => {
                r = r.header(header::CONTENT_TYPE, "application/json");
                Body::from(j.to_string())
            }
            None => Body::empty(),
        };
        send(&s.app, r.body(body).unwrap()).await
    }

    /// The owner's merge of `branch` into main after its preview.
    async fn merge(s: &Server, branch: &str) {
        let r = call(s, "owner", "merge_branch", json!({"source": branch})).await;
        assert_eq!(r["structuredContent"]["mergeable"], true, "{r}");
        let expect = r["structuredContent"]["expect"].clone();
        let r = call(
            s,
            "owner",
            "merge_branch",
            json!({"source": branch, "dryRun": false, "expect": expect}),
        )
        .await;
        assert_eq!(r["structuredContent"]["committed"], true, "{r}");
    }

    async fn memory_settings(s: &Server, extra: J) {
        let mut v = json!({
            "agentGraphs": ["https://example.org/memory/agents/*"],
            "consolidatedGraph": CONSOLIDATED,
        });
        if let (Some(m), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            m.extend(e.clone());
        }
        let r = http(s, "owner", "PUT", "/$/memory/mem", Some(v)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }

    /// A7, A8, A9, A10 and A11: register, cite, review, use existing, merge, and
    /// re-ingest a changed note.
    #[tokio::test(flavor = "multi_thread")]
    async fn a7_to_a11_ingest_review_and_reingest() {
        let s = authed();
        memory_settings(&s, json!({})).await;
        let r = http(
            &s,
            "owner",
            "PUT",
            "/$/ingest/mem/profiles/default",
            Some(json!({"predicates": [MEMBER_OF, "http://www.w3.org/ns/org#unitOf"]})),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let br = "proposals.agent-7.ingest-standup-1";
        let r = call(&s, "agent-7", "create_branch", json!({"name": br})).await;
        assert_eq!(r["isError"], false, "{r}");
        // A7
        let text = note(true);
        assert_eq!(text.chars().count(), 5120);
        let args = json!({"branch": br, "iri": NOTES, "graph": NOTES, "title": "Stand-up 2026-10-08",
                          "format": "text/markdown", "text": text});
        let r = call(&s, "agent-7", "register_source", args.clone()).await;
        assert_eq!(r["isError"], false, "{r}");
        let out = r["structuredContent"].clone();
        assert_eq!(out["alreadyRegistered"], false, "{out}");
        assert_eq!(out["committed"], true, "{out}");
        assert_eq!(out["length"], 5120);
        assert!(out["digest"].as_str().unwrap().starts_with("sha256:"));
        let rend = out["rendition"].as_str().unwrap().to_string();
        let mut at = 0;
        for c in out["chunks"].as_array().unwrap() {
            assert_eq!(c["start"], at, "{out}");
            assert!(c["end"].as_u64().unwrap() > at);
            at = c["end"].as_u64().unwrap();
        }
        assert_eq!(at, 5120);
        let before = head(&s, Some(br));
        let r = call(&s, "agent-7", "register_source", args).await;
        assert_eq!(r["structuredContent"]["alreadyRegistered"], true, "{r}");
        assert_eq!(r["structuredContent"]["committed"], false, "{r}");
        assert_eq!(head(&s, Some(br)), before);
        // A8
        let (a, e) = char_offset(&text, "Ana moved to the payments team");
        let span = json!({"rendition": rend, "start": a, "end": e});
        let fact = |quote: &str| {
            json!({"branch": br, "graph": NOTES,
                   "entities": [{"key": "_:pay", "label": "Payments", "types": ["org:OrganizationalUnit"],
                                 "distinctFrom": ["ex:payments"]}],
                   "facts": [{"s": "ex:ana", "p": "org:memberOf", "o": "_:pay", "quote": quote, "span": span},
                             {"s": "_:pay", "p": "org:unitOf", "o": "ex:acme", "span": span}],
                   "agent": {"name": "notes-bot"}})
        };
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            fact("Ana leads the payments team"),
        )
        .await;
        assert_eq!(code(&r), "span-mismatch", "{r}");
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            fact("Ana moved to the payments team"),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(r["structuredContent"]["branch"], br);
        let pay = r["structuredContent"]["minted"]["_:pay"]
            .as_str()
            .unwrap()
            .to_string();
        let cited = rows(
            &s,
            Some(br),
            &format!(
                "SELECT ?d WHERE {{ GRAPH <{NOTES}> {{ ?r rdf:reifies <<( ex:ana org:memberOf {pay} )>> ; prov:wasDerivedFrom ?d FILTER(CONTAINS(STR(?d), \"#char=\")) }} }}"
            ),
        );
        let iri = rend.trim_start_matches('<').trim_end_matches('>');
        assert_eq!(cited, [[format!("<{iri}#char={a},{e}>")]]);
        assert!(
            rows(&s, None, "ASK { GRAPH ?g { ex:ana org:memberOf ?x } }").is_empty() || {
                // main is unchanged
                let r = rows(
                    &s,
                    None,
                    "SELECT ?x WHERE { GRAPH ?g { ex:ana org:memberOf ?x } }",
                );
                r.is_empty()
            }
        );
        // A9
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            json!({"branch": br, "graph": NOTES,
                   "facts": [{"s": "ex:ana", "p": "ex:leads", "o": "ex:payments", "span": span}]}),
        )
        .await;
        assert!(r.to_string().contains("unknown-predicate"), "{r}");
        // A10: the review lists the proposed facts with their spans and the new entity
        let r = http(
            &s,
            "owner",
            "GET",
            &format!("/$/memory/mem/review/{br}"),
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let rv = r.json();
        let facts = rv["facts"].as_array().unwrap();
        let member = facts
            .iter()
            .find(|f| f["p"] == format!("<{MEMBER_OF}>"))
            .unwrap_or_else(|| panic!("{rv}"));
        assert_eq!(member["span"]["start"], a, "{rv}");
        assert_eq!(member["signals"]["span"], "pass", "{rv}");
        assert_eq!(
            rv["entities"][0]["iri"],
            pay.trim_start_matches('<').trim_end_matches('>'),
            "{rv}"
        );
        assert_eq!(rv["entities"][0]["label"], "Payments", "{rv}");
        assert!(
            rv["sources"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Ana moved"),
            "{rv}"
        );
        // a reader sees the review but may not change it
        let r = http(
            &s,
            "reader",
            "POST",
            "/$/memory/mem/relink",
            Some(json!({"branch": br, "from": pay, "to": "http://example.org/payments"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
        // Use existing: one commit on the branch
        let before = head(&s, Some(br));
        let r = http(
            &s,
            "owner",
            "POST",
            "/$/memory/mem/relink",
            Some(json!({"branch": br, "from": pay, "to": "http://example.org/payments"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(head(&s, Some(br)), before + 1);
        let left = rows(
            &s,
            Some(br),
            &format!(
                "SELECT ?s ?p ?o WHERE {{ GRAPH ?g {{ ?s ?p ?o FILTER(?s = {pay} || ?o = {pay}) }} }}"
            ),
        );
        assert!(left.is_empty(), "{left:?}");
        let moved = rows(
            &s,
            Some(br),
            &format!(
                "SELECT ?r WHERE {{ GRAPH <{NOTES}> {{ ex:ana org:memberOf ex:payments . ?r rdf:reifies <<( ex:ana org:memberOf ex:payments )>> }} }}"
            ),
        );
        assert_eq!(moved.len(), 1, "{moved:?}");
        // Merge
        merge(&s, br).await;
        let on_main = rows(
            &s,
            None,
            &format!("SELECT ?x WHERE {{ GRAPH <{NOTES}> {{ ex:ana org:memberOf ?x }} }}"),
        );
        assert_eq!(on_main, [["<http://example.org/payments>".to_string()]]);
        let units = rows(
            &s,
            None,
            "SELECT ?u WHERE { GRAPH ?g { ?u a org:OrganizationalUnit } }",
        );
        assert_eq!(units, [["<http://example.org/payments>".to_string()]]);
        // A11: a changed note no longer mentions Ana; re-extraction retracts her team
        let br2 = "proposals.agent-7.ingest-standup-2";
        let r = call(&s, "agent-7", "create_branch", json!({"name": br2})).await;
        assert_eq!(r["isError"], false, "{r}");
        let text2 = note(false);
        let r = call(
            &s,
            "agent-7",
            "register_source",
            json!({"branch": br2, "iri": NOTES, "graph": NOTES, "text": text2}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let out = &r["structuredContent"];
        assert_eq!(out["previousRendition"], rend.as_str(), "{out}");
        assert_eq!(out["staleFacts"], 2, "{out}");
        let rend2 = out["rendition"].as_str().unwrap().to_string();
        assert_ne!(rend2, rend);
        let (a2, e2) = char_offset(&text2, "Kai is on leave");
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            json!({"branch": br2, "graph": NOTES, "retractStale": rend2,
                   "facts": [{"s": "ex:payments", "p": "org:unitOf", "o": "ex:acme",
                              "span": {"rendition": rend2, "start": a2, "end": e2}}]}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let gone = rows(
            &s,
            Some(br2),
            &format!("SELECT ?x WHERE {{ GRAPH <{NOTES}> {{ ex:ana org:memberOf ?x }} }}"),
        );
        assert!(gone.is_empty(), "{gone:?}");
        let kept = rows(
            &s,
            Some(br2),
            &format!(
                "SELECT ?r WHERE {{ GRAPH <{NOTES}> {{ ?r rdf:reifies <<( ex:ana org:memberOf ex:payments )>> ; prov:wasInvalidatedBy ?a }} }}"
            ),
        );
        assert_eq!(kept.len(), 1, "{kept:?}");
        // the fact the new text supports again stays
        let unit = rows(
            &s,
            Some(br2),
            &format!("SELECT ?x WHERE {{ GRAPH <{NOTES}> {{ ex:payments org:unitOf ?x }} }}"),
        );
        assert_eq!(unit.len(), 1, "{unit:?}");
        // the inbox lists both proposal branches the owner can read
        let r = http(&s, "owner", "GET", "/$/memory/mem/inbox", None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let names: Vec<String> = r.json()["branches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&br2.to_string()), "{names:?}");
    }

    /// The session facts of A29 and A32: `agent-7` cites a note in its session graph.
    async fn session_facts(s: &Server) -> String {
        let text = "Ana Lima is now a staff engineer. Kai Ito is on leave until 2026-10-10.";
        let r = call(
            s,
            "agent-7",
            "register_source",
            json!({"graph": SESSION, "iri": format!("{SESSION}#note"), "text": text}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let rend = r["structuredContent"]["rendition"]
            .as_str()
            .unwrap()
            .to_string();
        let (a1, e1) = char_offset(text, "Ana Lima is now a staff engineer");
        let (a2, e2) = char_offset(text, "Kai Ito is on leave until 2026-10-10");
        let r = call(
            s,
            "agent-7",
            "assert_facts",
            json!({"graph": SESSION,
                   "facts": [
                     {"s": "ex:ana", "p": "schema:jobTitle", "o": "\"staff engineer\"", "confidence": 0.99,
                      "span": {"rendition": rend, "start": a1, "end": e1}},
                     {"s": "ex:kai", "p": "schema:status", "o": "\"on leave until 2026-10-10\"",
                      "span": {"rendition": rend, "start": a2, "end": e2}}]}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        rend
    }

    /// A29 and A32: the inbox signals, **Accept all that pass**, promotion and
    /// rejection.
    #[tokio::test(flavor = "multi_thread")]
    async fn a29_a32_inbox_promote_and_reject() {
        let s = authed();
        memory_settings(&s, json!({})).await;
        session_facts(&s).await;
        let r = http(&s, "owner", "GET", "/$/memory/mem/inbox", None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let inbox = r.json();
        assert_eq!(inbox["target"], CONSOLIDATED);
        let session = &inbox["sessions"][0];
        assert_eq!(session["graph"], SESSION, "{inbox}");
        let facts = session["facts"].as_array().unwrap();
        assert_eq!(facts.len(), 2, "{inbox}");
        let by = |p: &str| {
            facts
                .iter()
                .find(|f| f["p"].as_str().unwrap().ends_with(&format!("{p}>")))
                .unwrap_or_else(|| panic!("{p}: {inbox}"))
                .clone()
        };
        let ana = by("jobTitle");
        let kai = by("status");
        assert_eq!(ana["status"], "unreviewed");
        // A32: two people are labelled "Ana Lima": the link check fails despite 0.99
        assert_eq!(ana["confidence"], "0.99", "{ana}");
        assert_eq!(ana["signals"]["span"], "pass", "{ana}");
        assert_eq!(ana["signals"]["link"], "fail", "{ana}");
        assert_eq!(ana["passes"], false, "{ana}");
        assert_eq!(
            ana["candidates"][0]["iri"], "http://example.org/ana-copy",
            "{ana}"
        );
        assert_eq!(kai["signals"]["span"], "pass", "{kai}");
        assert_eq!(kai["signals"]["link"], "pass", "{kai}");
        assert_eq!(kai["signals"]["guard"], "pass", "{kai}");
        assert_eq!(kai["passes"], true, "{kai}");
        let pick = |f: &J| json!({"s": f["s"], "p": f["p"], "o": f["o"], "graph": f["graph"]});
        // a reader may not promote into the consolidated graph
        let r = http(
            &s,
            "reader",
            "POST",
            "/$/memory/mem/promote",
            Some(json!({"facts": [pick(&kai)]})),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
        // A29: promote, merge, and the fact is reviewed
        let r = http(
            &s,
            "owner",
            "POST",
            "/$/memory/mem/promote",
            Some(json!({"facts": [pick(&kai)]})),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let out = r.json();
        let branch = out["branch"].as_str().unwrap().to_string();
        assert!(branch.starts_with("review.owner."), "{out}");
        assert!(branch.ends_with("-1"), "{out}");
        assert_eq!(out["committed"], true, "{out}");
        let r = http(
            &s,
            "owner",
            "GET",
            &format!("/$/memory/mem/review/{branch}"),
            None,
        )
        .await;
        assert_eq!(
            r.json()["facts"].as_array().unwrap().len(),
            1,
            "{}",
            r.text()
        );
        merge(&s, &branch).await;
        let derived = rows(
            &s,
            None,
            &format!(
                "SELECT ?old WHERE {{ GRAPH <{CONSOLIDATED}> {{ ?r rdf:reifies <<( ex:kai schema:status \"on leave until 2026-10-10\" )>> ; prov:wasDerivedFrom ?old }}
                   GRAPH <{SESSION}> {{ ?old rdf:reifies ?t }} }}"
            ),
        );
        assert_eq!(
            derived.len(),
            1,
            "{derived:?} {:?}",
            rows(
                &s,
                None,
                &format!("SELECT ?s ?p ?o WHERE {{ GRAPH <{CONSOLIDATED}> {{ ?s ?p ?o }} }}")
            )
        );
        let r = call(
            &s,
            "owner",
            "recall",
            json!({"seeds": ["ex:kai"], "hops": 0, "format": "json"}),
        )
        .await;
        let text: J = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        let status: Vec<&J> = text["entities"][0]["facts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["p"] == "schema:status")
            .map(|f| &f["status"])
            .collect();
        assert!(
            !status.is_empty() && status.iter().all(|s| *s == "reviewed"),
            "{text}"
        );
        let r = http(&s, "owner", "GET", "/$/memory/mem/inbox", None).await;
        let left: Vec<J> = r.json()["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["facts"].as_array().unwrap().clone())
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
        // reject the other session fact
        let r = http(
            &s,
            "owner",
            "POST",
            "/$/memory/mem/reject",
            Some(json!({"facts": [pick(&ana)], "reason": "two people match"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let gone = rows(
            &s,
            None,
            &format!(
                "SELECT ?r WHERE {{ GRAPH <{SESSION}> {{ ?r rdf:reifies <<( ex:ana schema:jobTitle \"staff engineer\" )>> ; prov:wasInvalidatedBy ?a }} FILTER NOT EXISTS {{ GRAPH ?g {{ ex:ana schema:jobTitle ?t }} }} }}"
            ),
        );
        assert_eq!(gone.len(), 1, "{gone:?}");
        let r = http(&s, "owner", "GET", "/$/commits/mem?limit=1", None).await;
        assert_eq!(
            r.json()["commits"][0]["message"],
            "Rejected by owner: two people match",
            "{}",
            r.text()
        );
        let r = http(&s, "owner", "GET", "/$/memory/mem/inbox", None).await;
        assert_eq!(r.json()["open"], 0, "{}", r.text());
    }

    /// A31: with `conversationFacts: "review"` the agent's facts wait on its inbox
    /// branch.
    #[tokio::test(flavor = "multi_thread")]
    async fn a31_review_policy_holds_conversation_facts() {
        let s = authed();
        memory_settings(
            &s,
            json!({"agents": {"agent-7": {"conversationFacts": "review"}}}),
        )
        .await;
        let before = head(&s, None);
        let r = call(
            &s,
            "agent-7",
            "assert_facts",
            json!({"graph": SESSION, "facts": [{"s": "ex:kai", "p": "schema:status", "o": "\"away\""}]}),
        )
        .await;
        assert_eq!(r["isError"], false, "{r}");
        let out = &r["structuredContent"];
        assert_eq!(out["branch"], "proposals.agent-7.inbox", "{out}");
        assert!(out["notice"].as_str().unwrap().contains("review"), "{out}");
        assert_eq!(head(&s, None), before);
        let on = rows(
            &s,
            Some("proposals.agent-7.inbox"),
            "SELECT ?o WHERE { GRAPH ?g { ex:kai schema:status ?o } }",
        );
        assert_eq!(on, [["\"away\"".to_string()]]);
        // the owner is not an agent under review
        let r = call(
            &s,
            "owner",
            "assert_facts",
            json!({"graph": SESSION, "facts": [{"s": "ex:kai", "p": "schema:status", "o": "\"back\""}]}),
        )
        .await;
        assert!(r["structuredContent"].get("notice").is_none(), "{r}");
        assert_eq!(head(&s, None), before + 1);
        // the inbox lists the branch
        let r = http(&s, "owner", "GET", "/$/memory/mem/inbox", None).await;
        let b = r.json()["branches"][0].clone();
        assert_eq!(b["name"], "proposals.agent-7.inbox", "{b}");
        assert_eq!(b["kind"], "inbox", "{b}");
        assert_eq!(b["facts"], 1, "{b}");
    }
}
