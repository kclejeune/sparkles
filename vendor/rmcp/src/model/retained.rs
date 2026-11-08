//! Retained outgoing tool bodies. Incoming wire parsing stays with CallToolResult.
use super::{ResultType, ServerJsonRpcMessage};
use bytes::Bytes;
use serde::{
    Deserialize, Serialize,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::value::RawValue;
use std::{fmt, io, sync::Arc, time::Duration};

/// The actual wire framing requested by the transport. All members borrow the
/// current event; encoding must retain any new allocation through its final free.
#[derive(Clone, Copy, Debug)]
pub enum RetainedFraming<'a> {
    Json,
    Sse {
        event_id: Option<&'a str>,
        retry: Option<Duration>,
    },
}

/// An actual immutable physical body and its real receiving account. Implementors
/// must quote before allocating, retain payload and credit together, and share
/// physical storage on clone. Encoding refusal must never use an unpaid fallback.
pub trait RetainedToolBody: Send + Sync + 'static {
    fn text(&self) -> &str;
    fn structured_content(&self) -> Option<&RawValue> {
        None
    }
    fn meta(&self) -> Option<&RawValue> {
        None
    }
    fn encode_frame(
        &self,
        message: &ServerJsonRpcMessage,
        framing: RetainedFraming<'_>,
    ) -> io::Result<Bytes>;
}

/// An outgoing tool result whose clones retain the same actual body. The
/// ordinary incoming CallToolResult representation and wire shape are unchanged.
#[derive(Clone)]
pub struct RetainedToolResult {
    pub result_type: Option<ResultType>,
    pub is_error: Option<bool>,
    body: Arc<dyn RetainedToolBody>,
}
impl RetainedToolResult {
    /// The caller must admit this concrete body's Arc before constructing it.
    pub fn try_new(body: Arc<dyn RetainedToolBody>) -> io::Result<Self> {
        if body
            .meta()
            .is_some_and(|meta| !meta.get().trim_start().starts_with('{'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retained tool metadata must be an object",
            ));
        }
        Ok(Self {
            result_type: Some(ResultType::COMPLETE),
            is_error: Some(false),
            body,
        })
    }

    /// Borrow immutable payload bytes for an explicitly admitted receiving copy.
    /// These views create no owner, allocation allowance or graph authority.
    pub fn text(&self) -> &str {
        self.body.text()
    }
    pub fn structured_content(&self) -> Option<&RawValue> {
        self.body.structured_content()
    }
    pub fn meta(&self) -> Option<&RawValue> {
        self.body.meta()
    }

    pub fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = Some(is_error);
        self
    }

    pub fn encode_frame(
        &self,
        message: &ServerJsonRpcMessage,
        framing: RetainedFraming<'_>,
    ) -> io::Result<Bytes> {
        self.body.encode_frame(message, framing)
    }
}
impl fmt::Debug for RetainedToolResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedToolResult")
            .field("result_type", &self.result_type)
            .field("is_error", &self.is_error)
            .field("text_bytes", &self.body.text().len())
            .field(
                "structured_bytes",
                &self.body.structured_content().map(|v| v.get().len()),
            )
            .finish_non_exhaustive()
    }
}
impl PartialEq for RetainedToolResult {
    fn eq(&self, other: &Self) -> bool {
        self.result_type == other.result_type
            && self.is_error == other.is_error
            && (Arc::ptr_eq(&self.body, &other.body)
                || (self.body.text() == other.body.text()
                    && self.body.structured_content().map(RawValue::get)
                        == other.body.structured_content().map(RawValue::get)
                    && self.body.meta().map(RawValue::get) == other.body.meta().map(RawValue::get)))
    }
}
struct TextContent<'a>(&'a str);
impl Serialize for TextContent<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut content = serializer.serialize_map(Some(2))?;
        content.serialize_entry("type", "text")?;
        content.serialize_entry("text", self.0)?;
        content.end()
    }
}
struct Content<'a>(&'a str);
impl Serialize for Content<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut content = serializer.serialize_seq(Some(1))?;
        content.serialize_element(&TextContent(self.0))?;
        content.end()
    }
}
impl Serialize for RetainedToolResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut result = serializer.serialize_map(None)?;
        if let Some(result_type) = &self.result_type {
            result.serialize_entry("resultType", result_type)?;
        }
        result.serialize_entry("content", &Content(self.body.text()))?;
        if let Some(structured) = self.body.structured_content() {
            result.serialize_entry("structuredContent", structured)?;
        }
        if let Some(is_error) = self.is_error {
            result.serialize_entry("isError", &is_error)?;
        }
        if let Some(meta) = self.body.meta() {
            result.serialize_entry("_meta", meta)?;
        }
        result.end()
    }
}
impl<'de> Deserialize<'de> for RetainedToolResult {
    fn deserialize<D: serde::Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
        Err(serde::de::Error::custom(
            "retained tool result is outgoing only",
        ))
    }
}
#[cfg(feature = "schemars")]
impl schemars::JsonSchema for RetainedToolResult {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        <super::CallToolResult as schemars::JsonSchema>::schema_name()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <super::CallToolResult as schemars::JsonSchema>::json_schema(generator)
    }
}

impl ServerJsonRpcMessage {
    /// Encode a retained result without falling back on an unpaid native copy.
    pub fn retained_frame(&self, framing: RetainedFraming<'_>) -> Option<io::Result<Bytes>> {
        match self {
            Self::Response(response) => match &response.result {
                super::ServerResult::RetainedToolResult(result) => {
                    Some(result.encode_frame(self, framing))
                }
                super::ServerResult::GetTaskResult(task) => match &task.task.payload {
                    super::TaskPayload::RetainedCompleted { result } => {
                        Some(result.encode_frame(self, framing))
                    }
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CallToolResult, ContentBlock, RequestId, ServerResult};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Explicit application-owned fixtures test wire/model behavior only. The
    // product's original-budget and real free-point controls are separate tests.
    struct Fixture {
        text: String,
        structured: Option<Box<RawValue>>,
        meta: Option<Box<RawValue>>,
        dropped: Arc<AtomicUsize>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl RetainedToolBody for Fixture {
        fn text(&self) -> &str {
            &self.text
        }
        fn structured_content(&self) -> Option<&RawValue> {
            self.structured.as_deref()
        }
        fn meta(&self) -> Option<&RawValue> {
            self.meta.as_deref()
        }
        fn encode_frame(
            &self,
            _: &ServerJsonRpcMessage,
            _: RetainedFraming<'_>,
        ) -> io::Result<Bytes> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture encoding refusal",
            ))
        }
    }
    fn fixture(text: &str, structured: Option<Value>) -> Arc<Fixture> {
        Arc::new(Fixture {
            text: text.into(),
            structured: structured.map(|value| RawValue::from_string(value.to_string()).unwrap()),
            meta: None,
            dropped: Arc::default(),
        })
    }

    #[test]
    fn retained_text_preserves_exact_current_and_legacy_wire_shape() {
        let text = "quotes\"\nUnicode λ\t";
        let body = fixture(text, None);
        let retained = RetainedToolResult::try_new(body).unwrap();
        let ordinary = CallToolResult::success(vec![ContentBlock::text(text)]);
        assert_eq!(
            serde_json::to_vec(&retained).unwrap(),
            serde_json::to_vec(&ordinary).unwrap()
        );
        let mut retained = ServerResult::RetainedToolResult(retained);
        let mut ordinary = ServerResult::CallToolResult(ordinary);
        retained.strip_result_type_for_legacy_peer();
        ordinary.strip_result_type_for_legacy_peer();
        assert_eq!(
            serde_json::to_vec(&retained).unwrap(),
            serde_json::to_vec(&ordinary).unwrap()
        );
    }

    #[test]
    fn retained_structured_null_and_object_preserve_the_mirrored_text() {
        for value in [Value::Null, json!({"rows":[[null,"x\n"]],"vars":["v"]})] {
            let text = value.to_string();
            let retained =
                RetainedToolResult::try_new(fixture(&text, Some(value.clone()))).unwrap();
            let ordinary = CallToolResult::structured(value);
            assert_eq!(
                serde_json::to_vec(&retained).unwrap(),
                serde_json::to_vec(&ordinary).unwrap()
            );
        }
    }

    #[test]
    fn retained_clones_share_the_same_body_until_the_final_owner_drops() {
        let body = fixture(&"x".repeat(64 << 10), None);
        let dropped = body.dropped.clone();
        let weak = Arc::downgrade(&body);
        let retained = RetainedToolResult::try_new(body).unwrap();
        let last = retained.clone();
        assert!(Arc::ptr_eq(&retained.body, &last.body));
        assert_eq!(retained.body.text().as_ptr(), last.body.text().as_ptr());
        drop(retained);
        assert!(weak.upgrade().is_some());
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(last);
        assert!(weak.upgrade().is_none());
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retained_incoming_wire_uses_the_ordinary_type_and_encoder_refusal_is_not_fallback() {
        let retained = RetainedToolResult::try_new(fixture("body", None)).unwrap();
        let wire = serde_json::to_vec(&retained).unwrap();
        let incoming: ServerResult = serde_json::from_slice(&wire).unwrap();
        assert!(matches!(incoming, ServerResult::CallToolResult(_)));
        let message = ServerJsonRpcMessage::response(
            ServerResult::RetainedToolResult(retained),
            RequestId::Number(1),
        );
        let error = message
            .retained_frame(RetainedFraming::Json)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn retained_metadata_rejects_non_objects_before_publication() {
        let body = Arc::new(Fixture {
            text: "body".into(),
            structured: None,
            meta: Some(RawValue::from_string("null".into()).unwrap()),
            dropped: Arc::default(),
        });
        assert_eq!(
            RetainedToolResult::try_new(body).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn retained_completed_task_preserves_exact_wire_and_ordinary_incoming_representation() {
        use crate::model::{DetailedTask, GetTaskResult, Task, TaskPayload, TaskStatus};
        let ordinary = CallToolResult::structured(json!({"actual":"completed payload"}));
        let text = serde_json::to_string(ordinary.structured_content.as_ref().unwrap()).unwrap();
        let body = fixture(&text, ordinary.structured_content.clone());
        let retained = RetainedToolResult::try_new(body.clone()).unwrap();
        let base = Task::new("task-id", TaskStatus::Working, "created", "updated");
        let expected = GetTaskResult::new(DetailedTask::new(
            base.clone(),
            TaskPayload::Completed {
                result: serde_json::to_value(&ordinary)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .clone(),
            },
        ));
        let actual = GetTaskResult::new(DetailedTask::new(
            base,
            TaskPayload::RetainedCompleted { result: retained },
        ));
        assert_eq!(actual.task.status(), TaskStatus::Completed);
        let encoded = serde_json::to_vec(&actual).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&encoded).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let parsed: GetTaskResult = serde_json::from_slice(&encoded).unwrap();
        assert!(matches!(parsed.task.payload, TaskPayload::Completed { .. }));
        let last = actual.clone();
        if let TaskPayload::RetainedCompleted { result } = &last.task.payload {
            assert_eq!(result.text().as_ptr(), body.text.as_ptr());
        } else {
            panic!("retained poll clone must retain actual body");
        }
        let message = ServerJsonRpcMessage::response(ServerResult::GetTaskResult(actual), 3.into());
        assert_eq!(
            message
                .retained_frame(RetainedFraming::Json)
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        drop(message);
        assert_eq!(body.dropped.load(Ordering::SeqCst), 0);
        drop(last);
        assert_eq!(Arc::strong_count(&body), 1);
    }
}
