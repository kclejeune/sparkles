//! Response bodies serialized on a blocking thread: returned whole while small, streamed
//! once they pass [`STREAM_AFTER`] bytes.
//!
//! A whole body keeps the ordinary behavior: `Content-Length`, a proper error status when
//! serialization fails, and a complete request report. A streamed body starts with what
//! was buffered and continues in [`CHUNK`]-sized pieces through a bounded channel, so
//! memory stays flat. An error after the switch aborts the transfer (the client sees a
//! truncated response), and a client that goes away stops the serialization.

use super::{ApiError, ApiResult, err};
use axum::body::Bytes;
use axum::http::StatusCode;
use sparkles::Error;
use sparkles::sparql::results::LimitedWriter;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
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
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "client disconnected")
}

impl SwitchWriter {
    fn send_chunk(&mut self) -> io::Result<()> {
        let Some(tx) = &self.tx else {
            return Ok(());
        };
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK));
        tx.blocking_send(Ok(Bytes::from(chunk))).map_err(|_| gone())
    }
}

impl Write for SwitchWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(b);
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
        Ok(b.len())
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
    let (signal, outcome) = oneshot::channel();
    let span = tracing::Span::current();
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
        };
        // disconnects are noticed through the channel, not a cancellation flag
        let mut w = LimitedWriter::new(sw, limit, None::<Arc<AtomicBool>>);
        let result = write(&mut w).map_err(|e| w.classify(e));
        let bytes = w.written();
        let mut sw = w.into_inner();
        let result = result.and_then(|()| sw.send_chunk().map_err(Error::from));
        let serialize_ms = t0.elapsed().as_secs_f64() * 1000.0;
        match (sw.tx.take(), result) {
            (None, Ok(())) => {
                let body = std::mem::take(&mut sw.buf);
                drop(span);
                if let Some(s) = sw.signal.take() {
                    let _ = s.send(Outcome::Whole(body));
                }
            }
            (None, Err(e)) => {
                drop(span);
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
                    let _ = tx.blocking_send(Err(io::Error::other(msg)));
                }
            }
        }
    });
    match outcome.await {
        Ok(Outcome::Whole(body)) => Ok(Serialized::Whole {
            body,
            serialize_ms: t0.elapsed().as_secs_f64() * 1000.0,
        }),
        Ok(Outcome::Stream(rx)) => Ok(Serialized::Streamed(axum::body::Body::from_stream(
            ChunkStream(rx),
        ))),
        Ok(Outcome::Failed(e)) => Err(ApiError::from(e)),
        Err(_) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the serializer stopped unexpectedly",
        )),
    }
}

/// Serialize a body with `write` on the current thread, whole, under the result-size
/// `limit`: for results small enough that a thread hand-off would cost more than the
/// serialization.
pub fn serialize_now<F>(limit: Option<u64>, write: F) -> ApiResult<Serialized>
where
    F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()>,
{
    let t0 = Instant::now();
    // no signal, and a threshold it never passes: the writer only buffers
    let sw = SwitchWriter {
        buf: Vec::new(),
        threshold: usize::MAX,
        signal: None,
        tx: None,
    };
    let mut w = LimitedWriter::new(sw, limit, None::<Arc<AtomicBool>>);
    match write(&mut w).map_err(|e| w.classify(e)) {
        Ok(()) => Ok(Serialized::Whole {
            body: w.into_inner().buf,
            serialize_ms: t0.elapsed().as_secs_f64() * 1000.0,
        }),
        Err(e) => Err(ApiError::from(e)),
    }
}

/// The receiving end of a [`SwitchWriter`] as a body stream. It keeps answering `None`
/// after the end, since the compression layer polls once more.
struct ChunkStream(mpsc::Receiver<Chunk>);

impl futures_util::Stream for ChunkStream {
    type Item = Chunk;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
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
}
