//! Protected managed graphs (spec C25 §5 and §7, shared with C26 §2.3).
//!
//! A dataset can hand named graphs to an owner, such as the evidence profile. Once
//! a graph is managed, no ordinary write may change it. SPARQL Update, Graph Store
//! writes, LOAD, RDF Patch, merges, replays and cherry-picks are all refused with
//! `profile-managed-graph` before anything is published, and `bypass_validation`
//! does not skip the check. Only a write that carries the owner's [`ManagedWrite`]
//! capability may change the owner's graphs.
//!
//! The registry is recorded in `managed.json` at the dataset root:
//!
//! ```json
//! {"format": 1, "revision": 3,
//!  "owners": {"evidence": {"profile": 1, "settingsDigest": "sha256:…", "graphs": ["…"],
//!                            "visibility": "public", "merge": "refuse"}}}
//! ```
//!
//! Each owner keeps its settings in its own file, such as `evidence.json`, whose SHA-256
//! digest `managed.json` records. A dataset with `managed.json` needs dataset reader 6
//! ([`READER`]), so a release that does not enforce the registry refuses to open it.
//!
//! A `managed.json` of a format this build does not read, an owner it does not know, a
//! profile it does not support, or an owner settings file whose digest does not match
//! makes the registry unavailable. Reads continue, and every write of the dataset and
//! its branches fails closed with `managed-unavailable` until the files are repaired.
//!
//! The registry is shared by the dataset's main store and all its branches, including
//! branches created before the registry was enabled. Branch directories do not copy
//! `managed.json`. Backups and clones carry it.
//!
//! In library mode the capability guards against accidents. Code linked into the same
//! process can construct its own writes through [`crate::store::Store::managed_writer`],
//! so the check does not defend against a hostile embedder.

use crate::error::{Error, Result};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The registry file at the dataset root.
pub const FILE: &str = "managed.json";

/// The dataset reader capability that enforces the registry (`minimumReader` of
/// `dataset.json`).
pub const READER: u32 = 6;

/// The `managed.json` format this build reads and writes.
pub const FORMAT: u32 = 1;

/// The most graphs one owner may manage.
pub const MAX_GRAPHS: usize = 4096;

/// The longest graph IRI, in UTF-8 bytes.
pub const MAX_IRI_BYTES: usize = 4096;

/// The largest owner settings file, in bytes.
pub const MAX_SETTINGS_BYTES: usize = 16 << 20;

/// Who owns a set of managed graphs. Each owner has its own typed writer and settings
/// file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum OwnerKind {
    /// The evidence profile (spec C25), whose settings are `evidence.json`.
    Evidence,
    /// A second owner for the engine's own tests.
    #[cfg(test)]
    Test,
}

impl OwnerKind {
    /// Every owner this build knows.
    #[cfg(not(test))]
    pub const ALL: [OwnerKind; 1] = [OwnerKind::Evidence];
    #[cfg(test)]
    pub const ALL: [OwnerKind; 2] = [OwnerKind::Evidence, OwnerKind::Test];

    /// The owner's name in `managed.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            OwnerKind::Evidence => "evidence",
            #[cfg(test)]
            OwnerKind::Test => "test",
        }
    }

    /// The owner named `name` in `managed.json`, if this build knows it.
    pub fn parse(name: &str) -> Option<OwnerKind> {
        OwnerKind::ALL.into_iter().find(|o| o.as_str() == name)
    }

    /// The owner's settings file at the dataset root.
    pub fn settings_file(self) -> &'static str {
        match self {
            OwnerKind::Evidence => "evidence.json",
            #[cfg(test)]
            OwnerKind::Test => "managed-test.json",
        }
    }

    /// Why this build cannot read the owner's settings `bytes` for `profile`, if it
    /// cannot: the settings file's envelope, its `format` and `profile` members, must
    /// be ones this build reads.
    pub(crate) fn check_settings(self, bytes: &[u8], profile: u32) -> Option<String> {
        match self {
            OwnerKind::Evidence => {
                let file = self.settings_file();
                let v: serde_json::Value = match serde_json::from_slice(bytes) {
                    Ok(v @ serde_json::Value::Object(_)) => v,
                    Ok(_) => return Some(format!("{file} is not a JSON object")),
                    Err(e) => return Some(format!("{file} is not JSON: {e}")),
                };
                if let Err(e) = crate::formats::check_settings(file, &v) {
                    return Some(e.to_string());
                }
                match v.get("profile").map(serde_json::Value::as_u64) {
                    None => None,
                    Some(Some(p)) if p == u64::from(profile) => None,
                    Some(p) => Some(format!(
                        "{file} has profile {}, not the managed profile {profile}",
                        p.map_or_else(|| "?".to_string(), |p| p.to_string())
                    )),
                }
            }
            #[cfg(test)]
            OwnerKind::Test => None,
        }
    }

    /// The profile versions of this owner that this build enforces.
    pub fn supports_profile(self, profile: u32) -> bool {
        match self {
            OwnerKind::Evidence => profile == 1,
            #[cfg(test)]
            OwnerKind::Test => profile == 1,
        }
    }
}

impl std::fmt::Display for OwnerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One owner's entry in `managed.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnerEntry {
    /// The owner's profile version.
    pub profile: u32,
    /// `sha256:` and the lowercase hex SHA-256 digest of the owner's settings file.
    pub settings_digest: String,
    /// The absolute IRIs of the graphs the owner manages.
    pub graphs: Vec<String>,
    /// Who sees the graphs through the ordinary read surfaces: `public`, under the
    /// caller's grants. `hidden` is reserved for owners whose control graphs only
    /// their typed inspection renders (spec C26 §2.3), and this build refuses it.
    #[serde(default)]
    pub visibility: Visibility,
    /// What a merge does with changes to the graphs: `refuse` the merge. `exclude` is
    /// reserved for owners whose records stay on their branch, and this build refuses
    /// it.
    #[serde(default)]
    pub merge: MergePolicy,
}

/// An owner's [`OwnerEntry::visibility`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Visibility {
    #[default]
    Public,
    /// Reserved: refused by this build.
    Hidden,
}

/// An owner's [`OwnerEntry::merge`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergePolicy {
    #[default]
    Refuse,
    /// Reserved: refused by this build.
    Exclude,
}

impl OwnerEntry {
    /// The reason this build cannot enforce the entry's reserved members, if any.
    pub(crate) fn reserved(&self) -> Option<&'static str> {
        if self.visibility != Visibility::Public {
            return Some("visibility \"hidden\" is reserved and not supported by this build");
        }
        if self.merge != MergePolicy::Refuse {
            return Some("merge \"exclude\" is reserved and not supported by this build");
        }
        None
    }
}

impl OwnerEntry {
    /// An entry for `graphs` whose settings file holds `settings`.
    pub fn for_settings(profile: u32, graphs: Vec<String>, settings: &[u8]) -> OwnerEntry {
        OwnerEntry {
            profile,
            settings_digest: settings_digest(settings),
            graphs,
            visibility: Visibility::Public,
            merge: MergePolicy::Refuse,
        }
    }
}

/// `sha256:` and the lowercase hex SHA-256 digest of `bytes`.
pub fn settings_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let hash = Sha256::digest(bytes);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in hash {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The managed graphs of a dataset: the registry's revision and its owners.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedGraphs {
    /// Raised by every change of the registry.
    pub revision: u64,
    pub owners: BTreeMap<OwnerKind, OwnerEntry>,
}

impl ManagedGraphs {
    /// The owner of the graph `iri`, if it is managed.
    pub fn owner_of(&self, iri: &str) -> Option<OwnerKind> {
        self.owners
            .iter()
            .find(|(_, e)| e.graphs.iter().any(|g| g == iri))
            .map(|(o, _)| *o)
    }
}

/// The `managed.json` document.
#[derive(Serialize)]
struct ManagedFile<'a> {
    format: u32,
    revision: u64,
    owners: BTreeMap<&'static str, &'a OwnerEntry>,
}

/// Why writes are refused: a stable code and the reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedErrorKind {
    /// A write without the owner's capability would change a managed graph.
    Protected,
    /// The registry cannot be enforced, so every write fails closed.
    Unavailable,
    /// The owner or profile is not enabled or not supported by this build.
    UnsupportedProfile,
}

/// An error of the managed-graph boundary, with its stable code. Nothing was written.
#[derive(Clone, Debug)]
pub struct ManagedError {
    pub kind: ManagedErrorKind,
    /// `profile-managed-graph`, `managed-unavailable` or `unsupported-profile`
    pub code: &'static str,
    pub message: String,
}

impl ManagedError {
    /// The error of `kind`, with the kind's code.
    pub fn error(kind: ManagedErrorKind, message: impl Into<String>) -> Error {
        let code = match kind {
            ManagedErrorKind::Protected => "profile-managed-graph",
            ManagedErrorKind::Unavailable => "managed-unavailable",
            ManagedErrorKind::UnsupportedProfile => "unsupported-profile",
        };
        Error::Managed(Box::new(ManagedError {
            kind,
            code,
            message: message.into(),
        }))
    }

    /// The HTTP status of the error.
    pub fn status(&self) -> u16 {
        match self.kind {
            ManagedErrorKind::Protected => 409,
            ManagedErrorKind::Unavailable => 503,
            ManagedErrorKind::UnsupportedProfile => 409,
        }
    }
}

impl std::fmt::Display for ManagedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub(crate) fn protected(iri: &str) -> Error {
    ManagedError::error(
        ManagedErrorKind::Protected,
        format!(
            "graph <{iri}> is managed by a profile; change it through the profile's operations, or detach it first"
        ),
    )
}

fn unavailable(reason: &str) -> Error {
    ManagedError::error(
        ManagedErrorKind::Unavailable,
        format!(
            "writes are refused because the managed-graph registry cannot be enforced: {reason}"
        ),
    )
}

/// The installed registry of a dataset family, or why it cannot be enforced.
#[derive(Debug)]
pub(crate) enum ManagedState {
    Installed(Installed),
    /// Every write is refused with this reason. `repairable` states can be replaced by
    /// enabling an owner again: a settings file whose digest does not match.
    Unavailable {
        reason: String,
        repairable: bool,
    },
}

#[derive(Debug)]
pub(crate) struct Installed {
    pub graphs: ManagedGraphs,
    /// graph vocabulary key (`<` and the IRI) to its owner
    by_key: FxHashMap<Box<[u8]>, OwnerKind>,
    /// each owner's settings file, as recorded
    pub settings: BTreeMap<OwnerKind, Arc<[u8]>>,
}

impl Installed {
    pub(crate) fn new(
        graphs: ManagedGraphs,
        settings: BTreeMap<OwnerKind, Arc<[u8]>>,
    ) -> Result<Installed> {
        let mut by_key = FxHashMap::default();
        for (owner, entry) in &graphs.owners {
            for g in &entry.graphs {
                let key = crate::id::iri_key(g).into_boxed_slice();
                if let Some(other) = by_key.insert(key, *owner)
                    && other != *owner
                {
                    return Err(Error::Conflict(format!(
                        "graph <{g}> is managed by both {other} and {owner}"
                    )));
                }
            }
        }
        Ok(Installed {
            graphs,
            by_key,
            settings,
        })
    }

    /// The owner of the graph whose vocabulary key is `key`.
    pub(crate) fn owner_of_key(&self, key: &[u8]) -> Option<OwnerKind> {
        self.by_key.get(key).copied()
    }

    pub(crate) fn has_graphs(&self) -> bool {
        !self.by_key.is_empty()
    }

    /// The managed graph IRIs, for a builder that sees parsed quads.
    pub(crate) fn iris(&self) -> Arc<rustc_hash::FxHashSet<Box<str>>> {
        Arc::new(
            self.graphs
                .owners
                .values()
                .flat_map(|e| e.graphs.iter().map(|g| g.as_str().into()))
                .collect(),
        )
    }

    /// The bytes of `managed.json` for this registry.
    pub(crate) fn file_bytes(&self) -> Vec<u8> {
        file_bytes(&self.graphs)
    }
}

pub(crate) fn file_bytes(graphs: &ManagedGraphs) -> Vec<u8> {
    let file = ManagedFile {
        format: FORMAT,
        revision: graphs.revision,
        owners: graphs.owners.iter().map(|(o, e)| (o.as_str(), e)).collect(),
    };
    serde_json::to_vec_pretty(&file).expect("managed.json serializes")
}

/// The capability to change one owner's managed graphs in one write transaction
/// ([`crate::store::WriteTxn::set_managed`]). Only
/// [`Store::managed_writer`](crate::store::Store::managed_writer) makes one, and it is
/// valid only for the registry that was installed in its store's dataset when it was
/// made: a capability of another dataset, or of a registry that changed since, is
/// refused.
///
/// In library mode this guards against accidents, not against a hostile embedder,
/// which can always call `managed_writer` itself.
pub struct ManagedWrite {
    pub(crate) owner: OwnerKind,
    pub(crate) operation: uuid::Uuid,
    pub(crate) state: Arc<ManagedState>,
}

impl ManagedWrite {
    /// The owner whose graphs the capability may change.
    pub fn owner(&self) -> OwnerKind {
        self.owner
    }

    /// The typed operation the capability was made for.
    pub fn operation(&self) -> uuid::Uuid {
        self.operation
    }
}

impl std::fmt::Debug for ManagedWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedWrite")
            .field("owner", &self.owner)
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

/// Admission of one candidate commit against the installed registry `state`. Each
/// distinct graph is looked up once, by its vocabulary key. Graphs without a key (the
/// default graph and blank nodes) are never managed.
pub(crate) struct Admission<'a> {
    state: Option<&'a Arc<ManagedState>>,
    managed: Option<&'a ManagedWrite>,
    seen: rustc_hash::FxHashSet<u64>,
}

impl<'a> Admission<'a> {
    /// `None` when nothing needs checking: no registry and no capability.
    pub(crate) fn start(
        state: Option<&'a Arc<ManagedState>>,
        managed: Option<&'a ManagedWrite>,
    ) -> Result<Option<Admission<'a>>> {
        if let Some(m) = managed
            && !state.is_some_and(|s| Arc::ptr_eq(s, &m.state))
        {
            return Err(ManagedError::error(
                ManagedErrorKind::Protected,
                "the managed-write capability belongs to another dataset or to a registry that has changed since; make a new one",
            ));
        }
        let Some(state) = state else {
            return Ok(None);
        };
        match &**state {
            ManagedState::Unavailable { reason, .. } => Err(unavailable(reason)),
            ManagedState::Installed(i) if !i.has_graphs() => Ok(None),
            ManagedState::Installed(_) => Ok(Some(Admission {
                state: Some(state),
                managed,
                seen: Default::default(),
            })),
        }
    }

    /// The installed registry being checked against.
    pub(crate) fn installed(&self) -> &Installed {
        match self.state.map(|s| &**s) {
            Some(ManagedState::Installed(i)) => i,
            _ => unreachable!("admission starts only with an installed registry"),
        }
    }

    /// Check the graph `id` once, with `key` decoding its vocabulary key.
    pub(crate) fn graph<K: AsRef<[u8]>>(
        &mut self,
        id: u64,
        key: impl FnOnce() -> Option<K>,
    ) -> Result<()> {
        if !self.seen.insert(id) {
            return Ok(());
        }
        match key() {
            Some(k) => self.key(k.as_ref()),
            None => Ok(()),
        }
    }

    /// Check the graph whose vocabulary key is `key`.
    pub(crate) fn key(&self, key: &[u8]) -> Result<()> {
        let Some(owner) = self.installed().owner_of_key(key) else {
            return Ok(());
        };
        if self.managed.is_some_and(|m| m.owner == owner) {
            return Ok(());
        }
        let iri = String::from_utf8_lossy(key.strip_prefix(b"<").unwrap_or(key));
        Err(protected(&iri))
    }
}

/// A builder-side check of parsed quads: a quad in a managed graph refuses the build.
pub(crate) type GraphFence = Arc<rustc_hash::FxHashSet<Box<str>>>;

// ------------------------------------------------------------------ the file ------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default = "one")]
    format: u64,
    revision: u64,
    owners: BTreeMap<String, serde_json::Value>,
}

fn one() -> u64 {
    1
}

/// Decode `managed.json` and verify each owner's settings with `read_settings`, which
/// returns the bytes of an owner's settings file (`None` when it is missing). An `Err`
/// is a refusal of the whole registry: the state is then unavailable.
pub(crate) fn decode(
    bytes: &[u8],
    mut read_settings: impl FnMut(OwnerKind) -> Result<Option<Vec<u8>>>,
) -> ManagedState {
    match decode_inner(bytes, &mut read_settings) {
        Ok(installed) => ManagedState::Installed(installed),
        Err((reason, repairable)) => ManagedState::Unavailable { reason, repairable },
    }
}

fn decode_inner(
    bytes: &[u8],
    read_settings: &mut impl FnMut(OwnerKind) -> Result<Option<Vec<u8>>>,
) -> std::result::Result<Installed, (String, bool)> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| (format!("{FILE} is not JSON: {e}"), false))?;
    match v.get("format").map(serde_json::Value::as_u64) {
        None => {}
        Some(Some(f)) if f == u64::from(FORMAT) => {}
        Some(Some(f)) => {
            return Err((
                format!(
                    "{FILE} has format {f}; Sparkles {} reads format {FORMAT}. It was written by a newer release",
                    crate::formats::SPARKLES_VERSION
                ),
                false,
            ));
        }
        Some(None) => return Err((format!("{FILE}: format is not a version"), false)),
    }
    let raw: RawFile = serde_json::from_value(v).map_err(|e| (format!("{FILE}: {e}"), false))?;
    debug_assert_eq!(raw.format, u64::from(FORMAT));
    let mut owners = BTreeMap::new();
    let mut settings = BTreeMap::new();
    for (name, value) in raw.owners {
        let Some(owner) = OwnerKind::parse(&name) else {
            return Err((
                format!("{FILE} names the owner {name:?}, which this build does not know"),
                false,
            ));
        };
        let entry: OwnerEntry = serde_json::from_value(value)
            .map_err(|e| (format!("{FILE}: owner {name}: {e}"), false))?;
        if !owner.supports_profile(entry.profile) {
            return Err((
                format!(
                    "{FILE}: owner {name} has profile {}, which this build does not enforce",
                    entry.profile
                ),
                false,
            ));
        }
        if let Some(why) = entry.reserved() {
            return Err((format!("{FILE}: owner {name}: {why}"), false));
        }
        check_graphs(&entry.graphs).map_err(|e| (format!("{FILE}: owner {name}: {e}"), false))?;
        let file = owner.settings_file();
        let found = read_settings(owner).map_err(|e| (format!("{file}: {e}"), false))?;
        let Some(found) = found else {
            return Err((
                format!("{file} of the managed owner {name} is missing"),
                true,
            ));
        };
        if settings_digest(&found) != entry.settings_digest {
            return Err((
                format!("{file} does not match the digest {FILE} records for it"),
                true,
            ));
        }
        if let Some(why) = owner.check_settings(&found, entry.profile) {
            return Err((why, false));
        }
        settings.insert(owner, Arc::from(found));
        owners.insert(owner, entry);
    }
    Installed::new(
        ManagedGraphs {
            revision: raw.revision,
            owners,
        },
        settings,
    )
    .map_err(|e| (e.to_string(), false))
}

/// Refuse graph lists that are not absolute IRIs within the bounds.
pub(crate) fn check_graphs(graphs: &[String]) -> Result<()> {
    if graphs.len() > MAX_GRAPHS {
        return Err(Error::invalid(format!(
            "{} managed graphs; an owner manages at most {MAX_GRAPHS}",
            graphs.len()
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for g in graphs {
        if g.len() > MAX_IRI_BYTES {
            return Err(Error::invalid(format!(
                "a managed graph IRI is longer than {MAX_IRI_BYTES} bytes"
            )));
        }
        oxrdf::NamedNode::new(g.as_str())
            .map_err(|e| Error::invalid(format!("managed graph <{g}>: {e}")))?;
        if !seen.insert(g.as_str()) {
            return Err(Error::invalid(format!(
                "managed graph <{g}> is listed twice"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(graphs: &[&str], settings: &[u8]) -> OwnerEntry {
        OwnerEntry::for_settings(1, graphs.iter().map(|g| g.to_string()).collect(), settings)
    }

    #[test]
    fn a_file_round_trips() {
        let graphs = ManagedGraphs {
            revision: 2,
            owners: [(OwnerKind::Evidence, entry(&["http://e/g"], b"{}"))].into(),
        };
        let bytes = file_bytes(&graphs);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["format"], 1);
        assert_eq!(v["revision"], 2);
        assert_eq!(v["owners"]["evidence"]["graphs"][0], "http://e/g");
        assert_eq!(v["owners"]["evidence"]["visibility"], "public");
        assert_eq!(v["owners"]["evidence"]["merge"], "refuse");
        // written before the members existed: the defaults
        let mut old = v.clone();
        old["owners"]["evidence"]
            .as_object_mut()
            .unwrap()
            .remove("visibility");
        old["owners"]["evidence"]
            .as_object_mut()
            .unwrap()
            .remove("merge");
        match decode(&serde_json::to_vec(&old).unwrap(), |_| {
            Ok(Some(b"{}".to_vec()))
        }) {
            ManagedState::Installed(i) => assert_eq!(i.graphs, graphs),
            s => panic!("{s:?}"),
        }
        assert!(
            v["owners"]["evidence"]["settingsDigest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        match decode(&bytes, |_| Ok(Some(b"{}".to_vec()))) {
            ManagedState::Installed(i) => assert_eq!(i.graphs, graphs),
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn unknown_states_are_unavailable() {
        let graphs = ManagedGraphs {
            revision: 1,
            owners: [(OwnerKind::Evidence, entry(&["http://e/g"], b"{}"))].into(),
        };
        let good = file_bytes(&graphs);
        let mut v: serde_json::Value = serde_json::from_slice(&good).unwrap();
        v["format"] = 2.into();
        let newer = serde_json::to_vec(&v).unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&good).unwrap();
        v["owners"]["jobs"] = v["owners"]["evidence"].clone();
        let unknown_owner = serde_json::to_vec(&v).unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&good).unwrap();
        v["owners"]["evidence"]["profile"] = 2.into();
        let unknown_profile = serde_json::to_vec(&v).unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&good).unwrap();
        v["owners"]["evidence"]["visibility"] = "hidden".into();
        let hidden = serde_json::to_vec(&v).unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&good).unwrap();
        v["owners"]["evidence"]["merge"] = "exclude".into();
        let exclude = serde_json::to_vec(&v).unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&good).unwrap();
        v["owners"]["evidence"]["merge"] = "later".into();
        let unknown_merge = serde_json::to_vec(&v).unwrap();
        for (bytes, repairable, settings) in [
            (&newer[..], false, Some(b"{}".to_vec())),
            (&unknown_owner, false, Some(b"{}".to_vec())),
            (&unknown_profile, false, Some(b"{}".to_vec())),
            (&hidden, false, Some(b"{}".to_vec())),
            (&exclude, false, Some(b"{}".to_vec())),
            (&unknown_merge, false, Some(b"{}".to_vec())),
            (&good, true, Some(b"{ }".to_vec())),
            (&good, true, None),
            (b"[", false, None),
        ] {
            match decode(bytes, |_| Ok(settings.clone())) {
                ManagedState::Unavailable { repairable: r, .. } => assert_eq!(r, repairable),
                s => panic!("{s:?}"),
            }
        }
    }

    #[test]
    fn graph_lists_are_checked() {
        assert!(check_graphs(&["http://e/g".into()]).is_ok());
        assert!(check_graphs(&["not an iri".into()]).is_err());
        assert!(check_graphs(&["http://e/g".into(), "http://e/g".into()]).is_err());
    }
}
