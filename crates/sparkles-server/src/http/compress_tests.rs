//! Compressed request bodies, negotiated response encodings, compressed backups and
//! precompressed UI assets.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::codec::Codec;
use sparkles::store::StoreOptions;
use std::io::{Read, Write};
use tower::ServiceExt;

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn header(&self, n: &str) -> Option<String> {
        self.headers.get(n).map(|v| v.to_str().unwrap().to_string())
    }
    /// The body decoded per its `Content-Encoding`.
    fn decoded(&self) -> Vec<u8> {
        let codec = self
            .header("content-encoding")
            .map(|e| Codec::from_content_encoding(&e).expect("known encoding"))
            .unwrap_or_default();
        let mut out = Vec::new();
        codec
            .reader(&self.body[..], None)
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        out
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
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

fn compress(c: Codec, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut w = c.writer(&mut out, None, 1).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap();
    out
}

fn triples(n: usize, tag: &str) -> String {
    (0..n)
        .map(|i| format!("<urn:{tag}{i}> <urn:p> \"value {i} of a longer literal\" .\n"))
        .collect()
}

fn server(max_decompressed: Option<u64>) -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.limits.max_decompressed_bytes = max_decompressed;
    let st = Arc::new(st);
    st.create("c", DbType::Persistent).unwrap();
    let app = router(st.clone());
    (dir, st, app)
}

fn count(st: &AppState) -> u64 {
    st.datasets.read()["c"].store.snapshot().len()
}

fn post_encoded(uri: &str, ct: &str, encoding: &str, body: Vec<u8>) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, ct)
        .header(header::CONTENT_ENCODING, encoding)
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn request_bodies_are_decompressed() {
    let (_d, st, app) = server(Some(64 << 20));
    let mut expected = 0;
    for c in [Codec::Gzip, Codec::Brotli, Codec::Zstd] {
        let tag = c.name();
        let body = compress(c, triples(10, tag).as_bytes());
        let enc = c.content_encoding().unwrap();
        // Graph Store Protocol (the spooling reader)
        let r = send(
            &app,
            post_encoded("/c/data?default", "application/n-triples", enc, body),
        )
        .await;
        assert!(r.status.is_success(), "{c}: {} {}", r.status, r.text());
        expected += 10;
        assert_eq!(count(&st), expected, "{c}");
        // a SPARQL update (a buffered body)
        let u = format!("INSERT DATA {{ <urn:u-{tag}> <urn:p> 1 }}");
        let r = send(
            &app,
            post_encoded(
                "/c/update",
                "application/sparql-update",
                enc,
                compress(c, u.as_bytes()),
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{c}: {}", r.text());
        expected += 1;
        assert_eq!(count(&st), expected, "{c}");
    }
    // an encoding the server does not know: 415 naming the ones it does
    let r = send(
        &app,
        post_encoded(
            "/c/data?default",
            "application/n-triples",
            "compress",
            b"x".to_vec(),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let accepted = r.header("accept-encoding").unwrap_or_default();
    assert!(
        accepted.contains("zstd") && accepted.contains("gzip"),
        "{accepted}"
    );
    // an upload of compressed files, by name and magic bytes
    let boundary = "XyZ";
    let mut mp = Vec::new();
    for (name, c) in [("a.nt.zst", Codec::Zstd), ("b.nt.br", Codec::Brotli)] {
        write!(
            mp,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .unwrap();
        mp.extend(compress(c, triples(3, name).as_bytes()));
        mp.extend(b"\r\n");
    }
    write!(mp, "--{boundary}--\r\n").unwrap();
    let r = send(
        &app,
        Request::post("/c/upload")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(mp))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(count(&st), expected + 6);
}

#[tokio::test]
async fn decompressed_size_is_capped() {
    let (_d, st, app) = server(Some(64 << 10));
    let big = triples(5_000, "big");
    assert!(big.len() > 64 << 10);
    for (uri, ct, body) in [
        ("/c/data?default", "application/n-triples", big.clone()),
        (
            "/c/update",
            "application/sparql-update",
            format!("INSERT DATA {{ {big} }}"),
        ),
    ] {
        let z = compress(Codec::Zstd, body.as_bytes());
        assert!(z.len() < 64 << 10);
        let r = send(&app, post_encoded(uri, ct, "zstd", z)).await;
        assert_eq!(
            r.status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{uri}: {}",
            r.text()
        );
        assert_eq!(count(&st), 0, "{uri}");
    }
    // the same size uncompressed is not limited by this cap
    let r = send(
        &app,
        Request::post("/c/data?default")
            .header(header::CONTENT_TYPE, "application/n-triples")
            .body(Body::from(big))
            .unwrap(),
    )
    .await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    // compressed data inside an uncompressed body (magic bytes) is capped too
    let (_d, st, app) = server(Some(64 << 10));
    let r = send(
        &app,
        Request::post("/c/data?default")
            .header(header::CONTENT_TYPE, "application/n-triples")
            .body(Body::from(compress(
                Codec::Gzip,
                triples(5_000, "g").as_bytes(),
            )))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    assert_eq!(count(&st), 0);
}

#[tokio::test]
async fn responses_are_compressed_by_accept_encoding() {
    let (_d, _st, app) = server(None);
    // over 1 MiB: the streamed path
    let r = send(
        &app,
        Request::post("/c/data?default")
            .header(header::CONTENT_TYPE, "application/n-triples")
            .body(Body::from(triples(20_000, "r")))
            .unwrap(),
    )
    .await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    let get = |enc: Option<&str>| {
        let mut b = Request::get("/c/data?default").header(header::ACCEPT, "application/n-triples");
        if let Some(e) = enc {
            b = b.header(header::ACCEPT_ENCODING, e);
        }
        b.body(Body::empty()).unwrap()
    };
    let plain = send(&app, get(None)).await;
    assert_eq!(plain.status, StatusCode::OK);
    assert!(plain.header("content-encoding").is_none());
    assert!(plain.body.len() > 1 << 20, "{}", plain.body.len());
    for (accept, want) in [("zstd", "zstd"), ("br", "br"), ("gzip", "gzip")] {
        let r = send(&app, get(Some(accept))).await;
        assert_eq!(r.header("content-encoding").as_deref(), Some(want));
        assert!(
            r.body.len() < plain.body.len() / 4,
            "{want}: {}",
            r.body.len()
        );
        assert_eq!(r.decoded(), plain.body, "{want}");
    }
    // a tiny response is sent as is
    let r = send(
        &app,
        Request::get("/$/ping")
            .header(header::ACCEPT_ENCODING, "gzip")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(r.header("content-encoding").is_none());
}

#[tokio::test]
async fn compression_can_be_turned_off_or_restricted() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.http_compression =
        crate::compress::HttpCompression::parse("auto", "fastest", "gzip").unwrap();
    let st = Arc::new(st);
    st.create("c", DbType::Persistent).unwrap();
    let app = router(st);
    let q = |enc: &str| {
        Request::get("/$/datasets")
            .header(header::ACCEPT_ENCODING, enc)
            .body(Body::empty())
            .unwrap()
    };
    // the server info is over 256 bytes
    let r = send(&app, q("zstd, br")).await;
    assert!(r.header("content-encoding").is_none());
    let r = send(&app, q("zstd, gzip")).await;
    assert_eq!(r.header("content-encoding").as_deref(), Some("gzip"));
    assert!(crate::compress::HttpCompression::parse("auto", "default", "lzma").is_err());
    assert!(crate::compress::HttpCompression::parse("sometimes", "default", "gzip").is_err());
    assert!(
        !crate::compress::HttpCompression::parse("off", "default", "")
            .unwrap()
            .enabled
    );
}

#[tokio::test]
async fn backups_take_a_codec() {
    let (dir, st, app) = server(None);
    let r = send(
        &app,
        Request::post("/c/data?default")
            .header(header::CONTENT_TYPE, "application/n-triples")
            .body(Body::from(triples(50, "b")))
            .unwrap(),
    )
    .await;
    assert!(r.status.is_success());
    for (q, ext) in [("", ".nq.zst"), ("?compression=gzip&level=5", ".nq.gz")] {
        let r = send(
            &app,
            Request::post(format!("/$/backup/c{q}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
        let id: J = serde_json::from_slice(&r.body).unwrap();
        let id = id["id"].as_str().unwrap().to_string();
        // wait for the task
        let mut done = false;
        for _ in 0..200 {
            let t = send(
                &app,
                Request::get(format!("/$/tasks/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            let t: J = serde_json::from_slice(&t.body).unwrap_or(J::Null);
            if t["finishedAt"].is_string() {
                assert_eq!(t["state"], "done", "{t}");
                done = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(done, "backup task did not finish");
        let file = std::fs::read_dir(dir.path().join("backups"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().ends_with(ext))
            .unwrap_or_else(|| panic!("no {ext} backup"));
        let codec = Codec::from_extension(&file).unwrap();
        let mut nq = String::new();
        codec
            .reader(std::fs::File::open(&file).unwrap(), None)
            .unwrap()
            .read_to_string(&mut nq)
            .unwrap();
        assert_eq!(nq.lines().count() as u64, count(&st), "{ext}");
    }
    let r = send(
        &app,
        Request::post("/$/backup/c?compression=rar")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ui_assets_are_served_precompressed() {
    let Some(path) = crate::ui::precompressed_asset() else {
        // the UI build is not embedded in this build
        return;
    };
    let (_d, _st, app) = server(None);
    let uri = format!("/ui/{path}");
    let get = |enc: Option<&str>| {
        let mut b = Request::get(&uri);
        if let Some(e) = enc {
            b = b.header(header::ACCEPT_ENCODING, e);
        }
        b.body(Body::empty()).unwrap()
    };
    let plain = send(&app, get(None)).await;
    assert_eq!(plain.status, StatusCode::OK);
    assert!(plain.header("content-encoding").is_none());
    assert_eq!(plain.header("vary").as_deref(), Some("accept-encoding"));
    let br = send(&app, get(Some("gzip, br"))).await;
    // the prebuilt brotli file, not compressed again
    assert_eq!(br.header("content-encoding").as_deref(), Some("br"));
    assert!(br.body.len() < plain.body.len());
    assert_eq!(br.decoded(), plain.body);
    let gz = send(&app, get(Some("gzip, br;q=0"))).await;
    assert_eq!(gz.header("content-encoding").as_deref(), Some("gzip"));
    assert_eq!(gz.decoded(), plain.body);
    // zstd only: the response layer compresses the identity file
    let z = send(&app, get(Some("zstd"))).await;
    assert_eq!(z.decoded(), plain.body);
}
