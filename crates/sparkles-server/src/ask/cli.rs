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
        let policy = args.outbound.local_policy()?;
        // a line's own secrets give it its own models, read from the same file
        let line_models = |own: &[String]| -> Result<Arc<crate::models::Models>> {
            let margs = crate::models::ModelArgs {
                model_config: args.models.model_config.clone(),
                model_secret: merged_secrets(&args.models.model_secret, own),
            };
            margs.load(policy.clone())?.context("no --model-config")
        };
        let mut stdout = std::io::stdout();
        return batch(
            input,
            &mut stdout,
            args.jobs,
            &server,
            &models,
            &line_models,
            &base,
        );
    }
    let mut o = base(&models, args.question.clone().unwrap_or_default());
    o.clarification = args.clarification.clone();
    o.draft_start = args.try_harder.map(|i| i + 1);
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
    AskOptions {
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
        complexity_threshold: routing
            .complexity_threshold
            .or(model_routing.complexity_threshold)
            .unwrap_or(super::DEFAULT_COMPLEXITY_THRESHOLD),
        example_score: routing
            .example_score
            .or(model_routing.example_score)
            .unwrap_or(super::DEFAULT_EXAMPLE_SCORE),
        ..AskOptions::default()
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
        let mut o = base(models, line.question);
        o.clarification = line.clarification;
        o.draft_start = line.try_harder.map(|i| i + 1);
        let mut on = |_: &str, _: &Value| {};
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ask(server, models, &Principal::local(), &o, &mut on)
        }));
        let mut result = match result {
            Ok(v) => v,
            Err(_) => return line_error(id, "internal", "the question's pipeline panicked".into()),
        };
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
                    let text = run_line(raw).to_string();
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

#[cfg(test)]
mod tests {
    use super::super::tests::{ORG, draft, is_summary, opts, server};
    use super::*;
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
        let line_models = |own: &[String]| -> Result<Arc<Models>> {
            Ok(models(&mock.url(), &merged_secrets(&base_secret, own)))
        };
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
            json!({"id": "d", "question": "Which teams are there?"}).to_string(),
        ]
        .join("\n");
        let out = run(&input, 2, &mock, &key);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0]["outcome"], "answered");
        assert_eq!(out[1]["id"], "b");
        assert_eq!(out[1]["error"]["code"], "invalid-request");
        assert_eq!(out[2]["id"], Value::Null);
        assert_eq!(out[3]["outcome"], "answered");
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
}
