//! Deterministic local-provider checks for item 318. The ignored run emits all
//! samples, including cold report caches and clarification follow-ups.

use super::*;
use crate::ask::profile::{self, TARGET};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Value>>>);

#[derive(Default)]
struct Fields(Map<String, Value>);

impl Visit for Fields {
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.0.insert(f.name().into(), format!("{v:?}").into());
    }
    fn record_str(&mut self, f: &Field, v: &str) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.0.insert(f.name().into(), v.into());
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: Context<'_, S>,
    ) {
        if attrs.metadata().target() == TARGET {
            let mut fields = Fields::default();
            attrs.record(&mut fields);
            ctx.span(id).unwrap().extensions_mut().insert(fields);
        }
    }
    fn on_record(&self, id: &tracing::Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(fields) = extensions.get_mut::<Fields>() {
                values.record(fields);
            }
        }
    }
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if event.metadata().target() != TARGET {
            return;
        }
        let mut fields = Fields::default();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(parent) = span.extensions().get::<Fields>() {
                    fields.0.extend(parent.0.clone());
                }
            }
        }
        event.record(&mut fields);
        self.0.lock().push(Value::Object(fields.0));
    }
}

pub(super) fn capture<T>(f: impl FnOnce() -> T) -> (T, Vec<Value>) {
    // Keep the multi-dispatcher path active while parallel tests first register
    // shared callsites, following the HTTP access-log capture helpers.
    let _second = tracing::Dispatch::new(tracing_subscriber::registry());
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let result = tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        f()
    });
    let records = capture.0.lock().clone();
    (result, records)
}

fn semantic(mut v: Value) -> Value {
    // Only wall-clock diagnostics change. Every answer, usage count, choice,
    // ground fact, repair and error remains part of the comparison.
    match &mut v {
        Value::Object(m) => {
            for k in ["elapsedMs", "modelMs", "latencyMs", "timeMs"] {
                m.remove(k);
            }
            for value in m.values_mut() {
                *value = semantic(value.take());
            }
        }
        Value::Array(a) => {
            for value in a {
                *value = semantic(value.take());
            }
        }
        _ => {}
    }
    v
}

fn observed(
    s: &McpServer,
    m: &Models,
    p: &Principal,
    o: &AskOptions,
    enabled: bool,
) -> (Value, Vec<Value>, Vec<Value>) {
    let mut events = Vec::new();
    let call = || {
        ask(s, m, p, o, &mut |e, v| {
            events.push(json!({"event":e,"value":v}))
        })
    };
    let (out, stages) = if enabled {
        capture(call)
    } else {
        (
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), call),
            Vec::new(),
        )
    };
    (out, events, stages)
}

fn named(name: &str, hidden: bool) -> Principal {
    let mut p = Principal::local();
    p.kind = crate::auth::Kind::User;
    p.name = name.into();
    if hidden {
        p = p.hiding_graphs(&["urn:private".into()]);
    }
    p
}

fn fixture() -> McpServer {
    let s = server(&format!(
        "{ORG}\nres:ana rdfs:label \"Ana\"@en .\nres:ana2 a ex:Person ; rdfs:label \"Ana\"@en ; ex:memberOf res:platform .\n"
    ));
    s.state.datasets()["org"]
        .store
        .load(&[Source::from_bytes(
            b"<urn:hiddenSubject> <urn:secretPredicate> 42 .\n<urn:hiddenAna> <http://www.w3.org/2000/01/rdf-schema#label> \"Ana\"@en ; <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.org/ontology#Person> ; <http://example.org/ontology#memberOf> <http://example.org/resource/platform> .".to_vec(),
            RdfFormat::Turtle,
            Some(oxrdf::NamedNode::new("urn:private").unwrap()),
        )])
        .unwrap();
    s
}

fn profile_matrix(rounds: usize, emit: bool) {
    for context in [4096, 200000] {
        for reverse in [false, true] {
            let s = fixture();
            let mock = scripted(
                vec![draft("SELECT ?v WHERE { VALUES ?v { 7 } }")],
                json!({}),
            );
            let m = models(&mock.url(), json!({"contextTokens":context}));
            let users = if reverse {
                [named("bob", true), named("alice", false)]
            } else {
                [named("alice", false), named("bob", true)]
            };
            for p in &users {
                let cold_options = AskOptions {
                    summary: false,
                    mode: AskMode::Local,
                    ..opts("Which team is Ana on?")
                };
                let (cold, _, cold_stages) = observed(&s, &m, p, &cold_options, true);
                assert_eq!(cold["outcome"], "clarify");
                assert_eq!(
                    cold["clarify"]["choices"].as_array().unwrap().len(),
                    if p.name.as_ref() == "bob" { 2 } else { 3 }
                );
                assert_eq!(
                    cold.to_string().contains("hiddenAna"),
                    p.name.as_ref() != "bob"
                );
                if emit {
                    println!(
                        "ASK_PROFILE {}",
                        json!({"case":"clarification","contextTokens":context,"restricted":p.name.as_ref()=="bob","principalOrderReversed":reverse,"round":0,"cache":"first-access-for-view","elapsedMs":cold["usage"]["elapsedMs"],"modelMs":cold["usage"]["modelMs"],"stages":cold_stages})
                    );
                }
                for round in 0..rounds {
                    for (case, question, clarification, linker, recall) in [
                        ("clarification", "Which team is Ana on?", None, true, false),
                        (
                            "follow_up",
                            "Which team is Ana on?",
                            Some("res:ana2"),
                            true,
                            false,
                        ),
                        ("exact", "What is Acme?", None, true, false),
                        ("no_mention", "how many people?", None, false, false),
                        ("community_recall", "What is Acme?", None, true, true),
                    ] {
                        let o = AskOptions {
                            summary: false,
                            mode: AskMode::Local,
                            report_context: recall,
                            clarification: clarification.map(str::to_owned),
                            ..opts(question)
                        };
                        let before = mock.requests().len();
                        let baseline = observed(&s, &m, p, &o, false);
                        let candidate = observed(&s, &m, p, &o, true);
                        assert_eq!(
                            semantic(baseline.0),
                            semantic(candidate.0.clone()),
                            "{case}"
                        );
                        assert_eq!(
                            semantic(json!(baseline.1)),
                            semantic(json!(candidate.1)),
                            "{case}"
                        );
                        let stages = &candidate.2;
                        for name in ["ground", "schema_common", "schema_question", "examples"] {
                            assert_eq!(
                                stages.iter().filter(|v| v["stage"] == name).count(),
                                1,
                                "{case}: {stages:?}"
                            );
                        }
                        assert_eq!(
                            stages.iter().filter(|v| v["stage"] == "linker").count(),
                            usize::from(linker)
                        );
                        assert_eq!(
                            stages
                                .iter()
                                .filter(|v| v["stage"] == "community_recall")
                                .count(),
                            usize::from(recall)
                        );
                        if case == "clarification" {
                            assert_eq!(candidate.0["outcome"], "clarify");
                            assert_eq!(mock.requests().len(), before);
                            assert_eq!(candidate.0["usage"]["modelMs"], 0);
                            assert!(
                                !stages.iter().any(|v| v["stage"] == "query_check"
                                    || v["stage"] == "query_execution")
                            );
                        } else {
                            assert_eq!(candidate.0["outcome"], "answered", "{}", candidate.0);
                            for name in ["query_check", "query_execution"] {
                                assert_eq!(stages.iter().filter(|v| v["stage"] == name).count(), 1);
                            }
                            let requests = mock.requests();
                            let last = requests.len();
                            assert_eq!(
                                mock::prompt_of(&requests[last - 2]),
                                mock::prompt_of(&requests[last - 1])
                            );
                        }
                        assert!(
                            stages
                                .iter()
                                .all(|v| v["calls"] == 1 && v["elapsed_ns"].is_u64())
                        );
                        assert!(stages.iter().all(|v| v["context_tokens"] == context));
                        let diagnostics = json!(stages).to_string();
                        for secret in [
                            question,
                            "Ana",
                            "Acme",
                            "alice",
                            "bob",
                            "secretPredicate",
                            "urn:",
                            "http:",
                            "SELECT",
                        ] {
                            assert!(
                                !diagnostics.contains(secret),
                                "diagnostic leaked {secret}: {diagnostics}"
                            );
                        }
                        if p.name.as_ref() == "bob" {
                            assert!(!json!(candidate.1).to_string().contains("secretPredicate"));
                            assert!(!json!(candidate.1).to_string().contains("hiddenAna"));
                        }
                        if emit {
                            println!(
                                "ASK_PROFILE {}",
                                json!({"case":case,"contextTokens":context,"restricted":p.name.as_ref()=="bob","principalOrderReversed":reverse,"round":round,"cache":"warm-after-baseline","elapsedMs":candidate.0["usage"]["elapsedMs"],"modelMs":candidate.0["usage"]["modelMs"],"stages":stages})
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn stage_tracing_preserves_grounding_answers_choices_and_visibility() {
    profile_matrix(1, false);
}

#[test]
#[ignore = "offline stage measurement; emits every sample and uses only loopback fake providers"]
fn provider_free_stage_profile() {
    profile_matrix(12, true);
}

#[test]
fn stage_errors_are_bounded_and_checks_record_rejection() {
    let ((failed, rejected), stages) = capture(|| {
        let failed = profile::tool(profile::Stage::Examples, || {
            Err(ToolError::new(
                "novel-error",
                500,
                "private question and secret endpoint",
            ))
        });
        let rejected = profile::tool(profile::Stage::QueryCheck, || {
            Ok(json!({"ok":false,"commit":7,"issues":[{"message":"private query"}]}))
        });
        (failed, rejected)
    });
    assert!(failed.is_err());
    assert_eq!(rejected.unwrap()["ok"], false);
    assert_eq!(stages[0]["error_code"], "other");
    assert_eq!(stages[1]["outcome"], "rejected");
    assert_eq!(stages[1]["commit"], 7);
    assert!(!json!(stages).to_string().contains("private"));
}

#[cfg(feature = "text")]
fn embedding_fixture() -> (McpServer, sparkles::vector::embed::mock::MockProvider) {
    use sparkles::vector::embed::{Environment, mock::MockProvider};
    let s = fixture();
    let ds = s.state.datasets()["org"].clone();
    let provider = MockProvider::start(8);
    ds.dataset
        .indexes()
        .vector()
        .set_embedding_environment(Some(Environment {
            outbound: sparkles::outbound::OutboundPolicy {
                allow_private: true,
                ..Default::default()
            },
            ..Default::default()
        }));
    ds.queries.put("team", serde_json::from_value(json!({
        "query":"SELECT ?t WHERE { <http://example.org/resource/ana> <http://example.org/ontology#memberOf> ?t }",
        "description":"Which team is Ana on?", "questions":["Which team is Ana on?"]
    })).unwrap(), sparkles::stored::Change::default()).unwrap();
    ds.dataset.indexes().vector().put("labels", serde_json::from_value(json!({
        "predicate":"urn:profile:embedding", "dimension":8,
        "embedding":{"url":provider.url(),"model":"mock","predicates":["http://www.w3.org/2000/01/rdf-schema#label"]}
    })).unwrap()).unwrap();
    ds.dataset
        .indexes()
        .vector()
        .embed_until_idle(Duration::from_secs(30))
        .unwrap();
    (s, provider)
}

#[cfg(feature = "text")]
#[test]
fn embedding_work_is_nested_and_is_not_ask_role_model_time() {
    let (s, provider) = embedding_fixture();
    let before = provider.state().requests;
    let mock = scripted(vec![], json!({}));
    let m = models(&mock.url(), json!({}));
    let o = AskOptions {
        mode: AskMode::Local,
        summary: false,
        ..opts("Which team is Ana on?")
    };
    let (out, _, stages) = observed(&s, &m, &Principal::local(), &o, true);
    assert_eq!(out["outcome"], "clarify", "{out}");
    assert_eq!(out["usage"]["modelCalls"], 0);
    assert_eq!(out["usage"]["modelMs"], 0);
    assert!(mock.requests().is_empty());
    assert!(
        provider.state().requests > before,
        "the fixture must actually request embeddings"
    );
    for stage in [
        "example_query_embedding",
        "example_document_embedding",
        "linker_vector_search",
    ] {
        assert!(
            stages
                .iter()
                .any(|v| v["stage"] == stage && v["calls"] == 1 && v["outcome"] == "ok"),
            "missing {stage}: {stages:?}"
        );
    }
    assert!(!json!(stages).to_string().contains(&provider.url()));
}

#[derive(Clone, Default)]
struct JsonBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for JsonBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for JsonBuffer {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

fn production_json<T>(call: impl FnOnce() -> T) -> (T, Vec<Value>) {
    let _second = tracing::Dispatch::new(tracing_subscriber::registry());
    let buffer = JsonBuffer::default();
    let fmt = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(buffer.clone());
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            "warn,sparkles::ask::profile=debug",
        ))
        .with(fmt);
    let result = tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        call()
    });
    let text = String::from_utf8(buffer.0.lock().clone()).unwrap();
    let lines = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["target"] == TARGET)
        .collect();
    (result, lines)
}

#[cfg(feature = "text")]
#[test]
fn production_json_correlates_requests_after_grounding_and_nested_embeddings() {
    let (s, provider) = embedding_fixture();
    let mock = scripted(
        vec![draft("SELECT ?v WHERE { VALUES ?v { 7 } }")],
        json!({}),
    );
    let small = models(&mock.url(), json!({"contextTokens":4096}));
    let large = models(&mock.url(), json!({"contextTokens":200000}));
    let options = |clarification| AskOptions {
        summary: false,
        mode: AskMode::Local,
        id: Some("private-client-identifier".into()),
        clarification,
        ..opts("Which team is Ana on?")
    };
    let first = options(None);
    let second = options(Some("res:ana2".into()));
    let before = provider.state().requests;
    let (answers, logs) = production_json(|| {
        let mut events = Vec::new();
        let first = ask(&s, &small, &Principal::local(), &first, &mut |e, v| {
            events.push(json!({"event":e,"value":v}))
        });
        let second = ask(&s, &large, &Principal::local(), &second, &mut |e, v| {
            events.push(json!({"event":e,"value":v}))
        });
        (first, second, events)
    });
    assert!(provider.state().requests > before);
    assert!(logs.iter().all(|line| line.get("spans").is_none()));
    let baseline_first = observed(&s, &small, &Principal::local(), &first, false);
    let baseline_second = observed(&s, &large, &Principal::local(), &second, false);
    assert_eq!(semantic(answers.0.clone()), semantic(baseline_first.0));
    assert_eq!(semantic(answers.1.clone()), semantic(baseline_second.0));
    let mut baseline_events = baseline_first.1;
    baseline_events.extend(baseline_second.1);
    assert_eq!(semantic(json!(answers.2)), semantic(json!(baseline_events)));
    assert_eq!(answers.0["outcome"], "clarify");
    assert_eq!(answers.0["usage"]["modelCalls"], 0);
    assert_eq!(answers.0["usage"]["modelMs"], 0);
    assert_eq!(answers.1["outcome"], "answered");
    let mut ids = BTreeSet::new();
    for context in [4096, 200000] {
        let scoped: Vec<&Value> = logs
            .iter()
            .map(|v| &v["fields"])
            .filter(|v| v["context_tokens"] == context)
            .collect();
        assert!(!scoped.is_empty(), "missing context {context}: {logs:?}");
        let id = scoped[0]["pipeline_id"]
            .as_u64()
            .expect("numeric process-generated pipeline identifier");
        assert!(ids.insert(id), "two asks reused a pipeline identifier");
        assert!(scoped.iter().all(|v| v["pipeline_id"] == id));
        for stage in [
            "schema_common",
            "schema_question",
            "examples",
            "linker",
            "ground",
            "example_query_embedding",
            "example_document_embedding",
            "linker_vector_search",
        ] {
            assert!(
                scoped.iter().any(|v| v["stage"] == stage),
                "missing {stage} for {context}"
            );
        }
        for stage in ["query_check", "query_execution"] {
            assert_eq!(
                scoped.iter().any(|v| v["stage"] == stage),
                context == 200000
            );
        }
    }
    assert!(
        logs.iter()
            .all(|v| v["fields"]["pipeline_id"].is_u64() && v["fields"]["context_tokens"].is_u64())
    );
    for secret in [
        "private-client-identifier",
        "Ana",
        "res:ana2",
        "http:",
        "urn:",
        "SELECT",
    ] {
        assert!(
            !json!(logs).to_string().contains(secret),
            "profile logs leaked {secret}"
        );
    }
}

#[test]
fn production_json_restores_scope_after_nested_asks_and_unwind() {
    let s = fixture();
    let mock = scripted(vec![], json!({}));
    let small = models(&mock.url(), json!({"contextTokens":4096}));
    let large = models(&mock.url(), json!({"contextTokens":200000}));
    let o = AskOptions {
        summary: false,
        mode: AskMode::Local,
        ..opts("Which team is Ana on?")
    };
    let (_, logs) = production_json(|| {
        let outer = ask(&s, &small, &Principal::local(), &o, &mut |event, _| {
            if event == "ground" {
                let inner = ask(&s, &large, &Principal::local(), &o, &mut |_, _| {});
                assert_eq!(inner["outcome"], "clarify");
            }
        });
        assert_eq!(outer["outcome"], "clarify");
        let standalone =
            || profile::nested(profile::Stage::LinkerVectorSearch, 1, 0, || Ok::<_, ()>(()));
        standalone().unwrap();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ask(&s, &large, &Principal::local(), &o, &mut |event, _| {
                if event == "ground" {
                    panic!("intentional diagnostic-scope unwind");
                }
            })
        }));
        assert!(unwind.is_err());
        standalone().unwrap();
    });
    assert!(mock.requests().is_empty());
    let standalone: Vec<&Value> = logs
        .iter()
        .map(|v| &v["fields"])
        .filter(|v| v["stage"] == "linker_vector_search")
        .collect();
    assert_eq!(standalone.len(), 2);
    assert!(
        standalone
            .iter()
            .all(|v| v["pipeline_id"].is_null() && v["context_tokens"].is_null())
    );
    let scoped: Vec<&Value> = logs
        .iter()
        .map(|v| &v["fields"])
        .filter(|v| v["stage"] != "linker_vector_search")
        .collect();
    let ids: BTreeSet<u64> = scoped
        .iter()
        .map(|v| v["pipeline_id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids.len(), 3);
    let outer_id = scoped.iter().find(|v| v["context_tokens"] == 4096).unwrap()["pipeline_id"]
        .as_u64()
        .unwrap();
    assert!(
        scoped
            .iter()
            .filter(|v| v["context_tokens"] == 4096)
            .all(|v| v["pipeline_id"] == outer_id)
    );
    assert!(
        scoped
            .iter()
            .any(|v| v["stage"] == "ground" && v["pipeline_id"] == outer_id)
    );
    assert!(
        scoped
            .iter()
            .filter(|v| v["context_tokens"] == 200000)
            .all(|v| v["pipeline_id"] != outer_id)
    );
}
