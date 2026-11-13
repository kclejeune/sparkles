//! The control keyspace of the store (spec C26 §2.1 and §7): its two write paths, the
//! uncertainty guard, checkpoints and recovery at open, with a test for each crash point
//! of the §7.2 table that the core owns. The rows about claims, publications and
//! deliveries are the jobs library's.

use super::*;
use crate::control::{self, ControlState, Frame, Op};
use oxrdf::NamedNode;

fn open(dir: &Path) -> Result<Store> {
    Store::open(dir, StoreOptions::default())
}

fn setup() -> (tempfile::TempDir, PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let s = open(&root).unwrap();
    s.enable_control().unwrap();
    (dir, root, s)
}

fn quad(txn: &mut WriteTxn<'_>, v: &str) -> [Id; 4] {
    let mut id = |iri: &str| {
        txn.intern(&Term::NamedNode(NamedNode::new(iri).unwrap()))
            .unwrap()
    };
    [
        id("urn:s"),
        id("urn:p"),
        id(&format!("urn:{v}")),
        Id::DEFAULT_GRAPH,
    ]
}

fn value(v: &str) -> Arc<[u8]> {
    v.as_bytes().into()
}

/// Commit `puts` as control records, with a quad `<urn:s> <urn:p> <urn:{with}>` when
/// `with` is given.
fn transition(s: &Store, puts: &[(&str, &str)], with: Option<&str>) -> Result<Receipt> {
    let cap = s.control_writer(ControlOwner::Jobs)?;
    let mut txn = s.write();
    txn.set_control(cap)?;
    for (k, v) in puts {
        txn.control_put(k.as_bytes(), value(v))?;
    }
    if let Some(v) = with {
        let q = quad(&mut txn, v);
        txn.insert(q)?;
    }
    txn.commit()
}

fn get(s: &Store, key: &str) -> Option<String> {
    s.control_state().and_then(|c| {
        c.get(key.as_bytes())
            .map(|v| String::from_utf8(v.to_vec()).unwrap())
    })
}

fn seq(s: &Store) -> u64 {
    s.control_state().unwrap().seq
}

/// Copy the dataset at `root` to `to` as it is on disk now, which is what a process that
/// stopped at this moment leaves (the page cache survives a process crash).
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), &t).unwrap();
        }
    }
}

/// Run `op` with a hook at `at` that copies the dataset as a crash there would leave it,
/// let `op` finish, and open the copy.
fn crash_copy<T>(s: &Store, root: &Path, at: &'static str, op: impl FnOnce() -> T) -> (T, Store) {
    let to = root.with_file_name(format!("crash-{at}"));
    let (from, dest) = (root.to_path_buf(), to.clone());
    let hit = Arc::new(AtomicBool::new(false));
    let h = hit.clone();
    s.set_failpoint(
        at,
        Some(Arc::new(move |_| {
            if !h.swap(true, Ordering::SeqCst) {
                copy_tree(&from, &dest);
            }
        })),
    );
    let out = op();
    s.set_failpoint(at, None);
    assert!(hit.load(Ordering::SeqCst), "{at} was reached");
    (out, open(&to).unwrap())
}

/// Run `op` with a panic at `at`, as an unwind there would stop it.
fn unwind_at<T>(s: &Store, at: &'static str, op: impl FnOnce() -> T) {
    s.set_failpoint(at, Some(Arc::new(|_| panic!("unwind"))));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(op));
    s.set_failpoint(at, None);
    assert!(r.is_err(), "{at} was reached");
}

fn integrity(r: Result<Store>) -> String {
    match r {
        Err(Error::Corrupt(m)) => m,
        Err(e) => panic!("not an integrity error: {e}"),
        Ok(_) => panic!("the store opened"),
    }
}

#[test]
fn both_write_paths_survive_a_reopen() {
    let (_dir, root, s) = setup();
    assert_eq!(seq(&s), 0);
    // a transaction with quads is one commit whose log record carries the frame
    let r = transition(&s, &[("jobs/run/a", "1")], Some("a")).unwrap();
    assert!(r.committed);
    assert_eq!((r.commit.seq, r.commit.inserted), (1, 1));
    // one without is a control-only frame and not a commit
    let r = transition(&s, &[("jobs/run/b", "2")], None).unwrap();
    assert!(!r.committed);
    assert_eq!(r.commit.seq, 1);
    assert_eq!(s.snapshot().commit, 1);
    assert_eq!(s.control_state().unwrap().follows, 1);
    assert_eq!(
        (get(&s, "jobs/run/a"), get(&s, "jobs/run/b")),
        (Some("1".into()), Some("2".into()))
    );
    assert_eq!(seq(&s), 2);
    drop(s);
    let s = open(&root).unwrap();
    assert_eq!(seq(&s), 2);
    assert_eq!(
        (get(&s, "jobs/run/a"), get(&s, "jobs/run/b")),
        (Some("1".into()), Some("2".into()))
    );
    // the commit counts only its quads, and the history shows no control-only frame
    assert_eq!(s.snapshot().commit, 1);
    assert_eq!(s.snapshot().len(), 1);
    // later transitions continue the sequence
    transition(&s, &[("jobs/run/c", "3")], None).unwrap();
    assert_eq!(seq(&s), 3);
}

#[test]
fn a_transaction_reads_its_own_staged_records_and_only_its_owners_keys() {
    let (_dir, _root, s) = setup();
    transition(
        &s,
        &[("jobs/state/q/1", "x"), ("jobs/state/q/3", "y")],
        None,
    )
    .unwrap();
    let cap = s.control_writer(ControlOwner::Jobs).unwrap();
    let mut txn = s.write();
    txn.set_control(cap).unwrap();
    txn.control_put(b"jobs/state/q/2", value("z")).unwrap();
    txn.control_delete(b"jobs/state/q/3").unwrap();
    let (got, more) = txn.control_prefix(b"jobs/state/q/", 10).unwrap();
    let keys: Vec<_> = got
        .iter()
        .map(|(k, _)| String::from_utf8(k.to_vec()).unwrap())
        .collect();
    assert_eq!(keys, ["jobs/state/q/1", "jobs/state/q/2"]);
    assert!(!more);
    let (got, more) = txn.control_prefix(b"jobs/state/q/", 1).unwrap();
    assert_eq!((got.len(), more), (1, true));
    assert_eq!(txn.control_get(b"jobs/state/q/3").unwrap(), None);
    // the core's own keys and other prefixes are not the owner's
    assert!(txn.control_put(b"core/origin", value("x")).is_err());
    assert!(txn.control_put(b"other/x", value("x")).is_err());
    let stamp = txn.control_stamp().unwrap();
    assert_eq!((stamp.seq, stamp.head, stamp.commit), (2, 0, None));
    // the published state does not see the staged records
    assert_eq!(get(&s, "jobs/state/q/2"), None);
    drop(txn);
    assert_eq!(get(&s, "jobs/state/q/3"), Some("y".into()));
    assert_eq!(seq(&s), 1);
}

#[test]
fn control_records_are_refused_without_a_keyspace_in_a_dry_run_and_with_bulk_quads() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(&dir.path().join("ds")).unwrap();
    assert!(!s.control_enabled());
    assert!(s.control_writer(ControlOwner::Jobs).is_err());
    s.enable_control().unwrap();
    let cap = s.control_writer(ControlOwner::Jobs).unwrap();
    let opts = crate::guard::WriteOptions {
        dry_run: Some(Default::default()),
        ..Default::default()
    };
    let mut txn = s.write_with(CommitKind::Update, opts);
    assert!(txn.set_control(cap).is_err());
    drop(txn);
    drop(s);
    // a bulk batch, which the store applies by a rebuild, takes no control records, and
    // a transaction that stages control records takes its batches through the log
    let s = Store::open(&dir.path().join("ds"), bulk_options()).unwrap();
    let mut txn = s.write();
    let q = quad(&mut txn, "bulk");
    txn.insert_bulk(vec![q]).unwrap();
    assert!(txn.set_control(cap).is_err());
    drop(txn);
    let mut txn = s.write();
    txn.set_control(cap).unwrap();
    txn.control_put(b"jobs/x", value("1")).unwrap();
    let q = quad(&mut txn, "bulk");
    txn.insert_bulk(vec![q]).unwrap();
    let generation = s.snapshot().generation.name.clone();
    assert!(txn.commit().unwrap().committed);
    assert_eq!(s.snapshot().generation.name, generation);
    assert_eq!(get(&s, "jobs/x"), Some("1".into()));
}

fn bulk_options() -> StoreOptions {
    StoreOptions {
        bulk_threshold: 1,
        ..Default::default()
    }
}

#[test]
fn an_in_memory_store_keeps_its_control_state() {
    let s = Store::in_memory(StoreOptions::default());
    s.enable_control().unwrap();
    transition(&s, &[("jobs/a", "1")], Some("a")).unwrap();
    transition(&s, &[("jobs/b", "2")], None).unwrap();
    assert_eq!(seq(&s), 2);
    assert_eq!(get(&s, "jobs/b"), Some("2".into()));
}

// ------------------------------------------------------------- §7.2 crash points ----

/// Before a frame's log write the transaction is absent, during the write or before the
/// synchronization returns it is wholly present or absent, and after it, it is present.
#[test]
fn a_crash_around_a_control_only_frame_keeps_or_drops_it_whole() {
    for (at, kept) in [
        ("control-before-append", false),
        ("control-appended", true),
        ("control-synced", true),
        ("control-published", true),
    ] {
        let (_dir, root, s) = setup();
        transition(&s, &[("jobs/a", "1")], None).unwrap();
        let (r, c) = crash_copy(&s, &root, at, || transition(&s, &[("jobs/b", "2")], None));
        r.unwrap();
        assert_eq!(get(&c, "jobs/a"), Some("1".into()), "{at}");
        assert_eq!(get(&c, "jobs/b").is_some(), kept, "{at}");
        assert_eq!(seq(&c), 1 + u64::from(kept), "{at}");
        // the recovered store writes on from there without reusing a number
        transition(&c, &[("jobs/c", "3")], None).unwrap();
        assert_eq!(seq(&c), 2 + u64::from(kept), "{at}");
        drop(c);
        let root = root.with_file_name(format!("crash-{at}"));
        assert_eq!(seq(&open(&root).unwrap()), 2 + u64::from(kept), "{at}");
    }
}

/// A commit's quads and the frame it carries are durable together.
#[test]
fn a_crash_around_a_commit_with_a_frame_keeps_or_drops_both() {
    for (at, kept) in [
        ("control-commit-armed", false),
        ("control-commit-synced", true),
    ] {
        let (_dir, root, s) = setup();
        let (r, c) = crash_copy(&s, &root, at, || {
            transition(&s, &[("jobs/a", "1")], Some("a"))
        });
        r.unwrap();
        assert_eq!(get(&c, "jobs/a").is_some(), kept, "{at}");
        assert_eq!(c.snapshot().commit, u64::from(kept), "{at}");
        assert_eq!(c.snapshot().len(), u64::from(kept), "{at}");
    }
}

/// A torn last record of the control log is cut by a writable open, after which the
/// store writes on.
#[test]
fn a_torn_control_log_tail_is_truncated() {
    let (_dir, root, s) = setup();
    transition(&s, &[("jobs/a", "1")], None).unwrap();
    drop(s);
    let path = root.join(control::LOG_FILE);
    let good = std::fs::metadata(&path).unwrap().len();
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    let frame = Frame {
        seq: 2,
        follows: 0,
        time_ms: 0,
        ops: vec![Op::Put(b"jobs/b".as_slice().into(), value("2"))],
    };
    let rec = control::log_record(&frame.encode().unwrap());
    f.write_all(&rec[..rec.len() - 3]).unwrap();
    drop(f);
    let s = open(&root).unwrap();
    assert_eq!((seq(&s), get(&s, "jobs/b")), (1, None));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), good);
    transition(&s, &[("jobs/c", "3")], None).unwrap();
    drop(s);
    assert_eq!(seq(&open(&root).unwrap()), 2);
}

/// An unwind between a log write and publication poisons the writer, and the next open
/// recovers the frame that was durable.
#[test]
fn an_unwind_before_publication_poisons_the_writer() {
    for at in ["control-synced", "control-commit-synced"] {
        let (_dir, root, s) = setup();
        let with = (at == "control-commit-synced").then_some("a");
        unwind_at(&s, at, || transition(&s, &[("jobs/a", "1")], with));
        assert!(
            matches!(
                transition(&s, &[("jobs/b", "2")], None),
                Err(Error::Poisoned)
            ),
            "{at}"
        );
        // a quad write is refused too, from its first new term on
        let mut txn = s.write();
        let term = Term::NamedNode(NamedNode::new("urn:b").unwrap());
        assert!(matches!(txn.intern(&term), Err(Error::Poisoned)), "{at}");
        drop(txn);
        let len = std::fs::metadata(root.join(control::LOG_FILE))
            .unwrap()
            .len();
        drop(s);
        // a poisoned close trims neither log
        assert_eq!(
            std::fs::metadata(root.join(control::LOG_FILE))
                .unwrap()
                .len(),
            len
        );
        let s = open(&root).unwrap();
        assert_eq!(get(&s, "jobs/a"), Some("1".into()), "{at}");
        assert_eq!(seq(&s), 1, "{at}");
        transition(&s, &[("jobs/b", "2")], None).unwrap();
    }
}

/// During a checkpoint either the old or the new `control.bin` is present, and frames at
/// or below its sequence number are skipped.
#[test]
fn a_crash_during_a_checkpoint_loses_nothing() {
    for at in ["control-checkpoint-written", "control-log-replaced"] {
        let (_dir, root, s) = setup();
        transition(&s, &[("jobs/a", "1")], Some("a")).unwrap();
        transition(&s, &[("jobs/b", "2")], None).unwrap();
        let (r, c) = crash_copy(&s, &root, at, || s.compact());
        r.unwrap();
        assert_eq!(seq(&c), 2, "{at}");
        assert_eq!(
            (get(&c, "jobs/a"), get(&c, "jobs/b")),
            (Some("1".into()), Some("2".into())),
            "{at}"
        );
        transition(&c, &[("jobs/c", "3")], None).unwrap();
        assert_eq!(seq(&c), 3, "{at}");
    }
}

/// During a compaction or bulk switch the checkpoint precedes the switch, so either
/// generation recovers the same control state, and an unwind between the switch and its
/// publication poisons the writer.
#[test]
fn a_crash_during_a_generation_switch_recovers_the_same_state() {
    for at in ["control-switch-armed", "control-switch-published"] {
        let (_dir, root, s) = setup();
        transition(&s, &[("jobs/a", "1")], Some("a")).unwrap();
        transition(&s, &[("jobs/b", "2")], None).unwrap();
        let (r, c) = crash_copy(&s, &root, at, || s.compact());
        r.unwrap();
        assert_eq!(seq(&c), 2, "{at}");
        assert_eq!(get(&c, "jobs/a"), Some("1".into()), "{at}");
        assert_eq!(c.snapshot().len(), 1, "{at}");
        // the live store's writer is usable after the published switch
        transition(&s, &[("jobs/c", "3")], Some("c")).unwrap();
        drop(s);
        let s = open(&root).unwrap();
        assert_eq!((seq(&s), s.snapshot().len()), (3, 2), "{at}");
    }
    // a bulk commit switches through the same steps
    let (_dir, root, s) = setup();
    drop(s);
    let s = Store::open(&root, bulk_options()).unwrap();
    transition(&s, &[("jobs/a", "1")], Some("a")).unwrap();
    let (r, c) = crash_copy(&s, &root, "control-switch-armed", || {
        let mut txn = s.write();
        let q = quad(&mut txn, "bulk");
        txn.insert_bulk(vec![q])?;
        txn.commit()
    });
    r.unwrap();
    assert_eq!((seq(&c), get(&c, "jobs/a")), (1, Some("1".into())));
    let (_dir, _root, s) = setup();
    transition(&s, &[("jobs/a", "1")], Some("a")).unwrap();
    unwind_at(&s, "control-switch-armed", || s.compact());
    assert!(matches!(
        transition(&s, &[("jobs/b", "2")], None),
        Err(Error::Poisoned)
    ));
}

/// The steps of an enablement already durable remain, and enablement completes the rest.
#[test]
fn an_interrupted_enablement_is_completed() {
    for at in [
        "control-enable-checkpoint",
        "control-enable-log",
        "control-header-upgraded",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ds");
        let s = open(&root).unwrap();
        let mut txn = s.write();
        let q = quad(&mut txn, "a");
        txn.insert(q).unwrap();
        txn.commit().unwrap();
        let ((), c) = crash_copy(&s, &root, at, || s.enable_control().unwrap());
        // a checkpoint without a log is the interrupted enablement, which opens enabled
        assert!(c.control_enabled(), "{at}");
        assert_eq!(seq(&c), 0, "{at}");
        let wal = c.snapshot().generation.dir.clone().unwrap().join("wal.log");
        assert_eq!(
            wal::file_version(&wal).unwrap(),
            crate::formats::WAL_CONTROL_VERSION,
            "{at}"
        );
        c.enable_control().unwrap();
        transition(&c, &[("jobs/a", "1")], Some("b")).unwrap();
        drop(c);
        let c = open(&root.with_file_name(format!("crash-{at}"))).unwrap();
        assert_eq!((seq(&c), c.snapshot().len()), (1, 2), "{at}");
    }
}

/// A compaction whose log was created before enablement is abandoned at the switch.
#[test]
fn a_compaction_from_before_enablement_is_abandoned() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let s = Arc::new(open(&root).unwrap());
    let mut txn = s.write();
    let q = quad(&mut txn, "a");
    txn.insert(q).unwrap();
    txn.commit().unwrap();
    let s2 = s.clone();
    s.set_failpoint(
        "compact-caught-up",
        Some(Arc::new(move |_| {
            let s2 = s2.clone();
            std::thread::spawn(move || s2.enable_control().unwrap())
                .join()
                .unwrap();
        })),
    );
    let report = s.compact_with(&Default::default()).unwrap();
    s.set_failpoint("compact-caught-up", None);
    assert!(report.abandoned.is_some(), "{report:?}");
    // it runs again at the version the store writes
    let report = s.compact_with(&Default::default()).unwrap();
    assert!(report.abandoned.is_none(), "{report:?}");
    let wal = s.snapshot().generation.dir.clone().unwrap().join("wal.log");
    assert_eq!(
        wal::file_version(&wal).unwrap(),
        crate::formats::WAL_CONTROL_VERSION
    );
    transition(&s, &[("jobs/a", "1")], Some("b")).unwrap();
}

// --------------------------------------------------------------- integrity errors ----

#[test]
fn a_lost_checkpoint_or_log_is_an_integrity_error() {
    // a log without its checkpoint
    let (_dir, root, s) = setup();
    transition(&s, &[("jobs/a", "1")], None).unwrap();
    drop(s);
    std::fs::remove_file(root.join(control::CHECKPOINT_FILE)).unwrap();
    integrity(open(&root));
    assert!(
        root.join(control::LOG_FILE).exists(),
        "the refused open changed no file"
    );
    // a checkpoint past sequence zero without its log
    let (_dir, root, s) = setup();
    transition(&s, &[("jobs/a", "1")], Some("a")).unwrap();
    s.compact().unwrap();
    drop(s);
    std::fs::remove_file(root.join(control::LOG_FILE)).unwrap();
    integrity(open(&root));
    // jobs configured without a checkpoint
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    drop(open(&root).unwrap());
    std::fs::write(root.join(crate::jobs::FILE), b"{}").unwrap();
    integrity(open(&root));
}

#[test]
fn frames_that_contradict_the_ordering_are_integrity_errors() {
    let append = |root: &Path, f: Frame| {
        let mut file = OpenOptions::new()
            .append(true)
            .open(root.join(control::LOG_FILE))
            .unwrap();
        file.write_all(&control::log_record(&f.encode().unwrap()))
            .unwrap();
    };
    let put = |k: &str| vec![Op::Put(k.as_bytes().into(), value("1"))];
    // a gap after the checkpoint
    let (_dir, root, s) = setup();
    drop(s);
    append(
        &root,
        Frame {
            seq: 2,
            follows: 0,
            time_ms: 0,
            ops: put("jobs/a"),
        },
    );
    let before = std::fs::read(root.join(control::LOG_FILE)).unwrap();
    integrity(open(&root));
    assert_eq!(std::fs::read(root.join(control::LOG_FILE)).unwrap(), before);
    // a sequence number twice
    let (_dir, root, s) = setup();
    transition(&s, &[("jobs/a", "1")], None).unwrap();
    drop(s);
    append(
        &root,
        Frame {
            seq: 1,
            follows: 0,
            time_ms: 0,
            ops: put("jobs/b"),
        },
    );
    integrity(open(&root));
    // a control-only frame after a commit above the recovered head
    let (_dir, root, s) = setup();
    drop(s);
    append(
        &root,
        Frame {
            seq: 1,
            follows: 5,
            time_ms: 0,
            ops: put("jobs/a"),
        },
    );
    integrity(open(&root));
    // a checkpoint that follows a commit above the head
    let (_dir, root, s) = setup();
    drop(s);
    let mut state = ControlState::default();
    state.follows = 9;
    write_atomic(
        &root.join(control::CHECKPOINT_FILE),
        &control::encode_checkpoint(&state),
    )
    .unwrap();
    integrity(open(&root));
    // a damaged record with a complete record after it
    let (_dir, root, s) = setup();
    transition(&s, &[("jobs/a", "1")], None).unwrap();
    transition(&s, &[("jobs/b", "1")], None).unwrap();
    drop(s);
    let path = root.join(control::LOG_FILE);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[control::LOG_HEADER_BYTES as usize + 10] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();
    integrity(open(&root));
    // a commit with a frame that the replay drops as torn, explained by a later frame
    let (_dir, root, s) = setup();
    transition(&s, &[("jobs/a", "1")], Some("a")).unwrap();
    drop(s);
    let wal = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("wal.log"))
        .find(|p| p.exists())
        .unwrap();
    let len = std::fs::metadata(&wal).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&wal)
        .unwrap()
        .set_len(len - 10)
        .unwrap();
    append(
        &root,
        Frame {
            seq: 2,
            follows: 0,
            time_ms: 0,
            ops: put("jobs/b"),
        },
    );
    integrity(open(&root));
}

/// A26: a capture taken before enablement and written after it holds the header the log
/// had at the capture and no control file, and opens as a store without a keyspace.
#[test]
fn a_capture_from_before_enablement_keeps_the_old_header() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let s = open(&root).unwrap();
    let mut txn = s.write();
    let q = quad(&mut txn, "a");
    txn.insert(q).unwrap();
    txn.commit().unwrap();
    let wal = s.snapshot().generation.dir.clone().unwrap().join("wal.log");
    let before = wal::file_version(&wal).unwrap();
    assert!(before < crate::formats::WAL_CONTROL_VERSION);
    let capture = s.backup_capture("before").unwrap();
    s.enable_control().unwrap();
    transition(&s, &[("jobs/a", "1")], Some("b")).unwrap();
    assert_eq!(
        wal::file_version(&wal).unwrap(),
        crate::formats::WAL_CONTROL_VERSION
    );
    let out = dir.path().join("restored");
    capture.write_to(&out).unwrap();
    drop(capture);
    assert!(!out.join(control::CHECKPOINT_FILE).exists());
    assert!(!out.join(control::LOG_FILE).exists());
    let gen_dir = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.join("wal.log").is_file())
        .unwrap();
    assert_eq!(wal::file_version(&gen_dir.join("wal.log")).unwrap(), before);
    let r = open(&out).unwrap();
    assert!(!r.control_enabled());
    assert_eq!(r.snapshot().len(), 1);
}

/// A16: an open that refuses the control keyspace changes no file, not even the torn
/// tail of the write-ahead log or the commit catalog.
#[test]
fn a_refused_open_leaves_every_file_as_it_was() {
    let (_dir, root, s) = setup();
    transition(&s, &[("jobs/a", "1")], Some("a")).unwrap();
    transition(&s, &[("jobs/b", "2")], None).unwrap();
    let wal = s.snapshot().generation.dir.clone().unwrap().join("wal.log");
    drop(s);
    let mut torn = std::fs::read(&wal).unwrap();
    torn.extend_from_slice(&[7; 5]);
    std::fs::write(&wal, &torn).unwrap();
    std::fs::remove_file(root.join(control::CHECKPOINT_FILE)).unwrap();
    let files = |root: &Path| {
        let mut out = Vec::new();
        for e in walkdir(root) {
            out.push((e.clone(), std::fs::read(&e).unwrap()));
        }
        out.sort();
        out
    };
    let before = files(&root);
    integrity(open(&root));
    assert!(before == files(&root), "the refused open changed a file");
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walkdir(&p));
        } else if p.file_name().is_some_and(|n| n != "sparkles.lock") {
            out.push(p);
        }
    }
    out
}

/// A store whose log was upgraded for the keyspace needs it, so losing both control
/// files is an integrity error even when no commit carries a frame.
#[test]
fn an_upgraded_log_needs_its_control_files() {
    let (_dir, root, s) = setup();
    transition(&s, &[("jobs/a", "1")], None).unwrap();
    drop(s);
    std::fs::remove_file(root.join(control::CHECKPOINT_FILE)).unwrap();
    std::fs::remove_file(root.join(control::LOG_FILE)).unwrap();
    integrity(open(&root));
}
