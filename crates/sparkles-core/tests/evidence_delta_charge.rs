//! Regression tests and diagnostics for shared live and cached delta charges
//! in bounded history reads and evidence resolution (roadmap item 542).

use oxrdf::{Literal, NamedNode, Term, Triple};
use sparkles_core::access::{Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules};
use sparkles_core::evidence::{
    GraphIdentity, RecordKind, Settings, Sha256Digest, StoredFactRef,
    resolve::{MEMORY_BYTES, RecordRequest, Resolver},
};
use sparkles_core::history::{At, HistoryOptions, SnapshotOptions};
use sparkles_core::id::Id;
use sparkles_core::managed::{OwnerEntry, OwnerKind};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::task::Control;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use uuid::Uuid;

const EV: &str = "urn:x-sparkles:evidence:";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn all() -> Arc<GraphAccess> {
    Arc::new(GraphAccess::all())
}

fn used(r: &Resolver) -> u64 {
    r.preparation()
        .limit()
        .saturating_sub(r.preparation().available_bytes())
}

fn fact(store: &Store, commit: u64, subject: &str, pred: &str, obj: &str) -> StoredFactRef {
    StoredFactRef {
        dataset: store.dataset_id(),
        branch: store.dataset_id(),
        commit,
        graph: GraphIdentity::Default,
        expected: Triple::new(iri(subject), iri(pred), Literal::new_simple_literal(obj)),
        anchor: None,
    }
}

/// Insert `count` quads (`<urn:s{start+i}> <urn:p> "v{start+i}"` with every
/// tenth quad using `<urn:secret>` when `with_secret` is true) into the store's
/// uncompacted delta in one transaction.
fn insert_delta_quads(store: &Store, start: usize, count: usize, with_secret: bool) -> u64 {
    let mut txn = store.write();
    let p_vis = txn.intern(&Term::NamedNode(iri("urn:p"))).unwrap();
    let p_sec = if with_secret {
        txn.intern(&Term::NamedNode(iri("urn:secret"))).unwrap()
    } else {
        p_vis
    };
    for i in start..start + count {
        let s = txn
            .intern(&Term::NamedNode(iri(&format!("urn:s{i}"))))
            .unwrap();
        let o = txn
            .intern(&Term::Literal(Literal::new_simple_literal(format!("v{i}"))))
            .unwrap();
        let p = if with_secret && i % 10 == 0 {
            p_sec
        } else {
            p_vis
        };
        txn.insert([s, p, o, Id::DEFAULT_GRAPH]).unwrap();
    }
    txn.commit().unwrap();
    store.head_commit().seq
}

/// Enable evidence settings on `store` and publish one authenticated
/// `ev:SourceRevision` record (`urn:rev`) through the managed writer.
fn enable_and_write_record(store: &Store) -> NamedNode {
    let settings = Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into()],
        ..Settings::default()
    };
    let canonical = settings.canonical(&Control::none()).unwrap();
    let bytes = canonical.bytes().to_vec();
    store
        .enable_managed(
            OwnerKind::Evidence,
            OwnerEntry::for_settings(1, settings.managed_graphs(), &bytes),
            &bytes,
        )
        .unwrap();
    let nonce = store
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections[SOURCES];
    let digest = Sha256Digest::of_bytes(b"payload", &Control::none())
        .unwrap()
        .to_string();
    let mut txn = store.write();
    txn.set_managed(
        store
            .managed_writer(OwnerKind::Evidence, Uuid::new_v4())
            .unwrap(),
    )
    .unwrap();
    let (seq, _) = txn.managed_stamp().unwrap();
    let g = txn.intern(&Term::NamedNode(iri(SOURCES))).unwrap();
    let rev = txn.intern(&Term::NamedNode(iri("urn:rev"))).unwrap();
    let op = txn.intern(&Term::NamedNode(iri("urn:op:1"))).unwrap();
    let rdf_type = txn.intern(&Term::NamedNode(iri(RDF_TYPE))).unwrap();
    let rev_cls = txn
        .intern(&Term::NamedNode(iri(&format!("{EV}SourceRevision"))))
        .unwrap();
    let op_cls = txn
        .intern(&Term::NamedNode(iri(&format!("{EV}Operation"))))
        .unwrap();
    let p_receipt = txn
        .intern(&Term::NamedNode(iri(&format!("{EV}availabilityReceipt"))))
        .unwrap();
    let p_digest = txn
        .intern(&Term::NamedNode(iri(&format!("{EV}contentDigest"))))
        .unwrap();
    let p_prot = txn
        .intern(&Term::NamedNode(iri(&format!("{EV}graphProtection"))))
        .unwrap();
    let p_ds = txn
        .intern(&Term::NamedNode(iri(&format!("{EV}datasetId"))))
        .unwrap();
    let p_br = txn
        .intern(&Term::NamedNode(iri(&format!("{EV}branchId"))))
        .unwrap();
    let p_cm = txn
        .intern(&Term::NamedNode(iri(&format!("{EV}commit"))))
        .unwrap();
    let lit_digest = txn
        .intern(&Term::Literal(Literal::new_simple_literal(digest)))
        .unwrap();
    let lit_nonce = txn
        .intern(&Term::Literal(Literal::new_simple_literal(
            nonce.to_string(),
        )))
        .unwrap();
    let lit_ds = txn
        .intern(&Term::Literal(Literal::new_simple_literal(
            store.dataset_id().to_string(),
        )))
        .unwrap();
    let lit_br = txn
        .intern(&Term::Literal(Literal::new_simple_literal(
            store.branch_id().to_string(),
        )))
        .unwrap();
    let lit_seq = txn
        .intern(&Term::Literal(Literal::new_typed_literal(
            seq.to_string(),
            iri("http://www.w3.org/2001/XMLSchema#integer"),
        )))
        .unwrap();
    for q in [
        [rev, rdf_type, rev_cls, g],
        [rev, p_digest, lit_digest, g],
        [rev, p_receipt, op, g],
        [rev, p_prot, lit_nonce, g],
        [op, rdf_type, op_cls, g],
        [op, p_ds, lit_ds, g],
        [op, p_br, lit_br, g],
        [op, p_cm, lit_seq, g],
        [op, p_prot, lit_nonce, g],
    ] {
        txn.insert(q).unwrap();
    }
    txn.commit().unwrap();
    iri("urn:rev")
}

fn secret_policy() -> Arc<GraphAccess> {
    Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: BTreeMap::new(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ))
}

#[test]
fn shared_live_state_with_large_uncompacted_delta_resolves_within_allowance() {
    for persistent in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = if persistent {
            Store::open(dir.path(), StoreOptions::default()).unwrap()
        } else {
            Store::in_memory(StoreOptions::default())
        };
        let rev_iri = enable_and_write_record(&store);
        // 50,000 uncompacted delta quads exceed 64 MiB under the old 2 KiB/quad
        // charge (50,000 * 2 KiB = 100 MiB), but the live snapshot is shared.
        let commit = insert_delta_quads(&store, 0, 50_000, false);
        assert!(store.snapshot().delta.inserts() >= 50_000);

        // Bounded history read at Head and at live commit succeed under 64 MiB.
        let (snap, resolved) = store
            .snapshot_at_limited(&At::Head, &HistoryOptions::default(), MEMORY_BYTES)
            .unwrap();
        assert_eq!(resolved.commit.seq, commit);
        assert_eq!(snap.commit, commit);

        // Stored-fact resolution on the shared live state succeeds under 64 MiB
        // and retains only a small per-request handle charge.
        let mut r = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
        let verified = r
            .stored_fact(
                "live-fact",
                &store,
                Some(&all()),
                &fact(&store, commit, "urn:s49999", "urn:p", "v49999"),
            )
            .unwrap();
        assert_eq!(verified.commit, commit);
        assert!(
            used(&r) <= 16 << 10,
            "shared live snapshot over-charged: used={} (persistent={persistent})",
            used(&r)
        );

        // Even a tight 64 KiB cap admits the shared live snapshot with graph-all access.
        let mut tight = Resolver::new(Control::none(), 64 << 10).unwrap();
        tight
            .stored_fact(
                "live-tight",
                &store,
                Some(&all()),
                &fact(&store, commit, "urn:s0", "urn:p", "v0"),
            )
            .unwrap();

        // Record resolution on the shared live state also succeeds under 64 MiB.
        let mut rr = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
        let rec = rr
            .record(
                "live-record",
                &store,
                &store,
                Some(&all()),
                &RecordRequest {
                    iri: rev_iri,
                    kinds: vec![RecordKind::SourceRevision],
                    content_digest: None,
                },
            )
            .unwrap();
        assert_eq!(rec.kind, RecordKind::SourceRevision);

        // Cancellation and expired deadlines still refuse immediately.
        let flag = Arc::new(AtomicBool::new(false));
        let mut cancelled =
            Resolver::new(Control::with_cancel(flag.clone()), MEMORY_BYTES).unwrap();
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(
            cancelled.stored_fact(
                "cancelled",
                &store,
                Some(&all()),
                &fact(&store, commit, "urn:s0", "urn:p", "v0"),
            ),
            Err(sparkles_core::Error::Cancelled)
        ));
        let expired_opts = HistoryOptions {
            cancel: None,
            deadline: Some(Instant::now() - Duration::from_millis(1)),
        };
        assert!(matches!(
            store.snapshot_at_limited(&At::Head, &expired_opts, MEMORY_BYTES),
            Err(sparkles_core::Error::Timeout)
        ));
    }
}

#[test]
fn shared_cached_historical_state_resolves_while_private_replay_still_refuses() {
    // 1. Persistent store: commit 1 has 50,000 uncompacted delta quads; commit 2
    //    advances head so commit 1 is historical.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let c1 = insert_delta_quads(&store, 0, 50_000, false);
    let c2 = insert_delta_quads(&store, 50_000, 1, false);
    assert_eq!(c2, c1 + 1);

    // Cold private historical replay of 50,000 delta quads must still refuse under 64 MiB.
    assert!(matches!(
        store.snapshot_at_limited(&At::Commit(c1), &HistoryOptions::default(), MEMORY_BYTES),
        Err(sparkles_core::Error::BudgetExceeded(_))
    ));
    let mut cold = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
    let err = cold
        .stored_fact(
            "cold-replay",
            &store,
            Some(&all()),
            &fact(&store, c1, "urn:s49999", "urn:p", "v49999"),
        )
        .unwrap_err();
    assert_eq!(err.code(), "incomplete-evidence");

    // Once commit 1 is resident in the store's shared history cache (for example,
    // warmed by a warm pin within history_cache_bytes), bounded reads and evidence
    // resolution reuse the shared cached state without re-charging its delta.
    store
        .create_snapshot_opts(
            "warm-c1",
            &At::Commit(c1),
            &SnapshotOptions {
                warm: true,
                ..Default::default()
            },
        )
        .unwrap();
    let (cached_snap, resolved) = store
        .snapshot_at_limited(&At::Commit(c1), &HistoryOptions::default(), MEMORY_BYTES)
        .unwrap();
    assert_eq!(resolved.commit.seq, c1);
    assert_eq!(cached_snap.commit, c1);

    let mut warm = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
    let verified = warm
        .stored_fact(
            "cached-fact",
            &store,
            Some(&all()),
            &fact(&store, c1, "urn:s49999", "urn:p", "v49999"),
        )
        .unwrap();
    assert_eq!(verified.commit, c1);
    assert!(
        used(&warm) <= 16 << 10,
        "cached shared snapshot over-charged: used={}",
        used(&warm)
    );

    // Cancellation and expired deadlines still apply to cached hits.
    let flag = Arc::new(AtomicBool::new(false));
    let mut cancelled = Resolver::new(Control::with_cancel(flag.clone()), MEMORY_BYTES).unwrap();
    flag.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(matches!(
        cancelled.stored_fact(
            "cached-cancelled",
            &store,
            Some(&all()),
            &fact(&store, c1, "urn:s0", "urn:p", "v0"),
        ),
        Err(sparkles_core::Error::Cancelled)
    ));
    let expired_opts = HistoryOptions {
        cancel: None,
        deadline: Some(Instant::now() - Duration::from_millis(1)),
    };
    assert!(matches!(
        store.snapshot_at_limited(&At::Commit(c1), &expired_opts, MEMORY_BYTES),
        Err(sparkles_core::Error::Timeout)
    ));

    // 2. In-memory store with a retained historical snapshot in mem_history.
    let mem = Store::in_memory(StoreOptions::default());
    let mc1 = insert_delta_quads(&mem, 0, 50_000, false);
    mem.create_snapshot("pinned", &At::Head, None).unwrap();
    let _mc2 = insert_delta_quads(&mem, 50_000, 1, false);
    let mut mem_r = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
    let verified = mem_r
        .stored_fact(
            "mem-cached",
            &mem,
            Some(&all()),
            &fact(&mem, mc1, "urn:s49999", "urn:p", "v49999"),
        )
        .unwrap();
    assert_eq!(verified.commit, mc1);
    assert!(
        used(&mem_r) <= 16 << 10,
        "in-memory retained snapshot over-charged: used={}",
        used(&mem_r)
    );
}

#[test]
fn shared_live_state_with_triple_protection_mask_resolves_within_allowance() {
    let store = Store::in_memory(StoreOptions::default());
    let commit = insert_delta_quads(&store, 0, 50_000, true);
    let policy = secret_policy();
    let mut r = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
    // Visible fact verifies within 64 MiB.
    let verified = r
        .stored_fact(
            "vis",
            &store,
            Some(&policy),
            &fact(&store, commit, "urn:s1", "urn:p", "v1"),
        )
        .unwrap();
    assert_eq!(verified.commit, commit);
    // Protected fact stays hidden as evidence-unavailable.
    let hidden = r
        .stored_fact(
            "sec",
            &store,
            Some(&policy),
            &fact(&store, commit, "urn:s0", "urn:secret", "v0"),
        )
        .unwrap_err();
    assert_eq!(hidden.code(), "evidence-unavailable");
}

#[test]
fn private_historical_replays_accumulate_retained_charge_and_enforce_allowance() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let mut commits = Vec::new();
    for batch in 0..6 {
        let c = insert_delta_quads(&store, batch * 2_000, 2_000, false);
        commits.push(c);
    }
    // Commit 0..4 are historical (commit 5 is head).
    // Each private replay retains 2 KiB/quad + 128 KiB:
    //   c0: 2,000 quads (~4.1 MiB)
    //   c1: 4,000 quads (~8.1 MiB) -> cumulative ~12.2 MiB
    //   c2: 6,000 quads (~12.1 MiB) -> cumulative ~24.4 MiB > 20 MiB!
    let mut r = Resolver::new(Control::none(), 20 << 20).unwrap();
    r.stored_fact(
        "c0",
        &store,
        Some(&all()),
        &fact(&store, commits[0], "urn:s0", "urn:p", "v0"),
    )
    .unwrap();
    let after_c0 = used(&r);
    assert!(
        after_c0 >= 4_000_000,
        "private materialized replay must keep its delta charged: {after_c0}"
    );
    r.stored_fact(
        "c1",
        &store,
        Some(&all()),
        &fact(&store, commits[1], "urn:s2000", "urn:p", "v2000"),
    )
    .unwrap();
    let after_c1 = used(&r);
    assert!(
        after_c1 >= 12_000_000,
        "cumulative private replays must accumulate retained charge: {after_c1}"
    );
    let err = r
        .stored_fact(
            "c2",
            &store,
            Some(&all()),
            &fact(&store, commits[2], "urn:s4000", "urn:p", "v4000"),
        )
        .unwrap_err();
    assert_eq!(err.code(), "incomplete-evidence");
}

fn median(mut values: Vec<u128>) -> u128 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn smallest_admitting_cap(
    store: &Store,
    access: &Arc<GraphAccess>,
    reference: &StoredFactRef,
) -> u64 {
    let (mut lo, mut hi) = (1u64 << 10, MEMORY_BYTES);
    while hi - lo > 1024 {
        let mid = lo + (hi - lo) / 2;
        let ok = Resolver::new(Control::none(), mid)
            .and_then(|mut r| r.stored_fact("probe", store, Some(access), reference))
            .is_ok();
        if ok {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// Opt-in release diagnostic measuring evidence resolution on shared live and
/// cached states at 10^4 to 10^5 uncompacted delta quads (roadmap item 542).
#[test]
#[ignore = "local evidence resolution delta-charge diagnostic"]
fn evidence_resolution_delta_charge_diagnostic() {
    let rounds = 7;
    for quads in [10_000usize, 50_000, 100_000] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let rev_iri = enable_and_write_record(&store);
        let c1 = insert_delta_quads(&store, 0, quads, true);
        let target_subject = format!("urn:s{}", quads - 1);
        let target_obj = format!("v{}", quads - 1);
        let live_ref = fact(&store, c1, &target_subject, "urn:p", &target_obj);

        // 1. Shared live stored-fact resolution (graph-all)
        let mut live_ns = Vec::new();
        let mut live_used = 0u64;
        for _ in 0..rounds {
            let mut r = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
            let t0 = Instant::now();
            r.stored_fact("live", &store, Some(&all()), &live_ref)
                .unwrap();
            live_ns.push(t0.elapsed().as_nanos());
            live_used = used(&r);
        }
        let live_cap = smallest_admitting_cap(&store, &all(), &live_ref);
        eprintln!(
            "DIAG quads={quads} mode=live_all median_ns={} inner_used={} smallest_cap={live_cap}",
            median(live_ns),
            live_used
        );

        // 2. Shared live record resolution (graph-all)
        let rec_req = RecordRequest {
            iri: rev_iri,
            kinds: vec![RecordKind::SourceRevision],
            content_digest: None,
        };
        let mut rec_ns = Vec::new();
        let mut rec_used = 0u64;
        for _ in 0..rounds {
            let mut r = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
            let t0 = Instant::now();
            r.record("rec", &store, &store, Some(&all()), &rec_req)
                .unwrap();
            rec_ns.push(t0.elapsed().as_nanos());
            rec_used = used(&r);
        }
        eprintln!(
            "DIAG quads={quads} mode=live_record median_ns={} inner_used={rec_used}",
            median(rec_ns)
        );

        // 3. Shared live stored-fact resolution with triple protection (10% hidden)
        let policy = secret_policy();
        let mut masked_cold_ns = Vec::new();
        let mut masked_used = 0u64;
        for i in 0..3 {
            let p = Arc::new(GraphAccess::with_triples(
                Graphs::All,
                Graphs::none(),
                TripleRules {
                    rules: vec![Rule {
                        protection: Arc::new(Protection {
                            name: format!("secret-{i}"),
                            predicates: Some(vec!["urn:secret".into()]),
                            classes: None,
                            subclasses: false,
                            graphs: None,
                            pattern: None,
                            prefixes: BTreeMap::new(),
                            hide_inferences: false,
                        }),
                        read: Graphs::none(),
                        write: Graphs::none(),
                    }],
                    caller: Caller::default(),
                    limits: Limits::default(),
                },
            ));
            let mut r = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
            let t0 = Instant::now();
            r.stored_fact("masked-cold", &store, Some(&p), &live_ref)
                .unwrap();
            masked_cold_ns.push(t0.elapsed().as_nanos());
            masked_used = used(&r);
        }
        // Keep one view live so the controlled mask cache stays warm across rounds.
        let mut holder = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
        holder
            .stored_fact("masked-warm-seed", &store, Some(&policy), &live_ref)
            .unwrap();
        let mut masked_warm_ns = Vec::new();
        for _ in 0..rounds {
            let mut r = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
            let t0 = Instant::now();
            r.stored_fact("masked-warm", &store, Some(&policy), &live_ref)
                .unwrap();
            masked_warm_ns.push(t0.elapsed().as_nanos());
        }
        drop(holder);
        eprintln!(
            "DIAG quads={quads} mode=live_masked_10pct cold_median_ns={} warm_median_ns={} inner_used={masked_used}",
            median(masked_cold_ns),
            median(masked_warm_ns)
        );

        // 4. Advance head by 1 quad, verify private uncached replay behavior, then
        //    warm commit c1 in history cache and measure cached hit resolution.
        let _c2 = insert_delta_quads(&store, quads, 1, false);
        let cold_outcome = Resolver::new(Control::none(), MEMORY_BYTES)
            .unwrap()
            .stored_fact("cold-hist", &store, Some(&all()), &live_ref)
            .map(|_| "ok".to_string())
            .unwrap_or_else(|e| e.code().to_string());
        store
            .create_snapshot_opts(
                "warm-c1",
                &At::Commit(c1),
                &SnapshotOptions {
                    warm: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let mut cached_ns = Vec::new();
        let mut cached_used = 0u64;
        for _ in 0..rounds {
            let mut r = Resolver::new(Control::none(), MEMORY_BYTES).unwrap();
            let t0 = Instant::now();
            r.stored_fact("cached", &store, Some(&all()), &live_ref)
                .unwrap();
            cached_ns.push(t0.elapsed().as_nanos());
            cached_used = used(&r);
        }
        let cached_cap = smallest_admitting_cap(&store, &all(), &live_ref);
        eprintln!(
            "DIAG quads={quads} mode=cached_all cold_replay_64m={cold_outcome} median_ns={} inner_used={cached_used} smallest_cap={cached_cap}",
            median(cached_ns)
        );
    }
}
