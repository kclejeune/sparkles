//! Server-wide bodies described member by member: the JSON metrics snapshot, Fuseki's
//! statistics, backup file list and validators, and automatic compaction.

use super::kit::*;
use super::*;

/// `{name: integer}` for counters keyed by a label value.
fn counters(description: &str) -> J {
    json!({ "type": "object", "additionalProperties": int(), "description": description })
}

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    metrics(put);
    fuseki(put);
    compaction(put);
}

fn metrics(put: &mut dyn FnMut(&str, J)) {
    let cache_members = json!({
        "bytes": int(),
        "capacityBytes": int(),
        "entries": int(),
        "hits": int(),
        "misses": int(),
    });
    let block_cache = obj(
        &["bytes", "capacityBytes", "entries", "hits", "misses"],
        cache_members.clone(),
    );
    let mut result_members = cache_members;
    result_members["enabled"] = boolean();
    let result_cache = obj(
        &[
            "enabled",
            "bytes",
            "capacityBytes",
            "entries",
            "hits",
            "misses",
        ],
        result_members,
    );
    let geo = obj(
        &[
            "enabled",
            "rows",
            "buildSeconds",
            "candidates",
            "refined",
            "matches",
            "rechecked",
        ],
        json!({
            "enabled": boolean(),
            "rows": obj(&["base", "overlay", "tail"], json!({ "base": int(), "overlay": int(), "tail": int() })),
            "buildSeconds": nullable("number"),
            "candidates": int(),
            "refined": int(),
            "matches": int(),
            "rechecked": int(),
        }),
    );
    put(
        "MetricsSnapshot",
        doc(
            obj(
                &[
                    "formatVersion",
                    "version",
                    "uptimeSeconds",
                    "ready",
                    "processResidentBytes",
                    "limits",
                    "bucketBounds",
                    "active",
                    "requests",
                    "datasets",
                ],
                json!({
                    "formatVersion": { "const": 1 },
                    "version": string(),
                    "uptimeSeconds": num(),
                    "ready": boolean(),
                    "processResidentBytes": nullable("integer"),
                    "limits": sref("Limits"),
                    "bucketBounds": { "type": "array", "items": num(), "description": "Histogram upper bounds in seconds, without +Inf." },
                    "active": counters("Requests in progress, by operation."),
                    "requests": array(obj(
                        &["dataset", "operation", "outcomes", "count", "sumSeconds", "buckets", "responseBytes"],
                        json!({
                            "dataset": string(),
                            "operation": string(),
                            "outcomes": counters("Completed requests, by outcome."),
                            "count": int(),
                            "sumSeconds": num(),
                            "buckets": { "type": "array", "items": int(), "description": "Cumulative counts. The last one, for +Inf, equals `count`." },
                            "responseBytes": int(),
                        }),
                    )),
                    "datasets": array(obj(
                        &[
                            "name", "quads", "deltaInserts", "deltaDeletes", "walBytes", "diskBytes",
                            "quotaBytes", "resultRows", "budgetExceeded", "rateLimited", "blockCache",
                            "resultCache", "geo",
                        ],
                        json!({
                            "name": string(),
                            "quads": int(),
                            "deltaInserts": int(),
                            "deltaDeletes": int(),
                            "walBytes": int(),
                            "diskBytes": int(),
                            "quotaBytes": { "type": "integer", "description": "0 means unlimited." },
                            "resultRows": int(),
                            "budgetExceeded": or_null(counters("Requests over a budget, by budget.")),
                            "rateLimited": or_null(counters("Rate-limited requests, by class.")),
                            "blockCache": block_cache,
                            "resultCache": result_cache,
                            "geo": or_null(geo),
                        }),
                    )),
                }),
            ),
            "The counters of `/$/metrics` as JSON (`?format=json`).",
            "metrics",
        ),
    );
}

fn fuseki(put: &mut dyn FnMut(&str, J)) {
    let endpoint = obj(
        &[
            "Requests",
            "RequestsGood",
            "RequestsBad",
            "operation",
            "description",
        ],
        json!({
            "Requests": int(),
            "RequestsGood": int(),
            "RequestsBad": int(),
            "operation": string(),
            "description": string(),
        }),
    );
    let dataset = obj(
        &["Requests", "RequestsGood", "RequestsBad", "endpoints"],
        json!({
            "Requests": int(),
            "RequestsGood": int(),
            "RequestsBad": int(),
            "endpoints": {
                "type": "object",
                "additionalProperties": endpoint,
                "description": "The endpoints that had a request, by name. The dataset URL is `_1`, `_2`, …, and a name with two operations appears once per operation as `{name}_{operation}`.",
            },
        }),
    );
    put(
        "FusekiStats",
        doc(
            obj(
                &["datasets"],
                json!({
                    "datasets": {
                        "type": "object",
                        "additionalProperties": dataset,
                        "description": "The counters of each dataset the caller may read, keyed by its path, such as `/ds`.",
                    },
                }),
            ),
            "Fuseki's request counters.",
            "datasets-admin",
        ),
    );
    put(
        "BackupFiles",
        doc(
            obj(&["backups"], json!({ "backups": strings() })),
            "Fuseki's list of N-Quads backup files in `<data>/backups`, sorted.",
            "datasets-admin",
        ),
    );
    let parse_error = obj(
        &["parse-error"],
        json!({
            "parse-error": string(),
            "parse-error-line": int(),
            "parse-error-column": int(),
        }),
    );
    put(
        "ValidatorResult",
        doc(
            closed(
                &[],
                json!({
                    "input": { "type": "string", "description": "The query, update or data." },
                    "formatted": string(),
                    "algebra": { "type": "string", "description": "Queries: the algebra in SSE." },
                    "errors": array(parse_error),
                    "iris": array(obj(&["iri", "errors", "warning"], json!({
                        "iri": string(),
                        "errors": strings(),
                        "warning": strings(),
                    }))),
                    "langtags": array(obj(&["input", "errors"], json!({
                        "input": string(),
                        "errors": strings(),
                        "formatted": string(),
                        "language": string(),
                        "script": string(),
                        "region": string(),
                        "variant": string(),
                        "extension": string(),
                        "privateuse": string(),
                    }))),
                }),
            ),
            "The JSON answer of a Fuseki validator: `{input, formatted, algebra}` or `{input, errors}` for queries, updates and data, `{iris}` for IRIs and `{langtags}` for language tags.",
            "validators",
        ),
    );
}

fn compaction(put: &mut dyn FnMut(&str, J)) {
    let settings = json!({
        "enabled": boolean(),
        "minDeltaQuads": int(),
        "deltaRatio": num(),
        "maxDeltaQuads": int(),
        "maxDeltaMb": int(),
        "maxWalMb": int(),
        "idleSeconds": int(),
        "maxAgeSeconds": int(),
        "minIntervalSeconds": int(),
        "partial": string_enum(&["auto", "off", "always"]),
    });
    let all: Vec<&str> = settings
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    let mut request = settings.clone();
    request["format"] =
        json!({ "type": "integer", "description": "Ignored. Stored settings carry it." });
    put(
        "CompactionPolicy",
        doc(
            closed(&[], request),
            "The settings a dataset overrides. `0` turns off the size, idle and age triggers it is the limit of. An unknown setting is a `400`.",
            "automatic-compaction",
        ),
    );
    let policy = obj(&all, settings.clone());
    let own = closed(&[], settings);
    put(
        "CompactionStatus",
        doc(
            obj(
                &[
                    "dataset",
                    "enabled",
                    "serverEnabled",
                    "policy",
                    "own",
                    "state",
                    "measures",
                    "automaticRuns",
                    "failures",
                ],
                json!({
                    "dataset": string(),
                    "enabled": { "type": "boolean", "description": "The server's switch, the dataset's own and a writable server." },
                    "serverEnabled": { "type": "boolean", "description": "False under `--no-auto-compact`." },
                    "policy": with_desc(policy, "The settings in effect."),
                    "own": with_desc(own, "The settings the dataset overrides."),
                    "state": string_enum(&["off", "idle", "due", "deferred", "running"]),
                    "trigger": string(),
                    "triggerKind": string_enum(&["max-delta", "ratio", "delta-bytes", "wal-bytes", "idle", "age"]),
                    "deferred": string_enum(&[
                        "min-interval", "backoff", "bulk-load", "reasoning", "clone", "backup",
                        "restore", "history", "disk", "running-limit", "slots",
                    ]),
                    "deferredDetail": string(),
                    "task": { "type": "string", "description": "The running compaction task." },
                    "measures": obj(
                        &[
                            "generation", "baseQuads", "deltaQuads", "deltaBytes", "walBytes",
                            "idleSeconds", "oldestChangeSeconds", "threshold",
                        ],
                        json!({
                            "generation": string(),
                            "baseQuads": int(),
                            "deltaQuads": int(),
                            "deltaBytes": int(),
                            "walBytes": int(),
                            "idleSeconds": nullable("integer"),
                            "oldestChangeSeconds": nullable("integer"),
                            "threshold": { "type": "integer", "description": "The delta size of the `deltaRatio` trigger." },
                        }),
                    ),
                    "last": obj(
                        &["automatic", "startedAt", "finishedAt", "seconds", "outcome"],
                        json!({
                            "automatic": boolean(),
                            "trigger": string(),
                            "startedAt": string(),
                            "finishedAt": string(),
                            "seconds": num(),
                            "outcome": string_enum(&["done", "abandoned", "cancelled", "failed"]),
                            "generation": string(),
                            "lockMs": num(),
                            "buildMs": num(),
                            "caughtUpCommits": int(),
                            "mode": string_enum(&["full", "partial"]),
                            "blocksRewritten": int(),
                            "blocksCopied": int(),
                            "fullReason": string(),
                            "error": string(),
                        }),
                    ),
                    "automaticRuns": int(),
                    "failures": { "type": "integer", "description": "Consecutive failed automatic compactions." },
                }),
            ),
            "The dataset's automatic compaction: its settings, state, measures and last run.",
            "automatic-compaction",
        ),
    );
}
