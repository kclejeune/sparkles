//! The bodies of spec C18: model providers and their test calls.

use super::kit::*;
use super::*;

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    models(put);
    tools(put);
    memory(put);
    asking(put);
    explaining(put);
    ingestion(put);
    maintenance(put);
}

fn explaining(put: &mut dyn FnMut(&str, J)) {
    put(
        "ExplainRequest",
        doc(
            closed(
                &["query"],
                json!({
                    "query": with_desc(string(), "Any query form. An update is refused with `not-a-query`."),
                    "profile": with_desc(string_enum(&["estimate", "run", "given"]), "`estimate`, the default, plans without running. `run` runs the query read-only as the caller and counts its rows without returning them. `given` explains `plan`."),
                    "plan": {
                        "description": "With `profile: \"given\"`, the plan the client received, at most 10,000 nodes and 2 MiB.",
                        "oneOf": [sref("PlanNode"), sref("CursorPlan")],
                    },
                    "commit": with_desc(int(), "With `profile: \"given\"`, the commit of that plan."),
                    "error": any_object("With `profile: \"given\"`, the error body of the run that stopped, which names the budget."),
                    "describe": with_desc(boolean(), "Whether to call the `explain` role. `true` by default when the dataset enables `explain`."),
                    "timeoutSeconds": with_desc(num(), "The deadline of a run, 30 by default."),
                    "at": { "description": "A commit or time to read, as for `/{ds}/sparql`." },
                    "branch": string(),
                    "reasoning": boolean(),
                }),
            ),
            "A query to explain.",
            "explaining-a-query",
        ),
    );
    let note = obj(
        &["node", "code", "severity", "text", "source"],
        json!({
            "node": or_null(string()),
            "code": string(),
            "severity": string_enum(&["high", "warning", "info"]),
            "text": string(),
            "source": string_enum(&["explain", "planner", "lint", "schema", "model"]),
            "range": any_object("The editor range of a lint finding that names no node."),
        }),
    );
    let sentence = obj(
        &["text", "nodes"],
        json!({ "text": string(), "nodes": strings() }),
    );
    put(
        "ExplainResult",
        doc(
            obj(
                &[
                    "dataset",
                    "queryType",
                    "profile",
                    "executed",
                    "plan",
                    "nodes",
                    "notes",
                    "explanation",
                ],
                json!({
                    "dataset": string(),
                    "commit": or_null(int()),
                    "branch": string(),
                    "queryType": string(),
                    "profile": string_enum(&["estimate", "run", "given"]),
                    "executed": with_desc(boolean(), "Whether the plan has actual counts."),
                    "plan": { "oneOf": [sref("PlanNode"), sref("CursorPlan")] },
                    "estimatedRows": or_null(num()),
                    "rows": with_desc(int(), "After a run that finished, its rows."),
                    "elapsedMs": num(),
                    "stop": any_object("The budget that stopped the query: `budget`, `limit` and `elapsedMs`."),
                    "error": any_object("The error body of a run that a budget stopped."),
                    "warnings": array(obj(&["code", "message"], json!({ "code": string(), "message": string() }))),
                    "nodes": array(any_object("One operator: `id`, `operator`, `description`, `columns`, `estimatedRows` (null when hidden), `estimatedCost`, `actualRows`, `timeMs`, `selfMs`, `complete`, `partial`, `skipped`, `stoppedEarly`, `cached`, `runs` and `pushedFilters`.")),
                    "notes": array(note.clone()),
                    "shownNotes": with_desc(int(), "How many notes to show before **More**."),
                    "hiddenEstimates": boolean(),
                    "explanation": obj(
                        &["source", "asks", "notes"],
                        json!({
                            "source": string_enum(&["template", "model"]),
                            "asks": array(sentence.clone()),
                            "notes": array(note),
                            "provider": string(),
                            "model": string(),
                            "dropped": int(),
                            "replaced": int(),
                            "template": obj(&["asks"], json!({ "asks": array(sentence) })),
                            "fallback": { "description": "Why the template text is shown although a model was asked." },
                        }),
                    ),
                    "usage": any_object("The model calls: tokens, `modelCalls`, `failedCalls` and `steps`."),
                }),
            ),
            "The explanation in the JSON form: the members of the `plan` and `notes` events, and the `explanation` and `usage` events as members.",
            "explaining-a-query",
        ),
    );
}

/// `POST /$/ingest/{ds}` and its tasks (C18 Phase 4).
fn ingestion(put: &mut dyn FnMut(&str, J)) {
    let options = json!({
        "format": with_desc(string(), "The media type of `text` or of the file, such as `text/html` or `application/pdf`."),
        "name": with_desc(string(), "The file name, which may say the format."),
        "url": with_desc(string(), "Where the document comes from; fetched through the outbound policy when there is no text or file, and the source's IRI by default."),
        "title": string(),
        "iri": with_desc(string(), "The source's IRI."),
        "graph": with_desc(string(), "The named graph of the source and its facts."),
        "profile": with_desc(string(), "The ingest profile (default `default`)."),
        "mode": with_desc(string_enum(&["branch", "preview", "auto", "memory"]), "`memory` extracts a registered `source` of agent memory and writes its facts on main into the source's graph, which `agentGraphs` must match."),
        "branch": with_desc(string(), "The review branch (default `ingest.<slug>-<n>`)."),
        "allowPartial": with_desc(boolean(), "Register what can be read of a PDF that needs OCR, and record the other pages."),
        "extract": with_desc(boolean(), "Extract facts with the `extract` role (default: when the dataset lets ingestion use a provider)."),
        "confirm": with_desc(boolean(), "Confirm the estimate in advance."),
        "base": with_desc(string(), "The namespace of a table's rows in its mapping draft."),
        "message": with_desc(string(), "The commit message of the registration."),
        "deadlineSeconds": with_desc(num(), "The task's deadline, 1 to 86400 seconds (3600 by default)."),
    });
    let mut json_body = options.clone();
    json_body["text"] = with_desc(string(), "The document's text.");
    json_body["source"] = with_desc(
        string(),
        "The graph IRI of a registered source to extract from instead of a document: its current rendition goes through the estimate, the `extract` role and linking. Leave out text, url and the file.",
    );
    put(
        "IngestRequest",
        doc(
            closed(&[], json_body),
            "An ingestion from text or a URL. A multipart request sends the document as its `file` part and these options as fields.",
            "ingestion",
        ),
    );
    let mut form = options;
    form["file"] = json!({ "type": "string", "contentMediaType": "application/octet-stream" });
    put(
        "IngestForm",
        doc(
            closed(&["file"], form),
            "An ingestion of an uploaded file, with the options of `IngestRequest` as fields.",
            "ingestion",
        ),
    );
    put(
        "IngestTask",
        doc(
            obj(
                &[
                    "id",
                    "dataset",
                    "status",
                    "progress",
                    "createdAt",
                    "updatedAt",
                    "input",
                    "usage",
                ],
                json!({
                    "id": string(),
                    "dataset": string(),
                    "status": string_enum(&["queued", "scanning", "converting", "registering", "awaiting-confirmation", "extracting", "linking", "writing", "awaiting-approval", "done", "failed", "cancelled"]),
                    "progress": with_desc(num(), "From 0 to 1."),
                    "message": string(),
                    "createdAt": string(),
                    "updatedAt": string(),
                    "finishedAt": string(),
                    "input": any_object("What was ingested: its name, format, size, URL or source and mode. A maintenance task has `kind` `consolidation` or `retention`, and `scheduled`."),
                    "estimate": any_object("The estimate of the extraction: chunks, tokens, the first pair, its estimated cost, the threshold and whether it needs a confirmation."),
                    "usage": any_object("Model calls, tokens, estimated cost, escalations and the pair that answered each chunk."),
                    "result": any_object("The outcome (`registered`, `proposed`, `no-facts`, `preview`, `merged`, `approved`, `already-registered` or `mapping-draft`), with the source, rendition, branch, pages, proposals, or a table's mapping draft and preview. A consolidation's outcome is `proposed`, `merged`, `dry-run`, `nothing-to-consolidate`, `no-facts` or `pending-review`, with the branch, the repeated facts, the duplicates and the conflicts. A retention's is `deleted`, `dry-run` or `nothing-to-delete`, with each session graph and why it is kept or deleted."),
                    "error": any_object("The code and message of a failed task, such as `needs-ocr` with the pages that need OCR and their reasons."),
                }),
            ),
            "An ingestion task with its progress, estimate, usage and result.",
            "ingestion",
        ),
    );
    put(
        "IngestTaskList",
        doc(
            obj(
                &["dataset", "tasks", "capabilities"],
                json!({
                    "dataset": string(),
                    "tasks": array(sref("IngestTask")),
                    "capabilities": obj(&["pdf", "ocr"], json!({ "pdf": boolean(), "ocr": boolean() })),
                }),
            ),
            "The caller's ingestion tasks, newest first, and whether this server converts PDFs and reads scanned pages.",
            "ingestion",
        ),
    );
}

fn asking(put: &mut dyn FnMut(&str, J)) {
    let pair = obj(
        &["provider", "model"],
        json!({ "provider": string(), "model": string() }),
    );
    let send = string_enum(&["schema", "rows", "documents"]);
    let turn = closed(
        &["question", "query"],
        json!({ "question": string(), "query": string() }),
    );
    let choice = closed(&["value"], json!({ "id": string(), "value": string() }));
    put(
        "AskRequest",
        doc(
            closed(
                &["question"],
                json!({
                    "question": with_desc(string(), "At most 2000 characters."),
                    "context": with_desc(array(turn), "Earlier turns of the conversation, at most 5."),
                    "clarification": {
                        "description": "The chosen value of a `clarify` event, as `{id, value}` or as the value alone.",
                        "oneOf": [choice, string()],
                    },
                    "at": { "description": "A commit or time to read, as for `/{ds}/sparql`." },
                    "branch": string(),
                    "reasoning": boolean(),
                    "run": with_desc(boolean(), "Whether to run the checked query. `true` by default."),
                    "summary": with_desc(boolean(), "Whether to summarize the rows. `true` by default when the dataset sends rows."),
                    "maxRows": with_desc(int(), "The rows returned in `result`, 1000 by default."),
                    "tryHarder": with_desc(string(), "The id of an earlier ask of the caller. Drafting starts at the pair after the one that drafted it."),
                    "reviewedOnly": with_desc(boolean(), "Hide the agent memory graphs of the dataset from the query."),
                    "query": with_desc(string(), "A query to check, run and summarize without drafting, for **Summarize again** after an edit."),
                }),
            ),
            "A question for the dataset.",
            "asking-in-the-server",
        ),
    );
    put(
        "AskResult",
        doc(
            obj(
                &["dataset", "question", "outcome", "usage"],
                json!({
                    "dataset": string(),
                    "question": string(),
                    "outcome": string_enum(&["answered", "empty", "checked", "not-run", "clarify", "unanswerable", "failed", "error"]),
                    "result": any_object("The checked query with `explanation`, `assumptions`, `terms`, `graph`, `commit`, `attempt`, `verdict` and `results` in the `application/x-sparkles+json` form."),
                    "summary": obj(&["text", "citations"], json!({ "text": string(), "citations": array(int()), "rowsSent": int(), "provider": string(), "model": string() })),
                    "clarify": any_object("`{id, question, choices: [{label, value}]}`."),
                    "error": obj(&["code", "message"], json!({ "code": string(), "message": string() })),
                    "attempts": array(any_object("One draft with its check and run.")),
                    "notes": strings(),
                    "usage": any_object("`askId`, the tokens and estimated cost, `complexity`, the `steps` with their pairs and signals, `escalations`, `draftPair`, `answeredBy` and `tryHarder`."),
                }),
            ),
            "The whole answer of an ask in the JSON form.",
            "asking-in-the-server",
        ),
    );
    let mut roles = Map::new();
    for r in [
        "draft",
        "repair",
        "summarize",
        "extract",
        "explain",
        "optimize",
    ] {
        roles.insert(r.into(), array(pair.clone()));
    }
    put(
        "AssistantSettings",
        doc(
            obj(
                &[],
                json!({
                    "enabled": with_desc(boolean(), "Whether the dataset has an assistant. `false` by default."),
                    "roles": { "type": "object", "description": "Role lists that replace the server's for this dataset.", "properties": J::Object(roles) },
                    "ask": with_desc(boolean(), "Whether `POST /{ds}/ask` is enabled. `true` by default."),
                    "explain": boolean(),
                    "optimize": boolean(),
                    "ingest": boolean(),
                    "send": with_desc(send.clone(), "What may leave the server. `schema` by default."),
                    "sendByProvider": { "type": "object", "description": "A lower `send` level for some providers.", "additionalProperties": send },
                    "rowsForSummary": with_desc(int(), "The rows sent to the summary, 50 by default."),
                    "budget": closed(&[], json!({ "perRequest": int(), "perPrincipalPerDay": int(), "perDatasetPerDay": int() })),
                    "deadlineSecs": with_desc(num(), "The time an ask may take, 120 seconds by default."),
                    "historyDays": with_desc(int(), "The days an asked question is kept. 0 keeps nothing."),
                    "routing": any_object("`complexityThreshold` and `exampleScore` of the escalation rules."),
                    "status": with_desc(obj(&["models", "historyDays", "ask"], json!({
                        "models": boolean(),
                        "historyDays": int(),
                        "ask": boolean(),
                        "reason": string(),
                        "draft": array(pair.clone()),
                        "summary": with_desc(boolean(), "Whether an answer can carry a summary."),
                    })), "Answered by the server and ignored in a `PUT`."),
                }),
            ),
            "The assistant settings of a dataset.",
            "assistant-settings",
        ),
    );
    put(
        "AskHistory",
        doc(
            obj(
                &["dataset", "historyDays", "asks"],
                json!({
                    "dataset": string(),
                    "historyDays": int(),
                    "asks": array(obj(&["id", "principal", "at", "question", "result", "outcome", "routing"], json!({
                        "id": string(),
                        "principal": string(),
                        "at": { "type": "string", "format": "date-time" },
                        "question": string(),
                        "query": string(),
                        "commit": int(),
                        "result": with_desc(string(), "The pipeline's outcome."),
                        "outcome": string_enum(&["none", "accepted", "edited", "rejected"]),
                        "note": string(),
                        "routing": any_object("The complexity, steps, escalations, answering pair and tokens."),
                    }))),
                }),
            ),
            "The caller's asked questions, newest first.",
            "ask-history",
        ),
    );
    put(
        "AskFeedback",
        doc(
            closed(
                &["outcome"],
                json!({
                    "outcome": string_enum(&["accepted", "edited", "rejected"]),
                    "note": with_desc(string(), "At most 1000 characters."),
                }),
            ),
            "Feedback on an answer.",
            "ask-history",
        ),
    );
    put(
        "ModelUsage",
        doc(
            obj(
                &["days", "datasets"],
                json!({
                    "days": int(),
                    "datasets": array(obj(&["dataset", "asks"], json!({
                        "dataset": string(),
                        "asks": int(),
                        "answers": array(any_object("`{role, provider, model, outcome, count}`.")),
                        "escalations": array(any_object("`{role, signal, count}`.")),
                        "outcomes": array(any_object("`{bucket, outcome, count}`.")),
                        "tokens": array(any_object("`{provider, model, inputTokens, outputTokens, estimatedCost}`.")),
                    }))),
                }),
            ),
            "The routing counters by dataset.",
            "asking-in-the-server",
        ),
    );
}

/// The members every tool body may hold to pick a state.
fn read_args() -> Vec<(&'static str, J)> {
    vec![
        (
            "reasoning",
            with_desc(
                boolean(),
                "Include materialized inferences (default: true when the dataset has them).",
            ),
        ),
        (
            "timeoutSeconds",
            with_desc(num(), "At most the server's query timeout. 30 by default."),
        ),
        (
            "atCommit",
            with_desc(int(), "Read the snapshot of this commit."),
        ),
        (
            "at",
            json!({ "type": ["integer", "string"], "description": "A commit number, `commit:N`, `time:<RFC 3339>`, `snapshot:<name>` or `head`." }),
        ),
    ]
}

fn with_read_args(mut props: J) -> J {
    for (k, v) in read_args() {
        props[k] = v;
    }
    props
}

fn tools(put: &mut dyn FnMut(&str, J)) {
    let issue = obj(
        &["code", "severity", "message"],
        json!({
            "code": string(),
            "severity": string_enum(&["error", "warning"]),
            "message": string(),
            "term": string(),
            "line": int(),
            "column": int(),
            "suggestions": array(obj(&["term", "count", "why"], json!({
                "term": string(), "label": string(), "count": int(), "why": string(),
            }))),
        }),
    );
    put(
        "CheckRequest",
        doc(
            closed(
                &["query"],
                with_read_args(json!({
                    "query": with_desc(string(), "A SPARQL query. The dataset's prefixes are predeclared."),
                    "explain": with_desc(boolean(), "Add the plan's estimated rows and its warnings."),
                    "maxSuggestions": with_desc(int(), "Suggestions per issue, 0 to 10. 3 by default."),
                    "terms": with_desc(boolean(), "List every constant IRI of the query."),
                })),
            ),
            "The arguments of `check_query` without `dataset`.",
            "checking-and-explaining-queries",
        ),
    );
    put(
        "CheckResult",
        doc(
            obj(
                &["dataset", "commit", "ok", "issues", "prefixes"],
                json!({
                    "dataset": string(),
                    "commit": int(),
                    "ok": with_desc(boolean(), "False when an issue is an error."),
                    "issues": array(issue.clone()),
                    "estimatedRows": num(),
                    "terms": array(obj(&["term", "iri", "kind", "occurs"], json!({
                        "term": with_desc(string(), "The IRI, compacted with `prefixes`."),
                        "iri": string(),
                        "kind": string_enum(&["class", "property", "entity"]),
                        "label": string(),
                        "count": with_desc(int(), "Instances of a class, or triples of a property, in the caller's view."),
                        "types": strings(),
                        "occurs": json!({ "type": ["boolean", "null"], "description": "Whether the term occurs in the caller's view. Null when the check did not look it up." }),
                    }))),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "The issues of a query, and with `terms` the terms it uses.",
            "checking-and-explaining-queries",
        ),
    );
    put(
        "DiagnoseRequest",
        doc(
            closed(
                &["query"],
                with_read_args(json!({
                    "query": with_desc(string(), "The query that returned no rows."),
                })),
            ),
            "The arguments of `why_empty` without `dataset`.",
            "checking-and-explaining-queries",
        ),
    );
    let step = obj(
        &["kind", "text", "solutions"],
        json!({
            "kind": string_enum(&["pattern", "join", "filter"]),
            "text": string(),
            "solutions": json!({ "type": ["boolean", "null"], "description": "Null when the check ran out of time." }),
        }),
    );
    put(
        "Diagnosis",
        doc(
            obj(
                &[
                    "dataset", "commit", "empty", "steps", "complete", "message", "prefixes",
                ],
                json!({
                    "dataset": string(),
                    "commit": int(),
                    "empty": json!({ "type": ["boolean", "null"], "description": "Whether the query has no solutions. Null when the check ran out of time." }),
                    "first": obj(&["kind", "text", "constants", "issues"], json!({
                        "kind": string_enum(&["pattern", "join", "filter"]),
                        "text": string(),
                        "line": int(),
                        "column": int(),
                        "constants": array(obj(&["term", "occurs"], json!({ "term": string(), "occurs": boolean() }))),
                        "issues": array(issue),
                    })),
                    "steps": array(step),
                    "verdict": with_desc(string_enum(&["query", "data", "unknown"]), "For an empty result: `query` when a check issue explains the first element without solutions, `data` when the query is well formed for the data and the data holds no match, `unknown` otherwise."),
                    "unchecked": with_desc(strings(), "Parts of the query the diagnosis does not cut, such as MINUS."),
                    "complete": with_desc(boolean(), "Whether every check finished and every part was checked."),
                    "message": string(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "Why a query has no solutions: the first pattern, join or filter without any.",
            "checking-and-explaining-queries",
        ),
    );
    put(
        "RecallRequest",
        doc(
            closed(
                &[],
                with_read_args(json!({
                    "query": with_desc(string(), "The question or the words to search for. Give query, seeds or both."),
                    "seeds": with_desc(strings(), "Entity IRIs to start from."),
                    "types": with_desc(strings(), "Class IRIs that seeds found by search must have."),
                    "graphs": with_desc(strings(), "Graph IRIs to read, or `default`."),
                    "hops": int(),
                    "seedLimit": int(),
                    "maxTriples": int(),
                    "maxBytes": int(),
                    "includeSuperseded": boolean(),
                    "statuses": array(string_enum(&["reviewed", "unreviewed", "proposed"])),
                    "unreviewedWeight": num(),
                    "recency": with_desc(string(), "A half-life such as `90d` (units s, m, h, d, w, y). Found seeds rank by the age of their newest fact and by how many graphs assert their facts."),
                    "format": with_desc(string_enum(&["json"]), "Always `json` here."),
                })),
            ),
            "The arguments of `recall` without `dataset`.",
            "checking-and-explaining-queries",
        ),
    );
    let status = string_enum(&["reviewed", "unreviewed"]);
    put(
        "RecallResult",
        doc(
            obj(
                &[
                    "dataset",
                    "commit",
                    "entities",
                    "citations",
                    "conflicts",
                    "truncated",
                    "prefixes",
                ],
                json!({
                    "dataset": string(),
                    "commit": int(),
                    "entities": array(obj(&["iri", "types", "hop", "facts"], json!({
                        "iri": string(),
                        "label": string(),
                        "types": strings(),
                        "seed": int(),
                        "hop": int(),
                        "facts": array(obj(&["s", "p", "o", "citation"], json!({
                            "s": string(), "p": string(), "o": string(), "citation": int(),
                            "status": status.clone(),
                        }))),
                    }))),
                    "citations": array(obj(&["id", "graph"], json!({
                        "id": int(), "graph": string(), "reifier": string(), "source": string(),
                        "at": string(), "by": string(), "confidence": string(), "quote": string(),
                        "status": status,
                    }))),
                    "superseded": array(obj(&["s", "p", "o", "graph", "reifier"], json!({
                        "s": string(), "p": string(), "o": string(), "graph": string(), "reifier": string(),
                        "at": string(), "invalidatedAt": string(), "replacedBy": strings(),
                    }))),
                    "conflicts": array(obj(&["s", "p", "values"], json!({
                        "s": string(), "p": string(),
                        "values": array(obj(&["o", "citation"], json!({ "o": string(), "citation": int() }))),
                    }))),
                    "truncated": boolean(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "The facts around the seeds, with a citation for each.",
            "checking-and-explaining-queries",
        ),
    );
}

/// The maintenance tasks of agent memory (C18 Phase 5).
fn maintenance(put: &mut dyn FnMut(&str, J)) {
    put(
        "ConsolidateRequest",
        doc(
            closed(
                &[],
                json!({
                    "mode": with_desc(string_enum(&["branch", "auto"]), "`branch` (the default, or the dataset's `consolidation.mode`) leaves the proposals on a review branch. `auto` merges them when every fact passes and needs `admin`."),
                    "minSources": with_desc(int(), "The distinct sources that must assert a fact, 2 to 100 (default: the dataset's, else 2)."),
                    "dryRun": with_desc(boolean(), "Report the repeated facts, duplicates and conflicts, and write nothing."),
                    "message": with_desc(string(), "The commit message of the proposals."),
                    "deadlineSeconds": with_desc(num(), "The task's deadline, 1 to 86400 seconds (3600 by default)."),
                }),
            ),
            "A consolidation pass. An empty body uses the dataset's settings.",
            "memory-maintenance",
        ),
    );
    put(
        "RetentionRequest",
        doc(
            closed(
                &[],
                json!({
                    "after": with_desc(string(), "The age after which a session graph is deleted, such as `365d` (default: the dataset's `retention.after`)."),
                    "graphs": with_desc(strings(), "IRI patterns of the session graphs (default: the dataset's)."),
                    "requireConsolidated": with_desc(boolean(), "Delete only graphs whose facts a reviewed graph asserts too (default: the dataset's, else true)."),
                    "dryRun": with_desc(boolean(), "List what would be deleted, and delete nothing."),
                    "deadlineSeconds": with_desc(num(), "The task's deadline, 1 to 86400 seconds (3600 by default)."),
                }),
            ),
            "A retention pass. An empty body applies the dataset's `retention`.",
            "memory-maintenance",
        ),
    );
    let entry = obj(
        &["settings"],
        json!({
            "settings": any_object("The dataset's `consolidation` or `retention` member, or null."),
            "lastRun": with_desc(string(), "When the last scheduled task started."),
            "lastTask": with_desc(string(), "Its id, readable at `/$/ingest/{ds}/{task}`."),
            "nextRun": with_desc(string(), "When the next scheduled task starts, or `due`. Absent without a schedule."),
        }),
    );
    put(
        "MaintenanceStatus",
        doc(
            obj(
                &["dataset", "consolidation", "retention"],
                json!({ "dataset": string(), "consolidation": entry.clone(), "retention": entry }),
            ),
            "The schedules of consolidation and retention.",
            "memory-maintenance",
        ),
    );
}

fn memory(put: &mut dyn FnMut(&str, J)) {
    settings(put);
    server_settings(put);
    put(
        "MemorySettings",
        doc(
            closed(
                &[],
                json!({
                    "agentGraphs": with_desc(strings(), "Graph IRIs or `*` patterns of agent memory. Facts asserted only there are unreviewed."),
                    "consolidatedGraph": with_desc(string(), "The graph that promoted facts are written to."),
                    "agents": { "type": "object", "description": "Per-agent policies by agent name.", "additionalProperties": closed(&[], json!({
                        "conversationFacts": string_enum(&["immediate", "review"]),
                    })) },
                    "imports": with_desc(closed(&["base"], json!({
                        "base": with_desc(string(), "The prefix of every import graph, an IRI that ends in `/` or `#`. `agentGraphs` must cover it."),
                        "secretPatterns": array(closed(&["name", "regex"], json!({ "name": string(), "regex": string() }))),
                        "transcripts": with_desc(boolean(), "Whether transcripts may be imported (Phase 3m-b)."),
                        "extract": with_desc(string_enum(&["agent", "server", "none"]), "Who extracts facts from imported prose."),
                    })), "The imports of coding agents' memory files."),
                    "consolidation": with_desc(closed(&[], json!({
                        "every": with_desc(string(), "How often the server runs a pass, such as `1d` (at least `1h`). Without it a pass runs only on request."),
                        "mode": string_enum(&["branch", "auto"]),
                        "minSources": with_desc(int(), "The distinct sources that must assert a fact, 2 to 100, 2 by default."),
                    })), "The consolidation pass. Needs `agentGraphs` and `consolidatedGraph`."),
                    "retention": with_desc(closed(&["after"], json!({
                        "after": with_desc(string(), "The age of a session graph's newest fact after which the graph is deleted, such as `365d` (at least `1d`)."),
                        "graphs": with_desc(strings(), "IRI patterns with `*` of the session graphs. By default, the agent graphs whose IRI holds `/sessions/`."),
                        "requireConsolidated": with_desc(boolean(), "Delete only graphs whose facts a reviewed graph asserts too (true by default)."),
                        "every": with_desc(string(), "How often the server applies it, `1d` by default."),
                    })), "The retention of session graphs, off without it."),
                }),
            ),
            "The memory settings of a dataset.",
            "memory-settings",
        ),
    );
    let suggestion = obj(
        &["id", "question", "query", "by", "at"],
        json!({
            "id": string(),
            "question": string(),
            "query": string(),
            "explanation": string(),
            "by": with_desc(string(), "The principal who suggested it."),
            "at": { "type": "string", "format": "date-time" },
        }),
    );
    put(
        "Suggestion",
        doc(
            suggestion.clone(),
            "A suggested example.",
            "suggested-examples",
        ),
    );
    put(
        "SuggestionList",
        doc(
            obj(
                &["dataset", "suggestions"],
                json!({ "dataset": string(), "suggestions": array(suggestion) }),
            ),
            "The suggested examples, newest first.",
            "suggested-examples",
        ),
    );
    put(
        "SuggestRequest",
        doc(
            closed(
                &["question", "query"],
                json!({
                    "question": with_desc(string(), "At most 2000 characters."),
                    "query": with_desc(string(), "A SPARQL query of at most 65,536 characters."),
                    "explanation": with_desc(string(), "At most 400 characters."),
                }),
            ),
            "A question and the query that answers it.",
            "suggested-examples",
        ),
    );
    imports(put);
    review(put);
}

/// The review inbox, branch review, reviewer actions and ingest profiles (C18 Phase 3).
fn review(put: &mut dyn FnMut(&str, J)) {
    let fact_ref = closed(
        &["s", "p", "o", "graph"],
        json!({
            "s": string(), "p": string(),
            "o": with_desc(string(), "The object in N-Triples form, as the inbox lists it."),
            "graph": string(),
        }),
    );
    let signal = string_enum(&["pass", "fail", "none", "unchecked"]);
    let fact = obj(
        &["s", "p", "o", "graph", "status", "reifiers"],
        json!({
            "s": string(), "p": string(), "o": string(), "graph": string(),
            "shown": any_object("The terms in compact form."),
            "sLabel": string(), "oLabel": string(),
            "status": string_enum(&["unreviewed", "proposed", "reviewed"]),
            "reifiers": strings(),
            "time": string(), "confidence": string(), "quote": string(),
            "by": string(), "agent": string(),
            "span": obj(&["rendition", "start", "end"], json!({ "rendition": string(), "start": int(), "end": int() })),
            "signals": obj(&["span", "link", "guard", "corroboration"], json!({
                "span": signal.clone(), "link": signal.clone(), "guard": signal.clone(), "corroboration": signal,
            })),
            "passes": with_desc(boolean(), "Whether the span, link and guard signals pass."),
            "candidates": array(obj(&["iri"], json!({ "iri": string(), "shown": string(), "label": string() }))),
            "notes": strings(),
        }),
    );
    let branch = obj(
        &["name", "kind", "ahead", "behind"],
        json!({
            "name": string(),
            "kind": string_enum(&["ingest", "review", "inbox", "consolidation", "proposal"]),
            "ahead": int(), "behind": int(),
            "created": string(), "modified": string(), "note": string(), "creator": string(),
            "facts": with_desc(int(), "The facts the branch proposes."),
            "retracts": with_desc(int(), "The facts of main the branch retracts."),
        }),
    );
    put(
        "MemoryInbox",
        doc(
            obj(
                &[
                    "dataset",
                    "commit",
                    "sessions",
                    "branches",
                    "open",
                    "truncated",
                ],
                json!({
                    "dataset": string(), "commit": int(),
                    "agentGraphs": strings(),
                    "target": with_desc(string(), "The consolidated graph that promotions write to by default."),
                    "sessions": array(obj(&["graph", "facts"], json!({
                        "graph": string(), "shown": string(), "by": strings(),
                        "first": string(), "last": string(),
                        "facts": array(fact.clone()),
                    }))),
                    "branches": array(branch),
                    "open": with_desc(int(), "Unreviewed facts plus open review branches."),
                    "truncated": boolean(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "Everything that waits for a person: unreviewed session facts with their signals, by session, and the open review branches.",
            "review-inbox",
        ),
    );
    put(
        "BranchReview",
        doc(
            obj(
                &[
                    "dataset", "branch", "facts", "retracts", "entities", "sources",
                ],
                json!({
                    "dataset": string(), "branch": string(), "kind": string(),
                    "base": int(), "head": int(), "ahead": int(), "behind": int(),
                    "note": string(), "creator": string(),
                    "facts": array(fact.clone()),
                    "retracts": array(fact),
                    "rejected": with_desc(int(), "Facts made and retracted on the branch."),
                    "entities": array(obj(&["iri", "types", "candidates"], json!({
                        "iri": string(), "shown": string(), "label": string(), "types": strings(),
                        "candidates": array(obj(&["iri"], json!({ "iri": string(), "shown": string(), "label": string() }))),
                    }))),
                    "sources": array(obj(&["rendition", "length"], json!({
                        "rendition": string(), "source": string(), "title": string(), "format": string(),
                        "length": int(), "text": string(), "textOmitted": boolean(),
                        "pages": array(obj(&["page", "start"], json!({ "page": int(), "start": int() }))),
                        "ocrPages": array(int()),
                        "omittedPages": array(int()),
                    }))),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "What a review branch proposes and retracts, its new entities and the text of the sources it cites.",
            "review-inbox",
        ),
    );
    put(
        "PromoteRequest",
        doc(
            closed(
                &["facts"],
                json!({
                    "facts": array(fact_ref.clone()),
                    "target": with_desc(string(), "The graph to promote into (default: `consolidatedGraph`)."),
                    "branch": with_desc(string(), "An open review branch to add the facts to (default: a new `review.{person}.{date}-{n}`)."),
                    "message": string(),
                    "timeoutSeconds": num(),
                }),
            ),
            "The facts to promote, at most 500.",
            "review-inbox",
        ),
    );
    put(
        "PromoteResult",
        doc(
            obj(
                &["dataset", "branch", "target", "promoted", "committed"],
                json!({
                    "dataset": string(), "branch": string(), "target": string(),
                    "promoted": int(), "commit": int(), "committed": boolean(),
                }),
            ),
            "The review branch that holds the promoted facts, for the merge page.",
            "review-inbox",
        ),
    );
    put(
        "RejectRequest",
        doc(
            closed(
                &["facts"],
                json!({
                    "facts": array(fact_ref.clone()),
                    "branch": with_desc(string(), "The review branch the facts are on (default: main)."),
                    "reason": with_desc(string(), "Added to the commit message, at most 500 characters."),
                    "timeoutSeconds": num(),
                }),
            ),
            "The facts to retract, at most 500.",
            "review-inbox",
        ),
    );
    put(
        "RejectResult",
        doc(
            obj(
                &["dataset", "branch", "rejected", "commits"],
                json!({
                    "dataset": string(), "branch": string(), "rejected": int(),
                    "commits": array(any_object("The result of `assert_facts` for one graph.")),
                }),
            ),
            "The retractions, one commit per graph.",
            "review-inbox",
        ),
    );
    put(
        "RelinkRequest",
        doc(
            closed(
                &["branch", "from", "to"],
                json!({
                    "branch": string(),
                    "from": with_desc(string(), "The new entity's IRI."),
                    "to": with_desc(string(), "The existing entity's IRI."),
                    "timeoutSeconds": num(),
                }),
            ),
            "**Use existing**: name an existing entity instead of a new one on a review branch.",
            "review-inbox",
        ),
    );
    put(
        "EditFactRequest",
        doc(
            closed(
                &["fact", "o"],
                json!({
                    "fact": fact_ref,
                    "o": with_desc(string(), "The new object, as `assert_facts` takes it."),
                    "branch": with_desc(string(), "The review branch the fact is on (default: main)."),
                    "timeoutSeconds": num(),
                }),
            ),
            "**Edit value**: replace a fact's object, with the new reifier derived from the old one.",
            "review-inbox",
        ),
    );
    put(
        "MemoryWriteResult",
        doc(
            obj(
                &["dataset"],
                json!({
                    "dataset": string(), "branch": string(), "committed": boolean(),
                    "commit": int(), "inserted": int(), "deleted": int(),
                }),
            ),
            "The commit of a reviewer's change.",
            "review-inbox",
        ),
    );
    let profile = closed(
        &[],
        json!({
            "classes": with_desc(strings(), "The classes new entities may have (default: the classes with instances or a declaration)."),
            "predicates": with_desc(strings(), "The predicates facts may use (default: those with triples or a declaration)."),
            "shapes": with_desc(string(), "Extra SHACL shapes in Turtle."),
            "labelPredicate": string(),
            "language": with_desc(string(), "A BCP 47 language tag for new labels."),
            "vocabulary": with_desc(string(), "A graph whose classes and properties are offered."),
        }),
    );
    put(
        "IngestProfile",
        doc(profile.clone(), "An ingest profile.", "ingest-profiles"),
    );
    put(
        "IngestProfiles",
        doc(
            obj(
                &["dataset", "keepText", "profiles"],
                json!({
                    "dataset": string(),
                    "keepText": with_desc(boolean(), "Whether sources keep their text as chunks."),
                    "profiles": { "type": "object", "additionalProperties": profile },
                }),
            ),
            "The ingest settings of a dataset.",
            "ingest-profiles",
        ),
    );
    put(
        "IngestSettingsRequest",
        doc(
            closed(
                &[],
                json!({
                    "keepText": boolean(),
                    "confirmTokens": with_desc(or_null(int()), "The estimate in tokens above which an ingestion waits for a confirmation (200000 by default)."),
                    "autoConfidence": with_desc(or_null(num()), "The confidence that `auto` mode needs of every fact (0.8 by default)."),
                }),
            ),
            "Whether sources keep their text, and the thresholds of ingestion. Members left out keep their value, and `null` restores a default.",
            "ingest-profiles",
        ),
    );
}

/// `POST /{ds}/facts` and `POST /{ds}/memory/brief` (C18 Phase 3m-a).
fn imports(put: &mut dyn FnMut(&str, J)) {
    let fact = closed(
        &["s", "p", "o"],
        json!({
            "s": string(), "p": string(),
            "o": with_desc(string(), "An IRI, a key of `entities`, or a literal in SPARQL syntax."),
            "mode": string_enum(&["add", "replace"]),
            "confidence": num(),
            "quote": with_desc(string(), "At most 1000 characters."),
            "span": with_desc(closed(&["rendition", "start", "end"], json!({
                "rendition": string(), "start": int(), "end": int(),
            })), "The passage of a registered source that supports the fact, in code points of its rendition."),
            "derivedFrom": with_desc(strings(), "Reifiers of existing facts this fact rests on, at most 20."),
        }),
    );
    let retract = json!({ "oneOf": [
        with_desc(string(), "A reifier IRI."),
        closed(&["s", "p", "o", "graph"], json!({ "s": string(), "p": string(), "o": string(), "graph": string() })),
    ]});
    put(
        "AssertFactsRequest",
        doc(
            closed(
                &[],
                json!({
                    "graph": with_desc(string(), "The named graph to write (default: `source.iri`)."),
                    "source": closed(&["iri"], json!({ "iri": string(), "title": string() })),
                    "entities": array(closed(&["key", "label", "types"], json!({
                        "key": string(), "label": string(), "types": strings(),
                        "altLabels": strings(), "distinctFrom": strings(),
                    }))),
                    "facts": array(fact),
                    "retract": array(retract),
                    "replaceScope": string_enum(&["graph", "writable"]),
                    "message": string(),
                    "idempotencyKey": with_desc(string(), "At most 128 characters. A retry with the same key writes nothing and answers `alreadyApplied`."),
                    "agent": closed(&["name"], json!({ "name": string(), "model": string() })),
                    "iriBase": string(),
                    "allowUnknownIris": boolean(),
                    "dryRun": boolean(),
                    "changes": int(),
                    "ifHead": int(),
                    "timeoutSeconds": num(),
                    "branch": with_desc(string(), "Write on this branch instead of main."),
                    "retractStale": with_desc(string(), "A rendition of `register_source`. The facts of the graph that cite only earlier renditions of its source, and that this call does not assert again, are retracted."),
                }),
            ),
            "The arguments of `assert_facts` without `dataset`.",
            "importing-agent-memory",
        ),
    );
    let change = obj(
        &["triple", "graph"],
        json!({ "triple": string(), "graph": string(), "reason": string() }),
    );
    put(
        "AssertFactsResult",
        doc(
            obj(
                &["dataset", "graph", "committed", "head"],
                json!({
                    "dataset": string(), "branch": string(), "graph": string(),
                    "committed": boolean(), "wouldCommit": boolean(),
                    "commit": int(), "head": int(), "alreadyApplied": boolean(),
                    "activity": string(),
                    "minted": { "type": "object", "additionalProperties": string() },
                    "inserted": int(), "deleted": int(),
                    "superseded": array(change.clone()), "retracted": array(change.clone()),
                    "conflicts": array(change),
                    "warnings": array(any_object("A problem the write did not refuse.")),
                    "validation": any_object("The guard's report."),
                    "dryRun": any_object("A dry run's preview."),
                    "elapsedMs": num(),
                    "notice": with_desc(string(), "Set when the review policy wrote the facts to the agent's inbox branch."),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "What `assert_facts` wrote, or would write in a dry run.",
            "importing-agent-memory",
        ),
    );
    let chunk = obj(
        &["iri", "index", "start", "end"],
        json!({ "iri": string(), "index": int(), "start": int(), "end": int(), "text": string() }),
    );
    put(
        "RegisterSourceRequest",
        doc(
            closed(
                &["text"],
                json!({
                    "graph": with_desc(string(), "The named graph of the source and its facts (default: the source's IRI)."),
                    "iri": with_desc(string(), "The source's IRI (default: a `urn:uuid` minted from the text)."),
                    "title": string(),
                    "format": with_desc(string(), "The media type of the original document (default `text/plain`)."),
                    "text": with_desc(string(), "The document as text or Markdown, at most 2 MiB."),
                    "profile": string(),
                    "message": string(),
                    "dryRun": boolean(),
                    "original": with_desc(string(), "The file's bytes in base64 when the text is their normalized form. They are kept so an export can write the file back unchanged, and their SHA-256 becomes the digest."),
                    "reanchor": with_desc(boolean(), "Give each fact that cites the previous rendition a span in the new text where its quote occurs once, and retract the others."),
                    "reanchorFrom": with_desc(string(), "An earlier source whose facts are copied into this source's graph where their quotes occur in this text."),
                    "timeoutSeconds": num(),
                }),
            ),
            "The arguments of `register_source` without `dataset`.",
            "importing-agent-memory",
        ),
    );
    put(
        "RegisterSourceResult",
        doc(
            obj(
                &[
                    "dataset",
                    "graph",
                    "source",
                    "rendition",
                    "digest",
                    "length",
                    "alreadyRegistered",
                    "committed",
                    "chunks",
                ],
                json!({
                    "dataset": string(), "branch": string(), "graph": string(),
                    "source": string(), "rendition": string(), "digest": string(),
                    "length": int(), "alreadyRegistered": boolean(), "committed": boolean(),
                    "commit": int(), "head": int(), "textKept": boolean(), "profile": string(),
                    "previousRendition": string(), "staleFacts": int(), "originalKept": boolean(),
                    "reanchored": int(), "retracted": int(), "copied": int(),
                    "chunks": array(chunk),
                    "elapsedMs": num(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "What `register_source` stored.",
            "importing-agent-memory",
        ),
    );
    put(
        "SourceList",
        doc(
            obj(
                &["dataset", "commit", "sources", "truncated"],
                json!({
                    "dataset": string(), "branch": string(), "commit": int(),
                    "sources": array(obj(
                        &["source", "graph", "rendition", "chunks", "facts"],
                        json!({
                            "source": string(), "graph": string(), "title": string(),
                            "format": string(), "digest": string(), "rendition": string(),
                            "length": int(), "chunks": int(), "facts": int(),
                            "lastIngestion": string(), "needsExtraction": boolean(),
                            "invalidatedAt": string(),
                        }),
                    )),
                    "truncated": boolean(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "The sources `list_sources` found.",
            "importing-agent-memory",
        ),
    );
    put(
        "BriefRequest",
        doc(
            closed(
                &[],
                json!({
                    "scope": string_enum(&["project", "entity", "session"]),
                    "projectKey": with_desc(string(), "The project's key, such as `github.com/acme/shop`, with scope project."),
                    "entity": with_desc(string(), "An IRI, or a label that must link exactly, with scope entity."),
                    "query": with_desc(string(), "The words to recall, with scope session."),
                    "includeUnreviewed": boolean(),
                    "maxChars": with_desc(int(), "500 to 100,000, 8000 by default."),
                    "maxFacts": with_desc(int(), "1 to 500, 60 by default."),
                    "halfLifeDays": with_desc(num(), "The age at which a fact's weight halves, 90 by default."),
                    "unreviewedWeight": with_desc(num(), "0 to 1, 0.7 by default."),
                    "timeoutSeconds": num(),
                }),
            ),
            "What to brief.",
            "importing-agent-memory",
        ),
    );
    put(
        "BriefResult",
        doc(
            obj(
                &[
                    "dataset",
                    "commit",
                    "scope",
                    "reviewedOnly",
                    "matched",
                    "shown",
                    "text",
                ],
                json!({
                    "dataset": string(), "commit": int(), "scope": string(),
                    "reviewedOnly": boolean(),
                    "imports": with_desc(boolean(), "Whether the dataset has an import base."),
                    "matched": int(), "shown": int(),
                    "text": with_desc(string(), "The brief as plain text. Its first line says that the lines are recalled data."),
                    "facts": array(any_object("One fact with its citations.")),
                    "citations": array(any_object("One source of the facts.")),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "The brief of a project, an entity or a session.",
            "importing-agent-memory",
        ),
    );
}

fn models(put: &mut dyn FnMut(&str, J)) {
    let level = string_enum(&["auto", "json-schema", "json-object", "tool", "text"]);
    let pair = closed(
        &["provider", "model"],
        json!({
            "provider": string(),
            "model": string(),
            "maxOutputTokens": int(),
            "requestTimeoutSecs": num(),
        }),
    );
    let model = obj(
        &[
            "name",
            "contextTokens",
            "maxOutputTokens",
            "structuredOutput",
            "requestTimeoutSecs",
            "status",
        ],
        json!({
            "name": string(),
            "contextTokens": int(),
            "maxOutputTokens": int(),
            "structuredOutput": with_desc(level.clone(), "The configured level. `auto` detects it."),
            "requestTimeoutSecs": num(),
            "detected": or_null(with_desc(level.clone(), "The level detected or configured for the pair, once known.")),
            "pricing": obj(&["inputPerMTok", "outputPerMTok"], json!({ "inputPerMTok": num(), "outputPerMTok": num() })),
            "status": obj(&["state"], json!({
                "state": string_enum(&["untested", "ok", "failing"]),
                "at": { "type": "string", "format": "date-time" },
                "message": string(),
            })),
        }),
    );
    let provider = obj(
        &[
            "name",
            "kind",
            "endpoint",
            "status",
            "concurrency",
            "models",
        ],
        json!({
            "name": string(),
            "kind": string_enum(&["ollama", "openai", "anthropic"]),
            "endpoint": string(),
            "status": with_desc(string_enum(&["ok", "secret-missing"]), "`secret-missing` when the named key cannot be read."),
            "concurrency": int(),
            "models": array(model),
            "apiKey": with_desc(obj(&["secret", "source"], json!({ "secret": string(), "source": with_desc(string_enum(&["declared", "runtime", "missing"]), "Where the key comes from: `--model-secret`, a value stored through `PUT /$/server/secrets/{name}`, or nowhere.") })), "The name of the secret that holds the key and its source. The key itself is never returned."),
            "allowedModels": strings(),
            "requestsPerMinute": int(),
            "budget": obj(&["tokensPerDay", "usedToday"], json!({ "tokensPerDay": int(), "usedToday": int() })),
        }),
    );
    let mut roles = Map::new();
    for r in [
        "draft",
        "repair",
        "summarize",
        "extract",
        "explain",
        "optimize",
    ] {
        roles.insert(r.into(), array(pair.clone()));
    }
    put(
        "ModelProviders",
        doc(
            obj(
                &["configured", "providers", "roles"],
                json!({
                    "configured": with_desc(boolean(), "Whether the server runs with `--model-config`."),
                    "providers": array(provider),
                    "roles": { "type": "object", "properties": J::Object(roles) },
                    "routing": any_object("The routing settings of the configuration."),
                }),
            ),
            "The model providers, their models and the role lists.",
            "model-providers",
        ),
    );
    put(
        "ModelTestRequest",
        doc(
            closed(
                &[],
                json!({
                    "model": with_desc(string(), "The model to test. By default the first one the role lists name."),
                    "timeoutSeconds": with_desc(num(), "At most 600. 60 by default."),
                }),
            ),
            "A test call to one provider and model.",
            "model-providers",
        ),
    );
    put(
        "ModelTestResult",
        doc(
            obj(
                &[
                    "provider",
                    "model",
                    "ok",
                    "structuredOutput",
                    "latencyMs",
                    "inputTokens",
                    "outputTokens",
                    "requests",
                ],
                json!({
                    "provider": string(),
                    "model": string(),
                    "ok": boolean(),
                    "level": or_null(level),
                    "structuredOutput": with_desc(boolean(), "Whether the answer came at a structured level."),
                    "latencyMs": int(),
                    "inputTokens": int(),
                    "outputTokens": int(),
                    "requests": int(),
                    "error": obj(&["code", "message"], json!({ "code": string(), "message": string() })),
                }),
            ),
            "The outcome of a test call.",
            "model-providers",
        ),
    );
}

/// The layered settings of spec C19.
fn settings(put: &mut dyn FnMut(&str, J)) {
    let source = string_enum(&["default", "declared", "runtime", "locked"]);
    put(
        "SettingsKind",
        doc(
            obj(
                &[
                    "dataset",
                    "kind",
                    "effective",
                    "declared",
                    "runtime",
                    "sources",
                    "locked",
                    "overridden",
                    "overrides",
                    "status",
                    "etag",
                ],
                json!({
                    "dataset": string(),
                    "kind": string_enum(&["assistant", "memory", "ingest"]),
                    "effective": any_object("The effective object: the built-in defaults, the declared values and the runtime layer merged, with the locked fields from the settings file."),
                    "declared": any_object("The settings file's `defaults` and dataset entry for this kind, merged."),
                    "runtime": any_object("The runtime layer: the fields changed through the API. `null` removes a declared member."),
                    "sources": { "type": "object", "description": "The source of each field of `effective`, by dotted path such as `budget.perRequest`.", "additionalProperties": source },
                    "locked": with_desc(strings(), "The fields the settings file locks."),
                    "overridden": with_desc(strings(), "Locked fields whose runtime value is kept in the file but ignored."),
                    "overrides": with_desc(array(sref("SettingsOverride")), "The runtime values used in place of a different value of the settings file, one entry per field. Locked fields are left out, since their runtime value is ignored."),
                    "status": closed(&["valid"], json!({ "valid": boolean(), "error": string() })),
                    "etag": with_desc(string(), "The entity tag of the runtime layer, as in the `ETag` header."),
                }),
            ),
            "One settings kind of a dataset with its layers and sources.",
            "settings",
        ),
    );
    put(
        "SettingsOverride",
        doc(
            closed(
                &["path", "declared", "runtime"],
                json!({
                    "path": with_desc(string(), "The field, as a dotted path. It is the runtime leaf, or the field above it that the declared layers set to something other than an object."),
                    "declared": with_desc(json!({}), "The value of the declared layers that the runtime value replaces."),
                    "runtime": with_desc(json!({}), "The runtime value in use, or `null` for a declared provider removed at runtime."),
                }),
            ),
            "A runtime value that is used in place of a different declared value. A reset of `path` brings the declared value back.",
            "settings",
        ),
    );
    put(
        "DatasetSettings",
        doc(
            obj(
                &["dataset", "kinds"],
                json!({
                    "dataset": string(),
                    "kinds": { "type": "object", "additionalProperties": sref("SettingsKind") },
                }),
            ),
            "Every settings kind of a dataset.",
            "settings",
        ),
    );
    let file = obj(
        &["path", "readAt", "error", "errorAt"],
        json!({
            "path": or_null(string()),
            "readAt": or_null(string()),
            "error": with_desc(or_null(string()), "Why the last reload failed, while the previous file stays in use."),
            "errorAt": or_null(string()),
        }),
    );
    let mut status = file.clone();
    status["required"] = json!([
        "path",
        "readAt",
        "error",
        "errorAt",
        "declared",
        "unmatched",
        "kinds",
        "serverKinds",
        "models"
    ]);
    status["properties"]["declared"] =
        with_desc(strings(), "The dataset names of the settings file.");
    status["properties"]["unmatched"] = with_desc(
        strings(),
        "Declared names that match no dataset. Their entries apply when such a dataset is created.",
    );
    status["properties"]["kinds"] = strings();
    status["properties"]["serverKinds"] = with_desc(
        strings(),
        "The server-wide kinds of `/$/server/settings/{kind}`.",
    );
    status["properties"]["models"] = with_desc(
        file,
        "The model configuration of `--model-config`, which SIGHUP reads again too.",
    );
    put(
        "SettingsStatus",
        doc(
            status,
            "The settings file of `serve --settings` and its reads.",
            "settings",
        ),
    );
}

/// The server-wide settings and runtime secrets of spec C19 §11.
fn server_settings(put: &mut dyn FnMut(&str, J)) {
    let source = string_enum(&["default", "declared", "runtime", "locked"]);
    put(
        "ServerSettingsKind",
        doc(
            obj(
                &[
                    "scope",
                    "kind",
                    "effective",
                    "declared",
                    "runtime",
                    "sources",
                    "locked",
                    "overridden",
                    "overrides",
                    "status",
                    "etag",
                ],
                json!({
                    "scope": string_enum(&["server"]),
                    "kind": string_enum(&["models"]),
                    "effective": any_object("The effective model configuration: `providers`, `roles` and `routing`, from the built-in defaults, `--model-config` and the runtime layer, with the locked fields from the declared configuration."),
                    "declared": any_object("The model configuration of `--model-config`."),
                    "runtime": any_object("The runtime layer kept in `<dataDir>/models.json`. `null` for a provider removes a declared provider."),
                    "sources": { "type": "object", "description": "The source of each field of `effective`, by dotted path such as `providers.claude.endpoint`.", "additionalProperties": source },
                    "locked": with_desc(strings(), "The fields that `server.locked` of the settings file locks, without the `models.` prefix."),
                    "overridden": with_desc(strings(), "Locked fields whose runtime value is kept but ignored."),
                    "overrides": with_desc(array(sref("SettingsOverride")), "The runtime values used in place of a different value of `--model-config`, one entry per field, such as a changed budget or a removed provider. Locked fields are left out."),
                    "status": closed(&["valid"], json!({ "valid": boolean(), "error": string() })),
                    "etag": with_desc(string(), "The entity tag of the runtime layer, as in the `ETag` header."),
                }),
            ),
            "A server-wide settings kind with its layers and sources.",
            "server-settings",
        ),
    );
    put(
        "SecretList",
        doc(
            obj(
                &["secrets"],
                json!({
                    "secrets": array(obj(
                        &["name", "source", "declared", "locked", "setAt", "overridden", "providers"],
                        json!({
                            "name": string(),
                            "source": with_desc(string_enum(&["declared", "runtime", "missing"]), "The source in force: a stored value, `--model-secret`, or none."),
                            "declared": with_desc(boolean(), "Whether `--model-secret` names the secret."),
                            "locked": with_desc(boolean(), "Whether `server.locked` names `secrets.NAME`, so that only the declared source applies."),
                            "setAt": with_desc(or_null(string()), "When the runtime value was stored."),
                            "overridden": with_desc(boolean(), "A runtime value is stored but the lock ignores it."),
                            "providers": with_desc(strings(), "The providers whose `apiKey` names the secret."),
                            "channels": with_desc(strings(), "The notification channels that name the secret (spec C21)."),
                        }),
                    )),
                }),
            ),
            "The model secrets. A value is never returned.",
            "model-secrets",
        ),
    );
    let delivery = || {
        or_null(obj(
            &["at", "event"],
            json!({
                "at": string(),
                "event": string(),
                "id": string(),
                "status": with_desc(int(), "The HTTP status of a delivery."),
                "error": with_desc(string(), "Why a delivery failed. It never holds a secret."),
            }),
        ))
    };
    put(
        "NotificationStatus",
        doc(
            obj(
                &[
                    "enabled", "channels", "routes", "queued", "active", "recent",
                ],
                json!({
                    "enabled": boolean(),
                    "channels": array(obj(
                        &["name", "type", "target", "secrets", "lastSuccess", "lastFailure", "sent", "failed"],
                        json!({
                            "name": string(),
                            "type": string_enum(&["webhook", "ntfy"]),
                            "target": with_desc(string(), "Where the channel delivers, without credentials."),
                            "secrets": with_desc(strings(), "The secrets the channel names."),
                            "lastSuccess": delivery(),
                            "lastFailure": delivery(),
                            "sent": with_desc(int(), "Deliveries that succeeded since the start."),
                            "failed": with_desc(int(), "Deliveries that failed, were refused or were dropped since the start."),
                        }),
                    )),
                    "routes": any_object("The routes from event types to channels."),
                    "queued": with_desc(int(), "Deliveries waiting in the queue."),
                    "active": array(obj(
                        &["key", "event", "first", "last", "count"],
                        json!({
                            "key": string(),
                            "event": string(),
                            "dataset": or_null(string()),
                            "first": string(),
                            "last": string(),
                            "count": int(),
                        }),
                    )),
                    "recent": array(any_object("A delivery: `at`, `id`, `event`, `channel`, `result` (`ok`, `failed`, `refused` or `dropped`), `attempts`, and `status` or `error`.")),
                }),
            ),
            "Whether notifications are on, each channel's last success and failure, the conditions notified and the recent deliveries.",
            "notifications",
        ),
    );
    put(
        "NotificationTest",
        doc(
            obj(
                &["channel", "type", "result", "status", "id", "latencyMs"],
                json!({
                    "channel": string(),
                    "type": string_enum(&["webhook", "ntfy"]),
                    "result": string_enum(&["ok"]),
                    "status": with_desc(int(), "The HTTP status the channel answered."),
                    "id": with_desc(string(), "The envelope's id."),
                    "latencyMs": int(),
                }),
            ),
            "A test notification that the channel accepted.",
            "notifications",
        ),
    );
    put(
        "SecretValue",
        doc(
            obj(
                &["value"],
                json!({ "value": with_desc(string(), "The key. It is stored and never returned.") }),
            ),
            "The value of a model secret.",
            "model-secrets",
        ),
    );
}
