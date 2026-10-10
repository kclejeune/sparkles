//! `share_query` (C18 §9.1): a link that opens a checked query in a new tab of the
//! UI's query page, with the question and the explanation above it.
//!
//! The query is checked with `check_query` and never run. The arguments travel in the
//! link's fragment as base64url JSON, so they never reach a server log, and the UI
//! checks the query again against the viewer's own view before the viewer runs it.

use super::errors::ToolError;
use super::tools::{Tools, parse};
use super::{Outcome, render::Prefixes};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The largest payload a link carries, before base64.
pub const MAX_PAYLOAD: usize = 32 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ShareArgs {
    dataset: Option<String>,
    query: String,
    question: Option<String>,
    explanation: Option<String>,
    assumptions: Option<Vec<String>>,
    branch: Option<String>,
    at_commit: Option<u64>,
}

impl Tools<'_> {
    /// The base URL links point at: `--ui-url` or `server.public_url`, else the HTTP
    /// request's scheme and host.
    fn ui_base(&self) -> Option<String> {
        if let Some(u) = &self.cfg().ui_url {
            return Some(u.trim_end_matches('/').to_string());
        }
        let h = self.call.headers.as_ref()?;
        Some(crate::http::sd::base_url(&self.server.state, h))
    }

    pub(crate) fn share_query(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: ShareArgs = parse(args)?;
        let chars = |s: &Option<String>| s.as_ref().map_or(0, |s| s.chars().count());
        if a.query.trim().is_empty() {
            return Err(ToolError::bad_argument("query must not be empty"));
        }
        if a.query.chars().count() > 65536 {
            return Err(ToolError::bad_argument(
                "query must be at most 65536 characters",
            ));
        }
        if chars(&a.question) > 2000 {
            return Err(ToolError::bad_argument(
                "question must be at most 2000 characters",
            ));
        }
        if chars(&a.explanation) > 400 {
            return Err(ToolError::bad_argument(
                "explanation must be at most 400 characters",
            ));
        }
        if let Some(x) = &a.assumptions
            && (x.len() > 5 || x.iter().any(|s| s.chars().count() > 400))
        {
            return Err(ToolError::bad_argument(
                "assumptions: at most 5, each at most 400 characters",
            ));
        }
        if chars(&a.branch) > 200 {
            return Err(ToolError::bad_argument(
                "branch must be at most 200 characters",
            ));
        }
        let Some(base) = self.ui_base() else {
            return Err(ToolError::new(
                "unsupported",
                501,
                "this server does not know the address of its UI",
            )
            .hint("start sparkles mcp with --ui-url"));
        };
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = super::tools::dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, self.cfg().default_timeout().as_secs_f64());
        // an update is refused; a query that does not parse is shared with its issues
        if let Err(e) = self.parse_query(&a.query, &prefix_map, &ctx)
            && e.code == "not-a-query"
        {
            return Err(e.hint("share_query opens queries only"));
        }
        let mut check = Map::new();
        check.insert("dataset".into(), ds.name.clone().into());
        check.insert("query".into(), a.query.clone().into());
        if let Some(c) = a.at_commit {
            check.insert("atCommit".into(), c.into());
        }
        let checked = match self.check_query(check)? {
            Outcome::Structured(v) => v,
            Outcome::Text(_) => json!({}),
        };
        let mut payload = json!({ "dataset": ds.name, "query": a.query });
        if let Some(q) = &a.question {
            payload["question"] = q.clone().into();
        }
        if let Some(e) = &a.explanation {
            payload["explanation"] = e.clone().into();
        }
        if let Some(x) = &a.assumptions {
            payload["assumptions"] = json!(x);
        }
        if let Some(b) = &a.branch {
            payload["branch"] = b.clone().into();
        }
        if let Some(c) = a.at_commit {
            payload["atCommit"] = c.into();
        }
        let bytes = serde_json::to_vec(&payload).unwrap_or_default();
        if bytes.len() > MAX_PAYLOAD {
            return Err(ToolError::new(
                "too-large",
                413,
                format!(
                    "the link would carry {} bytes, more than {MAX_PAYLOAD}",
                    bytes.len()
                ),
            )
            .hint("shorten the explanation, or ask the person to paste the query"));
        }
        let ds_param: String = form_urlencoded::byte_serialize(ds.name.as_bytes()).collect();
        let url = format!(
            "{base}/ui/query?ds={ds_param}#ask={}",
            URL_SAFE_NO_PAD.encode(&bytes)
        );
        let mut out = json!({
            "url": url,
            "dataset": ds.name,
            "ok": checked["ok"],
            "issues": checked["issues"],
        });
        if let Some(c) = checked.get("commit") {
            out["commit"] = c.clone();
        }
        if let Some(p) = checked.get("prefixes") {
            out["prefixes"] = p.clone();
        }
        Ok(Outcome::Structured(out))
    }
}
