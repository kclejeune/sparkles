//! Store installation and persistence of an ordered pair of write guards.

use super::WriteGuard;
use crate::commit::CommitKind;
use crate::guard::config::{
    CONFIG_FILE, StatusFile, remove_files, sha256_hex, write_atomic, write_config,
};
use crate::guard::{
    Candidate, CommitGuard, GuardLanguage, GuardMode, GuardStatus, ValidationSummary,
};
use crate::store::{Snapshot, Store};
use crate::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

const FORMAT: u32 = 4;

/// One entry in an ordered write-time validation configuration.
#[derive(Clone, Debug, PartialEq)]
pub enum GuardConfig {
    Shacl(sparkles_shacl::guard::ValidationConfig),
    Shex(sparkles_shex::guard::ShexValidationConfig),
}

impl GuardConfig {
    fn language(&self) -> GuardLanguage {
        match self {
            Self::Shacl(_) => GuardLanguage::Shacl,
            Self::Shex(_) => GuardLanguage::Shex,
        }
    }

    fn prepare(
        self,
        snap: &Arc<Snapshot>,
        resolver: &dyn sparkles_shex::Resolver,
        establish: bool,
    ) -> Result<(WriteGuard, ValidationSummary)> {
        let invalid = crate::handles::from_anyhow;
        let mut result = match self {
            Self::Shacl(cfg) => {
                if cfg.mode == GuardMode::Off {
                    return Err(Error::invalid("omit disabled guards from the guards list"));
                }
                if cfg
                    .shapes
                    .inline
                    .as_ref()
                    .is_some_and(|text| text.len() > sparkles_shex::guard::MAX_SCHEMA_BYTES)
                {
                    return Err(Error::invalid("inline SHACL shapes exceed 16 MiB"));
                }
                let (guard, mut summary) =
                    sparkles_shacl::guard::prepare_detached(cfg, snap, establish)
                        .map_err(invalid)?;
                if guard.rejects_initial(&summary) {
                    summary.status = GuardStatus::Rejected;
                }
                (WriteGuard::Shacl(guard), summary)
            }
            Self::Shex(cfg) => {
                if cfg.mode == GuardMode::Off {
                    return Err(Error::invalid("omit disabled guards from the guards list"));
                }
                let (guard, mut summary) =
                    sparkles_shex::guard::prepare_detached(cfg, snap, resolver, establish)
                        .map_err(invalid)?;
                if guard.rejects_initial(&summary) {
                    summary.status = GuardStatus::Rejected;
                }
                (WriteGuard::Shex(guard), summary)
            }
        };
        result.1.language = result.0.language();
        Ok(result)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    format: u32,
    guards: Vec<Value>,
}

/// Read the format 4 configuration accepted by the dataset and server.
pub fn parse_configs(input: Value) -> Result<Vec<GuardConfig>> {
    let cfg: ConfigFile = serde_json::from_value(input).map_err(invalid)?;
    if cfg.format != FORMAT {
        return Err(Error::invalid(format!(
            "guards requires validation.json format {FORMAT}"
        )));
    }
    if !(1..=2).contains(&cfg.guards.len()) {
        return Err(Error::invalid("give one or two validation guards"));
    }
    cfg.guards
        .into_iter()
        .enumerate()
        .map(|(i, entry)| {
            let guard = match entry.get("language").and_then(Value::as_str) {
                Some("shacl") => serde_json::from_value(entry).map(GuardConfig::Shacl),
                Some("shex") => serde_json::from_value(entry).map(GuardConfig::Shex),
                _ => {
                    return Err(Error::invalid(format!(
                        "guards[{i}].language must be shacl or shex"
                    )));
                }
            };
            guard.map_err(|e| Error::invalid(format!("guards[{i}]: {e}")))
        })
        .collect()
}

/// Publish an unchanged member as the only guard without throwing away its baseline.
pub(crate) fn retain(
    dataset: &crate::Dataset,
    config: &GuardConfig,
) -> Result<Option<crate::handles::GuardOutcome>> {
    let store = dataset.store();
    let txn = store.write_as(CommitKind::Transaction);
    let Some(WriteGuard::Combined(combined)) = dataset.write_guard() else {
        return Ok(None);
    };
    let Some(guard) = combined
        .guards
        .iter()
        .find(|g| g.language() == config.language())
    else {
        return Ok(None);
    };
    let current = parse_configs(json!({ "format": FORMAT, "guards": [guard.json()["config"]] }))?;
    if current.first() != Some(config) {
        return Ok(None);
    }
    let publish = |guard: WriteGuard| {
        guard.attach(store);
        dataset.set_write_guard(Some(guard));
    };
    if let Some(root) = store.root() {
        let files = guard.memory_files();
        let Some((_, cfg)) = files.first() else {
            return Err(Error::invalid("could not serialize the remaining guard"));
        };
        let hash = sha256_hex(cfg);
        let head = txn.view().commit;
        match guard {
            WriteGuard::Shacl(g) => {
                let cfg = serde_json::from_slice(cfg).map_err(invalid)?;
                persist_retained(root, &files, publish, || {
                    let (guard, status) = g.retain_persistent(cfg, root, hash, head);
                    (WriteGuard::Shacl(guard), status)
                })?;
            }
            WriteGuard::Shex(g) => {
                persist_retained(root, &files, publish, || {
                    let (guard, status) = g.retain_persistent(root, hash, head);
                    (WriteGuard::Shex(guard), status)
                })?;
            }
            WriteGuard::Combined(_) => unreachable!(),
        }
    } else {
        publish(guard.clone());
    }
    Ok(Some(crate::handles::GuardOutcome::Retained))
}

fn persist_retained(
    root: &std::path::Path,
    files: &[(String, Vec<u8>)],
    publish: impl FnOnce(WriteGuard),
    promote: impl FnOnce() -> (WriteGuard, Option<StatusFile>),
) -> Result<()> {
    let [(config, cfg), sources @ ..] = files else {
        unreachable!();
    };
    for (file, data) in sources {
        write_atomic(&root.join(file), data)?;
    }
    write_config(root, cfg, || {
        let (guard, status) = promote();
        publish(guard);
        if let Err(e) = StatusFile::remove(root) {
            tracing::warn!("cannot remove obsolete validation status: {e}");
        }
        if let Some(status) = status
            && let Err(e) = status.write(root)
        {
            tracing::warn!("cannot write validation status: {e}");
        }
    })?;
    let mut keep = vec![config.as_str()];
    keep.extend(sources.iter().map(|(file, _)| file.as_str()));
    if let Err(e) = remove_files(root, &keep) {
        tracing::warn!("cannot remove obsolete validation files: {e}");
    }
    Ok(())
}

fn invalid(e: impl std::fmt::Display) -> Error {
    Error::invalid(format!("invalid validation configuration: {e}"))
}

/// A store guard that checks SHACL and ShEx in configuration order.
pub struct CombinedGuard {
    guards: [WriteGuard; 2],
}

impl CombinedGuard {
    pub(super) fn guards(&self) -> &[WriteGuard; 2] {
        &self.guards
    }

    pub(super) fn config(&self) -> Value {
        let guards: Vec<_> = self
            .guards
            .iter()
            .map(|guard| {
                let mut json = guard.json();
                json["config"].take()
            })
            .collect();
        json!({ "format": FORMAT, "guards": guards })
    }

    pub(super) fn fork(&self, snap: &Snapshot) -> Result<Arc<Self>> {
        let [first, second] = &self.guards;
        Ok(Arc::new(Self {
            guards: [first.fork_detached(snap)?, second.fork_detached(snap)?],
        }))
    }
}

impl CommitGuard for CombinedGuard {
    fn check(&self, candidate: &Candidate<'_>) -> Result<ValidationSummary> {
        let checked = self
            .guards
            .iter()
            .map(|guard| {
                candidate.opts.check()?;
                guard.commit_guard().check(candidate)
            })
            .collect::<Result<Vec<_>>>()
            .map(ValidationSummary::combined);
        if candidate.opts.dry_run.is_none()
            && !matches!(&checked, Ok(s) if s.status != GuardStatus::Rejected)
        {
            let seq = candidate.base.commit + 1;
            if let Some(guard) = self.guards.iter().find_map(WriteGuard::shacl) {
                guard.discard_pending(seq);
            }
            if let Some(guard) = self.guards.iter().find_map(WriteGuard::shex) {
                guard.discard_pending(seq);
            }
        }
        checked
    }

    fn committed(&self, seq: u64) {
        for guard in &self.guards {
            guard.commit_guard().committed(seq);
        }
    }

    fn bypassed(&self, seq: u64) {
        for guard in &self.guards {
            guard.commit_guard().bypassed(seq);
        }
    }

    fn bypass_summary(&self) -> ValidationSummary {
        ValidationSummary::combined(
            self.guards
                .iter()
                .map(|guard| guard.commit_guard().bypass_summary())
                .collect(),
        )
    }

    fn speculative(&self) -> bool {
        self.guards
            .iter()
            .all(|guard| guard.commit_guard().speculative())
    }

    fn close(&self, head: &crate::commit::CommitInfo, snapshot: &Snapshot) {
        for guard in &self.guards {
            guard.commit_guard().close(head, snapshot);
        }
    }

    fn rekey(&self, old: &Snapshot, new: &Snapshot) {
        for guard in &self.guards {
            guard.commit_guard().rekey(old, new);
        }
    }

    fn describe(&self) -> String {
        "write-time SHACL and ShEx validation".to_string()
    }

    fn language(&self) -> GuardLanguage {
        GuardLanguage::Combined
    }
}

fn prepare(
    configs: Vec<GuardConfig>,
    snap: &Arc<Snapshot>,
    resolver: &dyn sparkles_shex::Resolver,
    establish: bool,
) -> Result<(WriteGuard, ValidationSummary)> {
    let [first, second]: [GuardConfig; 2] = configs
        .try_into()
        .map_err(|_| Error::invalid("combined validation requires one SHACL and one ShEx guard"))?;
    if first.language() == second.language() {
        return Err(Error::invalid(
            "combined validation requires one SHACL and one ShEx guard",
        ));
    }
    let (first, first_summary) = first.prepare(snap, resolver, establish)?;
    let (second, second_summary) = second.prepare(snap, resolver, establish)?;
    let guard = CombinedGuard {
        guards: [first, second],
    };
    let summary = ValidationSummary::combined(vec![first_summary, second_summary]);
    Ok((WriteGuard::Combined(Arc::new(guard)), summary))
}

pub(super) fn install(store: &Store) -> Result<WriteGuard> {
    let root = store.root().expect("persistent validation installation");
    let txn = store.write_as(CommitKind::Transaction);
    let bytes = std::fs::read(root.join(CONFIG_FILE))?;
    let configs = parse_configs(serde_json::from_slice(&bytes).map_err(invalid)?)?;
    let (guard, _) = prepare(
        configs,
        &Arc::new(txn.view()),
        &sparkles_shex::NoImports,
        false,
    )?;
    guard.attach(store);
    Ok(guard)
}

// Validate everything before replacing the configuration. Inlining the two source
// copies keeps the publication to a single atomic file replacement.
pub(crate) fn set(
    dataset: &crate::Dataset,
    configs: Vec<GuardConfig>,
    resolver: &dyn sparkles_shex::Resolver,
) -> Result<crate::handles::GuardOutcome> {
    use crate::handles::GuardOutcome;
    let store = dataset.store();
    let txn = store.write_as(CommitKind::Transaction);
    let (guard, summary) = prepare(configs, &Arc::new(txn.view()), resolver, true)?;
    if summary.status == GuardStatus::Rejected {
        return Ok(GuardOutcome::NotConforming(summary));
    }
    let publish = || {
        guard.attach(store);
        dataset.set_write_guard(Some(guard.clone()));
    };
    if let Some(root) = store.root() {
        let WriteGuard::Combined(combined) = &guard else {
            unreachable!();
        };
        let bytes = serde_json::to_vec_pretty(&combined.config()).map_err(invalid)?;
        write_config(root, &bytes, publish)?;
        if let Err(e) = remove_files(root, &[CONFIG_FILE]).and_then(|()| StatusFile::remove(root)) {
            tracing::warn!("cannot remove obsolete validation files: {e}");
        }
    } else {
        publish();
    }
    Ok(GuardOutcome::Installed(summary))
}
