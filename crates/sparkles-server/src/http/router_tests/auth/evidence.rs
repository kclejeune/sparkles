//! Evidence resolution does not disclose datasets, branches or facts a caller cannot
//! read: an unreadable reference answers exactly as a missing one.
use super::*;

async fn resolve(s: &AuthServer, ds: &str, user: &str, references: serde_json::Value) -> R {
    call(
        &s.app,
        "POST",
        &format!("/{ds}/claims/resolve"),
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({ "references": references }).to_string(),
    )
    .await
}

fn reference(st: &AppState, ds: &str, triple: &str) -> serde_json::Value {
    let d = st.get(ds).unwrap();
    serde_json::json!({
        "key": "r",
        "storedFact": {
            "datasetId": d.store.dataset_id(),
            "branchId": d.store.dataset_id(),
            "commit": d.store.head_commit().seq,
            "triple": triple,
        },
    })
}

#[tokio::test]
async fn unreadable_references_answer_as_missing_ones() {
    let s = auth_server();
    let team = "<http://ex.org/team-a> <http://ex.org/p> \"1\"";
    let secret = "<http://ex.org/secret> <http://ex.org/p> \"1\"";
    // bob reads team-a from a dataset he writes
    let r = resolve(
        &s,
        "wiki",
        "bob",
        serde_json::json!([reference(&s.state, "team-a", team)]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v["resolved"][0]["key"], "r");
    // the secret dataset exists and holds the fact, but bob cannot read it
    let hidden = resolve(
        &s,
        "wiki",
        "bob",
        serde_json::json!([reference(&s.state, "secret", secret)]),
    )
    .await;
    assert_eq!(
        hidden.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        hidden.text()
    );
    let mut missing = reference(&s.state, "secret", secret);
    missing["storedFact"]["datasetId"] = serde_json::json!(uuid::Uuid::new_v4());
    let missing = resolve(&s, "wiki", "bob", serde_json::json!([missing])).await;
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(hidden.body, missing.body);
    let v: serde_json::Value = serde_json::from_slice(&missing.body).unwrap();
    assert_eq!(v["code"], "evidence-unavailable");
    assert_eq!(v["reference"], "r");
    // a branch identity the dataset does not list answers the same
    let mut branch = reference(&s.state, "team-a", team);
    branch["storedFact"]["branchId"] = serde_json::json!(uuid::Uuid::new_v4());
    let branch = resolve(&s, "wiki", "bob", serde_json::json!([branch])).await;
    assert_eq!(branch.body, missing.body);
    // alice, a server administrator, reads the secret dataset
    let r = resolve(
        &s,
        "wiki",
        "alice",
        serde_json::json!([reference(&s.state, "secret", secret)]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // the route itself needs read on its dataset
    let r = resolve(&s, "secret", "bob", serde_json::json!([])).await;
    assert!(
        matches!(r.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}",
        r.status
    );
}
