//! The Sparkles operations the client calls, by the `operationId` of the server's OpenAPI
//! description (`docs/openapi.json`). Every URL to a Sparkles server is built from an
//! entry here, and a request may only carry the query parameters, headers and body media
//! types its entry lists. The test at the end checks each entry against the checked-in
//! description, so a route the server renames or drops fails the client's tests.

use reqwest::Method;

/// One operation of the API.
#[derive(Debug)]
pub struct Op {
    pub id: &'static str,
    pub method: Method,
    /// The path template, with `{name}` parameters.
    pub path: &'static str,
    /// Query parameters the client may send.
    pub query: &'static [&'static str],
    /// Request headers the client may send, beyond the standard ones (`Accept`,
    /// `Content-Type`, `Authorization`, `Content-Encoding`).
    pub headers: &'static [&'static str],
    /// Body media types the client may send.
    pub body: &'static [&'static str],
}

const QUERY_PARAMS: &[&str] = &[
    "default-graph-uri",
    "named-graph-uri",
    "timeout",
    "reasoning",
    "at",
];
const GSP_WRITE_PARAMS: &[&str] = &[
    "graph", "default", "timeout", "dryRun", "receipt", "validate",
];
const WRITE_HEADERS: &[&str] = &["Sparkles-Commit-Message"];
const GSP_WRITE_HEADERS: &[&str] = &["Sparkles-Commit-Message", "If-Match", "If-None-Match"];

macro_rules! op {
    ($name:ident, $id:literal, $method:ident, $path:literal) => {
        op!($name, $id, $method, $path, &[], &[], &[]);
    };
    ($name:ident, $id:literal, $method:ident, $path:literal, $query:expr) => {
        op!($name, $id, $method, $path, $query, &[], &[]);
    };
    ($name:ident, $id:literal, $method:ident, $path:literal, $query:expr, $headers:expr, $body:expr) => {
        pub const $name: Op = Op {
            id: $id,
            method: Method::$method,
            path: $path,
            query: $query,
            headers: $headers,
            body: $body,
        };
    };
}

// the SPARQL Protocol on a dataset (`query` itself goes in the URL of a GET and in the
// form body of a POST)
op!(
    SPARQL_GET,
    "sparqlQueryGet",
    GET,
    "/{ds}/sparql",
    &[
        "query",
        "default-graph-uri",
        "named-graph-uri",
        "timeout",
        "reasoning",
        "at"
    ]
);
op!(
    SPARQL_POST,
    "sparqlQueryPost",
    POST,
    "/{ds}/sparql",
    QUERY_PARAMS,
    &[],
    &[
        "application/x-www-form-urlencoded",
        "application/sparql-query"
    ]
);
op!(
    UPDATE,
    "update",
    POST,
    "/{ds}/update",
    &[
        "using-graph-uri",
        "using-named-graph-uri",
        "timeout",
        "dryRun",
        "receipt",
        "validate"
    ],
    WRITE_HEADERS,
    &[
        "application/sparql-update",
        "application/x-www-form-urlencoded"
    ]
);

// the Graph Store Protocol
op!(
    GSP_GET,
    "gspGet",
    GET,
    "/{ds}/data",
    &["graph", "default", "at", "reasoning"],
    &["If-None-Match"],
    &[]
);
op!(
    GSP_PUT,
    "gspPut",
    PUT,
    "/{ds}/data",
    GSP_WRITE_PARAMS,
    GSP_WRITE_HEADERS,
    RDF_BODIES
);
op!(
    GSP_POST,
    "gspPost",
    POST,
    "/{ds}/data",
    GSP_WRITE_PARAMS,
    GSP_WRITE_HEADERS,
    RDF_BODIES
);
op!(
    GSP_DELETE,
    "gspDelete",
    DELETE,
    "/{ds}/data",
    GSP_WRITE_PARAMS,
    GSP_WRITE_HEADERS,
    &[]
);
op!(
    UPLOAD,
    "upload",
    POST,
    "/{ds}/upload",
    &[
        "graph", "base", "key", "timeout", "dryRun", "receipt", "validate"
    ],
    WRITE_HEADERS,
    &["multipart/form-data"]
);

/// The RDF syntaxes the client writes Graph Store bodies in.
pub const RDF_BODIES: &[&str] = &[
    "application/n-triples",
    "application/n-quads",
    "text/turtle",
    "application/trig",
    "application/rdf+xml",
    "application/ld+json",
];

// stored queries
op!(
    RUN_STORED,
    "runStoredQueryPost",
    POST,
    "/{ds}/queries/{name}",
    &["timeout", "reasoning", "at", "version"],
    &[],
    &["application/json"]
);
op!(LIST_STORED, "listStoredQueries", GET, "/$/queries/{ds}");
op!(
    GET_STORED,
    "getStoredQuery",
    GET,
    "/$/queries/{ds}/{name}",
    &["version"]
);
op!(
    PUT_STORED,
    "putStoredQuery",
    PUT,
    "/$/queries/{ds}/{name}",
    &[],
    &["If-Match", "If-None-Match", "Sparkles-Commit-Message"],
    &["application/json"]
);
op!(
    DELETE_STORED,
    "deleteStoredQuery",
    DELETE,
    "/$/queries/{ds}/{name}",
    &[],
    &["If-Match"],
    &[]
);

// commits, statistics, schema
op!(
    LIST_COMMITS,
    "listCommits",
    GET,
    "/$/commits/{ds}",
    &["limit", "before", "after"]
);
op!(GET_COMMIT, "getCommit", GET, "/$/commits/{ds}/{reference}");
op!(
    DATASET_STATS,
    "getDatasetStats",
    GET,
    "/$/stats/{ds}",
    &["at"]
);
op!(SERVER_STATS, "fusekiStats", GET, "/$/stats");
op!(
    SCHEMA,
    "getSchema",
    GET,
    "/$/schema/{ds}",
    &["at", "reasoning", "limit", "timeout", "graph"]
);

// datasets
op!(LIST_DATASETS, "listDatasets", GET, "/$/datasets");
op!(GET_DATASET, "getDataset", GET, "/$/datasets/{ds}");
op!(
    CREATE_DATASET,
    "createDataset",
    POST,
    "/$/datasets",
    &["dbName", "dbType"]
);
op!(DELETE_DATASET, "deleteDataset", DELETE, "/$/datasets/{ds}");

// backups and tasks
op!(
    BACKUP_NQUADS,
    "backupNquads",
    POST,
    "/$/backup/{ds}",
    &["compression", "level"]
);
op!(
    DATASET_BACKUPS,
    "listDatasetBackups",
    GET,
    "/$/backups/{ds}",
    &["repository"]
);
op!(BACKUP_FILES, "listBackupFiles", GET, "/$/backups-list");
op!(LIST_TASKS, "listTasks", GET, "/$/tasks");
op!(GET_TASK, "getTask", GET, "/$/tasks/{id}");
op!(CANCEL_TASK, "cancelTask", DELETE, "/$/tasks/{id}");

// the server
op!(PING, "ping", GET, "/$/ping");
op!(SERVER, "getServer", GET, "/$/server");
op!(WHOAMI, "whoami", GET, "/$/whoami");

/// Every operation above, for the contract test and [`crate::Client::call_json`].
pub const ALL: &[&Op] = &[
    &SPARQL_GET,
    &SPARQL_POST,
    &UPDATE,
    &GSP_GET,
    &GSP_PUT,
    &GSP_POST,
    &GSP_DELETE,
    &UPLOAD,
    &RUN_STORED,
    &LIST_STORED,
    &GET_STORED,
    &PUT_STORED,
    &DELETE_STORED,
    &LIST_COMMITS,
    &GET_COMMIT,
    &DATASET_STATS,
    &SERVER_STATS,
    &SCHEMA,
    &LIST_DATASETS,
    &GET_DATASET,
    &CREATE_DATASET,
    &DELETE_DATASET,
    &BACKUP_NQUADS,
    &DATASET_BACKUPS,
    &BACKUP_FILES,
    &LIST_TASKS,
    &GET_TASK,
    &CANCEL_TASK,
    &PING,
    &SERVER,
    &WHOAMI,
];

/// Characters escaped in a path segment: everything but RFC 3986's unreserved ones.
const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

impl Op {
    /// The path with its parameters filled in, each percent-encoded as one segment.
    pub fn fill(&self, params: &[(&str, &str)]) -> Result<String, String> {
        let mut out = String::with_capacity(self.path.len() + 16);
        let mut rest = self.path;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let end = rest[start..]
                .find('}')
                .map(|e| start + e)
                .ok_or_else(|| format!("{}: unclosed parameter", self.id))?;
            let name = &rest[start + 1..end];
            let value = params
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| *v)
                .ok_or_else(|| format!("{}: no value for path parameter {name}", self.id))?;
            out.extend(percent_encoding::utf8_percent_encode(value, SEGMENT));
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value, json};
    use std::collections::BTreeSet;

    fn openapi() -> Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/openapi.json");
        let text =
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()));
        serde_json::from_str(&text).unwrap()
    }

    /// Follow a `$ref` within the document.
    fn resolve<'a>(doc: &'a Value, v: &'a Value) -> &'a Value {
        match v.get("$ref").and_then(Value::as_str) {
            Some(r) => {
                let mut cur = doc;
                for part in r.trim_start_matches("#/").split('/') {
                    cur = &cur[part];
                }
                resolve(doc, cur)
            }
            None => v,
        }
    }

    #[test]
    fn every_operation_matches_the_openapi_description() {
        let doc = openapi();
        let mut ids = BTreeSet::new();
        for op in ALL {
            assert!(ids.insert(op.id), "{} is listed twice", op.id);
            let item = &doc["paths"][op.path];
            assert!(
                item.is_object(),
                "{}: path {} is not described",
                op.id,
                op.path
            );
            let o = &item[op.method.as_str().to_ascii_lowercase()];
            assert!(
                o.is_object(),
                "{}: {} {} is not described",
                op.id,
                op.method,
                op.path
            );
            assert_eq!(o["operationId"], op.id, "{} {}", op.method, op.path);
            let mut params = BTreeSet::new();
            for p in o["parameters"].as_array().into_iter().flatten() {
                let p = resolve(&doc, p);
                params.insert((
                    p["in"].as_str().unwrap().to_string(),
                    p["name"].as_str().unwrap().to_ascii_lowercase(),
                ));
            }
            for q in op.query {
                assert!(
                    params.contains(&("query".to_string(), q.to_ascii_lowercase())),
                    "{}: query parameter {q} is not declared",
                    op.id
                );
            }
            for h in op.headers {
                assert!(
                    params.contains(&("header".to_string(), h.to_ascii_lowercase())),
                    "{}: header {h} is not declared",
                    op.id
                );
            }
            // every `{param}` of the template is a declared path parameter
            for seg in op.path.split('/') {
                if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                    assert!(
                        params.contains(&("path".to_string(), name.to_ascii_lowercase())),
                        "{}: path parameter {name} is not declared",
                        op.id
                    );
                }
            }
            if !op.body.is_empty() {
                let body = resolve(&doc, &o["requestBody"]);
                let content = body["content"]
                    .as_object()
                    .unwrap_or_else(|| panic!("{}: no request body", op.id));
                for m in op.body {
                    assert!(
                        content.contains_key(*m),
                        "{}: body media type {m} is not declared ({:?})",
                        op.id,
                        content.keys().collect::<Vec<_>>()
                    );
                }
            }
        }
    }

    /// A value of a JSON schema: the first example or enum value, else a value of its
    /// type, with objects holding `all` members or only the required ones.
    fn sample(doc: &Value, schema: &Value, all: bool, depth: usize) -> Value {
        let s = resolve(doc, schema);
        if let Some(c) = s.get("const") {
            return c.clone();
        }
        if let Some(v) = s["examples"].as_array().and_then(|a| a.first()) {
            return v.clone();
        }
        if let Some(v) = s["enum"].as_array().and_then(|a| a.first()) {
            return v.clone();
        }
        for k in ["oneOf", "anyOf"] {
            if let Some(alts) = s[k].as_array() {
                // prefer the non-null alternative
                let alt = alts
                    .iter()
                    .find(|a| resolve(doc, a)["type"] != "null")
                    .unwrap_or(&alts[0]);
                return sample(doc, alt, all, depth + 1);
            }
        }
        let ty = match &s["type"] {
            Value::Array(ts) => ts
                .iter()
                .find(|t| *t != "null")
                .cloned()
                .unwrap_or(Value::Null),
            t => t.clone(),
        };
        match ty.as_str() {
            Some("string") => json!("x"),
            Some("integer") => json!(1),
            Some("number") => json!(1.5),
            Some("boolean") => json!(true),
            Some("null") => Value::Null,
            Some("array") => {
                if depth > 4 {
                    return json!([]);
                }
                json!([sample(doc, &s["items"], all, depth + 1)])
            }
            _ => {
                let mut m = Map::new();
                if depth > 4 {
                    return Value::Object(m);
                }
                let required: BTreeSet<&str> = s["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                for (k, v) in s["properties"].as_object().into_iter().flatten() {
                    if all || required.contains(k.as_str()) {
                        m.insert(k.clone(), sample(doc, v, all, depth + 1));
                    }
                }
                Value::Object(m)
            }
        }
    }

    fn check<T: serde::de::DeserializeOwned>(doc: &Value, name: &str) {
        let schema = &doc["components"]["schemas"][name];
        assert!(schema.is_object(), "schema {name} is not described");
        for all in [false, true] {
            let v = sample(doc, schema, all, 0);
            if let Err(e) = serde_json::from_value::<T>(v.clone()) {
                panic!(
                    "schema {name} ({} members): {e}\n{v:#}",
                    if all { "all" } else { "required" }
                );
            }
        }
    }

    #[test]
    fn typed_bodies_read_the_described_schemas() {
        use crate::types::*;
        let doc = openapi();
        check::<Commit>(&doc, "Commit");
        check::<CommitList>(&doc, "CommitList");
        check::<ReceiptBody>(&doc, "Receipt");
        check::<Task>(&doc, "Task");
        check::<DatasetInfo>(&doc, "DatasetInfo");
        check::<ServerInfo>(&doc, "ServerInfo");
        check::<Whoami>(&doc, "Whoami");
        check::<CommitResponse>(&doc, "CommitResponse");
        check::<DatasetList>(&doc, "DatasetList");
    }

    /// The schema of an operation's first 2xx response: `Name`, or `[Name]` for an array.
    fn response_schema(doc: &Value, op: &Op) -> String {
        let o = &doc["paths"][op.path][op.method.as_str().to_ascii_lowercase()];
        let (_, resp) = o["responses"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(k, _)| k.starts_with('2'))
            .unwrap_or_else(|| panic!("{}: no success response", op.id));
        let resp = resolve(doc, resp);
        let schema = &resp["content"]["application/json"]["schema"];
        let name = |v: &Value| {
            v["$ref"]
                .as_str()
                .and_then(|r| r.rsplit('/').next())
                .unwrap_or("?")
                .to_string()
        };
        if schema["type"] == "array" {
            format!("[{}]", name(&schema["items"]))
        } else {
            name(schema)
        }
    }

    #[test]
    fn typed_operations_answer_the_typed_schemas() {
        let doc = openapi();
        for (op, schema) in [
            (&GET_COMMIT, "CommitResponse"),
            (&LIST_COMMITS, "CommitList"),
            (&LIST_TASKS, "[Task]"),
            (&GET_TASK, "Task"),
            (&CANCEL_TASK, "Task"),
            (&BACKUP_NQUADS, "Task"),
            (&GET_DATASET, "DatasetInfo"),
            (&LIST_DATASETS, "DatasetList"),
            (&SERVER, "ServerInfo"),
            (&WHOAMI, "Whoami"),
        ] {
            assert_eq!(response_schema(&doc, op), schema, "{}", op.id);
        }
    }

    #[test]
    fn paths_fill_and_encode() {
        assert_eq!(GSP_GET.fill(&[("ds", "my ds")]).unwrap(), "/my%20ds/data");
        assert_eq!(
            GET_COMMIT
                .fill(&[("ds", "a/b"), ("reference", "commit:4")])
                .unwrap(),
            "/$/commits/a%2Fb/commit%3A4"
        );
        assert!(GET_COMMIT.fill(&[("ds", "a")]).is_err());
    }
}
