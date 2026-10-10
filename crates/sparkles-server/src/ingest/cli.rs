//! `sparkles ingest --loc DB DATASET FILE…` (spec C18 §10): the ingestion of
//! `POST /$/ingest/{ds}` on a database directory, one file after the other.

use super::pipeline::{self, Ctx, Mode, Quiet, Request};
use crate::auth::Principal;
use crate::mcp::{McpConfig, McpServer};
use crate::models::{ModelArgs, Pair};
use crate::state::{AppState, DbType};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sparkles::store::StoreOptions;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// `sparkles ingest` arguments.
#[derive(clap::Args, Debug)]
pub struct IngestArgs {
    /// The database directory to write
    #[arg(long, value_name = "PATH")]
    pub loc: PathBuf,
    /// The dataset's name, which results and review URLs report
    pub dataset: String,
    /// The documents: plain text, Markdown, HTML, PDF, CSV or TSV
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// branch writes the proposals on a review branch, preview writes nothing and prints
    /// them, auto merges them into main when every check passes
    #[arg(long, value_name = "MODE", default_value = "branch", value_parser = ["branch", "preview", "auto"])]
    pub mode: String,
    /// The review branch (default: ingest.<slug>-<n>)
    #[arg(long, value_name = "NAME")]
    pub branch: Option<String>,
    /// The ingest profile
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,
    /// The named graph of the source and its facts (default: the source's IRI)
    #[arg(long, value_name = "IRI")]
    pub graph: Option<String>,
    /// The source's title (default: the document's own, or its file name)
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,
    /// The media type of every file, instead of their extensions
    #[arg(long, value_name = "TYPE")]
    pub format: Option<String>,
    /// Register what can be read of a PDF that needs OCR, and record the other pages
    #[arg(long)]
    pub allow_partial: bool,
    /// Register the text only, without extracting facts
    #[arg(long, conflicts_with = "pairs")]
    pub no_extract: bool,
    /// The extract role's pair, as PROVIDER/MODEL (repeatable, in escalation order),
    /// instead of the dataset's assistant settings
    #[arg(long = "pair", value_name = "PROVIDER/MODEL")]
    pub pairs: Vec<String>,
    /// Go on when the estimate is above the dataset's confirmation threshold
    #[arg(long)]
    pub confirm: bool,
    /// The namespace of a table's rows in its mapping draft
    #[arg(long, value_name = "IRI")]
    pub base: Option<String>,
    /// The deadline of each file, in seconds
    #[arg(long, value_name = "SECS", default_value_t = 3600.0)]
    pub deadline: f64,
    #[command(flatten)]
    pub models: ModelArgs,
    #[command(flatten)]
    pub outbound: crate::outbound::OutboundArgs,
    #[command(flatten)]
    pub ocr: super::OcrArgs,
    /// Print each file's result as one JSON line
    #[arg(long)]
    pub json: bool,
}

pub fn run_cli(args: IngestArgs, store_opts: StoreOptions) -> Result<()> {
    if !(args.deadline.is_finite() && args.deadline > 0.0) {
        bail!("--deadline expects a positive number of seconds");
    }
    if !args.loc.is_dir() {
        bail!("--loc {}: not a database directory", args.loc.display());
    }
    let mode = Mode::parse(&args.mode).context("--mode is branch, preview or auto")?;
    let pairs = args
        .pairs
        .iter()
        .map(|p| Pair::parse(p).with_context(|| format!("--pair {p}: expected PROVIDER/MODEL")))
        .collect::<Result<Vec<_>>>()?;
    // a local command reaches local providers such as Ollama by default
    let models = args.models.load(args.outbound.local_policy()?)?;
    if !pairs.is_empty() {
        let Some(m) = &models else {
            bail!("--pair needs --model-config");
        };
        for p in &pairs {
            let Some(provider) = m.provider(&p.provider) else {
                bail!("--pair {}: no provider named {:?}", p.label(), p.provider);
            };
            if !provider.allows(&p.model) {
                bail!(
                    "--pair {}: the provider's allowedModels leave out {:?}",
                    p.label(),
                    p.model
                );
            }
        }
    }
    let timeout = Duration::from_secs_f64(args.deadline.min(3600.0));
    let mut st = AppState::standalone(store_opts, timeout);
    st.outbound = args.outbound.policy()?;
    st.ingest = super::Runtime::new(&super::IngestServeArgs {
        pdf_workers: 1,
        ocr: args.ocr.clone(),
    });
    let st = Arc::new(st);
    st.attach(&args.dataset, DbType::Persistent, Some(&args.loc))?;
    let cfg = McpConfig {
        max_timeout: timeout,
        allow_update: true,
        ..McpConfig::default()
    };
    let server = McpServer::new(st.clone(), cfg);
    let principal = Principal::local();
    let mut failed = 0;
    for file in &args.files {
        let bytes = std::fs::read(file).with_context(|| format!("{}", file.display()))?;
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned());
        let req = Request {
            dataset: args.dataset.clone(),
            bytes,
            name,
            media_type: args.format.clone(),
            url: None,
            title: args.title.clone(),
            iri: None,
            graph: args.graph.clone(),
            profile: args.profile.clone(),
            mode,
            branch: args.branch.clone(),
            allow_partial: args.allow_partial,
            extract: if args.no_extract {
                Some(false)
            } else if !pairs.is_empty() {
                Some(true)
            } else {
                None
            },
            confirm: args.confirm,
            base: args.base.clone(),
            message: None,
            pairs: pairs.clone(),
        };
        let ctx = Ctx {
            server: &server,
            models: models.as_deref(),
            principal: &principal,
            progress: &Quiet,
            deadline: Instant::now() + Duration::from_secs_f64(args.deadline),
            cancel: Arc::new(AtomicBool::new(false)),
            pdf: st.ingest.pdf.clone(),
            request_id: String::new(),
        };
        let (out, usage) = pipeline::run(&ctx, &req);
        let line = match &out {
            Ok(v) => json!({ "file": file.display().to_string(), "result": v, "usage": usage }),
            Err(e) => {
                failed += 1;
                json!({ "file": file.display().to_string(), "error": e, "usage": usage })
            }
        };
        if args.json {
            println!("{line}");
        } else {
            print_human(&line);
        }
    }
    if failed > 0 {
        std::process::exit(2);
    }
    Ok(())
}

fn print_human(line: &Value) {
    let s = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    };
    let file = s(&line["file"]);
    if let Some(e) = line.get("error") {
        eprintln!("{file}: {}: {}", s(&e["code"]), s(&e["message"]));
        for p in e["pages"].as_array().into_iter().flatten() {
            eprintln!("  page {}: {}", p["page"], s(&p["reasons"]));
        }
        return;
    }
    let r = &line["result"];
    match r["outcome"].as_str() {
        Some("mapping-draft") => {
            println!(
                "{file}: mapping draft ({}), {} rows, {} triples",
                s(&r["drafted"]),
                r["rows"],
                r["triples"]
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&r["mapping"]).unwrap_or_default()
            );
        }
        Some(o) => {
            println!("{file}: {o}");
            for k in ["source", "rendition", "branch", "proposed", "commit"] {
                if !r[k].is_null() {
                    println!("  {k}: {}", s(&r[k]));
                }
            }
            for k in ["omittedPages", "ocrPages"] {
                if !r[k].is_null() {
                    println!("  {k}: {}", r[k]);
                }
            }
        }
        None => println!("{file}: {r}"),
    }
    for n in r["notes"].as_array().into_iter().flatten() {
        println!("  note: {}", s(n));
    }
    let u = &line["usage"];
    if u["modelCalls"].as_u64().unwrap_or(0) > 0 {
        println!(
            "  {} model calls, {} input and {} output tokens",
            u["modelCalls"], u["inputTokens"], u["outputTokens"]
        );
    }
}
