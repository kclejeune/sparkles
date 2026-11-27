//! Qualified Streamable HTTP session manager for unmodified `rmcp 3.5` (item 1305).
//!
//! Replaces `rmcp`'s stock `LocalSessionManager` so legacy HTTP sessions (`Mcp-Session-Id`)
//! encode and replay SSE response frames from [`StagedResponse`] under the originating
//! query's memory credit (`Bytes::from_owner(FrameOwner)`), keeping credit live across:
//!
//! 1. in-flight SSE channel buffering and slow client writes (`CreditedSseBody`), and
//! 2. `Last-Event-ID` session replay buffers for the bounded `completed_cache_ttl` window
//!    (including disconnects after Hyper buffers a terminal frame mid-write).
//!
//! Terminal replay entries hold only their trimmed frame bytes (releasing `INITIAL_GRANT`
//! slack immediately) and release to `0` on client-observable `Last-Event-ID`
//! acknowledgment, `notifications/cancelled`, `completed_cache_ttl` expiry, `DELETE`,
//! idle timeout, or `HttpConf.shutdown`.

use super::retained::{Framing, ResponseSlot, StagedResponse};
use axum::body::Bytes;
use futures_util::Stream;
use http_body::{Body, Frame};
use parking_lot::Mutex;
use rmcp::model::{
    CancelledNotificationParam, ClientJsonRpcMessage, ClientNotification, ClientRequest,
    JsonRpcNotification, JsonRpcRequest, Notification, ProgressNotificationParam, ProgressToken,
    RequestId, ServerJsonRpcMessage, ServerNotification,
};
use rmcp::transport::WorkerTransport;
use rmcp::transport::common::server_side_http::{ServerSseMessage, SessionId, session_id};
use rmcp::transport::streamable_http_server::session::local::{
    EventId, EventIdParseError, SessionConfig,
};
use rmcp::transport::streamable_http_server::session::{RestoreOutcome, SessionManager};
use rmcp::transport::worker::{
    Worker, WorkerConfig, WorkerContext, WorkerQuitReason, WorkerSendRequest,
};
use rmcp::{ErrorData as McpError, RoleServer};
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

type HttpRequestId = u64;

/// A single pre-encoded SSE event frame (`data: ...\nid: ...\n\n` or priming).
/// For credited responses, `bytes` is backed by `FrameOwner` (`OwnedBytes`) so cloning
/// into the replay cache or onto a resumed stream shares the exact credited allocation.
#[derive(Clone)]
pub(super) struct CreditedSseEvent {
    pub(super) index: usize,
    pub(super) bytes: Bytes,
    pub(super) terminal: bool,
}

impl CreditedSseEvent {
    fn priming(index: usize, event_id: &str, retry: Duration) -> Self {
        let text = format!(
            "data: \nid: {event_id}\nretry: {}\n\n",
            retry.as_millis() as u64
        );
        Self {
            index,
            bytes: Bytes::from(text),
            terminal: false,
        }
    }

    fn from_uncredited_message(
        index: usize,
        event_id: &str,
        message: &ServerJsonRpcMessage,
        terminal: bool,
    ) -> Self {
        let mut out = Vec::new();
        out.extend_from_slice(b"data: ");
        let data = serde_json::to_string(message).unwrap_or_default();
        for (i, line) in data.split('\n').enumerate() {
            if i > 0 {
                out.extend_from_slice(b"\ndata: ");
            }
            out.extend_from_slice(line.as_bytes());
        }
        out.extend_from_slice(b"\nid: ");
        out.extend_from_slice(event_id.as_bytes());
        out.extend_from_slice(b"\n\n");
        Self {
            index,
            bytes: Bytes::from(out),
            terminal,
        }
    }
}

struct SseQueueInner {
    items: VecDeque<CreditedSseEvent>,
    /// Number of leading events in `items` preloaded as priming/replay. Live overflow
    /// bounds only live events queued after this prefix and discards only that live suffix
    /// so an unread replay prefix remains deliverable before EOF.
    replay_len: usize,
    capacity: usize,
    closed: bool,
    waker: Option<Waker>,
}

/// Non-blocking bounded queue shared between [`CreditedSessionWorker`] and
/// [`CreditedSseBody`].
///
/// Unlike `tokio::sync::mpsc`, this lets the session worker:
/// 1. never await on a stalled SSE client (preventing head-of-line blocking of TTL
///    eviction, cancellation, and `DELETE /$/mcp`), and
/// 2. discard queued credited frames immediately on teardown (`DELETE`, cancellation,
///    TTL expiry, or server shutdown) even if Hyper never polls `CreditedSseBody` again.
#[derive(Clone)]
pub(super) struct SharedSseQueue {
    inner: Arc<Mutex<SseQueueInner>>,
}

impl SharedSseQueue {
    fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            inner: Arc::new(Mutex::new(SseQueueInner {
                items: VecDeque::with_capacity(cap.min(16)),
                replay_len: 0,
                capacity: cap,
                closed: false,
                waker: None,
            })),
        }
    }

    fn closed() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SseQueueInner {
                items: VecDeque::new(),
                replay_len: 0,
                capacity: 1,
                closed: true,
                waker: None,
            })),
        }
    }

    fn is_closed(&self) -> bool {
        self.inner.lock().closed
    }

    /// Push a live `event` without blocking. If the client has fallen behind
    /// (`live_len >= capacity`), closes the queue and discards unread live events after any
    /// still-unread replay prefix so the worker stays responsive, the client still receives
    /// its valid replay window before EOF, and reconnecting via `Last-Event-ID` stays
    /// gap-free against the `C + 1` replay ring.
    fn try_push(&self, event: CreditedSseEvent) -> bool {
        let (waker, dropped) = {
            let mut q = self.inner.lock();
            if q.closed {
                drop(q);
                drop(event);
                return false;
            }
            let replay_len = q.replay_len;
            let live_len = q.items.len().saturating_sub(replay_len);
            if live_len >= q.capacity {
                q.closed = true;
                let dropped_live = q.items.split_off(replay_len);
                let waker = q.waker.take();
                (waker, Some((dropped_live, event)))
            } else {
                q.items.push_back(event);
                let waker = q.waker.take();
                (waker, None)
            }
        };
        let ok = dropped.is_none();
        drop(dropped);
        if let Some(w) = waker {
            w.wake();
        }
        ok
    }

    /// Preload a priming or cached replay event into a freshly established queue without
    /// counting against the live-overflow bound, so live `try_push` stays aligned with
    /// `SessionConfig::channel_capacity` and the `C + 1` replay ring while keeping the
    /// replay prefix deliverable if live overflow closes the queue before the client reads it.
    fn push_replay(&self, event: CreditedSseEvent) -> bool {
        let waker = {
            let mut q = self.inner.lock();
            if q.closed {
                drop(q);
                drop(event);
                return false;
            }
            debug_assert_eq!(
                q.replay_len,
                q.items.len(),
                "push_replay must only preload leading replay/priming events"
            );
            q.items.push_back(event);
            q.replay_len += 1;
            q.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
        true
    }

    /// Signal clean EOF after any already-queued events are drained by the body.
    fn close_eof(&self) {
        let waker = {
            let mut q = self.inner.lock();
            q.closed = true;
            q.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// Close immediately and drop every queued frame outside the lock so credited bytes
    /// release even if the consumer never polls again.
    fn close_and_discard(&self) {
        let (dropped, waker) = {
            let mut q = self.inner.lock();
            q.closed = true;
            q.replay_len = 0;
            (std::mem::take(&mut q.items), q.waker.take())
        };
        drop(dropped);
        if let Some(w) = waker {
            w.wake();
        }
    }

    fn poll_pop(&self, cx: &mut Context<'_>) -> Poll<Option<CreditedSseEvent>> {
        let mut q = self.inner.lock();
        if let Some(event) = q.items.pop_front() {
            q.replay_len = q.replay_len.saturating_sub(1);
            return Poll::Ready(Some(event));
        }
        if q.closed {
            return Poll::Ready(None);
        }
        q.waker = Some(cx.waker().clone());
        Poll::Pending
    }

    #[cfg(test)]
    fn pop_one(&self) -> Option<CreditedSseEvent> {
        let mut q = self.inner.lock();
        let event = q.items.pop_front()?;
        q.replay_len = q.replay_len.saturating_sub(1);
        Some(event)
    }
}

/// Hand-off payload installed into [`HTTP_SSE_SLOT`] by [`CreditedSessionManager`] so
/// `Endpoint::forward` replaces `rmcp`'s placeholder SSE body with a zero-copy
/// [`CreditedSseBody`].
///
/// Acts as a drop guard for the receiver half of [`SharedSseQueue`]: if an HTTP request
/// is cancelled during worker oneshot handoff or before [`CreditedSseBody::new`] takes
/// ownership, dropping the receiver closes and discards the queue immediately so no
/// abandoned primary or request queue stays open without a reader.
pub(super) struct CreditedSseReceiver {
    queue: Option<SharedSseQueue>,
}

impl CreditedSseReceiver {
    pub(super) fn new(queue: SharedSseQueue) -> Self {
        Self { queue: Some(queue) }
    }

    fn into_queue(mut self) -> SharedSseQueue {
        self.queue
            .take()
            .expect("CreditedSseReceiver queue already transferred")
    }
}

impl Drop for CreditedSseReceiver {
    fn drop(&mut self) {
        if let Some(queue) = self.queue.take() {
            queue.close_and_discard();
        }
    }
}

tokio::task_local! {
    /// Per-HTTP-request slot connecting `Endpoint::forward` to `CreditedSessionManager`
    /// without double-serializing large SSE payloads through `rmcp::sse_stream_response`.
    pub(super) static HTTP_SSE_SLOT: Arc<Mutex<Option<CreditedSseReceiver>>>;
}

/// An `http_body::Body` streaming credited SSE frames (`Bytes` backed by `FrameOwner`)
/// with optional keep-alive comments and server shutdown cancellation.
///
/// Dropping the body discards only its unread live queue; the session worker's replay
/// cache retains completed terminal events for the bounded `completed_cache_ttl` window
/// so disconnects after Hyper buffers a frame mid-write can still replay via
/// `Last-Event-ID`.
pub(super) struct CreditedSseBody {
    queue: SharedSseQueue,
    keep_alive: Option<Duration>,
    ping_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    shutdown: CancellationToken,
    shutdown_fut: Pin<Box<tokio_util::sync::WaitForCancellationFutureOwned>>,
    done: bool,
}

impl CreditedSseBody {
    pub(super) fn new(
        receiver: CreditedSseReceiver,
        keep_alive: Option<Duration>,
        shutdown: CancellationToken,
    ) -> Self {
        let ping_sleep = keep_alive.map(|d| Box::pin(tokio::time::sleep(d)));
        let shutdown_fut = Box::pin(shutdown.clone().cancelled_owned());
        Self {
            queue: receiver.into_queue(),
            keep_alive,
            ping_sleep,
            shutdown,
            shutdown_fut,
            done: false,
        }
    }
}

impl Drop for CreditedSseBody {
    fn drop(&mut self) {
        self.queue.close_and_discard();
    }
}

impl Body for CreditedSseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if this.shutdown.is_cancelled() || this.shutdown_fut.as_mut().poll(cx).is_ready() {
            this.done = true;
            this.queue.close_and_discard();
            return Poll::Ready(None);
        }
        match this.queue.poll_pop(cx) {
            Poll::Ready(Some(event)) => {
                if let Some(d) = this.keep_alive
                    && let Some(sleep) = &mut this.ping_sleep
                {
                    sleep.as_mut().reset(tokio::time::Instant::now() + d);
                }
                Poll::Ready(Some(Ok(Frame::data(event.bytes))))
            }
            Poll::Ready(None) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => {
                if let (Some(d), Some(sleep)) = (this.keep_alive, &mut this.ping_sleep)
                    && sleep.as_mut().poll(cx).is_ready()
                {
                    sleep.as_mut().reset(tokio::time::Instant::now() + d);
                    return Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b":\n\n")))));
                }
                Poll::Pending
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum CreditedSessionError {
    SessionServiceTerminated,
    InvalidEventId,
    ChannelClosed(Option<HttpRequestId>),
}

impl std::fmt::Display for CreditedSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionServiceTerminated => f.write_str("session service terminated"),
            Self::InvalidEventId => f.write_str("invalid event id"),
            Self::ChannelClosed(id) => write!(f, "channel closed: {id:?}"),
        }
    }
}

impl std::error::Error for CreditedSessionError {}

#[derive(Debug)]
pub(super) enum CreditedSessionManagerError {
    SessionNotFound(SessionId),
    SessionError(CreditedSessionError),
    InvalidEventId(EventIdParseError),
}

impl std::fmt::Display for CreditedSessionManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SessionNotFound(id) => write!(f, "Session not found: {id}"),
            Self::SessionError(e) => write!(f, "Session error: {e}"),
            Self::InvalidEventId(e) => write!(f, "Invalid event id: {e}"),
        }
    }
}

impl std::error::Error for CreditedSessionManagerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SessionNotFound(_) => None,
            Self::SessionError(e) => Some(e),
            Self::InvalidEventId(e) => Some(e),
        }
    }
}

impl From<CreditedSessionError> for CreditedSessionManagerError {
    fn from(e: CreditedSessionError) -> Self {
        Self::SessionError(e)
    }
}

impl From<EventIdParseError> for CreditedSessionManagerError {
    fn from(e: EventIdParseError) -> Self {
        Self::InvalidEventId(e)
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum ResourceKey {
    McpRequestId(RequestId),
    ProgressToken(ProgressToken),
}

struct ReplayOutcome {
    client_acked_terminal: bool,
}

struct CachedChannel {
    tx: SharedSseQueue,
    cache: VecDeque<CreditedSseEvent>,
    http_request_id: Option<HttpRequestId>,
    capacity: usize,
    next_index: usize,
}

impl CachedChannel {
    fn new(tx: SharedSseQueue, http_request_id: Option<HttpRequestId>, capacity: usize) -> Self {
        // Hold one more event than the live `SharedSseQueue` capacity so that when a
        // stalled live queue overflows on the `(capacity + 1)`-th unread event and closes,
        // the client's next unread index `n + 1` is still present for gap-free replay.
        let cache_capacity = capacity.max(1).saturating_add(1);
        Self {
            cache: VecDeque::with_capacity(cache_capacity.min(16)),
            capacity: cache_capacity,
            tx,
            http_request_id,
            next_index: 0,
        }
    }

    fn format_event_id(&self, index: usize) -> String {
        match self.http_request_id {
            Some(id) => format!("{index}/{id}"),
            None => format!("{index}"),
        }
    }

    fn alloc_index(&mut self) -> (usize, String) {
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        let id = self.format_event_id(index);
        (index, id)
    }

    fn send_priming(&mut self, retry: Duration) {
        let (index, event_id) = self.alloc_index();
        let event = CreditedSseEvent::priming(index, &event_id, retry);
        if self.cache.len() >= self.capacity {
            self.cache.pop_front();
        }
        self.cache.push_back(event.clone());
        let _ = self.tx.push_replay(event);
    }

    fn send_event(&mut self, event: CreditedSseEvent) -> bool {
        self.cache_and_send(event)
    }

    fn cache_and_send(&mut self, event: CreditedSseEvent) -> bool {
        if self.cache.len() >= self.capacity {
            self.cache.pop_front();
        }
        self.cache.push_back(event.clone());
        self.tx.try_push(event)
    }

    /// Validate `last_index` against the replay ring **before** mutating or replacing `tx`.
    ///
    /// Rejects future indexes (`last_index >= next_index`) and gaps where intermediate
    /// events rolled off the ring, except when `allow_gap_for_terminal` is true and a
    /// completed request channel retains its terminal JSON-RPC response in the ring so a
    /// stalled client can still recover that terminal response after intermediate progress
    /// notifications rolled off.
    fn can_replay_after(
        &self,
        last_index: usize,
        allow_gap_for_terminal: bool,
    ) -> Result<(), CreditedSessionError> {
        if last_index >= self.next_index {
            return Err(CreditedSessionError::InvalidEventId);
        }
        let Some(front) = self.cache.front() else {
            return Ok(());
        };
        if last_index.saturating_add(1) < front.index {
            let has_terminal =
                allow_gap_for_terminal && self.cache.back().is_some_and(|e| e.terminal);
            if !has_terminal {
                return Err(CreditedSessionError::InvalidEventId);
            }
        }
        Ok(())
    }

    /// Replay events strictly after `last_index` (matching SSE `Last-Event-ID` semantics)
    /// without blocking. Returns whether the client's `last_index` already acknowledged a
    /// cached terminal event.
    fn replay_after(
        &mut self,
        last_index: usize,
        allow_gap_for_terminal: bool,
    ) -> Result<ReplayOutcome, CreditedSessionError> {
        self.can_replay_after(last_index, allow_gap_for_terminal)?;
        let client_acked_terminal = self
            .cache
            .iter()
            .any(|e| e.terminal && e.index <= last_index);
        for event in self.cache.iter().filter(|e| e.index > last_index) {
            if !self.tx.push_replay(event.clone()) {
                return Err(CreditedSessionError::ChannelClosed(self.http_request_id));
            }
        }
        Ok(ReplayOutcome {
            client_acked_terminal,
        })
    }

    /// Replay every event currently held in the ring buffer (used when a fresh standalone
    /// `GET /$/mcp` without `Last-Event-ID` replaces the dead initial primary).
    fn replay_all(&mut self) -> Result<(), CreditedSessionError> {
        for event in &self.cache {
            if !self.tx.push_replay(event.clone()) {
                return Err(CreditedSessionError::ChannelClosed(self.http_request_id));
            }
        }
        Ok(())
    }
}

struct RequestChannel {
    mcp_request_id: Option<RequestId>,
    resources: HashSet<ResourceKey>,
    channel: CachedChannel,
    completed_at: Option<Instant>,
}

type RequestWiseChannelResult = Result<(HttpRequestId, CreditedSseReceiver), CreditedSessionError>;
type ResumeChannelResult =
    Result<(Option<HttpRequestId>, CreditedSseReceiver), CreditedSessionError>;

enum SessionEvent {
    /// Atomically establish a request-wise channel and forward the initiating client
    /// message so cancelling `create_stream` mid-flight cannot orphan a `RequestChannel`.
    CreateRequestStream {
        message: ClientJsonRpcMessage,
        responder: oneshot::Sender<RequestWiseChannelResult>,
    },
    ClientMessage {
        message: ClientJsonRpcMessage,
        http_request_id: Option<HttpRequestId>,
    },
    EstablishCommonChannel {
        responder: oneshot::Sender<Result<CreditedSseReceiver, CreditedSessionError>>,
    },
    Resume {
        last_event_id: EventId,
        responder: oneshot::Sender<ResumeChannelResult>,
    },
    InitializeRequest {
        request: ClientJsonRpcMessage,
        responder: oneshot::Sender<Result<ServerJsonRpcMessage, CreditedSessionError>>,
    },
    Close,
}

#[derive(Clone)]
pub(super) struct CreditedSessionHandle {
    event_tx: mpsc::Sender<SessionEvent>,
    /// Independent teardown token (child of `HttpConf.shutdown`), cancelled immediately by
    /// `DELETE /$/mcp` regardless of `event_tx` backpressure.
    session_ct: CancellationToken,
}

impl CreditedSessionHandle {
    fn close(&self) {
        self.session_ct.cancel();
        let _ = self.event_tx.try_send(SessionEvent::Close);
    }

    async fn push_message(
        &self,
        message: ClientJsonRpcMessage,
        http_request_id: Option<HttpRequestId>,
    ) -> Result<(), CreditedSessionError> {
        self.event_tx
            .send(SessionEvent::ClientMessage {
                message,
                http_request_id,
            })
            .await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)
    }

    async fn create_request_stream(
        &self,
        message: ClientJsonRpcMessage,
    ) -> Result<(HttpRequestId, CreditedSseReceiver), CreditedSessionError> {
        let (tx, rx) = oneshot::channel();
        self.event_tx
            .send(SessionEvent::CreateRequestStream {
                message,
                responder: tx,
            })
            .await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?;
        rx.await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?
    }

    async fn establish_common_channel(&self) -> Result<CreditedSseReceiver, CreditedSessionError> {
        let (tx, rx) = oneshot::channel();
        self.event_tx
            .send(SessionEvent::EstablishCommonChannel { responder: tx })
            .await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?;
        rx.await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?
    }

    async fn resume(
        &self,
        last_event_id: EventId,
    ) -> Result<(Option<HttpRequestId>, CreditedSseReceiver), CreditedSessionError> {
        let (tx, rx) = oneshot::channel();
        self.event_tx
            .send(SessionEvent::Resume {
                last_event_id,
                responder: tx,
            })
            .await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?;
        rx.await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?
    }

    async fn initialize(
        &self,
        request: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, CreditedSessionError> {
        let (tx, rx) = oneshot::channel();
        self.event_tx
            .send(SessionEvent::InitializeRequest {
                request,
                responder: tx,
            })
            .await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?;
        rx.await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?
    }
}

#[derive(Debug)]
pub(super) enum CreditedWorkerError {
    TransportTerminated,
    UnexpectedEvent,
    InitializeFailed,
    Session(CreditedSessionError),
    InitTimeout(Duration),
    TransportClosed,
    Join(tokio::task::JoinError),
}

impl std::fmt::Display for CreditedWorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TransportTerminated => f.write_str("transport terminated"),
            Self::UnexpectedEvent => f.write_str("unexpected session event"),
            Self::InitializeFailed => f.write_str("failed to send initialize response"),
            Self::Session(e) => write!(f, "session error: {e}"),
            Self::InitTimeout(d) => write!(f, "init timeout after {}ms", d.as_millis()),
            Self::TransportClosed => f.write_str("transport closed"),
            Self::Join(e) => write!(f, "tokio join error: {e}"),
        }
    }
}

impl std::error::Error for CreditedWorkerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Session(e) => Some(e),
            Self::Join(e) => Some(e),
            _ => None,
        }
    }
}

impl From<CreditedSessionError> for CreditedWorkerError {
    fn from(e: CreditedSessionError) -> Self {
        Self::Session(e)
    }
}

impl From<tokio::task::JoinError> for CreditedWorkerError {
    fn from(e: tokio::task::JoinError) -> Self {
        Self::Join(e)
    }
}

fn close_message_slot(message: &ClientJsonRpcMessage) {
    use rmcp::model::GetExtensions;
    if let ClientJsonRpcMessage::Request(req) = message
        && let Some(slot) = req
            .request
            .extensions()
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<ResponseSlot>())
    {
        slot.close();
    }
}

pub(super) struct CreditedSessionWorker {
    id: SessionId,
    next_http_request_id: HttpRequestId,
    tx_router: HashMap<HttpRequestId, RequestChannel>,
    resource_router: HashMap<ResourceKey, HttpRequestId>,
    /// Tracks completed request channels by MCP `RequestId` for their replay lifetime so
    /// a `notifications/cancelled` arriving after the terminal frame is cached still
    /// evicts the replay entry and discards queued body frames.
    replay_by_request_id: HashMap<RequestId, HttpRequestId>,
    /// Per-request staged response slots extracted from inbound HTTP `Parts`.
    pending_slots: HashMap<RequestId, ResponseSlot>,
    common: CachedChannel,
    shadow_queues: Vec<SharedSseQueue>,
    event_rx: mpsc::Receiver<SessionEvent>,
    session_ct: CancellationToken,
    config: SessionConfig,
    max_timeout_secs: f64,
}

impl Drop for CreditedSessionWorker {
    fn drop(&mut self) {
        for (_, slot) in self.pending_slots.drain() {
            slot.close();
        }
        self.common.tx.close_and_discard();
        for (_, rw) in self.tx_router.drain() {
            rw.channel.tx.close_and_discard();
        }
        for q in self.shadow_queues.drain(..) {
            q.close_and_discard();
        }
    }
}

enum OutboundChannel {
    RequestWise { id: HttpRequestId, close: bool },
    Common,
}

impl CreditedSessionWorker {
    fn next_eviction_deadline(&self) -> Option<Instant> {
        let ttl = self.config.completed_cache_ttl;
        self.tx_router
            .values()
            .filter_map(|rw| rw.completed_at.map(|at| at + ttl))
            .min()
    }

    fn evict_expired_channels(&mut self) {
        let ttl = self.config.completed_cache_ttl;
        let expired: Vec<HttpRequestId> = self
            .tx_router
            .iter()
            .filter_map(|(&id, rw)| {
                rw.completed_at
                    .is_some_and(|at| at.elapsed() >= ttl)
                    .then_some(id)
            })
            .collect();
        for id in expired {
            self.remove_request_channel(id);
        }
    }

    fn remove_request_channel(&mut self, http_request_id: HttpRequestId) {
        if let Some(channel) = self.tx_router.remove(&http_request_id) {
            channel.channel.tx.close_and_discard();
            for r in &channel.resources {
                if self.resource_router.get(r) == Some(&http_request_id) {
                    self.resource_router.remove(r);
                }
            }
            if let Some(mcp_id) = &channel.mcp_request_id
                && self.replay_by_request_id.get(mcp_id) == Some(&http_request_id)
            {
                self.replay_by_request_id.remove(mcp_id);
            }
        }
    }

    fn is_in_flight(&self, id: &RequestId) -> bool {
        self.pending_slots.contains_key(id)
            || self
                .resource_router
                .contains_key(&ResourceKey::McpRequestId(id.clone()))
    }

    fn register_request(
        &mut self,
        request: &JsonRpcRequest<ClientRequest>,
        http_request_id: Option<HttpRequestId>,
    ) {
        use rmcp::model::{GetExtensions, GetMeta};
        if let Some(slot) = request
            .request
            .extensions()
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<ResponseSlot>().cloned())
        {
            self.pending_slots.insert(request.id.clone(), slot);
        }
        if let Some(http_request_id) = http_request_id
            && let Some(channel) = self.tx_router.get_mut(&http_request_id)
        {
            channel.mcp_request_id = Some(request.id.clone());
            let key = ResourceKey::McpRequestId(request.id.clone());
            channel.resources.insert(key.clone());
            self.resource_router.insert(key, http_request_id);
            if let Some(token) = request.request.get_meta().get_progress_token() {
                let pkey = ResourceKey::ProgressToken(token.clone());
                channel.resources.insert(pkey.clone());
                self.resource_router.insert(pkey, http_request_id);
            }
        }
    }

    fn catch_cancellation(&mut self, notification: &JsonRpcNotification<ClientNotification>) {
        if let ClientNotification::CancelledNotification(n) = &notification.notification
            && let Some(request_id) = &n.params.request_id
        {
            if let Some(slot) = self.pending_slots.remove(request_id) {
                slot.close();
            }
            let key = ResourceKey::McpRequestId(request_id.clone());
            if let Some(http_request_id) = self.resource_router.remove(&key) {
                self.remove_request_channel(http_request_id);
            }
            if let Some(http_request_id) = self.replay_by_request_id.remove(request_id) {
                self.remove_request_channel(http_request_id);
            }
        }
    }

    fn alloc_request_wise_channel(&mut self) -> (HttpRequestId, CreditedSseReceiver) {
        let http_request_id = self.next_http_request_id;
        self.next_http_request_id = self.next_http_request_id.wrapping_add(1);
        let queue = SharedSseQueue::new(self.config.channel_capacity);
        let mut channel = CachedChannel::new(
            queue.clone(),
            Some(http_request_id),
            self.config.channel_capacity,
        );
        if let Some(retry) = self.config.sse_retry {
            channel.send_priming(retry);
        }
        self.tx_router.insert(
            http_request_id,
            RequestChannel {
                mcp_request_id: None,
                resources: HashSet::new(),
                channel,
                completed_at: None,
            },
        );
        (http_request_id, CreditedSseReceiver::new(queue))
    }

    fn reject_duplicate_request_stream(
        &mut self,
        req_id: RequestId,
    ) -> (HttpRequestId, CreditedSseReceiver) {
        let (http_request_id, receiver) = self.alloc_request_wise_channel();
        if let Some(channel) = self.tx_router.get_mut(&http_request_id) {
            let (index, event_id) = channel.channel.alloc_index();
            let err_msg = ServerJsonRpcMessage::error(
                McpError::invalid_request("duplicate in-flight request id", None),
                Some(req_id),
            );
            let event = CreditedSseEvent::from_uncredited_message(index, &event_id, &err_msg, true);
            channel.channel.send_event(event);
            channel.completed_at = Some(Instant::now());
            channel.channel.tx.close_eof();
        }
        (http_request_id, receiver)
    }

    fn resolve_outbound_channel(&self, message: &ServerJsonRpcMessage) -> OutboundChannel {
        match message {
            ServerJsonRpcMessage::Request(req) => {
                use rmcp::model::GetExtensions;
                req.request
                    .extensions()
                    .get::<rmcp::service::OriginatingRequestId>()
                    .and_then(|o| {
                        self.resource_router
                            .get(&ResourceKey::McpRequestId(o.0.clone()))
                    })
                    .map_or(OutboundChannel::Common, |&id| {
                        OutboundChannel::RequestWise { id, close: false }
                    })
            }
            ServerJsonRpcMessage::Notification(JsonRpcNotification {
                notification:
                    ServerNotification::ProgressNotification(Notification {
                        params: ProgressNotificationParam { progress_token, .. },
                        ..
                    }),
                ..
            }) => self
                .resource_router
                .get(&ResourceKey::ProgressToken(progress_token.clone()))
                .map_or(OutboundChannel::Common, |&id| {
                    OutboundChannel::RequestWise { id, close: false }
                }),
            ServerJsonRpcMessage::Notification(JsonRpcNotification {
                notification:
                    ServerNotification::CancelledNotification(Notification {
                        params:
                            CancelledNotificationParam {
                                request_id: Some(req_id),
                                ..
                            },
                        ..
                    }),
                ..
            }) => self
                .resource_router
                .get(&ResourceKey::McpRequestId(req_id.clone()))
                .map_or(OutboundChannel::Common, |&id| {
                    OutboundChannel::RequestWise { id, close: false }
                }),
            ServerJsonRpcMessage::Notification(_) => OutboundChannel::Common,
            ServerJsonRpcMessage::Response(resp) => self
                .resource_router
                .get(&ResourceKey::McpRequestId(resp.id.clone()))
                .map_or(OutboundChannel::Common, |&id| {
                    OutboundChannel::RequestWise { id, close: true }
                }),
            ServerJsonRpcMessage::Error(err) => err
                .id
                .as_ref()
                .and_then(|rid| {
                    self.resource_router
                        .get(&ResourceKey::McpRequestId(rid.clone()))
                })
                .map_or(OutboundChannel::Common, |&id| {
                    OutboundChannel::RequestWise { id, close: true }
                }),
        }
    }

    fn take_staged(&mut self, message: &ServerJsonRpcMessage) -> Option<StagedResponse> {
        let id = match message {
            ServerJsonRpcMessage::Response(resp) => Some(&resp.id),
            ServerJsonRpcMessage::Error(err) => err.id.as_ref(),
            _ => None,
        }?;
        self.pending_slots.remove(id).and_then(|slot| slot.take())
    }

    fn handle_server_message(
        &mut self,
        message: ServerJsonRpcMessage,
    ) -> Result<(), CreditedSessionError> {
        let staged = self.take_staged(&message);
        match self.resolve_outbound_channel(&message) {
            OutboundChannel::RequestWise { id, close } => {
                let Some(request_wise) = self.tx_router.get_mut(&id) else {
                    drop(staged);
                    return Err(CreditedSessionError::ChannelClosed(Some(id)));
                };
                let (index, event_id) = request_wise.channel.alloc_index();
                let event = match staged {
                    Some(staged) => {
                        let bytes = staged.into_frame_or_error(
                            Framing::Sse {
                                event_id: Some(&event_id),
                                retry: None,
                            },
                            self.max_timeout_secs,
                        );
                        CreditedSseEvent {
                            index,
                            bytes,
                            terminal: close,
                        }
                    }
                    None => {
                        CreditedSseEvent::from_uncredited_message(index, &event_id, &message, close)
                    }
                };
                let _ = request_wise.channel.send_event(event);
                if close {
                    let resources: Vec<_> = request_wise.resources.drain().collect();
                    let mcp_id = request_wise.mcp_request_id.clone();
                    request_wise.completed_at = Some(Instant::now());
                    // Signal clean EOF on the live stream after the terminal event while
                    // keeping the replay cache for `completed_cache_ttl` (or until client
                    // acknowledgment, cancellation, DELETE, or shutdown).
                    request_wise.channel.tx.close_eof();
                    for r in resources {
                        if self.resource_router.get(&r) == Some(&id) {
                            self.resource_router.remove(&r);
                        }
                    }
                    if let Some(mcp_id) = mcp_id {
                        self.replay_by_request_id.insert(mcp_id, id);
                    }
                }
            }
            OutboundChannel::Common => {
                // Never pin an unowned credited StagedResponse inside `common.cache`
                // without a request-wise replay TTL.
                if let Some(staged) = staged {
                    tracing::warn!(
                        id = %staged.id(),
                        "dropping orphaned staged response routed to common SSE stream"
                    );
                    drop(staged);
                    return Ok(());
                }
                let (index, event_id) = self.common.alloc_index();
                let event =
                    CreditedSseEvent::from_uncredited_message(index, &event_id, &message, false);
                let _ = self.common.send_event(event);
            }
        }
        Ok(())
    }

    fn resume(
        &mut self,
        last_event_id: EventId,
    ) -> Result<(Option<HttpRequestId>, CreditedSseReceiver), CreditedSessionError> {
        self.shadow_queues.retain(|q| !q.is_closed());
        let (index, req_id) = parse_event_id_parts(&last_event_id);
        match req_id {
            Some(http_request_id) => {
                let (receiver, acked_terminal) = {
                    let request_wise = self
                        .tx_router
                        .get_mut(&http_request_id)
                        .ok_or(CreditedSessionError::ChannelClosed(Some(http_request_id)))?;
                    let is_completed = request_wise.completed_at.is_some();
                    // Validate cursor BEFORE closing or replacing the active live stream.
                    request_wise.channel.can_replay_after(index, is_completed)?;
                    // Keep the live-overflow bound aligned with `channel_capacity` (and
                    // the `C + 1` replay ring); `replay_after` preloads cached events via
                    // `push_replay` without triggering live overflow.
                    let queue = SharedSseQueue::new(self.config.channel_capacity);
                    let receiver = CreditedSseReceiver::new(queue.clone());
                    request_wise.channel.tx.close_and_discard();
                    request_wise.channel.tx = queue;
                    let outcome = request_wise.channel.replay_after(index, is_completed)?;
                    if is_completed {
                        request_wise.channel.tx.close_eof();
                    }
                    (receiver, is_completed && outcome.client_acked_terminal)
                };
                if acked_terminal {
                    // Client explicitly sent a `Last-Event-ID` at or past the terminal
                    // event, proving delivery; evict the replay entry now.
                    self.remove_request_channel(http_request_id);
                }
                Ok((Some(http_request_id), receiver))
            }
            None => {
                let receiver = self.resume_or_shadow_common(Some(index))?;
                Ok((None, receiver))
            }
        }
    }

    fn establish_common_channel(&mut self) -> Result<CreditedSseReceiver, CreditedSessionError> {
        self.resume_or_shadow_common(None)
    }

    fn resume_or_shadow_common(
        &mut self,
        last_event_index: Option<usize>,
    ) -> Result<CreditedSseReceiver, CreditedSessionError> {
        self.shadow_queues.retain(|q| !q.is_closed());
        let replacing = self.common.tx.is_closed();
        if replacing && let Some(idx) = last_event_index {
            // Validate cursor BEFORE creating or installing a replacement primary queue.
            self.common.can_replay_after(idx, false)?;
        }
        let cap = if replacing {
            self.config.channel_capacity
        } else {
            2
        };
        let queue = SharedSseQueue::new(cap);
        let receiver = CreditedSseReceiver::new(queue.clone());
        if last_event_index.is_none()
            && let Some(retry) = self.config.sse_retry
        {
            let _ = queue.push_replay(CreditedSseEvent::priming(0, "0", retry));
        }
        if replacing {
            // Discard any unread replay prefix still sitting in the superseded primary
            // queue so it cannot leak outside session bounds or emit after teardown.
            self.common.tx.close_and_discard();
            self.common.tx = queue;
            match last_event_index {
                Some(idx) => {
                    let _ = self.common.replay_after(idx, false)?;
                }
                None => {
                    // Fresh standalone `GET /$/mcp` without `Last-Event-ID`: replay any
                    // queued common notifications (index >= 1), matching stock
                    // `LocalSessionWorker` backlog semantics without failing if older
                    // notifications already rolled off the ring buffer.
                    self.common.replay_all()?;
                }
            }
        } else {
            const MAX_SHADOW_STREAMS: usize = 32;
            if self.shadow_queues.len() >= MAX_SHADOW_STREAMS {
                let evicted = self.shadow_queues.remove(0);
                evicted.close_and_discard();
            }
            self.shadow_queues.push(queue);
        }
        Ok(receiver)
    }
}

fn parse_event_id_parts(id: &EventId) -> (usize, Option<HttpRequestId>) {
    let s = id.to_string();
    match s.split_once('/') {
        Some((idx, req)) => (idx.parse().unwrap_or(0), req.parse::<HttpRequestId>().ok()),
        None => (s.parse().unwrap_or(0), None),
    }
}

impl Worker for CreditedSessionWorker {
    type Error = CreditedWorkerError;
    type Role = RoleServer;

    fn err_closed() -> Self::Error {
        CreditedWorkerError::TransportClosed
    }

    fn err_join(e: tokio::task::JoinError) -> Self::Error {
        CreditedWorkerError::Join(e)
    }

    fn config(&self) -> WorkerConfig {
        let mut cfg = WorkerConfig::default();
        cfg.name = Some(format!("credited-mcp-session-{}", self.id));
        cfg.channel_buffer_capacity = self.config.channel_capacity;
        cfg
    }

    async fn run(
        mut self,
        mut context: WorkerContext<Self>,
    ) -> Result<(), WorkerQuitReason<Self::Error>> {
        enum InnerEvent {
            FromHttpService(SessionEvent),
            FromHandler(WorkerSendRequest<CreditedSessionWorker>),
        }
        let ct = context.cancellation_token.clone();
        let session_ct = self.session_ct.clone();
        let init_timeout = self.config.init_timeout.unwrap_or(Duration::MAX);
        let evt = tokio::select! {
            biased;
            _ = session_ct.cancelled() => {
                return Err(WorkerQuitReason::TransportClosed);
            }
            _ = ct.cancelled() => {
                return Err(WorkerQuitReason::Cancelled);
            }
            evt = self.event_rx.recv() => evt.ok_or_else(|| {
                WorkerQuitReason::fatal(
                    CreditedWorkerError::TransportTerminated,
                    "get initialize request",
                )
            })?,
            _ = tokio::time::sleep(init_timeout) => {
                return Err(WorkerQuitReason::fatal(
                    CreditedWorkerError::InitTimeout(init_timeout),
                    "waiting for initialize request",
                ));
            }
        };
        let SessionEvent::InitializeRequest { request, responder } = evt else {
            return Err(WorkerQuitReason::fatal(
                CreditedWorkerError::UnexpectedEvent,
                "get initialize request",
            ));
        };
        tokio::select! {
            biased;
            _ = session_ct.cancelled() => return Err(WorkerQuitReason::TransportClosed),
            _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
            res = context.send_to_handler(request) => res?,
        }
        let init_resp = tokio::select! {
            biased;
            _ = session_ct.cancelled() => return Err(WorkerQuitReason::TransportClosed),
            _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
            res = context.recv_from_handler() => res?,
        };
        responder.send(Ok(init_resp.message)).map_err(|_| {
            WorkerQuitReason::fatal(
                CreditedWorkerError::InitializeFailed,
                "send initialize response",
            )
        })?;
        init_resp
            .responder
            .send(Ok(()))
            .map_err(|_| WorkerQuitReason::HandlerTerminated)?;

        let keep_alive = self.config.keep_alive.unwrap_or(Duration::MAX);
        let mut last_activity = Instant::now();
        loop {
            self.evict_expired_channels();
            let now = Instant::now();
            let idle_wait = keep_alive.saturating_sub(now.saturating_duration_since(last_activity));
            let evict_wait = self
                .next_eviction_deadline()
                .map(|deadline| deadline.saturating_duration_since(now));
            let event = tokio::select! {
                biased;
                _ = session_ct.cancelled() => return Err(WorkerQuitReason::TransportClosed),
                _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
                evt = self.event_rx.recv() => match evt {
                    Some(e) => {
                        last_activity = Instant::now();
                        InnerEvent::FromHttpService(e)
                    }
                    None => {
                        return Err(WorkerQuitReason::fatal(
                            CreditedWorkerError::TransportTerminated,
                            "waiting next session event",
                        ));
                    }
                },
                from_handler = context.recv_from_handler() => {
                    last_activity = Instant::now();
                    InnerEvent::FromHandler(from_handler?)
                }
                _ = async {
                    if let Some(wait) = evict_wait {
                        tokio::time::sleep(wait).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    self.evict_expired_channels();
                    continue;
                }
                _ = tokio::time::sleep(idle_wait) => {
                    return Err(WorkerQuitReason::IdleTimeout(keep_alive));
                }
            };
            match event {
                InnerEvent::FromHandler(WorkerSendRequest {
                    message, responder, ..
                }) => {
                    let res = self
                        .handle_server_message(message)
                        .map_err(CreditedWorkerError::Session);
                    let _ = responder.send(res);
                }
                InnerEvent::FromHttpService(SessionEvent::CreateRequestStream {
                    message,
                    responder,
                }) => {
                    if responder.is_closed() {
                        close_message_slot(&message);
                        continue;
                    }
                    if let ClientJsonRpcMessage::Request(req) = &message
                        && self.is_in_flight(&req.id)
                    {
                        let dup_id = req.id.clone();
                        close_message_slot(&message);
                        let (dup_http_id, dup_rx) = self.reject_duplicate_request_stream(dup_id);
                        if responder.send(Ok((dup_http_id, dup_rx))).is_err() {
                            self.remove_request_channel(dup_http_id);
                        }
                        continue;
                    }
                    let (http_request_id, receiver) = self.alloc_request_wise_channel();
                    match &message {
                        ClientJsonRpcMessage::Request(req) => {
                            self.register_request(req, Some(http_request_id));
                        }
                        ClientJsonRpcMessage::Notification(n) => {
                            self.catch_cancellation(n);
                        }
                        _ => {}
                    }
                    if responder.send(Ok((http_request_id, receiver))).is_err() {
                        // Caller cancelled before receiving the stream handle.
                        close_message_slot(&message);
                        if let ClientJsonRpcMessage::Request(req) = &message {
                            self.pending_slots.remove(&req.id);
                        }
                        self.remove_request_channel(http_request_id);
                        continue;
                    }
                    tokio::select! {
                        biased;
                        _ = session_ct.cancelled() => return Err(WorkerQuitReason::TransportClosed),
                        _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
                        res = context.send_to_handler(message) => res?,
                    }
                }
                InnerEvent::FromHttpService(SessionEvent::ClientMessage {
                    message,
                    http_request_id,
                }) => {
                    if let ClientJsonRpcMessage::Request(req) = &message
                        && self.is_in_flight(&req.id)
                    {
                        close_message_slot(&message);
                        continue;
                    }
                    match &message {
                        ClientJsonRpcMessage::Request(req) => {
                            self.register_request(req, http_request_id);
                        }
                        ClientJsonRpcMessage::Notification(n) => {
                            self.catch_cancellation(n);
                        }
                        _ => {}
                    }
                    tokio::select! {
                        biased;
                        _ = session_ct.cancelled() => return Err(WorkerQuitReason::TransportClosed),
                        _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
                        res = context.send_to_handler(message) => res?,
                    }
                }
                InnerEvent::FromHttpService(SessionEvent::EstablishCommonChannel { responder }) => {
                    if responder.is_closed() {
                        continue;
                    }
                    let res = self.establish_common_channel();
                    let _ = responder.send(res);
                }
                InnerEvent::FromHttpService(SessionEvent::Resume {
                    last_event_id,
                    responder,
                }) => {
                    if responder.is_closed() {
                        continue;
                    }
                    let res = self.resume(last_event_id);
                    let _ = responder.send(res);
                }
                InnerEvent::FromHttpService(SessionEvent::Close) => {
                    return Err(WorkerQuitReason::TransportClosed);
                }
                InnerEvent::FromHttpService(SessionEvent::InitializeRequest { .. }) => {}
            }
        }
    }
}

/// Session manager for `/$/mcp` that keeps SSE frames and `Last-Event-ID` replay caches
/// under their originating query's `QueryPool` credit.
pub(super) struct CreditedSessionManager {
    pub(super) sessions: tokio::sync::RwLock<HashMap<SessionId, CreditedSessionHandle>>,
    pub(super) session_config: SessionConfig,
    max_timeout_secs: f64,
    shutdown: CancellationToken,
}

impl CreditedSessionManager {
    pub(super) fn new(
        session_config: SessionConfig,
        max_timeout_secs: f64,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            sessions: tokio::sync::RwLock::new(HashMap::new()),
            session_config,
            max_timeout_secs,
            shutdown,
        }
    }

    fn install_sse_receiver(receiver: CreditedSseReceiver) {
        let _ = HTTP_SSE_SLOT.try_with(|slot| {
            *slot.lock() = Some(receiver);
        });
    }
}

fn create_credited_session(
    id: SessionId,
    config: SessionConfig,
    max_timeout_secs: f64,
    session_ct: CancellationToken,
) -> (CreditedSessionHandle, CreditedSessionWorker) {
    let (event_tx, event_rx) = mpsc::channel(config.channel_capacity);
    let mut common = CachedChannel::new(SharedSseQueue::closed(), None, config.channel_capacity);
    // Standalone stream priming uses event ID "0"; real notifications start at "1".
    common.next_index = 1;
    let handle = CreditedSessionHandle {
        event_tx,
        session_ct: session_ct.clone(),
    };
    let worker = CreditedSessionWorker {
        id,
        next_http_request_id: 0,
        tx_router: HashMap::new(),
        resource_router: HashMap::new(),
        replay_by_request_id: HashMap::new(),
        pending_slots: HashMap::new(),
        common,
        shadow_queues: Vec::new(),
        event_rx,
        session_ct,
        config,
        max_timeout_secs,
    };
    (handle, worker)
}

impl SessionManager for CreditedSessionManager {
    type Error = CreditedSessionManagerError;
    type Transport = WorkerTransport<CreditedSessionWorker>;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        let id = session_id();
        let session_ct = self.shutdown.child_token();
        let (handle, worker) = create_credited_session(
            id.clone(),
            self.session_config.clone(),
            self.max_timeout_secs,
            session_ct.clone(),
        );
        self.sessions.write().await.insert(id.clone(), handle);
        Ok((id, WorkerTransport::spawn_with_ct(worker, session_ct)))
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        let handle = {
            let sessions = self.sessions.read().await;
            sessions
                .get(id)
                .cloned()
                .ok_or_else(|| CreditedSessionManagerError::SessionNotFound(id.clone()))?
        };
        let response = handle.initialize(message).await?;
        Ok(response)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        let sessions = self.sessions.read().await;
        Ok(sessions.contains_key(id))
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        let handle = self.sessions.write().await.remove(id);
        if let Some(handle) = handle {
            handle.close();
        }
        Ok(())
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + 'static, Self::Error> {
        let handle = {
            let sessions = self.sessions.read().await;
            sessions
                .get(id)
                .cloned()
                .ok_or_else(|| CreditedSessionManagerError::SessionNotFound(id.clone()))?
        };
        let (_http_request_id, receiver) = handle.create_request_stream(message).await?;
        Self::install_sse_receiver(receiver);
        // `Endpoint::forward` replaces the placeholder SSE body with `CreditedSseBody`.
        Ok(futures_util::stream::empty::<ServerSseMessage>())
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        let handle = {
            let sessions = self.sessions.read().await;
            sessions
                .get(id)
                .cloned()
                .ok_or_else(|| CreditedSessionManagerError::SessionNotFound(id.clone()))?
        };
        handle.push_message(message, None).await?;
        Ok(())
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + 'static, Self::Error> {
        let handle = {
            let sessions = self.sessions.read().await;
            sessions
                .get(id)
                .cloned()
                .ok_or_else(|| CreditedSessionManagerError::SessionNotFound(id.clone()))?
        };
        let receiver = handle.establish_common_channel().await?;
        Self::install_sse_receiver(receiver);
        Ok(futures_util::stream::empty::<ServerSseMessage>())
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + 'static, Self::Error> {
        let handle = {
            let sessions = self.sessions.read().await;
            sessions
                .get(id)
                .cloned()
                .ok_or_else(|| CreditedSessionManagerError::SessionNotFound(id.clone()))?
        };
        let parsed: EventId = last_event_id
            .parse()
            .map_err(|e: EventIdParseError| CreditedSessionManagerError::InvalidEventId(e))?;
        let (_http_request_id, receiver) = handle.resume(parsed).await?;
        Self::install_sse_receiver(receiver);
        Ok(futures_util::stream::empty::<ServerSseMessage>())
    }

    async fn restore_session(
        &self,
        id: SessionId,
    ) -> Result<RestoreOutcome<Self::Transport>, Self::Error> {
        let mut sessions = self.sessions.write().await;
        if sessions.contains_key(&id) {
            return Ok(RestoreOutcome::AlreadyPresent);
        }
        let session_ct = self.shutdown.child_token();
        let (handle, worker) = create_credited_session(
            id.clone(),
            self.session_config.clone(),
            self.max_timeout_secs,
            session_ct.clone(),
        );
        sessions.insert(id, handle);
        Ok(RestoreOutcome::Restored(WorkerTransport::spawn_with_ct(
            worker, session_ct,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_common_stream_replays_queued_notifications_without_last_event_id() {
        let mut cfg = SessionConfig::default();
        cfg.sse_retry = Some(Duration::from_secs(3));
        let (_handle, mut worker) =
            create_credited_session("s1".into(), cfg, 30.0, CancellationToken::new());
        // Emit a common notification before the first standalone GET opens.
        let notif = ServerJsonRpcMessage::notification(
            ServerNotification::ToolListChangedNotification(Default::default()),
        );
        worker.handle_server_message(notif).unwrap();

        let queue = worker.establish_common_channel().unwrap().into_queue();
        let mut events = Vec::new();
        {
            let mut q = queue.inner.lock();
            while let Some(ev) = q.items.pop_front() {
                events.push(String::from_utf8_lossy(&ev.bytes).into_owned());
            }
        }
        assert_eq!(
            events.len(),
            2,
            "expected priming + queued notification: {events:?}"
        );
        assert!(events[0].contains("id: 0"), "{events:?}");
        assert!(
            events[1].contains("notifications/tools/list_changed"),
            "{events:?}"
        );
    }

    #[test]
    fn full_sse_queue_closes_without_blocking_and_teardown_discards_buffered_frames() {
        let queue = SharedSseQueue::new(2);
        let mk = |idx: usize| CreditedSseEvent {
            index: idx,
            bytes: Bytes::from_static(b"data: {}\n\n"),
            terminal: false,
        };
        assert!(queue.try_push(mk(0)));
        assert!(queue.try_push(mk(1)));
        // Third push overflows capacity: closes & discards immediately instead of blocking.
        assert!(!queue.try_push(mk(2)));
        assert!(queue.is_closed());
        assert!(queue.inner.lock().items.is_empty());

        let queue2 = SharedSseQueue::new(4);
        assert!(queue2.try_push(mk(0)));
        assert_eq!(queue2.inner.lock().items.len(), 1);
        queue2.close_and_discard();
        assert!(queue2.is_closed());
        assert!(queue2.inner.lock().items.is_empty());
    }

    fn drain_queue_strings(queue: &SharedSseQueue) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(ev) = queue.pop_one() {
            out.push(String::from_utf8_lossy(&ev.bytes).into_owned());
        }
        out
    }

    #[test]
    fn stale_last_event_id_does_not_poison_common_or_disrupt_inflight_request() {
        let mut cfg = SessionConfig::default();
        cfg.channel_capacity = 2; // replay ring capacity = 3
        cfg.sse_retry = Some(Duration::from_secs(3));
        let (_handle, mut worker) =
            create_credited_session("s2".into(), cfg, 30.0, CancellationToken::new());

        // Push 4 common notifications (indices 1..=4); ring keeps [2, 3, 4], so index 0 is stale.
        for _ in 0..4 {
            let notif = ServerJsonRpcMessage::notification(
                ServerNotification::ToolListChangedNotification(Default::default()),
            );
            worker.handle_server_message(notif).unwrap();
        }
        assert!(worker.common.tx.is_closed());

        // Stale Last-Event-ID "0" must fail with InvalidEventId and leave `common.tx` CLOSED.
        let err = worker.resume("0".parse().unwrap()).err().unwrap();
        assert!(matches!(err, CreditedSessionError::InvalidEventId));
        assert!(
            worker.common.tx.is_closed(),
            "failed common resume must not install an open primary queue"
        );

        // Next standalone GET without Last-Event-ID must become primary (replaying [2, 3, 4]),
        // NOT a silent shadow stream!
        let primary = worker.establish_common_channel().unwrap();
        assert!(!worker.common.tx.is_closed());
        let replayed = drain_queue_strings(primary.queue.as_ref().unwrap());
        assert_eq!(
            replayed.len(),
            4,
            "expected priming + 3 retained notifications: {replayed:?}"
        );
        // Dropping `CreditedSseReceiver` without transferring to `CreditedSseBody` closes primary.
        drop(primary);
        assert!(
            worker.common.tx.is_closed(),
            "dropping abandoned CreditedSseReceiver must close primary queue"
        );

        // Now test an IN-FLIGHT request-wise stream: stale Last-Event-ID must NOT disrupt
        // the live stream, which still receives the eventual terminal response.
        let (http_id, live_rx) = worker.alloc_request_wise_channel();
        let req_id = RequestId::Number(42);
        let req = JsonRpcRequest {
            jsonrpc: Default::default(),
            id: req_id.clone(),
            request: ClientRequest::PingRequest(Default::default()),
        };
        worker.register_request(&req, Some(http_id));
        // Drain priming (index 0) and push 4 intermediate progress/cancel notifications so
        // index 0 rolls off the ring while the request is STILL in-flight.
        let _ = drain_queue_strings(live_rx.queue.as_ref().unwrap());
        for i in 1..=4usize {
            let ch = worker.tx_router.get_mut(&http_id).unwrap();
            let (idx, eid) = ch.channel.alloc_index();
            assert_eq!(idx, i);
            let ev = CreditedSseEvent {
                index: idx,
                bytes: Bytes::from(format!("data: {{\"n\":{i}}}\nid: {eid}\n\n")),
                terminal: false,
            };
            assert!(ch.channel.send_event(ev));
            // Drain live queue so it stays open and active.
            let _ = drain_queue_strings(live_rx.queue.as_ref().unwrap());
        }
        assert!(
            !worker
                .tx_router
                .get(&http_id)
                .unwrap()
                .channel
                .tx
                .is_closed()
        );

        // Stale resume at "0/<http_id>" must fail with InvalidEventId and leave `live_rx` open.
        let err = worker
            .resume(format!("0/{http_id}").parse().unwrap())
            .err()
            .unwrap();
        assert!(matches!(err, CreditedSessionError::InvalidEventId));
        assert!(
            !live_rx.queue.as_ref().unwrap().is_closed(),
            "failed request resume must not close active live stream"
        );

        // Delivering the terminal response still reaches `live_rx`.
        let resp = ServerJsonRpcMessage::response(rmcp::model::ServerResult::empty(()), req_id);
        worker.handle_server_message(resp).unwrap();
        let delivered = drain_queue_strings(live_rx.queue.as_ref().unwrap());
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert!(delivered[0].contains("\"id\":42"), "{delivered:?}");
    }

    #[test]
    fn sse_overflow_preserves_replay_and_recovers_terminal_response() {
        let mut cfg = SessionConfig::default();
        cfg.channel_capacity = 2; // live queue capacity = 2, replay ring capacity = 3
        cfg.sse_retry = Some(Duration::from_secs(3));
        let (_handle, mut worker) =
            create_credited_session("s3".into(), cfg, 30.0, CancellationToken::new());

        // Case 1: Exact overflow on the `(C + 1)`-th unread event (index 3 = terminal).
        let (http_id, live_rx) = worker.alloc_request_wise_channel();
        let req_id = RequestId::Number(7);
        let req = JsonRpcRequest {
            jsonrpc: Default::default(),
            id: req_id.clone(),
            request: ClientRequest::PingRequest(Default::default()),
        };
        worker.register_request(&req, Some(http_id));
        // Client reads priming (index 0), then stalls.
        let initial = drain_queue_strings(live_rx.queue.as_ref().unwrap());
        assert_eq!(initial.len(), 1);
        for i in 1..=2usize {
            let ch = worker.tx_router.get_mut(&http_id).unwrap();
            let (idx, eid) = ch.channel.alloc_index();
            assert_eq!(idx, i);
            assert!(ch.channel.send_event(CreditedSseEvent {
                index: idx,
                bytes: Bytes::from(format!("data: {{\"step\":{i}}}\nid: {eid}\n\n")),
                terminal: false,
            }));
        }
        // Terminal response (index 3) overflows the stalled 2-slot live queue and closes it.
        worker
            .handle_server_message(ServerJsonRpcMessage::response(
                rmcp::model::ServerResult::empty(()),
                req_id,
            ))
            .unwrap();
        assert!(live_rx.queue.as_ref().unwrap().is_closed());
        drop(live_rx);

        // Reconnect from "0/<http_id>": replay ring kept [1, 2, 3], zero gap!
        let (_, resumed) = worker
            .resume(format!("0/{http_id}").parse().unwrap())
            .expect("exact overflow must remain replayable from last read event");
        let events = drain_queue_strings(resumed.queue.as_ref().unwrap());
        assert_eq!(
            events.len(),
            3,
            "expected events 1, 2, and terminal 3: {events:?}"
        );
        assert!(events[2].contains("\"id\":7"), "{events:?}");
        drop(resumed);

        // Case 2: Heavy overflow (> C + 1 intermediate events roll off before terminal).
        let (http_id2, live_rx2) = worker.alloc_request_wise_channel();
        let req_id2 = RequestId::Number(8);
        let req2 = JsonRpcRequest {
            jsonrpc: Default::default(),
            id: req_id2.clone(),
            request: ClientRequest::PingRequest(Default::default()),
        };
        worker.register_request(&req2, Some(http_id2));
        let _ = drain_queue_strings(live_rx2.queue.as_ref().unwrap());
        for i in 1..=6usize {
            let ch = worker.tx_router.get_mut(&http_id2).unwrap();
            let (idx, eid) = ch.channel.alloc_index();
            let _ = ch.channel.send_event(CreditedSseEvent {
                index: idx,
                bytes: Bytes::from(format!("data: {{\"step\":{i}}}\nid: {eid}\n\n")),
                terminal: false,
            });
        }
        worker
            .handle_server_message(ServerJsonRpcMessage::response(
                rmcp::model::ServerResult::empty(()),
                req_id2,
            ))
            .unwrap();
        drop(live_rx2);

        // Even after intermediate notifications rolled off, resuming the completed request
        // from "0/<http_id2>" still delivers the retained terminal response!
        let (_, resumed2) = worker
            .resume(format!("0/{http_id2}").parse().unwrap())
            .expect("completed request terminal response must remain recoverable after gap");
        let events2 = drain_queue_strings(resumed2.queue.as_ref().unwrap());
        assert!(
            events2.iter().any(|e| e.contains("\"id\":8")),
            "expected terminal response for id 8 in {events2:?}"
        );
        // Bogus future index is still rejected with InvalidEventId.
        let err = worker
            .resume(format!("999/{http_id2}").parse().unwrap())
            .err()
            .unwrap();
        assert!(matches!(err, CreditedSessionError::InvalidEventId));
    }

    #[test]
    fn duplicate_inflight_request_stream_returns_32600_without_breaking_original() {
        let mut cfg = SessionConfig::default();
        cfg.sse_retry = Some(Duration::from_secs(3));
        let (_handle, mut worker) =
            create_credited_session("s4".into(), cfg, 30.0, CancellationToken::new());

        let req_id = RequestId::Number(99);
        let (orig_http_id, orig_rx) = worker.alloc_request_wise_channel();
        let req = JsonRpcRequest {
            jsonrpc: Default::default(),
            id: req_id.clone(),
            request: ClientRequest::PingRequest(Default::default()),
        };
        worker.register_request(&req, Some(orig_http_id));
        assert!(worker.is_in_flight(&req_id));

        // Duplicate in-flight RequestId gets an immediate -32600 error stream.
        let (_dup_http_id, dup_rx) = worker.reject_duplicate_request_stream(req_id.clone());
        let dup_events = drain_queue_strings(dup_rx.queue.as_ref().unwrap());
        assert!(
            dup_events
                .iter()
                .any(|e| e.contains("-32600") && e.contains("duplicate in-flight request id")),
            "expected -32600 duplicate rejection: {dup_events:?}"
        );

        // Original request channel stays intact and still receives its response.
        worker
            .handle_server_message(ServerJsonRpcMessage::response(
                rmcp::model::ServerResult::empty(()),
                req_id,
            ))
            .unwrap();
        let orig_events = drain_queue_strings(orig_rx.queue.as_ref().unwrap());
        assert!(
            orig_events.iter().any(|e| e.contains("\"id\":99")),
            "expected original response for id 99: {orig_events:?}"
        );
    }

    #[test]
    fn replacement_sse_queues_stay_aligned_with_replay_ring_on_overflow() {
        let mut cfg = SessionConfig::default();
        cfg.channel_capacity = 2; // live queue capacity = 2, replay ring capacity = 3
        cfg.sse_retry = Some(Duration::from_secs(3));
        let (_handle, mut worker) =
            create_credited_session("s5".into(), cfg, 30.0, CancellationToken::new());

        // Case 1: Common stream replacement after a full ring [1, 2, 3].
        let notif = || {
            ServerJsonRpcMessage::notification(ServerNotification::ToolListChangedNotification(
                Default::default(),
            ))
        };
        for _ in 0..3 {
            worker.handle_server_message(notif()).unwrap();
        }
        let primary = worker.establish_common_channel().unwrap();
        let initial = drain_queue_strings(primary.queue.as_ref().unwrap());
        assert_eq!(
            initial.len(),
            4,
            "expected priming + backlog [1, 2, 3]: {initial:?}"
        );
        // Client read through index 3 and now stalls. Indices 4 & 5 fill the 2-slot live
        // queue; index 6 overflows and closes it while ring retains [4, 5, 6].
        worker.handle_server_message(notif()).unwrap();
        assert!(!primary.queue.as_ref().unwrap().is_closed());
        worker.handle_server_message(notif()).unwrap();
        assert!(!primary.queue.as_ref().unwrap().is_closed());
        worker.handle_server_message(notif()).unwrap();
        assert!(
            primary.queue.as_ref().unwrap().is_closed(),
            "replacement common queue must overflow at channel_capacity + 1 unread events"
        );
        drop(primary);

        let (_, resumed) = worker
            .resume("3".parse().unwrap())
            .expect("common overflow after replacement must stay replayable from index 3");
        let replayed = drain_queue_strings(resumed.queue.as_ref().unwrap());
        assert_eq!(replayed.len(), 3, "expected [4, 5, 6]: {replayed:?}");
        assert!(replayed[0].contains("id: 4\n"), "{replayed:?}");
        assert!(replayed[2].contains("id: 6\n"), "{replayed:?}");

        // Second overflow after a `resume("3")` replacement must also remain gap-free.
        worker.handle_server_message(notif()).unwrap();
        worker.handle_server_message(notif()).unwrap();
        assert!(!resumed.queue.as_ref().unwrap().is_closed());
        worker.handle_server_message(notif()).unwrap();
        assert!(resumed.queue.as_ref().unwrap().is_closed());
        drop(resumed);

        let (_, resumed2) = worker
            .resume("6".parse().unwrap())
            .expect("common overflow after resume must stay replayable from index 6");
        let replayed2 = drain_queue_strings(resumed2.queue.as_ref().unwrap());
        assert_eq!(replayed2.len(), 3, "expected [7, 8, 9]: {replayed2:?}");
        assert!(replayed2[0].contains("id: 7\n"), "{replayed2:?}");
        assert!(replayed2[2].contains("id: 9\n"), "{replayed2:?}");
        drop(resumed2);

        // Case 2: In-flight request stream overflow after a successful resume.
        let (http_id, live_rx) = worker.alloc_request_wise_channel();
        let req_id = RequestId::Number(17);
        let req = JsonRpcRequest {
            jsonrpc: Default::default(),
            id: req_id.clone(),
            request: ClientRequest::PingRequest(Default::default()),
        };
        worker.register_request(&req, Some(http_id));
        let _ = drain_queue_strings(live_rx.queue.as_ref().unwrap()); // priming index 0
        for step in 1..=3usize {
            let ch = worker.tx_router.get_mut(&http_id).unwrap();
            let (idx, eid) = ch.channel.alloc_index();
            assert_eq!(idx, step);
            let pushed = ch.channel.send_event(CreditedSseEvent {
                index: idx,
                bytes: Bytes::from(format!("data: {{\"step\":{step}}}\nid: {eid}\n\n")),
                terminal: false,
            });
            assert_eq!(pushed, step < 3);
        }
        assert!(live_rx.queue.as_ref().unwrap().is_closed());
        drop(live_rx);

        let (_, req_resumed) = worker
            .resume(format!("0/{http_id}").parse().unwrap())
            .expect("in-flight request resume from 0 must succeed");
        let got = drain_queue_strings(req_resumed.queue.as_ref().unwrap());
        assert_eq!(got.len(), 3, "expected [1, 2, 3]: {got:?}");
        assert!(got[2].contains(&format!("id: 3/{http_id}\n")), "{got:?}");

        // Client stalls at index 3 while request stays in-flight; 4 & 5 queue, 6 overflows.
        for step in 4..=6usize {
            let ch = worker.tx_router.get_mut(&http_id).unwrap();
            let (idx, eid) = ch.channel.alloc_index();
            assert_eq!(idx, step);
            let pushed = ch.channel.send_event(CreditedSseEvent {
                index: idx,
                bytes: Bytes::from(format!("data: {{\"step\":{step}}}\nid: {eid}\n\n")),
                terminal: false,
            });
            assert_eq!(
                pushed,
                step < 6,
                "resumed in-flight request queue must overflow on step 6"
            );
        }
        assert!(req_resumed.queue.as_ref().unwrap().is_closed());
        drop(req_resumed);

        let (_, req_resumed2) = worker
            .resume(format!("3/{http_id}").parse().unwrap())
            .expect("in-flight request overflow after resume must stay replayable from index 3");
        let got2 = drain_queue_strings(req_resumed2.queue.as_ref().unwrap());
        assert_eq!(got2.len(), 3, "expected [4, 5, 6]: {got2:?}");
        assert!(got2[0].contains(&format!("id: 4/{http_id}\n")), "{got2:?}");
        assert!(got2[2].contains(&format!("id: 6/{http_id}\n")), "{got2:?}");
        drop(req_resumed2);
    }

    #[test]
    fn replacement_sse_preserves_replay_when_live_events_arrive_before_or_mid_read() {
        let mut cfg = SessionConfig::default();
        cfg.channel_capacity = 2; // live queue capacity = 2, replay ring capacity = 3
        cfg.sse_retry = Some(Duration::from_secs(3));
        let (_handle, mut worker) =
            create_credited_session("s6".into(), cfg, 30.0, CancellationToken::new());

        let notif = || {
            ServerJsonRpcMessage::notification(ServerNotification::ToolListChangedNotification(
                Default::default(),
            ))
        };

        // --- Case 1a: Common stream, live event arrives BEFORE the first replay read ---
        // Fill ring with [4, 5, 6] by pushing 6 notifications while primary is closed.
        for _ in 0..6 {
            worker.handle_server_message(notif()).unwrap();
        }
        let (_, resumed_before_read) = worker
            .resume("3".parse().unwrap())
            .expect("resume from 3 with ring [4, 5, 6] must succeed");
        let q = resumed_before_read.queue.as_ref().unwrap();
        // Live event 7 arrives before the body is first polled (ring evicts 4 -> [5, 6, 7]):
        // must NOT discard [4, 5, 6] or prematurely close the replacement!
        worker.handle_server_message(notif()).unwrap();
        assert!(
            !q.is_closed(),
            "first live event before replay read must be admitted without closing replacement"
        );
        // Live event 8 (ring [6, 7, 8]): still fits within live capacity 2.
        worker.handle_server_message(notif()).unwrap();
        assert!(!q.is_closed());
        // Live event 9 (ring [7, 8, 9]): overflows live capacity 2 -> closes queue, discards
        // live [7, 8] + 9, and KEEPS unread replay prefix [4, 5, 6] deliverable before EOF!
        worker.handle_server_message(notif()).unwrap();
        assert!(q.is_closed());
        let delivered = drain_queue_strings(q);
        assert_eq!(
            delivered.len(),
            3,
            "unread replay prefix [4, 5, 6] must remain deliverable on live overflow: {delivered:?}"
        );
        assert!(delivered[0].contains("id: 4\n"), "{delivered:?}");
        assert!(delivered[2].contains("id: 6\n"), "{delivered:?}");
        drop(resumed_before_read);

        // Client advanced to 6; reconnecting from 6 replays [7, 8, 9] with zero gap!
        let (_, resumed_mid_read) = worker
            .resume("6".parse().unwrap())
            .expect("reconnect from 6 after replay-prefix delivery must succeed");
        let q_mid = resumed_mid_read.queue.as_ref().unwrap();

        // --- Case 1b: Common stream, live traffic arrives MIDWAY through replay backlog ---
        // Read only event 7 (leaving [8, 9] unread in the replay prefix).
        let ev7 = String::from_utf8_lossy(&q_mid.pop_one().unwrap().bytes).into_owned();
        assert!(ev7.contains("id: 7\n"), "{ev7:?}");
        // Emit live 10 and 11 (admitted), then 12 (overflows live capacity 2).
        worker.handle_server_message(notif()).unwrap(); // 10
        assert!(!q_mid.is_closed());
        worker.handle_server_message(notif()).unwrap(); // 11
        assert!(!q_mid.is_closed());
        worker.handle_server_message(notif()).unwrap(); // 12 -> overflow
        assert!(q_mid.is_closed());
        // Remaining replay prefix [8, 9] is still delivered cleanly before EOF.
        let rest = drain_queue_strings(q_mid);
        assert_eq!(rest.len(), 2, "expected remaining replay [8, 9]: {rest:?}");
        assert!(rest[0].contains("id: 8\n"), "{rest:?}");
        assert!(rest[1].contains("id: 9\n"), "{rest:?}");
        drop(resumed_mid_read);

        // Reconnect from 9 -> replays [10, 11, 12] gap-free!
        // Also verify non-overflowing live arrival before first read delivers [10, 11, 12, 13].
        let (_, resumed_seamless) = worker
            .resume("9".parse().unwrap())
            .expect("reconnect from 9 must succeed");
        worker.handle_server_message(notif()).unwrap(); // 13 (live, non-overflowing)
        assert!(!resumed_seamless.queue.as_ref().unwrap().is_closed());
        let seamless = drain_queue_strings(resumed_seamless.queue.as_ref().unwrap());
        assert_eq!(seamless.len(), 4, "expected [10, 11, 12, 13]: {seamless:?}");
        assert!(seamless[0].contains("id: 10\n"), "{seamless:?}");
        assert!(seamless[3].contains("id: 13\n"), "{seamless:?}");
        drop(resumed_seamless);

        // --- Case 2a: In-flight request stream, live traffic BEFORE first replay read ---
        let (http_id, live_rx) = worker.alloc_request_wise_channel();
        let req_id = RequestId::Number(88);
        let req = JsonRpcRequest {
            jsonrpc: Default::default(),
            id: req_id.clone(),
            request: ClientRequest::PingRequest(Default::default()),
        };
        worker.register_request(&req, Some(http_id));
        let _ = drain_queue_strings(live_rx.queue.as_ref().unwrap()); // priming 0
        for step in 1..=3usize {
            let ch = worker.tx_router.get_mut(&http_id).unwrap();
            let (idx, eid) = ch.channel.alloc_index();
            assert_eq!(idx, step);
            let pushed = ch.channel.send_event(CreditedSseEvent {
                index: idx,
                bytes: Bytes::from(format!("data: {{\"step\":{step}}}\nid: {eid}\n\n")),
                terminal: false,
            });
            assert_eq!(pushed, step < 3);
        }
        drop(live_rx);

        let (_, req_before_read) = worker
            .resume(format!("0/{http_id}").parse().unwrap())
            .expect("in-flight resume from 0 with [1, 2, 3] must succeed");
        let rq = req_before_read.queue.as_ref().unwrap();
        // Before first replay read, emit live 4, 5 (admitted), then 6 (overflows live cap 2).
        for step in 4..=6usize {
            let ch = worker.tx_router.get_mut(&http_id).unwrap();
            let (idx, eid) = ch.channel.alloc_index();
            assert_eq!(idx, step);
            let pushed = ch.channel.send_event(CreditedSseEvent {
                index: idx,
                bytes: Bytes::from(format!("data: {{\"step\":{step}}}\nid: {eid}\n\n")),
                terminal: false,
            });
            assert_eq!(pushed, step < 6);
        }
        assert!(rq.is_closed());
        let req_delivered = drain_queue_strings(rq);
        assert_eq!(
            req_delivered.len(),
            3,
            "in-flight unread replay [1, 2, 3] must survive live overflow: {req_delivered:?}"
        );
        assert!(
            req_delivered[0].contains(&format!("id: 1/{http_id}\n")),
            "{req_delivered:?}"
        );
        assert!(
            req_delivered[2].contains(&format!("id: 3/{http_id}\n")),
            "{req_delivered:?}"
        );
        drop(req_before_read);

        // --- Case 2b: In-flight request stream, live traffic MIDWAY through replay ---
        let (_, req_mid_read) = worker
            .resume(format!("3/{http_id}").parse().unwrap())
            .expect("in-flight reconnect from 3 must succeed");
        let rq_mid = req_mid_read.queue.as_ref().unwrap();
        // Client reads event 4 (leaving replay [5, 6] unread).
        let ev4 = String::from_utf8_lossy(&rq_mid.pop_one().unwrap().bytes).into_owned();
        assert!(ev4.contains(&format!("id: 4/{http_id}\n")), "{ev4:?}");
        // Emit live 7, 8 (admitted), then terminal response 9 (overflows live capacity 2).
        for step in 7..=8usize {
            let ch = worker.tx_router.get_mut(&http_id).unwrap();
            let (idx, eid) = ch.channel.alloc_index();
            assert_eq!(idx, step);
            assert!(ch.channel.send_event(CreditedSseEvent {
                index: idx,
                bytes: Bytes::from(format!("data: {{\"step\":{step}}}\nid: {eid}\n\n")),
                terminal: false,
            }));
        }
        assert!(!rq_mid.is_closed());
        worker
            .handle_server_message(ServerJsonRpcMessage::response(
                rmcp::model::ServerResult::empty(()),
                req_id,
            ))
            .unwrap();
        assert!(rq_mid.is_closed());
        let mid_rest = drain_queue_strings(rq_mid);
        assert_eq!(
            mid_rest.len(),
            2,
            "remaining replay [5, 6] must still deliver before EOF: {mid_rest:?}"
        );
        assert!(
            mid_rest[0].contains(&format!("id: 5/{http_id}\n")),
            "{mid_rest:?}"
        );
        assert!(
            mid_rest[1].contains(&format!("id: 6/{http_id}\n")),
            "{mid_rest:?}"
        );
        drop(req_mid_read);

        // Final reconnect from 6 recovers [7, 8, terminal 9] gap-free!
        let (_, req_final) = worker
            .resume(format!("6/{http_id}").parse().unwrap())
            .expect("final reconnect from 6 must recover [7, 8, terminal 9]");
        let final_events = drain_queue_strings(req_final.queue.as_ref().unwrap());
        assert_eq!(
            final_events.len(),
            3,
            "expected [7, 8, terminal 9]: {final_events:?}"
        );
        assert!(
            final_events[0].contains(&format!("id: 7/{http_id}\n")),
            "{final_events:?}"
        );
        assert!(final_events[2].contains("\"id\":88"), "{final_events:?}");
        drop(req_final);
    }

    #[tokio::test]
    async fn superseded_common_sse_discards_unread_replay_before_and_after_session_delete() {
        use http_body_util::BodyExt;

        let mut cfg = SessionConfig::default();
        cfg.channel_capacity = 2; // live queue capacity = 2, replay ring capacity = 3
        cfg.sse_retry = Some(Duration::from_secs(3));
        let shutdown = CancellationToken::new();
        let (_handle, mut worker) =
            create_credited_session("s7".into(), cfg.clone(), 30.0, shutdown.clone());

        let notif = || {
            ServerJsonRpcMessage::notification(ServerNotification::ToolListChangedNotification(
                Default::default(),
            ))
        };

        // Populate replay ring with [4, 5, 6] while initial primary is closed.
        for _ in 0..6 {
            worker.handle_server_message(notif()).unwrap();
        }
        // Open common stream #1 from 3 -> preloads replay prefix [4, 5, 6].
        let (_, rx1) = worker
            .resume("3".parse().unwrap())
            .expect("resume from 3 must preload [4, 5, 6]");
        let mut body1 = CreditedSseBody::new(rx1, None, shutdown.clone());

        // Trigger live overflow (7, 8 admitted; 9 overflows) while body1 stays unread:
        // body1's queue closes with its replay prefix [4, 5, 6] still buffered.
        for _ in 0..3 {
            worker.handle_server_message(notif()).unwrap();
        }
        assert!(worker.common.tx.is_closed());

        // An invalid `Last-Event-ID` replacement attempt must be rejected without
        // discarding body1's still-draining replay prefix.
        assert!(matches!(
            worker.resume("99".parse().unwrap()),
            Err(CreditedSessionError::InvalidEventId)
        ));

        // Now supersede the closed primary with a new standalone GET while body1 is still
        // alive and unread. The superseded queue must be discarded immediately so body1
        // yields EOF with zero frames.
        let rx2 = worker
            .establish_common_channel()
            .expect("replacement standalone GET must succeed");
        let mut body2 = CreditedSseBody::new(rx2, None, shutdown.clone());
        assert!(
            body1.frame().await.is_none(),
            "superseded common SSE body must discard unread replay prefix and return EOF"
        );

        // Delete/expire the session while body2 is still alive and unread (with priming +
        // [7, 8, 9] buffered). Both body1 and body2 must emit nothing after teardown.
        drop(worker);
        assert!(
            body1.frame().await.is_none(),
            "superseded body must stay closed after session deletion"
        );
        assert!(
            body2.frame().await.is_none(),
            "active replacement body must discard unread replay on session deletion"
        );

        // Also verify direct session deletion while an unreplaced overflowed common stream
        // is still holding its unread replay prefix discards that prefix immediately.
        let (_handle2, mut worker2) =
            create_credited_session("s8".into(), cfg, 30.0, shutdown.clone());
        for _ in 0..6 {
            worker2.handle_server_message(notif()).unwrap();
        }
        let (_, rx3) = worker2.resume("3".parse().unwrap()).unwrap();
        let mut body3 = CreditedSseBody::new(rx3, None, shutdown);
        for _ in 0..3 {
            worker2.handle_server_message(notif()).unwrap();
        }
        assert!(worker2.common.tx.is_closed());
        drop(worker2);
        assert!(
            body3.frame().await.is_none(),
            "overflowed common SSE body must not emit unread replay after session deletion"
        );
    }
}
