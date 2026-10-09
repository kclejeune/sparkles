//! The admin bodies described member by member: history and snapshots, stored queries,
//! the full-text, vector and spatial indexes, reasoning, write-time validation, and
//! backup repositories and backups. They follow the TypeScript-style definitions of
//! `docs/API.md`, and `openapi::contract_tests` checks them against the bodies a server
//! returns.

use super::kit::*;
use super::*;

/// `{enabled: false}`, the answer when a feature is off.
fn disabled() -> J {
    obj(&["enabled"], json!({ "enabled": { "const": false } }))
}
/// `{include?, exclude?}` of the indexes' graph scopes.
fn graph_scope() -> J {
    obj(
        &[],
        json!({
            "include": { "oneOf": [{ "const": "all" }, strings()] },
            "exclude": strings(),
        }),
    )
}
/// `{at, ms, …}` of an index's last build.
fn last_build(count: &str) -> J {
    obj(
        &["at", "ms"],
        json!({ "at": string(), "ms": num(), count: int() }),
    )
}

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    history(put);
    queries(put);
    indexes(put);
    reasoning(put);
    validation(put);
    backups(put);
}

// ---------------------------------------------------------------- history --

fn history(put: &mut dyn FnMut(&str, J)) {
    let retention = obj(
        &[],
        json!({
            "keepCommits": nullable("integer"),
            "keepAge": { "type": ["string", "integer", "null"], "description": "`7d`, or seconds." },
            "maxBytes": { "type": ["integer", "string", "null"], "description": "A number, or `10GiB` in a PUT." },
        }),
    );
    let change_log = obj(
        &[
            "enabled", "segments", "bytes", "pending", "maxBytes", "settings",
        ],
        json!({
            "enabled": boolean(),
            "first": nullable("integer"),
            "last": nullable("integer"),
            "segments": int(),
            "bytes": int(),
            "pending": int(),
            "maxBytes": int(),
            "settings": obj(&[], json!({
                "enabled": nullable("boolean"),
                "keepCommits": nullable("integer"),
                "keepAge": { "type": ["string", "integer", "null"] },
                "maxBytes": { "type": ["integer", "string", "null"] },
            })),
            "error": nullable("string"),
        }),
    );
    put(
        "HistoryStatus",
        doc(
            obj(
                &[
                    "dataset",
                    "datasetId",
                    "head",
                    "reconstructable",
                    "bytes",
                    "generations",
                    "retention",
                    "schedules",
                    "snapshots",
                    "catalog",
                    "cache",
                ],
                json!({
                    "dataset": string(),
                    "datasetId": string(),
                    "head": int(),
                    "oldestReconstructable": nullable("integer"),
                    "reconstructable": array(obj(&["from", "to"], json!({ "from": int(), "to": int() }))),
                    "bytes": { "type": "integer", "description": "Disk of the kept generations that are not current." },
                    "generations": array(obj(
                        &["name", "baseSeq", "endSeq", "bytes", "current", "heldBy"],
                        json!({
                            "name": string(),
                            "baseSeq": int(),
                            "endSeq": int(),
                            "bytes": int(),
                            "current": boolean(),
                            "heldBy": { "type": "array", "items": string(), "description": "`head`, `snapshot:NAME` or `retention`." },
                        }),
                    )),
                    "retention": retention,
                    "schedules": array(obj(
                        &["prefix", "every", "keepLast"],
                        json!({ "prefix": string(), "every": string(), "keepLast": int() }),
                    )),
                    "snapshots": int(),
                    "catalog": obj(&["firstRetained"], json!({
                        "keepCommits": nullable("integer"),
                        "keepAge": { "type": ["string", "integer", "null"] },
                        "firstRetained": int(),
                    })),
                    "changeLog": or_null(change_log),
                    "cache": obj(
                        &["entries", "bytes", "hits", "misses", "materializations"],
                        json!({
                            "entries": int(),
                            "bytes": int(),
                            "hits": int(),
                            "misses": int(),
                            "materializations": int(),
                        }),
                    ),
                }),
            ),
            "The retention window, the kept generations, the change log and the history cache.",
            "named-snapshots-and-retention",
        ),
    );
    put(
        "NamedSnapshot",
        doc(
            obj(
                &[
                    "name",
                    "ref",
                    "seq",
                    "commit",
                    "created",
                    "expires",
                    "note",
                    "reconstructable",
                    "warm",
                ],
                json!({
                    "name": string(),
                    "ref": { "type": "string", "description": "`snapshot:NAME`." },
                    "seq": int(),
                    "commit": or_null(sref("Commit")),
                    "created": string(),
                    "expires": nullable("string"),
                    "note": nullable("string"),
                    "generation": nullable("string"),
                    "reconstructable": boolean(),
                    "warm": { "type": "boolean", "description": "Kept materialized." },
                }),
            ),
            "A named snapshot: the commit it pins and its expiry.",
            "named-snapshots-and-retention",
        ),
    );
    put(
        "SnapshotList",
        doc(
            obj(
                &["dataset", "datasetId", "head", "snapshots"],
                json!({
                    "dataset": string(),
                    "datasetId": string(),
                    "head": int(),
                    "snapshots": array(sref("NamedSnapshot")),
                }),
            ),
            "A dataset's named snapshots.",
            "named-snapshots-and-retention",
        ),
    );
    put(
        "HistoryChanges",
        doc(
            obj(
                &[
                    "dataset",
                    "datasetId",
                    "head",
                    "from",
                    "to",
                    "truncated",
                    "changes",
                    "unrecorded",
                ],
                json!({
                    "dataset": string(),
                    "datasetId": string(),
                    "head": int(),
                    "from": int(),
                    "to": int(),
                    "truncated": boolean(),
                    "changes": array(obj(
                        &["op", "subject", "predicate", "object", "graph", "commit", "timestamp", "kind"],
                        json!({
                            "op": string_enum(&["add", "remove"]),
                            "subject": string(),
                            "predicate": string(),
                            "object": string(),
                            "graph": { "type": ["string", "null"], "description": "`null` for the default graph." },
                            "commit": int(),
                            "timestamp": string(),
                            "kind": string(),
                            "author": string(),
                            "message": string(),
                        }),
                    )),
                    "unrecorded": array(obj(
                        &["from", "to", "reason"],
                        json!({
                            "from": int(),
                            "to": int(),
                            "reason": string_enum(&["before-log", "bulk", "gap"]),
                        }),
                    )),
                }),
            ),
            "The recorded changes that match a history query, in N-Triples syntax.",
            "history-queries",
        ),
    );
}

// ---------------------------------------------------------- stored queries --

fn queries(put: &mut dyn FnMut(&str, J)) {
    let version = obj(
        &["version", "created", "digest"],
        json!({
            "version": int(),
            "created": string(),
            "digest": { "type": "string", "description": "SHA-256 of the definition." },
            "datasetCommit": int(),
            "author": string(),
        }),
    );
    let members = json!({
        "name": string(),
        "dataset": string(),
        "query": string(),
        "description": string(),
        "kind": string_enum(&["SELECT", "ASK", "CONSTRUCT", "DESCRIBE"]),
        "parameters": { "type": "object", "additionalProperties": true, "description": "The parameters by name, with their types and defaults." },
        "results": { "type": "object", "additionalProperties": true },
        "mcp": { "type": "boolean", "description": "Whether the MCP server offers it as a tool." },
        "questions": { "type": "array", "items": string(), "maxItems": 20, "description": "Example questions the query answers, at most 500 characters each. The MCP tool similar_queries ranks queries by them." },
        "version": version,
        "changed": { "type": "boolean", "description": "On a PUT: whether a new version was stored." },
    });
    put(
        "StoredQuery",
        doc(
            obj(&["query"], members.clone()),
            "A stored query definition. A PUT sends `query` and the optional members, and reads add `name`, `dataset`, `kind` and `version`.",
            "stored-queries",
        ),
    );
    let mut brief = members;
    brief.as_object_mut().expect("an object").remove("query");
    put(
        "StoredQueryList",
        doc(
            obj(
                &["dataset", "queries"],
                json!({
                    "dataset": string(),
                    "queries": array(obj(&["name", "kind", "version"], brief)),
                }),
            ),
            "A dataset's stored queries, without their text.",
            "stored-queries",
        ),
    );
}

// ----------------------------------------------------------------- indexes --

fn indexes(put: &mut dyn FnMut(&str, J)) {
    put(
        "TextConfig",
        doc(
            obj(
                &[],
                json!({
                    "predicates": { "oneOf": [{ "const": "all" }, strings()] },
                    "graphs": graph_scope(),
                    "maxTextBytes": int(),
                    "maxHits": int(),
                    "docstoreCompression": string_enum(&["zstd", "lz4", "none"]),
                    "languages": {
                        "oneOf": [
                            { "const": "all" },
                            strings(),
                            { "type": "object", "additionalProperties": string(), "description": "A language tag's analyzer, such as `english` or `cjk`." },
                        ],
                    },
                }),
            ),
            "The full-text index's configuration.",
            "full-text-search",
        ),
    );
    put(
        "TextStatus",
        doc(
            json!({
                "anyOf": [
                    disabled(),
                    obj(
                        &["enabled", "state", "docs", "seq", "storeSeq", "epoch", "diskBytes", "segments", "config", "formatVersion"],
                        json!({
                            "enabled": { "const": true },
                            "state": string_enum(&["ready", "stale", "rebuilding", "failed"]),
                            "docs": int(),
                            "seq": { "type": "integer", "description": "The commit the index reflects." },
                            "storeSeq": int(),
                            "epoch": int(),
                            "diskBytes": int(),
                            "segments": int(),
                            "config": sref("TextConfig"),
                            "formatVersion": int(),
                            "lastRebuild": last_build("docs"),
                            "message": string(),
                        }),
                    ),
                ],
            }),
            "The full-text index's configuration and state, or `{enabled: false}`.",
            "full-text-search",
        ),
    );

    let hnsw = obj(
        &[],
        json!({ "m": int(), "efConstruction": int(), "efSearch": int(), "nodes": int(), "layers": int() }),
    );
    let embedding_config = obj(
        &["url", "model"],
        json!({
            "url": string(),
            "model": string(),
            "apiKey": { "type": "object", "description": "`{secret: NAME}`, or `{env}` and `{file}` locally.", "additionalProperties": string() },
            "sendDimensions": boolean(),
            "predicates": strings(),
            "languages": strings(),
            "classes": strings(),
            "query": string(),
            "combine": boolean(),
            "inputPrefix": string(),
            "queryPrefix": string(),
            "queryText": boolean(),
            "batchSize": int(),
            "maxInputChars": int(),
            "chunking": obj(&["size"], json!({
                "size": int(),
                "overlap": int(),
                "unit": string_enum(&["chars", "tokens"]),
            })),
            "requestsPerMinute": int(),
            "tokensPerMinute": int(),
            "maxRetries": int(),
            "timeoutSecs": num(),
        }),
    );
    put(
        "VectorIndexConfig",
        doc(
            obj(
                &["predicate", "dimension"],
                json!({
                    "predicate": string(),
                    "dimension": int(),
                    "metric": string_enum(&["cosine", "dot", "euclidean"]),
                    "model": string(),
                    "hnsw": { "oneOf": [{ "const": false }, obj(&[], json!({ "m": int(), "efConstruction": int(), "efSearch": int() }))] },
                    "exactThreshold": int(),
                    "embedding": embedding_config.clone(),
                }),
            ),
            "A vector index's configuration.",
            "vector-indexes",
        ),
    );
    put(
        "VectorIndexStatus",
        doc(
            obj(
                &[
                    "name",
                    "predicate",
                    "dimension",
                    "metric",
                    "state",
                    "generation",
                    "rows",
                    "overlay",
                    "skipped",
                    "memory",
                    "hnsw",
                    "exactThreshold",
                ],
                json!({
                    "name": string(),
                    "predicate": string(),
                    "dimension": int(),
                    "metric": string_enum(&["cosine", "dot", "euclidean"]),
                    "model": string(),
                    "state": string_enum(&["ready", "building", "failed", "over-budget"]),
                    "progress": num(),
                    "message": string(),
                    "generation": string(),
                    "rows": int(),
                    "overlay": obj(&["inserts", "deletes"], json!({ "inserts": int(), "deletes": int() })),
                    "skipped": obj(
                        &["malformed", "wrongDimension", "zeroNorm"],
                        json!({ "malformed": int(), "wrongDimension": int(), "zeroNorm": int() }),
                    ),
                    "memory": obj(
                        &["segmentBytes", "hnswBytes", "residency"],
                        json!({ "segmentBytes": int(), "hnswBytes": int(), "residency": string_enum(&["heap", "mmap"]) }),
                    ),
                    "hnsw": or_null(hnsw),
                    "exactThreshold": int(),
                    "files": obj(&["bytes", "opened"], json!({ "bytes": int(), "opened": boolean() })),
                    "lastBuild": last_build("rows"),
                    "embedding": obj(
                        &["state", "model", "endpoint", "backlog", "appliedSeq", "headSeq"],
                        json!({
                            "state": string_enum(&["idle", "scanning", "embedding", "backoff", "paused", "disabled"]),
                            "model": string(),
                            "endpoint": string(),
                            "backlog": int(),
                            "scan": obj(&["done", "total"], json!({ "done": int(), "total": int() })),
                            "appliedSeq": int(),
                            "headSeq": int(),
                            "embedded": int(),
                            "requests": int(),
                            "failed": int(),
                            "lastError": obj(&["at", "message"], json!({ "at": string(), "message": string(), "subject": string() })),
                            "retryAt": string(),
                            "lastBatch": obj(&["at", "inputs", "ms"], json!({ "at": string(), "inputs": int(), "ms": num() })),
                            "config": embedding_config,
                        }),
                    ),
                }),
            ),
            "One vector index: its configuration and state.",
            "vector-indexes",
        ),
    );
    put(
        "VectorStatus",
        doc(
            obj(
                &[
                    "budgetBytes",
                    "usedBytes",
                    "generation",
                    "indexes",
                    "predicates",
                ],
                json!({
                    "budgetBytes": int(),
                    "usedBytes": int(),
                    "generation": string(),
                    "indexes": array(sref("VectorIndexStatus")),
                    "predicates": array(obj(
                        &["predicate", "bytes", "malformed", "dimensions"],
                        json!({
                            "predicate": string(),
                            "bytes": int(),
                            "malformed": int(),
                            "dimensions": array(obj(&["dimension", "vectors"], json!({ "dimension": int(), "vectors": int() }))),
                        }),
                    )),
                }),
            ),
            "The vector indexes and the predicates packed without one.",
            "vector-indexes",
        ),
    );

    put(
        "GeoConfig",
        doc(
            obj(
                &[],
                json!({
                    "predicates": strings(),
                    "featureLinks": strings(),
                    "graphs": graph_scope(),
                    "distance": string_enum(&["geodesic", "haversine"]),
                    "maxGeometryBytes": int(),
                    "maxVertices": int(),
                    "wgs84": boolean(),
                    "queryRewrite": boolean(),
                    "formatVersion": int(),
                }),
            ),
            "The spatial index's configuration.",
            "geosparql",
        ),
    );
    put(
        "GeoStatus",
        doc(
            json!({
                "anyOf": [
                    disabled(),
                    obj(
                        &["enabled", "state", "generation", "commit", "rows", "literals", "skipped", "crs", "memory", "config", "formatVersion"],
                        json!({
                            "enabled": { "const": true },
                            "state": string_enum(&["ready", "building", "failed", "over-budget"]),
                            "progress": num(),
                            "message": string(),
                            "generation": string(),
                            "commit": int(),
                            "rows": obj(&["base", "overlay", "tail"], json!({ "base": int(), "overlay": int(), "tail": int(), "wgs84": int() })),
                            "literals": int(),
                            "skipped": obj(
                                &["malformed", "unknownCrs", "tooLarge", "empty"],
                                json!({ "malformed": int(), "unknownCrs": int(), "tooLarge": int(), "empty": int() }),
                            ),
                            "crs": { "type": "object", "additionalProperties": int(), "description": "Literals per CRS IRI." },
                            "memory": obj(
                                &["treeBytes", "geometryBytes", "overlayBytes", "budgetBytes"],
                                json!({ "treeBytes": int(), "geometryBytes": int(), "overlayBytes": int(), "budgetBytes": int(), "mappedBytes": int() }),
                            ),
                            "config": sref("GeoConfig"),
                            "formatVersion": int(),
                            "lastBuild": last_build("rows"),
                            "files": obj(&["bytes", "opened"], json!({ "bytes": int(), "opened": boolean() })),
                        }),
                    ),
                ],
            }),
            "The spatial index's configuration and state, or `{enabled: false}`.",
            "geosparql",
        ),
    );
}

// --------------------------------------------------------------- reasoning --

fn reasoning(put: &mut dyn FnMut(&str, J)) {
    let run = obj(
        &["method", "inferredAdded", "inferredRemoved"],
        json!({
            "method": string_enum(&["full", "incremental"]),
            "fallback": string(),
            "inferredAdded": int(),
            "inferredRemoved": int(),
            "changes": obj(&[], json!({
                "explicitAdded": int(),
                "explicitRemoved": int(),
                "checked": int(),
                "removed": int(),
                "derived": int(),
                "source": string_enum(&["memory", "store"]),
            })),
        }),
    );
    put(
        "ReasoningStatus",
        doc(
            obj(
                &[
                    "profile",
                    "inferred",
                    "at",
                    "commit",
                    "head",
                    "stale",
                    "commitsSince",
                    "auto",
                    "warnings",
                ],
                json!({
                    "profile": { "type": "string", "examples": ["rdfs", "rdfs-simple", "owl-rl", "rules"] },
                    "inferred": int(),
                    "at": { "type": "string", "description": "When the run finished." },
                    "commit": { "type": ["integer", "null"], "description": "The commit the inferences were materialized at." },
                    "head": int(),
                    "stale": nullable("boolean"),
                    "commitsSince": nullable("integer"),
                    "staleReason": string(),
                    "auto": obj(&["enabled", "source"], json!({
                        "enabled": boolean(),
                        "source": string_enum(&["server", "dataset"]),
                        "debounceSeconds": num(),
                        "maxDelaySeconds": num(),
                        "scheduledAt": string(),
                    })),
                    "warnings": strings(),
                    "vocabularies": strings(),
                    "geoDefaultGeometry": { "const": true },
                    "run": run,
                    "inputs": obj(&["dataGraphs", "imports"], json!({
                        "dataGraphs": strings(),
                        "ontologyGraphs": strings(),
                        "imports": string_enum(&["none", "dataset", "fetch"]),
                        "locationMapping": array(json!({ "type": "object", "additionalProperties": string() })),
                    })),
                    "inputGraphs": strings(),
                    "watchedGraphs": strings(),
                    "imports": array(obj(&["iri"], json!({ "iri": string(), "location": string(), "graph": string() }))),
                    "fetchedImports": strings(),
                }),
            ),
            "The materialized inferences of a dataset and whether they are current.",
            "reasoning-status-and-diagnostics",
        ),
    );
}

// -------------------------------------------------------------- validation --

fn validation(put: &mut dyn FnMut(&str, J)) {
    put(
        "ValidationConfig",
        doc(
            obj(
                &["mode"],
                json!({
                    "format": { "type": "integer", "description": "2 when stored." },
                    "language": string_enum(&["shacl", "shex"]),
                    "mode": string_enum(&["reject", "warn", "off"]),
                    "shapes": { "type": "object", "additionalProperties": true, "description": "SHACL: `{graphs?, inline?, format?}`, stored with `file` and `sha256`." },
                    "schema": { "type": "object", "additionalProperties": true, "description": "ShEx: `{inline, format?, base?, source?}` or `{graphs, prefixes?, base?}`, stored with `file`, `format` and `sha256`." },
                    "shapeMap": { "oneOf": [string(), array(obj(&["node", "shape"], json!({ "node": string(), "shape": string() })))] },
                    "dataGraph": { "oneOf": [string_enum(&["default", "union"]), strings()] },
                    "includeInferences": boolean(),
                    "threshold": string_enum(&["violation", "warning", "info"]),
                    "baseline": string_enum(&["strict", "grandfather"]),
                    "timeoutSeconds": num(),
                    "reportLimit": int(),
                    "updated": string(),
                }),
            ),
            "A write-time validation configuration, SHACL or ShEx.",
            "write-time-validation",
        ),
    );
    let counts = obj(
        &[],
        json!({ "violation": int(), "warning": int(), "info": int() }),
    );
    put(
        "ValidationStatus",
        doc(
            json!({
                "anyOf": [
                    obj(&["config"], json!({ "config": { "type": "null" } })),
                    obj(
                        &["language", "config", "status"],
                        json!({
                            "language": string_enum(&["shacl", "shex"]),
                            "config": sref("ValidationConfig"),
                            "status": obj(
                                &["mode", "counters", "warnings", "recentRejections"],
                                json!({
                                    "mode": string_enum(&["reject", "warn", "off"]),
                                    "shapeCount": int(),
                                    "baseline": or_null(obj(&[], json!({
                                        "commit": int(),
                                        "conforms": boolean(),
                                        "blocking": int(),
                                        "total": int(),
                                        "bySeverity": counts,
                                        "millis": num(),
                                    }))),
                                    "counters": obj(&[], json!({
                                        "passed": int(), "warned": int(), "rejected": int(), "skipped": int(), "bypassed": int(),
                                    })),
                                    "lastCheck": { "type": ["object", "null"], "additionalProperties": true },
                                    "recentRejections": array(json!({ "type": "object", "additionalProperties": true })),
                                    "lastFullMillis": num(),
                                    "incremental": obj(&[], json!({ "localShapes": int(), "fullShapes": strings() })),
                                    "associations": int(),
                                    "warnings": strings(),
                                }),
                            ),
                        }),
                    ),
                ],
            }),
            "`{language, config, status}`, or `{config: null}` without validation.",
            "write-time-validation",
        ),
    );
}

// ----------------------------------------------------------------- backups --

fn backups(put: &mut dyn FnMut(&str, J)) {
    let config_members = json!({
        "name": string(),
        "type": string_enum(&["fs", "s3", "gcs", "azure"]),
        "path": string(),
        "bucket": string(),
        "prefix": string(),
        "region": string(),
        "endpoint": string(),
        "pathStyle": boolean(),
        "allowHttp": boolean(),
        "credentials": {
            "type": "object",
            "required": ["source"],
            "properties": {
                "source": string_enum(&["default", "env", "file", "named"]),
                "accessKeyIdVar": string(),
                "secretAccessKeyVar": string(),
                "sessionTokenVar": string(),
                "path": string(),
                "name": string(),
            },
            "description": "Where the credentials come from. Sparkles stores no secrets.",
        },
        "sse": string_enum(&["AES256", "aws:kms"]),
        "kmsKeyId": string(),
        "conditionalWrites": boolean(),
        "readonly": boolean(),
        "maxConcurrency": int(),
        "maxUploadBytesPerSec": int(),
        "maxDownloadBytesPerSec": int(),
    });
    put(
        "RepositoryConfig",
        doc(
            obj(&["name", "type"], config_members.clone()),
            "A repository's settings: name, type, location and limits.",
            "backup-types",
        ),
    );
    let test_report = obj(
        &["ok", "conditionalWrites", "steps"],
        json!({
            "ok": boolean(),
            "conditionalWrites": boolean(),
            "steps": array(obj(&["step", "ok", "millis"], json!({
                "step": string_enum(&["create", "create-again", "read", "list", "delete"]),
                "ok": boolean(),
                "millis": int(),
                "error": string(),
            }))),
        }),
    );
    put(
        "TestReport",
        doc(
            test_report,
            "The result of a repository's connection test.",
            "backup-types",
        ),
    );
    let mut repo = config_members;
    let r = repo.as_object_mut().expect("an object");
    r.insert(
        "source".into(),
        with_desc(
            string_enum(&["api", "config"]),
            "`config`: from `--backup-config`, read-only through the API.",
        ),
    );
    r.insert("id".into(), nullable("string"));
    r.insert(
        "status".into(),
        obj(
            &["reachable", "checked", "singleWriter"],
            json!({
                "reachable": boolean(),
                "checked": string(),
                "error": string(),
                "conditionalWrites": boolean(),
                "singleWriter": boolean(),
            }),
        ),
    );
    r.insert(
        "stats".into(),
        or_null(obj(
            &[
                "backups",
                "datasets",
                "storedBytes",
                "logicalBytes",
                "dedupRatio",
                "asOf",
            ],
            json!({
                "backups": int(),
                "datasets": int(),
                "storedBytes": int(),
                "logicalBytes": int(),
                "dedupRatio": num(),
                "asOf": string(),
            }),
        )),
    );
    r.insert(
        "lastGc".into(),
        json!({ "type": ["object", "null"], "additionalProperties": true, "description": "The last garbage collection's report, with `finished`." }),
    );
    r.insert("policies".into(), strings());
    r.insert("test".into(), sref("TestReport"));
    put(
        "Repository",
        doc(
            obj(
                &[
                    "name", "type", "source", "id", "status", "stats", "lastGc", "policies",
                ],
                repo,
            ),
            "A backup repository with its settings, status and totals.",
            "backup-types",
        ),
    );
    put(
        "RepositoryList",
        doc(
            obj(
                &["repositories"],
                json!({
                    "repositories": array(json!({
                        "anyOf": [
                            sref("Repository"),
                            obj(&["name", "type", "readonly", "reachable"], json!({
                                "name": string(), "type": string(), "readonly": boolean(), "reachable": boolean(),
                            })),
                        ],
                    })),
                }),
            ),
            "The repositories, in full or in brief.",
            "backup-routes",
        ),
    );
    put(
        "BackupBranch",
        doc(
            obj(
                &["datasetId", "id", "name", "nextOrdinal"],
                json!({
                    "datasetId": { "type": "string", "format": "uuid", "description": "The enclosing dataset's identity." },
                    "id": { "type": "string", "format": "uuid", "description": "The captured branch identity, also dataset.id." },
                    "name": string(),
                    "nextOrdinal": { "type": "integer", "minimum": 2, "maximum": 65536 },
                }),
            ),
            "The provenance of a branch captured as a standalone dataset. Restores use a fresh identity and reserve its blank-node allocation range.",
            "backup-types",
        ),
    );
    let summary_members = json!({
        "name": string(),
        "repository": string(),
        "dataset": obj(&["name", "id", "type"], json!({
            "name": string(), "id": string(), "type": string_enum(&["persistent", "mem"]),
            "branch": sref("BackupBranch"),
        })),
        "commit": obj(&["seq", "timestamp", "quads", "ref"], json!({
            "seq": int(), "timestamp": string(), "quads": int(), "ref": string(),
        })),
        "created": string(),
        "completed": string(),
        "millis": int(),
        "logicalBytes": int(),
        "addedBytes": int(),
        "policy": nullable("string"),
        "run": nullable("string"),
        "note": nullable("string"),
        "sameLineage": boolean(),
        "verified": or_null(obj(&["level", "status", "at"], json!({
            "level": string_enum(&["exists", "data", "restore"]),
            "status": string_enum(&["ok", "error"]),
            "at": string(),
        }))),
    });
    let summary_required = [
        "name",
        "repository",
        "dataset",
        "commit",
        "created",
        "completed",
        "millis",
        "logicalBytes",
        "addedBytes",
        "policy",
        "run",
        "note",
        "verified",
    ];
    put(
        "BackupSummary",
        doc(
            obj(&summary_required, summary_members.clone()),
            "A backup as listings give it.",
            "backup-types",
        ),
    );
    let mut full = summary_members;
    let f = full.as_object_mut().expect("an object");
    f.get_mut("dataset")
        .and_then(|dataset| dataset.get_mut("properties"))
        .and_then(serde_json::Value::as_object_mut)
        .expect("dataset properties")
        .insert(
            "nextOrdinal".into(),
            json!({ "type": "integer", "minimum": 1, "maximum": 65536, "description": "The first unused branch blank-node ordinal. Restores reserve earlier allocation ranges, including those of merged or deleted branches. Older backups may omit this field." }),
        );
    f.insert("format".into(), int());
    f.insert("generation".into(), string());
    f.insert("indexFormat".into(), int());
    f.insert("parent".into(), nullable("string"));
    f.insert(
        "server".into(),
        obj(&["version"], json!({ "version": string() })),
    );
    f.insert(
        "files".into(),
        array(obj(
            &["path", "kind", "size", "sha256", "blobs"],
            json!({
                "path": string(),
                "kind": string_enum(&["immutable", "append", "meta"]),
                "size": int(),
                "sha256": string(),
                "blobs": array(obj(&["id", "size"], json!({ "id": string(), "size": int() }))),
            }),
        )),
    );
    f.insert(
        "stats".into(),
        obj(
            &[],
            json!({
                "logicalBytes": int(), "addedBytes": int(), "files": int(), "blobs": int(),
                "newBlobs": int(), "reusedBlobs": int(),
            }),
        ),
    );
    f.insert(
        "derived".into(),
        obj(
            &[],
            json!({ "text": or_null(obj(&["rebuildOnRestore"], json!({ "rebuildOnRestore": boolean() }))) }),
        ),
    );
    f.insert(
        "branchesOmitted".into(),
        json!({ "type": "integer", "description": "The other branches omitted from this standalone backup (absent when there were none)." }),
    );
    let mut required = summary_required.to_vec();
    required.extend(["format", "generation", "files"]);
    put(
        "Backup",
        doc(
            obj(&required, full),
            "A backup: the summary plus its manifest's files, blobs and upload statistics.",
            "backup-types",
        ),
    );
    put(
        "BackupList",
        doc(
            json!({
                "anyOf": [
                    obj(&["backups"], json!({
                        "backups": array(sref("BackupSummary")),
                        "next": nullable("string"),
                    })),
                    obj(&["dataset", "datasetId", "backups"], json!({
                        "dataset": string(),
                        "datasetId": string(),
                        "backups": array(sref("BackupSummary")),
                    })),
                ],
            }),
            "A page of backups, or a dataset's backups in every repository.",
            "backup-routes",
        ),
    );
}
