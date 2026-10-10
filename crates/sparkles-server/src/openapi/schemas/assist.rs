//! The bodies of spec C18: model providers and their test calls.

use super::kit::*;
use super::*;

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    models(put);
    tools(put);
    memory(put);
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
                    "statuses": array(string_enum(&["reviewed", "unreviewed"])),
                    "unreviewedWeight": num(),
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

fn memory(put: &mut dyn FnMut(&str, J)) {
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
            "apiKey": with_desc(obj(&["secret"], json!({ "secret": string() })), "The name of the secret that holds the key. The key itself is never returned."),
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
