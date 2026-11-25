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

/// Tasks running at once, over all callers.
pub const MAX_RUNNING: usize = 64;

/// How long a task is kept: a running one is stopped after this, and an ended one is
/// dropped this long after it ended.
pub const TTL: Duration = Duration::from_secs(600);

/// The polling interval suggested to clients.
const POLL_MS: u64 = 500;

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
        terminal_at: Instant,
        evict_timer: Option<AbortHandle>,
    },
    Failed {
        error: McpError,
        terminal_at: Instant,
        evict_timer: Option<AbortHandle>,
    },
    Cancelled {
        terminal_at: Instant,
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

    fn terminal_at(&self) -> Option<Instant> {
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
    created: Instant,
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
}

/// The tasks of one server, with their callers and retained results.
pub struct Tasks {
    inner: Arc<Mutex<Inner>>,
    ttl: Duration,
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
        }
    }

    /// Abort all running tasks and release all retained task results immediately.
    pub fn shutdown(&self) {
        let mut inner = self.inner.lock();
        for (_, mut entry) in inner.tasks.drain() {
            entry.state.abort_handles();
        }
    }

    fn sweep_expired(inner: &mut Inner) {
        let now = Instant::now();
        // Phase 1: fail overdue running tasks and drop their join handles.
        for entry in inner.tasks.values_mut() {
            if entry.state.is_working() && now.saturating_duration_since(entry.created) >= entry.ttl
            {
                entry.state.abort_handles();
                entry.state = TaskState::Failed {
                    error: McpError::internal_error(
                        "task expired: TTL elapsed before completion",
                        None,
                    ),
                    terminal_at: now,
                    evict_timer: None,
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

    fn schedule_eviction(inner: Arc<Mutex<Inner>>, task_id: String, ttl: Duration) -> AbortHandle {
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let mut guard = inner.lock();
            if let Some(entry) = guard.tasks.get(&task_id)
                && entry
                    .state
                    .terminal_at()
                    .is_some_and(|at| at.elapsed() >= entry.ttl)
            {
                guard.tasks.remove(&task_id);
            }
        })
        .abort_handle()
    }

    /// Whether another task may start.
    pub fn can_start(&self) -> bool {
        let mut inner = self.inner.lock();
        Self::sweep_expired(&mut inner);
        inner
            .tasks
            .values()
            .filter(|e| e.state.is_working())
            .count()
            < MAX_RUNNING
    }

    /// Make the running call `work` a task of `owner` that retains its credited result.
    /// `cancel` is the call's cancel flag, which `tasks/cancel` sets.
    pub(super) fn start_retained(
        &self,
        owner: String,
        mut work: JoinHandle<TaskOutcome>,
        cancel: Arc<AtomicBool>,
    ) -> Task {
        let task_id = uuid::Uuid::new_v4().to_string();
        let now = current_timestamp();
        let ttl = self.ttl;
        let task = Task::new(task_id.clone(), TaskStatus::Working, now.clone(), now)
            .with_ttl_ms(ttl.as_millis() as u64)
            .with_poll_interval_ms(POLL_MS)
            .with_status_message("running");
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let entry = TaskEntry {
            owner,
            task: task.clone(),
            state: TaskState::Working {
                cancel: cancel.clone(),
                cancel_tx,
                join_handle: None,
            },
            created: Instant::now(),
            ttl,
        };
        {
            let mut inner = self.inner.lock();
            Self::sweep_expired(&mut inner);
            inner.tasks.insert(task_id.clone(), entry);
        }

        let inner_arc = self.inner.clone();
        let id_for_task = task_id.clone();
        let handle = tokio::spawn(async move {
            let outcome = tokio::select! {
                r = &mut work => match r {
                    Ok(out) => Ok(out),
                    Err(e) => {
                        tracing::error!("MCP task failed: {e}");
                        Err(Some(McpError::internal_error("the tool call failed", None)))
                    }
                },
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
                () = tokio::time::sleep(ttl) => {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = work.await;
                    Err(Some(McpError::internal_error(
                        "task expired: TTL elapsed before completion",
                        None,
                    )))
                }
            };
            let mut guard = inner_arc.lock();
            let Some(entry) = guard.tasks.get_mut(&id_for_task) else {
                // Task was already evicted or shut down; drop `outcome` outside the lock.
                drop(guard);
                drop(outcome);
                return;
            };
            if !entry.state.is_working() {
                drop(guard);
                drop(outcome);
                return;
            }
            let terminal_at = Instant::now();
            let evict_timer = Some(Self::schedule_eviction(
                inner_arc.clone(),
                id_for_task.clone(),
                entry.ttl,
            ));
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
            entry.touch();
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
        task
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
        self.start_retained(owner, wrapped, cancel)
    }

    /// `tasks/get` of `owner`, returning a retained snapshot when completed.
    pub(super) fn get_retained(&self, owner: &str, id: &str) -> Result<TaskSnapshot, McpError> {
        let mut inner = self.inner.lock();
        Self::sweep_expired(&mut inner);
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
        Self::sweep_expired(&mut inner);
        let entry = inner
            .tasks
            .get(id)
            .filter(|e| e.owner == owner)
            .ok_or_else(|| unknown(id))?;
        Ok(entry.detailed())
    }

    /// `tasks/cancel` of `owner`.
    pub fn cancel(&self, owner: &str, id: &str) -> Result<(), McpError> {
        let mut inner = self.inner.lock();
        Self::sweep_expired(&mut inner);
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
        Self::sweep_expired(&mut inner);
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
