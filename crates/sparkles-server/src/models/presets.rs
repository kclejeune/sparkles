//! Provider presets (spec C19 §11.5): templates for the providers whose endpoints are
//! well known. A preset is not a kind. It fills the members of a new provider of an
//! existing kind, and the caller may change any of them. `sparkles settings set --global
//! --preset NAME PROVIDER` uses this table, and the UI's Add provider form uses the same
//! table in `ui/src/lib/provider-presets.json`. A test checks that the two agree.

use serde_json::{Value, json};

/// One preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    /// the provider's kind
    pub kind: &'static str,
    pub endpoint: &'static str,
    /// the secret its `apiKey` names, if it needs a key
    pub secret: Option<&'static str>,
    /// a model to start with, as an entry of the provider's `models`
    pub model: &'static str,
    /// the options of that model's entry, as JSON: the settings the Ask comparison
    /// measured the model with, so that the preset reproduces them
    pub model_options: &'static str,
}

/// The presets. Generic OpenAI-compatible gateways have none: they use the `openai` kind
/// with their own endpoint.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "anthropic",
        kind: "anthropic",
        endpoint: "https://api.anthropic.com",
        secret: Some("anthropic"),
        model: "claude-sonnet-5-5",
        model_options: r#"{"contextTokens":200000,"maxOutputTokens":4096,"reasoningEffort":"medium"}"#,
    },
    Preset {
        name: "openai",
        kind: "openai",
        endpoint: "https://api.openai.com/v1",
        secret: Some("openai"),
        model: "gpt-6-luna",
        model_options: r#"{"contextTokens":200000,"maxOutputTokens":16384,"reasoningEffort":"medium"}"#,
    },
    Preset {
        name: "ollama",
        kind: "ollama",
        endpoint: "http://127.0.0.1:11434",
        secret: None,
        model: "qwen3:8b",
        model_options: "{}",
    },
];

/// The preset `name`.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

impl Preset {
    /// The provider object the preset gives, in the form of the `models` kind. The
    /// model's entry states the default `structuredOutput`, since the merge of the
    /// settings layers drops an empty object.
    pub fn provider(&self) -> Value {
        let mut entry = json!({ "structuredOutput": "auto" });
        if let (Some(e), Ok(Value::Object(o))) = (
            entry.as_object_mut(),
            serde_json::from_str::<Value>(self.model_options),
        ) {
            e.extend(o);
        }
        let mut p = json!({
            "kind": self.kind,
            "endpoint": self.endpoint,
            "models": { self.model: entry },
        });
        if let Some(s) = self.secret {
            p["apiKey"] = json!({ "secret": s });
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ModelsConfig;

    /// Each preset gives a provider that the configuration checks accept.
    #[test]
    fn presets_are_valid_providers() {
        for p in PRESETS {
            let cfg = json!({ "providers": { p.name: p.provider() } });
            ModelsConfig::from_value(&cfg).unwrap_or_else(|e| panic!("{}: {e:#}", p.name));
        }
        assert!(preset("ollama").is_some());
        assert!(preset("gateway").is_none());
        // the hosted presets start with the models the Ask comparison chose by its rule
        // (docs/benchmarks/natural-language/RESULTS.md, Ask across providers)
        assert_eq!(preset("openai").unwrap().model, "gpt-6-luna");
        assert_eq!(preset("anthropic").unwrap().model, "claude-sonnet-5-5");
    }

    /// A provider from a hosted preset asks its model the way the comparison did: Luna
    /// as a reasoning model with `max_completion_tokens`, and each with the 200,000-token
    /// context, so that Ask does not trim the grounding for a small model.
    #[test]
    fn hosted_presets_reproduce_the_measured_settings() {
        use crate::models::client::{ChatRequest, Message, Part, build};
        for name in ["openai", "anthropic"] {
            let p = preset(name).unwrap();
            let cfg =
                ModelsConfig::from_value(&json!({ "providers": { name: p.provider() } })).unwrap();
            let pc = &cfg.providers[name];
            let m = pc.model(p.model);
            assert_eq!(m.context_tokens, 200_000, "{name}");
            assert_eq!(
                m.reasoning_effort.map(|e| e.as_str()),
                Some("medium"),
                "{name}"
            );
            let req = ChatRequest {
                model: p.model.into(),
                system: vec![Part::cached("sys")],
                messages: vec![Message::user("q")],
                schema: None,
                level: crate::models::Level::Text,
                max_output_tokens: m.max_output_tokens,
                temperature: m.temperature,
                reasoning_effort: m.reasoning_effort,
                num_ctx: None,
                timeout: std::time::Duration::from_secs(1),
            };
            let (_, body) = build(pc, &req);
            if name == "openai" {
                assert_eq!(body["max_completion_tokens"], json!(16384));
                assert_eq!(body["reasoning_effort"], json!("medium"));
                assert!(body.get("max_tokens").is_none() && body.get("temperature").is_none());
            } else {
                assert_eq!(body["max_tokens"], json!(4096));
            }
        }
    }

    /// The UI's table holds the same presets in the same order.
    #[test]
    fn the_ui_table_agrees() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../ui/src/lib/provider-presets.json");
        let Ok(text) = std::fs::read_to_string(&path) else {
            // a source tree without the UI, such as a packaged crate
            eprintln!("skipped: no {}", path.display());
            return;
        };
        let ui: Vec<Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(ui.len(), PRESETS.len(), "{}", path.display());
        for (u, p) in ui.iter().zip(PRESETS) {
            assert_eq!(u["name"], p.name);
            assert_eq!(u["kind"], p.kind, "{}", p.name);
            assert_eq!(u["endpoint"], p.endpoint, "{}", p.name);
            assert_eq!(u["secret"].as_str(), p.secret, "{}", p.name);
            assert_eq!(u["model"], p.model, "{}", p.name);
            let options: Value = serde_json::from_str(p.model_options).unwrap();
            assert_eq!(u["modelOptions"], options, "{}", p.name);
        }
    }
}
