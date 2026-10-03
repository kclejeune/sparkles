//! `components.schemas`: the bodies of the API as JSON Schema 2020-12.
//!
//! The common types are described member by member. Admin bodies that only `docs/API.md`
//! describes in full are open objects (`open`) that link to their section; the OpenAPI
//! description says so in its `info.description`.

use super::{api_doc, sref};
use serde_json::{Map, Value as J, json};

/// An object with these members, of which `required` must be present.
fn obj(required: &[&str], props: J) -> J {
    let mut o = json!({ "type": "object", "properties": props });
    if !required.is_empty() {
        o["required"] = json!(required);
    }
    o
}

/// An object described in full only in `docs/API.md` (at `anchor`); any member is allowed.
fn open(description: &str, anchor: &str) -> J {
    json!({
        "type": "object",
        "additionalProperties": true,
        "description": description,
        "externalDocs": { "url": api_doc(anchor) },
    })
}

fn array(items: J) -> J {
    json!({ "type": "array", "items": items })
}

fn nullable(t: &str) -> J {
    json!({ "type": [t, "null"] })
}

fn string_enum(values: &[&str]) -> J {
    json!({ "type": "string", "enum": values })
}

fn with_desc(mut v: J, d: &str) -> J {
    v["description"] = d.into();
    v
}

fn counts() -> J {
    obj(
        &["entries", "bytes", "hits", "misses"],
        json!({
            "entries": { "type": "integer" },
            "bytes": { "type": "integer" },
            "hits": { "type": "integer" },
            "misses": { "type": "integer" },
        }),
    )
}

pub(super) fn schemas() -> Map<String, J> {
    let mut s = Map::new();
    let mut put = |name: &str, v: J| {
        s.insert(name.to_string(), v);
    };

    // ------------------------------------------------------------------ errors --
    put(
        "Error",
        with_desc(
            json!({
                "type": "object",
                "required": ["error"],
                "properties": {
                    "error": { "type": "string", "description": "What went wrong, for people." },
                    "detail": { "type": "string", "description": "The whole message when `error` was cut." },
                    "line": { "type": "integer", "description": "1-based line of a syntax error." },
                    "column": { "type": "integer", "description": "1-based column of a syntax error, in characters." },
                    "code": { "type": "string", "description": "A machine-readable reason, such as `precondition-failed`, `history-gone`, `dataset-restoring` or a backup error code." },
                    "requestId": { "type": "string", "description": "The response's `X-Request-Id`." },
                },
                "additionalProperties": true,
            }),
            "The body of every non-2xx response. Some errors add members of their own.",
        ),
    );
    put(
        "BudgetError",
        json!({
            "allOf": [
                sref("Error"),
                obj(&["budget"], json!({
                    "budget": string_enum(&["memory", "result-bytes", "outbound-bytes", "validation-work", "rows", "rows-produced", "dataset-bytes"]),
                    "limit": { "type": "integer", "description": "Bytes, or rows for `rows` and `rows-produced`." },
                    "requested": { "type": "integer" },
                })),
            ],
            "description": "A `507` for a request over one of its budgets.",
            "externalDocs": { "url": api_doc("budgets") },
        }),
    );

    // ------------------------------------------------------------------ server --
    put(
        "Limits",
        with_desc(
            obj(
                &[],
                json!({
                    "timeoutSeconds": { "type": "number" },
                    "updateTimeoutSeconds": { "type": "number" },
                    "maxTimeoutSeconds": { "type": "number" },
                    "queryMemoryBytes": { "type": "integer" },
                    "maxResultBytes": { "type": "integer" },
                    "maxExportBytes": { "type": "integer" },
                    "maxRows": { "type": "integer" },
                    "maxRowsProduced": { "type": "integer" },
                    "maxDatasetBytes": { "type": "integer" },
                    "maxQueryBodyBytes": { "type": "integer" },
                    "maxUpdateBodyBytes": { "type": "integer" },
                    "maxAdminBodyBytes": { "type": "integer" },
                    "maxUploadBytes": { "type": "integer" },
                }),
            ),
            "The server's per-request budgets. 0 means unlimited.",
        ),
    );
    put(
        "ServerInfo",
        obj(
            &["startedAt", "uptimeSeconds", "readOnly", "datasets", "auth"],
            json!({
                "version": { "type": "string", "description": "Left out for anonymous callers when auth is on." },
                "startedAt": { "type": "string", "format": "date-time" },
                "uptimeSeconds": { "type": "number" },
                "readOnly": { "type": "boolean" },
                "datasets": array(sref("DatasetInfo")),
                "limits": sref("Limits"),
                "auth": obj(&["enabled"], json!({ "enabled": { "type": "boolean" } })),
                "startDateTime": { "type": "string", "description": "Fuseki's name for `startedAt`." },
                "uptime": { "type": "number", "description": "Fuseki's uptime in seconds." },
            }),
        ),
    );
    put(
        "ReadyInfo",
        obj(
            &["status", "ready", "uptimeSeconds", "datasets"],
            json!({
                "status": string_enum(&["starting", "ready", "draining"]),
                "ready": { "type": "boolean" },
                "uptimeSeconds": { "type": "number" },
                "datasets": array(obj(&["name", "type", "state", "ready"], json!({
                    "name": { "type": "string" },
                    "type": string_enum(&["persistent", "mem"]),
                    "state": { "type": "string" },
                    "ready": { "type": "boolean" },
                    "generation": { "type": "string" },
                    "walBytes": { "type": "integer" },
                    "deltaQuads": { "type": "integer" },
                }))),
            }),
        ),
    );
    put(
        "MetricsSnapshot",
        open(
            "The counters of `/$/metrics` as JSON (`?format=json`).",
            "metrics",
        ),
    );
    put(
        "Whoami",
        obj(
            &["authEnabled", "principal", "server", "datasets"],
            json!({
                "authEnabled": { "type": "boolean" },
                "principal": obj(&["kind"], json!({
                    "kind": string_enum(&["local", "anonymous", "user", "token", "oidc", "proxy"]),
                    "name": { "type": "string" },
                    "displayName": { "type": "string" },
                    "groups": array(json!({ "type": "string" })),
                    "owner": { "type": "string", "description": "Tokens: the owner, such as `oidc:alice@example.org`." },
                })),
                "method": string_enum(&["none", "basic", "bearer", "session", "proxy"]),
                "expires": { "type": "string", "format": "date-time" },
                "csrfToken": { "type": "string", "description": "Session and proxy principals send it as `X-Sparkles-CSRF` on unsafe requests." },
                "tokenId": { "type": "string" },
                "server": array(string_enum(&["metrics", "federate", "server-admin"])),
                "datasets": {
                    "type": "object",
                    "additionalProperties": sref("Level"),
                    "description": "The caller's level on each existing dataset it may use.",
                },
                "restricted": {
                    "type": "object",
                    "additionalProperties": obj(&["graphs"], json!({
                        "graphs": { "type": "boolean" },
                        "endpoints": array(json!({ "type": "string" })),
                    })),
                },
                "canMintTokens": { "type": "boolean" },
                "logout": { "type": "boolean" },
                "tokensPolicy": obj(&[], json!({
                    "defaultTtlSeconds": { "type": "integer" },
                    "maxTtlSeconds": { "type": "integer" },
                })),
            }),
        ),
    );
    put("Level", string_enum(&["read", "write", "admin"]));

    // ---------------------------------------------------------------- datasets --
    put(
        "DatasetInfo",
        json!({
            "type": "object",
            "required": ["name", "type", "endpoints", "quads"],
            "additionalProperties": true,
            "properties": {
                "name": { "type": "string" },
                "type": string_enum(&["persistent", "mem"]),
                "id": { "type": "string", "format": "uuid", "description": "The dataset id." },
                "head": { "type": "integer", "description": "The head commit." },
                "modified": { "type": "string", "format": "date-time", "description": "The head commit's time." },
                "endpoints": obj(&["query", "update", "gsp", "upload"], json!({
                    "query": { "type": "string" },
                    "update": { "type": "string" },
                    "gsp": { "type": "string" },
                    "upload": { "type": "string" },
                    "shacl": { "type": "string" },
                    "shex": { "type": "string" },
                })),
                "quads": { "type": "integer", "description": "Approximate total, base plus delta." },
                "reasoning": {
                    "oneOf": [{ "type": "null" }, obj(&["profile", "inferred", "at"], json!({
                        "profile": { "type": "string" },
                        "inferred": { "type": "integer" },
                        "at": { "type": "string", "format": "date-time" },
                        "commit": nullable("integer"),
                        "stale": nullable("boolean"),
                        "commitsSince": nullable("integer"),
                    }))],
                },
                "forkedFrom": obj(&["id", "seq"], json!({ "id": { "type": "string" }, "seq": { "type": "integer" } })),
                "origin": { "type": "object", "description": "Clones: the clone's `origin.json`." },
                "restoredFrom": { "type": "object", "description": "Restored from a backup repository." },
                "access": sref("Level"),
                "text": { "oneOf": [{ "type": "null" }, obj(&["state", "docs"], json!({ "state": { "type": "string" }, "docs": { "type": "integer" } }))] },
                "geo": { "oneOf": [{ "type": "null" }, obj(&["state", "rows"], json!({ "state": { "type": "string" }, "rows": { "type": "integer" } }))] },
                "ds.name": { "type": "string", "description": "Fuseki's dataset path, such as `/ds`." },
                "ds.state": { "type": "boolean", "description": "Fuseki's state; false while offline." },
                "ds.services": array(json!({ "type": "object" })),
            },
        }),
    );
    put(
        "DatasetList",
        obj(
            &["datasets"],
            json!({ "datasets": array(sref("DatasetInfo")) }),
        ),
    );
    put(
        "CreateDataset",
        obj(
            &["dbName"],
            json!({
                "dbName": { "type": "string", "pattern": "^[A-Za-z0-9_.-]+$" },
                "dbType": { "type": "string", "enum": ["persistent", "mem", "tdb2", "tdb"], "default": "persistent" },
                "geo": { "description": "`true` for a spatial index with the defaults, or a `GeoConfig`.", "oneOf": [{ "type": "boolean" }, { "type": "object" }] },
            }),
        ),
    );
    put(
        "CloneRequest",
        obj(
            &["name"],
            json!({
                "name": { "type": "string" },
                "inferences": { "type": "string", "enum": ["copy", "drop"], "default": "copy" },
                "at": { "type": "string", "description": "A point-in-time selector." },
            }),
        ),
    );
    put(
        "Task",
        obj(
            &["id", "kind", "dataset", "state", "startedAt", "cancellable"],
            json!({
                "id": { "type": "string" },
                "kind": { "type": "string", "examples": ["compact", "backup", "reason", "load", "clone", "text-rebuild", "geo-index", "backup-create", "backup-restore", "backup-verify", "backup-gc", "backup-policy"] },
                "dataset": { "type": "string", "description": "Empty for a server-wide task." },
                "target": { "type": "string" },
                "state": string_enum(&["queued", "running", "done", "failed", "cancelled"]),
                "startedAt": { "type": "string", "format": "date-time" },
                "finishedAt": { "type": "string", "format": "date-time" },
                "progress": { "type": "number", "minimum": 0, "maximum": 1 },
                "message": { "type": "string" },
                "cancellable": { "type": "boolean" },
                "detail": { "type": "object", "description": "A typed result, for task kinds that have one." },
                "taskId": { "type": "string" },
                "task": { "type": "string" },
                "started": { "type": "string" },
                "finished": { "type": "string" },
                "success": { "type": "boolean" },
            }),
        ),
    );
    put(
        "DatasetQuota",
        obj(
            &[
                "dataset",
                "maxBytes",
                "source",
                "defaultMaxBytes",
                "usedBytes",
            ],
            json!({
                "dataset": { "type": "string" },
                "maxBytes": nullable("integer"),
                "source": string_enum(&["dataset", "default"]),
                "defaultMaxBytes": nullable("integer"),
                "usedBytes": { "type": "integer" },
            }),
        ),
    );
    put(
        "QuotaRequest",
        json!({
            "type": "object",
            "description": "One of the two members; 0 means unlimited.",
            "properties": { "maxBytes": { "type": "integer" }, "maxMb": { "type": "integer" } },
        }),
    );
    put(
        "DatasetStats",
        json!({
            "type": "object",
            "additionalProperties": true,
            "required": ["name", "quads"],
            "properties": {
                "name": { "type": "string" },
                "quads": { "type": "integer" },
                "baseQuads": { "type": "integer" },
                "deltaInserts": { "type": "integer" },
                "deltaDeletes": { "type": "integer" },
                "terms": { "type": "integer" },
                "graphs": array(obj(&["name", "quads"], json!({ "name": nullable("string"), "quads": { "type": "integer" } }))),
                "diskBytes": { "type": "integer" },
                "quota": { "oneOf": [{ "type": "null" }, sref("DatasetQuota")] },
                "cache": counts(),
                "compaction": sref("CompactionStatus"),
            },
            "externalDocs": { "url": api_doc("datasets-admin") },
        }),
    );
    put(
        "FusekiStats",
        open(
            "Fuseki's request counters, `{datasets: {\"/ds\": FusekiCounters}}`.",
            "datasets-admin",
        ),
    );
    put(
        "Prefixes",
        obj(
            &["prefixes"],
            json!({ "prefixes": { "type": "object", "additionalProperties": { "type": "string" } } }),
        ),
    );
    put(
        "PrefixBinding",
        obj(
            &["prefix", "uri"],
            json!({ "prefix": { "type": "string" }, "uri": { "type": "string" } }),
        ),
    );
    put(
        "CacheCleared",
        obj(
            &["cleared", "bytes"],
            json!({ "cleared": { "type": "integer" }, "bytes": { "type": "integer" } }),
        ),
    );
    put(
        "DescribeSetting",
        obj(
            &[],
            json!({
                "mode": string_enum(&["cbd", "scbd", "outgoing"]),
                "labels": { "type": "boolean" },
                "reifiers": { "type": "boolean" },
                "maxTriples": nullable("integer"),
                "maxDepth": nullable("integer"),
            }),
        ),
    );
    put(
        "DescribeStatus",
        obj(
            &[
                "mode",
                "labels",
                "reifiers",
                "maxTriples",
                "maxDepth",
                "source",
                "modes",
            ],
            json!({
                "mode": string_enum(&["cbd", "scbd", "outgoing"]),
                "labels": { "type": "boolean" },
                "reifiers": { "type": "boolean" },
                "maxTriples": nullable("integer"),
                "maxDepth": nullable("integer"),
                "source": string_enum(&["dataset", "default"]),
                "modes": array(json!({ "type": "string" })),
            }),
        ),
    );
    put(
        "CompactionStatus",
        open(
            "The dataset's automatic compaction: its settings, state, measures and last run.",
            "automatic-compaction",
        ),
    );
    put(
        "CompactionPolicy",
        open(
            "Compaction settings: `minDeltaQuads`, `deltaRatio`, `maxDeltaQuads`, `maxDeltaMb`, `maxWalMb`, `idleSeconds`, `maxAgeSeconds`, `minIntervalSeconds` and `enabled`.",
            "automatic-compaction",
        ),
    );
    put(
        "ValidatorResult",
        open("The JSON answer of a Fuseki validator.", "validators"),
    );

    // ------------------------------------------------------------------ schema --
    put(
        "Lit",
        obj(
            &["value"],
            json!({ "value": { "type": "string" }, "lang": { "type": "string" } }),
        ),
    );
    put(
        "ClassEntry",
        json!({
            "type": "object",
            "required": ["iri", "builtin", "observed", "declared"],
            "additionalProperties": true,
            "properties": {
                "iri": { "type": "string" },
                "builtin": { "type": "boolean" },
                "observed": obj(&["instances"], json!({ "instances": { "type": "integer" } })),
                "declared": {
                    "type": "object",
                    "additionalProperties": true,
                    "properties": {
                        "types": array(json!({ "type": "string" })),
                        "superClasses": array(json!({ "type": "string" })),
                        "labels": array(sref("Lit")),
                        "comments": array(sref("Lit")),
                    },
                },
            },
        }),
    );
    put(
        "PredicateEntry",
        json!({
            "type": "object",
            "required": ["iri", "builtin", "observed", "declared"],
            "additionalProperties": true,
            "properties": {
                "iri": { "type": "string" },
                "builtin": { "type": "boolean" },
                "observed": obj(&["triples", "distinctSubjects", "distinctObjects"], json!({
                    "triples": { "type": "integer" },
                    "distinctSubjects": { "type": "integer" },
                    "distinctObjects": { "type": "integer" },
                    "maxPerSubject": { "type": "integer" },
                    "subjectsWithMultiple": { "type": "integer" },
                    "objects": { "type": "object" },
                })),
                "declared": { "type": "object" },
            },
        }),
    );
    for (page, item) in [
        ("ClassPage", "ClassEntry"),
        ("PredicatePage", "PredicateEntry"),
    ] {
        put(
            page,
            with_desc(
                obj(
                    &["items", "total", "next"],
                    json!({
                        "items": array(sref(item)),
                        "total": { "type": "integer" },
                        "next": { "type": ["string", "null"], "description": "The cursor of the next page, or null on the last page." },
                    }),
                ),
                "One page of a cursor-paginated listing, in IRI order.",
            ),
        );
    }
    put(
        "SchemaSummary",
        json!({
            "type": "object",
            "required": ["schemaFormat", "dataset", "snapshot", "selection", "totals", "classes", "predicates"],
            "additionalProperties": true,
            "properties": {
                "schemaFormat": { "const": 1 },
                "dataset": { "type": "string" },
                "snapshot": obj(&["version", "generation", "computedAt"], json!({
                    "version": { "type": "integer" },
                    "generation": { "type": "string" },
                    "computedAt": { "type": "string", "format": "date-time" },
                })),
                "selection": { "type": "object" },
                "totals": { "type": "object" },
                "ontology": array(json!({ "type": "object" })),
                "hierarchy": { "type": "object" },
                "classes": sref("ClassPage"),
                "predicates": sref("PredicatePage"),
                "constraints": { "type": "object" },
            },
            "externalDocs": { "url": api_doc("schema-discovery") },
        }),
    );
    put(
        "ConstraintsLayer",
        open(
            "The SHACL constraints layer of a schema report.",
            "constraints-layer",
        ),
    );

    // ----------------------------------------------------------------- commits --
    put(
        "Commit",
        json!({
            "type": "object",
            "required": ["seq", "parent", "ref", "kind", "inserted", "deleted", "quads", "generation", "bulk", "exact"],
            "additionalProperties": true,
            "properties": {
                "seq": { "type": "integer" },
                "parent": nullable("integer"),
                "ref": { "type": "string", "examples": ["commit:42"] },
                "timestamp": { "type": "string", "format": "date-time" },
                "kind": { "type": "string", "examples": ["create", "baseline", "update", "gsp-put", "gsp-post", "gsp-delete", "upload", "load", "reason", "reason-clear", "transaction", "embed", "patch", "unknown"] },
                "inserted": { "type": "integer" },
                "deleted": { "type": "integer" },
                "quads": { "type": "integer" },
                "generation": { "type": "string" },
                "bulk": { "type": "boolean" },
                "exact": { "type": "boolean" },
                "unvalidated": { "const": true },
                "message": { "type": "string" },
                "digest": { "type": "string" },
            },
        }),
    );
    put(
        "Receipt",
        obj(
            &["dataset", "datasetId", "committed", "commit"],
            json!({
                "dataset": { "type": "string" },
                "datasetId": { "type": "string" },
                "committed": { "type": "boolean" },
                "commit": sref("Commit"),
            }),
        ),
    );
    put(
        "CommitList",
        json!({
            "type": "object",
            "required": ["dataset", "datasetId", "head", "firstRetained", "complete", "commits", "next"],
            "additionalProperties": true,
            "properties": {
                "dataset": { "type": "string" },
                "datasetId": { "type": "string" },
                "head": { "type": "integer" },
                "firstRetained": { "type": "integer" },
                "complete": { "type": "boolean" },
                "oldestReconstructable": nullable("integer"),
                "commits": array(sref("Commit")),
                "next": { "type": ["string", "null"], "description": "The URL of the next page." },
            },
        }),
    );
    put(
        "CommitResponse",
        obj(
            &["dataset", "datasetId", "commit"],
            json!({
                "dataset": { "type": "string" },
                "datasetId": { "type": "string" },
                "commit": sref("Commit"),
            }),
        ),
    );
    put(
        "Diff",
        open(
            "The net change between two states: counts, and the quads with `quads=true`.",
            "diffs-between-commits",
        ),
    );
    put(
        "ChangeFeed",
        open(
            "The commits after `after`, oldest first, with their changes, and `next`.",
            "change-feed",
        ),
    );
    put(
        "HistoryChanges",
        open(
            "`{dataset, datasetId, head, from, to, truncated, changes: [{op, subject, predicate, object, graph, commit, timestamp, kind, author?, message?}], unrecorded: [{from, to, reason}]}`",
            "history-queries",
        ),
    );
    put(
        "NamedSnapshot",
        open(
            "A named snapshot: the commit it pins and its expiry.",
            "named-snapshots-and-retention",
        ),
    );
    put(
        "SnapshotList",
        open(
            "`{dataset, datasetId, head, snapshots: NamedSnapshot[]}`",
            "named-snapshots-and-retention",
        ),
    );
    put(
        "HistoryStatus",
        open(
            "The retention window, the kept generations and the history cache.",
            "named-snapshots-and-retention",
        ),
    );
    put(
        "DryRunReport",
        open(
            "The preview of a write run with `dryRun=true`: the commit it would make, its changes, validation, preconditions and storage.",
            "write-previews",
        ),
    );

    // ------------------------------------------------------------------ SPARQL --
    put(
        "RdfTerm",
        json!({
            "type": "object",
            "required": ["type", "value"],
            "properties": {
                "type": string_enum(&["uri", "bnode", "literal", "triple"]),
                "value": { "oneOf": [{ "type": "string" }, sref("TripleTerm")] },
                "datatype": { "type": "string" },
                "xml:lang": { "type": "string" },
                "its:dir": string_enum(&["ltr", "rtl"]),
            },
        }),
    );
    put(
        "TripleTerm",
        obj(
            &["subject", "predicate", "object"],
            json!({ "subject": sref("RdfTerm"), "predicate": sref("RdfTerm"), "object": sref("RdfTerm") }),
        ),
    );
    put(
        "SparqlResults",
        with_desc(
            obj(
                &["head"],
                json!({
                    "head": obj(&[], json!({
                        "vars": array(json!({ "type": "string" })),
                        "link": array(json!({ "type": "string" })),
                    })),
                    "results": obj(&["bindings"], json!({
                        "bindings": array(json!({ "type": "object", "additionalProperties": sref("RdfTerm") })),
                    })),
                    "boolean": { "type": "boolean" },
                }),
            ),
            "SPARQL 1.1 Query Results JSON: `results` for SELECT, `boolean` for ASK.",
        ),
    );
    put(
        "PlanNode",
        json!({
            "type": "object",
            "required": ["operator", "description", "children"],
            "additionalProperties": true,
            "properties": {
                "operator": { "type": "string" },
                "description": { "type": "string" },
                "columns": array(json!({ "type": "string" })),
                "sortedOn": array(json!({ "type": "string" })),
                "estimatedRows": { "type": "number" },
                "estimatedCost": { "type": "number" },
                "actualRows": { "type": "number" },
                "timeMs": { "type": "number" },
                "cached": { "type": "boolean" },
                "children": array(sref("PlanNode")),
                "counters": { "type": "object" },
                "warnings": array(obj(&["code", "message"], json!({ "code": { "type": "string" }, "message": { "type": "string" } }))),
            },
        }),
    );
    put(
        "SparklesResult",
        json!({
            "type": "object",
            "required": ["queryType", "meta"],
            "description": "`application/x-sparkles+json`, the UI's result format.",
            "properties": {
                "queryType": string_enum(&["SELECT", "ASK", "CONSTRUCT", "DESCRIBE"]),
                "vars": array(json!({ "type": "string" })),
                "rows": array(array(json!({ "oneOf": [{ "type": "null" }, sref("RdfTerm")] }))),
                "boolean": { "type": "boolean" },
                "triples": array(array(sref("RdfTerm"))),
                "meta": {
                    "type": "object",
                    "additionalProperties": true,
                    "properties": {
                        "totalRows": { "type": "integer" },
                        "sentRows": { "type": "integer" },
                        "timing": { "type": "object" },
                        "plan": sref("PlanNode"),
                        "memory": { "type": "object" },
                        "rowsProduced": { "type": "integer" },
                        "commit": { "type": "integer" },
                        "datasetId": { "type": "string" },
                    },
                },
            },
            "externalDocs": { "url": api_doc("applicationx-sparklesjson-ui-result-format") },
        }),
    );
    put(
        "Explain",
        obj(
            &["algebra", "plan"],
            json!({
                "algebra": { "type": "string", "description": "The algebra in SSE." },
                "plan": sref("PlanNode"),
            }),
        ),
    );
    put(
        "UpdateResult",
        json!({
            "type": "object",
            "additionalProperties": true,
            "description": "Statistics of a SPARQL update. With `receipt=true` or `Accept: application/x-sparkles+json` the body also has the members of a `Receipt`.",
            "properties": {
                "inserted": { "type": "integer" },
                "deleted": { "type": "integer" },
                "operations": { "type": "integer" },
                "memPeakBytes": { "type": "integer" },
                "rowsProduced": { "type": "integer" },
                "timing": { "type": "object" },
            },
        }),
    );
    put(
        "PatchResult",
        json!({
            "type": "object",
            "additionalProperties": true,
            "description": "What applying an RDF Patch did. With `receipt=true` or `Accept: application/x-sparkles+json` the body also has the members of a `Receipt`.",
            "properties": {
                "committed": { "type": "boolean", "description": "Whether the patch made a commit." },
                "inserted": { "type": "integer", "description": "`A` rows that added a quad." },
                "deleted": { "type": "integer", "description": "`D` rows that removed a quad." },
                "prefixesSet": { "type": "integer" },
                "prefixesRemoved": { "type": "integer" },
                "rows": { "type": "integer", "description": "The rows read, up to a `TA` that aborted the patch." },
                "aborted": { "type": "boolean", "description": "A `TA` row aborted the patch, and nothing was applied." },
                "prevChecked": { "type": "boolean", "description": "The patch's `prev` header named a commit of this dataset, which was the head." },
                "timing": { "type": "object" },
            },
        }),
    );
    put(
        "WriteCount",
        json!({
            "type": "object",
            "additionalProperties": true,
            "description": "Fuseki's answer to a Graph Store write or an upload, the quads added. With a receipt it also has the members of a `Receipt`. Uploads of tables add `tables`.",
            "properties": {
                "count": { "type": "integer" },
                "tripleCount": { "type": "integer" },
                "quadCount": { "type": "integer" },
                "tables": array(json!({ "type": "object" })),
            },
        }),
    );

    // --------------------------------------------------------------- formatter --
    put(
        "FormatOptions",
        obj(
            &[],
            json!({
                "lineWidth": { "type": "integer", "minimum": 40, "maximum": 400, "default": 100 },
                "indentWidth": { "type": "integer", "minimum": 1, "maximum": 8, "default": 2 },
                "prefixGroups": array(array(json!({ "type": "string" }))),
                "typeShorthand": { "type": "boolean", "default": true },
                "compactIris": { "type": "boolean", "default": true },
                "quoteStyle": { "type": "string", "enum": ["double", "preserve"], "default": "double" },
                "operatorPosition": { "type": "string", "enum": ["leading", "trailing"], "default": "leading" },
                "alignValues": { "type": "boolean", "default": false },
                "prunePrefixes": { "type": "boolean", "default": false },
                "directiveStyle": { "type": "string", "enum": ["sparql", "turtle"], "default": "sparql" },
                "turtleLayout": { "type": "string", "enum": ["diff", "conventional"], "default": "diff" },
                "sort": { "type": "boolean", "default": false },
            }),
        ),
    );
    put(
        "FormatRequest",
        obj(
            &["text"],
            json!({
                "text": { "type": "string" },
                "language": sref("FormatLanguage"),
                "cursorOffset": { "type": "integer", "description": "In UTF-16 code units." },
                "options": sref("FormatOptions"),
            }),
        ),
    );
    put(
        "FormatLanguage",
        string_enum(&["sparql", "turtle", "trig", "ntriples", "nquads", "jsonld"]),
    );
    put(
        "FormatResult",
        obj(
            &["text", "changed", "language", "cursorOffset", "warnings"],
            json!({
                "text": { "type": "string" },
                "changed": { "type": "boolean" },
                "language": { "type": "string" },
                "cursorOffset": nullable("integer"),
                "warnings": array(obj(&["code", "message", "line", "column"], json!({
                    "code": { "type": "string" },
                    "message": { "type": "string" },
                    "line": { "type": "integer" },
                    "column": { "type": "integer" },
                }))),
            }),
        ),
    );

    // ---------------------------------------------------------- authentication --
    put(
        "AuthConfig",
        json!({
            "type": "object",
            "required": ["enabled"],
            "additionalProperties": true,
            "properties": {
                "enabled": { "type": "boolean" },
                "methods": array(string_enum(&["oidc", "token", "password", "proxy"])),
                "oidc": obj(&[], json!({ "loginUrl": { "type": "string" }, "displayName": { "type": "string" } })),
                "cli": obj(&[], json!({
                    "authorizeUrl": { "type": "string" },
                    "deviceAuthorizationEndpoint": { "type": "string" },
                    "tokenEndpoint": { "type": "string" },
                    "deviceVerificationUri": { "type": "string" },
                })),
            },
        }),
    );
    put(
        "LoginRequest",
        json!({
            "type": "object",
            "description": "A user and password, or an API token when `session.token_login` is on.",
            "properties": {
                "user": { "type": "string" },
                "password": { "type": "string", "format": "password" },
                "token": { "type": "string", "format": "password" },
            },
        }),
    );
    put(
        "Logout",
        obj(&["redirect"], json!({ "redirect": nullable("string") })),
    );
    put(
        "TokenRequest",
        obj(
            &[],
            json!({
                "name": { "type": "string" },
                "datasets": { "type": "object", "additionalProperties": sref("Level") },
                "server": array(string_enum(&["metrics", "federate", "server-admin"])),
                "expiresIn": { "type": "string", "examples": ["30d"] },
            }),
        ),
    );
    put(
        "TokenInfo",
        json!({
            "type": "object",
            "required": ["id"],
            "additionalProperties": true,
            "properties": {
                "id": { "type": "string", "examples": ["tok_…"] },
                "name": { "type": "string" },
                "scope": { "type": "object" },
                "created": { "type": "string", "format": "date-time" },
                "expires": nullable("string"),
                "lastUsed": nullable("string"),
                "via": { "type": "string" },
                "client": { "type": "string" },
                "owner": { "type": "string" },
            },
        }),
    );
    put(
        "TokenCreated",
        json!({
            "allOf": [
                sref("TokenInfo"),
                obj(&["token"], json!({ "token": { "type": "string", "description": "The secret, `spk_…`. It appears only in this response." } })),
            ],
        }),
    );
    put(
        "TokenList",
        obj(&["tokens"], json!({ "tokens": array(sref("TokenInfo")) })),
    );
    put(
        "Revoked",
        obj(&["revoked"], json!({ "revoked": { "type": "integer" } })),
    );
    put(
        "DeviceAuthorization",
        obj(
            &[
                "device_code",
                "user_code",
                "verification_uri",
                "expires_in",
                "interval",
            ],
            json!({
                "device_code": { "type": "string" },
                "user_code": { "type": "string", "examples": ["WDJB-MJHT"] },
                "verification_uri": { "type": "string" },
                "verification_uri_complete": { "type": "string" },
                "expires_in": { "type": "integer" },
                "interval": { "type": "integer" },
            }),
        ),
    );
    put(
        "TokenGrant",
        json!({
            "type": "object",
            "required": ["grant_type"],
            "description": "RFC 8628 device code grant, or the authorization code grant of the browser login with PKCE.",
            "properties": {
                "grant_type": string_enum(&["urn:ietf:params:oauth:grant-type:device_code", "authorization_code"]),
                "device_code": { "type": "string" },
                "code": { "type": "string" },
                "code_verifier": { "type": "string" },
            },
        }),
    );
    put(
        "TokenGrantResponse",
        json!({
            "type": "object",
            "required": ["access_token", "token_type"],
            "additionalProperties": true,
            "properties": {
                "access_token": { "type": "string" },
                "token_type": { "const": "Bearer" },
                "expires_in": { "type": "integer" },
                "token_id": { "type": "string" },
                "principal": { "type": "string" },
            },
        }),
    );
    put(
        "OAuthError",
        obj(
            &["error"],
            json!({
                "error": { "type": "string", "examples": ["authorization_pending", "slow_down", "access_denied", "expired_token", "invalid_request"] },
                "error_description": { "type": "string" },
            }),
        ),
    );

    // ------------------------------------------------------ open admin bodies --
    for (name, desc, anchor) in [
        (
            "ReasonRequest",
            "`{profile: \"rdfs\" | \"owl-rl\" | \"rules\", rules?, vocabularies?, geoDefaultGeometry?}`",
            "datasets-admin",
        ),
        (
            "ReasoningStatus",
            "The materialized inferences of a dataset and whether they are current.",
            "reasoning-status-and-diagnostics",
        ),
        (
            "AutoReasonRequest",
            "`{enabled, debounceSeconds?, maxDelaySeconds?}`",
            "reasoning-status-and-diagnostics",
        ),
        (
            "DiagnosticsReport",
            "OWL 2 RL inconsistency checks.",
            "reasoning-status-and-diagnostics",
        ),
        (
            "TextStatus",
            "The full-text index's configuration and state, or `{enabled: false}`.",
            "full-text-search",
        ),
        (
            "TextConfig",
            "The full-text index's configuration.",
            "full-text-search",
        ),
        (
            "TextHits",
            "Full-text search hits, best first.",
            "full-text-search",
        ),
        (
            "VectorStatus",
            "`{budgetBytes, usedBytes, generation, indexes, predicates}`",
            "vector-indexes",
        ),
        (
            "VectorIndexStatus",
            "One vector index: its configuration and state.",
            "vector-indexes",
        ),
        (
            "VectorIndexConfig",
            "A vector index's configuration.",
            "vector-indexes",
        ),
        (
            "VectorIndexCreated",
            "`{index: VectorIndexStatus, task: Task}`",
            "vector-indexes",
        ),
        (
            "RecallReport",
            "`{k, samples, ef, recall, hnswMs, exactMs}`",
            "vector-indexes",
        ),
        (
            "GeoStatus",
            "The spatial index's configuration and state, or `{enabled: false}`.",
            "geosparql",
        ),
        (
            "GeoConfig",
            "The spatial index's configuration.",
            "geosparql",
        ),
        (
            "GeoConvertRequest",
            "`{literals: [{value, datatype}]}`, at most 10,000 literals.",
            "hulls-aggregates-jena-filter-functions-utm-and-conversion",
        ),
        (
            "GeoConvertResult",
            "`{results: [...]}`, one per literal.",
            "hulls-aggregates-jena-filter-functions-utm-and-conversion",
        ),
        (
            "ValidationStatus",
            "`{language, config, status}`, or `{config: null}`.",
            "write-time-validation",
        ),
        (
            "ValidationConfig",
            "A write-time validation configuration, SHACL or ShEx.",
            "write-time-validation",
        ),
        (
            "RdfsStatus",
            "The dataset's RDFS-on-read setting, or `{enabled: false}`.",
            "rdfs-on-read",
        ),
        (
            "RdfsRequest",
            "`{graph: IRI | \"default\"}`; a schema document in an RDF syntax is sent with its own media type.",
            "rdfs-on-read",
        ),
        (
            "HistoryRequest",
            "`{keepCommits?, keepAge?, maxBytes?, schedules?, catalog?}`",
            "named-snapshots-and-retention",
        ),
        (
            "SnapshotRequest",
            "`{name, at?, note?, expires?, warm?}`",
            "named-snapshots-and-retention",
        ),
        (
            "StoredQuery",
            "A stored query definition: `query`, `description`, `parameters`, `results`, `mcp`, and on reads `name`, `dataset`, `kind` and `version`.",
            "stored-queries",
        ),
        (
            "StoredQueryList",
            "`{dataset, queries: [...]}`",
            "stored-queries",
        ),
        (
            "StoredQueryVersions",
            "The kept versions of a stored query, newest first.",
            "stored-queries",
        ),
        (
            "ShexRequest",
            "The JSON envelope of a ShEx validation: `{schema, schemaFormat?, map, externs?, imports?, base?}`.",
            "shex-validation",
        ),
        ("ShexReport", "A ShEx validation result.", "shex-validation"),
        (
            "Repository",
            "A backup repository with its totals.",
            "backup-types",
        ),
        (
            "RepositoryList",
            "`{repositories: (Repository | RepositoryBrief)[]}`",
            "backup-routes",
        ),
        (
            "RepositoryConfig",
            "A repository's settings: name, type, location and limits.",
            "backup-types",
        ),
        (
            "TestReport",
            "The result of a repository's connection test.",
            "backup-types",
        ),
        (
            "VerifyRequest",
            "`{level?: \"exists\" | \"data\" | \"restore\"}`",
            "backup-routes",
        ),
        ("GcRequest", "`{dryRun?, graceHours?}`", "backup-routes"),
        (
            "LockList",
            "`{locks: Lock[]}`",
            "locks-and-garbage-collection",
        ),
        (
            "BackupList",
            "`{backups: BackupSummary[], next}` or, per dataset, `{dataset, datasetId, backups}`.",
            "backup-routes",
        ),
        (
            "Backup",
            "A backup: the summary plus its manifest's files, blobs and upload statistics.",
            "backup-types",
        ),
        (
            "BackupRequest",
            "`{repository, name?, note?}`",
            "backup-routes",
        ),
        (
            "RestoreRequest",
            "Where and how to restore a backup.",
            "restore",
        ),
        (
            "Policy",
            "A backup policy with its state.",
            "lifecycle-policies",
        ),
        ("PolicyList", "`{policies: Policy[]}`", "lifecycle-policies"),
        (
            "PolicyConfig",
            "A backup policy's settings: datasets, repository, schedule and retention.",
            "lifecycle-policies",
        ),
        (
            "SchedulePreviewRequest",
            "`{schedule, timezone?, count?, nameTemplate?, dataset?}`",
            "backup-routes",
        ),
        (
            "SchedulePreview",
            "`{next: string[], description, …}`",
            "backup-routes",
        ),
        (
            "RetentionResult",
            "`{dryRun, delete, keep, errors?}`",
            "backup-routes",
        ),
        ("PolicyRuns", "`{runs: PolicyRun[]}`", "backup-routes"),
        (
            "BackupFiles",
            "Fuseki's list of N-Quads backup files, `{backups: string[]}`.",
            "datasets-admin",
        ),
    ] {
        put(name, open(desc, anchor));
    }
    s
}
