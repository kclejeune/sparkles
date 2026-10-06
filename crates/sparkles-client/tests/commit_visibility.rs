//! Graph-restricted history exposes commit identity without dataset-wide counts.

use serde_json::json;
use sparkles_client::CommitList;

#[test]
fn commit_pages_preserve_missing_counts_and_known_zero_counts() {
    let mut page = json!({
        "dataset": "graphs", "datasetId": "b670e8c7-03ef-4941-a5d8-304d9f658d0c",
        "head": 2, "firstRetained": 0, "complete": true, "next": null,
        "commits": [{
            "seq": 2, "parent": 1, "ref": "commit:2", "kind": "update",
            "generation": "gen-0001", "bulk": false, "branch": "main"
        }]
    });
    let restricted: CommitList = serde_json::from_value(page.clone()).unwrap();
    let commit = &restricted.commits[0];
    assert_eq!(commit.seq, 2);
    assert_eq!(commit.extra["branch"], "main");
    assert_eq!(commit.inserted, None);
    assert_eq!(commit.deleted, None);
    assert_eq!(commit.quads, None);
    assert_eq!(commit.exact, None);
    let serialized = serde_json::to_value(&restricted).unwrap();
    for field in ["inserted", "deleted", "quads", "exact"] {
        assert!(serialized["commits"][0].get(field).is_none());
    }

    let full = page["commits"][0].as_object_mut().unwrap();
    full.insert("inserted".into(), json!(0));
    full.insert("deleted".into(), json!(1));
    full.insert("quads".into(), json!(10));
    full.insert("exact".into(), json!(false));
    let visible: CommitList = serde_json::from_value(page).unwrap();
    let commit = &visible.commits[0];
    assert_eq!(commit.inserted, Some(0));
    assert_eq!(commit.deleted, Some(1));
    assert_eq!(commit.quads, Some(10));
    assert_eq!(commit.exact, Some(false));
}
