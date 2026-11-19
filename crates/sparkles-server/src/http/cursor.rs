//! Native query execution with bounded response backpressure, for a request that asks for
//! streaming and for an answer that automatic execution streams.

use super::*;
use crate::obs::DeferredReport;
use axum::body::{Body, BodyDataStream};
use futures_util::Stream;
use sparkles::sparql::{CursorStats, ExecutionMode, QueryExecution, query_execution};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

pub(super) struct Request {
    pub timeouts: stream::Timeouts,
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
    /// the declared and runtime prefixes, for graph results (spec C20 §5)
    pub prefixes: std::collections::BTreeMap<String, String>,
    /// the I/O profile of the request (`profile=io`)
    pub io: Option<Arc<sparkles::ioacct::Profile>>,
}

/// How an opened execution is answered: everything of [`Request`] except what opening
/// it needed.
pub(super) struct Serve {
    pub timeouts: stream::Timeouts,
    pub params: Params,
    pub uri: Uri,
    pub guard: CancelOnDrop,
    pub limit: Option<u64>,
    pub format: SolutionsFormat,
    pub thrift: bool,
    pub rdf_format: OutFormat,
    pub native_graph: bool,
    pub prefixes: std::collections::BTreeMap<String, String>,
    pub io: Option<Arc<sparkles::ioacct::Profile>>,
    pub timeout: Option<std::time::Duration>,
    pub with_extra: bool,
    /// the start of the query, for the phase spans (`None` when OTel is off)
    pub t0: Option<std::time::SystemTime>,
    /// The quick-query key of an answer that automatic execution chose to stream, and
    /// when its run began on the thread that planned it. A streamed answer that completes in one whole
    /// body is remembered with its time, as an eager one is, so that a quick query runs
    /// in place again from its next run on. ASK retains automatic admission because
    /// its native path may be necessary to fit the request's budgets.
    pub quick: Option<(u64, Instant)>,
}

pub(super) async fn run(ds: Arc<Dataset>, request: Request) -> ApiResult {
    let Request {
        timeouts,
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
        prefixes,
        io,
    } = request;
    let at = history::at_param(&params)?;
    let timeout = options.timeout;
    let with_extra = !options.default_graph_extra.is_empty();
    let t0 = crate::otel::start();
    let captured_ds = ds.clone();
    let (cursor, seq, resolved) = blocking(move || {
        let (snapshot, resolved) = history::snapshot_for(&captured_ds, at.as_ref(), &options)?;
        let seq = snapshot.commit;
        let cursor = query_execution(
            snapshot,
            &query,
            &options,
            &Default::default(),
            ExecutionMode::Streaming,
        )?;
        Ok((cursor, seq, resolved))
    })
    .await
    .map_err(|e| with_timeout(e, timeout))?;
    serve(
        ds,
        cursor,
        seq,
        resolved,
        Serve {
            timeouts,
            params,
            uri,
            guard,
            limit,
            format,
            thrift,
            rdf_format,
            native_graph,
            prefixes,
            io,
            timeout,
            with_extra,
            t0,
            quick: None,
        },
    )
    .await
}

/// Answer an opened execution, writing a cursor's batches as they come.
pub(super) async fn serve(
    ds: Arc<Dataset>,
    mut cursor: QueryExecution,
    seq: u64,
    resolved: Option<sparkles::history::Resolved>,
    serve: Serve,
) -> ApiResult {
    let Serve {
        timeouts,
        params,
        uri,
        guard,
        limit,
        format,
        thrift,
        rdf_format,
        native_graph,
        prefixes: prefix_map,
        io,
        timeout,
        with_extra,
        t0,
        quick,
    } = serve;
    let send = params.get("send").and_then(|s| s.parse::<usize>().ok());
    // what query-time reasoning derived, for the response header
    let derived = cursor.reasoning();
    let graph = matches!(cursor.kind(), QueryKind::Construct | QueryKind::Describe);
    let kind = cursor.kind();
    // SPARQL Results Thrift holds SELECT results; ASK and the Sparkles document keep the
    // negotiated format
    let thrift = thrift && kind == QueryKind::Select && format != SolutionsFormat::Sparkles;
    let solutions_format = if thrift {
        results::Format::ResultsThrift
    } else {
        results::Format::Solutions(format)
    };
    let content_type = if thrift {
        jena_formats::RESULTS_THRIFT
    } else if graph && !native_graph {
        rdf_format.media_type()
    } else {
        format.media_type()
    };
    let prefixes = prefix_map
        .iter()
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect::<Vec<_>>();
    let control = stream::StreamControl::new(
        cursor.cancellation_token(),
        cursor.deadline(),
        cursor.deadline_pause(),
        timeout,
    );
    let completed: Arc<parking_lot::Mutex<Option<CursorStats>>> = Default::default();
    let stats_slot = completed.clone();
    // how the cursor ended, for the quick-query record of a whole body
    let ended: Arc<parking_lot::Mutex<Option<sparkles::sparql::CursorStatus>>> = Default::default();
    let ended_slot = ended.clone();
    // the plan as it stood when a native JSON stream failed before its first byte
    let failed_plan: Arc<parking_lot::Mutex<Option<serde_json::Value>>> = Default::default();
    let plan_slot = failed_plan.clone();
    // the rows that reached the response writer whole, for the report of a body stopped
    // partway
    let sent_rows: Arc<parking_lot::Mutex<Option<u64>>> = Default::default();
    let sent_slot = sent_rows.clone();
    let deferred = DeferredReport::default();
    let final_report = deferred.clone();
    let waits = control.clone();
    let began = Instant::now();
    let dataset_id = ds.store.dataset_id().to_string();
    // A request that asked for streaming gets its first bytes after the first batch.
    let early = quick.is_none();
    let serialized = stream::serialize_controlled(
        limit,
        control,
        early,
        timeouts,
        move |writer| {
            let metadata = Some(results::NativeJsonMetadata {
                commit: seq,
                dataset_id: &dataset_id,
                io: io.as_deref(),
            });
            let result = match &mut cursor {
                QueryExecution::Eager(result) => {
                    let sent = results::RowCount::with_ledger(writer.get_ref().ledger());
                    let written = serialize_result(
                        result.result(),
                        writer,
                        (!graph && format == SolutionsFormat::Sparkles) || native_graph,
                        graph,
                        solutions_format,
                        rdf_format,
                        send,
                        &prefix_map,
                        seq,
                        &dataset_id,
                        io.as_deref(),
                        &sent,
                    );
                    *sent_slot.lock() = Some(sent.get());
                    written
                }
                QueryExecution::Select(cursor) => {
                    let sent = results::RowCount::with_ledger(writer.get_ref().ledger());
                    let options = results::WriteOptions {
                        send,
                        metadata,
                        ..results::WriteOptions::new(solutions_format, &sent)
                    };
                    let written =
                        results::write(results::Source::Select(cursor), options, &mut *writer);
                    *sent_slot.lock() = Some(sent.get());
                    written.map(|_| ())
                }
                QueryExecution::Graph(cursor) => {
                    let sent = results::RowCount::with_ledger(writer.get_ref().ledger());
                    let format = if native_graph {
                        results::Format::Solutions(SolutionsFormat::Sparkles)
                    } else {
                        rdf_format.format()
                    };
                    let options = results::WriteOptions {
                        send,
                        prefixes: &prefixes,
                        metadata,
                        ..results::WriteOptions::new(format, &sent)
                    };
                    let written =
                        results::write(results::Source::Graph(cursor), options, &mut *writer);
                    *sent_slot.lock() = Some(sent.get());
                    written.map(|_| ())
                }
                QueryExecution::Ask(result) if format == SolutionsFormat::Sparkles => {
                    results::write_native_json(result.result(), &mut *writer, send, metadata)
                }
                QueryExecution::Ask(result) => {
                    results::write_solutions(result.result(), format, &mut *writer, send)
                }
            }
            .map_err(|e| writer.classify(e));
            if result.is_err() && (format == SolutionsFormat::Sparkles || native_graph) {
                // a cursor's plan serializes on stack segments it grows per level, and
                // the response writer sizes the stack for writing and dropping the value
                *plan_slot.lock() = match &cursor {
                    QueryExecution::Select(c) => serde_json::to_value(c.plan()).ok(),
                    QueryExecution::Graph(c) => serde_json::to_value(c.plan()).ok(),
                    _ => None,
                };
            }
            *stats_slot.lock() = Some(cursor.stats());
            result.map(|_| ())
        },
        move |end| {
            let stats = completed.lock().take();
            *ended_slot.lock() = stats.as_ref().map(|s| s.status);
            let wait_ms = waits.wait_ms();
            let exec_ms = stats.as_ref().map_or(0.0, |s| s.timing.exec_ms);
            let serialize_ms =
                (began.elapsed().as_secs_f64() * 1000.0 - exec_ms - wait_ms).max(0.0);
            let (outcome, budget) = end.outcome();
            // The statistics count all the rows an answer produced, so a body that stopped
            // partway reports the rows that reached the response writer whole.
            // The rows of a streamed body are the ones whose ends the transport handed over,
            // and a writer that records none there reports the rows it wrote whole. A body
            // that failed before it was committed is an error response, which sent none of
            // its rows.
            let rows = match (end.rows, *sent_rows.lock()) {
                _ if !end.committed => None,
                (Some(delivered), _) if end.error.is_some() => Some(delivered),
                (None, Some(sent)) if end.error.is_some() => Some(sent),
                _ => stats.as_ref().map(|s| s.emitted_rows),
            };
            if let Some(stats) = &stats {
                crate::otel::cursor_done(t0, kind, stats, serialize_ms);
            }
            let mut timing = stats.as_ref().map(|s| s.timing.clone());
            if let Some(t) = &mut timing {
                t.serialize_ms = serialize_ms;
            }
            final_report.producer_done(RequestReport {
                operation: Some(Op::Query),
                outcome,
                budget,
                rows,
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
            // a failed run is not known to be quick, as for an eager answer
            if let Some((key, _)) = quick {
                QUICK.record(key, f64::INFINITY);
            }
            let mut error = with_timeout(error, timeout);
            if let (Some(plan), Some(body)) = (failed_plan.lock().take(), error.1.as_object_mut()) {
                body.insert("plan".into(), plan);
                body.insert("commit".into(), seq.into());
            }
            let mut response = error.into_response();
            response.extensions_mut().insert(deferred);
            return Ok(response);
        }
    };
    let abort = serialized.abort_handle();
    let body = match serialized {
        stream::Serialized::Whole { body, .. } => {
            // A cursor that stopped at `send` has not run the whole query, so only a
            // complete answer counts.
            if let Some((key, began)) = quick
                && *ended.lock() == Some(sparkles::sparql::CursorStatus::Complete)
            {
                let ms = began.elapsed().as_secs_f64() * 1000.0;
                if kind == QueryKind::Ask {
                    QUICK.record_ask(key, ms);
                } else {
                    QUICK.record_cursor(key, ms);
                }
            }
            Body::from(body)
        }
        stream::Serialized::Streamed(body) | stream::Serialized::Timed { body, .. } => {
            // a body too large to buffer is not quick, as for an eager answer
            if let Some((key, _)) = quick {
                QUICK.record(key, f64::INFINITY);
            }
            body
        }
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
    let response = super::with_reasoning(response, derived.as_deref());
    let mut response = match &resolved {
        Some(r) if r.historical => super::with_historical_stale_read(response, derived.as_deref()),
        _ => with_inferences(
            response,
            &ds,
            with_extra && derived.as_ref().is_none_or(|a| a.reads_inferred()),
            seq,
        ),
    };
    response.extensions_mut().insert(deferred);
    if let Some(abort) = abort {
        response.extensions_mut().insert(abort);
    }
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
