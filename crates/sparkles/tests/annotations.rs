//! Commit annotations (messages and change digests) and write preconditions.

use sparkles::Error;
use sparkles::annotations::{self, Annotation};
use sparkles::commit::{CommitKind, Receipt};
use sparkles::guard::{Precondition, WriteOptions};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::update::update_as;
use sparkles::store::{ReplaceTarget, Store, StoreOptions, named};
use std::sync::Arc;

fn with_message(m: &str) -> WriteOptions {
    WriteOptions {
        message: Some(Arc::from(m)),
        ..Default::default()
    }
}

fn upd_with(s: &Store, text: &str, w: WriteOptions) -> sparkles::Result<Receipt> {
    let opts = QueryOptions {
        write: w,
        ..Default::default()
    };
    Ok(update_as(s, text, &opts, CommitKind::Update)?
        .commit
        .unwrap())
}

fn nt(text: &str) -> Source {
    Source::from_bytes(text.as_bytes().to_vec(), RdfFormat::NTriples, None)
}

fn message(s: &Store, seq: u64) -> Option<String> {
    s.annotation(seq)
        .and_then(|a| a.message)
        .map(|m| m.to_string())
}

#[test]
fn messages_are_recorded_with_commits_and_survive_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let opts = StoreOptions {
        bulk_threshold: 10,
        ..Default::default()
    };
    {
        let s = Store::open(&root, opts.clone()).unwrap();
        let r = upd_with(
            &s,
            "INSERT DATA { <urn:a> <urn:p> 1 }",
            with_message("  first write "),
        )
        .unwrap();
        assert_eq!(r.commit.seq, 1);
        assert_eq!(r.annotation.message.as_deref(), Some("first write"));
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["commit"]["message"], "first write");
        assert!(j["commit"].get("digest").is_none());
        // a commit without a message has none
        let r = upd_with(&s, "INSERT DATA { <urn:b> <urn:p> 2 }", Default::default()).unwrap();
        assert!(r.annotation.is_empty());
        // a write without net effect makes no commit: its receipt describes the head
        let r = upd_with(
            &s,
            "INSERT DATA { <urn:a> <urn:p> 1 }",
            with_message("ignored"),
        )
        .unwrap();
        assert!(!r.committed);
        assert_eq!((r.commit.seq, r.annotation.message), (2, None));
        // a Graph Store replace (transactional) and a bulk load
        let (_, r) = s
            .replace_with(
                ReplaceTarget::Named(named("urn:g")),
                &[nt("<urn:x> <urn:p> <urn:y> .")],
                CommitKind::GspPut,
                &with_message("replace g"),
            )
            .unwrap();
        assert_eq!((r.commit.seq, r.commit.bulk), (3, false));
        let mut big = String::new();
        for i in 0..50 {
            big.push_str(&format!("<urn:s{i}> <urn:p> <urn:o> .\n"));
        }
        let r = s
            .load_with(&[nt(&big)], CommitKind::Load, &with_message("bulk"))
            .unwrap();
        assert_eq!((r.commit.seq, r.commit.bulk), (4, true));
        assert_eq!(r.annotation.message.as_deref(), Some("bulk"));
    }
    let s = Store::open(&root, opts).unwrap();
    assert_eq!(message(&s, 1).as_deref(), Some("first write"));
    assert_eq!(message(&s, 2), None);
    assert_eq!(message(&s, 3).as_deref(), Some("replace g"));
    assert_eq!(message(&s, 4).as_deref(), Some("bulk"));
    // the lock-free reader sees the same
    let all = annotations::read(&root).unwrap();
    assert_eq!(all.keys().copied().collect::<Vec<_>>(), [1, 3, 4]);
    let r = upd_with(
        &s,
        "INSERT DATA { <urn:c> <urn:p> 3 }",
        with_message("after"),
    )
    .unwrap();
    assert_eq!(r.commit.seq, 5);
    assert_eq!(message(&s, 5).as_deref(), Some("after"));
}

#[test]
fn invalid_messages_are_refused_before_anything_is_written() {
    let s = Store::in_memory(StoreOptions::default());
    for bad in [
        "two\nlines",
        &"x".repeat(annotations::MAX_MESSAGE_BYTES + 1),
    ] {
        let e = upd_with(&s, "INSERT DATA { <urn:a> <urn:p> 1 }", with_message(bad));
        assert!(matches!(e, Err(Error::Invalid(_))), "{e:?}");
    }
    assert_eq!(s.head_commit().seq, 0);
    // a message of whitespace is no message
    let r = upd_with(&s, "INSERT DATA { <urn:a> <urn:p> 1 }", with_message("   ")).unwrap();
    assert!(r.committed && r.annotation.is_empty());
}

#[test]
fn annotations_of_commits_that_did_not_survive_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        upd_with(&s, "INSERT DATA { <urn:a> <urn:p> 1 }", with_message("one")).unwrap();
        upd_with(&s, "INSERT DATA { <urn:b> <urn:p> 2 }", with_message("two")).unwrap();
    }
    // lose commit 2 from the WAL (one data record and the commit record)
    let wal = root.join("gen-0001/wal.log");
    let len = std::fs::metadata(&wal).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&wal)
        .unwrap()
        .set_len(len - 66)
        .unwrap();
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 1);
        assert_eq!(message(&s, 1).as_deref(), Some("one"));
        assert_eq!(message(&s, 2), None);
        // the next commit reuses seq 2 and does not inherit the lost message
        let r = upd_with(&s, "INSERT DATA { <urn:c> <urn:p> 3 }", Default::default()).unwrap();
        assert_eq!((r.commit.seq, r.annotation.message.clone()), (2, None));
    }
    // a torn tail of the side-file is cut off at the next open
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    upd_with(
        &s,
        "INSERT DATA { <urn:d> <urn:p> 4 }",
        with_message("three"),
    )
    .unwrap();
    drop(s);
    let side = root.join(annotations::FILE);
    let len = std::fs::metadata(&side).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&side)
        .unwrap()
        .set_len(len - 1)
        .unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(message(&s, 1).as_deref(), Some("one"));
    assert_eq!(message(&s, 3), None);
    upd_with(
        &s,
        "INSERT DATA { <urn:e> <urn:p> 5 }",
        with_message("four"),
    )
    .unwrap();
    drop(s);
    let all = annotations::read(&root).unwrap();
    assert_eq!(all.keys().copied().collect::<Vec<_>>(), [1, 4]);
}

#[test]
fn change_digests_chain_the_net_changes_of_each_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let on = StoreOptions {
        commit_digests: true,
        bulk_threshold: 10,
        ..Default::default()
    };
    let d0;
    let d1;
    {
        let s = Store::open(&root, on.clone()).unwrap();
        assert!(s.commit_digests());
        let id = s.dataset_id();
        // the empty root commit has a digest
        let root_commit = s.head_commit();
        d0 = s.annotation(0).and_then(|a| a.digest).unwrap();
        assert_eq!(
            d0,
            annotations::change_digest(id, &root_commit, None, vec![], vec![])
        );
        // an insert and a delete of the same quad cancel out; only net changes count
        let r = upd_with(
            &s,
            "INSERT DATA { <urn:a> <urn:p> \"x\\ny\" . <urn:t> <urn:p> 0 } ; DELETE DATA { <urn:t> <urn:p> 0 } ; INSERT DATA { GRAPH <urn:g> { _:b <urn:p> <urn:o> } }",
            Default::default(),
        )
        .unwrap();
        d1 = r.annotation.digest.unwrap();
        let bnode = {
            let snap = s.snapshot();
            let q = snap.graph_ids().unwrap();
            assert_eq!(q.len(), 1);
            let mut label = String::new();
            snap.for_each_quad(|k| {
                if let Some(t) = snap.quad_to_terms(k)
                    && let oxrdf::NamedOrBlankNode::BlankNode(b) = &t.subject
                {
                    label = b.as_str().to_string();
                }
                Ok(())
            })
            .unwrap();
            label
        };
        let want = annotations::change_digest(
            id,
            &r.commit,
            Some(&d0),
            vec![],
            vec![
                "<urn:a> <urn:p> \"x\\ny\" .".to_string(),
                format!("_:{bnode} <urn:p> <urn:o> <urn:g> ."),
            ],
        );
        assert_eq!(d1, want);
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["commit"]["digest"].as_str().unwrap().len(), 64);
        // a delete chains from the previous digest
        let r = upd_with(
            &s,
            "DELETE DATA { <urn:a> <urn:p> \"x\\ny\" }",
            Default::default(),
        )
        .unwrap();
        let want = annotations::change_digest(
            id,
            &r.commit,
            Some(&d1),
            vec!["<urn:a> <urn:p> \"x\\ny\" .".to_string()],
            vec![],
        );
        assert_eq!(r.annotation.digest, Some(want));
        // bulk commits have no digest
        let mut big = String::new();
        for i in 0..50 {
            big.push_str(&format!("<urn:s{i}> <urn:p> <urn:o> .\n"));
        }
        let r = s
            .load_with(&[nt(&big)], CommitKind::Load, &Default::default())
            .unwrap();
        assert!(r.commit.bulk && r.annotation.digest.is_none());
    }
    // the setting sticks to the database
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(s.commit_digests());
    assert_eq!(s.annotation(1).and_then(|a| a.digest), Some(d1));
    let r = upd_with(&s, "INSERT DATA { <urn:z> <urn:p> 1 }", Default::default()).unwrap();
    // after a commit without a digest, the chain starts again from zeros
    let want = annotations::change_digest(
        s.dataset_id(),
        &r.commit,
        None,
        vec![],
        vec!["<urn:z> <urn:p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .".to_string()],
    );
    assert_eq!(r.annotation.digest, Some(want));
}

#[test]
fn digests_are_off_by_default() {
    let s = Store::in_memory(StoreOptions::default());
    assert!(!s.commit_digests());
    let r = upd_with(&s, "INSERT DATA { <urn:a> <urn:p> 1 }", Default::default()).unwrap();
    assert_eq!(r.annotation, Annotation::default());
    let m = Store::in_memory(StoreOptions {
        commit_digests: true,
        ..Default::default()
    });
    let r = upd_with(&m, "INSERT DATA { <urn:a> <urn:p> 1 }", Default::default()).unwrap();
    assert!(r.annotation.digest.is_some());
}

/// Only while the head is `seq`.
fn at_head(seq: u64) -> Precondition {
    Precondition::new(move |snap| {
        if snap.commit == seq {
            Ok(())
        } else {
            Err(Error::PreconditionFailed(format!(
                "head is {}, not {seq}",
                snap.commit
            )))
        }
    })
}

#[test]
fn a_failed_precondition_writes_nothing() {
    let s = Store::in_memory(StoreOptions::default());
    let w = |seq| WriteOptions {
        precondition: Some(at_head(seq)),
        ..Default::default()
    };
    let e = upd_with(&s, "INSERT DATA { <urn:a> <urn:p> 1 }", w(7));
    assert!(matches!(e, Err(Error::PreconditionFailed(_))), "{e:?}");
    assert_eq!(s.head_commit().seq, 0);
    assert_eq!(
        upd_with(&s, "INSERT DATA { <urn:a> <urn:p> 1 }", w(0))
            .unwrap()
            .commit
            .seq,
        1
    );
    // loads and replaces check it too
    assert!(matches!(
        s.load_with(
            &[nt("<urn:b> <urn:p> <urn:o> .")],
            CommitKind::GspPost,
            &w(0)
        ),
        Err(Error::PreconditionFailed(_))
    ));
    assert!(matches!(
        s.replace_with(
            ReplaceTarget::Default,
            &[nt("<urn:b> <urn:p> <urn:o> .")],
            CommitKind::GspPut,
            &w(0)
        ),
        Err(Error::PreconditionFailed(_))
    ));
    assert_eq!(s.head_commit().seq, 1);
}

#[test]
fn concurrent_writers_with_the_same_precondition_commit_once() {
    let s = Arc::new(Store::in_memory(StoreOptions::default()));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let s = s.clone();
            std::thread::spawn(move || {
                let w = WriteOptions {
                    precondition: Some(at_head(0)),
                    ..Default::default()
                };
                upd_with(&s, &format!("INSERT DATA {{ <urn:w{i}> <urn:p> {i} }}"), w)
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let failed = results
        .iter()
        .filter(|r| matches!(r, Err(Error::PreconditionFailed(_))))
        .count();
    assert_eq!((ok, failed), (1, 7));
    assert_eq!(s.head_commit().seq, 1);
}
