#![allow(dead_code)]
use std::{convert::Infallible, fmt::Display, sync::Arc, time::Duration};

use bytes::{Buf, Bytes};
use http::Response;
use http_body::Body;
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use sse_stream::Sse;
#[cfg(not(feature = "retained-output"))]
use sse_stream::{KeepAlive, SseBody};
use tokio_util::sync::{CancellationToken, DropGuard};

use super::http_header::EVENT_STREAM_MIME_TYPE;
use crate::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};

pub type SessionId = Arc<str>;

pub fn session_id() -> SessionId {
    uuid::Uuid::new_v4().to_string().into()
}

pub const DEFAULT_AUTO_PING_INTERVAL: Duration = Duration::from_secs(15);

pub(crate) type BoxResponse = Response<BoxBody<Bytes, Infallible>>;

pub(crate) fn accepted_response() -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(http::StatusCode::ACCEPTED)
        .body(Empty::new().boxed())
        .expect("valid response")
}
pin_project_lite::pin_project! {
    struct TokioTimer {
        #[pin]
        sleep: tokio::time::Sleep,
    }
}
impl Future for TokioTimer {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        this.sleep.poll(cx)
    }
}
impl sse_stream::Timer for TokioTimer {
    fn from_duration(duration: Duration) -> Self {
        Self {
            sleep: tokio::time::sleep(duration),
        }
    }

    fn reset(self: std::pin::Pin<&mut Self>, when: std::time::Instant) {
        let this = self.project();
        this.sleep.reset(tokio::time::Instant::from_std(when));
    }
}

pin_project_lite::pin_project! {
    struct CancelOnDropStream<S> {
        #[pin]
        inner: S,
        _drop_guard: DropGuard,
    }
}

impl<S: futures::Stream> futures::Stream for CancelOnDropStream<S> {
    type Item = S::Item;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().inner.poll_next(cx)
    }
}

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ServerSseMessage {
    /// The event ID for this message. When set, clients can use this ID
    /// with the `Last-Event-ID` header to resume the stream from this point.
    pub event_id: Option<String>,
    /// The JSON-RPC message content. Set to `None` for priming events.
    /// See [SEP-1699](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1699)
    pub message: Option<Arc<ServerJsonRpcMessage>>,
    /// The retry interval hint for clients. Clients should wait this duration
    /// before attempting to reconnect. This maps to the SSE `retry:` field.
    pub retry: Option<Duration>,
}

impl ServerSseMessage {
    /// Create a message carrying a JSON-RPC response/notification with an event ID.
    pub fn new(event_id: impl Into<String>, message: ServerJsonRpcMessage) -> Self {
        Self {
            event_id: Some(event_id.into()),
            message: Some(Arc::new(message)),
            retry: None,
        }
    }

    /// Wrap a JSON-RPC message without an event ID or retry hint.
    pub fn from_message(message: ServerJsonRpcMessage) -> Self {
        Self {
            event_id: None,
            message: Some(Arc::new(message)),
            retry: None,
        }
    }

    /// Create a priming event that tells the client to reconnect after `retry`
    /// if the connection drops.
    /// See [SEP-1699](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1699).
    pub fn priming(event_id: impl Into<String>, retry: Duration) -> Self {
        Self {
            event_id: Some(event_id.into()),
            message: None,
            retry: Some(retry),
        }
    }

    /// Create a retry hint without changing the client's last event ID.
    pub fn retry(retry: Duration) -> Self {
        Self {
            event_id: None,
            message: None,
            retry: Some(retry),
        }
    }
}

#[cfg(not(feature = "retained-output"))]
pub(crate) fn sse_stream_response(
    stream: impl futures::Stream<Item = ServerSseMessage> + Send + Sync + 'static,
    keep_alive: Option<Duration>,
    ct: CancellationToken,
) -> Response<BoxBody<Bytes, Infallible>> {
    use futures::StreamExt;
    let cancelled = ct.clone();
    let stream = stream
        .map(|message| {
            let mut sse = if let Some(ref msg) = message.message {
                let data = serde_json::to_string(msg.as_ref()).expect("valid message");
                Sse::default().data(data)
            } else {
                // Priming event: empty data per SEP-1699 (just "data:\n")
                Sse::default().data("")
            };

            sse.id = message.event_id;

            if let Some(retry) = message.retry {
                sse.retry = Some(retry.as_millis() as u64);
            }

            Result::<Sse, Infallible>::Ok(sse)
        })
        .take_until(async move { cancelled.cancelled().await });
    let stream = CancelOnDropStream {
        inner: stream,
        _drop_guard: ct.drop_guard(),
    };
    let stream = SseBody::new(stream);

    let stream = match keep_alive {
        Some(duration) => stream
            .with_keep_alive::<TokioTimer>(KeepAlive::new().interval(duration))
            .boxed(),
        None => stream.boxed(),
    };

    Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, EVENT_STREAM_MIME_TYPE)
        .header(http::header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(stream)
        .expect("valid response")
}

// Keep already-owned frames intact through the actual HTTP Body free point.
// In particular Sse::data(String) would allocate a second uncredited payload.
#[cfg(feature = "retained-output")]
pin_project_lite::pin_project! {
    struct RetainedSseBody<S> {
        #[pin]
        stream: S,
        #[pin]
        timer: Option<tokio::time::Sleep>,
        keep_alive: Option<Duration>,
        ct: CancellationToken,
        finished: bool,
    }
}
#[cfg(feature = "retained-output")]
impl<S: futures::Stream<Item = ServerSseMessage>> Body for RetainedSseBody<S> {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
        use std::task::Poll;
        let mut this = self.project();
        if *this.finished {
            return Poll::Ready(None);
        }
        match this.stream.as_mut().poll_next(cx) {
            Poll::Ready(None) => {
                *this.finished = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(message)) => {
                let retained = message.message.as_ref().and_then(|message_body| {
                    message_body.retained_frame(crate::model::RetainedFraming::Sse {
                        event_id: message.event_id.as_deref(),
                        retry: message.retry,
                    })
                });
                let frame = match retained {
                    Some(Ok(frame)) => frame,
                    Some(Err(error)) => {
                        // Headers may already be on the wire. End this owned
                        // stream and cancel it; never allocate an unpaid error
                        // or success frame after retained encoding refused.
                        tracing::error!(%error, "retained SSE encoding refused");
                        *this.finished = true;
                        this.ct.cancel();
                        return Poll::Ready(None);
                    }
                    None => {
                        let mut event = if let Some(ref body) = message.message {
                            match serde_json::to_string(body.as_ref()) {
                                Ok(data) => Sse::default().data(data),
                                Err(error) => {
                                    tracing::error!(%error, "SSE encoding refused");
                                    *this.finished = true;
                                    this.ct.cancel();
                                    return Poll::Ready(None);
                                }
                            }
                        } else {
                            Sse::default().data("")
                        };
                        event.id = message.event_id;
                        event.retry = message.retry.map(|retry| retry.as_millis() as u64);
                        Bytes::from(event)
                    }
                };
                if let (Some(timer), Some(interval)) =
                    (this.timer.as_mut().as_pin_mut(), *this.keep_alive)
                {
                    timer.reset(tokio::time::Instant::now() + interval);
                }
                Poll::Ready(Some(Ok(http_body::Frame::data(frame))))
            }
            Poll::Pending => {
                if let (Some(mut timer), Some(interval)) =
                    (this.timer.as_mut().as_pin_mut(), *this.keep_alive)
                {
                    if timer.as_mut().poll(cx).is_ready() {
                        timer.reset(tokio::time::Instant::now() + interval);
                        return Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from_static(
                            b":\n\n",
                        )))));
                    }
                }
                Poll::Pending
            }
        }
    }
}

#[cfg(feature = "retained-output")]
pub(crate) fn sse_stream_response(
    stream: impl futures::Stream<Item = ServerSseMessage> + Send + Sync + 'static,
    keep_alive: Option<Duration>,
    ct: CancellationToken,
) -> Response<BoxBody<Bytes, Infallible>> {
    use futures::StreamExt;
    let cancelled = ct.clone();
    let stream = CancelOnDropStream {
        inner: stream.take_until(async move { cancelled.cancelled().await }),
        _drop_guard: ct.clone().drop_guard(),
    };
    let body = RetainedSseBody {
        stream,
        timer: keep_alive.map(tokio::time::sleep),
        keep_alive,
        ct,
        finished: false,
    };
    Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, EVENT_STREAM_MIME_TYPE)
        .header(http::header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(body.boxed())
        .expect("valid response")
}

pub(crate) const fn internal_error_response<E: Display>(
    context: &str,
) -> impl FnOnce(E) -> Response<BoxBody<Bytes, Infallible>> {
    move |error| {
        tracing::error!("Internal server error when {context}: {error}");
        Response::builder()
            .status(http::StatusCode::INTERNAL_SERVER_ERROR)
            .body(
                Full::new(Bytes::from(format!(
                    "Encounter an error when {context}: {error}"
                )))
                .boxed(),
            )
            .expect("valid response")
    }
}

pub(crate) fn unexpected_message_response(expect: &str) -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(http::StatusCode::UNPROCESSABLE_ENTITY)
        .body(Full::new(Bytes::from(format!("Unexpected message, expect {expect}"))).boxed())
        .expect("valid response")
}

pub(crate) async fn expect_json<B>(
    body: B,
    max_bytes: usize,
) -> Result<ClientJsonRpcMessage, Response<BoxBody<Bytes, Infallible>>>
where
    B: Body + Send + 'static,
    B::Error: Display,
{
    let mut collected = bytes::BytesMut::new();
    let mut body = std::pin::pin!(body);
    loop {
        let frame = futures::future::poll_fn(|cx| body.as_mut().poll_frame(cx)).await;
        match frame {
            None => break,
            Some(Ok(frame)) => {
                let Ok(mut data) = frame.into_data() else {
                    continue;
                };
                if data.remaining() > max_bytes.saturating_sub(collected.len()) {
                    let response = Response::builder()
                        .status(http::StatusCode::PAYLOAD_TOO_LARGE)
                        .body(
                            Full::new(Bytes::from(format!(
                                "Payload Too Large: request body exceeds {max_bytes} bytes"
                            )))
                            .boxed(),
                        )
                        .expect("valid response");
                    return Err(response);
                }
                while data.has_remaining() {
                    let chunk = data.chunk();
                    let chunk_len = chunk.len();
                    collected.extend_from_slice(chunk);
                    data.advance(chunk_len);
                }
            }
            Some(Err(e)) => {
                let response = Response::builder()
                    .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                    .body(
                        Full::new(Bytes::from(format!("Failed to read request body: {e}"))).boxed(),
                    )
                    .expect("valid response");
                return Err(response);
            }
        }
    }

    match serde_json::from_slice::<ClientJsonRpcMessage>(&collected) {
        Ok(message) => Ok(message),
        Err(e) => {
            let response = Response::builder()
                .status(http::StatusCode::UNSUPPORTED_MEDIA_TYPE)
                .body(
                    Full::new(Bytes::from(format!("fail to deserialize request body {e}"))).boxed(),
                )
                .expect("valid response");
            Err(response)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use futures::stream;
    use http_body::Frame;
    use http_body_util::StreamBody;

    use super::*;
    use crate::model::{EmptyResult, JsonRpcResponse, JsonRpcVersion2_0, RequestId, ServerResult};

    const INITIALIZE_REQUEST: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}"#;

    fn dummy_message() -> ServerJsonRpcMessage {
        ServerJsonRpcMessage::Response(JsonRpcResponse {
            jsonrpc: JsonRpcVersion2_0,
            id: RequestId::Number(1),
            result: ServerResult::EmptyResult(EmptyResult {}),
        })
    }

    #[test]
    fn default_has_all_none() {
        let msg = ServerSseMessage::default();
        assert!(msg.event_id.is_none());
        assert!(msg.message.is_none());
        assert!(msg.retry.is_none());
    }

    #[test]
    fn new_sets_event_id_and_message() {
        let msg = ServerSseMessage::new("42", dummy_message());
        assert_eq!(msg.event_id.as_deref(), Some("42"));
        assert!(msg.message.is_some());
        assert!(msg.retry.is_none());
    }

    #[test]
    fn from_message_has_no_event_id() {
        let msg = ServerSseMessage::from_message(dummy_message());
        assert!(msg.event_id.is_none());
        assert!(msg.message.is_some());
        assert!(msg.retry.is_none());
    }

    #[test]
    fn priming_sets_event_id_and_retry() {
        let msg = ServerSseMessage::priming("0", Duration::from_secs(5));
        assert_eq!(msg.event_id.as_deref(), Some("0"));
        assert!(msg.message.is_none());
        assert_eq!(msg.retry, Some(Duration::from_secs(5)));
    }

    #[tokio::test]
    async fn expect_json_accepts_body_under_limit() {
        let body = Full::new(Bytes::from_static(INITIALIZE_REQUEST.as_bytes()));
        let result = expect_json(body, 4 * 1024 * 1024).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn expect_json_accepts_non_contiguous_body_data() {
        let split = INITIALIZE_REQUEST.len() / 2;
        let data = Bytes::copy_from_slice(&INITIALIZE_REQUEST.as_bytes()[..split]).chain(
            Bytes::copy_from_slice(&INITIALIZE_REQUEST.as_bytes()[split..]),
        );
        let result = expect_json(Full::new(data), 4 * 1024 * 1024).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn expect_json_accepts_multi_frame_body() {
        let split = INITIALIZE_REQUEST.len() / 2;
        let body = StreamBody::new(stream::iter([
            Ok::<_, Infallible>(Frame::data(Bytes::copy_from_slice(
                &INITIALIZE_REQUEST.as_bytes()[..split],
            ))),
            Ok(Frame::data(Bytes::copy_from_slice(
                &INITIALIZE_REQUEST.as_bytes()[split..],
            ))),
        ]));
        let result = expect_json(body, 4 * 1024 * 1024).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn expect_json_rejects_oversized_body() {
        let big_body = Full::new(Bytes::from(vec![b'x'; 128]));
        let result = expect_json(big_body, 64).await;
        let response = result.unwrap_err();
        assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn expect_json_rejects_oversized_multi_frame_body() {
        let body = StreamBody::new(stream::iter([
            Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"12345678"))),
            Ok(Frame::data(Bytes::from_static(b"9"))),
        ]));
        let response = expect_json(body, 8).await.unwrap_err();
        assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn expect_json_returns_415_for_invalid_json_under_limit() {
        let body = Full::new(Bytes::from("not valid json"));
        let result = expect_json(body, 4 * 1024 * 1024).await;
        let response = result.unwrap_err();
        assert_eq!(response.status(), http::StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}

#[cfg(all(test, feature = "retained-output"))]
mod retained_tests {
    use super::*;
    use crate::model::{RetainedFraming, RetainedToolBody, RetainedToolResult, ServerResult};
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Transport fixtures own their physical frames. Query/preparation accounting
    // is tested in core and in the product's complete retained response tests.
    struct FrameOwner {
        bytes: Vec<u8>,
        freed: Arc<AtomicUsize>,
    }
    impl AsRef<[u8]> for FrameOwner {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }
    impl Drop for FrameOwner {
        fn drop(&mut self) {
            self.freed.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct Fixture {
        freed: Arc<AtomicUsize>,
        calls: AtomicUsize,
        refuse: bool,
    }
    impl RetainedToolBody for Fixture {
        fn text(&self) -> &str {
            "real retained body"
        }
        fn encode_frame(
            &self,
            message: &ServerJsonRpcMessage,
            framing: RetainedFraming<'_>,
        ) -> std::io::Result<Bytes> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.refuse {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "actual frame refusal",
                ));
            }
            let json = serde_json::to_string(message).unwrap();
            let bytes = match framing {
                RetainedFraming::Json => json.into_bytes(),
                RetainedFraming::Sse { event_id, retry } => {
                    let mut event = Sse::default().data(json);
                    event.id = event_id.map(str::to_owned);
                    event.retry = retry.map(|retry| retry.as_millis() as u64);
                    Bytes::from(event).to_vec()
                }
            };
            Ok(Bytes::from_owner(FrameOwner {
                bytes,
                freed: self.freed.clone(),
            }))
        }
    }
    fn message(refuse: bool) -> (ServerJsonRpcMessage, Arc<Fixture>) {
        let body = Arc::new(Fixture {
            freed: Arc::default(),
            calls: AtomicUsize::new(0),
            refuse,
        });
        let result = RetainedToolResult::try_new(body.clone()).unwrap();
        let message =
            ServerJsonRpcMessage::response(ServerResult::RetainedToolResult(result), 7.into());
        (message, body)
    }

    #[tokio::test]
    async fn retained_sse_preserves_wire_and_last_frame_free_after_body_drop() {
        let (message, owner) = message(false);
        let mut expected = Sse::default().data(serde_json::to_string(&message).unwrap());
        expected.id = Some("4/2".into());
        expected.retry = Some(1500);
        let expected = Bytes::from(expected);
        let mut event = ServerSseMessage::new("4/2", message);
        event.retry = Some(Duration::from_millis(1500));
        let ct = CancellationToken::new();
        let mut body =
            sse_stream_response(futures::stream::iter([event]), None, ct.clone()).into_body();
        let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(frame, expected);
        assert_eq!(owner.calls.load(Ordering::SeqCst), 1);
        let last = frame.slice(1..);
        drop(frame);
        drop(body);
        assert!(ct.is_cancelled());
        assert_eq!(owner.freed.load(Ordering::SeqCst), 0);
        assert_eq!(last.as_ref(), &expected[1..]);
        drop(last);
        assert_eq!(owner.freed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retained_sse_refusal_closes_without_unpaid_fallback_or_later_frame() {
        let (message, owner) = message(true);
        let ct = CancellationToken::new();
        let events = [
            ServerSseMessage::from_message(message),
            ServerSseMessage::retry(Duration::from_millis(123)),
        ];
        let mut body =
            sse_stream_response(futures::stream::iter(events), None, ct.clone()).into_body();
        assert!(body.frame().await.is_none());
        assert!(body.frame().await.is_none());
        assert_eq!(owner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(owner.freed.load(Ordering::SeqCst), 0);
        assert!(ct.is_cancelled());
    }

    #[tokio::test]
    async fn retained_sse_priming_and_keepalive_preserve_legacy_bytes_and_cancel() {
        let ct = CancellationToken::new();
        let events =
            futures::stream::iter([ServerSseMessage::priming("0", Duration::from_millis(321))])
                .chain(futures::stream::pending());
        let mut body =
            sse_stream_response(events, Some(Duration::from_millis(1)), ct.clone()).into_body();
        assert_eq!(
            body.frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap()
                .as_ref(),
            b"data: \nid: 0\nretry: 321\n\n"
        );
        assert_eq!(
            body.frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap()
                .as_ref(),
            b":\n\n"
        );
        ct.cancel();
        assert!(body.frame().await.is_none());
    }
}
