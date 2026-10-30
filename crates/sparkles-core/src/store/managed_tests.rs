//! The managed-graph boundary (spec C25 §5 and §7): every ordinary write path refuses
//! to change a managed graph, a capability admits its owner's writes only, the
//! registry fails closed when it cannot be enforced, and enabling it is crash safe.

use super::*;
use crate::branch::{BranchOptions, MergeOptions};
use crate::guard::WriteOptions;
use crate::io::{RdfFormat, Source};
use crate::managed::{ManagedErrorKind, OwnerEntry, OwnerKind};
use oxrdf::{Literal, NamedNode, Term};

const M: &str = "urn:managed";
const T: &str = "urn:test-owned";
const SETTINGS: &[u8] = br#"{"format":1,"profile":1}"#;

fn entry(graphs: &[&str], settings: &[u8]) -> OwnerEntry {
    OwnerEntry::for_settings(1, graphs.iter().map(|g| g.to_string()).collect(), settings)
}

fn enable(s: &Store, graphs: &[&str]) -> crate::managed::ManagedGraphs {
    s.enable_managed(OwnerKind::Evidence, entry(graphs, SETTINGS), SETTINGS)
        .unwrap()
}

fn managed_code(r: Result<impl std::fmt::Debug>) -> &'static str {
    match r {
        Err(Error::Managed(m)) => m.code,
        r => panic!("not a managed-graph refusal: {r:?}"),
    }
}

fn upd(s: &Store, update: &str) -> Result<crate::sparql::update::UpdateStats> {
    crate::sparql::update::update(s, update, &crate::sparql::QueryOptions::default())
}

fn nq(lines: &str) -> Source {
    Source::from_bytes(lines.as_bytes().to_vec(), RdfFormat::NQuads, None)
}

/// Insert `<urn:s> <urn:p> "v"` into graph `g` (`None`: the default graph).
fn put(txn: &mut WriteTxn<'_>, g: Option<&str>, v: &str) {
    let s = txn
        .intern(&Term::NamedNode(NamedNode::new("urn:s").unwrap()))
        .unwrap();
    let p = txn
        .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
        .unwrap();
    let o = txn
        .intern(&Term::Literal(Literal::new_simple_literal(v)))
        .unwrap();
    let g = match g {
        Some(g) => txn
            .intern(&Term::NamedNode(NamedNode::new(g).unwrap()))
            .unwrap(),
        None => Id::DEFAULT_GRAPH,
    };
    txn.insert([s, p, o, g]).unwrap();
}

fn count(s: &Store, g: &str) -> u64 {
    let snap = s.snapshot();
    snap.lookup_iri(g)
        .map_or(0, |g| snap.count(Perm::Gspo, &[g.0]).unwrap())
}

/// A persistent dataset with a quad in `M`, the default graph and `urn:free`, and a
/// branch `dev` created before anything is managed.
fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    s.load(&[nq(&format!(
        "<urn:s> <urn:p> \"m\" <{M}> .\n<urn:s> <urn:p> \"d\" .\n<urn:s> <urn:p> \"f\" <urn:free> .\n"
    ))])
    .unwrap();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    (dir, s)
}

#[test]
fn sparql_updates_cannot_change_a_managed_graph() {
    let (_dir, s) = setup();
    enable(&s, &[M]);
    let head = s.head_commit().seq;
    assert_eq!(
        managed_code(upd(
            &s,
            &format!("INSERT DATA {{ GRAPH <{M}> {{ <urn:x> <urn:p> 1 }} }}")
        )),
        "profile-managed-graph"
    );
    assert_eq!(
        managed_code(upd(
            &s,
            &format!("DELETE WHERE {{ GRAPH <{M}> {{ ?s ?p ?o }} }}")
        )),
        "profile-managed-graph"
    );
    assert_eq!(
        managed_code(upd(&s, &format!("DROP GRAPH <{M}>"))),
        "profile-managed-graph"
    );
    assert_eq!(managed_code(upd(&s, "CLEAR ALL")), "profile-managed-graph");
    assert_eq!((s.head_commit().seq, count(&s, M)), (head, 1));
    // other graphs stay writable
    upd(
        &s,
        "INSERT DATA { GRAPH <urn:free> { <urn:x> <urn:p> 1 } <urn:x> <urn:p> 2 }",
    )
    .unwrap();
    assert_eq!(count(&s, "urn:free"), 2);
}

#[test]
fn validation_bypass_and_dry_runs_meet_the_boundary() {
    let (_dir, s) = setup();
    enable(&s, &[M]);
    let mut t = s.write_with(
        CommitKind::Transaction,
        WriteOptions {
            bypass_validation: true,
            ..Default::default()
        },
    );
    put(&mut t, Some(M), "bypassed");
    assert_eq!(managed_code(t.commit()), "profile-managed-graph");
    // a preview shows the refusal the commit would meet
    let mut t = s.write_with(
        CommitKind::Transaction,
        WriteOptions {
            dry_run: Some(Default::default()),
            ..Default::default()
        },
    );
    put(&mut t, Some(M), "previewed");
    assert_eq!(managed_code(t.commit()), "profile-managed-graph");
    assert_eq!(count(&s, M), 1);
}

#[test]
fn bulk_loads_and_replacements_are_refused() {
    let s = Store::in_memory(StoreOptions::default());
    enable(&s, &[M]);
    // a load into an empty store is a bulk rebuild, refused as the build reads the quad
    let r = s.load(&[nq(&format!(
        "<urn:a> <urn:p> \"1\" .\n<urn:s> <urn:p> \"x\" <{M}> .\n"
    ))]);
    assert_eq!(managed_code(r), "profile-managed-graph");
    assert!(s.snapshot().is_empty());
    s.load(&[nq("<urn:a> <urn:p> \"1\" <urn:free> .\n")])
        .unwrap();
    // a bulk replacement of the managed graph or of everything
    let opts = StoreOptions {
        bulk_threshold: 1,
        ..Default::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let b = Store::open(&dir.path().join("b"), opts).unwrap();
    b.load(&[nq(&format!("<urn:s> <urn:p> \"m\" <{M}> .\n"))])
        .unwrap();
    enable(&b, &[M]);
    let source = || nq("<urn:s> <urn:p> \"new\" .\n<urn:s> <urn:p> \"new2\" .\n");
    assert_eq!(
        managed_code(b.replace(ReplaceTarget::All, &[source()])),
        "profile-managed-graph"
    );
    assert_eq!(
        managed_code(b.replace(
            ReplaceTarget::Named(NamedNode::new(M).unwrap()),
            &[source()]
        )),
        "profile-managed-graph"
    );
    assert_eq!(
        managed_code(b.load(&[nq(&format!(
            "<urn:s> <urn:p> \"a\" <{M}> .\n<urn:s> <urn:p> \"b\" <{M}> .\n"
        ))])),
        "profile-managed-graph"
    );
    b.replace(ReplaceTarget::Default, &[source()]).unwrap();
    assert_eq!(count(&b, M), 1);
}

#[test]
fn rdf_patches_are_refused() {
    let (_dir, s) = setup();
    enable(&s, &[M]);
    let patch = format!("A <urn:s> <urn:p> \"patched\" <{M}> .\n");
    assert_eq!(
        managed_code(s.apply_patch(patch.as_bytes(), &Default::default())),
        "profile-managed-graph"
    );
    let patch = format!("D <urn:s> <urn:p> \"m\" <{M}> .\n");
    assert_eq!(
        managed_code(s.apply_patch(patch.as_bytes(), &Default::default())),
        "profile-managed-graph"
    );
    assert_eq!(count(&s, M), 1);
}

#[test]
fn branches_merges_and_replays_carry_no_exemption() {
    let (_dir, s) = setup();
    let dev = s.branch("dev").unwrap();
    // dev was created before the registry, and changed the graph before it too
    let mut t = dev.write();
    put(&mut t, Some(M), "from dev");
    let picked = t.commit().unwrap().commit.seq;
    enable(&s, &[M]);
    // the branch shares main's registry
    let mut t = dev.write();
    put(&mut t, Some(M), "after");
    assert_eq!(managed_code(t.commit()), "profile-managed-graph");
    // a branch created after the registry is protected as well
    s.create_branch("late", &BranchOptions::default()).unwrap();
    let late = s.branch("late").unwrap();
    assert_eq!(
        managed_code(upd(
            &late,
            &format!("INSERT DATA {{ GRAPH <{M}> {{ <urn:x> <urn:p> 1 }} }}")
        )),
        "profile-managed-graph"
    );
    let head = s.head_commit().seq;
    let o = MergeOptions::default();
    assert_eq!(
        managed_code(s.preview_merge("dev", "main", &o)),
        "profile-managed-graph"
    );
    assert_eq!(
        managed_code(s.merge("dev", "main", &o)),
        "profile-managed-graph"
    );
    let squash = MergeOptions {
        squash: true,
        ..Default::default()
    };
    assert_eq!(
        managed_code(s.merge("dev", "main", &squash)),
        "profile-managed-graph"
    );
    let replay = MergeOptions {
        replay: true,
        ..Default::default()
    };
    assert_eq!(
        managed_code(s.merge("dev", "main", &replay)),
        "profile-managed-graph"
    );
    let replay_dry = MergeOptions {
        replay: true,
        write: WriteOptions {
            dry_run: Some(Default::default()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        managed_code(s.merge("dev", "main", &replay_dry)),
        "profile-managed-graph"
    );
    assert_eq!(
        managed_code(s.cherry_pick("dev", picked, "main", &o)),
        "profile-managed-graph"
    );
    assert_eq!(
        managed_code(s.preview_cherry_pick("dev", picked, "main", &o)),
        "profile-managed-graph"
    );
    assert_eq!((s.head_commit().seq, count(&s, M)), (head, 1));
}

#[test]
fn capabilities_admit_only_their_owner_and_registry() {
    let (_dir, s) = setup();
    enable(&s, &[M]);
    s.enable_managed(OwnerKind::Test, entry(&[T], b"test"), b"test")
        .unwrap();
    let op = uuid::Uuid::new_v4();
    // the owner's typed write
    let mut t = s.write();
    t.set_managed(s.managed_writer(OwnerKind::Evidence, op).unwrap())
        .unwrap();
    put(&mut t, Some(M), "typed");
    put(&mut t, Some("urn:free"), "typed");
    assert!(t.commit().unwrap().committed);
    assert_eq!(count(&s, M), 2);
    // another owner's capability
    let mut t = s.write();
    t.set_managed(s.managed_writer(OwnerKind::Test, op).unwrap())
        .unwrap();
    put(&mut t, Some(M), "wrong owner");
    assert_eq!(managed_code(t.commit()), "profile-managed-graph");
    // a capability of another dataset
    let other = Store::in_memory(StoreOptions::default());
    enable(&other, &[M]);
    let foreign = other.managed_writer(OwnerKind::Evidence, op).unwrap();
    let mut t = s.write();
    t.set_managed(foreign).unwrap();
    put(&mut t, Some(M), "foreign");
    assert_eq!(managed_code(t.commit()), "profile-managed-graph");
    // a capability of a registry that changed since
    let stale = s.managed_writer(OwnerKind::Evidence, op).unwrap();
    s.detach_managed(OwnerKind::Test, T).unwrap();
    let mut t = s.write();
    t.set_managed(stale).unwrap();
    put(&mut t, Some(M), "stale");
    assert_eq!(managed_code(t.commit()), "profile-managed-graph");
    // an owner that manages nothing here has no capability
    let unmanaged = Store::in_memory(StoreOptions::default());
    assert_eq!(
        managed_code(unmanaged.managed_writer(OwnerKind::Evidence, op)),
        "unsupported-profile"
    );
    // a capability on a dataset without a registry is foreign too
    let mut t = unmanaged.write();
    t.set_managed(s.managed_writer(OwnerKind::Evidence, op).unwrap())
        .unwrap();
    put(&mut t, None, "x");
    assert_eq!(managed_code(t.commit()), "profile-managed-graph");
    // a capability is set before any change
    let mut t = s.write();
    put(&mut t, Some("urn:free"), "first");
    assert!(
        t.set_managed(s.managed_writer(OwnerKind::Evidence, op).unwrap())
            .is_err()
    );
    drop(t);
    // reserved entry members are refused
    let mut hidden = entry(&[M], SETTINGS);
    hidden.visibility = crate::managed::Visibility::Hidden;
    assert_eq!(
        managed_code(s.enable_managed(OwnerKind::Evidence, hidden, SETTINGS)),
        "unsupported-profile"
    );
    let mut exclude = entry(&[M], SETTINGS);
    exclude.merge = crate::managed::MergePolicy::Exclude;
    assert_eq!(
        managed_code(s.enable_managed(OwnerKind::Evidence, exclude, SETTINGS)),
        "unsupported-profile"
    );
    // a detached graph takes ordinary writes again
    upd(
        &s,
        &format!("INSERT DATA {{ GRAPH <{T}> {{ <urn:x> <urn:p> 1 }} }}"),
    )
    .unwrap();
    assert_eq!(count(&s, M), 2);
    let m = s.managed_graphs().unwrap().unwrap();
    assert_eq!(m.owner_of(M), Some(OwnerKind::Evidence));
    assert_eq!(m.owner_of(T), None);
}

#[test]
fn a_stamp_names_the_commit_exactly() {
    for group_commit in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(
            &dir.path().join("ds"),
            StoreOptions {
                group_commit,
                ..Default::default()
            },
        )
        .unwrap();
        enable(&s, &[M]);
        let tick = Arc::new(std::sync::atomic::AtomicI64::new(1_000_000));
        let clock = tick.clone();
        s.set_clock(Arc::new(move || clock.fetch_add(1000, Ordering::SeqCst)));
        upd(&s, "INSERT DATA { <urn:x> <urn:p> 0 }").unwrap();
        // writers wait for the lock while the stamped transaction holds it, which
        // would make an ordinary commit join a group
        let (seq, ts, r) = std::thread::scope(|scope| {
            let mut t = s.write();
            assert!(t.managed_stamp().is_err(), "a stamp needs a capability");
            t.set_managed(
                s.managed_writer(OwnerKind::Evidence, uuid::Uuid::new_v4())
                    .unwrap(),
            )
            .unwrap();
            let (seq, ts) = t.managed_stamp().unwrap();
            for i in 1..4 {
                let s = &s;
                scope.spawn(move || {
                    upd(&s, &format!("INSERT DATA {{ <urn:x{i}> <urn:p> {i} }}")).unwrap();
                });
            }
            while s.writers_waiting() < 3 {
                std::thread::yield_now();
            }
            assert_eq!(t.managed_stamp().unwrap(), (seq, ts));
            put(&mut t, Some(M), "stamped");
            (seq, ts, t.commit().unwrap())
        });
        assert_eq!((r.commit.seq, r.commit.timestamp_ms), (seq, ts));
        assert_eq!(s.commit(seq).unwrap().timestamp_ms, ts);
        assert_eq!(s.head_commit().seq, seq + 3);
    }
}

#[test]
fn enabling_raises_the_reader_and_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let before = crate::commit::read_dataset_versions(&root)
        .unwrap()
        .unwrap();
    assert!(before.minimum_reader < crate::managed::READER);
    enable(&s, &[M]);
    let after = crate::commit::read_dataset_versions(&root)
        .unwrap()
        .unwrap();
    assert_eq!(after.minimum_reader, 6);
    // a release that reads up to reader 5 refuses the dataset
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("dataset.json")).unwrap()).unwrap();
    assert!(matches!(
        crate::commit::check_dataset_value_as(&v, 5),
        Err(Error::Unsupported(_))
    ));
    crate::commit::check_dataset_value_as(&v, 6).unwrap();
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("managed.json")).unwrap()).unwrap();
    assert_eq!(file["format"], 1);
    assert_eq!(file["revision"], 1);
    assert_eq!(file["owners"]["evidence"]["graphs"][0], M);
    assert_eq!(std::fs::read(root.join("evidence.json")).unwrap(), SETTINGS);
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(
        managed_code(upd(
            &s,
            &format!("INSERT DATA {{ GRAPH <{M}> {{ <urn:x> <urn:p> 1 }} }}")
        )),
        "profile-managed-graph"
    );
    // a branch of the reopened dataset shares it, and does not copy the files
    s.create_branch("b", &BranchOptions::default()).unwrap();
    let b = s.branch("b").unwrap();
    assert_eq!(
        managed_code(upd(
            &b,
            &format!("INSERT DATA {{ GRAPH <{M}> {{ <urn:x> <urn:p> 1 }} }}")
        )),
        "profile-managed-graph"
    );
    assert!(!b.root().unwrap().join("managed.json").exists());
    assert!(
        b.enable_managed(OwnerKind::Evidence, entry(&[M], SETTINGS), SETTINGS)
            .is_err()
    );
}

/// Rewrite `managed.json` (or the owner's settings) of a closed dataset at `root`.
fn edit(root: &Path, name: &str, f: impl FnOnce(&mut serde_json::Value)) {
    let path = root.join(name);
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    f(&mut v);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
}

#[test]
fn an_unenforceable_registry_refuses_writes_and_keeps_reads() {
    type Edit = fn(&mut serde_json::Value);
    let cases: [(&str, &str, Edit, bool); 5] = [
        (
            "managed.json",
            "newer format",
            |v| v["format"] = 2.into(),
            false,
        ),
        (
            "managed.json",
            "unknown owner",
            |v| v["owners"]["jobs"] = v["owners"]["evidence"].clone(),
            false,
        ),
        (
            "managed.json",
            "unknown profile",
            |v| v["owners"]["evidence"]["profile"] = 9.into(),
            false,
        ),
        (
            "evidence.json",
            "changed settings",
            |v| v["profile"] = 2.into(),
            true,
        ),
        (
            "managed.json",
            "changed digest",
            |v| v["owners"]["evidence"]["settingsDigest"] = "sha256:00".into(),
            true,
        ),
    ];
    for (file, what, change, repairable) in cases {
        let (dir, s) = setup();
        enable(&s, &[M]);
        drop(s);
        let root = dir.path().join("ds");
        edit(&root, file, change);
        let s = Store::open(&root, StoreOptions::default())
            .unwrap_or_else(|e| panic!("{what}: the open is refused: {e}"));
        // reads continue
        assert_eq!(count(&s, M), 1, "{what}");
        assert_eq!(count(&s, "urn:free"), 1, "{what}");
        // every write of main and its branches fails closed
        let dev = s.branch("dev").unwrap();
        for store in [&s, &*dev] {
            let r = upd(
                &store,
                "INSERT DATA { GRAPH <urn:free> { <urn:x> <urn:p> 1 } }",
            );
            assert_eq!(managed_code(r), "managed-unavailable", "{what}");
            assert_eq!(
                managed_code(store.load(&[nq("<urn:x> <urn:p> \"1\" <urn:free> .\n")])),
                "managed-unavailable",
                "{what}"
            );
        }
        match s.managed_graphs() {
            Err(Error::Managed(m)) => assert_eq!(m.kind, ManagedErrorKind::Unavailable),
            r => panic!("{what}: {r:?}"),
        }
        let again = s.enable_managed(OwnerKind::Evidence, entry(&[M], SETTINGS), SETTINGS);
        if repairable {
            again.unwrap();
            upd(&s, "INSERT DATA { GRAPH <urn:free> { <urn:x> <urn:p> 1 } }").unwrap();
        } else {
            assert_eq!(managed_code(again), "managed-unavailable", "{what}");
        }
    }
}

/// Enable on a closed copy of `setup`'s dataset with a panic at `failpoint`, as a crash
/// there would stop it, then reopen.
fn crash_at(failpoint: &'static str, first: &[u8], second: &[u8]) -> (tempfile::TempDir, Store) {
    let (dir, s) = setup();
    if !first.is_empty() {
        s.enable_managed(OwnerKind::Evidence, entry(&[M], first), first)
            .unwrap();
    }
    s.set_failpoint(failpoint, Some(Arc::new(|_| panic!("crash"))));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.enable_managed(OwnerKind::Evidence, entry(&[M, T], second), second)
    }));
    assert!(r.is_err(), "{failpoint} was reached");
    s.set_failpoint(failpoint, None);
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    (dir, s)
}

#[test]
fn an_interrupted_enablement_recovers_safely() {
    let insert_m = format!("INSERT DATA {{ GRAPH <{M}> {{ <urn:x> <urn:p> 1 }} }}");
    let insert_t = format!("INSERT DATA {{ GRAPH <{T}> {{ <urn:x> <urn:p> 1 }} }}");
    // before managed.json: a first enablement leaves the dataset unmanaged, with the
    // reader raised, which is safe to resume
    for fp in ["managed-reader-raised", "managed-settings-written"] {
        let (dir, s) = crash_at(fp, b"", SETTINGS);
        assert_eq!(s.managed_graphs().unwrap(), None, "{fp}");
        let v = crate::commit::read_dataset_versions(&dir.path().join("ds"))
            .unwrap()
            .unwrap();
        assert_eq!(v.minimum_reader, 6, "{fp}");
        upd(&s, &insert_m.replace("1", "0")).unwrap();
        enable(&s, &[M]);
        assert_eq!(managed_code(upd(&s, &insert_m)), "profile-managed-graph");
    }
    // a second enablement whose settings did not change keeps the old registry
    let (_dir, s) = crash_at("managed-settings-written", SETTINGS, SETTINGS);
    let m = s.managed_graphs().unwrap().unwrap();
    assert_eq!((m.revision, m.owner_of(T)), (1, None));
    assert_eq!(managed_code(upd(&s, &insert_m)), "profile-managed-graph");
    // one whose settings changed fails closed until the owner is enabled again
    let (_dir, s) = crash_at("managed-settings-written", b"old", b"new");
    assert_eq!(managed_code(upd(&s, &insert_t)), "managed-unavailable");
    s.enable_managed(OwnerKind::Evidence, entry(&[M, T], b"new"), b"new")
        .unwrap();
    assert_eq!(managed_code(upd(&s, &insert_t)), "profile-managed-graph");
    // after the commit point, the new registry holds
    let (_dir, s) = crash_at("managed-published", SETTINGS, SETTINGS);
    let m = s.managed_graphs().unwrap().unwrap();
    assert_eq!((m.revision, m.owner_of(T)), (2, Some(OwnerKind::Evidence)));
    assert_eq!(managed_code(upd(&s, &insert_t)), "profile-managed-graph");
}

#[test]
fn backups_and_clones_keep_the_protection() {
    let (dir, s) = setup();
    enable(&s, &[M]);
    let insert = format!("INSERT DATA {{ GRAPH <{M}> {{ <urn:x> <urn:p> 1 }} }}");
    // a backup restored as a directory
    let cap = s.backup_capture("t").unwrap();
    assert!(cap.file("managed.json").is_some() && cap.file("evidence.json").is_some());
    let restored = dir.path().join("restored");
    cap.write_to(&restored).unwrap();
    drop(cap);
    let r = Store::open(&restored, StoreOptions::default()).unwrap();
    assert_eq!(managed_code(upd(&r, &insert)), "profile-managed-graph");
    assert_eq!(
        crate::commit::read_dataset_versions(&restored)
            .unwrap()
            .unwrap()
            .minimum_reader,
        6
    );
    // a persistent and an in-memory clone
    let cloned = dir.path().join("cloned");
    s.clone_to(&cloned, &Default::default()).unwrap();
    let c = Store::open(&cloned, StoreOptions::default()).unwrap();
    assert_eq!(managed_code(upd(&c, &insert)), "profile-managed-graph");
    let (m, _) = s
        .clone_to_memory(&Default::default(), StoreOptions::default())
        .unwrap();
    assert_eq!(managed_code(upd(&m, &insert)), "profile-managed-graph");
    // an in-memory store's registry is in memory, and its backup carries it
    let mem = Store::in_memory(StoreOptions::default());
    mem.load(&[nq("<urn:a> <urn:p> \"1\" .\n")]).unwrap();
    enable(&mem, &[M]);
    assert_eq!(managed_code(upd(&mem, &insert)), "profile-managed-graph");
    let cap = mem
        .memory_backup_capture(
            "m",
            &backup::MemoryCaptureOptions {
                tmp_dir: dir.path().join("tmp"),
                ..Default::default()
            },
        )
        .unwrap();
    let from_memory = dir.path().join("from-memory");
    cap.write_to(&from_memory).unwrap();
    drop(cap);
    let r = Store::open(&from_memory, StoreOptions::default()).unwrap();
    assert_eq!(managed_code(upd(&r, &insert)), "profile-managed-graph");
}
