//! The operations of every API route of `auth::ROUTES`, in the table's order.

use super::{Op, Paths, op, sref};
use axum::http::Method;
use serde_json::{Map, Value as J, json};

const GET: Method = Method::GET;
const HEAD: Method = Method::HEAD;
const POST: Method = Method::POST;
const PUT: Method = Method::PUT;
const DELETE: Method = Method::DELETE;
const PATCH: Method = Method::PATCH;

/// RDF syntaxes the server reads and writes, with whether they are binary.
const RDF: &[(&str, bool)] = &[
    ("text/turtle", false),
    ("application/n-triples", false),
    ("application/n-quads", false),
    ("application/trig", false),
    ("application/ld+json", false),
    ("application/rdf+xml", false),
    ("application/rdf+thrift", true),
    ("application/rdf+protobuf", true),
    ("application/rdf+json", false),
];

/// Result formats of SELECT and ASK beyond the two JSON ones.
const RESULTS: &[(&str, bool)] = &[
    ("application/sparql-results+xml", false),
    ("text/csv", false),
    ("text/tab-separated-values", false),
    ("application/sparql-results+thrift", true),
];

fn text() -> J {
    json!({ "schema": { "type": "string" } })
}

fn binary() -> J {
    json!({ "schema": { "type": "string", "contentMediaType": "application/octet-stream" } })
}

fn media(list: &[(&str, bool)], map: &mut Map<String, J>) {
    for (m, bin) in list {
        map.insert((*m).into(), if *bin { binary() } else { text() });
    }
}

/// RDF documents, as a request or response content map.
fn rdf() -> J {
    let mut m = Map::new();
    media(RDF, &mut m);
    J::Object(m)
}

/// RDF bodies a write reads: the RDF syntaxes and Jena's N3 media types, read as Turtle.
fn rdf_in() -> J {
    let mut m = Map::new();
    media(RDF, &mut m);
    for n3 in ["text/rdf+n3", "text/n3", "application/n3"] {
        m.insert(n3.into(), text());
    }
    J::Object(m)
}

/// Every response format of a SPARQL query: results for SELECT and ASK, RDF for
/// CONSTRUCT and DESCRIBE.
fn query_results() -> J {
    let mut m = Map::new();
    m.insert(
        "application/sparql-results+json".into(),
        json!({ "schema": sref("SparqlResults") }),
    );
    m.insert(
        "application/x-sparkles+json".into(),
        json!({ "schema": sref("SparklesResult") }),
    );
    media(RESULTS, &mut m);
    media(RDF, &mut m);
    J::Object(m)
}

fn s() -> J {
    json!({ "type": "string" })
}

fn int() -> J {
    json!({ "type": "integer" })
}

fn boolean() -> J {
    json!({ "type": "boolean" })
}

fn commit_headers() -> J {
    json!({
        "Sparkles-Commit": { "description": "The commit a read saw, or a write produced.", "schema": { "type": "integer" } },
        "Sparkles-Dataset-Id": { "description": "The dataset id.", "schema": { "type": "string" } },
    })
}

/// The parameters of a SPARQL query beyond `query`.
fn query_params(o: Op) -> Op {
    o.params(&[
        "defaultGraphUri",
        "namedGraphUri",
        "format",
        "timeout",
        "reasoning",
        "at",
        "send",
        "nocache",
        "memoryMb",
        "maxRows",
        "maxRowsProduced",
        "maxResultMb",
        "describe",
        "describeLabels",
        "describeReifiers",
        "describeMaxTriples",
        "describeMaxDepth",
    ])
    .query(
        "force-accept",
        s(),
        "Label the response `text/plain` so a browser shows it.",
    )
}

/// A query's success and error responses.
fn query_responses(o: Op) -> Op {
    o.resp_ref("200", "QueryResults")
        .errors(&[400, 408, 410, 413, 503, 507])
}

fn query_form() -> J {
    json!({
        "type": "object",
        "required": ["query"],
        "additionalProperties": true,
        "properties": {
            "query": { "type": "string" },
            "default-graph-uri": { "type": "array", "items": { "type": "string" } },
            "named-graph-uri": { "type": "array", "items": { "type": "string" } },
        },
        "description": "The query and, optionally, any query-string parameter of the operation.",
    })
}

fn update_form() -> J {
    json!({
        "type": "object",
        "required": ["update"],
        "additionalProperties": true,
        "properties": {
            "update": { "type": "string" },
            "using-graph-uri": { "type": "array", "items": { "type": "string" } },
            "using-named-graph-uri": { "type": "array", "items": { "type": "string" } },
        },
        "description": "The update and, optionally, any query-string parameter of the operation.",
    })
}

/// GET of a query endpoint: the query in `query=`, or the service description.
fn query_get(route: &'static str, id: &str) -> Op {
    let o = op(GET, route, id, "SPARQL", "Run a SPARQL query (GET)")
        .doc("The SPARQL 1.1 Query protocol with `query=`. Without `query`, a request that accepts RDF gets the dataset's SPARQL 1.1 Service Description. HEAD works the same way.")
        .see("per-dataset-sparql-protocol-fuseki-compatible")
        .query("query", s(), "The query. Leave it out for the service description.");
    query_responses(query_params(o))
}

/// POST of a query endpoint: the query as the body or a form.
fn query_post(route: &'static str, id: &str) -> Op {
    let o = op(POST, route, id, "SPARQL", "Run a SPARQL query (POST)")
        .doc("The SPARQL 1.1 Query protocol with an `application/sparql-query` body or a form.")
        .see("per-dataset-sparql-protocol-fuseki-compatible")
        .body_ref("SparqlQuery");
    query_responses(query_params(o))
}

/// The parameters of every write.
fn write_params(o: Op) -> Op {
    o.params(&[
        "timeout",
        "dryRun",
        "changes",
        "receipt",
        "validate",
        "commitMessage",
        "dryRunHeader",
    ])
    .query(
        "validationLimit",
        int(),
        "The most validation results reported when a write is rejected.",
    )
}

/// The answer of a Graph Store write or an upload.
fn write_ok(o: Op, status: &str) -> Op {
    o.resp_ref(status, "WriteDone")
}

/// The shared responses of `components.responses`.
pub(super) fn shared_responses() -> Map<String, J> {
    let mut m = Map::new();
    m.insert(
        "QueryResults".into(),
        json!({
            "description": "The results, negotiated with `Accept` or `format`: results for SELECT and ASK, RDF for CONSTRUCT and DESCRIBE.",
            "headers": commit_headers(),
            "content": query_results(),
        }),
    );
    m.insert(
        "GraphRead".into(),
        json!({
            "description": "The graph, or the whole dataset.",
            "headers": {
                "ETag": { "description": "A weak tag naming the commit and the serialization.", "schema": { "type": "string" } },
                "Sparkles-Commit": { "description": "The commit read.", "schema": { "type": "integer" } },
            },
            "content": rdf(),
        }),
    );
    m.insert(
        "WriteDone".into(),
        json!({
            "description": "The write's count, with a receipt when asked, or a dry run's report. A dry run with `Accept: application/rdf-patch` gets the change as RDF Patch.",
            "headers": commit_headers(),
            "content": {
                "application/json": { "schema": { "anyOf": [sref("WriteCount"), sref("Receipt"), sref("DryRunReport")] } },
                "application/rdf-patch": { "schema": { "type": "string" } },
            },
        }),
    );
    m.insert(
        "TaskStarted".into(),
        json!({
            "description": "The task was started. Follow it at `/$/tasks/{id}`.",
            "headers": {
                "Location": { "description": "Where to follow the result.", "schema": { "type": "string" } },
            },
            "content": { "application/json": { "schema": sref("Task") } },
        }),
    );
    m
}

/// The shared request bodies of `components.requestBodies`.
pub(super) fn shared_bodies() -> Map<String, J> {
    let mut m = Map::new();
    m.insert(
        "RdfData".into(),
        json!({
            "required": true,
            "description": "RDF data in the syntax its `Content-Type` names. Jena's N3 media types are read as Turtle.",
            "content": rdf_in(),
        }),
    );
    m.insert(
        "SparqlQuery".into(),
        json!({
            "required": true,
            "description": "The query, as the body or in a form.",
            "content": {
                "application/sparql-query": { "schema": { "type": "string" } },
                "application/x-www-form-urlencoded": { "schema": query_form() },
            },
        }),
    );
    m
}

fn write_errors(o: Op) -> Op {
    o.errors(&[400, 408, 412, 413, 415, 422, 503, 507])
}

/// The Graph Store operations of one route, with operation ids `{prefix}Get` and so on.
fn graph_store(p: &mut Paths, route: &'static str, prefix: &str, methods: &[Method], direct: bool) {
    let selector = |o: Op| {
        if direct {
            o
        } else {
            o.params(&["gspGraph", "gspDefault"])
        }
    };
    let read = |m: Method, suffix: &str, summary: &str| {
        let o = op(m, route, &format!("{prefix}{suffix}"), "Graph Store", summary)
            .doc("Reads a graph, or the whole dataset as N-Quads or TriG when no graph is named. The `ETag` names the commit read.")
            .see("per-dataset-sparql-protocol-fuseki-compatible");
        selector(o)
            .params(&[
                "format",
                "at",
                "reasoning",
                "ifMatch",
                "ifNoneMatch",
                "acceptDatetime",
            ])
            .resp_ref("200", "GraphRead")
            .resp("304", "Not modified (`If-None-Match`).", None)
            .errors(&[400, 410, 412, 507])
    };
    for m in methods {
        let o = match *m {
            Method::GET => read(GET, "Get", "Read a graph"),
            Method::HEAD => read(HEAD, "Head", "Read a graph's headers"),
            Method::PUT => {
                let o = op(
                    PUT,
                    route,
                    &format!("{prefix}Put"),
                    "Graph Store",
                    "Replace a graph",
                )
                .see("per-dataset-sparql-protocol-fuseki-compatible")
                .params(&["ifMatch", "ifNoneMatch"])
                .body_ref("RdfData");
                let o = write_ok(selector(o), "200").resp("201", "The graph was created.", None);
                write_errors(write_params(o))
            }
            Method::POST => {
                let o = op(
                    POST,
                    route,
                    &format!("{prefix}Post"),
                    "Graph Store",
                    "Add to a graph",
                )
                .see("per-dataset-sparql-protocol-fuseki-compatible")
                .params(&["ifMatch", "ifNoneMatch"])
                .body_ref("RdfData");
                let o = write_ok(selector(o), "200").resp("201", "The graph was created.", None);
                write_errors(write_params(o))
            }
            Method::DELETE => {
                let o = op(
                    DELETE,
                    route,
                    &format!("{prefix}Delete"),
                    "Graph Store",
                    "Delete a graph",
                )
                .see("per-dataset-sparql-protocol-fuseki-compatible")
                .params(&["ifMatch", "ifNoneMatch"]);
                let o = write_ok(selector(o), "200").resp("204", "The graph was deleted.", None);
                write_errors(write_params(o))
            }
            _ => unreachable!(),
        };
        p.add(o);
    }
}

/// An admin route that reads a dataset setting or status (GET), sets it (PUT) and removes
/// it (DELETE).
#[allow(clippy::too_many_arguments)]
fn setting(
    p: &mut Paths,
    route: &'static str,
    tag: &str,
    noun: &str,
    id: &str,
    status: &str,
    body: &str,
    anchor: &str,
) {
    p.add(
        op(
            GET,
            route,
            &format!("get{id}"),
            tag,
            &format!("Get the {noun}"),
        )
        .see(anchor)
        .json("200", &format!("The {noun}."), status),
    );
    p.add(
        op(
            PUT,
            route,
            &format!("set{id}"),
            tag,
            &format!("Set the {noun}"),
        )
        .see(anchor)
        .json_body(true, body)
        .json("200", &format!("The new {noun}."), status)
        .errors(&[400, 409]),
    );
    let delete = op(
        DELETE,
        route,
        &format!("delete{id}"),
        tag,
        &format!("Remove the {noun}"),
    )
    .see(anchor);
    // write-time validation answers 204; the others answer with the status after it
    let delete = if route == "/$/validation/{ds}" {
        delete.no_content("Removed.")
    } else {
        delete.json("200", &format!("The {noun} after the removal."), status)
    };
    p.add(delete);
}

pub(super) fn add_all(p: &mut Paths) {
    server(p);
    datasets(p);
    fuseki(p);
    schema(p);
    admin(p);
    queries(p);
    graphql(p);
    history(p);
    search(p);
    settings(p);
    format_and_mcp(p);
    backups(p);
    auth(p);
    protocol(p);
    openapi(p);
}

fn server(p: &mut Paths) {
    p.add(
        op(GET, "/$/ping", "ping", "Server", "Liveness check")
            .doc("Answers `200` with a timestamp whenever the process serves HTTP.")
            .see("server")
            .resp(
                "200",
                "The server's time.",
                Some(json!({ "text/plain": text() })),
            ),
    );
    p.add(
        op(
            POST,
            "/$/ping",
            "pingPost",
            "Server",
            "Liveness check (POST)",
        )
        .see("server")
        .resp(
            "200",
            "The server's time.",
            Some(json!({ "text/plain": text() })),
        ),
    );
    p.add(
        op(
            GET,
            "/$/whoami",
            "whoami",
            "Authentication",
            "The caller and its permissions",
        )
        .doc("Answers `401` only for invalid credentials.")
        .see("whoami")
        .json("200", "The caller.", "Whoami"),
    );
    for (m, id) in [(GET, "getServer"), (POST, "getServerPost")] {
        p.add(
            op(m, "/$/server", id, "Server", "Server information")
                .doc("The version, uptime, datasets and limits. Anonymous callers get no `version` or `limits` when auth is on.")
                .see("server")
                .json("200", "The server.", "ServerInfo"),
        );
    }
    p.add(
        op(GET, "/$/metrics", "metrics", "Server", "Metrics")
            .doc("Prometheus text format 0.0.4, or a JSON snapshot with `format=json`. `404` with `--no-metrics`.")
            .see("metrics")
            .query("format", json!({ "type": "string", "enum": ["json"] }), "`json` for a `MetricsSnapshot`.")
            .resp(
                "200",
                "The metrics.",
                Some(json!({
                    "text/plain": text(),
                    "application/json": { "schema": sref("MetricsSnapshot") },
                })),
            ),
    );
    p.add(
        op(GET, "/$/ready", "ready", "Server", "Readiness check")
            .doc("`200` when the server is ready and `503` otherwise, both with `ReadyInfo`. Without `metrics`, only readable datasets are listed.")
            .see("server")
            .json("200", "Ready.", "ReadyInfo")
            .json("503", "Starting or draining.", "ReadyInfo"),
    );
    p.add(
        op(
            GET,
            "/$/ready/{ds}",
            "readyDataset",
            "Server",
            "Readiness of one dataset",
        )
        .see("server")
        .json("200", "Ready.", "ReadyInfo")
        .json("503", "Not ready.", "ReadyInfo"),
    );
}

fn datasets(p: &mut Paths) {
    p.add(
        op(
            GET,
            "/$/datasets",
            "listDatasets",
            "Datasets",
            "List datasets",
        )
        .doc("The datasets the caller may read.")
        .see("datasets-admin")
        .json("200", "The datasets.", "DatasetList"),
    );
    p.add(
        op(POST, "/$/datasets", "createDataset", "Datasets", "Create a dataset")
            .doc("The form or JSON body has `dbName` and `dbType`, which may also be query parameters. A body in an RDF syntax is a Fuseki service description (an assembler).")
            .see("datasets-admin")
            .query("dbName", s(), "The dataset name.")
            .query("dbType", s(), "`persistent` (or `tdb2`, `tdb`) or `mem`.")
            .body(
                false,
                "The dataset to create.",
                json!({
                    "application/json": { "schema": sref("CreateDataset") },
                    "application/x-www-form-urlencoded": { "schema": sref("CreateDataset") },
                    "text/turtle": text(),
                    "application/trig": text(),
                    "application/n-triples": text(),
                    "application/n-quads": text(),
                    "application/rdf+xml": text(),
                    "application/ld+json": text(),
                }),
            )
            .json("201", "Created.", "DatasetInfo")
            .errors(&[400, 409, 501]),
    );
    p.add(
        op(
            GET,
            "/$/datasets/{ds}",
            "getDataset",
            "Datasets",
            "Get a dataset",
        )
        .see("datasets-admin")
        .json("200", "The dataset.", "DatasetInfo"),
    );
    p.add(
        op(POST, "/$/datasets/{ds}", "setDatasetState", "Datasets", "Take a dataset offline or bring it back")
            .doc("Fuseki's dataset state. An offline dataset answers `503` on its own endpoints. The state is not persisted.")
            .see("datasets-admin")
            .query_req("state", json!({ "type": "string", "enum": ["offline", "active"] }), "The new state.")
            .resp("200", "The state was set.", None)
            .errors(&[400]),
    );
    p.add(
        op(
            DELETE,
            "/$/datasets/{ds}",
            "deleteDataset",
            "Datasets",
            "Delete a dataset",
        )
        .doc("Removes the dataset and its files.")
        .see("datasets-admin")
        .resp("200", "Deleted.", None),
    );
    p.add(
        op(POST, "/$/datasets/{ds}/clone", "cloneDataset", "Datasets", "Clone a dataset")
            .doc("Copies one consistent snapshot into a new persistent dataset. Parameters come from the query string, a form or a JSON body.")
            .see("clone")
            .query("name", s(), "The new dataset's name.")
            .query("inferences", json!({ "type": "string", "enum": ["copy", "drop"] }), "Copy the inferred graph (default) or drop it.")
            .param("at")
            .body(
                false,
                "The clone's parameters.",
                json!({
                    "application/json": { "schema": sref("CloneRequest") },
                    "application/x-www-form-urlencoded": { "schema": sref("CloneRequest") },
                }),
            )
            .task()
            .errors(&[400, 409]),
    );
    for (m, id) in [(GET, "getDatasetStats"), (POST, "getDatasetStatsPost")] {
        p.add(
            op(m, "/$/stats/{ds}", id, "Datasets", "Dataset statistics")
                .doc("Sizes, graphs, top predicates and classes, caches, reasoning, quota and Fuseki's request counters.")
                .see("datasets-admin")
                .param("at")
                .json("200", "The statistics.", "DatasetStats"),
        );
    }
}

fn fuseki(p: &mut Paths) {
    for (m, id) in [(GET, "fusekiStats"), (POST, "fusekiStatsPost")] {
        p.add(
            op(m, "/$/stats", id, "Fuseki", "Fuseki's statistics")
                .doc("The request counters of every dataset the caller may read.")
                .see("datasets-admin")
                .json("200", "The counters.", "FusekiStats"),
        );
    }
    for (m, id) in [(GET, "listBackupFiles"), (POST, "listBackupFilesPost")] {
        p.add(
            op(m, "/$/backups-list", id, "Fuseki", "List N-Quads backup files")
                .doc("The files in `<data>/backups`. A caller without `server-admin` sees those of the datasets it administers.")
                .see("datasets-admin")
                .json("200", "The file names, sorted.", "BackupFiles"),
        );
    }
    type Fields = &'static [(&'static str, &'static str)];
    const SYNTAX: (&str, &str) = ("languageSyntax", "`SPARQL` (default) or `ARQ`.");
    let validators: [(&'static str, &str, &str, Fields); 5] = [
        (
            "/$/validate/query",
            "Query",
            "Validate a SPARQL query",
            &[("query", "The query."), SYNTAX],
        ),
        (
            "/$/validate/update",
            "Update",
            "Validate a SPARQL update",
            &[("update", "The update."), SYNTAX],
        ),
        (
            "/$/validate/iri",
            "Iri",
            "Validate IRIs",
            &[("iri", "An IRI (repeatable).")],
        ),
        (
            "/$/validate/data",
            "Data",
            "Validate RDF data",
            &[
                ("data", "The data."),
                (
                    "languageSyntax",
                    "Jena's syntax name, `N-Quads` by default.",
                ),
            ],
        ),
        (
            "/$/validate/langtag",
            "Langtag",
            "Validate language tags",
            &[("langtag", "A language tag (repeatable); `lang` works too.")],
        ),
    ];
    for (route, id, summary, params) in validators {
        for (m, suffix) in [(GET, ""), (POST, "Post")] {
            let mut o = op(m.clone(), route, &format!("validate{id}{suffix}"), "Fuseki", summary)
                .doc("Answers in JSON when `Accept` prefers `application/json` to `text/html`, else as an HTML page.")
                .see("validators");
            if m == GET {
                for (q, d) in params {
                    o = o.query(q, s(), d);
                }
            } else {
                let mut props = Map::new();
                for (q, d) in params {
                    props.insert((*q).into(), json!({ "type": "string", "description": d }));
                }
                o = o.body(
                    true,
                    "The input.",
                    json!({ "application/x-www-form-urlencoded": { "schema": { "type": "object", "properties": props } } }),
                );
            }
            p.add(
                o.resp(
                    "200",
                    "The result.",
                    Some(json!({
                        "application/json": { "schema": sref("ValidatorResult") },
                        "text/html": text(),
                    })),
                )
                .errors(&[400]),
            );
        }
    }
}

fn schema(p: &mut Paths) {
    let selection = |o: Op| {
        o.param("graphSel")
            .query(
                "declaredGraph",
                s(),
                "The graphs read for declarations; the same as `graph` by default.",
            )
            .param("reasoning")
            .query(
                "declared",
                json!({ "type": "string", "enum": ["asserted", "all"] }),
                "`all` also reads declarations from the inferred graph.",
            )
            .query(
                "detail",
                json!({ "type": "string", "enum": ["subjectClasses"] }),
                "List the classes of each predicate's subjects.",
            )
            .param("timeout")
            .param("at")
    };
    p.add(
        selection(op(GET, "/$/schema/{ds}", "getSchema", "Schema", "Schema summary"))
            .doc("Classes and predicates with exact counts and their declarations, and the constraints layer. An RDF `Accept` or `format` gets a VoID description.")
            .see("schema-discovery")
            .query("shapes", s(), "The constraints layer's sources: `guard`, `default`, a graph IRI or `none` (repeatable).")
            .param("limit")
            .param("format")
            .query("declarations", boolean(), "VoID: `false` leaves the declarations out.")
            .resp(
                "200",
                "The report, or its VoID description.",
                Some({
                    let mut m = Map::new();
                    m.insert("application/json".into(), json!({ "schema": sref("SchemaSummary") }));
                    media(&RDF[..6], &mut m);
                    J::Object(m)
                }),
            )
            .errors(&[400, 408, 409, 413]),
    );
    for (route, id, summary, page) in [
        (
            "/$/schema/{ds}/classes",
            "listSchemaClasses",
            "List classes",
            "ClassPage",
        ),
        (
            "/$/schema/{ds}/predicates",
            "listSchemaPredicates",
            "List predicates",
            "PredicatePage",
        ),
    ] {
        p.add(
            selection(op(GET, route, id, "Schema", summary))
                .see("schema-discovery")
                .params(&["limit", "cursor"])
                .json("200", "One page.", page)
                .errors(&[400, 408, 409, 413])
                .paginated("cursor", &["limit", "cursor"], "next"),
        );
    }
    p.add(
        op(GET, "/$/schema/{ds}/shapes", "draftShapes", "Schema", "Draft shapes from the data")
            .doc("SHACL shapes and a ShEx schema drafted from the data, as a starting point for write-time validation.")
            .see("drafted-shapes")
            .param("graphSel")
            .param("reasoning")
            .query("support", json!({ "type": "number", "exclusiveMinimum": 0, "maximum": 1 }), "The share of instances a constraint must hold for.")
            .param("format")
            .param("timeout")
            .resp(
                "200",
                "The drafted shapes.",
                Some(json!({
                    "application/json": { "schema": { "type": "object" } },
                    "text/turtle": text(),
                    "text/shex": text(),
                    "text/shaclc": text(),
                })),
            )
            .errors(&[400, 408]),
    );
    p.add(
        op(
            GET,
            "/$/schema/{ds}/constraints",
            "getSchemaConstraints",
            "Schema",
            "The constraints layer",
        )
        .doc("What the dataset's SHACL shapes require of the instances of each class.")
        .see("constraints-layer")
        .query(
            "shapes",
            s(),
            "The sources: `guard`, `default`, a graph IRI or `none` (repeatable).",
        )
        .json("200", "The constraints layer.", "ConstraintsLayer")
        .errors(&[400]),
    );
    p.add(
        op(
            GET,
            "/$/schema/{ds}/profiles",
            "getClassProfiles",
            "Schema",
            "Class profiles",
        )
        .doc("For each class with instances, the predicates its instances use, with counts, cardinalities and object kinds, and the predicates that point to them.")
        .see("class-profiles")
        .param("graphSel")
        .param("reasoning")
        .param("at")
        .param("timeout")
        .query("class", s(), "Profile only this class (repeatable).")
        .resp(
            "200",
            "The class profiles.",
            Some(json!({ "application/json": { "schema": { "type": "object" } } })),
        )
        .errors(&[400, 408, 410]),
    );
    p.add(
        op(
            GET,
            "/$/schema/{ds}/diff",
            "getSchemaDiff",
            "Schema",
            "Schema diff",
        )
        .doc("Compares the schema reports of two states of the dataset, field by field.")
        .see("schema-diffs")
        .query_req("from", s(), "The earlier state, in any form `at` takes.")
        .query(
            "to",
            s(),
            "The later state, in any form `at` takes (default: the head).",
        )
        .param("graphSel")
        .param("reasoning")
        .param("timeout")
        .resp(
            "200",
            "The changes between the two reports.",
            Some(json!({ "application/json": { "schema": { "type": "object" } } })),
        )
        .errors(&[400, 408, 410]),
    );
}

fn admin(p: &mut Paths) {
    p.add(
        op(
            POST,
            "/$/compact/{ds}",
            "compactDataset",
            "Datasets",
            "Compact a dataset",
        )
        .doc("Merges the delta into a new index generation while writes go on.")
        .see("datasets-admin")
        .task()
        .errors(&[409]),
    );
    p.add(
        op(
            POST,
            "/$/backup/{ds}",
            "backupNquads",
            "Backups",
            "Write an N-Quads backup file",
        )
        .doc("Writes a compressed N-Quads dump to `<data>/backups`.")
        .see("datasets-admin")
        .query(
            "compression",
            json!({ "type": "string", "enum": ["gzip", "zstd", "brotli", "lz4", "none"] }),
            "The codec.",
        )
        .query("level", int(), "The codec's level.")
        .task(),
    );
    p.add(
        op(GET, "/$/reason/{ds}", "getReasoning", "Reasoning", "Reasoning status")
            .doc("The materialized inferences and whether they are current, or `{reasoning: null, head}`.")
            .see("reasoning-status-and-diagnostics")
            .json("200", "The status.", "ReasoningStatus"),
    );
    p.add(
        op(
            POST,
            "/$/reason/{ds}",
            "reason",
            "Reasoning",
            "Materialize inferences",
        )
        .see("datasets-admin")
        .body(
            true,
            "The profile and its inputs.",
            json!({
                "application/json": { "schema": sref("ReasonRequest") },
                "application/x-www-form-urlencoded": { "schema": { "type": "object" } },
            }),
        )
        .task()
        .errors(&[400, 409]),
    );
    p.add(
        op(
            DELETE,
            "/$/reason/{ds}",
            "clearReasoning",
            "Reasoning",
            "Drop materialized inferences",
        )
        .see("datasets-admin")
        .json_inline("200", "Dropped.", json!({ "type": "object" })),
    );
    p.add(
        op(
            PUT,
            "/$/reason/{ds}/auto",
            "setAutoReasoning",
            "Reasoning",
            "Set automatic re-runs",
        )
        .see("datasets-admin")
        .json_body(true, "AutoReasonRequest")
        .json("200", "The reasoning status.", "ReasoningStatus")
        .errors(&[400, 409]),
    );
    p.add(
        op(
            DELETE,
            "/$/reason/{ds}/auto",
            "clearAutoReasoning",
            "Reasoning",
            "Remove the dataset's automatic re-run setting",
        )
        .see("datasets-admin")
        .json("200", "The reasoning status.", "ReasoningStatus"),
    );
    p.add(
        op(
            GET,
            "/$/reason/{ds}/diagnostics",
            "reasoningDiagnostics",
            "Reasoning",
            "Inconsistency diagnostics",
        )
        .doc("OWL 2 RL inconsistency checks.")
        .see("reasoning-status-and-diagnostics")
        .json("200", "The report.", "DiagnosticsReport"),
    );
    p.add(
        op(GET, "/$/tasks", "listTasks", "Tasks", "List tasks")
            .doc("Tasks of the datasets the caller may read. Server-wide tasks are listed for `server-admin` only.")
            .see("datasets-admin")
            .json_inline("200", "The tasks.", json!({ "type": "array", "items": sref("Task") })),
    );
    p.add(
        op(GET, "/$/tasks/{id}", "getTask", "Tasks", "Get a task")
            .see("datasets-admin")
            .json("200", "The task.", "Task"),
    );
    p.add(
        op(DELETE, "/$/tasks/{id}", "cancelTask", "Tasks", "Cancel a task")
            .doc("Needs `admin` on the task's dataset, checked by the handler. A task that cannot be cancelled now is a `409` with `code: not-cancellable`.")
            .see("datasets-admin")
            .resp("202", "Cancellation requested; the task ends `cancelled`.", Some(json!({ "application/json": { "schema": sref("Task") } })))
            .errors(&[409]),
    );
    p.add(
        op(
            GET,
            "/$/prefixes/{ds}",
            "getAllPrefixes",
            "Datasets",
            "Prefixes of a dataset",
        )
        .doc("The dataset's prefixes plus well-known ones.")
        .see("datasets-admin")
        .json("200", "The prefixes.", "Prefixes"),
    );
    p.add(
        op(
            POST,
            "/$/cache/clear/{ds}",
            "clearCache",
            "Datasets",
            "Clear the query result cache and the cache of remote SERVICE results",
        )
        .see("datasets-admin")
        .json("200", "What was dropped.", "CacheCleared"),
    );
}

fn queries(p: &mut Paths) {
    p.add(
        op(
            GET,
            "/$/queries/{ds}",
            "listStoredQueries",
            "Stored queries",
            "List stored queries",
        )
        .see("stored-queries")
        .json(
            "200",
            "The definitions without their text.",
            "StoredQueryList",
        ),
    );
    p.add(
        op(
            GET,
            "/$/queries/{ds}/{name}",
            "getStoredQuery",
            "Stored queries",
            "Get a stored query",
        )
        .see("stored-queries")
        .query("version", int(), "An older version, while it is kept.")
        .resp_h(
            "200",
            "The definition.",
            Some(json!({ "application/json": { "schema": sref("StoredQuery") } })),
            json!({ "ETag": { "description": "`\"v<N>\"`", "schema": { "type": "string" } } }),
        ),
    );
    p.add(
        op(PUT, "/$/queries/{ds}/{name}", "putStoredQuery", "Stored queries", "Store a query")
            .doc("Stores the definition as the next version. `If-Match: \"v<N>\"` stores only over version N, and `If-None-Match: *` only when the query does not exist.")
            .see("stored-queries")
            .params(&["ifMatch", "ifNoneMatch", "commitMessage"])
            .json_body(true, "StoredQuery")
            .json("200", "Stored as a new version, or unchanged (`changed: false`).", "StoredQuery")
            .json("201", "Created.", "StoredQuery")
            .errors(&[400, 409, 412]),
    );
    p.add(
        op(
            DELETE,
            "/$/queries/{ds}/{name}",
            "deleteStoredQuery",
            "Stored queries",
            "Delete a stored query",
        )
        .see("stored-queries")
        .param("ifMatch")
        .no_content("Deleted with its versions.")
        .errors(&[412]),
    );
    p.add(
        op(
            GET,
            "/$/queries/{ds}/{name}/versions",
            "listStoredQueryVersions",
            "Stored queries",
            "List the versions of a stored query",
        )
        .see("stored-queries")
        .json(
            "200",
            "The kept versions, newest first.",
            "StoredQueryVersions",
        ),
    );
    for (m, id, summary) in [
        (GET, "runStoredQuery", "Run a stored query"),
        (POST, "runStoredQueryPost", "Run a stored query (POST)"),
    ] {
        let mut o = op(m.clone(), "/{ds}/queries/{name}", id, "SPARQL", summary)
            .doc("The query's parameters are request parameters by name (`minAge=40` or `$minAge=40`). The run takes the parameters of `/{ds}/sparql`, and `version`.")
            .see("stored-queries")
            .params(&["format", "timeout", "reasoning", "nocache", "at", "send"])
            .query("version", int(), "Run an older version.");
        if m == POST {
            o = o.body(
                false,
                "Parameter values.",
                json!({
                    "application/json": { "schema": { "type": "object", "additionalProperties": true } },
                    "application/x-www-form-urlencoded": { "schema": { "type": "object", "additionalProperties": true } },
                }),
            );
        }
        p.add(
            o.resp_h(
                "200",
                "The results.",
                Some(query_results()),
                json!({ "Sparkles-Query-Version": { "description": "The version run.", "schema": { "type": "integer" } } }),
            )
            .errors(&[400, 408, 507]),
        );
    }
}

fn graphql_response() -> J {
    let schema = json!({
        "type": "object",
        "description": "A GraphQL response: `data`, `errors` with `extensions.code`, and `extensions.sparkles` with the commit read and, with `explain=true`, each fetch group's SPARQL, rows and time.",
        "properties": {
            "data": { "type": ["object", "null"] },
            "errors": { "type": "array", "items": { "type": "object" } },
            "extensions": { "type": "object" },
        },
    });
    json!({
        "application/graphql-response+json": { "schema": schema.clone() },
        "application/json": { "schema": schema },
    })
}

fn graphql(p: &mut Paths) {
    let config = || {
        json!({
            "application/json": { "schema": {
                "type": "object",
                "description": "The configuration: `sdl` (the mapping schema), `dataGraph`, `reasoning`, `introspection` and `limits`, with the version's `version`, `parent`, `created`, `author`, `message`, `datasetCommit` and `digest`.",
                "additionalProperties": true,
            } },
        })
    };
    for (m, id, summary) in [
        (GET, "graphqlGet", "Run a GraphQL query"),
        (POST, "graphqlPost", "Run a GraphQL query (POST)"),
    ] {
        let mut o = op(m.clone(), "/{ds}/graphql", id, "GraphQL", summary)
            .doc("Runs a GraphQL document against the dataset's installed schema, as the GraphQL over HTTP draft defines it. `GET` takes `query`, `operationName` and `variables` (JSON) in the query string and runs queries only. The parameters `at`, `timeout`, `reasoning`, `nocache`, `explain` and the budget overrides of `/{ds}/sparql` apply. The response is `application/graphql-response+json` when the client accepts it, else `application/json`; a document that does not validate answers `422` under the first and `200` under the second.")
            .see("graphql")
            .params(&["timeout", "reasoning", "nocache", "at"])
            .query("explain", boolean(), "Add each fetch group's SPARQL, rows and time to `extensions.sparkles.plan`.");
        if m == GET {
            o = o
                .query("query", s(), "The GraphQL document.")
                .query("operationName", s(), "The operation to run.")
                .query("variables", s(), "The variables as a JSON object.");
        } else {
            o = o.body(
                true,
                "A GraphQL request: `query`, `operationName` and `variables` as JSON, or the document alone as `application/graphql`.",
                json!({
                    "application/json": { "schema": {
                        "type": "object",
                        "required": ["query"],
                        "properties": {
                            "query": { "type": "string" },
                            "operationName": { "type": ["string", "null"] },
                            "variables": { "type": ["object", "null"] },
                            "extensions": { "type": ["object", "null"] },
                        },
                    } },
                    "application/graphql": { "schema": text() },
                }),
            );
        }
        let o = o
            .resp_h("200", "The response, with or without execution errors.", Some(graphql_response()), commit_headers())
            .resp("400", "A malformed request, or a document that does not parse.", Some(graphql_response()))
            .resp("404", "No such dataset, or no schema installed.", Some(graphql_response()))
            .resp("422", "A document that does not validate, or over a limit (`application/graphql-response+json`).", Some(graphql_response()))
            .errors(&[405, 408, 503, 507]);
        let o = if m == POST { o.errors(&[415]) } else { o };
        p.add(o);
    }
    p.add(
        op(
            GET,
            "/{ds}/graphql/schema",
            "graphqlApiSchema",
            "GraphQL",
            "Get the API schema",
        )
        .doc("The schema clients see, as SDL, without the mapping directives.")
        .see("graphql")
        .resp(
            "200",
            "The API schema.",
            Some(json!({ "text/plain": text() })),
        )
        .errors(&[404]),
    );
    p.add(
        op(
            GET,
            "/$/graphql/{ds}",
            "getGraphqlConfig",
            "GraphQL",
            "Get the GraphQL configuration",
        )
        .see("graphql")
        .query("version", int(), "An older version, while it is kept.")
        .resp_h(
            "200",
            "The configuration with its version.",
            Some(config()),
            json!({ "ETag": { "description": "`\"v<N>\"`", "schema": { "type": "string" } } }),
        )
        .errors(&[404]),
    );
    p.add(
        op(PUT, "/$/graphql/{ds}", "putGraphqlConfig", "GraphQL", "Install a GraphQL schema")
            .doc("Checks the mapping schema and installs it as the next version. The body is the configuration as JSON, with an optional `message`, or the SDL alone as `application/graphql`, which keeps the other fields. The answer has `changed` and `warnings`, such as a non-null field no write-time guard backs. `If-Match: \"v<N>\"` installs only over version N, and `If-None-Match: *` only when none is installed.")
            .see("graphql")
            .params(&["ifMatch", "ifNoneMatch", "commitMessage"])
            .body(
                true,
                "",
                json!({
                    "application/json": { "schema": { "type": "object", "required": ["sdl"], "additionalProperties": true } },
                    "application/graphql": { "schema": text() },
                }),
            )
            .resp("200", "Installed as a new version, or unchanged (`changed: false`).", Some(config()))
            .resp("201", "Installed.", Some(config()))
            .errors(&[400, 409, 412]),
    );
    p.add(
        op(
            DELETE,
            "/$/graphql/{ds}",
            "deleteGraphqlConfig",
            "GraphQL",
            "Remove the GraphQL configuration",
        )
        .see("graphql")
        .param("ifMatch")
        .no_content("Removed with its versions.")
        .errors(&[404, 412]),
    );
    p.add(
        op(GET, "/$/graphql/{ds}/versions", "listGraphqlVersions", "GraphQL", "List the versions of the GraphQL configuration")
            .see("graphql")
            .resp(
                "200",
                "The kept versions, newest first.",
                Some(json!({ "application/json": { "schema": { "type": "object", "additionalProperties": true } } })),
            ),
    );
    p.add(
        op(GET, "/$/graphql/{ds}/draft", "draftGraphqlSchema", "GraphQL", "Draft a mapping schema")
            .doc("Drafts a mapping schema for review, from SHACL shapes (`source=shapes`: the write-time guard's, or the graph `shapesGraph`) or from the data (`source=observed`, with the selection of `/$/schema/{ds}/shapes`). Nothing is installed.")
            .see("graphql")
            .query("source", json!({ "type": "string", "enum": ["shapes", "observed"] }), "Shapes when there is a shapes graph or a SHACL guard, else observed.")
            .query("shapesGraph", s(), "A named graph of SHACL shapes.")
            .query("support", json!({ "type": "number" }), "Observed: the share of instances a constraint must hold for (default 1).")
            .query("graph", s(), "Observed: `default`, `union` or a graph IRI.")
            .query("class", s(), "Observed: only these classes (repeatable).")
            .query("minInstances", int(), "Observed: skip classes with fewer instances.")
            .query("format", json!({ "type": "string", "enum": ["sdl", "json"] }), "`json` for the decisions behind the draft.")
            .param("timeout")
            .resp(
                "200",
                "The draft.",
                Some(json!({
                    "text/plain": text(),
                    "application/json": { "schema": { "type": "object", "additionalProperties": true } },
                })),
            )
            .errors(&[400]),
    );
}

fn history(p: &mut Paths) {
    p.add(
        op(
            GET,
            "/$/commits/{ds}",
            "listCommits",
            "History",
            "List commits",
        )
        .doc("Newest first, or oldest first after `after`.")
        .see("commits")
        .query(
            "limit",
            json!({ "type": "integer", "minimum": 1, "maximum": 1000, "default": 50 }),
            "The page size.",
        )
        .query("before", int(), "List the commits before this one.")
        .query(
            "after",
            int(),
            "List the commits after this one, oldest first.",
        )
        .json("200", "One page of commits.", "CommitList")
        .paginated("seq", &["limit", "before", "after"], "next"),
    );
    p.add(
        op(
            GET,
            "/$/commits/{ds}/{reference}",
            "getCommit",
            "History",
            "Get a commit",
        )
        .see("commits")
        .json("200", "The commit.", "CommitResponse")
        .errors(&[400, 410]),
    );
    p.add(
        op(
            GET,
            "/$/snapshots/{ds}",
            "listSnapshots",
            "History",
            "List named snapshots",
        )
        .see("named-snapshots-and-retention")
        .json("200", "The snapshots.", "SnapshotList"),
    );
    p.add(
        op(
            POST,
            "/$/snapshots/{ds}",
            "createSnapshot",
            "History",
            "Create a named snapshot",
        )
        .doc("Pins a commit under a name. The body is JSON, a form or the query string.")
        .see("named-snapshots-and-retention")
        .body(
            false,
            "The snapshot.",
            json!({
                "application/json": { "schema": sref("SnapshotRequest") },
                "application/x-www-form-urlencoded": { "schema": sref("SnapshotRequest") },
            }),
        )
        .json("200", "The name already pins that commit.", "NamedSnapshot")
        .json("201", "Created.", "NamedSnapshot")
        .errors(&[400, 409, 410]),
    );
    p.add(
        op(
            GET,
            "/$/snapshots/{ds}/{name}",
            "getSnapshot",
            "History",
            "Get a named snapshot",
        )
        .see("named-snapshots-and-retention")
        .json("200", "The snapshot.", "NamedSnapshot"),
    );
    p.add(
        op(
            DELETE,
            "/$/snapshots/{ds}/{name}",
            "deleteSnapshot",
            "History",
            "Delete a named snapshot",
        )
        .see("named-snapshots-and-retention")
        .no_content("Deleted."),
    );
    p.add(
        op(
            GET,
            "/$/history/{ds}",
            "getHistory",
            "History",
            "History retention status",
        )
        .see("named-snapshots-and-retention")
        .json("200", "The status.", "HistoryStatus"),
    );
    p.add(
        op(
            PUT,
            "/$/history/{ds}",
            "setHistory",
            "History",
            "Set the retention window",
        )
        .see("named-snapshots-and-retention")
        .json_body(true, "HistoryRequest")
        .json("200", "The status.", "HistoryStatus")
        .errors(&[400]),
    );
    p.add(
        op(GET, "/{ds}/diff", "diff", "History", "Diff two states")
            .doc("The net change between two readable states, as JSON, diff lines or RDF Patch.")
            .see("diffs-between-commits")
            .query(
                "from",
                s(),
                "A selector; by default the commit before `to`.",
            )
            .query("to", s(), "A selector; by default the head.")
            .query("graph", s(), "A graph IRI or `default`.")
            .query("quads", boolean(), "JSON: list the quads.")
            .query("limit", int(), "The most quads listed.")
            .param("format")
            .resp(
                "200",
                "The diff.",
                Some(json!({
                    "application/json": { "schema": sref("Diff") },
                    "text/x-sparkles-diff": text(),
                    "application/rdf-patch": text(),
                    "application/rdf-patch+thrift": binary(),
                })),
            )
            .resp("304", "Not modified.", None)
            .errors(&[400, 410, 507]),
    );
    p.add(
        op(GET, "/{ds}/changes", "changes", "History", "Change feed")
            .doc("The commits after `after`, oldest first, with their changes. `wait` long-polls, and `Accept: text/event-stream` streams the commits as server-sent events.")
            .see("change-feed")
            .query("after", s(), "The commit to start after; the head by default.")
            .query("limit", json!({ "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }), "The most commits listed.")
            .query("wait", json!({ "type": "number", "maximum": 60 }), "Seconds to wait for a commit.")
            .param("format")
            .header("Last-Event-ID", s(), "Resume an event stream after this commit.")
            .resp_h(
                "200",
                "The commits.",
                Some(json!({
                    "application/json": { "schema": sref("ChangeFeed") },
                    "application/rdf-patch": text(),
                    "application/rdf-patch+thrift": binary(),
                    "text/event-stream": text(),
                })),
                json!({ "Sparkles-Changes-Next": { "description": "The commit to ask after next.", "schema": { "type": "integer" } } }),
            )
            .errors(&[400, 410, 507])
            .paginated("seq", &["after", "limit"], "next"),
    );
    p.add(
        op(GET, "/{ds}/history", "historyChanges", "History", "History query")
            .doc("The recorded changes of a range of commits, read from the change log, which outlives compactions. Each change is an addition or a removal of a quad with its commit's number, time, kind, author and message. `subject`, `predicate` and `object` take N-Triples terms or bare IRIs and may repeat.")
            .see("history-queries")
            .query("subject", s(), "A subject term; repeat for several.")
            .query("predicate", s(), "A predicate IRI; repeat for several.")
            .query("object", s(), "An object term; repeat for several.")
            .query("graph", s(), "A graph IRI or `default`; repeat for several.")
            .query("from", s(), "A selector of the first commit read; the first by default.")
            .query("to", s(), "A selector of the last commit read; the head by default.")
            .query("op", json!({ "type": "string", "enum": ["add", "remove"] }), "Only additions or only removals.")
            .query("order", json!({ "type": "string", "enum": ["asc", "desc"], "default": "asc" }), "Oldest or newest commits first.")
            .query("limit", json!({ "type": "integer", "minimum": 1, "default": 1000 }), "The most changes listed, at most `--max-rows`.")
            .resp(
                "200",
                "The changes, and the commits whose changes are not recorded.",
                Some(json!({ "application/json": { "schema": sref("HistoryChanges") } })),
            )
            .errors(&[400, 501]),
    );
}

fn search(p: &mut Paths) {
    p.add(
        op(
            GET,
            "/$/text/{ds}",
            "getTextIndex",
            "Search",
            "Full-text index status",
        )
        .see("full-text-search")
        .json("200", "The status, or `{enabled: false}`.", "TextStatus"),
    );
    p.add(
        op(
            PUT,
            "/$/text/{ds}",
            "setTextIndex",
            "Search",
            "Enable or reconfigure the full-text index",
        )
        .see("full-text-search")
        .body(
            false,
            "The configuration; empty for the defaults.",
            json!({ "application/json": { "schema": sref("TextConfig") } }),
        )
        .task()
        .errors(&[400]),
    );
    p.add(
        op(
            DELETE,
            "/$/text/{ds}",
            "deleteTextIndex",
            "Search",
            "Disable and delete the full-text index",
        )
        .see("full-text-search")
        .no_content("Deleted."),
    );
    p.add(
        op(
            POST,
            "/$/text/{ds}/rebuild",
            "rebuildTextIndex",
            "Search",
            "Rebuild the full-text index",
        )
        .see("full-text-search")
        .task()
        .errors(&[400, 409]),
    );
    for (m, id) in [(GET, "textSearch"), (POST, "textSearchPost")] {
        p.add(
            op(m, "/{ds}/text", id, "Search", "Full-text search")
                .see("full-text-search")
                .query_req("q", s(), "The query string.")
                .query("predicate", s(), "Search these predicates (repeatable).")
                .query("lang", s(), "A language tag.")
                .query("graph", s(), "A named graph instead of the default graph.")
                .query("limit", int(), "The most hits.")
                .query("highlight", boolean(), "Mark the matches.")
                .json("200", "The hits, best first.", "TextHits")
                .errors(&[400, 501]),
        );
    }
    p.add(
        op(
            GET,
            "/$/geo/{ds}",
            "getGeoIndex",
            "Search",
            "Spatial index status",
        )
        .see("geosparql")
        .json("200", "The status, or `{enabled: false}`.", "GeoStatus"),
    );
    p.add(
        op(
            PUT,
            "/$/geo/{ds}",
            "setGeoIndex",
            "Search",
            "Enable or reconfigure the spatial index",
        )
        .see("geosparql")
        .body(
            false,
            "The configuration; empty for the defaults.",
            json!({ "application/json": { "schema": sref("GeoConfig") } }),
        )
        .task()
        .errors(&[400, 409]),
    );
    p.add(
        op(
            DELETE,
            "/$/geo/{ds}",
            "deleteGeoIndex",
            "Search",
            "Disable the spatial index",
        )
        .see("geosparql")
        .no_content("Deleted."),
    );
    p.add(
        op(
            POST,
            "/$/geo/{ds}/rebuild",
            "rebuildGeoIndex",
            "Search",
            "Rebuild the spatial index",
        )
        .see("geosparql")
        .task()
        .errors(&[400, 409]),
    );
    p.add(
        op(
            POST,
            "/$/geo/convert",
            "convertGeometries",
            "Search",
            "Convert geometry literals",
        )
        .doc("Converts WKT and GeoJSON literals for the UI's maps. Reads no dataset.")
        .see("hulls-aggregates-jena-filter-functions-utm-and-conversion")
        .json_body(true, "GeoConvertRequest")
        .json("200", "One result per literal.", "GeoConvertResult")
        .errors(&[400, 413]),
    );
    p.add(
        op(
            GET,
            "/{ds}/geo",
            "geoFeatures",
            "Search",
            "Geometries in a box",
        )
        .doc("The indexed geometries that meet a CRS84 box, as GeoJSON.")
        .see("geosparql")
        .query_req("bbox", s(), "`minLon,minLat,maxLon,maxLat`")
        .query("graph", s(), "A graph IRI.")
        .query("predicate", s(), "A geometry predicate.")
        .query("limit", int(), "The most features.")
        .query(
            "tolerance",
            json!({ "type": "number" }),
            "Simplification tolerance in degrees.",
        )
        .resp(
            "200",
            "A GeoJSON feature collection.",
            Some(json!({ "application/geo+json": { "schema": { "type": "object" } } })),
        )
        .errors(&[400]),
    );
    p.add(
        op(
            GET,
            "/$/vector/{ds}",
            "getVectorIndexes",
            "Search",
            "Vector indexes of a dataset",
        )
        .see("vector-indexes")
        .json("200", "The indexes and packed predicates.", "VectorStatus"),
    );
    p.add(
        op(
            GET,
            "/$/vector/{ds}/{name}",
            "getVectorIndex",
            "Search",
            "Get a vector index",
        )
        .see("vector-indexes")
        .json("200", "The index.", "VectorIndexStatus"),
    );
    p.add(
        op(
            PUT,
            "/$/vector/{ds}/{name}",
            "putVectorIndex",
            "Search",
            "Create or replace a vector index",
        )
        .see("vector-indexes")
        .json_body(true, "VectorIndexConfig")
        .json("200", "Replaced.", "VectorIndexCreated")
        .json("201", "Created.", "VectorIndexCreated")
        .errors(&[400, 409]),
    );
    p.add(
        op(
            DELETE,
            "/$/vector/{ds}/{name}",
            "deleteVectorIndex",
            "Search",
            "Drop a vector index",
        )
        .see("vector-indexes")
        .no_content("Dropped."),
    );
    p.add(
        op(
            POST,
            "/$/vector/{ds}/{name}/rebuild",
            "rebuildVectorIndex",
            "Search",
            "Rebuild a vector index",
        )
        .see("vector-indexes")
        .task(),
    );
    p.add(
        op(
            POST,
            "/$/vector/{ds}/{name}/reembed",
            "reembedVectorIndex",
            "Search",
            "Embed an index's texts again",
        )
        .doc("Embeds every selected text of the index again, for example after the model behind a name changed.")
        .see("embeddings-on-write")
        .json("202", "The index's status.", "VectorIndexStatus")
        .errors(&[400, 409]),
    );
    p.add(
        op(
            POST,
            "/$/vector/{ds}/{name}/recall",
            "measureVectorRecall",
            "Search",
            "Measure a vector index's recall",
        )
        .see("vector-indexes")
        .query(
            "samples",
            json!({ "type": "integer", "default": 100 }),
            "Stored vectors used as queries.",
        )
        .query(
            "k",
            json!({ "type": "integer", "default": 10 }),
            "Neighbours per query.",
        )
        .query("ef", int(), "The HNSW search width.")
        .json("200", "Recall@k against the exact search.", "RecallReport"),
    );
}

fn settings(p: &mut Paths) {
    setting(
        p,
        "/$/validation/{ds}",
        "Validation",
        "write-time validation",
        "WriteValidation",
        "ValidationStatus",
        "ValidationConfig",
        "write-time-validation",
    );
    setting(
        p,
        "/$/rdfs/{ds}",
        "Reasoning",
        "RDFS-on-read setting",
        "Rdfs",
        "RdfsStatus",
        "RdfsRequest",
        "rdfs-on-read",
    );
    setting(
        p,
        "/$/describe/{ds}",
        "SPARQL",
        "DESCRIBE setting",
        "Describe",
        "DescribeStatus",
        "DescribeSetting",
        "describe",
    );
    setting(
        p,
        "/$/quota/{ds}",
        "Datasets",
        "storage quota",
        "Quota",
        "DatasetQuota",
        "QuotaRequest",
        "storage-quotas",
    );
    setting(
        p,
        "/$/compaction/{ds}",
        "Datasets",
        "automatic compaction settings",
        "Compaction",
        "CompactionStatus",
        "CompactionPolicy",
        "automatic-compaction",
    );
}

fn format_and_mcp(p: &mut Paths) {
    let options = [
        "lineWidth",
        "indentWidth",
        "typeShorthand",
        "compactIris",
        "quoteStyle",
        "operatorPosition",
        "alignValues",
        "prunePrefixes",
        "directiveStyle",
        "turtleLayout",
        "sort",
    ];
    let mut o = op(POST, "/$/format", "format", "Formatting", "Format SPARQL or RDF")
        .doc("Formats a SPARQL query or update, or a Turtle, TriG, N-Triples, N-Quads or JSON-LD document. A JSON body gets a `FormatResult`. A raw body is answered with the formatted text in the same media type. `--format-endpoint` may limit or turn off the route.")
        .see("formatting")
        .query("language", sref("FormatLanguage"), "The language of a raw body when its media type does not say.");
    for name in options {
        o = o.query(name, s(), "A formatter option; see `FormatOptions`.");
    }
    let raw = [
        "application/sparql-query",
        "application/sparql-update",
        "text/turtle",
        "application/trig",
        "application/n-triples",
        "application/n-quads",
        "application/ld+json",
        "text/plain",
    ];
    let mut body = Map::new();
    body.insert(
        "application/json".into(),
        json!({ "schema": sref("FormatRequest") }),
    );
    let mut answer = Map::new();
    answer.insert(
        "application/json".into(),
        json!({ "schema": sref("FormatResult") }),
    );
    for m in raw {
        body.insert(m.into(), text());
        answer.insert(m.into(), text());
    }
    p.add(
        o.query("prefixGroup", s(), "One prefix group, comma-separated (repeatable).")
            .body(true, "The text, as a JSON `FormatRequest` or a raw body.", J::Object(body))
            .resp_h(
                "200",
                "The formatted text.",
                Some(J::Object(answer)),
                json!({ "Sparkles-Format-Changed": { "description": "Raw bodies: whether the text changed.", "schema": { "type": "boolean" } } }),
            )
            .errors(&[400, 408, 413, 415, 422]),
    );
    p.add(
        op(POST, "/$/lint", "lint", "Formatting", "Lint SPARQL, Turtle or TriG")
            .doc("Lints a SPARQL query or update, or a Turtle or TriG document, with the rules of `sparkles lint`. A syntax error is one of the findings, so the answer is `200` either way. `--format-endpoint` may limit or turn off the route.")
            .see("linting")
            .body(
                true,
                "A `LintRequest`: the text, its language, the rule levels and whether to apply the safe fixes.",
                json!({ "application/json": { "schema": { "type": "object", "required": ["text"] } } }),
            )
            .resp(
                "200",
                "A `LintResult`: the findings, and with `fix` the fixed text.",
                Some(json!({ "application/json": { "schema": { "type": "object" } } })),
            )
            .errors(&[400, 413, 415]),
    );
    let mcp = |m: Method, id: &str, summary: &str, d: &str| {
        op(m, "/$/mcp", id, "MCP", summary)
            .doc(d)
            .see("http-endpoint-mcp")
            .header("MCP-Protocol-Version", s(), "The protocol revision.")
            .header("Mcp-Session-Id", s(), "The session of a legacy client.")
            .errors(&[400, 413])
    };
    p.add(
        mcp(POST, "mcpPost", "Send an MCP message", "One JSON-RPC message of the Model Context Protocol's Streamable HTTP transport. `404` unless the server runs with `--mcp`.")
            .header("Mcp-Method", s(), "The JSON-RPC method (stateless requests).")
            .header("Mcp-Name", s(), "The tool, resource or prompt name (stateless requests).")
            .body(true, "A JSON-RPC message.", json!({ "application/json": { "schema": { "type": "object" } } }))
            .resp(
                "200",
                "The JSON-RPC response.",
                Some(json!({
                    "application/json": { "schema": { "type": "object" } },
                    "text/event-stream": text(),
                })),
            )
            .resp("202", "A notification or response was accepted.", None),
    );
    p.add(
        mcp(
            GET,
            "mcpStream",
            "Open a session's event stream",
            "The server-to-client stream of a legacy session.",
        )
        .resp(
            "200",
            "The stream.",
            Some(json!({ "text/event-stream": text() })),
        ),
    );
    p.add(
        mcp(
            DELETE,
            "mcpEndSession",
            "End an MCP session",
            "Ends a legacy session.",
        )
        .resp("202", "Ended.", None),
    );
}

fn backups(p: &mut Paths) {
    let tag = "Backup repositories";
    p.add(
        op(GET, "/$/repositories", "listRepositories", tag, "List backup repositories")
            .doc("A `server-admin` sees every repository, a caller with `admin` on a dataset sees names and types, others get an empty list.")
            .see("backup-routes")
            .json("200", "The repositories.", "RepositoryList"),
    );
    p.add(
        op(
            POST,
            "/$/repositories",
            "createRepository",
            tag,
            "Register a backup repository",
        )
        .see("backup-routes")
        .query("verify", boolean(), "`false` skips the connection test.")
        .json_body(true, "RepositoryConfig")
        .json("201", "Registered.", "Repository")
        .errors(&[400, 409]),
    );
    p.add(
        op(
            GET,
            "/$/repositories/{repo}",
            "getRepository",
            tag,
            "Get a backup repository",
        )
        .see("backup-routes")
        .json("200", "The repository.", "Repository"),
    );
    p.add(
        op(
            PUT,
            "/$/repositories/{repo}",
            "updateRepository",
            tag,
            "Change a backup repository",
        )
        .see("backup-routes")
        .json_body(true, "RepositoryConfig")
        .json("200", "The repository.", "Repository")
        .errors(&[400, 409]),
    );
    p.add(
        op(
            DELETE,
            "/$/repositories/{repo}",
            "deleteRepository",
            tag,
            "Unregister a backup repository",
        )
        .doc("Its contents stay.")
        .see("backup-routes")
        .no_content("Unregistered.")
        .errors(&[409]),
    );
    p.add(
        op(
            POST,
            "/$/repositories/{repo}/test",
            "testRepository",
            tag,
            "Test a repository's connection",
        )
        .see("backup-routes")
        .json("200", "The report.", "TestReport"),
    );
    p.add(
        op(
            POST,
            "/$/repositories/{repo}/verify",
            "verifyRepository",
            tag,
            "Verify every backup of a repository",
        )
        .see("backup-routes")
        .body(
            false,
            "",
            json!({ "application/json": { "schema": sref("VerifyRequest") } }),
        )
        .task()
        .errors(&[400, 409]),
    );
    p.add(
        op(
            GET,
            "/$/repositories/{repo}/backups",
            "listRepositoryBackups",
            tag,
            "List a repository's backups",
        )
        .doc("Newest first.")
        .see("backup-routes")
        .query("dataset", s(), "Only this dataset's.")
        .query("datasetId", s(), "Only this dataset id's.")
        .query("policy", s(), "Only this policy's.")
        .query(
            "limit",
            json!({ "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }),
            "The page size.",
        )
        .query("before", s(), "The `next` of the previous page.")
        .json("200", "One page of backups.", "BackupList")
        .paginated("time", &["limit", "before"], "next"),
    );
    p.add(
        op(
            POST,
            "/$/repositories/{repo}/gc",
            "collectRepository",
            tag,
            "Delete unreferenced blobs",
        )
        .see("backup-routes")
        .body(
            false,
            "",
            json!({ "application/json": { "schema": sref("GcRequest") } }),
        )
        .task()
        .errors(&[409]),
    );
    p.add(
        op(
            GET,
            "/$/repositories/{repo}/locks",
            "listRepositoryLocks",
            tag,
            "List a repository's locks",
        )
        .see("locks-and-garbage-collection")
        .json("200", "The locks.", "LockList"),
    );
    p.add(
        op(
            DELETE,
            "/$/repositories/{repo}/locks/{id}",
            "breakRepositoryLock",
            tag,
            "Break a lock",
        )
        .see("locks-and-garbage-collection")
        .no_content("Broken.")
        .errors(&[409]),
    );

    let tag = "Backups";
    p.add(
        op(
            GET,
            "/$/backups/{ds}",
            "listDatasetBackups",
            tag,
            "List a dataset's backups",
        )
        .doc("In every repository, or in `repository` only, newest first.")
        .see("backup-routes")
        .query("repository", s(), "One repository.")
        .json("200", "The backups.", "BackupList"),
    );
    p.add(
        op(POST, "/$/backups/{ds}", "createBackup", tag, "Back up a dataset")
            .doc("A JSON body backs up into a repository. Without one, this is Fuseki's alias of `POST /$/backup/{ds}`, an N-Quads file.")
            .see("backup-routes")
            .body(false, "", json!({ "application/json": { "schema": sref("BackupRequest") } }))
            .task()
            .errors(&[400, 409]),
    );
    p.add(
        op(
            GET,
            "/$/backups/{ds}/{repo}/{backup}",
            "getBackup",
            tag,
            "Get a backup",
        )
        .see("backup-routes")
        .json("200", "The backup.", "Backup"),
    );
    p.add(
        op(
            DELETE,
            "/$/backups/{ds}/{repo}/{backup}",
            "deleteBackup",
            tag,
            "Delete a backup",
        )
        .doc("Deletes the manifest. Its blobs go at the next GC.")
        .see("backup-routes")
        .no_content("Deleted.")
        .errors(&[409]),
    );
    p.add(
        op(
            POST,
            "/$/backups/{ds}/{repo}/{backup}/restore",
            "restoreBackup",
            tag,
            "Restore a backup",
        )
        .doc("Needs `admin` on the target too.")
        .see("restore")
        .json_body(true, "RestoreRequest")
        .task()
        .errors(&[400, 409]),
    );
    p.add(
        op(
            POST,
            "/$/backups/{ds}/{repo}/{backup}/verify",
            "verifyBackup",
            tag,
            "Verify a backup",
        )
        .see("backup-routes")
        .body(
            false,
            "",
            json!({ "application/json": { "schema": sref("VerifyRequest") } }),
        )
        .task(),
    );

    let tag = "Backup policies";
    p.add(
        op(
            GET,
            "/$/backup-policies",
            "listPolicies",
            tag,
            "List backup policies",
        )
        .see("lifecycle-policies")
        .json("200", "The policies.", "PolicyList"),
    );
    p.add(
        op(
            POST,
            "/$/backup-policies",
            "createPolicy",
            tag,
            "Create a backup policy",
        )
        .see("lifecycle-policies")
        .json_body(true, "PolicyConfig")
        .json("201", "Created.", "Policy")
        .errors(&[400, 409]),
    );
    p.add(
        op(
            POST,
            "/$/backup-policies/preview",
            "previewSchedule",
            tag,
            "Preview a schedule",
        )
        .see("backup-routes")
        .json_body(true, "SchedulePreviewRequest")
        .json("200", "The next runs.", "SchedulePreview")
        .errors(&[400]),
    );
    p.add(
        op(
            GET,
            "/$/backup-policies/{policy}",
            "getPolicy",
            tag,
            "Get a backup policy",
        )
        .see("lifecycle-policies")
        .json("200", "The policy.", "Policy"),
    );
    p.add(
        op(
            PUT,
            "/$/backup-policies/{policy}",
            "updatePolicy",
            tag,
            "Change a backup policy",
        )
        .see("lifecycle-policies")
        .json_body(true, "PolicyConfig")
        .json("200", "The policy.", "Policy")
        .errors(&[400, 409]),
    );
    p.add(
        op(
            DELETE,
            "/$/backup-policies/{policy}",
            "deletePolicy",
            tag,
            "Delete a backup policy",
        )
        .see("lifecycle-policies")
        .no_content("Deleted.")
        .errors(&[409]),
    );
    p.add(
        op(
            POST,
            "/$/backup-policies/{policy}/run",
            "runPolicy",
            tag,
            "Run a backup policy now",
        )
        .see("backup-routes")
        .task()
        .errors(&[409]),
    );
    p.add(
        op(
            POST,
            "/$/backup-policies/{policy}/retention",
            "applyRetention",
            tag,
            "Apply a policy's retention now",
        )
        .see("backup-routes")
        .query("dryRun", boolean(), "Delete nothing.")
        .json("200", "What was deleted and kept.", "RetentionResult"),
    );
    p.add(
        op(
            GET,
            "/$/backup-policies/{policy}/runs",
            "listPolicyRuns",
            tag,
            "List a policy's runs",
        )
        .see("backup-routes")
        .query(
            "limit",
            json!({ "type": "integer", "minimum": 1, "maximum": 1000, "default": 50 }),
            "The most runs.",
        )
        .json("200", "The runs, newest first.", "PolicyRuns"),
    );
}

fn auth(p: &mut Paths) {
    let tag = "Authentication";
    p.add(
        op(GET, "/$/auth/config", "authConfig", tag, "How to sign in")
            .doc("The sign-in methods of the UI and the CLI, or `{enabled: false}`.")
            .see("whoami")
            .json("200", "The configuration.", "AuthConfig"),
    );
    p.add(
        op(POST, "/$/auth/login", "login", tag, "Sign in to the web UI")
            .doc("Sets the session cookie. A token login needs `session.token_login`.")
            .see("web-ui-sign-in-and-sessions")
            .json_body(true, "LoginRequest")
            .resp_h(
                "204",
                "Signed in.",
                None,
                json!({ "Set-Cookie": { "description": "The session cookie.", "schema": { "type": "string" } } }),
            )
            .errors(&[400, 401, 403, 429]),
    );
    p.add(
        op(POST, "/$/auth/logout", "logout", tag, "Sign out")
            .doc("Ends the session. `redirect` is the provider's or proxy's logout URL, if any.")
            .see("web-ui-sign-in-and-sessions")
            .json("200", "Signed out.", "Logout"),
    );
    p.add(
        op(
            GET,
            "/$/auth/oidc/login",
            "oidcLogin",
            tag,
            "Start an OIDC sign-in",
        )
        .doc("Redirects to the identity provider (authorization code flow with PKCE).")
        .see("web-ui-sign-in-and-sessions")
        .query("return_to", s(), "The UI path to return to.")
        .resp("302", "To the provider.", None),
    );
    p.add(
        op(GET, "/$/auth/oidc/callback", "oidcCallback", tag, "The OIDC redirect URI")
            .doc("Redeems the code, checks the ID token and admission, and redirects to `return_to` with a session cookie.")
            .see("web-ui-sign-in-and-sessions")
            .query("code", s(), "The authorization code.")
            .query("state", s(), "The login's state.")
            .resp("303", "Signed in, or to the login page with an error.", None),
    );
    p.add(
        op(
            POST,
            "/$/auth/oidc/backchannel-logout",
            "oidcBackchannelLogout",
            tag,
            "OIDC back-channel logout",
        )
        .doc(
            "OpenID Connect Back-Channel Logout 1.0: the provider ends sessions by `sid` or `sub`.",
        )
        .see("web-ui-sign-in-and-sessions")
        .body(
            true,
            "The logout token.",
            json!({ "application/x-www-form-urlencoded": { "schema": {
                    "type": "object", "required": ["logout_token"],
                    "properties": { "logout_token": { "type": "string" } },
                } } }),
        )
        .resp("200", "Done.", None)
        .json("400", "An invalid token.", "OAuthError"),
    );
    let tag = "Tokens";
    p.add(
        op(GET, "/$/auth/tokens", "listTokens", tag, "List API tokens")
            .doc("The caller's own tokens. `all=true` (server-admin) adds everyone's and the static ones.")
            .see("api-tokens")
            .query("all", boolean(), "Everyone's tokens.")
            .json("200", "The tokens.", "TokenList"),
    );
    p.add(
        op(POST, "/$/auth/tokens", "createToken", tag, "Mint an API token")
            .doc("By default a token gets all of the minter's access and the default lifetime. The secret appears only in this response.")
            .see("api-tokens")
            .json_body(false, "TokenRequest")
            .json("201", "Minted.", "TokenCreated")
            .errors(&[400]),
    );
    p.add(
        op(
            DELETE,
            "/$/auth/tokens",
            "revokeOwnerTokens",
            tag,
            "Revoke every token of an owner",
        )
        .see("api-tokens")
        .query_req("owner", s(), "The owner, such as `oidc:alice@example.org`.")
        .json("200", "How many were revoked.", "Revoked"),
    );
    p.add(
        op(
            DELETE,
            "/$/auth/tokens/{id}",
            "revokeToken",
            tag,
            "Revoke a token",
        )
        .doc("`self` names the token in use. Tokens minted by it die with it.")
        .see("api-tokens")
        .no_content("Revoked."),
    );
    let tag = "Authentication";
    p.add(
        op(
            POST,
            "/$/auth/device",
            "deviceAuthorization",
            tag,
            "Start a device login",
        )
        .doc("RFC 8628 device authorization for `sparkles auth login --device`.")
        .see("cli-logins")
        .json("200", "The codes.", "DeviceAuthorization")
        .errors(&[429, 503]),
    );
    p.add(
        op(
            GET,
            "/$/auth/device/{user_code}",
            "getDeviceLogin",
            tag,
            "Look up a device login",
        )
        .see("cli-logins")
        .json_inline("200", "The pending login.", json!({ "type": "object" }))
        .errors(&[429]),
    );
    p.add(
        op(POST, "/$/auth/device/{user_code}/approve", "approveDeviceLogin", tag, "Approve a device login")
            .doc("Mints the CLI's token. The body may narrow its scope, as for `POST /$/auth/tokens`.")
            .see("cli-logins")
            .json_body(false, "TokenRequest")
            .json_inline(
                "200",
                "Approved.",
                json!({ "type": "object", "properties": { "approved": { "const": true }, "tokenId": { "type": "string" } } }),
            )
            .errors(&[400, 429]),
    );
    p.add(
        op(
            POST,
            "/$/auth/device/{user_code}/deny",
            "denyDeviceLogin",
            tag,
            "Deny a device login",
        )
        .see("cli-logins")
        .json_inline(
            "200",
            "Denied.",
            json!({ "type": "object", "properties": { "denied": { "const": true } } }),
        )
        .errors(&[429]),
    );
    p.add(
        op(
            POST,
            "/$/auth/cli/authorize",
            "authorizeCli",
            tag,
            "Approve a browser CLI login",
        )
        .doc("Mints the CLI's token and issues the one-time code the browser hands to the CLI's loopback listener. The body may narrow the token's scope, as for `POST /$/auth/tokens`.")
        .see("cli-logins")
        .body(
            true,
            "The CLI's listener and PKCE challenge.",
            json!({ "application/json": { "schema": {
                "allOf": [
                    sref("TokenRequest"),
                    {
                        "type": "object",
                        "required": ["port", "state", "code_challenge"],
                        "properties": {
                            "port": { "type": "integer", "minimum": 1024, "maximum": 65535 },
                            "state": { "type": "string", "maxLength": 256 },
                            "code_challenge": { "type": "string", "description": "The S256 challenge, 43 base64url characters." },
                            "label": { "type": "string" },
                            "hostname": { "type": "string" },
                        },
                    },
                ],
            } } }),
        )
        .json_inline(
            "200",
            "Where to send the browser.",
            json!({ "type": "object", "properties": { "redirect": { "type": "string" } } }),
        )
        .errors(&[400]),
    );
    p.add(
        op(POST, "/$/auth/token", "tokenGrant", tag, "Redeem a CLI login")
            .doc("The token endpoint of the CLI logins: a device code grant or an authorization code grant with PKCE.")
            .see("cli-logins")
            .body(
                true,
                "The grant.",
                json!({
                    "application/x-www-form-urlencoded": { "schema": sref("TokenGrant") },
                    "application/json": { "schema": sref("TokenGrant") },
                }),
            )
            .json("200", "The token.", "TokenGrantResponse")
            .json("400", "Pending, slowed down, denied, expired or invalid.", "OAuthError")
            .errors(&[429]),
    );
}

fn protocol(p: &mut Paths) {
    // the dataset URL: query, update or Graph Store, by the request
    p.add(
        query_get("/{ds}", "datasetGet")
            .doc("A query with `query=`, an update with `update=` is refused (`405`), and any other GET reads the whole dataset or a graph with the Graph Store Protocol (`graph`, `default`).")
            .params(&["gspGraph", "gspDefault"])
            .errors(&[405]),
    );
    p.add(
        op(
            HEAD,
            "/{ds}",
            "datasetHead",
            "Graph Store",
            "Read the dataset's headers",
        )
        .see("per-dataset-sparql-protocol-fuseki-compatible")
        .params(&["gspGraph", "gspDefault", "at", "ifNoneMatch"])
        .resp("200", "The headers of the read.", None)
        .resp("304", "Not modified.", None),
    );
    let mut post_body = Map::new();
    post_body.insert("application/sparql-query".into(), text());
    post_body.insert("application/sparql-update".into(), text());
    post_body.insert(
        "application/x-www-form-urlencoded".into(),
        json!({ "schema": { "anyOf": [query_form(), update_form()] } }),
    );
    if let J::Object(m) = rdf_in() {
        post_body.extend(m);
    }
    post_body.insert("application/rdf-patch".into(), text());
    post_body.insert("application/rdf-patch+thrift".into(), binary());
    let o = op(POST, "/{ds}", "datasetPost", "SPARQL", "Query, update or add data")
        .doc("A query (`application/sparql-query`, or a form with `query`), an update (`application/sparql-update`, or a form with `update`), an RDF Patch (`application/rdf-patch` or `application/rdf-patch+thrift`, as `/{ds}/patch`), or else a Graph Store POST of the RDF body. A form with neither is refused.")
        .see("per-dataset-sparql-protocol-fuseki-compatible")
        .params(&["gspGraph", "gspDefault"])
        .body(true, "A query, an update or RDF data.", J::Object(post_body));
    let o = query_params(write_params(o));
    p.add(
        o.resp_h(
            "200",
            "The results, the update's statistics or the write's count.",
            Some({
                let mut m = match query_results() {
                    J::Object(m) => m,
                    _ => unreachable!(),
                };
                m.insert(
                    "application/json".into(),
                    json!({ "schema": { "anyOf": [sref("UpdateResult"), sref("WriteCount"), sref("PatchResult"), sref("Receipt"), sref("DryRunReport")] } }),
                );
                J::Object(m)
            }),
            commit_headers(),
        )
        .resp("201", "A graph was created.", None)
        .errors(&[400, 408, 412, 413, 415, 422, 503, 507]),
    );
    for (m, id, summary) in [
        (PUT, "datasetPut", "Replace a graph or the dataset"),
        (DELETE, "datasetDelete", "Delete a graph"),
    ] {
        let mut o = op(m.clone(), "/{ds}", id, "Graph Store", summary)
            .doc("The Graph Store Protocol on the dataset URL.")
            .see("per-dataset-sparql-protocol-fuseki-compatible")
            .params(&["gspGraph", "gspDefault", "ifMatch", "ifNoneMatch"]);
        if m == PUT {
            o = o.body_ref("RdfData");
        }
        let o = write_ok(o, "200")
            .resp("201", "Created.", None)
            .resp("204", "Deleted.", None);
        p.add(write_errors(write_params(o)));
    }
    p.add(query_get("/{ds}/sparql", "sparqlQueryGet"));
    p.add(query_post("/{ds}/sparql", "sparqlQueryPost"));
    p.add(query_get("/{ds}/query", "queryGet"));
    p.add(query_post("/{ds}/query", "queryPost"));
    let o = op(POST, "/{ds}/update", "update", "SPARQL", "Run a SPARQL update")
        .doc("The SPARQL 1.1 Update protocol, with an `application/sparql-update` body or a form. `using-graph-uri` and `using-named-graph-uri` are the `USING` and `USING NAMED` of every operation.")
        .see("per-dataset-sparql-protocol-fuseki-compatible")
        .query("using-graph-uri", json!({ "type": "array", "items": { "type": "string" } }), "`USING` graphs.")
        .query("using-named-graph-uri", json!({ "type": "array", "items": { "type": "string" } }), "`USING NAMED` graphs.")
        .params(&["memoryMb", "maxRows", "maxRowsProduced"])
        .body(
            true,
            "The update.",
            json!({
                "application/sparql-update": { "schema": { "type": "string" } },
                "application/x-www-form-urlencoded": { "schema": update_form() },
            }),
        );
    p.add(
        write_params(o)
            .resp_h(
                "200",
                "The update's statistics, with a receipt or a dry run's report when asked.",
                Some(json!({
                    "application/json": { "schema": { "anyOf": [sref("UpdateResult"), sref("Receipt"), sref("DryRunReport")] } },
                    "application/x-sparkles+json": { "schema": sref("UpdateResult") },
                    "application/rdf-patch": text(),
                })),
                commit_headers(),
            )
            .errors(&[400, 405, 408, 412, 413, 415, 422, 503, 507]),
    );
    graph_store(
        p,
        "/{ds}/data",
        "gsp",
        &[GET, HEAD, PUT, POST, DELETE],
        false,
    );
    graph_store(p, "/{ds}/get", "gspRead", &[GET, HEAD], false);
    let o = op(POST, "/{ds}/upload", "upload", "Graph Store", "Upload files")
        .doc("A multipart upload of RDF files, and CSV or TSV tables mapped to triples, in one commit. The format comes from each file name or content type. A plain `text/csv` or `text/tab-separated-values` body is one table.")
        .see("csv-and-tsv-uploads")
        .query("graph", s(), "The target graph.")
        .query("base", s(), "The default mapping's namespace for tables.")
        .query("key", s(), "The column that names each row in the default mapping.")
        .body(
            true,
            "The files.",
            json!({
                "multipart/form-data": { "schema": {
                    "type": "object",
                    "additionalProperties": true,
                    "properties": {
                        "file": { "type": "array", "items": { "type": "string", "contentMediaType": "application/octet-stream" } },
                        "graph": { "type": "string" },
                        "mapping": { "type": "string", "description": "A CSVW metadata document." },
                        "template": { "type": "string", "description": "A SPARQL CONSTRUCT template for tables." },
                    },
                } },
                "text/csv": text(),
                "text/tab-separated-values": text(),
            }),
        );
    p.add(write_errors(write_params(write_ok(o, "200"))));
    for (m, id) in [(POST, "patchPost"), (PATCH, "patch")] {
        let o = op(m, "/{ds}/patch", id, "Graph Store", "Apply an RDF Patch")
            .doc("Applies an RDF Patch as one write transaction, as Fuseki's `patch` operation does. The text form is `application/rdf-patch`, which a missing content type or `application/x-www-form-urlencoded` also means, and the binary form is `application/rdf-patch+thrift`. `TX`, `TC` and `Z` are markers, and `TA` aborts the whole patch with `200` and `aborted: true`. `PA` and `PD` change the dataset's prefixes. A `prev` header that names a commit of this dataset applies the patch only when that commit is the head, and is `412` otherwise. The body is limited by `--max-upload-mb`. A patch that changes data makes one commit of kind `patch`. The other methods are `405`.")
            .see("applying-rdf-patch")
            .body(
                true,
                "The patch.",
                json!({
                    "application/rdf-patch": text(),
                    "application/rdf-patch+thrift": binary(),
                }),
            );
        p.add(
            write_params(o)
                .resp_h(
                    "200",
                    "What the patch did, with a receipt or a dry run's report when asked.",
                    Some(json!({
                        "application/json": { "schema": { "anyOf": [sref("PatchResult"), sref("DryRunReport")] } },
                        "application/x-sparkles+json": { "schema": sref("PatchResult") },
                    })),
                    commit_headers(),
                )
                .errors(&[400, 403, 405, 408, 412, 413, 415, 422, 503, 507]),
        );
    }
    for (m, id) in [(GET, "explainGet"), (POST, "explainPost")] {
        let mut o = op(m.clone(), "/{ds}/explain", id, "SPARQL", "Explain a query")
            .doc("The algebra and the plan, without running the query.")
            .see("explain")
            .params(&["at", "reasoning", "nocache"]);
        o = if m == GET {
            o.query_req("query", s(), "The query.")
        } else {
            o.body_ref("SparqlQuery")
        };
        p.add(o.json("200", "The plan.", "Explain").errors(&[400]));
    }
    let mut shapes = Map::new();
    media(&RDF[..6], &mut shapes);
    shapes.insert("text/shaclc".into(), text());
    let mut report = Map::new();
    media(&RDF[..6], &mut report);
    report.insert(
        "application/json".into(),
        json!({ "schema": { "type": "object" } }),
    );
    p.add(
        op(POST, "/{ds}/shacl", "shacl", "Validation", "Validate with SHACL")
            .doc("Validates a data graph of the dataset against the shapes graph in the body, as Fuseki's `/{ds}/shacl` does. The report answers `200` whether or not the data conforms.")
            .see("shacl-validation")
            .param("graphSel")
            .query("target", s(), "Validate one node, an IRI or prefixed name.")
            .params(&["reasoning", "timeout", "at", "format"])
            .body(true, "The shapes graph.", J::Object(shapes))
            .resp("200", "The validation report.", Some(J::Object(report)))
            .errors(&[400, 408, 507]),
    );
    p.add(
        op(POST, "/{ds}/shex", "shex", "Validation", "Validate with ShEx")
            .doc("Validates nodes against a ShEx 2.1 schema: the schema as the body with the shape map in the query, or a JSON envelope with both.")
            .see("shex-validation")
            .query("map", s(), "A compact shape map.")
            .query("node", s(), "A node to validate, with `shape`.")
            .query("shape", s(), "A shape label; `START` by default.")
            .query("schema-format", json!({ "type": "string", "enum": ["shexc", "shexj", "shexr"] }), "The schema's syntax when the media type does not say.")
            .query("base", s(), "The base IRI of the schema.")
            .params(&["graphSel", "reasoning", "timeout", "format"])
            .body(
                true,
                "The schema, or a JSON envelope.",
                json!({
                    "text/shex": text(),
                    "application/shex+json": { "schema": { "type": "object" } },
                    "application/json": { "schema": { "anyOf": [sref("ShexRequest"), { "type": "object", "description": "A ShExJ schema." }] } },
                    "text/turtle": text(),
                }),
            )
            .resp(
                "200",
                "The result.",
                Some(json!({
                    "application/json": { "schema": sref("ShexReport") },
                    "text/turtle": text(),
                })),
            )
            .errors(&[400, 408, 501, 507]),
    );
    p.add(
        op(GET, "/{ds}/prefixes", "getPrefixes", "Datasets", "Read prefixes")
            .doc("Fuseki's prefixes service: `prefix=p` returns one binding (`404` if unbound), `uri=u` the prefixes of an IRI, and neither all of them.")
            .see("datasets-admin")
            .query("prefix", s(), "A prefix name.")
            .query("uri", s(), "A namespace IRI.")
            .json_inline(
                "200",
                "The binding or bindings.",
                json!({ "anyOf": [sref("PrefixBinding"), sref("Prefixes"), { "type": "object" }] }),
            ),
    );
    for (m, id) in [(POST, "addPrefix"), (PUT, "putPrefix")] {
        p.add(
            op(m, "/{ds}/prefixes", id, "Datasets", "Bind a prefix")
                .doc("Binds `prefix` to `uri`, given in the query, a form or a JSON body.")
                .see("datasets-admin")
                .query("prefix", s(), "The prefix name.")
                .query("uri", s(), "The namespace IRI.")
                .body(
                    false,
                    "The binding.",
                    json!({
                        "application/json": { "schema": sref("PrefixBinding") },
                        "application/x-www-form-urlencoded": { "schema": sref("PrefixBinding") },
                    }),
                )
                .json("200", "The binding.", "PrefixBinding")
                .errors(&[400]),
        );
    }
    p.add(
        op(
            DELETE,
            "/{ds}/prefixes",
            "deletePrefix",
            "Datasets",
            "Remove a prefix",
        )
        .see("datasets-admin")
        .query_req("prefix", s(), "The prefix name.")
        .no_content("Removed."),
    );
    graph_store(
        p,
        "/{ds}/{*graph}",
        "directGraph",
        &[GET, HEAD, PUT, POST, DELETE],
        true,
    );
}

fn openapi(p: &mut Paths) {
    p.add(
        op(
            GET,
            "/$/openapi.json",
            "openapiJson",
            "OpenAPI",
            "This description as JSON",
        )
        .see("openapi-description")
        .param("ifNoneMatch")
        .resp(
            "200",
            "The OpenAPI 3.1 document.",
            Some(json!({ "application/json": { "schema": { "type": "object" } } })),
        )
        .resp("304", "Not modified.", None),
    );
    p.add(
        op(
            GET,
            "/$/openapi.yaml",
            "openapiYaml",
            "OpenAPI",
            "This description as YAML",
        )
        .see("openapi-description")
        .param("ifNoneMatch")
        .resp(
            "200",
            "The OpenAPI 3.1 document.",
            Some(json!({ "application/yaml": text() })),
        )
        .resp("304", "Not modified.", None),
    );
}
