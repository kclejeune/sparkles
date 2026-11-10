//! Read-only origin descriptors captured under the family's opening/table guards.
//! Physical files describe lineage; they confer neither write nor receipt authority.
mod configuration;

use super::*;
use crate::store::cold::{ColdBudget, Loaded};

/// Validate immutable identity against the guarded table. A committed rename may
/// leave the descriptive name behind; the table's name remains authoritative.
fn check_identity(
    root: &Path,
    family: uuid::Uuid,
    entry: &Entry,
    budget: &mut ColdBudget<'_>,
) -> Result<Option<crate::history::ConfigurationBinding>> {
    let start = budget.used();
    budget.charge(
        (root.as_os_str().len() as u64)
            .saturating_mul(4)
            .saturating_add(2048),
    )?;
    let result = (|| {
        let bytes = budget.read_file(&root.join(BRANCH_FILE))?;
        let mut raw: Loaded<serde_json::Value> = budget.json(&bytes)?;
        if raw
            .value
            .get("format")
            .is_some_and(|value| !value.is_null() && !value.is_number())
        {
            return Err(Error::Corrupt(
                "branch.json: format is not a version".into(),
            ));
        }
        crate::formats::check_settings(BRANCH_FILE, &raw.value)?;
        if let Some(object) = raw.value.as_object_mut() {
            object.entry("format").or_insert(1.into());
        }
        let descriptor: BranchFile = budget.value(raw.value)?;
        descriptor.check_configuration(&bytes)?;
        if descriptor.dataset_id != family
            || descriptor.id != entry.id
            || descriptor.ordinal != entry.ordinal
            || descriptor.from != entry.from
        {
            return Err(Error::Corrupt(
                "retained branch descriptor disagrees with its family table".into(),
            ));
        }
        if commit::read_dataset_file_controlled(root, budget)? != Some(entry.id) {
            return Err(Error::Corrupt(
                "retained branch dataset identity is missing or foreign".into(),
            ));
        }
        budget.check()?;
        Ok(descriptor
            .configuration_origin
            .as_ref()
            .map(|origin| origin.binding))
    })();
    // Parsed names/extensions and temporary paths drop before their charge returns.
    budget.release(budget.used() - start);
    result
}

struct Current {
    name: String,
    number: u32,
    dir: PathBuf,
    base: CommitInfo,
    configuration: crate::history::ConfigurationBinding,
}

fn current_controlled(
    root: &Path,
    id: uuid::Uuid,
    budget: &mut ColdBudget<'_>,
) -> Result<Loaded<Current>> {
    let start = budget.used();
    let path_scratch = (root.as_os_str().len() as u64)
        .saturating_mul(6)
        .saturating_add(1024);
    budget.charge(path_scratch)?;
    let result = (|| {
        let bytes = budget.read_file(&root.join("CURRENT"))?;
        let selected = budget.trim_text(&bytes)?;
        let digits = selected
            .strip_prefix("gen-")
            .ok_or_else(|| Error::Corrupt("retained CURRENT does not name a generation".into()))?;
        if digits.is_empty() {
            return Err(Error::Corrupt(
                "retained CURRENT has no generation number".into(),
            ));
        }
        let mut number = 0u32;
        for digit in digits.bytes() {
            budget.check()?;
            if !digit.is_ascii_digit() {
                return Err(Error::Corrupt(
                    "retained CURRENT leaves its generation namespace".into(),
                ));
            }
            number = number
                .checked_mul(10)
                .and_then(|n| n.checked_add(u32::from(digit - b'0')))
                .ok_or_else(|| {
                    Error::Corrupt("retained CURRENT generation number overflows".into())
                })?;
        }
        // Select the actual directory entry before copying its bounded component
        // name. An arbitrarily long zero-padded CURRENT cannot force a giant
        // String/PathBuf copy before the filesystem refuses that component.
        let iteration_scratch = (64 << 10) + root.as_os_str().len() as u64 * 4;
        budget.charge(iteration_scratch)?;
        let mut found = None;
        for entry in std::fs::read_dir(root)? {
            budget.check()?;
            let entry = entry?;
            if entry.file_name() == selected {
                if !entry.file_type()?.is_dir() {
                    return Err(Error::Corrupt(
                        "retained CURRENT does not name a directory".into(),
                    ));
                }
                let name_charge = selected.len() as u64 + 64;
                let dir_charge = (root.as_os_str().len() as u64)
                    .saturating_add(selected.len() as u64)
                    .saturating_add(128)
                    .saturating_mul(2);
                budget.charge(
                    name_charge
                        .saturating_add(dir_charge)
                        .saturating_add(std::mem::size_of::<Current>() as u64),
                )?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::Corrupt("retained generation name is not UTF-8".into()))?;
                found = Some((name, entry.path()));
                break;
            }
        }
        let (name, dir) = found.ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "retained CURRENT generation is missing",
            ))
        })?;
        budget.release(iteration_scratch);
        budget.drop_vec(bytes);
        let physical = commit::read_gen_commit_controlled(&dir, budget)?.ok_or_else(|| {
            Error::Corrupt("retained CURRENT has no physical commit identity".into())
        })?;
        if physical.value.dataset_id != id {
            return Err(Error::Corrupt(
                "retained CURRENT belongs to another branch".into(),
            ));
        }
        let value = Current {
            name,
            number,
            dir,
            base: physical.value.commit,
            configuration: physical.value.configuration,
        };
        drop(physical.value);
        budget.release(physical.charge);
        budget.check()?;
        Ok(value)
    })();
    budget.release(path_scratch);
    match result {
        Ok(value) => Ok(Loaded {
            value,
            charge: budget.used() - start,
        }),
        Err(error) => {
            budget.release(budget.used() - start);
            Err(error)
        }
    }
}

fn prove_file(
    root: &Path,
    id: uuid::Uuid,
    generation: &Current,
    target: u64,
    terms: u64,
    budget: &mut ColdBudget<'_>,
) -> Result<wal::probe::ProvenWal> {
    prove_file_with(root, id, generation, target, terms, budget, &mut |_| Ok(()))
}

fn prove_file_with(
    root: &Path,
    id: uuid::Uuid,
    generation: &Current,
    target: u64,
    terms: u64,
    budget: &mut ColdBudget<'_>,
    visit: &mut dyn FnMut(wal::probe::WalCommitObservation) -> Result<()>,
) -> Result<wal::probe::ProvenWal> {
    let start = budget.used();
    budget.charge(
        (root.as_os_str().len() as u64 + generation.dir.as_os_str().len() as u64)
            .saturating_mul(4)
            .saturating_add(1024),
    )?;
    let result = (|| {
        let annotations = crate::annotations::seqs_controlled(root, id, budget)?;
        let mut primary = configuration::PrimaryReader::load(root, id, budget)?;
        primary.generation(generation, budget)?;
        let path = generation.dir.join("wal.log");
        let loaded = match budget.buffered(&path) {
            Ok(loaded) => loaded,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                budget.check()?;
                let point = wal::WalPoint {
                    seq: generation.base.seq,
                    offset: 0,
                };
                return Ok(wal::probe::ProvenWal {
                    head: point,
                    selected: (target == generation.base.seq).then_some(point),
                });
            }
            Err(error) => return Err(error),
        };
        let mut reader = loaded.value;
        let extent = reader.get_ref().metadata()?.len();
        let mut configuration_error = None;
        let proof = wal::probe::prove_prefix_with(
            &mut reader,
            extent,
            generation.base.seq,
            target,
            terms,
            annotations.value.as_deref(),
            &path,
            budget,
            &mut |observation| {
                // Observations are provisional until the original complete
                // framing/durable-bound proof succeeds. Never publish a prefix
                // merely because a configuration target happened to validate.
                if configuration_error.is_none()
                    && let Err(error) = primary.observation(observation, budget)
                {
                    configuration_error = Some(error);
                }
                visit(observation)
            },
        )?;
        if let Some(error) = configuration_error {
            return Err(error);
        }
        Ok(proof)
    })();
    // These are temporary proof buffers and membership, never retained on a view.
    budget.release(budget.used() - start);
    result
}

/// A resident Store lends the same bounded physical proof to a retained
/// generation before exposing its raw log through history/diff/change readers.
/// The caller holds history exclusion; no parent Store is opened or recovered.
pub(in crate::store) fn authenticate_retained_generation(
    store: &Store,
    entry: &crate::history::GenEntry,
    limit: &crate::history::ReadLimit<'_>,
    through: u64,
) -> Result<()> {
    let mut budget = ColdBudget::new(*limit)?;
    budget.charge(
        (entry.dir.as_os_str().len() as u64)
            .saturating_mul(4)
            .saturating_add(entry.name.len() as u64 * 4)
            .saturating_add(2048),
    )?;
    let physical = commit::read_gen_commit_controlled(&entry.dir, &mut budget)?;
    if physical.as_ref().is_some_and(|physical| {
        physical.value.dataset_id != store.dataset_id || physical.value.commit.seq != entry.base.seq
    }) {
        return Err(Error::Corrupt(
            "retained primary generation identity differs".into(),
        ));
    }
    let generation = Current {
        name: entry.name.clone(),
        number: commit::generation_number(&entry.name),
        dir: entry.dir.clone(),
        base: entry.base,
        configuration: physical.as_ref().map_or(
            crate::history::ConfigurationBinding::LegacyUnbound,
            |physical| physical.value.configuration,
        ),
    };
    if let Some(physical) = physical {
        drop(physical.value);
        budget.release(physical.charge);
    }
    let mut primary = configuration::PrimaryReader::for_store(store, &mut budget)?;
    primary.generation(&generation, &budget)?;
    let vocabulary = cold::head_vocabulary_controlled(
        &entry.dir,
        store.root.as_deref().ok_or_else(|| {
            Error::HistoryUnsupported("retained primary root is unavailable".into())
        })?,
        &mut budget,
    )?;
    let terms = vocabulary.value.len_limited(&budget.owned_limit()?)?;
    let root = store.root.as_deref().expect("checked persistent root");
    // This variant uses the actual resident family capture, including main.
    let annotations = crate::annotations::seqs_controlled(root, store.dataset_id, &mut budget)?;
    let path = entry.dir.join("wal.log");
    let loaded = match budget.buffered(&path) {
        Ok(loaded) => loaded,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let mut reader = loaded.value;
    let extent = reader.get_ref().metadata()?.len();
    let mut configuration_error = None;
    let mut observe = |observation| {
        if configuration_error.is_none()
            && let Err(error) = primary.observation(observation, &budget)
        {
            configuration_error = Some(error);
        }
        Ok(())
    };
    // A read of one commit needs that commit's prefix, as its replay reads it.
    // Readers of the whole log pass `u64::MAX` and prove all of it.
    if through == u64::MAX {
        wal::probe::prove_prefix_with(
            &mut reader,
            extent,
            entry.base.seq,
            u64::MAX,
            terms,
            annotations.value.as_deref(),
            &path,
            &budget,
            &mut observe,
        )?;
    } else {
        wal::probe::prove_prefix_through(
            &mut reader,
            extent,
            entry.base.seq,
            through,
            terms,
            annotations.value.as_deref(),
            &path,
            &budget,
            &mut observe,
        )?;
    }
    if let Some(error) = configuration_error {
        return Err(error);
    }
    budget.check()
}

fn head_controlled(
    root: &Path,
    id: uuid::Uuid,
    current: &Current,
    budget: &mut ColdBudget<'_>,
) -> Result<u64> {
    let start = budget.used();
    let result = (|| {
        let vocab = crate::store::cold::head_vocabulary_controlled(&current.dir, root, budget)?;
        let terms = vocab.value.len_limited(&budget.owned_limit()?)?;
        let proof = prove_file(root, id, current, u64::MAX, terms, budget)?;
        Ok(proof.head.seq)
    })();
    budget.release(budget.used() - start);
    result
}

mod catalog;
mod coverage;
use coverage::gone_controlled;
mod replay;
use catalog::{reconcile_tail, select_catalog};

fn generation_for(
    root: &Path,
    id: uuid::Uuid,
    current: u32,
    head: u64,
    seq: u64,
    budget: &mut ColdBudget<'_>,
) -> Result<Option<Loaded<Current>>> {
    let start = budget.used();
    let scratch = (64 << 10) + root.as_os_str().len() as u64 * 4;
    budget.charge(scratch)?;
    let result = (|| {
        let mut best: Option<Loaded<Current>> = None;
        for entry in std::fs::read_dir(root)? {
            budget.check()?;
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(text) = name.to_str() else {
                continue;
            };
            let Some(digits) = text.strip_prefix("gen-") else {
                continue;
            };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(number) = digits.parse::<u32>() else {
                continue;
            };
            if number > current
                || best
                    .as_ref()
                    .is_some_and(|best| best.value.number >= number)
            {
                continue;
            }
            let before = budget.used();
            let path_charge =
                (root.as_os_str().len() as u64 + text.len() as u64 + 128).saturating_mul(4);
            budget.charge(path_charge)?;
            let dir = entry.path();
            let Some(identity) = commit::read_gen_commit_controlled(&dir, budget)? else {
                drop(dir);
                budget.release(budget.used() - before);
                continue;
            };
            if identity.value.dataset_id != id || identity.value.commit.seq > seq {
                drop(identity.value);
                drop(dir);
                budget.release(budget.used() - before);
                continue;
            }
            budget.charge(
                (text.len() as u64)
                    .saturating_mul(4)
                    .saturating_add(128)
                    .saturating_add(std::mem::size_of::<Current>() as u64),
            )?;
            let value = Current {
                name: text.to_owned(),
                number,
                dir,
                base: identity.value.commit,
                configuration: identity.value.configuration,
            };
            drop(identity.value);
            budget.release(identity.charge);
            if generation_end(root, id, current, head, &value, budget)? < seq {
                drop(value);
                budget.release(budget.used() - before);
                continue;
            }
            let candidate = Loaded {
                value,
                charge: budget.used() - before,
            };
            if let Some(old) = best.replace(candidate) {
                drop(old.value);
                budget.release(old.charge);
            }
        }
        budget.check()?;
        Ok(best)
    })();
    budget.release(scratch);
    if result.is_err() {
        budget.release(budget.used() - start);
    }
    result
}

pub(in crate::store) enum Purpose {
    Snapshot,
    Configuration,
    Clock,
    TimeGone(i64),
}

pub(in crate::store) enum OriginRead {
    Clock(i64),
    Resident(Arc<Store>),
    View(Arc<Snapshot>, crate::history::Resolved),
    Configuration {
        binding: crate::history::ConfigurationBinding,
        first_bound_own: Option<u64>,
    },
    Inherited(CommitRef),
}

impl Store {
    /// The caller chose an explicit absent resident UUID. This method rechecks
    /// that choice under opening serialization and never catches another error as
    /// permission to search a different branch or recover a Store.
    pub(in crate::store) fn readonly_branch_snapshot_limited(
        &self,
        id: uuid::Uuid,
        at: &At,
        limit: &crate::history::ReadLimit<'_>,
    ) -> Result<OriginRead> {
        self.readonly_branch_read_limited(id, at, Purpose::Snapshot, limit)
    }

    pub(in crate::store) fn readonly_branch_read_limited(
        &self,
        id: uuid::Uuid,
        at: &At,
        purpose: Purpose,
        limit: &crate::history::ReadLimit<'_>,
    ) -> Result<OriginRead> {
        limit.check(0)?;
        if !matches!(at, At::Commit(_) | At::Time(_)) {
            return Err(Error::invalid(
                "cold inherited reads require a resolved commit or original time selector",
            ));
        }
        let set = self.owned_set()?;
        let _opening = limit.lock(&set.opening)?;
        if let Some(store) = limit.lock(&set.stores)?.get(&id).cloned() {
            return Ok(OriginRead::Resident(store));
        }
        if set.memory {
            return Err(branch::no_such_branch(&id.to_string()));
        }
        let table = limit.lock(&set.table)?;
        let entry = table.any_by_id_limited(id, limit)?.ok_or_else(|| {
            BranchError::error(
                BranchErrorKind::Gone,
                "merge-base-gone",
                "the retained origin branch was deleted",
            )
        })?;
        let mut budget = ColdBudget::new(*limit)?;
        budget.charge(
            (set.root.as_os_str().len() as u64)
                .saturating_mul(8)
                .saturating_add(4096),
        )?;
        let root = set.branch_root(id);
        let root_configuration = check_identity(&root, set.dataset_id, entry, &mut budget)?;
        let first_bound_own = root_configuration.and_then(|_| entry.from.seq.checked_add(1));
        let current = current_controlled(&root, id, &mut budget)?;
        let head = head_controlled(&root, id, &current.value, &mut budget)?;
        if let Purpose::TimeGone(ms) = purpose {
            return Err(gone_controlled(
                &root,
                id,
                &current.value,
                head,
                None,
                Some(At::Time(ms)),
                None,
                &mut budget,
            )?);
        }
        let wanted = match at {
            At::Commit(seq) => Some(*seq),
            At::Time(_) => None,
            _ => unreachable!("validated inherited selector"),
        };
        if let Some(seq) = wanted {
            if seq > head {
                return Err(Error::NotFound(format!("no commit {seq} (head is {head})")));
            }
            if seq < entry.from.seq {
                return Ok(OriginRead::Inherited(CommitRef {
                    branch_id: entry.from.branch_id,
                    seq,
                }));
            }
            if seq == entry.from.seq && matches!(purpose, Purpose::Configuration) {
                // The child's primary root wins even when all of its root RDF
                // is now served by a retained ancestor generation.
                return Ok(OriginRead::Configuration {
                    binding: root_configuration.unwrap_or_default(),
                    first_bound_own,
                });
            }
        }
        let mut selection =
            select_catalog(&root, id, at, wanted, current.value.base, head, &mut budget)?;
        reconcile_tail(
            &root,
            id,
            &current.value,
            at,
            wanted,
            head,
            &mut selection,
            &mut budget,
        )?;
        // the state at the starting commit is the upstream's, which serves it once
        // this branch no longer keeps its own copy or metadata
        let start = CommitRef {
            branch_id: entry.from.branch_id,
            seq: entry.from.seq,
        };
        let Some(mut metadata) = selection.selected else {
            if matches!(at, At::Time(_)) {
                return Ok(OriginRead::Inherited(entry.from.into()));
            }
            if wanted == Some(entry.from.seq) {
                return Ok(OriginRead::Inherited(start));
            }
            return Err(gone_controlled(
                &root,
                id,
                &current.value,
                head,
                wanted,
                None,
                None,
                &mut budget,
            )?);
        };
        if matches!(at, At::Time(_)) && metadata.seq < entry.from.seq {
            return Ok(OriginRead::Inherited(entry.from.into()));
        }
        if matches!(purpose, Purpose::Clock) {
            budget.check()?;
            return Ok(OriginRead::Clock(metadata.timestamp_ms));
        }
        let selected = generation_for(
            &root,
            id,
            current.value.number,
            head,
            metadata.seq,
            &mut budget,
        )?;
        let Some(selected) = selected else {
            if metadata.seq == entry.from.seq && wanted.is_some() {
                return Ok(OriginRead::Inherited(start));
            }
            return Err(gone_controlled(
                &root,
                id,
                &current.value,
                head,
                Some(metadata.seq),
                None,
                Some(metadata),
                &mut budget,
            )?);
        };
        if matches!(purpose, Purpose::Configuration) {
            if metadata.seq == selected.value.base.seq {
                return Ok(OriginRead::Configuration {
                    binding: selected.value.configuration,
                    first_bound_own,
                });
            }
            let vocabulary =
                cold::head_vocabulary_controlled(&selected.value.dir, &root, &mut budget)?;
            let terms = vocabulary.value.len_limited(&budget.owned_limit()?)?;
            let mut digest = None;
            let proof = prove_file_with(
                &root,
                id,
                &selected.value,
                metadata.seq,
                terms,
                &mut budget,
                &mut |observation| {
                    if observation.point.seq == metadata.seq {
                        digest = observation.configuration;
                    }
                    Ok(())
                },
            )?;
            if proof.selected.is_none() {
                return Err(super::super::managed_history::unavailable(
                    "retained configuration WAL state is unavailable",
                ));
            }
            let binding = match digest {
                None => crate::history::ConfigurationBinding::LegacyUnbound,
                Some(generation) => {
                    let state = self.managed_state_shared();
                    let lineage = match state.as_deref() {
                        Some(crate::managed::ManagedState::Installed(installed)) => {
                            installed.configuration_history.as_ref()
                        }
                        _ => None,
                    }
                    .ok_or_else(|| {
                        super::super::managed_history::unavailable(
                            "retained WAL binding has no configuration lineage",
                        )
                    })?;
                    crate::history::ConfigurationBinding::Known {
                        configuration: crate::history::ConfigurationIdentity {
                            dataset_id: self.owner_dataset_id(),
                            lineage_id: lineage.lineage_id,
                            generation,
                        },
                    }
                }
            };
            return Ok(OriginRead::Configuration {
                binding,
                first_bound_own,
            });
        }
        let geo = historical_geo_controlled(&root, &set.opts, &mut budget)?;
        let loaded = match cold::read_link_controlled(&selected.value.dir, &mut budget)? {
            Some(file) => Generation::open_linked_controlled(
                &selected.value.dir,
                &selected.value.name,
                &root,
                file,
                &self.cache,
                &mut budget,
            )?,
            None => Generation::open_sealed_controlled(
                &selected.value.dir,
                &selected.value.name,
                None,
                &mut budget,
            )?,
        };
        let generation = Arc::new(loaded.value);
        let terms = generation.dvocab.len_limited(&budget.owned_limit()?)?;
        let mut previous = wal::WalPoint {
            seq: selected.value.base.seq,
            offset: 0,
        };
        let mut parent_point = None;
        let proof = prove_file_with(
            &root,
            id,
            &selected.value,
            metadata.seq,
            terms,
            &mut budget,
            &mut |observation| {
                if observation.point.seq == metadata.seq {
                    parent_point = Some(previous);
                }
                previous = observation.point;
                Ok(())
            },
        )?;
        let target = proof.selected.ok_or_else(|| {
            Error::HistoryUnsupported("the selected cold retained WAL state is unavailable".into())
        })?;
        let seed = generation.link.as_ref().map(|link| &link.base_delta);
        let seed_bytes = seed.map_or(0, history_limited::preparation_delta_bytes);
        budget.charge(seed_bytes)?;
        let mut delta = seed.cloned().unwrap_or_default();
        budget.release(seed_bytes);
        let replay_to = if selection.recovered {
            parent_point.ok_or_else(|| {
                Error::Corrupt(
                    "selected recovered commit has no parent transaction boundary".into(),
                )
            })?
        } else {
            target
        };
        wal::apply_forward_limited(
            &selected.value.dir.join("wal.log"),
            &generation,
            &self.cache,
            &mut delta,
            wal::WalPoint {
                seq: selected.value.base.seq,
                offset: 0,
            },
            replay_to,
            &budget.owned_limit()?,
        )?;
        if selection.recovered {
            replay::selected_metadata(
                &generation,
                &self.cache,
                &mut delta,
                &selected.value.dir.join("wal.log"),
                replay_to,
                target,
                &mut metadata,
                &mut budget,
            )?;
        }
        budget.charge(history_limited::preparation_delta_bytes(&delta))?;
        budget.check()?;
        let settings = ChangeLog::settings_controlled(&root, &mut budget)?;
        let log = ChangeLog::capture_read_only(
            &root,
            id,
            metadata.seq,
            head,
            Store::log_limits(&set.opts),
            settings,
            &mut budget,
        )?;
        let snapshot = Arc::new(Snapshot {
            dataset_id: set.dataset_id,
            generation,
            delta,
            version: 0,
            cache: self.cache.clone(),
            results: self.results.clone(),
            cacheable: true,
            dvocab_len: terms,
            commit: metadata.seq,
            // Ordinary past snapshots deliberately have no live full-text index.
            text: None,
            geo,
            union_default_graph: set.opts.union_default_graph,
            geo_op_vertices: set.opts.geo_op_vertices,
            historical: true,
            counts: Default::default(),
            mask: None,
            change_log: Some(log.value),
        });
        budget.check()?;
        Ok(OriginRead::View(
            snapshot,
            crate::history::Resolved {
                at: at.clone(),
                commit: metadata,
                head,
                historical: metadata.seq != head,
            },
        ))
    }
}

/// The same physical coverage used by ordinary HistoryState construction. Catalog
/// generation numbers preserve original commit provenance across compaction.
fn generation_end(
    root: &Path,
    id: uuid::Uuid,
    current: u32,
    head: u64,
    generation: &Current,
    budget: &mut ColdBudget<'_>,
) -> Result<u64> {
    if generation.number == current {
        budget.check()?;
        return Ok(head);
    }
    let mut recorded = None;
    let catalog =
        commit::walk_catalog_controlled(&root.join("commits.bin"), budget, |commit, budget| {
            budget.check()?;
            if commit.seq <= head && commit.generation == generation.number {
                recorded = Some(commit.seq.max(generation.base.seq));
            }
            Ok(())
        })?;
    if catalog.is_some_and(|(found, _, _)| found != id) {
        return Err(Error::Corrupt(
            "retained catalog belongs to another branch".into(),
        ));
    }
    if let Some(end) = recorded {
        budget.check()?;
        return Ok(end);
    }
    let start = budget.used();
    let result = (|| {
        let vocab = cold::head_vocabulary_controlled(&generation.dir, root, budget)?;
        let terms = vocab.value.len_limited(&budget.owned_limit()?)?;
        let proof = prove_file(root, id, generation, u64::MAX, terms, budget)?;
        budget.check()?;
        Ok(proof.head.seq)
    })();
    budget.release(budget.used() - start);
    result
}

/// Match ordinary past-state provider semantics without mutable provider startup.
/// Invalid spatial configuration disables the index at ordinary Store open too.
fn historical_geo_controlled(
    root: &Path,
    options: &StoreOptions,
    budget: &mut ColdBudget<'_>,
) -> Result<Option<Arc<crate::geo::GeoView>>> {
    #[cfg(not(feature = "geo"))]
    {
        let _ = (root, options);
        budget.check()?;
        Ok(None)
    }
    #[cfg(feature = "geo")]
    {
        let start = budget.used();
        let result = (|| {
            budget.charge(root.as_os_str().len() as u64 * 2 + 4096)?;
            let bytes = match budget.read_file(&root.join(crate::geo::CONFIG_FILE)) {
                Ok(bytes) => bytes,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            let mut config: Loaded<crate::geo::GeoConfig> = match budget.json(&bytes) {
                Ok(config) => config,
                Err(Error::Corrupt(_)) => return Ok(None),
                Err(error) => return Err(error),
            };
            budget.drop_vec(bytes);
            // Validation can format an offending configured IRI. Its diagnostic is
            // covered by the retained parser charge, including owned strings.
            match config.value.validate_controlled(|| budget.check()) {
                Ok(()) => {}
                Err(Error::Invalid(_)) => return Ok(None),
                Err(error) => return Err(error),
            }
            config.value.query_rewrite &= options.geo_query_rewrite;
            let view = crate::store::geo::historical_geo_view(Arc::new(config.value), 1);
            budget.check()?;
            Ok(Some(view))
        })();
        match result {
            Ok(Some(view)) => {
                // Keep the conservative decoded-config charge beside replay and
                // the returned view. Empty defaults and provider headers fit in 4 KiB.
                budget.check()?;
                Ok(Some(view))
            }
            Ok(None) => {
                budget.release(budget.used() - start);
                budget.check()?;
                Ok(None)
            }
            Err(error) => {
                budget.release(budget.used() - start);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, Instant},
    };

    #[test]
    fn cold_origin_opening_wait_observes_cancellation_and_deadline() {
        for deadline in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let store = Arc::new(Store::open(root.path(), Default::default()).unwrap());
            let set = store.owned_set().unwrap();
            let held = set.opening.lock();
            let flag = Arc::new(AtomicBool::new(false));
            let read = store.clone();
            let cancel = flag.clone();
            let (send, receive) = std::sync::mpsc::channel();
            let (started, wait) = std::sync::mpsc::channel();
            let task = std::thread::spawn(move || {
                let options = crate::history::HistoryOptions {
                    cancel: Some(cancel),
                    deadline: deadline.then(|| Instant::now() + Duration::from_millis(50)),
                };
                let limit = crate::history::ReadLimit {
                    options: &options,
                    memory: Some(8 << 20),
                    held: 0,
                };
                started.send(()).unwrap();
                send.send(read.readonly_branch_snapshot_limited(
                    uuid::Uuid::new_v4(),
                    &At::Commit(0),
                    &limit,
                ))
                .unwrap();
            });
            wait.recv_timeout(Duration::from_secs(1)).unwrap();
            if !deadline {
                std::thread::sleep(Duration::from_millis(30));
                flag.store(true, Ordering::Relaxed);
            }
            let result = receive.recv_timeout(Duration::from_secs(1));
            drop(held);
            task.join().unwrap();
            if deadline {
                assert!(matches!(result, Ok(Err(Error::Timeout))));
            } else {
                assert!(matches!(result, Ok(Err(Error::Cancelled))));
            }
        }
    }
}
