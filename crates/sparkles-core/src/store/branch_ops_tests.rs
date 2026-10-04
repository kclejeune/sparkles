//! Branches and merges, Phase 2: squash merges, reverts, cherry-picks, replayed
//! fast-forwards, renames, deletions that re-parent, exempt predicates and merges as
//! tasks. The acceptance examples A24 onward of F09, at the library level.

use super::branch_tests::{apply, code, dump, has, int, merge, merged, setup};
use super::*;
use crate::branch::{BranchOptions, MergeOptions, MergeOutcome};

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
