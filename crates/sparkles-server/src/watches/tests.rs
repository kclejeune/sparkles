//! Watches on entities: the routes, the follower's matching of entity and subtree
//! watches, the access checks and the file.

use super::follow::Followers;
use super::*;
use crate::auth::Peer;
use crate::state::DbType;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::{Value as J, json};
use tower::ServiceExt;

const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const G: &str = "urn:g:onto";

struct Srv {
    _dir: tempfile::TempDir,
    st: Arc<AppState>,
    app: Router,
    f: Followers,
}

fn auth_config() -> String {
    let h = |pw: &str| crate::auth::hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
version = 1

[[users]]
name = "alice"
password = "{}"
server = ["server-admin"]

[[users]]
name = "bob"
password = "{}"

[[users.grants]]
dataset = "ds"
level = "write"

[[users]]
name = "erin"
password = "{}"

[[users.grants]]
dataset = "ds"
level = "read"
graphs = ["urn:g:open"]

[[users]]
name = "pat"
password = "{}"
datasets = {{ pds = "read" }}

[[protections]]
name = "hidden"
dataset = "pds"
predicates = ["urn:secret"]
"#,
        h("alice-pw"),
        h("bob-pw"),
        h("erin-pw"),
        h("pat-pw"),
    )
}

fn server() -> Srv {
    let dir = tempfile::tempdir().unwrap();
    let mut st = AppState::new(
        &dir.path().join("data"),
        sparkles::store::StoreOptions::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    let cfg = dir.path().join("auth.toml");
    std::fs::write(&cfg, auth_config()).unwrap();
    st.auth = Some(Arc::new(
        crate::auth::Auth::open(&cfg, dir.path()).unwrap().0,
    ));
    crate::settings::server::start(&mut st, None, &Default::default()).unwrap();
    let st = Arc::new(st);
    st.create("ds", DbType::Persistent).unwrap();
    st.create("pds", DbType::Persistent).unwrap();
    st.set_phase(crate::obs::Phase::Ready);
    let app = crate::http::router(st.clone());
    Srv {
        _dir: dir,
        st,
        app,
        f: Followers::default(),
    }
}

fn basic(user: &str) -> String {
    use base64::Engine;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{user}-pw"))
    )
}

impl Srv {
    async fn call(&self, method: &str, uri: &str, user: &str, body: J) -> (StatusCode, J) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())))
            .header("authorization", basic(user));
        let body = if body.is_null() {
            Body::empty()
        } else {
            req = req.header("content-type", "application/json");
            Body::from(body.to_string())
        };
        let res = self
            .app
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let s = res.status();
        let b = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (s, serde_json::from_slice(&b).unwrap_or(J::Null))
    }

    /// Run a SPARQL update on `branch` as `user`, which the commit records as its author.
    async fn update(&self, user: &str, branch: &str, u: &str) {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/ds/update?branch={branch}"))
            .extension(ConnectInfo(Peer::Tcp("127.0.0.2:1".parse().unwrap())))
            .header("authorization", basic(user))
            .header("content-type", "application/sparql-update")
            .body(Body::from(u.to_string()))
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        assert!(res.status().is_success(), "update {u}: {}", res.status());
    }

    async fn watch(&self, user: &str, body: J) -> J {
        let (s, v) = self.call("POST", "/$/watches", user, body).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v
    }

    /// Run the follower, then answer the commits watch `id` matched, oldest first.
    async fn matched(&mut self, user: &str, id: &str) -> Vec<J> {
        let st = self.st.clone();
        let mut f = std::mem::take(&mut self.f);
        let f = tokio::task::spawn_blocking(move || {
            f.step(&st);
            f
        })
        .await
        .unwrap();
        self.f = f;
        let (s, v) = self
            .call("GET", &format!("/$/watches/{id}/matches"), user, J::Null)
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let mut m = v["matches"].as_array().unwrap().clone();
        m.reverse();
        m
    }

    fn head(&self) -> u64 {
        self.st.datasets()["ds"].store.snapshot().commit
    }
}

fn sub(c: &str, p: &str) -> String {
    format!("INSERT DATA {{ GRAPH <{G}> {{ <{c}> <{RDFS}subClassOf> <{p}> }} }}")
}

fn label(c: &str, l: &str) -> String {
    format!("INSERT DATA {{ GRAPH <{G}> {{ <{c}> <{RDFS}label> \"{l}\" }} }}")
}

fn commits(m: &[J]) -> Vec<u64> {
    m.iter().map(|x| x["commit"].as_u64().unwrap()).collect()
}

#[tokio::test]
async fn an_entity_watch_matches_commits_that_name_it() {
    let mut s = server();
    s.update("alice", "main", &label("urn:a", "A")).await;
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let id = w["id"].as_str().unwrap();
    assert_eq!(w["since"], s.head());
    s.update("alice", "main", &label("urn:a", "A2")).await;
    let named = s.head();
    // another entity, and the entity in another graph
    s.update("alice", "main", &label("urn:b", "B")).await;
    s.update(
        "alice",
        "main",
        "INSERT DATA { GRAPH <urn:g:other> { <urn:a> <urn:p> 1 } }",
    )
    .await;
    // the entity as object, by its owner
    s.update(
        "bob",
        "main",
        &format!("INSERT DATA {{ GRAPH <{G}> {{ <urn:c> <urn:refers> <urn:a> }} }}"),
    )
    .await;
    let own = s.head();
    let m = s.matched("bob", id).await;
    assert_eq!(commits(&m), vec![named, own]);
    assert_eq!(m[0]["members"], json!(["urn:a"]));
    assert_eq!(m[0]["author"], "user:alice");
    assert_eq!(m[0]["exact"], true);
    assert!(m[0].get("own").is_none());
    assert_eq!(m[1]["own"], true);
    // the list counts them unread until marked read
    let (_, l) = s.call("GET", "/$/watches", "bob", J::Null).await;
    assert_eq!(l["unread"], 2);
    let (st, _) = s
        .call(
            "POST",
            &format!("/$/watches/{id}/seen"),
            "bob",
            json!({"commit": named}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    // a lower value does not move it back
    s.call(
        "POST",
        &format!("/$/watches/{id}/seen"),
        "bob",
        json!({"commit": 1}),
    )
    .await;
    let (_, l) = s.call("GET", "/$/watches", "bob", J::Null).await;
    assert_eq!(l["unread"], 1);
    assert_eq!(l["watches"][0]["seen"], named);
    // the same watch again is the same watch
    let (st, again) = s
        .call(
            "POST",
            "/$/watches",
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(again["id"], id);
    // another user neither lists nor reads it
    let (_, l) = s.call("GET", "/$/watches", "alice", J::Null).await;
    assert_eq!(l["watches"], json!([]));
    let (st, _) = s
        .call("GET", &format!("/$/watches/{id}/matches"), "alice", J::Null)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .call("DELETE", &format!("/$/watches/{id}"), "alice", J::Null)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = s
        .call("DELETE", &format!("/$/watches/{id}"), "bob", J::Null)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_subtree_watch_follows_moves_in_and_out() {
    let mut s = server();
    s.update("alice", "main", &sub("urn:b", "urn:root")).await;
    s.update("alice", "main", &sub("urn:c", "urn:b")).await;
    s.update("alice", "main", &sub("urn:x", "urn:other")).await;
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:root", "scope": "subtree"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    // a grandchild's label
    s.update("alice", "main", &label("urn:c", "C")).await;
    let c1 = s.head();
    // outside the subtree
    s.update("alice", "main", &label("urn:x", "X")).await;
    // into the subtree, then out again, before the follower runs
    s.update(
        "alice",
        "main",
        &format!(
            "DELETE DATA {{ GRAPH <{G}> {{ <urn:x> <{RDFS}subClassOf> <urn:other> }} }}; {}",
            sub("urn:x", "urn:c")
        ),
    )
    .await;
    let into = s.head();
    s.update("alice", "main", &label("urn:x", "X2")).await;
    let inside = s.head();
    s.update(
        "alice",
        "main",
        &format!("DELETE DATA {{ GRAPH <{G}> {{ <urn:x> <{RDFS}subClassOf> <urn:c> }} }}"),
    )
    .await;
    let out = s.head();
    s.update("alice", "main", &label("urn:x", "X3")).await;
    let m = s.matched("bob", &id).await;
    assert_eq!(commits(&m), vec![c1, into, inside, out]);
    assert_eq!(m[0]["members"], json!(["urn:c"]));
    assert!(
        m[1]["members"]
            .as_array()
            .unwrap()
            .contains(&json!("urn:x"))
    );
    // a change to the root itself matches too
    s.update("alice", "main", &label("urn:root", "R")).await;
    let root = s.head();
    let m = s.matched("bob", &id).await;
    assert_eq!(*commits(&m).last().unwrap(), root);
    // deleting a class in the subtree matches
    s.update(
        "alice",
        "main",
        &format!("DELETE DATA {{ GRAPH <{G}> {{ <urn:c> <{RDFS}subClassOf> <urn:b> }} }}"),
    )
    .await;
    let gone = s.head();
    let m = s.matched("bob", &id).await;
    assert_eq!(*commits(&m).last().unwrap(), gone);
}

#[tokio::test]
async fn a_depth_limit_keeps_deeper_classes_out() {
    let mut s = server();
    s.update("alice", "main", &sub("urn:b", "urn:root")).await;
    s.update("alice", "main", &sub("urn:c", "urn:b")).await;
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:root", "scope": "subtree", "depth": 1}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    s.update("alice", "main", &label("urn:c", "C")).await;
    s.update("alice", "main", &label("urn:b", "B")).await;
    let b = s.head();
    let m = s.matched("bob", &id).await;
    assert_eq!(commits(&m), vec![b]);
    let (st, _) = s
        .call(
            "POST",
            "/$/watches",
            "bob",
            json!({"dataset": "ds", "entity": "urn:root", "depth": 2}),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "depth needs the subtree scope");
}

#[tokio::test]
async fn a_large_commit_is_one_summary_entry() {
    let mut s = server();
    let (st, v) = s
        .call(
            "PATCH",
            "/$/server/settings/notifications",
            "alice",
            json!({"watches": {"largeCommit": 2}}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    s.update("alice", "main", &sub("urn:b", "urn:root")).await;
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:root", "scope": "subtree"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    let many: String = (0..5)
        .map(|i| format!("<urn:k{i}> <{RDFS}subClassOf> <urn:b> . "))
        .collect();
    s.update(
        "alice",
        "main",
        &format!("INSERT DATA {{ GRAPH <{G}> {{ {many} }} }}"),
    )
    .await;
    let m = s.matched("bob", &id).await;
    assert_eq!(m.len(), 1);
    assert_eq!(m[0]["summary"], true);
    assert_eq!(m[0]["count"], 6, "five new classes and their parent");
    assert!(m[0].get("members").is_none());
}

#[tokio::test]
async fn watches_follow_a_renamed_branch_and_go_with_a_deleted_one() {
    let mut s = server();
    let main = s.st.datasets()["ds"].clone();
    main.store
        .create_branch("dev", &Default::default())
        .unwrap();
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "branch": "dev", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    main.store.rename_branch("dev", "next").unwrap();
    s.st.renamed_branch(&main, "dev", "next");
    s.update("alice", "next", &label("urn:a", "A")).await;
    let m = s.matched("bob", &id).await;
    assert_eq!(m.len(), 1);
    let (_, l) = s.call("GET", "/$/watches", "bob", J::Null).await;
    assert_eq!(l["watches"][0]["branch"], "next");
    // a branch made again under the old name is another branch
    main.store
        .create_branch("dev", &Default::default())
        .unwrap();
    main.store.delete_branch("next", true).unwrap();
    let st = s.st.clone();
    let mut f = std::mem::take(&mut s.f);
    tokio::task::spawn_blocking(move || f.step(&st))
        .await
        .unwrap();
    let (_, l) = s.call("GET", "/$/watches", "bob", J::Null).await;
    assert_eq!(l["watches"], json!([]));
}

#[tokio::test]
async fn access_decides_what_can_be_watched_and_listed() {
    let mut s = server();
    // erin reads only urn:g:open
    let (st, _) = s
        .call(
            "POST",
            "/$/watches",
            "erin",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let w = s
        .watch(
            "erin",
            json!({"dataset": "ds", "graph": "urn:g:open", "entity": "urn:a"}),
        )
        .await;
    assert_eq!(w["graph"], "urn:g:open");
    // triple protections are refused
    let (st, v) = s
        .call(
            "POST",
            "/$/watches",
            "pat",
            json!({"dataset": "pds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(v["code"], "watch-unsupported-protections", "{v}");
    // no dataset of that name for a caller without access
    let (st, _) = s
        .call(
            "POST",
            "/$/watches",
            "erin",
            json!({"dataset": "nope", "entity": "urn:a"}),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // a watch of an owner that no longer exists matches nothing
    let ghost = Owner {
        kind: Kind::User,
        name: "ghost".into(),
        groups: vec![],
    };
    let ds = s.st.datasets()["ds"].clone();
    let (gw, _) =
        s.st.watches
            .create(
                &s.st,
                &ghost,
                NewWatch {
                    dataset: "ds".into(),
                    branch: "main".into(),
                    branch_id: ds.store.branch_id(),
                    graph: Some(G.into()),
                    entity: "urn:a".into(),
                    scope: Scope::Entity,
                    depth: None,
                    head: s.head(),
                },
                Utc::now(),
            )
            .unwrap();
    s.update("alice", "main", &label("urn:a", "A")).await;
    let _ = s.matched("erin", w["id"].as_str().unwrap()).await;
    let (ws, _) = s.st.watches.snapshot(&s.st).unwrap();
    let g = ws.iter().find(|x| x.id == gw.id).unwrap();
    assert!(g.matches.is_empty());
}

#[tokio::test]
async fn a_refused_file_is_left_as_it_is() {
    let s = server();
    let p = s.st.data_dir.join(FILE);
    std::fs::write(&p, r#"{"format": 99, "watches": []}"#).unwrap();
    let (st, v) = s.call("GET", "/$/watches", "bob", J::Null).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["code"], "watches-unavailable");
    let (st, _) = s
        .call(
            "POST",
            "/$/watches",
            "bob",
            json!({"dataset": "ds", "entity": "urn:a"}),
        )
        .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        r#"{"format": 99, "watches": []}"#
    );
}

#[tokio::test]
async fn matches_and_cursors_survive_a_restart() {
    let mut s = server();
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    s.update("alice", "main", &label("urn:a", "A")).await;
    let first = s.head();
    assert_eq!(commits(&s.matched("bob", &id).await), vec![first]);
    // a new follower and a fresh read of the file, as after a restart
    s.f = Followers::default();
    *s.st.watches.inner.lock() = Inner::default();
    s.update("alice", "main", &label("urn:a", "A2")).await;
    let second = s.head();
    assert_eq!(commits(&s.matched("bob", &id).await), vec![first, second]);
    let saved = std::fs::read_to_string(s.st.data_dir.join(FILE)).unwrap();
    assert!(saved.contains("\"format\": 1"), "{saved}");
}
