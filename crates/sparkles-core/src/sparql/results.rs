//! Result serialization (ARQ `ResultSetFormatter` / RIOT writers).

use super::{QueryKind, QueryResult};
use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::{Id, Tag};
use oxrdf::{Term, Variable};
use oxrdfio::{RdfFormat, RdfSerializer};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::{Value as J, json};
use sparesults::{QueryResultsFormat, QueryResultsSerializer};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolutionsFormat {
    Json,
    Xml,
    Csv,
    Tsv,
    /// `application/x-sparkles+json` (rich format for the UI)
    Sparkles,
}

impl SolutionsFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            SolutionsFormat::Json => "application/sparql-results+json",
            SolutionsFormat::Xml => "application/sparql-results+xml",
            SolutionsFormat::Csv => "text/csv; charset=utf-8",
            SolutionsFormat::Tsv => "text/tab-separated-values; charset=utf-8",
            SolutionsFormat::Sparkles => "application/x-sparkles+json",
        }
    }
    /// A lower bound of the serialized size of `rows` solutions of `vars` variables (any
    /// of which may be unbound), for refusing a response over its size budget before
    /// serializing it.
    pub fn min_bytes(self, rows: usize, vars: usize) -> u64 {
        let per_row = match self {
            // separators and the line end
            SolutionsFormat::Tsv => vars.max(1),
            SolutionsFormat::Csv => vars + 1,
            // `{}`
            SolutionsFormat::Json => 2,
            // `<result/>`
            SolutionsFormat::Xml => 9,
            // `[null,…]`
            SolutionsFormat::Sparkles => 5 * vars + 1,
        };
        (rows as u64).saturating_mul(per_row as u64)
    }

    /// Parse a media type or Fuseki `format=` short name.
    pub fn from_name(s: &str) -> Option<SolutionsFormat> {
        let base = s.split(';').next()?.trim().to_ascii_lowercase();
        Some(match base.as_str() {
            "application/sparql-results+json" | "application/json" | "json" => {
                SolutionsFormat::Json
            }
            "application/sparql-results+xml" | "application/xml" | "text/xml" | "xml" => {
                SolutionsFormat::Xml
            }
            "text/csv" | "csv" => SolutionsFormat::Csv,
            "text/tab-separated-values" | "tsv" => SolutionsFormat::Tsv,
            "application/x-sparkles+json" | "sparkles" => SolutionsFormat::Sparkles,
            _ => return None,
        })
    }
}

/// The RDF syntax a result is written in, by a name or a media type of the syntax
/// registry ([`crate::syntaxes`]). The generic JSON and XML media types, which also name
/// result formats, and N3, which no writer writes, are not among them. JSON-LD's media
/// type keeps its profile.
pub fn rdf_format_from_name(s: &str) -> Option<RdfFormat> {
    let base = s.split(';').next()?.trim().to_ascii_lowercase();
    match base.as_str() {
        "application/ld+json" => return crate::io::format_for_media_type(s),
        "application/json" | "application/xml" | "text/xml" => return None,
        _ => {}
    }
    crate::syntaxes::by_name(&base)
        .or_else(|| crate::syntaxes::by_media_type(&base))
        .filter(|f| *f != RdfFormat::N3)
}

pub fn rdf_media_type(f: RdfFormat) -> &'static str {
    match f {
        RdfFormat::Turtle => "text/turtle; charset=utf-8",
        RdfFormat::NTriples => "application/n-triples",
        RdfFormat::NQuads => "application/n-quads",
        RdfFormat::TriG => "application/trig",
        RdfFormat::RdfXml => "application/rdf+xml",
        RdfFormat::JsonLd { .. } => "application/ld+json",
        _ => "text/plain",
    }
}

fn io(e: std::io::Error) -> Error {
    Error::Io(e)
}

/// `io::Write` adapter for the result-size budget: the first write that would take the
/// output past `limit` bytes fails, and the cancellation flag is checked every 64 KiB.
/// [`LimitedWriter::classify`] turns the resulting I/O error back into the reason.
pub struct LimitedWriter<W> {
    inner: W,
    limit: Option<u64>,
    written: u64,
    cancel: Option<Arc<AtomicBool>>,
    next_check: u64,
    /// size the refused write would have reached
    exceeded: Option<u64>,
    cancelled: bool,
}

const CANCEL_CHECK_BYTES: u64 = 64 << 10;

impl<W: Write> LimitedWriter<W> {
    pub fn new(inner: W, limit: Option<u64>, cancel: Option<Arc<AtomicBool>>) -> Self {
        LimitedWriter {
            inner,
            limit,
            written: 0,
            cancel,
            next_check: CANCEL_CHECK_BYTES,
            exceeded: None,
            cancelled: false,
        }
    }

    /// Bytes written so far.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Inspect the underlying writer without consuming it.
    pub fn get_ref(&self) -> &W {
        &self.inner
    }

    /// An error of a serializer writing through this adapter: caused by the limit ->
    /// [`Error::BudgetExceeded`] (result bytes); by cancellation -> [`Error::Cancelled`];
    /// otherwise unchanged.
    pub fn classify(&self, e: Error) -> Error {
        if !matches!(&e, Error::Io(_)) {
            return e;
        }
        if let Some(requested) = self.exceeded {
            Error::BudgetExceeded(Budget {
                kind: BudgetKind::ResultBytes,
                limit: self.limit.unwrap_or(u64::MAX),
                requested,
            })
        } else if self.cancelled {
            Error::Cancelled
        } else {
            e
        }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> LimitedWriter<W> {
    /// Admit `len` more bytes: the size limit, and the cancellation flag every 64 KiB.
    #[inline]
    fn admit(&mut self, len: usize) -> std::io::Result<()> {
        let end = self.written.saturating_add(len as u64);
        if let Some(l) = self.limit
            && end > l
        {
            self.exceeded = Some(end);
            return Err(std::io::Error::other("result size budget exceeded"));
        }
        if end >= self.next_check {
            self.next_check = end.saturating_add(CANCEL_CHECK_BYTES);
            if self
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                self.cancelled = true;
                return Err(std::io::Error::other("query cancelled"));
            }
        }
        Ok(())
    }
}

impl<W: Write> Write for LimitedWriter<W> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.admit(buf.len())?;
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    // Admit the whole piece before writing, but count successful partial writes
    // even if a later channel wait fails. A Vec still consumes it in one write.
    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.admit(buf.len())?;
        let mut remaining = buf;
        while !remaining.is_empty() {
            match self.inner.write(remaining) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.written += n as u64;
                    remaining = &remaining[n..];
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// The bytes [`Buffered`] collects before it writes them on. Serializers write a few
/// bytes at a time, and each write that went straight to the response writer passed its
/// size limit, cancellation check and channel buffer.
const OUTPUT_BUFFER: usize = 8 << 10;

/// The writer that the eager result serializers write to: their output in pieces of
/// [`OUTPUT_BUFFER`] bytes on the way to the caller's writer. Its type is the engine's,
/// so the serializers are compiled with the engine and not once per caller's writer in
/// the crate that calls them, where their speed moved with unrelated changes.
///
/// A serializer counts its rows in `staged` after writing each of them. A row counts in
/// the caller's `sent` only once its bytes have reached the caller's writer, so `sent`
/// still holds the rows of the output when that writer stops it partway.
pub(crate) struct Buffered<'a> {
    inner: &'a mut dyn Write,
    buf: Vec<u8>,
    staged: &'a Cell<u64>,
    /// `staged` when it was last looked at
    rows: u64,
    /// where in `buf` each count of rows was reached, in order: the rows up to that count
    /// end there
    marks: Vec<(usize, u64)>,
    sent: &'a Cell<u64>,
    /// the caller's writer failed, so nothing more goes to it
    failed: bool,
}

impl Buffered<'_> {
    /// Note the rows counted since the last look, which end where `buf` ends now.
    #[inline(always)]
    fn note(&mut self) {
        let rows = self.staged.get();
        if rows != self.rows {
            self.mark(rows);
        }
    }

    /// [`Write::write`] of `b` when it does not fit in what is left of `buf`.
    #[inline(never)]
    fn write_full(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.drain()?;
        if b.len() >= OUTPUT_BUFFER {
            // no row ends inside one write, so this piece ends none
            let r = self.inner.write_all(b);
            self.failed |= r.is_err();
            r?;
        } else {
            self.buf.extend_from_slice(b);
        }
        Ok(b.len())
    }

    /// The rows up to `rows` end where `buf` ends now. Kept out of line so that the
    /// serializers' many small writes inline only the check.
    #[inline(never)]
    fn mark(&mut self, rows: u64) {
        self.rows = rows;
        self.marks.push((self.buf.len(), rows));
    }

    /// Write the collected bytes on. Each row counts in `sent` as soon as the caller's
    /// writer has taken the bytes up to its end, also when it takes them in parts and
    /// fails before the last.
    fn drain(&mut self) -> std::io::Result<()> {
        let mut done = 0;
        let mut mark = 0;
        while done < self.buf.len() {
            match self.inner.write(&self.buf[done..]) {
                Ok(0) => {
                    self.failed = true;
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                Ok(n) => {
                    done += n;
                    while let Some(&(end, rows)) = self.marks.get(mark)
                        && end <= done
                    {
                        self.sent.set(rows);
                        mark += 1;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.failed = true;
                    return Err(e);
                }
            }
        }
        // every mark ends within `buf`
        if let Some(&(_, rows)) = self.marks.last() {
            self.sent.set(rows);
        }
        self.buf.clear();
        self.marks.clear();
        Ok(())
    }
}

impl Write for Buffered<'_> {
    #[inline(always)]
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.note();
        if b.len() > OUTPUT_BUFFER - self.buf.len() {
            return self.write_full(b);
        }
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    #[inline(always)]
    fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.write(b).map(|_| ())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.note();
        self.drain()?;
        self.inner.flush()
    }
}

/// Run `f`, a serializer, on a [`Buffered`] writer to `w`, and write what it left in the
/// buffer on. A serializer that fails still has the bytes before its failure written on,
/// as they were without the buffer, so `sent` counts the rows that reached `w`.
fn write_buffered(
    w: &mut dyn Write,
    sent: &Cell<u64>,
    f: &mut dyn FnMut(&mut Buffered<'_>, &Cell<u64>) -> Result<()>,
) -> Result<()> {
    let staged = Cell::new(sent.get());
    let mut out = Buffered {
        inner: w,
        buf: Vec::with_capacity(OUTPUT_BUFFER),
        staged: &staged,
        rows: staged.get(),
        marks: Vec::new(),
        sent,
        failed: false,
    };
    let result = f(&mut out, &staged);
    out.note();
    match result {
        Ok(()) => out.drain().map_err(io),
        Err(e) => {
            // the error is the one to report, and the output stops here either way
            if !out.failed {
                let _ = out.drain();
            }
            Err(e)
        }
    }
}

/// The reservation for the cells of a streamed row that the batch's term cache does not
/// hold, which are decoded or built for the row and dropped after it. It grows to the
/// largest such row and is held until the output ends, so a row costs no reservation of
/// its own. One reservation per row went through the query's memory ledger for every row
/// with a computed value, which took about a quarter of the time of writing 500,000 rows
/// with a BIND.
#[derive(Default)]
struct RowCharge {
    charge: Option<super::ctx::OwnedCharge>,
    held: u64,
    /// what the current row has asked for so far
    used: u64,
}

impl RowCharge {
    fn start_row(&mut self) {
        self.used = 0;
    }

    /// Cover `bytes` more of the current row before they are allocated.
    fn cover(&mut self, cursor: &super::QueryCursor, bytes: u64) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        self.used = self.used.saturating_add(bytes);
        if self.used <= self.held {
            return Ok(());
        }
        match &mut self.charge {
            Some(charge) => charge.resize(self.used)?,
            None => self.charge = Some(cursor.charge(self.used)?),
        }
        self.held = self.used;
        Ok(())
    }

    /// The term of a cell, from the batch's term cache or decoded under this
    /// reservation.
    fn term<'b>(
        &mut self,
        cursor: &super::QueryCursor,
        batch: &super::QueryBatch,
        decoded: Option<&'b super::cursor::DecodedBatch<'b>>,
        row: usize,
        col: usize,
    ) -> Result<Option<Cow<'b, Term>>> {
        match decoded {
            Some(d) => d.term(row, col),
            None => Ok(batch
                .covered_term(row, col, |bytes| self.cover(cursor, bytes))?
                .map(Cow::Owned)),
        }
    }
}

/// Consume SELECT batches directly, applying writer backpressure before requesting
/// the next batch. A later error leaves partial bytes and no success terminator.
/// A writer that a serializer writes to while the code around it keeps a way to flush
/// it, which the cursor writers use after their first batch.
pub(in crate::sparql) struct Shared<'a, W>(pub(in crate::sparql) &'a std::cell::RefCell<W>);

impl<W: Write> Write for Shared<'_, W> {
    #[inline]
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().write(b)
    }

    #[inline]
    fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.0.borrow_mut().write_all(b)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.borrow_mut().flush()
    }
}

/// Write a cursor's solutions in `fmt`. The writer is flushed once, after the rows of
/// the first batch, so that a writer which commits a response on a flush sends them
/// while the cursor computes the next batch. Later batches are not flushed.
pub fn write_cursor_solutions(
    cursor: &mut super::QueryCursor,
    fmt: SolutionsFormat,
    writer: impl Write,
    send: Option<usize>,
) -> Result<super::CursorStats> {
    let format = match fmt {
        SolutionsFormat::Json => QueryResultsFormat::Json,
        SolutionsFormat::Xml => QueryResultsFormat::Xml,
        SolutionsFormat::Csv => QueryResultsFormat::Csv,
        SolutionsFormat::Tsv => QueryResultsFormat::Tsv,
        SolutionsFormat::Sparkles => return write_cursor_native_json(cursor, writer, send, None),
    };
    let variables: Vec<Variable> = cursor
        .variables()
        .iter()
        .map(Variable::new_unchecked)
        .collect();
    let shared = std::cell::RefCell::new(writer);
    let mut out = Shared(&shared);
    let result = (|| {
        cursor.check()?;
        let mut serializer = QueryResultsSerializer::from_format(format)
            .serialize_solutions_to_writer(&mut out, variables.clone())
            .map_err(io)?;
        let mut left = send.unwrap_or(usize::MAX);
        let mut row_charge = RowCharge::default();
        let mut first = true;
        let remaining = cursor.for_each_decoded(send, |cursor, batch, decoded| {
            let _row_storage = cursor.charge(
                batch.width() as u64 * std::mem::size_of::<(usize, Cow<'_, Term>)>() as u64 + 128,
            )?;
            let mut terms = Vec::with_capacity(batch.width());
            for row in 0..batch.len().min(left) {
                if row.is_multiple_of(1024) {
                    cursor.check()?;
                }
                row_charge.start_row();
                if let Some(d) = decoded {
                    row_charge.cover(cursor, d.row_bytes(row)?)?;
                }
                for col in 0..batch.width() {
                    if let Some(term) = row_charge.term(cursor, batch, decoded, row, col)? {
                        terms.push((col, term));
                    }
                }
                serializer
                    .serialize(
                        terms
                            .iter()
                            .map(|(col, term)| (variables[*col].as_ref(), term.as_ref().as_ref())),
                    )
                    .map_err(io)?;
                // Release owned fallbacks before their row reservation drops;
                // keep only the adapter capacity for the next row.
                terms.clear();
                left -= 1;
            }
            if std::mem::take(&mut first) {
                shared.borrow_mut().flush().map_err(io)?;
            }
            Ok(())
        })?;
        if remaining == 0 {
            cursor.close();
        }
        cursor.check()?;
        serializer.finish().map_err(io)?;
        Ok(cursor.stats())
    })();
    if let Err(error) = &result {
        cursor.fail_output(error);
    }
    result
}

/// Serialize SELECT / ASK results.
pub fn write_solutions(
    r: &QueryResult,
    fmt: SolutionsFormat,
    w: impl Write,
    send: Option<usize>,
) -> Result<()> {
    write_solutions_counted(r, fmt, w, send, &Cell::new(0))
}

/// [`write_solutions`], counting in `sent` the rows written whole. A caller whose writer
/// stops the output partway, such as at the result-size budget, learns from it how many
/// rows the output holds.
pub fn write_solutions_counted(
    r: &QueryResult,
    fmt: SolutionsFormat,
    mut w: impl Write,
    send: Option<usize>,
    sent: &Cell<u64>,
) -> Result<()> {
    if fmt == SolutionsFormat::Sparkles {
        return write_native_json_counted(r, w, send, None, sent);
    }
    write_buffered(&mut w, sent, &mut |w, sent| {
        solutions_to(r, fmt, w, send, sent)
    })
}

/// [`write_solutions_counted`] into the buffer of [`write_buffered`].
fn solutions_to(
    r: &QueryResult,
    fmt: SolutionsFormat,
    w: &mut Buffered<'_>,
    send: Option<usize>,
    sent: &Cell<u64>,
) -> Result<()> {
    r.inner.ctx.check_memory_failure()?;
    let f = match fmt {
        SolutionsFormat::Json => QueryResultsFormat::Json,
        SolutionsFormat::Xml => QueryResultsFormat::Xml,
        SolutionsFormat::Csv => QueryResultsFormat::Csv,
        SolutionsFormat::Tsv => QueryResultsFormat::Tsv,
        SolutionsFormat::Sparkles => unreachable!(),
    };
    let ser = QueryResultsSerializer::from_format(f);
    if r.kind == QueryKind::Ask {
        // CSV and TSV have no boolean form: Jena writes a column `_askResult`, and its
        // clients read nothing else
        match fmt {
            SolutionsFormat::Csv => write!(w, "_askResult\r\n{}\r\n", r.boolean).map_err(io)?,
            SolutionsFormat::Tsv => write!(w, "?_askResult\n{}\n", r.boolean).map_err(io)?,
            _ => {
                ser.serialize_boolean_to_writer(w, r.boolean).map_err(io)?;
            }
        }
        return Ok(());
    }
    let vars: Vec<Variable> = r
        .variables()
        .iter()
        .map(|v| Variable::new_unchecked(v.clone()))
        .collect();
    let mut s = ser
        .serialize_solutions_to_writer(w, vars.clone())
        .map_err(io)?;
    let n = send.map_or(r.inner.table.len(), |s| s.min(r.inner.table.len()));
    if n <= DECODE_ROWS {
        // a result of one chunk is decoded row by row: under concurrent load that keeps
        // less memory live per request than a chunk's table of terms, and in the 10.5M
        // benchmark it served 25% more requests a second
        // Even a small answer's terms lie on scattered vocabulary pages. Keep
        // row-by-row decoding's small working set, but overlap those cold reads.
        if n >= 2 && crate::index::io_hints() {
            prefetch_answer(r, n)?;
        }
        for i in 0..n {
            let row = output::ScratchRow::decode(r, i)?;
            s.serialize(
                row.terms
                    .iter()
                    .enumerate()
                    .filter_map(|(c, term)| term.as_ref().map(|t| (vars[c].as_ref(), t.as_ref()))),
            )
            .map_err(io)?;
            sent.set(sent.get() + 1);
        }
        r.inner.ctx.check_memory_failure()?;
        s.finish().map_err(io)?;
        return Ok(());
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
        let _unwind_observer = ChunkCallerUnwindTrace(&r.inner.ctx);
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
                    let _trace = ChunkWorkerTrace {
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
            for i in start..end {
                if failure.failed() {
                    break;
                }
                let row = match ChunkRow::decode(r, &d, i) {
                    Ok(row) => row,
                    Err(error) => {
                        failure.record(error);
                        break;
                    }
                };
                if let Err(error) = s.serialize(
                    row.terms
                        .iter()
                        .map(|(c, term)| (vars[*c].as_ref(), term.as_ref().as_ref())),
                ) {
                    failure.record(io(error));
                    #[cfg(test)]
                    r.inner.ctx.chunk_event("io-failure-recorded", i);
                    break;
                }
                sent.set(sent.get() + 1);
            }
            let completed = next.and_then(|handle| match handle.join() {
                Ok(cells) => cells,
                Err(panic) => {
                    tracing::warn!("query output prefetch worker panicked");
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
                    completed.ok_or_else(|| Error::invalid("missing completed prefetch chunk"))?,
                );
            }
            start = end;
        }
        r.inner.ctx.check()?;
        Ok(())
    })?;
    drop(scope_header);
    s.finish().map_err(io)?;
    Ok(())
}

// Keep prefetch preparation out of the generic serializer's row loop and stack frame.
#[inline(never)]
fn prefetch_answer(r: &QueryResult, n: usize) -> Result<()> {
    let ids = || {
        r.inner.table.cols.iter().flat_map(|col| {
            col[..n.min(col.len())]
                .iter()
                .filter(|id| id.tag() == Tag::Vocab)
                .map(|id| id.payload())
        })
    };
    let ctx = &r.inner.ctx;
    ctx.check()?;
    let vocab = &ctx.snap.generation.vocab;
    if n < PREFETCH_BLOCKS_BELOW && vocab.prefetch_would_skip(ids()) {
        return Ok(());
    }
    let count = ids().count();
    if count == 0 {
        return Ok(());
    }
    let scratch = ctx.charge(0)?;
    if n < PREFETCH_BLOCKS_BELOW {
        let held = ctx
            .charge(chunk_bytes::<u64>(ctx, count)?)?
            .into_output(ctx);
        let mut sorted = chunk_storage(ctx, count, &held, "answer-prefetch-ids")?;
        sorted.extend(ids());
        sorted.sort_unstable();
        sorted.dedup();
        vocab.prefetch_sorted_checked(&sorted, |need| {
            ctx.check()?;
            scratch.resize(
                scratch
                    .bytes()
                    .max(u64::try_from(need).map_err(|_| ctx.memory_exceeded(u64::MAX))?),
            )
        })?;
    } else {
        vocab.prefetch_terms_checked(ids(), count, |need| {
            ctx.check()?;
            scratch.resize(
                scratch
                    .bytes()
                    .max(u64::try_from(need).map_err(|_| ctx.memory_exceeded(u64::MAX))?),
            )
        })?;
    }
    ctx.check()
}

/// Answers of fewer rows prefetch the exact vocabulary blocks of their terms rather
/// than the sparse-index groups that hold them.
const PREFETCH_BLOCKS_BELOW: usize = 256;

/// Result rows decoded at a time for serialization.
const DECODE_ROWS: usize = 1 << 16;
/// Sorted base-vocabulary ids decoded by one parallel task.
const DECODE_TASK: usize = 2048;
/// Sorted base-vocabulary ids below which a chunk is decoded in the calling thread.
const PAR_DECODE_MIN: usize = 4 * DECODE_TASK;

/// The terms of the distinct base-vocabulary ids in a chunk of the rows of a result of
/// several chunks (an export), decoded in id order: the vocabulary's front-coded blocks
/// are visited once each and in file order, by parallel tasks for many ids, instead of
/// once per row and column in row order. Their pages are asked of the kernel ahead of the
/// decoding, the next chunk's while a chunk is written. A cold vocabulary is then read by
/// many requests at once, mostly in ascending runs of pages, rather than one random page
/// fault at a time.
struct Decoded {
    start: usize,
    rows: usize,
    terms: ChunkTerms,
    slot: Vec<u32>,
    _credit: super::ctx::OutputCredit,
}

// Every partial/complete term vector destroys nested payload before its guard.
#[derive(Default)]
struct ChunkTerms {
    values: Vec<Term>,
    depth: usize,
}
impl std::ops::Deref for ChunkTerms {
    type Target = [Term];
    fn deref(&self) -> &[Term] {
        &self.values
    }
}
impl Drop for ChunkTerms {
    fn drop(&mut self) {
        super::depth::with_stack(self.depth, || self.values.clear());
    }
}
struct ChunkPart<'a> {
    terms: ChunkTerms,
    credit: Option<super::ctx::Charge<'a>>,
}
#[cfg(test)]
struct ChunkWorkerTrace<'a> {
    ctx: &'a super::Ctx,
    stage: &'static str,
    count: usize,
}
#[cfg(test)]
impl Drop for ChunkWorkerTrace<'_> {
    fn drop(&mut self) {
        self.ctx.chunk_event(self.stage, self.count);
    }
}

#[cfg(test)]
struct ChunkCallerUnwindTrace<'a>(&'a super::Ctx);
#[cfg(test)]
impl Drop for ChunkCallerUnwindTrace<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.chunk_event("caller-scope-unwind", 0);
        }
    }
}

struct Cells {
    start: usize,
    rows: usize,
    pairs: Vec<(u64, u32)>,
    prefetched: bool,
    _credit: super::ctx::OutputCredit,
}

fn chunk_bytes<T>(ctx: &super::Ctx, count: usize) -> Result<u64> {
    count
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))
}
fn chunk_storage<T>(
    ctx: &super::Ctx,
    count: usize,
    held: &super::ctx::OutputCredit,
    site: &'static str,
) -> Result<Vec<T>> {
    let bytes = chunk_bytes::<T>(ctx, count)?;
    if !held.belongs_to(ctx) || held.bytes() < bytes {
        return Err(Error::invalid("missing concrete chunk storage admission"));
    }
    ctx.check()?;
    #[cfg(test)]
    {
        use crate::algo::memory::probe;
        probe::at(probe::Point::Allocation(site, count));
        if probe::fails(site) {
            return Err(Error::invalid("injected chunk storage allocation failure"));
        }
    }
    let _ = site;
    let mut out = Vec::new();
    out.try_reserve_exact(count)
        .map_err(|_| Error::invalid("chunk storage allocation failed"))?;
    if out.capacity() != count {
        return Err(Error::invalid("chunk capacity exceeds admission"));
    }
    ctx.check()?;
    Ok(out)
}

// One scoped operation's selected failure; neither global nor an error classifier.
// Secondary errors and their payloads are always destroyed after leaving the lock.
#[derive(Default)]
struct ChunkFailure(parking_lot::Mutex<Option<Error>>);
impl ChunkFailure {
    fn record(&self, error: Error) {
        let secondary = {
            let mut first = self.0.lock();
            if first.is_none() {
                *first = Some(error);
                None
            } else {
                Some(error)
            }
        };
        drop(secondary);
    }
    fn failed(&self) -> bool {
        self.0.lock().is_some()
    }
    fn take(&self) -> Option<Error> {
        self.0.lock().take()
    }
}

impl Decoded {
    #[cfg(test)]
    fn cells(r: &QueryResult, rows: std::ops::Range<usize>, ahead: bool) -> Cells {
        Self::try_cells(r, rows, ahead).expect("positive bulk cell fixture admission")
    }
    #[cfg(test)]
    fn new(r: &QueryResult, cells: Cells) -> Decoded {
        Self::try_new(r, cells).expect("positive bulk decode fixture admission")
    }

    fn try_cells(r: &QueryResult, rows: std::ops::Range<usize>, ahead: bool) -> Result<Cells> {
        use rayon::prelude::*;
        let ctx = &r.inner.ctx;
        ctx.check()?;
        let n = rows.len();
        let slots = r
            .inner
            .table
            .width()
            .checked_mul(n)
            .filter(|&n| n <= u32::MAX as usize)
            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
        // Count over borrowed columns without allocating a temporary index.
        let mut count = 0usize;
        for col in &r.inner.table.cols {
            let col = col
                .get(rows.clone())
                .ok_or_else(|| Error::invalid("chunk row range exceeds its result"))?;
            count = count
                .checked_add(col.iter().filter(|id| id.tag() == Tag::Vocab).count())
                .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
        }
        let held = ctx
            .charge(chunk_bytes::<(u64, u32)>(ctx, count)?)?
            .into_output(ctx);
        let mut pairs = chunk_storage(ctx, count, &held, "chunk-pairs")?;
        for (c, col) in r.inner.table.cols.iter().enumerate() {
            for (j, id) in col[rows.clone()].iter().enumerate() {
                if id.tag() == Tag::Vocab {
                    let cell = c
                        .checked_mul(n)
                        .and_then(|x| x.checked_add(j))
                        .filter(|&x| x < slots)
                        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
                    pairs.push((id.payload(), cell as u32));
                }
            }
        }
        if pairs.len() < PAR_DECODE_MIN {
            pairs.sort_unstable();
        } else {
            let tasks = pairs.len().div_ceil(DECODE_TASK);
            let bytes = u64::try_from(tasks)
                .ok()
                .and_then(|n| n.checked_mul(4096))
                .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
            let task_headers = ctx.charge(bytes)?;
            let sorted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pairs.par_sort_unstable()
            }));
            drop(task_headers);
            if let Err(panic) = sorted {
                tracing::warn!("query output sort worker panicked");
                drop(panic);
                return Err(Error::invalid("query output sort worker panicked"));
            }
        }
        if ahead {
            let ids_held = ctx
                .charge(chunk_bytes::<u64>(ctx, pairs.len())?)?
                .into_output(ctx);
            let mut ids = chunk_storage(ctx, pairs.len(), &ids_held, "chunk-prefetch-ids")?;
            ids.extend(pairs.iter().map(|p| p.0));
            ids.dedup();
            let scratch = ctx.charge(0)?;
            r.inner
                .ctx
                .snap
                .generation
                .vocab
                .prefetch_sorted_checked(&ids, |need| {
                    ctx.check()?;
                    scratch.resize(
                        scratch
                            .bytes()
                            .max(u64::try_from(need).map_err(|_| ctx.memory_exceeded(u64::MAX))?),
                    )
                })?;
        }
        ctx.check()?;
        Ok(Cells {
            start: rows.start,
            rows: n,
            pairs,
            prefetched: ahead,
            _credit: held,
        })
    }

    fn decode_part<'a>(
        r: &'a QueryResult,
        part: &[u64],
        container: &super::ctx::OutputCredit,
        ahead: bool,
    ) -> Result<ChunkPart<'a>> {
        let ctx = &r.inner.ctx;
        #[cfg(test)]
        let _trace = ChunkWorkerTrace {
            ctx,
            stage: "decode-end",
            count: part.len(),
        };
        #[cfg(test)]
        ctx.chunk_event("decode-start", part.len());
        let vocab = &ctx.snap.generation.vocab;
        let payload = ctx.charge(0)?;
        let mut terms = ChunkTerms {
            values: chunk_storage(ctx, part.len(), container, "chunk-term-container")?,
            depth: 0,
        };
        let reconstruction = ctx.charge(0)?;
        vocab.get_sorted_owned_with_ahead(
            part,
            |need| {
                ctx.check()?;
                let bytes = u64::try_from(need)
                    .ok()
                    .and_then(|n| n.checked_mul(2))
                    .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
                reconstruction.resize(reconstruction.bytes().max(bytes))
            },
            |_, key| {
                if terms.values.len() == terms.values.capacity() {
                    return Err(Error::invalid("chunk decoded count exceeds admission"));
                }
                let term = ctx.output_key(key, &payload)?;
                terms.depth = terms.depth.max(crate::sparql::output::term_depth(&term));
                terms.values.push(term);
                ctx.check()
            },
            ahead,
        )?;
        if terms.values.len() != part.len() {
            return Err(Error::Corrupt(
                "missing vocabulary id in output chunk".into(),
            ));
        }
        ctx.check()?;
        Ok(ChunkPart {
            terms,
            credit: Some(payload),
        })
    }

    fn try_new(r: &QueryResult, cells: Cells) -> Result<Decoded> {
        use rayon::prelude::*;
        let ctx = &r.inner.ctx;
        if !cells._credit.belongs_to(ctx) {
            return Err(Error::invalid("bulk cells belong to another query account"));
        }
        ctx.check()?;
        let slots = r
            .inner
            .table
            .width()
            .checked_mul(cells.rows)
            .filter(|&n| n <= u32::MAX as usize)
            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
        let distinct = cells
            .pairs
            .iter()
            .map(|p| p.0)
            .try_fold((0usize, None), |(n, old), id| {
                n.checked_add(usize::from(old != Some(id)))
                    .map(|n| (n, Some(id)))
            })
            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?
            .0;
        let ids_held = ctx
            .charge(chunk_bytes::<u64>(ctx, distinct)?)?
            .into_output(ctx);
        let mut ids = chunk_storage(ctx, distinct, &ids_held, "chunk-distinct-ids")?;
        let tasks = if distinct < PAR_DECODE_MIN {
            1
        } else {
            distinct.div_ceil(DECODE_TASK)
        };
        // Final and worker term-container capacity overlap while payloads move.
        // Runtime task headers are bounded separately from process-owned stacks.
        let containers = chunk_bytes::<u32>(ctx, slots)?
            .checked_add(
                chunk_bytes::<Term>(ctx, distinct)?
                    .checked_mul(2)
                    .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?,
            )
            .and_then(|n| {
                n.checked_add(if distinct < PAR_DECODE_MIN {
                    0
                } else {
                    (tasks as u64).checked_mul(4096)?
                })
            })
            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
        let mut final_credit = ctx.charge(containers)?.into_output(ctx);
        let mut slot = chunk_storage(ctx, slots, &final_credit, "chunk-slots")?;
        slot.resize(slots, u32::MAX);
        for &(id, cell) in &cells.pairs {
            if ids.last() != Some(&id) {
                ids.push(id);
            }
            *slot
                .get_mut(cell as usize)
                .ok_or_else(|| Error::invalid("chunk slot exceeds admission"))? =
                (ids.len() - 1) as u32;
        }
        let ahead = !cells.prefetched;
        let mut terms = ChunkTerms {
            values: chunk_storage(ctx, distinct, &final_credit, "chunk-final-terms")?,
            depth: 0,
        };
        if distinct < PAR_DECODE_MIN {
            let mut part = Self::decode_part(r, &ids, &final_credit, ahead)?;
            final_credit.absorb_charge(&mut part.credit)?;
            terms.depth = part.terms.depth;
            terms.values.append(&mut part.terms.values);
        } else {
            let failure = ChunkFailure::default();
            let task_held = ctx
                .charge(chunk_bytes::<Option<ChunkPart<'_>>>(ctx, tasks)?)?
                .into_output(ctx);
            let mut parts = chunk_storage(ctx, tasks, &task_held, "chunk-task-index")?;
            parts.resize_with(tasks, || None);
            let joined = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ids.par_chunks(DECODE_TASK)
                    .zip(parts.par_iter_mut())
                    .for_each(|(ids, out)| {
                        if failure.failed() {
                            return;
                        }
                        match Self::decode_part(r, ids, &final_credit, ahead) {
                            Ok(part) => *out = Some(part),
                            Err(error) => failure.record(error),
                        }
                    });
            }));
            if let Err(panic) = joined {
                tracing::warn!("query output decode worker panicked");
                failure.record(Error::invalid("query output decode worker panicked"));
                drop(panic);
            }
            if let Some(error) = failure.take() {
                return Err(error);
            }
            for part in &mut parts {
                let part = part
                    .as_mut()
                    .ok_or_else(|| Error::invalid("missing completed chunk task"))?;
                final_credit.absorb_charge(&mut part.credit)?;
                terms.depth = terms.depth.max(part.terms.depth);
                terms.values.append(&mut part.terms.values);
            }
        }
        // Workspace/container capacity returns only after those allocations retire.
        let start = cells.start;
        let rows = cells.rows;
        drop(cells);
        drop(ids);
        drop(ids_held);
        let retained = chunk_bytes::<Term>(ctx, terms.values.capacity())?
            .checked_add(chunk_bytes::<u32>(ctx, slot.capacity())?)
            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
        // Existing credit consists of the initial workspace quote plus adopted
        // payload contributions; discard only the now-freed workspace portion.
        let workspace = containers
            .checked_sub(retained)
            .ok_or_else(|| Error::invalid("chunk workspace accounting mismatch"))?;
        final_credit.resize(
            final_credit
                .bytes()
                .checked_sub(workspace)
                .ok_or_else(|| Error::invalid("chunk retained accounting mismatch"))?,
        )?;
        ctx.check()?;
        Ok(Decoded {
            start,
            rows,
            terms,
            slot,
            _credit: final_credit,
        })
    }

    fn term<'a>(
        &'a self,
        r: &QueryResult,
        c: usize,
        i: usize,
        id: Id,
        row: &super::ctx::Charge<'_>,
    ) -> Result<Option<Cow<'a, Term>>> {
        if !self._credit.belongs_to(&r.inner.ctx) || !row.belongs_to(&r.inner.ctx) {
            return Err(Error::invalid(
                "bulk decoded row belongs to another query account",
            ));
        }
        let at = c
            .checked_mul(self.rows)
            .and_then(|n| n.checked_add(i.checked_sub(self.start)?))
            .ok_or_else(|| Error::invalid("chunk row index overflow"))?;
        match *self
            .slot
            .get(at)
            .ok_or_else(|| Error::invalid("chunk row exceeds admission"))?
        {
            s if (s as usize) < self.terms.len() => {
                Ok(Some(Cow::Borrowed(&self.terms[s as usize])))
            }
            _ => r.inner.ctx.output_term(id, row).map(|t| t.map(Cow::Owned)),
        }
    }
}

struct ChunkRow<'a> {
    terms: Vec<(usize, Cow<'a, Term>)>,
    depth: usize,
    _credit: super::ctx::Charge<'a>,
}
impl<'a> ChunkRow<'a> {
    fn decode(r: &'a QueryResult, decoded: &'a Decoded, index: usize) -> Result<Self> {
        let ctx = &r.inner.ctx;
        if !decoded._credit.belongs_to(ctx) {
            return Err(Error::invalid("bulk row belongs to another query account"));
        }
        ctx.check()?;
        let width = r.inner.table.width();
        let credit = ctx.charge(chunk_bytes::<(usize, Cow<'a, Term>)>(ctx, width)?)?;
        #[cfg(test)]
        {
            use crate::algo::memory::probe;
            probe::at(probe::Point::Allocation("chunk-row", width));
            if probe::fails("chunk-row") {
                return Err(Error::invalid("injected chunk row allocation failure"));
            }
        }
        let mut terms = Vec::new();
        terms
            .try_reserve_exact(width)
            .map_err(|_| Error::invalid("chunk row allocation failed"))?;
        if terms.capacity() != width {
            return Err(Error::invalid("chunk row capacity exceeds admission"));
        }
        let mut row = Self {
            terms,
            depth: 0,
            _credit: credit,
        };
        for (c, column) in r.inner.table.cols.iter().enumerate() {
            if let Some(term) = decoded.term(r, c, index, column[index], &row._credit)? {
                row.depth = row
                    .depth
                    .max(crate::sparql::output::term_depth(term.as_ref()));
                row.terms.push((c, term));
            }
        }
        ctx.check()?;
        Ok(row)
    }
}
impl Drop for ChunkRow<'_> {
    fn drop(&mut self) {
        super::depth::with_stack(self.depth, || self.terms.clear());
    }
}

/// Serialize CONSTRUCT / DESCRIBE results. A dataset format (TriG, N-Quads, JSON-LD)
/// also gets the quads of a CONSTRUCT with `GRAPH` blocks; a graph format, the default
/// graph only, as Fuseki writes it.
pub fn write_graph(
    r: &QueryResult,
    fmt: RdfFormat,
    prefixes: &BTreeMap<String, String>,
    w: impl Write,
) -> Result<()> {
    write_graph_counted(r, fmt, prefixes, w, &Cell::new(0))
}

/// [`write_graph`], counting in `sent` the triples and quads written whole.
pub fn write_graph_counted(
    r: &QueryResult,
    fmt: RdfFormat,
    prefixes: &BTreeMap<String, String>,
    mut w: impl Write,
    sent: &Cell<u64>,
) -> Result<()> {
    write_buffered(&mut w, sent, &mut |w, sent| {
        graph_to(r, fmt, prefixes, w, sent)
    })
}

/// [`write_graph_counted`] into the buffer of [`write_buffered`].
fn graph_to(
    r: &QueryResult,
    fmt: RdfFormat,
    prefixes: &BTreeMap<String, String>,
    w: &mut Buffered<'_>,
    sent: &Cell<u64>,
) -> Result<()> {
    let mut ser = RdfSerializer::from_format(fmt);
    if matches!(fmt, RdfFormat::Turtle | RdfFormat::TriG | RdfFormat::RdfXml) {
        for (p, ns) in prefixes {
            ser = ser
                .with_prefix(p.clone(), ns.clone())
                .map_err(|e| Error::invalid(e.to_string()))?;
        }
    }
    let mut s = ser.for_writer(w);
    for t in &r.inner.triples {
        s.serialize_triple(t).map_err(io)?;
        sent.set(sent.get() + 1);
    }
    if fmt.supports_datasets() {
        for q in &r.inner.quads {
            s.serialize_quad(q).map_err(io)?;
            sent.set(sent.get() + 1);
        }
    }
    s.finish().map_err(io)?;
    Ok(())
}

/// Serialize CONSTRUCT / DESCRIBE results in one of Jena's syntaxes, by the policy of
/// [`write_graph`]: TriX, RDF Thrift and RDF Protobuf also get the quads of a CONSTRUCT
/// with `GRAPH` blocks, and RDF/JSON, which holds one graph, the default graph only.
pub fn write_jena_graph(
    r: &QueryResult,
    fmt: crate::jena_formats::JenaFormat,
    w: impl Write,
) -> Result<()> {
    write_jena_graph_counted(r, fmt, w, &Cell::new(0))
}

/// [`write_jena_graph`], counting in `sent` the triples and quads written whole.
pub fn write_jena_graph_counted(
    r: &QueryResult,
    fmt: crate::jena_formats::JenaFormat,
    mut w: impl Write,
    sent: &Cell<u64>,
) -> Result<()> {
    write_buffered(&mut w, sent, &mut |w, sent| jena_graph_to(r, fmt, w, sent))
}

/// [`write_jena_graph_counted`] into the buffer of [`write_buffered`].
fn jena_graph_to(
    r: &QueryResult,
    fmt: crate::jena_formats::JenaFormat,
    w: &mut Buffered<'_>,
    sent: &Cell<u64>,
) -> Result<()> {
    // RDF/JSON groups the triples by subject and writes them all at the end, so they
    // count by subject as `finish_counted` writes them
    let deferred = fmt == crate::jena_formats::JenaFormat::RdfJson;
    let mut out = crate::jena_formats::RdfWriter::new(fmt, w);
    for t in &r.inner.triples {
        out.triple(t).map_err(io)?;
        if !deferred {
            sent.set(sent.get() + 1);
        }
    }
    if fmt.quads() {
        for q in &r.inner.quads {
            out.quad(q).map_err(io)?;
            if !deferred {
                sent.set(sent.get() + 1);
            }
        }
    }
    out.finish_counted(|n| sent.set(sent.get() + n))
        .map_err(io)?;
    Ok(())
}

/// Extra metadata for an HTTP native response. Its timing includes serialization
/// through the result rows; metadata is written last so that no document must be
/// retained just to patch the timing.
pub struct NativeJsonMetadata<'a> {
    pub commit: u64,
    pub dataset_id: &'a str,
    /// the request asked for the I/O profile (`profile=io`): its report goes into the
    /// metadata, written after the rows
    pub io: Option<&'a crate::ioacct::Profile>,
}

/// Write the rich native result without building an intermediate JSON document.
/// SELECT admits and decodes one complete row before writing its cells.
/// Graph results and plan metadata are borrowed.
/// The query's underlying result tables/graphs are already materialized.
pub fn write_native_json(
    r: &QueryResult,
    w: impl Write,
    send: Option<usize>,
    metadata: Option<NativeJsonMetadata<'_>>,
) -> Result<()> {
    write_native_json_counted(r, w, send, metadata, &Cell::new(0))
}

/// [`write_native_json`], counting in `sent` the rows written whole.
pub fn write_native_json_counted(
    r: &QueryResult,
    mut w: impl Write,
    send: Option<usize>,
    metadata: Option<NativeJsonMetadata<'_>>,
    sent: &Cell<u64>,
) -> Result<()> {
    let mut metadata = metadata;
    write_buffered(&mut w, sent, &mut |w, sent| {
        native_json_to(r, w, send, metadata.take(), sent)
    })
}

/// [`write_native_json_counted`] into the buffer of [`write_buffered`].
fn native_json_to(
    r: &QueryResult,
    w: &mut Buffered<'_>,
    send: Option<usize>,
    metadata: Option<NativeJsonMetadata<'_>>,
    sent: &Cell<u64>,
) -> Result<()> {
    r.inner.ctx.check_memory_failure()?;
    let n = send.map_or(r.len(), |s| s.min(r.len()));
    // Serde's error type cannot carry the engine's typed refusal. Keep the exact
    // first decode error at this one boundary instead of converting it into I/O.
    let failure = RefCell::new(None);
    let result = serde_json::to_writer(
        w,
        &NativeJson {
            r,
            n,
            commit: None,
            metadata,
            started: Instant::now(),
            sent,
            failure: &failure,
        },
    );
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    result.map_err(|e| Error::Io(e.into()))
}

/// Consume native SELECT rows without collecting them. Metadata is written only
/// after successful production; a consumer cap leaves `totalRows` unknown.
pub fn write_cursor_native_json(
    cursor: &mut super::QueryCursor,
    mut writer: impl Write,
    send: Option<usize>,
    metadata: Option<NativeJsonMetadata<'_>>,
) -> Result<super::CursorStats> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Meta<'a> {
        total_rows: Option<u64>,
        sent_rows: u64,
        status: super::CursorStatus,
        timing: &'a super::Timing,
        plan: &'a super::CursorPlan,
        memory: NativeMemory,
        rows_produced: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        commit: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dataset_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        io: Option<crate::ioacct::IoReport>,
    }
    fn json(w: &mut impl Write, v: &impl Serialize) -> Result<()> {
        serde_json::to_writer(w, v).map_err(|e| Error::Io(e.into()))
    }
    let started = Instant::now();
    let initial_exec = cursor.stats().timing.exec_ms;
    let result = (|| {
        cursor.check()?;
        writer.write_all(b"{\"queryType\":\"SELECT\",\"vars\":")?;
        json(&mut writer, &cursor.variables())?;
        writer.write_all(b",\"rows\":[")?;
        let mut left = send.unwrap_or(usize::MAX);
        let mut sent = 0u64;
        let mut row_charge = RowCharge::default();
        let mut first = true;
        let remaining = cursor.for_each_decoded(send, |cursor, batch, decoded| {
            for row in 0..batch.len().min(left) {
                if row.is_multiple_of(1024) {
                    cursor.check()?;
                }
                if sent > 0 {
                    writer.write_all(b",")?;
                }
                writer.write_all(b"[")?;
                // One reservation covers the row's uncached cells through their
                // serialization, as in the other result formats.
                row_charge.start_row();
                if let Some(d) = decoded {
                    row_charge.cover(cursor, d.row_bytes(row)?)?;
                }
                for col in 0..batch.width() {
                    if col > 0 {
                        writer.write_all(b",")?;
                    }
                    let term = row_charge.term(cursor, batch, decoded, row, col)?;
                    json(
                        &mut writer,
                        &term.as_ref().map(|t| NativeTerm(t.as_ref().as_ref())),
                    )?;
                }
                writer.write_all(b"]")?;
                sent += 1;
                left -= 1;
            }
            // the first batch's rows reach a writer that commits on a flush
            if std::mem::take(&mut first) {
                writer.flush()?;
            }
            Ok(())
        })?;
        if remaining == 0 {
            cursor.close();
        }
        cursor.check()?;
        let stats = cursor.stats();
        let mut timing = stats.timing.clone();
        timing.serialize_ms =
            (started.elapsed().as_secs_f64() * 1000.0 - (timing.exec_ms - initial_exec)).max(0.0);
        writer.write_all(b"],\"meta\":")?;
        super::depth::with_stack(cursor.stack_depth(), || {
            json(
                &mut writer,
                &Meta {
                    total_rows: (stats.status == super::CursorStatus::Complete).then_some(sent),
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
        writer.write_all(b"}")?;
        Ok(stats)
    })();
    if let Err(error) = &result {
        cursor.fail_output(error);
    }
    result
}

/// Stream graph rows in the native document. Default graph rows are represented
/// as quads with a null graph, so mixed graph output needs no partition buffer.
pub fn write_cursor_graph_native_json(
    cursor: &mut super::GraphCursor,
    mut writer: impl Write,
    send: Option<usize>,
    metadata: Option<NativeJsonMetadata<'_>>,
) -> Result<super::CursorStats> {
    let result = super::depth::with_stack(cursor.stack_depth(), || {
        cursor.check()?;
        let _charge = cursor.charge(4096)?;
        write!(writer, "{{\"queryType\":")?;
        serde_json::to_writer(&mut writer, &cursor.kind()).map_err(|e| Error::Io(e.into()))?;
        writer.write_all(b",\"triples\":[],\"quads\":[")?;
        let mut sent = 0usize;
        let mut first = true;
        while send.is_none_or(|max| sent < max) {
            let Some(batch) = cursor.next_at_most(send.map_or(usize::MAX, |max| max - sent))?
            else {
                break;
            };
            for quad in batch.quads() {
                if sent.is_multiple_of(1024) {
                    cursor.check()?;
                }
                if sent > 0 {
                    writer.write_all(b",")?;
                }
                // Reuse the borrowed native term serializer, without a JSON tree.
                serde_json::to_writer(&mut writer, &NativeQuad(quad))
                    .map_err(|e| Error::Io(e.into()))?;
                sent += 1;
            }
            // the first batch's quads reach a writer that commits on a flush
            if std::mem::take(&mut first) {
                writer.flush()?;
            }
        }
        if send.is_some_and(|max| sent >= max) {
            cursor.close();
        }
        let stats = cursor.stats();
        writer.write_all(b"],\"meta\":{")?;
        write!(
            writer,
            "\"totalRows\":{},\"sentRows\":{sent},\"status\":",
            if stats.status == super::CursorStatus::Complete {
                sent.to_string()
            } else {
                "null".into()
            }
        )?;
        serde_json::to_writer(&mut writer, &stats.status).map_err(|e| Error::Io(e.into()))?;
        writer.write_all(b",\"timing\":")?;
        serde_json::to_writer(&mut writer, &stats.timing).map_err(|e| Error::Io(e.into()))?;
        writer.write_all(b",\"plan\":")?;
        serde_json::to_writer(&mut writer, cursor.plan()).map_err(|e| Error::Io(e.into()))?;
        let granted = stats
            .granted_bytes
            .map_or(String::new(), |g| format!(",\"grantedBytes\":{g}"));
        write!(
            writer,
            ",\"memory\":{{\"peakBytes\":{}{granted}}},\"rowsProduced\":{},\"describeTruncated\":{}",
            stats.mem_peak_bytes,
            stats.rows_produced,
            cursor.describe_truncated()
        )?;
        if let Some(meta) = metadata {
            write!(writer, ",\"commit\":{},\"datasetId\":", meta.commit)?;
            serde_json::to_writer(&mut writer, meta.dataset_id).map_err(|e| Error::Io(e.into()))?;
            if let Some(p) = meta.io {
                writer.write_all(b",\"io\":")?;
                serde_json::to_writer(&mut writer, &p.report()).map_err(|e| Error::Io(e.into()))?;
            }
        }
        writer.write_all(b"}}")?;
        Ok(stats)
    });
    if let Err(error) = &result {
        cursor.fail_output(error);
    }
    result
}

/// Jena's wire formats stream one statement at a time. RDF/JSON groups objects
/// by subject and predicate; reserve its retained representation before insertion.
pub fn write_cursor_jena_graph(
    cursor: &mut super::GraphCursor,
    format: crate::jena_formats::JenaFormat,
    output: impl Write,
    send: Option<usize>,
) -> Result<super::CursorStats> {
    let result = super::depth::with_stack(cursor.stack_depth(), || {
        let mut charge = cursor.charge(128 << 10)?;
        let mut bytes = 128 << 10;
        let shared = std::cell::RefCell::new(output);
        let mut writer = crate::jena_formats::RdfWriter::new(format, Shared(&shared));
        let mut sent = 0usize;
        let mut first = true;
        while send.is_none_or(|max| sent < max) {
            let Some(batch) = cursor.next_at_most(send.map_or(usize::MAX, |max| max - sent))?
            else {
                break;
            };
            for quad in batch.quads() {
                if sent.is_multiple_of(1024) {
                    cursor.check()?;
                }
                if format == crate::jena_formats::JenaFormat::RdfJson {
                    bytes += super::graph_quad_bytes(quad).saturating_mul(4) + 1024;
                    charge.resize(bytes)?;
                }
                if format.quads() {
                    writer.quad(quad).map_err(io)?;
                } else if quad.graph_name == oxrdf::GraphName::DefaultGraph {
                    writer
                        .triple(&oxrdf::Triple::new(
                            quad.subject.clone(),
                            quad.predicate.clone(),
                            quad.object.clone(),
                        ))
                        .map_err(io)?;
                }
                sent += 1;
            }
            // what the first batch wrote reaches a writer that commits on a flush
            if std::mem::take(&mut first) {
                shared.borrow_mut().flush()?;
            }
        }
        if send.is_some_and(|max| sent >= max) {
            cursor.close();
        }
        writer.finish().map_err(io)?;
        Ok(cursor.stats())
    });
    if let Err(error) = &result {
        cursor.fail_output(error);
    }
    result
}

struct NativeJson<'a> {
    r: &'a QueryResult,
    n: usize,
    commit: Option<u64>,
    metadata: Option<NativeJsonMetadata<'a>>,
    started: Instant,
    sent: &'a Cell<u64>,
    failure: &'a RefCell<Option<Error>>,
}

impl Serialize for NativeJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let r = self.r;
        let mut map = s.serialize_map(None)?;
        map.serialize_entry("queryType", &r.kind)?;
        match r.kind {
            QueryKind::Select => {
                map.serialize_entry("vars", &r.variables())?;
                map.serialize_entry(
                    "rows",
                    &NativeRows {
                        r,
                        n: self.n,
                        sent: self.sent,
                        failure: self.failure,
                    },
                )?;
            }
            QueryKind::Ask => map.serialize_entry("boolean", &r.boolean)?,
            _ => {
                map.serialize_entry(
                    "triples",
                    &NativeGraphRows {
                        rows: GraphRows::Triples(
                            &r.inner.triples[..self.n.min(r.inner.triples.len())],
                        ),
                        sent: self.sent,
                    },
                )?;
                if !r.inner.quads.is_empty() {
                    let n = self
                        .n
                        .saturating_sub(r.inner.triples.len())
                        .min(r.inner.quads.len());
                    let rows = NativeGraphRows {
                        rows: GraphRows::Quads(&r.inner.quads[..n]),
                        sent: self.sent,
                    };
                    map.serialize_entry("quads", &rows)?;
                }
            }
        }
        if let Err(error) = r.inner.ctx.check_memory_failure() {
            return Err(native_refusal::<S::Error>(self.failure, error));
        }
        let mut timing = r.timing.clone();
        if self.metadata.is_some() {
            timing.serialize_ms = self.started.elapsed().as_secs_f64() * 1000.0;
            timing.total_ms += timing.serialize_ms;
        }
        map.serialize_entry(
            "meta",
            &NativeMeta {
                total_rows: r.len(),
                sent_rows: self.n,
                timing: &timing,
                plan: &r.inner.plan,
                memory: NativeMemory {
                    peak_bytes: r.mem_peak_bytes,
                    granted_bytes: r.inner.ctx.grant().map(|g| g.peak()),
                },
                rows_produced: r.rows_produced,
                commit: self.metadata.as_ref().map(|m| m.commit).or(self.commit),
                dataset_id: self.metadata.as_ref().map(|m| m.dataset_id),
                io: self
                    .metadata
                    .as_ref()
                    .and_then(|m| m.io)
                    .map(|p| p.report()),
            },
        )?;
        map.end()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeMeta<'a> {
    total_rows: usize,
    sent_rows: usize,
    timing: &'a super::Timing,
    plan: &'a super::PlanInfo,
    memory: NativeMemory,
    rows_produced: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dataset_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    io: Option<crate::ioacct::IoReport>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeMemory {
    peak_bytes: u64,
    /// the most the query held of the server's memory pool
    #[serde(skip_serializing_if = "Option::is_none")]
    granted_bytes: Option<u64>,
}

struct NativeRows<'a> {
    r: &'a QueryResult,
    n: usize,
    sent: &'a Cell<u64>,
    failure: &'a RefCell<Option<Error>>,
}

impl Serialize for NativeRows<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.n))?;
        // the terms of a small answer lie on scattered vocabulary pages: overlap their
        // cold reads as the other result writers do
        let ahead = self.n.min(DECODE_ROWS).min(self.r.inner.table.len());
        if ahead >= 2 && crate::index::io_hints() {
            prefetch_answer(self.r, ahead)
                .map_err(|error| native_refusal::<S::Error>(self.failure, error))?;
        }
        for i in 0..self.n {
            let row = output::ScratchRow::decode(self.r, i)
                .map_err(|error| native_refusal::<S::Error>(self.failure, error))?;
            seq.serialize_element(&NativeRow { terms: &row.terms })?;
            self.sent.set(self.sent.get() + 1);
        }
        seq.end()
    }
}

struct NativeRow<'a> {
    terms: &'a [Option<Term>],
}

impl Serialize for NativeRow<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.terms.len()))?;
        for term in self.terms {
            seq.serialize_element(&term.as_ref().map(|t| NativeTerm(t.as_ref())))?;
        }
        seq.end()
    }
}

fn native_refusal<E: serde::ser::Error>(failure: &RefCell<Option<Error>>, error: Error) -> E {
    // No payload or account is dropped while this short local borrow is held.
    let mut first = failure.borrow_mut();
    if first.is_none() {
        *first = Some(error);
    }
    E::custom("query output refused")
}

mod output;

struct NativeQuad<'a>(&'a oxrdf::Quad);
impl Serialize for NativeQuad<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let q = self.0.as_ref();
        let graph = match q.graph_name {
            oxrdf::GraphNameRef::NamedNode(n) => Some(NativeTerm(n.into())),
            oxrdf::GraphNameRef::BlankNode(b) => Some(NativeTerm(b.into())),
            oxrdf::GraphNameRef::DefaultGraph => None,
        };
        [
            Some(NativeTerm(q.subject.into())),
            Some(NativeTerm(q.predicate.into())),
            Some(NativeTerm(q.object)),
            graph,
        ]
        .serialize(s)
    }
}

enum GraphRows<'a> {
    Triples(&'a [oxrdf::Triple]),
    Quads(&'a [oxrdf::Quad]),
}

struct NativeGraphRows<'a> {
    rows: GraphRows<'a>,
    sent: &'a Cell<u64>,
}

impl Serialize for NativeGraphRows<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(match self.rows {
            GraphRows::Triples(rows) => rows.len(),
            GraphRows::Quads(rows) => rows.len(),
        }))?;
        match self.rows {
            GraphRows::Triples(rows) => {
                for row in rows {
                    let t = row.as_ref();
                    seq.serialize_element(&[
                        NativeTerm(t.subject.into()),
                        NativeTerm(t.predicate.into()),
                        NativeTerm(t.object),
                    ])?;
                    self.sent.set(self.sent.get() + 1);
                }
            }
            GraphRows::Quads(rows) => {
                for row in rows {
                    let q = row.as_ref();
                    let graph = match q.graph_name {
                        oxrdf::GraphNameRef::NamedNode(n) => Some(NativeTerm(n.into())),
                        oxrdf::GraphNameRef::BlankNode(b) => Some(NativeTerm(b.into())),
                        oxrdf::GraphNameRef::DefaultGraph => None,
                    };
                    seq.serialize_element(&[
                        Some(NativeTerm(q.subject.into())),
                        Some(NativeTerm(q.predicate.into())),
                        Some(NativeTerm(q.object)),
                        graph,
                    ])?;
                    self.sent.set(self.sent.get() + 1);
                }
            }
        }
        seq.end()
    }
}

struct NativeTerm<'a>(oxrdf::TermRef<'a>);

impl Serialize for NativeTerm<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use oxrdf::TermRef;
        let mut map = s.serialize_map(None)?;
        match self.0 {
            TermRef::NamedNode(n) => {
                map.serialize_entry("type", "uri")?;
                map.serialize_entry("value", n.as_str())?;
            }
            TermRef::BlankNode(b) => {
                map.serialize_entry("type", "bnode")?;
                map.serialize_entry("value", b.as_str())?;
            }
            TermRef::Literal(l) => {
                map.serialize_entry("type", "literal")?;
                map.serialize_entry("value", l.value())?;
                if let Some(lang) = l.language() {
                    map.serialize_entry("xml:lang", lang)?;
                    if let Some(d) = l.direction() {
                        map.serialize_entry(
                            "its:dir",
                            match d {
                                oxrdf::BaseDirection::Ltr => "ltr",
                                oxrdf::BaseDirection::Rtl => "rtl",
                            },
                        )?;
                    }
                } else if l.datatype() != oxrdf::vocab::xsd::STRING {
                    map.serialize_entry("datatype", l.datatype().as_str())?;
                }
            }
            TermRef::Triple(t) => {
                #[derive(Serialize)]
                struct TripleValue<'a> {
                    subject: NativeTerm<'a>,
                    predicate: NativeTerm<'a>,
                    object: NativeTerm<'a>,
                }
                let t = t.as_ref();
                map.serialize_entry("type", "triple")?;
                map.serialize_entry(
                    "value",
                    &TripleValue {
                        subject: NativeTerm(t.subject.into()),
                        predicate: NativeTerm(t.predicate.into()),
                        object: NativeTerm(t.object),
                    },
                )?;
            }
        }
        map.end()
    }
}

pub fn term_json(t: &Term) -> J {
    match t {
        Term::NamedNode(n) => json!({"type": "uri", "value": n.as_str()}),
        Term::BlankNode(b) => json!({"type": "bnode", "value": b.as_str()}),
        Term::Literal(l) => {
            let mut o = serde_json::Map::new();
            o.insert("type".into(), "literal".into());
            o.insert("value".into(), l.value().into());
            if let Some(lang) = l.language() {
                o.insert("xml:lang".into(), lang.into());
                if let Some(d) = l.direction() {
                    o.insert(
                        "its:dir".into(),
                        match d {
                            oxrdf::BaseDirection::Ltr => "ltr",
                            oxrdf::BaseDirection::Rtl => "rtl",
                        }
                        .into(),
                    );
                }
            } else if l.datatype() != oxrdf::vocab::xsd::STRING {
                o.insert("datatype".into(), l.datatype().as_str().into());
            }
            J::Object(o)
        }
        Term::Triple(t) => json!({
            "type": "triple",
            "value": {
                "subject": term_json(&t.subject.clone().into()),
                "predicate": term_json(&Term::NamedNode(t.predicate.clone())),
                "object": term_json(&t.object),
            }
        }),
    }
}

/// The retained `application/x-sparkles+json` document (see docs/API.md).
/// Its final encoded bytes retain their original actual query account.
pub fn sparkles_json(r: &QueryResult, send: Option<usize>) -> Result<super::output::OwnedJson> {
    sparkles_document(r, send, None)
}

/// The same document with an explicit commit in its existing metadata object.
/// This avoids an unowned parse-and-mutate copy at the asking-pipeline boundary.
pub fn sparkles_json_with_commit(
    r: &QueryResult,
    send: Option<usize>,
    commit: u64,
) -> Result<super::output::OwnedJson> {
    sparkles_document(r, send, Some(commit))
}

fn sparkles_document(
    r: &QueryResult,
    send: Option<usize>,
    commit: Option<u64>,
) -> Result<super::output::OwnedJson> {
    let failure = RefCell::new(None);
    let document = super::depth::with_stack(crate::nesting::MAX_TRIPLE_TERMS, || {
        r.output_budget().json(&NativeJson {
            r,
            n: send.map_or(r.len(), |n| n.min(r.len())),
            commit,
            metadata: None,
            started: Instant::now(),
            sent: &Cell::new(0),
            failure: &failure,
        })
    });
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    document
}

#[cfg(test)]
mod buffered_tests {
    use super::*;

    /// A writer that takes `room` bytes and refuses the write that would pass them, as
    /// the response writer does at the result-size budget.
    struct Capped {
        out: Vec<u8>,
        room: usize,
    }

    impl Write for Capped {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.out.len() + b.len() > self.room {
                return Err(std::io::Error::other("over the budget"));
            }
            self.out.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Rows of `row` bytes written a few bytes at a time and counted after each, as the
    /// serializers do.
    fn rows(n: u64, row: usize) -> impl FnMut(&mut Buffered<'_>, &Cell<u64>) -> Result<()> {
        move |w, staged| {
            for i in 0..n {
                let line = format!("{i:0>width$}\n", width = row - 1);
                for piece in line.as_bytes().chunks(7) {
                    w.write_all(piece).map_err(io)?;
                }
                staged.set(staged.get() + 1);
            }
            Ok(())
        }
    }

    #[test]
    fn the_output_is_whole_and_every_row_counted() {
        let mut out = Vec::new();
        let sent = Cell::new(0);
        write_buffered(&mut out, &sent, &mut rows(5000, 40)).unwrap();
        assert_eq!(out.len(), 5000 * 40);
        assert_eq!(sent.get(), 5000);
        // a piece longer than the buffer goes through as it is
        let mut out = Vec::new();
        let sent = Cell::new(0);
        write_buffered(&mut out, &sent, &mut rows(3, OUTPUT_BUFFER * 2 + 5)).unwrap();
        assert_eq!(out.len(), 3 * (OUTPUT_BUFFER * 2 + 5));
        assert_eq!(sent.get(), 3);
    }

    #[test]
    fn a_writer_that_stops_partway_has_exactly_the_rows_counted() {
        for room in [100, OUTPUT_BUFFER - 1, OUTPUT_BUFFER * 3 + 17, 150_000] {
            let mut w = Capped {
                out: Vec::new(),
                room,
            };
            let sent = Cell::new(0);
            assert!(write_buffered(&mut w, &sent, &mut rows(5000, 40)).is_err());
            assert!(w.out.len() <= room);
            let whole = w.out.iter().filter(|&&b| b == b'\n').count() as u64;
            assert_eq!(sent.get(), whole, "{room}");
        }
    }

    /// A writer that takes at most `step` bytes a call, `room` bytes in all, and then
    /// fails, as a response writer filling its chunk and then losing its client does.
    struct Short {
        out: Vec<u8>,
        step: usize,
        room: usize,
    }

    impl Write for Short {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            let n = b.len().min(self.step).min(self.room - self.out.len());
            if n == 0 {
                return Err(std::io::Error::other("the client went away"));
            }
            self.out.extend_from_slice(&b[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_writer_that_takes_part_and_then_fails_has_its_whole_rows_counted() {
        for (step, room) in [(100, 100), (37, 1000), (4096, 10_000), (1, 333)] {
            let mut w = Short {
                out: Vec::new(),
                step,
                room,
            };
            let sent = Cell::new(0);
            assert!(write_buffered(&mut w, &sent, &mut rows(5000, 40)).is_err());
            assert_eq!(w.out.len(), room);
            let whole = w.out.iter().filter(|&&b| b == b'\n').count() as u64;
            assert_eq!(sent.get(), whole, "{step} {room}");
        }
    }

    #[test]
    fn rdf_json_counts_its_triples_only_once_they_are_written() {
        use crate::io::{RdfFormat as In, Source};
        use crate::jena_formats::JenaFormat;
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<http://ex.org/s> <http://ex.org/p> \"o\" .\n".to_vec(),
                In::NTriples,
                None,
            )])
            .unwrap();
        let r = super::super::query(
            store.snapshot(),
            "CONSTRUCT WHERE { ?s ?p ?o }",
            &Default::default(),
        )
        .unwrap();
        let mut short = Short {
            out: Vec::new(),
            step: 1,
            room: 1,
        };
        let sent = Cell::new(0);
        assert!(write_jena_graph_counted(&r, JenaFormat::RdfJson, &mut short, &sent).is_err());
        assert_eq!(short.out, b"{");
        assert_eq!(sent.get(), 0);
        let sent = Cell::new(0);
        write_jena_graph_counted(&r, JenaFormat::RdfJson, Vec::new(), &sent).unwrap();
        assert_eq!(sent.get(), 1);
    }

    /// A writer that takes everything and fails on its flush.
    struct FailFlush(Vec<u8>);

    impl Write for FailFlush {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("flush failed"))
        }
    }

    #[test]
    fn rdf_json_counts_each_subject_the_writer_took() {
        use crate::io::{RdfFormat as In, Source};
        use crate::jena_formats::{JenaFormat, RdfWriter};
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<http://ex.org/a> <http://ex.org/p> \"1\" .\n\
                  <http://ex.org/a> <http://ex.org/p> \"2\"@en .\n\
                  <http://ex.org/a> <http://ex.org/q> _:b .\n\
                  _:b <http://ex.org/p> <http://ex.org/a> .\n"
                    .to_vec(),
                In::NTriples,
                None,
            )])
            .unwrap();
        let r = super::super::query(
            store.snapshot(),
            "CONSTRUCT WHERE { ?s ?p ?o }",
            &Default::default(),
        )
        .unwrap();
        // the document is the one `finish` writes from the whole map
        let mut whole = Vec::new();
        let sent = Cell::new(0);
        write_jena_graph_counted(&r, JenaFormat::RdfJson, &mut whole, &sent).unwrap();
        assert_eq!(sent.get(), 4);
        let mut plain = RdfWriter::new(JenaFormat::RdfJson, Vec::new());
        for t in &r.inner.triples {
            plain.triple(t).unwrap();
        }
        assert_eq!(plain.finish().unwrap(), whole);
        let doc: serde_json::Value = serde_json::from_slice(&whole).unwrap();
        assert_eq!(doc.as_object().unwrap().len(), 2);
        // a flush that fails after the whole document leaves every triple counted
        let mut late = FailFlush(Vec::new());
        let sent = Cell::new(0);
        assert!(write_jena_graph_counted(&r, JenaFormat::RdfJson, &mut late, &sent).is_err());
        assert_eq!(late.0, whole);
        assert_eq!(sent.get(), 4);
        // a writer that stops partway counts the subjects whose bytes it took: the blank
        // subject sorts first and has one triple
        let (key, preds) = doc.as_object().unwrap().iter().next().unwrap();
        assert!(key.starts_with("_:"));
        let first_end = 1 + serde_json::to_string(key).unwrap().len() + 1 + preds.to_string().len();
        for (room, counted) in [(first_end - 1, 0), (first_end, 1), (whole.len() - 1, 4)] {
            let mut w = Short {
                out: Vec::new(),
                step: 5,
                room,
            };
            let sent = Cell::new(0);
            assert!(write_jena_graph_counted(&r, JenaFormat::RdfJson, &mut w, &sent).is_err());
            assert_eq!(w.out, whole[..room]);
            assert_eq!(sent.get(), counted, "{room}");
        }
    }

    #[test]
    fn a_serializer_that_fails_leaves_what_it_wrote() {
        let mut out = Vec::new();
        let sent = Cell::new(0);
        let mut f = |w: &mut Buffered<'_>, staged: &Cell<u64>| -> Result<()> {
            w.write_all(b"row\n").map_err(io)?;
            staged.set(staged.get() + 1);
            w.write_all(b"half").map_err(io)?;
            Err(Error::invalid("stopped"))
        };
        assert!(write_buffered(&mut out, &sent, &mut f).is_err());
        assert_eq!(out, b"row\nhalf");
        assert_eq!(sent.get(), 1);
    }
}

#[cfg(test)]
mod output_boundary_tests;

#[cfg(test)]
mod chunk_ownership_tests;
