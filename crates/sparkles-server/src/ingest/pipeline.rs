//! One ingestion, as its caller (spec C18 §7): conversion, registration on the review
//! branch, the estimate and its confirmation, extraction, linking and the proposals.
//!
//! The same [`run`] serves `POST /$/ingest/{ds}`, which runs it as a task and reports
//! its progress, and `sparkles ingest`. A document that cannot be converted, such as a
//! PDF that needs OCR on a server without it, ends the run before anything is written:
//! no branch, no source.

use super::convert::{self, Converted, Format, Refusal};
use super::{PdfRuntime, Status, Task};
use crate::auth::{Level, Principal};
use crate::mcp::errors::ToolError;
use crate::mcp::{Call, McpServer, Outcome};
use crate::models::{Models, Pair, Role, StepRecord};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The default estimate above which a task waits for confirmation, in tokens (§7.9).
pub const DEFAULT_CONFIRM_TOKENS: u64 = 200_000;
/// The default confidence that `auto` mode needs of every fact (§7.7).
pub const DEFAULT_AUTO_CONFIDENCE: f64 = 0.8;

/// The review modes of §7.7.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// proposals on a scratch branch for a person to review and merge
    #[default]
    Branch,
    /// nothing written: the proposals wait in the task for an approval
    Preview,
    /// written to `main` when every check passes, else as `branch`
    Auto,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "branch" => Some(Mode::Branch),
            "preview" => Some(Mode::Preview),
            "auto" => Some(Mode::Auto),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Branch => "branch",
            Mode::Preview => "preview",
            Mode::Auto => "auto",
        }
    }
}

/// What to ingest and how.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub dataset: String,
    pub bytes: Vec<u8>,
    /// the file's name, which may say its format
    pub name: Option<String>,
    /// the declared media type
    pub media_type: Option<String>,
    /// where the document came from; the source's IRI by default
    pub url: Option<String>,
    pub title: Option<String>,
    /// the source's IRI
    pub iri: Option<String>,
    /// the named graph of the source and its facts
    pub graph: Option<String>,
    pub profile: Option<String>,
    pub mode: Mode,
    /// the review branch (default: `ingest.<slug>-<n>`, or `proposals.<agent>.ingest-…`
    /// for an agent of `memory.json`)
    pub branch: Option<String>,
    pub allow_partial: bool,
    /// extract facts with the `extract` role (default: when the dataset lets ingestion
    /// use its providers)
    pub extract: Option<bool>,
    /// the estimate is confirmed in advance
    pub confirm: bool,
    /// the namespace of the rows of a table's mapping draft
    pub base: Option<String>,
    pub message: Option<String>,
    /// the extract role's pairs, instead of the dataset's (`sparkles ingest --pair`)
    pub pairs: Vec<Pair>,
}

/// Where a run reports progress and waits for its confirmation.
pub trait Progress {
    fn status(&self, status: Status, progress: f32, message: Option<String>);
    fn estimate(&self, estimate: &Value);
    /// wait for the confirmation of the estimate: `false` when it did not come
    fn confirm(&self) -> bool;
    /// keep what an approval of a preview writes
    fn approval(&self, a: Approval);
}

/// A task reports to itself.
impl Progress for Task {
    fn status(&self, status: Status, progress: f32, message: Option<String>) {
        self.set(status, progress, message);
    }

    fn estimate(&self, estimate: &Value) {
        self.update(|s| s.estimate = Some(estimate.clone()));
    }

    fn confirm(&self) -> bool {
        let progress = self.state.lock().progress;
        self.set(
            Status::AwaitingConfirmation,
            progress,
            Some("the estimate is above the dataset's threshold: confirm to go on".into()),
        );
        self.wait_confirmation(super::CONFIRM_WAIT)
    }

    fn approval(&self, a: Approval) {
        self.update(|s| s.approval = Some(a));
    }
}

/// The command line reports nothing and never waits: an estimate above the threshold
/// needs `--confirm`.
pub struct Quiet;

impl Progress for Quiet {
    fn status(&self, _: Status, _: f32, _: Option<String>) {}
    fn estimate(&self, _: &Value) {}
    fn confirm(&self) -> bool {
        false
    }
    fn approval(&self, _: Approval) {}
}

/// What the approval of a preview writes to `main` (§7.7).
#[derive(Clone, Debug)]
pub struct Approval {
    /// `register_converted`'s arguments
    pub register: Map<String, Value>,
    /// `assert_facts` calls, in order, with `ifHead` set on the first
    pub asserts: Vec<Map<String, Value>>,
    /// `main`'s head when the preview was made
    pub head: u64,
}

/// What a run needs.
pub struct Ctx<'a> {
    pub server: &'a McpServer,
    pub models: Option<&'a Models>,
    pub principal: &'a Principal,
    pub progress: &'a dyn Progress,
    pub deadline: Instant,
    pub cancel: Arc<AtomicBool>,
    pub pdf: Arc<PdfRuntime>,
    pub request_id: String,
}

/// A failure of a run: its code, status and message, with details.
pub struct Failed(pub Value);

impl Failed {
    pub fn new(code: &str, message: impl Into<String>) -> Failed {
        Failed(json!({ "code": code, "message": message.into() }))
    }
}

impl From<ToolError> for Failed {
    fn from(e: ToolError) -> Failed {
        let mut j = json!({ "code": e.code, "message": e.message, "status": e.status });
        if let Some(h) = e.hint {
            j["hint"] = h.into();
        }
        if let Some(d) = e.data {
            j["data"] = d;
        }
        Failed(j)
    }
}

impl From<Refusal> for Failed {
    fn from(r: Refusal) -> Failed {
        let mut j = r.json();
        j["status"] = r.status.into();
        Failed(j)
    }
}

/// The steps a run's model calls took.
#[derive(Default)]
pub struct Usage {
    pub steps: Vec<StepRecord>,
    pub escalations: Vec<Value>,
    pub chunks: Vec<Value>,
}

impl Usage {
    pub fn json(&self, started: Instant) -> Value {
        let input: u64 = self.steps.iter().map(|s| s.input_tokens).sum();
        let output: u64 = self.steps.iter().map(|s| s.output_tokens).sum();
        let mut j = json!({
            "modelCalls": self.steps.iter().map(|s| u64::from(s.requests)).sum::<u64>(),
            "inputTokens": input,
            "outputTokens": output,
            "elapsedMs": started.elapsed().as_millis() as u64,
            "steps": self.steps.iter().map(StepRecord::json).collect::<Vec<_>>(),
        });
        let costs: Vec<f64> = self.steps.iter().filter_map(|s| s.estimated_cost).collect();
        if !costs.is_empty() {
            j["estimatedCost"] = json!((costs.iter().sum::<f64>() * 1e6).round() / 1e6);
        }
        if !self.escalations.is_empty() {
            j["escalations"] = self.escalations.clone().into();
        }
        if !self.chunks.is_empty() {
            j["chunks"] = self.chunks.clone().into();
        }
        j
    }
}

pub(crate) struct Run<'a, 'c> {
    pub ctx: &'a Ctx<'c>,
    pub dataset: String,
    pub usage: Usage,
}

impl Run<'_, '_> {
    pub fn cancelled(&self) -> bool {
        self.ctx.cancel.load(Ordering::Relaxed)
    }

    pub fn check(&self) -> Result<(), Failed> {
        if self.cancelled() {
            return Err(Failed::new("cancelled", "the ingestion was cancelled"));
        }
        if Instant::now() >= self.ctx.deadline {
            return Err(Failed::new("timeout", "the ingestion's deadline passed"));
        }
        Ok(())
    }

    /// Call `name` as the caller, on `branch` when given.
    pub fn tool(&self, name: &str, args: Value, branch: Option<&str>) -> Result<Value, ToolError> {
        let mut m = match args {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        m.insert("dataset".into(), self.dataset.clone().into());
        let left = self.ctx.deadline.saturating_duration_since(Instant::now());
        let secs = left.min(self.ctx.server.cfg().max_timeout).as_secs_f64();
        if secs < 0.05 {
            return Err(ToolError::new(
                "timeout",
                504,
                "the ingestion's deadline passed",
            ));
        }
        if !m.contains_key("timeoutSeconds") && !matches!(name, "create_branch" | "delete_branch") {
            m.insert("timeoutSeconds".into(), json!(secs));
        }
        let call = Call {
            arrived: Instant::now(),
            cancel: self.ctx.cancel.clone(),
            request_id: self.ctx.request_id.clone(),
            principal: self.ctx.principal.clone().on_branch(branch),
            headers: None,
            held: None,
        };
        Ok(match self.ctx.server.run_now(name, m, &call)? {
            Outcome::Structured(v) => v,
            Outcome::Text(t) => serde_json::from_str(&t).unwrap_or(Value::String(t)),
        })
    }
}

/// A lower-case slug of a title or file name for branch names: letters, digits and `-`,
/// at most 40.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "document".into()
    } else {
        out
    }
}

/// The name a file or URL gives a document, without its directories.
fn base_name(s: &str) -> &str {
    let s = s.split(['?', '#']).next().unwrap_or(s);
    let s = s.trim_end_matches('/');
    s.rsplit('/').next().unwrap_or(s)
}

/// Run one ingestion. The value is the result; the usage is reported either way.
pub fn run(ctx: &Ctx, req: &Request) -> (Result<Value, Value>, Value) {
    let started = Instant::now();
    let mut r = Run {
        ctx,
        dataset: req.dataset.clone(),
        usage: Usage::default(),
    };
    let out = run_inner(&mut r, req).map_err(|f| f.0);
    let usage = r.usage.json(started);
    (out, usage)
}

/// The dataset's `ingest.json` members of Phase 4.
pub(crate) struct Limits {
    pub confirm_tokens: u64,
    pub auto_confidence: f64,
}

fn run_inner(r: &mut Run, req: &Request) -> Result<Value, Failed> {
    let ctx = r.ctx;
    let st = &ctx.server.state;
    let ds = ctx
        .server
        .dataset(ctx.principal, Some(&req.dataset))
        .map_err(Failed::from)?;
    let name = req
        .name
        .clone()
        .or_else(|| req.url.as_deref().map(|u| base_name(u).to_string()));
    let format = Format::detect(req.media_type.as_deref(), name.as_deref(), &req.bytes)?;
    if matches!(format, Format::Csv | Format::Tsv) {
        return super::csv::draft(r, req, &ds, format, name.as_deref());
    }
    if req.mode == Mode::Auto && !ctx.principal.can(&ds.name, Level::Admin) {
        return Err(Failed::new(
            "forbidden",
            "only an admin of the dataset may ingest in auto mode",
        ));
    }
    // the providers this ingestion may use
    let settings = crate::assistant::settings(st, &ds);
    let pairs = if req.pairs.is_empty() {
        extract_pairs(ctx.models, &settings)
    } else {
        req.pairs.clone()
    };
    let extract = match req.extract {
        Some(true) if pairs.is_empty() => {
            return Err(Failed::new(
                "no-model",
                "the dataset's ingestion has no provider and model in the extract role: enable ingest in its assistant settings, with a provider that may receive documents",
            ));
        }
        Some(x) => x,
        None => !pairs.is_empty(),
    };
    // 1. conversion
    ctx.progress.status(Status::Converting, 0.05, None);
    r.check()?;
    let converted = convert::convert(
        &req.bytes,
        format,
        &convert::Options {
            allow_partial: req.allow_partial,
            max_input: convert::MAX_INPUT_BYTES,
            max_text: convert::MAX_TEXT_BYTES,
            deadline: ctx.deadline,
            pdf: ctx.pdf.clone(),
        },
    )?;
    r.check()?;
    let title = req
        .title
        .clone()
        .or_else(|| converted.title.clone())
        .or_else(|| name.clone());
    let iri = req.iri.clone().or_else(|| {
        req.url
            .clone()
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
    });
    let profile = req.profile.clone().unwrap_or_else(|| "default".into());
    let ingest = crate::mcp::memory::ingest::ingest_settings(st, &ds);
    let limits = Limits {
        confirm_tokens: ingest.confirm_tokens.unwrap_or(DEFAULT_CONFIRM_TOKENS),
        auto_confidence: ingest.auto_confidence.unwrap_or(DEFAULT_AUTO_CONFIDENCE),
    };
    // 2. the estimate, before anything is written
    let chunks = crate::mcp::memory::ingest::chunk_bounds(&converted.text);
    let vocab = if extract {
        Some(super::extract::Vocabulary::read(r, &profile)?)
    } else {
        None
    };
    if let (Some(v), Some(models)) = (&vocab, ctx.models) {
        let est = super::extract::estimate(models, &pairs, v, &converted.text, &chunks);
        let mut e = est.clone();
        e["threshold"] = limits.confirm_tokens.into();
        let tokens = est["tokens"].as_u64().unwrap_or(0);
        e["needsConfirmation"] = (tokens > limits.confirm_tokens).into();
        ctx.progress.estimate(&e);
        budget_check(st, &ds, &settings, ctx.principal)?;
        if tokens > limits.confirm_tokens && !req.confirm && !ctx.progress.confirm() {
            return Err(Failed(json!({
                "code": if r.cancelled() { "cancelled" } else { "not-confirmed" },
                "message": format!(
                    "the extraction would take about {tokens} tokens, above the dataset's threshold of {}, and was not confirmed",
                    limits.confirm_tokens
                ),
                "estimate": e,
            })));
        }
    }
    r.check()?;
    // 3. registration, on the review branch (a preview writes nothing)
    ctx.progress.status(Status::Registering, 0.15, None);
    let mut reg = Map::new();
    if let Some(g) = &req.graph {
        reg.insert("graph".into(), g.clone().into());
    }
    if let Some(i) = &iri {
        reg.insert("iri".into(), i.clone().into());
    }
    if let Some(t) = &title {
        reg.insert(
            "title".into(),
            t.chars().take(1000).collect::<String>().into(),
        );
    }
    reg.insert("format".into(), converted.media_type.clone().into());
    reg.insert("text".into(), converted.text.clone().into());
    reg.insert("profile".into(), profile.clone().into());
    if let Some(m) = &req.message {
        reg.insert("message".into(), m.clone().into());
    }
    reg.insert("rendition".into(), rendition_extras(&converted));
    let base_head = ds.store.snapshot().commit;
    let branch = match req.mode {
        Mode::Preview => None,
        Mode::Branch | Mode::Auto => Some(review_branch(r, req, &ds, title.as_deref())?),
    };
    let registered = if req.mode == Mode::Preview {
        let mut dry = reg.clone();
        dry.insert("dryRun".into(), true.into());
        r.tool("register_converted", Value::Object(dry), None)?
    } else {
        match r.tool(
            "register_converted",
            Value::Object(reg.clone()),
            branch.as_deref(),
        ) {
            Ok(v) => v,
            Err(e) => {
                discard_branch(r, req, branch.as_deref());
                return Err(e.into());
            }
        }
    };
    if registered["alreadyRegistered"] == true {
        discard_branch(r, req, branch.as_deref());
        return Ok(json!({
            "outcome": "already-registered",
            "format": format.name(),
            "source": registered["source"],
            "graph": registered["graph"],
            "rendition": registered["rendition"],
            "digest": registered["digest"],
            "length": registered["length"],
            "message": "the same text is already registered in this graph; nothing was written",
            "prefixes": registered["prefixes"],
        }));
    }
    let rendition = registered["rendition"].as_str().unwrap_or("").to_string();
    let mut out = json!({
        "outcome": "registered",
        "mode": req.mode.as_str(),
        "format": format.name(),
        "source": registered["source"],
        "graph": registered["graph"],
        "rendition": rendition,
        "digest": registered["digest"],
        "length": registered["length"],
        "chunks": chunks.len(),
        "profile": profile,
    });
    if let Some(b) = &branch {
        out["branch"] = b.clone().into();
        out["review"] = format!(
            "/ui/datasets/{}/review/{}",
            percent_encoding::utf8_percent_encode(&ds.name, percent_encoding::NON_ALPHANUMERIC),
            percent_encoding::utf8_percent_encode(b, percent_encoding::NON_ALPHANUMERIC)
        )
        .into();
    }
    if let Some(c) = registered.get("commit") {
        out["commit"] = c.clone();
    }
    if let Some(p) = registered.get("previousRendition") {
        out["previousRendition"] = p.clone();
        out["staleFacts"] = registered["staleFacts"].clone();
    }
    pages_json(&converted, &mut out);
    if !converted.notes.is_empty() {
        out["notes"] = converted.notes.clone().into();
    }
    if let Some(p) = registered.get("prefixes") {
        out["prefixes"] = p.clone();
    }
    let Some(vocab) = vocab else {
        if !extract && req.extract.is_none() {
            out["notes"] = json!([
                "no fact was extracted: the dataset's assistant does not let ingestion use a provider"
            ]);
        }
        return Ok(out);
    };
    let models = ctx.models.expect("an extraction has providers");
    // 6. extraction, linking and the proposals
    let ex = super::extract::extract(r, models, &pairs, &vocab, &converted.text, &chunks)?;
    let proposals = super::extract::propose(
        r,
        &vocab,
        &ex,
        &registered,
        branch.as_deref(),
        req.mode == Mode::Preview,
        &converted,
    )?;
    let facts_ok = proposals.summary["proposed"].as_u64().unwrap_or(0);
    out["outcome"] = if facts_ok > 0 { "proposed" } else { "no-facts" }.into();
    for (k, v) in proposals.summary.as_object().into_iter().flatten() {
        out[k] = v.clone();
    }
    match req.mode {
        Mode::Preview => {
            out["outcome"] = "preview".into();
            out["head"] = base_head.into();
            let mut asserts = proposals.calls.clone();
            if let Some(first) = asserts.first_mut() {
                first.insert("ifHead".into(), (base_head + 1).into());
            }
            ctx.progress.approval(Approval {
                register: reg,
                asserts,
                head: base_head,
            });
        }
        Mode::Auto => {
            let b = branch.as_deref().expect("auto has a branch");
            let message = format!("Ingest {}", title.as_deref().unwrap_or("a document"));
            let mut why = auto_blocker(&proposals, &limits);
            // the merge preview: the guard of main and conflicts keep the branch
            let preview = if why.is_none() {
                let p = r.tool(
                    "merge_branch",
                    json!({ "source": b, "message": message }),
                    None,
                )?;
                if p["mergeable"] != true {
                    why = Some("the merge preview found conflicts or guard findings".into());
                }
                Some(p)
            } else {
                None
            };
            match (why, preview) {
                (None, Some(p)) => {
                    let m = r.tool(
                        "merge_branch",
                        json!({ "source": b, "message": message, "dryRun": false,
                                "expect": p["expect"] }),
                        None,
                    )?;
                    let _ = r.tool("delete_branch", json!({ "name": b, "force": true }), None);
                    out["outcome"] = "merged".into();
                    out["merge"] = m;
                    if let Some(o) = out.as_object_mut() {
                        o.remove("review");
                    }
                }
                (w, _) => {
                    out["autoFallback"] = w.unwrap_or_default().into();
                }
            }
        }
        Mode::Branch => {}
    }
    ctx.progress.status(Status::Writing, 0.99, None);
    Ok(out)
}

/// Why `auto` mode keeps the proposals on the branch, if it does (§7.7).
fn auto_blocker(p: &super::extract::Proposals, limits: &Limits) -> Option<String> {
    let s = &p.summary;
    if s["proposed"].as_u64().unwrap_or(0) == 0 {
        return Some("no fact was proposed".into());
    }
    if s["failed"].as_array().is_some_and(|f| !f.is_empty()) {
        return Some("some facts failed their checks".into());
    }
    if s["entities"]["ambiguous"].as_u64().unwrap_or(0) > 0 {
        return Some("an entity is ambiguous".into());
    }
    if p.guard_findings {
        return Some("the dataset's guard reported findings".into());
    }
    if p.min_confidence < limits.auto_confidence {
        return Some(format!(
            "a fact's confidence is below {}",
            limits.auto_confidence
        ));
    }
    None
}

/// The pages, OCR pages and omitted pages of a rendition, for `register_converted`.
fn rendition_extras(c: &Converted) -> Value {
    json!({
        "pageStarts": c.page_starts,
        "ocrPages": c.ocr_pages,
        "omittedPages": c.omitted_pages,
    })
}

fn pages_json(c: &Converted, out: &mut Value) {
    if !c.page_starts.is_empty() {
        out["pages"] = c
            .page_starts
            .iter()
            .filter_map(|s| {
                let page = convert::page_of(&c.text, &c.page_starts, *s)?;
                Some(json!({ "page": page, "start": s }))
            })
            .collect::<Vec<_>>()
            .into();
    }
    if !c.ocr_pages.is_empty() {
        out["ocrPages"] = c.ocr_pages.clone().into();
    }
    if !c.omitted_pages.is_empty() {
        out["omittedPages"] = c.omitted_pages.clone().into();
    }
}

/// The `extract` role's pairs for a dataset: none unless its assistant is enabled with
/// `ingest`, and only providers that may receive documents (§3.5).
pub fn extract_pairs(
    models: Option<&Models>,
    s: &crate::assistant::AssistantSettings,
) -> Vec<Pair> {
    let Some(models) = models else {
        return Vec::new();
    };
    if !s.enabled || !s.ingest {
        return Vec::new();
    }
    let lists = crate::assistant::lists(models, s);
    lists
        .get(&Role::Extract)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| s.send_to(&p.provider) >= crate::assistant::Send::Documents)
        .collect()
}

/// A daily cap of the dataset that is already spent (§3.5) refuses the extraction
/// before any call.
fn budget_check(
    st: &crate::state::AppState,
    ds: &crate::state::Dataset,
    s: &crate::assistant::AssistantSettings,
    p: &Principal,
) -> Result<(), Failed> {
    let over = |used: u64, cap: Option<u64>| cap.is_some_and(|c| used >= c);
    if over(
        st.asks.tokens_today(&ds.name, None),
        s.budget.per_dataset_per_day,
    ) || over(
        st.asks.tokens_today(&ds.name, Some(&p.id())),
        s.budget.per_principal_per_day,
    ) {
        return Err(Failed::new(
            "budget-exceeded",
            "this dataset's model budget for today is used up",
        ));
    }
    Ok(())
}

/// Create the review branch: the request's, or `ingest.<slug>-<n>` (an agent of
/// `memory.json` gets `proposals.<agent>.ingest-<slug>-<n>`, which its grants cover).
fn review_branch(
    r: &Run,
    req: &Request,
    ds: &Arc<crate::state::Dataset>,
    title: Option<&str>,
) -> Result<String, Failed> {
    let st = &r.ctx.server.state;
    let note = format!("Ingestion of {}", title.unwrap_or("a document"));
    let create = |name: &str| r.tool("create_branch", json!({ "name": name, "note": note }), None);
    if let Some(b) = &req.branch {
        if st.branch_dataset(ds, b).is_ok() {
            return Ok(b.clone());
        }
        create(b)?;
        return Ok(b.clone());
    }
    let s = slug(title.unwrap_or("document"));
    let settings = crate::assist::memory_settings(st, ds);
    let caller = r.ctx.principal.caller();
    let agent = std::iter::once(caller.user.clone())
        .flatten()
        .chain(caller.roles.iter().cloned())
        .find(|n| settings.agents.contains_key(n));
    let stem = match agent {
        Some(a) => format!("proposals.{a}.ingest-{s}"),
        None => format!("ingest.{s}"),
    };
    let existing: std::collections::BTreeSet<String> = ds
        .store
        .branches()
        .map(|l| l.into_iter().map(|b| b.name).collect())
        .unwrap_or_default();
    for n in 1..1000 {
        let name = format!("{stem}-{n}");
        if existing.contains(&name) || !sparkles::branch::valid_name(&name) {
            continue;
        }
        match create(&name) {
            Ok(_) => return Ok(name),
            // created meanwhile by another ingestion
            Err(e) if e.code == "branch-exists" || e.status == 409 => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(Failed::new(
        "no-branch",
        format!("no free branch name {stem}-N"),
    ))
}

/// Delete a review branch that this run created and that holds nothing.
fn discard_branch(r: &Run, req: &Request, branch: Option<&str>) {
    if let Some(b) = branch
        && req.branch.as_deref() != Some(b)
    {
        let _ = r.tool("delete_branch", json!({ "name": b, "force": true }), None);
    }
}

/// Approve a preview: register the source and write its facts on `main`, unless `main`
/// moved since the preview (`ifHead`).
pub fn approve(ctx: &Ctx, dataset: &str, a: &Approval) -> Result<Value, Value> {
    let r = Run {
        ctx,
        dataset: dataset.to_string(),
        usage: Usage::default(),
    };
    let ds = ctx
        .server
        .dataset(ctx.principal, Some(dataset))
        .map_err(|e| Failed::from(e).0)?;
    let head = ds.store.snapshot().commit;
    if head != a.head {
        return Err(json!({
            "code": "conflict",
            "status": 409,
            "message": format!("the dataset changed since the preview (head {} then, {head} now): ingest the document again", a.head),
        }));
    }
    let reg = r
        .tool(
            "register_converted",
            Value::Object(a.register.clone()),
            None,
        )
        .map_err(|e| Failed::from(e).0)?;
    let mut commits = Vec::new();
    for call in &a.asserts {
        let out = r
            .tool("assert_facts", Value::Object(call.clone()), None)
            .map_err(|e| Failed::from(e).0)?;
        if let Some(c) = out.get("commit") {
            commits.push(c.clone());
        }
    }
    Ok(json!({
        "outcome": "approved",
        "source": reg["source"],
        "rendition": reg["rendition"],
        "commits": commits,
    }))
}

/// How long a run may take by default.
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(3600);
