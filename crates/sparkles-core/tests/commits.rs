//! Durable commit identity: ids, counts, receipts, replay, catalog repair, migration.

use sparkles_core::commit::{CommitKind, CommitRange};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{Store, StoreOptions};
use std::path::Path;
use std::sync::Arc;

fn upd(s: &Store, text: &str) -> sparkles_core::commit::Receipt {
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
    let (_, recs) = sparkles_core::commit::read_catalog(&cat).unwrap().unwrap();
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

/// A store preallocates its WAL (`StoreOptions::wal_prealloc_bytes`): the file grows
/// ahead of the commits by zero bytes, and its logical end is the last commit's.
#[test]
fn a_preallocated_wal_ends_at_its_last_commit() {
    use sparkles_core::history::{At, HistoryOptions};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    for i in 0..50 {
        upd(&s, &format!("INSERT DATA {{ <urn:s{i}> <urn:p> {i} }}"));
    }
    let path = wal(&root);
    // two records per commit: a data record and the commit record
    assert_eq!(s.wal_bytes(), 50 * 2 * 33);
    let on_disk = std::fs::metadata(&path).unwrap().len();
    assert!(on_disk >= 64 << 10, "{on_disk} bytes");
    // readers of the live log stop where it ends: past states and diffs
    let (past, _) = s
        .snapshot_at(&At::Commit(20), &HistoryOptions::default())
        .unwrap();
    assert_eq!(past.len(), 20);
    let d = s
        .diff(&At::Commit(10), &At::Head, &Default::default())
        .unwrap();
    assert_eq!((d.added, d.removed), (40, 0));
    // the check of a live store reports the zeros as preallocated space
    let r =
        sparkles_core::check::check(&root, &sparkles_core::check::CheckOptions { quick: false });
    let r = r.unwrap();
    let w = r.get("wal").unwrap();
    assert_eq!(
        w.status,
        sparkles_core::check::Status::Ok,
        "{}",
        r.to_text()
    );
    assert!(w.summary.contains("preallocated"), "{}", w.summary);
    // a crash leaves the zeros behind; a clean close does not
    let crashed = std::fs::read(&path).unwrap();
    drop(s);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 50 * 2 * 33);
    std::fs::write(&path, &crashed).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, 50);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 50 * 2 * 33);
    assert_eq!(
        upd(&s, "INSERT DATA { <urn:s50> <urn:p> 50 }").commit.seq,
        51
    );
    assert_eq!(s.snapshot().len(), 51);
    drop(s);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, 51);
    assert_eq!(s.snapshot().len(), 51);
}

/// A commit that overwrites preallocated bytes can reach the disk in any order of its
/// pages, so a crash can leave zeros in the middle of the last transaction, before its
/// commit record. It was never acknowledged: open drops it like a torn tail. Zeros in
/// an earlier transaction are damage, and open refuses the database.
#[test]
fn zeros_inside_the_last_transaction_are_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        for i in 0..3 {
            upd(
                &s,
                &format!(
                    "INSERT DATA {{ <urn:a{i}> <urn:p> 1 . <urn:b{i}> <urn:p> 2 . <urn:c{i}> <urn:p> 3 }}"
                ),
            );
        }
    }
    let path = wal(&root);
    let full = std::fs::read(&path).unwrap();
    // three transactions of three data records and a commit record
    assert_eq!(full.len(), 3 * 4 * 33);
    for zeroed in [8..9, 8..10, 9..11] {
        let mut v = full.clone();
        v[zeroed.start * 33..zeroed.end * 33].fill(0);
        std::fs::write(&path, &v).unwrap();
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 2, "records {zeroed:?} zeroed");
        assert_eq!(s.snapshot().len(), 6);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 2 * 4 * 33);
        let r = upd(
            &s,
            "INSERT DATA { <urn:a2> <urn:p> 1 . <urn:b2> <urn:p> 2 . <urn:c2> <urn:p> 3 }",
        );
        assert_eq!(r.commit.seq, 3);
    }
    // the same records again (with a later timestamp)
    let full = std::fs::read(&path).unwrap();
    assert_eq!(full.len(), 3 * 4 * 33);
    let mut v = full.clone();
    v[4 * 33..5 * 33].fill(0);
    std::fs::write(&path, &v).unwrap();
    let e = Store::open(&root, StoreOptions::default()).err().unwrap();
    assert!(e.to_string().contains("wal.log"), "{e}");
    assert_eq!(std::fs::read(&path).unwrap(), v, "the WAL was modified");
}

#[test]
fn without_preallocation_each_commit_appends() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let opts = || StoreOptions {
        wal_prealloc_bytes: 0,
        ..Default::default()
    };
    let s = Store::open(&root, opts()).unwrap();
    for i in 0..5 {
        upd(&s, &format!("INSERT DATA {{ <urn:s{i}> <urn:p> {i} }}"));
        assert_eq!(std::fs::metadata(wal(&root)).unwrap().len(), s.wal_bytes());
    }
    drop(s);
    // a log preallocated by an earlier run is truncated at open and appended to
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    upd(&s, "INSERT DATA { <urn:s5> <urn:p> 5 }");
    assert!(std::fs::metadata(wal(&root)).unwrap().len() > s.wal_bytes());
    drop(s);
    let s = Store::open(&root, opts()).unwrap();
    upd(&s, "INSERT DATA { <urn:s6> <urn:p> 6 }");
    assert_eq!(std::fs::metadata(wal(&root)).unwrap().len(), 7 * 2 * 33);
    assert_eq!(s.snapshot().len(), 7);
}

/// Whether the file system under `dir` accepts `O_DIRECT`, which the write-through
/// WAL path needs (`StoreOptions::wal_direct_writes`).
fn direct_io(dir: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .custom_flags(libc::O_DIRECT)
            .open(dir.join("direct-probe"))
            .is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        false
    }
}

/// A store whose small commits go through the write-through path (direct writes with
/// `O_DSYNC` into the preallocated log) commits what the buffered path does. Commits
/// that cross 4 KiB blocks, add terms, delete, and a commit too long for a direct write
/// in between all survive a crash image of the log, and later commits continue it.
#[test]
fn direct_wal_writes_commit_what_buffered_writes_do() {
    let dir = tempfile::tempdir().unwrap();
    let supported = direct_io(dir.path());
    for direct in [true, false] {
        let root = dir.path().join(format!("db-{direct}"));
        let opts = || StoreOptions {
            wal_direct_writes: direct,
            ..Default::default()
        };
        let s = Store::open(&root, opts()).unwrap();
        // 200 one-quad commits of 66 bytes span four blocks, each adding a literal
        for i in 0..200 {
            upd(
                &s,
                &format!("INSERT DATA {{ <urn:s{i}> <urn:p> \"v{i}\" }}"),
            );
        }
        assert_eq!(s.wal_direct_active(), direct && supported);
        // 2,500 quads are about 80 KiB of records, more than one direct write takes
        let big: String = (0..2500)
            .map(|i| format!("<urn:b{i}> <urn:p> {i} . "))
            .collect();
        upd(&s, &format!("INSERT DATA {{ {big} }}"));
        for i in 0..100 {
            upd(
                &s,
                &format!("DELETE DATA {{ <urn:s{i}> <urn:p> \"v{i}\" }}"),
            );
        }
        for i in 0..50 {
            upd(
                &s,
                &format!("INSERT DATA {{ <urn:s{i}> <urn:p> \"v{i}\" }}"),
            );
        }
        assert_eq!(s.wal_direct_active(), direct && supported);
        let head = s.head_commit().seq;
        assert_eq!(head, 351);
        let len = s.snapshot().len();
        assert_eq!(len, 200 + 2500 - 100 + 50);
        let logical = s.wal_bytes();
        // the log as a crash would leave it, with its preallocated zeros
        let crashed = std::fs::read(wal(&root)).unwrap();
        assert!(crashed.len() as u64 > logical);
        drop(s);
        std::fs::write(wal(&root), &crashed).unwrap();
        let s = Store::open(&root, opts()).unwrap();
        assert_eq!(s.head_commit().seq, head);
        assert_eq!(s.snapshot().len(), len);
        assert_eq!(s.wal_bytes(), logical);
        assert!(ask(&s, "ASK { <urn:s0> <urn:p> \"v0\" }"));
        assert!(!ask(&s, "ASK { <urn:s60> <urn:p> \"v60\" }"));
        assert!(ask(&s, "ASK { <urn:s199> <urn:p> \"v199\" }"));
        assert!(ask(&s, "ASK { <urn:b2499> <urn:p> 2499 }"));
        // the reopened log is cut at its end, grows again, and takes direct writes
        for i in 0..20 {
            upd(&s, &format!("INSERT DATA {{ <urn:t{i}> <urn:p> {i} }}"));
        }
        assert_eq!(s.wal_direct_active(), direct && supported);
        drop(s);
        let s = Store::open(&root, opts()).unwrap();
        assert_eq!(s.head_commit().seq, head + 20);
        assert_eq!(s.snapshot().len(), len + 20);
    }
}

/// A compaction switches the store to a new generation's log. The write-through
/// handle of the old log is dropped, and the commits after the switch reach the new log.
#[test]
fn direct_wal_writes_follow_a_compaction_to_the_new_log() {
    let dir = tempfile::tempdir().unwrap();
    let supported = direct_io(dir.path());
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    for i in 0..30 {
        upd(&s, &format!("INSERT DATA {{ <urn:s{i}> <urn:p> {i} }}"));
    }
    assert_eq!(s.wal_direct_active(), supported);
    let before = wal(&root);
    s.compact().unwrap();
    assert_ne!(wal(&root), before);
    assert!(!s.wal_direct_active());
    for i in 30..60 {
        upd(&s, &format!("INSERT DATA {{ <urn:s{i}> <urn:p> {i} }}"));
    }
    assert_eq!(s.wal_direct_active(), supported);
    let crashed = std::fs::read(wal(&root)).unwrap();
    let head = s.head_commit().seq;
    drop(s);
    std::fs::write(wal(&root), &crashed).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, head);
    assert_eq!(s.snapshot().len(), 60);
}

fn ask(s: &Store, q: &str) -> bool {
    sparkles_core::sparql::query(s.snapshot(), q, &QueryOptions::default())
        .unwrap()
        .boolean
}

/// Where each chunk of a framed delta vocabulary file ends, in order.
fn chunk_ends(path: &Path) -> Vec<u64> {
    sparkles_core::vocab::delta::chunk_ends(&std::fs::read(path).unwrap())
        .unwrap()
        .into_iter()
        .map(|e| e as u64)
        .collect()
}

fn vocab_of(root: &Path) -> std::path::PathBuf {
    wal(root).with_file_name("delta.vocab")
}

fn truncate(path: &Path, len: u64) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(len)
        .unwrap();
}

/// A commit's new terms and its WAL records are written at the same time, so a crash
/// can leave the commit record on disk without the chunk of `delta.vocab` that holds
/// the terms it names, or with that chunk torn. That commit was never acknowledged:
/// open drops it like any torn tail.
#[test]
fn a_last_commit_whose_new_terms_were_lost_is_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        upd(&s, "INSERT DATA { <urn:a> <urn:p> \"one\" }");
        upd(&s, "INSERT DATA { <urn:b> <urn:p> \"two\" }");
    }
    let wal_path = wal(&root);
    let vocab = vocab_of(&root);
    let wal_full = std::fs::read(&wal_path).unwrap();
    let vocab_full = std::fs::read(&vocab).unwrap();
    // urn:a, urn:p and "one" in commit 1's chunk, urn:b and "two" in commit 2's
    let ends = chunk_ends(&vocab);
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[1], vocab_full.len() as u64);
    let wal_one = {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 2);
        drop(s);
        // the WAL of commit 1 alone: up to its commit record
        let recs = wal_full.len() / 33;
        let first = (0..recs).find(|&i| wal_full[i * 33] == 3).unwrap();
        ((first + 1) * 33) as u64
    };
    let c2 = ends[0] as usize;
    let damaged: Vec<(&str, Vec<u8>)> = vec![
        // commit 2's chunk lost entirely
        ("lost", vocab_full[..c2].to_vec()),
        // cut short half-way, or by its last byte
        ("cut", vocab_full[..c2 + 30].to_vec()),
        ("cut by one", vocab_full[..vocab_full.len() - 1].to_vec()),
        // the file's length reached the disk, its data did not (preallocated zeros)
        ("zeros", {
            let mut v = vocab_full.clone();
            v[c2..].fill(0);
            v.resize(64 << 10, 0);
            v
        }),
        // a sector of the chunk did not reach the disk: its checksum fails
        ("torn", {
            let mut v = vocab_full.clone();
            v[vocab_full.len() - 3..].fill(0);
            v.resize(8192, 0);
            v
        }),
        // the chunk's header missing, its entries there
        ("no header", {
            let mut v = vocab_full.clone();
            v[c2..c2 + 24].fill(0);
            v
        }),
    ];
    for (what, v) in &damaged {
        std::fs::write(&wal_path, &wal_full).unwrap();
        std::fs::write(&vocab, v).unwrap();
        {
            let s = Store::open(&root, StoreOptions::default()).unwrap();
            assert_eq!(s.head_commit().seq, 1, "{what}");
            assert!(ask(&s, "ASK { <urn:a> <urn:p> \"one\" }"));
            assert!(!ask(&s, "ASK { <urn:b> ?p ?o }"));
            assert_eq!(std::fs::metadata(&wal_path).unwrap().len(), wal_one);
            // open cut the vocabulary back to commit 1's chunk
            assert_eq!(std::fs::metadata(&vocab).unwrap().len(), ends[0], "{what}");
            // the next commit takes the number the lost one had
            let r = upd(&s, "INSERT DATA { <urn:b> <urn:p> \"two\" }");
            assert_eq!(r.commit.seq, 2);
        }
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 2, "{what}");
        assert!(ask(&s, "ASK { <urn:b> <urn:p> \"two\" }"));
        drop(s);
        assert_eq!(std::fs::read(&vocab).unwrap(), vocab_full, "{what}");
    }
    // the terms on disk without the WAL records: the commit is simply not there, and
    // the terms are used again when the triple is inserted again
    std::fs::write(&wal_path, &wal_full).unwrap();
    std::fs::write(&vocab, &vocab_full).unwrap();
    truncate(&wal_path, wal_one);
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 1);
        assert!(!ask(&s, "ASK { <urn:b> ?p ?o }"));
        assert_eq!(
            upd(&s, "INSERT DATA { <urn:b> <urn:p> \"two\" }")
                .commit
                .seq,
            2
        );
    }
    assert_eq!(std::fs::read(&vocab).unwrap(), vocab_full);
    // terms missing for a commit before the last one cannot come from a crash: open
    // refuses the database and leaves its files alone
    std::fs::write(&wal_path, &wal_full).unwrap();
    let mut v = vocab_full.clone();
    v[ends[0] as usize - 2] ^= 0xff;
    std::fs::write(&vocab, &v).unwrap();
    let e = Store::open(&root, StoreOptions::default()).err().unwrap();
    assert!(e.to_string().contains("delta.vocab"), "{e}");
    assert_eq!(std::fs::read(&wal_path).unwrap(), wal_full);
    assert_eq!(std::fs::read(&vocab).unwrap(), v);
}

/// Terms go to `delta.vocab` in one checksummed chunk per commit, written with
/// `O_DIRECT | O_DSYNC` into zeros the file was grown by, at the same time as the
/// commit's WAL records. A crash image of both files, preallocated zeros included,
/// opens with every commit, and later commits continue both files.
#[test]
fn new_terms_take_direct_writes_and_survive_a_crash_image() {
    let dir = tempfile::tempdir().unwrap();
    let supported = direct_io(dir.path());
    for direct in [true, false] {
        let root = dir.path().join(format!("db-{direct}"));
        let opts = || StoreOptions {
            wal_direct_writes: direct,
            ..Default::default()
        };
        let s = Store::open(&root, opts()).unwrap();
        for i in 0..300 {
            upd(
                &s,
                &format!("INSERT DATA {{ <urn:s> <urn:p> \"literal {i}\" }}"),
            );
        }
        // a commit with more terms than one direct write takes
        let big: String = (0..3000)
            .map(|i| format!("<urn:s> <urn:q> \"a longer literal number {i}\" . "))
            .collect();
        upd(&s, &format!("INSERT DATA {{ {big} }}"));
        for i in 300..400 {
            upd(
                &s,
                &format!("INSERT DATA {{ <urn:s> <urn:p> \"literal {i}\" }}"),
            );
        }
        assert_eq!(s.wal_direct_active(), direct && supported);
        assert_eq!(s.vocab_direct_active(), direct && supported);
        let head = s.head_commit().seq;
        let len = s.snapshot().len();
        let vocab = vocab_of(&root);
        let (wal_crash, vocab_crash) = (
            std::fs::read(wal(&root)).unwrap(),
            std::fs::read(&vocab).unwrap(),
        );
        // one chunk for each commit, then the space preallocated for later ones
        let ends = chunk_ends(&vocab);
        assert_eq!(ends.len(), 401);
        let last = *ends.last().unwrap();
        assert!(vocab_crash.len() as u64 > last);
        assert!(vocab_crash[last as usize..].iter().all(|&b| b == 0));
        drop(s);
        // a clean close cuts the zeros
        assert_eq!(std::fs::metadata(&vocab).unwrap().len(), last);
        std::fs::write(wal(&root), &wal_crash).unwrap();
        std::fs::write(&vocab, &vocab_crash).unwrap();
        let s = Store::open(&root, opts()).unwrap();
        assert_eq!(s.head_commit().seq, head);
        assert_eq!(s.snapshot().len(), len);
        assert!(ask(&s, "ASK { <urn:s> <urn:p> \"literal 0\" }"));
        assert!(ask(&s, "ASK { <urn:s> <urn:p> \"literal 399\" }"));
        assert!(ask(
            &s,
            "ASK { <urn:s> <urn:q> \"a longer literal number 2999\" }"
        ));
        for i in 400..420 {
            upd(
                &s,
                &format!("INSERT DATA {{ <urn:s> <urn:p> \"literal {i}\" }}"),
            );
        }
        assert_eq!(s.wal_direct_active(), direct && supported);
        assert_eq!(s.vocab_direct_active(), direct && supported);
        drop(s);
        let s = Store::open(&root, opts()).unwrap();
        assert_eq!(s.head_commit().seq, head + 20);
        assert!(ask(&s, "ASK { <urn:s> <urn:p> \"literal 419\" }"));
        assert_eq!(chunk_ends(&vocab).len(), 421);
    }
}

/// The `minimumReader` of a dataset's `dataset.json`.
fn minimum_reader(root: &Path) -> u64 {
    let ds: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("dataset.json")).unwrap()).unwrap();
    ds["minimumReader"].as_u64().unwrap()
}

/// A dataset from an earlier release has a legacy `delta.vocab`, the entries alone. It
/// opens and reads as before, and nothing in the file changes until a commit adds a
/// term. That commit first requires reader 4 in `dataset.json`, which earlier releases
/// refuse, and then rewrites the file framed. Commits that add no terms keep the
/// dataset's reader.
#[test]
fn a_legacy_delta_vocabulary_is_rewritten_framed_by_the_first_new_term() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        upd(&s, "INSERT DATA { <urn:a> <urn:p> \"one\" }");
        upd(&s, "INSERT DATA { <urn:b> <urn:p> \"two\" }");
    }
    // the dataset as an earlier release left it, with a torn entry at the end
    let vocab = vocab_of(&root);
    let parsed = std::fs::read(&vocab).unwrap();
    let keys = sparkles_core::vocab::parse_delta(&parsed).unwrap().keys;
    assert_eq!(keys.len(), 5);
    let mut legacy: Vec<u8> = keys
        .iter()
        .flat_map(|k| [&(k.len() as u32).to_le_bytes()[..], k].concat())
        .collect();
    let legacy_len = legacy.len();
    legacy.extend_from_slice(&[9, 0, 0, 0, b'<']);
    std::fs::write(&vocab, &legacy).unwrap();
    let mut ds: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("dataset.json")).unwrap()).unwrap();
    ds["minimumReader"] = serde_json::json!(1);
    std::fs::write(root.join("dataset.json"), ds.to_string()).unwrap();
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 2);
        assert!(ask(&s, "ASK { <urn:b> <urn:p> \"two\" }"));
        // a commit without new terms leaves the file as it is
        upd(&s, "INSERT DATA { <urn:b> <urn:p> \"one\" }");
        assert_eq!(minimum_reader(&root), 1);
    }
    // the open dropped the torn entry, and the legacy entries are as they were
    assert_eq!(std::fs::read(&vocab).unwrap(), &legacy[..legacy_len]);
    let check = |root: &Path| {
        let r = sparkles_core::check::check(root, &Default::default()).unwrap();
        r.get("delta-vocabulary").unwrap().summary.clone()
    };
    assert!(check(&root).contains("legacy format"), "{}", check(&root));
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        upd(&s, "INSERT DATA { <urn:c> <urn:p> \"three\" }");
        assert_eq!(minimum_reader(&root), 4);
        assert_eq!(s.head_commit().seq, 4);
    }
    let framed = std::fs::read(&vocab).unwrap();
    assert_eq!(&framed[..8], &sparkles_core::vocab::delta::MAGIC);
    // the rewritten entries in one chunk, the new terms in the next
    assert_eq!(chunk_ends(&vocab).len(), 2);
    assert!(check(&root).contains("framed format"), "{}", check(&root));
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, 4);
    for q in [
        "ASK { <urn:a> <urn:p> \"one\" }",
        "ASK { <urn:b> <urn:p> \"two\" }",
        "ASK { <urn:b> <urn:p> \"one\" }",
        "ASK { <urn:c> <urn:p> \"three\" }",
    ] {
        assert!(ask(&s, q), "{q}");
    }
    drop(s);
    // a release that reads up to reader 3 refuses the dataset now, as this one
    // refuses a dataset that requires a later reader
    assert_eq!(sparkles_core::commit::DATASET_READER, 4);
    let mut ds: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("dataset.json")).unwrap()).unwrap();
    ds["minimumReader"] = serde_json::json!(sparkles_core::commit::DATASET_READER + 1);
    std::fs::write(root.join("dataset.json"), ds.to_string()).unwrap();
    let e = Store::open(&root, StoreOptions::default()).err().unwrap();
    assert!(e.to_string().contains("requires reader 5"), "{e}");
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
                sparkles_core::store::ReplaceTarget::Default,
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
        sparkles_core::commit::rfc3339_ms(1_000_000),
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

/// A dataset holds at most `max_prefixes` prefixes: a new one past it is refused, the
/// prefixes of loaded data stop being added, and names and IRIs have a length limit.
#[test]
fn prefixes_are_capped() {
    use sparkles_core::store::{MAX_PREFIX_IRI_BYTES, MAX_PREFIX_NAME_BYTES};
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
        matches!(&e, sparkles_core::Error::Invalid(m) if m.contains("the most allowed")),
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

#[test]
fn commits_record_whether_they_changed_the_default_graph() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let opts = || StoreOptions {
        bulk_threshold: 10,
        ..Default::default()
    };
    let named = |n: usize| {
        let mut s = ttl(n, "n");
        s.graph = Some(oxrdf::NamedNode::new_unchecked("urn:g"));
        s
    };
    let flags = |s: &Store| -> Vec<bool> {
        let mut v: Vec<_> = s
            .commits(CommitRange::Latest, 100)
            .commits
            .iter()
            .map(|c| c.default_graph)
            .collect();
        v.reverse();
        v
    };
    {
        let s = Store::open(&root, opts()).unwrap();
        // 1: the default graph; 2: a named graph; 3: a bulk load into a named graph
        upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
        upd(&s, "INSERT DATA { GRAPH <urn:g> { <urn:a> <urn:p> 1 } }");
        let r = s.load_as(&[named(100)], CommitKind::Load).unwrap();
        assert!(r.commit.bulk && !r.commit.default_graph);
        // 4: a named graph through the WAL of the new generation
        upd(&s, "DELETE DATA { GRAPH <urn:g> { <urn:a> <urn:p> 1 } }");
        assert_eq!(flags(&s), [true, true, false, false, false]);
        assert!(s.default_graph_changed(0, 4));
        assert!(!s.default_graph_changed(1, 4));
        assert!(!s.default_graph_changed(1, 2));
        assert!(s.default_graph_changed(0, 1));
        // 5: a bulk load into the default graph
        s.load_as(&[ttl(100, "d")], CommitKind::Load).unwrap();
        assert!(s.default_graph_changed(4, 5));
        // a past position is answered from the records
        assert!(!s.default_graph_changed(2, 4));
        upd(&s, "INSERT DATA { GRAPH <urn:g> { <urn:b> <urn:p> 1 } }");
    }
    // replay and the catalog keep the flags, and so does compaction
    let s = Store::open(&root, opts()).unwrap();
    assert_eq!(flags(&s), [true, true, false, false, false, true, false]);
    assert!(!s.default_graph_changed(5, 6));
    s.compact().unwrap();
    drop(s);
    let s = Store::open(&root, opts()).unwrap();
    assert_eq!(flags(&s), [true, true, false, false, false, true, false]);
    assert!(!s.default_graph_changed(5, 6));
    assert!(s.default_graph_changed(4, 6));
    // in-memory stores track the same flag
    let m = Store::in_memory(StoreOptions::default());
    upd(&m, "INSERT DATA { GRAPH <urn:g> { <urn:a> <urn:p> 1 } }");
    assert!(!m.default_graph_changed(0, 1));
    upd(&m, "INSERT DATA { <urn:a> <urn:p> 1 }");
    assert!(m.default_graph_changed(1, 2));
}
