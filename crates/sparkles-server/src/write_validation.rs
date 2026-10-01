//! Write-time validation of a dataset in the server and the CLI: the installed guard,
//! SHACL or ShEx after the `language` of `validation.json`, its configuration and status
//! as JSON, and installing it when a store is opened.

// (no validator: nothing is installed, and a dataset that requires one refuses writes)
#![cfg_attr(not(any(feature = "shacl", feature = "shex")), allow(dead_code))]

use anyhow::Result;
use serde_json::{Value as J, json};
use sparkles::guard::GuardLanguage;
use sparkles::store::Store;
#[cfg(any(feature = "shacl", feature = "shex"))]
use std::sync::Arc;

/// The write-time validation installed on a dataset.
#[derive(Clone)]
pub enum Validation {
    #[cfg(feature = "shacl")]
    Shacl(Arc<sparkles_shacl::guard::ShaclGuard>),
    #[cfg(feature = "shex")]
    Shex(Arc<sparkles_shex::guard::ShexGuard>),
}

impl Validation {
    pub fn language(&self) -> GuardLanguage {
        match *self {
            #[cfg(feature = "shacl")]
            Validation::Shacl(_) => GuardLanguage::Shacl,
            #[cfg(feature = "shex")]
            Validation::Shex(_) => GuardLanguage::Shex,
        }
    }

    /// `{language, config, status}` (`GET /$/validation/{ds}`, `sparkles validation
    /// --status --format json`).
    pub fn json(&self) -> J {
        match *self {
            #[cfg(feature = "shacl")]
            Validation::Shacl(ref g) => {
                json!({ "language": "shacl", "config": g.config(), "status": g.status() })
            }
            #[cfg(feature = "shex")]
            Validation::Shex(ref g) => {
                json!({ "language": "shex", "config": g.config(), "status": g.status() })
            }
        }
    }

    /// The `sparkles stats` line: `reject · 2 shape graphs · 20 shapes · last full 164 ms`.
    pub fn stats_line(&self) -> String {
        match *self {
            #[cfg(feature = "shacl")]
            Validation::Shacl(ref g) => {
                let cfg = g.config();
                let s = g.status();
                let mut parts = vec![
                    mode_name(cfg.mode).to_string(),
                    match &cfg.shapes.graphs {
                        Some(g) => format!(
                            "{} shape graph{}",
                            g.len(),
                            if g.len() == 1 { "" } else { "s" }
                        ),
                        None => "shapes file".to_string(),
                    },
                    format!("{} shapes", s.shape_count),
                ];
                if let Some(ms) = s.last_full_millis {
                    parts.push(format!("last full {ms} ms"));
                }
                parts.join(" · ")
            }
            #[cfg(feature = "shex")]
            Validation::Shex(ref g) => {
                let s = g.status();
                let mut parts = vec![
                    mode_name(g.config().mode).to_string(),
                    "ShEx".to_string(),
                    format!("{} shapes", s.shape_count),
                ];
                if let Some(ms) = s.last_full_millis {
                    parts.push(format!("last full {ms} ms"));
                }
                parts.join(" · ")
            }
        }
    }
}

/// `reject`, `warn` or `off`.
pub fn mode_name(m: sparkles::guard::GuardMode) -> &'static str {
    match m {
        sparkles::guard::GuardMode::Reject => "reject",
        sparkles::guard::GuardMode::Warn => "warn",
        sparkles::guard::GuardMode::Off => "off",
    }
}

/// The JSON of a dataset without write-time validation.
pub fn none_json() -> J {
    json!({ "config": null })
}

/// Install a persistent store's write-time validation from its `validation.json`, with
/// the validator its `language` names. Without a configuration (or with mode `off`)
/// nothing is installed. An error (a configuration that cannot be loaded, or a language
/// this binary was built without) leaves the store refusing writes: it fails closed.
pub fn install(store: &Store) -> Result<Option<Validation>> {
    let Some(root) = store.root() else {
        return Ok(None);
    };
    let Some(language) = sparkles::guard::config::config_language(root)? else {
        return Ok(None);
    };
    match language {
        #[cfg(feature = "shacl")]
        GuardLanguage::Shacl => Ok(sparkles_shacl::guard::install(store)?.map(Validation::Shacl)),
        #[cfg(feature = "shex")]
        GuardLanguage::Shex => Ok(sparkles_shex::guard::install(store)?.map(Validation::Shex)),
        #[allow(unreachable_patterns)]
        l => anyhow::bail!(
            "the dataset uses write-time {} validation, but this binary was built without the `{}` feature",
            l.title(),
            l.name()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_configuration_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        assert!(install(&store).unwrap().is_none());
        assert!(!store.guard_required());
    }

    #[cfg(feature = "shex")]
    #[test]
    fn shex_configuration_fails_closed_until_implemented() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        drop(store);
        std::fs::write(
            dir.path().join(sparkles::guard::config::CONFIG_FILE),
            r#"{"format":2,"language":"shex","mode":"reject","schema":{"file":"validation-schema.shex"},"shapeMap":"<http://ex.org/a>@START"}"#,
        )
        .unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        assert!(store.guard_required());
        match install(&store) {
            // the guard is not implemented yet: the store keeps refusing writes
            Err(e) => assert!(format!("{e:#}").contains("ShEx"), "{e:#}"),
            Ok(v) => assert!(v.is_some_and(|v| v.language() == GuardLanguage::Shex)),
        }
    }
}
