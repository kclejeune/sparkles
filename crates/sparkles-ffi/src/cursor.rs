//! Explicit, fallible SELECT cursors. Existing FfiQuery execution stays eager.
use crate::encode::{self, RowWriter, TermTable};
use crate::error::{ErrorKind, FfiError, FfiResult};
use crate::labels::Labels;
use crate::read::Batch;
use parking_lot::Mutex;
use sparkles::sparql::{CursorStatus, QueryBatch, QueryCursor};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct State {
    cursor: QueryCursor,
    batch: Option<QueryBatch>,
    row: usize,
    encoded: u64,
    terminal: Option<CursorStatus>,
    error: Option<String>,
}

impl State {
    fn status(&self) -> CursorStatus {
        let status = self.terminal.unwrap_or_else(|| self.cursor.status());
        if status == CursorStatus::Complete
            && self.batch.as_ref().is_some_and(|b| self.row < b.len())
        {
            CursorStatus::Open
        } else {
            status
        }
    }
}

#[derive(uniffi::Object)]
pub struct FfiSelectCursor {
    state: Mutex<State>,
    variables: Vec<String>,
    labels: Labels,
    cancel: Arc<AtomicBool>,
    closed: AtomicBool,
}

impl FfiSelectCursor {
    pub(crate) fn new(cursor: QueryCursor, labels: Labels, cancel: Arc<AtomicBool>) -> Self {
        let variables = cursor.variables().to_vec();
        Self {
            state: Mutex::new(State {
                cursor,
                batch: None,
                row: 0,
                encoded: 0,
                terminal: None,
                error: None,
            }),
            variables,
            labels,
            cancel,
            closed: AtomicBool::new(false),
        }
    }
}

#[uniffi::export]
impl FfiSelectCursor {
    pub fn variables(&self) -> Vec<String> {
        self.variables.clone()
    }

    /// Encoding dictionaries restart every batch, bounding both native and JVM
    /// decoder state. The returned bytes use the existing row-batch encoding.
    pub fn next_batch(&self, max_rows: u32) -> FfiResult<Batch> {
        let mut state = self.state.try_lock().ok_or_else(|| {
            FfiError::new(ErrorKind::Invalid, "a cursor operation is in progress")
        })?;
        if self.variables.len() > u16::MAX as usize {
            return Err(FfiError::new(
                ErrorKind::Invalid,
                "too many columns for the JVM row encoding",
            ));
        }
        if self.closed.load(Ordering::Relaxed) {
            state.batch = None;
            state.cursor.close();
            return Ok(Batch {
                batch: encode::empty_batch(self.variables.len() as u16),
                done: true,
            });
        }
        let limit = max_rows.clamp(1, 1 << 16) as usize;
        let result: sparkles::Result<Batch> = self.labels.with(|_, label| {
            // A fresh table forces self-contained wire batches. The first frame
            // also explicitly restarts the JVM dictionary (bit zero).
            let mut table = TermTable::new(1);
            let mut writer = RowWriter::new(&mut table, self.variables.len() as u16);
            let mut row = vec![0; self.variables.len()];
            while writer.rows() < limit as u32 && writer.bytes() < 1 << 20 {
                if self.closed.load(Ordering::Relaxed) || self.cancel.load(Ordering::Relaxed) {
                    return Err(sparkles::Error::Cancelled);
                }
                if state.batch.as_ref().is_none_or(|b| state.row == b.len()) {
                    state.batch = None;
                    state.row = 0;
                    state.batch = state.cursor.next_batch()?;
                }
                let Some(batch) = &state.batch else { break };
                for (column, cell) in row.iter_mut().enumerate() {
                    let term = batch.term(state.row, column)?;
                    *cell = term.map_or(0, |term| writer.cell(term.clone(), || Some(term), label));
                }
                writer.push_row(&row);
                state.row += 1;
                state.encoded += 1;
            }
            let done = state.cursor.status() != CursorStatus::Open
                && state.batch.as_ref().is_none_or(|b| state.row == b.len());
            if done {
                state.batch = None;
            }
            let mut bytes = writer.finish();
            bytes[1] |= 1;
            Ok(Batch { batch: bytes, done })
        });
        if let Err(error) = &result {
            state.terminal = Some(if self.closed.load(Ordering::Relaxed) {
                CursorStatus::Stopped
            } else {
                CursorStatus::Failed
            });
            state.error = Some(error.to_string());
            state.batch = None;
            state.cursor.close();
            self.closed.store(true, Ordering::Relaxed);
        }
        result.map_err(Into::into)
    }

    pub fn status(&self) -> FfiResult<String> {
        let state = self.state.try_lock().ok_or_else(|| {
            FfiError::new(ErrorKind::Invalid, "a cursor operation is in progress")
        })?;
        Ok(state
            .terminal
            .unwrap_or_else(|| state.status())
            .as_str()
            .into())
    }

    pub fn stats_json(&self) -> FfiResult<String> {
        let state = self.state.try_lock().ok_or_else(|| {
            FfiError::new(ErrorKind::Invalid, "a cursor operation is in progress")
        })?;
        let mut stats = state.cursor.stats();
        stats.status = state.status();
        if let Some(terminal) = state.terminal {
            stats.status = terminal;
        }
        if let Some(error) = &state.error {
            stats.error = Some(error.clone());
        }
        Ok(serde_json::json!({"stats":stats, "encodedRows":state.encoded}).to_string())
    }

    pub fn plan_json(&self) -> FfiResult<String> {
        let state = self.state.try_lock().ok_or_else(|| {
            FfiError::new(ErrorKind::Invalid, "a cursor operation is in progress")
        })?;
        state.cursor.plan_json().map_err(Into::into)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Also interrupts a pull on another thread. The active pull releases its
    /// batch when it observes close; no thread waits on its own callback lock.
    pub fn release(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(mut state) = self.state.try_lock() {
            if state
                .batch
                .as_ref()
                .is_some_and(|batch| state.row < batch.len())
                && state.cursor.status() == CursorStatus::Complete
            {
                state.terminal = Some(CursorStatus::Stopped);
            }
            state.batch = None;
            state.cursor.close();
        }
    }
}

impl Drop for FfiSelectCursor {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
