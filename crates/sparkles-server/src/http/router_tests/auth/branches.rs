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

/// C18 A30: the agent template lets an agent write its own graphs on main, change a
/// curated graph only on its proposal branches, and never merge.
#[tokio::test]
async fn c18_a30_agent_template() {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let template = crate::auth::cli::agent_template(
        "agent-7",
        "org",
        "https://example.org/memory/agents/agent-7/",
        &[
            "https://example.org/hr".into(),
            "https://example.org/memory/consolidated".into(),
        ],
    )
    .unwrap();
    let s = build(Fixture {
        extra: format!(
            "{template}\n[[users]]\nname = \"agent-7\"\npassword = \"{a}\"\nroles = [\"agent-7\"]\n\n[[users]]\nname = \"owner\"\npassword = \"{o}\"\ndatasets = {{ org = \"admin\" }}\n",
            a = h("agent-7-pw"),
            o = h("owner-pw"),
        ),
        ..Default::default()
    });
    s.state.attach("org", DbType::Persistent, None).unwrap();
    let app = &s.app;
    let r = post_json(
        app,
        "owner",
        "/$/branches/org",
        r#"{"name":"proposals.agent-7.fix"}"#,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let agent = b("agent-7");
    let write = |g: &str| format!("INSERT DATA {{ GRAPH <{g}> {{ <a:x> <a:y> <a:z> }} }}");
    // its own graphs on main
    let r = update_as(
        app,
        "org",
        &agent,
        &write("https://example.org/memory/agents/agent-7/sessions/s1"),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // a curated graph: not on main, but on its proposal branch
    let r = update_as(app, "org", &agent, &write("https://example.org/hr")).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = update_as(
        app,
        "org@proposals.agent-7.fix",
        &agent,
        &write("https://example.org/hr"),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // never a merge or a merge preview
    let r = post_json(
        app,
        "agent-7",
        "/$/merge/org",
        r#"{"source":"proposals.agent-7.fix"}"#,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = get_as(
        app,
        "/$/merge/org?source=proposals.agent-7.fix",
        Some(&agent),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // it reads everything
    let r = get_as(app, &format!("/org{ASK}"), Some(&agent)).await;
    assert_eq!(r.status, StatusCode::OK);
    // bad input
    assert!(
        crate::auth::cli::agent_template("bad name", "org", "https://x.example/", &[]).is_err()
    );
    assert!(
        crate::auth::cli::agent_template(
            "a",
            "org",
            "https://x.example/a/",
            &["https://x.example/*".into()]
        )
        .is_err()
    );
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
    // so does the commit graph, which draws only those branches
    let r = get_as(app, "/$/commit-graph/br", Some(&devs)).await;
    assert_eq!(r.status, StatusCode::OK);
    let g = r.json();
    let drawn: Vec<&str> = g["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(drawn, ["dev", "dev2"]);
    assert!(
        g["commits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["branch"] == "dev" || c["branch"] == "dev2"),
        "{g}"
    );
    let r = get_as(app, "/$/commit-graph/br?branches=other", Some(&devs)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get_as(app, "/$/commit-graph/br?branches=main", Some(&devs)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
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

/// Relinking needs admin on the branch: write is not enough, and a grant limited to
/// some branches may relink those it covers but no other.
#[tokio::test]
async fn relinking_needs_admin_on_the_branch() {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    let extra = format!(
        r#"{}
[[users]]
name = "keeper"
password = "{}"
[[users.grants]]
dataset = "br"
level = "admin"
branches = ["dev*"]
"#,
        users(),
        h("keeper-pw"),
    );
    let s = build(Fixture {
        extra,
        ..Default::default()
    });
    s.state.attach("br", DbType::Persistent, None).unwrap();
    let app = &s.app;
    for name in ["dev", "other"] {
        let r = post_json(
            app,
            "owner",
            "/$/branches/br",
            &format!(r#"{{"name":"{name}"}}"#),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let r = post_json(app, "devs", "/$/branches/br/dev/relink", "").await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = post_json(app, "keeper", "/$/branches/br/other/relink", "").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["code"], "no-such-branch");
    let r = post_json(app, "graphy", "/$/branches/br/dev/relink", "").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    for (user, branch) in [("keeper", "dev"), ("owner", "other")] {
        let r = post_json(app, user, &format!("/$/branches/br/{branch}/relink"), "").await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "{user} {branch}: {}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(r.json()["branch"], branch);
    }
}

async fn patch_json(app: &Router, user: &str, uri: &str, body: &str) -> R {
    let h = json_call(user);
    let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
    call(app, "PATCH", uri, &h, body).await
}

#[tokio::test]
async fn a28_renames_reverts_and_cherry_picks_follow_branch_grants() {
    let s = build(Fixture {
        extra: users(),
        ..Default::default()
    });
    s.state.attach("br", DbType::Persistent, None).unwrap();
    let app = &s.app;
    for name in ["dev", "dev2"] {
        let r = post_json(
            app,
            "owner",
            "/$/branches/br",
            &format!(r#"{{"name":"{name}"}}"#),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let devs = b("devs");
    let r = update_as(app, "br@dev", &devs, INSERT).await;
    assert_eq!(r.status, StatusCode::OK);
    // devs may revert and cherry-pick on its branches, not on main
    let r = post_json(app, "devs", "/$/revert/br?branch=dev&commit=1", "").await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = post_json(app, "devs", "/$/revert/br?commit=1", "").await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = post_json(
        app,
        "devs",
        "/$/cherry-pick/br?source=dev&commit=1&branch=dev2",
        "",
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = post_json(app, "devs", "/$/cherry-pick/br?source=dev&commit=1", "").await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // devs may rename within its grant, and no grant changes what it covers
    let r = patch_json(app, "devs", "/$/branches/br/dev2", r#"{"name":"dev3"}"#).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.json()["name"], "dev3");
    assert_eq!(r.json()["grantsChanged"], 0);
    // not out of it
    let r = patch_json(app, "devs", "/$/branches/br/dev3", r#"{"name":"other"}"#).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    // the owner may, and is told that devs' grant no longer covers the branch
    let r = patch_json(app, "owner", "/$/branches/br/dev", r#"{"name":"feature"}"#).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.json()["grantsChanged"], 1);
    let r = update_as(app, "br@feature", &devs, INSERT).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

/// The branch listing, each branch's description and the head of `br`, without the
/// sizes of the branches' files, which change as they are opened.
async fn branch_state(app: &Router, st: &AppState, auth: &str) -> (Vec<J>, u64) {
    fn without_sizes(mut b: J) -> J {
        b.as_object_mut().unwrap().remove("storage");
        b
    }
    let r = get_as(app, "/$/branches/br", Some(auth)).await;
    assert_eq!(r.status, StatusCode::OK);
    let mut list = r.json();
    let all: Vec<J> = list["branches"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .map(without_sizes)
        .collect();
    list["branches"] = J::Array(all);
    let mut infos = vec![list];
    for name in ["main", "dev", "dev2"] {
        let r = get_as(app, &format!("/$/branches/br/{name}"), Some(auth)).await;
        assert_eq!(r.status, StatusCode::OK, "{name}");
        infos.push(without_sizes(r.json()));
    }
    (infos, head(st, "br"))
}

/// A read-only server refuses every branch mutation as it refuses an update, whoever
/// asks, and changes no branch, head or setting.
#[tokio::test]
async fn read_only_servers_refuse_branch_mutations() {
    let mut s = build(Fixture {
        extra: users(),
        ..Default::default()
    });
    s.state.attach("br", DbType::Persistent, None).unwrap();
    for name in ["dev", "dev2"] {
        let r = post_json(
            &s.app,
            "owner",
            "/$/branches/br",
            &format!(r#"{{"name":"{name}"}}"#),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let r = update_as(&s.app, "br@dev", &b("owner"), INSERT).await;
    assert_eq!(r.status, StatusCode::OK);
    s.fixture.read_only = true;
    let s = s.restart();
    // reopened from its directory
    if s.state.get("br").is_none() {
        s.state.attach("br", DbType::Persistent, None).unwrap();
    }
    let app = &s.app;
    assert!(s.state.read_only);
    let owner = b("owner");
    let before = branch_state(app, &s.state, &owner).await;
    // the update endpoint's answer, which every branch mutation gives
    let r = update_as(app, "br", &owner, INSERT).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // without the request's own id
    let shape = |mut v: J| {
        v.as_object_mut().unwrap().remove("requestId");
        v
    };
    let refusal = shape(r.json());
    assert_eq!(refusal["error"], "server is read-only");
    let json = |user: &str| {
        vec![
            ("authorization".to_string(), b(user)),
            ("content-type".to_string(), "application/json".to_string()),
        ]
    };
    let mut async_merge = json("owner");
    async_merge.push(("prefer".into(), "respond-async".into()));
    // method, path, headers, body
    type Case<'a> = (&'a str, &'a str, Vec<(String, String)>, &'a str);
    let cases: Vec<Case> = vec![
        (
            "POST",
            "/$/branches/br",
            json("owner"),
            r#"{"name":"dev3"}"#,
        ),
        (
            "POST",
            "/$/branches/br",
            json("devs"),
            r#"{"name":"dev4","from":"dev"}"#,
        ),
        (
            "PATCH",
            "/$/branches/br",
            json("owner"),
            r#"{"exemptPredicates":["urn:p"]}"#,
        ),
        (
            "PATCH",
            "/$/branches/br/dev",
            json("devs"),
            r#"{"note":"changed"}"#,
        ),
        (
            "PATCH",
            "/$/branches/br/dev",
            json("owner"),
            r#"{"protected":true}"#,
        ),
        (
            "PATCH",
            "/$/branches/br/dev2",
            json("devs"),
            r#"{"name":"dev5"}"#,
        ),
        ("DELETE", "/$/branches/br/dev2", json("devs"), ""),
        (
            "DELETE",
            "/$/branches/br/dev?reparent=true",
            json("devs"),
            "",
        ),
        ("DELETE", "/$/branches/br/dev?force=true", json("owner"), ""),
        ("POST", "/$/merge/br", json("owner"), r#"{"source":"dev"}"#),
        ("POST", "/$/merge/br", async_merge, r#"{"source":"dev"}"#),
        (
            "POST",
            "/$/merge/br",
            json("devs"),
            r#"{"source":"dev","target":"dev2"}"#,
        ),
        (
            "POST",
            "/$/merge/br",
            json("owner"),
            r#"{"source":"dev","squash":true}"#,
        ),
        (
            "POST",
            "/$/merge/br",
            json("owner"),
            r#"{"source":"dev","ff":"replay"}"#,
        ),
        (
            "POST",
            "/$/merge/br",
            json("owner"),
            r#"{"source":"dev","dryRun":true}"#,
        ),
        ("POST", "/$/revert/br?branch=dev&commit=1", json("devs"), ""),
        (
            "POST",
            "/$/cherry-pick/br?source=dev&commit=1&branch=dev2",
            json("devs"),
            "",
        ),
        ("POST", "/$/branches/br/dev/relink", json("owner"), ""),
    ];
    for (method, uri, headers, body) in &cases {
        let h: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let r = call(app, method, uri, &h, body).await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{method} {uri} {body}: {}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(shape(r.json()), refusal, "{method} {uri} {body}");
    }
    assert_eq!(branch_state(app, &s.state, &owner).await, before);
    // reads and previews still work
    let r = get_as(app, "/$/merge/br?source=dev", Some(&owner)).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}
