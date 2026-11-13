//! Durable jobs: the acceptance cases of C26 §8 that a single process shows (A1, A4, A6,
//! A9, A12) and, under `failpoints`, the crash cases (A2, A3, A5, A7).

use super::*;
use crate::Dataset;
use oxrdf::{GraphName, Literal, NamedNode, Quad};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

const G: &str = "urn:g";

fn principal(name: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:x-sparkles:principal:{name}"))
}

fn admin() -> Caller {
    Caller::admin(principal("ops"))
}

fn worker(name: &str) -> Caller {
    Caller::user(principal(name))
}

fn settings() -> Settings {
    let mut s = Settings {
        enabled: true,
        ..Settings::default()
    };
    s.definitions
        .insert("collect".into(), Definition::quads(vec![G.into()]));
    s
}

fn configured(ds: &Dataset) {
    ds.jobs().configure(&settings()).unwrap();
}

fn quad(k: u32) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("urn:s{k}")),
        NamedNode::new_unchecked("urn:p"),
        Literal::new_simple_literal(format!("v{k}")),
        GraphName::NamedNode(NamedNode::new_unchecked(G)),
    )
}

fn has(ds: &Dataset, k: u32) -> bool {
    ds.ask(&format!(
        "ASK {{ GRAPH <{G}> {{ <urn:s{k}> <urn:p> \"v{k}\" }} }}"
    ))
    .unwrap()
}

#[track_caller]
fn code<T: std::fmt::Debug>(r: Result<T>) -> String {
    match r {
        Err(e) => e.code().to_string(),
        Ok(v) => panic!("succeeded: {v:?}"),
    }
}

fn submit(ds: &Dataset, key: &str) -> Submitted {
    ds.jobs()
        .submit(
            &Submission::new("collect", serde_json::json!({"page": 1}), key),
            &admin(),
        )
        .unwrap()
}

fn claim(ds: &Dataset, w: &Caller) -> Claimed {
    ds.jobs().claim(&[], w).unwrap().expect("a run is eligible")
}

fn publication(op: Uuid, k: u32) -> Publication {
    let mut p = Publication::new(op);
    p.insert.push(quad(k));
    p.checkpoint = Some(format!("cursor-{k}"));
    p
}

/// Make the lease this process granted for `run` expire now.
fn expire(ds: &Dataset, run: Uuid) {
    if let Some(v) = ds.jobs().runtime().leases.lock().get_mut(&run) {
        for l in v {
            l.deadline = Instant::now() - Duration::from_millis(1);
        }
    }
}

#[test]
fn configuration_protects_the_ledger_and_refuses_ordinary_writes() {
    let ds = Dataset::memory();
    let status = ds.jobs().settings().unwrap();
    assert!(!status.settings.enabled);
    assert!(status.digest.is_none());
    configured(&ds);
    let status = ds.jobs().settings().unwrap();
    assert!(status.protected);
    assert!(status.revisions.contains_key("collect"));
    let r = ds.update(&format!(
        "INSERT DATA {{ GRAPH <{LEDGER_GRAPH}> {{ <urn:x> <urn:y> <urn:z> }} }}"
    ));
    assert_eq!(code(r), "profile-managed-graph");
    // disabled settings of an unconfigured dataset write nothing, and an unknown
    // definition kind is refused
    let fresh = Dataset::memory();
    fresh.jobs().configure(&Settings::default()).unwrap();
    assert!(fresh.jobs().settings().unwrap().digest.is_none());
    let mut bad = settings();
    bad.definitions.get_mut("collect").unwrap().kind = "shell".into();
    assert!(fresh.jobs().configure(&bad).is_err());
    // operations before enablement are refused
    assert_eq!(code(submit_result(&fresh, "k")), "unsupported-profile");
}

fn submit_result(ds: &Dataset, key: &str) -> Result<Submitted> {
    ds.jobs().submit(
        &Submission::new("collect", serde_json::json!({"page": 1}), key),
        &admin(),
    )
}

#[test]
fn the_ledger_graph_leaves_every_query_view_while_typed_inspection_reads_it() {
    let ds = Dataset::memory();
    configured(&ds);
    let s = submit(&ds, "k1");
    let hidden = ds.store().hidden_graphs();
    assert_eq!(hidden, vec![LEDGER_GRAPH.to_string()]);
    assert!(
        !ds.ask(&format!("ASK {{ GRAPH <{LEDGER_GRAPH}> {{ ?s ?p ?o }} }}"))
            .unwrap()
    );
    assert!(!ds.ask("ASK { GRAPH ?g { ?s ?p ?o } }").unwrap());
    let opts = crate::sparql::QueryOptions {
        graphs: Some(Arc::new(crate::access::GraphAccess::all())),
        ..Default::default()
    };
    let r = ds
        .query_with(
            &format!("ASK {{ GRAPH <{LEDGER_GRAPH}> {{ ?s ?p ?o }} }}"),
            &opts,
        )
        .unwrap();
    assert!(
        !r.boolean,
        "an explicit all-graphs view still leaves the ledger out"
    );
    // options with the defaults applied, and a replaced view, are no way around it
    let mut applied = ds.query_options();
    applied.graphs = Some(Arc::new(crate::access::GraphAccess::all()));
    assert!(applied.defaults_applied);
    let ask = format!("ASK {{ GRAPH <{LEDGER_GRAPH}> {{ ?s ?p ?o }} }}");
    assert!(!ds.query_with(&ask, &applied).unwrap().boolean);
    assert!(ds.jobs().get(s.run, &admin()).unwrap().is_some());
    // options made before the ledger existed leave it out once it does
    let fresh = Dataset::memory();
    let early = fresh.query_options();
    configured(&fresh);
    submit(&fresh, "k1");
    assert!(!fresh.query_with(&ask, &early).unwrap().boolean);
}

#[test]
fn a_registry_that_does_not_read_still_hides_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    configured(&ds);
    submit(&ds, "k1");
    drop(ds);
    std::fs::write(dir.path().join("managed.json"), b"{").unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    assert_eq!(ds.store().hidden_graphs(), vec![LEDGER_GRAPH.to_string()]);
    let ask = format!("ASK {{ GRAPH <{LEDGER_GRAPH}> {{ ?s ?p ?o }} }}");
    let visible = ds.query(&ask).map(|r| r.boolean).unwrap_or(false);
    assert!(!visible, "a damaged registry made the ledger visible");
    assert!(!ds.ask("ASK { GRAPH ?g { ?s ?p ?o } }").unwrap());
}

#[test]
fn statistics_count_only_the_visible_graphs() {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k1");
    submit(&ds, "k2");
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{G}> {{ <urn:s1> a <urn:C> ; <urn:p> \"v\" }} <urn:s2> a <urn:C> }}"
    ))
    .unwrap();
    let st = ds.stats(&Default::default()).unwrap();
    assert_eq!(st.quads, 3, "{st:?}");
    assert_eq!(st.base_quads, 0);
    assert_eq!(st.delta_inserts, 3);
    assert_eq!(st.delta_deletes, 0);
    let names: Vec<Option<&str>> = st.graphs.iter().map(|g| g.name.as_deref()).collect();
    assert_eq!(names, vec![None, Some(G)], "{st:?}");
    assert!(
        st.predicates
            .iter()
            .all(|p| !p.iri.starts_with("urn:x-sparkles:jobs:")),
        "a ledger predicate is listed: {st:?}"
    );
    let rdf_type = st
        .predicates
        .iter()
        .find(|p| p.iri == oxrdf::vocab::rdf::TYPE.as_str())
        .expect("rdf:type is a visible predicate");
    assert_eq!(rdf_type.count, 2, "the runs' types are left out: {st:?}");
    assert_eq!(st.classes.len(), 1, "the run class is left out: {st:?}");
    assert_eq!(st.classes[0].iri, "urn:C");
    assert_eq!(st.classes[0].instances, 2);
    // the figures of the raw snapshot differ, which is what the exclusion hides
    assert!(ds.snapshot().len() > 3);
}

#[cfg(feature = "reasoning")]
#[test]
fn materialization_never_reads_the_ledger() {
    use crate::reasoning::{ReasonRequest, reasoner::GraphRef};
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k1");
    // a public schema triple over a ledger predicate: with the ledger as an input, RDFS
    // would copy every run's definition name into the visible inferred graph
    ds.update(
        "INSERT DATA { <urn:x-sparkles:jobs:definition> \
         <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> <urn:visible-definition> }",
    )
    .unwrap();
    let req = ReasonRequest {
        incremental: false,
        threads: Some(1),
        inputs: crate::reasoning::reasoner::Inputs {
            data_graphs: vec![GraphRef::Default, GraphRef::Named(LEDGER_GRAPH.into())],
            ..Default::default()
        },
        ..Default::default()
    };
    // the resolution the run uses leaves the ledger out, with a warning for the request
    let hidden = ds.store().hidden_graphs();
    let resolved =
        crate::reasoning::reasoner::inputs::resolve_excluding(&ds.snapshot(), &req.inputs, &hidden);
    assert_eq!(resolved.graphs, vec![GraphRef::Default]);
    assert!(
        resolved
            .warnings
            .iter()
            .any(|w| w.contains(LEDGER_GRAPH) && w.contains("left out")),
        "{:?}",
        resolved.warnings
    );
    ds.reasoning().run(&req).unwrap();
    assert!(
        !ds.ask("ASK { ?run <urn:visible-definition> \"collect\" }")
            .unwrap(),
        "materialization copied a hidden ledger value into the inferred graph"
    );
    // the record describes what was read, so a job transition does not stale the closure
    let record = ds.reasoning_record().expect("a record");
    let recorded = record
        .inputs
        .as_ref()
        .map(|i| i.to_string())
        .unwrap_or_default();
    assert!(!recorded.contains(LEDGER_GRAPH), "{record:?}");
}

#[test]
fn predicate_distinct_counts_leave_the_hidden_subjects_and_objects_out() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    configured(&ds);
    submit(&ds, "k");
    ds.update("INSERT DATA { <urn:public> a <urn:Public> }")
        .unwrap();
    ds.compact().unwrap();
    let stats = ds.stats(&Default::default()).unwrap();
    let p = stats
        .predicates
        .iter()
        .find(|p| p.iri == oxrdf::vocab::rdf::TYPE.as_str())
        .unwrap();
    assert_eq!(p.count, 1);
    assert_eq!((p.distinct_subjects, p.distinct_objects), (1, 1), "{p:?}");
}

#[test]
fn the_schema_facade_never_reads_the_ledger() {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k");
    let mut req = crate::handles::ReportRequest::default();
    req.options.graph = crate::schema::GraphSelection::Union;
    let report = ds.schema().report(&req).unwrap();
    let j = serde_json::to_string(report.report.as_ref()).unwrap();
    assert!(!j.contains("urn:x-sparkles:jobs:"), "{j}");
    // an explicit all-graphs view, the report of an opened state and the profiles
    let mut opts = req.options.clone();
    opts.graphs = Some(Arc::new(crate::access::GraphAccess::all()));
    let (r, _) = ds.schema().report_at(&ds.snapshot(), &opts).unwrap();
    let j = serde_json::to_string(r.as_ref()).unwrap();
    assert!(!j.contains("urn:x-sparkles:jobs:"), "{j}");
    let profiles = ds
        .schema()
        .profiles(&crate::schema::profile::ProfileOptions {
            schema: opts,
            classes: Vec::new(),
        })
        .unwrap();
    let j = format!("{profiles:?}");
    assert!(!j.contains("urn:x-sparkles:jobs:"), "{j}");
}

#[cfg(feature = "shacl")]
#[test]
fn library_validation_never_reads_the_ledger() {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k");
    let shapes = sparkles_shacl::Shapes::parse(
        "@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape ; sh:targetClass <urn:x-sparkles:jobs:Run> ; sh:property [ sh:path <urn:x-sparkles:jobs:state> ; sh:maxCount 0 ] .",
        sparkles_shacl::ShapesSyntax::default(),
        None,
    )
    .unwrap();
    for data_graph in ["urn:x-arq:UnionGraph", LEDGER_GRAPH] {
        let opts = sparkles_shacl::ValidateOptions {
            data_graph: Some(data_graph.into()),
            ..Default::default()
        };
        let report = ds.validation().shacl(&shapes, &opts).unwrap();
        assert!(report.conforms, "{data_graph}: {report:?}");
    }
}

#[cfg(feature = "shacl")]
#[test]
fn library_forms_never_read_the_ledger() {
    let ds = Dataset::memory();
    configured(&ds);
    let run = submit(&ds, "k").run;
    ds.update("PREFIX sh: <http://www.w3.org/ns/shacl#> INSERT DATA { GRAPH <urn:shapes> { <urn:S> a sh:NodeShape ; sh:targetClass <urn:x-sparkles:jobs:Run> ; sh:property [ sh:path <urn:x-sparkles:jobs:state> ] } }")
        .unwrap();
    let settings = crate::forms::FormSettings {
        shapes: crate::forms::ShapesSource::Graphs,
        graphs: vec!["urn:shapes".into()],
        ..Default::default()
    };
    let req = crate::forms::FormRequest::new(format!("urn:x-sparkles:jobs:run:{run}"), settings);
    let model = ds.forms().model(&req, &Default::default()).unwrap();
    let j = serde_json::to_string(&model).unwrap();
    assert!(!j.contains("queued"), "{j}");
}

#[test]
fn explanations_never_read_the_ledger() {
    let ds = Dataset::memory();
    configured(&ds);
    let run = submit(&ds, "k").run;
    ds.reasoning()
        .rdfs()
        .set(crate::reasoning::rdfs::NewSchema::Triples(vec![]))
        .unwrap();
    let term = |s: &str| oxrdf::Term::NamedNode(NamedNode::new(s).unwrap());
    let req = crate::reasoning::explain::ExplainRequest::new([
        term(&format!("urn:x-sparkles:jobs:run:{run}")),
        term(oxrdf::vocab::rdf::TYPE.as_str()),
        term("urn:x-sparkles:jobs:Run"),
    ]);
    let result = ds.reasoning().explain(&req).unwrap();
    assert!(
        matches!(result.explained, crate::explain::Explained::NotFound),
        "{:?}",
        result.explained
    );
}

#[test]
fn a1_submission_is_idempotent_by_key_and_payload() {
    let ds = Dataset::memory();
    configured(&ds);
    let first = submit(&ds, "advisory-1");
    assert!(!first.replayed);
    assert_eq!(first.state, State::Queued);
    let again = ds
        .jobs()
        .submit(
            // member order and whitespace do not change the digest
            &Submission::new("collect", serde_json::json!({"page": 1}), "advisory-1"),
            &admin(),
        )
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.run, first.run);
    let changed = ds.jobs().submit(
        &Submission::new("collect", serde_json::json!({"page": 2}), "advisory-1"),
        &admin(),
    );
    assert_eq!(code(changed), "idempotency-conflict");
    // another principal's key is another scope
    let other = ds
        .jobs()
        .submit(
            &Submission::new("collect", serde_json::json!({"page": 1}), "advisory-1"),
            &Caller::admin(principal("other")),
        )
        .unwrap();
    assert_ne!(other.run, first.run);
    let view = ds.jobs().get(first.run, &admin()).unwrap().unwrap();
    assert_eq!(view.version, 1);
    assert_eq!(view.attempts, 0);
    assert_eq!(
        ds.jobs()
            .list(&ListFilter::default(), &admin())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn claim_publish_and_replay_commit_business_quads_checkpoint_and_state_together() {
    let ds = Dataset::memory();
    configured(&ds);
    let s = submit(&ds, "k1");
    let w = worker("w1");
    let c = claim(&ds, &w);
    assert_eq!(c.run, s.run);
    assert_eq!(c.generation, 1);
    assert_eq!(c.number, 1);
    assert_eq!(c.payload, serde_json::json!({"page": 1}));
    assert_eq!(c.graphs, vec![G.to_string()]);
    assert!(
        ds.jobs().claim(&[], &worker("w2")).unwrap().is_none(),
        "concurrency 1"
    );
    let renewed = ds
        .jobs()
        .renew(c.run, &c.token, Some(5_000), Some(c.version), &w)
        .unwrap();
    assert_eq!(renewed.version, c.version + 1);
    let stale = ds.jobs().renew(c.run, &c.token, None, Some(c.version), &w);
    assert!(matches!(stale, Err(Error::PreconditionFailed(_))));
    let op = Uuid::new_v4();
    let head = ds.store().head_commit().seq;
    let p = ds
        .jobs()
        .publish(c.run, &c.token, &publication(op, 1), &w)
        .unwrap();
    assert!(!p.replayed);
    assert_eq!(
        p.commit,
        head + 1,
        "one commit holds the quads and the ledger"
    );
    assert!(has(&ds, 1));
    let view = ds.jobs().get(c.run, &admin()).unwrap().unwrap();
    assert_eq!(view.state, State::Succeeded);
    assert_eq!(view.checkpoint.as_deref(), Some("cursor-1"));
    assert_eq!(view.publication.as_ref().unwrap().commit, p.commit);
    assert_eq!(
        view.attempt.as_ref().unwrap().state,
        ledger::AttemptState::Published
    );
    // the same operation returns the original receipt even after the lease is gone
    let again = ds
        .jobs()
        .publish(c.run, &c.token, &publication(op, 1), &w)
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.commit, p.commit);
    assert_eq!(ds.store().head_commit().seq, p.commit);
    let other = ds.jobs().publish(c.run, &c.token, &publication(op, 2), &w);
    assert_eq!(code(other), "publication-conflict");
    assert!(!has(&ds, 2));
    // the receipt is the worker's own: a stranger with the operation id learns nothing,
    // as for an unknown run, the worker with a wrong token learns that its lease is
    // lost, and a run the worker never worked on is unknown to it
    let stranger = worker("stranger");
    assert_eq!(
        code(
            ds.jobs()
                .publish(c.run, &c.token, &publication(op, 1), &stranger)
        ),
        "not-found"
    );
    assert_eq!(
        code(
            ds.jobs()
                .publish(Uuid::new_v4(), &c.token, &publication(op, 1), &stranger)
        ),
        "not-found"
    );
    assert_eq!(
        code(ds.jobs().publish(c.run, "wrong", &publication(op, 1), &w)),
        "lease-lost"
    );
    let s2 = submit(&ds, "k2");
    assert_eq!(
        code(ds.jobs().publish(s2.run, &c.token, &publication(op, 1), &w)),
        "not-found"
    );
    assert_eq!(
        code(ds.jobs().renew(s2.run, &c.token, None, None, &stranger)),
        "not-found"
    );
    assert_eq!(
        code(ds.jobs().renew(s2.run, &c.token, None, None, &admin())),
        "lease-lost",
        "the submitter learns the state"
    );
    // a new operation under the finished lease is fenced
    let late = ds
        .jobs()
        .publish(c.run, &c.token, &publication(Uuid::new_v4(), 3), &w);
    assert_eq!(code(late), "lease-lost");
    assert_eq!(
        code(ds.jobs().renew(c.run, &c.token, None, None, &w)),
        "lease-lost"
    );
}

#[test]
fn a4_an_expired_worker_cannot_renew_publish_or_fail_after_another_claims() {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k1");
    let a = worker("a");
    let b = worker("b");
    let ca = claim(&ds, &a);
    assert!(
        ds.jobs().claim(&[], &b).unwrap().is_none(),
        "the lease is live"
    );
    expire(&ds, ca.run);
    // the expired attempt may have had an effect, so the run follows its retry policy
    // before another worker takes it over (C26 §4.2)
    assert!(ds.jobs().claim(&[], &b).unwrap().is_none());
    let view = ds.jobs().get(ca.run, &admin()).unwrap().unwrap();
    assert_eq!(view.state, State::RetryWait);
    assert_eq!(
        view.attempt.as_ref().unwrap().state,
        ledger::AttemptState::Expired
    );
    assert!(view.reason.unwrap().contains("the lease expired"));
    assert!(view.next_eligible_ms.unwrap() > ledger::now_ms() - 1_000);
    force_eligible(&ds, ca.run);
    let cb = claim(&ds, &b);
    assert_eq!(cb.run, ca.run);
    assert_eq!(cb.generation, 2);
    assert_eq!(cb.number, 2);
    assert_eq!(
        code(ds.jobs().renew(ca.run, &ca.token, None, None, &a)),
        "lease-lost"
    );
    assert_eq!(
        code(
            ds.jobs()
                .publish(ca.run, &ca.token, &publication(Uuid::new_v4(), 1), &a)
        ),
        "lease-lost"
    );
    let f = Failure {
        transient: true,
        reason: "stalled".into(),
        retry_after_ms: None,
    };
    assert_eq!(
        code(ds.jobs().fail(ca.run, &ca.token, &f, None, &a)),
        "lease-lost"
    );
    assert!(!has(&ds, 1));
    // b's token used by a is refused too
    assert_eq!(
        code(ds.jobs().renew(cb.run, &cb.token, None, None, &a)),
        "lease-lost"
    );
    let p = ds
        .jobs()
        .publish(cb.run, &cb.token, &publication(Uuid::new_v4(), 2), &b)
        .unwrap();
    assert!(has(&ds, 2));
    let view = ds.jobs().get(p.run, &admin()).unwrap().unwrap();
    assert_eq!(view.attempts, 2);
    assert_eq!(view.attempt.unwrap().worker, principal("b").as_str());
}

#[test]
fn a6_cancellation_fences_publication_and_a_terminal_run_is_immutable() {
    let ds = Dataset::memory();
    configured(&ds);
    let s = submit(&ds, "k1");
    let w = worker("w");
    let c = claim(&ds, &w);
    let view = ds.jobs().cancel(c.run, &admin()).unwrap();
    assert_eq!(view.state, State::Cancelled);
    assert!(view.reason.unwrap().contains("may still take effect"));
    assert_eq!(
        code(
            ds.jobs()
                .publish(c.run, &c.token, &publication(Uuid::new_v4(), 1), &w)
        ),
        "lease-lost"
    );
    assert!(!has(&ds, 1));
    assert!(matches!(
        ds.jobs().cancel(c.run, &admin()),
        Err(Error::Conflict(_))
    ));
    // the submitter may cancel a queued run, a stranger cannot see it
    let s2 = submit(&ds, "k2");
    assert!(
        ds.jobs()
            .get(s2.run, &worker("stranger"))
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        ds.jobs().cancel(s2.run, &worker("stranger")),
        Err(Error::NotFound(_))
    ));
    ds.jobs().cancel(s2.run, &admin()).unwrap();
    // an explicit retry makes a new run that links the old one
    let r = ds.jobs().retry(s.run, "k1-retry", &admin()).unwrap();
    assert_ne!(r.run, s.run);
    let view = ds.jobs().get(r.run, &admin()).unwrap().unwrap();
    assert_eq!(view.retry_of, Some(s.run));
    assert_eq!(view.state, State::Queued);
    assert!(matches!(
        ds.jobs().retry(r.run, "again", &admin()),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn failures_retry_with_backoff_within_attempts_and_end_otherwise() {
    let ds = Dataset::memory();
    let mut s = settings();
    s.definitions.get_mut("collect").unwrap().attempts = 2;
    ds.jobs().configure(&s).unwrap();
    submit(&ds, "k1");
    let w = worker("w");
    let c = claim(&ds, &w);
    let transient = Failure {
        transient: true,
        reason: "503 from the source\u{0007}".into(),
        retry_after_ms: Some(30_000),
    };
    let view = ds
        .jobs()
        .fail(c.run, &c.token, &transient, None, &w)
        .unwrap();
    assert_eq!(view.state, State::RetryWait);
    let next = view.next_eligible_ms.unwrap();
    assert!(next >= ledger::now_ms() + 29_000, "Retry-After is honored");
    let a = view.attempt.unwrap();
    assert_eq!(a.state, ledger::AttemptState::Failed);
    assert_eq!(a.diagnostic.as_deref(), Some("503 from the source"));
    assert!(
        ds.jobs().claim(&[], &w).unwrap().is_none(),
        "not yet eligible"
    );
    assert_eq!(
        code(ds.jobs().renew(c.run, &c.token, None, None, &w)),
        "lease-lost"
    );
    // make it eligible now and exhaust the attempts
    let mut def = s.definitions["collect"].clone();
    def.lease_ms = 1;
    def.max_lease_extension_ms = 1;
    let _ = def;
    force_eligible(&ds, c.run);
    let c2 = claim(&ds, &w);
    assert_eq!(c2.number, 2);
    let view = ds
        .jobs()
        .fail(c2.run, &c2.token, &transient, None, &w)
        .unwrap();
    assert_eq!(view.state, State::Failed);
    assert!(view.reason.unwrap().contains("exhausted"));
    // a permanent failure ends a run at once
    submit(&ds, "k2");
    let c3 = claim(&ds, &w);
    let permanent = Failure {
        transient: false,
        reason: "schema rejected".into(),
        retry_after_ms: None,
    };
    let view = ds
        .jobs()
        .fail(c3.run, &c3.token, &permanent, None, &w)
        .unwrap();
    assert_eq!(view.state, State::Failed);
    assert_eq!(view.attempts, 1);
    let failed = ds
        .jobs()
        .list(
            &ListFilter {
                state: Some(State::Failed),
                ..Default::default()
            },
            &admin(),
        )
        .unwrap();
    assert_eq!(failed.len(), 2);
}

/// Move a waiting run's eligibility into the past through the typed path a test owns:
/// an administrator's cancel and retry would make a new run, so this rewrites the one
/// value under the capability, as a clock change in a later process would.
fn force_eligible(ds: &Dataset, run: Uuid) {
    let store = ds.store();
    let cap = store
        .managed_writer(OwnerKind::Jobs, Uuid::new_v4())
        .unwrap();
    let mut t = store.write();
    t.set_managed(cap).unwrap();
    let base = t.base().clone();
    let prep =
        crate::access::Preparation::new(crate::task::Control::none(), ledger::READS_BYTES).unwrap();
    let mut l = ledger::Ledger::new(&prep, &base);
    let mut ch = ledger::Changes::default();
    ch.set(
        &mut l,
        &ledger::run_iri(run),
        "nextEligibleAtMs",
        Some(ledger::ms(0)),
    )
    .unwrap();
    drop(l);
    ch.apply(&mut t).unwrap();
    t.commit().unwrap();
}

#[test]
fn a12_invalid_results_publish_nothing_and_overload_stores_no_run() {
    let ds = Dataset::memory();
    let mut s = settings();
    s.max_queued = 1;
    s.definitions.get_mut("collect").unwrap().max_output_quads = 1;
    ds.jobs().configure(&s).unwrap();
    submit(&ds, "k1");
    assert_eq!(code(submit_result(&ds, "k2")), "jobs-overloaded");
    let w = worker("w");
    let c = claim(&ds, &w);
    let head = ds.store().head_commit().seq;
    // too many quads
    let mut p = publication(Uuid::new_v4(), 1);
    p.insert.push(quad(2));
    assert!(matches!(
        ds.jobs().publish(c.run, &c.token, &p, &w),
        Err(Error::Invalid(_))
    ));
    // a graph outside the definition
    let mut p = Publication::new(Uuid::new_v4());
    p.insert.push(Quad::new(
        NamedNode::new_unchecked("urn:s"),
        NamedNode::new_unchecked("urn:p"),
        Literal::new_simple_literal("v"),
        GraphName::NamedNode(NamedNode::new_unchecked("urn:elsewhere")),
    ));
    assert!(matches!(
        ds.jobs().publish(c.run, &c.token, &p, &w),
        Err(Error::NotPermitted(_))
    ));
    // the ledger graph itself, even when a definition names it
    let mut s2 = s.clone();
    s2.definitions
        .get_mut("collect")
        .unwrap()
        .graphs
        .push(LEDGER_GRAPH.into());
    assert_eq!(
        ds.store().head_commit().seq,
        head,
        "refusals commit nothing"
    );
    let view = ds.jobs().get(c.run, &admin()).unwrap().unwrap();
    assert_eq!(
        view.state,
        State::Leased,
        "the lease survives a refused result"
    );
    let ok = ds
        .jobs()
        .publish(c.run, &c.token, &publication(Uuid::new_v4(), 1), &w)
        .unwrap();
    assert!(!ok.replayed);
    assert!(has(&ds, 1));
    // the queue has room again
    submit(&ds, "k2");
}

#[test]
fn a_changed_definition_stops_claims_and_a_worker_grant_restricts_them() {
    let ds = Dataset::memory();
    let mut s = settings();
    s.definitions.get_mut("collect").unwrap().workers = vec![principal("w").as_str().into()];
    ds.jobs().configure(&s).unwrap();
    submit(&ds, "k1");
    assert!(ds.jobs().claim(&[], &worker("other")).unwrap().is_none());
    assert!(
        ds.jobs()
            .claim(&["other-def".into()], &worker("w"))
            .unwrap()
            .is_none()
    );
    // editing the definition changes its revision, so the accepted run waits
    s.definitions.get_mut("collect").unwrap().attempts = 3;
    ds.jobs().configure(&s).unwrap();
    assert!(ds.jobs().claim(&[], &worker("w")).unwrap().is_none());
    let view = ds
        .jobs()
        .get(submit(&ds, "k1").run, &admin())
        .unwrap()
        .unwrap();
    assert_eq!(view.state, State::Queued);
    // restoring it lets the run go
    s.definitions.get_mut("collect").unwrap().attempts = 8;
    ds.jobs().configure(&s).unwrap();
    assert!(ds.jobs().claim(&[], &worker("w")).unwrap().is_some());
    // jobs cannot be disabled under a live run
    let mut off = s.clone();
    off.enabled = false;
    assert!(matches!(ds.jobs().configure(&off), Err(Error::Conflict(_))));
}

#[test]
fn a_removed_disabled_or_edited_definition_fences_the_lease() {
    let ds = Dataset::memory();
    let mut s = settings();
    s.definitions.get_mut("collect").unwrap().workers = vec![principal("w").as_str().into()];
    ds.jobs().configure(&s).unwrap();
    submit(&ds, "k1");
    let w = worker("w");
    let f = Failure {
        transient: true,
        reason: "stalled".into(),
        retry_after_ms: None,
    };
    let fenced = |c: &Claimed| {
        let head = ds.store().head_commit().seq;
        assert_eq!(
            code(ds.jobs().renew(c.run, &c.token, None, None, &w)),
            "lease-lost"
        );
        assert_eq!(
            code(
                ds.jobs()
                    .publish(c.run, &c.token, &publication(Uuid::new_v4(), 1), &w)
            ),
            "lease-lost"
        );
        assert_eq!(
            code(ds.jobs().fail(c.run, &c.token, &f, None, &w)),
            "lease-lost"
        );
        assert!(!has(&ds, 1));
        assert_eq!(
            ds.store().head_commit().seq,
            head,
            "refusals commit nothing"
        );
        let view = ds.jobs().get(c.run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::Leased);
    };
    // the worker is removed from the definition
    let c = claim(&ds, &w);
    let mut revoked = s.clone();
    revoked.definitions.get_mut("collect").unwrap().workers =
        vec![principal("other").as_str().into()];
    ds.jobs().configure(&revoked).unwrap();
    fenced(&c);
    // restoring the definition restores the lease, and disabling it fences again
    ds.jobs().configure(&s).unwrap();
    ds.jobs().renew(c.run, &c.token, None, None, &w).unwrap();
    let mut disabled = s.clone();
    disabled.definitions.get_mut("collect").unwrap().enabled = false;
    ds.jobs().configure(&disabled).unwrap();
    fenced(&c);
    // an edit of the definition's bounds is another revision
    let mut edited = s.clone();
    edited
        .definitions
        .get_mut("collect")
        .unwrap()
        .graphs
        .push("urn:h".into());
    ds.jobs().configure(&edited).unwrap();
    fenced(&c);
    // and its removal
    let mut removed = s.clone();
    removed.definitions.clear();
    ds.jobs().configure(&removed).unwrap();
    fenced(&c);
    ds.jobs().configure(&s).unwrap();
    ds.jobs()
        .publish(c.run, &c.token, &publication(Uuid::new_v4(), 1), &w)
        .unwrap();
    assert!(has(&ds, 1));
}

#[test]
fn equal_definitions_and_retries_have_separate_operation_scopes() {
    let ds = Dataset::memory();
    let mut s = settings();
    let same = s.definitions["collect"].clone();
    s.definitions.insert("collect2".into(), same);
    ds.jobs().configure(&s).unwrap();
    let a = submit(&ds, "k1");
    let b = ds
        .jobs()
        .submit(
            &Submission::new("collect2", serde_json::json!({"page": 1}), "k1"),
            &admin(),
        )
        .unwrap();
    assert!(
        !b.replayed,
        "an equal definition under another name is another scope"
    );
    assert_ne!(a.run, b.run);
    // a retry under the predecessor's key is a new run, not the predecessor's receipt
    ds.jobs().cancel(a.run, &admin()).unwrap();
    let r = ds.jobs().retry(a.run, "k1", &admin()).unwrap();
    assert!(!r.replayed);
    assert_ne!(r.run, a.run);
    assert_eq!(r.state, State::Queued);
    let again = ds.jobs().retry(a.run, "k1", &admin()).unwrap();
    assert!(again.replayed);
    assert_eq!(again.run, r.run);
    // a retry of another predecessor of the same definition under the same key is a
    // conflict, not a replay
    let a2 = submit(&ds, "k2");
    ds.jobs().cancel(a2.run, &admin()).unwrap();
    assert_eq!(
        code(ds.jobs().retry(a2.run, "k1", &admin())),
        "idempotency-conflict"
    );
    // and another definition's retry is another scope
    ds.jobs().cancel(b.run, &admin()).unwrap();
    assert!(!ds.jobs().retry(b.run, "k1", &admin()).unwrap().replayed);
}

#[test]
fn a_changed_checkpoint_or_input_set_under_a_known_key_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
        .unwrap();
    ds.update("INSERT DATA { <urn:a> <urn:b> <urn:d> }")
        .unwrap();
    configured(&ds);
    let branch = ds.store().branch_id();
    let input = |seq| crate::branch::CommitRef {
        branch_id: branch,
        seq,
    };
    let mut sub = Submission::new("collect", serde_json::json!({"page": 1}), "k1");
    sub.checkpoint = Some("cursor-0".into());
    sub.inputs = vec![input(2), input(1)];
    let first = ds.jobs().submit(&sub, &admin()).unwrap();
    // the input order is not part of the request
    let mut reordered = sub.clone();
    reordered.inputs = vec![input(1), input(2), input(2)];
    let again = ds.jobs().submit(&reordered, &admin()).unwrap();
    assert!(again.replayed);
    assert_eq!(again.run, first.run);
    let mut other_checkpoint = sub.clone();
    other_checkpoint.checkpoint = Some("cursor-1".into());
    assert_eq!(
        code(ds.jobs().submit(&other_checkpoint, &admin())),
        "idempotency-conflict"
    );
    let mut no_checkpoint = sub.clone();
    no_checkpoint.checkpoint = None;
    assert_eq!(
        code(ds.jobs().submit(&no_checkpoint, &admin())),
        "idempotency-conflict"
    );
    let mut empty_checkpoint = sub.clone();
    empty_checkpoint.checkpoint = Some(String::new());
    assert_eq!(
        code(ds.jobs().submit(&empty_checkpoint, &admin())),
        "idempotency-conflict"
    );
    let mut fewer_inputs = sub.clone();
    fewer_inputs.inputs = vec![input(1)];
    assert_eq!(
        code(ds.jobs().submit(&fewer_inputs, &admin())),
        "idempotency-conflict"
    );
    // refused submissions leave no hold under a run that does not exist
    let holds = ds.store().source_holds().unwrap();
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].id, first.run);
    // a publication's absent checkpoint is not an empty one
    let mut absent = Publication::new(Uuid::new_v4());
    absent.insert.push(quad(1));
    let mut empty = absent.clone();
    empty.checkpoint = Some(String::new());
    assert_ne!(absent.digest(), empty.digest());
    // more inputs than a run may pin are refused before anything is held
    let mut many = Submission::new("collect", serde_json::json!({}), "k-many");
    many.inputs = (0..=sparkles_core::jobs::MAX_INPUTS as u64)
        .map(|i| crate::branch::CommitRef {
            branch_id: Uuid::new_v4(),
            seq: i,
        })
        .collect();
    assert!(matches!(
        ds.jobs().submit(&many, &admin()),
        Err(Error::Invalid(_))
    ));
    assert_eq!(ds.store().source_holds().unwrap().len(), 1);
}

#[test]
fn a_submitter_whose_view_cannot_write_a_definition_graph_is_refused() {
    let ds = Dataset::memory();
    configured(&ds);
    use crate::access::{GraphAccess, GraphRule, Graphs};
    let only = |g: &str| {
        Arc::new(GraphAccess::graphs(
            Graphs::All,
            Graphs::Only(GraphRule::new([g], &[])),
        ))
    };
    let mut restricted = Caller::user(principal("narrow"));
    restricted.write = Some(only("urn:other"));
    assert!(matches!(
        ds.jobs().submit(
            &Submission::new("collect", serde_json::json!({}), "k1"),
            &restricted
        ),
        Err(Error::NotPermitted(_))
    ));
    restricted.write = Some(only(G));
    ds.jobs()
        .submit(
            &Submission::new("collect", serde_json::json!({}), "k1"),
            &restricted,
        )
        .unwrap();
}

#[test]
fn an_expired_lease_and_an_overdue_retry_end_at_the_horizon() {
    let ds = Dataset::memory();
    let mut s = settings();
    s.definitions.get_mut("collect").unwrap().retry_horizon_ms = 1_500;
    ds.jobs().configure(&s).unwrap();
    let w = worker("w");
    // a transient failure waits within the horizon, and a claim after the horizon ends
    // the run instead of taking it
    submit(&ds, "k1");
    let c = claim(&ds, &w);
    let f = Failure {
        transient: true,
        reason: String::new(),
        retry_after_ms: None,
    };
    let view = ds.jobs().fail(c.run, &c.token, &f, None, &w).unwrap();
    assert_eq!(view.state, State::RetryWait);
    force_eligible(&ds, c.run);
    std::thread::sleep(Duration::from_millis(1_600));
    assert!(ds.jobs().claim(&[], &w).unwrap().is_none());
    let view = ds.jobs().get(c.run, &admin()).unwrap().unwrap();
    assert_eq!(view.state, State::Failed);
    assert!(view.reason.unwrap().contains("horizon"));
    // an expired lease past the horizon ends the run too
    submit(&ds, "k2");
    let c2 = claim(&ds, &w);
    std::thread::sleep(Duration::from_millis(1_600));
    expire(&ds, c2.run);
    assert!(ds.jobs().claim(&[], &w).unwrap().is_none());
    let view = ds.jobs().get(c2.run, &admin()).unwrap().unwrap();
    assert_eq!(view.state, State::Failed);
    assert_eq!(view.attempt.unwrap().state, ledger::AttemptState::Expired);
}

#[test]
fn a_requested_delay_past_the_horizon_fails_instead_of_scheduling_in_the_past() {
    let ds = Dataset::memory();
    configured(&ds);
    let w = worker("w");
    for delay in [
        u64::MAX,
        i64::MAX as u64 + 1,
        i64::MAX as u64,
        48 * 3_600_000,
    ] {
        let s = submit(&ds, &format!("k-{delay}"));
        let c = claim(&ds, &w);
        assert_eq!(c.run, s.run);
        let f = Failure {
            transient: true,
            reason: "slow".into(),
            retry_after_ms: Some(delay),
        };
        let view = ds.jobs().fail(c.run, &c.token, &f, None, &w).unwrap();
        assert_eq!(view.state, State::Failed, "delay {delay}");
        assert!(view.next_eligible_ms.is_none());
        assert!(view.reason.unwrap().contains("passes the retry horizon"));
    }
    // a delay within the horizon is honored
    let s = submit(&ds, "k-fine");
    let c = claim(&ds, &w);
    assert_eq!(c.run, s.run);
    let f = Failure {
        transient: true,
        reason: String::new(),
        retry_after_ms: Some(3_600_000),
    };
    let view = ds.jobs().fail(c.run, &c.token, &f, None, &w).unwrap();
    assert_eq!(view.state, State::RetryWait);
    assert!(view.next_eligible_ms.unwrap() >= ledger::now_ms() + 3_599_000);
}

#[test]
fn a_run_header_carries_no_payload_or_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
        .unwrap();
    configured(&ds);
    let mut sub = Submission::new("collect", serde_json::json!({"big": "x"}), "k1");
    sub.checkpoint = Some("c".repeat(1024));
    sub.inputs.push(crate::branch::CommitRef {
        branch_id: ds.store().branch_id(),
        seq: ds.store().head_commit().seq,
    });
    let s = ds.jobs().submit(&sub, &admin()).unwrap();
    let snap = ds.store().snapshot();
    let prep =
        crate::access::Preparation::new(crate::task::Control::none(), ledger::READS_BYTES).unwrap();
    let mut l = ledger::Ledger::new(&prep, &snap);
    let header = l.run_header(s.run).unwrap().unwrap();
    assert!(header.payload.is_empty());
    assert!(header.inputs.is_empty());
    assert!(header.checkpoint.is_none());
    assert_eq!(header.state, State::Queued);
    let full = l.run(s.run).unwrap().unwrap();
    assert_eq!(full.payload, r#"{"big":"x"}"#);
    assert_eq!(full.inputs.len(), 1);
    assert_eq!(full.checkpoint.as_deref().map(str::len), Some(1024));
    assert_eq!(full.payload_digest, header.payload_digest);
}

/// A commit guard that refuses the first commit it checks with a conflict.
struct RefuseOnce(std::sync::atomic::AtomicBool);

impl crate::guard::CommitGuard for RefuseOnce {
    fn check(&self, _: &crate::guard::Candidate<'_>) -> Result<crate::guard::ValidationSummary> {
        if self.0.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::Conflict("refused once".into()));
        }
        Ok(crate::guard::ValidationSummary::empty(
            crate::guard::GuardStatus::Passed,
            crate::guard::GuardMode::Reject,
            crate::guard::Severity::Violation,
        ))
    }
    fn describe(&self) -> String {
        "refuse once".into()
    }
}

/// A commit guard that unwinds.
struct PanicGuard;

impl crate::guard::CommitGuard for PanicGuard {
    fn check(&self, _: &crate::guard::Candidate<'_>) -> Result<crate::guard::ValidationSummary> {
        panic!("the guard unwinds")
    }
    fn describe(&self) -> String {
        "panic".into()
    }
}

fn refuse_once() -> Arc<dyn crate::guard::CommitGuard> {
    Arc::new(RefuseOnce(std::sync::atomic::AtomicBool::new(true)))
}

/// A commit guard that refuses every commit with the error `refusal` makes.
struct RefuseWith(fn() -> Error);

impl crate::guard::CommitGuard for RefuseWith {
    fn check(&self, _: &crate::guard::Candidate<'_>) -> Result<crate::guard::ValidationSummary> {
        Err((self.0)())
    }
    fn describe(&self) -> String {
        "refuse with".into()
    }
}

/// `f` on a new thread, whose next renewal or failure of `ds` pauses after its commit
/// returned, when the writer is released and before the runtime sees the outcome, with
/// the channel that says it paused and the one that resumes it.
fn paused<T: Send + 'static>(
    ds: &Dataset,
    f: impl FnOnce() -> T + Send + 'static,
) -> (
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<T>,
) {
    let (ready, ready_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume) = std::sync::mpsc::channel::<()>();
    *ds.jobs().runtime().after_commit.lock() = Some(Box::new(move || {
        let _ = ready.send(());
        let _ = resume.recv_timeout(Duration::from_secs(30));
    }));
    (ready_rx, resume_tx, std::thread::spawn(f))
}

fn lease_ms(ds: &Dataset, ms: u64) {
    let mut s = settings();
    s.definitions.get_mut("collect").unwrap().lease_ms = ms;
    ds.jobs().configure(&s).unwrap();
}

#[test]
fn a_refused_renewal_paused_after_the_writer_does_not_extend_the_lease() {
    let ds = Dataset::memory();
    // long enough that a loaded machine reaches the renewal's commit within the lease
    lease_ms(&ds, 1500);
    submit(&ds, "k");
    let c = claim(&ds, &worker("w"));
    ds.store().set_guard(Some(refuse_once()));
    let (d, run, token) = (ds.clone(), c.run, c.token.clone());
    let (ready, resume, h) = paused(&ds, move || {
        d.jobs()
            .renew(run, &token, Some(60_000), None, &worker("w"))
    });
    if ready.recv_timeout(Duration::from_secs(30)).is_err() {
        panic!("the renewal did not reach its commit: {:?}", h.join());
    }
    // past the committed deadline, while the refused renewal has not returned
    std::thread::sleep(Duration::from_millis(1600));
    let published = ds.jobs().publish(
        c.run,
        &c.token,
        &publication(Uuid::new_v4(), 1),
        &worker("w"),
    );
    resume.send(()).unwrap();
    assert!(h.join().unwrap().is_err());
    ds.store().set_guard(None);
    assert!(
        code(published).contains("lease-lost"),
        "a refused renewal authorized a publication past the committed deadline"
    );
}

#[test]
fn two_refused_renewals_do_not_revive_an_abandoned_deadline() {
    let ds = Dataset::memory();
    lease_ms(&ds, 1500);
    submit(&ds, "k");
    let c = claim(&ds, &worker("w"));
    let renew = |ds: &Dataset| {
        let (d, run, token) = (ds.clone(), c.run, c.token.clone());
        paused(ds, move || {
            d.jobs()
                .renew(run, &token, Some(60_000), None, &worker("w"))
        })
    };
    ds.store().set_guard(Some(refuse_once()));
    let (a_ready, a_resume, a) = renew(&ds);
    if a_ready.recv_timeout(Duration::from_secs(30)).is_err() {
        panic!("renewal a did not reach its commit: {:?}", a.join());
    }
    ds.store().set_guard(Some(refuse_once()));
    let (b_ready, b_resume, b) = renew(&ds);
    if b_ready.recv_timeout(Duration::from_secs(30)).is_err() {
        panic!("renewal b did not reach its commit: {:?}", b.join());
    }
    a_resume.send(()).unwrap();
    assert!(a.join().unwrap().is_err());
    b_resume.send(()).unwrap();
    assert!(b.join().unwrap().is_err());
    ds.store().set_guard(None);
    std::thread::sleep(Duration::from_millis(1600));
    assert!(
        code(ds.jobs().renew(c.run, &c.token, None, None, &worker("w"))).contains("lease-lost"),
        "two refused renewals left an extension of the expired lease"
    );
}

#[test]
fn a_refused_or_unwinding_lease_commit_leaves_the_deadlines_of_committed_leases() {
    // the runtime by itself: a deadline counts only at the version it was recorded
    // under, a grant that is not kept removes only its own entry, and a kept one drops
    // the older versions
    let rt = Runtime::default();
    let run = Uuid::new_v4();
    let a = rt.grant(run, 2, 60_000);
    assert!(
        rt.lease_expired(run, 1),
        "a deadline of an uncommitted version"
    );
    drop(a);
    let b = rt.grant(run, 2, 60_000);
    b.keep();
    assert!(!rt.lease_expired(run, 2));
    let c = rt.grant(run, 3, 120_000);
    drop(c);
    assert!(!rt.lease_expired(run, 2), "c's rollback left b's deadline");
    assert!(rt.lease_expired(run, 3));
    // two pending grants of one version: the later replaces the earlier, and neither
    // rollback revives anything
    let d = rt.grant(run, 3, 120_000);
    let e = rt.grant(run, 3, 120_000);
    drop(d);
    drop(e);
    assert!(rt.lease_expired(run, 3));
    assert!(!rt.lease_expired(run, 2));
    let f = rt.grant(run, 3, 60_000);
    f.keep();
    assert!(
        rt.lease_expired(run, 2),
        "a kept grant drops the older versions"
    );
    let g = rt.grant(Uuid::new_v4(), 1, 60_000);
    let other = g.run;
    drop(g);
    assert!(
        rt.lease_expired(other, 1),
        "a fresh grant that is not kept is gone"
    );
    // a claim whose commit is refused leaves no deadline, and the next claim takes
    // the run with a live lease
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k1");
    ds.store().set_guard(Some(refuse_once()));
    assert!(matches!(
        ds.jobs().claim(&[], &worker("a")),
        Err(Error::Conflict(_))
    ));
    ds.store().set_guard(None);
    let cb = claim(&ds, &worker("b"));
    assert_eq!(cb.generation, 1);
    ds.jobs()
        .renew(cb.run, &cb.token, None, None, &worker("b"))
        .unwrap();
    // a renewal that unwinds does not extend the lease
    let ds = Dataset::memory();
    let mut s = settings();
    s.definitions.get_mut("collect").unwrap().lease_ms = 200;
    ds.jobs().configure(&s).unwrap();
    submit(&ds, "k1");
    let w = worker("w");
    let c = claim(&ds, &w);
    ds.store().set_guard(Some(Arc::new(PanicGuard)));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ds.jobs().renew(c.run, &c.token, Some(60_000), None, &w)
    }));
    assert!(r.is_err());
    ds.store().set_guard(None);
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        code(ds.jobs().renew(c.run, &c.token, None, None, &w)),
        "lease-lost",
        "the uncommitted extension did not survive"
    );
    // and a refused renewal keeps the previous deadline
    submit(&ds, "k2");
    let c2 = claim(&ds, &w);
    ds.store().set_guard(Some(refuse_once()));
    assert!(matches!(
        ds.jobs().renew(c2.run, &c2.token, Some(60_000), None, &w),
        Err(Error::Conflict(_))
    ));
    ds.store().set_guard(None);
    ds.jobs().renew(c2.run, &c2.token, None, None, &w).unwrap();
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(
        code(ds.jobs().renew(c2.run, &c2.token, None, None, &w)),
        "lease-lost"
    );
}

#[test]
fn an_expired_run_keeps_the_retry_policy_it_was_accepted_with() {
    let ds = Dataset::memory();
    let mut original = settings();
    original.definitions.get_mut("collect").unwrap().workers = vec![principal("w").as_str().into()];
    ds.jobs().configure(&original).unwrap();
    let r = submit(&ds, "k1");
    let w = worker("w");
    let c = claim(&ds, &w);
    assert_eq!(c.run, r.run);
    // the definition now allows one attempt, which the accepted run does not adopt
    let mut edited = original.clone();
    edited.definitions.get_mut("collect").unwrap().attempts = 1;
    ds.jobs().configure(&edited).unwrap();
    expire(&ds, r.run);
    assert!(
        ds.jobs()
            .claim(&["other".into()], &worker("unadmitted"))
            .unwrap()
            .is_none()
    );
    let view = ds.jobs().get(r.run, &admin()).unwrap().unwrap();
    assert_eq!(view.state, State::RetryWait, "{view:?}");
    assert_eq!(view.attempts, 1);
    // a removed definition leaves the run waiting too, and restoring it lets it go on
    let mut removed = original.clone();
    removed.definitions.clear();
    ds.jobs().configure(&removed).unwrap();
    force_eligible(&ds, r.run);
    assert!(ds.jobs().claim(&[], &w).unwrap().is_none());
    assert_eq!(
        ds.jobs().get(r.run, &admin()).unwrap().unwrap().state,
        State::RetryWait
    );
    ds.jobs().configure(&original).unwrap();
    let c2 = claim(&ds, &w);
    assert_eq!(c2.run, r.run);
    assert_eq!(c2.number, 2);
    // a restart under a removed definition waits as well
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.jobs().configure(&original).unwrap();
    let r = submit(&ds, "k1");
    claim(&ds, &w);
    ds.jobs().configure(&removed).unwrap();
    drop(ds);
    let ds = Dataset::open(dir.path()).unwrap();
    assert_eq!(ds.jobs().recover().unwrap().interrupted, vec![r.run]);
    assert_eq!(
        ds.jobs().get(r.run, &admin()).unwrap().unwrap().state,
        State::RetryWait
    );
}

#[test]
fn a_refused_submission_commit_releases_its_hold() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
        .unwrap();
    configured(&ds);
    let mut sub = Submission::new("collect", serde_json::json!({}), "k1");
    sub.inputs.push(crate::branch::CommitRef {
        branch_id: ds.store().branch_id(),
        seq: ds.store().head_commit().seq,
    });
    ds.store().set_guard(Some(refuse_once()));
    assert!(matches!(
        ds.jobs().submit(&sub, &admin()),
        Err(Error::Conflict(_))
    ));
    ds.store().set_guard(None);
    assert!(ds.store().source_holds().unwrap().is_empty());
    assert!(
        ds.jobs()
            .list(&ListFilter::default(), &admin())
            .unwrap()
            .is_empty()
    );
    let s = ds.jobs().submit(&sub, &admin()).unwrap();
    assert_eq!(ds.store().source_holds().unwrap()[0].id, s.run);
}

#[test]
fn a_storage_or_managed_refusal_of_a_submission_releases_its_hold() {
    let refusals: [fn() -> Error; 2] = [
        || Error::StorageFull("the reserve would be exceeded".into()),
        || {
            crate::managed::ManagedError::error(
                crate::managed::ManagedErrorKind::Protected,
                "a managed graph",
            )
        },
    ];
    for (i, refusal) in refusals.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
            .unwrap();
        configured(&ds);
        let mut sub = Submission::new("collect", serde_json::json!({}), "k1");
        sub.inputs.push(crate::branch::CommitRef {
            branch_id: ds.store().branch_id(),
            seq: ds.store().head_commit().seq,
        });
        ds.store().set_guard(Some(Arc::new(RefuseWith(refusal))));
        let r = ds.jobs().submit(&sub, &admin());
        assert!(r.is_err(), "refusal {i}");
        ds.store().set_guard(None);
        assert!(
            ds.store().source_holds().unwrap().is_empty(),
            "refusal {i} left the hold of {r:?}"
        );
    }
}

#[test]
fn inputs_are_held_until_the_run_is_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
        .unwrap();
    configured(&ds);
    let head = ds.store().head_commit().seq;
    let mut sub = Submission::new("collect", serde_json::json!({}), "k1");
    sub.inputs.push(crate::branch::CommitRef {
        branch_id: ds.store().branch_id(),
        seq: head,
    });
    let s = ds.jobs().submit(&sub, &admin()).unwrap();
    let holds = ds.store().source_holds().unwrap();
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].id, s.run);
    assert_eq!(holds[0].owner, OwnerKind::Jobs);
    assert!(
        ds.jobs().reconcile_holds(0).unwrap().is_empty(),
        "a live run keeps its hold"
    );
    let w = worker("w");
    let c = claim(&ds, &w);
    assert_eq!(c.inputs.len(), 1);
    ds.jobs()
        .publish(c.run, &c.token, &publication(Uuid::new_v4(), 1), &w)
        .unwrap();
    assert!(ds.store().source_holds().unwrap().is_empty());
}

#[test]
fn an_enablement_interrupted_after_the_reader_rose_reopens_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
        .unwrap();
    ds.store().require_dataset_reader(READER).unwrap();
    drop(ds);
    let ds = Dataset::open(dir.path()).unwrap();
    assert!(ds.ask("ASK { <urn:a> <urn:b> <urn:c> }").unwrap());
    configured(&ds);
    submit(&ds, "k1");
}

#[test]
fn the_dataset_reader_rises_to_nine_and_the_branch_handle_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    configured(&ds);
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("dataset.json")).unwrap()).unwrap();
    assert_eq!(file["minimumReader"], READER);
    assert!(dir.path().join("jobs.json").exists());
    ds.create_branch("dev", &Default::default()).unwrap();
    let dev = ds.branch("dev").unwrap();
    assert!(matches!(dev.jobs().settings(), Err(Error::Invalid(_))));
    assert!(matches!(submit_result(&dev, "k"), Err(Error::Invalid(_))));
}

#[cfg(feature = "failpoints")]
mod crashes {
    use super::*;
    use std::sync::Arc;

    fn crash<T>(ds: &Dataset, at: &'static str, f: impl FnOnce() -> Result<T>) {
        ds.store()
            .set_failpoint(at, Some(Arc::new(|_| panic!("crash"))));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        assert!(r.is_err(), "{at} was reached");
        ds.store().set_failpoint(at, None);
    }

    #[test]
    fn a2_a_submission_that_stops_before_its_commit_leaves_no_run() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        configured(&ds);
        crash(&ds, "jobs-before-commit", || submit_result(&ds, "k1"));
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        assert!(
            ds.jobs()
                .list(&ListFilter::default(), &admin())
                .unwrap()
                .is_empty()
        );
        let s = submit(&ds, "k1");
        assert!(!s.replayed);
    }

    #[test]
    fn a2_a_submission_that_stops_after_its_commit_is_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        configured(&ds);
        crash(&ds, "jobs-committed", || submit_result(&ds, "k1"));
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        let runs = ds.jobs().list(&ListFilter::default(), &admin()).unwrap();
        assert_eq!(runs.len(), 1);
        let s = submit(&ds, "k1");
        assert!(s.replayed);
        assert_eq!(s.run, runs[0].id);
    }

    #[test]
    fn a_crash_between_the_hold_and_the_commit_leaves_an_orphan_that_reconciles() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
            .unwrap();
        configured(&ds);
        let mut sub = Submission::new("collect", serde_json::json!({}), "k1");
        sub.inputs.push(crate::branch::CommitRef {
            branch_id: ds.store().branch_id(),
            seq: ds.store().head_commit().seq,
        });
        crash(&ds, "jobs-held", || ds.jobs().submit(&sub, &admin()));
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        assert_eq!(ds.store().source_holds().unwrap().len(), 1);
        assert!(ds.jobs().reconcile_holds(3_600_000).unwrap().is_empty());
        assert_eq!(ds.jobs().reconcile_holds(0).unwrap().len(), 1);
        assert!(ds.store().source_holds().unwrap().is_empty());
    }

    fn held_submission(ds: &Dataset, key: &str) -> Submission {
        let mut sub = Submission::new("collect", serde_json::json!({}), key);
        sub.inputs.push(crate::branch::CommitRef {
            branch_id: ds.store().branch_id(),
            seq: ds.store().head_commit().seq,
        });
        sub
    }

    /// Run `f` once, the first time the failpoint `at` is reached.
    fn once_at(ds: &Dataset, at: &'static str, f: impl Fn() + Send + Sync + 'static) {
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        ds.store().set_failpoint(
            at,
            Some(Arc::new(move |_| {
                if !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    f();
                }
            })),
        );
    }

    #[test]
    fn a_restart_keeps_the_source_hold_of_a_retryable_run_and_releases_an_ended_one() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
            .unwrap();
        configured(&ds);
        let s = ds
            .jobs()
            .submit(&held_submission(&ds, "k1"), &admin())
            .unwrap();
        let w = worker("w");
        let c = claim(&ds, &w);
        assert_eq!(c.run, s.run);
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        let recovered = ds.jobs().recover().unwrap();
        assert_eq!(recovered.interrupted, vec![s.run]);
        let view = ds.jobs().get(s.run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::RetryWait);
        let holds = ds.store().source_holds().unwrap();
        assert_eq!(holds.len(), 1, "a run that will retry keeps its inputs");
        assert_eq!(holds[0].id, s.run);
        // the next attempt still reads the pinned inputs
        force_eligible(&ds, s.run);
        let c2 = claim(&ds, &w);
        assert_eq!(c2.inputs.len(), 1);
        // a run whose attempts ran out at a claim releases its hold
        let mut s2 = settings();
        s2.definitions.get_mut("collect").unwrap().attempts = 2;
        ds.jobs().configure(&s2).unwrap();
        let cancelled = ds.jobs().cancel(s.run, &admin()).unwrap();
        assert_eq!(cancelled.state, State::Cancelled);
        assert!(ds.store().source_holds().unwrap().is_empty());
        let s3 = ds
            .jobs()
            .submit(&held_submission(&ds, "k3"), &admin())
            .unwrap();
        let c3 = claim(&ds, &w);
        assert_eq!(c3.run, s3.run);
        expire(&ds, c3.run);
        assert!(ds.jobs().claim(&[], &w).unwrap().is_none());
        assert_eq!(ds.store().source_holds().unwrap().len(), 1);
        force_eligible(&ds, s3.run);
        let c4 = claim(&ds, &w);
        assert_eq!(c4.number, 2);
        expire(&ds, c4.run);
        assert!(ds.jobs().claim(&[], &w).unwrap().is_none());
        let view = ds.jobs().get(s3.run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::Failed);
        assert!(view.reason.unwrap().contains("exhausted"));
        assert!(ds.store().source_holds().unwrap().is_empty());
        // a restart that ends a run releases its hold too
        let mut s4 = settings();
        s4.definitions.get_mut("collect").unwrap().attempts = 1;
        ds.jobs().configure(&s4).unwrap();
        let s5 = ds
            .jobs()
            .submit(&held_submission(&ds, "k5"), &admin())
            .unwrap();
        claim(&ds, &w);
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        assert_eq!(ds.jobs().recover().unwrap().interrupted, vec![s5.run]);
        assert_eq!(
            ds.jobs().get(s5.run, &admin()).unwrap().unwrap().state,
            State::Failed
        );
        assert!(ds.store().source_holds().unwrap().is_empty());
    }

    #[test]
    fn reconciliation_cannot_drop_the_hold_of_a_submission_that_commits() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
            .unwrap();
        configured(&ds);
        // A reconciliation that runs between the hold and the commit, with no grace,
        // sees no run and releases the hold. The commit then finds its hold gone and
        // refuses, so no run exists without its inputs pinned.
        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        {
            let ds = ds.clone();
            let seen = seen.clone();
            once_at(&ds.clone(), "jobs-before-commit", move || {
                seen.lock()
                    .push(ds.jobs().reconcile_holds(0).unwrap().len());
            });
        }
        let r = ds.jobs().submit(&held_submission(&ds, "k1"), &admin());
        assert_eq!(code(r), "hold-conflict");
        assert_eq!(*seen.lock(), vec![1]);
        assert!(ds.store().source_holds().unwrap().is_empty());
        assert!(
            ds.jobs()
                .list(&ListFilter::default(), &admin())
                .unwrap()
                .is_empty()
        );
        ds.store().set_failpoint("jobs-before-commit", None);
        // A reconciliation right after the commit reads the committed run and keeps the
        // hold.
        {
            let ds = ds.clone();
            let seen = seen.clone();
            once_at(&ds.clone(), "jobs-committed", move || {
                seen.lock()
                    .push(ds.jobs().reconcile_holds(0).unwrap().len());
            });
        }
        let s = ds
            .jobs()
            .submit(&held_submission(&ds, "k1"), &admin())
            .unwrap();
        assert_eq!(*seen.lock(), vec![1, 0]);
        let holds = ds.store().source_holds().unwrap();
        assert_eq!(holds.len(), 1);
        assert_eq!(holds[0].id, s.run);
        ds.store().set_failpoint("jobs-committed", None);
    }

    #[test]
    fn a_submission_is_revalidated_against_the_settings_admitted_under_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        ds.update("INSERT DATA { <urn:a> <urn:b> <urn:c> }")
            .unwrap();
        configured(&ds);
        // the submitter is revoked after the request was checked and before the writer
        {
            let ds = ds.clone();
            once_at(&ds.clone(), "jobs-held", move || {
                let mut s = settings();
                s.definitions.get_mut("collect").unwrap().submitters =
                    vec![principal("someone-else").as_str().into()];
                ds.jobs().configure(&s).unwrap();
            });
        }
        let r = ds.jobs().submit(&held_submission(&ds, "k1"), &admin());
        assert!(matches!(r, Err(Error::Conflict(_))), "{r:?}");
        assert!(
            ds.jobs()
                .list(&ListFilter::default(), &admin())
                .unwrap()
                .is_empty()
        );
        assert!(
            ds.store().source_holds().unwrap().is_empty(),
            "a refused submission leaves no hold"
        );
        ds.store().set_failpoint("jobs-held", None);
        assert!(matches!(
            ds.jobs().submit(&held_submission(&ds, "k1"), &admin()),
            Err(Error::NotPermitted(_))
        ));
    }

    #[test]
    fn disabling_jobs_keeps_the_ledger_protected_under_a_run_that_slipped_in() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        configured(&ds);
        let nonce = |ds: &Dataset| {
            ds.store()
                .managed_snapshot()
                .unwrap()
                .unwrap()
                .graphs
                .owners[&OwnerKind::Jobs]
                .protections[LEDGER_GRAPH]
        };
        let before = nonce(&ds);
        // a submission commits after the check of active runs and before the registry
        // change that disables jobs
        let slipped = Arc::new(parking_lot::Mutex::new(None));
        {
            let ds = ds.clone();
            let slipped = slipped.clone();
            once_at(&ds.clone(), "jobs-before-disable", move || {
                *slipped.lock() = Some(submit(&ds, "k1").run);
            });
        }
        let mut off = settings();
        off.enabled = false;
        let status = ds.jobs().configure(&off).unwrap();
        ds.store().set_failpoint("jobs-before-disable", None);
        let run = slipped.lock().unwrap();
        assert!(!status.settings.enabled);
        assert!(status.protected, "disabling never lifts the write fence");
        assert_eq!(nonce(&ds), before, "the nonce is kept");
        assert_eq!(
            ds.store().hidden_graphs(),
            vec![LEDGER_GRAPH.to_string()],
            "and the ledger stays hidden"
        );
        assert_eq!(
            code(ds.update(&format!(
                "INSERT DATA {{ GRAPH <{LEDGER_GRAPH}> {{ <urn:x> <urn:y> <urn:z> }} }}"
            ))),
            "profile-managed-graph"
        );
        // nothing is admitted or claimed while jobs are disabled, and the run waits
        assert_eq!(code(submit_result(&ds, "k2")), "unsupported-profile");
        assert_eq!(
            code(ds.jobs().claim(&[], &worker("w"))),
            "unsupported-profile"
        );
        // enabling again dispatches the run under the same protection
        configured(&ds);
        assert_eq!(nonce(&ds), before);
        let view = ds.jobs().get(run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::Queued);
        let c = claim(&ds, &worker("w"));
        assert_eq!(c.run, run);
    }

    #[test]
    fn a3_a7_a_publication_that_stops_around_its_commit_is_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        configured(&ds);
        submit(&ds, "k1");
        let w = worker("w");
        let c = claim(&ds, &w);
        let op = Uuid::new_v4();
        crash(&ds, "jobs-before-publish-commit", || {
            ds.jobs().publish(c.run, &c.token, &publication(op, 1), &w)
        });
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        assert!(!has(&ds, 1));
        let view = ds.jobs().get(c.run, &admin()).unwrap().unwrap();
        assert_eq!(
            view.state,
            State::Leased,
            "nothing of the publication is visible"
        );
        assert!(view.publication.is_none());
        // A5: the new process fences the old token before any claim
        let recovered = ds.jobs().recover().unwrap();
        assert!(recovered.installed);
        assert_eq!(recovered.interrupted, vec![c.run]);
        assert_eq!(
            code(ds.jobs().publish(c.run, &c.token, &publication(op, 1), &w)),
            "lease-lost"
        );
        let view = ds.jobs().get(c.run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::RetryWait);
        assert_eq!(
            view.attempt.unwrap().state,
            ledger::AttemptState::Interrupted
        );
        force_eligible(&ds, c.run);
        let c2 = claim(&ds, &w);
        assert_eq!(c2.generation, 2);
        // the retry publishes under the same operation id and stops after the commit
        crash(&ds, "jobs-published", || {
            ds.jobs()
                .publish(c2.run, &c2.token, &publication(op, 1), &w)
        });
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        assert!(has(&ds, 1));
        let view = ds.jobs().get(c2.run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::Succeeded);
        assert_eq!(view.checkpoint.as_deref(), Some("cursor-1"));
        // the lost response resolves by publication id, with any token state
        let again = ds
            .jobs()
            .publish(c2.run, &c2.token, &publication(op, 1), &w)
            .unwrap();
        assert!(again.replayed);
        assert_eq!(again.commit, view.publication.unwrap().commit);
        assert_eq!(
            code(
                ds.jobs()
                    .publish(c2.run, &c2.token, &publication(op, 2), &w)
            ),
            "publication-conflict"
        );
    }

    #[test]
    fn a_claim_that_stops_before_its_commit_leaves_the_run_queued() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        configured(&ds);
        let s = submit(&ds, "k1");
        let w = worker("w");
        crash(&ds, "jobs-before-claim-commit", || ds.jobs().claim(&[], &w));
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        let view = ds.jobs().get(s.run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::Queued);
        assert_eq!(view.attempts, 0);
        assert_eq!(view.generation, 0);
        assert!(view.attempt.is_none());
        let c = claim(&ds, &w);
        assert_eq!(c.run, s.run);
        assert_eq!(c.generation, 1);
        assert_eq!(c.number, 1);
    }

    #[test]
    fn a_claim_that_stops_after_its_commit_holds_a_lease_the_restart_interrupts() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        configured(&ds);
        let s = submit(&ds, "k1");
        let w = worker("w");
        crash(&ds, "jobs-claim-committed", || ds.jobs().claim(&[], &w));
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        let view = ds.jobs().get(s.run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::Leased, "the claim committed");
        assert_eq!(view.attempts, 1);
        assert_eq!(view.generation, 1);
        let recovered = ds.jobs().recover().unwrap();
        assert_eq!(recovered.interrupted, vec![s.run]);
        let view = ds.jobs().get(s.run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::RetryWait);
        assert_eq!(
            view.attempt.unwrap().state,
            ledger::AttemptState::Interrupted
        );
    }

    #[test]
    fn a5_a_restart_fences_the_old_token_before_a_new_claim() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        configured(&ds);
        submit(&ds, "k1");
        let w = worker("w");
        let c = claim(&ds, &w);
        drop(ds);
        let ds = Dataset::open(dir.path()).unwrap();
        // the first operation of the new process installs its epoch
        assert_eq!(
            code(ds.jobs().renew(c.run, &c.token, None, None, &w)),
            "lease-lost"
        );
        let view = ds.jobs().get(c.run, &admin()).unwrap().unwrap();
        assert_eq!(view.state, State::RetryWait);
        assert!(
            !ds.jobs().recover().unwrap().installed,
            "installed once per open"
        );
        force_eligible(&ds, c.run);
        let c2 = claim(&ds, &w);
        assert_eq!(c2.generation, 2);
        assert_eq!(
            code(ds.jobs().renew(c.run, &c.token, None, None, &w)),
            "lease-lost"
        );
        ds.jobs().renew(c2.run, &c2.token, None, None, &w).unwrap();
    }
}

#[test]
fn a_failed_attempt_keeps_the_lease_of_the_claim_that_follows_it() {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k");
    let first = claim(&ds, &worker("w"));
    let (d, token, run) = (ds.clone(), first.token.clone(), first.run);
    // the failure stops after its commit has released the writer, before its cleanup
    let (ready_rx, resume, h) = paused(&ds, move || {
        d.jobs().fail(
            run,
            &token,
            &Failure {
                transient: true,
                reason: "retry".into(),
                retry_after_ms: None,
            },
            None,
            &worker("w"),
        )
    });
    if let Err(e) = ready_rx.recv_timeout(Duration::from_secs(30)) {
        panic!("the failure did not commit ({e}): {:?}", h.join());
    }
    force_eligible(&ds, run);
    let next = claim(&ds, &worker("w"));
    assert_eq!((next.run, next.generation), (run, first.generation + 1));
    resume.send(()).unwrap();
    h.join().unwrap().unwrap();
    let renewed = ds
        .jobs()
        .renew(next.run, &next.token, None, None, &worker("w"));
    assert!(
        renewed.is_ok(),
        "the failed attempt's cleanup ended the later lease: {renewed:?}"
    );
}

#[test]
fn constraints_answer_the_ledger_as_a_graph_that_does_not_exist() {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k");
    for view in [None, Some(Arc::new(crate::access::GraphAccess::all()))] {
        let req = crate::handles::ConstraintsRequest {
            shapes: crate::handles::ShapesRequest {
                guard: Some(false),
                graphs: vec![LEDGER_GRAPH.into()],
            },
            graphs: view.clone(),
            ..Default::default()
        };
        let r = ds.schema().constraints(&req);
        assert!(matches!(r, Err(Error::NotFound(_))), "{view:?}: {r:?}");
        let snap = ds.snapshot();
        let r = ds
            .schema()
            .constraints_at(&snap, &req.shapes, view.as_deref());
        assert!(matches!(r, Err(Error::NotFound(_))), "{view:?}: {r:?}");
    }
}

/// A quad of the ledger graph written under the jobs capability, as an owner would.
fn ledger_cell(value: &str) -> Quad {
    Quad::new(
        NamedNode::new_unchecked("urn:hidden-subject"),
        NamedNode::new_unchecked("urn:cell"),
        Literal::new_simple_literal(value),
        GraphName::NamedNode(NamedNode::new_unchecked(LEDGER_GRAPH)),
    )
}

fn set_ledger_cell(ds: &Dataset, old: Option<&str>, new: &str) {
    let cap = ds
        .store()
        .managed_writer(OwnerKind::Jobs, Uuid::new_v4())
        .unwrap();
    let mut tx = ds.store().write();
    tx.set_managed(cap).unwrap();
    let mut labels = Default::default();
    if let Some(v) = old {
        let ids = tx.encode_quad(&ledger_cell(v), &mut labels).unwrap();
        tx.delete(ids).unwrap();
    }
    let ids = tx.encode_quad(&ledger_cell(new), &mut labels).unwrap();
    tx.insert(ids).unwrap();
    tx.commit().unwrap();
}

/// A dataset with jobs whose ledger cell conflicts between `main` and branch `work`,
/// with a public conflict too when `public`.
fn conflicting(public: bool) -> Dataset {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k");
    set_ledger_cell(&ds, None, "base");
    if public {
        ds.update("INSERT DATA { <urn:s> <urn:p> <urn:base> }")
            .unwrap();
    }
    ds.create_branch("work", &Default::default()).unwrap();
    let work = ds.branch("work").unwrap();
    set_ledger_cell(&ds, Some("base"), "ours");
    set_ledger_cell(&work, Some("base"), "theirs");
    if public {
        ds.update("DELETE DATA { <urn:s> <urn:p> <urn:base> }; INSERT DATA { <urn:s> <urn:p> <urn:ours> }")
            .unwrap();
        work.update("DELETE DATA { <urn:s> <urn:p> <urn:base> }; INSERT DATA { <urn:s> <urn:p> <urn:theirs> }")
            .unwrap();
    }
    ds
}

#[test]
fn resolved_ledger_conflicts_leave_the_merge_counters() {
    use crate::branch::{MergeOptions, Resolution, Take};
    // a rule for every conflict
    let o = MergeOptions {
        on_conflict: Some(Take::Ours),
        ..Default::default()
    };
    let r = conflicting(false)
        .preview_merge("work", "main", &o)
        .unwrap();
    assert_eq!((r.conflicts_found, r.conflicts_resolved), (0, 0), "{r:?}");
    let r = conflicting(true).preview_merge("work", "main", &o).unwrap();
    assert_eq!((r.conflicts_found, r.conflicts_resolved), (1, 1), "{r:?}");
    // an explicit resolution of the ledger cell
    let o = MergeOptions {
        resolutions: vec![Resolution {
            graph: GraphName::NamedNode(NamedNode::new_unchecked(LEDGER_GRAPH)),
            subject: Some(NamedNode::new_unchecked("urn:hidden-subject").into()),
            predicate: Some(NamedNode::new_unchecked("urn:cell")),
            take: Take::Ours,
        }],
        ..Default::default()
    };
    let r = conflicting(false)
        .preview_merge("work", "main", &o)
        .unwrap();
    assert_eq!((r.conflicts_found, r.conflicts_resolved), (0, 0), "{r:?}");
    // the public conflict remains and is shown, the resolved ledger one is not counted
    let r = conflicting(true).preview_merge("work", "main", &o).unwrap();
    assert_eq!((r.conflicts_found, r.conflicts_resolved), (1, 0), "{r:?}");
    assert_eq!(r.conflicts.as_ref().map(|c| c.conflicts), Some(1), "{r:?}");
}

#[test]
fn unresolved_ledger_only_conflicts_are_refused_as_managed() {
    let ds = conflicting(false);
    for r in [
        ds.preview_merge("work", "main", &Default::default())
            .map(|_| ()),
        ds.merge("work", "main", &Default::default()).map(|_| ()),
    ] {
        assert_eq!(r.unwrap_err().code(), "profile-managed-graph");
    }
}

#[test]
fn merges_without_hidden_graphs_count_and_resolve_every_conflict() {
    use crate::branch::{MergeOptions, Take};
    let ds = Dataset::memory();
    ds.update("INSERT DATA { <urn:s> <urn:p> <urn:base> }")
        .unwrap();
    ds.create_branch("work", &Default::default()).unwrap();
    let work = ds.branch("work").unwrap();
    ds.update(
        "DELETE DATA { <urn:s> <urn:p> <urn:base> }; INSERT DATA { <urn:s> <urn:p> <urn:ours> }",
    )
    .unwrap();
    work.update(
        "DELETE DATA { <urn:s> <urn:p> <urn:base> }; INSERT DATA { <urn:s> <urn:p> <urn:theirs> }",
    )
    .unwrap();
    let r = ds
        .preview_merge("work", "main", &Default::default())
        .unwrap();
    assert_eq!(r.conflicts_found, 1);
    assert_eq!(r.conflicts.unwrap().conflicts, 1);
    let o = MergeOptions {
        on_conflict: Some(Take::Theirs),
        ..Default::default()
    };
    let r = ds.preview_merge("work", "main", &o).unwrap();
    assert_eq!(
        (
            r.inserted,
            r.deleted,
            r.conflicts_found,
            r.conflicts_resolved
        ),
        (1, 1, 1, 1)
    );
    ds.merge("work", "main", &o).unwrap();
    assert!(ds.ask("ASK { <urn:s> <urn:p> <urn:theirs> }").unwrap());
}

#[test]
fn a_shared_schema_report_never_holds_the_ledger() {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k");
    ds.update("INSERT DATA { <urn:public> a <urn:Public> }")
        .unwrap();
    let mut req = crate::handles::ReportRequest::default();
    req.options.graph = crate::schema::GraphSelection::Union;
    let first = ds.schema().report(&req).unwrap();
    let j = serde_json::to_string(first.report.as_ref()).unwrap();
    assert!(!j.contains("urn:x-sparkles:jobs:"), "{j}");
    let mut all = req.clone();
    all.options.graphs = Some(Arc::new(crate::access::GraphAccess::all()));
    let second = ds.schema().report(&all).unwrap();
    assert_eq!(second.computed, crate::handles::Computed::Cached);
    assert!(Arc::ptr_eq(&first.report, &second.report));
    claim(&ds, &admin());
    let third = ds.schema().report(&req).unwrap();
    let j = serde_json::to_string(third.report.as_ref()).unwrap();
    assert!(!j.contains("urn:x-sparkles:jobs:"), "{j}");
    assert_eq!(third.report.totals.triples, 1);
}

#[test]
fn live_queries_never_read_the_ledger() {
    for access in [None, Some(Arc::new(crate::access::GraphAccess::all()))] {
        let ds = Dataset::memory();
        configured(&ds);
        submit(&ds, "k");
        let q = format!("SELECT ?s ?p ?o WHERE {{ GRAPH <{LEDGER_GRAPH}> {{ ?s ?p ?o }} }}");
        let live = ds.live_as(&q, access, &Default::default()).unwrap();
        match live
            .next_timeout(Duration::from_secs(10))
            .expect("a first event")
        {
            crate::incr::LiveEvent::Snapshot { rows, .. } => {
                assert!(
                    rows.is_empty(),
                    "{} ledger rows: {:?}",
                    rows.len(),
                    rows.first()
                )
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

#[test]
fn a_live_query_started_before_jobs_ends_when_they_are_enabled() {
    let ds = Dataset::memory();
    ds.update("INSERT DATA { <urn:public> <urn:p> <urn:o> }")
        .unwrap();
    let live = ds
        .live_as(
            "SELECT ?s ?p ?o WHERE { GRAPH ?g { ?s ?p ?o } }",
            None,
            &Default::default(),
        )
        .unwrap();
    assert!(matches!(
        live.next_timeout(Duration::from_secs(10)),
        Some(crate::incr::LiveEvent::Snapshot { .. })
    ));
    configured(&ds);
    submit(&ds, "k");
    // what its callers may read changed: the subscription ends instead of showing a
    // change of the ledger
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let e = live.next_timeout(Duration::from_secs(10));
        match e {
            Some(crate::incr::LiveEvent::Error { code, .. }) => {
                assert_eq!(code, "forbidden");
                break;
            }
            Some(crate::incr::LiveEvent::Snapshot { rows, .. }) => {
                let j = format!("{rows:?}");
                assert!(!j.contains("urn:x-sparkles:jobs:"), "{j}");
            }
            Some(other) => {
                let j = format!("{other:?}");
                assert!(!j.contains("urn:x-sparkles:jobs:"), "{j}");
            }
            None => panic!("the subscription neither ended nor changed"),
        }
        assert!(Instant::now() < deadline, "the subscription did not end");
    }
}

#[test]
fn kept_views_never_read_the_ledger() {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k");
    let q = format!("SELECT ?s ?p ?o WHERE {{ GRAPH <{LEDGER_GRAPH}> {{ ?s ?p ?o }} }}");
    ds.views()
        .put("ledger", crate::incr::ViewDef::table(q))
        .unwrap();
    ds.views()
        .wait_built("ledger", Duration::from_secs(10))
        .unwrap();
    let rows = ds.views().read("ledger").unwrap();
    assert!(rows.rows.is_empty(), "{:?}", rows.rows.first());
}

#[test]
fn predicate_distinct_counts_leave_out_the_deleted_hidden_base_values() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    configured(&ds);
    submit(&ds, "k");
    ds.update("INSERT DATA { <urn:public> <urn:x-sparkles:jobs:version> 100 }")
        .unwrap();
    ds.compact().unwrap();
    let counts = |ds: &Dataset| {
        let s = ds.stats(&Default::default()).unwrap();
        let p = s
            .predicates
            .iter()
            .find(|p| p.iri == "urn:x-sparkles:jobs:version")
            .unwrap()
            .clone();
        (p.count, p.distinct_subjects, p.distinct_objects)
    };
    assert_eq!(counts(&ds), (1, 1, 1));
    // the claim replaces the run's version in the ledger's base
    claim(&ds, &admin());
    assert_eq!(counts(&ds), (1, 1, 1));
}

#[cfg(feature = "ontology")]
#[test]
fn an_ontology_opened_before_jobs_are_enabled_is_not_reused_after() {
    use crate::handles::ontology::OntologyRequest;
    use crate::reasoning::reasoner::inputs::GraphRef;
    let ds = Dataset::memory();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{LEDGER_GRAPH}> {{ <urn:SecretClass> a <http://www.w3.org/2002/07/owl#Class> }} }}"
    ))
    .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::Named(LEDGER_GRAPH.into())],
        ..Default::default()
    };
    let snap = ds.snapshot();
    let first = ds
        .ontology()
        .open_at(&snap, &req, &Default::default())
        .unwrap();
    assert_eq!(first.summary().triples, 1);
    configured(&ds);
    // the configuration writes no RDF commit, so only the hidden graphs changed
    assert_eq!(ds.snapshot().commit, snap.commit);
    let after = ds
        .ontology()
        .open_at(&ds.snapshot(), &req, &Default::default())
        .unwrap();
    assert_eq!(after.summary().triples, 0, "cached: {}", after.cached);
}

/// A dataset with jobs enabled whose guard names shapes kept in the ledger graph. Neither
/// the guard configuration nor enablement admits that, so the configuration file is
/// written by hand and the dataset opened again, as a damaged or edited directory would
/// be.
#[cfg(feature = "shacl")]
fn ledger_shapes() -> (tempfile::TempDir, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    {
        let ds = Dataset::open(dir.path()).unwrap();
        ds.update(&format!(
            "PREFIX sh: <http://www.w3.org/ns/shacl#> INSERT DATA {{ <urn:person> a <urn:Public> . GRAPH <{LEDGER_GRAPH}> {{ <urn:SecretShape> a sh:NodeShape ; sh:targetClass <urn:Public> ; sh:property <urn:SecretPropertyShape> . <urn:SecretPropertyShape> sh:path <urn:secret-path> ; sh:minCount 1 }} }}"
        ))
        .unwrap();
        configured(&ds);
    }
    std::fs::write(
        dir.path().join(crate::guard::config::CONFIG_FILE),
        serde_json::to_vec(&serde_json::json!({"format": 2, "language": "shacl",
            "mode": "warn", "dataGraph": "default", "shapes": {"graphs": [LEDGER_GRAPH]}}))
        .unwrap(),
    )
    .unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    assert!(ds.validation().guard().get().is_some());
    (dir, ds)
}

#[cfg(feature = "shacl")]
#[test]
fn constraints_never_take_a_guard_whose_shapes_are_in_the_ledger() {
    let (_dir, ds) = ledger_shapes();
    let mut req = crate::handles::ConstraintsRequest::default();
    req.shapes.guard = Some(true);
    let r = ds.schema().constraints(&req);
    assert!(matches!(r, Err(Error::NotFound(_))), "{r:?}");
    // without a required guard, the layer has no source
    req.shapes.guard = None;
    let layer = ds.schema().constraints(&req).unwrap();
    assert!(!format!("{layer:?}").contains("secret-path"), "{layer:?}");
}

#[cfg(all(feature = "graphql", feature = "shacl"))]
#[test]
fn a_graphql_draft_never_takes_guard_shapes_from_the_ledger() {
    let (_dir, ds) = ledger_shapes();
    let r = ds.graphql().draft("d", Default::default());
    if let Ok((draft, _)) = r {
        let j = serde_json::to_string(&draft).unwrap();
        assert!(
            !j.contains("urn:SecretShape") && !j.contains("urn:secret-path"),
            "{j}"
        );
    }
}

#[test]
fn a_public_kept_view_stays_readable_on_a_dataset_with_jobs() {
    let ds = Dataset::memory();
    configured(&ds);
    submit(&ds, "k");
    ds.update("INSERT DATA { <urn:public> <urn:p> <urn:o> }")
        .unwrap();
    ds.views()
        .put(
            "public",
            crate::incr::ViewDef::table("SELECT ?s WHERE { ?s <urn:p> <urn:o> }"),
        )
        .unwrap();
    ds.views()
        .wait_built("public", Duration::from_secs(10))
        .unwrap();
    let q = "PREFIX view: <urn:x-sparkles:view#> SELECT ?s WHERE { SERVICE view:read { [] view:name \"public\" ; view:bind ?s } }";
    // as a caller who reads every visible graph, and as one with no view of its own
    for graphs in [None, Some(Arc::new(crate::access::GraphAccess::all()))] {
        let opts = crate::sparql::QueryOptions {
            graphs,
            ..Default::default()
        };
        if let Err(e) = ds.query_with(q, &opts) {
            panic!("{e}");
        }
    }
}
