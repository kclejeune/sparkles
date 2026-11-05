//! Evidence-profile settings and source holds in the server (spec C25 §3 and §7):
//! `GET` and `PUT /$/evidence/{ds}/settings` read and record a dataset's evidence
//! settings, `POST /$/evidence/{ds}/detach` stops protecting one graph of their roles,
//! and `GET` and `POST /$/evidence/{ds}/holds` and `DELETE
//! /$/evidence/{ds}/holds/{id}` list, acquire and release durable source holds.
//! `POST /{ds}/claims/resolve` verifies evidence references at their exact retained
//! views and records by their publication origin, and `POST /{ds}/claims` publishes
//! authenticated claim proposals (see [`claims`]).
//!
//! Authorization (the route table in `auth/routes.rs`): every `/$/evidence` route needs
//! `admin` on `{ds}`. The settings name the dataset's protected graphs and policies,
//! and holds name commits of any branch and decide what history is retained. The
//! resolution route needs `read` on `{ds}` through the query endpoint, and each stored
//! fact needs it on the dataset and branch the reference names, under their current
//! names. A reference the caller cannot read answers as a missing one does.

use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, blocking, dataset, err};
use crate::state::AppState;
use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value as J, json};
use sparkles::evidence::SettingsStatus;
use sparkles::task::Control;
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/$/evidence/{ds}/settings",
            get(get_settings).put(put_settings),
        )
        .route("/$/evidence/{ds}/detach", post(detach))
        .route("/$/evidence/{ds}/holds", get(list_holds).post(acquire_hold))
        .route("/$/evidence/{ds}/holds/{id}", delete(release_hold))
        .route("/{ds}/claims/resolve", post(resolve))
        .route("/{ds}/claims", post(claims::propose))
        .route("/{ds}/claims/review", post(review::review))
        .route("/{ds}/claims/withdraw", post(review::withdraw))
        .route("/{ds}/claims/identities", post(review::identities))
        .route("/{ds}/claims/dependents", post(review::dependents))
}

mod claims;
mod review;

/// The response body: the canonical settings, their digest, the registry revision and
/// the role graphs that are protected or detached.
fn body(status: SettingsStatus) -> ApiResult<Json<J>> {
    let settings = status.settings.canonical(&Control::none())?;
    Ok(Json(json!({
        "settings": settings.value(),
        "digest": status.digest,
        "revision": status.revision,
        "managed": status.managed,
        "detached": status.detached,
    })))
}

/// `GET /$/evidence/{ds}/settings`: the recorded settings, or the disabled defaults.
async fn get_settings(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    blocking(move || body(sparkles::evidence::settings(&ds.dataset)?)).await
}

/// `PUT /$/evidence/{ds}/settings`: record complete settings. A configuration that
/// would stop protecting a graph, or that races with another registry change, is a
/// `409` that writes nothing.
async fn put_settings(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        let control = Control::none();
        let settings = sparkles::evidence::parse_settings(&bytes, &control)?;
        body(sparkles::evidence::configure(
            &ds.dataset,
            &settings,
            &control,
        )?)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Detach {
    graph: String,
}

/// `POST /$/evidence/{ds}/detach` with `{"graph": IRI}`: ordinary writes can change the
/// graph until the settings are recorded again.
async fn detach(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let request: Detach = serde_json::from_slice(&bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid detach request: {e}"),
        )
    })?;
    let ds = dataset(&st, &name)?;
    blocking(move || body(sparkles::evidence::detach(&ds.dataset, &request.graph)?)).await
}

// ------------------------------------------------------------------ holds ------

/// The hold routes name commits of every branch, so they need `admin` on all of the
/// dataset, not only on `main`.
fn admin_everywhere(p: &Principal, ds: &str) -> ApiResult<()> {
    if p.admin_everywhere(ds) {
        return Ok(());
    }
    Err(err(
        StatusCode::FORBIDDEN,
        format!("source holds need admin on every branch and graph of /{ds}"),
    ))
}

/// The most holds one page lists.
const MAX_PAGE: usize = 1000;

/// A branch's name by its id, `None` for a deleted one.
fn branch_names(
    store: &sparkles::store::Store,
) -> ApiResult<std::collections::HashMap<uuid::Uuid, String>> {
    let mut names = std::collections::HashMap::new();
    names.insert(store.dataset_id(), sparkles::branch::MAIN.to_string());
    if store.branch_set().is_some() {
        for b in store.branches()? {
            names.insert(b.id, b.name);
        }
    }
    Ok(names)
}

fn hold_json(
    h: &sparkles::evidence::SourceHold,
    names: &std::collections::HashMap<uuid::Uuid, String>,
) -> J {
    json!({
        "id": h.id,
        "owner": h.owner.as_str(),
        "created": sparkles::commit::rfc3339_ms(h.created_ms),
        "commits": h.commits.iter().map(|c| json!({
            "branch": names.get(&c.branch_id),
            "branchId": c.branch_id,
            "commit": c.seq,
        })).collect::<Vec<_>>(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListHolds {
    /// only the holds that name this branch (not `branch`, which selects the branch a
    /// request acts on)
    on_branch: Option<String>,
    /// the id after which the page starts
    after: Option<uuid::Uuid>,
    limit: Option<usize>,
}

/// `GET /$/evidence/{ds}/holds`: the evidence holds by id, a page at a time.
async fn list_holds(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    Query(q): Query<ListHolds>,
) -> ApiResult<Json<J>> {
    admin_everywhere(&p, &name)?;
    let ds = dataset(&st, &name)?;
    let limit = q.limit.unwrap_or(MAX_PAGE);
    if limit == 0 || limit > MAX_PAGE {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("limit must be between 1 and {MAX_PAGE}"),
        ));
    }
    blocking(move || {
        let store = ds.dataset.store();
        let names = branch_names(store)?;
        let branch = match &q.on_branch {
            None => None,
            Some(b) => Some(branch_id(store, b)?),
        };
        let all = sparkles::evidence::holds(&ds.dataset)?;
        let held: usize = all.iter().map(|h| h.commits.len()).sum();
        let mut page: Vec<J> = Vec::new();
        let mut next = None;
        for h in all.iter().filter(|h| {
            q.after.is_none_or(|a| h.id > a)
                && branch.is_none_or(|b| h.commits.iter().any(|c| c.branch_id == b))
        }) {
            if page.len() == limit {
                next = page.last().map(|p: &J| p["id"].clone());
                break;
            }
            page.push(hold_json(h, &names));
        }
        Ok(Json(json!({
            "holds": page,
            "next": next,
            "heldCommits": held,
            "maxCommits": sparkles::evidence::MAX_COMMITS,
        })))
    })
    .await
}

/// The id of branch `name` of `store`'s dataset.
fn branch_id(store: &sparkles::store::Store, name: &str) -> ApiResult<uuid::Uuid> {
    if name == sparkles::branch::MAIN {
        return Ok(store.dataset_id());
    }
    Ok(store.branch_info(name)?.id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HeldCommit {
    branch: Option<String>,
    branch_id: Option<uuid::Uuid>,
    commit: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Acquire {
    id: Option<uuid::Uuid>,
    commits: Vec<HeldCommit>,
}

/// `POST /$/evidence/{ds}/holds` with `{"id"?, "commits": [{"branch" | "branchId",
/// "commit"}]}`: keep the commits readable. `201` with a new hold, `200` with the
/// existing one of the same id and commits.
async fn acquire_hold(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    admin_everywhere(&p, &name)?;
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let request: Acquire = serde_json::from_slice(&bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid hold request: {e}"),
        )
    })?;
    if request.commits.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "a hold names at least one commit",
        ));
    }
    if request.commits.len() > sparkles::evidence::MAX_COMMITS {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!(
                "a hold names at most {} commits",
                sparkles::evidence::MAX_COMMITS
            ),
        ));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        let store = ds.dataset.store();
        let mut commits = Vec::with_capacity(request.commits.len());
        for c in &request.commits {
            let branch_id = match (&c.branch, c.branch_id) {
                (Some(b), None) => branch_id(store, b)?,
                (None, Some(id)) => id,
                (None, None) => branch_id(store, sparkles::branch::MAIN)?,
                (Some(_), Some(_)) => {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        "a held commit names its branch by branch or branchId, not both",
                    ));
                }
            };
            commits.push(sparkles::evidence::CommitRef {
                branch_id,
                seq: c.commit,
            });
        }
        let id = request.id.unwrap_or_else(uuid::Uuid::new_v4);
        let acquired = sparkles::evidence::acquire_hold(&ds.dataset, id, &commits)?;
        let names = branch_names(store)?;
        let status = if acquired.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        Ok((status, Json(hold_json(&acquired.hold, &names))).into_response())
    })
    .await
}

/// `DELETE /$/evidence/{ds}/holds/{id}`: release the hold; `204`, or `404` when the
/// dataset has no evidence hold of that id.
async fn release_hold(
    State(st): St,
    Path((name, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
) -> ApiResult {
    admin_everywhere(&p, &name)?;
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let id: uuid::Uuid = id
        .parse()
        .map_err(|_| err(StatusCode::BAD_REQUEST, format!("not a hold id: {id}")))?;
    let ds = dataset(&st, &name)?;
    blocking(move || {
        if sparkles::evidence::release_hold(&ds.dataset, id)? {
            Ok(StatusCode::NO_CONTENT.into_response())
        } else {
            Err(err(StatusCode::NOT_FOUND, format!("no evidence hold {id}")))
        }
    })
    .await
}

// ------------------------------------------------------------- resolution ------

use sparkles::evidence::resolve::{
    BlankAnchor, GraphIdentity, RecordKind, RecordRequest, Reference, Resolution, StoredFactRef,
    Verified,
};

/// The most bytes a resolution request body holds (spec C25 §8).
const MAX_RESOLVE_BYTES: usize = 16 << 20;
/// The most bytes of a reference key.
const MAX_KEY_BYTES: usize = 256;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveRequest {
    references: Vec<ReferenceJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReferenceJson {
    key: String,
    stored_fact: Option<StoredFactJson>,
    record: Option<RecordJson>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredFactJson {
    dataset_id: uuid::Uuid,
    branch_id: uuid::Uuid,
    commit: u64,
    /// the named graph's IRI; absent or `null` for the default graph
    graph: Option<String>,
    /// one N-Triples 1.2 triple without its final `.`
    triple: String,
    reifier: Option<String>,
    import_map: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordJson {
    iri: String,
    #[serde(default)]
    kinds: Vec<String>,
    content_digest: Option<String>,
}

fn bad(field: String, why: impl std::fmt::Display) -> crate::http::ApiError {
    crate::http::err_code(
        StatusCode::BAD_REQUEST,
        "invalid-reference",
        format!("{field}: {why}"),
    )
}

fn named(field: &str, value: &str) -> ApiResult<oxrdf::NamedNode> {
    oxrdf::NamedNode::new(value).map_err(|e| bad(field.to_string(), e))
}

/// The record kind named `name` (`SourceRevision`, `Claim`, …).
fn record_kind(name: &str) -> Option<RecordKind> {
    use RecordKind::*;
    [
        SourceRevision,
        Rendition,
        Passage,
        EvidenceRef,
        Claim,
        EvidenceLink,
        Assessment,
        Note,
        Decision,
        Task,
        ReviewDecision,
        IdentityProposal,
        AcceptanceSupport,
        Withdrawal,
        Projection,
    ]
    .into_iter()
    .find(|k| {
        sparkles::evidence::resolve::kind_iri(*k).strip_prefix("urn:x-sparkles:evidence:")
            == Some(name)
    })
}

fn kind_name(kind: RecordKind) -> &'static str {
    let iri = sparkles::evidence::resolve::kind_iri(kind);
    iri.strip_prefix("urn:x-sparkles:evidence:").unwrap_or(iri)
}

/// One N-Triples 1.2 triple, `<s> <p> <o>`, without its final dot.
fn parse_triple(field: &str, text: &str) -> ApiResult<oxrdf::Triple> {
    let line = format!("{text} .\n");
    let mut quads =
        oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::NTriples).for_slice(line.as_bytes());
    let quad = match quads.next() {
        Some(Ok(q)) => q,
        Some(Err(e)) => return Err(bad(field.to_string(), e)),
        None => return Err(bad(field.to_string(), "expected one triple")),
    };
    if quads.next().is_some() {
        return Err(bad(field.to_string(), "expected one triple"));
    }
    Ok(oxrdf::Triple::new(
        quad.subject,
        quad.predicate,
        quad.object,
    ))
}

/// A stored-fact reference of a request body, whose fields are named under `at`.
fn stored_fact_ref(at: &str, f: StoredFactJson) -> ApiResult<StoredFactRef> {
    let graph = match &f.graph {
        None => GraphIdentity::Default,
        Some(g) => GraphIdentity::Named(named(&format!("{at}.graph"), g)?),
    };
    let anchor = match (&f.reifier, &f.import_map) {
        (Some(_), Some(_)) => {
            return Err(bad(
                at.to_string(),
                "name a reifier or an import mapping, not both",
            ));
        }
        (Some(r), None) => Some(BlankAnchor::Reifier(named(&format!("{at}.reifier"), r)?)),
        (None, Some(m)) => Some(BlankAnchor::ImportMap(named(
            &format!("{at}.importMap"),
            m,
        )?)),
        (None, None) => None,
    };
    Ok(StoredFactRef {
        dataset: f.dataset_id,
        branch: f.branch_id,
        commit: f.commit,
        graph,
        expected: parse_triple(&format!("{at}.triple"), &f.triple)?,
        anchor,
    })
}
fn parse_references(request: ResolveRequest) -> ApiResult<Vec<(String, Reference)>> {
    use sparkles::evidence::resolve::MAX_REFERENCES;
    if request.references.len() > MAX_REFERENCES {
        return Err(bad(
            "references".into(),
            format!("at most {MAX_REFERENCES} references"),
        ));
    }
    let mut keys = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(request.references.len());
    for (i, r) in request.references.into_iter().enumerate() {
        let at = format!("references[{i}]");
        if r.key.is_empty() || r.key.len() > MAX_KEY_BYTES {
            return Err(bad(
                format!("{at}.key"),
                format!("a key has 1 to {MAX_KEY_BYTES} bytes"),
            ));
        }
        if !keys.insert(r.key.clone()) {
            return Err(bad(format!("{at}.key"), "keys are unique"));
        }
        let reference = match (r.stored_fact, r.record) {
            (Some(f), None) => {
                Reference::StoredFact(stored_fact_ref(&format!("{at}.storedFact"), f)?)
            }
            (None, Some(rec)) => {
                let at = format!("{at}.record");
                let mut kinds = Vec::new();
                for (j, k) in rec.kinds.iter().enumerate() {
                    let kind = record_kind(k).ok_or_else(|| {
                        bad(
                            format!("{at}.kinds[{j}]"),
                            format!("unknown record kind {k}"),
                        )
                    })?;
                    if !kinds.contains(&kind) {
                        kinds.push(kind);
                    }
                }
                let content_digest = match &rec.content_digest {
                    None => None,
                    Some(d) => Some(
                        d.parse()
                            .map_err(|e| bad(format!("{at}.contentDigest"), e))?,
                    ),
                };
                Reference::Record(RecordRequest {
                    iri: named(&format!("{at}.iri"), &rec.iri)?,
                    kinds,
                    content_digest,
                })
            }
            _ => {
                return Err(bad(
                    at,
                    "a reference has exactly one of storedFact and record",
                ));
            }
        };
        out.push((r.key, reference));
    }
    Ok(out)
}

fn graph_json(graph: &GraphIdentity) -> J {
    match graph {
        GraphIdentity::Default => J::Null,
        GraphIdentity::Named(n) => J::String(n.as_str().to_string()),
    }
}

fn verified_json(key: &str, v: &Verified) -> J {
    match v {
        Verified::Fact(f) => json!({
            "key": key,
            "storedFact": {
                "datasetId": f.dataset,
                "branchId": f.branch,
                "commit": f.commit,
                "servedBy": f.served_by,
                "graph": graph_json(&f.graph),
                "reifier": f.reifier.as_ref().map(|r| r.as_str()),
            },
        }),
        Verified::Record(r) => json!({
            "key": key,
            "record": {
                "iri": r.iri.as_str(),
                "kind": kind_name(r.kind),
                "graph": r.graph.as_str(),
                "receipt": {
                    "iri": r.receipt.iri.as_str(),
                    "datasetId": r.receipt.dataset,
                    "branchId": r.receipt.branch,
                    "commit": r.receipt.commit,
                },
                "contentDigest": r.content_digest.map(|d| d.to_string()),
                "recordDigest": r.record_digest.map(|d| d.to_string()),
                "span": r.span.as_ref().map(|s| json!({
                    "rendition": s.rendition.as_str(),
                    "start": s.start,
                    "end": s.end,
                    "textDigest": s.text_digest.to_string(),
                })),
            },
        }),
    }
}

/// `POST /{ds}/claims/resolve` with `{"references": [{"key", "storedFact" | "record"}]}`:
/// verify each reference in order. `200` with every verified reference, or the first
/// reference that does not verify with its code: `422` for `evidence-unavailable`,
/// `incomplete-evidence`, `unanchored-evidence`, `span-mismatch` and
/// `origin-not-reproduced`, and `409` for `immutable-record-conflict`. Records resolve
/// at the current state of the request's branch.
async fn resolve(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    run_http("resolve", st, name, p, bytes).await
}

fn resolve_now(
    st: Arc<AppState>,
    name: String,
    p: Principal,
    bytes: axum::body::Bytes,
    execution: Execution,
) -> ApiResult<J> {
    if bytes.len() > MAX_RESOLVE_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("a resolution request holds at most {MAX_RESOLVE_BYTES} bytes"),
        ));
    }
    let request: ResolveRequest = serde_json::from_slice(&bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid resolution request: {e}"),
        )
    })?;
    let references = parse_references(request)?;
    let branch = execution.branch.clone();
    let logical = execution.dataset(&st, &name)?;
    let main = if branch == sparkles::branch::MAIN {
        logical.clone()
    } else {
        crate::http::main_dataset(&st, &name)?
    };
    use crate::auth::{Endpoint, on_branch};
    let here = on_branch(&main.name, &branch);
    let opts = crate::http::caller::CallerContext::for_caller(&st, &logical, &p).options_on(
        &crate::http::Params::empty(),
        &here,
        Endpoint::Query,
    );
    let control = execution.control(&opts);
    let mut resolution = Resolution::new(control)?;
    // What the caller may read on a dataset and branch, by their current names. One
    // policy per dataset and branch serves the whole request, so that its references
    // share prepared views.
    type Policy = Option<Arc<sparkles::access::GraphAccess>>;
    let mut policies: std::collections::HashMap<(uuid::Uuid, String), Policy> =
        std::collections::HashMap::new();
    let mut access = |ds: &sparkles::Dataset, branch: &str| -> Policy {
        policies
            .entry((ds.dataset_id(), branch.to_string()))
            .or_insert_with(|| policy(&st, &p, ds, branch))
            .clone()
    };
    let mut resolved = Vec::with_capacity(references.len());
    for (key, reference) in &references {
        let one = [(key.clone(), reference.clone())];
        match resolution.all(
            Some(&st.catalog),
            &main.dataset,
            &logical.dataset,
            &one,
            &mut access,
        ) {
            Ok(mut v) => resolved.push(verified_json(key, &v.remove(0))),
            Err(sparkles::Error::Managed(m)) if sparkles::evidence::resolve::is_outcome(m.code) => {
                let status =
                    StatusCode::from_u16(m.status()).unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
                return Err(crate::http::err_body(
                    status,
                    json!({ "error": m.message, "code": m.code, "reference": key }),
                ));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(json!({ "resolved": resolved }))
}

/// What `p` may read on branch `branch` of `ds` through the query endpoint, by their
/// current names: `None` when it may read nothing there.
fn policy(
    st: &AppState,
    p: &Principal,
    ds: &sparkles::Dataset,
    branch: &str,
) -> Option<Arc<sparkles::access::GraphAccess>> {
    use crate::auth::{Endpoint, Level, on_branch};
    let name = ds.name()?;
    let server = st.get(name)?;
    if server.dataset.dataset_id() != ds.dataset_id() {
        return None;
    }
    let qualified = on_branch(name, branch);
    if !p.can_at(&qualified, Endpoint::Query, Level::Read) {
        return None;
    }
    let opts = crate::http::caller::CallerContext::for_caller(st, &server, p).options_on(
        &crate::http::Params::empty(),
        &qualified,
        Endpoint::Query,
    );
    Some(
        opts.graphs
            .unwrap_or_else(|| Arc::new(sparkles::access::GraphAccess::all())),
    )
}

/// Transport context captured before entering a blocking worker. Evidence keeps its
/// own shared preparation allowance; the admitted grant stays alive for the operation.
pub(crate) struct Execution {
    pub(crate) branch: String,
    pub(crate) cancel: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) deadline: Option<std::time::Instant>,
    pub(crate) grant: Option<Arc<sparkles::sparql::pool::PoolGrant>>,
}

impl Execution {
    fn dataset(&self, st: &AppState, name: &str) -> ApiResult<Arc<crate::state::Dataset>> {
        let main = crate::http::main_dataset(st, name)?;
        if self.branch == sparkles::branch::MAIN {
            Ok(main)
        } else {
            Ok(st.branch_dataset(&main, &self.branch)?)
        }
    }

    fn control(&self, opts: &sparkles::sparql::QueryOptions) -> Control {
        let server = opts.timeout.map(|t| std::time::Instant::now() + t);
        Control {
            deadline: match (server, self.deadline) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
            ..Control::with_cancel(self.cancel.clone())
        }
    }
}

/// One typed operation shared by HTTP and MCP. Both transports perform their endpoint
/// check; record access and protected writes are checked here by the existing writer.
pub(crate) fn execute(
    operation: &str,
    st: Arc<AppState>,
    name: String,
    principal: Principal,
    bytes: axum::body::Bytes,
    execution: Execution,
) -> ApiResult<J> {
    // Keep admission ownership even when the caller drops its transport future.
    let _grant = execution.grant.clone();
    match operation {
        "propose" => claims::propose_now(st, name, principal, bytes, execution),
        "review" => review::review_now(st, name, principal, bytes, execution),
        "withdraw" => review::withdraw_now(st, name, principal, bytes, execution),
        "resolve" => resolve_now(st, name, principal, bytes, execution),
        "identities" => review::identities_now(st, name, principal, bytes, execution),
        "dependents" => review::dependents_now(st, name, principal, bytes, execution),
        _ => unreachable!("unregistered evidence operation"),
    }
}

async fn run_http(
    operation: &'static str,
    st: Arc<AppState>,
    name: String,
    principal: Principal,
    bytes: axum::body::Bytes,
) -> ApiResult {
    let (cancel, _cancel_on_drop) = crate::http::cancel_on_drop();
    let execution = Execution {
        branch: crate::http::branches::current()
            .unwrap_or_else(|| sparkles::branch::MAIN.to_string()),
        cancel,
        deadline: None,
        grant: crate::server_memory::current_grant(),
    };
    match blocking(move || execute(operation, st, name, principal, bytes, execution)).await {
        Ok(value) => Ok(Json(value).into_response()),
        Err(error) if error.audit_code() == "authority-changed" => {
            Ok(crate::auth::routes::authority_changed_response())
        }
        Err(error)
            if operation == "resolve"
                && sparkles::evidence::resolve::is_outcome(error.audit_code()) =>
        {
            // Reference outcomes have always used the resolver's exact JSON body,
            // including its key, without decorating it with a per-request id.
            let (status, body) = error.into_parts();
            Ok((status, Json(body)).into_response())
        }
        Err(error) => Err(error),
    }
}
