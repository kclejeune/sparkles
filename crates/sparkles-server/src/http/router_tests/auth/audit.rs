//! Item 62 observes actual authorized lifecycle operations through the router.
use super::*;

fn records(buf: &Buf) -> Vec<J> {
    String::from_utf8(buf.0.lock().clone())
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<J>(line).ok())
        .filter(|record| record["target"] == "sparkles::audit")
        .collect()
}

fn capture<T>(run: impl FnOnce() -> T) -> (T, Vec<J>) {
    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(buf.clone())
        .finish();
    let result = tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        run()
    });
    (result, records(&buf))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn mutate(s: &AuthServer, method: &str, uri: &str, body: &str, id: &str) -> R {
    call(
        &s.app,
        method,
        uri,
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/json"),
            ("x-request-id", id),
        ],
        body,
    )
    .await
}

#[test]
fn item62_audit_http_lifecycle_uses_actual_ids_and_selected_commit() {
    let ((source, branch, target), events) = capture(|| {
        runtime().block_on(async {
            let s = auth_server();
            let created = mutate(
                &s,
                "POST",
                "/$/datasets",
                r#"{"dbName":"audit-source","dbType":"persistent"}"#,
                "audit-create",
            )
            .await;
            assert_eq!(created.status, StatusCode::CREATED, "{}", created.text());
            let source = created.json()["id"].clone();
            {
                let ds = s.state.get("audit-source").unwrap();
                for value in [1, 2] {
                    sparkles::sparql::update::update(
                        &ds.store,
                        &format!("INSERT DATA {{ <urn:audit:{value}> <urn:p> {value} }}"),
                        &Default::default(),
                    )
                    .unwrap();
                }
            }
            let created = mutate(
                &s,
                "POST",
                "/$/branches/audit-source",
                r#"{"name":"review","note":"private-note-must-not-be-audited"}"#,
                "audit-branch-create",
            )
            .await;
            assert_eq!(created.status, StatusCode::CREATED, "{}", created.text());
            let branch = created.json()["id"].clone();
            let deleted = mutate(
                &s,
                "DELETE",
                "/$/branches/audit-source/review",
                "",
                "audit-branch-delete",
            )
            .await;
            assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text());
            let cloned = mutate(
                &s,
                "POST",
                "/$/datasets/audit-source/clone",
                r#"{"name":"audit-copy","at":"1"}"#,
                "audit-clone",
            )
            .await;
            assert_eq!(cloned.status, StatusCode::ACCEPTED, "{}", cloned.text());
            let task = wait_task(&s.state, cloned.json()["id"].as_str().unwrap()).await;
            assert_eq!(task.state, "done", "{task:?}");
            let target = s
                .state
                .get("audit-copy")
                .unwrap()
                .store
                .dataset_id()
                .to_string();
            let deleted =
                mutate(&s, "DELETE", "/$/datasets/audit-source", "", "audit-delete").await;
            assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.text());
            (source, branch, target)
        })
    });
    let find = |event: &str| -> &J {
        let found: Vec<_> = events
            .iter()
            .filter(|record| record["fields"]["event"] == event)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "missing or duplicate {event} audit event: {events:?}"
        );
        found[0]
    };
    let created = find("dataset_created");
    assert_eq!(created["fields"]["dataset"], "audit-source");
    assert_eq!(created["fields"]["id"], source);
    assert_eq!(created["fields"]["principal"], "user:alice");
    assert_eq!(created["fields"]["declared"], false);
    let created = find("branch_created");
    assert_eq!(created["fields"]["branch"], "review");
    assert_eq!(created["fields"]["branch_id"], branch);
    let deleted = find("branch_deleted");
    assert_eq!(deleted["fields"]["branch"], "review");
    assert_eq!(deleted["fields"]["id"], source);
    assert!(deleted["fields"].get("branch_id").is_none());
    let cloned = find("dataset_cloned");
    assert_eq!(cloned["fields"]["source"], "audit-source");
    assert_eq!(cloned["fields"]["source_commit"], 1);
    assert_eq!(cloned["fields"]["dataset"], "audit-copy");
    assert_eq!(cloned["fields"]["id"], target);
    assert_eq!(cloned["fields"]["principal"], "user:alice");
    assert_eq!(cloned["span"]["request_id"], "audit-clone");
    let deleted = find("dataset_deleted");
    assert_eq!(deleted["fields"]["id"], source);
    assert!(deleted["fields"].get("quads").is_none());
    assert_eq!(
        events.len(),
        5,
        "one event per completed operation: {events:?}"
    );
    let text = serde_json::to_string(&events).unwrap();
    assert!(!text.contains("private-note-must-not-be-audited"));
    assert!(!text.contains("alice-pw"));
}

#[test]
fn item62_audit_authorized_failures_do_not_log_denied_attempts_or_payloads() {
    let (_, events) = capture(|| {
        runtime().block_on(async {
            let s = auth_server();
            let conflict = mutate(
                &s,
                "POST",
                "/$/datasets",
                r#"{"dbName":"wiki","password":"private-audit-body"}"#,
                "audit-conflict",
            )
            .await;
            assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.text());
            let malformed = mutate(&s, "POST", "/$/datasets", "{", "audit-invalid").await;
            assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
            let denied = call(
                &s.app,
                "POST",
                "/$/datasets",
                &[("authorization", &b("bob"))],
                "dbName=denied",
            )
            .await;
            assert_eq!(denied.status, StatusCode::FORBIDDEN);
            let hidden = call(
                &s.app,
                "DELETE",
                "/$/datasets/secret",
                &[("authorization", &b("bob"))],
                "",
            )
            .await;
            assert_eq!(hidden.status, StatusCode::NOT_FOUND);
        })
    });
    assert_eq!(
        events.len(),
        2,
        "authorized failures need exactly one audit record each: {events:?}"
    );
    for record in &events {
        assert_eq!(record["fields"]["event"], "dataset_admin_failed");
        assert_eq!(record["fields"]["principal"], "user:alice");
        assert!(
            record["fields"]["code"]
                .as_str()
                .is_some_and(|code| !code.is_empty())
        );
        assert!(record["fields"].get("error").is_none());
    }
    let text = serde_json::to_string(&events).unwrap();
    for private in ["private-audit-body", "alice-pw", "bob-pw", "secret"] {
        assert!(!text.contains(private), "audit leaked {private}: {text}");
    }
}

#[test]
fn item62_audit_queued_clone_cancellation_has_one_failure_and_no_publication() {
    let (_, events) = capture(|| {
        runtime().block_on(async {
            let s = auth_server();
            s.state.task_queue.set_max(1);
            let (release, wait) = std::sync::mpsc::channel();
            let blocker = s.state.start_task("test", "wiki", move |_| {
                wait.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok("released".into())
            });
            let cloned = mutate(
                &s,
                "POST",
                "/$/datasets/wiki/clone",
                r#"{"name":"audit-cancel"}"#,
                "audit-queued-clone",
            )
            .await;
            assert_eq!(cloned.status, StatusCode::ACCEPTED, "{}", cloned.text());
            assert_eq!(cloned.json()["state"], "queued");
            let id = cloned.json()["id"].as_str().unwrap().to_string();
            let cancelled = s.state.cancel_task(&id).unwrap().unwrap();
            assert_eq!(cancelled.state, "cancelled");
            assert!(s.state.get("audit-cancel").is_none());
            release.send(()).unwrap();
            assert_eq!(wait_task(&s.state, &blocker.id).await.state, "done");
        })
    });
    assert_eq!(
        events.len(),
        1,
        "queued cancellation must audit once: {events:?}"
    );
    assert_eq!(events[0]["fields"]["event"], "dataset_admin_failed");
    assert_eq!(events[0]["fields"]["operation"], "dataset_cloned");
    assert_eq!(events[0]["fields"]["code"], "aborted");
    assert_eq!(events[0]["fields"]["principal"], "user:alice");
    assert_eq!(events[0]["span"]["request_id"], "audit-queued-clone");
}

#[cfg(feature = "mcp")]
#[test]
fn item62_audit_mcp_http_branch_operations_retain_the_actual_caller() {
    let (_, events) = capture(|| {
        runtime().block_on(async {
            let s = build(Fixture {
                mcp: Some(vec!["--mcp-allow-update"]),
                ..Default::default()
            });
            for (id, tool) in [(62, "create_branch"), (63, "delete_branch")] {
                let body = serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "method": "tools/call",
                    "params": {
                        "name": tool,
                        "arguments": {"dataset": "wiki", "name": "audit-mcp"},
                        "_meta": {
                            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                            "io.modelcontextprotocol/clientCapabilities": {}
                        }
                    }
                })
                .to_string();
                let response = call(
                    &s.app,
                    "POST",
                    "/$/mcp",
                    &[
                        ("authorization", &b("alice")),
                        ("content-type", "application/json"),
                        ("accept", "application/json, text/event-stream"),
                        ("mcp-protocol-version", "2026-07-28"),
                        ("mcp-method", "tools/call"),
                        ("mcp-name", tool),
                    ],
                    &body,
                )
                .await;
                assert_eq!(response.status, StatusCode::OK, "{}", response.text());
                let text = response.text();
                let json = text
                    .lines()
                    .filter_map(|line| line.strip_prefix("data: "))
                    .rfind(|line| line.starts_with('{'))
                    .unwrap_or(&text);
                let result: J = serde_json::from_str(json).unwrap();
                assert!(result.get("error").is_none(), "{result}");
                assert_eq!(result["result"]["isError"], false, "{result}");
            }
        })
    });
    assert_eq!(
        events.len(),
        2,
        "actual MCP branch mutations must audit once each: {events:?}"
    );
    assert_eq!(events[0]["fields"]["event"], "branch_created");
    assert_eq!(events[1]["fields"]["event"], "branch_deleted");
    for (id, event) in [("62", &events[0]), ("63", &events[1])] {
        assert_eq!(event["fields"]["principal"], "user:alice");
        assert_eq!(event["fields"]["dataset"], "wiki");
        assert_eq!(event["fields"]["branch"], "audit-mcp");
        assert_eq!(event["fields"]["request_id"], id);
    }
}

#[cfg(feature = "mcp")]
#[test]
fn item62_audit_scratch_expiry_uses_system_actor_after_actual_deletion() {
    let (dataset_id, events) = capture(|| {
        let s = auth_server();
        let ds = s.state.get("wiki").unwrap();
        let branch = ds
            .store
            .create_branch("audit-expire", &Default::default())
            .unwrap();
        ds.store
            .set_branch_scratch(
                "audit-expire",
                Some(sparkles::branch::Scratch {
                    creator: "user:alice".into(),
                }),
            )
            .unwrap();
        let gone = crate::mcp::branches::sweep(
            &s.state,
            Duration::from_secs(3600),
            branch.created_ms + 2 * 3600 * 1000,
        );
        assert_eq!(gone, ["wiki@audit-expire"]);
        assert!(ds.store.branch_info("audit-expire").is_err());
        ds.store.dataset_id().to_string()
    });
    assert_eq!(
        events.len(),
        1,
        "missing successful expiry audit event: {events:?}"
    );
    let fields = &events[0]["fields"];
    assert_eq!(fields["event"], "branch_deleted");
    assert_eq!(fields["principal"], "system:scratch-expiry");
    assert_eq!(fields["reason"], "idle-expiry");
    assert_eq!(fields["dataset"], "wiki");
    assert_eq!(fields["id"], dataset_id);
    assert_eq!(fields["branch"], "audit-expire");
    assert!(fields.get("branch_id").is_none());
    assert!(fields.get("request_id").is_none());
    assert!(fields.get("creator").is_none());
}

#[derive(Clone, Copy, Debug)]
enum DetachedOperation {
    DatasetCreate,
    DatasetDelete,
    BranchCreate,
    BranchDelete,
}

#[test]
fn item62_audit_authorized_branch_validation_failure_is_owned_by_handler() {
    let (_, events) = capture(|| {
        runtime().block_on(async {
            let s = auth_server();
            let response = mutate(
                &s,
                "POST",
                "/$/branches/wiki",
                r#"{"name":"audit-invalid","at":"commit:private-invalid-selector"}"#,
                "audit-branch-validation",
            )
            .await;
            assert_eq!(
                response.status,
                StatusCode::BAD_REQUEST,
                "{}",
                response.text()
            );
            assert!(
                s.state
                    .get("wiki")
                    .unwrap()
                    .store
                    .branch_info("audit-invalid")
                    .is_err()
            );
        })
    });
    assert_eq!(
        events.len(),
        1,
        "authorized validation must audit once: {events:?}"
    );
    let fields = &events[0]["fields"];
    assert_eq!(fields["event"], "dataset_admin_failed");
    assert_eq!(fields["operation"], "branch_created");
    assert_eq!(fields["principal"], "user:alice");
    assert_eq!(fields["branch"], "audit-invalid");
    assert_eq!(events[0]["span"]["request_id"], "audit-branch-validation");
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("private-invalid-selector")
    );
}

fn cancellation_worker_case(operation: DetachedOperation, fail: bool, detach: bool) {
    use crate::http::algo::probe::{CURRENT, Gate, Probe, Release};

    let ((id, request_id), events) = capture(|| {
        runtime().block_on(async {
            let s = auth_server();
            let name = "audit-detached";
            let branch = "audit-detached-branch";
            if !matches!(operation, DetachedOperation::DatasetCreate) {
                s.state.create(name, DbType::Persistent).unwrap();
            }
            if matches!(operation, DetachedOperation::BranchDelete) {
                s.state
                    .get(name)
                    .unwrap()
                    .store
                    .create_branch(branch, &Default::default())
                    .unwrap();
            }
            // A real live handle makes deletion fail inside the worker, after the
            // request's ordinary pre-dispatch checks have passed.
            let held = if fail && matches!(operation, DetachedOperation::DatasetDelete) {
                s.state.get(name)
            } else {
                None
            };
            let (method, uri, body) = match operation {
                DetachedOperation::DatasetCreate => (
                    "POST",
                    "/$/datasets".to_string(),
                    format!(r#"{{"dbName":"{name}","dbType":"persistent"}}"#),
                ),
                DetachedOperation::DatasetDelete => {
                    ("DELETE", format!("/$/datasets/{name}"), String::new())
                }
                DetachedOperation::BranchCreate => (
                    "POST",
                    format!("/$/branches/{name}"),
                    format!(r#"{{"name":"{branch}"}}"#),
                ),
                DetachedOperation::BranchDelete => (
                    "DELETE",
                    format!("/$/branches/{name}/{branch}"),
                    String::new(),
                ),
            };
            let request_id = format!("audit-{operation:?}-fail-{fail}-detach-{detach}");
            let gate = Arc::new(Gate::default());
            let probe = Probe {
                blocking: Some(gate.clone()),
                ..Default::default()
            };
            let app = s.app.clone();
            let rid = request_id.clone();
            let request = tokio::spawn(CURRENT.scope(probe, async move {
                call(
                    &app,
                    method,
                    &uri,
                    &[
                        ("authorization", &b("alice")),
                        ("content-type", "application/json"),
                        ("x-request-id", &rid),
                    ],
                    &body,
                )
                .await
            }));
            tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
                .await
                .expect("lifecycle mutation never entered its real blocking worker");
            let release = Release(gate.clone());
            let identity = s.state.get(name).map(|ds| ds.store.dataset_id());
            // Introduce conflicts only after dispatch, so failure ownership must
            // be the worker's rather than the handler's validation wrapper.
            if fail {
                match operation {
                    DetachedOperation::DatasetCreate => {
                        s.state.create(name, DbType::Persistent).unwrap();
                    }
                    DetachedOperation::DatasetDelete => {}
                    DetachedOperation::BranchCreate => {
                        s.state
                            .get(name)
                            .unwrap()
                            .store
                            .create_branch(branch, &Default::default())
                            .unwrap();
                    }
                    DetachedOperation::BranchDelete => {
                        s.state
                            .get(name)
                            .unwrap()
                            .store
                            .delete_branch(branch, false)
                            .unwrap();
                    }
                }
            }
            let request = if detach {
                request.abort();
                assert!(matches!(request.await, Err(error) if error.is_cancelled()));
                None
            } else {
                Some(request)
            };
            gate.release();
            tokio::time::timeout(Duration::from_secs(10), gate.done.notified())
                .await
                .expect("actual lifecycle worker did not finish after release");
            assert!(!gate.timed_out.load(Ordering::Relaxed));
            if let Some(request) = request {
                let response = request.await.unwrap();
                if fail {
                    assert!(
                        response.status.is_client_error() || response.status.is_server_error(),
                        "worker failure returned success: {}",
                        response.text()
                    );
                } else {
                    assert!(response.status.is_success(), "{}", response.text());
                }
            }
            // State is inspected after the worker completion notification, not
            // after an HTTP response or a timing-only sleep.
            match operation {
                DetachedOperation::DatasetCreate => assert!(s.state.get(name).is_some()),
                DetachedOperation::DatasetDelete => {
                    assert_eq!(s.state.get(name).is_some(), fail);
                    if fail {
                        assert_eq!(
                            s.state.get(name).unwrap().store.dataset_id(),
                            identity.unwrap()
                        );
                    }
                }
                DetachedOperation::BranchCreate => {
                    assert!(s.state.get(name).unwrap().store.branch_info(branch).is_ok());
                }
                DetachedOperation::BranchDelete => {
                    assert!(
                        s.state
                            .get(name)
                            .unwrap()
                            .store
                            .branch_info(branch)
                            .is_err()
                    );
                }
            }
            let id = identity.or_else(|| s.state.get(name).map(|ds| ds.store.dataset_id()));
            drop(held);
            drop(release);
            (id.unwrap().to_string(), request_id)
        })
    });
    assert_eq!(
        events.len(),
        1,
        "cancellation worker terminal event missing or duplicated for {request_id}: {events:?}"
    );
    let fields = &events[0]["fields"];
    let operation = match operation {
        DetachedOperation::DatasetCreate => "dataset_created",
        DetachedOperation::DatasetDelete => "dataset_deleted",
        DetachedOperation::BranchCreate => "branch_created",
        DetachedOperation::BranchDelete => "branch_deleted",
    };
    assert_eq!(fields["principal"], "user:alice");
    assert_eq!(fields["dataset"], "audit-detached");
    assert_eq!(events[0]["span"]["request_id"], request_id);
    if fail {
        assert_eq!(fields["event"], "dataset_admin_failed");
        assert_eq!(fields["operation"], operation);
        assert!(fields["code"].as_str().is_some_and(|code| !code.is_empty()));
        assert!(fields.get("error").is_none());
    } else {
        assert_eq!(fields["event"], operation);
        assert_eq!(fields["id"], id);
    }
    if operation.starts_with("branch_") {
        assert_eq!(fields["branch"], "audit-detached-branch");
    }
}

#[test]
fn item62_audit_cancellation_attached_worker_controls() {
    for operation in [
        DetachedOperation::DatasetCreate,
        DetachedOperation::DatasetDelete,
        DetachedOperation::BranchCreate,
        DetachedOperation::BranchDelete,
    ] {
        for fail in [false, true] {
            cancellation_worker_case(operation, fail, false);
        }
    }
}

#[test]
fn item62_audit_cancellation_dataset_create_success() {
    cancellation_worker_case(DetachedOperation::DatasetCreate, false, true);
}

#[test]
fn item62_audit_cancellation_dataset_create_failure() {
    cancellation_worker_case(DetachedOperation::DatasetCreate, true, true);
}

#[test]
fn item62_audit_cancellation_dataset_delete_success() {
    cancellation_worker_case(DetachedOperation::DatasetDelete, false, true);
}

#[test]
fn item62_audit_cancellation_dataset_delete_failure() {
    cancellation_worker_case(DetachedOperation::DatasetDelete, true, true);
}

#[test]
fn item62_audit_cancellation_branch_create_success() {
    cancellation_worker_case(DetachedOperation::BranchCreate, false, true);
}

#[test]
fn item62_audit_cancellation_branch_create_failure() {
    cancellation_worker_case(DetachedOperation::BranchCreate, true, true);
}

#[test]
fn item62_audit_cancellation_branch_delete_success() {
    cancellation_worker_case(DetachedOperation::BranchDelete, false, true);
}

#[test]
fn item62_audit_cancellation_branch_delete_failure() {
    cancellation_worker_case(DetachedOperation::BranchDelete, true, true);
}
