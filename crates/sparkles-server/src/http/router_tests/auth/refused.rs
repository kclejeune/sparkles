//! `refusedDatasets` of `GET /$/server`: a caller sees only the refused datasets it may
//! read, and only a server administrator sees the reason, which names paths.

use super::*;

const READER_A: char = 'D';
const READER_B: char = 'E';

/// A server with the healthy dataset `wiki` and the dataset `b`, whose index is in a
/// format this release does not read.
fn server_with_a_refused_dataset() -> AuthServer {
    let tokens = format!(
        r#"
[[tokens]]
name = "reader-a"
hash = "{}"
datasets = {{ wiki = "read" }}

[[tokens]]
name = "reader-b"
hash = "{}"
datasets = {{ b = "read" }}
"#,
        token_hash(&tok(READER_A)),
        token_hash(&tok(READER_B))
    );
    let s = build(Fixture {
        extra: tokens,
        ..Default::default()
    });
    let ds = s.state.create("b", DbType::Persistent).unwrap();
    let db = ds.store.root().unwrap().to_path_buf();
    drop(ds);
    let current = std::fs::read_to_string(db.join("CURRENT")).unwrap();
    let meta = db.join(current.trim()).join("meta.json");
    let mut m: J = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
    m["format_version"] = 99.into();
    std::fs::write(&meta, serde_json::to_vec(&m).unwrap()).unwrap();
    let s = s.restart();
    assert!(s.state.get("b").is_none());
    s
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_datasets_follow_the_callers_access_and_redact_the_reason() {
    let s = server_with_a_refused_dataset();
    let listed = |r: &R| -> Vec<J> {
        r.json()["refusedDatasets"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };

    // a caller that reads only the healthy dataset learns nothing about b
    let r = get_as(&s.app, "/$/server", Some(&bearer(&tok(READER_A)))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(listed(&r).iter().all(|d| d["name"] != "b"), "{}", r.text());

    // a caller that reads b sees it, with a reason that names no path
    let r = get_as(&s.app, "/$/server", Some(&bearer(&tok(READER_B)))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let seen = listed(&r);
    assert_eq!(seen.len(), 1, "{}", r.text());
    assert_eq!(seen[0]["name"], "b");
    let reason = seen[0]["reason"].as_str().unwrap();
    // the fixed text names the route, not a path of the data directory
    assert!(
        !reason.contains(s.dir.path().to_str().unwrap()) && !reason.contains("gen-0001"),
        "{reason}"
    );
    assert_eq!(
        reason,
        "this release does not read the dataset's files; a server administrator can see why in the server log or in GET /$/server"
    );

    // a server administrator sees the whole reason
    let r = get_as(&s.app, "/$/server", Some(&b("alice"))).await;
    let seen = listed(&r);
    assert_eq!(seen.len(), 1, "{}", r.text());
    assert!(
        seen[0]["reason"]
            .as_str()
            .unwrap()
            .contains("index version 99"),
        "{}",
        r.text()
    );
}
