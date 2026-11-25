//! Qualified Streamable HTTP session manager for unmodified `rmcp 3.5` (item 1305).
//!
//! Replaces `rmcp`'s stock `LocalSessionManager` so legacy HTTP sessions (`Mcp-Session-Id`)
//! encode and replay SSE response frames from [`StagedResponse`] under the originating
//! query's memory credit (`Bytes::from_owner(FrameOwner)`), keeping credit live across:
//!
//! 1. in-flight SSE channel buffering and slow client writes (`CreditedSseBody`), and
//! 2. `Last-Event-ID` session replay buffers when a client disconnects before receiving
//!    its response frame.
//!
//! Once the terminal response frame is delivered to the client (or the replay cache TTL
//! elapses, or the session closes via `DELETE` / idle timeout / server shutdown), the
//! cached `Bytes` drops and its `QueryPool` credit returns to `0`.

use super::retained::{Framing, ResponseSlot, StagedResponse};
use axum::body::Bytes;
use futures_util::Stream;
use http_body::{Body, Frame};
use parking_lot::Mutex;
use rmcp::RoleServer;
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
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
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

/// Hand-off payload installed into [`HTTP_SSE_SLOT`] by [`CreditedSessionManager`] so
/// `Endpoint::forward` replaces `rmcp`'s placeholder SSE body with a zero-copy
/// [`CreditedSseBody`].
pub(super) struct CreditedSseReceiver {
    rx: mpsc::Receiver<CreditedSseEvent>,
    /// Notifies the session worker when a request-wise SSE stream finishes or disconnects.
    finish: Option<(mpsc::Sender<SessionEvent>, HttpRequestId)>,
}

tokio::task_local! {
    /// Per-HTTP-request slot connecting `Endpoint::forward` to `CreditedSessionManager`
    /// without double-serializing large SSE payloads through `rmcp::sse_stream_response`.
    pub(super) static HTTP_SSE_SLOT: Arc<Mutex<Option<CreditedSseReceiver>>>;
}

/// An `http_body::Body` streaming credited SSE frames (`Bytes` backed by `FrameOwner`)
/// with optional keep-alive comments and server shutdown cancellation.
///
/// Tracks whether the terminal JSON-RPC response frame was actually yielded to hyper:
/// - If yielded, notifies the session worker so the replay entry can release immediately
///   once hyper finishes writing the body.
/// - If dropped before yielding (client disconnect), leaves the replay cache entry alive
///   for `completed_cache_ttl` so `GET /$/mcp` with `Last-Event-ID` can replay it.
pub(super) struct CreditedSseBody {
    rx: mpsc::Receiver<CreditedSseEvent>,
    finish: Option<(mpsc::Sender<SessionEvent>, HttpRequestId)>,
    delivered_terminal: bool,
    keep_alive: Option<Duration>,
    ping_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    shutdown: CancellationToken,
    shutdown_fut: Pin<Box<tokio_util::sync::WaitForCancellationFutureOwned>>,
    /// The last yielded frame's bytes are already owned by hyper once returned from
    /// `poll_frame`. Any buffered event still inside `rx` stays charged until polled or
    /// dropped here.
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
            rx: receiver.rx,
            finish: receiver.finish,
            delivered_terminal: false,
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
        if let Some((event_tx, http_request_id)) = self.finish.take()
            && self.delivered_terminal
            && let Err(mpsc::error::TrySendError::Full(evt)) =
                event_tx.try_send(SessionEvent::Delivered { http_request_id })
        {
            tokio::spawn(async move {
                let _ = event_tx.send(evt).await;
            });
        }
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
            return Poll::Ready(None);
        }
        match this.rx.poll_recv(cx) {
            Poll::Ready(Some(event)) => {
                if event.terminal {
                    this.delivered_terminal = true;
                }
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

struct CachedChannel {
    tx: mpsc::Sender<CreditedSseEvent>,
    cache: VecDeque<CreditedSseEvent>,
    http_request_id: Option<HttpRequestId>,
    capacity: usize,
    next_index: usize,
}

impl CachedChannel {
    fn new(
        tx: mpsc::Sender<CreditedSseEvent>,
        http_request_id: Option<HttpRequestId>,
        capacity: usize,
    ) -> Self {
        Self {
            cache: VecDeque::with_capacity(capacity.min(16)),
            capacity: capacity.max(1),
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

    async fn send_priming(&mut self, retry: Duration) {
        let (index, event_id) = self.alloc_index();
        let event = CreditedSseEvent::priming(index, &event_id, retry);
        self.cache_and_send(event).await;
    }

    async fn send_event(&mut self, event: CreditedSseEvent) -> bool {
        self.cache_and_send(event).await
    }

    async fn cache_and_send(&mut self, event: CreditedSseEvent) -> bool {
        if self.cache.len() >= self.capacity {
            self.cache.pop_front();
        }
        self.cache.push_back(event.clone());
        self.tx.send(event).await.is_ok()
    }

    /// Replay events strictly after `last_index` (matching SSE `Last-Event-ID` semantics).
    async fn replay_after(&mut self, last_index: usize) -> Result<bool, CreditedSessionError> {
        let Some(front) = self.cache.front() else {
            return Ok(false);
        };
        if last_index.saturating_add(1) < front.index {
            return Err(CreditedSessionError::InvalidEventId);
        }
        let mut replayed_terminal = false;
        for event in self.cache.iter().filter(|e| e.index > last_index) {
            if event.terminal {
                replayed_terminal = true;
            }
            if self.tx.send(event.clone()).await.is_err() {
                return Err(CreditedSessionError::ChannelClosed(self.http_request_id));
            }
        }
        Ok(replayed_terminal)
    }
}

struct RequestChannel {
    resources: HashSet<ResourceKey>,
    channel: CachedChannel,
    completed_at: Option<Instant>,
}

type RequestWiseChannelResult =
    Result<(HttpRequestId, mpsc::Receiver<CreditedSseEvent>), CreditedSessionError>;
type ResumeChannelResult =
    Result<(Option<HttpRequestId>, mpsc::Receiver<CreditedSseEvent>), CreditedSessionError>;

enum SessionEvent {
    ClientMessage {
        message: ClientJsonRpcMessage,
        http_request_id: Option<HttpRequestId>,
    },
    EstablishRequestWiseChannel {
        responder: oneshot::Sender<RequestWiseChannelResult>,
    },
    EstablishCommonChannel {
        responder: oneshot::Sender<Result<mpsc::Receiver<CreditedSseEvent>, CreditedSessionError>>,
    },
    Resume {
        last_event_id: EventId,
        responder: oneshot::Sender<ResumeChannelResult>,
    },
    InitializeRequest {
        request: ClientJsonRpcMessage,
        responder: oneshot::Sender<Result<ServerJsonRpcMessage, CreditedSessionError>>,
    },
    Delivered {
        http_request_id: HttpRequestId,
    },
    Close,
}

#[derive(Clone)]
pub(super) struct CreditedSessionHandle {
    event_tx: mpsc::Sender<SessionEvent>,
}

impl CreditedSessionHandle {
    async fn close(&self) -> Result<(), CreditedSessionError> {
        self.event_tx
            .send(SessionEvent::Close)
            .await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)
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

    async fn establish_request_wise_channel(
        &self,
    ) -> Result<(HttpRequestId, mpsc::Receiver<CreditedSseEvent>), CreditedSessionError> {
        let (tx, rx) = oneshot::channel();
        self.event_tx
            .send(SessionEvent::EstablishRequestWiseChannel { responder: tx })
            .await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?;
        rx.await
            .map_err(|_| CreditedSessionError::SessionServiceTerminated)?
    }

    async fn establish_common_channel(
        &self,
    ) -> Result<mpsc::Receiver<CreditedSseEvent>, CreditedSessionError> {
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
    ) -> Result<(Option<HttpRequestId>, mpsc::Receiver<CreditedSseEvent>), CreditedSessionError>
    {
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

pub(super) struct CreditedSessionWorker {
    id: SessionId,
    next_http_request_id: HttpRequestId,
    tx_router: HashMap<HttpRequestId, RequestChannel>,
    resource_router: HashMap<ResourceKey, HttpRequestId>,
    /// Per-request staged response slots extracted from inbound HTTP `Parts`.
    pending_slots: HashMap<RequestId, ResponseSlot>,
    common: CachedChannel,
    shadow_txs: Vec<mpsc::Sender<CreditedSseEvent>>,
    event_rx: mpsc::Receiver<SessionEvent>,
    config: SessionConfig,
    max_timeout_secs: f64,
}

impl Drop for CreditedSessionWorker {
    fn drop(&mut self) {
        for (_, slot) in self.pending_slots.drain() {
            slot.close();
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
        self.tx_router
            .retain(|_, rw| rw.completed_at.is_none_or(|at| at.elapsed() < ttl));
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
            && let Some(old) = self.pending_slots.insert(request.id.clone(), slot)
        {
            old.close();
        }
        if let Some(http_request_id) = http_request_id
            && let Some(channel) = self.tx_router.get_mut(&http_request_id)
        {
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
            if let Some(http_request_id) = self.resource_router.remove(&key)
                && let Some(channel) = self.tx_router.get_mut(&http_request_id)
            {
                let resources: Vec<_> = channel.resources.drain().collect();
                for r in resources {
                    self.resource_router.remove(&r);
                }
                channel.completed_at = Some(Instant::now());
                let (closed_tx, _) = mpsc::channel(1);
                channel.channel.tx = closed_tx;
            }
        }
    }

    async fn establish_request_wise_channel(
        &mut self,
    ) -> Result<(HttpRequestId, mpsc::Receiver<CreditedSseEvent>), CreditedSessionError> {
        let http_request_id = self.next_http_request_id;
        self.next_http_request_id = self.next_http_request_id.wrapping_add(1);
        let (tx, rx) = mpsc::channel(self.config.channel_capacity);
        let mut channel =
            CachedChannel::new(tx, Some(http_request_id), self.config.channel_capacity);
        if let Some(retry) = self.config.sse_retry {
            channel.send_priming(retry).await;
        }
        self.tx_router.insert(
            http_request_id,
            RequestChannel {
                resources: HashSet::new(),
                channel,
                completed_at: None,
            },
        );
        Ok((http_request_id, rx))
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

    async fn handle_server_message(
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
                let _delivered_to_channel = request_wise.channel.send_event(event).await;
                if close {
                    let resources: Vec<_> = request_wise.resources.drain().collect();
                    for r in resources {
                        self.resource_router.remove(&r);
                    }
                    request_wise.completed_at = Some(Instant::now());
                    // Close the sender so the client's SSE stream ends after reading
                    // the terminal event, while keeping the replay cache until
                    // delivery confirmation (`SessionEvent::Delivered`) or
                    // `completed_cache_ttl` expiry.
                    let (closed_tx, _) = mpsc::channel(1);
                    request_wise.channel.tx = closed_tx;
                }
            }
            OutboundChannel::Common => {
                let (index, event_id) = self.common.alloc_index();
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
                            terminal: false,
                        }
                    }
                    None => {
                        CreditedSseEvent::from_uncredited_message(index, &event_id, &message, false)
                    }
                };
                let _ = self.common.send_event(event).await;
            }
        }
        Ok(())
    }

    async fn resume(
        &mut self,
        last_event_id: EventId,
    ) -> Result<(Option<HttpRequestId>, mpsc::Receiver<CreditedSseEvent>), CreditedSessionError>
    {
        self.shadow_txs.retain(|tx| !tx.is_closed());
        let (index, req_id) = parse_event_id_parts(&last_event_id);
        match req_id {
            Some(http_request_id) => {
                let request_wise = self
                    .tx_router
                    .get_mut(&http_request_id)
                    .ok_or(CreditedSessionError::ChannelClosed(Some(http_request_id)))?;
                let is_completed = request_wise.completed_at.is_some();
                let (tx, rx) = mpsc::channel(self.config.channel_capacity);
                request_wise.channel.tx = tx;
                request_wise.channel.replay_after(index).await?;
                if is_completed {
                    let (closed_tx, _) = mpsc::channel(1);
                    request_wise.channel.tx = closed_tx;
                }
                Ok((Some(http_request_id), rx))
            }
            None => {
                let rx = self.resume_or_shadow_common(Some(index)).await?;
                Ok((None, rx))
            }
        }
    }

    async fn establish_common_channel(
        &mut self,
    ) -> Result<mpsc::Receiver<CreditedSseEvent>, CreditedSessionError> {
        self.resume_or_shadow_common(None).await
    }

    async fn resume_or_shadow_common(
        &mut self,
        last_event_index: Option<usize>,
    ) -> Result<mpsc::Receiver<CreditedSseEvent>, CreditedSessionError> {
        let replacing = self.common.tx.is_closed();
        let cap = if replacing {
            self.config.channel_capacity.max(2)
        } else {
            2
        };
        let (tx, rx) = mpsc::channel(cap);
        if last_event_index.is_none()
            && let Some(retry) = self.config.sse_retry
        {
            let _ = tx.send(CreditedSseEvent::priming(0, "0", retry)).await;
        }
        if replacing {
            self.common.tx = tx;
            if let Some(idx) = last_event_index {
                self.common.replay_after(idx).await?;
            }
        } else {
            const MAX_SHADOW_STREAMS: usize = 32;
            if self.shadow_txs.len() >= MAX_SHADOW_STREAMS {
                self.shadow_txs.remove(0);
            }
            self.shadow_txs.push(tx);
        }
        Ok(rx)
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
        let init_timeout = self.config.init_timeout.unwrap_or(Duration::MAX);
        let evt = tokio::select! {
            evt = self.event_rx.recv() => evt.ok_or_else(|| {
                WorkerQuitReason::fatal(
                    CreditedWorkerError::TransportTerminated,
                    "get initialize request",
                )
            })?,
            _ = context.cancellation_token.cancelled() => {
                return Err(WorkerQuitReason::Cancelled);
            }
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
        context.send_to_handler(request).await?;
        let init_resp = context.recv_from_handler().await?;
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

        let ct = context.cancellation_token.clone();
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
                _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
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
                        .await
                        .map_err(CreditedWorkerError::Session);
                    let _ = responder.send(res);
                }
                InnerEvent::FromHttpService(SessionEvent::ClientMessage {
                    message,
                    http_request_id,
                }) => {
                    match &message {
                        ClientJsonRpcMessage::Request(req) => {
                            self.register_request(req, http_request_id);
                        }
                        ClientJsonRpcMessage::Notification(n) => {
                            self.catch_cancellation(n);
                        }
                        _ => {}
                    }
                    context.send_to_handler(message).await?;
                }
                InnerEvent::FromHttpService(SessionEvent::EstablishRequestWiseChannel {
                    responder,
                }) => {
                    let res = self.establish_request_wise_channel().await;
                    let _ = responder.send(res);
                }
                InnerEvent::FromHttpService(SessionEvent::EstablishCommonChannel { responder }) => {
                    let res = self.establish_common_channel().await;
                    let _ = responder.send(res);
                }
                InnerEvent::FromHttpService(SessionEvent::Resume {
                    last_event_id,
                    responder,
                }) => {
                    let res = self.resume(last_event_id).await;
                    let _ = responder.send(res);
                }
                InnerEvent::FromHttpService(SessionEvent::Delivered { http_request_id }) => {
                    // The client received the terminal response frame on this request's
                    // SSE stream; evict its replay cache immediately so QueryPool credit
                    // returns to 0 promptly.
                    self.tx_router.remove(&http_request_id);
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
}

impl CreditedSessionManager {
    pub(super) fn new(session_config: SessionConfig, max_timeout_secs: f64) -> Self {
        Self {
            sessions: tokio::sync::RwLock::new(HashMap::new()),
            session_config,
            max_timeout_secs,
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
) -> (CreditedSessionHandle, CreditedSessionWorker) {
    let (event_tx, event_rx) = mpsc::channel(config.channel_capacity);
    let (common_tx, _) = mpsc::channel(config.channel_capacity);
    let mut common = CachedChannel::new(common_tx, None, config.channel_capacity);
    // Standalone stream priming uses event ID "0"; real notifications start at "1".
    common.next_index = 1;
    let handle = CreditedSessionHandle { event_tx };
    let worker = CreditedSessionWorker {
        id,
        next_http_request_id: 0,
        tx_router: HashMap::new(),
        resource_router: HashMap::new(),
        pending_slots: HashMap::new(),
        common,
        shadow_txs: Vec::new(),
        event_rx,
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
        let (handle, worker) = create_credited_session(
            id.clone(),
            self.session_config.clone(),
            self.max_timeout_secs,
        );
        self.sessions.write().await.insert(id.clone(), handle);
        Ok((id, WorkerTransport::spawn(worker)))
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
            match handle.close().await {
                Ok(()) | Err(CreditedSessionError::SessionServiceTerminated) => {}
                Err(e) => return Err(e.into()),
            }
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
        let (http_request_id, rx) = handle.establish_request_wise_channel().await?;
        handle.push_message(message, Some(http_request_id)).await?;
        Self::install_sse_receiver(CreditedSseReceiver {
            rx,
            finish: Some((handle.event_tx.clone(), http_request_id)),
        });
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
        let rx = handle.establish_common_channel().await?;
        Self::install_sse_receiver(CreditedSseReceiver { rx, finish: None });
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
        let (http_request_id, rx) = handle.resume(parsed).await?;
        Self::install_sse_receiver(CreditedSseReceiver {
            rx,
            finish: http_request_id.map(|rid| (handle.event_tx.clone(), rid)),
        });
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
        let (handle, worker) = create_credited_session(
            id.clone(),
            self.session_config.clone(),
            self.max_timeout_secs,
        );
        sessions.insert(id, handle);
        Ok(RestoreOutcome::Restored(WorkerTransport::spawn(worker)))
    }
}
