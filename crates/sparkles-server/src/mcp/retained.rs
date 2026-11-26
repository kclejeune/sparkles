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
use sparkles::task::Control;
use std::alloc::Layout;
use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

fn implementation() -> Implementation {
    Implementation::new("sparkles", env!("CARGO_PKG_VERSION"))
}

struct TrimOnDrop(Preparation);

impl Drop for TrimOnDrop {
    fn drop(&mut self) {
        self.0.trim_grant();
    }
}

struct FrameOwner {
    bytes: OwnedBytes,
    _credit: NativeReservation,
    // Drops LAST (after `bytes` and `_credit` release their grant charges) so a
    // retained task's grant shrinks back to actual retained usage once a `tasks/get`
    // wire frame finishes writing.
    _trim_on_drop: Option<TrimOnDrop>,
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

fn bytes_from_owned(
    bytes: OwnedBytes,
    budget: &NativeBudget,
    trim_on_drop: Option<Preparation>,
) -> Result<Bytes> {
    let metadata = u64::try_from(std::mem::size_of::<BytesOwnerLayout<FrameOwner>>())
        .map_err(|_| overflow())?;
    let credit = budget.reserve(metadata)?;
    budget.check()?;
    let frame = Bytes::from_owner(FrameOwner {
        bytes,
        _credit: credit,
        _trim_on_drop: trim_on_drop.map(TrimOnDrop),
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
    prev_cr: bool,
}

impl Write for SseData<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut start = 0;
        for (i, &byte) in bytes.iter().enumerate() {
            if byte == b'\n' && self.prev_cr {
                self.prev_cr = false;
                start = i + 1;
                continue;
            }
            self.prev_cr = byte == b'\r';
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

fn valid_event_id(id: &str) -> bool {
    !id.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
}

fn encode_with_framing<T: Serialize>(
    value: &T,
    budget: &NativeBudget,
    framing: Framing<'_>,
    trim_on_drop: Option<Preparation>,
) -> Result<Bytes> {
    if let Framing::Sse {
        event_id: Some(id), ..
    } = framing
        && !valid_event_id(id)
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
                        prev_cr: false,
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
    bytes_from_owned(bytes, budget, trim_on_drop)
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
            let _ = serde_json::to_writer(
                SseData {
                    writer: &mut out,
                    prev_cr: false,
                },
                value,
            );
            out.push(b'\n');
            if let Some(id) = event_id
                && valid_event_id(id)
            {
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

/// Format `ToolError` text directly into a `NativeBudget` without an intermediate `String`.
struct ToolErrorText<'a>(&'a ToolError);

impl fmt::Display for ToolErrorText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.message)?;
        if let Some(hint) = &self.0.hint {
            f.write_str("\nHint: ")?;
            f.write_str(hint)?;
        }
        if let Some(data) = &self.0.data {
            f.write_str("\n")?;
            fmt::Display::fmt(data, f)?;
        }
        Ok(())
    }
}

/// Serialize `ToolError` metadata directly into a `NativeBudget` without cloning `data`.
struct ToolErrorMeta<'a>(&'a ToolError);

impl Serialize for ToolErrorMeta<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let count = 2 + usize::from(self.0.budget.is_some()) + usize::from(self.0.data.is_some());
        let mut map = serializer.serialize_map(Some(count))?;
        map.serialize_entry("code", &*self.0.code)?;
        map.serialize_entry("status", &self.0.status)?;
        if let Some(budget) = self.0.budget {
            map.serialize_entry("budget", budget)?;
        }
        if let Some(data) = &self.0.data {
            map.serialize_entry("data", data)?;
        }
        map.end()
    }
}

/// Whether `error` is a strictly small, fixed-bound diagnostic error with no attached `data`
/// payload (e.g., `budget-memory` with a short remedy hint) that may be returned even when a
/// tiny configured `query_memory_bytes` ceiling cannot also admit its error text.
pub(super) fn is_small_bounded_error(error: &ToolError) -> bool {
    error.data.is_none()
        && error.code.len() <= 64
        && error.message.len() <= 512
        && error.hint.as_deref().map_or(0, str::len) <= 512
}

const MAX_WHY_EMPTY_SUMMARY_BYTES: usize = 2048;

fn json_char_escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\x08' | '\x0c' | '\n' | '\r' | '\t' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// Truncate `s` on a UTF-8 boundary so its JSON-encoded string contents (excluding
/// surrounding quotes) occupy at most `max_json_bytes`.
fn truncate_json_str_bytes(s: &str, max_json_bytes: usize) -> &str {
    let mut used = 0usize;
    for (idx, ch) in s.char_indices() {
        let len = json_char_escaped_len(ch);
        if used + len > max_json_bytes {
            return &s[..idx];
        }
        used += len;
    }
    s
}

/// Build a strictly bounded (<= 2 KiB serialized) incomplete diagnostic summary for
/// `why_empty` when its own checks hit the memory budget under a tiny ceiling (e.g. 512 B)
/// and the full diagnosis object cannot be admitted into `NativeBudget`.
pub(super) fn bounded_why_empty_summary(value: &Value) -> Option<Value> {
    let obj = value.as_object()?;
    if obj.get("complete") != Some(&Value::Bool(false)) {
        return None;
    }
    let dataset = obj
        .get("dataset")
        .and_then(Value::as_str)
        .map(|s| truncate_json_str_bytes(s, 128))
        .unwrap_or("");
    let commit = obj.get("commit").and_then(Value::as_u64).unwrap_or(0);
    let empty = match obj.get("empty") {
        Some(Value::Bool(b)) => Value::Bool(*b),
        _ => Value::Null,
    };
    let message = obj
        .get("message")
        .and_then(Value::as_str)
        .map(|s| truncate_json_str_bytes(s, 256))
        .unwrap_or("query diagnosis exceeded the memory budget; steps are incomplete");
    let mut summary = serde_json::json!({
        "dataset": dataset,
        "commit": commit,
        "empty": empty,
        "steps": [],
        "complete": false,
        "message": message,
        "prefixes": {},
    });
    let mut serialized_len = serde_json::to_vec(&summary).ok()?.len();
    let mut steps = Vec::new();
    if let Some(arr) = obj.get("steps").and_then(Value::as_array) {
        for step in arr.iter().take(8) {
            let Some(so) = step.as_object() else {
                continue;
            };
            let kind = so
                .get("kind")
                .and_then(Value::as_str)
                .map(|s| truncate_json_str_bytes(s, 32))
                .unwrap_or("pattern");
            let text = so
                .get("text")
                .and_then(Value::as_str)
                .map(|s| truncate_json_str_bytes(s, 128))
                .unwrap_or("");
            let solutions = match so.get("solutions") {
                Some(Value::Bool(b)) => Value::Bool(*b),
                _ => Value::Null,
            };
            let candidate = serde_json::json!({
                "kind": kind,
                "text": text,
                "solutions": solutions,
            });
            let step_len = serde_json::to_vec(&candidate).ok()?.len();
            let extra = step_len + usize::from(!steps.is_empty());
            if serialized_len.saturating_add(extra) > MAX_WHY_EMPTY_SUMMARY_BYTES {
                break;
            }
            serialized_len += extra;
            steps.push(candidate);
        }
    }
    summary["steps"] = Value::Array(steps);
    if !serde_json::to_vec(&summary).is_ok_and(|b| b.len() <= MAX_WHY_EMPTY_SUMMARY_BYTES) {
        summary["steps"] = Value::Array(Vec::new());
        summary["dataset"] = Value::String(truncate_json_str_bytes(dataset, 64).to_string());
        summary["message"] = Value::String(
            "query diagnosis exceeded the memory budget; steps are incomplete".to_string(),
        );
    }
    debug_assert!(
        serde_json::to_vec(&summary).is_ok_and(|b| b.len() <= MAX_WHY_EMPTY_SUMMARY_BYTES)
    );
    Some(summary)
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
    preparation: Preparation,
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
    /// so the caller can return a bounded MCP tool error without allocating past the ceiling.
    pub(super) fn from_outcome(
        outcome: std::result::Result<Outcome, ToolError>,
        preparation: &Preparation,
        modern: bool,
        err_ctx: &ErrorContext<'_>,
    ) -> std::result::Result<Self, ToolError> {
        let budget = NativeBudget::from_preparation(preparation).map_err(|e| match &outcome {
            Err(tool_error) if is_small_bounded_error(tool_error) => tool_error.clone(),
            _ => err_ctx.engine(e),
        })?;
        let mut small_err: Option<ToolError> = None;
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
                let text = match budget.format_text(&ToolErrorText(&tool_error)) {
                    Ok(t) => t,
                    Err(admit_err) => {
                        return Err(if is_small_bounded_error(&tool_error) {
                            tool_error
                        } else {
                            err_ctx.engine(admit_err)
                        });
                    }
                };
                let error_meta = match budget.json(&ToolErrorMeta(&tool_error)) {
                    Ok(m) => m,
                    Err(admit_err) => {
                        return Err(if is_small_bounded_error(&tool_error) {
                            tool_error
                        } else {
                            err_ctx.engine(admit_err)
                        });
                    }
                };
                if is_small_bounded_error(&tool_error) {
                    small_err = Some(tool_error);
                }
                RetainedCallKind::Error { text, error_meta }
            }
        };
        let map_final = |e: Error| match small_err.take() {
            Some(tool_error) => tool_error,
            None => err_ctx.engine(e),
        };
        let credit = budget
            .reserve(arc_bytes::<RetainedCallInner>().map_err(|e| err_ctx.engine(e))?)
            .map_err(map_final)?;
        budget.check().map_err(|e| err_ctx.engine(e))?;
        Ok(Self {
            inner: Arc::new(RetainedCallInner {
                kind,
                modern,
                budget,
                preparation: preparation.clone(),
                _credit: credit,
            }),
        })
    }

    /// Borrow a structured `Value` and admit it into `preparation` without cloning `value`.
    pub(super) fn from_structured_ref(
        value: &Value,
        preparation: &Preparation,
        modern: bool,
    ) -> Result<Self> {
        let budget = NativeBudget::from_preparation(preparation)?;
        let kind = RetainedCallKind::Structured(budget.json(value)?);
        let credit = budget.reserve(arc_bytes::<RetainedCallInner>()?)?;
        budget.check()?;
        Ok(Self {
            inner: Arc::new(RetainedCallInner {
                kind,
                modern,
                budget,
                preparation: preparation.clone(),
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

    pub(super) fn trim_grant(&self) {
        self.inner.preparation.trim_grant();
    }

    /// Create a fresh-control [`TaskFrameBudget`] sharing this retained result's remaining
    /// `query_memory_bytes` allowance and `PoolGrant`, so a later `tasks/get` poll can
    /// render a wire frame after the original query's deadline without exceeding the
    /// configured memory ceiling or deadlocking on a second `INITIAL_GRANT` admission.
    ///
    /// Carries a trailing [`TrimOnDrop`] guard from admission through every exit path
    /// (including early cancellation, closed slots, and cancelled stdio jobs) so an
    /// abandoned poll never leaves a trimmed task holding a 4 MiB granule.
    pub(super) fn fresh_frame_budget(&self, control: Control) -> Result<TaskFrameBudget> {
        let trim_on_drop = TrimOnDrop(self.inner.preparation.clone());
        let child = self.inner.preparation.child_with_control(control)?;
        let budget = NativeBudget::from_preparation(&child)?;
        Ok(TaskFrameBudget {
            budget,
            _trim_on_drop: trim_on_drop,
        })
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
    preparation: Preparation,
    _credit: NativeReservation,
}

/// An immutable, `Arc`-shared MCP `resources/read` result held under query credit.
#[derive(Clone)]
pub(super) struct RetainedResourceResult {
    inner: Arc<RetainedResourceInner>,
}

impl RetainedResourceResult {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        uri: String,
        mime_type: &'static str,
        text: OwnedText,
        modern: bool,
        ttl_ms: u64,
        cache_scope: CacheScope,
        budget: NativeBudget,
        preparation: Preparation,
    ) -> Result<Self> {
        let uri_bytes = u64::try_from(uri.capacity()).map_err(|_| overflow())?;
        let total = arc_bytes::<RetainedResourceInner>()?
            .checked_add(uri_bytes)
            .ok_or_else(overflow)?;
        let credit = budget.reserve(total)?;
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
                preparation,
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
/// Matches `rmcp::model::GetTaskResult` verbatim, including outer `"resultType": "complete"`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompletedTaskWire<'a> {
    result_type: ResultType,
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

/// Fresh-control frame budget for rendering a completed background task on `tasks/get`.
///
/// Field drop order is load-bearing: `budget` (and its child `Preparation` /
/// `ParentHandle` reservations) drops first, then `_trim_on_drop` trims the backing
/// `PoolGrant` back to the retained task's actual usage on every exit path.
pub(super) struct TaskFrameBudget {
    budget: NativeBudget,
    _trim_on_drop: TrimOnDrop,
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
        task: Box<Task>,
        result: RetainedCallResult,
        frame_budget: TaskFrameBudget,
        cancel: Arc<AtomicBool>,
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

    fn cancel_on_close(&self) {
        if let Self::TaskCompleted { cancel, .. } = self {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn trim_preparation(&self) -> Preparation {
        match self {
            Self::Tool { result, .. } | Self::TaskCompleted { result, .. } => {
                result.inner.preparation.clone()
            }
            Self::Resource { result, .. } => result.inner.preparation.clone(),
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
                None,
            ),
            Self::TaskCompleted {
                id,
                task,
                result,
                frame_budget,
                ..
            } => encode_with_framing(
                &JsonRpcEnvelope {
                    jsonrpc: "2.0",
                    id,
                    result: CompletedTaskWire {
                        result_type: ResultType::COMPLETE,
                        task: task.as_ref(),
                        result,
                    },
                },
                &frame_budget.budget,
                framing,
                Some(result.inner.preparation.clone()),
            ),
            Self::Resource { id, result } => encode_with_framing(
                &JsonRpcEnvelope {
                    jsonrpc: "2.0",
                    id,
                    result,
                },
                &result.inner.budget,
                framing,
                None,
            ),
        }
    }

    /// Encode `self` into a credited wire frame, or if encoding is refused by a memory
    /// or control limit, drop `self` FIRST (releasing all credit!) and return a clean,
    /// tiny MCP error frame without allocating past the pool ceiling.
    ///
    /// On success, drops the staged intermediate payload (`RetainedCallResult` /
    /// `RetainedResourceResult`) and trims unused `INITIAL_GRANT` slack from the
    /// backing `PoolGrant` so idle HTTP bodies and SSE replay caches hold only the
    /// encoded frame's actual bytes.
    pub(super) fn into_frame_or_error(self, framing: Framing<'_>, max_timeout_secs: f64) -> Bytes {
        match self.encode_frame(framing) {
            Ok(bytes) => {
                let trim_prep = self.trim_preparation();
                drop(self);
                trim_prep.trim_grant();
                drop(trim_prep);
                bytes
            }
            Err(err) => {
                let id = self.id().clone();
                let modern = self.modern();
                let is_tool = matches!(self, Self::Tool { .. });
                let trim_prep =
                    matches!(self, Self::TaskCompleted { .. }).then(|| self.trim_preparation());
                // Release all retained payload credit before building the tiny error frame.
                drop(self);
                if let Some(prep) = trim_prep {
                    prep.trim_grant();
                }
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
    Open {
        staged: Option<Box<StagedResponse>>,
        on_cancel: Option<Box<dyn FnOnce() + Send>>,
    },
    Closed,
}

/// One-shot handoff slot carried in request extensions between `Adapter` and a qualified
/// transport. Closing the slot drops any parked payload immediately, invokes any armed
/// cancellation hook, and rejects any late park, guaranteeing zero credit leaks or
/// unreachable detached tasks on cancellation, error, or disconnect.
#[derive(Clone)]
pub(super) struct ResponseSlot {
    inner: Arc<Mutex<SlotState>>,
}

impl ResponseSlot {
    pub(super) fn open() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SlotState::Open {
                staged: None,
                on_cancel: None,
            })),
        }
    }

    pub(super) fn park(&self, new_staged: StagedResponse) {
        let mut guard = self.inner.lock();
        if let SlotState::Open { staged, .. } = &mut *guard {
            *staged = Some(Box::new(new_staged));
        } else {
            drop(guard);
            new_staged.cancel_on_close();
            drop(new_staged);
        }
    }

    /// Arm a hook to run if this slot is closed before [`Self::take`] consumes it.
    /// Returns `false` (and runs `hook` immediately) if the slot is already closed.
    pub(super) fn arm_cancel_hook(&self, hook: impl FnOnce() + Send + 'static) -> bool {
        let mut guard = self.inner.lock();
        if let SlotState::Open { on_cancel, .. } = &mut *guard {
            *on_cancel = Some(Box::new(hook));
            true
        } else {
            drop(guard);
            hook();
            false
        }
    }

    /// Disarm any pending cancel hook without closing or consuming staged payloads
    /// (used when a non-JSON 2xx response succeeds on stateless HTTP).
    pub(super) fn disarm_cancel_hook(&self) {
        let mut guard = self.inner.lock();
        if let SlotState::Open { on_cancel, .. } = &mut *guard {
            *on_cancel = None;
        }
    }

    pub(super) fn take(&self) -> Option<StagedResponse> {
        let mut guard = self.inner.lock();
        match std::mem::replace(&mut *guard, SlotState::Closed) {
            SlotState::Open { staged, .. } => staged.map(|b| *b),
            SlotState::Closed => None,
        }
    }

    pub(super) fn close(&self) {
        let state = {
            let mut guard = self.inner.lock();
            std::mem::replace(&mut *guard, SlotState::Closed)
        };
        if let SlotState::Open { staged, on_cancel } = state {
            if let Some(hook) = on_cancel {
                hook();
            }
            if let Some(dropped) = staged {
                dropped.cancel_on_close();
                drop(dropped);
            }
        }
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
    use sparkles::sparql::pool::{PoolGrant, QueryPool};
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

    #[test]
    fn completed_task_wire_includes_outer_result_type_and_enforces_query_memory_bytes() {
        // 96 KiB cap: retains 64 KiB payload, so fresh_frame_budget must NOT allow another 64 KiB!
        let (prep, grant, pool, _) = fixture(96 << 10);
        let budget = NativeBudget::from_preparation(&prep).unwrap();
        let text = budget.text(&"k".repeat(64 << 10)).unwrap();
        let retained = RetainedCallResult::from_outcome(
            Ok(Outcome::Retained(CompactPage::Table(text))),
            &prep,
            true,
            &err_ctx(),
        )
        .unwrap();
        drop(budget);
        drop(prep);
        let task = Task::new(
            "t-1".to_string(),
            rmcp::model::TaskStatus::Completed,
            "2026-01-01T00:00:00Z".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let frame_budget = retained
            .fresh_frame_budget(Control::with_cancel(cancel.clone()))
            .unwrap();
        let staged = StagedResponse::TaskCompleted {
            id: RequestId::Number(3),
            task: Box::new(task.clone()),
            result: retained.clone(),
            frame_budget,
            cancel,
        };
        // Encoding the >64 KiB wire frame on top of the 64 KiB retained payload exceeds 96 KiB!
        let err = staged.encode_frame(Framing::Json).unwrap_err();
        assert!(matches!(
            err,
            Error::BudgetExceeded(b) if b.kind == sparkles::BudgetKind::Memory
        ));
        drop(staged);
        drop(retained);
        drop(grant);
        assert_eq!(pool.granted(), 0);

        // With enough room, CompletedTaskWire emits outer `resultType: "complete"`.
        let (prep, grant, pool, _) = fixture(4 << 20);
        let retained = RetainedCallResult::from_outcome(
            Ok(Outcome::Text("hello".to_string())),
            &prep,
            true,
            &err_ctx(),
        )
        .unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let frame_budget = retained
            .fresh_frame_budget(Control::with_cancel(cancel.clone()))
            .unwrap();
        let staged = StagedResponse::TaskCompleted {
            id: RequestId::Number(4),
            task: Box::new(task),
            result: retained,
            frame_budget,
            cancel,
        };
        let frame = staged.encode_frame(Framing::Json).unwrap();
        let v: Value = serde_json::from_slice(&frame).unwrap();
        assert_eq!(v["result"]["resultType"], "complete", "{v}");
        assert_eq!(v["result"]["taskId"], "t-1");
        assert_eq!(v["result"]["status"], "completed");
        assert_eq!(v["result"]["result"]["resultType"], "complete");
        assert_eq!(v["result"]["result"]["content"][0]["text"], "hello");
        drop((frame, staged, prep, grant));
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn detailed_tool_error_cannot_bypass_memory_budget_uncredited() {
        let (prep, grant, pool, _) = fixture(8 << 10);
        let huge_errors: Vec<Value> = (0..200)
            .map(|i| serde_json::json!({"code": "e", "message": format!("problem-{i}-{}", "x".repeat(80))}))
            .collect();
        let detailed = ToolError::new("invalid-facts", 422, "200 checks failed")
            .hint("fix all errors")
            .data(serde_json::json!({ "errors": huge_errors }));
        let err = RetainedCallResult::from_outcome(Err(detailed), &prep, true, &err_ctx())
            .err()
            .expect("must refuse oversized detailed ToolError");
        assert_eq!(&*err.code, "budget-memory");
        assert!(
            err.data.is_none(),
            "must not attach unbounded original data"
        );
        drop((prep, grant));
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn sse_data_handles_crlf_as_single_line_break_and_uncredited_skips_bad_event_id() {
        let mut out = Vec::new();
        {
            let mut sse = SseData {
                writer: &mut out,
                prev_cr: false,
            };
            sse.write_all(b"a\r\nb\r").unwrap();
            sse.write_all(b"\nc\nd").unwrap();
        }
        assert_eq!(out, b"a\ndata: b\ndata: c\ndata: d");

        let frame = encode_uncredited_framing(
            &serde_json::json!({"ok": true}),
            Framing::Sse {
                event_id: Some("bad\nid"),
                retry: None,
            },
        );
        let s = String::from_utf8(frame.to_vec()).unwrap();
        assert!(
            !s.contains("id:"),
            "invalid event_id must not be emitted: {s:?}"
        );
    }

    #[test]
    fn bounded_why_empty_summary_enforces_2kib_cap_on_max_and_escape_heavy_inputs() {
        // 1. Plain ASCII at max individual field caps previously serialized to 2096 B > 2048 B.
        let ascii_steps: Vec<Value> = (0..8)
            .map(|i| {
                serde_json::json!({
                    "kind": "k".repeat(64),
                    "text": format!("step-{i}-{}", "a".repeat(256)),
                    "solutions": false,
                })
            })
            .collect();
        let ascii_input = serde_json::json!({
            "dataset": "d".repeat(256),
            "commit": u64::MAX,
            "empty": true,
            "complete": false,
            "message": "m".repeat(512),
            "steps": ascii_steps,
            "prefixes": { "ex": "http://example.org/" },
        });
        let ascii_summary = bounded_why_empty_summary(&ascii_input).expect("incomplete summary");
        let ascii_wire = serde_json::to_vec(&ascii_summary).unwrap();
        assert!(
            ascii_wire.len() <= MAX_WHY_EMPTY_SUMMARY_BYTES,
            "ASCII summary exceeded 2 KiB cap: {} bytes",
            ascii_wire.len()
        );
        assert_eq!(ascii_summary["complete"], false);

        // 2. Escape-heavy SPARQL literals (quotes, backslashes, control chars, UTF-8).
        let escaped_text = "\"\\\n\r\t\u{0000}\u{001f}é🚀".repeat(40);
        let escaped_steps: Vec<Value> = (0..8)
            .map(|_| {
                serde_json::json!({
                    "kind": "\"\\\u{0001}pattern".repeat(8),
                    "text": escaped_text,
                    "solutions": Value::Null,
                })
            })
            .collect();
        let escaped_input = serde_json::json!({
            "dataset": "\"\\\u{0000}ds".repeat(40),
            "commit": 42,
            "empty": Value::Null,
            "complete": false,
            "message": "\"\\\n\u{0000}msg".repeat(60),
            "steps": escaped_steps,
        });
        let escaped_summary =
            bounded_why_empty_summary(&escaped_input).expect("escape-heavy summary");
        let escaped_wire = serde_json::to_vec(&escaped_summary).unwrap();
        assert!(
            escaped_wire.len() <= MAX_WHY_EMPTY_SUMMARY_BYTES,
            "escape-heavy summary exceeded 2 KiB cap: {} bytes",
            escaped_wire.len()
        );
        assert_eq!(escaped_summary["complete"], false);
    }

    #[test]
    fn abandoned_task_poll_re_trims_grant_and_keeps_task_readable() {
        let (prep, grant, pool, _) = fixture(4 << 20);
        let retained = RetainedCallResult::from_outcome(
            Ok(Outcome::Text("small completed payload".to_string())),
            &prep,
            true,
            &err_ctx(),
        )
        .unwrap();
        drop(prep);
        retained.trim_grant();
        let pre_poll_granted = grant.granted();
        assert!(
            pre_poll_granted < (64 << 10),
            "expected trimmed task grant, got {pre_poll_granted}"
        );

        // 1. Early drop of `fresh_frame_budget` (e.g. cancelled before parking) re-trims grant.
        let budget = retained.fresh_frame_budget(Control::none()).unwrap();
        assert!(
            grant.granted() > pre_poll_granted,
            "fresh_frame_budget should grow grant to a granule during poll"
        );
        drop(budget);
        assert_eq!(
            grant.granted(),
            pre_poll_granted,
            "dropping abandoned TaskFrameBudget must restore pre-poll grant"
        );

        // 2. Closing a parked `StagedResponse::TaskCompleted` before `into_frame_or_error`
        //    also restores the pre-poll grant, and the retained task remains readable.
        let task = Task::new(
            "t-trim".to_string(),
            rmcp::model::TaskStatus::Completed,
            "2026-01-01T00:00:00Z".to_string(),
            "2026-01-01T00:00:00Z".to_string(),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let budget2 = retained
            .fresh_frame_budget(Control::with_cancel(cancel.clone()))
            .unwrap();
        assert!(grant.granted() > pre_poll_granted);
        let slot = ResponseSlot::open();
        slot.park(StagedResponse::TaskCompleted {
            id: RequestId::Number(11),
            task: Box::new(task.clone()),
            result: retained.clone(),
            frame_budget: budget2,
            cancel: cancel.clone(),
        });
        slot.close();
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(
            grant.granted(),
            pre_poll_granted,
            "closing parked TaskCompleted must restore pre-poll grant"
        );

        // Task remains readable via a subsequent poll and trims back after frame drop.
        let budget3 = retained.fresh_frame_budget(Control::none()).unwrap();
        let staged3 = StagedResponse::TaskCompleted {
            id: RequestId::Number(12),
            task: Box::new(task),
            result: retained.clone(),
            frame_budget: budget3,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let frame = staged3.into_frame_or_error(Framing::Json, 30.0);
        let v: Value = serde_json::from_slice(&frame).unwrap();
        assert_eq!(
            v["result"]["result"]["content"][0]["text"],
            "small completed payload"
        );
        drop(frame);
        assert_eq!(grant.granted(), pre_poll_granted);

        drop((retained, grant));
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn response_slot_cancel_hook_fires_only_on_close_or_already_closed() {
        let fired = Arc::new(AtomicBool::new(false));
        let slot = ResponseSlot::open();
        let f = fired.clone();
        assert!(slot.arm_cancel_hook(move || f.store(true, Ordering::SeqCst)));
        // Normal take() disarms the hook without running it.
        assert!(slot.take().is_none());
        assert!(!fired.load(Ordering::SeqCst));

        // Arming on an already-closed slot runs the hook immediately and returns false.
        let f2 = fired.clone();
        assert!(!slot.arm_cancel_hook(move || f2.store(true, Ordering::SeqCst)));
        assert!(fired.load(Ordering::SeqCst));

        // close() runs an armed hook on an open slot.
        let fired2 = Arc::new(AtomicBool::new(false));
        let slot2 = ResponseSlot::open();
        let f3 = fired2.clone();
        assert!(slot2.arm_cancel_hook(move || f3.store(true, Ordering::SeqCst)));
        slot2.close();
        assert!(fired2.load(Ordering::SeqCst));
    }
}
