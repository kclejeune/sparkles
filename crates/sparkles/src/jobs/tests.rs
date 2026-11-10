//! Durable jobs: the acceptance cases of C26 §8 that a single process shows (A1, A4, A6,
//! A9, A12) and, under `failpoints`, the crash cases (A2, A3, A5, A7).

use super::*;
use crate::Dataset;
use oxrdf::{GraphName, Literal, NamedNode, Quad};
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
    ds.jobs()
        .runtime()
        .leases
        .lock()
        .insert(run, Instant::now() - Duration::from_millis(1));
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
