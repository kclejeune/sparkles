//! Response bodies serialized on a blocking thread: returned whole while small, streamed
//! once they pass [`STREAM_AFTER`] bytes.
//!
//! A whole body keeps the ordinary behavior: `Content-Length`, a proper error status when
//! serialization fails, and a complete request report. A streamed body starts with what
//! was buffered and continues in [`CHUNK`]-sized pieces through a bounded channel, so
//! serialized-body buffering stays bounded. The query result itself may already be
//! materialized. An error after the switch aborts the transfer (the client sees a
//! truncated response), and a client that goes away stops the serialization. A body that
//! reaches the result-size budget after the switch first sends the bytes it had buffered,
//! so the client receives everything the serializer wrote before the budget stopped it.

use super::{ApiError, ApiResult, err};
use axum::body::Bytes;
use axum::http::StatusCode;
use sparkles::Error;
use sparkles::sparql::results::{LimitedWriter, RowLedger};
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

mod lifetime;
pub(crate) use lifetime::{Abort, Timeouts};

/// Bodies up to this size are returned whole.
pub const STREAM_AFTER: usize = 1 << 20;
/// An explicitly streamed body commits the response once it holds this many bytes, or
/// once its writer flushes after the first batch, whichever comes first.
pub const FIRST_BYTES_AFTER: usize = 64 << 10;
/// Size of the chunks of a streamed body.
pub const CHUNK: usize = 64 << 10;
/// Bounded in-flight channel entries per streamed response (16 entries). Post-switch
/// chunks are at most [`CHUNK`] (64 KiB), while a threshold-driven switch queues its
/// initial buffer as one entry (up to [`STREAM_AFTER`] + 1 byte before the remaining 15
/// chunks). Raising the queue from 4 to 16 entries adds at most 768 KiB of queued payload
/// per active streamed response so decoded cursor batches can enqueue without stalling the
/// blocking producer when queue space allows.
pub const CHANNEL_CAP: usize = 16;
/// A controlled writer checks its deadline whenever its buffer crosses a multiple of
/// 2^CONTROL_SHIFT bytes.
const CONTROL_SHIFT: u32 = 14;

type Chunk = io::Result<Bytes>;

/// A serialized response body.
pub(super) enum Serialized {
    Whole {
        body: Vec<u8>,
        serialize_ms: f64,
    },
    Streamed(axum::body::Body),
    Timed {
        body: axum::body::Body,
        abort: Abort,
    },
}

impl Serialized {
    pub(crate) fn abort_handle(&self) -> Option<Abort> {
        match self {
            Self::Timed { abort, .. } => Some(abort.clone()),
            _ => None,
        }
    }
}

/// How a streamed body ended, reported on the serializing thread.
pub struct StreamEnd {
    pub bytes: u64,
    pub serialize_ms: f64,
    pub error: Option<Error>,
    /// the client went away before the end
    pub disconnected: bool,
    /// The rows whose bytes the transport accepted, from the writer's row ledger, or
    /// `None` when the writer recorded no rows there or the body failed before it was
    /// committed. A row counts once the send that holds its end has enqueued.
    pub rows: Option<u64>,
    /// The response was committed: it switched to streaming, or its whole body went
    /// out. A body that failed before then is answered with an error status, and no
    /// row of it reached the client.
    pub committed: bool,
}

impl StreamEnd {
    /// The outcome and budget that the access log and the metrics record for a body that
    /// ended this way. A complete body has no outcome of its own.
    pub(crate) fn outcome(&self) -> (Option<crate::obs::Outcome>, Option<sparkles::BudgetKind>) {
        use crate::obs::Outcome as O;
        match &self.error {
            Some(Error::Timeout) => (Some(O::Timeout), None),
            Some(Error::Cancelled) => (Some(O::Cancelled), None),
            Some(Error::BudgetExceeded(b)) => (Some(O::Budget), Some(b.kind)),
            Some(_) if self.disconnected => (Some(O::Cancelled), None),
            Some(_) => (Some(O::Error), None),
            None => (None, None),
        }
    }

    /// The rows that the access log and the metrics report for a body that ended this way.
    /// A body that failed before it was committed is an error response, which sent none of
    /// its rows. One that failed after reports the rows whose ends the transport handed
    /// over, or for a writer that records none there the rows it `written` whole, and
    /// otherwise the `complete` count. A complete body reports `complete`.
    pub(crate) fn reported_rows(&self, written: Option<u64>, complete: Option<u64>) -> Option<u64> {
        if !self.committed {
            None
        } else if self.error.is_some() {
            self.rows.or(written).or(complete)
        } else {
            complete
        }
    }
}

enum Outcome {
    Whole(Vec<u8>),
    Stream(mpsc::Receiver<Chunk>),
    Timed(lifetime::Receiver),
    Failed(Error),
    EarlyReturn,
}

/// The writer serializers write to: buffers until the threshold, then streams.
pub struct SwitchWriter {
    buf: Vec<u8>,
    threshold: usize,
    signal: Option<oneshot::Sender<Outcome>>,
    tx: Option<mpsc::Sender<Chunk>>,
    deferred: bool,
    /// In-place cursor trial: keep accepting writes past `threshold` without failing with
    /// `WouldBlock`, so the cursor can pause cleanly at a row/batch boundary and hand off
    /// this writer to a blocking streaming task.
    handoff: bool,
    control: Option<StreamControl>,
    /// Whether a flush before the switch switches to streaming, so that the bytes of a
    /// cursor's first batch reach the client before the next batch is computed.
    commit_on_flush: bool,
    timeouts: Option<Timeouts>,
    timed: Option<lifetime::Producer>,
    held: Option<crate::ratelimit::Held>,
    /// where the writer's rows end, credited at each send that enqueued
    ledger: RowLedger,
    /// the bytes copied into `buf` since the start
    copied: u64,
    /// the bytes that have been enqueued into `tx`
    sent: u64,
    /// An error of a send that had already enqueued its chunk, such as the body timer
    /// aborting the transfer just after. The chunk's rows count, and the error is
    /// returned from the next write, flush or send.
    after_send: Option<io::Error>,
    #[cfg(test)]
    producer_gate: Option<Arc<super::algo::probe::Gate>>,
    #[cfg(test)]
    queued_producer_gate: Option<Arc<super::algo::probe::Gate>>,
}

/// Controls for a native cursor's producer, including time spent waiting for a
/// slow reader. Eager serialization keeps its existing blocking-send path.
///
/// The deadline counts the query's own time. A wait for room in the channel is added to
/// the query's deadline pause, and one wait that lasts longer than the stall bound ends
/// the stream as a timeout.
#[derive(Clone)]
pub(super) struct StreamControl {
    cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
    /// The nanoseconds the producer spent waiting for the reader. It is the query's
    /// deadline pause, so a wait does not count against the deadline.
    wait_ns: Arc<AtomicU64>,
    /// The longest the producer may wait for room in one stretch.
    stall: Option<Duration>,
    #[cfg(test)]
    clock_advance_ns: Option<Arc<AtomicU64>>,
}

impl StreamControl {
    pub(super) fn new(
        cancel: Arc<AtomicBool>,
        deadline: Option<Instant>,
        pause: Arc<AtomicU64>,
        stall: Option<Duration>,
    ) -> Self {
        Self {
            cancel,
            deadline,
            wait_ns: pause,
            stall,
            #[cfg(test)]
            clock_advance_ns: None,
        }
    }
    fn check_cancel(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "query cancelled",
            ));
        }
        Ok(())
    }
    fn check(&self) -> io::Result<()> {
        self.check_cancel()?;
        if let Some(d) = self.deadline {
            // the deadline counts the query's own time, not the time it waited for the
            // reader, and a deadline too far out to represent never passes
            let paused = Duration::from_nanos(self.wait_ns.load(Ordering::Relaxed));
            #[cfg(test)]
            let now = self
                .clock_advance_ns
                .as_ref()
                .map(|ns| Duration::from_nanos(ns.load(Ordering::Relaxed)))
                .and_then(|offset| Instant::now().checked_add(offset))
                .unwrap_or_else(Instant::now);
            #[cfg(not(test))]
            let now = Instant::now();
            if now >= d && d.checked_add(paused).is_some_and(|end| now >= end) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "query timed out"));
            }
        }
        Ok(())
    }
    pub(super) fn classify(&self, error: Error) -> Error {
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
    /// Switch to streaming: hand the receiver to the handler, then send what is
    /// buffered. The initial chunk is capped at `threshold + 1` bytes so an in-place
    /// hand-off preserves the ordinary pool switch bound; any remaining full `CHUNK`
    /// slices drain through the bounded channel with backpressure.
    fn switch(&mut self) -> io::Result<()> {
        let (tx, rx) = mpsc::channel(CHANNEL_CAP);
        let signal = self.signal.take().expect("signal before the switch");
        if let Some(timeouts) = self.timeouts {
            let cancel = self
                .control
                .as_ref()
                .map_or_else(|| Arc::new(AtomicBool::new(false)), |c| c.cancel.clone());
            let pause = self.control.as_ref().map(|c| c.wait_ns.clone());
            let (producer, receiver) =
                lifetime::Producer::new(rx, timeouts, cancel, self.held.take(), pause);
            self.timed = Some(producer);
            signal.send(Outcome::Timed(receiver)).map_err(|_| gone())?;
        } else {
            signal.send(Outcome::Stream(rx)).map_err(|_| gone())?;
        }
        self.tx = Some(tx);
        #[cfg(test)]
        if let Some(gate) = self.producer_gate.take() {
            gate.wait();
        }
        let max_first = self.threshold.saturating_add(1);
        let pending = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK));
        if pending.len() <= max_first {
            self.send_bytes(Bytes::from(pending))?;
            #[cfg(test)]
            if let Some(gate) = self.queued_producer_gate.take() {
                gate.wait();
            }
            return Ok(());
        }
        // Copy overflow slices into independently owned `Bytes` so neither `first` nor
        // subsequent chunks retain the oversized `pending` backing allocation.
        let mut offset = max_first;
        self.send_bytes(Bytes::copy_from_slice(&pending[..max_first]))?;
        #[cfg(test)]
        if let Some(gate) = self.queued_producer_gate.take() {
            gate.wait();
        }
        while pending.len() - offset >= CHUNK {
            let piece = Bytes::copy_from_slice(&pending[offset..offset + CHUNK]);
            offset += CHUNK;
            self.send_bytes(piece)?;
        }
        if offset < pending.len() {
            self.buf.extend_from_slice(&pending[offset..]);
        }
        Ok(())
    }

    /// A capped synchronous trial reached its byte threshold and will be retried
    /// on the blocking pool. That trial must not report final serialization timing.
    pub(super) fn deferred(&self) -> bool {
        self.deferred
    }

    /// The ledger in which a result writer records where its rows end.
    pub(crate) fn ledger(&self) -> RowLedger {
        self.ledger.clone()
    }

    /// A writer for draining a cursor in place: buffers into `buf` up to `threshold`,
    /// checks `control` on writes, and never fails with `WouldBlock` so the cursor can
    /// stop at a row or batch boundary and move `self` to a blocking streaming task.
    pub(super) fn new_in_place_cursor(threshold: usize, control: StreamControl) -> Self {
        SwitchWriter {
            buf: Vec::new(),
            threshold,
            signal: None,
            tx: None,
            deferred: false,
            handoff: true,
            control: Some(control),
            commit_on_flush: false,
            timeouts: None,
            timed: None,
            held: None,
            ledger: RowLedger::with_split_prefix(threshold.saturating_add(1) as u64),
            copied: 0,
            sent: 0,
            after_send: None,
            #[cfg(test)]
            producer_gate: None,
            #[cfg(test)]
            queued_producer_gate: None,
        }
    }

    /// A bounded synchronous trial writer for repeatable results (such as ASK native JSON):
    /// checks `control`, stops immediately with `WouldBlock` (`deferred() == true`) before
    /// buffering past `threshold`, so oversized responses fall back to the pool without
    /// serializing the whole document twice.
    pub(super) fn new_in_place_trial(threshold: usize, control: StreamControl) -> Self {
        let mut sw = Self::new_in_place_cursor(threshold, control);
        sw.handoff = false;
        sw
    }

    /// Extract the completed buffered body after an in-place cursor finished whole,
    /// marking every copied row as delivered in its ledger.
    pub(super) fn into_whole_body(self) -> Vec<u8> {
        self.ledger.handed_over(self.copied);
        self.buf
    }

    fn attach_for_handoff(
        &mut self,
        signal: oneshot::Sender<Outcome>,
        timeouts: Timeouts,
        held: crate::ratelimit::Held,
        #[cfg(test)] probe: &super::algo::probe::Probe,
    ) {
        self.handoff = false;
        self.signal = Some(signal);
        self.timeouts = Some(timeouts);
        self.held = Some(held);
        #[cfg(test)]
        {
            if probe.producer.is_some() || probe.queued_producer.is_some() {
                self.threshold = 1;
            }
            self.producer_gate = probe.producer.clone();
            self.queued_producer_gate = probe.queued_producer.clone();
            if let Some(control) = &mut self.control {
                control.clock_advance_ns = probe.clock_advance_ns.clone();
            }
        }
    }

    fn switch_if_over_threshold(&mut self) -> io::Result<()> {
        if self.tx.is_none() && self.buf.len() > self.threshold {
            if let Some(control) = &self.control {
                control.check()?;
            }
            self.switch()?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn new(threshold: usize, signal: Option<oneshot::Sender<Outcome>>) -> Self {
        SwitchWriter {
            buf: Vec::new(),
            threshold,
            signal,
            tx: None,
            deferred: false,
            handoff: false,
            control: None,
            commit_on_flush: false,
            timeouts: None,
            timed: None,
            held: None,
            ledger: RowLedger::new(),
            copied: 0,
            sent: 0,
            after_send: None,
            #[cfg(test)]
            producer_gate: None,
            #[cfg(test)]
            queued_producer_gate: None,
        }
    }

    /// The error of an earlier send that enqueued and then failed.
    #[inline]
    fn failed_send(&mut self) -> io::Result<()> {
        match self.after_send.take() {
            None => Ok(()),
            Some(e) => {
                let copy = io::Error::new(e.kind(), e.to_string());
                self.after_send = Some(e);
                Err(copy)
            }
        }
    }

    /// The chunk enqueued: its bytes and rows count. A check that then fails is kept for
    /// the next call, so that the write that sent the chunk still counts its bytes.
    fn enqueued(&mut self, len: usize, after: io::Result<()>) {
        self.sent += len as u64;
        self.ledger.handed_over(self.sent);
        if let Err(e) = after {
            self.after_send = Some(e);
        }
    }

    fn send_bytes(&mut self, chunk: Bytes) -> io::Result<()> {
        self.failed_send()?;
        let Some(tx) = &self.tx else {
            return Ok(());
        };
        if chunk.is_empty() {
            return Ok(());
        }
        let len = chunk.len();
        if self.control.is_none() && self.timed.is_none() {
            tx.blocking_send(Ok(chunk)).map_err(|_| gone())?;
            self.enqueued(len, Ok(()));
            return Ok(());
        }
        let mut item = Ok(chunk);
        let stalled = Instant::now();
        let after = loop {
            if let Some(control) = &self.control {
                control.check()?;
            }
            if let Some(producer) = &self.timed {
                producer.check()?;
            }
            let sent = match &self.timed {
                Some(producer) => producer.try_send(tx, item)?,
                None => tx.try_send(item),
            };
            match sent {
                Ok(()) => break self.timed.as_ref().map_or(Ok(()), |p| p.check()),
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(gone()),
                Err(mpsc::error::TrySendError::Full(returned)) => {
                    item = returned;
                    let began = Instant::now();
                    let capacity = tokio::runtime::Handle::current().block_on(async {
                        tokio::time::timeout(Duration::from_millis(10), tx.reserve()).await
                    });
                    if let Some(control) = &self.control {
                        let ns = began.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                        let _ =
                            control
                                .wait_ns
                                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                                    Some(n.saturating_add(ns))
                                });
                    }
                    match capacity {
                        Ok(Ok(permit)) => {
                            if let Some(control) = &self.control {
                                control.check()?;
                            }
                            if let Some(producer) = &self.timed {
                                producer.permit_send(permit, item)?;
                                break producer.check();
                            }
                            permit.send(item);
                            break Ok(());
                        }
                        Ok(Err(_)) => return Err(gone()),
                        Err(_) => {
                            // The configured body timer supersedes the old request-
                            // coupled stall bound, including while this thread works.
                            if self.timed.is_none()
                                && self
                                    .control
                                    .as_ref()
                                    .and_then(|c| c.stall)
                                    .is_some_and(|s| stalled.elapsed() >= s)
                            {
                                return Err(io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "the client read nothing for longer than the query timeout",
                                ));
                            }
                        }
                    }
                }
            }
        };
        self.enqueued(len, after);
        Ok(())
    }

    fn send_chunk(&mut self) -> io::Result<()> {
        self.failed_send()?;
        if self.tx.is_none() || self.buf.is_empty() {
            return Ok(());
        }
        // The switch already drained the split prefix down to less than one chunk.
        // Subsequent writes fill the chunk a piece at a time, so the send covers the
        // entire copied buffer.
        debug_assert!(self.buf.len() <= CHUNK);
        let chunk = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        self.send_bytes(chunk)
    }
}

impl Write for SwitchWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.failed_send()?;
        if let Some(control) = &self.control {
            // Serializers write a few bytes at a time, and reading the clock on each
            // write took a third of a streamed response's time. The deadline is
            // checked when the buffer crosses a 16 KiB mark and before each chunk is
            // sent. The cancellation flag is checked on every write.
            let end = self.buf.len().saturating_add(b.len());
            if end >> CONTROL_SHIFT != self.buf.len() >> CONTROL_SHIFT {
                control.check()?;
            } else {
                control.check_cancel()?;
            }
        }
        if let Some(producer) = &self.timed {
            producer.check()?;
        }
        if self.signal.is_none() && self.tx.is_none() {
            // A quick result is tried synchronously, but its row count says nothing
            // about literal size. Stop before allocating beyond the byte threshold;
            // the caller retries this immutable result through the bounded stream.
            // An in-place cursor trial keeps writing until its row/batch boundary and
            // then moves `self` onto the blocking pool.
            if !self.handoff && b.len() > self.threshold.saturating_sub(self.buf.len()) {
                self.deferred = true;
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "response requires streaming",
                ));
            }
            self.buf.extend_from_slice(b);
            self.copied += b.len() as u64;
            self.ledger.copied(self.copied);
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
        self.copied += n as u64;
        self.ledger.copied(self.copied);
        if self.tx.is_none() && self.buf.len() > self.threshold {
            self.switch()?;
        } else if self.tx.is_some() && self.buf.len() >= CHUNK {
            self.send_chunk()?;
        }
        Ok(n)
    }

    /// A flush commits an explicitly streamed response that has not switched yet. Every
    /// other flush leaves the buffer to fill its chunk.
    fn flush(&mut self) -> io::Result<()> {
        self.failed_send()?;
        if self.commit_on_flush
            && self.tx.is_none()
            && self.signal.is_some()
            && !self.buf.is_empty()
        {
            if let Some(control) = &self.control {
                control.check()?;
            }
            self.switch()?;
        }
        Ok(())
    }
}

/// Serialize a body with `write` on a blocking thread, under the result-size `limit`.
/// `on_stream_end` runs on that thread when a *streamed* body ends (whole bodies are
/// reported by the caller). An error before the switch is returned as the response
/// error.
pub(super) async fn serialize_timed<F, D>(
    limit: Option<u64>,
    timeouts: Timeouts,
    write: F,
    on_end: D,
) -> ApiResult<Serialized>
where
    F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    serialize_inner_timed(
        STREAM_AFTER,
        false,
        limit,
        None,
        Some(timeouts),
        write,
        on_end,
    )
    .await
}

#[cfg(test)]
pub(super) async fn serialize_with<F, D>(
    threshold: usize,
    limit: Option<u64>,
    write: F,
    on_stream_end: D,
) -> ApiResult<Serialized>
where
    F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    serialize_inner(threshold, false, limit, None, write, on_stream_end).await
}

/// Serialize a cursor's body under `control`. With `early`, for a request that asked
/// for streaming, the response commits after [`FIRST_BYTES_AFTER`] bytes or at the
/// writer's first flush, which the cursor writers send after their first batch.
/// Otherwise it commits after [`STREAM_AFTER`] bytes, as an eager body does. A failure
/// after the commit aborts the transfer.
pub(super) async fn serialize_controlled<F, D>(
    limit: Option<u64>,
    control: StreamControl,
    early: bool,
    timeouts: Timeouts,
    write: F,
    on_end: D,
) -> ApiResult<Serialized>
where
    F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    let threshold = if early {
        FIRST_BYTES_AFTER
    } else {
        STREAM_AFTER
    };
    serialize_inner_timed(
        threshold,
        early,
        limit,
        Some(control),
        Some(timeouts),
        write,
        on_end,
    )
    .await
}

#[cfg(test)]
async fn serialize_inner<F, D>(
    threshold: usize,
    commit_on_flush: bool,
    limit: Option<u64>,
    control: Option<StreamControl>,
    write: F,
    on_end: D,
) -> ApiResult<Serialized>
where
    F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    serialize_inner_timed(
        threshold,
        commit_on_flush,
        limit,
        control,
        None,
        write,
        on_end,
    )
    .await
}

async fn serialize_inner_timed<F, D>(
    threshold: usize,
    commit_on_flush: bool,
    limit: Option<u64>,
    control: Option<StreamControl>,
    timeouts: Option<Timeouts>,
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
    #[cfg(test)]
    let probe = super::algo::probe::current();
    #[cfg(test)]
    let threshold = if probe.producer.is_some() || probe.queued_producer.is_some() {
        1
    } else {
        threshold
    };
    let t0 = Instant::now();
    tokio::task::spawn_blocking(move || {
        crate::cpus::place_request();
        #[cfg(test)]
        let _completion = probe.producer.clone().map(super::algo::probe::Completion);
        #[cfg(test)]
        let _queued_completion = probe
            .queued_producer
            .clone()
            .map(super::algo::probe::Completion);
        #[cfg(test)]
        let _completion_only = probe.completion.clone().map(super::algo::probe::Completion);
        #[cfg(test)]
        let _after_write_completion = probe
            .after_write
            .clone()
            .map(super::algo::probe::Completion);
        // A local binding drops the permit before the test completion observer,
        // including unwinding. Ordinary builds keep the original capture/drop path.
        #[cfg(test)]
        #[allow(clippy::redundant_locals)]
        let held = held;
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
            handoff: false,
            control: control.clone(),
            commit_on_flush,
            timeouts,
            timed: None,
            held: Some(held),
            ledger: RowLedger::new(),
            copied: 0,
            sent: 0,
            after_send: None,
            #[cfg(test)]
            producer_gate: probe.producer,
            #[cfg(test)]
            queued_producer_gate: probe.queued_producer,
        };
        // disconnects are noticed through the channel, not a cancellation flag
        let mut w = LimitedWriter::new(sw, limit, None::<Arc<AtomicBool>>);
        let result = if control.is_some() || timeouts.is_some() {
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
        #[cfg(test)]
        if let Some(gate) = &probe.after_write {
            gate.wait();
        }
        finish_producer(
            w,
            result,
            control.as_ref(),
            t0,
            &terminal,
            span,
            on_stream_end,
        );
    });
    receive_outcome(outcome, body_terminal, t0).await
}

fn finish_producer<D>(
    w: LimitedWriter<SwitchWriter>,
    result: sparkles::Result<()>,
    control: Option<&StreamControl>,
    t0: Instant,
    terminal: &Arc<std::sync::Mutex<Option<String>>>,
    span: tracing::span::EnteredSpan,
    on_stream_end: D,
) where
    D: FnOnce(StreamEnd),
{
    let result = result
        .map_err(|e| w.classify(e))
        .map_err(|e| match control {
            Some(c) => c.classify(e),
            None => e,
        });
    let w_written = w.written();
    let mut sw = w.into_inner();
    if let (Some(producer), Err(error)) = (&sw.timed, &result) {
        producer.record_error(error);
    }
    let result = match result {
        Ok(()) => sw.send_chunk().map_err(Error::from),
        Err(e) => {
            // A body stopped by the result-size budget sends what it buffered before
            // it aborts, so that the client receives every row the report counts,
            // which are the rows the size limit let through. Its writes already
            // waited for the reader, so this wait is one more chunk of the same kind,
            // under a cursor's deadline and stall bound. Any other failure ends the
            // body at once. The bytes reported leave out what was not sent.
            if matches!(e, Error::BudgetExceeded(_)) && sw.tx.is_some() {
                let _ = sw.send_chunk();
            }
            Err(e)
        }
    };
    let bytes = if sw.tx.is_some() { sw.sent } else { w_written };
    let result = result.map_err(|e| match control {
        Some(c) => c.classify(e),
        None => e,
    });
    let serialize_ms = t0.elapsed().as_secs_f64() * 1000.0;
    if let Some(mut producer) = sw.timed.take() {
        // the producer is installed before the handoff, which can fail
        let committed = sw.tx.is_some();
        drop(sw.tx.take());
        let result = producer.finish(result);
        let serialize_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let disconnected = producer.disconnected();
        producer.release_permit();
        on_stream_end(StreamEnd {
            bytes,
            serialize_ms,
            error: result.err(),
            disconnected,
            // a handoff that failed committed nothing, so no row reached a client
            rows: committed.then(|| sw.ledger.delivered()).flatten(),
            committed,
        });
        drop(span);
        producer.publish();
        return;
    }
    match (sw.tx.take(), result) {
        (None, Ok(())) => {
            let body = std::mem::take(&mut sw.buf);
            if control.is_some() {
                // a whole body delivers every row it holds
                sw.ledger.handed_over(sw.copied);
                on_stream_end(StreamEnd {
                    bytes,
                    serialize_ms,
                    error: None,
                    disconnected: false,
                    rows: sw.ledger.delivered(),
                    committed: true,
                });
            }
            drop(span);
            // Completion may wake the handler immediately. Release the
            // producer's share before publishing the buffered result.
            drop(sw.held.take());
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
                    // the response is an error status, and none of its rows was sent
                    rows: None,
                    committed: false,
                });
                drop(span);
                drop(sw.held.take());
                if let Some(s) = sw.signal.take() {
                    let _ = s.send(Outcome::Failed(e));
                }
                return;
            }
            drop(span);
            drop(sw.held.take());
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
                rows: sw.ledger.delivered(),
                committed: true,
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
            drop(sw.held.take());
            drop(tx);
        }
    }
}

async fn receive_outcome(
    outcome: oneshot::Receiver<Outcome>,
    body_terminal: Arc<std::sync::Mutex<Option<String>>>,
    t0: Instant,
) -> ApiResult<Serialized> {
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
        Ok(Outcome::Timed(receiver)) => {
            let abort = receiver.abort_handle();
            Ok(Serialized::Timed {
                body: axum::body::Body::from_stream(receiver),
                abort,
            })
        }
        Ok(Outcome::Failed(e)) => Err(ApiError::from(e)),
        Ok(Outcome::EarlyReturn) => unreachable!("early return handled by open_and_serialize"),
        Err(_) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the serializer stopped unexpectedly",
        )),
    }
}

/// What the open closure of [`open_and_serialize_controlled`] produced inside the single
/// blocking producer task.
pub(super) enum Opened<E, M, S> {
    /// Automatic execution chose an eager result: return it to the handler without
    /// invoking `on_end`.
    Eager(E),
    /// A cursor opened and will be serialized in this same blocking task.
    Cursor {
        meta: M,
        control: StreamControl,
        state: S,
    },
}

/// Result returned to the async handler from [`open_and_serialize_controlled`].
pub(super) enum OpenedSerialized<E, M> {
    Eager(E),
    Cursor {
        meta: M,
        serialized: ApiResult<Serialized>,
    },
}

enum OpenSlot<E, M> {
    Eager(E),
    OpenErr(ApiError),
    Cursor(M),
}

/// Open a query execution and, if it yields a cursor, serialize its body in the same
/// blocking-pool task (item 534). Removes the return wake and second blocking-pool wake
/// between opening a cursor and writing its body.
pub(super) async fn open_and_serialize_controlled<E, M, S, O, W, D>(
    limit: Option<u64>,
    early: bool,
    timeouts: Timeouts,
    open: O,
    write: W,
    on_end: D,
) -> ApiResult<OpenedSerialized<E, M>>
where
    E: Send + 'static,
    M: Send + 'static,
    S: Send + 'static,
    O: FnOnce() -> ApiResult<Opened<E, M, S>> + Send + 'static,
    W: FnOnce(S, &mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    let (signal, outcome) = oneshot::channel();
    let terminal: Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let body_terminal = terminal.clone();
    let open_slot: Arc<parking_lot::Mutex<Option<OpenSlot<E, M>>>> = Default::default();
    let producer_slot = open_slot.clone();
    let span = tracing::Span::current();
    let held = crate::ratelimit::hold();
    #[cfg(test)]
    let probe = super::algo::probe::current();
    let base_threshold = if early {
        FIRST_BYTES_AFTER
    } else {
        STREAM_AFTER
    };
    #[cfg(test)]
    let threshold = if probe.producer.is_some() || probe.queued_producer.is_some() {
        1
    } else {
        base_threshold
    };
    #[cfg(not(test))]
    let threshold = base_threshold;
    let t0 = Instant::now();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _blocking_completion = probe.blocking.clone().map(super::algo::probe::Completion);
        // Permit drops before the blocking test completion observer, including unwinding.
        #[cfg(test)]
        #[allow(clippy::redundant_locals)]
        let held = held;
        #[cfg(test)]
        if let Some(gate) = probe.blocking {
            gate.wait();
        }
        crate::cpus::place_request();
        let span = span.entered();
        let opened = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(open)) {
            Ok(res) => res,
            Err(p) => {
                let msg = p
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("the request panicked: {msg}"),
                ))
            }
        };
        let (control, state) = match opened {
            Ok(Opened::Eager(eager)) => {
                *producer_slot.lock() = Some(OpenSlot::Eager(eager));
                drop(span);
                drop(held);
                let _ = signal.send(Outcome::EarlyReturn);
                return;
            }
            Err(open_err) => {
                *producer_slot.lock() = Some(OpenSlot::OpenErr(open_err));
                drop(span);
                drop(held);
                let _ = signal.send(Outcome::EarlyReturn);
                return;
            }
            Ok(Opened::Cursor {
                meta,
                control,
                state,
            }) => {
                *producer_slot.lock() = Some(OpenSlot::Cursor(meta));
                (control, state)
            }
        };
        // Producer completion observers are installed only once this task actually becomes
        // the cursor producer; an eager return hands serialization to `serialize_timed`.
        #[cfg(test)]
        let _completion = probe.producer.clone().map(super::algo::probe::Completion);
        #[cfg(test)]
        let _queued_completion = probe
            .queued_producer
            .clone()
            .map(super::algo::probe::Completion);
        #[cfg(test)]
        let _completion_only = probe.completion.clone().map(super::algo::probe::Completion);
        #[cfg(test)]
        let _after_write_completion = probe
            .after_write
            .clone()
            .map(super::algo::probe::Completion);
        let sw = SwitchWriter {
            buf: Vec::new(),
            threshold,
            signal: Some(signal),
            tx: None,
            deferred: false,
            handoff: false,
            control: Some(control.clone()),
            commit_on_flush: early,
            timeouts: Some(timeouts),
            timed: None,
            held: Some(held),
            ledger: RowLedger::new(),
            copied: 0,
            sent: 0,
            after_send: None,
            #[cfg(test)]
            producer_gate: probe.producer,
            #[cfg(test)]
            queued_producer_gate: probe.queued_producer,
        };
        let mut w = LimitedWriter::new(sw, limit, None::<Arc<AtomicBool>>);
        let write_t0 = Instant::now();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write(state, &mut w)))
                .unwrap_or_else(|_| {
                    Err(Error::Io(io::Error::other(
                        "query producer stopped unexpectedly",
                    )))
                });
        #[cfg(test)]
        if let Some(gate) = &probe.after_write {
            gate.wait();
        }
        finish_producer(w, result, Some(&control), write_t0, &terminal, span, on_end);
    });
    match outcome.await {
        Ok(Outcome::EarlyReturn) => match open_slot.lock().take().expect("early return payload") {
            OpenSlot::Eager(eager) => Ok(OpenedSerialized::Eager(eager)),
            OpenSlot::OpenErr(e) => Err(e),
            OpenSlot::Cursor(_) => unreachable!("cursor metadata with early return"),
        },
        other => {
            let Some(OpenSlot::Cursor(meta)) = open_slot.lock().take() else {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "the serializer stopped unexpectedly",
                ));
            };
            let serialized = match other {
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
                Ok(Outcome::Timed(receiver)) => {
                    let abort = receiver.abort_handle();
                    Ok(Serialized::Timed {
                        body: axum::body::Body::from_stream(receiver),
                        abort,
                    })
                }
                Ok(Outcome::Failed(e)) => Err(ApiError::from(e)),
                Ok(Outcome::EarlyReturn) => unreachable!(),
                Err(_) => Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "the serializer stopped unexpectedly",
                )),
            };
            Ok(OpenedSerialized::Cursor { meta, serialized })
        }
    }
}

/// Continue streaming a cursor body that crossed its byte threshold during in-place
/// execution (item 534). Moves the already-buffered [`SwitchWriter`] onto a blocking task,
/// switches to streaming immediately when buffered bytes exceed the threshold, and drains
/// the remainder through `write`.
pub(super) async fn finish_handoff_controlled<F, D>(
    mut writer: LimitedWriter<SwitchWriter>,
    control: StreamControl,
    timeouts: Timeouts,
    began: Instant,
    write: F,
    on_end: D,
) -> ApiResult<Serialized>
where
    F: FnOnce(
            sparkles::Result<()>,
            &mut Option<LimitedWriter<SwitchWriter>>,
        ) -> sparkles::Result<()>
        + Send
        + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    let (signal, outcome) = oneshot::channel();
    let terminal: Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let body_terminal = terminal.clone();
    let span = tracing::Span::current();
    let held = crate::ratelimit::hold();
    #[cfg(test)]
    let probe = super::algo::probe::current();
    tokio::task::spawn_blocking(move || {
        crate::cpus::place_request();
        #[cfg(test)]
        let _completion = probe.producer.clone().map(super::algo::probe::Completion);
        #[cfg(test)]
        let _queued_completion = probe
            .queued_producer
            .clone()
            .map(super::algo::probe::Completion);
        #[cfg(test)]
        let _completion_only = probe.completion.clone().map(super::algo::probe::Completion);
        #[cfg(test)]
        let _after_write_completion = probe
            .after_write
            .clone()
            .map(super::algo::probe::Completion);
        let span = span.entered();
        writer.get_mut().attach_for_handoff(
            signal,
            timeouts,
            held,
            #[cfg(test)]
            &probe,
        );
        let mut writer_slot = Some(writer);
        let switched = writer_slot
            .as_mut()
            .expect("handoff writer installed")
            .get_mut()
            .switch_if_over_threshold()
            .map_err(|e| {
                let w = writer_slot.as_ref().expect("handoff writer installed");
                control.classify(w.classify(Error::from(e)))
            });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write(switched, &mut writer_slot)
        }))
        .unwrap_or_else(|_| {
            Err(Error::Io(io::Error::other(
                "query producer stopped unexpectedly",
            )))
        });
        #[cfg(test)]
        if let Some(gate) = &probe.after_write {
            gate.wait();
        }
        let w = writer_slot.expect("handoff writer preserved across unwind");
        finish_producer(w, result, Some(&control), began, &terminal, span, on_end);
    });
    receive_outcome(outcome, body_terminal, began).await
}

/// Try a quick result on the current thread, buffering at most [`STREAM_AFTER`]
/// bytes. A larger result retries its repeatable serializer on the blocking pool,
/// where it can apply channel backpressure. Real errors retain their usual behavior.
#[cfg(test)]
pub(super) async fn serialize_quick<F, D>(
    limit: Option<u64>,
    write: F,
    on_stream_end: D,
) -> ApiResult<Serialized>
where
    F: Fn(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    serialize_quick_inner(limit, None, write, on_stream_end).await
}

pub(super) async fn serialize_quick_timed<F, D>(
    limit: Option<u64>,
    timeouts: Timeouts,
    write: F,
    on_end: D,
) -> ApiResult<Serialized>
where
    F: Fn(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    D: FnOnce(StreamEnd) + Send + 'static,
{
    serialize_quick_inner(limit, Some(timeouts), write, on_end).await
}

async fn serialize_quick_inner<F, D>(
    limit: Option<u64>,
    timeouts: Option<Timeouts>,
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
        let body = serialize_inner_timed(
            STREAM_AFTER,
            false,
            limit,
            None,
            timeouts,
            write,
            move |mut end| {
                end.serialize_ms += trial_ms;
                on_stream_end(end);
            },
        )
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
        handoff: false,
        control: None,
        commit_on_flush: false,
        timeouts: None,
        timed: None,
        held: None,
        ledger: RowLedger::new(),
        copied: 0,
        sent: 0,
        after_send: None,
        #[cfg(test)]
        producer_gate: None,
        #[cfg(test)]
        queued_producer_gate: None,
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

    /// Both query routes report a body's rows by one rule: none for a body that failed
    /// before it was committed, whatever its writer counted, then the delivered rows, the
    /// rows written whole and the complete count, in that order (items 828 and 243).
    #[test]
    fn the_reported_rows_follow_the_commit_and_the_error() {
        let end = |committed, error: bool, rows| StreamEnd {
            bytes: 0,
            serialize_ms: 0.0,
            error: error.then_some(Error::Cancelled),
            disconnected: false,
            rows,
            committed,
        };
        // an error response sent no rows, also when the buffer had whole rows in it
        assert_eq!(
            end(false, true, Some(3)).reported_rows(Some(5), Some(9)),
            None
        );
        assert_eq!(end(false, true, None).reported_rows(Some(5), Some(9)), None);
        assert_eq!(
            end(true, true, Some(3)).reported_rows(Some(5), Some(9)),
            Some(3)
        );
        assert_eq!(
            end(true, true, None).reported_rows(Some(5), Some(9)),
            Some(5)
        );
        assert_eq!(end(true, true, None).reported_rows(None, Some(9)), Some(9));
        assert_eq!(
            end(true, false, Some(3)).reported_rows(Some(5), Some(9)),
            Some(9)
        );
    }

    /// A send that enqueued and then failed its check counts the rows that end in its
    /// chunk, and its error comes back from the next write, flush or send, also when the
    /// serializer writes nothing more (item 243).
    #[test]
    fn a_send_that_enqueued_and_then_failed_counts_its_rows() {
        let store = sparkles::store::Store::in_memory(Default::default());
        let r = sparkles::sparql::query(
            store.snapshot(),
            "SELECT ?x WHERE { VALUES ?x { 1 2 3 } }",
            &Default::default(),
        )
        .unwrap();
        let mut sw = SwitchWriter::new(1 << 20, None);
        let rows = sparkles::sparql::results::RowCount::with_ledger(sw.ledger());
        use sparkles::sparql::results as res;
        res::write(
            res::Source::Result(&r),
            res::WriteOptions::new(res::Format::Solutions(res::SolutionsFormat::Tsv), &rows),
            &mut sw,
        )
        .unwrap();
        assert_eq!(rows.get(), 3);
        assert_eq!(sw.ledger().delivered(), Some(0), "nothing was sent");
        let len = sw.buf.len();
        sw.enqueued(len, Err(io::Error::new(io::ErrorKind::TimedOut, "aborted")));
        assert_eq!(sw.ledger().delivered(), Some(3));
        for _ in 0..2 {
            assert_eq!(sw.send_chunk().unwrap_err().kind(), io::ErrorKind::TimedOut);
            assert_eq!(sw.write(b"x").unwrap_err().kind(), io::ErrorKind::TimedOut);
            assert_eq!(sw.flush().unwrap_err().kind(), io::ErrorKind::TimedOut);
        }
    }

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
        let size = CHUNK * (CHANNEL_CAP + 4) + 17;
        let producer = tokio::task::spawn_blocking(move || {
            let mut w = SwitchWriter::new(100, Some(signal));
            w.write_all(&vec![b'x'; size])?;
            w.send_chunk()
        });
        let Outcome::Stream(rx) = outcome.await.unwrap() else {
            panic!("expected a stream");
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while rx.len() < CHANNEL_CAP && !producer.is_finished() {
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
        assert_eq!(rx.len(), CHANNEL_CAP);
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
        assert_eq!(rx.len(), CHANNEL_CAP);
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

    /// A cursor body cut by the result-size budget sends what it had buffered before it
    /// aborts. A reader that has stopped reading holds that send only to the stall
    /// bound, and the body still ends as a budget failure that reports only the bytes
    /// that reached the channel.
    #[tokio::test]
    async fn a_budget_cut_with_a_stalled_reader_stays_a_budget_failure() {
        let (seen, on_end) = ends();
        let control = StreamControl::new(
            Default::default(),
            None,
            Default::default(),
            Some(Duration::from_millis(150)),
        );
        // The switch sends the first 101 bytes and `CHANNEL_CAP - 1` full chunks fill the
        // channel, so the budget cuts the next chunk halfway while the channel is full.
        let sent = 101 + (CHUNK * (CHANNEL_CAP - 1)) as u64;
        let limit = sent + (CHUNK / 2) as u64;
        let started = Instant::now();
        let Ok(Serialized::Streamed(body)) = serialize_inner(
            100,
            false,
            Some(limit),
            Some(control),
            write_n(10_000_000, None),
            on_end,
        )
        .await
        else {
            panic!("expected a stream");
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while seen.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("a stalled reader must not hold the budget flush past the stall bound");
        assert!(started.elapsed() >= Duration::from_millis(150));
        {
            let seen = seen.lock().unwrap();
            assert!(
                matches!(seen[0].error, Some(Error::BudgetExceeded(_))),
                "the failed flush must not replace the budget error: {:?}",
                seen[0].error
            );
            assert_eq!(seen[0].bytes, sent, "the unsent chunk must not be reported");
        }
        let received = axum::body::to_bytes(body, usize::MAX).await;
        assert!(received.is_err(), "a cut body must not end successfully");
    }

    #[tokio::test]
    async fn a_reader_that_stalls_past_the_stall_bound_times_out() {
        let (seen, on_end) = ends();
        let control = StreamControl::new(
            Default::default(),
            None,
            Default::default(),
            Some(Duration::from_millis(150)),
        );
        let waits = control.clone();
        let Ok(Serialized::Streamed(body)) = serialize_inner(
            100,
            false,
            None,
            Some(control),
            write_n(10_000_000, None),
            on_end,
        )
        .await
        else {
            panic!("expected a stream");
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while seen.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("a stalled reader must not hold the producer past the stall bound");
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
    async fn controlled_waits_do_not_count_against_the_deadline() {
        use futures_util::StreamExt;
        let (seen, on_end) = ends();
        let control = StreamControl::new(
            Default::default(),
            Some(Instant::now() + Duration::from_millis(150)),
            Default::default(),
            Some(Duration::from_secs(1)),
        );
        let Ok(Serialized::Streamed(body)) = serialize_inner(
            100,
            false,
            None,
            Some(control),
            write_n(400_000, None),
            on_end,
        )
        .await
        else {
            panic!("expected a stream");
        };
        // a reader that takes a chunk every 10 ms needs far longer than the deadline
        let began = Instant::now();
        let mut stream = body.into_data_stream();
        let mut bytes = 0;
        while let Some(chunk) = stream.next().await {
            bytes += chunk
                .expect("a slow steady reader gets the whole body")
                .len();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(bytes, 4_000_000);
        assert!(began.elapsed() > Duration::from_millis(300));
        tokio::time::timeout(Duration::from_secs(5), async {
            while seen.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the stream ends");
        assert!(seen.lock().unwrap()[0].error.is_none());
    }

    #[tokio::test]
    async fn controlled_waits_observe_cancellation_and_report_only_once() {
        let (seen, on_end) = ends();
        let cancel = Arc::new(AtomicBool::new(false));
        let Ok(Serialized::Streamed(body)) = serialize_inner(
            100,
            false,
            None,
            Some(StreamControl::new(
                cancel.clone(),
                None,
                Default::default(),
                None,
            )),
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

    /// A cursor graph written in Turtle or N-Triples keeps the kind of the I/O error that
    /// stops it, so a deadline that passes mid-stream is reported as a timeout and a
    /// cancellation as a cancellation, as in the other graph writers. The failure comes
    /// after the first batch's flush, at the first write after it, which the serializer
    /// makes while it writes a later statement.
    #[tokio::test]
    async fn a_cursor_graph_stream_is_classified_by_the_error_that_stops_it() {
        use sparkles::sparql::results as res;
        /// Trips the failure at its first write after a flush.
        struct Trip<'w> {
            inner: &'w mut LimitedWriter<SwitchWriter>,
            flushed: bool,
            trip: Option<Box<dyn FnOnce() + 'w>>,
        }
        impl Write for Trip<'_> {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                if self.flushed
                    && let Some(trip) = self.trip.take()
                {
                    trip();
                }
                self.inner.write(b)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushed = true;
                self.inner.flush()
            }
        }
        let store = sparkles::store::Store::in_memory(Default::default());
        let data = (0..5000)
            .map(|i| format!("<urn:s:{i}> <urn:p> \"value {i}\" .\n"))
            .collect::<String>();
        store
            .load(&[sparkles::io::Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        for format in [oxrdfio::RdfFormat::Turtle, oxrdfio::RdfFormat::NTriples] {
            for timeout in [true, false] {
                let (seen, on_end) = ends();
                let cancel = Arc::new(AtomicBool::new(false));
                // the deadline has passed, and a pause too long to represent holds it off
                // until the trip clears the pause
                let pause = Arc::new(AtomicU64::new(u64::MAX));
                let control = StreamControl::new(
                    cancel.clone(),
                    timeout.then(Instant::now),
                    pause.clone(),
                    None,
                );
                let mut cursor = sparkles::sparql::graph_cursor(
                    store.snapshot(),
                    "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }",
                    &Default::default(),
                    &sparkles::sparql::CursorOptions {
                        batch_rows: 100,
                        ..Default::default()
                    },
                )
                .unwrap();
                let taken = Arc::new(AtomicU64::new(0));
                let tripped = Arc::new(AtomicBool::new(false));
                let (taken_in, tripped_in) = (taken.clone(), tripped.clone());
                let write = move |w: &mut LimitedWriter<SwitchWriter>| {
                    // the response switches to streaming before the graph is written
                    w.write_all(&[b'#'; 200])?;
                    w.write_all(b"\n")?;
                    let rows = res::RowCount::with_ledger(w.get_ref().ledger());
                    let mut trip = Trip {
                        inner: w,
                        flushed: false,
                        trip: Some(Box::new(move || {
                            tripped_in.store(true, Ordering::Relaxed);
                            if timeout {
                                pause.store(0, Ordering::Relaxed);
                            } else {
                                cancel.store(true, Ordering::Relaxed);
                            }
                        })),
                    };
                    let written = res::write(
                        res::Source::Graph(&mut cursor),
                        res::WriteOptions::new(res::Format::Rdf(format), &rows),
                        &mut trip,
                    );
                    taken_in.store(rows.get(), Ordering::Relaxed);
                    written.map(|_| ())
                };
                let Ok(Serialized::Streamed(body)) =
                    serialize_inner(100, false, None, Some(control), write, on_end).await
                else {
                    panic!("expected a stream");
                };
                assert!(axum::body::to_bytes(body, usize::MAX).await.is_err());
                let seen = seen.lock().unwrap();
                assert_eq!(seen.len(), 1);
                let error = &seen[0].error;
                if timeout {
                    assert!(
                        matches!(error, Some(Error::Timeout)),
                        "{format:?}: {error:?}"
                    );
                } else {
                    assert!(
                        matches!(error, Some(Error::Cancelled)),
                        "{format:?}: {error:?}"
                    );
                }
                // the first batch was written and flushed, and the failure came later
                assert!(tripped.load(Ordering::Relaxed), "{format:?} {timeout}");
                let taken = taken.load(Ordering::Relaxed);
                assert!(
                    (100..5000).contains(&taken),
                    "{format:?} {timeout}: {taken}"
                );
            }
        }
    }

    #[tokio::test]
    async fn controlled_early_errors_preserve_budget_status_and_report() {
        let (seen, on_end) = ends();
        let r = serialize_inner(
            100,
            false,
            Some(50),
            Some(StreamControl::new(
                Default::default(),
                None,
                Default::default(),
                None,
            )),
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

#[cfg(test)]
mod timed_tests {
    use super::*;
    use futures_util::StreamExt;

    fn timeouts() -> Timeouts {
        Timeouts {
            idle: Duration::from_millis(30),
            transfer: None,
        }
    }
    async fn timed<F>(
        timeouts: Timeouts,
        limit: Option<u64>,
        write: F,
    ) -> (axum::body::Body, Abort, oneshot::Receiver<StreamEnd>)
    where
        F: FnOnce(&mut LimitedWriter<SwitchWriter>) -> sparkles::Result<()> + Send + 'static,
    {
        let (send, end) = oneshot::channel();
        let result =
            serialize_inner_timed(1, false, limit, None, Some(timeouts), write, move |e| {
                let _ = send.send(e);
            })
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "timed fixture failed before streaming: {} {}",
                    error.0, error.1
                )
            });
        match result {
            Serialized::Timed { body, abort } => (body, abort, end),
            _ => panic!("fixture must actually switch to streaming"),
        }
    }
    async fn ended(end: oneshot::Receiver<StreamEnd>) -> StreamEnd {
        tokio::time::timeout(Duration::from_secs(5), end)
            .await
            .expect("producer and its owned timer must stop")
            .unwrap()
    }
    // Dropping a test future must unblock its real producer before runtime shutdown.
    struct Release(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normal_eof_follows_timer_join_and_real_producer_report() {
        let (body, _, end) = timed(timeouts(), None, |w| {
            w.write_all(b"hello")?;
            Ok(())
        })
        .await;
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let report = ended(end).await;
        assert_eq!(bytes, b"hello"[..]);
        assert!(report.error.is_none());
        assert!(!report.disconnected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_unread_tail_expires_and_late_binding_observes_abort() {
        let (body, abort, end) = timed(timeouts(), None, |w| {
            w.write_all(b"tail")?;
            Ok(())
        })
        .await;
        let report = ended(end).await;
        let called = Arc::new(AtomicBool::new(false));
        let observed = called.clone();
        abort.bind_observation(move || {
            observed.store(true, Ordering::Relaxed);
        });
        let failed = axum::body::to_bytes(body, usize::MAX).await.is_err();
        assert!(matches!(report.error, Some(Error::Timeout)));
        assert!(!report.disconnected);
        assert!(failed);
        assert!(called.load(Ordering::Relaxed));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn independent_timer_expires_while_real_producer_is_busy() {
        let (release, wait) = std::sync::mpsc::channel();
        let release = Release(Some(release));
        let (body, abort, mut end) = timed(timeouts(), None, move |w| {
            w.write_all(b"queued")?;
            wait.recv().unwrap();
            w.write_all(b"after work")?;
            Ok(())
        })
        .await;
        let (signal, aborted) = oneshot::channel();
        abort.bind_admission(move || {
            let _ = signal.send(());
        });
        let independent = tokio::time::timeout(Duration::from_secs(2), aborted)
            .await
            .is_ok();
        let producer_still_owned =
            matches!(end.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        drop(release);
        let report = ended(end).await;
        let failed = axum::body::to_bytes(body, usize::MAX).await.is_err();
        assert!(independent);
        assert!(
            producer_still_owned,
            "logical abort must not manufacture producer completion"
        );
        assert!(matches!(report.error, Some(Error::Timeout)));
        assert!(failed);
    }

    async fn empty_while_busy(transfer: Option<Duration>) -> (bool, StreamEnd, bool) {
        let (release, wait) = std::sync::mpsc::channel();
        let release = Release(Some(release));
        let (body, abort, end) = timed(
            Timeouts {
                transfer,
                ..timeouts()
            },
            None,
            move |w| {
                w.write_all(b"ok")?;
                wait.recv().unwrap();
                w.write_all(b"done")?;
                Ok(())
            },
        )
        .await;
        let aborted = Arc::new(AtomicBool::new(false));
        let marker = aborted.clone();
        abort.bind_observation(move || marker.store(true, Ordering::Relaxed));
        let mut body = body.into_data_stream();
        let first = body.next().await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let expired = aborted.load(Ordering::Relaxed);
        drop(release);
        let mut failed = false;
        let mut bytes = first.to_vec();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(chunk) => bytes.extend_from_slice(&chunk),
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        let end = ended(end).await;
        if !failed {
            assert_eq!(bytes, b"okdone");
        }
        (expired, end, failed)
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn empty_computing_queue_does_not_use_idle_as_compute_timeout() {
        let (expired, report, failed) = empty_while_busy(None).await;
        assert!(!expired && !failed);
        assert!(report.error.is_none());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn absolute_transfer_limit_expires_with_empty_computing_queue() {
        let (expired, report, failed) = empty_while_busy(Some(Duration::from_millis(50))).await;
        assert!(expired && failed);
        assert!(matches!(report.error, Some(Error::Timeout)));
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn budget_error_precedes_idle_and_is_never_rewritten() {
        let (body, _, end) = timed(timeouts(), Some(20), |w| {
            w.write_all(b"prefix")?;
            w.write_all(&[b'x'; 100])?;
            Ok(())
        })
        .await;
        let report = ended(end).await;
        let failed = axum::body::to_bytes(body, usize::MAX).await.is_err();
        assert!(matches!(report.error, Some(Error::BudgetExceeded(_))));
        assert!(failed);
        assert!(!report.disconnected);
    }
    /// The budget flush of a cursor body under the body timers, as the server runs every
    /// cursor request. A reader that has stopped reading holds the flush only until the
    /// idle timer, and the body ends as a budget failure that reports only the bytes that
    /// reached the channel.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timed_budget_cut_with_a_stalled_reader_stays_a_budget_failure() {
        let (send, end) = oneshot::channel();
        let control = StreamControl::new(Default::default(), None, Default::default(), None);
        // as in the untimed test, the budget cuts a chunk while the channel is full
        let sent = 101 + (CHUNK * (CHANNEL_CAP - 1)) as u64;
        let limit = sent + (CHUNK / 2) as u64;
        let result = serialize_inner_timed(
            100,
            false,
            Some(limit),
            Some(control),
            Some(timeouts()),
            |w: &mut LimitedWriter<SwitchWriter>| -> sparkles::Result<()> {
                for _ in 0..10_000_000 {
                    w.write_all(b"0123456789")?;
                }
                Ok(())
            },
            move |e| {
                let _ = send.send(e);
            },
        )
        .await;
        let Ok(Serialized::Timed {
            body,
            abort: _abort,
        }) = result
        else {
            panic!("expected a timed stream");
        };
        let report = ended(end).await;
        assert!(
            matches!(report.error, Some(Error::BudgetExceeded(_))),
            "the idle timer must not replace the budget error: {:?}",
            report.error
        );
        assert_eq!(report.bytes, sent, "the unsent chunk must not be reported");
        assert!(axum::body::to_bytes(body, usize::MAX).await.is_err());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writer_unwind_is_failed_transfer_and_owned_timer_joins() {
        let (body, _, end) = timed(timeouts(), None, |w| {
            w.write_all(b"prefix")?;
            panic!("injected writer unwind");
        })
        .await;
        let failed = axum::body::to_bytes(body, usize::MAX).await.is_err();
        let report = ended(end).await;
        assert!(failed);
        assert!(
            report
                .error
                .unwrap()
                .to_string()
                .contains("producer stopped unexpectedly")
        );
    }
    /// A handler that went away before the switch to streaming leaves the timed handoff
    /// failed and the body uncommitted, so its report gives no rows, though the writer
    /// recorded its rows in the ledger.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_handoff_reports_no_rows() {
        use sparkles::sparql::results as res;
        let store = sparkles::store::Store::in_memory(Default::default());
        let r = sparkles::sparql::query(
            store.snapshot(),
            "SELECT ?x WHERE { VALUES ?x { 1 2 3 } }",
            &Default::default(),
        )
        .unwrap();
        let (gate_tx, gate) = std::sync::mpsc::channel::<()>();
        let release = Release(Some(gate_tx));
        let (send, end) = oneshot::channel();
        let mut serialized = Box::pin(serialize_inner_timed(
            1,
            false,
            None,
            None,
            Some(timeouts()),
            move |w: &mut LimitedWriter<SwitchWriter>| {
                let _ = gate.recv();
                let rows = res::RowCount::with_ledger(w.get_ref().ledger());
                res::write(
                    res::Source::Result(&r),
                    res::WriteOptions::new(
                        res::Format::Solutions(res::SolutionsFormat::Tsv),
                        &rows,
                    ),
                    w,
                )
                .map(|_| ())
            },
            move |e| {
                let _ = send.send(e);
            },
        ));
        // the producer starts and waits at the gate, and the handler goes away
        assert!(futures_util::poll!(serialized.as_mut()).is_pending());
        drop(serialized);
        drop(release);
        let report = ended(end).await;
        assert!(report.error.is_some());
        assert!(!report.committed);
        assert_eq!(report.rows, None);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_body_cancels_and_joins_producer_timer() {
        let (body, _, end) = timed(timeouts(), None, |w| {
            w.write_all(&vec![b'x'; CHUNK * (CHANNEL_CAP + 4)])?;
            Ok(())
        })
        .await;
        drop(body);
        let report = ended(end).await;
        assert!(report.error.is_some());
        assert!(report.disconnected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handoff_writer_unwind_reports_error_and_joins_timer() {
        let control = StreamControl::new(Default::default(), None, Default::default(), None);
        let mut w = LimitedWriter::new(
            SwitchWriter::new_in_place_cursor(1, control.clone()),
            None,
            None,
        );
        w.write_all(b"prefix").unwrap();
        let (send, end) = oneshot::channel();
        let res = finish_handoff_controlled(
            w,
            control,
            timeouts(),
            Instant::now(),
            |switched, _w| {
                switched?;
                panic!("injected handoff writer unwind");
            },
            move |e| {
                let _ = send.send(e);
            },
        )
        .await;
        let Ok(Serialized::Timed { body, .. }) = res else {
            panic!("expected timed stream after handoff switch");
        };
        let failed = axum::body::to_bytes(body, usize::MAX).await.is_err();
        let report = ended(end).await;
        assert!(failed, "panicking handoff producer must abort body");
        assert!(report.committed);
        // Initial chunk is capped at threshold + 1 (2 bytes); remaining 4 bytes never sent.
        assert_eq!(report.bytes, 2);
        assert!(
            report
                .error
                .unwrap()
                .to_string()
                .contains("producer stopped unexpectedly")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handoff_pre_switch_timeout_and_cancel_fail_pre_commit() {
        for is_timeout in [true, false] {
            let cancel = Arc::new(AtomicBool::new(!is_timeout));
            let deadline = is_timeout.then(|| Instant::now() - Duration::from_secs(1));
            let control = StreamControl::new(cancel, deadline, Default::default(), None);
            // Buffer over threshold before handoff while control is not yet expired/cancelled,
            // by using a fresh uncancelled control to fill, then swapping in the expired one.
            let mut sw = SwitchWriter::new_in_place_cursor(
                10,
                StreamControl::new(Default::default(), None, Default::default(), None),
            );
            sw.write_all(&[b'a'; 64]).unwrap();
            sw.control = Some(control.clone());
            let w = LimitedWriter::new(sw, None, None);
            let (send, end) = oneshot::channel();
            let observed_err = Arc::new(std::sync::Mutex::new(None::<String>));
            let observed = observed_err.clone();
            let res = finish_handoff_controlled(
                w,
                control,
                timeouts(),
                Instant::now(),
                move |switched, _w| {
                    if let Err(e) = switched {
                        *observed.lock().unwrap() = Some(format!("{e:?}"));
                        return Err(e);
                    }
                    Ok(())
                },
                move |e| {
                    let _ = send.send(e);
                },
            )
            .await;
            let report = ended(end).await;
            assert!(!report.committed, "must fail pre-commit, not send 200 OK");
            assert_eq!(report.rows, None);
            if is_timeout {
                assert!(matches!(res, Err(ApiError(StatusCode::REQUEST_TIMEOUT, _))));
                assert!(matches!(report.error, Some(Error::Timeout)));
                assert_eq!(
                    observed_err.lock().unwrap().as_deref(),
                    Some("Timeout"),
                    "switched error must be classified before fail_output"
                );
            } else {
                assert!(res.is_err());
                assert!(matches!(report.error, Some(Error::Cancelled)));
                assert_eq!(
                    observed_err.lock().unwrap().as_deref(),
                    Some("Cancelled"),
                    "switched error must be classified before fail_output"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handoff_failed_initial_send_reports_zero_bytes_and_zero_rows() {
        use crate::http::algo::probe::{CURRENT, Gate, Probe, Release};
        use sparkles::sparql::results as res;

        let store = sparkles::store::Store::in_memory(Default::default());
        let r = sparkles::sparql::query(
            store.snapshot(),
            "SELECT ?x WHERE { VALUES ?x { 1 2 3 } }",
            &Default::default(),
        )
        .unwrap();
        let control = StreamControl::new(Default::default(), None, Default::default(), None);
        let sw = SwitchWriter::new_in_place_cursor(1, control.clone());
        let ledger = sw.ledger();
        let mut w = LimitedWriter::new(sw, None, None);
        let rows = res::RowCount::with_ledger(ledger);
        res::write(
            res::Source::Result(&r),
            res::WriteOptions::new(res::Format::Solutions(res::SolutionsFormat::Tsv), &rows),
            &mut w,
        )
        .unwrap();
        assert_eq!(rows.get(), 3);
        assert!(w.written() > 1);

        let gate = Arc::new(Gate::default());
        let _release = Release(gate.clone());
        let probe = Probe {
            producer: Some(gate.clone()),
            ..Default::default()
        };
        let (send, end) = oneshot::channel();
        let serialized = CURRENT
            .scope(
                probe,
                finish_handoff_controlled(
                    w,
                    control,
                    timeouts(),
                    Instant::now(),
                    |switched, _w| switched,
                    move |e| {
                        let _ = send.send(e);
                    },
                ),
            )
            .await;
        let Ok(Serialized::Timed { body, .. }) = serialized else {
            panic!("expected timed stream");
        };
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
        // Drop the receiver before the first chunk enqueues.
        drop(body);
        gate.release();
        let report = ended(end).await;
        assert!(report.committed);
        assert!(report.disconnected);
        assert_eq!(report.bytes, 0, "failed initial send must report 0 bytes");
        assert_eq!(
            report.rows,
            Some(0),
            "failed initial send must report 0 rows"
        );
        assert_eq!(
            report.outcome(),
            (Some(crate::obs::Outcome::Cancelled), None)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handoff_disconnect_after_partial_prefix_counts_only_complete_rows() {
        use crate::http::algo::probe::{CURRENT, Gate, Probe, Release};
        use sparkles::sparql::results as res;

        let store = sparkles::store::Store::in_memory(Default::default());
        let result = sparkles::sparql::query(
            store.snapshot(),
            "SELECT ?x WHERE { VALUES ?x { <urn:a> <urn:b> <urn:c> } } ORDER BY ?x",
            &Default::default(),
        )
        .unwrap();
        let control = StreamControl::new(Default::default(), None, Default::default(), None);
        let prefix = b"?x";
        let sw = SwitchWriter::new_in_place_cursor(prefix.len() - 1, control.clone());
        let rows = res::RowCount::with_ledger(sw.ledger());
        let mut writer = LimitedWriter::new(sw, None, None);
        res::write(
            res::Source::Result(&result),
            res::WriteOptions::new(res::Format::Solutions(res::SolutionsFormat::Tsv), &rows),
            &mut writer,
        )
        .unwrap();
        assert_eq!(rows.get(), 3);

        let gate = Arc::new(Gate::default());
        let _release = Release(gate.clone());
        let (send, end) = oneshot::channel();
        let serialized = CURRENT
            .scope(
                Probe {
                    queued_producer: Some(gate.clone()),
                    ..Default::default()
                },
                finish_handoff_controlled(
                    writer,
                    control,
                    timeouts(),
                    Instant::now(),
                    |switched, _| switched,
                    move |report| {
                        let _ = send.send(report);
                    },
                ),
            )
            .await;
        let Ok(Serialized::Timed { body, .. }) = serialized else {
            panic!("expected timed stream");
        };
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .unwrap();
        let mut body = body.into_data_stream();
        assert_eq!(body.next().await.unwrap().unwrap().as_ref(), prefix);
        drop(body);
        gate.release();

        let report = ended(end).await;
        assert!(report.committed && report.disconnected);
        assert_eq!(report.bytes, prefix.len() as u64);
        assert_eq!(report.rows, Some(0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handoff_initial_chunk_is_bounded_and_overflow_drains_in_chunks() {
        let control = StreamControl::new(Default::default(), None, Default::default(), None);
        let threshold = 100usize;
        let total = threshold + CHUNK * 3 + 17;
        let payload: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        let mut w = LimitedWriter::new(
            SwitchWriter::new_in_place_cursor(threshold, control.clone()),
            None,
            None,
        );
        w.write_all(&payload).unwrap();
        let (send, end) = oneshot::channel();
        let Ok(Serialized::Timed { body, .. }) = finish_handoff_controlled(
            w,
            control,
            timeouts(),
            Instant::now(),
            |switched, _w| switched,
            move |e| {
                let _ = send.send(e);
            },
        )
        .await
        else {
            panic!("expected timed stream");
        };
        let mut stream = body.into_data_stream();
        let mut chunks = Vec::new();
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            let bytes = item.unwrap();
            chunks.push(bytes.len());
            out.extend_from_slice(&bytes);
        }
        let report = ended(end).await;
        assert!(report.error.is_none());
        assert_eq!(report.bytes, total as u64);
        assert_eq!(out, payload);
        assert_eq!(
            chunks[0],
            threshold + 1,
            "initial handoff chunk must not exceed threshold + 1"
        );
        for (idx, &len) in chunks.iter().enumerate().skip(1) {
            assert!(
                len <= CHUNK,
                "post-switch chunk {idx} had {len} bytes > CHUNK ({CHUNK})"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handoff_switch_initial_chunk_does_not_pin_oversized_backing_allocation() {
        let control = StreamControl::new(Default::default(), None, Default::default(), None);
        let threshold = 100usize;
        let max_first = threshold + 1;
        let total = threshold + CHUNK * 4 + 17;
        let payload: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        let mut w = LimitedWriter::new(
            SwitchWriter::new_in_place_cursor(threshold, control.clone()),
            None,
            None,
        );
        w.write_all(&payload).unwrap();
        let (send, end) = oneshot::channel();
        let Ok(Serialized::Timed { body, .. }) = finish_handoff_controlled(
            w,
            control,
            timeouts(),
            Instant::now(),
            |switched, _w| switched,
            move |e| {
                let _ = send.send(e);
            },
        )
        .await
        else {
            panic!("expected timed stream");
        };
        let mut stream = body.into_data_stream();
        let mut chunks = Vec::new();
        while let Some(item) = stream.next().await {
            chunks.push(item.unwrap());
        }
        let report = ended(end).await;
        assert!(report.error.is_none());
        assert_eq!(report.bytes, total as u64);
        assert!(chunks.len() >= 2);

        // Keep all later chunks alive while checking that `first` owns an independent,
        // bounded backing allocation rather than pinning the oversized `Vec<u8>`.
        let first = chunks.remove(0);
        assert_eq!(first.len(), max_first);
        let first_mut = first.try_into_mut().expect(
            "initial handoff chunk must own its backing allocation independently of later chunks",
        );
        assert!(
            first_mut.capacity() <= max_first,
            "initial handoff chunk retained oversized backing capacity {} (max_first={max_first})",
            first_mut.capacity()
        );

        let mut reassembled = first_mut.to_vec();
        for piece in chunks {
            let len = piece.len();
            let piece_mut = piece
                .try_into_mut()
                .expect("overflow handoff chunk must own its backing allocation independently");
            assert!(
                piece_mut.capacity() <= CHUNK,
                "overflow chunk retained oversized backing capacity {} (CHUNK={CHUNK})",
                piece_mut.capacity()
            );
            assert_eq!(piece_mut.len(), len);
            reassembled.extend_from_slice(&piece_mut);
        }
        assert_eq!(reassembled, payload);
    }
}
