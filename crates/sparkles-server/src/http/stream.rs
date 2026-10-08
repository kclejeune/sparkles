//! Response bodies serialized on a blocking thread: returned whole while small, streamed
//! once they pass [`STREAM_AFTER`] bytes.
//!
//! A whole body keeps the ordinary behavior: `Content-Length`, a proper error status when
//! serialization fails, and a complete request report. A streamed body starts with what
//! was buffered and continues in [`CHUNK`]-sized pieces through a bounded channel, so
//! serialized-body buffering stays bounded. The query result itself may already be
//! materialized. An error after the switch aborts the transfer (the client sees a
//! truncated response), and a client that goes away stops the serialization.

use super::{ApiError, ApiResult, err};
use axum::body::Bytes;
use axum::http::StatusCode;
use sparkles::Error;
use sparkles::sparql::results::LimitedWriter;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// Bodies up to this size are returned whole.
pub const STREAM_AFTER: usize = 1 << 20;
/// Size of the chunks of a streamed body.
pub const CHUNK: usize = 64 << 10;

type Chunk = io::Result<Bytes>;

/// A serialized response body.
pub enum Serialized {
    Whole { body: Vec<u8>, serialize_ms: f64 },
    Streamed(axum::body::Body),
}

/// How a streamed body ended, reported on the serializing thread.
pub struct StreamEnd {
    pub bytes: u64,
    pub serialize_ms: f64,
    pub error: Option<Error>,
    /// the client went away before the end
    pub disconnected: bool,
}

enum Outcome {
    Whole(Vec<u8>),
    Stream(mpsc::Receiver<Chunk>),
    Failed(Error),
}

/// The writer serializers write to: buffers until the threshold, then streams.
pub struct SwitchWriter {
    buf: Vec<u8>,
    threshold: usize,
    signal: Option<oneshot::Sender<Outcome>>,
    tx: Option<mpsc::Sender<Chunk>>,
    deferred: bool,
    control: Option<StreamControl>,
}

/// Controls for a native cursor's producer, including time spent waiting for a
/// slow reader. Eager serialization keeps its existing blocking-send path.
#[derive(Clone)]
pub(super) struct StreamControl {
    cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
    wait_ns: Arc<AtomicU64>,
}

impl StreamControl {
    pub(super) fn new(cancel: Arc<AtomicBool>, deadline: Option<Instant>) -> Self {
        Self {
            cancel,
            deadline,
            wait_ns: Default::default(),
        }
    }
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "query cancelled",
            ));
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "query timed out"));
        }
        Ok(())
    }
    fn classify(&self, error: Error) -> Error {
        if let Error::Io(io) = &error {
            if io.kind() == io::ErrorKind::TimedOut {
                return Error::Timeout;
            }
            if io.kind() == io::ErrorKind::ConnectionAborted {
                return Error::Cancelled;
            }
        }
        error
    }
    pub(super) fn wait_ms(&self) -> f64 {
        self.wait_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }
}

fn copy_error(error: &Error) -> Error {
    match error {
        Error::BudgetExceeded(budget) => Error::BudgetExceeded(*budget),
        Error::Timeout => Error::Timeout,
        Error::Cancelled => Error::Cancelled,
        Error::Io(e) => Error::Io(io::Error::new(e.kind(), e.to_string())),
        _ => Error::Io(io::Error::other(error.to_string())),
    }
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "client disconnected")
}

impl SwitchWriter {
    /// A capped synchronous trial reached its byte threshold and will be retried
    /// on the blocking pool. That trial must not report final serialization timing.
    pub(super) fn deferred(&self) -> bool {
        self.deferred
    }

    fn send_chunk(&mut self) -> io::Result<()> {
        let Some(tx) = &self.tx else {
            return Ok(());
        };
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK));
        let Some(control) = &self.control else {
            return tx.blocking_send(Ok(Bytes::from(chunk))).map_err(|_| gone());
        };
        let mut item = Ok(Bytes::from(chunk));
        loop {
            control.check()?;
            match tx.try_send(item) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(gone()),
                Err(mpsc::error::TrySendError::Full(returned)) => {
                    item = returned;
                    let began = Instant::now();
                    // Capacity wakes the producer immediately; the finite timeout
                    // also checks controls when the reader never polls again.
                    let capacity = tokio::runtime::Handle::current().block_on(async {
                        tokio::time::timeout(Duration::from_millis(10), tx.reserve()).await
                    });
                    let ns = began.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                    let _ =
                        control
                            .wait_ns
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                                Some(n.saturating_add(ns))
                            });
                    match capacity {
                        Ok(Ok(permit)) => {
                            control.check()?;
                            permit.send(item);
                            return Ok(());
                        }
                        Ok(Err(_)) => return Err(gone()),
                        Err(_) => (),
                    }
                }
            }
        }
    }
}

impl Write for SwitchWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if let Some(control) = &self.control {
            control.check()?;
        }
        if self.signal.is_none() && self.tx.is_none() {
            // A quick result is tried synchronously, but its row count says nothing
            // about literal size. Stop before allocating beyond the byte threshold;
            // the caller retries this immutable result through the bounded stream.
            if b.len() > self.threshold.saturating_sub(self.buf.len()) {
                self.deferred = true;
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "response requires streaming",
                ));
            }
            self.buf.extend_from_slice(b);
            return Ok(b.len());
        }
        // A serializer may hand us one enormous literal/body. Consume only the
        // prefix needed to switch, or to fill a streamed chunk, rather than
        // copying that entire slice before applying backpressure. `write_all`
        // retries the rest through the ordinary Write contract.
        let room = if self.tx.is_some() {
            CHUNK - self.buf.len()
        } else {
            self.threshold
                .saturating_sub(self.buf.len())
                .saturating_add(1)
        };
        let n = b.len().min(room);
        self.buf.extend_from_slice(&b[..n]);
        if self.tx.is_none() && self.buf.len() > self.threshold {
            // switch to streaming: hand the receiver to the handler first
            let (tx, rx) = mpsc::channel(4);
            let signal = self.signal.take().expect("signal before the switch");
            signal.send(Outcome::Stream(rx)).map_err(|_| gone())?;
            self.tx = Some(tx);
            self.send_chunk()?;
        } else if self.tx.is_some() && self.buf.len() >= CHUNK {
            self.send_chunk()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serialize a body with `write` on a blocking thread, under the result-size `limit`.
/// `on_stream_end` runs on that thread when a *streamed* body ends (whole bodies are
/// reported by the caller). An error before the switch is returned as the response
/// error.
pub async fn serialize<F, D>(
    limit: Option<u64>,
    write: F,
    on_stream_end: D,
) -> ApiResult<Serialized>
where
    F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    serialize_with(STREAM_AFTER, limit, write, on_stream_end).await
}

pub async fn serialize_with<F, D>(
    threshold: usize,
    limit: Option<u64>,
    write: F,
    on_stream_end: D,
) -> ApiResult<Serialized>
where
    F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    serialize_inner(threshold, limit, None, write, on_stream_end).await
}

pub(super) async fn serialize_controlled<F, D>(
    limit: Option<u64>,
    control: StreamControl,
    write: F,
    on_end: D,
) -> ApiResult<Serialized>
where
    F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    serialize_inner(STREAM_AFTER, limit, Some(control), write, on_end).await
}

async fn serialize_inner<F, D>(
    threshold: usize,
    limit: Option<u64>,
    control: Option<StreamControl>,
    write: F,
    on_stream_end: D,
) -> ApiResult<Serialized>
where
    F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    let (signal, outcome) = oneshot::channel();
    // A terminal failure does not need space in the already full data channel.
    let terminal: Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let body_terminal = terminal.clone();
    let span = tracing::Span::current();
    let held = crate::ratelimit::hold();
    let t0 = Instant::now();
    tokio::task::spawn_blocking(move || {
        // released before the body is handed over: this may be the request span's last
        // handle, and the span ends (and is exported) when that drops, which must come
        // before the client can see the response
        let span = span.entered();
        let sw = SwitchWriter {
            buf: Vec::new(),
            threshold,
            signal: Some(signal),
            tx: None,
            deferred: false,
            control: control.clone(),
        };
        // disconnects are noticed through the channel, not a cancellation flag
        let mut w = LimitedWriter::new(sw, limit, None::<Arc<AtomicBool>>);
        let result = if control.is_some() {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write(&mut w))).unwrap_or_else(
                |_| {
                    Err(Error::Io(io::Error::other(
                        "query producer stopped unexpectedly",
                    )))
                },
            )
        } else {
            write(&mut w)
        };
        let result = result.map_err(|e| w.classify(e));
        let bytes = w.written();
        let mut sw = w.into_inner();
        let result = result.and_then(|()| sw.send_chunk().map_err(Error::from));
        let result = result.map_err(|e| match &control {
            Some(c) => c.classify(e),
            None => e,
        });
        let serialize_ms = t0.elapsed().as_secs_f64() * 1000.0;
        match (sw.tx.take(), result) {
            (None, Ok(())) => {
                let body = std::mem::take(&mut sw.buf);
                if control.is_some() {
                    on_stream_end(StreamEnd {
                        bytes,
                        serialize_ms,
                        error: None,
                        disconnected: false,
                    });
                }
                drop(span);
                // Completion may wake the handler immediately. Release the
                // producer's share before publishing the buffered result.
                drop(held);
                if let Some(s) = sw.signal.take() {
                    let _ = s.send(Outcome::Whole(body));
                }
            }
            (None, Err(e)) => {
                if control.is_some() {
                    on_stream_end(StreamEnd {
                        bytes,
                        serialize_ms,
                        error: Some(copy_error(&e)),
                        disconnected: false,
                    });
                    drop(span);
                    drop(held);
                    if let Some(s) = sw.signal.take() {
                        let _ = s.send(Outcome::Failed(e));
                    }
                    return;
                }
                drop(span);
                drop(held);
                if let Some(s) = sw.signal.take() {
                    let _ = s.send(Outcome::Failed(e));
                }
            }
            (Some(tx), result) => {
                let disconnected = tx.is_closed();
                let error = result.err();
                let abort = error
                    .as_ref()
                    .filter(|_| !disconnected)
                    .map(|e| e.to_string());
                // reported before the client can see the end of the body
                on_stream_end(StreamEnd {
                    bytes,
                    serialize_ms,
                    error,
                    disconnected,
                });
                drop(span);
                if let Some(msg) = abort {
                    // an error item aborts the response: the client sees a truncated
                    // transfer rather than a complete-looking one
                    if control.is_some() {
                        *terminal.lock().expect("terminal error lock") = Some(msg);
                    } else {
                        let _ = tx.blocking_send(Err(io::Error::other(msg)));
                    }
                }
                // Closing the sender publishes EOF. The next request must not
                // observe the completed producer's concurrency permit.
                drop(held);
                drop(tx);
            }
        }
    });
    match outcome.await {
        Ok(Outcome::Whole(body)) => Ok(Serialized::Whole {
            body,
            serialize_ms: t0.elapsed().as_secs_f64() * 1000.0,
        }),
        Ok(Outcome::Stream(rx)) => Ok(Serialized::Streamed(axum::body::Body::from_stream(
            ChunkStream {
                receiver: rx,
                terminal: body_terminal,
                done: false,
            },
        ))),
        Ok(Outcome::Failed(e)) => Err(ApiError::from(e)),
        Err(_) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the serializer stopped unexpectedly",
        )),
    }
}

/// Try a quick result on the current thread, buffering at most [`STREAM_AFTER`]
/// bytes. A larger result retries its repeatable serializer on the blocking pool,
/// where it can apply channel backpressure. Real errors retain their usual behavior.
pub async fn serialize_quick<F, D>(
    limit: Option<u64>,
    write: F,
    on_stream_end: D,
) -> ApiResult<Serialized>
where
    F: Fn(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    let t0 = Instant::now();
    if let Some(body) = try_serialize_now(STREAM_AFTER, limit, &write)? {
        Ok(body)
    } else {
        let trial_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let body = serialize(limit, write, move |mut end| {
            end.serialize_ms += trial_ms;
            on_stream_end(end);
        })
        .await?;
        Ok(match body {
            Serialized::Whole { body, serialize_ms } => Serialized::Whole {
                body,
                serialize_ms: trial_ms + serialize_ms,
            },
            body => body,
        })
    }
}

fn try_serialize_now<F>(
    threshold: usize,
    limit: Option<u64>,
    write: &F,
) -> ApiResult<Option<Serialized>>
where
    F: Fn(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()>,
{
    let t0 = Instant::now();
    // No signal: reaching the threshold aborts this bounded trial.
    let sw = SwitchWriter {
        buf: Vec::new(),
        threshold,
        signal: None,
        tx: None,
        deferred: false,
        control: None,
    };
    let mut w = LimitedWriter::new(sw, limit, None::<Arc<AtomicBool>>);
    let result = write(&mut w).map_err(|e| w.classify(e));
    if w.get_ref().deferred() {
        return Ok(None);
    }
    match result {
        Ok(()) => Ok(Some(Serialized::Whole {
            body: w.into_inner().buf,
            serialize_ms: t0.elapsed().as_secs_f64() * 1000.0,
        })),
        Err(e) => Err(ApiError::from(e)),
    }
}

/// The receiving end of a [`SwitchWriter`] as a body stream. It keeps answering `None`
/// after the end, since the compression layer polls once more.
struct ChunkStream {
    receiver: mpsc::Receiver<Chunk>,
    terminal: Arc<std::sync::Mutex<Option<String>>>,
    done: bool,
}

impl futures_util::Stream for ChunkStream {
    type Item = Chunk;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.done {
            return std::task::Poll::Ready(None);
        }
        match self.receiver.poll_recv(cx) {
            std::task::Poll::Ready(None) => {
                self.done = true;
                let error = self.terminal.lock().expect("terminal error lock").take();
                std::task::Poll::Ready(error.map(|e| Err(io::Error::other(e))))
            }
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn ends() -> (
        Arc<Mutex<Vec<StreamEnd>>>,
        impl FnOnce(StreamEnd) + Send + 'static,
    ) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        (seen, move |e| s.lock().unwrap().push(e))
    }

    fn write_n(
        n: usize,
        fail_after: Option<usize>,
    ) -> impl FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static
    {
        move |w| {
            for i in 0..n {
                if fail_after == Some(i) {
                    return Err(Error::invalid("boom"));
                }
                w.write_all(b"0123456789")?;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn small_bodies_are_whole() {
        let (seen, on_end) = ends();
        match serialize_with(100, None, write_n(5, None), on_end).await {
            Ok(Serialized::Whole { body, .. }) => assert_eq!(body.len(), 50),
            _ => panic!("expected a whole body"),
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn quick_trial_accepts_the_byte_boundary_and_preserves_real_errors() {
        let write = |w: &mut LimitedWriter<SwitchWriter>| {
            w.write_all(&[b'x'; 100])?;
            Ok(())
        };
        let Ok(Some(Serialized::Whole { body, .. })) = try_serialize_now(100, None, &write) else {
            panic!("exactly the threshold should remain whole");
        };
        assert_eq!(body, vec![b'x'; 100]);
        assert!(matches!(try_serialize_now(99, None, &write), Ok(None)));
        assert!(matches!(
            try_serialize_now(100, Some(50), &write),
            Err(ApiError(StatusCode::INSUFFICIENT_STORAGE, _))
        ));
        let fail = |_: &mut LimitedWriter<SwitchWriter>| Err(Error::invalid("boom"));
        assert!(matches!(
            try_serialize_now(100, None, &fail),
            Err(ApiError(StatusCode::BAD_REQUEST, _))
        ));
    }

    #[tokio::test]
    async fn quick_large_answer_retries_once_as_a_bounded_stream() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let size = STREAM_AFTER + CHUNK * 8 + 17;
        let input = vec![b'x'; size];
        let (seen, on_end) = ends();
        let Ok(Serialized::Streamed(body)) = serialize_quick(
            None,
            move |w| {
                count.fetch_add(1, Ordering::Relaxed);
                w.write_all(&input)?;
                Ok(())
            },
            on_end,
        )
        .await
        else {
            panic!("few cells with huge bytes must stream");
        };
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            axum::body::to_bytes(body, usize::MAX).await.unwrap().len(),
            size
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].bytes, size as u64);
        assert!(seen[0].error.is_none() && !seen[0].disconnected);
    }

    #[tokio::test]
    async fn large_bodies_stream_and_report_their_end() {
        let (seen, on_end) = ends();
        let Ok(Serialized::Streamed(body)) =
            serialize_with(100, None, write_n(20_000, None), on_end).await
        else {
            panic!("expected a stream");
        };
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(bytes.len(), 200_000);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].bytes, 200_000);
        assert!(seen[0].error.is_none() && !seen[0].disconnected);
    }

    // A serializer can issue one write_all for a very large literal. Bound each
    // underlying write even then, and apply channel backpressure before consuming
    // the rest of that literal.
    async fn single_large_write() -> (
        mpsc::Receiver<Chunk>,
        tokio::task::JoinHandle<io::Result<()>>,
        usize,
    ) {
        let (signal, outcome) = oneshot::channel();
        let size = CHUNK * 8 + 17;
        let producer = tokio::task::spawn_blocking(move || {
            let mut w = SwitchWriter {
                buf: Vec::new(),
                threshold: 100,
                signal: Some(signal),
                tx: None,
                deferred: false,
                control: None,
            };
            w.write_all(&vec![b'x'; size])?;
            w.send_chunk()
        });
        let Outcome::Stream(rx) = outcome.await.unwrap() else {
            panic!("expected a stream");
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while rx.len() < 4 && !producer.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("producer should fill the bounded channel");
        (rx, producer, size)
    }

    #[tokio::test]
    async fn a_single_large_write_has_bounded_chunks_and_backpressure() {
        let (mut rx, producer, size) = single_large_write().await;
        assert_eq!(rx.len(), 4);
        assert!(
            !producer.is_finished(),
            "the full channel must block writes"
        );
        let mut bytes = Vec::new();
        let mut chunks = 0;
        while let Some(chunk) = rx.recv().await {
            let chunk = chunk.unwrap();
            assert!(chunk.len() <= if chunks == 0 { 101 } else { CHUNK });
            bytes.extend_from_slice(&chunk);
            chunks += 1;
        }
        producer.await.unwrap().unwrap();
        assert_eq!(bytes, vec![b'x'; size]);
    }

    #[tokio::test]
    async fn disconnect_interrupts_a_single_large_write() {
        let (rx, producer, _) = single_large_write().await;
        assert_eq!(rx.len(), 4);
        assert!(!producer.is_finished());
        drop(rx);
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), producer)
            .await
            .expect("disconnect must unblock the producer")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn errors_before_the_switch_are_responses() {
        let (_, on_end) = ends();
        let r = serialize_with(100, None, write_n(20, Some(3)), on_end).await;
        assert!(matches!(r, Err(ApiError(StatusCode::BAD_REQUEST, _))));
        // so is a result-size budget exceeded while still buffering
        let (_, on_end) = ends();
        let r = serialize_with(100, Some(50), write_n(20, None), on_end).await;
        assert!(matches!(
            r,
            Err(ApiError(StatusCode::INSUFFICIENT_STORAGE, _))
        ));
    }

    #[tokio::test]
    async fn errors_after_the_switch_abort_the_body() {
        let (seen, on_end) = ends();
        let Ok(Serialized::Streamed(body)) =
            serialize_with(100, None, write_n(20_000, Some(10_000)), on_end).await
        else {
            panic!("expected a stream");
        };
        assert!(axum::body::to_bytes(body, usize::MAX).await.is_err());
        let seen = seen.lock().unwrap();
        assert!(seen[0].error.is_some() && !seen[0].disconnected);
    }

    #[tokio::test]
    async fn a_dropped_body_stops_the_serializer() {
        let (seen, on_end) = ends();
        let Ok(Serialized::Streamed(body)) =
            serialize_with(100, None, write_n(10_000_000, None), on_end).await
        else {
            panic!("expected a stream");
        };
        drop(body);
        for _ in 0..200 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let seen = seen.lock().unwrap();
        assert!(seen[0].disconnected, "the serializer should notice");
        assert!(seen[0].bytes < 100_000_000);
    }

    #[tokio::test]
    async fn controlled_waits_expire_without_reading_or_terminal_channel_space() {
        let (seen, on_end) = ends();
        let control = StreamControl::new(
            Default::default(),
            Some(Instant::now() + Duration::from_millis(150)),
        );
        let waits = control.clone();
        let Ok(Serialized::Streamed(body)) =
            serialize_inner(100, None, Some(control), write_n(10_000_000, None), on_end).await
        else {
            panic!("expected a stream");
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while seen.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("a stalled reader must not hold the producer past its deadline");
        assert!(matches!(
            seen.lock().unwrap()[0].error,
            Some(Error::Timeout)
        ));
        assert!(waits.wait_ms() > 0.0);
        assert!(
            axum::body::to_bytes(body, usize::MAX).await.is_err(),
            "a late timeout must abort even when the data channel was full"
        );
    }

    #[tokio::test]
    async fn controlled_waits_observe_cancellation_and_report_only_once() {
        let (seen, on_end) = ends();
        let cancel = Arc::new(AtomicBool::new(false));
        let Ok(Serialized::Streamed(body)) = serialize_inner(
            100,
            None,
            Some(StreamControl::new(cancel.clone(), None)),
            write_n(10_000_000, None),
            on_end,
        )
        .await
        else {
            panic!("expected a stream")
        };
        cancel.store(true, Ordering::Relaxed);
        assert!(axum::body::to_bytes(body, usize::MAX).await.is_err());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(matches!(seen[0].error, Some(Error::Cancelled)));
    }

    #[tokio::test]
    async fn controlled_early_errors_preserve_budget_status_and_report() {
        let (seen, on_end) = ends();
        let r = serialize_inner(
            100,
            Some(50),
            Some(StreamControl::new(Default::default(), None)),
            write_n(20, None),
            on_end,
        )
        .await;
        assert!(matches!(
            r,
            Err(ApiError(StatusCode::INSUFFICIENT_STORAGE, _))
        ));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(matches!(seen[0].error, Some(Error::BudgetExceeded(_))));
    }
}
