//! Mappings stored per dataset in `<db>/mappings.json` (spec F19 §2.2, §8): R2RML,
//! RML, YARRRML, CSVW metadata and CONSTRUCT templates, each with a description, its
//! versions and the record of its last run, in the shape of the stored queries of
//! `queries.json` ([`crate::stored`]).
//!
//! A mapping is parsed and checked before it is stored, so a stored mapping always
//! parses. It refers to connections by name, and a connection that does not exist yet is
//! a warning when the mapping is stored and an error when it runs.

use crate::error::{Error, Result};
use crate::mapping::{self, MappingKind};
use crate::stored::Version;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file of a database directory that holds its stored mappings.
pub const FILE: &str = "mappings.json";

/// Version of the file's JSON shape.
pub const FORMAT: u32 = 1;

/// Versions kept per mapping; older ones are dropped.
pub const MAX_VERSIONS: usize = 100;

/// A stored mapping's definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Definition {
    /// `rml` (R2RML and RML), `yarrrml`, `csvw` or `template`.
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// What a check of a definition found.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub kind: String,
    /// R2RML, RML (rml.io), RML-Core or YARRRML, for the mapping engine's kinds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vocabulary: Option<String>,
    pub triples_maps: usize,
    /// the files the mapping reads, by name
    pub files: Vec<String>,
    /// the connections it reads (`""` is the default connection of an R2RML mapping)
    pub connections: Vec<String>,
    /// the graphs its graph maps name; `None` when one is not constant
    pub graphs: Option<Vec<String>>,
    pub warnings: Vec<String>,
}

impl Definition {
    pub fn kind(&self) -> Result<MappingKind> {
        MappingKind::parse(&self.kind).ok_or_else(|| {
            Error::invalid(format!(
                "unknown mapping kind {:?}: use rml, yarrrml, csvw or template",
                self.kind
            ))
        })
    }

    /// Parse and check the mapping.
    pub fn check(&self) -> Result<Summary> {
        if self.text.len() > mapping::MAX_MAPPING_BYTES {
            return Err(Error::invalid("the mapping is larger than 1 MiB"));
        }
        let kind = self.kind()?;
        let mut s = Summary {
            kind: kind.name().to_string(),
            ..Default::default()
        };
        match kind {
            MappingKind::Rml | MappingKind::Yarrrml => {
                let m = mapping::parse(&self.text, kind, None)?;
                s.vocabulary = Some(m.vocabulary.name().to_string());
                s.triples_maps = m.maps.len();
                for src in &m.sources {
                    match &src.kind {
                        mapping::model::SourceKind::Sql { connection, .. } => {
                            if !s.connections.contains(connection) {
                                s.connections.push(connection.clone());
                            }
                        }
                        k => {
                            let f = k.file().unwrap_or_default().to_string();
                            if !s.files.contains(&f) {
                                s.files.push(f);
                            }
                        }
                    }
                }
                s.graphs = m.constant_graphs().map(|gs| {
                    gs.into_iter()
                        .map(|g| {
                            g.map_or_else(|| "default".to_string(), |n| n.as_str().to_string())
                        })
                        .collect()
                });
                s.warnings = m.warnings;
            }
            MappingKind::Csvw => {
                let m = crate::tabular::csvw::parse(&self.text, None).map_err(Error::invalid)?;
                s.triples_maps = m.tables.len();
                s.files = m.tables.iter().filter_map(|t| t.url_text.clone()).collect();
                s.graphs = Some(vec!["default".into()]);
                s.warnings = m.warnings;
            }
            MappingKind::Template => {
                let t = crate::tabular::Template::parse(&self.text, None)?;
                s.triples_maps = t.len();
                s.graphs = Some(vec!["default".into()]);
            }
        }
        Ok(s)
    }
}

/// The record of a stored mapping's last run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastRun {
    /// RFC 3339 UTC.
    pub time: String,
    pub version: u64,
    /// `add` or `replace`.
    pub mode: String,
    /// The commit the run made (none for a dry run or a run that changed nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<u64>,
    pub quads: u64,
    pub rows: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// One stored version: its metadata and its definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stored {
    #[serde(flatten)]
    pub version: Version,
    pub definition: Definition,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    /// oldest first; the last one is current
    versions: Vec<Stored>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_run: Option<LastRun>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct FileDoc {
    format: u32,
    mappings: BTreeMap<String, Entry>,
}

/// Who saves a version, and the version a client expects to replace.
pub type Change = crate::stored::Change;

/// The outcome of a [`Catalog::put`].
#[derive(Clone, Debug)]
pub struct Saved {
    pub stored: Stored,
    pub summary: Summary,
    /// `false`: the definition equals the current one and no version was added.
    pub changed: bool,
    pub created: bool,
}

/// The stored mappings of one dataset, kept in memory and written to
/// `<root>/mappings.json` (in-memory datasets keep them in memory only).
pub struct Catalog {
    root: Option<PathBuf>,
    doc: RwLock<FileDoc>,
    broken: Option<String>,
}

fn digest(parent: Option<&str>, def: &Definition) -> String {
    let mut h = Sha256::new();
    h.update(parent.unwrap_or("").as_bytes());
    h.update([0u8]);
    h.update(serde_json::to_vec(def).unwrap_or_default());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn empty() -> FileDoc {
    FileDoc {
        format: FORMAT,
        ..Default::default()
    }
}

impl Catalog {
    /// The catalog of a database directory (`None`: in memory). A missing file is an
    /// empty catalog; a malformed one is an error, and a newer format is refused.
    pub fn open(root: Option<&Path>) -> Result<Catalog> {
        let doc = match root {
            Some(r) => match std::fs::read(r.join(FILE)) {
                Ok(b) => {
                    let v: serde_json::Value = serde_json::from_slice(&b)
                        .map_err(|e| Error::invalid(format!("{FILE}: {e}")))?;
                    crate::formats::check_settings(FILE, &v)?;
                    serde_json::from_value(v).map_err(|e| Error::invalid(format!("{FILE}: {e}")))?
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
        })
    }

    /// [`Catalog::open`], or, when the file cannot be read, an empty catalog that
    /// refuses changes (so a malformed file is never overwritten) and names the problem.
    pub fn open_or_broken(root: Option<&Path>) -> Catalog {
        Catalog::open(root).unwrap_or_else(|e| Catalog {
            root: root.map(Path::to_path_buf),
            doc: RwLock::new(empty()),
            broken: Some(e.to_string()),
        })
    }

    pub fn broken(&self) -> Option<&str> {
        self.broken.as_deref()
    }

    fn writable(&self) -> Result<()> {
        match &self.broken {
            Some(e) => Err(Error::Conflict(format!(
                "the stored mappings cannot be changed until {FILE} is fixed or removed: {e}"
            ))),
            None => Ok(()),
        }
    }

    /// The current version of every mapping, by name, with its last run.
    pub fn list(&self) -> Vec<(String, Stored, Option<LastRun>)> {
        self.doc
            .read()
            .mappings
            .iter()
            .filter_map(|(n, e)| Some((n.clone(), e.versions.last()?.clone(), e.last_run.clone())))
            .collect()
    }

    /// The current version of a mapping, or the given version while it is kept.
    pub fn get(&self, name: &str, version: Option<u64>) -> Option<Stored> {
        let doc = self.doc.read();
        let e = doc.mappings.get(name)?;
        match version {
            None => e.versions.last().cloned(),
            Some(v) => e.versions.iter().find(|s| s.version.version == v).cloned(),
        }
    }

    /// The last run of a mapping.
    pub fn last_run(&self, name: &str) -> Option<LastRun> {
        self.doc.read().mappings.get(name)?.last_run.clone()
    }

    /// The kept versions of a mapping, newest first.
    pub fn versions(&self, name: &str) -> Option<Vec<Version>> {
        let doc = self.doc.read();
        let e = doc.mappings.get(name)?;
        Some(e.versions.iter().rev().map(|s| s.version.clone()).collect())
    }

    fn save(&self, doc: &FileDoc) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let path = root.join(FILE);
        if doc.mappings.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        let bytes = serde_json::to_vec_pretty(doc).map_err(|e| Error::Io(e.into()))?;
        crate::store::write_atomic(&path, &bytes)?;
        Ok(())
    }

    /// Store a checked definition as the mapping's next version. An unchanged
    /// definition adds no version.
    pub fn put(&self, name: &str, def: Definition, change: Change) -> Result<Saved> {
        if !crate::stored::valid_name(name) {
            return Err(Error::invalid(format!(
                "invalid mapping name '{name}': use letters, digits, _ and -, at most {} characters",
                crate::stored::MAX_NAME
            )));
        }
        self.writable()?;
        let summary = def.check()?;
        let mut doc = self.doc.write();
        let current = doc
            .mappings
            .get(name)
            .and_then(|e| e.versions.last())
            .cloned();
        let have = current.as_ref().map_or(0, |c| c.version.version);
        if let Some(want) = change.if_version
            && want != have
        {
            return Err(Error::PreconditionFailed(format!(
                "mapping '{name}' is at version {have}, not {want}"
            )));
        }
        if let Some(c) = &current
            && c.definition == def
        {
            return Ok(Saved {
                stored: c.clone(),
                summary,
                changed: false,
                created: false,
            });
        }
        let version = Version {
            version: have + 1,
            parent: (have > 0).then_some(have),
            created: crate::builder::now_rfc3339(),
            author: change.author,
            message: change.message,
            dataset_commit: change.dataset_commit,
            digest: digest(current.as_ref().map(|c| c.version.digest.as_str()), &def),
        };
        let stored = Stored {
            version,
            definition: def,
        };
        let mut next = doc.clone();
        let entry = next.mappings.entry(name.to_string()).or_default();
        entry.versions.push(stored.clone());
        if entry.versions.len() > MAX_VERSIONS {
            let drop = entry.versions.len() - MAX_VERSIONS;
            entry.versions.drain(..drop);
        }
        self.save(&next)?;
        *doc = next;
        Ok(Saved {
            stored,
            summary,
            changed: true,
            created: have == 0,
        })
    }

    /// Record the last run of a mapping. A mapping deleted meanwhile is left deleted.
    pub fn record_run(&self, name: &str, run: LastRun) -> Result<()> {
        self.writable()?;
        let mut doc = self.doc.write();
        if !doc.mappings.contains_key(name) {
            return Ok(());
        }
        let mut next = doc.clone();
        if let Some(e) = next.mappings.get_mut(name) {
            e.last_run = Some(run);
        }
        self.save(&next)?;
        *doc = next;
        Ok(())
    }

    /// Remove a mapping and its versions; `false` when there was none.
    pub fn delete(&self, name: &str, if_version: Option<u64>) -> Result<bool> {
        self.writable()?;
        let mut doc = self.doc.write();
        let have = doc
            .mappings
            .get(name)
            .and_then(|e| e.versions.last())
            .map_or(0, |c| c.version.version);
        if let Some(want) = if_version
            && want != have
        {
            return Err(Error::PreconditionFailed(format!(
                "mapping '{name}' is at version {have}, not {want}"
            )));
        }
        if have == 0 {
            return Ok(false);
        }
        let mut next = doc.clone();
        next.mappings.remove(name);
        self.save(&next)?;
        *doc = next;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = "mappings:\n  a:\n    sources: [a.csv~csv]\n    s: http://e/$(a)\n    po:\n      - [http://e/p, $(b)]\n";

    #[test]
    fn versions_runs_and_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let c = Catalog::open(Some(dir.path())).unwrap();
        let def = Definition {
            kind: "yarrrml".into(),
            text: YAML.into(),
            description: None,
        };
        let s = c.put("m", def.clone(), Change::default()).unwrap();
        assert!(s.created && s.changed);
        assert_eq!(s.summary.files, vec!["a.csv".to_string()]);
        assert_eq!(s.summary.graphs, Some(vec!["default".to_string()]));
        // the same definition adds no version
        assert!(!c.put("m", def.clone(), Change::default()).unwrap().changed);
        let mut d2 = def.clone();
        d2.description = Some("x".into());
        assert_eq!(
            c.put("m", d2, Change::default())
                .unwrap()
                .stored
                .version
                .version,
            2
        );
        assert_eq!(c.versions("m").unwrap().len(), 2);
        assert_eq!(c.get("m", Some(1)).unwrap().definition, def);
        // a mapping that does not parse is not stored
        let bad = Definition {
            kind: "yarrrml".into(),
            text: "mappings: 3".into(),
            description: None,
        };
        assert!(c.put("bad", bad, Change::default()).is_err());
        assert!(c.put("bad name", def.clone(), Change::default()).is_err());
        c.record_run(
            "m",
            LastRun {
                time: "t".into(),
                version: 2,
                mode: "add".into(),
                commit: Some(3),
                quads: 1,
                rows: 1,
                warnings: Vec::new(),
                author: None,
            },
        )
        .unwrap();
        // the file reads back
        let c2 = Catalog::open(Some(dir.path())).unwrap();
        assert_eq!(c2.last_run("m").unwrap().commit, Some(3));
        assert_eq!(c2.list().len(), 1);
        // a newer format is refused
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE)).unwrap()).unwrap();
        v["format"] = 99.into();
        std::fs::write(dir.path().join(FILE), v.to_string()).unwrap();
        assert!(matches!(
            Catalog::open(Some(dir.path())),
            Err(Error::Unsupported(_))
        ));
        let broken = Catalog::open_or_broken(Some(dir.path()));
        assert!(broken.broken().is_some());
        assert!(broken.delete("m", None).is_err());
        // deleting the last mapping removes the file
        std::fs::remove_file(dir.path().join(FILE)).unwrap();
        let c3 = Catalog::open(Some(dir.path())).unwrap();
        c3.put("m", def, Change::default()).unwrap();
        assert!(c3.delete("m", None).unwrap());
        assert!(!dir.path().join(FILE).exists());
    }
}
