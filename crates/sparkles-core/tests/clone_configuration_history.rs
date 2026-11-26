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

    // 2. Lowered minimumReader (< 10) with format 2 origin.json -> Error::Unsupported.
    let low_reader = tmp.path().join("low-reader");
    copy_dir_recursive(&good, &low_reader);
    let mut ds: serde_json::Value =
        serde_json::from_slice(&std::fs::read(low_reader.join("dataset.json")).unwrap()).unwrap();
    ds["minimumReader"] = 9.into();
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
