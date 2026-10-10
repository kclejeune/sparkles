//! The stricter per-agent policy of C18 §8.8: with `conversationFacts: "review"` in the
//! agent's entry of `memory.json`, an `assert_facts` call of that agent on `main` runs on
//! its branch `proposals.{agent}.inbox` instead, which is created when needed, and the
//! result says so.

use crate::auth::Principal;
use crate::mcp::errors::ToolError;
use crate::mcp::tools::Tools;
use crate::mcp::{Call, McpServer, Outcome};
use serde_json::{Map, Value, json};
use sparkles::branch::MAIN;

/// The agent of `memory.json` whose policy is `review` and that `p` is: its user name or
/// one of its roles.
fn reviewed_agent(p: &Principal, settings: &crate::assist::MemorySettings) -> Option<String> {
    use crate::assist::ConversationFacts;
    let c = p.caller();
    std::iter::once(c.user.clone())
        .flatten()
        .chain(c.roles.iter().cloned())
        .find(|n| {
            settings
                .agents
                .get(n)
                .is_some_and(|a| a.conversation_facts == ConversationFacts::Review)
        })
}

/// The branch of an agent's held conversation facts.
pub(crate) fn inbox_branch(agent: &str) -> String {
    format!("proposals.{agent}.inbox")
}

/// For `assert_facts` on `main` by an agent under the `review` policy: the call on its
/// inbox branch, created when needed, and the notice for its result.
pub(crate) fn redirect(
    server: &McpServer,
    args: &Map<String, Value>,
    call: &Call,
) -> Result<Option<(Call, String)>, ToolError> {
    if call.principal.branch.as_deref().is_some_and(|b| b != MAIN) {
        return Ok(None);
    }
    let name = args.get("dataset").and_then(Value::as_str);
    let Ok(ds) = server.dataset(&call.principal, name) else {
        // the tool reports the unknown dataset itself
        return Ok(None);
    };
    let settings = crate::assist::memory_settings(&server.state, &ds);
    let Some(agent) = reviewed_agent(&call.principal, &settings) else {
        return Ok(None);
    };
    let branch = inbox_branch(&agent);
    if !sparkles::branch::valid_name(&branch) {
        return Ok(None);
    }
    if server.state.branch_dataset(&ds, &branch).is_err() {
        let t = Tools { server, call };
        let mut a = Map::new();
        a.insert("dataset".into(), ds.name.clone().into());
        a.insert("name".into(), branch.clone().into());
        a.insert(
            "note".into(),
            format!("Conversation facts of {agent} held for review").into(),
        );
        match t.create_branch(a) {
            Ok(_) => {}
            // created meanwhile by another call
            Err(e) if server.state.branch_dataset(&ds, &branch).is_ok() => {
                tracing::debug!("{branch}: {}", e.message);
            }
            Err(e) => return Err(e),
        }
    }
    let notice = format!(
        "conversationFacts is review for {agent}: the facts were written to branch {branch}, not main, for a person to review. Pass branch \"{branch}\" to recall to read them."
    );
    Ok(Some((
        Call {
            arrived: call.arrived,
            cancel: call.cancel.clone(),
            request_id: call.request_id.clone(),
            principal: call.principal.clone().on_branch(Some(&branch)),
            headers: call.headers.clone(),
            held: call.held.clone(),
        },
        notice,
    )))
}

/// The result of a redirected call with its notice.
pub(crate) fn with_notice(
    out: Result<Outcome, ToolError>,
    notice: &str,
) -> Result<Outcome, ToolError> {
    match out {
        Ok(Outcome::Structured(Value::Object(mut m))) => {
            m.insert("notice".into(), json!(notice));
            Ok(Outcome::Structured(Value::Object(m)))
        }
        other => other,
    }
}
