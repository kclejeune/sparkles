//! Minted API tokens: mint, use, list, revoke, effective permissions, chains, rules and
//! persistence.

use super::*;

/// `POST /$/auth/tokens` with a JSON body as `auth` (an `Authorization` value) or a
/// session (`cookie`, `csrf`).
pub(super) async fn mint_as(app: &Router, creds: &[(&str, &str)], body: &str) -> R {
    let mut h = vec![("content-type", "application/json")];
    h.extend_from_slice(creds);
    call(app, "POST", "/$/auth/tokens", &h, body).await
}

fn session_creds<'a>(cookie: &'a str, csrf: &'a str) -> [(&'static str, &'a str); 2] {
    [("cookie", cookie), ("x-sparkles-csrf", csrf)]
}

#[tokio::test]
async fn mint_use_list_revoke() {
    let s = auth_server();
    let (cookie, csrf) = password_session(&s, "bob").await;
    let r = mint_as(
        &s.app,
        &session_creds(&cookie, &csrf),
        r#"{"name":"ci","datasets":{"wiki":"read"},"expiresIn":"7d"}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let j = r.json();
    let token = j["token"].as_str().unwrap().to_string();
    let id = j["id"].as_str().unwrap().to_string();
    assert!(
        regex::Regex::new("^spk_[A-Za-z0-9_-]{43}$")
            .unwrap()
            .is_match(&token)
    );
    assert!(
        regex::Regex::new("^tok_[a-z2-7]{12}$")
            .unwrap()
            .is_match(&id)
    );
    assert_eq!(j["owner"], "user:bob");

    let t = bearer(&token);
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&t))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        update_as(&s.app, "wiki", &t, INSERT).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get_as(&s.app, &format!("/team-a{ASK}"), Some(&t))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // the token's own whoami
    let who = get_as(&s.app, "/$/whoami", Some(&t)).await.json();
    assert_eq!(who["principal"]["kind"], "token");
    assert_eq!(who["principal"]["name"], id.as_str());
    assert_eq!(who["datasets"], serde_json::json!({ "wiki": "read" }));

    let list = call(&s.app, "GET", "/$/auth/tokens", &[("cookie", &cookie)], "").await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text());
    let l = list.json();
    let tokens = l["tokens"].as_array().unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0]["id"], id.as_str());
    assert!(tokens[0].get("hash").is_none() && tokens[0].get("token").is_none());
    assert!(!list.text().contains("spk_"));
    assert!(tokens[0]["lastUsed"].is_string());

    let path = s.dir.path().join("auth/tokens.json");
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(!file.contains("spk_"), "{file}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let del = call(
        &s.app,
        "DELETE",
        &format!("/$/auth/tokens/{id}"),
        &session_creds(&cookie, &csrf),
        "",
    )
    .await;
    assert_eq!(del.status, StatusCode::NO_CONTENT, "{}", del.text());
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&t))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert!(metric(&s.app, "sparkles_auth_tokens_minted_total{via=\"api\"}").await >= 1);
    assert!(metric(&s.app, "sparkles_auth_tokens_revoked_total").await >= 1);
}

#[tokio::test]
async fn tokens_never_exceed_their_owner_and_chains_shrink() {
    let s = auth_server();
    let bob = b("bob");
    let r = mint_as(
        &s.app,
        &[("authorization", &bob)],
        r#"{"name":"all","datasets":{"*":"admin"},"server":["*"]}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let t1 = r.json()["token"].as_str().unwrap().to_string();
    let t1_id = r.json()["id"].as_str().unwrap().to_string();
    let a1 = bearer(&t1);
    // bob only reads team-*: the token cannot write it; bob has no metrics
    assert_eq!(
        update_as(&s.app, "team-a", &a1, INSERT).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get_as(&s.app, "/$/metrics", Some(&a1)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        update_as(&s.app, "wiki", &a1, INSERT).await.status,
        StatusCode::OK
    );

    // a chain: T1 mints T2, which may not outlive T1
    let long = mint_as(
        &s.app,
        &[("authorization", &a1)],
        r#"{"name":"child","expiresIn":"365d"}"#,
    )
    .await;
    assert_eq!(long.status, StatusCode::BAD_REQUEST, "{}", long.text());
    let r2 = mint_as(
        &s.app,
        &[("authorization", &a1)],
        r#"{"name":"child","datasets":{"*":"read"},"expiresIn":"1d"}"#,
    )
    .await;
    assert_eq!(r2.status, StatusCode::CREATED, "{}", r2.text());
    assert_eq!(r2.json()["parent"], t1_id.as_str());
    let a2 = bearer(r2.json()["token"].as_str().unwrap());
    assert_eq!(
        get_as(&s.app, &format!("/team-a{ASK}"), Some(&a2))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        update_as(&s.app, "wiki", &a2, INSERT).await.status,
        StatusCode::FORBIDDEN
    );
    // the child lists with the same owner
    let l = get_as(&s.app, "/$/auth/tokens", Some(&a2)).await.json();
    assert_eq!(l["tokens"].as_array().unwrap().len(), 2);

    // removing bob's team-* grant shrinks both tokens at their next request
    s.write_config(&Fixture {
        bob_team: false,
        ..Default::default()
    });
    s.auth().reload().unwrap();
    assert_eq!(
        get_as(&s.app, &format!("/team-a{ASK}"), Some(&a1))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get_as(&s.app, &format!("/team-a{ASK}"), Some(&a2))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // revoking the parent invalidates the child
    let del = call(
        &s.app,
        "DELETE",
        &format!("/$/auth/tokens/{t1_id}"),
        &[("authorization", &bob)],
        "",
    )
    .await;
    assert_eq!(del.status, StatusCode::NO_CONTENT);
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&a2))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn mint_rules() {
    let s = auth_server();
    let anon = mint_as(&s.app, &[], r#"{"name":"x"}"#).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    let stat = mint_as(
        &s.app,
        &[("authorization", &bearer(&t_prom()))],
        r#"{"name":"x"}"#,
    )
    .await;
    assert_eq!(stat.status, StatusCode::FORBIDDEN, "{}", stat.text());
    let long = mint_as(
        &s.app,
        &[("authorization", &b("bob"))],
        r#"{"name":"x","expiresIn":"400d"}"#,
    )
    .await;
    assert_eq!(long.status, StatusCode::BAD_REQUEST);
    let noname = mint_as(
        &s.app,
        &[("authorization", &b("bob"))],
        &format!(r#"{{"name":"{}"}}"#, "n".repeat(81)),
    )
    .await;
    assert_eq!(noname.status, StatusCode::BAD_REQUEST);
    let badpat = mint_as(
        &s.app,
        &[("authorization", &b("bob"))],
        r#"{"name":"x","datasets":{"a b":"read"}}"#,
    )
    .await;
    assert_eq!(badpat.status, StatusCode::BAD_REQUEST);
    let old = mint_as(
        &s.app,
        &[("authorization", &bearer(&t_old()))],
        r#"{"name":"x"}"#,
    )
    .await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    assert_eq!(old.json()["error"], "token expired");
    let cfg = call(
        &s.app,
        "DELETE",
        "/$/auth/tokens/cfg-prometheus",
        &[("authorization", &b("alice"))],
        "",
    )
    .await;
    assert_eq!(cfg.status, StatusCode::FORBIDDEN);
    // someone else's token is hidden; server-admin sees and revokes everything
    let r = mint_as(&s.app, &[("authorization", &b("bob"))], r#"{"name":"b"}"#).await;
    let id = r.json()["id"].as_str().unwrap().to_string();
    let carol = call(
        &s.app,
        "DELETE",
        &format!("/$/auth/tokens/{id}"),
        &[("authorization", &b("carol"))],
        "",
    )
    .await;
    assert_eq!(carol.status, StatusCode::NOT_FOUND);
    let all_bob = get_as(&s.app, "/$/auth/tokens?all=true", Some(&b("bob"))).await;
    assert_eq!(all_bob.status, StatusCode::FORBIDDEN);
    let all = get_as(&s.app, "/$/auth/tokens?all=true", Some(&b("alice")))
        .await
        .json();
    let ids: Vec<&str> = all["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&id.as_str()) && ids.contains(&"cfg-prometheus"),
        "{ids:?}"
    );
    let by_owner = call(
        &s.app,
        "DELETE",
        "/$/auth/tokens?owner=user:bob",
        &[("authorization", &b("alice"))],
        "",
    )
    .await;
    assert_eq!(by_owner.json()["revoked"], 1);
    // `self` revokes the token in use
    let r = mint_as(&s.app, &[("authorization", &b("bob"))], r#"{"name":"me"}"#).await;
    let me = bearer(r.json()["token"].as_str().unwrap());
    let del = call(
        &s.app,
        "DELETE",
        "/$/auth/tokens/self",
        &[("authorization", &me)],
        "",
    )
    .await;
    assert_eq!(del.status, StatusCode::NO_CONTENT);
    assert_eq!(
        get_as(&s.app, "/$/whoami", Some(&me)).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn tokens_survive_a_restart() {
    let s = auth_server();
    let keep = mint_as(&s.app, &[("authorization", &b("bob"))], r#"{"name":"k"}"#).await;
    let gone = mint_as(&s.app, &[("authorization", &b("bob"))], r#"{"name":"g"}"#).await;
    let keep = bearer(keep.json()["token"].as_str().unwrap());
    let gone_id = gone.json()["id"].as_str().unwrap().to_string();
    let gone = bearer(gone.json()["token"].as_str().unwrap());
    let del = call(
        &s.app,
        "DELETE",
        &format!("/$/auth/tokens/{gone_id}"),
        &[("authorization", &b("bob"))],
        "",
    )
    .await;
    assert_eq!(del.status, StatusCode::NO_CONTENT);
    let s = s.restart();
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&keep))
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        get_as(&s.app, &format!("/wiki{ASK}"), Some(&gone))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn token_routes_are_absent_without_auth() {
    let s = build(Fixture {
        enabled: false,
        ..Default::default()
    });
    let c = get_as(&s.app, "/$/auth/config", None).await;
    assert_eq!(c.json(), serde_json::json!({ "enabled": false }));
    assert_eq!(
        get_as(&s.app, "/$/auth/tokens", None).await.status,
        StatusCode::NOT_FOUND
    );
}
