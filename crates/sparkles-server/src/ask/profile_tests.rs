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

fn capture<T>(f: impl FnOnce() -> T) -> (T, Vec<Value>) {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let result = tracing::subscriber::with_default(subscriber, f);
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
#[test]
fn embedding_work_is_nested_and_is_not_ask_role_model_time() {
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
