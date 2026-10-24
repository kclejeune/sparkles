//! The dictionary analyzers (`zh-dict`, `ja-dict`, `ko-dict`) with an installed
//! dictionary: enabling one, searching with it, and an index whose dictionary was
//! replaced. Their own test binary, since the dictionary directory is process-wide.
#![cfg(feature = "text-cjk-dict")]

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::text::{Languages, TextConfig, dict};

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:a rdfs:label "研究生命起源"@zh .
ex:b rdfs:label "研究生"@zh .
"#;

const P: &str = "PREFIX ex: <http://example.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX text: <http://jena.apache.org/text#> ";

fn install(dir: &std::path::Path, list: &str) {
    let f = dir.join("dict.txt");
    std::fs::write(&f, list).unwrap();
    let bytes = dict::compile(&f, dict::Layout::Frequency).unwrap();
    dict::install("zh-dict", &bytes).unwrap();
}

fn config() -> TextConfig {
    TextConfig {
        languages: Languages::from_args(&["zh=zh-dict".to_string()]).unwrap(),
        ..Default::default()
    }
}

fn subjects(s: &Store, q: &str) -> Result<Vec<String>, String> {
    let r = query(
        s.snapshot(),
        &format!("{P}SELECT ?s WHERE {{ ?s text:query (rdfs:label {q:?} 'lang:zh') }}"),
        &QueryOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| match row[0].clone() {
            Some(oxrdf::Term::NamedNode(n)) => n.as_str().rsplit('/').next().unwrap().to_string(),
            t => format!("{t:?}"),
        })
        .collect();
    v.sort();
    Ok(v)
}

#[test]
fn a_dictionary_analyzer_searches_with_the_installed_dictionary() {
    let tmp = tempfile::tempdir().unwrap();
    let dicts = tmp.path().join("dicts");
    dict::set_dir(&dicts);
    let root = tmp.path().join("store");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();

    // without the dictionary the analyzer cannot be enabled
    let e = s.enable_text(config()).unwrap_err().to_string();
    assert!(e.contains("zh-dict"), "{e}");

    install(
        tmp.path(),
        "研究 2000 vn\n研究生 300 n\n生命 1500 n\n起源 800 n\n",
    );
    s.enable_text(config()).unwrap();
    assert_eq!(subjects(&s, "生命").unwrap(), ["a"]);
    // 研究生 is one word of the list, and 研究 is a word of ex:a only
    assert_eq!(subjects(&s, "研究").unwrap(), ["a"]);
    assert_eq!(subjects(&s, "研究生").unwrap(), ["b"]);
    drop(s);

    // the same dictionary installed again opens as before
    install(
        tmp.path(),
        "研究 2000 vn\n研究生 300 n\n生命 1500 n\n起源 800 n\n",
    );
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(subjects(&s, "生命").unwrap(), ["a"]);
    drop(s);

    // another dictionary cuts the text differently: the index adopts it and is built
    // again in the background
    install(tmp.path(), "研究 2000 vn\n生命 1500 n\n起源 800 n\n");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.rebuild_text().unwrap();
    assert_eq!(subjects(&s, "研究").unwrap(), ["a", "b"]);
    drop(s);

    // without its dictionary the index cannot open, and searches fail until the
    // dictionary is installed again
    std::fs::remove_file(dict::path_of("zh-dict")).unwrap();
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let e = s.rebuild_text().unwrap_err().to_string();
    assert!(subjects(&s, "研究").is_err(), "{e}");
    install(tmp.path(), "研究 2000 vn\n生命 1500 n\n起源 800 n\n");
    s.rebuild_text().unwrap();
    assert_eq!(subjects(&s, "研究").unwrap(), ["a", "b"]);
}
