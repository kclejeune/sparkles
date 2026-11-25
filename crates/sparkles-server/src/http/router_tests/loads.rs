//! `LOAD <file:…>` under `--load-dir`, and the prefix cap, over HTTP.

use super::{Server, get_json, send, server, sparql_update};
use crate::state::{AppState, DbType};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sparkles::sparql::FileLoads;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;

/// A server with an empty in-memory dataset `ds` whose file loads are `files`.
fn with_file_loads(files: FileLoads) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.file_loads = files;
    let state = Arc::new(st);
    state.attach("ds", DbType::Mem, None).unwrap();
    let app = super::router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

#[tokio::test]
async fn file_loads_need_a_load_dir() {
    let files = tempfile::tempdir().unwrap();
    let f = files.path().join("a.nt");
    std::fs::write(&f, "<urn:a> <urn:p> <urn:b> .\n").unwrap();
    let load = format!("LOAD <file://{}>", f.display());

    // without --load-dir: refused, also for the local principal
    let s = server();
    let (r, _) = sparql_update(&s.app, &load, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(
        r.json()["error"],
        "LOAD <file:…> is not enabled: no load directory is configured"
    );
    let (r, _) = sparql_update(&s.app, "LOAD <file:///etc/hostname>", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // with it: files inside only
    let s = with_file_loads(FileLoads::under(files.path()).unwrap());
    let (r, _) = sparql_update(&s.app, &load, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(s.state.get("ds").unwrap().store.snapshot().len(), 1);
    for url in [
        "file:///etc/hostname".to_string(),
        format!("file://{}/../x.nt", files.path().display()),
    ] {
        let (r, _) = sparql_update(&s.app, &format!("LOAD SILENT <{url}>"), None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{url}: {}", r.text());
        assert!(
            r.text().contains("not a file in the load directory"),
            "{}",
            r.text()
        );
    }
}

#[tokio::test]
async fn prefixes_are_capped() {
    let s = server();
    let post = |i: usize| {
        Request::post("/ds/prefixes")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "prefix=p{i}&uri=http%3A%2F%2Fp{i}.example%2F"
            )))
            .unwrap()
    };
    let have = get_json(&s.app, "/ds/prefixes").await["prefixes"]
        .as_object()
        .unwrap()
        .len();
    let max = sparkles::store::DEFAULT_MAX_PREFIXES;
    for i in have..max {
        let r = send(&s.app, post(i)).await;
        assert_eq!(r.status, StatusCode::OK, "{i}: {}", r.text());
    }
    let r = send(&s.app, post(max)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("remove one first"), "{}", r.text());
    // replacing one is fine
    let r = send(&s.app, post(max - 1)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // a Turtle export with the full map
    let r = send(
        &s.app,
        Request::get("/ds/data?default")
            .header(header::ACCEPT, "text/turtle")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains(&format!("@prefix p{}:", max - 1)));
}

#[tokio::test]
async fn jsonld_streaming_profile_survives_graph_store_and_plain_upload() {
    let media = "application/ld+json; profile=\"http://www.w3.org/ns/json-ld#streaming\"";
    let late = r#"{"@id":"urn:s","p":"value","@context":{"p":"urn:p"}}"#;
    let ordered = r#"{"@context":{"p":"urn:p"},"@id":"urn:s","p":"value"}"#;
    for route in ["/ds/data?default", "/ds/upload"] {
        let server = with_file_loads(FileLoads::default());
        let request = |content_type: &str, body: &str| {
            Request::post(route)
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body.to_owned()))
                .unwrap()
        };
        let response = send(&server.app, request(media, late)).await;
        assert_eq!(
            response.status,
            StatusCode::BAD_REQUEST,
            "{route}: {}",
            response.text()
        );
        assert_eq!(server.state.get("ds").unwrap().store.snapshot().len(), 0);
        let response = send(&server.app, request(media, ordered)).await;
        assert!(response.status.is_success(), "{route}: {}", response.text());
        assert_eq!(server.state.get("ds").unwrap().store.snapshot().len(), 1);
        // General JSON-LD keeps its unordered-context behavior.
        let response = send(&server.app, request("application/ld+json", late)).await;
        assert!(response.status.is_success(), "{route}: {}", response.text());
    }
}

#[tokio::test]
async fn sparql_load_uses_bulk_plan_over_http_update() {
    const THRESHOLD: u64 = 10;
    const BYTES_PER_QUAD: usize = 80;

    let ntriples = |from: u64, n: u64| -> Vec<u8> {
        let mut out = Vec::new();
        for i in from..from + n {
            let head = format!("<http://ex.org/s{i}> <http://ex.org/p> \"");
            let pad = BYTES_PER_QUAD - head.len() - "\" .\n".len();
            out.extend_from_slice(head.as_bytes());
            out.extend(std::iter::repeat_n(b'x', pad));
            out.extend_from_slice(b"\" .\n");
        }
        out
    };

    let files = tempfile::tempdir().unwrap();
    let small = files.path().join("small.nt");
    let large = files.path().join("large.nt");
    let broken = files.path().join("broken.nt");
    std::fs::write(&small, ntriples(0, THRESHOLD)).unwrap();
    std::fs::write(&large, ntriples(100, THRESHOLD + 2)).unwrap();
    let mut bad = ntriples(200, THRESHOLD + 2);
    bad.extend_from_slice(b"<urn:broken \n");
    std::fs::write(&broken, bad).unwrap();

    let server_with_threshold = || -> Server {
        let dir = tempfile::tempdir().unwrap();
        let store_opts = StoreOptions {
            bulk_threshold: THRESHOLD,
            ..StoreOptions::default()
        };
        let mut st = AppState::new(dir.path(), store_opts, Duration::from_secs(30)).unwrap();
        st.file_loads = FileLoads::under(files.path()).unwrap();
        let state = Arc::new(st);
        state.attach("ds", DbType::Mem, None).unwrap();
        let app = super::router(state.clone());
        Server {
            _dir: dir,
            state,
            app,
        }
    };

    // Single-op small LOAD stays transactional.
    let s = server_with_threshold();
    let (r, _) = sparql_update(&s.app, &format!("LOAD <file://{}>", small.display()), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["inserted"], THRESHOLD);
    assert!(!s.state.get("ds").unwrap().store.snapshot().delta.is_empty());

    // Single-op large LOAD into default graph takes the bulk path, and repeating the same
    // ground LOAD preserves the commit head with committed=false and inserted=0.
    let s = server_with_threshold();
    let (r, _) = sparql_update(&s.app, &format!("LOAD <file://{}>", large.display()), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["inserted"], THRESHOLD + 2);
    let head1 = s.state.get("ds").unwrap().store.snapshot().commit;
    let snap = s.state.get("ds").unwrap().store.snapshot();
    assert!(snap.delta.is_empty(), "bulk LOAD leaves an empty delta");
    assert_eq!(snap.len(), THRESHOLD + 2);
    let (r2, _) = sparql_update(&s.app, &format!("LOAD <file://{}>", large.display()), None).await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.text());
    assert_eq!(r2.json()["inserted"], 0);
    assert_eq!(
        s.state.get("ds").unwrap().store.snapshot().commit,
        head1,
        "repeat large LOAD must preserve commit head"
    );

    // Single-op large LOAD INTO GRAPH takes the bulk path and places triples in the target graph.
    let s = server_with_threshold();
    let (r, _) = sparql_update(
        &s.app,
        &format!(
            "LOAD <file://{}> INTO GRAPH <http://ex.org/g>",
            large.display()
        ),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["inserted"], THRESHOLD + 2);
    let snap = s.state.get("ds").unwrap().store.snapshot();
    assert!(snap.delta.is_empty());
    let ask = send(
        &s.app,
        Request::post("/ds/sparql")
            .header(header::CONTENT_TYPE, "application/sparql-query")
            .header(header::ACCEPT, "application/sparql-results+json")
            .body(Body::from(
                "ASK { GRAPH <http://ex.org/g> { <http://ex.org/s100> ?p ?o } }",
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(ask.status, StatusCode::OK, "{}", ask.text());
    assert_eq!(ask.json()["boolean"], true);

    // Multi-operation update with a large LOAD stays transactional.
    let s = server_with_threshold();
    let multi = format!(
        "INSERT DATA {{ <http://ex.org/seed> <http://ex.org/p> 1 }} ; LOAD <file://{}>",
        large.display()
    );
    let (r, _) = sparql_update(&s.app, &multi, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["inserted"], THRESHOLD + 3);
    assert!(!s.state.get("ds").unwrap().store.snapshot().delta.is_empty());

    // LOAD SILENT of a large broken file suppresses the parse error and leaves nothing behind.
    let s = server_with_threshold();
    let (r, _) = sparql_update(
        &s.app,
        &format!("LOAD SILENT <file://{}>", broken.display()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["inserted"], 0);
    assert_eq!(s.state.get("ds").unwrap().store.snapshot().len(), 0);
}
