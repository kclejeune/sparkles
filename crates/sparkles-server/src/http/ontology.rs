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
use sparkles::handles::ontology::{
    self as onto, CheckRequest, EditRequest, OntologyRequest, Opened,
};
use sparkles::owl::{self, Axiom, ofn::Renderer, profiles::Profile};
use sparkles_reasoner::inputs::{GraphRef, ImportMode};

/// The most axioms one page lists.
const MAX_LIMIT: usize = 10_000;
/// The axioms a page lists by default.
const DEFAULT_LIMIT: usize = 100;
/// The diagnostics a summary lists.
const SUMMARY_DIAGNOSTICS: usize = 100;

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

fn header_json(h: &owl::Header, r: &Renderer) -> J {
    json!({
        "iri": h.iri.as_ref().map(|i| i.as_str()),
        "versionIri": h.version_iri.as_ref().map(|i| i.as_str()),
        "imports": h.imports.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
        "annotations": h.annotations.iter().map(|a| r.annotation(a)).collect::<Vec<_>>(),
    })
}

fn opened_json(name: &str, o: &Opened, req: &OntologyRequest) -> J {
    json!({
        "dataset": name,
        "commit": o.snapshot.commit,
        "mode": req.mode.as_str(),
        "graphs": o.graphs.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "imports": o.imports.iter().map(|i| json!({"iri": i.iri, "graph": i.graph})).collect::<Vec<_>>(),
        "warnings": o.warnings,
    })
}

/// The JSON of the summary of an open ontology, as `GET /$/ontology/{ds}` answers it.
pub(crate) fn summary_body(name: &str, o: &Opened, req: &OntologyRequest, r: &Renderer) -> J {
    let s = o.summary();
    let mut j = opened_json(name, o, req);
    j["header"] = header_json(&s.header, r);
    j["axioms"] = json!(
        s.axioms
            .iter()
            .map(|(t, n)| (t.name().to_string(), json!(*n)))
            .collect::<serde_json::Map<String, J>>()
    );
    j["axiomCount"] = json!(s.axioms.values().sum::<usize>());
    j["entities"] = json!(
        s.entities
            .iter()
            .map(|(k, n)| (k.name().to_string(), json!(*n)))
            .collect::<serde_json::Map<String, J>>()
    );
    j["triples"] = json!(s.triples);
    j["unmapped"] = json!(s.unmapped);
    j["diagnostics"] = json!({
        "warnings": s.warnings,
        "errors": s.errors,
        "list": o.ontology.diagnostics.iter().take(SUMMARY_DIAGNOSTICS).map(|d| json!({
            "severity": d.severity.as_str(),
            "code": d.code,
            "message": d.message,
        })).collect::<Vec<_>>(),
    });
    j["cached"] = json!(o.cached);
    j
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

/// What a listing of axioms keeps.
pub(crate) struct AxiomQuery {
    pub types: Option<std::collections::BTreeSet<owl::AxiomType>>,
    pub about: Option<String>,
    pub assertions: bool,
    pub limit: usize,
    pub offset: usize,
}

/// One page of axioms, with the graph each was read from, and the total.
pub(crate) struct Page {
    pub items: Vec<(Axiom, Option<u32>)>,
    pub total: usize,
    pub offset: usize,
}

/// The page `q` asks for: the terminology's axioms in order, then the assertions the
/// store holds when `q.assertions` is set.
pub(crate) fn axiom_page(
    o: &Opened,
    q: &AxiomQuery,
    opts: &sparkles::sparql::QueryOptions,
) -> sparkles::Result<Page> {
    let keep = |ax: &Axiom| {
        q.types
            .as_ref()
            .is_none_or(|t| t.contains(&ax.axiom_type()))
            && q.about.as_deref().is_none_or(|a| ax.mentions(a))
    };
    let mut items: Vec<(Axiom, Option<u32>)> = Vec::new();
    let mut total = 0usize;
    for (ax, origins) in &o.ontology.axioms {
        if keep(ax) {
            if total >= q.offset && items.len() < q.limit {
                items.push((ax.clone(), origins.first().map(|x| x.graph)));
            }
            total += 1;
        }
    }
    let wants_assertions = q
        .types
        .as_ref()
        .is_none_or(|t| t.iter().any(|t| t.is_assertion()));
    if q.assertions && wants_assertions {
        o.assertions(opts, |ax, _, g| {
            if let Some(ax) = ax
                && keep(&ax)
            {
                if total >= q.offset && items.len() < q.limit {
                    items.push((ax, Some(g)));
                }
                total += 1;
            }
            Ok(true)
        })?;
    }
    Ok(Page {
        items,
        total,
        offset: q.offset,
    })
}

/// The JSON of a page of axioms.
pub(crate) fn page_body(
    name: &str,
    o: &Opened,
    req: &OntologyRequest,
    r: &Renderer,
    page: &Page,
) -> J {
    let next = page.offset + page.items.len();
    let list: Vec<J> = page
        .items
        .iter()
        .map(|(ax, g)| {
            let mut j = json!({
                "type": ax.axiom_type().name(),
                "ofn": r.axiom(ax),
                "axiom": owl::json::axiom(ax),
            });
            if let Some(g) = g {
                j["graph"] = o.graph_name(*g).into();
            }
            j
        })
        .collect();
    let mut j = opened_json(name, o, req);
    j["axioms"] = J::Array(list);
    j["total"] = json!(page.total);
    j["offset"] = json!(page.offset);
    j["next"] = if next < page.total {
        json!(next)
    } else {
        J::Null
    };
    j
}

/// The JSON of the reports of a check.
pub(crate) fn check_body(
    name: &str,
    o: &Opened,
    req: &OntologyRequest,
    r: &Renderer,
    reports: &[owl::profiles::Report],
    assertions: bool,
) -> J {
    let mut j = opened_json(name, o, req);
    j["conforms"] = json!(reports.iter().all(|r| r.conforms));
    j["assertions"] = json!(assertions);
    j["reports"] = J::Array(reports.iter().map(|x| onto::report_json(o, x, r)).collect());
    j
}

/// The JSON of what an edit did.
pub(crate) fn edit_body(e: &onto::EditOutcome, r: &Renderer) -> J {
    json!({
        "added": e.added,
        "present": e.present,
        "removed": e.removed,
        "absent": e.absent.iter().map(|a| r.axiom(a)).collect::<Vec<_>>(),
        "declared": e.declared.iter().map(|d| r.entity(d)).collect::<Vec<_>>(),
        "inserted": e.inserted,
        "deleted": e.deleted,
        "kept": e.kept,
        "committed": e.receipt.committed,
    })
}

/// Whether the ontology's terminology declares or uses `iri`.
pub(crate) fn mentioned(o: &Opened, iri: &str) -> bool {
    o.ontology.axiom_set().any(|a| a.mentions(iri))
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

/// The JSON of one axiom of a listing or a diff.
fn axiom_json(ax: &Axiom, graph: Option<&str>, r: &Renderer) -> J {
    let mut j = json!({
        "type": ax.axiom_type().name(),
        "ofn": r.axiom(ax),
        "axiom": owl::json::axiom(ax),
    });
    if let Some(g) = graph {
        j["graph"] = g.into();
    }
    j
}

/// The JSON of an ontology diff, with at most `limit` axioms on each side.
pub(crate) fn diff_body(
    name: &str,
    d: &onto::OntologyDiff,
    req: &OntologyRequest,
    assertions: bool,
    limit: usize,
    r: &Renderer,
) -> J {
    let side = |v: &[onto::AxiomChange]| -> J {
        J::Array(
            v.iter()
                .take(limit)
                .map(|c| axiom_json(&c.axiom, Some(&c.graph), r))
                .collect(),
        )
    };
    json!({
        "dataset": name,
        "from": d.from,
        "to": d.to,
        "graphs": req.graphs.iter().map(|g| g.as_str()).collect::<Vec<_>>(),
        "mode": req.mode.as_str(),
        "assertions": assertions,
        "added": side(&d.added),
        "removed": side(&d.removed),
        "addedCount": d.added.len(),
        "removedCount": d.removed.len(),
        "truncated": d.added.len() > limit || d.removed.len() > limit,
        "changedQuads": d.changed_quads,
        "method": d.method.as_str(),
    })
}

/// The diff as lines: `+ axiom` for each added axiom, then `- axiom` for each removed
/// one, in functional-style syntax with the dataset's prefixes.
pub(crate) fn diff_lines(d: &onto::OntologyDiff, limit: usize, r: &Renderer) -> String {
    let mut s = String::new();
    for c in d.added.iter().take(limit) {
        s.push_str("+ ");
        s.push_str(&r.axiom(&c.axiom));
        s.push('\n');
    }
    for c in d.removed.iter().take(limit) {
        s.push_str("- ");
        s.push_str(&r.axiom(&c.axiom));
        s.push('\n');
    }
    s
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
