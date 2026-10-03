//! The installed configuration of a dataset's GraphQL endpoint (§3.1): the mapping SDL
//! and its options, kept with a version history in `<db>/graphql.json` (in memory for
//! in-memory datasets). Every change adds a version with the time, the author, a
//! message, the dataset's head commit and a digest chained to its parent's, as stored
//! queries do. The last [`MAX_VERSIONS`] versions are kept.

use crate::Compiled;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sparkles::error::{Error, Result};
use sparkles::guard::config::DataGraphSel;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The file of a database directory that holds the GraphQL configuration.
pub const FILE: &str = "graphql.json";

/// Version of the file's JSON shape.
pub const FORMAT: u32 = 1;

/// Versions kept.
pub const MAX_VERSIONS: usize = 20;

/// Largest SDL text, in bytes.
pub const MAX_SDL_BYTES: usize = 1 << 20;

fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn is_default_graph(d: &DataGraphSel) -> bool {
    *d == DataGraphSel::default()
}

/// The limits of §7 a schema sets; each is at most the server's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_first: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_first: Option<u32>,
}

impl Limits {
    fn is_empty(&self) -> bool {
        *self == Limits::default()
    }
}

/// The configuration an administrator installs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub sdl: String,
    /// `"default"`, `"union"` or a list of graph IRIs
    #[serde(default, skip_serializing_if = "is_default_graph")]
    pub data_graph: DataGraphSel,
    /// include the inferred graph (`true`), leave it out (`false`), or follow the SPARQL
    /// endpoint's default (`null`)
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub introspection: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub persisted_only: bool,
    #[serde(default, skip_serializing_if = "Limits::is_empty")]
    pub limits: Limits,
}

impl Config {
    pub fn new(sdl: impl Into<String>) -> Config {
        Config {
            sdl: sdl.into(),
            data_graph: DataGraphSel::default(),
            reasoning: None,
            introspection: true,
            persisted_only: false,
            limits: Limits::default(),
        }
    }

    /// Check the fields other than the SDL.
    pub fn check(&self) -> Result<()> {
        if self.sdl.len() > MAX_SDL_BYTES {
            return Err(Error::Invalid(format!(
                "sdl: at most {MAX_SDL_BYTES} bytes"
            )));
        }
        self.data_graph.check()?;
        if self.persisted_only {
            return Err(Error::Invalid(
                "persistedOnly needs stored GraphQL queries, which this version does not have"
                    .into(),
            ));
        }
        let l = &self.limits;
        if l.max_depth == Some(0) || l.max_first == Some(0) || l.max_nodes == Some(0) {
            return Err(Error::Invalid("limits must be positive".into()));
        }
        if let (Some(d), Some(m)) = (l.default_first, l.max_first)
            && d > m
        {
            return Err(Error::Invalid(
                "limits: defaultFirst must be at most maxFirst".into(),
            ));
        }
        Ok(())
    }
}

/// The metadata of a version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_commit: Option<u64>,
    pub digest: String,
}

/// A configuration with its version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stored {
    #[serde(flatten)]
    pub version: Version,
    pub config: Config,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct FileDoc {
    format: u32,
    versions: Vec<Stored>,
}

/// Who changes the configuration, and the precondition of the change.
#[derive(Clone, Debug, Default)]
pub struct Change {
    pub author: Option<String>,
    pub message: Option<String>,
    pub dataset_commit: Option<u64>,
    /// fail with [`Error::PreconditionFailed`] unless the current version is this one
    /// (`Some(0)`: no configuration may be installed yet)
    pub if_version: Option<u64>,
}

/// The outcome of a [`Catalog::put`].
#[derive(Clone)]
pub struct Saved {
    pub stored: Stored,
    /// `false`: the configuration equals the current one and no version was added
    pub changed: bool,
    pub created: bool,
    pub compiled: Arc<Compiled>,
}

fn digest(parent: Option<&str>, c: &Config) -> String {
    let mut h = Sha256::new();
    h.update(parent.unwrap_or("").as_bytes());
    h.update([0u8]);
    h.update(serde_json::to_vec(c).unwrap_or_default());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The GraphQL configuration of one dataset.
pub struct Catalog {
    root: Option<PathBuf>,
    doc: RwLock<FileDoc>,
    broken: Option<String>,
    /// the compiled current version
    compiled: RwLock<Option<Arc<Compiled>>>,
}

fn empty() -> FileDoc {
    FileDoc {
        format: FORMAT,
        versions: Vec::new(),
    }
}

impl Catalog {
    /// The configuration of a database directory (`None`: in memory). A missing file is
    /// no configuration; a malformed one is an error.
    pub fn open(root: Option<&Path>) -> Result<Catalog> {
        let doc = match root {
            Some(r) => match std::fs::read(r.join(FILE)) {
                Ok(b) => {
                    let d: FileDoc = serde_json::from_slice(&b)
                        .map_err(|e| Error::Invalid(format!("{FILE}: {e}")))?;
                    if d.format != FORMAT {
                        return Err(Error::Invalid(format!(
                            "{FILE}: unknown format {} (this version reads {FORMAT})",
                            d.format
                        )));
                    }
                    d
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => empty(),
                Err(e) => return Err(e.into()),
            },
            None => empty(),
        };
        Ok(Catalog {
            root: root.map(Path::to_path_buf),
            doc: RwLock::new(doc),
            broken: None,
            compiled: RwLock::new(None),
        })
    }

    /// [`Catalog::open`], or, when the file cannot be read, an empty catalog that refuses
    /// changes and names the problem.
    pub fn open_or_broken(root: Option<&Path>) -> Catalog {
        Catalog::open(root).unwrap_or_else(|e| Catalog {
            root: root.map(Path::to_path_buf),
            doc: RwLock::new(empty()),
            broken: Some(e.to_string()),
            compiled: RwLock::new(None),
        })
    }

    /// Why the file could not be read, if it could not.
    pub fn broken(&self) -> Option<&str> {
        self.broken.as_deref()
    }

    /// The installed configuration, or the given version while it is kept.
    pub fn get(&self, version: Option<u64>) -> Option<Stored> {
        let doc = self.doc.read();
        match version {
            None => doc.versions.last().cloned(),
            Some(v) => doc
                .versions
                .iter()
                .find(|s| s.version.version == v)
                .cloned(),
        }
    }

    /// The kept versions, newest first.
    pub fn versions(&self) -> Vec<Version> {
        self.doc
            .read()
            .versions
            .iter()
            .rev()
            .map(|s| s.version.clone())
            .collect()
    }

    /// The compiled current configuration (`None`: none is installed). It is compiled
    /// once per version.
    pub fn compiled(&self) -> Result<Option<Arc<Compiled>>> {
        let Some(cur) = self.get(None) else {
            return Ok(None);
        };
        if let Some(c) = self.compiled.read().as_ref()
            && c.version == cur.version.version
        {
            return Ok(Some(c.clone()));
        }
        let c = Arc::new(
            Compiled::new(cur.config, cur.version.version, &|_, _, _| true)
                .map_err(|e| Error::Invalid(e.message()))?
                .0,
        );
        *self.compiled.write() = Some(c.clone());
        Ok(Some(c))
    }

    fn save(&self, doc: &FileDoc) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let path = root.join(FILE);
        if doc.versions.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        let bytes = serde_json::to_vec_pretty(doc).map_err(|e| Error::Io(e.into()))?;
        sparkles::guard::config::write_atomic(&path, &bytes)
    }

    fn writable(&self) -> Result<()> {
        match &self.broken {
            Some(e) => Err(Error::Conflict(format!(
                "the GraphQL configuration cannot be changed until {FILE} is fixed or removed: {e}"
            ))),
            None => Ok(()),
        }
    }

    /// Install a configuration as the next version, after it compiles. An unchanged
    /// configuration adds no version. `backing` decides the non-null warnings.
    pub fn put(
        &self,
        config: Config,
        change: Change,
        backing: crate::mapping::Backing,
    ) -> std::result::Result<(Saved, Vec<String>), crate::PutError> {
        self.writable().map_err(crate::PutError::Engine)?;
        config.check().map_err(crate::PutError::Engine)?;
        let mut doc = self.doc.write();
        let current = doc.versions.last().cloned();
        let have = current.as_ref().map_or(0, |c| c.version.version);
        if let Some(want) = change.if_version
            && want != have
        {
            return Err(crate::PutError::Engine(Error::PreconditionFailed(format!(
                "the GraphQL configuration is at version {have}, not {want}"
            ))));
        }
        let next_version = if current.as_ref().is_some_and(|c| c.config == config) {
            have
        } else {
            have + 1
        };
        let (compiled, warnings) = Compiled::new(config.clone(), next_version, backing)?;
        let compiled = Arc::new(compiled);
        if let Some(c) = &current
            && c.config == config
        {
            return Ok((
                Saved {
                    stored: c.clone(),
                    changed: false,
                    created: false,
                    compiled,
                },
                warnings,
            ));
        }
        let version = Version {
            version: next_version,
            parent: (have > 0).then_some(have),
            created: sparkles::builder::now_rfc3339(),
            author: change.author,
            message: change.message,
            dataset_commit: change.dataset_commit,
            digest: digest(current.as_ref().map(|c| c.version.digest.as_str()), &config),
        };
        let stored = Stored { version, config };
        let mut next = doc.clone();
        next.versions.push(stored.clone());
        if next.versions.len() > MAX_VERSIONS {
            let drop = next.versions.len() - MAX_VERSIONS;
            next.versions.drain(..drop);
        }
        self.save(&next).map_err(crate::PutError::Engine)?;
        *doc = next;
        *self.compiled.write() = Some(compiled.clone());
        Ok((
            Saved {
                stored,
                changed: true,
                created: have == 0,
                compiled,
            },
            warnings,
        ))
    }

    /// Remove the configuration and its versions; `false` when there was none.
    pub fn delete(&self, if_version: Option<u64>) -> Result<bool> {
        self.writable()?;
        let mut doc = self.doc.write();
        let have = doc.versions.last().map_or(0, |c| c.version.version);
        if let Some(want) = if_version
            && want != have
        {
            return Err(Error::PreconditionFailed(format!(
                "the GraphQL configuration is at version {have}, not {want}"
            )));
        }
        if have == 0 {
            return Ok(false);
        }
        let next = empty();
        self.save(&next)?;
        *doc = next;
        *self.compiled.write() = None;
        Ok(true)
    }
}
