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
email = "bob@example.org"

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
name = "rita"
password = "{}"

[[users.grants]]
dataset = "ds"
level = "read"
branches = ["dev"]

[[users]]
name = "pat"
password = "{}"
datasets = {{ pds = "read" }}

[roles.readers]
datasets = {{ ds = "read" }}

[external]
allowed_groups = ["staff"]
group_roles = {{ staff = ["readers"] }}

[proxy]
trusted = ["127.0.0.3/32"]
user_header = "X-User"
groups_header = "X-Groups"
verified_email_header = "X-Email"

[[protections]]
name = "hidden"
dataset = "pds"
predicates = ["urn:secret"]
"#,
        h("alice-pw"),
        h("bob-pw"),
        h("erin-pw"),
        h("rita-pw"),
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
    // mail goes to a local test server whose certificate the test CA signs
    st.outbound.allow_private = true;
    st.outbound.tls.extra_roots_pem = Some(crate::notify::tests::TEST_CA.to_vec());
    let st = Arc::new(st);
    super::install(&st);
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

    /// A request through the trusted proxy as account `user` with `groups`, and with
    /// the verified address `email` when there is one.
    async fn proxied(
        &self,
        uri: &str,
        user: &str,
        groups: &str,
        email: Option<&str>,
    ) -> StatusCode {
        let mut req = Request::builder()
            .method("GET")
            .uri(uri)
            .extension(ConnectInfo(Peer::Tcp("127.0.0.3:1".parse().unwrap())))
            .header("x-user", user)
            .header("x-groups", groups);
        if let Some(e) = email {
            req = req.header("x-email", e);
        }
        let res = self
            .app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        res.status()
    }

    /// Run the follower once.
    async fn follow(&mut self) {
        let st = self.st.clone();
        let mut f = std::mem::take(&mut self.f);
        self.f = tokio::task::spawn_blocking(move || {
            f.step(&st);
            f
        })
        .await
        .unwrap();
    }

    /// Run a SPARQL update on `branch` as `user`, which the commit records as its author.
    async fn update(&self, user: &str, branch: &str, u: &str) {
        self.update_in("ds", user, branch, u).await;
    }

    async fn update_in(&self, ds: &str, user: &str, branch: &str, u: &str) {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/{ds}/update?branch={branch}"))
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
    let ghost = Owner::named(Kind::User, "ghost");
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
    let (ws, _, _) = s.st.watches.snapshot(&s.st).unwrap();
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

fn new_watch(s: &Srv, graph: &str, entity: &str) -> NewWatch {
    let ds = s.st.datasets()["ds"].clone();
    NewWatch {
        dataset: "ds".into(),
        branch: "main".into(),
        branch_id: ds.store.branch_id(),
        graph: Some(graph.into()),
        entity: entity.into(),
        scope: Scope::Entity,
        depth: None,
        head: s.head(),
    }
}

fn user(name: &str) -> Owner {
    Owner::named(Kind::User, name)
}

#[tokio::test]
async fn a_watch_on_an_unreadable_graph_cannot_be_deleted_or_read() {
    let s = server();
    // erin reads only urn:g:open; a watch of hers on G, as a narrower token would see it
    let (w, _) =
        s.st.watches
            .create(&s.st, &user("erin"), new_watch(&s, G, "urn:a"), Utc::now())
            .unwrap();
    let (st, v) = s
        .call("DELETE", &format!("/$/watches/{}", w.id), "erin", J::Null)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["code"], "watch-access-lost");
    let (ws, _, _) = s.st.watches.snapshot(&s.st).unwrap();
    assert!(ws.iter().any(|x| x.id == w.id));
}

#[tokio::test]
async fn a_renamed_branch_is_authorized_under_its_new_name_at_once() {
    let s = server();
    let main = s.st.datasets()["ds"].clone();
    main.store
        .create_branch("dev", &Default::default())
        .unwrap();
    let w = s
        .watch(
            "rita",
            json!({"dataset": "ds", "branch": "dev", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let id = w["id"].as_str().unwrap();
    main.store.rename_branch("dev", "next").unwrap();
    s.st.renamed_branch(&main, "dev", "next");
    // rita's grant names dev, which no longer is this branch; the follower has not run
    let (_, l) = s.call("GET", "/$/watches", "rita", J::Null).await;
    assert_eq!(l["watches"], json!([]));
    let (st, _) = s
        .call("GET", &format!("/$/watches/{id}/matches"), "rita", J::Null)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_graph_made_after_its_watch_is_followed() {
    let mut s = server();
    s.update("alice", "main", &label("urn:z", "Z")).await;
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": "urn:g:later", "entity": "urn:a"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    // a commit the follower reads binds the generation before the graph exists
    s.update("alice", "main", &label("urn:z", "Z2")).await;
    assert!(s.matched("bob", &id).await.is_empty());
    s.update(
        "alice",
        "main",
        "INSERT DATA { GRAPH <urn:g:later> { <urn:a> <urn:p> 1 } }",
    )
    .await;
    let made = s.head();
    assert_eq!(commits(&s.matched("bob", &id).await), vec![made]);
}

#[tokio::test]
async fn the_first_hierarchy_edge_after_a_subtree_watch_is_followed() {
    let mut s = server();
    s.update("alice", "main", &label("urn:root", "Root")).await;
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:root", "scope": "subtree"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    s.update("alice", "main", &label("urn:z", "Z")).await;
    assert!(s.matched("bob", &id).await.is_empty());
    // the store has no subClassOf until now
    s.update("alice", "main", &sub("urn:c", "urn:root")).await;
    let edge = s.head();
    s.update("alice", "main", &label("urn:c", "C")).await;
    let below = s.head();
    assert_eq!(commits(&s.matched("bob", &id).await), vec![edge, below]);
}

#[tokio::test]
async fn a_watch_made_from_an_older_head_reads_the_commits_after_it() {
    let mut s = server();
    let a = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let a = a["id"].as_str().unwrap().to_string();
    // the request that makes the second watch read the head before this commit
    let n = new_watch(&s, G, "urn:b");
    s.update("alice", "main", &label("urn:b", "B")).await;
    let named = s.head();
    let _ = s.matched("bob", &a).await;
    let (_, cursors, _) = s.st.watches.snapshot(&s.st).unwrap();
    assert_eq!(cursors[&n.branch_id], named);
    let (b, _) =
        s.st.watches
            .create(&s.st, &user("bob"), n, Utc::now())
            .unwrap();
    assert_eq!(commits(&s.matched("bob", &b.id).await), vec![named]);
}

#[tokio::test]
async fn a_dataset_that_is_draining_after_a_rename_keeps_its_watches() {
    let mut s = server();
    let w = s
        .watch(
            "alice",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    s.st.rename("ds", "ds2").unwrap();
    {
        // a deletion of ds2 that waits for its holders, and may yet fail
        let _leaving = s.st.leave_for_tests("ds2");
        let st = s.st.clone();
        let mut f = std::mem::take(&mut s.f);
        s.f = tokio::task::spawn_blocking(move || {
            f.step(&st);
            f
        })
        .await
        .unwrap();
    }
    let (ws, _, _) = s.st.watches.snapshot(&s.st).unwrap();
    assert!(ws.iter().any(|x| x.id == id), "the watch was removed");
    // a new dataset under the old name does not hold the branch
    s.st.create("ds", DbType::Persistent).unwrap();
    s.update_in("ds2", "alice", "main", &label("urn:a", "A"))
        .await;
    let named = s.st.datasets()["ds2"].store.snapshot().commit;
    let st = s.st.clone();
    let mut f = std::mem::take(&mut s.f);
    s.f = tokio::task::spawn_blocking(move || {
        f.step(&st);
        f
    })
    .await
    .unwrap();
    let (ws, _, _) = s.st.watches.snapshot(&s.st).unwrap();
    let w = ws.iter().find(|x| x.id == id).expect("kept");
    assert_eq!(w.dataset, "ds2");
    assert_eq!(commits_of(w), vec![named]);
}

#[tokio::test]
async fn names_of_hierarchy_nodes_that_come_and_go_are_let_go() {
    let mut s = server();
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:root", "scope": "subtree"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    for i in 0..100 {
        s.update(
            "alice",
            "main",
            &sub(&format!("urn:n{i}"), &format!("urn:x{i}")),
        )
        .await;
        s.update(
            "alice",
            "main",
            &sub(&format!("urn:n{i}"), &format!("urn:x{i}")).replace("INSERT", "DELETE"),
        )
        .await;
        if i % 10 == 9 {
            assert!(s.matched("bob", &id).await.is_empty());
        }
    }
    assert!(s.matched("bob", &id).await.is_empty());
    assert!(s.f.names() <= 64, "{} names kept", s.f.names());
    s.update("alice", "main", &sub("urn:c", "urn:root")).await;
    s.update("alice", "main", &label("urn:c", "C")).await;
    let below = s.head();
    assert_eq!(commits(&s.matched("bob", &id).await).last(), Some(&below));
}

fn commits_of(w: &Watch) -> Vec<u64> {
    w.matches.iter().map(|m| m.commit).collect()
}

#[tokio::test]
async fn a_watch_the_run_did_not_know_holds_the_cursor_back() {
    let mut s = server();
    let a = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let _ = s.matched("bob", a["id"].as_str().unwrap()).await;
    let b = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:b"}),
        )
        .await;
    s.update("alice", "main", &label("urn:b", "B")).await;
    let head = s.head();
    let bid = s.st.datasets()["ds"].store.branch_id();
    let known: std::collections::BTreeSet<String> = [a["id"].as_str().unwrap().to_string()].into();
    // a run that began before b existed finished at the head
    assert!(s.st.watches.record(
        &s.st,
        bid,
        head,
        vec![],
        &BTreeMap::new(),
        ("ds", "main"),
        &known
    ));
    let (_, cursors, _) = s.st.watches.snapshot(&s.st).unwrap();
    assert!(cursors[&bid] <= b["since"].as_u64().unwrap());
    assert_eq!(
        commits(&s.matched("bob", b["id"].as_str().unwrap()).await),
        vec![head]
    );
}

#[tokio::test]
async fn a_file_without_its_format_is_refused() {
    let s = server();
    let p = s.st.data_dir.join(FILE);
    std::fs::write(&p, r#"{"watches": []}"#).unwrap();
    let (st, _) = s.call("GET", "/$/watches", "bob", J::Null).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(std::fs::read_to_string(&p).unwrap(), r#"{"watches": []}"#);
}

// ------------------------------------------------------------- mail to owners ------

/// Notifications on, with an email channel to the local server `port` that carries
/// watches and is also the target of every route.
async fn mailing(s: &Srv, port: u16) {
    let (st, v) = s
        .call(
            "PATCH",
            "/$/server/settings/notifications",
            "alice",
            json!({
                "enabled": true,
                "channels": {"mail": {
                    "type": "email", "host": "127.0.0.1", "port": port, "tls": "starttls",
                    "from": "sparkles@example.org", "to": ["ops@example.org"], "watchers": true
                }},
                "routes": {"*": ["mail"]},
                "delivery": {"attempts": 2, "backoffSecs": 0.05, "maxBackoffSecs": 1, "timeoutSecs": 5},
            }),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

fn build(s: &Srv, b: &deliver::Batch, now: DateTime<Utc>) -> Result<(J, String), String> {
    deliver::build(&s.st, &crate::notify::settings(&s.st), b, "id-1", now)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_batch_is_mailed_to_its_owner_alone() {
    use crate::notify::tests::{FakeSmtp, SmtpMode};
    let smtp = FakeSmtp::start(SmtpMode::Starttls, vec![]);
    let mut s = server();
    mailing(&s, smtp.port).await;
    let a = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let b = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:b"}),
        )
        .await;
    s.update("alice", "main", &label("urn:a", "A")).await;
    let first = s.head();
    // bob's own commit is listed in the feed but not mailed
    s.update("bob", "main", &label("urn:b", "B")).await;
    s.update("alice", "main", &label("urn:b", "B2")).await;
    let third = s.head();
    s.follow().await;
    let waiting = s.st.watches.batches.waiting();
    assert_eq!(waiting.len(), 1, "one batch per owner and dataset");
    assert_eq!(
        waiting[0].refs,
        vec![
            (a["id"].as_str().unwrap().to_string(), first),
            (b["id"].as_str().unwrap().to_string(), third)
        ]
    );
    s.st.watches.batches.close_all();
    deliver::flush(&s.st);
    let got = tokio::task::spawn_blocking(move || smtp.wait(1))
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].to, ["bob@example.org"]);
    let d = &got[0].data;
    assert!(d.contains("To: bob@example.org\r\n"), "{d}");
    assert!(!d.contains("ops@example.org"), "{d}");
    assert!(
        d.contains("Subject: Changes to 2 watched entities in ds\r\n"),
        "{d}"
    );
    assert!(d.contains("Event: entity.changed\r\n"), "{d}");
    assert!(d.contains("urn:a") && d.contains("urn:b"), "{d}");
    assert!(
        !d.contains(&format!("\"commit\": {}", third - 1)),
        "own commit mailed: {d}"
    );
}

#[tokio::test]
async fn nothing_waits_while_no_channel_carries_watches() {
    let mut s = server();
    let a = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    s.update("alice", "main", &label("urn:a", "A")).await;
    assert_eq!(s.matched("bob", a["id"].as_str().unwrap()).await.len(), 1);
    assert!(s.st.watches.batches.waiting().is_empty());
}

#[tokio::test]
async fn a_batch_is_built_again_from_the_watches_at_each_attempt() {
    let mut s = server();
    mailing(&s, 9).await;
    let a = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let id = a["id"].as_str().unwrap().to_string();
    s.update("alice", "main", &label("urn:a", "A")).await;
    s.follow().await;
    let b = s.st.watches.batches.waiting().pop().unwrap();
    let (env, to) = build(&s, &b, Utc::now()).unwrap();
    assert_eq!(to, "bob@example.org");
    assert_eq!(env["type"], "entity.changed");
    assert_eq!(env["id"], "id-1");
    assert_eq!(env["dataset"], "ds");
    assert_eq!(env["data"]["watches"][0]["entity"], "urn:a");
    // the watch is removed before the attempt: nothing is left to send
    let (st, _) = s
        .call("DELETE", &format!("/$/watches/{id}"), "bob", J::Null)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert!(build(&s, &b, Utc::now()).is_err());
}

#[tokio::test]
async fn a_user_without_an_address_is_not_mailed() {
    let mut s = server();
    mailing(&s, 9).await;
    // rita reads only dev and has no address in the configuration
    let main = s.st.datasets()["ds"].clone();
    main.store
        .create_branch("dev", &Default::default())
        .unwrap();
    let mut n = new_watch(&s, G, "urn:a");
    n.branch = "dev".into();
    n.branch_id = main.store.branch_id_of("dev").unwrap();
    s.st.watches
        .create(&s.st, &user("rita"), n, Utc::now())
        .unwrap();
    s.update("alice", "dev", &label("urn:a", "A")).await;
    s.follow().await;
    let b = s.st.watches.batches.waiting().pop().unwrap();
    let e = build(&s, &b, Utc::now()).unwrap_err();
    assert!(e.contains("no email"), "{e}");
}

#[tokio::test]
async fn a_proxy_account_is_mailed_only_at_a_verified_address_while_it_is_current() {
    let mut s = server();
    mailing(&s, 9).await;
    // the proxy vouches for pam, in staff, with a verified address
    assert_eq!(
        s.proxied("/$/ping", "pam", "staff", Some("pam@example.org"))
            .await,
        StatusCode::OK
    );
    let (w, _) =
        s.st.watches
            .create(
                &s.st,
                &Owner::named(Kind::Proxy, "pam"),
                new_watch(&s, G, "urn:a"),
                Utc::now(),
            )
            .unwrap();
    assert_eq!(w.owner.groups, ["staff"]);
    assert_eq!(w.owner.email.as_deref(), Some("pam@example.org"));
    s.update("alice", "main", &label("urn:a", "A")).await;
    s.follow().await;
    let b = s.st.watches.batches.waiting().pop().unwrap();
    let now = Utc::now();
    assert_eq!(build(&s, &b, now).unwrap().1, "pam@example.org");
    // not confirmed within identityMaxAgeDays (30)
    let e = build(&s, &b, now + chrono::TimeDelta::days(31)).unwrap_err();
    assert!(e.contains("30 days"), "{e}");
    // a request without the address header: no verified address any more
    s.proxied("/$/ping", "pam", "staff", None).await;
    let e = build(&s, &b, now).unwrap_err();
    assert!(e.contains("no verified address"), "{e}");
    // an account name that looks like an address is never used
    s.proxied("/$/ping", "pam", "staff", Some("not an address"))
        .await;
    assert!(build(&s, &b, now).is_err());
}

/// Server review finding 7: groups that a request on another route takes away are
/// recorded on the watches at once, so they authorize neither a waiting mail nor
/// later matches.
#[tokio::test]
async fn groups_a_proxy_takes_away_end_mail_and_matches() {
    let mut s = server();
    mailing(&s, 9).await;
    s.proxied("/$/ping", "pam", "staff", Some("pam@example.org"))
        .await;
    let (w, _) =
        s.st.watches
            .create(
                &s.st,
                &Owner::named(Kind::Proxy, "pam"),
                new_watch(&s, G, "urn:a"),
                Utc::now(),
            )
            .unwrap();
    s.update("alice", "main", &label("urn:a", "A")).await;
    s.follow().await;
    let b = s.st.watches.batches.waiting().pop().unwrap();
    assert!(build(&s, &b, Utc::now()).is_ok());
    // pam leaves staff, as a request to an unrelated route asserts
    s.proxied("/$/ping", "pam", "others", Some("pam@example.org"))
        .await;
    let e = build(&s, &b, Utc::now()).unwrap_err();
    assert!(e.contains("readable"), "{e}");
    s.update("alice", "main", &label("urn:a", "A2")).await;
    s.follow().await;
    let (ws, _, _) = s.st.watches.snapshot(&s.st).unwrap();
    let w = ws.iter().find(|x| x.id == w.id).unwrap();
    assert_eq!(w.owner.groups, ["others"]);
    assert_eq!(
        w.matches.len(),
        1,
        "a match under groups that were taken away"
    );
}

/// Review finding email-ui-r1 1: a dataset that has not opened yet may hold the branch.
#[tokio::test]
async fn a_watch_is_kept_while_a_dataset_has_not_opened() {
    let mut s = server();
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    assert!(s.st.delete("ds").unwrap());
    s.st.hold_for_startup("late", DbType::Persistent);
    s.follow().await;
    let kept = |s: &Srv| {
        s.st.watches
            .snapshot(&s.st)
            .unwrap()
            .0
            .iter()
            .any(|x| x.id == id)
    };
    assert!(kept(&s), "removed while a dataset had not opened");
    s.st.release_startup();
    s.follow().await;
    assert!(!kept(&s));
}

/// Review finding email-ui-r1 2: a vouching that comes while a watch is made reaches the
/// new watch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vouching_during_creation_reaches_the_new_watch() {
    let s = server();
    s.proxied("/$/ping", "pam", "staff", Some("pam@example.org"))
        .await;
    let st = s.st.clone();
    let handle: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
    let h = handle.clone();
    *s.st.watches.before_create.lock() = Some(Box::new(move || {
        let st = st.clone();
        *h.lock() = Some(std::thread::spawn(move || {
            let who = Identity {
                kind: Kind::Proxy,
                name: "pam".into(),
                groups: vec!["others".into()],
                display_name: None,
            };
            st.watches
                .vouched(&st, &who, true, &EmailClaim::Unverified, Utc::now());
        }));
        std::thread::sleep(Duration::from_millis(200));
    }));
    let n = new_watch(&s, G, "urn:a");
    let st = s.st.clone();
    let (w, _) = tokio::task::spawn_blocking(move || {
        st.watches
            .create(&st, &Owner::named(Kind::Proxy, "pam"), n, Utc::now())
            .unwrap()
    })
    .await
    .unwrap();
    *s.st.watches.before_create.lock() = None;
    let t = handle.lock().take().unwrap();
    tokio::task::spawn_blocking(move || t.join().unwrap())
        .await
        .unwrap();
    let (ws, _, _) = s.st.watches.snapshot(&s.st).unwrap();
    let saved = ws.iter().find(|x| x.id == w.id).unwrap();
    assert_eq!(saved.owner.groups, ["others"]);
    assert_eq!(saved.owner.email, None);
}

/// Review finding email-ui-r1 3: a revocation that cannot be saved still ends mail at
/// once, and the next vouching saves it.
#[cfg(unix)]
#[tokio::test]
async fn a_revocation_that_cannot_be_saved_still_ends_mail() {
    use std::os::unix::fs::PermissionsExt;
    let mut s = server();
    mailing(&s, 9).await;
    s.proxied("/$/ping", "pam", "staff", Some("pam@example.org"))
        .await;
    s.st.watches
        .create(
            &s.st,
            &Owner::named(Kind::Proxy, "pam"),
            new_watch(&s, G, "urn:a"),
            Utc::now(),
        )
        .unwrap();
    s.update("alice", "main", &label("urn:a", "A")).await;
    s.follow().await;
    let b = s.st.watches.batches.waiting().pop().unwrap();
    assert!(build(&s, &b, Utc::now()).is_ok());
    let dir = s.st.data_dir.clone();
    let mode = |m| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o500);
    s.proxied("/$/ping", "pam", "others", Some("pam@example.org"))
        .await;
    let e = build(&s, &b, Utc::now());
    mode(0o700);
    assert!(e.is_err(), "mailed under groups that were taken away");
    let file = || std::fs::read_to_string(s.st.data_dir.join(FILE)).unwrap();
    assert!(!file().contains("others"));
    s.proxied("/$/ping", "pam", "others", Some("pam@example.org"))
        .await;
    assert!(
        file().contains("others"),
        "the revocation was not saved again"
    );
}

/// Review finding email-ui-r2 1: words that could not be recorded outlast a full
/// memory of vouchings.
#[cfg(unix)]
#[tokio::test]
async fn an_unrecorded_revocation_outlasts_a_full_memory() {
    use std::os::unix::fs::PermissionsExt;
    let mut s = server();
    mailing(&s, 9).await;
    s.proxied("/$/ping", "pam", "staff", Some("pam@example.org"))
        .await;
    s.st.watches
        .create(
            &s.st,
            &Owner::named(Kind::Proxy, "pam"),
            new_watch(&s, G, "urn:a"),
            Utc::now(),
        )
        .unwrap();
    s.update("alice", "main", &label("urn:a", "A")).await;
    s.follow().await;
    let b = s.st.watches.batches.waiting().pop().unwrap();
    let dir = s.st.data_dir.clone();
    let mode = |m| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o500);
    s.proxied("/$/ping", "pam", "others", Some("pam@example.org"))
        .await;
    {
        // owners whose words could not be recorded either fill the memory
        let mut m = s.st.watches.vouched.lock();
        for i in m.len()..VOUCHED_MAX {
            let owner = Owner::named(Kind::Proxy, &format!("p{i}"));
            m.insert(
                owner.key(),
                Vouch {
                    owner,
                    written: None,
                    pending: true,
                },
            );
        }
    }
    s.proxied("/$/ping", "quinn", "staff", None).await;
    let e = build(&s, &b, Utc::now());
    mode(0o700);
    assert!(e.is_err(), "mailed under groups that were taken away");
    s.update("alice", "main", &label("urn:a", "A2")).await;
    s.follow().await;
    let (ws, _, _) = s.st.watches.snapshot(&s.st).unwrap();
    let w = ws.iter().find(|x| x.owner.name == "pam").unwrap();
    assert_eq!(
        w.matches.len(),
        1,
        "a match under groups that were taken away"
    );
    s.proxied("/$/ping", "pam", "others", Some("pam@example.org"))
        .await;
    let file = std::fs::read_to_string(s.st.data_dir.join(FILE)).unwrap();
    assert!(file.contains("others"), "the revocation was not recorded");
}

/// Review finding email-ui-r2 3: retrying a record that failed does not make the
/// follower authorize again.
#[cfg(unix)]
#[tokio::test]
async fn a_retried_record_leaves_the_follower_alone() {
    use std::os::unix::fs::PermissionsExt;
    let s = server();
    s.proxied("/$/ping", "pam", "staff", Some("pam@example.org"))
        .await;
    s.st.watches
        .create(
            &s.st,
            &Owner::named(Kind::Proxy, "pam"),
            new_watch(&s, G, "urn:a"),
            Utc::now(),
        )
        .unwrap();
    let dir = s.st.data_dir.clone();
    let mode = |m| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(m)).unwrap();
    let file = || std::fs::read_to_string(s.st.data_dir.join(FILE)).unwrap();
    mode(0o500);
    let v0 = s.st.watches.version();
    s.proxied("/$/ping", "pam", "others", Some("pam@example.org"))
        .await;
    let v1 = s.st.watches.version();
    assert!(v1 > v0, "the follower did not hear of new groups");
    s.proxied("/$/ping", "pam", "others", Some("pam@example.org"))
        .await;
    assert_eq!(s.st.watches.version(), v1, "a retry woke the follower");
    assert!(!file().contains("others"));
    mode(0o700);
    s.proxied("/$/ping", "pam", "others", Some("pam@example.org"))
        .await;
    assert!(file().contains("others"), "the record was not retried");
    assert_eq!(s.st.watches.version(), v1, "a retry woke the follower");
}

/// Review finding email-ui-r2 2: a deletion or rename that starts while the follower
/// reads the catalog removes no watch. The catalog's own window, where a dataset that
/// is renamed is in no list, cannot be paused from here, so the test starts and ends
/// a deletion inside the follower's reading instead.
#[tokio::test]
async fn a_rename_during_the_catalog_read_removes_no_watch() {
    let mut s = server();
    let w = s
        .watch(
            "bob",
            json!({"dataset": "ds", "graph": G, "entity": "urn:a"}),
        )
        .await;
    let id = w["id"].as_str().unwrap().to_string();
    assert!(s.st.delete("ds").unwrap());
    let st = s.st.clone();
    let mut once = true;
    *s.st.watches.before_scan.lock() = Some(Box::new(move || {
        if std::mem::take(&mut once) {
            drop(st.leave_for_tests("elsewhere"));
        }
    }));
    let kept = |s: &Srv| {
        s.st.watches
            .snapshot(&s.st)
            .unwrap()
            .0
            .iter()
            .any(|x| x.id == id)
    };
    s.follow().await;
    assert!(kept(&s), "removed while a rename moved a dataset");
    s.follow().await;
    *s.st.watches.before_scan.lock() = None;
    assert!(!kept(&s));
}

#[test]
fn routes_never_carry_watch_mail() {
    let check = |v: J| crate::notify::config::parse(&v).and_then(|c| c.validate());
    let ch = |to: J| json!({"type": "email", "host": "mail.example.org", "from": "s@example.org", "to": to, "watchers": true});
    assert!(check(json!({"channels": {"w": ch(json!([]))}})).is_ok());
    let e = check(json!({"channels": {"w": ch(json!([]))}, "routes": {"*": ["w"]}})).unwrap_err();
    assert!(e.contains("only carries watches"), "{e}");
    for key in ["entity.changed", "entity.*"] {
        let e =
            check(json!({"channels": {"w": ch(json!(["a@example.org"]))}, "routes": {key: ["w"]}}))
                .unwrap_err();
        assert!(e.contains("owners of watches"), "{e}");
    }
    let all = crate::notify::config::parse(&json!({
        "channels": {"w": ch(json!(["a@example.org"]))}, "routes": {"*": ["w"]}
    }))
    .unwrap();
    assert_eq!(all.route("backup.failed"), ["w"]);
    assert!(all.route("entity.changed").is_empty());
    let plain =
        json!({"type": "email", "host": "mail.example.org", "from": "s@example.org", "to": []});
    assert!(check(json!({"channels": {"p": plain}})).is_err());
}
