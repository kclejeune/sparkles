//! Write previews (dry runs) over HTTP: updates, Graph Store writes and uploads report
//! the receipt the write gets, with its status, and write nothing.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use std::collections::BTreeMap;
use tower::ServiceExt;

struct Server {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
}

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn header(&self, n: &str) -> Option<String> {
        self.headers.get(n).map(|v| v.to_str().unwrap().to_string())
    }
}

async fn call(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> Resp {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

const UPDATE: &str = "application/sparql-update";
const NQ: &str = "application/n-quads";
const TTL: &str = "text/turtle";

/// A persistent dataset `d` holding a quad in the default graph and one in `urn:g`.
fn server_with(opts: StoreOptions) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::new(dir.path(), opts, Duration::from_secs(30)).unwrap());
    state.attach("d", DbType::Persistent, None).unwrap();
    state
        .get("d")
        .unwrap()
        .store
        .load(&[sparkles::io::Source::from_bytes(
            b"<urn:a> <urn:p> \"1\" .\n<urn:b> <urn:p> \"2\" <urn:g> .\n".to_vec(),
            sparkles::io::RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

fn server() -> Server {
    server_with(StoreOptions::default())
}

impl Server {
    fn ds(&self) -> Arc<Dataset> {
        self.state.get("d").unwrap()
    }
    fn head(&self) -> u64 {
        self.ds().store.head_commit().seq
    }
    /// Every file of the dataset's directory with its bytes, once the change log's
    /// background writer has written what earlier commits queued.
    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        self.ds().store.flush_change_log().unwrap();
        let root = self.ds().store.root().unwrap().to_path_buf();
        let mut out = BTreeMap::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p
                        .strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        out
    }
}

/// A commit without the members only a real commit has.
fn bare(mut c: J) -> J {
    if let Some(m) = c.as_object_mut() {
        m.remove("timestamp");
        m.remove("digest");
    }
    c
}

/// A dry run of a write and then the write itself: the preview's commit equals the
/// receipt's, the per-graph counts add up to it, and the dry run changed no file.
async fn preview_then_write(
    s: &Server,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (J, J) {
    let before = s.files();
    let head = s.head();
    let sep = if path.contains('?') { '&' } else { '?' };
    let p = call(
        &s.app,
        method,
        &format!("{path}{sep}dryRun=true&changes=100"),
        headers,
        body,
    )
    .await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.text());
    assert_eq!(p.header("sparkles-dry-run").as_deref(), Some("true"));
    assert_eq!(p.header("sparkles-commit"), Some(head.to_string()));
    assert_eq!(s.head(), head);
    assert_eq!(s.files(), before, "{method} {path}");
    let p = p.json();
    assert_eq!(
        (&p["dryRun"], &p["committed"]),
        (&json!(true), &json!(false))
    );
    let r = call(
        &s.app,
        method,
        &format!("{path}{sep}receipt=true"),
        headers,
        body,
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = r.json();
    assert_eq!(p["wouldCommit"], r["committed"]);
    assert_eq!(
        bare(p["commit"].clone()),
        bare(r["commit"].clone()),
        "{p} {r}"
    );
    if r["committed"] == json!(true) {
        let sum = |k: &str| -> u64 {
            p["graphs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g[k].as_u64().unwrap())
                .sum()
        };
        assert_eq!(json!(sum("inserted")), r["commit"]["inserted"]);
        assert_eq!(json!(sum("deleted")), r["commit"]["deleted"]);
        if r["commit"]["exact"] == json!(true) {
            assert_eq!(
                p["changes"]["total"].as_u64().unwrap(),
                sum("inserted") + sum("deleted")
            );
        }
    }
    (p, r)
}

/// A small deterministic random source.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

fn random_update(r: &mut Rng, n: usize) -> String {
    let mut ops = Vec::new();
    for _ in 0..1 + r.below(3) {
        let s = r.below(4);
        let o = r.below(4);
        let g = r.below(3);
        let quad = if g == 0 {
            format!("<urn:s{s}> <urn:p> \"{o}\"")
        } else {
            format!("GRAPH <urn:g{g}> {{ <urn:s{s}> <urn:p> \"{o}\" }}")
        };
        ops.push(match r.below(6) {
            0 | 1 => format!(
                "INSERT DATA {{ {quad} . <urn:new{n}> <urn:q> _:x{} }}",
                ops.len()
            ),
            2 => format!("DELETE DATA {{ {quad} }}"),
            3 => format!("DELETE WHERE {{ <urn:s{s}> ?p ?o }}"),
            4 => format!("CLEAR SILENT GRAPH <urn:g{g}>"),
            _ => "INSERT { GRAPH <urn:copy> { ?s ?p ?o } } WHERE { ?s <urn:p> ?o }".into(),
        });
    }
    ops.join(" ;\n")
}

#[tokio::test]
async fn update_previews_equal_receipts() {
    let s = server();
    let mut r = Rng(0x5eed_1234_abcd_0001);
    let mut committed = 0;
    for i in 0..60 {
        let u = random_update(&mut r, i);
        let (_, rec) =
            preview_then_write(&s, "POST", "/d/update", &[("content-type", UPDATE)], &u).await;
        committed += (rec["committed"] == json!(true)) as usize;
    }
    assert!(committed > 30, "{committed}");
    // the change feed has nothing a dry run made
    let head = s.head();
    let p = call(
        &s.app,
        "POST",
        "/d/update?dryRun",
        &[("content-type", UPDATE)],
        "INSERT DATA { <urn:z> <urn:p> 1 }",
    )
    .await;
    assert_eq!(p.status, StatusCode::OK);
    let c = call(&s.app, "GET", &format!("/d/changes?after={head}"), &[], "").await;
    assert_eq!(c.json()["commits"], json!([]), "{}", c.text());
}

#[tokio::test]
async fn the_preview_document() {
    let s = server();
    let head = s.head();
    let p = call(
        &s.app,
        "POST",
        "/d/update?dryRun=true&changes=10",
        &[
            ("content-type", UPDATE),
            ("sparkles-commit-message", "fix the values"),
        ],
        "DELETE DATA { <urn:a> <urn:p> \"1\" } ; INSERT DATA { <urn:a> <urn:p> \"2\" . GRAPH <urn:g9> { <urn:b> <urn:p> \"3\" } }",
    )
    .await;
    assert_eq!(p.status, StatusCode::OK);
    assert_eq!(
        p.header("sparkles-dry-run-outcome").as_deref(),
        Some("commit")
    );
    let j = p.json();
    assert_eq!(j["outcome"], "commit");
    assert_eq!(j["head"], head);
    assert_eq!(j["wouldCommit"], true);
    let c = &j["commit"];
    assert_eq!(
        (
            &c["seq"],
            &c["parent"],
            &c["kind"],
            &c["inserted"],
            &c["deleted"],
            &c["message"]
        ),
        (
            &json!(head + 1),
            &json!(head),
            &json!("update"),
            &json!(2),
            &json!(1),
            &json!("fix the values")
        )
    );
    assert!(c.get("timestamp").is_none());
    assert_eq!(
        j["graphs"],
        json!([
            { "graph": null, "inserted": 1, "deleted": 1 },
            { "graph": "urn:g9", "inserted": 1, "deleted": 0 },
        ])
    );
    let ch = &j["changes"];
    assert_eq!((&ch["total"], &ch["truncated"]), (&json!(3), &json!(false)));
    assert_eq!(ch["quads"][0]["op"], "-");
    assert_eq!(ch["quads"][0]["object"], "\"1\"");
    assert_eq!(j["storage"]["status"], "fits");
    assert!(j.get("validation").is_none() && j.get("precondition").is_none());
    // a write without net effect
    let p = call(
        &s.app,
        "POST",
        "/d/update?dryRun=true",
        &[("content-type", UPDATE)],
        "INSERT DATA { <urn:a> <urn:p> \"1\" }",
    )
    .await;
    let j = p.json();
    assert_eq!(
        (&j["outcome"], &j["wouldCommit"]),
        (&json!("no-change"), &json!(false))
    );
    assert_eq!(j["commit"]["seq"], head);
    assert_eq!(j["graphs"], json!([]));
    assert_eq!(s.head(), head);
}

#[tokio::test]
async fn flags_and_their_errors() {
    let s = server();
    let head = s.head();
    let ins = "INSERT DATA { <urn:x> <urn:p> 1 }";
    // the form, the header, and an empty value
    let form = format!("update={}&dryRun=true", urlencode(ins));
    for (uri, headers, body) in [
        (
            "/d/update",
            vec![("content-type", "application/x-www-form-urlencoded")],
            form.as_str(),
        ),
        (
            "/d/update",
            vec![("content-type", UPDATE), ("sparkles-dry-run", "true")],
            ins,
        ),
        ("/d/update?dryRun", vec![("content-type", UPDATE)], ins),
        ("/d?dryRun=1", vec![("content-type", UPDATE)], ins),
    ] {
        let r = call(&s.app, "POST", uri, &headers, body).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert_eq!(r.json()["dryRun"], true, "{uri}");
    }
    // a misspelt flag never writes
    for (uri, headers) in [
        ("/d/update?dryRun=ture", vec![("content-type", UPDATE)]),
        (
            "/d/update",
            vec![("content-type", UPDATE), ("sparkles-dry-run", "yes please")],
        ),
        (
            "/d/update?dryRun=true&changes=10001",
            vec![("content-type", UPDATE)],
        ),
        (
            "/d/update?dryRun=true&changes=2",
            vec![
                ("content-type", UPDATE),
                ("accept", "application/rdf-patch"),
            ],
        ),
    ] {
        let r = call(&s.app, "POST", uri, &headers, ins).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uri}: {}", r.text());
    }
    // dryRun=false is an ordinary write
    let r = call(
        &s.app,
        "POST",
        "/d/update?dryRun=false",
        &[("content-type", UPDATE)],
        ins,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("sparkles-dry-run").is_none());
    assert_eq!(s.head(), head + 1);
}

fn urlencode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[tokio::test]
async fn graph_store_writes_and_uploads() {
    for bulk in [false, true] {
        let s = server_with(StoreOptions {
            bulk_threshold: if bulk { 5 } else { 1_000_000 },
            ..Default::default()
        });
        let body: String = (0..12)
            .map(|i| format!("<urn:x{i}> <urn:p> \"v{i}\" .\n"))
            .collect();
        let half: String = (6..18)
            .map(|i| format!("<urn:x{i}> <urn:p> \"v{i}\" .\n"))
            .collect();
        let (p, _) = preview_then_write(
            &s,
            "PUT",
            "/d/data?graph=urn:g",
            &[("content-type", TTL)],
            &body,
        )
        .await;
        assert_eq!(p["commit"]["kind"], "gsp-put");
        assert_eq!(p["commit"]["bulk"], json!(bulk));
        let (p, _) = preview_then_write(
            &s,
            "PUT",
            "/d/data?graph=urn:g",
            &[("content-type", TTL)],
            &half,
        )
        .await;
        // only the difference is listed
        assert_eq!(p["changes"]["total"], 12, "{p}");
        let (p, _) = preview_then_write(
            &s,
            "POST",
            "/d/data?default",
            &[("content-type", TTL)],
            &body,
        )
        .await;
        assert_eq!(p["commit"]["kind"], "gsp-post");
        let quads = "<urn:q> <urn:p> \"q\" <urn:g2> .\n<urn:q> <urn:p> \"r\" .\n";
        let (p, _) =
            preview_then_write(&s, "POST", "/d/upload", &[("content-type", NQ)], quads).await;
        assert_eq!(p["commit"]["kind"], "upload");
        let (p, _) = preview_then_write(&s, "DELETE", "/d/data?graph=urn:g", &[], "").await;
        assert_eq!(p["commit"]["kind"], "gsp-delete");
        assert_eq!(
            p["graphs"],
            json!([{ "graph": "urn:g", "inserted": 0, "deleted": 12 }])
        );
        let (p, _) = preview_then_write(&s, "PUT", "/d/data", &[("content-type", NQ)], quads).await;
        assert_eq!(p["commit"]["kind"], "gsp-put");
        // a missing graph is missing in a dry run too
        let r = call(&s.app, "DELETE", "/d/data?graph=urn:none&dryRun", &[], "").await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn rdf_patches() {
    let s = server();
    let head = s.head();
    let id = s.ds().store.dataset_id();
    let r = call(
        &s.app,
        "POST",
        "/d/update?dryRun=true",
        &[
            ("content-type", UPDATE),
            ("accept", "application/rdf-patch"),
        ],
        "DELETE DATA { <urn:a> <urn:p> \"1\" } ; INSERT DATA { <urn:c> <urn:p> \"3\" }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.header("content-type")
            .unwrap()
            .starts_with("application/rdf-patch")
    );
    assert_eq!(
        r.header("sparkles-dry-run-outcome").as_deref(),
        Some("commit")
    );
    let want = format!(
        "H id <urn:uuid:{id}#commit:{}> .\nH prev <urn:uuid:{id}#commit:{head}> .\nTX .\nD <urn:a> <urn:p> \"1\" .\nA <urn:c> <urn:p> \"3\" .\nTC .\n",
        head + 1
    );
    assert_eq!(r.text(), want);
    // the binary form
    let r = call(
        &s.app,
        "PUT",
        "/d/data?graph=urn:g&dryRun",
        &[
            ("content-type", TTL),
            ("accept", "application/rdf-patch+thrift"),
        ],
        "<urn:n> <urn:p> 1 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.header("content-type").as_deref(),
        Some("application/rdf-patch+thrift")
    );
    assert!(!r.body.is_empty());
    assert_eq!(s.head(), head);
}

#[tokio::test]
async fn preconditions_are_reported_with_the_rest() {
    let s = server();
    let tag = call(&s.app, "GET", "/d/data?graph=urn:g", &[], "")
        .await
        .header("etag")
        .unwrap();
    let r = call(
        &s.app,
        "PUT",
        "/d/data?graph=urn:g&dryRun=true",
        &[("content-type", TTL), ("if-match", &tag)],
        "<urn:n> <urn:p> 1 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["precondition"], json!({ "status": "passed" }));
    call(
        &s.app,
        "POST",
        "/d/update",
        &[("content-type", UPDATE)],
        "INSERT DATA { <urn:later> <urn:p> 1 }",
    )
    .await;
    let r = call(
        &s.app,
        "PUT",
        "/d/data?graph=urn:g&dryRun=true",
        &[("content-type", TTL), ("if-match", &tag)],
        "<urn:n> <urn:p> 1 .",
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED, "{}", r.text());
    let j = r.json();
    assert_eq!(
        (&j["outcome"], &j["code"], &j["precondition"]["status"]),
        (
            &json!("precondition-failed"),
            &json!("precondition-failed"),
            &json!("failed")
        )
    );
    // the rest of the preview is there
    assert_eq!(j["commit"]["inserted"], 1);
    assert_eq!(j["graphs"][0]["graph"], "urn:g");
}

#[tokio::test]
async fn the_quota_is_reported() {
    let s = server();
    let store = &s.ds().store;
    let used = store.disk_usage();
    store.set_quota(Some(used + 10)).unwrap();
    let r = call(
        &s.app,
        "POST",
        "/d/update?dryRun=true",
        &[("content-type", UPDATE)],
        "INSERT DATA { <urn:big> <urn:p> 1 }",
    )
    .await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    let j = r.json();
    assert_eq!(j["outcome"], "storage-refused");
    assert_eq!(j["budget"], "dataset-bytes");
    assert_eq!(j["storage"]["status"], "refused");
    assert_eq!(j["storage"]["limit"], used + 10);
    assert!(j["storage"]["projected"].as_u64().unwrap() > used + 10);
    // the real write gets the same refusal
    let real = call(
        &s.app,
        "POST",
        "/d/update",
        &[("content-type", UPDATE)],
        "INSERT DATA { <urn:big> <urn:p> 1 }",
    )
    .await;
    assert_eq!(real.status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(real.json()["error"], j["error"]);
    // deletes fit
    let r = call(
        &s.app,
        "POST",
        "/d/update?dryRun=true",
        &[("content-type", UPDATE)],
        "DELETE DATA { <urn:a> <urn:p> \"1\" }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn read_only_servers_refuse_dry_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.read_only = true;
    let st = Arc::new(st);
    st.attach("d", DbType::Persistent, None).unwrap();
    let app = router(st);
    let r = call(
        &app,
        "POST",
        "/d/update?dryRun=true",
        &[("content-type", UPDATE)],
        "INSERT DATA { <urn:a> <urn:p> 1 }",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[cfg(feature = "shacl")]
#[tokio::test]
async fn validation_is_reported_and_the_guard_left_alone() {
    let s = server();
    let shapes = r#"@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path ex:name ; sh:minCount 1 ] ."#;
    for mode in ["reject", "warn"] {
        let cfg = json!({ "mode": mode, "shapes": { "inline": shapes, "format": "text/turtle" } });
        let r = call(
            &s.app,
            "PUT",
            "/$/validation/d",
            &[("content-type", "application/json")],
            &cfg.to_string(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let status = || async {
            let mut j = call(&s.app, "GET", "/$/validation/d", &[], "").await.json();
            j["status"]
                .as_object_mut()
                .unwrap()
                .remove("lastFullMillis");
            j
        };
        let before = status().await;
        let bad = "INSERT DATA { <http://ex.org/x> a <http://ex.org/Person> }";
        let r = call(
            &s.app,
            "POST",
            "/d/update?dryRun=true",
            &[("content-type", UPDATE)],
            bad,
        )
        .await;
        let j = r.json();
        if mode == "reject" {
            assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{j}");
            assert_eq!(j["outcome"], "rejected");
            assert_eq!(j["validation"]["status"], "rejected");
            assert!(j["error"].as_str().unwrap().contains("blocking result"));
            assert_eq!(j["commit"]["inserted"], 1);
        } else {
            assert_eq!(r.status, StatusCode::OK, "{j}");
            assert_eq!(j["validation"]["status"], "warned");
        }
        assert!(
            r.header("sparkles-validation")
                .unwrap()
                .starts_with(&format!(
                    "status={}",
                    if mode == "reject" {
                        "rejected"
                    } else {
                        "warned"
                    }
                ))
        );
        assert_eq!(status().await, before);
        // the real write gets the summary the preview had
        let real = call(
            &s.app,
            "POST",
            "/d/update?receipt=true",
            &[("content-type", UPDATE)],
            bad,
        )
        .await;
        let mut want = real.json()["validation"].take();
        let mut got = j["validation"].clone();
        for x in [&mut want, &mut got] {
            if let Some(m) = x.as_object_mut() {
                m.remove("millis");
            }
        }
        assert_eq!(got, want);
        call(&s.app, "DELETE", "/$/validation/d", &[], "").await;
        call(
            &s.app,
            "POST",
            "/d/update",
            &[("content-type", UPDATE)],
            "DELETE WHERE { <http://ex.org/x> ?p ?o }",
        )
        .await;
    }
}
