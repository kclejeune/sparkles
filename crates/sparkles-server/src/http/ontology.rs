//! The OWL 2 ontology of a dataset over HTTP (spec F21 §2.4): the summary
//! `GET /$/ontology/{ds}`, the axioms `GET /$/ontology/{ds}/axioms`, the profile checks
//! `POST /$/ontology/{ds}/check`, edits `POST /$/ontology/{ds}/axioms` and the diff of
//! the axioms of two commits `GET /$/ontology/{ds}/diff`. The library
//! reads, checks and edits (`sparkles::handles::ontology`); these handlers parse the
//! request and apply the caller's grants through [`CallerContext`].
//!
//! Every route reads the graphs `graph` names (the default graph when none), with the
//! imports that resolve to graphs of the dataset unless `imports` is false, in the
//! `mode` asked for (lenient by default). The summary carries an entity tag that names
//! its commit, which an edit's `If-Match` can name.

use super::*;
use crate::auth::Endpoint;
use sparkles::handles::ontology::{self as onto, CheckRequest, EditRequest, OntologyRequest};
use sparkles::owl::{self, Axiom, ofn::Renderer, profiles::Profile};
use sparkles_reasoner::inputs::{GraphRef, ImportMode};

pub(crate) use onto::wire::{
    AxiomQuery, DEFAULT_LIMIT, MAX_LIMIT, axiom_page, check_body, diff_body, diff_lines, edit_body,
    mentioned, page_body, summary_body,
};

fn bad(m: impl Into<String>) -> ApiError {
    err_code(StatusCode::BAD_REQUEST, "invalid-request", m)
}

fn graph_ref(s: &str) -> ApiResult<GraphRef> {
    GraphRef::parse(s.trim_start_matches('<').trim_end_matches('>')).map_err(bad)
}

fn imports_of(v: &str) -> ApiResult<ImportMode> {
    match v {
        "true" | "dataset" => Ok(ImportMode::Dataset),
        "false" | "none" => Ok(ImportMode::None),
        v => Err(bad(format!("imports must be true or false, not '{v}'"))),
    }
}

fn mode_of(v: &str) -> ApiResult<owl::Mode> {
    owl::Mode::parse(v).ok_or_else(|| bad(format!("mode must be lenient or strict, not '{v}'")))
}

/// The ontology the query parameters name.
fn request_of(params: &Params) -> ApiResult<OntologyRequest> {
    let mut req = OntologyRequest::default();
    let graphs = params.all("graph");
    if !graphs.is_empty() {
        req.graphs = graphs
            .iter()
            .map(|g| graph_ref(g))
            .collect::<ApiResult<_>>()?;
    }
    if let Some(v) = params.get("imports") {
        req.imports = imports_of(v)?;
    }
    if let Some(v) = params.get("mode") {
        req.mode = mode_of(v)?;
    }
    req.at = history::at_param(params)?;
    if let Some(v) = params.get("inferences") {
        req.inferences = truthy(v);
    }
    Ok(req)
}

/// The members of a body that name the ontology: `graph` (a string or a list),
/// `imports` and `mode`, over what the query parameters say.
fn request_body(
    o: &serde_json::Map<String, J>,
    mut req: OntologyRequest,
) -> ApiResult<OntologyRequest> {
    match o.get("graph") {
        None | Some(J::Null) => {}
        Some(J::String(g)) => req.graphs = vec![graph_ref(g)?],
        Some(J::Array(a)) => {
            req.graphs = a
                .iter()
                .map(|g| {
                    g.as_str()
                        .ok_or_else(|| bad("graph: a string or a list of strings"))
                        .and_then(graph_ref)
                })
                .collect::<ApiResult<_>>()?;
        }
        Some(_) => return Err(bad("graph: a string or a list of strings")),
    }
    match o.get("imports") {
        None | Some(J::Null) => {}
        Some(J::Bool(b)) => {
            req.imports = if *b {
                ImportMode::Dataset
            } else {
                ImportMode::None
            }
        }
        Some(J::String(s)) => req.imports = imports_of(s)?,
        Some(_) => return Err(bad("imports: true or false")),
    }
    match o.get("mode") {
        None | Some(J::Null) => {}
        Some(J::String(s)) => req.mode = mode_of(s)?,
        Some(_) => return Err(bad("mode: lenient or strict")),
    }
    match o.get("inferences") {
        None | Some(J::Null) => {}
        Some(J::Bool(b)) => req.inferences = *b,
        Some(_) => return Err(bad("inferences: true or false")),
    }
    Ok(req)
}

fn body_object(bytes: &[u8], allowed: &[&str]) -> ApiResult<serde_json::Map<String, J>> {
    let j: J = if bytes.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        serde_json::from_slice(bytes).map_err(|e| bad(format!("invalid JSON: {e}")))?
    };
    let J::Object(o) = j else {
        return Err(bad("the body is a JSON object"));
    };
    if let Some(k) = o.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(bad(format!("unknown member '{k}': {}", allowed.join(", "))));
    }
    Ok(o)
}

fn renderer(ds: &Dataset) -> Renderer {
    Renderer::new(ds.store.prefixes())
}

/// The library error of a request, with `invalid-axiom` for an axiom the forward
/// mapping cannot write.
fn ontology_error(e: Error) -> ApiError {
    match e {
        Error::Invalid(m) if m.contains("operands") || m.contains("anonymous individual") => {
            err_code(StatusCode::BAD_REQUEST, "invalid-axiom", m)
        }
        e => e.into(),
    }
}

/// `GET /$/ontology/{ds}`: the header, the counts of entities and axioms, the graphs
/// read and the unmapped triples.
pub(super) async fn summary(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let req = request_of(&params)?;
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        // one deadline for the read and everything after it
        let opts = ds
            .dataset
            .ontology()
            .anchored(&opts)
            .map_err(ontology_error)?;
        let o = ds
            .dataset
            .ontology()
            .open(&req, &opts)
            .map_err(ontology_error)?;
        let j = summary_body(&ds.name, &o, &req, &renderer(&ds));
        let seq = o.snapshot.commit;
        let tag = conditional::etag(ds.store.dataset_id(), seq, "owl");
        let resp = match conditional::check_read(&headers, ds.store.dataset_id(), seq, &tag)? {
            conditional::ReadOutcome::NotModified => conditional::not_modified(&tag),
            conditional::ReadOutcome::Send => {
                conditional::with_etag(Json(j).into_response(), &tag, false)
            }
        };
        Ok(with_commit(resp, &ds, seq))
    })
    .await
}

/// `GET /$/ontology/{ds}/axioms`: the axioms with a type in `type` and mentioning
/// `about`, one page at a time (`limit`, `offset`), as JSON or as a functional-style
/// document (`format=ofn`). `assertions=true` adds the assertions the store holds.
pub(super) async fn axioms(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let req = request_of(&params)?;
    let types = match params.get("type") {
        None => None,
        Some(t) => Some(onto::parse_axiom_types(t).map_err(|e| bad(e.to_string()))?),
    };
    let about = params
        .get("about")
        .map(|a| a.trim_start_matches('<').trim_end_matches('>').to_string());
    let ofn = match params.get("format") {
        None | Some("json") => false,
        Some("ofn") => true,
        Some(f) => return Err(bad(format!("format must be json or ofn, not '{f}'"))),
    };
    let assertions = match params.get("assertions") {
        None => false,
        Some(v) => truthy(v),
    };
    let number = |k: &str, default: usize, max: usize| -> ApiResult<usize> {
        match params.get(k) {
            None => Ok(default),
            Some(v) => v
                .parse::<usize>()
                .ok()
                .filter(|n| *n <= max)
                .ok_or_else(|| bad(format!("{k} must be an integer from 0 to {max}"))),
        }
    };
    let limit = number("limit", DEFAULT_LIMIT, MAX_LIMIT)?.max(1);
    let offset = number("offset", 0, usize::MAX)?;
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        // one deadline for the read and everything after it
        let opts = ds
            .dataset
            .ontology()
            .anchored(&opts)
            .map_err(ontology_error)?;
        let o = ds
            .dataset
            .ontology()
            .open(&req, &opts)
            .map_err(ontology_error)?;
        let q = AxiomQuery {
            types,
            about,
            assertions,
            limit,
            offset,
        };
        let page = axiom_page(&o, &q, &opts).map_err(ontology_error)?;
        if let Some(a) = &q.about
            && page.total == 0
            && !mentioned(&o, a)
        {
            return Err(err_code(
                StatusCode::NOT_FOUND,
                "not-found",
                format!("the ontology neither declares nor uses <{a}>"),
            ));
        }
        let r = renderer(&ds);
        let seq = o.snapshot.commit;
        if ofn {
            let doc = owl::ofn::document(&r, &o.ontology.header, page.items.iter().map(|(a, _)| a));
            let resp = (
                [(header::CONTENT_TYPE, "text/owl-functional; charset=utf-8")],
                doc,
            )
                .into_response();
            return Ok(with_commit(resp, &ds, seq));
        }
        let j = page_body(&ds.name, &o, &req, &r, &page);
        Ok(with_commit(Json(j).into_response(), &ds, seq))
    })
    .await
}

/// `POST /$/ontology/{ds}/check`: one report per profile in `profiles` (`dl` when
/// none), over the terminology and, unless `assertions` is false, the assertions.
pub(super) async fn check(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    bytes: Bytes,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let o = body_object(
        &bytes,
        &[
            "graph",
            "imports",
            "mode",
            "inferences",
            "profiles",
            "assertions",
            "maxViolations",
        ],
    )?;
    let req = request_body(&o, request_of(&params)?)?;
    let mut check = CheckRequest::default();
    if let Some(v) = params.get("profiles") {
        check.profiles = onto::parse_profiles(v).map_err(|e| bad(e.to_string()))?;
    }
    match o.get("profiles") {
        None | Some(J::Null) => {}
        Some(J::Array(a)) => {
            let mut ps: Vec<Profile> = Vec::new();
            for v in a {
                let s = v
                    .as_str()
                    .ok_or_else(|| bad("profiles: a list of el, ql, rl and dl"))?;
                ps.extend(onto::parse_profiles(s).map_err(|e| bad(e.to_string()))?);
            }
            check.profiles = ps;
        }
        Some(J::String(s)) => {
            check.profiles = onto::parse_profiles(s).map_err(|e| bad(e.to_string()))?
        }
        Some(_) => return Err(bad("profiles: a list of el, ql, rl and dl")),
    }
    if check.profiles.is_empty() {
        check.profiles = vec![Profile::Dl];
    }
    match o.get("assertions") {
        None | Some(J::Null) => {}
        Some(J::Bool(b)) => check.assertions = *b,
        Some(_) => return Err(bad("assertions: true or false")),
    }
    match o.get("maxViolations") {
        None | Some(J::Null) => {}
        Some(v) => {
            check.max_violations = v
                .as_u64()
                .filter(|n| (1..=MAX_LIMIT as u64).contains(n))
                .ok_or_else(|| bad(format!("maxViolations: an integer from 1 to {MAX_LIMIT}")))?
                as usize;
        }
    }
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        // one deadline for the read and everything after it
        let opts = ds
            .dataset
            .ontology()
            .anchored(&opts)
            .map_err(ontology_error)?;
        let o = ds
            .dataset
            .ontology()
            .open(&req, &opts)
            .map_err(ontology_error)?;
        let reports = onto::check_opened(&o, &check, &opts).map_err(ontology_error)?;
        let j = check_body(
            &ds.name,
            &o,
            &req,
            &renderer(&ds),
            &reports,
            check.assertions,
        );
        Ok(with_commit(Json(j).into_response(), &ds, o.snapshot.commit))
    })
    .await
}

/// The axioms of a body member, in their JSON form.
fn axiom_list(o: &serde_json::Map<String, J>, k: &str) -> ApiResult<Vec<Axiom>> {
    match o.get(k) {
        None | Some(J::Null) => Ok(Vec::new()),
        Some(J::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(i, v)| {
                owl::json::read_axiom(v).map_err(|e| {
                    err_code(
                        StatusCode::BAD_REQUEST,
                        "invalid-axiom",
                        format!("{k}[{i}]: {e}"),
                    )
                })
            })
            .collect(),
        Some(_) => Err(bad(format!("{k}: a list of axioms"))),
    }
}

/// `POST /$/ontology/{ds}/axioms`: add and remove axioms in one commit. Takes the
/// write parameters of the other write routes (`dryRun`, `receipt`, the commit
/// message header) and `If-Match` with the summary's tag.
pub(super) async fn edit(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    history::reject_at(&params)?;
    let o = body_object(
        &bytes,
        &[
            "graph", "imports", "mode", "add", "remove", "declare", "target", "message",
        ],
    )?;
    let req = request_body(&o, request_of(&params)?)?;
    let edit = EditRequest {
        add: axiom_list(&o, "add")?,
        remove: axiom_list(&o, "remove")?,
        declare: match o.get("declare") {
            None | Some(J::Null) => None,
            Some(J::Bool(b)) => Some(*b),
            Some(_) => return Err(bad("declare: true or false")),
        },
        graph: match o.get("target") {
            None | Some(J::Null) => None,
            Some(J::String(g)) => Some(graph_ref(g)?),
            Some(_) => return Err(bad("target: the graph that takes the added axioms")),
        },
        message: match o.get("message") {
            None | Some(J::Null) => None,
            Some(J::String(m)) => Some(m.clone()),
            Some(_) => return Err(bad("message: a string")),
        },
    };
    if edit.add.is_empty() && edit.remove.is_empty() {
        return Err(bad("name axioms to add or remove"));
    }
    let mut opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Update);
    let restricted = opts.graphs.is_some();
    let wanted = receipt_wanted(&params, &headers);
    let (mut wopts, _cancel_on_drop) = body_write_options(&st, &params, &headers, Some(&p))?;
    wopts.opts.graphs = opts.graphs.clone();
    if wopts.opts.precondition.is_none() {
        wopts.opts.precondition =
            conditional::write_precondition(&headers, ds.store.dataset_id(), &Target::Dataset);
    }
    let dry = wopts
        .opts
        .dry_run
        .as_ref()
        .map(|d| {
            dry_run::Request::for_write(
                &st,
                &headers,
                &params,
                d,
                restricted,
                Some(&p),
                &ds,
                &wopts.opts,
            )
        })
        .transpose()?;
    let (wopts, timeout) = wopts.start();
    let dry = dry.map(|request| request.started(&wopts));
    opts.cancel = wopts.cancel.clone();
    opts.deadline = wopts.deadline;
    opts.timeout = None;
    opts.write = wopts;
    blocking(move || {
        let r = ds.dataset.ontology().edit(&req, &edit, &opts);
        let e = match (r, dry) {
            (Err(Error::DryRun(preview)), Some(d)) => return Ok(d.respond(&ds, &preview)),
            (Err(e @ Error::Invalid(_)), _) => return Err(ontology_error(e)),
            (r, _) => r.map_err(|e| write_error(e, restricted))?,
        };
        let body = edit_body(&e, &renderer(&ds));
        Ok(write_response(
            &ds,
            StatusCode::OK,
            Some(body),
            &e.receipt,
            wanted,
            restricted,
        ))
    })
    .await
    .map_err(|e| with_timeout(e, timeout))
}

/// `GET /$/ontology/{ds}/diff?from=&to=`: the axioms that differ between two commits
/// (`to` is the head by default), read as the summary reads the ontology, with the
/// assertions that the changed triples carry when `assertions=true`. `format=text`
/// answers `+ axiom` and `- axiom` lines. `limit` caps each side (1,000 by default).
pub(super) async fn diff(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    if params.get("at").is_some() {
        return Err(bad("a diff takes from and to, not at"));
    }
    let req = request_of(&params)?;
    let from = super::diff::selector(&params, "from")?
        .ok_or_else(|| bad("from names the earlier commit"))?;
    let to = super::diff::selector(&params, "to")?.unwrap_or(sparkles::history::At::Head);
    let assertions = params.get("assertions").is_some_and(truthy);
    let text = match params.get("format") {
        None | Some("json") => false,
        Some("text") => true,
        Some(f) => return Err(bad(format!("format must be json or text, not '{f}'"))),
    };
    let limit = match params.get("limit") {
        None => 1000,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or_else(|| bad(format!("limit must be an integer from 1 to {MAX_LIMIT}")))?,
    };
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Diff);
    blocking(move || {
        let d = ds
            .dataset
            .ontology()
            .diff(&req, &from, &to, assertions, &opts)
            .map_err(ontology_error)?;
        let r = renderer(&ds);
        let resp = if text {
            (
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                diff_lines(&d, limit, &r),
            )
                .into_response()
        } else {
            Json(diff_body(&ds.name, &d, &req, assertions, limit, &r)).into_response()
        };
        Ok(with_commit(resp, &ds, d.to))
    })
    .await
}

#[cfg(test)]
mod tests;
