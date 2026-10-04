//! Bodies about the data's changes and its shapes, member by member: diffs, the change
//! feed, write previews and their validation summary, the constraints layer, and the
//! requests of history settings, snapshots and stored query versions.

use super::kit::*;
use super::*;

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    changes(put);
    previews(put);
    constraints(put);
    history(put);
}

fn changes(put: &mut dyn FnMut(&str, J)) {
    put(
        "ChangedQuad",
        doc(
            obj(
                &["op", "subject", "predicate", "object", "graph"],
                json!({
                    "op": { "type": "string", "enum": ["+", "-"], "description": "`+` added, `-` removed." },
                    "subject": { "type": "string", "description": "In N-Triples syntax, as are the other terms." },
                    "predicate": string(),
                    "object": string(),
                    "graph": { "type": ["string", "null"], "description": "`null` for the default graph." },
                }),
            ),
            "A quad added or removed.",
            "diffs-between-commits",
        ),
    );
    let side = obj(
        &["selector", "commit"],
        json!({
            "selector": { "type": "string", "description": "The state as asked for, such as `commit:4` or `head`." },
            "commit": sref("Commit"),
        }),
    );
    put(
        "Diff",
        doc(
            obj(
                &[
                    "dataset",
                    "datasetId",
                    "from",
                    "to",
                    "added",
                    "removed",
                    "method",
                ],
                json!({
                    "dataset": string(),
                    "datasetId": string(),
                    "from": side.clone(),
                    "to": side,
                    "added": int(),
                    "removed": int(),
                    "method": string_enum(&["same", "log", "compare"]),
                    "logChanges": { "type": "integer", "description": "Changes read from the write-ahead logs. Left out for a caller limited to some graphs." },
                    "compared": { "type": "integer", "description": "Quads compared state against state. Left out for a caller limited to some graphs." },
                    "quads": { "type": "array", "items": sref("ChangedQuad"), "description": "With `quads=true`: removals first, then additions, up to `limit`." },
                }),
            ),
            "The net change between two states: counts, and the quads with `quads=true`.",
            "diffs-between-commits",
        ),
    );
    put(
        "ChangeFeed",
        doc(
            obj(
                &["dataset", "datasetId", "after", "next", "head", "commits"],
                json!({
                    "dataset": string(),
                    "datasetId": string(),
                    "after": { "type": "integer", "description": "The commit the page starts after." },
                    "next": { "type": "integer", "description": "The commit to ask after next." },
                    "head": sref("Commit"),
                    "commits": array(obj(
                        &["commit", "complete"],
                        json!({
                            "commit": sref("Commit"),
                            "added": { "type": "integer", "description": "Left out of an incomplete commit for a caller limited to some graphs." },
                            "removed": int(),
                            "complete": { "type": "boolean", "description": "False for a commit whose changes are not listed: over the rows budget, or a bulk commit the change log holds only the counts of." },
                            "changes": array(sref("ChangedQuad")),
                        }),
                    )),
                }),
            ),
            "The commits after `after`, oldest first, with their changes, and `next`.",
            "change-feed",
        ),
    );
}

fn previews(put: &mut dyn FnMut(&str, J)) {
    let by_severity = obj(
        &["violation", "warning", "info"],
        json!({ "violation": int(), "warning": int(), "info": int() }),
    );
    put(
        "ValidationSummary",
        doc(
            obj(
                &[
                    "language",
                    "status",
                    "mode",
                    "strategy",
                    "threshold",
                    "conforms",
                    "blocking",
                    "total",
                    "bySeverity",
                    "limit",
                    "truncated",
                    "millis",
                    "results",
                ],
                json!({
                    "language": string_enum(&["shacl", "shex"]),
                    "status": string_enum(&["passed", "warned", "rejected", "skipped", "bypassed"]),
                    "mode": string_enum(&["reject", "warn", "off"]),
                    "strategy": string_enum(&["full", "incremental", "none"]),
                    "threshold": string_enum(&["violation", "warning", "info"]),
                    "conforms": boolean(),
                    "blocking": { "type": "integer", "description": "Results at or above the threshold." },
                    "total": int(),
                    "bySeverity": by_severity,
                    "limit": int(),
                    "truncated": boolean(),
                    "millis": int(),
                    "results": {
                        "type": "array",
                        "items": any_object("A SHACL result, or a ShEx result-map entry."),
                        "description": "The first `limit` results, blocking first.",
                    },
                    "shapesError": string(),
                    "introduced": { "type": "integer", "description": "Grandfather mode: the blocking results the write introduced." },
                    "focusNodes": { "type": "integer", "description": "The focus nodes an incremental validation validated." },
                    "fallback": { "type": "string", "description": "Why the write, or a shape, was validated in full." },
                    "head": { "type": "integer", "description": "Rejections: the commit the write ran against." },
                    "kind": { "type": "string", "description": "Rejections: the kind of the commit the write would have made." },
                }),
            ),
            "What write-time validation found about a write.",
            "write-time-validation",
        ),
    );
    put(
        "DryRunReport",
        doc(
            obj(
                &[
                    "dryRun",
                    "dataset",
                    "datasetId",
                    "committed",
                    "wouldCommit",
                    "outcome",
                    "head",
                    "commit",
                    "graphs",
                    "storage",
                ],
                json!({
                    "dryRun": { "const": true },
                    "dataset": string(),
                    "datasetId": string(),
                    "committed": { "const": false },
                    "wouldCommit": boolean(),
                    "outcome": string_enum(&["commit", "no-change", "precondition-failed", "rejected", "storage-refused"]),
                    "head": { "type": "integer", "description": "The commit the write ran against." },
                    "commit": with_desc(sref("Commit"), "The commit the write would create, without `timestamp` and `digest`, or the head when it would create none."),
                    "graphs": array(obj(
                        &["graph", "inserted", "deleted"],
                        json!({ "graph": nullable("string"), "inserted": int(), "deleted": int() }),
                    )),
                    "changes": obj(
                        &["total", "limit", "truncated", "quads"],
                        json!({
                            "total": int(),
                            "limit": int(),
                            "truncated": boolean(),
                            "quads": array(sref("ChangedQuad")),
                        }),
                    ),
                    "validation": sref("ValidationSummary"),
                    "precondition": obj(
                        &["status"],
                        json!({ "status": string_enum(&["passed", "failed"]), "error": string() }),
                    ),
                    "storage": obj(
                        &["status"],
                        json!({
                            "status": string_enum(&["fits", "refused"]),
                            "limit": int(),
                            "used": int(),
                            "projected": int(),
                            "budget": string(),
                            "code": string(),
                            "error": string(),
                        }),
                    ),
                    "error": { "type": "string", "description": "The error the write would get." },
                    "code": string(),
                    "budget": string(),
                    "limit": int(),
                    "requested": int(),
                }),
            ),
            "The preview of a write run with `dryRun=true`: the commit it would make, its changes, validation, preconditions and storage.",
            "write-previews",
        ),
    );
}

fn constraints(put: &mut dyn FnMut(&str, J)) {
    let property = obj(
        &["path", "severity", "enforcement"],
        json!({
            "path": string(),
            "shape": string(),
            "severity": { "type": "string", "description": "An IRI, `sh:Violation` unless the shape says otherwise." },
            "enforcement": string_enum(&["reject-on-write", "warn-on-write", "validated-on-request"]),
            "minCount": int(),
            "maxCount": int(),
            "datatype": string(),
            "class": strings(),
            "nodeKind": string(),
            "other": { "type": "array", "items": string(), "description": "The components of the other constraints, such as `sh:PatternConstraintComponent`." },
        }),
    );
    let class = obj(
        &["class", "shapes", "closed", "properties", "otherPaths"],
        json!({
            "class": string(),
            "shapes": strings(),
            "closed": boolean(),
            "properties": array(property),
            "otherPaths": { "type": "integer", "description": "Property shapes whose path is not a single predicate." },
        }),
    );
    let source = obj(
        &["kind", "graphs", "shapes", "otherTargets", "classes"],
        json!({
            "kind": string_enum(&["guard", "graphs"]),
            "graphs": { "type": "array", "items": string(), "description": "The shapes graphs read, `default` for the default graph." },
            "file": { "const": true, "description": "The guard also has shapes from a file or given inline." },
            "mode": string_enum(&["reject", "warn"]),
            "threshold": string_enum(&["violation", "warning", "info"]),
            "shapes": int(),
            "otherTargets": { "type": "integer", "description": "Active shapes whose targets are not classes." },
            "classes": array(class),
        }),
    );
    put(
        "ConstraintsLayer",
        doc(
            obj(&["sources"], json!({ "sources": array(source) })),
            "The SHACL constraints layer of a schema report.",
            "constraints-layer",
        ),
    );
    put(
        "ConstraintsReport",
        doc(
            obj(
                &["schemaFormat", "dataset", "snapshot", "constraints"],
                json!({
                    "schemaFormat": { "const": 1 },
                    "dataset": string(),
                    "snapshot": obj(&["version", "generation"], json!({ "version": int(), "generation": string() })),
                    "constraints": sref("ConstraintsLayer"),
                }),
            ),
            "The constraints layer alone, without counting anything.",
            "constraints-layer",
        ),
    );
}

fn history(put: &mut dyn FnMut(&str, J)) {
    let age = json!({ "type": ["string", "integer", "null"], "description": "Seconds, or a duration such as `90s`, `30m`, `12h`, `7d` or `2w`." });
    let size = json!({ "type": ["integer", "string", "null"], "description": "Bytes, or a size such as `512MiB` or `10GiB`." });
    put(
        "HistoryRequest",
        doc(
            obj(
                &[],
                json!({
                    "keepCommits": nullable("integer"),
                    "keepAge": age.clone(),
                    "maxBytes": size.clone(),
                    "schedules": {
                        "type": ["array", "null"],
                        "items": obj(&["prefix", "every", "keepLast"], json!({
                            "prefix": string(),
                            "every": { "type": ["string", "integer"], "description": "Seconds, or a duration." },
                            "keepLast": int(),
                        })),
                        "description": "Replaces the pin schedules when present. `null` removes them.",
                    },
                    "catalog": {
                        "type": ["object", "null"],
                        "properties": { "keepCommits": nullable("integer"), "keepAge": age.clone() },
                        "description": "Replaces the commit catalog's horizon when present.",
                    },
                    "changeLog": {
                        "type": ["object", "null"],
                        "additionalProperties": false,
                        "properties": {
                            "enabled": nullable("boolean"),
                            "keepCommits": nullable("integer"),
                            "keepAge": age,
                            "maxBytes": size,
                        },
                        "description": "Replaces the change log settings when present. `null` restores the server's.",
                    },
                }),
            ),
            "The retention window, and optionally the pin schedules, the catalog horizon and the change log settings.",
            "named-snapshots-and-retention",
        ),
    );
    put(
        "SnapshotRequest",
        doc(
            obj(
                &[],
                json!({
                    "name": { "type": "string", "description": "Required, here or in the query string." },
                    "at": { "type": ["string", "integer"], "description": "A point-in-time selector; the head by default." },
                    "note": string(),
                    "expires": { "type": ["string", "integer"], "description": "An RFC 3339 time, a duration from now, or seconds." },
                    "warm": { "type": ["boolean", "string"], "description": "Keep the snapshot materialized." },
                }),
            ),
            "A named snapshot to create. Each member can also be a query parameter.",
            "named-snapshots-and-retention",
        ),
    );
    put(
        "StoredQueryVersions",
        doc(
            obj(
                &["dataset", "name", "versions"],
                json!({
                    "dataset": string(),
                    "name": string(),
                    "versions": array(obj(
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
                    )),
                }),
            ),
            "The kept versions of a stored query, newest first.",
            "stored-queries",
        ),
    );
}
