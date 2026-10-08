//! Explicit native query execution with bounded response backpressure.

use super::*;
use crate::obs::DeferredReport;
use axum::body::{Body, BodyDataStream};
use futures_util::Stream;
use sparkles::sparql::{CursorStats, ExecutionMode, QueryExecution, query_execution};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

pub(super) struct Request {
    pub query: String,
    pub options: QueryOptions,
    pub params: Params,
    pub uri: Uri,
    pub guard: CancelOnDrop,
    pub limit: Option<u64>,
    pub format: SolutionsFormat,
    pub thrift: bool,
    pub rdf_format: OutFormat,
    pub native_graph: bool,
    pub execution: ExecutionMode,
}

pub(super) async fn run(ds: Arc<Dataset>, request: Request) -> ApiResult {
    let Request {
        query,
        options,
        params,
        uri,
        guard,
        limit,
        format,
        thrift,
        rdf_format,
        native_graph,
        execution,
    } = request;
    let at = history::at_param(&params)?;
    let send = params.get("send").and_then(|s| s.parse::<usize>().ok());
    let timeout = options.timeout;
    let with_extra = !options.default_graph_extra.is_empty();
    let captured_ds = ds.clone();
    let (mut cursor, seq, resolved) = blocking(move || {
        let (snapshot, resolved) = history::snapshot_for(&captured_ds, at.as_ref(), &options)?;
        let seq = snapshot.commit;
        let cursor = query_execution(snapshot, &query, &options, &Default::default(), execution)?;
        Ok((cursor, seq, resolved))
    })
    .await
    .map_err(|e| with_timeout(e, timeout))?;
    if thrift && cursor.kind() == QueryKind::Select && !matches!(cursor, QueryExecution::Eager(_)) {
        return Err(Error::Unsupported(
            "streaming SELECT does not support Thrift; choose JSON, XML, CSV, TSV or native JSON"
                .into(),
        )
        .into());
    }
    let graph = matches!(cursor.kind(), QueryKind::Construct | QueryKind::Describe);
    let content_type =
        if thrift && cursor.kind() == QueryKind::Select && format != SolutionsFormat::Sparkles {
            jena_formats::RESULTS_THRIFT
        } else if graph && !native_graph {
            rdf_format.media_type()
        } else {
            format.media_type()
        };
    let prefix_map = ds.store.prefixes();
    let prefixes = prefix_map
        .iter()
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect::<Vec<_>>();
    let control = stream::StreamControl::new(cursor.cancellation_token(), cursor.deadline());
    let completed: Arc<parking_lot::Mutex<Option<CursorStats>>> = Default::default();
    let stats_slot = completed.clone();
    let deferred = DeferredReport::default();
    let final_report = deferred.clone();
    let waits = control.clone();
    let began = Instant::now();
    let dataset_id = ds.store.dataset_id().to_string();
    let serialized = stream::serialize_controlled(
        limit,
        control,
        move |writer| {
            let metadata = Some(results::NativeJsonMetadata {
                commit: seq,
                dataset_id: &dataset_id,
            });
            let result = match &mut cursor {
                QueryExecution::Eager(result)
                    if thrift
                        && result.result().kind == QueryKind::Select
                        && format != SolutionsFormat::Sparkles =>
                {
                    write_thrift_results(result.result(), writer, send)
                }
                QueryExecution::Eager(result) => serialize_result(
                    result.result(),
                    writer,
                    (!graph && format == SolutionsFormat::Sparkles) || native_graph,
                    graph,
                    format,
                    rdf_format,
                    send,
                    &prefix_map,
                    seq,
                    &dataset_id,
                ),
                QueryExecution::Select(cursor) if format == SolutionsFormat::Sparkles => {
                    results::write_cursor_native_json(cursor, &mut *writer, send, metadata)
                        .map(|_| ())
                }
                QueryExecution::Select(cursor) => {
                    results::write_cursor_solutions(cursor, format, &mut *writer, send).map(|_| ())
                }
                QueryExecution::Graph(cursor) if native_graph => {
                    results::write_cursor_graph_native_json(cursor, &mut *writer, send, metadata)
                        .map(|_| ())
                }
                QueryExecution::Graph(cursor) => match rdf_format {
                    OutFormat::Rdf(format) => sparkles::sparql::cursor::graph::write_cursor_graph(
                        cursor,
                        format,
                        &mut *writer,
                        send,
                        &prefixes,
                    )
                    .map(|_| ()),
                    OutFormat::Jena(format) => {
                        results::write_cursor_jena_graph(cursor, format, &mut *writer, send)
                            .map(|_| ())
                    }
                },
                QueryExecution::Ask(result) if format == SolutionsFormat::Sparkles => {
                    results::write_native_json(result.result(), &mut *writer, send, metadata)
                }
                QueryExecution::Ask(result) => {
                    results::write_solutions(result.result(), format, &mut *writer, send)
                }
            }
            .map_err(|e| writer.classify(e));
            *stats_slot.lock() = Some(cursor.stats());
            result.map(|_| ())
        },
        move |end| {
            let stats = completed.lock().take();
            let wait_ms = waits.wait_ms();
            let exec_ms = stats.as_ref().map_or(0.0, |s| s.timing.exec_ms);
            let serialize_ms =
                (began.elapsed().as_secs_f64() * 1000.0 - exec_ms - wait_ms).max(0.0);
            let (outcome, budget) = match &end.error {
                Some(Error::Timeout) => (Some(Outcome::Timeout), None),
                Some(Error::Cancelled) => (Some(Outcome::Cancelled), None),
                Some(Error::BudgetExceeded(b)) => (Some(Outcome::Budget), Some(b.kind)),
                Some(_) if end.disconnected => (Some(Outcome::Cancelled), None),
                Some(_) => (Some(Outcome::Error), None),
                None => (None, None),
            };
            let mut timing = stats.as_ref().map(|s| s.timing.clone());
            if let Some(t) = &mut timing {
                t.serialize_ms = serialize_ms;
            }
            final_report.producer_done(RequestReport {
                operation: Some(Op::Query),
                outcome,
                budget,
                rows: stats.as_ref().map(|s| s.emitted_rows),
                timing,
                serialize_ms: Some(serialize_ms),
                response_bytes: Some(end.bytes),
                mem_peak_bytes: stats.as_ref().map(|s| s.mem_peak_bytes),
                rows_produced: stats.as_ref().map(|s| s.rows_produced),
                cursor_status: stats.as_ref().map(|s| s.status),
                transport_wait_ms: Some(wait_ms),
                ..Default::default()
            });
        },
    )
    .await;
    let serialized = match serialized {
        Ok(serialized) => serialized,
        Err(error) => {
            let mut response = with_timeout(error, timeout).into_response();
            response.extensions_mut().insert(deferred);
            return Ok(response);
        }
    };
    let body = match serialized {
        stream::Serialized::Whole { body, .. } => Body::from(body),
        stream::Serialized::Streamed(body) => body,
    };
    let ct = if params.has("force-accept") {
        "text/plain; charset=utf-8"
    } else {
        content_type
    };
    let body = Body::from_stream(CancelBody {
        inner: body.into_data_stream(),
        _guard: guard,
    });
    let response = with_commit(
        ([(header::CONTENT_TYPE, ct)], body).into_response(),
        &ds,
        seq,
    );
    let response = history::history_headers(response, resolved.as_ref(), &uri);
    let mut response = match &resolved {
        Some(r) if r.historical => response,
        _ => with_inferences(response, &ds, with_extra, seq),
    };
    response.extensions_mut().insert(deferred);
    Ok(response)
}

struct CancelBody {
    inner: BodyDataStream,
    _guard: CancelOnDrop,
}
impl Stream for CancelBody {
    type Item = Result<Bytes, axum::Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().inner).poll_next(cx)
    }
}
