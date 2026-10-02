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
                    match (&cfg.shapes.graphs, &cfg.shapes.file) {
                        (Some(g), file) => format!(
                            "{} shape graph{}{}",
                            g.len(),
                            if g.len() == 1 { "" } else { "s" },
                            if file.is_some() {
                                " and a shapes file"
                            } else {
                                ""
                            }
                        ),
                        (None, _) => "shapes file".to_string(),
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

/// `serve --validate NAME[=CONFIG.json]`: set a dataset's write-time validation from a
/// configuration file (the body of `PUT /$/validation/{ds}`), or with `NAME` alone
/// validate the dataset with the configuration it has. Either way the data is validated
/// in full under the writer lock, which also makes the state of the head known for the
/// writes that follow. Returns the line to log.
///
/// In the file, shapes (SHACL) or a schema (ShEx) without `inline` text or a stored
/// `file` are read from the path in `source`, relative to the file's directory. A
/// configuration whose `reject` mode the data does not pass is an error, and nothing
/// changes. With `NAME` alone, data that does not pass is reported, and the
/// configuration stays.
#[cfg(any(feature = "shacl", feature = "shex"))]
pub fn validate_at_startup(st: &crate::state::AppState, spec: &str) -> Result<String> {
    use anyhow::{Context, bail};
    use sparkles::guard::ValidationSummary;
    let (name, path) = match spec.split_once('=') {
        Some((n, p)) => (n, Some(std::path::PathBuf::from(p))),
        None => (spec, None),
    };
    let name = name.trim_start_matches('/');
    let ds = st
        .get(name)
        .with_context(|| format!("--validate: no dataset {name}"))?;
    if st.read_only {
        bail!("--validate: the server is read-only");
    }
    let (mut j, dir): (J, Option<std::path::PathBuf>) = match &path {
        Some(p) => {
            let bytes = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            let j = serde_json::from_slice(&bytes)
                .with_context(|| format!("{}: invalid validation configuration", p.display()))?;
            let dir = std::path::absolute(p)?
                .parent()
                .map(std::path::Path::to_path_buf);
            (j, dir)
        }
        None => {
            let v = ds.validation.read().clone().with_context(|| {
                format!(
                    "--validate: /{name} has no write-time validation (give {name}=CONFIG.json)"
                )
            })?;
            let j = match v {
                #[cfg(feature = "shacl")]
                Validation::Shacl(g) => {
                    let mut c = g.config().clone();
                    c.updated = None;
                    serde_json::to_value(c)?
                }
                #[cfg(feature = "shex")]
                Validation::Shex(g) => {
                    let mut c = g.config().clone();
                    c.updated = None;
                    // an in-memory dataset keeps its schema in the guard
                    if c.schema.inline.is_none()
                        && let Some((_, text)) = g.schema_copy()
                    {
                        c.schema.inline = Some(text.to_string());
                        c.schema.file = None;
                    }
                    let mut j = serde_json::to_value(&c)?;
                    if let Some(t) = c.schema.inline {
                        j["schema"]["inline"] = json!(t);
                    }
                    j
                }
            };
            (j, None)
        }
    };
    // `source` paths of a configuration file
    for key in ["shapes", "schema"] {
        let Some(src) = j.get_mut(key) else { continue };
        if src.is_object()
            && src.get("inline").is_none()
            && src.get("file").is_none()
            && let (Some(s), Some(dir)) = (src.get("source").and_then(J::as_str), &dir)
        {
            let file = dir.join(s);
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            if key == "schema" && src.get("base").is_none() {
                src["base"] = json!(sparkles_shex_base(&file));
            }
            src["source"] = json!(file.display().to_string());
            src["inline"] = json!(text);
        }
    }
    // a stored SHACL configuration keeps its inline shapes in the guard
    #[cfg(feature = "shacl")]
    if path.is_none()
        && let Some(Validation::Shacl(g)) = ds.validation.read().as_ref()
        && let Some(t) = &g.config().shapes.inline
    {
        j["shapes"]["inline"] = json!(t);
    }
    let language: GuardLanguage = match j.get("language") {
        None | Some(J::Null) => GuardLanguage::Shacl,
        Some(l) => serde_json::from_value(l.clone())
            .with_context(|| format!("unknown language {l} (\"shacl\" or \"shex\")"))?,
    };
    enum Set {
        Installed(Validation, ValidationSummary),
        NotConforming(ValidationSummary),
        Removed,
    }
    let set = match language {
        #[cfg(feature = "shacl")]
        GuardLanguage::Shacl => {
            use sparkles_shacl::guard::{SetOutcome, ValidationConfig, set_config};
            let cfg: ValidationConfig =
                serde_json::from_value(j).context("invalid SHACL validation configuration")?;
            match set_config(&ds.store, Some(cfg))? {
                SetOutcome::Installed(g, s) => Set::Installed(Validation::Shacl(g), s),
                SetOutcome::NotConforming(s) => Set::NotConforming(s),
                SetOutcome::Removed => Set::Removed,
            }
        }
        #[cfg(feature = "shex")]
        GuardLanguage::Shex => {
            use sparkles_shex::guard::{
                CONFIG_FORMAT, SetOutcome, ShexValidationConfig, set_config,
            };
            if let Some(o) = j.as_object_mut() {
                o.entry("format").or_insert(json!(CONFIG_FORMAT));
            }
            let cfg: ShexValidationConfig =
                serde_json::from_value(j).context("invalid ShEx validation configuration")?;
            // imports resolve against the configuration's directory, as on the command line
            let budget = sparkles::outbound::RequestBudget::new(&st.outbound);
            let resolver = sparkles_shex::FileResolver {
                dirs: dir.into_iter().collect(),
                outbound: Some((st.outbound.clone(), budget)),
                ..Default::default()
            };
            match set_config(&ds.store, Some(cfg), &resolver)? {
                SetOutcome::Installed(g, s) => Set::Installed(Validation::Shex(g), s),
                SetOutcome::NotConforming(s) => Set::NotConforming(s),
                SetOutcome::Removed => Set::Removed,
            }
        }
        #[allow(unreachable_patterns)]
        l => bail!("--validate: built without the `{}` feature", l.name()),
    };
    let counts = |s: &ValidationSummary| match s.language {
        GuardLanguage::Shex => format!("{} associations, {} nonconformant", s.total, s.blocking),
        _ => format!("{} results, {} blocking", s.total, s.blocking),
    };
    Ok(match set {
        Set::Installed(v, s) => {
            let mode = mode_name(s.mode);
            *ds.validation.write() = Some(v);
            format!(
                "write-time {} validation on /{name} ({mode}): {} in {} ms",
                s.language.title(),
                counts(&s),
                s.millis
            )
        }
        Set::NotConforming(s) if path.is_some() => bail!(
            "--validate {spec}: /{name} does not pass ({}); fix the data or use mode \"warn\" first",
            counts(&s)
        ),
        Set::NotConforming(s) => format!(
            "write-time {} validation on /{name}: the data does not pass ({}); writes that touch it are rejected until it is fixed",
            s.language.title(),
            counts(&s)
        ),
        Set::Removed => {
            *ds.validation.write() = None;
            format!("write-time validation on /{name} is off")
        }
    })
}

/// The base IRI of a schema file (its `file:` URL).
#[cfg(any(feature = "shacl", feature = "shex"))]
fn sparkles_shex_base(file: &std::path::Path) -> String {
    #[cfg(feature = "shex")]
    {
        sparkles_shex::resolve::file_url(file)
    }
    #[cfg(not(feature = "shex"))]
    {
        file.display().to_string()
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

    /// `serve --validate`: a configuration file with its shapes and schema read from
    /// `source`, a dataset validated with the configuration it has, and a `reject`
    /// configuration the data does not pass.
    #[cfg(all(feature = "shex", feature = "shacl"))]
    #[test]
    fn validation_at_startup() {
        use crate::state::{AppState, DbType};
        let dir = tempfile::tempdir().unwrap();
        let st = AppState::new(
            &dir.path().join("data"),
            Default::default(),
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        let ds = st.attach("ds", DbType::Mem, None).unwrap();
        let mem = st.attach("mem", DbType::Mem, None).unwrap();
        insert(&ds.store).unwrap();
        let conf = dir.path().join("conf");
        std::fs::create_dir(&conf).unwrap();
        std::fs::write(
            conf.join("shapes.ttl"),
            "<urn:S> a <http://www.w3.org/ns/shacl#NodeShape> ; \
             <http://www.w3.org/ns/shacl#targetClass> <http://ex.org/P> ; \
             <http://www.w3.org/ns/shacl#property> [ \
             <http://www.w3.org/ns/shacl#path> <http://ex.org/name> ; \
             <http://www.w3.org/ns/shacl#minCount> 1 ] .",
        )
        .unwrap();
        std::fs::write(
            conf.join("s.shex"),
            "<http://ex.org/S> { <http://ex.org/name> . }",
        )
        .unwrap();
        let write = |name: &str, j: J| {
            let p = conf.join(name);
            std::fs::write(&p, serde_json::to_vec(&j).unwrap()).unwrap();
            p.display().to_string()
        };
        let reject = write(
            "reject.json",
            json!({"mode": "reject", "shapes": {"source": "shapes.ttl"}}),
        );
        let warn = write(
            "warn.json",
            json!({"mode": "warn", "shapes": {"source": "shapes.ttl"}}),
        );
        let shex = write(
            "shex.json",
            json!({"language": "shex", "mode": "reject", "schema": {"source": "s.shex"},
                   "shapeMap": "{FOCUS a <http://ex.org/P>}@<http://ex.org/S>"}),
        );
        // the data does not pass `reject`: an error, and nothing is installed
        let e = validate_at_startup(&st, &format!("ds={reject}")).unwrap_err();
        assert!(
            format!("{e:#}").contains("does not pass (1 results, 1 blocking)"),
            "{e:#}"
        );
        assert!(ds.validation.read().is_none() && !ds.store.guard_required());
        let line = validate_at_startup(&st, &format!("ds={warn}")).unwrap();
        assert!(
            line.contains("SHACL validation on /ds (warn): 1 results, 1 blocking"),
            "{line}"
        );
        assert!(matches!(
            ds.validation.read().as_ref(),
            Some(Validation::Shacl(_))
        ));
        // NAME alone validates with the configuration the dataset has
        let line = validate_at_startup(&st, "ds").unwrap();
        assert!(line.contains("(warn): 1 results"), "{line}");
        assert!(validate_at_startup(&st, "mem").is_err());
        assert!(validate_at_startup(&st, "other").is_err());
        let line = validate_at_startup(&st, &format!("mem={shex}")).unwrap();
        assert!(
            line.contains("ShEx validation on /mem (reject): 0 associations"),
            "{line}"
        );
        let line = validate_at_startup(&st, "/mem").unwrap();
        assert!(line.contains("(reject)"), "{line}");
        assert!(matches!(
            insert(&mem.store),
            Err(sparkles::Error::Rejected(_))
        ));
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
