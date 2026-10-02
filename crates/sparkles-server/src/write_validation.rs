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

    /// The files of this validation in a database directory, for a backup of an
    /// in-memory dataset, which has no directory: `validation.json`, and the copy of
    /// SHACL shapes given inline or of the ShEx schema. They are what a persistent
    /// dataset with the same configuration keeps.
    #[cfg(feature = "backup")]
    pub fn memory_files(&self) -> Vec<(String, Vec<u8>)> {
        #[cfg(any(feature = "shacl", feature = "shex"))]
        let config = sparkles::guard::config::CONFIG_FILE.to_string();
        match *self {
            #[cfg(feature = "shacl")]
            Validation::Shacl(ref g) => {
                // inline shapes become the copy a persistent dataset keeps
                let mut cfg = g.config().clone();
                let mut out = Vec::new();
                if let Some(text) = cfg.shapes.inline.take() {
                    let file = sparkles::guard::config::SHACL_SHAPES_FILE;
                    cfg.shapes.file = Some(file.to_string());
                    cfg.shapes.sha256 = Some(sparkles::guard::config::sha256_hex(text.as_bytes()));
                    cfg.shapes.format = None;
                    out.push((file.to_string(), text.into_bytes()));
                }
                match serde_json::to_vec_pretty(&cfg) {
                    Ok(b) => out.insert(0, (config, b)),
                    Err(_) => out.clear(),
                }
                out
            }
            #[cfg(feature = "shex")]
            Validation::Shex(ref g) => {
                let Some((file, text)) = g.schema_copy() else {
                    return Vec::new();
                };
                match serde_json::to_vec_pretty(g.config()) {
                    Ok(b) => vec![(config, b), (file.to_string(), text.as_bytes().to_vec())],
                    Err(_) => Vec::new(),
                }
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

    /// A database with a ShEx configuration (and its schema copy).
    fn shex_database() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path(), Default::default()).unwrap());
        std::fs::write(
            dir.path().join(sparkles::guard::config::CONFIG_FILE),
            r#"{"format":2,"language":"shex","mode":"reject","schema":{"file":"validation-schema.shex","format":"shexc"},"shapeMap":"{FOCUS a <http://ex.org/P>}@<http://ex.org/S>"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join(sparkles::guard::config::SHEX_SCHEMA_SHEXC_FILE),
            "<http://ex.org/S> { <http://ex.org/name> . }",
        )
        .unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        assert!(store.guard_required());
        (dir, store)
    }

    fn insert(store: &Store) -> sparkles::Result<sparkles::sparql::update::UpdateStats> {
        sparkles::sparql::update::update(
            store,
            "INSERT DATA { <http://ex.org/a> a <http://ex.org/P> }",
            &Default::default(),
        )
    }

    #[cfg(feature = "shex")]
    #[test]
    fn shex_configurations_install_the_shex_guard() {
        let (dir, store) = shex_database();
        let v = install(&store).unwrap().unwrap();
        assert_eq!(v.language(), GuardLanguage::Shex);
        assert_eq!(v.json()["language"], "shex");
        assert_eq!(v.json()["status"]["shapeCount"], 1);
        assert_eq!(v.stats_line(), "reject · ShEx · 1 shapes");
        assert!(matches!(insert(&store), Err(sparkles::Error::Rejected(_))));
        // without its schema copy the guard cannot be installed: writes stay refused
        drop(store);
        std::fs::remove_file(
            dir.path()
                .join(sparkles::guard::config::SHEX_SCHEMA_SHEXC_FILE),
        )
        .unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        assert!(install(&store).is_err());
        assert!(matches!(
            insert(&store),
            Err(sparkles::Error::GuardMissing(_))
        ));
    }

    /// The files of an in-memory dataset's validation install the same guard in a
    /// database directory (a backup restored as a persistent dataset).
    #[cfg(all(feature = "shex", feature = "shacl", feature = "backup"))]
    #[test]
    fn in_memory_validation_files_install_in_a_directory() {
        let mem = Store::in_memory(Default::default());
        let cfg = serde_json::from_value(json!({
            "language": "shex",
            "mode": "reject",
            "schema": {"inline": "<http://ex.org/S> { <http://ex.org/name> . }"},
            "shapeMap": "{FOCUS a <http://ex.org/P>}@<http://ex.org/S>",
        }))
        .unwrap();
        let g = match sparkles_shex::guard::set_config(&mem, Some(cfg), &sparkles_shex::NoImports)
            .unwrap()
        {
            sparkles_shex::guard::SetOutcome::Installed(g, _) => g,
            _ => panic!("not installed"),
        };
        let shacl = {
            let mem = Store::in_memory(Default::default());
            let cfg = serde_json::from_value(json!({
                "mode": "reject",
                "shapes": {"inline": "<urn:S> a <http://www.w3.org/ns/shacl#NodeShape> ; \
                    <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/P> ; \
                    <http://www.w3.org/ns/shacl#property> [ \
                    <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ; \
                    <http://www.w3.org/ns/shacl#minCount> 1 ] ."},
            }))
            .unwrap();
            match sparkles_shacl::guard::set_config(&mem, Some(cfg)).unwrap() {
                sparkles_shacl::guard::SetOutcome::Installed(g, _) => Validation::Shacl(g),
                _ => panic!("not installed"),
            }
        };
        for v in [Validation::Shex(g), shacl] {
            let files = v.memory_files();
            assert_eq!(
                files.len(),
                2,
                "{:?}",
                files.iter().map(|f| &f.0).collect::<Vec<_>>()
            );
            let dir = tempfile::tempdir().unwrap();
            drop(Store::open(dir.path(), Default::default()).unwrap());
            for (name, bytes) in &files {
                std::fs::write(dir.path().join(name), bytes).unwrap();
            }
            let store = Store::open(dir.path(), Default::default()).unwrap();
            let installed = install(&store).unwrap().unwrap();
            assert_eq!(installed.language(), v.language());
            assert!(matches!(insert(&store), Err(sparkles::Error::Rejected(_))));
        }
    }

    #[cfg(not(feature = "shex"))]
    #[test]
    fn shex_configurations_fail_closed_without_the_feature() {
        let (_dir, store) = shex_database();
        let e = install(&store).err().unwrap();
        assert!(
            format!("{e:#}").contains("built without the `shex` feature"),
            "{e:#}"
        );
        assert!(matches!(
            insert(&store),
            Err(sparkles::Error::GuardMissing(_))
        ));
    }
}
