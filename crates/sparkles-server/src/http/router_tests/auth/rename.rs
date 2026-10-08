//! Renames require server administration and refuse grants that cover only one
//! of the two names.
use super::*;
async fn rename(s: &AuthServer, source: &str, target: &str, user: &str) -> R {
    call(
        &s.app,
        "POST",
        &format!("/$/datasets/{source}/rename"),
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &serde_json::json!({"name":target}).to_string(),
    )
    .await
}
#[tokio::test]
async fn rename_requires_server_admin_and_identifies_grants() {
    let s = auth_server();
    drop(s.state.create("ungranted", DbType::Persistent).unwrap());
    assert_eq!(
        rename(&s, "wiki", "renamed", "carol").await.status,
        StatusCode::FORBIDDEN
    );
    let r = rename(&s, "wiki", "renamed", "alice").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("grants") && r.text().contains("wiki"));
    // Even an ungranted source cannot be moved into a name covered by a grant.
    assert_eq!(
        rename(&s, "ungranted", "team-renamed", "alice")
            .await
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        rename(&s, "ungranted", "renamed", "alice").await.status,
        StatusCode::OK
    );
}
#[tokio::test]
async fn rename_refuses_an_active_minted_scope() {
    let s = auth_server();
    drop(s.state.create("ungranted", DbType::Persistent).unwrap());
    let token = tokens::mint_as(
        &s.app,
        &[("authorization", &b("alice"))],
        r#"{"name":"reader","datasets":{"ungranted":"read"},"expiresIn":"1d"}"#,
    )
    .await;
    assert_eq!(token.status, StatusCode::CREATED, "{}", token.text());
    let r = rename(&s, "ungranted", "renamed", "alice").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("token") && r.text().contains("ungranted"));
}
#[tokio::test]
async fn grants_covering_both_names_do_not_block_a_rename() {
    let s = auth_server();
    drop(s.state.create("ungranted", DbType::Persistent).unwrap());
    let token = tokens::mint_as(
        &s.app,
        &[("authorization", &b("alice"))],
        r#"{"name":"everything","datasets":{"*":"read"},"expiresIn":"1d"}"#,
    )
    .await;
    assert_eq!(token.status, StatusCode::CREATED, "{}", token.text());
    // `*` covers the new name as it covered the old one
    let r = rename(&s, "ungranted", "renamed", "alice").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // so does bob's `team-*` grant for a rename within it
    drop(s.state.create("team-x", DbType::Persistent).unwrap());
    let r = rename(&s, "team-x", "team-y", "alice").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}
