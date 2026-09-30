//! Full-text search (`text:query`): indexing, retrieval semantics, maintenance on
//! updates, compaction and bulk loads, recovery.
#![cfg(feature = "text")]

use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
use sparkles::text::{PredicateSet, TextConfig};

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:b1 a ex:Book ; rdfs:label "The Quick Brown Fox"@en .
ex:b2 a ex:Book ; rdfs:label "Brown Bears" ; rdfs:comment "A field guide to brown bears" .
ex:b3 a ex:Film ; rdfs:label "Le renard brun"@fr .
ex:p1 rdfs:label "Foxglove" ; ex:code "fox"^^ex:Code .
ex:g1 { ex:b4 rdfs:label "Fox in Socks" }
"#;

const P: &str = "PREFIX ex: <http://example.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX text: <http://jena.apache.org/text#> ";

fn load(s: &Store) {
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
}

fn mem() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    s.enable_text(TextConfig::default()).unwrap();
    s
}

fn rows(s: &Store, q: &str) -> Vec<String> {
    let r = query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default())
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    r.rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| {
                    t.map_or("-".into(), |t| match t {
                        oxrdf::Term::NamedNode(n) => {
                            n.as_str().rsplit('/').next().unwrap().to_string()
                        }
                        oxrdf::Term::Literal(l) => l.value().to_string(),
                        t => t.to_string(),
                    })
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn err(s: &Store, q: &str) -> String {
    query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default())
        .err()
        .unwrap_or_else(|| panic!("{q}: no error"))
        .to_string()
}

#[test]
fn retrieval_semantics() {
    let s = mem();
    // A1: default graph only; tokens, not substrings; typed literals not indexed
    assert_eq!(rows(&s, "SELECT ?s { ?s text:query \"fox\" }"), ["b1"]);
    // A2: GRAPH ?g binds the hit's graph
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?lit ?g { GRAPH ?g { (?s ?sc ?lit) text:query \"fox\" } }"
        ),
        ["b4 Fox in Socks g1"]
    );
    // A3: predicate restriction, ranking
    assert_eq!(
        rows(
            &s,
            "SELECT ?s { (?s ?sc) text:query (rdfs:label \"brown\") } ORDER BY DESC(?sc)"
        ),
        ["b2", "b1"]
    );
    // A4: one row per matching quad
    assert_eq!(
        sorted(rows(
            &s,
            "SELECT ?s ?lit { (?s ?sc ?lit) text:query \"brown\" }"
        )),
        [
            "b1 The Quick Brown Fox",
            "b2 A field guide to brown bears",
            "b2 Brown Bears"
        ]
    );
    // A5: phrases
    assert_eq!(
        rows(&s, "SELECT ?s { ?s text:query \"\\\"brown bears\\\"\" }").len(),
        2
    );
    assert_eq!(
        rows(
            &s,
            "SELECT ?s { ?s text:query (rdfs:label \"\\\"brown bears\\\"\") }"
        ),
        ["b2"]
    );
    // A6: language
    assert_eq!(
        rows(
            &s,
            "SELECT ?s { ?s text:query (rdfs:label \"renard\" \"lang:fr\") }"
        ),
        ["b3"]
    );
    assert!(rows(&s, "SELECT ?s { ?s text:query \"renard\"@en }").is_empty());
    // A7: joins
    assert_eq!(
        sorted(rows(
            &s,
            "SELECT DISTINCT ?s { ?s text:query \"brown\" . ?s a ex:Book }"
        )),
        ["b1", "b2"]
    );
    // A8: limit is the top n within the scope, before joins
    assert_eq!(
        rows(&s, "SELECT ?s { (?s ?sc) text:query (\"brown\" 1) }"),
        ["b2"]
    );
    // A9: OPTIONAL
    let r = sorted(rows(
        &s,
        "SELECT ?s ?sc { ?s a ex:Book OPTIONAL { (?s ?sc) text:query \"fox\" } }",
    ));
    assert_eq!(r.len(), 2);
    assert!(r[0].starts_with("b1 ") && r[0] != "b1 -");
    assert_eq!(r[1], "b2 -");
    // A13: a constant subject
    assert_eq!(
        rows(&s, "SELECT (COUNT(*) AS ?n) { ex:b2 text:query \"bears\" }"),
        ["2"]
    );
    // score is xsd:float, the graph slot names the default graph
    let r = query(
        s.snapshot(),
        &format!("{P}SELECT ?sc ?g {{ (ex:b1 ?sc ?lit ?g) text:query \"fox\" }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let rows_ = r.rows();
    assert_eq!(rows_.len(), 1, "{:?} {:?}", r.vars, rows_);
    let row = &rows_[0];
    let oxrdf::Term::Literal(sc) = row[0].as_ref().unwrap() else {
        panic!()
    };
    assert_eq!(
        sc.datatype().as_str(),
        "http://www.w3.org/2001/XMLSchema#float"
    );
    assert_eq!(
        row[1].as_ref().unwrap().to_string(),
        "<urn:x-arq:DefaultGraph>"
    );
    // EXPLAIN shows the operator (a query not run before: a result-cache hit at the root
    // would hide the children)
    let r = query(
        s.snapshot(),
        &format!("{P}SELECT ?s {{ ?s text:query \"quick\" }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    fn has(p: &sparkles::sparql::PlanInfo) -> bool {
        p.operator == "TextSearch" || p.children.iter().any(has)
    }
    assert!(has(&r.plan));
}

#[test]
fn errors() {
    let s = mem();
    for (q, needle) in [
        ("SELECT ?s { ?s text:query 42 }", "string literal"),
        (
            "SELECT ?s { (?s 1) text:query \"x\" }",
            "must be a variable",
        ),
        ("SELECT ?s { ?s text:query \"(fox\" }", "text:query"),
    ] {
        let e = err(&s, q);
        assert!(e.contains(needle), "{q}: {e}");
    }
    // not enabled
    let plain = Store::in_memory(StoreOptions::default());
    load(&plain);
    assert!(err(&plain, "SELECT ?s { ?s text:query \"fox\" }").contains("no full-text index"));
    // an unindexed predicate
    let only = Store::in_memory(StoreOptions::default());
    load(&only);
    only.enable_text(TextConfig {
        predicates: PredicateSet::Only(vec!["http://www.w3.org/2000/01/rdf-schema#label".into()]),
        ..Default::default()
    })
    .unwrap();
    assert!(
        err(&only, "SELECT ?s { ?s text:query (rdfs:comment \"x\") }").contains("not text-indexed")
    );
    assert_eq!(
        sorted(rows(
            &only,
            "SELECT ?s ?lit { (?s ?sc ?lit) text:query \"brown\" }"
        )),
        ["b1 The Quick Brown Fox", "b2 Brown Bears"]
    );
}

#[test]
fn updates_are_visible_to_the_next_query() {
    let s = mem();
    sparkles::sparql::update::update(
        &s,
        &format!("{P}INSERT DATA {{ ex:b5 rdfs:label \"Brown Owl\" }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let r = rows(
        &s,
        "SELECT ?s { (?s ?sc) text:query (rdfs:label \"brown\") } ORDER BY DESC(?sc)",
    );
    assert_eq!(sorted(r.clone()), ["b1", "b2", "b5"]);
    assert_eq!(s.text_status().unwrap().docs, 7);
    sparkles::sparql::update::update(
        &s,
        &format!("{P}DELETE DATA {{ ex:b1 rdfs:label \"The Quick Brown Fox\"@en }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(
        sorted(rows(&s, "SELECT ?s { ?s text:query \"brown\" }")),
        ["b2", "b2", "b5"]
    );
    let st = s.text_status().unwrap();
    assert_eq!(
        (st.docs, st.state.as_str(), st.seq),
        (6, "ready", st.store_seq)
    );
    // an update touching no literal keeps the index as is
    sparkles::sparql::update::update(
        &s,
        &format!("{P}INSERT DATA {{ ex:b5 a ex:Book }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(s.text_status().unwrap().state, "ready");
    // a snapshot taken before an update keeps its own view
    let old = s.snapshot();
    sparkles::sparql::update::update(
        &s,
        &format!("{P}INSERT DATA {{ ex:b6 rdfs:label \"brown\" }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let r = query(
        old,
        &format!("{P}SELECT ?s {{ ?s text:query \"brown\" }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(r.len(), 3);
}

#[test]
fn persistence_compaction_bulk_and_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let opts = || StoreOptions {
        bulk_threshold: 20,
        ..Default::default()
    };
    {
        let s = Store::open(&root, opts()).unwrap();
        load(&s);
        s.enable_text(TextConfig::default()).unwrap();
        let epoch = s.text_status().unwrap().epoch;
        // compaction touches nothing
        s.compact().unwrap();
        assert_eq!(rows(&s, "SELECT ?s { ?s text:query \"fox\" }"), ["b1"]);
        assert_eq!(s.text_status().unwrap().epoch, epoch);
        // a bulk load (above the threshold) rebuilds from the new data
        let mut ttl = String::new();
        for i in 0..50 {
            ttl.push_str(&format!("<http://example.org/n{i}> <http://www.w3.org/2000/01/rdf-schema#label> \"number {i} walrus\" .\n"));
        }
        s.load(&[Source::from_bytes(
            ttl.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let st = s.text_status().unwrap();
        assert_eq!((st.state.as_str(), st.seq), ("ready", st.store_seq));
        assert_eq!(
            rows(&s, "SELECT (COUNT(*) AS ?n) { ?s text:query \"walrus\" }"),
            ["50"]
        );
    }
    // reopen: the index is reused as it is
    {
        let s = Store::open(&root, opts()).unwrap();
        assert!(s.text_enabled());
        assert_eq!(rows(&s, "SELECT ?s { ?s text:query \"fox\" }"), ["b1"]);
        // A12: a failed text update leaves the index stale (queries refuse), and the
        // RDF write still succeeds
        s.fail_next_text_commit();
        sparkles::sparql::update::update(
            &s,
            &format!("{P}INSERT DATA {{ ex:b7 rdfs:label \"Brown Owl\" }}"),
            &QueryOptions::default(),
        )
        .unwrap();
        assert_eq!(s.text_status().unwrap().state, "stale");
        let e = err(&s, "SELECT ?s { ?s text:query \"fox\" }");
        assert!(e.contains("stale"), "{e}");
    }
    // reopen: the index is behind, and catches up from the WAL instead of a rebuild
    {
        let s = Store::open(&root, opts()).unwrap();
        let st = s.text_status().unwrap();
        assert_eq!(st.state, "ready");
        assert!(st.last_rebuild.is_none());
        assert!(rows(&s, "SELECT ?s { ?s text:query \"owl\" }").contains(&"b7".to_string()));
    }
    // a deleted index is rebuilt too
    std::fs::remove_dir_all(root.join("text")).unwrap();
    let s = Store::open(&root, opts()).unwrap();
    assert_eq!(rows(&s, "SELECT ?s { ?s text:query \"fox\" }"), ["b1"]);
    s.disable_text().unwrap();
    assert!(!root.join("text").exists() && !root.join("text.json").exists());
}

#[test]
fn merged_default_graphs_merge_hits() {
    let s = Store::in_memory(StoreOptions {
        union_default_graph: true,
        ..Default::default()
    });
    s.load(&[Source::from_bytes(
        br#"<http://example.org/g1> { <http://example.org/b2> <http://www.w3.org/2000/01/rdf-schema#label> "Brown Bears" }
            <http://example.org/g2> { <http://example.org/b2> <http://www.w3.org/2000/01/rdf-schema#label> "Brown Bears" }"#
            .to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s.enable_text(TextConfig::default()).unwrap();
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?lit { (?s ?sc ?lit) text:query (rdfs:label \"bears\") }"
        )
        .len(),
        1
    );
    assert_eq!(
        rows(
            &s,
            "SELECT ?s ?lit ?g { (?s ?sc ?lit ?g) text:query (rdfs:label \"bears\") }"
        )
        .len(),
        2
    );
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else if std::fs::copy(e.path(), &target).is_err() {
            // removed meanwhile (a merge's garbage collection)
        }
    }
}

fn insert(s: &Store, triples: &str) {
    sparkles::sparql::update::update(
        s,
        &format!("{P}INSERT DATA {{ {triples} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
}

#[test]
fn commits_skip_fsync_until_a_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let marker = root.join("text.dirty");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        load(&s);
        s.enable_text(TextConfig::default()).unwrap();
        // a fresh build is durable
        assert!(!marker.exists());
        // an update writes the index without fsync, so it is marked
        insert(&s, "ex:b8 rdfs:label \"Crimson Heron\"");
        assert!(marker.exists());
        // compaction checkpoints it
        s.compact().unwrap();
        assert!(!marker.exists());
        insert(&s, "ex:b9 rdfs:label \"Violet Heron\"");
        assert!(marker.exists());
    }
    // so does closing the store
    assert!(!marker.exists());
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let st = s.text_status().unwrap();
    assert_eq!(
        (st.state.as_str(), st.last_rebuild.is_none()),
        ("ready", true)
    );
    let mut got = rows(&s, "SELECT ?s { ?s text:query \"heron\" }");
    got.sort();
    assert_eq!(got, ["b8", "b9"]);
}

#[test]
fn crash_images_are_verified_caught_up_or_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let img = |n: &str| dir.path().join(n);
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        load(&s);
        s.enable_text(TextConfig::default()).unwrap();
        insert(&s, "ex:b8 rdfs:label \"Crimson Heron\"");
        // a crash image: unsynced index files, marker present, index at the head
        copy_dir(&root, &img("current"));
        // a commit the index misses: the image's index is behind the WAL
        s.fail_next_text_commit();
        insert(&s, "ex:b9 rdfs:label \"Violet Heron\"");
        copy_dir(&root, &img("behind"));
        copy_dir(&root, &img("damaged"));
    }
    // damage the largest file of the damaged image's index
    let victim = std::fs::read_dir(img("damaged").join("text"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e != "json"))
        .max_by_key(|p| std::fs::metadata(p).unwrap().len())
        .unwrap();
    let mut bytes = std::fs::read(&victim).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&victim, bytes).unwrap();

    for (name, want, rebuilt) in [
        ("current", vec!["b8"], false),
        ("behind", vec!["b8", "b9"], false),
        ("damaged", vec!["b8", "b9"], true),
    ] {
        let root = img(name);
        assert!(root.join("text.dirty").exists(), "{name}");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        let st = s.text_status().unwrap();
        assert_eq!(st.state, "ready", "{name}");
        assert_eq!(st.last_rebuild.is_some(), rebuilt, "{name}");
        let mut got = rows(&s, "SELECT ?s { ?s text:query \"heron\" }");
        got.sort();
        assert_eq!(got, want, "{name}");
        // durable again once closed (merges started by the catch-up may still be running
        // at the checkpoint during open, which then keeps the marker)
        drop(s);
        assert!(!root.join("text.dirty").exists(), "{name}");
    }
}
