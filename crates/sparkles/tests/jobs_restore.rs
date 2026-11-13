//! Restored and cloned job ledgers (spec C26 §7.3): A10, A21, A24 and A25.
#![cfg(feature = "backup")]

use oxrdf::NamedNode;
use sparkles::backup::{CreateOptions, RepoConfig, RepoType, RestoreRequest};
use sparkles::jobs::{Caller, Definition, RecoveryPhase, Settings, State, Submission};
use sparkles::{Catalog, Dataset};

fn admin() -> Caller {
    Caller::admin(NamedNode::new_unchecked("urn:x-sparkles:principal:ops"))
}

fn worker() -> Caller {
    Caller::user(NamedNode::new_unchecked("urn:x-sparkles:principal:w"))
}

fn enable(ds: &Dataset) {
    let mut s = Settings {
        enabled: true,
        ..Default::default()
    };
    let mut d = Definition::quads(vec!["urn:g".into()]);
    d.concurrency = 16;
    s.definitions.insert("collect".into(), d);
    ds.jobs().configure(&s).unwrap();
}

fn submit(ds: &Dataset, key: &str) -> uuid::Uuid {
    ds.jobs()
        .submit(
            &Submission::new("collect", serde_json::json!({ "k": key }), key),
            &admin(),
        )
        .unwrap()
        .run
}

fn state(ds: &Dataset, run: uuid::Uuid) -> State {
    ds.jobs().get(run, &admin()).unwrap().unwrap().state
}

fn code<T: std::fmt::Debug>(r: sparkles::Result<T>) -> String {
    match r {
        Err(e) => e.code().to_string(),
        Ok(v) => panic!("succeeded: {v:?}"),
    }
}

/// A source with a queued run, a leased one and a terminal one, backed up, and its
/// restore as `copy`.
fn restored() -> (
    tempfile::TempDir,
    Catalog,
    Dataset,
    Dataset,
    Vec<uuid::Uuid>,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path().join("data"), Default::default()).unwrap();
    let repos = cat.repositories().unwrap();
    repos
        .add(RepoConfig {
            name: "local".into(),
            kind: RepoType::Fs,
            path: Some(dir.path().join("repo").display().to_string()),
            ..Default::default()
        })
        .unwrap();
    let repo = repos.open("local").unwrap();
    let ds = cat.create("src", &Default::default()).unwrap();
    ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    enable(&ds);
    let queued = submit(&ds, "q");
    let leased = submit(&ds, "l");
    let failed = submit(&ds, "f");
    // the earliest submission is claimed first
    let c = ds.jobs().claim(&[], &worker()).unwrap().unwrap();
    assert_eq!(c.run, queued);
    let c2 = ds.jobs().claim(&[], &worker()).unwrap().unwrap();
    ds.jobs().cancel(c2.run, &admin()).unwrap();
    let _ = (leased, failed);
    ds.backups(&repo)
        .create_with(
            &CreateOptions {
                name: "b".into(),
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    let copy = cat
        .restore(
            &repo,
            "b",
            &RestoreRequest {
                target: Some("copy".into()),
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    (dir, cat, ds, copy, vec![queued, leased, failed], c.token)
}

/// A10, A21 and A25: restored work is paused before the epoch is installed, waits for
/// the recovery route, and a retry of a member joins the set paused.
#[test]
fn restored_work_waits_paused_for_the_recovery_route() {
    let (_dir, _cat, _src, copy, runs, token) = restored();
    let (leased_at_capture, queued_at_capture, cancelled) = (runs[0], runs[2], runs[1]);
    let recovered = copy.jobs().recover().unwrap();
    assert!(recovered.installed);
    for run in [leased_at_capture, queued_at_capture] {
        assert_eq!(state(&copy, run), State::Paused, "{run}");
    }
    assert_eq!(state(&copy, cancelled), State::Cancelled);
    // nothing restored is claimed, and the old token fails
    assert!(copy.jobs().claim(&[], &worker()).unwrap().is_none());
    assert!(
        code(
            copy.jobs()
                .renew(leased_at_capture, &token, None, None, &worker())
        )
        .contains("lease-lost")
    );
    // a new submission is dispatched as usual
    let fresh = submit(&copy, "new");
    assert_eq!(
        copy.jobs().claim(&[], &worker()).unwrap().unwrap().run,
        fresh
    );
    // A25: a retry of a terminal member is paused and joins the set
    let retried = copy.jobs().retry(cancelled, "again", &admin()).unwrap();
    assert_eq!(retried.state, State::Paused);
    // a paused member is cancelled meanwhile, and its retry is paused too
    copy.jobs().cancel(queued_at_capture, &admin()).unwrap();
    let retried2 = copy
        .jobs()
        .retry(queued_at_capture, "again2", &admin())
        .unwrap();
    assert_eq!(retried2.state, State::Paused);
    // only an administrator resumes, and only paused members
    assert!(copy.jobs().resume(&[leased_at_capture], &worker()).is_err());
    assert!(copy.jobs().resume(&[fresh], &admin()).is_err());
    let status = copy
        .jobs()
        .resume(&[leased_at_capture, retried.run], &admin())
        .unwrap();
    let rec = status.recovery.unwrap();
    assert_eq!(rec.phase, RecoveryPhase::AwaitingDecision);
    assert_eq!((rec.paused, rec.ended), (1, false));
    assert_eq!(state(&copy, leased_at_capture), State::Queued);
    let status = copy.jobs().resume(&[retried2.run], &admin()).unwrap();
    assert!(status.recovery.unwrap().ended);
    // the recovery stays once nothing is paused, so a later retry of a restored run
    // that ended still waits for a decision
    let rec = copy.jobs().resume(&[], &admin()).unwrap().recovery.unwrap();
    assert_eq!((rec.paused, rec.ended), (0, true));
    let late = copy.jobs().retry(cancelled, "again3", &admin()).unwrap();
    assert_eq!(late.state, State::Paused);
    copy.jobs().resume(&[late.run], &admin()).unwrap();
    // the resumed runs are claimed again
    let mut claimed = Vec::new();
    while let Some(c) = copy.jobs().claim(&[], &worker()).unwrap() {
        claimed.push(c.run);
    }
    claimed.sort();
    let mut expected = vec![leased_at_capture, retried.run, retried2.run, late.run];
    expected.sort();
    assert_eq!(claimed, expected);
}

/// A21: the recovery continues after a reopen, and the origin fences every transition
/// until it runs.
#[test]
fn a_reopened_restore_continues_its_recovery() {
    let (dir, cat, _src, copy, runs, _) = restored();
    let path = copy.store().root().unwrap().to_path_buf();
    drop(copy);
    drop(cat);
    let _ = dir;
    let copy = Dataset::open(&path).unwrap();
    let r = copy.jobs().get(runs[0], &admin()).unwrap().unwrap();
    assert_eq!(
        r.state,
        State::Leased,
        "nothing changes before the recovery runs"
    );
    copy.jobs().recover().unwrap();
    assert_eq!(state(&copy, runs[0]), State::Paused);
    drop(copy);
    // A24: once the origin is consumed, a lost control log is an integrity error
    std::fs::remove_file(path.join("control.log")).unwrap();
    assert!(Dataset::open(&path).is_err());
}

/// A10: a clone holds no ledger, and its copied settings need reader 9.
#[test]
fn a_clone_starts_with_an_empty_ledger() {
    let (dir, _cat, src, _copy, _runs, _) = restored();
    let to = dir.path().join("clone");
    src.store().clone_to(&to, &Default::default()).unwrap();
    let clone = Dataset::open(&to).unwrap();
    assert!(clone.store().control_enabled());
    assert!(
        clone
            .jobs()
            .list(&Default::default(), &admin())
            .unwrap()
            .is_empty()
    );
    assert!(clone.jobs().recover().unwrap().interrupted.is_empty());
}

/// A21: the recovery route refuses a request that finds the restored work still to be
/// paused, and admits the next one once the pausing ended.
#[test]
fn the_recovery_route_refuses_until_the_restored_work_is_paused() {
    let (_dir, _cat, _src, copy, runs, _) = restored();
    assert_eq!(code(copy.jobs().resume(&[], &admin())), "jobs-recovering");
    assert_eq!(state(&copy, runs[0]), State::Paused);
    let rec = copy.jobs().resume(&[], &admin()).unwrap().recovery.unwrap();
    assert_eq!(rec.phase, RecoveryPhase::AwaitingDecision);
    copy.jobs().resume(&[runs[0]], &admin()).unwrap();
    assert_eq!(state(&copy, runs[0]), State::Queued);
}

/// A backup of a restored dataset whose runs still await a decision restores with those
/// runs in the new recovery, which resumes them.
#[test]
fn a_restore_of_a_restore_adopts_the_runs_it_holds_paused() {
    let (_dir, cat, _src, copy, runs, _) = restored();
    copy.jobs().recover().unwrap();
    for run in [runs[0], runs[2]] {
        assert_eq!(state(&copy, run), State::Paused);
    }
    let repo = cat.repositories().unwrap().open("local").unwrap();
    copy.backups(&repo)
        .create_with(
            &CreateOptions {
                name: "b2".into(),
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    let again = cat
        .restore(
            &repo,
            "b2",
            &RestoreRequest {
                target: Some("again".into()),
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    again.jobs().recover().unwrap();
    let status = again.jobs().resume(&[runs[0], runs[2]], &admin()).unwrap();
    assert_eq!(status.resumed.len(), 2);
    for run in [runs[0], runs[2]] {
        assert_eq!(state(&again, run), State::Queued);
    }
}
