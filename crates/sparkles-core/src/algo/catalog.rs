//! Named projections (spec F14 §2.4): the definitions of a dataset's projections, kept in
//! `<db>/algo.json` (in-memory datasets keep them in memory only).
//!
//! A definition holds the projection parameters of a call, with IRIs written out in
//! full. `algo:projection "name"` in a `SERVICE algo:…` block stands for it. The
//! built projections are cached by [`super::cache`], not here.

use crate::error::{Error, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The file of a database directory that holds its named projections.
pub const FILE: &str = "algo.json";

/// Version of the file's JSON shape. Format 2 added `deleted`. A format 1 file reads
/// as format 2 with no deleted projections.
pub const FORMAT: u32 = 2;

/// Longest projection name.
pub const MAX_NAME: usize = 64;

fn invalid(m: impl Into<String>) -> Error {
    Error::invalid(m)
}

/// A predicate of a projection, with the direction it is read in if it has its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredicateDef {
    pub iri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

impl PredicateDef {
    pub fn new(iri: impl Into<String>, direction: Option<&str>) -> PredicateDef {
        PredicateDef {
            iri: iri.into(),
            direction: direction.map(str::to_string),
        }
    }

    pub fn iri(&self) -> &str {
        &self.iri
    }

    pub fn direction(&self) -> Option<&str> {
        self.direction.as_deref()
    }
}

/// What a client stores: the parameters of a projection.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectionDef {
    /// the predicates whose triples are edges; none: every predicate between resources
    /// except `rdf:type`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicates: Option<Vec<PredicateDef>>,
    /// `forward`, `backward` or `undirected`; none: the algorithm's own
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_classes: Vec<String>,
    /// the property of an edge's reifier that holds its weight
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_weight: Option<f64>,
    /// `none`, `sum`, `max` or `count`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<String>,
}

impl ProjectionDef {
    /// A definition from JSON, where a bare string in `predicates` stands for that
    /// predicate with the algorithm's direction.
    pub fn from_json(mut v: serde_json::Value) -> Result<ProjectionDef> {
        Self::bare_predicates(&mut v);
        serde_json::from_value(v).map_err(|e| invalid(format!("invalid definition: {e}")))
    }

    /// Rewrite the bare strings of a JSON definition's `predicates` as objects.
    pub fn bare_predicates(v: &mut serde_json::Value) {
        if let Some(ps) = v.get_mut("predicates").and_then(|p| p.as_array_mut()) {
            for p in ps {
                if let serde_json::Value::String(s) = p {
                    *p = serde_json::json!({ "iri": s });
                }
            }
        }
    }

    /// Check the definition: absolute IRIs and known words.
    pub fn check(&self) -> Result<()> {
        let iri = |what: &str, i: &str| {
            oxiri::Iri::parse(i)
                .map(|_| ())
                .map_err(|_| invalid(format!("{what} '{i}' is not an absolute IRI")))
        };
        let dir = |d: &str| match d {
            "forward" | "backward" | "undirected" => Ok(()),
            _ => Err(invalid(format!(
                "direction '{d}' is forward, backward or undirected"
            ))),
        };
        if self.predicates.as_ref().is_some_and(Vec::is_empty) {
            return Err(invalid(
                "predicates must name at least one predicate; leave it out for every predicate but rdf:type",
            ));
        }
        for p in self.predicates.iter().flatten() {
            iri("predicate", p.iri())?;
            if let Some(d) = p.direction() {
                dir(d)?;
            }
        }
        if let Some(d) = &self.direction {
            dir(d)?;
        }
        for c in &self.node_classes {
            iri("node class", c)?;
        }
        if let Some(w) = &self.weight {
            iri("weight property", w)?;
        }
        if let Some(w) = self.default_weight
            && (!w.is_finite() || w < 0.0)
        {
            return Err(invalid("defaultWeight is a non-negative number"));
        }
        if self.default_weight.is_some() && self.weight.is_none() {
            return Err(invalid("defaultWeight needs weight"));
        }
        if let Some(a) = &self.aggregate
            && !matches!(a.as_str(), "none" | "sum" | "max" | "count")
        {
            return Err(invalid("aggregate is none, sum, max or count"));
        }
        Ok(())
    }
}

/// A projection name: `[A-Za-z0-9_-]`, starting with a letter or digit, at most
/// [`MAX_NAME`] characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// A definition with its revision and when it was saved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stored {
    /// 1 for a new projection, one more for each replacement
    pub revision: u64,
    /// RFC 3339
    pub updated: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub definition: ProjectionDef,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileDoc {
    #[serde(default = "default_format")]
    format: u32,
    #[serde(default)]
    projections: BTreeMap<String, Stored>,
    /// The last revision of each deleted projection, so that a projection created
    /// again under the same name continues its revisions. A client that holds a
    /// revision of the deleted projection cannot mistake the new one for it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    deleted: BTreeMap<String, u64>,
}

fn default_format() -> u32 {
    FORMAT
}

/// The named projections of one dataset.
pub struct Catalog {
    root: Option<PathBuf>,
    doc: RwLock<FileDoc>,
    /// why the file could not be read; changes are refused
    broken: Option<String>,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field("projections", &self.doc.read().projections.len())
            .finish()
    }
}

impl Catalog {
    /// An empty catalog that is kept in memory.
    pub fn memory() -> Catalog {
        Catalog::open_or_broken(None)
    }

    /// The catalog of a database directory (`None`: in memory). A missing file is an
    /// empty catalog; a malformed one is an error.
    pub fn open(root: Option<&Path>) -> Result<Catalog> {
        let doc = match root {
            Some(r) => match std::fs::read(r.join(FILE)) {
                Ok(b) => {
                    let value: serde_json::Value =
                        serde_json::from_slice(&b).map_err(|e| invalid(format!("{FILE}: {e}")))?;
                    crate::formats::check_settings(FILE, &value)?;
                    // Probe compatibility first, then validate untouched bytes so
                    // duplicate known fields are not discarded by Value parsing.
                    let d: FileDoc =
                        serde_json::from_slice(&b).map_err(|e| invalid(format!("{FILE}: {e}")))?;
                    if let Some(n) = d.deleted.keys().find(|n| !valid_name(n)) {
                        return Err(invalid(format!("{FILE}: invalid projection name '{n}'")));
                    }
                    for (n, p) in &d.projections {
                        if !valid_name(n) {
                            return Err(invalid(format!("{FILE}: invalid projection name '{n}'")));
                        }
                        p.definition
                            .check()
                            .map_err(|e| invalid(format!("{FILE}: '{n}': {e}")))?;
                    }
                    d
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileDoc {
                    format: FORMAT,
                    ..Default::default()
                },
                Err(e) => return Err(e.into()),
            },
            None => FileDoc {
                format: FORMAT,
                ..Default::default()
            },
        };
        Ok(Catalog {
            root: root.map(Path::to_path_buf),
            doc: RwLock::new(doc),
            broken: None,
        })
    }

    /// [`Catalog::open`], or, when the file cannot be read, an empty catalog that refuses
    /// changes (so a malformed file is never overwritten) and names the problem.
    pub fn open_or_broken(root: Option<&Path>) -> Catalog {
        Catalog::open(root).unwrap_or_else(|e| Catalog {
            root: root.map(Path::to_path_buf),
            doc: RwLock::new(FileDoc {
                format: FORMAT,
                ..Default::default()
            }),
            broken: Some(e.to_string()),
        })
    }

    /// Why the file could not be read, if it could not.
    pub fn broken(&self) -> Option<&str> {
        self.broken.as_deref()
    }

    fn writable(&self) -> Result<()> {
        match &self.broken {
            Some(e) => Err(Error::Conflict(format!(
                "the named projections cannot be changed until {FILE} is fixed or removed: {e}"
            ))),
            None => Ok(()),
        }
    }

    /// Every definition, by name.
    pub fn list(&self) -> Vec<(String, Stored)> {
        self.doc
            .read()
            .projections
            .iter()
            .map(|(n, d)| (n.clone(), d.clone()))
            .collect()
    }

    /// A definition with its revision.
    pub fn get_stored(&self, name: &str) -> Option<Stored> {
        self.doc.read().projections.get(name).cloned()
    }

    /// The definition of a projection.
    pub fn get(&self, name: &str) -> Option<ProjectionDef> {
        self.get_stored(name).map(|s| s.definition)
    }

    fn save(&self, doc: &FileDoc) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let path = root.join(FILE);
        if doc.projections.is_empty() && doc.deleted.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        let bytes = crate::formats::settings_to_vec(FILE, doc)?;
        crate::store::write_atomic(&path, &bytes)?;
        Ok(())
    }

    /// Create or replace a definition, if its revision is `if_revision` when that is
    /// given (0: it must not exist). Returns the stored definition and whether it is new.
    pub fn put(
        &self,
        name: &str,
        def: ProjectionDef,
        author: Option<String>,
        if_revision: Option<u64>,
    ) -> Result<(Stored, bool)> {
        if !valid_name(name) {
            return Err(invalid(format!(
                "invalid projection name '{name}': use letters, digits, _ and -, at most {MAX_NAME} characters"
            )));
        }
        self.writable()?;
        def.check()?;
        let mut doc = self.doc.write();
        let have = doc.projections.get(name).map_or(0, |s| s.revision);
        if let Some(want) = if_revision
            && want != have
        {
            return Err(Error::PreconditionFailed(format!(
                "projection '{name}' is at revision {have}, not {want}"
            )));
        }
        // a name used before continues from the last revision it had
        let last = have.max(doc.deleted.get(name).copied().unwrap_or(0));
        let stored = Stored {
            revision: last + 1,
            updated: crate::guard::config::now_rfc3339(),
            author,
            definition: def,
        };
        let mut next = doc.clone();
        next.projections.insert(name.to_string(), stored.clone());
        next.deleted.remove(name);
        self.save(&next)?;
        *doc = next;
        Ok((stored, have == 0))
    }

    /// Remove a definition; whether it existed.
    pub fn delete(&self, name: &str, if_revision: Option<u64>) -> Result<bool> {
        self.writable()?;
        let mut doc = self.doc.write();
        if let (Some(want), Some(have)) = (if_revision, doc.projections.get(name))
            && want != have.revision
        {
            return Err(Error::PreconditionFailed(format!(
                "projection '{name}' is at revision {}, not {want}",
                have.revision
            )));
        }
        let mut next = doc.clone();
        let removed = next.projections.remove(name);
        let had = removed.is_some();
        if let Some(gone) = removed {
            next.deleted.insert(name.to_string(), gone.revision);
            self.save(&next)?;
            *doc = next;
        }
        Ok(had)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> ProjectionDef {
        ProjectionDef {
            predicates: Some(vec![
                PredicateDef::new("http://ex/knows", None),
                PredicateDef::new("http://ex/likes", Some("backward")),
            ]),
            direction: Some("undirected".into()),
            node_classes: vec!["http://ex/Person".into()],
            weight: Some("http://ex/w".into()),
            default_weight: Some(1.0),
            aggregate: Some("sum".into()),
        }
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = Catalog::open(Some(dir.path())).unwrap();
        assert!(c.put("people", def(), None, None).unwrap().1);
        let (st, new) = c.put("people", def(), Some("kc".into()), Some(1)).unwrap();
        assert!(!new);
        assert_eq!(st.revision, 2);
        assert!(c.put("people", def(), None, Some(1)).is_err());
        let again = Catalog::open(Some(dir.path())).unwrap();
        assert_eq!(again.get("people"), Some(def()));
        assert!(again.delete("people", Some(2)).unwrap());
        assert!(again.list().is_empty());
        // the file keeps the last revision of the deleted projection
        let c = Catalog::open(Some(dir.path())).unwrap();
        let (st, new) = c.put("people", def(), None, Some(0)).unwrap();
        assert!(new);
        assert_eq!(
            st.revision, 3,
            "a projection created again continues its revisions"
        );
        assert!(c.put("people", def(), None, Some(2)).is_err());
        assert_eq!(c.put("people", def(), None, Some(3)).unwrap().0.revision, 4);
        // in memory too
        let m = Catalog::memory();
        m.put("p", def(), None, None).unwrap();
        m.delete("p", None).unwrap();
        assert_eq!(m.put("p", def(), None, None).unwrap().0.revision, 2);
    }

    #[test]
    fn a_format_1_file_reads_and_is_written_as_format_2() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FILE),
            br#"{"format":1,"projections":{"keep":{"revision":4,"updated":"fixture","definition":{}}}}"#,
        )
        .unwrap();
        let c = Catalog::open(Some(dir.path())).unwrap();
        assert_eq!(c.get_stored("keep").unwrap().revision, 4);
        assert!(c.delete("keep", Some(4)).unwrap());
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE)).unwrap()).unwrap();
        assert_eq!(doc["format"], 2);
        assert_eq!(doc["deleted"]["keep"], 4);
        let c = Catalog::open(Some(dir.path())).unwrap();
        assert_eq!(c.put("keep", def(), None, None).unwrap().0.revision, 5);
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE)).unwrap()).unwrap();
        assert!(doc.get("deleted").is_none());
        // a deleted name is checked like a projection name
        std::fs::write(
            dir.path().join(FILE),
            br#"{"format":2,"deleted":{"bad name":1}}"#,
        )
        .unwrap();
        assert!(Catalog::open(Some(dir.path())).is_err());
    }

    #[test]
    fn rejects_bad_definitions_and_files() {
        let c = Catalog::memory();
        assert!(c.put("bad name", def(), None, None).is_err());
        let mut d = def();
        d.direction = Some("sideways".into());
        assert!(c.put("x", d, None, None).is_err());
        let empty = ProjectionDef {
            predicates: Some(vec![]),
            ..Default::default()
        };
        assert!(c.put("x", empty, None, None).is_err());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), b"{\"format\":1,\"extra\":1}").unwrap();
        let c = Catalog::open_or_broken(Some(dir.path()));
        assert!(c.broken().is_some());
        assert!(
            c.put("x", def(), None, None).is_err(),
            "a broken file is not overwritten"
        );
    }
}

#[cfg(test)]
mod algo_catalog_format_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn algo_catalog_future_format_precedes_shape_validation() {
        let dir = tempfile::tempdir().unwrap();
        let mut wrong = Vec::new();
        for doc in [
            json!({"format":FORMAT + 1,"projections":{}}),
            json!({"format":FORMAT + 1,"unknownFutureShape":[1,2]}),
        ] {
            std::fs::write(dir.path().join(FILE), serde_json::to_vec(&doc).unwrap()).unwrap();
            let result = Catalog::open(Some(dir.path()));
            if !matches!(&result, Err(Error::Unsupported(_))) {
                wrong.push(format!("{doc}: {result:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "future formats lost typed Unsupported: {wrong:?}"
        );
    }
    #[test]
    fn algo_catalog_invalid_versions_use_registry_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut wrong = Vec::new();
        for marker in [json!(0), json!("next"), json!(4294967296u64)] {
            let doc = json!({"format":marker,"projections":{}});
            std::fs::write(dir.path().join(FILE), serde_json::to_vec(&doc).unwrap()).unwrap();
            let result = Catalog::open(Some(dir.path()));
            if !matches!(&result, Err(Error::Corrupt(_))) {
                wrong.push(format!("{doc}: {result:?}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "invalid version markers bypassed registry: {wrong:?}"
        );
    }
    #[test]
    fn algo_catalog_versionless_read_does_not_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(Some(dir.path())).unwrap();
        let (stored, _) = catalog
            .put(
                "people",
                ProjectionDef::default(),
                Some("fixture".into()),
                None,
            )
            .unwrap();
        drop(catalog);
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE)).unwrap()).unwrap();
        doc.as_object_mut().unwrap().remove("format");
        let bytes = serde_json::to_vec(&doc).unwrap();
        std::fs::write(dir.path().join(FILE), &bytes).unwrap();
        let catalog = Catalog::open(Some(dir.path())).unwrap();
        assert_eq!(catalog.get_stored("people"), Some(stored));
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), bytes);
        catalog
            .put("people", ProjectionDef::default(), None, Some(1))
            .unwrap();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(FILE)).unwrap()).unwrap();
        assert_eq!(doc["format"], FORMAT);
    }
    #[test]
    fn algo_catalog_null_version_stays_broken_without_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let value = json!({"format":null,"projections":{}});
        let bytes = serde_json::to_vec(&value).unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, &bytes).unwrap();
        // Compatibility probing is not typed schema acceptance. The existing
        // catalog reader rejects an explicit null rather than defaulting it.
        assert_eq!(crate::formats::check_settings(FILE, &value).unwrap(), 1);
        crate::formats::check_dataset_dir(dir.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(matches!(
            Catalog::open(Some(dir.path())),
            Err(Error::Invalid(_))
        ));
        let catalog = Catalog::open_or_broken(Some(dir.path()));
        assert!(catalog.broken().is_some());
        assert!(catalog.list().is_empty());
        assert!(matches!(
            catalog.put("valid", ProjectionDef::default(), None, None),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            catalog.delete("valid", None),
            Err(Error::Conflict(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn algo_catalog_missing_current_and_damaged_controls() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Catalog::open(Some(dir.path())).unwrap().list().is_empty());
        assert!(!dir.path().join(FILE).exists());
        let c = Catalog::open(Some(dir.path())).unwrap();
        let (stored, _) = c
            .put("people", ProjectionDef::default(), None, None)
            .unwrap();
        drop(c);
        let bytes = std::fs::read(dir.path().join(FILE)).unwrap();
        assert_eq!(
            Catalog::open(Some(dir.path()))
                .unwrap()
                .get_stored("people"),
            Some(stored)
        );
        assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), bytes);
        for bytes in [b"{".as_slice(),br#"{"format":1,"extra":true}"#,
            br#"{"format":1,"projections":{"bad name":{"revision":1,"updated":"fixture","definition":{}}}}"#,
            br#"{"format":1,"projections":{"bad":{"revision":1,"updated":"fixture","definition":{"direction":"sideways"}}}}"#] {
            std::fs::write(dir.path().join(FILE),bytes).unwrap();
            let c=Catalog::open_or_broken(Some(dir.path()));assert!(c.broken().is_some());
            assert!(matches!(c.put("valid",ProjectionDef::default(),None,None),Err(Error::Conflict(_))));
            assert!(matches!(c.delete("valid",None),Err(Error::Conflict(_))));
            assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(),bytes);
        }
    }
}

#[cfg(test)]
mod duplicate_field_tests {
    use super::*;

    // These are raw JSON field sequences. Going through Value would discard the
    // duplicate before Catalog sees it, which is the defect under test.
    fn documents() -> Vec<(String, String)> {
        const KEPT: &str = r#"{"keep":{"revision":1,"updated":"fixture","definition":{}}}"#;
        let mut fields = vec![
            (
                "FileDoc.projections.same".into(),
                format!(r#""projections":{KEPT},"projections":{KEPT}"#),
            ),
            (
                "FileDoc.projections.keep-then-empty".into(),
                format!(r#""projections":{KEPT},"projections":{{}}"#),
            ),
        ];
        for (name, first, second, extra) in [
            (
                "revision",
                "1",
                "2",
                r#""updated":"fixture","definition":{}"#,
            ),
            (
                "updated",
                r#""first""#,
                r#""second""#,
                r#""revision":1,"definition":{}"#,
            ),
            (
                "author",
                r#""first""#,
                r#""second""#,
                r#""revision":1,"updated":"fixture","definition":{}"#,
            ),
            (
                "definition",
                "{}",
                r#"{"direction":"forward"}"#,
                r#""revision":1,"updated":"fixture""#,
            ),
        ] {
            let mut pairs = vec![("same", first, first), ("different", first, second)];
            if name == "author" {
                pairs.extend([("null-first", "null", first), ("null-last", first, "null")]);
            }
            for (case, a, b) in pairs {
                fields.push((
                    format!("Stored.{name}.{case}"),
                    format!(r#""projections":{{"keep":{{{extra},"{name}":{a},"{name}":{b}}}}}"#),
                ));
            }
        }
        for (name, first, second, optional, extra) in [
            (
                "predicates",
                r#"[{"iri":"urn:p"}]"#,
                r#"[{"iri":"urn:q"}]"#,
                true,
                "",
            ),
            ("direction", r#""forward""#, r#""backward""#, true, ""),
            ("nodeClasses", r#"["urn:C"]"#, r#"["urn:D"]"#, false, ""),
            ("weight", r#""urn:w""#, r#""urn:x""#, true, ""),
            ("defaultWeight", "1", "2", true, r#", "weight":"urn:w""#),
            ("aggregate", r#""sum""#, r#""max""#, true, ""),
        ] {
            let mut pairs = vec![("same", first, first), ("different", first, second)];
            if optional {
                pairs.extend([("null-first", "null", first), ("null-last", first, "null")]);
            }
            for (case, a, b) in pairs {
                fields.push((format!("ProjectionDef.{name}.{case}"), format!(r#""projections":{{"keep":{{"revision":1,"updated":"fixture","definition":{{"{name}":{a},"{name}":{b}{extra}}}}}}}"#)));
            }
        }
        for (name, first, second, extra) in [
            ("iri", r#""urn:p""#, r#""urn:q""#, ""),
            (
                "direction",
                r#""forward""#,
                r#""backward""#,
                r#", "iri":"urn:p""#,
            ),
        ] {
            let mut pairs = vec![("same", first, first), ("different", first, second)];
            if name == "direction" {
                pairs.extend([("null-first", "null", first), ("null-last", first, "null")]);
            }
            for (case, a, b) in pairs {
                fields.push((format!("PredicateDef.{name}.{case}"), format!(r#""projections":{{"keep":{{"revision":1,"updated":"fixture","definition":{{"predicates":[{{"{name}":{a},"{name}":{b}{extra}}}]}}}}}}"#)));
            }
        }
        let mut docs = Vec::new();
        for (name, body) in fields {
            for (marker, prefix) in [("current", r#""format":1,"#), ("absent", "")] {
                docs.push((format!("{name}.{marker}"), format!("{{{prefix}{body}}}")));
            }
        }
        docs.extend([
            (
                "FileDoc.format.same".into(),
                r#"{"format":1,"format":1,"projections":{}}"#.into(),
            ),
            (
                "FileDoc.format.invalid-then-current".into(),
                r#"{"format":0,"format":1,"projections":{}}"#.into(),
            ),
        ]);
        docs
    }

    #[test]
    fn duplicate_known_fields_are_invalid() {
        let mut accepted = Vec::new();
        for (name, raw) in documents() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(FILE), raw.as_bytes()).unwrap();
            let result = Catalog::open(Some(dir.path()));
            if !matches!(result, Err(Error::Invalid(_))) {
                eprintln!("duplicate accepted instead of Invalid: {name}: {result:?}; raw={raw}");
                accepted.push(name);
            }
        }
        assert!(
            accepted.is_empty(),
            "raw duplicate fields were accepted: {accepted:?}"
        );
    }

    #[test]
    fn duplicate_known_fields_stay_read_only() {
        let mut writable = Vec::new();
        for (name, raw) in documents() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(FILE);
            std::fs::write(&path, raw.as_bytes()).unwrap();
            let permissions = std::fs::metadata(&path).unwrap().permissions();
            let entries = || {
                let mut names: Vec<_> = std::fs::read_dir(dir.path())
                    .unwrap()
                    .map(|e| e.unwrap().file_name())
                    .collect();
                names.sort();
                names
            };
            let before = entries();
            let unchanged = || {
                std::fs::read(&path).is_ok_and(|bytes| bytes == raw.as_bytes())
                    && std::fs::metadata(&path).is_ok_and(|m| m.permissions() == permissions)
                    && entries() == before
            };
            let c = Catalog::open_or_broken(Some(dir.path()));
            let broken =
                c.broken().is_some_and(|e| !e.is_empty()) && c.list().is_empty() && unchanged();
            let put = matches!(
                c.put("new", ProjectionDef::default(), None, None),
                Err(Error::Conflict(_))
            ) && unchanged();
            let delete = matches!(c.delete("keep", None), Err(Error::Conflict(_))) && unchanged();
            let missing =
                matches!(c.delete("missing", None), Err(Error::Conflict(_))) && unchanged();
            if !(broken && put && delete && missing) {
                eprintln!(
                    "duplicate fallback not read-only: {name}: broken={broken}, put={put}, delete={delete}, missing={missing}"
                );
                writable.push(name);
            }
        }
        assert!(
            writable.is_empty(),
            "damaged catalog writable: {writable:?}"
        );
    }

    #[test]
    fn future_duplicate_body_keeps_compatibility_precedence() {
        for raw in [
            r#"{"format":3,"projections":{},"projections":{"future":null},"unknownFutureShape":true}"#,
            r#"{"format":1,"format":3,"projections":{},"projections":{}}"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(FILE);
            std::fs::write(&path, raw).unwrap();
            assert!(matches!(
                Catalog::open(Some(dir.path())),
                Err(Error::Unsupported(_))
            ));
            assert_eq!(std::fs::read(&path).unwrap(), raw.as_bytes());
        }
    }
}
