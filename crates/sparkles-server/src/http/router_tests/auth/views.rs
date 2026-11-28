//! Who reads an incremental view through the server (spec F13 §6.1): containment of
//! grant sets, the `view:{name}` grant that declassifies a view, and the audit records.

use super::graphs::DATA;
use super::*;
use crate::auth::Endpoint;

/// The view principal `vsec` reads the default graph and the secret graph
/// `http://ex/b/1`. Its view `secret-count` counts the secret graph's triples.
/// * `vwide` reads every graph of the dataset by name, which contains `vsec`'s grants;
/// * `vnarrow` reads the default graph and `http://ex/a/*`;
/// * `vnamed` has `vnarrow`'s grants and a grant that names `secret-count`;
/// * `vonly` has only the grant that names `secret-count`.
fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[users]]
name = "vsec"
password = "{vsec}"
[[users.grants]]
dataset = "graphs"
level = "read"
graphs = ["urn:x-arq:DefaultGraph", "http://ex/b/1"]

[[users]]
name = "vwide"
password = "{vwide}"
[[users.grants]]
dataset = "graphs"
level = "read"
graphs = ["urn:x-arq:DefaultGraph", "http://ex/*"]

[[users]]
name = "vnarrow"
password = "{vnarrow}"
[[users.grants]]
dataset = "graphs"
level = "read"
graphs = ["urn:x-arq:DefaultGraph", "http://ex/a/*"]

[[users]]
name = "vnamed"
password = "{vnamed}"
[[users.grants]]
dataset = "graphs"
level = "read"
graphs = ["urn:x-arq:DefaultGraph", "http://ex/a/*"]
[[users.grants]]
dataset = "graphs"
level = "read"
endpoints = ["view:secret-count"]

[[users]]
name = "vonly"
password = "{vonly}"
[[users.grants]]
dataset = "graphs"
level = "read"
endpoints = ["view:secret-count"]
"#,
        vsec = h("vsec-pw"),
        vwide = h("vwide-pw"),
        vnarrow = h("vnarrow-pw"),
        vnamed = h("vnamed-pw"),
        vonly = h("vonly-pw"),
    )
}

const SECRET: &str = "http://ex/b/1";
const READ: &str = "PREFIX view: <urn:x-sparkles:view#> SELECT ?n WHERE { SERVICE view:read { [] view:name \"secret-count\" ; view:bind ?n } }";
/// The view's rows joined with a count of what the caller reads, so that a node above
/// the view read is the query's root.
const JOINED: &str = "PREFIX view: <urn:x-sparkles:view#> SELECT ?n ?c WHERE { SERVICE view:read { [] view:name \"secret-count\" ; view:bind ?n } { SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o } } }";

fn enc(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

async fn server() -> AuthServer {
    let s = build(Fixture {
        extra: users(),
        ..Default::default()
    });
    let ds = s.state.attach("graphs", DbType::Memory, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let body = serde_json::json!({
        "query": format!("SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{SECRET}> {{ ?s ?p ?o }} }}"),
        "principal": "vsec",
    });
    let r = call(
        &s.app,
        "PUT",
        "/$/views/graphs/secret-count",
        &[
            ("authorization", &b("alice")),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    s.state
        .get("graphs")
        .unwrap()
        .dataset
        .views()
        .wait_built("secret-count", Duration::from_secs(30))
        .unwrap();
    s
}

async fn read_as(s: &AuthServer, user: &str) -> R {
    query_as(s, user, READ).await
}

async fn query_as(s: &AuthServer, user: &str, q: &str) -> R {
    call(
        &s.app,
        "GET",
        &format!("/graphs/sparql?query={}", enc(q)),
        &[("authorization", &b(user)), ("accept", "text/csv")],
        "",
    )
    .await
}

/// A caller whose grants contain the principal's reads the view, a caller with a
/// grant that names it reads it, and a narrower caller gets `403 view-access` without
/// the name of the graph it may not read.
#[tokio::test]
async fn view_reads_follow_containment_and_named_grants() {
    let s = server().await;
    for user in ["alice", "vsec", "vwide", "vnamed"] {
        let r = read_as(&s, user).await;
        assert_eq!(r.status, StatusCode::OK, "{user}: {}", r.text());
        assert_eq!(r.text().lines().nth(1), Some("3"), "{user}: {}", r.text());
    }
    let r = read_as(&s, "vnarrow").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(r.text().contains("view-access"), "{}", r.text());
    assert!(!r.text().contains(SECRET), "{}", r.text());
    // a grant that only names a view opens nothing, so the dataset stays hidden
    let r = read_as(&s, "vonly").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    let r = get_as(&s.app, "/$/views/graphs", Some(&b("vonly"))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    // the result cache never answers a narrower caller with a granted caller's rows,
    // also when the view read sits below a join; both callers read the same graphs
    for _ in 0..2 {
        let r = query_as(&s, "vnamed", JOINED).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let r = query_as(&s, "vnarrow", JOINED).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    }
}

/// A grant that names a view adds no graph, no protection and no write.
#[tokio::test]
async fn a_grant_that_names_a_view_reads_nothing_else() {
    let s = server().await;
    let auth = s.state.auth.clone().unwrap();
    let only = auth.user_principal("vonly").unwrap();
    assert!(only.level("graphs").is_none());
    assert!(only.level_any_branch("graphs").is_none());
    for e in Endpoint::ALL {
        assert!(!only.can_at("graphs", e, crate::auth::Level::Read), "{e:?}");
    }
    let named = auth
        .user_principal("vnamed")
        .unwrap()
        .view("graphs", Endpoint::Query)
        .unwrap();
    let narrow = auth
        .user_principal("vnarrow")
        .unwrap()
        .view("graphs", Endpoint::Query)
        .unwrap();
    assert_eq!(named.read, narrow.read);
    assert_eq!(named.write, narrow.write);
    assert!(named.views.grants("secret-count"));
    assert!(!narrow.views.grants("secret-count"));
    assert!(!named.views.grants("other"));
}

/// Listings show a view's row count to the callers who may read it, including one
/// that a grant names, and to no one else.
#[tokio::test]
async fn view_metadata_follows_the_decision() {
    let s = server().await;
    for (user, sees) in [("vwide", true), ("vnamed", true), ("vnarrow", false)] {
        let r = get_as(&s.app, "/$/views/graphs/secret-count", Some(&b(user))).await;
        assert_eq!(r.status, StatusCode::OK, "{user}: {}", r.text());
        assert_eq!(r.json().get("rows").is_some(), sees, "{user}: {}", r.text());
    }
}

/// Grants name views exactly, by a valid name.
#[test]
fn view_grants_need_a_view_name() {
    let base = r#"version = 1
[[users]]
name = "u"
password = "$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNo"
[[users.grants]]
dataset = "ds"
level = "read"
"#;
    let parse = |endpoints: &str| {
        crate::auth::config::FileConfig::parse(&format!("{base}endpoints = {endpoints}\n"))
    };
    for bad in ["view:*", "view:", "view:no spaces", "view:a/b"] {
        let e = format!("{:#}", parse(&format!("[\"{bad}\"]")).unwrap_err());
        assert!(e.contains("not a view name"), "{bad}: {e}");
    }
    parse(r#"["query", "view:team-sizes"]"#).unwrap();
    parse(r#"["view:team-sizes"]"#).unwrap();
    // a grant that names only views declassifies them and grants nothing else
    let only = |extra: &str| {
        crate::auth::config::FileConfig::parse(&format!(
            "{}{extra}\nendpoints = [\"view:team-sizes\"]\n",
            base.replace("level = \"read\"\n", "")
        ))
    };
    only("level = \"read\"").unwrap();
    for bad in [
        "level = \"write\"",
        "level = \"read\"\ngraphs = [\"http://ex/*\"]",
        "level = \"read\"\nserver_models = true",
    ] {
        let e = format!("{:#}", only(bad).unwrap_err());
        assert!(e.contains("names only views"), "{bad}: {e}");
    }
}

/// The audit log names the dataset, the view, the decision and the caller, and never
/// the graphs of either grant set.
#[test]
fn view_decisions_are_audited_without_graph_names() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let s = rt.block_on(server());
    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(buf.clone())
        .finish();
    let auth = s.state.auth.clone().unwrap();
    let views = s.state.get("graphs").unwrap().dataset.views();
    tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        for user in ["vnarrow", "vnamed", "vwide"] {
            let p = auth.user_principal(user).unwrap();
            let access = p.view("graphs", Endpoint::Query);
            views
                .authorize(
                    "secret-count",
                    access.as_ref(),
                    sparkles::incr::ViewOp::Read,
                )
                .unwrap();
        }
    });
    let text = String::from_utf8(buf.0.lock().clone()).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|j: &serde_json::Value| j["fields"]["event"] == "view_access")
        .collect();
    let seen: Vec<(String, String, String)> = lines
        .iter()
        .map(|j| {
            let f = &j["fields"];
            assert_eq!(f["dataset"], "graphs", "{j}");
            assert_eq!(f["view"], "secret-count", "{j}");
            (
                f["principal"].as_str().unwrap().to_string(),
                f["decision"].as_str().unwrap().to_string(),
                f["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expect = |u: &str, d: &str, r: &str| {
        seen.iter()
            .any(|(p, dd, rr)| p.contains(u) && dd == d && rr == r)
    };
    assert!(expect("vnarrow", "deny", "graphs"), "{seen:?}");
    assert!(expect("vnamed", "allow", "declassified"), "{seen:?}");
    assert!(expect("vwide", "allow", "contained"), "{seen:?}");
    assert!(!text.contains(SECRET), "{text}");
    assert!(!text.contains("http://ex/a/"), "{text}");
    assert!(!text.contains("vnarrow-pw"), "{text}");
}

/// The view grants follow the dataset, the branch and token scopes of their grant, and a
/// principal whose reads hide graphs whatever its grants say reads no view by name but
/// is still named in audit records.
#[tokio::test]
async fn view_grants_follow_their_scope() {
    use crate::auth::{Access, Grants, Level, Restricted, Scope};
    let grant = |dataset: &str, branches: Option<Vec<String>>| Restricted {
        dataset: dataset.into(),
        level: Level::Read,
        graphs: None,
        endpoints: Some(Vec::new()),
        lifts: Vec::new(),
        branches,
        server_models: false,
        views: vec!["v".into()],
    };
    let grants = Grants {
        restricted: vec![
            grant("graphs", None),
            grant("dev-only", Some(vec!["dev".into()])),
        ],
        ..Default::default()
    };
    let a = Access::of(grants.clone());
    assert_eq!(a.views("graphs"), ["v"]);
    assert!(a.views("other").is_empty(), "another dataset");
    assert!(
        a.views("dev-only").is_empty(),
        "a branch the grant does not cover"
    );
    assert_eq!(a.views("dev-only@dev"), ["v"]);
    let scoped = |ds: &str| Access {
        grants: grants.clone(),
        scopes: vec![Scope {
            datasets: [(ds.to_string(), Level::Read)].into(),
            server: Vec::new(),
        }],
    };
    assert_eq!(scoped("graphs").views("graphs"), ["v"]);
    assert!(
        scoped("other").views("graphs").is_empty(),
        "a scope that does not reach it"
    );
    // a grant that names only views grants no level
    assert!(a.level("graphs").is_none());
    assert!(grants.every_branch_level("graphs").is_none());

    let s = server().await;
    let auth = s.state.auth.clone().unwrap();
    let hidden = auth
        .user_principal("vnamed")
        .unwrap()
        .hiding_graphs(&["http://ex/zzz/*".into()])
        .view("graphs", Endpoint::Query)
        .unwrap();
    assert!(!hidden.views.grants("secret-count"));
    assert!(
        hidden
            .views
            .audit_id()
            .is_some_and(|id| id.contains("vnamed"))
    );
    let admin = auth
        .user_principal("alice")
        .unwrap()
        .hiding_graphs(&["http://ex/zzz/*".into()])
        .view("graphs", Endpoint::Query)
        .unwrap();
    assert!(
        admin
            .views
            .audit_id()
            .is_some_and(|id| id.contains("alice"))
    );
}
