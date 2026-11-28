//! FSST-coded vocabularies (spec X11 Phase 2a): a database built with the `fsst` codec
//! answers as one built with front coding, keeps its codec through partial and full
//! compactions, changes it when asked, and is refused by name when its meta lists a
//! feature this build does not read.

use sparkles_core::builder::{BuildOptions, FEATURE_FSST_VOCAB, FORMAT_VERSION, VocabCodec};
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
                .unwrap()
                .iter()
                .map(|r| {
                    let t = format!("{:?}", r.values());
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
    assert_eq!(g.meta.format_version, FORMAT_VERSION);
    assert_eq!(g.meta.features, [FEATURE_FSST_VOCAB]);
    let fg = &front.snapshot().generation;
    assert!(!fg.vocab.is_fsst());
    assert_eq!(fg.meta.format_version, FORMAT_VERSION);
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
    assert_eq!(s.snapshot().generation.meta.format_version, FORMAT_VERSION);
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
    assert_eq!(g.meta.format_version, FORMAT_VERSION);
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

/// A release that read index formats 2 and 3 refuses a generation of this release's
/// format as newer, by the version in its meta.json, and this release reads only its
/// own format.
#[test]
fn a_format_3_reader_refuses_the_current_format() {
    let old = sparkles_core::formats::FileKind {
        reads: &[2, 3],
        writes: 3,
        ..sparkles_core::formats::INDEX
    };
    let e = old
        .check("gen-0002", FORMAT_VERSION, None)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("index version 5") && e.contains("newer release"),
        "{e}"
    );
    assert!(sparkles_core::formats::INDEX.reads(FORMAT_VERSION));
    assert!(!sparkles_core::formats::INDEX.reads(4));
}

/// `sparkles migrate --check` refuses what opening the store refuses: an unknown index
/// feature, and FSST tables of a version this build does not read.
#[test]
fn migration_preflight_checks_features_and_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    let s = load(&root, Some(VocabCodec::Fsst));
    let dir = gen_dir(&s);
    drop(s);
    let plan = sparkles_core::migrate::plan(&root).unwrap();
    assert!(plan.refused.is_empty(), "{:?}", plan.refused);
    assert!(plan.files.iter().any(|f| f.kind == "vocab-fsst"));
    let meta_path = dir.join("meta.json");
    let meta = std::fs::read(&meta_path).unwrap();
    let mut v: serde_json::Value = serde_json::from_slice(&meta).unwrap();
    v["features"] = serde_json::json!(["fsst-vocab", "zz-future-blocks"]);
    std::fs::write(&meta_path, serde_json::to_vec(&v).unwrap()).unwrap();
    let plan = sparkles_core::migrate::plan(&root).unwrap();
    assert!(
        plan.refused.iter().any(|r| r.contains("zz-future-blocks")),
        "{:?}",
        plan.refused
    );
    std::fs::write(&meta_path, &meta).unwrap();
    let tables = dir.join("vocab.fsst");
    let mut b = std::fs::read(&tables).unwrap();
    b[8..12].copy_from_slice(&2u32.to_le_bytes());
    std::fs::write(&tables, &b).unwrap();
    let plan = sparkles_core::migrate::plan(&root).unwrap();
    assert!(
        plan.refused
            .iter()
            .any(|r| r.contains("vocab-fsst") && r.contains("version 2")),
        "{:?}",
        plan.refused
    );
    assert!(Store::open(&root, StoreOptions::default()).is_err());
}

#[test]
fn index_settings_apply_to_builds_and_compactions_without_relabeling_snapshots() {
    use sparkles_core::store::IndexSettings;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("db");
    let store = Arc::new(Store::open(&root, options(None)).unwrap());
    store
        .load(&[Source::from_bytes(
            b"<urn:a> <urn:p> <urn:b> .\n<urn:b> <urn:q> <urn:a> .\n".to_vec(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    let old = store.snapshot();
    assert_eq!(old.generation.vocab_codec(), VocabCodec::Front);
    assert!(old.generation.patterns.file.is_some());
    let count = |s| {
        let result = sparkles_core::sparql::query(
            s,
            "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) { ?s ?p ?o } GROUP BY ?p ORDER BY ?p",
            &QueryOptions {
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        result
            .rows()
            .unwrap()
            .iter()
            .map(|row| format!("{:?}", row.values()))
            .collect::<Vec<_>>()
    };
    let expected_count = count(old.clone());
    let mut expected_dump = Vec::new();
    store.dump_nquads(&mut expected_dump).unwrap();

    let switched = IndexSettings {
        vocab_codec: Some(VocabCodec::Fsst),
        patterns: Some(false),
    };
    store.set_index_settings(switched).unwrap();
    assert_eq!(store.index_settings(), switched);
    assert!(Arc::ptr_eq(&old, &store.snapshot()));

    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    let result = store.compact_with(&CompactOptions {
        cancel: Some(cancelled.clone()),
        progress: Some(Arc::new(move |_| stop.store(true, Ordering::Relaxed))),
        ..Default::default()
    });
    assert!(cancelled.load(Ordering::Relaxed));
    assert!(matches!(result, Err(sparkles_core::Error::Cancelled)));
    assert!(Arc::ptr_eq(&old, &store.snapshot()));
    assert_clean(&root);

    let always = CompactOptions {
        partial: Some(PartialMode::Always),
        ..Default::default()
    };
    let after_start = IndexSettings {
        patterns: Some(true),
        ..switched
    };
    let edit_store = store.clone();
    let report = store
        .compact_with(&CompactOptions {
            progress: Some(Arc::new(move |_| {
                if edit_store.index_settings_layer() != after_start {
                    edit_store.set_index_settings(after_start).unwrap();
                }
            })),
            ..always.clone()
        })
        .unwrap();
    assert_eq!(report.mode, "full");
    assert!(report.full_reason.unwrap().contains("vocabulary codec"));
    assert_eq!(store.index_settings(), after_start);
    let fsst = store.snapshot();
    assert_eq!(fsst.generation.vocab_codec(), VocabCodec::Fsst);
    assert!(fsst.generation.patterns.file.is_none());
    assert_eq!(
        fsst.generation
            .meta
            .patterns
            .as_ref()
            .unwrap()
            .skipped
            .as_deref(),
        Some("turned off for this build"),
    );
    assert_eq!(count(fsst.clone()), expected_count);
    assert_eq!(count(old.clone()), expected_count);
    assert_eq!(old.generation.vocab_codec(), VocabCodec::Front);
    let mut dump = Vec::new();
    store.dump_nquads(&mut dump).unwrap();
    assert_eq!(dump, expected_dump);

    let report = store.compact_with(&always).unwrap();
    assert_eq!(report.mode, "full");
    assert!(report.full_reason.unwrap().contains("predicate patterns"));
    assert!(store.snapshot().generation.patterns.file.is_some());
    assert!(fsst.generation.patterns.file.is_none());
    assert_eq!(count(store.snapshot()), expected_count);

    store.set_index_settings(switched).unwrap();
    let report = store.compact_with(&always).unwrap();
    assert_eq!(report.mode, "partial", "{report:?}");
    assert!(store.snapshot().generation.patterns.file.is_none());
    assert_eq!(count(store.snapshot()), expected_count);
    assert_clean(&root);

    store
        .set_index_settings(IndexSettings {
            vocab_codec: Some(VocabCodec::Front),
            patterns: None,
        })
        .unwrap();
    drop((store, old, fsst));
    // The saved codec takes precedence over the opening's explicit build option.
    let store = Store::open(&root, options(Some(VocabCodec::Fsst))).unwrap();
    assert_eq!(store.index_settings().vocab_codec, Some(VocabCodec::Front));
    assert_eq!(store.snapshot().generation.vocab_codec(), VocabCodec::Fsst);
    assert!(store.snapshot().generation.patterns.file.is_none());
    let fresh_root = temp.path().join("fresh");
    let fresh = Store::open(&fresh_root, options(None)).unwrap();
    fresh.set_index_settings(switched).unwrap();
    fresh
        .load(&[Source::from_bytes(
            b"<urn:c> <urn:p> <urn:a> .\n".to_vec(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    assert_eq!(fresh.snapshot().generation.vocab_codec(), VocabCodec::Fsst);
    assert!(fresh.snapshot().generation.patterns.file.is_none());
    assert_clean(&fresh_root);
    store.compact().unwrap();
    assert_eq!(store.snapshot().generation.vocab_codec(), VocabCodec::Front);
    assert!(store.snapshot().generation.patterns.file.is_some());
    assert_clean(&root);
}

#[test]
fn index_settings_reject_unknown_versions_and_choices_before_open() {
    use sparkles_core::store::INDEX_SETTINGS_FILE;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("db");
    drop(Store::open(&root, options(None)).unwrap());
    let current = std::fs::read(root.join("CURRENT")).unwrap();
    for (json, message, unsupported) in [
        (r#"{"format": 2, "vocabCodec": "fsst"}"#, "format 2", true),
        (r#"{"format": 1, "vocabCodec": "zstd"}"#, "zstd", false),
        (r#"{"format": 1, "patterns": "yes"}"#, "boolean", false),
        (r#"{"format": 1, "blockCodec": "lz4"}"#, "blockCodec", false),
    ] {
        std::fs::write(root.join(INDEX_SETTINGS_FILE), json).unwrap();
        let error = Store::open(&root, options(None)).err().unwrap();
        assert_eq!(
            matches!(error, sparkles_core::Error::Unsupported(_)),
            unsupported
        );
        let error = error.to_string();
        assert!(
            error.contains(INDEX_SETTINGS_FILE) && error.contains(message),
            "{error}"
        );
        let report = check(&root, &CheckOptions { quick: true }).unwrap();
        assert_eq!(report.status, Status::Error, "{}", report.to_text());
        assert!(report.to_text().contains(message));
        assert_eq!(std::fs::read(root.join("CURRENT")).unwrap(), current);
    }
}

#[test]
fn branch_rebuilds_read_the_live_dataset_index_policy() {
    use sparkles_core::store::IndexSettings;

    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path(), options(None)).unwrap();
    store
        .load(&[Source::from_bytes(
            b"<urn:a> <urn:p> <urn:b> .\n".to_vec(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    store.create_branch("work", &Default::default()).unwrap();
    let branch = store.branch("work").unwrap();
    let old = branch.snapshot();
    let index = IndexSettings {
        vocab_codec: Some(VocabCodec::Fsst),
        patterns: Some(false),
    };
    store.set_index_settings(index).unwrap();
    assert_eq!(branch.index_settings(), index);
    assert_eq!(old.generation.vocab_codec(), VocabCodec::Front);
    branch
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Off),
            ..Default::default()
        })
        .unwrap();
    let new = branch.snapshot();
    assert_eq!(new.generation.vocab_codec(), VocabCodec::Fsst);
    assert!(new.generation.patterns.file.is_none());
    assert_eq!(new.len(), old.len());
    assert_eq!(store.snapshot().generation.vocab_codec(), VocabCodec::Front);
}

#[test]
fn explicit_patterns_respect_the_budget_without_forcing_full_compaction() {
    use sparkles_core::store::IndexSettings;

    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(
        temp.path(),
        StoreOptions {
            build: BuildOptions {
                pattern_bytes: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    store
        .set_index_settings(IndexSettings {
            patterns: Some(true),
            ..Default::default()
        })
        .unwrap();
    store
        .load(&[Source::from_bytes(
            b"<urn:a> <urn:p> <urn:b> .\n<urn:b> <urn:q> <urn:a> .\n".to_vec(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    let old = store.snapshot();
    assert!(old.generation.patterns.file.is_none());
    assert!(
        old.generation
            .patterns
            .missing
            .as_deref()
            .unwrap()
            .contains("memory budget"),
        "{:?}",
        old.generation.patterns.missing
    );

    let partial = CompactOptions {
        partial: Some(PartialMode::Always),
        ..Default::default()
    };
    let report = store.compact_with(&partial).unwrap();
    assert_eq!(report.mode, "partial", "{report:?}");
    assert!(store.snapshot().generation.patterns.file.is_none());

    update(
        &store,
        "INSERT DATA { <urn:a> <urn:q> <urn:b> }",
        &QueryOptions::default(),
    )
    .unwrap();
    let report = store.compact_with(&partial).unwrap();
    assert_eq!(report.mode, "partial", "{report:?}");
    assert_eq!(store.snapshot().len(), 3);
    assert!(store.snapshot().generation.patterns.file.is_none());
}
