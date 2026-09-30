//! Backup metrics (Prometheus at `/$/metrics`). Label sets are closed: `repository` is
//! capped like `dataset` (`--metrics-max-datasets`, overflow `$other`).
//!
//! | Name | Type | Labels |
//! |---|---|---|
//! | `sparkles_backup_operations_total` | counter | `repository`, `operation`, `result` |
//! | `sparkles_backup_operation_duration_seconds` | histogram | `operation` |
//! | `sparkles_backup_bytes_uploaded_total`, `…_downloaded_total` | counter | `repository` |
//! | `sparkles_backup_blobs_uploaded_total`, `…_blobs_reused_total` | counter | `repository` |
//! | `sparkles_backup_object_requests_total` | counter | `repository`, `op`, `result` |
//! | `sparkles_backup_last_success_timestamp_seconds` | gauge | `dataset`, `repository` |
//! | `sparkles_backup_capture_lock_seconds` | histogram | |
//! | `sparkles_backup_repository_stored_bytes`, `…_logical_bytes`, `…_backups` | gauge | `repository` |
//! | `sparkles_backup_lock_conflicts_total` | counter | `repository` |
//! | `sparkles_backup_policy_runs_total` | counter | `policy`, `result` |
//! | `sparkles_backup_policy_last_success_timestamp_seconds`, `…_next_run_timestamp_seconds`, `…_consecutive_failures` | gauge | `policy` |
//!
//! Object requests are counted from the request totals that verification and GC
//! reports carry. The policy series are fed by the scheduler
//! ([`BackupMetrics::policy_run`], [`BackupMetrics::set_policy`]).

use crate::state::AppState;
use parking_lot::Mutex;
use sparkles_backup::{BackupError, Code};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::time::Duration;

/// Overflow label of repositories beyond the cap.
const OTHER: &str = "$other";

/// Operations (`operation` label).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Operation {
    Create,
    Restore,
    Verify,
    Delete,
    Gc,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Create => "create",
            Operation::Restore => "restore",
            Operation::Verify => "verify",
            Operation::Delete => "delete",
            Operation::Gc => "gc",
        }
    }
}

/// Upper bounds (seconds) of the operation duration histogram.
const OP_BUCKETS: [f64; 10] = [
    1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0, 1800.0, 3600.0, 7200.0,
];
/// Upper bounds (seconds) of the capture lock histogram.
const LOCK_BUCKETS: [f64; 10] = [
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 1.0,
];

/// A histogram with fixed bounds (non-cumulative counts; the last is `+Inf`).
#[derive(Clone, Debug)]
struct Hist {
    bounds: &'static [f64],
    counts: Vec<u64>,
    sum: f64,
}

impl Hist {
    fn new(bounds: &'static [f64]) -> Hist {
        Hist {
            bounds,
            counts: vec![0; bounds.len() + 1],
            sum: 0.0,
        }
    }

    fn observe(&mut self, d: Duration) {
        let s = d.as_secs_f64();
        let i = self
            .bounds
            .iter()
            .position(|b| s <= *b)
            .unwrap_or(self.bounds.len());
        self.counts[i] += 1;
        self.sum += s;
    }

    fn render(&self, o: &mut String, name: &str, labels: &str) {
        let sep = if labels.is_empty() { "" } else { "," };
        let mut acc = 0;
        for (i, c) in self.counts.iter().enumerate() {
            acc += c;
            let le = match self.bounds.get(i) {
                Some(b) => format!("{b}"),
                None => "+Inf".into(),
            };
            let _ = writeln!(o, "{name}_bucket{{{labels}{sep}le=\"{le}\"}} {acc}");
        }
        let braces = if labels.is_empty() {
            String::new()
        } else {
            format!("{{{labels}}}")
        };
        let _ = writeln!(o, "{name}_sum{braces} {}", self.sum);
        let _ = writeln!(o, "{name}_count{braces} {acc}");
    }
}

/// Scheduler state of a policy, for the policy gauges.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolicyGauges {
    /// Unix seconds of the last successful run
    pub last_success: Option<f64>,
    /// Unix seconds of the next scheduled run
    pub next_run: Option<f64>,
    pub consecutive_failures: u32,
}

struct Inner {
    /// repositories with a label of their own
    named: BTreeSet<String>,
    ops: BTreeMap<(String, Operation, &'static str), u64>,
    durations: BTreeMap<Operation, Hist>,
    uploaded: BTreeMap<String, u64>,
    downloaded: BTreeMap<String, u64>,
    blobs_uploaded: BTreeMap<String, u64>,
    blobs_reused: BTreeMap<String, u64>,
    requests: BTreeMap<(String, &'static str, &'static str), u64>,
    last_success: BTreeMap<(String, String), f64>,
    capture_lock: Hist,
    lock_conflicts: BTreeMap<String, u64>,
    policy_runs: BTreeMap<(String, String), u64>,
    policies: BTreeMap<String, PolicyGauges>,
}

/// The backup counters of a server (`BackupState::metrics`).
pub struct BackupMetrics {
    inner: Mutex<Inner>,
}

impl Default for BackupMetrics {
    fn default() -> BackupMetrics {
        BackupMetrics {
            inner: Mutex::new(Inner {
                named: BTreeSet::new(),
                ops: BTreeMap::new(),
                durations: BTreeMap::new(),
                uploaded: BTreeMap::new(),
                downloaded: BTreeMap::new(),
                blobs_uploaded: BTreeMap::new(),
                blobs_reused: BTreeMap::new(),
                requests: BTreeMap::new(),
                last_success: BTreeMap::new(),
                capture_lock: Hist::new(&LOCK_BUCKETS),
                lock_conflicts: BTreeMap::new(),
                policy_runs: BTreeMap::new(),
                policies: BTreeMap::new(),
            }),
        }
    }
}

impl Inner {
    /// The `repository` label of `repo`: its own while fewer than `cap` have one.
    fn label(&mut self, repo: &str, cap: usize) -> String {
        if self.named.contains(repo) {
            return repo.to_string();
        }
        if self.named.len() < cap {
            self.named.insert(repo.to_string());
            repo.to_string()
        } else {
            OTHER.to_string()
        }
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// What one finished operation adds to the counters.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// bytes uploaded (a create's `addedBytes`)
    pub uploaded: u64,
    /// bytes downloaded (a restore's logical size)
    pub downloaded: u64,
    pub blobs_uploaded: u64,
    pub blobs_reused: u64,
    /// object requests by `op` (`list`, `head`, `get`, `put`, `delete`), all `ok`
    pub requests: Vec<(&'static str, u64)>,
    /// a create: the dataset label of `last_success`
    pub dataset: Option<String>,
}

impl BackupMetrics {
    /// Count one operation on repository `repo` (`cap`: `--metrics-max-datasets`).
    pub fn operation(
        &self,
        cap: usize,
        repo: &str,
        op: Operation,
        result: Result<&Outcome, &BackupError>,
        elapsed: Duration,
    ) {
        let mut m = self.inner.lock();
        let label = m.label(repo, cap);
        let res = match result {
            Ok(_) => "ok",
            Err(e) if e.is_cancelled() => "cancelled",
            Err(_) => "failed",
        };
        *m.ops.entry((label.clone(), op, res)).or_default() += 1;
        m.durations
            .entry(op)
            .or_insert_with(|| Hist::new(&OP_BUCKETS))
            .observe(elapsed);
        match result {
            Ok(o) => {
                *m.uploaded.entry(label.clone()).or_default() += o.uploaded;
                *m.downloaded.entry(label.clone()).or_default() += o.downloaded;
                *m.blobs_uploaded.entry(label.clone()).or_default() += o.blobs_uploaded;
                *m.blobs_reused.entry(label.clone()).or_default() += o.blobs_reused;
                for (k, n) in &o.requests {
                    *m.requests.entry((label.clone(), k, "ok")).or_default() += n;
                }
                if op == Operation::Create
                    && let Some(ds) = &o.dataset
                {
                    m.last_success.insert((ds.clone(), label), now_secs());
                }
            }
            Err(e) if e.code() == Code::RepositoryLocked => {
                *m.lock_conflicts.entry(label).or_default() += 1;
            }
            Err(_) => {}
        }
    }

    /// The writer-lock hold time of a capture.
    pub fn capture_lock(&self, d: Duration) {
        self.inner.lock().capture_lock.observe(d);
    }

    /// Count a finished policy run (`result`: `ok`, `partial`, `failed`, `skipped`).
    #[cfg_attr(not(test), allow(dead_code))] // the policy scheduler
    pub fn policy_run(&self, policy: &str, result: &str) {
        *self
            .inner
            .lock()
            .policy_runs
            .entry((policy.to_string(), result.to_string()))
            .or_default() += 1;
    }

    /// Set the scheduler gauges of a policy (`None`: forget a removed policy).
    #[cfg_attr(not(test), allow(dead_code))] // the policy scheduler
    pub fn set_policy(&self, policy: &str, g: Option<PolicyGauges>) {
        let mut m = self.inner.lock();
        match g {
            Some(g) => {
                m.policies.insert(policy.to_string(), g);
            }
            None => {
                m.policies.remove(policy);
            }
        }
    }
}

fn esc(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn family(o: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(o, "# HELP {name} {help}");
    let _ = writeln!(o, "# TYPE {name} {kind}");
}

fn per_repo(o: &mut String, name: &str, kind: &str, help: &str, m: &BTreeMap<String, u64>) {
    family(o, name, kind, help);
    for (r, n) in m {
        let _ = writeln!(o, "{name}{{repository=\"{}\"}} {n}", esc(r));
    }
}

/// Append the backup metric families to a Prometheus exposition (called by
/// `obs::render_prometheus`). Writes nothing without `AppState::backup`.
pub fn render(st: &AppState, out: &mut String) {
    let Some(b) = &st.backup else { return };
    let cap = st.metrics.max_datasets();
    // repository gauges from the registry's last listing or GC
    let mut stored = BTreeMap::new();
    let mut logical = BTreeMap::new();
    let mut backups = BTreeMap::new();
    let repos: Vec<(String, Option<sparkles_backup::RepoStats>)> = b
        .registry
        .repos
        .read()
        .iter()
        .map(|(n, e)| (n.clone(), e.stats.clone()))
        .collect();
    let mut m = b.metrics.inner.lock();
    for (name, stats) in repos {
        let Some(s) = stats else { continue };
        let label = m.label(&name, cap);
        *stored.entry(label.clone()).or_default() += s.stored_bytes;
        *logical.entry(label.clone()).or_default() += s.logical_bytes;
        *backups.entry(label).or_default() += s.backups;
    }
    let o = out;
    family(
        o,
        "sparkles_backup_operations_total",
        "counter",
        "Backup operations by repository, operation and result.",
    );
    for ((r, op, res), n) in &m.ops {
        let _ = writeln!(
            o,
            "sparkles_backup_operations_total{{repository=\"{}\",operation=\"{}\",result=\"{res}\"}} {n}",
            esc(r),
            op.as_str()
        );
    }
    family(
        o,
        "sparkles_backup_operation_duration_seconds",
        "histogram",
        "Duration of backup operations in seconds.",
    );
    for (op, h) in &m.durations {
        h.render(
            o,
            "sparkles_backup_operation_duration_seconds",
            &format!("operation=\"{}\"", op.as_str()),
        );
    }
    per_repo(
        o,
        "sparkles_backup_bytes_uploaded_total",
        "counter",
        "Bytes uploaded to backup repositories.",
        &m.uploaded,
    );
    per_repo(
        o,
        "sparkles_backup_bytes_downloaded_total",
        "counter",
        "Bytes downloaded from backup repositories by restores.",
        &m.downloaded,
    );
    per_repo(
        o,
        "sparkles_backup_blobs_uploaded_total",
        "counter",
        "Blobs uploaded to backup repositories.",
        &m.blobs_uploaded,
    );
    per_repo(
        o,
        "sparkles_backup_blobs_reused_total",
        "counter",
        "Blobs a backup found in its repository already.",
        &m.blobs_reused,
    );
    family(
        o,
        "sparkles_backup_object_requests_total",
        "counter",
        "Object requests to backup repositories by operation and result.",
    );
    for ((r, op, res), n) in &m.requests {
        let _ = writeln!(
            o,
            "sparkles_backup_object_requests_total{{repository=\"{}\",op=\"{op}\",result=\"{res}\"}} {n}",
            esc(r)
        );
    }
    family(
        o,
        "sparkles_backup_last_success_timestamp_seconds",
        "gauge",
        "Time of the last successful backup of a dataset into a repository (Unix seconds).",
    );
    for ((ds, r), t) in &m.last_success {
        let _ = writeln!(
            o,
            "sparkles_backup_last_success_timestamp_seconds{{dataset=\"{}\",repository=\"{}\"}} {t:.3}",
            esc(&st.metrics.dataset_label(Some(ds))),
            esc(r)
        );
    }
    family(
        o,
        "sparkles_backup_capture_lock_seconds",
        "histogram",
        "How long backup captures held the writer lock, in seconds.",
    );
    m.capture_lock
        .render(o, "sparkles_backup_capture_lock_seconds", "");
    per_repo(
        o,
        "sparkles_backup_repository_stored_bytes",
        "gauge",
        "Stored bytes of a repository (from its last listing or GC).",
        &stored,
    );
    per_repo(
        o,
        "sparkles_backup_repository_logical_bytes",
        "gauge",
        "Logical bytes of the backups in a repository (from its last listing or GC).",
        &logical,
    );
    per_repo(
        o,
        "sparkles_backup_repository_backups",
        "gauge",
        "Backups in a repository (from its last listing or GC).",
        &backups,
    );
    per_repo(
        o,
        "sparkles_backup_lock_conflicts_total",
        "counter",
        "Operations that gave up waiting for a repository lock.",
        &m.lock_conflicts,
    );
    family(
        o,
        "sparkles_backup_policy_runs_total",
        "counter",
        "Backup policy runs by result.",
    );
    for ((p, res), n) in &m.policy_runs {
        let _ = writeln!(
            o,
            "sparkles_backup_policy_runs_total{{policy=\"{}\",result=\"{}\"}} {n}",
            esc(p),
            esc(res)
        );
    }
    let gauge =
        |o: &mut String, name: &str, help: &str, f: &dyn Fn(&PolicyGauges) -> Option<f64>| {
            family(o, name, "gauge", help);
            for (p, g) in &m.policies {
                if let Some(v) = f(g) {
                    let _ = writeln!(o, "{name}{{policy=\"{}\"}} {v}", esc(p));
                }
            }
        };
    gauge(
        o,
        "sparkles_backup_policy_last_success_timestamp_seconds",
        "Time of the last successful run of a policy (Unix seconds).",
        &|g| g.last_success,
    );
    gauge(
        o,
        "sparkles_backup_policy_next_run_timestamp_seconds",
        "Time of the next scheduled run of a policy (Unix seconds).",
        &|g| g.next_run,
    );
    gauge(
        o,
        "sparkles_backup_policy_consecutive_failures",
        "Failed runs of a policy since its last success.",
        &|g| Some(f64::from(g.consecutive_failures)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_caps_repositories() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = AppState::standalone(Default::default(), Duration::from_secs(1));
        st.backup = Some(std::sync::Arc::new(
            crate::backup::BackupState::new(dir.path(), None, 2).unwrap(),
        ));
        let m = &st.backup.as_ref().unwrap().metrics;
        let ok = Outcome {
            uploaded: 100,
            blobs_uploaded: 3,
            blobs_reused: 2,
            dataset: Some("ds".into()),
            requests: vec![("list", 2)],
            ..Default::default()
        };
        m.operation(
            1,
            "local",
            Operation::Create,
            Ok(&ok),
            Duration::from_millis(1500),
        );
        m.operation(
            1,
            "local",
            Operation::Create,
            Ok(&ok),
            Duration::from_millis(10),
        );
        let locked = BackupError::new(Code::RepositoryLocked, "locked");
        m.operation(
            1,
            "other",
            Operation::Create,
            Err(&locked),
            Duration::from_secs(1),
        );
        m.operation(
            1,
            "other",
            Operation::Restore,
            Err(&BackupError::cancelled()),
            Duration::from_secs(1),
        );
        m.capture_lock(Duration::from_micros(700));
        m.policy_run("nightly", "ok");
        m.set_policy(
            "nightly",
            Some(PolicyGauges {
                last_success: Some(5.0),
                next_run: None,
                consecutive_failures: 0,
            }),
        );
        let mut o = String::new();
        render(&st, &mut o);
        for want in [
            "sparkles_backup_operations_total{repository=\"local\",operation=\"create\",result=\"ok\"} 2",
            "sparkles_backup_operations_total{repository=\"$other\",operation=\"create\",result=\"failed\"} 1",
            "sparkles_backup_operations_total{repository=\"$other\",operation=\"restore\",result=\"cancelled\"} 1",
            "sparkles_backup_bytes_uploaded_total{repository=\"local\"} 200",
            "sparkles_backup_blobs_reused_total{repository=\"local\"} 4",
            "sparkles_backup_object_requests_total{repository=\"local\",op=\"list\",result=\"ok\"} 4",
            "sparkles_backup_lock_conflicts_total{repository=\"$other\"} 1",
            "sparkles_backup_capture_lock_seconds_count 1",
            "sparkles_backup_capture_lock_seconds_bucket{le=\"0.001\"} 1",
            "sparkles_backup_operation_duration_seconds_bucket{operation=\"create\",le=\"1\"} 2",
            "sparkles_backup_operation_duration_seconds_count{operation=\"create\"} 3",
            "sparkles_backup_policy_runs_total{policy=\"nightly\",result=\"ok\"} 1",
            "sparkles_backup_policy_last_success_timestamp_seconds{policy=\"nightly\"} 5",
            "sparkles_backup_policy_consecutive_failures{policy=\"nightly\"} 0",
        ] {
            assert!(o.contains(want), "{want} missing in\n{o}");
        }
        assert!(
            o.contains("sparkles_backup_last_success_timestamp_seconds{dataset=\"ds\",repository=\"local\"}"),
            "{o}"
        );
        assert!(!o.contains("policy_next_run_timestamp_seconds{"), "{o}");
    }
}
