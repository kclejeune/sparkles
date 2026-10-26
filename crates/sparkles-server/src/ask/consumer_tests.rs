//! Conservative versioned evidence consumption without automatic resolution.

use super::*;

/// An explicit kind uses types beyond the three presentation slots, but still
/// offers all choices and stops before contacting the provider.
#[test]
fn consumer_evidence_explicit_film_kind_orders_without_resolving() {
    let ttl = format!(
        r#"{ORG}
      <urn:a-city> a <http://dbpedia.org/ontology/City> ; rdfs:label "Casablanca"@en .
      <urn:b-category> a <http://www.w3.org/2004/02/skos/core#Concept> ; rdfs:label "Casablanca"@en .
      <urn:z-film> a <http://aaa.example/A>, <http://aaa.example/B>, <http://aaa.example/C>, <http://dbpedia.org/ontology/Film> ; rdfs:label "Casablanca"@en ."#
    );
    let s = server(&ttl);
    for context in [4096, 200000] {
        let mock = scripted(vec![], json!({}));
        let m = models(&mock.url(), json!({"contextTokens":context}));
        for (question, expected_first) in [
            ("Who directed the film Casablanca?", "<urn:z-film>"),
            ("Where is Casablanca?", "<urn:a-city>"),
        ] {
            let (out, _) = run(&s, &m, &opts(question));
            assert_eq!(out["outcome"], "clarify", "{out:#}");
            let choices = out["clarify"]["choices"].as_array().unwrap();
            assert_eq!(choices.len(), 3);
            assert_eq!(
                choices[0]["value"], expected_first,
                "explicit named kind must order its typed choice: {out:#}"
            );
            assert!(mock.requests().is_empty());
        }
    }
}

#[test]
fn consumer_evidence_full_identity_survives_display_abbreviation() {
    let long = format!("urn:z-city:{}", "x".repeat(400));
    let ttl = format!(
        r#"{ORG}
      <urn:a-city> a <http://dbpedia.org/ontology/City> ; rdfs:label "Berlin"@en .
      <{long}> a <http://dbpedia.org/ontology/City> ; rdfs:label "Berlin"@en ."#
    );
    let s = server(&ttl);
    let mock = scripted(vec![], json!({}));
    let m = models(&mock.url(), json!({}));
    let (out, _) = run(&s, &m, &opts("Which city is Berlin?"));
    assert_eq!(out["outcome"], "clarify", "{out:#}");
    let choices = out["clarify"]["choices"].as_array().unwrap();
    assert_eq!(choices.len(), 2);
    assert!(
        choices
            .iter()
            .any(|choice| choice["value"] == format!("<{long}>")),
        "full identity must survive presentation truncation: {choices:?}"
    );
    assert!(mock.requests().is_empty());
}

#[test]
fn consumer_evidence_byte_refusal_preserves_clarification() {
    let long_label = "x".repeat(4096);
    let ttl = format!(
        r#"{ORG}
      <urn:a-city> a <http://dbpedia.org/ontology/City> ; rdfs:label "Berlin"@en ; <http://www.w3.org/2004/02/skos/core#altLabel> "{long_label}" .
      <urn:z-city> a <http://dbpedia.org/ontology/City> ; rdfs:label "Berlin"@en ."#
    );
    let original = server(&ttl);
    let s = McpServer::new(
        original.state.clone(),
        crate::mcp::McpConfig {
            max_bytes: 1024,
            ..Default::default()
        },
    );
    let mock = scripted(vec![], json!({}));
    let m = models(&mock.url(), json!({}));
    let (out, _) = run(&s, &m, &opts("Which city is Berlin?"));
    assert_eq!(out["outcome"], "clarify", "{out:#}");
    assert_eq!(out["clarify"]["choices"].as_array().unwrap().len(), 2);
    assert!(mock.requests().is_empty());
}

fn observed_as(
    s: &McpServer,
    m: &Models,
    principal: &Principal,
    o: &AskOptions,
) -> (Value, Vec<Value>, Vec<Value>) {
    let mut events = Vec::new();
    let (out, stages) = super::profile_tests::capture(|| {
        ask(s, m, principal, o, &mut |name, value| {
            events.push(json!({"event":name,"value":value}))
        })
    });
    (out, events, stages)
}

fn principal(name: &str, hidden: bool) -> Principal {
    let mut p = Principal::local();
    p.kind = crate::auth::Kind::User;
    p.name = name.into();
    if hidden {
        p = p.hiding_graphs(&["urn:private-evidence".into()]);
    }
    p
}

#[test]
fn consumer_evidence_respects_full_and_restricted_views_in_both_orders() {
    let ttl = format!(
        r#"{ORG}
      <urn:a-city> a <http://dbpedia.org/ontology/City> ; rdfs:label "Casablanca"@en .
      <urn:z-film> a <http://aaa.example/A>, <http://aaa.example/B>, <http://aaa.example/C>, <http://dbpedia.org/ontology/Film> ; rdfs:label "Casablanca"@en ."#
    );
    for reverse in [false, true] {
        let s = server(&ttl);
        s.state.datasets()["org"].store.load(&[Source::from_bytes(
            br#"<urn:hidden-film> a <http://dbpedia.org/ontology/Film> ; <http://www.w3.org/2000/01/rdf-schema#label> "Casablanca"@en ; <urn:secretPredicate> "private actor label" ."#.to_vec(),
            RdfFormat::Turtle,Some(oxrdf::NamedNode::new("urn:private-evidence").unwrap()))]).unwrap();
        let users = if reverse {
            [principal("limited", true), principal("full", false)]
        } else {
            [principal("full", false), principal("limited", true)]
        };
        for context in [4096, 200000] {
            let mock = scripted(vec![], json!({}));
            let m = models(&mock.url(), json!({"contextTokens":context}));
            for p in &users {
                let (out, events, _) =
                    observed_as(&s, &m, p, &opts("Who directed the film Casablanca?"));
                assert_eq!(out["outcome"], "clarify", "{out}");
                let choices = out["clarify"]["choices"].as_array().unwrap();
                if p.name.as_ref() == "limited" {
                    assert_eq!(choices.len(), 2);
                    assert_eq!(choices[0]["value"], "<urn:z-film>");
                    for secret in ["hidden-film", "private actor label", "secretPredicate"] {
                        assert!(!json!(events).to_string().contains(secret));
                        assert!(!out.to_string().contains(secret));
                    }
                } else {
                    assert_eq!(choices.len(), 3);
                    assert!(choices.iter().any(|c| c["value"] == "<urn:hidden-film>"));
                    assert_eq!(
                        choices[0]["value"], "<urn:a-city>",
                        "two films cannot justify a preference"
                    );
                }
                assert!(mock.requests().is_empty());
            }
        }
    }
}

#[test]
fn consumer_evidence_known_disambiguation_cannot_fall_through_to_display_types() {
    let ttl = format!(
        r#"{ORG}
      <http://aaa.example/CategoryBerlin> a <http://www.w3.org/2004/02/skos/core#Concept> ; rdfs:label "Berlin"@en .
      <http://dbpedia.org/resource/Berlin> a <http://aaa.example/A>, <http://aaa.example/B>, <http://aaa.example/C>, <http://dbpedia.org/ontology/WikimediaDisambiguation> ; rdfs:label "Berlin"@en ."#
    );
    let s = server(&ttl);
    for context in [4096, 200000] {
        let mock = scripted(vec![], json!({}));
        let m = models(&mock.url(), json!({"contextTokens":context}));
        let (out, _) = run(&s, &m, &opts("Where is Berlin?"));
        assert_eq!(out["outcome"], "clarify");
        assert_eq!(out["clarify"]["choices"].as_array().unwrap().len(), 2);
        assert_eq!(
            out["clarify"]["choices"][0]["value"],
            "<http://aaa.example/CategoryBerlin>"
        );
        assert!(mock.requests().is_empty());
    }
}

#[test]
fn consumer_evidence_byte_fallback_preserves_shipped_resource_ordering_and_timing() {
    let alternative = "x".repeat(4096);
    let ttl = format!(
        r#"{ORG}
      <http://aaa.example/CategoryBerlin> a <http://www.w3.org/2004/02/skos/core#Concept> ; rdfs:label "Berlin"@en .
      <http://dbpedia.org/resource/Berlin> a <http://dbpedia.org/ontology/City> ; rdfs:label "Berlin"@en ; <http://www.w3.org/2004/02/skos/core#altLabel> "{alternative}" ."#
    );
    let original = server(&ttl);
    let s = McpServer::new(
        original.state.clone(),
        crate::mcp::McpConfig {
            max_bytes: 1024,
            ..Default::default()
        },
    );
    for context in [4096, 200000] {
        let mock = scripted(vec![], json!({}));
        let m = models(&mock.url(), json!({"contextTokens":context}));
        let (out, _, stages) = observed_as(&s, &m, &Principal::local(), &opts("Where is Berlin?"));
        assert_eq!(out["outcome"], "clarify");
        assert_eq!(out["clarify"]["choices"].as_array().unwrap().len(), 2);
        assert_eq!(
            out["clarify"]["choices"][0]["value"],
            "<http://dbpedia.org/resource/Berlin>"
        );
        let calls: Vec<_> = stages.iter().filter(|v| v["stage"] == "linker").collect();
        assert_eq!(calls.len(), 2, "both tool attempts must be timed");
        assert_eq!(calls[0]["outcome"], "error");
        assert_eq!(calls[1]["outcome"], "ok");
        assert!(mock.requests().is_empty());
    }
}

#[test]
fn consumer_evidence_preserves_branch_historical_identity_and_reasoning_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut state =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    state.read_only = true;
    state.limits.max_result_bytes = None;
    let state = Arc::new(state);
    let ds = state.attach("org", DbType::Persistent, None).unwrap();
    let long = format!("urn:z-history:{}", "x".repeat(400));
    let ttl = format!(
        r#"{ORG}
      <urn:a-city> a <http://dbpedia.org/ontology/City> ; rdfs:label "Berlin"@en .
      <{long}> a <http://dbpedia.org/ontology/City> ; rdfs:label "Berlin"@en ."#
    );
    ds.store
        .load(&[Source::from_bytes(
            ttl.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let historic = ds.store.snapshot().commit;
    ds.store
        .create_branch("triage", &Default::default())
        .unwrap();
    for (branch, iri) in [(None, "urn:main-only"), (Some("triage"), "urn:branch-only")] {
        let ttl = format!(
            r#"<{iri}> a <http://dbpedia.org/ontology/City> ; <http://www.w3.org/2000/01/rdf-schema#label> "Berlin"@en ."#
        );
        let sources = [Source::from_bytes(
            ttl.into_bytes(),
            RdfFormat::Turtle,
            None,
        )];
        match branch {
            None => {
                ds.store.load(&sources).unwrap();
            }
            Some(name) => {
                ds.store.branch(name).unwrap().load(&sources).unwrap();
            }
        }
    }
    let s = McpServer::new(state, crate::mcp::McpConfig::default());
    for context in [4096, 200000] {
        let mock = scripted(vec![], json!({}));
        let m = models(&mock.url(), json!({"contextTokens":context}));
        for (branch, at, expected, forbidden) in [
            (None, Some(json!(historic)), 2, "main-only"),
            (None, None, 3, "branch-only"),
            (Some("triage"), None, 3, "main-only"),
        ] {
            let o = AskOptions {
                branch: branch.map(str::to_owned),
                at,
                reasoning: Some(false),
                ..opts("Which city is Berlin?")
            };
            let (out, _, stages) = observed_as(&s, &m, &Principal::local(), &o);
            assert_eq!(out["outcome"], "clarify", "{out}");
            let choices = out["clarify"]["choices"].as_array().unwrap();
            assert_eq!(choices.len(), expected, "{out}");
            assert!(choices.iter().any(|v| v["value"] == format!("<{long}>")));
            assert!(!out["clarify"].to_string().contains(forbidden));
            if expected == 2 {
                assert!(
                    stages
                        .iter()
                        .filter(|v| v["stage"] == "linker")
                        .all(|v| v["commit"] == historic)
                );
            }
            assert!(mock.requests().is_empty());
        }
    }
}

#[derive(Default)]
struct LinkFailure {
    linker: bool,
    error: bool,
}
impl tracing::field::Visit for LinkFailure {
    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "stage" => self.linker = value == "linker",
            "outcome" => self.error = value == "error",
            _ => {}
        }
    }
}

struct StopAfterEvidenceRefusal {
    cancel: Option<Arc<AtomicBool>>,
    delay: Duration,
    failures: Arc<std::sync::atomic::AtomicUsize>,
}
impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for StopAfterEvidenceRefusal {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != profile::TARGET {
            return;
        }
        let mut fields = LinkFailure::default();
        event.record(&mut fields);
        if fields.linker && fields.error && self.failures.fetch_add(1, Ordering::SeqCst) == 0 {
            if let Some(cancel) = &self.cancel {
                cancel.store(true, Ordering::SeqCst);
            }
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
        }
    }
}

#[test]
fn consumer_evidence_retry_cannot_escape_cancellation_or_the_original_deadline() {
    use tracing_subscriber::prelude::*;
    let alternative = "x".repeat(4096);
    let ttl = format!(
        r#"{ORG}
      <urn:a-city> a <http://dbpedia.org/ontology/City> ; rdfs:label "Berlin"@en ; <http://www.w3.org/2004/02/skos/core#altLabel> "{alternative}" .
      <urn:z-city> a <http://dbpedia.org/ontology/City> ; rdfs:label "Berlin"@en ."#
    );
    let original = server(&ttl);
    let s = McpServer::new(
        original.state.clone(),
        crate::mcp::McpConfig {
            max_bytes: 1024,
            ..Default::default()
        },
    );
    for cancelled in [true, false] {
        let mock = scripted(vec![], json!({}));
        let m = models(&mock.url(), json!({"contextTokens":4096}));
        let cancel = Arc::new(AtomicBool::new(false));
        let failures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = StopAfterEvidenceRefusal {
            cancel: cancelled.then(|| cancel.clone()),
            delay: if cancelled {
                Duration::ZERO
            } else {
                Duration::from_millis(2100)
            },
            failures: failures.clone(),
        };
        let _second = tracing::Dispatch::new(tracing_subscriber::registry());
        let subscriber = tracing_subscriber::registry().with(stop);
        let o = AskOptions {
            cancel: Some(cancel),
            deadline: Duration::from_secs(2),
            ..opts("Which city is Berlin?")
        };
        let (out, _) = tracing::subscriber::with_default(subscriber, || {
            tracing::callsite::rebuild_interest_cache();
            run(&s, &m, &o)
        });
        assert_eq!(out["outcome"], "error", "{out}");
        assert_eq!(
            out["error"]["code"],
            if cancelled { "cancelled" } else { "timeout" },
            "{out}"
        );
        assert_eq!(
            failures.load(Ordering::SeqCst),
            2,
            "one refusal and one stopped fallback attempt"
        );
        assert!(mock.requests().is_empty());
    }
}

#[test]
fn consumer_evidence_permission_error_is_not_retried() {
    use crate::auth::{Access, Endpoint, Grants, Kind, Level, Restricted, Scheme};
    let s = server(ORG);
    let p = Principal::new(
        Kind::User,
        "info-only",
        Scheme::None,
        Access::of(Grants {
            restricted: vec![Restricted {
                dataset: "org".into(),
                level: Level::Read,
                graphs: None,
                endpoints: Some(vec![Endpoint::Info]),
                lifts: vec![],
                branches: None,
                server_models: false,
            }],
            ..Default::default()
        }),
    );
    let mock = scripted(
        vec![draft("SELECT ?v WHERE { VALUES ?v { 7 } }")],
        json!({}),
    );
    let m = models(&mock.url(), json!({}));
    let (out, events, stages) = observed_as(&s, &m, &p, &opts("What is Acme?"));
    let calls: Vec<_> = stages.iter().filter(|v| v["stage"] == "linker").collect();
    assert_eq!(
        calls.len(),
        1,
        "a policy error must not cause a second linking attempt"
    );
    assert_eq!(calls[0]["outcome"], "error");
    assert_ne!(out["outcome"], "answered");
    let ground = events.iter().find(|v| v["event"] == "ground").unwrap();
    assert!(ground["value"]["entities"].as_array().unwrap().is_empty());
}
