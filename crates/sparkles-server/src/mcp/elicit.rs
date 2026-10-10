//! Elicitation for ambiguous candidates (C18 §9.5): when `assert_facts` fails with
//! `possible-duplicate` and the client declared elicitation, the call answers an
//! `input_required` result (SEP-2322) that asks the person to pick, for each new
//! entity, one of the existing candidates or "a new entity". The client retries the call
//! with the answers, and the server applies them to the original arguments: a chosen
//! candidate replaces the entity's key in the facts, and "a new entity" lists every
//! candidate in `distinctFrom`. The choices are checked against the candidates the
//! retried call finds, never against the echoed `requestState`.

use super::errors::ToolError;
use super::{Call, McpServer, Outcome};
use rmcp::model::{InputRequest, InputRequests, InputRequiredResult, InputResponses};
use serde_json::{Map, Value, json};

/// The `requestState` of the elicitation, an opaque marker the server does not trust.
const STATE: &str = "sparkles.possible-duplicate.v1";
/// The key of the one elicitation request.
const REQUEST: &str = "entities";
/// The choice that keeps the new entity.
const NEW: &str = "new";

/// A new entity that may duplicate existing ones.
struct Duplicate {
    index: usize,
    key: String,
    candidates: Vec<String>,
}

/// The possible duplicates of a failed `assert_facts`.
fn duplicates(e: &ToolError) -> Vec<Duplicate> {
    let Some(errors) = e
        .data
        .as_ref()
        .and_then(|d| d.get("errors"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    errors
        .iter()
        .filter(|p| p["code"] == "possible-duplicate")
        .filter_map(|p| {
            let index = p["at"]
                .as_str()?
                .strip_prefix("entities[")?
                .strip_suffix(']')?
                .parse()
                .ok()?;
            let candidates: Vec<String> = p["candidates"]
                .as_array()?
                .iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect();
            (!candidates.is_empty()).then(|| Duplicate {
                index,
                key: p["term"].as_str().unwrap_or_default().to_string(),
                candidates,
            })
        })
        .collect()
}

/// Whether every check that failed is a possible duplicate, so that choices can fix the
/// call.
fn only_duplicates(e: &ToolError) -> bool {
    e.data
        .as_ref()
        .and_then(|d| d.get("errors"))
        .and_then(Value::as_array)
        .is_some_and(|l| !l.is_empty() && l.iter().all(|p| p["code"] == "possible-duplicate"))
}

fn field(i: usize) -> String {
    format!("entity{i}")
}

/// The elicitation that asks for a choice per new entity.
fn ask(args: &Map<String, Value>, dups: &[Duplicate]) -> Option<InputRequiredResult> {
    let mut props = Map::new();
    let mut required = Vec::new();
    for d in dups {
        let label = args
            .get("entities")
            .and_then(|e| e.get(d.index))
            .and_then(|e| e.get("label"))
            .and_then(Value::as_str)
            .unwrap_or(&d.key);
        let mut options: Vec<Value> = d
            .candidates
            .iter()
            .map(|c| json!({"const": c, "title": c}))
            .collect();
        options.push(json!({"const": NEW, "title": "a new entity"}));
        props.insert(
            field(d.index),
            json!({
                "type": "string",
                "title": format!("\"{label}\""),
                "description": format!("Is \"{label}\" one of the entities the dataset already has, or a new entity?"),
                "oneOf": options,
            }),
        );
        required.push(field(d.index));
    }
    let request: InputRequest = serde_json::from_value(json!({
        "method": "elicitation/create",
        "params": {
            "message": "Some new entities may already exist in the dataset. Pick the existing entity each one is, or keep it as a new entity.",
            "requestedSchema": {"type": "object", "properties": props, "required": required},
        }
    }))
    .ok()?;
    let mut requests = InputRequests::new();
    requests.insert(REQUEST.to_string(), request);
    Some(InputRequiredResult::new(
        Some(requests),
        Some(STATE.to_string()),
    ))
}

/// The arguments with the person's choices applied, or `None` when the answer is not
/// an acceptance or names something other than the candidates.
fn apply(
    mut args: Map<String, Value>,
    dups: &[Duplicate],
    responses: &InputResponses,
) -> Option<Map<String, Value>> {
    let answer = responses.get(REQUEST)?;
    if answer["action"] != "accept" {
        return None;
    }
    let content = answer.get("content")?.as_object()?;
    let mut drop: Vec<usize> = Vec::new();
    for d in dups {
        let choice = content.get(&field(d.index))?.as_str()?;
        if choice == NEW {
            let e = args
                .get_mut("entities")?
                .get_mut(d.index)?
                .as_object_mut()?;
            let list = e
                .entry("distinctFrom")
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()?;
            for c in &d.candidates {
                if !list.iter().any(|x| x == c) {
                    list.push(c.clone().into());
                }
            }
        } else if d.candidates.iter().any(|c| c == choice) {
            if let Some(facts) = args.get_mut("facts").and_then(Value::as_array_mut) {
                for f in facts {
                    for k in ["s", "o"] {
                        if f.get(k).and_then(Value::as_str) == Some(d.key.as_str()) {
                            f[k] = choice.into();
                        }
                    }
                }
            }
            drop.push(d.index);
        } else {
            return None;
        }
    }
    drop.sort_unstable();
    if let Some(list) = args.get_mut("entities").and_then(Value::as_array_mut) {
        for i in drop.into_iter().rev() {
            if i < list.len() {
                list.remove(i);
            }
        }
    }
    Some(args)
}

fn again(c: &Call) -> Call {
    Call {
        arrived: c.arrived,
        cancel: c.cancel.clone(),
        request_id: c.request_id.clone(),
        principal: c.principal.clone(),
        headers: c.headers.clone(),
        held: c.held.clone(),
    }
}

/// Run `assert_facts` with elicitation: the outcome, and the `input_required` result to
/// answer instead when the person should choose.
pub(super) async fn assert_facts(
    server: &McpServer,
    args: Map<String, Value>,
    call: Call,
    responses: Option<InputResponses>,
    can_elicit: bool,
) -> (Result<Outcome, ToolError>, Option<InputRequiredResult>) {
    let first = match server
        .call("assert_facts", args.clone(), again(&call))
        .await
    {
        Ok(o) => o,
        Err(u) => {
            return (
                Err(ToolError::new(
                    "unknown-tool",
                    404,
                    format!("Unknown tool: {}", u.0),
                )),
                None,
            );
        }
    };
    let Err(e) = &first else {
        return (first, None);
    };
    if !only_duplicates(e) {
        return (first, None);
    }
    let dups = duplicates(e);
    if dups.is_empty() {
        return (first, None);
    }
    match responses {
        Some(r) => match apply(args, &dups, &r) {
            Some(fixed) => match server.call("assert_facts", fixed, call).await {
                Ok(o) => (o, None),
                Err(_) => (first, None),
            },
            None => (first, None),
        },
        None if can_elicit => {
            let ask = ask(&args, &dups);
            (first, ask)
        }
        None => (first, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dup() -> Vec<Duplicate> {
        vec![Duplicate {
            index: 0,
            key: "_:pay".into(),
            candidates: vec!["ex:payments".into()],
        }]
    }

    fn args() -> Map<String, Value> {
        json!({
            "entities": [{"key": "_:pay", "label": "Payments", "types": ["org:OrganizationalUnit"]}],
            "facts": [{"s": "ex:ana", "p": "org:memberOf", "o": "_:pay"}]
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn choices_apply_to_the_arguments() {
        let r: InputResponses = [(
            REQUEST.to_string(),
            json!({"action": "accept", "content": {"entity0": "ex:payments"}}),
        )]
        .into();
        let out = apply(args(), &dup(), &r).unwrap();
        assert_eq!(out["facts"][0]["o"], "ex:payments");
        assert_eq!(out["entities"], json!([]));
        let r: InputResponses = [(
            REQUEST.to_string(),
            json!({"action": "accept", "content": {"entity0": "new"}}),
        )]
        .into();
        let out = apply(args(), &dup(), &r).unwrap();
        assert_eq!(out["entities"][0]["distinctFrom"], json!(["ex:payments"]));
        // a value that is not a candidate, or a declined form, applies nothing
        let r: InputResponses = [(
            REQUEST.to_string(),
            json!({"action": "accept", "content": {"entity0": "ex:other"}}),
        )]
        .into();
        assert!(apply(args(), &dup(), &r).is_none());
        let r: InputResponses = [(REQUEST.to_string(), json!({"action": "decline"}))].into();
        assert!(apply(args(), &dup(), &r).is_none());
    }

    #[test]
    fn the_question_lists_the_candidates_and_a_new_entity() {
        let a = ask(&args(), &dup()).unwrap();
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["resultType"], "input_required");
        let schema = &v["inputRequests"][REQUEST]["params"]["requestedSchema"];
        let opts: Vec<&str> = schema["properties"]["entity0"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["const"].as_str().unwrap())
            .collect();
        assert_eq!(opts, ["ex:payments", "new"]);
    }
}
