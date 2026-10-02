//! Clone a dataset into a new, independent persistent database (a sandbox): one
//! consistent snapshot of the source, rebuilt with the same blank-node ids, plus
//! `origin.json` and, with the inferences, a rebased `reasoning.json`.
//!
//! The clone is built in a temporary sibling directory and renamed into place, so a
//! failure leaves no half-created destination.

use crate::state::{ReasoningInfo, sync_dir, write_file_atomic, write_reasoning_file};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value as J;
use sparkles::commit::ForkedFrom;
use sparkles::store::{CloneOptions, CloneReport, ProgressFn, Store};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What happens to the materialized inferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inferences {
    /// copy the inferred graph and the reasoning status
    Copy,
    /// leave both out
    Drop,
}

impl Inferences {
    pub fn parse(s: &str) -> Option<Inferences> {
        match s {
            "copy" => Some(Inferences::Copy),
            "drop" => Some(Inferences::Drop),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Inferences::Copy => "copy",
            Inferences::Drop => "drop",
        }
    }
}

/// `origin.json` of a clone (informational).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OriginFile {
    origin_format: u32,
    cloned_at: String,
    source: OriginSource,
    forked_from: ForkedFrom,
    inferences: String,
}

#[derive(Serialize, Deserialize)]
struct OriginSource {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    version: u64,
    generation: String,
    quads: u64,
}

/// `origin.json` of a database directory, as JSON, if it is a clone.
pub fn read_origin(root: &Path) -> Option<J> {
    serde_json::from_slice(&std::fs::read(root.join("origin.json")).ok()?).ok()
}

/// Removes a directory on drop unless disarmed.
struct RemoveDir(Option<PathBuf>);

impl Drop for RemoveDir {
    fn drop(&mut self) {
        if let Some(d) = self.0.take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// Test hook: a clone into a directory with this name fails after the build, before
/// the rename.
#[cfg(test)]
pub const FAIL_BEFORE_RENAME: &str = "fail-before-rename";

/// Clone `store` (dataset `name`) into `dst`, building in `tmp` (which must not exist)
/// and renaming it into place. `dst` must not exist, or be an empty directory.
/// `reasoning` is the source's reasoning status, read before this call: a reasoning run
/// that commits in between then reads as stale in the clone, never falsely fresh.
/// Setting `cancel` stops the clone before the rename with `sparkles::Error::Cancelled`,
/// leaving nothing behind.
#[allow(clippy::too_many_arguments)]
pub fn clone_into(
    store: &Store,
    name: &str,
    reasoning: Option<ReasoningInfo>,
    tmp: &Path,
    dst: &Path,
    inferences: Inferences,
    progress: Option<ProgressFn>,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<CloneReport> {
    if tmp.exists() {
        bail!("{} already exists", tmp.display());
    }
    let cancelled = || cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
    if cancelled() {
        return Err(sparkles::Error::Cancelled.into());
    }
    let opts = CloneOptions {
        exclude_graphs: match inferences {
            Inferences::Copy => Vec::new(),
            Inferences::Drop => vec![oxrdf::NamedNode::new_unchecked(crate::http::INFERRED_GRAPH)],
        },
        cancel: cancel.clone(),
        progress,
    };
    let report = store.clone_to(tmp, &opts)?;
    let mut guard = RemoveDir(Some(tmp.to_path_buf()));
    if cancelled() {
        return Err(sparkles::Error::Cancelled.into());
    }
    // the clone gets the default storage quota: a copy larger than that is refused
    if let Some(limit) = store.options().max_disk_bytes {
        let size = dir_bytes(tmp);
        if size > limit {
            return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::DatasetBytes,
                limit,
                requested: size,
            })
            .into());
        }
    }
    let origin = OriginFile {
        origin_format: 1,
        cloned_at: crate::state::now(),
        source: OriginSource {
            name: name.to_string(),
            path: store
                .root()
                .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
                .map(|p| p.display().to_string()),
            version: report.version,
            generation: report.generation.clone(),
            quads: report.source_quads,
        },
        forked_from: report.forked_from,
        inferences: inferences.name().to_string(),
    };
    write_file_atomic(
        &tmp.join("origin.json"),
        &serde_json::to_vec_pretty(&origin)?,
    )?;
    if let (Inferences::Copy, Some(info)) = (inferences, reasoning) {
        write_reasoning_file(tmp, Some(&rebased(info, store, &report)))?;
    }
    sync_dir(tmp)?;
    #[cfg(test)]
    if dst.file_name().is_some_and(|n| n == FAIL_BEFORE_RENAME) {
        bail!("injected failure before the rename");
    }
    if dst.exists() {
        // an empty directory (checked by the caller) makes way for the clone
        std::fs::remove_dir(dst)
            .with_context(|| format!("{} exists and is not empty", dst.display()))?;
    }
    std::fs::rename(tmp, dst)
        .with_context(|| format!("renaming {} to {}", tmp.display(), dst.display()))?;
    guard.0 = None;
    sync_dir(
        dst.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    Ok(report)
}

/// Bytes of the files under `dir`.
fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_bytes(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// The source's reasoning status for the clone: fresh at the copied snapshot stays
/// fresh (at the clone's root commit), stale is inherited as stale, unknown stays
/// unknown.
fn rebased(info: ReasoningInfo, source: &Store, report: &CloneReport) -> ReasoningInfo {
    let f = crate::reasoning::freshness(&info, source, report.forked_from.seq);
    let fresh = f.stale == Some(false);
    ReasoningInfo {
        commit: fresh.then_some(0),
        position_source: fresh.then(|| "commit".to_string()),
        dataset_id: fresh.then(|| report.dataset_id.to_string()),
        inherited_stale: f.stale == Some(true),
        ..info
    }
}
