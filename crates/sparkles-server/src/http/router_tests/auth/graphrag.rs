//! The disclosure guard of GraphRAG reports graphs (spec C22 §6.6, A15, A16) on every
//! read path: SPARQL, the graph list, counts, time travel, the Graph Store, the schema
//! report and the change feed.

use super::*;
use crate::auth::Endpoint;
use crate::graphrag::guard::{CaptureInput, Entry, capture};

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
    )
}

fn all_graphs() -> String {
    format!("\"urn:g:*\", \"{REPORTS}\"")
}

/// A server whose reports graph has the footprint of a build that read graphs a, b and
/// c as tua.
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
    s.state
        .graphrag_guard
        .install(
            "rag",
            vec![Entry {
                graph: REPORTS.into(),
                footprint,
            }],
        )
        .unwrap();
    s
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
    for u in SEES {
        assert_eq!(
            column(&s.app, u, &q(&in_reports)).await,
            ["http://ex/x", "the report"],
            "{u}"
        );
        assert!(
            column(&s.app, u, &q(listed))
                .await
                .contains(&REPORTS.to_string()),
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
    // counts leave the hidden graph out: tub misses the reports, tua also misses the
    // secret triple of the protection, which tua owns
    let n = |v: Vec<String>| v[0].parse::<u64>().unwrap();
    assert_eq!(n(column(&s.app, "tadmin", &q(count)).await), 7);
    assert_eq!(n(column(&s.app, "tua", &q(count)).await), 7);
    assert_eq!(n(column(&s.app, "tub", &q(count)).await), 4);
    assert_eq!(n(column(&s.app, "tlift", &q(count)).await), 7);
    // a past state is read under the same guard
    let at = format!("/rag/sparql?at=1&query={}", enc(&in_reports));
    assert_eq!(column(&s.app, "tua", &at).await.len(), 2);
    assert!(column(&s.app, "tub", &at).await.is_empty());
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
        7
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

#[tokio::test]
async fn the_change_feed_follows_coverage() {
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
        ("tua", true),
        ("tub", true),
        ("tlift", true),
        ("tpart", false),
        ("tnarrow", false),
    ] {
        let r = get(&s.app, u, "/rag/changes?after=1", "application/json").await;
        assert_eq!(r.status, StatusCode::OK, "{u}: {}", r.text());
        let f = r.json();
        assert_eq!(f["commits"][0]["added"], u64::from(sees), "{u}: {f}");
        assert_eq!(
            f["commits"][0]["changes"].to_string().contains("later"),
            sees,
            "{u}"
        );
    }
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

/// A change feed stream opened before a policy reload or a registry install ends with
/// `access-changed` before it reads another commit (§6.3).
#[tokio::test]
async fn streams_end_when_access_changes() {
    use futures_util::StreamExt;
    use tower::ServiceExt;
    async fn open(s: &AuthServer) -> axum::body::BodyDataStream {
        let res = s
            .app
            .clone()
            .oneshot(
                axum::extract::Request::get("/rag/changes?after=1")
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
    // a registry install
    let mut body = open(&s).await;
    let mut buf = String::new();
    assert_eq!(next(&mut body, &mut buf).await.0, "commit");
    let root = s
        .state
        .datasets()
        .get("rag")
        .unwrap()
        .store
        .root()
        .unwrap()
        .to_path_buf();
    let entry = crate::graphrag::guard::Registry::load(&root)
        .unwrap()
        .entries[0]
        .clone();
    let mut other = entry.clone();
    other.footprint.source_commit += 1;
    s.state.graphrag_guard.install("rag", vec![other]).unwrap();
    update_as(&s.app, "rag", &b("tadmin"), &up(3)).await;
    loop {
        let (event, data) = next(&mut body, &mut buf).await;
        if event == "error" {
            assert!(data.contains("access-changed"), "{data}");
            break;
        }
        assert_eq!(event, "commit");
    }
}
