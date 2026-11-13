//! Durable jobs (spec C26 Phase 1): a job ledger in a protected control graph of the
//! dataset, fenced worker leases and atomic publication.
//!
//! A run is submitted with an idempotency key, claimed by a worker that receives a
//! bearer lease token, renewed, and published or failed under that token. Every
//! transition is one ordinary commit of the dataset's main store under the jobs owner's
//! managed-write capability, so a ledger record is durable exactly when its commit is,
//! and a publication's business quads, publication record, checkpoint and completion
//! state commit together or not at all (C26 §7.1).
//!
//! ```no_run
//! use sparkles::Dataset;
//! use sparkles::jobs::{Caller, Definition, Settings, Submission};
//!
//! let ds = Dataset::open("/data/example")?;
//! let mut settings = Settings { enabled: true, ..Settings::default() };
//! settings.definitions.insert("collect".into(), Definition::quads(vec!["urn:g".into()]));
//! ds.jobs().configure(&settings)?;
//! let caller = Caller::admin(oxrdf::NamedNode::new_unchecked("urn:x-sparkles:principal:ops"));
//! let submitted = ds.jobs().submit(
//!     &Submission::new("collect", serde_json::json!({"page": 1}), "advisory-2026-001"),
//!     &caller,
//! )?;
//! println!("run {}", submitted.run);
//! # Ok::<_, sparkles::Error>(())
//! ```
//!
//! The ledger graph is write-protected and hidden: it leaves every caller's read view,
//! and only the typed inspection of this module renders it. It stores no secrets: a
//! lease token is never written, only its SHA-256 verifier, and failure diagnostics are
//! bounded and redacted by the worker before they are recorded.

mod ledger;
mod ops;

pub use ledger::{AttemptView, PublicationView, RunView, State};
pub use ops::{
    Claimed, Failure, ListFilter, Publication, Published, Recovered, Renewed, Submission, Submitted,
};
pub use sparkles_core::jobs::{
    Definition, KIND_QUADS, LEDGER_GRAPH, MAX_IDEMPOTENCY_KEY_BYTES, NAMESPACE, PROFILE, READER,
    Settings, canonical_json, operation_id,
};

use crate::managed::{ManagedError, ManagedErrorKind, OwnerEntry, OwnerKind};
use crate::{Dataset, Error, Result};
use oxrdf::NamedNode;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// The process-local state of a dataset's jobs: the boot epoch chosen when the
/// dataset was opened, whether this process has recorded it, and the monotonic expiry
/// of each lease this process granted (C26 §4.2).
pub struct Runtime {
    epoch: uuid::Uuid,
    installed: AtomicBool,
    installing: parking_lot::Mutex<()>,
    leases: parking_lot::Mutex<HashMap<uuid::Uuid, Vec<Lease>>>,
    grants: std::sync::atomic::AtomicU64,
    /// Run once by the next renewal or failure after its commit returns and before its
    /// outcome reaches the runtime, so a test can order other operations there.
    #[cfg(test)]
    pub(crate) after_commit: parking_lot::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// One recorded deadline: the version of the run whose commit grants or extends the
/// lease, and the grant that recorded it (spec C26 §7.1). A check honours it only once
/// that version is the committed one.
#[derive(Clone, Copy, Debug)]
struct Lease {
    grant: u64,
    version: u64,
    deadline: Instant,
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime {
            epoch: uuid::Uuid::new_v4(),
            installed: AtomicBool::new(false),
            installing: parking_lot::Mutex::new(()),
            leases: parking_lot::Mutex::new(HashMap::new()),
            grants: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            after_commit: parking_lot::Mutex::new(None),
        }
    }
}

/// A deadline recorded before its commit, under the version the commit writes. Kept
/// once the commit published, which drops the deadlines of older versions. Dropped
/// without [`keep`](Grant::keep), as after a refused, unwinding or uncertain commit, it
/// removes its own entry and nothing else: the deadline of the committed version was
/// never replaced, so there is nothing to put back (spec C26 §7.1).
#[must_use]
pub(crate) struct Grant<'r> {
    rt: &'r Runtime,
    run: uuid::Uuid,
    id: u64,
    version: u64,
    kept: bool,
}

impl Grant<'_> {
    /// The commit published: the deadline stays, and those of older versions go.
    fn keep(mut self) {
        self.kept = true;
        if let Some(v) = self.rt.leases.lock().get_mut(&self.run) {
            v.retain(|l| l.version >= self.version);
        }
    }
}

impl Drop for Grant<'_> {
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        let mut leases = self.rt.leases.lock();
        if let Some(v) = leases.get_mut(&self.run) {
            v.retain(|l| l.grant != self.id);
            if v.is_empty() {
                leases.remove(&self.run);
            }
        }
    }
}

impl Runtime {
    /// The boot epoch of this process for the dataset.
    pub fn epoch(&self) -> uuid::Uuid {
        self.epoch
    }

    fn installed(&self) -> bool {
        self.installed.load(Ordering::Acquire)
    }

    fn mark_installed(&self) {
        self.installed.store(true, Ordering::Release);
    }

    /// Whether the lease of `run` at its committed version `version` has expired on the
    /// monotonic clock. Only the deadline recorded under that version counts, so one
    /// whose commit did not publish is never honoured (spec C26 §7.1). A lease this
    /// process never granted is expired: its attempt belongs to another epoch.
    fn lease_expired(&self, run: uuid::Uuid, version: u64) -> bool {
        self.leases.lock().get(&run).is_none_or(|v| {
            v.iter()
                .find(|l| l.version == version)
                .is_none_or(|l| l.deadline <= Instant::now())
        })
    }

    /// Record a deadline `ms` from now for `run` under `version`, the version the commit
    /// that grants or extends the lease writes, before that commit publishes. A deadline
    /// recorded earlier under the same version belongs to a commit that did not publish,
    /// since only one commit writes each version, and is replaced. The returned guard
    /// removes the entry unless it is kept.
    fn grant(&self, run: uuid::Uuid, version: u64, ms: u64) -> Grant<'_> {
        let id = self
            .grants
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let lease = Lease {
            grant: id,
            version,
            deadline: Instant::now() + std::time::Duration::from_millis(ms),
        };
        let mut leases = self.leases.lock();
        let v = leases.entry(run).or_default();
        v.retain(|l| l.version != version);
        v.push(lease);
        Grant {
            rt: self,
            run,
            id,
            version,
            kept: false,
        }
    }

    /// The test hook of [`after_commit`](Self::after_commit), if one is set.
    #[cfg(test)]
    fn after_commit(&self) {
        // taken before it runs, so that a test can set the next hook meanwhile
        let hook = self.after_commit.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Forget the deadlines of `run` recorded under `version` or earlier, after the
    /// commit that wrote `version` ended its lease. A deadline of a later version
    /// belongs to a claim that committed after this transition and stays (spec C26
    /// §7.1).
    fn revoke_through(&self, run: uuid::Uuid, version: u64) {
        let mut leases = self.leases.lock();
        if let Some(v) = leases.get_mut(&run) {
            v.retain(|l| l.version > version);
            if v.is_empty() {
                leases.remove(&run);
            }
        }
    }
}

/// Who performs a job operation: the authenticated principal, whether it administers
/// the dataset, and the graphs it may write when its grants do not cover every graph.
#[derive(Clone, Debug)]
pub struct Caller {
    pub principal: NamedNode,
    /// An administrator sees every run and may cancel any run. Others see the runs they
    /// submitted or currently work on.
    pub admin: bool,
    /// The caller's write view, checked on every business quad of a publication.
    pub write: Option<Arc<crate::access::GraphAccess>>,
}

impl Caller {
    /// A dataset administrator without graph restrictions.
    pub fn admin(principal: NamedNode) -> Caller {
        Caller {
            principal,
            admin: true,
            write: None,
        }
    }

    /// A principal without administration, seeing its own runs.
    pub fn user(principal: NamedNode) -> Caller {
        Caller {
            principal,
            admin: false,
            write: None,
        }
    }
}

/// The dataset's durable jobs (from [`Dataset::jobs`]).
#[derive(Clone)]
pub struct Jobs {
    pub(crate) ds: Dataset,
}

impl Dataset {
    /// The dataset's durable jobs (spec C26). Jobs are managed on the dataset's main
    /// store, and every operation of a branch's handle is refused.
    pub fn jobs(&self) -> Jobs {
        Jobs { ds: self.clone() }
    }
}

/// A dataset's jobs settings and the protection they have now.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsStatus {
    /// The recorded settings, or the disabled defaults when jobs were never configured.
    pub settings: Settings,
    /// The SHA-256 digest of the recorded `jobs.json`, `None` when there is none.
    pub digest: Option<String>,
    /// The revision of the dataset's managed-graph registry, 0 when it has none.
    pub revision: u64,
    /// Whether the ledger graph is protected.
    pub protected: bool,
    /// The revision of each enabled definition.
    pub revisions: std::collections::BTreeMap<String, String>,
}

pub(crate) fn managed(kind: ManagedErrorKind, message: impl Into<String>) -> Error {
    ManagedError::error(kind, message)
}

fn unavailable(reason: String) -> Error {
    managed(ManagedErrorKind::Unavailable, reason)
}

/// The recorded settings `bytes`, which must be readable by this build.
pub(crate) fn recorded(bytes: &[u8]) -> Result<Settings> {
    Settings::from_slice(bytes).map_err(|e| {
        unavailable(format!(
            "{} cannot be read by this build: {e}",
            OwnerKind::Jobs.settings_file()
        ))
    })
}

fn status_from(
    graphs: Option<&crate::managed::ManagedGraphs>,
    bytes: Option<&[u8]>,
) -> Result<SettingsStatus> {
    let revision = graphs.map_or(0, |g| g.revision);
    let Some(bytes) = bytes else {
        return Ok(SettingsStatus {
            settings: Settings::default(),
            digest: None,
            revision,
            protected: false,
            revisions: Default::default(),
        });
    };
    let settings = recorded(bytes)?;
    let protected = graphs
        .and_then(|g| g.owners.get(&OwnerKind::Jobs))
        .is_some_and(|e| e.graphs.iter().any(|g| g == LEDGER_GRAPH));
    let revisions = settings
        .definitions
        .iter()
        .map(|(n, d)| (n.clone(), d.revision()))
        .collect();
    Ok(SettingsStatus {
        settings,
        digest: Some(crate::managed::settings_digest(bytes)),
        revision,
        protected,
        revisions,
    })
}

impl Jobs {
    fn main(&self) -> Result<&Dataset> {
        if self.ds.store().is_branch() {
            return Err(Error::invalid(
                "jobs are managed on the dataset's main store, not on a branch",
            ));
        }
        Ok(&self.ds)
    }

    /// The jobs settings of the dataset. Fails with `managed-unavailable` when the
    /// managed-graph registry cannot be enforced.
    pub fn settings(&self) -> Result<SettingsStatus> {
        let snapshot = self.main()?.store().managed_snapshot()?;
        if let Some(reason) = snapshot.as_ref().and_then(|s| s.unavailable.as_ref()) {
            return Err(unavailable(reason.clone()));
        }
        let bytes = snapshot
            .as_ref()
            .and_then(|s| s.settings.get(&OwnerKind::Jobs).cloned());
        status_from(snapshot.as_ref().map(|s| &s.graphs), bytes.as_deref())
    }

    /// Record `settings` as the dataset's jobs settings and protect the ledger graph.
    /// The first configuration raises the dataset reader to 9 durably, before
    /// `jobs.json` and then `managed.json` are written. Disabling the settings while
    /// runs are not terminal is refused, and a disabled configuration of a dataset that
    /// was never configured writes nothing unless it differs from the defaults, which
    /// is refused. A configuration that races with another change of the registry is
    /// refused with a conflict.
    ///
    /// Once configured, the ledger graph stays protected and hidden under its nonce
    /// while jobs are disabled, so a submission that commits between the check of active
    /// runs and the disabling registry change leaves a queued run that no ordinary write
    /// can touch, that no claim takes while jobs are disabled, and that is dispatched
    /// again once they are enabled.
    ///
    /// Editing a definition changes its revision. An accepted run keeps the revision it
    /// was accepted under, and a claim of a run whose definition revision is no longer
    /// registered is refused, so the run waits until the definition is restored or the
    /// run is cancelled.
    pub fn configure(&self, settings: &Settings) -> Result<SettingsStatus> {
        let ds = self.main()?;
        let store = ds.store();
        settings.validate()?;
        let canonical = settings.canonical();
        let snapshot = store.managed_snapshot()?;
        let revision = snapshot.as_ref().map_or(0, |s| s.graphs.revision);
        let current = snapshot
            .as_ref()
            .and_then(|s| s.graphs.owners.get(&OwnerKind::Jobs));
        if current.is_none() && !settings.enabled {
            if canonical != Settings::default().canonical() {
                return Err(Error::invalid(
                    "jobs are not enabled; enable them before storing disabled settings",
                ));
            }
            if let Some(reason) = snapshot.as_ref().and_then(|s| s.unavailable.as_ref()) {
                return Err(unavailable(reason.clone()));
            }
            return status_from(snapshot.as_ref().map(|s| &s.graphs), None);
        }
        if current.is_some() && !settings.enabled {
            if self.has_active_runs()? {
                return Err(Error::Conflict(
                    "jobs cannot be disabled while runs are queued, leased or waiting to retry; cancel them first".into(),
                ));
            }
            store.failpoint("jobs-before-disable");
        }
        if settings.enabled && store.root().is_some() {
            // The reader rises before the first ledger byte can be written, and the
            // settings file and the registry follow in `enable_managed_at`.
            store.require_dataset_reader(READER)?;
        }
        let graphs = settings.managed_graphs();
        // The write guard leaves the ledger out before it can hold a record, and it
        // follows the graphs the store hides once the registry is written, or not.
        let mut hiding = store.hidden_graphs();
        hiding.extend(graphs.iter().cloned());
        ds.rehide_write_guard(hiding);
        let mut entry = OwnerEntry::for_settings(PROFILE, graphs, &canonical);
        // the ledger leaves every caller's read view (C26 §2.3)
        entry.visibility = crate::managed::Visibility::Hidden;
        let enabled = store.enable_managed_at(OwnerKind::Jobs, entry, &canonical, revision);
        ds.rehide_write_guard(store.hidden_graphs());
        let graphs = enabled?;
        // the hidden graphs changed without a commit of the data: the views' worker
        // resolves their access again and ends the live queries
        self.ds.state().views.kick();
        status_from(Some(&graphs), Some(&canonical))
    }

    /// The recorded settings, enabled, or the refusal of an operation on a dataset
    /// without jobs.
    pub(crate) fn enabled_settings(&self) -> Result<(Settings, Arc<[u8]>)> {
        let bytes = self
            .main()?
            .store()
            .managed_settings(OwnerKind::Jobs)?
            .ok_or_else(|| {
                managed(
                    ManagedErrorKind::UnsupportedProfile,
                    "jobs are not enabled in this dataset; record jobs settings first",
                )
            })?;
        let settings = recorded(&bytes)?;
        if !settings.enabled {
            return Err(managed(
                ManagedErrorKind::UnsupportedProfile,
                "jobs are disabled in this dataset",
            ));
        }
        Ok((settings, bytes))
    }

    pub(crate) fn runtime(&self) -> &Runtime {
        &self.ds.state().jobs
    }
}

#[cfg(test)]
mod tests;
