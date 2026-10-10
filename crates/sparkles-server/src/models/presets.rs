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
    },
    Preset {
        name: "openai",
        kind: "openai",
        endpoint: "https://api.openai.com/v1",
        secret: Some("openai"),
        model: "gpt-5-mini",
    },
    Preset {
        name: "ollama",
        kind: "ollama",
        endpoint: "http://127.0.0.1:11434",
        secret: None,
        model: "qwen3:8b",
    },
];

/// The preset `name`.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

impl Preset {
    /// The provider object the preset gives, in the form of the `models` kind.
    pub fn provider(&self) -> Value {
        let mut p = json!({
            "kind": self.kind,
            "endpoint": self.endpoint,
            "models": { self.model: {} },
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
        }
    }
}
