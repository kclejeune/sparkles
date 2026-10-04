//! Branches and merges over HTTP: the acceptance examples of F09 that the router
//! serves (create and isolate, the path form, merges and conflicts, protection,
//! deletion, inherited commits, diffs across branches, clones of a branch).

use super::*;
use serde_json::json;
use std::path::Path;

fn open(dir: &Path) -> Arc<AppState> {
    Arc::new(AppState::new(dir, StoreOptions::default(), Duration::from_secs(30)).unwrap())
}

async fn update(app: &Router, ds: &str, text: &str) -> (Resp, axum::http::HeaderMap) {
    let path = match ds.split_once('?') {
        Some((d, q)) => format!("/{d}/update?{q}"),
        None => format!("/{ds}/update"),
    };
    send_h(
        app,
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from(text.to_string()))
            .unwrap(),
    )
    .await
}

/// The subjects of `?s <urn:p> ?o` and `?s <urn:age> ?o`, sorted.
async fn subjects(app: &Router, path: &str) -> Vec<String> {
    let r = send(
        app,
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::from("SELECT ?s { ?s ?p ?o } ORDER BY ?s"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
    let mut v: Vec<String> = r.json()["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["s"]["value"].as_str().unwrap().to_string())
        .collect();
    v.dedup();
    v
}

async fn json_req(
    app: &Router,
    method: &str,
    path: &str,
    body: J,
) -> (Resp, axum::http::HeaderMap) {
    send_h(
        app,
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn get(app: &Router, path: &str) -> (Resp, axum::http::HeaderMap) {
    send_h(app, Request::get(path).body(Body::empty()).unwrap()).await
}

/// A persistent `ds` with commits 1 (`<urn:a> <urn:age> 30`) and 2 (`<urn:b>`).
async fn setup(dir: &Path) -> (Arc<AppState>, Router) {
    let st = open(dir);
    st.create("ds", DbType::Persistent).unwrap();
    let app = router(st.clone());
    let (r, _) = update(&app, "ds", "INSERT DATA { <urn:a> <urn:age> 30 }").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    update(&app, "ds", "INSERT DATA { <urn:b> <urn:name> \"B\" }").await;
    (st, app)
}

#[tokio::test]
async fn a1_a2_create_isolate_and_the_path_form() {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    let (r, h) = json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(h["location"], "/$/branches/ds/dev");
    let b = r.json();
    assert_eq!(b["from"]["branch"], "main");
    assert_eq!(b["from"]["seq"], 2);
    assert_eq!(b["storage"]["linked"], true);
    let id = b["id"].as_str().unwrap().to_string();
    let bdir = dir.path().join("databases/ds/branches").join(&id);
    assert!(bdir.join("gen-0001/link.json").exists());

    let (r, h) = update(&app, "ds?branch=dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(commit_header(&h), 3);
    assert_eq!(h["sparkles-branch"], "dev");
    assert_eq!(h["sparkles-branch-id"], id.as_str());
    let (_, h) = update(&app, "ds", "INSERT DATA { <urn:d> <urn:p> 1 }").await;
    assert_eq!(commit_header(&h), 3);
    assert!(h.get("sparkles-branch").is_none());

    assert_eq!(
        subjects(&app, "/ds/sparql").await,
        ["urn:a", "urn:b", "urn:d"]
    );
    assert_eq!(
        subjects(&app, "/ds/sparql?branch=dev").await,
        ["urn:a", "urn:b", "urn:c"]
    );
    // the path form
    assert_eq!(
        subjects(&app, "/ds@dev/sparql").await,
        ["urn:a", "urn:b", "urn:c"]
    );
    let (r, _) = get(&app, "/ds@dev/sparql?query=ASK%7B%7D&branch=main").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let (r, _) = get(&app, "/ds@nope/sparql?query=ASK%7B%7D").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    assert_eq!(r.json()["code"], "no-such-branch");
    let (r, h) = get(&app, "/ds@dev/data?default").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let etag = h["etag"].to_str().unwrap();
    assert!(etag.starts_with(&format!("W/\"{id}:3:")), "{etag}");
    // the dataset id stays the dataset's
    let main_id = st.get("ds").unwrap().store.dataset_id().to_string();
    assert_eq!(h["sparkles-dataset-id"], main_id.as_str());

    // the branch's history reaches back through main's
    let (r, _) = get(&app, "/$/commits/ds?branch=dev").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let commits: Vec<(u64, String)> = r.json()["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["seq"].as_u64().unwrap(),
                c["branch"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        commits,
        [
            (3, "dev".to_string()),
            (2, "main".into()),
            (1, "main".into()),
            (0, "main".into())
        ]
    );
    // the listing, and the dataset's branch count
    let (r, _) = get(&app, "/$/branches/ds").await;
    let list = r.json();
    let names: Vec<&str> = list["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["main", "dev"]);
    let (r, _) = get(&app, "/$/datasets/ds").await;
    assert_eq!(r.json()["branches"], 2);
    // an admin route that takes no branch refuses one
    let (r, _) = get(&app, "/$/quota/ds?branch=dev").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn a3_a4_fast_forward_and_three_way_merges() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    let (r, _) = get(&app, "/$/merge/ds?source=dev&target=main").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let p = r.json();
    assert_eq!(p["fastForward"], true);
    assert_eq!(p["changes"]["inserted"], 1);
    assert_eq!(p["merged"], false);
    let (r, h) = json_req(&app, "POST", "/$/merge/ds", json!({ "source": "dev" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let m = r.json();
    assert_eq!(m["merged"], true);
    assert_eq!(m["commit"]["kind"], "merge");
    assert_eq!(m["commit"]["seq"], 3);
    assert_eq!(m["commit"]["mergedFrom"]["branch"], "dev");
    assert_eq!(m["commit"]["mergedFrom"]["seq"], 3);
    assert_eq!(commit_header(&h), 3);
    assert_eq!(
        subjects(&app, "/ds/sparql").await,
        subjects(&app, "/ds@dev/sparql").await
    );
    let (r, _) = json_req(&app, "POST", "/$/merge/ds", json!({ "source": "dev" })).await;
    assert_eq!(r.json()["upToDate"], true);
    // the merge commit in main's history
    let (r, _) = get(&app, "/$/commits/ds?limit=1").await;
    let c = &r.json()["commits"][0];
    assert_eq!(c["mergedFrom"]["branch"], "dev");

    // a three-way merge
    update(&app, "ds@dev", "INSERT DATA { <urn:e> <urn:p> 1 }").await;
    update(&app, "ds", "INSERT DATA { <urn:f> <urn:p> 1 }").await;
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "ff": "only" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "not-fast-forward");
    let (r, _) = json_req(&app, "POST", "/$/merge/ds", json!({ "source": "dev" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["fastForward"], false);
    let s = subjects(&app, "/ds/sparql").await;
    for x in ["urn:c", "urn:e", "urn:f"] {
        assert!(s.contains(&x.to_string()), "{x}: {s:?}");
    }
}

#[tokio::test]
async fn a5_a7_conflicts_resolutions_and_stale_heads() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
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
    let (r, _) = json_req(&app, "POST", "/$/merge/ds", json!({ "source": "dev" })).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let c = r.json();
    assert_eq!(c["code"], "merge-conflict");
    assert_eq!(c["conflicts"], 1);
    let cell = &c["cells"][0];
    assert_eq!(cell["subject"], "<urn:a>");
    assert_eq!(cell["predicate"], "<urn:age>");
    assert_eq!(cell["graph"], J::Null);
    let int = |n: u32| format!("\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>");
    assert_eq!(cell["base"], json!([int(30)]));
    assert_eq!(cell["ours"], json!([int(31)]));
    assert_eq!(cell["theirs"], json!([int(32)]));
    // the preview reports the same without failing
    let (r, _) = get(&app, "/$/merge/ds?source=dev").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["conflictCount"], 1);
    // stale heads
    let expect = json!({ "source": c["source"]["seq"], "target": c["target"]["seq"] });
    update(&app, "ds", "INSERT DATA { <urn:g> <urn:p> 1 }").await;
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "expect": expect, "onConflict": "theirs" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json()["code"], "head-moved");
    // a resolution that covers no conflict
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "resolutions": [
            { "graph": null, "subject": "<urn:b>", "take": "ours" }
        ] }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["code"], "invalid-merge");
    // a cell resolved to given objects
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "resolutions": [
            { "graph": null, "subject": "<urn:a>", "predicate": "<urn:age>",
              "take": "objects", "objects": ["33"] }
        ] }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["conflicts"]["resolved"], 1);
    let r = send(
        &app,
        Request::get(
            "/ds/sparql?query=SELECT%20%3Fo%20%7B%3Curn%3Aa%3E%20%3Curn%3Aage%3E%20%3Fo%7D",
        )
        .header(header::ACCEPT, "application/sparql-results+json")
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    let v = r.json();
    let b = v["results"]["bindings"].as_array().unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0]["o"]["value"], "33");
}

#[tokio::test]
async fn a11_a12_protection_and_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    let (r, _) = json_req(
        &app,
        "PATCH",
        "/$/branches/ds/main",
        json!({ "protected": true }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["protected"], true);
    let (r, _) = update(&app, "ds", "INSERT DATA { <urn:x> <urn:p> 1 }").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["code"], "branch-protected");
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    let (r, _) = json_req(&app, "POST", "/$/merge/ds", json!({ "source": "dev" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    json_req(
        &app,
        "PATCH",
        "/$/branches/ds/main",
        json!({ "protected": false, "note": "open" }),
    )
    .await;
    // deletion: unmerged, forced, main, children
    update(&app, "ds@dev", "INSERT DATA { <urn:d> <urn:p> 1 }").await;
    let del = |path: &'static str| {
        let app = app.clone();
        async move { send(&app, Request::delete(path).body(Body::empty()).unwrap()).await }
    };
    let r = del("/$/branches/ds/dev").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(r.json()["code"], "unmerged");
    let r = del("/$/branches/ds/main").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    json_req(
        &app,
        "POST",
        "/$/branches/ds",
        json!({ "name": "child", "from": "dev" }),
    )
    .await;
    let r = del("/$/branches/ds/dev?force=true").await;
    assert_eq!(r.json()["code"], "has-children");
    let r = del("/$/branches/ds/child?force=true").await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = del("/$/branches/ds/dev?force=true").await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let (r, _) = get(&app, "/$/branches/ds/dev").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        std::fs::read_dir(dir.path().join("databases/ds/branches"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn a14_a20_a19_inherited_commits_diffs_and_clones() {
    let dir = tempfile::tempdir().unwrap();
    let (st, app) = setup(dir.path()).await;
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    update(&app, "ds@dev", "INSERT DATA { <urn:c> <urn:p> 1 }").await;
    update(&app, "ds", "INSERT DATA { <urn:d> <urn:p> 1 }").await;
    // commit 1 of dev is main's
    assert_eq!(
        subjects(&app, "/ds/sparql?branch=dev&at=1").await,
        ["urn:a"]
    );
    // the diff from main's head to dev's
    let (r, _) = get(&app, "/ds/diff?fromBranch=main&toBranch=dev&quads=true").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let d = r.json();
    assert_eq!(d["added"], 1);
    assert_eq!(d["removed"], 1);
    let ops: Vec<String> = d["quads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| {
            format!(
                "{}{}",
                q["op"].as_str().unwrap(),
                q["subject"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(ops, ["-<urn:d>", "+<urn:c>"]);
    // a clone of the branch at its commit 3
    let (r, _) = send_h(
        &app,
        Request::post("/$/datasets/ds/clone?name=x&branch=dev&at=3")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_string();
    let t0 = std::time::Instant::now();
    loop {
        let t = st
            .tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .unwrap();
        if t.state != "running" && t.state != "queued" {
            assert_eq!(t.state, "done", "{t:?}");
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        subjects(&app, "/x/sparql").await,
        ["urn:a", "urn:b", "urn:c"]
    );
    let (r, _) = get(&app, "/$/datasets/x").await;
    let dev_id = st
        .get("ds")
        .unwrap()
        .store
        .branch_id_of("dev")
        .unwrap()
        .to_string();
    assert_eq!(r.json()["forkedFrom"]["id"], dev_id.as_str());
    assert_eq!(r.json()["forkedFrom"]["seq"], 3);
}

/// A10: the target's write-time validation refuses a merge, and a dry run reports it.
#[cfg(feature = "shacl")]
#[tokio::test]
async fn a10_validation_refuses_a_merge_and_a_dry_run_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let (_st, app) = setup(dir.path()).await;
    // dev starts before main gets its guard, so dev has none
    json_req(&app, "POST", "/$/branches/ds", json!({ "name": "dev" })).await;
    let shapes = "@prefix sh: <http://www.w3.org/ns/shacl#> .
<urn:PersonShape> a sh:NodeShape ; sh:targetClass <urn:Person> ;
  sh:property [ sh:path <urn:name> ; sh:minCount 1 ; sh:maxCount 1 ] .";
    let (r, _) = json_req(
        &app,
        "PUT",
        "/$/validation/ds",
        json!({ "mode": "reject", "shapes": { "inline": shapes } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let (r, _) = update(&app, "ds@dev", "INSERT DATA { <urn:p1> a <urn:Person> }").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let head = |app: Router| async move {
        get(&app, "/$/commits/ds?limit=1").await.0.json()["head"].clone()
    };
    let before = head(app.clone()).await;
    let (r, _) = json_req(&app, "POST", "/$/merge/ds", json!({ "source": "dev" })).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text());
    assert_eq!(head(app.clone()).await, before, "nothing is committed");
    let (r, _) = json_req(
        &app,
        "POST",
        "/$/merge/ds",
        json!({ "source": "dev", "dryRun": true }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let p = r.json();
    assert_eq!(p["dryRun"], true);
    assert_eq!(p["outcome"], "rejected", "{p:#}");
    assert_eq!(p["merge"]["fastForward"], true);
    assert_eq!(head(app.clone()).await, before);
}
