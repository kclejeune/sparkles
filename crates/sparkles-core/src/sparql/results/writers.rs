//! One writer per result format family over the sources of [`super::source`], and the
//! entry that every caller writes a result through.

use super::source::{
    BatchVisitor, CursorSolutions, EagerSolutions, RowBatch, RowView, SolutionSource,
};
use super::{
    NativeJsonMetadata, NativeTerm, RowCount, RowOut, Shared, SolutionsFormat, io, json,
    write_buffered,
};
use crate::error::{Error, Result};
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
            let mut source = EagerSolutions { r, scratch: true };
            solutions(&mut source, writer, rows, |s, out, staged| {
                native(s, out, staged, send, metadata)
            })
        }
        (Source::Result(r), Format::Solutions(SolutionsFormat::Sparkles)) => {
            write_buffered(writer, rows, Some(r.inner.ctx.clone()), &mut |w, sent| {
                super::native_graph_to(r, w, send, metadata, sent)
            })
            .map(|()| None)
        }
        (Source::Result(r), Format::Solutions(fmt)) => {
            let mut source = EagerSolutions { r, scratch: false };
            solutions(&mut source, writer, rows, |s, out, staged| {
                select(s, fmt, out, staged, send)
            })
        }
        (Source::Result(r), Format::Rdf(fmt)) => {
            write_buffered(writer, rows, Some(r.inner.ctx.clone()), &mut |w, sent| {
                super::graph_to(r, fmt, prefixes, w, sent)
            })
            .map(|()| None)
        }
        (Source::Result(r), Format::Jena(fmt)) => {
            write_buffered(writer, rows, Some(r.inner.ctx.clone()), &mut |w, sent| {
                super::jena_graph_to(r, fmt, w, sent)
            })
            .map(|()| None)
        }
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
        (Source::Graph(cursor), Format::Solutions(SolutionsFormat::Sparkles)) => {
            super::cursor_graph_native_json(cursor, writer, send, metadata, rows).map(Some)
        }
        (Source::Graph(cursor), Format::Rdf(fmt)) => {
            crate::sparql::cursor::graph::write_cursor_graph_rows(
                cursor, fmt, writer, send, prefixes, rows,
            )
            .map(Some)
        }
        (Source::Graph(cursor), Format::Jena(fmt)) => {
            super::cursor_jena_graph(cursor, fmt, writer, send, rows).map(Some)
        }
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
