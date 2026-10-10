//! What waits for review in agent memory, counted cheaply enough to report all the time:
//! the metrics `sparkles_memory_review_pending` and `sparkles_memory_review_oldest_seconds`,
//! the `review` member of `GET /$/memory/{ds}/maintenance`, the review lines of the brief
//! and the `sparkles://{ds}/memory/review` resource.
//!
//! The counts follow the inbox (§8.9). A session fact is open while `main` asserts it
//! only in agent graphs, and a review branch is open while it has commits of its own.
//! Facts are counted as the server, up to [`MAX_COUNTED`], without the inbox's signals.
//! Each count is kept per dataset with a fingerprint of what it read: the dataset's head
//! commit, its branches and its memory settings. The maintenance tick refreshes every
//! dataset with memory settings once a minute, and the tools that change the inbox
//! (`assert_facts`, the reviewer's actions, merges and branch deletions) refresh their
//! dataset on a thread of their own. A refresh whose fingerprint did not change reads
//! nothing.
//!
//! A fact's age is the time of its reifier. A fact without one, such as an imported
//! memory, is as old as the time the server first counted its graph, which is kept in the
//! dataset's `review.json` so that a restart does not reset it.

use super::inbox::{asserting_graphs, plain_facts, reified_facts, review_kind};
use crate::auth::{Level, Principal, on_branch};
use crate::mcp::errors::ToolError;
use crate::mcp::render::escape_into;
use crate::mcp::tools::Tools;
use crate::mcp::{Call, McpServer};
use crate::state::{AppState, Dataset};
use oxrdf::NamedNode;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// The most unreviewed facts a count reads.
pub(crate) const MAX_COUNTED: usize = 5000;
/// The open branches whose proposed facts a refresh counts, newest first.
const MAX_BRANCH_FACTS: usize = 10;
/// The longest a refresh runs.
const TIMEOUT: Duration = Duration::from_secs(30);
/// The first times the server counted graphs whose facts have no time.
const SEEN_FILE: &str = "review.json";

/// The kinds of open items: the two kinds of session facts, then the review branches by
/// the kinds of the inbox.
pub(crate) const KINDS: [&str; 7] = [
    "session",
    "import",
    "consolidation",
    "ingest",
    "review",
    "inbox",
    "proposal",
];

/// The open items of one kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct KindCount {
    pub open: u64,
    /// the oldest open item, in milliseconds since the Unix epoch
    pub oldest_ms: Option<i64>,
}

impl KindCount {
    fn add(&mut self, at: Option<i64>) {
        self.open += 1;
        if let Some(t) = at {
            self.oldest_ms = Some(self.oldest_ms.map_or(t, |o| o.min(t)));
        }
    }
}

/// An open review branch.
#[derive(Clone, Debug)]
pub(crate) struct OpenBranch {
    pub name: String,
    pub kind: &'static str,
    pub created_ms: i64,
    /// the facts it proposes, for the newest [`MAX_BRANCH_FACTS`] branches
    pub facts: Option<usize>,
}

/// The open review items of one dataset.
#[derive(Clone, Debug)]
pub(crate) struct Counts {
    fingerprint: u64,
    /// session facts by kind (`session`, `import`)
    pub facts: BTreeMap<&'static str, KindCount>,
    /// open review branches, newest first
    pub branches: Vec<OpenBranch>,
    /// more facts than [`MAX_COUNTED`] wait
    pub truncated: bool,
    pub updated_ms: i64,
}

impl Counts {
    /// Every kind with its count, branches included (all of them, or those `visible`).
    pub fn kinds(&self, visible: impl Fn(&str) -> bool) -> BTreeMap<&'static str, KindCount> {
        let mut out = self.facts.clone();
        for b in self.branches.iter().filter(|b| visible(&b.name)) {
            out.entry(b.kind).or_default().add(Some(b.created_ms));
        }
        out
    }
}

/// The counts of a server's datasets (`ingest::Runtime::review`).
#[derive(Default)]
pub struct Registry {
    counts: Mutex<HashMap<String, Arc<Counts>>>,
    /// datasets a refresh thread works on, and whether another refresh was asked for
    busy: Mutex<HashMap<String, bool>>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn hash<T: Hash>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

/// Whether the dataset has memory settings that make an inbox: agent graphs or a
/// consolidated graph.
fn configured(m: &crate::assist::MemorySettings) -> bool {
    !m.agent_graphs.is_empty() || m.consolidated_graph.is_some()
}

/// What a count of `ds` reads: the dataset, its head, its branches and its settings.
fn fingerprint(ds: &Arc<Dataset>, m: &crate::assist::MemorySettings) -> u64 {
    let branches: Vec<(String, u64, Option<u64>)> = ds
        .store
        .branches()
        .unwrap_or_default()
        .into_iter()
        .filter(|b| review_kind(&b.name).is_some())
        .map(|b| (b.name, b.ahead, b.head.map(|h| h.seq)))
        .collect();
    hash(&(
        Arc::as_ptr(ds) as usize,
        ds.store.head_commit().seq,
        branches,
        serde_json::to_string(m).unwrap_or_default(),
    ))
}

/// The time of an `xsd:dateTime` lexical form, in milliseconds (UTC when it has no zone).
fn parse_time(t: &str) -> Option<i64> {
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(t) {
        return Some(d.timestamp_millis());
    }
    chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|d| d.and_utc().timestamp_millis())
}

impl Registry {
    /// The counts of dataset `name`, when it has memory settings and was counted.
    pub(crate) fn get(&self, name: &str) -> Option<Arc<Counts>> {
        self.counts.lock().get(name).cloned()
    }

    /// The counted datasets, by name.
    pub(crate) fn all(&self) -> BTreeMap<String, Arc<Counts>> {
        self.counts
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// Count the open review items of `ds` again unless nothing they depend on changed, and
/// keep the result. A dataset without memory settings is forgotten.
pub(crate) fn refresh(st: &Arc<AppState>, ds: &Arc<Dataset>) -> Option<Arc<Counts>> {
    let reg = &st.ingest.review;
    let memory = crate::assist::memory_settings(st, ds);
    if !configured(&memory) {
        reg.counts.lock().remove(&ds.name);
        return None;
    }
    let fp = fingerprint(ds, &memory);
    if let Some(c) = reg.get(&ds.name)
        && c.fingerprint == fp
    {
        return Some(c);
    }
    match count(st, ds, &memory, fp) {
        Ok(c) => {
            let c = Arc::new(c);
            reg.counts.lock().insert(ds.name.clone(), c.clone());
            Some(c)
        }
        Err(e) => {
            tracing::warn!(dataset = %ds.name, code = %e.code, "counting the review inbox: {}", e.message);
            reg.get(&ds.name)
        }
    }
}

/// The counts of `ds`: those kept, else a count now.
pub(crate) fn current(st: &Arc<AppState>, ds: &Arc<Dataset>) -> Option<Arc<Counts>> {
    st.ingest.review.get(&ds.name).or_else(|| refresh(st, ds))
}

/// Refresh every dataset, and forget the datasets that are gone (the maintenance tick).
pub fn refresh_all(st: &Arc<AppState>) {
    let datasets = st.datasets();
    st.ingest
        .review
        .counts
        .lock()
        .retain(|name, _| datasets.contains_key(name));
    for ds in datasets.values() {
        refresh(st, ds);
    }
}

/// Refresh dataset `name` on a thread of its own, after a change to its inbox. A refresh
/// asked for while one runs runs once more after it.
pub(crate) fn touch(st: &Arc<AppState>, name: &str) {
    let name = crate::auth::split_branch(name).0.to_string();
    let Some(ds) = st.datasets().get(&name).cloned() else {
        return;
    };
    if !configured(&crate::assist::memory_settings(st, &ds)) {
        st.ingest.review.counts.lock().remove(&name);
        return;
    }
    {
        let mut busy = st.ingest.review.busy.lock();
        if let Some(again) = busy.get_mut(&name) {
            *again = true;
            return;
        }
        busy.insert(name.clone(), false);
    }
    let st2 = st.clone();
    let spawned = std::thread::Builder::new()
        .name("review-count".into())
        .spawn(move || {
            loop {
                refresh(&st2, &ds);
                let mut busy = st2.ingest.review.busy.lock();
                match busy.get_mut(&ds.name) {
                    Some(again) if *again => *again = false,
                    _ => {
                        busy.remove(&ds.name);
                        break;
                    }
                }
            }
        });
    if spawned.is_err() {
        st.ingest.review.busy.lock().remove(&name);
    }
}

/// Count the open items of `ds` as the server.
fn count(
    st: &Arc<AppState>,
    ds: &Arc<Dataset>,
    memory: &crate::assist::MemorySettings,
    fingerprint: u64,
) -> Result<Counts, ToolError> {
    let server = McpServer::new(
        st.clone(),
        crate::mcp::rest::config(st, crate::mcp::rest::Mode::Internal),
    );
    let call = Call {
        arrived: Instant::now(),
        cancel: Arc::new(AtomicBool::new(false)),
        request_id: String::new(),
        principal: Principal::local(),
        headers: None,
        held: None,
    };
    let t = Tools {
        server: &server,
        call: &call,
    };
    let ctx = t.ctx(&[], TIMEOUT.as_secs_f64());
    let deadline = call.arrived + TIMEOUT;
    let r = t.reader(ds, None, None, Some(false), deadline, &ctx)?;
    let eng = |e| ctx.engine(e);
    // the unreviewed facts, as the inbox finds them
    let mut agent_graphs: Vec<NamedNode> = Vec::new();
    if !memory.agent_graphs.is_empty() {
        let q = "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } } LIMIT 5000";
        for row in r.rows(q, Vec::new()).map_err(eng)? {
            if let Some(Some(oxrdf::Term::NamedNode(g))) = row.first()
                && memory.is_agent_graph(g.as_str())
            {
                agent_graphs.push(g.clone());
            }
        }
    }
    let mut facts = Vec::new();
    for chunk in agent_graphs.chunks(100) {
        let room = (MAX_COUNTED + 1).saturating_sub(facts.len());
        if room == 0 {
            break;
        }
        facts.extend(reified_facts(&r, chunk, true, room).map_err(eng)?);
    }
    for chunk in agent_graphs.chunks(100) {
        let room = (MAX_COUNTED + 1).saturating_sub(facts.len());
        if room == 0 {
            break;
        }
        facts.extend(plain_facts(&r, chunk, room).map_err(eng)?);
    }
    let truncated = facts.len() > MAX_COUNTED;
    facts.truncate(MAX_COUNTED);
    let graphs_of = asserting_graphs(&r, &facts).map_err(eng)?;
    facts.retain(|f| {
        graphs_of
            .get(&format!("{} {} {}", f.s, f.p, f.o))
            .is_some_and(|gs| {
                gs.contains(f.g.as_str()) && gs.iter().all(|g| memory.is_agent_graph(g))
            })
    });
    // the graphs whose facts have no time are as old as their first count
    let mut seen: BTreeMap<String, i64> = crate::assist::read_file(st, ds, SEEN_FILE)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v["firstSeen"].clone()).ok())
        .unwrap_or_default();
    let before = seen.clone();
    let now = now_ms();
    let import_base = memory.imports.as_ref().map(|i| i.base.as_str());
    let mut by_kind: BTreeMap<&'static str, KindCount> = BTreeMap::new();
    let mut timeless: BTreeMap<String, i64> = BTreeMap::new();
    for f in &facts {
        let g = f.g.as_str();
        let kind = if import_base.is_some_and(|b| g.starts_with(b)) {
            "import"
        } else {
            "session"
        };
        let at = match f.time.as_deref().and_then(parse_time) {
            Some(t) => t,
            None => *timeless
                .entry(g.to_string())
                .or_insert_with(|| seen.get(g).copied().unwrap_or(now)),
        };
        by_kind.entry(kind).or_default().add(Some(at));
    }
    seen = timeless;
    if seen != before
        && !st.read_only
        && let Err(e) = crate::assist::write_file(st, ds, SEEN_FILE, &json!({ "firstSeen": seen }))
    {
        tracing::warn!(dataset = %ds.name, "{e:#}");
    }
    // the open review branches, newest first
    let mut branches: Vec<OpenBranch> = ds
        .store
        .branches()
        .unwrap_or_default()
        .into_iter()
        .filter(|b| b.ahead > 0)
        .filter_map(|b| {
            Some(OpenBranch {
                kind: review_kind(&b.name)?,
                created_ms: b.created_ms,
                name: b.name,
                facts: None,
            })
        })
        .collect();
    branches.sort_by_key(|b| std::cmp::Reverse(b.created_ms));
    for b in branches.iter_mut().take(MAX_BRANCH_FACTS) {
        if Instant::now() >= deadline {
            break;
        }
        b.facts = t
            .branch_counts(ds, &b.name, deadline, &ctx)
            .ok()
            .map(|(a, _)| a);
    }
    Ok(Counts {
        fingerprint,
        facts: by_kind,
        branches,
        truncated,
        updated_ms: now,
    })
}

/// Whether `p` may review the inbox of `ds`: it may write to the dataset.
pub(crate) fn may_review(p: &Principal, ds: &str) -> bool {
    p.clone()
        .on_branch(None)
        .level(ds)
        .is_some_and(|l| l >= Level::Write)
}

/// Whether `p` gets the `sparkles://{ds}/memory/review` resource: `ds` has memory
/// settings and `p` may review its inbox.
pub(crate) fn offered(st: &AppState, ds: &Dataset, p: &Principal) -> bool {
    may_review(p, &ds.name) && configured(&crate::assist::memory_settings(st, ds))
}

/// The state of the review resource of `ds` for `p`, from the kept counts only (the
/// change notifications look at it every few seconds): `None` when `p` does not get it.
pub(crate) fn state(st: &AppState, ds: &Dataset, p: &Principal) -> Option<u64> {
    if !may_review(p, &ds.name) {
        return None;
    }
    let c = st.ingest.review.get(&ds.name)?;
    let pb = p.clone().on_branch(None);
    let kinds: Vec<(&str, u64, Option<i64>)> = c
        .kinds(|b| pb.level(&on_branch(&ds.name, b)).is_some())
        .into_iter()
        .map(|(k, n)| (k, n.open, n.oldest_ms))
        .collect();
    Some(hash(&kinds))
}

/// The review counts of `ds` as `p` sees them, in JSON: `None` when the dataset has no
/// memory settings or `p` may not review it.
pub(crate) fn review_json(st: &Arc<AppState>, ds: &Arc<Dataset>, p: &Principal) -> Option<Value> {
    if !may_review(p, &ds.name) {
        return None;
    }
    let c = current(st, ds)?;
    let pb = p.clone().on_branch(None);
    let visible = |b: &str| pb.level(&on_branch(&ds.name, b)).is_some();
    let kinds = c.kinds(visible);
    let open: u64 = kinds.values().map(|k| k.open).sum();
    let oldest = kinds.values().filter_map(|k| k.oldest_ms).min();
    let mut j = json!({
        "open": open,
        "kinds": kinds
            .iter()
            .filter(|(_, k)| k.open > 0)
            .map(|(name, k)| {
                let mut e = json!({ "open": k.open });
                if let Some(t) = k.oldest_ms {
                    e["oldest"] = sparkles::commit::rfc3339_ms(t).into();
                }
                (name.to_string(), e)
            })
            .collect::<serde_json::Map<_, _>>(),
        "branches": c
            .branches
            .iter()
            .filter(|b| visible(&b.name))
            .map(|b| {
                let mut e = json!({
                    "name": b.name,
                    "kind": b.kind,
                    "created": sparkles::commit::rfc3339_ms(b.created_ms),
                });
                if let Some(n) = b.facts {
                    e["facts"] = n.into();
                }
                e
            })
            .collect::<Vec<_>>(),
        "truncated": c.truncated,
        "updated": sparkles::commit::rfc3339_ms(c.updated_ms),
    });
    if let Some(t) = oldest {
        j["oldest"] = sparkles::commit::rfc3339_ms(t).into();
    }
    Some(j)
}

/// The most branches the brief names one by one.
const BRIEF_BRANCHES: usize = 5;

/// The review lines of the brief: one per open review branch, then one per kind of
/// session fact. Every line begins with `# review:` and its names are escaped.
pub(crate) fn brief_lines(review: &Value) -> String {
    let mut out = String::new();
    let since = |t: &Value| t.as_str().map_or(String::new(), |t| format!(" since {t}"));
    let branches = review["branches"].as_array().cloned().unwrap_or_default();
    for b in branches.iter().take(BRIEF_BRANCHES) {
        let mut name = String::new();
        escape_into(&mut name, b["name"].as_str().unwrap_or(""));
        let kind = b["kind"].as_str().unwrap_or("review");
        let what = match (kind, b["facts"].as_u64()) {
            ("consolidation", Some(n)) => format!(
                "{n} consolidated {} on branch {name} {}",
                if n == 1 { "fact" } else { "facts" },
                if n == 1 { "awaits" } else { "await" }
            ),
            (k, Some(n)) => format!(
                "{n} proposed {} on {k} branch {name} {}",
                if n == 1 { "fact" } else { "facts" },
                if n == 1 { "awaits" } else { "await" }
            ),
            (k, None) => format!("{k} branch {name} awaits"),
        };
        out.push_str(&format!(
            "# review: {what} review{}\n",
            since(&b["created"])
        ));
    }
    if branches.len() > BRIEF_BRANCHES {
        out.push_str(&format!(
            "# review: {} more review branches are open\n",
            branches.len() - BRIEF_BRANCHES
        ));
    }
    for (kind, label) in [("session", "unreviewed session"), ("import", "imported")] {
        let k = &review["kinds"][kind];
        if let Some(n) = k["open"].as_u64().filter(|n| *n > 0) {
            out.push_str(&format!(
                "# review: {n} {label} {} {} review{}\n",
                if n == 1 { "fact" } else { "facts" },
                if n == 1 { "awaits" } else { "await" },
                since(&k["oldest"])
            ));
        }
    }
    out
}

/// One series of the review metrics: `(dataset label, kind, open, oldest age in seconds)`.
pub type Series = (String, &'static str, u64, f64);

/// The review series of the counted datasets, every kind of each, with the datasets past
/// `--metrics-max-datasets` added up under their shared label.
pub fn series(st: &AppState) -> Vec<Series> {
    let now = now_ms();
    let mut agg: BTreeMap<(String, &'static str), KindCount> = BTreeMap::new();
    for (name, c) in st.ingest.review.all() {
        let label = st.metrics.dataset_label(Some(&name));
        let kinds = c.kinds(|_| true);
        for kind in KINDS {
            let k = kinds.get(kind).copied().unwrap_or_default();
            let e = agg.entry((label.clone(), kind)).or_default();
            e.open += k.open;
            if let Some(t) = k.oldest_ms.filter(|_| k.open > 0) {
                e.oldest_ms = Some(e.oldest_ms.map_or(t, |o| o.min(t)));
            }
        }
    }
    agg.into_iter()
        .map(|((ds, kind), k)| {
            let age = k
                .oldest_ms
                .map_or(0.0, |t| ((now - t).max(0) as f64) / 1000.0);
            (ds, kind, k.open, age)
        })
        .collect()
}

/// Append the review families to a Prometheus exposition (nothing while no dataset
/// with memory settings was counted).
pub fn metrics(st: &AppState, out: &mut String) {
    use std::fmt::Write;
    let s = series(st);
    if s.is_empty() {
        return;
    }
    let esc = |v: &str| {
        v.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let _ = writeln!(
        out,
        "# HELP sparkles_memory_review_pending Open items of the agent memory review inbox: unreviewed facts, or open review branches."
    );
    let _ = writeln!(out, "# TYPE sparkles_memory_review_pending gauge");
    for (ds, kind, open, _) in &s {
        let _ = writeln!(
            out,
            "sparkles_memory_review_pending{{dataset=\"{}\",kind=\"{kind}\"}} {open}",
            esc(ds)
        );
    }
    let _ = writeln!(
        out,
        "# HELP sparkles_memory_review_oldest_seconds Age of the oldest open item of the agent memory review inbox, 0 when none is open."
    );
    let _ = writeln!(out, "# TYPE sparkles_memory_review_oldest_seconds gauge");
    for (ds, kind, _, age) in &s {
        let _ = writeln!(
            out,
            "sparkles_memory_review_oldest_seconds{{dataset=\"{}\",kind=\"{kind}\"}} {age:.3}",
            esc(ds)
        );
    }
}

/// The review series for the JSON snapshot of `/$/metrics?format=json` (and OTel).
pub fn series_json(st: &AppState) -> Value {
    series(st)
        .into_iter()
        .map(|(ds, kind, open, age)| {
            json!({ "dataset": ds, "kind": kind, "pending": open, "oldestSeconds": age })
        })
        .collect::<Vec<_>>()
        .into()
}
