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
    let k = |s: &str| ClientKey::ip(ip(s));
    let from = |t: &TrustedProxies, peer: &str, h: &HeaderMap| {
        t.client_key(Some(PeerAddr::Ip(ip(peer))), h)
    };
    let mut h = HeaderMap::new();
    h.insert("x-forwarded-for", "203.0.113.7, 10.1.2.3".parse().unwrap());
    // the rightmost untrusted hop
    assert_eq!(from(&t, "10.0.0.1", &h), k("203.0.113.7"));
    // an untrusted peer's headers are ignored
    assert_eq!(from(&t, "198.51.100.1", &h), k("198.51.100.1"));
    // a spoofed hop left of a real client does not win
    h.insert("x-forwarded-for", "1.1.1.1, 203.0.113.7".parse().unwrap());
    assert_eq!(from(&t, "::1", &h), k("203.0.113.7"));
    // only the configured header is read: the client's own Forwarded changes nothing
    h.insert(header::FORWARDED, "for=198.51.100.9".parse().unwrap());
    assert_eq!(from(&t, "10.9.9.9", &h), k("203.0.113.7"));
    // with `forwarded` configured, X-Forwarded-For is the one ignored (quoted IPv6, ports)
    let f = t.clone().with_header(ForwardHeader::Forwarded);
    h.insert(
        header::FORWARDED,
        "for=192.0.2.60;proto=http, for=\"[2001:db8::1]:4711\""
            .parse()
            .unwrap(),
    );
    assert_eq!(from(&f, "10.9.9.9", &h), k("2001:db8::1"));
    // a hop that is not an address is a client of its own (never the proxy before it)
    h.insert(
        header::FORWARDED,
        "for=_hidden, for=10.2.2.2".parse().unwrap(),
    );
    assert_eq!(
        from(&f, "10.0.0.1", &h),
        ClientKey::Opaque("_hidden".into())
    );
    h.insert(header::FORWARDED, "proto=https".parse().unwrap());
    assert_eq!(
        from(&f, "10.0.0.1", &h),
        ClientKey::Opaque("unknown".into())
    );
    let mut x = HeaderMap::new();
    x.insert("x-forwarded-for", "garbage, 10.2.2.2".parse().unwrap());
    assert_eq!(
        from(&t, "10.0.0.1", &x),
        ClientKey::Opaque("garbage".into())
    );
    let long = "x".repeat(300);
    x.insert("x-forwarded-for", long.parse().unwrap());
    assert_eq!(
        from(&t, "10.0.0.1", &x),
        ClientKey::Opaque("x".repeat(64).into())
    );
    // every hop trusted: the leftmost; no hop: the peer
    x.insert("x-forwarded-for", "10.3.3.3, 10.2.2.2".parse().unwrap());
    assert_eq!(from(&t, "10.0.0.1", &x), k("10.3.3.3"));
    assert_eq!(from(&t, "10.0.0.1", &HeaderMap::new()), k("10.0.0.1"));
    // the Unix socket: one shared key unless trusted, then the forwarded client
    let u = TrustedProxies::parse(&["unix".into()]).unwrap();
    x.insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
    assert!(u.unix() && !t.unix());
    assert_eq!(t.client_key(Some(PeerAddr::Unix), &x), ClientKey::Unknown);
    assert_eq!(u.client_key(Some(PeerAddr::Unix), &x), k("203.0.113.7"));
    assert_eq!(
        u.client_key(Some(PeerAddr::Unix), &HeaderMap::new()),
        ClientKey::Unknown
    );
    assert_eq!(u.client_key(None, &x), ClientKey::Unknown);
    // trusting the socket trusts no TCP peer
    assert_eq!(from(&u, "127.0.0.1", &x), k("127.0.0.1"));
    assert!(TrustedProxies::parse(&["socket".into()]).is_err());
    assert!(ForwardHeader::parse("X-Real-IP").is_err());
    assert_eq!(
        ForwardHeader::parse("X-Forwarded-For").unwrap(),
        ForwardHeader::XForwardedFor
    );
    let j: Config = serde_json::from_str(r#"{"trustedProxyHeader":"x-real-ip"}"#).unwrap();
    assert!(j.validate().is_err());
    let j: Config = serde_json::from_str(
        r#"{"trustedProxies":["unix","10.0.0.0/8"],"trustedProxyHeader":"forwarded"}"#,
    )
    .unwrap();
    let t = j.trusted().unwrap();
    assert!(t.unix() && t.contains(ip("10.1.1.1")));
    // IPv6 clients share a key per /64; mapped IPv4 is IPv4
    assert_eq!(k("2001:db8::1"), k("2001:db8::ffff"));
    assert_ne!(k("2001:db8::1"), k("2001:db8:0:1::1"));
    assert_eq!(k("::ffff:192.0.2.1"), k("192.0.2.1"));
    assert!(
        TrustedProxies::parse(&["10.0.0.0/8".into()])
            .unwrap()
            .contains(ip("::ffff:10.1.1.1"))
    );
}

#[test]
fn listeners_behind_an_untrusted_proxy_are_warned_about() {
    let cfg = |trusted: &[&str]| Config {
        trusted_proxies: trusted.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    // without auth: nothing to say
    assert!(client_warnings(None, false, true, true).is_empty());
    let w = client_warnings(None, true, true, false);
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("--rate-limit-trusted-proxy unix"), "{w:?}");
    assert!(client_warnings(Some(&cfg(&["unix"])), true, true, false).is_empty());
    let w = client_warnings(Some(&cfg(&["unix"])), true, false, true);
    assert!(
        w[0].contains("--rate-limit-trusted-proxy 127.0.0.1"),
        "{w:?}"
    );
    assert!(client_warnings(Some(&cfg(&["127.0.0.1"])), true, false, true).is_empty());
    // a network listener is not assumed to be behind a proxy
    assert!(client_warnings(None, true, false, false).is_empty());
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
    // a client-sent Forwarded does not override the proxy's X-Forwarded-For
    let spoof = [
        ("x-forwarded-for", "203.0.113.1"),
        ("forwarded", "for=198.51.100.77"),
    ];
    let r = s.call("GET", Q, "10.0.0.5", &spoof).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    // an untrusted peer cannot choose its key; its headers are counted (and warned about)
    assert_eq!(s.rl.stats().untrusted_forwarded, 0);
    let r = s.call("GET", Q, "198.51.100.1", &via("203.0.113.9")).await;
    assert_eq!(r.status(), StatusCode::OK);
    let r = s.call("GET", Q, "198.51.100.1", &via("203.0.113.10")).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(s.rl.stats().untrusted_forwarded, 2);
    assert!(s.rl.forwarded_warned.load(Ordering::Relaxed));
}

#[tokio::test]
async fn the_forwarded_header_is_read_only_when_configured() {
    let mut cfg = Config::default();
    cfg.apply_flag("query=1/min").unwrap();
    cfg.trusted_proxies = vec!["10.0.0.0/8".into()];
    cfg.trusted_proxy_header = Some("forwarded".into());
    let s = server_cfg(cfg);
    let r = s
        .call("GET", Q, "10.0.0.5", &[("forwarded", "for=203.0.113.1")])
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    // X-Forwarded-For is now the header clients may set
    let h = [
        ("forwarded", "for=203.0.113.1"),
        ("x-forwarded-for", "198.51.100.1"),
    ];
    let r = s.call("GET", Q, "10.0.0.5", &h).await;
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
        trusted_proxy_header: Some("forwarded".into()),
        auth: false,
    };
    let c = s.load().unwrap().unwrap();
    assert_eq!(c.classes["query"].rate.unwrap().count, 50);
    assert_eq!(c.classes["update"].rate.unwrap().count, 1);
    assert_eq!(c.trusted_proxies, ["10.0.0.0/8"]);
    assert_eq!(c.trusted_proxy_header.as_deref(), Some("forwarded"));
    assert!(Sources::default().load().unwrap().is_none());
    // trusted proxies alone: a limiter without limits, to name clients
    let proxies = Sources {
        trusted_proxies: vec!["unix".into()],
        ..Default::default()
    };
    let c = proxies.load().unwrap().unwrap();
    assert!(c.is_empty() && c.trusted().unwrap().unix());
    let bad = Sources {
        trusted_proxy_header: Some("x-real-ip".into()),
        ..Default::default()
    };
    assert!(bad.load().is_err());
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

// ---------------------------------------------------- before authentication ------

fn limiter(flags: &[&str]) -> (Arc<RateLimiter>, Arc<AtomicU64>) {
    let mut cfg = Config::default();
    for f in flags {
        cfg.apply_flag(f).unwrap();
    }
    let clock = Arc::new(AtomicU64::new(0));
    let rl = RateLimiter::new(&cfg)
        .unwrap()
        .with_clock(Clock::Manual(clock.clone()));
    (Arc::new(rl), clock)
}

/// The pre-authentication stage around a stub of the auth layer: `x-reserve` reserves a
/// failure first (as a password check does), `x-fail` answers 401.
fn preauth_app(rl: Arc<RateLimiter>) -> Router {
    async fn stub(req: Request, next: Next) -> Response {
        let (reserve, fail) = (
            req.headers().contains_key("x-reserve"),
            req.headers().contains_key("x-fail"),
        );
        if reserve
            && let Some(a) = req.extensions().get::<Admission>()
            && !a.reserve()
        {
            return a.refusal();
        }
        if fail {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        next.run(req).await
    }
    Router::new()
        .route("/{ds}/sparql", axum::routing::get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(stub))
        .layer(axum::middleware::from_fn_with_state(Some(rl), admit))
}

async fn send(app: &Router, peer: &str, headers: &[&str]) -> Response {
    let mut b = HttpRequest::get("/ds/sparql");
    for h in headers {
        b = b.header(*h, "1");
    }
    let mut req = b.body(Body::empty()).unwrap();
    let addr: SocketAddr = format!("{peer}:1").parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(addr));
    app.clone().oneshot(req).await.unwrap()
}

#[test]
fn preauth_is_a_server_wide_failure_budget() {
    let mut c = Config::default();
    assert!(c.apply_flag("preauth@ds=1/s").is_err());
    assert!(c.apply_flag("preauth=1/s,concurrency=2").is_err());
    c.apply_flag("preauth=10/min,burst=20,failure-cost=2")
        .unwrap();
    assert_eq!(c.classes["preauth"].failure_cost, Some(2));
    let j: Config =
        serde_json::from_str(r#"{"datasets":{"x":{"preauth":{"rate":"1/s"}}}}"#).unwrap();
    assert!(j.validate().is_err());
    let j: Config =
        serde_json::from_str(r#"{"classes":{"preauth":{"clientConcurrency":1}}}"#).unwrap();
    assert!(j.validate().is_err());
    // on by default with auth; `off` turns it off
    let on = Sources {
        auth: true,
        ..Default::default()
    };
    let cfg = on.load().unwrap().unwrap();
    assert_eq!(
        cfg.classes["preauth"],
        Limit::parse(DEFAULT_PREAUTH).unwrap()
    );
    let off = Sources {
        auth: true,
        flags: vec!["preauth=off".into()],
        ..Default::default()
    };
    assert!(off.load().unwrap().is_none());
    let tuned = Sources {
        auth: true,
        flags: vec!["preauth=5/min".into()],
        ..Default::default()
    };
    let cfg = tuned.load().unwrap().unwrap();
    assert_eq!(cfg.classes["preauth"].rate.unwrap().count, 5);
}

#[tokio::test]
async fn preauth_refuses_addresses_that_failed_too_often() {
    let (rl, clock) = limiter(&["preauth=3/min"]);
    let app = preauth_app(rl.clone());
    // successes cost nothing, and leave no state behind
    for _ in 0..5 {
        let r = send(&app, "192.0.2.1", &[]).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert!(r.headers().get("ratelimit").is_none());
    }
    assert_eq!(rl.tracked(), 0);
    for (remaining, reset) in [(2, 20), (1, 40), (0, 60)] {
        let r = send(&app, "192.0.2.1", &["x-fail"]).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(hdr(&r, "ratelimit-policy"), "\"preauth\";q=3;w=60");
        assert_eq!(
            hdr(&r, "ratelimit"),
            format!("\"preauth\";r={remaining};t={reset}")
        );
    }
    // spent: every request of the address is refused, before authentication
    let r = send(&app, "192.0.2.1", &[]).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hdr(&r, "retry-after"), "20");
    let j = body_json(r).await;
    assert_eq!(j["limitClass"], "preauth");
    assert_eq!(j["reason"], "failures");
    assert!(
        j["error"]
            .as_str()
            .unwrap()
            .contains("too many failed authentications")
    );
    // other addresses are not affected
    assert_eq!(send(&app, "192.0.2.2", &[]).await.status(), StatusCode::OK);
    // a refused request costs nothing more; one failure refills in 20 s
    clock.fetch_add(20_000_000_000, Ordering::Relaxed);
    assert_eq!(send(&app, "192.0.2.1", &[]).await.status(), StatusCode::OK);
    assert_eq!(
        send(&app, "192.0.2.1", &["x-fail"]).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&app, "192.0.2.1", &[]).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn reservations_are_refunded_unless_the_request_fails() {
    let (rl, _) = limiter(&["preauth=2/min"]);
    let app = preauth_app(rl.clone());
    for _ in 0..5 {
        let r = send(&app, "192.0.2.1", &["x-reserve"]).await;
        assert_eq!(r.status(), StatusCode::OK);
    }
    // a reserved check that fails costs one failure, not two
    let r = send(&app, "192.0.2.1", &["x-reserve", "x-fail"]).await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(hdr(&r, "ratelimit"), "\"preauth\";r=1;t=30");
    // concurrent checks cannot all start: a reservation holds the failure it may cost
    let inner = rl.inner.load_full();
    let policy = inner.by_class[Class::PreAuth.index()].unwrap();
    let admission = || {
        Admission(Arc::new(AdmissionState {
            inner: inner.clone(),
            policy,
            client: ClientKey::ip("192.0.2.1".parse().unwrap()),
            clock: rl.clock.clone(),
            reserved: AtomicU32::new(0),
        }))
    };
    let (a, b) = (admission(), admission());
    assert!(a.reserve());
    assert!(a.reserve(), "a request reserves once");
    assert!(!b.reserve());
    let r = b.refusal();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hdr(&r, "retry-after"), "30");
}

// ------------------------------------------------------------ named limits ------

#[tokio::test]
async fn named_limits_are_charged_by_code() {
    let mut cfg = Config::default();
    cfg.named
        .insert("mint", (Limit::parse("2/h").unwrap(), "tokens minted"));
    let rl = RateLimiter::new(&cfg)
        .unwrap()
        .with_clock(Clock::Manual(Arc::new(AtomicU64::new(0))));
    let bob = ClientKey::principal("user:bob");
    let alice = ClientKey::principal("user:alice");
    assert!(rl.acquire("mint", bob.clone(), 1).is_ok());
    assert!(rl.acquire("mint", bob.clone(), 1).is_ok());
    let r = rl.acquire("mint", bob.clone(), 1).unwrap_err();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(hdr(&r, "retry-after"), "1800");
    assert_eq!(hdr(&r, "ratelimit-policy"), "\"mint\";q=2;w=3600");
    let j = body_json(r).await;
    assert_eq!(j["limitClass"], "auth");
    assert_eq!(j["reason"], "mint");
    assert_eq!(j["error"], "too many tokens minted: retry in 1800 s");
    // per key; a name that is not configured is not limited
    assert!(rl.acquire("mint", alice.clone(), 1).is_ok());
    assert!(rl.acquire("other", bob.clone(), 1).is_ok());
    // failures found after the fact: charge, then check
    assert!(rl.check("mint", &alice).is_ok());
    rl.charge("mint", alice.clone(), 1);
    assert!(rl.check("mint", &alice).is_err());
    // a reload keeps the debts of an unchanged limit
    rl.reload(&cfg).unwrap();
    assert!(rl.acquire("mint", bob, 1).is_err());
}

// ------------------------------------------------------- bounded state ------

#[tokio::test]
async fn evicted_debts_are_remembered_and_evictions_counted() {
    let mut cfg = Config::default();
    cfg.apply_flag("query=1/min").unwrap();
    cfg.max_keys = Some(64);
    let clock = Arc::new(AtomicU64::new(0));
    let rl = Arc::new(
        RateLimiter::new(&cfg)
            .unwrap()
            .with_clock(Clock::Manual(clock.clone())),
    );
    let app = Router::new()
        .route("/{ds}/sparql", axum::routing::get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(rl.clone(), limit));
    assert_eq!(send(&app, "192.0.2.1", &[]).await.status(), StatusCode::OK);
    assert_eq!(
        send(&app, "192.0.2.1", &[]).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // churn the cache with clients seen more often until the first one is evicted
    let inner = rl.inner.load_full();
    let key = SlotKey {
        policy: inner.policies[0].id,
        client: ClientKey::ip("192.0.2.1".parse().unwrap()),
    };
    let mut i = 0u32;
    while inner.buckets.slots.peek(&key).is_some() {
        assert!(i < 50_000, "never evicted");
        let [_, _, a, b] = i.to_be_bytes();
        for _ in 0..3 {
            send(&app, &format!("198.51.{a}.{b}"), &[]).await;
        }
        i += 1;
    }
    let st = rl.stats();
    assert!(st.evictions > 0);
    assert!(st.penalties >= 1);
    assert_eq!(st.max_keys, 64);
    // evicted, but its debt is not forgiven
    assert_eq!(
        send(&app, "192.0.2.1", &[]).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // paid debts are swept
    clock.fetch_add(3_600_000_000_000, Ordering::Relaxed);
    rl.sweep();
    assert_eq!(rl.stats().penalties, 0);
    let mut text = String::new();
    render_metrics(&mut text, &[("requests", &rl)]);
    let evictions = rl.stats().evictions;
    assert!(
        text.contains(&format!(
            "sparkles_rate_limit_evictions_total{{limiter=\"requests\"}} {evictions}"
        )),
        "{text}"
    );
    assert!(text.contains("sparkles_rate_limit_max_keys{limiter=\"requests\"} 64"));
}

#[tokio::test]
async fn limiter_state_is_in_the_metrics() {
    let s = server(&["query=1/min"], &[]);
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
        text.contains("sparkles_rate_limit_keys{limiter=\"requests\"} 1"),
        "{text}"
    );
    assert!(text.contains("sparkles_rate_limit_evictions_total{limiter=\"requests\"} 0"));
    assert!(text.contains("sparkles_rate_limit_penalties{limiter=\"requests\"} 0"));
    assert!(text.contains("sparkles_rate_limited_total{dataset=\"ds\",class=\"preauth\"} 0"));
}

#[tokio::test]
async fn reload_keeps_debts_and_counts_requests_in_flight() {
    let s = server(&["query=1/min"], &[]);
    assert_eq!(s.get(Q, "192.0.2.1").await.status(), StatusCode::OK);
    let mut same = Config::default();
    same.apply_flag("query=1/min").unwrap();
    same.apply_flag("update=5/s").unwrap();
    s.rl.reload(&same).unwrap();
    // the debt survives the reload
    assert_eq!(
        s.get(Q, "192.0.2.1").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // so does the state when maxKeys changes
    same.max_keys = Some(1000);
    s.rl.reload(&same).unwrap();
    assert_eq!(
        s.get(Q, "192.0.2.1").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    // server-wide concurrency under traffic: four streamed responses in flight
    let s = server(&["query=concurrency=4"], &[]);
    let mut held = Vec::new();
    for i in 0..4 {
        let r = s.get("/ds/data", &format!("192.0.2.{i}")).await;
        assert_eq!(r.status(), StatusCode::OK);
        held.push(r);
    }
    let cap = |n: u32| {
        let mut c = Config::default();
        c.apply_flag(&format!("query=concurrency={n}")).unwrap();
        c
    };
    // a lower cap counts the requests already running: nothing new until they drop
    // below it
    s.rl.reload(&cap(2)).unwrap();
    assert_eq!(
        s.get(Q, "192.0.2.9").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    held.truncate(2);
    assert_eq!(
        s.get(Q, "192.0.2.9").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    held.truncate(1);
    assert_eq!(s.get(Q, "192.0.2.9").await.status(), StatusCode::OK);
    // a higher cap admits up to it, not up to a fresh count
    s.rl.reload(&cap(3)).unwrap();
    for i in 0..2 {
        let r = s.get("/ds/data", &format!("192.0.2.{}", 20 + i)).await;
        assert_eq!(r.status(), StatusCode::OK);
        held.push(r);
    }
    assert_eq!(
        s.get(Q, "192.0.2.9").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(held);
    assert_eq!(s.get(Q, "192.0.2.9").await.status(), StatusCode::OK);
}
