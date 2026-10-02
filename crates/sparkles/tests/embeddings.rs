//! Embeddings computed on write (F08) against a mock OpenAI-compatible endpoint:
//! reconciliation of inserts, changes and deletions, filters, graphs, outages,
//! restarts, re-embedding, rejected inputs, text queries, secrets and the outbound
//! policy.

use sparkles::commit::CommitKind;
use sparkles::outbound::OutboundPolicy;
use sparkles::sparql::update::update;
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
use sparkles::vector::VectorIndexConfig;
use sparkles::vector::embed::mock::MockProvider;
use sparkles::vector::embed::{ApiKey, EmbeddingConfig, Environment, SecretSource};
use std::time::Duration;

const EMB: &str = "http://example.org/emb";
const LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const DIM: usize = 8;
const PREFIXES: &str = "PREFIX ex: <http://example.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX spk: <urn:x-sparkles:> ";

/// Loopback endpoints are allowed: the mock listens there.
fn env() -> Environment {
    Environment {
        outbound: OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn config(mock: &MockProvider, f: impl FnOnce(&mut EmbeddingConfig)) -> VectorIndexConfig {
    let mut c = VectorIndexConfig::new(EMB, DIM);
    let mut e = EmbeddingConfig::new(&mock.url(), "mock-model").from_predicates(&[LABEL]);
    e.max_retries = 0;
    f(&mut e);
    c.embedding = Some(e);
    c
}

fn store(mock: &MockProvider, f: impl FnOnce(&mut EmbeddingConfig)) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.set_embedding_environment(Some(env()));
    s.create_vector_index("docs", config(mock, f)).unwrap();
    s
}

fn run(s: &Store, q: &str) {
    update(s, &format!("{PREFIXES}{q}"), &QueryOptions::default()).unwrap();
}

fn embed(s: &Store) {
    s.embed_until_idle(Duration::from_secs(20)).unwrap();
}

/// The vectors of `subject` (in `graph`, else the default graph), sorted.
fn vectors(s: &Store, subject: &str, graph: Option<&str>) -> Vec<String> {
    let pattern = format!("<{subject}> <{EMB}> ?v");
    let q = match graph {
        Some(g) => format!("SELECT ?v WHERE {{ GRAPH <{g}> {{ {pattern} }} }}"),
        None => format!("SELECT ?v WHERE {{ {pattern} }}"),
    };
    let r = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
    let mut out: Vec<String> = r
        .rows()
        .into_iter()
        .filter_map(|row| match &row[0] {
            Some(oxrdf::Term::Literal(l)) => Some(l.value().to_string()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// The literal the worker writes for `text`.
fn expected(text: &str) -> String {
    sparkles::vector::canonical(&MockProvider::vector(DIM, text))
}

#[test]
fn inserts_changes_and_deletes_are_reconciled() {
    let mock = MockProvider::start(DIM);
    let s = store(&mock, |_| {});
    run(
        &s,
        "INSERT DATA { ex:a rdfs:label \"alpha\" . ex:b rdfs:label \"beta\" . ex:c ex:other \"x\" }",
    );
    let head = s.snapshot().commit;
    let st = s.embedding_status("docs").unwrap();
    assert_eq!((st.state.as_str(), st.backlog), ("paused", 2));
    assert!(st.applied_seq < head);
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/a", None),
        [expected("alpha")]
    );
    assert_eq!(
        vectors(&s, "http://example.org/b", None),
        [expected("beta")]
    );
    assert!(vectors(&s, "http://example.org/c", None).is_empty());
    let st = s.embedding_status("docs").unwrap();
    assert_eq!((st.state.as_str(), st.backlog, st.embedded), ("idle", 0, 2));
    assert_eq!(st.applied_seq, s.snapshot().commit);
    // the vectors came in one commit of their own kind
    let last = s.commit(s.snapshot().commit).unwrap();
    assert_eq!(last.kind, CommitKind::Embed);

    // a changed label replaces its vector; a second label adds one
    run(
        &s,
        "DELETE DATA { ex:a rdfs:label \"alpha\" } ; INSERT DATA { ex:a rdfs:label \"alpha2\" . ex:b rdfs:label \"bravo\" }",
    );
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/a", None),
        [expected("alpha2")]
    );
    let mut b = vec![expected("beta"), expected("bravo")];
    b.sort();
    assert_eq!(vectors(&s, "http://example.org/b", None), b);

    // deleting the text deletes the vectors
    run(&s, "DELETE WHERE { ex:b rdfs:label ?l }");
    embed(&s);
    assert!(vectors(&s, "http://example.org/b", None).is_empty());

    // nothing changed: no request
    let before = mock.state().requests;
    run(&s, "INSERT DATA { ex:a ex:other \"unrelated\" }");
    embed(&s);
    assert_eq!(mock.state().requests, before);
}

#[test]
fn languages_classes_graphs_and_combined_inputs() {
    let mock = MockProvider::start(DIM);
    let s = store(&mock, |e| {
        e.languages = Some(vec!["en".into(), "".into()]);
        e.classes = vec!["http://example.org/Doc".into()];
        e.input_prefix = "passage: ".into();
    });
    run(
        &s,
        r#"INSERT DATA {
        ex:a a ex:Doc ; rdfs:label "cat"@en , "chat"@fr , "plain" .
        ex:b rdfs:label "untyped"@en .
        GRAPH ex:g { ex:c a ex:Doc ; rdfs:label "in a graph" }
    }"#,
    );
    embed(&s);
    let mut a = vec![expected("passage: cat"), expected("passage: plain")];
    a.sort();
    assert_eq!(vectors(&s, "http://example.org/a", None), a);
    assert!(vectors(&s, "http://example.org/b", None).is_empty());
    assert_eq!(
        vectors(&s, "http://example.org/c", Some("http://example.org/g")),
        [expected("passage: in a graph")]
    );
    // the type arrives, then goes
    run(&s, "INSERT DATA { ex:b a ex:Doc }");
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/b", None),
        [expected("passage: untyped")]
    );
    run(&s, "DELETE DATA { ex:b a ex:Doc }");
    embed(&s);
    assert!(vectors(&s, "http://example.org/b", None).is_empty());

    let mock2 = MockProvider::start(DIM);
    let s = store(&mock2, |e| {
        e.combine = true;
        e.predicates.push("http://example.org/body".into());
    });
    run(
        &s,
        "INSERT DATA { ex:d rdfs:label \"Title\" ; ex:body \"Body text\" }",
    );
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/d", None),
        [expected("Title\nBody text")]
    );
}

#[test]
fn query_sources() {
    let mock = MockProvider::start(DIM);
    let s = store(&mock, |e| {
        e.predicates.clear();
        e.query = Some(
            "PREFIX ex: <http://example.org/> SELECT ?s ?text WHERE { ?s ex:title ?t ; ex:year ?y BIND(CONCAT(?t, \" (\", STR(?y), \")\") AS ?text) }".into(),
        );
    });
    run(
        &s,
        "INSERT DATA { ex:a ex:title \"Dune\" ; ex:year 1965 . ex:b ex:title \"no year\" }",
    );
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/a", None),
        [expected("Dune (1965)")]
    );
    assert!(vectors(&s, "http://example.org/b", None).is_empty());
    run(&s, "INSERT DATA { ex:b ex:year 2000 }");
    // passes of query sources are spaced by a second
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/b", None),
        [expected("no year (2000)")]
    );
}

#[test]
fn outages_back_off_and_writes_go_on() {
    let mock = MockProvider::start(DIM);
    let s = store(&mock, |_| {});
    mock.state().fail = Some(503);
    run(&s, "INSERT DATA { ex:a rdfs:label \"alpha\" }");
    assert!(s.embed_until_idle(Duration::from_secs(2)).is_err());
    let st = s.embedding_status("docs").unwrap();
    assert_eq!(st.state, "backoff");
    assert_eq!(st.backlog, 1);
    assert!(st.retry_at.is_some());
    assert!(st.last_error.unwrap().message.contains("503"));
    // writes are not held up
    run(&s, "INSERT DATA { ex:b rdfs:label \"beta\" }");
    assert_eq!(s.embedding_status("docs").unwrap().backlog, 2);
    mock.state().fail = None;
    s.retry_embedding();
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/a", None),
        [expected("alpha")]
    );
    assert_eq!(
        vectors(&s, "http://example.org/b", None),
        [expected("beta")]
    );
    assert_eq!(s.embedding_status("docs").unwrap().state, "idle");
}

#[test]
fn rejected_inputs_fail_alone() {
    let mock = MockProvider::start(DIM);
    let s = store(&mock, |_| {});
    mock.state().reject = Some("BAD".into());
    run(
        &s,
        "INSERT DATA { ex:a rdfs:label \"good one\" . ex:b rdfs:label \"BAD input\" . ex:c rdfs:label \"good two\" . ex:d rdfs:label \"good three\" }",
    );
    embed(&s);
    for x in ["a", "c", "d"] {
        assert_eq!(
            vectors(&s, &format!("http://example.org/{x}"), None).len(),
            1,
            "{x}"
        );
    }
    assert!(vectors(&s, "http://example.org/b", None).is_empty());
    let st = s.embedding_status("docs").unwrap();
    assert_eq!(st.failed, 1);
    assert_eq!(
        st.last_error.unwrap().subject.as_deref(),
        Some("<http://example.org/b>")
    );
    // not tried again until its text changes
    let before = mock.state().requests;
    embed(&s);
    assert_eq!(mock.state().requests, before);
    run(
        &s,
        "DELETE DATA { ex:b rdfs:label \"BAD input\" } ; INSERT DATA { ex:b rdfs:label \"fixed\" }",
    );
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/b", None),
        [expected("fixed")]
    );
}

#[test]
fn restarts_catch_up_without_embedding_again() {
    let dir = tempfile::tempdir().unwrap();
    let mock = MockProvider::start(DIM);
    {
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        s.set_embedding_environment(Some(env()));
        s.create_vector_index("docs", config(&mock, |_| {}))
            .unwrap();
        run(
            &s,
            "INSERT DATA { ex:a rdfs:label \"alpha\" . ex:b rdfs:label \"beta\" }",
        );
        embed(&s);
    }
    assert_eq!(mock.state().inputs.len(), 2);
    {
        // written while no worker ran
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        run(
            &s,
            "INSERT DATA { ex:c rdfs:label \"gamma\" } ; DELETE DATA { ex:b rdfs:label \"beta\" }",
        );
    }
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    s.set_embedding_environment(Some(env()));
    embed(&s);
    // only the new text was sent; the deleted one lost its vector
    assert_eq!(mock.state().inputs, ["alpha", "beta", "gamma"]);
    assert_eq!(
        vectors(&s, "http://example.org/c", None),
        [expected("gamma")]
    );
    assert!(vectors(&s, "http://example.org/b", None).is_empty());

    // re-embedding sends every text again
    s.reembed("docs").unwrap();
    embed(&s);
    let mut sent = mock.state().inputs[3..].to_vec();
    sent.sort();
    assert_eq!(sent, ["alpha", "gamma"]);

    // a new model embeds everything again, a new URL does not
    let mut c = s.vector_configs()["docs"].clone();
    c.embedding.as_mut().unwrap().model = "other-model".into();
    s.create_vector_index("docs", c.clone()).unwrap();
    embed(&s);
    assert_eq!(mock.state().inputs.len(), 7);
    let other = MockProvider::start(DIM);
    c.embedding.as_mut().unwrap().url = other.url();
    s.create_vector_index("docs", c).unwrap();
    embed(&s);
    assert_eq!(other.state().inputs.len(), 0);
}

#[test]
fn bulk_loads_are_embedded() {
    let mock = MockProvider::start(DIM);
    let s = Store::in_memory(StoreOptions {
        bulk_threshold: 10,
        ..Default::default()
    });
    s.set_embedding_environment(Some(env()));
    s.create_vector_index("docs", config(&mock, |_| {}))
        .unwrap();
    let nt: String = (0..50)
        .map(|i| format!("<http://example.org/n{i}> <{LABEL}> \"text {i}\" .\n"))
        .collect();
    s.load(&[sparkles::io::Source::from_bytes(
        nt.into_bytes(),
        sparkles::io::RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/n7", None),
        [expected("text 7")]
    );
    assert_eq!(s.embedding_status("docs").unwrap().embedded, 50);
}

#[test]
fn searches_with_text() {
    let mock = MockProvider::start(DIM);
    let s = store(&mock, |e| e.query_prefix = String::new());
    run(
        &s,
        "INSERT DATA { ex:a rdfs:label \"alpha\" . ex:b rdfs:label \"beta\" . ex:c rdfs:label \"gamma\" }",
    );
    embed(&s);
    let q = format!(
        "{PREFIXES}SELECT ?s ?score WHERE {{ (?s ?score) spk:vectorSearch (ex:emb \"beta\" 1) }}"
    );
    let r = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
    let rows = r.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0][0].as_ref().unwrap().to_string(),
        "<http://example.org/b>"
    );
    // a variable bound to text, and a cached second query
    let requests = mock.state().requests;
    let q = format!(
        "{PREFIXES}SELECT ?s WHERE {{ VALUES ?q {{ \"beta\" }} (?s ?score) spk:vectorSearch (ex:emb ?q 1) }}"
    );
    let r = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
    assert_eq!(
        r.rows()[0][0].as_ref().unwrap().to_string(),
        "<http://example.org/b>"
    );
    assert_eq!(mock.state().requests, requests);
    // an index without a provider
    let q = format!(
        "{PREFIXES}SELECT ?s WHERE {{ (?s ?score) spk:vectorSearch (ex:nope \"beta\" 1) }}"
    );
    let Err(e) = query(s.snapshot(), &q, &QueryOptions::default()) else {
        panic!("no error")
    };
    assert!(e.to_string().contains("no embedding provider"), "{e}");
    // the provider fails: 502-style service error
    mock.state().fail = Some(500);
    let q = format!(
        "{PREFIXES}SELECT ?s WHERE {{ (?s ?score) spk:vectorSearch (ex:emb \"something new\" 1) }}"
    );
    let Err(e) = query(s.snapshot(), &q, &QueryOptions::default()) else {
        panic!("no error")
    };
    assert!(matches!(e, sparkles::Error::Service(_)), "{e:?}");
}

#[test]
fn secrets_and_the_outbound_policy() {
    let mock = MockProvider::start(DIM);
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, "sk-test\n").unwrap();
    let s = store(&mock, |e| {
        e.api_key = Some(ApiKey::Secret("mock".into()));
        e.send_dimensions = true;
    });
    let mut e = env();
    e.secrets
        .insert("mock".into(), SecretSource::File(key.clone()));
    s.set_embedding_environment(Some(e));
    run(&s, "INSERT DATA { ex:a rdfs:label \"alpha\" }");
    embed(&s);
    assert_eq!(
        mock.state().auth.last().unwrap().as_deref(),
        Some("Bearer sk-test")
    );
    assert_eq!(
        mock.state().models.last().unwrap(),
        &("mock-model".to_string(), Some(DIM as u64))
    );

    // an unknown secret fails the requests and shows in the status
    s.set_embedding_environment(Some(env()));
    run(&s, "INSERT DATA { ex:b rdfs:label \"beta\" }");
    assert!(s.embed_until_idle(Duration::from_secs(1)).is_err());
    let err = s
        .embedding_status("docs")
        .unwrap()
        .last_error
        .unwrap()
        .message;
    assert!(err.contains("no embedding secret named \"mock\""), "{err}");

    // the default policy refuses loopback: nothing is sent
    let refused = MockProvider::start(DIM);
    let s = Store::in_memory(StoreOptions::default());
    s.set_embedding_environment(Some(Environment::default()));
    s.create_vector_index("docs", config(&refused, |_| {}))
        .unwrap();
    run(&s, "INSERT DATA { ex:a rdfs:label \"alpha\" }");
    assert!(s.embed_until_idle(Duration::from_secs(1)).is_err());
    assert_eq!(refused.state().requests, 0);
    let err = s
        .embedding_status("docs")
        .unwrap()
        .last_error
        .unwrap()
        .message;
    assert!(err.contains("refused") || err.contains("loopback"), "{err}");

    // disabled: no work, no text queries
    s.set_embedding_environment(Some(Environment {
        enabled: false,
        ..env()
    }));
    assert_eq!(s.embedding_status("docs").unwrap().state, "disabled");
}

#[test]
fn configuration_errors() {
    let mock = MockProvider::start(DIM);
    let s = Store::in_memory(StoreOptions::default());
    let mut c = config(&mock, |_| {});
    c.embedding.as_mut().unwrap().predicates = vec![EMB.into()];
    assert!(
        s.create_vector_index("x", c)
            .unwrap_err()
            .to_string()
            .contains("cannot be a source")
    );
    assert!(s.reembed("x").is_err());
    // credentials belong in apiKey
    let mut c = config(&mock, |_| {});
    c.embedding.as_mut().unwrap().url = "http://user:secret@127.0.0.1:1/v1/embeddings".into();
    assert!(
        s.create_vector_index("y", c)
            .unwrap_err()
            .to_string()
            .contains("credentials go in apiKey")
    );
}

#[test]
fn text_that_changes_while_its_request_is_out_is_embedded_again() {
    use sparkles::vector::embed::Prepared;
    let mock = MockProvider::start(DIM);
    let s = store(&mock, |_| {});
    run(&s, "INSERT DATA { ex:a rdfs:label \"first\" }");
    let batch = loop {
        match s.embed_prepare() {
            Prepared::Batch(b) => break b,
            Prepared::Progress => continue,
            _ => panic!("no batch"),
        }
    };
    let done = batch.run(&|_| true);
    run(
        &s,
        "DELETE DATA { ex:a rdfs:label \"first\" } ; INSERT DATA { ex:a rdfs:label \"second\" }",
    );
    s.embed_apply(done);
    // the stale vector was not written, and the new text waits
    assert!(vectors(&s, "http://example.org/a", None).is_empty());
    assert_eq!(s.embedding_status("docs").unwrap().backlog, 1);
    embed(&s);
    assert_eq!(
        vectors(&s, "http://example.org/a", None),
        [expected("second")]
    );
}
