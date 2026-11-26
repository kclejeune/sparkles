//! `sparkles ask`: the asking pipeline from the command line (C18 §10), which the
//! evaluation of §11.4 drives.

use super::{AskMode, AskOptions, ask};
use crate::auth::Principal;
use crate::mcp::{McpConfig, McpServer};
use crate::models::{ModelArgs, Pair, Role};
use crate::state::{AppState, DbType};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::io::Write;
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
    #[arg(required_unless_present = "questions", conflicts_with = "questions")]
    pub question: Option<String>,
    /// Ask every question of a JSON-lines file, or `-` for standard input, with one
    /// store and model configuration. Each line is `{"id", "question",
    /// "clarification"?, "tryHarder"?, "modelSecrets"?}`, and each result is printed as
    /// one JSON line with its `id`, in the order of the input
    #[arg(long, value_name = "FILE", conflicts_with_all = ["events", "clarification", "try_harder"])]
    pub questions: Option<PathBuf>,
    /// With --questions, how many questions run at once
    #[arg(long, value_name = "N", default_value_t = 1, requires = "questions")]
    pub jobs: usize,
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
    /// Answer from the data (`local`), from the community reports (`global`), or as the
    /// question needs (`auto`, spec C22 §7.1)
    #[arg(long, value_enum, default_value_t = AskMode::Auto)]
    pub mode: AskMode,
    /// Use only community reports that a person marked reviewed
    #[arg(long)]
    pub reviewed_only: bool,
    /// Cite claim occurrences under this review policy in the summary (spec C25)
    #[arg(long, value_name = "POLICY")]
    pub review_policy: Option<String>,
    /// Resolve linked entities through this matching policy's map (spec C25)
    #[arg(long, value_name = "POLICY")]
    pub matching_policy: Option<String>,
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
    /// Budget for the estimated memory of each query's intermediate results, in MiB (0:
    /// unlimited), as `sparkles mcp --query-memory-mb` sets it
    #[arg(long, value_name = "N", default_value_t = 2048)]
    pub query_memory_mb: u64,
    /// Above this many quads, the schema reports count classes and predicates from the
    /// index statistics instead of scanning the indexes; `off` always scans
    #[arg(long, value_name = "QUADS", default_value_t = crate::state::StatisticsAbove::default())]
    pub schema_statistics_above: crate::state::StatisticsAbove,
    /// Explore the data once when a draft gives no query or its query returns no rows,
    /// with further link candidates and the predicates the entities use, and draft
    /// again. On by default
    #[arg(long, overrides_with = "no_explore")]
    pub explore: bool,
    /// Do not explore the data after a failed draft
    #[arg(long, overrides_with = "explore")]
    pub no_explore: bool,
    /// Repair once when the result's shape does not fit the question's form, such as
    /// several rows for "How many", keeping the first answer if the repair fails. Off
    /// by default
    #[arg(long, overrides_with = "no_shape_check")]
    pub shape_check: bool,
    /// Do not check the result's shape
    #[arg(long, overrides_with = "shape_check")]
    pub no_shape_check: bool,
    /// Ask this many first drafts and keep the one whose result most drafts share, 1 by
    /// default. A preview with --no-run asks one, since the vote runs each draft
    #[arg(long, value_name = "N")]
    pub drafts: Option<usize>,
    /// Link each mention with the classes the words around it name, and show how many
    /// triples name each candidate. Off by default
    #[arg(long, overrides_with = "no_type_hints")]
    pub type_hints: bool,
    /// Do not hint mention types
    #[arg(long, overrides_with = "type_hints")]
    pub no_type_hints: bool,
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
            format!(
                "--pair {f}: unknown role {r:?} ({})",
                Role::ALL.map(Role::as_str).join(", ")
            )
        })?;
        let pair =
            Pair::parse(p).with_context(|| format!("--pair {f}: expected PROVIDER/MODEL"))?;
        out.retain(|(x, _)| *x != role);
        out.push((role, pair));
    }
    Ok(out)
}

pub fn run_cli(args: AskArgs, store_opts: StoreOptions) -> Result<()> {
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    if !run_cli_with(args, store_opts, &mut stdout, &mut stderr)? {
        std::process::exit(2);
    }
    Ok(())
}

fn run_cli_with(
    args: AskArgs,
    store_opts: StoreOptions,
    stdout: &mut (dyn Write + Send),
    stderr: &mut (dyn Write + Send),
) -> Result<bool> {
    if !(args.timeout.is_finite() && args.timeout > 0.0) {
        bail!("--timeout expects a positive number of seconds");
    }
    if !(args.deadline.is_finite() && args.deadline > 0.0) {
        bail!("--deadline expects a positive number of seconds");
    }
    if args.max_rows == 0 {
        bail!("--max-rows must be at least 1");
    }
    if !(1..=64).contains(&args.jobs) {
        bail!("--jobs expects 1 to 64");
    }
    if let Some(n) = args.drafts
        && !(1..=super::MAX_DRAFTS).contains(&n)
    {
        bail!("--drafts expects 1 to {}", super::MAX_DRAFTS);
    }
    let pairs = parse_pairs(&args.pairs)?;
    // a local command reaches local providers such as Ollama by default
    let Some(models) = args.models.load(args.outbound.local_policy()?)? else {
        bail!("sparkles ask needs --model-config");
    };
    check_pairs(&models, &pairs)?;
    // a database's assistant.json: its role overrides and routing, as the server reads them
    let settings = match &args.loc {
        Some(path) => crate::assistant::settings_at(path)?,
        None => None,
    }
    .unwrap_or_default();
    if let Err(e) = settings.validate(crate::settings::Providers::Checked(Some(&models))) {
        bail!("assistant.json: {e}");
    }
    let server = open_server(&args, store_opts)?;
    let base = |models: &crate::models::Models, question: String| {
        options(&args, &settings, models, pairs.clone(), question)
    };
    if let Some(file) = &args.questions {
        let input: Box<dyn std::io::BufRead + Send> = if file.as_os_str() == "-" {
            Box::new(std::io::BufReader::new(std::io::stdin()))
        } else {
            Box::new(std::io::BufReader::new(
                std::fs::File::open(file)
                    .with_context(|| format!("--questions {}", file.display()))?,
            ))
        };
        let line_models = |own: &[String]| line_view(&models, &args.models.model_secret, own);
        batch(
            input,
            stdout,
            args.jobs,
            &server,
            &models,
            &line_models,
            &base,
        )?;
        return Ok(true);
    }
    let mut o = base(&models, args.question.clone().unwrap_or_default());
    o.clarification = args.clarification.clone();
    o.draft_start = match args.try_harder {
        Some(i) => Some(i.checked_add(1).context("--try-harder is too large")?),
        None => None,
    };
    let events = args.events;
    let out = {
        let mut on = |event: &str, v: &Value| {
            if events {
                let _ = writeln!(
                    stdout,
                    "{}",
                    serde_json::json!({ "event": event, "data": v })
                );
            }
        };
        ask(&server, &models, &Principal::local(), &o, &mut on)
    };
    if args.json || args.events {
        writeln!(stdout, "{out}")?;
    } else {
        write_human(&out, stdout, stderr)?;
    }
    Ok(out["outcome"] != "error")
}

/// Refuse a forced pair whose provider is missing or does not allow its model.
fn check_pairs(models: &crate::models::Models, pairs: &[(Role, Pair)]) -> Result<()> {
    for (role, p) in pairs {
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
    Ok(())
}

/// The read-only state and MCP server that every question of a run shares, so a batch
/// opens the store once and keeps its caches warm across questions.
fn open_server(args: &AskArgs, store_opts: StoreOptions) -> Result<McpServer> {
    let timeout = Duration::from_secs_f64(args.timeout);
    let mut st = AppState::standalone(store_opts, timeout);
    st.read_only = true;
    st.allow_service = false;
    st.outbound = args.outbound.policy()?;
    st.schema_statistics_above = args.schema_statistics_above.0;
    let memory = (args.query_memory_mb > 0).then_some(args.query_memory_mb << 20);
    st.limits.query_memory_bytes = memory;
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
        query_memory_bytes: memory,
        ..McpConfig::default()
    };
    Ok(McpServer::new(st, cfg))
}

/// The options of one question under the run's flags.
fn options(
    args: &AskArgs,
    settings: &crate::assistant::AssistantSettings,
    models: &crate::models::Models,
    pairs: Vec<(Role, Pair)>,
    question: String,
) -> AskOptions {
    let lists = crate::assistant::lists(models, settings);
    let report_context = crate::assistant::rows_allowed(
        settings,
        &lists,
        &pairs,
        &[crate::models::Role::Draft, crate::models::Role::Repair],
    );
    let routing = settings.routing.clone().unwrap_or_default();
    let model_routing = models.config.routing.clone().unwrap_or_default();
    let mut o = AskOptions {
        dataset: args.dataset.clone(),
        question,
        pairs,
        clarification: None,
        run: !args.no_run,
        summary: !args.no_summary,
        max_rows: args.max_rows,
        deadline: Duration::from_secs_f64(args.deadline),
        max_tokens: args.max_tokens,
        lists: Some(lists),
        draft_start: None,
        mode: args.mode,
        reviewed_only: args.reviewed_only,
        report_context,
        review_policy: args.review_policy.clone(),
        matching_policy: args.matching_policy.clone(),
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
    settings.repairs.or(repairs_of(args)).apply(&mut o);
    o
}

/// The repair flags that the command line sets, each unset when neither its flag nor
/// its `--no-` flag is given.
pub(super) fn repairs_of(args: &AskArgs) -> crate::assistant::AskRepairs {
    let flag = |on: bool, off: bool| (on || off).then_some(on);
    crate::assistant::AskRepairs {
        explore: flag(args.explore, args.no_explore),
        shape_check: flag(args.shape_check, args.no_shape_check),
        drafts: args.drafts,
        type_hints: flag(args.type_hints, args.no_type_hints),
    }
}

/// One line of `--questions`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct BatchLine {
    id: String,
    question: String,
    clarification: Option<String>,
    try_harder: Option<usize>,
    /// NAME=SOURCE secrets that replace the --model-secret of the same NAME for this
    /// question, such as a key that bills it to its own account
    #[serde(default)]
    model_secrets: Vec<String>,
}

fn line_error(id: Value, code: &str, message: String) -> Value {
    serde_json::json!({
        "id": id,
        "outcome": "error",
        "error": {"code": code, "message": message},
    })
}

/// `base` with each NAME=SOURCE of `own` in place of the one with the same NAME.
/// The models of a batch line that names its own secrets: a view of `models` with
/// those secrets in place of the `--model-secret` flags of the same names. The view
/// shares the providers' state, so the line's requests count against the same
/// concurrency, spacing and daily token limits as every other line.
fn line_view(
    models: &crate::models::Models,
    flags: &[String],
    own: &[String],
) -> Result<Arc<crate::models::Models>> {
    let secrets = crate::models::parse_secrets(&merged_secrets(flags, own))?;
    Ok(Arc::new(models.with_secrets(secrets)))
}

fn merged_secrets(base: &[String], own: &[String]) -> Vec<String> {
    let name = |s: &String| s.split_once('=').map_or(s.clone(), |(n, _)| n.to_string());
    let mut out: Vec<String> = base
        .iter()
        .filter(|b| own.iter().all(|o| name(o) != name(b)))
        .cloned()
        .collect();
    out.extend(own.iter().cloned());
    out
}

type LineModels<'a> = dyn Fn(&[String]) -> Result<Arc<crate::models::Models>> + Sync + 'a;
type BaseOptions<'a> = dyn Fn(&crate::models::Models, String) -> AskOptions + Sync + 'a;

/// Ask every line of `input` and write each result to `output` in input order. Up to
/// `jobs` questions run at once on the shared server. Each keeps its own deadline,
/// token budget and query limits, and a line that is invalid, fails or panics yields
/// its own error result while the others go on.
fn batch(
    input: Box<dyn std::io::BufRead + Send>,
    output: &mut (dyn std::io::Write + Send),
    jobs: usize,
    server: &McpServer,
    models: &Arc<crate::models::Models>,
    line_models: &LineModels<'_>,
    base: &BaseOptions<'_>,
) -> Result<()> {
    use std::collections::BTreeMap;
    use std::io::BufRead;
    let lines = parking_lot::Mutex::new((input.lines(), 0usize));
    // results wait here until every earlier line is written
    let out = parking_lot::Mutex::new((output, 0usize, BTreeMap::<usize, String>::new()));
    let failed = parking_lot::Mutex::new(None::<anyhow::Error>);
    let run_line = |raw: String| -> Value {
        let line: BatchLine = match serde_json::from_str(&raw) {
            Ok(l) => l,
            Err(e) => {
                let id = serde_json::from_str::<Value>(&raw)
                    .ok()
                    .and_then(|v| v.get("id").cloned())
                    .unwrap_or(Value::Null);
                return line_error(id, "invalid-request", format!("--questions line: {e}"));
            }
        };
        let id = Value::String(line.id.clone());
        let own;
        let models: &crate::models::Models = if line.model_secrets.is_empty() {
            models
        } else {
            match line_models(&line.model_secrets) {
                Ok(m) => {
                    own = m;
                    &own
                }
                Err(e) => return line_error(id, "invalid-request", format!("{e:#}")),
            }
        };
        let draft_start = match line.try_harder.map(|i| i.checked_add(1)) {
            Some(None) => {
                return line_error(id, "invalid-request", "tryHarder is too large".into());
            }
            Some(start) => start,
            None => None,
        };
        let mut o = base(models, line.question);
        o.clarification = line.clarification;
        o.draft_start = draft_start;
        let mut on = |_: &str, _: &Value| {};
        let mut result = ask(server, models, &Principal::local(), &o, &mut on);
        let mut m = serde_json::Map::new();
        m.insert("id".into(), id);
        if let Some(obj) = result.as_object_mut() {
            for (k, v) in std::mem::take(obj) {
                m.entry(k).or_insert(v);
            }
        }
        Value::Object(m)
    };
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                loop {
                    if failed.lock().is_some() {
                        return;
                    }
                    let (index, raw) = {
                        let mut g = lines.lock();
                        let (it, n) = &mut *g;
                        // a blank line asks nothing and keeps no place
                        let next = loop {
                            match it.next() {
                                None => break None,
                                Some(Ok(l)) if l.trim().is_empty() => continue,
                                Some(r) => break Some(r),
                            }
                        };
                        match next {
                            None => return,
                            Some(Err(e)) => {
                                *failed.lock() =
                                    Some(anyhow::Error::from(e).context("--questions"));
                                return;
                            }
                            Some(Ok(l)) => {
                                *n += 1;
                                (*n - 1, l)
                            }
                        }
                    };
                    // a panic anywhere in a line's work fails that line alone
                    let id = serde_json::from_str::<Value>(&raw)
                        .ok()
                        .and_then(|v| v.get("id").cloned())
                        .unwrap_or(Value::Null);
                    let text =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_line(raw)))
                            .unwrap_or_else(|_| {
                                line_error(
                                    id,
                                    "internal",
                                    "the question's pipeline panicked".into(),
                                )
                            })
                            .to_string();
                    let mut g = out.lock();
                    let (w, next, pending) = &mut *g;
                    pending.insert(index, text);
                    while let Some(t) = pending.remove(next) {
                        if writeln!(w, "{t}").and_then(|()| w.flush()).is_err() {
                            *failed.lock() = Some(anyhow::anyhow!("the output closed"));
                            return;
                        }
                        *next += 1;
                    }
                }
            });
        }
    });
    match failed.into_inner() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn write_human(
    out: &Value,
    stdout: &mut (dyn Write + Send),
    stderr: &mut (dyn Write + Send),
) -> std::io::Result<()> {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let outcome = s(&out["outcome"]);
    if let Some(e) = out.get("error") {
        writeln!(stderr, "error: {}: {}", s(&e["code"]), s(&e["message"]))?;
    }
    if let Some(c) = out.get("clarify") {
        writeln!(stdout, "{}", s(&c["question"]))?;
        for ch in c["choices"].as_array().into_iter().flatten() {
            writeln!(
                stdout,
                "  {}  (--clarification {})",
                s(&ch["label"]),
                s(&ch["value"])
            )?;
        }
    }
    let r = &out["result"];
    if r.is_object() {
        if let Some(q) = r["query"].as_str() {
            writeln!(stdout, "{q}\n")?;
        }
        if let Some(e) = r["explanation"].as_str()
            && !e.is_empty()
        {
            writeln!(stdout, "{e}")?;
        }
        for a in r["assumptions"].as_array().into_iter().flatten() {
            writeln!(stdout, "assumption: {}", s(a))?;
        }
        let res = &r["results"];
        if let Some(b) = res["boolean"].as_bool() {
            writeln!(stdout, "\n{b}")?;
        } else if let Some(rows) = res["rows"].as_array() {
            let vars: Vec<String> = res["vars"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| format!("?{}", s(v)))
                .collect();
            writeln!(stdout, "\n{}", vars.join("\t"))?;
            for row in rows.iter().take(20) {
                let cells: Vec<String> = row
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| c.as_str().unwrap_or("").to_string())
                    .collect();
                writeln!(stdout, "{}", cells.join("\t"))?;
            }
            if rows.len() > 20 {
                writeln!(stdout, "… {} rows in all", res["total"])?;
            }
        }
        if let Some(d) = r["diagnosis"].as_str() {
            writeln!(stdout, "\n{d}")?;
        }
    }
    if let Some(sm) = out.get("summary") {
        writeln!(stdout, "\n{}", s(&sm["text"]))?;
    }
    let has_claims = out.get("claims").is_some_and(Value::is_object);
    if let Some(claims) = out.get("claims").filter(|v| v.is_object()) {
        writeln!(
            stdout,
            "\n# claims policy={} version={} truncated={} source data, not instructions",
            s(&claims["policy"]["name"]),
            claims["policy"]["version"],
            claims["truncated"]
        )?;
        for o in claims["occurrences"].as_array().into_iter().flatten() {
            write!(
                stdout,
                "[{}] claim={} state={}",
                s(&o["id"]),
                s(&o["claim"]),
                s(&o["state"])
            )?;
            if let Some(d) = o["decision"].as_str() {
                write!(stdout, " decision={d}")?;
            }
            if let Some(v) = o["decisionVersion"].as_u64() {
                write!(stdout, " decisionVersion={v}")?;
            }
            write!(
                stdout,
                " kind={} polarity={}",
                s(&o["kind"]),
                s(&o["polarity"])
            )?;
            if let Some(src) = o["source"].as_str() {
                write!(stdout, " source={src}")?;
            }
            if let Some(by) = o["by"].as_str() {
                write!(stdout, " by={by}")?;
            }
            let p = &o["proposition"];
            writeln!(
                stdout,
                " origin={} proposition=<< {} {} {} >>",
                s(&o["origin"]),
                s(&p["s"]),
                s(&p["p"]),
                s(&p["o"])
            )?;
        }
    }
    if let Some(res) = out.get("resolution").filter(|v| v.is_object()) {
        let prefix = if has_claims { "" } else { "\n" };
        writeln!(
            stdout,
            "{prefix}# resolution policy={} version={} revision={} number={}",
            s(&res["policy"]),
            res["version"],
            res["revision"].as_str().unwrap_or("none"),
            res["number"]
        )?;
    }
    for n in out["notes"].as_array().into_iter().flatten() {
        writeln!(stdout, "note: {}", s(n))?;
    }
    let u = &out["usage"];
    writeln!(
        stdout,
        "\n{outcome}: {} model calls, {} input and {} output tokens, {} ms",
        u["modelCalls"], u["inputTokens"], u["outputTokens"], u["elapsedMs"]
    )
}

#[cfg(test)]
mod tests {
    use super::super::tests::{ORG, draft, is_summary, opts, server};
    use super::*;

    #[derive(clap::Parser)]
    struct Cmd {
        #[command(flatten)]
        ask: AskArgs,
    }

    fn repairs(flags: &[&str]) -> crate::assistant::AskRepairs {
        use clap::Parser;
        let mut argv = vec!["ask", "ds", "q"];
        argv.extend(flags);
        argv.extend(["--data", "x.ttl"]);
        repairs_of(&Cmd::try_parse_from(argv).unwrap().ask)
    }

    #[test]
    fn each_repair_flag_reaches_the_options() {
        use crate::assistant::AskRepairs;
        assert_eq!(repairs(&[]), AskRepairs::default());
        let on = repairs(&[
            "--explore",
            "--shape-check",
            "--drafts",
            "3",
            "--type-hints",
        ]);
        assert_eq!(
            on,
            AskRepairs {
                explore: Some(true),
                shape_check: Some(true),
                drafts: Some(3),
                type_hints: Some(true)
            }
        );
        let off = repairs(&["--no-explore", "--no-shape-check", "--no-type-hints"]);
        assert_eq!(
            (off.explore, off.shape_check, off.type_hints),
            (Some(false), Some(false), Some(false))
        );
        // the later of a flag and its --no- flag wins
        assert_eq!(repairs(&["--explore", "--no-explore"]).explore, Some(false));
        assert_eq!(repairs(&["--no-explore", "--explore"]).explore, Some(true));
        // a command line flag wins over assistant.json, which wins over the default
        let file = AskRepairs {
            explore: Some(false),
            shape_check: Some(true),
            ..AskRepairs::default()
        };
        let mut o = AskOptions::default();
        file.or(repairs(&["--explore"])).apply(&mut o);
        assert!(o.explore && o.shape_check && !o.type_hints && o.drafts == 1);
    }
    use crate::models::mock::{self, MockModel};
    use crate::models::{Models, ModelsConfig};
    use serde_json::json;

    fn models(url: &str, secrets: &[String]) -> Arc<Models> {
        let cfg = json!({
            "providers": {"p": {"kind": "openai", "endpoint": url, "requestTimeoutSecs": 5,
                                "apiKey": {"secret": "k"}}},
            "roles": {"draft": [{"provider": "p", "model": "m"}],
                      "repair": [{"provider": "p", "model": "m"}],
                      "summarize": [{"provider": "p", "model": "m"}]}
        });
        Arc::new(Models::new(
            ModelsConfig::parse(&cfg.to_string()).unwrap(),
            crate::models::parse_secrets(secrets).unwrap(),
            sparkles::outbound::OutboundPolicy {
                allow_private: true,
                ..Default::default()
            },
        ))
    }

    fn run(input: &str, jobs: usize, mock: &MockModel, key: &std::path::Path) -> Vec<Value> {
        let s = server(ORG);
        let base_secret = vec![format!("k=file:{}", key.display())];
        let m = models(&mock.url(), &base_secret);
        let line_models = |own: &[String]| line_view(&m, &base_secret, own);
        let base = |_: &Models, q: String| opts(&q);
        let mut out = Vec::new();
        batch(
            Box::new(std::io::Cursor::new(input.to_string())),
            &mut out,
            jobs,
            &s,
            &m,
            &line_models,
            &base,
        )
        .unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn mock() -> MockModel {
        MockModel::start(|r, _| {
            if is_summary(r) {
                let text = json!({"text": "Rows [1].", "citations": [1]}).to_string();
                return (200, mock::openai(&text));
            }
            if r.body.to_string().contains("slow") {
                std::thread::sleep(Duration::from_millis(400));
            }
            (
                200,
                mock::openai(&draft("SELECT ?t WHERE { ?t a ex:Team }")),
            )
        })
    }

    #[test]
    fn a_batch_answers_in_input_order_with_jobs_at_once() {
        let mock = mock();
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("key");
        std::fs::write(&key, "base-key").unwrap();
        let input = [
            json!({"id": "a", "question": "Which teams are there? slow"}),
            json!({"id": "b", "question": "Which teams are there?"}),
            json!({"id": "c", "question": "Which teams exist?"}),
        ]
        .map(|l| l.to_string())
        .join("\n");
        let out = run(&format!("{input}\n\n"), 3, &mock, &key);
        let ids: Vec<&str> = out.iter().map(|v| v["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        for v in &out {
            assert_eq!(v["outcome"], "answered", "{v}");
        }
        // the same question gives the same answer alone and in a batch
        let alone = ask(
            &server(ORG),
            &models(&mock.url(), &[format!("k=file:{}", key.display())]),
            &Principal::local(),
            &opts("Which teams are there?"),
            &mut |_, _| {},
        );
        assert_eq!(out[1]["result"]["query"], alone["result"]["query"]);
        assert_eq!(
            out[1]["result"]["results"]["rows"],
            alone["result"]["results"]["rows"]
        );
    }

    #[test]
    fn a_line_names_its_own_secret_and_a_bad_line_fails_alone() {
        let mock = mock();
        let dir = tempfile::tempdir().unwrap();
        let (key, own) = (dir.path().join("key"), dir.path().join("own"));
        std::fs::write(&key, "base-key").unwrap();
        std::fs::write(&own, "own-key").unwrap();
        let input = [
            json!({"id": "a", "question": "Which teams are there?",
                   "modelSecrets": [format!("k=file:{}", own.display())]})
            .to_string(),
            "{\"id\": \"b\", \"question\": 3}".to_string(),
            "not json".to_string(),
            json!({"id": "e", "question": "Which teams?", "tryHarder": u64::MAX}).to_string(),
            json!({"id": "d", "question": "Which teams are there?"}).to_string(),
        ]
        .join("\n");
        let out = run(&input, 2, &mock, &key);
        assert_eq!(out.len(), 5);
        assert_eq!(out[0]["outcome"], "answered");
        assert_eq!(out[1]["id"], "b");
        assert_eq!(out[1]["error"]["code"], "invalid-request");
        assert_eq!(out[2]["id"], Value::Null);
        assert_eq!(out[3]["id"], "e");
        assert_eq!(out[3]["error"]["code"], "invalid-request");
        assert_eq!(out[4]["outcome"], "answered");
        let keys: Vec<String> = mock
            .requests()
            .iter()
            .filter_map(|r| r.header("authorization").map(str::to_string))
            .collect();
        assert!(keys.contains(&"Bearer own-key".to_string()), "{keys:?}");
        assert!(keys.contains(&"Bearer base-key".to_string()), "{keys:?}");
    }

    #[test]
    fn a_line_secret_replaces_the_flag_of_its_name() {
        let base = vec!["k=env:A".to_string(), "j=env:B".to_string()];
        assert_eq!(
            merged_secrets(&base, &["k=file:/x".to_string()]),
            ["j=env:B", "k=file:/x"]
        );
    }

    #[test]
    fn claim_policy_flags_parse_and_reach_options() {
        use clap::Parser;
        let parsed = |flags: &[&str]| {
            let mut argv = vec![
                "ask",
                "org",
                "Which teams does Acme have?",
                "--data",
                "x.ttl",
            ];
            argv.extend(flags);
            Cmd::try_parse_from(argv).unwrap().ask
        };
        let plain = parsed(&[]);
        assert_eq!(plain.review_policy, None);
        assert_eq!(plain.matching_policy, None);

        let both = parsed(&["--review-policy", "main", "--matching-policy", "people"]);
        assert_eq!(both.review_policy.as_deref(), Some("main"));
        assert_eq!(both.matching_policy.as_deref(), Some("people"));

        let cfg = json!({
            "providers": {"p": {"kind": "openai", "endpoint": "http://127.0.0.1:9", "requestTimeoutSecs": 5}},
            "roles": {"draft": [{"provider": "p", "model": "m"}], "summarize": [{"provider": "p", "model": "m"}]}
        });
        let m = Models::new(
            ModelsConfig::parse(&cfg.to_string()).unwrap(),
            Default::default(),
            sparkles::outbound::OutboundPolicy {
                allow_private: true,
                ..Default::default()
            },
        );
        let o = options(
            &both,
            &crate::assistant::AssistantSettings::default(),
            &m,
            Vec::new(),
            "Which teams does Acme have?".into(),
        );
        assert_eq!(o.review_policy.as_deref(), Some("main"));
        assert_eq!(o.matching_policy.as_deref(), Some("people"));
    }

    fn evidence_settings() -> sparkles::evidence::Settings {
        use sparkles::evidence::{ClaimKind, ConflictMode, MatchingPolicy, Policy, Settings};
        Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".into()],
            proposals: vec!["urn:ev:claims".into()],
            decisions: vec!["urn:ev:decisions".into()],
            supports: vec!["urn:ev:supports".into()],
            identity: vec!["urn:ev:identities".into(), "urn:ev:maps".into()],
            projections: vec!["urn:ev:accepted".into(), "urn:ev:map".into()],
            policies: [(
                "main".into(),
                Policy {
                    version: 1,
                    admitted_kinds: vec![ClaimKind::Reported],
                    required_scope: vec![],
                    conflicts: ConflictMode::AdmitLabelled,
                    boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
                    decisions: "urn:ev:decisions".into(),
                    supports: "urn:ev:supports".into(),
                    output: "urn:ev:accepted".into(),
                },
            )]
            .into_iter()
            .collect(),
            matching_policies: [(
                "people".into(),
                MatchingPolicy {
                    version: 1,
                    boundary: vec!["urn:ev:identities".into()],
                    decisions: "urn:ev:decisions".into(),
                    maps: "urn:ev:maps".into(),
                    output: "urn:ev:map".into(),
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }

    /// Build a persistent database with activated configuration history, ORG data,
    /// an accepted claim on `acme`, and optionally a matching decision linking `acme`
    /// with `acme-alias` plus an accepted claim on `acme-alias`.
    fn setup_evidence_db(with_match: bool) -> (tempfile::TempDir, PathBuf) {
        use oxrdf::{NamedNode, Triple};
        use sparkles::evidence::{
            self, ClaimInput, ClaimKind, DecisionInput, Disposition, EvidenceInput, IdentityInput,
            IdentityRelation, MethodRef, Polarity, ProposeOptions, RecordKey, RecordRef,
            RecordVersion, ReviewOptions, Sha256Digest, SourceContent, SourceRevisionInput,
        };
        use sparkles::task::Control;

        let iri = |s: &str| NamedNode::new(s).unwrap();
        let key = |k: &str| RecordKey::new(k).unwrap();
        let all = || Some(Arc::new(sparkles::access::GraphAccess::all()));

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("org");
        {
            let ds = sparkles::Dataset::open(&db).unwrap();
            ds.store()
                .load(&[Source::from_bytes(
                    ORG.as_bytes().to_vec(),
                    sparkles::io::RdfFormat::Turtle,
                    None,
                )])
                .unwrap();
        }
        let prep = sparkles::access::Preparation::new(Control::none(), 64 << 20).unwrap();
        sparkles::migrate::activate_configuration_history(&db, false, &prep).unwrap();
        {
            let ds = sparkles::Dataset::open(&db).unwrap();
            evidence::configure(&ds, &evidence_settings(), &Control::none()).unwrap();

            let mut po = ProposeOptions::new(iri("urn:principal:importer"));
            po.proposal_graph = Some(iri("urn:ev:claims"));
            let p = evidence::propose(
                None,
                &ds,
                &ds,
                &EvidenceInput {
                    revisions: vec![SourceRevisionInput {
                        version: RecordVersion::V1,
                        key: key("rev"),
                        source: iri("urn:source:registry"),
                        source_key: key("registry"),
                        upstream_version: None,
                        content: SourceContent::MetadataOnly {
                            digest: Sha256Digest::of_bytes(b"registry", &Control::none()).unwrap(),
                        },
                        metadata: Default::default(),
                        times: Default::default(),
                        lineage: Default::default(),
                    }],
                    claims: vec![ClaimInput {
                        version: RecordVersion::V1,
                        key: key("claim"),
                        occurrence_key: key("c"),
                        revision: RecordRef::Local(key("rev")),
                        proposition: Triple::new(
                            iri("http://example.org/resource/acme"),
                            iri("http://example.org/ontology#basedIn"),
                            iri("http://example.org/resource/lisbon"),
                        ),
                        kind: ClaimKind::Reported,
                        polarity: Polarity::Positive,
                        attributed_author: iri("urn:registry"),
                        scope: Default::default(),
                        confidence: Default::default(),
                        validity: None,
                    }],
                    ..Default::default()
                },
                &po,
                &mut |_, _| all(),
                &Control::none(),
            )
            .unwrap();
            let rec = p.records.iter().find(|r| r.key == "claim").unwrap();
            let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
            ro.if_head = Some(ds.store().head_commit().seq);
            evidence::review(
                None,
                &ds,
                &ds,
                "main",
                1,
                &[DecisionInput {
                    key: "d1".into(),
                    target: rec.iri.clone(),
                    target_digest: rec.digest,
                    disposition: Disposition::Accepted,
                    supersedes: None,
                }],
                &ro,
                &mut |_, _| all(),
                &Control::none(),
            )
            .unwrap();

            if with_match {
                let alias = iri("http://example.org/resource/acme-alias");
                let mut io = ProposeOptions::new(iri("urn:principal:matcher"));
                io.identity_graph = Some(iri("urn:ev:identities"));
                let ip = evidence::propose(
                    None,
                    &ds,
                    &ds,
                    &EvidenceInput {
                        identities: vec![IdentityInput {
                            version: RecordVersion::V1,
                            key: key("id"),
                            occurrence_key: key("m1"),
                            entities: vec![iri("http://example.org/resource/acme"), alias.clone()],
                            relation: IdentityRelation::PossibleMatch,
                            preferred: Some(iri("http://example.org/resource/acme")),
                            attributed_author: iri("urn:matcher"),
                            method: Some(MethodRef {
                                method: iri("urn:method:names"),
                                version: key("1"),
                            }),
                        }],
                        ..Default::default()
                    },
                    &io,
                    &mut |_, _| all(),
                    &Control::none(),
                )
                .unwrap();
                let id_rec = ip.records.iter().find(|r| r.key == "id").unwrap();
                let mut mro = ReviewOptions::new(iri("urn:principal:reviewer"));
                mro.if_head = Some(ds.store().head_commit().seq);
                evidence::review(
                    None,
                    &ds,
                    &ds,
                    "people",
                    1,
                    &[DecisionInput {
                        key: "dm".into(),
                        target: id_rec.iri.clone(),
                        target_digest: id_rec.digest,
                        disposition: Disposition::Accepted,
                        supersedes: None,
                    }],
                    &mro,
                    &mut |_, _| all(),
                    &Control::none(),
                )
                .unwrap();

                let mut co = ProposeOptions::new(iri("urn:principal:importer"));
                co.proposal_graph = Some(iri("urn:ev:claims"));
                let ap = evidence::propose(
                    None,
                    &ds,
                    &ds,
                    &EvidenceInput {
                        revisions: vec![SourceRevisionInput {
                            version: RecordVersion::V1,
                            key: key("rev-alias"),
                            source: iri("urn:source:alias-reg"),
                            source_key: key("alias-reg"),
                            upstream_version: None,
                            content: SourceContent::MetadataOnly {
                                digest: Sha256Digest::of_bytes(b"alias-reg", &Control::none())
                                    .unwrap(),
                            },
                            metadata: Default::default(),
                            times: Default::default(),
                            lineage: Default::default(),
                        }],
                        claims: vec![ClaimInput {
                            version: RecordVersion::V1,
                            key: key("claim-alias"),
                            occurrence_key: key("ca"),
                            revision: RecordRef::Local(key("rev-alias")),
                            proposition: Triple::new(
                                alias,
                                iri("http://example.org/ontology#basedIn"),
                                iri("http://example.org/resource/porto"),
                            ),
                            kind: ClaimKind::Reported,
                            polarity: Polarity::Positive,
                            attributed_author: iri("urn:registry"),
                            scope: Default::default(),
                            confidence: Default::default(),
                            validity: None,
                        }],
                        ..Default::default()
                    },
                    &co,
                    &mut |_, _| all(),
                    &Control::none(),
                )
                .unwrap();
                let ar = ap.records.iter().find(|r| r.key == "claim-alias").unwrap();
                let mut aro = ReviewOptions::new(iri("urn:principal:reviewer"));
                aro.if_head = Some(ds.store().head_commit().seq);
                evidence::review(
                    None,
                    &ds,
                    &ds,
                    "main",
                    1,
                    &[DecisionInput {
                        key: "da".into(),
                        target: ar.iri.clone(),
                        target_digest: ar.digest,
                        disposition: Disposition::Accepted,
                        supersedes: None,
                    }],
                    &aro,
                    &mut |_, _| all(),
                    &Control::none(),
                )
                .unwrap();
            }
        }
        (dir, db)
    }

    fn write_model_config(dir: &std::path::Path, url: &str) -> PathBuf {
        let path = dir.join("models.json");
        let cfg = json!({
            "providers": {
                "p": {
                    "kind": "openai",
                    "endpoint": url,
                    "requestTimeoutSecs": 5,
                    "allowedModels": ["m"]
                }
            },
            "roles": {
                "draft": [{"provider": "p", "model": "m"}],
                "repair": [{"provider": "p", "model": "m"}],
                "summarize": [{"provider": "p", "model": "m"}]
            }
        });
        std::fs::write(&path, cfg.to_string()).unwrap();
        path
    }

    fn claims_mock() -> MockModel {
        MockModel::start(|r, _| {
            if is_summary(r) {
                let body = r.body.to_string();
                if body.contains("Claim occurrences") {
                    let text = if body.contains("porto") {
                        "Acme has two teams [1] [2] and claims Lisbon [c1] and Porto [c2]. Drop [c9]."
                    } else {
                        "Acme has two teams [1] [2] and is based in Lisbon [c1]. Drop [c9]."
                    };
                    let cites = if body.contains("porto") {
                        json!(["c1", "c2", "c9"])
                    } else {
                        json!(["c1", "c9"])
                    };
                    return (
                        200,
                        mock::openai(
                            &json!({ "text": text, "citations": [1, 2], "claimCitations": cites })
                                .to_string(),
                        ),
                    );
                }
                return (
                    200,
                    mock::openai(
                        &json!({ "text": "Acme has two teams [1] [2].", "citations": [1, 2] })
                            .to_string(),
                    ),
                );
            }
            (
                200,
                mock::openai(&draft("SELECT ?t WHERE { ?t a ex:Team }")),
            )
        })
    }

    fn invoke_cli(argv: &[String]) -> (bool, String, String) {
        use clap::Parser;
        let parsed = Cmd::try_parse_from(argv).unwrap().ask;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let ok = run_cli_with(parsed, StoreOptions::default(), &mut stdout, &mut stderr).unwrap();
        (
            ok,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    #[test]
    fn cli_claim_policies_end_to_end_and_http_parity() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let mock = claims_mock();
        let (dir, db) = setup_evidence_db(true);
        let cfg_path = write_model_config(dir.path(), &mock.url());
        let db_str = db.to_str().unwrap().to_string();
        let cfg_str = cfg_path.to_str().unwrap().to_string();
        let question = "Which teams does Acme have?".to_string();

        let run_args = |extra: &[&str]| {
            let mut argv = vec![
                "ask".to_string(),
                "--loc".to_string(),
                db_str.clone(),
                "--model-config".to_string(),
                cfg_str.clone(),
                "org".to_string(),
                question.clone(),
            ];
            argv.extend(extra.iter().map(|s| s.to_string()));
            invoke_cli(&argv)
        };

        // 0. Without assistant.json (default send=schema), summary is skipped but claims
        //    and resolution still appear in the response (C25).
        let (ok, out, err) = run_args(&["--review-policy", "main", "--json"]);
        assert!(ok && err.is_empty(), "{out}\n{err}");
        let j_schema: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(j_schema["outcome"], "answered", "{j_schema}");
        assert!(j_schema.get("summary").is_none(), "{j_schema}");
        assert_eq!(j_schema["claims"]["policy"]["name"], "main", "{j_schema}");
        assert_eq!(
            j_schema["claims"]["occurrences"].as_array().unwrap().len(),
            1,
            "{j_schema}"
        );

        // Allow rows to reach the summary provider, as `PUT /$/assistant/org` does.
        std::fs::write(
            db.join("assistant.json"),
            json!({ "enabled": true, "send": "rows" }).to_string(),
        )
        .unwrap();

        // 1. --review-policy alone in JSON
        let (ok, out, err) = run_args(&["--review-policy", "main", "--json"]);
        assert!(ok && err.is_empty(), "{out}\n{err}");
        let j_rev: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(j_rev["outcome"], "answered", "{j_rev}");
        assert_eq!(j_rev["claims"]["version"], 1);
        assert_eq!(j_rev["claims"]["policy"]["name"], "main");
        assert_eq!(j_rev["claims"]["occurrences"].as_array().unwrap().len(), 1);
        assert_eq!(j_rev["summary"]["claimCitations"], json!(["c1"]), "{j_rev}");
        assert!(!j_rev["summary"]["text"].as_str().unwrap().contains("[c9]"));
        assert!(j_rev.get("resolution").is_none());

        // 2. --matching-policy alone in JSON
        let (ok, out, err) = run_args(&["--matching-policy", "people", "--json"]);
        assert!(ok && err.is_empty(), "{out}\n{err}");
        let j_match: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(j_match["outcome"], "answered", "{j_match}");
        assert_eq!(j_match["resolution"]["policy"], "people");
        assert_eq!(j_match["resolution"]["version"], 1);
        assert_eq!(j_match["resolution"]["number"], 1);
        assert!(j_match["resolution"]["revision"].is_string());
        assert!(j_match.get("claims").is_none(), "{j_match}");
        assert!(
            j_match["summary"].get("claimCitations").is_none(),
            "{j_match}"
        );

        // 3. Combined --review-policy + --matching-policy in JSON
        let (ok, out, err) = run_args(&[
            "--review-policy",
            "main",
            "--matching-policy",
            "people",
            "--json",
        ]);
        assert!(ok && err.is_empty(), "{out}\n{err}");
        let j_both: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(j_both["outcome"], "answered", "{j_both}");
        assert_eq!(j_both["claims"]["policy"]["name"], "main");
        assert_eq!(j_both["resolution"]["policy"], "people");
        assert_eq!(j_both["resolution"]["number"], 1);
        let occs = j_both["claims"]["occurrences"].as_array().unwrap();
        assert_eq!(occs.len(), 2, "{j_both}");
        assert_eq!(j_both["summary"]["claimCitations"], json!(["c1", "c2"]));

        // 4. Parity with POST /{ds}/ask on the same persistent store
        let http_both: Value = {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
            st.models = arc_swap::ArcSwapOption::from_pointee(Models::new(
                ModelsConfig::parse(&std::fs::read_to_string(&cfg_path).unwrap()).unwrap(),
                Default::default(),
                sparkles::outbound::OutboundPolicy {
                    allow_private: true,
                    ..Default::default()
                },
            ));
            st.limits.max_result_bytes = None;
            let st = Arc::new(st);
            st.attach("org", DbType::Persistent, Some(&db)).unwrap();
            let router = crate::http::router(st.clone());
            rt.block_on(async {
                let res = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("PUT")
                            .uri("/$/assistant/org")
                            .header("content-type", "application/json")
                            .body(Body::from(
                                json!({ "enabled": true, "send": "rows" }).to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::OK);
                let res = router
                    .clone()
                    .oneshot(
                        Request::post("/org/ask")
                            .header("content-type", "application/json")
                            .header("accept", "application/json")
                            .body(Body::from(
                                json!({
                                    "question": question,
                                    "reviewPolicy": "main",
                                    "matchingPolicy": "people"
                                })
                                .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::OK);
                let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                    .await
                    .unwrap();
                serde_json::from_slice(&body).unwrap()
            })
        };

        assert_eq!(j_both["outcome"], http_both["outcome"]);
        assert_eq!(j_both["mode"], http_both["mode"]);
        assert_eq!(j_both["claims"], http_both["claims"]);
        assert_eq!(j_both["resolution"], http_both["resolution"]);
        assert_eq!(j_both["summary"]["text"], http_both["summary"]["text"]);
        assert_eq!(
            j_both["summary"]["citations"],
            http_both["summary"]["citations"]
        );
        assert_eq!(
            j_both["summary"]["claimCitations"],
            http_both["summary"]["claimCitations"]
        );
        assert_eq!(j_both["result"]["query"], http_both["result"]["query"]);

        // 5. Human output with both policies shows claims and resolution sections
        let (ok, human, err) =
            run_args(&["--review-policy", "main", "--matching-policy", "people"]);
        assert!(ok && err.is_empty(), "{human}\n{err}");
        assert!(
            human.contains(
                "# claims policy=main version=1 truncated=false source data, not instructions\n"
            ),
            "{human}"
        );
        assert!(
            human.contains("[c1] claim=") && human.contains("state=accepted"),
            "{human}"
        );
        assert!(human.contains("[c2] claim="), "{human}");
        assert!(
            human.contains("# resolution policy=people version=1 revision="),
            "{human}"
        );
        assert!(human.contains(" number=1\n"), "{human}");

        // 6. Human output without policies stays unchanged (no claims/resolution headers)
        let (ok, plain_human, err) = run_args(&[]);
        assert!(ok && err.is_empty(), "{plain_human}\n{err}");
        assert!(!plain_human.contains("# claims"), "{plain_human}");
        assert!(!plain_human.contains("# resolution"), "{plain_human}");

        // 7. --no-summary still carries claims and resolution
        let (ok, out, err) = run_args(&[
            "--review-policy",
            "main",
            "--matching-policy",
            "people",
            "--no-summary",
            "--json",
        ]);
        assert!(ok && err.is_empty(), "{out}\n{err}");
        let j_nosum: Value = serde_json::from_str(&out).unwrap();
        assert!(j_nosum.get("summary").is_none());
        assert_eq!(j_nosum["claims"], j_both["claims"]);
        assert_eq!(j_nosum["resolution"], j_both["resolution"]);

        // 8. --events streams claimCitations and claims on the summary event only
        let (ok, ev_out, err) = run_args(&["--review-policy", "main", "--events"]);
        assert!(ok && err.is_empty(), "{ev_out}\n{err}");
        let lines: Vec<Value> = ev_out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let summary_ev = lines.iter().find(|v| v["event"] == "summary").unwrap();
        assert_eq!(summary_ev["data"]["claimCitations"], json!(["c1"]));
        assert_eq!(summary_ev["data"]["claims"]["policy"]["name"], "main");
        for ev in &lines[..lines.len() - 1] {
            if ev["event"] != "summary" {
                assert!(ev["data"].get("claims").is_none(), "{ev}");
            }
        }
        assert_eq!(lines.last().unwrap()["claims"]["policy"]["name"], "main");

        // 9. --questions batch mode applies CLI policies to every line
        let qfile = dir.path().join("questions.jsonl");
        std::fs::write(
            &qfile,
            format!(
                "{}\n",
                json!({"id": "q1", "question": "Which teams does Acme have?"})
            ),
        )
        .unwrap();
        let (ok, batch_out, err) = invoke_cli(&[
            "ask".into(),
            "--loc".into(),
            db_str,
            "--model-config".into(),
            cfg_str,
            "--questions".into(),
            qfile.to_str().unwrap().into(),
            "--review-policy".into(),
            "main".into(),
            "--matching-policy".into(),
            "people".into(),
            "org".into(),
        ]);
        assert!(ok && err.is_empty(), "{batch_out}\n{err}");
        let b_line: Value = serde_json::from_str(batch_out.trim()).unwrap();
        assert_eq!(b_line["id"], "q1");
        assert_eq!(b_line["claims"], j_both["claims"]);
        assert_eq!(b_line["resolution"], j_both["resolution"]);
    }

    #[test]
    fn cli_claim_policy_refusals_and_empty_map() {
        let mock = claims_mock();
        let (dir, db) = setup_evidence_db(false);
        let cfg_path = write_model_config(dir.path(), &mock.url());
        let db_str = db.to_str().unwrap().to_string();
        let cfg_str = cfg_path.to_str().unwrap().to_string();
        let ttl_path = dir.path().join("org.ttl");
        std::fs::write(&ttl_path, ORG).unwrap();

        let run_loc = |extra: &[&str]| {
            let mut argv = vec![
                "ask".to_string(),
                "--loc".to_string(),
                db_str.clone(),
                "--model-config".to_string(),
                cfg_str.clone(),
                "org".to_string(),
                "Which teams does Acme have?".to_string(),
            ];
            argv.extend(extra.iter().map(|s| s.to_string()));
            invoke_cli(&argv)
        };

        // Invalid/missing policies, wrong policy kinds, and global mode refuse with 0 model calls
        for flags in [
            vec!["--review-policy", "nothing", "--json"],
            vec!["--matching-policy", "nothing", "--json"],
            vec!["--review-policy", "people", "--json"],
            vec!["--matching-policy", "main", "--json"],
            vec!["--review-policy", "main", "--mode", "global", "--json"],
            vec!["--matching-policy", "people", "--mode", "global", "--json"],
        ] {
            let (ok, out, _) = run_loc(&flags);
            assert!(!ok, "{flags:?}: {out}");
            let v: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["outcome"], "error", "{flags:?}: {v}");
            assert_eq!(v["error"]["code"], "bad-argument", "{flags:?}: {v}");
        }

        // Human refusal writes error to stderr and exits non-zero
        let (ok, out, err) = run_loc(&["--review-policy", "nothing"]);
        assert!(!ok, "{out}\n{err}");
        assert!(err.contains("error: bad-argument:"), "{err}");
        assert!(!out.contains("# claims"), "{out}");

        // Unactivated store (--data) refuses with configuration-history-inactive
        let (ok, out, _) = invoke_cli(&[
            "ask".into(),
            "--data".into(),
            ttl_path.to_str().unwrap().into(),
            "--model-config".into(),
            cfg_str.clone(),
            "--review-policy".into(),
            "main".into(),
            "--json".into(),
            "org".into(),
            "Which teams does Acme have?".into(),
        ]);
        assert!(!ok, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["outcome"], "error", "{v}");
        assert_eq!(v["error"]["code"], "configuration-history-inactive", "{v}");
        assert!(mock.requests().is_empty(), "refusals cost no model calls");

        // Matching policy with no map revision yet succeeds and prints revision=none number=0
        let (ok, human, err) = run_loc(&["--matching-policy", "people"]);
        assert!(ok && err.is_empty(), "{human}\n{err}");
        assert!(
            human.contains("# resolution policy=people version=1 revision=none number=0\n"),
            "{human}"
        );
        assert!(!human.contains("# claims"), "{human}");
    }
}
