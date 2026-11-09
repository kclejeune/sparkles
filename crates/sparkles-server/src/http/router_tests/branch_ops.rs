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
async fn authoring_revert_pins_the_target_branch_incarnation() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "draft" })).await;
    update(&app, "ds@draft", "INSERT DATA { <urn:old> <urn:p> 1 }").await;
    let (old, _) = get(&app, "/$/branches/ds/draft").await;
    let old_id = old.json()["id"].as_str().unwrap().to_string();
    let (deleted, _) = json_req(&app, "DELETE", "/$/branches/ds/draft?force=true", json!({})).await;
    assert!(deleted.status.is_success(), "{}", deleted.text());
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "draft" })).await;
    update(&app, "ds@draft", "INSERT DATA { <urn:new> <urn:p> 1 }").await;
    let (new, _) = get(&app, "/$/branches/ds/draft").await;
    let new = new.json();
    let head = new["head"].as_u64().unwrap();
    let before = subjects(&app, "/ds@draft/sparql").await;
    for dry in [false, true] {
        let (r, _) = json_req(
            &app,
            "POST",
            &format!("/$/revert/ds?branch=draft&commit={head}"),
            json!({ "dryRun": dry, "expect": { "target": head, "targetId": old_id } }),
        )
        .await;
        assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.text());
        assert_eq!(subjects(&app, "/ds@draft/sparql").await, before);
    }
    let (invalid, _) = json_req(
        &app,
        "POST",
        &format!("/$/revert/ds?branch=draft&commit={head}"),
        json!({ "expect": { "target": head, "targetId": "not-a-uuid" } }),
    )
    .await;
    assert_eq!(
        invalid.status,
        StatusCode::BAD_REQUEST,
        "{}",
        invalid.text()
    );
    let (valid, _) = json_req(
        &app,
        "POST",
        &format!("/$/revert/ds?branch=draft&commit={head}"),
        json!({ "expect": { "target": head, "targetId": new["id"] } }),
    )
    .await;
    assert_eq!(valid.status, StatusCode::OK, "{}", valid.text());
    assert!(
        !subjects(&app, "/ds@draft/sparql")
            .await
            .contains(&"urn:new".to_string())
    );
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

async fn consult_malformed_expect(operation: &str) {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let ds = st.get("ds").unwrap();
    let store = &ds.store;
    store
        .create_branch("source", &sparkles::branch::BranchOptions::default())
        .unwrap();
    let (r, _) = update(
        &app,
        "ds@source",
        "INSERT DATA { <urn:source-change> <urn:p> 1 }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let source_head = store.branch("source").unwrap().head_commit().seq;
    let malformed = [
        json!("0"),
        json!(-1),
        json!(false),
        json!(1.5),
        json!([]),
        json!({}),
        J::Null,
    ];
    let mut failures = Vec::new();
    let mut index = 0;
    // Each case owns a fresh target, so an old-code write cannot poison a later case.
    for dry in [true, false] {
        for field in if operation == "merge" {
            vec!["target", "source", "targetId", "container"]
        } else {
            vec!["target", "targetId", "container"]
        } {
            for bad in &malformed {
                // An empty expect object omits every optional field and is valid.
                if field == "container" && bad.is_object() {
                    continue;
                }
                index += 1;
                let target = format!("case-{index}");
                store
                    .create_branch(&target, &sparkles::branch::BranchOptions::default())
                    .unwrap();
                if operation == "revert" {
                    let (r, _) = update(
                        &app,
                        &format!("ds@{target}"),
                        "INSERT DATA { <urn:target-change> <urn:p> 1 }",
                    )
                    .await;
                    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
                }
                let target_store = store.branch(&target).unwrap();
                let head = target_store.head_commit().seq;
                let before = subjects(&app, &format!("/ds@{target}/sparql")).await;
                let mut expect = json!({ "targetId": target_store.dataset_id() });
                if field == "container" {
                    expect = bad.clone();
                } else {
                    expect[field] = bad.clone();
                }
                let mut body = json!({ "dryRun": dry, "expect": expect });
                let uri = match operation {
                    "merge" => {
                        body["source"] = json!("source");
                        body["target"] = json!(target);
                        "/$/merge/ds".to_string()
                    }
                    "cherry-pick" => format!(
                        "/$/cherry-pick/ds?source=source&branch={target}&commit={source_head}"
                    ),
                    _ => format!("/$/revert/ds?branch={target}&commit={head}"),
                };
                let (r, _) = json_req(&app, "POST", &uri, body).await;
                let after = subjects(&app, &format!("/ds@{target}/sparql")).await;
                if r.status != StatusCode::BAD_REQUEST
                    || r.json()["code"] != "invalid-merge"
                    || target_store.head_commit().seq != head
                    || after != before
                {
                    failures.push(format!("{operation} dry={dry} {field}={bad}: status={} body={} head {head}->{} changed={}",
                        r.status, r.text(), target_store.head_commit().seq, after != before));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "explicit expectations were erased: {failures:#?}"
    );
}

#[tokio::test]
async fn authoring_consult_revert_malformed_expect_refuses_without_changes() {
    consult_malformed_expect("revert").await;
}
#[tokio::test]
async fn authoring_consult_cherry_malformed_expect_refuses_without_changes() {
    consult_malformed_expect("cherry-pick").await;
}
#[tokio::test]
async fn authoring_consult_merge_malformed_expect_refuses_without_changes() {
    consult_malformed_expect("merge").await;
}

#[cfg(feature = "shacl")]
async fn consult_serving_store_header(schedule: &str) {
    use crate::http::algo::probe::{Gate, Probe, Release};
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let ds = st.get("ds").unwrap();
    for name in ["parked", "draft"] {
        ds.store
            .create_branch(name, &sparkles::branch::BranchOptions::default())
            .unwrap();
    }
    assert_eq!(
        update(
            &app,
            "ds@parked",
            "INSERT DATA { <urn:focus> <urn:p> <urn:a> }"
        )
        .await
        .0
        .status,
        StatusCode::OK
    );
    assert_eq!(
        update(
            &app,
            "ds@draft",
            "INSERT DATA { <urn:focus> <urn:q> <urn:b> }"
        )
        .await
        .0
        .status,
        StatusCode::OK
    );
    let a = ds.store.branch("parked").unwrap();
    let b = ds.store.branch("draft").unwrap();
    let expected_a = a.dataset_id().to_string();
    let serving_b = b.dataset_id().to_string();
    let head = b.head_commit().seq;
    assert_eq!(a.head_commit().seq, head);
    assert_ne!(expected_a, serving_b);
    let seq = if schedule == "inherited" { 1 } else { head };
    let gate = Arc::new(Gate::default());
    let release = Release(gate.clone());
    let probe = Probe {
        branch_response: Some(gate.clone()),
        ..Default::default()
    };
    let req = Request::post(format!("/ds@draft/shacl?at={seq}"))
        .header(header::CONTENT_TYPE, "text/turtle")
        .header(header::ACCEPT, "application/json")
        .extension(probe)
        .body(Body::from("@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> sh:targetNode <urn:focus>; sh:property [ sh:path <urn:p>; sh:minCount 1 ] .")).unwrap();
    let owned_app = app.clone();
    let running = tokio::spawn(async move { send_h(&owned_app, req).await });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("actual SHACL handler did not complete before the response gate");
    match schedule {
        "delete" => ds.store.delete_branch("draft", true).unwrap(),
        "replace" => {
            ds.store.delete_branch("draft", true).unwrap();
            ds.store
                .create_branch("draft", &sparkles::branch::BranchOptions::default())
                .unwrap();
        }
        _ => {
            ds.store.rename_branch("draft", "saved-b").unwrap();
            ds.store.rename_branch("parked", "draft").unwrap();
        }
    }
    gate.release();
    let (r, headers) = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    drop(release);
    assert!(!gate.timed_out.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["conforms"],
        false,
        "the captured B snapshot lacks urn:p"
    );
    assert_eq!(commit_header(&headers), seq);
    assert_eq!(
        headers["sparkles-dataset-id"],
        ds.store.dataset_id().to_string()
    );
    assert_eq!(
        headers
            .get("sparkles-branch-id")
            .and_then(|v| v.to_str().ok()),
        Some(serving_b.as_str()),
        "{schedule}: B's completed report must carry B's serving UUID, never stale expected A={expected_a}; {headers:?}"
    );
    assert_ne!(headers["sparkles-branch-id"], expected_a);
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn authoring_consult_report_identity_survives_aba_rename() {
    consult_serving_store_header("rename").await;
}
#[cfg(feature = "shacl")]
#[tokio::test]
async fn authoring_consult_report_identity_survives_replacement() {
    consult_serving_store_header("replace").await;
}
#[cfg(feature = "shacl")]
#[tokio::test]
async fn authoring_consult_report_identity_survives_deletion() {
    consult_serving_store_header("delete").await;
}
#[cfg(feature = "shacl")]
#[tokio::test]
async fn authoring_consult_report_identity_keeps_branch_for_inherited_at() {
    consult_serving_store_header("inherited").await;
}

async fn consult_valid_expect(operation: &str) {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let ds = st.get("ds").unwrap();
    ds.store
        .create_branch("source", &sparkles::branch::BranchOptions::default())
        .unwrap();
    assert_eq!(
        update(
            &app,
            "ds@source",
            "INSERT DATA { <urn:source-change> <urn:p> 1 }"
        )
        .await
        .0
        .status,
        StatusCode::OK
    );
    let source_head = ds.store.branch("source").unwrap().head_commit().seq;
    let mut cases = vec![
        "zero", "maximum", "wrong-id", "current", "id-only", "empty", "omitted",
    ];
    if operation == "merge" {
        cases.extend(["source-zero", "source-only", "target-only"]);
    }
    let mut index = 0;
    for dry in [true, false] {
        for case in &cases {
            index += 1;
            let name = format!("valid-{index}");
            ds.store
                .create_branch(&name, &sparkles::branch::BranchOptions::default())
                .unwrap();
            if operation == "revert" {
                assert_eq!(
                    update(
                        &app,
                        &format!("ds@{name}"),
                        "INSERT DATA { <urn:target-change> <urn:p> 1 }"
                    )
                    .await
                    .0
                    .status,
                    StatusCode::OK
                );
            }
            let target = ds.store.branch(&name).unwrap();
            let head = target.head_commit().seq;
            let before = subjects(&app, &format!("/ds@{name}/sparql")).await;
            let mut expect = json!({"target": head, "targetId": target.dataset_id()});
            let status = match *case {
                "zero" => {
                    expect["target"] = json!(0);
                    StatusCode::CONFLICT
                }
                "maximum" => {
                    expect["target"] = json!(u64::MAX);
                    StatusCode::CONFLICT
                }
                "source-zero" => {
                    expect["source"] = json!(0);
                    StatusCode::CONFLICT
                }
                "wrong-id" => {
                    expect["targetId"] = json!(ds.store.dataset_id());
                    StatusCode::PRECONDITION_FAILED
                }
                "source-only" => {
                    expect = json!({"source": source_head});
                    StatusCode::OK
                }
                "target-only" => {
                    expect = json!({"target": head});
                    StatusCode::OK
                }
                "id-only" => {
                    expect = json!({"targetId": target.dataset_id()});
                    StatusCode::OK
                }
                "empty" => {
                    expect = json!({});
                    StatusCode::OK
                }
                _ => StatusCode::OK,
            };
            let mut body = json!({"dryRun": dry});
            if *case != "omitted" {
                body["expect"] = expect;
            }
            let uri = match operation {
                "merge" => {
                    body["source"] = json!("source");
                    body["target"] = json!(name);
                    "/$/merge/ds".to_string()
                }
                "cherry-pick" => {
                    format!("/$/cherry-pick/ds?source=source&branch={name}&commit={source_head}")
                }
                _ => format!("/$/revert/ds?branch={name}&commit={head}"),
            };
            let (r, _) = json_req(&app, "POST", &uri, body).await;
            assert_eq!(
                r.status,
                status,
                "{operation} {case} dry={dry}: {}",
                r.text()
            );
            if status == StatusCode::CONFLICT {
                assert_eq!(r.json()["code"], "head-moved");
            }
            let after = subjects(&app, &format!("/ds@{name}/sparql")).await;
            if dry || status != StatusCode::OK {
                assert_eq!(target.head_commit().seq, head);
                assert_eq!(after, before);
            } else {
                assert_eq!(target.head_commit().seq, head + 1);
                assert_ne!(after, before, "the operation must be nonempty");
            }
        }
    }
}

#[tokio::test]
async fn authoring_consult_valid_revert_expectations_keep_contracts() {
    consult_valid_expect("revert").await;
}
#[tokio::test]
async fn authoring_consult_valid_cherry_expectations_keep_contracts() {
    consult_valid_expect("cherry-pick").await;
}
#[tokio::test]
async fn authoring_consult_valid_merge_expectations_keep_contracts() {
    consult_valid_expect("merge").await;
}

async fn consult_same_head_replacement(operation: &str) {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let ds = st.get("ds").unwrap();
    for name in ["draft", "source"] {
        ds.store
            .create_branch(name, &sparkles::branch::BranchOptions::default())
            .unwrap();
    }
    let old = ds.store.branch("draft").unwrap();
    let head = old.head_commit().seq;
    let old_id = old.dataset_id();
    ds.store.rename_branch("draft", "saved").unwrap();
    ds.store
        .create_branch("draft", &sparkles::branch::BranchOptions::default())
        .unwrap();
    let target = ds.store.branch("draft").unwrap();
    assert_eq!(head, target.head_commit().seq);
    assert_ne!(old_id, target.dataset_id());
    assert_eq!(
        update(
            &app,
            "ds@source",
            "INSERT DATA { <urn:source-change> <urn:p> 1 }"
        )
        .await
        .0
        .status,
        StatusCode::OK
    );
    let source_head = ds.store.branch("source").unwrap().head_commit().seq;
    let before = subjects(&app, "/ds@draft/sparql").await;
    for dry in [true, false] {
        let mut body = json!({"dryRun": dry, "expect": {"target": head, "targetId": old_id}});
        let uri = if operation == "merge" {
            body["source"] = json!("source");
            body["target"] = json!("draft");
            "/$/merge/ds".to_string()
        } else {
            format!("/$/cherry-pick/ds?source=source&branch=draft&commit={source_head}")
        };
        let (r, _) = json_req(&app, "POST", &uri, body).await;
        assert_eq!(
            r.status,
            StatusCode::PRECONDITION_FAILED,
            "{operation} dry={dry}: {}",
            r.text()
        );
        assert_eq!(subjects(&app, "/ds@draft/sparql").await, before);
        assert_eq!(target.head_commit().seq, head);
    }
}

#[tokio::test]
async fn authoring_consult_same_head_merge_replacement_refuses() {
    consult_same_head_replacement("merge").await;
}
#[tokio::test]
async fn authoring_consult_same_head_cherry_replacement_refuses() {
    consult_same_head_replacement("cherry-pick").await;
}

async fn consult_http_revert_aba(dry: bool) {
    use crate::http::algo::probe::{Completion, Gate, Release};
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let ds = st.get("ds").unwrap();
    for name in ["parked", "draft"] {
        ds.store
            .create_branch(name, &sparkles::branch::BranchOptions::default())
            .unwrap();
    }
    assert_eq!(
        update(&app, "ds@parked", "INSERT DATA { <urn:a-only> <urn:p> 1 }")
            .await
            .0
            .status,
        StatusCode::OK
    );
    assert_eq!(
        update(&app, "ds@draft", "INSERT DATA { <urn:b-only> <urn:p> 1 }")
            .await
            .0
            .status,
        StatusCode::OK
    );
    let a = ds.store.branch("parked").unwrap();
    let b = ds.store.branch("draft").unwrap();
    let head = a.head_commit().seq;
    assert_eq!(head, b.head_commit().seq);
    let before_a = subjects(&app, "/ds@parked/sparql").await;
    let before_b = subjects(&app, "/ds@draft/sparql").await;
    let gate = Arc::new(Gate::default());
    let release = Release(gate.clone());
    let hook_gate = gate.clone();
    ds.store.set_failpoint(
        "pick-source-resolved",
        Some(Arc::new(move |_| {
            let _completion = Completion(hook_gate.clone());
            hook_gate.wait();
        })),
    );
    let body = json!({"dryRun":dry,"expect":{"target":head,"targetId":a.dataset_id()}});
    let owned_app = app.clone();
    let running = tokio::spawn(async move {
        json_req(
            &owned_app,
            "POST",
            &format!("/$/revert/ds?branch=draft&commit={head}"),
            body,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("actual HTTP revert never reached the lineage barrier");
    ds.store.rename_branch("draft", "saved-b").unwrap();
    ds.store.rename_branch("parked", "draft").unwrap();
    gate.release();
    let (r, _) = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    drop(release);
    ds.store.set_failpoint("pick-source-resolved", None);
    assert!(!gate.timed_out.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        r.status,
        StatusCode::PRECONDITION_FAILED,
        "dry={dry}: {}",
        r.text()
    );
    assert_eq!(a.head_commit().seq, head);
    assert_eq!(b.head_commit().seq, head);
    assert_eq!(subjects(&app, "/ds@draft/sparql").await, before_a);
    assert_eq!(subjects(&app, "/ds@saved-b/sparql").await, before_b);
}

#[tokio::test]
async fn authoring_consult_http_revert_aba_preview_refuses() {
    consult_http_revert_aba(true).await;
}
#[tokio::test]
async fn authoring_consult_http_revert_aba_write_refuses() {
    consult_http_revert_aba(false).await;
}

#[tokio::test]
async fn authoring_consult_http_revert_keeps_inherited_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let ds = st.get("ds").unwrap();
    ds.store
        .create_branch("draft", &sparkles::branch::BranchOptions::default())
        .unwrap();
    let target = ds.store.branch("draft").unwrap();
    let id = target.dataset_id().to_string();
    let head = target.head_commit().seq;
    let before = subjects(&app, "/ds/sparql").await;
    for dry in [true, false] {
        let (r, headers) = json_req(
            &app,
            "POST",
            "/$/revert/ds?branch=draft&commit=2",
            json!({"dryRun":dry,"expect":{"target":head,"targetId":id}}),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(headers["sparkles-branch-id"], id);
        assert_eq!(
            headers["sparkles-dataset-id"],
            ds.store.dataset_id().to_string()
        );
        assert_eq!(commit_header(&headers), if dry { head } else { head + 1 });
    }
    assert_eq!(subjects(&app, "/ds@draft/sparql").await, vec!["urn:a"]);
    assert_eq!(subjects(&app, "/ds/sparql").await, before);
}

#[cfg(feature = "reasoning")]
#[tokio::test]
async fn authoring_consult_branch_entailment_previews_keep_captured_identity() {
    use oxrdf::{GraphName, NamedNode, Quad};

    let data = |store: &sparkles::store::Store| {
        let snapshot = store.snapshot();
        let mut quads = std::collections::BTreeSet::new();
        snapshot
            .for_each_quad(|q| {
                quads.insert(snapshot.quad_to_terms(q).unwrap().to_string());
                Ok(())
            })
            .unwrap();
        quads
    };
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let (loaded, _) = update(
        &app,
        "ds",
        "INSERT DATA { <urn:Manager> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:Employee> . <urn:alice> a <urn:Manager> }",
    ).await;
    assert_eq!(loaded.status, StatusCode::OK, "{}", loaded.text());
    let ds = st.get("ds").unwrap();
    for name in ["draft", "entailment-source"] {
        ds.store
            .create_branch(name, &sparkles::branch::BranchOptions::default())
            .unwrap();
    }
    let target = ds.store.branch("draft").unwrap();
    let source = ds.store.branch("entailment-source").unwrap();
    let owner = ds.store.owner_dataset_id().to_string();
    let id = target.branch_id().to_string();
    let source_id = source.branch_id().to_string();
    assert_ne!(id, owner);
    assert_ne!(id, source_id);
    assert_ne!(source_id, owner);
    let head = target.head_commit().seq;
    let main_head = ds.store.head_commit().seq;
    assert_eq!(head, main_head);
    assert_eq!(source.head_commit().seq, head);
    let before = data(&target);
    let main_before = data(&ds.store);
    assert_eq!(before, main_before);
    assert_eq!(data(&source), before);
    let alice_type = Quad::new(
        NamedNode::new("urn:alice").unwrap(),
        NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#type").unwrap(),
        NamedNode::new("urn:Manager").unwrap(),
        GraphName::DefaultGraph,
    )
    .to_string();
    let schema = Quad::new(
        NamedNode::new("urn:Manager").unwrap(),
        NamedNode::new("http://www.w3.org/2000/01/rdf-schema#subClassOf").unwrap(),
        NamedNode::new("urn:Employee").unwrap(),
        GraphName::DefaultGraph,
    )
    .to_string();
    assert!(before.contains(&alice_type));
    assert!(before.contains(&schema));
    let (deleted, _) = update(
        &app,
        "ds@entailment-source",
        "DELETE DATA { <urn:alice> a <urn:Manager> }",
    )
    .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.text());
    let source_head = source.head_commit().seq;
    assert_eq!(source_head, head + 1);
    let source_before = data(&source);
    let mut expected_source = before.clone();
    assert!(expected_source.remove(&alice_type));
    assert_eq!(source_before, expected_source);
    assert!(source_before.contains(&schema));
    assert_eq!(data(&target), before);
    assert_eq!(data(&ds.store), main_before);
    assert_eq!(target.head_commit().seq, head);
    assert_eq!(ds.store.head_commit().seq, main_head);
    for post in [false, true] {
        let uri = format!("/$/revert/ds?branch=draft&commit={head}");
        let (r, headers) = if post {
            json_req(
                &app,
                "POST",
                &uri,
                json!({"dryRun":true,"entailments":true,"profile":"rdfs-simple",
                    "expect":{"target":head,"targetId":id}}),
            )
            .await
        } else {
            get(
                &app,
                "/$/merge/ds?source=entailment-source&target=draft&entailments=true&profile=rdfs-simple",
            ).await
        };
        assert_eq!(r.status, StatusCode::OK, "post={post}: {}", r.text());
        assert_eq!(headers["sparkles-dataset-id"], owner);
        assert_eq!(headers["sparkles-branch"], "draft");
        assert_eq!(headers["sparkles-branch-id"], id);
        assert_eq!(commit_header(&headers), head);
        let doc = r.json();
        assert_eq!(doc["entailments"]["added"], 0, "{doc}");
        assert_eq!(doc["entailments"]["removed"], 1, "{doc}");
        if post {
            assert_eq!(doc["datasetId"], owner);
            assert_eq!(doc["committed"], false);
        } else {
            assert_eq!(doc["target"]["branch"], "draft", "{doc}");
            assert_eq!(doc["target"]["seq"], head, "{doc}");
            assert_eq!(doc["changes"]["inserted"], 0, "{doc}");
            assert_eq!(doc["changes"]["deleted"], 1, "{doc}");
            assert_eq!(doc["upToDate"], false, "{doc}");
        }
        assert_eq!(target.head_commit().seq, head);
        assert_eq!(ds.store.head_commit().seq, main_head);
        assert_eq!(source.head_commit().seq, source_head);
        assert_eq!(data(&target), before);
        assert_eq!(data(&ds.store), main_before);
        assert_eq!(data(&source), source_before);
    }
}

#[tokio::test]
async fn authoring_consult_response_identity_covers_gsp_query_metadata_and_writes() {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let ds = st.get("ds").unwrap();
    ds.store
        .create_branch("draft", &sparkles::branch::BranchOptions::default())
        .unwrap();
    let target = ds.store.branch("draft").unwrap();
    let id = target.dataset_id().to_string();
    let owner = ds.store.dataset_id().to_string();
    let head = target.head_commit().seq;
    let (r, first) = get(&app, "/ds@draft/data?default").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let etag = first["etag"].clone();
    for req in [
        Request::head("/ds@draft/data?default")
            .body(Body::empty())
            .unwrap(),
        Request::get("/ds/data?branch=draft&default")
            .header(header::IF_NONE_MATCH, etag)
            .body(Body::empty())
            .unwrap(),
        Request::post("/ds@draft/sparql")
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .body(Body::from("SELECT ?s { ?s ?p ?o } ORDER BY ?s"))
            .unwrap(),
        Request::get("/$/prefixes/ds?branch=draft")
            .body(Body::empty())
            .unwrap(),
    ] {
        let (r, headers) = send_h(&app, req).await;
        assert!(
            r.status.is_success() || r.status == StatusCode::NOT_MODIFIED,
            "{}",
            r.text()
        );
        assert_eq!(headers["sparkles-branch-id"], id);
        assert_eq!(headers["sparkles-branch"], "draft");
        assert_eq!(headers["sparkles-dataset-id"], owner);
    }
    for query in [
        "INSERT DATA { <urn:new> <urn:p> 1 }",
        "INSERT DATA { <urn:new> <urn:p> 1 }",
    ] {
        let (r, headers) = update(&app, "ds@draft", query).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(headers["sparkles-branch-id"], id);
        assert_eq!(headers["sparkles-dataset-id"], owner);
        assert_eq!(
            commit_header(&headers),
            head + 1,
            "requested no-op keeps its actual head"
        );
    }
    let (r, _) = get(&app, "/ds@draft/data?graph=urn:missing").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let (r, headers) = get(&app, "/ds/sparql?query=ASK%7B%7D").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(headers.get("sparkles-branch-id").is_none());
}

#[tokio::test]
async fn authoring_consult_protected_picks_keep_identity_and_preview_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let ds = st.get("ds").unwrap();
    for name in ["draft", "source"] {
        ds.store
            .create_branch(name, &sparkles::branch::BranchOptions::default())
            .unwrap();
    }
    assert_eq!(
        update(&app, "ds@draft", "INSERT DATA { <urn:target> <urn:p> 1 }")
            .await
            .0
            .status,
        StatusCode::OK
    );
    assert_eq!(
        update(&app, "ds@source", "INSERT DATA { <urn:source> <urn:p> 1 }")
            .await
            .0
            .status,
        StatusCode::OK
    );
    ds.store.set_branch_protected("draft", true).unwrap();
    let target = ds.store.branch("draft").unwrap();
    let head = target.head_commit().seq;
    let before = subjects(&app, "/ds@draft/sparql").await;
    for dry in [true, false] {
        for operation in ["revert", "cherry-pick"] {
            let uri = if operation == "revert" {
                format!("/$/revert/ds?branch=draft&commit={head}")
            } else {
                format!("/$/cherry-pick/ds?source=source&branch=draft&commit={head}")
            };
            let (r, _) = json_req(
                &app,
                "POST",
                &uri,
                json!({"dryRun":dry,"expect":{"target":head,"targetId":target.dataset_id()}}),
            )
            .await;
            assert_eq!(
                r.status,
                StatusCode::FORBIDDEN,
                "{operation} dry={dry}: {}",
                r.text()
            );
            assert_eq!(r.json()["code"], "branch-protected");
            assert_eq!(target.head_commit().seq, head);
            assert_eq!(subjects(&app, "/ds@draft/sparql").await, before);
        }
    }
    let (preview, headers) = get(&app, &format!("/$/revert/ds?branch=draft&commit={head}")).await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.text());
    assert_eq!(
        headers["sparkles-branch-id"],
        target.dataset_id().to_string()
    );
}
