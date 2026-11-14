//! The routes of the resource catalog (spec C30, phase 1).
//!
//! Authorization (the route table in `auth/routes.rs`): every `/$/resources` route needs
//! server `admin`, except `POST /$/resources/{id}/install/{ds}`, which needs `admin` on
//! `{ds}`. Writes answer `403 server-read-only` on a `--read-only` server. Errors are
//! `{error, code}`, and a catalog file this build does not read is `503
//! catalog-unreadable` on every route.

use super::{
    CatalogFile, Error, Kind, Resource, ResourceInput, Resources, VersionInput, find, report,
};
use crate::auth::Principal;
use crate::http::caller::CallerContext;
use crate::http::{ApiError, ApiResult, blocking, err_code};
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use oxrdf::Term;
use oxrdfio::RdfFormat;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/resources", get(list).post(create))
        .route("/$/resources/{id}", get(read).put(replace).delete(remove))
        .route("/$/resources/{id}/versions", post(add_version))
        .route(
            "/$/resources/{id}/versions/{version}",
            axum::routing::delete(remove_version),
        )
        .route(
            "/$/resources/{id}/acceptance",
            post(accept).delete(withdraw),
        )
        .route("/$/resources/{id}/license", get(license))
        .route("/$/resources/{id}/notice", get(notice))
        .route("/$/resources/{id}/header", get(header_read))
        .route("/$/resources/{id}/install/{ds}", post(install))
}

impl From<Error> for ApiError {
    fn from(e: Error) -> ApiError {
        match e {
            Error::Unreadable(m) => err_code(
                StatusCode::SERVICE_UNAVAILABLE,
                "catalog-unreadable",
                format!("the resource catalog cannot be read, and it is left as it is: {m}"),
            ),
            Error::NotFound(code, m) => err_code(StatusCode::NOT_FOUND, code, m),
            Error::Invalid(m) => err_code(StatusCode::BAD_REQUEST, "bad-resource", m),
            Error::Conflict(code, m) => err_code(StatusCode::CONFLICT, code, m),
            Error::Precondition(m) => {
                err_code(StatusCode::PRECONDITION_FAILED, "precondition-failed", m)
            }
            Error::Limit(m) => err_code(StatusCode::BAD_REQUEST, "catalog-limit", m),
            Error::Io(m) => err_code(StatusCode::INTERNAL_SERVER_ERROR, "catalog-write-failed", m),
        }
    }
}

fn bad(m: impl Into<String>) -> ApiError {
    err_code(StatusCode::BAD_REQUEST, "bad-resource", m)
}

fn writable(st: &AppState) -> ApiResult<()> {
    if st.read_only {
        return Err(err_code(
            StatusCode::FORBIDDEN,
            "server-read-only",
            "server is read-only",
        ));
    }
    Ok(())
}

fn store(st: &AppState) -> &Resources {
    &st.resources
}

/// The members a resource's writer gives, and those the catalog sets.
const RESOURCE_MEMBERS: &[&str] = &[
    "kind",
    "title",
    "description",
    "homepage",
    "ownership",
    "license",
    "licenseClass",
    "licenseUrl",
    "attribution",
    "namespaces",
    "graphs",
    "dataset",
];
const VERSION_MEMBERS: &[&str] = &[
    "version",
    "issued",
    "notes",
    "license",
    "licenseClass",
    "licenseUrl",
    "distributions",
    "imports",
    "graphs",
];
const SET_BY_CATALOG: &[&str] = &["acceptance", "created", "modified", "added"];

fn object(body: &[u8]) -> ApiResult<serde_json::Map<String, Value>> {
    match serde_json::from_slice(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(bad("the body is not a JSON object")),
        Err(e) => Err(bad(format!("the body is not JSON: {e}"))),
    }
}

/// Refuse members outside `allowed`, naming those that the catalog sets itself.
fn members(m: &serde_json::Map<String, Value>, allowed: &[&str], what: &str) -> ApiResult<()> {
    for k in m.keys() {
        if allowed.contains(&k.as_str()) {
            continue;
        }
        if SET_BY_CATALOG.contains(&k.as_str()) {
            return Err(bad(format!(
                "{k} is set by the catalog, not by the writer of a {what}"
            )));
        }
        return Err(bad(format!("{k} is not a member of a {what}")));
    }
    Ok(())
}

fn typed<T: serde::de::DeserializeOwned>(m: serde_json::Map<String, Value>) -> ApiResult<T> {
    serde_json::from_value(Value::Object(m)).map_err(|e| bad(e.to_string()))
}

fn version_input(v: Value) -> ApiResult<VersionInput> {
    let Value::Object(m) = v else {
        return Err(bad("a version is a JSON object"));
    };
    members(&m, VERSION_MEMBERS, "version")?;
    typed(m)
}

fn query(uri: &Uri) -> BTreeMap<String, String> {
    uri.query()
        .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default()
}

fn if_match(h: &HeaderMap) -> Option<String> {
    h.get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn with_etag(r: &Resource, status: StatusCode) -> Response {
    let mut resp = (status, Json(r)).into_response();
    if let Ok(v) = HeaderValue::from_str(&r.etag()) {
        resp.headers_mut().insert(header::ETAG, v);
    }
    if status == StatusCode::CREATED
        && let Ok(v) = HeaderValue::from_str(&format!("/$/resources/{}", r.id))
    {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

/// The RDF syntax a read asks for with `format=` or `Accept`; `None` is JSON.
fn rdf_format(q: &BTreeMap<String, String>, h: &HeaderMap) -> ApiResult<Option<RdfFormat>> {
    use sparkles::sparql::results;
    if let Some(f) = q.get("format") {
        if f == "json" {
            return Ok(None);
        }
        return results::rdf_format_from_name(f).map(Some).ok_or_else(|| {
            bad(format!(
                "unknown format '{f}': json, turtle, ntriples, nquads, trig, rdfxml or jsonld"
            ))
        });
    }
    const OFFERS: [&str; 7] = [
        "application/json",
        "text/turtle",
        "application/n-triples",
        "application/ld+json",
        "application/rdf+xml",
        "application/trig",
        "application/n-quads",
    ];
    let accept = h
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*");
    Ok(match crate::http::negotiate(accept, &OFFERS) {
        None | Some(0) => None,
        Some(i) => results::rdf_format_from_name(OFFERS[i]),
    })
}

fn rdf(c: &CatalogFile, only: Option<&str>, f: RdfFormat) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            sparkles::sparql::results::rdf_media_type(f),
        )],
        super::rdf::describe(c, only, f),
    )
        .into_response()
}

fn audit(event: &str, id: &str, principal: &str) {
    tracing::info!(
        target: "sparkles::audit",
        event,
        resource = id,
        principal
    );
}

/// `GET /$/resources`: every resource, as JSON or in DCAT.
async fn list(State(st): St, uri: Uri, h: HeaderMap) -> ApiResult {
    let f = rdf_format(&query(&uri), &h)?;
    blocking(move || {
        let c = store(&st).read()?;
        Ok(match f {
            Some(f) => rdf(&c, None, f),
            None => Json(json!({ "resources": c.resources })).into_response(),
        })
    })
    .await
}

/// `POST /$/resources`: add a resource, with any initial versions.
async fn create(State(st): St, p: Extension<Principal>, body: Bytes) -> ApiResult {
    writable(&st)?;
    let mut m = object(&body)?;
    let id = match m.remove("id") {
        Some(Value::String(s)) => s,
        Some(_) => return Err(bad("id is a string")),
        None => return Err(bad("a resource needs an id")),
    };
    let versions = match m.remove("versions") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.into_iter().map(version_input).collect::<ApiResult<_>>()?,
        Some(_) => return Err(bad("versions is an array of versions")),
    };
    members(&m, RESOURCE_MEMBERS, "resource")?;
    let input: ResourceInput = typed(m)?;
    let by = p.id();
    blocking(move || {
        let r = store(&st).create(&id, input, versions, &by)?;
        audit("resource_create", &r.id, &by);
        Ok(with_etag(&r, StatusCode::CREATED))
    })
    .await
}

/// `GET /$/resources/{id}`: one resource, as JSON with its `ETag`, or in DCAT.
async fn read(State(st): St, Path(id): Path<String>, uri: Uri, h: HeaderMap) -> ApiResult {
    let f = rdf_format(&query(&uri), &h)?;
    blocking(move || {
        let c = store(&st).read()?;
        let r = find(&c, &id).ok_or_else(|| super::no_such(&id))?;
        Ok(match f {
            Some(f) => rdf(&c, Some(&id), f),
            None => with_etag(r, StatusCode::OK),
        })
    })
    .await
}

/// `PUT /$/resources/{id}`: replace the descriptive members.
async fn replace(
    State(st): St,
    Path(id): Path<String>,
    p: Extension<Principal>,
    h: HeaderMap,
    body: Bytes,
) -> ApiResult {
    writable(&st)?;
    let mut m = object(&body)?;
    if m.contains_key("versions") {
        return Err(bad(
            "versions change with POST /$/resources/{id}/versions and DELETE /$/resources/{id}/versions/{version}",
        ));
    }
    if let Some(v) = m.remove("id")
        && v != Value::String(id.clone())
    {
        return Err(bad(format!("the body names another id than {id}")));
    }
    members(&m, RESOURCE_MEMBERS, "resource")?;
    let input: ResourceInput = typed(m)?;
    let by = p.id();
    let im = if_match(&h);
    blocking(move || {
        let r = store(&st).replace(&id, input, im.as_deref(), &by)?;
        audit("resource_replace", &id, &by);
        Ok(with_etag(&r, StatusCode::OK))
    })
    .await
}

/// `DELETE /$/resources/{id}`
async fn remove(
    State(st): St,
    Path(id): Path<String>,
    p: Extension<Principal>,
    h: HeaderMap,
) -> ApiResult {
    writable(&st)?;
    let by = p.id();
    let im = if_match(&h);
    blocking(move || {
        store(&st).delete(&id, im.as_deref())?;
        audit("resource_delete", &id, &by);
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// `POST /$/resources/{id}/versions`: add a version, which never changes afterwards.
async fn add_version(
    State(st): St,
    Path(id): Path<String>,
    p: Extension<Principal>,
    body: Bytes,
) -> ApiResult {
    writable(&st)?;
    let v = version_input(Value::Object(object(&body)?))?;
    let by = p.id();
    blocking(move || {
        let r = store(&st).add_version(&id, v, &by)?;
        audit("resource_version_add", &id, &by);
        Ok(with_etag(&r, StatusCode::CREATED))
    })
    .await
}

/// `DELETE /$/resources/{id}/versions/{version}`
async fn remove_version(
    State(st): St,
    Path((id, version)): Path<(String, String)>,
    p: Extension<Principal>,
) -> ApiResult {
    writable(&st)?;
    let by = p.id();
    blocking(move || {
        store(&st).delete_version(&id, &version, &by)?;
        audit("resource_version_delete", &id, &by);
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// `POST /$/resources/{id}/acceptance`: record the caller's acceptance of restricted
/// terms.
async fn accept(State(st): St, Path(id): Path<String>, p: Extension<Principal>) -> ApiResult {
    writable(&st)?;
    let by = p.id();
    blocking(move || {
        let r = store(&st).accept(&id, &by)?;
        audit("resource_accept", &id, &by);
        Ok(with_etag(&r, StatusCode::OK))
    })
    .await
}

/// `DELETE /$/resources/{id}/acceptance`
async fn withdraw(State(st): St, Path(id): Path<String>, p: Extension<Principal>) -> ApiResult {
    writable(&st)?;
    let by = p.id();
    blocking(move || {
        let r = store(&st).withdraw(&id, &by)?;
        audit("resource_withdraw", &id, &by);
        Ok(with_etag(&r, StatusCode::OK))
    })
    .await
}

fn license_report(
    st: &AppState,
    id: &str,
    uri: &Uri,
) -> ApiResult<(CatalogFile, report::LicenseReport)> {
    let q = query(uri);
    let c = store(st).read()?;
    let rep = report::license_report(
        &c,
        id,
        q.get("version").map(String::as_str),
        st.license_policy,
    )?;
    Ok((c, rep))
}

/// `GET /$/resources/{id}/license`: the obligations of a version and the gate's verdict.
async fn license(State(st): St, Path(id): Path<String>, uri: Uri) -> ApiResult {
    blocking(move || {
        let (_, rep) = license_report(&st, &id, &uri)?;
        Ok(Json(rep).into_response())
    })
    .await
}

/// `GET /$/resources/{id}/notice`: the NOTICE of a version, as plain text.
async fn notice(State(st): St, Path(id): Path<String>, uri: Uri) -> ApiResult {
    blocking(move || {
        let (c, rep) = license_report(&st, &id, &uri)?;
        Ok((
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            report::notice(&c, &rep),
        )
            .into_response())
    })
    .await
}

// ------------------------------------------------------------------- header ------

const HEADER_QUERY: &str = r#"
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
PREFIX dct: <http://purl.org/dc/terms/>
PREFIX vann: <http://purl.org/vocab/vann/>
SELECT ?h (SAMPLE(?vi) AS ?versionIRI) (SAMPLE(?vinfo) AS ?versionInfo)
       (SAMPLE(?lic) AS ?license) (SAMPLE(?ti) AS ?title)
       (SAMPLE(?pre) AS ?prefix) (SAMPLE(?ns) AS ?namespace)
WHERE {
  GRAPH <GRAPH> {
    VALUES ?type { owl:Ontology skos:ConceptScheme }
    ?h a ?type .
    OPTIONAL { ?h owl:versionIRI ?vi }
    OPTIONAL { ?h owl:versionInfo ?vinfo }
    OPTIONAL { ?h dct:license ?lic }
    OPTIONAL { ?h dct:title ?ti }
    OPTIONAL { ?h vann:preferredNamespacePrefix ?pre }
    OPTIONAL { ?h vann:preferredNamespaceUri ?ns }
  }
}
GROUP BY ?h ORDER BY ?h LIMIT 1
"#;

fn term_str(t: &Option<Term>) -> Option<String> {
    match t {
        Some(Term::NamedNode(n)) => Some(n.as_str().to_string()),
        Some(Term::Literal(l)) => Some(l.value().to_string()),
        Some(Term::BlankNode(b)) => Some(format!("_:{}", b.as_str())),
        _ => None,
    }
}

/// A licence page or IRI for comparison.
#[derive(PartialEq, Eq)]
enum LicenceKey {
    /// an SPDX licence page or IRI, by identifier: its scheme, the host's case, a
    /// trailing `.html` and the identifier's case do not matter
    Spdx(String),
    /// any other IRI, compared exactly
    Iri(String),
}

fn licence_key(s: &str) -> LicenceKey {
    if let Ok(u) = url::Url::parse(s)
        && matches!(u.scheme(), "http" | "https")
        && u.host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case("spdx.org"))
        // `port` is `None` for the scheme's default port
        && u.port().is_none()
        && u.username().is_empty()
        && u.password().is_none()
        && u.query().is_none()
        && u.fragment().is_none()
        && let Some(id) = u.path().strip_prefix("/licenses/")
    {
        let id = id.trim_end_matches('/');
        let id = id.strip_suffix(".html").unwrap_or(id);
        if !id.is_empty() && !id.contains('/') {
            return LicenceKey::Spdx(id.to_ascii_lowercase());
        }
    }
    LicenceKey::Iri(s.to_string())
}

/// Whether a header's version IRI names `version`: one of its path segments, or its
/// fragment, is exactly the version. The authority and the query never match.
fn names_version(iri: &str, version: &str) -> bool {
    let Ok(u) = url::Url::parse(iri) else {
        return false;
    };
    let path = u.path();
    let segments: Vec<&str> = if u.cannot_be_a_base() {
        // such as `urn:example:vocab:1.0`
        path.split([':', '/']).collect()
    } else {
        path.split('/').collect()
    };
    segments.contains(&version) || u.fragment() == Some(version)
}

/// Where a graph's header disagrees with the record of the current version.
pub(super) fn disagreements(r: &Resource, h: &BTreeMap<&str, Option<String>>) -> Vec<String> {
    let mut out = Vec::new();
    let cur = r.current();
    if let Some(cur) = cur {
        let v = &cur.input.version;
        let info = h["versionInfo"].as_deref();
        let viri = h["versionIRI"].as_deref();
        // owl:versionInfo names the version exactly; without it, the version IRI must
        // hold the version as a whole segment
        let differs = match (info, viri) {
            (Some(i), _) => (i != v.as_str()).then(|| i.to_string()),
            (None, Some(i)) => (!names_version(i, v)).then(|| i.to_string()),
            (None, None) => None,
        };
        if let Some(named) = differs {
            out.push(format!(
                "the header names version {named}, the catalog's current version is {v}"
            ));
        }
    }
    if let Some(lic) = h["license"].as_deref() {
        let l = r.licence_of(cur);
        let known: Vec<LicenceKey> = [l.spdx_iri(), l.page(), l.url.map(str::to_string)]
            .into_iter()
            .flatten()
            .map(|s| licence_key(&s))
            .collect();
        if !known.is_empty() && !known.contains(&licence_key(lic)) {
            out.push(format!(
                "the header's licence is {lic}, the catalog records {}",
                l.expression.unwrap_or("NOASSERTION")
            ));
        } else if known.is_empty() && l.expression.is_none() {
            out.push(format!(
                "the header's licence is {lic}, the catalog records none"
            ));
        }
    }
    if let Some(p) = h["prefix"].as_deref()
        && !r.input.namespaces.iter().any(|n| n.prefix == p)
    {
        out.push(format!(
            "the header prefers the prefix {p}, which the catalog does not list"
        ));
    }
    if let Some(ns) = h["namespace"].as_deref()
        && !r.input.namespaces.iter().any(|n| n.iri == ns)
    {
        out.push(format!(
            "the header prefers the namespace {ns}, which the catalog does not list"
        ));
    }
    out
}

/// `GET /$/resources/{id}/header`: the header of each graph that holds a vocabulary,
/// read under the caller's grants, and where it disagrees with the catalog.
async fn header_read(
    State(st): St,
    Path(id): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult {
    blocking(move || {
        let c = store(&st).read()?;
        let r = find(&c, &id).ok_or_else(|| super::no_such(&id))?;
        if r.input.kind != Kind::Vocabulary {
            return Err(err_code(
                StatusCode::CONFLICT,
                "not-a-vocabulary",
                format!(
                    "{id} is a {}; only a vocabulary has a header",
                    r.input.kind.as_str()
                ),
            ));
        }
        let mut graphs = Vec::new();
        for l in &r.input.graphs {
            let ds = match crate::http::dataset(&st, &l.dataset) {
                Ok(ds) => ds,
                Err(_) => {
                    graphs.push(json!({
                        "dataset": l.dataset,
                        "graph": l.graph,
                        "error": format!("the server has no open dataset {}", l.dataset),
                    }));
                    continue;
                }
            };
            let opts = CallerContext::for_caller(&st, &ds, &p).read_options();
            let q = HEADER_QUERY.replace("GRAPH <GRAPH>", &format!("GRAPH <{}>", l.graph));
            let rows = sparkles::sparql::query(ds.store.snapshot(), &q, &opts)
                .and_then(|r| r.rows())
                .map_err(ApiError::from)?;
            let Some(row) = rows.iter().next() else {
                graphs.push(json!({
                    "dataset": l.dataset,
                    "graph": l.graph,
                    "found": false,
                }));
                continue;
            };
            let names = [
                "header",
                "versionIRI",
                "versionInfo",
                "license",
                "title",
                "prefix",
                "namespace",
            ];
            let h: BTreeMap<&str, Option<String>> = names
                .iter()
                .zip(row.values().iter())
                .map(|(n, t)| (*n, term_str(t)))
                .collect();
            let differs = disagreements(r, &h);
            let mut entry = json!({
                "dataset": l.dataset,
                "graph": l.graph,
                "found": true,
                "disagreements": differs,
            });
            for (k, v) in &h {
                if let Some(v) = v {
                    entry[*k] = json!(v);
                }
            }
            graphs.push(entry);
        }
        Ok(Json(json!({
            "resource": id,
            "version": r.current().map(|v| v.input.version.clone()),
            "graphs": graphs,
        }))
        .into_response())
    })
    .await
}

// ------------------------------------------------------------------ install ------

/// The namespaces of a vocabulary and of the vocabularies its current version imports,
/// transitively. Each pinned version is expanded once, so shared imports cost one visit.
pub(super) fn bundle(c: &CatalogFile, r: &Resource) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut resources = BTreeSet::new();
    let mut pins = BTreeSet::new();
    let mut add = |t: &Resource, out: &mut Vec<(String, String, String)>| {
        if t.input.kind == Kind::Vocabulary && resources.insert(t.id.clone()) {
            for n in &t.input.namespaces {
                out.push((n.prefix.clone(), n.iri.clone(), t.id.clone()));
            }
        }
    };
    add(r, &mut out);
    let mut todo: Vec<super::Pin> = r
        .current()
        .map(|v| v.input.imports.clone())
        .unwrap_or_default();
    while let Some(p) = todo.pop() {
        if !pins.insert(p.clone()) {
            continue;
        }
        let Some(t) = find(c, &p.resource) else {
            continue;
        };
        add(t, &mut out);
        if let Some(v) = t.version(&p.version) {
            todo.extend(
                v.input
                    .imports
                    .iter()
                    .filter(|q| !pins.contains(*q))
                    .cloned(),
            );
        }
    }
    out
}

/// What an install binds, decided from one state of the dataset's prefixes.
struct Plan {
    planned: Vec<(String, String)>,
    conflicts: Vec<Value>,
    unchanged: Vec<Value>,
    /// the entity tag of the prefixes the plan was made from
    etag: String,
}

fn plan_install(st: &AppState, target: &crate::state::Dataset, id: &str) -> ApiResult<Plan> {
    let c = store(st).read()?;
    let r = find(&c, id).ok_or_else(|| super::no_such(id))?;
    if r.input.kind != Kind::Vocabulary {
        return Err(err_code(
            StatusCode::CONFLICT,
            "not-a-vocabulary",
            format!(
                "{id} is a {}; only a vocabulary has namespaces to install",
                r.input.kind.as_str()
            ),
        ));
    }
    // the bindings and their entity tag come from one read, so the write's precondition
    // names exactly the state the plan saw
    let resolved = crate::settings::resolved(st, target, &crate::settings::prefixes::PREFIXES);
    let etag = resolved.etag;
    let have: BTreeMap<String, String> = resolved
        .effective
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let mut plan = Plan {
        planned: Vec::new(),
        conflicts: Vec::new(),
        unchanged: Vec::new(),
        etag,
    };
    for (prefix, iri, from) in bundle(&c, r) {
        match have.get(&prefix) {
            Some(old) if *old == iri => plan.unchanged.push(json!({"prefix": prefix, "iri": iri})),
            Some(old) => {
                plan.conflicts
                    .push(json!({"prefix": prefix, "iri": iri, "bound": old, "resource": from}));
                plan.planned.push((prefix, iri));
            }
            None => plan.planned.push((prefix, iri)),
        }
    }
    Ok(plan)
}

/// Test hooks that run once between an install's plan and its write, by dataset.
#[cfg(test)]
pub(super) static BEFORE_INSTALL_WRITE: parking_lot::Mutex<
    BTreeMap<String, Box<dyn FnOnce() + Send>>,
> = parking_lot::Mutex::new(BTreeMap::new());

/// How often an install plans again when another write changed the prefixes between its
/// plan and its write.
const INSTALL_ATTEMPTS: usize = 4;

/// `POST /$/resources/{id}/install/{ds}`: bind the namespaces of a vocabulary and of its
/// imports as prefixes of a dataset in one write. The write carries the entity tag of
/// the prefixes the plan read, so a concurrent change makes it plan again rather than
/// overwrite a binding it did not see.
async fn install(
    State(st): St,
    Path((id, ds_name)): Path<(String, String)>,
    p: Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    writable(&st)?;
    let replace = query(&uri)
        .get("replace")
        .is_some_and(|v| v == "true" || v == "1");
    let ds = crate::http::dataset(&st, &ds_name)?;
    let target = ds.main().unwrap_or_else(|| ds.clone());
    let mut attempt = 0;
    let plan = loop {
        attempt += 1;
        let plan = {
            let (st, target, id) = (st.clone(), target.clone(), id.clone());
            blocking(move || plan_install(&st, &target, &id)).await?
        };
        if !plan.conflicts.is_empty() && !replace {
            return Err(crate::http::err_body(
                StatusCode::CONFLICT,
                json!({
                    "error": format!(
                        "{} binds a prefix of the vocabulary to another IRI; set replace=true to rebind it",
                        target.name
                    ),
                    "code": "prefix-conflict",
                    "conflicts": plan.conflicts,
                }),
            ));
        }
        if plan.planned.is_empty() {
            break plan;
        }
        let body: serde_json::Map<String, Value> = plan
            .planned
            .iter()
            .map(|(p, i)| (p.clone(), Value::String(i.clone())))
            .collect();
        let bytes = serde_json::to_vec(&Value::Object(body)).unwrap_or_default();
        #[cfg(test)]
        {
            let hook = BEFORE_INSTALL_WRITE.lock().remove(&target.name);
            if let Some(hook) = hook {
                hook();
            }
        }
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&plan.etag) {
            h.insert(header::IF_MATCH, v);
        }
        match crate::settings::http::write(
            st.clone(),
            target.clone(),
            &crate::settings::prefixes::PREFIXES,
            crate::settings::http::Write::Patch(bytes.into()),
            &h,
        )
        .await
        {
            Ok(_) => break plan,
            Err(e)
                if e.status() == StatusCode::PRECONDITION_FAILED && attempt < INSTALL_ATTEMPTS =>
            {
                continue;
            }
            Err(e) => return Err(e),
        }
    };
    tracing::info!(
        target: "sparkles::audit",
        event = "resource_install",
        resource = id.as_str(),
        dataset = target.name.as_str(),
        prefixes = plan.planned.len(),
        principal = p.id().as_str()
    );
    let installed: Vec<Value> = plan
        .planned
        .iter()
        .map(|(p, i)| json!({"prefix": p, "iri": i}))
        .collect();
    Ok(Json(json!({
        "dataset": target.name,
        "installed": installed,
        "replaced": plan.conflicts,
        "unchanged": plan.unchanged,
    }))
    .into_response())
}
