//! Per-request ceilings: body sizes per request class, streamed Graph Store and upload
//! bodies, free disk space for spooling, requested timeouts and Graph Store GET size.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::codec::Codec;
use sparkles::store::StoreOptions;
use std::io::Write;
use tower::ServiceExt;

struct Resp {
    status: StatusCode,
    body: Vec<u8>,
}

impl Resp {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn error(&self) -> String {
        let j: J =
            serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.text()));
        j["error"].as_str().unwrap_or_default().to_string()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp { status, body }
}

fn server(set: impl FnOnce(&mut AppState)) -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    set(&mut st);
    let st = Arc::new(st);
    st.attach("c", DbType::Mem, None).unwrap();
    let app = router(st.clone());
    (dir, st, app)
}

fn count(st: &AppState) -> u64 {
    st.get("c").unwrap().store.snapshot().len()
}

/// `n` bytes of SPARQL: `text` padded with a comment.
fn padded(text: &str, n: usize) -> String {
    let mut s = format!("{text}\n#");
    s.push_str(&"x".repeat(n.saturating_sub(s.len())));
    s
}

fn post(uri: &str, ct: &str, body: impl Into<Body>) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(body.into())
        .unwrap()
}

/// A body without a declared length, sent in 256-byte chunks.
fn chunked(data: String) -> Body {
    let chunks: Vec<Result<Bytes, std::io::Error>> = data
        .into_bytes()
        .chunks(256)
        .map(|c| Ok(Bytes::copy_from_slice(c)))
        .collect();
    Body::from_stream(futures_util::stream::iter(chunks))
}

const INSERT: &str = "INSERT DATA { <urn:a> <urn:p> 1 }";

#[tokio::test]
async fn bodies_have_a_ceiling_per_request_class() {
    let (_d, st, app) = server(|st| {
        st.limits.max_query_body_bytes = Some(1024);
        st.limits.max_update_body_bytes = Some(2048);
        st.limits.max_admin_body_bytes = Some(1024);
    });
    let q = |n: usize| padded("ASK {}", n);
    let form_q = |n: usize| format!("query={}", padded("ASK%7B%7D", n - 6));
    // queries: declared and streamed lengths
    for uri in ["/c/sparql", "/c/query", "/c", "/c/explain"] {
        let r = send(&app, post(uri, "application/sparql-query", q(1000))).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        let r = send(&app, post(uri, "application/sparql-query", q(1500))).await;
        assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{uri}");
        assert_eq!(
            r.error(),
            "request body exceeds 1.0 KiB (--max-query-body-mb)"
        );
        let r = send(
            &app,
            post(uri, "application/sparql-query", chunked(q(1500))),
        )
        .await;
        assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{uri} (chunked)");
    }
    // a form query to the dataset root gets the query ceiling too
    let r = send(&app, post("/c", FORM, form_q(1500))).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    // updates have their own
    for uri in ["/c/update", "/c"] {
        let r = send(
            &app,
            post(uri, "application/sparql-update", padded(INSERT, 1500)),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        let r = send(
            &app,
            post(
                uri,
                "application/sparql-update",
                chunked(padded(INSERT, 3000)),
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{uri}");
        assert!(r.error().contains("--max-update-body-mb"), "{}", r.error());
    }
    let form_u = format!("update={}", padded("CLEAR%20ALL", 1500));
    let r = send(&app, post("/c", FORM, form_u)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // a compressed update is measured decompressed
    let mut z = Vec::new();
    let mut w = Codec::Zstd.writer(&mut z, None, 1).unwrap();
    w.write_all(padded(INSERT, 100_000).as_bytes()).unwrap();
    w.finish().unwrap();
    assert!(z.len() < 1024);
    let r = send(
        &app,
        Request::post("/c/update")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .header(header::CONTENT_ENCODING, "zstd")
            .body(Body::from(z))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    // admin requests
    let name = "n".repeat(2000);
    for (uri, body) in [
        ("/$/datasets", format!(r#"{{"dbName":"{name}"}}"#)),
        (
            "/c/prefixes",
            format!(r#"{{"prefix":"p","uri":"urn:{name}"}}"#),
        ),
        (
            "/$/history/c",
            format!(r#"{{"keepCommits":1,"x":"{name}"}}"#),
        ),
    ] {
        let method = if uri.starts_with("/$/history") {
            "PUT"
        } else {
            "POST"
        };
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let r = send(&app, req).await;
        assert_eq!(
            r.status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{uri}: {}",
            r.text()
        );
        assert!(r.error().contains("--max-admin-body-mb"), "{}", r.error());
    }
    // Graph Store bodies stream: no class ceiling, through the dataset root too
    let before = count(&st);
    let data: String = (0..200)
        .map(|i| format!("<urn:s{i}> <urn:p> \"a literal long enough\" .\n"))
        .collect();
    assert!(data.len() > 4096);
    for uri in ["/c", "/c/data"] {
        let r = send(
            &app,
            post(uri, "application/n-triples", chunked(data.clone())),
        )
        .await;
        assert!(r.status.is_success(), "{uri}: {} {}", r.status, r.text());
    }
    assert_eq!(count(&st), before + 200);
}

const FORM: &str = "application/x-www-form-urlencoded";

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn json_of(r: &Resp) -> J {
    serde_json::from_slice(&r.body).unwrap_or_else(|e| panic!("{e}: {}", r.text()))
}

fn ntriples(n: usize) -> String {
    (0..n)
        .map(|i| format!("<urn:s{i}> <urn:p> \"a literal long enough\" .\n"))
        .collect()
}

fn multipart(name: &str, data: &str) -> (String, Vec<u8>) {
    let boundary = "XyZ";
    let mut mp = Vec::new();
    write!(
        mp,
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: application/n-triples\r\n\r\n{data}\r\n--{boundary}--\r\n"
    )
    .unwrap();
    (format!("multipart/form-data; boundary={boundary}"), mp)
}

#[test]
fn requested_timeouts_are_capped() {
    let mut st = AppState::standalone(StoreOptions::default(), Duration::from_secs(60));
    let p = |q: &str| Params::from_query(&format!("/x?{q}").parse::<Uri>().unwrap());
    let secs = Duration::from_secs;
    assert_eq!(st.limits.max_timeout, Some(secs(1800)));
    assert_eq!(timeout_param(&st, &p("")), secs(60));
    assert_eq!(timeout_param(&st, &p("timeout=5")), secs(5));
    assert_eq!(timeout_param(&st, &p("timeout=100000")), secs(1800));
    // unusable values fall back to the default (a huge one used to panic)
    for bad in ["timeout=1e300", "timeout=-1", "timeout=NaN", "timeout=x"] {
        assert_eq!(timeout_param(&st, &p(bad)), secs(60), "{bad}");
    }
    assert_eq!(update_timeout(&st, &p("")), None);
    assert_eq!(update_timeout(&st, &p("timeout=100000")), Some(secs(1800)));
    // the cap never goes below the server's own defaults
    st.limits.max_timeout = Some(secs(10));
    st.limits.update_timeout = Some(secs(120));
    assert_eq!(timeout_param(&st, &p("timeout=100000")), secs(60));
    assert_eq!(update_timeout(&st, &p("timeout=100000")), Some(secs(120)));
    assert_eq!(update_timeout(&st, &p("timeout=3")), Some(secs(3)));
    // 0: unlimited
    st.limits.max_timeout = None;
    assert_eq!(timeout_param(&st, &p("timeout=100000")), secs(100000));
    assert_eq!(st.limits.json(st.default_timeout)["maxTimeoutSeconds"], 0.0);
}

#[tokio::test]
async fn timeouts_are_capped_and_reported() {
    let (_d, _st, app) = server(|st| {
        st.default_timeout = Duration::from_nanos(1);
        st.limits.max_timeout = Some(Duration::from_nanos(1));
        st.limits.update_timeout = Some(Duration::from_nanos(1));
    });
    let q = "/c/sparql?query=SELECT%20*%20%7B%3Fs%20%3Fp%20%3Fo%7D";
    for uri in [q.to_string(), format!("{q}&timeout=100")] {
        let r = send(&app, get(&uri)).await;
        assert_eq!(r.status, StatusCode::REQUEST_TIMEOUT, "{uri}: {}", r.text());
        assert_eq!(json_of(&r)["timeoutSeconds"], 1e-9, "{uri}");
    }
    let r = send(
        &app,
        post("/c/update?timeout=100", "application/sparql-update", INSERT),
    )
    .await;
    assert_eq!(r.status, StatusCode::REQUEST_TIMEOUT, "{}", r.text());
    assert_eq!(json_of(&r)["timeoutSeconds"], 1e-9);
    let server = send(&app, get("/$/server")).await;
    assert_eq!(json_of(&server)["limits"]["maxTimeoutSeconds"], 1e-9);
}

#[tokio::test]
async fn graph_store_reads_have_the_export_budget() {
    // the query result budget does not apply to exports, which are unlimited by default
    let (_d, _st, app) = server(|st| st.limits.max_result_bytes = Some(1000));
    assert_eq!(export_limit(&app).await, 0);
    let r = send(
        &app,
        post("/c/data?default", "application/n-triples", ntriples(100)),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    for uri in ["/c/data", "/c/data?default", "/c/get", "/c?format=nt"] {
        let r = send(&app, get(uri)).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
    }
    let q = "/c/sparql?query=SELECT%20*%20%7B%3Fs%20%3Fp%20%3Fo%7D&format=tsv";
    let r = send(&app, get(q)).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());

    let (_d, st, app) = server(|st| st.limits.max_export_bytes = Some(1000));
    assert_eq!(export_limit(&app).await, 1000);
    let r = send(
        &app,
        post("/c/data?default", "application/n-triples", ntriples(100)),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    for uri in ["/c/data", "/c/data?default", "/c/get", "/c?format=nt"] {
        let r = send(&app, get(uri)).await;
        assert_eq!(
            r.status,
            StatusCode::INSUFFICIENT_STORAGE,
            "{uri}: {}",
            r.text()
        );
        assert_eq!(json_of(&r)["budget"], "result-bytes", "{uri}");
    }
    // under the budget, a read is whole
    let del = Request::delete("/c/data").body(Body::empty()).unwrap();
    assert_eq!(send(&app, del).await.status, StatusCode::NO_CONTENT);
    assert_eq!(count(&st), 0);
    let r = send(
        &app,
        post("/c/data?default", "application/n-triples", ntriples(2)),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = send(&app, get("/c/data?default&format=nt")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // a streamed read is cut off at the budget: the transfer is aborted
    let (_d, st, app) = server(|st| st.limits.max_export_bytes = Some(1536 << 10));
    let big = ntriples(60_000);
    assert!(big.len() > 2 << 20);
    let r = send(&app, post("/c/data", "application/n-triples", big)).await;
    assert!(r.status.is_success(), "{}", r.text());
    assert_eq!(count(&st), 60_000);
    let res = app.clone().oneshot(get("/c/data?format=nt")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .is_err()
    );
}

/// `maxExportBytes` of `/$/server`.
async fn export_limit(app: &Router) -> u64 {
    let r = send(app, get("/$/server")).await;
    json_of(&r)["limits"]["maxExportBytes"].as_u64().unwrap()
}

#[tokio::test]
async fn streamed_bodies_have_a_ceiling() {
    let (_d, st, app) = server(|st| st.limits.max_upload_bytes = Some(4096));
    let data = ntriples(200);
    assert!(data.len() > 4096);
    for (method, uri) in [
        ("PUT", "/c/data?default"),
        ("POST", "/c/data"),
        ("POST", "/c"),
        ("POST", "/c/upload"),
    ] {
        for body in [Body::from(data.clone()), chunked(data.clone())] {
            let req = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/n-triples")
                .body(body)
                .unwrap();
            let r = send(&app, req).await;
            assert_eq!(
                r.status,
                StatusCode::PAYLOAD_TOO_LARGE,
                "{method} {uri}: {}",
                r.text()
            );
            assert!(r.error().contains("--max-upload-mb"), "{}", r.error());
        }
    }
    // a multipart upload, and a compressed body counted decompressed
    let (ct, mp) = multipart("a.nt", &data);
    let r = send(&app, post("/c/upload", &ct, mp)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    let mut z = Vec::new();
    let mut w = Codec::Zstd.writer(&mut z, None, 1).unwrap();
    w.write_all(data.as_bytes()).unwrap();
    w.finish().unwrap();
    assert!(z.len() < 4096);
    let r = send(
        &app,
        Request::put("/c/data?default")
            .header(header::CONTENT_TYPE, "application/n-triples")
            .header(header::CONTENT_ENCODING, "zstd")
            .body(Body::from(z))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    assert_eq!(count(&st), 0);
    // under the ceiling, all of these work
    let small = ntriples(10);
    let r = send(
        &app,
        post("/c/data", "application/n-triples", chunked(small.clone())),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let (ct, mp) = multipart("b.nt", &small);
    let r = send(&app, post("/c/upload", &ct, mp)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(count(&st), 10);
}

#[tokio::test]
async fn spooling_keeps_free_disk_space() {
    let (_d, st, app) = server(|st| st.limits.min_free_disk_bytes = Some(u64::MAX / 2));
    let data = ntriples(10);
    let (ct, mp) = multipart("a.nt", &data);
    for (ct, body) in [
        ("application/n-triples", data.clone().into_bytes()),
        (ct.as_str(), mp),
    ] {
        let r = send(&app, post("/c/upload", ct, body)).await;
        assert_eq!(
            r.status,
            StatusCode::INSUFFICIENT_STORAGE,
            "{ct}: {}",
            r.text()
        );
        assert!(r.error().contains("--min-free-disk-mb"), "{}", r.error());
    }
    // bodies small enough to stay in memory need no disk
    let r = send(&app, post("/c/data", "application/n-triples", data)).await;
    assert!(r.status.is_success(), "{}", r.text());
    assert_eq!(count(&st), 10);
    // a large Graph Store body goes to disk, and is checked there
    let mut budget = BodyBudget::new(&st.limits);
    let big = Body::from(vec![b' '; SPOOL_AFTER + 1]);
    let e = spool(big, &mut budget).await.err().unwrap();
    assert_eq!(e.0, StatusCode::INSUFFICIENT_STORAGE);
    // without a reserve there is no check
    budget.reserve = None;
    assert!(budget.disk(&std::env::temp_dir(), 1).is_ok());
    assert!(free_disk_bytes(&std::env::temp_dir()).unwrap() > 0);
}
