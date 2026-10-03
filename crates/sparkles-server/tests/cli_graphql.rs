//! `sparkles graphql` run as the real binary: a draft from the data, installing it,
//! running documents, versions and exit statuses.

#![cfg(feature = "graphql")]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const DATA: &str = r#"@prefix ex: <http://example.org/> .
ex:a a ex:Person ; ex:name "Ann" ; ex:age 31 ; ex:knows ex:b, ex:c .
ex:b a ex:Person ; ex:name "Bob" ; ex:age 25 .
ex:c a ex:Person ; ex:name "Cy" ; ex:age 40 .
"#;

fn sparkles(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn with_stdin(dir: &Path, args: &[&str], input: &str) -> Output {
    let mut c = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    c.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn draft_install_and_run() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    std::fs::write(dir.join("data.ttl"), DATA).unwrap();
    let o = sparkles(dir, &["load", "--loc", "db", "data.ttl"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));

    // no schema yet
    let o = with_stdin(dir, &["graphql", "--loc", "db", "run"], "{ __typename }");
    assert_ne!(o.status.code(), Some(0));
    assert!(err(&o).contains("schema draft"), "{}", err(&o));

    let o = sparkles(
        dir,
        &[
            "graphql", "--loc", "db", "schema", "draft", "--source", "observed",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let draft = out(&o);
    assert!(draft.contains("type Person"), "{draft}");
    assert!(draft.contains("knows: [Person!]!"), "{draft}");
    std::fs::write(dir.join("schema.graphql"), &draft).unwrap();
    let o = sparkles(
        dir,
        &[
            "graphql",
            "--loc",
            "db",
            "schema",
            "put",
            "schema.graphql",
            "--message",
            "first",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o).trim(), "version 1");

    let o = with_stdin(
        dir,
        &["graphql", "--loc", "db", "run"],
        r#"{ allPerson { nodes { name knows { name } } } }"#,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let j: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(
        j["data"]["allPerson"]["nodes"],
        serde_json::json!([
            { "name": "Ann", "knows": [{ "name": "Bob" }, { "name": "Cy" }] },
            { "name": "Bob", "knows": [] },
            { "name": "Cy", "knows": [] }
        ])
    );
    // variables and an operation name
    std::fs::write(
        dir.join("q.graphql"),
        "query A($id: ID!) { person(id: $id) { age } } query B { __typename }",
    )
    .unwrap();
    let o = sparkles(
        dir,
        &[
            "graphql",
            "--loc",
            "db",
            "run",
            "--query",
            "q.graphql",
            "--operation",
            "A",
            "--variables",
            r#"{"id": "ex:a"}"#,
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("\"age\": 31"), "{}", out(&o));
    // an error in the response exits with status 1
    let o = with_stdin(dir, &["graphql", "--loc", "db", "run"], "{ nope }");
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("GRAPHQL_VALIDATION_FAILED"));

    let o = sparkles(dir, &["graphql", "--loc", "db", "schema", "get", "--api"]);
    assert!(out(&o).contains("allPerson("), "{}", out(&o));
    let o = sparkles(dir, &["graphql", "--loc", "db", "schema", "versions"]);
    assert!(out(&o).starts_with("v1"), "{}", out(&o));
    let o = sparkles(dir, &["graphql", "--loc", "db", "schema", "delete"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!dir.join("db").join("graphql.json").exists());
}
