//! The additive administration API against an HTTP fixture, without server binaries.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::response::Response;
use serde_json::{Value, json};
use sparkles_client::admin_types::{BackupList, BackupSummary, DatasetStats, StoredQuery};
use sparkles_client::{At, Client, Error, Method};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct Seen {
    method: Method,
    uri: String,
    accept: String,
    body: Value,
}

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<Vec<Seen>>>);

fn schema() -> Value {
    json!({
        "schemaFormat": 1, "dataset": "my ds",
        "snapshot": {"version": 7, "generation": "gen", "computedAt": "2026-10-01T00:00:00Z", "new": true},
        "selection": {"graph": "all"}, "totals": {"quads": 3},
        "classes": {"items": [], "next": null, "total": 0},
        "predicates": {"items": [], "next": null, "total": 0},
        "new": [1, 2]
    })
}

fn stored() -> Value {
    json!({
        "query": "ASK {}", "name": "by/name", "kind": "ASK",
        "parameters": {"limit": {"type": "integer", "default": 3}},
        "version": {"version": 1, "created": "now", "digest": "abc", "new": true},
        "new": {"server": true}
    })
}

fn backup() -> Value {
    json!({
        "name": "first", "repository": "local",
        "dataset": {"name": "my ds", "id": "uuid", "type": "future-storage"},
        "commit": {"seq": 7, "ref": "commit:7", "timestamp": "now", "quads": 3},
        "created": "now", "completed": "now", "millis": 1,
        "logicalBytes": 10, "addedBytes": 5,
        "policy": null, "run": null, "note": null, "verified": null,
        "new": "kept"
    })
}

async fn handle(State(mock): State<Mock>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 64 * 1024).await.unwrap();
    let path = parts.uri.path();
    let value = if path == "/$/stats" {
        json!({"datasets": {"/my ds": {"Requests": 2, "RequestsGood": 1, "RequestsBad": 1, "endpoints": {"sparql": {"Requests": 2, "RequestsGood": 1, "RequestsBad": 1, "operation": "query", "description": "SPARQL"}}}}})
    } else if path == "/$/backups-list" {
        json!({"backups": ["first.nq"], "new": true})
    } else if path.starts_with("/$/schema/") {
        schema()
    } else if path == "/$/stats/bad" {
        json!({"name": "bad", "quads": "not an integer"})
    } else if path.starts_with("/$/stats/") {
        json!({"name": "my ds", "quads": 3, "quota": null, "new": true})
    } else if path.starts_with("/$/backups/") {
        json!({"dataset": "my ds", "datasetId": "uuid", "backups": [backup()], "new": true})
    } else if path.ends_with("by%2Fname") {
        stored()
    } else if path.starts_with("/$/queries/") {
        let mut summary = stored();
        summary.as_object_mut().unwrap().remove("query");
        json!({"dataset": "my ds", "queries": [summary]})
    } else {
        panic!("unexpected request: {} {}", parts.method, parts.uri);
    };
    mock.0.lock().unwrap().push(Seen {
        method: parts.method,
        uri: parts.uri.to_string(),
        accept: parts
            .headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
        body: if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    });
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}

async fn start() -> (Client, Mock, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mock = Mock::default();
    let app = Router::new().fallback(handle).with_state(mock.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Client::new(url).unwrap(), mock, task)
}

#[tokio::test]
async fn typed_calls_preserve_the_wire_contract_and_untyped_api() {
    let (client, mock, task) = start().await;
    assert_eq!(
        client.stats_typed().await.unwrap().datasets["/my ds"].requests,
        2
    );
    assert_eq!(
        client.backup_files_typed().await.unwrap().backups,
        ["first.nq"]
    );
    let ds = client.dataset("my ds");
    let stats = ds.stats_typed(Some(At::Commit(7))).await.unwrap();
    assert_eq!(stats.quads, 3);
    assert_eq!(stats.quota, None);
    assert_eq!(stats.extra["new"], true);
    let report = ds.schema_typed(Some(At::Commit(7))).await.unwrap();
    assert_eq!(report.snapshot.version, 7);
    assert_eq!(report.snapshot.extra["new"], true);
    assert_eq!(report.totals["quads"], 3);
    assert_eq!(report.classes.next, None);
    assert_eq!(
        ds.stored_queries_typed().await.unwrap().queries[0].name,
        "by/name"
    );
    let definition = ds.stored_query_typed("by/name").await.unwrap();
    assert_eq!(definition.query, "ASK {}");
    assert_eq!(definition.version.as_ref().unwrap().extra["new"], true);
    assert_eq!(
        ds.put_stored_query_typed("by/name", &definition)
            .await
            .unwrap(),
        definition
    );
    match ds.backups_typed().await.unwrap() {
        BackupList::Alternative2(list) => {
            assert_eq!(list.dataset, "my ds");
            assert_eq!(list.backups[0].verified, None);
            assert_eq!(list.backups[0].extra["new"], "kept");
        }
        other => panic!("dataset listing must select the specific alternative: {other:?}"),
    }
    {
        let seen = mock.0.lock().unwrap();
        assert_eq!(seen.len(), 8);
        assert_eq!(seen[2].uri, "/$/stats/my%20ds?at=commit%3A7");
        assert_eq!(seen[3].uri, "/$/schema/my%20ds?at=commit%3A7");
        assert_eq!(seen[3].accept, "application/json");
        assert_eq!(seen[6].uri, "/$/queries/my%20ds/by%2Fname");
        assert_eq!(seen[6].method, Method::PUT);
        assert_eq!(seen[6].body["new"], json!({"server": true}));
        assert_eq!(seen[6].body["parameters"]["limit"]["default"], 3);
    }
    // Existing return types and generic calls remain usable by downstream callers.
    let raw: Value = ds.stats(None).await.unwrap();
    assert_eq!(raw["quads"], 3);
    let raw = client
        .call_json(Method::GET, "/$/backups-list", &[], None)
        .await
        .unwrap();
    assert_eq!(raw["new"], true);
    assert!(matches!(
        client.dataset("bad").stats_typed(None).await,
        Err(Error::Parse { .. })
    ));
    assert_eq!(
        client.dataset("bad").stats(None).await.unwrap()["quads"],
        "not an integer"
    );
    task.abort();
}

#[test]
fn generated_models_preserve_nullable_union_and_unknown_members() {
    let original = backup();
    let typed: BackupSummary = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), original);
    let mut verified = original.clone();
    verified["verified"] = json!({"at": "now", "level": "future-level", "status": "ok", "new": 1});
    let typed: BackupSummary = serde_json::from_value(verified.clone()).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), verified);
    let page: BackupList =
        serde_json::from_value(json!({"backups": [], "next": "cursor", "future": 7})).unwrap();
    match page {
        BackupList::Alternative1(page) => {
            assert_eq!(page.next.as_deref(), Some("cursor"));
            assert_eq!(page.extra["future"], 7);
        }
        other => panic!("unexpected alternative: {other:?}"),
    }
    for count in [json!(u64::MAX), json!(i64::MIN)] {
        let value = json!({"name": "ds", "quads": count});
        let stats: DatasetStats = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(stats).unwrap(), value);
    }
    for count in [json!(u64::MAX), json!(i64::MIN)] {
        let mut entry = backup();
        entry["addedBytes"] = count.clone();
        entry["commit"]["seq"] = count;
        entry["dataset"]["branch"] = json!({"datasetId": "enclosing", "id": "branch", "name": "feature", "nextOrdinal": 65536});
        let list: BackupList = serde_json::from_value(
            json!({"dataset": "ds", "datasetId": "uuid", "backups": [entry.clone()]}),
        )
        .unwrap();
        let BackupList::Alternative2(list) = list else {
            panic!("nonempty dataset listing must decode through the specific alternative");
        };
        assert_eq!(serde_json::to_value(&list.backups[0]).unwrap(), entry);
        let page: BackupList =
            serde_json::from_value(json!({"backups": [entry.clone()], "next": "cursor"})).unwrap();
        let BackupList::Alternative1(page) = page else {
            panic!("nonempty generic page must decode through the page alternative");
        };
        assert_eq!(serde_json::to_value(&page.backups[0]).unwrap(), entry);
        assert_eq!(page.next.as_deref(), Some("cursor"));
    }
    for count in [json!(u64::MAX), json!(i64::MIN)] {
        let stats: DatasetStats =
            serde_json::from_value(json!({"name": "ds", "quads": 3, "baseQuads": count.clone()}))
                .unwrap();
        assert_eq!(serde_json::to_value(stats).unwrap()["baseQuads"], count);
    }
    let stats: DatasetStats =
        serde_json::from_value(json!({"name": "ds", "quads": 3, "baseQuads": null})).unwrap();
    assert_eq!(stats.base_quads, None);
    assert!(serde_json::from_value::<DatasetStats>(json!({"name": "ds", "quads": 3.0})).is_err());
    assert!(
        serde_json::from_value::<DatasetStats>(json!({"name": "ds", "quads": 3, "baseQuads": 3.0}))
            .is_err()
    );
    let minimal: StoredQuery = serde_json::from_value(json!({"query": "ASK {}"})).unwrap();
    assert_eq!(
        serde_json::to_value(minimal).unwrap(),
        json!({"query": "ASK {}"})
    );
    assert!(serde_json::from_value::<DatasetStats>(json!({"name": "ds"})).is_err());
    assert!(serde_json::from_value::<StoredQuery>(json!({"query": 1})).is_err());
}

#[cfg(feature = "blocking")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_typed_calls_use_the_same_models() {
    let (client, _, task) = start().await;
    tokio::task::spawn_blocking(move || {
        let client = sparkles_client::blocking::Client::from_async(client).unwrap();
        assert_eq!(client.stats_typed().unwrap().datasets["/my ds"].requests, 2);
        assert_eq!(client.backup_files_typed().unwrap().backups.len(), 1);
        let ds = client.dataset("my ds");
        assert_eq!(ds.stats_typed(None).unwrap().quads, 3);
        assert_eq!(ds.schema_typed(None).unwrap().dataset, "my ds");
        assert_eq!(ds.stored_queries_typed().unwrap().queries.len(), 1);
        let definition = ds.stored_query_typed("by/name").unwrap();
        assert_eq!(
            ds.put_stored_query_typed("by/name", &definition).unwrap(),
            definition
        );
        assert!(matches!(
            ds.backups_typed().unwrap(),
            BackupList::Alternative2(_)
        ));
    })
    .await
    .unwrap();
    task.abort();
}
