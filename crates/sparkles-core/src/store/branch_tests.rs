//! Branches and merges: the acceptance examples of F09 at the library level.

use super::*;
use crate::branch::{
    BranchOptions, CommitRef, ConflictScope, MergeOptions, MergeOutcome, Resolution, Take,
};
use crate::history::At;
use std::collections::BTreeSet;

const XSD_INT: &str = "http://www.w3.org/2001/XMLSchema#integer";

pub(super) fn int(n: i64) -> String {
    format!("\"{n}\"^^<{XSD_INT}>")
}

/// Apply lines `+<s> <p> <o> .` and `-…` (N-Quads) in one commit. Blank-node labels of
/// stored nodes (`_:b…`) name them; others are new nodes scoped to the call.
pub(super) fn apply(s: &Store, lines: &str) -> Receipt {
    let mut t = s.write();
    let mut labels = std::collections::HashMap::new();
    for line in lines.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (op, rest) = line.split_at(1);
        let q = oxttl::NQuadsParser::new()
            .for_slice(rest.as_bytes())
            .next()
            .expect("a quad")
            .expect("valid N-Quads");
        // a stored node's label names it; other labels make new nodes
        for b in [&q.subject]
            .into_iter()
            .filter_map(|s| match s {
                NamedOrBlankNode::BlankNode(b) => Some(b.clone()),
                _ => None,
            })
            .chain(match &q.object {
                Term::BlankNode(b) => Some(b.clone()),
                _ => None,
            })
        {
            if let Some(id) = parse_bnode_label(b.as_str()) {
                labels.insert(b.as_str().to_string(), id);
            }
        }
        let ids = t.encode_quad(&q, &mut labels).unwrap();
        match op {
            "+" => {
                t.insert(ids).unwrap();
            }
            "-" => {
                t.delete(ids).unwrap();
            }
            _ => panic!("bad op {op}"),
        }
    }
    t.commit().unwrap()
}

pub(super) fn dump_snap(snap: &Snapshot) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    snap.for_each_quad(|q| {
        out.insert(crate::annotations::nquads_line(
            &snap.quad_to_terms(q).unwrap(),
        ));
        Ok(())
    })
    .unwrap();
    out
}

pub(super) fn dump(s: &Store) -> BTreeSet<String> {
    dump_snap(&s.snapshot())
}

pub(super) fn has(s: &Store, needle: &str) -> bool {
    dump(s).iter().any(|l| l.contains(needle))
}

pub(super) fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    apply(&s, &format!("+<urn:a> <urn:age> {} .", int(30)));
    apply(&s, "+<urn:b> <urn:name> \"B\" .");
    (dir, s)
}

pub(super) fn merge(s: &Store, src: &str, tgt: &str, o: &MergeOptions) -> MergeOutcome {
    s.merge(src, tgt, o).unwrap()
}

pub(super) fn merged(o: MergeOutcome) -> crate::branch::MergeReport {
    match o {
        MergeOutcome::Merged(r) => r,
        o => panic!("not merged: {o:?}"),
    }
}

pub(super) fn code(e: &Error) -> &'static str {
    match e {
        Error::Branch(b) => b.code,
        e => panic!("not a branch error: {e}"),
    }
}

#[test]
fn a1_create_and_isolate() {
    let (dir, s) = setup();
    let info = s.create_branch("dev", &BranchOptions::default()).unwrap();
    assert_eq!(info.from.as_ref().unwrap().seq, 2);
    assert!(info.storage.linked);
    let bdir = dir.path().join("ds/branches").join(info.id.to_string());
    assert!(bdir.join("gen-0001/link.json").exists());
    assert!(
        !bdir.join("gen-0001/meta.json").exists(),
        "no index is built"
    );
    let dev = s.branch("dev").unwrap();
    assert_eq!(dump(&dev), dump(&s));
    // the branch reads main's index files through the same cached blocks
    let (a, b) = (dev.snapshot(), s.snapshot());
    for (x, y) in a.generation.perms.iter().zip(b.generation.perms.iter()) {
        assert_eq!(x.uid, y.uid);
    }
    assert!(Arc::ptr_eq(dev.cache(), s.cache()));
    assert_eq!(apply(&dev, "+<urn:c> <urn:p> <urn:x> .").commit.seq, 3);
    assert_eq!(apply(&s, "+<urn:d> <urn:p> <urn:x> .").commit.seq, 3);
    assert!(has(&s, "urn:d") && !has(&s, "urn:c"));
    assert!(has(&dev, "urn:c") && !has(&dev, "urn:d"));
    let log = s
        .branch_commits("dev", CommitRange::Latest, 10)
        .unwrap()
        .into_iter()
        .map(|c| (c.commit.seq, c.branch.unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(
        log,
        vec![
            (3, "dev".to_string()),
            (2, "main".into()),
            (1, "main".into()),
            (0, "main".into())
        ]
    );
    // a reopen replays the inherited segment and the branch's own log
    let (main_dump, dev_dump) = (dump(&s), dump(&dev));
    drop(dev);
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert_eq!(dump(&s), main_dump);
    assert_eq!(dump(&s.branch("dev").unwrap()), dev_dump);
    assert_eq!(s.branches().unwrap().len(), 2);
}

#[test]
fn a3_fast_forward_and_up_to_date() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:c> <urn:p> <urn:x> .");
    let p = s.preview_merge("dev", "main", &Default::default()).unwrap();
    assert!(p.fast_forward);
    assert_eq!(p.inserted, 1);
    let r = merged(merge(&s, "dev", "main", &Default::default()));
    let c = r.commit.unwrap().commit;
    assert_eq!((c.seq, c.kind), (3, CommitKind::Merge));
    assert_eq!(dump(&s), dump(&dev));
    let m = s.merge_record(3).unwrap();
    assert_eq!(m.source.seq, 3);
    assert_eq!(m.source.branch_id, dev.dataset_id());
    assert!(matches!(
        merge(&s, "dev", "main", &Default::default()),
        MergeOutcome::UpToDate(_)
    ));
    assert_eq!(s.head_commit().seq, 3);
}

#[test]
fn a4_clean_three_way_merge() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:c> <urn:p> <urn:x> .");
    apply(&s, "+<urn:d> <urn:p> <urn:x> .");
    let only = MergeOptions {
        ff_only: true,
        ..Default::default()
    };
    assert_eq!(
        code(&s.merge("dev", "main", &only).unwrap_err()),
        "not-fast-forward"
    );
    let dev_before = dump(&dev);
    let r = merged(merge(&s, "dev", "main", &Default::default()));
    assert!(!r.fast_forward);
    assert_eq!(r.base.as_ref().unwrap().seq, 2);
    assert_eq!(r.base.as_ref().unwrap().branch.as_deref(), Some("main"));
    for x in ["urn:a", "urn:b", "urn:c", "urn:d"] {
        assert!(has(&s, x), "{x}");
    }
    assert_eq!(dump(&dev), dev_before);
}

pub(super) fn a5_state() -> (tempfile::TempDir, Store) {
    let (dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
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
    drop(dev);
    (dir, s)
}

pub(super) fn ages(s: &Store) -> Vec<String> {
    dump(s)
        .into_iter()
        .filter(|l| l.contains("urn:age"))
        .map(|l| l.split('"').nth(1).unwrap().to_string())
        .collect()
}

#[test]
fn a5_cell_conflicts_and_resolutions() {
    let (_dir, s) = a5_state();
    let head = s.head_commit().seq;
    let MergeOutcome::Conflicts(c) = merge(&s, "dev", "main", &Default::default()) else {
        panic!("expected conflicts");
    };
    assert_eq!(c.conflicts, 1);
    assert_eq!(c.cells[0].base, vec![int(30)]);
    assert_eq!(c.cells[0].ours, vec![int(31)]);
    assert_eq!(c.cells[0].theirs, vec![int(32)]);
    assert_eq!(c.cells[0].subject, "<urn:a>");
    assert_eq!(c.cells[0].predicate.as_deref(), Some("<urn:age>"));
    assert_eq!(s.head_commit().seq, head, "nothing is committed");

    for (o, want) in [
        (
            MergeOptions {
                on_conflict: Some(Take::Theirs),
                ..Default::default()
            },
            vec!["32"],
        ),
        (
            MergeOptions {
                resolutions: vec![Resolution {
                    graph: GraphName::DefaultGraph,
                    subject: Some(Term::NamedNode(named("urn:a"))),
                    predicate: Some(named("urn:age")),
                    take: Take::Objects(vec![Term::Literal(oxrdf::Literal::new_typed_literal(
                        "33",
                        named(XSD_INT),
                    ))]),
                }],
                ..Default::default()
            },
            vec!["33"],
        ),
        (
            MergeOptions {
                on_conflict: Some(Take::Union),
                ..Default::default()
            },
            vec!["31", "32"],
        ),
        (
            MergeOptions {
                scope: ConflictScope::Quad,
                ..Default::default()
            },
            vec!["31", "32"],
        ),
        (
            MergeOptions {
                on_conflict: Some(Take::Ours),
                ..Default::default()
            },
            vec!["31"],
        ),
        (
            MergeOptions {
                on_conflict: Some(Take::Base),
                ..Default::default()
            },
            vec!["30"],
        ),
    ] {
        let (_d, s) = a5_state();
        let r = merged(merge(&s, "dev", "main", &o));
        assert_eq!(ages(&s), want);
        assert_eq!(r.conflicts_found, u64::from(o.scope != ConflictScope::Quad));
    }
    // both sides making the same change do not conflict
    let (_d, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let change = format!(
        "-<urn:a> <urn:age> {} .\n+<urn:a> <urn:age> {} .",
        int(30),
        int(31)
    );
    apply(&s, &change);
    apply(&s.branch("dev").unwrap(), &change);
    let r = merged(merge(&s, "dev", "main", &Default::default()));
    assert_eq!(r.conflicts_found, 0);
    // a merge commit is made even with no net change
    assert_eq!(r.commit.unwrap().commit.inserted, 0);
    assert_eq!(ages(&s), vec!["31"]);
}

#[test]
fn a6_subject_scope() {
    let (_dir, s) = setup();
    apply(&s, "+<urn:a> <urn:name> \"A\" .");
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(
        &s,
        &format!(
            "-<urn:a> <urn:age> {} .\n-<urn:a> <urn:name> \"A\" .",
            int(30)
        ),
    );
    apply(
        &dev,
        &format!(
            "-<urn:a> <urn:age> {} .\n+<urn:a> <urn:age> {} .",
            int(30),
            int(40)
        ),
    );
    let MergeOutcome::Conflicts(c) = merge(&s, "dev", "main", &Default::default()) else {
        panic!()
    };
    assert_eq!(c.cells.len(), 1);
    assert_eq!(c.cells[0].predicate.as_deref(), Some("<urn:age>"));
    let sub = MergeOptions {
        scope: ConflictScope::Subject,
        ..Default::default()
    };
    let MergeOutcome::Conflicts(c) = merge(&s, "dev", "main", &sub) else {
        panic!()
    };
    assert_eq!(c.cells.len(), 1);
    assert_eq!(c.cells[0].predicate, None);
    assert_eq!(c.cells[0].subject, "<urn:a>");
    // with the cell scope, resolved for the age, the name only main deleted is deleted
    let o = MergeOptions {
        on_conflict: Some(Take::Ours),
        ..Default::default()
    };
    merged(merge(&s, "dev", "main", &o));
    assert!(!has(&s, "\"A\""));
}

#[test]
fn a7_stale_resolutions() {
    let (_dir, s) = a5_state();
    let MergeOutcome::Conflicts(c) = merge(&s, "dev", "main", &Default::default()) else {
        panic!()
    };
    apply(&s, "+<urn:e> <urn:p> <urn:x> .");
    let o = MergeOptions {
        expect_source: Some(c.source.seq),
        expect_target: Some(c.target.seq),
        on_conflict: Some(Take::Theirs),
        ..Default::default()
    };
    assert_eq!(code(&s.merge("dev", "main", &o).unwrap_err()), "head-moved");
    assert!(matches!(
        merge(&s, "dev", "main", &Default::default()),
        MergeOutcome::Conflicts(_)
    ));
    let o = MergeOptions {
        resolutions: vec![Resolution {
            graph: GraphName::DefaultGraph,
            subject: Some(Term::NamedNode(named("urn:b"))),
            predicate: None,
            take: Take::Ours,
        }],
        on_conflict: Some(Take::Ours),
        ..Default::default()
    };
    assert_eq!(
        code(&s.merge("dev", "main", &o).unwrap_err()),
        "invalid-merge"
    );
}

#[test]
fn a8_blank_nodes_keep_their_labels_and_structures_stay_whole() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    apply(&s, "+<urn:a> <urn:addr> _:x .\n+_:x <urn:city> \"Oslo\" .");
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    let label = |st: &Store| {
        dump(st)
            .into_iter()
            .find(|l| l.contains("urn:addr"))
            .unwrap()
            .split(' ')
            .nth(2)
            .unwrap()
            .to_string()
    };
    let x = label(&s);
    assert_eq!(label(&dev), x);
    // a node made on dev carries dev's ordinal in its high bits
    apply(&dev, "+<urn:z> <urn:p> _:new .");
    let z = dump(&dev)
        .into_iter()
        .find(|l| l.contains("urn:z"))
        .unwrap()
        .split(' ')
        .nth(2)
        .unwrap()
        .to_string();
    let payload = crate::id::parse_bnode_payload(z.trim_start_matches("_:")).unwrap();
    assert_eq!(crate::branch::bnode_ordinal(payload), 1);
    merged(merge(&s, "dev", "main", &Default::default()));
    assert!(
        dump(&s).iter().any(|l| l.contains(&z)),
        "the label survives the merge"
    );
    apply(&s, "+<urn:y> <urn:p> _:other .");
    let y = dump(&s).into_iter().find(|l| l.contains("urn:y")).unwrap();
    assert!(!y.contains(&z));
    // both sides replace the address: resolving as ours leaves no orphan of dev's
    let swap = |st: &Store, city: &str| {
        apply(
            st,
            &format!(
                "-<urn:a> <urn:addr> {x} .\n-{x} <urn:city> \"Oslo\" .\n+<urn:a> <urn:addr> _:n .\n+_:n <urn:city> \"{city}\" ."
            ),
        );
    };
    swap(&s, "Bergen");
    swap(&dev, "Trondheim");
    let MergeOutcome::Conflicts(c) = merge(&s, "dev", "main", &Default::default()) else {
        panic!()
    };
    assert_eq!(c.cells.len(), 1);
    let ours = MergeOptions {
        on_conflict: Some(Take::Ours),
        ..Default::default()
    };
    merged(merge(&s, "dev", "main", &ours));
    let d = dump(&s);
    assert!(!d.iter().any(|l| l.contains("Trondheim")), "{d:?}");
    assert!(d.iter().any(|l| l.contains("Bergen")));
}

#[test]
fn a8_theirs_removes_ours_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    apply(&s, "+<urn:a> <urn:addr> <urn:old> .");
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(
        &s,
        "-<urn:a> <urn:addr> <urn:old> .\n+<urn:a> <urn:addr> _:m .\n+_:m <urn:city> \"Bergen\" .",
    );
    apply(
        &dev,
        "-<urn:a> <urn:addr> <urn:old> .\n+<urn:a> <urn:addr> _:d .\n+_:d <urn:city> \"Trondheim\" .",
    );
    let theirs = MergeOptions {
        on_conflict: Some(Take::Theirs),
        ..Default::default()
    };
    merged(merge(&s, "dev", "main", &theirs));
    let d = dump(&s);
    assert!(!d.iter().any(|l| l.contains("Bergen")), "{d:?}");
    assert!(d.iter().any(|l| l.contains("Trondheim")));
    assert_eq!(d.len(), 2, "{d:?}");
}

#[test]
fn a9_inferred_graph_is_left_out_unless_included() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(
        &dev,
        "+<urn:t> <urn:type> <urn:C> .\n+<urn:t> <urn:type> <urn:D> <urn:x-sparkles:inferred> .",
    );
    let r = merged(merge(&s, "dev", "main", &Default::default()));
    assert_eq!(r.inferences_excluded, Some(1));
    assert!(has(&s, "<urn:C>") && !has(&s, "<urn:D>"));
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    apply(
        &s.branch("dev").unwrap(),
        "+<urn:t> <urn:type> <urn:D> <urn:x-sparkles:inferred> .",
    );
    let o = MergeOptions {
        include_inferences: true,
        ..Default::default()
    };
    merged(merge(&s, "dev", "main", &o));
    assert!(has(&s, "<urn:D>"));
}

#[test]
fn a11_protected_branches_take_merges_only() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    s.set_branch_protected("main", true).unwrap();
    let mut t = s.write();
    let q = t
        .encode_quad(
            &Quad::new(
                named("urn:q"),
                named("urn:p"),
                named("urn:x"),
                GraphName::DefaultGraph,
            ),
            &mut Default::default(),
        )
        .unwrap();
    t.insert(q).unwrap();
    assert_eq!(code(&t.commit().unwrap_err()), "branch-protected");
    apply(&s.branch("dev").unwrap(), "+<urn:c> <urn:p> <urn:x> .");
    merged(merge(&s, "dev", "main", &Default::default()));
    assert!(has(&s, "urn:c"));
    s.set_branch_protected("main", false).unwrap();
    apply(&s, "+<urn:q> <urn:p> <urn:x> .");
}

#[test]
fn a12_deletion() {
    let (dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let id = s.branch_id_of("dev").unwrap();
    apply(&s.branch("dev").unwrap(), "+<urn:c> <urn:p> <urn:x> .");
    assert_eq!(
        code(&s.delete_branch("dev", false).unwrap_err()),
        "unmerged"
    );
    assert_eq!(
        code(&s.delete_branch("main", false).unwrap_err()),
        "invalid-branch"
    );
    s.create_branch(
        "child",
        &BranchOptions {
            from: "dev".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        code(&s.delete_branch("dev", true).unwrap_err()),
        "has-children"
    );
    s.delete_branch("child", true).unwrap();
    s.delete_branch("dev", true).unwrap();
    assert!(!dir.path().join("ds/branches").join(id.to_string()).exists());
    assert_eq!(code(&s.branch("dev").err().unwrap()), "no-such-branch");
    // a merged branch deletes without force; the name can be used again
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    assert_ne!(s.branch_id_of("dev").unwrap(), id);
    apply(&s.branch("dev").unwrap(), "+<urn:c> <urn:p> <urn:x> .");
    merged(merge(&s, "dev", "main", &Default::default()));
    s.delete_branch("dev", false).unwrap();
}

#[test]
fn a13_holds_keep_the_upstream_generation_until_the_branch_compacts() {
    let (dir, s) = setup();
    s.compact().unwrap();
    let held = s.snapshot().generation.name.clone();
    apply(&s, "+<urn:m1> <urn:p> <urn:x> .");
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:d1> <urn:p> <urn:x> .");
    // main moves on, so its next generation's base is past dev's starting commit
    apply(&s, "+<urn:m2> <urn:p> <urn:x> .");
    let before = dump(&dev);
    s.compact().unwrap();
    assert_ne!(s.snapshot().generation.name, held);
    let h = s.history();
    let g = h.generations.iter().find(|g| g.name == held).unwrap();
    let holds: Vec<String> = g.held_by.iter().map(|h| h.to_string()).collect();
    assert!(holds.contains(&"branch:dev".to_string()), "{holds:?}");
    assert!(holds.contains(&"branch-base:dev".to_string()), "{holds:?}");
    assert!(dir.path().join("ds").join(&held).exists());
    assert_eq!(dump(&dev), before);
    // the branch's own rebuild releases the link hold
    dev.compact().unwrap();
    assert_eq!(dev.snapshot().generation.name, "gen-0002");
    assert!(dev.snapshot().generation.linked().is_none());
    let info = s.branch_info("dev").unwrap();
    assert!(!info.storage.linked);
    s.compact().unwrap();
    let h = s.history();
    let holds: Vec<String> = h
        .generations
        .iter()
        .filter(|g| g.name == held)
        .flat_map(|g| g.held_by.iter().map(|h| h.to_string()))
        .collect();
    assert_eq!(holds, vec!["branch-base:dev".to_string()]);
    assert_eq!(dump(&dev), before);
    // a merge still works after the branch rebuilt
    merged(merge(&s, "dev", "main", &Default::default()));
    assert!(has(&s, "urn:d1"));
    drop(dev);
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert!(has(&s.branch("dev").unwrap(), "urn:d1"));
}

#[test]
fn a14_reading_inherited_commits() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:c> <urn:p> <urn:x> .");
    dev.create_snapshot("x", &At::Head, None).unwrap();
    s.create_snapshot("onmain", &At::Commit(1), None).unwrap();
    let (snap, r) = s
        .branch_snapshot_at("dev", &At::Commit(1), &Default::default())
        .unwrap();
    assert_eq!(r.commit.seq, 1);
    assert_eq!(dump_snap(&snap).len(), 1);
    let e = dev
        .snapshot_at(&At::Commit(1), &Default::default())
        .err()
        .unwrap();
    assert_eq!(crate::branch::inherited_commit(&e).unwrap().seq, 1);
    let (snap, _) = s
        .branch_snapshot_at("dev", &At::Snapshot("x".into()), &Default::default())
        .unwrap();
    assert!(dump_snap(&snap).iter().any(|l| l.contains("urn:c")));
    assert!(matches!(
        s.branch_snapshot_at("dev", &At::Snapshot("onmain".into()), &Default::default()),
        Err(Error::NotFound(_))
    ));
    // the starting commit itself is readable on the branch
    let (snap, _) = dev
        .snapshot_at(&At::Commit(2), &Default::default())
        .unwrap();
    assert_eq!(dump_snap(&snap), {
        let (m, _) = s.snapshot_at(&At::Commit(2), &Default::default()).unwrap();
        dump_snap(&m)
    });
}

#[test]
fn a15_crash_during_creation() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    for (point, exists) in [
        ("branch-create-built", false),
        ("branch-create-renamed", false),
        ("branch-create-committed", true),
    ] {
        let (dir, s) = setup();
        s.set_failpoint(point, Some(Arc::new(|_: &Store| panic!("crash"))));
        let r = catch_unwind(AssertUnwindSafe(|| {
            s.create_branch("dev", &BranchOptions::default())
        }));
        assert!(r.is_err());
        drop(s);
        let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
        let dirs: Vec<String> = std::fs::read_dir(dir.path().join("ds/branches"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        if exists {
            assert_eq!(dirs.len(), 1, "{point}: {dirs:?}");
            assert_eq!(dump(&s.branch("dev").unwrap()), dump(&s));
        } else {
            assert!(dirs.is_empty(), "{point}: {dirs:?}");
            assert!(s.branch("dev").is_err(), "{point}");
            // and the name is free
            s.create_branch("dev", &BranchOptions::default()).unwrap();
        }
    }
}

#[test]
fn a15_crash_between_the_merge_record_and_the_log() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let (dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    apply(&s.branch("dev").unwrap(), "+<urn:c> <urn:p> <urn:x> .");
    apply(&s, "+<urn:d> <urn:p> <urn:x> .");
    let head = s.head_commit().seq;
    s.set_failpoint(
        "merge-recorded",
        Some(Arc::new(|_: &Store| panic!("crash"))),
    );
    let r = catch_unwind(AssertUnwindSafe(|| {
        s.merge("dev", "main", &Default::default())
    }));
    assert!(r.is_err());
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, head);
    assert!(s.merge_record(head + 1).is_none());
    assert_eq!(
        std::fs::metadata(dir.path().join("ds/merges.bin"))
            .unwrap()
            .len(),
        0
    );
    // and the merge goes through after the reopen
    merged(merge(&s, "dev", "main", &Default::default()));
    assert!(has(&s, "urn:c"));
}

#[test]
fn a15_a_generation_only_a_branch_holds_survives_main_collection() {
    let (dir, s) = setup();
    s.compact().unwrap();
    let held = s.snapshot().generation.name.clone();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev_dump = dump(&s.branch("dev").unwrap());
    s.compact().unwrap();
    s.compact().unwrap();
    drop(s);
    assert!(dir.path().join("ds").join(&held).exists());
    // main opened alone honours the holds of branches.json
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert!(dir.path().join("ds").join(&held).exists());
    assert_eq!(dump(&s.branch("dev").unwrap()), dev_dump);
}

#[test]
fn a17_criss_cross_merges_need_a_chosen_base() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&s, "+<urn:m1> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d1> <urn:p> <urn:x> .");
    // each merges the other's head as it was before either merge
    let (m_head, d_head) = (s.head_commit().seq, dev.head_commit().seq);
    merged(merge(&s, "dev", "main", &Default::default()));
    let o = MergeOptions {
        expect_target: Some(d_head),
        ..Default::default()
    };
    // merge main's pre-merge head into dev: emulate with a merge record of main@m_head
    let r = s.merge("main", "dev", &o).unwrap();
    let _ = (r, m_head);
    apply(&s, "+<urn:m2> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d2> <urn:p> <urn:x> .");
    let bases = s.merge_base("dev", "main").unwrap();
    // main merged dev@3, dev merged main@4 (which holds dev@3): dev@3 ⊂ main@4, a
    // single base
    assert_eq!(bases.len(), 1, "{bases:?}");
    merged(merge(&s, "dev", "main", &Default::default()));
    assert_eq!(dump(&s), {
        let mut d = dump(&dev);
        d.insert(dump(&s).into_iter().find(|l| l.contains("urn:m2")).unwrap());
        d
    });
}

#[test]
fn a17_true_criss_cross_uses_a_virtual_base() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&s, "+<urn:m1> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d1> <urn:p> <urn:x> .");
    let main_set = s.branching.set().unwrap();
    let (m1, d1) = (s.head_commit().seq, dev.head_commit().seq);
    // main merges dev@d1 and dev merges main@m1, each from the other's pre-merge head
    merged(merge(&s, "dev", "main", &Default::default()));
    {
        // a merge of main@m1 into dev, made by hand: the toggles of main since the base
        let set = main_set.clone();
        let base = set
            .merge_bases(
                CommitRef {
                    branch_id: s.dataset_id(),
                    seq: m1,
                },
                CommitRef {
                    branch_id: dev.dataset_id(),
                    seq: d1,
                },
            )
            .unwrap()[0];
        let t = s
            .toggles(
                &set,
                base,
                CommitRef {
                    branch_id: s.dataset_id(),
                    seq: m1,
                },
                &Default::default(),
            )
            .unwrap();
        let mut txn = dev.write_as(CommitKind::Merge);
        txn.force = true;
        txn.merge = Some(branching::MergeRec {
            seq: 0,
            source: CommitRef {
                branch_id: s.dataset_id(),
                seq: m1,
            },
            resolved: 0,
            flags: 0,
        });
        for (k, add) in t {
            let q = merge::tests_quad_ids(&mut txn, &k).unwrap();
            if add {
                txn.insert(q).unwrap();
            } else {
                txn.delete(q).unwrap();
            }
        }
        txn.commit().unwrap();
    }
    apply(&s, "+<urn:m2> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d2> <urn:p> <urn:x> .");
    assert_eq!(s.merge_base("dev", "main").unwrap().len(), 2);
    let head = s.head_commit().seq;
    let preview = s.preview_merge("dev", "main", &Default::default()).unwrap();
    assert!(preview.base.is_none());
    assert_eq!(preview.conflicts_found, 0);
    assert_eq!(s.head_commit().seq, head, "a virtual base never commits");
    let report = merged(merge(&s, "dev", "main", &Default::default()));
    assert!(report.base.is_none());
    assert_eq!(report.inserted, 1);
    for x in ["urn:m1", "urn:m2", "urn:d1", "urn:d2"] {
        assert!(has(&s, x), "{x}");
    }
}

/// Make a merge with a historical second parent to construct genuine criss-cross
/// histories, without publishing fake merge records or bypassing the merge planner.
pub(super) fn merge_historical(s: &Store, source: &str, seq: u64, target: &str, o: &MergeOptions) {
    let set = s.owned_set().unwrap();
    let source = s.branch(source).unwrap();
    let target = s.branch(target).unwrap();
    let sc = CommitRef {
        branch_id: source.dataset_id(),
        seq,
    };
    let tc = CommitRef {
        branch_id: target.dataset_id(),
        seq: target.head_commit().seq,
    };
    let base = set.merge_bases(sc, tc).unwrap()[0];
    let report =
        crate::branch::MergeReport::new(set.named(sc), set.named(tc), Some(set.named(base)));
    let step = s
        .three_way(
            set,
            &target,
            report,
            base,
            sc,
            tc,
            o,
            false,
            merge::Writing {
                kind: CommitKind::Merge,
                message: "historical merge".into(),
                record: Some(branching::MergeRec {
                    seq: 0,
                    source: sc,
                    resolved: 0,
                    flags: 0,
                }),
                force: true,
                what: "historical merge".into(),
            },
        )
        .unwrap();
    assert!(matches!(step, merge::Step::Done(MergeOutcome::Merged(_))));
}

#[test]
fn virtual_base_conflicts_fail_closed_and_explicit_base_remains_available() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
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
    let m1 = s.head_commit().seq;
    let theirs = MergeOptions {
        on_conflict: Some(Take::Theirs),
        ..Default::default()
    };
    merged(merge(&s, "dev", "main", &theirs));
    merge_historical(&s, "main", m1, "dev", &theirs);
    let before = dump(&s);
    let head = s.head_commit().seq;
    // A final merge rule must not silently resolve ambiguity in the ancestors.
    let e = s.merge("dev", "main", &theirs).unwrap_err();
    assert_eq!(code(&e), "ambiguous-merge-base");
    let Error::Branch(e) = e else { unreachable!() };
    assert_eq!(e.candidates.len(), 2);
    assert_eq!(s.head_commit().seq, head);
    assert_eq!(dump(&s), before);
    let explicit = MergeOptions {
        base: Some(CommitRef {
            branch_id: e.candidates[0].branch_id,
            seq: e.candidates[0].seq,
        }),
        ..theirs
    };
    merged(merge(&s, "dev", "main", &explicit));
}

#[test]
fn recursive_virtual_bases_preserve_blank_nodes_and_survive_restart() {
    let (dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&s, "+_:main <urn:p> \"main\" .");
    apply(&dev, "+_:dev <urn:p> \"dev\" .");
    for level in 0..4 {
        let before = s.head_commit().seq;
        merged(merge(&s, "dev", "main", &Default::default()));
        merge_historical(&s, "main", before, "dev", &Default::default());
        apply(&s, &format!("+<urn:m{level}> <urn:p> <urn:x> ."));
        apply(&dev, &format!("+<urn:d{level}> <urn:p> <urn:x> ."));
        assert_eq!(s.merge_base("main", "dev").unwrap().len(), 2);
    }
    let expected: BTreeSet<_> = dump(&s).union(&dump(&dev)).cloned().collect();
    drop(dev);
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    let cancelled = MergeOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(
        s.merge("dev", "main", &cancelled),
        Err(Error::Cancelled)
    ));
    let limited = MergeOptions {
        max_quads: 1,
        ..Default::default()
    };
    assert!(matches!(
        s.merge("dev", "main", &limited),
        Err(Error::BudgetExceeded(_))
    ));
    let before = s.head_commit().seq;
    let report = merged(merge(&s, "dev", "main", &Default::default()));
    assert!(report.base.is_none());
    assert_eq!(s.head_commit().seq, before + 1);
    assert_eq!(dump(&s), expected);
    assert_eq!(dump(&s).iter().filter(|q| q.starts_with("_:b")).count(), 2);
}

#[test]
fn three_virtual_base_candidates_merge_symmetrically() {
    let (_dir, s) = setup();
    s.create_branch("a", &BranchOptions::default()).unwrap();
    s.create_branch("b", &BranchOptions::default()).unwrap();
    let a = s.branch("a").unwrap();
    let b = s.branch("b").unwrap();
    apply(&s, "+<urn:main1> <urn:p> <urn:x> .");
    apply(&a, "+<urn:a1> <urn:p> <urn:x> .");
    apply(&b, "+<urn:b1> <urn:p> <urn:x> .");
    let main1 = s.head_commit().seq;
    merged(merge(&s, "a", "main", &Default::default()));
    merged(merge(&s, "b", "main", &Default::default()));
    merge_historical(&s, "main", main1, "a", &Default::default());
    merged(merge(&s, "b", "a", &Default::default()));
    apply(&s, "+<urn:main2> <urn:p> <urn:x> .");
    apply(&a, "+<urn:a2> <urn:p> <urn:x> .");
    assert_eq!(s.merge_base("main", "a").unwrap().len(), 3);
    let left = s.preview_merge("a", "main", &Default::default()).unwrap();
    let right = s.preview_merge("main", "a", &Default::default()).unwrap();
    assert!(left.base.is_none() && right.base.is_none());
    assert_eq!((left.inserted, left.deleted), (1, 0));
    assert_eq!((right.inserted, right.deleted), (1, 0));
    assert_eq!(left.conflicts_found + right.conflicts_found, 0);
    merged(merge(&s, "a", "main", &Default::default()));
    for term in ["urn:main1", "urn:main2", "urn:a1", "urn:a2", "urn:b1"] {
        assert!(has(&s, term));
    }
}

#[test]
fn memory_virtual_base_retains_fork_snapshots() {
    let s = Store::in_memory(StoreOptions::default());
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&s, "+<urn:m> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d> <urn:p> <urn:x> .");
    let before = s.head_commit().seq;
    merged(merge(&s, "dev", "main", &Default::default()));
    merge_historical(&s, "main", before, "dev", &Default::default());
    apply(&s, "+<urn:m2> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d2> <urn:p> <urn:x> .");
    let report = merged(merge(&s, "dev", "main", &Default::default()));
    assert!(report.base.is_none());
    assert_eq!(report.inserted, 1);
    for term in ["urn:m", "urn:d", "urn:m2", "urn:d2"] {
        assert!(has(&s, term));
    }
}

/// A criss-cross whose ancestors both changed the same cell, which the merges
/// accepted under `o`. The next merge under `o` synthesizes the virtual base with
/// the same options, so it merges instead of reporting an ambiguous base.
fn criss_cross_same_cell(p: &str, o: &MergeOptions) {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&s, &format!("+<urn:a> <{p}> <urn:T1> ."));
    apply(&dev, &format!("+<urn:a> <{p}> <urn:T2> ."));
    let m1 = s.head_commit().seq;
    merged(merge(&s, "dev", "main", o));
    merge_historical(&s, "main", m1, "dev", o);
    apply(&s, "+<urn:m2> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d2> <urn:p> <urn:x> .");
    assert_eq!(s.merge_base("dev", "main").unwrap().len(), 2);
    let report = merged(merge(&s, "dev", "main", o));
    assert!(report.base.is_none());
    assert_eq!((report.inserted, report.deleted), (1, 0));
    for term in ["urn:T1", "urn:T2", "urn:m2", "urn:d2"] {
        assert!(has(&s, term), "{term}");
    }
}

#[test]
fn virtual_base_honours_exempt_predicates() {
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let o = MergeOptions {
        exempt: vec![NamedNode::new(rdf_type).unwrap()],
        ..Default::default()
    };
    criss_cross_same_cell(rdf_type, &o);
}

#[test]
fn virtual_base_honours_the_quad_scope() {
    let o = MergeOptions {
        scope: ConflictScope::Quad,
        ..Default::default()
    };
    criss_cross_same_cell("urn:kind", &o);
}

#[test]
fn virtual_base_carries_deletes_of_both_ancestors() {
    let (_dir, s) = setup();
    apply(&s, "+<urn:c> <urn:p> <urn:x> .\n+<urn:e> <urn:p> <urn:x> .");
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    // each side deletes a different quad that the other keeps
    apply(
        &s,
        "-<urn:c> <urn:p> <urn:x> .\n+<urn:m1> <urn:p> <urn:x> .",
    );
    apply(
        &dev,
        &format!(
            "-<urn:e> <urn:p> <urn:x> .\n-<urn:a> <urn:age> {} .",
            int(30)
        ),
    );
    let m1 = s.head_commit().seq;
    merged(merge(&s, "dev", "main", &Default::default()));
    merge_historical(&s, "main", m1, "dev", &Default::default());
    // after the criss-cross each side deletes one more quad
    apply(&s, "-<urn:b> <urn:name> \"B\" .");
    apply(&dev, "-<urn:m1> <urn:p> <urn:x> .");
    assert_eq!(s.merge_base("dev", "main").unwrap().len(), 2);
    let preview = s.preview_merge("dev", "main", &Default::default()).unwrap();
    assert!(preview.base.is_none());
    assert_eq!((preview.inserted, preview.deleted), (0, 1));
    let report = merged(merge(&s, "dev", "main", &Default::default()));
    assert!(report.base.is_none());
    assert_eq!((report.inserted, report.deleted), (0, 1));
    // nothing deleted on either side comes back
    for term in ["urn:c>", "urn:e>", "urn:age", "urn:name", "urn:m1"] {
        assert!(!has(&s, term), "{term}");
    }
    // and the other way, main's later delete reaches dev
    let back = merged(merge(&s, "main", "dev", &Default::default()));
    assert_eq!(back.deleted, 1);
    assert_eq!(dump(&s), dump(&dev));
}

#[test]
fn relink_preserves_history_blank_nodes_and_child_branches_after_restart() {
    let (dir, s) = setup();
    s.compact().unwrap();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+_:dev <urn:p> \"first\" .");
    let pinned = dev.head_commit().seq;
    dev.create_snapshot("before", &At::Head, None).unwrap();
    let before = dump(&dev);
    apply(&dev, "+<urn:dev> <urn:p> <urn:branch-only> .");
    let head = dev.head_commit().seq;
    let id = dev.dataset_id();
    let expected = dump(&dev);
    apply(
        &s,
        "-<urn:b> <urn:name> \"B\" .\n+<urn:main> <urn:p> <urn:main-only> .",
    );
    s.compact().unwrap();
    let shared = s.snapshot().generation.name.clone();
    let report = s.relink_branch("dev", &Default::default()).unwrap();
    assert_eq!(report.mode, "relink");
    assert_eq!((dev.dataset_id(), dev.head_commit().seq), (id, head));
    assert_eq!(dump(&dev), expected);
    assert_eq!(
        dump_snap(
            &dev.snapshot_at(&At::Commit(pinned), &Default::default())
                .unwrap()
                .0
        ),
        before
    );
    assert!(
        dev.snapshot()
            .generation
            .linked()
            .unwrap()
            .base_dir()
            .ends_with(&shared)
    );
    assert!(
        !dev.snapshot()
            .generation
            .dir
            .as_ref()
            .unwrap()
            .join("spo.dat")
            .exists()
    );
    s.create_branch(
        "child",
        &BranchOptions {
            from: "dev".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(dump(&s.branch("child").unwrap()), expected);
    drop(dev);
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    assert_eq!(dump(&dev), expected);
    assert_eq!(dump(&s.branch("child").unwrap()), expected);
    assert_eq!(
        dump_snap(
            &dev.snapshot_at(&At::Snapshot("before".into()), &Default::default())
                .unwrap()
                .0
        ),
        before
    );
    apply(&dev, "+_:dev <urn:p> \"second\" .");
    let nodes: BTreeSet<_> = dump(&dev)
        .iter()
        .filter(|line| line.starts_with("_:b"))
        .map(|line| line.split(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(
        nodes.len(),
        2,
        "blank-node allocation must not reuse overlay IDs"
    );
    s.compact().unwrap();
    assert!(
        dir.path().join("ds").join(&shared).exists(),
        "the relinked generation stays held"
    );
}

#[test]
fn relink_catches_up_concurrent_branch_commits_without_changing_their_identity() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:before> <urn:p> <urn:x> .");
    apply(&s, "+<urn:upstream> <urn:p> <urn:x> .");
    s.compact().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let done_rx = Arc::new(Mutex::new(done_rx));
    dev.set_failpoint(
        "compact-built",
        Some(Arc::new(move |_| {
            ready_tx.send(()).unwrap();
            done_rx
                .lock()
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
        })),
    );
    std::thread::scope(|scope| {
        let build = scope.spawn(|| s.relink_branch("dev", &Default::default()).unwrap());
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        let receipt = apply(&dev, "+_:during <urn:p> \"catch-up\" .");
        done_tx.send(()).unwrap();
        let report = build.join().unwrap();
        assert_eq!(report.caught_up_commits, 1);
        assert_eq!(dev.head_commit().seq, receipt.commit.seq);
        assert_eq!(
            dev.commit(receipt.commit.seq).unwrap().kind,
            receipt.commit.kind
        );
    });
    assert!(has(&dev, "catch-up"));
    assert!(!has(&dev, "upstream"));
}

#[test]
fn relink_crash_boundaries_recover_the_same_state_and_pins() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    for point in [
        "compact-built",
        "compact-before-current",
        "relink-held",
        "relink-current",
    ] {
        let (dir, s) = setup();
        s.create_branch("dev", &BranchOptions::default()).unwrap();
        let dev = s.branch("dev").unwrap();
        apply(&dev, "+_:dev <urn:p> \"before-crash\" .");
        dev.create_snapshot("keep", &At::Head, None).unwrap();
        let expected = dump(&dev);
        let head = dev.head_commit().seq;
        apply(&s, "+<urn:later> <urn:p> <urn:x> .");
        s.compact().unwrap();
        dev.set_failpoint(point, Some(Arc::new(|_| panic!("crash"))));
        assert!(
            catch_unwind(AssertUnwindSafe(
                || s.relink_branch("dev", &Default::default())
            ))
            .is_err()
        );
        drop(dev);
        drop(s);
        let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
        let dev = s.branch("dev").unwrap();
        assert_eq!(dev.head_commit().seq, head, "{point}");
        assert_eq!(dump(&dev), expected, "{point}");
        assert_eq!(
            dump_snap(
                &dev.snapshot_at(&At::Snapshot("keep".into()), &Default::default())
                    .unwrap()
                    .0
            ),
            expected,
            "{point}"
        );
        apply(&dev, "+_:fresh <urn:p> \"after-crash\" .");
        assert_eq!(
            dump(&dev)
                .iter()
                .filter(|line| line.starts_with("_:b"))
                .count(),
            2,
            "{point}"
        );
    }
}

/// A relink that crashed after recording its hold on main's generation, and before
/// the branch's `CURRENT` named the relinked generation, leaves a hold that nothing
/// needs. Main's open releases it even when the branch is never opened again, and
/// main's next compaction can then collect that generation.
#[test]
fn main_open_releases_the_hold_of_an_unpublished_relink() {
    let (dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:dev> <urn:p> <urn:x> .");
    apply(&s, "+<urn:later> <urn:p> <urn:x> .");
    s.compact().unwrap();
    let shared = s.snapshot().generation.name.clone();
    let number = commit::generation_number(&shared);
    let held = |s: &Store| {
        let set = s.owned_set().unwrap();
        set.holds_on(s.dataset_id())
            .0
            .iter()
            .any(|(no, _)| *no == number)
    };
    // The image of the files at the crash point. An in-process failure releases the
    // hold as it unwinds, so only a crash leaves it behind.
    let image = dir.path().join("crash");
    let (from, to) = (dir.path().join("ds"), image.clone());
    dev.set_failpoint(
        "relink-held",
        Some(Arc::new(move |_| {
            super::compaction_tests::copy_dir(&from, &to)
        })),
    );
    s.relink_branch("dev", &Default::default()).unwrap();
    drop(dev);
    drop(s);
    let held_on_disk = || {
        let t: serde_json::Value =
            serde_json::from_slice(&std::fs::read(image.join(BRANCHES_FILE)).unwrap()).unwrap();
        t["branches"].as_array().unwrap().iter().any(|e| {
            e["holds"]
                .as_array()
                .is_some_and(|h| h.iter().any(|h| h["generation"] == shared.as_str()))
        })
    };
    assert!(held_on_disk(), "the hold precedes the branch's CURRENT");
    let s = Store::open(&image, StoreOptions::default()).unwrap();
    assert!(!held(&s), "main's open released the stale hold");
    assert!(!held_on_disk(), "the release is durable");
    apply(&s, "+<urn:after> <urn:p> <urn:x> .");
    s.compact().unwrap();
    assert!(
        !image.join(&shared).exists(),
        "nothing holds the generation of the unpublished relink"
    );
    // the branch still reads its own linked generation
    let dev = s.branch("dev").unwrap();
    assert!(has(&dev, "urn:dev") && !has(&dev, "urn:later"));
}

#[test]
fn cancelled_relink_and_damaged_overlay_fail_closed() {
    let (dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:branch> <urn:p> <urn:x> .");
    s.compact().unwrap();
    let generation = dev.snapshot().generation.name.clone();
    let cancelled = CompactOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(
        s.relink_branch("dev", &cancelled),
        Err(Error::Cancelled)
    ));
    assert_eq!(dev.snapshot().generation.name, generation);
    s.relink_branch("dev", &Default::default()).unwrap();
    let overlay = dev
        .snapshot()
        .generation
        .dir
        .as_ref()
        .unwrap()
        .join(link::OVERLAY_FILE);
    s.create_branch(
        "child",
        &BranchOptions {
            from: "dev".into(),
            ..Default::default()
        },
    )
    .unwrap();
    drop(dev);
    drop(s);
    let mut bytes = std::fs::read(&overlay).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&overlay, &bytes).unwrap();
    assert!(matches!(
        link::read_link_file(overlay.parent().unwrap()),
        Err(Error::Corrupt(_))
    ));
    for quick in [false, true] {
        let report = crate::check::check(
            &dir.path().join("ds"),
            &crate::check::CheckOptions { quick },
        )
        .unwrap();
        assert_eq!(report.status, crate::check::Status::Error);
        assert!(report.checks.iter().any(|c| {
            c.name == "branches"
                && c.issues
                    .iter()
                    .any(|i| i.message.contains("invalid base overlay"))
        }));
    }
    assert_eq!(std::fs::read(&overlay).unwrap(), bytes);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert!(matches!(s.branch("dev"), Err(Error::Corrupt(_))));
    assert!(matches!(s.branch("child"), Err(Error::Corrupt(_))));
}

#[test]
fn relink_capture_materializes_parent_and_child_without_overlay_dependencies() {
    let (dir, s) = setup();
    s.create_branch("dev", &Default::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+_:dev <urn:p> \"first\" .");
    apply(&s, "+<urn:main-only> <urn:p> <urn:x> .");
    s.compact().unwrap();
    s.relink_branch("dev", &Default::default()).unwrap();
    s.create_branch(
        "child",
        &BranchOptions {
            from: "dev".into(),
            ..Default::default()
        },
    )
    .unwrap();
    for name in ["dev", "child"] {
        let branch = s.branch(name).unwrap();
        let expected = dump(&branch);
        let capture = branch
            .materialized_backup_capture(
                name,
                &MemoryCaptureOptions {
                    tmp_dir: dir.path().join(format!("scratch-{name}")),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(capture.materialized && !capture.in_memory);
        assert!(
            !capture
                .files
                .iter()
                .any(|file| file.path.ends_with("link.json") || file.path.ends_with("base.delta"))
        );
        let path = dir.path().join(format!("restored-{name}"));
        capture.write_to(&path).unwrap();
        let restored = Store::open(&path, Default::default()).unwrap();
        assert_eq!(dump(&restored), expected);
        assert!(restored.snapshot().generation.linked().is_none());
        apply(&restored, "+_:fresh <urn:p> \"second\" .");
        assert_eq!(
            dump(&restored)
                .iter()
                .filter(|q| q.starts_with("_:b"))
                .count(),
            2
        );
    }
}

#[test]
fn relink_cancellation_waiting_for_branch_writer_does_not_wait_for_its_release() {
    let (_dir, s) = setup();
    s.create_branch("dev", &Default::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    let writer = dev.write();
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let flag = cancel.clone();
        scope.spawn(|| {
            tx.send(s.relink_branch(
                "dev",
                &CompactOptions {
                    cancel: Some(flag),
                    ..Default::default()
                },
            ))
            .unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(30));
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap(),
            Err(Error::Cancelled)
        ));
        drop(writer);
    });
}

#[test]
fn late_rebuild_cancellation_does_not_wait_for_retained_writer() {
    let mut blocked = Vec::new();
    for relink in [false, true] {
        for point in [
            "compact-indexed",
            "compact-catching-up",
            "compact-caught-up",
        ] {
            let (_dir, s) = setup();
            s.create_branch("dev", &Default::default()).unwrap();
            let dev = s.branch("dev").unwrap();
            let before = dump(&dev);
            let generation = dev.snapshot().generation.name.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (resume_tx, resume_rx) = std::sync::mpsc::channel();
            let resume_rx = Mutex::new(resume_rx);
            dev.set_failpoint(
                point,
                Some(Arc::new(move |_| {
                    entered_tx.send(()).unwrap();
                    resume_rx.lock().recv().unwrap();
                })),
            );
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let options = CompactOptions {
                        cancel: Some(cancel.clone()),
                        ..Default::default()
                    };
                    let result = if relink {
                        s.relink_branch("dev", &options)
                    } else {
                        dev.compact_with(&options)
                    };
                    result_tx.send(result).unwrap();
                });
                entered_rx
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .unwrap();
                let mut writer = dev.write();
                // Admission waits are deliberately entered with cancellation still off;
                // the cleanup checkpoint cancels before the worker can leave its hook.
                if point == "compact-indexed" {
                    cancel.store(true, Ordering::Relaxed);
                }
                resume_tx.send(()).unwrap();
                if point != "compact-indexed" {
                    cancel.store(true, Ordering::Relaxed);
                }
                let result = result_rx.recv_timeout(std::time::Duration::from_millis(300));
                if result.is_err() {
                    blocked.push((relink, point));
                }
                // A retained writer is still usable after rebuild cancellation. This also
                // drains an inactive tap lazily without waiting for its former run.
                let g = writer
                    .intern(&Term::NamedNode(
                        NamedNode::new("urn:cancel-writer").unwrap(),
                    ))
                    .unwrap();
                let p = writer
                    .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
                    .unwrap();
                let o = writer
                    .intern(&Term::NamedNode(NamedNode::new("urn:o").unwrap()))
                    .unwrap();
                writer.insert([g, p, o, Id::DEFAULT_GRAPH]).unwrap();
                writer.commit().unwrap();
                let result = result.unwrap_or_else(|_| {
                    result_rx
                        .recv_timeout(std::time::Duration::from_secs(3))
                        .unwrap()
                });
                assert!(matches!(result, Err(Error::Cancelled)));
            });
            dev.set_failpoint(point, None);
            assert_eq!(dev.snapshot().generation.name, generation);
            assert!(dump(&dev).is_superset(&before));
            assert!(dev.writer.lock().tap.is_none());
            assert!(!dev.compaction.running.load(Ordering::Acquire));
            let after = dump(&dev);
            s.relink_branch("dev", &Default::default()).unwrap();
            assert_eq!(dump(&dev), after);
        }
    }
    assert!(
        blocked.is_empty(),
        "cancellation waited for writer: {blocked:?}"
    );
}

#[test]
fn a20_diff_across_branches() {
    let (_dir, s) = setup();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(
        &dev,
        "+<urn:c> <urn:p> <urn:x> .\n-<urn:b> <urn:name> \"B\" .",
    );
    apply(&s, "+<urn:d> <urn:p> <urn:x> .");
    let d = s
        .branch_diff("main", &At::Head, "dev", &At::Head, &Default::default())
        .unwrap();
    let (main, dev_d) = (dump(&s), dump(&dev));
    let added: BTreeSet<String> = dev_d.difference(&main).cloned().collect();
    let removed: BTreeSet<String> = main.difference(&dev_d).cloned().collect();
    let got_add: BTreeSet<String> = d
        .iter()
        .filter(|(op, _)| *op == diff::DiffOp::Add)
        .map(|(_, q)| crate::annotations::nquads_line(&q))
        .collect();
    let got_rm: BTreeSet<String> = d
        .iter()
        .filter(|(op, _)| *op == diff::DiffOp::Remove)
        .map(|(_, q)| crate::annotations::nquads_line(&q))
        .collect();
    assert_eq!(got_add, added);
    assert_eq!(got_rm, removed);
}

#[test]
fn branches_of_branches_and_the_depth_limit() {
    let dir = tempfile::tempdir().unwrap();
    let opts = StoreOptions {
        max_branch_depth: 2,
        ..Default::default()
    };
    let s = Store::open(&dir.path().join("ds"), opts).unwrap();
    apply(&s, "+<urn:a> <urn:p> <urn:x> .");
    let mut from = "main".to_string();
    for (i, name) in ["b1", "b2", "b3"].iter().enumerate() {
        s.create_branch(
            name,
            &BranchOptions {
                from: from.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        let b = s.branch(name).unwrap();
        apply(&b, &format!("+<urn:{name}> <urn:p> <urn:x> ."));
        assert_eq!(dump(&b).len(), i + 2);
        from = name.to_string();
    }
    // b3's link would chain three segments: it was built instead
    let info = s.branch_info("b3").unwrap();
    assert!(!info.storage.linked);
    assert_eq!(info.storage.generation, "gen-0001");
    assert!(s.branch_info("b2").unwrap().storage.linked);
    let want = dump(&s.branch("b3").unwrap());
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert_eq!(dump(&s.branch("b3").unwrap()), want);
    // merges up the chain
    merged(merge(&s, "b3", "b2", &Default::default()));
    merged(merge(&s, "b2", "b1", &Default::default()));
    merged(merge(&s, "b1", "main", &Default::default()));
    assert_eq!(dump(&s), want);
}

#[test]
fn limits_and_names() {
    let dir = tempfile::tempdir().unwrap();
    let opts = StoreOptions {
        max_branches: 2,
        ..Default::default()
    };
    let s = Store::open(&dir.path().join("ds"), opts).unwrap();
    for bad in ["", "head", "123", "-x", "a/b", &"x".repeat(65)] {
        assert_eq!(
            code(&s.create_branch(bad, &Default::default()).unwrap_err()),
            "invalid-branch",
            "{bad}"
        );
    }
    assert_eq!(
        code(&s.create_branch("main", &Default::default()).unwrap_err()),
        "branch-exists"
    );
    s.create_branch("dev", &Default::default()).unwrap();
    assert_eq!(
        code(&s.create_branch("dev", &Default::default()).unwrap_err()),
        "branch-exists"
    );
    assert_eq!(
        code(&s.create_branch("dev2", &Default::default()).unwrap_err()),
        "branch-limit"
    );
    let mem = Store::in_memory(Default::default());
    mem.create_branch("dev", &Default::default()).unwrap();
    assert_eq!(mem.branches().unwrap().len(), 2);
}

/// A21: random histories. After each merge with `union` and the quad scope, the
/// target equals the three-way rule applied to dumps of the base, ours and theirs.
#[test]
fn a21_random_histories_follow_the_three_way_rule() {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let runs: usize = std::env::var("SPARKLES_A21_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    for run in 0..runs {
        let dir = tempfile::tempdir().unwrap();
        let opts = StoreOptions {
            bulk_threshold: 40,
            ..Default::default()
        };
        let s = Store::open(&dir.path().join("ds"), opts).unwrap();
        let names = ["main", "b1", "b2"];
        // a dump per (branch, seq), for the base state
        let mut states: std::collections::HashMap<(uuid::Uuid, u64), BTreeSet<String>> =
            Default::default();
        let record = |s: &Store, st: &mut std::collections::HashMap<_, _>, name: &str| {
            let b = s.branch(name).unwrap();
            st.insert((b.dataset_id(), b.head_commit().seq), dump(&b));
        };
        record(&s, &mut states, "main");
        let mut created = vec!["main"];
        for _step in 0..30 {
            let r = next() % 100;
            let name = created[(next() as usize) % created.len()];
            if r < 55 {
                // a write: inserts and deletes over a small universe
                let b = s.branch(name).unwrap();
                let mut lines = String::new();
                for _ in 0..(1 + next() % 4) {
                    let (sx, px, ox) = (next() % 4, next() % 3, next() % 4);
                    let g = if next() % 4 == 0 { " <urn:g>" } else { "" };
                    let op = if next() % 3 == 0 { "-" } else { "+" };
                    lines.push_str(&format!("{op}<urn:s{sx}> <urn:p{px}> <urn:o{ox}>{g} .\n"));
                }
                if next() % 5 == 0 {
                    lines.push_str("+<urn:s0> <urn:p0> _:fresh .\n");
                }
                apply(&b, &lines);
                drop(b);
                record(&s, &mut states, name);
            } else if r < 62 {
                // a bulk commit or a compaction
                let b = s.branch(name).unwrap();
                if next() % 2 == 0 {
                    b.compact().unwrap();
                } else {
                    let mut nt = String::new();
                    for i in 0..50 {
                        nt.push_str(&format!(
                            "<urn:bulk{run}x{i}> <urn:p0> <urn:o{}> .\n",
                            i % 3
                        ));
                    }
                    b.load(&[Source::from_bytes(
                        nt.into_bytes(),
                        crate::io::RdfFormat::NTriples,
                        None,
                    )])
                    .unwrap();
                    drop(b);
                    record(&s, &mut states, name);
                }
            } else if r < 72 && created.len() < names.len() {
                let new = names[created.len()];
                s.create_branch(
                    new,
                    &BranchOptions {
                        from: name.to_string(),
                        ..Default::default()
                    },
                )
                .unwrap();
                created.push(new);
                record(&s, &mut states, new);
            } else if created.len() > 1 {
                let other = created[(next() as usize) % created.len()];
                if other == name {
                    continue;
                }
                let (src, tgt) = (s.branch(other).unwrap(), s.branch(name).unwrap());
                let sc = CommitRef {
                    branch_id: src.dataset_id(),
                    seq: src.head_commit().seq,
                };
                let tc = CommitRef {
                    branch_id: tgt.dataset_id(),
                    seq: tgt.head_commit().seq,
                };
                let (theirs, ours) = (dump(&src), dump(&tgt));
                drop((src, tgt));
                let set = s.branching.set().unwrap();
                let bases = set.merge_bases(sc, tc).unwrap();
                let base = bases[0];
                let o = MergeOptions {
                    scope: ConflictScope::Quad,
                    on_conflict: Some(Take::Union),
                    base: Some(base),
                    include_inferences: true,
                    ..Default::default()
                };
                let base_state = states
                    .get(&(base.branch_id, base.seq))
                    .cloned()
                    .unwrap_or_else(|| {
                        let n = set.name_of(base.branch_id).unwrap();
                        let (snap, _) = s
                            .branch_snapshot_at(&n, &At::Commit(base.seq), &Default::default())
                            .unwrap();
                        dump_snap(&snap)
                    });
                let out = s.merge(other, name, &o).unwrap();
                let tgt = s.branch(name).unwrap();
                let got = dump(&tgt);
                drop(tgt);
                // the three-way rule on the dumps, blank nodes by label
                let mut want = BTreeSet::new();
                for q in base_state.iter().chain(&ours).chain(&theirs) {
                    let (b, o_, t) = (base_state.contains(q), ours.contains(q), theirs.contains(q));
                    let keep = if o_ == t {
                        o_
                    } else if o_ == b {
                        t
                    } else {
                        o_
                    };
                    if keep {
                        want.insert(q.clone());
                    }
                }
                assert_eq!(got, want, "run {run}: merge {other} into {name}: {out:?}");
                record(&s, &mut states, name);
            }
        }
        // everything survives a reopen
        let all: Vec<(String, BTreeSet<String>)> = created
            .iter()
            .map(|n| (n.to_string(), dump(&s.branch(n).unwrap())))
            .collect();
        drop(s);
        let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
        for (n, d) in all {
            assert_eq!(
                dump(&s.branch(&n).unwrap()),
                d,
                "run {run}: {n} after reopen"
            );
        }
    }
}

/// With the cell scope, the reported conflicts are those computed from the dumps.
#[test]
fn a21_cell_conflicts_match_the_dumps() {
    let mut x = 0x1234_5678_9abc_def1u64;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for run in 0..30 {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
        let mut init = String::new();
        for i in 0..6 {
            init.push_str(&format!(
                "+<urn:s{}> <urn:p{}> <urn:o{i}> .\n",
                i % 3,
                i % 2
            ));
        }
        apply(&s, &init);
        s.create_branch("dev", &Default::default()).unwrap();
        let dev = s.branch("dev").unwrap();
        let base = dump(&s);
        for st in [&s, &*dev] {
            let mut lines = String::new();
            for _ in 0..3 {
                let op = if next() % 2 == 0 { "-" } else { "+" };
                lines.push_str(&format!(
                    "{op}<urn:s{}> <urn:p{}> <urn:o{}> .\n",
                    next() % 3,
                    next() % 2,
                    next() % 6
                ));
            }
            apply(st, &lines);
        }
        let (ours, theirs) = (dump(&s), dump(&dev));
        let cell = |l: &String| l.split(' ').take(2).collect::<Vec<_>>().join(" ");
        let cells: BTreeSet<String> = base.iter().chain(&ours).chain(&theirs).map(cell).collect();
        let mut want = BTreeSet::new();
        for c in cells {
            let pick = |d: &BTreeSet<String>| -> BTreeSet<String> {
                d.iter().filter(|l| cell(l) == c).cloned().collect()
            };
            let (b, o, t) = (pick(&base), pick(&ours), pick(&theirs));
            if b != o && b != t && o != t {
                want.insert(c);
            }
        }
        let got: BTreeSet<String> = match s.merge("dev", "main", &Default::default()).unwrap() {
            MergeOutcome::Conflicts(c) => c
                .cells
                .iter()
                .map(|c| format!("{} {}", c.subject, c.predicate.clone().unwrap()))
                .collect(),
            _ => BTreeSet::new(),
        };
        assert_eq!(got, want, "run {run}");
    }
}

/// A linked branch whose delta adds no terms compacts partially, copying its upstream's
/// vocabulary.
#[test]
fn a_linked_branch_compacts_partially_from_its_upstreams_files() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    let mut nt = String::new();
    for i in 0..2000 {
        nt.push_str(&format!("<urn:s{i}> <urn:p> <urn:o{}> .\n", i % 50));
    }
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        crate::io::RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    // only terms the base vocabulary has, and numbers
    apply(
        &dev,
        "+<urn:s1> <urn:p> <urn:o7> .\n-<urn:s2> <urn:p> <urn:o2> .\n+<urn:s3> <urn:p> \"5\"^^<http://www.w3.org/2001/XMLSchema#integer> .",
    );
    let before = dump(&dev);
    let r = dev
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.mode, "partial", "{r:?}");
    assert_eq!(dump(&dev), before);
    assert!(dev.snapshot().generation.linked().is_none());
    drop(dev);
    drop(s);
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    assert_eq!(dump(&s.branch("dev").unwrap()), before);
}

/// A18: the first rebuild of a linked branch adds a full index, so it is refused over the
/// dataset's quota, and the compaction status says so; writes that fit go on.
#[test]
fn a18_the_first_rebuild_of_a_linked_branch_keeps_the_quota() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let mut nt = String::new();
    for i in 0..5000 {
        nt.push_str(&format!("<urn:s{i}> <urn:p> \"value {i}\" .\n"));
    }
    s.load(&[Source::from_bytes(
        nt.into_bytes(),
        crate::io::RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s.create_branch("dev", &BranchOptions::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:new> <urn:p> <urn:x> .");
    // the change logs' background writer records the load and the branch's commit
    // after them: wait for it, and measure the directory afresh, or a later walk finds
    // more bytes than the quota was set from
    s.flush_change_log().unwrap();
    dev.flush_change_log().unwrap();
    s.quota.invalidate();
    let used = s.disk_usage();
    s.set_quota(Some(used + (64 << 10))).unwrap();
    let b = dev.compaction_blocker(false).expect("the quota blocks it");
    assert_eq!(b.reason, "quota", "{}", b.detail);
    let e = dev.compact().unwrap_err();
    assert!(
        matches!(&e, Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::DatasetBytes),
        "{e}"
    );
    assert!(dev.snapshot().generation.linked().is_some());
    let id = s.branch_id_of("dev").unwrap();
    assert!(
        !root
            .join("branches")
            .join(id.to_string())
            .join("gen-0002")
            .exists()
    );
    // writes that fit still succeed
    apply(&dev, "+<urn:new2> <urn:p> <urn:x> .");
    assert!(has(&dev, "urn:new2"));
}
