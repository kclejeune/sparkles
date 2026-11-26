//! Regression tests for the shared evidence binding codec (`sparkles::evidence::binding`,
//! spec C25, roadmap item 509).

use serde_json::{Value, json};
use sparkles::evidence::binding;
use sparkles::task::Control;
use sparkles::{Dataset, Error};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const SOURCES_G: &str = "https://example.org/g/sources";
const CLAIMS_G: &str = "https://example.org/g/claims";
const DECISIONS_G: &str = "https://example.org/g/decisions";
const SUPPORTS_G: &str = "https://example.org/g/supports";
const IDENTITY_G: &str = "https://example.org/g/identity";
const ACCEPTED_G: &str = "https://example.org/g/accepted";
const MAPS_G: &str = "https://example.org/g/maps";
const MAP_OUT_G: &str = "https://example.org/g/map-out";

fn settings_bytes() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "format": 1,
        "profile": 1,
        "enabled": true,
        "sourceRecords": [SOURCES_G],
        "proposals": [CLAIMS_G],
        "decisions": [DECISIONS_G],
        "supports": [SUPPORTS_G],
        "identity": [IDENTITY_G, MAPS_G],
        "projections": [ACCEPTED_G, MAP_OUT_G],
        "policies": {
            "review-v1": {
                "version": 1,
                "admittedKinds": ["reported"],
                "requiredScope": [],
                "conflicts": "admitLabelled",
                "boundary": [SOURCES_G, CLAIMS_G],
                "decisions": DECISIONS_G,
                "supports": SUPPORTS_G,
                "output": ACCEPTED_G,
            }
        },
        "matchingPolicies": {
            "match-v1": {
                "version": 1,
                "boundary": [IDENTITY_G],
                "decisions": DECISIONS_G,
                "maps": MAPS_G,
                "output": MAP_OUT_G,
            }
        }
    }))
    .unwrap()
}

fn claim_proposal_bytes(source: &str, occ: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "revisions": [{
            "key": "rev1",
            "source": source,
            "sourceKey": occ,
            "content": { "text": "Release 1 is affected by CVE-1." },
        }],
        "claims": [{
            "key": "c1",
            "occurrenceKey": occ,
            "revision": { "key": "rev1" },
            "proposition": "<https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1>",
            "kind": "reported",
            "polarity": "positive",
            "author": "https://example.org/org/a",
        }],
    }))
    .unwrap()
}

/// Regression for F2 (Sol P1 #2): hold listing reads only the branch table and never takes
/// writer locks, so it does not deadlock when a write transaction is open on a persistent store.
#[test]
fn holds_does_not_deadlock_during_open_write_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    ds.update("INSERT DATA { <urn:s> <urn:p> '1' }").unwrap();
    ds.create_branch("work", &Default::default()).unwrap();

    ds.transaction(|_tx| {
        let page = binding::holds(&ds, None, None, Some(10))?;
        assert_eq!(page["heldCommits"], 0);
        assert_eq!(page["holds"].as_array().unwrap().len(), 0);
        let filtered = binding::holds(&ds, Some("work"), None, Some(10))?;
        assert_eq!(filtered["holds"].as_array().unwrap().len(), 0);
        Ok(())
    })
    .unwrap();
}

/// Regression for F2 (Sol P1 #2): acquiring a hold succeeds and returns its generated hold ID
/// and branch name even when an unrelated branch's on-disk descriptor is corrupt.
#[test]
fn acquire_hold_ignores_unrelated_unreadable_branch_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let ds = Dataset::open(&root).unwrap();
    ds.update("INSERT DATA { <urn:s> <urn:p> '1' }").unwrap();
    let good = ds.create_branch("good", &Default::default()).unwrap();
    let bad = ds.create_branch("bad", &Default::default()).unwrap();

    // Corrupt the unrelated branch's branch.json descriptor on disk so opening it would fail.
    let bad_file = root
        .join("branches")
        .join(bad.id.to_string())
        .join("branch.json");
    std::fs::write(&bad_file, b"{not valid json").unwrap();

    let req = serde_json::to_vec(&json!({
        "commits": [{ "branch": "good", "commit": good.head.unwrap().seq }]
    }))
    .unwrap();
    let acquired = binding::acquire_hold_bytes(&ds, &req).unwrap();
    assert_eq!(acquired["created"], true);
    assert!(acquired["hold"]["id"].as_str().is_some());
    assert_eq!(acquired["hold"]["commits"][0]["branch"], "good");
    assert_eq!(
        acquired["hold"]["commits"][0]["branchId"],
        good.id.to_string()
    );
}

/// Regression for F3 (Sol P2 #3): configure honors cancellation/deadline while waiting for
/// the main writer lock and publishes nothing when cancelled before acquisition.
#[test]
fn configure_cancelled_while_main_writer_held_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();

    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder_ds = ds.clone();
    let holder = std::thread::spawn(move || {
        holder_ds
            .transaction(|_tx| {
                locked_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
    });
    locked_rx.recv().unwrap();

    let ctl = Control {
        deadline: Some(Instant::now() + Duration::from_millis(80)),
        ..Control::with_cancel(Arc::new(AtomicBool::new(false)))
    };
    let err = binding::configure(&ds, &settings_bytes(), &ctl).unwrap_err();
    assert!(
        matches!(err, Error::Timeout),
        "expected Timeout, got {err:?}"
    );

    release_tx.send(()).unwrap();
    holder.join().unwrap();

    // Nothing was published.
    let status = binding::settings(&ds).unwrap();
    assert_eq!(status["settings"]["enabled"], false);
    assert_eq!(status["revision"], 0);
}

/// Regression for F3 (Sol P2 #3): once configure has published the registry, a branch writer
/// delaying the post-commit family barrier past the deadline still lets the barrier complete
/// and returns the published status instead of failing as a timeout.
#[test]
fn configure_preserves_published_status_when_branch_delays_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    ds.create_branch("work", &Default::default()).unwrap();
    let br = ds.branch("work").unwrap();

    let (locked_tx, locked_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        br.transaction(|_tx| {
            locked_tx.send(()).unwrap();
            // Hold the branch transaction longer than configure's deadline so the barrier waits
            // past the deadline, then release cleanly.
            std::thread::sleep(Duration::from_millis(200));
            Ok(())
        })
        .unwrap();
    });
    locked_rx.recv().unwrap();

    let ctl = Control {
        deadline: Some(Instant::now() + Duration::from_millis(60)),
        ..Control::with_cancel(Arc::new(AtomicBool::new(false)))
    };
    let status = binding::configure(&ds, &settings_bytes(), &ctl).unwrap();
    holder.join().unwrap();
    assert_eq!(status["settings"]["enabled"], true);
    assert_eq!(status["revision"], 1);
}

/// Regression for F9 (Neon P2-2): all thirteen evidence binding entry points reject a branch
/// Dataset handle uniformly with Invalid (including disabled-default configure).
#[test]
fn all_evidence_binding_ops_reject_branch_handles() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    ds.create_branch("feat", &Default::default()).unwrap();
    let br = ds.branch("feat").unwrap();
    let ctl = Control::none();

    let disabled_defaults = serde_json::to_vec(&sparkles::evidence::Settings::default()).unwrap();
    let results: Vec<(&str, sparkles::Result<Value>)> = vec![
        ("settings", binding::settings(&br)),
        (
            "configure-defaults",
            binding::configure(&br, &disabled_defaults, &ctl),
        ),
        (
            "configure-enabled",
            binding::configure(&br, &settings_bytes(), &ctl),
        ),
        ("detach", binding::detach(&br, ACCEPTED_G)),
        ("holds", binding::holds(&br, None, None, None)),
        (
            "acquire_hold",
            binding::acquire_hold_bytes(&br, br#"{"commits":[{"commit":0}]}"#),
        ),
        (
            "release_hold",
            binding::release_hold(&br, &uuid::Uuid::new_v4().to_string()).map(Value::Bool),
        ),
        ("resolve", binding::resolve_bytes(&br, None, b"[]", &ctl)),
        (
            "propose",
            binding::propose_bytes(&br, None, None, None, b"{}", &ctl),
        ),
        (
            "review",
            binding::review_bytes(
                &br,
                None,
                None,
                None,
                br#"{"policy":"review-v1","policyVersion":1,"ifHead":0,"decisions":[]}"#,
                &ctl,
            ),
        ),
        (
            "withdraw",
            binding::withdraw_bytes(&br, None, None, None, br#"{"withdrawals":[]}"#, &ctl),
        ),
        (
            "claim_states",
            binding::claim_states(&br, None, None, "review-v1", &[], &ctl),
        ),
        (
            "resolve_entities",
            binding::resolve_entities(
                &br,
                None,
                "match-v1",
                &["https://example.org/e/a".into()],
                &ctl,
            ),
        ),
        (
            "find_dependents",
            binding::find_dependents(&br, None, &["https://example.org/c/1".into()], false, &ctl),
        ),
    ];
    for (name, res) in results {
        let err = res.expect_err(&format!("{name} must fail on a branch handle"));
        assert!(
            matches!(err, Error::Invalid(_)),
            "{name} expected Error::Invalid, got {err:?}"
        );
    }
}

/// Regression for F4 (Sol P2 #4 + Neon P2-3): request byte limits, decoded base64 byte cap,
/// collection count bounds, and cancellation checkpoints reject before full decoding.
#[test]
fn request_bounds_base64_cap_and_cancellation_apply_before_conversion() {
    let ds = Dataset::memory();
    let ctl = Control::none();

    // 1. Pre-cancelled control refuses immediately on propose/review/withdraw/resolve/claim_states.
    let flag = Arc::new(AtomicBool::new(true));
    let cancelled = Control::with_cancel(flag);
    assert!(matches!(
        binding::resolve_bytes(&ds, None, b"[]", &cancelled),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        binding::propose_bytes(&ds, None, None, None, b"{}", &cancelled),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        binding::review_bytes(
            &ds,
            None,
            None,
            None,
            br#"{"policy":"review-v1","policyVersion":1,"ifHead":0,"decisions":[]}"#,
            &cancelled,
        ),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        binding::withdraw_bytes(&ds, None, None, None, br#"{"withdrawals":[]}"#, &cancelled),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        binding::claim_states(&ds, None, None, "review-v1", &[], &cancelled),
        Err(Error::Cancelled)
    ));

    // 2. Oversized base64 payload (> 16 MiB decoded, < 24 MiB raw JSON) fails with BudgetExceeded
    // before calling base64::decode (even with invalid base64 characters inside!).
    let over_16m_b64 = "!".repeat(22_400_000);
    let oversized_b64_req = format!(
        r#"{{"revisions":[{{"key":"r1","source":"https://example.org/s","sourceKey":"k","content":{{"base64":"{over_16m_b64}"}}}}]}}"#
    );
    assert!(oversized_b64_req.len() < binding::MAX_CLAIMS_BYTES);
    let err = binding::propose_bytes(&ds, None, None, None, oversized_b64_req.as_bytes(), &ctl)
        .unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(_)),
        "expected BudgetExceeded before base64 decode, got {err:?}"
    );

    // 3. Oversized review decisions array (> MAX_DECISIONS) fails with Invalid before iterating.
    let mut decisions = Vec::with_capacity(sparkles::evidence::MAX_DECISIONS + 1);
    for i in 0..=sparkles::evidence::MAX_DECISIONS {
        decisions.push(json!({
            "key": format!("d{i}"),
            "target": "not-even-an-iri",
            "targetDigest": "bad",
            "disposition": "invalid",
        }));
    }
    let review_req = serde_json::to_vec(&json!({
        "policy": "review-v1",
        "policyVersion": 1,
        "ifHead": 0,
        "decisions": decisions,
    }))
    .unwrap();
    let err = binding::review_bytes(&ds, None, None, None, &review_req, &ctl).unwrap_err();
    assert!(
        matches!(err, Error::Invalid(ref m) if m.contains("decisions")),
        "expected decisions count refusal, got {err:?}"
    );
}

/// Regression for F6 (Sol P2 #6): binding::claim_states reads unreviewed, accepted, withdrawn,
/// missing (`null`), `origin: "instructions"`, branch targeting, and historical `at` views.
#[test]
fn binding_claim_states_covers_live_and_historical_views() {
    let ds = Dataset::memory();
    let ctl = Control::none();
    binding::configure(&ds, &settings_bytes(), &ctl).unwrap();

    // Mark source as mem:Instructions in the default graph.
    ds.update("INSERT DATA { <https://example.org/src/inst> a <urn:x-sparkles:mem:Instructions> }")
        .unwrap();

    let proposed = binding::propose_bytes(
        &ds,
        None,
        None,
        None,
        &claim_proposal_bytes("https://example.org/src/inst", "occ-1"),
        &ctl,
    )
    .unwrap();
    let claim_rec = proposed["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "c1")
        .unwrap();
    let claim_iri = claim_rec["iri"].as_str().unwrap().to_string();
    let claim_digest = claim_rec["recordDigest"].as_str().unwrap().to_string();

    // Unreviewed live read (works without activated configuration history).
    let states = binding::claim_states(
        &ds,
        None,
        None,
        "review-v1",
        &[claim_iri.clone(), "https://example.org/missing".into()],
        &ctl,
    )
    .unwrap();
    assert_eq!(states["policy"], "review-v1");
    assert_eq!(states["claims"][0]["state"], "unreviewed");
    assert_eq!(states["claims"][0]["origin"], "instructions");
    assert_eq!(states["claims"][0]["kind"], "reported");
    assert_eq!(states["claims"][0]["polarity"], "positive");
    assert!(states["claims"][1].is_null());

    // Explicit historical `at` refuses with configuration-history-inactive when not activated.
    let err = binding::claim_states(
        &ds,
        None,
        Some("1"),
        "review-v1",
        std::slice::from_ref(&claim_iri),
        &ctl,
    )
    .unwrap_err();
    assert_eq!(err.code(), "configuration-history-inactive");

    // Review -> accepted, then withdraw -> withdrawn.
    let review_req = serde_json::to_vec(&json!({
        "policy": "review-v1",
        "policyVersion": 1,
        "ifHead": ds.head_commit().seq,
        "decisions": [{
            "key": "d1",
            "target": claim_iri,
            "targetDigest": claim_digest,
            "disposition": "accepted",
        }],
    }))
    .unwrap();
    let reviewed = binding::review_bytes(&ds, None, None, None, &review_req, &ctl).unwrap();
    let decision_iri = reviewed["decisions"][0]["iri"].as_str().unwrap();

    let accepted = binding::claim_states(
        &ds,
        None,
        None,
        "review-v1",
        std::slice::from_ref(&claim_iri),
        &ctl,
    )
    .unwrap();
    assert_eq!(accepted["claims"][0]["state"], "accepted");
    assert_eq!(accepted["claims"][0]["decision"], decision_iri);
    assert_eq!(accepted["claims"][0]["decisionVersion"], 1);

    let withdraw_req = serde_json::to_vec(&json!({
        "withdrawals": [{
            "key": "w1",
            "target": claim_iri,
            "targetDigest": claim_digest,
        }]
    }))
    .unwrap();
    binding::withdraw_bytes(&ds, None, None, None, &withdraw_req, &ctl).unwrap();
    let withdrawn = binding::claim_states(
        &ds,
        None,
        None,
        "review-v1",
        std::slice::from_ref(&claim_iri),
        &ctl,
    )
    .unwrap();
    assert_eq!(withdrawn["claims"][0]["state"], "withdrawn");

    // Activated memory dataset supports historical `at` reads.
    let ds2 = Dataset::memory();
    let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds2.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    binding::configure(&ds2, &settings_bytes(), &ctl).unwrap();
    let p2 = binding::propose_bytes(
        &ds2,
        None,
        None,
        None,
        &claim_proposal_bytes("https://example.org/src/plain", "occ-2"),
        &ctl,
    )
    .unwrap();
    let c2 = p2["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "c1")
        .unwrap();
    let c2_iri = c2["iri"].as_str().unwrap().to_string();
    let c2_digest = c2["recordDigest"].as_str().unwrap().to_string();
    let before_review_seq = ds2.head_commit().seq;
    ds2.snapshots()
        .create(
            "before-review",
            &sparkles::history::At::Head,
            &Default::default(),
        )
        .unwrap();
    let rev2 = serde_json::to_vec(&json!({
        "policy": "review-v1",
        "policyVersion": 1,
        "ifHead": before_review_seq,
        "decisions": [{
            "key": "d1",
            "target": c2_iri,
            "targetDigest": c2_digest,
            "disposition": "accepted",
        }],
    }))
    .unwrap();
    binding::review_bytes(&ds2, None, None, None, &rev2, &ctl).unwrap();

    let at_before = binding::claim_states(
        &ds2,
        None,
        Some(&before_review_seq.to_string()),
        "review-v1",
        std::slice::from_ref(&c2_iri),
        &ctl,
    )
    .unwrap();
    assert_eq!(at_before["commit"], before_review_seq);
    assert_eq!(at_before["claims"][0]["state"], "unreviewed");
    assert_eq!(at_before["claims"][0]["origin"], "source");

    let at_head = binding::claim_states(
        &ds2,
        None,
        None,
        "review-v1",
        std::slice::from_ref(&c2_iri),
        &ctl,
    )
    .unwrap();
    assert_eq!(at_head["claims"][0]["state"], "accepted");
}
