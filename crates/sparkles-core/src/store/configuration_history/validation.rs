//! Read-only authentication before a copied directory becomes a dataset. Reuse
//! the ordinary cold framing proof rather than inventing a second WAL parser.
use super::*;
use crate::store::cold::ColdBudget;

pub(super) fn validate(root: &Path, required: bool, preparation: &Preparation) -> Result<()> {
    preparation.check()?;
    let published = match primary::recorded_root(root, preparation) {
        Ok(history) => history,
        Err(error) if required => return Err(error),
        // An unavailable genuinely legacy registry still permits RDF copying.
        // Any primary reference encountered below refuses the unreadable root.
        Err(error) => {
            preparation.check()?;
            if matches!(
                error,
                Error::Cancelled
                    | Error::Timeout
                    | Error::BudgetExceeded(_)
                    | Error::Unsupported(_)
            ) {
                return Err(error);
            }
            None
        }
    };
    if required && published.is_none() {
        return Err(managed_history::unavailable(
            "required copied configuration history is missing",
        ));
    }
    let allowance = preparation.room();
    let _held = preparation.reserve(allowance)?;
    let options = HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = cold::ColdBudget::new(ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    let family = commit::read_dataset_file_controlled(root, &mut budget)?;
    if family.is_none() && (required || published.is_some()) {
        return Err(managed_history::unavailable(
            "copied dataset identity is missing",
        ));
    }
    if let Some(history) = &published {
        let temporary = metadata_preparation(&budget)?;
        // Even a copy with no bound data commits must authenticate the complete
        // current publication and coverage before it can supply a new Head.
        verify_registry(
            root,
            family.expect("published root requires its real identity"),
            history,
            &temporary,
        )?;
    }
    let legacy = !required && published.is_none();
    inspect_branch(root, root, family, legacy, &mut budget)?;
    let branches = match std::fs::read_dir(root.join("branches")) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(branches) = branches {
        for entry in branches {
            budget.check()?;
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                return Err(managed_history::unavailable("copied branch is a symlink"));
            }
            if !entry.file_type()?.is_dir()
                || crate::formats::transient_branch_directory(&entry.file_name().to_string_lossy())
            {
                continue;
            }
            inspect_branch(root, &entry.path(), family, legacy, &mut budget)?;
        }
    }
    budget.check()
}

fn verify_registry(
    root: &Path,
    family: uuid::Uuid,
    history: &managed_history::Root,
    preparation: &Preparation,
) -> Result<()> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Registry {
        format: u32,
        revision: u64,
        owners: std::collections::BTreeMap<crate::managed::OwnerKind, crate::managed::OwnerEntry>,
        configuration_history: managed_history::Root,
    }
    let bytes = managed_history::read_bytes(&root.join(crate::managed::FILE), preparation)?;
    let _decoded = preparation.reserve(
        (bytes.value.len() as u64)
            .checked_mul(32)
            .and_then(|length| length.checked_add(4096))
            .ok_or_else(|| preparation.exhausted())?,
    )?;
    let registry: Registry = serde_json::from_slice(&bytes.value)
        .map_err(|_| managed_history::unavailable("invalid copied configuration registry"))?;
    let document =
        managed_history::resolve(root, family, history, history.identity(family), preparation)?;
    if registry.format != 3
        || registry.configuration_history != *history
        || registry.revision != document.value.registry_revision
        || registry.owners != document.value.owners
    {
        return Err(managed_history::unavailable(
            "copied registry and published configuration differ",
        ));
    }
    let control = crate::guard::WriteOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
        ..Default::default()
    };
    for (owner, bytes) in &document.value.owner_settings {
        preparation.check()?;
        let expected =
            crate::evidence::Sha256Digest::of_bytes(bytes.as_bytes(), preparation.control())?;
        managed_history::capture::pinned_file(
            &root.join(owner.settings_file()),
            expected,
            preparation,
            &control,
        )?;
    }
    preparation.check()
}

/// The caller's cold allocations remain charged during this synchronous loan.
/// No decoded generation, allocation or reservation escapes the check.
fn metadata_preparation(budget: &ColdBudget<'_>) -> Result<Preparation> {
    let limit = budget.owned_limit()?;
    Preparation::new(
        crate::task::Control {
            cancel: limit
                .options
                .cancel
                .clone()
                .map(crate::task::Cancel::from_flag)
                .unwrap_or_default(),
            deadline: limit.options.deadline,
            ..Default::default()
        },
        budget.available(),
    )
}

fn inspect_branch(
    family_root: &Path,
    root: &Path,
    family: Option<uuid::Uuid>,
    legacy: bool,
    budget: &mut ColdBudget<'_>,
) -> Result<()> {
    let start = budget.used();
    let result = (|| {
        budget.charge(
            (root.as_os_str().len() as u64)
                .saturating_mul(8)
                .saturating_add(8192),
        )?;
        let branch = commit::read_dataset_file_controlled(root, budget)?;
        if branch.is_none() && family.is_some() {
            return Err(managed_history::unavailable(
                "copied branch identity is missing",
            ));
        }
        let origin = if root != family_root {
            if family.is_none() {
                return Err(managed_history::unavailable(
                    "copied branch has no actual family identity",
                ));
            }
            let encoded = budget.read_file(&root.join(super::super::branching::BRANCH_FILE))?;
            let mut raw = budget.json::<serde_json::Value>(&encoded)?;
            crate::formats::check_settings(super::super::branching::BRANCH_FILE, &raw.value)?;
            if let Some(object) = raw.value.as_object_mut() {
                object.entry("format").or_insert(1.into());
            }
            let descriptor = budget.value::<super::super::branching::BranchFile>(raw.value)?;
            descriptor.check_configuration(&encoded)?;
            if family.is_some_and(|family| descriptor.dataset_id != family)
                || branch.is_some_and(|branch| descriptor.id != branch)
            {
                return Err(managed_history::unavailable(
                    "copied branch/family identity differs",
                ));
            }
            descriptor
                .configuration_origin
                .as_ref()
                .map(|origin| (descriptor.from.seq, origin.binding))
        } else {
            None
        };
        // A generation newer than CURRENT is a leftover that open removes or sets
        // aside, as `settle_newer_generations` does. It never becomes primary, so
        // its references authenticate nothing and a torn one is not an error.
        let current = match budget.read_file(&root.join("CURRENT")) {
            Ok(bytes) => std::str::from_utf8(&bytes)
                .ok()
                .map(|name| commit::generation_number(name.trim()))
                .filter(|number| *number != 0),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        for entry in std::fs::read_dir(root)? {
            budget.check()?;
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.strip_prefix("gen-").is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
            }) {
                continue;
            }
            if current.is_some_and(|current| commit::generation_number(name) > current) {
                continue;
            }
            if !entry.file_type()?.is_dir() || entry.file_type()?.is_symlink() {
                return Err(managed_history::unavailable(
                    "copied generation is not a directory",
                ));
            }
            inspect_generation(
                family_root,
                root,
                &entry.path(),
                family,
                branch,
                origin,
                legacy,
                budget,
            )?;
        }
        Ok(())
    })();
    budget.release(budget.used() - start);
    result
}

#[allow(clippy::too_many_arguments)]
fn inspect_generation(
    family_root: &Path,
    root: &Path,
    directory: &Path,
    family: Option<uuid::Uuid>,
    branch: Option<uuid::Uuid>,
    origin: Option<(u64, ConfigurationBinding)>,
    legacy: bool,
    budget: &mut ColdBudget<'_>,
) -> Result<()> {
    let start = budget.used();
    let result = (|| {
        let (base, binding, folding, strict, branch) =
            match commit::read_gen_commit_controlled(directory, budget)? {
                Some(identity) => {
                    if family.is_none() {
                        return Err(managed_history::unavailable(
                            "copied generation metadata has no actual family identity",
                        ));
                    }
                    if branch.is_some_and(|branch| identity.value.dataset_id != branch) {
                        return Err(managed_history::unavailable(
                            "copied generation identity differs",
                        ));
                    }
                    let result = (
                        identity.value.commit.seq,
                        identity.value.configuration,
                        identity.value.origin == "baseline",
                        identity.value.wal_strict_from,
                        Some(identity.value.dataset_id),
                    );
                    drop(identity.value);
                    budget.release(identity.charge);
                    result
                }
                // First-identity migration folds genuinely pre-metadata records into
                // baseline0. Still prove the WAL: deleting both small identity files
                // must not hide a surviving accepted configuration seal.
                None if legacy => (0, ConfigurationBinding::LegacyUnbound, true, None, branch),
                None => {
                    return Err(managed_history::unavailable(
                        "copied generation identity is missing",
                    ));
                }
            };
        let mut primary = match family {
            Some(family) => {
                let temporary = metadata_preparation(budget)?;
                let mut primary = primary::Header::on_disk(
                    family_root,
                    family,
                    branch.expect("identified generation needs its actual branch"),
                    origin,
                    &temporary,
                )?;
                primary.generation(family_root, &temporary, base, binding)?;
                Some(primary)
            }
            None => None,
        };
        let vocabulary = cold::head_vocabulary_controlled(directory, root, budget)?;
        let terms = vocabulary.value.len_limited(&budget.owned_limit()?)?;
        let annotations = match branch {
            None => cold::Loaded {
                value: None,
                charge: 0,
            },
            Some(branch) => crate::annotations::seqs_controlled(root, branch, budget)?,
        };
        let path = directory.join("wal.log");
        let loaded = match budget.buffered(&path) {
            Ok(loaded) => loaded,
            Err(Error::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound && strict.is_none() =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let mut reader = loaded.value;
        let extent = reader.get_ref().metadata()?.len();
        let mut configuration_error = None;
        wal::probe::prove_prefix_with(
            &mut reader,
            extent,
            base,
            u64::MAX,
            folding,
            strict,
            terms,
            annotations.value.as_deref(),
            &path,
            budget,
            &mut |observation| {
                if configuration_error.is_none() {
                    let Some(primary) = primary.as_mut() else {
                        if observation.configuration.is_some() {
                            configuration_error = Some(managed_history::unavailable(
                                "copied sealed reference has no actual family identity",
                            ));
                        }
                        return Ok(());
                    };
                    match primary.cached_observation(observation) {
                        Ok(true) => {}
                        Ok(false) => {
                            let temporary = metadata_preparation(budget)?;
                            if let Err(error) =
                                primary.observation(family_root, &temporary, observation)
                            {
                                configuration_error = Some(error);
                            }
                        }
                        Err(error) => configuration_error = Some(error),
                    }
                }
                Ok(())
            },
        )?;
        if let Some(error) = configuration_error {
            return Err(error);
        }
        Ok(())
    })();
    budget.release(budget.used() - start);
    result
}
