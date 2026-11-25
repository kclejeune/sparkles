//! Retained MCP tool/resource/task payloads and credited wire frames on unmodified
//! crates.io `rmcp` (item 1305).
//!
//! Each retained result keeps its rendered payload (`OwnedText` / `OwnedJson`) and
//! final wire frame (`Bytes` backed by `OwnedBytes`) charged against the originating
//! call's [`NativeBudget`] / [`Preparation`] / [`PoolGrant`] until the transport
//! finishes writing the frame or drops it on disconnect, cancellation, TTL expiry, or
//! error.

use super::Outcome;
use super::errors::{ERROR_META, ErrorContext, ToolError};
use super::render::CompactPage;
use axum::body::Bytes;
use parking_lot::Mutex;
use rmcp::model::{
    CacheScope, CallToolResult, ContentBlock, DetailedTask, GetTaskResult, Implementation,
    JsonObject, MetaObject, RequestId, ResultType, Task, TaskPayload,
};
use serde::Serialize;
use serde::ser::SerializeMap;
use serde_json::Value;
use serde_json::value::RawValue;
use sparkles::access::Preparation;
use sparkles::error::{Error, Result};
use sparkles::sparql::output::{NativeBudget, NativeReservation, OwnedBytes, OwnedJson, OwnedText};
use sparkles::sparql::pool::PoolGrant;
use sparkles::task::Control;
use std::alloc::Layout;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

fn implementation() -> Implementation {
    Implementation::new("sparkles", env!("CARGO_PKG_VERSION"))
}

struct FrameOwner {
    bytes: OwnedBytes,
    _credit: NativeReservation,
}

impl AsRef<[u8]> for FrameOwner {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

// `bytes::Bytes::from_owner` allocates this repr(C) refcount + owner header.
#[repr(C)]
struct BytesOwnerLayout<T> {
    references: AtomicUsize,
    owner: T,
}

fn overflow() -> Error {
    Error::invalid("native MCP owner layout overflow")
}

fn arc_bytes<T>() -> Result<u64> {
    let (layout, _) = Layout::new::<[AtomicUsize; 2]>()
        .extend(Layout::new::<T>())
        .map_err(|_| overflow())?;
    u64::try_from(layout.pad_to_align().size()).map_err(|_| overflow())
}

fn bytes_from_owned(bytes: OwnedBytes, budget: &NativeBudget) -> Result<Bytes> {
    let metadata = u64::try_from(std::mem::size_of::<BytesOwnerLayout<FrameOwner>>())
        .map_err(|_| overflow())?;
    let credit = budget.reserve(metadata)?;
    budget.check()?;
    let frame = Bytes::from_owner(FrameOwner {
        bytes,
        _credit: credit,
    });
    budget.check()?;
    Ok(frame)
}

/// Wire framing for an encoded MCP JSON-RPC response.
#[derive(Clone, Copy, Debug)]
pub(super) enum Framing<'a> {
    /// Stateless HTTP `application/json` body.
    Json,
    /// Stdio newline-delimited JSON-RPC frame (`{...}\n`).
    NdJson,
    /// Streamable HTTP Server-Sent Event frame (`data: {...}\n[id: ...]\n[retry: ...]\n\n`).
    Sse {
        event_id: Option<&'a str>,
        retry: Option<Duration>,
    },
}

// SSE requires each raw line of JSON data to carry its own `data: ` prefix.
struct SseData<'a> {
    writer: &'a mut dyn Write,
}

impl Write for SseData<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut start = 0;
        for (i, byte) in bytes.iter().enumerate() {
            if matches!(byte, b'\r' | b'\n') {
                self.writer.write_all(&bytes[start..i])?;
                self.writer.write_all(b"\ndata: ")?;
                start = i + 1;
            }
        }
        self.writer.write_all(&bytes[start..])?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn encode_with_framing<T: Serialize>(
    value: &T,
    budget: &NativeBudget,
    framing: Framing<'_>,
) -> Result<Bytes> {
    if let Framing::Sse {
        event_id: Some(id), ..
    } = framing
        && id.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
    {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSE event ID",
        )));
    }
    let bytes = budget.encode_bytes(|writer| {
        match framing {
            Framing::Json => {
                serde_json::to_writer(&mut *writer, value).map_err(io::Error::other)?;
            }
            Framing::NdJson => {
                serde_json::to_writer(&mut *writer, value).map_err(io::Error::other)?;
                writer.write_all(b"\n")?;
            }
            Framing::Sse { event_id, retry } => {
                writer.write_all(b"data: ")?;
                serde_json::to_writer(
                    SseData {
                        writer: &mut *writer,
                    },
                    value,
                )
                .map_err(io::Error::other)?;
                writer.write_all(b"\n")?;
                if let Some(id) = event_id {
                    writeln!(writer, "id: {id}")?;
                }
                if let Some(retry) = retry {
                    writeln!(writer, "retry: {}", retry.as_millis() as u64)?;
                }
                writer.write_all(b"\n")?;
            }
        }
        Ok(())
    })?;
    bytes_from_owned(bytes, budget)
}

fn encode_uncredited_framing<T: Serialize>(value: &T, framing: Framing<'_>) -> Bytes {
    let mut out = Vec::new();
    match framing {
        Framing::Json => {
            let _ = serde_json::to_writer(&mut out, value);
        }
        Framing::NdJson => {
            let _ = serde_json::to_writer(&mut out, value);
            out.push(b'\n');
        }
        Framing::Sse { event_id, retry } => {
            out.extend_from_slice(b"data: ");
            let _ = serde_json::to_writer(SseData { writer: &mut out }, value);
            out.push(b'\n');
            if let Some(id) = event_id {
                let _ = writeln!(&mut out, "id: {id}");
            }
            if let Some(retry) = retry {
                let _ = writeln!(&mut out, "retry: {}", retry.as_millis() as u64);
            }
            out.push(b'\n');
        }
    }
    Bytes::from(out)
}

enum RetainedCallKind {
    Text(OwnedText),
    JsonText(OwnedJson),
    Structured(OwnedJson),
    Error {
        text: OwnedText,
        error_meta: OwnedJson,
    },
}

struct RetainedCallInner {
    kind: RetainedCallKind,
    modern: bool,
    budget: NativeBudget,
    grant: Option<Arc<PoolGrant>>,
    memory_limit: u64,
    _credit: NativeReservation,
}

/// An immutable, `Arc`-shared MCP `tools/call` result whose rendered payload stays
/// charged under its originating query/call credit until the last owner drops.
#[derive(Clone)]
pub(super) struct RetainedCallResult {
    inner: Arc<RetainedCallInner>,
}

impl RetainedCallResult {
    /// Admit `outcome` into `preparation`'s `NativeBudget` while keeping any already-paid
    /// `CompactPage` without copying.
    ///
    /// Returns `Err(ToolError)` if admitting the result hits a memory/cancel/timeout limit
    /// so the caller can return a clean MCP tool error without allocating past the ceiling.
    pub(super) fn from_outcome(
        outcome: std::result::Result<Outcome, ToolError>,
        preparation: &Preparation,
        grant: Option<Arc<PoolGrant>>,
        modern: bool,
        err_ctx: &ErrorContext<'_>,
    ) -> std::result::Result<Self, ToolError> {
        let budget = NativeBudget::from_preparation(preparation).map_err(|e| err_ctx.engine(e))?;
        let kind = match outcome {
            Ok(Outcome::Retained(CompactPage::Table(text))) => RetainedCallKind::Text(text),
            Ok(Outcome::Retained(CompactPage::Json(json))) => RetainedCallKind::JsonText(json),
            Ok(Outcome::Text(text)) => {
                RetainedCallKind::Text(budget.text(&text).map_err(|e| err_ctx.engine(e))?)
            }
            Ok(Outcome::Structured(value)) => {
                RetainedCallKind::Structured(budget.json(&value).map_err(|e| err_ctx.engine(e))?)
            }
            Err(tool_error) => {
                // Tool errors are tiny diagnostic payloads; if the call already exhausted
                // its budget, return `Err(tool_error)` so the caller renders it directly.
                let text = budget
                    .text(&tool_error.text())
                    .map_err(|_| tool_error.clone())?;
                let error_meta = budget
                    .json(&tool_error.meta())
                    .map_err(|_| tool_error.clone())?;
                RetainedCallKind::Error { text, error_meta }
            }
        };
        let credit = budget
            .reserve(arc_bytes::<RetainedCallInner>().map_err(|e| err_ctx.engine(e))?)
            .map_err(|e| err_ctx.engine(e))?;
        budget.check().map_err(|e| err_ctx.engine(e))?;
        Ok(Self {
            inner: Arc::new(RetainedCallInner {
                kind,
                modern,
                budget,
                grant,
                memory_limit: preparation.limit(),
                _credit: credit,
            }),
        })
    }

    pub(super) fn modern(&self) -> bool {
        self.inner.modern
    }

    pub(super) fn budget(&self) -> &NativeBudget {
        &self.inner.budget
    }

    /// Create a fresh-control `NativeBudget` sharing this retained result's `PoolGrant`
    /// and memory ceiling, so a later `tasks/get` poll can render a wire frame without
    /// failing on the original query's expired deadline or deadlocking on a second
    /// `INITIAL_GRANT` admission on a small pool.
    pub(super) fn fresh_frame_budget(&self, control: Control) -> Result<NativeBudget> {
        let prep = match &self.inner.grant {
            Some(grant) => {
                Preparation::with_grant(control, self.inner.memory_limit, grant.clone())?
            }
            None => Preparation::new(control, self.inner.memory_limit)?,
        };
        NativeBudget::from_preparation(&prep)
    }

    /// Fallback conversion to an ordinary `CallToolResult` when no qualified transport
    /// is attached (e.g., direct unit tests of `Adapter`).
    pub(super) fn to_call_tool_result(&self) -> CallToolResult {
        let mut r = match &self.inner.kind {
            RetainedCallKind::Text(t) => {
                CallToolResult::success(vec![ContentBlock::text(t.as_str().to_string())])
            }
            RetainedCallKind::JsonText(j) => {
                CallToolResult::success(vec![ContentBlock::text(j.as_str().to_string())])
            }
            RetainedCallKind::Structured(j) => {
                let v: Value = serde_json::from_str(j.as_str()).unwrap_or(Value::Null);
                CallToolResult::structured(v)
            }
            RetainedCallKind::Error { text, error_meta } => {
                let mut meta = MetaObject::default();
                if let Ok(v) = serde_json::from_str(error_meta.as_str()) {
                    meta.insert(ERROR_META.to_string(), v);
                }
                CallToolResult::error(vec![ContentBlock::text(text.as_str().to_string())])
                    .with_meta(Some(meta))
            }
        };
        if self.inner.modern {
            let meta = r.meta.get_or_insert_with(MetaObject::default);
            if let Ok(v) = serde_json::to_value(implementation()) {
                meta.insert(SERVER_INFO.to_string(), v);
            }
        } else {
            r.result_type = None;
        }
        r
    }

    /// Fallback conversion to a `JsonObject` for `DetailedTask` when no qualified
    /// transport is attached.
    pub(super) fn to_json_object(&self) -> JsonObject {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map,
            _ => JsonObject::new(),
        }
    }
}

struct TextBlock<'a>(&'a str);

impl Serialize for TextBlock<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("type", "text")?;
        map.serialize_entry("text", self.0)?;
        map.end()
    }
}

struct ToolMeta<'a> {
    error_meta: Option<&'a RawValue>,
    modern: bool,
}

impl Serialize for ToolMeta<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let count = usize::from(self.error_meta.is_some()) + usize::from(self.modern);
        let mut map = serializer.serialize_map(Some(count))?;
        if let Some(error_meta) = self.error_meta {
            map.serialize_entry(ERROR_META, error_meta)?;
        }
        if self.modern {
            map.serialize_entry(SERVER_INFO, &implementation())?;
        }
        map.end()
    }
}

impl Serialize for RetainedCallResult {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let (text, structured, is_error, error_meta) = match &self.inner.kind {
            RetainedCallKind::Text(t) => (t.as_str(), None, false, None),
            RetainedCallKind::JsonText(j) => (j.as_str(), None, false, None),
            RetainedCallKind::Structured(j) => (j.as_str(), Some(j.as_raw()), false, None),
            RetainedCallKind::Error { text, error_meta } => {
                (text.as_str(), None, true, Some(error_meta.as_raw()))
            }
        };
        let has_meta = self.inner.modern || error_meta.is_some();
        let len = usize::from(self.inner.modern)
            + 1 // content
            + usize::from(structured.is_some())
            + 1 // isError
            + usize::from(has_meta);
        let mut map = serializer.serialize_map(Some(len))?;
        if self.inner.modern {
            map.serialize_entry("resultType", ResultType::COMPLETE.as_str())?;
        }
        map.serialize_entry("content", &[TextBlock(text)])?;
        if let Some(structured) = structured {
            map.serialize_entry("structuredContent", structured)?;
        }
        map.serialize_entry("isError", &is_error)?;
        if has_meta {
            map.serialize_entry(
                "_meta",
                &ToolMeta {
                    error_meta,
                    modern: self.inner.modern,
                },
            )?;
        }
        map.end()
    }
}

struct RetainedResourceInner {
    uri: String,
    mime_type: &'static str,
    text: OwnedText,
    modern: bool,
    ttl_ms: u64,
    cache_scope: CacheScope,
    budget: NativeBudget,
    _credit: NativeReservation,
}

/// An immutable, `Arc`-shared MCP `resources/read` result held under query credit.
#[derive(Clone)]
pub(super) struct RetainedResourceResult {
    inner: Arc<RetainedResourceInner>,
}

impl RetainedResourceResult {
    pub(super) fn new(
        uri: String,
        mime_type: &'static str,
        text: OwnedText,
        modern: bool,
        ttl_ms: u64,
        cache_scope: CacheScope,
        budget: NativeBudget,
    ) -> Result<Self> {
        let credit = budget.reserve(arc_bytes::<RetainedResourceInner>()?)?;
        budget.check()?;
        Ok(Self {
            inner: Arc::new(RetainedResourceInner {
                uri,
                mime_type,
                text,
                modern,
                ttl_ms,
                cache_scope,
                budget,
                _credit: credit,
            }),
        })
    }
}

struct ResourceTextEntry<'a> {
    uri: &'a str,
    mime_type: &'a str,
    text: &'a str,
}

impl Serialize for ResourceTextEntry<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("uri", self.uri)?;
        map.serialize_entry("mimeType", self.mime_type)?;
        map.serialize_entry("text", self.text)?;
        map.end()
    }
}

impl Serialize for RetainedResourceResult {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let len = usize::from(self.inner.modern) + 3;
        let mut map = serializer.serialize_map(Some(len))?;
        if self.inner.modern {
            map.serialize_entry("resultType", ResultType::COMPLETE.as_str())?;
        }
        map.serialize_entry(
            "contents",
            &[ResourceTextEntry {
                uri: &self.inner.uri,
                mime_type: self.inner.mime_type,
                text: self.inner.text.as_str(),
            }],
        )?;
        map.serialize_entry("ttlMs", &self.inner.ttl_ms)?;
        map.serialize_entry("cacheScope", &self.inner.cache_scope)?;
        map.end()
    }
}

/// Wire shape for a completed `GetTaskResult` whose `result` borrows a `RetainedCallResult`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompletedTaskWire<'a> {
    #[serde(flatten)]
    task: &'a Task,
    result: &'a RetainedCallResult,
}

#[derive(Serialize)]
struct JsonRpcEnvelope<'a, R: Serialize> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    result: R,
}

#[derive(Serialize)]
struct JsonRpcErrorEnvelope<'a> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    error: rmcp::ErrorData,
}

/// A retained response staged by `Adapter` for a qualified transport (`CreditedStdioTransport`,
/// stateless HTTP `Endpoint`, or `CreditedSessionManager`).
pub(super) enum StagedResponse {
    Tool {
        id: RequestId,
        result: RetainedCallResult,
    },
    TaskCompleted {
        id: RequestId,
        task: Task,
        result: RetainedCallResult,
        frame_budget: NativeBudget,
    },
    Resource {
        id: RequestId,
        result: RetainedResourceResult,
    },
}

impl StagedResponse {
    pub(super) fn id(&self) -> &RequestId {
        match self {
            Self::Tool { id, .. } | Self::TaskCompleted { id, .. } | Self::Resource { id, .. } => {
                id
            }
        }
    }

    pub(super) fn modern(&self) -> bool {
        match self {
            Self::Tool { result, .. } | Self::TaskCompleted { result, .. } => result.modern(),
            Self::Resource { result, .. } => result.inner.modern,
        }
    }

    /// Encode this staged response into a credited `Bytes` frame under its `NativeBudget`.
    pub(super) fn encode_frame(&self, framing: Framing<'_>) -> Result<Bytes> {
        match self {
            Self::Tool { id, result } => encode_with_framing(
                &JsonRpcEnvelope {
                    jsonrpc: "2.0",
                    id,
                    result,
                },
                result.budget(),
                framing,
            ),
            Self::TaskCompleted {
                id,
                task,
                result,
                frame_budget,
            } => encode_with_framing(
                &JsonRpcEnvelope {
                    jsonrpc: "2.0",
                    id,
                    result: CompletedTaskWire { task, result },
                },
                frame_budget,
                framing,
            ),
            Self::Resource { id, result } => encode_with_framing(
                &JsonRpcEnvelope {
                    jsonrpc: "2.0",
                    id,
                    result,
                },
                &result.inner.budget,
                framing,
            ),
        }
    }

    /// Encode `self` into a credited wire frame, or if encoding is refused by a memory
    /// or control limit, drop `self` FIRST (releasing all credit!) and return a clean,
    /// tiny MCP error frame without allocating past the pool ceiling.
    pub(super) fn into_frame_or_error(self, framing: Framing<'_>, max_timeout_secs: f64) -> Bytes {
        match self.encode_frame(framing) {
            Ok(bytes) => bytes,
            Err(err) => {
                let id = self.id().clone();
                let modern = self.modern();
                let is_tool = matches!(self, Self::Tool { .. });
                // Release all retained payload credit before building the tiny error frame.
                drop(self);
                let req_id = id.to_string();
                let tool_err = ErrorContext {
                    timeout_secs: max_timeout_secs,
                    max_timeout_secs,
                    prefix_names: &[],
                    request_id: &req_id,
                }
                .engine(err);
                if is_tool {
                    let mut meta = MetaObject::default();
                    meta.insert(ERROR_META.to_string(), tool_err.meta());
                    if modern && let Ok(v) = serde_json::to_value(implementation()) {
                        meta.insert(SERVER_INFO.to_string(), v);
                    }
                    let mut result =
                        CallToolResult::error(vec![ContentBlock::text(tool_err.text())])
                            .with_meta(Some(meta));
                    if !modern {
                        result.result_type = None;
                    }
                    encode_uncredited_framing(
                        &JsonRpcEnvelope {
                            jsonrpc: "2.0",
                            id: &id,
                            result,
                        },
                        framing,
                    )
                } else {
                    encode_uncredited_framing(
                        &JsonRpcErrorEnvelope {
                            jsonrpc: "2.0",
                            id: &id,
                            error: rmcp::ErrorData::internal_error(
                                tool_err.text(),
                                Some(tool_err.meta()),
                            ),
                        },
                        framing,
                    )
                }
            }
        }
    }
}

/// Helper to build a tiny placeholder `GetTaskResult` when `StagedResponse::TaskCompleted`
/// is already parked in a `ResponseSlot`.
pub(super) fn placeholder_task_result(task: Task) -> GetTaskResult {
    GetTaskResult::new(DetailedTask::new(task, TaskPayload::Working))
}

enum SlotState {
    Open(Option<Box<StagedResponse>>),
    Closed,
}

/// One-shot handoff slot carried in request extensions between `Adapter` and a qualified
/// transport. Closing the slot drops any parked payload immediately and rejects any late
/// park, guaranteeing zero credit leaks on cancellation, error, or disconnect.
#[derive(Clone)]
pub(super) struct ResponseSlot {
    inner: Arc<Mutex<SlotState>>,
}

impl ResponseSlot {
    pub(super) fn open() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SlotState::Open(None))),
        }
    }

    pub(super) fn park(&self, staged: StagedResponse) {
        let mut guard = self.inner.lock();
        if let SlotState::Open(slot) = &mut *guard {
            *slot = Some(Box::new(staged));
        } else {
            drop(guard);
            drop(staged);
        }
    }

    pub(super) fn take(&self) -> Option<StagedResponse> {
        let mut guard = self.inner.lock();
        match std::mem::replace(&mut *guard, SlotState::Closed) {
            SlotState::Open(staged) => staged.map(|b| *b),
            SlotState::Closed => None,
        }
    }

    pub(super) fn close(&self) {
        let dropped = self.take();
        drop(dropped);
    }
}

/// Drop guard ensuring an unconsumed `ResponseSlot` is always closed on early exit or
/// future cancellation, unless explicitly disarmed when handed off to a session worker.
pub(super) struct SlotGuard(Option<ResponseSlot>);

impl SlotGuard {
    pub(super) fn new(slot: ResponseSlot) -> Self {
        Self(Some(slot))
    }

    pub(super) fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if let Some(slot) = self.0.take() {
            slot.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::sparql::pool::QueryPool;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn fixture(limit: u64) -> (Preparation, Arc<PoolGrant>, Arc<QueryPool>, Arc<AtomicBool>) {
        let pool = QueryPool::new(16 << 20);
        let grant = Arc::new(pool.empty_grant());
        let cancel = Arc::new(AtomicBool::new(false));
        let preparation =
            Preparation::with_grant(Control::with_cancel(cancel.clone()), limit, grant.clone())
                .unwrap();
        (preparation, grant, pool, cancel)
    }

    fn err_ctx() -> ErrorContext<'static> {
        ErrorContext {
            timeout_secs: 30.0,
            max_timeout_secs: 60.0,
            prefix_names: &[],
            request_id: "9",
        }
    }

    #[test]
    fn complete_mcp_frame_slices_retain_real_bytes_and_credit_until_final_free() {
        let (prep, grant, pool, _) = fixture(4 << 20);
        let budget = NativeBudget::from_preparation(&prep).unwrap();
        let text = budget.text(&"x".repeat(64 << 10)).unwrap();
        let retained = RetainedCallResult::from_outcome(
            Ok(Outcome::Retained(CompactPage::Table(text))),
            &prep,
            Some(grant.clone()),
            true,
            &err_ctx(),
        )
        .unwrap();
        let body_held = grant.used();
        let staged = StagedResponse::Tool {
            id: RequestId::Number(9),
            result: retained,
        };
        let frame = staged.encode_frame(Framing::Json).unwrap();
        assert!(grant.used() >= body_held + frame.len() as u64);
        let expected: Value = serde_json::from_slice(&frame).unwrap();
        assert_eq!(expected["id"], 9);
        assert_eq!(expected["result"]["resultType"], "complete");
        assert_eq!(expected["result"]["isError"], false);
        assert_eq!(
            expected["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .len(),
            64 << 10
        );
        let last = frame.slice(1..frame.len() - 1);
        drop(frame);
        drop(staged);
        drop(budget);
        drop(prep);
        assert!(grant.used() >= last.len() as u64);
        assert_eq!(last[0], b'"');
        drop(last);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn real_mcp_frame_cancel_and_invalid_event_id_publish_no_unpaid_fallback() {
        let (prep, grant, pool, cancel) = fixture(4 << 20);
        let retained = RetainedCallResult::from_outcome(
            Ok(Outcome::Structured(serde_json::json!({"rows": [["ok"]]}))),
            &prep,
            Some(grant.clone()),
            false,
            &err_ctx(),
        )
        .unwrap();
        let staged = StagedResponse::Tool {
            id: RequestId::Number(2),
            result: retained,
        };
        let before = grant.used();
        let err = staged
            .encode_frame(Framing::Sse {
                event_id: Some("bad\nid"),
                retry: None,
            })
            .unwrap_err();
        assert!(matches!(err, Error::Io(e) if e.kind() == io::ErrorKind::InvalidInput));
        assert_eq!(grant.used(), before);
        cancel.store(true, Ordering::SeqCst);
        let err = staged.encode_frame(Framing::Json).unwrap_err();
        assert!(matches!(err, Error::Cancelled));
        assert_eq!(grant.used(), before);
        drop(staged);
        drop(prep);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn into_frame_or_error_refuses_cleanly_and_releases_credit_on_tight_budget() {
        // Enough room for the 64 KiB text, not enough for a second 64 KiB wire frame.
        let (prep, grant, pool, _) = fixture(96 << 10);
        let budget = NativeBudget::from_preparation(&prep).unwrap();
        let text = budget.text(&"z".repeat(64 << 10)).unwrap();
        let retained = RetainedCallResult::from_outcome(
            Ok(Outcome::Retained(CompactPage::Table(text))),
            &prep,
            Some(grant.clone()),
            true,
            &err_ctx(),
        )
        .unwrap();
        let staged = StagedResponse::Tool {
            id: RequestId::Number(7),
            result: retained,
        };
        drop(budget);
        drop(prep);
        let frame = staged.into_frame_or_error(Framing::Json, 30.0);
        // All retained credit was released before building the tiny error frame.
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
        let v: Value = serde_json::from_slice(&frame).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["result"]["isError"], true);
        assert_eq!(
            v["result"]["_meta"]["io.github.kclejeune.sparkles/error"]["code"],
            "budget-memory"
        );
    }

    #[test]
    fn response_slot_close_drops_parked_and_rejects_late_park() {
        let (prep, grant, pool, _) = fixture(4 << 20);
        let retained = RetainedCallResult::from_outcome(
            Ok(Outcome::Text("a".repeat(64 << 10))),
            &prep,
            Some(grant.clone()),
            true,
            &err_ctx(),
        )
        .unwrap();
        let slot = ResponseSlot::open();
        slot.park(StagedResponse::Tool {
            id: RequestId::Number(1),
            result: retained,
        });
        drop(prep);
        assert!(grant.used() >= (64 << 10));
        slot.close();
        assert_eq!(grant.used(), 0);
        // Late park after close drops immediately.
        let prep2 = Preparation::with_grant(Control::default(), 4 << 20, grant.clone()).unwrap();
        let retained2 = RetainedCallResult::from_outcome(
            Ok(Outcome::Text("b".repeat(64 << 10))),
            &prep2,
            Some(grant.clone()),
            true,
            &err_ctx(),
        )
        .unwrap();
        drop(prep2);
        slot.park(StagedResponse::Tool {
            id: RequestId::Number(2),
            result: retained2,
        });
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }
}
