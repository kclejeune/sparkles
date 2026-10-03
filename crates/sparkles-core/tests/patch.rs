//! Applying RDF Patch (`Store::apply_patch`, spec F10 Phase 1): the acceptance examples
//! that do not need a server, round trips through the diff, Jena-written patches
//! (`tests/patch`, see `GenPatch.java`), the bulk path and crash safety of a patch
//! commit.

use oxrdf::Dataset as RdfDataset;
use oxrdf::dataset::CanonicalizationAlgorithm;
use sparkles_core::commit::CommitKind;
use sparkles_core::history::At;
use sparkles_core::patch::{PatchErrorKind, PatchWriter, commit_iri};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{DiffOptions, PatchOptions, PatchOutcome, Store, StoreOptions};
use sparkles_core::{Error, Result};
use std::path::Path;

fn mem() -> Store {
    Store::in_memory(StoreOptions::default())
}

/// A persistent store, which keeps the states a diff reads.
fn disk() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    (dir, s)
}

fn apply(s: &Store, patch: &str) -> Result<PatchOutcome> {
    s.apply_patch(patch.as_bytes(), &PatchOptions::default())
}

fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap();
}

fn ask(s: &Store, q: &str) -> bool {
    sparkles_core::sparql::query(s.snapshot(), q, &QueryOptions::default())
        .unwrap()
        .boolean
}

fn kind(e: Error) -> PatchErrorKind {
    match e {
        Error::Patch(p) => p.kind,
        e => panic!("not a patch error: {e}"),
    }
}

/// The store's quads, canonicalized (blank nodes relabelled canonically).
fn canonical(s: &Store) -> Vec<String> {
    let mut out = Vec::new();
    s.dump_nquads(&mut out).unwrap();
    let mut d: RdfDataset = oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::NQuads)
        .for_slice(&out)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .collect();
    d.canonicalize(CanonicalizationAlgorithm::Unstable);
    let mut lines: Vec<String> = d.iter().map(|q| q.to_string()).collect();
    lines.sort();
    lines
}

/// The patch of a store's diff between two commits, as the diff endpoint writes it.
fn diff_patch(s: &Store, from: u64, to: u64, binary: bool) -> Vec<u8> {
    let d = s
        .diff(&At::Commit(from), &At::Commit(to), &DiffOptions::default())
        .unwrap();
    let id = s.dataset_id();
    let mut w = PatchWriter::new(Vec::new(), binary);
    sparkles_core::patch::write_patch(
        &mut w,
        &commit_iri(id, to),
        Some(&commit_iri(id, from)),
        d.iter().collect::<Vec<_>>().iter().map(|(op, q)| (*op, q)),
    )
    .unwrap();
    w.into_inner()
}

#[test]
fn p1_a_text_patch_is_one_commit_of_kind_patch() {
    let s = mem();
    let o = apply(
        &s,
        "TX .\nA <urn:a> <urn:p> \"1\" .\nA <urn:b> <urn:p> <urn:c> <urn:g> .\nTC .\n",
    )
    .unwrap();
    assert!(o.receipt.committed);
    assert_eq!((o.inserted, o.deleted, o.rows), (2, 0, 4));
    assert_eq!(o.receipt.commit.seq, 1);
    assert_eq!(s.commit(1).unwrap().kind, CommitKind::Patch);
    assert_eq!(CommitKind::Patch.code(), 12);
    assert_eq!(CommitKind::from_name("patch"), Some(CommitKind::Patch));
    assert!(ask(&s, "ASK { GRAPH <urn:g> { <urn:b> <urn:p> <urn:c> } }"));
}

#[test]
fn p2_p3_no_net_change_makes_no_commit_and_rows_apply_in_order() {
    let s = mem();
    apply(&s, "A <urn:a> <urn:p> \"1\" .").unwrap();
    let o = apply(&s, "A <urn:a> <urn:p> \"1\" .\nD <urn:x> <urn:p> <urn:y> .").unwrap();
    assert!(!o.receipt.committed);
    assert_eq!((o.inserted, o.deleted, o.receipt.commit.seq), (0, 0, 1));
    let o = apply(&s, "A <urn:z> <urn:p> \"1\" . D <urn:z> <urn:p> \"1\" .").unwrap();
    assert!(!o.receipt.committed);
    assert_eq!((o.inserted, o.deleted), (1, 1));
    assert!(!ask(&s, "ASK { <urn:z> ?p ?o }"));
}

#[test]
fn p4_prev_is_a_precondition_within_the_dataset() {
    let s = mem();
    apply(&s, "A <urn:a> <urn:p> 1 .").unwrap();
    let id = s.dataset_id();
    let p = format!("H prev <{}> .\nA <urn:b> <urn:p> 2 .", commit_iri(id, 1));
    let o = apply(&s, &p).unwrap();
    assert!(o.prev_checked);
    assert_eq!(o.receipt.commit.seq, 2);
    match apply(&s, &p).unwrap_err() {
        Error::Patch(e) => {
            assert_eq!(e.kind, PatchErrorKind::PrevMismatch);
            let m = e.mismatch.unwrap();
            assert_eq!((m.expected, m.head), (1, 2));
            assert_eq!(m.prev, commit_iri(id, 1));
        }
        e => panic!("{e}"),
    }
    assert_eq!(s.head_commit().seq, 2);
    // a prev of another dataset, or not a commit IRI, is ignored
    for prev in [
        "<urn:uuid:00000000-0000-4000-8000-000000000000#commit:1>",
        "<uuid:bbe2edae-325e-11ec-abcc-a70bbba0dfb1>",
    ] {
        let o = apply(
            &s,
            &format!(
                "H prev {prev} .\nA <urn:c> <urn:p> {} .",
                s.head_commit().seq
            ),
        )
        .unwrap();
        assert!(!o.prev_checked);
        assert!(o.receipt.committed);
    }
    // a prev after the first data row is checked too
    let late = format!("A <urn:d> <urn:p> 1 .\nH prev <{}> .", commit_iri(id, 1));
    assert_eq!(
        kind(apply(&s, &late).unwrap_err()),
        PatchErrorKind::PrevMismatch
    );
    assert!(!ask(&s, "ASK { <urn:d> ?p ?o }"));
}

#[test]
fn p5_ta_aborts_the_whole_patch() {
    let s = mem();
    for p in [
        "TX . A <urn:q> <urn:p> \"1\" . TA .",
        "TX . A <urn:q> <urn:p> \"1\" . TC . TX . PA \"ex\" <http://example/> . TA .",
    ] {
        let o = apply(&s, p).unwrap();
        assert!(o.aborted);
        assert!(!o.receipt.committed);
        assert_eq!(o.receipt.commit.seq, 0);
    }
    assert!(!ask(&s, "ASK { <urn:q> ?p ?o }"));
    assert!(s.prefixes().is_empty());
}

#[test]
fn p6_prefix_rows_change_the_prefix_map_without_a_commit() {
    let s = mem();
    let o = apply(&s, "PA \"ex\" <http://example.org/> .").unwrap();
    assert_eq!((o.prefixes_set, o.prefixes_removed), (1, 0));
    assert!(!o.receipt.committed);
    assert_eq!(s.prefixes()["ex"], "http://example.org/");
    // a graph term does not narrow the change
    let o = apply(&s, "PD \"ex\" <urn:g> .").unwrap();
    assert_eq!(o.prefixes_removed, 1);
    assert!(s.prefixes().is_empty());
    // an invalid prefix applies nothing, data included
    let e = apply(
        &s,
        "A <urn:a> <urn:p> 1 .\nPA \"1bad\" <http://example.org/> .",
    )
    .unwrap_err();
    assert_eq!(kind(e), PatchErrorKind::Term);
    assert_eq!(s.head_commit().seq, 0);
}

#[test]
fn p7_a_diff_applied_to_an_empty_dataset_gives_an_isomorphic_dataset() {
    let (_dir, a) = disk();
    upd(
        &a,
        "INSERT DATA { <urn:s> <urn:p> 1 , \"x\"@en , \"y\"@ar--rtl . GRAPH <urn:g> { <urn:s> <urn:q> <<( <urn:a> <urn:b> <urn:c> )>> } }",
    );
    upd(
        &a,
        "INSERT { _:x <urn:p> _:y . _:y <urn:p> <urn:s> . GRAPH <urn:g2> { _:x <urn:r> <<( _:x <urn:p> 2 )>> } } WHERE {}",
    );
    upd(&a, "INSERT { _:z <urn:p> 3 } WHERE {}");
    upd(&a, "DELETE DATA { <urn:s> <urn:p> 1 }");
    upd(&a, "DELETE { ?s <urn:p> 3 } WHERE { ?s <urn:p> 3 }");
    let head = a.head_commit().seq;
    for binary in [false, true] {
        let b = mem();
        let patch = diff_patch(&a, 0, head, binary);
        let o = b
            .apply_patch(
                &patch[..],
                &PatchOptions {
                    binary,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(!o.prev_checked, "another dataset's prev");
        assert_eq!(canonical(&a), canonical(&b), "binary: {binary}");
    }
}

#[test]
fn p8_stored_blank_node_labels_name_stored_nodes_within_the_dataset() {
    let (_dir, s) = disk();
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
    upd(&s, "INSERT DATA { <urn:a> <urn:p> 2 }");
    upd(&s, "INSERT DATA { _:x <urn:p> 1 }");
    let text = String::from_utf8(diff_patch(&s, 2, 3, false)).unwrap();
    // the diff's adds turned into deletes, with prev set to commit 3
    let undo = |prev: &str| {
        text.lines()
            .filter_map(|l| l.strip_prefix("A "))
            .map(|l| format!("D {l}\n"))
            .fold(format!("H prev <{prev}> .\n"), |a, b| a + &b)
    };
    let foreign = undo("urn:uuid:00000000-0000-4000-8000-000000000000#commit:3");
    let o = apply(&s, &foreign).unwrap();
    assert_eq!(o.deleted, 0);
    assert!(ask(&s, "ASK { ?b <urn:p> 1 FILTER isBlank(?b) }"));
    let own = undo(&commit_iri(s.dataset_id(), 3));
    let o = apply(&s, &own).unwrap();
    assert_eq!(o.deleted, 1);
    assert!(!ask(&s, "ASK { ?b <urn:p> 1 FILTER isBlank(?b) }"));
    // a local label is the same node throughout the patch, and a new one
    apply(
        &s,
        "A _:n <urn:p> 5 . A _:n <urn:q> 6 . A <_:b0> <urn:p> 7 .",
    )
    .unwrap();
    assert!(ask(
        &s,
        "ASK { ?b <urn:p> 5 ; <urn:q> 6 FILTER isBlank(?b) }"
    ));
    assert!(!ask(&s, "ASK { ?b <urn:p> 5 , 7 }"));
}

#[test]
fn p9_jena_patches_apply_alike_in_both_forms() {
    let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/patch"));
    let t = mem();
    let o = t
        .apply_patch(
            &std::fs::read(dir.join("jena-1.rdfp")).unwrap()[..],
            &PatchOptions::default(),
        )
        .unwrap();
    assert_eq!((o.inserted, o.deleted, o.rows), (10, 1, 17));
    assert_eq!((o.prefixes_set, o.prefixes_removed), (2, 1));
    let b = mem();
    b.apply_patch(
        &std::fs::read(dir.join("jena-1.trp")).unwrap()[..],
        &PatchOptions {
            binary: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(canonical(&t), canonical(&b));
    assert_eq!(t.prefixes(), b.prefixes());
    assert_eq!(t.prefixes()["foaf"], "http://xmlns.com/foaf/0.1/");
    assert!(ask(
        &t,
        "ASK { <http://example/s> <http://example/p> 123, 12.5, 1.0e0, true, \"chat\"@fr }"
    ));
}

#[test]
fn p11_a_malformed_row_applies_nothing() {
    let s = mem();
    let e = apply(
        &s,
        "A <urn:a> <urn:p> 1 .\nA <urn:b> <urn:p> 2 .\nA <urn:c> <urn:p> .\n",
    )
    .unwrap_err();
    match e {
        Error::Patch(p) => {
            assert_eq!(p.kind, PatchErrorKind::Syntax);
            assert_eq!((p.line, p.row), (Some(3), Some(3)));
        }
        e => panic!("{e}"),
    }
    assert_eq!(s.head_commit().seq, 0);
    assert!(s.snapshot().is_empty());
}

#[test]
fn p14_dry_runs_apply_nothing_and_message_headers_annotate() {
    let s = mem();
    let e = s
        .apply_patch(
            &b"A <urn:a> <urn:p> 1 .\nPA \"ex\" <http://example.org/> ."[..],
            &PatchOptions {
                write: sparkles_core::guard::WriteOptions {
                    dry_run: Some(Default::default()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap_err();
    let Error::DryRun(p) = e else { panic!("{e}") };
    assert_eq!(p.commit.unwrap().inserted, 1);
    assert!(s.snapshot().is_empty());
    assert!(s.prefixes().is_empty());
    let o = apply(&s, "H message \"from a patch\" .\nA <urn:a> <urn:p> 1 .").unwrap();
    assert_eq!(
        s.annotation(o.receipt.commit.seq)
            .unwrap()
            .message
            .as_deref(),
        Some("from a patch")
    );
}

#[test]
fn large_patches_of_adds_take_the_bulk_path() {
    let s = Store::in_memory(StoreOptions {
        bulk_threshold: 10,
        ..Default::default()
    });
    let mut p = String::from("TX .\n");
    for i in 0..30 {
        p.push_str(&format!(
            "A <urn:s{i}> <urn:p> {i} .\nA <urn:s{i}> <urn:p> {i} .\n"
        ));
    }
    p.push_str("TC .\n");
    let o = apply(&s, &p).unwrap();
    assert!(o.receipt.commit.bulk);
    assert_eq!((o.inserted, o.receipt.commit.quads), (30, 30));
    assert_eq!(o.receipt.commit.kind, CommitKind::Patch);
    // a delete keeps the patch on the transaction's path
    let o = apply(&s, &format!("{p}D <urn:s0> <urn:p> 0 .")).unwrap();
    assert!(!o.receipt.commit.bulk);
    assert_eq!((o.inserted, o.deleted), (0, 1));
}

fn wal(root: &Path) -> std::path::PathBuf {
    let cur = std::fs::read_to_string(root.join("CURRENT")).unwrap();
    root.join(cur.trim()).join("wal.log")
}

/// A patch commit is a WAL commit like any other: it replays with its kind and its
/// blank-node counter, and a torn tail of it is dropped at open.
#[test]
fn patch_commits_survive_restarts_and_torn_tails() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        apply(&s, "A _:x <urn:p> 1 .").unwrap();
        apply(
            &s,
            "A <urn:a> <urn:p> 2 .\nPA \"ex\" <http://example.org/> .",
        )
        .unwrap();
    }
    let path = wal(&root);
    let good = std::fs::read(&path).unwrap();
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 2);
        assert_eq!(s.commit(1).unwrap().kind, CommitKind::Patch);
        assert_eq!(s.commit(2).unwrap().kind, CommitKind::Patch);
        assert_eq!(s.prefixes()["ex"], "http://example.org/");
        // the blank-node counter was replayed: a new node is a different one
        apply(&s, "A _:x <urn:p> 3 .").unwrap();
        assert!(!ask(&s, "ASK { ?b <urn:p> 1, 3 }"));
    }
    // commit 3 torn half-way: dropped, and the next commit takes its number
    let full = std::fs::read(&path).unwrap();
    std::fs::write(&path, &full[..good.len() + (full.len() - good.len()) / 2]).unwrap();
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 2);
        assert!(!ask(&s, "ASK { ?b <urn:p> 3 }"));
        assert_eq!(std::fs::read(&path).unwrap(), good);
        let o = apply(&s, "A <urn:c> <urn:p> 4 .").unwrap();
        assert_eq!(o.receipt.commit.seq, 3);
    }
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(s.head_commit().seq, 3);
    assert_eq!(s.commit(3).unwrap().kind, CommitKind::Patch);
}
