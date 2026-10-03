//! Cloning a store into a new, independent database from one snapshot.

use oxrdf::NamedNode;
use sparkles_core::commit::CommitKind;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{CloneOptions, Store, StoreOptions};
use std::collections::BTreeSet;

const TRIG: &str = r#"@prefix ex: <http://ex.org/> .
ex:s ex:p _:x . _:x ex:q "v"@en--ltr .
GRAPH ex:g { ex:s ex:r <<( _:x ex:p 1 )>> }
GRAPH _:gb { _:x ex:in _:gb }
"#;

fn dump(s: &Store) -> Vec<String> {
    let mut buf = Vec::new();
    s.dump_nquads(&mut buf).unwrap();
    let mut lines: Vec<String> = String::from_utf8(buf)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

fn select(s: &Store, q: &str) -> Vec<String> {
    let r = query(s.snapshot(), q, &QueryOptions::default()).unwrap();
    r.rows()
        .into_iter()
        .map(|row| row.into_iter().flatten().map(|t| t.to_string()).collect())
        .collect()
}

fn ask(s: &Store, q: &str) -> bool {
    query(s.snapshot(), q, &QueryOptions::default())
        .unwrap()
        .boolean
}

fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap();
}

fn source(dir: &std::path::Path) -> Store {
    let s = Store::open(&dir.join("src"), StoreOptions::default()).unwrap();
    s.load(&[Source::from_bytes(
        TRIG.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    // one quad in the delta
    upd(
        &s,
        "INSERT DATA { <http://ex.org/t> <http://ex.org/p> <http://ex.org/u> }",
    );
    assert!(!s.snapshot().delta.is_empty());
    s
}

#[test]
fn a_clone_has_the_same_quads_and_blank_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path());
    let dst_dir = dir.path().join("dst");
    let rep = src.clone_to(&dst_dir, &CloneOptions::default()).unwrap();
    assert_eq!(rep.quads, 5);
    assert_eq!(rep.source_quads, 5);
    assert_eq!(rep.graphs, 3);
    assert_eq!(rep.forked_from.id, src.dataset_id());
    assert_eq!(rep.forked_from.seq, src.head_commit().seq);

    let dst = Store::open(&dst_dir, StoreOptions::default()).unwrap();
    // identical N-Quads: same `_:b<hex>` labels, so also the same canonical form
    assert_eq!(dump(&src), dump(&dst));
    let count = "SELECT (COUNT(*) AS ?n) { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }";
    assert_eq!(select(&src, count), select(&dst, count));
    assert!(ask(&dst, "ASK { GRAPH ?g { ?s ?p <<( ?x ?q ?y )>> } }"));
    let snap = dst.snapshot();
    assert!(snap.delta.is_empty());
    assert_eq!(snap.generation.name, "gen-0001");
    let gens: Vec<String> = std::fs::read_dir(&dst_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("gen-"))
        .collect();
    assert_eq!(gens, ["gen-0001"]);
    assert!(dst.prefixes().contains_key("ex"));

    // a new lineage, forked from the source's snapshot
    assert_ne!(dst.dataset_id(), src.dataset_id());
    assert_eq!(dst.dataset_id(), rep.dataset_id);
    let head = dst.head_commit();
    assert_eq!(
        (head.seq, head.kind, head.quads),
        (0, CommitKind::Create, 5)
    );
    assert_eq!(dst.forked_from(), Some(rep.forked_from));
    assert_eq!(src.forked_from(), None);

    // blank-node labels are stable; new blank nodes never collide with copied ones
    let o = "SELECT ?o { <http://ex.org/s> <http://ex.org/p> ?o }";
    let label = select(&src, o);
    assert!(label[0].starts_with("_:b"), "{label:?}");
    assert_eq!(select(&dst, o), label);
    upd(
        &dst,
        "INSERT DATA { <http://ex.org/n> <http://ex.org/p> [] }",
    );
    let src_labels: BTreeSet<String> = dump(&src)
        .iter()
        .flat_map(|l| {
            l.split(' ')
                .filter(|w| w.starts_with("_:"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();
    let new = select(&dst, "SELECT ?o { <http://ex.org/n> <http://ex.org/p> ?o }");
    assert!(!src_labels.contains(&new[0]), "{new:?} in {src_labels:?}");
    assert_eq!(dst.head_commit().seq, 1);

    // the clone is independent of the source
    assert_eq!(src.snapshot().len(), 5);
    drop(dst);
    let dst = Store::open(&dst_dir, StoreOptions::default()).unwrap();
    assert_eq!(dst.snapshot().len(), 6);
    assert_eq!(dst.forked_from(), Some(rep.forked_from));
}

#[test]
fn graphs_can_be_left_out_and_memory_stores_cloned() {
    let dir = tempfile::tempdir().unwrap();
    let src = Store::in_memory(StoreOptions::default());
    src.load(&[Source::from_bytes(
        TRIG.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let opts = CloneOptions {
        exclude_graphs: vec![NamedNode::new_unchecked("http://ex.org/g")],
        ..Default::default()
    };
    let rep = src.clone_to(&dir.path().join("dst"), &opts).unwrap();
    assert_eq!((rep.source_quads, rep.quads, rep.graphs), (4, 3, 2));
    let dst = Store::open(&dir.path().join("dst"), StoreOptions::default()).unwrap();
    assert!(!ask(&dst, "ASK { GRAPH <http://ex.org/g> { ?s ?p ?o } }"));
    assert_eq!(dst.snapshot().len(), 3);
}

#[test]
fn the_destination_must_be_absent_or_empty() {
    let dir = tempfile::tempdir().unwrap();
    let src = source(dir.path());
    // an empty directory is fine
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    src.clone_to(&empty, &CloneOptions::default()).unwrap();
    // a non-empty one is refused and left alone
    let err = src.clone_to(&empty, &CloneOptions::default()).unwrap_err();
    assert!(err.to_string().contains("not empty"), "{err}");
    assert!(empty.join("CURRENT").exists());
    // a cancelled clone leaves nothing behind
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let big = Store::in_memory(StoreOptions::default());
    let mut t = String::new();
    for i in 0..70_000 {
        t.push_str(&format!("<urn:s{i}> <urn:p> \"{i}\" .\n"));
    }
    big.load(&[Source::from_bytes(
        t.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let gone = dir.path().join("gone");
    let r = big.clone_to(
        &gone,
        &CloneOptions {
            cancel: Some(cancel),
            ..Default::default()
        },
    );
    assert!(r.is_err());
    assert!(!gone.exists());
}

#[cfg(feature = "text")]
#[test]
fn clones_keep_full_text_search() {
    use sparkles_core::text::{PredicateSet, TextConfig};
    let tmp = tempfile::tempdir().unwrap();
    let src = Store::open(&tmp.path().join("src"), StoreOptions::default()).unwrap();
    src.load(&[Source::from_bytes(
        b"<urn:a> <urn:label> \"quick brown fox\" .".to_vec(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let cfg = TextConfig {
        predicates: PredicateSet::Only(vec!["urn:label".into()]),
        ..TextConfig::default()
    };
    src.enable_text(cfg).unwrap();
    let dst = tmp.path().join("dst");
    src.clone_to(&dst, &CloneOptions::default()).unwrap();
    let c = Store::open(&dst, StoreOptions::default()).unwrap();
    assert!(c.text_enabled());
    let q = "SELECT ?s { ?s <http://jena.apache.org/text#query> \"fox\" }";
    assert_eq!(select(&c, q), ["<urn:a>"]);
    let status = c.text_status().unwrap();
    assert_eq!(
        serde_json::to_value(status).unwrap()["config"]["predicates"],
        serde_json::to_value(src.text_status().unwrap()).unwrap()["config"]["predicates"]
    );
}
