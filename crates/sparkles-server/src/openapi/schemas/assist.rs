//! The bodies of spec C18: model providers and their test calls.

use super::kit::*;
use super::*;

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    models(put);
    tools(put);
    memory(put);
    asking(put);
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
                    "imports": with_desc(closed(&["base"], json!({
                        "base": with_desc(string(), "The prefix of every import graph, an IRI that ends in `/` or `#`. `agentGraphs` must cover it."),
                        "secretPatterns": array(closed(&["name", "regex"], json!({ "name": string(), "regex": string() }))),
                        "transcripts": with_desc(boolean(), "Whether transcripts may be imported (Phase 3m-b)."),
                        "extract": with_desc(string_enum(&["agent", "server", "none"]), "Who extracts facts from imported prose."),
                    })), "The imports of coding agents' memory files."),
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
            closed(&["keepText"], json!({ "keepText": boolean() })),
            "Whether sources keep their text.",
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
