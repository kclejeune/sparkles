//! FSST-coded vocabularies (spec X11 Phase 2a): a database built with the `fsst` codec
//! answers as one built with front coding, keeps its codec through partial and full
//! compactions, changes it when asked, and is refused by name when its meta lists a
//! feature this build does not read.

use sparkles_core::builder::{BuildOptions, FEATURE_FSST_VOCAB, VocabCodec};
use sparkles_core::check::{CheckOptions, Status, check};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{CompactOptions, PartialMode, Store, StoreOptions};
use std::path::Path;

fn data() -> String {
    let mut s = String::from(
        "@prefix ex: <http://example.org/> .\n\
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 0..3000 {
        s.push_str(&format!(
            "ex:item{i} ex:label \"Item number {i} of the collection\"@en ;\n\
             \tex:note \"A longer note about item {i}, which repeats words that FSST codes\" ;\n\
             \tex:size {} ; ex:when \"2020-01-{:02}\"^^xsd:date ;\n\
             \tex:link <http://dbpedia.org/resource/Thing_{}> , [ ex:p \"blank {i}\" ] .\n",
            i * 7,
            i % 28 + 1,
            i * 7919 % 10007
        ));
        if i % 100 == 0 {
            s.push_str(&format!(
                "ex:item{i} ex:says <<( ex:item{i} ex:size {} )>> .\n",
                i * 7
            ));
        }
    }
    s
}

fn options(codec: Option<VocabCodec>) -> StoreOptions {
    StoreOptions {
        build: BuildOptions {
            vocab_codec: codec,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn load(root: &Path, codec: Option<VocabCodec>) -> Store {
    let s = Store::open(root, options(codec)).unwrap();
    s.load(&[Source::from_bytes(
        data().into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

const QUERIES: &[&str] = &[
    "SELECT ?s ?l WHERE { ?s <http://example.org/label> ?l } ORDER BY ?l LIMIT 50",
    "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
    "SELECT ?s WHERE { ?s <http://example.org/link> ?o FILTER(STRSTARTS(STR(?o), \"http://dbpedia.org/resource/Thing_12\")) } ORDER BY ?s",
    "SELECT ?o WHERE { ?s <http://example.org/note> ?o FILTER(REGEX(?o, \"item 29\")) } ORDER BY ?o",
    "SELECT ?l WHERE { ?s <http://example.org/label> ?l FILTER(LANG(?l) = \"en\") } ORDER BY DESC(?l) LIMIT 7",
    "SELECT ?s ?d WHERE { ?s <http://example.org/when> ?d FILTER(?d > \"2020-01-25\"^^<http://www.w3.org/2001/XMLSchema#date>) } ORDER BY ?s LIMIT 20",
    "SELECT ?t WHERE { ?s <http://example.org/says> ?t } ORDER BY ?t",
    "SELECT ?b ?v WHERE { <http://example.org/item42> <http://example.org/link> ?b . ?b ?p ?v }",
    "SELECT ?s WHERE { ?s ?p \"Item number 77 of the collection\"@en }",
    "SELECT (SUM(?n) AS ?t) WHERE { ?s <http://example.org/size> ?n }",
];

fn answers(s: &Store) -> Vec<Vec<String>> {
    QUERIES
        .iter()
        .map(|q| {
            let r = sparkles_core::sparql::query(
                s.snapshot(),
                q,
                &QueryOptions {
                    no_cache: true,
                    ..Default::default()
                },
            )
            .unwrap();
            // blank node labels differ between the two stores
            r.rows()
                .into_iter()
                .map(|r| {
                    let t = format!("{r:?}");
                    if t.contains("BlankNode") {
                        "bnode".into()
                    } else {
                        t
                    }
                })
                .collect()
        })
        .collect()
}

fn assert_clean(root: &Path) {
    let r = check(root, &CheckOptions { quick: false }).unwrap();
    assert_ne!(r.status, Status::Error, "{}", r.to_text());
}

fn gen_dir(s: &Store) -> std::path::PathBuf {
    s.snapshot().generation.dir.clone().unwrap()
}

#[test]
fn an_fsst_vocabulary_answers_as_front_coding_does() {
    let tmp = tempfile::tempdir().unwrap();
    let (fr, fs) = (tmp.path().join("front"), tmp.path().join("fsst"));
    let front = load(&fr, None);
    let fsst = load(&fs, Some(VocabCodec::Fsst));
    let g = &fsst.snapshot().generation;
    assert!(g.vocab.is_fsst());
    assert_eq!(g.meta.format_version, 4);
    assert_eq!(g.meta.features, [FEATURE_FSST_VOCAB]);
    let fg = &front.snapshot().generation;
    assert!(!fg.vocab.is_fsst());
    assert_eq!(fg.meta.format_version, 3);
    assert!(fg.meta.features.is_empty());
    let size = |s: &Store| {
        std::fs::metadata(gen_dir(s).join("vocab.dat"))
            .unwrap()
            .len()
    };
    assert!(
        size(&fsst) < size(&front),
        "{} {}",
        size(&fsst),
        size(&front)
    );
    let want = answers(&front);
    assert_eq!(answers(&fsst), want);
    drop((front, fsst));
    assert_clean(&fs);
    let r = check(&fs, &CheckOptions { quick: false }).unwrap();
    assert!(r.to_text().contains("FSST codes"), "{}", r.to_text());

    // a partial compaction copies the vocabulary and keeps the codec
    let s = Store::open(&fs, options(None)).unwrap();
    update(
        &s,
        "INSERT DATA { <http://example.org/item5> <http://example.org/size> 1 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let r = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.mode, "partial", "{r:?}");
    assert!(s.snapshot().generation.vocab.is_fsst());
    assert_eq!(s.snapshot().generation.meta.format_version, 4);
    // a full compaction with no codec named keeps it too
    update(
        &s,
        "INSERT DATA { <http://example.org/new> <http://example.org/label> \"brand new\" }",
        &QueryOptions::default(),
    )
    .unwrap();
    s.compact().unwrap();
    assert!(s.snapshot().generation.vocab.is_fsst());
    let n = s.snapshot().len();
    drop(s);
    assert_clean(&fs);
    let s = Store::open(&fs, options(None)).unwrap();
    assert_eq!(s.snapshot().len(), n);
    assert!(s.snapshot().generation.vocab.is_fsst());
    drop(s);

    // naming front rebuilds in full, as format 3
    let s = Store::open(&fs, options(Some(VocabCodec::Front))).unwrap();
    update(
        &s,
        "INSERT DATA { <http://example.org/item6> <http://example.org/size> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let r = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(r.mode, "full", "{r:?}");
    assert!(r.full_reason.unwrap().contains("vocabulary codec"));
    let g = &s.snapshot().generation;
    assert!(!g.vocab.is_fsst());
    assert_eq!(g.meta.format_version, 3);
    assert!(!gen_dir(&s).join("vocab.fsst").exists());
    drop(s);
    assert_clean(&fs);
}

/// A generation that lists a feature this build does not know is refused with the
/// feature's name, by the store and by `check`. One whose tables are missing or damaged
/// is refused as damaged.
#[test]
fn unknown_features_and_damaged_tables_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    let s = load(&root, Some(VocabCodec::Fsst));
    let dir = gen_dir(&s);
    drop(s);
    let meta_path = dir.join("meta.json");
    let meta = std::fs::read(&meta_path).unwrap();
    let mut v: serde_json::Value = serde_json::from_slice(&meta).unwrap();
    v["features"] = serde_json::json!(["fsst-vocab", "zz-future-blocks"]);
    std::fs::write(&meta_path, serde_json::to_vec(&v).unwrap()).unwrap();
    let e = Store::open(&root, StoreOptions::default())
        .err()
        .unwrap()
        .to_string();
    assert!(
        e.contains("`zz-future-blocks`") && e.contains("newer release"),
        "{e}"
    );
    assert!(!e.contains("`fsst-vocab`"), "{e}");
    let r = check(&root, &CheckOptions { quick: true }).unwrap();
    assert_eq!(r.status, Status::Error);
    assert!(r.to_text().contains("zz-future-blocks"), "{}", r.to_text());
    std::fs::write(&meta_path, &meta).unwrap();

    // a damaged table file
    let tables = dir.join("vocab.fsst");
    let good = std::fs::read(&tables).unwrap();
    let mut bad = good.clone();
    let at = bad.len() / 2;
    bad[at] ^= 0x40;
    std::fs::write(&tables, &bad).unwrap();
    assert!(Store::open(&root, StoreOptions::default()).is_err());
    let r = check(&root, &CheckOptions { quick: true }).unwrap();
    assert_eq!(r.status, Status::Error, "{}", r.to_text());
    std::fs::remove_file(&tables).unwrap();
    let e = Store::open(&root, StoreOptions::default())
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("vocab.fsst"), "{e}");
    std::fs::write(&tables, &good).unwrap();
    assert!(Store::open(&root, StoreOptions::default()).is_ok());
}

/// A release that reads index formats 2 and 3 refuses an FSST generation as newer, by
/// the version in its meta.json.
#[test]
fn a_format_3_reader_refuses_format_4() {
    let old = sparkles_core::formats::FileKind {
        reads: &[2, 3],
        writes: 3,
        ..sparkles_core::formats::INDEX
    };
    let e = old.check("gen-0002", 4, None).unwrap_err().to_string();
    assert!(
        e.contains("index version 4") && e.contains("newer release"),
        "{e}"
    );
    assert!(sparkles_core::formats::INDEX.reads(4));
}
