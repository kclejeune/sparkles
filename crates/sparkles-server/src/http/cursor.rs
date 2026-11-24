//! Native query execution with bounded response backpressure, for a request that asks for
//! streaming and for an answer that automatic execution streams.
//!
//! Item 534 removes the thread-handoff gap in two ways:
//! 1. Pool-routed cursors (`execution=streaming`, and `execution=auto` when not running in
//!    place) open and serialize in one blocking-pool task instead of opening on the pool,
//!    returning to the handler, and waking a second pool task.
//! 2. In-place automatic cursors (`execution=auto` on a first run or quick cursor history)
//!    open and drain batches in place up to [`stream::STREAM_AFTER`], answering whole when
//!    the cursor ends first and handing the cursor and buffered bytes off to a blocking
//!    streaming producer otherwise. Explicit `execution=streaming` always stays on the
//!    blocking pool so client disconnect cancels it immediately.

use super::*;
use crate::obs::DeferredReport;
use axum::body::{Body, BodyDataStream};
use futures_util::Stream;
use sparkles::sparql::results::{
    CursorWriteOptions, CursorWriter, LimitedWriter, OwnedNativeJsonMetadata, WriteStep,
};
use sparkles::sparql::{CursorStats, CursorStatus, ExecutionMode, QueryExecution, query_execution};
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

/// Automatic-route request parameters shared by [`run_in_place`] and [`run_auto_pool`].
pub(super) struct AutoRequest<'a> {
    pub timeouts: stream::Timeouts,
    pub query: String,
    pub options: QueryOptions,
    pub at: Option<sparkles::history::At>,
    pub params: &'a Params,
    pub uri: &'a Uri,
    pub guard: CancelOnDrop,
    pub limit: Option<u64>,
    pub format: SolutionsFormat,
    pub thrift: bool,
    pub rdf_format: OutFormat,
    pub native_graph: bool,
    pub prefixes: std::collections::BTreeMap<String, String>,
    pub io: Option<Arc<sparkles::ioacct::Profile>>,
    pub quick_key: u64,
    pub auto: bool,
    pub trial: bool,
    pub claim: Option<Arc<TrialClaim>>,
}

/// Outcome of [`run_in_place`] or [`run_auto_pool`].
#[allow(clippy::large_enum_variant)]
pub(super) enum AutoOutcome {
    /// Automatic execution (or eager trial) produced a materialized result.
    Eager {
        result: sparkles::sparql::QueryResult,
        seq: u64,
        resolved: Option<sparkles::history::Resolved>,
        t0: Option<std::time::SystemTime>,
        began: Instant,
        guard: CancelOnDrop,
        prefixes: std::collections::BTreeMap<String, String>,
    },
    /// A cursor opened and produced a complete or streaming HTTP response.
    Responded(axum::response::Response),
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
    /// when its run began on the thread that planned it.
    pub quick: Option<(u64, Instant)>,
}

struct ResolvedFormat {
    kind: QueryKind,
    graph: bool,
    solutions_format: results::Format,
    content_type: &'static str,
    sparkles_error_plan: bool,
}

impl ResolvedFormat {
    fn resolve(
        kind: QueryKind,
        format: SolutionsFormat,
        thrift: bool,
        rdf_format: OutFormat,
        native_graph: bool,
    ) -> Self {
        let graph = matches!(kind, QueryKind::Construct | QueryKind::Describe);
        // SPARQL Results Thrift holds SELECT results; ASK and the Sparkles document keep
        // the negotiated format.
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
        let sparkles_error_plan = format == SolutionsFormat::Sparkles || native_graph;
        Self {
            kind,
            graph,
            solutions_format,
            content_type,
            sparkles_error_plan,
        }
    }
}

struct OpenMeta {
    seq: u64,
    resolved: Option<sparkles::history::Resolved>,
    derived: Option<Arc<sparkles::sparql::reasoning::Applied>>,
    fmt: ResolvedFormat,
    t0: Option<std::time::SystemTime>,
    quick_began: Instant,
}

struct OpenState {
    cursor: QueryExecution,
    seq: u64,
    fmt: ResolvedFormat,
    _claim: Option<Arc<TrialClaim>>,
}

struct SharedReportState {
    began: Instant,
    kind: QueryKind,
    t0: Option<std::time::SystemTime>,
    waits: stream::StreamControl,
}

#[allow(clippy::too_many_arguments)]
fn write_cursor_execution(
    cursor: &mut QueryExecution,
    writer: &mut LimitedWriter<stream::SwitchWriter>,
    seq: u64,
    dataset_id: &str,
    io: Option<&sparkles::ioacct::Profile>,
    format: SolutionsFormat,
    rdf_format: OutFormat,
    native_graph: bool,
    send: Option<usize>,
    prefix_map: &std::collections::BTreeMap<String, String>,
    prefixes: &[(String, String)],
    fmt: &ResolvedFormat,
    sent_slot: &parking_lot::Mutex<Option<u64>>,
    plan_slot: &parking_lot::Mutex<Option<serde_json::Value>>,
    stats_slot: &parking_lot::Mutex<Option<CursorStats>>,
) -> sparkles::Result<()> {
    let metadata = Some(results::NativeJsonMetadata {
        commit: seq,
        dataset_id,
        io,
    });
    let result = match cursor {
        QueryExecution::Eager(result) => {
            let sent = results::RowCount::with_ledger(writer.get_ref().ledger());
            let written = serialize_result(
                result.result(),
                writer,
                (!fmt.graph && format == SolutionsFormat::Sparkles) || native_graph,
                fmt.graph,
                fmt.solutions_format,
                rdf_format,
                send,
                prefix_map,
                seq,
                dataset_id,
                io,
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
                ..results::WriteOptions::new(fmt.solutions_format, &sent)
            };
            let written = results::write(results::Source::Select(cursor), options, &mut *writer);
            *sent_slot.lock() = Some(sent.get());
            written.map(|_| ())
        }
        QueryExecution::Graph(cursor) => {
            let sent = results::RowCount::with_ledger(writer.get_ref().ledger());
            let graph_format = if native_graph {
                results::Format::Solutions(SolutionsFormat::Sparkles)
            } else {
                rdf_format.format()
            };
            let options = results::WriteOptions {
                send,
                prefixes,
                metadata,
                ..results::WriteOptions::new(graph_format, &sent)
            };
            let written = results::write(results::Source::Graph(cursor), options, &mut *writer);
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
    if result.is_err() && fmt.sparkles_error_plan {
        // a cursor's plan serializes on stack segments it grows per level, and the
        // response writer sizes the stack for writing and dropping the value
        *plan_slot.lock() = match &*cursor {
            QueryExecution::Select(c) => serde_json::to_value(c.plan()).ok(),
            QueryExecution::Graph(c) => serde_json::to_value(c.plan()).ok(),
            _ => None,
        };
    }
    *stats_slot.lock() = Some(cursor.stats());
    result
}

fn finish_stream_end(
    end: stream::StreamEnd,
    stats: Option<CursorStats>,
    sent_rows: Option<u64>,
    ended_slot: &parking_lot::Mutex<Option<CursorStatus>>,
    final_report: &DeferredReport,
    shared: &SharedReportState,
) {
    *ended_slot.lock() = stats.as_ref().map(|s| s.status);
    let wait_ms = shared.waits.wait_ms();
    let exec_ms = stats.as_ref().map_or(0.0, |s| s.timing.exec_ms);
    let serialize_ms = (shared.began.elapsed().as_secs_f64() * 1000.0 - exec_ms - wait_ms).max(0.0);
    let (outcome, budget) = end.outcome();
    let rows = end.reported_rows(sent_rows, stats.as_ref().map(|s| s.emitted_rows));
    if let Some(stats) = &stats {
        crate::otel::cursor_done(shared.t0, shared.kind, stats, serialize_ms);
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
}

#[allow(clippy::too_many_arguments)]
fn build_cursor_headers(
    ds: &Arc<Dataset>,
    params: &Params,
    uri: &Uri,
    with_extra: bool,
    seq: u64,
    resolved: Option<&sparkles::history::Resolved>,
    derived: Option<&sparkles::sparql::reasoning::Applied>,
    content_type: &'static str,
    body: Body,
) -> axum::response::Response {
    let ct = if params.has("force-accept") {
        "text/plain; charset=utf-8"
    } else {
        content_type
    };
    let response = with_commit(
        ([(header::CONTENT_TYPE, ct)], body).into_response(),
        ds,
        seq,
    );
    let response = history::history_headers(response, resolved, uri);
    let response = super::with_reasoning(response, derived);
    match resolved {
        Some(r) if r.historical => super::with_historical_stale_read(response, derived),
        _ => with_inferences(
            response,
            ds,
            with_extra && derived.is_none_or(|a| a.reads_inferred()),
            seq,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_serialized_response(
    ds: &Arc<Dataset>,
    params: &Params,
    uri: &Uri,
    guard: CancelOnDrop,
    timeout: Option<std::time::Duration>,
    with_extra: bool,
    meta: OpenMeta,
    quick: Option<(u64, Instant)>,
    serialized: ApiResult<stream::Serialized>,
    ended: &parking_lot::Mutex<Option<CursorStatus>>,
    failed_plan: &parking_lot::Mutex<Option<serde_json::Value>>,
    deferred: DeferredReport,
) -> ApiResult {
    let serialized = match serialized {
        Ok(serialized) => serialized,
        Err(error) => {
            if let Some((key, _)) = quick {
                QUICK.record(key, f64::INFINITY);
            }
            let mut error = with_timeout(error, timeout);
            if let (Some(plan), Some(body)) = (failed_plan.lock().take(), error.1.as_object_mut()) {
                body.insert("plan".into(), plan);
                body.insert("commit".into(), meta.seq.into());
            }
            let mut response = error.into_response();
            response.extensions_mut().insert(deferred);
            return Ok(response);
        }
    };
    let abort = serialized.abort_handle();
    let body = match serialized {
        stream::Serialized::Whole { body, .. } => {
            if let Some((key, began)) = quick
                && *ended.lock() == Some(CursorStatus::Complete)
            {
                let ms = began.elapsed().as_secs_f64() * 1000.0;
                if meta.fmt.kind == QueryKind::Ask {
                    QUICK.record_ask(key, ms);
                } else {
                    QUICK.record_cursor(key, ms);
                }
            }
            Body::from(body)
        }
        stream::Serialized::Streamed(body) | stream::Serialized::Timed { body, .. } => {
            if let Some((key, _)) = quick {
                QUICK.record(key, f64::INFINITY);
            }
            body
        }
    };
    let body = Body::from_stream(CancelBody {
        inner: body.into_data_stream(),
        _guard: guard,
    });
    let mut response = build_cursor_headers(
        ds,
        params,
        uri,
        with_extra,
        meta.seq,
        meta.resolved.as_ref(),
        meta.derived.as_deref(),
        meta.fmt.content_type,
        body,
    );
    response.extensions_mut().insert(deferred);
    if let Some(abort) = abort {
        response.extensions_mut().insert(abort);
    }
    Ok(response)
}

/// Explicit `execution=streaming`: opens and writes the cursor in one blocking-pool task
/// so client disconnect cancels it while avoiding the two middle thread hops (item 534).
pub(super) async fn run(ds: Arc<Dataset>, request: Request) -> ApiResult {
    #[cfg(test)]
    route_stats::note_pool_streaming();
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
        prefixes: prefix_map,
        io,
    } = request;
    let at = history::at_param(&params)?;
    let timeout = options.timeout;
    let with_extra = !options.default_graph_extra.is_empty();
    let send = params.get("send").and_then(|s| s.parse::<usize>().ok());
    let t0 = crate::otel::start();
    let prefixes = prefix_map
        .iter()
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect::<Vec<_>>();
    let dataset_id = ds.store.dataset_id().to_string();

    let completed: Arc<parking_lot::Mutex<Option<CursorStats>>> = Default::default();
    let stats_slot = completed.clone();
    let ended: Arc<parking_lot::Mutex<Option<CursorStatus>>> = Default::default();
    let ended_slot = ended.clone();
    let failed_plan: Arc<parking_lot::Mutex<Option<serde_json::Value>>> = Default::default();
    let plan_slot = failed_plan.clone();
    let sent_rows: Arc<parking_lot::Mutex<Option<u64>>> = Default::default();
    let sent_slot = sent_rows.clone();
    let report_state: Arc<parking_lot::Mutex<Option<SharedReportState>>> = Default::default();
    let report_state_open = report_state.clone();
    let deferred = DeferredReport::default();
    let final_report = deferred.clone();

    let open_ds = ds.clone();
    let opened = stream::open_and_serialize_controlled::<(), OpenMeta, OpenState, _, _, _>(
        limit,
        /* early */ true,
        timeouts,
        move || {
            let quick_began = Instant::now();
            let (snapshot, resolved) = history::snapshot_for(&open_ds, at.as_ref(), &options)?;
            let seq = snapshot.commit;
            let cursor = query_execution(
                snapshot,
                &query,
                &options,
                &Default::default(),
                ExecutionMode::Streaming,
            )?;
            let derived = cursor.reasoning();
            let kind = cursor.kind();
            let fmt = ResolvedFormat::resolve(kind, format, thrift, rdf_format, native_graph);
            let control = stream::StreamControl::new(
                cursor.cancellation_token(),
                cursor.deadline(),
                cursor.deadline_pause(),
                timeout,
            );
            let began = Instant::now();
            *report_state_open.lock() = Some(SharedReportState {
                began,
                kind,
                t0,
                waits: control.clone(),
            });
            Ok(stream::Opened::Cursor {
                meta: OpenMeta {
                    seq,
                    resolved,
                    derived,
                    fmt: ResolvedFormat::resolve(kind, format, thrift, rdf_format, native_graph),
                    t0,
                    quick_began,
                },
                control,
                state: OpenState {
                    cursor,
                    seq,
                    fmt,
                    _claim: None,
                },
            })
        },
        move |mut state, writer| {
            let _claim = state._claim;
            write_cursor_execution(
                &mut state.cursor,
                writer,
                state.seq,
                &dataset_id,
                io.as_deref(),
                format,
                rdf_format,
                native_graph,
                send,
                &prefix_map,
                &prefixes,
                &state.fmt,
                &sent_slot,
                &plan_slot,
                &stats_slot,
            )
        },
        move |end| {
            let shared = report_state
                .lock()
                .take()
                .expect("report state set on cursor open");
            finish_stream_end(
                end,
                completed.lock().take(),
                *sent_rows.lock(),
                &ended_slot,
                &final_report,
                &shared,
            );
        },
    )
    .await
    .map_err(|e| with_timeout(e, timeout))?;

    match opened {
        stream::OpenedSerialized::Eager(()) => unreachable!("streaming never returns eager"),
        stream::OpenedSerialized::Cursor { meta, serialized } => finish_serialized_response(
            &ds,
            &params,
            &uri,
            guard,
            timeout,
            with_extra,
            meta,
            None,
            serialized,
            &ended,
            &failed_plan,
            deferred,
        ),
    }
}

/// Pool route for `execution=auto` (or an eager trial) when not running in place: opens
/// the query and, if automatic execution selects a cursor, writes its body in the same
/// blocking-pool task (item 534).
pub(super) async fn run_auto_pool(
    ds: Arc<Dataset>,
    req: AutoRequest<'_>,
) -> ApiResult<AutoOutcome> {
    let AutoRequest {
        timeouts,
        query,
        options,
        at,
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
        quick_key,
        auto,
        trial,
        claim,
    } = req;
    let timeout = options.timeout;
    let with_extra = !options.default_graph_extra.is_empty();
    let send = params.get("send").and_then(|s| s.parse::<usize>().ok());
    let failed_plan_flag = format == SolutionsFormat::Sparkles;
    let prefixes = prefix_map
        .iter()
        .map(|(a, b)| (a.clone(), b.clone()))
        .collect::<Vec<_>>();
    let eager_prefixes = prefix_map.clone();
    let dataset_id = ds.store.dataset_id().to_string();

    let completed: Arc<parking_lot::Mutex<Option<CursorStats>>> = Default::default();
    let stats_slot = completed.clone();
    let ended: Arc<parking_lot::Mutex<Option<CursorStatus>>> = Default::default();
    let ended_slot = ended.clone();
    let failed_plan: Arc<parking_lot::Mutex<Option<serde_json::Value>>> = Default::default();
    let plan_slot = failed_plan.clone();
    let sent_rows: Arc<parking_lot::Mutex<Option<u64>>> = Default::default();
    let sent_slot = sent_rows.clone();
    let report_state: Arc<parking_lot::Mutex<Option<SharedReportState>>> = Default::default();
    let report_state_open = report_state.clone();
    let deferred = DeferredReport::default();
    let final_report = deferred.clone();

    let open_ds = ds.clone();
    let open_claim = claim.clone();
    let opened = stream::open_and_serialize_controlled(
        limit,
        /* early */ false,
        timeouts,
        move || {
            let t = Instant::now();
            let t0 = crate::otel::start();
            let (snap, resolved) = history::snapshot_for(&open_ds, at.as_ref(), &options)?;
            let seq = snap.commit;
            let automatic = |snap, opts: &QueryOptions| {
                sparkles::sparql::query_execution_with_plan(
                    snap,
                    &query,
                    opts,
                    &Default::default(),
                    ExecutionMode::Auto,
                )
                .map(|execution| match execution {
                    QueryExecution::Eager(r) => Ran::Eager(r.into_result()),
                    cursor => Ran::Cursor(Box::new(cursor)),
                })
            };
            let ran = if auto {
                automatic(snap, &options)
            } else if trial {
                let shared = one_request(&options);
                match sparkles::sparql::query_with_plan(snap.clone(), &query, &shared) {
                    Err(f) if outgrew_memory(&f.error) => {
                        QUICK.record_eager_failure(quick_key);
                        automatic(snap, &shared)
                    }
                    ran => ran.map(Ran::Eager),
                }
            } else {
                unreachable!("run_auto_pool handles auto or trial")
            };
            match ran {
                Ok(Ran::Eager(r)) => {
                    tracing::debug!("query executed in {:?} ({} results)", t.elapsed(), r.len());
                    Ok(stream::Opened::Eager((r, seq, resolved, t0, t)))
                }
                Ok(Ran::Cursor(cursor)) => {
                    let cursor = *cursor;
                    #[cfg(test)]
                    route_stats::note_pool_auto(quick_key);
                    let derived = cursor.reasoning();
                    let kind = cursor.kind();
                    let fmt =
                        ResolvedFormat::resolve(kind, format, thrift, rdf_format, native_graph);
                    let control = stream::StreamControl::new(
                        cursor.cancellation_token(),
                        cursor.deadline(),
                        cursor.deadline_pause(),
                        timeout,
                    );
                    let began = Instant::now();
                    *report_state_open.lock() = Some(SharedReportState {
                        began,
                        kind,
                        t0,
                        waits: control.clone(),
                    });
                    Ok(stream::Opened::Cursor {
                        meta: OpenMeta {
                            seq,
                            resolved,
                            derived,
                            fmt: ResolvedFormat::resolve(
                                kind,
                                format,
                                thrift,
                                rdf_format,
                                native_graph,
                            ),
                            t0,
                            quick_began: t,
                        },
                        control,
                        state: OpenState {
                            cursor,
                            seq,
                            fmt,
                            _claim: open_claim,
                        },
                    })
                }
                Err(f) => Err(query_failure_error(f, failed_plan_flag, seq)),
            }
        },
        move |mut state, writer| {
            let _claim = state._claim;
            write_cursor_execution(
                &mut state.cursor,
                writer,
                state.seq,
                &dataset_id,
                io.as_deref(),
                format,
                rdf_format,
                native_graph,
                send,
                &prefix_map,
                &prefixes,
                &state.fmt,
                &sent_slot,
                &plan_slot,
                &stats_slot,
            )
        },
        move |end| {
            let shared = report_state
                .lock()
                .take()
                .expect("report state set on cursor open");
            finish_stream_end(
                end,
                completed.lock().take(),
                *sent_rows.lock(),
                &ended_slot,
                &final_report,
                &shared,
            );
        },
    )
    .await;

    let opened = match opened {
        Ok(opened) => opened,
        Err(e) => {
            QUICK.record(quick_key, f64::INFINITY);
            return Err(with_timeout(e, timeout));
        }
    };
    match opened {
        stream::OpenedSerialized::Eager((result, seq, resolved, t0, began)) => {
            Ok(AutoOutcome::Eager {
                result,
                seq,
                resolved,
                t0,
                began,
                guard,
                prefixes: eager_prefixes,
            })
        }
        stream::OpenedSerialized::Cursor { meta, serialized } => {
            let quick = Some((quick_key, meta.quick_began));
            let resp = finish_serialized_response(
                &ds,
                params,
                uri,
                guard,
                timeout,
                with_extra,
                meta,
                quick,
                serialized,
                &ended,
                &failed_plan,
                deferred,
            )?;
            drop(claim);
            Ok(AutoOutcome::Responded(resp))
        }
    }
}

#[allow(clippy::large_enum_variant)]
enum InPlaceWork {
    OpenErr(ApiError),
    Eager(
        sparkles::sparql::QueryResult,
        u64,
        Option<sparkles::history::Resolved>,
        Option<std::time::SystemTime>,
        Instant,
    ),
    CursorWhole {
        body: Vec<u8>,
        bytes: u64,
        meta: OpenMeta,
        stats: CursorStats,
        serialize_ms: f64,
    },
    CursorFailed {
        error: Error,
        plan: Option<serde_json::Value>,
        bytes: u64,
        meta: OpenMeta,
        stats: Option<CursorStats>,
        serialize_ms: f64,
    },
    CursorHandoff {
        writer: CursorWriter<LimitedWriter<stream::SwitchWriter>>,
        w: LimitedWriter<stream::SwitchWriter>,
        control: stream::StreamControl,
        meta: OpenMeta,
        began: Instant,
    },
    /// Only used if an `ASK` native JSON document (e.g. gigantic EXPLAIN plan) exceeds
    /// `STREAM_AFTER` bytes; `AskResult` is repeatable so it falls back to `serve`.
    CursorFallback {
        cursor: Box<QueryExecution>,
        meta: OpenMeta,
    },
}

/// In-place route for `execution=auto` when eligible (first run or quick cursor/ASK
/// history): opens and drains the cursor in place up to [`stream::STREAM_AFTER`] bytes,
/// returning a whole response with zero thread hops when it finishes under the threshold,
/// or handing off the cursor and buffered bytes to a blocking streaming task otherwise
/// (item 534).
pub(super) async fn run_in_place(ds: Arc<Dataset>, req: AutoRequest<'_>) -> ApiResult<AutoOutcome> {
    let AutoRequest {
        timeouts,
        query,
        options,
        at,
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
        quick_key,
        claim,
        ..
    } = req;
    let timeout = options.timeout;
    let with_extra = !options.default_graph_extra.is_empty();
    let send = params.get("send").and_then(|s| s.parse::<usize>().ok());
    let failed_plan_flag = format == SolutionsFormat::Sparkles;
    let dataset_id = ds.store.dataset_id().to_string();
    #[cfg(test)]
    let threshold = {
        let probe = super::algo::probe::current();
        if probe.producer.is_some() || probe.queued_producer.is_some() {
            1
        } else {
            stream::STREAM_AFTER
        }
    };
    #[cfg(not(test))]
    let threshold = stream::STREAM_AFTER;

    let run_ds = ds.clone();
    let run_prefixes = prefix_map.clone();
    let run_io = io.clone();
    let work = inline::run(move || -> InPlaceWork {
        let t = Instant::now();
        let t0 = crate::otel::start();
        let (snap, resolved) = match history::snapshot_for(&run_ds, at.as_ref(), &options) {
            Ok(v) => v,
            Err(e) => return InPlaceWork::OpenErr(e.into()),
        };
        let seq = snap.commit;
        let execution = match sparkles::sparql::query_execution_with_plan(
            snap,
            &query,
            &options,
            &Default::default(),
            ExecutionMode::Auto,
        ) {
            Ok(exec) => exec,
            Err(f) => return InPlaceWork::OpenErr(query_failure_error(f, failed_plan_flag, seq)),
        };
        let execution = match execution {
            QueryExecution::Eager(r) => {
                let r = r.into_result();
                tracing::debug!("query executed in {:?} ({} results)", t.elapsed(), r.len());
                return InPlaceWork::Eager(r, seq, resolved, t0, t);
            }
            other => other,
        };
        let derived = execution.reasoning();
        let kind = execution.kind();
        let fmt = ResolvedFormat::resolve(kind, format, thrift, rdf_format, native_graph);
        let control = stream::StreamControl::new(
            execution.cancellation_token(),
            execution.deadline(),
            execution.deadline_pause(),
            timeout,
        );
        let meta = OpenMeta {
            seq,
            resolved,
            derived,
            fmt,
            t0,
            quick_began: t,
        };
        let began = Instant::now();
        let sw = stream::SwitchWriter::new_in_place_cursor(threshold, control.clone());
        let ledger = sw.ledger();
        let mut w = LimitedWriter::new(sw, limit, None);
        match execution {
            QueryExecution::Eager(_) => unreachable!(),
            QueryExecution::Ask(result) => {
                let metadata = Some(results::NativeJsonMetadata {
                    commit: seq,
                    dataset_id: &dataset_id,
                    io: run_io.as_deref(),
                });
                let written = if format == SolutionsFormat::Sparkles {
                    results::write_native_json(result.result(), &mut w, send, metadata)
                } else {
                    results::write_solutions(result.result(), format, &mut w, send)
                }
                .map_err(|e| control.classify(w.classify(e)));
                let exec_stats = QueryExecution::Ask(result);
                let stats = exec_stats.stats();
                let serialize_ms =
                    (began.elapsed().as_secs_f64() * 1000.0 - stats.timing.exec_ms).max(0.0);
                match written {
                    Ok(()) if w.written() <= threshold as u64 => {
                        let bytes = w.written();
                        let body = w.into_inner().into_whole_body();
                        crate::otel::cursor_done(t0, meta.fmt.kind, &stats, serialize_ms);
                        InPlaceWork::CursorWhole {
                            body,
                            bytes,
                            meta,
                            stats,
                            serialize_ms,
                        }
                    }
                    Ok(()) => InPlaceWork::CursorFallback {
                        cursor: Box::new(exec_stats),
                        meta,
                    },
                    Err(error) => {
                        crate::otel::cursor_done(t0, meta.fmt.kind, &stats, serialize_ms);
                        InPlaceWork::CursorFailed {
                            error,
                            plan: None,
                            bytes: w.written(),
                            meta,
                            stats: Some(stats),
                            serialize_ms,
                        }
                    }
                }
            }
            QueryExecution::Select(cursor) => {
                let metadata =
                    (format == SolutionsFormat::Sparkles).then(|| OwnedNativeJsonMetadata {
                        commit: seq,
                        dataset_id,
                        io: run_io,
                    });
                let opts = CursorWriteOptions {
                    format: meta.fmt.solutions_format,
                    send,
                    prefixes: Vec::new(),
                    metadata,
                    ledger: Some(ledger),
                };
                let mut cursor_writer = match CursorWriter::select(*cursor, opts) {
                    Ok(cw) => cw,
                    Err(error) => {
                        let serialize_ms = began.elapsed().as_secs_f64() * 1000.0;
                        return InPlaceWork::CursorFailed {
                            error,
                            plan: None,
                            bytes: 0,
                            meta,
                            stats: None,
                            serialize_ms,
                        };
                    }
                };
                let (w, step) = cursor_writer.write_until(w, threshold as u64);
                match step {
                    Ok(WriteStep::Done(stats)) => {
                        let bytes = w.written();
                        let body = w.into_inner().into_whole_body();
                        let serialize_ms = (began.elapsed().as_secs_f64() * 1000.0
                            - stats.timing.exec_ms)
                            .max(0.0);
                        crate::otel::cursor_done(t0, meta.fmt.kind, &stats, serialize_ms);
                        InPlaceWork::CursorWhole {
                            body,
                            bytes,
                            meta,
                            stats,
                            serialize_ms,
                        }
                    }
                    Ok(WriteStep::Paused) => InPlaceWork::CursorHandoff {
                        writer: cursor_writer,
                        w,
                        control,
                        meta,
                        began,
                    },
                    Err(e) => {
                        let error = control.classify(w.classify(e));
                        let plan = if meta.fmt.sparkles_error_plan {
                            cursor_writer.plan_value()
                        } else {
                            None
                        };
                        let stats = cursor_writer.stats();
                        let serialize_ms = (began.elapsed().as_secs_f64() * 1000.0
                            - stats.timing.exec_ms)
                            .max(0.0);
                        crate::otel::cursor_done(t0, meta.fmt.kind, &stats, serialize_ms);
                        InPlaceWork::CursorFailed {
                            error,
                            plan,
                            bytes: w.written(),
                            meta,
                            stats: Some(stats),
                            serialize_ms,
                        }
                    }
                }
            }
            QueryExecution::Graph(cursor) => {
                let graph_format = if native_graph {
                    results::Format::Solutions(SolutionsFormat::Sparkles)
                } else {
                    rdf_format.format()
                };
                let metadata = native_graph.then(|| OwnedNativeJsonMetadata {
                    commit: seq,
                    dataset_id,
                    io: run_io,
                });
                let prefixes = run_prefixes.into_iter().collect::<Vec<(String, String)>>();
                let opts = CursorWriteOptions {
                    format: graph_format,
                    send,
                    prefixes,
                    metadata,
                    ledger: Some(ledger),
                };
                let mut cursor_writer = match CursorWriter::graph(*cursor, opts) {
                    Ok(cw) => cw,
                    Err(error) => {
                        let serialize_ms = began.elapsed().as_secs_f64() * 1000.0;
                        return InPlaceWork::CursorFailed {
                            error,
                            plan: None,
                            bytes: 0,
                            meta,
                            stats: None,
                            serialize_ms,
                        };
                    }
                };
                let (w, step) = cursor_writer.write_until(w, threshold as u64);
                match step {
                    Ok(WriteStep::Done(stats)) => {
                        let bytes = w.written();
                        let body = w.into_inner().into_whole_body();
                        let serialize_ms = (began.elapsed().as_secs_f64() * 1000.0
                            - stats.timing.exec_ms)
                            .max(0.0);
                        crate::otel::cursor_done(t0, meta.fmt.kind, &stats, serialize_ms);
                        InPlaceWork::CursorWhole {
                            body,
                            bytes,
                            meta,
                            stats,
                            serialize_ms,
                        }
                    }
                    Ok(WriteStep::Paused) => InPlaceWork::CursorHandoff {
                        writer: cursor_writer,
                        w,
                        control,
                        meta,
                        began,
                    },
                    Err(e) => {
                        let error = control.classify(w.classify(e));
                        let plan = if meta.fmt.sparkles_error_plan {
                            cursor_writer.plan_value()
                        } else {
                            None
                        };
                        let stats = cursor_writer.stats();
                        let serialize_ms = (began.elapsed().as_secs_f64() * 1000.0
                            - stats.timing.exec_ms)
                            .max(0.0);
                        crate::otel::cursor_done(t0, meta.fmt.kind, &stats, serialize_ms);
                        InPlaceWork::CursorFailed {
                            error,
                            plan,
                            bytes: w.written(),
                            meta,
                            stats: Some(stats),
                            serialize_ms,
                        }
                    }
                }
            }
        }
    });

    let work = match work {
        Ok(work) => work,
        Err(panic_err) => {
            QUICK.record(quick_key, f64::INFINITY);
            return Err(panic_err);
        }
    };
    match work {
        InPlaceWork::OpenErr(e) => {
            QUICK.record(quick_key, f64::INFINITY);
            Err(with_timeout(e, timeout))
        }
        InPlaceWork::Eager(result, seq, resolved, t0, began) => Ok(AutoOutcome::Eager {
            result,
            seq,
            resolved,
            t0,
            began,
            guard,
            prefixes: prefix_map,
        }),
        InPlaceWork::CursorWhole {
            body,
            bytes,
            meta,
            stats,
            serialize_ms,
        } => {
            #[cfg(test)]
            route_stats::note_in_place_whole(quick_key);
            if body.len() > stream::STREAM_AFTER {
                QUICK.record(quick_key, f64::INFINITY);
            } else if stats.status == CursorStatus::Complete {
                let ms = meta.quick_began.elapsed().as_secs_f64() * 1000.0;
                if meta.fmt.kind == QueryKind::Ask {
                    QUICK.record_ask(quick_key, ms);
                } else {
                    QUICK.record_cursor(quick_key, ms);
                }
            }
            drop(claim);
            let mut timing = stats.timing.clone();
            timing.serialize_ms = serialize_ms;
            let report = RequestReport {
                operation: Some(Op::Query),
                rows: Some(stats.emitted_rows),
                timing: Some(timing),
                serialize_ms: Some(serialize_ms),
                response_bytes: Some(bytes),
                mem_peak_bytes: Some(stats.mem_peak_bytes),
                rows_produced: Some(stats.rows_produced),
                cursor_status: Some(stats.status),
                transport_wait_ms: Some(0.0),
                ..Default::default()
            };
            let response = build_cursor_headers(
                &ds,
                params,
                uri,
                with_extra,
                meta.seq,
                meta.resolved.as_ref(),
                meta.derived.as_deref(),
                meta.fmt.content_type,
                Body::from(body),
            );
            Ok(AutoOutcome::Responded(report.attach(response)))
        }
        InPlaceWork::CursorFailed {
            error,
            plan,
            bytes,
            meta,
            stats,
            serialize_ms,
        } => {
            QUICK.record(quick_key, f64::INFINITY);
            drop(claim);
            let end = stream::StreamEnd {
                bytes,
                serialize_ms,
                error: Some(match &error {
                    Error::BudgetExceeded(b) => Error::BudgetExceeded(*b),
                    Error::Timeout => Error::Timeout,
                    Error::Cancelled => Error::Cancelled,
                    other => Error::Io(std::io::Error::other(other.to_string())),
                }),
                disconnected: false,
                rows: None,
                committed: false,
            };
            let (outcome, budget) = end.outcome();
            let mut timing = stats.as_ref().map(|s| s.timing.clone());
            if let Some(t) = &mut timing {
                t.serialize_ms = serialize_ms;
            }
            let report = RequestReport {
                operation: Some(Op::Query),
                outcome,
                budget,
                rows: None,
                timing,
                serialize_ms: Some(serialize_ms),
                response_bytes: Some(bytes),
                mem_peak_bytes: stats.as_ref().map(|s| s.mem_peak_bytes),
                rows_produced: stats.as_ref().map(|s| s.rows_produced),
                cursor_status: stats.as_ref().map(|s| s.status),
                transport_wait_ms: Some(0.0),
                ..Default::default()
            };
            let mut api_err = with_timeout(ApiError::from(error), timeout);
            if let (Some(plan), Some(body)) = (plan, api_err.1.as_object_mut()) {
                body.insert("plan".into(), plan);
                body.insert("commit".into(), meta.seq.into());
            }
            Ok(AutoOutcome::Responded(
                report.attach(api_err.into_response()),
            ))
        }
        InPlaceWork::CursorHandoff {
            mut writer,
            w,
            control,
            meta,
            began,
        } => {
            #[cfg(test)]
            route_stats::note_in_place_handoff(quick_key);
            let completed: Arc<parking_lot::Mutex<Option<CursorStats>>> = Default::default();
            let stats_slot = completed.clone();
            let ended: Arc<parking_lot::Mutex<Option<CursorStatus>>> = Default::default();
            let ended_slot = ended.clone();
            let failed_plan: Arc<parking_lot::Mutex<Option<serde_json::Value>>> =
                Default::default();
            let plan_slot = failed_plan.clone();
            let sent_rows: Arc<parking_lot::Mutex<Option<u64>>> = Default::default();
            let sent_slot = sent_rows.clone();
            let deferred = DeferredReport::default();
            let final_report = deferred.clone();
            let sparkles_error_plan = meta.fmt.sparkles_error_plan;
            let shared = SharedReportState {
                began,
                kind: meta.fmt.kind,
                t0: meta.t0,
                waits: control.clone(),
            };
            let serialized = stream::finish_handoff_controlled(
                w,
                control,
                timeouts,
                began,
                move |switched, w| {
                    let _claim = claim;
                    if let Err(error) = switched {
                        writer.fail_output(&error);
                        *sent_slot.lock() = Some(writer.rows_written());
                        *stats_slot.lock() = Some(writer.stats());
                        return (w, Err(error));
                    }
                    let (w, res) = writer.finish(w);
                    let res = res.map(|_| ()).map_err(|e| w.classify(e));
                    *sent_slot.lock() = Some(writer.rows_written());
                    if res.is_err() && sparkles_error_plan {
                        *plan_slot.lock() = writer.plan_value();
                    }
                    *stats_slot.lock() = Some(writer.stats());
                    (w, res)
                },
                move |end| {
                    finish_stream_end(
                        end,
                        completed.lock().take(),
                        *sent_rows.lock(),
                        &ended_slot,
                        &final_report,
                        &shared,
                    );
                },
            )
            .await;
            let quick = Some((quick_key, meta.quick_began));
            let resp = finish_serialized_response(
                &ds,
                params,
                uri,
                guard,
                timeout,
                with_extra,
                meta,
                quick,
                serialized,
                &ended,
                &failed_plan,
                deferred,
            )?;
            Ok(AutoOutcome::Responded(resp))
        }
        InPlaceWork::CursorFallback { cursor, meta } => {
            let resp = serve(
                ds,
                *cursor,
                meta.seq,
                meta.resolved,
                Serve {
                    timeouts,
                    params: Params(params.0.clone()),
                    uri: uri.clone(),
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
                    t0: meta.t0,
                    quick: Some((quick_key, meta.quick_began)),
                },
            )
            .await?;
            drop(claim);
            Ok(AutoOutcome::Responded(resp))
        }
    }
}

fn query_failure_error(f: Box<sparkles::sparql::Failed>, failed_plan: bool, seq: u64) -> ApiError {
    let mut e: ApiError = f.error.into();
    if failed_plan && let (Some(plan), Some(body)) = (f.plan, e.1.as_object_mut()) {
        let value = sparkles::sparql::depth::with_stack(plan.depth(), move || {
            serde_json::to_value(&plan).unwrap_or_default()
        });
        body.insert("plan".into(), value);
        body.insert("commit".into(), seq.into());
    }
    e
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
    let derived = cursor.reasoning();
    let kind = cursor.kind();
    let fmt = ResolvedFormat::resolve(kind, format, thrift, rdf_format, native_graph);
    let write_fmt = ResolvedFormat::resolve(kind, format, thrift, rdf_format, native_graph);
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
    let ended: Arc<parking_lot::Mutex<Option<CursorStatus>>> = Default::default();
    let ended_slot = ended.clone();
    let failed_plan: Arc<parking_lot::Mutex<Option<serde_json::Value>>> = Default::default();
    let plan_slot = failed_plan.clone();
    let sent_rows: Arc<parking_lot::Mutex<Option<u64>>> = Default::default();
    let sent_slot = sent_rows.clone();
    let deferred = DeferredReport::default();
    let final_report = deferred.clone();
    let began = Instant::now();
    let shared = SharedReportState {
        began,
        kind,
        t0,
        waits: control.clone(),
    };
    let dataset_id = ds.store.dataset_id().to_string();
    // A request that asked for streaming gets its first bytes after the first batch.
    let early = quick.is_none();
    let serialized = stream::serialize_controlled(
        limit,
        control,
        early,
        timeouts,
        move |writer| {
            write_cursor_execution(
                &mut cursor,
                writer,
                seq,
                &dataset_id,
                io.as_deref(),
                format,
                rdf_format,
                native_graph,
                send,
                &prefix_map,
                &prefixes,
                &write_fmt,
                &sent_slot,
                &plan_slot,
                &stats_slot,
            )
        },
        move |end| {
            finish_stream_end(
                end,
                completed.lock().take(),
                *sent_rows.lock(),
                &ended_slot,
                &final_report,
                &shared,
            );
        },
    )
    .await;
    let quick_began = quick.map_or(began, |(_, b)| b);
    finish_serialized_response(
        &ds,
        &params,
        &uri,
        guard,
        timeout,
        with_extra,
        OpenMeta {
            seq,
            resolved,
            derived,
            fmt,
            t0,
            quick_began,
        },
        quick,
        serialized,
        &ended,
        &failed_plan,
        deferred,
    )
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

/// Test-only counters of which cursor route handled a request (keyed by quick-query key
/// or global total), so router tests can assert in-place whole, in-place hand-off, and
/// pool-routed execution deterministically under parallel test runs.
#[cfg(test)]
pub(super) mod route_stats {
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(super) static POOL_STREAMING: AtomicU64 = AtomicU64::new(0);
    pub(super) static IN_PLACE_WHOLE: LazyLock<Mutex<HashMap<u64, u64>>> =
        LazyLock::new(Default::default);
    pub(super) static IN_PLACE_HANDOFF: LazyLock<Mutex<HashMap<u64, u64>>> =
        LazyLock::new(Default::default);
    pub(super) static POOL_AUTO: LazyLock<Mutex<HashMap<u64, u64>>> =
        LazyLock::new(Default::default);

    pub(super) fn note_pool_streaming() {
        POOL_STREAMING.fetch_add(1, Ordering::Relaxed);
    }
    pub(super) fn note_in_place_whole(key: u64) {
        *IN_PLACE_WHOLE.lock().entry(key).or_default() += 1;
    }
    pub(super) fn note_in_place_handoff(key: u64) {
        *IN_PLACE_HANDOFF.lock().entry(key).or_default() += 1;
    }
    pub(super) fn note_pool_auto(key: u64) {
        *POOL_AUTO.lock().entry(key).or_default() += 1;
    }

    pub(crate) fn in_place_whole(key: u64) -> u64 {
        IN_PLACE_WHOLE.lock().get(&key).copied().unwrap_or(0)
    }
    pub(crate) fn in_place_handoff(key: u64) -> u64 {
        IN_PLACE_HANDOFF.lock().get(&key).copied().unwrap_or(0)
    }
    pub(crate) fn pool_auto(key: u64) -> u64 {
        POOL_AUTO.lock().get(&key).copied().unwrap_or(0)
    }
    pub(crate) fn pool_streaming() -> u64 {
        POOL_STREAMING.load(Ordering::Relaxed)
    }
}
