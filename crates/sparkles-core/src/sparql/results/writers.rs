//! One writer per result format family over the sources of [`super::source`], and the
//! entry that every caller writes a result through.

use super::encode::{Csv, Json, Tsv, Xml};
use super::rowout::Explicit;
use super::source::{
    BatchVisitor, CursorGraph, CursorSolutions, EagerGraph, EagerMode, EagerSolutions, GraphSource,
    NativeGraphLayout, RowBatch, RowView, SolutionSource, Statement, StatementVisitor,
};
use super::{
    NativeJsonMetadata, NativeTerm, RowCount, RowOut, Shared, SolutionsFormat, io, json,
    write_buffered,
};
use crate::error::{Error, Result};
use crate::jena_formats::{JenaFormat, ThriftResults};
use crate::sparql::{CursorStats, GraphCursor, QueryCursor, QueryKind, QueryResult};
use oxrdf::Variable;
use oxrdfio::RdfFormat;
use sparesults::{QueryResultsFormat, QueryResultsSerializer, WriterSolutionsSerializer};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::time::Instant;

/// What a result is written from.
pub enum Source<'a> {
    /// A complete result of any query form.
    Result(&'a QueryResult),
    /// A SELECT cursor.
    Select(&'a mut QueryCursor),
    /// A CONSTRUCT or DESCRIBE cursor.
    Graph(&'a mut GraphCursor),
}

/// The syntax a result is written in. [`SolutionsFormat::Sparkles`] is the native JSON
/// document for every query form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Solutions(SolutionsFormat),
    /// Jena's SPARQL Results Thrift (`application/sparql-results+thrift`), which holds
    /// SELECT results only. An ASK result is written in another format by the caller.
    ResultsThrift,
    Rdf(RdfFormat),
    Jena(crate::jena_formats::JenaFormat),
}

/// How a result is written.
pub struct WriteOptions<'a> {
    pub format: Format,
    /// the rows, or the statements of a cursor's graph, to write at most. A complete
    /// graph result is written whole in the RDF and Jena formats.
    pub send: Option<usize>,
    /// The namespace prefixes of a graph result, in order. A cursor declares those that
    /// are valid in every format that has prefixes. A complete result declares them in
    /// Turtle, TriG and RDF/XML only, and fails on an invalid one.
    pub prefixes: &'a [(String, String)],
    /// the native document's request metadata
    pub metadata: Option<NativeJsonMetadata<'a>>,
    /// counts the rows that reached the writer whole, and records their ends in its
    /// ledger when it has one
    pub rows: &'a RowCount,
}

impl<'a> WriteOptions<'a> {
    /// Options for `format` with no `send`, prefixes or metadata.
    pub fn new(format: Format, rows: &'a RowCount) -> Self {
        WriteOptions {
            format,
            send: None,
            prefixes: &[],
            metadata: None,
            rows,
        }
    }
}

/// Write `source` to `writer` as `options` say. The output goes through the writers'
/// buffer, so a writer that stops it partway, such as at the result-size budget, has
/// `options.rows` hold the rows of the output it took. A cursor's statistics are
/// returned after a whole output, and a cursor whose output fails is failed with the
/// error.
pub fn write(
    source: Source<'_>,
    options: WriteOptions<'_>,
    writer: &mut dyn Write,
) -> Result<Option<CursorStats>> {
    let WriteOptions {
        format,
        send,
        prefixes,
        metadata,
        rows,
    } = options;
    match (source, format) {
        (Source::Result(r), Format::Solutions(SolutionsFormat::Sparkles))
            if matches!(r.kind, QueryKind::Select | QueryKind::Ask) =>
        {
            let mut source = EagerSolutions {
                r,
                mode: EagerMode::Scratch,
            };
            solutions(&mut source, writer, rows, |s, out, staged| {
                native(s, out, staged, send, metadata)
            })
        }
        (Source::Result(r), Format::Solutions(SolutionsFormat::Sparkles)) => {
            graphs(&mut EagerGraph { r }, writer, rows, |s, out, staged| {
                native_graph(s, out, staged, send, metadata)
            })
        }
        (Source::Result(r), Format::Solutions(fmt)) => {
            let mut source = EagerSolutions {
                r,
                mode: EagerMode::Chunks,
            };
            solutions(&mut source, writer, rows, |s, out, staged| {
                select(s, fmt, out, staged, send)
            })
        }
        // a complete graph result is written whole in the RDF and Jena formats
        (Source::Result(r), Format::Rdf(fmt)) => {
            graphs(&mut EagerGraph { r }, writer, rows, |s, out, staged| {
                rdf(s, fmt, prefixes, out, staged, None)
            })
        }
        (Source::Result(r), Format::Jena(fmt)) => {
            graphs(&mut EagerGraph { r }, writer, rows, |s, out, staged| {
                jena(s, fmt, out, staged, None)
            })
        }
        (Source::Result(r), Format::ResultsThrift) if r.kind == QueryKind::Select => {
            // row by row into one scratch row with each row's control check and no
            // prefetch, as the server wrote it before
            let mut source = EagerSolutions {
                r,
                mode: EagerMode::Rows,
            };
            solutions(&mut source, writer, rows, |s, out, staged| {
                thrift(s, out, staged, send)
            })
        }
        (Source::Select(cursor), Format::ResultsThrift) => {
            let mut source = CursorSolutions { cursor };
            solutions(&mut source, writer, rows, |s, out, staged| {
                thrift(s, out, staged, send)
            })
        }
        (_, Format::ResultsThrift) => Err(Error::invalid(
            "SPARQL Results Thrift holds SELECT results only",
        )),
        (Source::Select(cursor), Format::Solutions(fmt)) => {
            let mut source = CursorSolutions { cursor };
            if fmt == SolutionsFormat::Sparkles {
                solutions(&mut source, writer, rows, |s, out, staged| {
                    native(s, out, staged, send, metadata)
                })
            } else {
                solutions(&mut source, writer, rows, |s, out, staged| {
                    select(s, fmt, out, staged, send)
                })
            }
        }
        (Source::Graph(cursor), Format::Solutions(SolutionsFormat::Sparkles)) => graphs(
            &mut CursorGraph { cursor },
            writer,
            rows,
            |s, out, staged| native_graph(s, out, staged, send, metadata),
        ),
        (Source::Graph(cursor), Format::Rdf(fmt)) => graphs(
            &mut CursorGraph { cursor },
            writer,
            rows,
            |s, out, staged| rdf(s, fmt, prefixes, out, staged, send),
        ),
        (Source::Graph(cursor), Format::Jena(fmt)) => graphs(
            &mut CursorGraph { cursor },
            writer,
            rows,
            |s, out, staged| jena(s, fmt, out, staged, send),
        ),
        (Source::Select(_), _) => Err(Error::invalid(
            "a SELECT cursor is written in a result format",
        )),
        (Source::Graph(_), Format::Solutions(_)) => Err(Error::invalid(
            "a graph cursor is written in an RDF syntax or the native document",
        )),
    }
}

/// Run `write` on `source` into the writers' buffer, failing the source with its error.
fn solutions<S: SolutionSource>(
    source: &mut S,
    writer: &mut dyn Write,
    rows: &RowCount,
    mut write: impl FnMut(&mut S, &mut RowOut<'_>, &Cell<u64>) -> Result<()>,
) -> Result<Option<CursorStats>> {
    let ctx = source.ctx();
    let result = write_buffered(writer, rows, Some(ctx), &mut |out, staged| {
        write(source, out, staged)
    });
    match result {
        Ok(()) => Ok(source.stats()),
        Err(error) => {
            source.fail_output(&error);
            Err(error)
        }
    }
}

/// The SPARQL result formats, through sparesults, from either source. ASK in CSV and TSV
/// is written as Jena writes it, a column `_askResult` that its clients read.
fn select<S: SolutionSource>(
    source: &mut S,
    fmt: SolutionsFormat,
    out: &mut RowOut<'_>,
    staged: &Cell<u64>,
    send: Option<usize>,
) -> Result<()> {
    source.start()?;
    let format = match fmt {
        SolutionsFormat::Json => QueryResultsFormat::Json,
        SolutionsFormat::Xml => QueryResultsFormat::Xml,
        SolutionsFormat::Csv => QueryResultsFormat::Csv,
        SolutionsFormat::Tsv => QueryResultsFormat::Tsv,
        SolutionsFormat::Sparkles => unreachable!("the native document has its own writer"),
    };
    let serializer = QueryResultsSerializer::from_format(format);
    if let Some(boolean) = source.boolean() {
        match fmt {
            SolutionsFormat::Csv => write!(out, "_askResult\r\n{boolean}\r\n").map_err(io)?,
            SolutionsFormat::Tsv => write!(out, "?_askResult\n{boolean}\n").map_err(io)?,
            _ => {
                serializer
                    .serialize_boolean_to_writer(out, boolean)
                    .map_err(io)?;
            }
        }
        return Ok(());
    }
    match fmt {
        SolutionsFormat::Tsv => return encoded(source, out, send, Tsv::header, Tsv::row, None),
        SolutionsFormat::Csv => return encoded(source, out, send, Csv::header, Csv::row, None),
        SolutionsFormat::Json => {
            let mut json = Json::new(out.ctx(), source.variables())?;
            return encoded(
                source,
                out,
                send,
                Json::header,
                |out: &mut Explicit<'_, '_>, row: RowView<'_>, width| json.row(out, row, width),
                Some(Json::footer),
            );
        }
        SolutionsFormat::Xml => {
            let mut xml = Xml::new(out.ctx(), source.variables())?;
            return encoded(
                source,
                out,
                send,
                Xml::header,
                |out: &mut Explicit<'_, '_>, row: RowView<'_>, width| xml.row(out, row, width),
                Some(Xml::footer),
            );
        }
        _ => {}
    }
    let vars: Vec<Variable> = source
        .variables()
        .iter()
        .map(|v| Variable::new_unchecked(v.clone()))
        .collect();
    if S::FLUSH_FIRST {
        // the serializer holds the writer, and the first batch's flush goes past it
        let shared = RefCell::new(out);
        let mut writer = Shared(&shared);
        let mut rows = SelectRows {
            serializer: serializer
                .serialize_solutions_to_writer(&mut writer, vars.clone())
                .map_err(io)?,
            vars: &vars,
            staged,
            flush: Some(|| shared.borrow_mut().flush()),
        };
        source.drive(send, &mut rows)?;
        rows.serializer.finish().map_err(io)?;
    } else {
        let mut rows = SelectRows {
            serializer: serializer
                .serialize_solutions_to_writer(out, vars.clone())
                .map_err(io)?,
            vars: &vars,
            staged,
            flush: None::<fn() -> std::io::Result<()>>,
        };
        source.drive(send, &mut rows)?;
        rows.serializer.finish().map_err(io)?;
    }
    Ok(())
}

/// A SELECT in a format the engine encodes, its header, its rows, each marked in the
/// explicit mode, and its footer. The row encoder is a function item or a closure, so each
/// format compiles its own loop.
fn encoded<S: SolutionSource, R>(
    source: &mut S,
    out: &mut RowOut<'_>,
    send: Option<usize>,
    header: fn(&mut Explicit<'_, '_>, &[String]) -> std::io::Result<()>,
    row: R,
    footer: Option<fn(&mut Explicit<'_, '_>) -> std::io::Result<()>>,
) -> Result<()>
where
    R: FnMut(&mut Explicit<'_, '_>, RowView<'_>, usize) -> std::io::Result<()>,
{
    let width = source.variables().len();
    let mut out = Explicit::new(out);
    header(&mut out, source.variables()).map_err(io)?;
    let mut rows = EncodedRows {
        out,
        width,
        flush: S::FLUSH_FIRST,
        row,
    };
    source.drive(send, &mut rows)?;
    if let Some(footer) = footer {
        footer(&mut rows.out).map_err(io)?;
    }
    Ok(())
}

/// The rows of a format the engine encodes, each marked in the explicit mode.
struct EncodedRows<'o, 'a, R> {
    out: Explicit<'o, 'a>,
    width: usize,
    /// flush after the first batch
    flush: bool,
    row: R,
}

impl<R> BatchVisitor for EncodedRows<'_, '_, R>
where
    R: FnMut(&mut Explicit<'_, '_>, RowView<'_>, usize) -> std::io::Result<()>,
{
    fn batch<B: RowBatch>(&mut self, batch: &mut B) -> Result<()> {
        for i in 0..batch.len() {
            let row = batch.row(i)?;
            self.out.begin_row()?;
            (self.row)(&mut self.out, row, self.width).map_err(io)?;
            self.out.end_row(1).map_err(io)?;
        }
        if std::mem::take(&mut self.flush) {
            self.out.flush().map_err(io)?;
        }
        Ok(())
    }
}

struct SelectRows<'v, W: Write, F> {
    serializer: WriterSolutionsSerializer<W>,
    vars: &'v [Variable],
    staged: &'v Cell<u64>,
    /// the flush after the first batch
    flush: Option<F>,
}

impl<W: Write, F: FnOnce() -> std::io::Result<()>> BatchVisitor for SelectRows<'_, W, F> {
    fn batch<B: RowBatch>(&mut self, batch: &mut B) -> Result<()> {
        let vars = self.vars;
        for i in 0..batch.len() {
            match batch.row(i)? {
                RowView::Dense(terms) => {
                    self.serializer
                        .serialize(terms.iter().enumerate().filter_map(|(c, term)| {
                            term.as_ref().map(|t| (vars[c].as_ref(), t.as_ref()))
                        }))
                }
                RowView::Sparse(terms) => self.serializer.serialize(
                    terms
                        .iter()
                        .map(|(c, term)| (vars[*c].as_ref(), term.as_ref().as_ref())),
                ),
            }
            .map_err(io)?;
            self.staged.set(self.staged.get() + 1);
        }
        if let Some(flush) = self.flush.take() {
            flush().map_err(io)?;
        }
        Ok(())
    }
}

/// SPARQL Results Thrift of a SELECT, from either source: the variables, then a data
/// tuple per row.
fn thrift<S: SolutionSource>(
    source: &mut S,
    out: &mut RowOut<'_>,
    staged: &Cell<u64>,
    send: Option<usize>,
) -> Result<()> {
    source.start()?;
    let mut rows = ThriftRows {
        width: source.variables().len(),
        out: ThriftResults::new(&mut *out, source.variables()).map_err(io)?,
        staged,
        flush: S::FLUSH_FIRST,
    };
    source.drive(send, &mut rows)?;
    rows.out.finish().map_err(io)?;
    Ok(())
}

struct ThriftRows<'o, W: Write> {
    out: ThriftResults<W>,
    staged: &'o Cell<u64>,
    width: usize,
    /// flush after the first batch
    flush: bool,
}

impl<W: Write> BatchVisitor for ThriftRows<'_, W> {
    fn batch<B: RowBatch>(&mut self, batch: &mut B) -> Result<()> {
        for i in 0..batch.len() {
            match batch.row(i)? {
                RowView::Dense(terms) => self.out.row(terms),
                RowView::Sparse(terms) => {
                    let mut bound = terms.iter().peekable();
                    self.out.row_cells(
                        self.width,
                        (0..self.width)
                            .map(|c| bound.next_if(|(col, _)| *col == c).map(|(_, t)| t.as_ref())),
                    )
                }
            }
            .map_err(io)?;
            self.staged.set(self.staged.get() + 1);
        }
        // the first batch's rows reach a writer that commits on a flush
        if std::mem::take(&mut self.flush) {
            self.out.get_mut().flush().map_err(io)?;
        }
        Ok(())
    }
}

/// The native JSON document of a SELECT or ASK, from either source. Its rows are
/// written by hand, cell by cell, and the source writes its own `meta` member.
fn native<S: SolutionSource>(
    source: &mut S,
    out: &mut RowOut<'_>,
    staged: &Cell<u64>,
    send: Option<usize>,
    metadata: Option<NativeJsonMetadata<'_>>,
) -> Result<()> {
    let started = Instant::now();
    let initial_exec = source.exec_ms();
    source.start()?;
    if let Some(boolean) = source.boolean() {
        out.write_all(b"{\"queryType\":\"ASK\",\"boolean\":")?;
        out.write_all(if boolean { b"true" } else { b"false" })?;
        return source.native_trailer(out, None, send, metadata, started, initial_exec);
    }
    out.write_all(b"{\"queryType\":\"SELECT\",\"vars\":")?;
    json(&mut *out, &source.variables())?;
    out.write_all(b",\"rows\":[")?;
    let mut rows = NativeRows {
        width: source.variables().len(),
        out: &mut *out,
        staged,
        sent: 0,
        flush: S::FLUSH_FIRST,
    };
    source.drive(send, &mut rows)?;
    let sent = rows.sent;
    out.write_all(b"]")?;
    source.native_trailer(out, Some(sent), send, metadata, started, initial_exec)
}

struct NativeRows<'o, 'w> {
    out: &'o mut RowOut<'w>,
    staged: &'o Cell<u64>,
    width: usize,
    sent: u64,
    /// flush after the first batch
    flush: bool,
}

impl BatchVisitor for NativeRows<'_, '_> {
    fn batch<B: RowBatch>(&mut self, batch: &mut B) -> Result<()> {
        let out = &mut *self.out;
        for i in 0..batch.len() {
            let row = batch.row(i)?;
            if self.sent > 0 {
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
                    for c in 0..self.width {
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
            self.staged.set(self.staged.get() + 1);
            self.sent += 1;
        }
        // the first batch's rows reach a writer that commits on a flush
        if std::mem::take(&mut self.flush) {
            out.flush()?;
        }
        Ok(())
    }
}

/// Run `write` on a graph source into the writers' buffer, on the source's stack, failing
/// the source with its error.
fn graphs<S: GraphSource>(
    source: &mut S,
    writer: &mut dyn Write,
    rows: &RowCount,
    mut write: impl FnMut(&mut S, &mut RowOut<'_>, &Cell<u64>) -> Result<()>,
) -> Result<Option<CursorStats>> {
    let ctx = source.ctx();
    let depth = source.stack_depth();
    let result = write_buffered(writer, rows, Some(ctx), &mut |out, staged| match depth {
        Some(depth) => crate::sparql::depth::with_stack(depth, || write(source, out, staged)),
        None => write(source, out, staged),
    });
    match result {
        Ok(()) => Ok(source.stats()),
        Err(error) => {
            source.fail_output(&error);
            Err(error)
        }
    }
}

/// The oxrdfio formats from either source. A format without datasets leaves out the
/// named-graph quads, as Fuseki writes it.
fn rdf<S: GraphSource>(
    source: &mut S,
    format: RdfFormat,
    prefixes: &[(String, String)],
    out: &mut RowOut<'_>,
    staged: &Cell<u64>,
    send: Option<usize>,
) -> Result<()> {
    let _charge = source.charge(
        (128 << 10)
            + prefixes
                .iter()
                .map(|(a, b)| (a.len() + b.len()) as u64 * 4 + 128)
                .sum::<u64>(),
    )?;
    let serializer = source.serializer(format, prefixes)?;
    let datasets = format.supports_datasets();
    if S::FLUSH_FIRST {
        // the serializer holds the writer, and the first batch's flush goes past it
        let shared = RefCell::new(out);
        let mut rows = RdfRows {
            writer: serializer.for_writer(Shared(&shared)),
            datasets,
            staged,
            flush: Some(|| shared.borrow_mut().flush()),
        };
        source.drive(send, &mut rows)?;
        rows.writer.finish().map_err(io)?;
    } else {
        let mut rows = RdfRows {
            writer: serializer.for_writer(out),
            datasets,
            staged,
            flush: None::<fn() -> std::io::Result<()>>,
        };
        source.drive(send, &mut rows)?;
        rows.writer.finish().map_err(io)?;
    }
    Ok(())
}

struct RdfRows<'v, W: Write, F> {
    writer: oxrdfio::WriterQuadSerializer<W>,
    datasets: bool,
    staged: &'v Cell<u64>,
    /// the flush after the first batch
    flush: Option<F>,
}

impl<W: Write, F: FnOnce() -> std::io::Result<()>> StatementVisitor for RdfRows<'_, W, F> {
    #[inline]
    fn statement(&mut self, statement: Statement<'_>) -> Result<()> {
        match statement {
            Statement::Triple(t) => self.writer.serialize_triple(t),
            Statement::Quad(q) if self.datasets || q.graph_name.is_default_graph() => {
                self.writer.serialize_quad(q)
            }
            Statement::Quad(_) => return Ok(()),
        }
        .map_err(io)?;
        self.staged.set(self.staged.get() + 1);
        Ok(())
    }

    fn first_batch(&mut self) -> Result<()> {
        match self.flush.take() {
            Some(flush) => flush().map_err(io),
            None => Ok(()),
        }
    }
}

/// Jena's syntaxes from either source. TriX, RDF Thrift and RDF Protobuf also hold the
/// quads, each statement in the message of its own kind, and RDF/JSON, which holds one
/// graph, the default graph only.
fn jena<S: GraphSource>(
    source: &mut S,
    format: JenaFormat,
    out: &mut RowOut<'_>,
    staged: &Cell<u64>,
    send: Option<usize>,
) -> Result<()> {
    let charge = source.charge(128 << 10)?;
    // RDF/JSON groups the triples by subject and writes them all at the end, so they
    // count by subject as `finish_counted` writes them
    let deferred = format == JenaFormat::RdfJson;
    if S::FLUSH_FIRST {
        // the writer holds the output, and the first batch's flush goes past it
        let shared = RefCell::new(out);
        let mut rows = JenaRows::<S, _, _> {
            writer: crate::jena_formats::RdfWriter::new(format, Shared(&shared)),
            format,
            deferred,
            charge,
            staged,
            flush: Some(|| shared.borrow_mut().flush()),
        };
        source.drive(send, &mut rows)?;
        rows.writer
            .finish_counted(|n| staged.set(staged.get() + n))
            .map_err(io)?;
    } else {
        let mut rows = JenaRows::<S, _, _> {
            writer: crate::jena_formats::RdfWriter::new(format, out),
            format,
            deferred,
            charge,
            staged,
            flush: None::<fn() -> std::io::Result<()>>,
        };
        source.drive(send, &mut rows)?;
        rows.writer
            .finish_counted(|n| staged.set(staged.get() + n))
            .map_err(io)?;
    }
    Ok(())
}

struct JenaRows<'v, S: GraphSource, W: Write, F> {
    writer: crate::jena_formats::RdfWriter<W>,
    format: JenaFormat,
    deferred: bool,
    charge: S::Charge,
    staged: &'v Cell<u64>,
    /// the flush after the first batch
    flush: Option<F>,
}

impl<S: GraphSource, W: Write, F: FnOnce() -> std::io::Result<()>> StatementVisitor
    for JenaRows<'_, S, W, F>
{
    #[inline]
    fn statement(&mut self, statement: Statement<'_>) -> Result<()> {
        if self.format == JenaFormat::RdfJson {
            S::keep(&mut self.charge, statement)?;
        }
        match statement {
            Statement::Triple(t) => self.writer.triple(t).map_err(io)?,
            Statement::Quad(q) if self.format.quads() => self.writer.quad(q).map_err(io)?,
            Statement::Quad(q) if q.graph_name.is_default_graph() => self
                .writer
                .triple(&oxrdf::Triple::new(
                    q.subject.clone(),
                    q.predicate.clone(),
                    q.object.clone(),
                ))
                .map_err(io)?,
            Statement::Quad(_) => return Ok(()),
        }
        if !self.deferred {
            self.staged.set(self.staged.get() + 1);
        }
        Ok(())
    }

    fn first_batch(&mut self) -> Result<()> {
        match self.flush.take() {
            Some(flush) => flush().map_err(io),
            None => Ok(()),
        }
    }
}

/// The native document of a CONSTRUCT or DESCRIBE from either source, in the source's
/// layout. Its statements are written by hand, cell by cell.
fn native_graph<S: GraphSource>(
    source: &mut S,
    out: &mut RowOut<'_>,
    staged: &Cell<u64>,
    send: Option<usize>,
    metadata: Option<NativeJsonMetadata<'_>>,
) -> Result<()> {
    let started = Instant::now();
    source.native_start()?;
    let _charge = source.charge(4096)?;
    let layout = source.native_layout();
    out.write_all(b"{\"queryType\":")?;
    json(&mut *out, &source.kind())?;
    out.write_all(match layout {
        NativeGraphLayout::Split { .. } => b",\"triples\":[",
        NativeGraphLayout::Quads => b",\"triples\":[],\"quads\":[",
    })?;
    let mut rows = NativeGraphRows {
        out: &mut *out,
        staged,
        in_triples: matches!(layout, NativeGraphLayout::Split { .. }),
        section: 0,
        sent: 0,
        flush: S::FLUSH_FIRST,
    };
    source.drive(send, &mut rows)?;
    let (sent, in_triples) = (rows.sent, rows.in_triples);
    out.write_all(b"]")?;
    if in_triples && layout == (NativeGraphLayout::Split { quads: true }) {
        out.write_all(b",\"quads\":[]")?;
    }
    source.native_trailer(out, sent, send, metadata, started)
}

struct NativeGraphRows<'o, 'w> {
    out: &'o mut RowOut<'w>,
    staged: &'o Cell<u64>,
    /// the statements go into `triples`, until the first quad of a split layout
    in_triples: bool,
    /// the statements written into the open array
    section: u64,
    sent: u64,
    /// flush after the first batch
    flush: bool,
}

impl StatementVisitor for NativeGraphRows<'_, '_> {
    #[inline]
    fn statement(&mut self, statement: Statement<'_>) -> Result<()> {
        let out = &mut *self.out;
        match statement {
            Statement::Triple(t) => {
                if self.section > 0 {
                    out.write_all(b",")?;
                }
                let t = t.as_ref();
                if self.in_triples {
                    json(
                        &mut *out,
                        &[
                            NativeTerm(t.subject.into()),
                            NativeTerm(t.predicate.into()),
                            NativeTerm(t.object),
                        ],
                    )?;
                } else {
                    json(
                        &mut *out,
                        &[
                            Some(NativeTerm(t.subject.into())),
                            Some(NativeTerm(t.predicate.into())),
                            Some(NativeTerm(t.object)),
                            None,
                        ],
                    )?;
                }
            }
            Statement::Quad(q) => {
                if std::mem::take(&mut self.in_triples) {
                    out.write_all(b"],\"quads\":[")?;
                    self.section = 0;
                }
                if self.section > 0 {
                    out.write_all(b",")?;
                }
                let q = q.as_ref();
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
                )?;
            }
        }
        self.staged.set(self.staged.get() + 1);
        self.section += 1;
        self.sent += 1;
        Ok(())
    }

    fn first_batch(&mut self) -> Result<()> {
        // the first batch's statements reach a writer that commits on a flush
        if std::mem::take(&mut self.flush) {
            self.out.flush()?;
        }
        Ok(())
    }
}
