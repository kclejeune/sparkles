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
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        &dir.path().join("data"),
        sparkles::store::StoreOptions::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    st.outbound.allow_private = allow_private;
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
