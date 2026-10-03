//! Query results and RDF read from a response body as it arrives: solutions through
//! sparesults' parsers, triples and quads through oxrdfio's.

use crate::client::Resp;
use crate::error::{Error, Result};
use crate::retry::{self, RateLimit};
use bytes::Bytes;
use futures_util::Stream;
use oxrdf::{Quad, Triple, Variable};
use oxrdfio::{RdfFormat, RdfParser, TokioAsyncReaderQuadParser};
use sparesults::{
    QueryResultsFormat, QueryResultsParser, QuerySolution,
    TokioAsyncReaderQueryResultsParserOutput, TokioAsyncReaderSolutionsParser,
};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_util::io::StreamReader;
use tokio_util::sync::WaitForCancellationFutureOwned;

/// What a response said about itself, beyond its body.
#[derive(Clone, Debug, Default)]
pub struct ResponseMeta {
    pub status: u16,
    /// The media type without parameters.
    pub content_type: Option<String>,
    /// `Sparkles-Commit`: the commit a read saw or a write produced.
    pub commit: Option<u64>,
    /// `Sparkles-Dataset-Id`
    pub dataset_id: Option<String>,
    /// `Sparkles-Head`, on reads with `at`.
    pub head: Option<u64>,
    /// `Sparkles-At`: the state read, in canonical form, such as `commit:42`.
    pub at: Option<String>,
    /// `ETag` of a Graph Store read.
    pub etag: Option<String>,
    /// `Sparkles-Query-Version` of a stored query's run.
    pub query_version: Option<u64>,
    /// `X-Request-Id`
    pub request_id: Option<String>,
    /// The `RateLimit` field.
    pub rate_limit: Option<RateLimit>,
}

impl ResponseMeta {
    pub(crate) fn of(r: &reqwest::Response) -> ResponseMeta {
        let h = r.headers();
        let s = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
        let n = |k: &str| s(k).and_then(|v| v.trim().parse::<u64>().ok());
        ResponseMeta {
            status: r.status().as_u16(),
            content_type: s("content-type").map(|c| {
                c.split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
            }),
            commit: n("sparkles-commit"),
            dataset_id: s("sparkles-dataset-id"),
            head: n("sparkles-head"),
            at: s("sparkles-at"),
            etag: s("etag"),
            query_version: n("sparkles-query-version"),
            request_id: s("x-request-id"),
            rate_limit: retry::rate_limit(h),
        }
    }

    /// Whether the server answered `304 Not Modified`.
    pub fn not_modified(&self) -> bool {
        self.status == 304
    }
}

/// The marker of an I/O error made by a cancellation.
#[derive(Debug)]
struct CancelledMarker;
impl std::fmt::Display for CancelledMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}
impl std::error::Error for CancelledMarker {}

/// The response body as a stream of chunks that ends with an error when the call is
/// cancelled.
pub(crate) struct BodyStream {
    inner: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    cancel: Option<Pin<Box<WaitForCancellationFutureOwned>>>,
    done: bool,
}

impl Stream for BodyStream {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        if let Some(c) = self.cancel.as_mut()
            && c.as_mut().poll(cx).is_ready()
        {
            self.done = true;
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::Interrupted,
                CancelledMarker,
            ))));
        }
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Err(e))) => {
                self.done = true;
                let kind = if e.is_timeout() {
                    io::ErrorKind::TimedOut
                } else {
                    io::ErrorKind::Other
                };
                Poll::Ready(Some(Err(io::Error::new(kind, e))))
            }
            Poll::Ready(Some(Ok(b))) => Poll::Ready(Some(Ok(b))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(crate) type BodyReader = StreamReader<BodyStream, Bytes>;

/// Reads the body of `resp` as it arrives.
pub(crate) struct Body {
    pub reader: BodyReader,
    pub meta: ResponseMeta,
    pub url: String,
    pub deadline: Option<Duration>,
}

impl Body {
    pub(crate) fn new(resp: Resp) -> Body {
        let meta = ResponseMeta::of(&resp.response);
        let url = resp.response.url().to_string();
        let stream = BodyStream {
            inner: Box::pin(resp.response.bytes_stream()),
            cancel: resp.cancel.map(|c| Box::pin(c.cancelled_owned())),
            done: false,
        };
        Body {
            reader: StreamReader::new(stream),
            meta,
            url,
            deadline: resp.deadline,
        }
    }
}

/// The error of a parser, telling cancellations, deadlines and transport failures apart
/// from malformed data.
fn io_or_parse(e: io::Error, format: &str, url: &str, deadline: Option<Duration>) -> Error {
    if e.get_ref().is_some_and(|i| i.is::<CancelledMarker>()) {
        return Error::Cancelled;
    }
    if e.kind() == io::ErrorKind::TimedOut {
        return Error::Deadline(deadline.unwrap_or_default());
    }
    if e.kind() == io::ErrorKind::Other && e.get_ref().is_some_and(|i| i.is::<reqwest::Error>()) {
        return Error::Io {
            path: url.to_string(),
            source: e,
        };
    }
    Error::parse(format, e)
}

fn results_error(
    e: sparesults::QueryResultsParseError,
    f: QueryResultsFormat,
    url: &str,
    d: Option<Duration>,
) -> Error {
    match e {
        sparesults::QueryResultsParseError::Io(io) => io_or_parse(io, f.name(), url, d),
        e => Error::parse(f.name(), e),
    }
}

fn rdf_error(e: oxrdfio::RdfParseError, f: RdfFormat, url: &str, d: Option<Duration>) -> Error {
    match e {
        oxrdfio::RdfParseError::Io(io) => io_or_parse(io, f.name(), url, d),
        e => Error::parse(f.name(), e),
    }
}

/// The result of a query: solutions (SELECT), a boolean (ASK) or a graph (CONSTRUCT,
/// DESCRIBE). The variant comes from the response's media type.
pub enum QueryResults {
    Solutions(Solutions),
    Boolean(bool, ResponseMeta),
    Graph(Triples),
}

impl std::fmt::Debug for QueryResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryResults::Solutions(s) => f.debug_tuple("Solutions").field(&s.variables()).finish(),
            QueryResults::Boolean(b, _) => f.debug_tuple("Boolean").field(b).finish(),
            QueryResults::Graph(_) => f.write_str("Graph"),
        }
    }
}

impl QueryResults {
    /// The response's metadata.
    pub fn meta(&self) -> &ResponseMeta {
        match self {
            QueryResults::Solutions(s) => &s.meta,
            QueryResults::Boolean(_, m) => m,
            QueryResults::Graph(t) => t.meta(),
        }
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            QueryResults::Solutions(_) => "solutions",
            QueryResults::Boolean(..) => "a boolean",
            QueryResults::Graph(_) => "a graph",
        }
    }

    /// The solutions of a SELECT, or an error for another form.
    pub fn into_solutions(self) -> Result<Solutions> {
        match self {
            QueryResults::Solutions(s) => Ok(s),
            o => Err(Error::UnexpectedResults {
                expected: "solutions",
                got: o.kind(),
            }),
        }
    }

    /// The answer of an ASK, or an error for another form.
    pub fn into_boolean(self) -> Result<bool> {
        match self {
            QueryResults::Boolean(b, _) => Ok(b),
            o => Err(Error::UnexpectedResults {
                expected: "a boolean",
                got: o.kind(),
            }),
        }
    }

    /// The triples of a CONSTRUCT or DESCRIBE, or an error for another form.
    pub fn into_graph(self) -> Result<Triples> {
        match self {
            QueryResults::Graph(t) => Ok(t),
            o => Err(Error::UnexpectedResults {
                expected: "a graph",
                got: o.kind(),
            }),
        }
    }

    pub(crate) async fn from_response(resp: Resp) -> Result<QueryResults> {
        let body = Body::new(resp);
        let ct = body.meta.content_type.clone().unwrap_or_default();
        if let Some(f) = RdfFormat::from_media_type(&ct) {
            return Ok(QueryResults::Graph(Triples(Quads::with_format(body, f))));
        }
        match QueryResultsFormat::from_media_type(&ct) {
            Some(QueryResultsFormat::Csv) | None => Err(Error::MediaType(ct)),
            Some(f) => {
                let Body {
                    reader,
                    meta,
                    url,
                    deadline,
                } = body;
                match QueryResultsParser::from_format(f)
                    .for_tokio_async_reader(reader)
                    .await
                    .map_err(|e| results_error(e, f, &url, deadline))?
                {
                    TokioAsyncReaderQueryResultsParserOutput::Boolean(b) => {
                        Ok(QueryResults::Boolean(b, meta))
                    }
                    TokioAsyncReaderQueryResultsParserOutput::Solutions(parser) => {
                        Ok(QueryResults::Solutions(Solutions {
                            parser,
                            format: f,
                            meta,
                            url,
                            deadline,
                        }))
                    }
                }
            }
        }
    }
}

/// The solutions of a SELECT query, parsed as they arrive. Dropping it closes the
/// connection, which cancels the query on a Sparkles server.
pub struct Solutions {
    parser: TokioAsyncReaderSolutionsParser<BodyReader>,
    format: QueryResultsFormat,
    meta: ResponseMeta,
    url: String,
    deadline: Option<Duration>,
}

impl Solutions {
    /// The projected variables.
    pub fn variables(&self) -> &[Variable] {
        self.parser.variables()
    }

    /// The response's metadata.
    pub fn meta(&self) -> &ResponseMeta {
        &self.meta
    }

    /// The next solution, or `None` at the end.
    pub async fn next(&mut self) -> Option<Result<QuerySolution>> {
        let r = self.parser.next().await?;
        Some(r.map_err(|e| results_error(e, self.format, &self.url, self.deadline)))
    }

    /// The remaining solutions.
    pub async fn collect(mut self) -> Result<Vec<QuerySolution>> {
        let mut out = Vec::new();
        while let Some(s) = self.next().await {
            out.push(s?);
        }
        Ok(out)
    }

    /// The solutions as a `futures::Stream`.
    pub fn into_stream(self) -> impl Stream<Item = Result<QuerySolution>> + Send {
        futures_util::stream::unfold(self, |mut s| async move { s.next().await.map(|r| (r, s)) })
    }
}

impl std::fmt::Debug for Solutions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Solutions")
            .field("variables", &self.variables())
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Quads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Quads")
            .field("format", &self.format)
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Triples {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Triples")
            .field("format", &self.0.format)
            .field("meta", &self.0.meta)
            .finish_non_exhaustive()
    }
}

/// Quads parsed from a response as they arrive: a Graph Store read of a dataset, or a
/// CONSTRUCT with `GRAPH` templates. Triples in a triple syntax are in the default graph.
pub struct Quads {
    parser: Option<TokioAsyncReaderQuadParser<BodyReader>>,
    format: RdfFormat,
    meta: ResponseMeta,
    url: String,
    deadline: Option<Duration>,
}

impl Quads {
    pub(crate) fn with_format(body: Body, format: RdfFormat) -> Quads {
        Quads {
            parser: Some(RdfParser::from_format(format).for_tokio_async_reader(body.reader)),
            format,
            meta: body.meta,
            url: body.url,
            deadline: body.deadline,
        }
    }

    /// The parser for a response, by its media type; an empty stream for a `304`.
    pub(crate) fn from_response(resp: Resp) -> Result<Quads> {
        let body = Body::new(resp);
        if body.meta.not_modified() {
            return Ok(Quads {
                parser: None,
                format: RdfFormat::NQuads,
                meta: body.meta,
                url: body.url,
                deadline: body.deadline,
            });
        }
        let ct = body.meta.content_type.clone().unwrap_or_default();
        let f = RdfFormat::from_media_type(&ct).ok_or(Error::MediaType(ct))?;
        Ok(Quads::with_format(body, f))
    }

    /// The response's metadata.
    pub fn meta(&self) -> &ResponseMeta {
        &self.meta
    }

    /// The syntax the server answered in.
    pub fn format(&self) -> RdfFormat {
        self.format
    }

    /// The next quad, or `None` at the end.
    pub async fn next(&mut self) -> Option<Result<Quad>> {
        let r = self.parser.as_mut()?.next().await?;
        Some(r.map_err(|e| rdf_error(e, self.format, &self.url, self.deadline)))
    }

    /// The remaining quads.
    pub async fn collect(mut self) -> Result<Vec<Quad>> {
        let mut out = Vec::new();
        while let Some(q) = self.next().await {
            out.push(q?);
        }
        Ok(out)
    }

    /// The quads as a `futures::Stream`.
    pub fn into_stream(self) -> impl Stream<Item = Result<Quad>> + Send {
        futures_util::stream::unfold(self, |mut s| async move { s.next().await.map(|r| (r, s)) })
    }
}

/// Triples parsed from a response as they arrive: a CONSTRUCT or DESCRIBE result, or a
/// Graph Store read of one graph. Graph names of a quad syntax are dropped.
pub struct Triples(pub(crate) Quads);

impl Triples {
    pub(crate) fn from_response(resp: Resp) -> Result<Triples> {
        Quads::from_response(resp).map(Triples)
    }

    /// The response's metadata.
    pub fn meta(&self) -> &ResponseMeta {
        &self.0.meta
    }

    /// The syntax the server answered in.
    pub fn format(&self) -> RdfFormat {
        self.0.format
    }

    /// The next triple, or `None` at the end.
    pub async fn next(&mut self) -> Option<Result<Triple>> {
        Some(self.0.next().await?.map(Triple::from))
    }

    /// The remaining triples.
    pub async fn collect(mut self) -> Result<Vec<Triple>> {
        let mut out = Vec::new();
        while let Some(t) = self.next().await {
            out.push(t?);
        }
        Ok(out)
    }

    /// The triples as a `futures::Stream`.
    pub fn into_stream(self) -> impl Stream<Item = Result<Triple>> + Send {
        futures_util::stream::unfold(self, |mut s| async move { s.next().await.map(|r| (r, s)) })
    }
}
