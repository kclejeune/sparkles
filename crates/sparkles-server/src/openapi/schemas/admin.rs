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
    views(put);
    algorithms(put);
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
    let mapping_version = obj(
        &["version", "created", "digest"],
        json!({
            "version": int(),
            "parent": int(),
            "created": string(),
            "author": string(),
            "message": string(),
            "datasetCommit": { "type": "integer", "description": "The dataset's head when the version was saved." },
            "digest": { "type": "string", "description": "Hex SHA-256 over the parent's digest and the definition." },
        }),
    );
    let mapping = json!({
        "name": string(),
        "dataset": string(),
        "kind": string_enum(&["rml", "yarrrml", "csvw", "template"]),
        "description": string(),
        "text": { "type": "string", "description": "The mapping text. Listings leave it out." },
        "version": mapping_version.clone(),
        "summary": obj(&["kind", "triplesMaps", "files", "connections", "warnings"], json!({
            "kind": string(),
            "vocabulary": { "type": "string", "description": "R2RML, RML (rml.io), RML-Core or YARRRML." },
            "triplesMaps": int(),
            "files": strings(),
            "connections": { "type": "array", "items": string(), "description": "The connections it reads. An empty name is an R2RML mapping's default connection." },
            "graphs": { "type": ["array", "null"], "items": string(), "description": "The graphs its graph maps name, `default` for the default graph, or null when one is not constant." },
            "warnings": strings(),
        })),
        "lastRun": obj(&["time", "version", "mode", "quads", "rows"], json!({
            "time": string(),
            "version": int(),
            "mode": string_enum(&["add", "replace"]),
            "commit": int(),
            "quads": int(),
            "rows": int(),
            "warnings": strings(),
            "author": string(),
        })),
        "changed": { "type": "boolean", "description": "On a PUT: whether a new version was stored." },
    });
    put(
        "StoredMapping",
        doc(
            obj(&["name", "kind", "version"], mapping.clone()),
            "A stored mapping, with the summary of its check and its last run.",
            "stored-mappings",
        ),
    );
    put(
        "StoredMappingList",
        doc(
            obj(
                &["dataset", "mappings"],
                json!({ "dataset": string(), "mappings": array(obj(&["name", "kind", "version"], mapping)), "warning": string() }),
            ),
            "A dataset's stored mappings, without their text.",
            "stored-mappings",
        ),
    );
    put(
        "StoredMappingVersions",
        doc(
            obj(
                &["dataset", "name", "versions"],
                json!({ "dataset": string(), "name": string(), "versions": array(mapping_version) }),
            ),
            "The kept versions of a stored mapping, newest first.",
            "stored-mappings",
        ),
    );
    put(
        "MappingRun",
        doc(
            obj(
                &["mapping", "version", "mode", "rows", "quads", "warnings"],
                json!({
                    "mapping": string(),
                    "version": int(),
                    "mode": string_enum(&["add", "replace"]),
                    "rows": int(),
                    "quads": { "type": "integer", "description": "The quads the mapping made, before the load removed duplicates." },
                    "count": { "type": "integer", "description": "The quads the commit added." },
                    "warnings": strings(),
                    "reports": { "type": "array", "items": { "type": "object", "additionalProperties": true } },
                }),
            ),
            "What a stored mapping's run made and committed.",
            "stored-mappings",
        ),
    );
}

// ------------------------------------------------------------------ views --

fn views(put: &mut dyn FnMut(&str, J)) {
    let definition = json!({
        "query": with_desc(string(), "A SELECT or CONSTRUCT query; the dataset's prefixes apply."),
        "storedQuery": with_desc(string(), "A stored query by name, in place of `query`."),
        "mode": string_enum(&["table", "graph"]),
        "graph": string(),
        "principal": with_desc(string(), "The user whose view of the dataset the query reads; the definer by default."),
        "provenance": string_enum(&["none", "count", "why", "lineage"]),
        "maintenance": string_enum(&["async", "commit"]),
        "branches": array(string()),
        "memoryBytes": { "type": "integer", "minimum": 1, "description": "The budget of the view's state." },
        "keepCommits": { "type": "integer", "minimum": 0 },
    });
    put(
        "ViewDefinition",
        doc(
            obj(&[], definition.clone()),
            "A view's definition. Phase 1 takes `table` views maintained `async` on `main`, without provenance and with `keepCommits` 0.",
            "incremental-views",
        ),
    );
    let plan = json!({
        "type": "object",
        "additionalProperties": true,
        "description": "The view's plan: each node has its `operator`, its `mode` (`incremental` or `recompute`), its `vars`, a `detail` that gives a recompute node's reason, and its `children`.",
    });
    let members = json!({
        "name": string(),
        "dataset": string(),
        "definition": sref("ViewDefinition"),
        "state": string_enum(&["building", "current", "behind", "suspended", "failed"]),
        "commits": { "type": "integer", "description": "With `behind`: the commits it lags." },
        "reason": with_desc(string(), "With `suspended`: why."),
        "error": with_desc(string(), "With `failed`: why."),
        "commit": { "type": "integer", "description": "The commit its rows reflect." },
        "columns": array(string()),
        "rows": { "type": "integer", "description": "Its rows, multiplicities counted." },
        "stateBytes": { "type": "integer" },
        "passes": { "type": "integer" },
        "skipped": { "type": "integer", "description": "Passes its footprint skipped." },
        "passMs": { "type": "number" },
        "recomputeMs": { "type": "number", "description": "Time spent in its recompute nodes." },
        "evaluation": with_desc(string_enum(&["incremental", "recompute"]), "`recompute` when every relevant commit evaluates the whole query again, as on a dataset with RDFS on read or materialized inferences. Absent while the view has no circuit."),
        "evaluationReason": with_desc(string(), "With `recompute`: why the whole query is evaluated again."),
        "plan": plan,
        "task": with_desc(string(), "On a PUT: the task of the first evaluation."),
    });
    put(
        "View",
        doc(
            obj(&["name", "definition", "state"], members.clone()),
            "A view with its state.",
            "incremental-views",
        ),
    );
    let mut brief = members;
    brief.as_object_mut().expect("an object").remove("plan");
    put(
        "ViewList",
        doc(
            obj(
                &["dataset", "views"],
                json!({ "dataset": string(), "views": array(obj(&["name", "state"], brief)) }),
            ),
            "A dataset's views without their plans.",
            "incremental-views",
        ),
    );
    put(
        "ViewTask",
        doc(
            obj(
                &["dataset", "name", "task"],
                json!({ "dataset": string(), "name": string(), "task": string() }),
            ),
            "A view's rebuild and the task that runs it.",
            "incremental-views",
        ),
    );
}

// -------------------------------------------------------- graph algorithms --

fn algorithms(put: &mut dyn FnMut(&str, J)) {
    let predicate = json!({
        "oneOf": [
            string(),
            obj(&["iri"], json!({ "iri": string(), "direction": string_enum(&["forward", "backward", "undirected"]) })),
        ],
        "description": "An IRI (a prefixed name in a run request), or an object with the direction this predicate is read in.",
    });
    let definition = json!({
        "predicates": array(predicate),
        "direction": string_enum(&["forward", "backward", "undirected"]),
        "nodeClasses": strings(),
        "weight": { "type": "string", "description": "The property of an edge's reifier that holds its weight." },
        "defaultWeight": num(),
        "aggregate": string_enum(&["none", "sum", "max", "count"]),
    });
    put(
        "AlgoProjectionDefinition",
        doc(
            closed(&[], definition.clone()),
            "The edges of a projection. Without `predicates` every predicate between resources counts, except `rdf:type`. An empty `predicates` list is refused.",
            "graph-algorithms",
        ),
    );
    put(
        "AlgoProjection",
        doc(
            obj(
                &["name", "revision", "updated", "definition"],
                json!({
                    "name": string(),
                    "revision": { "type": "integer", "description": "1 for a new projection, one more for each replacement. A projection created again after a delete continues from the revision it had." },
                    "updated": string(),
                    "author": string(),
                    "definition": sref("AlgoProjectionDefinition"),
                }),
            ),
            "A named projection of a dataset. A PUT sends the definition, or this object with its `definition` member.",
            "graph-algorithms",
        ),
    );
    put(
        "AlgoProjectionList",
        doc(
            obj(
                &["dataset", "projections"],
                json!({
                    "dataset": string(),
                    "projections": array(sref("AlgoProjection")),
                    "cache": obj(&["bytes", "entries"], json!({ "bytes": int(), "entries": int() })),
                    "warning": { "type": "string", "description": "Why `algo.json` could not be read. The list is then empty and refuses changes." },
                }),
            ),
            "A dataset's named projections. A server administrator also sees `cache`, the size of the cache of built projections, which every dataset shares.",
            "graph-algorithms",
        ),
    );
    put(
        "AlgoRequest",
        doc(
            closed(
                &["algorithm"],
                json!({
                    "algorithm": string_enum(&["pageRank", "betweenness", "wcc", "scc", "labelPropagation", "louvain", "leiden", "nodeSimilarity", "triangleCount", "clustering", "kCore", "bfs", "sssp", "fastRP", "node2vec"]),
                    "projection": {
                        "oneOf": [string(), sref("AlgoProjectionDefinition")],
                        "description": "The name of a stored projection, or a definition.",
                    },
                    "params": any_object("The parameters of the algorithm, named as in `algo:` without the prefix, such as `maxIterations`, `seed` or `topK`."),
                    "nodes": { "type": "array", "items": string(), "maxItems": 20000, "description": "View-only runs. The IRIs of the nodes whose rows the answer holds. The whole result is computed, and each community or component row also carries `communitySize`, its size in the whole projection." },
                    "keep": { "type": "boolean", "description": "View-only runs. Keep the whole result in the server's cache of view-only results, keyed by branch, commit, the caller's view and the request without `nodes`, `keep`, `async` and `top`." },
                    "async": { "type": "boolean", "description": "View-only runs with `keep`. Run as an `algo-view` task and answer `202`, or join the task of a run with the same key." },
                    "top": { "type": "integer", "minimum": 1, "maximum": 20000, "description": "View-only runs. Only the rows with the highest values of the first numeric output, such as `score`." },
                    "write": closed(
                        &["graph", "predicates"],
                        json!({
                            "graph": string(),
                            "mode": string_enum(&["replace", "merge"]),
                            "predicates": { "type": "object", "additionalProperties": string(), "description": "The predicate that each output is written with, such as `{\"score\": \"ex:rank\"}`." },
                            "batchSize": int(),
                            "atomic": boolean(),
                        }),
                    ),
                }),
            ),
            "A run of one algorithm. With `write` the results are written to a graph in batched commits. Without it, `nodes`, `keep`, `async` and `top` make it a view-only run.",
            "graph-algorithms",
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
                            { "type": "object", "additionalProperties": string(), "description": "A language tag's analyzer, such as `english`, `english-lucene`, `cjk` or `ja-dict`." },
                        ],
                    },
                    "dictionaries": {
                        "type": "object",
                        "additionalProperties": string(),
                        "readOnly": true,
                        "description": "The SHA-256 of the installed dictionary of each dictionary analyzer the index uses. The server fills it in when the index is configured.",
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
        &["model"],
        json!({
            "url": with_desc(string(), "The endpoint. Required unless `provider` is set."),
            "provider": with_desc(string(), "A provider of the model configuration, instead of `url` and `apiKey`."),
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
                    "overlay": obj(
                        &["inserts", "deletes", "indexed", "pending", "overBudget"],
                        json!({
                            "inserts": int(),
                            "deletes": int(),
                            "indexed": int(),
                            "pending": int(),
                            "overBudget": boolean(),
                        }),
                    ),
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
                    "lastBuild": obj(
                        &["at", "ms"],
                        json!({ "at": string(), "ms": num(), "rows": int(), "carried": boolean() }),
                    ),
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
                            "local": any_object("For a provider of the local kind, the model's entry of `local` in `GET /$/models`."),
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
            "peakBytes": int(),
            "baseScans": int(),
            "threads": int(),
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
                    "format": { "type": "integer", "description": "2 for stored SHACL configurations and 3 for stored ShEx configurations. ShEx format 2 is also read." },
                    "language": string_enum(&["shacl", "shex"]),
                    "mode": string_enum(&["reject", "warn", "off"]),
                    "shapes": { "type": "object", "additionalProperties": true, "description": "SHACL: `{graphs?, inline?, format?}`, stored with `file` and `sha256`." },
                    "schema": { "type": "object", "additionalProperties": true, "description": "ShEx: `{inline, format?, base?, source?}` or `{graphs, prefixes?, base?}`. Copied schemas store `file`, `format`, original-source `sha256` and verified-copy `copySha256`." },
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
                    obj(
                        &["config"],
                        json!({ "config": { "type": "null" }, "unionDefaultGraph": boolean() }),
                    ),
                    obj(
                        &["language", "config", "status"],
                        json!({
                            "language": string_enum(&["shacl", "shex"]),
                            "config": sref("ValidationConfig"),
                            "unionDefaultGraph": boolean(),
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
