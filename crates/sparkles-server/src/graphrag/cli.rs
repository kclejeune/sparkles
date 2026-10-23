//! `sparkles graphrag` (spec C22 §2.2): build, refresh and read a dataset's community
//! reports through the routes of `/$/graphrag/{ds}`, on a server or, with `--loc`, on a
//! database directory served in the process with the model configuration of
//! `--model-config`.

use crate::memory_cmd::Common;
use crate::memory_cmd::conn::{CmdError, Conn, enc};
use crate::models::ModelArgs;
use crate::state::AppState;
use anyhow::Result;
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use sparkles::store::StoreOptions;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Args, Debug)]
pub struct GraphragArgs {
    /// The server (else SPARKLES_SERVER or the saved default of `sparkles auth login`)
    #[arg(long, global = true, env = "SPARKLES_SERVER")]
    pub server: Option<String>,
    /// The dataset (with --loc, default: the directory's name)
    #[arg(long, global = true)]
    pub dataset: Option<String>,
    /// A database directory to open in this process instead of a server
    #[arg(long, global = true)]
    pub loc: Option<PathBuf>,
    /// Allow plain http to a server other than localhost
    #[arg(long, global = true)]
    pub insecure_http: bool,
    /// One JSON document per run
    #[arg(long, global = true)]
    pub json: bool,
    #[command(flatten)]
    pub models: ModelArgs,
    #[command(flatten)]
    pub outbound: crate::outbound::OutboundArgs,
    #[command(subcommand)]
    pub cmd: GraphragCmd,
}

#[derive(Subcommand, Debug)]
pub enum GraphragCmd {
    /// Write a report for every community of the dataset's projection
    Build(RunFlags),
    /// Rewrite the reports of the communities that changed since the last run
    Refresh(RunFlags),
    /// The reports by status, the communities per level and the last build
    Status,
    /// The communities and their reports' titles and statuses
    Reports {
        /// Only reports of this status
        #[arg(long, value_parser = ["unreviewed", "reviewed", "rejected", "stale", "failed"])]
        status: Option<String>,
    },
}

#[derive(Args, Debug, Clone)]
pub struct RunFlags {
    /// Send without asking: the estimate is accepted as it is
    #[arg(long)]
    pub confirm: bool,
    /// Print the task and return; it waits for confirmation on the server unless
    /// --confirm is given
    #[arg(long, conflicts_with = "loc")]
    pub no_wait: bool,
}

const ENDED: &[&str] = &["done", "failed", "cancelled"];

fn connect(args: &GraphragArgs, opts: StoreOptions) -> Result<Conn, CmdError> {
    if let Some(loc) = &args.loc {
        let dataset = args.dataset.clone().unwrap_or_else(|| {
            loc.canonicalize()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "db".into())
        });
        let mut st = AppState::standalone(opts, Duration::from_secs(300));
        st.outbound = args
            .outbound
            .policy()
            .map_err(|e| CmdError::usage(format!("{e:#}")))?;
        // a local command reaches local providers such as Ollama by default
        let policy = args
            .outbound
            .local_policy()
            .map_err(|e| CmdError::usage(format!("{e:#}")))?;
        let models = args
            .models
            .load(policy)
            .map_err(|e| CmdError::usage(format!("{e:#}")))?;
        st.models = arc_swap::ArcSwapOption::new(models);
        return Conn::local_state(loc, dataset, st);
    }
    if args.models.model_config.is_some() {
        return Err(CmdError::usage(
            "--model-config applies with --loc; a server uses its own model configuration",
        ));
    }
    let dataset = args
        .dataset
        .clone()
        .ok_or_else(|| CmdError::usage("no dataset: pass --dataset"))?;
    let common = Common {
        server: None,
        dataset: Some(dataset.clone()),
        loc: None,
        branch: None,
        insecure_http: args.insecure_http,
        if_reachable: false,
        json: args.json,
    };
    Conn::open(&common, args.server.clone(), dataset, opts)
}

pub fn run(args: GraphragArgs, opts: StoreOptions) -> Result<()> {
    let json_mode = args.json;
    match dispatch(args, opts) {
        Ok((v, text)) => {
            if json_mode {
                crate::memory_cmd::print_json(&v);
            } else if !text.is_empty() {
                println!("{text}");
            }
            Ok(())
        }
        Err(e) => {
            if json_mode {
                let mut j = json!({ "error": e.message, "code": e.code });
                if let Some(d) = &e.detail {
                    j["detail"] = d.clone();
                }
                crate::memory_cmd::print_json(&j);
            } else {
                eprintln!("error: {}", e.message);
            }
            std::process::exit(if e.exit == 0 { 1 } else { e.exit });
        }
    }
}

fn dispatch(args: GraphragArgs, opts: StoreOptions) -> Result<(Value, String), CmdError> {
    let conn = connect(&args, opts)?;
    let base = format!("/$/graphrag/{}", enc(&conn.dataset));
    match &args.cmd {
        GraphragCmd::Build(f) => start(&conn, &base, "build", f, args.json),
        GraphragCmd::Refresh(f) => start(&conn, &base, "refresh", f, args.json),
        GraphragCmd::Status => {
            let v = conn.get_json(&base)?;
            let t = status_text(&v);
            Ok((v, t))
        }
        GraphragCmd::Reports { status } => {
            let mut v = conn.get_json(&format!("{base}/communities"))?;
            if let (Some(s), Some(list)) = (status, v["communities"].as_array_mut()) {
                list.retain(|c| c["report"]["status"].as_str() == Some(s.as_str()));
            }
            let t = reports_text(&v);
            Ok((v, t))
        }
    }
}

fn task_path(id: &str) -> String {
    format!("/$/tasks/{}", enc(id))
}

/// Start a run and, unless `--no-wait`, follow it: show the estimate, ask for
/// confirmation on a terminal when `--confirm` is not given, and wait for the end.
fn start(
    conn: &Conn,
    base: &str,
    route: &str,
    f: &RunFlags,
    json_mode: bool,
) -> Result<(Value, String), CmdError> {
    let mut task = conn
        .post_json(&format!("{base}/{route}"), &json!({ "confirm": f.confirm }))?
        .check()?
        .json()?;
    let id = task["id"]
        .as_str()
        .ok_or_else(|| CmdError::error("bad-response", "the server answered no task id"))?
        .to_string();
    if f.no_wait {
        let t = format!(
            "task {id} {}{}",
            task["state"].as_str().unwrap_or(""),
            if f.confirm {
                String::new()
            } else {
                format!(
                    "; it waits for confirmation (POST {base}/confirm {{\"task\":\"{id}\"}} or the Communities tab)"
                )
            }
        );
        return Ok((task, t));
    }
    let mut asked = f.confirm;
    loop {
        task = conn.get_json(&task_path(&id))?;
        let state = task["state"].as_str().unwrap_or("");
        if ENDED.contains(&state) {
            break;
        }
        if !asked && task["detail"]["phase"] == "awaiting-confirmation" {
            asked = true;
            let yes = confirm(&task["detail"]["estimate"], json_mode)?;
            conn.post_json(
                &format!("{base}/confirm"),
                &json!({ "task": id, "confirm": yes }),
            )?
            .check()?;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if task["state"] != "done" {
        let msg = task["message"].as_str().unwrap_or("no reason given");
        return Err(CmdError::error(
            "task-failed",
            format!("task {id} {}: {msg}", task["state"].as_str().unwrap_or("")),
        )
        .with_detail(task));
    }
    let d = &task["detail"];
    let mut t = format!(
        "GraphRAG {route} of /{}: {} communities, {} reports written, {} kept, {} failed, {} tokens",
        conn.dataset, d["communities"], d["written"], d["kept"], d["failed"], d["tokens"]
    );
    if let Some(why) = d["stopped"].as_str() {
        t.push_str(&format!("\n  stopped with {} pending: {why}", d["pending"]));
    }
    Ok((task, t))
}

/// Ask on a terminal whether to send. Without a terminal, or with `--json`, the answer
/// is no and the run ends with a hint to pass `--confirm`.
fn confirm(estimate: &Value, json_mode: bool) -> Result<bool, CmdError> {
    let text = format!(
        "estimate (an upper bound): {} communities, {} model requests to {}, up to {} tokens (budget {} per build, {} per day)",
        estimate["communities"],
        estimate["requests"],
        estimate["model"].as_str().unwrap_or("?"),
        estimate["tokens"],
        estimate["tokensPerBuild"],
        estimate["tokensPerDay"],
    );
    if json_mode || !std::io::stdin().is_terminal() {
        eprintln!("{text}");
        eprintln!("not confirmed: pass --confirm to send without asking");
        return Ok(false);
    }
    eprint!("{text}\nsend? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(CmdError::from)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn status_text(v: &Value) -> String {
    let mut t = format!(
        "/{}: GraphRAG {}, reports graph {}",
        v["dataset"].as_str().unwrap_or(""),
        if v["enabled"] == true {
            "enabled"
        } else {
            "disabled"
        },
        v["reportsGraph"].as_str().unwrap_or("")
    );
    if v["available"] == false {
        t.push_str(&format!(
            "\n  {}",
            v["reason"].as_str().unwrap_or("not available")
        ));
        return t;
    }
    if let Some(r) = v["reports"].as_object() {
        let parts: Vec<String> = r.iter().map(|(k, n)| format!("{n} {k}")).collect();
        t.push_str(&format!("\n  reports: {}", parts.join(", ")));
    }
    for l in v["levels"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\n  level {}: {} communities",
            l["level"], l["communities"]
        ));
    }
    if let Some(b) = v["lastBuild"].as_object() {
        t.push_str(&format!(
            "\n  last run: commit {} on {} at {}, {} tokens, {} pending",
            b["sourceCommit"],
            b["branch"].as_str().unwrap_or(""),
            b["finishedAt"].as_str().unwrap_or(""),
            b["tokens"],
            b["pending"]
        ));
    }
    if let Some(b) = v["budget"].as_object() {
        t.push_str(&format!(
            "\n  budget: {} of {} tokens used today, {} per build",
            b["usedToday"], b["tokensPerDay"], b["tokensPerBuild"]
        ));
    }
    let anomalies = v["anomalies"].as_array().map_or(0, Vec::len);
    if anomalies > 0 {
        t.push_str(&format!(
            "\n  {anomalies} accounting anomalies: a provider reported more usage than its reservation"
        ));
    }
    t
}

fn reports_text(v: &Value) -> String {
    if v["available"] == false {
        return "the reports are not available to this caller".into();
    }
    let mut t = String::new();
    for c in v["communities"].as_array().into_iter().flatten() {
        let r = &c["report"];
        t.push_str(&format!(
            "level {} {} ({} members): {} [{}]\n",
            c["level"],
            c["iri"].as_str().unwrap_or(""),
            c["memberCount"],
            r["title"].as_str().unwrap_or("(no report)"),
            r["status"].as_str().unwrap_or("-"),
        ));
    }
    if t.is_empty() {
        t.push_str("no communities");
    }
    t.trim_end().to_string()
}
