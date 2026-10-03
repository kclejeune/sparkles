//! Retries, deadlines, cancellation and token refresh against a mock server that answers
//! as scripted and counts the attempts it receives (spec P02 §4.8, A11).

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use sparkles_client::{
    CancellationToken, Client, ClientBuilder, Error, QueryOptions, RetryPolicy, TokenSource,
    UpdateOptions,
};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
struct Mock {
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl Mock {
    fn hits(&self, k: &str) -> usize {
        self.hits.lock().unwrap().get(k).copied().unwrap_or(0)
    }
}

const ASK_TRUE: &str = r#"{"head":{},"boolean":true}"#;

fn reply(status: u16, headers: &[(&str, &str)], body: impl Into<Body>) -> Response {
    let mut b = Response::builder().status(status);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(body.into()).unwrap()
}

fn ok_json(body: &'static str) -> Response {
    reply(
        200,
        &[("content-type", "application/sparql-results+json")],
        body,
    )
}

/// `/{case}`: the n-th attempt (from 1) of each case answers as its script says.
async fn handle(State(m): State<Mock>, Path(case): Path<String>, headers: HeaderMap) -> Response {
    let n = {
        let mut h = m.hits.lock().unwrap();
        let e = h.entry(case.clone()).or_insert(0);
        *e += 1;
        *e
    };
    match case.as_str() {
        // twice "too many requests", then the answer
        "rate" if n <= 2 => reply(429, &[("retry-after", "1")], "{\"error\":\"slow down\"}"),
        "rate" => ok_json(ASK_TRUE),
        // an exhausted RateLimit field without Retry-After
        "ratelimit" if n == 1 => reply(429, &[("ratelimit", "\"query\";r=0;t=1")], ""),
        "ratelimit" => ok_json(ASK_TRUE),
        "bad-gateway" => reply(502, &[], "upstream down"),
        "busy" => reply(503, &[], "{\"error\":\"wal failed\"}"),
        "busy-retry" if n == 1 => reply(503, &[("retry-after", "0")], ""),
        "busy-retry" => reply(
            200,
            &[
                ("content-type", "application/json"),
                ("sparkles-commit", "7"),
            ],
            "{\"inserted\":1}",
        ),
        "always-busy" => reply(503, &[("retry-after", "0")], ""),
        "long-wait" => reply(429, &[("retry-after", "3600")], ""),
        "two-seconds" => reply(429, &[("retry-after", "2")], ""),
        "auth" => {
            let a = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if a == "Bearer fresh" {
                ok_json(ASK_TRUE)
            } else {
                reply(
                    401,
                    &[("www-authenticate", "Bearer")],
                    "{\"error\":\"expired\"}",
                )
            }
        }
        // one solution, then a body that never ends
        "stalls" => {
            let first = bytes::Bytes::from_static(
                br#"{"head":{"vars":["x"]},"results":{"bindings":[{"x":{"type":"literal","value":"a"}},"#,
            );
            let s = futures_util::stream::once(async move { Ok::<_, std::io::Error>(first) })
                .chain(futures_util::stream::pending());
            reply(
                200,
                &[("content-type", "application/sparql-results+json")],
                Body::from_stream(s),
            )
        }
        _ => reply(404, &[], ""),
    }
}

use futures_util::StreamExt;

/// The mock on a free port in 5550–5559.
async fn start() -> (String, Mock) {
    let mock = Mock::default();
    let app = Router::new()
        .route("/{case}", axum::routing::any(handle))
        .with_state(mock.clone());
    for port in 5550..5560 {
        if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
            return (format!("http://127.0.0.1:{port}"), mock);
        }
    }
    panic!("no free port in 5550-5559");
}

fn fast() -> RetryPolicy {
    RetryPolicy {
        initial_backoff: Duration::from_millis(10),
        ..RetryPolicy::default()
    }
}

struct Rotating;
impl TokenSource for Rotating {
    fn token(
        &self,
        refresh: bool,
    ) -> Pin<Box<dyn Future<Output = sparkles_client::Result<String>> + Send + '_>> {
        Box::pin(async move { Ok(if refresh { "fresh" } else { "stale" }.to_string()) })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_waits_and_cancellation() {
    let (base, mock) = start().await;
    let client = ClientBuilder::new().retry(fast()).build().unwrap();
    let ep = |case: &str| {
        client
            .endpoint(format!("{base}/{case}"))
            .unwrap()
            .with_update_url(format!("{base}/{case}"))
            .unwrap()
    };

    // A11: 429 with Retry-After: 1, twice, then the answer
    let t0 = Instant::now();
    assert!(ep("rate").ask("ASK {}").await.unwrap());
    assert_eq!(mock.hits("rate"), 3);
    assert!(
        t0.elapsed() >= Duration::from_millis(1900),
        "{:?}",
        t0.elapsed()
    );

    // the reset time of an exhausted RateLimit field
    let t0 = Instant::now();
    assert!(ep("ratelimit").ask("ASK {}").await.unwrap());
    assert_eq!(mock.hits("ratelimit"), 2);
    assert!(
        t0.elapsed() >= Duration::from_millis(900),
        "{:?}",
        t0.elapsed()
    );

    // a write is never retried after a 502, whose outcome is unknown; a query is
    let e = ep("bad-gateway")
        .update("INSERT DATA {}")
        .await
        .unwrap_err();
    assert_eq!(e.status(), Some(502));
    assert_eq!(mock.hits("bad-gateway"), 1);
    let e = ep("bad-gateway").ask("ASK {}").await.unwrap_err();
    assert_eq!(e.status(), Some(502));
    assert_eq!(mock.hits("bad-gateway"), 1 + 4);

    // 503 without Retry-After: the write may have run, so it is not retried
    let e = ep("busy").update("INSERT DATA {}").await.unwrap_err();
    assert_eq!(e.status(), Some(503));
    assert!(e.to_string().contains("wal failed"), "{e}");
    assert_eq!(mock.hits("busy"), 1);

    // 503 with Retry-After: refused before any work, so a write is retried
    let r = ep("busy-retry").update("INSERT DATA {}").await.unwrap();
    assert_eq!(r.commit_seq, Some(7));
    assert_eq!(r.body["inserted"], 1);
    assert_eq!(mock.hits("busy-retry"), 2);

    // a Retry-After beyond max_retry_after ends the retries at once
    let t0 = Instant::now();
    let e = ep("long-wait").ask("ASK {}").await.unwrap_err();
    assert_eq!(e.status(), Some(429));
    let Error::Status(s) = &e else { unreachable!() };
    assert_eq!(s.retry_after, Some(Duration::from_secs(3600)));
    assert_eq!(mock.hits("long-wait"), 1);
    assert!(t0.elapsed() < Duration::from_secs(5));

    // a wait that would pass the deadline is not taken
    let e = ep("two-seconds")
        .query_with(
            "ASK {}",
            &QueryOptions::new().deadline(Duration::from_millis(500)),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Deadline(_)), "{e}");
    assert_eq!(mock.hits("two-seconds"), 1);

    // no_retry, and the policy's retry count
    let e = ep("always-busy")
        .update_with("INSERT DATA {}", &UpdateOptions::new().no_retry())
        .await
        .unwrap_err();
    assert_eq!(e.status(), Some(503));
    assert_eq!(mock.hits("always-busy"), 1);
    ep("always-busy")
        .update("INSERT DATA {}")
        .await
        .unwrap_err();
    assert_eq!(mock.hits("always-busy"), 1 + 4);

    // a token source is asked again after a 401
    let authed = ClientBuilder::new()
        .token_source(Rotating)
        .build()
        .unwrap()
        .endpoint(format!("{base}/auth"))
        .unwrap();
    assert!(authed.ask("ASK {}").await.unwrap());
    assert_eq!(mock.hits("auth"), 2);
    let e = ClientBuilder::new()
        .bearer_token("wrong")
        .build()
        .unwrap()
        .endpoint(format!("{base}/auth"))
        .unwrap()
        .ask("ASK {}")
        .await
        .unwrap_err();
    assert_eq!(e.status(), Some(401));
    assert_eq!(
        e.to_string(),
        format!("401 {base}/auth?query=ASK+%7B%7D: expired")
    );

    // cancelling the token ends a stream that is waiting for data
    let token = CancellationToken::new();
    let mut sols = ep("stalls")
        .query_with("SELECT * {}", &QueryOptions::new().cancel(token.clone()))
        .await
        .unwrap()
        .into_solutions()
        .unwrap();
    assert!(sols.next().await.unwrap().is_ok());
    let t = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        t.cancel();
    });
    let e = sols.next().await.unwrap().unwrap_err();
    assert!(matches!(e, Error::Cancelled), "{e}");

    // and a deadline ends one too
    let mut sols = ep("stalls")
        .query_with(
            "SELECT * {}",
            &QueryOptions::new().deadline(Duration::from_millis(300)),
        )
        .await
        .unwrap()
        .into_solutions()
        .unwrap();
    assert!(sols.next().await.unwrap().is_ok());
    let e = sols.next().await.unwrap().unwrap_err();
    assert!(matches!(e, Error::Deadline(_)), "{e}");

    // a client without a server URL reports it
    let e = ClientBuilder::new()
        .build()
        .unwrap()
        .dataset("ds")
        .ask("ASK {}")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no server URL"), "{e}");
    let _ = Client::new(&base).unwrap();
}
