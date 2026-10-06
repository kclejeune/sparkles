//! Delta invariants through writes, retained views, historical replay and relinking.

use oxrdf::{GraphName, Literal, NamedNode, Quad};
use sparkles_core::branch::BranchOptions;
use sparkles_core::history::{At, HistoryOptions};
use sparkles_core::index::Perm;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::CompactOptions;
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use std::collections::{BTreeSet, HashMap};

fn quad(n: i64) -> Quad {
    Quad::new(
        NamedNode::new(format!("urn:subject:{n}")).unwrap(),
        NamedNode::new("urn:predicate").unwrap(),
        Literal::from(n),
        if n % 2 == 0 {
            GraphName::DefaultGraph
        } else {
            NamedNode::new("urn:graph").unwrap().into()
        },
    )
}

fn contents(s: &Snapshot) -> BTreeSet<String> {
    s.scan_keys(Perm::Spo, &[])
        .unwrap()
        .iter()
        .map(|k| s.quad_to_terms(&Perm::Spo.to_quad(k)).unwrap().to_string())
        .collect()
}

fn check(s: &Snapshot, expected: &BTreeSet<String>) {
    assert_eq!(s.len(), expected.len() as u64);
    for permutation in Perm::ALL {
        let keys = s.scan_keys(permutation, &[]).unwrap();
        assert_eq!(keys.len(), expected.len(), "{permutation:?}");
        let actual: BTreeSet<_> = keys
            .iter()
            .map(|k| {
                s.quad_to_terms(&permutation.to_quad(k))
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(&actual, expected, "{permutation:?}");
    }
}

fn seed(s: &Store) {
    let mut text: String = (0..48).map(|n| format!("{} .\n", quad(n))).collect();
    text.push_str("_:seed <urn:predicate> <urn:blank-object> .\n");
    s.load(&[Source::from_bytes(
        text.into_bytes(),
        RdfFormat::NQuads,
        None,
    )])
    .unwrap();
    let mut txn = s.write();
    let mut labels = HashMap::new();
    // Both opposite sets are nonempty and shared at subsequent admission.
    for n in 0..8 {
        let ids = txn.encode_quad(&quad(n), &mut labels).unwrap();
        assert!(txn.delete(ids).unwrap());
    }
    for n in 80..96 {
        let ids = txn.encode_quad(&quad(n), &mut labels).unwrap();
        assert!(txn.insert(ids).unwrap());
    }
    txn.commit().unwrap();
}

fn exercise(s: &Store) -> BTreeSet<String> {
    let mut expected = contents(&s.snapshot());
    let mut retained = Vec::new();
    // Starts cover physical-base-present, physical-base-deleted, delta-inserted
    // and absent quads. Each transaction also exercises duplicates and undo.
    for target in [16, 0, 80, 200] {
        for operations in [
            vec![true, true],
            vec![false, false],
            vec![true, false],
            vec![false, true],
            vec![true, false, true],
            vec![false, true, false],
        ] {
            retained.push((s.snapshot(), expected.clone()));
            let before = expected.clone();
            let mut txn = s.write();
            let q = quad(target);
            let ids = txn.encode_quad(&q, &mut HashMap::new()).unwrap();
            for insert in operations {
                let changed = if insert {
                    expected.insert(q.to_string())
                } else {
                    expected.remove(&q.to_string())
                };
                assert_eq!(
                    if insert {
                        txn.insert(ids)
                    } else {
                        txn.delete(ids)
                    }
                    .unwrap(),
                    changed
                );
                check(&txn.view(), &expected);
            }
            let receipt = txn.commit().unwrap();
            let inserted = expected.difference(&before).count() as u64;
            let deleted = before.difference(&expected).count() as u64;
            assert_eq!(receipt.committed, inserted + deleted > 0);
            if receipt.committed {
                assert_eq!(receipt.commit.inserted, inserted);
                assert_eq!(receipt.commit.deleted, deleted);
            }
            check(&s.snapshot(), &expected);
            for (held, state) in &retained {
                check(held, state);
            }
        }
    }
    expected
}

#[test]
fn mixed_delta_sequences_preserve_every_order_and_retained_snapshot() {
    let memory = Store::in_memory(StoreOptions::default());
    seed(&memory);
    exercise(&memory);

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let disk = Store::open(&root, StoreOptions::default()).unwrap();
    seed(&disk);
    let original = contents(&disk.snapshot());
    disk.create_snapshot("seed", &At::Head, None).unwrap();
    let expected = exercise(&disk);
    check(
        &disk
            .snapshot_at(&At::Snapshot("seed".into()), &HistoryOptions::default())
            .unwrap()
            .0,
        &original,
    );
    drop(disk);
    let disk = Store::open(&root, StoreOptions::default()).unwrap();
    check(&disk.snapshot(), &expected);
    disk.compact().unwrap();
    check(&disk.snapshot(), &expected);
    check(
        &disk
            .snapshot_at(&At::Snapshot("seed".into()), &HistoryOptions::default())
            .unwrap()
            .0,
        &original,
    );
}

#[test]
fn linked_delta_sequences_survive_relink_reopen_and_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let disk = Store::open(&root, StoreOptions::default()).unwrap();
    seed(&disk);
    let main = contents(&disk.snapshot());
    disk.create_branch("dev", &BranchOptions::default())
        .unwrap();
    let branch = disk.branch("dev").unwrap();
    let original = contents(&branch.snapshot());
    branch.create_snapshot("seed", &At::Head, None).unwrap();
    let expected = exercise(&branch);
    let head = branch.head_commit();
    disk.compact().unwrap();
    disk.relink_branch("dev", &CompactOptions::default())
        .unwrap();
    assert_eq!(branch.head_commit(), head);
    check(&branch.snapshot(), &expected);
    check(&disk.snapshot(), &main);
    drop(branch);
    drop(disk);
    let disk = Store::open(&root, StoreOptions::default()).unwrap();
    let branch = disk.branch("dev").unwrap();
    check(&branch.snapshot(), &expected);
    check(
        &branch
            .snapshot_at(&At::Snapshot("seed".into()), &HistoryOptions::default())
            .unwrap()
            .0,
        &original,
    );
    branch.compact().unwrap();
    check(&branch.snapshot(), &expected);
}

#[test]
fn hundred_quad_data_operations_preserve_linked_base_and_net_receipts() {
    use sparkles_core::sparql::{QueryOptions, update};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let disk = Store::open(&root, StoreOptions::default()).unwrap();
    seed(&disk);
    disk.create_branch("data", &BranchOptions::default())
        .unwrap();
    let branch = disk.branch("data").unwrap();
    let before = contents(&branch.snapshot());
    let held = branch.snapshot();
    let data: String = (0..100)
        .map(|n| {
            let q = quad(n);
            let triple = format!("{} {} {} .", q.subject, q.predicate, q.object);
            match q.graph_name {
                GraphName::DefaultGraph => triple,
                graph => format!("GRAPH {graph} {{ {triple} }}"),
            }
        })
        .collect();
    let stats = update::update_as(
        &branch,
        &format!("DELETE DATA {{ {data} }}; DELETE DATA {{ {data} }}; INSERT DATA {{ {data} }}; INSERT DATA {{ {data} }}"),
        &QueryOptions::default(),
        sparkles_core::commit::CommitKind::Update,
    ).unwrap();
    let expected: BTreeSet<_> = before
        .iter()
        .cloned()
        .chain((0..100).map(|n| quad(n).to_string()))
        .collect();
    assert_eq!(stats.inserted, 100);
    assert_eq!(
        stats.deleted,
        before
            .iter()
            .filter(|q| (0..100).any(|n| *q == &quad(n).to_string()))
            .count() as u64
    );
    let receipt = stats.commit.unwrap();
    assert_eq!(
        receipt.commit.inserted,
        expected.difference(&before).count() as u64
    );
    assert_eq!(receipt.commit.deleted, 0);
    check(&held, &before);
    check(&branch.snapshot(), &expected);
    drop(branch);
    drop(disk);
    let disk = Store::open(&root, StoreOptions::default()).unwrap();
    let branch = disk.branch("data").unwrap();
    check(&branch.snapshot(), &expected);
    disk.compact().unwrap();
    disk.relink_branch("data", &CompactOptions::default())
        .unwrap();
    check(&branch.snapshot(), &expected);
}
