//! Bounded debug diagnostics for Ask stages. Timings are inclusive wall time, so a
//! grounding total contains its tool stages and tools contain nested embedding work.
//! These diagnostics do not change the public usage accounting.

use super::Ground;
use crate::mcp::errors::ToolError;
use serde_json::Value;
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tracing::Span;

pub(crate) const TARGET: &str = "sparkles::ask::profile";

#[derive(Clone, Copy)]
struct Scope {
    id: u64,
    context_tokens: Option<u64>,
}

thread_local! {
    static SCOPE: Cell<Option<Scope>> = const { Cell::new(None) };
}
static NEXT_PIPELINE: AtomicU64 = AtomicU64::new(1);

/// Ask is synchronous. Restore the caller's diagnostic scope after a nested ask
/// or unwinding, and prevent this guard from moving to another thread.
pub(super) struct Pipeline {
    previous: Option<Scope>,
    id: Option<u64>,
    _not_send: PhantomData<Rc<()>>,
}

impl Pipeline {
    pub(super) fn id(&self) -> Option<u64> {
        self.id
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        let _ = SCOPE.try_with(|scope| scope.set(self.previous));
    }
}

pub(super) fn pipeline() -> Pipeline {
    let scope = if tracing::enabled!(target: TARGET, tracing::Level::DEBUG) {
        // Stop assigning identifiers on exhaustion instead of reusing one.
        NEXT_PIPELINE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .ok()
            .map(|id| Scope {
                id,
                context_tokens: None,
            })
    } else {
        None
    };
    Pipeline {
        previous: SCOPE.with(|current| current.replace(scope)),
        id: scope.map(|scope| scope.id),
        _not_send: PhantomData,
    }
}

pub(super) fn context_tokens(tokens: u64) {
    SCOPE.with(|current| {
        if let Some(mut scope) = current.get() {
            scope.context_tokens = Some(tokens);
            current.set(Some(scope));
        }
    });
}

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    Ground,
    SchemaCommon,
    SchemaQuestion,
    Examples,
    Linker,
    CommunityRecall,
    QueryCheck,
    QueryExecution,
    ExampleQueryEmbedding,
    ExampleDocumentEmbedding,
    LinkerVectorSearch,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::Ground => "ground",
            Self::SchemaCommon => "schema_common",
            Self::SchemaQuestion => "schema_question",
            Self::Examples => "examples",
            Self::Linker => "linker",
            Self::CommunityRecall => "community_recall",
            Self::QueryCheck => "query_check",
            Self::QueryExecution => "query_execution",
            Self::ExampleQueryEmbedding => "example_query_embedding",
            Self::ExampleDocumentEmbedding => "example_document_embedding",
            Self::LinkerVectorSearch => "linker_vector_search",
        }
    }
}

struct Timer {
    started: Option<Instant>,
    span: Span,
    stage: Stage,
    pipeline_id: Option<u64>,
    context_tokens: Option<u64>,
    inputs: Option<usize>,
}

impl Timer {
    fn new(stage: Stage, context_tokens: Option<u64>, inputs: Option<usize>) -> Self {
        let enabled = tracing::enabled!(target: TARGET, tracing::Level::DEBUG);
        let scope = if enabled { SCOPE.with(Cell::get) } else { None };
        let pipeline_id = scope.map(|scope| scope.id);
        let context_tokens =
            context_tokens.or_else(|| scope.and_then(|scope| scope.context_tokens));
        Self {
            started: enabled.then(Instant::now),
            span: if enabled {
                tracing::debug_span!(target: TARGET, "ask.stage", stage = stage.name(), pipeline_id, context_tokens, inputs)
            } else {
                Span::none()
            },
            stage,
            pipeline_id,
            context_tokens,
            inputs,
        }
    }

    fn finish(&self, outcome: &'static str, error_code: &'static str, commit: Option<u64>) {
        if let Some(started) = self.started {
            tracing::debug!(target: TARGET, parent: &self.span,
                stage = self.stage.name(), calls = 1_u64,
                pipeline_id = self.pipeline_id, context_tokens = self.context_tokens,
                inputs = self.inputs,
                elapsed_ns = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                outcome, error_code, commit,
                "Ask stage completed");
        }
    }
}

/// Tool error codes are static, but an explicit set keeps the diagnostic vocabulary
/// stable when a tool adds a new error. Messages and error details never enter it.
fn error_code(code: &str) -> &'static str {
    match code {
        "timeout" => "timeout",
        "cancelled" => "cancelled",
        "forbidden" => "forbidden",
        "budget-exceeded" => "budget-exceeded",
        "bad-argument" => "bad-argument",
        "not-found" => "not-found",
        _ => "other",
    }
}

pub(super) fn tool(
    stage: Stage,
    call: impl FnOnce() -> Result<Value, ToolError>,
) -> Result<Value, ToolError> {
    let timer = Timer::new(stage, None, None);
    let _entered = timer.span.enter();
    let result = call();
    match &result {
        Ok(v) => timer.finish(
            if matches!(stage, Stage::QueryCheck) && v["ok"] == false {
                "rejected"
            } else {
                "ok"
            },
            "none",
            v["commit"].as_u64(),
        ),
        Err(e) => timer.finish("error", error_code(e.code), None),
    }
    result
}

pub(super) fn ground(context_tokens: u64, call: impl FnOnce() -> Ground) -> Ground {
    let timer = Timer::new(Stage::Ground, Some(context_tokens), None);
    let _entered = timer.span.enter();
    let ground = call();
    timer.finish(
        if ground.ambiguous.is_some() {
            "ambiguous"
        } else {
            "ok"
        },
        "none",
        None,
    );
    ground
}

/// Nested work is inclusive of embedding admission, caches and any provider/local
/// model work. A vector search also includes query execution. Neither is remote
/// inference time, and neither is added to Ask's role-step `modelMs`.
pub(crate) fn nested<T, E>(
    stage: Stage,
    commit: u64,
    inputs: usize,
    call: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let timer = Timer::new(stage, None, Some(inputs));
    let _entered = timer.span.enter();
    let result = call();
    timer.finish(
        if result.is_ok() { "ok" } else { "error" },
        if result.is_ok() {
            "none"
        } else {
            "nested-work-failed"
        },
        Some(commit),
    );
    result
}

pub(super) fn execution<T>(
    call: impl FnOnce() -> Result<(Value, T), ToolError>,
) -> Result<(Value, T), ToolError> {
    let timer = Timer::new(Stage::QueryExecution, None, None);
    let _entered = timer.span.enter();
    let result = call();
    match &result {
        Ok((v, _)) => timer.finish("ok", "none", v["commit"].as_u64()),
        Err(e) => timer.finish("error", error_code(e.code), None),
    }
    result
}
