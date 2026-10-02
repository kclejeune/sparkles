//! Tool execution errors: results with `isError: true`, a message naming the remedy,
//! and `_meta["io.github.kclejeune.sparkles/error"] = {code, status, budget?}`.

use serde_json::{Value, json};
use sparkles::error::{BudgetKind, Error};
use sparkles::schema::SchemaError;

/// `_meta` key of the error details.
pub const ERROR_META: &str = "io.github.kclejeune.sparkles/error";

#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    /// stable machine-readable code, e.g. `timeout`
    pub code: &'static str,
    /// the equivalent HTTP status (for logs and tests)
    pub status: u16,
    pub message: String,
    pub hint: Option<String>,
    /// the exceeded budget (`memory`, `rows`)
    pub budget: Option<&'static str>,
}

impl ToolError {
    pub fn new(code: &'static str, status: u16, message: impl Into<String>) -> ToolError {
        ToolError {
            code,
            status,
            message: message.into(),
            hint: None,
            budget: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> ToolError {
        self.hint = Some(hint.into());
        self
    }

    pub fn bad_argument(message: impl Into<String>) -> ToolError {
        ToolError::new("bad-argument", 400, message)
    }

    pub fn internal(request_id: &str) -> ToolError {
        ToolError::new(
            "internal",
            500,
            format!("internal error (request id {request_id})"),
        )
    }

    /// `"<message>\nHint: <hint>"`
    pub fn text(&self) -> String {
        match &self.hint {
            Some(h) => format!("{}\nHint: {h}", self.message),
            None => self.message.clone(),
        }
    }

    /// `{code, status, budget?}`
    pub fn meta(&self) -> Value {
        let mut m = json!({ "code": self.code, "status": self.status });
        if let Some(b) = self.budget {
            m["budget"] = b.into();
        }
        m
    }
}

/// What an engine error means for the call that got it.
pub struct ErrorContext<'a> {
    /// the call's timeout, in seconds
    pub timeout_secs: f64,
    /// the server's largest timeout, in seconds
    pub max_timeout_secs: f64,
    /// the dataset's prefix names (for syntax error hints)
    pub prefix_names: &'a [&'a str],
    pub request_id: &'a str,
}

/// `{seconds}` without a trailing `.0`.
pub fn secs(s: f64) -> String {
    format!("{s}")
}

const BUDGET_HINT: &str = "make the query more selective, avoid cartesian products (patterns without shared variables), aggregate with COUNT/GROUP BY instead of listing, add LIMIT inside subqueries";

impl ErrorContext<'_> {
    pub fn engine(&self, e: Error) -> ToolError {
        match e {
            Error::SparqlSyntax(_) => {
                let names: Vec<&str> = self.prefix_names.iter().take(20).copied().collect();
                let more = if self.prefix_names.len() > 20 { ", …" } else { "" };
                ToolError::new("syntax", 400, syntax_summary(&e.to_string())).hint(format!(
                    "check PREFIX names; predeclared prefixes: {}{more}",
                    names.join(", ")
                ))
            }
            Error::Invalid(m) => ToolError::new("syntax", 400, m),
            Error::Timeout => ToolError::new(
                "timeout",
                408,
                format!("query exceeded the {} s timeout", secs(self.timeout_secs)),
            )
            .hint(format!(
                "add LIMIT, start from a class or constant, restrict with GRAPH, check the plan with explain_query, or raise timeoutSeconds (max {})",
                secs(self.max_timeout_secs)
            )),
            Error::BudgetExceeded(b) => {
                let (code, budget) = match b.kind {
                    BudgetKind::Memory => ("budget-memory", "memory"),
                    BudgetKind::Rows => ("budget-rows", "rows"),
                    BudgetKind::ResultBytes => ("budget-result-bytes", "result-bytes"),
                    BudgetKind::DecompressedBytes => {
                        ("budget-decompressed-bytes", "decompressed-bytes")
                    }
                    BudgetKind::OutboundBytes => ("budget-outbound-bytes", "outbound-bytes"),
                    BudgetKind::ValidationWork => ("budget-validation-work", "validation-work"),
                };
                let mut t = ToolError::new(code, 507, b.to_string()).hint(BUDGET_HINT);
                t.budget = Some(budget);
                t
            }
            Error::Service(m) if m == "SERVICE is disabled" => {
                ToolError::new("service-disabled", 403, "SERVICE is disabled for MCP calls")
                    .hint("query the remote endpoint directly")
            }
            Error::Service(m) => ToolError::new("service-error", 502, format!("SERVICE error: {m}")),
            Error::TextUnavailable(m) => ToolError::new("text-unavailable", 503, m).hint(
                "retry later, or use FILTER(CONTAINS(LCASE(?x), \"…\")) with a narrow pattern",
            ),
            Error::Poisoned => ToolError::new(
                "write-failed",
                503,
                "writes are disabled until the server restarts; reads still work",
            ),
            Error::Unsupported(m) => ToolError::new("unsupported", 501, m),
            // SERVICE or LOAD without the server permission it needs
            Error::NotPermitted(m) => ToolError::new("forbidden", 403, m),
            Error::Rejected(r) => ToolError::new("validation-failed", 422, r.to_string()).hint(
                "nothing was written: change the update so that the data conforms to the dataset's shapes",
            ),
            Error::GuardMissing(m) => ToolError::new("write-failed", 503, m),
            Error::StorageFull(m) => ToolError::new("storage-full", 507, m),
            Error::Cancelled => ToolError::new("internal", 500, "call cancelled: server shutting down"),
            e => {
                tracing::error!(request_id = self.request_id, "MCP tool call failed: {e}");
                ToolError::internal(self.request_id)
            }
        }
    }

    pub fn schema(&self, e: SchemaError) -> ToolError {
        match e {
            SchemaError::NoSuchGraph(g) => {
                ToolError::new("unknown-graph", 404, format!("no graph <{g}>"))
                    .hint("graph=union covers all graphs")
            }
            SchemaError::Timeout { phase } => ToolError::new(
                "timeout",
                408,
                format!(
                    "schema discovery exceeded the {} s timeout while {phase}",
                    secs(self.timeout_secs)
                ),
            )
            .hint("narrow graph to one named graph, or use sparql_query with GROUP BY"),
            SchemaError::Cancelled => self.engine(Error::Cancelled),
            e @ SchemaError::TooManyEntries { .. } => {
                ToolError::new("too-many-entries", 413, e.to_string())
                    .hint("use sparql_query with GROUP BY")
            }
            SchemaError::Store(e) => self.engine(e),
        }
    }
}

/// "SPARQL syntax error at line L, column C: …" (at most about 160 characters) from the
/// parser's message ("SPARQL syntax error: error at L:C: expected one of …").
pub fn syntax_summary(msg: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?s)error at (\d+):(\d+):\s*(.*)").unwrap()
    });
    const MAX: usize = 160;
    let Some(c) = RE.captures(msg) else {
        return msg.to_string();
    };
    let summary = format!(
        "SPARQL syntax error at line {}, column {}: {}",
        &c[1],
        &c[2],
        c[3].trim()
    );
    if summary.chars().count() <= MAX {
        return summary;
    }
    summary
        .chars()
        .take(MAX)
        .collect::<String>()
        .trim_end()
        .to_string()
        + "…"
}
