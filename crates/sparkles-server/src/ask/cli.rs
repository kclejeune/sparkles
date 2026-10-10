//! `sparkles ask`: the asking pipeline from the command line (C18 §10), which the
//! evaluation of §11.4 drives.

use super::{AskOptions, ask};
use crate::auth::Principal;
use crate::mcp::{McpConfig, McpServer};
use crate::models::{ModelArgs, Pair, Role};
use crate::state::{AppState, DbType};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// `sparkles ask` arguments.
#[derive(clap::Args, Debug)]
pub struct AskArgs {
    /// The database directory to read
    #[arg(
        long,
        value_name = "PATH",
        required_unless_present = "data",
        conflicts_with = "data"
    )]
    pub loc: Option<PathBuf>,
    /// RDF files loaded into an in-memory dataset instead of a database
    #[arg(long, value_name = "FILE", num_args = 1..)]
    pub data: Vec<PathBuf>,
    /// Index the --data dataset for full-text search, which link_entities uses
    #[cfg(feature = "text")]
    #[arg(long, requires = "data")]
    pub text: bool,
    /// The dataset's name, which the prompts and the result report
    pub dataset: String,
    /// The question, at most 2000 characters
    pub question: String,
    /// Force a pair for one role, as ROLE=PROVIDER/MODEL (repeatable). The role then
    /// uses that pair alone and never escalates; the other roles use their lists, from
    /// the database's assistant.json over --model-config, and escalate (spec C18 §5.5)
    #[arg(long = "pair", value_name = "ROLE=PROVIDER/MODEL")]
    pub pairs: Vec<String>,
    /// The answer of an earlier run to try harder on: its draft pair's index in the
    /// draft list (the `draftPair` of its usage); the draft starts at the next pair
    #[arg(long, value_name = "INDEX")]
    pub try_harder: Option<usize>,
    #[command(flatten)]
    pub models: ModelArgs,
    #[command(flatten)]
    pub outbound: crate::outbound::OutboundArgs,
    /// The answer to the clarification that an earlier run of the same question asked
    /// for: a choice's value
    #[arg(long, value_name = "ANSWER")]
    pub clarification: Option<String>,
    /// Stop after the check and print the checked query without running it
    #[arg(long)]
    pub no_run: bool,
    /// Do not summarize the rows
    #[arg(long)]
    pub no_summary: bool,
    /// The rows of the result
    #[arg(long, value_name = "N", default_value_t = 1000)]
    pub max_rows: usize,
    /// The deadline of the whole ask, in seconds
    #[arg(long, value_name = "SECS", default_value_t = 120.0)]
    pub deadline: f64,
    /// The token budget of the whole ask
    #[arg(long, value_name = "N", default_value_t = 50_000)]
    pub max_tokens: u64,
    /// Largest timeout of one query, in seconds
    #[arg(long, value_name = "SECS", default_value_t = 60.0)]
    pub timeout: f64,
    /// Print the result as one JSON object
    #[arg(long, conflicts_with = "events")]
    pub json: bool,
    /// Print each step's event as a JSON line as it happens, then the result
    #[arg(long)]
    pub events: bool,
}

/// `--pair ROLE=PROVIDER/MODEL` flags.
fn parse_pairs(flags: &[String]) -> Result<Vec<(Role, Pair)>> {
    let mut out: Vec<(Role, Pair)> = Vec::new();
    for f in flags {
        let (r, p) = f
            .split_once('=')
            .with_context(|| format!("--pair {f}: expected ROLE=PROVIDER/MODEL"))?;
        let role = Role::parse(r).with_context(|| {
            format!("--pair {f}: unknown role {r:?} (draft, repair, summarize, extract, explain or optimize)")
        })?;
        let pair =
            Pair::parse(p).with_context(|| format!("--pair {f}: expected PROVIDER/MODEL"))?;
        out.retain(|(x, _)| *x != role);
        out.push((role, pair));
    }
    Ok(out)
}

pub fn run_cli(args: AskArgs, store_opts: StoreOptions) -> Result<()> {
    if !(args.timeout.is_finite() && args.timeout > 0.0) {
        bail!("--timeout expects a positive number of seconds");
    }
    if !(args.deadline.is_finite() && args.deadline > 0.0) {
        bail!("--deadline expects a positive number of seconds");
    }
    if args.max_rows == 0 {
        bail!("--max-rows must be at least 1");
    }
    let pairs = parse_pairs(&args.pairs)?;
    // a local command reaches local providers such as Ollama by default
    let Some(models) = args.models.load(args.outbound.local_policy()?)? else {
        bail!("sparkles ask needs --model-config");
    };
    for (role, p) in &pairs {
        let Some(provider) = models.provider(&p.provider) else {
            bail!(
                "--pair {}={}: no provider named {:?}",
                role.as_str(),
                p.label(),
                p.provider
            );
        };
        if !provider.allows(&p.model) {
            bail!(
                "--pair {}={}: the provider's allowedModels leave out {:?}",
                role.as_str(),
                p.label(),
                p.model
            );
        }
    }
    // a database's assistant.json: its role overrides and routing, as the server reads them
    let settings = match &args.loc {
        Some(path) => crate::assistant::settings_at(path)?,
        None => None,
    }
    .unwrap_or_default();
    if let Err(e) =
        crate::assistant::AssistantSettings::parse(&serde_json::to_vec(&settings)?, Some(&models))
    {
        bail!("assistant.json: {e}");
    }
    let lists = crate::assistant::lists(&models, &settings);
    let routing = settings.routing.clone().unwrap_or_default();
    let model_routing = models.config.routing.clone().unwrap_or_default();
    let timeout = Duration::from_secs_f64(args.timeout);
    let mut st = AppState::standalone(store_opts, timeout);
    st.read_only = true;
    st.allow_service = false;
    st.outbound = args.outbound.policy()?;
    let st = Arc::new(st);
    match &args.loc {
        Some(path) => {
            if !path.is_dir() {
                bail!("--loc {}: not a database directory", path.display());
            }
            st.attach(&args.dataset, DbType::Persistent, Some(path))?;
        }
        None => {
            let ds = st.attach(&args.dataset, DbType::Mem, None)?;
            let sources = args
                .data
                .iter()
                .map(|f| Source::from_path(f, None))
                .collect::<Result<Vec<_>, _>>()?;
            ds.store.load(&sources)?;
            #[cfg(feature = "text")]
            if args.text {
                ds.store
                    .enable_text(sparkles::text::TextConfig::default())?;
            }
        }
    }
    let cfg = McpConfig {
        max_rows: args.max_rows.max(1000),
        max_bytes: 4 << 20,
        max_timeout: timeout,
        ..McpConfig::default()
    };
    let server = McpServer::new(st, cfg);
    let o = AskOptions {
        dataset: args.dataset.clone(),
        question: args.question.clone(),
        pairs,
        clarification: args.clarification.clone(),
        run: !args.no_run,
        summary: !args.no_summary,
        max_rows: args.max_rows,
        deadline: Duration::from_secs_f64(args.deadline),
        max_tokens: args.max_tokens,
        lists: Some(lists),
        draft_start: args.try_harder.map(|i| i + 1),
        complexity_threshold: routing
            .complexity_threshold
            .or(model_routing.complexity_threshold)
            .unwrap_or(super::DEFAULT_COMPLEXITY_THRESHOLD),
        example_score: routing
            .example_score
            .or(model_routing.example_score)
            .unwrap_or(super::DEFAULT_EXAMPLE_SCORE),
        ..AskOptions::default()
    };
    let events = args.events;
    let mut on = |event: &str, v: &Value| {
        if events {
            println!("{}", serde_json::json!({ "event": event, "data": v }));
        }
    };
    let out = ask(&server, &models, &Principal::local(), &o, &mut on);
    if args.json || args.events {
        println!("{out}");
    } else {
        print_human(&out);
    }
    if out["outcome"] == "error" {
        std::process::exit(2);
    }
    Ok(())
}

fn print_human(out: &Value) {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let outcome = s(&out["outcome"]);
    if let Some(e) = out.get("error") {
        eprintln!("error: {}: {}", s(&e["code"]), s(&e["message"]));
    }
    if let Some(c) = out.get("clarify") {
        println!("{}", s(&c["question"]));
        for ch in c["choices"].as_array().into_iter().flatten() {
            println!(
                "  {}  (--clarification {})",
                s(&ch["label"]),
                s(&ch["value"])
            );
        }
    }
    let r = &out["result"];
    if r.is_object() {
        if let Some(q) = r["query"].as_str() {
            println!("{q}\n");
        }
        if let Some(e) = r["explanation"].as_str()
            && !e.is_empty()
        {
            println!("{e}");
        }
        for a in r["assumptions"].as_array().into_iter().flatten() {
            println!("assumption: {}", s(a));
        }
        let res = &r["results"];
        if let Some(b) = res["boolean"].as_bool() {
            println!("\n{b}");
        } else if let Some(rows) = res["rows"].as_array() {
            let vars: Vec<String> = res["vars"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| format!("?{}", s(v)))
                .collect();
            println!("\n{}", vars.join("\t"));
            for row in rows.iter().take(20) {
                let cells: Vec<String> = row
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| c.as_str().unwrap_or("").to_string())
                    .collect();
                println!("{}", cells.join("\t"));
            }
            if rows.len() > 20 {
                println!("… {} rows in all", res["total"]);
            }
        }
        if let Some(d) = r["diagnosis"].as_str() {
            println!("\n{d}");
        }
    }
    if let Some(sm) = out.get("summary") {
        println!("\n{}", s(&sm["text"]));
    }
    for n in out["notes"].as_array().into_iter().flatten() {
        println!("note: {}", s(n));
    }
    let u = &out["usage"];
    println!(
        "\n{outcome}: {} model calls, {} input and {} output tokens, {} ms",
        u["modelCalls"], u["inputTokens"], u["outputTokens"], u["elapsedMs"]
    );
}
