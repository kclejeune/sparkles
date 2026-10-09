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

pub fn rdf_format_from_name(s: &str) -> Option<RdfFormat> {
    let base = s.split(';').next()?.trim().to_ascii_lowercase();
    Some(match base.as_str() {
        "text/turtle" | "turtle" | "ttl" => RdfFormat::Turtle,
        "application/n-triples" | "ntriples" | "nt" | "text/plain" => RdfFormat::NTriples,
        "application/n-quads" | "nquads" | "nq" => RdfFormat::NQuads,
        "application/trig" | "trig" => RdfFormat::TriG,
        "application/rdf+xml" | "rdfxml" | "xml" => RdfFormat::RdfXml,
        "application/ld+json" => return crate::io::format_for_media_type(s),
        "jsonld" | "json-ld" => RdfFormat::JsonLd {
            profile: oxrdfio::JsonLdProfileSet::empty(),
        },
        "jsonld-streaming" | "json-ld-streaming" => RdfFormat::JsonLd {
            profile: oxrdfio::JsonLdProfile::Streaming.into(),
        },
        _ => return None,
    })
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

/// Consume SELECT batches directly, applying writer backpressure before requesting
/// the next batch. A later error leaves partial bytes and no success terminator.
pub fn write_cursor_solutions(
    cursor: &mut super::QueryCursor,
    fmt: SolutionsFormat,
    mut writer: impl Write,
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
    let result = (|| {
        cursor.check()?;
        let mut serializer = QueryResultsSerializer::from_format(format)
            .serialize_solutions_to_writer(&mut writer, variables.clone())
            .map_err(io)?;
        let mut remaining = send.unwrap_or(usize::MAX);
        while remaining > 0 {
            let Some(batch) = cursor.next_batch_at_most(remaining)? else {
                break;
            };
            let decoded = batch.decoded()?;
            let _row_storage = cursor.charge(
                batch.width() as u64 * std::mem::size_of::<(usize, Cow<'_, Term>)>() as u64 + 128,
            )?;
            let mut terms = Vec::with_capacity(batch.width());
            for row in 0..batch.len().min(remaining) {
                if row.is_multiple_of(1024) {
                    cursor.check()?;
                }
                let bytes = match &decoded {
                    Some(d) => d.row_bytes(row)?,
                    None => batch.row_bytes(row)?,
                };
                let _charge = (bytes != 0).then(|| cursor.charge(bytes)).transpose()?;
                for col in 0..batch.width() {
                    let term = match &decoded {
                        Some(d) => d.term(row, col),
                        None => Ok(batch.term(row, col)?.map(Cow::Owned)),
                    }?;
                    if let Some(term) = term {
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
                remaining -= 1;
            }
        }
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
    mut w: impl Write,
    send: Option<usize>,
) -> Result<()> {
    if fmt == SolutionsFormat::Sparkles {
        return write_native_json(r, w, send, None);
    }
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
        .vars
        .iter()
        .map(|v| Variable::new_unchecked(v.clone()))
        .collect();
    let mut s = ser
        .serialize_solutions_to_writer(w, vars.clone())
        .map_err(io)?;
    let n = send.map_or(r.table.len(), |s| s.min(r.table.len()));
    if n <= DECODE_ROWS {
        // a result of one chunk is decoded row by row: under concurrent load that keeps
        // less memory live per request than a chunk's table of terms, and in the 10.5M
        // benchmark it served 25% more requests a second
        // Medium answers can contain thousands of scattered vocabulary pages. Keep
        // row-by-row decoding's small working set, but overlap those cold reads.
        if n >= 256 && crate::index::io_hints() {
            prefetch_answer(r, n);
        }
        for i in 0..n {
            let terms: Vec<(usize, Term)> = r
                .table
                .cols
                .iter()
                .enumerate()
                .filter_map(|(c, col)| r.term(col[i]).map(|t| (c, t)))
                .collect();
            s.serialize(terms.iter().map(|(c, t)| (vars[*c].as_ref(), t.as_ref())))
                .map_err(io)?;
        }
        s.finish().map_err(io)?;
        return Ok(());
    }
    let chunk = |start: usize| start..n.min(start + DECODE_ROWS);
    std::thread::scope(|sc| -> Result<()> {
        let mut ids = Decoded::cells(r, chunk(0), true);
        let mut start = 0;
        while start < n {
            let end = n.min(start + DECODE_ROWS);
            let d = Decoded::new(r, std::mem::take(&mut ids));
            // the next chunk's pages are asked for while this one is written
            let next = (end < n).then(|| sc.spawn(move || Decoded::cells(r, chunk(end), true)));
            for i in start..end {
                let terms: Vec<(usize, Cow<'_, Term>)> = r
                    .table
                    .cols
                    .iter()
                    .enumerate()
                    .filter_map(|(c, col)| d.term(r, c, i, col[i]).map(|t| (c, t)))
                    .collect();
                s.serialize(
                    terms
                        .iter()
                        .map(|(c, t)| (vars[*c].as_ref(), t.as_ref().as_ref())),
                )
                .map_err(io)?;
            }
            if let Some(h) = next {
                ids = h
                    .join()
                    .unwrap_or_else(|_| Decoded::cells(r, chunk(end), false));
            }
            start = end;
        }
        Ok(())
    })?;
    s.finish().map_err(io)?;
    Ok(())
}

// Keep prefetch preparation out of the generic serializer's row loop and stack frame.
#[inline(never)]
fn prefetch_answer(r: &QueryResult, n: usize) {
    let ids = r.table.cols.iter().flat_map(|col| {
        col[..n]
            .iter()
            .filter(|id| id.tag() == Tag::Vocab)
            .map(|id| id.payload())
    });
    r.ctx.snap.generation.vocab.prefetch_terms(ids);
}

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
    /// the first row and the rows of the chunk
    start: usize,
    rows: usize,
    /// the distinct terms, in id order
    terms: Vec<Term>,
    /// per cell (column-major: column `c`, row `i` at `c * rows + i - start`), the index of
    /// its term, `u32::MAX` for an id outside the base vocabulary
    slot: Vec<u32>,
}

/// The base-vocabulary cells of a chunk of rows, sorted by id: `(id, cell)`.
#[derive(Default)]
struct Cells {
    start: usize,
    rows: usize,
    pairs: Vec<(u64, u32)>,
    prefetched: bool,
}

impl Decoded {
    /// The base-vocabulary cells of `rows`, sorted by id, with the pages of their terms
    /// asked for when `ahead`.
    fn cells(r: &QueryResult, rows: std::ops::Range<usize>, ahead: bool) -> Cells {
        use rayon::prelude::*;
        let n = rows.len();
        let mut pairs: Vec<(u64, u32)> = Vec::new();
        for (c, col) in r.table.cols.iter().enumerate() {
            for (j, id) in col[rows.clone()].iter().enumerate() {
                if id.tag() == Tag::Vocab {
                    pairs.push((id.payload(), (c * n + j) as u32));
                }
            }
        }
        if pairs.len() < PAR_DECODE_MIN {
            pairs.sort_unstable();
        } else {
            pairs.par_sort_unstable();
        }
        if ahead {
            let mut ids: Vec<u64> = pairs.iter().map(|p| p.0).collect();
            ids.dedup();
            r.ctx.snap.generation.vocab.prefetch_sorted(&ids);
        }
        Cells {
            start: rows.start,
            rows: n,
            pairs,
            prefetched: ahead,
        }
    }

    /// Decode the distinct ids of `cells`.
    fn new(r: &QueryResult, cells: Cells) -> Decoded {
        use rayon::prelude::*;
        let mut slot = vec![u32::MAX; r.table.cols.len() * cells.rows];
        let mut ids: Vec<u64> = Vec::new();
        for &(id, cell) in &cells.pairs {
            if ids.last() != Some(&id) {
                ids.push(id);
            }
            slot[cell as usize] = (ids.len() - 1) as u32;
        }
        let vocab = &r.ctx.snap.generation.vocab;
        let ahead = !cells.prefetched;
        // ids past the vocabulary (none in a consistent snapshot) end the decoding: they
        // are a suffix, whose cells fall back to decoding one by one
        let terms = if ids.len() < PAR_DECODE_MIN {
            let mut terms = Vec::with_capacity(ids.len());
            vocab.get_sorted_with_ahead(&ids, |_, k| terms.push(crate::id::key_to_term(k)), ahead);
            terms
        } else {
            ids.par_chunks(DECODE_TASK)
                .flat_map_iter(|c| {
                    let mut out = Vec::with_capacity(c.len());
                    vocab.get_sorted_with_ahead(
                        c,
                        |_, k| out.push(crate::id::key_to_term(k)),
                        ahead,
                    );
                    out
                })
                .collect()
        };
        Decoded {
            start: cells.start,
            rows: cells.rows,
            terms,
            slot,
        }
    }

    /// The term of id `id` in column `c` of row `i`.
    fn term<'a>(&'a self, r: &QueryResult, c: usize, i: usize, id: Id) -> Option<Cow<'a, Term>> {
        match self.slot[c * self.rows + i - self.start] {
            s if (s as usize) < self.terms.len() => Some(Cow::Borrowed(&self.terms[s as usize])),
            _ => r.term(id).map(Cow::Owned),
        }
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
    let mut ser = RdfSerializer::from_format(fmt);
    if matches!(fmt, RdfFormat::Turtle | RdfFormat::TriG | RdfFormat::RdfXml) {
        for (p, ns) in prefixes {
            ser = ser
                .with_prefix(p.clone(), ns.clone())
                .map_err(|e| Error::invalid(e.to_string()))?;
        }
    }
    let mut s = ser.for_writer(w);
    for t in &r.triples {
        s.serialize_triple(t).map_err(io)?;
    }
    if fmt.supports_datasets() {
        for q in &r.quads {
            s.serialize_quad(q).map_err(io)?;
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
    let mut out = crate::jena_formats::RdfWriter::new(fmt, w);
    for t in &r.triples {
        out.triple(t).map_err(io)?;
    }
    if fmt.quads() {
        for q in &r.quads {
            out.quad(q).map_err(io)?;
        }
    }
    out.finish().map_err(io)?;
    Ok(())
}

/// Extra metadata for an HTTP native response. Its timing includes serialization
/// through the result rows; metadata is written last so that no document must be
/// retained just to patch the timing.
pub struct NativeJsonMetadata<'a> {
    pub commit: u64,
    pub dataset_id: &'a str,
}

/// Write the rich native result without building an intermediate JSON document.
/// SELECT decodes one cell at a time; graph results and plan metadata are borrowed.
/// The query's underlying result tables/graphs are already materialized.
pub fn write_native_json(
    r: &QueryResult,
    w: impl Write,
    send: Option<usize>,
    metadata: Option<NativeJsonMetadata<'_>>,
) -> Result<()> {
    let n = send.map_or(r.len(), |s| s.min(r.len()));
    serde_json::to_writer(
        w,
        &NativeJson {
            r,
            n,
            metadata,
            started: Instant::now(),
        },
    )
    .map_err(|e| Error::Io(e.into()))
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
        let mut remaining = send.unwrap_or(usize::MAX);
        let mut sent = 0u64;
        while remaining > 0 {
            let Some(batch) = cursor.next_batch_at_most(remaining)? else {
                break;
            };
            let decoded = batch.decoded()?;
            for row in 0..batch.len() {
                if row.is_multiple_of(1024) {
                    cursor.check()?;
                }
                if sent > 0 {
                    writer.write_all(b",")?;
                }
                writer.write_all(b"[")?;
                // One reservation covers the row's uncached cells through their
                // serialization, as in the other result formats.
                let bytes = match &decoded {
                    Some(d) => d.row_bytes(row)?,
                    None => batch.row_bytes(row)?,
                };
                let _charge = (bytes != 0).then(|| cursor.charge(bytes)).transpose()?;
                for col in 0..batch.width() {
                    if col > 0 {
                        writer.write_all(b",")?;
                    }
                    let term = match &decoded {
                        Some(d) => d.term(row, col)?,
                        None => batch.term(row, col)?.map(Cow::Owned),
                    };
                    json(
                        &mut writer,
                        &term.as_ref().map(|t| NativeTerm(t.as_ref().as_ref())),
                    )?;
                }
                writer.write_all(b"]")?;
                sent += 1;
                remaining -= 1;
            }
        }
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
                    },
                    rows_produced: stats.rows_produced,
                    commit: metadata.as_ref().map(|m| m.commit),
                    dataset_id: metadata.as_ref().map(|m| m.dataset_id),
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
        write!(
            writer,
            ",\"memory\":{{\"peakBytes\":{}}},\"rowsProduced\":{},\"describeTruncated\":{}",
            stats.mem_peak_bytes,
            stats.rows_produced,
            cursor.describe_truncated()
        )?;
        if let Some(meta) = metadata {
            write!(writer, ",\"commit\":{},\"datasetId\":", meta.commit)?;
            serde_json::to_writer(&mut writer, meta.dataset_id).map_err(|e| Error::Io(e.into()))?;
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
        let mut writer = crate::jena_formats::RdfWriter::new(format, output);
        let mut sent = 0usize;
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
    metadata: Option<NativeJsonMetadata<'a>>,
    started: Instant,
}

impl Serialize for NativeJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let r = self.r;
        let mut map = s.serialize_map(None)?;
        map.serialize_entry("queryType", &r.kind)?;
        match r.kind {
            QueryKind::Select => {
                map.serialize_entry("vars", &r.vars)?;
                map.serialize_entry("rows", &NativeRows { r, n: self.n })?;
            }
            QueryKind::Ask => map.serialize_entry("boolean", &r.boolean)?,
            _ => {
                map.serialize_entry(
                    "triples",
                    &NativeGraphRows::Triples(&r.triples[..self.n.min(r.triples.len())]),
                )?;
                if !r.quads.is_empty() {
                    let n = self.n.saturating_sub(r.triples.len()).min(r.quads.len());
                    map.serialize_entry("quads", &NativeGraphRows::Quads(&r.quads[..n]))?;
                }
            }
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
                plan: &r.plan,
                memory: NativeMemory {
                    peak_bytes: r.mem_peak_bytes,
                },
                rows_produced: r.rows_produced,
                commit: self.metadata.as_ref().map(|m| m.commit),
                dataset_id: self.metadata.as_ref().map(|m| m.dataset_id),
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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeMemory {
    peak_bytes: u64,
}

struct NativeRows<'a> {
    r: &'a QueryResult,
    n: usize,
}

impl Serialize for NativeRows<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.n))?;
        for i in 0..self.n {
            seq.serialize_element(&NativeRow { r: self.r, i })?;
        }
        seq.end()
    }
}

struct NativeRow<'a> {
    r: &'a QueryResult,
    i: usize,
}

impl Serialize for NativeRow<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.r.table.width()))?;
        for col in &self.r.table.cols {
            let term = self.r.term(col[self.i]);
            seq.serialize_element(&term.as_ref().map(|t| NativeTerm(t.as_ref())))?;
        }
        seq.end()
    }
}

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

enum NativeGraphRows<'a> {
    Triples(&'a [oxrdf::Triple]),
    Quads(&'a [oxrdf::Quad]),
}

impl Serialize for NativeGraphRows<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(match self {
            Self::Triples(rows) => rows.len(),
            Self::Quads(rows) => rows.len(),
        }))?;
        match self {
            Self::Triples(rows) => {
                for row in *rows {
                    let t = row.as_ref();
                    seq.serialize_element(&[
                        NativeTerm(t.subject.into()),
                        NativeTerm(t.predicate.into()),
                        NativeTerm(t.object),
                    ])?;
                }
            }
            Self::Quads(rows) => {
                for row in *rows {
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

/// The `application/x-sparkles+json` document (see docs/API.md).
pub fn sparkles_json(r: &QueryResult, send: Option<usize>) -> J {
    let total = r.len();
    let n = send.map_or(total, |s| s.min(total));
    let mut out = serde_json::Map::new();
    out.insert("queryType".into(), serde_json::to_value(r.kind).unwrap());
    match r.kind {
        QueryKind::Select => {
            out.insert("vars".into(), json!(r.vars));
            let rows: Vec<J> = (0..n)
                .map(|i| {
                    J::Array(
                        r.table
                            .cols
                            .iter()
                            .map(|c| r.term(c[i]).map_or(J::Null, |t| term_json(&t)))
                            .collect(),
                    )
                })
                .collect();
            out.insert("rows".into(), J::Array(rows));
        }
        QueryKind::Ask => {
            out.insert("boolean".into(), r.boolean.into());
        }
        _ => {
            let triples: Vec<J> = r
                .triples
                .iter()
                .take(n)
                .map(|t| {
                    json!([
                        term_json(&Term::from(t.subject.clone())),
                        term_json(&Term::NamedNode(t.predicate.clone())),
                        term_json(&t.object)
                    ])
                })
                .collect();
            out.insert("triples".into(), J::Array(triples));
            if !r.quads.is_empty() {
                // what the row budget leaves after the triples
                let quads: Vec<J> = r
                    .quads
                    .iter()
                    .take(n.saturating_sub(r.triples.len()))
                    .map(|q| {
                        json!([
                            term_json(&Term::from(q.subject.clone())),
                            term_json(&Term::NamedNode(q.predicate.clone())),
                            term_json(&q.object),
                            match &q.graph_name {
                                oxrdf::GraphName::NamedNode(g) =>
                                    term_json(&Term::NamedNode(g.clone())),
                                oxrdf::GraphName::BlankNode(b) =>
                                    term_json(&Term::BlankNode(b.clone())),
                                oxrdf::GraphName::DefaultGraph => J::Null,
                            }
                        ])
                    })
                    .collect();
                out.insert("quads".into(), J::Array(quads));
            }
        }
    }
    out.insert(
        "meta".into(),
        json!({
            "totalRows": total,
            "sentRows": n,
            "timing": r.timing,
            "plan": r.plan,
            "memory": { "peakBytes": r.mem_peak_bytes },
            "rowsProduced": r.rows_produced,
        }),
    );
    J::Object(out)
}
