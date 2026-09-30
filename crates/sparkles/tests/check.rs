//! `sparkles::check`: a clean database passes, specific damage fails the right check,
//! and checking never changes a byte.

use sparkles::check::{CheckOptions, CheckReport, Status, check};
use sparkles::index::{Key, Perm, PermIndex};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::update::update;
use sparkles::store::{Store, StoreOptions};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const WAL_REC: usize = 33;
const CATALOG_REC: usize = 64;

fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap();
}

/// 40,000 quads (two blocks per permutation) with blank nodes, named graphs, a triple
/// term, inline and vocabulary literals.
fn data() -> Source {
    let mut t = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..40_000 {
        match i % 10 {
            0 => t.push_str(&format!("ex:s{i} ex:label \"item number {i}\"@en .\n")),
            1 => t.push_str(&format!("GRAPH ex:g{} {{ ex:s{i} ex:p _:b{i} }}\n", i % 3)),
            2 => t.push_str(&format!("_:b{} ex:q \"{i}\" .\n", i - 1)),
            _ => t.push_str(&format!("ex:s{i} ex:n{} {i} .\n", i % 7)),
        }
    }
    t.push_str("ex:a ex:says <<( ex:b ex:p \"x\" )>> .\n");
    Source::from_bytes(t.into_bytes(), RdfFormat::TriG, None)
}

/// A database with a bulk load, WAL updates (inserts, deletes, a DELETE/INSERT), a
/// compaction, more updates after it, full-text search (when built with it) and a
/// reasoning status, closed cleanly. Built once; each test works on a copy.
fn template() -> &'static Path {
    static T: OnceLock<PathBuf> = OnceLock::new();
    T.get_or_init(|| {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("check-template-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        s.load(&[data()]).unwrap();
        upd(&s, "PREFIX ex: <http://ex.org/> INSERT DATA { ex:new ex:label \"a brand new label\" . GRAPH ex:g9 { ex:new ex:p 1 } }");
        upd(&s, "PREFIX ex: <http://ex.org/> DELETE DATA { ex:s0 ex:label \"item number 0\"@en }");
        upd(&s, "PREFIX ex: <http://ex.org/> DELETE { ?s ex:n3 ?o } INSERT { ?s ex:m3 ?o } WHERE { ?s ex:n3 ?o FILTER(?o < 200) }");
        #[cfg(feature = "text")]
        s.enable_text(sparkles::text::TextConfig::default()).unwrap();
        s.compact().unwrap();
        upd(&s, "PREFIX ex: <http://ex.org/> INSERT DATA { ex:later ex:label \"added after compaction\" }");
        upd(&s, "PREFIX ex: <http://ex.org/> DELETE DATA { ex:s10 ex:label \"item number 10\"@en }");
        upd(&s, "PREFIX ex: <http://ex.org/> INSERT DATA { _:x ex:label \"blank\" . ex:new ex:label \"another\" }");
        let head = s.head_commit();
        assert_eq!(head.seq, 7);
        let id = s.dataset_id();
        drop(s);
        std::fs::write(
            root.join("reasoning.json"),
            format!(
                r#"{{"reasoningFormat":2,"profile":"rdfs","inferred":12,"at":"2026-09-30T00:00:00.000Z","commit":{},"positionSource":"commit","datasetId":"{id}"}}"#,
                head.seq
            ),
        )
        .unwrap();
        root
    })
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), to).unwrap();
        }
    }
}

/// A fresh copy of the template.
fn fresh() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    copy_dir(template(), &root);
    (dir, root)
}

/// Every file under `dir` with its contents.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(d: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), out);
            } else {
                out.insert(e.path(), std::fs::read(e.path()).unwrap());
            }
        }
    }
    walk(dir, &mut out);
    out
}

/// Check in both modes, asserting that nothing under `root` changes.
fn run(root: &Path, quick: bool) -> CheckReport {
    let before = snapshot(root);
    let r = check(root, &CheckOptions { quick }).unwrap();
    let after = snapshot(root);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "check created or removed files"
    );
    for (p, b) in &before {
        assert!(after[p] == *b, "check changed {}", p.display());
    }
    // the JSON form serializes
    serde_json::to_string(&r).unwrap();
    r
}

fn status(r: &CheckReport, name: &str) -> Status {
    r.get(name)
        .unwrap_or_else(|| panic!("no check {name}:\n{}", r.to_text()))
        .status
}

fn messages(r: &CheckReport, name: &str) -> String {
    r.get(name)
        .map(|c| {
            c.issues
                .iter()
                .map(|i| i.message.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn gen_dir(root: &Path) -> PathBuf {
    root.join(
        std::fs::read_to_string(root.join("CURRENT"))
            .unwrap()
            .trim(),
    )
}

#[test]
fn a_clean_database_passes_in_both_modes() {
    let (_d, root) = fresh();
    for quick in [false, true] {
        let r = run(&root, quick);
        assert_eq!(r.status, Status::Ok, "{}", r.to_text());
        assert_eq!((r.errors, r.warnings, r.exit_code()), (0, 0, 0));
        assert_eq!(r.head, Some(7));
        assert_eq!(r.generation.as_deref(), Some("gen-0003"));
        let names: Vec<&str> = r.checks.iter().map(|c| c.name.as_str()).collect();
        for n in [
            "layout",
            "generation",
            "vocabulary",
            "delta-vocabulary",
            "perm.spo",
            "perm.gspo",
            "permutations",
            "wal",
            "catalog",
            "reasoning",
        ] {
            assert!(names.contains(&n), "{n} missing from {names:?}");
        }
        #[cfg(feature = "text")]
        assert!(names.contains(&"text"));
    }
    // and the database still opens (the check left nothing half-done)
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, 7);
}

#[test]
fn checking_next_to_a_writing_store_finds_no_errors() {
    let (_d, root) = fresh();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            for i in 0..40 {
                upd(
                    &s,
                    &format!("INSERT DATA {{ <urn:live{i}> <http://ex.org/label> \"live {i}\" }}"),
                );
            }
        });
        for _ in 0..5 {
            let r = check(&root, &CheckOptions::default()).unwrap();
            assert_eq!(r.errors, 0, "{}", r.to_text());
        }
        writer.join().unwrap();
    });
    let r = check(&root, &CheckOptions::default()).unwrap();
    assert_eq!(r.errors, 0, "{}", r.to_text());
    assert_eq!(r.head, Some(47));
}

#[test]
fn a_bad_current_is_an_error() {
    let (_d, root) = fresh();
    std::fs::write(root.join("CURRENT"), "gen-0099").unwrap();
    let r = run(&root, true);
    assert_eq!(status(&r, "layout"), Status::Error);
    assert!(
        messages(&r, "layout").contains("does not exist"),
        "{}",
        r.to_text()
    );
    assert_eq!(r.exit_code(), 1);
    std::fs::write(root.join("CURRENT"), "../elsewhere").unwrap();
    let r = run(&root, true);
    assert!(
        messages(&r, "layout").contains("not a generation"),
        "{}",
        r.to_text()
    );
    std::fs::remove_file(root.join("CURRENT")).unwrap();
    let r = run(&root, false);
    assert!(
        messages(&r, "layout").contains("missing"),
        "{}",
        r.to_text()
    );
    assert_eq!(r.status, Status::Error);
}

#[test]
fn leftovers_are_warnings() {
    let (_d, root) = fresh();
    std::fs::create_dir(root.join("text.new")).unwrap();
    std::fs::write(root.join("prefixes.tmp"), b"{").unwrap();
    std::fs::create_dir(root.join("gen-0002")).unwrap();
    let r = run(&root, true);
    assert_eq!(status(&r, "layout"), Status::Warning, "{}", r.to_text());
    assert_eq!(r.get("layout").unwrap().warnings, 3);
    assert_eq!(r.exit_code(), 2);
}

#[test]
fn a_flipped_byte_in_a_permutation_block_is_found() {
    let (_d, root) = fresh();
    let dir = gen_dir(&root);
    let idx = PermIndex::open(&dir, Perm::Spo).unwrap();
    let m = &idx.blocks[0];
    // inside the object column of the first block
    let at = m.offset as usize + (m.col_len[0] + m.col_len[1]) as usize + m.col_len[2] as usize / 2;
    drop(idx);
    let path = dir.join("spo.dat");
    let mut b = std::fs::read(&path).unwrap();
    b[at] ^= 0x20;
    std::fs::write(&path, &b).unwrap();
    let r = run(&root, false);
    assert_eq!(r.status, Status::Error, "{}", r.to_text());
    // the block no longer decodes, or it decodes to other quads than the others hold
    let spo = status(&r, "perm.spo") == Status::Error;
    let perms = messages(&r, "permutations").contains("spo");
    assert!(spo || perms, "{}", r.to_text());
    // metadata alone cannot see it
    assert_eq!(run(&root, true).status, Status::Ok);
}

#[test]
fn a_truncated_permutation_file_is_found_in_both_modes() {
    let (_d, root) = fresh();
    let path = gen_dir(&root).join("pos.dat");
    let len = std::fs::metadata(&path).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(len - 100)
        .unwrap();
    for quick in [true, false] {
        let r = run(&root, quick);
        assert_eq!(status(&r, "perm.pos"), Status::Error, "{}", r.to_text());
        assert!(
            messages(&r, "perm.pos").contains("truncated"),
            "{}",
            r.to_text()
        );
        assert_eq!(r.exit_code(), 1);
    }
}

// ---- crafted blocks: re-encode a permutation with the same block layout ------------

fn encode_column(col: &[u64]) -> Vec<u8> {
    let mut raw = Vec::new();
    let mut prev = 0u64;
    for &v in col {
        let d = v.wrapping_sub(prev) as i64;
        let mut z = ((d << 1) ^ (d >> 63)) as u64;
        while z >= 0x80 {
            raw.push((z as u8) | 0x80);
            z >>= 7;
        }
        raw.push(z as u8);
        prev = v;
    }
    lz4_flex::compress_prepend_size(&raw)
}

/// Decode every block of `perm`, let `edit` change the keys, and write the permutation
/// back with consistent metadata (first / last keys, offsets, row counts).
fn rewrite_perm(dir: &Path, perm: Perm, edit: impl FnOnce(&mut Vec<Vec<Key>>)) {
    let idx = PermIndex::open(dir, perm).unwrap();
    let mut blocks: Vec<Vec<Key>> = (0..idx.blocks.len())
        .map(|b| {
            let blk = idx.decode_block(b).unwrap();
            (0..blk.len()).map(|i| blk.key(i)).collect()
        })
        .collect();
    drop(idx);
    edit(&mut blocks);
    let (mut dat, mut meta) = (Vec::new(), Vec::new());
    let mut row_start = 0u64;
    for keys in &blocks {
        let offset = dat.len() as u64;
        let mut col_len = [0u32; 4];
        for (c, len) in col_len.iter_mut().enumerate() {
            let enc = encode_column(&keys.iter().map(|k| k[c]).collect::<Vec<_>>());
            *len = enc.len() as u32;
            dat.extend_from_slice(&enc);
        }
        let (first, last) = (keys[0], keys[keys.len() - 1]);
        for v in first.iter().chain(last.iter()) {
            meta.extend_from_slice(&v.to_le_bytes());
        }
        meta.extend_from_slice(&offset.to_le_bytes());
        for l in col_len {
            meta.extend_from_slice(&l.to_le_bytes());
        }
        meta.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        meta.extend_from_slice(&row_start.to_le_bytes());
        row_start += keys.len() as u64;
    }
    std::fs::write(dir.join(format!("{}.dat", perm.name())), dat).unwrap();
    std::fs::write(dir.join(format!("{}.meta", perm.name())), meta).unwrap();
}

#[test]
fn a_rewritten_permutation_without_changes_passes() {
    let (_d, root) = fresh();
    rewrite_perm(&gen_dir(&root), Perm::Osp, |_| {});
    let r = run(&root, false);
    assert_eq!(r.status, Status::Ok, "{}", r.to_text());
}

#[test]
fn swapped_keys_break_the_order_but_not_the_quads() {
    let (_d, root) = fresh();
    rewrite_perm(&gen_dir(&root), Perm::Pso, |b| b[0].swap(100, 101));
    let r = run(&root, false);
    assert_eq!(status(&r, "perm.pso"), Status::Error, "{}", r.to_text());
    let m = messages(&r, "perm.pso");
    assert!(m.contains("sorts before"), "{m}");
    let issue = &r.get("perm.pso").unwrap().issues[0];
    assert_eq!((issue.block, issue.row), (Some(0), Some(101)));
    // the same set of quads
    assert_eq!(status(&r, "permutations"), Status::Ok, "{}", r.to_text());
    // block metadata still looks fine
    assert_eq!(run(&root, true).status, Status::Ok);
}

#[test]
fn a_duplicated_key_is_found() {
    let (_d, root) = fresh();
    rewrite_perm(&gen_dir(&root), Perm::Ops, |b| b[0][201] = b[0][200]);
    let r = run(&root, false);
    assert!(
        messages(&r, "perm.ops").contains("repeats"),
        "{}",
        r.to_text()
    );
    // one quad replaced by a copy of another: OPS no longer holds the others' quads
    assert!(
        messages(&r, "permutations").contains("ops"),
        "{}",
        r.to_text()
    );
}

#[test]
fn a_duplicated_key_across_blocks_is_found_from_metadata() {
    let (_d, root) = fresh();
    rewrite_perm(&gen_dir(&root), Perm::Gspo, |b| {
        assert!(b.len() >= 2);
        let first = b[1][0];
        let n = b[0].len();
        b[0][n - 1] = first;
    });
    let r = run(&root, true);
    assert_eq!(status(&r, "perm.gspo"), Status::Error, "{}", r.to_text());
    assert!(
        messages(&r, "perm.gspo").contains("does not follow"),
        "{}",
        r.to_text()
    );
    let r = run(&root, false);
    assert!(
        messages(&r, "permutations").contains("gspo"),
        "{}",
        r.to_text()
    );
    assert!(
        messages(&r, "permutations").contains("SPO and GSPO"),
        "{}",
        r.to_text()
    );
}

// ---- WAL ------------------------------------------------------------------------------

#[test]
fn a_wal_checksum_mismatch_mid_log_is_an_error() {
    let (_d, root) = fresh();
    let path = gen_dir(&root).join("wal.log");
    let mut b = std::fs::read(&path).unwrap();
    b[5] ^= 0x01; // a data record of the first transaction
    std::fs::write(&path, &b).unwrap();
    let r = run(&root, true);
    assert_eq!(status(&r, "wal"), Status::Error, "{}", r.to_text());
    assert!(
        messages(&r, "wal").contains("checksum mismatch"),
        "{}",
        r.to_text()
    );
    // which open refuses too
    assert!(Store::open(&root, StoreOptions::default()).is_err());
}

#[test]
fn a_torn_final_wal_transaction_is_a_warning() {
    let (_d, root) = fresh();
    let path = gen_dir(&root).join("wal.log");
    let good = std::fs::read(&path).unwrap();
    // an interrupted append: a copy of the last transaction whose commit record is damaged
    let recs = good.len() / WAL_REC;
    let last_start = (0..recs - 1)
        .rev()
        .find(|&i| good[i * WAL_REC] == 3)
        .map_or(0, |i| i + 1);
    let mut torn = good.clone();
    torn.extend_from_slice(&good[last_start * WAL_REC..]);
    let n = torn.len();
    torn[n - 2] ^= 0xff;
    std::fs::write(&path, &torn).unwrap();
    let r = run(&root, false);
    assert_eq!(status(&r, "wal"), Status::Warning, "{}", r.to_text());
    assert!(
        messages(&r, "wal").contains("truncated on open"),
        "{}",
        r.to_text()
    );
    assert_eq!(r.status, Status::Warning, "{}", r.to_text());
    assert_eq!(r.exit_code(), 2);
    assert_eq!(r.head, Some(7));
    // a partial record at the very end too
    let mut partial = good.clone();
    partial.extend_from_slice(&good[..10]);
    std::fs::write(&path, &partial).unwrap();
    let r = run(&root, false);
    assert_eq!(r.status, Status::Warning, "{}", r.to_text());
    // open agrees: the head is unchanged
    std::fs::write(&path, &torn).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, 7);
}

#[test]
fn a_damaged_final_commit_that_the_catalog_lists_is_an_error() {
    let (_d, root) = fresh();
    let path = gen_dir(&root).join("wal.log");
    let mut b = std::fs::read(&path).unwrap();
    let n = b.len();
    b[n - 2] ^= 0xff; // the CRC of the last (acknowledged) commit
    std::fs::write(&path, &b).unwrap();
    let r = run(&root, false);
    assert_eq!(status(&r, "wal"), Status::Warning, "{}", r.to_text());
    // the catalog proves the commit was acknowledged: data would be lost
    assert_eq!(status(&r, "catalog"), Status::Error, "{}", r.to_text());
    assert!(
        messages(&r, "catalog").contains("missing from the WAL"),
        "{}",
        r.to_text()
    );
}

#[test]
fn an_unknown_wal_record_before_the_last_commit_is_an_error() {
    let (_d, root) = fresh();
    let path = gen_dir(&root).join("wal.log");
    let mut b = std::fs::read(&path).unwrap();
    b[0] = 0x7f;
    std::fs::write(&path, &b).unwrap();
    let r = run(&root, true);
    assert!(
        messages(&r, "wal").contains("unknown type"),
        "{}",
        r.to_text()
    );
    assert_eq!(status(&r, "wal"), Status::Error);
}

// ---- catalog --------------------------------------------------------------------------

fn remove_catalog_record(root: &Path, seq: u64) {
    let path = root.join("commits.bin");
    let mut b = std::fs::read(&path).unwrap();
    let at = CATALOG_REC * (1 + seq as usize);
    b.drain(at..at + CATALOG_REC);
    std::fs::write(&path, &b).unwrap();
}

#[test]
fn a_gap_in_the_catalog_before_the_generation_is_an_error() {
    let (_d, root) = fresh();
    // commits 0..=4 predate gen-0003 (base commit 4): only the catalog holds them
    remove_catalog_record(&root, 2);
    let r = run(&root, true);
    assert_eq!(status(&r, "catalog"), Status::Error, "{}", r.to_text());
    let c = r.get("catalog").unwrap();
    assert!(c.issues[0].message.contains("gap"), "{}", r.to_text());
    assert_eq!(c.issues[0].seq, Some(2));
}

#[test]
fn a_gap_in_the_catalog_within_the_wal_is_a_warning() {
    let (_d, root) = fresh();
    remove_catalog_record(&root, 6);
    let r = run(&root, true);
    assert_eq!(status(&r, "catalog"), Status::Warning, "{}", r.to_text());
    assert!(
        messages(&r, "catalog").contains("rebuilt from the WAL"),
        "{}",
        r.to_text()
    );
    // a lagging catalog (an append lost in a crash) is repaired on open as well
    let (_d, root) = fresh();
    let path = root.join("commits.bin");
    let len = std::fs::metadata(&path).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(len - CATALOG_REC as u64)
        .unwrap();
    let r = run(&root, true);
    assert_eq!(status(&r, "catalog"), Status::Warning, "{}", r.to_text());
    assert!(
        messages(&r, "catalog").contains("lags the WAL"),
        "{}",
        r.to_text()
    );
}

#[test]
fn a_catalog_of_another_dataset_is_an_error() {
    let (_d, root) = fresh();
    let path = root.join("commits.bin");
    let mut b = std::fs::read(&path).unwrap();
    // a different dataset id with a valid header checksum
    b[16] ^= 1;
    let crc = {
        let mut c = flate2::Crc::new();
        c.update(&b[..60]);
        c.sum()
    };
    b[60..64].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(&path, &b).unwrap();
    let r = run(&root, true);
    assert!(
        messages(&r, "catalog").contains("belongs to dataset"),
        "{}",
        r.to_text()
    );
    assert_eq!(r.status, Status::Error);
}

// ---- full-text index --------------------------------------------------------------------

#[cfg(feature = "text")]
#[test]
fn a_missing_text_segment_file_is_an_error() {
    let (_d, root) = fresh();
    let seg = std::fs::read_dir(root.join("text"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "idx"))
        .expect("a segment file");
    std::fs::remove_file(&seg).unwrap();
    for quick in [true, false] {
        let r = run(&root, quick);
        assert_eq!(status(&r, "text"), Status::Error, "{}", r.to_text());
        assert!(messages(&r, "text").contains("missing"), "{}", r.to_text());
    }
    // with the dirty marker, open verifies and rebuilds it: a warning
    std::fs::write(root.join("text.dirty"), b"").unwrap();
    let r = run(&root, false);
    assert_eq!(status(&r, "text"), Status::Warning, "{}", r.to_text());
}

#[cfg(feature = "text")]
#[test]
fn a_damaged_text_segment_file_fails_its_checksum() {
    let (_d, root) = fresh();
    let seg = std::fs::read_dir(root.join("text"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "store"))
        .expect("a segment file");
    let mut b = std::fs::read(&seg).unwrap();
    b[0] ^= 0xff;
    std::fs::write(&seg, &b).unwrap();
    let r = run(&root, false);
    assert!(messages(&r, "text").contains("checksum"), "{}", r.to_text());
    assert_eq!(status(&r, "text"), Status::Error);
    // presence only in quick mode
    assert_eq!(status(&run(&root, true), "text"), Status::Ok);
}

#[cfg(feature = "text")]
#[test]
fn a_text_index_behind_the_wal_is_a_warning() {
    let (_d, root) = fresh();
    std::fs::write(root.join("text.json"), b"{ not json").unwrap();
    let r = run(&root, true);
    assert_eq!(status(&r, "text"), Status::Error, "{}", r.to_text());
    // another configuration than the index was built for: rebuilt on open
    let (_d, root) = fresh();
    std::fs::write(
        root.join("text.json"),
        br#"{"predicates": ["http://ex.org/label"]}"#,
    )
    .unwrap();
    let r = run(&root, true);
    assert_eq!(status(&r, "text"), Status::Warning, "{}", r.to_text());
    assert!(
        messages(&r, "text").contains("configuration"),
        "{}",
        r.to_text()
    );
}

// ---- reasoning status -------------------------------------------------------------------

#[test]
fn a_reasoning_status_naming_a_missing_commit_is_a_warning() {
    let (_d, root) = fresh();
    let path = root.join("reasoning.json");
    let s = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, s.replace("\"commit\":7", "\"commit\":70")).unwrap();
    let r = run(&root, true);
    assert_eq!(status(&r, "reasoning"), Status::Warning, "{}", r.to_text());
    std::fs::write(&path, b"{").unwrap();
    let r = run(&root, true);
    assert_eq!(status(&r, "reasoning"), Status::Error, "{}", r.to_text());
}

#[test]
fn a_missing_directory_is_an_error_result() {
    let dir = tempfile::tempdir().unwrap();
    assert!(check(&dir.path().join("nope"), &CheckOptions::default()).is_err());
}
