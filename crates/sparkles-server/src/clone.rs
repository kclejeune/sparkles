//! Clone a dataset into a new, independent database (a sandbox): one consistent
//! snapshot of the source with the same blank-node ids, plus `origin.json` and, with
//! the inferences, a rebased `reasoning.json`.
//!
//! A persistent clone is built in a temporary sibling directory and renamed into
//! place, so a failure leaves no half-created destination. An in-memory clone is a new
//! store that is registered once it is complete.

use crate::state::{ReasoningInfo, sync_dir, write_file_atomic, write_reasoning_file};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value as J;
use sparkles::access::{GraphRule, Graphs};
use sparkles::commit::ForkedFrom;
use sparkles::store::{CloneMode, CloneOptions, CloneReport, ProgressFn, Store, StoreOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What happens to the materialized inferences.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Inferences {
    /// copy the inferred graph and the reasoning status
    #[default]
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

/// What a clone copies, and how.
#[derive(Clone, Debug, Default)]
pub struct Spec {
    pub inferences: Inferences,
    /// the graphs to copy: `default` (or `urn:x-arq:DefaultGraph`), graph IRIs and IRI
    /// patterns with `*`, as in a graph grant; `None`: every graph
    pub graphs: Option<Vec<String>>,
    /// whether the source's index files may be shared
    pub mode: CloneMode,
    /// clone this past state instead of the head
    pub at: Option<sparkles::history::At>,
}

impl Spec {
    /// The library options of this clone, labelled `label` in the source's history.
    fn options(
        &self,
        label: &str,
        progress: Option<ProgressFn>,
        cancel: Option<Arc<AtomicBool>>,
    ) -> CloneOptions {
        CloneOptions {
            exclude_graphs: match self.inferences {
                Inferences::Copy => Vec::new(),
                Inferences::Drop => {
                    vec![oxrdf::NamedNode::new_unchecked(crate::http::INFERRED_GRAPH)]
                }
            },
            graphs: self.rule().map(Graphs::Only),
            cancel,
            progress,
            at: self.at.clone(),
            mode: self.mode,
            label: label.to_string(),
        }
    }

    /// The graph selection; patterns never match the inferred graph, which is copied
    /// only when it is named.
    fn rule(&self) -> Option<GraphRule> {
        self.graphs
            .as_ref()
            .map(|g| GraphRule::new(g, &[crate::http::INFERRED_GRAPH]))
    }

    /// Whether the clone holds the inferred graph, and so a reasoning status.
    fn keeps_inferences(&self) -> bool {
        self.inferences == Inferences::Copy
            && self
                .rule()
                .is_none_or(|r| Graphs::Only(r).allows_iri(crate::http::INFERRED_GRAPH))
    }
}

/// Whether `g` names graphs of a clone selection: `default` or
/// `urn:x-arq:DefaultGraph`, an absolute IRI, or an IRI pattern with `*`.
pub fn valid_graph_name(g: &str) -> bool {
    if matches!(g, "default" | sparkles::sparql::ctx::DEFAULT_GRAPH_IRI) || g == "*" {
        return true;
    }
    g != sparkles::sparql::ctx::UNION_GRAPH_IRI
        && oxrdf::NamedNode::new(g.replace('*', "x")).is_ok()
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
    /// the graphs selected, for a partial clone
    #[serde(default, skip_serializing_if = "Option::is_none")]
    graphs: Option<Vec<String>>,
    /// how the copy was made: `link`, `reflink`, `copy` or `rebuild`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    method: Option<String>,
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

/// The origin record of a clone of `store` (dataset `name`).
fn origin(store: &Store, name: &str, spec: &Spec, report: &CloneReport) -> OriginFile {
    OriginFile {
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
        inferences: spec.inferences.name().to_string(),
        graphs: spec.graphs.clone(),
        method: Some(report.method.name().to_string()),
    }
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

/// Clone `store` (dataset `name`) as `spec` says into `dst`, building in `tmp` (which
/// must not exist) and renaming it into place. `dst` must not exist, or be an empty
/// directory. `reasoning` is the source's reasoning status, read before this call: a
/// reasoning run that commits in between then reads as stale in the clone, never
/// falsely fresh. Setting `cancel` stops the clone before the rename with
/// `sparkles::Error::Cancelled`, leaving nothing behind.
#[allow(clippy::too_many_arguments)]
pub fn clone_into(
    store: &Store,
    name: &str,
    reasoning: Option<ReasoningInfo>,
    tmp: &Path,
    dst: &Path,
    spec: &Spec,
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
    let label = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let opts = spec.options(&label, progress, cancel.clone());
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
    write_file_atomic(
        &tmp.join("origin.json"),
        &serde_json::to_vec_pretty(&origin(store, name, spec, &report))?,
    )?;
    if let Some(info) = reasoning.filter(|_| spec.keeps_inferences()) {
        write_reasoning_file(tmp, Some(&rebased(info, store, &report, spec)))?;
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

/// An in-memory clone, ready to be registered.
pub struct MemoryClone {
    pub store: Store,
    pub report: CloneReport,
    /// the rebased reasoning status, when the clone holds the inferences
    pub reasoning: Option<ReasoningInfo>,
    /// what `origin.json` holds for a persistent clone
    pub origin: J,
}

/// Clone `store` (dataset `name`) as `spec` says into a new in-memory store with
/// options `store_opts` (see [`Store::clone_to_memory`]). `reasoning` is the source's
/// reasoning status, as for [`clone_into`].
#[allow(clippy::too_many_arguments)]
pub fn clone_into_memory(
    store: &Store,
    name: &str,
    target: &str,
    reasoning: Option<ReasoningInfo>,
    spec: &Spec,
    store_opts: StoreOptions,
    progress: Option<ProgressFn>,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<MemoryClone> {
    let opts = spec.options(target, progress, cancel);
    let (clone, report) = store.clone_to_memory(&opts, store_opts)?;
    let reasoning = reasoning
        .filter(|_| spec.keeps_inferences())
        .map(|info| rebased(info, store, &report, spec));
    let origin = serde_json::to_value(origin(store, name, spec, &report))?;
    Ok(MemoryClone {
        store: clone,
        report,
        reasoning,
        origin,
    })
}

/// Remove what `sparkles clone` runs that did not finish left next to `dst`: the
/// `{dst}.clone-tmp-{pid}` directories of processes that are gone. They are never
/// databases anyone opened, so removing them is always safe.
pub fn sweep_cli_leftovers(dst: &Path) {
    let (Some(parent), Some(name)) = (dst.parent(), dst.file_name()) else {
        return;
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let prefix = format!("{}.clone-tmp-", name.to_string_lossy());
    let Ok(rd) = std::fs::read_dir(parent) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = n.strip_prefix(&prefix).and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if pid != std::process::id() && !process_alive(pid) {
            tracing::info!("removing unfinished clone {}", e.path().display());
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Whether process `pid` exists (on Unix; elsewhere it is assumed to).
fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return true;
        };
        // SAFETY: signal 0 only checks that the process exists
        let r = unsafe { libc::kill(pid, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
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
/// unknown. Inferences copied without all of the graphs they were drawn from are
/// stale.
fn rebased(
    info: ReasoningInfo,
    source: &Store,
    report: &CloneReport,
    spec: &Spec,
) -> ReasoningInfo {
    let f = crate::reasoning::freshness(&info, source, report.forked_from.seq);
    let partial = spec.graphs.is_some();
    let fresh = f.stale == Some(false) && !partial;
    ReasoningInfo {
        commit: fresh.then_some(0),
        position_source: fresh.then(|| "commit".to_string()),
        dataset_id: fresh.then(|| report.dataset_id.to_string()),
        inherited_stale: f.stale == Some(true) || partial,
        ..info
    }
}
