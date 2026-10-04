//! Branches and merges: the acceptance examples of F09 at the library level.

use super::*;
use crate::branch::{
    BranchOptions, CommitRef, ConflictScope, MergeOptions, MergeOutcome, Resolution, Take,
};
use crate::history::At;
use std::collections::BTreeSet;

const XSD_INT: &str = "http://www.w3.org/2001/XMLSchema#integer";

fn int(n: i64) -> String {
    format!("\"{n}\"^^<{XSD_INT}>")
}

/// Apply lines `+<s> <p> <o> .` and `-…` (N-Quads) in one commit. Blank-node labels of
/// stored nodes (`_:b…`) name them; others are new nodes scoped to the call.
fn apply(s: &Store, lines: &str) -> Receipt {
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

fn dump_snap(snap: &Snapshot) -> BTreeSet<String> {
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

fn dump(s: &Store) -> BTreeSet<String> {
    dump_snap(&s.snapshot())
}

fn has(s: &Store, needle: &str) -> bool {
    dump(s).iter().any(|l| l.contains(needle))
}

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("ds"), StoreOptions::default()).unwrap();
    apply(&s, &format!("+<urn:a> <urn:age> {} .", int(30)));
    apply(&s, "+<urn:b> <urn:name> \"B\" .");
    (dir, s)
}

fn merge(s: &Store, src: &str, tgt: &str, o: &MergeOptions) -> MergeOutcome {
    s.merge(src, tgt, o).unwrap()
}

fn merged(o: MergeOutcome) -> crate::branch::MergeReport {
    match o {
        MergeOutcome::Merged(r) => r,
        o => panic!("not merged: {o:?}"),
    }
}

fn code(e: &Error) -> &'static str {
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
    assert!(bdir.join("gen-0000/link.json").exists());
    assert!(
        !bdir.join("gen-0000/meta.json").exists(),
        "no index is built"
    );
    let dev = s.branch("dev").unwrap();
    assert_eq!(dump(&dev), dump(&s));
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

fn a5_state() -> (tempfile::TempDir, Store) {
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

fn ages(s: &Store) -> Vec<String> {
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
    assert_eq!(dev.snapshot().generation.name, "gen-0001");
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
fn a17_true_criss_cross_is_ambiguous() {
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
    let e = s.merge("dev", "main", &Default::default()).unwrap_err();
    assert_eq!(code(&e), "ambiguous-merge-base");
    let Error::Branch(b) = &e else { unreachable!() };
    assert_eq!(b.candidates.len(), 2);
    let pick = &b.candidates[0];
    let o = MergeOptions {
        base: Some(CommitRef {
            branch_id: pick.branch_id,
            seq: pick.seq,
        }),
        ..Default::default()
    };
    merged(merge(&s, "dev", "main", &o));
    for x in ["urn:m1", "urn:m2", "urn:d1", "urn:d2"] {
        assert!(has(&s, x), "{x}");
    }
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
    assert_eq!(
        code(&mem.create_branch("dev", &Default::default()).unwrap_err()),
        "branches-unsupported"
    );
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
