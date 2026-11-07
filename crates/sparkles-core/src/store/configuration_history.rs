//! Primary data/configuration pair selection. This never certifies a caller's
//! digest, opens an inherited writer, or substitutes the current Head binding.
use super::*;
use crate::access::Preparation;
use crate::history::{ConfigurationBinding, ConfigurationIdentity, HistoryOptions, ReadLimit};

pub(super) mod primary;

#[cfg(test)]
mod tests;

impl Store {
    /// Call under this Store's writer and before acquiring its history mutex.
    /// The data head cannot move while a Head pair is captured; a concurrent main
    /// configure on a branch may advance afterward without changing this pin.
    pub(super) fn pin_configuration_at(
        &self,
        at: &crate::history::At,
        seq: u64,
        preparation: &Preparation,
    ) -> Result<Option<crate::history::PinnedConfiguration>> {
        let state = self.managed_state_shared();
        let installed = match state.as_deref() {
            None => return Ok(None),
            Some(crate::managed::ManagedState::Installed(installed)) => installed,
            Some(crate::managed::ManagedState::Unavailable { reason, .. }) => {
                return Err(managed_history::unavailable(reason.clone()));
            }
        };
        let Some(history) = &installed.configuration_history else {
            return Ok(None);
        };
        let _copy = preparation.reserve(512)?;
        let binding = match at {
            crate::history::At::Head => {
                managed_history::binding(history.identity(self.owner_dataset_id()))
            }
            crate::history::At::Commit(_) | crate::history::At::Time(_) => {
                self.configuration_binding_at(seq, preparation)?
            }
            crate::history::At::Snapshot(name) => {
                let configuration = if let Some(history) = &self.history {
                    preparation
                        .lock(history)?
                        .pins
                        .get(name)
                        .and_then(|pin| pin.configuration.clone())
                } else if let Some(history) = &self.mem_history {
                    preparation
                        .lock(history)?
                        .pins
                        .get(name)
                        .and_then(|(pin, _)| pin.configuration.clone())
                } else {
                    None
                };
                configuration.map_or(ConfigurationBinding::LegacyUnbound, |configuration| {
                    configuration.binding
                })
            }
        };
        let selected = crate::history::PinnedConfiguration {
            binding,
            selector: crate::history::ConfigurationSelector::capture(at),
        };
        selected.validate(self.owner_dataset_id())?;
        Ok(Some(selected))
    }

    /// Activate genuine configuration history for an in-memory family. This
    /// refuses live writers and never certifies data committed before activation.
    /// Persistent activation is a separate closed-store copy/check/swap operation.
    pub fn activate_memory_configuration_history(
        &self,
        preparation: &Preparation,
    ) -> Result<ConfigurationIdentity> {
        if self.root.is_some() || self.is_branch() {
            return Err(Error::invalid(
                "in-memory configuration activation needs the owning in-memory store",
            ));
        }
        preparation.check()?;
        let _admission = self.family.gate.try_admit()?;
        let writer = preparation.lock(&self.writer)?;
        if writer.poisoned {
            return Err(Error::Poisoned);
        }
        let state = self.managed_state_shared();
        let empty;
        let installed = match state.as_deref() {
            Some(crate::managed::ManagedState::Installed(installed)) => installed,
            Some(crate::managed::ManagedState::Unavailable { reason, .. }) => {
                return Err(managed_history::unavailable(reason.clone()));
            }
            None => {
                empty = crate::managed::Installed::new(
                    crate::managed::ManagedGraphs {
                        revision: 0,
                        owners: Default::default(),
                    },
                    Default::default(),
                )?;
                &empty
            }
        };
        if let Some(root) = &installed.configuration_history {
            return Ok(root.identity(self.owner_dataset_id()));
        }
        let lineage = uuid::Uuid::new_v4();
        let prepared = managed_history::prepare(
            self.owner_dataset_id(),
            lineage,
            1,
            None,
            installed,
            preparation,
        )?;
        // The actual exclusive permit excludes every family writer and branch
        // mutation. Memory branches are resident; physical deleted roots do not
        // exist in this mode. Reserve map nodes before constructing coverage.
        let stores = preparation.lock(&self.owned_set()?.stores)?;
        let _scratch = preparation.reserve(
            (stores.len() as u64)
                .checked_add(1)
                .and_then(|n| n.checked_mul(256))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let mut branches = std::collections::BTreeMap::new();
        let mut add = |id, head: u64| -> Result<()> {
            preparation.check()?;
            branches.insert(
                id,
                managed_history::BranchCoverage {
                    activation_head: head,
                    first_bound_own: head
                        .checked_add(1)
                        .ok_or_else(|| Error::invalid("configuration coverage commit overflow"))?,
                },
            );
            Ok(())
        };
        add(self.dataset_id, writer.head.seq)?;
        for (&id, store) in stores.iter() {
            add(id, store.snapshot().commit)?;
        }
        let coverage = managed_history::Coverage {
            format: managed_history::FORMAT,
            dataset_id: self.owner_dataset_id(),
            lineage_id: lineage,
            activation_generation: prepared.identity.generation,
            branches,
        };
        let registry_copy_bytes = (prepared.bytes.value.len() as u64)
            .saturating_mul(32)
            .saturating_add(4096);
        let (memory, root) =
            managed_history::MemoryHistory::initial(prepared, coverage, preparation)?;
        let snapshot = self.snapshot();
        self.check_memory(
            snapshot
                .generation
                .disk_bytes()
                .saturating_add(delta_bytes(&snapshot.delta))
                .saturating_add(snapshot.generation.dvocab.with(|v| v.bytes()) as u64)
                .saturating_add(memory.retained()),
        )?;
        let _registry_copy = preparation.reserve(registry_copy_bytes)?;
        let mut next =
            crate::managed::Installed::new(installed.graphs.clone(), installed.settings.clone())?;
        next.configuration_history = Some(root.clone());
        next.history_memory = Some(memory);
        preparation.check()?;
        self.family
            .managed
            .store(Some(Arc::new(crate::managed::ManagedState::Installed(
                next,
            ))));
        Ok(root.identity(self.owner_dataset_id()))
    }

    /// Read the original binding of a locally retained data commit. The caller
    /// may hold the writer; this method never reacquires it. Inherited selection
    /// is handled by the family reader, with logical and serving IDs kept apart.
    pub(super) fn configuration_binding_at(
        &self,
        seq: u64,
        preparation: &Preparation,
    ) -> Result<ConfigurationBinding> {
        preparation.check()?;
        let state = self.managed_state_shared();
        let installed = match state.as_deref() {
            Some(crate::managed::ManagedState::Unavailable { reason, .. }) => {
                return Err(managed_history::unavailable(reason.clone()));
            }
            Some(crate::managed::ManagedState::Installed(installed)) => Some(installed),
            None => None,
        };
        let root_origin = self
            .branching
            .ident
            .as_ref()
            .filter(|identity| identity.from.seq == seq)
            .and_then(|identity| identity.configuration_origin.as_ref());
        let binding =
            if let Some(origin) = root_origin {
                origin.validate(self.owner_dataset_id(), seq)?;
                origin.binding
            } else if let Some(memory) = &self.mem_history {
                preparation
                    .lock(memory)?
                    .configurations
                    .get(&seq)
                    .copied()
                    .unwrap_or_default()
            } else {
                // Hold the whole temporary decoder allowance before using the existing
                // cold reader. Its buffers are gone before this reservation returns.
                let allowance = preparation.room();
                let _held = preparation.reserve(allowance)?;
                let options = HistoryOptions {
                    cancel: Some(preparation.control().cancel.flag()),
                    deadline: preparation.control().deadline,
                };
                let limit = ReadLimit {
                    options: &options,
                    memory: Some(allowance),
                    held: 0,
                };
                let snapshot = self.snapshot();
                let retained = self.history.as_ref().ok_or_else(|| {
                    managed_history::unavailable("commit has no retained history")
                })?;
                let history = limit.lock(retained)?;
                let owner = history
                    .owner_limited(
                        seq,
                        commit::generation_number(&snapshot.generation.name),
                        snapshot.commit,
                        &limit,
                    )?
                    .ok_or_else(|| {
                        managed_history::unavailable("commit configuration is no longer retained")
                    })?;
                let entry = history.gens.get(&owner).ok_or_else(|| {
                    managed_history::unavailable("retained generation is missing")
                })?;
                let mut budget = cold::ColdBudget::new(limit)?;
                // Paths remain borrowed while history keeps this generation alive.
                budget.charge(
                    (entry.dir.as_os_str().len() as u64)
                        .saturating_mul(4)
                        .saturating_add(4096),
                )?;
                let physical = commit::read_gen_commit_controlled(&entry.dir, &mut budget)?
                    .ok_or_else(|| {
                        managed_history::unavailable("retained generation has no commit identity")
                    })?;
                if physical.value.dataset_id != self.dataset_id
                    || physical.value.commit.seq != entry.base.seq
                {
                    return Err(managed_history::unavailable(
                        "retained commit identity differs",
                    ));
                }
                if physical.value.commit.seq == seq {
                    physical.value.configuration
                } else {
                    let from = wal::WalPoint {
                        seq: entry.base.seq,
                        offset: 0,
                        folding: entry.fold_legacy,
                    };
                    let cursor_limit = budget.owned_limit()?;
                    let mut cursor = wal::WalCursor::open_limited(
                        &entry.dir.join("wal.log"),
                        from,
                        &cursor_limit,
                        0,
                    )?;
                    loop {
                        let found = match cursor.next_limited(&cursor_limit, 0)? {
                            Some((found, _, _)) => found,
                            None => {
                                return Err(managed_history::unavailable(
                                    "retained WAL has no selected binding",
                                ));
                            }
                        };
                        if found > seq {
                            return Err(managed_history::unavailable(
                                "retained WAL passed selected binding",
                            ));
                        }
                        if found == seq {
                            break match cursor.configuration() {
                                Some(generation) => {
                                    let history = installed
                                        .and_then(|i| i.configuration_history.as_ref())
                                        .ok_or_else(|| {
                                            managed_history::unavailable(
                                                "WAL binding has no published lineage",
                                            )
                                        })?;
                                    managed_history::binding(ConfigurationIdentity {
                                        dataset_id: self.owner_dataset_id(),
                                        lineage_id: history.lineage_id,
                                        generation,
                                    })
                                }
                                None => ConfigurationBinding::LegacyUnbound,
                            };
                        }
                    }
                }
            };
        let cutoff = self
            .branching
            .ident
            .as_ref()
            .and_then(|identity| identity.configuration_origin.as_ref())
            .map(|origin| origin.first_bound_own);
        self.authenticate_configuration_binding(self.dataset_id, seq, binding, cutoff, preparation)
    }

    /// Primary family authentication shared by resident and read-only origins.
    /// A cutoff from a new branch is accepted only from its checked descriptor.
    pub(super) fn authenticate_configuration_binding(
        &self,
        branch: uuid::Uuid,
        seq: u64,
        binding: ConfigurationBinding,
        branch_cutoff: Option<u64>,
        preparation: &Preparation,
    ) -> Result<ConfigurationBinding> {
        preparation.check()?;
        let state = self.managed_state_shared();
        let installed = match state.as_deref() {
            Some(crate::managed::ManagedState::Installed(installed)) => Some(installed),
            Some(crate::managed::ManagedState::Unavailable { reason, .. }) => {
                return Err(managed_history::unavailable(reason.clone()));
            }
            None => None,
        };
        let history = installed.and_then(|installed| installed.configuration_history.as_ref());
        binding.validate(self.owner_dataset_id())?;
        let Some(history) = history else {
            return match binding {
                ConfigurationBinding::LegacyUnbound
                | ConfigurationBinding::ImportedUnbound { .. } => Ok(binding),
                ConfigurationBinding::Known { .. } => Err(managed_history::unavailable(
                    "commit binding has no configuration publication",
                )),
            };
        };
        if let Some(root) = self.managed_root() {
            let coverage = managed_history::read_coverage(
                &root,
                self.owner_dataset_id(),
                history,
                preparation,
            )?;
            let cutoff = coverage
                .value
                .branches
                .get(&branch)
                .map(|cutoff| cutoff.first_bound_own)
                .or(branch_cutoff)
                .ok_or_else(|| {
                    managed_history::unavailable("branch configuration coverage is missing")
                })?;
            if seq >= cutoff && binding.configuration().is_none() {
                return Err(managed_history::unavailable(
                    "covered commit is missing its configuration binding",
                ));
            }
            if let Some(identity) = binding.configuration() {
                managed_history::resolve(
                    &root,
                    self.owner_dataset_id(),
                    history,
                    identity,
                    preparation,
                )?;
            }
        } else {
            let memory = installed
                .and_then(|installed| installed.history_memory.as_ref())
                .ok_or_else(|| {
                    managed_history::unavailable("configuration memory history is missing")
                })?;
            let cutoff = memory
                .coverage
                .value
                .branches
                .get(&branch)
                .map(|cutoff| cutoff.first_bound_own)
                .or(branch_cutoff)
                .ok_or_else(|| {
                    managed_history::unavailable("branch configuration coverage is missing")
                })?;
            if seq >= cutoff && binding.configuration().is_none() {
                return Err(managed_history::unavailable(
                    "covered commit is missing its configuration binding",
                ));
            }
            if let Some(identity) = binding.configuration() {
                let mut node = Some(memory.as_ref());
                loop {
                    preparation.check()?;
                    let current = node.ok_or_else(|| {
                        managed_history::unavailable("configuration was not published in memory")
                    })?;
                    if current.prepared.identity == identity {
                        break;
                    }
                    node = current.previous.as_deref();
                }
            }
        }
        Ok(binding)
    }
}
