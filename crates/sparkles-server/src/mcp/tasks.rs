//! The tasks extension of MCP (SEP-2663, `io.modelcontextprotocol/tasks`): a tool call
//! of a client that declares the extension and runs longer than
//! [`McpConfig::task_after`](super::McpConfig::task_after) becomes a task. The client
//! then polls `tasks/get` for its state and result, and may stop it with
//! `tasks/cancel`, which sets the call's cancel flag as `notifications/cancelled` would.
//!
//! A task belongs to the caller that started it: another caller's `tasks/get` or
//! `tasks/cancel` gets the answer for an unknown task. Completed tasks retain their
//! [`RetainedCallResult`] directly under its originating query memory credit for [`TTL`]
//! after they end (swept both opportunistically and by an expiry timer), and at most
//! [`MAX_RUNNING`] run at once; beyond that a call simply runs to its end, as for a
//! client without the extension.

use super::retained::RetainedCallResult;
use parking_lot::Mutex;
use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, DetailedTask, JsonObject, Task, TaskPayload, TaskStatus};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::task::{AbortHandle, JoinHandle};
use tokio_util::sync::CancellationToken;

/// Tasks running at once, over all callers.
pub const MAX_RUNNING: usize = 64;

/// How long a task is kept: a running one is stopped after this, and an ended one is
/// dropped this long after it ended.
pub const TTL: Duration = Duration::from_secs(600);

/// The polling interval suggested to clients.
const POLL_MS: u64 = 500;

#[cfg(test)]
fn duration_to_nanos(dur: Duration) -> u64 {
    u64::try_from(dur.as_nanos()).unwrap_or(u64::MAX)
}

/// Deterministic clock for tests that need to control task TTL expiry without racing
/// real wall-clock execution on loaded runners.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct ManualClock {
    now_ns: Arc<watch::Sender<u64>>,
}

#[cfg(test)]
impl ManualClock {
    pub(crate) fn new() -> Self {
        let (tx, _) = watch::channel(0u64);
        Self {
            now_ns: Arc::new(tx),
        }
    }

    pub(crate) fn advance(&self, delta: Duration) {
        let delta_ns = duration_to_nanos(delta);
        self.now_ns
            .send_modify(|now| *now = now.saturating_add(delta_ns));
    }

    fn now_ns(&self) -> u64 {
        *self.now_ns.borrow()
    }

    async fn sleep_until_ns(&self, target_ns: u64) {
        let mut rx = self.now_ns.subscribe();
        while *rx.borrow_and_update() < target_ns {
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
struct TaskInstant(Instant);

#[cfg(test)]
#[derive(Clone, Copy)]
enum TaskInstant {
    System(Instant),
    Manual(u64),
}

impl TaskInstant {
    fn saturating_duration_since(self, earlier: Self) -> Duration {
        #[cfg(not(test))]
        {
            self.0.saturating_duration_since(earlier.0)
        }
        #[cfg(test)]
        match (self, earlier) {
            (Self::System(a), Self::System(b)) => a.saturating_duration_since(b),
            (Self::Manual(a), Self::Manual(b)) => Duration::from_nanos(a.saturating_sub(b)),
            _ => unreachable!("mixed task clock instants"),
        }
    }
}

#[cfg(not(test))]
#[derive(Clone)]
struct TaskClock;

#[cfg(test)]
#[derive(Clone)]
enum TaskClock {
    System,
    Manual(ManualClock),
}

impl TaskClock {
    fn system() -> Self {
        #[cfg(not(test))]
        {
            Self
        }
        #[cfg(test)]
        {
            Self::System
        }
    }

    fn now(&self) -> TaskInstant {
        #[cfg(not(test))]
        {
            TaskInstant(Instant::now())
        }
        #[cfg(test)]
        match self {
            Self::System => TaskInstant::System(Instant::now()),
            Self::Manual(clock) => TaskInstant::Manual(clock.now_ns()),
        }
    }

    async fn sleep_until(&self, start: TaskInstant, dur: Duration) {
        #[cfg(not(test))]
        {
            let at = start.0;
            while let Some(remaining) = dur.checked_sub(at.elapsed()) {
                if remaining.is_zero() {
                    break;
                }
                tokio::time::sleep(remaining).await;
            }
        }
        #[cfg(test)]
        match (self, start) {
            (Self::System, TaskInstant::System(at)) => {
                while let Some(remaining) = dur.checked_sub(at.elapsed()) {
                    if remaining.is_zero() {
                        break;
                    }
                    tokio::time::sleep(remaining).await;
                }
            }
            (Self::Manual(clock), TaskInstant::Manual(at)) => {
                clock
                    .sleep_until_ns(at.saturating_add(duration_to_nanos(dur)))
                    .await;
            }
            _ => unreachable!("mixed task clock instants"),
        }
    }
}

fn current_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn unknown(id: &str) -> McpError {
    McpError::invalid_params(format!("unknown task: {id}"), None)
}

fn error_to_object(error: &McpError) -> JsonObject {
    match serde_json::to_value(error) {
        Ok(Value::Object(map)) => map,
        _ => JsonObject::new(),
    }
}

fn call_result_to_object(result: &CallToolResult) -> JsonObject {
    match serde_json::to_value(result) {
        Ok(Value::Object(map)) => map,
        _ => JsonObject::new(),
    }
}

/// What a background task produces when it finishes.
pub(super) enum TaskOutcome {
    /// Retained result keeping its originating query credit until the task is evicted.
    Retained(RetainedCallResult),
    /// Plain `CallToolResult` fallback (e.g., when a tool failed before preparation or in
    /// direct unit tests).
    Fallback(CallToolResult),
}

/// Snapshot returned to `Adapter::get_task` so a qualified transport can render the wire
/// frame directly from the retained result without cloning an uncharged `JsonObject`.
pub(super) enum TaskSnapshot {
    Detailed(DetailedTask),
    CompletedRetained {
        task: Task,
        result: RetainedCallResult,
    },
}

enum TaskState {
    Working {
        cancel: Arc<AtomicBool>,
        cancel_tx: watch::Sender<bool>,
        join_handle: Option<AbortHandle>,
    },
    Completed {
        outcome: TaskOutcome,
        terminal_at: TaskInstant,
        evict_timer: Option<AbortHandle>,
    },
    Failed {
        error: McpError,
        terminal_at: TaskInstant,
        evict_timer: Option<AbortHandle>,
    },
    Cancelled {
        terminal_at: TaskInstant,
        evict_timer: Option<AbortHandle>,
    },
}

impl TaskState {
    fn status(&self) -> TaskStatus {
        match self {
            Self::Working { .. } => TaskStatus::Working,
            Self::Completed { .. } => TaskStatus::Completed,
            Self::Failed { .. } => TaskStatus::Failed,
            Self::Cancelled { .. } => TaskStatus::Cancelled,
        }
    }

    fn is_working(&self) -> bool {
        matches!(self, Self::Working { .. })
    }

    fn terminal_at(&self) -> Option<TaskInstant> {
        match self {
            Self::Working { .. } => None,
            Self::Completed { terminal_at, .. }
            | Self::Failed { terminal_at, .. }
            | Self::Cancelled { terminal_at, .. } => Some(*terminal_at),
        }
    }

    fn abort_handles(&mut self) {
        match self {
            Self::Working {
                cancel,
                join_handle,
                ..
            } => {
                cancel.store(true, Ordering::Relaxed);
                if let Some(handle) = join_handle.take() {
                    handle.abort();
                }
            }
            Self::Completed { evict_timer, .. }
            | Self::Failed { evict_timer, .. }
            | Self::Cancelled { evict_timer, .. } => {
                if let Some(handle) = evict_timer.take() {
                    handle.abort();
                }
            }
        }
    }
}

struct TaskEntry {
    owner: String,
    task: Task,
    state: TaskState,
    created: TaskInstant,
    ttl: Duration,
}

impl TaskEntry {
    fn touch(&mut self) {
        self.task.last_updated_at = current_timestamp();
        self.task.status = self.state.status();
    }

    fn snapshot(&self) -> TaskSnapshot {
        let mut task = self.task.clone();
        task.status = self.state.status();
        match &self.state {
            TaskState::Working { .. } => {
                TaskSnapshot::Detailed(DetailedTask::new(task, TaskPayload::Working))
            }
            TaskState::Cancelled { .. } => {
                TaskSnapshot::Detailed(DetailedTask::new(task, TaskPayload::Cancelled))
            }
            TaskState::Failed { error, .. } => TaskSnapshot::Detailed(DetailedTask::new(
                task,
                TaskPayload::Failed {
                    error: error_to_object(error),
                },
            )),
            TaskState::Completed {
                outcome: TaskOutcome::Retained(retained),
                ..
            } => TaskSnapshot::CompletedRetained {
                task,
                result: retained.clone(),
            },
            TaskState::Completed {
                outcome: TaskOutcome::Fallback(result),
                ..
            } => TaskSnapshot::Detailed(DetailedTask::new(
                task,
                TaskPayload::Completed {
                    result: call_result_to_object(result),
                },
            )),
        }
    }

    #[cfg(test)]
    fn detailed(&self) -> DetailedTask {
        match self.snapshot() {
            TaskSnapshot::Detailed(d) => d,
            TaskSnapshot::CompletedRetained { task, result } => DetailedTask::new(
                task,
                TaskPayload::Completed {
                    result: result.to_json_object(),
                },
            ),
        }
    }
}

impl Drop for TaskEntry {
    fn drop(&mut self) {
        self.state.abort_handles();
    }
}

#[derive(Default)]
struct Inner {
    tasks: HashMap<String, TaskEntry>,
    shut_down: bool,
    shutdown_watchers: Vec<AbortHandle>,
}

/// The tasks of one server, with their callers and retained results.
pub struct Tasks {
    inner: Arc<Mutex<Inner>>,
    ttl: Duration,
    clock: TaskClock,
}

impl Default for Tasks {
    fn default() -> Self {
        Self::with_ttl(TTL)
    }
}

impl Drop for Tasks {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Tasks {
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            ttl,
            clock: TaskClock::system(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_ttl_and_clock(ttl: Duration, clock: ManualClock) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            ttl,
            clock: TaskClock::Manual(clock),
        }
    }

    /// Propagate a server shutdown token to this task store so active tasks stop and
    /// retained task results release their `QueryPool` credit immediately on shutdown,
    /// even while the router or server remains alive.
    pub fn attach_shutdown(&self, shutdown: CancellationToken) {
        let inner_arc = self.inner.clone();
        let mut inner = self.inner.lock();
        if inner.shut_down || shutdown.is_cancelled() {
            drop(inner);
            self.shutdown();
            return;
        }
        let watcher = tokio::spawn(async move {
            shutdown.cancelled().await;
            Self::shutdown_inner(&inner_arc);
        })
        .abort_handle();
        inner.shutdown_watchers.push(watcher);
    }

    fn shutdown_inner(inner_arc: &Arc<Mutex<Inner>>) {
        let dropped: Vec<TaskEntry> = {
            let mut inner = inner_arc.lock();
            inner.shut_down = true;
            for handle in inner.shutdown_watchers.drain(..) {
                handle.abort();
            }
            inner.tasks.drain().map(|(_, entry)| entry).collect()
        };
        // Drop entries outside the mutex so pool wake-ups never run under `Tasks`'s lock.
        drop(dropped);
    }

    /// Abort all running tasks and release all retained task results immediately.
    pub fn shutdown(&self) {
        Self::shutdown_inner(&self.inner);
    }

    fn sweep_expired(inner_arc: &Arc<Mutex<Inner>>, clock: &TaskClock, inner: &mut Inner) {
        let now = clock.now();
        // Phase 1: fail overdue running tasks, drop their join handles, and schedule
        // their terminal eviction timers so failed entries never linger without new RPCs.
        for (id, entry) in inner.tasks.iter_mut() {
            if entry.state.is_working() && now.saturating_duration_since(entry.created) >= entry.ttl
            {
                entry.state.abort_handles();
                let evict_timer = Some(Self::schedule_eviction(
                    inner_arc.clone(),
                    clock.clone(),
                    id.clone(),
                    now,
                    entry.ttl,
                ));
                entry.state = TaskState::Failed {
                    error: McpError::internal_error(
                        "task expired: TTL elapsed before completion",
                        None,
                    ),
                    terminal_at: now,
                    evict_timer,
                };
                entry.touch();
            }
        }
        // Phase 2: evict expired terminal tasks so their retained results drop.
        inner.tasks.retain(|_, entry| {
            entry
                .state
                .terminal_at()
                .is_none_or(|at| now.saturating_duration_since(at) < entry.ttl)
        });
    }

    fn schedule_eviction(
        inner: Arc<Mutex<Inner>>,
        clock: TaskClock,
        task_id: String,
        terminal_at: TaskInstant,
        ttl: Duration,
    ) -> AbortHandle {
        tokio::spawn(async move {
            clock.sleep_until(terminal_at, ttl).await;
            let removed = {
                let mut guard = inner.lock();
                if let Some(entry) = guard.tasks.get(&task_id)
                    && entry
                        .state
                        .terminal_at()
                        .is_some_and(|at| clock.now().saturating_duration_since(at) >= entry.ttl)
                {
                    guard.tasks.remove(&task_id)
                } else {
                    None
                }
            };
            drop(removed);
        })
        .abort_handle()
    }

    /// Whether another task may start.
    pub fn can_start(&self) -> bool {
        let mut inner = self.inner.lock();
        if inner.shut_down {
            return false;
        }
        Self::sweep_expired(&self.inner, &self.clock, &mut inner);
        inner
            .tasks
            .values()
            .filter(|e| e.state.is_working())
            .count()
            < MAX_RUNNING
    }

    /// Atomically admit the running call `work` as a background task of `owner` if the
    /// server has not shut down, `request_ct` has not been cancelled, and fewer than
    /// [`MAX_RUNNING`] tasks are active. Returns `Err(work)` without cancelling `cancel`
    /// when full, shut down, or cancelled so the caller can keep running `work` inline
    /// or handle cancellation cleanly.
    /// `cancel` is the call's cancel flag, which `tasks/cancel` sets.
    pub(super) fn try_start_retained(
        &self,
        owner: String,
        mut work: JoinHandle<TaskOutcome>,
        cancel: Arc<AtomicBool>,
        request_ct: Option<&CancellationToken>,
    ) -> Result<Task, JoinHandle<TaskOutcome>> {
        let ttl = self.ttl;
        let clock = self.clock.clone();
        let (task_id, task, created, mut cancel_rx) = {
            let mut inner = self.inner.lock();
            if inner.shut_down || request_ct.is_some_and(CancellationToken::is_cancelled) {
                return Err(work);
            }
            Self::sweep_expired(&self.inner, &self.clock, &mut inner);
            let running = inner
                .tasks
                .values()
                .filter(|e| e.state.is_working())
                .count();
            if running >= MAX_RUNNING {
                return Err(work);
            }
            let task_id = uuid::Uuid::new_v4().to_string();
            let now = current_timestamp();
            let created = self.clock.now();
            let task = Task::new(task_id.clone(), TaskStatus::Working, now.clone(), now)
                .with_ttl_ms(ttl.as_millis() as u64)
                .with_poll_interval_ms(POLL_MS)
                .with_status_message("running");
            let (cancel_tx, cancel_rx) = watch::channel(false);
            let entry = TaskEntry {
                owner,
                task: task.clone(),
                state: TaskState::Working {
                    cancel: cancel.clone(),
                    cancel_tx,
                    join_handle: None,
                },
                created,
                ttl,
            };
            inner.tasks.insert(task_id.clone(), entry);
            (task_id, task, created, cancel_rx)
        };

        let inner_arc = self.inner.clone();
        let id_for_task = task_id.clone();
        let handle = tokio::spawn(async move {
            let outcome = tokio::select! {
                biased;
                _ = async {
                    while !*cancel_rx.borrow_and_update() {
                        if cancel_rx.changed().await.is_err() {
                            return;
                        }
                    }
                } => {
                    cancel.store(true, Ordering::Relaxed);
                    // Wait for the blocking engine work to observe cancel and drop its credit.
                    let _ = work.await;
                    Err(None)
                }
                () = clock.sleep_until(created, ttl) => {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = work.await;
                    Err(Some(McpError::internal_error(
                        "task expired: TTL elapsed before completion",
                        None,
                    )))
                }
                r = &mut work => match r {
                    Ok(out) => Ok(out),
                    Err(e) => {
                        tracing::error!("MCP task failed: {e}");
                        Err(Some(McpError::internal_error("the tool call failed", None)))
                    }
                },
            };
            // Release active-query `INITIAL_GRANT` slack outside `Tasks`'s mutex before
            // retaining the completed result for `TTL`.
            if let Ok(TaskOutcome::Retained(ref retained)) = outcome {
                retained.trim_grant();
            }
            let mut dropped_outcome: Option<Result<TaskOutcome, Option<McpError>>> = None;
            {
                let mut guard = inner_arc.lock();
                let Some(entry) = guard.tasks.get_mut(&id_for_task) else {
                    dropped_outcome = Some(outcome);
                    drop(guard);
                    drop(dropped_outcome);
                    return;
                };
                if !entry.state.is_working() {
                    dropped_outcome = Some(outcome);
                    drop(guard);
                    drop(dropped_outcome);
                    return;
                }
                let terminal_at = clock.now();
                let evict_timer = Some(Self::schedule_eviction(
                    inner_arc.clone(),
                    clock.clone(),
                    id_for_task.clone(),
                    terminal_at,
                    entry.ttl,
                ));
                // Re-check acknowledged cancellation under the lock so a cancel that raced
                // with `work` finishing still transitions to Cancelled and drops credit now.
                if *cancel_rx.borrow() && outcome.is_ok() {
                    dropped_outcome = Some(outcome);
                    entry.state = TaskState::Cancelled {
                        terminal_at,
                        evict_timer,
                    };
                } else {
                    entry.state = match outcome {
                        Ok(out) => TaskState::Completed {
                            outcome: out,
                            terminal_at,
                            evict_timer,
                        },
                        Err(None) => TaskState::Cancelled {
                            terminal_at,
                            evict_timer,
                        },
                        Err(Some(error)) => TaskState::Failed {
                            error,
                            terminal_at,
                            evict_timer,
                        },
                    };
                }
                entry.touch();
            }
            drop(dropped_outcome);
        });

        let mut inner = self.inner.lock();
        match inner.tasks.get_mut(&task_id) {
            Some(entry) => {
                if let TaskState::Working { join_handle, .. } = &mut entry.state {
                    *join_handle = Some(handle.abort_handle());
                }
            }
            None => handle.abort(),
        }
        Ok(task)
    }

    /// Remove and drop a task immediately without waiting for TTL.
    ///
    /// Used when request cancellation races task detachment before the caller can
    /// receive the task ID, ensuring no unreachable task or retained credit lingers.
    pub(super) fn retire(&self, id: &str) {
        let removed = {
            let mut inner = self.inner.lock();
            inner.tasks.remove(id)
        };
        drop(removed);
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.inner.lock().tasks.len()
    }

    /// Convenience wrapper for tests that spawn a plain `CallToolResult`.
    #[cfg(test)]
    pub fn start(
        &self,
        owner: String,
        work: JoinHandle<CallToolResult>,
        cancel: Arc<AtomicBool>,
    ) -> Task {
        let wrapped = tokio::spawn(async move {
            match work.await {
                Ok(result) => TaskOutcome::Fallback(result),
                Err(_) => TaskOutcome::Fallback(CallToolResult::error(Vec::new())),
            }
        });
        self.try_start_retained(owner, wrapped, cancel, None)
            .ok()
            .expect("task capacity available")
    }

    /// `tasks/get` of `owner`, returning a retained snapshot when completed.
    pub(super) fn get_retained(&self, owner: &str, id: &str) -> Result<TaskSnapshot, McpError> {
        let mut inner = self.inner.lock();
        Self::sweep_expired(&self.inner, &self.clock, &mut inner);
        let entry = inner
            .tasks
            .get(id)
            .filter(|e| e.owner == owner)
            .ok_or_else(|| unknown(id))?;
        Ok(entry.snapshot())
    }

    /// `tasks/get` of `owner` as a plain `DetailedTask`.
    #[cfg(test)]
    pub fn get(&self, owner: &str, id: &str) -> Result<DetailedTask, McpError> {
        let mut inner = self.inner.lock();
        Self::sweep_expired(&self.inner, &self.clock, &mut inner);
        let entry = inner
            .tasks
            .get(id)
            .filter(|e| e.owner == owner)
            .ok_or_else(|| unknown(id))?;
        Ok(entry.detailed())
    }

    /// `tasks/cancel` of `owner`.
    ///
    /// Per MCP SEP-2663, cancelling a running task signals its cooperative cancel flag so
    /// it stops and releases credit immediately; cancelling an already-terminal task is an
    /// idempotent acknowledgement that leaves its terminal state unchanged.
    pub fn cancel(&self, owner: &str, id: &str) -> Result<(), McpError> {
        let mut inner = self.inner.lock();
        Self::sweep_expired(&self.inner, &self.clock, &mut inner);
        let entry = inner
            .tasks
            .get_mut(id)
            .filter(|e| e.owner == owner)
            .ok_or_else(|| unknown(id))?;
        if let TaskState::Working {
            cancel, cancel_tx, ..
        } = &entry.state
        {
            cancel.store(true, Ordering::Relaxed);
            let _ = cancel_tx.send(true);
            entry.touch();
        }
        Ok(())
    }

    /// `tasks/update` of `owner`: no tool asks for input during a task.
    pub fn update(&self, owner: &str, id: &str) -> Result<(), McpError> {
        let mut inner = self.inner.lock();
        Self::sweep_expired(&self.inner, &self.clock, &mut inner);
        let _ = inner
            .tasks
            .get(id)
            .filter(|e| e.owner == owner)
            .ok_or_else(|| unknown(id))?;
        Err(McpError::invalid_params(
            format!("task {id} asks for no input"),
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn try_start_retained_enforces_max_running_atomically() {
        let tasks = Tasks::with_ttl(Duration::from_secs(60));
        let mut gates = Vec::new();
        for _ in 0..MAX_RUNNING {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            gates.push(tx);
            let work = tokio::spawn(async move {
                let _ = rx.await;
                TaskOutcome::Fallback(CallToolResult::default())
            });
            assert!(
                tasks
                    .try_start_retained(
                        "alice".into(),
                        work,
                        Arc::new(AtomicBool::new(false)),
                        None
                    )
                    .is_ok()
            );
        }
        // MAX_RUNNING + 1 must be refused atomically and return `work` WITHOUT tripping `cancel`.
        let extra_cancel = Arc::new(AtomicBool::new(false));
        let check_cancel = extra_cancel.clone();
        let (extra_tx, extra_rx) = tokio::sync::oneshot::channel::<()>();
        let extra = tokio::spawn(async move {
            let _ = extra_rx.await;
            if check_cancel.load(Ordering::SeqCst) {
                TaskOutcome::Fallback(CallToolResult::error(Vec::new()))
            } else {
                TaskOutcome::Fallback(CallToolResult::success(Vec::new()))
            }
        });
        let returned = tasks
            .try_start_retained("alice".into(), extra, extra_cancel.clone(), None)
            .expect_err("must reject beyond MAX_RUNNING");
        assert!(
            !extra_cancel.load(Ordering::SeqCst),
            "refusal beyond MAX_RUNNING must not cancel inline fallback work"
        );
        let _ = extra_tx.send(());
        match returned.await.unwrap() {
            TaskOutcome::Fallback(r) => assert_eq!(r.is_error, Some(false)),
            TaskOutcome::Retained(_) => panic!("unexpected retained"),
        }

        // Cancelled request token refuses detachment without setting cancel on `work`.
        let ct = CancellationToken::new();
        ct.cancel();
        let tasks2 = Tasks::with_ttl(Duration::from_secs(60));
        let ct_cancel = Arc::new(AtomicBool::new(false));
        let ct_work = tokio::spawn(async { TaskOutcome::Fallback(CallToolResult::default()) });
        let ct_ret = tasks2
            .try_start_retained("alice".into(), ct_work, ct_cancel.clone(), Some(&ct))
            .expect_err("cancelled token must refuse detachment");
        assert!(!ct_cancel.load(Ordering::SeqCst));
        assert!(tasks2.inner.lock().tasks.is_empty());
        let _ = ct_ret.await;

        // Shut-down store also refuses without tripping `cancel`.
        tasks2.shutdown();
        let sd_cancel = Arc::new(AtomicBool::new(false));
        let sd_work = tokio::spawn(async { TaskOutcome::Fallback(CallToolResult::default()) });
        let sd_ret = tasks2
            .try_start_retained("alice".into(), sd_work, sd_cancel.clone(), None)
            .expect_err("shut down store must refuse detachment");
        assert!(!sd_cancel.load(Ordering::SeqCst));
        let _ = sd_ret.await;

        tasks.shutdown();
    }

    #[tokio::test]
    async fn sweep_expired_phase1_schedules_eviction_timer() {
        let tasks = Tasks::with_ttl(Duration::from_millis(60));
        let work = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            TaskOutcome::Fallback(CallToolResult::default())
        });
        let task = tasks
            .try_start_retained("alice".into(), work, Arc::new(AtomicBool::new(false)), None)
            .ok()
            .unwrap();
        // Wait past initial TTL so Phase 1 transitions Working -> Failed with an eviction timer.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let _ = tasks.can_start();
        // Wait past the second TTL window without any further RPCs: eviction timer removes it.
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(!tasks.inner.lock().tasks.contains_key(&task.task_id));
    }

    #[tokio::test]
    async fn retire_immediately_cancels_and_removes_detached_task() {
        let tasks = Tasks::with_ttl(Duration::from_secs(60));
        let cancel = Arc::new(AtomicBool::new(false));
        let work = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            TaskOutcome::Fallback(CallToolResult::default())
        });
        let task = tasks
            .try_start_retained("alice".into(), work, cancel.clone(), None)
            .ok()
            .unwrap();
        tasks.retire(&task.task_id);
        assert!(cancel.load(Ordering::SeqCst));
        assert!(!tasks.inner.lock().tasks.contains_key(&task.task_id));
    }
}
