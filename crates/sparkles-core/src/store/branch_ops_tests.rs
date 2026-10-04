//! Branches and merges, Phase 2: squash merges, reverts, cherry-picks, replayed
//! fast-forwards, renames, deletions that re-parent, exempt predicates and merges as
//! tasks. The acceptance examples A24 onward of F09, at the library level.

use super::branch_tests::{apply, code, dump, has, int, merge, merged, setup};
use super::*;
use crate::branch::{BranchOptions, MergeOptions, MergeOutcome};
use crate::history::At;

#[test]
fn a24_squash_merges_record_no_second_parent() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:c> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d> <urn:p> <urn:x> .");
    let squash = MergeOptions {
        squash: true,
        ..Default::default()
    };
    let r = merged(merge(&s, "dev", "main", &squash));
    assert!(r.squashed && r.fast_forward);
    let c = r.commit.unwrap();
    assert_eq!((c.commit.seq, c.commit.kind), (3, CommitKind::Merge));
    assert_eq!(c.commit.inserted, 2);
    assert_eq!(
        c.annotation.message.as_deref(),
        Some("squash dev (commit 4) into main")
    );
    assert!(s.merge_record(3).is_none(), "no second parent");
    assert_eq!(dump(&s), dump(&dev));
    // main does not descend from dev: dev is still ahead
    assert_eq!(s.branch_info("dev").unwrap().ahead, 2);
    // the same changes again are no change, and no commit
    assert!(matches!(
        merge(&s, "dev", "main", &squash),
        MergeOutcome::UpToDate(_)
    ));
    assert_eq!(s.head_commit().seq, 3);
    // a later squash brings only the newer changes
    apply(&dev, "+<urn:e> <urn:p> <urn:x> .");
    let r = merged(merge(&s, "dev", "main", &squash));
    assert_eq!((r.inserted, r.deleted), (1, 0));
    assert!(!r.fast_forward);
    assert!(has(&s, "urn:e"));
    // and an ordinary merge records the second parent at last
    let r = merged(merge(&s, "dev", "main", &Default::default()));
    assert_eq!((r.inserted, r.deleted), (0, 0));
    assert!(s.merge_record(r.commit.unwrap().commit.seq).is_some());
    assert_eq!(s.branch_info("dev").unwrap().ahead, 0);
    // a protected target takes squash merges
    s.set_branch_protected("main", true).unwrap();
    apply(&dev, "+<urn:f> <urn:p> <urn:x> .");
    merged(merge(&s, "dev", "main", &squash));
    assert!(has(&s, "urn:f"));
}

#[test]
fn a25_reverts_merge_a_commits_parent_over_the_commit() {
    let (_dir, s) = setup();
    apply(&s, "+<urn:c> <urn:p> <urn:x> .");
    apply(
        &s,
        &format!(
            "-<urn:a> <urn:age> {} .\n+<urn:a> <urn:age> {} .",
            int(30),
            int(31)
        ),
    );
    let p = s.preview_revert("main", 3, &Default::default()).unwrap();
    assert_eq!((p.inserted, p.deleted, p.merged), (0, 1, false));
    let r = merged(s.revert("main", 3, &Default::default()).unwrap());
    let c = r.commit.unwrap();
    assert_eq!((c.commit.seq, c.commit.kind), (5, CommitKind::Revert));
    assert_eq!(c.annotation.message.as_deref(), Some("revert commit 3"));
    assert_eq!(r.base.as_ref().unwrap().seq, 3);
    assert_eq!(r.source.seq, 2);
    assert!(s.merge_record(5).is_none());
    assert!(!has(&s, "urn:c") && has(&s, "urn:b"));
    // reverting it again changes nothing
    assert!(matches!(
        s.revert("main", 3, &Default::default()).unwrap(),
        MergeOutcome::UpToDate(_)
    ));
    // commit 1 set the age that commit 4 changed: the cell conflicts
    let MergeOutcome::Conflicts(rep) = s.revert("main", 1, &Default::default()).unwrap() else {
        panic!("expected a conflict");
    };
    assert_eq!(rep.conflicts, 1);
    assert!(
        rep.error
            .starts_with("1 conflict reverting commit 1 on main"),
        "{}",
        rep.error
    );
    let theirs = MergeOptions {
        on_conflict: Some(crate::branch::Take::Theirs),
        ..Default::default()
    };
    merged(s.revert("main", 1, &theirs).unwrap());
    assert!(!has(&s, "urn:age"));
    // a revert on a branch of a commit it shares with main changes the branch alone
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    merged(s.revert("dev", 2, &Default::default()).unwrap());
    assert!(!has(&dev, "urn:b") && has(&s, "urn:b"));
    assert_eq!(
        code(&s.revert("main", 0, &Default::default()).unwrap_err()),
        "invalid-merge"
    );
    assert!(matches!(
        s.revert("main", 99, &Default::default()).unwrap_err(),
        Error::NotFound(_)
    ));
    // a protected branch takes changes through merges only
    s.set_branch_protected("main", true).unwrap();
    assert_eq!(
        code(&s.revert("main", 2, &Default::default()).unwrap_err()),
        "branch-protected"
    );
}

#[test]
fn a26_cherry_picks_merge_a_commit_over_its_parent() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:c> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d> <urn:p> <urn:x> .");
    let p = s
        .preview_cherry_pick("dev", 4, "main", &Default::default())
        .unwrap();
    assert_eq!((p.inserted, p.deleted), (1, 0));
    let r = merged(
        s.cherry_pick("dev", 4, "main", &Default::default())
            .unwrap(),
    );
    let c = r.commit.unwrap();
    assert_eq!((c.commit.seq, c.commit.kind), (3, CommitKind::CherryPick));
    assert_eq!(
        c.annotation.message.as_deref(),
        Some("cherry-pick commit 4 of dev")
    );
    assert!(s.merge_record(3).is_none());
    assert!(has(&s, "urn:d") && !has(&s, "urn:c"));
    // the same commit again, or one main already has, changes nothing
    assert!(matches!(
        s.cherry_pick("dev", 4, "main", &Default::default())
            .unwrap(),
        MergeOutcome::UpToDate(_)
    ));
    assert!(matches!(
        s.cherry_pick("dev", 2, "main", &Default::default())
            .unwrap(),
        MergeOutcome::UpToDate(_)
    ));
    // a later merge brings the rest, and the picked change does not conflict
    let r = merged(merge(&s, "dev", "main", &Default::default()));
    assert_eq!((r.inserted, r.deleted, r.conflicts_found), (1, 0, 0));
    assert_eq!(dump(&s), dump(&dev));
    // a commit that changes a cell the target changed conflicts
    apply(
        &s,
        &format!(
            "-<urn:a> <urn:age> {} .\n+<urn:a> <urn:age> {} .",
            int(30),
            int(31)
        ),
    );
    let seq = apply(
        &dev,
        &format!(
            "-<urn:a> <urn:age> {} .\n+<urn:a> <urn:age> {} .",
            int(30),
            int(32)
        ),
    )
    .commit
    .seq;
    let MergeOutcome::Conflicts(rep) = s
        .cherry_pick("dev", seq, "main", &Default::default())
        .unwrap()
    else {
        panic!("expected a conflict");
    };
    assert!(
        rep.error.starts_with(&format!(
            "1 conflict cherry-picking commit {seq} of dev into main"
        )),
        "{}",
        rep.error
    );
    s.set_branch_protected("main", true).unwrap();
    assert_eq!(
        code(
            &s.cherry_pick("dev", seq, "main", &Default::default())
                .unwrap_err()
        ),
        "branch-protected"
    );
}

/// Insert `<s> <urn:p> <urn:x>` on `store` as `author`, with `message`.
fn insert_as(store: &Store, s: &str, author: &str, message: &str) -> Receipt {
    let mut t = store.write_with(
        CommitKind::Update,
        crate::guard::WriteOptions {
            author: Some(author.into()),
            message: Some(message.into()),
            ..Default::default()
        },
    );
    let q = t
        .encode_quad(
            &Quad::new(
                named(s),
                named("urn:p"),
                named("urn:x"),
                GraphName::DefaultGraph,
            ),
            &mut Default::default(),
        )
        .unwrap();
    t.insert(q).unwrap();
    t.commit().unwrap()
}

#[test]
fn a27_replayed_fast_forwards_keep_each_commit() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    insert_as(&dev, "urn:c", "ann", "add c");
    apply(&dev, "-<urn:b> <urn:name> \"B\" .");
    let replay = MergeOptions {
        replay: true,
        ..Default::default()
    };
    let p = s.preview_merge("dev", "main", &replay).unwrap();
    assert_eq!(p.replayed.len(), 2);
    assert!(p.replayed.iter().all(|r| r.receipt.is_none()));
    assert_eq!(s.head_commit().seq, 2);
    let r = merged(merge(&s, "dev", "main", &replay));
    assert!(r.fast_forward);
    assert_eq!((r.inserted, r.deleted), (1, 1));
    let seqs: Vec<(u64, u64)> = r
        .replayed
        .iter()
        .map(|c| (c.from.seq, c.receipt.as_ref().unwrap().commit.seq))
        .collect();
    assert_eq!(seqs, [(3, 3), (4, 4)]);
    assert_eq!(dump(&s), dump(&dev));
    let c3 = s.commit(3).unwrap();
    assert_eq!(c3.kind, CommitKind::Update);
    assert_eq!(s.annotation(3).unwrap().message.as_deref(), Some("add c"));
    assert_eq!(s.commit_author(3).as_deref(), Some("ann"));
    assert_eq!(s.commit(4).unwrap().kind, CommitKind::Transaction);
    let log = s.branch_commits("main", CommitRange::Latest, 2).unwrap();
    let from: Vec<(u64, u64)> = log
        .iter()
        .map(|c| (c.commit.seq, c.replayed_from.as_ref().unwrap().seq))
        .collect();
    assert_eq!(from, [(4, 4), (3, 3)]);
    assert!(log.iter().all(|c| c.merged_from.is_none()));
    assert_eq!(s.branch_info("dev").unwrap().ahead, 0);
    assert!(matches!(
        merge(&s, "dev", "main", &replay),
        MergeOutcome::UpToDate(_)
    ));
    // later commits replay on top, onto a protected main too
    s.set_branch_protected("main", true).unwrap();
    apply(&dev, "+<urn:e> <urn:p> <urn:x> .");
    let r = merged(merge(&s, "dev", "main", &replay));
    assert_eq!(r.replayed.len(), 1);
    assert_eq!(r.replayed[0].from.seq, 5);
    assert_eq!(dump(&s), dump(&dev));
    s.set_branch_protected("main", false).unwrap();
    // a target with changes of its own is no fast-forward
    apply(&s, "+<urn:m> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:f> <urn:p> <urn:x> .");
    assert_eq!(
        code(&s.merge("dev", "main", &replay).unwrap_err()),
        "not-fast-forward"
    );
    let both = MergeOptions {
        replay: true,
        squash: true,
        ..Default::default()
    };
    assert_eq!(
        code(&s.merge("dev", "main", &both).unwrap_err()),
        "invalid-merge"
    );
}

#[test]
fn a27_replay_needs_the_base_in_the_sources_own_history() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:c> <urn:p> <urn:x> .");
    apply(&s, "+<urn:m> <urn:p> <urn:x> .");
    // dev takes main's commit: the base is then main's head, not on dev's own line
    merged(merge(&s, "main", "dev", &Default::default()));
    let replay = MergeOptions {
        replay: true,
        ..Default::default()
    };
    assert_eq!(
        code(&s.merge("dev", "main", &replay).unwrap_err()),
        "cannot-replay"
    );
    // an ordinary merge goes through
    merged(merge(&s, "dev", "main", &Default::default()));
    assert_eq!(dump(&s), dump(&dev));
}

/// The names of the directories under `ds/branches`.
fn branch_dirs(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir.join("ds/branches"))
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn table(dir: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join("ds/branches.json")).unwrap()).unwrap()
}

fn holds_of(s: &Store) -> Vec<String> {
    let mut v: Vec<String> = s
        .history()
        .generations
        .iter()
        .flat_map(|g| g.held_by.iter().map(|h| h.to_string()))
        .collect();
    v.sort();
    v.dedup();
    v
}

#[test]
fn a28_renames_keep_the_branch_and_its_children() {
    let (dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let id = s.branch_id_of("dev").unwrap();
    apply(&s.branch("dev").unwrap(), "+<urn:c> <urn:p> <urn:x> .");
    s.create_branch(
        "feat",
        &BranchOptions {
            from: "dev".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let before = dump(&s.branch("dev").unwrap());
    let info = s.rename_branch("dev", "work").unwrap();
    assert_eq!((info.name.as_str(), info.id), ("work", id));
    assert_eq!(code(&s.branch("dev").err().unwrap()), "no-such-branch");
    let work = s.branch("work").unwrap();
    assert_eq!(work.branch_name(), "work");
    assert_eq!(dump(&work), before);
    assert_eq!(
        s.branch_info("feat").unwrap().upstream.as_deref(),
        Some("work")
    );
    assert_eq!(
        s.branch_info("feat")
            .unwrap()
            .from
            .unwrap()
            .branch
            .as_deref(),
        Some("work")
    );
    assert!(
        holds_of(&s).contains(&"branch:work".to_string()),
        "{:?}",
        holds_of(&s)
    );
    assert!(!holds_of(&s).iter().any(|h| h.ends_with(":dev")));
    let log = s.branch_commits("work", CommitRange::Latest, 1).unwrap();
    assert_eq!(log[0].branch.as_deref(), Some("work"));
    let bf =
        super::branching::read_branch_file(&dir.path().join("ds/branches").join(id.to_string()))
            .unwrap()
            .unwrap();
    assert_eq!(bf.name, "work");
    // writes and merges under the new name
    apply(&work, "+<urn:d> <urn:p> <urn:x> .");
    merged(merge(&s, "work", "main", &Default::default()));
    assert!(has(&s, "urn:d"));
    // refusals
    assert_eq!(
        code(&s.rename_branch("work", "feat").unwrap_err()),
        "branch-exists"
    );
    assert_eq!(
        code(&s.rename_branch("work", "main").unwrap_err()),
        "branch-exists"
    );
    assert_eq!(
        code(&s.rename_branch("main", "x").unwrap_err()),
        "invalid-branch"
    );
    assert_eq!(
        code(&s.rename_branch("work", "head").unwrap_err()),
        "invalid-branch"
    );
    assert_eq!(
        code(&s.rename_branch("nope", "x").unwrap_err()),
        "no-such-branch"
    );
    // the old name is free again
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    assert_ne!(s.branch_id_of("dev").unwrap(), id);
    drop(work);
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert_eq!(s.branch_id_of("work").unwrap(), id);
    assert!(has(&s.branch("work").unwrap(), "urn:d"));
}

#[test]
fn a28_a_crash_in_a_rename_leaves_one_name() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let (dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let id = s.branch_id_of("dev").unwrap();
    apply(&s.branch("dev").unwrap(), "+<urn:c> <urn:p> <urn:x> .");
    s.set_failpoint(
        "branch-rename-committed",
        Some(Arc::new(|_: &Store| panic!("crash"))),
    );
    let r = catch_unwind(AssertUnwindSafe(|| s.rename_branch("dev", "work")));
    assert!(r.is_err());
    drop(s);
    let bdir = dir.path().join("ds/branches").join(id.to_string());
    // the table committed the rename; the identity file had not followed yet
    assert_eq!(
        super::branching::read_branch_file(&bdir)
            .unwrap()
            .unwrap()
            .name,
        "dev"
    );
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert_eq!(s.branch_id_of("work").unwrap(), id);
    assert_eq!(code(&s.branch("dev").err().unwrap()), "no-such-branch");
    assert!(has(&s.branch("work").unwrap(), "urn:c"));
    assert_eq!(
        super::branching::read_branch_file(&bdir)
            .unwrap()
            .unwrap()
            .name,
        "work"
    );
}

/// main 1–2; `dev` from main 2 inserts c (3); `feat` from dev 3 inserts f (4); dev
/// inserts d (4).
fn family() -> (tempfile::TempDir, Store, uuid::Uuid) {
    let (dir, s) = setup();
    s.compact().unwrap();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev_id = s.branch_id_of("dev").unwrap();
    apply(&s.branch("dev").unwrap(), "+<urn:c> <urn:p> <urn:x> .");
    s.create_branch(
        "feat",
        &BranchOptions {
            from: "dev".into(),
            ..Default::default()
        },
    )
    .unwrap();
    apply(&s.branch("feat").unwrap(), "+<urn:f> <urn:p> <urn:x> .");
    apply(&s.branch("dev").unwrap(), "+<urn:d> <urn:p> <urn:x> .");
    (dir, s, dev_id)
}

#[test]
fn a29_deleting_a_branch_re_parents_its_children() {
    let (dir, s, dev_id) = family();
    let feat_dump = dump(&s.branch("feat").unwrap());
    assert_eq!(
        code(&s.delete_branch("dev", true).unwrap_err()),
        "has-children"
    );
    let re = crate::branch::DeleteOptions {
        force: false,
        reparent: true,
    };
    // d (dev's commit 4) is in no other history; c is in feat's
    assert_eq!(
        code(&s.delete_branch_with("dev", &re).unwrap_err()),
        "unmerged"
    );
    let re = crate::branch::DeleteOptions {
        force: true,
        reparent: true,
    };
    s.delete_branch_with("dev", &re).unwrap();
    let names: Vec<String> = s.branches().unwrap().into_iter().map(|b| b.name).collect();
    assert_eq!(names, ["main", "feat"]);
    assert_eq!(code(&s.branch("dev").err().unwrap()), "no-such-branch");
    let feat = s.branch_info("feat").unwrap();
    assert_eq!(feat.upstream.as_deref(), Some("main"));
    assert_eq!(feat.from.as_ref().unwrap().branch, None);
    assert_eq!(feat.from.as_ref().unwrap().branch_id, dev_id);
    assert_eq!(feat.ahead, 2, "c and f");
    // the retired branch's storage stays
    assert!(branch_dirs(dir.path()).contains(&dev_id.to_string()));
    assert_eq!(table(dir.path())["format"], 2);
    assert_eq!(table(dir.path())["retired"][0]["id"], dev_id.to_string());
    assert_eq!(dump(&s.branch("feat").unwrap()), feat_dump);
    let log = s.branch_commits("feat", CommitRange::Latest, 10).unwrap();
    let owners: Vec<(u64, Option<String>)> = log
        .iter()
        .map(|c| (c.commit.seq, c.branch.clone()))
        .collect();
    assert_eq!(
        owners,
        [
            (4, Some("feat".into())),
            (3, None),
            (2, Some("main".into())),
            (1, Some("main".into())),
            (0, Some("main".into()))
        ]
    );
    // reads of a commit of the retired branch, and main's compactions, keep working
    let (snap, _) = s
        .branch_snapshot_at("feat", &At::Commit(3), &Default::default())
        .unwrap();
    assert!(
        super::branch_tests::dump_snap(&snap)
            .iter()
            .any(|l| l.contains("urn:c"))
    );
    s.compact().unwrap();
    s.compact().unwrap();
    // the name is free again
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    assert_ne!(s.branch_id_of("dev").unwrap(), dev_id);
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert_eq!(dump(&s.branch("feat").unwrap()), feat_dump);
    // a merge into main brings c and f
    merged(merge(&s, "feat", "main", &Default::default()));
    assert!(has(&s, "urn:c") && has(&s, "urn:f") && !has(&s, "urn:d"));
    // deleting the last child lets the retired branch go
    s.delete_branch("feat", false).unwrap();
    assert!(!branch_dirs(dir.path()).contains(&dev_id.to_string()));
    assert_eq!(table(dir.path())["format"], 1);
    assert!(table(dir.path()).get("retired").is_none());
}

#[test]
fn a29_a_crash_in_a_deletion_that_re_parents() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let re = crate::branch::DeleteOptions {
        force: true,
        reparent: true,
    };
    // after the table names dev retired
    let (dir, s, dev_id) = family();
    let feat_dump = dump(&s.branch("feat").unwrap());
    s.set_failpoint(
        "branch-delete-committed",
        Some(Arc::new(|_: &Store| panic!("crash"))),
    );
    assert!(catch_unwind(AssertUnwindSafe(|| s.delete_branch_with("dev", &re))).is_err());
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert_eq!(code(&s.branch("dev").err().unwrap()), "no-such-branch");
    assert_eq!(dump(&s.branch("feat").unwrap()), feat_dump);
    assert!(branch_dirs(dir.path()).contains(&dev_id.to_string()));
    // the last child goes, and the retired branch with it, crashing mid-removal
    let feat_id = s.branch_id_of("feat").unwrap();
    s.set_failpoint(
        "branch-delete-renamed",
        Some(Arc::new(|_: &Store| panic!("crash"))),
    );
    assert!(catch_unwind(AssertUnwindSafe(|| s.delete_branch("feat", true))).is_err());
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert!(
        branch_dirs(dir.path()).is_empty(),
        "{:?}",
        branch_dirs(dir.path())
    );
    assert_eq!(s.branches().unwrap().len(), 1);
    let t = table(dir.path());
    assert_eq!(t["format"], 1);
    let gone: Vec<String> = t["deleted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect();
    assert!(gone.contains(&dev_id.to_string()) && gone.contains(&feat_id.to_string()));
}

#[test]
fn a30_exempt_predicates_never_conflict() {
    let (dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    let label = "http://www.w3.org/2000/01/rdf-schema#label";
    apply(&s, &format!("+<urn:a> <{label}> \"ours\" ."));
    apply(&dev, &format!("+<urn:a> <{label}> \"theirs\" ."));
    apply(&dev, "+<urn:a> <urn:other> <urn:x> .");
    let MergeOutcome::Conflicts(c) = merge(&s, "dev", "main", &Default::default()) else {
        panic!("labels conflict by default");
    };
    assert_eq!(c.conflicts, 1);
    // per merge
    let o = MergeOptions {
        exempt: vec![named(label)],
        ..Default::default()
    };
    let p = s.preview_merge("dev", "main", &o).unwrap();
    assert!(p.conflicts.is_none());
    assert_eq!((p.inserted, p.conflicts_found), (2, 0));
    // per dataset, kept in the branch table
    assert!(s.merge_exempt().unwrap().is_empty());
    let set = s.set_merge_exempt(&[named(label), named(label)]).unwrap();
    assert_eq!(set, [named(label)]);
    drop(dev);
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert_eq!(s.merge_exempt().unwrap(), [named(label)]);
    merged(merge(&s, "dev", "main", &Default::default()));
    assert!(has(&s, "\"ours\"") && has(&s, "\"theirs\""));
    // an exempt cell takes both sides' changes in the subject scope too
    let dev = s.branch("dev").unwrap();
    apply(
        &s,
        &format!(
            "-<urn:a> <urn:age> {} .\n+<urn:a> <urn:age> {} .",
            int(30),
            int(31)
        ),
    );
    apply(
        &dev,
        &format!(
            "-<urn:a> <urn:age> {} .\n+<urn:a> <urn:age> {} .",
            int(30),
            int(32)
        ),
    );
    let o = MergeOptions {
        exempt: vec![named("urn:age")],
        scope: crate::branch::ConflictScope::Subject,
        ..Default::default()
    };
    merged(merge(&s, "dev", "main", &o));
    assert_eq!(super::branch_tests::ages(&s), ["31", "32"]);
    s.set_merge_exempt(&[]).unwrap();
    assert!(s.merge_exempt().unwrap().is_empty());
}

#[test]
fn a31_merges_report_progress_and_stop_when_cancelled() {
    use crate::task::{Cancel, Control, Progress};
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    apply(&s.branch("dev").unwrap(), "+<urn:c> <urn:p> <urn:x> .");
    // a cancel that comes while the merge commits publishes nothing
    let cancel = Cancel::new();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let (c2, s2) = (cancel.clone(), seen.clone());
    let ctl = Control {
        cancel: cancel.clone(),
        progress: Progress::new(move |_, m| {
            s2.lock().push(m.to_string());
            if m == "committing" {
                c2.cancel();
            }
        }),
        deadline: None,
    };
    let o = MergeOptions::default().with_control(&ctl);
    assert!(matches!(s.merge("dev", "main", &o), Err(Error::Cancelled)));
    assert_eq!(s.head_commit().seq, 2);
    assert!(s.merge_record(3).is_none());
    assert_eq!(
        *seen.lock(),
        [
            "reading the changes of both sides",
            "finding conflicts",
            "committing"
        ]
    );
    // the same merge without the cancel goes through and reports its end
    seen.lock().clear();
    let s3 = seen.clone();
    let ctl = Control {
        progress: Progress::new(move |p, m| s3.lock().push(format!("{p} {m}"))),
        ..Control::none()
    };
    merged(
        s.merge("dev", "main", &MergeOptions::default().with_control(&ctl))
            .unwrap(),
    );
    assert_eq!(seen.lock().last().map(String::as_str), Some("1 committed"));
}
