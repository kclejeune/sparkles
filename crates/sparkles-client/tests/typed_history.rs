//! Fixed history response and wire fixtures, independent of a server executable.
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::response::Response;
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use sparkles_client::admin_types::{Diff, DiffGroupObject, NamedSnapshot};
use sparkles_client::{
    At, CancellationToken, Client, DiffOptions, Error, Graph, HistoryOptions, Method,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone)]
enum Reply {
    Json(Value),
    Status(u16),
    Malformed,
    Stalled,
}

#[derive(Debug)]
struct Seen {
    method: Method,
    uri: String,
    query: Vec<(String, String)>,
    accept: String,
    extra_header: Option<String>,
    body: Vec<u8>,
}

#[derive(Clone)]
struct Mock {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    started_body: Arc<Notify>,
}

async fn handle(State(mock): State<Mock>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 64 * 1024).await.unwrap();
    let url = reqwest::Url::parse(&format!("http://fixture{}", parts.uri)).unwrap();
    mock.seen.lock().unwrap().push(Seen {
        method: parts.method,
        uri: parts.uri.to_string(),
        query: url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
        accept: parts
            .headers
            .get("accept")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned(),
        extra_header: parts
            .headers
            .get("x-history-test")
            .map(|h| h.to_str().unwrap().to_owned()),
        body: body.to_vec(),
    });
    let reply = mock
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected HTTP request");
    let (status, body) = match reply {
        Reply::Json(value) => (200, Body::from(value.to_string())),
        Reply::Status(status) => (status, Body::from(json!({"error":"fixed refusal","code":"history-unavailable","detail":"retained origin","requestId":"history-fixture"}).to_string())),
        Reply::Malformed => (200, Body::from("{unfinished")),
        Reply::Stalled => {
            let started = mock.started_body.clone();
            let first = stream::once(async move {
                started.notify_one();
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"{"))
            });
            (200, Body::from_stream(first.chain(stream::pending::<Result<Bytes, std::io::Error>>())))
        }
    };
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

async fn start(replies: Vec<Reply>) -> (Client, Mock, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = Mock {
        replies: Arc::new(Mutex::new(replies.into())),
        seen: Default::default(),
        started_body: Arc::new(Notify::new()),
    };
    let app = Router::new().fallback(handle).with_state(mock.clone());
    let base = format!("http://{}/prefix/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Client::new(base).unwrap(), mock, task)
}

fn commit() -> Value {
    json!({"seq":u64::MAX,"parent":null,"ref":"commit:18446744073709551615","kind":"future-commit-kind","generation":"g","bulk":false,"future":true})
}

fn snapshot() -> Value {
    json!({"name":"saved/é%","ref":"snapshot:saved/é%","seq":u64::MAX,"commit":commit(),"created":"now","expires":null,"note":null,"reconstructable":true,"warm":false,"future":{"kept":true}})
}

fn snapshots() -> Value {
    json!({"dataset":"my ds","datasetId":"uuid","head":u64::MAX,"snapshots":[snapshot(),snapshot()],"future":1})
}

fn branch() -> Value {
    json!({"name":"topic/one","id":"branch-uuid","head":null,"protected":false,"storage":{"generation":"g","ownBytes":0,"heldBytes":u64::MAX,"linked":true,"future":"kept"},"from":null,"future":true})
}

fn branches() -> Value {
    json!({"dataset":"my ds","datasetId":"uuid","branches":[branch()],"exemptPredicates":["urn:exempt"],"future":true})
}

fn diff() -> Value {
    json!({"dataset":"my ds","datasetId":"uuid","from":{"selector":"snapshot:saved","commit":commit()},"to":{"selector":"head","commit":commit()},"added":u64::MAX,"removed":i64::MIN,"method":"future-method","group":"class","groupCount":3,"truncated":true,
        "groups":[
            {"subjectClass":null,"predicate":"urn:p","object":{"class":null},"added":1,"removed":0},
            {"subjectClass":"urn:Class","predicate":"urn:p","object":{"literal":"http://www.w3.org/2001/XMLSchema#string"},"added":1,"removed":0},
            {"subjectClass":null,"predicate":"urn:p","object":{"iri":true},"added":1,"removed":0}
        ],"quads":[{"op":"+","subject":"_:b1","predicate":"<urn:p>","object":"\"é\"@fr","graph":null}],"future":[1,2]})
}

#[tokio::test]
async fn typed_history_reads_preserve_paths_models_and_generic_json_access() {
    let (client, mock, task) = start(vec![
        Reply::Json(snapshots()),
        Reply::Json(snapshot()),
        Reply::Json(branches()),
        Reply::Json(branch()),
        Reply::Json(diff()),
        Reply::Json(snapshot()),
        Reply::Json(branches()),
    ])
    .await;
    let ds = client.dataset("my ds");
    let list = ds.snapshots().await.unwrap();
    assert_eq!(list.head, i128::from(u64::MAX));
    assert_eq!(list.snapshots.len(), 2);
    let saved = ds.snapshot("saved/é%").await.unwrap();
    assert_eq!(saved.seq, i128::from(u64::MAX));
    assert_eq!(saved.commit.unwrap().kind, "future-commit-kind");
    assert_eq!(saved.extra["future"], json!({"kept":true}));
    let list = ds.branches().await.unwrap();
    assert_eq!(list.branches[0].head, None);
    assert_eq!(
        list.branches[0].storage.held_bytes,
        Some(i128::from(u64::MAX))
    );
    let b = ds
        .branch_with(
            "topic/one",
            &HistoryOptions::new().header("X-History-Test", "present"),
        )
        .await
        .unwrap();
    assert_eq!(b.storage.extra["future"], "kept");
    let response = ds.diff(&DiffOptions::new()).await.unwrap();
    assert_eq!(response.added, i128::from(u64::MAX));
    assert_eq!(response.removed, i128::from(i64::MIN));
    assert_eq!(response.method, "future-method");
    assert!(matches!(
        &response.groups.as_ref().unwrap()[1].object,
        DiffGroupObject::Alternative2(_)
    ));
    assert!(matches!(
        &response.groups.as_ref().unwrap()[2].object,
        DiffGroupObject::Alternative3(_)
    ));
    let raw: Value = client
        .call_json(
            Method::GET,
            "/$/snapshots/my%20ds/saved%2F%C3%A9%25",
            &[],
            None,
        )
        .await
        .unwrap();
    assert_eq!(raw, snapshot());
    client.dataset("data/é%").branches().await.unwrap();
    let seen = mock.seen.lock().unwrap();
    assert_eq!(seen.len(), 7);
    assert_eq!(seen[0].uri, "/prefix/$/snapshots/my%20ds");
    assert_eq!(seen[1].uri, "/prefix/$/snapshots/my%20ds/saved%2F%C3%A9%25");
    assert_eq!(seen[2].uri, "/prefix/$/branches/my%20ds");
    assert_eq!(seen[3].uri, "/prefix/$/branches/my%20ds/topic%2Fone");
    assert_eq!(seen[3].extra_header.as_deref(), Some("present"));
    assert_eq!(seen[4].uri, "/prefix/my%20ds/diff");
    assert_eq!(seen[6].uri, "/prefix/$/branches/data%2F%C3%A9%25");
    assert!(
        seen.iter().all(|r| r.method == Method::GET
            && r.accept == "application/json"
            && r.body.is_empty())
    );
    task.abort();
}

#[tokio::test]
async fn diff_query_values_keep_repetition_rdf_spelling_and_server_refusals() {
    use sparkles_client::oxrdf::{Literal, NamedNode};
    let (client, mock, task) = start(vec![
        Reply::Json(diff()),
        Reply::Json(diff()),
        Reply::Status(400),
    ])
    .await;
    let ds = client.dataset("my ds");
    let term = Literal::new_language_tagged_literal("é", "fr").unwrap();
    ds.diff(
        &DiffOptions::new()
            .from(At::Snapshot("saved/a".into()))
            .to(At::Commit(u64::MAX))
            .graph(Graph::Default)
            .term(term.clone())
            .term(term)
            .quads(false)
            .group_by_class()
            .limit(42)
            .from_branch("source/a")
            .to_branch("target b"),
    )
    .await
    .unwrap();
    ds.diff(
        &DiffOptions::new()
            .graph(NamedNode::new("urn:graph").unwrap())
            .quads(true),
    )
    .await
    .unwrap();
    let error = ds
        .diff(&DiffOptions::new().quads(true).group_by_class())
        .await
        .unwrap_err();
    assert_eq!(error.status(), Some(400));
    let seen = mock.seen.lock().unwrap();
    assert_eq!(
        seen[0].query,
        [
            ("from", "snapshot:saved/a"),
            ("to", "commit:18446744073709551615"),
            ("graph", "default"),
            ("term", "\"é\"@fr"),
            ("term", "\"é\"@fr"),
            ("quads", "false"),
            ("group", "class"),
            ("limit", "42"),
            ("fromBranch", "source/a"),
            ("toBranch", "target b")
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
    );
    assert_eq!(
        seen[1].query,
        [
            ("graph".to_owned(), "urn:graph".to_owned()),
            ("quads".to_owned(), "true".to_owned())
        ]
    );
    task.abort();
}

#[tokio::test]
async fn malformed_and_retained_or_feature_absence_errors_are_not_empty_results() {
    let mut missing = snapshot();
    missing.as_object_mut().unwrap().remove("expires");
    let mut wrong = snapshots();
    wrong["head"] = json!("not-an-integer");
    let (client, _, task) = start(vec![
        Reply::Status(404),
        Reply::Status(410),
        Reply::Status(501),
        Reply::Malformed,
        Reply::Json(missing),
        Reply::Json(wrong),
    ])
    .await;
    let ds = client.dataset("ds");
    for status in [404, 410, 501] {
        let error = ds.snapshot("saved").await.unwrap_err();
        assert_eq!(error.status(), Some(status));
        assert_eq!(error.code(), Some("history-unavailable"));
        let Error::Status(detail) = error else {
            panic!("status error expected")
        };
        assert_eq!(detail.request_id.as_deref(), Some("history-fixture"));
    }
    assert!(matches!(
        ds.branches().await.unwrap_err(),
        Error::Parse { .. }
    ));
    assert!(matches!(
        ds.snapshot("saved").await.unwrap_err(),
        Error::Parse { .. }
    ));
    assert!(matches!(
        ds.snapshots().await.unwrap_err(),
        Error::Parse { .. }
    ));
    task.abort();
}

#[tokio::test]
async fn unrepresentable_dot_segments_and_union_refuse_before_http() {
    let (client, mock, task) = start(vec![]).await;
    for name in [".", ".."] {
        assert!(matches!(
            client.dataset("ds").snapshot(name).await.unwrap_err(),
            Error::Config(_)
        ));
        assert!(matches!(
            client.dataset(name).branches().await.unwrap_err(),
            Error::Config(_)
        ));
    }
    assert!(matches!(
        client
            .dataset("ds")
            .diff(&DiffOptions::new().graph(Graph::Union))
            .await
            .unwrap_err(),
        Error::Config(_)
    ));
    assert!(mock.seen.lock().unwrap().is_empty());
    task.abort();
}

#[test]
fn required_nullable_union_members_are_present_and_known_alternatives_do_not_collapse() {
    let typed: Diff = serde_json::from_value(diff()).unwrap();
    let groups = typed.groups.unwrap();
    assert!(matches!(
        &groups[0].object,
        DiffGroupObject::Alternative1(_)
    ));
    assert!(matches!(
        &groups[1].object,
        DiffGroupObject::Alternative2(_)
    ));
    assert!(matches!(
        &groups[2].object,
        DiffGroupObject::Alternative3(_)
    ));
    let mut missing = snapshot();
    missing.as_object_mut().unwrap().remove("note");
    assert!(serde_json::from_value::<NamedSnapshot>(missing).is_err());
    assert!(serde_json::from_value::<DiffGroupObject>(json!({"future":true})).is_err());
    let saved: NamedSnapshot = serde_json::from_value(snapshot()).unwrap();
    assert_eq!(saved.note, None);
}

#[tokio::test]
async fn history_controls_cancel_before_send_and_during_a_stalled_json_body() {
    let (client, mock, task) = start(vec![Reply::Stalled, Reply::Stalled]).await;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        client
            .dataset("ds")
            .snapshots_with(&HistoryOptions::new().cancel(cancelled))
            .await
            .unwrap_err(),
        Error::Cancelled
    ));
    assert!(mock.seen.lock().unwrap().is_empty());
    let cancel = CancellationToken::new();
    let options = HistoryOptions::new().cancel(cancel.clone());
    let ds = client.dataset("ds");
    let pending = tokio::spawn(async move { ds.snapshots_with(&options).await });
    tokio::time::timeout(Duration::from_secs(2), mock.started_body.notified())
        .await
        .unwrap();
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        Error::Cancelled
    ));
    let error = client
        .dataset("ds")
        .branches_with(&HistoryOptions::new().deadline(Duration::from_millis(250)))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Deadline(_)));
    task.abort();
}

#[cfg(feature = "blocking")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_history_uses_the_same_wire_contract_and_types() {
    let (client, mock, task) = start(vec![
        Reply::Json(snapshots()),
        Reply::Json(snapshot()),
        Reply::Json(branches()),
        Reply::Json(branch()),
        Reply::Json(diff()),
    ])
    .await;
    tokio::task::spawn_blocking(move || {
        let client = sparkles_client::blocking::Client::from_async(client).unwrap();
        let ds = client.dataset("my ds");
        assert_eq!(ds.snapshots().unwrap().snapshots.len(), 2);
        assert_eq!(ds.snapshot("saved/é%").unwrap().seq, i128::from(u64::MAX));
        assert_eq!(
            ds.branches_with(&HistoryOptions::new())
                .unwrap()
                .branches
                .len(),
            1
        );
        assert_eq!(ds.branch("topic/one").unwrap().name, "topic/one");
        assert_eq!(
            ds.diff(&DiffOptions::new().from(At::Commit(7)))
                .unwrap()
                .added,
            i128::from(u64::MAX)
        );
    })
    .await
    .unwrap();
    assert_eq!(mock.seen.lock().unwrap().len(), 5);
    task.abort();
}
