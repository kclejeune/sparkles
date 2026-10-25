//! Stored mappings (spec F19 §2.2 and §8): `/$/mappings/{ds}[/{name}[/versions]]`
//! manages them and `POST /{ds}/mappings/{name}/run` runs one into the dataset.
//!
//! A stored mapping's body is its text, with `Content-Type` naming its kind. A run
//! reads the multipart file parts its logical sources name and the server's
//! connections (the `connections` settings kind), and commits once. `mode=replace`
//! replaces the constant graphs the mapping writes in that commit, so a refresh of a
//! materialized copy changes only what changed.

use super::tabular::{self, Ctx, Mappings};
use super::{
    AdminBody, ApiError, ApiResult, BodyBudget, Params, St, Target, blocking, conditional,
    content_type, dataset, err, write_error, write_response,
};
use crate::auth::Principal;
use crate::state::Dataset;
use axum::Extension;
use axum::Json;
use axum::extract::{Multipart, Path};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::IntoResponse;
use serde_json::{Value as J, json};
use sparkles::Error;
use sparkles::mapping::MappingKind;
use sparkles::stored_mappings::{Change, Definition, LastRun, Stored};
use std::path::PathBuf;

fn not_found(ds: &str, name: &str) -> ApiError {
    err(
        StatusCode::NOT_FOUND,
        format!("no stored mapping '{name}' in /{ds}"),
    )
}

fn etag(v: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"v{v}\"")).unwrap_or(HeaderValue::from_static("\"v0\""))
}

/// The connections a mapping names that the server does not declare.
fn missing_connections(st: &crate::state::AppState, names: &[String]) -> Vec<String> {
    let have = crate::settings::connections::effective(st);
    names
        .iter()
        .filter(|n| {
            // an R2RML mapping's default connection is the only one, or `default`
            if n.is_empty() {
                !(have.len() == 1 || have.iter().any(|c| c.name == "default"))
            } else {
                !have
                    .iter()
                    .any(|c| &c.name == *n || c.jdbc_dsn.as_deref() == Some(n.as_str()))
            }
        })
        .map(|n| {
            if n.is_empty() {
                "the default connection of an R2RML mapping".to_string()
            } else {
                format!("connection {n:?}")
            }
        })
        .collect()
}

/// A stored mapping as JSON: its name, kind, description, version, the check's summary
/// and the last run; the text unless `text` is false.
fn stored_json(st: &crate::state::AppState, ds: &Dataset, name: &str, s: &Stored, text: bool) -> J {
    let mut j = json!({
        "name": name,
        "dataset": ds.name,
        "kind": s.definition.kind,
        "version": s.version,
    });
    if let Some(d) = &s.definition.description {
        j["description"] = d.clone().into();
    }
    if text {
        j["text"] = s.definition.text.clone().into();
    }
    if let Ok(mut summary) = s.definition.check() {
        for c in missing_connections(st, &summary.connections) {
            summary.warnings.push(format!(
                "the mapping reads {c}, which the server's connections settings do not declare; a run fails until they do"
            ));
        }
        j["summary"] = serde_json::to_value(summary).unwrap_or_default();
    }
    if let Some(r) = ds.dataset.mappings().last_run(name) {
        j["lastRun"] = serde_json::to_value(r).unwrap_or_default();
    }
    j
}

/// `GET /$/mappings/{ds}`
pub(super) async fn list(st: St, Path(ds_name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &ds_name)?;
    let mappings: Vec<J> = ds
        .dataset
        .mappings()
        .list()
        .into_iter()
        .map(|(name, s, _)| {
            let mut j = stored_json(&st, &ds, &name, &s, false);
            if let Some(o) = j.as_object_mut() {
                o.remove("dataset");
            }
            j
        })
        .collect();
    let mut out = json!({ "dataset": ds.name, "mappings": mappings });
    if let Some(e) = ds.dataset.mappings().error() {
        out["warning"] = e.into();
    }
    Ok(Json(out))
}

fn version_param(params: &Params) -> ApiResult<Option<u64>> {
    params
        .get("version")
        .map(|v| {
            v.trim_start_matches('v')
                .parse::<u64>()
                .map_err(|_| err(StatusCode::BAD_REQUEST, "version must be a version number"))
        })
        .transpose()
}

/// `GET /$/mappings/{ds}/{name}`: the JSON description with the text, or the text
/// alone with the kind's media type when the client asks for it.
pub(super) async fn get_mapping(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &ds_name)?;
    let version = version_param(&Params::from_query(&uri))?;
    let s = ds
        .dataset
        .mappings()
        .get(&name, version)
        .ok_or_else(|| not_found(&ds.name, &name))?;
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut r = match s.definition.kind() {
        Ok(k) if accept.contains(k.media_type()) => (
            [(header::CONTENT_TYPE, k.media_type())],
            s.definition.text.clone(),
        )
            .into_response(),
        _ => Json(stored_json(&st, &ds, &name, &s, true)).into_response(),
    };
    r.headers_mut()
        .insert(header::ETAG, etag(s.version.version));
    Ok(r)
}

/// `GET /$/mappings/{ds}/{name}/versions`
pub(super) async fn versions(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &ds_name)?;
    let v = ds
        .dataset
        .mappings()
        .versions(&name)
        .ok_or_else(|| not_found(&ds.name, &name))?;
    Ok(Json(
        json!({ "dataset": ds.name, "name": name, "versions": v }),
    ))
}

/// The version an `If-Match` or `If-None-Match: *` header requires (`Some(0)`: none).
fn precondition(headers: &HeaderMap, exists: bool) -> ApiResult<Option<u64>> {
    let text = |h| headers.get(h).and_then(|v: &HeaderValue| v.to_str().ok());
    if let Some(m) = text(header::IF_NONE_MATCH) {
        return if m.trim() == "*" {
            Ok(Some(0))
        } else {
            Err(err(
                StatusCode::BAD_REQUEST,
                "If-None-Match takes * only here",
            ))
        };
    }
    let Some(m) = text(header::IF_MATCH) else {
        return Ok(None);
    };
    let m = m.trim();
    if m == "*" {
        return if exists {
            Ok(None)
        } else {
            Err(err(
                StatusCode::PRECONDITION_FAILED,
                "the stored mapping does not exist",
            ))
        };
    }
    m.trim_start_matches("W/")
        .trim_matches('"')
        .strip_prefix('v')
        .and_then(|v| v.parse::<u64>().ok())
        .map(Some)
        .ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "If-Match must be a version tag such as \"v3\"",
            )
        })
}

/// `PUT /$/mappings/{ds}/{name}`: the mapping text, with `Content-Type` naming its kind
/// (or `kind=`), `description=` and `Sparkles-Commit-Message`.
pub(super) async fn put_mapping(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let params = Params::from_query(&uri);
    let ct = content_type(&headers);
    let kind = match params.get("kind") {
        Some(k) => MappingKind::parse(k).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                format!("kind {k:?}: use rml, yarrrml, csvw or template"),
            )
        })?,
        None => MappingKind::from_media_type(&ct).ok_or_else(|| {
            err(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!(
                    "send the mapping as text/turtle (R2RML, RML), application/yaml (YARRRML), application/csvm+json (CSVW) or application/sparql-query (a template), not {ct:?}"
                ),
            )
        })?,
    };
    let text = String::from_utf8(body.to_vec())
        .map_err(|_| err(StatusCode::BAD_REQUEST, "the mapping is not UTF-8"))?;
    let description = params.get("description").map(str::to_string);
    if description
        .as_ref()
        .is_some_and(|d| d.len() > 1024 || d.chars().any(char::is_control))
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "description must be at most 1024 bytes without control characters",
        ));
    }
    let message = super::commit_message_header(&headers)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let if_version = precondition(&headers, ds.dataset.mappings().get(&name, None).is_some())?;
    let author = (!p.name.is_empty()).then(|| p.name.to_string());
    blocking(move || {
        let change = Change {
            author,
            message,
            dataset_commit: Some(ds.store.snapshot().commit),
            if_version,
        };
        let def = Definition {
            kind: kind.name().to_string(),
            text,
            description,
        };
        let saved = ds.dataset.mappings().put(&name, def, change)?;
        let status = if saved.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        let mut body = stored_json(&st, &ds, &name, &saved.stored, false);
        body["changed"] = saved.changed.into();
        let mut r = (status, Json(body)).into_response();
        r.headers_mut()
            .insert(header::ETAG, etag(saved.stored.version.version));
        Ok(r)
    })
    .await
}

/// `DELETE /$/mappings/{ds}/{name}`
pub(super) async fn delete_mapping(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let exists = ds.dataset.mappings().get(&name, None).is_some();
    let if_version = precondition(&headers, exists)?.filter(|v| *v > 0);
    blocking(move || {
        if ds.dataset.mappings().delete(&name, if_version)? {
            Ok(StatusCode::NO_CONTENT.into_response())
        } else {
            Err(not_found(&ds.name, &name))
        }
    })
    .await
}

/// Spool the file parts of a multipart body into `dir`, as `{n}-{name}` so that the
/// conversion matches them to the mapping's sources by name.
async fn spool_files(
    request: axum::extract::Request,
    st: &crate::state::AppState,
    dir: &std::path::Path,
) -> ApiResult<Vec<PathBuf>> {
    use axum::extract::FromRequest;
    let mut files = Vec::new();
    let mut budget = BodyBudget::new(&st.limits);
    let mp_err = |e: axum::extract::multipart::MultipartError| err(e.status(), e.body_text());
    let mut mp = Multipart::from_request(request, &())
        .await
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    while let Some(mut field) = mp.next_field().await.map_err(mp_err)? {
        let Some(fname) = field.file_name().map(str::to_string) else {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!(
                    "part {:?} has no file name; a run takes the files its mapping reads, named as the mapping names them",
                    field.name().unwrap_or("")
                ),
            ));
        };
        let fname = std::path::Path::new(&fname)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .filter(|f| !f.is_empty())
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "a part has an empty file name"))?;
        let path = dir.join(format!("{}-{fname}", files.len()));
        let io = |e: std::io::Error| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        let mut out = std::io::BufWriter::new(std::fs::File::create(&path).map_err(io)?);
        while let Some(chunk) = field.chunk().await.map_err(mp_err)? {
            budget.read(chunk.len())?;
            budget.disk(dir, chunk.len())?;
            std::io::Write::write_all(&mut out, &chunk).map_err(io)?;
        }
        std::io::Write::flush(&mut out).map_err(io)?;
        files.push(path);
    }
    Ok(files)
}

/// `POST /{ds}/mappings/{name}/run`: run a stored mapping and commit once.
pub(super) async fn run(
    st: St,
    Path((ds_name, name)): Path<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let params = Params::from_query(&uri);
    super::history::reject_at(&params)?;
    let principal = request.extensions().get::<Principal>().cloned();
    let wanted = super::receipt_wanted(&params, &headers);
    let replace = match params.get("mode") {
        None | Some("add") => false,
        Some("replace") => true,
        Some(m) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("mode {m:?}: use add or replace"),
            ));
        }
    };
    let (mut wopts, _cancel_on_drop) =
        super::body_write_options(&st, &params, &headers, principal.as_ref())?;
    wopts.opts.graphs = principal
        .as_ref()
        .and_then(|p| p.view(&ds_name, crate::auth::Endpoint::Upload));
    let restricted = wopts.opts.graphs.is_some();
    let lookup_grants = wopts.opts.graphs.clone();
    wopts.opts.precondition =
        conditional::write_precondition(&headers, ds.store.dataset_id(), &Target::Dataset);
    let dry = wopts
        .opts
        .dry_run
        .as_ref()
        .map(|d| {
            super::dry_run::Request::for_write(
                &st,
                &headers,
                &params,
                d,
                restricted,
                principal.as_ref(),
                &ds,
                &wopts.opts,
            )
        })
        .transpose()?;
    let mut mappings = Mappings::default();
    let mut q = Params(vec![("mapping".into(), name.clone())]);
    if let Some(v) = params.get("version") {
        q.0.push(("mappingVersion".into(), v.to_string()));
    }
    mappings.resolve(&q, &ds)?;
    let (stored_name, version) = mappings.stored.clone().expect("resolved");
    let kind = mappings.kind.expect("resolved");
    let graph = params
        .get("graph")
        .map(|g| oxrdf::NamedNode::new(g).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string())))
        .transpose()?;
    // the graphs replace mode clears: the mapping's constant graphs, or the one graph
    let targets: Option<Vec<Option<oxrdf::NamedNode>>> = if !replace {
        None
    } else if let Some(g) = &graph {
        Some(vec![Some(g.clone())])
    } else {
        match kind {
            MappingKind::Rml | MappingKind::Yarrrml => {
                let m = sparkles::mapping::parse(
                    mappings.mapping.as_deref().unwrap_or_default(),
                    kind,
                    None,
                )?;
                Some(m.constant_graphs().ok_or_else(|| {
                    super::err_code(
                        StatusCode::BAD_REQUEST,
                        "mapping-graphs-unknown",
                        "mode=replace needs constant graph maps, since the graphs a template graph map would clear are not known before the run",
                    )
                })?)
            }
            _ => Some(vec![None]),
        }
    };
    let tmp = tempfile::Builder::new()
        .prefix("sparkles-mapping-")
        .tempdir()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let ct = content_type(&headers);
    let files = if ct == "multipart/form-data" {
        spool_files(request, &st, tmp.path()).await?
    } else if ct.is_empty()
        || headers
            .get(header::CONTENT_LENGTH)
            .is_some_and(|v| v == "0")
    {
        Vec::new()
    } else {
        return Err(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send the files the mapping reads as multipart/form-data, or no body",
        ));
    };
    let lookup = super::lookup_param(&params)?;
    let (mut wopts, timeout) = wopts.start();
    let dry = dry.map(|request| request.started(&wopts));
    if wopts.message.is_none() {
        wopts.message = Some(format!("mapping {stored_name} (version {version})").into());
    }
    let grant = crate::server_memory::current_grant();
    let author = wopts.author.as_ref().map(|a| a.to_string());
    blocking(move || {
        let lookup = match lookup {
            true => Some(super::tabular_lookup(&ds, lookup_grants)?),
            false => None,
        };
        let cx = Ctx {
            st: &st,
            wopts: &wopts,
            grant: grant.as_ref(),
            lookup,
            only_graph: graph.clone(),
            dir: tmp.path(),
        };
        let given = files.clone();
        let mut files = files;
        let reports = tabular::convert(&mut files, &mappings, &params, &cx)?;
        if files.iter().any(|f| given.contains(f)) {
            let extra: Vec<String> = files
                .iter()
                .filter(|f| given.contains(f))
                .map(|f| tabular::client_name(f))
                .collect();
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!(
                    "the mapping does not read {}; a run takes only the files its mapping reads",
                    extra.join(", ")
                ),
            ));
        }
        let (rows, quads) = reports.iter().fold((0u64, 0u64), |(r, q), x| {
            (
                r + x["rows"].as_u64().unwrap_or(0),
                q + x["quads"]
                    .as_u64()
                    .or_else(|| x["triples"].as_u64())
                    .unwrap_or(0),
            )
        });
        let warnings: Vec<String> = reports
            .iter()
            .flat_map(|x| x["warnings"].as_array().cloned().unwrap_or_default())
            .filter_map(|w| w.as_str().map(str::to_string))
            .collect();
        let sources = files
            .iter()
            .map(|p| {
                // a mapping's N-Quads already name the graph
                let g = if p.extension().is_some_and(|e| e == "nq") {
                    None
                } else {
                    graph.clone()
                };
                sparkles::io::Source::from_path(p, g).map(|mut s| {
                    s.max_decompressed = st.limits.max_decompressed_bytes;
                    s
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let kind_commit = sparkles::commit::CommitKind::Load;
        let result = match &targets {
            None => ds.store.load_with(&sources, kind_commit, &wopts),
            Some(gs) => ds
                .store
                .replace_with(
                    sparkles::store::ReplaceTarget::Graphs(gs.clone()),
                    &sources,
                    kind_commit,
                    &wopts,
                )
                .map(|(_, r)| r),
        };
        let receipt = match (result, dry) {
            (Err(Error::DryRun(p)), Some(d)) => return Ok(d.respond(&ds, &p)),
            (r, _) => r.map_err(|e| write_error(e, restricted))?,
        };
        drop(tmp);
        let commit = receipt.committed.then_some(receipt.commit.seq);
        ds.dataset.mappings().record_run(
            &stored_name,
            LastRun {
                time: sparkles::builder::now_rfc3339(),
                version,
                mode: if replace { "replace" } else { "add" }.into(),
                commit,
                quads,
                rows,
                warnings: warnings.clone(),
                author,
            },
        )?;
        let body = json!({
            "mapping": stored_name,
            "version": version,
            "mode": if replace { "replace" } else { "add" },
            "rows": rows,
            "quads": quads,
            "warnings": warnings,
            "reports": reports,
            "count": if receipt.committed { receipt.commit.inserted } else { 0 },
        });
        Ok(
            super::write_report(crate::obs::Op::Upload, quads).attach(write_response(
                &ds,
                StatusCode::OK,
                Some(body),
                &receipt,
                wanted,
                restricted,
            )),
        )
    })
    .await
    .map_err(|e| super::with_timeout(e, timeout))
}
