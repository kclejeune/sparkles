//! The OWL 2 ontology of a dataset over HTTP (spec F21 §2.4): the summary
//! `GET /$/ontology/{ds}`, the axioms `GET /$/ontology/{ds}/axioms`, the entities
//! `GET /$/ontology/{ds}/entities` and `GET /$/ontology/{ds}/entity`, the removal of an
//! entity `DELETE /$/ontology/{ds}/entity`, the profile checks
//! `POST /$/ontology/{ds}/check`, edits `POST /$/ontology/{ds}/axioms` and the diff of
//! the axioms of two commits `GET /$/ontology/{ds}/diff`, the OWL 2 EL
//! classification `POST /$/ontology/{ds}/classify` (spec F24), the entity search
//! `GET /$/ontology/{ds}/search` (item 616), and the Manchester reader
//! `POST /$/ontology/{ds}/parse` and DL queries `POST /$/ontology/{ds}/dl-query`
//! (spec U12). The library
//! reads, checks and edits (`sparkles::handles::ontology`); these handlers parse the
//! request and apply the caller's grants through [`CallerContext`].
//!
//! Every route reads the graphs `graph` names (the default graph when none), with the
//! imports that resolve to graphs of the dataset unless `imports` is false, in the
//! `mode` asked for (lenient by default). The summary carries an entity tag that names
//! its commit, which an edit's `If-Match` can name.

use super::*;
use crate::auth::Endpoint;
use sparkles::handles::ontology::classify::ClassifyRequest;
use sparkles::handles::ontology::{self as onto, CheckRequest, EditRequest, OntologyRequest};
use sparkles::owl::{self, Axiom, ofn::Renderer, profiles::Profile};
use sparkles_reasoner::inputs::{GraphRef, ImportMode};

pub(crate) use onto::wire::{
    AxiomQuery, ClassifyQuery, DEFAULT_LIMIT, MAX_LIMIT, axiom_page, check_body, classify_body,
    diff_body, diff_lines, edit_body, expansion_of, mentioned, page_body, summary_body,
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

fn classify_object(
    bytes: &[u8],
    query_projection: bool,
    allowed: &[&str],
) -> ApiResult<serde_json::Map<String, J>> {
    let empty = b"{}";
    let bytes = if bytes.iter().all(u8::is_ascii_whitespace) {
        &empty[..]
    } else {
        bytes
    };
    let value = if query_projection {
        onto::wire::decode_strict_arguments(bytes)
    } else {
        onto::wire::decode_arguments("classify", bytes)
    }
    .map_err(ontology_error)?;
    let J::Object(object) = value else {
        return Err(bad("the body is a JSON object"));
    };
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(bad(format!("unknown member '{key}'")));
    }
    Ok(object)
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
    let metrics = match params.get("metrics") {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(bad("metrics must be true or false")),
    };
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
        let mut j = summary_body(&ds.name, &o, &req, &renderer(&ds));
        if metrics {
            j["metrics"] = onto::metrics::body(&ds.name, &o, &req, &renderer(&ds), &opts)
                .map_err(ontology_error)?;
        }
        let seq = o.snapshot.commit;
        let tag = conditional::etag(
            ds.store.dataset_id(),
            seq,
            if metrics { "owl-metrics" } else { "owl" },
        );
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

fn number_param(
    params: &Params,
    k: &str,
    default: usize,
    min: usize,
    max: usize,
) -> ApiResult<usize> {
    match params.get(k) {
        None => Ok(default),
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| bad(format!("{k} must be an integer from {min} to {max}"))),
    }
}

/// `GET /$/ontology/{ds}/entities`: the entities the ontology declares or, in a lenient
/// read, types by use, of the kinds in `kind` (all by default), one page at a time.
pub(super) async fn entities(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let req = request_of(&params)?;
    let kinds = match params.get("kind") {
        None => None,
        Some(k) => Some(onto::wire::parse_entity_kinds(k).map_err(|e| bad(e.to_string()))?),
    };
    let limit = number_param(&params, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?;
    let offset = number_param(&params, "offset", 0, 0, usize::MAX)?;
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        let handle = ds.dataset.ontology();
        let opts = handle.anchored(&opts).map_err(ontology_error)?;
        let o = handle.open(&req, &opts).map_err(ontology_error)?;
        let j = onto::wire::entities_body(&ds.name, &o, &req, kinds.as_ref(), limit, offset);
        Ok(with_commit(Json(j).into_response(), &ds, o.snapshot.commit))
    })
    .await
}

/// `GET /$/ontology/{ds}/entity`: what the ontology says about the entity `iri`: its
/// kinds, labels and annotations, and for each kind its superclasses, domains, ranges,
/// types and the other accessors of spec F21 §2.1. `assertions=true` also streams the
/// instances of a class and the types and property assertions of an individual from the
/// store, at most `limit` of each.
pub(super) async fn entity(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let req = request_of(&params)?;
    let iri = params
        .get("iri")
        .ok_or_else(|| bad("iri names the entity"))?
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string();
    let kind = match params.get("kind") {
        None => None,
        Some(k) => Some(owl::EntityKind::parse(k).ok_or_else(|| {
            bad(format!(
                "kind must be one of {}, not '{k}'",
                owl::EntityKind::ALL.map(|k| k.name()).join(", ")
            ))
        })?),
    };
    let q = onto::wire::EntityQuery {
        iri,
        kind,
        assertions: params.get("assertions").is_some_and(truthy),
        limit: number_param(&params, "limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?,
    };
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        let handle = ds.dataset.ontology();
        let opts = handle.anchored(&opts).map_err(ontology_error)?;
        let o = handle.open(&req, &opts).map_err(ontology_error)?;
        let j = onto::wire::entity_body(&ds.name, &o, &req, &renderer(&ds), &q, &opts)
            .map_err(ontology_error)?;
        Ok(with_commit(Json(j).into_response(), &ds, o.snapshot.commit))
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

/// `POST /$/ontology/{ds}/parse`: read Manchester `text` against the ontology (spec
/// U12), as `as` says: a class expression (the default), a data range, an individual,
/// a literal or an axiom. The answer has the value in its JSON form, in functional-style
/// syntax and as the Manchester renderer writes it back. Text that does not parse is a
/// `400` with the code `invalid-manchester`, its `reason` (`syntax`, `unknown`,
/// `ambiguous` or `kind`), its span as `start` and `end` in UTF-16 code units, and the
/// `candidates` of an ambiguous name.
pub(super) async fn parse(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    bytes: Bytes,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let o = body_object(&bytes, &["graph", "imports", "mode", "text", "as"])?;
    let req = request_body(&o, request_of(&params)?)?;
    if req.inferences {
        return Err(bad(
            "a parse reads the asserted axioms: leave out 'inferences'",
        ));
    }
    let text = match o.get("text") {
        Some(J::String(s)) => s.clone(),
        _ => return Err(bad("text: the Manchester text to read")),
    };
    let what = match o.get("as") {
        None | Some(J::Null) => onto::wire::what_of(None),
        Some(J::String(s)) => onto::wire::what_of(Some(s)),
        Some(_) => return Err(bad("as: a string")),
    }
    .map_err(ontology_error)?;
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        let handle = ds.dataset.ontology();
        let opts = handle.anchored(&opts).map_err(ontology_error)?;
        let o = handle.open(&req, &opts).map_err(ontology_error)?;
        let parsed = handle
            .parse_manchester(&o, &text, what, &opts)
            .map_err(ontology_error)?;
        let body = onto::wire::parse_body(
            &ds.name,
            &o,
            &req,
            &renderer(&ds),
            &handle.vocabulary(&o),
            what,
            &parsed,
        );
        Ok(with_commit(
            Json(body).into_response(),
            &ds,
            o.snapshot.commit,
        ))
    })
    .await
}

/// `POST /$/ontology/{ds}/dl-query`: a DL query (spec U12). `expression` is a class
/// expression in Manchester syntax, in functional-style syntax with
/// `syntax: "functional"`, or in its JSON form. `reasoning` is `auto` (the default),
/// `el` or `rl`, `answers` lists the answers wanted (every one by default), and `limit`
/// caps each list. The answer lists the classes and individuals by IRI, with the
/// reasoning used, its gaps and notes, and the counts.
pub(super) async fn dl_query(
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
            "expression",
            "syntax",
            "reasoning",
            "answers",
            "limit",
            "threads",
            "maxDerived",
        ],
    )?;
    let req = request_body(&o, request_of(&params)?)?;
    let (q, limit) = onto::wire::dl_query_of(&o).map_err(ontology_error)?;
    let mut opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    // a client that goes away stops the run
    let (cancel, _cancel_on_drop) = cancel_on_drop();
    opts.cancel = Some(cancel);
    blocking(move || {
        let handle = ds.dataset.ontology();
        let opts = handle.anchored(&opts).map_err(ontology_error)?;
        let opened = handle.open(&req, &opts).map_err(ontology_error)?;
        let prefixes: Vec<(String, String)> = ds.dataset.prefixes().into_iter().collect();
        let q = onto::wire::dl_expression(&handle, &opened, &o, q, &prefixes, &opts)
            .map_err(ontology_error)?;
        let out = handle
            .dl_query_opened(opened, &req, &q, &opts)
            .map_err(ontology_error)?;
        let seq = out.opened.snapshot.commit;
        let body = onto::wire::dl_query_body(
            &ds.name,
            &out,
            &req,
            &renderer(&ds),
            &handle.vocabulary(&out.opened),
            limit,
        );
        Ok(with_commit(Json(body).into_response(), &ds, seq))
    })
    .await
}

/// The axioms of a body member, each in its JSON form or as a string in functional-style
/// syntax with the dataset's prefixes, as the library's edit takes them.
fn axiom_list(
    o: &serde_json::Map<String, J>,
    k: &str,
    prefixes: &[(String, String)],
) -> ApiResult<Vec<Axiom>> {
    match o.get(k) {
        None | Some(J::Null) => Ok(Vec::new()),
        Some(J::Array(a)) => a
            .iter()
            .enumerate()
            .map(|(i, v)| {
                onto::wire::axiom_of(v, &format!("{k}[{i}]"), prefixes).map_err(|e| {
                    let m = match e {
                        Error::Invalid(m) => m,
                        e => e.to_string(),
                    };
                    err_code(StatusCode::BAD_REQUEST, "invalid-axiom", m)
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
    let prefixes: Vec<(String, String)> = ds.dataset.prefixes().into_iter().collect();
    let edit = EditRequest {
        add: axiom_list(&o, "add", &prefixes)?,
        remove: axiom_list(&o, "remove", &prefixes)?,
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

/// `DELETE /$/ontology/{ds}/entity?iri=IRI`: remove an entity in one commit, with its
/// declarations, the axioms that mention it and the assertions about it in the graphs
/// read (spec F21 §5.3). Takes the write parameters of the other write routes (`dryRun`,
/// `receipt`, the commit message header or `message`) and `If-Match` with the summary's
/// tag. Answers as an edit does, so `removedAxioms` lists what the removal took.
pub(super) async fn remove_entity(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    history::reject_at(&params)?;
    let req = request_of(&params)?;
    if req.inferences {
        return Err(bad(
            "a removal changes the asserted axioms: leave out 'inferences'",
        ));
    }
    let iri = params
        .get("iri")
        .ok_or_else(|| bad("iri names the entity to remove"))?
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string();
    if iri.is_empty() {
        return Err(bad("iri names the entity to remove"));
    }
    let mut opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Update);
    let restricted = opts.graphs.is_some();
    let wanted = receipt_wanted(&params, &headers);
    let (mut wopts, _cancel_on_drop) = body_write_options(&st, &params, &headers, Some(&p))?;
    wopts.opts.graphs = opts.graphs.clone();
    if let Some(m) = params.get("message") {
        wopts.opts.message = Some(std::sync::Arc::from(m));
    }
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
        let r = ds.dataset.ontology().remove_entity(&req, &iri, &opts);
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

/// The language tags of an `Accept-Language` header, most preferred first: tags with a
/// weight of zero, `*` and malformed tags are left out.
fn accept_languages(headers: &HeaderMap) -> Vec<String> {
    let Some(v) = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
    else {
        return Vec::new();
    };
    let mut tags: Vec<(u32, usize, String)> = Vec::new();
    for (i, part) in v.split(',').enumerate() {
        let mut it = part.split(';');
        let tag = it.next().unwrap_or("").trim();
        let mut q = 1000u32;
        for p in it {
            if let Some(w) = p.trim().strip_prefix("q=") {
                q = w
                    .trim()
                    .parse::<f32>()
                    .map_or(0, |w| (w.clamp(0.0, 1.0) * 1000.0) as u32);
            }
        }
        if q == 0 || tag.is_empty() || tag == "*" {
            continue;
        }
        if oxilangtag::LanguageTag::parse(tag).is_err() {
            continue;
        }
        tags.push((q, i, tag.to_string()));
    }
    tags.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    tags.into_iter().map(|(_, _, t)| t).collect()
}

/// `GET /$/ontology/{ds}/search`: the entities whose labels, synonyms, short forms or
/// definitions match `q`, ranked for typeahead with typo tolerance, from the branch's
/// entity index (`kind`, `lang` or `Accept-Language`, `limit`, `offset`, `fuzzy`,
/// `definitions`, and the graphs of the ontology read).
pub(super) async fn search(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let req = request_of(&params)?;
    let mut a = serde_json::Map::new();
    let q = params
        .get("q")
        .ok_or_else(|| bad("q is the text searched for"))?;
    a.insert("q".into(), json!(q));
    let kinds = params.all("kind");
    if !kinds.is_empty() {
        a.insert("kind".into(), json!(kinds.join(",")));
    }
    match params.get("lang") {
        Some(l) => {
            a.insert("lang".into(), json!(l));
        }
        None => {
            a.insert("lang".into(), json!(accept_languages(&headers)));
        }
    }
    for k in ["limit", "offset"] {
        if let Some(v) = params.get(k) {
            a.insert(k.into(), json!(v));
        }
    }
    for k in ["fuzzy", "definitions"] {
        if let Some(v) = params.get(k) {
            a.insert(k.into(), json!(truthy(v)));
        }
    }
    let s = onto::wire::search_request_of(&a).map_err(|e| bad(e.to_string()))?;
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        let found = ds
            .dataset
            .ontology()
            .search(&req, &s, &opts)
            .map_err(ontology_error)?;
        let j = onto::wire::search_body(&ds.name, &s, &found);
        Ok(with_commit(Json(j).into_response(), &ds, found.commit))
    })
    .await
}

/// The members of a browser request that the query parameters give, with the caller's
/// languages from `lang` or `Accept-Language`.
fn browse_args(
    params: &Params,
    headers: &HeaderMap,
    keys: &[&str],
    flags: &[&str],
) -> serde_json::Map<String, serde_json::Value> {
    let mut a = serde_json::Map::new();
    for k in keys {
        if let Some(v) = params.get(k) {
            a.insert((*k).into(), json!(v));
        }
    }
    for k in flags {
        if let Some(v) = params.get(k) {
            a.insert((*k).into(), json!(truthy(v)));
        }
    }
    match params.get("lang") {
        Some(l) => a.insert("lang".into(), json!(l)),
        None => a.insert("lang".into(), json!(accept_languages(headers))),
    };
    a
}

/// Parse the numbers of a browser request, which the library reads as JSON numbers.
fn numbers(a: &mut serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> ApiResult<()> {
    for k in keys {
        if let Some(serde_json::Value::String(v)) = a.get(*k) {
            let n: u64 = v
                .parse()
                .map_err(|_| bad(format!("{k} must be a non-negative integer, not '{v}'")))?;
            a.insert((*k).into(), json!(n));
        }
    }
    Ok(())
}

/// A JSON answer with the commit it read as its entity tag, or `304` when the caller
/// already has it.
fn tagged(
    ds: &Dataset,
    headers: &HeaderMap,
    seq: u64,
    ext: &str,
    j: serde_json::Value,
) -> ApiResult {
    let tag = conditional::etag(ds.store.dataset_id(), seq, ext);
    let resp = match conditional::check_read(headers, ds.store.dataset_id(), seq, &tag)? {
        conditional::ReadOutcome::NotModified => conditional::not_modified(&tag),
        conditional::ReadOutcome::Send => {
            conditional::with_etag(Json(j).into_response(), &tag, false)
        }
    };
    Ok(with_commit(resp, ds, seq))
}

/// `GET /$/ontology/{ds}/hierarchy`: one level of the class, property or concept
/// hierarchy (item 607): the children of `iri`, or the roots, a page at a time
/// (`limit`, `offset`), with their own numbers of children and parents, and the
/// ancestors of `iri` with `ancestors=true`. `kind`, `scheme`, `deprecated` and `lang`
/// or `Accept-Language` refine it.
pub(super) async fn hierarchy(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let req = request_of(&params)?;
    let mut a = browse_args(
        &params,
        &headers,
        &["kind", "iri", "scheme", "limit", "offset"],
        &["ancestors", "deprecated"],
    );
    numbers(&mut a, &["limit", "offset"])?;
    let h = onto::wire::hierarchy_request_of(&a).map_err(|e| bad(e.to_string()))?;
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        let found = ds
            .dataset
            .ontology()
            .hierarchy(&req, &h, &opts)
            .map_err(ontology_error)?;
        let j = onto::wire::hierarchy_body(&ds.name, &h, &found);
        tagged(&ds, &headers, found.commit, "owl-tree", j)
    })
    .await
}

/// Named individuals by asserted named type, with bounded keyset pages.
pub(super) async fn individuals(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let req = request_of(&params)?;
    let mut a = browse_args(
        &params,
        &headers,
        &["type", "iri", "after", "limit"],
        &["untyped"],
    );
    numbers(&mut a, &["limit"])?;
    let q = onto::wire::individuals_request_of(&a).map_err(|e| bad(e.to_string()))?;
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        let found = ds
            .dataset
            .ontology()
            .individuals(&req, &q, &opts)
            .map_err(ontology_error)?;
        let j = onto::wire::individuals_body(&ds.name, &found);
        tagged(&ds, &headers, found.commit, "owl-individuals", j)
    })
    .await
}

/// Read existing kept state only. Availability changes without a data commit,
/// so every handler response is no-store and bypasses commit-only conditional GET.
pub(super) async fn kept_classification(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let answer: ApiResult = async {
        let ds = dataset(&st, &name)?;
        let params = Params::from_query(&uri);
        let allowed = [
            "graph", "imports", "mode", "at", "part", "about", "offset", "limit", "resultId",
            "lang", "timeout", "branch",
        ];
        for (key, _) in &params.0 {
            if !allowed.contains(&key.as_str()) {
                return Err(bad(format!("unknown selector '{key}'")));
            }
            if key != "graph" && params.all(key).len() != 1 {
                return Err(bad(format!("duplicate selector '{key}'")));
            }
        }
        let req = request_of(&params)?;
        let mut a = browse_args(
            &params,
            &headers,
            &["part", "about", "offset", "limit", "resultId"],
            &[],
        );
        numbers(&mut a, &["offset", "limit"])?;
        let query = onto::wire::kept_query_of(&a).map_err(ontology_error)?;
        query.validate(&req).map_err(ontology_error)?;
        let mut opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
        let (cancel, _guard) = cancel_on_drop();
        opts.cancel = Some(cancel);
        blocking(move || {
            let found = ds
                .dataset
                .ontology()
                .kept_classification(&req, &query, &opts)
                .map_err(ontology_error)?;
            let body = onto::wire::kept_body(&ds.name, &found).map_err(ontology_error)?;
            Ok(with_commit(Json(body).into_response(), &ds, found.commit))
        })
        .await
    }
    .await;
    let mut response = answer.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(response)
}

/// The download formats of `GET /$/ontology/{ds}/term`.
fn term_format(f: &str) -> ApiResult<Option<(oxrdfio::RdfFormat, &'static str, &'static str)>> {
    use oxrdfio::RdfFormat;
    Ok(Some(match f {
        "json" => return Ok(None),
        "ttl" | "turtle" => (RdfFormat::Turtle, "text/turtle; charset=utf-8", "ttl"),
        "nt" | "ntriples" => (
            RdfFormat::NTriples,
            "application/n-triples; charset=utf-8",
            "nt",
        ),
        "rdfxml" | "owl" => (
            RdfFormat::RdfXml,
            "application/rdf+xml; charset=utf-8",
            "rdf",
        ),
        "jsonld" => (
            RdfFormat::JsonLd {
                profile: oxrdfio::JsonLdProfileSet::empty(),
            },
            "application/ld+json; charset=utf-8",
            "jsonld",
        ),
        "ofn" => (
            RdfFormat::NTriples,
            "text/owl-functional; charset=utf-8",
            "ofn",
        ),
        f => {
            return Err(bad(format!(
                "format must be json, ttl, nt, rdfxml, jsonld or ofn, not '{f}'"
            )));
        }
    }))
}

/// A file name for an entity's download: its short form or local name, made safe.
fn download_name(t: &onto::browse::TermPage, ext: &str) -> String {
    let base = t.name.short_form.clone().unwrap_or_else(|| {
        t.name
            .iri
            .rsplit(['/', '#'])
            .next()
            .unwrap_or("term")
            .to_string()
    });
    let safe: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "{}.{ext}",
        if safe.is_empty() { "term".into() } else { safe }
    )
}

/// `GET /$/ontology/{ds}/term?iri=IRI`: what the graphs read say about one entity
/// without reading the whole ontology: its statements with their annotations, the
/// axioms they map to, the axioms of other entities that mention it, the statements
/// that point at it, and the labels of every IRI used. `format` downloads the triples
/// about it in Turtle, N-Triples, RDF/XML or JSON-LD, or its axioms in functional-style
/// syntax.
pub(super) async fn term(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let req = request_of(&params)?;
    let mut a = browse_args(&params, &headers, &["iri", "mentions", "incoming"], &[]);
    numbers(&mut a, &["mentions", "incoming"])?;
    let t = onto::wire::term_request_of(&a).map_err(|e| bad(e.to_string()))?;
    let format = term_format(params.get("format").unwrap_or("json"))?;
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        let page = ds
            .dataset
            .ontology()
            .term(&req, &t, &opts)
            .map_err(ontology_error)?;
        let r = renderer(&ds);
        let Some((fmt, mime, ext)) = format else {
            let j = onto::wire::term_body(&ds.name, &page, &r);
            return tagged(&ds, &headers, page.commit, "owl-term", j);
        };
        let body: Vec<u8> = if ext == "ofn" {
            let header = owl::Header::default();
            owl::ofn::document(&r, &header, page.axioms.iter().map(|(a, _)| a)).into_bytes()
        } else {
            let mut ser = sparkles::io::with_prefixes(
                oxrdfio::RdfSerializer::from_format(fmt),
                ds.store.prefixes(),
            )
            .for_writer(Vec::new());
            for q in &page.quads {
                ser.serialize_triple(oxrdf::TripleRef::new(
                    q.subject.as_ref(),
                    q.predicate.as_ref(),
                    q.object.as_ref(),
                ))
                .map_err(|e| ApiError::from(sparkles::Error::from(e)))?;
            }
            ser.finish()
                .map_err(|e| ApiError::from(sparkles::Error::from(e)))?
        };
        let resp = (
            [
                (header::CONTENT_TYPE, mime.to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", download_name(&page, ext)),
                ),
            ],
            body,
        )
            .into_response();
        Ok(with_commit(resp, &ds, page.commit))
    })
    .await
}

#[cfg(test)]
mod tests;

/// `POST /$/ontology/{ds}/classify`: the OWL 2 EL classification of the ontology (spec
/// F24), with the members of the ontology read and `assertions`, `about`, `nodes`,
/// `limit`, `offset`, `threads`, `partial` and `expansion`. `target` writes the taxonomy
/// to that named graph in one commit, as an edit does, with `message`, a dry run and
/// `If-Match`. `format=ofn` answers the taxonomy as a functional-style document.
pub(super) async fn classify(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let o = classify_object(
        &bytes,
        params.has("projection"),
        &[
            "graph",
            "imports",
            "mode",
            "inferences",
            "assertions",
            "target",
            "message",
            "about",
            "nodes",
            "limit",
            "offset",
            "threads",
            "partial",
            "expansion",
            "projection",
            "lang",
        ],
    )?;
    let req = request_body(&o, request_of(&params)?)?;
    let flag = |k: &str| -> ApiResult<bool> {
        match o.get(k) {
            None | Some(J::Null) => Ok(params.get(k).is_some_and(truthy)),
            Some(J::Bool(b)) => Ok(*b),
            Some(_) => Err(bad(format!("{k}: true or false"))),
        }
    };
    let number = |k: &str, default: usize, min: usize, max: usize| -> ApiResult<usize> {
        let v = match o.get(k) {
            None | Some(J::Null) => match params.get(k) {
                None => return Ok(default),
                Some(v) => v.parse::<u64>().ok(),
            },
            Some(v) => v.as_u64(),
        };
        v.map(|n| n as usize)
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| bad(format!("{k}: an integer from {min} to {max}")))
    };
    let text = |k: &str| -> ApiResult<Option<String>> {
        match o.get(k) {
            None | Some(J::Null) => Ok(params.get(k).map(str::to_string)),
            Some(J::String(v)) => Ok(Some(v.clone())),
            Some(_) => Err(bad(format!("{k}: a string"))),
        }
    };
    let about = text("about")?.map(|a| a.trim_start_matches('<').trim_end_matches('>').to_string());
    let c = ClassifyRequest {
        assertions: flag("assertions")?,
        threads: number("threads", 0, 0, 1024)?,
        target: text("target")?.map(|g| graph_ref(&g)).transpose()?,
        message: text("message")?,
        about: about.clone(),
        el_only: false,
        partial: flag("partial")?,
        expansion: expansion_of(text("expansion")?.as_deref()).map_err(ontology_error)?,
    };
    let projection =
        onto::wire::projection_of(text("projection")?.as_deref()).map_err(ontology_error)?;
    if projection.is_some() {
        for key in [
            "projection",
            "about",
            "nodes",
            "limit",
            "offset",
            "threads",
            "partial",
            "expansion",
            "target",
            "message",
            "lang",
            "format",
        ] {
            if params.all(key).len() > 1 {
                return Err(bad(format!("duplicate selector '{key}'")));
            }
            if params.has(key) && o.get(key).is_some_and(|v| !v.is_null()) {
                return Err(bad(format!("'{key}' appears in both query and body")));
            }
        }
    }
    let mut display = browse_args(&params, &headers, &[], &[]);
    if let Some(lang) = o.get("lang").filter(|v| !v.is_null()) {
        display.insert("lang".into(), lang.clone());
    }
    let q = ClassifyQuery {
        projection,
        languages: onto::wire::languages_of(&display).map_err(ontology_error)?,
        about,
        nodes: flag("nodes")?,
        limit: number("limit", DEFAULT_LIMIT, 1, MAX_LIMIT)?,
        offset: number("offset", 0, 0, usize::MAX)?,
    };
    let ofn = match params.get("format") {
        None | Some("json") => false,
        Some("ofn") => true,
        Some(f) => return Err(bad(format!("format must be json or ofn, not '{f}'"))),
    };
    q.validate_projection(
        c.target.is_some(),
        ofn,
        params.has("lang") || o.get("lang").is_some_and(|v| !v.is_null()),
    )
    .map_err(ontology_error)?;
    if c.partial && ofn {
        return Err(bad(
            "format=ofn has no place to say that an answer is partial: leave out 'partial' or 'format'",
        ));
    }
    let writes = c.target.is_some();
    if writes && ofn {
        return Err(bad(
            "format=ofn answers a classification without a write: leave out 'target' or 'format'",
        ));
    }
    // the route table admits readers, so a run that writes its taxonomy checks write here
    if writes
        && let Some(denied) = crate::auth::dataset_denial(
            &st,
            &p,
            &headers,
            &name,
            crate::auth::Level::Write,
            Endpoint::Update,
        )
    {
        return Ok(denied);
    }
    if writes && st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let endpoint = if writes {
        Endpoint::Update
    } else {
        Endpoint::Query
    };
    let mut opts = CallerContext::for_caller(&st, &ds, &p).options(&params, endpoint);
    let restricted = opts.graphs.is_some();
    let wanted = receipt_wanted(&params, &headers);
    let mut dry = None;
    let mut timeout = None;
    // a client that goes away stops the run
    let (cancel, cancel_guard) = cancel_on_drop();
    let mut _cancel_on_drop = Some(cancel_guard);
    opts.cancel = Some(cancel);
    if writes {
        let (mut wopts, cancel) = body_write_options(&st, &params, &headers, Some(&p))?;
        _cancel_on_drop = Some(cancel);
        wopts.opts.graphs = opts.graphs.clone();
        if wopts.opts.precondition.is_none() {
            wopts.opts.precondition =
                conditional::write_precondition(&headers, ds.store.dataset_id(), &Target::Dataset);
        }
        let d = wopts
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
        let (wopts, t) = wopts.start();
        dry = d.map(|request| request.started(&wopts));
        timeout = t;
        opts.cancel = wopts.cancel.clone();
        opts.deadline = wopts.deadline;
        opts.timeout = None;
        opts.write = wopts;
    }
    blocking(move || {
        let r = ds.dataset.ontology().classify(&req, &c, &opts);
        let out = match (r, dry) {
            (Err(Error::DryRun(preview)), Some(d)) => return Ok(d.respond(&ds, &preview)),
            (Err(e @ Error::Invalid(_)), _) => return Err(ontology_error(e)),
            (r, _) if writes => r.map_err(|e| write_error(e, restricted))?,
            (r, _) => r.map_err(ontology_error)?,
        };
        let rend = renderer(&ds);
        let seq = out.opened.snapshot.commit;
        if ofn {
            let axioms = out.classification.taxonomy.axioms();
            let doc = owl::ofn::document(&rend, &owl::Header::default(), axioms.iter());
            let resp = (
                [(header::CONTENT_TYPE, "text/owl-functional; charset=utf-8")],
                doc,
            )
                .into_response();
            return Ok(with_commit(resp, &ds, seq));
        }
        let body = classify_body(&ds.name, &out, &req, &rend, &q).map_err(|e| match e {
            Error::NotFound(m) => err_code(StatusCode::NOT_FOUND, "not-found", m),
            e => ontology_error(e),
        })?;
        match &out.written {
            Some(w) => Ok(write_response(
                &ds,
                StatusCode::OK,
                Some(body),
                &w.receipt,
                wanted,
                restricted,
            )),
            None => Ok(with_commit(Json(body).into_response(), &ds, seq)),
        }
    })
    .await
    .map_err(|e| with_timeout(e, timeout))
}
