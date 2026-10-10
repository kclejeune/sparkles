//! The OpenAPI 3.1 description of the HTTP API (spec X03), served at `/$/openapi.json`
//! and `/$/openapi.yaml` and printed by `sparkles openapi`.
//!
//! The document is built from Rust: `paths` lists the operations of each route template
//! of [`crate::auth::ROUTES`], `schemas` the bodies. Each operation's security and
//! `x-sparkles-permission` come from [`crate::auth::need`], so they follow the
//! authorization layer. A test compares the paths with the route table, and the
//! document with its checked-in copy, `docs/openapi.json`.

#[cfg(test)]
mod contract_tests;
mod paths;
mod schemas;
#[cfg(test)]
mod tests;
mod yaml;

use crate::auth::{self, Need, ServerPerm};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value as J, json};
use std::sync::OnceLock;

/// Where `externalDocs` point: the API reference in the repository.
const DOCS: &str = "https://github.com/kclejeune/sparkles/blob/main/docs/API.md";

/// The web UI's routes, which serve HTML and assets rather than the API.
#[cfg(test)]
pub(crate) const UI_ROUTES: &[&str] = &["/", "/ui", "/ui/", "/ui/{*path}"];

pub(crate) fn api_doc(anchor: &str) -> String {
    format!("{DOCS}#{anchor}")
}

pub(crate) fn sref(name: &str) -> J {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn param_ref(name: &str) -> J {
    json!({ "$ref": format!("#/components/parameters/{name}") })
}

fn response_ref(name: &str) -> J {
    json!({ "$ref": format!("#/components/responses/{name}") })
}

/// The OpenAPI path of a route template: `{*graph}` becomes `{graph}`.
pub(crate) fn openapi_path(route: &str) -> String {
    route.replace("{*", "{")
}

// ------------------------------------------------------------------ operations ----

/// One operation, built by [`op`] and its methods, and added to the document by
/// [`Paths::add`].
pub(crate) struct Op {
    route: &'static str,
    method: Method,
    obj: Map<String, J>,
    params: Vec<J>,
    responses: Map<String, J>,
}

/// An operation on `route` (a template of `auth::ROUTES`), with its id, tag and summary.
pub(crate) fn op(method: Method, route: &'static str, id: &str, tag: &str, summary: &str) -> Op {
    let mut obj = Map::new();
    obj.insert("operationId".into(), id.into());
    obj.insert("tags".into(), json!([tag]));
    obj.insert("summary".into(), summary.into());
    Op {
        route,
        method,
        obj,
        params: Vec::new(),
        responses: Map::new(),
    }
}

impl Op {
    /// The operation's description (CommonMark).
    pub fn doc(mut self, text: &str) -> Self {
        self.obj.insert("description".into(), text.into());
        self
    }

    /// A link to a section of `docs/API.md`.
    pub fn see(mut self, anchor: &str) -> Self {
        self.obj
            .insert("externalDocs".into(), json!({ "url": api_doc(anchor) }));
        self
    }

    /// A reusable parameter of `components.parameters`.
    pub fn param(mut self, name: &str) -> Self {
        self.params.push(param_ref(name));
        self
    }

    /// Several reusable parameters.
    pub fn params(mut self, names: &[&str]) -> Self {
        for n in names {
            self.params.push(param_ref(n));
        }
        self
    }

    /// An optional query parameter.
    pub fn query(mut self, name: &str, schema: J, description: &str) -> Self {
        self.params.push(json!({
            "name": name, "in": "query", "schema": schema, "description": description,
        }));
        self
    }

    /// A required query parameter.
    pub fn query_req(mut self, name: &str, schema: J, description: &str) -> Self {
        self.params.push(json!({
            "name": name, "in": "query", "required": true, "schema": schema,
            "description": description,
        }));
        self
    }

    /// A request header.
    pub fn header(mut self, name: &str, schema: J, description: &str) -> Self {
        self.params.push(json!({
            "name": name, "in": "header", "schema": schema, "description": description,
        }));
        self
    }

    /// The request body: `content` maps media types to media type objects.
    pub fn body(mut self, required: bool, description: &str, content: J) -> Self {
        self.obj.insert(
            "requestBody".into(),
            json!({ "required": required, "description": description, "content": content }),
        );
        self
    }

    /// A JSON request body of a named schema.
    pub fn json_body(self, required: bool, schema: &str) -> Self {
        let content = json!({ "application/json": { "schema": sref(schema) } });
        self.body(required, "", content)
    }

    /// A response with `content` (media types to media type objects), or none.
    pub fn resp(mut self, status: &str, description: &str, content: Option<J>) -> Self {
        let mut r = json!({ "description": description });
        if let Some(c) = content {
            r["content"] = c;
        }
        self.responses.insert(status.into(), r);
        self
    }

    /// A response with `headers` too.
    pub fn resp_h(
        mut self,
        status: &str,
        description: &str,
        content: Option<J>,
        headers: J,
    ) -> Self {
        self = self.resp(status, description, content);
        self.responses.get_mut(status).unwrap()["headers"] = headers;
        self
    }

    /// A shared response of `components.responses`.
    pub fn resp_ref(mut self, status: &str, name: &str) -> Self {
        self.responses.insert(status.into(), response_ref(name));
        self
    }

    /// A shared request body of `components.requestBodies`.
    pub fn body_ref(mut self, name: &str) -> Self {
        self.obj.insert(
            "requestBody".into(),
            json!({ "$ref": format!("#/components/requestBodies/{name}") }),
        );
        self
    }

    /// A JSON response of a named schema.
    pub fn json(self, status: &str, description: &str, schema: &str) -> Self {
        self.resp(
            status,
            description,
            Some(json!({ "application/json": { "schema": sref(schema) } })),
        )
    }

    /// A JSON response of an inline schema.
    pub fn json_inline(self, status: &str, description: &str, schema: J) -> Self {
        self.resp(
            status,
            description,
            Some(json!({ "application/json": { "schema": schema } })),
        )
    }

    /// `202` with the `Task` that does the work.
    pub fn task(self) -> Self {
        self.resp_ref("202", "TaskStarted")
    }

    /// `204` with no body.
    pub fn no_content(self, description: &str) -> Self {
        self.resp("204", description, None)
    }

    /// The named error responses of `components.responses` for these statuses.
    pub fn errors(mut self, statuses: &[u16]) -> Self {
        for s in statuses {
            self.responses
                .insert(s.to_string(), response_ref(error_response(*s)));
        }
        self
    }

    /// A vendor extension (`x-…`).
    pub fn ext(mut self, key: &str, value: J) -> Self {
        self.obj.insert(key.into(), value);
        self
    }

    /// Pagination of the listing, as `x-sparkles-pagination`.
    pub fn paginated(self, style: &str, request: &[&str], next: &str) -> Self {
        self.ext(
            "x-sparkles-pagination",
            json!({ "style": style, "parameters": request, "next": next }),
        )
    }
}

/// The name in `components.responses` of the error response for `status`.
fn error_response(status: u16) -> &'static str {
    match status {
        400 => "BadRequest",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "NotFound",
        405 => "MethodNotAllowed",
        408 => "Timeout",
        409 => "Conflict",
        410 => "Gone",
        412 => "PreconditionFailed",
        413 => "PayloadTooLarge",
        415 => "UnsupportedMediaType",
        422 => "Unprocessable",
        429 => "TooManyRequests",
        501 => "NotImplemented",
        503 => "Unavailable",
        507 => "InsufficientStorage",
        _ => panic!("no error response for {status}"),
    }
}

// --------------------------------------------------------------------- security ----

/// The need of a route and method, as `auth::need` answers it for a bare request.
fn need_of(route: &str, method: &Method) -> Option<Need> {
    let uri = "/x".parse().unwrap();
    auth::need(route, method, &uri, &HeaderMap::new())
}

/// `x-sparkles-permission` for a need.
fn permission(route: &str, need: Option<Need>) -> String {
    if route == "/{ds}" {
        return "read or write, by operation".into();
    }
    match need {
        Some(Need::Public) => "public".into(),
        Some(Need::Caller) => "any caller".into(),
        Some(Need::Authed) => "signed in".into(),
        Some(Need::Interactive) => "web session".into(),
        Some(Need::Dataset(l)) => l.as_str().into(),
        Some(Need::Server(p)) => p.as_str().into(),
        None => ServerPerm::ServerAdmin.as_str().into(),
    }
}

/// The security requirements of an operation with this need.
fn security(need: Option<Need>, method: &Method) -> J {
    let unsafe_method = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let session = if unsafe_method {
        json!({ "sessionCookie": [], "csrfToken": [] })
    } else {
        json!({ "sessionCookie": [] })
    };
    let all = [
        json!({ "basicAuth": [] }),
        json!({ "apiToken": [] }),
        json!({ "oidcAccessToken": [] }),
        session.clone(),
        json!({ "cloudflareAccess": [] }),
    ];
    match need {
        Some(Need::Public | Need::Caller) => {
            let mut v = vec![json!({})];
            v.extend(all);
            J::Array(v)
        }
        Some(Need::Interactive) => json!([session, { "cloudflareAccess": [] }]),
        _ => J::Array(all.to_vec()),
    }
}

/// The document's `security`: every scheme, for an operation that needs a permission
/// and is safe (GET, HEAD).
fn default_security() -> J {
    security(Some(Need::Authed), &Method::GET)
}

// ------------------------------------------------------------------------ paths ----

/// The operations, keyed by OpenAPI path and lowercase method.
#[derive(Default)]
pub(crate) struct Paths(Map<String, J>);

impl Paths {
    pub fn add(&mut self, o: Op) {
        let Op {
            route,
            method,
            mut obj,
            mut params,
            mut responses,
        } = o;
        let need = need_of(route, &method);
        // the path's parameters come first, in template order
        let mut path_params: Vec<J> = route
            .split('{')
            .skip(1)
            .map(|s| param_ref(s.split('}').next().unwrap().trim_start_matches('*')))
            .collect();
        path_params.append(&mut params);
        // a parameter added twice (by two helpers) is listed once
        let mut seen = Vec::new();
        path_params.retain(|p| {
            let dup = seen.contains(p);
            seen.push(p.clone());
            !dup
        });
        if !path_params.is_empty() {
            obj.insert("parameters".into(), J::Array(path_params));
        }
        let mut add = |status: u16| {
            responses
                .entry(status.to_string())
                .or_insert_with(|| response_ref(error_response(status)));
        };
        if !matches!(need, Some(Need::Public)) {
            add(401);
            add(403);
        }
        if matches!(need, Some(Need::Dataset(_))) || route.contains("{ds}") {
            add(404);
        }
        responses.insert("default".into(), response_ref("Error"));
        obj.insert("responses".into(), J::Object(responses));
        // the document's `security` applies unless the operation's differs
        let sec = security(need, &method);
        if sec != default_security() {
            obj.insert("security".into(), sec);
        }
        obj.insert(
            "x-sparkles-permission".into(),
            permission(route, need).into(),
        );
        let path = self
            .0
            .entry(openapi_path(route))
            .or_insert_with(|| json!({}));
        let m = method.as_str().to_ascii_lowercase();
        assert!(
            path.get(&m).is_none(),
            "{route} {method} is described twice"
        );
        path[m] = J::Object(obj);
    }
}

// --------------------------------------------------------------------- document ----

fn error_responses() -> Map<String, J> {
    let err = |d: &str| {
        json!({
            "description": d,
            "content": { "application/json": { "schema": sref("Error") } },
        })
    };
    let retry = json!({
        "Retry-After": { "description": "Seconds to wait before trying again.", "schema": { "type": "integer" } },
    });
    let mut m = Map::new();
    let mut put = |k: &str, v: J| {
        m.insert(k.into(), v);
    };
    put(
        "BadRequest",
        err("A parse error, a bad parameter or a malformed body."),
    );
    let mut unauth = err("Missing or invalid credentials.");
    unauth["headers"] = json!({
        "WWW-Authenticate": { "description": "The challenge: `Bearer realm=\"sparkles\"`, and `Basic` for browsers or when users are configured.", "schema": { "type": "string" } },
    });
    put("Unauthorized", unauth);
    put(
        "Forbidden",
        err(
            "The caller lacks the permission, a cross-origin request was refused, or the CSRF token is missing.",
        ),
    );
    put(
        "NotFound",
        err(
            "An unknown dataset or resource. A dataset the caller may not read answers the same way.",
        ),
    );
    put(
        "MethodNotAllowed",
        err("The method is not allowed here, such as an update sent with GET."),
    );
    put(
        "Timeout",
        err("The request ran out of time. `timeoutSeconds` names the timeout that applied."),
    );
    put(
        "Conflict",
        err("The request conflicts with the current state."),
    );
    put(
        "Gone",
        err("The commit or state is no longer kept (`history-gone`)."),
    );
    put(
        "PreconditionFailed",
        err("`If-Match` or `If-None-Match` failed (`code: precondition-failed`)."),
    );
    put("PayloadTooLarge", err("The body is over its ceiling."));
    put(
        "UnsupportedMediaType",
        err("An unsupported `Content-Type` or `Content-Encoding`."),
    );
    put(
        "Unprocessable",
        err(
            "The write-time validation guard rejected the write, or the formatter refused its output.",
        ),
    );
    let mut many = err("Over a rate limit.");
    many["headers"] = retry.clone();
    put("TooManyRequests", many);
    put("NotImplemented", err("The build lacks the feature."));
    let mut unavailable = err(
        "Cancelled, over a concurrency limit, offline, being restored, or the identity provider is unreachable.",
    );
    unavailable["headers"] = retry;
    put("Unavailable", unavailable);
    put(
        "InsufficientStorage",
        json!({
            "description": "Over a budget (`budget`), a storage quota or the free-disk reserve.",
            "content": { "application/json": { "schema": sref("BudgetError") } },
        }),
    );
    put("Error", err("An error. Every error has this body."));
    m
}

fn parameters() -> Map<String, J> {
    let path = |name: &str, d: &str| json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" }, "description": d });
    let q = |name: &str, schema: J, d: &str| json!({ "name": name, "in": "query", "schema": schema, "description": d });
    let h = |name: &str, schema: J, d: &str| json!({ "name": name, "in": "header", "schema": schema, "description": d });
    let s = || json!({ "type": "string" });
    let mut m = Map::new();
    let mut put = |k: &str, v: J| {
        m.insert(k.into(), v);
    };
    let mut ds = path("ds", "The dataset name.");
    ds["schema"]["pattern"] = "^[A-Za-z0-9_.-]+$".into();
    put("ds", ds);
    put(
        "name",
        path(
            "name",
            "The name of the stored query, snapshot, vector index, branch or ingest profile.",
        ),
    );
    let mut kind = path(
        "kind",
        "The settings kind: `assistant`, `memory` or `ingest`.",
    );
    kind["schema"]["enum"] = json!(["assistant", "memory", "ingest"]);
    put("kind", kind);
    put("id", path("id", "The id of the task, token or lock."));
    put("task", path("task", "The id of the ingestion task."));
    put("repo", path("repo", "The backup repository."));
    put("backup", path("backup", "The backup's id."));
    put("policy", path("policy", "The backup policy."));
    put(
        "reference",
        path("reference", "A commit: `42`, `commit:42` or `head`."),
    );
    put(
        "user_code",
        path(
            "user_code",
            "The user code of a device login, such as `WDJB-MJHT`.",
        ),
    );
    put(
        "graph",
        path(
            "graph",
            "The rest of the path, which may hold slashes. The graph is the one whose IRI is the request URL without its query.",
        ),
    );
    put(
        "at",
        q(
            "at",
            s(),
            "The state to read: `head`, `42`, `commit:42`, `time:<RFC 3339>` or `snapshot:NAME`.",
        ),
    );
    put(
        "branch",
        q(
            "branch",
            s(),
            "The branch to work on (default `main`). The path form `/{ds}@{branch}/…` chooses one too; the two must agree.",
        ),
    );
    put(
        "timeout",
        q(
            "timeout",
            json!({ "type": "number" }),
            "Seconds. Capped at the server's `--max-timeout`.",
        ),
    );
    put(
        "format",
        q(
            "format",
            s(),
            "The response format, in place of `Accept`. Fuseki's `output` and `results` are the same parameter.",
        ),
    );
    put(
        "reasoning",
        q(
            "reasoning",
            json!({ "type": "boolean" }),
            "Include materialized inferences. The default is true when the dataset has any.",
        ),
    );
    put(
        "graphSel",
        q(
            "graph",
            s(),
            "`default`, `union` or a graph IRI. `urn:x-arq:DefaultGraph` and `urn:x-arq:UnionGraph` work too.",
        ),
    );
    put(
        "limit",
        q(
            "limit",
            json!({ "type": "integer", "minimum": 1 }),
            "The page size.",
        ),
    );
    put(
        "cursor",
        q(
            "cursor",
            s(),
            "The `next` of the previous page. Send the same selection with it.",
        ),
    );
    put(
        "dryRun",
        q(
            "dryRun",
            json!({ "type": "boolean" }),
            "Run the write as a preview and roll it back. `dryRun` with no value means true.",
        ),
    );
    put(
        "changes",
        q(
            "changes",
            json!({ "type": "integer", "minimum": 0, "maximum": 10000 }),
            "With `dryRun`: list up to this many changed quads.",
        ),
    );
    put(
        "receipt",
        q(
            "receipt",
            json!({ "type": "boolean" }),
            "Add a commit receipt to the body.",
        ),
    );
    put(
        "validate",
        q(
            "validate",
            json!({ "type": "boolean" }),
            "`false` skips write-time validation when the server allows it (`--allow-unvalidated-writes`).",
        ),
    );
    put(
        "defaultGraphUri",
        q(
            "default-graph-uri",
            json!({ "type": "array", "items": { "type": "string" } }),
            "The graphs of the default graph (SPARQL Protocol).",
        ),
    );
    put(
        "namedGraphUri",
        q(
            "named-graph-uri",
            json!({ "type": "array", "items": { "type": "string" } }),
            "The named graphs (SPARQL Protocol).",
        ),
    );
    put(
        "execution",
        q(
            "execution",
            json!({ "type": "string", "enum": ["eager", "streaming", "auto"], "default": "eager" }),
            "Query execution mode. Streaming consumes bounded SELECT or graph batches with visible, budgeted materialization barriers; ASK stops after a qualifying solution. Auto conservatively selects streaming for large immutable SELECT scans and eligible uncached OPTIONAL counts, and eager execution otherwise. The default remains eager. Explicit streaming SELECT does not support Thrift results, and auto runs those requests eagerly. Deadlines include blocked response writes; late failures abort the body.",
        ),
    );
    put(
        "send",
        q(
            "send",
            json!({ "type": "integer" }),
            "The most rows serialized. Eager native metadata reports the full total; streaming stops production at the prefix and reports a null total unless exhausted.",
        ),
    );
    put(
        "nocache",
        q(
            "nocache",
            json!({ "type": "boolean" }),
            "Bypass the query result cache.",
        ),
    );
    put(
        "memoryMb",
        q(
            "memory-mb",
            json!({ "type": "integer", "minimum": 1 }),
            "A lower memory budget, in MiB.",
        ),
    );
    put(
        "maxRows",
        q(
            "max-rows",
            json!({ "type": "integer", "minimum": 1 }),
            "A lower budget of intermediate rows.",
        ),
    );
    put(
        "maxRowsProduced",
        q(
            "max-rows-produced",
            json!({ "type": "integer", "minimum": 1 }),
            "A budget of the rows all operators produce.",
        ),
    );
    put(
        "maxResultMb",
        q(
            "max-result-mb",
            json!({ "type": "integer", "minimum": 1 }),
            "A lower budget of the serialized result, in MiB.",
        ),
    );
    put(
        "describe",
        q(
            "describe",
            json!({ "type": "string", "enum": ["cbd", "scbd", "outgoing"] }),
            "The DESCRIBE mode of this query, over the dataset's setting.",
        ),
    );
    put(
        "describeLabels",
        q(
            "describe-labels",
            json!({ "type": "boolean" }),
            "Add the labels of the IRIs a DESCRIBE result links to.",
        ),
    );
    put(
        "describeReifiers",
        q(
            "describe-reifiers",
            json!({ "type": "boolean" }),
            "Include the reifiers of described triples.",
        ),
    );
    put(
        "describeMaxTriples",
        q(
            "describe-max-triples",
            json!({ "type": "integer", "minimum": 1 }),
            "A lower limit on the triples of a DESCRIBE result.",
        ),
    );
    put(
        "describeMaxDepth",
        q(
            "describe-max-depth",
            json!({ "type": "integer", "minimum": 1 }),
            "A lower limit on the levels a DESCRIBE follows.",
        ),
    );
    put(
        "gspDefault",
        q(
            "default",
            json!({ "type": "boolean" }),
            "Name the default graph (`?default`, no value needed).",
        ),
    );
    put(
        "gspGraph",
        q(
            "graph",
            s(),
            "The graph IRI. `default` and `union` name the default graph and the union of the named graphs.",
        ),
    );
    put(
        "ifMatch",
        h(
            "If-Match",
            s(),
            "Entity tags. A write needs a tag that names the current head.",
        ),
    );
    put(
        "ifNoneMatch",
        h("If-None-Match", s(), "Entity tags, or `*`."),
    );
    put(
        "commitMessage",
        h(
            "Sparkles-Commit-Message",
            s(),
            "The commit message of the write, at most 1024 bytes of UTF-8. RFC 8187 extended values are accepted.",
        ),
    );
    put(
        "dryRunHeader",
        h(
            "Sparkles-Dry-Run",
            json!({ "type": "boolean" }),
            "`true` runs the write as a preview.",
        ),
    );
    put(
        "acceptDatetime",
        h(
            "Accept-Datetime",
            s(),
            "An HTTP date. Read the last commit at or before it (RFC 7089).",
        ),
    );
    m
}

fn security_schemes() -> J {
    json!({
        "basicAuth": {
            "type": "http", "scheme": "basic",
            "description": "A configured user and password. An API token is accepted as the password, with any user name.",
        },
        "apiToken": {
            "type": "http", "scheme": "bearer", "bearerFormat": "spk_…",
            "description": "An API token minted at `/$/auth/tokens`, or a static token of the configuration.",
        },
        "oidcAccessToken": {
            "type": "http", "scheme": "bearer", "bearerFormat": "JWT",
            "description": "An access token of the OIDC provider, accepted when `oidc.api_audience` is set.",
        },
        "sessionCookie": {
            "type": "apiKey", "in": "cookie", "name": "sparkles_session",
            "description": "The web UI's session, set by `/$/auth/login` or the OIDC callback. Over https the cookie is `__Host-sparkles_session`.",
        },
        "csrfToken": {
            "type": "apiKey", "in": "header", "name": "X-Sparkles-CSRF",
            "description": "The `csrfToken` of `/$/whoami`. Session and proxy principals send it on unsafe requests.",
        },
        "cloudflareAccess": {
            "type": "apiKey", "in": "header", "name": "Cf-Access-Jwt-Assertion",
            "description": "The assertion Cloudflare Access adds to the requests it lets through, with `[cloudflare_access]` configured.",
        },
    })
}

const INFO: &str = "\
The HTTP API of Sparkles, a SPARQL server compatible with Apache Jena Fuseki. \
`docs/API.md` is the reference, and each operation links to its section.

**Authentication.** Without `sparkles serve --auth-config` the server needs no \
credentials, and every caller may do everything. With it, each operation lists the \
schemes it accepts, and `x-sparkles-permission` names the permission it needs: `public`, \
`any caller`, `signed in`, `web session`, a dataset level (`read`, `write`, `admin`) or a \
server permission (`metrics`, `federate`, `server-admin`). A caller without a level on a \
dataset gets `404`, as for a missing dataset. Session and proxy principals send \
`X-Sparkles-CSRF` on unsafe requests. Trusted proxy headers are honored only from \
configured proxies and are not listed as schemes.

**Errors.** Every non-2xx response has the `Error` body, with the `requestId` of the \
response's `X-Request-Id`. A budget error (`507`) adds `budget`, `limit` and `requested`.

**Pagination.** Listings that page say so in `x-sparkles-pagination`. Schema listings \
take `limit` and an opaque `cursor` and answer `{items, total, next}`. Commit listings \
take `limit` with `before` or `after`. Backup listings take `limit` and `before`. A null \
`next` ends the listing.

**Conditional requests.** Graph Store reads carry a weak `ETag` naming the commit read. \
`If-Match` and `If-None-Match` make writes conditional, and a failed condition answers \
`412`. Responses of queries and writes carry `Sparkles-Commit` and `Sparkles-Dataset-Id`.

**Coverage.** Every route of the server is described with its methods, parameters and \
media types. Every named schema is described member by member. Members the reference \
calls free-form, such as SHACL results and GeoJSON geometries, are open objects inside \
them, and a few answers, such as class profiles and MCP messages, are plain objects.";

fn tags() -> J {
    let t = |name: &str, d: &str, anchor: &str| json!({ "name": name, "description": d, "externalDocs": { "url": api_doc(anchor) } });
    json!([
        t(
            "Server",
            "Liveness, readiness, metrics and server information.",
            "server"
        ),
        t(
            "Datasets",
            "Creating, listing, cloning and administering datasets.",
            "datasets-admin"
        ),
        t(
            "SPARQL",
            "The SPARQL 1.1 Query and Update protocols, explain and stored query runs.",
            "per-dataset-sparql-protocol-fuseki-compatible"
        ),
        t(
            "Graph Store",
            "The SPARQL 1.1 Graph Store HTTP Protocol and uploads.",
            "per-dataset-sparql-protocol-fuseki-compatible"
        ),
        t(
            "Schema",
            "Classes, predicates, constraints and drafted shapes of a dataset.",
            "schema-discovery"
        ),
        t(
            "Stored queries",
            "Named, parameterized queries of a dataset.",
            "stored-queries"
        ),
        t(
            "GraphQL",
            "Read-only GraphQL over a dataset, and its mapping schema.",
            "graphql"
        ),
        t(
            "Branches",
            "Branches of a dataset and merges between them.",
            "branches-and-merges"
        ),
        t(
            "History",
            "Commits, diffs, the change feed, snapshots and retention.",
            "commits"
        ),
        t(
            "Search",
            "Full-text, vector and spatial indexes and searches.",
            "full-text-search"
        ),
        t(
            "Reasoning",
            "Materialized inferences and RDFS on read.",
            "reasoning-status-and-diagnostics"
        ),
        t(
            "Validation",
            "SHACL and ShEx validation, and write-time validation.",
            "write-time-validation"
        ),
        t("Tasks", "Background tasks.", "datasets-admin"),
        t(
            "Backups",
            "Backups of a dataset, in N-Quads files or backup repositories.",
            "backup-repositories"
        ),
        t(
            "Backup repositories",
            "Repositories on a file system or S3.",
            "backup-repositories"
        ),
        t(
            "Backup policies",
            "Scheduled backups and their retention.",
            "lifecycle-policies"
        ),
        t(
            "Formatting",
            "The formatter for SPARQL and RDF.",
            "formatting"
        ),
        t(
            "Authentication",
            "Signing in, sessions, CLI logins and the caller's permissions.",
            "authentication-and-access-control"
        ),
        t("Tokens", "API tokens.", "api-tokens"),
        t(
            "Fuseki",
            "Fuseki's admin routes: statistics, validators and backup files.",
            "validators"
        ),
        t(
            "MCP",
            "The Model Context Protocol endpoint.",
            "http-endpoint-mcp"
        ),
        t(
            "Models",
            "Model providers and the role lists of the question pipeline.",
            "model-providers"
        ),
        t(
            "Assistant",
            "Asking a dataset questions in plain language, the history of asked questions and feedback.",
            "asking-in-the-server"
        ),
        t("OpenAPI", "This description.", "openapi-description"),
    ])
}

/// The whole document, with `version` as `info.version`.
pub(crate) fn build(version: &str) -> J {
    let mut paths = Paths::default();
    paths::add_all(&mut paths);
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Sparkles HTTP API",
            "version": version,
            "description": INFO,
            "license": { "name": "Apache-2.0", "identifier": "Apache-2.0" },
        },
        "externalDocs": { "description": "API reference", "url": DOCS },
        "servers": [{ "url": "/", "description": "The server that serves this document." }],
        "tags": tags(),
        "security": default_security(),
        "paths": J::Object(paths.0),
        "components": {
            "schemas": J::Object(schemas::schemas()),
            "parameters": J::Object(parameters()),
            "responses": J::Object({
                let mut r = error_responses();
                r.extend(paths::shared_responses());
                r
            }),
            "requestBodies": J::Object(paths::shared_bodies()),
            "securitySchemes": security_schemes(),
        },
    })
}

/// `v` with the keys of every object in sorted order, whichever map type serde_json has.
pub(crate) fn sorted(v: &J) -> J {
    match v {
        J::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            J::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), sorted(&m[k])))
                    .collect(),
            )
        }
        J::Array(a) => J::Array(a.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// The document as pretty-printed JSON with sorted keys and a final newline.
pub(crate) fn to_json(doc: &J) -> String {
    let mut s = serde_json::to_string_pretty(&sorted(doc)).expect("serializable");
    s.push('\n');
    s
}

/// The document as YAML with sorted keys.
pub(crate) fn to_yaml(doc: &J) -> String {
    yaml::to_string(&sorted(doc))
}

struct Served {
    json: String,
    yaml: String,
    json_etag: String,
    yaml_etag: String,
}

fn served() -> &'static Served {
    static S: OnceLock<Served> = OnceLock::new();
    S.get_or_init(|| {
        let doc = build(env!("CARGO_PKG_VERSION"));
        let json = to_json(&doc);
        let yaml = to_yaml(&doc);
        let tag = |s: &str| {
            use sha2::Digest;
            let h = sha2::Sha256::digest(s.as_bytes());
            let hex: String = h[..12].iter().map(|b| format!("{b:02x}")).collect();
            format!("\"{hex}\"")
        };
        Served {
            json_etag: tag(&json),
            yaml_etag: tag(&yaml),
            json,
            yaml,
        }
    })
}

fn respond(headers: &HeaderMap, body: &'static str, etag: &str, media: &'static str) -> Response {
    let matches = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .map(|t| t.trim().trim_start_matches("W/"))
                .any(|t| t == etag || t == "*")
        });
    let etag = HeaderValue::from_str(etag).expect("hex etag");
    let common = [
        (header::ETAG, etag),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
    ];
    if matches {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    (
        common,
        [(header::CONTENT_TYPE, HeaderValue::from_static(media))],
        body,
    )
        .into_response()
}

/// `GET /$/openapi.json`
pub async fn serve_json(headers: HeaderMap) -> Response {
    let s = served();
    respond(&headers, &s.json, &s.json_etag, "application/json")
}

/// `GET /$/openapi.yaml`
pub async fn serve_yaml(headers: HeaderMap) -> Response {
    let s = served();
    respond(&headers, &s.yaml, &s.yaml_etag, "application/yaml")
}

/// `sparkles openapi`: print the document.
pub fn print(format: OutputFormat) -> anyhow::Result<()> {
    let doc = build(env!("CARGO_PKG_VERSION"));
    let text = match format {
        OutputFormat::Json => to_json(&doc),
        OutputFormat::Yaml => to_yaml(&doc),
    };
    crate::cli_docs::write_stdout(text.as_bytes())
}

/// The output of `sparkles openapi`.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum OutputFormat {
    Json,
    Yaml,
}
