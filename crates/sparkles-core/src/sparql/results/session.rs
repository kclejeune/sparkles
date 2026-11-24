//! Resumable cursor writers that can drain a query in place up to a byte threshold and
//! move across threads to a streaming producer if the body grows past it (item 534).

use super::encode::{Csv, Json, Tsv, Xml};
use super::rowout::{
    Explicit, FIRST_MARKS, LEDGER_MARK_BYTES, MARK_BYTES, OUTPUT_BUFFER, write_buffered_counted,
};
use super::source::{
    CursorCharge, CursorRows, RowBatch, RowView, write_graph_cursor_native_trailer,
    write_select_cursor_native_trailer,
};
use super::{
    Format, NativeJsonMetadata, NativeTerm, RowCharge, RowCount, RowLedger, RowOut,
    SolutionsFormat, io, json,
};
use crate::error::{Error, Result};
use crate::jena_formats::{JenaFormat, RdfWriter, ThriftResults};
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::cursor::{DecodedBatch, TermCache};
use crate::sparql::{CursorStats, GraphBatch, GraphCursor, QueryBatch, QueryCursor};
use oxrdf::Quad;
use parking_lot::Mutex;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Owned version of [`NativeJsonMetadata`] so a resumable cursor writer can move across
/// threads between batches.
#[derive(Clone)]
pub struct OwnedNativeJsonMetadata {
    pub commit: u64,
    pub dataset_id: String,
    pub io: Option<Arc<crate::ioacct::Profile>>,
}

impl OwnedNativeJsonMetadata {
    fn as_ref(&self) -> NativeJsonMetadata<'_> {
        NativeJsonMetadata {
            commit: self.commit,
            dataset_id: &self.dataset_id,
            io: self.io.as_deref(),
        }
    }
}

/// Owned options for a resumable [`CursorWriter`].
pub struct CursorWriteOptions {
    pub format: Format,
    pub send: Option<usize>,
    pub prefixes: Vec<(String, String)>,
    pub metadata: Option<OwnedNativeJsonMetadata>,
    pub ledger: Option<RowLedger>,
}

/// Result of writing a cursor up to a byte threshold in place.
pub enum WriteStep {
    /// The cursor finished (or reached `send`) and its complete body, including any
    /// footer or native trailer, was written.
    Done(CursorStats),
    /// Output crossed the byte threshold before completing. Move the writer to a
    /// background streaming task and call [`CursorWriter::finish`] to stream the rest.
    Paused,
}

/// Resumable writer for either a `SELECT` or a graph (`CONSTRUCT`/`DESCRIBE`) cursor.
pub enum CursorWriter<W: Write + Send + 'static> {
    Select(Box<SelectWriter>),
    Graph(Box<GraphWriter<W>>),
}

impl<W: Write + Send + 'static> CursorWriter<W> {
    pub fn select(cursor: QueryCursor, options: CursorWriteOptions) -> Self {
        Self::Select(Box::new(SelectWriter::new(cursor, options)))
    }

    pub fn graph(cursor: GraphCursor, options: CursorWriteOptions) -> Self {
        Self::Graph(Box::new(GraphWriter::new(cursor, options)))
    }

    /// Pull and serialize rows/statements into `writer` until the cursor ends or total
    /// output exceeds `limit_bytes`. Keeps `writer` inside the caller's `Option<W>` across
    /// normal returns and panic unwinds so the caller can always recover it.
    pub fn write_until(&mut self, writer: &mut Option<W>, limit_bytes: u64) -> Result<WriteStep> {
        match self {
            Self::Select(s) => {
                let w = writer.as_mut().expect("cursor writer present");
                s.write_until(w, limit_bytes)
            }
            Self::Graph(g) => g.write_until(writer, limit_bytes),
        }
    }

    /// Finish writing all remaining rows/statements and the format footer/trailer into
    /// `writer`, using full decode-ahead parallelism on `SELECT` cursors. Keeps `writer`
    /// inside the caller's `Option<W>` across normal returns and panic unwinds.
    pub fn finish(&mut self, writer: &mut Option<W>) -> Result<CursorStats> {
        match self {
            Self::Select(s) => {
                let w = writer.as_mut().expect("cursor writer present");
                s.finish(w)
            }
            Self::Graph(g) => g.finish(writer),
        }
    }

    /// Rows/statements whose complete bytes have been accepted by the caller's writer.
    pub fn rows_written(&self) -> u64 {
        match self {
            Self::Select(s) => s.rows_written(),
            Self::Graph(g) => g.rows_written(),
        }
    }

    /// Current cursor statistics.
    pub fn stats(&self) -> CursorStats {
        match self {
            Self::Select(s) => s.cursor.stats(),
            Self::Graph(g) => g.cursor.stats(),
        }
    }

    /// Serialized cursor plan for a failed Sparkles native JSON response.
    pub fn plan_value(&self) -> Option<serde_json::Value> {
        match self {
            Self::Select(s) => crate::sparql::depth::with_stack(s.cursor.stack_depth(), || {
                serde_json::to_value(s.cursor.plan()).ok()
            }),
            Self::Graph(g) => crate::sparql::depth::with_stack(g.cursor.stack_depth(), || {
                serde_json::to_value(g.cursor.plan()).ok()
            }),
        }
    }

    /// Mark the cursor failed when the transport fails outside the writer (for example,
    /// during hand-off channel setup before [`Self::finish`] runs).
    pub fn fail_output(&mut self, error: &Error) {
        match self {
            Self::Select(s) => s.cursor.fail_output(error),
            Self::Graph(g) => g.cursor.fail_output(error),
        }
    }
}

struct PendingSelectBatch {
    // Drop the term cache before the batch backing, and both before the cursor.
    cache: Option<TermCache>,
    batch: QueryBatch,
    next_row: usize,
    end_row: usize,
}

enum SelectState {
    Pending {
        format: Format,
        started: Instant,
        initial_exec: f64,
    },
    Tsv,
    Csv,
    Json(Json<'static>),
    Xml(Xml<'static>),
    Thrift,
    Native {
        sent: u64,
        started: Instant,
        initial_exec: f64,
    },
}

/// Resumable writer for a `SELECT` cursor.
pub struct SelectWriter {
    // Drop pending batch and owned encoder charges before the cursor.
    pending: Option<PendingSelectBatch>,
    state: SelectState,
    cursor: QueryCursor,
    send: Option<usize>,
    remaining: usize,
    metadata: Option<OwnedNativeJsonMetadata>,
    ledger: Option<RowLedger>,
    rows_written: u64,
    emitted_bytes: u64,
    header_written: bool,
    rows_done: bool,
    finished: bool,
}

impl SelectWriter {
    pub fn new(cursor: QueryCursor, options: CursorWriteOptions) -> Self {
        let initial_exec = cursor.stats().timing.exec_ms;
        Self {
            pending: None,
            state: SelectState::Pending {
                format: options.format,
                started: Instant::now(),
                initial_exec,
            },
            cursor,
            send: options.send,
            remaining: options.send.unwrap_or(usize::MAX),
            metadata: options.metadata,
            ledger: options.ledger,
            rows_written: 0,
            emitted_bytes: 0,
            header_written: false,
            rows_done: false,
            finished: false,
        }
    }

    fn ensure_initialized(&mut self) -> Result<()> {
        let SelectState::Pending {
            format,
            started,
            initial_exec,
        } = self.state
        else {
            return Ok(());
        };
        let ctx = self.cursor.output_ctx();
        self.state = match format {
            Format::Solutions(SolutionsFormat::Tsv) => SelectState::Tsv,
            Format::Solutions(SolutionsFormat::Csv) => SelectState::Csv,
            Format::Solutions(SolutionsFormat::Json) => {
                SelectState::Json(Json::new_owned(&ctx, self.cursor.variables())?)
            }
            Format::Solutions(SolutionsFormat::Xml) => {
                SelectState::Xml(Xml::new_owned(&ctx, self.cursor.variables())?)
            }
            Format::Solutions(SolutionsFormat::Sparkles) => SelectState::Native {
                sent: 0,
                started,
                initial_exec,
            },
            Format::ResultsThrift => SelectState::Thrift,
            Format::Rdf(_) | Format::Jena(_) => {
                return Err(Error::invalid(
                    "a SELECT cursor is written in a result format",
                ));
            }
        };
        Ok(())
    }

    pub fn rows_written(&self) -> u64 {
        self.rows_written
    }

    pub fn write_until(&mut self, writer: &mut dyn Write, limit_bytes: u64) -> Result<WriteStep> {
        if self.finished {
            return Ok(if self.emitted_bytes > limit_bytes {
                WriteStep::Paused
            } else {
                WriteStep::Done(self.cursor.stats())
            });
        }
        if let Err(error) = self.ensure_initialized() {
            self.cursor.fail_output(&error);
            return Err(error);
        }
        let ctx = self.cursor.output_ctx();
        let count = match &self.ledger {
            Some(l) => RowCount::with_ledger(l.clone()),
            None => RowCount::new(),
        };
        let mut paused = false;
        let result = write_buffered_counted(writer, &count, Some(ctx), &mut |out, _| {
            self.write_header_if_needed(out)?;
            if self
                .emitted_bytes
                .saturating_add(out.written_and_buffered())
                > limit_bytes
            {
                paused = true;
                return Ok(());
            }
            if !self.rows_done {
                let mut row_charge = RowCharge::default();
                // Finish any batch that paused on an earlier `write_until` call.
                if let Some(pending) = self.pending.take()
                    && self.write_pending_until(pending, out, &mut row_charge, limit_bytes)?
                {
                    paused = true;
                    return Ok(());
                }
                while self.remaining > 0 {
                    let Some(batch) = self.cursor.next_batch_at_most(self.remaining)? else {
                        break;
                    };
                    let cache = batch.term_cache()?;
                    let end_row = batch.len().min(self.remaining);
                    let pending = PendingSelectBatch {
                        cache,
                        batch,
                        next_row: 0,
                        end_row,
                    };
                    if self.write_pending_until(pending, out, &mut row_charge, limit_bytes)? {
                        paused = true;
                        return Ok(());
                    }
                }
                drop(row_charge);
                self.rows_done = true;
            }
            if self.remaining == 0 {
                self.cursor.close();
            }
            self.cursor.check()?;
            let current = self
                .emitted_bytes
                .saturating_add(out.written_and_buffered());
            if current.saturating_add(select_footer_min_bytes(&self.state)) > limit_bytes {
                paused = true;
                return Ok(());
            }
            self.write_footer(out)?;
            self.finished = true;
            Ok(())
        });
        self.rows_written += count.get();
        match result {
            Ok(taken) => {
                self.emitted_bytes = self.emitted_bytes.saturating_add(taken);
                if paused || self.emitted_bytes > limit_bytes {
                    Ok(WriteStep::Paused)
                } else {
                    Ok(WriteStep::Done(self.cursor.stats()))
                }
            }
            Err(error) => {
                self.cursor.fail_output(&error);
                Err(error)
            }
        }
    }

    pub fn finish(&mut self, writer: &mut dyn Write) -> Result<CursorStats> {
        if self.finished {
            return Ok(self.cursor.stats());
        }
        if let Err(error) = self.ensure_initialized() {
            self.cursor.fail_output(&error);
            return Err(error);
        }
        let ctx = self.cursor.output_ctx();
        let count = match &self.ledger {
            Some(l) => RowCount::with_ledger(l.clone()),
            None => RowCount::new(),
        };
        let result = write_buffered_counted(writer, &count, Some(ctx), &mut |out, _| {
            self.write_header_if_needed(out)?;
            if !self.rows_done {
                let mut row_charge = RowCharge::default();
                let width = self.cursor.variables().len();
                if let Some(PendingSelectBatch {
                    cache,
                    batch,
                    next_row,
                    end_row,
                }) = self.pending.take()
                {
                    let decoded = cache.map(|c| DecodedBatch::new(&batch, c));
                    let mut rows = CursorRows::new(
                        &self.cursor,
                        &batch,
                        decoded.as_ref(),
                        &mut row_charge,
                        end_row,
                    )?;
                    let mut explicit = Explicit::new(&mut *out);
                    for i in next_row..end_row {
                        let row = rows.row(i)?;
                        write_select_row(&mut self.state, &mut explicit, row, width)?;
                        self.remaining -= 1;
                    }
                    drop(rows);
                    drop(decoded);
                    drop(batch);
                }
                if self.remaining > 0 {
                    let send_left = self.send.map(|_| self.remaining);
                    let mut left = self.remaining;
                    let state = &mut self.state;
                    let remaining =
                        self.cursor
                            .for_each_decoded(send_left, |cursor, batch, decoded| {
                                let len = batch.len().min(left);
                                let mut rows =
                                    CursorRows::new(cursor, batch, decoded, &mut row_charge, len)?;
                                let mut explicit = Explicit::new(&mut *out);
                                for i in 0..len {
                                    let row = rows.row(i)?;
                                    write_select_row(state, &mut explicit, row, width)?;
                                }
                                left -= len;
                                Ok(())
                            })?;
                    self.remaining = remaining;
                }
                drop(row_charge);
                self.rows_done = true;
            }
            if self.remaining == 0 {
                self.cursor.close();
            }
            self.cursor.check()?;
            self.write_footer(out)?;
            self.finished = true;
            Ok(())
        });
        self.rows_written += count.get();
        match result {
            Ok(taken) => {
                self.emitted_bytes = self.emitted_bytes.saturating_add(taken);
                Ok(self.cursor.stats())
            }
            Err(error) => {
                self.cursor.fail_output(&error);
                Err(error)
            }
        }
    }

    fn write_header_if_needed(&mut self, out: &mut RowOut<'_>) -> Result<()> {
        if self.header_written {
            return Ok(());
        }
        self.cursor.check()?;
        let vars = self.cursor.variables();
        match &self.state {
            SelectState::Pending { .. } => unreachable!("select writer initialized"),
            SelectState::Tsv => {
                let mut explicit = Explicit::new(out);
                Tsv::header(&mut explicit, vars).map_err(io)?;
            }
            SelectState::Csv => {
                let mut explicit = Explicit::new(out);
                Csv::header(&mut explicit, vars).map_err(io)?;
            }
            SelectState::Json(_) => {
                let mut explicit = Explicit::new(out);
                Json::header(&mut explicit, vars).map_err(io)?;
            }
            SelectState::Xml(_) => {
                let mut explicit = Explicit::new(out);
                Xml::header(&mut explicit, vars).map_err(io)?;
            }
            SelectState::Thrift => {
                out.begin_piece();
                let mut thrift = ThriftResults::new(Explicit::new(out), vars).map_err(io)?;
                thrift.get_mut().end_piece().map_err(io)?;
            }
            SelectState::Native { .. } => {
                let mut explicit = Explicit::new(out);
                explicit.begin_piece();
                explicit.write_all(b"{\"queryType\":\"SELECT\",\"vars\":")?;
                json(&mut explicit, &vars)?;
                explicit.write_all(b",\"rows\":[")?;
                explicit.end_piece()?;
            }
        }
        self.header_written = true;
        Ok(())
    }

    /// Write rows of `pending` until it ends or `limit_bytes` is exceeded.
    /// Returns `Ok(true)` if paused (saving any remaining rows into `self.pending`).
    fn write_pending_until(
        &mut self,
        pending: PendingSelectBatch,
        out: &mut RowOut<'_>,
        row_charge: &mut RowCharge,
        limit_bytes: u64,
    ) -> Result<bool> {
        let PendingSelectBatch {
            cache,
            batch,
            next_row,
            end_row,
        } = pending;
        let width = self.cursor.variables().len();
        let decoded = cache.map(|c| DecodedBatch::new(&batch, c));
        let mut rows =
            CursorRows::new(&self.cursor, &batch, decoded.as_ref(), row_charge, end_row)?;
        let mut explicit = Explicit::new(&mut *out);
        for i in next_row..end_row {
            let row = rows.row(i)?;
            let current = self
                .emitted_bytes
                .saturating_add(explicit.written_and_buffered());
            if current.saturating_add(row_min_bytes(&row)) > limit_bytes {
                drop(rows);
                let cache = decoded.map(DecodedBatch::into_cache);
                self.pending = Some(PendingSelectBatch {
                    cache,
                    batch,
                    next_row: i,
                    end_row,
                });
                return Ok(true);
            }
            write_select_row(&mut self.state, &mut explicit, row, width)?;
            self.remaining -= 1;
            if self
                .emitted_bytes
                .saturating_add(explicit.written_and_buffered())
                > limit_bytes
            {
                drop(rows);
                let cache = decoded.map(DecodedBatch::into_cache);
                if i + 1 < end_row {
                    self.pending = Some(PendingSelectBatch {
                        cache,
                        batch,
                        next_row: i + 1,
                        end_row,
                    });
                }
                return Ok(true);
            }
        }
        drop(rows);
        drop(decoded);
        drop(batch);
        Ok(false)
    }

    fn write_footer(&mut self, out: &mut RowOut<'_>) -> Result<()> {
        match &self.state {
            SelectState::Pending { .. } => unreachable!("select writer initialized"),
            SelectState::Tsv | SelectState::Csv => Ok(()),
            SelectState::Json(_) => {
                let mut explicit = Explicit::new(out);
                Json::footer(&mut explicit).map_err(io)
            }
            SelectState::Xml(_) => {
                let mut explicit = Explicit::new(out);
                Xml::footer(&mut explicit).map_err(io)
            }
            SelectState::Thrift => {
                let thrift = ThriftResults::resume(Explicit::new(out));
                thrift.finish().map_err(io).map(drop)
            }
            SelectState::Native {
                sent,
                started,
                initial_exec,
            } => {
                out.write_all(b"]")?;
                write_select_cursor_native_trailer(
                    &self.cursor,
                    out,
                    *sent,
                    self.metadata.as_ref().map(OwnedNativeJsonMetadata::as_ref),
                    *started,
                    *initial_exec,
                )
            }
        }
    }
}

fn term_ref_min_bytes(term: oxrdf::TermRef<'_>) -> u64 {
    match term {
        oxrdf::TermRef::NamedNode(n) => n.as_str().len() as u64,
        oxrdf::TermRef::BlankNode(b) => b.as_str().len() as u64,
        oxrdf::TermRef::Literal(l) => l.value().len() as u64,
        oxrdf::TermRef::Triple(t) => term_ref_min_bytes(t.subject.as_ref().into())
            .saturating_add(term_ref_min_bytes(t.predicate.as_ref().into()))
            .saturating_add(term_ref_min_bytes(t.object.as_ref())),
    }
}

fn row_min_bytes(row: &RowView<'_>) -> u64 {
    match row {
        RowView::Dense(terms) => terms.iter().flatten().fold(0u64, |acc, t| {
            acc.saturating_add(term_ref_min_bytes(t.as_ref()))
        }),
        RowView::Sparse(terms) => terms.iter().fold(0u64, |acc, (_, t)| {
            acc.saturating_add(term_ref_min_bytes(t.as_ref().as_ref()))
        }),
    }
}

fn select_footer_min_bytes(state: &SelectState) -> u64 {
    match state {
        SelectState::Pending { .. } | SelectState::Tsv | SelectState::Csv | SelectState::Thrift => {
            0
        }
        SelectState::Json(_) => 3,
        SelectState::Xml(_) => 19,
        SelectState::Native { .. } => 10,
    }
}

fn write_select_row(
    state: &mut SelectState,
    out: &mut Explicit<'_, '_>,
    row: RowView<'_>,
    width: usize,
) -> Result<()> {
    out.begin_row()?;
    match state {
        SelectState::Pending { .. } => unreachable!("select writer initialized"),
        SelectState::Tsv => Tsv::row(out, row, width).map_err(io)?,
        SelectState::Csv => Csv::row(out, row, width).map_err(io)?,
        SelectState::Json(enc) => enc.row(out, row, width).map_err(io)?,
        SelectState::Xml(enc) => enc.row(out, row, width).map_err(io)?,
        SelectState::Thrift => {
            let mut thrift = ThriftResults::resume(&mut *out);
            match row {
                RowView::Dense(terms) => thrift.row(terms),
                RowView::Sparse(terms) => {
                    let mut bound = terms.iter().peekable();
                    thrift.row_cells(
                        width,
                        (0..width)
                            .map(|c| bound.next_if(|(col, _)| *col == c).map(|(_, t)| t.as_ref())),
                    )
                }
            }
            .map_err(io)?;
        }
        SelectState::Native { sent, .. } => {
            if *sent > 0 {
                out.write_all(b",")?;
            }
            out.write_all(b"[")?;
            match row {
                RowView::Dense(terms) => {
                    for (c, term) in terms.iter().enumerate() {
                        if c > 0 {
                            out.write_all(b",")?;
                        }
                        json(&mut *out, &term.as_ref().map(|t| NativeTerm(t.as_ref())))?;
                    }
                }
                RowView::Sparse(terms) => {
                    let mut bound = terms.iter().peekable();
                    for c in 0..width {
                        if c > 0 {
                            out.write_all(b",")?;
                        }
                        let term = bound
                            .next_if(|(col, _)| *col == c)
                            .map(|(_, t)| NativeTerm(t.as_ref().as_ref()));
                        json(&mut *out, &term)?;
                    }
                }
            }
            out.write_all(b"]")?;
            *sent += 1;
        }
    }
    out.end_row(1).map_err(io)
}

// ------------------------------------------------------------------ graph ------

type SinkSlot<W> = Arc<Mutex<Option<W>>>;

struct SlotGuard<'a, W> {
    slot: &'a SinkSlot<W>,
    out: &'a mut Option<W>,
}

impl<'a, W> SlotGuard<'a, W> {
    fn install(slot: &'a SinkSlot<W>, out: &'a mut Option<W>) -> Self {
        let w = out.take().expect("graph writer sink present");
        *slot.lock() = Some(w);
        Self { slot, out }
    }
}

impl<W> Drop for SlotGuard<'_, W> {
    fn drop(&mut self) {
        if let Some(w) = self.slot.lock().take() {
            *self.out = Some(w);
        }
    }
}

/// Inner state of [`GraphSink`]: drop settles only the ledger and never writes to the
/// external writer, so an inner-writer panic during unwind cannot double-panic/abort.
struct GraphSinkInner<W: Write + Send + 'static> {
    // Drop buffer and marks before their OwnedCharge.
    buf: Vec<u8>,
    marks: Vec<(usize, u64)>,
    charge: Option<OwnedCharge>,
    slot: SinkSlot<W>,
    ctx: Arc<Ctx>,
    staged: Arc<AtomicU64>,
    rows_taken: Arc<AtomicU64>,
    progress_bytes: Arc<AtomicU64>,
    refused: Arc<Mutex<Option<Error>>>,
    ledger: Option<RowLedger>,
    rows: u64,
    base: u64,
    taken: u64,
    failed: bool,
}

impl<W: Write + Send + 'static> Drop for GraphSinkInner<W> {
    fn drop(&mut self) {
        if let Some(ledger) = &self.ledger {
            ledger.settle();
        }
    }
}

/// Owned, `Send` staged-mode row buffer for graph serializers (`oxrdfio` and Jena) that
/// own their `Write` sink and move across threads on hand-off.
struct GraphSink<W: Write + Send + 'static>(Arc<Mutex<GraphSinkInner<W>>>);

impl<W: Write + Send + 'static> Clone for GraphSink<W> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<W: Write + Send + 'static> GraphSink<W> {
    fn new(
        slot: SinkSlot<W>,
        ctx: Arc<Ctx>,
        staged: Arc<AtomicU64>,
        rows_taken: Arc<AtomicU64>,
        progress_bytes: Arc<AtomicU64>,
        refused: Arc<Mutex<Option<Error>>>,
        ledger: Option<RowLedger>,
    ) -> Self {
        let base = ledger.as_ref().map_or(0, RowLedger::start);
        Self(Arc::new(Mutex::new(GraphSinkInner {
            buf: Vec::new(),
            marks: Vec::new(),
            charge: None,
            slot,
            ctx,
            staged,
            rows_taken,
            progress_bytes,
            refused,
            ledger,
            rows: 0,
            base,
            taken: 0,
            failed: false,
        })))
    }

    fn finish_drain(self) -> Result<()> {
        let mut inner = self.0.lock();
        let written = match inner.note() {
            Ok(()) => inner.drain(),
            Err(e) => {
                if !inner.failed {
                    let _ = inner.drain();
                }
                Err(e)
            }
        };
        written.map_err(|e| inner.refused.lock().take().unwrap_or(Error::Io(e)))
    }

    /// Explicit error-path drainage (called on normal `Err` return, never from `Drop`).
    fn drain_on_error(&self) {
        let mut inner = self.0.lock();
        if !inner.failed && inner.slot.lock().is_some() {
            let _ = inner.note();
            if !inner.buf.is_empty() || !inner.marks.is_empty() {
                let _ = inner.drain();
            }
        }
    }

    fn taken(&self) -> u64 {
        self.0.lock().taken
    }
}

impl<W: Write + Send + 'static> GraphSinkInner<W> {
    fn held(&self, buf: usize, marks: usize) -> u64 {
        let slot = if self.ledger.is_some() {
            MARK_BYTES + LEDGER_MARK_BYTES
        } else {
            MARK_BYTES
        };
        buf as u64 + marks as u64 * slot
    }

    fn cover(&mut self, bytes: u64) -> Result<()> {
        match &mut self.charge {
            Some(c) if c.bytes() >= bytes => Ok(()),
            Some(c) => c.resize(bytes),
            None => {
                self.charge = Some(OwnedCharge::new(&self.ctx, bytes)?);
                Ok(())
            }
        }
    }

    fn reserve_pending(&self, marks: usize) {
        if let Some(ledger) = &self.ledger {
            ledger.reserve_pending(marks);
        }
    }

    #[cold]
    fn refuse(&self, e: Error) -> std::io::Error {
        let io_err = std::io::Error::other(e.to_string());
        self.refused.lock().get_or_insert(e);
        io_err
    }

    #[cold]
    fn start(&mut self) -> std::io::Result<()> {
        let marks = self.marks.capacity().max(FIRST_MARKS);
        if let Err(e) = self.cover(self.held(OUTPUT_BUFFER, marks)) {
            return Err(self.refuse(e));
        }
        self.buf.reserve_exact(OUTPUT_BUFFER);
        self.marks.reserve_exact(marks - self.marks.len());
        self.reserve_pending(marks);
        Ok(())
    }

    #[cold]
    fn grow_marks(&mut self) -> std::io::Result<()> {
        let old = self.marks.capacity();
        let marks = (old * 2).max(FIRST_MARKS);
        let held = self.held(self.buf.capacity(), marks);
        let overlap = old as u64 * MARK_BYTES.max(LEDGER_MARK_BYTES);
        if let Err(e) = self.cover(held + overlap) {
            return Err(self.refuse(e));
        }
        self.marks.reserve_exact(marks - self.marks.len());
        self.reserve_pending(marks);
        if let Some(charge) = &mut self.charge
            && let Err(e) = charge.resize(held)
        {
            return Err(self.refuse(e));
        }
        Ok(())
    }

    fn push_mark(&mut self, n: u64) {
        let offset = self.buf.len();
        match self.marks.last_mut() {
            Some(last) if last.0 == offset => last.1 += n,
            _ => self.marks.push((offset, n)),
        }
    }

    #[inline]
    fn note(&mut self) -> std::io::Result<()> {
        let rows = self.staged.load(Ordering::Relaxed);
        if rows != self.rows {
            let n = rows - self.rows;
            self.rows = rows;
            if self.marks.capacity() == 0 {
                self.grow_marks()?;
            }
            self.push_mark(n);
            if self.marks.len() >= self.marks.capacity() {
                self.grow_marks()?;
            }
        }
        Ok(())
    }

    fn register(&self) {
        if let Some(ledger) = &self.ledger
            && !self.marks.is_empty()
        {
            let start = self.base + self.taken;
            ledger.register(
                start,
                self.marks
                    .iter()
                    .map(|&(offset, n)| (start + offset as u64, n)),
            );
        }
    }

    fn drain(&mut self) -> std::io::Result<()> {
        if self.failed {
            return Err(std::io::Error::other("the output already failed"));
        }
        self.register();
        let end = self.buf.len();
        let mut done = 0;
        let mut mark = 0;
        let mut guard = self.slot.lock();
        let inner = guard.as_mut().expect("sink writer installed during drain");
        loop {
            while let Some(&(at, n)) = self.marks.get(mark)
                && at <= done
            {
                self.rows_taken.fetch_add(n, Ordering::Relaxed);
                mark += 1;
            }
            if done == end {
                break;
            }
            match inner.write(&self.buf[done..end]) {
                Ok(0) => {
                    self.failed = true;
                    self.marks.clear();
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                Ok(n) => {
                    done += n;
                    self.taken += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.failed = true;
                    self.marks.clear();
                    return Err(e);
                }
            }
        }
        self.buf.clear();
        self.marks.clear();
        self.progress_bytes.store(self.taken, Ordering::Relaxed);
        Ok(())
    }

    fn write_slice(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.note()?;
        if b.len() <= self.buf.capacity() - self.buf.len() {
            self.buf.extend_from_slice(b);
            self.progress_bytes.store(
                self.taken.saturating_add(self.buf.len() as u64),
                Ordering::Relaxed,
            );
            return Ok(b.len());
        }
        if self.buf.capacity() == 0 {
            self.start()?;
            if !self.marks.is_empty() {
                self.drain()?;
            }
            if b.len() < OUTPUT_BUFFER {
                self.buf.extend_from_slice(b);
                self.progress_bytes.store(
                    self.taken.saturating_add(self.buf.len() as u64),
                    Ordering::Relaxed,
                );
                return Ok(b.len());
            }
        }
        self.drain()?;
        if b.len() >= OUTPUT_BUFFER {
            let mut guard = self.slot.lock();
            let inner = guard.as_mut().expect("sink writer installed during write");
            let r = inner.write_all(b);
            self.failed |= r.is_err();
            r?;
            self.taken += b.len() as u64;
            self.progress_bytes.store(self.taken, Ordering::Relaxed);
        } else {
            self.buf.extend_from_slice(b);
            self.progress_bytes.store(
                self.taken.saturating_add(self.buf.len() as u64),
                Ordering::Relaxed,
            );
        }
        Ok(b.len())
    }

    fn flush_inner(&mut self) -> std::io::Result<()> {
        self.note()?;
        self.drain()?;
        let mut guard = self.slot.lock();
        guard
            .as_mut()
            .expect("sink writer installed during flush")
            .flush()
    }
}

impl<W: Write + Send + 'static> Write for GraphSink<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().write_slice(b)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().flush_inner()
    }
}

struct PendingGraphBatch {
    batch: GraphBatch,
    next: usize,
}

enum GraphState<W: Write + Send + 'static> {
    Pending {
        format: Format,
        prefixes: Vec<(String, String)>,
    },
    Native {
        _charge: OwnedCharge,
        sent: u64,
        section: u64,
        header_written: bool,
    },
    Rdf {
        serializer: Option<oxrdfio::WriterQuadSerializer<GraphSink<W>>>,
        sink: GraphSink<W>,
        _charge: OwnedCharge,
        format: oxrdfio::RdfFormat,
        datasets: bool,
        slot: SinkSlot<W>,
        staged: Arc<AtomicU64>,
        rows_taken: Arc<AtomicU64>,
        progress_bytes: Arc<AtomicU64>,
        refused: Arc<Mutex<Option<Error>>>,
    },
    Jena {
        writer: Option<RdfWriter<GraphSink<W>>>,
        sink: GraphSink<W>,
        charge: CursorCharge,
        format: JenaFormat,
        deferred: bool,
        slot: SinkSlot<W>,
        staged: Arc<AtomicU64>,
        rows_taken: Arc<AtomicU64>,
        progress_bytes: Arc<AtomicU64>,
        refused: Arc<Mutex<Option<Error>>>,
    },
}

/// Resumable writer for a `CONSTRUCT` or `DESCRIBE` cursor.
pub struct GraphWriter<W: Write + Send + 'static> {
    // Drop pending batch and serializer/sink charges before the cursor.
    pending: Option<PendingGraphBatch>,
    state: GraphState<W>,
    cursor: GraphCursor,
    send: Option<usize>,
    sent: usize,
    metadata: Option<OwnedNativeJsonMetadata>,
    ledger: Option<RowLedger>,
    rows_written: u64,
    emitted_bytes: u64,
    rows_done: bool,
    finished: bool,
}

impl<W: Write + Send + 'static> GraphWriter<W> {
    pub fn new(cursor: GraphCursor, options: CursorWriteOptions) -> Self {
        Self {
            pending: None,
            state: GraphState::Pending {
                format: options.format,
                prefixes: options.prefixes,
            },
            cursor,
            send: options.send,
            sent: 0,
            metadata: options.metadata,
            ledger: options.ledger,
            rows_written: 0,
            emitted_bytes: 0,
            rows_done: false,
            finished: false,
        }
    }

    fn ensure_initialized(&mut self) -> Result<()> {
        let (format, prefixes) = match &mut self.state {
            GraphState::Pending { format, prefixes } => (*format, std::mem::take(prefixes)),
            _ => return Ok(()),
        };
        let ctx = self.cursor.output_ctx();
        self.state = match format {
            Format::Solutions(SolutionsFormat::Sparkles) => {
                self.cursor.check()?;
                let charge = self.cursor.charge(4096)?;
                GraphState::Native {
                    _charge: charge,
                    sent: 0,
                    section: 0,
                    header_written: false,
                }
            }
            Format::Rdf(format) => {
                let bytes = (128 << 10)
                    + prefixes
                        .iter()
                        .map(|(a, b)| (a.len() + b.len()) as u64 * 4 + 128)
                        .sum::<u64>();
                let charge = self.cursor.charge(bytes)?;
                let slot: SinkSlot<W> = Arc::new(Mutex::new(None));
                let staged = Arc::new(AtomicU64::new(0));
                let rows_taken = Arc::new(AtomicU64::new(0));
                let progress_bytes = Arc::new(AtomicU64::new(0));
                let refused: Arc<Mutex<Option<Error>>> = Arc::new(Mutex::new(None));
                let sink = GraphSink::new(
                    slot.clone(),
                    ctx,
                    staged.clone(),
                    rows_taken.clone(),
                    progress_bytes.clone(),
                    refused.clone(),
                    self.ledger.clone(),
                );
                let serializer =
                    crate::io::with_prefixes(oxrdfio::RdfSerializer::from_format(format), prefixes)
                        .for_writer(sink.clone());
                GraphState::Rdf {
                    serializer: Some(serializer),
                    sink,
                    _charge: charge,
                    format,
                    datasets: format.supports_datasets(),
                    slot,
                    staged,
                    rows_taken,
                    progress_bytes,
                    refused,
                }
            }
            Format::Jena(format) => {
                let bytes = 128 << 10;
                let charge = CursorCharge {
                    charge: self.cursor.charge(bytes)?,
                    bytes,
                };
                let slot: SinkSlot<W> = Arc::new(Mutex::new(None));
                let staged = Arc::new(AtomicU64::new(0));
                let rows_taken = Arc::new(AtomicU64::new(0));
                let progress_bytes = Arc::new(AtomicU64::new(0));
                let refused: Arc<Mutex<Option<Error>>> = Arc::new(Mutex::new(None));
                let sink = GraphSink::new(
                    slot.clone(),
                    ctx,
                    staged.clone(),
                    rows_taken.clone(),
                    progress_bytes.clone(),
                    refused.clone(),
                    self.ledger.clone(),
                );
                let writer = RdfWriter::new(format, sink.clone());
                GraphState::Jena {
                    writer: Some(writer),
                    sink,
                    charge,
                    format,
                    deferred: format == JenaFormat::RdfJson,
                    slot,
                    staged,
                    rows_taken,
                    progress_bytes,
                    refused,
                }
            }
            Format::Solutions(_) | Format::ResultsThrift => {
                return Err(Error::invalid(
                    "a graph cursor is written in an RDF syntax or the native document",
                ));
            }
        };
        Ok(())
    }

    pub fn rows_written(&self) -> u64 {
        match &self.state {
            GraphState::Pending { .. } | GraphState::Native { .. } => self.rows_written,
            GraphState::Rdf { rows_taken, .. } | GraphState::Jena { rows_taken, .. } => {
                rows_taken.load(Ordering::Relaxed)
            }
        }
    }

    pub fn write_until(&mut self, writer: &mut Option<W>, limit_bytes: u64) -> Result<WriteStep> {
        if self.finished {
            let final_bytes = self.total_emitted_bytes();
            return Ok(if final_bytes > limit_bytes {
                WriteStep::Paused
            } else {
                WriteStep::Done(self.cursor.stats())
            });
        }
        let depth = self.cursor.stack_depth();
        crate::sparql::depth::with_stack(depth, || self.drive(writer, Some(limit_bytes)))
    }

    pub fn finish(&mut self, writer: &mut Option<W>) -> Result<CursorStats> {
        if self.finished {
            return Ok(self.cursor.stats());
        }
        let depth = self.cursor.stack_depth();
        let res = crate::sparql::depth::with_stack(depth, || self.drive(writer, None));
        res.map(|step| match step {
            WriteStep::Done(stats) => stats,
            WriteStep::Paused => unreachable!("finish runs without a byte pause limit"),
        })
    }

    fn total_emitted_bytes(&self) -> u64 {
        match &self.state {
            GraphState::Pending { .. } | GraphState::Native { .. } => self.emitted_bytes,
            GraphState::Rdf { sink, .. } | GraphState::Jena { sink, .. } => sink.taken(),
        }
    }

    fn drive(&mut self, writer: &mut Option<W>, limit_bytes: Option<u64>) -> Result<WriteStep> {
        if let Err(error) = self.ensure_initialized() {
            self.cursor.fail_output(&error);
            return Err(error);
        }
        if matches!(self.state, GraphState::Native { .. }) {
            let w = writer.as_mut().expect("graph writer present");
            return self.drive_native(w, limit_bytes);
        }
        let slot = match &self.state {
            GraphState::Rdf { slot, .. } | GraphState::Jena { slot, .. } => slot.clone(),
            GraphState::Pending { .. } | GraphState::Native { .. } => unreachable!(),
        };
        let _guard = SlotGuard::install(&slot, writer);
        self.drive_sink(limit_bytes)
    }

    fn drive_native(
        &mut self,
        writer: &mut dyn Write,
        limit_bytes: Option<u64>,
    ) -> Result<WriteStep> {
        let ctx = self.cursor.output_ctx();
        let count = match &self.ledger {
            Some(l) => RowCount::with_ledger(l.clone()),
            None => RowCount::new(),
        };
        let mut paused = false;
        let result = write_buffered_counted(writer, &count, Some(ctx), &mut |out, staged| {
            let GraphState::Native {
                sent,
                section,
                header_written,
                ..
            } = &mut self.state
            else {
                unreachable!()
            };
            if !*header_written {
                out.write_all(b"{\"queryType\":")?;
                json(&mut *out, &self.cursor.kind())?;
                out.write_all(b",\"triples\":[],\"quads\":[")?;
                *header_written = true;
                if limit_bytes.is_some_and(|l| {
                    self.emitted_bytes
                        .saturating_add(out.written_and_buffered())
                        > l
                }) {
                    paused = true;
                    return Ok(());
                }
            }
            if !self.rows_done {
                if let Some(PendingGraphBatch { batch, next }) = self.pending.take() {
                    for (offset, quad) in batch.quads()[next..].iter().enumerate() {
                        if self.send.is_some_and(|max| self.sent == max) {
                            break;
                        }
                        if self.sent.is_multiple_of(1024) {
                            self.cursor.check()?;
                        }
                        let current = self
                            .emitted_bytes
                            .saturating_add(out.written_and_buffered());
                        if limit_bytes
                            .is_some_and(|l| current.saturating_add(quad_min_bytes(quad)) > l)
                        {
                            let next_idx = next + offset;
                            self.pending = Some(PendingGraphBatch {
                                batch,
                                next: next_idx,
                            });
                            paused = true;
                            return Ok(());
                        }
                        write_native_quad(out, quad, *section)?;
                        staged.set(staged.get() + 1);
                        *section += 1;
                        *sent += 1;
                        self.sent += 1;
                        if limit_bytes.is_some_and(|l| {
                            self.emitted_bytes
                                .saturating_add(out.written_and_buffered())
                                > l
                        }) {
                            let next_idx = next + offset + 1;
                            if next_idx < batch.len() && self.send.is_none_or(|max| self.sent < max)
                            {
                                self.pending = Some(PendingGraphBatch {
                                    batch,
                                    next: next_idx,
                                });
                            }
                            paused = true;
                            return Ok(());
                        }
                    }
                }
                while self.send.is_none_or(|max| self.sent < max) {
                    let Some(batch) = self.cursor.next_at_most(
                        self.send
                            .map_or(usize::MAX, |max| max.saturating_sub(self.sent)),
                    )?
                    else {
                        break;
                    };
                    for (i, quad) in batch.quads().iter().enumerate() {
                        if self.send.is_some_and(|max| self.sent == max) {
                            break;
                        }
                        if self.sent.is_multiple_of(1024) {
                            self.cursor.check()?;
                        }
                        let current = self
                            .emitted_bytes
                            .saturating_add(out.written_and_buffered());
                        if limit_bytes
                            .is_some_and(|l| current.saturating_add(quad_min_bytes(quad)) > l)
                        {
                            self.pending = Some(PendingGraphBatch { batch, next: i });
                            paused = true;
                            return Ok(());
                        }
                        write_native_quad(out, quad, *section)?;
                        staged.set(staged.get() + 1);
                        *section += 1;
                        *sent += 1;
                        self.sent += 1;
                        if limit_bytes.is_some_and(|l| {
                            self.emitted_bytes
                                .saturating_add(out.written_and_buffered())
                                > l
                        }) {
                            let next_idx = i + 1;
                            if next_idx < batch.len() && self.send.is_none_or(|max| self.sent < max)
                            {
                                self.pending = Some(PendingGraphBatch {
                                    batch,
                                    next: next_idx,
                                });
                            }
                            paused = true;
                            return Ok(());
                        }
                    }
                }
                self.rows_done = true;
            }
            if self.send.is_some_and(|max| self.sent >= max) {
                self.cursor.close();
            }
            let current = self
                .emitted_bytes
                .saturating_add(out.written_and_buffered());
            if limit_bytes.is_some_and(|l| current.saturating_add(10) > l) {
                paused = true;
                return Ok(());
            }
            out.write_all(b"]")?;
            write_graph_cursor_native_trailer(
                &self.cursor,
                out,
                *sent,
                self.metadata.as_ref().map(OwnedNativeJsonMetadata::as_ref),
            )?;
            self.finished = true;
            Ok(())
        });
        self.rows_written += count.get();
        match result {
            Ok(taken) => {
                self.emitted_bytes = self.emitted_bytes.saturating_add(taken);
                if paused || limit_bytes.is_some_and(|l| self.emitted_bytes > l) {
                    Ok(WriteStep::Paused)
                } else {
                    Ok(WriteStep::Done(self.cursor.stats()))
                }
            }
            Err(error) => {
                self.cursor.fail_output(&error);
                Err(error)
            }
        }
    }

    fn drive_sink(&mut self, limit_bytes: Option<u64>) -> Result<WriteStep> {
        let mut paused = false;
        let step_result = (|| -> Result<()> {
            if !self.rows_done {
                if let Some(PendingGraphBatch { batch, next }) = self.pending.take() {
                    for (offset, quad) in batch.quads()[next..].iter().enumerate() {
                        if self.send.is_some_and(|max| self.sent == max) {
                            break;
                        }
                        if self.sent.is_multiple_of(1024) {
                            self.cursor.check()?;
                        }
                        if limit_bytes.is_some_and(|l| {
                            sink_progress(&self.state)
                                .saturating_add(sink_quad_min_bytes(&self.state, quad))
                                > l
                        }) {
                            let next_idx = next + offset;
                            self.pending = Some(PendingGraphBatch {
                                batch,
                                next: next_idx,
                            });
                            paused = true;
                            return Ok(());
                        }
                        write_sink_quad(&mut self.state, quad)?;
                        self.sent += 1;
                        if limit_bytes.is_some_and(|l| sink_progress(&self.state) > l) {
                            let next_idx = next + offset + 1;
                            if next_idx < batch.len() && self.send.is_none_or(|max| self.sent < max)
                            {
                                self.pending = Some(PendingGraphBatch {
                                    batch,
                                    next: next_idx,
                                });
                            }
                            paused = true;
                            return Ok(());
                        }
                    }
                }
                while self.send.is_none_or(|max| self.sent < max) {
                    let Some(batch) = self.cursor.next_at_most(
                        self.send
                            .map_or(usize::MAX, |max| max.saturating_sub(self.sent)),
                    )?
                    else {
                        break;
                    };
                    for (i, quad) in batch.quads().iter().enumerate() {
                        if self.send.is_some_and(|max| self.sent == max) {
                            break;
                        }
                        if self.sent.is_multiple_of(1024) {
                            self.cursor.check()?;
                        }
                        if limit_bytes.is_some_and(|l| {
                            sink_progress(&self.state)
                                .saturating_add(sink_quad_min_bytes(&self.state, quad))
                                > l
                        }) {
                            self.pending = Some(PendingGraphBatch { batch, next: i });
                            paused = true;
                            return Ok(());
                        }
                        write_sink_quad(&mut self.state, quad)?;
                        self.sent += 1;
                        if limit_bytes.is_some_and(|l| sink_progress(&self.state) > l) {
                            let next_idx = i + 1;
                            if next_idx < batch.len() && self.send.is_none_or(|max| self.sent < max)
                            {
                                self.pending = Some(PendingGraphBatch {
                                    batch,
                                    next: next_idx,
                                });
                            }
                            paused = true;
                            return Ok(());
                        }
                    }
                }
                self.rows_done = true;
            }
            if self.send.is_some_and(|max| self.sent >= max) {
                self.cursor.close();
            }
            if limit_bytes.is_some_and(|l| {
                sink_progress(&self.state).saturating_add(sink_footer_min_bytes(&self.state)) > l
            }) {
                paused = true;
                return Ok(());
            }
            match &mut self.state {
                GraphState::Rdf { serializer, .. } => {
                    let sink = serializer
                        .take()
                        .expect("active RDF serializer")
                        .finish()
                        .map_err(io)?;
                    sink.finish_drain()?;
                }
                GraphState::Jena { writer, staged, .. } => {
                    let sink = writer
                        .take()
                        .expect("active Jena writer")
                        .finish_counted(|n| {
                            staged.fetch_add(n, Ordering::Relaxed);
                        })
                        .map_err(io)?;
                    sink.finish_drain()?;
                }
                GraphState::Pending { .. } | GraphState::Native { .. } => unreachable!(),
            }
            self.finished = true;
            Ok(())
        })();
        let refused = match &self.state {
            GraphState::Rdf { refused, .. } | GraphState::Jena { refused, .. } => {
                refused.lock().take()
            }
            GraphState::Pending { .. } | GraphState::Native { .. } => None,
        };
        let step_result = match (step_result, refused) {
            (Err(Error::Io(_)), Some(refused)) => Err(refused),
            (res, _) => res,
        };
        match step_result {
            Ok(()) => {
                let final_bytes = self.total_emitted_bytes();
                if paused || limit_bytes.is_some_and(|l| final_bytes > l) {
                    Ok(WriteStep::Paused)
                } else {
                    Ok(WriteStep::Done(self.cursor.stats()))
                }
            }
            Err(error) => {
                // Explicitly drain any buffered bytes before dropping the serializer/sink;
                // `GraphSinkInner::drop` only settles the ledger and never writes externally.
                match &mut self.state {
                    GraphState::Rdf {
                        serializer, sink, ..
                    } => {
                        sink.drain_on_error();
                        drop(serializer.take());
                    }
                    GraphState::Jena { writer, sink, .. } => {
                        sink.drain_on_error();
                        drop(writer.take());
                    }
                    GraphState::Pending { .. } | GraphState::Native { .. } => {}
                }
                self.cursor.fail_output(&error);
                Err(error)
            }
        }
    }
}

fn quad_min_bytes(quad: &Quad) -> u64 {
    let q = quad.as_ref();
    let base = term_ref_min_bytes(q.subject.into())
        .saturating_add(term_ref_min_bytes(q.predicate.into()))
        .saturating_add(term_ref_min_bytes(q.object));
    match q.graph_name {
        oxrdf::GraphNameRef::NamedNode(n) => base.saturating_add(n.as_str().len() as u64),
        oxrdf::GraphNameRef::BlankNode(b) => base.saturating_add(b.as_str().len() as u64),
        oxrdf::GraphNameRef::DefaultGraph => base,
    }
}

fn sink_quad_min_bytes<W: Write + Send + 'static>(state: &GraphState<W>, quad: &Quad) -> u64 {
    match state {
        GraphState::Rdf { datasets, .. } => {
            if *datasets || quad.graph_name.is_default_graph() {
                quad_min_bytes(quad)
            } else {
                0
            }
        }
        GraphState::Jena {
            format, deferred, ..
        } => {
            if !format.quads() && !quad.graph_name.is_default_graph() {
                return 0;
            }
            if *deferred {
                crate::sparql::graph_quad_bytes(quad)
                    .saturating_mul(4)
                    .saturating_add(1024)
            } else {
                quad_min_bytes(quad)
            }
        }
        GraphState::Pending { .. } | GraphState::Native { .. } => 0,
    }
}

fn sink_footer_min_bytes<W: Write + Send + 'static>(state: &GraphState<W>) -> u64 {
    match state {
        GraphState::Rdf { format, .. } => match format {
            oxrdfio::RdfFormat::RdfXml => 10,
            _ => 0,
        },
        GraphState::Jena { format, .. } => match format {
            JenaFormat::TriX => 7,
            JenaFormat::RdfJson => 2,
            _ => 0,
        },
        GraphState::Pending { .. } | GraphState::Native { .. } => 0,
    }
}

fn write_native_quad(out: &mut RowOut<'_>, quad: &Quad, section: u64) -> Result<()> {
    if section > 0 {
        out.write_all(b",")?;
    }
    let q = quad.as_ref();
    let graph = match q.graph_name {
        oxrdf::GraphNameRef::NamedNode(n) => Some(NativeTerm(n.into())),
        oxrdf::GraphNameRef::BlankNode(b) => Some(NativeTerm(b.into())),
        oxrdf::GraphNameRef::DefaultGraph => None,
    };
    json(
        &mut *out,
        &[
            Some(NativeTerm(q.subject.into())),
            Some(NativeTerm(q.predicate.into())),
            Some(NativeTerm(q.object)),
            graph,
        ],
    )
}

fn write_sink_quad<W: Write + Send + 'static>(
    state: &mut GraphState<W>,
    quad: &Quad,
) -> Result<()> {
    match state {
        GraphState::Rdf {
            serializer,
            datasets,
            staged,
            ..
        } => {
            if *datasets || quad.graph_name.is_default_graph() {
                serializer
                    .as_mut()
                    .expect("active RDF serializer")
                    .serialize_quad(quad)
                    .map_err(io)?;
                staged.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        }
        GraphState::Jena {
            writer,
            charge,
            format,
            deferred,
            staged,
            ..
        } => {
            if *format == JenaFormat::RdfJson {
                let kept = crate::sparql::graph_quad_bytes(quad);
                charge.bytes += kept.saturating_mul(4) + 1024;
                charge.charge.resize(charge.bytes)?;
            }
            let w = writer.as_mut().expect("active Jena writer");
            if format.quads() {
                w.quad(quad).map_err(io)?;
            } else if quad.graph_name.is_default_graph() {
                w.triple(&oxrdf::Triple::new(
                    quad.subject.clone(),
                    quad.predicate.clone(),
                    quad.object.clone(),
                ))
                .map_err(io)?;
            } else {
                return Ok(());
            }
            if !*deferred {
                staged.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        }
        GraphState::Pending { .. } | GraphState::Native { .. } => unreachable!(),
    }
}

fn sink_progress<W: Write + Send + 'static>(state: &GraphState<W>) -> u64 {
    match state {
        GraphState::Rdf { progress_bytes, .. } => progress_bytes.load(Ordering::Relaxed),
        GraphState::Jena {
            deferred,
            charge,
            progress_bytes,
            ..
        } => {
            if *deferred {
                charge.bytes
            } else {
                progress_bytes.load(Ordering::Relaxed)
            }
        }
        GraphState::Pending { .. } | GraphState::Native { .. } => 0,
    }
}

#[allow(dead_code)]
fn _assert_cursor_writer_is_send<W: Write + Send + 'static>() {
    fn is_send<T: Send>() {}
    is_send::<SelectWriter>();
    is_send::<GraphWriter<W>>();
    is_send::<CursorWriter<W>>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::Source as LoadSource;
    use crate::sparql::results::{Source, WriteOptions, write};
    use crate::sparql::{
        CursorOptions, ExecutionMode, QueryExecution, QueryOptions, query_execution,
    };
    use crate::store::Store;
    use oxrdfio::RdfFormat;

    fn sample_store() -> Store {
        let store = Store::in_memory(Default::default());
        let mut ttl = String::from("@prefix ex: <urn:ex:> .\n");
        for i in 0..80 {
            ttl.push_str(&format!(
                "ex:s{i} ex:p \"val-{i}-\\\"<&>\" ; ex:num {i} .\n"
            ));
        }
        ttl.push_str("ex:g { ex:s0 ex:p \"in-graph\" . }\n");
        store
            .load(&[LoadSource::from_bytes(
                ttl.into_bytes(),
                RdfFormat::TriG,
                None,
            )])
            .unwrap();
        store
    }

    fn scrub_native(bytes: Vec<u8>) -> Vec<u8> {
        let text = String::from_utf8(bytes).unwrap();
        let at = text.rfind(",\"meta\":").unwrap();
        let (head, _) = text.split_at(at);
        head.as_bytes().to_vec()
    }

    struct LedgerVec {
        buf: Vec<u8>,
        ledger: RowLedger,
    }

    impl LedgerVec {
        fn new(ledger: RowLedger) -> Self {
            Self {
                buf: Vec::new(),
                ledger,
            }
        }
    }

    impl Write for LedgerVec {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.buf.extend_from_slice(b);
            self.ledger.copied(self.buf.len() as u64);
            Ok(b.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn select_writer_matches_direct_write_with_and_without_handoff() {
        let store = sample_store();
        let query = "SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY ?s ?p ?o";
        let formats = [
            Format::Solutions(SolutionsFormat::Json),
            Format::Solutions(SolutionsFormat::Xml),
            Format::Solutions(SolutionsFormat::Csv),
            Format::Solutions(SolutionsFormat::Tsv),
            Format::Solutions(SolutionsFormat::Sparkles),
            Format::ResultsThrift,
        ];
        for fmt in formats {
            for send in [None, Some(0), Some(15)] {
                let mut baseline = Vec::new();
                let mut base_cursor = match query_execution(
                    store.snapshot(),
                    query,
                    &QueryOptions::default(),
                    &CursorOptions {
                        batch_rows: 17,
                        ..Default::default()
                    },
                    ExecutionMode::Streaming,
                )
                .unwrap()
                {
                    QueryExecution::Select(c) => *c,
                    _ => unreachable!(),
                };
                let base_rows = RowCount::new();
                write(
                    Source::Select(&mut base_cursor),
                    WriteOptions {
                        send,
                        ..WriteOptions::new(fmt, &base_rows)
                    },
                    &mut baseline,
                )
                .unwrap();

                // Test whole in-place (large limit) and mid-stream pause + cross-thread hand-off (tiny limits).
                for limit in [u64::MAX, 1, 64, 512, 4096] {
                    let cursor = match query_execution(
                        store.snapshot(),
                        query,
                        &QueryOptions::default(),
                        &CursorOptions {
                            batch_rows: 17,
                            ..Default::default()
                        },
                        ExecutionMode::Streaming,
                    )
                    .unwrap()
                    {
                        QueryExecution::Select(c) => *c,
                        _ => unreachable!(),
                    };
                    let ledger = RowLedger::new();
                    let mut writer = CursorWriter::select(
                        cursor,
                        CursorWriteOptions {
                            format: fmt,
                            send,
                            prefixes: Vec::new(),
                            metadata: None,
                            ledger: Some(ledger.clone()),
                        },
                    );
                    let mut out_slot = Some(LedgerVec::new(ledger.clone()));
                    let step = writer.write_until(&mut out_slot, limit);
                    let mut out = out_slot.expect("writer returned");
                    let stats = match step.unwrap() {
                        WriteStep::Done(stats) => {
                            assert!(
                                out.buf.len() as u64 <= limit,
                                "Done exceeded limit: {} > {limit}",
                                out.buf.len()
                            );
                            ledger.handed_over(out.buf.len() as u64);
                            stats
                        }
                        WriteStep::Paused => {
                            ledger.handed_over(out.buf.len() as u64);
                            let ledger_bg = ledger.clone();
                            let (finished_out, stats) = std::thread::spawn(move || {
                                let mut slot = Some(out);
                                let stats = writer.finish(&mut slot).unwrap();
                                let out = slot.expect("writer returned");
                                ledger_bg.handed_over(out.buf.len() as u64);
                                (out, stats)
                            })
                            .join()
                            .unwrap();
                            out = finished_out;
                            stats
                        }
                    };
                    if fmt == Format::Solutions(SolutionsFormat::Sparkles) {
                        assert_eq!(scrub_native(out.buf), scrub_native(baseline.clone()));
                    } else {
                        assert_eq!(
                            out.buf, baseline,
                            "format={fmt:?} send={send:?} limit={limit}"
                        );
                    }
                    assert_eq!(
                        ledger.delivered(),
                        Some(base_rows.get()),
                        "ledger rows format={fmt:?} send={send:?} limit={limit}"
                    );
                    assert_eq!(stats.emitted_rows, base_cursor.stats().emitted_rows);
                }
            }
        }
    }

    #[test]
    fn graph_writer_matches_direct_write_with_and_without_handoff() {
        let store = sample_store();
        let query = "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o } ORDER BY ?s ?p ?o";
        let formats = [
            Format::Solutions(SolutionsFormat::Sparkles),
            Format::Rdf(RdfFormat::Turtle),
            Format::Rdf(RdfFormat::NTriples),
            Format::Rdf(RdfFormat::NQuads),
            Format::Rdf(RdfFormat::TriG),
            Format::Rdf(RdfFormat::RdfXml),
            Format::Jena(JenaFormat::Thrift),
            Format::Jena(JenaFormat::Protobuf),
            Format::Jena(JenaFormat::RdfJson),
            Format::Jena(JenaFormat::TriX),
        ];
        let prefixes = vec![("ex".into(), "urn:ex:".into())];
        for fmt in formats {
            for send in [None, Some(10)] {
                let mut baseline = Vec::new();
                let mut base_cursor = match query_execution(
                    store.snapshot(),
                    query,
                    &QueryOptions::default(),
                    &CursorOptions {
                        batch_rows: 13,
                        ..Default::default()
                    },
                    ExecutionMode::Streaming,
                )
                .unwrap()
                {
                    QueryExecution::Graph(c) => *c,
                    _ => unreachable!(),
                };
                let base_rows = RowCount::new();
                write(
                    Source::Graph(&mut base_cursor),
                    WriteOptions {
                        send,
                        prefixes: &prefixes,
                        ..WriteOptions::new(fmt, &base_rows)
                    },
                    &mut baseline,
                )
                .unwrap();

                for limit in [u64::MAX, 1, 256, 2048] {
                    let cursor = match query_execution(
                        store.snapshot(),
                        query,
                        &QueryOptions::default(),
                        &CursorOptions {
                            batch_rows: 13,
                            ..Default::default()
                        },
                        ExecutionMode::Streaming,
                    )
                    .unwrap()
                    {
                        QueryExecution::Graph(c) => *c,
                        _ => unreachable!(),
                    };
                    let ledger = RowLedger::new();
                    let mut writer = CursorWriter::graph(
                        cursor,
                        CursorWriteOptions {
                            format: fmt,
                            send,
                            prefixes: prefixes.clone(),
                            metadata: None,
                            ledger: Some(ledger.clone()),
                        },
                    );
                    let mut out_slot = Some(LedgerVec::new(ledger.clone()));
                    let step = writer.write_until(&mut out_slot, limit);
                    let mut out = out_slot.expect("writer returned");
                    match step.unwrap() {
                        WriteStep::Done(_) => {
                            assert!(
                                out.buf.len() as u64 <= limit,
                                "graph Done exceeded limit: {} > {limit}",
                                out.buf.len()
                            );
                            ledger.handed_over(out.buf.len() as u64);
                        }
                        WriteStep::Paused => {
                            ledger.handed_over(out.buf.len() as u64);
                            let ledger_bg = ledger.clone();
                            out = std::thread::spawn(move || {
                                let mut slot = Some(out);
                                writer.finish(&mut slot).unwrap();
                                let out = slot.expect("writer returned");
                                ledger_bg.handed_over(out.buf.len() as u64);
                                out
                            })
                            .join()
                            .unwrap();
                        }
                    }
                    if fmt == Format::Solutions(SolutionsFormat::Sparkles) {
                        assert_eq!(scrub_native(out.buf), scrub_native(baseline.clone()));
                    } else {
                        assert_eq!(
                            out.buf, baseline,
                            "format={fmt:?} send={send:?} limit={limit}"
                        );
                    }
                    assert_eq!(
                        ledger.delivered(),
                        Some(base_rows.get()),
                        "ledger rows format={fmt:?} send={send:?} limit={limit}"
                    );
                }
            }
        }
    }

    #[test]
    fn graph_sink_panicking_writer_unwinds_without_double_panic() {
        struct PanicWriter {
            calls: Arc<AtomicU64>,
        }
        impl Write for PanicWriter {
            fn write(&mut self, _b: &[u8]) -> std::io::Result<usize> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                panic!("always-panicking writer");
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let store = Store::in_memory(Default::default());
        store
            .load(&[LoadSource::from_bytes(
                b"<urn:s:1> <urn:p:1> \"small\" .\n".to_vec(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let cursor = match query_execution(
            store.snapshot(),
            "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }",
            &QueryOptions::default(),
            &CursorOptions::default(),
            ExecutionMode::Streaming,
        )
        .unwrap()
        {
            QueryExecution::Graph(c) => *c,
            _ => unreachable!(),
        };
        let calls = Arc::new(AtomicU64::new(0));
        let mut writer = CursorWriter::graph(
            cursor,
            CursorWriteOptions {
                format: Format::Rdf(RdfFormat::NTriples),
                send: None,
                prefixes: Vec::new(),
                metadata: None,
                ledger: Some(RowLedger::new()),
            },
        );
        let mut slot = Some(PanicWriter {
            calls: calls.clone(),
        });
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = writer.finish(&mut slot);
        }));
        assert!(caught.is_err(), "panic should unwind cleanly");
        assert!(
            slot.is_some(),
            "SlotGuard must restore writer into Option<W> on unwind"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "GraphSink::drop must not invoke external writes again during unwind"
        );
    }

    #[test]
    fn single_oversized_row_and_quad_pause_before_copying_in_place() {
        let store = Store::in_memory(Default::default());
        let big = "x".repeat(32_768);
        store
            .load(&[LoadSource::from_bytes(
                format!("<urn:s:big> <urn:p:big> \"{big}\" .\n").into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();

        // SELECT single oversized row pauses before copying the 32 KiB value in place.
        let select_q = "SELECT ?s ?o WHERE { ?s <urn:p:big> ?o }";
        let select_cursor = match query_execution(
            store.snapshot(),
            select_q,
            &QueryOptions::default(),
            &CursorOptions::default(),
            ExecutionMode::Streaming,
        )
        .unwrap()
        {
            QueryExecution::Select(c) => *c,
            _ => unreachable!(),
        };
        let mut sw = CursorWriter::select(
            select_cursor,
            CursorWriteOptions {
                format: Format::Solutions(SolutionsFormat::Json),
                send: None,
                prefixes: Vec::new(),
                metadata: None,
                ledger: None,
            },
        );
        let mut slot = Some(Vec::<u8>::new());
        let step = sw.write_until(&mut slot, 1024).unwrap();
        assert!(matches!(step, WriteStep::Paused));
        assert!(
            slot.as_ref().unwrap().len() <= 1024,
            "oversized row must not be copied in place: {} bytes",
            slot.as_ref().unwrap().len()
        );
        sw.finish(&mut slot).unwrap();
        let out = slot.unwrap();
        assert!(out.len() > 32_768);
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["results"]["bindings"][0]["o"]["value"], big);

        // Graph single oversized quad pauses before copying the 32 KiB value in place.
        for fmt in [
            Format::Rdf(RdfFormat::NTriples),
            Format::Solutions(SolutionsFormat::Sparkles),
            Format::Jena(JenaFormat::RdfJson),
        ] {
            let graph_cursor = match query_execution(
                store.snapshot(),
                "CONSTRUCT { ?s <urn:p:big> ?o } WHERE { ?s <urn:p:big> ?o }",
                &QueryOptions::default(),
                &CursorOptions::default(),
                ExecutionMode::Streaming,
            )
            .unwrap()
            {
                QueryExecution::Graph(c) => *c,
                _ => unreachable!(),
            };
            let mut gw = CursorWriter::graph(
                graph_cursor,
                CursorWriteOptions {
                    format: fmt,
                    send: None,
                    prefixes: Vec::new(),
                    metadata: None,
                    ledger: None,
                },
            );
            let mut slot = Some(Vec::<u8>::new());
            let step = gw.write_until(&mut slot, 1024).unwrap();
            assert!(matches!(step, WriteStep::Paused), "graph {fmt:?}");
            assert!(
                slot.as_ref().unwrap().len() <= 1024,
                "graph {fmt:?} oversized quad copied {} bytes in place",
                slot.as_ref().unwrap().len()
            );
            gw.finish(&mut slot).unwrap();
            assert!(slot.unwrap().len() > 32_768, "graph {fmt:?}");
        }
    }

    #[test]
    fn final_footer_crossing_threshold_pauses_select_and_graph() {
        let store = sample_store();

        // SELECT JSON: set limit to (full_len - 2) so header + rows fit, and only the
        // 3-byte `]}` footer crosses the limit.
        let select_q = "SELECT ?s ?o WHERE { ?s <urn:ex:num> ?o } LIMIT 3";
        let mut full_json = Vec::new();
        let mut base = match query_execution(
            store.snapshot(),
            select_q,
            &QueryOptions::default(),
            &CursorOptions::default(),
            ExecutionMode::Streaming,
        )
        .unwrap()
        {
            QueryExecution::Select(c) => *c,
            _ => unreachable!(),
        };
        write(
            Source::Select(&mut base),
            WriteOptions::new(Format::Solutions(SolutionsFormat::Json), &RowCount::new()),
            &mut full_json,
        )
        .unwrap();
        let limit = (full_json.len() - 2) as u64;
        let cursor = match query_execution(
            store.snapshot(),
            select_q,
            &QueryOptions::default(),
            &CursorOptions::default(),
            ExecutionMode::Streaming,
        )
        .unwrap()
        {
            QueryExecution::Select(c) => *c,
            _ => unreachable!(),
        };
        let mut writer = CursorWriter::select(
            cursor,
            CursorWriteOptions {
                format: Format::Solutions(SolutionsFormat::Json),
                send: None,
                prefixes: Vec::new(),
                metadata: None,
                ledger: None,
            },
        );
        let mut slot = Some(Vec::<u8>::new());
        let step = writer.write_until(&mut slot, limit).unwrap();
        assert!(
            matches!(step, WriteStep::Paused),
            "JSON footer crossing limit must return Paused, not Done"
        );
        writer.finish(&mut slot).unwrap();
        assert_eq!(slot.unwrap(), full_json);

        // Graph RdfXml: set limit to (full_len - 5) so closing `</rdf:RDF>` crosses limit.
        let graph_q = "CONSTRUCT { ?s <urn:ex:num> ?o } WHERE { ?s <urn:ex:num> ?o } LIMIT 3";
        let mut full_xml = Vec::new();
        let mut base_g = match query_execution(
            store.snapshot(),
            graph_q,
            &QueryOptions::default(),
            &CursorOptions::default(),
            ExecutionMode::Streaming,
        )
        .unwrap()
        {
            QueryExecution::Graph(c) => *c,
            _ => unreachable!(),
        };
        write(
            Source::Graph(&mut base_g),
            WriteOptions::new(Format::Rdf(RdfFormat::RdfXml), &RowCount::new()),
            &mut full_xml,
        )
        .unwrap();
        let g_limit = (full_xml.len() - 5) as u64;
        let g_cursor = match query_execution(
            store.snapshot(),
            graph_q,
            &QueryOptions::default(),
            &CursorOptions::default(),
            ExecutionMode::Streaming,
        )
        .unwrap()
        {
            QueryExecution::Graph(c) => *c,
            _ => unreachable!(),
        };
        let mut gw = CursorWriter::graph(
            g_cursor,
            CursorWriteOptions {
                format: Format::Rdf(RdfFormat::RdfXml),
                send: None,
                prefixes: Vec::new(),
                metadata: None,
                ledger: None,
            },
        );
        let mut slot = Some(Vec::<u8>::new());
        let step = gw.write_until(&mut slot, g_limit).unwrap();
        assert!(
            matches!(step, WriteStep::Paused),
            "RdfXml closing tag crossing limit must return Paused"
        );
        gw.finish(&mut slot).unwrap();
        assert_eq!(slot.unwrap(), full_xml);
    }

    #[test]
    fn lazy_init_failure_preserves_cursor_stats_and_plan() {
        let store = sample_store();
        let cursor = match query_execution(
            store.snapshot(),
            "SELECT ?s WHERE { ?s <urn:ex:num> ?o } LIMIT 1",
            &QueryOptions::default(),
            &CursorOptions::default(),
            ExecutionMode::Streaming,
        )
        .unwrap()
        {
            QueryExecution::Select(c) => *c,
            _ => unreachable!(),
        };
        let mut sw = CursorWriter::select(
            cursor,
            CursorWriteOptions {
                format: Format::Rdf(RdfFormat::NTriples),
                send: None,
                prefixes: Vec::new(),
                metadata: None,
                ledger: None,
            },
        );
        let mut slot = Some(Vec::<u8>::new());
        let err = sw.write_until(&mut slot, 4096).err().unwrap();
        assert!(matches!(err, Error::Invalid(_)));
        assert_eq!(sw.stats().status, crate::sparql::CursorStatus::Failed);
        assert!(sw.plan_value().is_some());
    }
}
