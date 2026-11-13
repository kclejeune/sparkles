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
    // format 3 is written only once configuration history is activated, so in a
    // dataset without it the file is a newer release's registry
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && let Some(format) = value.get("format").and_then(serde_json::Value::as_u64)
        && format >= 3
        && let Some(reader) = crate::commit::read_dataset_versions(root)
            .ok()
            .flatten()
            .map(|versions| versions.minimum_reader)
        && let Err(error) = crate::formats::admit_configuration_version(
            reader,
            crate::managed::FILE,
            u32::try_from(format).unwrap_or(u32::MAX),
        )
    {
        return Some(ManagedState::Unavailable {
            reason: error.to_string(),
            repairable: false,
        });
    }
    let mut state = crate::managed::decode(&bytes, |owner| {
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
    // A settings mismatch can still be repaired against a verified old root. A
    // missing/corrupt anchored lineage cannot be reset through that repair path.
    let history_check = crate::managed::recorded_history(&bytes).and_then(|history| {
        if history.is_some() {
            verified_history_registry(root, &bytes)?;
        }
        Ok(())
    });
    if let Err(error) = history_check {
        state = ManagedState::Unavailable {
            reason: error.to_string(),
            repairable: false,
        };
    }
    if let ManagedState::Unavailable { reason, .. } = &state {
        tracing::error!(target: "sparkles::store", "{}: writes are refused: {reason}", root.display());
    }
    Some(state)
}

/// A coherent view of a dataset's managed-graph registry
/// ([`Store::managed_snapshot`]).
#[derive(Clone, Debug)]
pub struct ManagedSnapshot {
    pub graphs: ManagedGraphs,
    /// Each owner's settings file, as the registry records it, or as found on disk
    /// when the registry is unavailable.
    pub settings: BTreeMap<OwnerKind, Arc<[u8]>>,
    /// Why writes are refused until an owner is enabled again, when they are.
    pub unavailable: Option<String>,
}

/// Retains the one actual admission state through guards, bulk builds and queues.
pub(super) struct AdmittedManaged {
    pub(super) configuration: crate::history::ConfigurationBinding,
    state: Option<Arc<ManagedState>>,
}

impl AdmittedManaged {
    pub(super) fn admit_ids(
        &self,
        snap: &Snapshot,
        graphs: impl Iterator<Item = Id>,
        capability: Option<&ManagedWrite>,
    ) -> Result<()> {
        Store::admit_managed_ids_at(self.state.as_ref(), snap, graphs, capability)
    }

    pub(super) fn fence(&self) -> Result<Option<GraphFence>> {
        Ok(Admission::start(self.state.as_ref(), None)?.map(|a| a.installed().iris()))
    }
}

impl Store {
    /// Raise the dataset's `minimumReader` to at least `reader` durably, before the
    /// first file that reader needs is written. A store without a directory does
    /// nothing. Through the main store of the dataset.
    pub fn require_dataset_reader(&self, reader: u32) -> Result<()> {
        if reader > crate::commit::DATASET_READER {
            return Err(Error::invalid(format!(
                "reader {reader} is above what Sparkles {} reads",
                crate::formats::SPARKLES_VERSION
            )));
        }
        match self.managed_root() {
            Some(root) => crate::commit::require_reader(&root, reader),
            None => Ok(()),
        }
    }

    fn managed_state(&self) -> Option<Arc<ManagedState>> {
        self.family.managed.load_full()
    }

    /// The running registry state, shared and not copied, for controlled readers.
    pub(crate) fn managed_state_shared(&self) -> Option<Arc<ManagedState>> {
        self.managed_state()
    }

    /// Whether this store's family has activated configuration history, which
    /// selects the newer versions of the files it writes from now on (see
    /// [`super::managed_history::activated`]).
    pub(crate) fn configuration_activated(&self) -> bool {
        super::managed_history::activated(
            self.managed_state().as_deref(),
            self.managed_root().as_deref(),
        )
    }

    pub(super) fn same_managed_state(&self, captured: &Option<Arc<ManagedState>>) -> bool {
        match (self.managed_state(), captured) {
            (None, None) => true,
            (Some(current), Some(captured)) => Arc::ptr_eq(&current, captured),
            _ => false,
        }
    }

    /// The managed graphs that leave every caller's read view: those of owners whose
    /// visibility is `hidden`. Empty without a registry. A registry this build cannot
    /// enforce answers the graphs its file still names as hidden, when the file reads,
    /// together with the graphs that every owner whose settings file is present always
    /// hides, so that a damaged or unreadable registry does not disclose them.
    pub fn hidden_graphs(&self) -> Vec<String> {
        match self.managed_state().as_deref() {
            None => Vec::new(),
            Some(ManagedState::Installed(i)) => i.graphs.hidden_graphs(),
            Some(ManagedState::Unavailable { .. }) => {
                let Some(root) = self.managed_root() else {
                    return Vec::new();
                };
                let mut hidden = std::fs::read(root.join(crate::managed::FILE))
                    .ok()
                    .and_then(|bytes| crate::managed::hidden_graphs_of_file(&bytes))
                    .unwrap_or_default();
                for owner in OwnerKind::ALL {
                    if root.join(owner.settings_file()).is_file() {
                        hidden.extend(owner.always_hidden_graphs());
                    }
                }
                hidden.sort_unstable();
                hidden.dedup();
                hidden
            }
        }
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
    /// that has not yet published is admitted against it: it waits for every writer of
    /// the dataset and its branches that entered before the new registry was installed,
    /// so a thread that holds one of their write transactions must not call it. Those
    /// writers can still take nested family operations while it waits. A change that fails or unwinds after its
    /// first file is written leaves every write refused until the owner is enabled
    /// again.
    pub fn enable_managed(
        &self,
        owner: OwnerKind,
        entry: OwnerEntry,
        settings: &[u8],
    ) -> Result<ManagedGraphs> {
        self.enable_managed_checked(owner, entry, settings, None)
    }

    /// [`enable_managed`](Self::enable_managed) only while the registry's revision is
    /// still `expected`, which is 0 when no registry is installed. Under the writer lock
    /// of main, any other revision refuses with a conflict and writes nothing, so a
    /// caller that derived `entry` from the registry it read cannot overwrite a change
    /// made since.
    pub fn enable_managed_at(
        &self,
        owner: OwnerKind,
        entry: OwnerEntry,
        settings: &[u8],
        expected: u64,
    ) -> Result<ManagedGraphs> {
        self.enable_managed_checked(owner, entry, settings, Some(expected))
    }

    /// The bytes of `owner`'s settings file as the installed registry records them,
    /// `None` when the owner is not enabled. Fails with `managed-unavailable` when the
    /// registry cannot be enforced.
    pub fn managed_settings(&self, owner: OwnerKind) -> Result<Option<Arc<[u8]>>> {
        match self.managed_state().as_deref() {
            None => Ok(None),
            Some(ManagedState::Installed(i)) => Ok(i.settings.get(&owner).cloned()),
            Some(ManagedState::Unavailable { reason, .. }) => Err(ManagedError::error(
                ManagedErrorKind::Unavailable,
                reason.clone(),
            )),
        }
    }

    fn enable_managed_checked(
        &self,
        owner: OwnerKind,
        entry: OwnerEntry,
        settings: &[u8],
        expected: Option<u64>,
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
        if let Some(why) = owner.check_settings(settings, entry.profile) {
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
            let revision = current.map_or(0, |i| i.graphs.revision);
            if let Some(expected) = expected
                && expected != revision
            {
                return Err(Error::Conflict(format!(
                    "the managed-graph registry is at revision {revision}, not {expected}; read it again"
                )));
            }
            let mut graphs = current.map_or_else(
                || ManagedGraphs {
                    revision: 0,
                    owners: BTreeMap::new(),
                },
                |i| i.graphs.clone(),
            );
            let mut files = current.map(|i| i.settings.clone()).unwrap_or_default();
            graphs.revision = graphs.revision.checked_add(1)
                .ok_or_else(|| Error::invalid("managed registry revision overflow"))?;
            // A graph that stays protected keeps its nonce; one that becomes protected
            // gets a new random one, which nobody could have known before.
            let previous = graphs.owners.get(&owner);
            let mut entry = entry;
            entry.protections = entry
                .graphs
                .iter()
                .map(|g| {
                    let kept = previous
                        .filter(|p| p.graphs.contains(g))
                        .and_then(|p| p.protections.get(g).copied());
                    (g.clone(), kept.unwrap_or_else(uuid::Uuid::new_v4))
                })
                .collect();
            graphs.owners.insert(owner, entry);
            files.insert(owner, Arc::from(settings));
            Ok((Installed::new(graphs, files)?, Some(owner)))
        })
    }

    /// Stop managing `graph`, which `owner` manages, so that ordinary writes can change
    /// it again. The owner keeps its other graphs and its settings. Dependent outputs
    /// are the owner's to mark unavailable.
    pub fn detach_managed(&self, owner: OwnerKind, graph: &str) -> Result<ManagedGraphs> {
        self.detach_managed_checked(owner, graph, None)
    }

    /// [`detach_managed`](Self::detach_managed) only while the registry's revision is
    /// still `expected`. Any other revision refuses with a conflict and writes nothing,
    /// so a caller that checked the registry it read cannot detach under a newer one.
    pub fn detach_managed_at(
        &self,
        owner: OwnerKind,
        graph: &str,
        expected: u64,
    ) -> Result<ManagedGraphs> {
        self.detach_managed_checked(owner, graph, Some(expected))
    }

    /// One coherent view of the dataset's registry: its graphs and the owner settings
    /// files it records, from a single load. `None` without a registry. A registry
    /// that enabling an owner again repairs, such as one whose owner settings file does
    /// not match its digest or one whose change did not complete, is returned as
    /// `managed.json` records it, or empty at revision 0 when there is no such file,
    /// with the settings files on disk and [`ManagedSnapshot::unavailable`] set. Such a
    /// registry of a store without a directory, as an in-memory clone can inherit,
    /// fails with `managed-unavailable`. Any other registry that cannot
    /// be enforced fails with `managed-unavailable`.
    pub fn managed_snapshot(&self) -> Result<Option<ManagedSnapshot>> {
        let Some(state) = self.managed_state() else {
            return Ok(None);
        };
        match &*state {
            ManagedState::Installed(i) => Ok(Some(ManagedSnapshot {
                graphs: i.graphs.clone(),
                settings: i.settings.clone(),
                unavailable: None,
            })),
            ManagedState::Unavailable {
                reason,
                repairable: true,
            } => {
                // What `managed.json` records, or no registry at all when a first
                // enablement stopped before writing it: the same state that enabling
                // the owner again starts from.
                // A store without a directory, such as an in-memory clone of a
                // dataset whose change did not complete, has no recorded registry to
                // repair from, so it is not repaired from nothing.
                let Some(root) = self.managed_root() else {
                    return Err(rootless_repair(reason));
                };
                let recorded = recorded_entries(&root).map_err(|e| {
                    ManagedError::error(
                        ManagedErrorKind::Unavailable,
                        format!("{reason}; {}: {e}", crate::managed::FILE),
                    )
                })?;
                let (graphs, settings) = match recorded {
                    Some(i) => (i.graphs, i.settings),
                    None => (
                        ManagedGraphs {
                            revision: 0,
                            owners: BTreeMap::new(),
                        },
                        BTreeMap::new(),
                    ),
                };
                Ok(Some(ManagedSnapshot {
                    graphs,
                    settings,
                    unavailable: Some(reason.clone()),
                }))
            }
            ManagedState::Unavailable { reason, .. } => Err(ManagedError::error(
                ManagedErrorKind::Unavailable,
                reason.clone(),
            )),
        }
    }

    fn detach_managed_checked(
        &self,
        owner: OwnerKind,
        graph: &str,
        expected: Option<u64>,
    ) -> Result<ManagedGraphs> {
        self.change_managed(|current| {
            let Some(current) = current else {
                return Err(not_enabled(owner));
            };
            if let Some(expected) = expected
                && expected != current.graphs.revision
            {
                return Err(Error::Conflict(format!(
                    "the managed-graph registry is at revision {}, not {expected}; read it again",
                    current.graphs.revision
                )));
            }
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
            // Protecting the graph again chooses a new nonce, so a record written while
            // it is detached never carries the nonce of a protection.
            entry.protections.remove(graph);
            graphs.revision = graphs
                .revision
                .checked_add(1)
                .ok_or_else(|| Error::invalid("managed registry revision overflow"))?;
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
            // Repairing without the recorded files could drop protected graphs.
            (None, Some(ManagedState::Unavailable { reason, .. }), None) => {
                return Err(rootless_repair(reason));
            }
            (i, _, _) => i,
        };
        let (mut next, written) = change(installed)?;
        // The write guard checks every commit under this writer lock with the hidden
        // graphs of the registry it finds, so it never reads one it does not know is
        // hidden. A change that would hide a graph the guard reads by name is refused
        // (spec C26 §2.3).
        let sorted = |mut g: Vec<String>| {
            g.sort_unstable();
            g.dedup();
            g
        };
        let hidden_before = sorted(self.hidden_graphs());
        let hidden_after = sorted(next.graphs.hidden_graphs());
        let guard = (hidden_before != hidden_after)
            .then(|| self.guard())
            .flatten();
        if let Some(g) = &guard {
            let reads = g.reads_graphs();
            if let Some(x) = hidden_after
                .iter()
                .find(|h| !hidden_before.contains(h) && reads.contains(h))
            {
                return Err(Error::Conflict(format!(
                    "the write guard reads <{x}>, which this change would hide; configure the guard without it first"
                )));
            }
        }
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
        let previous_history = installed.and_then(|i| i.configuration_history.as_ref());
        let preparation = previous_history
            .map(|_| super::managed_history::administrative_preparation())
            .transpose()?;
        let mut prepared_history = match (previous_history, preparation.as_ref()) {
            (Some(history), Some(preparation)) => {
                let ordinal = if let Some(root) = &self.root {
                    let current = super::managed_history::resolve(
                        root,
                        self.owner_dataset_id(),
                        history,
                        history.identity(self.owner_dataset_id()),
                        preparation,
                    )?;
                    current.value.ordinal
                } else {
                    installed
                        .and_then(|i| i.history_memory.as_ref())
                        .filter(|memory| {
                            memory.prepared.identity == history.identity(self.owner_dataset_id())
                        })
                        .ok_or_else(|| {
                            super::managed_history::unavailable(
                                "in-memory configuration lineage missing",
                            )
                        })?
                        .prepared
                        .document
                        .value
                        .ordinal
                };
                let prepared = super::managed_history::prepare(
                    self.owner_dataset_id(),
                    history.lineage_id,
                    ordinal
                        .checked_add(1)
                        .ok_or_else(|| Error::invalid("configuration ordinal overflow"))?,
                    Some(history.current_generation),
                    &next,
                    preparation,
                )?;
                next.configuration_history = Some(super::managed_history::Root {
                    current_generation: prepared.identity.generation,
                    ..history.clone()
                });
                Some(prepared)
            }
            _ => None,
        };
        if self.root.is_none()
            && let Some(prepared) = prepared_history.take()
        {
            let previous = installed
                .and_then(|i| i.history_memory.as_ref())
                .ok_or_else(|| {
                    super::managed_history::unavailable("in-memory configuration lineage missing")
                })?
                .clone();
            let old_size = previous.retained();
            let memory = super::managed_history::MemoryHistory::next(prepared, previous)?;
            let snapshot = self.snapshot();
            let data = snapshot
                .generation
                .disk_bytes()
                .checked_add(super::delta_bytes(&snapshot.delta))
                .and_then(|n| n.checked_add(snapshot.generation.dvocab.with(|v| v.bytes()) as u64))
                .and_then(|n| n.checked_add(memory.retained() - old_size))
                .ok_or_else(|| Error::invalid("configuration memory size overflow"))?;
            self.check_memory(data)?;
            next.history_memory = Some(memory);
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
            let _quota_invalidation = RegistryQuota(&self.quota);
            // 1. the reader that enforces the registry, before anything relies on it
            crate::commit::require_reader(
                root,
                if previous_history.is_some() {
                    super::managed_history::READER
                } else {
                    crate::managed::READER
                },
            )?;
            self.failpoint("managed-reader-raised");
            let _history_reservation = if let (Some(prepared), Some(preparation)) =
                (prepared_history.as_ref(), preparation.as_ref())
            {
                let additional = (prepared.bytes.value.len() as u64)
                    .checked_add(next.settings.values().map(|b| b.len() as u64).sum::<u64>())
                    .and_then(|n| {
                        n.checked_add((prepared.bytes.value.len() as u64).saturating_mul(2))
                    })
                    .ok_or_else(|| Error::invalid("configuration disk size overflow"))?;
                let reservation = self.quota.reserve(additional);
                self.check_disk(self.quota.unwritten())?;
                self.quota.check_commit(self.quota.unwritten())?;
                self.quota.invalidate();
                let persisted = super::managed_history::persist(root, prepared, preparation);
                self.quota.invalidate();
                persisted?;
                self.failpoint("managed-history-written");
                Some(reservation)
            } else {
                None
            };
            fence.armed = true;
            // 2. the owner's settings, which the registry's digest names
            if next.configuration_history.is_some() {
                // A verified historical root can repair interrupted settings. Write
                // every owner from that complete candidate, not an old digest paired
                // with another owner's damaged mutable file.
                for (owner, bytes) in &next.settings {
                    write_atomic(&root.join(owner.settings_file()), bytes)?;
                    sync_dir(root)?;
                }
            } else if let Some(owner) = written {
                let bytes = &next.settings[&owner];
                write_atomic(&root.join(owner.settings_file()), bytes)?;
                sync_dir(root)?;
            }
            self.failpoint("managed-settings-written");
            // 3. the commit point
            let _root_bytes = preparation
                .as_ref()
                .map(|preparation| {
                    let bytes = prepared_history
                        .as_ref()
                        .map_or(0, |p| p.bytes.value.len() as u64);
                    preparation.reserve(bytes.saturating_mul(2).saturating_add(4096))
                })
                .transpose()?;
            write_atomic(&root.join(crate::managed::FILE), &next.file_bytes())?;
            sync_dir(root)?;
            self.quota.invalidate();
            self.failpoint("managed-published");
        }
        let graphs = next.graphs.clone();
        // 4. main and every branch admit against the new registry from here on
        self.family
            .managed
            .store(Some(Arc::new(ManagedState::Installed(next))));
        fence.armed = false;
        if let Some(g) = &guard {
            g.hidden_changed(&hidden_after);
        }
        drop(w);
        self.managed_barrier()?;
        Ok(graphs)
    }

    /// Wait until every writer of the family that may have been admitted against the
    /// previous registry has published or refused. Each writer holds a family permit
    /// from before it takes its writer lock until its commit is durably published or
    /// refused, including a pending group commit whose submitter has gone and a writer
    /// of a branch that was deleted or retired while a handle to it is retained. The
    /// registry is installed before the gate starts a new cohort, so a permit of the
    /// new cohort admits against it, and the wait covers exactly the permits entered
    /// before. New permits are not held back, so a waited-for writer can still take
    /// nested family permits. While it waits, the pending group publications of each
    /// live member are driven when that member's writer is free. A thread that holds a
    /// write transaction of the family must not enable or detach managed graphs.
    fn managed_barrier(&self) -> Result<()> {
        let o = crate::guard::WriteOptions::default();
        let set = self.branch_set();
        self.family.gate.wait_for_cohorts_before(&o, || {
            self.failpoint("managed-barrier-waiting");
            let mut members: Vec<Arc<Store>> = Vec::new();
            if let Some(set) = &set {
                members.extend(set.stores.lock().values().cloned());
                members.extend(
                    set.openings
                        .lock()
                        .values()
                        .filter_map(|o| o.store.upgrade()),
                );
            }
            let drive = |m: &Store| {
                if let Some(mut w) = m.writer.try_lock() {
                    m.drain_group(&mut w, None).ok();
                }
            };
            drive(self);
            for m in &members {
                drive(m);
            }
            // Members are released after their writers, outside any lock.
            drop(members);
            Ok(())
        })
    }

    /// Admit a candidate commit's changes, as WAL records, against the registry: a
    /// managed graph changes only with its owner's capability. A single load and a
    /// `None` check when no registry is installed and no capability is given.
    pub(super) fn admit_managed_commit(
        &self,
        snap: &Snapshot,
        log: &[(u8, [Id; 4])],
        bulk: impl Iterator<Item = Id>,
        managed: Option<&ManagedWrite>,
    ) -> Result<AdmittedManaged> {
        // Both graph checks and the returned reference use this one actual state.
        // Configure may install a later state while guard callbacks run; it cannot
        // replace the reference that admitted this publication.
        let admitted = self.capture_managed_admission(managed)?;
        admitted.admit_ids(snap, log.iter().map(|(_, q)| q[3]), managed)?;
        admitted.admit_ids(snap, bulk, None)?;
        Ok(admitted)
    }

    /// The replay preview checks a log before its guard without publishing a
    /// transaction. Real commits retain their admitted state through publication.
    pub(super) fn admit_managed_log(
        &self,
        snapshot: &Snapshot,
        log: &[(u8, [Id; 4])],
        managed: Option<&ManagedWrite>,
    ) -> Result<()> {
        self.admit_managed_commit(snapshot, log, std::iter::empty(), managed)
            .map(|_| ())
    }

    pub(super) fn capture_managed_admission(
        &self,
        capability: Option<&ManagedWrite>,
    ) -> Result<AdmittedManaged> {
        let state = self.family.managed.load_full();
        Admission::start(state.as_ref(), capability)?;
        let configuration = match state.as_deref() {
            Some(ManagedState::Installed(i)) => i.configuration_history.as_ref().map_or(
                crate::history::ConfigurationBinding::LegacyUnbound,
                |history| {
                    super::managed_history::binding(history.identity(self.owner_dataset_id()))
                },
            ),
            _ => crate::history::ConfigurationBinding::LegacyUnbound,
        };
        Ok(AdmittedManaged {
            configuration,
            state,
        })
    }

    fn admit_managed_ids_at(
        state: Option<&Arc<ManagedState>>,
        snap: &Snapshot,
        graphs: impl Iterator<Item = Id>,
        managed: Option<&ManagedWrite>,
    ) -> Result<()> {
        if state.is_none() && managed.is_none() {
            return Ok(());
        }
        let Some(mut admission) = Admission::start(state, managed)? else {
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

    /// The registry's files for a backup or clone: `managed.json` and each owner's
    /// settings, as installed. An unavailable registry is copied from the directory as
    /// it is, so that the copy refuses writes too.
    pub(crate) fn managed_files(&self) -> Result<Vec<(&'static str, Vec<u8>)>> {
        let state = self.managed_state();
        self.managed_files_at(state.as_deref())
    }

    pub(super) fn managed_files_at(
        &self,
        state: Option<&ManagedState>,
    ) -> Result<Vec<(&'static str, Vec<u8>)>> {
        match state {
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
                // Only a registry whose owners, and so whose settings files, this
                // build can name is copied whole. Anything else would make a copy
                // without some of the registry's dependencies.
                let refuse = |why: &str| {
                    Err(ManagedError::error(
                        ManagedErrorKind::Unavailable,
                        format!("the managed-graph registry cannot be copied: {why}"),
                    ))
                };
                let bytes = match std::fs::read(root.join(crate::managed::FILE)) {
                    Ok(b) => b,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return refuse("its change did not complete; enable the owner again");
                    }
                    Err(e) => return Err(e.into()),
                };
                // A registry of a newer format is refused as a copy, as any other
                // registry whose dependencies this build cannot name.
                match crate::managed::recorded_history(&bytes) {
                    Ok(Some(_)) => {
                        return refuse("its anchored configuration history is unavailable");
                    }
                    Ok(None) => {}
                    Err(error) => return refuse(&error.to_string()),
                }
                let owners = match crate::managed::recorded_owners(&bytes) {
                    Ok(owners) => owners,
                    Err(why) => return refuse(&why),
                };
                let mut out = vec![(crate::managed::FILE, bytes)];
                for owner in owners {
                    let name = owner.settings_file();
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
    pub(super) fn managed_root(&self) -> Option<std::path::PathBuf> {
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

/// Registry/history files can remain after a failed preparation. Future quota
/// checks must measure those real bytes, including after an unwind.
struct RegistryQuota<'a>(&'a Arc<super::quota::Quota>);
impl Drop for RegistryQuota<'_> {
    fn drop(&mut self) {
        self.0.invalidate();
    }
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
    if crate::managed::recorded_history(&bytes)?.is_some() {
        return verified_history_registry(root, &bytes).map(Some);
    }
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

/// The old generation supplies exact owner bytes only after its entire anchored
/// lineage and the registry document have been checked. Current bytes alone are
/// never retroactively certified by this recovery helper.
fn verified_history_registry(root: &Path, bytes: &[u8]) -> Result<Installed> {
    let history = crate::managed::recorded_history(bytes)?
        .ok_or_else(|| super::managed_history::unavailable("configuration history root missing"))?;
    let dataset_id = crate::commit::read_dataset_file(root)?.ok_or_else(|| {
        super::managed_history::unavailable("configuration dataset identity missing")
    })?;
    let preparation = super::managed_history::administrative_preparation()?;
    let document = super::managed_history::resolve(
        root,
        dataset_id,
        &history,
        history.identity(dataset_id),
        &preparation,
    )?;
    match crate::managed::decode(bytes, |owner| {
        Ok(document
            .value
            .owner_settings
            .get(&owner)
            .map(|text| text.as_bytes().to_vec()))
    }) {
        ManagedState::Installed(installed) if document.value.matches_installed(&installed) => {
            Ok(installed)
        }
        _ => Err(super::managed_history::unavailable(
            "published configuration and registry differ",
        )),
    }
}

fn rootless_repair(reason: &str) -> Error {
    ManagedError::error(
        ManagedErrorKind::Unavailable,
        format!("{reason}; a store without a directory has no recorded registry to repair it from"),
    )
}

fn not_enabled(owner: OwnerKind) -> Error {
    ManagedError::error(
        ManagedErrorKind::UnsupportedProfile,
        format!("the {owner} profile does not manage graphs in this dataset"),
    )
}
