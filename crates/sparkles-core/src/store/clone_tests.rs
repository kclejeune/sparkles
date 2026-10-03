//! Clones by shared files, in memory, and of some graphs only.

use super::*;
use crate::access::GraphRule;
use crate::history::Hold;
use crate::io::{RdfFormat, Source};
use crate::sparql::QueryOptions;
use crate::sparql::update::update;
use parking_lot::Mutex;
use std::sync::atomic::AtomicBool;

const TRIG: &str = r#"@prefix ex: <http://ex.org/> .
ex:s ex:p _:x . _:x ex:q "v"@en--ltr .
GRAPH ex:g { ex:s ex:r <<( _:x ex:p 1 )>> }
GRAPH ex:h { ex:s ex:r 2 . ex:t ex:r 3 }
GRAPH _:gb { _:x ex:in _:gb }
GRAPH <urn:x-sparkles:inferred> { ex:s a ex:C }
"#;

fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap();
}

fn load(s: &Store, trig: &str) {
    s.load(&[Source::from_bytes(
        trig.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
}

fn dump(s: &Store) -> Vec<String> {
    let mut out = Vec::new();
    s.dump_nquads(&mut out).unwrap();
    let mut v: Vec<String> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

fn objects(s: &Store, subject: &str) -> Vec<String> {
    let q = format!("SELECT ?o {{ <{subject}> <urn:p> ?o }}");
    let r = crate::sparql::query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
    r.rows()
        .into_iter()
        .map(|row| row.into_iter().flatten().map(|t| t.to_string()).collect())
        .collect()
}

fn gens(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("gen-"))
        .collect();
    v.sort();
    v
}

/// A persistent source whose quads are all in its base generation.
fn source(root: &Path) -> Store {
    let s = Store::open(root, StoreOptions::default()).unwrap();
    load(&s, TRIG);
    s.set_prefix("ex", "http://ex.org/").unwrap();
    assert!(s.snapshot().delta.is_empty());
    s
}

fn checked(dir: &Path) -> Store {
    let report = crate::check::check(dir, &crate::check::CheckOptions { quick: false }).unwrap();
    assert_eq!(report.errors, 0, "{report:?}");
    Store::open(dir, StoreOptions::default()).unwrap()
}

#[cfg(unix)]
fn inode(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).unwrap().ino()
}

/// Shared files and a rebuild give the same database; links share inodes, copies do
/// not.
#[test]
fn shared_files_match_a_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    let want = dump(&src);
    let mut seen = Vec::new();
    for mode in [CloneMode::Rebuild, CloneMode::Auto, CloneMode::Link] {
        let out = dir.path().join(mode.name());
        let o = CloneOptions {
            mode,
            ..Default::default()
        };
        let rep = src.clone_to(&out, &o).unwrap();
        assert_eq!((rep.quads, rep.source_quads), (7, 7));
        assert_eq!(rep.graphs, 5, "{mode:?}");
        assert!(rep.bytes > 0);
        match mode {
            CloneMode::Rebuild => {
                assert_eq!(rep.method, CloneMethod::Rebuild);
                assert_eq!(rep.rebuild_reason, Some("a rebuild was asked for"));
            }
            CloneMode::Auto => {
                assert!(
                    matches!(rep.method, CloneMethod::Copy | CloneMethod::Reflink),
                    "{:?}",
                    rep.method
                );
                assert_eq!(rep.rebuild_reason, None);
            }
            CloneMode::Link => assert_eq!(rep.method, CloneMethod::Link),
        }
        let c = checked(&out);
        assert_eq!(dump(&c), want, "{mode:?}");
        assert_eq!(gens(&out), ["gen-0001"]);
        assert_eq!(c.head_commit().seq, 0);
        assert_eq!(c.head_commit().quads, 7);
        assert_eq!(c.forked_from(), Some(rep.forked_from));
        assert_eq!(c.prefixes().get("ex").unwrap(), "http://ex.org/");
        #[cfg(unix)]
        {
            let a = src
                .root()
                .unwrap()
                .join(&src.snapshot().generation.name)
                .join("spo.dat");
            let b = out.join("gen-0001/spo.dat");
            assert_eq!(inode(&a) == inode(&b), mode == CloneMode::Link);
            // the clone's own files are never shared
            for f in ["meta.json", "commit.json", "wal.log"] {
                let a = a.with_file_name(f);
                assert_ne!(inode(&a), inode(&b.with_file_name(f)), "{f}");
            }
        }
        seen.push(c);
    }
    // the source's generation is not leased any more
    let h = src.history();
    assert!(h.generations.iter().all(|g| g.held_by == [Hold::Head]));
}

/// Writes, compactions and the removal of either side never reach the other.
#[test]
fn clones_by_link_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    let src = source(&root);
    let want = dump(&src);
    let out = dir.path().join("dst");
    let o = CloneOptions {
        mode: CloneMode::Link,
        ..Default::default()
    };
    src.clone_to(&out, &o).unwrap();
    let c = Store::open(&out, StoreOptions::default()).unwrap();
    upd(
        &c,
        "INSERT DATA { <urn:a> <urn:p> 1 } ; \
         DELETE DATA { GRAPH <http://ex.org/h> { <http://ex.org/t> <http://ex.org/r> 3 } }",
    );
    c.compact().unwrap();
    upd(&src, "INSERT DATA { <urn:b> <urn:p> 2 }");
    src.compact().unwrap();
    assert_eq!(objects(&src, "urn:a"), Vec::<String>::new());
    assert_eq!(objects(&c, "urn:b"), Vec::<String>::new());
    assert_eq!(objects(&c, "urn:a").len(), 1);
    assert_eq!(dump(&src).len(), want.len() + 1);
    assert_eq!(dump(&c).len(), want.len());
    // the source's directory goes away entirely: the clone opens and reads as before
    let cd = dump(&c);
    drop(src);
    std::fs::remove_dir_all(&root).unwrap();
    drop(c);
    let c = checked(&out);
    assert_eq!(dump(&c), cd);
}

/// A bulk commit switches the source's generation while the clone copies the old one:
/// the lease keeps it (shown as `clone:<label>`), the clone holds exactly the captured
/// state, and the old generation is collected once the clone is done.
#[test]
fn a_generation_switch_during_the_copy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    // every load is a bulk commit, which switches generations
    let src = Store::open(
        &root,
        StoreOptions {
            bulk_threshold: 2,
            ..Default::default()
        },
    )
    .unwrap();
    load(&src, TRIG);
    let want = dump(&src);
    let before = src.snapshot().generation.name.clone();
    let held = Arc::new(Mutex::new(Vec::new()));
    let h2 = held.clone();
    src.set_failpoint(
        "clone-captured",
        Some(Arc::new(move |s: &Store| {
            let nt: String = (0..20)
                .map(|i| format!("<urn:x{i}> <urn:p> {i} .\n"))
                .collect();
            load(s, &nt);
            let h = s.history();
            let g = h.generations.iter().find(|g| g.name == before).unwrap();
            *h2.lock() = g.held_by.iter().map(|x| x.to_string()).collect();
        })),
    );
    let out = dir.path().join("dst");
    let o = CloneOptions {
        label: "sandbox".into(),
        ..Default::default()
    };
    let rep = src.clone_to(&out, &o).unwrap();
    src.set_failpoint("clone-captured", None);
    assert_ne!(rep.method, CloneMethod::Rebuild);
    assert_eq!(*held.lock(), ["clone:sandbox"]);
    assert_eq!(dump(&checked(&out)), want);
    assert_eq!(dump(&src).len(), want.len() + 20);
    // the lease is gone, and with it the old generation
    assert_eq!(gens(&root).len(), 1);
    assert_ne!(gens(&root)[0], rep.generation);

    // a compaction (C13) during the copy, likewise
    let want = dump(&src);
    src.set_failpoint(
        "clone-captured",
        Some(Arc::new(|s: &Store| {
            upd(s, "INSERT DATA { <urn:late> <urn:p> 1 }");
            s.compact().unwrap();
        })),
    );
    let out = dir.path().join("dst2");
    let rep = src.clone_to(&out, &CloneOptions::default()).unwrap();
    src.set_failpoint("clone-captured", None);
    assert_ne!(rep.method, CloneMethod::Rebuild);
    assert_eq!(dump(&checked(&out)), want);
    assert_eq!(dump(&src).len(), want.len() + 1);
    assert_eq!(gens(&root).len(), 1);
    assert_ne!(gens(&root)[0], rep.generation);
}

/// Blank nodes made and deleted again since the base was built are never handed out
/// again by the clone.
#[test]
fn the_blank_node_counter_survives_shared_files() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    upd(&src, "INSERT DATA { <urn:n> <urn:p> [] }");
    let gone = objects(&src, "urn:n");
    upd(&src, "DELETE WHERE { <urn:n> <urn:p> ?o }");
    assert!(src.snapshot().delta.is_empty());
    let out = dir.path().join("dst");
    let rep = src.clone_to(&out, &CloneOptions::default()).unwrap();
    assert_ne!(rep.method, CloneMethod::Rebuild);
    let c = Store::open(&out, StoreOptions::default()).unwrap();
    upd(&c, "INSERT DATA { <urn:m> <urn:p> [] }");
    let new = objects(&c, "urn:m");
    assert_eq!(new.len(), 1);
    assert_ne!(new, gone);
}

/// A clone with changes in the delta, of a past state, or of some graphs is rebuilt,
/// and says why.
#[test]
fn rebuilds_say_why() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    let some = CloneOptions {
        graphs: Some(Graphs::Only(GraphRule::new(["http://ex.org/g"], &[]))),
        ..Default::default()
    };
    let rep = src.clone_to(&dir.path().join("a"), &some).unwrap();
    assert_eq!(rep.method, CloneMethod::Rebuild);
    assert_eq!(rep.rebuild_reason, Some("some graphs are left out"));
    // an excluded graph without quads keeps the fast path
    let none = CloneOptions {
        exclude_graphs: vec![NamedNode::new_unchecked("urn:nothing")],
        ..Default::default()
    };
    let rep = src.clone_to(&dir.path().join("b"), &none).unwrap();
    assert_ne!(rep.method, CloneMethod::Rebuild);
    upd(&src, "INSERT DATA { <urn:a> <urn:p> 1 }");
    let rep = src
        .clone_to(&dir.path().join("c"), &CloneOptions::default())
        .unwrap();
    assert_eq!(
        rep.rebuild_reason,
        Some("the source has changes since its last compaction")
    );
    let past = CloneOptions {
        at: Some(crate::history::At::Commit(1)),
        ..Default::default()
    };
    let rep = src.clone_to(&dir.path().join("d"), &past).unwrap();
    assert_eq!(rep.rebuild_reason, Some("a past state is cloned"));
    assert_eq!(rep.quads, 7);
}

/// Graph selections: the default graph, IRIs and patterns; patterns never match the
/// inferred graph, and blank-node graphs are never selected.
#[test]
fn partial_clones_copy_the_selected_graphs() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    let inferred = "urn:x-sparkles:inferred";
    let cases: [(&[&str], u64, u64); 5] = [
        (&["default"], 2, 1),
        (&["http://ex.org/g", "http://ex.org/h"], 3, 2),
        (&["http://ex.org/*"], 3, 2),
        (&["*"], 3, 2),
        (&["default", inferred], 3, 2),
    ];
    for (i, (names, quads, graphs)) in cases.into_iter().enumerate() {
        let out = dir.path().join(format!("c{i}"));
        let o = CloneOptions {
            graphs: Some(Graphs::Only(GraphRule::new(names, &[inferred]))),
            ..Default::default()
        };
        let rep = src.clone_to(&out, &o).unwrap();
        assert_eq!((rep.quads, rep.graphs), (quads, graphs), "{names:?}");
        let c = checked(&out);
        assert_eq!(c.snapshot().len(), quads);
        // every copied quad is the source's, with its blank-node labels
        let all = dump(&src);
        assert!(dump(&c).iter().all(|l| all.contains(l)), "{names:?}");
    }
    // exclusions apply on top of a selection
    let o = CloneOptions {
        graphs: Some(Graphs::Only(GraphRule::new(["*", "default"], &[]))),
        exclude_graphs: vec![NamedNode::new_unchecked("http://ex.org/h")],
        ..Default::default()
    };
    let rep = src.clone_to(&dir.path().join("x"), &o).unwrap();
    assert_eq!((rep.quads, rep.graphs), (4, 3));
}

/// In-memory clones of a persistent store (by shared files) and of an in-memory one
/// (rebuilt): the same data and blank nodes, independent of the source.
#[test]
fn in_memory_clones() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    let want = dump(&src);
    let (m, rep) = src
        .clone_to_memory(&CloneOptions::default(), StoreOptions::default())
        .unwrap();
    assert_ne!(rep.method, CloneMethod::Rebuild);
    assert!(m.root().is_none());
    assert_eq!(dump(&m), want);
    assert_eq!(m.forked_from(), Some(rep.forked_from));
    assert_eq!(m.dataset_id(), rep.dataset_id);
    assert_eq!(m.head_commit().seq, 0);
    assert_eq!(m.prefixes().get("ex").unwrap(), "http://ex.org/");
    upd(&m, "INSERT DATA { <urn:n> <urn:p> [] }");
    assert_eq!(objects(&src, "urn:n"), Vec::<String>::new());
    let label = objects(&m, "urn:n");
    assert!(
        want.iter()
            .flat_map(|l| l.split(' '))
            .all(|w| w != label[0]),
        "{label:?}"
    );
    // and once more from the in-memory clone, which is rebuilt
    let (m2, rep) = m
        .clone_to_memory(&CloneOptions::default(), StoreOptions::default())
        .unwrap();
    assert_eq!(rep.method, CloneMethod::Rebuild);
    assert_eq!(rep.rebuild_reason, Some("the source is in memory"));
    assert_eq!(dump(&m2), dump(&m));
    // an in-memory clone stays within its limit
    let small = StoreOptions {
        max_memory_bytes: Some(1024),
        ..Default::default()
    };
    let e = src
        .clone_to_memory(&CloneOptions::default(), small)
        .err()
        .unwrap();
    assert!(matches!(e, Error::StorageFull(_)), "{e}");
}

#[test]
fn a_cancelled_file_clone_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(&dir.path().join("src"));
    let out = dir.path().join("dst");
    let o = CloneOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(src.clone_to(&out, &o), Err(Error::Cancelled)));
    assert!(!out.exists());
    assert!(
        src.history()
            .generations
            .iter()
            .all(|g| g.held_by == [Hold::Head])
    );
}
