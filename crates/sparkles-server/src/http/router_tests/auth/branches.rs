//! Grants limited to branches (F09 §6.1, A16): a grant with `branches` covers only those
//! branches, a branch it does not cover answers like one that does not exist, and a
//! grant limited to some graphs can neither create branches nor merge.

use super::*;

fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[users]]
name = "owner"
password = "{owner}"
datasets = {{ br = "admin" }}

[[users]]
name = "devs"
password = "{devs}"
[[users.grants]]
dataset = "br"
level = "write"
branches = ["dev*"]

[[users]]
name = "graphy"
password = "{graphy}"
[[users.grants]]
dataset = "br"
level = "write"
graphs = ["http://ex/g"]
"#,
        owner = h("owner-pw"),
        devs = h("devs-pw"),
        graphy = h("graphy-pw"),
    )
}

fn json_call(user: &str) -> [(&'static str, String); 2] {
    [
        ("authorization", b(user)),
        ("content-type", "application/json".to_string()),
    ]
}

async fn post_json(app: &Router, user: &str, uri: &str, body: &str) -> R {
    let h = json_call(user);
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
    call(app, "POST", uri, &h, body).await
}

#[tokio::test]
async fn a16_grants_limited_to_branches() {
    let s = build(Fixture {
        extra: users(),
        ..Default::default()
    });
    s.state.attach("br", DbType::Persistent, None).unwrap();
    let app = &s.app;
    // the owner makes dev and other
    for name in ["dev", "other"] {
        let r = post_json(
            app,
            "owner",
            "/$/branches/br",
            &format!(r#"{{"name":"{name}"}}"#),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
    }
    let devs = b("devs");
    // devs writes dev
    let r = update_as(app, "br@dev", &devs, INSERT).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = call(
        app,
        "POST",
        "/br/update?branch=dev",
        &[
            ("authorization", &devs),
            ("content-type", "application/sparql-update"),
        ],
        "INSERT DATA { <a:x> <a:y> <a:z> }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // creates dev2 from dev
    let r = post_json(
        app,
        "devs",
        "/$/branches/br",
        r#"{"name":"dev2","from":"dev"}"#,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // other answers as a branch that does not exist
    let r = get_as(app, &format!("/br{ASK}&branch=other"), Some(&devs)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["code"], "no-such-branch");
    let r = get_as(app, &format!("/br{ASK}&branch=nope"), Some(&devs)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["code"], "no-such-branch");
    // main is known to exist: a write there is forbidden
    let r = update_as(app, "br", &devs, INSERT).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // the listing shows only the covered branches
    let r = get_as(app, "/$/branches/br", Some(&devs)).await;
    assert_eq!(r.status, StatusCode::OK);
    let names: Vec<String> = r.json()["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["dev", "dev2"]);
    // a merge into main needs write on main
    let r = post_json(app, "devs", "/$/merge/br", r#"{"source":"dev"}"#).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // but dev2 into dev is fine
    let r = post_json(
        app,
        "devs",
        "/$/merge/br",
        r#"{"source":"dev2","target":"dev"}"#,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // protecting needs admin on the branch
    let r = call(
        app,
        "PATCH",
        "/$/branches/br/dev",
        &[
            ("authorization", &devs),
            ("content-type", "application/json"),
        ],
        r#"{"protected":true}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // a grant limited to some graphs can neither create branches nor merge
    let r = post_json(app, "graphy", "/$/branches/br", r#"{"name":"g1"}"#).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = post_json(app, "graphy", "/$/merge/br", r#"{"source":"dev"}"#).await;
    assert!(
        matches!(r.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}",
        r.status
    );
}
