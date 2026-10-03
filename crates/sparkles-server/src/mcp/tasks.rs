//! The tasks extension of MCP (SEP-2663, `io.modelcontextprotocol/tasks`): a tool call
//! of a client that declares the extension and runs longer than
//! [`McpConfig::task_after`](super::McpConfig::task_after) becomes a task. The client
//! then polls `tasks/get` for its state and result, and may stop it with
//! `tasks/cancel`, which sets the call's cancel flag as `notifications/cancelled` would.
//!
//! A task belongs to the caller that started it: another caller's `tasks/get` or
//! `tasks/cancel` gets the answer for an unknown task. Tasks keep their result for
//! [`TTL`] after they end, and at most [`MAX_RUNNING`] run at once; beyond that a call
//! simply runs to its end, as for a client without the extension. With `adapter.rs` and
//! `http.rs`, this is the only code that knows the MCP SDK.

use parking_lot::Mutex;
use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, DetailedTask, Task};
use rmcp::task_manager::{TaskExit, TaskManager, TaskOptions};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle;

/// Tasks running at once, over all callers.
pub const MAX_RUNNING: usize = 64;

/// How long a task is kept: a running one is stopped after this, and an ended one is
/// dropped this long after it ended.
pub const TTL: Duration = Duration::from_secs(600);

/// The polling interval suggested to clients.
const POLL_MS: u64 = 500;

/// The tasks of one server, with their callers.
#[derive(Default)]
pub struct Tasks {
    manager: TaskManager,
    /// task id → [`crate::auth::Principal::id`] of its caller
    owners: Mutex<HashMap<String, String>>,
}

fn unknown(id: &str) -> McpError {
    McpError::invalid_params(format!("unknown task: {id}"), None)
}

impl Tasks {
    /// Whether another task may start.
    pub fn can_start(&self) -> bool {
        self.manager.running_task_count() < MAX_RUNNING
    }

    /// Make the running call `work` a task of `owner`. `cancel` is the call's cancel
    /// flag, which `tasks/cancel` sets.
    pub fn start(
        &self,
        owner: String,
        mut work: JoinHandle<CallToolResult>,
        cancel: Arc<AtomicBool>,
    ) -> Task {
        let options = TaskOptions::new()
            .with_ttl_ms(TTL.as_millis() as u64)
            .with_poll_interval_ms(POLL_MS)
            .with_status_message("running");
        let task = self.manager.spawn(options, move |ctx| {
            Box::pin(async move {
                tokio::select! {
                    r = &mut work => r.map_err(|e| {
                        tracing::error!("MCP task failed: {e}");
                        TaskExit::Error(McpError::internal_error("the tool call failed", None))
                    }),
                    () = ctx.cancelled() => {
                        // the engine stops at its next check; the call's result is moot
                        cancel.store(true, Ordering::Relaxed);
                        let _ = work.await;
                        Err(TaskExit::Cancelled)
                    }
                }
            })
        });
        let mut owners = self.owners.lock();
        if owners.len() >= 4 * MAX_RUNNING {
            // forget the tasks the manager dropped
            owners.retain(|id, _| self.manager.get_task(id).is_ok());
        }
        owners.insert(task.task_id.clone(), owner);
        task
    }

    fn check(&self, owner: &str, id: &str) -> Result<(), McpError> {
        match self.owners.lock().get(id) {
            Some(o) if o == owner => Ok(()),
            _ => Err(unknown(id)),
        }
    }

    /// `tasks/get` of `owner`.
    pub fn get(&self, owner: &str, id: &str) -> Result<DetailedTask, McpError> {
        self.check(owner, id)?;
        self.manager.get_task(id)
    }

    /// `tasks/cancel` of `owner`.
    pub fn cancel(&self, owner: &str, id: &str) -> Result<(), McpError> {
        self.check(owner, id)?;
        self.manager.cancel_task(id)
    }

    /// `tasks/update` of `owner`: no tool asks for input during a task.
    pub fn update(&self, owner: &str, id: &str) -> Result<(), McpError> {
        self.check(owner, id)?;
        self.manager.get_task(id)?;
        Err(McpError::invalid_params(
            format!("task {id} asks for no input"),
            None,
        ))
    }
}
