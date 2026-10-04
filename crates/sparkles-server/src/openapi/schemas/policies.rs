//! Backup bodies described member by member: the requests of backups, restores,
//! verifications and garbage collection, repository locks, and lifecycle policies with
//! their runs, schedule previews and retention.

use super::kit::*;
use super::*;

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    requests(put);
    policies(put);
}

fn requests(put: &mut dyn FnMut(&str, J)) {
    put(
        "BackupRequest",
        doc(
            obj(
                &["repository"],
                json!({
                    "repository": string(),
                    "name": { "type": "string", "description": "The backup's name; generated when left out." },
                    "note": string(),
                }),
            ),
            "A backup to make now.",
            "backup-routes",
        ),
    );
    put(
        "RestoreRequest",
        doc(
            obj(
                &[],
                json!({
                    "target": { "type": "string", "description": "The dataset to create, or to replace. `{ds}` by default." },
                    "replace": { "type": "boolean", "default": false, "description": "Replace the registered dataset `target` in place." },
                    "identity": { "type": "string", "enum": ["auto", "new", "keep"], "default": "auto" },
                    "check": { "type": "string", "enum": ["quick", "full", "none"], "default": "quick", "description": "The `sparkles check` run before the dataset is published." },
                    "keepReplaced": { "type": "boolean", "default": false, "description": "In place: keep the old files." },
                }),
            ),
            "Where and how to restore a backup.",
            "restore",
        ),
    );
    put(
        "VerifyRequest",
        doc(
            obj(
                &[],
                json!({ "level": { "type": "string", "enum": ["exists", "data", "restore"], "default": "exists", "description": "`restore` verifies one backup only." } }),
            ),
            "How deep a verification goes.",
            "backup-routes",
        ),
    );
    put(
        "GcRequest",
        doc(
            obj(
                &[],
                json!({
                    "dryRun": { "type": "boolean", "default": false },
                    "graceHours": { "type": "number", "default": 24, "description": "Unreferenced blobs younger than this are kept." },
                }),
            ),
            "A garbage collection of a repository.",
            "backup-routes",
        ),
    );
    put(
        "LockList",
        doc(
            obj(
                &["locks"],
                json!({
                    "locks": array(obj(
                        &["id", "kind", "operation", "holder", "created", "lastModified", "stale"],
                        json!({
                            "id": string(),
                            "kind": string_enum(&["shared", "exclusive"]),
                            "operation": string_enum(&["create", "restore", "verify", "delete", "gc"]),
                            "holder": obj(&["host", "pid", "server", "version"], json!({
                                "host": string(),
                                "pid": int(),
                                "server": { "type": "string", "description": "A hash of the holder's data directory, empty for the CLI." },
                                "version": string(),
                            })),
                            "created": string(),
                            "lastModified": { "type": "string", "description": "The storage server's time of the lock's last refresh." },
                            "stale": { "type": "boolean", "description": "Not refreshed for 30 minutes: ignored, and removed by garbage collection." },
                        }),
                    )),
                }),
            ),
            "The locks held in a repository.",
            "locks-and-garbage-collection",
        ),
    );
}

fn policies(put: &mut dyn FnMut(&str, J)) {
    let config_members = json!({
        "name": { "type": "string", "pattern": "^[a-z0-9][a-z0-9_-]{0,63}$" },
        "repository": string(),
        "datasets": { "type": "array", "items": string(), "default": ["*"], "description": "Names or `*` globs." },
        "schedule": { "type": "string", "description": "A cron expression with 5 or 6 fields, or `every <duration>`." },
        "timezone": { "type": "string", "default": "UTC", "description": "An IANA time zone." },
        "nameTemplate": { "type": "string", "default": "{policy}-{dataset}-{time}" },
        "retention": obj(&[], json!({
            "expireAfter": { "type": ["string", "null"], "description": "A duration such as `30d`. `null` sets no age limit." },
            "minCount": { "type": "integer", "default": 1, "description": "The newest backups kept whatever their age." },
            "maxCount": nullable("integer"),
        })),
        "skipUnchanged": { "type": "boolean", "default": false, "description": "Skip a dataset whose head is its last policy backup's commit." },
        "gcAfterRetention": { "type": "boolean", "default": false, "description": "Collect the repository after retention deleted something, at most once a day." },
        "catchUp": { "type": "string", "enum": ["one", "none"], "default": "one", "description": "What happens to instants missed while the server was down." },
        "enabled": { "type": "boolean", "default": true },
    });
    put(
        "PolicyConfig",
        doc(
            obj(&["repository", "schedule"], config_members.clone()),
            "A backup policy's settings. `name` is required on create, and a replacement may leave it out but cannot change it.",
            "lifecycle-policies",
        ),
    );
    let dataset_result = string_enum(&["ok", "failed", "skipped"]);
    put(
        "PolicyRun",
        doc(
            obj(
                &[
                    "id",
                    "policy",
                    "trigger",
                    "scheduledFor",
                    "started",
                    "finished",
                    "result",
                    "datasets",
                    "retention",
                    "gc",
                ],
                json!({
                    "id": string(),
                    "policy": string(),
                    "trigger": string_enum(&["schedule", "catch-up", "manual"]),
                    "scheduledFor": nullable("string"),
                    "started": string(),
                    "finished": nullable("string"),
                    "result": string_enum(&["ok", "partial", "failed", "skipped"]),
                    "reason": { "type": "string", "description": "Why a scheduled run was skipped." },
                    "datasets": array(obj(
                        &["dataset", "backup", "result"],
                        json!({
                            "dataset": string(),
                            "backup": nullable("string"),
                            "result": dataset_result,
                            "reason": string(),
                            "addedBytes": int(),
                            "millis": int(),
                        }),
                    )),
                    "retention": or_null(obj(&["deleted"], json!({ "deleted": strings(), "error": string() }))),
                    "gc": or_null(obj(&["task"], json!({ "task": { "type": "string", "description": "The `backup-gc` task the run started." } }))),
                }),
            ),
            "One run of a backup policy.",
            "backup-types",
        ),
    );
    let mut policy_members = config_members;
    policy_members["source"] = string_enum(&["api", "config"]);
    policy_members["state"] = obj(
        &[
            "nextRun",
            "lastScheduledFor",
            "lastRun",
            "lastSuccess",
            "consecutiveFailures",
            "runningTask",
        ],
        json!({
            "nextRun": nullable("string"),
            "lastScheduledFor": nullable("string"),
            "lastRun": or_null(sref("PolicyRun")),
            "lastSuccess": nullable("string"),
            "consecutiveFailures": int(),
            "runningTask": nullable("string"),
        }),
    );
    put(
        "Policy",
        doc(
            obj(
                &[
                    "name",
                    "repository",
                    "datasets",
                    "schedule",
                    "timezone",
                    "nameTemplate",
                    "retention",
                    "skipUnchanged",
                    "gcAfterRetention",
                    "catchUp",
                    "enabled",
                    "source",
                    "state",
                ],
                policy_members,
            ),
            "A backup policy: its settings, where it is defined, and its scheduler state.",
            "lifecycle-policies",
        ),
    );
    put(
        "PolicyList",
        doc(
            obj(&["policies"], json!({ "policies": array(sref("Policy")) })),
            "The backup policies.",
            "lifecycle-policies",
        ),
    );
    put(
        "PolicyRuns",
        doc(
            obj(&["runs"], json!({ "runs": array(sref("PolicyRun")) })),
            "A policy's runs, newest first.",
            "backup-routes",
        ),
    );
    put(
        "SchedulePreviewRequest",
        doc(
            obj(
                &["schedule"],
                json!({
                    "schedule": string(),
                    "timezone": { "type": "string", "default": "UTC" },
                    "count": { "type": "integer", "default": 5, "maximum": 20, "description": "How many instants to list." },
                    "nameTemplate": { "type": "string", "description": "A template to render a sample name with." },
                    "dataset": { "type": "string", "description": "The dataset of the sample name, `ds` by default." },
                }),
            ),
            "A schedule to preview.",
            "backup-routes",
        ),
    );
    put(
        "SchedulePreview",
        doc(
            obj(
                &["next", "description"],
                json!({
                    "next": { "type": "array", "items": string(), "description": "The next instants, RFC 3339 in UTC." },
                    "description": { "type": "string", "examples": ["at 02:30 every day (Europe/Berlin)"] },
                    "sample": { "type": "string", "description": "`nameTemplate` rendered for `dataset` at the first instant." },
                }),
            ),
            "The next runs of a schedule.",
            "backup-routes",
        ),
    );
    put(
        "RetentionResult",
        doc(
            obj(
                &["dryRun", "delete", "keep"],
                json!({
                    "dryRun": boolean(),
                    "delete": { "type": "array", "items": sref("BackupSummary"), "description": "The backups deleted, or with `dryRun` those that would be. A deletion that failed stays here." },
                    "keep": array(sref("BackupSummary")),
                    "errors": strings(),
                }),
            ),
            "What a policy's retention deleted and kept.",
            "backup-routes",
        ),
    );
}
