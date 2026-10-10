//! The ingestion routes (spec C18 §10):
//!
//! - `POST /$/ingest/{ds}` starts an ingestion task from an uploaded file (multipart,
//!   with the options as fields), or from JSON with `text` or a `url` that the server
//!   fetches through its outbound policy. It answers `202` with the task.
//! - `GET /$/ingest/{ds}` lists the caller's tasks, and `GET /$/ingest/{ds}/{task}`
//!   reads one with its progress, estimate, usage and result. `?wait=SECONDS` holds the
//!   answer until the task ends or waits for the caller, at most 60 seconds.
//! - `POST /$/ingest/{ds}/{task}/confirm` confirms an estimate above the dataset's
//!   threshold, `POST …/approve` writes an approved preview to `main`, and
//!   `DELETE /$/ingest/{ds}/{task}` cancels a running task or forgets a finished one.
//!
//! A task is visible to the principal that started it and to admins of the dataset.
//! Every write runs through the tools as the caller, so the routes need only `read` on
//! the dataset: what the caller may not write fails in the task.

use super::pipeline::{self, Ctx, Mode, Request};
use super::{Status, Task};
use crate::auth::{Level, Principal};
use crate::http::{ApiResult, err, err_body, err_code};
use crate::state::AppState;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// The most a request body may hold: the input ceiling and room for the fields.
const MAX_BODY: usize = super::convert::MAX_INPUT_BYTES + (1 << 20);
/// The longest `?wait=`.
const MAX_WAIT: Duration = Duration::from_secs(60);

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/$/ingest/{ds}",
            post(start).get(list).layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .route("/$/ingest/{ds}/{task}", get(read).delete(cancel))
        .route("/$/ingest/{ds}/{task}/confirm", post(confirm))
        .route("/$/ingest/{ds}/{task}/approve", post(approve))
}

fn bad(m: impl Into<String>) -> crate::http::ApiError {
    err_code(StatusCode::BAD_REQUEST, "bad-argument", m)
}

/// The options of `POST /$/ingest/{ds}`, as JSON members or multipart fields.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Options {
    /// the document's text (JSON only)
    text: Option<String>,
    /// the media type of `text`, or of the file when its part has none
    format: Option<String>,
    /// the file name, which may say the format
    name: Option<String>,
    url: Option<String>,
    title: Option<String>,
    iri: Option<String>,
    graph: Option<String>,
    profile: Option<String>,
    mode: Option<String>,
    branch: Option<String>,
    allow_partial: Option<bool>,
    extract: Option<bool>,
    confirm: Option<bool>,
    base: Option<String>,
    message: Option<String>,
    deadline_seconds: Option<f64>,
}

impl Options {
    /// Set a multipart text field.
    fn set(&mut self, k: &str, v: String) -> Result<(), crate::http::ApiError> {
        let flag = |v: &str| match v.trim() {
            "true" | "1" | "on" | "yes" => Ok(true),
            "false" | "0" | "off" | "no" | "" => Ok(false),
            _ => Err(bad(format!("{k} is true or false"))),
        };
        let v_opt = Some(v.trim().to_string()).filter(|s| !s.is_empty());
        match k {
            "format" => self.format = v_opt,
            "name" => self.name = v_opt,
            "url" => self.url = v_opt,
            "title" => self.title = v_opt,
            "iri" => self.iri = v_opt,
            "graph" => self.graph = v_opt,
            "profile" => self.profile = v_opt,
            "mode" => self.mode = v_opt,
            "branch" => self.branch = v_opt,
            "base" => self.base = v_opt,
            "message" => self.message = v_opt,
            "allowPartial" => self.allow_partial = Some(flag(&v)?),
            "extract" => self.extract = Some(flag(&v)?),
            "confirm" => self.confirm = Some(flag(&v)?),
            "deadlineSeconds" => {
                self.deadline_seconds = Some(
                    v.trim()
                        .parse()
                        .map_err(|_| bad("deadlineSeconds is a number"))?,
                );
            }
            _ => return Err(bad(format!("unknown field {k:?}"))),
        }
        Ok(())
    }
}

/// The task, if the caller may see it.
fn visible(st: &AppState, p: &Principal, ds: &str, id: &str) -> ApiResult<Arc<Task>> {
    let t = st
        .ingest
        .get(id)
        .filter(|t| t.dataset == ds && (t.owner == p.id() || p.can(ds, Level::Admin)));
    t.ok_or_else(|| {
        err_code(
            StatusCode::NOT_FOUND,
            "unknown-task",
            format!("no ingestion task {id:?} of yours in dataset {ds}"),
        )
    })
}

fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(&crate::obs::X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

async fn start(
    State(st): State<Arc<AppState>>,
    Path(ds): Path<String>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> ApiResult<Response> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    if st.get(&ds).is_none() {
        return Err(err_code(
            StatusCode::NOT_FOUND,
            "unknown-dataset",
            format!("no such dataset: /{ds}"),
        ));
    }
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mut o = Options::default();
    let mut file: Option<(Vec<u8>, Option<String>, Option<String>)> = None;
    if ct == "multipart/form-data" {
        let mp_err = |e: axum::extract::multipart::MultipartError| err(e.status(), e.body_text());
        let mut mp = Multipart::from_request(request, &())
            .await
            .map_err(|e| bad(e.to_string()))?;
        while let Some(field) = mp.next_field().await.map_err(mp_err)? {
            let name = field.name().unwrap_or("").to_string();
            if name == "file" {
                if file.is_some() {
                    return Err(bad("one file per ingestion"));
                }
                let fname = field.file_name().map(|f| {
                    std::path::Path::new(f)
                        .file_name()
                        .map_or_else(|| f.to_string(), |n| n.to_string_lossy().into_owned())
                });
                let mt = field.content_type().map(str::to_string);
                let bytes = field.bytes().await.map_err(mp_err)?;
                file = Some((bytes.to_vec(), fname, mt));
            } else {
                let bytes = field.bytes().await.map_err(mp_err)?;
                if bytes.len() > 64 << 10 {
                    return Err(bad(format!("the field {name} is too long")));
                }
                let v = String::from_utf8(bytes.to_vec())
                    .map_err(|_| bad(format!("the field {name} is not UTF-8")))?;
                o.set(&name, v)?;
            }
        }
    } else if ct == "application/json" || ct.is_empty() {
        let body = axum::body::to_bytes(request.into_body(), MAX_BODY)
            .await
            .map_err(|e| err(StatusCode::PAYLOAD_TOO_LARGE, e.to_string()))?;
        o = serde_json::from_slice(&body).map_err(|e| bad(format!("the body: {e}")))?;
    } else {
        return Err(err_code(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported-format",
            "send multipart/form-data with a file part, or JSON with text or url",
        ));
    }
    let mode = match o.mode.as_deref() {
        None => Mode::Branch,
        Some(m) => Mode::parse(m).ok_or_else(|| bad("mode is branch, preview or auto"))?,
    };
    if let Some(b) = &o.branch
        && !sparkles::branch::valid_name(b)
    {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "invalid-branch",
            format!("invalid branch name '{b}'"),
        ));
    }
    if mode == Mode::Preview && o.branch.is_some() {
        return Err(bad("a preview writes no branch: leave branch out"));
    }
    let deadline = match o.deadline_seconds {
        Some(s) if !(1.0..=86_400.0).contains(&s) => {
            return Err(bad("deadlineSeconds is between 1 and 86400"));
        }
        Some(s) => Duration::from_secs_f64(s),
        None => pipeline::DEFAULT_DEADLINE,
    };
    // the document: the file, the text, or the URL
    let (bytes, name, media_type, fetched) = match (file, o.text.take()) {
        (Some(_), Some(_)) => return Err(bad("send a file or text, not both")),
        (Some((b, n, mt)), None) => {
            let mt = o.format.clone().or(mt).filter(|m| {
                // a browser's generic type says nothing
                m != "application/octet-stream"
            });
            (b, o.name.clone().or(n), mt, false)
        }
        (None, Some(t)) => (t.into_bytes(), o.name.clone(), o.format.clone(), false),
        (None, None) => match &o.url {
            Some(u) if u.starts_with("http://") || u.starts_with("https://") => {
                (Vec::new(), o.name.clone(), o.format.clone(), true)
            }
            Some(_) => return Err(bad("url is an http or https URL")),
            None => return Err(bad("send a file part, text or url")),
        },
    };
    if bytes.len() > super::convert::MAX_INPUT_BYTES {
        return Err(err_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too-large",
            format!(
                "the document has {} bytes; ingestion takes at most {}",
                bytes.len(),
                super::convert::MAX_INPUT_BYTES
            ),
        ));
    }
    let mut input = json!({ "mode": mode.as_str() });
    if let Some(n) = &name {
        input["name"] = n.clone().into();
    }
    if let Some(u) = &o.url {
        input["url"] = u.clone().into();
    }
    if let Some(m) = &media_type {
        input["format"] = m.clone().into();
    }
    if !fetched {
        input["bytes"] = bytes.len().into();
    }
    let req = Request {
        dataset: ds.clone(),
        bytes,
        name,
        media_type,
        url: o.url.clone(),
        title: o.title.clone(),
        iri: o.iri.clone(),
        graph: o.graph.clone(),
        profile: o.profile.clone(),
        mode,
        branch: o.branch.clone(),
        allow_partial: o.allow_partial.unwrap_or(false),
        extract: o.extract,
        confirm: o.confirm.unwrap_or(false),
        base: o.base.clone(),
        message: o.message.clone(),
        pairs: Vec::new(),
    };
    let task = Arc::new(Task::new(&ds, &p.id(), input));
    st.ingest
        .add(task.clone())
        .map_err(|m| err_code(StatusCode::TOO_MANY_REQUESTS, "too-many-tasks", m))?;
    spawn(
        st.clone(),
        p,
        task.clone(),
        req,
        fetched,
        deadline,
        request_id(&headers),
    );
    let loc = format!("/$/ingest/{}/{}", urlencode(&ds), task.id);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, loc)],
        Json(task.json()),
    )
        .into_response())
}

fn urlencode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// Run the task on a thread of its own.
fn spawn(
    st: Arc<AppState>,
    p: Principal,
    task: Arc<Task>,
    mut req: Request,
    fetch: bool,
    deadline: Duration,
    request_id: String,
) {
    let span = tracing::Span::current();
    let t = task.clone();
    let run = move || {
        let _e = span.enter();
        let started = Instant::now();
        let deadline = started + deadline;
        if fetch {
            t.set(
                Status::Converting,
                0.01,
                Some("fetching the document".into()),
            );
            let url = req.url.clone().unwrap_or_default();
            let budget = sparkles::outbound::RequestBudget::new(&st.outbound);
            match sparkles::outbound::fetch_bytes(
                &st.outbound,
                &budget,
                &url,
                "text/html, application/pdf, text/markdown, text/plain;q=0.9, text/csv;q=0.8, */*;q=0.1",
            ) {
                Ok((bytes, ct)) => {
                    req.bytes = bytes;
                    if req.media_type.is_none() && !ct.is_empty() {
                        req.media_type = Some(ct);
                    }
                }
                Err(e) => {
                    let code = match &e {
                        sparkles::error::Error::NotPermitted(_) => "fetch-refused",
                        _ => "fetch-failed",
                    };
                    t.finish(
                        Err(json!({ "code": code, "message": e.to_string() })),
                        json!({}),
                    );
                    return;
                }
            }
        }
        let cfg = crate::mcp::rest::config(&st, crate::mcp::rest::Mode::Write);
        let server = crate::mcp::McpServer::new(st.clone(), cfg);
        let models = st.models.clone();
        let ctx = Ctx {
            server: &server,
            models: models.as_deref(),
            principal: &p,
            progress: t.as_ref(),
            deadline,
            cancel: t.cancel.clone(),
            pdf: st.ingest.pdf.clone(),
            request_id,
        };
        let (out, usage) = pipeline::run(&ctx, &req);
        let tokens = usage["inputTokens"].as_u64().unwrap_or(0)
            + usage["outputTokens"].as_u64().unwrap_or(0);
        if tokens > 0 {
            st.asks.add_tokens(&req.dataset, &p.id(), tokens);
        }
        if let Err(e) = &out {
            tracing::info!(task = %t.id, code = %e["code"], "ingestion failed");
        }
        t.finish(out, usage);
    };
    if let Err(e) = std::thread::Builder::new().name("ingest".into()).spawn(run) {
        task.finish(
            Err(json!({ "code": "internal", "message": format!("no thread for the task: {e}") })),
            json!({}),
        );
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadQuery {
    wait: Option<f64>,
}

async fn read(
    State(st): State<Arc<AppState>>,
    Path((ds, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    Query(q): Query<ReadQuery>,
) -> ApiResult<Response> {
    let t = visible(&st, &p, &ds, &id)?;
    if let Some(w) = q.wait.filter(|w| *w > 0.0) {
        let wait = Duration::from_secs_f64(w.min(MAX_WAIT.as_secs_f64()));
        let t2 = t.clone();
        tokio::task::spawn_blocking(move || t2.wait_settled(wait))
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(Json(t.json()).into_response())
}

async fn list(
    State(st): State<Arc<AppState>>,
    Path(ds): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Response> {
    let me = p.id();
    let admin = p.can(&ds, Level::Admin);
    let tasks: Vec<Value> = st
        .ingest
        .list(&ds)
        .into_iter()
        .filter(|t| admin || t.owner == me)
        .map(|t| {
            let mut j = t.json();
            // the list is a summary: the result's details stay on the task
            if let Some(o) = j.as_object_mut() {
                o.remove("usage");
                if let Some(r) = o.get_mut("result").and_then(Value::as_object_mut) {
                    r.retain(|k, _| {
                        matches!(
                            k.as_str(),
                            "outcome"
                                | "branch"
                                | "review"
                                | "source"
                                | "proposed"
                                | "rows"
                                | "triples"
                        )
                    });
                }
            }
            j
        })
        .collect();
    Ok(
        Json(
            json!({ "dataset": ds, "tasks": tasks, "capabilities": st.ingest.pdf.capabilities() }),
        )
        .into_response(),
    )
}

async fn confirm(
    State(st): State<Arc<AppState>>,
    Path((ds, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Response> {
    let t = visible(&st, &p, &ds, &id)?;
    let status = t.state.lock().status;
    if status != Status::AwaitingConfirmation {
        return Err(err_code(
            StatusCode::CONFLICT,
            "not-waiting",
            format!("the task is {}, not awaiting confirmation", status.as_str()),
        ));
    }
    t.update(|s| {
        s.confirmed = true;
        s.status = Status::Extracting;
        s.message = None;
    });
    t.wake();
    Ok(Json(t.json()).into_response())
}

async fn approve(
    State(st): State<Arc<AppState>>,
    Path((ds, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let t = visible(&st, &p, &ds, &id)?;
    let approval = {
        let mut s = t.state.lock();
        if s.status != Status::AwaitingApproval {
            return Err(err_code(
                StatusCode::CONFLICT,
                "not-waiting",
                format!(
                    "the task is {}, not a preview awaiting approval",
                    s.status.as_str()
                ),
            ));
        }
        // one approval at a time
        s.status = Status::Writing;
        s.approval.clone()
    };
    let Some(a) = approval else {
        return Err(err_code(
            StatusCode::CONFLICT,
            "not-waiting",
            "the preview holds nothing to write",
        ));
    };
    let st2 = st.clone();
    let rid = request_id(&headers);
    let t2 = t.clone();
    let out = crate::http::blocking(move || {
        let cfg = crate::mcp::rest::config(&st2, crate::mcp::rest::Mode::Write);
        let server = crate::mcp::McpServer::new(st2.clone(), cfg);
        let ctx = Ctx {
            server: &server,
            models: None,
            principal: &p,
            progress: &pipeline::Quiet,
            deadline: Instant::now() + Duration::from_secs(300),
            cancel: t2.cancel.clone(),
            pdf: st2.ingest.pdf.clone(),
            request_id: rid,
        };
        Ok(pipeline::approve(&ctx, &ds, &a))
    })
    .await?;
    match out {
        Ok(v) => {
            t.update(|s| {
                s.status = Status::Done;
                s.approval = None;
                if let Some(r) = s.result.as_mut() {
                    r["outcome"] = "approved".into();
                    r["approved"] = v.clone();
                }
            });
            Ok(Json(t.json()).into_response())
        }
        Err(e) => {
            t.update(|s| s.status = Status::AwaitingApproval);
            let status = e["status"]
                .as_u64()
                .and_then(|s| StatusCode::from_u16(s as u16).ok())
                .unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
            let mut body = e.clone();
            body["error"] = e["message"].clone();
            Err(err_body(status, body))
        }
    }
}

async fn cancel(
    State(st): State<Arc<AppState>>,
    Path((ds, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Response> {
    let t = visible(&st, &p, &ds, &id)?;
    let active = t.state.lock().status.active();
    if active {
        t.cancel.store(true, Ordering::Relaxed);
        t.wake();
        return Ok((StatusCode::ACCEPTED, Json(t.json())).into_response());
    }
    st.ingest.remove(&t.id);
    Ok(StatusCode::NO_CONTENT.into_response())
}
