//! The disclosure guard of GraphRAG reports graphs (spec C22 §6.6, A15, A16) on every
//! read path: SPARQL, the graph list, counts, time travel, the Graph Store, the schema
//! report and the change feed.

use super::*;
use crate::auth::Endpoint;
use crate::graphrag::guard::{CaptureInput, DISCLOSURE_GRAPH, capture, gr};

const REPORTS: &str = "urn:x-sparkles:graphrag:reports";
const REPORTS_ENC: &str = "urn%3Ax-sparkles%3Agraphrag%3Areports";

const DATA: &str = r#"@prefix ex: <http://ex/> .
<urn:g:a> { ex:x ex:knows ex:y . ex:x ex:owner "tua" . ex:x ex:secret "s" . }
<urn:g:b> { ex:y ex:knows ex:z . }
<urn:g:c> { ex:z ex:knows ex:x . }
<urn:x-sparkles:graphrag:reports> { ex:r ex:about ex:x . ex:r ex:summary "the report" . }
"#;

/// The users, each with the password `<name>-pw`. The protection `own` hides
/// `ex:secret`, and with `pattern` only on resources tua does not own:
/// * `tadmin` administers the dataset and reads every graph;
/// * `tua` reads `urn:g:*` and the reports graph under the protection `own`, and the
///   reports were built as tua;
/// * `tub` has tua's grants, but `own` depends on `?user`, so tub sees other triples;
/// * `tlift` has the same grants and lifts `own`;
/// * `tpart` reads `urn:g:a` and the reports graph only;
/// * `tnarrow` reads the same as tpart and lifts `own`.
///
/// The change feed refuses callers whose protections depend on the data, so its tests
/// use the protection without its pattern.
fn users(tua_graphs: &str, pattern: bool) -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[protections]]
name = "own"
dataset = "rag"
predicates = ["http://ex/secret"]
{pattern}

[[users]]
name = "tadmin"
password = "{tadmin}"
datasets = {{ rag = "admin" }}

[[users]]
name = "tua"
password = "{tua}"
[[users.grants]]
dataset = "rag"
level = "read"
graphs = [{tua_graphs}]

[[users]]
name = "tub"
password = "{tub}"
[[users.grants]]
dataset = "rag"
level = "read"
graphs = ["urn:g:*", "{REPORTS}"]

[[users]]
name = "tlift"
password = "{tlift}"
[[users.grants]]
dataset = "rag"
level = "read"
graphs = ["urn:g:*", "{REPORTS}"]
lifts = ["own"]

[[users]]
name = "tpart"
password = "{tpart}"
[[users.grants]]
dataset = "rag"
level = "read"
graphs = ["urn:g:a", "{REPORTS}"]

[[users]]
name = "tzero"
password = "{tzero}"

[[users]]
name = "twrite"
password = "{twrite}"
[[users.grants]]
dataset = "rag"
level = "write"
graphs = ["urn:g:*", "{REPORTS}"]
lifts = ["own"]

[[users]]
name = "tmixed"
password = "{tmixed}"
[[users.grants]]
dataset = "rag"
level = "read"
endpoints = ["query", "update"]
[[users.grants]]
dataset = "rag"
level = "write"
graphs = ["urn:g:a"]
endpoints = ["info"]

[[users]]
name = "tnarrow"
password = "{tnarrow}"
[[users.grants]]
dataset = "rag"
level = "read"
graphs = ["urn:g:a", "{REPORTS}"]
lifts = ["own"]
"#,
        pattern = if pattern {
            r#"pattern = "?s <http://ex/owner> ?user""#
        } else {
            ""
        },
        tadmin = h("tadmin-pw"),
        tua = h("tua-pw"),
        tub = h("tub-pw"),
        tlift = h("tlift-pw"),
        tpart = h("tpart-pw"),
        tnarrow = h("tnarrow-pw"),
        tzero = h("tzero-pw"),
        twrite = h("twrite-pw"),
        tmixed = h("tmixed-pw"),
    )
}

fn all_graphs() -> String {
    format!("\"urn:g:*\", \"{REPORTS}\"")
}

/// A server whose reports graph has the footprint of a build that read graphs a, b and
/// c as tua. Commit 1 loads the data without a footprint, and commit 2 adds the
/// footprint and the report's link to it.
fn server() -> AuthServer {
    server_with(true)
}

fn server_with(pattern: bool) -> AuthServer {
    let s = build(Fixture {
        extra: users(&all_graphs(), pattern),
        ..Default::default()
    });
    let ds = s.state.attach("rag", DbType::Persistent, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let p = s.auth().user_principal("tua").unwrap();
    let view = p.view("rag", Endpoint::Query);
    let graphs: Vec<String> = ["urn:g:a", "urn:g:b", "urn:g:c"]
        .iter()
        .map(|g| g.to_string())
        .collect();
    let footprint = capture(&CaptureInput {
        dataset: "rag",
        branch: "main",
        source_commit: 1,
        principal: "tua",
        endpoint: "query",
        view: view.as_deref(),
        caller: &p.caller(),
        graphs: &graphs,
        reports: &[REPORTS.to_string()],
    })
    .unwrap();
    let mut trig = format!(
        "<{REPORTS}> {{ <http://ex/r> <{}> <{}> . }}\n",
        gr("disclosure"),
        footprint.iri()
    );
    for q in footprint.quads(REPORTS) {
        trig.push_str(&format!(
            "{} {{ {} {} {} . }}\n",
            q.graph_name, q.subject, q.predicate, q.object
        ));
    }
    ds.store
        .load(&[Source::from_bytes(
            trig.into_bytes(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    s
}

/// The footprint IRI the fixture's report links to.
async fn footprint_iri(s: &AuthServer) -> String {
    let text = format!(
        "SELECT ?f {{ GRAPH <{REPORTS}> {{ <http://ex/r> <{}> ?f }} }}",
        gr("disclosure")
    );
    column(&s.app, "tadmin", &q(&text)).await[0].clone()
}

fn enc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn get(app: &Router, user: &str, uri: &str, accept: &str) -> R {
    call(
        app,
        "GET",
        uri,
        &[("authorization", &b(user)), ("accept", accept)],
        "",
    )
    .await
}

/// The values of a SELECT's first column, sorted.
async fn column(app: &Router, user: &str, uri: &str) -> Vec<String> {
    let r = get(app, user, uri, "text/csv").await;
    assert_eq!(r.status, StatusCode::OK, "{user} {uri}: {}", r.text());
    let mut v: Vec<String> = r.text().lines().skip(1).map(str::to_string).collect();
    v.sort();
    v
}

fn q(query: &str) -> String {
    format!("/rag/sparql?query={}", enc(query))
}

const SEES: [&str; 3] = ["tadmin", "tua", "tlift"];
const BLIND: [&str; 2] = ["tub", "tpart"];

#[tokio::test]
async fn sparql_reads_follow_coverage() {
    let s = server();
    let in_reports = format!("SELECT ?o {{ GRAPH <{REPORTS}> {{ ?s ?p ?o }} }}");
    let listed = "SELECT DISTINCT ?g { GRAPH ?g { ?s ?p ?o } }";
    let count = "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } }";
    let fp = footprint_iri(&s).await;
    for u in SEES {
        assert_eq!(
            column(&s.app, u, &q(&in_reports)).await,
            ["http://ex/x", "the report", fp.as_str()],
            "{u}"
        );
        let graphs = column(&s.app, u, &q(listed)).await;
        assert!(graphs.contains(&REPORTS.to_string()), "{u}");
        // only unrestricted readers see the footprints
        assert_eq!(
            graphs.contains(&DISCLOSURE_GRAPH.to_string()),
            u == "tadmin",
            "{u}"
        );
    }
    for u in BLIND {
        assert!(column(&s.app, u, &q(&in_reports)).await.is_empty(), "{u}");
        assert!(
            !column(&s.app, u, &q(listed))
                .await
                .contains(&REPORTS.to_string()),
            "{u}"
        );
    }
    // counts leave the hidden graphs out: tadmin also counts the two footprint
    // triples, tub misses the reports and the secret triple of x, which tua owns
    let n = |v: Vec<String>| v[0].parse::<u64>().unwrap();
    assert_eq!(n(column(&s.app, "tadmin", &q(count)).await), 10);
    assert_eq!(n(column(&s.app, "tua", &q(count)).await), 8);
    assert_eq!(n(column(&s.app, "tub", &q(count)).await), 4);
    assert_eq!(n(column(&s.app, "tlift", &q(count)).await), 8);
    // a past state is decided against its own footprints: commit 1 has none
    let at = |c: u32| format!("/rag/sparql?at={c}&query={}", enc(&in_reports));
    assert!(column(&s.app, "tua", &at(1)).await.is_empty());
    assert_eq!(column(&s.app, "tadmin", &at(1)).await.len(), 2);
    assert_eq!(column(&s.app, "tua", &at(2)).await.len(), 3);
    assert!(column(&s.app, "tub", &at(2)).await.is_empty());
}

#[tokio::test]
async fn graph_store_and_schema_follow_coverage() {
    let s = server();
    let one = format!("/rag/data?graph={REPORTS_ENC}");
    for u in SEES {
        let r = get(&s.app, u, &one, "application/n-triples").await;
        assert_eq!(r.status, StatusCode::OK, "{u}: {}", r.text());
        assert!(r.text().contains("the report"), "{u}");
        let all = get(&s.app, u, "/rag/data", "application/trig").await;
        assert!(all.text().contains("the report"), "{u}");
        assert_eq!(all.text().contains(DISCLOSURE_GRAPH), u == "tadmin", "{u}");
    }
    for u in BLIND {
        let r = get(&s.app, u, &one, "application/n-triples").await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{u}: {}", r.text());
        let all = get(&s.app, u, "/rag/data", "application/trig").await;
        assert_eq!(all.status, StatusCode::OK, "{u}");
        let text = all.text();
        assert!(
            !text.contains("the report") && !text.contains(REPORTS),
            "{u}: {text}"
        );
    }
    // the schema report counts the visible graphs only
    let triples = |r: R| r.json()["totals"]["triples"].as_u64().unwrap();
    let schema = "/$/schema/rag?graph=union";
    assert_eq!(
        triples(get(&s.app, "tlift", schema, "application/json").await),
        8
    );
    assert_eq!(
        triples(get(&s.app, "tub", schema, "application/json").await),
        4
    );
    let r = get(
        &s.app,
        "tub",
        &format!("/$/schema/rag?graph={REPORTS_ENC}"),
        "application/json",
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// The change feed shows changes between commits, which no single snapshot's
/// footprints cover, so it never shows a restricted reader a guarded graph (§6.5).
#[tokio::test]
async fn the_change_feed_hides_guarded_graphs_from_restricted_readers() {
    let s = server_with(false);
    let up = format!(
        "INSERT DATA {{ GRAPH <{REPORTS}> {{ <http://ex/r2> <http://ex/summary> \"later\" }} }}"
    );
    assert_eq!(
        update_as(&s.app, "rag", &b("tadmin"), &up).await.status,
        StatusCode::OK
    );
    for (u, sees) in [
        ("tadmin", true),
        ("tua", false),
        ("tlift", false),
        ("tnarrow", false),
    ] {
        let r = get(&s.app, u, "/rag/changes?after=2", "application/json").await;
        assert_eq!(r.status, StatusCode::OK, "{u}: {}", r.text());
        let f = r.json();
        assert_eq!(f["commits"][0]["added"], u64::from(sees), "{u}: {f}");
        assert_eq!(
            f["commits"][0]["changes"].to_string().contains("later"),
            sees,
            "{u}"
        );
    }
    // the reports are read at a commit instead
    let q = format!("ASK {{ GRAPH <{REPORTS}> {{ <http://ex/r2> ?p ?o }} }}");
    let r = get(
        &s.app,
        "tlift",
        &self::q(&q),
        "application/sparql-results+json",
    )
    .await;
    assert_eq!(r.json()["boolean"], true);
}

/// A15: the principal loses a graph its build read after the build. Coverage follows the
/// footprint, so tua now lacks graph b and loses the reports graph, while tlift keeps it.
#[tokio::test]
async fn a_revoked_grant_hides_the_reports_graph() {
    let s = server();
    let in_reports = q(&format!("ASK {{ GRAPH <{REPORTS}> {{ ?s ?p ?o }} }}"));
    let ask = |r: R| r.json()["boolean"].as_bool().unwrap();
    let json = "application/sparql-results+json";
    assert!(ask(get(&s.app, "tua", &in_reports, json).await));
    let f = Fixture {
        extra: users(&format!("\"urn:g:a\", \"urn:g:c\", \"{REPORTS}\""), true),
        ..Default::default()
    };
    s.write_config(&f);
    s.auth().reload().unwrap();
    assert!(!ask(get(&s.app, "tua", &in_reports, json).await));
    assert!(ask(get(&s.app, "tlift", &in_reports, json).await));
}

/// A change feed stream opened before a policy reload ends with `access-changed` before
/// it reads another commit (§6.3).
#[tokio::test]
async fn streams_end_when_access_changes() {
    use futures_util::StreamExt;
    use tower::ServiceExt;
    async fn open(s: &AuthServer) -> axum::body::BodyDataStream {
        let res = s
            .app
            .clone()
            .oneshot(
                axum::extract::Request::get("/rag/changes?after=2")
                    .header("accept", "text/event-stream")
                    .header("authorization", b("tlift"))
                    .extension(ConnectInfo(default_peer()))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        res.into_body().into_data_stream()
    }
    /// The next event's type and data.
    async fn next(body: &mut axum::body::BodyDataStream, buf: &mut String) -> (String, String) {
        loop {
            if let Some(end) = buf.find("\n\n") {
                let ev: String = buf.drain(..end + 2).collect();
                let field = |k: &str| {
                    ev.lines()
                        .filter_map(|l| l.strip_prefix(k))
                        .map(|v| v.strip_prefix(' ').unwrap_or(v))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                if !field("event:").is_empty() {
                    return (field("event:"), field("data:"));
                }
                continue;
            }
            let chunk = tokio::time::timeout(Duration::from_secs(10), body.next())
                .await
                .expect("an event within 10 s")
                .expect("the stream is open")
                .unwrap();
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
    let s = server_with(false);
    let up = |n: u32| {
        format!("INSERT DATA {{ GRAPH <urn:g:a> {{ <http://ex/n{n}> <http://ex/p> {n} }} }}")
    };
    // a policy reload
    let mut body = open(&s).await;
    let mut buf = String::new();
    update_as(&s.app, "rag", &b("tadmin"), &up(1)).await;
    assert_eq!(next(&mut body, &mut buf).await.0, "commit");
    s.write_config(&s.fixture);
    s.auth().reload().unwrap();
    update_as(&s.app, "rag", &b("tadmin"), &up(2)).await;
    let (event, data) = next(&mut body, &mut buf).await;
    assert_eq!(event, "error", "{data}");
    assert!(data.contains("access-changed"), "{data}");
}

/// A long poll that waited across a policy reload answers `access-changed` instead of
/// reading under the old view (§6.3).
#[tokio::test]
async fn long_polls_end_when_access_changes() {
    let s = server_with(false);
    let app = s.app.clone();
    let poll = tokio::spawn(async move {
        get(
            &app,
            "tlift",
            "/rag/changes?after=2&wait=10",
            "application/json",
        )
        .await
    });
    // let the poll start waiting
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.write_config(&s.fixture);
    s.auth().reload().unwrap();
    let up = "INSERT DATA { GRAPH <urn:g:a> { <http://ex/n> <http://ex/p> 1 } }";
    update_as(&s.app, "rag", &b("tadmin"), up).await;
    let r = tokio::time::timeout(Duration::from_secs(10), poll)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.err()["code"], "access-changed");
}

/// The GraphRAG routes decide with the same guard: a caller for whom the reports graph
/// is not proven learns nothing derived from it (§6.4).
#[cfg(feature = "mcp")]
#[tokio::test]
async fn graphrag_routes_follow_coverage() {
    let s = server();
    for u in SEES {
        let r = get(&s.app, u, "/$/graphrag/rag", "application/json").await;
        assert_eq!(r.status, StatusCode::OK, "{u}: {}", r.text());
        let v: serde_json::Value = serde_json::from_str(&r.text()).unwrap();
        assert_eq!(v["available"], true, "{u}: {v}");
        assert!(v["reports"].is_object(), "{u}: {v}");
        // only a dataset admin sees the budget and the anomalies
        assert_eq!(v["budget"].is_object(), u == "tadmin", "{u}: {v}");
    }
    for u in BLIND {
        let r = get(&s.app, u, "/$/graphrag/rag", "application/json").await;
        let v: serde_json::Value = serde_json::from_str(&r.text()).unwrap();
        assert_eq!(v["available"], false, "{u}: {v}");
        assert!(
            v.get("reports").is_none() && v.get("levels").is_none(),
            "{u}: {v}"
        );
        let r = get(&s.app, u, "/$/graphrag/rag/communities", "application/json").await;
        let v: serde_json::Value = serde_json::from_str(&r.text()).unwrap();
        assert_eq!(v["available"], false, "{u}: {v}");
        let uri = format!("/$/graphrag/rag/community?iri={}", enc("http://ex/r"));
        let r = get(&s.app, u, &uri, "application/json").await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{u}");
        let r = call(
            &s.app,
            "POST",
            "/$/graphrag/rag/review",
            &[
                ("authorization", &b(u)),
                ("content-type", "application/json"),
            ],
            r#"{"report":"http://ex/r","status":"reviewed"}"#,
        )
        .await;
        assert!(
            r.status == StatusCode::NOT_FOUND || r.status == StatusCode::FORBIDDEN,
            "{u}: {}",
            r.status
        );
    }
    // a build needs dataset admin
    let r = call(
        &s.app,
        "POST",
        "/$/graphrag/rag/build",
        &[
            ("authorization", &b("tua")),
            ("content-type", "application/json"),
        ],
        "{}",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

/// A build reads as a principal that may query the dataset: a user without any grant
/// has no view, which must not read as unrestricted (round 6, finding 2).
#[cfg(feature = "mcp")]
#[tokio::test]
async fn a_principal_without_a_grant_cannot_build() {
    let s = server();
    let ds = s.state.datasets().get("rag").cloned().unwrap();
    let e = crate::graphrag::build::authorized_principal(&s.state, &ds, "tzero").unwrap_err();
    assert!(e.contains("may not query"), "{e}");
    assert!(crate::graphrag::build::authorized_principal(&s.state, &ds, "tua").is_ok());
}

/// Only an unrestricted writer reviews a report, so a restricted writer can neither
/// write past its grants nor copy a report it sees in part (round 6, findings 3 and 8).
#[cfg(feature = "mcp")]
#[tokio::test]
async fn restricted_writers_cannot_review() {
    let s = server_with(false);
    let r = call(
        &s.app,
        "POST",
        "/$/graphrag/rag/review",
        &[
            ("authorization", &b("twrite")),
            ("content-type", "application/json"),
        ],
        r#"{"report":"http://ex/r","status":"reviewed"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(r.text().contains("restricted-writer"), "{}", r.text());
    // a read-only update grant is unrestricted too, and a write grant through another
    // endpoint does not make it a writer (round 7)
    let r = call(
        &s.app,
        "POST",
        "/$/graphrag/rag/review",
        &[
            ("authorization", &b("tmixed")),
            ("content-type", "application/json"),
        ],
        r#"{"report":"http://ex/r","status":"reviewed"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(r.text().contains("restricted-writer"), "{}", r.text());
}

/// A community and its report in the reports graph, linked to the build's footprint,
/// with GraphRAG enabled for the dataset (C22 Phase 2).
#[cfg(feature = "mcp")]
async fn with_community(s: &AuthServer) {
    let fp = footprint_iri(s).await;
    let ds = s.state.datasets().get("rag").cloned().unwrap();
    let rdfs = "http://www.w3.org/2000/01/rdf-schema#label";
    let trig = format!(
        "<{REPORTS}> {{ <urn:c:1> a <{community}> ; <{level}> 0 ; <{count}> 2 ; <{member}> <http://ex/x> ; <{report}> <http://ex/r> ; <{disc}> <{fp}> . <http://ex/r> a <{rep}> ; <{status}> \"unreviewed\" ; <{rdfs}> \"Team\" ; <{summary}> \"the report\" ; <{finding}> <http://ex/r#f1> . <http://ex/r#f1> <{order}> 1 ; <{text}> \"x knows y\" . }}\n",
        community = gr("Community"),
        level = gr("level"),
        count = gr("memberCount"),
        member = gr("member"),
        report = gr("report"),
        disc = gr("disclosure"),
        rep = gr("Report"),
        status = gr("status"),
        summary = gr("summary"),
        finding = gr("finding"),
        order = gr("order"),
        text = gr("text"),
    );
    ds.store
        .load(&[Source::from_bytes(
            trig.into_bytes(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    crate::settings::store_runtime(
        &s.state,
        &ds,
        &crate::settings::GRAPHRAG,
        &serde_json::json!({ "enabled": true, "principal": "tua", "projection": { "graphs": ["urn:g:*"] } }),
    )
    .unwrap();
}

/// `user`'s principal as a request has it, with the server's disclosure guard.
#[cfg(feature = "mcp")]
fn guarded(s: &AuthServer, user: &str) -> crate::auth::Principal {
    s.auth()
        .user_principal(user)
        .unwrap()
        .with_guard(s.state.graphrag_guard.clone(), s.state.access_epoch())
}

/// A tool call as `user`.
#[cfg(feature = "mcp")]
fn tool_as(s: &AuthServer, user: &str, name: &str, args: serde_json::Value) -> serde_json::Value {
    let call = crate::mcp::Call {
        arrived: std::time::Instant::now(),
        cancel: Default::default(),
        request_id: "t".into(),
        principal: guarded(s, user),
        headers: None,
        held: None,
        grant: None,
    };
    let serde_json::Value::Object(mut m) = args else {
        panic!("arguments are an object")
    };
    m.insert("dataset".into(), "rag".into());
    let server = crate::mcp::McpServer::new(s.state.clone(), crate::mcp::McpConfig::default());
    match server.run_now(name, m, &call).unwrap() {
        crate::mcp::Outcome::Structured(v) => v,
        crate::mcp::Outcome::Text(t) => {
            serde_json::from_str(&t).unwrap_or(serde_json::Value::String(t))
        }
    }
}

/// `community_reports` and the communities of `recall` and the brief read through the
/// guard: a caller it does not prove learns only that the reports are unavailable.
#[cfg(feature = "mcp")]
#[tokio::test]
async fn report_consumers_follow_coverage() {
    let s = server();
    with_community(&s).await;
    let s = std::sync::Arc::new(s);
    let s2 = s.clone();
    tokio::task::spawn_blocking(move || {
        let s = s2;
        for u in SEES {
            let v = tool_as(&s, u, "community_reports", serde_json::json!({}));
            assert_eq!(v["available"], true, "{u}: {v}");
            assert_eq!(v["reports"][0]["title"], "Team", "{u}: {v}");
            assert_eq!(v["reports"][0]["findings"][0]["text"], "x knows y", "{u}: {v}");
            let r = tool_as(
                &s,
                u,
                "recall",
                serde_json::json!({ "seeds": ["http://ex/x"], "hops": 0, "communities": true, "format": "json" }),
            );
            assert_eq!(r["communities"]["reports"][0]["title"], "Team", "{u}: {r}");
        }
        for u in BLIND {
            let v = tool_as(&s, u, "community_reports", serde_json::json!({}));
            assert_eq!(v["available"], false, "{u}: {v}");
            assert_eq!(v["reason"], "unavailable", "{u}: {v}");
            assert_eq!(v["reports"], serde_json::json!([]), "{u}: {v}");
            assert!(!v.to_string().contains("Team"), "{u}: {v}");
            let r = tool_as(
                &s,
                u,
                "recall",
                serde_json::json!({ "seeds": ["http://ex/x"], "hops": 0, "communities": true, "format": "json" }),
            );
            assert_eq!(r["communities"]["unavailable"], "access", "{u}: {r}");
            assert!(!r.to_string().contains("the report"), "{u}: {r}");
        }
    })
    .await
    .unwrap();
}

/// A5: a caller the guard does not prove gets a local answer and a note that the
/// summaries are unavailable to it, even when it asks for a global answer.
#[cfg(feature = "mcp")]
#[tokio::test(flavor = "multi_thread")]
async fn a5_unproven_callers_answer_locally() {
    use crate::models::mock::{self, MockModel};
    let s = server();
    with_community(&s).await;
    let mock = MockModel::start(|r, _| {
        let sys = r.body["messages"][0]["content"].as_str().unwrap_or("");
        let text = if sys.contains("translate a person's question") {
            let mut d = serde_json::json!({
                "query": "SELECT ?s WHERE { ?s ?p ?o } LIMIT 1",
                "explanation": "Lists a subject.",
                "assumptions": [],
                "clarify": { "question": "", "choices": [] },
                "graph": { "subject": "", "predicate": "", "object": "" }
            });
            if r.body.to_string().contains("ScopedDraft") {
                d["scope"] = "global".into();
            }
            d.to_string()
        } else if sys.contains("community reports about one dataset") {
            serde_json::json!({ "points": [{ "text": "A team.", "score": 50, "reports": ["R1"], "findings": ["F1.1"] }] }).to_string()
        } else {
            serde_json::json!({ "text": "One subject [1].", "citations": [1] }).to_string()
        };
        (200, mock::openai(&text))
    });
    let pair = serde_json::json!({ "provider": "p", "model": "m" });
    let cfg = serde_json::json!({
        "providers": { "p": { "kind": "openai", "endpoint": mock.url(), "requestTimeoutSecs": 5,
                              "allowedModels": ["m"], "structuredOutput": "json-schema" } },
        "roles": { "draft": [pair], "summarize": [pair] }
    });
    s.state
        .models
        .store(Some(std::sync::Arc::new(crate::models::Models::new(
            crate::models::ModelsConfig::parse(&cfg.to_string()).unwrap(),
            Default::default(),
            sparkles::outbound::OutboundPolicy {
                allow_private: true,
                ..Default::default()
            },
        ))));
    let s = std::sync::Arc::new(s);
    let s2 = s.clone();
    tokio::task::spawn_blocking(move || {
        let s = s2;
        let models = s.state.models().unwrap();
        let server = crate::mcp::McpServer::new(s.state.clone(), crate::mcp::McpConfig::default());
        for (u, mode) in [
            ("tpart", crate::ask::AskMode::Global),
            ("tub", crate::ask::AskMode::Auto),
        ] {
            let o = crate::ask::AskOptions {
                dataset: "rag".into(),
                question: "What are the main groups?".into(),
                mode,
                ..Default::default()
            };
            let p = guarded(&s, u);
            let mut on = |_: &str, _: &serde_json::Value| {};
            let out = crate::ask::ask(&server, &models, &p, &o, &mut on);
            assert_eq!(out["mode"], "local", "{u}: {out}");
            assert!(
                out["notes"]
                    .to_string()
                    .contains("summaries are unavailable to you"),
                "{u}: {out}"
            );
            assert!(!out.to_string().contains("the report"), "{u}: {out}");
        }
        // a proven caller gets a global answer from the same question
        let o = crate::ask::AskOptions {
            dataset: "rag".into(),
            question: "What are the main groups?".into(),
            mode: crate::ask::AskMode::Global,
            ..Default::default()
        };
        let p = guarded(&s, "tua");
        let mut on = |_: &str, _: &serde_json::Value| {};
        let out = crate::ask::ask(&server, &models, &p, &o, &mut on);
        assert_eq!(out["mode"], "global", "{out}");
    })
    .await
    .unwrap();
}

/// C22 §6.6 in every build: a history entry lists the reports of a global answer only
/// while the guard proves their graph for the reader, so a history written by a build
/// with global answers stays filtered by one without them (round 4, finding 1).
#[tokio::test]
async fn history_lists_reports_only_while_proven() {
    let s = server();
    let ds = s.state.datasets().get("rag").cloned().unwrap();
    let at = chrono::Utc::now().to_rfc3339();
    let users: Vec<&str> = SEES.iter().chain(BLIND.iter()).copied().collect();
    let records: Vec<crate::assistant::AskRecord> = users
        .iter()
        .map(|u| {
            serde_json::from_value(serde_json::json!({
                "id": format!("ask-{u}"),
                "principal": s.auth().user_principal(u).unwrap().id(),
                "at": at,
                "question": "What are the main groups?",
                "result": "answered",
                "outcome": "none",
                "routing": {},
                "mode": "global",
                "reports": ["http://ex/r"],
                "reportsGraph": REPORTS,
            }))
            .unwrap()
        })
        .collect();
    crate::assistant::store_history(&s.state, &ds, &records).unwrap();
    for u in SEES {
        let r = get(&s.app, u, "/$/asks/rag", "application/json").await;
        assert_eq!(r.status, StatusCode::OK, "{u}");
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(
            v["asks"][0]["reports"],
            serde_json::json!(["http://ex/r"]),
            "{u}: {v}"
        );
    }
    for u in BLIND {
        let r = get(&s.app, u, "/$/asks/rag", "application/json").await;
        assert_eq!(r.status, StatusCode::OK, "{u}");
        let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["asks"].as_array().map(Vec::len), Some(1), "{u}: {v}");
        assert!(v["asks"][0].get("reports").is_none(), "{u}: {v}");
        assert!(v["asks"][0].get("reportsGraph").is_none(), "{u}: {v}");
    }
}

/// Communities in the reports graph of [`server`], in the form a build writes them.
const COMMUNITIES: &str = r#"@prefix ex: <http://ex/> .
@prefix gr: <urn:x-sparkles:graphrag#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:x-sparkles:graphrag:reports> {
  <urn:c:one> a gr:Community ; gr:level 0 ; gr:memberCount 2 ; gr:member ex:x, ex:y ;
    gr:report <urn:r:one> .
  <urn:r:one> rdfs:label "Secret team" ; gr:status "unreviewed" .
  <urn:c:two> a gr:Community ; gr:level 0 ; gr:memberCount 1 ; gr:member ex:z ;
    gr:report <urn:r:two> .
  <urn:r:two> rdfs:label "Other team" ; gr:status "reviewed" .
}
"#;

fn with_communities() -> AuthServer {
    let s = server();
    let ds = s.state.datasets().get("rag").cloned().unwrap();
    ds.store
        .load(&[Source::from_bytes(
            COMMUNITIES.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    s
}

async fn groups(s: &AuthServer, user: &str, uri: &str, body: serde_json::Value) -> R {
    call(
        &s.app,
        "POST",
        uri,
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await
}

/// U06 with C22 §6.4: the overview's GraphRAG grouping reads the reports graph under the
/// guard that the GraphRAG routes use. An unrestricted reader and a proven restricted
/// reader see the communities with their titles. An unproven reader gets an answer that
/// says they are unavailable and holds no community, report, title, status or member,
/// also for the group ids it took from another caller's answer, which the cache holds.
/// The test needs only `auth`, so it also runs in a build without `mcp`, where the
/// GraphRAG routes do not exist (C22 review round 4 found report identities leaking in
/// such a build).
#[tokio::test]
async fn the_overview_reads_communities_only_where_proven() {
    let s = with_communities();
    let uri = "/$/graph/rag/groups";
    let top = serde_json::json!({ "by": { "reports": {} } });
    let mut ids = Vec::new();
    for u in SEES {
        let r = groups(&s, u, uri, top.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{u}: {}", r.text());
        let v = r.json();
        assert_eq!(v["available"], true, "{u}: {v}");
        let labels: Vec<&str> = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Secret team", "Other team"], "{u}");
        assert_eq!(v["groups"][0]["report"]["status"], "unreviewed");
        assert_eq!(v["groups"][0]["size"], 2);
        // y knows z from the first team to the second, and z knows x back
        let between: Vec<u64> = v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["source"] != e["target"])
            .map(|e| e["count"].as_u64().unwrap())
            .collect();
        assert_eq!(between, [1, 1], "{u}: {v}");
        for g in v["groups"].as_array().unwrap() {
            ids.push(g["id"].as_str().unwrap().to_string());
        }
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2);
    // a proven reader opens a team into its members
    for id in &ids {
        let r = groups(
            &s,
            "tua",
            uri,
            serde_json::json!({ "within": { "group": id } }),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["grouping"]["kind"], "members");
    }
    let mut bodies = vec![top.clone()];
    for id in &ids {
        bodies.push(serde_json::json!({ "within": { "group": id } }));
        bodies.push(serde_json::json!({ "within": { "group": id }, "by": "members" }));
    }
    for u in BLIND {
        for body in &bodies {
            let r = groups(&s, u, uri, body.clone()).await;
            assert_eq!(r.status, StatusCode::OK, "{u} {body}: {}", r.text());
            let v = r.json();
            assert_eq!(v["available"], false, "{u}: {v}");
            assert_eq!(v["unavailable"], "access", "{u}: {v}");
            let text = r.text();
            for secret in [
                "Secret team",
                "Other team",
                "urn:c:",
                "urn:r:",
                "unreviewed",
                "http://ex/",
            ] {
                assert!(!text.contains(secret), "{u} {body} leaks {secret}: {text}");
            }
        }
    }
}

/// The reports belong to main, so a branch refuses the grouping and keeps the others.
#[tokio::test]
async fn the_overview_reads_communities_on_main_only() {
    let s = with_communities();
    let r = call(
        &s.app,
        "POST",
        "/$/branches/rag",
        &[
            ("authorization", &b("tadmin")),
            ("content-type", "application/json"),
        ],
        r#"{"name":"dev"}"#,
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let uri = "/$/graph/rag/groups?branch=dev";
    let r = groups(
        &s,
        "tadmin",
        uri,
        serde_json::json!({ "by": { "reports": {} } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("read on main"), "{}", r.text());
    assert!(!r.text().contains("Secret team"));
}

/// The reports graph the overview reads by default is the one the dataset's GraphRAG
/// settings name, and the library's constants agree with the server's.
#[test]
fn the_overview_uses_the_guarded_namespace_of_the_server() {
    use crate::graphrag::settings::{GUARDED_PREFIX, GraphragSettings, MAX_LEVELS};
    assert_eq!(sparkles::overview::GUARDED_PREFIX, GUARDED_PREFIX);
    assert_eq!(sparkles::overview::DISCLOSURE_GRAPH, DISCLOSURE_GRAPH);
    assert_eq!(
        sparkles::overview::DEFAULT_REPORTS_GRAPH,
        GraphragSettings::default().reports_graph
    );
    assert!(sparkles::overview::MAX_REPORT_LEVEL >= MAX_LEVELS);
}
