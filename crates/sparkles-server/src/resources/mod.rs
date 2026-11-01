//! The resource catalog of spec C30, phase 1: a server-level catalog of versioned
//! sources, vocabularies and datasets, kept in `<dataDir>/resources.json`.
//!
//! A resource has a kind, an ownership policy, a licence and versions. A version cannot
//! change once it is added, and its imports and graph sources pin versions that already
//! exist, so references never form a cycle and a pin always names the same record.
//! Removing a referenced version or resource is refused. Every write checks the whole
//! catalog and writes the file atomically. A file of a format this build does not read,
//! or one that does not parse, is never rewritten: every operation reports it instead.
//!
//! [`license`] classifies licences, [`report`] propagates their obligations and writes
//! the NOTICE, [`rdf`] describes the catalog in DCAT 3, PROV-O, SPDX, VANN and VoID, and
//! [`http`] serves the routes.

pub mod http;
pub mod license;
pub mod rdf;
pub mod report;
#[cfg(test)]
mod tests;

use license::Class;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The catalog's file in the data directory.
pub const FILE: &str = "resources.json";

/// The most resources a catalog holds.
pub const MAX_RESOURCES: usize = 4096;
/// The most versions of one resource.
pub const MAX_VERSIONS: usize = 1000;
/// The most namespaces, graph locations, distributions, imports, graphs or graph sources
/// of one record.
pub const MAX_LIST: usize = 256;
/// The largest catalog file.
pub const MAX_FILE_BYTES: usize = 16 << 20;

/// What a resource is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Source,
    Vocabulary,
    Dataset,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Source => "source",
            Kind::Vocabulary => "vocabulary",
            Kind::Dataset => "dataset",
        }
    }
}

/// Who may change a resource's data. This build keeps owned resources only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ownership {
    #[default]
    Owned,
    Mirrored,
    Synced,
}

/// Who did something, and when (RFC 3339, milliseconds, UTC).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub by: String,
    pub at: String,
}

impl Stamp {
    pub fn now(by: &str) -> Stamp {
        Stamp {
            by: by.to_string(),
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }
    }
}

/// An administrator's acceptance of a resource's restricted terms.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Acceptance {
    pub by: String,
    pub at: String,
    /// the licences of the resource and its versions when they were accepted
    pub licenses: Vec<String>,
}

/// A namespace that a vocabulary owns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Namespace {
    pub prefix: String,
    pub iri: String,
}

/// A graph of a server dataset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub dataset: String,
    pub graph: String,
}

/// A pinned reference to a version of a resource.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub resource: String,
    pub version: String,
}

/// A file a version is published as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Distribution {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_size: Option<u64>,
    /// the SHA-256 digest of the file, in lowercase hexadecimal
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// A graph of a dataset version and the source versions it was built from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphRecord {
    pub graph: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Pin>,
}

/// The members of a version that its writer gives.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionInput {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_class: Option<Class>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub distributions: Vec<Distribution>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<Pin>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphs: Vec<GraphRecord>,
}

/// A version as the catalog keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    #[serde(flatten)]
    pub input: VersionInput,
    pub added: Stamp,
}

/// The descriptive members of a resource that its writer gives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInput {
    pub kind: Kind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default)]
    pub ownership: Ownership,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_class: Option<Class>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license_url: Option<String>,
    /// the attribution text the licence asks for, written into the NOTICE
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// the namespaces a vocabulary owns
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub namespaces: Vec<Namespace>,
    /// the dataset graphs that hold a vocabulary
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphs: Vec<Location>,
    /// the server dataset a dataset resource describes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset: Option<String>,
}

/// A resource as the catalog keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub id: String,
    #[serde(flatten)]
    pub input: ResourceInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<Acceptance>,
    #[serde(default)]
    pub versions: Vec<Version>,
    pub created: Stamp,
    pub modified: Stamp,
}

impl Resource {
    pub fn version(&self, v: &str) -> Option<&Version> {
        self.versions.iter().find(|x| x.input.version == v)
    }

    /// The version added last.
    pub fn current(&self) -> Option<&Version> {
        self.versions.last()
    }

    /// The licence of a version: its own, else the resource's.
    pub fn licence_of<'a>(&'a self, v: Option<&'a Version>) -> license::Licence<'a> {
        match v {
            Some(v) if v.input.license.is_some() || v.input.license_class.is_some() => {
                license::Licence {
                    expression: v.input.license.as_deref(),
                    class: v.input.license_class,
                    url: v.input.license_url.as_deref(),
                }
            }
            _ => license::Licence {
                expression: self.input.license.as_deref(),
                class: self.input.license_class,
                url: self.input.license_url.as_deref(),
            },
        }
    }

    /// The licences of the resource and its versions, as an acceptance records them.
    pub fn licences(&self) -> Vec<String> {
        let show = |l: license::Licence| {
            format!(
                "{} [{}]",
                l.expression.unwrap_or("NOASSERTION"),
                l.class().map_or("unknown", Class::as_str)
            )
        };
        let mut set = BTreeSet::new();
        set.insert(show(self.licence_of(None)));
        for v in &self.versions {
            set.insert(show(self.licence_of(Some(v))));
        }
        set.into_iter().collect()
    }

    /// Whether the resource or one of its versions has restricted terms.
    pub fn restricted(&self) -> bool {
        std::iter::once(None)
            .chain(self.versions.iter().map(Some))
            .any(|v| self.licence_of(v).class() == Ok(Class::Restricted))
    }

    /// Whether an administrator accepted the resource's current terms.
    pub fn accepted(&self) -> bool {
        self.acceptance
            .as_ref()
            .is_some_and(|a| a.licenses == self.licences())
    }

    /// The entity tag of the resource's JSON.
    pub fn etag(&self) -> String {
        use sha2::Digest;
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        let d = sha2::Sha256::digest(&bytes);
        let hex: String = d[..12].iter().map(|b| format!("{b:02x}")).collect();
        format!("\"{hex}\"")
    }

    /// The pins of a version: its imports and its graphs' sources.
    pub fn pins(v: &Version) -> impl Iterator<Item = &Pin> {
        v.input
            .imports
            .iter()
            .chain(v.input.graphs.iter().flat_map(|g| g.sources.iter()))
    }
}

/// The catalog file. Its records flatten their writers' members, so serde cannot deny
/// unknown members in them. The routes check request bodies member by member instead.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogFile {
    #[serde(default)]
    pub resources: Vec<Resource>,
}

/// Why a catalog operation failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// the file has a format this build does not read, or does not parse
    Unreadable(String),
    /// no such resource or version
    NotFound(&'static str, String),
    /// the request breaks a rule of the catalog
    Invalid(String),
    /// the request conflicts with the catalog's state
    Conflict(&'static str, String),
    /// `If-Match` names another state of the resource
    Precondition(String),
    /// a limit of the catalog
    Limit(String),
    /// the file could not be written
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;

fn invalid(m: impl Into<String>) -> Error {
    Error::Invalid(m.into())
}

// -------------------------------------------------------------------- checks ------

/// A resource id: 1 to 64 letters, digits, `.`, `_` and `-`, starting with a letter or
/// digit.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A version string: 1 to 64 letters, digits, `.`, `_`, `+` and `-`.
pub fn valid_version(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

fn text(what: &str, s: &str, max: usize, required: bool) -> Result<()> {
    if required && s.trim().is_empty() {
        return Err(invalid(format!("{what} is empty")));
    }
    if s.len() > max {
        return Err(invalid(format!("{what} is longer than {max} bytes")));
    }
    if let Some(c) = s
        .chars()
        .find(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err(invalid(format!(
            "{what} contains the control character U+{:04X}",
            c as u32
        )));
    }
    Ok(())
}

fn iri(what: &str, s: &str) -> Result<()> {
    if s.len() > 4096 || oxrdf::NamedNode::new(s).is_err() {
        return Err(invalid(format!("{what}: {s:?} is not an absolute IRI")));
    }
    Ok(())
}

fn list<T>(what: &str, v: &[T]) -> Result<()> {
    if v.len() > MAX_LIST {
        return Err(Error::Limit(format!(
            "{what} has {} entries, more than the {MAX_LIST} allowed",
            v.len()
        )));
    }
    Ok(())
}

fn licence(what: &str, l: &license::Licence) -> Result<()> {
    if let Some(u) = l.url {
        iri(&format!("{what}Url"), u)?;
    }
    if l.expression.is_none() && l.class.is_some() {
        return Err(invalid(format!(
            "{what}Class needs {what}, the licence it classifies"
        )));
    }
    l.class()
        .map(|_| ())
        .map_err(|e| invalid(format!("{what}: {e}")))
}

fn media_type(s: &str) -> bool {
    let token = |t: &str| {
        !t.is_empty()
            && t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
    };
    s.len() <= 255 && s.split_once('/').is_some_and(|(a, b)| token(a) && token(b))
}

fn date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
        || chrono::DateTime::parse_from_rfc3339(s).is_ok()
}

/// Check the descriptive members of a resource.
pub fn check_input(r: &ResourceInput) -> Result<()> {
    text("title", &r.title, 512, true)?;
    if let Some(d) = &r.description {
        text("description", d, 8192, false)?;
    }
    if let Some(h) = &r.homepage {
        iri("homepage", h)?;
    }
    if let Some(a) = &r.attribution {
        text("attribution", a, 2048, false)?;
    }
    match r.ownership {
        Ownership::Owned => {}
        Ownership::Mirrored => {
            return Err(invalid(
                "ownership mirrored needs the immutable mirrors of spec C30, which build on the managed graphs of item 81; this build keeps owned resources",
            ));
        }
        Ownership::Synced => {
            return Err(invalid(
                "ownership synced needs the source update monitoring of spec C30, which depends on the durable jobs of item 339; this build keeps owned resources",
            ));
        }
    }
    licence(
        "license",
        &license::Licence {
            expression: r.license.as_deref(),
            class: r.license_class,
            url: r.license_url.as_deref(),
        },
    )?;
    list("namespaces", &r.namespaces)?;
    list("graphs", &r.graphs)?;
    if r.kind != Kind::Vocabulary && !r.namespaces.is_empty() {
        return Err(invalid(format!(
            "namespaces belong to vocabulary resources, not to a {}",
            r.kind.as_str()
        )));
    }
    if r.kind != Kind::Vocabulary && !r.graphs.is_empty() {
        return Err(invalid(format!(
            "graphs lists where a vocabulary is kept; a {} lists its graphs in its versions",
            r.kind.as_str()
        )));
    }
    if r.kind != Kind::Dataset && r.dataset.is_some() {
        return Err(invalid("dataset names the dataset of a dataset resource"));
    }
    if let Some(d) = &r.dataset
        && !sparkles::catalog::valid_name(d)
    {
        return Err(invalid(format!("dataset: {d:?} is not a dataset name")));
    }
    let mut prefixes = BTreeSet::new();
    let mut iris = BTreeSet::new();
    for n in &r.namespaces {
        if !crate::settings::prefixes::valid_name(&n.prefix) {
            return Err(invalid(format!(
                "namespaces: {:?} is not a prefix name",
                n.prefix
            )));
        }
        sparkles::store::check_prefix(&n.prefix, &n.iri)
            .map_err(|e| invalid(format!("namespaces: {e}")))?;
        if !prefixes.insert(&n.prefix) || !iris.insert(&n.iri) {
            return Err(invalid(format!(
                "namespaces: {} or {} is listed twice",
                n.prefix, n.iri
            )));
        }
    }
    let mut locations = BTreeSet::new();
    for l in &r.graphs {
        if !sparkles::catalog::valid_name(&l.dataset) {
            return Err(invalid(format!(
                "graphs: {:?} is not a dataset name",
                l.dataset
            )));
        }
        iri("graphs", &l.graph)?;
        if !locations.insert((&l.dataset, &l.graph)) {
            return Err(invalid(format!(
                "graphs: {} of {} is listed twice",
                l.graph, l.dataset
            )));
        }
    }
    Ok(())
}

/// Check a version's own members, without its references.
pub fn check_version(kind: Kind, v: &VersionInput) -> Result<()> {
    if !valid_version(&v.version) {
        return Err(invalid(format!(
            "version: {:?} is not a version (1 to 64 letters, digits, '.', '_', '+' and '-')",
            v.version
        )));
    }
    if let Some(d) = &v.issued
        && !date(d)
    {
        return Err(invalid(format!(
            "issued: {d:?} is not a date (YYYY-MM-DD) or an RFC 3339 time"
        )));
    }
    if let Some(n) = &v.notes {
        text("notes", n, 8192, false)?;
    }
    licence(
        "license",
        &license::Licence {
            expression: v.license.as_deref(),
            class: v.license_class,
            url: v.license_url.as_deref(),
        },
    )?;
    list("distributions", &v.distributions)?;
    list("imports", &v.imports)?;
    list("graphs", &v.graphs)?;
    for d in &v.distributions {
        iri("distributions.url", &d.url)?;
        if let Some(m) = &d.media_type
            && !media_type(m)
        {
            return Err(invalid(format!(
                "distributions.mediaType: {m:?} is not a media type"
            )));
        }
        if let Some(h) = &d.sha256
            && !(h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(invalid(format!(
                "distributions.sha256: {h:?} is not a SHA-256 digest in hexadecimal"
            )));
        }
    }
    if kind != Kind::Dataset && !v.graphs.is_empty() {
        return Err(invalid(
            "graphs of a version belong to dataset resources; a vocabulary lists the graphs that hold it on the resource",
        ));
    }
    let mut pins = BTreeSet::new();
    for p in &v.imports {
        if !pins.insert(p) {
            return Err(invalid(format!(
                "imports: {} {} is listed twice",
                p.resource, p.version
            )));
        }
    }
    let mut seen = BTreeSet::new();
    for g in &v.graphs {
        iri("graphs.graph", &g.graph)?;
        list("graphs.sources", &g.sources)?;
        let mut sources = BTreeSet::new();
        if let Some(p) = g.sources.iter().find(|p| !sources.insert(*p)) {
            return Err(invalid(format!(
                "graphs.sources of {}: {} {} is listed twice",
                g.graph, p.resource, p.version
            )));
        }
        if !seen.insert(&g.graph) {
            return Err(invalid(format!("graphs: {} is listed twice", g.graph)));
        }
    }
    Ok(())
}

/// Normalize what a check accepts in more than one spelling.
fn normalize_version(v: &mut VersionInput) {
    for d in &mut v.distributions {
        if let Some(h) = &mut d.sha256 {
            h.make_ascii_lowercase();
        }
    }
}

/// Check the whole catalog: each resource, unique ids, namespaces owned once, pins that
/// resolve, and the limits.
pub fn check(c: &CatalogFile) -> Result<()> {
    if c.resources.len() > MAX_RESOURCES {
        return Err(Error::Limit(format!(
            "the catalog would hold {} resources, more than the {MAX_RESOURCES} allowed",
            c.resources.len()
        )));
    }
    let mut ids = BTreeSet::new();
    let mut prefixes = std::collections::BTreeMap::new();
    let mut iris = std::collections::BTreeMap::new();
    for r in &c.resources {
        if !valid_id(&r.id) {
            return Err(invalid(format!(
                "{:?} is not a resource id (1 to 64 letters, digits, '.', '_' and '-', starting with a letter or digit)",
                r.id
            )));
        }
        if !ids.insert(r.id.as_str()) {
            return Err(Error::Conflict(
                "resource-exists",
                format!("the resource {} exists", r.id),
            ));
        }
        check_input(&r.input).map_err(|e| prefix_error(&r.id, e))?;
        if r.versions.len() > MAX_VERSIONS {
            return Err(Error::Limit(format!(
                "{} has {} versions, more than the {MAX_VERSIONS} allowed",
                r.id,
                r.versions.len()
            )));
        }
        let mut versions = BTreeSet::new();
        for v in &r.versions {
            check_version(r.input.kind, &v.input).map_err(|e| prefix_error(&r.id, e))?;
            if !versions.insert(v.input.version.as_str()) {
                return Err(Error::Conflict(
                    "version-exists",
                    format!("{} has the version {} twice", r.id, v.input.version),
                ));
            }
        }
        for n in &r.input.namespaces {
            if let Some(o) = prefixes.insert(n.prefix.as_str(), r.id.as_str()) {
                return Err(Error::Conflict(
                    "namespace-owned",
                    format!(
                        "the vocabularies {o} and {} both claim the prefix {}; a prefix belongs to one vocabulary of the server",
                        r.id, n.prefix
                    ),
                ));
            }
            if let Some(o) = iris.insert(n.iri.as_str(), r.id.as_str()) {
                return Err(Error::Conflict(
                    "namespace-owned",
                    format!(
                        "the vocabularies {o} and {} both claim the namespace {}; a namespace belongs to one vocabulary of the server",
                        r.id, n.iri
                    ),
                ));
            }
        }
    }
    for r in &c.resources {
        for v in &r.versions {
            for p in Resource::pins(v) {
                if find(c, &p.resource)
                    .and_then(|t| t.version(&p.version))
                    .is_none()
                {
                    return Err(Error::NotFound(
                        "no-such-version",
                        format!(
                            "{} {} references {} {}, which the catalog does not hold",
                            r.id, v.input.version, p.resource, p.version
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn prefix_error(id: &str, e: Error) -> Error {
    match e {
        Error::Invalid(m) => Error::Invalid(format!("{id}: {m}")),
        Error::Limit(m) => Error::Limit(format!("{id}: {m}")),
        e => e,
    }
}

pub fn find<'a>(c: &'a CatalogFile, id: &str) -> Option<&'a Resource> {
    c.resources.iter().find(|r| r.id == id)
}

fn find_mut<'a>(c: &'a mut CatalogFile, id: &str) -> Result<&'a mut Resource> {
    c.resources
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or_else(|| no_such(id))
}

pub fn no_such(id: &str) -> Error {
    Error::NotFound(
        "no-such-resource",
        format!("the catalog holds no resource {id}"),
    )
}

/// The versions of other records that reference a version of `id` (or `version` alone).
fn references(c: &CatalogFile, id: &str, version: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    for r in &c.resources {
        for v in &r.versions {
            let hit = Resource::pins(v).any(|p| {
                p.resource == id
                    && version.is_none_or(|x| x == p.version)
                    && !(r.id == id && version.is_none())
            });
            if hit {
                out.push(format!("{} {}", r.id, v.input.version));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn check_match(r: &Resource, if_match: Option<&str>) -> Result<()> {
    match if_match {
        Some(t) if t.trim() != "*" && t.split(',').all(|x| x.trim() != r.etag()) => Err(
            Error::Precondition(format!("{} has changed; read it again", r.id)),
        ),
        _ => Ok(()),
    }
}

// --------------------------------------------------------------------- store ------

/// The catalog of a server: its file, or memory for a server without a data directory.
pub struct Resources {
    path: Option<PathBuf>,
    /// the catalog in memory without a file; with one, the lock of read-modify-write
    mem: Mutex<CatalogFile>,
}

impl Resources {
    /// The catalog of a data directory, or one in memory when `dir` is empty.
    pub fn in_dir(dir: &Path) -> Resources {
        Resources {
            path: (!dir.as_os_str().is_empty()).then(|| dir.join(FILE)),
            mem: Mutex::new(CatalogFile::default()),
        }
    }

    fn load(&self, mem: &CatalogFile) -> Result<CatalogFile> {
        let Some(path) = &self.path else {
            return Ok(mem.clone());
        };
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CatalogFile::default());
            }
            Err(e) => return Err(Error::Unreadable(format!("{}: {e}", path.display()))),
        };
        // read at most one byte past the limit, so an oversized file is refused without
        // reading it whole
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAX_FILE_BYTES as u64 + 1),
            &mut bytes,
        )
        .map_err(|e| Error::Unreadable(format!("{}: {e}", path.display())))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(Error::Unreadable(format!(
                "{} is larger than the {MAX_FILE_BYTES} bytes a catalog may take",
                path.display()
            )));
        }
        let c: CatalogFile = sparkles::formats::settings_from_slice(FILE, &bytes)
            .map_err(|e| Error::Unreadable(format!("{}: {e}", path.display())))?;
        check(&c).map_err(|e| Error::Unreadable(format!("{}: {}", path.display(), message(&e))))?;
        Ok(c)
    }

    /// Read the catalog.
    pub fn read(&self) -> Result<CatalogFile> {
        let mem = self.mem.lock();
        self.load(&mem)
    }

    /// Change the catalog: `f` changes a copy, which is checked whole and written.
    pub fn update<T>(&self, f: impl FnOnce(&mut CatalogFile) -> Result<T>) -> Result<T> {
        let mut mem = self.mem.lock();
        let mut c = self.load(&mem)?;
        let out = f(&mut c)?;
        c.resources.sort_by(|a, b| a.id.cmp(&b.id));
        check(&c)?;
        let bytes =
            sparkles::formats::settings_to_vec(FILE, &c).map_err(|e| Error::Io(e.to_string()))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(Error::Limit(format!(
                "the catalog would take {} bytes, more than the {MAX_FILE_BYTES} allowed",
                bytes.len()
            )));
        }
        match &self.path {
            Some(path) => sparkles::fsutil::write_atomic(path, &bytes)
                .map_err(|e| Error::Io(format!("writing {}: {e}", path.display())))?,
            None => *mem = c,
        }
        Ok(out)
    }

    /// Add a resource, with any initial versions, which may reference each other in
    /// order.
    pub fn create(
        &self,
        id: &str,
        input: ResourceInput,
        versions: Vec<VersionInput>,
        by: &str,
    ) -> Result<Resource> {
        if !valid_id(id) {
            return Err(invalid(format!(
                "{id:?} is not a resource id (1 to 64 letters, digits, '.', '_' and '-', starting with a letter or digit)"
            )));
        }
        check_input(&input)?;
        self.update(|c| {
            if find(c, id).is_some() {
                return Err(Error::Conflict(
                    "resource-exists",
                    format!("the resource {id} exists"),
                ));
            }
            let stamp = Stamp::now(by);
            c.resources.push(Resource {
                id: id.to_string(),
                input,
                acceptance: None,
                versions: Vec::new(),
                created: stamp.clone(),
                modified: stamp,
            });
            for v in versions {
                add_version(c, id, v, by)?;
            }
            Ok(find(c, id).cloned().expect("just added"))
        })
    }

    /// Replace a resource's descriptive members. A change of its licence withdraws the
    /// acceptance of its terms.
    pub fn replace(
        &self,
        id: &str,
        input: ResourceInput,
        if_match: Option<&str>,
        by: &str,
    ) -> Result<Resource> {
        check_input(&input)?;
        self.update(|c| {
            let r = find_mut(c, id)?;
            check_match(r, if_match)?;
            if r.input.kind != input.kind {
                return Err(invalid(format!(
                    "{id} is a {}; a resource keeps its kind",
                    r.input.kind.as_str()
                )));
            }
            let before = r.licences();
            r.input = input;
            if r.licences() != before {
                r.acceptance = None;
            }
            r.modified = Stamp::now(by);
            Ok(r.clone())
        })
    }

    /// Remove a resource that no other resource references.
    pub fn delete(&self, id: &str, if_match: Option<&str>) -> Result<()> {
        self.update(|c| {
            let r = find(c, id).ok_or_else(|| no_such(id))?;
            check_match(r, if_match)?;
            let refs = references(c, id, None);
            if !refs.is_empty() {
                return Err(Error::Conflict(
                    "resource-in-use",
                    format!("{id} is referenced by {}", refs.join(", ")),
                ));
            }
            c.resources.retain(|r| r.id != id);
            Ok(())
        })
    }

    /// Add a version to a resource.
    pub fn add_version(&self, id: &str, v: VersionInput, by: &str) -> Result<Resource> {
        self.update(|c| {
            add_version(c, id, v, by)?;
            Ok(find(c, id).cloned().expect("exists"))
        })
    }

    /// Remove a version that nothing references.
    pub fn delete_version(&self, id: &str, version: &str, by: &str) -> Result<()> {
        self.update(|c| {
            let r = find(c, id).ok_or_else(|| no_such(id))?;
            if r.version(version).is_none() {
                return Err(no_such_version(id, version));
            }
            let refs = references(c, id, Some(version));
            if !refs.is_empty() {
                return Err(Error::Conflict(
                    "resource-in-use",
                    format!("{id} {version} is referenced by {}", refs.join(", ")),
                ));
            }
            let r = find_mut(c, id)?;
            r.versions.retain(|v| v.input.version != version);
            r.modified = Stamp::now(by);
            Ok(())
        })
    }

    /// Record an administrator's acceptance of a resource's restricted terms.
    pub fn accept(&self, id: &str, by: &str) -> Result<Resource> {
        self.update(|c| {
            let r = find_mut(c, id)?;
            if !r.restricted() {
                return Err(Error::Conflict(
                    "not-restricted",
                    format!("{id} has no restricted terms to accept"),
                ));
            }
            let s = Stamp::now(by);
            r.acceptance = Some(Acceptance {
                by: s.by,
                at: s.at,
                licenses: r.licences(),
            });
            r.modified = Stamp::now(by);
            Ok(r.clone())
        })
    }

    /// Withdraw the acceptance of a resource's terms.
    pub fn withdraw(&self, id: &str, by: &str) -> Result<Resource> {
        self.update(|c| {
            let r = find_mut(c, id)?;
            if r.acceptance.take().is_some() {
                r.modified = Stamp::now(by);
            }
            Ok(r.clone())
        })
    }
}

pub fn no_such_version(id: &str, version: &str) -> Error {
    Error::NotFound("no-such-version", format!("{id} has no version {version}"))
}

fn add_version(c: &mut CatalogFile, id: &str, mut v: VersionInput, by: &str) -> Result<()> {
    let kind = find(c, id).ok_or_else(|| no_such(id))?.input.kind;
    check_version(kind, &v)?;
    normalize_version(&mut v);
    let r = find(c, id).expect("exists");
    if r.version(&v.version).is_some() {
        return Err(Error::Conflict(
            "version-exists",
            format!(
                "{id} has the version {}; a version does not change once it is added",
                v.version
            ),
        ));
    }
    if r.versions.len() >= MAX_VERSIONS {
        return Err(Error::Limit(format!(
            "{id} has {MAX_VERSIONS} versions, the most allowed"
        )));
    }
    for p in v
        .imports
        .iter()
        .chain(v.graphs.iter().flat_map(|g| g.sources.iter()))
    {
        let t = find(c, &p.resource).ok_or_else(|| no_such(&p.resource))?;
        if t.version(&p.version).is_none() {
            return Err(no_such_version(&p.resource, &p.version));
        }
    }
    let r = find_mut(c, id)?;
    r.versions.push(Version {
        input: v,
        added: Stamp::now(by),
    });
    r.modified = Stamp::now(by);
    Ok(())
}

/// The message of an error, for a report.
pub fn message(e: &Error) -> String {
    match e {
        Error::Unreadable(m)
        | Error::NotFound(_, m)
        | Error::Invalid(m)
        | Error::Conflict(_, m)
        | Error::Precondition(m)
        | Error::Limit(m)
        | Error::Io(m) => m.clone(),
    }
}
