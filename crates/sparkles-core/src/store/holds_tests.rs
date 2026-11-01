//! Durable source holds (spec C25 §3 and §7): held commits survive trimming,
//! compaction and generation retirement, holds are durable and bounded, a held branch
//! is not deleted unless its holds are released, and every step is crash safe.

use super::*;
use crate::branch::{BranchOptions, CommitRef, DeleteOptions};
use crate::history::{At, Hold, Retention};
use crate::managed::OwnerKind;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64};

const EV: OwnerKind = OwnerKind::Evidence;

fn upd(s: &Store, u: &str) {
    crate::sparql::update::update(s, u, &crate::sparql::QueryOptions::default()).unwrap();
}

fn bump(s: &Store, n: u64) {
    for i in 0..n {
        upd(
            s,
            &format!(
                "INSERT DATA {{ <urn:s> <urn:p> {} }}",
                s.head_commit().seq * 1000 + i
            ),
        );
    }
}

fn id(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}

fn at(s: &Store, seq: u64) -> CommitRef {
    CommitRef {
        branch_id: s.dataset_id(),
        seq,
    }
}

fn readable(s: &Store, seq: u64) -> bool {
    s.snapshot_at(&At::Commit(seq), &Default::default()).is_ok()
}

fn gens(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("gen-") && !n.contains('.'))
        .collect();
    v.sort();
    v
}

fn code(r: Result<impl std::fmt::Debug>) -> String {
    match r {
        Err(e) => e.code().to_string(),
        Ok(v) => panic!("succeeded: {v:?}"),
    }
}

fn minimum_reader(root: &Path) -> u64 {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("dataset.json")).unwrap()).unwrap();
    v["minimumReader"].as_u64().unwrap_or(1)
}

/// A persistent dataset with commits 1–3.
fn setup() -> (tempfile::TempDir, PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    bump(&s, 3);
    assert_eq!(s.head_commit().seq, 3);
    (dir, root, s)
}

#[test]
fn a_hold_keeps_its_generation_across_compaction_and_reopens() {
    let (_dir, root, s) = setup();
    assert!(!root.join(crate::holds::FILE).exists());
    let a = s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    assert!(a.created);
    assert_eq!(a.hold.commits, [at(&s, 1)]);
    assert_eq!(minimum_reader(&root), 7);
    // a release that honors managed graphs (reader 6) but not holds refuses the dataset
    let ds: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("dataset.json")).unwrap()).unwrap();
    assert!(matches!(
        crate::commit::check_dataset_value_as(&ds, 6),
        Err(Error::Unsupported(_))
    ));
    crate::commit::check_dataset_value_as(&ds, 7).unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join(crate::holds::FILE)).unwrap()).unwrap();
    assert_eq!(v["format"], 1);
    assert_eq!(v["datasetId"], s.dataset_id().to_string());
    assert_eq!(v["holds"][0]["owner"], "evidence");
    s.compact().unwrap();
    bump(&s, 2);
    s.compact().unwrap();
    assert_eq!(gens(&root), ["gen-0001", "gen-0003"]);
    let h = s.history();
    let g1 = h.generations.iter().find(|g| g.name == "gen-0001").unwrap();
    assert_eq!(g1.held_by, [Hold::Source]);
    assert!(readable(&s, 1));
    assert!(!readable(&s, 4), "commit 4 was only in gen-0002");
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.source_holds().unwrap(), std::slice::from_ref(&a.hold));
    assert_eq!(gens(&root), ["gen-0001", "gen-0003"]);
    assert!(readable(&s, 1));
    // a release collects what only the hold kept
    assert_eq!(s.release_holds(EV, &[id(1), id(2)]).unwrap(), [id(1)]);
    assert!(s.release_holds(EV, &[id(1)]).unwrap().is_empty());
    assert_eq!(gens(&root), ["gen-0003"]);
    assert!(!readable(&s, 1));
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(s.source_holds().unwrap().is_empty());
}

#[test]
fn the_retention_window_and_its_byte_limit_do_not_trim_held_commits() {
    let (_dir, root, s) = setup();
    s.set_retention(Retention {
        keep_commits: Some(1),
        keep_age_ms: None,
        max_bytes: Some(1),
    })
    .unwrap();
    s.acquire_hold(EV, id(1), &[at(&s, 2)]).unwrap();
    for _ in 0..3 {
        bump(&s, 1);
        s.compact().unwrap();
    }
    s.history_tick().unwrap();
    assert!(readable(&s, 2));
    assert!(gens(&root).contains(&"gen-0001".to_string()));
    s.release_holds(EV, &[id(1)]).unwrap();
    s.compact().unwrap();
    assert!(!readable(&s, 2));
}

#[test]
fn acquisitions_are_idempotent_checked_and_leave_nothing_when_refused() {
    let (_dir, root, s) = setup();
    let first = s
        .acquire_hold(EV, id(1), &[at(&s, 2), at(&s, 1), at(&s, 2)])
        .unwrap();
    assert_eq!(
        first.hold.commits,
        [at(&s, 1), at(&s, 2)],
        "sorted and distinct"
    );
    let again = s.acquire_hold(EV, id(1), &[at(&s, 1), at(&s, 2)]).unwrap();
    assert!(!again.created);
    assert_eq!(again.hold, first.hold);
    assert_eq!(
        code(s.acquire_hold(EV, id(1), &[at(&s, 1)])),
        "hold-conflict"
    );
    assert_eq!(
        code(s.acquire_hold(OwnerKind::Test, id(1), &[at(&s, 1), at(&s, 2)])),
        "hold-conflict"
    );
    let bytes = std::fs::read(root.join(crate::holds::FILE)).unwrap();
    // a commit past the head, an unknown branch and no commit at all
    assert_eq!(
        code(s.acquire_hold(EV, id(2), &[at(&s, 9)])),
        "history-gone"
    );
    let unknown = CommitRef {
        branch_id: id(77),
        seq: 1,
    };
    assert_eq!(
        code(s.acquire_hold(EV, id(2), &[unknown])),
        "no-such-branch"
    );
    assert_eq!(code(s.acquire_hold(EV, id(2), &[])), "invalid");
    // a collected commit
    s.compact().unwrap();
    bump(&s, 2);
    s.compact().unwrap();
    assert!(!readable(&s, 4));
    let bytes2 = std::fs::read(root.join(crate::holds::FILE)).unwrap();
    assert_eq!(
        code(s.acquire_hold(EV, id(3), &[at(&s, 4), at(&s, 5)])),
        "history-gone"
    );
    assert_eq!(
        std::fs::read(root.join(crate::holds::FILE)).unwrap(),
        bytes2
    );
    assert_eq!(bytes, bytes2);
    assert_eq!(s.source_holds().unwrap().len(), 1);
    // a branch store does not change holds
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    assert_eq!(
        code(dev.acquire_hold(EV, id(4), &[at(&s, 5)])),
        "invalid-branch"
    );
}

#[test]
fn branch_commits_are_held_where_they_were_made_and_hold_the_branch() {
    let (_dir, root, s) = setup();
    let dev = s.create_branch("dev", &BranchOptions::default()).unwrap();
    {
        let b = s.branch("dev").unwrap();
        bump(&b, 2);
    }
    bump(&s, 2);
    let on_dev = |seq| CommitRef {
        branch_id: dev.id,
        seq,
    };
    // commit 2 of dev is main's, commit 5 is dev's own
    let a = s.acquire_hold(EV, id(1), &[on_dev(2), on_dev(5)]).unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join(crate::holds::FILE)).unwrap()).unwrap();
    let commits = &raw["holds"][0]["commits"];
    assert_eq!(commits[0]["storeId"], s.dataset_id().to_string());
    assert!(commits[1].get("storeId").is_none());
    // main's history keeps commit 2 for the hold
    assert!(s.family.holds.index.load().holds(s.dataset_id(), 2));
    assert!(s.family.holds.index.load().holds(dev.id, 5));
    s.compact().unwrap();
    bump(&s, 1);
    s.compact().unwrap();
    assert!(readable(&s, 2));
    {
        let b = s.branch("dev").unwrap();
        b.compact().unwrap();
        bump(&b, 1);
        b.compact().unwrap();
        assert!(readable(&b, 5));
    }
    // a hold on main does not hold dev
    s.acquire_hold(EV, id(2), &[at(&s, 2)]).unwrap();
    let r = s.delete_branch_with(
        "dev",
        &DeleteOptions {
            force: true,
            ..Default::default()
        },
    );
    assert_eq!(code(r), "branch-held");
    assert!(s.branch("dev").is_ok());
    assert_eq!(s.source_holds().unwrap().len(), 2);
    s.delete_branch_with(
        "dev",
        &DeleteOptions {
            force: true,
            release_holds: true,
            ..Default::default()
        },
    )
    .unwrap();
    let left: Vec<_> = s
        .source_holds()
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(left, [id(2)]);
    assert!(s.branch("dev").is_err());
    assert_ne!(a.hold.id, id(2));
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.source_holds().unwrap().len(), 1);
    assert!(readable(&s, 2));
    // the deleted branch cannot be held again
    assert_eq!(
        code(s.acquire_hold(EV, id(3), &[on_dev(5)])),
        "no-such-branch"
    );
}

#[test]
fn in_memory_holds_keep_their_states() {
    let s = Store::in_memory(StoreOptions::default());
    s.set_retention(Retention {
        keep_commits: Some(2),
        keep_age_ms: None,
        max_bytes: None,
    })
    .unwrap();
    bump(&s, 3);
    s.acquire_hold(EV, id(1), &[at(&s, 2), at(&s, 3)]).unwrap();
    bump(&s, 4);
    assert!(readable(&s, 2) && readable(&s, 3));
    assert!(!readable(&s, 4), "the window keeps two commits");
    assert_eq!(
        code(s.acquire_hold(EV, id(2), &[at(&s, 4)])),
        "history-gone"
    );
    s.release_holds(EV, &[id(1)]).unwrap();
    assert!(!readable(&s, 2) && !readable(&s, 3));
    // branches of a memory family hold the same way
    let dev = s.create_branch("dev", &BranchOptions::default()).unwrap();
    let head = s.head_commit().seq;
    s.acquire_hold(
        EV,
        id(3),
        &[CommitRef {
            branch_id: dev.id,
            seq: head,
        }],
    )
    .unwrap();
    assert_eq!(
        code(s.delete_branch_with("dev", &DeleteOptions::default())),
        "branch-held"
    );
}

#[test]
fn acquiring_a_hold_again_starts_its_grace_period_again() {
    let (_dir, _root, s) = setup();
    let now = Arc::new(AtomicI64::new(1_000_000));
    let n = now.clone();
    s.set_clock(Arc::new(move || n.load(Ordering::SeqCst)));
    let c = at(&s, 1);
    s.acquire_hold(EV, id(1), std::slice::from_ref(&c)).unwrap();
    // the owner acquires the same hold again just before reconciliation
    now.store(1_000_000 + 10_000, Ordering::SeqCst);
    let again = s.acquire_hold(EV, id(1), std::slice::from_ref(&c)).unwrap();
    assert!(!again.created);
    assert_eq!(again.hold.created_ms, 1_000_000, "creation time is kept");
    now.store(1_000_000 + 10_001, Ordering::SeqCst);
    let mut asked = Vec::new();
    let released = s
        .reconcile_holds(EV, 5_000, &mut |h| {
            asked.push(h.id);
            Ok(false)
        })
        .unwrap();
    assert!(asked.is_empty(), "a renewed hold is not yet asked about");
    assert!(released.is_empty());
    // once the renewed grace period has passed it is an orphan again
    now.store(1_000_000 + 15_000, Ordering::SeqCst);
    let released = s.reconcile_holds(EV, 5_000, &mut |_| Ok(false)).unwrap();
    assert_eq!(released, [id(1)]);
}

#[test]
fn reconciliation_releases_old_orphans_only() {
    let (_dir, _root, s) = setup();
    let now = Arc::new(AtomicI64::new(1_000_000));
    let n = now.clone();
    s.set_clock(Arc::new(move || n.load(Ordering::SeqCst)));
    s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    s.acquire_hold(EV, id(2), &[at(&s, 2)]).unwrap();
    s.acquire_hold(OwnerKind::Test, id(3), &[at(&s, 3)])
        .unwrap();
    now.store(1_000_000 + 10_000, Ordering::SeqCst);
    s.acquire_hold(EV, id(4), &[at(&s, 3)]).unwrap();
    let mut asked = Vec::new();
    let released = s
        .reconcile_holds(EV, 5_000, &mut |h| {
            asked.push(h.id);
            Ok(h.id == id(2))
        })
        .unwrap();
    assert_eq!(
        asked,
        [id(1), id(2)],
        "only old holds of the owner are asked"
    );
    assert_eq!(released, [id(1)]);
    let left: Vec<_> = s
        .source_holds()
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(left, [id(2), id(3), id(4)]);
    // another owner's hold is not released by this one
    assert!(s.release_holds(EV, &[id(3)]).unwrap().is_empty());
    // a failing check releases nothing
    let r = s.reconcile_holds(EV, 0, &mut |_| Err(Error::Timeout));
    assert!(matches!(r, Err(Error::Timeout)));
    assert_eq!(s.source_holds().unwrap().len(), 3);
}

#[test]
fn the_holds_are_bounded() {
    let (_dir, root, s) = setup();
    // 32 branches of commit 2048 name 65,536 distinct commits of main
    bump(&s, 2045);
    assert_eq!(s.head_commit().seq, 2048);
    let ids: Vec<uuid::Uuid> = (0..32)
        .map(|i| {
            s.create_branch(&format!("b{i}"), &BranchOptions::default())
                .unwrap()
                .id
        })
        .collect();
    for (h, two) in ids.chunks(2).enumerate() {
        let commits: Vec<CommitRef> = two
            .iter()
            .flat_map(|b| (1..=2048).map(|seq| CommitRef { branch_id: *b, seq }))
            .collect();
        s.acquire_hold(EV, id(h as u128 + 1), &commits).unwrap();
    }
    assert_eq!(s.held_commits().unwrap(), crate::holds::MAX_COMMITS);
    assert_eq!(
        code(s.acquire_hold(EV, id(100), &[at(&s, 1)])),
        "holds-limit"
    );
    let one_too_many: Vec<CommitRef> = (0..=crate::holds::MAX_COMMITS as u64)
        .map(|seq| at(&s, seq))
        .collect();
    assert_eq!(
        code(s.acquire_hold(EV, id(101), &one_too_many)),
        "holds-limit"
    );
    let size = std::fs::metadata(root.join(crate::holds::FILE))
        .unwrap()
        .len();
    assert!(size < crate::holds::MAX_FILE_BYTES, "{size}");
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.held_commits().unwrap(), crate::holds::MAX_COMMITS);
    s.release_holds(EV, &[id(1)]).unwrap();
    s.acquire_hold(EV, id(100), &[at(&s, 1)]).unwrap();
}

/// Acquire, then crash at `failpoint`, then reopen.
fn crash_acquiring(failpoint: &'static str) -> (tempfile::TempDir, PathBuf, Store) {
    let (dir, root, s) = setup();
    s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    s.set_failpoint(failpoint, Some(Arc::new(|_| panic!("crash"))));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.acquire_hold(EV, id(2), &[at(&s, 2)])
    }));
    assert!(r.is_err(), "{failpoint} was reached");
    s.set_failpoint(failpoint, None);
    // the running store protects at least what the file records
    let live: Vec<_> = s
        .source_holds()
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect();
    let protects_2 = s.family.holds.index.load().holds(s.dataset_id(), 2);
    assert_eq!(live.contains(&id(2)), protects_2, "{failpoint}");
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    (dir, root, s)
}

#[test]
fn an_interrupted_acquisition_is_all_or_nothing() {
    for failpoint in ["holds-protected", "holds-verified", "holds-reader-raised"] {
        let (_dir, _root, s) = crash_acquiring(failpoint);
        let ids: Vec<_> = s
            .source_holds()
            .unwrap()
            .into_iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(ids, [id(1)], "{failpoint}");
        assert!(s.acquire_hold(EV, id(2), &[at(&s, 2)]).unwrap().created);
    }
    let (_dir, root, s) = crash_acquiring("holds-published");
    let ids: Vec<_> = s
        .source_holds()
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(ids, [id(1), id(2)]);
    s.compact().unwrap();
    bump(&s, 1);
    s.compact().unwrap();
    assert!(readable(&s, 2));
    assert!(gens(&root).contains(&"gen-0001".to_string()));
}

#[test]
fn an_interrupted_release_keeps_the_hold_or_drops_it_whole() {
    let (_dir, root, s) = setup();
    s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    s.compact().unwrap();
    s.set_failpoint(
        "holds-release-published",
        Some(Arc::new(|_| panic!("crash"))),
    );
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.release_holds(EV, &[id(1)])
    }));
    assert!(r.is_err());
    // released on disk, still protected in the stopped process
    assert!(s.family.holds.index.load().holds(s.dataset_id(), 1));
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(s.source_holds().unwrap().is_empty());
    assert_eq!(gens(&root), ["gen-0002"]);
}

#[test]
fn an_interrupted_branch_deletion_releases_before_it_deletes() {
    let (_dir, root, s) = setup();
    let dev = s.create_branch("dev", &BranchOptions::default()).unwrap();
    s.acquire_hold(
        EV,
        id(1),
        &[CommitRef {
            branch_id: dev.id,
            seq: 3,
        }],
    )
    .unwrap();
    s.set_failpoint(
        "branch-delete-holds-released",
        Some(Arc::new(|_| panic!("crash"))),
    );
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.delete_branch_with(
            "dev",
            &DeleteOptions {
                release_holds: true,
                ..Default::default()
            },
        )
    }));
    assert!(r.is_err());
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(s.source_holds().unwrap().is_empty());
    assert!(s.branch("dev").is_ok(), "the branch stays");
    s.delete_branch("dev", false).unwrap();
}

#[test]
fn unreadable_holds_keep_every_past_state() {
    let (_dir, root, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    s.compact().unwrap();
    bump(&s, 1);
    drop(s);
    std::fs::write(root.join(crate::holds::FILE), b"{not json").unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(code(s.source_holds()), "holds-unavailable");
    assert_eq!(
        code(s.acquire_hold(EV, id(1), &[at(&s, 4)])),
        "holds-unavailable"
    );
    assert_eq!(code(s.release_holds(EV, &[id(1)])), "holds-unavailable");
    assert_eq!(
        code(s.delete_branch_with(
            "dev",
            &DeleteOptions {
                force: true,
                release_holds: true,
                ..Default::default()
            }
        )),
        "holds-unavailable"
    );
    // writes and reads go on, and nothing is collected
    bump(&s, 1);
    s.compact().unwrap();
    assert!(readable(&s, 1) && readable(&s, 4));
    let h = s.history();
    assert!(
        h.generations
            .iter()
            .filter(|g| !g.current)
            .all(|g| g.held_by.contains(&Hold::Source))
    );
    // a file of another dataset is unreadable too
    drop(s);
    let other = crate::holds::encode(id(5), &Default::default());
    std::fs::write(root.join(crate::holds::FILE), other).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(code(s.source_holds()), "holds-unavailable");
    drop(s);
    // a newer format refuses the open
    let newer = format!(r#"{{"format":2,"datasetId":"{}"}}"#, id(5));
    std::fs::write(root.join(crate::holds::FILE), newer).unwrap();
    assert!(matches!(
        Store::open(&root, StoreOptions::default()),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn offline_retention_counts_holds() {
    let (_dir, root, s) = setup();
    s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    s.compact().unwrap();
    let id = s.dataset_id();
    drop(s);
    let kept = crate::history::retained_offline(&root, id, 2).unwrap();
    assert_eq!(kept.get(&1), Some(&vec![Hold::Source]));
}

#[test]
fn offline_retention_of_a_dataset_in_a_directory_named_branches_reads_its_own_holds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(crate::store::BRANCHES_DIR).join("ds");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.set_retention(Retention {
        keep_commits: Some(100),
        keep_age_ms: None,
        max_bytes: None,
    })
    .unwrap();
    bump(&s, 3);
    s.compact().unwrap();
    bump(&s, 2);
    s.compact().unwrap();
    let id = s.dataset_id();
    drop(s);
    let current: u32 = gens(&root).last().unwrap()["gen-".len()..].parse().unwrap();
    // no holds: the window keeps older generations, but none is listed as source-held
    let kept = crate::history::retained_offline(&root, id, current).unwrap();
    assert!(!kept.is_empty());
    assert!(
        kept.values().all(|why| !why.contains(&Hold::Source)),
        "{kept:?}"
    );
}

#[test]
fn an_oversized_holds_file_opens_the_dataset_and_fails_closed() {
    let (_dir, root, s) = setup();
    drop(s);
    // a newer format behind more padding than a holds file may have: the preflight
    // does not read it whole and the dataset opens with its holds unavailable
    let mut bytes = vec![b' '; crate::holds::MAX_FILE_BYTES as usize + 1];
    bytes.extend_from_slice(br#"{"format":2}"#);
    std::fs::write(root.join(crate::holds::FILE), bytes).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(code(s.source_holds()), "holds-unavailable");
    bump(&s, 3);
    s.compact().unwrap();
    assert!(readable(&s, 1), "unreadable holds keep every past state");
}

#[cfg(unix)]
#[test]
fn a_hold_whose_rename_fails_stays_protected_and_is_written_on_retry() {
    let (_dir, root, s) = setup();
    // a directory in the way makes the rename fail
    let path = root.join(crate::holds::FILE);
    std::fs::create_dir_all(path.join("blocker")).unwrap();
    assert!(s.acquire_hold(EV, id(1), &[at(&s, 1)]).is_err());
    assert!(s.family.holds.index.load().holds(s.dataset_id(), 1));
    assert_eq!(s.source_holds().unwrap().len(), 1);
    s.compact().unwrap();
    bump(&s, 2);
    s.compact().unwrap();
    assert!(readable(&s, 1));
    // the retry of the same acquisition writes the file
    std::fs::remove_dir_all(&path).unwrap();
    let again = s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    assert!(!again.created);
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.source_holds().unwrap().len(), 1);
    assert!(readable(&s, 1));
}

/// Acquisitions and releases race with commits, compactions and collections, and a
/// held commit never stops being readable while its hold exists.
#[test]
fn concurrent_holds_and_collections_never_lose_a_held_commit() {
    let (_dir, _root, s) = setup();
    let s = Arc::new(s);
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicU64::new(0));
    let held_ok = Arc::new(AtomicU64::new(0));
    let writer = {
        let (s, stop) = (s.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut rounds = 0;
            while !stop.load(Ordering::SeqCst) && rounds < 400 {
                bump(&s, 2);
                s.compact().unwrap();
                rounds += 1;
            }
        })
    };
    let holders: Vec<_> = (0..3)
        .map(|t| {
            let (s, stop, lost, held_ok) = (s.clone(), stop.clone(), lost.clone(), held_ok.clone());
            std::thread::spawn(move || {
                let mut n = 0u128;
                while !stop.load(Ordering::SeqCst) {
                    n += 1;
                    let hold = id(((t as u128) << 64) | n);
                    let seq = s.head_commit().seq.saturating_sub(1);
                    match s.acquire_hold(EV, hold, &[at(&s, seq)]) {
                        Ok(_) => {
                            held_ok.fetch_add(1, Ordering::SeqCst);
                            for _ in 0..3 {
                                if !readable(&s, seq) {
                                    lost.fetch_add(1, Ordering::SeqCst);
                                }
                                std::thread::yield_now();
                            }
                            assert_eq!(s.release_holds(EV, &[hold]).unwrap(), [hold]);
                        }
                        Err(Error::HistoryGone(_)) => {}
                        Err(e) => panic!("{e}"),
                    }
                }
            })
        })
        .collect();
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(8) && !writer.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap();
    for h in holders {
        h.join().unwrap();
    }
    assert_eq!(lost.load(Ordering::SeqCst), 0);
    assert!(held_ok.load(Ordering::SeqCst) > 10);
    assert!(s.source_holds().unwrap().is_empty());
}

/// Review findings of the first round, each with its regression.
#[test]
fn a_collection_waiting_for_the_branch_table_does_not_deadlock_acquisitions() {
    let (_dir, _root, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let s = Arc::new(s);
    // A collection holds main's history lock and will take the branch table.
    let history = s.history.as_ref().unwrap().clone();
    let guard = history.lock();
    // One acquisition takes the holds' lock and waits for that history lock, and a
    // second one and a branch deletion wait for the holds' lock.
    let spawn = |f: Box<dyn FnOnce(&Store) + Send>| {
        let s = s.clone();
        std::thread::spawn(move || f(&s))
    };
    let a = spawn(Box::new(|s| {
        s.acquire_hold(EV, id(1), &[at(s, 1)]).unwrap();
    }));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let b = spawn(Box::new(|s| {
        s.acquire_hold(EV, id(2), &[at(s, 2)]).unwrap();
    }));
    let c = spawn(Box::new(|s| {
        let _ = s.delete_branch("dev", true);
    }));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // the collection can still take the table
    let (tx, rx) = std::sync::mpsc::channel();
    let set = s.branch_set().unwrap();
    let id_main = s.dataset_id();
    let probe = std::thread::spawn(move || {
        let _ = set.holds_on(id_main);
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok(),
        "the branch table is held by a waiter of the holds' lock"
    );
    drop(guard);
    for t in [a, b, c, probe] {
        t.join().unwrap();
    }
    assert_eq!(s.source_holds().unwrap().len(), 2);
}

#[test]
fn a_hold_whose_file_is_replaced_stays_protected_in_the_running_store() {
    let (_dir, root, s) = setup();
    s.set_failpoint("holds-renamed", Some(Arc::new(|_| panic!("sync failed"))));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.acquire_hold(EV, id(1), &[at(&s, 1)])
    }));
    assert!(r.is_err());
    s.set_failpoint("holds-renamed", None);
    assert!(s.family.holds.index.load().holds(s.dataset_id(), 1));
    assert_eq!(s.source_holds().unwrap().len(), 1);
    s.compact().unwrap();
    bump(&s, 2);
    s.compact().unwrap();
    assert!(readable(&s, 1));
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.source_holds().unwrap().len(), 1);
    assert!(readable(&s, 1));
}

#[test]
fn reconciliation_does_not_release_a_hold_acquired_again_while_it_asks() {
    let (_dir, _root, s) = setup();
    s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    let released = s
        .reconcile_holds(EV, 0, &mut |h| {
            // a retry of the operation acquires the same hold again meanwhile
            let again = s.acquire_hold(EV, h.id, &h.commits).unwrap();
            assert!(!again.created);
            Ok(false)
        })
        .unwrap();
    assert!(released.is_empty());
    assert_eq!(s.source_holds().unwrap().len(), 1);
    assert_eq!(
        s.reconcile_holds(EV, 0, &mut |_| Ok(false)).unwrap(),
        [id(1)]
    );
}

#[test]
fn held_memory_states_keep_their_commit_metadata_past_the_ring() {
    let s = Store::in_memory(StoreOptions {
        memory_commit_ring: 2,
        ..Default::default()
    });
    bump(&s, 1);
    s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    bump(&s, 4);
    assert!(readable(&s, 1));
    let limited = s.snapshot_at_limited(&At::Commit(1), &Default::default(), 64 << 20);
    assert!(limited.is_ok(), "{:?}", limited.err());
    s.release_holds(EV, &[id(1)]).unwrap();
    assert!(!readable(&s, 1));
}

#[test]
fn a_branch_deletion_drops_the_memory_states_it_released() {
    let s = Store::in_memory(StoreOptions::default());
    s.set_retention(Retention {
        keep_commits: Some(2),
        keep_age_ms: None,
        max_bytes: None,
    })
    .unwrap();
    bump(&s, 2);
    let dev = s.create_branch("dev", &BranchOptions::default()).unwrap();
    // commit 1 of dev is main's, held in main's memory
    s.acquire_hold(
        EV,
        id(1),
        &[CommitRef {
            branch_id: dev.id,
            seq: 1,
        }],
    )
    .unwrap();
    assert_eq!(s.mem_history.as_ref().unwrap().lock().held.len(), 1);
    bump(&s, 3);
    s.delete_branch_with(
        "dev",
        &DeleteOptions {
            release_holds: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.mem_history.as_ref().unwrap().lock().held.is_empty());
    assert!(!readable(&s, 1));
}

#[test]
fn damaged_holds_files_open_and_fail_closed() {
    let (_dir, root, s) = setup();
    s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    drop(s);
    // a version member that is not a version
    let path = root.join(crate::holds::FILE);
    let good = std::fs::read(&path).unwrap();
    std::fs::write(&path, br#"{"format":"one"}"#).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(code(s.source_holds()), "holds-unavailable");
    bump(&s, 1);
    drop(s);
    // a file the process cannot read
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&path, &good).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_err() {
            let s = Store::open(&root, StoreOptions::default()).unwrap();
            assert_eq!(code(s.source_holds()), "holds-unavailable");
            drop(s);
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    std::fs::write(&path, &good).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.source_holds().unwrap().len(), 1);
}

/// The concurrency test of an in-memory dataset: releases settle held states against
/// the holds acquired since.
#[test]
fn concurrent_memory_holds_never_lose_a_held_state() {
    let s = Arc::new(Store::in_memory(StoreOptions::default()));
    s.set_retention(Retention {
        keep_commits: Some(2),
        keep_age_ms: None,
        max_bytes: None,
    })
    .unwrap();
    bump(&s, 3);
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicU64::new(0));
    let writer = {
        let (s, stop) = (s.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                bump(&s, 1);
            }
        })
    };
    let holders: Vec<_> = (0..3)
        .map(|t| {
            let (s, stop, lost) = (s.clone(), stop.clone(), lost.clone());
            std::thread::spawn(move || {
                let mut n = 0u128;
                while !stop.load(Ordering::SeqCst) {
                    n += 1;
                    let hold = id(((t as u128) << 64) | n);
                    let seq = s.head_commit().seq;
                    if s.acquire_hold(EV, hold, &[at(&s, seq)]).is_ok() {
                        std::thread::yield_now();
                        if !readable(&s, seq) {
                            lost.fetch_add(1, Ordering::SeqCst);
                        }
                        s.release_holds(EV, &[hold]).unwrap();
                    }
                }
            })
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_secs(3));
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap();
    for h in holders {
        h.join().unwrap();
    }
    assert_eq!(lost.load(Ordering::SeqCst), 0);
    assert!(s.mem_history.as_ref().unwrap().lock().held.is_empty());
}

#[test]
fn an_uncertain_write_is_repeated_before_a_hold_is_acknowledged_again() {
    let (_dir, root, s) = setup();
    s.set_failpoint("holds-renamed", Some(Arc::new(|_| panic!("sync failed"))));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.acquire_hold(EV, id(1), &[at(&s, 1)])
    }));
    assert!(r.is_err());
    s.set_failpoint("holds-renamed", None);
    // as if the rename had not survived
    std::fs::remove_file(root.join(crate::holds::FILE)).unwrap();
    let again = s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    assert!(!again.created);
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.source_holds().unwrap().len(), 1);
    // a certain file is not written for a repeated acquisition
    let before = std::fs::metadata(root.join(crate::holds::FILE))
        .unwrap()
        .modified()
        .unwrap();
    std::fs::remove_file(root.join(crate::holds::FILE)).unwrap();
    s.acquire_hold(EV, id(1), &[at(&s, 1)]).unwrap();
    assert!(!root.join(crate::holds::FILE).exists(), "{before:?}");
}
