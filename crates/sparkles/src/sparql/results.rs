//! Result serialization (ARQ `ResultSetFormatter` / RIOT writers).

use super::{QueryKind, QueryResult};
use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::{Id, Tag};
use oxrdf::{Term, Variable};
use oxrdfio::{RdfFormat, RdfSerializer};
use serde_json::{Value as J, json};
use sparesults::{QueryResultsFormat, QueryResultsSerializer};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
        "application/ld+json" | "jsonld" | "json-ld" => RdfFormat::JsonLd {
            profile: oxrdfio::JsonLdProfileSet::empty(),
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

    // serializers write many small pieces: hand them on whole, so a `Vec` appends them
    // without the generic retry loop
    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.admit(buf.len())?;
        self.inner.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Serialize SELECT / ASK results.
pub fn write_solutions(
    r: &QueryResult,
    fmt: SolutionsFormat,
    mut w: impl Write,
    send: Option<usize>,
) -> Result<()> {
    if fmt == SolutionsFormat::Sparkles {
        serde_json::to_writer(&mut w, &sparkles_json(r, send)).map_err(|e| Error::Io(e.into()))?;
        return Ok(());
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
        // ids past the vocabulary (none in a consistent snapshot) end the decoding: they
        // are a suffix, whose cells fall back to decoding one by one
        let terms = if ids.len() < PAR_DECODE_MIN {
            let mut terms = Vec::with_capacity(ids.len());
            vocab.get_sorted(&ids, |_, k| terms.push(crate::id::key_to_term(k)));
            terms
        } else {
            ids.par_chunks(DECODE_TASK)
                .flat_map_iter(|c| {
                    let mut out = Vec::with_capacity(c.len());
                    vocab.get_sorted(c, |_, k| out.push(crate::id::key_to_term(k)));
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
