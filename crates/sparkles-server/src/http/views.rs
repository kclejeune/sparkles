//! Incremental views (spec F13 §2.1): `/$/views/{ds}` lists a dataset's views with
//! their state, `/$/views/{ds}/{name}` defines, reads and removes one, and
//! `/$/views/{ds}/{name}/rebuild` evaluates one again. `SERVICE view:read` reads a
//! view's rows inside a query.

use super::{AdminBody, ApiError, ApiResult, St, blocking, dataset, err};
use crate::auth::{Endpoint, Principal};
use crate::state::AppState;
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value as J, json};
use sparkles::access::GraphAccess;
use sparkles::incr::{Host, ViewDef};
use std::sync::Arc;
use std::time::Duration;

/// The status of an error of the views by its code.
pub(super) fn status(code: &str) -> StatusCode {
    match code {
        "view-not-found" => StatusCode::NOT_FOUND,
        "invalid-view" | "unsupported-option" | "not-incremental" => StatusCode::BAD_REQUEST,
        "view-access" | "forbidden" => StatusCode::FORBIDDEN,
        "view-behind" | "view-ahead" | "view-suspended" | "views-unavailable" | "pass-timeout" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        "too-many-live-queries" => StatusCode::TOO_MANY_REQUESTS,
        "over-budget" => StatusCode::INSUFFICIENT_STORAGE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// How the server resolves a view's principal: a configured user's view of the
/// dataset through the query endpoint, refused unless the user may query it.
struct ServerHost {
    #[cfg(feature = "auth")]
    auth: Option<Arc<crate::auth::Auth>>,
    #[cfg(feature = "auth")]
    dataset: String,
    state: sparkles::dataset::StateReference,
}

impl Host for ServerHost {
    fn access(&self, principal: Option<&str>) -> sparkles::Result<Option<Arc<GraphAccess>>> {
        #[cfg(feature = "auth")]
        if let (Some(a), Some(name)) = (&self.auth, principal) {
            let p = a.user_principal(name).ok_or_else(|| {
                sparkles::incr::error(
                    "invalid-view",
                    format!("the view principal {name} is not a user of the access policy"),
                )
            })?;
            if !p.can_at(&self.dataset, Endpoint::Query, crate::auth::Level::Read) {
                return Err(sparkles::incr::error(
                    "invalid-view",
                    format!("the view principal {name} may not query /{}", self.dataset),
                ));
            }
            return Ok(p.view(&self.dataset, Endpoint::Query));
        }
        let _ = principal;
        Ok(None)
    }

    fn prefixes(&self) -> Vec<(String, String)> {
        self.state
            .upgrade()
            .map(|s| s.store.prefixes().into_iter().collect())
            .unwrap_or_default()
    }

    fn inference(&self) -> Option<sparkles::incr::Inference> {
        sparkles::handles::views::inference(&*self.state.upgrade()?)
    }

    fn aggregate_errors(&self) -> sparkles::sparql::AggregateErrors {
        self.state
            .upgrade()
            .map(|s| s.store.aggregate_errors())
            .unwrap_or_default()
    }

    fn stored_query(&self, name: &str) -> sparkles::Result<String> {
        let s = self
            .state
            .upgrade()
            .ok_or_else(|| sparkles::Error::invalid("the dataset is closed"))?;
        s.queries
            .get(name, None)
            .map(|q| q.definition.query)
            .ok_or_else(|| {
                sparkles::incr::error("invalid-view", format!("no stored query {name:?}"))
            })
    }

    fn epoch(&self) -> u64 {
        #[cfg(feature = "auth")]
        if let Some(a) = &self.auth {
            return a.epoch();
        }
        0
    }

    fn view_access(&self, event: &sparkles::incr::ViewAccessEvent<'_>) {
        #[cfg(feature = "auth")]
        let dataset = Some(self.dataset.as_str());
        #[cfg(not(feature = "auth"))]
        let dataset = None;
        event.log(dataset);
    }
}

/// Start the views of a dataset the server opens, with the server's principals.
pub(crate) fn start(st: &AppState, name: &str, ds: &sparkles::Dataset) {
    #[cfg(not(feature = "auth"))]
    let _ = name;
    ds.inner_state().views.set_options(st.views_options.clone());
    ds.start_views(Arc::new(ServerHost {
        #[cfg(feature = "auth")]
        auth: st.auth.clone(),
        #[cfg(feature = "auth")]
        dataset: name.to_string(),
        state: ds.state_reference(),
    }));
}

fn info_json(ds: &str, v: &sparkles::incr::ViewInfo) -> J {
    let mut j = serde_json::to_value(v).unwrap_or_default();
    j["dataset"] = ds.into();
    j
}

/// Leave out what a view's data discloses for a caller who may not read the view: its
/// row count, the size of its state, and the details of why it is suspended, which can
/// name the size of its state. `readable` is decided on the same registry entry as `j`.
fn redact(j: &mut J, readable: bool) {
    if readable {
        return;
    }
    let Some(o) = j.as_object_mut() else {
        return;
    };
    o.remove("rows");
    o.remove("stateBytes");
    if o.get("state").and_then(J::as_str) == Some("suspended")
        && let Some(reason) = o.get_mut("reason")
    {
        // keep the code that names the cause, such as `over-budget`, and drop the rest
        let code = reason
            .as_str()
            .and_then(|r| r.split_once(':'))
            .map(|(code, _)| code.trim())
            .filter(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'))
            .unwrap_or("suspended")
            .to_string();
        *reason = J::String(code);
    }
}

/// The grants a metadata route decides a caller's views by.
fn metadata_reader(ds: &crate::state::Dataset, p: &Principal) -> Option<Option<Arc<GraphAccess>>> {
    p.view_reader(&ds.name)
}

/// Whether `j` is readable for `reader` as [`metadata_reader`] gives it.
fn readable_for(reader: &Option<Option<Arc<GraphAccess>>>, readable: bool) -> bool {
    reader.is_some() && readable
}

/// `GET /$/views/{ds}`
pub(super) async fn list(
    st: St,
    Path(ds_name): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &ds_name)?;
    let reader = metadata_reader(&ds, &p);
    let captured = ds
        .dataset
        .views()
        .list_readable(reader.as_ref().and_then(|r| r.as_ref()));
    #[cfg(test)]
    hooks::after_capture(&ds.dataset.store().dataset_id().to_string());
    let views: Vec<J> = captured
        .iter()
        .map(|(v, readable)| {
            let mut j = info_json(&ds.name, v);
            if let Some(o) = j.as_object_mut() {
                o.remove("plan");
                o.remove("dataset");
            }
            redact(&mut j, readable_for(&reader, *readable));
            j
        })
        .collect();
    Ok(Json(json!({ "dataset": ds.name, "views": views })))
}

/// `GET /$/views/{ds}/{name}`
pub(super) async fn get_view(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &ds_name)?;
    let reader = metadata_reader(&ds, &p);
    let (v, readable) = ds
        .dataset
        .views()
        .get_readable(&name, reader.as_ref().and_then(|r| r.as_ref()))
        .ok_or_else(|| {
            ApiError::from(sparkles::incr::error(
                "view-not-found",
                format!("no view '{name}' in /{}", ds.name),
            ))
        })?;
    #[cfg(test)]
    hooks::after_capture(&ds.dataset.store().dataset_id().to_string());
    let mut j = info_json(&ds.name, &v);
    redact(&mut j, readable_for(&reader, readable));
    Ok(Json(j))
}

/// Record a refused view management request (`PUT`, `DELETE` or `POST .../rebuild`) of
/// a caller who sees dataset `ds`, as `view_access` with `op = "manage"` in the audit
/// log. `reason` is `level` when the caller's grants do not reach the route's level
/// and `read-only` when the server refuses every change.
pub(crate) fn manage_refused(ds: &str, view: &str, p: &Principal, reason: &str) {
    tracing::info!(
        target: "sparkles::audit",
        event = "view_access",
        dataset = ds,
        view = view,
        op = "manage",
        decision = "deny",
        reason = reason,
        cached = false,
        principal = p.id().as_str(),
    );
}

/// The read-only refusal of a view management request, recorded in the audit log.
fn read_only(ds: &str, view: &str, p: &Principal) -> ApiError {
    manage_refused(ds, view, p, "read-only");
    err(StatusCode::FORBIDDEN, "server is read-only")
}

/// `PUT /$/views/{ds}/{name}`: the view's definition. Without `principal`, the view
/// reads as its definer: nobody when the definer reads every quad of the dataset,
/// else the definer, who must then be a configured user.
pub(super) async fn put_view(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    AdminBody(body): AdminBody,
) -> ApiResult<Response> {
    if st.read_only {
        return Err(read_only(&ds_name, &name, &p));
    }
    let ds = dataset(&st, &ds_name)?;
    let mut j: J = serde_json::from_slice(&body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
    if let Some(o) = j.as_object_mut() {
        // the members of a GET answer, so that one can be sent back
        for k in [
            "name",
            "dataset",
            "state",
            "commit",
            "columns",
            "rows",
            "stateBytes",
            "passes",
            "skipped",
            "passMs",
            "recomputeMs",
            "evaluation",
            "evaluationReason",
            "plan",
            "reason",
            "error",
            "definition",
        ] {
            o.remove(k);
        }
    }
    let mut def: ViewDef = serde_json::from_value(j)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid view: {e}")))?;
    if def.principal.is_none() && !p.is_local() && p.view(&ds.name, Endpoint::Query).is_some() {
        if p.kind != crate::auth::Kind::User {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "your grants do not cover every graph: name a user as the view's principal",
            ));
        }
        def.principal = Some(p.name.to_string());
    }
    let existed = ds.dataset.views().get(&name).is_some();
    let who = p.id();
    let ds2 = ds.clone();
    let n2 = name.clone();
    let info = blocking(move || Ok(ds2.dataset.views().put(&n2, def)?)).await?;
    tracing::info!(
        target: "sparkles::audit",
        event = "view_defined",
        op = "manage",
        decision = "allow",
        dataset = ds.name.as_str(),
        view = name.as_str(),
        principal = who.as_str()
    );
    // the first evaluation, as a task the caller can follow
    let ds3 = ds.clone();
    let n3 = name.clone();
    let task = st.start_task("view-build", &ds.name, move |_| {
        let v = ds3
            .dataset
            .views()
            .wait_built(&n3, Duration::from_secs(24 * 3600))?;
        match &v.state {
            sparkles::incr::ViewState::Failed { error } => Err(anyhow::anyhow!("{error}")),
            sparkles::incr::ViewState::Suspended { reason } => Err(anyhow::anyhow!("{reason}")),
            _ => Ok(format!(
                "view {n3} built at commit {}",
                v.commit.unwrap_or(0)
            )),
        }
    });
    let mut out = info_json(&ds.name, &info);
    out["task"] = task.id.into();
    let code = if existed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((code, Json(out)).into_response())
}

/// `DELETE /$/views/{ds}/{name}`
pub(super) async fn delete_view(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
) -> ApiResult<StatusCode> {
    if st.read_only {
        return Err(read_only(&ds_name, &name, &p));
    }
    let ds = dataset(&st, &ds_name)?;
    let ds2 = ds.clone();
    let n2 = name.clone();
    let existed = blocking(move || Ok(ds2.dataset.views().delete(&n2)?)).await?;
    if !existed {
        return Err(sparkles::incr::error(
            "view-not-found",
            format!("no view '{name}' in /{}", ds.name),
        )
        .into());
    }
    tracing::info!(
        target: "sparkles::audit",
        event = "view_deleted",
        op = "manage",
        decision = "allow",
        dataset = ds.name.as_str(),
        view = name.as_str(),
        principal = p.id().as_str()
    );
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /$/views/{ds}/{name}/rebuild`
pub(super) async fn rebuild(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Response> {
    // a rebuild resumes a restored or cloned view, which rewrites its definition
    if st.read_only {
        return Err(read_only(&ds_name, &name, &p));
    }
    let ds = dataset(&st, &ds_name)?;
    ds.dataset.views().rebuild(&name)?;
    tracing::info!(
        target: "sparkles::audit",
        event = "view_rebuilt",
        op = "manage",
        decision = "allow",
        dataset = ds.name.as_str(),
        view = name.as_str(),
        principal = p.id().as_str()
    );
    let ds3 = ds.clone();
    let n3 = name.clone();
    let task = st.start_task("view-build", &ds.name, move |_| {
        let v = ds3
            .dataset
            .views()
            .wait_built(&n3, Duration::from_secs(24 * 3600))?;
        Ok(format!(
            "view {n3} built at commit {}",
            v.commit.unwrap_or(0)
        ))
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "dataset": ds.name, "name": name, "task": task.id })),
    )
        .into_response())
}

#[cfg(test)]
pub(crate) mod hooks {
    //! Test hooks of the metadata routes, by dataset so that tests running together
    //! in one process do not see each other's.
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    pub(crate) type Hook = Arc<dyn Fn() + Send + Sync>;

    fn hooks() -> &'static Mutex<HashMap<String, Hook>> {
        static HOOKS: OnceLock<Mutex<HashMap<String, Hook>>> = OnceLock::new();
        HOOKS.get_or_init(Default::default)
    }

    /// Run `hook` in the metadata routes of dataset `ds` between capturing the views
    /// and answering.
    pub(crate) fn set_after_capture(ds: &str, hook: Option<Hook>) {
        let mut h = hooks().lock().unwrap();
        match hook {
            Some(hook) => h.insert(ds.to_string(), hook),
            None => h.remove(ds),
        };
    }

    pub(super) fn after_capture(ds: &str) {
        let hook = hooks().lock().unwrap().get(ds).cloned();
        if let Some(hook) = hook {
            hook();
        }
    }
}
