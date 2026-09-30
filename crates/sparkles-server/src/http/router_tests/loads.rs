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
