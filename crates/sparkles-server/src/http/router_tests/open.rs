//! The open server (no `--auth-config`): what a web page can do to it through the
//! operator's browser (cross-site requests, CORS reads, DNS rebinding), and the security
//! headers of every response.

use super::*;
use axum::http::HeaderMap;

/// An open server with `--cors-origin` and `--public-host` values.
fn open_server(cors: &[&str], public_hosts: &[&str]) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.cors_origins = cors.iter().map(|s| s.to_string()).collect();
    let public: Vec<String> = public_hosts.iter().map(|s| s.to_string()).collect();
    st.hosts = crate::exposure::Hosts::new("127.0.0.1", &public).unwrap();
    let state = Arc::new(st);
    state.attach("ds", DbType::Mem, None).unwrap();
    let app = router(state.clone());
    Server {
        _dir: dir,
        state,
        app,
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (Resp, HeaderMap) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    send_h(app, req.body(Body::from(body.to_string())).unwrap()).await
}

/// The dataset's head commit.
fn head(s: &Server) -> u64 {
    s.state.get("ds").unwrap().store.head_commit().seq
}

const HOST: (&str, &str) = ("host", "127.0.0.1:3030");
const UPDATE: (&str, &str) = ("content-type", "application/sparql-update");
const INSERT: &str = "INSERT DATA { <a:a> <a:b> <a:c> }";

#[tokio::test]
async fn cross_site_writes_are_refused() {
    let s = open_server(&[], &[]);
    let before = head(&s);
    // a form or fetch from another site: its Origin, or only Sec-Fetch-Site
    for h in [
        ("origin", "https://evil.example"),
        ("sec-fetch-site", "cross-site"),
    ] {
        let (r, _) = call(&s.app, "POST", "/ds/update", &[HOST, UPDATE, h], INSERT).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{h:?}");
        assert_eq!(r.json()["error"], "cross-origin request refused");
        assert!(r.json()["requestId"].is_string());
    }
    // a form POST (no preflight) that would LOAD a local file
    let (r, _) = call(
        &s.app,
        "POST",
        "/ds/update",
        &[
            HOST,
            ("content-type", "application/x-www-form-urlencoded"),
            ("origin", "null"),
        ],
        "update=LOAD%20%3Cfile%3A%2F%2F%2Fetc%2Fpasswd%3E",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // an update over GET from an <img> is a write too
    let (r, _) = call(
        &s.app,
        "GET",
        "/ds?update=DROP%20ALL",
        &[HOST, ("sec-fetch-site", "cross-site")],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // dataset management
    let (r, _) = call(
        &s.app,
        "DELETE",
        "/$/datasets/ds",
        &[HOST, ("origin", "https://evil.example")],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(head(&s), before);
    assert!(s.state.get("ds").is_some());

    // the UI (same origin), and clients that are not browsers, still write
    for (i, h) in [
        vec![HOST, UPDATE, ("origin", "http://127.0.0.1:3030")],
        vec![HOST, UPDATE, ("sec-fetch-site", "same-origin")],
        vec![("host", "localhost:3030"), UPDATE],
        vec![UPDATE],
    ]
    .into_iter()
    .enumerate()
    {
        let insert = format!("INSERT DATA {{ <a:a> <a:b> <a:{i}> }}");
        let (r, _) = call(&s.app, "POST", "/ds/update", &h, &insert).await;
        assert_eq!(r.status, StatusCode::OK, "{h:?}: {}", r.text());
    }
    assert_eq!(head(&s), before + 4);
    // a cross-site read runs, but the page cannot read the answer (no CORS)
    let (r, h) = call(
        &s.app,
        "GET",
        "/ds/sparql?query=ASK%7B%7D",
        &[HOST, ("origin", "https://evil.example")],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(h.get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn rebound_host_names_are_refused() {
    let s = open_server(&[], &["sparql.example.org"]);
    let before = head(&s);
    for host in [
        "evil.example",
        "evil.example:3030",
        "localhost.evil.example",
    ] {
        let (r, _) = call(&s.app, "GET", "/$/datasets", &[("host", host)], "").await;
        assert_eq!(r.status, StatusCode::MISDIRECTED_REQUEST, "{host}");
        assert!(
            r.json()["error"]
                .as_str()
                .unwrap()
                .contains("--public-host")
        );
        // not even the UI or a same-origin write of the rebound page
        let (r, _) = call(
            &s.app,
            "POST",
            "/ds/update",
            &[
                ("host", host),
                UPDATE,
                ("origin", &format!("http://{host}")),
            ],
            INSERT,
        )
        .await;
        assert_eq!(r.status, StatusCode::MISDIRECTED_REQUEST, "{host}");
        let (r, _) = call(&s.app, "GET", "/ui/", &[("host", host)], "").await;
        assert_eq!(r.status, StatusCode::MISDIRECTED_REQUEST, "{host}");
    }
    assert_eq!(head(&s), before);
    for host in [
        "127.0.0.1:3030",
        "[::1]:3030",
        "localhost:3030",
        "sparql.example.org",
        "192.168.1.20:3030",
    ] {
        let (r, _) = call(&s.app, "GET", "/$/datasets", &[("host", host)], "").await;
        assert_eq!(r.status, StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn no_cors_unless_configured() {
    let preflight = |origin: &'static str| {
        [
            ("origin", origin),
            ("access-control-request-method", "POST"),
            ("access-control-request-headers", "content-type"),
            HOST,
        ]
    };
    let s = open_server(&[], &[]);
    let (_, h) = call(
        &s.app,
        "OPTIONS",
        "/ds/sparql",
        &preflight("https://evil.example"),
        "",
    )
    .await;
    assert!(h.get("access-control-allow-origin").is_none(), "{h:?}");
    assert!(h.get("access-control-allow-credentials").is_none(), "{h:?}");
    let (_, h) = call(
        &s.app,
        "GET",
        "/$/datasets",
        &[HOST, ("origin", "https://evil.example")],
        "",
    )
    .await;
    assert!(h.get("access-control-allow-origin").is_none(), "{h:?}");

    // --cors-origin: that origin reads and writes, without credentials
    let s = open_server(&["https://yasgui.example"], &[]);
    let (_, h) = call(
        &s.app,
        "OPTIONS",
        "/ds/sparql",
        &preflight("https://yasgui.example"),
        "",
    )
    .await;
    assert_eq!(h["access-control-allow-origin"], "https://yasgui.example");
    assert!(h.get("access-control-allow-credentials").is_none());
    let (_, h) = call(
        &s.app,
        "OPTIONS",
        "/ds/sparql",
        &preflight("https://evil.example"),
        "",
    )
    .await;
    assert!(h.get("access-control-allow-origin").is_none());
    let yasgui = [
        HOST,
        UPDATE,
        ("origin", "https://yasgui.example"),
        ("sec-fetch-site", "cross-site"),
    ];
    let (r, h) = call(&s.app, "POST", "/ds/update", &yasgui, INSERT).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(h["access-control-allow-origin"], "https://yasgui.example");
    let (r, _) = call(
        &s.app,
        "POST",
        "/ds/update",
        &[HOST, UPDATE, ("origin", "https://evil.example")],
        INSERT,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn security_headers() {
    let s = open_server(&[], &[]);
    let (r, h) = call(&s.app, "GET", "/ui/", &[HOST], "").await;
    assert_eq!(r.status, StatusCode::OK);
    let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(csp.contains("script-src 'self'"), "{csp}");
    // WebAssembly compilation (the formatter in the browser), never JavaScript's eval
    assert!(csp.contains("'wasm-unsafe-eval'"), "{csp}");
    assert!(!csp.contains("'unsafe-eval'"), "{csp}");
    assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(h[header::REFERRER_POLICY], "same-origin");
    // a client-side route is the same page
    let (_, h2) = call(&s.app, "GET", "/ui/cli/device?code=ABCD-EFGH", &[HOST], "").await;
    assert_eq!(h2[header::CONTENT_SECURITY_POLICY], csp);
    assert_eq!(h2[header::X_FRAME_OPTIONS], "DENY");
    // every inline script of the page is allowed by its hash, and only those
    let page = r.text();
    let scripts = page.matches("<script>").count();
    assert_eq!(csp.matches("'sha256-").count(), scripts, "{csp}");
    // API responses and errors: data only
    for uri in ["/$/datasets", "/ds/sparql?query=ASK%7B%7D", "/nope/sparql"] {
        let (_, h) = call(&s.app, "GET", uri, &[HOST], "").await;
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{uri}");
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY", "{uri}");
        assert_eq!(
            h[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; frame-ancestors 'none'",
            "{uri}"
        );
    }
    // refusals of the open server's gates too
    let (_, h) = call(
        &s.app,
        "GET",
        "/$/datasets",
        &[("host", "evil.example")],
        "",
    )
    .await;
    assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
}

#[tokio::test]
async fn map_style_url_origin_in_the_page_policy() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.hosts = crate::exposure::Hosts::new("127.0.0.1", &[]).unwrap();
    st.map_style_url = Some("https://tiles.example.com/styles/basic.json".into());
    let app = router(Arc::new(st));
    for uri in ["/ui/", "/ui/query"] {
        let (r, h) = call(&app, "GET", uri, &[HOST], "").await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
        let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(
            csp.contains("connect-src 'self' https://tiles.example.com;"),
            "{csp}"
        );
        assert!(
            csp.contains("img-src 'self' data: blob: https://tiles.example.com;"),
            "{csp}"
        );
        assert!(!csp.contains("unsafe-eval"), "{csp}");
    }
    // the maps' worker runs under its own script's policy: it fetches from the UI and the
    // style's origin
    if let Some(worker) = crate::ui::worker_asset() {
        let (r, h) = call(&app, "GET", &format!("/ui/{worker}"), &[HOST], "").await;
        assert_eq!(r.status, StatusCode::OK);
        let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.starts_with("default-src 'none';"), "{csp}");
        assert!(
            csp.contains("connect-src 'self' https://tiles.example.com;"),
            "{csp}"
        );
    }
    let (r, _) = call(&app, "GET", "/$/server", &[HOST], "").await;
    assert_eq!(
        r.json()["mapStyleUrl"],
        "https://tiles.example.com/styles/basic.json"
    );
}

#[test]
fn task_messages_lose_absolute_paths() {
    use crate::http::redact_paths;
    assert_eq!(
        redact_paths(
            "backup written to /var/lib/sparkles/backups/wiki.nq.zst (1 MiB, zstd, 0.1 s)"
        ),
        "backup written to …/wiki.nq.zst (1 MiB, zstd, 0.1 s)"
    );
    assert_eq!(
        redact_paths("reading '/data/wiki/manifest.json': No such file or directory"),
        "reading '…/manifest.json': No such file or directory"
    );
    assert_eq!(
        redact_paths("opening /srv/db/: denied"),
        "opening …/db: denied"
    );
    // datasets, URLs and media types stay
    for keep in [
        "cloned /wiki into /wiki-copy",
        "LOAD <http://example.org/a/b.ttl> failed",
        "expected text/turtle",
        "a / b // c",
        "done",
    ] {
        assert_eq!(redact_paths(keep), keep);
    }
}
