//! Rate-limit tests: configuration parsing, and the router with a manual clock (no
//! sleeps).

use super::*;
use crate::http::router;
use crate::obs::Phase;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::http::Request as HttpRequest;
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

const DATA: &str = "<http://example.org/a> <http://example.org/p> \"1\" .\n";

struct Server {
    _dir: tempfile::TempDir,
    app: Router,
    clock: Arc<AtomicU64>,
    rl: Arc<RateLimiter>,
}

fn server(flags: &[&str], trusted: &[&str]) -> Server {
    let mut cfg = Config::default();
    for f in flags {
        cfg.apply_flag(f).unwrap();
    }
    cfg.trusted_proxies = trusted.iter().map(|s| s.to_string()).collect();
    server_cfg(cfg)
}

fn server_cfg(cfg: Config) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    let clock = Arc::new(AtomicU64::new(0));
    let rl = Arc::new(
        RateLimiter::new(&cfg)
            .unwrap()
            .with_clock(Clock::Manual(clock.clone())),
    );
    st.rate_limit = Some(rl.clone());
    let st = Arc::new(st);
    for name in ["ds", "other"] {
        let ds = st.attach(name, DbType::Mem, None).unwrap();
        ds.store
            .load(&[Source::from_bytes(
                DATA.as_bytes().to_vec(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
    }
    st.set_phase(Phase::Ready);
    Server {
        _dir: dir,
        app: router(st),
        clock,
        rl,
    }
}

impl Server {
    fn advance(&self, d: Duration) {
        self.clock.fetch_add(d.as_nanos() as u64, Ordering::Relaxed);
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        peer: &str,
        headers: &[(&str, &str)],
    ) -> Response {
        let mut b = HttpRequest::builder().method(method).uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut req = b.body(Body::empty()).unwrap();
        let addr: SocketAddr = format!("{peer}:40000").parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
        self.app.clone().oneshot(req).await.unwrap()
    }

    async fn get(&self, uri: &str, peer: &str) -> Response {
        self.call("GET", uri, peer, &[]).await
    }
}

const Q: &str = "/ds/sparql?query=ASK%7B%7D";

fn hdr(r: &Response, name: &str) -> String {
    r.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

async fn body_json(r: Response) -> serde_json::Value {
    let b = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&b).unwrap()
}

#[test]
fn parses_limits_and_rates() {
    assert_eq!(
        Rate::parse("100/s").unwrap(),
        Rate {
            count: 100,
            period: Duration::from_secs(1)
        }
    );
    assert_eq!(
        Rate::parse("10/min").unwrap().period,
        Duration::from_secs(60)
    );
    assert!(Rate::parse("0/s").is_err());
    assert!(Rate::parse("5/fortnight").is_err());
    let l = Limit::parse("100/s,burst=200,concurrency=8,client-concurrency=2").unwrap();
    assert_eq!(l.burst, Some(200));
    assert_eq!(l.concurrency, Some(8));
    assert_eq!(l.client_concurrency, Some(2));
    assert!(Limit::parse("burst=5").is_err());
    assert!(Limit::parse("off").unwrap().is_unlimited());
    let l = Limit::parse("concurrency=4").unwrap();
    assert!(l.rate.is_none() && !l.is_unlimited());

    let mut c = Config::default();
    c.apply_flag("query=100/s,burst=200").unwrap();
    c.apply_flag("update@big=1/s").unwrap();
    assert!(c.apply_flag("reads=1/s").is_err());
    assert!(c.apply_flag("query").is_err());
    assert_eq!(c.classes["query"].burst, Some(200));
    assert!(c.datasets["big"].contains_key("update"));

    let j: Config = serde_json::from_str(
        r#"{"classes":{"auth":{"rate":"10/min","burst":5,"failureCost":3}},
            "datasets":{"big":{"query":{"concurrency":2}}},
            "trustedProxies":["10.0.0.0/8","::1"],"maxKeys":1000}"#,
    )
    .unwrap();
    j.validate().unwrap();
    assert_eq!(j.classes["auth"].failure_cost, Some(3));
    assert!(serde_json::from_str::<Config>(r#"{"classes":{"query":{"rat":"1/s"}}}"#).is_err());
    let bad: Config = serde_json::from_str(r#"{"trustedProxies":["nope"]}"#).unwrap();
    assert!(bad.validate().is_err());
}

#[test]
fn classifies_routes() {
    let c = |route: Option<&str>, m: Method, uri: &str| {
        classify(route, &m, &uri.parse().unwrap(), &HeaderMap::new())
    };
    assert_eq!(c(None, Method::POST, "/$/auth/login"), Some(Class::Auth));
    assert_eq!(c(Some("/$/ping"), Method::GET, "/$/ping"), None);
    assert_eq!(c(Some("/$/ready/{ds}"), Method::GET, "/$/ready/ds"), None);
    assert_eq!(c(Some("/$/metrics"), Method::GET, "/$/metrics"), None);
    assert_eq!(c(Some("/ui/{*path}"), Method::GET, "/ui/x.js"), None);
    assert_eq!(
        c(Some("/{ds}/sparql"), Method::POST, "/ds/sparql"),
        Some(Class::Query)
    );
    assert_eq!(
        c(Some("/{ds}/data"), Method::GET, "/ds/data"),
        Some(Class::Query)
    );
    assert_eq!(
        c(Some("/{ds}/data"), Method::PUT, "/ds/data"),
        Some(Class::Update)
    );
    assert_eq!(
        c(Some("/{ds}/upload"), Method::POST, "/ds/upload"),
        Some(Class::Update)
    );
    assert_eq!(
        c(Some("/{ds}"), Method::GET, "/ds?query=x"),
        Some(Class::Query)
    );
    assert_eq!(
        c(Some("/{ds}"), Method::GET, "/ds?update=x"),
        Some(Class::Update)
    );
    assert_eq!(c(Some("/{ds}"), Method::POST, "/ds"), Some(Class::Update));
    assert_eq!(
        c(Some("/$/schema/{ds}"), Method::GET, "/$/schema/ds"),
        Some(Class::Query)
    );
    assert_eq!(c(Some("/$/datasets"), Method::GET, "/$/datasets"), None);
    assert_eq!(
        c(Some("/$/datasets"), Method::POST, "/$/datasets"),
        Some(Class::Admin)
    );
    assert_eq!(
        c(Some("/$/compact/{ds}"), Method::POST, "/$/compact/ds"),
        Some(Class::Admin)
    );
}

#[test]
fn trusted_proxies_name_the_client() {
    let t = TrustedProxies::parse(&["10.0.0.0/8".into(), "::1".into()]).unwrap();
    let ip = |s: &str| -> IpAddr { s.parse().unwrap() };
    let mut h = HeaderMap::new();
    h.insert("x-forwarded-for", "203.0.113.7, 10.1.2.3".parse().unwrap());
    // the rightmost untrusted hop
    assert_eq!(t.client_ip(ip("10.0.0.1"), &h), ip("203.0.113.7"));
    // an untrusted peer's headers are ignored
    assert_eq!(t.client_ip(ip("198.51.100.1"), &h), ip("198.51.100.1"));
    // a spoofed hop left of a real client does not win
    h.insert("x-forwarded-for", "1.1.1.1, 203.0.113.7".parse().unwrap());
    assert_eq!(t.client_ip(ip("::1"), &h), ip("203.0.113.7"));
    // Forwarded takes precedence, with quoted IPv6 and ports
    h.insert(
        header::FORWARDED,
        "for=192.0.2.60;proto=http, for=\"[2001:db8::1]:4711\""
            .parse()
            .unwrap(),
    );
    assert_eq!(t.client_ip(ip("10.9.9.9"), &h), ip("2001:db8::1"));
    // an obfuscated hop stops the walk at the last known address
    let mut h = HeaderMap::new();
    h.insert(
        header::FORWARDED,
        "for=_hidden, for=10.2.2.2".parse().unwrap(),
    );
    assert_eq!(t.client_ip(ip("10.0.0.1"), &h), ip("10.2.2.2"));
    // IPv6 clients share a key per /64; mapped IPv4 is IPv4
    assert_eq!(
        ClientKey::ip(ip("2001:db8::1")),
        ClientKey::ip(ip("2001:db8::ffff"))
    );
    assert_ne!(
        ClientKey::ip(ip("2001:db8::1")),
        ClientKey::ip(ip("2001:db8:0:1::1"))
    );
    assert_eq!(
        ClientKey::ip(ip("::ffff:192.0.2.1")),
        ClientKey::ip(ip("192.0.2.1"))
    );
    assert!(t.contains(ip("::ffff:10.1.1.1")));
}

#[tokio::test]
async fn over_the_rate_answers_429_with_retry_after() {
    let s = server(&["query=2/s,burst=2"], &[]);
    for remaining in [1, 0] {
        let r = s.get(Q, "192.0.2.1").await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(hdr(&r, "ratelimit-policy"), "\"query\";q=2;w=1");
        assert_eq!(hdr(&r, "ratelimit"), format!("\"query\";r={remaining};t=1"));
    }
    let r = s.get(Q, "192.0.2.1").await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hdr(&r, "retry-after"), "1");
    assert_eq!(hdr(&r, "ratelimit"), "\"query\";r=0;t=1");
    assert!(!hdr(&r, "x-request-id").is_empty());
    let j = body_json(r).await;
    assert_eq!(j["limitClass"], "query");
    assert_eq!(j["reason"], "rate");
    assert_eq!(j["retryAfterSeconds"], 1);
    assert!(
        j["error"]
            .as_str()
            .unwrap()
            .contains("too many query requests")
    );
    // half a second refills one request
    s.advance(Duration::from_millis(500));
    assert_eq!(s.get(Q, "192.0.2.1").await.status(), StatusCode::OK);
    assert_eq!(
        s.get(Q, "192.0.2.1").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // Retry-After rounds up to whole seconds
    let s = server(&["query=1/min"], &[]);
    assert_eq!(s.get(Q, "192.0.2.1").await.status(), StatusCode::OK);
    let r = s.get(Q, "192.0.2.1").await;
    assert_eq!(hdr(&r, "retry-after"), "60");
    s.advance(Duration::from_secs(59));
    assert_eq!(hdr(&s.get(Q, "192.0.2.1").await, "retry-after"), "1");
    s.advance(Duration::from_secs(1));
    assert_eq!(s.get(Q, "192.0.2.1").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn classes_and_clients_have_separate_buckets() {
    let s = server(&["auth=1/min", "query=1/min"], &[]);
    // auth: no auth routes exist yet, but the class applies to every /$/auth/ path
    let r = s.call("POST", "/$/auth/login", "192.0.2.1", &[]).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert_eq!(hdr(&r, "ratelimit-policy"), "\"auth\";q=1;w=60");
    let r = s.call("POST", "/$/auth/login", "192.0.2.1", &[]).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body_json(r).await["limitClass"], "auth");
    // the same client still has its query budget
    assert_eq!(s.get(Q, "192.0.2.1").await.status(), StatusCode::OK);
    assert_eq!(
        s.get(Q, "192.0.2.1").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // another client has its own buckets
    assert_eq!(s.get(Q, "192.0.2.2").await.status(), StatusCode::OK);
    let r = s.call("POST", "/$/auth/login", "192.0.2.2", &[]).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    // updates are not limited: no update class configured
    for _ in 0..3 {
        let r = s
            .call(
                "POST",
                "/ds/update",
                "192.0.2.1",
                &[("content-type", "application/sparql-update")],
            )
            .await;
        assert_ne!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(r.headers().get("ratelimit").is_none());
    }
}

#[tokio::test]
async fn health_endpoints_are_never_limited() {
    let s = server(
        &[
            "auth=1/min",
            "query=1/min",
            "update=1/min",
            "admin=1/min,concurrency=1",
        ],
        &[],
    );
    for _ in 0..5 {
        for uri in [
            "/$/ping",
            "/$/ready",
            "/$/ready/ds",
            "/$/metrics",
            "/$/server",
        ] {
            let r = s.get(uri, "192.0.2.1").await;
            assert_eq!(r.status(), StatusCode::OK, "{uri}");
            assert!(r.headers().get("ratelimit").is_none(), "{uri}");
        }
    }
}

#[tokio::test]
async fn trusted_proxies_key_by_forwarded_client() {
    let s = server(&["query=1/min"], &["10.0.0.0/8"]);
    let via = |c: &'static str| [("x-forwarded-for", c)];
    let r = s.call("GET", Q, "10.0.0.5", &via("203.0.113.1")).await;
    assert_eq!(r.status(), StatusCode::OK);
    // a different client behind the same proxy
    let r = s.call("GET", Q, "10.0.0.5", &via("203.0.113.2")).await;
    assert_eq!(r.status(), StatusCode::OK);
    // the first client again, through another proxy
    let r = s.call("GET", Q, "10.0.0.6", &via("203.0.113.1")).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    // an untrusted peer cannot choose its key
    let r = s.call("GET", Q, "198.51.100.1", &via("203.0.113.9")).await;
    assert_eq!(r.status(), StatusCode::OK);
    let r = s.call("GET", Q, "198.51.100.1", &via("203.0.113.10")).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn concurrency_limits_answer_503_until_the_body_is_sent() {
    let s = server(&["query=concurrency=1"], &[]);
    // a streamed Graph Store GET holds its permit until its body is consumed
    let first = s.get("/ds/data", "192.0.2.1").await;
    assert_eq!(first.status(), StatusCode::OK);
    assert!(first.headers().get("ratelimit").is_none());
    let r = s.get(Q, "192.0.2.2").await;
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(hdr(&r, "retry-after"), "1");
    let j = body_json(r).await;
    assert_eq!(j["reason"], "concurrency");
    assert_eq!(j["limitClass"], "query");
    let body = axum::body::to_bytes(first.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!body.is_empty());
    assert_eq!(s.get(Q, "192.0.2.2").await.status(), StatusCode::OK);

    // per client: others are unaffected
    let s = server(&["query=client-concurrency=1"], &[]);
    let held = s.get("/ds/data", "192.0.2.1").await;
    let r = s.get(Q, "192.0.2.1").await;
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body_json(r).await["reason"], "client-concurrency");
    assert_eq!(s.get(Q, "192.0.2.2").await.status(), StatusCode::OK);
    drop(held);
    assert_eq!(s.get(Q, "192.0.2.1").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn dataset_overrides_replace_the_class_limit() {
    let s = server(&["query=1/min", "query@other=3/min", "update@ds=off"], &[]);
    assert_eq!(s.get(Q, "192.0.2.1").await.status(), StatusCode::OK);
    assert_eq!(
        s.get(Q, "192.0.2.1").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let q2 = "/other/sparql?query=ASK%7B%7D";
    for _ in 0..3 {
        let r = s.get(q2, "192.0.2.1").await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(hdr(&r, "ratelimit-policy"), "\"query@other\";q=3;w=60");
    }
    assert_eq!(
        s.get(q2, "192.0.2.1").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn limited_requests_are_counted() {
    let s = server(&["query=1/min"], &[]);
    s.get(Q, "192.0.2.1").await;
    s.get(Q, "192.0.2.1").await;
    s.get(Q, "192.0.2.1").await;
    let r = s.get("/$/metrics", "192.0.2.1").await;
    let text = String::from_utf8(
        axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        text.contains(
            "sparkles_requests_total{dataset=\"ds\",operation=\"query\",outcome=\"rate_limited\"} 2"
        ),
        "{text}"
    );
    assert!(text.contains("sparkles_rate_limited_total{dataset=\"ds\",class=\"query\"} 2"));
    assert!(text.contains("sparkles_rate_limited_total{dataset=\"ds\",class=\"auth\"} 0"));
    let r = s.get("/$/metrics?format=json", "192.0.2.1").await;
    let j = body_json(r).await;
    let ds = j["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "ds")
        .unwrap()
        .clone();
    assert_eq!(ds["rateLimited"]["query"], 2);
}

#[tokio::test]
async fn failed_auth_costs_more() {
    // no auth routes yet: every /$/auth/ request answers 404, so weight 404s through a
    // stub that answers 401
    let mut cfg = Config::default();
    cfg.apply_flag("auth=3/min,failure-cost=3").unwrap();
    let rl = Arc::new(
        RateLimiter::new(&cfg)
            .unwrap()
            .with_clock(Clock::Manual(Arc::new(AtomicU64::new(0)))),
    );
    let app = Router::new()
        .route(
            "/$/auth/login",
            axum::routing::post(|| async { StatusCode::UNAUTHORIZED }),
        )
        .layer(axum::middleware::from_fn_with_state(rl, limit));
    let call = || async {
        app.clone()
            .oneshot(
                HttpRequest::post("/$/auth/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    };
    let r = call().await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    // one failed attempt used the whole budget of three
    assert_eq!(hdr(&r, "ratelimit"), "\"auth\";r=0;t=60");
    assert_eq!(call().await.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn idle_buckets_are_evicted_and_keys_are_capped() {
    let mut cfg = Config::default();
    cfg.apply_flag("query=10/s").unwrap();
    cfg.max_keys = Some(64);
    let s = server_cfg(cfg);
    for i in 0..10u8 {
        s.get(Q, &format!("192.0.2.{i}")).await;
    }
    assert_eq!(s.rl.tracked(), 10);
    // not yet refilled: kept
    assert_eq!(s.rl.sweep(), 0);
    s.advance(Duration::from_secs(1));
    assert_eq!(s.rl.sweep(), 10);
    assert_eq!(s.rl.tracked(), 0);
    // a flood of distinct addresses stays within the cap
    for i in 0..2000u32 {
        let [_, _, a, b] = i.to_be_bytes();
        s.get(Q, &format!("198.51.{a}.{b}")).await;
    }
    assert!(s.rl.tracked() <= 64 + 16, "{}", s.rl.tracked());
}

#[tokio::test]
async fn reload_switches_configuration() {
    let s = server(&["query=1/min"], &[]);
    s.get(Q, "192.0.2.1").await;
    assert_eq!(
        s.get(Q, "192.0.2.1").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    let mut cfg = Config::default();
    cfg.apply_flag("query=off").unwrap();
    s.rl.reload(&cfg).unwrap();
    for _ in 0..3 {
        assert_eq!(s.get(Q, "192.0.2.1").await.status(), StatusCode::OK);
    }
}

#[test]
fn sources_merge_file_and_flags() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("limits.json");
    std::fs::write(
        &f,
        r#"{"classes":{"query":{"rate":"5/s"},"update":{"rate":"1/s"}}}"#,
    )
    .unwrap();
    let s = Sources {
        file: Some(f),
        flags: vec!["query=50/s".into()],
        trusted_proxies: vec!["10.0.0.0/8".into()],
    };
    let c = s.load().unwrap().unwrap();
    assert_eq!(c.classes["query"].rate.unwrap().count, 50);
    assert_eq!(c.classes["update"].rate.unwrap().count, 1);
    assert_eq!(c.trusted_proxies, ["10.0.0.0/8"]);
    assert!(Sources::default().load().unwrap().is_none());
    let bad = Sources {
        flags: vec!["query=fast".into()],
        ..Default::default()
    };
    assert!(bad.load().is_err());
}

/// Stands in for the principal an authentication layer puts into the request.
#[derive(Clone)]
struct User(&'static str);

/// Keys authenticated requests by principal, the rest (and the auth class) by address.
struct ByPrincipal;

impl ClientKeyer for ByPrincipal {
    fn key(&self, class: Class, req: &Request, trusted: &TrustedProxies) -> ClientKey {
        match req.extensions().get::<User>() {
            Some(u) if class != Class::Auth => ClientKey::principal(u.0),
            _ => PeerKeyer.key(class, req, trusted),
        }
    }
}

#[tokio::test]
async fn a_keyer_can_count_principals_instead_of_addresses() {
    let mut cfg = Config::default();
    cfg.apply_flag("query=1/min").unwrap();
    let rl = Arc::new(
        RateLimiter::new(&cfg)
            .unwrap()
            .with_clock(Clock::Manual(Arc::new(AtomicU64::new(0))))
            .with_keyer(Arc::new(ByPrincipal)),
    );
    let app = Router::new()
        .route("/{ds}/sparql", axum::routing::get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(rl, limit));
    let call = |user: Option<&'static str>, peer: &'static str| {
        let app = app.clone();
        async move {
            let mut req = HttpRequest::get("/ds/sparql").body(Body::empty()).unwrap();
            let addr: SocketAddr = format!("{peer}:1").parse().unwrap();
            req.extensions_mut().insert(ConnectInfo(addr));
            if let Some(u) = user {
                req.extensions_mut().insert(User(u));
            }
            app.oneshot(req).await.unwrap().status()
        }
    };
    assert_eq!(call(Some("alice"), "192.0.2.1").await, StatusCode::OK);
    // the same principal from another address shares the budget
    assert_eq!(
        call(Some("alice"), "192.0.2.2").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // another principal behind the first address does not
    assert_eq!(call(Some("bob"), "192.0.2.1").await, StatusCode::OK);
    assert_eq!(call(None, "192.0.2.1").await, StatusCode::OK);
}
