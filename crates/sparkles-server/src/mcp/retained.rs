//! Physical MCP bodies and complete wire frames under an existing real receiver.
use axum::body::Bytes;
use rmcp::model::{RetainedFraming, RetainedToolBody, RetainedToolResult, ServerJsonRpcMessage};
use sparkles::{
    error::{Error, Result},
    sparql::output::{NativeBudget, NativeReservation, OwnedBytes, OwnedJson, OwnedText},
};
use std::{
    alloc::Layout,
    io::{self, Write},
    sync::{Arc, atomic::AtomicUsize},
};

struct Body {
    text: OwnedText,
    structured: Option<OwnedJson>,
    meta: Option<OwnedJson>,
    receiver: NativeBudget,
    _credit: NativeReservation,
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
// bytes1.12.1::Bytes::from_owner allocates exactly this repr(C) layout.
// Its own retained owner supplies lifetime/drop; we never access private fields.
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

/// Reuse actual immutable payload owners and an explicitly supplied existing
/// receiver. This creates no graph authority, allowance or replacement context.
pub(super) fn result(
    text: OwnedText,
    structured: Option<OwnedJson>,
    meta: Option<OwnedJson>,
    receiver: NativeBudget,
) -> Result<RetainedToolResult> {
    receiver.check()?;
    let credit = receiver.reserve(arc_bytes::<Body>()?)?;
    let body = Arc::new(Body {
        text,
        structured,
        meta,
        receiver,
        _credit: credit,
    });
    body.receiver.check()?;
    RetainedToolResult::try_new(body).map_err(Error::Io)
}

// SSE requires each raw line of JSON data to carry its own data prefix. The
// ordinary compact JSON case stays byte-identical to the upstream encoder.
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
impl RetainedToolBody for Body {
    fn text(&self) -> &str {
        self.text.as_str()
    }
    fn structured_content(&self) -> Option<&serde_json::value::RawValue> {
        self.structured.as_ref().map(OwnedJson::as_raw)
    }
    fn meta(&self) -> Option<&serde_json::value::RawValue> {
        self.meta.as_ref().map(OwnedJson::as_raw)
    }
    fn encode_frame(
        &self,
        message: &ServerJsonRpcMessage,
        framing: RetainedFraming<'_>,
    ) -> io::Result<Bytes> {
        if let RetainedFraming::Sse {
            event_id: Some(id), ..
        } = framing
        {
            if id.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0)) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid SSE event ID",
                ));
            }
        }
        let bytes =
            self.receiver
                .encode_bytes(|writer| {
                    match framing {
                        RetainedFraming::Json => serde_json::to_writer(&mut *writer, message)
                            .map_err(io::Error::other)?,
                        RetainedFraming::Sse { event_id, retry } => {
                            writer.write_all(b"data: ")?;
                            serde_json::to_writer(
                                SseData {
                                    writer: &mut *writer,
                                },
                                message,
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
                })
                .map_err(io::Error::other)?;
        let metadata = u64::try_from(std::mem::size_of::<BytesOwnerLayout<FrameOwner>>())
            .map_err(|_| io::Error::other(overflow()))?;
        let credit = self.receiver.reserve(metadata).map_err(io::Error::other)?;
        self.receiver.check().map_err(io::Error::other)?;
        let frame = Bytes::from_owner(FrameOwner {
            bytes,
            _credit: credit,
        });
        self.receiver.check().map_err(io::Error::other)?;
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ServerResult;
    use sparkles::{access::Preparation, sparql::pool::QueryPool, task::Control};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn fixture() -> (
        NativeBudget,
        Preparation,
        Arc<sparkles::sparql::pool::PoolGrant>,
        Arc<QueryPool>,
        Arc<AtomicBool>,
    ) {
        let pool = QueryPool::new(16 << 20);
        let grant = Arc::new(pool.empty_grant());
        let cancel = Arc::new(AtomicBool::new(false));
        let preparation =
            Preparation::with_grant(Control::with_cancel(cancel.clone()), 4 << 20, grant.clone())
                .unwrap();
        let budget = NativeBudget::from_preparation(&preparation).unwrap();
        (budget, preparation, grant, pool, cancel)
    }
    fn message(budget: &NativeBudget) -> ServerJsonRpcMessage {
        let text = budget.text(&"x".repeat(64 << 10)).unwrap();
        let structured = budget
            .json(&serde_json::json!({"rows":[["retained"]]}))
            .unwrap();
        let meta = budget
            .json(&serde_json::json!({"source":"same actual preparation"}))
            .unwrap();
        let result = result(text, Some(structured), Some(meta), budget.clone()).unwrap();
        ServerJsonRpcMessage::response(ServerResult::RetainedToolResult(result), 9.into())
    }

    #[test]
    fn complete_mcp_frame_slices_retain_real_bytes_and_receiving_credit_until_final_free() {
        let (budget, preparation, grant, pool, _) = fixture();
        let message = message(&budget);
        let body_held = grant.used();
        let frame = message
            .retained_frame(RetainedFraming::Json)
            .unwrap()
            .unwrap();
        assert!(grant.used() >= body_held + frame.len() as u64);
        let expected: serde_json::Value = serde_json::from_slice(&frame).unwrap();
        assert_eq!(
            expected["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .len(),
            64 << 10
        );
        assert_eq!(
            expected["result"]["structuredContent"]["rows"][0][0],
            "retained"
        );
        let last = frame.slice(1..frame.len() - 1);
        drop(frame);
        drop(message);
        drop(budget);
        drop(preparation);
        assert!(grant.used() >= last.len() as u64);
        assert_eq!(last[0], b'"');
        drop(last);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn real_mcp_frame_cancel_and_invalid_event_id_publish_no_unpaid_fallback() {
        let (budget, preparation, grant, pool, cancel) = fixture();
        let message = message(&budget);
        let before = grant.used();
        let error = message
            .retained_frame(RetainedFraming::Sse {
                event_id: Some("bad\nid"),
                retry: None,
            })
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(grant.used(), before);
        cancel.store(true, Ordering::SeqCst);
        let error = message
            .retained_frame(RetainedFraming::Json)
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            error.get_ref().and_then(|e| e.downcast_ref::<Error>()),
            Some(Error::Cancelled)
        ));
        assert_eq!(grant.used(), before);
        drop(message);
        drop(budget);
        drop(preparation);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn sse_data_framing_handles_raw_whitespace_without_intermediate_owned_strings() {
        let raw = b"{\r\n\"actual\": [\n  1, 2\r\n]}";
        // This independent application fixture checks only the streaming line
        // transformation. The actual credited frame path is covered above.
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"data: ");
        let mut data = SseData {
            writer: &mut encoded,
        };
        for chunk in raw.chunks(3) {
            data.write_all(chunk).unwrap();
        }
        encoded.extend_from_slice(b"\n\n");
        let joined = std::str::from_utf8(&encoded)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&joined).unwrap(),
            serde_json::json!({"actual":[1,2]})
        );
    }
}
