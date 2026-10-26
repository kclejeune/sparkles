//! Read-only origin descriptors captured under the family's opening/table guards.
//! Physical files describe lineage; they confer neither write nor receipt authority.
use super::*;
use crate::store::cold::{ColdBudget, Loaded};

/// Validate immutable identity against the guarded table. A committed rename may
/// leave the descriptive name behind; the table's name remains authoritative.
fn check_identity(
    root: &Path,
    family: uuid::Uuid,
    entry: &Entry,
    budget: &mut ColdBudget<'_>,
) -> Result<()> {
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
        budget.check()
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
    folding: bool,
    strict_from: Option<u64>,
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
            folding: physical.value.origin == "baseline",
            strict_from: physical.value.wal_strict_from,
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
    dir: &Path,
    id: uuid::Uuid,
    base: CommitInfo,
    folding: bool,
    strict_from: Option<u64>,
    target: u64,
    terms: u64,
    budget: &mut ColdBudget<'_>,
) -> Result<wal::probe::ProvenWal> {
    prove_file_with(
        root,
        dir,
        id,
        base,
        folding,
        strict_from,
        target,
        terms,
        budget,
        &mut |_| Ok(()),
    )
}

fn prove_file_with(
    root: &Path,
    dir: &Path,
    id: uuid::Uuid,
    base: CommitInfo,
    folding: bool,
    strict_from: Option<u64>,
    target: u64,
    terms: u64,
    budget: &mut ColdBudget<'_>,
    visit: &mut dyn FnMut(wal::probe::WalCommitObservation) -> Result<()>,
) -> Result<wal::probe::ProvenWal> {
    let start = budget.used();
    budget.charge(
        (root.as_os_str().len() as u64 + dir.as_os_str().len() as u64)
            .saturating_mul(4)
            .saturating_add(1024),
    )?;
    let result = (|| {
        let annotations = crate::annotations::seqs_controlled(root, id, budget)?;
        let path = dir.join("wal.log");
        let loaded = match budget.buffered(&path) {
            Ok(loaded) => loaded,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                if strict_from.is_some() {
                    return Err(Error::Corrupt(
                        "retained generation lost its recorded durable WAL".into(),
                    ));
                }
                budget.check()?;
                let point = wal::WalPoint {
                    seq: base.seq,
                    offset: 0,
                    folding,
                };
                return Ok(wal::probe::ProvenWal {
                    head: point,
                    selected: (target == base.seq).then_some(point),
                });
            }
            Err(error) => return Err(error),
        };
        let mut reader = loaded.value;
        let extent = reader.get_ref().metadata()?.len();
        wal::probe::prove_prefix_with(
            &mut reader,
            extent,
            base.seq,
            target,
            folding,
            strict_from,
            terms,
            annotations.value.as_deref(),
            &path,
            budget,
            visit,
        )
    })();
    // These are temporary proof buffers and membership, never retained on a view.
    budget.release(budget.used() - start);
    result
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
        let proof = prove_file(
            root,
            &current.dir,
            id,
            current.base,
            current.folding,
            current.strict_from,
            u64::MAX,
            terms,
            budget,
        )?;
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
                folding: identity.value.origin == "baseline",
                strict_from: identity.value.wal_strict_from,
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
    Clock,
    TimeGone(i64),
}

pub(in crate::store) enum OriginRead {
    Clock(i64),
    Resident(Arc<Store>),
    View(Arc<Snapshot>, crate::history::Resolved),
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
        if let At::Snapshot(name) = at
            && !crate::history::valid_name(name)
        {
            return Err(Error::invalid("invalid snapshot name"));
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
        check_identity(&root, set.dataset_id, entry, &mut budget)?;
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
            At::Head => Some(head),
            At::Commit(seq) => Some(*seq),
            At::Snapshot(name) => Some(
                crate::history::pin_controlled(&root, id, name, &mut budget)?
                    .ok_or_else(|| Error::NotFound(format!("no snapshot '{name}'")))?,
            ),
            At::Time(_) => None,
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
        let Some(mut metadata) = selection.selected else {
            if matches!(at, At::Time(_)) {
                return Ok(OriginRead::Inherited(entry.from.into()));
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
        // Configured auxiliary live providers need their own admitted preparation;
        // returning a data view with silently changed provider semantics is unsafe.
        for name in ["geo.json", "text.json"] {
            budget.check()?;
            if root.join(name).try_exists()? {
                return Err(Error::HistoryUnsupported(
                    "a cold origin's auxiliary provider preparation is incomplete".into(),
                ));
            }
        }
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
            folding: selected.value.folding,
        };
        let mut parent_point = None;
        let proof = prove_file_with(
            &root,
            &selected.value.dir,
            id,
            selected.value.base,
            selected.value.folding,
            selected.value.strict_from,
            metadata.seq,
            terms,
            &mut budget,
            &mut |observation| {
                if observation.point.seq == metadata.seq && !observation.point.folding {
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
                folding: selected.value.folding,
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
            text: None,
            geo: None,
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
        let proof = prove_file(
            root,
            &generation.dir,
            id,
            generation.base,
            generation.folding,
            generation.strict_from,
            u64::MAX,
            terms,
            budget,
        )?;
        budget.check()?;
        Ok(proof.head.seq)
    })();
    budget.release(budget.used() - start);
    result
}
