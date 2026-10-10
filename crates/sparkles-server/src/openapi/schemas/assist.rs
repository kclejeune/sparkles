//! The bodies of spec C18: model providers and their test calls.

use super::kit::*;
use super::*;

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    models(put);
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
