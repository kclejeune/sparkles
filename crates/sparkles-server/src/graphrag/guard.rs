//! The disclosure guard of reports graphs (spec C22 §6).
//!
//! Every graph under [`GUARDED_PREFIX`] is guarded. A build records a [`Footprint`] of
//! what it read, and a dataset's footprints are kept in a versioned [`Registry`] in
//! `<db>/graphrag/disclosure.json`, to which entries are only ever added. When a request
//! resolves its view, [`apply`] removes every guarded graph that [`covers`] does not
//! prove for the reader, so every read path treats it as a graph that does not exist.
//!
//! Coverage compares definitions and caller attributes, never data:
//!
//! 1. a reader with no view, or one that reads every graph with no protection in force
//!    for reads, is proven;
//! 2. otherwise every graph of the footprint must be in the reader's read graphs, and on
//!    each, every protection in force for the reader must be in force in the footprint
//!    with an identical definition, with the footprint's values of every caller
//!    variable its pattern uses.
//!
//! The second rule includes the identical resolved view. Everything else is unproven
//! and fails closed. A guarded graph with no valid footprint is proven only by rule 1.

use super::settings::GUARDED_PREFIX;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sparkles::access::{Caller, GraphAccess, Graphs, Protection, Rule};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The registry format this server reads and writes.
pub const REGISTRY_VERSION: u32 = 1;
/// The footprint format this server reads and writes.
pub const FOOTPRINT_VERSION: u32 = 1;
/// The pattern that names every guarded graph.
pub fn guarded_pattern() -> String {
    format!("{GUARDED_PREFIX}*")
}
/// The most entries a registry may hold.
pub const MAX_ENTRIES: usize = 10_000;
/// The largest registry file read, in bytes.
pub const MAX_REGISTRY_BYTES: u64 = 64 << 20;

/// The file of a database directory.
pub fn path_in(db: &Path) -> PathBuf {
    db.join("graphrag").join("disclosure.json")
}

/// Serializes registry installs in the process.
static INSTALLS: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Create `dir` inside the database directory `db` and flush `db`, so that a file
/// written in `dir` survives a power loss. `db` is flushed on every call, since an
/// earlier call may have created `dir` and then failed to flush.
pub(crate) fn create_dir_durably(db: &Path, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    std::fs::File::open(db)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = db;
    Ok(())
}

/// A graph set as the footprint keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase", tag = "kind")]
pub enum FpGraphs {
    All,
    Only {
        #[serde(default)]
        default_graph: bool,
        #[serde(default)]
        iris: Vec<String>,
        #[serde(default)]
        patterns: Vec<String>,
        #[serde(default)]
        protected: Vec<String>,
        #[serde(default)]
        blank_nodes: bool,
        #[serde(default)]
        excluded: Vec<String>,
    },
}

impl FpGraphs {
    /// The graph set, or `None` when it holds a guard and so is not a protection's own
    /// scope as configured.
    fn of(g: &Graphs) -> Option<FpGraphs> {
        Some(match g {
            Graphs::All => FpGraphs::All,
            Graphs::Only(r) => {
                if !r.guards.is_empty() {
                    return None;
                }
                FpGraphs::Only {
                    default_graph: r.default_graph,
                    iris: r.iris.clone(),
                    patterns: r.patterns.clone(),
                    protected: r.protected.clone(),
                    blank_nodes: r.blank_nodes,
                    excluded: r.excluded.clone(),
                }
            }
        })
    }
}

/// A protection's full definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FpProtection {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicates: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classes: Option<Vec<String>>,
    pub subclasses: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphs: Option<FpGraphs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default)]
    pub prefixes: BTreeMap<String, String>,
    pub hide_inferences: bool,
}

impl FpProtection {
    fn of(p: &Protection) -> Option<FpProtection> {
        Some(FpProtection {
            name: p.name.clone(),
            predicates: p.predicates.clone(),
            classes: p.classes.clone(),
            subclasses: p.subclasses,
            graphs: match &p.graphs {
                None => None,
                Some(g) => Some(FpGraphs::of(g)?),
            },
            pattern: p.pattern.clone(),
            prefixes: p.prefixes.clone(),
            hide_inferences: p.hide_inferences,
        })
    }
}

/// The caller attributes a pattern may bind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FpCaller {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

impl FpCaller {
    fn of(c: &Caller) -> FpCaller {
        let mut roles = c.roles.clone();
        roles.sort();
        roles.dedup();
        let mut groups = c.groups.clone();
        groups.sort();
        groups.dedup();
        FpCaller {
            user: c.user.clone(),
            roles,
            groups,
        }
    }
}

/// A graph the build read, with the protections in force on it for the principal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FpGraph {
    /// an IRI, or `default` for the default graph
    pub graph: String,
    #[serde(default)]
    pub protections: Vec<FpProtection>,
}

/// What one build read (§6.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Footprint {
    pub version: u32,
    pub dataset: String,
    pub branch: String,
    pub source_commit: u64,
    pub principal: String,
    pub caller: FpCaller,
    pub endpoint: String,
    pub graphs: Vec<FpGraph>,
    #[serde(default)]
    pub reports: Vec<String>,
}

/// Why a footprint cannot be captured.
#[derive(Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// the principal cannot read a graph the build would read
    Unreadable(String),
    /// a graph or a protection cannot be written in the footprint's form
    Unrepresentable(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Unreadable(g) => write!(f, "the principal cannot read the graph {g}"),
            CaptureError::Unrepresentable(m) => {
                write!(f, "the disclosure footprint cannot describe {m}")
            }
        }
    }
}

impl std::error::Error for CaptureError {}

fn term_of(graph: &str) -> Option<oxrdf::Term> {
    if graph == "default" {
        None
    } else {
        Some(oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            graph,
        )))
    }
}

/// The rules in force for reads on `graph` for a view.
fn in_force_on<'a>(view: &'a GraphAccess, graph: &str) -> Vec<&'a Rule> {
    let term = term_of(graph);
    view.triples.as_ref().map_or_else(Vec::new, |t| {
        t.rules
            .iter()
            .filter(|r| r.protection.covers_graph(term.as_ref()) && !r.read.allows(term.as_ref()))
            .collect()
    })
}

/// Whether a view reads `graph`.
fn reads(view: &GraphAccess, graph: &str) -> bool {
    view.read.allows(term_of(graph).as_ref())
}

pub struct CaptureInput<'a> {
    pub dataset: &'a str,
    pub branch: &'a str,
    pub source_commit: u64,
    pub principal: &'a str,
    pub endpoint: &'a str,
    /// the principal's view at that endpoint, `None` when it is unrestricted
    pub view: Option<&'a GraphAccess>,
    /// the caller attributes of the principal
    pub caller: &'a Caller,
    /// the concrete graphs the build reads: IRIs, or `default`
    pub graphs: &'a [String],
    pub reports: &'a [String],
}

/// Capture the footprint of a build.
pub fn capture(i: &CaptureInput<'_>) -> Result<Footprint, CaptureError> {
    let mut graphs = Vec::with_capacity(i.graphs.len());
    let mut names: Vec<&String> = i.graphs.iter().collect();
    names.sort();
    names.dedup();
    for g in names {
        if g.starts_with("_:") || g.is_empty() {
            return Err(CaptureError::Unrepresentable(format!("the graph {g:?}")));
        }
        if super::settings::is_guarded(g) {
            return Err(CaptureError::Unrepresentable(format!(
                "a build that reads the guarded graph {g}"
            )));
        }
        let mut protections = Vec::new();
        if let Some(v) = i.view {
            if !reads(v, g) {
                return Err(CaptureError::Unreadable(g.clone()));
            }
            for r in in_force_on(v, g) {
                protections.push(FpProtection::of(&r.protection).ok_or_else(|| {
                    CaptureError::Unrepresentable(format!("the protection {}", r.protection.name))
                })?);
            }
        }
        graphs.push(FpGraph {
            graph: g.clone(),
            protections,
        });
    }
    Ok(Footprint {
        version: FOOTPRINT_VERSION,
        dataset: i.dataset.to_string(),
        branch: i.branch.to_string(),
        source_commit: i.source_commit,
        principal: i.principal.to_string(),
        caller: FpCaller::of(i.caller),
        endpoint: i.endpoint.to_string(),
        graphs,
        reports: i.reports.to_vec(),
    })
}

/// A coverage decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Proven,
    /// the reader cannot read a graph of the footprint
    Denied,
    /// no admitted rule proves coverage
    Unproven,
}

/// Whether a view reads everything with no protection in force for reads (rule 1).
pub fn unrestricted(view: Option<&GraphAccess>) -> bool {
    match view {
        None => true,
        Some(v) => v.read.is_all() && v.triples.as_ref().is_none_or(|t| !t.hides()),
    }
}

/// The caller variables a pattern uses: `?user`, `?role` and `?group`, or the same
/// names with `$`.
fn uses_var(pattern: &str, var: &str) -> bool {
    let b = pattern.as_bytes();
    let n = var.len();
    (0..b.len()).any(|i| {
        (b[i] == b'?' || b[i] == b'$')
            && pattern.get(i + 1..i + 1 + n) == Some(var)
            && b.get(i + 1 + n)
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_' || *c >= 0x80))
    })
}

/// Decide whether a reader's view covers a footprint.
pub fn covers(reader: Option<&GraphAccess>, fp: &Footprint) -> Decision {
    if unrestricted(reader) {
        return Decision::Proven;
    }
    let Some(v) = reader else {
        return Decision::Proven;
    };
    if fp.version != FOOTPRINT_VERSION {
        return Decision::Unproven;
    }
    let caller = v
        .triples
        .as_ref()
        .map(|t| FpCaller::of(&t.caller))
        .unwrap_or_default();
    for g in &fp.graphs {
        if !reads(v, &g.graph) {
            return Decision::Denied;
        }
        for r in in_force_on(v, &g.graph) {
            let Some(p) = FpProtection::of(&r.protection) else {
                return Decision::Unproven;
            };
            if !g.protections.contains(&p) {
                return Decision::Unproven;
            }
            if let Some(pat) = &p.pattern
                && (uses_var(pat, "user") && caller.user != fp.caller.user
                    || uses_var(pat, "role") && caller.roles != fp.caller.roles
                    || uses_var(pat, "group") && caller.groups != fp.caller.groups)
            {
                return Decision::Unproven;
            }
        }
    }
    Decision::Proven
}

/// One footprint for one guarded graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Entry {
    pub graph: String,
    pub footprint: Footprint,
}

/// `graphrag/disclosure.json`: the footprints of a dataset, only ever added to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Registry {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

impl Default for Registry {
    fn default() -> Registry {
        Registry {
            version: REGISTRY_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum RegistryError {
    UnknownVersion(u64),
    Corrupt(String),
    Bound(&'static str),
    Io(std::io::Error),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::UnknownVersion(v) => write!(
                f,
                "the disclosure registry has format version {v}, and this server reads version {REGISTRY_VERSION}"
            ),
            RegistryError::Corrupt(m) => write!(f, "the disclosure registry cannot be read: {m}"),
            RegistryError::Bound(m) => write!(f, "the disclosure registry is too large: {m}"),
            RegistryError::Io(e) => write!(f, "the disclosure registry: {e}"),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<std::io::Error> for RegistryError {
    fn from(e: std::io::Error) -> RegistryError {
        RegistryError::Io(e)
    }
}

impl Registry {
    fn check(&self) -> Result<(), RegistryError> {
        if self.version != REGISTRY_VERSION {
            return Err(RegistryError::UnknownVersion(self.version.into()));
        }
        if self.entries.len() > MAX_ENTRIES {
            return Err(RegistryError::Bound("entries"));
        }
        for e in &self.entries {
            if !super::settings::is_guarded(&e.graph) {
                return Err(RegistryError::Corrupt(format!(
                    "the entry for {} is outside the guarded namespace",
                    e.graph
                )));
            }
            if e.footprint.version != FOOTPRINT_VERSION {
                return Err(RegistryError::UnknownVersion(e.footprint.version.into()));
            }
        }
        Ok(())
    }

    /// Parse a registry, refusing an unknown version before any other member.
    pub fn parse(bytes: &[u8]) -> Result<Registry, RegistryError> {
        #[derive(Deserialize)]
        struct Probe {
            version: Option<serde_json::Value>,
        }
        let p: Probe =
            serde_json::from_slice(bytes).map_err(|e| RegistryError::Corrupt(e.to_string()))?;
        match p.version.as_ref().and_then(|v| v.as_u64()) {
            Some(v) if v == u64::from(REGISTRY_VERSION) => {}
            Some(v) => return Err(RegistryError::UnknownVersion(v)),
            None => return Err(RegistryError::Corrupt("the version is missing".into())),
        }
        let r: Registry =
            serde_json::from_slice(bytes).map_err(|e| RegistryError::Corrupt(e.to_string()))?;
        r.check()?;
        Ok(r)
    }

    /// The registry of a database directory, empty when there is none.
    pub fn load(db: &Path) -> Result<Registry, RegistryError> {
        let path = path_in(db);
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_REGISTRY_BYTES => Err(RegistryError::Bound("bytes")),
            Ok(_) => Registry::parse(&std::fs::read(&path)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Add entries and write the registry durably: a new file, a flush and a rename.
    /// The registry on disk is read again first, so an entry written by another build
    /// is kept, and an unreadable registry refuses the install.
    /// Installs are serialized in the process, so two cannot both read the old registry
    /// and lose each other's entries.
    pub fn install(db: &Path, entries: Vec<Entry>) -> Result<Registry, RegistryError> {
        let _serial = INSTALLS.lock();
        let mut r = Registry::load(db)?;
        for e in entries {
            if !r.entries.contains(&e) {
                r.entries.push(e);
            }
        }
        r.check()?;
        let bytes = serde_json::to_vec(&r).expect("serializable");
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(RegistryError::Bound("bytes"));
        }
        let path = path_in(db);
        let dir = path.parent().expect("the registry has a directory");
        create_dir_durably(db, dir)?;
        let tmp = dir.join("disclosure.json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(r)
    }

    /// The footprints of each guarded graph.
    fn by_graph(&self) -> HashMap<String, Vec<Footprint>> {
        let mut m: HashMap<String, Vec<Footprint>> = HashMap::new();
        for e in &self.entries {
            m.entry(e.graph.clone())
                .or_default()
                .push(e.footprint.clone());
        }
        m
    }
}

/// What the guard knows of one dataset.
#[derive(Clone, Debug)]
enum Known {
    /// the footprints of each guarded graph
    Valid(Arc<HashMap<String, Vec<Footprint>>>),
    /// the registry could not be read: no guarded graph has a valid footprint
    Invalid,
}

/// A dataset as the guard's resolver finds it: its identity, and its directory when it
/// is persistent.
pub type Resolved = (uuid::Uuid, Option<PathBuf>);

/// Finds a dataset by name.
pub type Resolver = Box<dyn Fn(&str) -> Option<Resolved> + Send + Sync>;

/// The registry file's length and modification time, which tell a changed file apart.
/// The registry file's size, modification time and, on Unix, inode. Every install
/// renames a new file into place, so the inode changes even within one clock tick.
type Stamp = Option<(u64, std::time::SystemTime, u64)>;

struct Cached {
    id: uuid::Uuid,
    stamp: Stamp,
    known: Known,
}

/// The registries of a server's datasets. A registry is read from its dataset's
/// directory when first needed and read again when its file changes or the dataset is
/// replaced. An in-memory dataset keeps its registry here only.
pub struct Guard {
    resolve: Resolver,
    cache: RwLock<HashMap<String, Cached>>,
    /// bumped whenever a registry gains an entry; a long-lived read made before ends
    /// when it sees a new value (§6.3), together with the policy's own epoch
    epoch: AtomicU64,
}

impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Guard").finish_non_exhaustive()
    }
}

fn stamp(db: &Path) -> Result<Stamp, std::io::Error> {
    match std::fs::metadata(path_in(db)) {
        Ok(m) => {
            #[cfg(unix)]
            let ino = std::os::unix::fs::MetadataExt::ino(&m);
            #[cfg(not(unix))]
            let ino = 0;
            Ok(Some((m.len(), m.modified()?, ino)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl Guard {
    pub fn new(resolve: Resolver) -> Guard {
        Guard {
            resolve,
            cache: RwLock::new(HashMap::new()),
            epoch: AtomicU64::new(0),
        }
    }

    /// The footprints the guard knows for `ds`, `None` when its registry is invalid.
    #[cfg(test)]
    fn footprints(&self, ds: &str) -> Option<usize> {
        match self.known(ds) {
            Known::Valid(m) => Some(m.values().map(Vec::len).sum()),
            Known::Invalid => None,
        }
    }

    /// The number of registry installs this guard has made.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// What the guard knows of `ds` now.
    fn known(&self, ds: &str) -> Known {
        let Some((id, root)) = (self.resolve)(ds) else {
            return Known::Valid(Arc::default());
        };
        let current = match &root {
            None => None,
            Some(db) => match stamp(db) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(target: "sparkles::graphrag", "dataset {ds}: the disclosure registry cannot be read: {e}");
                    return Known::Invalid;
                }
            },
        };
        if let Some(c) = self.cache.read().get(ds)
            && c.id == id
            && (root.is_none() || c.stamp == current)
        {
            return c.known.clone();
        }
        let known = match &root {
            // a new in-memory dataset has no footprint until a build installs one
            None => Known::Valid(Arc::default()),
            Some(db) => match Registry::load(db) {
                Ok(r) => Known::Valid(Arc::new(r.by_graph())),
                Err(e) => {
                    tracing::warn!(
                        target: "sparkles::graphrag",
                        "dataset {ds}: {e}; its reports are readable only by unrestricted readers"
                    );
                    Known::Invalid
                }
            },
        };
        let mut cache = self.cache.write();
        // an install may have published a newer registry since the read above: an
        // in-memory dataset's entry is never replaced by its initial empty registry
        if root.is_none()
            && let Some(c) = cache.get(ds)
            && c.id == id
        {
            return c.known.clone();
        }
        cache.insert(
            ds.to_string(),
            Cached {
                id,
                stamp: current,
                known: known.clone(),
            },
        );
        known
    }

    /// Add footprints to a dataset's registry, durably for a persistent dataset, before
    /// any report they cover is committed. Decisions made after it use them.
    pub fn install(&self, ds: &str, entries: Vec<Entry>) -> Result<(), RegistryError> {
        let Some((id, root)) = (self.resolve)(ds) else {
            return Err(RegistryError::Corrupt(format!("no dataset named {ds}")));
        };
        match root {
            Some(db) => {
                Registry::install(&db, entries)?;
                // a reader that loaded the old file keeps its old stamp, so the next
                // decision reads the new file
                self.cache.write().remove(ds);
            }
            None => {
                let mut cache = self.cache.write();
                let mut r = Registry::default();
                if let Some(c) = cache.get(ds).filter(|c| c.id == id)
                    && let Known::Valid(m) = &c.known
                {
                    for (g, fps) in m.iter() {
                        for f in fps {
                            r.entries.push(Entry {
                                graph: g.clone(),
                                footprint: f.clone(),
                            });
                        }
                    }
                }
                for e in entries {
                    if !r.entries.contains(&e) {
                        r.entries.push(e);
                    }
                }
                r.check()?;
                cache.insert(
                    ds.to_string(),
                    Cached {
                        id,
                        stamp: None,
                        known: Known::Valid(Arc::new(r.by_graph())),
                    },
                );
            }
        }
        self.epoch.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// The guarded graphs proven for a reader of `ds`.
    fn proven(&self, ds: &str, view: &GraphAccess) -> Vec<String> {
        let Known::Valid(m) = self.known(ds) else {
            return Vec::new();
        };
        let mut out: Vec<String> = m
            .iter()
            .filter(|(_, fps)| {
                !fps.is_empty()
                    && fps
                        .iter()
                        .all(|f| covers(Some(view), f) == Decision::Proven)
            })
            .map(|(g, _)| g.clone())
            .collect();
        out.sort();
        out
    }
}

/// The dataset part of a name qualified with a branch (`ds@branch`).
fn base(ds: &str) -> &str {
    ds.split_once('@').map_or(ds, |(d, _)| d)
}

/// Apply the guard to a resolved view of `ds`: `None` for a reader without a view,
/// which is unrestricted. A restricted view loses every guarded graph not proven for
/// it. Without a guard (`None`), no guarded graph is proven.
pub fn apply(view: GraphAccess, ds: &str, guard: Option<&Guard>) -> GraphAccess {
    if unrestricted(Some(&view)) {
        return view;
    }
    let exempt = guard.map_or_else(Vec::new, |g| g.proven(base(ds), &view));
    let pattern = guarded_pattern();
    GraphAccess {
        read: view.read.guarded(&pattern, &exempt),
        write: view.write.guarded(&pattern, &exempt),
        triples: view.triples,
    }
}

#[cfg(test)]
mod tests;
