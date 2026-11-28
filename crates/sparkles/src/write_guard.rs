//! Write-time validation of a dataset: the guard installed on its store, SHACL or ShEx
//! after the `language` of `validation.json`, and installing it when the dataset opens.
//!
//! A store whose directory has a `validation.json` refuses writes with
//! [`Error::GuardMissing`] until a guard is installed. [`Dataset::open`] installs it with
//! the validator the configuration names, as the server does. When that validator is
//! not in the build (the `shacl` or `shex` feature), or the configuration cannot be
//! loaded, the dataset keeps refusing writes and [`Dataset::guard_error`] says why.
//!
//! [`Dataset::open`]: crate::Dataset::open
//! [`Dataset::guard_error`]: crate::Dataset::guard_error

// (without a validator nothing is installed, and a dataset that requires one refuses
// writes)
#![cfg_attr(not(any(feature = "shacl", feature = "shex")), allow(unused_imports))]

#[cfg(any(feature = "shacl", feature = "shex"))]
use crate::error::Error;
use crate::error::Result;
use crate::guard::GuardLanguage;
use crate::store::Store;
use serde_json::{Value as J, json};
#[cfg(any(feature = "shacl", feature = "shex"))]
use std::sync::Arc;

#[cfg(all(feature = "shacl", feature = "shex"))]
pub(crate) mod combined;
#[cfg(all(feature = "shacl", feature = "shex"))]
pub use combined::{CombinedGuard, GuardConfig, parse_configs};

/// The write-time validation installed on a dataset. It has no variants in a build
/// without the `shacl` and `shex` features.
#[derive(Clone)]
pub enum WriteGuard {
    #[cfg(feature = "shacl")]
    Shacl(Arc<sparkles_shacl::guard::ShaclGuard>),
    #[cfg(feature = "shex")]
    Shex(Arc<sparkles_shex::guard::ShexGuard>),
    #[cfg(all(feature = "shacl", feature = "shex"))]
    Combined(Arc<CombinedGuard>),
}

impl std::fmt::Debug for WriteGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("WriteGuard")
            .field(&self.language().name())
            .finish()
    }
}

impl WriteGuard {
    pub fn language(&self) -> GuardLanguage {
        match *self {
            #[cfg(feature = "shacl")]
            WriteGuard::Shacl(_) => GuardLanguage::Shacl,
            #[cfg(feature = "shex")]
            WriteGuard::Shex(_) => GuardLanguage::Shex,
            #[cfg(all(feature = "shacl", feature = "shex"))]
            WriteGuard::Combined(_) => GuardLanguage::Combined,
        }
    }

    /// The strictest mode of the installed guards.
    pub fn mode(&self) -> crate::guard::GuardMode {
        #[cfg(all(feature = "shacl", feature = "shex"))]
        use crate::guard::GuardMode;
        match *self {
            #[cfg(feature = "shacl")]
            Self::Shacl(ref guard) => guard.config().mode,
            #[cfg(feature = "shex")]
            Self::Shex(ref guard) => guard.config().mode,
            #[cfg(all(feature = "shacl", feature = "shex"))]
            Self::Combined(ref guard) => {
                if guard
                    .guards()
                    .iter()
                    .any(|guard| guard.mode() == GuardMode::Reject)
                {
                    GuardMode::Reject
                } else {
                    GuardMode::Warn
                }
            }
        }
    }

    /// The SHACL guard, when configured individually or with ShEx.
    #[cfg(feature = "shacl")]
    pub fn shacl(&self) -> Option<&Arc<sparkles_shacl::guard::ShaclGuard>> {
        match self {
            Self::Shacl(guard) => Some(guard),
            #[cfg(all(feature = "shacl", feature = "shex"))]
            Self::Combined(guard) => guard.guards().iter().find_map(Self::shacl),
            #[cfg(feature = "shex")]
            Self::Shex(_) => None,
        }
    }

    /// The ShEx guard, when configured individually or with SHACL.
    #[cfg(feature = "shex")]
    pub fn shex(&self) -> Option<&Arc<sparkles_shex::guard::ShexGuard>> {
        match self {
            Self::Shex(guard) => Some(guard),
            #[cfg(all(feature = "shacl", feature = "shex"))]
            Self::Combined(guard) => guard.guards().iter().find_map(Self::shex),
            #[cfg(feature = "shacl")]
            Self::Shacl(_) => None,
        }
    }

    #[cfg(all(feature = "shacl", feature = "shex"))]
    fn commit_guard(&self) -> &dyn crate::guard::CommitGuard {
        match self {
            Self::Shacl(guard) => guard.as_ref(),
            Self::Shex(guard) => guard.as_ref(),
            Self::Combined(guard) => guard.as_ref(),
        }
    }

    #[cfg(any(feature = "shacl", feature = "shex"))]
    fn attach(&self, store: &Store) {
        let guard: Arc<dyn crate::guard::CommitGuard> = match self {
            #[cfg(feature = "shacl")]
            Self::Shacl(guard) => guard.clone(),
            #[cfg(feature = "shex")]
            Self::Shex(guard) => guard.clone(),
            #[cfg(all(feature = "shacl", feature = "shex"))]
            Self::Combined(guard) => guard.clone(),
        };
        store.set_guard(Some(guard));
        store.set_guard_required(true);
    }

    #[cfg(any(feature = "shacl", feature = "shex"))]
    fn fork_detached(&self, snap: &crate::store::Snapshot) -> Result<Self> {
        Ok(match self {
            #[cfg(feature = "shacl")]
            Self::Shacl(guard) => Self::Shacl(
                guard
                    .fork_detached(snap)
                    .map_err(crate::catalog::component)?,
            ),
            #[cfg(feature = "shex")]
            Self::Shex(guard) => Self::Shex(
                guard
                    .fork_detached(snap)
                    .map_err(crate::catalog::component)?,
            ),
            #[cfg(all(feature = "shacl", feature = "shex"))]
            Self::Combined(guard) => Self::Combined(guard.fork(snap)?),
        })
    }

    pub(crate) fn fork_for_store(&self, store: &Store) -> Result<Self> {
        #[cfg(any(feature = "shacl", feature = "shex"))]
        {
            let guard = self.fork_detached(&store.snapshot())?;
            guard.attach(store);
            Ok(guard)
        }
        #[cfg(not(any(feature = "shacl", feature = "shex")))]
        {
            let _ = store;
            match *self {}
        }
    }

    /// `{language, config, status}`, the configuration and the counts the server shows
    /// at `GET /$/validation/{ds}`.
    pub fn json(&self) -> J {
        match *self {
            #[cfg(feature = "shacl")]
            WriteGuard::Shacl(ref g) => {
                let mut config = json!(g.config());
                // Inline sources have no file on memory stores. Include them in the
                // editable response; on-disk configuration serialization stays compact.
                if let Some(text) = &g.config().shapes.inline {
                    config["shapes"]["inline"] = json!(text);
                    if let Some(format) = &g.config().shapes.format {
                        config["shapes"]["format"] = json!(format);
                    }
                }
                json!({ "language": "shacl", "config": config, "status": g.status() })
            }
            #[cfg(feature = "shex")]
            WriteGuard::Shex(ref g) => {
                let mut config = json!(g.config());
                if let Some((_, text)) = g.schema_copy()
                    && let Some(schema) = config["schema"].as_object_mut()
                {
                    schema.remove("file");
                    schema.insert("inline".into(), json!(text));
                }
                json!({ "language": "shex", "config": config, "status": g.status() })
            }
            #[cfg(all(feature = "shacl", feature = "shex"))]
            WriteGuard::Combined(ref guard) => {
                let guards: Vec<_> = guard
                    .guards()
                    .iter()
                    .map(|guard| {
                        let mut j = guard.json();
                        json!({ "language": guard.language().name(), "status": j["status"].take() })
                    })
                    .collect();
                json!({ "language": "combined", "config": guard.config(), "status": { "guards": guards } })
            }
        }
    }

    /// The files of this validation in a database directory, for a backup of an
    /// in-memory dataset, which has no directory: `validation.json`, and the copy of
    /// SHACL shapes given inline or of the ShEx schema. They are what a persistent
    /// dataset with the same configuration keeps.
    pub fn memory_files(&self) -> Vec<(String, Vec<u8>)> {
        #[cfg(any(feature = "shacl", feature = "shex"))]
        let config = crate::guard::config::CONFIG_FILE.to_string();
        match *self {
            #[cfg(feature = "shacl")]
            WriteGuard::Shacl(ref g) => {
                // inline shapes become the copy a persistent dataset keeps
                let mut cfg = g.config().clone();
                let mut out = Vec::new();
                if let Some(text) = cfg.shapes.inline.take() {
                    // Match persistent installation: the copied file is always Turtle.
                    let syntax = match cfg.shapes.format.as_deref() {
                        None => sparkles_shacl::ShapesSyntax::default(),
                        Some(media) => match sparkles_shacl::ShapesSyntax::from_media_type(media) {
                            Some(syntax) => syntax,
                            None => return Vec::new(),
                        },
                    };
                    let text = if syntax == sparkles_shacl::ShapesSyntax::default() {
                        text
                    } else {
                        match sparkles_shacl::syntax::convert(
                            &text,
                            syntax,
                            sparkles_shacl::ShapesSyntax::default(),
                            None,
                        ) {
                            Ok(text) => text,
                            Err(_) => return Vec::new(),
                        }
                    };
                    let file = crate::guard::config::SHACL_SHAPES_FILE;
                    cfg.shapes.file = Some(file.to_string());
                    cfg.shapes.sha256 = Some(crate::guard::config::sha256_hex(text.as_bytes()));
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
            WriteGuard::Shex(ref g) => match serde_json::to_vec_pretty(g.config()) {
                Ok(b) => {
                    let mut files = vec![(config, b)];
                    if let Some((file, text)) = g.schema_copy() {
                        files.push((file.to_string(), text.as_bytes().to_vec()));
                    }
                    files
                }
                Err(_) => Vec::new(),
            },
            #[cfg(all(feature = "shacl", feature = "shex"))]
            WriteGuard::Combined(ref guard) => match serde_json::to_vec_pretty(&guard.config()) {
                Ok(bytes) => vec![(config, bytes)],
                Err(_) => Vec::new(),
            },
        }
    }

    /// One line for `sparkles stats`: `reject · 2 shape graphs · 20 shapes · last full
    /// 164 ms`.
    pub fn stats_line(&self) -> String {
        match *self {
            #[cfg(feature = "shacl")]
            WriteGuard::Shacl(ref g) => {
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
            WriteGuard::Shex(ref g) => {
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
            #[cfg(all(feature = "shacl", feature = "shex"))]
            WriteGuard::Combined(ref guard) => guard
                .guards()
                .iter()
                .map(Self::stats_line)
                .collect::<Vec<_>>()
                .join(" / "),
        }
    }
}

/// `reject`, `warn` or `off`.
pub fn mode_name(m: crate::guard::GuardMode) -> &'static str {
    match m {
        crate::guard::GuardMode::Reject => "reject",
        crate::guard::GuardMode::Warn => "warn",
        crate::guard::GuardMode::Off => "off",
    }
}

/// Install a persistent store's write-time validation from its `validation.json`, with
/// the validator its `language` names. Without a configuration, or with mode `off`,
/// nothing is installed. An error, such as a configuration that cannot be loaded or a
/// language this build has no feature for, leaves the store refusing writes: it fails
/// closed.
pub fn install(store: &Store) -> Result<Option<WriteGuard>> {
    let Some(root) = store.root() else {
        return Ok(None);
    };
    let Some(language) = crate::guard::config::config_language(root)? else {
        return Ok(None);
    };
    #[cfg(any(feature = "shacl", feature = "shex"))]
    let failed = |e: anyhow::Error| Error::invalid(format!("{e:#}"));
    match language {
        #[cfg(feature = "shacl")]
        GuardLanguage::Shacl => Ok(sparkles_shacl::guard::install(store)
            .map_err(failed)?
            .map(WriteGuard::Shacl)),
        #[cfg(feature = "shex")]
        GuardLanguage::Shex => Ok(sparkles_shex::guard::install(store)
            .map_err(failed)?
            .map(WriteGuard::Shex)),
        #[cfg(all(feature = "shacl", feature = "shex"))]
        GuardLanguage::Combined => combined::install(store).map(Some),
        #[cfg(not(all(feature = "shacl", feature = "shex")))]
        GuardLanguage::Combined => Err(crate::Error::unsupported(
            "combined write-time validation needs both the `shacl` and `shex` features",
        )),
        #[allow(unreachable_patterns)]
        l => Err(crate::Error::unsupported(format!(
            "the dataset uses write-time {} validation, but Sparkles was built without the `{}` feature",
            l.title(),
            l.name()
        ))),
    }
}
