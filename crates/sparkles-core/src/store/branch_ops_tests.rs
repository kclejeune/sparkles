//! Branches and merges, Phase 2: squash merges, reverts, cherry-picks, replayed
//! fast-forwards, renames, deletions that re-parent, exempt predicates and merges as
//! tasks. The acceptance examples A24 onward of F09, at the library level.

use super::branch_tests::{apply, dump, has, merge, merged, setup};
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
