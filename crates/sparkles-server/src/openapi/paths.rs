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

fn str_enum(values: &[&str]) -> J {
    json!({ "type": "string", "enum": values })
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
        "Sparkles-Branch": { "description": "The branch, when it is not `main`.", "schema": { "type": "string" } },
        "Sparkles-Branch-Id": { "description": "The branch's id, when it is not `main`.", "schema": { "type": "string" } },
    })
}

/// The parameters of a SPARQL query beyond `query`.
fn query_params(o: Op) -> Op {
    o.params(&[
        "defaultGraphUri",
        "namedGraphUri",
        "format",
        "timeout",
        "queryReasoning",
        "reasoningProfile",
        "maxDerived",
        "at",
        "branch",
        "send",
        "execution",
        "profile",
        "nocache",
        "memoryMb",
        "spillMb",
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
        .errors(&[400, 408, 410, 413, 501, 503, 507])
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
    analysis_params(o.params(&[
        "branch",
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
    .query("entailments", boolean(), "Include complete candidate entailment changes in a JSON dry run. Requires dryRun=true."))
}

/// Program and memory controls shared by read-only entailment analyses.
fn analysis_params(o: Op) -> Op {
    o.query(
        "profile",
        s(),
        "The shared reasoning profile. Omit it to use the recorded program.",
    )
    .query("rules", s(), "Jena rule text for the rules profile.")
    .query(
        "inputs",
        s(),
        "JSON AnalysisInputs with dataGraphs, ontologyGraphs, imports and locationMapping.",
    )
    .query(
        "vocabularies",
        s(),
        "A JSON array of built-in vocabulary names.",
    )
    .query(
        "geoDefaultGeometry",
        boolean(),
        "Include GeoSPARQL default geometry inference.",
    )
    .query(
        "maxMemory",
        int(),
        "The analysis memory allowance in bytes, capped at the server budget.",
    )
    .query(
        "maxInferred",
        int(),
        "The most inferred triples, capped at the server reasoning limit.",
    )
    .query("threads", int(), "HTTP uses the server reasoning pool.")
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
    let mut rdf_and_tables = rdf_in();
    if let Some(m) = rdf_and_tables.as_object_mut() {
        m.insert("text/csv".into(), text());
        m.insert("text/tab-separated-values".into(), text());
    }
    m.insert(
        "RdfData".into(),
        json!({
            "required": true,
            "description": "RDF data in the syntax its `Content-Type` names. Jena's N3 media types are read as Turtle. A `text/csv` or `text/tab-separated-values` body is a table that `mapping` or `base` maps into the graph.",
            "content": rdf_and_tables,
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

/// The parameters of a Graph Store write whose body is a CSV or TSV table (F19 §2.3).
fn table_params(o: Op) -> Op {
    o.query(
        "mapping",
        s(),
        "A CSV or TSV body: the stored mapping that maps it.",
    )
    .query(
        "base",
        s(),
        "A CSV or TSV body: the default mapping's namespace.",
    )
    .query(
        "key",
        s(),
        "A CSV or TSV body: the column that names each row in the default mapping.",
    )
    .query(
        "source",
        s(),
        "A CSV or TSV body: the file name the mapping reads it as, when the mapping reads several.",
    )
    .query(
        "csvwMode",
        str_enum(&["minimal", "standard"]),
        "A CSVW mapping: standard mode adds the table group, table and row descriptions.",
    )
    .query(
        "csvwCheck",
        str_enum(&["keys", "none"]),
        "A CSVW mapping: check primary and foreign keys (the default in standard mode).",
    )
    .query(
        "lookup",
        boolean(),
        "A CONSTRUCT template: read the dataset's head while mapping.",
    )
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
                let o = table_params(o);
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
                let o = table_params(o);
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
                .doc("Deletes the graph. With a `text/csv` or `text/tab-separated-values` body and `mapping` or `base`, removes the mapped triples from the graph instead.")
                .see("per-dataset-sparql-protocol-fuseki-compatible")
                .params(&["ifMatch", "ifNoneMatch"])
                .body(
                    false,
                    "A table whose mapped triples are removed.",
                    json!({ "text/csv": text(), "text/tab-separated-values": text() }),
                );
                let o = table_params(o);
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
    ontology(p);
    forms(p);
    admin(p);
    queries(p);
    mappings(p);
    algorithms(p);
    graphql(p);
    history(p);
    branches(p);
    search(p);
    settings(p);
    resources(p);
    format_and_mcp(p);
    assist(p);
    backups(p);
    auth(p);
    protocol(p);
    openapi(p);
}

/// The resource catalog of spec C30.
fn resources(p: &mut Paths) {
    let tag = "Catalog";
    let rdf_or_json = || {
        let mut m = Map::new();
        m.insert(
            "application/json".into(),
            json!({ "schema": sref("ResourceList") }),
        );
        media(RDF, &mut m);
        J::Object(m)
    };
    let format = json!({ "type": "string", "enum": ["json", "turtle", "ntriples", "nquads", "trig", "rdfxml", "jsonld"] });
    let etag = json!({ "ETag": { "description": "The resource's entity tag.", "schema": s() } });
    p.add(
        op(GET, "/$/resources", "listResources", tag, "List the resource catalog")
            .doc("Every source, vocabulary and dataset resource of the server with its versions, as JSON, or the catalog in DCAT 3 with PROV-O, SPDX, VANN and VoID on `Accept` or `format=`. Needs server `admin`.")
            .see("resource-catalog")
            .query("format", format.clone(), "`json`, or the RDF syntax of the description. `Accept` decides when it is left out.")
            .resp("200", "The catalog.", Some(rdf_or_json()))
            .errors(&[400, 503]),
    );
    p.add(
        op(POST, "/$/resources", "createResource", tag, "Add a resource")
            .doc("Adds a resource with any initial versions, which may reference earlier ones in the list. A `mirrored` or `synced` ownership policy is refused, since this build keeps owned resources. Needs server `admin`.")
            .see("resource-catalog")
            .json_body(true, "ResourceCreate")
            .resp_h("201", "The resource, with `Location`.", Some(json!({ "application/json": { "schema": sref("Resource") } })), json!({
                "ETag": { "description": "The resource's entity tag.", "schema": s() },
                "Location": { "description": "The resource's route.", "schema": s() },
            }))
            .errors(&[400, 403, 404, 409, 503]),
    );
    p.add(
        op(GET, "/$/resources/{id}", "getResource", tag, "Get a resource")
            .doc("The resource with its `ETag`, or its description in DCAT on `Accept` or `format=`. Needs server `admin`.")
            .see("resource-catalog")
            .query("format", format, "`json`, or the RDF syntax of the description.")
            .resp_h("200", "The resource.", Some({
                let mut m = Map::new();
                m.insert("application/json".into(), json!({ "schema": sref("Resource") }));
                media(RDF, &mut m);
                J::Object(m)
            }), etag.clone())
            .errors(&[400, 404, 503]),
    );
    p.add(
        op(PUT, "/$/resources/{id}", "replaceResource", tag, "Replace a resource's description")
            .doc("Replaces the descriptive members. The kind stays, and `versions` is refused, since versions are added and removed on their own routes. A change of the licences withdraws the acceptance of restricted terms. Needs server `admin`.")
            .see("resource-catalog")
            .param("ifMatch")
            .json_body(true, "ResourceUpdate")
            .resp_h("200", "The resource.", Some(json!({ "application/json": { "schema": sref("Resource") } })), etag.clone())
            .errors(&[400, 403, 404, 409, 412, 503]),
    );
    p.add(
        op(DELETE, "/$/resources/{id}", "deleteResource", tag, "Remove a resource")
            .doc("Removes a resource that no version of another resource references (`409 resource-in-use` otherwise). Needs server `admin`.")
            .see("resource-catalog")
            .param("ifMatch")
            .no_content("Removed.")
            .errors(&[403, 404, 409, 412, 503]),
    );
    p.add(
        op(POST, "/$/resources/{id}/versions", "addResourceVersion", tag, "Add a version")
            .doc("Adds a version, which never changes afterwards. Its imports and graph sources pin versions that exist. Needs server `admin`.")
            .see("resource-catalog")
            .json_body(true, "ResourceVersionInput")
            .resp_h("201", "The resource with the new version.", Some(json!({ "application/json": { "schema": sref("Resource") } })), etag.clone())
            .errors(&[400, 403, 404, 409, 503]),
    );
    p.add(
        op(DELETE, "/$/resources/{id}/versions/{version}", "deleteResourceVersion", tag, "Remove a version")
            .doc("Removes a version that nothing references (`409 resource-in-use` otherwise). Needs server `admin`.")
            .see("resource-catalog")
            .no_content("Removed.")
            .errors(&[403, 404, 409, 503]),
    );
    p.add(
        op(POST, "/$/resources/{id}/acceptance", "acceptResourceTerms", tag, "Accept restricted terms")
            .doc("Records the caller's acceptance of the resource's restricted terms, with the licences it accepts. A resource without restricted terms is `409 not-restricted`. Needs server `admin`.")
            .see("resource-licences")
            .resp_h("200", "The resource.", Some(json!({ "application/json": { "schema": sref("Resource") } })), etag.clone())
            .errors(&[403, 404, 409, 503]),
    );
    p.add(
        op(
            DELETE,
            "/$/resources/{id}/acceptance",
            "withdrawResourceTerms",
            tag,
            "Withdraw an acceptance",
        )
        .doc("Withdraws the acceptance of the resource's terms. Needs server `admin`.")
        .see("resource-licences")
        .resp_h(
            "200",
            "The resource.",
            Some(json!({ "application/json": { "schema": sref("Resource") } })),
            etag,
        )
        .errors(&[403, 404, 503]),
    );
    p.add(
        op(GET, "/$/resources/{id}/license", "getResourceLicense", tag, "Get a version's licence obligations")
            .doc("The licences that apply to a version, propagated through its imports and its graphs' sources, the class of each graph, whether the data may be redistributed and the verdict of the licence gate under `serve --license-policy`. Needs server `admin`.")
            .see("resource-licences")
            .query("version", s(), "The version; the current version by default.")
            .json("200", "The report.", "LicenseReport")
            .errors(&[404, 503]),
    );
    p.add(
        op(GET, "/$/resources/{id}/notice", "getResourceNotice", tag, "Get a version's NOTICE")
            .doc("The NOTICE of a version as plain text, with each licence, its attribution, its page and the graphs it covers. Needs server `admin`.")
            .see("resource-licences")
            .query("version", s(), "The version; the current version by default.")
            .resp("200", "The NOTICE.", Some(json!({ "text/plain": { "schema": s() } })))
            .errors(&[404, 503]),
    );
    p.add(
        op(GET, "/$/resources/{id}/header", "getVocabularyHeader", tag, "Read a vocabulary's header")
            .doc("Reads the `owl:Ontology` or `skos:ConceptScheme` header of each graph that holds the vocabulary, under the caller's grants and the server's query timeout and budgets, and lists where it disagrees with the current version's record. Changes nothing. Needs server `admin`.")
            .see("vocabulary-resources")
            .json("200", "The headers.", "VocabularyHeader")
            .errors(&[404, 409, 503]),
    );
    p.add(
        op(POST, "/$/resources/{id}/install/{ds}", "installVocabularyPrefixes", tag, "Install a vocabulary's prefixes")
            .doc("Binds the namespaces of the vocabulary and of the vocabularies its current version imports as prefixes of the dataset in one settings write, which honours the operator's locks and the prefix limit. A prefix the dataset binds to another IRI is `409 prefix-conflict` unless `replace=true`. Needs `admin` on the dataset.")
            .see("vocabulary-resources")
            .query("replace", boolean(), "Rebind prefixes that the dataset binds to other IRIs.")
            .json("200", "What the write bound.", "PrefixInstall")
            .errors(&[400, 403, 404, 409, 503]),
    );
}

/// Editing forms generated from SHACL shapes (U02).
fn ontology(p: &mut Paths) {
    let sel = |o: Op| {
        o.query(
            "graph",
            s(),
            "A graph read: an IRI or `default` (repeatable). The default graph when none is named.",
        )
        .query(
            "imports",
            boolean(),
            "Follow `owl:imports` to graphs of the dataset (the default).",
        )
        .query(
            "mode",
            json!({ "type": "string", "enum": ["lenient", "strict"] }),
            "How the reverse mapping reads the graphs: `lenient` (the default) types undeclared entities from their use, `strict` keeps every triple outside the mapping unmapped.",
        )
    };
    let read = |o: Op| sel(o.param("branch"));
    let object = json!({ "application/json": { "schema": { "type": "object" } } });
    let edited = json!({
        "type": "array",
        "description": "The axioms the edit wrote or removed, at most 10,000, each with its `type`, its functional-style form `ofn` and its JSON form `axiom`, so that a client can undo the edit.",
        "items": { "type": "object", "properties": {
            "type": { "type": "string" }, "ofn": { "type": "string" }, "axiom": { "type": "object" }
        } }
    });
    let selection = json!({
        "graph": { "description": "A graph IRI or `default`, or a list of them." },
        "imports": { "type": "boolean" },
        "mode": { "type": "string", "enum": ["lenient", "strict"] },
    });
    p.add(
        read(op(GET, "/$/ontology/{ds}", "getOntology", "Ontology", "Ontology summary"))
            .doc("The ontology header, the counts of declared entities by kind and of axioms by type in the terminology, the graphs read with the imports that resolved, the triples read, the unmapped triples and the diagnostics of the read. The entity tag names the commit read, and an edit's `If-Match` can name it.")
            .see("ontology")
            .param("at")
            .query("inferences", boolean(), "Also read the materialized inferences while the dataset has them, so that the axioms include the materialized closure. The answer's `inferences` says whether they were read and whether they are stale.")
            .param("ifNoneMatch")
            .resp("200", "The summary.", Some(object.clone()))
            .resp("304", "The summary is still at the tagged commit.", None)
            .errors(&[400, 404, 408]),
    );
    p.add(
        read(op(GET, "/$/ontology/{ds}/axioms", "getOntologyAxioms", "Ontology", "Ontology axioms"))
            .doc("The axioms of the ontology with a type in `type` and mentioning `about`, one page at a time, in functional-style syntax and in their JSON form, or as one functional-style document with `format=ofn`. The assertions the store holds are listed with `assertions=true`. `about` an IRI that the ontology neither declares nor uses is `404`.")
            .see("ontology")
            .param("at")
            .query("inferences", boolean(), "Also read the materialized inferences while the dataset has them, so that the axioms include the materialized closure. The answer's `inferences` says whether they were read and whether they are stale.")
            .query("type", s(), "Axiom types, comma separated, such as `SubClassOf,EquivalentClasses`.")
            .query("about", s(), "An entity IRI the axioms mention.")
            .query("assertions", boolean(), "Also list the assertions the store holds.")
            .query("format", json!({ "type": "string", "enum": ["json", "ofn"] }), "`json` (the default) or `ofn`.")
            .query("limit", int(), "The most axioms listed, from 1 to 10,000 (100 by default).")
            .query("offset", int(), "The axioms skipped.")
            .resp(
                "200",
                "One page of axioms.",
                Some(json!({
                    "application/json": { "schema": { "type": "object" } },
                    "text/owl-functional": { "schema": { "type": "string" } }
                })),
            )
            .errors(&[400, 404, 408]),
    );
    p.add(
        read(op(GET, "/$/ontology/{ds}/entities", "listOntologyEntities", "Ontology", "Ontology entities"))
            .doc("The entities the ontology declares or, in a lenient read, types by their use, with their kinds, in IRI order and one page at a time.")
            .see("ontology")
            .param("at")
            .query("inferences", boolean(), "Also read the materialized inferences while the dataset has them.")
            .query("kind", s(), "Entity kinds, comma separated, such as `Class,ObjectProperty` (all kinds by default).")
            .query("limit", int(), "The most entities listed, from 1 to 10,000 (100 by default).")
            .query("offset", int(), "The entities skipped.")
            .resp("200", "One page of entities.", Some(object.clone()))
            .errors(&[400, 404, 408]),
    );
    p.add(
        read(op(GET, "/$/ontology/{ds}/entity", "getOntologyEntity", "Ontology", "Ontology entity"))
            .doc("What the ontology says about one entity: its kinds, labels and annotations, and for each kind the superclasses, subclasses, equivalent and disjoint classes, keys and instances of a class, the domains, ranges, hierarchy, inverses and characteristics of a property, the definitions of a datatype, and the types, equalities and property assertions of an individual. Each expression comes in functional-style syntax and in its JSON form. With `assertions=true`, the instances of a class and the types and property assertions of an individual also come from the assertions the store holds, at most `limit` of each. An IRI that the ontology neither declares nor uses as an entity, or as the `kind` asked for, is `404`.")
            .see("ontology")
            .param("at")
            .query("inferences", boolean(), "Also read the materialized inferences while the dataset has them, so that the superclasses include the materialized closure.")
            .query("iri", s(), "The entity's IRI.")
            .query("kind", s(), "The one entity kind answered, such as `Class` (every kind the IRI has by default).")
            .query("assertions", boolean(), "Also read the instances, types and property assertions the store holds.")
            .query("limit", int(), "The most entries of each streamed list, from 1 to 10,000 (100 by default).")
            .resp("200", "The entity.", Some(object.clone()))
            .errors(&[400, 404, 408]),
    );
    p.add(
        op(POST, "/$/ontology/{ds}/check", "checkOntology", "Ontology", "Check OWL 2 profiles")
            .param("branch")
            .doc("One report per profile in `profiles` (`el`, `ql`, `rl`, `dl` or `all`; `dl` by default), over the terminology and, unless `assertions` is false, the assertions the store holds. Each report lists the violations with the rule of the OWL 2 Profiles or the structural specification that each breaks. The EL, QL and RL reports also list the OWL 2 DL violations.")
            .see("ontology")
            .param("timeout")
            .body(
                false,
                "The ontology and the profiles.",
                json!({ "application/json": { "schema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "graph": selection["graph"],
                        "imports": selection["imports"],
                        "mode": selection["mode"],
                        "inferences": { "type": "boolean", "description": "Also read the materialized inferences while the dataset has them." },
                        "profiles": { "type": "array", "items": { "type": "string", "enum": ["el", "ql", "rl", "dl", "all"] } },
                        "assertions": { "type": "boolean", "description": "Check the assertions the store holds (the default)." },
                        "maxViolations": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "The most violations each report lists (1,000 by default)." }
                    }
                } } }),
            )
            .resp("200", "The reports.", Some(object.clone()))
            .errors(&[400, 404, 408]),
    );
    p.add(
        write_params(op(POST, "/$/ontology/{ds}/classify", "classifyOntology", "Ontology", "Classify an OWL 2 EL ontology"))
            .doc("Classifies the terminology with the OWL 2 EL classifier and answers whether it is consistent, the unsatisfiable classes, the axioms outside OWL 2 EL that the classification left out or used in part, and the run's statistics. `about` adds one class's direct and inferred superclasses and subclasses and its equivalent classes, which is `404` for a class the ontology does not mention, and `nodes` adds the taxonomy one page of nodes at a time. The dataset keeps the last classification and reuses it incrementally when the ontology changes. `target` writes the direct taxonomy into that named graph as one commit with PROV-O provenance on its ontology header, which needs write access, and replaces the taxonomy the graph held. A graph read that changed after the classification is `409`. `format=ofn` answers the direct taxonomy in functional-style syntax. A run past the memory budget is `507` and past its deadline `408`.")
            .see("ontology")
            .query("format", json!({ "type": "string", "enum": ["json", "ofn"] }), "`json` (the default) or `ofn`.")
            .body(
                false,
                "The ontology and what to answer.",
                json!({ "application/json": { "schema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "graph": selection["graph"],
                        "imports": selection["imports"],
                        "mode": selection["mode"],
                        "assertions": { "type": "boolean", "description": "Also classify the assertions the store holds, so that they can make the ontology inconsistent. Such a run is not kept." },
                        "target": { "type": "string", "description": "A named graph outside the graphs read that takes the direct taxonomy." },
                        "message": { "type": "string", "description": "The commit message of the write, also kept as a comment on the header." },
                        "about": { "type": "string", "description": "A class IRI to describe." },
                        "nodes": { "type": "boolean", "description": "List the taxonomy's nodes." },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "The most nodes, unsatisfiable classes and excluded axioms listed (100 by default)." },
                        "offset": { "type": "integer", "minimum": 0, "description": "The first node listed." },
                        "threads": { "type": "integer", "minimum": 0, "maximum": 1024, "description": "The worker threads, within the algorithm pool's grant (0 for the grant)." }
                    }
                } } }),
            )
            .resp(
                "200",
                "The classification, with the receipt when a write asked for one.",
                Some(json!({
                    "application/json": { "schema": { "type": "object" } },
                    "text/owl-functional": { "schema": { "type": "string" } }
                })),
            )
            .errors(&[400, 403, 404, 408, 409, 507]),
    );
    p.add(
        op(POST, "/$/ontology/{ds}/parse", "parseManchester", "Ontology", "Read Manchester syntax")
            .param("branch")
            .doc("Reads `text` in OWL 2 Manchester syntax as `as` (`classExpression` by default, or `dataRange`, `individual`, `literal` or `axiom`) and answers its JSON form, its functional-style text and its Manchester rendering. Names are full IRIs in angle brackets, prefixed names with the dataset's prefixes, local names, and labels in single quotes when they hold a space. They resolve against the entities and the `rdfs:label` and `skos:prefLabel` values of the ontology. Text that does not read is `400` with `code` `invalid-manchester`, `reason` (`syntax`, `unknown`, `ambiguous` or `kind`), the span `start` and `end` in UTF-16 code units, and for an ambiguous name the `candidates` with their IRIs, kinds and labels.")
            .see("ontology")
            .body(
                true,
                "The ontology and the text.",
                json!({ "application/json": { "schema": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["text"],
                    "properties": {
                        "graph": selection["graph"],
                        "imports": selection["imports"],
                        "mode": selection["mode"],
                        "text": { "type": "string", "description": "The Manchester text." },
                        "as": { "type": "string", "enum": ["classExpression", "dataRange", "individual", "literal", "axiom"], "description": "What the text is (`classExpression` by default)." }
                    }
                } } }),
            )
            .resp("200", "The value in its JSON form, `ofn` and `manchester`.", Some(object.clone()))
            .errors(&[400, 404, 408]),
    );
    p.add(
        op(POST, "/$/ontology/{ds}/dl-query", "dlQueryOntology", "Ontology", "Answer a DL query")
            .param("branch")
            .doc("Answers the class expression `expression` with its equivalent classes, its direct and inferred superclasses and subclasses, and its instances, as `answers` selects (every one by default). When the expression and every logical axiom of the ontology are in OWL 2 EL, the EL classifier answers it completely by classifying the expression as a fresh class with the dataset's kept classification, incrementally, and leaves the kept taxonomy as it was. Otherwise the OWL 2 RL rules answer it by demand reasoning over a copy of the terminology and the assertions, which can miss class answers. `reasoning` (`auto`, `el` or `rl`) chooses the engine, and `el` keeps to the classifier outside EL, leaving out the axioms it cannot use. The answer's `reasoning` names the engine, why it was chosen, whether the answers are complete, and the axioms outside the profile that can make them incomplete. `syntax` reads a text expression as Manchester syntax (the default) or functional-style syntax, and an object expression is read as the JSON form. A Manchester expression that does not read is `400` as `POST /$/ontology/{ds}/parse` answers it. A caller whose grants limit endpoints needs the `query` endpoint.")
            .see("ontology")
            .param("timeout")
            .body(
                true,
                "The ontology, the expression and the answers.",
                json!({ "application/json": { "schema": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["expression"],
                    "properties": {
                        "graph": selection["graph"],
                        "imports": selection["imports"],
                        "mode": selection["mode"],
                        "expression": { "description": "The class expression: Manchester or functional-style text, or the JSON form." },
                        "syntax": { "type": "string", "enum": ["manchester", "functional"], "description": "The syntax of a text expression (`manchester` by default). An object is read as the JSON form." },
                        "reasoning": { "type": "string", "enum": ["auto", "el", "rl"], "description": "The engine (`auto` by default)." },
                        "answers": { "type": "array", "items": { "type": "string", "enum": ["equivalents", "superclasses", "directSuperclasses", "subclasses", "directSubclasses", "instances"] } },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "The most IRIs each answer and the gaps list (1,000 by default)." },
                        "threads": { "type": "integer", "minimum": 0, "maximum": 1024, "description": "The worker threads of the EL classifier, within the algorithm pool's grant (0 for the grant)." },
                        "maxDerived": { "type": "integer", "minimum": 1, "description": "The most facts the classifier derives before the run stops with `507`." }
                    }
                } } }),
            )
            .resp("200", "The answers, each a list of IRIs with its count, and the reasoning that found them.", Some(object.clone()))
            .errors(&[400, 404, 408, 507]),
    );
    p.add(
        read(op(GET, "/$/ontology/{ds}/diff", "diffOntology", "Ontology", "Diff the axioms of two commits"))
            .doc("The axioms that differ between the commits `from` and `to` (the head by default), with the graph of each. The terminology of each state is read as the summary reads it, and the two sets of axioms are compared. With `assertions=true`, the assertions that the triples changed between the two commits carry are added and removed too. When no triple of the graphs read changed, the diff is empty. `format=text` answers `+ axiom` and `- axiom` lines in functional-style syntax. A caller whose grants limit endpoints needs the `diff` endpoint.")
            .see("ontology")
            .query("from", s(), "The earlier state: a commit number, a timestamp or a named snapshot, as `at` takes them.")
            .query("to", s(), "The later state (the head by default).")
            .query("assertions", boolean(), "Also diff the assertions that the changed triples carry.")
            .query("format", json!({ "type": "string", "enum": ["json", "text"] }), "`json` (the default) or `text`.")
            .query("limit", int(), "The most axioms listed on each side, from 1 to 10,000 (1,000 by default).")
            .resp(
                "200",
                "The axioms added and removed.",
                Some(json!({
                    "application/json": { "schema": { "type": "object" } },
                    "text/plain": { "schema": { "type": "string" } }
                })),
            )
            .errors(&[400, 404, 408, 410]),
    );
    p.add(
        read(op(GET, "/$/ontology/{ds}/search", "searchOntology", "Ontology", "Search the entities of an ontology"))
            .doc("The entities whose labels, synonyms, short forms, IRIs or definitions match `q`, in any language and with typo tolerance, ranked for a typeahead: a label equal to the query first, then labels that start with it, synonyms, short forms and IRIs equal to it or starting with it, labels and then synonyms that contain its words, matches within a few edits, and definitions that contain its words. Within each tier the order is the edits, the field, entities that are not deprecated first, the caller's language, the length of the matched text, the text and the IRI. The search reads the head of the branch from an entity index that each commit's changes keep current, and the caller sees only the graphs their grants allow. A query of 1 to 3 characters takes no edits as typed, 4 to 7 characters take one and longer queries two. `at` and `inferences` are refused.")
            .see("ontology")
            .query("q", s(), "The text searched for.")
            .query("kind", s(), "Entity kinds, comma separated: `class`, `property` (every kind of property), `objectProperty`, `dataProperty`, `annotationProperty`, `individual`, `datatype`, `concept`, `conceptScheme` or `collection` (all kinds by default).")
            .query("lang", s(), "The caller's languages, comma separated and most preferred first, as BCP 47 tags. `Accept-Language` is read when it is absent. Values in these languages rank first within a tier, then untagged values, then English, then the others.")
            .query("limit", int(), "The most entities listed, from 1 to 100 (10 by default).")
            .query("offset", int(), "The entities skipped. `offset` and `limit` together reach at most 10,000, and `next` is null when no later page fits in that window although `more` is true.")
            .query("fuzzy", boolean(), "Rank the matches within a few edits (true by default).")
            .query("definitions", boolean(), "Rank the matches in definitions (true by default).")
            .resp(
                "200",
                "One page of ranked entities.",
                Some(json!({ "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "dataset": { "type": "string" },
                        "commit": { "type": "integer" },
                        "graphs": { "type": "array", "items": { "type": "string" } },
                        "query": { "type": "string" },
                        "results": { "type": "array", "items": {
                            "type": "object",
                            "description": "One entity. `type` is `entity`, so that lists which mix kinds of result tell them apart.",
                            "properties": {
                                "type": { "type": "string", "enum": ["entity"] },
                                "iri": { "type": "string" },
                                "kinds": { "type": "array", "items": { "type": "string" } },
                                "label": { "type": ["string", "null"], "description": "The label in the caller's best language." },
                                "labelLang": { "type": ["string", "null"] },
                                "shortForm": { "type": ["string", "null"], "description": "The `oboInOwl:id`, `skos:notation`, prefixed name or local name." },
                                "deprecated": { "type": "boolean" },
                                "match": { "type": "object", "properties": {
                                    "field": { "type": "string", "enum": ["label", "synonym", "shortForm", "definition"] },
                                    "property": { "type": ["string", "null"], "description": "The property of the value, or null for the IRI and its local name." },
                                    "text": { "type": "string" },
                                    "lang": { "type": ["string", "null"] },
                                    "kind": { "type": "string", "enum": ["exact", "prefix", "words", "fuzzy"] },
                                    "distance": { "type": "integer" },
                                    "tier": { "type": "integer" },
                                    "highlight": { "type": "array", "description": "The matched ranges of `text` as `[start, end]` pairs in UTF-16 code units.", "items": { "type": "array", "items": { "type": "integer" } } }
                                } }
                            }
                        } },
                        "offset": { "type": "integer" },
                        "limit": { "type": "integer" },
                        "more": { "type": "boolean" },
                        "next": { "type": ["integer", "null"] },
                        "index": { "type": "string", "enum": ["current", "updated", "built"], "description": "Whether the index reflected the head, applied the changes of later commits or was built for this search." }
                    }
                } } })),
            )
            .errors(&[400, 404, 408, 409]),
    );
    let name_props = json!({
        "iri": { "type": "string" },
        "label": { "type": ["string", "null"], "description": "The `rdfs:label`, `skos:prefLabel` or SKOS-XL preferred label in the caller's best language." },
        "labelLang": { "type": ["string", "null"] },
        "shortForm": { "type": ["string", "null"], "description": "The `oboInOwl:id`, `skos:notation`, prefixed name or local name." },
        "kinds": { "type": "array", "items": { "type": "string" } },
        "deprecated": { "type": "boolean" }
    });
    let mut node_props = name_props.clone();
    node_props["children"] =
        json!({ "type": "integer", "description": "The number of direct children." });
    node_props["parents"] =
        json!({ "type": "integer", "description": "The number of direct parents." });
    let node = json!({ "type": "object", "properties": node_props });
    p.add(
        read(op(GET, "/$/ontology/{ds}/hierarchy", "ontologyHierarchy", "Ontology", "List one level of an entity hierarchy"))
            .doc("One level of the class, object property, data property, annotation property or SKOS concept hierarchy, read from the store's indexes without reading the whole ontology: the direct children of `iri`, or the roots, a page at a time in label order with deprecated entities last, each with the numbers of its own children and parents. Classes follow `rdfs:subClassOf` between named classes, properties `rdfs:subPropertyOf`, and concepts `skos:broader` and `skos:narrower` in either direction. The roots of the class hierarchy are the ontology header's `IAO_0000700` root terms when it names some, and otherwise the classes without a parent; the roots of a scheme are its top concepts with its concepts that have no broader one. `ancestors=true` adds the ancestors of `iri` up to the roots, with the edges between them, at most 2,000. The caller sees only the graphs and triples their grants allow. `inferences` is refused.")
            .see("ontology")
            .query("kind", json!({ "type": "string", "enum": ["class", "objectProperty", "dataProperty", "annotationProperty", "concept"] }), "The hierarchy (`class` by default).")
            .query("iri", s(), "The node whose children to list; the roots when absent.")
            .query("ancestors", boolean(), "Also list the ancestors of `iri`.")
            .query("scheme", s(), "Keep the concepts of this scheme, and start the roots at its top concepts.")
            .query("deprecated", boolean(), "List deprecated children (true by default, after the others).")
            .query("lang", s(), "The caller's languages for labels, comma separated and most preferred first. `Accept-Language` is read when it is absent.")
            .query("limit", int(), "The most children listed, from 1 to 1,000 (100 by default).")
            .query("offset", int(), "The children skipped.")
            .resp(
                "200",
                "The node, a page of its children or of the roots, and its ancestors when asked for.",
                Some(json!({ "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "dataset": { "type": "string" },
                        "commit": { "type": "integer" },
                        "graphs": { "type": "array", "items": { "type": "string" } },
                        "kind": { "type": "string" },
                        "node": { "oneOf": [node.clone(), { "type": "null" }] },
                        "children": { "type": "array", "items": node.clone() },
                        "total": { "type": "integer" },
                        "offset": { "type": "integer" },
                        "limit": { "type": "integer" },
                        "next": { "type": ["integer", "null"] },
                        "order": { "type": "string", "enum": ["label", "iri"], "description": "`iri` for a node with more than 20,000 children, which are listed in IRI order." },
                        "roots": { "type": "string", "enum": ["ontology", "scheme", "computed"], "description": "Where the roots came from, for a list of roots." },
                        "deprecatedRoots": { "type": "integer", "description": "The deprecated roots `deprecated=false` left out." },
                        "ancestors": { "type": "object", "properties": {
                            "nodes": { "type": "array", "items": node.clone() },
                            "edges": { "type": "array", "description": "`[child, parent]` IRI pairs.", "items": { "type": "array", "items": { "type": "string" } } },
                            "truncated": { "type": "boolean" }
                        } }
                    }
                } } })),
            )
            .errors(&[400, 404, 408]),
    );
    let incoming = json!({ "type": "object", "properties": {
        "property": { "type": "string" },
        "count": { "type": "integer", "description": "The statements counted, at most 10,000." },
        "more": { "type": "boolean" },
        "subjects": { "type": "array", "items": { "type": "string" } }
    } });
    p.add(
        read(op(GET, "/$/ontology/{ds}/term", "ontologyTerm", "Ontology", "Describe one entity"))
            .doc("What the graphs read say about one entity, read from the store's indexes without reading the whole ontology: its triples with a named or literal object and the annotations their reifications give (`statements`), the axioms the triples about it map to by the OWL 2 reverse mapping (`axioms`), the axioms of other entities that hold it inside a class expression (`mentions`), the statements of named subjects that point at it by predicate (`incoming`), the statements that use it as their predicate (`usage`), and the label, short form and kinds of every IRI the answer uses (`names`). `format` downloads the triples about the entity, its blank nodes and the reifications of its annotated triples in Turtle, N-Triples, RDF/XML or JSON-LD, or its axioms in functional-style syntax, as an attachment. The caller sees only the graphs and triples their grants allow. `inferences` is refused.")
            .see("ontology")
            .query("iri", s(), "The entity.")
            .query("mentions", int(), "The most axioms of other entities listed, from 0 to 2,000 (200 by default).")
            .query("incoming", int(), "The subjects listed per incoming predicate, from 0 to 200 (20 by default).")
            .query("lang", s(), "The caller's languages for labels, comma separated and most preferred first. `Accept-Language` is read when it is absent.")
            .query("format", json!({ "type": "string", "enum": ["json", "ttl", "nt", "rdfxml", "jsonld", "ofn"] }), "`json` (the default), or a download format.")
            .resp(
                "200",
                "The entity's description, or a download.",
                Some(json!({
                    "application/json": { "schema": {
                        "type": "object",
                        "properties": {
                            "dataset": { "type": "string" },
                            "commit": { "type": "integer" },
                            "graphs": { "type": "array", "items": { "type": "string" } },
                            "iri": { "type": "string" },
                            "label": { "type": ["string", "null"] },
                            "labelLang": { "type": ["string", "null"] },
                            "shortForm": { "type": ["string", "null"] },
                            "kinds": { "type": "array", "items": { "type": "string" } },
                            "deprecated": { "type": "boolean" },
                            "types": { "type": "array", "items": { "type": "string" } },
                            "ontologies": { "type": "array", "items": { "type": "object", "properties": { "graph": { "type": "string" }, "ontology": { "type": ["string", "null"] } } } },
                            "statements": { "type": "array", "items": { "type": "object", "properties": {
                                "property": { "type": "string" },
                                "value": { "description": "An IRI as a string, or a literal as `{\"@value\", \"@language\"?, \"@type\"?}`." },
                                "graph": { "type": "string" },
                                "annotations": { "type": "array", "items": { "type": "object", "properties": { "property": { "type": "string" }, "value": {} } } }
                            } } },
                            "statementsTruncated": { "type": "boolean" },
                            "xlLabels": { "type": "array", "description": "The literal forms of the entity's SKOS-XL labels, with the label property and the graph.", "items": { "type": "object", "properties": {
                                "property": { "type": "string" },
                                "value": { "description": "A literal as `{\"@value\", \"@language\"?, \"@type\"?}`." },
                                "graph": { "type": "string" }
                            } } },
                            "axioms": { "type": "array", "items": { "type": "object", "description": "An axiom with `type`, `ofn`, `axiom` (its JSON form) and `graph`." } },
                            "mentions": { "type": "object", "properties": {
                                "axioms": { "type": "array", "items": { "type": "object" } },
                                "total": { "type": "integer" },
                                "truncated": { "type": "boolean" }
                            } },
                            "incoming": { "type": "array", "items": incoming.clone() },
                            "usage": { "oneOf": [incoming, { "type": "null" }] },
                            "names": { "type": "object", "additionalProperties": { "type": "object", "properties": name_props } }
                        }
                    } },
                    "text/turtle": { "schema": { "type": "string" } },
                    "application/n-triples": { "schema": { "type": "string" } },
                    "application/rdf+xml": { "schema": { "type": "string" } },
                    "application/ld+json": { "schema": { "type": "object" } },
                    "text/owl-functional": { "schema": { "type": "string" } }
                })),
            )
            .errors(&[400, 404, 408]),
    );
    // an axiom an edit takes: its JSON form, or a string in functional-style syntax
    let axiom_in = json!({ "oneOf": [
        { "type": "object", "description": "The axiom's JSON form." },
        { "type": "string", "description": "The axiom in functional-style syntax with the dataset's prefixes." }
    ] });
    p.add(
        write_params(op(POST, "/$/ontology/{ds}/axioms", "editOntology", "Ontology", "Add and remove axioms"))
            .doc("Adds and removes axioms, each in its JSON form or in functional-style syntax with the dataset's prefixes, as one commit. The ontology is read from the head under the writer lock. A removed axiom takes its triples with it, except those another axiom or triple still uses, and removing an axiom the ontology does not hold is not an error. An added axiom goes to `target` (the first graph read by default) with the declarations of the entities it uses that the ontology lacks, unless `declare` is false. An axiom the forward mapping cannot write is `400` with code `invalid-axiom`.")
            .see("ontology")
            .param("ifMatch")
            .body(
                true,
                "The axioms.",
                json!({ "application/json": { "schema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "graph": selection["graph"],
                        "imports": selection["imports"],
                        "mode": selection["mode"],
                        "add": { "type": "array", "items": axiom_in.clone() },
                        "remove": { "type": "array", "items": axiom_in },
                        "declare": { "type": "boolean" },
                        "target": { "type": "string", "description": "The graph that takes the added axioms." },
                        "message": { "type": "string" }
                    }
                } } }),
            )
            .resp(
                "200",
                "What the edit did, with the receipt when asked for.",
                Some(json!({ "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "added": { "type": "integer" },
                        "present": { "type": "integer" },
                        "removed": { "type": "integer" },
                        "absent": { "type": "array", "items": { "type": "string" } },
                        "declared": { "type": "array", "items": { "type": "string" } },
                        "addedAxioms": edited.clone(),
                        "removedAxioms": edited.clone(),
                        "axiomsTruncated": { "type": "boolean", "description": "Whether `addedAxioms` or `removedAxioms` was cut at 10,000." },
                        "inserted": { "type": "integer" },
                        "deleted": { "type": "integer" },
                        "kept": { "type": "integer" },
                        "committed": { "type": "boolean" }
                    }
                } } })),
            )
            .errors(&[400, 403, 404, 408, 412, 422, 507]),
    );
    p.add(
        write_params(sel(op(DELETE, "/$/ontology/{ds}/entity", "removeOntologyEntity", "Ontology", "Remove an entity")))
            .doc("Removes an entity in one commit: its declarations, the axioms of the graphs read that mention it and the assertions about it that the store holds there. The ontology is read from the head under the writer lock. The answer is an edit's, and `removedAxioms` lists what the removal took. An IRI the ontology does not mention removes nothing and makes no commit.")
            .see("ontology")
            .param("ifMatch")
            .query("iri", s(), "The entity's IRI.")
            .query("message", s(), "The commit message, as the `Sparkles-Commit-Message` header gives it.")
            .resp(
                "200",
                "What the removal did, with the receipt when asked for.",
                Some(json!({ "application/json": { "schema": {
                    "type": "object",
                    "properties": {
                        "removed": { "type": "integer" },
                        "removedAxioms": edited.clone(),
                        "axiomsTruncated": { "type": "boolean" },
                        "deleted": { "type": "integer" },
                        "kept": { "type": "integer" },
                        "committed": { "type": "boolean" }
                    }
                } } })),
            )
            .errors(&[400, 403, 404, 408, 412, 422, 507]),
    );
}

fn forms(p: &mut Paths) {
    let term = json!({ "type": "object", "description": "A term in the SPARQL JSON results form: `{\"type\": \"uri\", \"value\": …}` or `{\"type\": \"literal\", \"value\": …, \"datatype\"?: …, \"xml:lang\"?: …}`. A blank node `{\"type\": \"bnode\", \"value\": label}` names a nested child: a stored one by the label the nested model gives it, or a new one that `nested` describes." });
    let field_changes = json!({ "type": "array", "items": { "type": "object", "required": ["field"], "properties": {
        "field": { "type": "string", "description": "The field's id, its path in SPARQL syntax." },
        "remove": { "type": "array", "items": term.clone(), "description": "Values to remove. A stored blank node of a field that declares `sh:node` also removes the child's visible quads when nothing else refers to it." },
        "add": { "type": "array", "items": term.clone(), "description": "Values to add. A blank node is a new child that `nested` describes." },
        "list": { "type": "array", "items": term.clone(), "description": "For a list field, the members its `rdf:List` takes in order, replacing the stored list. `remove` and `add` stay empty." }
    } } });
    let change = json!({ "application/json": { "schema": {
        "type": "object",
        "additionalProperties": false,
        "required": ["changes"],
        "properties": {
            "focus": { "type": "string", "description": "The IRI of the resource. Give either `focus` or `class`." },
            "class": { "type": "string", "description": "The class of a new instance, whose IRI the server mints from the `forms` setting's `newIri`. The change also adds the instance's `rdf:type`." },
            "branch": { "type": "string", "description": "The branch the form was opened on. A form opened on `main` saves where the dataset's `forms` setting says, which may be the caller's session branch." },
            "shapes": { "type": "string", "enum": ["guard", "graphs", "drafted", "skos"], "description": "The shapes the form was built from, when not the setting's. `skos` names the built-in SKOS shapes." },
            "changes": field_changes.clone(),
            "expect": { "type": "object", "additionalProperties": { "type": "array", "items": term.clone() }, "description": "The values the form loaded, by field id, for every changed field, and for a list field the members of its list in order." },
            "nested": { "type": "array", "description": "Changes to nested blank-node children, committed atomically with `changes`. Each entry names its `node`, the `parent` it hangs from (absent for the root, else another entry's node) and the parent's `field`. A stored child must be a visible value of that field now, and every node between it and the root needs an entry.", "items": { "type": "object", "required": ["node", "field"], "additionalProperties": false, "properties": {
                "node": term.clone(),
                "parent": term.clone(),
                "field": { "type": "string", "description": "The parent's field id." },
                "changes": field_changes,
                "expect": { "type": "object", "additionalProperties": { "type": "array", "items": term }, "description": "For a stored child, the values the form loaded for every changed field." }
            } } },
            "message": { "type": "string", "description": "The commit message." }
        }
    } } });
    p.add(
        op(GET, "/$/forms/{ds}", "getForm", "Validation", "The form of a resource")
            .doc("The fields that the property shapes of the node shapes targeting `focus` describe, laid out by `sh:group` and `sh:order` and labelled by `sh:name`, with the resource's values read under the caller's view, the widget of each field, the constraints the browser checks and the triples no property shape covers. Fields the caller's protections hide from reads are absent, and fields it cannot write are read-only with a `reason`. Reads follow the caller's query endpoint grants and writeability its update endpoint grants on the branch saves go to. A focus no shape targets answers `200` with no fields. With `class` instead of `focus`, the form is that of a new instance of the class, with the fields of the node shapes whose class targets select it and no values, and `focus` is `null`. `save` says where a save goes, and `guard` gives the write guard's mode.")
            .see("editing-forms")
            .query("focus", s(), "The IRI of the resource. Give either `focus` or `class`.")
            .query("class", s(), "The class of a new instance.")
            .query("shapes", json!({ "type": "string", "enum": ["guard", "graphs", "drafted", "skos"] }), "Where the shapes come from, when not the dataset's `forms` setting. `skos` names the built-in SKOS shapes of `GET /$/skos/shapes`, which only a request can choose.")
            .query("lang", s(), "The language labels are picked in, then English, then untagged.")
            .query("nested", json!({ "type": "boolean" }), "Also answer the blank-node children that fields declaring `sh:node` reach (`nestedNodes`) and the empty forms of new children (`templates`). A nested model past its allowances answers the flat form with `nested.refused`.")
            .params(&["branch", "at", "timeout"])
            .resp(
                "200",
                "The form model.",
                Some(json!({ "application/json": { "schema": {
                    "type": "object",
                    "required": ["formFormat", "focus", "types", "commit", "branch", "editable", "shapes", "groups", "fields", "other", "graph", "closed", "save"],
                    "properties": {
                        "formFormat": { "type": "integer", "enum": [1] },
                        "focus": { "type": ["string", "null"] },
                        "types": { "type": "array", "items": { "type": "string" } },
                        "commit": { "type": "integer" },
                        "branch": { "type": "string" },
                        "editable": { "type": "boolean", "description": "The caller may save changes through this form." },
                        "shapes": { "type": "object", "description": "`{source, nodeShapes, missing?, unknownHints}`." },
                        "groups": { "type": "array", "items": { "type": "object", "description": "`{iri, label, order}`." } },
                        "fields": { "type": "array", "items": { "type": "object", "description": "`{id, path, inverse, editable, reason?, label, description?, group, order, widget, editor?, viewer?, constraints, sources, values}`, where each value is `{term, graph, label?, readOnly?, items?}` and `items` lists the members of a list field's `rdf:List`. `constraints.list` holds a list field's lengths and member constraints, and `constraints.or` the options of `sh:or` or `sh:xone` over value shapes." } },
                        "other": { "type": "array", "items": { "type": "object", "description": "`{predicate, values}`." } },
                        "graph": { "type": ["string", "null"], "description": "The graph new values go into, `null` for the default graph." },
                        "closed": { "type": "boolean" },
                        "save": { "type": "object", "description": "`{to, branch, exists, mainProtected}`: `to` is `main` or `branch`." },
                        "guard": { "type": ["object", "null"], "description": "`{language, mode}` of the write guard." },
                        "nestedNodes": { "type": "array", "items": { "type": "object" }, "description": "With `nested=true`: `{id, term, anchors, …}` of each stored child, with the fields of a form model. Each anchor is `{parent, field, graph, inverse}`, where `parent` is `root` or another child's `id`." },
                        "templates": { "type": "array", "items": { "type": "object" }, "description": "With `nested=true`: `{node, …}`, the empty form of a new child under a field whose `constraints.node` is `node`." },
                        "nested": { "type": "object", "description": "`{refused}`: why the nested model was refused, when it was." }
                    }
                } } })),
            )
            .errors(&[400, 403, 404, 408, 501]),
    );
    p.add(
        op(GET, "/$/skos/shapes", "getSkosShapes", "Validation", "The built-in SKOS shapes")
            .doc("The SHACL shapes that lay out the forms of concept schemes, concepts, collections, ordered collections and SKOS-XL labels and check the integrity conditions of the SKOS Reference: S9, S13, S14, S27, S37 and S46, with S51, S52 and S58 of SKOS-XL and the literal forms of SKOS-XL labels counted for S13 and S14. Each result's message names its condition. Forms read them with `shapes=skos`, and `POST /{ds}/shacl` and `PUT /$/validation/{ds}` take the text as shapes.")
            .see("skos-shapes")
            .resp("200", "The shapes.", Some(json!({ "text/turtle": text() }))),
    );
    p.add(
        op(POST, "/$/forms/{ds}/check", "checkForm", "Validation", "Check a form's changes")
            .doc("Turns the changes, with those of nested children, into the quads one update deletes and inserts, runs it as a dry run and validates the nodes it touches at the state it would make against the form's shapes, whether or not the dataset has a guard. The answer is `200` with the write preview, the update, the fields another commit changed since the form loaded them (`conflicts`, which a save would refuse), and the validation results, each with the `field` it belongs to or `null`. A check of a session branch that does not exist yet previews its first commit on `main`'s head, which `main`'s protection does not refuse. A caller whose view hides part of the data gets no validation (`withheld`), as the validation routes refuse such callers. It needs write on the branch, through the update endpoint.")
            .see("editing-forms")
            .body(true, "The changes.", change.clone())
            .resp(
                "200",
                "The preview and the validation.",
                Some(json!({ "application/json": { "schema": {
                    "type": "object",
                    "required": ["branch", "preview", "update", "conflicts", "validation"],
                    "properties": {
                        "branch": { "type": "string" },
                        "preview": { "type": "object", "description": "The dry run, as a write preview answers it." },
                        "update": { "type": "string" },
                        "conflicts": { "type": "array", "items": { "type": "object" }, "description": "`{id, node?, now}` of each changed field whose stored values differ from `expect`, with the nested `node` it belongs to, and of each parent field a stored child no longer hangs from." },
                        "validation": { "type": "object", "description": "`{checked, withheld, timedOut, conforms, results}`." }
                    }
                } } })),
            )
            .errors(&[400, 403, 404, 408]),
    );
    p.add(
        op(POST, "/$/forms/{ds}/save", "saveForm", "Validation", "Save a form's changes")
            .doc("Commits the changes, with those of nested children, as one update with the message, when the stored values of every changed field are still those `expect` names and every stored child still hangs from its parent, compared under the writer lock. Otherwise it answers `409` with code `form-conflict` and the fields with their stored values `now` and nested `node`. The write guard runs as for any write. A save of a form opened on `main` while the `forms` setting sends saves to a branch creates the caller's session branch from `main`'s head when it does not exist.")
            .see("editing-forms")
            .body(true, "The changes.", change)
            .resp(
                "200",
                "The commit receipt.",
                Some(json!({ "application/json": { "schema": {
                    "type": "object",
                    "required": ["committed", "commit", "branch", "branchCreated"],
                    "properties": {
                        "committed": { "type": "boolean" },
                        "commit": { "type": "object" },
                        "branch": { "type": "string" },
                        "branchCreated": { "type": "boolean" },
                        "focus": { "type": "string", "description": "The resource saved, which for a new instance is the IRI minted for it." },
                        "dataset": { "type": "string" },
                        "datasetId": { "type": "string" },
                        "validation": { "type": "object" }
                    }
                } } })),
            )
            .errors(&[400, 403, 404, 408, 409, 422, 507]),
    );
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
    let resource_metadata = json!({
        "type": "object",
        "required": ["resource", "authorization_servers", "scopes_supported", "bearer_methods_supported"],
        "properties": {
            "resource": { "type": "string", "format": "uri", "description": "The resource identifier of `/$/mcp`: `server.public_url` followed by `/$/mcp`." },
            "authorization_servers": { "type": "array", "items": { "type": "string", "format": "uri" }, "description": "The issuer of the `[oidc]` provider." },
            "scopes_supported": { "type": "array", "items": { "type": "string" } },
            "bearer_methods_supported": { "type": "array", "items": { "type": "string", "enum": ["header"] } },
            "resource_name": { "type": "string" }
        }
    });
    for (path, id, summary) in [
        (
            "/.well-known/oauth-protected-resource",
            "oauthProtectedResource",
            "Protected-resource metadata of the MCP endpoint",
        ),
        (
            "/.well-known/oauth-protected-resource/$/mcp",
            "oauthProtectedResourceMcp",
            "Protected-resource metadata of the MCP endpoint (path-suffixed)",
        ),
    ] {
        p.add(
            op(GET, path, id, "MCP", summary)
                .doc("RFC 9728 metadata that tells MCP clients which authorization server issues tokens for `/$/mcp` and which scopes it takes. Both paths answer the same document. `404` unless the server runs with `--mcp`, an auth configuration and an `[oidc]` provider.")
                .see("oauth-for-the-mcp-endpoint")
                .json_inline("200", "The metadata.", resource_metadata.clone())
                .errors(&[404]),
        );
    }
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
        op(GET, "/$/server/memory", "serverMemory", "Server", "Server memory")
            .doc("The query memory pool (capacity, bytes granted, requests waiting) and the block, result, SERVICE and history caches, with the bytes each dataset holds in the shared ones.")
            .see("budgets")
            .json("200", "The memory state.", "ServerMemory"),
    );
    p.add(
        op(GET, "/$/ready", "ready", "Server", "Readiness check")
            .doc("`200` when the server is ready and `503` otherwise, both with `ReadyInfo`. The server is `starting` while datasets open and `degraded` when one failed to open, which is not ready unless `--ready-ignore-failed` is set. Without `metrics`, only readable datasets are listed.")
            .see("server")
            .json("200", "Ready.", "ReadyInfo")
            .json("503", "Starting, degraded or draining.", "ReadyInfo"),
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
        op(POST, "/$/datasets/{ds}/rename", "renameDataset", "Datasets", "Rename a dataset")
            .doc("Requires server-admin. Refused while source or destination is covered by a dataset grant, or while persistent dataset handles remain alive. Dataset identity is retained.")
            .body(true, "The new name.", json!({"application/json": {"schema": {
                "type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}
            }}}))
            .json_inline("200", "Renamed.", json!({"type": "object", "required": ["name", "renamedFrom"],
                "properties": {"name": {"type": "string"}, "renamedFrom": {"type": "string"}}}))
            .errors(&[400, 409]),
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
        .json("200", "The constraints layer.", "ConstraintsReport")
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
            POST,
            "/$/graph/{ds}/groups",
            "getGraphGroups",
            "Schema",
            "Groups of a class or group",
        )
        .doc("Groups the instances of a class, or of a group from an earlier answer, by the communities F14 wrote, by a property's most frequent values or ranges, or into the members themselves, and counts the edges between the groups, to the sibling groups of the path and to the other classes. `by` is `auto` (the default), `members`, `{\"community\": IRI}`, `{\"property\": IRI, \"buckets\": n}` or `{\"reports\": {\"level\": n}}`. The last groups the whole dataset by its GraphRAG community hierarchy, leaves `within` out, reads only main and answers `available: false` with a reason, and nothing from the reports, when the caller's access does not cover the reports graph. Every count comes from queries run under the caller's view, timeout and memory budget, so it needs read through the query endpoint and fails with `507` like a query. Answers hold at most 5,000 groups or members and 20,000 edges, and say when they were cut. An edge's ends are `g:` and a group id, `c:` and a class IRI, or `m:` and a member IRI.")
        .see("graph-overview")
        .param("timeout")
        .param("reasoning")
        .body(
            true,
            "What to group and how.",
            json!({ "application/json": { "schema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "within": { "type": "object", "description": "`{\"class\": IRI}` or `{\"group\": id}`. It is required except with `by: {\"reports\": {}}`." },
                    "by": { "description": "`auto`, `members`, `{\"community\": IRI}`, `{\"property\": IRI, \"buckets\": n}` with 2 to 48 buckets and 12 by default, or `{\"reports\": {\"level\": n, \"graph\": IRI}}` with a level from 0 to 15 and 0 by default." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 5000, "description": "Groups or members at most, and the size under which `auto` lists the members." },
                    "at": { "type": "string", "description": "A commit, time or named snapshot." }
                }
            } } }),
        )
        .resp(
            "200",
            "The groups or members, and the edges.",
            Some(json!({ "application/json": { "schema": {
                "type": "object",
                "required": ["commit", "within", "grouping", "groups", "edges", "truncated"],
                "properties": {
                    "commit": { "type": "integer" },
                    "within": { "type": "object" },
                    "grouping": { "type": "object" },
                    "communities": { "type": "boolean" },
                    "groups": { "type": "array", "items": { "type": "object" } },
                    "members": { "type": "array", "items": { "type": "object" } },
                    "edges": { "type": "array", "items": { "type": "object" } },
                    "truncated": { "type": "boolean" },
                    "note": { "type": "string" },
                    "available": { "type": "boolean", "description": "A GraphRAG grouping only. It is false when the caller may not read the reports graph or the reports hold no communities." },
                    "unavailable": { "type": "string", "enum": ["access", "none"] },
                    "reason": { "type": "string" },
                    "levels": { "type": "array", "items": { "type": "object" }, "description": "A GraphRAG grouping only. The levels of the hierarchy and their community counts." }
                }
            } } })),
        )
        .errors(&[400, 403, 404, 408, 503, 507]),
    );
    p.add(
        op(
            POST,
            "/$/graph/{ds}/facets",
            "getGraphFacets",
            "Schema",
            "Facet histograms of a set",
        )
        .doc("Counts the entities of a set by ranges of round width of a property's numbers or of the years of its dates, or by its 12 most frequent values, with the entities that have no value. The set is a class, up to 20,000 IRIs, or the distinct values of a variable of a `SELECT` query, and a set above 1,000,000 entities is refused with `507`. Each brush in `filters` keeps the entities with a value in its range or among its values, and each facet counts the set filtered by every brush but its own, so the facets filter each other. `matching` counts the set under every brush. A facet over a budget carries an `error` while the others stay. `candidates: true` also lists the properties of a sample of the set, ranked by the share that has a value. Every count comes from queries run under the caller's view, timeout and memory budget, so it needs read through the query endpoint and fails with `507` like a query.")
        .see("facets")
        .param("timeout")
        .param("reasoning")
        .body(
            true,
            "The set, its facets and the brushes.",
            json!({ "application/json": { "schema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["within"],
                "properties": {
                    "within": { "type": "object", "description": "`{\"class\": IRI}`, `{\"nodes\": [IRI, …]}` with up to 20,000 IRIs, or `{\"query\": SELECT, \"variable\": name}`." },
                    "facets": { "type": "array", "maxItems": 6, "items": { "type": "object", "description": "`{\"property\": IRI, \"bins\"?: 2–40, \"top\"?: 1–12, \"kind\"?: \"numeric\" | \"date\" | \"values\", \"edges\"?: [number, …], \"bars\"?: [term, …]}`. `edges` and `bars` hand back the ranges or values of an earlier answer." } },
                    "filters": { "type": "array", "maxItems": 6, "items": { "type": "object", "description": "`{\"property\": IRI, \"range\": [from, to], \"kind\"?: \"numeric\" | \"date\"}`, where a date range is in whole years and `to` is excluded, or `{\"property\": IRI, \"values\": [term, …]}` with terms in the SPARQL JSON results form." } },
                    "candidates": { "type": "boolean", "description": "Also list the properties of the set." },
                    "at": { "type": "string", "description": "A commit, time or named snapshot." }
                }
            } } }),
        )
        .resp(
            "200",
            "The counts of each facet.",
            Some(json!({ "application/json": { "schema": {
                "type": "object",
                "required": ["commit", "size", "matching", "facets", "truncated"],
                "properties": {
                    "commit": { "type": "integer" },
                    "size": { "type": "integer", "description": "Entities in the set." },
                    "matching": { "type": "integer", "description": "Entities of the set under every brush." },
                    "facets": { "type": "array", "items": { "type": "object", "description": "`property`, `kind`, `missing`, and `bins` of `{from, to, count}` for a range facet or `values` of `{value, count, label?}` and `other` for a bar list, or `error` with its `budget` and `message`." } },
                    "candidates": { "type": "array", "items": { "type": "object", "description": "`{property, share, kind}`." } },
                    "truncated": { "type": "boolean", "description": "The candidates come from a sample of the set or were cut to 40." }
                }
            } } })),
        )
        .errors(&[400, 403, 404, 408, 503, 507]),
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
            json!({ "type": "string", "enum": ["gzip", "xz", "bzip2", "zstd", "brotli", "lz4", "none"] }),
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
        .doc("OWL 2 RL inconsistency checks. With justify=true, attach the first subset-minimal support to each finding under one deadline and check budget.")
        .query("justify", boolean(), "Attach first minimal supports.")
        .query("maxChecks", int(), "The most oracle checks shared by all requested supports.")
        .see("reasoning-status-and-diagnostics")
        .json("200", "The report.", "DiagnosticsReport"),
    );
    p.add(
        analysis_params(op(GET, "/$/reason/{ds}/diff", "entailmentDiff", "Reasoning", "Compare entailments between retained states"))
            .doc("Reads both states under their branch-specific query graph grants. Uses the same program on both states and does not write or change materialization records. JSON counts cover the complete delta with bounded samples. TriG and RDF Patch require retaining every changed triple and refuse a truncated delta.")
            .see("explanations")
            .query_req("from", s(), "A branch, branch@commit or current-branch commit selector.")
            .query("to", s(), "The second state selector, head by default.")
            .query("timeout", json!({"type":"number","exclusiveMinimum":0}), "Seconds, capped at the server timeout.")
            .query("sampleLimit", json!({"type":"integer","minimum":1,"maximum":10000,"default":100}), "The most retained triples in each direction.")
            .resp("200", "The complete counts and samples, or all changed triples.", Some(json!({
                "application/json":{"schema":sref("EntailmentDiff")},
                "application/trig":{"schema":s()},"application/rdf-patch":{"schema":s()},"text/rdf-patch":{"schema":s()}
            })))
            .errors(&[400,403,404,408,410,422,501,507]),
    );
    p.add(
        op(POST, "/$/reason/{ds}/justify", "justifyFinding", "Reasoning", "Find minimal inconsistency supports")
            .doc("Targets one finding, one class IRI or classes=all. Supports contain asserted quads and their schema or assertion role. Alternative supports share one bounded search. Incomplete results never certify a negative result or return an uncertified support. Reads only query-visible graphs and writes no dataset state.")
            .see("explanations").json_body(true,"JustifyRequest")
            .json("200","Minimal supports and search completeness.","JustificationReport")
            .errors(&[400,403,404,408,410,422,501,507]),
    );
    p.add(
        op(
            POST,
            "/$/reason/{ds}/explain",
            "explainInference",
            "Reasoning",
            "Explain why a triple is inferred",
        )
        .doc("Derivation trees for a triple of the materialized inferences or of what RDFS on read answers, searched from the facts the caller reads. Mode `whyNot` returns verified sets of missing facts. Source `demand` derives from the current visible inputs, with an explicit profile when the dataset has no recorded program. A bounded partial tree marks unexplained premises. A triple with no derivation from readable facts answers `404` with code `no-visible-derivation`, as one that is not entailed does, and a search over its limits answers `422` with code `reasoning-budget`.")
        .see("explanations")
        .json_body(true, "InferenceExplainRequest")
        .json("200", "The trees.", "Explanation")
        .errors(&[400, 404, 422]),
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
        .doc("The dataset's effective prefixes: the well-known ones, the prefixes the settings file declares and the dataset's own, as the `prefixes` settings kind computes them.")
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

fn algorithms(p: &mut Paths) {
    p.add(
        op(
            POST,
            "/$/algo/{ds}/run",
            "runAlgorithm",
            "Graph algorithms",
            "Run a graph algorithm",
        )
        .doc("Without `write` the run is the SPARQL query its request stands for, answered as `/{ds}/sparql` answers (`Accept`, `timeout`, `execution` and the budget overrides apply), one row per node, node pair or node and level. `summary=true` returns only the statistics of the run. A request with `nodes`, `keep`, `async` or `top` is a view-only run, which answers in SPARQL JSON with the rows of the listed nodes, `communitySize` for community and component rows, the run's `summary` and a `run` member that names the commit and whether the cache answered. With `keep` the whole result is kept in a cache of at most 8 results and 512 MiB charged to the server memory pool, and with `async` as well the run is an `algo-view` task that needs only read, which a request for the same key joins and the caller that started it may cancel. With `write` the run is a task that computes at the head commit and writes the rows in batched commits, with a run record in the graph. Read-only runs need read through the query endpoint and use Query rate/concurrency limits. Write-back runs need write through the update endpoint and use Update HTTP admission through the 202 response body. Background work uses the existing task queue and algorithm-thread admission, not an HTTP permit. A task can be cancelled with `DELETE /$/tasks/{id}`.")
        .see("graph-algorithms")
        .params(&["format", "timeout", "nocache", "execution"])
        .query("summary", json!({ "type": "boolean" }), "Return only the summary of the run.")
        .json_body(true, "AlgoRequest")
        .resp_h(
            "200",
            "The rows of the run, or its summary.",
            Some(json!({ "application/sparql-results+json": { "schema": { "type": "object" } } })),
            json!({}),
        )
        .resp("202", "A run with `write`, or a view-only run with `keep` and `async`, started as a task. A view-only answer has `joined: true` when it joined a run already in progress.", Some(json!({ "application/json": { "schema": sref("Task") } })))
        .errors(&[400, 409, 413, 429, 503, 507]),
    );
    p.add(
        op(
            GET,
            "/$/algo/{ds}/projections",
            "listAlgorithmProjections",
            "Graph algorithms",
            "List the named projections",
        )
        .see("graph-algorithms")
        .json(
            "200",
            "The definitions and the projection cache.",
            "AlgoProjectionList",
        ),
    );
    p.add(
        op(
            GET,
            "/$/algo/{ds}/projections/{name}",
            "getAlgorithmProjection",
            "Graph algorithms",
            "Get a named projection",
        )
        .see("graph-algorithms")
        .resp_h(
            "200",
            "The definition with its revision.",
            Some(json!({ "application/json": { "schema": sref("AlgoProjection") } })),
            json!({ "ETag": { "description": "`\"r<N>\"`", "schema": { "type": "string" } } }),
        ),
    );
    p.add(
        op(
            PUT,
            "/$/algo/{ds}/projections/{name}",
            "putAlgorithmProjection",
            "Graph algorithms",
            "Define a named projection",
        )
        .doc("Stores the definition as the next revision. `If-Match: \"r<N>\"` replaces only revision N, and `If-None-Match: *` only creates. A bare string in `predicates` is a predicate in the algorithm's own direction.")
        .see("graph-algorithms")
        .params(&["ifMatch", "ifNoneMatch"])
        .json_body(true, "AlgoProjectionDefinition")
        .json("200", "Replaced.", "AlgoProjection")
        .json("201", "Created.", "AlgoProjection")
        .errors(&[400, 412]),
    );
    p.add(
        op(
            DELETE,
            "/$/algo/{ds}/projections/{name}",
            "deleteAlgorithmProjection",
            "Graph algorithms",
            "Delete a named projection",
        )
        .see("graph-algorithms")
        .param("ifMatch")
        .no_content("Deleted.")
        .errors(&[412]),
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
    p.add(
        op(GET, "/$/views/{ds}", "listViews", "Views", "List views")
            .doc("The dataset's incremental views with their state: `building`, `current`, `behind` with the commits it lags, `suspended` with a reason, or `failed`.")
            .see("incremental-views")
            .json("200", "The views without their plans.", "ViewList"),
    );
    p.add(
        op(GET, "/$/views/{ds}/{name}", "getView", "Views", "Get a view")
            .see("incremental-views")
            .json("200", "The view with its state and its plan, each operator marked `incremental` or `recompute`.", "View")
            .errors(&[404]),
    );
    p.add(
        op(PUT, "/$/views/{ds}/{name}", "putView", "Views", "Define a view")
            .doc("Compiles the query, keeps the definition and starts the first evaluation as a task in `/$/tasks`. Needs `admin` on the dataset. A query outside the incremental fragment is refused with `400 not-incremental`, and Phase 2 options with `400 unsupported-option`.")
            .see("incremental-views")
            .json_body(true, "ViewDefinition")
            .json("200", "Replaced.", "View")
            .json("201", "Created.", "View")
            .errors(&[400]),
    );
    p.add(
        op(
            DELETE,
            "/$/views/{ds}/{name}",
            "deleteView",
            "Views",
            "Delete a view",
        )
        .doc("Removes the view and its state. Needs `admin` on the dataset.")
        .see("incremental-views")
        .no_content("Deleted.")
        .errors(&[404]),
    );
    p.add(
        op(POST, "/$/views/{ds}/{name}/rebuild", "rebuildView", "Views", "Rebuild a view")
            .doc("Evaluates the view again from scratch as a task, and lifts a suspension. Needs `admin` on the dataset.")
            .see("incremental-views")
            .json("202", "The rebuild's task.", "ViewTask")
            .errors(&[404]),
    );
    p.add(
        op(GET, "/{ds}/live", "liveQuery", "Views", "Live query")
            .doc("A SELECT, ASK or CONSTRUCT query that a view could be, as server-sent events: `snapshot` pages of the result, then a `diff` after each commit that changes it, `reset` when the server evaluated the query again, and `error` when the stream ends. Each event's id is a commit.")
            .see("live-queries")
            .query("query", s(), "The query.")
            .query("after", s(), "Resume after this commit (`commit:N`): the first event is the change since it, or a `reset`.")
            .query("coalesceMs", json!({ "type": "integer", "minimum": 0, "default": 0 }), "Merge the diffs of commits that arrive within this many milliseconds.")
            .header("Last-Event-ID", s(), "Resume after this commit.")
            .resp("200", "The events.", Some(json!({ "text/event-stream": text() })))
            .errors(&[400, 403, 429]),
    );
    for (m, id, summary) in [
        (GET, "runStoredQuery", "Run a stored query"),
        (POST, "runStoredQueryPost", "Run a stored query (POST)"),
    ] {
        let mut o = op(m.clone(), "/{ds}/queries/{name}", id, "SPARQL", summary)
            .doc("The query's parameters are request parameters by name (`minAge=40` or `$minAge=40`). The run takes the parameters of `/{ds}/sparql`, and `version`.")
            .see("stored-queries")
            .params(&["format", "timeout", "reasoning", "nocache", "at", "send", "execution"])
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

fn mappings(p: &mut Paths) {
    let text_kinds = || {
        json!({
            "text/turtle": text(),
            "application/yaml": text(),
            "application/csvm+json": text(),
            "application/sparql-query": text(),
        })
    };
    p.add(
        op(
            GET,
            "/$/mappings/{ds}",
            "listStoredMappings",
            "Stored mappings",
            "List stored mappings",
        )
        .see("stored-mappings")
        .json(
            "200",
            "The mappings without their text.",
            "StoredMappingList",
        ),
    );
    p.add(
        op(GET, "/$/mappings/{ds}/{name}", "getStoredMapping", "Stored mappings", "Get a stored mapping")
            .doc("The mapping with its text and metadata, or the text alone when `Accept` names the kind's media type.")
            .see("stored-mappings")
            .query("version", int(), "An older version, while it is kept.")
            .resp_h(
                "200",
                "The mapping.",
                Some({
                    let mut c = text_kinds();
                    c["application/json"] = json!({ "schema": sref("StoredMapping") });
                    c
                }),
                json!({ "ETag": { "description": "`\"v<N>\"`", "schema": { "type": "string" } } }),
            )
            .errors(&[404]),
    );
    p.add(
        op(PUT, "/$/mappings/{ds}/{name}", "putStoredMapping", "Stored mappings", "Store a mapping")
            .doc("Stores the mapping text as the next version, after parsing and checking it. `Content-Type` names the kind: `text/turtle` for R2RML and RML, `application/yaml` for YARRRML, `application/csvm+json` for CSVW metadata and `application/sparql-query` for a CONSTRUCT template. A connection the server does not declare is a warning. `If-Match: \"v<N>\"` stores only over version N, and `If-None-Match: *` only when the mapping does not exist.")
            .see("stored-mappings")
            .params(&["ifMatch", "ifNoneMatch", "commitMessage"])
            .query("kind", str_enum(&["rml", "yarrrml", "csvw", "template"]), "The kind, instead of the media type.")
            .query("description", s(), "A description of at most 1024 bytes.")
            .body(true, "The mapping text.", text_kinds())
            .json("200", "Stored as a new version, or unchanged (`changed: false`).", "StoredMapping")
            .json("201", "Created.", "StoredMapping")
            .errors(&[400, 409, 412, 415]),
    );
    p.add(
        op(
            DELETE,
            "/$/mappings/{ds}/{name}",
            "deleteStoredMapping",
            "Stored mappings",
            "Delete a stored mapping",
        )
        .see("stored-mappings")
        .param("ifMatch")
        .no_content("Deleted with its versions.")
        .errors(&[404, 412]),
    );
    p.add(
        op(
            GET,
            "/$/mappings/{ds}/{name}/versions",
            "listStoredMappingVersions",
            "Stored mappings",
            "List the versions of a stored mapping",
        )
        .see("stored-mappings")
        .json(
            "200",
            "The kept versions, newest first.",
            "StoredMappingVersions",
        )
        .errors(&[404]),
    );
    let o = op(POST, "/{ds}/mappings/{name}/run", "runStoredMapping", "Stored mappings", "Run a stored mapping")
        .doc("Runs the mapping over the file parts its sources name and the server's connections, and commits once. `mode=replace` replaces the constant graphs the mapping writes in that commit, and is `400` with code `mapping-graphs-unknown` for a template graph map. The commit message defaults to `mapping NAME (version V)`, and the mapping records the run.")
        .see("stored-mappings")
        .query("mode", str_enum(&["add", "replace"]), "Add the triples (the default), or replace the graphs the mapping writes.")
        .query("version", int(), "Run an older version.")
        .query("graph", s(), "Write every triple into this graph. A mapping whose graph maps name another graph is `400` with code `mapping-graph-conflict`.")
        .query("base", s(), "The base IRI of generated relative IRIs.")
        .query("lookup", boolean(), "A CONSTRUCT template: read the dataset's head while mapping.")
        .query("csvwMode", str_enum(&["minimal", "standard"]), "A CSVW mapping: standard mode.")
        .query("csvwCheck", str_enum(&["keys", "none"]), "A CSVW mapping: check primary and foreign keys.")
        .params(&["ifMatch"])
        .body(
            false,
            "The files the mapping reads, each a part whose file name is the one the mapping names.",
            json!({ "multipart/form-data": { "schema": {
                "type": "object",
                "additionalProperties": true,
                "properties": { "file": { "type": "array", "items": { "type": "string", "contentMediaType": "application/octet-stream" } } },
            } } }),
        );
    let mut o = write_params(o).json(
        "200",
        "The run's counts, with the commit receipt when asked for.",
        "MappingRun",
    );
    o = write_errors(o);
    p.add(o.errors(&[404]));
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

fn branches(p: &mut Paths) {
    p.add(
        op(
            GET,
            "/$/branches/{ds}",
            "listBranches",
            "Branches",
            "List branches",
        )
        .doc("The branches the caller may read, `main` first, then by name.")
        .see("branches-and-merges")
        .json("200", "The branches.", "BranchList")
        .errors(&[501]),
    );
    p.add(
        op(PATCH, "/$/branches/{ds}", "updateBranchSettings", "Branches", "Change the dataset's merge settings")
            .doc("`exemptPredicates`: the predicates whose cells never conflict in the dataset's merges, reverts and cherry-picks. Both sides' changes to them are kept, as with the `quad` scope. Needs admin on `main`.")
            .see("merges")
            .json_body(true, "BranchSettings")
            .json("200", "The settings.", "BranchSettings")
            .errors(&[400, 403, 501]),
    );
    p.add(
        op(POST, "/$/branches/{ds}", "createBranch", "Branches", "Create a branch")
            .doc("Creates a branch from a commit of another branch. The branch shares the index files of the generation that holds the commit until it compacts, so creating one writes a few kilobytes. Needs read on the source branch and write on the new one, from grants without graph restrictions.")
            .see("branches-and-merges")
            .json_body(true, "BranchCreate")
            .json("201", "The branch.", "Branch")
            .errors(&[400, 404, 409, 501]),
    );
    p.add(
        op(
            GET,
            "/$/branches/{ds}/{name}",
            "getBranch",
            "Branches",
            "Get a branch",
        )
        .see("branches-and-merges")
        .json("200", "The branch.", "Branch")
        .errors(&[404]),
    );
    p.add(
        op(PATCH, "/$/branches/{ds}/{name}", "updateBranch", "Branches", "Rename, protect or annotate a branch")
            .doc("`name` renames the branch: it keeps its id, commits, storage and children, and the old name answers `404` afterwards. A rename needs write on the old and the new name, admin when the branch is protected, and grants without graph restrictions. Its answer carries `Location` and `grantsChanged`, the number of configured grants that cover one of the two names and not the other. `protected` needs admin on the branch. A protected branch takes changes through merges only.")
            .see("branches-and-merges")
            .json_body(true, "BranchPatch")
            .json("200", "The branch.", "Branch")
            .errors(&[400, 403, 404]),
    );
    p.add(
        op(DELETE, "/$/branches/{ds}/{name}", "deleteBranch", "Branches", "Delete a branch")
            .doc("Deletes the branch's commits, snapshots and storage. Refused with `409 unmerged` while it has commits its upstream does not have, unless `force=true`, and with `409 has-children` while other branches start from it, unless `reparent=true`. With `reparent=true` those branches take the deleted branch's upstream as theirs, and its storage stays while their history needs it. A branch that an evidence hold names is refused with `409 branch-held` unless `releaseHolds=true`, which needs `admin` and releases those holds first.")
            .see("branches-and-merges")
            .query("force", boolean(), "Delete a branch with unmerged commits.")
            .query("reparent", boolean(), "Delete a branch that other branches start from, and re-parent them.")
            .query("releaseHolds", boolean(), "Release the source holds that name the branch, then delete it. Needs `admin`.")
            .resp("204", "Deleted.", None)
            .errors(&[400, 403, 404, 409, 503]),
    );
    p.add(
        op(POST, "/$/branches/{ds}/{name}/relink", "relinkBranch", "Branches", "Relink a branch to main's index")
            .doc("Moves a persistent branch that still links to an upstream's index onto the index of `main`'s current generation. The branch's changes since its start become a sparse overlay on that index, and its id, head, commits, snapshots and state stay as they were. Writes to the branch go on during the relink and are carried into the new generation. A relink never happens on its own, and ordinary compaction still gives a branch an index of its own. Needs admin on the branch. Relinking `main` answers `400 invalid-branch`, and a branch that owns its index or belongs to an in-memory dataset answers `409 not-relinkable`. The body is empty or `{}`.")
            .see("relinking")
            .header("Prefer", s(), "`respond-async` runs it as a cancellable task with progress: `202` with the task and `Location: /$/tasks/{id}`. The task's `detail` is the result.")
            .resp("202", "Started as a task (`Prefer: respond-async`).", Some(json!({ "application/json": { "schema": sref("Task") } })))
            .json("200", "The relink.", "RelinkResult")
            .errors(&[400, 403, 404, 409, 503]),
    );
    p.add(
        analysis_params(op(GET, "/$/merge/{ds}", "previewMerge", "Branches", "Preview a merge"))
            .query("entailments", boolean(), "Include reasoning changes for the complete merged candidate. Unresolved conflicts mark entailments unavailable.")
            .query("timeout", json!({"type":"number","exclusiveMinimum":0}), "Analysis timeout in seconds, capped at the server timeout.")
            .doc("What merging `source` into `target` would do: the merge base, the changes and the conflicts. Nothing is written, and conflicts do not fail the request.")
            .see("merges")
            .query_req("source", s(), "The branch to merge.")
            .query("target", s(), "The branch to merge into (default `main`).")
            .query("conflicts", json!({ "type": "string", "enum": ["cell", "subject", "quad"] }), "What counts as one value (default `cell`).")
            .query("onConflict", json!({ "type": "string", "enum": ["fail", "ours", "theirs", "union"] }), "The rule for conflicts.")
            .query("squash", boolean(), "Preview a squash merge.")
            .query("exempt", s(), "A predicate IRI exempt from conflicts in this merge; repeatable.")
            .query("limit", int(), "The most conflict cells listed (default 100, at most 10000).")
            .json("200", "The preview.", "MergeResult")
            .errors(&[400, 404, 409, 410, 507]),
    );
    p.add(
        op(POST, "/$/merge/{ds}", "merge", "Branches", "Merge a branch")
            .doc("Merges `source` into `target` as one commit of kind `merge`: a fast-forward when the target has not moved since the merge base, a three-way merge of quad sets otherwise. With `ff: \"replay\"` and a target that holds the merge base's state, each commit of the source after the base is replayed as its own commit on the target with its kind, message and author, and records the commit it replays. With `squash: true` the changes are one commit of kind `merge` that records no second parent, so the target does not descend from the source, and a squash that changes nothing makes no commit. Conflicts that resolutions and `onConflict` leave answer `409` with the conflict report, and nothing is written. `dryRun: true` answers the write preview of the merge commit with the merge fields under `merge`.")
            .see("merges")
            .header("Prefer", s(), "`respond-async` runs it as a cancellable task: `202` with the task and `Location: /$/tasks/{id}`.")
            .resp("202", "Started as a task (`Prefer: respond-async`).", Some(json!({ "application/json": { "schema": sref("Task") } })))
            .json_body(true, "MergeRequest")
            .json("200", "The merge.", "MergeResult")
            .resp(
                "409",
                "Conflicts remain (`merge-conflict`, with the report), the target moved (`head-moved`), not a fast-forward (`not-fast-forward`), or several merge bases (`ambiguous-merge-base`).",
                Some(json!({ "application/json": { "schema": sref("ConflictReport") } })),
            )
            .errors(&[400, 403, 404, 410, 422, 507]),
    );
    p.add(
        op(GET, "/$/revert/{ds}", "previewRevert", "Branches", "Preview a revert")
            .doc("What reverting commit `commit` of branch `branch` would do: the changes and the conflicts. Nothing is written, and conflicts do not fail the request.")
            .see("reverts-and-cherry-picks")
            .query("branch", s(), "The branch whose history holds the commit, and that the revert writes to (default `main`).")
            .query_req("commit", int(), "The commit to revert.")
            .query("conflicts", json!({ "type": "string", "enum": ["cell", "subject", "quad"] }), "What counts as one value (default `cell`).")
            .query("onConflict", json!({ "type": "string", "enum": ["fail", "ours", "theirs", "union"] }), "The rule for conflicts.")
            .query("limit", int(), "The most conflict cells listed (default 100, at most 10000).")
            .json("200", "The preview.", "MergeResult")
            .errors(&[400, 404, 409, 410, 507]),
    );
    p.add(
        op(POST, "/$/revert/{ds}", "revert", "Branches", "Revert a commit")
            .doc("Undoes commit `commit` of branch `branch`'s history with one commit of kind `revert` on that branch: a three-way merge of the commit's parent into the branch, with the commit as the merge base. Later changes to the same cells conflict as in a merge. The body is optional and takes the merge options that apply. A revert that would change nothing makes no commit and answers `upToDate: true`. Needs write on the branch through the `merge` endpoint, and a protected branch refuses it.")
            .see("reverts-and-cherry-picks")
            .query("branch", s(), "The branch whose history holds the commit, and that the revert writes to (default `main`).")
            .query_req("commit", int(), "The commit to revert.")
            .header("Prefer", s(), "`respond-async` runs it as a cancellable task: `202` with the task and `Location: /$/tasks/{id}`.")
            .resp("202", "Started as a task (`Prefer: respond-async`).", Some(json!({ "application/json": { "schema": sref("Task") } })))
            .json_body(false, "PickRequest")
            .json("200", "The revert, with `reverted`.", "MergeResult")
            .resp(
                "409",
                "Conflicts remain (`merge-conflict`, with the report), or the branch moved (`head-moved`).",
                Some(json!({ "application/json": { "schema": sref("ConflictReport") } })),
            )
            .errors(&[400, 403, 404, 410, 422, 507]),
    );
    p.add(
        op(GET, "/$/cherry-pick/{ds}", "previewCherryPick", "Branches", "Preview a cherry-pick")
            .doc("What applying commit `commit` of branch `source`'s history to branch `branch` would do: the changes and the conflicts. Nothing is written, and conflicts do not fail the request.")
            .see("reverts-and-cherry-picks")
            .query_req("source", s(), "The branch whose history holds the commit.")
            .query_req("commit", int(), "The commit to apply.")
            .query("branch", s(), "The branch the commit is applied to (default `main`).")
            .query("conflicts", json!({ "type": "string", "enum": ["cell", "subject", "quad"] }), "What counts as one value (default `cell`).")
            .query("onConflict", json!({ "type": "string", "enum": ["fail", "ours", "theirs", "union"] }), "The rule for conflicts.")
            .query("limit", int(), "The most conflict cells listed (default 100, at most 10000).")
            .json("200", "The preview.", "MergeResult")
            .errors(&[400, 404, 409, 410, 507]),
    );
    p.add(
        op(POST, "/$/cherry-pick/{ds}", "cherryPick", "Branches", "Cherry-pick a commit")
            .doc("Applies the changes of commit `commit` of branch `source`'s history to branch `branch` as one commit of kind `cherry-pick`: a three-way merge of the commit into the branch, with the commit's parent as the merge base. It records no second parent. The body is optional and takes the merge options that apply. A cherry-pick whose changes the branch already holds makes no commit and answers `upToDate: true`. Needs read on the source and write on the branch through the `merge` endpoint, and a protected branch refuses it.")
            .see("reverts-and-cherry-picks")
            .query_req("source", s(), "The branch whose history holds the commit.")
            .query_req("commit", int(), "The commit to apply.")
            .query("branch", s(), "The branch the commit is applied to (default `main`).")
            .header("Prefer", s(), "`respond-async` runs it as a cancellable task: `202` with the task and `Location: /$/tasks/{id}`.")
            .resp("202", "Started as a task (`Prefer: respond-async`).", Some(json!({ "application/json": { "schema": sref("Task") } })))
            .json_body(false, "PickRequest")
            .json("200", "The cherry-pick, with `picked`.", "MergeResult")
            .resp(
                "409",
                "Conflicts remain (`merge-conflict`, with the report), or the branch moved (`head-moved`).",
                Some(json!({ "application/json": { "schema": sref("ConflictReport") } })),
            )
            .errors(&[400, 403, 404, 410, 422, 507]),
    );
    p.add(
        op(GET, "/$/commit-graph/{ds}", "getCommitGraph", "Branches", "Get the commit graph")
            .doc("The own commits of several branches, newest first by time, each with the branch that made it and its parents: the previous commit, or the starting commit for a branch's first commit, and a merge commit's merged commit. The answer also lists each branch's head, upstream and starting commit. Only the branches the caller may read are drawn.")
            .see("commit-graph")
            .query("branches", s(), "The branches to draw, comma-separated or repeated (default every branch the caller may read).")
            .query(
                "limit",
                json!({ "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }),
                "The page size.",
            )
            .query("before", s(), "The cursor of the next page, as `next` gives it.")
            .json("200", "One page of the graph.", "CommitGraph")
            .paginated("cursor", &["limit", "before"], "next")
            .errors(&[400, 403, 404, 501]),
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
        .doc("Newest first, or oldest first after `after`. On a branch other than `main`, its own commits come first, then those it shares with its upstream; each commit names the branch that made it.")
        .see("commits")
        .param("branch")
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
            "/$/commits/{ds}/timeline",
            "commitTimeline",
            "History",
            "Commit timeline",
        )
        .doc("The commits of the branch in buckets of consecutive numbers, with the commits' times, kinds and quad counts, and the ranges whose state can still be read. For a time slider over a long history.")
        .see("commits")
        .param("branch")
        .query(
            "buckets",
            json!({ "type": "integer", "minimum": 1, "maximum": 2000, "default": 200 }),
            "The most buckets.",
        )
        .query("from", int(), "The first commit number; the first by default.")
        .query("to", int(), "The last commit number; the head by default.")
        .json("200", "The buckets.", "CommitTimeline")
        .errors(&[400]),
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
            .query(
                "term",
                json!({ "type": "array", "items": { "type": "string" }, "maxItems": 1000 }),
                "Only the changes whose subject or object is this term, an N-Triples term or a bare IRI. Repeat the parameter for several, up to 1,000. Not for RDF Patch.",
            )
            .query("quads", boolean(), "JSON: list the quads.")
            .query(
                "group",
                json!({ "type": "string", "enum": ["class"] }),
                "`class`: count the changes by the class of the subject, the predicate and the class or kind of the object, without the quads. Classes are read under the caller's view, at the `to` state for an added quad and at the `from` state for a removed one. JSON only, and not with `quads=true`.",
            )
            .query("limit", int(), "The most quads listed, or with `group=class` the most groups, from 1 to 5,000 (default 5,000).")
            .query("fromBranch", s(), "A diff across branches: the branch `from` is read on, its head by default.")
            .query("toBranch", s(), "A diff across branches: the branch `to` is read on.")
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
        op(POST, "/{ds}/sparql/diff", "sparqlDiff", "History", "Diff a query between two states")
            .doc("Runs one SELECT query at two states and returns the rows only one of them has, as bags of their terms. The server keeps a hash for each row of the earlier result and no row, then reads the later result against them and the earlier query once more to list up to `limit` differing rows of each side. The request counts twice against the `query` rate limit. A query that returns different rows when it runs again is refused with `409`.")
            .see("comparing-a-query-between-two-states")
            .query("from", s(), "A selector of the earlier state.")
            .query("to", s(), "A selector of the later state; the head by default.")
            .query("limit", json!({ "type": "integer", "minimum": 1, "maximum": 10000, "default": 100 }), "The most differing rows listed for each side.")
            .params(&["branch", "reasoning"])
            .body(
                true,
                "The SELECT query.",
                json!({
                    "application/sparql-query": { "schema": { "type": "string" } },
                    "application/x-www-form-urlencoded": { "schema": { "type": "object", "required": ["query"], "properties": { "query": { "type": "string" } } } },
                    "application/json": { "schema": { "type": "object", "required": ["query"], "properties": { "query": { "type": "string" }, "from": { "type": "string" }, "to": { "type": "string" }, "limit": { "type": "integer" } } } },
                }),
            )
            .json("200", "The differing rows and the counts.", "SparqlDiff")
            .errors(&[400, 408, 409, 410, 507]),
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
        .doc("Formats a SPARQL query or update, or a Turtle, TriG, N-Triples, N-Quads, JSON-LD or SHACL Compact Syntax (SHACLC) document. A JSON body gets a `FormatResult`. A raw body is answered with the formatted text in the same media type. `--format-endpoint` may limit or turn off the route.")
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
        "text/shaclc",
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
        op(
            POST,
            "/$/vector/{ds}/{name}/projection",
            "computeVectorProjection",
            "Search",
            "Compute a projection of a vector index",
        )
        .doc("Computes a two-dimensional projection of the index's entities at the head and keeps it in memory for `GET`. `pca` projects every vector on the two leading principal components of a sample, and an entity with several vectors sits at the mean of their projections. It runs in the request for an index of up to 1,000,000 vectors and as a task above that. `neighbours` lays out the index's HNSW neighbour graph from the PCA positions and always runs as a task. The server keeps four projections, 1 GiB in all, charged to the query memory pool, and refuses one it cannot hold with `507`. A caller whose access covers part of the dataset gets `403`.")
        .see("embedding-map")
        .query("method", json!({ "type": "string", "enum": ["pca", "neighbours"], "default": "pca" }), "How the vectors are placed.")
        .param("timeout")
        .body(
            false,
            "The sample, seed and layout steps.",
            json!({ "application/json": { "schema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "sample": { "type": "integer", "minimum": 1, "maximum": 100000, "description": "Vectors the PCA learns from." },
                    "seed": { "type": "integer", "minimum": 0, "description": "The seed of the first directions, the sampling priorities and the layout." },
                    "iterations": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Steps of the neighbour layout." }
                }
            } } }),
        )
        .json_inline("200", "The projection, without its points.", projection_summary())
        .task()
        .errors(&[400, 403, 404, 408, 503, 507]),
    );
    p.add(
        op(
            GET,
            "/$/vector/{ds}/{name}/projection",
            "getVectorProjection",
            "Search",
            "Read a kept projection of a vector index",
        )
        .doc("Returns the kept projection's method, commit, bounds, a density grid of 256 by 256 cells, its classes, and up to 50,000 points as `{entity, x, y, class?, colour?}`. `bbox` returns the points inside a region and its density, at most `limit`, those of lowest sampling priority when there are more, so zooming in keeps the points already shown. `colour` names a predicate whose first value per point is returned as an index into `colour.values`. `Accept: application/x-sparkles-points` returns the binary points format of the embedding map guide. A projection read at an older commit than the head has `stale: true`.")
        .see("embedding-map")
        .query("method", json!({ "type": "string", "enum": ["pca", "neighbours"] }), "The projection's method. The most recent of any method when absent.")
        .query("bbox", json!({ "type": "string" }), "`minX,minY,maxX,maxY`.")
        .query("limit", json!({ "type": "integer", "minimum": 0, "maximum": 50000, "default": 50000 }), "The most points to return.")
        .query("colour", json!({ "type": "string" }), "A predicate IRI whose values colour the points.")
        .resp(
            "200",
            "The projection and its points.",
            Some(json!({
                "application/json": { "schema": {
                    "type": "object",
                    "required": ["index", "method", "commit", "stale", "entities", "bounds", "region", "density", "classes", "points"],
                    "properties": {
                        "index": { "type": "string" },
                        "method": { "type": "string" },
                        "commit": { "type": "integer" },
                        "generation": { "type": "string" },
                        "stale": { "type": "boolean", "description": "The head has moved since the projection was computed." },
                        "vectors": { "type": "integer" },
                        "dimension": { "type": "integer" },
                        "entities": { "type": "integer", "description": "Points in the whole projection." },
                        "bounds": { "type": "array", "items": { "type": "number" }, "description": "`[minX, minY, maxX, maxY]`." },
                        "explained": { "type": "array", "items": { "type": "number" }, "description": "The share of the sample's variance on each component." },
                        "builtMs": { "type": "number" },
                        "bytes": { "type": "integer" },
                        "region": { "type": "object", "description": "`{bbox, total, shown, sampled}`: the points inside the region and how many are returned." },
                        "density": { "type": "object", "description": "`{size, bbox, max, counts}`, where `counts` is base64 of little-endian `Uint32` cell counts, row by row from the minimum y." },
                        "classes": { "type": "array", "items": { "type": "object", "description": "`{iri, count}`." } },
                        "colour": { "type": "object", "description": "`{predicate, values}`, where `values` holds `{value, count}` with a term in the SPARQL JSON results form." },
                        "points": { "type": "array", "items": { "type": "object", "description": "`{entity, x, y, class?, colour?}`, where `class` and `colour` index `classes` and `colour.values`." } }
                    }
                } },
                "application/x-sparkles-points": { "schema": { "type": "string", "format": "binary" } }
            })),
        )
        .errors(&[400, 403, 404]),
    );
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
        op(POST, "/$/lint", "lint", "Formatting", "Lint SPARQL or RDF")
            .doc("Lints a SPARQL query or update, or a Turtle, TriG, N-Triples, N-Quads, JSON-LD or SHACLC document, with the rules of `sparkles lint`. The SPARQL in `sh:select`, `sh:ask`, `sh:construct` and `sh:update` literals is linted too. A syntax error is one of the findings, so the answer is `200` either way. `--format-endpoint` may limit or turn off the route.")
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
        mcp(POST, "mcpPost", "Send an MCP message", "One JSON-RPC message of the Model Context Protocol's Streamable HTTP transport. `404` unless the server runs with `--mcp`. With an `[oidc]` provider the endpoint is an OAuth protected resource. A `401` then names the protected-resource metadata in `WWW-Authenticate`, and a `403` with `error=\"insufficient_scope\"` names the scope an access token lacks for this message.")
            .header("Mcp-Method", s(), "The JSON-RPC method (stateless requests).")
            .header("Mcp-Name", s(), "The tool, resource or prompt name, or the task id (stateless requests).")
            .body(true, "A JSON-RPC message.", json!({ "application/json": { "schema": { "type": "object" } } }))
            .resp(
                "200",
                "The JSON-RPC response, or an SSE stream for `subscriptions/listen` and the requests of a legacy session.",
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

fn assist(p: &mut Paths) {
    let tool = |route: &'static str, id: &str, summary: &str, d: &str, req: &str, res: &str| {
        op(POST, route, id, "SPARQL", summary)
            .doc(d)
            .see("checking-and-explaining-queries")
            .body(
                true,
                "",
                json!({ "application/json": { "schema": sref(req) } }),
            )
            .json("200", "The result.", res)
            .errors(&[400, 404, 408])
    };
    p.add(tool(
        "/{ds}/check",
        "checkQuery",
        "Check a query against the schema",
        "The MCP tool `check_query` as the caller, over the caller's view. With `terms`, every constant IRI of the query is listed with its kind, label, count and whether it occurs.",
        "CheckRequest",
        "CheckResult",
    ));
    p.add(tool(
        "/{ds}/sparql/diagnose",
        "diagnoseQuery",
        "Explain an empty result",
        "The MCP tool `why_empty`: each triple pattern alone, then the patterns joined in order with the filters, each an ASK under a tenth of the timeout. The answer names the first pattern, join or filter without solutions.",
        "DiagnoseRequest",
        "Diagnosis",
    ));
    p.add(tool(
        "/{ds}/recall",
        "recallFacts",
        "Recall facts",
        "The MCP tool `recall` in its JSON format: the facts around the seeds or the entities a search finds, with citations and, when the dataset names agent memory graphs, the review status of each fact.",
        "RecallRequest",
        "RecallResult",
    ));
    p.add(
        op(POST, "/{ds}/facts", "assertFacts", "SPARQL", "Write facts with provenance")
            .doc("The MCP tool `assert_facts` as the caller, with its checks. Needs `write` on the graphs written and counts as an update. `sparkles memory import` and `assert` call it.")
            .see("importing-agent-memory")
            .json_body(true, "AssertFactsRequest")
            .json("200", "The result.", "AssertFactsResult")
            .errors(&[400, 403, 404, 408, 409, 422]),
    );
    p.add(
        op(POST, "/{ds}/claims/resolve", "resolveEvidence", "SPARQL", "Verify evidence references")
            .doc("Needs `read` on the dataset through the query endpoint, and counts as a query. Verifies each reference in order: a stored fact at the exact commit of the branch its identities name, read through the caller's view of that dataset and branch, and a record as an authenticated record of the dataset's protected evidence graphs at the request branch's current state. The first reference that does not verify is answered with its `code` and `reference` key: `422 evidence-unavailable` for a missing, unretained or unreadable reference alike, `422 incomplete-evidence` when the 64 MiB resolution limit or the caller's access policy stops verification, `422 unanchored-evidence`, `422 span-mismatch`, `422 origin-not-reproduced` and `409 immutable-record-conflict`. A malformed reference is a `400` that names its field.")
            .see("evidence-resolution")
            .json_body(true, "EvidenceResolveRequest")
            .json("200", "Every reference, verified.", "EvidenceResolveResult")
            .errors(&[400, 403, 404, 408, 409, 413, 422]),
    );
    p.add(
        op(POST, "/{ds}/claims", "proposeClaims", "SPARQL", "Propose claims with their evidence")
            .doc("Needs `write` on the dataset or branch through the update endpoint, update grants on every graph the batch writes, and read access to every record and stored fact it references, decided as `POST /{ds}/claims/resolve` decides it. Counts as an update. Publishes the batch's source revisions, renditions and passages in the evidence settings' source-record graph and its references, claims, links and assessments in the proposal graph, as one commit of the evidence owner. Claims describe their propositions and never assert them. The server sets each record's receipt, creation activity and record digest, a source revision's `firstAvailableAt` and the operation that the authenticated caller is the agent of. A repeated `idempotencyKey` with the same payload answers the first receipt with `replayed` and writes nothing. Refusals write nothing: `400` for a malformed or inconsistent batch, naming its field, `403` for a written graph the caller may not update, `409 head-changed`, `409 idempotency-conflict`, `409 immutable-record-conflict` for a record identity that holds other content or no authenticated record, `409 unsupported-profile`, the reference outcomes of `POST /{ds}/claims/resolve`, and `503 authority-changed` when the caller's credential was revoked or the access policy changed before publication.")
            .see("evidence-proposals")
            .json_body(true, "EvidenceProposal")
            .json("200", "The operation and its records, published, replayed or previewed.", "EvidenceProposalResult")
            .errors(&[400, 403, 404, 408, 409, 413, 422, 503]),
    );
    p.add(
        op(POST, "/{ds}/claims/review", "reviewClaims", "SPARQL", "Review claims under a policy")
            .doc("Needs `write` on the dataset or branch through the update endpoint, update and read grants on the policy's decisions, supports and output graphs, and read access to each target and, for an acceptance, to every evidence link of the target with its reference and what that names and to every negative claim of the same proposition in the policy's evidence boundary. Such a record the caller cannot read answers exactly as an unreadable target, `422 evidence-unavailable`. Counts as an update. Appends one immutable decision per target in the policy's decisions graph as one commit of the evidence owner, with the caller as the reviewer. An acceptance of a positive claim of an admitted kind with the required scope records an acceptance support and projects the claim's proposition into the policy's output graph. A projected triple stays while one live support owns it, and a decision that supersedes an acceptance ends its support. Every read is made at `ifHead`. Under a matching policy the targets are identity proposals and the policy's resolution map follows the decisions: an operation that changes which accepted decisions it follows writes one immutable map revision in the policy's maps graph and rewrites the clusters it changed in the output graph, and needs update and read grants on the decisions, maps and output graphs and read access to every effective proposal of those clusters. Refusals write nothing: `400` for a malformed request or an unknown policy, `403` for a graph the caller may not update or read, `409 policy-changed`, `409 head-changed`, `409 target-changed`, `409 decision-superseded`, `409 target-withdrawn`, `409 evidence-conflict` for contrary evidence under a policy that refuses conflicts, `409 identity-conflict` when an accepted separation would share a cluster, `409 idempotency-conflict`, `422 evidence-unavailable`, `422 incomplete-evidence`, `503 projection-unavailable` when a graph of the policy's projection was detached and protected again, and `503 authority-changed`.")
            .see("evidence-reviews")
            .json_body(true, "EvidenceReview")
            .json("200", "The operation and its decisions, published, replayed or previewed.", "EvidenceReviewResult")
            .errors(&[400, 403, 404, 408, 409, 413, 422, 503]),
    );
    p.add(
        op(POST, "/{ds}/claims/withdraw", "withdrawClaims", "SPARQL", "Withdraw claim occurrences")
            .doc("Needs `write` on the dataset or branch through the update endpoint, update grants on each claim's graph, and update and read grants on the output graph and read grants on the decisions and supports graphs of every review policy. Counts as an update. Records a withdrawal with `prov:invalidatedAtTime` in each claim's graph as one commit of the evidence owner, without making a contrary claim. The claims' supports stop being live, and each projected triple left without a live support leaves its output graph. A claim that is already withdrawn is answered as `existing`. Refusals write nothing: `400`, `403`, `409 head-changed`, `409 target-changed`, `409 idempotency-conflict`, `422 evidence-unavailable`, `503 projection-unavailable` and `503 authority-changed`.")
            .see("evidence-reviews")
            .json_body(true, "EvidenceWithdrawal")
            .json("200", "The operation and its withdrawals, published, replayed or previewed.", "EvidenceWithdrawalResult")
            .errors(&[400, 403, 404, 408, 409, 413, 422, 503]),
    );
    p.add(
        op(POST, "/{ds}/claims/identities", "resolveIdentities", "SPARQL", "Resolve entities through a resolution map")
            .doc("Needs `read` on the dataset or branch through the query endpoint, and read grants on the matching policy's decisions, maps and output graphs and on the identity graphs of its boundary. Counts as a query. Answers what the policy's current resolution map selects for each entity: its representative, the members of its cluster and the current accepted decisions that join it. An entity the map does not name resolves to itself. The map is ordinary RDF in the policy's output graph, `<member> ev:resolvesTo <representative>`, which queries join explicitly. Refusals: `400` for a malformed request or an unknown policy, `403` for a graph the caller may not read, `422 incomplete-evidence` when a cluster's records exceed 10,000, and `503 projection-unavailable` when a graph of the policy was detached and protected again or an identity graph the map was built from left the boundary.")
            .see("evidence-identities")
            .json_body(true, "EvidenceIdentities")
            .json("200", "The map's revision and each entity's resolution.", "EvidenceIdentitiesResult")
            .errors(&[400, 403, 404, 408, 413, 422, 503]),
    );
    p.add(
        op(POST, "/{ds}/claims/dependents", "listEvidenceDependents", "SPARQL", "Find the assessments that depend on changed records")
            .doc("Needs `read` on the dataset or branch through the query endpoint, and counts as a query. Answers the authenticated assessments in the proposal graphs the caller can read whose `ev:target` or `prov:used` names one of the changed record identities, and with `transitive` the assessments that depend on those, each once at its smallest depth. A graph the caller cannot read is not searched, and an assessment any of whose triples the caller's view hides is neither reported nor followed, so the answer discloses nothing hidden. A review or withdrawal names its changed identities in `changed`, its decisions and its withdrawals. Refusals: `400` for a malformed request and `422 incomplete-evidence` when more than 10,000 candidate records would be read.")
            .see("evidence-dependents")
            .json_body(true, "EvidenceDependents")
            .json("200", "The dependent assessments.", "EvidenceDependentsResult")
            .errors(&[400, 403, 404, 408, 413, 422]),
    );
    p.add(
        op(POST, "/{ds}/sources", "registerSource", "SPARQL", "Register a source document")
            .doc("The MCP tool `register_source` as the caller. Needs `write` on the graph and counts as an update. With `original` the file's bytes are kept beside the normalized text, `reanchor` moves the facts that cite the previous rendition to the new text, and `reanchorFrom` copies the facts of a renamed file's source. In a graph under the dataset's import base, a text that matches a secret pattern is refused with 422 `secret-detected`, the pattern's name and the offset. `sparkles memory import` calls it.")
            .see("importing-agent-memory")
            .json_body(true, "RegisterSourceRequest")
            .json("200", "The source and its rendition.", "RegisterSourceResult")
            .errors(&[400, 403, 404, 408, 409, 422]),
    );
    p.add(
        op(GET, "/{ds}/sources", "listSources", "SPARQL", "List registered sources")
            .doc("The MCP tool `list_sources` as the caller: the sources in the graphs the caller can read, with their current rendition, chunk and fact counts, and whether the rendition still needs extraction. Needs `read` and counts as a query.")
            .see("importing-agent-memory")
            .query("graph", s(), "Only sources in this graph; repeat for several, up to 20.")
            .query("graphPrefix", s(), "Only sources in graphs whose IRI starts with this.")
            .query("needsExtraction", boolean(), "Only sources whose current rendition no extraction has cited.")
            .query("limit", json!({ "type": "integer", "minimum": 1, "maximum": 500, "default": 50 }), "The most sources listed.")
            .query("atCommit", int(), "Read at this commit.")
            .json("200", "The sources.", "SourceList")
            .errors(&[400, 403, 404, 408]),
    );
    p.add(
        op(POST, "/{ds}/memory/brief", "memoryBrief", "SPARQL", "Brief what the graph knows")
            .doc("The brief of C18 §8.10.9 for a project's import graphs, an entity or a recall query: reviewed facts by default, ranked by age and corroboration, bounded in facts and characters, with a citation per source.")
            .see("importing-agent-memory")
            .json_body(true, "BriefRequest")
            .json("200", "The brief.", "BriefResult")
            .errors(&[400, 404, 408, 422]),
    );
    p.add(
        op(
            GET,
            "/$/memory/{ds}",
            "getMemorySettings",
            "Datasets",
            "Get the memory settings",
        )
        .see("memory-settings")
        .json("200", "The effective settings.", "MemorySettings"),
    );
    p.add(
        op(
            PUT,
            "/$/memory/{ds}",
            "putMemorySettings",
            "Datasets",
            "Set the memory settings",
        )
        .doc("Makes the effective memory settings equal to the body, as `PUT /$/settings/{ds}/memory` does.")
        .see("memory-settings")
        .json_body(true, "MemorySettings")
        .json("200", "The effective settings.", "MemorySettings")
        .errors(&[400]),
    );
    p.add(
        op(
            GET,
            "/$/evidence/{ds}/settings",
            "getEvidenceSettings",
            "Datasets",
            "Get the evidence settings",
        )
        .doc("Needs `admin` on the dataset. Answers the disabled defaults before the first configuration.")
        .see("evidence-settings")
        .json("200", "The settings and their protection.", "EvidenceSettingsStatus")
        .errors(&[503]),
    );
    p.add(
        op(
            PUT,
            "/$/evidence/{ds}/settings",
            "putEvidenceSettings",
            "Datasets",
            "Set the evidence settings",
        )
        .doc("Needs `admin` on the dataset. Records complete settings and protects every graph of their roles. A configuration that would stop protecting a graph, or that races with another change, is a `409` that writes nothing.")
        .see("evidence-settings")
        .json_body(true, "EvidenceSettings")
        .json("200", "The settings and their protection.", "EvidenceSettingsStatus")
        .errors(&[400, 409, 503]),
    );
    p.add(
        op(
            POST,
            "/$/evidence/{ds}/detach",
            "detachEvidenceGraph",
            "Datasets",
            "Stop protecting an evidence graph",
        )
        .doc("Needs `admin` on the dataset. Ordinary writes can change the graph until the settings are recorded again.")
        .see("evidence-settings")
        .json_body(true, "EvidenceDetach")
        .json("200", "The settings and their protection.", "EvidenceSettingsStatus")
        .errors(&[400, 404, 409, 503]),
    );
    p.add(
        op(
            GET,
            "/$/evidence/{ds}/holds",
            "listEvidenceHolds",
            "Datasets",
            "List the evidence holds",
        )
        .doc("Needs `admin` on the dataset. Lists the durable source holds of the evidence profile by id, a page at a time.")
        .see("evidence-holds")
        .query("onBranch", s(), "Only holds that name this branch.")
        .query("after", json!({ "type": "string", "format": "uuid" }), "Start after this hold id, the `next` of the previous page.")
        .query("limit", json!({ "type": "integer", "minimum": 1, "maximum": 1000 }), "The most holds in the page, 1000 by default.")
        .json("200", "A page of holds.", "EvidenceHoldList")
        .paginated("cursor", &["after", "limit"], "next")
        .errors(&[400, 404, 503]),
    );
    p.add(
        op(
            POST,
            "/$/evidence/{ds}/holds",
            "acquireEvidenceHold",
            "Datasets",
            "Acquire an evidence hold",
        )
        .doc("Needs `admin` on the dataset. Keeps the commits readable until the hold is released: history trimming, compaction and generation retirement keep them, and a branch the hold names is deleted only with `releaseHolds=true`. The hold is durable when the answer arrives. The same id with the same commits answers `200` with the existing hold. Another set of commits under that id is `409 hold-conflict`, more than 65,536 held commits in the dataset is `409 holds-limit`, and a commit that is no longer readable is `410 history-gone`.")
        .see("evidence-holds")
        .json_body(true, "EvidenceHoldRequest")
        .json("201", "The new hold.", "EvidenceHold")
        .json("200", "The existing hold of that id.", "EvidenceHold")
        .errors(&[400, 404, 409, 410, 503]),
    );
    p.add(
        op(
            DELETE,
            "/$/evidence/{ds}/holds/{id}",
            "releaseEvidenceHold",
            "Datasets",
            "Release an evidence hold",
        )
        .doc("Needs `admin` on the dataset. The held commits can be collected once the release is durable.")
        .see("evidence-holds")
        .no_content("Released.")
        .errors(&[400, 404, 503]),
    );
    p.add(
        op(
            GET,
            "/$/memory/{ds}/inbox",
            "getMemoryInbox",
            "Datasets",
            "List what waits for review",
        )
        .doc("Needs `read` on the dataset and covers the caller's view. Unreviewed session facts come with their span, link, guard and corroboration signals.")
        .see("review-inbox")
        .query("limit", int(), "The most facts, 1 to 500, 200 by default.")
        .query("timeoutSeconds", json!({"type": "number"}), "The time limit.")
        .json("200", "The inbox.", "MemoryInbox")
        .errors(&[400, 404, 408]),
    );
    p.add(
        op(
            GET,
            "/$/memory/{ds}/review/{name}",
            "getBranchReview",
            "Datasets",
            "Review a branch",
        )
        .doc("What the branch proposes and retracts relative to main, with signals, new entities and their possible duplicates, and the text of the sources the facts cite.")
        .see("review-inbox")
        .query("limit", int(), "The most facts, 1 to 2000, 500 by default.")
        .query("timeoutSeconds", json!({"type": "number"}), "The time limit.")
        .json("200", "The review.", "BranchReview")
        .errors(&[400, 404, 408]),
    );
    p.add(
        op(
            POST,
            "/$/memory/{ds}/promote",
            "promoteFacts",
            "Datasets",
            "Promote facts for review",
        )
        .doc("Writes the facts into the target graph on a review branch with reifiers derived from the facts' own, for a person to merge. Needs `write` on the target graph on that branch.")
        .see("review-inbox")
        .json_body(true, "PromoteRequest")
        .json("200", "The review branch.", "PromoteResult")
        .errors(&[400, 403, 404, 408, 422]),
    );
    p.add(
        op(
            POST,
            "/$/memory/{ds}/reject",
            "rejectFacts",
            "Datasets",
            "Reject facts",
        )
        .doc("Retracts the facts on main or on a review branch, keeping their reifiers with `prov:wasInvalidatedBy`, with a commit message that names the reviewer.")
        .see("review-inbox")
        .json_body(true, "RejectRequest")
        .json("200", "The retractions.", "RejectResult")
        .errors(&[400, 403, 404, 408, 422]),
    );
    p.add(
        op(
            POST,
            "/$/memory/{ds}/relink",
            "relinkEntity",
            "Datasets",
            "Use an existing entity",
        )
        .doc("On a review branch, every triple and reifier that names `from` names `to` instead, and `from`'s own types and labels are removed, in one commit.")
        .see("review-inbox")
        .json_body(true, "RelinkRequest")
        .json("200", "The commit.", "MemoryWriteResult")
        .errors(&[400, 403, 404, 408, 422]),
    );
    p.add(
        op(
            POST,
            "/$/memory/{ds}/edit",
            "editFact",
            "Datasets",
            "Edit a fact's value",
        )
        .doc("Retracts the fact and asserts it with the new object, with the new reifier derived from the old one.")
        .see("review-inbox")
        .json_body(true, "EditFactRequest")
        .json("200", "The result of `assert_facts`.", "AssertFactsResult")
        .errors(&[400, 403, 404, 408, 422]),
    );
    p.add(
        op(
            POST,
            "/$/memory/{ds}/consolidate",
            "startConsolidation",
            "Datasets",
            "Consolidate agent memory",
        )
        .doc("Starts a consolidation task: the facts that several sources of agent memory assert, and no reviewed graph yet, are asserted once in the consolidated graph on a review branch with reifiers derived from the session reifiers. The result lists duplicate entities and conflicts for a person. Needs `read` on the dataset; the writes run as the caller, and `auto` mode needs `admin`. The task routes of `/$/ingest/{ds}/{task}` read and cancel it.")
        .see("memory-maintenance")
        .json_body(false, "ConsolidateRequest")
        .json("202", "The task.", "IngestTask")
        .errors(&[400, 403, 404, 429]),
    );
    p.add(
        op(
            POST,
            "/$/memory/{ds}/retention",
            "startRetention",
            "Datasets",
            "Delete old session graphs",
        )
        .doc("Starts a retention task: each session graph whose newest fact is older than `after`, and whose facts a reviewed graph asserts too unless `requireConsolidated` is false, is deleted with its own Graph Store `DELETE`. Needs `admin` on the dataset.")
        .see("memory-maintenance")
        .json_body(false, "RetentionRequest")
        .json("202", "The task.", "IngestTask")
        .errors(&[400, 403, 404, 429]),
    );
    p.add(
        op(
            GET,
            "/$/memory/{ds}/maintenance",
            "getMaintenance",
            "Datasets",
            "Read the maintenance schedules",
        )
        .see("memory-maintenance")
        .json("200", "The schedules.", "MaintenanceStatus")
        .errors(&[404]),
    );
    p.add(
        op(
            GET,
            "/$/ingest/{ds}/profiles",
            "listIngestProfiles",
            "Datasets",
            "List the ingest profiles",
        )
        .see("ingest-profiles")
        .json("200", "The settings and profiles.", "IngestProfiles"),
    );
    p.add(
        op(
            PUT,
            "/$/ingest/{ds}/settings",
            "putIngestSettings",
            "Datasets",
            "Set the ingest settings",
        )
        .doc("Changes the settings members of the `ingest` kind as `PATCH /$/settings/{ds}/ingest` does. Needs `admin` on the dataset.")
        .see("ingest-profiles")
        .json_body(true, "IngestSettingsRequest")
        .json("200", "The stored setting.", "IngestSettingsRequest")
        .errors(&[400]),
    );
    p.add(
        op(
            POST,
            "/$/ingest/{ds}",
            "startIngest",
            "Datasets",
            "Ingest a document",
        )
        .doc("Starts an ingestion task: converts the document, registers it as a source on a review branch, and extracts its facts with the `extract` role into proposals there. A CSV or TSV file gets a C05 mapping draft instead, and nothing is written. Needs `read` on the dataset; every write runs as the caller, so it needs what `register_source` and `assert_facts` need. A PDF that needs OCR on a server without it fails with `needs-ocr` and its pages.")
        .see("ingestion")
        .body(
            true,
            "The document: a multipart upload, or JSON with text or a URL.",
            json!({
                "multipart/form-data": { "schema": sref("IngestForm") },
                "application/json": { "schema": sref("IngestRequest") },
            }),
        )
        .json("202", "The task.", "IngestTask")
        .errors(&[400, 403, 404, 413, 415, 429]),
    );
    p.add(
        op(
            GET,
            "/$/ingest/{ds}",
            "listIngestTasks",
            "Datasets",
            "List ingestion tasks",
        )
        .doc("The caller's tasks, or every task for an admin of the dataset, without their usage. Finished tasks are kept for 7 days.")
        .see("ingestion")
        .json("200", "The tasks.", "IngestTaskList"),
    );
    p.add(
        op(
            GET,
            "/$/ingest/{ds}/{task}",
            "getIngestTask",
            "Datasets",
            "Read an ingestion task",
        )
        .doc("Visible to the principal that started it and to admins of the dataset.")
        .see("ingestion")
        .query(
            "wait",
            json!({"type": "number"}),
            "Hold the answer until the task ends or waits for the caller, at most 60 seconds.",
        )
        .json("200", "The task.", "IngestTask")
        .errors(&[404]),
    );
    p.add(
        op(
            DELETE,
            "/$/ingest/{ds}/{task}",
            "cancelIngestTask",
            "Datasets",
            "Cancel or forget an ingestion task",
        )
        .doc("Cancels a running task (`202`), or forgets one that has ended (`204`). A cancelled task removes nothing it already wrote on its review branch.")
        .see("ingestion")
        .json("202", "The task, cancelling.", "IngestTask")
        .no_content("Forgotten.")
        .errors(&[404]),
    );
    p.add(
        op(
            POST,
            "/$/ingest/{ds}/{task}/confirm",
            "confirmIngestTask",
            "Datasets",
            "Confirm an ingestion's estimate",
        )
        .doc("A task whose estimate is above the dataset's `confirmTokens` waits in `awaiting-confirmation` for this call.")
        .see("ingestion")
        .json("200", "The task.", "IngestTask")
        .errors(&[404, 409]),
    );
    p.add(
        op(
            POST,
            "/$/ingest/{ds}/{task}/approve",
            "approveIngestTask",
            "Datasets",
            "Approve a preview",
        )
        .doc("Writes a preview's source and facts to `main` as the caller. Fails with `409` when `main` changed since the preview.")
        .see("ingestion")
        .json("200", "The task.", "IngestTask")
        .errors(&[403, 404, 409, 422]),
    );
    p.add(
        op(
            GET,
            "/$/ingest/{ds}/profiles/{name}",
            "getIngestProfile",
            "Datasets",
            "Get an ingest profile",
        )
        .doc("The profile `default` answers `{}` when it is not stored, which means the whole schema.")
        .see("ingest-profiles")
        .json("200", "The profile.", "IngestProfile")
        .errors(&[400, 404]),
    );
    p.add(
        op(
            PUT,
            "/$/ingest/{ds}/profiles/{name}",
            "putIngestProfile",
            "Datasets",
            "Set an ingest profile",
        )
        .doc("Needs `admin` on the dataset. A dataset keeps at most 50 profiles.")
        .see("ingest-profiles")
        .json_body(true, "IngestProfile")
        .json("200", "The stored profile.", "IngestProfile")
        .errors(&[400]),
    );
    p.add(
        op(
            DELETE,
            "/$/ingest/{ds}/profiles/{name}",
            "deleteIngestProfile",
            "Datasets",
            "Remove an ingest profile",
        )
        .doc("Needs `admin` on the dataset.")
        .see("ingest-profiles")
        .no_content("Removed.")
        .errors(&[400, 404]),
    );
    p.add(
        op(
            GET,
            "/$/queries/{ds}/suggestions",
            "listSuggestions",
            "Stored queries",
            "List suggested examples",
        )
        .doc("Needs `admin` on the dataset.")
        .see("suggested-examples")
        .json("200", "The suggestions, newest first.", "SuggestionList"),
    );
    p.add(
        op(
            POST,
            "/$/queries/{ds}/suggestions",
            "suggestExample",
            "Stored queries",
            "Suggest an example",
        )
        .doc("Needs `read` on the dataset. A dataset keeps at most 500 suggestions.")
        .see("suggested-examples")
        .json_body(true, "SuggestRequest")
        .json("201", "The suggestion.", "Suggestion")
        .errors(&[400, 409]),
    );
    p.add(
        op(DELETE, "/$/queries/{ds}/suggestions", "deleteSuggestion", "Stored queries", "Remove a suggested example")
            .doc("Needs `admin` on the dataset. Promoting a suggestion is a `PUT` of the stored query and then this call.")
            .see("suggested-examples")
            .query_req("id", s(), "The suggestion's id.")
            .no_content("Removed.")
            .errors(&[404]),
    );
    let tag = "Models";
    p.add(
        op(GET, "/$/models", "listModelProviders", tag, "List model providers")
            .doc("The providers of the effective model configuration, their models with the detected structured-output level and last status, and the role lists. Keys are never returned, only the names of their secrets and where each comes from. `configured` is `false` when neither `serve --model-config` nor the runtime layer configures models.")
            .see("model-providers")
            .json("200", "The providers and role lists.", "ModelProviders"),
    );
    p.add(
        op(POST, "/$/models/{name}/test", "testModelProvider", tag, "Test a model provider")
            .doc("Sends a short prompt to one model of the provider, detects its structured-output level again, and reports the latency, level and token counts. A failed call is a `200` with `ok` false and the error.")
            .see("model-providers")
            .body(false, "A `ModelTestRequest`.", json!({ "application/json": { "schema": sref("ModelTestRequest") } }))
            .json("200", "The outcome.", "ModelTestResult")
            .errors(&[400, 404]),
    );
    p.add(
        op(GET, "/$/models/usage", "modelUsage", tag, "Model usage by dataset")
            .doc("The routing counters of the last `days` days by dataset: asks, answers by role, pair and feedback, escalations by role and signal, feedback by complexity bucket, and tokens with the estimated cost by pair. The counters live in memory and start again when the server restarts.")
            .see("asking-in-the-server")
            .query("days", json!({ "type": "integer", "minimum": 1, "maximum": 400, "default": 30 }), "The days counted, today included.")
            .json("200", "The counters.", "ModelUsage")
            .errors(&[400]),
    );
    let tag = "Assistant";
    p.add(
        op(POST, "/{ds}/ask", "askQuestion", tag, "Ask a question")
            .doc("Runs the pipeline of spec C18 with the dataset's model pairs. It grounds the question, drafts a query, checks and runs it, repairs it after a failure, escalates to a later pair on a verified signal, and summarizes the rows when the dataset sends rows. The answer streams as server-sent events, one JSON object per event, unless `Accept` names `application/json` without `text/event-stream`. Needs `read` and counts as a query for rate limits.")
            .see("asking-in-the-server")
            .json_body(true, "AskRequest")
            .resp(
                "200",
                "The events `ground`, `clarify`, `draft`, `escalate`, `check`, `run`, `diagnosis`, `result`, `summary`, `error` and finally `usage`, or the whole answer as one object.",
                Some(json!({
                    "text/event-stream": text(),
                    "application/json": { "schema": sref("AskResult") },
                })),
            )
            .errors(&[400, 404, 409, 429]),
    );
    p.add(
        op(POST, "/{ds}/sparql/explain", "explainQuery", tag, "Explain a query")
            .doc("The plan of a query with node ids, notes on the operators that matter, and a description of what the query asks (spec C18 §6.6). `profile` plans, runs read-only, or reads a plan the client gives, such as the plan in the error body of a query that a budget stopped. The notes and a template description need no model. When the dataset enables `explain`, its `explain` role rewrites them as prose, checked against the plan. The answer streams as server-sent events unless `Accept` names `application/json` without `text/event-stream`. Needs `read` and counts as a query for rate limits.")
            .see("explaining-a-query")
            .json_body(true, "ExplainRequest")
            .resp(
                "200",
                "The events `plan`, `notes`, `explanation`, `usage` when a model was called, and `error`, or the whole explanation as one object.",
                Some(json!({
                    "text/event-stream": text(),
                    "application/json": { "schema": sref("ExplainResult") },
                })),
            )
            .errors(&[400, 403, 404, 408, 413, 507]),
    );
    p.add(
        op(GET, "/$/assistant/{ds}", "getAssistantSettings", "Datasets", "Get the assistant settings")
            .doc("The dataset's effective assistant settings with a `status` that says whether asking works, and why not when it does not.")
            .see("assistant-settings")
            .json("200", "The settings and their status.", "AssistantSettings"),
    );
    p.add(
        op(PUT, "/$/assistant/{ds}", "putAssistantSettings", "Datasets", "Set the assistant settings")
            .doc("Makes the effective assistant settings equal to the body, as `PUT /$/settings/{ds}/assistant` does, and answers in the shape of `GET`. Providers and models must be ones the server configuration names, and an `endpoint` or `apiKey` anywhere is refused.")
            .see("assistant-settings")
            .json_body(true, "AssistantSettings")
            .json("200", "The stored settings and their status.", "AssistantSettings")
            .errors(&[400]),
    );
    p.add(
        op(GET, "/$/graphrag/{ds}", "getGraphrag", "Datasets", "Read the state of the community reports")
            .doc("The settings in force, the reports by status, the communities by level and the last build. A caller for whom the reports graph is not proven gets `available: false` and nothing derived from the reports. A dataset admin also gets the budget and the accounting anomalies.")
            .see("graphrag")
            .json("200", "The state.", "GraphragStatus")
            .errors(&[404]),
    );
    for (path, id, summary, what) in [
        (
            "/$/graphrag/{ds}/build",
            "buildGraphrag",
            "Build the community reports",
            "writes a report for every community",
        ),
        (
            "/$/graphrag/{ds}/refresh",
            "refreshGraphrag",
            "Refresh the community reports",
            "rewrites the reports of communities that changed",
        ),
    ] {
        p.add(
            op(POST, path, id, "Datasets", summary)
                .doc(&format!("Starts a task of kind `graphrag` that {what}. Without `confirm` the task records its estimate in its `detail` and waits for `POST /$/graphrag/{{ds}}/confirm`. Needs `admin` on the dataset, and GraphRAG enabled in its settings."))
                .see("graphrag")
                .json_body(false, "GraphragBuildRequest")
                .resp("202", "The task.", Some(json!({ "application/json": { "schema": sref("Task") } })))
                .errors(&[400, 403, 404, 409, 503]),
        );
    }
    p.add(
        op(POST, "/$/graphrag/{ds}/confirm", "confirmGraphrag", "Datasets", "Answer a build's estimate")
            .doc("Confirms or refuses the estimate of a task that waits for it. Needs `admin` on the dataset.")
            .see("graphrag")
            .json_body(true, "GraphragConfirmRequest")
            .resp("200", "The answer was recorded.", None)
            .errors(&[400, 403, 404, 409]),
    );
    p.add(
        op(GET, "/$/graphrag/{ds}/communities", "listCommunities", "Datasets", "List the communities")
            .doc("The communities of the reports graph with their reports' titles, statuses and ratings, read through the caller's view.")
            .see("graphrag")
            .json("200", "The communities.", "GraphragCommunities")
            .errors(&[404]),
    );
    p.add(
        op(GET, "/$/graphrag/{ds}/community", "getCommunity", "Datasets", "Read a community and its report")
            .query("iri", s(), "The community's IRI.")
            .doc("A community with its members, its children and its report with findings and citations, read through the caller's view. A community the caller may not read is a `404`.")
            .see("graphrag")
            .json("200", "The community.", "GraphragCommunity")
            .errors(&[400, 404]),
    );
    p.add(
        op(POST, "/$/graphrag/{ds}/memberships", "listCommunityMemberships", "Datasets", "Read the communities of nodes")
            .doc("The communities of the reports graph that hold each listed node, at the given level or at every level, with each community's member count and report title. Read on main only and through the caller's view. A caller for whom the reports graph is not proven, or a request on a branch, gets `available: false`.")
            .see("graphrag")
            .json_body(true, "GraphragMembershipsRequest")
            .json("200", "The memberships.", "GraphragMemberships")
            .errors(&[400, 404, 413]),
    );
    p.add(
        op(POST, "/$/graphrag/{ds}/review", "reviewCommunityReport", "Datasets", "Review a community report")
            .doc("Sets a report's status to `reviewed` or `rejected`, or supersedes it with a person's edit of its title or summary. Needs `write` on the dataset and a reports graph the caller may read.")
            .see("graphrag")
            .json_body(true, "GraphragReviewRequest")
            .json("200", "The review.", "GraphragReviewResult")
            .errors(&[400, 403, 404]),
    );
    p.add(
        op(GET, "/$/settings", "getSettingsStatus", "Server", "Read the settings file's status")
            .doc("The path of `serve --settings`, when it and the model configuration were last read, the last reload error, and the declared dataset names that match no dataset. Needs server `admin`.")
            .see("settings")
            .json("200", "The status.", "SettingsStatus"),
    );
    p.add(
        op(
            GET,
            "/$/settings/{ds}",
            "getDatasetSettings",
            "Datasets",
            "Get every settings kind",
        )
        .doc("Each kind's effective object with its layers, sources and locks.")
        .see("settings")
        .json("200", "The kinds.", "DatasetSettings"),
    );
    let kind_body = |o: super::Op| {
        o.body(
            true,
            "Fields of the kind: `AssistantSettings`, `MemorySettings`, `IngestSettingsRequest`, or for `prefixes` a map from prefix names to IRIs.",
            json!({ "application/json": { "schema": { "type": "object" } } }),
        )
        .header(
            "If-Match",
            json!({ "type": "string" }),
            "The `etag` of the runtime layer the change is based on.",
        )
        .json("200", "The kind after the change.", "SettingsKind")
        .errors(&[400, 409, 412])
    };
    p.add(
        op(GET, "/$/settings/{ds}/{kind}", "getSettings", "Datasets", "Get a settings kind")
            .doc("The kind's effective object, its declared and runtime layers, the source of each field, the locked fields and whether the effective object is valid. The `ETag` names the runtime layer.")
            .see("settings")
            .json("200", "The kind.", "SettingsKind")
            .errors(&[404]),
    );
    p.add(kind_body(
        op(PATCH, "/$/settings/{ds}/{kind}", "patchSettings", "Datasets", "Change settings fields")
            .doc("Merges the body into the runtime layer as RFC 7396 says. `null` removes a runtime value, so the field falls back to the declared value or the default. A change to a locked field is a `409` with `locked-by-config` and `fields`, and an `endpoint` or `apiKey` anywhere is a `400`. Needs `admin`.")
            .see("settings"),
    ));
    p.add(kind_body(
        op(PUT, "/$/settings/{ds}/{kind}", "putSettings", "Datasets", "Set a settings kind")
            .doc("Makes the effective object equal to the body, with built-in defaults for the members it leaves out and the current value for locked fields it leaves out. The runtime layer keeps the fields where the body differs from the declared values and the defaults. Needs `admin`.")
            .see("settings"),
    ));
    p.add(
        op(DELETE, "/$/settings/{ds}/{kind}", "resetSettings", "Datasets", "Reset settings")
            .doc("Clears the runtime layer, or with `field` one field of it, so the declared values and the defaults apply. Needs `admin`.")
            .see("settings")
            .query("field", json!({ "type": "string" }), "A dotted field such as `historyDays` or `budget.perRequest`.")
            .header("If-Match", json!({ "type": "string" }), "The `etag` of the runtime layer the change is based on.")
            .json("200", "The kind after the change.", "SettingsKind")
            .errors(&[400, 409, 412]),
    );
    let server_body = |o: super::Op| {
        o.body(
            true,
            "Fields of the kind: `providers`, `roles` and `routing` of `models` (`ModelsSettings`), or `enabled`, `server`, `baseUrl`, `repeatEvery`, `channels`, `routes` and `delivery` of `notifications`.",
            json!({ "application/json": { "schema": { "type": "object" } } }),
        )
        .header(
            "If-Match",
            json!({ "type": "string" }),
            "The `etag` of the runtime layer the change is based on.",
        )
        .json("200", "The kind after the change.", "ServerSettingsKind")
        .errors(&[400, 404, 409, 412])
    };
    p.add(
        op(GET, "/$/server/settings/{kind}", "getServerSettings", "Server", "Get a server settings kind")
            .doc("The model configuration (`models`) or the notifications (`notifications`): the effective object, the declared layer (`--model-config`, or the settings file's `server.notifications`), the runtime layer, the source of each field, the fields `server.locked` locks and whether the effective object is valid. The `ETag` names the runtime layer. Needs server `admin`.")
            .see("server-settings")
            .json("200", "The kind.", "ServerSettingsKind")
            .errors(&[404]),
    );
    p.add(server_body(
        op(PATCH, "/$/server/settings/{kind}", "patchServerSettings", "Server", "Change server settings fields")
            .doc("Merges the body into the runtime layer as RFC 7396 says. Providers merge member by member, and `null` for a declared provider removes it. A role list is one field. Headers that carry credentials and endpoints with credentials, a query or a fragment are refused. A change to a locked field is a `409` with `locked-by-config`. A change that leaves the model configuration invalid is a `400` with `bad-settings`, and its `field` names the field that fails as a dotted path, such as `providers.claude.models.m.temperature`, when the check names one. Requests that start after the change use it. Needs server `admin`.")
            .see("server-settings"),
    ));
    p.add(server_body(
        op(PUT, "/$/server/settings/{kind}", "putServerSettings", "Server", "Set a server settings kind")
            .doc("Makes the effective model configuration equal to the body. The runtime layer keeps the fields where the body differs from the declared configuration, with `null` for a declared provider the body leaves out. Needs server `admin`.")
            .see("server-settings"),
    ));
    p.add(
        op(DELETE, "/$/server/settings/{kind}", "resetServerSettings", "Server", "Reset server settings")
            .doc("Clears the runtime layer, or with `field` one field of it, so the declared configuration applies. Needs server `admin`.")
            .see("server-settings")
            .query("field", json!({ "type": "string" }), "A dotted field such as `providers.claude` or `roles.draft`.")
            .header("If-Match", json!({ "type": "string" }), "The `etag` of the runtime layer the change is based on.")
            .json("200", "The kind after the change.", "ServerSettingsKind")
            .errors(&[400, 404, 409, 412]),
    );
    p.add(
        op(GET, "/$/server/secrets", "listSecrets", "Server", "List model secrets")
            .doc("Each secret that `--model-secret`, a stored value, a provider or a lock names, with its source, whether it is locked, when a runtime value was stored and the providers that use it, and whether stored values are sealed and with which key. Values are never returned. Needs server `admin`.")
            .see("model-secrets")
            .json("200", "The secrets.", "SecretList"),
    );
    p.add(
        op(PUT, "/$/server/secrets/{name}", "putSecret", "Server", "Store a model secret")
            .doc("Stores a runtime value for the secret in the data directory with mode 0600. Builds with the default `backup` feature seal values unless the server runs with `--secrets-key none`. Builds without it keep plaintext. When sealing is enabled, the first value stored without a configured key generates `<dataDir>/secrets.key`. It overrides the `--model-secret` source, and requests that start after it use it. A secret that `server.locked` locks is a `409` with `locked-by-config`. Needs server `admin`.")
            .see("model-secrets")
            .json_body(true, "SecretValue")
            .no_content("Stored.")
            .errors(&[400, 409]),
    );
    p.add(
        op(DELETE, "/$/server/secrets/{name}", "deleteSecret", "Server", "Remove a model secret's runtime value")
            .doc("Removes the runtime value, so the `--model-secret` source applies again. Needs server `admin`.")
            .see("model-secrets")
            .no_content("Removed, or there was no runtime value.")
            .errors(&[400]),
    );
    p.add(
        op(GET, "/$/notifications", "getNotifications", "Server", "Get the status of notifications")
            .doc("Whether outbound notifications are on, each channel with its target, its last success and its last failure, the routes, the length of the queue, the conditions that were notified and the last 50 deliveries. The counters start empty at each start. Needs server `admin`.")
            .see("notifications")
            .json("200", "The status.", "NotificationStatus"),
    );
    p.add(
        op(POST, "/$/notifications/test/{channel}", "testNotification", "Server", "Send a test notification")
            .doc("Sends a `notification.test` envelope to the channel now, in one attempt, whether or not notifications are on. A failure is a `502` with the code `delivery-failed`, or `outbound-refused` when the outbound policy refuses the channel. Needs server `admin`.")
            .see("notifications")
            .json("200", "The channel accepted the notification.", "NotificationTest")
            .errors(&[404, 502]),
    );
    p.add(
        op(GET, "/$/asks/{ds}", "listAsks", tag, "List your asked questions")
            .doc("The caller's own asks on the dataset, newest first, kept for `historyDays`. An entry holds the question, the final query, the commit, the feedback and the routing record, never rows or summaries.")
            .see("ask-history")
            .query("limit", json!({ "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }), "The most entries listed.")
            .json("200", "The entries.", "AskHistory"),
    );
    p.add(
        op(
            DELETE,
            "/$/asks/{ds}",
            "deleteAsks",
            tag,
            "Forget your asked questions",
        )
        .doc("Removes the caller's entries, or the one entry `id` names.")
        .see("ask-history")
        .query(
            "id",
            s(),
            "The entry to remove. All of the caller's entries without it.",
        )
        .no_content("Removed.")
        .errors(&[404]),
    );
    p.add(
        op(POST, "/$/asks/{ds}/{id}/feedback", "askFeedback", tag, "Give feedback on an answer")
            .doc("Records `accepted`, `edited` or `rejected` for one of the caller's asks, in the routing counters and, when history is kept, in its entry.")
            .see("ask-history")
            .json_body(true, "AskFeedback")
            .no_content("Recorded.")
            .errors(&[400, 404]),
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
        .param("branch")
        .query("repository", s(), "One repository.")
        .json("200", "The backups.", "BackupList"),
    );
    p.add(
        op(POST, "/$/backups/{ds}", "createBackup", tag, "Back up a dataset")
            .doc("A JSON body captures the selected branch as a standalone dataset in a repository. Branch restores use a fresh identity. Without a JSON body, this is Fuseki's N-Quads file alias.")
            .param("branch")
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
        .param("branch")
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
        .param("branch")
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
        .param("branch")
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
        .param("branch")
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
            .errors(&[400, 401, 403, 429, 503]),
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
            .errors(&[400, 409, 503]),
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
    let mut o = query_params(write_params(o));
    // This endpoint accepts queries and writes. Its single profile parameter names
    // either query I/O profiling or the preview's reasoning program.
    o.params
        .retain(|parameter| parameter != &super::param_ref("profile"));
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
        .doc("A multipart upload of RDF files, CSV or TSV tables mapped to triples, and the files an R2RML, RML or YARRRML mapping reads, in one commit. The format comes from each file name or content type. A plain `text/csv` or `text/tab-separated-values` body is one table. A mapping file source without its part is `400` with code `mapping-source-missing`.")
        .see("csv-and-tsv-uploads")
        .query("graph", s(), "The target graph.")
        .query("base", s(), "The default mapping's namespace for tables.")
        .query("key", s(), "The column that names each row in the default mapping.")
        .query("mapping", s(), "A stored mapping to use instead of a `mapping` part.")
        .query("mappingVersion", int(), "The stored mapping's version (the latest by default).")
        .query("mappingKind", str_enum(&["rml", "yarrrml", "csvw", "template"]), "The kind of the `mapping` part when its content does not tell.")
        .query("csvwMode", str_enum(&["minimal", "standard"]), "A CSVW mapping: standard mode adds the table group, table and row descriptions.")
        .query("csvwCheck", str_enum(&["keys", "none"]), "A CSVW mapping: check primary and foreign keys (the default in standard mode).")
        .query("lookup", boolean(), "A CONSTRUCT template: read the dataset's head while mapping. The report gives `lookupCommit`, and `If-Match` makes the commit fail when the head moved.")
        .param("ifMatch")
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
                        "mapping": { "type": "string", "description": "CSVW metadata, an R2RML, RML or YARRRML mapping, or a CONSTRUCT template. The file parts its sources name are read by it." },
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
    shapes.insert("application/x-www-form-urlencoded".into(), text());
    let mut report = Map::new();
    media(&RDF[..6], &mut report);
    report.insert(
        "application/json".into(),
        json!({ "schema": { "type": "object" } }),
    );
    p.add(
        op(POST, "/{ds}/shacl", "shacl", "Validation", "Validate with SHACL")
            .doc("Validates a data graph of the dataset against the shapes graph in the body, as Fuseki's `/{ds}/shacl` does. The report answers `200` whether or not the data conforms. With `shapes=guard` or `shapes=<graph IRI>` the shapes come from the write-time guard or from a graph of the dataset, and the body is empty or a form of further parameters. A named source with any other body is a `400`.")
            .see("shacl-validation")
            .param("graphSel")
            .query("target", json!({ "type": "array", "items": { "type": "string" }, "maxItems": 1000 }), "Validate these nodes against the shapes whose targets select them. Each is an IRI, a prefixed name or a blank node label, and the parameter may repeat up to 1,000 times.")
            .query("shapes", s(), "`guard` for the write-time guard's shapes on its data graph, or the IRI of a shapes graph of the dataset.")
            .params(&["reasoning", "timeout", "at", "format"])
            .body(false, "The shapes graph, or with `shapes` an empty body or a form of parameters.", J::Object(shapes))
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
            .query("schema", json!({ "type": "string", "enum": ["guard"] }), "`guard` validates against the write-time guard's schema on its data graph. The body is then empty or a form of parameters, and the shape map names up to 1,000 nodes, where `START` takes the shapes the guard's shape map gives each node.")
            .params(&["graphSel", "reasoning", "timeout", "at", "format"])
            .body(
                false,
                "The schema, or a JSON envelope, or with `schema=guard` an empty body or a form of parameters.",
                json!({
                    "text/shex": text(),
                    "application/shex+json": { "schema": { "type": "object" } },
                    "application/json": { "schema": { "anyOf": [sref("ShexRequest"), { "type": "object", "description": "A ShExJ schema." }] } },
                    "text/turtle": text(),
                    "application/x-www-form-urlencoded": text(),
                }),
            )
            .resp(
                "200",
                "The result.",
                Some(json!({
                    "application/json": { "schema": { "anyOf": [sref("ShexReport"), sref("ShexResultMap")] } },
                    "text/plain": text(),
                })),
            )
            .errors(&[400, 408, 501, 507]),
    );
    p.add(
        op(GET, "/{ds}/prefixes", "getPrefixes", "Datasets", "Read prefixes")
            .doc("Fuseki's prefixes service on the stored prefixes, the runtime layer of the `prefixes` settings kind: `prefix=p` returns one binding (`404` if unbound), `uri=u` the prefixes of an IRI, and neither all of them. Declared and well-known prefixes are at `/$/prefixes/{ds}`.")
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
                .doc("Binds `prefix` to `uri`, given in the query, a form or a JSON body, in the runtime layer of the `prefixes` settings kind, and clears a removal of the name. A name the settings file locks is a `409` with `locked-by-config`.")
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
                .errors(&[400, 409]),
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
        .doc("Removes a stored binding. For a name the settings file declares, it also stores a removal, so the declared prefix stops applying until a settings reset. A locked name is a `409` with `locked-by-config`, and a name neither stored nor declared is a `404`.")
        .see("datasets-admin")
        .query_req("prefix", s(), "The prefix name.")
        .no_content("Removed.")
        .errors(&[404, 409]),
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

/// The facts of a projection, as `POST /$/vector/{ds}/{name}/projection` answers them.
fn projection_summary() -> J {
    json!({
        "type": "object",
        "required": ["index", "method", "commit", "stale", "entities", "bounds"],
        "properties": {
            "index": { "type": "string" },
            "method": { "type": "string", "enum": ["pca", "neighbours"] },
            "commit": { "type": "integer" },
            "generation": { "type": "string" },
            "stale": { "type": "boolean" },
            "vectors": { "type": "integer" },
            "dimension": { "type": "integer" },
            "entities": { "type": "integer" },
            "bounds": { "type": "array", "items": { "type": "number" } },
            "explained": { "type": "array", "items": { "type": "number" } },
            "builtMs": { "type": "number" },
            "bytes": { "type": "integer" }
        }
    })
}
