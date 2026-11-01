//! Evidence settings administration (spec C25 §7): configuration protects every role
//! graph, never weakens protection, detaches graphs explicitly, refuses stale
//! registry revisions and keeps `evidence.json` canonical and versioned.

use sparkles::Dataset;
use sparkles::evidence::{self, ClaimKind, ConflictMode, Policy, Settings};
use sparkles::managed::{OwnerEntry, OwnerKind};
use sparkles::task::Control;
use std::sync::{Arc, Barrier};

const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const DECISIONS: &str = "urn:ev:decisions";
const SUPPORTS: &str = "urn:ev:supports";
const ACCEPTED: &str = "urn:ev:accepted";

fn enabled() -> Settings {
    Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into()],
        ..Settings::default()
    }
}

fn reviewed() -> Settings {
    let mut s = enabled();
    s.decisions = vec![DECISIONS.into()];
    s.supports = vec![SUPPORTS.into()];
    s.projections = vec![ACCEPTED.into()];
    s.policies.insert(
        "advisories".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![CLAIMS.into(), SOURCES.into()],
            decisions: DECISIONS.into(),
            supports: SUPPORTS.into(),
            output: ACCEPTED.into(),
        },
    );
    s
}

/// Insert a triple not inserted before, so that the write always changes `graph`.
fn insert(ds: &Dataset, graph: &str) -> sparkles::Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{graph}> {{ <urn:s> <urn:p> {n} }} }}"
    ))
    .map(|_| ())
}

#[track_caller]
fn code(r: sparkles::Result<impl std::fmt::Debug>) -> &'static str {
    match r {
        Err(sparkles::Error::Managed(m)) => m.code,
        r => panic!("not a managed refusal: {r:?}"),
    }
}

#[test]
fn configuration_protects_every_role_and_round_trips() {
    let ds = Dataset::memory();
    let status = evidence::settings(&ds).unwrap();
    assert_eq!(status.settings, Settings::default());
    assert_eq!((status.digest, status.revision), (None, 0));
    insert(&ds, ACCEPTED).unwrap();

    let status = evidence::configure(&ds, &reviewed(), &Control::none()).unwrap();
    assert_eq!(status.settings, reviewed());
    assert_eq!(status.revision, 1);
    assert_eq!(
        status.managed,
        [ACCEPTED, CLAIMS, DECISIONS, SOURCES, SUPPORTS]
    );
    assert!(status.detached.is_empty());
    let canonical = reviewed().canonical(&Control::none()).unwrap();
    assert_eq!(
        status.digest.as_deref(),
        Some(sparkles::managed::settings_digest(canonical.bytes()).as_str())
    );
    for graph in [SOURCES, CLAIMS, DECISIONS, SUPPORTS, ACCEPTED] {
        assert_eq!(code(insert(&ds, graph)), "profile-managed-graph", "{graph}");
    }
    insert(&ds, "urn:ordinary").unwrap();
    // the same settings again: a new registry revision, the same protection
    let again = evidence::configure(&ds, &reviewed(), &Control::none()).unwrap();
    assert_eq!(again.revision, 2);
    assert_eq!(again.managed, status.managed);
}

#[test]
fn configuration_never_weakens_protection() {
    let ds = Dataset::memory();
    evidence::configure(&ds, &reviewed(), &Control::none()).unwrap();
    // dropping a protected graph from its role
    let mut fewer = reviewed();
    fewer.policies.clear();
    fewer.projections.clear();
    assert!(matches!(
        evidence::configure(&ds, &fewer, &Control::none()),
        Err(sparkles::Error::Conflict(_))
    ));
    // disabling while graphs are protected
    let mut off = reviewed();
    off.enabled = false;
    assert!(matches!(
        evidence::configure(&ds, &off, &Control::none()),
        Err(sparkles::Error::Conflict(_))
    ));
    assert_eq!(code(insert(&ds, ACCEPTED)), "profile-managed-graph");
    assert_eq!(evidence::settings(&ds).unwrap().revision, 1);

    // detach first, then the narrower settings are accepted
    let status = evidence::detach(&ds, ACCEPTED).unwrap();
    assert_eq!(status.detached, [ACCEPTED]);
    insert(&ds, ACCEPTED).unwrap();
    let status = evidence::configure(&ds, &fewer, &Control::none()).unwrap();
    assert!(status.detached.is_empty());
    assert!(!status.managed.contains(&ACCEPTED.to_string()));
    insert(&ds, ACCEPTED).unwrap();

    // configuring the role again protects the detached graph again
    let status = evidence::configure(&ds, &reviewed(), &Control::none()).unwrap();
    assert!(status.managed.contains(&ACCEPTED.to_string()));
    assert_eq!(code(insert(&ds, ACCEPTED)), "profile-managed-graph");

    // disabling after every graph is detached
    for graph in status.managed {
        evidence::detach(&ds, &graph).unwrap();
    }
    let status = evidence::configure(&ds, &off, &Control::none()).unwrap();
    assert!(!status.settings.enabled);
    assert!(status.managed.is_empty() && status.detached.is_empty());
    insert(&ds, SOURCES).unwrap();
}

#[test]
fn disabled_settings_of_a_dataset_never_configured() {
    let ds = Dataset::memory();
    let status = evidence::configure(&ds, &Settings::default(), &Control::none()).unwrap();
    assert_eq!(status.revision, 0);
    assert_eq!(ds.store().managed_graphs().unwrap(), None);
    let mut off = reviewed();
    off.enabled = false;
    assert!(matches!(
        evidence::configure(&ds, &off, &Control::none()),
        Err(sparkles::Error::Invalid(_))
    ));
    assert_eq!(ds.store().managed_graphs().unwrap(), None);
    insert(&ds, SOURCES).unwrap();
}

#[test]
fn invalid_settings_and_foreign_detach_write_nothing() {
    let ds = Dataset::memory();
    let mut bad = reviewed();
    bad.proposals.push(ACCEPTED.into());
    assert!(evidence::configure(&ds, &bad, &Control::none()).is_err());
    assert_eq!(ds.store().managed_graphs().unwrap(), None);
    evidence::configure(&ds, &enabled(), &Control::none()).unwrap();
    assert!(matches!(
        evidence::detach(&ds, "urn:not-managed"),
        Err(sparkles::Error::NotFound(_))
    ));
    let control = Control::none();
    control.cancel.cancel();
    assert!(matches!(
        evidence::configure(&ds, &reviewed(), &control),
        Err(sparkles::Error::Cancelled)
    ));
    assert_eq!(evidence::settings(&ds).unwrap().revision, 1);
}

#[test]
fn a_stale_registry_revision_is_refused() {
    let ds = Dataset::memory();
    evidence::configure(&ds, &enabled(), &Control::none()).unwrap();
    let bytes = enabled().canonical(&Control::none()).unwrap();
    let entry = || OwnerEntry::for_settings(1, enabled().managed_graphs(), bytes.bytes());
    assert!(matches!(
        ds.store()
            .enable_managed_at(OwnerKind::Evidence, entry(), bytes.bytes(), 0),
        Err(sparkles::Error::Conflict(_))
    ));
    ds.store()
        .enable_managed_at(OwnerKind::Evidence, entry(), bytes.bytes(), 1)
        .unwrap();
    assert_eq!(evidence::settings(&ds).unwrap().revision, 2);
}

/// Concurrent configurations either succeed one after another or refuse as stale, and
/// a detach that races with a configuration is never undone by it.
#[test]
fn concurrent_configuration_and_detach_keep_one_consistent_registry() {
    for _ in 0..20 {
        let ds = Dataset::memory();
        evidence::configure(&ds, &reviewed(), &Control::none()).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let ds = ds.clone();
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                evidence::configure(&ds, &reviewed(), &Control::none()).map(|s| s.revision)
            }));
        }
        let detacher = {
            let ds = ds.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                evidence::detach(&ds, ACCEPTED).map(|s| s.revision)
            })
        };
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        let detached = detacher.join().unwrap().unwrap();
        for r in &results {
            match r {
                Ok(_) | Err(sparkles::Error::Conflict(_)) => {}
                Err(e) => panic!("{e}"),
            }
        }
        let succeeded = results.iter().filter(|r| r.is_ok()).count() as u64;
        let status = evidence::settings(&ds).unwrap();
        // every successful change raised the revision exactly once
        assert_eq!(status.revision, 1 + succeeded + 1);
        // the accepted graph is protected only if a configuration ran after the detach
        let reattached = results
            .iter()
            .any(|r| matches!(r, Ok(revision) if *revision > detached));
        assert_eq!(status.managed.contains(&ACCEPTED.to_string()), reattached);
        assert_eq!(insert(&ds, ACCEPTED).is_ok(), !reattached);
    }
}

#[test]
fn persistent_settings_survive_reopen_and_unknown_versions_refuse_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ds");
    {
        let ds = Dataset::open(&path).unwrap();
        evidence::configure(&ds, &reviewed(), &Control::none()).unwrap();
    }
    let canonical = reviewed().canonical(&Control::none()).unwrap();
    assert_eq!(
        std::fs::read(path.join("evidence.json")).unwrap(),
        canonical.bytes()
    );
    {
        let ds = Dataset::open(&path).unwrap();
        let status = evidence::settings(&ds).unwrap();
        assert_eq!(status.settings, reviewed());
        assert_eq!(code(insert(&ds, ACCEPTED)), "profile-managed-graph");
    }
    // a settings file of a later profile, with its digest recorded consistently, keeps
    // reads and refuses every write
    let newer = br#"{"format":1,"profile":2,"enabled":true}"#;
    std::fs::write(path.join("evidence.json"), newer).unwrap();
    let managed = std::fs::read(path.join("managed.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_slice(&managed).unwrap();
    v["owners"]["evidence"]["settingsDigest"] = sparkles::managed::settings_digest(newer).into();
    std::fs::write(path.join("managed.json"), serde_json::to_vec(&v).unwrap()).unwrap();
    let ds = Dataset::open(&path).unwrap();
    assert_eq!(code(insert(&ds, "urn:ordinary")), "managed-unavailable");
    assert_eq!(code(evidence::settings(&ds)), "managed-unavailable");
    assert_eq!(
        code(evidence::configure(&ds, &reviewed(), &Control::none())),
        "managed-unavailable"
    );
}
