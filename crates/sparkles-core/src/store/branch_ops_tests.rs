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
