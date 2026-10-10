//! Per-dataset files of spec C18 Phase 1: the memory settings of §8.8 in
//! `<db>/memory.json` (`GET`, `PUT /$/memory/{ds}`), and the suggested examples in
//! `<db>/query-suggestions.json` (`GET`, `POST`, `DELETE /$/queries/{ds}/suggestions`).
//!
//! An in-memory dataset keeps both in the process ([`Volatile`]). Both files are small
//! JSON documents written atomically, and both are changed only through these routes.

use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, blocking, dataset, err, err_code};
use crate::state::{AppState, Dataset};
use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub const MEMORY_FILE: &str = "memory.json";
pub const SUGGESTIONS_FILE: &str = "query-suggestions.json";
/// The most suggestions a dataset keeps.
pub const MAX_SUGGESTIONS: usize = 500;
/// The most agent graph patterns.
const MAX_AGENT_GRAPHS: usize = 50;

type St = State<Arc<AppState>>;

/// The files of in-memory datasets, by dataset name and file name.
#[derive(Default)]
pub struct Volatile(Mutex<HashMap<(String, &'static str), Value>>);

/// How an agent's conversation facts are written (§8.8).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationFacts {
    #[default]
    Immediate,
    Review,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentPolicy {
    #[serde(default)]
    pub conversation_facts: ConversationFacts,
}

/// `memory.json` (§8.8).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MemorySettings {
    /// graph IRIs or `*` patterns of agent memory: facts asserted only there are
    /// unreviewed
    #[serde(default)]
    pub agent_graphs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consolidated_graph: Option<String>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentPolicy>,
    /// the imports of harness memory (§8.10.2)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imports: Option<Imports>,
}

/// Who extracts facts from imported prose (§8.10.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Extract {
    #[default]
    Agent,
    Server,
    None,
}

/// A redaction pattern of the dataset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretPattern {
    pub name: String,
    pub regex: String,
}

/// `imports` of `memory.json` (§8.10.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Imports {
    /// the prefix of every import graph
    pub base: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_patterns: Vec<SecretPattern>,
    #[serde(default)]
    pub transcripts: bool,
    #[serde(default)]
    pub extract: Extract,
}

impl MemorySettings {
    /// Whether facts of `graph` are agent memory.
    pub fn is_agent_graph(&self, graph: &str) -> bool {
        self.agent_graphs
            .iter()
            .any(|p| crate::auth::glob(p, graph))
    }

    fn validate(&self) -> Result<(), String> {
        if self.agent_graphs.len() > MAX_AGENT_GRAPHS {
            return Err(format!("agentGraphs: at most {MAX_AGENT_GRAPHS} patterns"));
        }
        let iri_like = |s: &str| {
            let t = s.replace('*', "x");
            oxrdf::NamedNode::new(&t).is_ok()
        };
        for g in &self.agent_graphs {
            if g.is_empty() || !iri_like(g) {
                return Err(format!(
                    "agentGraphs: {g:?} is not a graph IRI or an IRI pattern with *"
                ));
            }
        }
        if let Some(c) = &self.consolidated_graph {
            if oxrdf::NamedNode::new(c).is_err() {
                return Err(format!("consolidatedGraph: {c:?} is not an IRI"));
            }
            if self.is_agent_graph(c) {
                return Err("consolidatedGraph must not match agentGraphs".into());
            }
        }
        if let Some(im) = &self.imports {
            im.validate(self)?;
        }
        for name in self.agents.keys() {
            if name.is_empty() || name.len() > 200 {
                return Err(format!("agents: invalid agent name {name:?}"));
            }
        }
        Ok(())
    }
}

impl Imports {
    fn validate(&self, s: &MemorySettings) -> Result<(), String> {
        if oxrdf::NamedNode::new(format!("{}x", self.base)).is_err()
            || !(self.base.ends_with('/') || self.base.ends_with('#'))
        {
            return Err(format!(
                "imports.base: {:?} is not an IRI that ends in / or #",
                self.base
            ));
        }
        // imported facts stay unreviewed until a person promotes them
        if !s.is_agent_graph(&format!("{}x", self.base)) {
            return Err(format!(
                "imports.base: agentGraphs must match the import graphs; add {:?}",
                format!("{}*", self.base)
            ));
        }
        if self.secret_patterns.len() > 100 {
            return Err("imports.secretPatterns: at most 100 patterns".into());
        }
        for p in &self.secret_patterns {
            if p.name.is_empty()
                || p.name.len() > 64
                || !p
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            {
                return Err(format!(
                    "imports.secretPatterns: invalid name {:?}: use 1 to 64 letters, digits, - or _",
                    p.name
                ));
            }
            if let Err(e) = regex::Regex::new(&p.regex) {
                return Err(format!("imports.secretPatterns: {}: {e}", p.name));
            }
        }
        Ok(())
    }
}

fn read_file(st: &AppState, ds: &Dataset, file: &'static str) -> Result<Option<Value>> {
    match ds.store.root() {
        Some(root) => match std::fs::read(root.join(file)) {
            Ok(b) => Ok(Some(
                serde_json::from_slice(&b).with_context(|| format!("{file} of {}", ds.name))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {file} of {}", ds.name)),
        },
        None => Ok(st.volatile.0.lock().get(&(ds.name.clone(), file)).cloned()),
    }
}

fn write_file(st: &AppState, ds: &Dataset, file: &'static str, v: &Value) -> Result<()> {
    match ds.store.root() {
        Some(root) => {
            let bytes = serde_json::to_vec_pretty(v)?;
            sparkles::guard::config::write_atomic(&root.join(file), &bytes)
                .with_context(|| format!("writing {file} of {}", ds.name))
        }
        None => {
            st.volatile
                .0
                .lock()
                .insert((ds.name.clone(), file), v.clone());
            Ok(())
        }
    }
}

/// The memory settings of a dataset (the defaults without a file, or with one that
/// cannot be read, which is logged).
pub fn memory_settings(st: &AppState, ds: &Dataset) -> MemorySettings {
    match read_file(st, ds, MEMORY_FILE) {
        Ok(Some(v)) => serde_json::from_value(v).unwrap_or_else(|e| {
            tracing::warn!(dataset = %ds.name, "{MEMORY_FILE}: {e}");
            MemorySettings::default()
        }),
        Ok(None) => MemorySettings::default(),
        Err(e) => {
            tracing::warn!(dataset = %ds.name, "{e:#}");
            MemorySettings::default()
        }
    }
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/memory/{ds}", get(get_memory).put(put_memory))
        .route(
            "/$/queries/{ds}/suggestions",
            get(list_suggestions)
                .post(suggest)
                .delete(delete_suggestion),
        )
}

async fn get_memory(State(st): St, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let ds = dataset(&st, &name)?;
    Ok(Json(
        serde_json::to_value(memory_settings(&st, &ds)).unwrap_or_default(),
    ))
}

async fn put_memory(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<Value>> {
    let ds = dataset(&st, &name)?;
    let s: MemorySettings =
        serde_json::from_slice(&body).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    s.validate().map_err(|m| err(StatusCode::BAD_REQUEST, m))?;
    let v = serde_json::to_value(&s).unwrap_or_default();
    blocking(move || {
        write_file(&st, &ds, MEMORY_FILE, &v)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        Ok(Json(v))
    })
    .await
}

/// One suggested example.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Suggestion {
    id: String,
    question: String,
    query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    explanation: Option<String>,
    by: String,
    at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuggestBody {
    question: String,
    query: String,
    explanation: Option<String>,
}

fn suggestions_of(st: &AppState, ds: &Dataset) -> Result<Vec<Suggestion>> {
    Ok(match read_file(st, ds, SUGGESTIONS_FILE)? {
        Some(v) => serde_json::from_value(v.get("suggestions").cloned().unwrap_or_default())
            .with_context(|| format!("{SUGGESTIONS_FILE} of {}", ds.name))?,
        None => Vec::new(),
    })
}

fn store_suggestions(st: &AppState, ds: &Dataset, list: &[Suggestion]) -> Result<()> {
    write_file(
        st,
        ds,
        SUGGESTIONS_FILE,
        &json!({ "format": 1, "suggestions": list }),
    )
}

/// Serializes the read-modify-write of suggestion files.
static SUGGESTIONS_LOCK: Mutex<()> = Mutex::new(());

async fn list_suggestions(State(st): St, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let ds = dataset(&st, &name)?;
    blocking(move || {
        let mut list = suggestions_of(&st, &ds)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        list.reverse();
        Ok(Json(json!({ "dataset": ds.name, "suggestions": list })))
    })
    .await
}

async fn suggest(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(body): AdminBody,
) -> ApiResult<Response> {
    let ds = dataset(&st, &name)?;
    let b: SuggestBody =
        serde_json::from_slice(&body).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    let question = b.question.trim().to_string();
    if question.is_empty() || question.chars().count() > 2000 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "question must hold 1 to 2000 characters",
        ));
    }
    if b.query.trim().is_empty() || b.query.chars().count() > 65536 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "query must hold 1 to 65536 characters",
        ));
    }
    if b.explanation
        .as_ref()
        .is_some_and(|e| e.chars().count() > 400)
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "explanation must be at most 400 characters",
        ));
    }
    let mut prefixes = sparkles::io::standard_prefixes();
    prefixes.extend(ds.store.prefixes());
    let pv: Vec<(String, String)> = prefixes.into_iter().collect();
    if let Err(e) = sparkles::sparql::parse_query(&b.query, None, &pv) {
        return Err(
            if sparkles::sparql::update::parse_update(
                &b.query,
                &sparkles::sparql::QueryOptions {
                    prefixes: pv.clone(),
                    ..Default::default()
                },
            )
            .is_ok()
            {
                err_code(
                    StatusCode::BAD_REQUEST,
                    "not-a-query",
                    "this is SPARQL Update; only queries can be examples",
                )
            } else {
                err_code(StatusCode::BAD_REQUEST, "syntax", e.to_string())
            },
        );
    }
    let s = Suggestion {
        id: uuid::Uuid::new_v4().simple().to_string()[..16].to_string(),
        question,
        query: b.query,
        explanation: b.explanation.filter(|e| !e.trim().is_empty()),
        by: p.id(),
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    blocking(move || {
        let _g = SUGGESTIONS_LOCK.lock();
        let mut list = suggestions_of(&st, &ds)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        if list.len() >= MAX_SUGGESTIONS {
            return Err(err_code(
                StatusCode::CONFLICT,
                "too-many-suggestions",
                format!(
                    "dataset {} already has {MAX_SUGGESTIONS} suggestions; an administrator must review them first",
                    ds.name
                ),
            ));
        }
        list.push(s.clone());
        store_suggestions(&st, &ds, &list)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        Ok((StatusCode::CREATED, Json(s)).into_response())
    })
    .await
}

#[derive(Deserialize)]
struct IdParam {
    id: String,
}

async fn delete_suggestion(
    State(st): St,
    Path(name): Path<String>,
    Query(q): Query<IdParam>,
) -> ApiResult<StatusCode> {
    let ds = dataset(&st, &name)?;
    blocking(move || {
        let _g = SUGGESTIONS_LOCK.lock();
        let mut list = suggestions_of(&st, &ds)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        let before = list.len();
        list.retain(|s| s.id != q.id);
        if list.len() == before {
            return Err(err_code(
                StatusCode::NOT_FOUND,
                "unknown-suggestion",
                format!("no suggestion {:?}", q.id),
            ));
        }
        store_suggestions(&st, &ds, &list)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
        let res = app.clone().oneshot(req).await.unwrap();
        let s = res.status();
        let b = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    fn json_req(method: &str, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn memory_settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(
            AppState::new(
                dir.path(),
                sparkles::store::StoreOptions::default(),
                std::time::Duration::from_secs(30),
            )
            .unwrap(),
        );
        let ds = st.create("org", crate::state::DbType::Persistent).unwrap();
        let app = crate::http::router(st.clone());
        let (s, v) = send(
            &app,
            Request::get("/$/memory/org").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v, json!({"agentGraphs": [], "agents": {}}));
        let set = json!({
            "agentGraphs": ["https://example.org/memory/agents/*"],
            "consolidatedGraph": "https://example.org/memory/consolidated",
            "agents": {"agent-7": {"conversationFacts": "immediate"}}
        });
        let (s, v) = send(&app, json_req("PUT", "/$/memory/org", set.clone())).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v, set);
        assert!(ds.store.root().unwrap().join(MEMORY_FILE).exists());
        let m = memory_settings(&st, &ds);
        assert!(m.is_agent_graph("https://example.org/memory/agents/agent-7/sessions/s1"));
        assert!(!m.is_agent_graph("https://example.org/hr"));
        for bad in [
            json!({"agentGraphs": ["not an iri"]}),
            json!({"agentGraphs": [], "unknown": 1}),
            json!({"agentGraphs": ["https://x.example/*"], "consolidatedGraph": "https://x.example/c"}),
            json!({"agents": {"a": {"conversationFacts": "later"}}}),
        ] {
            let (s, _) = send(&app, json_req("PUT", "/$/memory/org", bad.clone())).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn suggestions() {
        let st = Arc::new(AppState::standalone(
            sparkles::store::StoreOptions::default(),
            std::time::Duration::from_secs(30),
        ));
        st.attach("org", crate::state::DbType::Mem, None).unwrap();
        let app = crate::http::router(st.clone());
        let q = "SELECT ?p WHERE { ?p ?q ?o }";
        let (s, v) = send(
            &app,
            json_req(
                "POST",
                "/$/queries/org/suggestions",
                json!({"question": "Who is on the payments team?", "query": q}),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        assert_eq!(v["by"], "local");
        let id = v["id"].as_str().unwrap().to_string();
        let (s, v) = send(
            &app,
            json_req(
                "POST",
                "/$/queries/org/suggestions",
                json!({"question": "x", "query": "INSERT DATA { <urn:a> <urn:b> <urn:c> }"}),
            ),
        )
        .await;
        assert_eq!(
            (s, v["code"].clone()),
            (StatusCode::BAD_REQUEST, json!("not-a-query"))
        );
        let (s, _) = send(
            &app,
            json_req(
                "POST",
                "/$/queries/org/suggestions",
                json!({"question": "", "query": q}),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, v) = send(
            &app,
            Request::get("/$/queries/org/suggestions")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            v["suggestions"][0]["question"],
            "Who is on the payments team?"
        );
        let (s, _) = send(
            &app,
            Request::delete(format!("/$/queries/org/suggestions?id={id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _) = send(
            &app,
            Request::delete(format!("/$/queries/org/suggestions?id={id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        // the cap
        let ds = st.get("org").unwrap();
        let many: Vec<Suggestion> = (0..MAX_SUGGESTIONS)
            .map(|i| Suggestion {
                id: format!("{i}"),
                question: "q".into(),
                query: q.into(),
                explanation: None,
                by: "x".into(),
                at: "2026-10-09T00:00:00Z".into(),
            })
            .collect();
        store_suggestions(&st, &ds, &many).unwrap();
        let (s, v) = send(
            &app,
            json_req(
                "POST",
                "/$/queries/org/suggestions",
                json!({"question": "q", "query": q}),
            ),
        )
        .await;
        assert_eq!(
            (s, v["code"].clone()),
            (StatusCode::CONFLICT, json!("too-many-suggestions"))
        );
    }
}
