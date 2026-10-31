//! The store side of the managed-graph boundary (see [`crate::managed`]): loading,
//! enabling and detaching the family's registry, and admitting each candidate commit.

use super::{Snapshot, Store, sync_dir, write_atomic};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::managed::{
    Admission, GraphFence, Installed, ManagedError, ManagedErrorKind, ManagedGraphs, ManagedState,
    ManagedWrite, OwnerEntry, OwnerKind,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// The registry recorded at `root`, `None` without `managed.json`. A registry that
/// cannot be read or verified is unavailable, which refuses writes and keeps reads.
pub(super) fn load_dir(root: &Path) -> Option<ManagedState> {
    let bytes = match std::fs::read(root.join(crate::managed::FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(ManagedState::Unavailable {
                reason: format!("{}: {e}", crate::managed::FILE),
                repairable: false,
            });
        }
    };
    let state = crate::managed::decode(&bytes, |owner| {
        let path = root.join(owner.settings_file());
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > crate::managed::MAX_SETTINGS_BYTES as u64 => {
                return Err(Error::invalid(format!(
                    "larger than {} bytes",
                    crate::managed::MAX_SETTINGS_BYTES
                )));
            }
            _ => {}
        }
        match std::fs::read(&path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    });
    if let ManagedState::Unavailable { reason, .. } = &state {
        tracing::error!(target: "sparkles::store", "{}: writes are refused: {reason}", root.display());
    }
    Some(state)
}

impl Store {
    fn managed_state(&self) -> Option<Arc<ManagedState>> {
        self.family.managed.load_full()
    }

    /// The managed graphs of this store's dataset, `None` when no owner manages any.
    /// Fails with `managed-unavailable` when the registry cannot be enforced.
    pub fn managed_graphs(&self) -> Result<Option<ManagedGraphs>> {
        match self.managed_state().as_deref() {
            None => Ok(None),
            Some(ManagedState::Installed(i)) => Ok(Some(i.graphs.clone())),
            Some(ManagedState::Unavailable { reason, .. }) => Err(ManagedError::error(
                ManagedErrorKind::Unavailable,
                reason.clone(),
            )),
        }
    }

    /// The capability for one typed operation of `owner` to change its managed graphs
    /// through [`WriteTxn::set_managed`](super::WriteTxn::set_managed). It is valid for
    /// this dataset's registry as it is now, and is refused once the registry changes.
    ///
    /// In library mode this guards against accidental writes, not against a hostile
    /// embedder: any code in the process can call it.
    pub fn managed_writer(&self, owner: OwnerKind, operation: uuid::Uuid) -> Result<ManagedWrite> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        let Some(state) = self.managed_state() else {
            return Err(not_enabled(owner));
        };
        match &*state {
            ManagedState::Unavailable { reason, .. } => {
                return Err(ManagedError::error(
                    ManagedErrorKind::Unavailable,
                    format!("the managed-graph registry cannot be enforced: {reason}"),
                ));
            }
            ManagedState::Installed(i) if !i.graphs.owners.contains_key(&owner) => {
                return Err(not_enabled(owner));
            }
            ManagedState::Installed(_) => {}
        }
        Ok(ManagedWrite {
            owner,
            operation,
            state,
        })
    }

    /// Hand the graphs of `entry` to `owner`, whose settings are `settings`, replacing
    /// the owner's previous entry. Through the dataset's main store, under its writer
    /// lock, in this order: the dataset reader rises to 6 durably, the owner's settings
    /// file is replaced durably, `managed.json` is replaced durably, which is the commit
    /// point, and the registry is installed for main and every branch. A crash before
    /// `managed.json` is replaced leaves the previous registry, or, when the owner's
    /// settings changed, a registry whose digest does not match, which refuses writes
    /// until the owner is enabled again. An in-memory store keeps the registry in memory.
    ///
    /// Returns the installed registry. When this returns, every commit of the dataset
    /// that has not yet published is admitted against it.
    pub fn enable_managed(
        &self,
        owner: OwnerKind,
        entry: OwnerEntry,
        settings: &[u8],
    ) -> Result<ManagedGraphs> {
        if !owner.supports_profile(entry.profile) {
            return Err(ManagedError::error(
                ManagedErrorKind::UnsupportedProfile,
                format!(
                    "{owner} profile {} is not supported by Sparkles {}",
                    entry.profile,
                    crate::formats::SPARKLES_VERSION
                ),
            ));
        }
        if let Some(why) = entry.reserved() {
            return Err(ManagedError::error(
                ManagedErrorKind::UnsupportedProfile,
                why,
            ));
        }
        crate::managed::check_graphs(&entry.graphs)?;
        if settings.len() > crate::managed::MAX_SETTINGS_BYTES {
            return Err(Error::invalid(format!(
                "{} is larger than {} bytes",
                owner.settings_file(),
                crate::managed::MAX_SETTINGS_BYTES
            )));
        }
        if crate::managed::settings_digest(settings) != entry.settings_digest {
            return Err(Error::invalid(format!(
                "settingsDigest does not match the {} bytes",
                owner.settings_file()
            )));
        }
        self.change_managed(|current| {
            let mut graphs = current.map_or_else(
                || ManagedGraphs {
                    revision: 0,
                    owners: BTreeMap::new(),
                },
                |i| i.graphs.clone(),
            );
            let mut files = current.map(|i| i.settings.clone()).unwrap_or_default();
            graphs.revision += 1;
            graphs.owners.insert(owner, entry);
            files.insert(owner, Arc::from(settings));
            Ok((Installed::new(graphs, files)?, Some(owner)))
        })
    }

    /// Stop managing `graph`, which `owner` manages, so that ordinary writes can change
    /// it again. The owner keeps its other graphs and its settings. Dependent outputs
    /// are the owner's to mark unavailable.
    pub fn detach_managed(&self, owner: OwnerKind, graph: &str) -> Result<ManagedGraphs> {
        self.change_managed(|current| {
            let Some(current) = current else {
                return Err(not_enabled(owner));
            };
            let mut graphs = current.graphs.clone();
            let Some(entry) = graphs.owners.get_mut(&owner) else {
                return Err(not_enabled(owner));
            };
            let Some(at) = entry.graphs.iter().position(|g| g == graph) else {
                return Err(Error::NotFound(format!(
                    "graph <{graph}> is not managed by {owner}"
                )));
            };
            entry.graphs.remove(at);
            graphs.revision += 1;
            Ok((Installed::new(graphs, current.settings.clone())?, None))
        })
    }

    /// Replace the registry with what `change` makes of the installed one, writing the
    /// owner settings file of the owner it names first.
    fn change_managed(
        &self,
        change: impl FnOnce(Option<&Installed>) -> Result<(Installed, Option<OwnerKind>)>,
    ) -> Result<ManagedGraphs> {
        crate::sparql::extensions::check_family(self.owner_dataset_id())?;
        if self.is_branch() {
            return Err(crate::branch::invalid_branch(
                "managed graphs are changed through the dataset's main store",
            ));
        }
        let o = crate::guard::WriteOptions::default();
        let w = self.lock_writer(&o)?;
        if w.poisoned {
            return Err(Error::Poisoned);
        }
        let current = self.managed_state();
        let installed = match current.as_deref() {
            None => None,
            Some(ManagedState::Installed(i)) => Some(i),
            Some(ManagedState::Unavailable {
                repairable: true, ..
            }) => {
                // A settings file that does not match: the owner's entry is written
                // again from the files on disk, which this build reads.
                None
            }
            Some(ManagedState::Unavailable { reason, .. }) => {
                return Err(ManagedError::error(
                    ManagedErrorKind::Unavailable,
                    format!("the managed-graph registry cannot be changed by this build: {reason}"),
                ));
            }
        };
        let repaired;
        let installed = match (installed, current.as_deref(), &self.root) {
            (None, Some(ManagedState::Unavailable { .. }), Some(root)) => {
                repaired = recorded_entries(root)?;
                repaired.as_ref()
            }
            (i, _, _) => i,
        };
        let (next, written) = change(installed)?;
        for (owner, entry) in &next.graphs.owners {
            let matches = next
                .settings
                .get(owner)
                .is_some_and(|b| crate::managed::settings_digest(b) == entry.settings_digest);
            if !matches {
                return Err(ManagedError::error(
                    ManagedErrorKind::Unavailable,
                    format!(
                        "{} does not match its recorded digest; enable {owner} again with its settings",
                        owner.settings_file()
                    ),
                ));
            }
        }
        // From the first registry file written until the new registry is installed,
        // the files may no longer match the installed registry. An error or unwind
        // in between leaves the family refusing every write, so that no ordinary
        // write lands under a registry that may already be the old one on disk.
        let mut fence = UncertainRegistry {
            family: &self.family,
            armed: false,
        };
        if let Some(root) = &self.root {
            // 1. the reader that enforces the registry, before anything relies on it
            crate::commit::require_reader(root, crate::managed::READER)?;
            self.failpoint("managed-reader-raised");
            fence.armed = true;
            // 2. the owner's settings, which the registry's digest names
            if let Some(owner) = written {
                let bytes = &next.settings[&owner];
                write_atomic(&root.join(owner.settings_file()), bytes)?;
                sync_dir(root)?;
            }
            self.failpoint("managed-settings-written");
            // 3. the commit point
            write_atomic(&root.join(crate::managed::FILE), &next.file_bytes())?;
            sync_dir(root)?;
            self.failpoint("managed-published");
        }
        let graphs = next.graphs.clone();
        // 4. main and every branch admit against the new registry from here on
        self.family
            .managed
            .store(Some(Arc::new(ManagedState::Installed(next))));
        fence.armed = false;
        drop(w);
        self.managed_barrier()?;
        Ok(graphs)
    }

    /// Wait until every resident branch has published or refused the commits it may
    /// have admitted against the previous registry. A branch writer admits under its
    /// own writer lock, so taking and releasing each lock, with its pending group
    /// commits drained, is enough. A branch that is not resident has no writer.
    fn managed_barrier(&self) -> Result<()> {
        let Some(set) = self.branch_set() else {
            return Ok(());
        };
        let o = crate::guard::WriteOptions::default();
        for branch in set.open_stores() {
            drop(branch.lock_writer(&o)?);
        }
        Ok(())
    }

    /// Admit a candidate commit's changes, as WAL records, against the registry: a
    /// managed graph changes only with its owner's capability. A single load and a
    /// `None` check when no registry is installed and no capability is given.
    pub(crate) fn admit_managed_log(
        &self,
        snap: &Snapshot,
        log: &[(u8, [Id; 4])],
        managed: Option<&ManagedWrite>,
    ) -> Result<()> {
        self.admit_managed_ids(snap, log.iter().map(|(_, q)| q[3]), managed)
    }

    /// [`admit_managed_log`](Self::admit_managed_log) for the graphs `graphs`.
    pub(crate) fn admit_managed_ids(
        &self,
        snap: &Snapshot,
        graphs: impl Iterator<Item = Id>,
        managed: Option<&ManagedWrite>,
    ) -> Result<()> {
        let state = self.family.managed.load();
        if state.is_none() && managed.is_none() {
            return Ok(());
        }
        let Some(mut admission) = Admission::start(state.as_ref(), managed)? else {
            return Ok(());
        };
        let mut last = None;
        for g in graphs {
            if last == Some(g) || g == Id::DEFAULT_GRAPH {
                continue;
            }
            last = Some(g);
            admission.graph(g.0, || snap.key(g))?;
        }
        Ok(())
    }

    /// Admit changes given as vocabulary keys of their graphs (a merge plan's).
    pub(crate) fn admit_managed_keys<'k>(
        &self,
        graphs: impl Iterator<Item = &'k [u8]>,
    ) -> Result<()> {
        let state = self.family.managed.load();
        let Some(admission) = Admission::start(state.as_ref(), None)? else {
            return Ok(());
        };
        for key in graphs {
            admission.key(key)?;
        }
        Ok(())
    }

    /// The managed graph IRIs a bulk build refuses quads in, after refusing an
    /// unavailable registry. `None` when no graph is managed.
    pub(crate) fn managed_fence(&self) -> Result<Option<GraphFence>> {
        let state = self.family.managed.load();
        Ok(Admission::start(state.as_ref(), None)?.map(|a| a.installed().iris()))
    }

    /// The registry's files for a backup or clone: `managed.json` and each owner's
    /// settings, as installed. An unavailable registry is copied from the directory as
    /// it is, so that the copy refuses writes too.
    pub(crate) fn managed_files(&self) -> Result<Vec<(&'static str, Vec<u8>)>> {
        match self.managed_state().as_deref() {
            None => Ok(Vec::new()),
            Some(ManagedState::Installed(i)) => {
                let mut out = vec![(crate::managed::FILE, i.file_bytes())];
                for (owner, bytes) in &i.settings {
                    out.push((owner.settings_file(), bytes.to_vec()));
                }
                Ok(out)
            }
            Some(ManagedState::Unavailable { .. }) => {
                let Some(root) = self.managed_root() else {
                    return Err(ManagedError::error(
                        ManagedErrorKind::Unavailable,
                        "the managed-graph registry cannot be enforced or copied",
                    ));
                };
                let mut out = Vec::new();
                for name in std::iter::once(crate::managed::FILE)
                    .chain(OwnerKind::ALL.iter().map(|o| o.settings_file()))
                {
                    match std::fs::read(root.join(name)) {
                        Ok(b) => out.push((name, b)),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                Ok(out)
            }
        }
    }

    /// The directory that holds the family's `managed.json`: the dataset root.
    fn managed_root(&self) -> Option<std::path::PathBuf> {
        if !self.is_branch() {
            return self.root.clone();
        }
        self.branch_set()
            .filter(|s| !s.is_memory())
            .map(|s| s.root().to_path_buf())
    }

    /// Install the registry of a copy made from `files` ([`Self::managed_files`]),
    /// such as an in-memory clone's.
    pub(crate) fn install_managed_files(&self, files: &[(&'static str, Vec<u8>)]) {
        let Some((_, bytes)) = files.iter().find(|(n, _)| *n == crate::managed::FILE) else {
            return;
        };
        let state = crate::managed::decode(bytes, |owner| {
            Ok(files
                .iter()
                .find(|(n, _)| *n == owner.settings_file())
                .map(|(_, b)| b.clone()))
        });
        self.family.managed.store(Some(Arc::new(state)));
    }
}

/// Fences a family whose registry files changed without the new registry being
/// installed: every write is refused until the owner is enabled again.
struct UncertainRegistry<'a> {
    family: &'a super::family::Family,
    armed: bool,
}

impl Drop for UncertainRegistry<'_> {
    fn drop(&mut self) {
        if self.armed {
            tracing::error!(target: "sparkles::store", "a managed-graph registry change did not complete; writes are refused until it is made again");
            self.family
                .managed
                .store(Some(Arc::new(ManagedState::Unavailable {
                    reason: "a change of the registry did not complete; enable the owner again"
                        .into(),
                    repairable: true,
                })));
        }
    }
}

/// The owners recorded on disk, read without their digests, for repairing a registry
/// whose settings file does not match.
fn recorded_entries(root: &Path) -> Result<Option<Installed>> {
    #[derive(serde::Deserialize)]
    struct Raw {
        revision: u64,
        owners: BTreeMap<OwnerKind, OwnerEntry>,
    }
    let bytes = match std::fs::read(root.join(crate::managed::FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut raw: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Corrupt(format!("{}: {e}", crate::managed::FILE)))?;
    if let Some(o) = raw.as_object_mut() {
        o.remove("format");
    }
    let raw: Raw = serde_json::from_value(raw)
        .map_err(|e| Error::Corrupt(format!("{}: {e}", crate::managed::FILE)))?;
    let mut settings = BTreeMap::new();
    for owner in raw.owners.keys() {
        if let Ok(b) = std::fs::read(root.join(owner.settings_file())) {
            settings.insert(*owner, Arc::from(b));
        }
    }
    Installed::new(
        ManagedGraphs {
            revision: raw.revision,
            owners: raw.owners,
        },
        settings,
    )
    .map(Some)
}

fn not_enabled(owner: OwnerKind) -> Error {
    ManagedError::error(
        ManagedErrorKind::UnsupportedProfile,
        format!("the {owner} profile does not manage graphs in this dataset"),
    )
}
