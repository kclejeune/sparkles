//! Cloning stores with activated configuration history (roadmap item 764):
//! destination root establishment, source provenance closures, historical evidence
//! resolution, independence from the source, crash/cancellation cleanup, and
//! tamper/version refusals.

use oxrdf::NamedNode;
use sparkles_core::access::{GraphRule, Graphs, Preparation};
use sparkles_core::check::{CheckOptions, check};
use sparkles_core::evidence::Settings;
use sparkles_core::history::At;
use sparkles_core::managed::{OwnerEntry, OwnerKind};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{CloneMethod, CloneMode, CloneOptions, Store, StoreOptions};
use sparkles_core::task::Control;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn prep() -> Preparation {
    Preparation::new(Control::none(), 64 << 20).unwrap()
}

fn upd(store: &Store, sparql: &str) {
    update(store, sparql, &QueryOptions::default()).unwrap();
}

fn insert(store: &Store, label: &str) {
    upd(store, &format!("INSERT DATA {{ <urn:{label}> <urn:p> 1 }}"));
}

fn configure_evidence(store: &Store, source_graph: &str, proposal_graph: &str) {
    let settings = Settings {
        enabled: true,
        source_records: vec![source_graph.into()],
        proposals: vec![proposal_graph.into()],
        ..Default::default()
    };
    let canonical = settings.canonical(&Control::none()).unwrap();
    store
        .enable_managed(
            OwnerKind::Evidence,
            OwnerEntry::for_settings(1, settings.managed_graphs(), canonical.bytes()),
            canonical.bytes(),
        )
        .unwrap();
}

fn activated_store(root: &Path) -> Store {
    let store = Store::open(root, StoreOptions::default()).unwrap();
    upd(&store, "INSERT DATA { <urn:seed> <urn:p> 0 }");
    drop(store);
    sparkles_core::migrate::activate_configuration_history(root, false, &prep()).unwrap();
    let store = Store::open(root, StoreOptions::default()).unwrap();
    configure_evidence(&store, "urn:src:v1", "urn:prop:v1");
    upd(&store, "INSERT DATA { <urn:s1> <urn:p> 1 }");
    store.compact().unwrap();
    configure_evidence(&store, "urn:src:v2", "urn:prop:v2");
    upd(
        &store,
        "INSERT DATA { GRAPH <urn:g1> { <urn:s2> <urn:p> 2 } }",
    );
    store
}

fn dump_sorted(store: &Store) -> Vec<String> {
    let mut out = Vec::new();
    store.dump_nquads(&mut out).unwrap();
    let mut lines: Vec<String> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "sparkles.lock" {
            continue;
        }
        let target = dst.join(name);
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn persistent_clone_modes_and_historical_evidence_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    src.set_retention(sparkles_core::history::Retention {
        keep_commits: Some(10),
        ..Default::default()
    })
    .unwrap();
    src.compact().unwrap();
    let src_quads = dump_sorted(&src);
    let src_head = src.evidence_view(&At::Head, &prep()).unwrap();

    for mode in [CloneMode::Auto, CloneMode::Link, CloneMode::Rebuild] {
        let dst_root = tmp.path().join(format!("clone-{}", mode.name()));
        let report = src
            .clone_to(
                &dst_root,
                &CloneOptions {
                    mode,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(report.forked_from.id, src.dataset_id());
        assert_eq!(report.forked_from.seq, src.head_commit().seq);
        if mode == CloneMode::Rebuild {
            assert_eq!(report.method, CloneMethod::Rebuild);
        }
        let check_report = check(&dst_root, &CheckOptions { quick: false }).unwrap();
        assert_eq!(check_report.errors, 0, "{mode:?}: {check_report:?}");

        let dst = Store::open(&dst_root, StoreOptions::default()).unwrap();
        assert_eq!(dump_sorted(&dst), src_quads);
        assert_eq!(dst.dataset_id(), report.dataset_id);
        assert_ne!(dst.dataset_id(), src.dataset_id());

        // Head and Commit(0) resolve to the clone's own genesis generation.
        let gen1 = dst.evidence_view(&At::Head, &prep()).unwrap();
        let at0 = dst.evidence_view(&At::Commit(0), &prep()).unwrap();
        assert_eq!(gen1.configuration(), at0.configuration());
        assert_eq!(gen1.configuration().dataset_id(), dst.dataset_id());
        assert_ne!(gen1.configuration(), src_head.configuration());

        // Compacting at seq 0 preserves creation_commit validation across reopen.
        dst.compact().unwrap();
        drop(dst);
        let check_after_compact0 = check(&dst_root, &CheckOptions { quick: false }).unwrap();
        assert_eq!(check_after_compact0.errors, 0, "{check_after_compact0:?}");
        let dst = Store::open(&dst_root, StoreOptions::default()).unwrap();
        assert_eq!(
            dst.evidence_view(&At::Commit(0), &prep())
                .unwrap()
                .configuration(),
            gen1.configuration()
        );

        // New writes and configuration changes advance the clone independently.
        upd(&dst, "INSERT DATA { <urn:clone-w1> <urn:p> 10 }");
        configure_evidence(&dst, "urn:src:clone-v3", "urn:prop:clone-v3");
        let gen2 = dst.evidence_view(&At::Head, &prep()).unwrap();
        assert_ne!(gen2.configuration(), gen1.configuration());
        upd(&dst, "INSERT DATA { <urn:clone-w2> <urn:p> 20 }");
        assert_eq!(
            dst.evidence_view(&At::Commit(1), &prep())
                .unwrap()
                .configuration(),
            gen1.configuration()
        );
        assert_eq!(
            dst.evidence_view(&At::Commit(2), &prep())
                .unwrap()
                .configuration(),
            gen2.configuration()
        );
        // Managed graph protection is enforced in the clone.
        assert!(
            update(
                &dst,
                "INSERT DATA { GRAPH <urn:prop:clone-v3> { <urn:bad> <urn:p> 1 } }",
                &QueryOptions::default(),
            )
            .is_err()
        );
        dst.compact().unwrap();
        drop(dst);
        let final_check = check(&dst_root, &CheckOptions { quick: false }).unwrap();
        assert_eq!(final_check.errors, 0, "{final_check:?}");
    }

    // Past-state and partial-graph clones also establish valid destination roots.
    let past_root = tmp.path().join("clone-past");
    let past_rep = src
        .clone_to(
            &past_root,
            &CloneOptions {
                at: Some(At::Commit(2)),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(past_rep.forked_from.seq, 2);
    assert_eq!(
        check(&past_root, &CheckOptions { quick: false })
            .unwrap()
            .errors,
        0
    );
    let past = Store::open(&past_root, StoreOptions::default()).unwrap();
    assert_eq!(
        past.evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration()
            .dataset_id(),
        past.dataset_id()
    );

    let partial_root = tmp.path().join("clone-partial");
    src.clone_to(
        &partial_root,
        &CloneOptions {
            graphs: Some(Graphs::Only(GraphRule::new(["urn:g1"], &[]))),
            exclude_graphs: vec![NamedNode::new_unchecked("urn:x-sparkles:inferred")],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        check(&partial_root, &CheckOptions { quick: false })
            .unwrap()
            .errors,
        0
    );
    let partial = Store::open(&partial_root, StoreOptions::default()).unwrap();
    assert_eq!(partial.snapshot().len(), 1);
    assert_eq!(
        partial
            .evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration()
            .dataset_id(),
        partial.dataset_id()
    );

    // Source was unaffected by clone mutations, and deleting source leaves clones working.
    assert_eq!(
        src.evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration(),
        src_head.configuration()
    );
    drop(src);
    std::fs::remove_dir_all(&src_root).unwrap();
    let reopened = Store::open(&tmp.path().join("clone-link"), StoreOptions::default()).unwrap();
    assert_eq!(
        reopened
            .evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration()
            .dataset_id(),
        reopened.dataset_id()
    );
}

#[test]
fn clone_of_clone_and_branch_clone_preserve_provenance_dag() {
    let tmp = tempfile::tempdir().unwrap();
    let a_root = tmp.path().join("a");
    let a = activated_store(&a_root);

    // Clone A -> B
    let b_root = tmp.path().join("b");
    a.clone_to(&b_root, &Default::default()).unwrap();
    let b = Store::open(&b_root, StoreOptions::default()).unwrap();
    configure_evidence(&b, "urn:src:b", "urn:prop:b");
    upd(&b, "INSERT DATA { <urn:b> <urn:p> 1 }");

    // Clone B -> C: provenance DAG carries both A and B.
    let c_root = tmp.path().join("c");
    b.clone_to(&c_root, &Default::default()).unwrap();
    let origin: serde_json::Value = serde_json::from_slice(
        &std::fs::read(c_root.join("evidence-history/origin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(origin["format"].as_u64(), Some(2));
    assert_eq!(origin["sources"].as_array().unwrap().len(), 2);
    let check_c = check(&c_root, &CheckOptions { quick: false }).unwrap();
    assert_eq!(check_c.errors, 0, "{check_c:?}");
    let c = Store::open(&c_root, StoreOptions::default()).unwrap();
    assert_eq!(
        c.evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration()
            .dataset_id(),
        c.dataset_id()
    );

    // Branch clone (both persistent and in-memory).
    a.create_branch("feature", &Default::default()).unwrap();
    let branch = a.branch("feature").unwrap();
    upd(&branch, "INSERT DATA { <urn:br> <urn:p> 1 }");
    let br_clone_root = tmp.path().join("branch-clone");
    let br_rep = branch
        .clone_to(&br_clone_root, &Default::default())
        .unwrap();
    assert_eq!(br_rep.forked_from.id, branch.dataset_id());
    let check_br = check(&br_clone_root, &CheckOptions { quick: false }).unwrap();
    assert_eq!(check_br.errors, 0, "{check_br:?}");
    let br_dst = Store::open(&br_clone_root, StoreOptions::default()).unwrap();
    assert_eq!(
        br_dst
            .evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration()
            .dataset_id(),
        br_dst.dataset_id()
    );
    let (br_mem, _) = branch
        .clone_to_memory(&Default::default(), StoreOptions::default())
        .unwrap();
    assert_eq!(
        br_mem
            .evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration()
            .dataset_id(),
        br_mem.dataset_id()
    );
}

#[test]
fn in_memory_clones_of_activated_stores_are_independent() {
    let tmp = tempfile::tempdir().unwrap();
    let mem_src = Store::in_memory(StoreOptions::default());
    mem_src
        .activate_memory_configuration_history(&prep())
        .unwrap();
    configure_evidence(&mem_src, "urn:mem:src1", "urn:mem:prop1");
    upd(&mem_src, "INSERT DATA { <urn:m> <urn:p> 1 }");
    let src_cfg = mem_src
        .evidence_view(&At::Head, &prep())
        .unwrap()
        .configuration();

    // In-memory -> in-memory
    let (mem_clone, _) = mem_src
        .clone_to_memory(&Default::default(), StoreOptions::default())
        .unwrap();
    mem_clone
        .set_retention(sparkles_core::history::Retention {
            keep_commits: Some(10),
            ..Default::default()
        })
        .unwrap();
    let clone_gen1 = mem_clone
        .evidence_view(&At::Head, &prep())
        .unwrap()
        .configuration();
    assert_eq!(clone_gen1.dataset_id(), mem_clone.dataset_id());
    assert_ne!(clone_gen1, src_cfg);
    assert_eq!(
        mem_clone
            .evidence_view(&At::Commit(0), &prep())
            .unwrap()
            .configuration(),
        clone_gen1
    );
    configure_evidence(&mem_clone, "urn:mem:src2", "urn:mem:prop2");
    upd(&mem_clone, "INSERT DATA { <urn:m2> <urn:p> 2 }");
    let clone_gen2 = mem_clone
        .evidence_view(&At::Head, &prep())
        .unwrap()
        .configuration();
    assert_ne!(clone_gen2, clone_gen1);
    assert_eq!(
        mem_clone
            .evidence_view(&At::Commit(0), &prep())
            .unwrap()
            .configuration(),
        clone_gen1
    );
    assert_eq!(
        mem_clone
            .evidence_view(&At::Commit(1), &prep())
            .unwrap()
            .configuration(),
        clone_gen2
    );
    assert_eq!(
        mem_src
            .evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration(),
        src_cfg
    );

    // In-memory -> persistent
    let disk_root = tmp.path().join("mem-to-disk");
    mem_src.clone_to(&disk_root, &Default::default()).unwrap();
    let check_disk = check(&disk_root, &CheckOptions { quick: false }).unwrap();
    assert_eq!(check_disk.errors, 0, "{check_disk:?}");
    let disk = Store::open(&disk_root, StoreOptions::default()).unwrap();
    assert_eq!(
        disk.evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration()
            .dataset_id(),
        disk.dataset_id()
    );
}

#[test]
fn incomplete_clone_and_cancellation_leave_no_partial_state() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);

    // Pre-cancelled clone writes nothing.
    let cancelled_dir = tmp.path().join("cancelled");
    let err = src
        .clone_to(
            &cancelled_dir,
            &CloneOptions {
                cancel: Some(Arc::new(AtomicBool::new(true))),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, sparkles_core::Error::Cancelled), "{err}");
    assert!(!cancelled_dir.exists());

    // Simulated crash before CURRENT leaves an unopenable directory (never opened as fresh/legacy).
    let good = tmp.path().join("good");
    src.clone_to(&good, &Default::default()).unwrap();
    let crashed = tmp.path().join("crashed");
    copy_dir_recursive(&good, &crashed);
    std::fs::remove_file(crashed.join("CURRENT")).unwrap();
    assert!(Store::open(&crashed, StoreOptions::default()).is_err());
}

#[test]
fn clone_origin_refuses_tampering_missing_closure_and_unknown_versions() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    let good = tmp.path().join("good");
    src.clone_to(&good, &Default::default()).unwrap();
    assert_eq!(
        check(&good, &CheckOptions { quick: false }).unwrap().errors,
        0
    );

    // 1. Unknown origin.json format version -> Error::Unsupported on both open and check.
    let bad_ver = tmp.path().join("bad-ver");
    copy_dir_recursive(&good, &bad_ver);
    let mut origin: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bad_ver.join("evidence-history/origin.json")).unwrap(),
    )
    .unwrap();
    origin["format"] = 99.into();
    std::fs::write(
        bad_ver.join("evidence-history/origin.json"),
        serde_json::to_vec(&origin).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        Store::open(&bad_ver, StoreOptions::default()),
        Err(sparkles_core::Error::Unsupported(_))
    ));
    assert!(matches!(
        sparkles_core::check::configuration_history(&bad_ver, true, &Control::none()),
        Err(sparkles_core::Error::Unsupported(_))
    ));

    // 2. Lowered minimumReader (< 11) with format 2 origin.json -> Error::Unsupported.
    let low_reader = tmp.path().join("low-reader");
    copy_dir_recursive(&good, &low_reader);
    let mut ds: serde_json::Value =
        serde_json::from_slice(&std::fs::read(low_reader.join("dataset.json")).unwrap()).unwrap();
    ds["minimumReader"] = (sparkles_core::commit::DATASET_READER - 1).into();
    std::fs::write(
        low_reader.join("dataset.json"),
        serde_json::to_vec_pretty(&ds).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        Store::open(&low_reader, StoreOptions::default()),
        Err(sparkles_core::Error::Unsupported(_))
    ));
    assert!(matches!(
        sparkles_core::check::configuration_history(&low_reader, true, &Control::none()),
        Err(sparkles_core::Error::Unsupported(_))
    ));

    // 3. Orphan imports/ without origin.json -> refused.
    let orphan = tmp.path().join("orphan-imports");
    copy_dir_recursive(&good, &orphan);
    std::fs::remove_file(orphan.join("evidence-history/origin.json")).unwrap();
    assert!(Store::open(&orphan, StoreOptions::default()).is_err());
    assert!(sparkles_core::check::configuration_history(&orphan, true, &Control::none()).is_err());

    // 4. Tampered source provenance file inside evidence-history/imports/ -> refused.
    let bad_src = tmp.path().join("tampered-src");
    copy_dir_recursive(&good, &bad_src);
    let imp_dir = std::fs::read_dir(bad_src.join("evidence-history/imports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let target = imp_dir.join("source/managed.json");
    let mut bytes = std::fs::read(&target).unwrap();
    bytes[0] ^= 0x20;
    std::fs::write(&target, bytes).unwrap();
    assert!(Store::open(&bad_src, StoreOptions::default()).is_err());
    assert!(
        check(&bad_src, &CheckOptions { quick: false })
            .unwrap()
            .errors
            > 0
    );

    // 5. Missing imported source descriptor -> refused.
    let missing_desc = tmp.path().join("missing-desc");
    copy_dir_recursive(&good, &missing_desc);
    let imp_dir = std::fs::read_dir(missing_desc.join("evidence-history/imports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::remove_file(imp_dir.join("origin.json")).unwrap();
    assert!(Store::open(&missing_desc, StoreOptions::default()).is_err());
    assert!(
        check(&missing_desc, &CheckOptions { quick: false })
            .unwrap()
            .errors
            > 0
    );

    // 6. Tampered creationCommit pin in origin.json -> refused.
    let bad_pin = tmp.path().join("bad-pin");
    copy_dir_recursive(&good, &bad_pin);
    let mut origin: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bad_pin.join("evidence-history/origin.json")).unwrap(),
    )
    .unwrap();
    origin["destination"]["creationCommit"] = format!("sha256:{}", "00".repeat(32)).into();
    std::fs::write(
        bad_pin.join("evidence-history/origin.json"),
        serde_json::to_vec(&origin).unwrap(),
    )
    .unwrap();
    assert!(Store::open(&bad_pin, StoreOptions::default()).is_err());
    assert!(
        check(&bad_pin, &CheckOptions { quick: false })
            .unwrap()
            .errors
            > 0
    );
}

#[test]
fn clone_of_activated_store_keeps_paused_views_and_empty_control_keyspace() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    // Add a running view definition.
    let view_dir = src_root.join("views/recent");
    std::fs::create_dir_all(&view_dir).unwrap();
    std::fs::write(
        view_dir.join("view.json"),
        serde_json::json!({
            "format": 1,
            "query": "SELECT ?s WHERE { ?s <urn:p> ?o }",
            "principal": "urn:x-sparkles:principal:ops"
        })
        .to_string(),
    )
    .unwrap();
    // Enable control keyspace.
    src.enable_control().unwrap();

    let dst_root = tmp.path().join("dst");
    src.clone_to(&dst_root, &Default::default()).unwrap();
    assert_eq!(
        check(&dst_root, &CheckOptions { quick: false })
            .unwrap()
            .errors,
        0
    );

    // View is paused with reason "cloned".
    let view_bytes = std::fs::read(dst_root.join("views/recent/view.json")).unwrap();
    let view_json: serde_json::Value = serde_json::from_slice(&view_bytes).unwrap();
    assert_eq!(view_json["paused"]["reason"], "cloned");

    // Control keyspace opens clean and empty, and evidence views still work.
    let dst = Store::open(&dst_root, StoreOptions::default()).unwrap();
    assert!(dst.control_enabled());
    assert_eq!(
        dst.evidence_view(&At::Head, &prep())
            .unwrap()
            .configuration()
            .dataset_id(),
        dst.dataset_id()
    );
}

#[test]
fn cold_retained_clone_generation_0_serves_evidence_view_and_past_state_clone() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    let clone_root = tmp.path().join("clone");
    src.clone_to(&clone_root, &Default::default()).unwrap();

    // Open with generation retention, write own commit, and compact past gen-0001.
    let retain_opts = StoreOptions {
        history_max_generations: 4,
        ..StoreOptions::default()
    };
    let dst = Store::open(&clone_root, retain_opts).unwrap();
    dst.set_retention(sparkles_core::history::Retention {
        keep_commits: Some(100),
        ..Default::default()
    })
    .unwrap();
    let gen0_cfg = dst
        .evidence_view(&At::Head, &prep())
        .unwrap()
        .configuration();
    insert(&dst, "own-after-clone");
    dst.compact().unwrap();
    assert_ne!(
        std::fs::read_to_string(clone_root.join("CURRENT"))
            .unwrap()
            .trim(),
        "gen-0001"
    );

    // Cold read of retained sequence 0 must succeed via creationCommit authentication.
    let cold_at0 = dst
        .evidence_view(&At::Commit(0), &prep())
        .expect("cold retained sequence 0 evidence view must succeed");
    assert_eq!(cold_at0.configuration(), gen0_cfg);

    // Past-state clone from retained sequence 0 must also succeed and reopen cleanly.
    let past_root = tmp.path().join("past-clone");
    dst.clone_to(
        &past_root,
        &CloneOptions {
            at: Some(At::Commit(0)),
            ..Default::default()
        },
    )
    .expect("past-state clone of retained sequence 0 must succeed");
    let past = Store::open(&past_root, StoreOptions::default()).unwrap();
    assert_eq!(past.snapshot().len(), src.snapshot().len());
    assert_eq!(
        past.evidence_view(&At::Commit(0), &prep())
            .unwrap()
            .configuration()
            .dataset_id(),
        past.dataset_id()
    );
}

#[test]
fn imported_destination_branch_cutoff_origin_and_cold_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    insert(&src, "bound-1");
    let src_cfg = src
        .evidence_view(&At::Head, &prep())
        .unwrap()
        .configuration();

    // Reidentify a copy of `src` so its head has an `ImportedUnbound` binding at seq_head.
    let dst_root = tmp.path().join("reid");
    let cap = src.backup_capture("b").unwrap();
    cap.write_to(&dst_root).unwrap();
    let new_id = uuid::Uuid::new_v4();
    sparkles_core::commit::reidentify_configuration_history(
        &dst_root,
        new_id,
        sparkles_core::commit::ForkedFrom {
            id: src.dataset_id(),
            seq: src.head_commit().seq,
        },
        &Control::none(),
    )
    .unwrap();
    let imported_seq = src.head_commit().seq;

    let dst = Store::open(&dst_root, StoreOptions::default()).unwrap();
    insert(&dst, "dst-own-write");
    let own_seq = dst.head_commit().seq;

    // 1. Valid branch forked at imported commit (`ImportedUnbound`) opens, checks,
    //    compacts, and supports a child branch / cold read.
    let b_imp = dst
        .create_branch(
            "from-imported",
            &sparkles_core::branch::BranchOptions {
                at: At::Commit(imported_seq),
                ..Default::default()
            },
        )
        .unwrap();
    let b_imp_store = dst.branch("from-imported").unwrap();
    b_imp_store.compact().unwrap();
    dst.create_branch(
        "child-of-imported",
        &sparkles_core::branch::BranchOptions {
            from: "from-imported".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let child = dst.branch("child-of-imported").unwrap();
    assert_eq!(child.snapshot().len(), b_imp_store.snapshot().len());
    drop(child);
    drop(b_imp_store);
    drop(dst);

    assert_eq!(
        check(&dst_root, &CheckOptions { quick: false })
            .unwrap()
            .errors,
        0
    );
    // Reopen family and open the compacted ImportedUnbound branch + child branch.
    let dst = Store::open(&dst_root, StoreOptions::default()).unwrap();
    let b_imp_reopened = dst.branch("from-imported").unwrap();
    assert_eq!(b_imp_reopened.head_commit().seq, imported_seq);
    let _child_reopened = dst.branch("child-of-imported").unwrap();

    // Create a second branch at `own_seq` (`Known` origin) and write a post-cutoff own commit.
    let b_head = dst.create_branch("from-head", &Default::default()).unwrap();
    let b_head_store = dst.branch("from-head").unwrap();
    insert(&b_head_store, "branch-own-1");
    b_head_store.compact().unwrap();
    let b_head_gen = std::fs::read_to_string(
        dst_root
            .join("branches")
            .join(b_head.id.to_string())
            .join("CURRENT"),
    )
    .unwrap()
    .trim()
    .to_string();
    // Create a third branch at `own_seq` (no own writes -> `branch.json` pins `Known` at `own_seq`).
    let b_pinned = dst
        .create_branch("from-head-pinned", &Default::default())
        .unwrap();
    drop(_child_reopened);
    drop(b_imp_reopened);
    drop(b_head_store);
    drop(dst);
    let _ = (b_imp, own_seq);

    // 2. Post-cutoff tamper: rewrite `from-head`'s compacted post-cutoff `commit.json`
    //    to `ImportedUnbound { source: src_cfg }`. Both `validate_copy` and `open_branch`
    //    must refuse.
    let post_tamper = tmp.path().join("tamper-post-cutoff");
    copy_dir_recursive(&dst_root, &post_tamper);
    let post_commit_path = post_tamper
        .join("branches")
        .join(b_head.id.to_string())
        .join(&b_head_gen)
        .join("commit.json");
    let mut cj: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&post_commit_path).unwrap()).unwrap();
    cj["configurationBinding"] = serde_json::json!({
        "kind": "imported-unbound",
        "source": src_cfg,
    });
    std::fs::write(&post_commit_path, serde_json::to_vec_pretty(&cj).unwrap()).unwrap();
    assert!(
        sparkles_core::check::configuration_history(&post_tamper, true, &Control::none()).is_err(),
        "validate_copy must refuse post-cutoff ImportedUnbound branch base"
    );
    let reopened = Store::open(&post_tamper, StoreOptions::default()).unwrap();
    assert!(
        reopened.branch("from-head").is_err(),
        "Store::branch must refuse post-cutoff ImportedUnbound branch base"
    );
    drop(reopened);

    // 3. Branch-origin mismatch tamper: rewrite `from-head-pinned`'s root `commit.json`
    //    from `Known` to `ImportedUnbound { source: src_cfg }` while `branch.json` still
    //    pins `Known`. Both `validate_copy` and `open_branch` must refuse.
    let origin_tamper = tmp.path().join("tamper-origin-mismatch");
    copy_dir_recursive(&dst_root, &origin_tamper);
    let pinned_dir = origin_tamper.join("branches").join(b_pinned.id.to_string());
    let pinned_gen = std::fs::read_to_string(pinned_dir.join("CURRENT"))
        .unwrap()
        .trim()
        .to_string();
    let pinned_commit_path = pinned_dir.join(&pinned_gen).join("commit.json");
    let mut cj: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pinned_commit_path).unwrap()).unwrap();
    cj["configurationBinding"] = serde_json::json!({
        "kind": "imported-unbound",
        "source": src_cfg,
    });
    std::fs::write(&pinned_commit_path, serde_json::to_vec_pretty(&cj).unwrap()).unwrap();
    assert!(
        sparkles_core::check::configuration_history(&origin_tamper, true, &Control::none())
            .is_err(),
        "validate_copy must refuse origin-mismatched ImportedUnbound branch base"
    );
    let reopened = Store::open(&origin_tamper, StoreOptions::default()).unwrap();
    assert!(
        reopened.branch("from-head-pinned").is_err(),
        "Store::branch must refuse origin-mismatched ImportedUnbound branch base"
    );
}

#[test]
fn reader_11_compacted_native_only_refuses_missing_origin_and_format_1_substitution() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    let good = tmp.path().join("clone");
    src.clone_to(&good, &Default::default()).unwrap();
    // Write a native commit and compact so CURRENT is native-only (no imported base/ranges).
    {
        let s = Store::open(&good, StoreOptions::default()).unwrap();
        insert(&s, "own-1");
        s.compact().unwrap();
    }

    // 1. Delete BOTH origin.json and imports/ -> reader 11 must refuse open and check.
    let stripped = tmp.path().join("stripped");
    copy_dir_recursive(&good, &stripped);
    std::fs::remove_file(stripped.join("evidence-history/origin.json")).unwrap();
    std::fs::remove_dir_all(stripped.join("evidence-history/imports")).unwrap();
    assert!(Store::open(&stripped, StoreOptions::default()).is_err());
    assert!(
        sparkles_core::check::configuration_history(&stripped, true, &Control::none()).is_err()
    );

    // 2. Substitute a format-1 origin.json with the same datasetId -> reader 11 must refuse.
    let fmt1 = tmp.path().join("format1-sub");
    copy_dir_recursive(&good, &fmt1);
    let mut origin: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fmt1.join("evidence-history/origin.json")).unwrap())
            .unwrap();
    origin["format"] = 1.into();
    origin.as_object_mut().unwrap().remove("destination");
    std::fs::write(
        fmt1.join("evidence-history/origin.json"),
        serde_json::to_vec(&origin).unwrap(),
    )
    .unwrap();
    assert!(Store::open(&fmt1, StoreOptions::default()).is_err());
    assert!(sparkles_core::check::configuration_history(&fmt1, true, &Control::none()).is_err());
}

#[test]
fn migrate_plan_reports_unsupported_and_under_reader_origin_json() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    let good = tmp.path().join("clone");
    src.clone_to(&good, &Default::default()).unwrap();

    // 1. Unknown origin format 99 reported by migrate::plan.
    let bad_fmt = tmp.path().join("bad-fmt");
    copy_dir_recursive(&good, &bad_fmt);
    let mut origin: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bad_fmt.join("evidence-history/origin.json")).unwrap(),
    )
    .unwrap();
    origin["format"] = 99.into();
    std::fs::write(
        bad_fmt.join("evidence-history/origin.json"),
        serde_json::to_vec(&origin).unwrap(),
    )
    .unwrap();
    let plan = sparkles_core::migrate::plan(&bad_fmt).unwrap();
    assert!(
        plan.refused
            .iter()
            .any(|msg| msg.contains("origin.json") && msg.contains("99")),
        "{plan:?}"
    );

    // 2. Format 2 origin.json paired with minimumReader 10 reported by migrate::plan.
    let low_reader = tmp.path().join("low-reader");
    copy_dir_recursive(&good, &low_reader);
    let mut ds: serde_json::Value =
        serde_json::from_slice(&std::fs::read(low_reader.join("dataset.json")).unwrap()).unwrap();
    ds["minimumReader"] = (sparkles_core::commit::DATASET_READER - 1).into();
    std::fs::write(
        low_reader.join("dataset.json"),
        serde_json::to_vec_pretty(&ds).unwrap(),
    )
    .unwrap();
    let plan = sparkles_core::migrate::plan(&low_reader).unwrap();
    assert!(
        plan.refused
            .iter()
            .any(|msg| msg.contains("origin.json") && msg.contains("reader 11")),
        "{plan:?}"
    );
}

#[test]
fn live_origin_substitution_is_refused_on_runtime_binding_lookup() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    let clone_root = tmp.path().join("clone");
    src.clone_to(&clone_root, &Default::default()).unwrap();
    let dst = Store::open(&clone_root, StoreOptions::default()).unwrap();
    insert(&dst, "own-live-1");
    let seq = dst.head_commit().seq;
    // Sanity check: lookup succeeds before tampering.
    dst.evidence_view(&At::Commit(seq), &prep()).unwrap();

    // Externally substitute origin.json bytes (whitespace change -> valid JSON, different digest).
    let origin_path = clone_root.join("evidence-history/origin.json");
    let mut bytes = std::fs::read(&origin_path).unwrap();
    bytes.push(b'\n');
    std::fs::write(&origin_path, &bytes).unwrap();

    assert!(
        dst.evidence_view(&At::Commit(seq), &prep()).is_err(),
        "live origin substitution must be detected on runtime configuration lookup"
    );
}

#[cfg(unix)]
#[test]
fn disk_closure_refuses_import_symlink_and_dag_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    let good = tmp.path().join("clone");
    src.clone_to(&good, &Default::default()).unwrap();

    // 1. Intermediate directory symlink under evidence-history/imports/<hex>/source -> refused.
    let sym = tmp.path().join("symlinked");
    copy_dir_recursive(&good, &sym);
    let imp_dir = std::fs::read_dir(sym.join("evidence-history/imports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let real_src = tmp.path().join("outside-source");
    std::fs::rename(imp_dir.join("source"), &real_src).unwrap();
    std::os::unix::fs::symlink(&real_src, imp_dir.join("source")).unwrap();
    assert!(Store::open(&sym, StoreOptions::default()).is_err());
    assert!(sparkles_core::check::configuration_history(&sym, true, &Control::none()).is_err());

    // 2. Self-referential / cyclic dependency in descriptor -> refused by DiskClosure::load.
    let cyc = tmp.path().join("cycle");
    copy_dir_recursive(&good, &cyc);
    let imp_dir = std::fs::read_dir(cyc.join("evidence-history/imports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let desc_bytes = std::fs::read(imp_dir.join("origin.json")).unwrap();
    let mut desc: serde_json::Value = serde_json::from_slice(&desc_bytes).unwrap();
    let self_hex = imp_dir.file_name().unwrap().to_str().unwrap().to_string();
    desc["sources"] = serde_json::json!([format!("sha256:{self_hex}")]);
    let new_desc_bytes = serde_json::to_vec(&desc).unwrap();
    std::fs::write(imp_dir.join("origin.json"), &new_desc_bytes).unwrap();
    assert!(Store::open(&cyc, StoreOptions::default()).is_err());
    assert!(sparkles_core::check::configuration_history(&cyc, true, &Control::none()).is_err());
}

#[test]
fn reidentify_streams_annotations_respects_cancel_and_guards_misuse() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    // Write multiple large annotated commits so both `annotations.bin` and `wal.log`
    // span multiple 64 KiB chunks (messages stay within the 1024-byte commit message cap).
    for i in 0..180 {
        let mut w = src.write_with(
            sparkles_core::commit::CommitKind::Update,
            sparkles_core::guard::WriteOptions {
                message: Some(Arc::from(format!("commit-{i:03}-{}", "x".repeat(960)))),
                ..Default::default()
            },
        );
        let p = w
            .intern(&oxrdf::Term::NamedNode(
                oxrdf::NamedNode::new("urn:p").unwrap(),
            ))
            .unwrap();
        for j in 0..20 {
            let s = w
                .intern(&oxrdf::Term::NamedNode(
                    oxrdf::NamedNode::new(format!("urn:ann:{i}:{j}")).unwrap(),
                ))
                .unwrap();
            let o = w
                .intern(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
                    format!("v{i}-{j}"),
                )))
                .unwrap();
            w.insert([s, p, o, sparkles_core::id::Id::DEFAULT_GRAPH])
                .unwrap();
        }
        w.commit().unwrap();
    }
    let src_id = src.dataset_id();
    let src_seq = src.head_commit().seq;
    let cap = src.backup_capture("b-ann").unwrap();

    // 1. Plain `commit::reidentify` must refuse an activated directory.
    let plain_dir = tmp.path().join("plain-reid");
    cap.write_to(&plain_dir).unwrap();
    assert!(
        sparkles_core::commit::reidentify(
            &plain_dir,
            uuid::Uuid::new_v4(),
            sparkles_core::commit::ForkedFrom {
                id: src_id,
                seq: src_seq,
            },
        )
        .is_err(),
        "commit::reidentify must refuse activated stores"
    );

    // 2. Multi-generation directory must be refused by `reidentify_configuration_history`.
    let multi_dir = tmp.path().join("multi-gen");
    cap.write_to(&multi_dir).unwrap();
    std::fs::create_dir(multi_dir.join("gen-9999")).unwrap();
    assert!(
        sparkles_core::commit::reidentify_configuration_history(
            &multi_dir,
            uuid::Uuid::new_v4(),
            sparkles_core::commit::ForkedFrom {
                id: src_id,
                seq: src_seq,
            },
            &Control::none(),
        )
        .is_err(),
        "reidentify_configuration_history must refuse directories with extra generations"
    );

    // 3. Cancelled control must return Error::Cancelled cleanly.
    let cancel_dir = tmp.path().join("cancel-reid");
    cap.write_to(&cancel_dir).unwrap();
    let control = Control::none();
    control.cancel.cancel();
    assert!(matches!(
        sparkles_core::commit::reidentify_configuration_history(
            &cancel_dir,
            uuid::Uuid::new_v4(),
            sparkles_core::commit::ForkedFrom {
                id: src_id,
                seq: src_seq,
            },
            &control,
        ),
        Err(sparkles_core::Error::Cancelled)
    ));

    // 4. Successful streaming reidentification preserves all annotations and checks cleanly.
    let ok_dir = tmp.path().join("ok-reid");
    cap.write_to(&ok_dir).unwrap();
    let new_id = uuid::Uuid::new_v4();
    sparkles_core::commit::reidentify_configuration_history(
        &ok_dir,
        new_id,
        sparkles_core::commit::ForkedFrom {
            id: src_id,
            seq: src_seq,
        },
        &Control::none(),
    )
    .unwrap();
    assert_eq!(
        check(&ok_dir, &CheckOptions { quick: false })
            .unwrap()
            .errors,
        0
    );
    let reopened = Store::open(&ok_dir, StoreOptions::default()).unwrap();
    assert_eq!(reopened.dataset_id(), new_id);
    let anns = sparkles_core::annotations::read(&ok_dir).unwrap();
    assert_eq!(anns.len(), 180);
    assert!(
        std::fs::metadata(ok_dir.join(sparkles_core::annotations::FILE))
            .unwrap()
            .len()
            > 150_000,
        "annotations.bin must span multiple 64 KiB chunks"
    );
}

#[test]
fn reidentify_configuration_history_refuses_branch_table_and_preserves_bytes() {
    fn snapshot_tree(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn walk(
            base: &Path,
            dir: &Path,
            out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
        ) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let rel = path.strip_prefix(base).unwrap().to_path_buf();
                if entry.file_type().unwrap().is_dir() {
                    walk(base, &path, out);
                } else {
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    let tmp = tempfile::tempdir().unwrap();

    // Case 1: single-generation activated main store with an existing branch.
    let branched_root = tmp.path().join("branched");
    let src_id;
    let src_seq;
    {
        let s = activated_store(&branched_root);
        s.create_branch("feature", &Default::default()).unwrap();
        src_id = s.dataset_id();
        src_seq = s.head_commit().seq;
    }
    let before = snapshot_tree(&branched_root);
    let err = sparkles_core::commit::reidentify_configuration_history(
        &branched_root,
        uuid::Uuid::new_v4(),
        sparkles_core::commit::ForkedFrom {
            id: src_id,
            seq: src_seq,
        },
        &Control::none(),
    )
    .unwrap_err();
    assert!(
        matches!(err, sparkles_core::Error::Invalid(_)),
        "expected Error::Invalid refusing branch state, got {err:?}"
    );
    assert_eq!(
        snapshot_tree(&branched_root),
        before,
        "refusal must preserve every byte on disk"
    );
    let reopened = Store::open(&branched_root, StoreOptions::default()).unwrap();
    assert_eq!(reopened.dataset_id(), src_id);
    assert!(reopened.branch("feature").is_ok());
    drop(reopened);

    // Case 2: single-generation activated store with only an ordinal-reservation branches.json.
    let ordinal_root = tmp.path().join("ordinal-only");
    let ord_id;
    let ord_seq;
    {
        let s = activated_store(&ordinal_root);
        ord_id = s.dataset_id();
        ord_seq = s.head_commit().seq;
    }
    sparkles_core::commit::reserve_branch_ordinals(&ordinal_root, ord_id, 5).unwrap();
    let before_ord = snapshot_tree(&ordinal_root);
    assert!(
        sparkles_core::commit::reidentify_configuration_history(
            &ordinal_root,
            uuid::Uuid::new_v4(),
            sparkles_core::commit::ForkedFrom {
                id: ord_id,
                seq: ord_seq,
            },
            &Control::none(),
        )
        .is_err(),
        "reidentify_configuration_history must refuse branches.json ordinal table"
    );
    assert_eq!(snapshot_tree(&ordinal_root), before_ord);
    assert_eq!(
        Store::open(&ordinal_root, StoreOptions::default())
            .unwrap()
            .dataset_id(),
        ord_id
    );
}

#[test]
fn transitive_memory_clone_preserves_ancestor_provenance_dag() {
    let tmp = tempfile::tempdir().unwrap();
    let a_root = tmp.path().join("a");
    let a = activated_store(&a_root);
    let a_id = a.dataset_id();
    let a_cur = std::fs::read_to_string(a_root.join("CURRENT"))
        .unwrap()
        .trim()
        .to_string();

    // A (disk) -> B (in-memory clone) -> C (disk clone)
    let (b, _) = a
        .clone_to_memory(&Default::default(), StoreOptions::default())
        .unwrap();
    let b_id = b.dataset_id();
    let c_root = tmp.path().join("c");
    b.clone_to(&c_root, &Default::default()).unwrap();

    sparkles_core::check::imported_configuration_history(&c_root, &Control::none()).unwrap();
    let c = Store::open(&c_root, StoreOptions::default()).unwrap();
    assert_eq!(c.snapshot().len(), a.snapshot().len());

    // Inspect C's evidence-history/imports/: both B and A must be present, with A's
    // original familyId and generation preserved.
    let mut found_families = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(c_root.join("evidence-history/imports")).unwrap() {
        let entry = entry.unwrap();
        let desc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.path().join("origin.json")).unwrap())
                .unwrap();
        let fid = desc["familyId"].as_str().unwrap().to_string();
        let gen_name = desc["roots"]["main"]["generation"]
            .as_str()
            .unwrap()
            .to_string();
        found_families.insert(fid, gen_name);
    }
    assert_eq!(
        found_families.get(&a_id.to_string()).map(String::as_str),
        Some(a_cur.as_str()),
        "ancestor A must survive in C's imports: {found_families:?}"
    );
    assert!(
        found_families.contains_key(&b_id.to_string()),
        "intermediate memory clone B must survive in C's imports: {found_families:?}"
    );

    // 3-hop chain: A (disk) -> B (memory) -> B2 (memory) -> C2 (disk)
    let (b2, _) = b
        .clone_to_memory(&Default::default(), StoreOptions::default())
        .unwrap();
    let b2_id = b2.dataset_id();
    let c2_root = tmp.path().join("c2");
    b2.clone_to(&c2_root, &Default::default()).unwrap();
    sparkles_core::check::imported_configuration_history(&c2_root, &Control::none()).unwrap();
    let mut found_3 = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(c2_root.join("evidence-history/imports")).unwrap() {
        let entry = entry.unwrap();
        let desc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.path().join("origin.json")).unwrap())
                .unwrap();
        found_3.insert(desc["familyId"].as_str().unwrap().to_string());
    }
    assert!(found_3.contains(&a_id.to_string()));
    assert!(found_3.contains(&b_id.to_string()));
    assert!(found_3.contains(&b2_id.to_string()));
}

#[cfg(unix)]
#[test]
fn reidentify_configuration_history_refuses_symlinked_origin_preflight() {
    let tmp = tempfile::tempdir().unwrap();
    let src_root = tmp.path().join("src");
    let src = activated_store(&src_root);
    let src_id = src.dataset_id();
    let src_seq = src.head_commit().seq;
    let cap = src.backup_capture("b-sym-preflight").unwrap();
    drop(src);

    let sym_dir = tmp.path().join("sym-origin");
    cap.write_to(&sym_dir).unwrap();
    let decoy = tmp.path().join("decoy-origin.json");
    std::fs::write(&decoy, b"{\"format\":99}").unwrap();
    std::os::unix::fs::symlink(&decoy, sym_dir.join("evidence-history/origin.json")).unwrap();
    let err = sparkles_core::commit::reidentify_configuration_history(
        &sym_dir,
        uuid::Uuid::new_v4(),
        sparkles_core::commit::ForkedFrom {
            id: src_id,
            seq: src_seq,
        },
        &Control::none(),
    )
    .unwrap_err();
    // Must fail via anchored O_NOFOLLOW (not Error::Unsupported from unanchored format probe).
    assert!(
        !matches!(err, sparkles_core::Error::Unsupported(_)),
        "anchored preflight must refuse symlinked origin before decoding format, got {err:?}"
    );
}
