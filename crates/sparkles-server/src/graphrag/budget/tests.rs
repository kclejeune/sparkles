//! Admission of every HTTP attempt against local mock providers (C22 §3.5, A8, A11,
//! A12). No request leaves the loopback interface.

use super::*;
use crate::models::mock::{self, MockModel, Received};
use crate::models::{Models, ModelsConfig, OutputSchema, Pair, Role, StepError, fenced};
use serde_json::json;
use std::time::{Duration, Instant};

const MAX_OUT: u32 = 100;

fn models(providers: Value) -> Models {
    let cfg = ModelsConfig::parse(&json!({ "providers": providers }).to_string()).unwrap();
    let policy = sparkles::outbound::OutboundPolicy {
        allow_private: true,
        ..Default::default()
    };
    Models::new(cfg, Default::default(), policy)
}

fn schema() -> OutputSchema {
    OutputSchema {
        name: "Draft",
        schema: json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"],
            "additionalProperties": false
        }),
        from_text: |t| fenced(t, &["sparql"]).map(|q| json!({ "query": q })),
        text_instruction: "Write the query in one fenced sparql block.",
    }
}

fn pair(provider: &str) -> Pair {
    let mut p = Pair::new(provider, "m1");
    p.max_output_tokens = Some(MAX_OUT);
    p
}

fn ledger(build: u64, day: u64) -> Arc<Ledger> {
    Arc::new(Ledger::new(
        build,
        day,
        Arc::new(Mutex::new(DayLedger::default())),
        || 20_000,
    ))
}

fn call(
    m: &Models,
    provider: &str,
    a: &BuildAdmission,
) -> Result<crate::models::Answer, crate::models::Failure> {
    m.call_admitted(
        Some(Role::Community),
        &pair(provider),
        "system text",
        "user text",
        &schema(),
        Instant::now() + Duration::from_secs(10),
        Some(a),
    )
}

/// A tokenizer that counts characters of the user messages, standing in for an exact
/// tokenizer of a model.
struct CharCounter;

impl LocalTokenizer for CharCounter {
    fn identity(&self) -> String {
        "test-chars-1".into()
    }
    fn count(&self, _: Kind, body: &Value) -> Option<u64> {
        Some(
            body["messages"]
                .as_array()?
                .iter()
                .map(|m| m["content"].as_str().unwrap_or("").chars().count() as u64)
                .sum(),
        )
    }
}

fn ok(kind: Kind) -> impl Fn(&Received, usize) -> (u16, Value) + Send + Sync + 'static {
    move |_, _| (200, mock::answer(kind, r#"{"query":"ASK {}"}"#))
}

#[test]
fn an_exact_local_tokenizer_comes_first() {
    let mock = MockModel::start(ok(Kind::Anthropic));
    let m = models(json!({ "p": { "kind": "anthropic", "endpoint": mock.url() } }));
    let mut t = Tokenizers::default();
    t.insert("p", "m1", Arc::new(CharCounter));
    let l = ledger(1_000_000, 1_000_000);
    let a = BuildAdmission::new(l.clone(), t, true);
    call(&m, "p", &a).unwrap();
    let b = a.bounds();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].source, Source::ExactLocal);
    assert_eq!(b[0].counter.as_deref(), Some("test-chars-1"));
    assert!(b[0].source.exact());
    // the user text, counted by the stand-in
    assert!(b[0].tokens >= "user text".len() as u64);
    // no counting request: only the message call
    assert_eq!(mock.requests().len(), 1);
    // settled to the mock's reported 10 + 5
    assert_eq!(l.build_used(), 15);
}

#[test]
fn the_provider_endpoint_counts_when_allowed() {
    let mock = MockModel::start(|r: &Received, _| {
        if r.path.ends_with("/count_tokens") {
            (200, json!({ "input_tokens": 42 }))
        } else {
            (200, mock::anthropic(r#"{"query":"ASK {}"}"#))
        }
    });
    let m = models(json!({ "p": { "kind": "anthropic", "endpoint": mock.url() } }));
    let l = ledger(1_000_000, 1_000_000);
    let a = BuildAdmission::new(l.clone(), Tokenizers::default(), true);
    call(&m, "p", &a).unwrap();
    let b = a.bounds();
    assert_eq!(b[0].source, Source::ProviderEndpoint);
    assert_eq!(b[0].tokens, 42);
    let reqs = mock.requests();
    assert_eq!(reqs[0].path, "/v1/messages/count_tokens");
    // the count carries the request without its output limit, to the same provider
    assert!(reqs[0].body.get("max_tokens").is_none());
    assert_eq!(reqs[0].body["messages"], reqs[1].body["messages"]);
    assert_eq!(reqs[1].path, "/v1/messages");

    // turned off: no counting request, the byte bound instead
    let mock2 = MockModel::start(ok(Kind::Anthropic));
    let m2 = models(json!({ "p": { "kind": "anthropic", "endpoint": mock2.url() } }));
    let a2 = BuildAdmission::new(ledger(1_000_000, 1_000_000), Tokenizers::default(), false);
    call(&m2, "p", &a2).unwrap();
    assert_eq!(a2.bounds()[0].source, Source::ByteBound);
    assert!(
        mock2
            .requests()
            .iter()
            .all(|r| !r.path.ends_with("/count_tokens"))
    );
}

#[test]
fn a_failed_count_falls_back_to_the_byte_bound() {
    let mock = MockModel::start(|r: &Received, _| {
        if r.path.ends_with("/count_tokens") {
            (404, json!({ "error": { "message": "no such route" } }))
        } else {
            (200, mock::anthropic(r#"{"query":"ASK {}"}"#))
        }
    });
    let m = models(json!({ "p": { "kind": "anthropic", "endpoint": mock.url() } }));
    let a = BuildAdmission::new(ledger(1_000_000, 1_000_000), Tokenizers::default(), true);
    call(&m, "p", &a).unwrap();
    assert_eq!(a.bounds()[0].source, Source::ByteBound);
}

#[test]
fn the_byte_bound_covers_the_serialized_request() {
    let mock = MockModel::start(ok(Kind::Openai));
    let m = models(json!({ "p": { "kind": "openai", "endpoint": mock.url() } }));
    let a = BuildAdmission::new(ledger(1_000_000, 1_000_000), Tokenizers::default(), true);
    call(&m, "p", &a).unwrap();
    let b = &a.bounds()[0];
    assert_eq!(b.source, Source::ByteBound);
    assert!(!b.source.exact());
    let sent = serde_json::to_vec(&mock.requests()[0].body).unwrap();
    // one user message plus the system text
    assert_eq!(
        b.tokens,
        sent.len() as u64 + 2 * FRAMING_PER_MESSAGE + ALLOWANCE_PER_REQUEST
    );
}

/// A `503` then a `200`: two reservations, the first kept whole (A12).
#[test]
fn transport_retries_are_admitted_and_unknown_usage_stays_reserved() {
    let mock = MockModel::start(|_, n| {
        if n == 0 {
            (503, json!({ "error": { "message": "busy" } }))
        } else {
            (200, mock::openai(r#"{"query":"ASK {}"}"#))
        }
    });
    let m = models(json!({ "p": { "kind": "openai", "endpoint": mock.url() } }));
    let l = ledger(1_000_000, 1_000_000);
    let a = BuildAdmission::new(l.clone(), Tokenizers::default(), true);
    call(&m, "p", &a).unwrap();
    let b = a.bounds();
    assert_eq!(b.len(), 2);
    assert_eq!(mock.requests().len(), 2);
    let first = b[0].tokens + u64::from(MAX_OUT);
    assert_eq!(l.build_used(), first + 15);
}

#[test]
fn schema_retries_and_level_fallback_are_admitted() {
    // an invalid answer, then a valid one: the correction retry is a second attempt
    let mock = MockModel::start(|_, n| {
        let text = if n == 0 {
            r#"{"nope":1}"#
        } else {
            r#"{"query":"ASK {}"}"#
        };
        (200, mock::openai(text))
    });
    let m = models(json!({ "p": { "kind": "openai", "endpoint": mock.url() } }));
    let a = BuildAdmission::new(ledger(1_000_000, 1_000_000), Tokenizers::default(), true);
    call(&m, "p", &a).unwrap();
    assert_eq!(a.bounds().len(), 2);
    assert_ne!(a.bounds()[0].request_digest, a.bounds()[1].request_digest);

    // a refused level moves to the next, which is admitted again
    let mock = MockModel::start(|r: &Received, _| {
        if r.body["response_format"]["type"] == "json_schema" {
            (400, json!({ "error": { "message": "unsupported" } }))
        } else {
            (200, mock::openai(r#"{"query":"ASK {}"}"#))
        }
    });
    let m = models(json!({ "p": { "kind": "openai", "endpoint": mock.url() } }));
    let a = BuildAdmission::new(ledger(1_000_000, 1_000_000), Tokenizers::default(), true);
    call(&m, "p", &a).unwrap();
    assert_eq!(a.bounds().len(), 2);
    assert_eq!(mock.requests().len(), 2);
}

/// A reservation that does not fit sends nothing (A8).
#[test]
fn an_exhausted_budget_sends_no_request() {
    let mock = MockModel::start(ok(Kind::Openai));
    let m = models(json!({ "p": { "kind": "openai", "endpoint": mock.url() } }));
    let a = BuildAdmission::new(ledger(50, 1_000_000), Tokenizers::default(), true);
    let f = call(&m, "p", &a).unwrap_err();
    assert!(matches!(f.error, StepError::Budget(_)), "{:?}", f.error);
    assert_eq!(f.record.requests, 0);
    assert!(mock.requests().is_empty());

    // the day's budget is checked as well
    let a = BuildAdmission::new(ledger(1_000_000, 50), Tokenizers::default(), true);
    let f = call(&m, "p", &a).unwrap_err();
    assert!(
        f.error.message().contains("tokensPerDay"),
        "{}",
        f.error.message()
    );
    assert!(mock.requests().is_empty());
}

#[test]
fn usage_above_the_reservation_stops_the_build() {
    let mock = MockModel::start(|_, _| {
        (
            200,
            json!({ "choices": [{ "message": { "content": r#"{"query":"ASK {}"}"# }, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 1_000_000, "completion_tokens": 5 } }),
        )
    });
    let m = models(json!({ "p": { "kind": "openai", "endpoint": mock.url() } }));
    let mut t = Tokenizers::default();
    t.insert("p", "m1", Arc::new(CharCounter));
    let l = ledger(10_000_000, 10_000_000);
    let a = BuildAdmission::new(l.clone(), t, true);
    call(&m, "p", &a).unwrap();
    let an = l.anomaly().expect("an anomaly");
    assert_eq!(an.reported, 1_000_005);
    assert_eq!(an.source, Source::ExactLocal);
    assert_eq!(
        l.build_used(),
        1_000_005,
        "the ledger counts the reported usage"
    );
    let f = call(&m, "p", &a).unwrap_err();
    assert_eq!(f.error.code(), "accounting-anomaly");
    assert_eq!(mock.requests().len(), 1, "no further request");
}

#[test]
fn an_answer_without_usage_keeps_its_reservation() {
    let mock = MockModel::start(|_, _| {
        (
            200,
            json!({ "choices": [{ "message": { "content": r#"{"query":"ASK {}"}"# }, "finish_reason": "stop" }] }),
        )
    });
    let m = models(json!({ "p": { "kind": "openai", "endpoint": mock.url() } }));
    let l = ledger(1_000_000, 1_000_000);
    let a = BuildAdmission::new(l.clone(), Tokenizers::default(), true);
    call(&m, "p", &a).unwrap();
    assert_eq!(l.build_used(), a.bounds()[0].tokens + u64::from(MAX_OUT));
}

#[test]
fn concurrent_reservations_never_pass_the_cap() {
    let day = Arc::new(Mutex::new(DayLedger::default()));
    // two builds share the day's ledger
    let a = Arc::new(Ledger::new(1_000, 1_500, day.clone(), || 7));
    let b = Arc::new(Ledger::new(1_000, 1_500, day.clone(), || 7));
    let granted = std::sync::atomic::AtomicU64::new(0);
    std::thread::scope(|s| {
        for i in 0..16 {
            let l = if i % 2 == 0 { a.clone() } else { b.clone() };
            let granted = &granted;
            s.spawn(move || {
                for _ in 0..50 {
                    if l.reserve(7).is_ok() {
                        granted.fetch_add(7, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            });
        }
    });
    let g = granted.load(std::sync::atomic::Ordering::SeqCst);
    assert!(g <= 1_500, "{g}");
    assert!(a.build_used() <= 1_000 && b.build_used() <= 1_000);
    assert_eq!(day.lock().used, g);
}

#[test]
fn a_new_utc_day_starts_an_empty_day_ledger() {
    let day = Arc::new(Mutex::new(DayLedger { day: 1, used: 90 }));
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(1));
    let c = clock.clone();
    let l = Ledger::new(1_000, 100, day.clone(), move || {
        c.load(std::sync::atomic::Ordering::SeqCst)
    });
    assert!(l.reserve(20).is_err());
    clock.store(2, std::sync::atomic::Ordering::SeqCst);
    assert!(l.reserve(20).is_ok());
    assert_eq!(*day.lock(), DayLedger { day: 2, used: 20 });
}

/// A tokenizer that counts every request as 100 tokens.
struct Fixed;

impl LocalTokenizer for Fixed {
    fn identity(&self) -> String {
        "test-fixed-1".into()
    }
    fn count(&self, _: Kind, _: &Value) -> Option<u64> {
        Some(100)
    }
}

/// A refusal that reports usage settles it, so an overrun is still an anomaly
/// (review finding 1).
#[test]
fn usage_of_a_refused_answer_is_settled() {
    let mock = MockModel::start(|_, _| {
        (
            200,
            json!({ "choices": [{ "message": { "content": null, "refusal": "no" }, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 1_000_000, "completion_tokens": 5 } }),
        )
    });
    let m = models(json!({ "p": { "kind": "openai", "endpoint": mock.url() } }));
    let mut t = Tokenizers::default();
    t.insert("p", "m1", Arc::new(Fixed));
    let l = ledger(10_000_000, 10_000_000);
    let a = BuildAdmission::new(l.clone(), t, true);
    let f = call(&m, "p", &a).unwrap_err();
    assert_eq!(f.error.code(), "refusal");
    assert_eq!(l.build_used(), 1_000_005);
    assert!(l.anomaly().is_some());
}

/// Counting spends the request's remaining time, and an attempt whose time ran out
/// while counting is not sent (review finding 2).
#[test]
fn counting_stays_within_the_deadline() {
    let mock = MockModel::start(|r: &Received, _| {
        if r.path.ends_with("/count_tokens") {
            std::thread::sleep(Duration::from_millis(600));
            (200, json!({ "input_tokens": 42 }))
        } else {
            (200, mock::anthropic(r#"{"query":"ASK {}"}"#))
        }
    });
    let m = models(json!({ "p": { "kind": "anthropic", "endpoint": mock.url() } }));
    let l = ledger(1_000_000, 1_000_000);
    let a = BuildAdmission::new(l.clone(), Tokenizers::default(), true);
    let started = Instant::now();
    let r = m.call_admitted(
        Some(Role::Community),
        &pair("p"),
        "system text",
        "user text",
        &schema(),
        Instant::now() + Duration::from_millis(300),
        Some(&a),
    );
    assert!(r.is_err(), "the deadline passed while counting");
    assert!(
        started.elapsed() < Duration::from_millis(550),
        "{:?}",
        started.elapsed()
    );
    assert!(
        mock.requests()
            .iter()
            .all(|r| r.path.ends_with("/count_tokens")),
        "no inference request after the deadline"
    );
    // nothing was sent, so nothing stays reserved
    assert_eq!(l.build_used(), 0);
}

/// A retry refused after a sent attempt keeps that attempt in the record (review
/// finding 5).
#[test]
fn a_refused_retry_keeps_the_sent_attempt_in_the_record() {
    let mock = MockModel::start(|_, _| (503, json!({ "error": { "message": "busy" } })));
    let m = models(json!({ "p": { "kind": "openai", "endpoint": mock.url() } }));
    let mut t = Tokenizers::default();
    t.insert("p", "m1", Arc::new(Fixed));
    // room for exactly one reservation of 100 + MAX_OUT
    let a = BuildAdmission::new(ledger(100 + u64::from(MAX_OUT), 1_000_000), t, true);
    let f = call(&m, "p", &a).unwrap_err();
    assert!(matches!(f.error, StepError::Budget(_)), "{:?}", f.error);
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(f.record.requests, 1);
}
