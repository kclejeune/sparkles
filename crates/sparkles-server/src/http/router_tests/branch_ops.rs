//! Branches and merges over HTTP, Phase 2 of F09: squash merges, reverts,
//! cherry-picks, replayed fast-forwards, renames, deletions that re-parent, exempt
//! predicates and merges as tasks (the acceptance examples A24 onward).

use super::branches::{get, json_req, setup, subjects, update};
use super::*;
use serde_json::json;

#[tokio::test]
async fn a24_squash_merges() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    update(&app, "ds@dev", "INSERT DATA { <urn:d> <urn:p> 1 }").await;
    let (r, _) = get(&app, "/$/merge/ds?source=dev&squash=true").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["squashed"], true);
    let (r, h) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "squash": true }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let m = r.json();
    assert_eq!(m["merged"], true);
    assert_eq!(m["squashed"], true);
    assert_eq!(m["commit"]["kind"], "merge");
    assert_eq!(m["commit"]["message"], "squash dev (commit 4) into main");
    assert!(m["commit"].get("mergedFrom").is_none(), "{m}");
    assert_eq!(commit_header(&h), 3);
    assert_eq!(
        subjects(&app, "/ds/sparql").await,
        subjects(&app, "/ds@dev/sparql").await
    );
    // no second parent: dev is still ahead, and the listing shows no mergedFrom
    let (r, _) = get(&app, "/$/branches/ds/dev").await;
    assert_eq!(r.json()["ahead"], 2);
    let (r, _) = get(&app, "/$/commits/ds?limit=1").await;
    assert!(r.json()["commits"][0].get("mergedFrom").is_none());
    // nothing new: no commit
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "squash": true }),
    )
    .await;
    assert_eq!(r.json()["upToDate"], true, "{}", r.text());
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "squash": 1 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

async fn post(app: &Router, path: &str, body: &str) -> (Resp, axum::http::HeaderMap) {
    send_h(
        app,
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn a25_reverts() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    update(&app, "ds", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    update(
        &app,
        "ds",
        "DELETE DATA { <urn:a> <urn:age> 30 } ; INSERT DATA { <urn:a> <urn:age> 31 }",
    )
    .await;
    let (r, _) = get(&app, "/$/revert/ds?commit=3").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["changes"]["deleted"], 1);
    assert_eq!(r.json()["merged"], false);
    let (r, h) = post(&app, "/$/revert/ds?commit=3", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let m = r.json();
    assert_eq!(m["commit"]["kind"], "revert");
    assert_eq!(m["commit"]["seq"], 5);
    assert_eq!(m["commit"]["message"], "revert commit 3");
    assert!(m["commit"].get("mergedFrom").is_none(), "{m}");
    assert_eq!(m["reverted"], json!({ "branch": "main", "seq": 3 }));
    assert_eq!(m["base"]["seq"], 3);
    assert_eq!(m["source"]["seq"], 2);
    assert_eq!(commit_header(&h), 5);
    assert_eq!(subjects(&app, "/ds/sparql").await, ["urn:a", "urn:b"]);
    let (r, _) = post(&app, "/$/revert/ds?commit=3", "{}").await;
    assert_eq!(r.json()["upToDate"], true, "{}", r.text());
    // a later change to the same cell conflicts
    let (r, _) = post(&app, "/$/revert/ds?commit=1", "").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "merge-conflict");
    assert_eq!(r.json()["cells"][0]["predicate"], "<urn:age>");
    let (r, _) = post(
        &app,
        "/$/revert/ds?commit=1",
        r#"{"onConflict":"theirs","message":"no age"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["commit"]["message"], "no age");
    assert_eq!(subjects(&app, "/ds/sparql").await, ["urn:b"]);
    // on a branch, of a commit it shares with main
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    let (r, _) = post(&app, "/$/revert/ds?branch=dev&commit=2", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["target"]["branch"], "dev");
    assert!(subjects(&app, "/ds@dev/sparql").await.is_empty());
    assert_eq!(subjects(&app, "/ds/sparql").await, ["urn:b"]);
    // errors
    for (path, body, status) in [
        ("/$/revert/ds?commit=0", "", StatusCode::BAD_REQUEST),
        ("/$/revert/ds", "", StatusCode::BAD_REQUEST),
        ("/$/revert/ds?commit=99", "", StatusCode::NOT_FOUND),
        (
            "/$/revert/ds?commit=2&branch=nope",
            "",
            StatusCode::NOT_FOUND,
        ),
        (
            "/$/revert/ds?commit=2",
            r#"{"squash":true}"#,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (r, _) = post(&app, path, body).await;
        assert_eq!(r.status, status, "{path} {body}: {}", r.text());
    }
    json_req(
        &app,
        "PATCH",
        "/$/branches/ds/main",
        json!({ "protected": true }),
    )
    .await;
    let (r, _) = post(&app, "/$/revert/ds?commit=2", "").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["code"], "branch-protected");
}

#[tokio::test]
async fn a26_cherry_picks() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    update(&app, "ds@dev", "INSERT DATA { <urn:d> <urn:p> 1 }").await;
    let (r, _) = get(&app, "/$/cherry-pick/ds?source=dev&commit=4").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["changes"]["inserted"], 1);
    let (r, h) = post(&app, "/$/cherry-pick/ds?source=dev&commit=4", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let m = r.json();
    assert_eq!(m["commit"]["kind"], "cherry-pick");
    assert_eq!(m["commit"]["message"], "cherry-pick commit 4 of dev");
    assert!(m["commit"].get("mergedFrom").is_none(), "{m}");
    assert_eq!(m["picked"], json!({ "branch": "dev", "seq": 4 }));
    assert_eq!(m["base"]["seq"], 3);
    assert_eq!(commit_header(&h), 3);
    assert_eq!(
        subjects(&app, "/ds/sparql").await,
        ["urn:a", "urn:b", "urn:d"]
    );
    let (r, _) = post(&app, "/$/cherry-pick/ds?source=dev&commit=4", "").await;
    assert_eq!(r.json()["upToDate"], true, "{}", r.text());
    // into another branch than main
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "qa" })).await;
    let (r, _) = post(&app, "/$/cherry-pick/ds?source=dev&commit=3&branch=qa", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["target"]["branch"], "qa");
    assert!(
        subjects(&app, "/ds@qa/sparql")
            .await
            .contains(&"urn:c".to_string())
    );
    for (path, status) in [
        ("/$/cherry-pick/ds?commit=4", StatusCode::BAD_REQUEST),
        ("/$/cherry-pick/ds?source=dev", StatusCode::BAD_REQUEST),
        (
            "/$/cherry-pick/ds?source=nope&commit=4",
            StatusCode::NOT_FOUND,
        ),
        (
            "/$/cherry-pick/ds?source=dev&commit=9",
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (r, _) = post(&app, path, "").await;
        assert_eq!(r.status, status, "{path}: {}", r.text());
    }
}

#[tokio::test]
async fn a27_replayed_fast_forwards() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    update(&app, "ds@dev", "DELETE DATA { <urn:b> <urn:name> \"B\" }").await;
    let (r, _) = get(&app, "/$/merge/ds?source=dev&ff=replay").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["replayed"].as_array().unwrap().len(), 2);
    assert_eq!(r.json()["replayed"][0]["commit"], J::Null);
    let (r, h) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "ff": "replay" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let m = r.json();
    assert_eq!(m["replayed"][0]["from"]["seq"], 3);
    assert_eq!(m["replayed"][1]["commit"], 4);
    assert_eq!(m["commit"]["kind"], "update");
    assert_eq!(commit_header(&h), 4);
    assert_eq!(
        subjects(&app, "/ds/sparql").await,
        subjects(&app, "/ds@dev/sparql").await
    );
    let (r, _) = get(&app, "/$/commits/ds?limit=2").await;
    let cs = r.json()["commits"].clone();
    assert_eq!(cs[0]["kind"], "update");
    assert_eq!(cs[0]["replayedFrom"]["branch"], "dev");
    assert_eq!(cs[0]["replayedFrom"]["seq"], 4);
    assert!(cs[0].get("mergedFrom").is_none());
    // a target with changes of its own
    update(&app, "ds", "INSERT DATA { <urn:m> <urn:p> 1 }").await;
    update(&app, "ds@dev", "INSERT DATA { <urn:e> <urn:p> 1 }").await;
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "ff": "replay" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "not-fast-forward");
}

#[tokio::test]
async fn a28_renames() {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    json_req(
        &app,
        "POST",
        "/$/branches/ds",
        json!({ "name": "feat", "from": "dev" }),
    )
    .await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    // the branch's dataset object is open and cached under its name
    let before = subjects(&app, "/ds@dev/sparql").await;
    let id = st.get("ds").unwrap().store.branch_id_of("dev").unwrap();
    let (r, h) = json_req(
        &app,
        "PATCH",
        "/$/branches/ds/dev",
        json!({ "name": "work" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(h["location"], "/$/branches/ds/work");
    assert_eq!(r.json()["name"], "work");
    assert_eq!(r.json()["id"], id.to_string());
    assert_eq!(r.json()["grantsChanged"], 0);
    assert_eq!(subjects(&app, "/ds@work/sparql").await, before);
    let (r, _) = get(&app, "/ds@dev/sparql?query=ASK%7B%7D").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let (r, h) = update(&app, "ds@work", "INSERT DATA { <urn:d> <urn:p> 1 }").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(h["sparkles-branch"], "work");
    let (r, _) = get(&app, "/$/branches/ds/feat").await;
    assert_eq!(r.json()["upstream"], "work");
    assert_eq!(r.json()["from"]["branch"], "work");
    let (r, _) = get(&app, "/$/commits/ds?branch=work&limit=1").await;
    assert_eq!(r.json()["commits"][0]["branch"], "work");
    // the server's per-branch state follows the name
    let main = st.get("ds").unwrap();
    let names: Vec<String> = main.branches.lock().keys().cloned().collect();
    assert!(
        names.contains(&"work".to_string()) && !names.contains(&"dev".to_string()),
        "{names:?}"
    );
    assert_eq!(st.branch_dataset(&main, "work").unwrap().key(), "ds@work");
    // refusals
    for (path, body, status) in [
        (
            "/$/branches/ds/work",
            json!({ "name": "feat" }),
            StatusCode::CONFLICT,
        ),
        (
            "/$/branches/ds/work",
            json!({ "name": "main" }),
            StatusCode::CONFLICT,
        ),
        (
            "/$/branches/ds/main",
            json!({ "name": "x" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/$/branches/ds/work",
            json!({ "name": "a/b" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/$/branches/ds/nope",
            json!({ "name": "x" }),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (r, _) = json_req(&app, "PATCH", path, body.clone()).await;
        assert_eq!(r.status, status, "{path} {body}: {}", r.text());
    }
    // a rename with a note
    let (r, _) = json_req(
        &app,
        "PATCH",
        "/$/branches/ds/work",
        json!({ "name": "work2", "note": "renamed" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["note"], "renamed");
}

#[tokio::test]
async fn a29_deletions_that_re_parent() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    json_req(
        &app,
        "POST",
        "/$/branches/ds",
        json!({ "name": "feat", "from": "dev" }),
    )
    .await;
    update(&app, "ds@feat", "INSERT DATA { <urn:f> <urn:p> 1 }").await;
    let feat = subjects(&app, "/ds@feat/sparql").await;
    let del = |q: &'static str| {
        let app = app.clone();
        async move {
            send(
                &app,
                Request::delete(format!("/$/branches/ds/dev{q}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
        }
    };
    let r = del("").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "has-children");
    // c is in feat's history: nothing is lost
    let r = del("?reparent=true").await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let (r, _) = get(&app, "/$/branches/ds").await;
    let names: Vec<String> = r.json()["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["main", "feat"]);
    let (r, _) = get(&app, "/$/branches/ds/feat").await;
    assert_eq!(r.json()["upstream"], "main");
    assert_eq!(r.json()["from"]["branch"], J::Null);
    assert_eq!(subjects(&app, "/ds@feat/sparql").await, feat);
    let (r, _) = get(&app, "/$/commits/ds?branch=feat").await;
    let owners: Vec<J> = r.json()["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["branch"].clone())
        .collect();
    assert_eq!(owners[0], "feat");
    assert_eq!(owners[1], J::Null);
    // a read at the retired branch's commit
    let (r, _) = get(
        &app,
        "/ds/sparql?branch=feat&at=3&query=ASK%7B%3Curn%3Ac%3E%3Fp%3Fo%7D",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let (r, _) = json_req(&app, "POST", "/$/merge/ds", json!({ "source": "feat" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let (r, _) = get(&app, "/$/branches/ds/dev").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a30_exempt_predicates() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    let label = "http://www.w3.org/2000/01/rdf-schema#label";
    update(
        &app,
        "ds",
        &format!("INSERT DATA {{ <urn:a> <{label}> \"ours\" }}"),
    )
    .await;
    update(
        &app,
        "ds@dev",
        &format!("INSERT DATA {{ <urn:a> <{label}> \"theirs\" }}"),
    )
    .await;
    let (r, _) = json_req(&app, "POST", "/$/merge/ds", json!({ "source": "dev" })).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let q = format!(
        "/$/merge/ds?source=dev&exempt={}",
        percent_encoding::utf8_percent_encode(label, percent_encoding::NON_ALPHANUMERIC)
    );
    let (r, _) = get(&app, &q).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["conflicts"]["found"], 0);
    // the dataset's list
    let (r, _) = json_req(
        &app,
        "PATCH",
        "/$/branches/ds",
        json!({ "exemptPredicates": [format!("<{label}>")] }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["exemptPredicates"], json!([label]));
    let (r, _) = get(&app, "/$/branches/ds").await;
    assert_eq!(r.json()["exemptPredicates"], json!([label]));
    let (r, _) = json_req(&app, "POST", "/$/merge/ds", json!({ "source": "dev" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for (body, status) in [
        (
            json!({ "exemptPredicates": ["not an iri"] }),
            StatusCode::BAD_REQUEST,
        ),
        (json!({ "other": 1 }), StatusCode::BAD_REQUEST),
        (json!({ "exemptPredicates": [] }), StatusCode::OK),
    ] {
        let (r, _) = json_req(&app, "PATCH", "/$/branches/ds", body.clone()).await;
        assert_eq!(r.status, status, "{body}: {}", r.text());
    }
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "exempt": "x" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn a31_merges_as_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    let req = |path: &str, body: J| {
        Request::post(path.to_string())
            .header(header::CONTENT_TYPE, "application/json")
            .header("prefer", "respond-async, wait=10")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (r, h) = send_h(&app, req("/$/merge/ds", json!({ "source": "dev" }))).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(h["preference-applied"], "respond-async");
    let t = r.json();
    assert_eq!(t["kind"], "merge");
    assert_eq!(t["cancellable"], true);
    let id = t["id"].as_str().unwrap().to_string();
    assert_eq!(h["location"], format!("/$/tasks/{id}").as_str());
    let done = super::tasks::wait_done(&st, &id).await;
    assert_eq!(done.state, "done", "{:?}", done.message);
    let d = done.detail.unwrap();
    assert_eq!(d["merged"], true);
    assert_eq!(d["commit"]["seq"], 3);
    assert!(
        subjects(&app, "/ds/sparql")
            .await
            .contains(&"urn:c".to_string())
    );
    // a merge that conflicts fails, with the report as its detail
    update(
        &app,
        "ds",
        "DELETE DATA { <urn:a> <urn:age> 30 } ; INSERT DATA { <urn:a> <urn:age> 31 }",
    )
    .await;
    update(
        &app,
        "ds@dev",
        "DELETE DATA { <urn:a> <urn:age> 30 } ; INSERT DATA { <urn:a> <urn:age> 32 }",
    )
    .await;
    let (r, _) = send_h(&app, req("/$/merge/ds", json!({ "source": "dev" }))).await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    let done = super::tasks::wait_done(&st, &id).await;
    assert_eq!(done.state, "failed");
    assert_eq!(done.detail.unwrap()["code"], "merge-conflict");
    // reverts and cherry-picks too
    let (r, _) = send_h(&app, req("/$/revert/ds?commit=4", json!({}))).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(r.json()["kind"], "revert");
    let id = r.json()["id"].as_str().unwrap().to_string();
    let done = super::tasks::wait_done(&st, &id).await;
    assert_eq!(done.state, "done", "{:?}", done.message);
    assert_eq!(done.detail.unwrap()["reverted"]["seq"], 4);
    // a dry run answers at once
    let (r, _) = send_h(
        &app,
        req(
            "/$/merge/ds",
            json!({ "source": "dev", "dryRun": true, "onConflict": "theirs" }),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn a32_per_branch_gauges() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    let (r, _) = get(&app, "/$/metrics").await;
    assert_eq!(r.status, StatusCode::OK);
    let text = r.text();
    for line in [
        "sparkles_branch_quads{dataset=\"ds\",branch=\"main\"} 2",
        "sparkles_branch_quads{dataset=\"ds\",branch=\"dev\"} 3",
        "sparkles_branch_delta_quads{dataset=\"ds\",branch=\"dev\",kind=\"insert\"} 3",
        "sparkles_branch_held_bytes{dataset=\"ds\"} 0",
        "sparkles_branches{dataset=\"ds\"} 2",
    ] {
        assert!(text.contains(line), "{line} not in:\n{text}");
    }
    assert!(text.contains("sparkles_branch_disk_bytes{dataset=\"ds\",branch=\"dev\"}"));
    assert!(text.contains("sparkles_branch_wal_bytes{dataset=\"ds\",branch=\"dev\"}"));
}

/// `POST /$/branches/{ds}/{name}/relink` moves a linked branch onto main's current
/// index and keeps its id, head and data, at once or as a task.
#[tokio::test]
async fn relinks_linked_branches() {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    for name in ["dev", "dev2", "own"] {
        let (r, _) = json_req(&app, "POST", "/$/branches/ds", json!({ "name": name })).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    update(&app, "ds", "INSERT DATA { <urn:d> <urn:p> 1 }").await;
    let ds = st.get("ds").unwrap();
    ds.store.compact_with(&Default::default()).unwrap();
    let (before, _) = get(&app, "/$/branches/ds/dev").await;
    let before = before.json();
    let (r, _) = post(&app, "/$/branches/ds/dev/relink", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["dataset"], "ds");
    assert_eq!(j["branch"], "dev");
    assert_eq!(j["branchId"], before["id"]);
    assert_eq!(j["mode"], "relink");
    assert_eq!(j["quads"], 3);
    assert!(j.get("abandoned").is_none(), "{j}");
    let (after, _) = get(&app, "/$/branches/ds/dev").await;
    let after = after.json();
    assert_eq!(after["id"], before["id"]);
    assert_eq!(after["head"], before["head"]);
    assert_eq!(after["storage"]["linked"], true);
    let s = subjects(&app, "/ds@dev/sparql").await;
    assert!(s.contains(&"urn:c".to_string()), "{s:?}");
    assert!(!s.contains(&"urn:d".to_string()), "{s:?}");
    // a relinked branch keeps taking writes
    let (r, _) = update(&app, "ds@dev", "INSERT DATA { <urn:e> <urn:p> 1 }").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // as a task, with the result as its detail
    let (r, h) = send_h(
        &app,
        Request::post("/$/branches/ds/dev2/relink")
            .header("prefer", "respond-async")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    assert_eq!(h["preference-applied"], "respond-async");
    let t = r.json();
    assert_eq!(t["kind"], "relink");
    assert_eq!(t["target"], "dev2");
    assert_eq!(t["cancellable"], true);
    let id = t["id"].as_str().unwrap().to_string();
    assert_eq!(h["location"], format!("/$/tasks/{id}").as_str());
    let done = super::tasks::wait_done(&st, &id).await;
    assert_eq!(done.state, "done", "{:?}", done.message);
    assert_eq!(done.progress, Some(1.0));
    let d = done.detail.unwrap();
    assert_eq!(d["branch"], "dev2");
    assert_eq!(d["quads"], 2);
    // refusals
    let (r, _) = post(&app, "/$/branches/ds/main/relink", "").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(r.json()["code"], "invalid-branch");
    let (r, _) = post(&app, "/$/branches/ds/absent/relink", "").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    assert_eq!(r.json()["code"], "no-such-branch");
    let (r, _) = post(&app, "/$/branches/ds/dev/relink", r#"{"force":true}"#).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let (r, _) = post(&app, "/$/branches/none/dev/relink", "").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // a branch with an index of its own
    ds.store
        .branch("own")
        .unwrap()
        .compact_with(&Default::default())
        .unwrap();
    let (r, _) = get(&app, "/$/branches/ds/own").await;
    assert_eq!(r.json()["storage"]["linked"], false, "{}", r.text());
    let (r, _) = post(&app, "/$/branches/ds/own/relink", "").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "not-relinkable");
}

/// In-memory datasets have no index files to share, so their branches cannot be
/// relinked.
#[tokio::test]
async fn memory_branches_are_not_relinkable() {
    let dir = tempfile::tempdir().unwrap();
    let st = super::branches::open(dir.path());
    st.create("m", DbType::Mem).unwrap();
    let app = router(st.clone());
    let (r, _) = json_req(&app, "POST", "/$/branches/m", json!({ "name": "dev" })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let (r, _) = post(&app, "/$/branches/m/dev/relink", "").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "not-relinkable");
}
