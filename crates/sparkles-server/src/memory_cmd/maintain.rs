//! `sparkles memory consolidate`, `retention` and `maintenance`: the maintenance tasks
//! of spec C18 §8.3 and §8.4 through `POST /$/memory/{ds}/consolidate`,
//! `POST /$/memory/{ds}/retention` and `GET /$/memory/{ds}/maintenance`.

use super::TaskFlags;
use super::conn::{CmdError, Conn, enc};
use serde_json::{Value, json};

/// The statuses after which a task changes no more.
const ENDED: &[&str] = &["done", "failed", "cancelled"];

/// Start a task with `POST path` and, unless `--no-wait`, wait for it to end. A failed
/// or cancelled task is an error that carries the task.
fn start(conn: &Conn, route: &str, mut body: Value, flags: &TaskFlags) -> Result<Value, CmdError> {
    if let Some(d) = flags.deadline {
        body["deadlineSeconds"] = d.into();
    }
    let path = format!("/$/memory/{}/{route}", enc(&conn.dataset));
    let mut task = conn.post_json(&path, &body)?.check()?.json()?;
    if flags.no_wait {
        return Ok(task);
    }
    let id = task["id"]
        .as_str()
        .ok_or_else(|| CmdError::error("bad-response", "the server answered no task id"))?
        .to_string();
    let poll = format!("/$/ingest/{}/{}?wait=30", enc(&conn.dataset), enc(&id));
    while !task["status"].as_str().is_some_and(|s| ENDED.contains(&s)) {
        task = conn.get_json(&poll)?;
    }
    if task["status"] != "done" {
        let msg = task["error"]["message"]
            .as_str()
            .or_else(|| task["message"].as_str())
            .unwrap_or("no reason given");
        return Err(CmdError::error(
            task["error"]["code"].as_str().unwrap_or("task-failed"),
            format!("task {id} {}: {msg}", task["status"].as_str().unwrap_or("")),
        )
        .with_detail(task));
    }
    Ok(task)
}

fn started_text(task: &Value) -> String {
    format!(
        "task {} {} (sparkles memory maintenance, or GET /$/ingest/{}/{})",
        task["id"].as_str().unwrap_or(""),
        task["status"].as_str().unwrap_or(""),
        task["dataset"].as_str().unwrap_or(""),
        task["id"].as_str().unwrap_or("")
    )
}

fn count(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| v.as_array().map(|a| a.len() as u64))
        .unwrap_or(0)
}

pub fn consolidate(
    conn: &Conn,
    mode: Option<String>,
    min_sources: Option<u32>,
    dry_run: bool,
    message: Option<String>,
    flags: &TaskFlags,
) -> Result<(Value, String), CmdError> {
    let mut body = json!({});
    if let Some(m) = mode {
        body["mode"] = m.into();
    }
    if let Some(n) = min_sources {
        body["minSources"] = n.into();
    }
    if dry_run {
        body["dryRun"] = true.into();
    }
    if let Some(m) = message {
        body["message"] = m.into();
    }
    let task = start(conn, "consolidate", body, flags)?;
    if flags.no_wait {
        let t = started_text(&task);
        return Ok((task, t));
    }
    let r = &task["result"];
    let mut t = format!(
        "consolidation of /{}: {}",
        conn.dataset,
        r["outcome"].as_str().unwrap_or("")
    );
    if let Some(b) = r["branch"].as_str() {
        t.push_str(&format!(" on the branch {b}"));
    }
    t.push_str(&format!(
        "\n  {} repeated facts, {} proposed, {} possible duplicate entities, {} conflicts",
        count(&r["repeated"]),
        count(&r["proposed"]),
        count(&r["duplicates"]),
        count(&r["conflicts"])
    ));
    for f in r["facts"].as_array().into_iter().flatten().take(20) {
        t.push_str(&format!(
            "\n  {} {} {}  ({} sources)",
            f["s"].as_str().unwrap_or(""),
            f["p"].as_str().unwrap_or(""),
            f["o"].as_str().unwrap_or(""),
            f["sources"]
        ));
    }
    Ok((task, t))
}

pub fn retention(
    conn: &Conn,
    after: Option<String>,
    graphs: Vec<String>,
    require_consolidated: Option<bool>,
    dry_run: bool,
    flags: &TaskFlags,
) -> Result<(Value, String), CmdError> {
    let mut body = json!({});
    if let Some(a) = after {
        body["after"] = a.into();
    }
    if !graphs.is_empty() {
        body["graphs"] = graphs.into();
    }
    if let Some(r) = require_consolidated {
        body["requireConsolidated"] = r.into();
    }
    if dry_run {
        body["dryRun"] = true.into();
    }
    let task = start(conn, "retention", body, flags)?;
    if flags.no_wait {
        let t = started_text(&task);
        return Ok((task, t));
    }
    let r = &task["result"];
    let mut t = format!(
        "retention of /{} after {}: {}",
        conn.dataset,
        r["after"].as_str().unwrap_or("?"),
        r["outcome"].as_str().unwrap_or("")
    );
    for g in r["graphs"].as_array().into_iter().flatten() {
        let what = if g["delete"] == true {
            if r["outcome"] == "deleted" {
                "deleted".to_string()
            } else {
                "would be deleted".to_string()
            }
        } else {
            format!("kept: {}", g["kept"].as_str().unwrap_or(""))
        };
        t.push_str(&format!(
            "\n  {}  {what}",
            g["graph"].as_str().unwrap_or("")
        ));
    }
    for e in r["errors"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\n  {}  not deleted: {}",
            e["graph"].as_str().unwrap_or(""),
            e["message"].as_str().unwrap_or("")
        ));
    }
    Ok((task, t))
}

pub fn maintenance(conn: &Conn) -> Result<(Value, String), CmdError> {
    let mut m = conn.get_json(&format!("/$/memory/{}/maintenance", enc(&conn.dataset)))?;
    // the outcome of each last run, while the server still lists its task
    for name in ["consolidation", "retention"] {
        let Some(id) = m[name]["lastTask"].as_str().map(str::to_string) else {
            continue;
        };
        if let Ok(task) = conn.get_json(&format!("/$/ingest/{}/{}", enc(&conn.dataset), enc(&id))) {
            m[name]["lastStatus"] = task["status"].clone();
            if let Some(o) = task["result"]["outcome"].as_str() {
                m[name]["lastOutcome"] = o.into();
            }
        }
    }
    let mut t = format!("memory maintenance of /{}", conn.dataset);
    for name in ["consolidation", "retention"] {
        let e = &m[name];
        let s = &e["settings"];
        let mut line = format!("\n{name}: ");
        if s.is_null() {
            line.push_str("not configured");
        } else {
            let mut parts = Vec::new();
            match s["every"].as_str() {
                Some(every) => parts.push(format!("every {every}")),
                None if name == "retention" => parts.push("every 1d".into()),
                None => parts.push("on demand only".into()),
            }
            if let Some(mode) = s["mode"].as_str() {
                parts.push(format!("mode {mode}"));
            }
            if let Some(n) = s["minSources"].as_u64() {
                parts.push(format!("at least {n} sources"));
            }
            if let Some(a) = s["after"].as_str() {
                parts.push(format!("after {a}"));
            }
            line.push_str(&parts.join(", "));
        }
        t.push_str(&line);
        match (e["lastRun"].as_str(), e["lastTask"].as_str()) {
            (Some(at), Some(task)) => {
                let outcome = e["lastOutcome"].as_str().or(e["lastStatus"].as_str());
                t.push_str(&format!(
                    "\n  last run {at}: {} (task {task})",
                    outcome.unwrap_or("no longer listed")
                ))
            }
            (Some(at), None) => t.push_str(&format!("\n  last run {at}")),
            _ => {}
        }
        if let Some(next) = e["nextRun"].as_str() {
            t.push_str(&format!("\n  next run {next}"));
        }
    }
    t.push_str(&review_text(&m["review"]));
    Ok((m, t))
}

/// The `review` member of the maintenance answer in lines: the open items, how long the
/// oldest has waited, and each kind. Empty when the answer has none.
pub(crate) fn review_text(r: &Value) -> String {
    let Some(open) = r["open"].as_u64() else {
        return String::new();
    };
    if open == 0 {
        return "\nreview: nothing waits for review".into();
    }
    let mut t = format!("\nreview: {open} open");
    if let Some(o) = r["oldest"].as_str() {
        t.push_str(&format!(", the oldest since {o}"));
        if let Ok(at) = chrono::DateTime::parse_from_rfc3339(o) {
            let age = chrono::Utc::now() - at.with_timezone(&chrono::Utc);
            t.push_str(&format!(" ({})", age_words(age.num_seconds())));
        }
    }
    if r["truncated"] == true {
        t.push_str(", counted up to the first 5000 facts");
    }
    for (kind, k) in r["kinds"].as_object().into_iter().flatten() {
        t.push_str(&format!("\n  {kind}: {}", k["open"].as_u64().unwrap_or(0)));
        if let Some(o) = k["oldest"].as_str() {
            t.push_str(&format!(" since {o}"));
        }
    }
    t
}

/// An age in seconds as days, hours or minutes.
fn age_words(secs: i64) -> String {
    let secs = secs.max(0);
    if secs >= 86_400 {
        format!("{} d", secs / 86_400)
    } else if secs >= 3600 {
        format!("{} h", secs / 3600)
    } else {
        format!("{} min", secs / 60)
    }
}
