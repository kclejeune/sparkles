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
    rows_at(s.snapshot(), q)
}

fn rows_at(snap: std::sync::Arc<sparkles::store::Snapshot>, q: &str) -> Vec<String> {
    let r = query(snap, &format!("{P}{q}"), &QueryOptions::default())
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
        s.set_text_ticks(false);
        // a fresh build is durable
        assert!(!marker.exists());
        // an update only stages its documents (indexing may write files already); the
        // search that needs them commits the index, without fsync, so it is marked
        insert(&s, "ex:b8 rdfs:label \"Crimson Heron\"");
        assert_eq!(rows(&s, "SELECT ?s { ?s text:query \"crimson\" }"), ["b8"]);
        assert!(marker.exists());
        // compaction checkpoints it
        s.compact().unwrap();
        assert!(!marker.exists());
        insert(&s, "ex:b9 rdfs:label \"Violet Heron\"");
        assert_eq!(rows(&s, "SELECT ?s { ?s text:query \"violet\" }"), ["b9"]);
        assert!(marker.exists());
        // a staged update is committed by closing too
        insert(&s, "ex:b10 rdfs:label \"Grey Heron\"");
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
    assert_eq!(got, ["b10", "b8", "b9"]);
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
        s.set_text_ticks(false);
        insert(&s, "ex:b8 rdfs:label \"Crimson Heron\"");
        // a crash image: unsynced index files, marker present, index at the head
        assert_eq!(rows(&s, "SELECT ?s { ?s text:query \"heron\" }"), ["b8"]);
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

/// The forms of Lucene's query syntax that jena-text accepts.
#[test]
fn lucene_query_syntax() {
    let s = mem();
    let hits = |q: &str| {
        sorted(rows(
            &s,
            &format!("SELECT ?s ?lit {{ (?s ?sc ?lit) text:query \"{q}\" }}"),
        ))
    };
    let brown = [
        "b1 The Quick Brown Fox",
        "b2 A field guide to brown bears",
        "b2 Brown Bears",
    ];
    // a prefix of one word, alone and in boolean queries
    assert_eq!(hits("fox*"), ["b1 The Quick Brown Fox", "p1 Foxglove"]);
    assert_eq!(hits("FOX*"), hits("fox*"), "prefixes are lowercased");
    assert_eq!(hits("+brown +bea*"), &brown[1..]);
    assert_eq!(hits("+brown -bea*"), &brown[..1]);
    assert_eq!(hits("\\\"fox\\\"*"), hits("fox*"));
    // a phrase prefix (the last word)
    assert_eq!(hits("\\\"quick bro\\\"*"), &brown[..1]);
    // wildcards, also leading ones
    assert_eq!(hits("f?x"), ["b1 The Quick Brown Fox"]);
    assert_eq!(hits("*glove"), ["p1 Foxglove"]);
    assert_eq!(hits("b*s"), &brown[1..]);
    assert_eq!(
        hits("f\\\\*x"),
        Vec::<String>::new(),
        "an escaped * is literal"
    );
    // fuzzy terms
    // "brwn" is one edit away from both "brown" and "brun"
    let brun = [&brown[..], &["b3 Le renard brun"]].concat();
    assert_eq!(hits("brwn~1"), brun);
    assert_eq!(hits("brwn~"), brun);
    assert_eq!(hits("browm~1"), brown);
    assert!(hits("brwn").is_empty());
    assert_eq!(
        hits("bxown~0.6"),
        brown,
        "a similarity of 0.6 allows 2 edits of 5"
    );
    // boolean operators and grouping
    assert_eq!(hits("brown AND fox"), &brown[..1]);
    assert_eq!(hits("brown && NOT fox"), &brown[1..]);
    assert_eq!(hits("brown -fox"), &brown[1..]);
    assert_eq!(hits("brown !fox"), &brown[1..]);
    assert_eq!(hits("(fox OR bears) AND brown"), brown);
    assert_eq!(hits("fox OR bears AND guide"), &brown[1..2]);
    assert_eq!(hits("+(fox socks) -quick"), Vec::<String>::new());
    // phrases with a slop, in either order (as in Lucene)
    assert_eq!(hits("\\\"quick fox\\\"~1"), &brown[..1]);
    assert!(hits("\\\"quick fox\\\"").is_empty());
    assert!(hits("\\\"fox quick\\\"~2").is_empty());
    assert_eq!(hits("\\\"fox quick\\\"~3"), &brown[..1]);
    // regular expressions, ranges, boosts, every document
    assert_eq!(
        hits("/fox(glove)?/"),
        ["b1 The Quick Brown Fox", "p1 Foxglove"]
    );
    assert_eq!(hits("[foxa TO foxz]"), ["p1 Foxglove"]);
    assert_eq!(hits("{fox TO foxglove}"), Vec::<String>::new());
    assert_eq!(hits("fox^2 OR bears"), hits("fox OR bears"));
    assert_eq!(hits("*").len(), 5, "the default graph's literals");
    // a word the analyzer splits is an OR of its parts
    assert_eq!(hits("quick-bears"), hits("quick OR bears"));
    // forms that are refused instead of finding nothing
    for (q, needle) in [
        ("label:fox", "field names"),
        ("*:*", "* alone"),
        ("-fox", "not excluded"),
        ("NOT fox", "not excluded"),
        ("...", "no word"),
        ("", "empty query"),
        ("(fox", "missing ')'"),
        ("fox AND", "word should follow"),
        ("/a@b/", "not supported"),
        ("\\\"a b a\\\"~2", "repeat a word"),
        ("fox~1.5", "fractional"),
    ] {
        let e = err(&s, &format!("SELECT ?s {{ ?s text:query \"{q}\" }}"));
        assert!(e.contains("text:query") && e.contains(needle), "{q}: {e}");
    }
}

/// Jena's `highlight:` option, with the examples of jena-text's documentation.
#[test]
fn highlighting() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        br#"
@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:a rdfs:label "the quick brown fox jumped over the lazy baboon"@en .
ex:b rdfs:comment "one two three four five six seven eight nine ten eleven twelve fox thirteen fourteen fifteen sixteen fox seventeen" .
ex:c rdfs:comment "Foxes are not foxglove" .
"#
        .to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.enable_text(TextConfig::default()).unwrap();
    let hl = |q: &str, opts: &str| {
        let r = query(
            s.snapshot(),
            &format!(
                "{P}SELECT ?lit {{ (?s ?sc ?lit) text:query (\"{q}\" \"highlight:{opts}\") }} ORDER BY ?lit"
            ),
            &QueryOptions::default(),
        )
        .unwrap_or_else(|e| panic!("{q} {opts}: {e}"));
        r.rows()
            .into_iter()
            .map(|row| match row[0].as_ref().unwrap() {
                oxrdf::Term::Literal(l) => {
                    format!(
                        "{}{}",
                        l.value(),
                        l.language().map(|t| format!("@{t}")).unwrap_or_default()
                    )
                }
                t => panic!("{t}"),
            })
            .collect::<Vec<_>>()
    };
    // the documented defaults: arrows, and a phrase marked as one
    assert_eq!(
        hl("brown fox", ""),
        [
            "one two three four five six seven eight nine ten eleven twelve ↦fox↤ thirteen fourteen fifteen sixteen ↦fox↤ seventeen",
            "the quick ↦brown fox↤ jumped over the lazy baboon@en",
        ]
    );
    assert_eq!(
        hl("+brown +fox", "jh:n"),
        ["the quick ↦brown↤ ↦fox↤ jumped over the lazy baboon@en"]
    );
    assert_eq!(
        hl("+brown +fox", "s:<em class='hiLite'> | e:</em>"),
        ["the quick <em class='hiLite'>brown fox</em> jumped over the lazy baboon@en"]
    );
    // fragments of about z: characters (each starts with the space before its first
    // word, as in Lucene), the best m: ones first, joined by f:
    assert_eq!(
        hl("+thirteen +fox", "z:20"),
        [" twelve ↦fox thirteen↤∣ sixteen ↦fox↤ seventeen"]
    );
    assert_eq!(
        hl("+thirteen +fox", "z:20 | m:2 | f: … "),
        [" twelve ↦fox thirteen↤ … sixteen ↦fox↤ seventeen"]
    );
    assert_eq!(
        hl("+thirteen +fox", "z:20 | m:1"),
        [" twelve ↦fox thirteen↤"]
    );
    // adjacent fragments are merged unless jf:n
    assert_eq!(
        hl("+fourteen +fox", "z:20"),
        [" twelve ↦fox↤ thirteen ↦fourteen↤ fifteen sixteen ↦fox↤ seventeen"]
    );
    assert_eq!(
        hl("+fourteen +fox", "z:20 | jf:n"),
        [" twelve ↦fox↤ thirteen∣ ↦fourteen↤ fifteen∣ sixteen ↦fox↤ seventeen"]
    );
    // prefixes, wildcards and fuzzy words are marked; excluded words are not
    assert_eq!(
        hl("fox*", ""),
        [
            "one two three four five six seven eight nine ten eleven twelve ↦fox↤ thirteen fourteen fifteen sixteen ↦fox↤ seventeen",
            "↦Foxes↤ are not ↦foxglove↤",
            "the quick brown ↦fox↤ jumped over the lazy baboon@en",
        ]
    );
    assert_eq!(
        hl("foxs~1 -twelve", ""),
        [
            "↦Foxes↤ are not foxglove",
            "the quick brown ↦fox↤ jumped over the lazy baboon@en",
        ]
    );
    assert_eq!(
        hl("f?x -twelve", "s:[|e:]"),
        ["the quick brown [fox] jumped over the lazy baboon@en"]
    );
}

/// An explicit limit above `maxHits` is no limit: more hits than `maxHits` are an error,
/// as without a limit, instead of the first `maxHits` + 1.
#[test]
fn limits_above_max_hits_are_refused_like_no_limit() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    s.enable_text(TextConfig {
        max_hits: 2,
        ..Default::default()
    })
    .unwrap();
    let count = |limit: &str| {
        query(
            s.snapshot(),
            &format!(
                "{P}SELECT (COUNT(*) AS ?n) {{ (?s ?sc ?lit) text:query (\"brown\"{limit}) }}"
            ),
            &QueryOptions::default(),
        )
        .map(|r| r.rows()[0][0].as_ref().unwrap().to_string())
    };
    for limit in ["", " 3", " 1000", " 100000000"] {
        let e = count(limit).expect_err(limit);
        assert!(
            matches!(&e, sparkles::Error::BudgetExceeded(b) if b.limit == 2 && b.requested == 3),
            "limit{limit}: {e}"
        );
    }
    assert!(count(" 2").unwrap().starts_with("\"2\""));
    assert!(count(" 1").unwrap().starts_with("\"1\""));
    // within maxHits every hit is returned, whatever the limit
    assert!(
        rows(&s, "SELECT ?s { ?s text:query (\"fox\" 1000) }").len() == 1,
        "one hit"
    );
}

/// A score or literal the query does not use is not produced, so the search reads no
/// literal; one used anywhere else still is.
#[test]
fn unused_outputs_are_left_out() {
    let s = mem();
    let plan = |q: &str| {
        let r = query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default()).unwrap();
        fn find(p: &sparkles::sparql::PlanInfo) -> Option<String> {
            if p.operator == "TextSearch" {
                return Some(p.description.clone());
            }
            p.children.iter().find_map(find)
        }
        (r.rows().len(), find(&r.plan).unwrap())
    };
    let (n, d) = plan("SELECT (COUNT(*) AS ?n) { (?s ?sc ?lit) text:query \"brown\" }");
    assert_eq!(n, 1);
    assert!(d.starts_with("?s ←"), "{d}");
    assert_eq!(
        rows(
            &s,
            "SELECT (COUNT(*) AS ?n) { (?s ?sc ?lit) text:query \"brown\" }"
        ),
        ["3"]
    );
    let (_, d) = plan("SELECT ?s { (?s ?sc ?lit) text:query \"brown\" } ORDER BY ?lit");
    assert!(d.contains("?lit") && !d.contains("?sc"), "{d}");
    let (_, d) = plan("SELECT ?s { (?s ?sc ?lit) text:query \"brown\" FILTER(?sc > 0) }");
    assert!(d.contains("?sc") && !d.contains("?lit"), "{d}");
    let (_, d) = plan("SELECT * { (?s ?sc ?lit) text:query \"brown\" }");
    assert!(d.contains("?sc") && d.contains("?lit"), "{d}");
    // COUNT(DISTINCT *) depends on every variable
    assert_eq!(
        rows(
            &s,
            "SELECT (COUNT(DISTINCT *) AS ?n) { (?s ?sc ?lit) text:query \"brown\" }"
        ),
        ["3"]
    );
}

#[test]
fn searches_count_toward_the_memory_budget() {
    let s = mem();
    let opts = QueryOptions {
        max_memory_bytes: Some(8),
        ..Default::default()
    };
    let Err(e) = query(
        s.snapshot(),
        &format!("{P}SELECT ?s {{ ?s text:query \"fox\" }}"),
        &opts,
    ) else {
        panic!("over budget");
    };
    assert!(
        matches!(e, sparkles::Error::BudgetExceeded(b) if b.kind == sparkles::BudgetKind::Memory),
        "{e}"
    );
}

fn delete(s: &Store, triples: &str) {
    sparkles::sparql::update::update(
        s,
        &format!("{P}DELETE DATA {{ {triples} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
}

#[test]
fn snapshots_search_their_own_state_while_documents_are_staged() {
    let s = mem();
    s.set_text_ticks(false);
    let brown = "SELECT ?s { ?s text:query \"brown\" }";
    insert(&s, "ex:b5 rdfs:label \"Brown Owl\"");
    let old = s.snapshot();
    // the same batch: a removal of a document the old snapshot has, one of a document
    // staged in the batch, and an addition
    delete(&s, "ex:b1 rdfs:label \"The Quick Brown Fox\"@en");
    delete(&s, "ex:b5 rdfs:label \"Brown Owl\"");
    insert(&s, "ex:b6 rdfs:label \"brown\"");
    let mid = s.snapshot();
    // the old snapshot seals the batch, and still finds exactly its own documents
    assert_eq!(
        sorted(rows_at(old.clone(), brown)),
        ["b1", "b2", "b2", "b5"]
    );
    // hits filtered out are made up for (b6 scores best)
    assert_eq!(
        rows_at(old.clone(), "SELECT ?s { ?s text:query (\"brown\" 2) }").len(),
        2
    );
    // a removed quad added back: one document
    insert(&s, "ex:b1 rdfs:label \"The Quick Brown Fox\"@en");
    assert_eq!(sorted(rows_at(mid, brown)), ["b2", "b2", "b6"]);
    assert_eq!(sorted(rows(&s, brown)), ["b1", "b2", "b2", "b6"]);
    assert_eq!(sorted(rows_at(old, brown)), ["b1", "b2", "b2", "b5"]);
    // an addition removed again before any search leaves nothing behind
    insert(&s, "ex:b7 rdfs:label \"Brown Heron\"");
    delete(&s, "ex:b7 rdfs:label \"Brown Heron\"");
    assert_eq!(sorted(rows(&s, brown)), ["b1", "b2", "b2", "b6"]);
    let st = s.text_status().unwrap();
    assert_eq!((st.docs, st.state.as_str()), (7, "ready"));
}

struct RejectAll;

impl sparkles::guard::CommitGuard for RejectAll {
    fn check(
        &self,
        _: &sparkles::guard::Candidate<'_>,
    ) -> sparkles::Result<sparkles::guard::ValidationSummary> {
        Ok(sparkles::guard::ValidationSummary::empty(
            sparkles::guard::GuardStatus::Rejected,
            sparkles::guard::GuardMode::Reject,
            sparkles::guard::Severity::Violation,
        ))
    }
    fn describe(&self) -> String {
        "reject all".into()
    }
}

#[test]
fn rejected_writes_leave_staged_documents_alone() {
    let s = mem();
    s.set_text_ticks(false);
    insert(&s, "ex:b5 rdfs:label \"Brown Owl\"");
    s.set_guard(Some(std::sync::Arc::new(RejectAll)));
    let e = sparkles::sparql::update::update(
        &s,
        &format!(
            "{P}DELETE DATA {{ ex:b5 rdfs:label \"Brown Owl\" }} ; INSERT DATA {{ ex:b6 rdfs:label \"brown\" }}"
        ),
        &QueryOptions::default(),
    );
    assert!(matches!(e, Err(sparkles::Error::Rejected(_))), "{e:?}");
    s.set_guard(None);
    assert_eq!(
        sorted(rows(&s, "SELECT ?s { ?s text:query \"brown\" }")),
        ["b1", "b2", "b2", "b5"]
    );
    let st = s.text_status().unwrap();
    assert_eq!((st.docs, st.state.as_str()), (7, "ready"));
}

/// The commit a crash image's full-text index names in its payload.
fn text_payload_seq(root: &std::path::Path) -> u64 {
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("text/meta.json")).unwrap()).unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(meta["payload"].as_str().unwrap()).unwrap();
    payload["seq"].as_u64().unwrap()
}

#[test]
fn staged_and_kept_documents_are_recovered_after_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let img = |n: &str| dir.path().join(n);
    let head;
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        load(&s);
        s.enable_text(TextConfig::default()).unwrap();
        s.set_text_ticks(false);
        // staged only: the index on disk does not have it
        insert(&s, "ex:b8 rdfs:label \"Crimson Heron\"");
        copy_dir(&root, &img("staged"));
        // a search commits the batch, keeping the removed quad's document (the payload
        // then names the commit before the removal)
        delete(&s, "ex:b1 rdfs:label \"The Quick Brown Fox\"@en");
        insert(&s, "ex:b9 rdfs:label \"Violet Heron\"");
        assert_eq!(
            sorted(rows(&s, "SELECT ?s { ?s text:query \"heron\" }")),
            ["b8", "b9"]
        );
        copy_dir(&root, &img("kept"));
        // more staged changes on top
        insert(&s, "ex:b10 rdfs:label \"Grey Heron\"");
        delete(&s, "ex:b8 rdfs:label \"Crimson Heron\"");
        copy_dir(&root, &img("mixed"));
        head = s.snapshot().commit;
    }
    for (name, heron, fox) in [
        ("staged", vec!["b8"], vec!["b1"]),
        ("kept", vec!["b8", "b9"], vec![]),
        ("mixed", vec!["b10", "b9"], vec![]),
    ] {
        let root = img(name);
        // the crash lost what the index had only staged or kept
        let payload = text_payload_seq(&root);
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        let data = s.snapshot().commit;
        assert!(payload < data && data <= head, "{name}: {payload} {data}");
        let st = s.text_status().unwrap();
        assert_eq!(st.state, "ready", "{name}");
        assert!(st.last_rebuild.is_none(), "{name}");
        assert_eq!(
            sorted(rows(&s, "SELECT ?s { ?s text:query \"heron\" }")),
            heron,
            "{name}"
        );
        assert_eq!(
            rows(&s, "SELECT ?s { ?s text:query \"fox\" }"),
            fox,
            "{name}"
        );
    }
}

#[test]
fn large_batches_are_committed_by_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    load(&s);
    s.enable_text(TextConfig::default()).unwrap();
    s.set_text_ticks(false);
    let batch = |from: usize, n: usize| {
        (from..from + n)
            .map(|i| format!("ex:m{i} rdfs:label \"moose {i}\" ."))
            .collect::<Vec<_>>()
            .join(" ")
    };
    // a small batch stays staged
    insert(&s, &batch(0, 1000));
    let before = text_payload_seq(&root);
    assert!(before < s.snapshot().commit);
    // one that reaches the limit is committed without waiting for a search
    for k in 1..20 {
        insert(&s, &batch(k * 1000, 1000));
    }
    let head = s.snapshot().commit;
    let seq = text_payload_seq(&root);
    assert!(seq > before + 1 && seq < head, "{before} {seq} {head}");
    assert_eq!(
        rows(&s, "SELECT (COUNT(*) AS ?n) { ?s text:query \"moose\" }"),
        ["20000"]
    );
}

#[test]
fn concurrent_searches_match_their_snapshots() {
    let s = std::sync::Arc::new(mem());
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (s, done) = (s.clone(), done.clone());
            std::thread::spawn(move || {
                let mut n = 0;
                while !done.load(std::sync::atomic::Ordering::SeqCst) || n == 0 {
                    let snap = s.snapshot();
                    let text = rows_at(
                        snap.clone(),
                        "SELECT (COUNT(*) AS ?n) { ?s text:query \"walrus\" }",
                    );
                    let data = rows_at(
                        snap,
                        "SELECT (COUNT(*) AS ?n) { ?s rdfs:label ?l FILTER(CONTAINS(?l, \"walrus\")) }",
                    );
                    assert_eq!(text, data);
                    n += 1;
                }
            })
        })
        .collect();
    for i in 0..150 {
        insert(&s, &format!("ex:w{i} rdfs:label \"walrus {i}\""));
        if i >= 3 {
            delete(
                &s,
                &format!("ex:w{} rdfs:label \"walrus {}\"", i - 3, i - 3),
            );
        }
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    for r in readers {
        r.join().unwrap();
    }
    assert_eq!(
        rows(&s, "SELECT (COUNT(*) AS ?n) { ?s text:query \"walrus\" }"),
        ["3"]
    );
}

/// Rough write-path timing with full-text search on and off (not a benchmark: run it
/// alone, optimized, with `--ignored --nocapture`).
#[test]
#[ignore]
fn batch_insert_timing() {
    let batch = |verb: &str| {
        let mut u = format!("{verb} DATA {{ GRAPH <http://example.org/bench/g> {{\n");
        for i in 0..1000 {
            u.push_str(&format!("<http://example.org/bench/doc{i}> <http://example.org/title> \"Batch document {i} about graph databases and full text search\" .\n"));
        }
        u + "} }"
    };
    let (ins, del) = (batch("INSERT"), batch("DELETE"));
    let mut base = String::new();
    for i in 0..50_000 {
        base.push_str(&format!(
            "<http://example.org/n{i}> <http://www.w3.org/2000/01/rdf-schema#label> \"number {i} walrus {}\" .\n",
            i % 97
        ));
    }
    for text in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
        s.load(&[Source::from_bytes(
            base.clone().into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        if text {
            s.enable_text(TextConfig::default()).unwrap();
        }
        let o = QueryOptions::default();
        let mut times = Vec::new();
        for round in 0..30 {
            let t = std::time::Instant::now();
            sparkles::sparql::update::update(&s, &ins, &o).unwrap();
            let el = t.elapsed();
            if round >= 5 {
                times.push(el.as_secs_f64() * 1000.0);
            }
            sparkles::sparql::update::update(&s, &del, &o).unwrap();
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!(
            "text {}: insert 1k median {:.1} ms, min {:.1} ms, max {:.1} ms",
            if text { "on" } else { "off" },
            times[times.len() / 2],
            times[0],
            times[times.len() - 1]
        );
        if text {
            // read your writes: a search right after each insert commits it
            let mut times = Vec::new();
            for _ in 0..10 {
                sparkles::sparql::update::update(&s, &del, &o).unwrap();
                let t = std::time::Instant::now();
                sparkles::sparql::update::update(&s, &ins, &o).unwrap();
                let n = rows(
                    &s,
                    "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s text:query (\"batch\" 10) } }",
                );
                assert_eq!(n, ["10"]);
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            eprintln!(
                "insert 1k + top-10 search: median {:.1} ms, min {:.1} ms",
                times[times.len() / 2],
                times[0]
            );
            // the first search after a burst of writes (no tick meanwhile) commits it
            s.set_text_ticks(false);
            for rounds in [1, 5, 10, 20, 40] {
                let mut times = Vec::new();
                for _ in 0..5 {
                    for _ in 0..rounds {
                        sparkles::sparql::update::update(&s, &del, &o).unwrap();
                        sparkles::sparql::update::update(&s, &ins, &o).unwrap();
                    }
                    let t = std::time::Instant::now();
                    let n = rows(
                        &s,
                        "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s text:query (\"batch\" 10) } }",
                    );
                    assert_eq!(n, ["10"]);
                    times.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                eprintln!(
                    "first search after {rounds} x (delete 1k, insert 1k): median {:.1} ms, max {:.1} ms",
                    times[times.len() / 2],
                    times[times.len() - 1]
                );
            }
            s.set_text_ticks(true);
        }
    }
}
