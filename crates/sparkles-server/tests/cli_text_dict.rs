//! `sparkles text-dict`: install a dictionary from a local source, list it, and index and
//! search a database with its analyzer, run as the real binary.
#![cfg(feature = "text-cjk-dict")]

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const FIXTURE: &str = r#"
@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:a rdfs:label "研究生命起源"@zh .
ex:b rdfs:label "研究生"@zh .
"#;

#[track_caller]
fn expect(args: &[&str], dicts: &str, code: i32) -> Output {
    let o = Command::new(BIN)
        .args(args)
        .env("SPARKLES_TEXT_DICTS", dicts)
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(code),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

#[test]
fn text_dict_install_list_and_search() {
    let dir = tempfile::tempdir().unwrap();
    let dicts = dir.path().join("dicts");
    let dicts = dicts.to_str().unwrap();
    let list = dir.path().join("dict.txt");
    std::fs::write(
        &list,
        "研究 2000 vn\n研究生 300 n\n生命 1500 n\n起源 800 n\n",
    )
    .unwrap();
    let license = dir.path().join("LICENSE");
    std::fs::write(&license, "test words\n").unwrap();

    // a layout that does not suit the name is refused
    expect(
        &[
            "text-dict",
            "install",
            "ja-dict",
            "--from",
            list.to_str().unwrap(),
            "--dir",
            dicts,
        ],
        dicts,
        1,
    );
    expect(
        &[
            "text-dict",
            "install",
            "zh-dict",
            "--from",
            list.to_str().unwrap(),
            "--license",
            license.to_str().unwrap(),
            "--dir",
            dicts,
        ],
        dicts,
        0,
    );
    let o = expect(&["text-dict", "list", "--json", "--dir", dicts], dicts, 0);
    let rows: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["name"], "zh-dict");
    assert_eq!(rows[0]["words"], 4);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("dicts/zh-dict.license.txt")).unwrap(),
        "test words\n"
    );

    let data = dir.path().join("data.ttl");
    std::fs::write(&data, FIXTURE).unwrap();
    let db = dir.path().join("db");
    let db = db.to_str().unwrap();
    expect(&["load", "--loc", db, data.to_str().unwrap()], dicts, 0);
    expect(
        &[
            "text-index",
            "--loc",
            db,
            "--predicate",
            "http://www.w3.org/2000/01/rdf-schema#label",
            "--language",
            "zh=zh-dict",
            "--text-dicts",
            dicts,
        ],
        dicts,
        0,
    );
    let q = "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
             PREFIX text: <http://jena.apache.org/text#> \
             SELECT ?s WHERE { ?s text:query (rdfs:label '研究' 'lang:zh') }";
    let o = expect(&["query", "--loc", db, "--results", "tsv", q], dicts, 0);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("<http://example.org/a>"), "{out}");
    assert!(!out.contains("<http://example.org/b>"), "{out}");
}
