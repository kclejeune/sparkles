//! Durable commit identity: ids, counts, receipts, replay, catalog repair, migration.

use oxrdf::{GraphNameRef, NamedNodeRef, QuadRef, TermRef};
use sparkles::Dataset;
use sparkles::commit::{CommitKind, CommitRange};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::update::update;
use sparkles::store::{Store, StoreOptions};
use std::path::Path;
use std::sync::Arc;

fn upd(s: &Store, text: &str) -> sparkles::commit::Receipt {
    update(s, text, &QueryOptions::default())
        .unwrap()
        .commit
        .unwrap()
}

fn seqs(s: &Store) -> Vec<u64> {
    s.commits(CommitRange::Latest, 100)
        .commits
        .iter()
        .map(|c| c.seq)
        .collect()
}

fn ttl(n: usize, prefix: &str) -> Source {
    let mut t = String::new();
    for i in 0..n {
        t.push_str(&format!("<urn:{prefix}{i}> <urn:p> {i} .\n"));
    }
    Source::from_bytes(t.into_bytes(), RdfFormat::Turtle, None)
}

#[test]
fn a_new_database_has_a_root_commit_and_numbers_its_writes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let id;
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        id = s.dataset_id();
        let h = s.head_commit();
        assert_eq!(
            (h.seq, h.kind, h.quads, h.parent()),
            (0, CommitKind::Create, 0, None)
        );
        let r = upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 . <urn:b> <urn:p> 2 }");
        assert!(r.committed);
        assert_eq!(r.dataset_id, id);
        assert_eq!(
            (
                r.commit.seq,
                r.commit.inserted,
                r.commit.deleted,
                r.commit.quads,
                r.commit.kind
            ),
            (1, 2, 0, 2, CommitKind::Update)
        );
        assert_eq!(s.snapshot().commit, 1);
        // no-op writes create no commit
        let r = upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
        assert!(!r.committed);
        assert_eq!(r.commit.seq, 1);
        let r = upd(&s, "DELETE DATA { <urn:zz> <urn:p> 1 }");
        assert!(!r.committed);
        // net counting: an insert undone in the same request is no change
        let r = upd(
            &s,
            "INSERT DATA { <urn:x> <urn:p> 9 } ; DELETE DATA { <urn:x> <urn:p> 9 }",
        );
        assert!(!r.committed);
        let r = upd(
            &s,
            "DELETE DATA { <urn:a> <urn:p> 1 } ; INSERT DATA { <urn:a> <urn:p> 1 }",
        );
        assert!(!r.committed);
        let r = upd(
            &s,
            "DELETE DATA { <urn:a> <urn:p> 1 } ; INSERT DATA { <urn:c> <urn:p> 3 . <urn:d> <urn:p> 4 }",
        );
        assert_eq!(
            (
                r.commit.seq,
                r.commit.inserted,
                r.commit.deleted,
                r.commit.quads
            ),
            (2, 2, 1, 3)
        );
        assert_eq!(seqs(&s), [2, 1, 0]);
    }
    // restart: the same ids, timestamps and counts, and the next commit continues
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.dataset_id(), id);
    assert_eq!(s.head_commit().seq, 2);
    assert_eq!(s.snapshot().commit, 2);
    let c2 = s.commit(2).unwrap();
    assert_eq!((c2.inserted, c2.deleted, c2.quads), (2, 1, 3));
    assert_eq!(upd(&s, "INSERT DATA { <urn:e> <urn:p> 5 }").commit.seq, 3);
    assert_eq!(seqs(&s), [3, 2, 1, 0]);
}

#[test]
fn the_catalog_is_rebuilt_from_the_wal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let before;
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        for i in 0..3 {
            upd(&s, &format!("INSERT DATA {{ <urn:a{i}> <urn:p> {i} }}"));
        }
        before = s.commits(CommitRange::Latest, 10).commits;
    }
    let cat = root.join("commits.bin");
    let len = std::fs::metadata(&cat).unwrap().len();
    let f = std::fs::OpenOptions::new().write(true).open(&cat).unwrap();
    f.set_len(len - 100).unwrap();
    drop(f);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.commits(CommitRange::Latest, 10).commits, before);
    // and it is intact on disk again
    drop(s);
    let (_, recs) = sparkles::commit::read_catalog(&cat).unwrap().unwrap();
    assert_eq!(recs.len(), 4);
    // a deleted catalog is rebuilt too (from the generation's root and the WAL)
    std::fs::remove_file(&cat).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.commits(CommitRange::Latest, 10).commits, before);
}

fn wal(root: &Path) -> std::path::PathBuf {
    let cur = std::fs::read_to_string(root.join("CURRENT")).unwrap();
    root.join(cur.trim()).join("wal.log")
}

#[test]
fn torn_wal_tails_are_dropped_and_damage_before_them_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
        upd(&s, "INSERT DATA { <urn:b> <urn:p> 2 }");
    }
    let path = wal(&root);
    let good = std::fs::read(&path).unwrap();
    // a partial transaction (a data record without its commit record)
    let mut torn = good.clone();
    torn.extend_from_slice(&good[..33]);
    std::fs::write(&path, &torn).unwrap();
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 2);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), good.len() as u64);
        assert_eq!(upd(&s, "INSERT DATA { <urn:c> <urn:p> 3 }").commit.seq, 3);
    }
    // a damaged last transaction is a torn tail too
    let good = std::fs::read(&path).unwrap();
    let mut bad = good.clone();
    let n = bad.len();
    bad[n - 40] ^= 0x55; // inside the last transaction's data record
    std::fs::write(&path, &bad).unwrap();
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 2);
    }
    // damage in an earlier transaction would reuse acknowledged ids: refuse to open
    let good = std::fs::read(&path).unwrap();
    let mut bad = good.clone();
    bad[5] ^= 0x55;
    std::fs::write(&path, &bad).unwrap();
    let e = Store::open(&root, StoreOptions::default()).err().unwrap();
    assert!(e.to_string().contains("wal.log"), "{e}");
}

#[test]
fn an_unknown_wal_record_before_the_last_commit_is_not_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
        upd(&s, "INSERT DATA { <urn:b> <urn:p> 2 }");
    }
    let path = wal(&root);
    let good = std::fs::read(&path).unwrap();
    // a damaged record type in the first transaction: truncating there would silently
    // drop the second, acknowledged, commit
    let mut bad = good.clone();
    bad[0] = 0x7f;
    std::fs::write(&path, &bad).unwrap();
    let e = Store::open(&root, StoreOptions::default()).err().unwrap();
    assert!(e.to_string().contains("unknown record type"), "{e}");
    assert_eq!(std::fs::read(&path).unwrap(), bad, "the WAL was modified");
    // after the last commit record it is a torn tail, truncated as before
    let mut torn = good.clone();
    let mut rec = good[..33].to_vec();
    rec[0] = 0x7f;
    torn.extend_from_slice(&rec);
    std::fs::write(&path, &torn).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, 2);
    drop(s);
    assert_eq!(std::fs::read(&path).unwrap(), good);
}

#[test]
fn compaction_keeps_the_head_and_bulk_writes_are_commits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let opts = || StoreOptions {
        bulk_threshold: 10,
        ..Default::default()
    };
    {
        let s = Store::open(&root, opts()).unwrap();
        // a load into an empty store is a bulk commit
        let r = s.load_as(&[ttl(20, "a")], CommitKind::Load).unwrap();
        assert!(r.committed && r.commit.bulk && r.commit.exact);
        assert_eq!(
            (r.commit.seq, r.commit.inserted, r.commit.quads),
            (1, 20, 20)
        );
        assert_eq!(r.commit.generation_name(), "gen-0002");
        upd(&s, "INSERT DATA { <urn:x> <urn:p> 1 }");
        let head = s.head_commit();
        assert_eq!((head.seq, head.generation_name().as_str()), (2, "gen-0002"));
        s.compact().unwrap();
        assert_eq!(s.head_commit(), head);
        assert_eq!(s.snapshot().generation.name, "gen-0003");
        assert_eq!(
            upd(&s, "INSERT DATA { <urn:y> <urn:p> 1 }")
                .commit
                .generation_name(),
            "gen-0003"
        );
        // a large load into a non-empty store rebuilds as a commit
        let r = s.load_as(&[ttl(100, "b")], CommitKind::Load).unwrap();
        assert_eq!(
            (r.commit.seq, r.commit.inserted, r.commit.bulk),
            (4, 100, true)
        );
        // a bulk transaction that also deleted counts inexactly but keeps the invariant
        let (_, r) = s
            .replace_as(
                sparkles::store::ReplaceTarget::Default,
                &[ttl(30, "c")],
                CommitKind::GspPut,
            )
            .unwrap();
        assert_eq!(r.commit.seq, 5);
        assert!(!r.commit.exact);
        assert_eq!(r.commit.quads, 30);
        let prev = s.commit(4).unwrap();
        assert_eq!(
            prev.quads + r.commit.inserted - r.commit.deleted,
            r.commit.quads
        );
    }
    let s = Store::open(&root, opts()).unwrap();
    assert_eq!(s.head_commit().seq, 5);
    assert_eq!(seqs(&s), [5, 4, 3, 2, 1, 0]);
    assert_eq!(upd(&s, "INSERT DATA { <urn:z> <urn:p> 1 }").commit.seq, 6);
}

#[test]
fn databases_from_older_versions_get_a_baseline_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.load(&[ttl(3, "a")]).unwrap();
        upd(&s, "INSERT DATA { <urn:x> <urn:p> 1 }");
        upd(&s, "DELETE DATA { <urn:a0> <urn:p> 0 }");
    }
    // make it look like an older version's database: legacy WAL commit records, no
    // dataset.json, commit.json or catalog
    let path = wal(&root);
    let mut w = std::fs::read(&path).unwrap();
    for rec in w.as_chunks_mut::<33>().0 {
        if rec[0] == 3 {
            rec[9..].fill(0);
        }
    }
    std::fs::write(&path, &w).unwrap();
    let cur = std::fs::read_to_string(root.join("CURRENT")).unwrap();
    std::fs::remove_file(root.join(cur.trim()).join("commit.json")).unwrap();
    std::fs::remove_file(root.join("dataset.json")).unwrap();
    std::fs::remove_file(root.join("commits.bin")).unwrap();
    let id;
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        let h = s.head_commit();
        assert_eq!(
            (h.seq, h.kind, h.inserted, h.quads),
            (0, CommitKind::Baseline, 3, 3)
        );
        id = s.dataset_id();
        assert_eq!(upd(&s, "INSERT DATA { <urn:y> <urn:p> 1 }").commit.seq, 1);
    }
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.dataset_id(), id);
    assert_eq!(seqs(&s), [1, 0]);
    assert_eq!(s.commit(0).unwrap().kind, CommitKind::Baseline);
}

#[test]
fn timestamps_never_go_backwards() {
    let s = Store::in_memory(StoreOptions::default());
    let root = s.head_commit().timestamp_ms;
    let t = Arc::new(std::sync::atomic::AtomicI64::new(root + 10_000));
    let t2 = t.clone();
    s.set_clock(Arc::new(move || {
        t2.load(std::sync::atomic::Ordering::SeqCst)
    }));
    let a = upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }").commit;
    t.store(root + 5_000, std::sync::atomic::Ordering::SeqCst);
    let b = upd(&s, "INSERT DATA { <urn:b> <urn:p> 1 }").commit;
    assert_eq!(
        (a.timestamp_ms, b.timestamp_ms),
        (root + 10_000, root + 10_000)
    );
    assert_eq!(
        sparkles::commit::rfc3339_ms(1_000_000),
        "1970-01-01T00:16:40.000Z"
    );
}

#[test]
fn in_memory_stores_keep_a_bounded_catalog() {
    let s = Store::in_memory(StoreOptions {
        memory_commit_ring: 3,
        ..Default::default()
    });
    for i in 0..5 {
        upd(&s, &format!("INSERT DATA {{ <urn:a{i}> <urn:p> 1 }}"));
    }
    let page = s.commits(CommitRange::Latest, 10);
    assert_eq!(
        page.commits.iter().map(|c| c.seq).collect::<Vec<_>>(),
        [5, 4, 3]
    );
    assert_eq!(page.first_retained, 3);
    assert!(s.commit(1).is_none());
    let after = s.commits(CommitRange::After(3), 10);
    assert_eq!(
        after.commits.iter().map(|c| c.seq).collect::<Vec<_>>(),
        [4, 5]
    );
    let before = s.commits(CommitRange::Before(5), 1);
    assert_eq!(
        before.commits.iter().map(|c| c.seq).collect::<Vec<_>>(),
        [4]
    );
}

#[test]
fn library_receipts() {
    let ds = Dataset::memory();
    let s = ds
        .update("INSERT DATA { <urn:a> <urn:p> <urn:o> }")
        .unwrap();
    let r = s.commit.unwrap();
    assert!(r.committed && r.commit.seq == 1);
    assert_eq!(ds.head_commit().seq, 1);
    let q = QuadRef::new(
        NamedNodeRef::new("urn:a").unwrap(),
        NamedNodeRef::new("urn:p").unwrap(),
        TermRef::NamedNode(NamedNodeRef::new("urn:o").unwrap()),
        GraphNameRef::DefaultGraph,
    );
    let (_, r) = ds.transaction_receipt(|tx| tx.insert(q)).unwrap();
    assert!(!r.committed && r.commit.seq == 1);
    let (_, r) = ds.transaction_receipt(|tx| tx.remove(q)).unwrap();
    assert!(r.committed);
    assert_eq!(
        (r.commit.seq, r.commit.deleted, r.commit.kind),
        (2, 1, CommitKind::Transaction)
    );
    let json = serde_json::to_value(r).unwrap();
    assert_eq!(json["commit"]["ref"], "commit:2");
    assert_eq!(json["commit"]["parent"], 1);
    assert_eq!(json["commit"]["kind"], "transaction");
    assert_eq!(json["datasetId"], ds.dataset_id().to_string());
}

/// A dataset holds at most `max_prefixes` prefixes: a new one past it is refused, the
/// prefixes of loaded data stop being added, and names and IRIs have a length limit.
#[test]
fn prefixes_are_capped() {
    use sparkles::store::{MAX_PREFIX_IRI_BYTES, MAX_PREFIX_NAME_BYTES};
    assert_eq!(StoreOptions::default().max_prefixes, 1000);
    let s = Store::in_memory(StoreOptions {
        max_prefixes: 3,
        ..Default::default()
    });
    for i in 0..3 {
        s.set_prefix(&format!("p{i}"), &format!("http://p{i}.example/"))
            .unwrap();
    }
    let e = s.set_prefix("p3", "http://p3.example/").unwrap_err();
    assert!(
        matches!(&e, sparkles::Error::Invalid(m) if m.contains("the most allowed")),
        "{e:?}"
    );
    // replacing or removing one still works
    s.set_prefix("p0", "http://other.example/").unwrap();
    assert!(s.remove_prefix("p1").unwrap());
    s.set_prefix("p3", "http://p3.example/").unwrap();
    assert!(s.remove_prefix("p3").unwrap());
    // the prefixes of loaded data fill the room left, and no more
    s.add_prefixes(
        [("a", "http://a.example/"), ("b", "http://b.example/")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into(),
    )
    .unwrap();
    assert_eq!(s.prefixes().keys().collect::<Vec<_>>(), ["a", "p0", "p2"]);
    // lengths
    let s = Store::in_memory(StoreOptions::default());
    let long = "a".repeat(MAX_PREFIX_NAME_BYTES + 1);
    assert!(s.set_prefix(&long, "http://x.example/").is_err());
    let iri = format!("http://x.example/{}", "a".repeat(MAX_PREFIX_IRI_BYTES));
    assert!(s.set_prefix("x", &iri).is_err());
    s.add_prefixes(
        [
            (long, "http://x.example/".to_string()),
            ("x".to_string(), iri),
        ]
        .into(),
    )
    .unwrap();
    assert!(s.prefixes().is_empty());
    // 0: unlimited
    let s = Store::in_memory(StoreOptions {
        max_prefixes: 0,
        ..Default::default()
    });
    for i in 0..1500 {
        s.set_prefix(&format!("p{i}"), &format!("http://p{i}.example/"))
            .unwrap();
    }
    assert_eq!(s.prefixes().len(), 1500);
}

#[test]
fn prefix_changes_persist_without_commits() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.add_prefixes([("ex".to_string(), "http://ex.org/".to_string())].into())
            .unwrap();
        // compaction writes the prefixes into the new generation too
        s.compact().unwrap();
        s.set_prefix("zz", "http://zz.example/").unwrap();
        let head = s.head_commit().seq;
        assert!(s.remove_prefix("ex").unwrap());
        assert!(!s.remove_prefix("ex").unwrap());
        // metadata only
        assert_eq!(s.head_commit().seq, head);
    }
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let p = s.prefixes();
    assert_eq!(p.get("zz").map(String::as_str), Some("http://zz.example/"));
    assert!(
        !p.contains_key("ex"),
        "a removed prefix stays removed: {p:?}"
    );
}
