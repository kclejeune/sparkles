//! The sources that the result writers write from, an eager result or a cursor.
//!
//! A source drives and the writer is a visitor, since both of the eager answer's and the
//! cursor's decoding strategies own a scope around the writing: the cursor decodes its
//! next batch on the pool while the current one is written, and a large eager answer
//! decodes its next chunk on a scoped thread. A source hands the writer one batch of rows
//! at a time, and each row is decoded into the batch's buffer when the writer asks for it.
//! Every source keeps its own checks, its own memory accounting and its own trailer, so
//! that each writes the bytes it wrote before the writers were shared.

use super::{
    ChunkFailure, ChunkRow, DECODE_ROWS, Decoded, NativeJsonMetadata, NativeMemory, NativeMeta,
    RowCharge, RowOut, json, output, prefetch_answer,
};
use crate::error::{Error, Result};
use crate::sparql::ctx::Ctx;
use crate::sparql::{CursorStats, QueryKind, QueryResult};
use oxrdf::Term;
use serde::Serialize;
use std::borrow::Cow;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

/// One decoded row: every cell of the row in column order, or only its bound cells with
/// their columns, as the source decodes them.
pub(super) enum RowView<'r> {
    Dense(&'r [Option<Term>]),
    Sparse(&'r [(usize, Cow<'r, Term>)]),
}

/// A batch of rows, decoded one at a time into the batch's buffer. A row's view lives
/// until the next call.
pub(super) trait RowBatch {
    fn len(&self) -> usize;
    fn row(&mut self, i: usize) -> Result<RowView<'_>>;
}

/// The writer's side of [`SolutionSource::drive`]: it writes each batch the source hands
/// it. A source calls it with its own batch type, so that the row loop is compiled for
/// each source with no dynamic call per row.
pub(super) trait BatchVisitor {
    fn batch<B: RowBatch>(&mut self, batch: &mut B) -> Result<()>;
}

/// The solutions of a SELECT or the boolean of an ASK, with the checks, the accounting
/// and the native trailer of the source they come from.
pub(super) trait SolutionSource {
    /// The writer flushes its output after the first batch, so that a writer which
    /// commits a response on a flush sends the first rows while the source computes the
    /// next batch.
    const FLUSH_FIRST: bool;
    fn variables(&self) -> &[String];
    /// The context that the output buffer is charged to.
    fn ctx(&self) -> Arc<Ctx>;
    /// The check before the first byte.
    fn start(&mut self) -> Result<()>;
    /// The boolean of an ASK result.
    fn boolean(&self) -> Option<bool>;
    /// Hand the rows, `send` at most, to `visitor` one batch at a time, with the checks
    /// after the last.
    fn drive<V: BatchVisitor>(&mut self, send: Option<usize>, visitor: &mut V) -> Result<()>;
    /// Execution time already spent when the writer starts.
    fn exec_ms(&self) -> f64;
    /// The native document's `meta` member and its closing brace, after `sent` rows of a
    /// SELECT, or after an ASK's boolean when `sent` is `None`.
    fn native_trailer(
        &mut self,
        out: &mut RowOut<'_>,
        sent: Option<u64>,
        send: Option<usize>,
        metadata: Option<NativeJsonMetadata<'_>>,
        started: Instant,
        initial_exec: f64,
    ) -> Result<()>;
    /// The cursor's statistics after a whole output.
    fn stats(&self) -> Option<CursorStats>;
    /// The output failed with `error`.
    fn fail_output(&mut self, error: &Error);
}

/// An eager result. The terms of an answer of one chunk, and of every native JSON
/// answer, are decoded row by row into one scratch row. Larger answers in the other
/// formats decode a chunk of rows at a time, the next chunk on a scoped thread.
pub(super) struct EagerSolutions<'r> {
    pub(super) r: &'r QueryResult,
    /// decode every row into the scratch row, as the native writer does
    pub(super) scratch: bool,
}

struct ScratchRows<'r> {
    row: output::ScratchRow<'r>,
    n: usize,
}

impl RowBatch for ScratchRows<'_> {
    fn len(&self) -> usize {
        self.n
    }

    #[inline]
    fn row(&mut self, i: usize) -> Result<RowView<'_>> {
        Ok(RowView::Dense(self.row.load(i)?))
    }
}

/// One chunk of a large eager answer. A row's error is kept as the chunk's failure, and
/// the scoped prefetch thread's failure stops the chunk at its next row.
struct ChunkRows<'a> {
    r: &'a QueryResult,
    d: &'a Decoded,
    failure: &'a ChunkFailure,
    start: usize,
    end: usize,
    /// the row last handed out, dropped before the next is decoded
    current: Option<ChunkRow<'a>>,
    /// the index of the row last asked for
    at: usize,
    /// the error came from decoding a row or from the prefetch thread, not the writer
    row_failed: bool,
}

impl RowBatch for ChunkRows<'_> {
    fn len(&self) -> usize {
        self.end - self.start
    }

    fn row(&mut self, i: usize) -> Result<RowView<'_>> {
        self.current = None;
        let index = self.start + i;
        self.at = index;
        if self.failure.failed() {
            self.row_failed = true;
            // the prefetch thread's error is the one recorded, and this one is dropped
            return Err(Error::invalid("query output prefetch failed"));
        }
        match ChunkRow::decode(self.r, self.d, index) {
            Ok(row) => {
                let row = self.current.insert(row);
                Ok(RowView::Sparse(&row.terms))
            }
            Err(error) => {
                self.row_failed = true;
                Err(error)
            }
        }
    }
}

impl SolutionSource for EagerSolutions<'_> {
    const FLUSH_FIRST: bool = false;

    fn variables(&self) -> &[String] {
        self.r.variables()
    }

    fn ctx(&self) -> Arc<Ctx> {
        self.r.inner.ctx.clone()
    }

    fn start(&mut self) -> Result<()> {
        self.r.inner.ctx.check_memory_failure()
    }

    fn boolean(&self) -> Option<bool> {
        (self.r.kind == QueryKind::Ask).then_some(self.r.boolean)
    }

    fn drive<V: BatchVisitor>(&mut self, send: Option<usize>, visitor: &mut V) -> Result<()> {
        let r = self.r;
        let n = send.map_or(r.inner.table.len(), |s| s.min(r.inner.table.len()));
        if self.scratch || n <= DECODE_ROWS {
            // a result of one chunk is decoded row by row: under concurrent load that
            // keeps less memory live per request than a chunk's table of terms, and in the
            // 10.5M benchmark it served 25% more requests a second
            // Even a small answer's terms lie on scattered vocabulary pages. Keep
            // row-by-row decoding's small working set, but overlap those cold reads.
            let ahead = n.min(DECODE_ROWS);
            if ahead >= 2 && crate::index::io_hints() {
                prefetch_answer(r, ahead)?;
            }
            let mut rows = ScratchRows {
                row: output::ScratchRow::new(r)?,
                n,
            };
            visitor.batch(&mut rows)?;
            drop(rows);
            return r.inner.ctx.check_memory_failure();
        }
        let chunk = |start: usize| start..n.min(start + DECODE_ROWS);
        // Scope and one live prefetch packet have fixed runtime headers. Stacks are
        // process runtime memory, as documented by depth::with_stack, not heap DTOs.
        let scope_header = r.inner.ctx.charge(4096)?;
        // This guard must outlive the scope closure: caller unwinding drops that
        // closure's locals before scope automatically joins its real workers.
        let mut packet = None;
        let failure = ChunkFailure::default();
        std::thread::scope(|sc| -> Result<()> {
            #[cfg(test)]
            let _unwind_observer = super::ChunkCallerUnwindTrace(&r.inner.ctx);
            let mut cells = Some(Decoded::try_cells(r, chunk(0), true)?);
            let mut start = 0;
            while start < n {
                let end = n.min(start + DECODE_ROWS);
                let d = Decoded::try_new(r, cells.take().expect("owned next chunk"))?;
                packet = if end < n {
                    Some(r.inner.ctx.charge(4096)?)
                } else {
                    None
                };
                let failure_ref = &failure;
                let next = (end < n).then(|| {
                    sc.spawn(move || {
                        #[cfg(test)]
                        let _trace = super::ChunkWorkerTrace {
                            ctx: &r.inner.ctx,
                            stage: "prefetch-end",
                            count: end,
                        };
                        #[cfg(test)]
                        r.inner.ctx.chunk_event("prefetch-start", end);
                        #[cfg(test)]
                        {
                            use crate::algo::memory::probe;
                            probe::at(probe::Point::Allocation("chunk-prefetch-worker", end));
                            if probe::fails("chunk-prefetch-worker") {
                                panic!("injected chunk prefetch worker panic");
                            }
                        }
                        match Decoded::try_cells(r, chunk(end), true) {
                            Ok(cells) => Some(cells),
                            Err(error) => {
                                failure_ref.record(error);
                                None
                            }
                        }
                    })
                });
                let mut rows = ChunkRows {
                    r,
                    d: &d,
                    failure: &failure,
                    start,
                    end,
                    current: None,
                    at: start,
                    row_failed: false,
                };
                let written = visitor.batch(&mut rows);
                let (_at, row_failed) = (rows.at, rows.row_failed);
                // the last row's terms and credit go before the join
                drop(rows);
                if let Err(error) = written {
                    // the first failure wins, so a row's own error, or the prefetch
                    // thread's, is kept over the one the writer returned for it
                    failure.record(error);
                    #[cfg(test)]
                    if !row_failed {
                        r.inner.ctx.chunk_event("io-failure-recorded", _at);
                    }
                    let _ = row_failed;
                }
                let completed = next.and_then(|handle| match handle.join() {
                    Ok(cells) => cells,
                    Err(panic) => {
                        tracing::warn!(target: "sparkles::sparql::results", "query output prefetch worker panicked");
                        failure.record(Error::invalid("query output prefetch worker panicked"));
                        // Do not destroy an arbitrary panic payload under a failure lock.
                        drop(panic);
                        None
                    }
                });
                drop(packet.take());
                if let Some(error) = failure.take() {
                    return Err(error);
                }
                if end < n {
                    cells = Some(
                        completed
                            .ok_or_else(|| Error::invalid("missing completed prefetch chunk"))?,
                    );
                }
                start = end;
            }
            r.inner.ctx.check()?;
            Ok(())
        })?;
        drop(scope_header);
        Ok(())
    }

    fn exec_ms(&self) -> f64 {
        0.0
    }

    fn native_trailer(
        &mut self,
        out: &mut RowOut<'_>,
        _sent: Option<u64>,
        send: Option<usize>,
        metadata: Option<NativeJsonMetadata<'_>>,
        started: Instant,
        _initial_exec: f64,
    ) -> Result<()> {
        let r = self.r;
        // what `send` selects, which is what a whole output holds
        let n = send.map_or(r.len(), |s| s.min(r.len()));
        r.inner.ctx.check_memory_failure()?;
        let mut timing = r.timing.clone();
        if metadata.is_some() {
            timing.serialize_ms = started.elapsed().as_secs_f64() * 1000.0;
            timing.total_ms += timing.serialize_ms;
        }
        out.write_all(b",\"meta\":")?;
        json(
            out,
            &NativeMeta {
                total_rows: r.len(),
                sent_rows: n,
                timing: &timing,
                plan: &r.inner.plan,
                memory: NativeMemory {
                    peak_bytes: r.mem_peak_bytes,
                    granted_bytes: r.inner.ctx.grant().map(|g| g.peak()),
                },
                rows_produced: r.rows_produced,
                commit: metadata.as_ref().map(|m| m.commit),
                dataset_id: metadata.as_ref().map(|m| m.dataset_id),
                io: metadata.as_ref().and_then(|m| m.io).map(|p| p.report()),
            },
        )?;
        out.write_all(b"}")?;
        Ok(())
    }

    fn stats(&self) -> Option<CursorStats> {
        None
    }

    fn fail_output(&mut self, _error: &Error) {}
}

/// A SELECT cursor. Its batches come from [`crate::sparql::QueryCursor::for_each_decoded`],
/// with the batch's base-vocabulary terms decoded once and the other cells decoded for
/// the row under the writer's [`RowCharge`].
pub(super) struct CursorSolutions<'c> {
    pub(super) cursor: &'c mut crate::sparql::QueryCursor,
}

struct CursorRows<'b> {
    // Rust drops fields in declaration order: the row's owned fallbacks go before the
    // reservation of their slots.
    terms: Vec<(usize, Cow<'b, Term>)>,
    _row_storage: crate::sparql::ctx::OwnedCharge,
    cursor: &'b crate::sparql::QueryCursor,
    batch: &'b crate::sparql::QueryBatch,
    decoded: Option<&'b crate::sparql::cursor::DecodedBatch<'b>>,
    charge: &'b mut RowCharge,
    len: usize,
}

impl RowBatch for CursorRows<'_> {
    fn len(&self) -> usize {
        self.len
    }

    #[inline]
    fn row(&mut self, i: usize) -> Result<RowView<'_>> {
        if i.is_multiple_of(1024) {
            self.cursor.check()?;
        }
        // release the last row's owned fallbacks, keeping the slots' capacity
        self.terms.clear();
        self.charge.start_row();
        if let Some(d) = self.decoded {
            self.charge.cover(self.cursor, d.row_bytes(i)?)?;
        }
        for col in 0..self.batch.width() {
            if let Some(term) = self
                .charge
                .term(self.cursor, self.batch, self.decoded, i, col)?
            {
                self.terms.push((col, term));
            }
        }
        Ok(RowView::Sparse(&self.terms))
    }
}

impl SolutionSource for CursorSolutions<'_> {
    const FLUSH_FIRST: bool = true;

    fn variables(&self) -> &[String] {
        self.cursor.variables()
    }

    fn ctx(&self) -> Arc<Ctx> {
        self.cursor.output_ctx()
    }

    fn start(&mut self) -> Result<()> {
        self.cursor.check()
    }

    fn boolean(&self) -> Option<bool> {
        None
    }

    fn drive<V: BatchVisitor>(&mut self, send: Option<usize>, visitor: &mut V) -> Result<()> {
        let mut left = send.unwrap_or(usize::MAX);
        let mut row_charge = RowCharge::default();
        let remaining = self
            .cursor
            .for_each_decoded(send, |cursor, batch, decoded| {
                // the slots of one row's bound cells, held for the batch
                let row_storage = cursor.charge(
                    batch.width() as u64 * std::mem::size_of::<(usize, Cow<'_, Term>)>() as u64
                        + 128,
                )?;
                let len = batch.len().min(left);
                let mut rows = CursorRows {
                    terms: Vec::with_capacity(batch.width()),
                    _row_storage: row_storage,
                    cursor,
                    batch,
                    decoded,
                    charge: &mut row_charge,
                    len,
                };
                visitor.batch(&mut rows)?;
                left -= len;
                Ok(())
            })?;
        if remaining == 0 {
            self.cursor.close();
        }
        self.cursor.check()
    }

    fn exec_ms(&self) -> f64 {
        self.cursor.stats().timing.exec_ms
    }

    fn native_trailer(
        &mut self,
        out: &mut RowOut<'_>,
        sent: Option<u64>,
        _send: Option<usize>,
        metadata: Option<NativeJsonMetadata<'_>>,
        started: Instant,
        initial_exec: f64,
    ) -> Result<()> {
        let sent = sent.unwrap_or(0);
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Meta<'a> {
            total_rows: Option<u64>,
            sent_rows: u64,
            status: crate::sparql::CursorStatus,
            timing: &'a crate::sparql::Timing,
            plan: &'a crate::sparql::CursorPlan,
            memory: NativeMemory,
            rows_produced: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            commit: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            dataset_id: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            io: Option<crate::ioacct::IoReport>,
        }
        let cursor = &*self.cursor;
        let stats = cursor.stats();
        let mut timing = stats.timing.clone();
        timing.serialize_ms =
            (started.elapsed().as_secs_f64() * 1000.0 - (timing.exec_ms - initial_exec)).max(0.0);
        out.write_all(b",\"meta\":")?;
        crate::sparql::depth::with_stack(cursor.stack_depth(), || {
            json(
                &mut *out,
                &Meta {
                    total_rows: (stats.status == crate::sparql::CursorStatus::Complete)
                        .then_some(sent),
                    sent_rows: sent,
                    status: stats.status,
                    timing: &timing,
                    plan: cursor.plan(),
                    memory: NativeMemory {
                        peak_bytes: stats.mem_peak_bytes,
                        granted_bytes: stats.granted_bytes,
                    },
                    rows_produced: stats.rows_produced,
                    commit: metadata.as_ref().map(|m| m.commit),
                    dataset_id: metadata.as_ref().map(|m| m.dataset_id),
                    io: metadata.as_ref().and_then(|m| m.io).map(|p| p.report()),
                },
            )
        })?;
        out.write_all(b"}")?;
        Ok(())
    }

    fn stats(&self) -> Option<CursorStats> {
        Some(self.cursor.stats())
    }

    fn fail_output(&mut self, error: &Error) {
        self.cursor.fail_output(error);
    }
}
