//! The model configuration that `/$/server/settings/models` holds (spec C18 §3.4 and
//! spec C19 §11), member by member. The web UI builds the form of the Models section from
//! these schemas, so each member carries a `title` for its label and a `description`
//! for its help, with the bounds that the server checks. Five extensions say what JSON
//! Schema cannot:
//!
//! - `x-sparkles-order` lists an object's members in the order a form shows them.
//! - `x-sparkles-kinds` lists the provider kinds a member applies to, and the server
//!   refuses it for the others.
//! - `x-sparkles-secret` marks a string that names a secret, whose value is set apart.
//! - `x-sparkles-unset` says in words what an unset optional member means.
//! - `x-sparkles-section` names the heading a form shows the member under.
//!
//! A member can also carry `x-sparkles-https` (it applies to https endpoints only),
//! `x-sparkles-unit` (`seconds` or `tokens`), `x-sparkles-enum-kinds` (the kinds that
//! offer each value of an enum) and `x-sparkles-fallback` (the role list a role uses
//! while it is unset).

use super::kit::*;
use super::*;

/// The kinds that call a remote endpoint.
const REMOTE: &[&str] = &["ollama", "openai", "anthropic"];

fn titled(mut v: J, title: &str, description: &str) -> J {
    v["title"] = title.into();
    v["description"] = description.into();
    v
}

fn at_least(mut v: J, min: f64) -> J {
    v["minimum"] = json!(min);
    v
}

fn kinds(mut v: J, kinds: &[&str]) -> J {
    v["x-sparkles-kinds"] = json!(kinds);
    v
}

fn unset(mut v: J, words: &str) -> J {
    v["x-sparkles-unset"] = words.into();
    v
}

fn unit(mut v: J, unit: &str) -> J {
    v["x-sparkles-unit"] = unit.into();
    v
}

fn default(mut v: J, d: J) -> J {
    v["default"] = d;
    v
}

/// A positive number of seconds.
fn seconds(title: &str, description: &str) -> J {
    let mut v = unit(titled(num(), title, description), "seconds");
    v["exclusiveMinimum"] = json!(0);
    v
}

/// An object of these members in this order, none required, nothing else allowed.
fn ordered(order: &[&str], props: J) -> J {
    let mut o = closed(&[], props);
    o["x-sparkles-order"] = json!(order);
    o
}

/// A secret's name, as `--model-secret NAME=…` and `PUT /$/server/secrets/{name}` use it.
fn secret_name(title: &str, description: &str) -> J {
    let mut v = titled(string(), title, description);
    v["pattern"] = "^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$".into();
    v["x-sparkles-secret"] = true.into();
    v
}

/// The members of one model, which a provider also takes as its models' defaults.
fn model_options() -> (Vec<&'static str>, J) {
    let level = string_enum(&["auto", "json-schema", "json-object", "tool", "text"]);
    let mut level = titled(
        level,
        "Structured output",
        "How answers are held to a JSON Schema. `auto` tries json-schema, then json-object, then text, and keeps the first that works. The anthropic kind offers no json-object level and the ollama kind no tool level.",
    );
    level["x-sparkles-enum-kinds"] = json!({
        "json-object": ["ollama", "openai", "local"],
        "tool": ["openai", "anthropic", "local"],
    });
    let local = &["local"];
    let order = vec![
        "contextTokens",
        "maxOutputTokens",
        "temperature",
        "structuredOutput",
        "requestTimeoutSecs",
        "numCtx",
        "pricing",
        "repo",
        "revision",
        "path",
        "dtype",
        "threads",
        "idleUnloadSecs",
        "dimensions",
        "maxTokens",
        "queryPrefix",
        "documentPrefix",
    ];
    let props = json!({
        "contextTokens": default(unit(at_least(titled(int(), "Context tokens", "The tokens the model reads at most, prompt and answer together."), 1.0), "tokens"), json!(8192)),
        "maxOutputTokens": default(unit(at_least(titled(int(), "Output tokens", "The tokens of one answer at most."), 1.0), "tokens"), json!(2048)),
        "temperature": unset({
            let mut t = titled(num(), "Temperature", "Sampling temperature from 0 to 2. Other kinds send 0 when it is unset, and the anthropic kind sends nothing.");
            t["minimum"] = json!(0);
            t["maximum"] = json!(2);
            t
        }, "0, or none for anthropic"),
        "structuredOutput": default(level, json!("auto")),
        "requestTimeoutSecs": default(seconds("Request timeout", "How long one call may take."), json!(60)),
        "numCtx": kinds(unit(at_least(titled(int(), "Context (num_ctx)", "Ollama's context option, sent with each call."), 1.0), "tokens"), &["ollama"]),
        "pricing": {
            "type": "object",
            "title": "Pricing",
            "description": "Prices per million tokens, for cost estimates only.",
            "additionalProperties": false,
            "x-sparkles-order": ["inputPerMTok", "outputPerMTok"],
            "properties": {
                "inputPerMTok": default(at_least(titled(num(), "Input price", "The price of a million input tokens. A model's prices replace the provider's both at once."), 0.0), json!(0)),
                "outputPerMTok": default(at_least(titled(num(), "Output price", "The price of a million output tokens. A model's prices replace the provider's both at once."), 0.0), json!(0)),
            },
        },
        "repo": kinds(titled(string(), "Repository", "The Hugging Face repository of the model's snapshot."), local),
        "revision": kinds({
            let mut v = titled(string(), "Revision", "The snapshot's full 40-character commit.");
            v["pattern"] = "^[0-9a-f]{40}$".into();
            v
        }, local),
        "path": kinds(titled(string(), "Snapshot path", "An absolute snapshot directory on the server, in place of repo and revision."), local),
        "dtype": kinds(titled(string_enum(&["f32", "bf16"]), "Weights type", "The type the weights are computed in."), local),
        "threads": kinds({
            let mut v = titled(int(), "Threads", "The threads of the model's inference pool, from 1 to 256.");
            v["minimum"] = json!(1);
            v["maximum"] = json!(256);
            v
        }, local),
        "idleUnloadSecs": kinds(unit(at_least(titled(int(), "Unload after", "Seconds without work before the weights are dropped. 0 keeps them."), 0.0), "seconds"), local),
        "dimensions": kinds(at_least(titled(int(), "Dimensions", "Truncate the vectors to this many components."), 1.0), local),
        "maxTokens": kinds(unit(at_least(titled(int(), "Tokens per text", "The tokens of one text at most, up to the model's own limit."), 2.0), "tokens"), local),
        "queryPrefix": kinds({
            let mut v = titled(string(), "Query prefix", "Text put before queries, in place of the snapshot's query prompt.");
            v["maxLength"] = json!(4096);
            v
        }, local),
        "documentPrefix": kinds({
            let mut v = titled(string(), "Document prefix", "Text put before stored documents, in place of the snapshot's prompt.");
            v["maxLength"] = json!(4096);
            v
        }, local),
    });
    (order, props)
}

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    let (option_order, options) = model_options();
    put(
        "ModelOptions",
        doc(
            ordered(&option_order, options.clone()),
            "The options of one model of a provider. A member left out takes the provider's value, then the built-in default.",
            "server-settings",
        ),
    );

    let mut tls = ordered(
        &["caCert", "insecureSkipVerify"],
        json!({
            "caCert": {
                "type": "object",
                "title": "CA certificate",
                "description": "A PEM CA certificate or bundle trusted for this provider in addition to the system roots, named by a file or a secret. Set only one of the two.",
                "additionalProperties": false,
                "x-sparkles-order": ["file", "secret"],
                "properties": {
                    "file": titled(string(), "CA certificate file", "An absolute path on the server to a PEM CA certificate or bundle. Use it for an internal CA or a self-signed certificate."),
                    "secret": secret_name("CA certificate secret", "A secret that holds the PEM CA certificate, in place of a file."),
                },
            },
            "insecureSkipVerify": default(titled(boolean(), "Skip certificate verification", "Unsafe. The API key and the prompts sent to this provider can be read and changed by anyone on the network path. Prefer a CA certificate."), json!(false)),
        }),
    );
    tls["title"] = "TLS".into();
    tls["description"] = "How the provider's certificate is checked.".into();
    tls["x-sparkles-https"] = true.into();
    tls["x-sparkles-kinds"] = json!(REMOTE);

    let mut order = vec![
        "kind",
        "endpoint",
        "apiKey",
        "connectTimeoutSecs",
        "concurrency",
        "requestsPerMinute",
        "budget",
        "headers",
        "keepAlive",
        "version",
        "tls",
    ];
    order.extend(option_order.iter().copied());
    order.push("allowedModels");
    order.push("models");
    let mut props = json!({
        "kind": titled(string_enum(&["ollama", "openai", "anthropic", "local"]), "Protocol", "The protocol the provider speaks. The local kind runs embedding models in the server and takes no endpoint or key."),
        "endpoint": kinds({
            let mut v = titled(string(), "Endpoint", "The base URL, http or https, without credentials, a query or a fragment. A private address needs --outbound-allow-private.");
            v["format"] = "uri".into();
            v
        }, REMOTE),
        "apiKey": kinds({
            let mut v = closed(&["secret"], json!({
                "secret": secret_name("API key", "The secret that holds the key. The key itself is stored with Set key or under API keys and is never shown."),
            }));
            v["title"] = "API key".into();
            v["description"] = "The key, named by a secret and never written here.".into();
            unset(v, "no key")
        }, REMOTE),
        "connectTimeoutSecs": kinds(default(seconds("Connect timeout", "How long opening a connection may take."), json!(10)), REMOTE),
        "concurrency": default(at_least(titled(int(), "Concurrent calls", "The calls to this provider that may run at once."), 1.0), json!(4)),
        "requestsPerMinute": unset(at_least(titled(int(), "Requests per minute", "The calls per minute at most. 0 sets no limit."), 0.0), "no limit"),
        "budget": {
            "type": "object",
            "title": "Budget",
            "description": "Token caps of the provider.",
            "additionalProperties": false,
            "x-sparkles-order": ["tokensPerDay"],
            "properties": {
                "tokensPerDay": unset(unit(at_least(titled(int(), "Tokens per day", "Input and output tokens per day at most. 0 stops every call."), 0.0), "tokens"), "no cap"),
            },
        },
        "allowedModels": kinds(unset({
            let mut v = titled(strings(), "Allowed models", "The models that role lists may name with this provider. When it is unset, any model is allowed.");
            v["uniqueItems"] = true.into();
            v["items"]["minLength"] = json!(1);
            v
        }, "any model"), REMOTE),
        "headers": kinds({
            let mut v = titled(
                json!({ "type": "object", "additionalProperties": { "type": "string" } }),
                "Headers",
                "Extra request headers that are not secret. Headers that carry a key, such as Authorization, are refused.",
            );
            v["propertyNames"] = json!({ "pattern": "^[!#$%&'*+.^_`|~0-9A-Za-z-]+$" });
            v
        }, &["openai"]),
        "keepAlive": kinds(unset({
            let mut v = titled(string(), "Keep alive", "How long Ollama keeps the model loaded, as a duration such as 10m or 1h, or -1 to keep it.");
            v["pattern"] = "^(-1|-?[0-9]+|-?([0-9]+(\\.[0-9]+)?(ns|us|µs|ms|s|m|h))+)$".into();
            v
        }, "Ollama's default"), &["ollama"]),
        "version": kinds(default(titled(string(), "API version", "The anthropic-version header."), json!("2023-06-01")), &["anthropic"]),
        "tls": tls,
        "models": {
            "type": "object",
            "title": "Per-model options",
            "description": "Options by model name. A model's entry overrides the provider's defaults above for that model.",
            "additionalProperties": sref("ModelOptions"),
        },
    });
    for m in ["allowedModels", "models"] {
        props[m]["x-sparkles-section"] = "Models".into();
    }
    for (k, v) in options.as_object().unwrap() {
        let mut v = v.clone();
        v["x-sparkles-section"] = "Model defaults".into();
        props[k] = v;
    }
    let mut provider = obj(&["kind"], props);
    provider["additionalProperties"] = J::Bool(false);
    provider["x-sparkles-order"] = json!(order);
    put(
        "ModelProviderSettings",
        doc(
            provider,
            "One provider of the model configuration: its protocol, endpoint, key and limits, the defaults of its models (the members in the `Model defaults` section) and the options of each model.",
            "server-settings",
        ),
    );

    let pair = {
        let mut v = closed(
            &["provider", "model"],
            json!({
                "provider": titled(string(), "Provider", "A provider of the configuration, of a kind other than local."),
                "model": {
                    "type": "string",
                    "minLength": 1,
                    "title": "Model",
                    "description": "A model of the provider that its allowed models permit.",
                },
                "maxOutputTokens": unit(at_least(titled(int(), "Output tokens", "Caps the answer of this entry below the model's own cap."), 1.0), "tokens"),
                "requestTimeoutSecs": seconds("Request timeout", "The timeout of this entry, in place of the model's."),
            }),
        );
        v["x-sparkles-order"] =
            json!(["provider", "model", "maxOutputTokens", "requestTimeoutSecs"]);
        v
    };
    put(
        "ModelRolePair",
        doc(
            pair,
            "One entry of a role list: a provider and model, with optional caps.",
            "server-settings",
        ),
    );

    let roles: [(&str, &str); 7] = [
        (
            "draft",
            "The first draft of a query from a question. The first entry answers, and the server moves to the next only on the signals it verifies. The role is off when unset.",
        ),
        (
            "repair",
            "A new draft after a diagnosis. It uses the draft list when unset, and an empty list turns it off.",
        ),
        (
            "summarize",
            "The answer from the rows. The role is off when unset.",
        ),
        (
            "community",
            "The reports of GraphRAG communities. It uses the summarize list when unset, and an empty list turns it off.",
        ),
        (
            "extract",
            "Extraction from document chunks and the CSV mapping draft. The role is off when unset.",
        ),
        (
            "explain",
            "The prose of a query explanation. The role is off when unset.",
        ),
        (
            "optimize",
            "A proposed query rewrite. The role is off when unset.",
        ),
    ];
    let mut role_props = Map::new();
    for (r, d) in roles {
        let mut v = titled(array(sref("ModelRolePair")), r, d);
        if let Some(f) = match r {
            "repair" => Some("draft"),
            "community" => Some("summarize"),
            _ => None,
        } {
            v["x-sparkles-fallback"] = f.into();
        }
        role_props.insert(r.into(), v);
    }
    let mut role_obj = closed(&[], J::Object(role_props));
    role_obj["title"] = "Roles".into();
    role_obj["description"] =
        "The provider and model pairs each step calls, in order. A role list is one field.".into();
    role_obj["x-sparkles-order"] = json!(roles.map(|(r, _)| r));

    let routing = {
        let mut v = ordered(
            &["complexityThreshold", "exampleScore"],
            json!({
                "complexityThreshold": default(at_least(titled(int(), "Complexity threshold", "A query whose complexity score reaches it starts repair, or the next draft, at the second entry of its list. A dataset's assistant settings can override it."), 0.0), json!(8)),
                "exampleScore": {
                    "type": "number",
                    "minimum": 0,
                    "maximum": 1,
                    "title": "Example score",
                    "description": "The score from 0 to 1 that the best stored example needs before its complexity counts.",
                    "default": 0.8,
                },
            }),
        );
        v["title"] = "Routing".into();
        v["description"] = "When a question starts at the second entry of a role list.".into();
        v
    };

    let mut settings = ordered(
        &["providers", "roles", "routing"],
        json!({
            "providers": {
                "type": "object",
                "title": "Providers",
                "description": "The providers by name. A name has letters, digits, _, - and . only.",
                "propertyNames": { "pattern": "^[A-Za-z0-9_.-]+$" },
                "additionalProperties": sref("ModelProviderSettings"),
            },
            "roles": role_obj,
            "routing": routing,
        }),
    );
    settings["title"] = "Model configuration".into();
    put(
        "ModelsSettings",
        doc(
            settings,
            "The model configuration of `--model-config` and of the `models` kind of `/$/server/settings/models`: providers, role lists and routing. The web UI builds its form from this schema.",
            "server-settings",
        ),
    );
}
