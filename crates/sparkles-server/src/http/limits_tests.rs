//! Per-request ceilings: body sizes per request class.

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
