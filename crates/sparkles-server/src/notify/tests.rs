//! Outbound notifications (spec C21) in the process, against a local HTTP receiver:
//! signed webhooks, ntfy, retries, refusals by the outbound policy, repeats of lasting
//! conditions, the memory review and backup events, and the settings kind with its
//! locks.

use super::*;
use crate::state::DbType;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::Ordering;
use tower::ServiceExt;

/// One request the receiver got.
#[derive(Clone, Debug)]
struct Got {
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

/// A local HTTP receiver that answers with `statuses` in turn, then 200.
struct Receiver {
    port: u16,
    got: Arc<Mutex<Vec<Got>>>,
}

impl Receiver {
    fn start(statuses: Vec<u16>) -> Receiver {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let got: Arc<Mutex<Vec<Got>>> = Arc::default();
        let g = got.clone();
        std::thread::spawn(move || {
            let mut statuses = statuses.into_iter();
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut headers = BTreeMap::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let n: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0; n];
                let _ = r.read_exact(&mut body);
                g.lock().push(Got {
                    path,
                    headers,
                    body,
                });
                let s = statuses.next().unwrap_or(200);
                let text = if s == 200 {
                    "{}"
                } else {
                    "{\"error\":\"nope\"}"
                };
                let _ = write!(
                    c,
                    "HTTP/1.1 {s} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        Receiver { port, got }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/hook", self.port)
    }

    fn got(&self) -> Vec<Got> {
        self.got.lock().clone()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    st: Arc<AppState>,
    app: axum::Router,
}

/// A server with the settings file `settings` and the outbound policy's
/// `allow_private`.
fn fixture(settings: Option<Value>, allow_private: bool) -> Fixture {
    fixture_with(settings, allow_private, None)
}

/// [`fixture`] with an outbound policy that also trusts the PEM roots `roots`.
fn fixture_with(settings: Option<Value>, allow_private: bool, roots: Option<&[u8]>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        &dir.path().join("data"),
        sparkles::store::StoreOptions::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    st.outbound.allow_private = allow_private;
    st.outbound.tls.extra_roots_pem = roots.map(<[u8]>::to_vec);
    let file = settings.map(|s| {
        let f = dir.path().join("settings.json");
        std::fs::write(&f, s.to_string()).unwrap();
        f
    });
    crate::settings::server::start(&mut st, file.as_deref(), &Default::default()).unwrap();
    let st = Arc::new(st);
    st.set_phase(crate::obs::Phase::Ready);
    let app = crate::http::router(st.clone());
    Fixture { _dir: dir, st, app }
}

impl Fixture {
    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let b = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        };
        let res = self.app.clone().oneshot(req).await.unwrap();
        let s = res.status();
        let b = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    async fn patch(&self, body: Value) -> (StatusCode, Value) {
        self.send("PATCH", "/$/server/settings/notifications", Some(body))
            .await
    }

    /// Store a runtime secret, as `PUT /$/server/secrets/{name}` does.
    fn secret(&self, name: &str, value: &str) {
        let dir = self.st.settings.server.secrets_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), value).unwrap();
    }

    async fn metrics(&self) -> String {
        let mut out = String::new();
        metrics(&self.st, &mut out);
        out
    }
}

/// Wait until the channel has `n` final results.
async fn settled(st: &AppState, channel: &str, n: u64) -> Value {
    for _ in 0..200 {
        let v = status_json(st);
        let c = v["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == channel)
            .cloned()
            .unwrap();
        if c["sent"].as_u64().unwrap() + c["failed"].as_u64().unwrap() >= n {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no result for {channel}: {}", status_json(st));
}

fn fast() -> Value {
    json!({ "attempts": 3, "backoffSecs": 0.05, "maxBackoffSecs": 1, "timeoutSecs": 5 })
}

fn verify(g: &Got, key: &[u8]) {
    let id = &g.headers["webhook-id"];
    let ts: i64 = g.headers["webhook-timestamp"].parse().unwrap();
    assert!((Utc::now().timestamp() - ts).abs() < 60);
    assert_eq!(
        g.headers["webhook-signature"],
        sign::standard(key, id, ts, &g.body)
    );
}

/// A1, A9: a test send to a signed webhook works while notifications are off, and the
/// signature verifies.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_send_signs_the_webhook() {
    let rx = Receiver::start(vec![]);
    let f = fixture(None, true);
    f.secret("hook-signing", "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw");
    let (s, v) = f
        .patch(json!({"channels": {"hook": {"type": "webhook", "url": rx.url(), "signingSecret": {"secret": "hook-signing"}}}}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["effective"]["enabled"], false);
    let (s, v) = f.send("POST", "/$/notifications/test/hook", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["result"], "ok");
    let got = rx.got();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path, "/hook");
    verify(
        &got[0],
        &sign::key_of("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw"),
    );
    let env: Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(env["type"], "notification.test");
    assert_eq!(env["version"], 1);
    assert_eq!(env["id"], got[0].headers["webhook-id"].as_str());
    // A9: an event is not delivered while notifications are off
    let r = raise(
        &f.st,
        Event {
            kind: "backup.failed",
            dataset: None,
            severity: Severity::Critical,
            title: "t".into(),
            summary: "s".into(),
            link: None,
            data: json!({}),
        },
        None,
        Utc::now(),
    );
    assert_eq!(r, Raised::Disabled);
    let (s, v) = f.send("POST", "/$/notifications/test/nope", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    // A10: no answer holds the secret's value
    let (_, v) = f.send("GET", "/$/notifications", None).await;
    assert!(!v.to_string().contains("MfKQ9r8"), "{v}");
    let (_, v) = f.send("GET", "/$/server/secrets", None).await;
    assert_eq!(v["secrets"][0]["channels"], json!(["hook"]), "{v}");
}

fn event(kind: &'static str) -> Event {
    Event {
        kind,
        dataset: None,
        severity: Severity::Warning,
        title: "Title".into(),
        summary: "Summary".into(),
        link: Some("/ui/server".into()),
        data: json!({ "x": 1 }),
    }
}

/// A2: two 503 answers and then a 200, after waits; A3: a 400 is tried once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_failures_are_retried() {
    let flaky = Receiver::start(vec![503, 503]);
    let bad = Receiver::start(vec![400]);
    let f = fixture(None, true);
    let (s, v) = f
        .patch(json!({
            "enabled": true,
            "baseUrl": "https://sparkles.example.org/",
            "channels": {
                "flaky": {"type": "webhook", "url": flaky.url()},
                "bad": {"type": "webhook", "url": bad.url()}
            },
            "routes": {"backup.*": ["flaky"], "*": ["bad"]},
            "delivery": fast(),
        }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let started = Instant::now();
    let r = raise(&f.st, event("backup.failed"), None, Utc::now());
    assert_eq!(r, Raised::Queued(vec!["bad".into(), "flaky".into()]));
    let v = settled(&f.st, "flaky", 1).await;
    // 0.05 s then 0.1 s
    assert!(started.elapsed() >= Duration::from_millis(140));
    let v = if v["channels"][0]["failed"] == 0 {
        settled(&f.st, "bad", 1).await
    } else {
        v
    };
    let got = flaky.got();
    assert_eq!(got.len(), 3);
    let env: Value = serde_json::from_slice(&got[2].body).unwrap();
    assert_eq!(env["link"], "https://sparkles.example.org/ui/server");
    // the same id on each attempt
    let first: Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(first["id"], env["id"]);
    assert_eq!(bad.got().len(), 1);
    let ch = |n: &str| {
        v["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == n)
            .cloned()
            .unwrap()
    };
    assert_eq!(ch("flaky")["lastSuccess"]["status"], 200);
    assert!(
        ch("bad")["lastFailure"]["error"]
            .as_str()
            .unwrap()
            .starts_with("HTTP 400")
    );
    let m = f.metrics().await;
    assert!(m.contains(r#"sparkles_notifications_sent_total{channel="flaky",event="backup.failed",result="ok"} 1"#), "{m}");
    assert!(m.contains(r#"sparkles_notifications_sent_total{channel="bad",event="backup.failed",result="failed"} 1"#), "{m}");
    assert!(
        m.contains(r#"sparkles_notifications_retries_total{channel="flaky"} 2"#),
        "{m}"
    );
    let recent = v["recent"].as_array().unwrap();
    assert!(
        recent
            .iter()
            .any(|r| r["channel"] == "flaky" && r["attempts"] == 3)
    );
}

/// A4: a channel on loopback without `--outbound-allow-private` is never contacted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_outbound_policy_refuses_private_channels() {
    let rx = Receiver::start(vec![]);
    let f = fixture(None, false);
    let (s, v) = f
        .patch(json!({
            "enabled": true,
            "channels": {"hook": {"type": "webhook", "url": rx.url()}},
            "routes": {"*": ["hook"]},
            "delivery": fast(),
        }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    raise(&f.st, event("backup.failed"), None, Utc::now());
    let v = settled(&f.st, "hook", 1).await;
    assert!(
        v["channels"][0]["lastFailure"]["error"]
            .as_str()
            .unwrap()
            .contains("loopback")
    );
    let (s, v) = f.send("POST", "/$/notifications/test/hook", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert_eq!(v["code"], "outbound-refused");
    assert!(rx.got().is_empty());
    assert!(f.metrics().await.contains(r#"result="refused"} 1"#));
}

/// ntfy: the JSON publish API with a bearer token from a secret, and the priority of
/// the severity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ntfy_publishes_json() {
    let rx = Receiver::start(vec![]);
    let f = fixture(None, true);
    f.secret("ntfy-token", "tk_secretvalue");
    let (s, v) = f
        .patch(json!({
            "enabled": true,
            "baseUrl": "https://sparkles.example.org",
            "channels": {"phone": {"type": "ntfy", "server": format!("http://127.0.0.1:{}", rx.port), "topic": "ops", "token": {"secret": "ntfy-token"}, "tags": ["db"]}},
            "routes": {"backup.failed": ["phone"]},
        }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    raise(&f.st, event("backup.failed"), None, Utc::now());
    settled(&f.st, "phone", 1).await;
    let got = rx.got();
    assert_eq!(got[0].path, "/");
    assert_eq!(got[0].headers["authorization"], "Bearer tk_secretvalue");
    let m: Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(m["topic"], "ops");
    assert_eq!(m["title"], "Title");
    assert_eq!(m["message"], "Summary");
    assert_eq!(m["priority"], 4);
    assert_eq!(m["tags"], json!(["warning", "db"]));
    assert_eq!(m["click"], "https://sparkles.example.org/ui/server");
    let (_, v) = f.send("GET", "/$/notifications", None).await;
    assert!(!v.to_string().contains("tk_secretvalue"));
}

/// A5: the memory review event end to end, with its repeat and its clearing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn memory_review_notifies_then_repeats() {
    let rx = Receiver::start(vec![]);
    let f = fixture(None, true);
    let (s, v) = f
        .patch(json!({
            "enabled": true,
            "channels": {"hook": {"type": "webhook", "url": rx.url()}},
            "routes": {"memory.review.pending": ["hook"]},
        }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ds = f.st.attach("org", DbType::Mem, None).unwrap();
    // no notifyAfter: nothing
    let t0 = Utc::now();
    assert_eq!(events::memory_review(&f.st, "org", t0), None);
    let (s, v) = f
        .send(
            "PATCH",
            "/$/settings/org/memory",
            Some(json!({"review": {"notifyAfter": "2d", "repeatEvery": "1d"}})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = f
        .send(
            "PATCH",
            "/$/settings/org/memory",
            Some(json!({"review": {"notifyAfter": "5s"}})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    // a review branch without commits of its own is not open
    ds.dataset
        .create_branch("consolidation.20261007-1", &Default::default())
        .unwrap();
    assert_eq!(events::memory_review(&f.st, "org", t0), None);
    ds.dataset
        .branch("consolidation.20261007-1")
        .unwrap()
        .update("INSERT DATA { <urn:a> <urn:p> 1 }")
        .unwrap();
    // too young
    let day = TimeDelta::days(1);
    assert_eq!(events::memory_review(&f.st, "org", t0 + day), None);
    let t = t0 + day * 3;
    assert_eq!(
        events::memory_review(&f.st, "org", t),
        Some(Raised::Queued(vec!["hook".into()]))
    );
    for h in [1, 12, 23] {
        assert_eq!(
            events::memory_review(&f.st, "org", t + TimeDelta::hours(h)),
            Some(Raised::Suppressed)
        );
    }
    assert_eq!(
        events::memory_review(&f.st, "org", t + day),
        Some(Raised::Queued(vec!["hook".into()]))
    );
    settled(&f.st, "hook", 2).await;
    let got = rx.got();
    assert_eq!(got.len(), 2);
    let env: Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(env["type"], "memory.review.pending");
    assert_eq!(env["dataset"], "org");
    assert_eq!(env["data"]["open"], 1);
    assert_eq!(env["data"]["oldest"]["branch"], "consolidation.20261007-1");
    assert_eq!(env["link"], "/ui/memory?ds=org&tab=inbox");
    assert!(is_active(&f.st, "memory.review.pending/org"));
    // the state survives in the data directory
    let saved = std::fs::read_to_string(f.st.data_dir.join(STATE_FILE)).unwrap();
    assert!(saved.contains("memory.review.pending/org"), "{saved}");
    // reviewed: the condition clears, and a new branch notifies once it is old enough
    ds.dataset
        .delete_branch("consolidation.20261007-1", true)
        .unwrap();
    assert_eq!(events::memory_review(&f.st, "org", t + day * 2), None);
    assert!(!is_active(&f.st, "memory.review.pending/org"));
    ds.dataset
        .create_branch("review.alice.1", &Default::default())
        .unwrap();
    ds.dataset
        .branch("review.alice.1")
        .unwrap()
        .update("INSERT DATA { <urn:b> <urn:p> 1 }")
        .unwrap();
    assert_eq!(
        events::memory_review(&f.st, "org", Utc::now() + day * 2 + TimeDelta::hours(1)),
        Some(Raised::Queued(vec!["hook".into()]))
    );
    assert!(
        f.metrics().await.contains(
            r#"sparkles_notifications_suppressed_total{event="memory.review.pending"} 3"#
        )
    );
}

/// A8: a failing backup policy notifies once per `repeatEvery`, and a success clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_failures_repeat_and_clear() {
    let rx = Receiver::start(vec![]);
    let f = fixture(None, true);
    f.patch(json!({
        "enabled": true,
        "repeatEvery": "12h",
        "channels": {"hook": {"type": "webhook", "url": rx.url()}},
        "routes": {"backup.failed": ["hook"]},
    }))
    .await;
    let run = json!({"id": "r1", "datasets": [{"dataset": "a", "result": "ok"}, {"dataset": "b", "result": "failed", "reason": "disk full"}]});
    let t = Utc::now();
    let q = Some(Raised::Queued(vec!["hook".into()]));
    assert_eq!(
        events::backup_run(&f.st, "nightly", "local", "partial", &run, 1, t),
        q
    );
    assert_eq!(
        events::backup_run(
            &f.st,
            "nightly",
            "local",
            "failed",
            &run,
            2,
            t + TimeDelta::hours(1)
        ),
        Some(Raised::Suppressed)
    );
    assert_eq!(
        events::backup_run(
            &f.st,
            "nightly",
            "local",
            "ok",
            &run,
            0,
            t + TimeDelta::hours(2)
        ),
        None
    );
    assert_eq!(
        events::backup_run(
            &f.st,
            "nightly",
            "local",
            "failed",
            &run,
            1,
            t + TimeDelta::hours(3)
        ),
        q
    );
    settled(&f.st, "hook", 2).await;
    let env: Value = serde_json::from_slice(&rx.got()[0].body).unwrap();
    assert_eq!(env["type"], "backup.failed");
    assert_eq!(env["severity"], "warning");
    assert_eq!(
        env["data"]["failedDatasets"],
        json!([{"dataset": "b", "reason": "disk full"}])
    );
    assert!(
        env["summary"]
            .as_str()
            .unwrap()
            .contains("did not back up b")
    );
}

/// A6, A7: locks from the settings file, inline credentials and unknown channels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_settings_kind_has_layers_and_locks() {
    let f = fixture(
        Some(json!({"server": {
            "notifications": {
                "channels": {"phone": {"type": "ntfy", "topic": "ops"}},
                "routes": {"*": ["phone"]}
            },
            "locked": ["notifications.channels.phone"]
        }})),
        true,
    );
    let (s, v) = f
        .send("GET", "/$/server/settings/notifications", None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["effective"]["channels"]["phone"]["topic"], "ops");
    assert_eq!(v["sources"]["channels.phone.topic"], "locked");
    assert_eq!(v["sources"]["routes.*"], "declared");
    let (s, v) = f
        .patch(json!({"channels": {"phone": {"type": "ntfy", "topic": "other"}}}))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["code"], "locked-by-config");
    let (s, v) = f.patch(json!({"channels": {"phone": null}})).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, v) = f
        .patch(json!({"enabled": true, "channels": {"hook": {"type": "webhook", "url": "https://hooks.example.org/x"}}}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["sources"]["enabled"], "runtime");
    // A7
    let (s, v) = f
        .patch(
            json!({"channels": {"n2": {"type": "ntfy", "topic": "t", "token": "tk_inline_value"}}}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(!v.to_string().contains("tk_inline_value"), "{v}");
    let (s, v) = f.patch(json!({"routes": {"backup.*": ["nope"]}})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    for url in [
        "https://user:pw@hooks.example.org/x",
        "https://hooks.example.org/x?token=abc",
        "ftp://hooks.example.org/x",
    ] {
        let (s, v) = f
            .patch(json!({"channels": {"h2": {"type": "webhook", "url": url}}}))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{url}: {v}");
    }
    // the runtime layer survives in its file
    let saved = std::fs::read_to_string(f.st.data_dir.join(config::NOTIFICATIONS_FILE)).unwrap();
    assert!(saved.contains("hooks.example.org"), "{saved}");
    let (_, v) = f.send("GET", "/$/settings", None).await;
    assert_eq!(
        v["serverKinds"],
        json!(["models", "notifications", "connections"])
    );
}

#[test]
fn the_settings_file_checks_notifications() {
    let parse = |v: Value| {
        crate::settings::Declared::parse(&v.to_string(), crate::settings::Providers::Unchecked)
    };
    assert!(parse(json!({"server": {"notifications": {"enabled": true}}})).is_ok());
    let e = parse(json!({"server": {"notifications": {"routes": {"*": ["x"]}}}})).unwrap_err();
    assert!(e.contains("server.notifications"), "{e}");
    let e = parse(json!({"server": {"locked": ["notifications.channel"]}})).unwrap_err();
    assert!(e.contains("no member"), "{e}");
    assert!(parse(json!({"server": {"locked": ["notifications.delivery.attempts"]}})).is_ok());
    assert!(parse(json!({"server": {"models": {}}})).is_err());
}

#[test]
fn routes_and_backoff() {
    let s: NotifySettings = serde_json::from_value(json!({
        "channels": {"a": {"type": "ntfy", "topic": "t"}, "b": {"type": "ntfy", "topic": "u"}},
        "routes": {"backup.*": ["a"], "backup.failed": ["a", "b"], "memory.review.pending": ["b"]}
    }))
    .unwrap();
    assert_eq!(s.route("backup.failed"), ["a", "b"]);
    assert_eq!(s.route("backup.other"), ["a"]);
    assert_eq!(s.route("backupx.failed"), Vec::<String>::new());
    assert_eq!(s.route("memory.review.pending"), ["b"]);
    let d = Delivery::default();
    assert_eq!(d.wait(2, None), Duration::from_secs(30));
    assert_eq!(d.wait(3, None), Duration::from_secs(60));
    assert_eq!(d.wait(20, None), Duration::from_secs(1800));
    assert_eq!(d.wait(2, Some(90.0)), Duration::from_secs(90));
    assert_eq!(d.wait(2, Some(9e9)), Duration::from_secs(1800));
}

#[test]
fn a_newer_state_file_is_refused() {
    let writes = sparkles::formats::settings_file(STATE_FILE).unwrap().writes;
    let newer = format!(r#"{{"format": {}, "active": {{}}}}"#, writes + 1);
    assert!(matches!(
        parse_state(newer.as_bytes()),
        Err(sparkles::error::Error::Unsupported(_))
    ));
    assert!(parse_state(br#"{"active": {}}"#).unwrap().is_empty());
    let saved = sparkles::formats::settings_to_vec(STATE_FILE, &StateFile::default()).unwrap();
    assert!(parse_state(&saved).is_ok());
}

/// A local server that accepts connections and never answers, as a hung webhook does.
struct Stalled {
    port: u16,
    accepted: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Stalled {
    fn start() -> Stalled {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let accepted: Arc<std::sync::atomic::AtomicUsize> = Arc::default();
        let stop: Arc<std::sync::atomic::AtomicBool> = Arc::default();
        let (a, s) = (accepted.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut held = Vec::new();
            for c in l.incoming() {
                if s.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(c) = c else { continue };
                a.fetch_add(1, Ordering::SeqCst);
                held.push(c);
            }
        });
        Stalled {
            port,
            accepted,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/hook", self.port)
    }
}

impl Drop for Stalled {
    /// Stop accepting and close the held connections.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The channel's entry in `GET /$/notifications`.
fn channel_of(v: &Value, name: &str) -> Value {
    v["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .cloned()
        .unwrap()
}

/// A hung channel holds only its own deliveries: an event for another channel, raised
/// after the hung one's, arrives at once while the hung channel waits for its timeout,
/// and raising events never waits for a delivery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_channel_does_not_delay_the_others() {
    let slow = Stalled::start();
    let rx = Receiver::start(vec![]);
    let f = fixture(None, true);
    let (s, v) = f
        .patch(json!({
            "enabled": true,
            "channels": {
                "slow": {"type": "webhook", "url": slow.url()},
                "fast": {"type": "webhook", "url": rx.url()}
            },
            "routes": {"backup.*": ["slow"], "memory.*": ["fast"]},
            "delivery": {"attempts": 1, "backoffSecs": 1, "maxBackoffSecs": 1, "timeoutSecs": 20},
        }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for _ in 0..3 {
        let t = Instant::now();
        raise(&f.st, event("backup.failed"), None, Utc::now());
        assert!(
            t.elapsed() < Duration::from_millis(200),
            "{:?}",
            t.elapsed()
        );
    }
    // the hung channel has started its first attempt
    for _ in 0..200 {
        if slow.accepted.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(slow.accepted.load(Ordering::SeqCst), 1);
    let t = Instant::now();
    raise(&f.st, event("memory.review.pending"), None, Utc::now());
    let v = settled(&f.st, "fast", 1).await;
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    assert_eq!(rx.got().len(), 1);
    assert_eq!(channel_of(&v, "fast")["sent"], 1);
    let s = channel_of(&v, "slow");
    assert_eq!(s["sent"], 0);
    assert_eq!(s["failed"], 0);
    assert_eq!(s["queued"], 2, "{v}");
    assert_eq!(v["queued"], 2, "{v}");
    let m = f.metrics().await;
    assert!(
        m.contains(r#"sparkles_notifications_channel_queued{channel="slow"} 2"#),
        "{m}"
    );
    assert!(m.contains("sparkles_notifications_queued 2"), "{m}");
}

/// Each channel has its share of the queue, at least 50 deliveries: a hung channel
/// that fills its share drops its own new deliveries with the result `dropped`, and
/// another channel still queues and delivers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_channel_queue_drops_only_its_own_deliveries() {
    let slow = Stalled::start();
    let rx = Receiver::start(vec![]);
    let f = fixture(None, true);
    let mut channels = serde_json::Map::new();
    channels.insert("slow".into(), json!({"type": "webhook", "url": slow.url()}));
    channels.insert("fast".into(), json!({"type": "webhook", "url": rx.url()}));
    // 30 channels: a share of 1000 / 30 = 33, raised to the floor of 50
    for i in 0..28 {
        channels.insert(
            format!("idle{i}"),
            json!({"type": "webhook", "url": "https://hooks.example.org/x"}),
        );
    }
    let (s, v) = f
        .patch(json!({
            "enabled": true,
            "channels": channels,
            "routes": {"backup.*": ["slow"], "memory.*": ["fast"]},
            "delivery": {"attempts": 1, "backoffSecs": 1, "maxBackoffSecs": 1, "timeoutSecs": 20},
        }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for _ in 0..60 {
        raise(&f.st, event("backup.failed"), None, Utc::now());
    }
    raise(&f.st, event("memory.review.pending"), None, Utc::now());
    for _ in 0..200 {
        if slow.accepted.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let v = settled(&f.st, "fast", 1).await;
    assert_eq!(channel_of(&v, "fast")["sent"], 1);
    let s = channel_of(&v, "slow");
    // one in flight and 49 waiting fill the share of 50, and the other 10 are dropped
    assert_eq!(s["queueCapacity"], 50, "{v}");
    assert_eq!(s["queued"], 49, "{v}");
    assert_eq!(s["failed"], 10, "{v}");
    assert_eq!(s["lastFailure"]["error"], "the channel's queue is full");
    let m = f.metrics().await;
    assert!(m.contains(r#"sparkles_notifications_sent_total{channel="slow",event="backup.failed",result="dropped"} 10"#), "{m}");
    assert!(
        m.contains(r#"sparkles_notifications_channel_queue_capacity{channel="slow"} 50"#),
        "{m}"
    );
}

/// The kind has at most 32 channels, which bounds the delivery threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_kind_has_at_most_32_channels() {
    let f = fixture(None, true);
    let channels: serde_json::Map<String, Value> = (0..33)
        .map(|i| {
            (
                format!("c{i}"),
                json!({"type": "webhook", "url": "https://hooks.example.org/x"}),
            )
        })
        .collect();
    let (s, v) = f.patch(json!({ "channels": channels })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.to_string().contains("at most 32"), "{v}");
}

/// Channels that were renamed while their deliveries wait still have queues, but the
/// queues of all channels, current and removed, are at most 64, so the threads stay
/// bounded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renamed_channels_do_not_add_threads_without_bound() {
    let slow = Stalled::start();
    let f = fixture(None, true);
    let set = |prefix: &str| {
        let channels: serde_json::Map<String, Value> = (0..32)
            .map(|i| {
                (
                    format!("{prefix}{i}"),
                    json!({"type": "webhook", "url": slow.url()}),
                )
            })
            .collect();
        let names: Vec<String> = channels.keys().cloned().collect();
        json!({
            "enabled": true,
            "channels": channels,
            "routes": {"*": names},
            "delivery": {"attempts": 1, "backoffSecs": 1, "maxBackoffSecs": 1, "timeoutSecs": 20},
        })
    };
    let lanes = |st: &AppState| st.notifier.shared.inner.lock().lanes.len();
    let mut previous: Option<&str> = None;
    for prefix in ["a", "b", "c"] {
        let mut body = set(prefix);
        if let Some(p) = previous {
            for n in 0..32 {
                body["channels"][format!("{p}{n}")] = Value::Null;
            }
        }
        let (s, v) = f.patch(body).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        raise(&f.st, event("backup.failed"), None, Utc::now());
        previous = Some(prefix);
    }
    assert_eq!(lanes(&f.st), MAX_LANES, "{}", status_json(&f.st));
    let m = f.metrics().await;
    assert!(m.contains(r#"sparkles_notifications_sent_total{channel="c0",event="backup.failed",result="dropped"} 1"#), "{m}");
}

/// The queue of a channel that was removed ends once it is empty, without waiting for
/// the idle time of a current channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_channel_gives_back_its_queue() {
    let rx = Receiver::start(vec![]);
    let f = fixture(None, true);
    f.patch(json!({
        "enabled": true,
        "channels": {"hook": {"type": "webhook", "url": rx.url()}},
        "routes": {"*": ["hook"]},
    }))
    .await;
    raise(&f.st, event("backup.failed"), None, Utc::now());
    settled(&f.st, "hook", 1).await;
    assert!(f.st.notifier.shared.inner.lock().lanes.contains_key("hook"));
    let (s, v) = f
        .patch(json!({"channels": {"hook": null}, "routes": {"*": null}}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let mut gone = false;
    for _ in 0..100 {
        if !f.st.notifier.shared.inner.lock().lanes.contains_key("hook") {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(gone, "the removed channel's queue is still there");
}

/// A runtime layer written before the cap, with 33 channels, is not delivered to past
/// the cap of threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runtime_layer_past_the_cap_is_refused_on_reading() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let channels: serde_json::Map<String, Value> = (0..33)
        .map(|i| {
            (
                format!("c{i}"),
                json!({"type": "webhook", "url": "https://hooks.example.org/x"}),
            )
        })
        .collect();
    std::fs::write(
        data.join(config::NOTIFICATIONS_FILE),
        json!({"enabled": true, "channels": channels, "routes": {"*": ["c0"]}}).to_string(),
    )
    .unwrap();
    let mut st = AppState::new(
        &data,
        sparkles::store::StoreOptions::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    crate::settings::server::start(&mut st, None, &Default::default()).unwrap();
    let st = Arc::new(st);
    let cfg = settings(&st);
    assert!(!cfg.enabled);
    assert!(cfg.channels.is_empty());
    assert_eq!(
        raise(&st, event("backup.failed"), None, Utc::now()),
        Raised::Disabled
    );
}
// ------------------------------------------------------------------ email (§4.3) ------

const TEST_CA: &[u8] = include_bytes!("../tls/testdata/ca.pem");
const TEST_CERT: &[u8] = include_bytes!("../tls/testdata/cert-a.pem");
const TEST_KEY: &[u8] = include_bytes!("../tls/testdata/key-a.pem");

trait Rw: Read + Write {}
impl<T: Read + Write> Rw for T {}

/// How the fake mail server offers TLS.
#[derive(Clone, Copy, PartialEq)]
enum SmtpMode {
    /// STARTTLS on a plain connection
    Starttls,
    /// TLS from the first byte
    Tls,
    /// a plain server without STARTTLS
    Plain,
}

/// One message the fake mail server took, or the commands of a session that ended
/// before DATA.
#[derive(Clone, Debug, Default)]
struct Mail {
    /// whether the commands after the greeting ran over TLS
    secure: bool,
    /// `\0user\0password` decoded from AUTH PLAIN
    auth: Option<String>,
    from: String,
    to: Vec<String>,
    data: String,
    commands: Vec<String>,
}

/// How the fake mail server answers, beyond the plain success.
#[derive(Clone, Default)]
struct Script {
    /// the replies to RCPT in turn, then 250
    rcpt: Vec<u16>,
    /// answer AUTH with a 535 that quotes the credentials and the token
    auth_echo: bool,
    /// send the greeting one byte at a time, 300 ms apart
    trickle: bool,
    /// answer 500 to a command line longer than this, CRLF included
    max_command: Option<usize>,
}

/// A local mail server inside the test process. It stops when it is dropped.
struct FakeSmtp {
    port: u16,
    got: Arc<Mutex<Vec<Mail>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn server_tls() -> Arc<rustls::ServerConfig> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(TEST_CERT)
        .map(Result::unwrap)
        .collect();
    let key = PrivateKeyDer::from_pem_slice(TEST_KEY).unwrap();
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap(),
    )
}

fn read_line(s: &mut dyn Rw) -> Option<String> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match s.read(&mut b) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {
                out.push(b[0]);
                if b[0] == b'\n' {
                    return Some(String::from_utf8_lossy(&out).into_owned());
                }
            }
        }
    }
}

enum Next {
    Done,
    Upgrade,
}

/// One SMTP session on `s` until QUIT, a closed connection or STARTTLS.
fn smtp_session(
    s: &mut dyn Rw,
    mode: SmtpMode,
    secure: bool,
    greet: bool,
    script: &Script,
    rcpt: &Mutex<std::vec::IntoIter<u16>>,
    mail: &mut Mail,
) -> Next {
    use base64::Engine as _;
    if greet {
        let g = b"220 fake.example ESMTP\r\n";
        if script.trickle {
            for b in g {
                if s.write_all(&[*b]).is_err() {
                    return Next::Done;
                }
                let _ = s.flush();
                std::thread::sleep(Duration::from_millis(300));
            }
        } else {
            let _ = s.write_all(g);
        }
    }
    mail.secure = secure;
    let decode = |t: &str| {
        let d = base64::engine::general_purpose::STANDARD
            .decode(t.trim())
            .unwrap_or_default();
        String::from_utf8_lossy(&d).into_owned()
    };
    loop {
        let Some(line) = read_line(s) else {
            return Next::Done;
        };
        let cmd = line.trim_end().to_string();
        let upper = cmd.to_ascii_uppercase();
        mail.commands.push(if upper.starts_with("AUTH") {
            "AUTH PLAIN [token]".into()
        } else {
            cmd.clone()
        });
        if script.max_command.is_some_and(|m| line.len() > m) {
            let _ = s.write_all(b"500 line too long\r\n");
            continue;
        }
        let reply: String = if upper.starts_with("EHLO") {
            let tls = if mode == SmtpMode::Starttls && !secure {
                "250-STARTTLS\r\n"
            } else {
                ""
            };
            format!("250-fake.example greets you\r\n{tls}250-AUTH LOGIN PLAIN\r\n250 8BITMIME\r\n")
        } else if upper == "STARTTLS" {
            let _ = s.write_all(b"220 ready\r\n");
            return Next::Upgrade;
        } else if upper.starts_with("AUTH PLAIN") {
            let token = match cmd.strip_prefix("AUTH PLAIN ") {
                Some(t) => t.trim().to_string(),
                None => {
                    // no initial response: the empty challenge, then the token
                    let _ = s.write_all(b"334 \r\n");
                    match read_line(s) {
                        Some(t) => t.trim().to_string(),
                        None => return Next::Done,
                    }
                }
            };
            mail.auth = Some(decode(&token));
            if script.auth_echo {
                format!("535 bad credentials {:?} in {token}\r\n", decode(&token))
            } else {
                "235 ok\r\n".into()
            }
        } else if upper.starts_with("MAIL FROM:") {
            mail.from = cmd["MAIL FROM:".len()..]
                .trim_matches(['<', '>'])
                .to_string();
            "250 ok\r\n".into()
        } else if upper.starts_with("RCPT TO:") {
            let code = rcpt.lock().next().unwrap_or(250);
            if code == 250 || code == 251 {
                mail.to
                    .push(cmd["RCPT TO:".len()..].trim_matches(['<', '>']).to_string());
                format!("{code} ok\r\n")
            } else {
                format!("{code} not now\r\n")
            }
        } else if upper == "DATA" {
            let _ = s.write_all(b"354 go on\r\n");
            let mut data = String::new();
            while let Some(l) = read_line(s) {
                if l == ".\r\n" {
                    break;
                }
                data.push_str(&l);
            }
            mail.data = data;
            "250 queued\r\n".into()
        } else if upper == "QUIT" {
            let _ = s.write_all(b"221 bye\r\n");
            return Next::Done;
        } else {
            "500 what\r\n".into()
        };
        let _ = s.write_all(reply.as_bytes());
        let _ = s.flush();
    }
}

impl FakeSmtp {
    fn start(mode: SmtpMode, rcpt: Vec<u16>) -> FakeSmtp {
        FakeSmtp::start_with(
            mode,
            Script {
                rcpt,
                ..Default::default()
            },
        )
    }

    fn start_with(mode: SmtpMode, script: Script) -> FakeSmtp {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let got: Arc<Mutex<Vec<Mail>>> = Arc::default();
        let stop: Arc<std::sync::atomic::AtomicBool> = Arc::default();
        let (g, halt) = (got.clone(), stop.clone());
        let config = server_tls();
        let thread = std::thread::spawn(move || {
            let rcpt = Mutex::new(script.rcpt.clone().into_iter());
            for c in l.incoming() {
                if halt.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(tcp) = c else { continue };
                let _ = tcp.set_read_timeout(Some(Duration::from_secs(5)));
                let mut mail = Mail::default();
                let tls_stream = |tcp: std::net::TcpStream| {
                    rustls::StreamOwned::new(
                        rustls::ServerConnection::new(config.clone()).unwrap(),
                        tcp,
                    )
                };
                match mode {
                    SmtpMode::Tls => {
                        let mut s = tls_stream(tcp);
                        smtp_session(&mut s, mode, true, true, &script, &rcpt, &mut mail);
                    }
                    SmtpMode::Starttls | SmtpMode::Plain => {
                        let mut tcp = tcp;
                        if let Next::Upgrade =
                            smtp_session(&mut tcp, mode, false, true, &script, &rcpt, &mut mail)
                        {
                            let mut s = tls_stream(tcp);
                            smtp_session(&mut s, mode, true, false, &script, &rcpt, &mut mail);
                        }
                    }
                }
                g.lock().push(mail);
            }
        });
        FakeSmtp {
            port,
            got,
            stop,
            thread: Some(thread),
        }
    }

    fn got(&self) -> Vec<Mail> {
        self.got.lock().clone()
    }

    /// The sessions once `n` have ended, since a session ends after the client has its
    /// last reply.
    fn wait(&self, n: usize) -> Vec<Mail> {
        for _ in 0..200 {
            if self.got.lock().len() >= n {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.got()
    }
}

impl Drop for FakeSmtp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn email_channel(port: u16, tls: &str) -> Value {
    json!({
        "type": "email", "host": "127.0.0.1", "port": port, "tls": tls,
        "username": "sparkles", "password": {"secret": "smtp"},
        "from": "sparkles@example.org", "to": ["ops@example.org", "oncall@example.org"]
    })
}

/// §4.3: a message submitted over STARTTLS with AUTH PLAIN, whose password comes from
/// a secret and appears in no answer, and whose subject is the title.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_is_submitted_over_starttls() {
    let smtp = FakeSmtp::start(SmtpMode::Starttls, vec![]);
    let f = fixture_with(None, true, Some(TEST_CA));
    f.secret("smtp", "s3cret-smtp-pass");
    let (s, v) = f
        .patch(json!({
            "enabled": true,
            "baseUrl": "https://sparkles.example.org",
            "channels": {"mail": email_channel(smtp.port, "starttls")},
            "routes": {"backup.*": ["mail"]},
            "delivery": fast(),
        }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    raise(&f.st, event("backup.failed"), None, Utc::now());
    let v = settled(&f.st, "mail", 1).await;
    let c = channel_of(&v, "mail");
    assert_eq!(c["sent"], 1, "{v}");
    assert_eq!(c["lastSuccess"]["status"], 250);
    assert_eq!(c["type"], "email");
    assert_eq!(c["secrets"], json!(["smtp"]));
    let got = smtp.wait(1);
    assert_eq!(got.len(), 1);
    let m = &got[0];
    assert!(m.secure, "{m:?}");
    assert_eq!(m.auth.as_deref(), Some("\0sparkles\0s3cret-smtp-pass"));
    assert_eq!(m.from, "sparkles@example.org");
    assert_eq!(m.to, ["ops@example.org", "oncall@example.org"]);
    // STARTTLS comes before AUTH and MAIL, and EHLO is sent again over TLS
    let at = |p: &str| m.commands.iter().position(|c| c.starts_with(p)).unwrap();
    assert!(at("STARTTLS") < at("AUTH"), "{:?}", m.commands);
    assert_eq!(
        m.commands.iter().filter(|c| c.starts_with("EHLO")).count(),
        2
    );
    assert!(m.data.contains("Subject: Title\r\n"), "{}", m.data);
    assert!(
        m.data
            .contains("To: ops@example.org,\r\n oncall@example.org\r\n")
    );
    assert!(
        m.data
            .contains("Content-Type: text/plain; charset=utf-8\r\n")
    );
    assert!(
        m.data
            .contains("\r\n\r\nSummary\r\n\r\nhttps://sparkles.example.org/ui/server\r\n"),
        "{}",
        m.data
    );
    assert!(m.data.contains("Event: backup.failed\r\n"));
    assert!(m.data.contains("\"x\": 1"), "{}", m.data);
    let (_, v) = f.send("GET", "/$/notifications", None).await;
    assert!(!v.to_string().contains("s3cret-smtp-pass"), "{v}");
    let (_, v) = f
        .send("GET", "/$/server/settings/notifications", None)
        .await;
    assert!(!v.to_string().contains("s3cret-smtp-pass"), "{v}");
}

/// Implicit TLS on its own port, through a test send, and a 451 that is retried.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_over_implicit_tls_retries_a_4xx() {
    let smtp = FakeSmtp::start(SmtpMode::Tls, vec![451]);
    let f = fixture_with(None, true, Some(TEST_CA));
    f.secret("smtp", "s3cret-smtp-pass");
    let (s, v) = f
        .patch(json!({
            "enabled": true,
            "channels": {"mail": email_channel(smtp.port, "tls")},
            "routes": {"*": ["mail"]},
            "delivery": fast(),
        }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // the test send has one attempt, so the 451 fails it
    let (s, v) = f.send("POST", "/$/notifications/test/mail", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert!(v.to_string().contains("SMTP 451"), "{v}");
    let (s, v) = f.send("POST", "/$/notifications/test/mail", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["result"], "ok");
    assert_eq!(v["status"], 250);
    assert!(smtp.wait(2)[1].secure);
    assert!(
        smtp.got()[1]
            .data
            .contains("Subject: Test notification\r\n")
    );
    // a delivery retries a 451 and succeeds on the second attempt
    let smtp2 = FakeSmtp::start(SmtpMode::Tls, vec![451]);
    let (s, v) = f
        .patch(json!({"channels": {"mail": email_channel(smtp2.port, "tls")}}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    raise(&f.st, event("backup.failed"), None, Utc::now());
    let v = settled(&f.st, "mail", 3).await;
    assert_eq!(
        channel_of(&v, "mail")["lastSuccess"]["event"],
        "backup.failed",
        "{v}"
    );
    assert_eq!(smtp2.wait(2).len(), 2);
    assert!(
        f.metrics()
            .await
            .contains(r#"sparkles_notifications_retries_total{channel="mail"} 1"#)
    );
}

/// A 550 ends the delivery after one attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_does_not_retry_a_5xx() {
    let smtp = FakeSmtp::start(SmtpMode::Starttls, vec![550]);
    let f = fixture_with(None, true, Some(TEST_CA));
    f.secret("smtp", "s3cret-smtp-pass");
    f.patch(json!({
        "enabled": true,
        "channels": {"mail": email_channel(smtp.port, "starttls")},
        "routes": {"*": ["mail"]},
        "delivery": fast(),
    }))
    .await;
    raise(&f.st, event("backup.failed"), None, Utc::now());
    let v = settled(&f.st, "mail", 1).await;
    let c = channel_of(&v, "mail");
    assert_eq!(c["failed"], 1);
    assert!(
        c["lastFailure"]["error"]
            .as_str()
            .unwrap()
            .starts_with("SMTP 550"),
        "{v}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(smtp.wait(1).len(), 1);
    assert!(
        !f.metrics()
            .await
            .contains(r#"sparkles_notifications_retries_total{channel="mail"}"#)
    );
}

/// A server without STARTTLS gets neither the password nor the message, and one whose
/// certificate the server does not trust gets nothing past the handshake.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_never_goes_out_in_the_clear() {
    let plain = FakeSmtp::start(SmtpMode::Plain, vec![]);
    let f = fixture_with(None, true, Some(TEST_CA));
    f.secret("smtp", "s3cret-smtp-pass");
    f.patch(json!({"channels": {"mail": email_channel(plain.port, "starttls")}}))
        .await;
    let (s, v) = f.send("POST", "/$/notifications/test/mail", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert!(v.to_string().contains("does not offer STARTTLS"), "{v}");
    let got = plain.wait(1);
    assert_eq!(got.len(), 1);
    assert!(got[0].auth.is_none());
    assert!(
        !got[0].commands.iter().any(|c| c.starts_with("MAIL")),
        "{:?}",
        got[0].commands
    );
    // without the test CA the certificate is not trusted
    let tls = FakeSmtp::start(SmtpMode::Starttls, vec![]);
    let f = fixture(None, true);
    f.secret("smtp", "s3cret-smtp-pass");
    f.patch(json!({"channels": {"mail": email_channel(tls.port, "starttls")}}))
        .await;
    let (s, v) = f.send("POST", "/$/notifications/test/mail", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert!(v.to_string().contains("TLS"), "{v}");
    assert!(
        tls.wait(1)
            .iter()
            .all(|m| m.auth.is_none() && m.data.is_empty())
    );
}

/// The outbound policy checks the mail host before any connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_outbound_policy_refuses_a_private_mail_server() {
    let smtp = FakeSmtp::start(SmtpMode::Starttls, vec![]);
    let f = fixture_with(None, false, Some(TEST_CA));
    f.secret("smtp", "s3cret-smtp-pass");
    f.patch(json!({"channels": {"mail": email_channel(smtp.port, "starttls")}}))
        .await;
    let (s, v) = f.send("POST", "/$/notifications/test/mail", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["code"], "outbound-refused");
    assert!(smtp.got().is_empty());
}

/// The email channel's settings: no plain mode, secret passwords and checked addresses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_email_channel_settings_are_checked() {
    let f = fixture(None, true);
    let base = email_channel(587, "starttls");
    let with = |k: &str, v: Value| {
        let mut c = base.clone();
        c[k] = v;
        json!({"channels": {"mail": c}})
    };
    for (body, what) in [
        (with("tls", json!("none")), "tls"),
        (with("password", json!("inline-pass-value")), "password"),
        (with("from", json!("not an address")), "from"),
        (with("to", json!([])), "to"),
        (with("to", json!(["a@example.org>, evil@x.org"])), "to"),
        (with("host", json!("bad host")), "host"),
    ] {
        let (s, v) = f.patch(body).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{what}: {v}");
        assert!(v.to_string().contains(what), "{what}: {v}");
        assert!(!v.to_string().contains("inline-pass-value"), "{v}");
    }
    let mut c = base.clone();
    c.as_object_mut().unwrap().remove("password");
    let (s, v) = f.patch(json!({"channels": {"mail": c}})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = f.patch(json!({"channels": {"mail": base}})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = f.send("GET", "/$/notifications", None).await;
    assert_eq!(
        channel_of(&v, "mail")["target"],
        "127.0.0.1 port 587 (starttls) to ops@example.org, oncall@example.org"
    );
}

/// The message: an encoded subject outside ASCII, quoted-printable text, and a body line
/// that starts with a dot.
#[test]
fn the_email_message_is_rfc_5322() {
    let e: config::Email = serde_json::from_value(json!({
        "host": "smtp.example.org", "from": "s@example.org", "to": ["o@example.org"]
    }))
    .unwrap();
    assert_eq!(e.port(), 587);
    let env = json!({
        "id": "abc", "type": "backup.failed", "time": "2026-10-10T09:00:00Z",
        "server": "s", "severity": "critical", "title": "Sauvegarde échouée",
        "summary": ".leading dot and caf\u{e9}", "data": {}
    });
    let m = String::from_utf8(smtp::message(&e, &env)).unwrap();
    assert!(m.contains("Subject: =?utf-8?B?"), "{m}");
    assert!(m.contains("Content-Transfer-Encoding: quoted-printable\r\n"));
    assert!(m.contains("\r\n..leading dot and caf=C3=A9\r\n"), "{m}");
    assert!(
        m.contains("Date: Sat, 10 Oct 2026 09:00:00 +0000\r\n"),
        "{m}"
    );
    assert!(m.contains("Message-ID: <abc@example.org>\r\n"));
    assert!(m.ends_with("\r\n.\r\n"));
    assert!(!m.contains("Data:"));
    assert_eq!(smtp::subject("plain"), "plain");
    assert_eq!(smtp::quoted_printable("a=b \n"), "a=3Db=20\r\n");
    let long = "x".repeat(200);
    assert!(smtp::quoted_printable(&long).lines().all(|l| l.len() <= 76));
}

/// A server that quotes the credentials in its AUTH reply gets them into no answer,
/// however short the password.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_email_login_quotes_nothing_secret() {
    use base64::Engine as _;
    let smtp = FakeSmtp::start_with(
        SmtpMode::Tls,
        Script {
            auth_echo: true,
            ..Default::default()
        },
    );
    let f = fixture_with(None, true, Some(TEST_CA));
    f.secret("smtp", "pw");
    f.patch(json!({"channels": {"mail": email_channel(smtp.port, "tls")}}))
        .await;
    let (s, v) = f.send("POST", "/$/notifications/test/mail", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    let token = base64::engine::general_purpose::STANDARD.encode("\0sparkles\0pw");
    let text = v.to_string();
    assert!(text.contains("SMTP 535: authentication failed"), "{text}");
    assert!(!text.contains(&token), "{text}");
    let (_, v) = f.send("GET", "/$/notifications", None).await;
    assert!(!v.to_string().contains(&token), "{v}");
    assert!(!v.to_string().contains("\\u0000pw"), "{v}");
    assert_eq!(
        smtp::scrub("a pw and a token".into(), &["pw", "token"]),
        "a [redacted] and a [redacted]"
    );
}

/// One deadline bounds the attempt: a server that sends its greeting a byte at a time
/// cannot hold the channel past `timeoutSecs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trickling_mail_server_times_out() {
    let smtp = FakeSmtp::start_with(
        SmtpMode::Starttls,
        Script {
            trickle: true,
            ..Default::default()
        },
    );
    let f = fixture_with(None, true, Some(TEST_CA));
    f.secret("smtp", "s3cret-smtp-pass");
    f.patch(json!({
        "channels": {"mail": email_channel(smtp.port, "starttls")},
        "delivery": {"attempts": 1, "backoffSecs": 1, "maxBackoffSecs": 1, "timeoutSecs": 1},
    }))
    .await;
    let t = Instant::now();
    let (s, v) = f.send("POST", "/$/notifications/test/mail", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert!(v.to_string().contains("in time"), "{v}");
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

/// A recipient that the server forwards (251) is accepted, and credentials too long for
/// an initial response go after the server's challenge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_accepts_forwarding_and_long_credentials() {
    let smtp = FakeSmtp::start_with(
        SmtpMode::Tls,
        Script {
            rcpt: vec![251],
            max_command: Some(512),
            ..Default::default()
        },
    );
    let f = fixture_with(None, true, Some(TEST_CA));
    let password = "p".repeat(140);
    f.secret("smtp", &password);
    let mut c = email_channel(smtp.port, "tls");
    let user = "u".repeat(240);
    c["username"] = user.clone().into();
    let (s, v) = f.patch(json!({"channels": {"mail": c}})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = f.send("POST", "/$/notifications/test/mail", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let m = &smtp.wait(1)[0];
    assert_eq!(
        m.auth.as_deref(),
        Some(format!("\0{user}\0{password}").as_str())
    );
    assert_eq!(m.to, ["ops@example.org", "oncall@example.org"]);
}

/// No line of a message passes 998 characters, and no line of an encoded subject 76.
#[test]
fn email_header_lines_stay_within_the_limits() {
    let to: Vec<String> = (0..20)
        .map(|i| format!("{}{i:02}@example.org", "r".repeat(48)))
        .collect();
    let e: config::Email = serde_json::from_value(json!({
        "host": "smtp.example.org", "from": "s@example.org", "to": to
    }))
    .unwrap();
    let env = json!({
        "id": "abc", "type": "backup.failed", "time": "2026-10-10T09:00:00Z",
        "server": "s", "severity": "critical", "title": "é".repeat(120),
        "summary": "s", "data": {}
    });
    let m = String::from_utf8(smtp::message(&e, &env)).unwrap();
    assert!(m.split("\r\n").all(|l| l.len() <= 998), "{m}");
    let header = &m[..m.find("\r\n\r\n").unwrap()];
    let mut in_subject = false;
    for l in header.split("\r\n") {
        if l.starts_with("Subject:") {
            in_subject = true;
        } else if !l.starts_with(' ') {
            in_subject = false;
        }
        if in_subject {
            assert!(l.len() <= 76, "{l:?}");
        }
    }
    assert!(header.contains("To: rrr"), "{header}");
    assert_eq!(header.matches("@example.org,\r\n ").count(), 19, "{header}");
}

/// A mail server on loopback that writes `greeting` to each of `n` connections and
/// closes it, and then ends; the connections it took.
fn one_line_server(
    greeting: &'static [u8],
    n: usize,
) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let taken: Arc<std::sync::atomic::AtomicUsize> = Arc::default();
    let counter = taken.clone();
    std::thread::spawn(move || {
        for c in l.incoming().take(n) {
            let Ok(mut c) = c else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = c.write_all(greeting);
        }
    });
    (port, taken)
}

/// A connection that ends within a line is a transient failure, which is retried, and
/// a line without its CRLF is not SMTP, which is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_reply_framing() {
    let f = fixture_with(None, true, Some(TEST_CA));
    f.secret("smtp", "s3cret-smtp-pass");
    let (cut, cuts) = one_line_server(b"220 ready", 2);
    let (bare, bares) = one_line_server(b"220 ready\n", 1);
    f.patch(json!({
        "enabled": true,
        "channels": {
            "cut": email_channel(cut, "starttls"),
            "bare": email_channel(bare, "starttls"),
        },
        "routes": {"*": ["bare", "cut"]},
        "delivery": {"attempts": 2, "backoffSecs": 0.05, "maxBackoffSecs": 1, "timeoutSecs": 5},
    }))
    .await;
    raise(&f.st, event("backup.failed"), None, Utc::now());
    let v = settled(&f.st, "cut", 1).await;
    let v = if channel_of(&v, "bare")["failed"] == 0 {
        settled(&f.st, "bare", 1).await
    } else {
        v
    };
    assert_eq!(cuts.load(Ordering::SeqCst), 2, "{v}");
    assert_eq!(bares.load(Ordering::SeqCst), 1, "{v}");
    assert!(
        channel_of(&v, "bare")["lastFailure"]["error"]
            .as_str()
            .unwrap()
            .contains("not SMTP"),
        "{v}"
    );
}

/// A password inside the AUTH token does not leave part of the token readable.
#[test]
fn overlapping_secrets_are_scrubbed_whole() {
    use base64::Engine as _;
    let token = base64::engine::general_purpose::STANDARD.encode("\0aasparkles\0c3Bhcmts");
    assert_eq!(token, "AGFhc3BhcmtsZXMAYzNCaGNtdHM=");
    let out = smtp::scrub(
        format!("554 rejected {token} for c3Bhcmts"),
        &["c3Bhcmts", &token],
    );
    assert_eq!(out, "554 rejected [redacted] for [redacted]");
}

/// Verifications that fail and succeed at once from several threads always leave the
/// repository's condition exactly when one of its problems is left, since each one
/// changes the problems and the condition in one step.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_verifications_keep_the_repository_condition_consistent() {
    let f = fixture(None, false);
    let (s, v) = f
        .patch(json!({
            "enabled": true,
            "repeatEvery": "1m",
            "channels": {"hook": {"type": "webhook", "url": "http://127.0.0.1:9/x"}},
            "routes": {"*": ["hook"]},
        }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let key = "backup.verify.failed/r";
    let consistent = |st: &AppState| {
        let keys = active_keys(st);
        let problems = keys.iter().any(|k| k.starts_with(&format!("{key}/")));
        keys.iter().any(|k| k == key) == problems
    };
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let st = f.st.clone();
            std::thread::spawn(move || {
                let names = vec!["b1".to_string()];
                for i in 0..200 {
                    let fail = (i + t) % 2 == 0;
                    let broken: &[String] = if fail { &names } else { &[] };
                    events::backup_verify(
                        &st,
                        "r",
                        false,
                        "data",
                        &names,
                        &names,
                        broken,
                        None,
                        Utc::now(),
                    );
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert!(consistent(&f.st), "{:?}", active_keys(&f.st));
    // a last success leaves nothing
    let names = vec!["b1".to_string()];
    events::backup_verify(
        &f.st,
        "r",
        false,
        "data",
        &names,
        &names,
        &[],
        None,
        Utc::now(),
    );
    assert!(active_keys(&f.st).iter().all(|k| !k.starts_with(key)));
}
