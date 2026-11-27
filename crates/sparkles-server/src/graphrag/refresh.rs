//! Automatic refresh with an internal F13 accumulator circuit (spec C22 §5, F13 §3.6).
//!
//! A loop on the server's runtime looks at every dataset whose GraphRAG settings have
//! `refresh.mode: "auto"` once a [`TICK`]. For each such dataset with a build on `main`,
//! the server attaches an internal F13 accumulator circuit (`graphrag-refresh`) over the
//! projection's graphs and predicates, read through the settings' principal's view. The
//! views worker advances the accumulator incrementally on every commit and checkpoints it
//! under `<db>/circuits/graphrag-refresh/main/`.
//!
//! A dataset is due after `debounceSeconds` without a new commit, or after ten debounce
//! periods while commits continue. A due dataset reads the accumulator's counts since the
//! commit of its last build or refresh and totals them per stored community. When a
//! community's changed facts reach `minChangedFraction` of the facts its report was
//! written from, when an entity joins a community, or when the accumulator cannot cover
//! every commit (a bulk commit, a change-log gap, memory eviction, a circuit failure, or
//! data-dependent protections), the loop starts a refresh task. The refresh compares the
//! fact digests of every report, as a manual refresh does, so it rewrites exactly the
//! reports that changed enough and keeps the rest, then discards the acted-on counts. It
//! needs no confirmation, and the daily token budget bounds it.
//!
//! The loop never starts a first build, never runs on a branch other than main, detaches
//! the internal circuit when automatic refresh is disabled or a dataset is removed or
//! replaced, and leaves a dataset alone while one of its GraphRAG tasks is queued or
//! running. A run that the budget stopped with pending communities is retried once a day.
//!
//! Under a review policy, a report's input can change without any change that the
//! principal's view lists (spec C25, "Refresh"). The loop then runs a private check in
//! place of a refresh: when a commit changes the policy's decisions or supports graph or
//! adds a withdrawal to a proposal graph, which it reads from the feed without the
//! principal's view, when the selected evidence configuration or the access policy's
//! generation differs from the one it last checked, when it first sees the dataset, and
//! when the accumulator cannot cover every commit. The check is a refresh's preparation
//! and plan with no model call, commit, state change or task, and it starts a refresh
//! only for a change of a report's input. It keeps its own backoff, so a check that fails
//! never holds back what the principal's view starts.

use super::build::{self, Mode};
use super::runtime::RefreshCircuit;
use super::settings::{GraphragSettings, RefreshMode};
use super::state::BuildState;
use crate::auth::Endpoint;
use crate::state::{AppState, Dataset, task_state};
use oxrdf::{GraphName, Term};
use sparkles::commit::CommitKind;
use sparkles::incr::{Counts, InternalAccess, InternalCircuit, InternalSpec};
use sparkles::store::ChangesOptions;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// How often the loop looks at the datasets.
pub const TICK: Duration = Duration::from_secs(1);

/// The internal F13 circuit identifier used for GraphRAG change counting.
pub(crate) const CIRCUIT_ID: &str = "graphrag-refresh";

/// The default memory budget of the refresh accumulator circuit (64 MiB), further
/// bounded by `--views-memory` when configured.
const CIRCUIT_MEMORY_BYTES: u64 = 64 << 20;

/// How long a due check waits for the views worker to catch up to the head commit.
const CIRCUIT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long replacing an existing refresh circuit waits for the worker to collect the
/// dropped handle before re-attaching under the same id.
const CIRCUIT_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

/// The most exact graph IRIs expanded into a per-graph `UNION` so the F13 footprint
/// skips commits to unrelated graphs in one hash probe.
const MAX_EXACT_GRAPHS_UNION: usize = 16;

/// The most commits one page of the evidence feed lists.
const PAGE_COMMITS: usize = 1000;

/// The most changes one page of the evidence feed lists.
const PAGE_QUADS: u64 = 200_000;

/// How long one reading of the evidence feed may take.
const FEED_DEADLINE: Duration = Duration::from_secs(60);

const RDF_TYPE: &str = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";

/// The class of a claim's withdrawal.
const WITHDRAWAL: &str = "urn:x-sparkles:evidence:Withdrawal";

/// What the loop remembers of one dataset.
#[derive(Debug, Default)]
pub struct Watch {
    /// the dataset instance this watch tracks, so an in-place restore or recreate resets it
    dataset_id: Option<uuid::Uuid>,
    runtime: Option<Weak<sparkles::incr::Runtime>>,
    /// the head commit seen last
    seen: Option<u64>,
    /// when the last new commit was seen
    last_change: Option<Instant>,
    /// when the first commit since the last check was seen
    first_pending: Option<Instant>,
    /// the day a run that left pending communities was last retried
    retried_day: Option<u64>,
    /// the last check found communities pending
    has_pending: bool,
    /// checks or refreshes that failed in a row, and when to try again
    failures: u32,
    retry_at: Option<Instant>,
    /// the refresh task this loop started last, until it ends
    task: Option<String>,
    /// when the review policy was last compared with the last build's, which needs no
    /// commit, and whether the last build read under one
    policy_checked: Option<Instant>,
    had_policy: bool,
    /// what the eager circuit attachment last synced against, avoiding disk I/O on idle ticks
    attached_settings: Option<GraphragSettings>,
    attached_epoch: Option<u64>,
    attached_head: Option<u64>,
    attached_uncountable: bool,
    no_build_head: Option<u64>,
    /// the private check under a review policy: whether one is due, the head through
    /// which the evidence graphs' changes were read, the evidence configuration and the
    /// access policy's generation it last saw, and its own backoff
    private: Private,
}

/// What the loop remembers for the private check of one dataset.
#[derive(Debug, Default)]
pub(crate) struct Private {
    pub(crate) due: bool,
    evidence_seen: Option<u64>,
    configuration: Option<String>,
    access: Option<u64>,
    /// the head that the last check which found nothing read at least
    covered: Option<u64>,
    failures: u32,
    retry_at: Option<Instant>,
    /// the checks run, for tests
    pub(crate) runs: u64,
}

impl Private {
    /// Whether a due check may run at `now`, after its own backoff.
    fn ready(&self, now: Instant) -> bool {
        self.due && self.retry_at.is_none_or(|t| now >= t)
    }

    fn failed(&mut self, now: Instant, debounce: Duration) {
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(now + debounce * (1u32 << self.failures.min(6)));
    }

    fn done(&mut self) {
        self.due = false;
        self.failures = 0;
        self.retry_at = None;
    }

    /// The principal's feed cannot count the changes up to `head`: a check is due unless
    /// one already covered that head, so an idle dataset is not checked on every pass.
    fn uncounted(&mut self, head: u64) {
        if self.covered != Some(head) {
            self.due = true;
        }
    }
}

impl Watch {
    /// A fresh watch bound to `ds`.
    fn for_dataset(ds: &Dataset) -> Self {
        Self {
            dataset_id: Some(ds.store.dataset_id()),
            runtime: Some(Arc::downgrade(&ds.dataset.inner_state().views)),
            ..Default::default()
        }
    }

    /// Whether this watch still tracks the exact same open dataset instance.
    fn same_dataset(&self, ds: &Dataset) -> bool {
        self.dataset_id == Some(ds.store.dataset_id())
            && self.runtime.as_ref().is_some_and(|w| {
                w.strong_count() > 0
                    && Weak::ptr_eq(w, &Arc::downgrade(&ds.dataset.inner_state().views))
            })
    }

    /// Clear cached attachment tracking so the next tick re-evaluates attachment state.
    fn clear_attached(&mut self) {
        self.attached_settings = None;
        self.attached_epoch = None;
        self.attached_head = None;
        self.attached_uncountable = false;
    }

    /// The private check's record, for tests.
    pub(crate) fn private(&self) -> &Private {
        &self.private
    }

    /// Follow the refresh this loop started, whose task is in `state`: whether it is
    /// still queued or running. A failure backs off. Only a refresh that ended well
    /// read everything a due private check would have read, so a cancelled or missing
    /// task leaves the check due.
    fn follow(&mut self, state: Option<&str>, head: u64, now: Instant, debounce: Duration) -> bool {
        match state {
            Some(task_state::QUEUED | task_state::RUNNING) => {
                self.due(head, now, debounce);
                return true;
            }
            Some(task_state::FAILED) => {
                self.failed(now, debounce);
                // the failed run used no daily retry of pending communities
                self.retried_day = None;
            }
            Some(task_state::DONE) => {
                self.failures = 0;
                self.private.done();
            }
            _ => self.failures = 0,
        }
        self.task = None;
        false
    }

    /// Note the head at `now`; whether a check is due.
    fn due(&mut self, head: u64, now: Instant, debounce: Duration) -> bool {
        match self.seen {
            // a dataset seen for the first time is checked once after a debounce, so
            // commits made while the server was down count
            None => {
                self.seen = Some(head);
                self.last_change = Some(now);
                self.first_pending = Some(now);
            }
            Some(s) if s != head => {
                self.seen = Some(head);
                self.last_change = Some(now);
                self.first_pending.get_or_insert(now);
            }
            Some(_) => {}
        }
        match (self.last_change, self.first_pending) {
            (Some(last), Some(first)) => {
                now.duration_since(last) >= debounce || now.duration_since(first) >= debounce * 10
            }
            _ => false,
        }
    }

    /// Whether to check now: after the debounce of new commits, at a retry after a
    /// failure, or on a new day while communities are pending, which needs no commit.
    fn wants_check(&mut self, head: u64, now: Instant, debounce: Duration, today: u64) -> bool {
        let commits = self.due(head, now, debounce);
        // an outstanding backoff holds every trigger back
        match self.retry_at {
            Some(t) if now < t => false,
            Some(_) => true,
            None => commits || (self.has_pending && self.retried_day != Some(today)),
        }
    }

    fn checked(&mut self) {
        self.last_change = None;
        self.first_pending = None;
        self.retry_at = None;
    }

    /// A check or a refresh failed: try again after a backoff that doubles up to 64
    /// debounce periods, without waiting for another commit.
    fn failed(&mut self, now: Instant, debounce: Duration) {
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(now + debounce * (1u32 << self.failures.min(6)));
    }
}

/// Start the loop on `h`. It ends with the runtime.
pub fn start(st: &Arc<AppState>, h: &tokio::runtime::Handle) {
    if st.read_only {
        return;
    }
    let st = st.clone();
    h.spawn(async move {
        let mut watches: HashMap<String, Watch> = HashMap::new();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let st2 = st.clone();
            let w = std::mem::take(&mut watches);
            match tokio::task::spawn_blocking(move || {
                let mut w = w;
                tick_once(&st2, &mut w, Instant::now());
                w
            })
            .await
            {
                Ok(w) => watches = w,
                Err(e) => tracing::warn!(target: "sparkles::graphrag", "automatic refresh: {e}"),
            }
        }
    });
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TickHookPoint {
    BeforeRetain,
    AfterSettings,
    BeforeEnsureLock,
    BeforeDue,
    BeforePrivateCheck,
    BeforeSpawn,
}

#[cfg(test)]
type TickHook = Box<dyn FnMut(TickHookPoint, &str)>;

#[cfg(test)]
thread_local! {
    static TICK_HOOK: std::cell::RefCell<Option<TickHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct TickHookGuard;

#[cfg(test)]
impl Drop for TickHookGuard {
    fn drop(&mut self) {
        TICK_HOOK.with(|h| *h.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) fn set_tick_hook(f: impl FnMut(TickHookPoint, &str) + 'static) -> TickHookGuard {
    TICK_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    TickHookGuard
}

#[cfg(test)]
fn fire_tick_hook(point: TickHookPoint, name: &str) {
    let f = TICK_HOOK.with(|h| h.borrow_mut().take());
    if let Some(mut f) = f {
        f(point, name);
        TICK_HOOK.with(|h| {
            if h.borrow().is_none() {
                *h.borrow_mut() = Some(f);
            }
        });
    }
}

/// One look at every dataset: keep refresh circuits attached and start the refreshes that
/// are due.
pub fn tick_once(st: &Arc<AppState>, watches: &mut HashMap<String, Watch>, now: Instant) {
    let datasets = st.datasets();
    watches.retain(|name, _| datasets.contains_key(name));
    #[cfg(test)]
    fire_tick_hook(TickHookPoint::BeforeRetain, "");
    st.graphrag.retain_circuits(
        |name| datasets.contains_key(name),
        |name, dataset_id, runtime| is_live_owner(st, name, dataset_id, runtime),
    );
    for (name, ds) in datasets {
        if !is_live_dataset(st, &ds) {
            watches.remove(&name);
            detach_if_not_live(st, &ds);
            continue;
        }
        let settings: GraphragSettings =
            crate::settings::effective(st, &ds, &crate::settings::GRAPHRAG);
        #[cfg(test)]
        fire_tick_hook(TickHookPoint::AfterSettings, &name);
        if !settings.enabled || settings.refresh.mode != RefreshMode::Auto {
            watches.remove(&name);
            detach_if_not_auto(st, &ds);
            continue;
        }
        let head = ds.store.head_commit().seq;
        let w = watches
            .entry(name.clone())
            .or_insert_with(|| Watch::for_dataset(&ds));
        if !w.same_dataset(&ds) {
            *w = Watch::for_dataset(&ds);
            st.graphrag
                .detach_if_not_live_owner(&name, |n, dataset_id, runtime| {
                    is_live_owner(st, n, dataset_id, runtime)
                });
        }
        // Keep the internal F13 accumulator attached ahead of the debounce window so
        // commits arriving after startup, settings updates or policy reloads are counted
        // incrementally rather than falling back.
        let (settings, head) = match ensure_attached(st, &ds, &settings, head, w) {
            AttachState::Active(s, h) => (*s, h),
            AttachState::Inactive | AttachState::NotLive => {
                watches.remove(&name);
                continue;
            }
        };
        let debounce = Duration::from_secs(settings.refresh.debounce_seconds);
        // the refresh this loop started: wait for it, and back off when it failed
        if let Some(id) = &w.task {
            let state = st
                .tasks
                .lock()
                .iter()
                .find(|t| &t.id == id)
                .map(|t| t.state.clone());
            if w.follow(state.as_deref(), head, now, debounce) {
                continue;
            }
        }
        // a review policy that changed, or a new version of it, makes the dataset due
        // without a commit, so it is compared once a debounce period
        let policy_due = (settings.review_policy.is_some() || w.had_policy)
            && w.retry_at.is_none_or(|t| now >= t)
            && w.policy_checked
                .is_none_or(|t| now.duration_since(t) >= debounce);
        // a private check that failed is retried after its own backoff, which an
        // outstanding backoff of the loop holds back as well
        let private_retry = settings.review_policy.is_some()
            && w.private.ready(now)
            && w.retry_at.is_none_or(|t| now >= t);
        if !w.wants_check(head, now, debounce, build::today()) && !policy_due && !private_retry {
            continue;
        }
        // a running or queued build or refresh goes first, and the check waits for it
        if st.active_task(build::TASK_KIND, &name).is_some() || st.task_room().is_err() {
            continue;
        }
        w.checked();
        w.policy_checked = Some(now);
        #[cfg(test)]
        fire_tick_hook(TickHookPoint::BeforeDue, &name);
        let (reason, settings) = match due_outcome(st, &ds, &settings, w, true) {
            Ok(DueOutcome::Reason(r, live)) => (Some(r), *live),
            Ok(DueOutcome::None(live)) => (None, *live),
            Ok(DueOutcome::NotLive | DueOutcome::NotAuto) => {
                watches.remove(&name);
                continue;
            }
            Err(e) => {
                tracing::warn!(target: "sparkles::graphrag", dataset = %name, "automatic refresh: {e}");
                w.failed(now, debounce);
                continue;
            }
        };
        let reason = match reason {
            Some(r) => Some(r),
            None => {
                #[cfg(test)]
                fire_tick_hook(TickHookPoint::BeforePrivateCheck, &name);
                match private_check(st, &ds, &settings, w, now, debounce) {
                    PrivateOutcome::Reason(r) => Some(r),
                    PrivateOutcome::None => None,
                    PrivateOutcome::NotLive => {
                        watches.remove(&name);
                        continue;
                    }
                }
            }
        };
        // the failures in a row end only when no refresh is needed, so the refreshes
        // that private checks start back off as the others do
        let Some(reason) = reason else {
            w.failures = 0;
            continue;
        };
        #[cfg(test)]
        fire_tick_hook(TickHookPoint::BeforeSpawn, &name);
        match build::spawn(
            st,
            ds.clone(),
            settings,
            Mode::Refresh,
            true,
            Some(reason.clone()),
        ) {
            Ok(t) => {
                tracing::info!(target: "sparkles::graphrag", dataset = %name, "automatic refresh: {reason}");
                w.task = Some(t.id);
            }
            Err(_) if !is_live_dataset(st, &ds) => {
                watches.remove(&name);
                detach_if_not_live(st, &ds);
            }
            // a build or refresh started meanwhile: check again after it
            Err(_) => w.retry_at = Some(now + debounce),
        }
    }
}

/// The reason a refresh starts after a private check finds a changed input. It names no
/// record and no trigger, since the change may be one the principal cannot see.
pub const CHECK_REASON: &str = "a check under the review policy found reports whose input changed";

enum PrivateOutcome {
    Reason(String),
    None,
    NotLive,
}

/// Run the private check when one is due and its backoff has passed. `Reason` when it
/// found a change of a report's input; nothing else the check does is visible.
fn private_check(
    st: &Arc<AppState>,
    ds: &Arc<Dataset>,
    settings: &GraphragSettings,
    w: &mut Watch,
    now: Instant,
    debounce: Duration,
) -> PrivateOutcome {
    if !is_live_dataset(st, ds) {
        detach_if_not_live(st, ds);
        return PrivateOutcome::NotLive;
    }
    if settings.review_policy.is_none() || !w.private.ready(now) {
        return PrivateOutcome::None;
    }
    w.private.runs += 1;
    let head = ds.store.head_commit().seq;
    let r = build::Run {
        st,
        ds: ds.clone(),
        settings: settings.clone(),
        mode: Mode::Refresh,
        confirmed: true,
        task: None,
        task_id: String::new(),
        cancel: Default::default(),
        limits: Default::default(),
    };
    match r.private_check() {
        Ok(None) => {
            if !is_live_dataset(st, ds) {
                detach_if_not_live(st, ds);
                return PrivateOutcome::NotLive;
            }
            w.private.done();
            w.private.covered = Some(head);
            PrivateOutcome::None
        }
        Ok(Some(why)) => {
            if !is_live_dataset(st, ds) {
                detach_if_not_live(st, ds);
                return PrivateOutcome::NotLive;
            }
            // the refresh it starts clears the check once it ends well
            tracing::debug!(target: "sparkles::graphrag", dataset = %ds.name, "automatic refresh check: {why}");
            PrivateOutcome::Reason(CHECK_REASON.into())
        }
        Err(e) => {
            if !is_live_dataset(st, ds) {
                detach_if_not_live(st, ds);
                return PrivateOutcome::NotLive;
            }
            tracing::warn!(target: "sparkles::graphrag", dataset = %ds.name, "automatic refresh check: {e}");
            w.private.failed(now, debounce);
            PrivateOutcome::None
        }
    }
}

/// Note what makes a private check due under the review policy `e`: the dataset seen
/// for the first time, as when the server starts, another evidence configuration or
/// access policy generation, and a commit since the last look that changed the
/// policy's decisions or supports graph or added a withdrawal to a proposal graph, read
/// from the feed without the principal's view.
fn note_private(st: &AppState, ds: &Dataset, e: &super::evidence::Evidence, w: &mut Watch) {
    let p = &mut w.private;
    if p.configuration.as_deref() != Some(e.policy.configuration.as_str()) {
        p.configuration = Some(e.policy.configuration.clone());
        p.due = true;
    }
    let access = st.auth.as_ref().map_or(0, |a| a.epoch());
    if p.access != Some(access) {
        p.access = Some(access);
        p.due = true;
    }
    let head = ds.store.head_commit().seq;
    let Some(after) = p.evidence_seen else {
        p.evidence_seen = Some(head);
        p.due = true;
        return;
    };
    if after >= head {
        return;
    }
    p.evidence_seen = Some(head);
    if !p.due && evidence_changed(ds, e, after, head) {
        p.due = true;
    }
}

/// Whether a commit in `after..=head` changed the decisions or supports graph of `e`'s
/// policy or added a withdrawal to a proposal graph, or may have when the feed cannot
/// list it.
fn evidence_changed(ds: &Dataset, e: &super::evidence::Evidence, after: u64, head: u64) -> bool {
    use sparkles::access::{GraphAccess, GraphRule, Graphs};
    let mut names: Vec<&str> = vec![&e.policy.decisions, &e.policy.supports];
    names.extend(e.proposals.iter().map(String::as_str));
    let o = ChangesOptions {
        max_commits: PAGE_COMMITS,
        max_quads: PAGE_QUADS,
        cancel: None,
        deadline: Some(Instant::now() + FEED_DEADLINE),
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new(names, &[])),
            Graphs::none(),
        ))),
    };
    let mut after = after;
    while after < head {
        let Ok(page) = ds.store.changes(after, &o) else {
            return true;
        };
        for c in &page.commits {
            if c.commit.seq > head {
                return false;
            }
            if !c.complete() {
                return true;
            }
            for (_, q) in c.iter() {
                let GraphName::NamedNode(g) = &q.graph_name else {
                    continue;
                };
                let g = g.as_str();
                if g == e.policy.decisions || g == e.policy.supports {
                    return true;
                }
                if q.predicate.as_str() == super::store::RDF_TYPE
                    && matches!(&q.object, Term::NamedNode(o) if o.as_str() == WITHDRAWAL)
                {
                    return true;
                }
            }
        }
        let next = page.next();
        if page.commits.is_empty() || next >= head.min(page.head.seq) {
            break;
        }
        after = next;
    }
    false
}

/// The memory cap of the refresh accumulator circuit on `st`.
fn circuit_memory_bytes(st: &AppState) -> u64 {
    st.views_options
        .views_memory
        .map_or(CIRCUIT_MEMORY_BYTES, |m| m.min(CIRCUIT_MEMORY_BYTES))
}

/// Escape `s` as a double-quoted SPARQL string literal.
fn sparql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Build the canonical SPARQL query and key columns for the internal F13 accumulator
/// circuit of `(settings, extra)` (spec C22 §5, F13 §3.6).
///
/// The accumulator keeps the changed fact's identity (`?g`, `?p`, `?o`) through to the
/// sink so churn across commits and replacements within one commit never cancel, while
/// keying by `(?s, ?peer)` (and `?nonfact` when `rdf:type` is not excluded) so literal
/// changes fold onto one key per subject and IRI-to-IRI edges still identify entities
/// joining a stored community. Returns `None` when no graph can match.
pub(crate) fn build_accumulator_query(
    settings: &GraphragSettings,
    extra: &super::projection::Extra<'_>,
) -> Option<(String, Vec<&'static str>)> {
    let mut exact_graphs: BTreeSet<String> = BTreeSet::new();
    let mut prefix_graphs: BTreeSet<String> = BTreeSet::new();
    for g in &settings.projection.graphs {
        if let Some(stem) = g.strip_suffix('*') {
            prefix_graphs.insert(stem.to_string());
        } else if extra.reads(settings, g) {
            exact_graphs.insert(g.clone());
        }
    }
    if let Some(out) = extra.output
        && extra.reads(settings, out)
    {
        exact_graphs.insert(out.to_string());
    }
    if exact_graphs.is_empty() && prefix_graphs.is_empty() {
        return None;
    }

    let excluded_preds: BTreeSet<String> = settings
        .projection
        .exclude
        .iter()
        .filter_map(|p| oxrdf::NamedNode::new(p.as_str()).ok())
        .map(|n| n.to_string())
        .collect();
    let rdf_type_excluded = excluded_preds.contains(RDF_TYPE);

    let mut pred_filters: Vec<String> = Vec::new();
    if !excluded_preds.is_empty() {
        let list: Vec<&str> = excluded_preds.iter().map(String::as_str).collect();
        pred_filters.push(format!("?p NOT IN ({})", list.join(", ")));
    }
    if rdf_type_excluded {
        pred_filters.push("isIRI(?s) && (isIRI(?o) || isLiteral(?o))".into());
    } else {
        pred_filters.push(format!(
            "isIRI(?s) && ((?p != {RDF_TYPE} && (isIRI(?o) || isLiteral(?o))) || (isIRI(?o) && ?s != ?o))"
        ));
    }

    let peer_bind = "BIND(IF(isIRI(?o) && ?s != ?o, ?o, ?_unset) AS ?peer)";
    let (key_select, keys, nonfact_bind) = if rdf_type_excluded {
        ("?s ?peer", vec!["s", "peer"], String::new())
    } else {
        (
            "?s ?peer ?nonfact",
            vec!["s", "peer", "nonfact"],
            format!(" BIND(IF(?p = {RDF_TYPE}, false, ?_unset) AS ?nonfact)"),
        )
    };

    // Route `urn:x-arq:*` IRIs through the `GRAPH ?g` filter path: F13 compiles constant
    // `GRAPH <urn:x-arq:...>` clauses to `Recompute` (scanning and retaining rows), and
    // the SPARQL planner interprets `UnionGraph`/`DefaultGraph` as aliases rather than
    // physical named graphs.
    let exact_iris: Option<Vec<String>> = if prefix_graphs.is_empty()
        && exact_graphs.len() <= MAX_EXACT_GRAPHS_UNION
        && !exact_graphs.iter().any(|g| g.starts_with("urn:x-arq:"))
    {
        exact_graphs
            .iter()
            .map(|g| {
                oxrdf::NamedNode::new(g.as_str())
                    .ok()
                    .map(|n| n.to_string())
            })
            .collect()
    } else {
        None
    };

    let query = match exact_iris {
        Some(iris) if iris.len() == 1 => {
            let g = &iris[0];
            format!(
                "SELECT {key_select} ?p ?o WHERE {{ GRAPH {g} {{ ?s ?p ?o }} FILTER({}) {peer_bind}{nonfact_bind} }}",
                pred_filters.join(" && ")
            )
        }
        Some(iris) => {
            let branches: Vec<String> = iris
                .iter()
                .map(|g| format!("{{ GRAPH {g} {{ ?s ?p ?o }} BIND({g} AS ?g) }}"))
                .collect();
            format!(
                "SELECT {key_select} ?g ?p ?o WHERE {{ {} FILTER({}) {peer_bind}{nonfact_bind} }}",
                branches.join(" UNION "),
                pred_filters.join(" && ")
            )
        }
        None => {
            let mut graph_terms: Vec<String> = Vec::new();
            for g in &exact_graphs {
                graph_terms.push(format!("STR(?g) = {}", sparql_string(g)));
            }
            for stem in &prefix_graphs {
                graph_terms.push(format!("STRSTARTS(STR(?g), {})", sparql_string(stem)));
            }
            let mut all_filters = Vec::with_capacity(pred_filters.len() + 3);
            all_filters.push(format!(
                "!STRSTARTS(STR(?g), {})",
                sparql_string(super::settings::GUARDED_PREFIX)
            ));
            all_filters.push(format!("({})", graph_terms.join(" || ")));
            if let Some(excl) = extra.excluded
                && !excl.is_empty()
            {
                let list: Vec<String> = excl.iter().map(|g| sparql_string(g)).collect();
                all_filters.push(format!("STR(?g) NOT IN ({})", list.join(", ")));
            }
            all_filters.extend(pred_filters);
            format!(
                "SELECT {key_select} ?g ?p ?o WHERE {{ GRAPH ?g {{ ?s ?p ?o }} FILTER({}) {peer_bind}{nonfact_bind} }}",
                all_filters.join(" && ")
            )
        }
    };
    Some((query, keys))
}

enum CircuitPlan {
    Empty,
    Uncountable(&'static str),
    Attach {
        spec: InternalSpec,
        access: Option<Arc<sparkles::access::GraphAccess>>,
        access_fp: u64,
    },
}

enum EnsureCircuit {
    Attached(InternalCircuit),
    NotLive,
    Failed(String),
}

/// Whether `ds` is still the live, non-restoring dataset registered under `ds.name`.
pub(crate) fn is_live_dataset(st: &AppState, ds: &Dataset) -> bool {
    if st.catalog.restoring_by(&ds.name).is_some() {
        return false;
    }
    st.get(&ds.name).is_some_and(|cur| {
        cur.store.dataset_id() == ds.store.dataset_id()
            && Arc::ptr_eq(
                &cur.dataset.inner_state().views,
                &ds.dataset.inner_state().views,
            )
    })
}

/// Whether `(dataset_id, runtime)` is still the live, non-restoring dataset registered
/// under `name`.
fn is_live_owner(
    st: &AppState,
    name: &str,
    dataset_id: uuid::Uuid,
    runtime: &Weak<sparkles::incr::Runtime>,
) -> bool {
    if st.catalog.restoring_by(name).is_some() {
        return false;
    }
    runtime.strong_count() > 0
        && st.get(name).is_some_and(|cur| {
            cur.store.dataset_id() == dataset_id
                && Weak::ptr_eq(runtime, &Arc::downgrade(&cur.dataset.inner_state().views))
        })
}

/// Detach `ds`'s circuit under `circuit_lock` only if `ds` is still non-live.
fn detach_if_not_live(st: &AppState, ds: &Dataset) {
    let lock = st.graphrag.circuit_lock(&ds.name);
    let _held = lock.lock();
    if !is_live_dataset(st, ds) {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
    }
}

/// Detach `ds`'s circuit under `circuit_lock` only if `ds` is non-live or live settings
/// are still disabled / non-auto.
fn detach_if_not_auto(st: &AppState, ds: &Dataset) {
    let lock = st.graphrag.circuit_lock(&ds.name);
    let _held = lock.lock();
    if !is_live_dataset(st, ds) {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
        return;
    }
    let live: GraphragSettings = crate::settings::effective(st, ds, &crate::settings::GRAPHRAG);
    if !live.enabled || live.refresh.mode != RefreshMode::Auto {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
    }
}

fn is_internal_conflict(e: &sparkles::Error) -> bool {
    matches!(e, sparkles::Error::Component(c) if c.code == "internal-conflict")
}

fn resolve_evidence(
    st: &AppState,
    ds: &Dataset,
    settings: &GraphragSettings,
) -> Result<Option<super::evidence::Evidence>, String> {
    let Some(name) = &settings.review_policy else {
        return Ok(None);
    };
    crate::settings::evidence::require_reconciled(st, ds)
        .map_err(|e| super::evidence::engine(e).to_string())?;
    super::evidence::Evidence::admit(
        ds,
        Some(name),
        None,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        Default::default(),
        &ds.dataset.query_options(),
    )
    .map_err(|e| e.to_string())
}

/// Plan the internal F13 accumulator circuit for `ds` under `settings` and `evidence`,
/// detaching any existing handle for `ds` when authorization fails or the projection is
/// empty or uncountable. Caller must hold `st.graphrag.circuit_lock(&ds.name)`.
fn plan_circuit_locked(
    st: &AppState,
    ds: &Dataset,
    settings: &GraphragSettings,
    evidence: Option<&super::evidence::Evidence>,
) -> Result<CircuitPlan, String> {
    let principal = match build::authorized_principal(st, ds, &settings.principal) {
        Ok(p) => p,
        Err(e) => {
            st.graphrag.detach_circuit_for_locked(&ds.name, ds);
            return Err(e);
        }
    };
    let access = principal.view(&ds.name, Endpoint::Query);
    if let Some(reason) = sparkles::incr::runtime::whole_reason(access.as_ref()) {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
        return Ok(CircuitPlan::Uncountable(reason));
    }
    let extra = super::projection::Extra {
        output: evidence.map(|e| e.policy.output.as_str()),
        excluded: evidence.map(|e| &e.excluded),
    };
    let Some((query, keys)) = build_accumulator_query(settings, &extra) else {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
        return Ok(CircuitPlan::Empty);
    };
    let spec =
        InternalSpec::accumulator(CIRCUIT_ID, query, keys).with_memory(circuit_memory_bytes(st));
    let access_fp = sparkles::incr::runtime::access_fingerprint(access.as_ref());
    Ok(CircuitPlan::Attach {
        spec,
        access,
        access_fp,
    })
}

/// Return the attached refresh circuit matching `(spec, access_fp)`, replacing any
/// stale handle for `ds.name` after waiting for the worker to collect it. Caller must
/// hold `st.graphrag.circuit_lock(&ds.name)`.
fn ensure_circuit_locked(
    st: &AppState,
    ds: &Dataset,
    settings: &GraphragSettings,
    access_epoch: u64,
    spec: InternalSpec,
    access: Option<Arc<sparkles::access::GraphAccess>>,
    access_fp: u64,
) -> EnsureCircuit {
    if !is_live_dataset(st, ds) {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
        return EnsureCircuit::NotLive;
    }
    if let Some(handle) = st.graphrag.refresh_circuit_sync_locked(
        &ds.name,
        ds,
        settings,
        access_epoch,
        &spec,
        access_fp,
    ) {
        return EnsureCircuit::Attached(handle);
    }
    let views = &ds.dataset.inner_state().views;
    let deadline = Instant::now() + CIRCUIT_SETTLE_TIMEOUT;
    if let Some(old) = st.graphrag.take_refresh_circuit_locked(&ds.name) {
        let same_store = old.same_store(ds);
        drop(old);
        if same_store && !views.is_closed() {
            views.settle(deadline.saturating_duration_since(Instant::now()));
        }
    }
    let handle = loop {
        if !is_live_dataset(st, ds) {
            return EnsureCircuit::NotLive;
        }
        let internal_access = match &access {
            Some(g) => InternalAccess::reading(g.clone()),
            None => InternalAccess::everything(),
        };
        match ds.dataset.attach_internal(spec.clone(), internal_access) {
            Ok(h) => break h,
            Err(e) if is_internal_conflict(&e) && !views.is_closed() => {
                // A single `settle()` can finish an in-flight round that had already
                // passed `collect_internal`, or follow an earlier unsettled detach; retry
                // across subsequent rounds until collection completes or the budget ends.
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return EnsureCircuit::Failed(e.to_string());
                }
                views.settle(remaining);
            }
            Err(e) => return EnsureCircuit::Failed(e.to_string()),
        }
    };
    if !is_live_dataset(st, ds) {
        drop(handle);
        return EnsureCircuit::NotLive;
    }
    let displaced = st.graphrag.set_refresh_circuit_locked(
        &ds.name,
        RefreshCircuit::new(
            ds,
            settings.clone(),
            access_epoch,
            spec,
            access_fp,
            handle.clone(),
        ),
    );
    drop(displaced);
    EnsureCircuit::Attached(handle)
}

/// Sync the refresh circuit right after a build or refresh saves its state on `main`,
/// discarding counts that the completed run already covers. Always reads live settings,
/// access epoch and review policy under the per-dataset circuit lock so a finishing
/// build never clobbers a newer projection or re-attaches during a restore or delete.
pub(crate) fn after_build(st: &AppState, ds: &Dataset, source_commit: u64) {
    if ds.branch.is_some() {
        return;
    }
    let lock = st.graphrag.circuit_lock(&ds.name);
    let _held = lock.lock();
    if !is_live_dataset(st, ds) {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
        return;
    }
    let settings: GraphragSettings = crate::settings::effective(st, ds, &crate::settings::GRAPHRAG);
    let epoch = st.access_epoch();
    if !settings.enabled || settings.refresh.mode != RefreshMode::Auto {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
        return;
    }
    let evidence = match resolve_evidence(st, ds, &settings) {
        Ok(e) => e,
        Err(e) => {
            st.graphrag.detach_circuit_for_locked(&ds.name, ds);
            tracing::warn!(
                target: "sparkles::graphrag",
                dataset = %ds.name,
                "automatic refresh circuit: {e}"
            );
            return;
        }
    };
    match plan_circuit_locked(st, ds, &settings, evidence.as_ref()) {
        Ok(CircuitPlan::Attach {
            spec,
            access,
            access_fp,
        }) => match ensure_circuit_locked(st, ds, &settings, epoch, spec, access, access_fp) {
            EnsureCircuit::Attached(handle) => {
                let head = ds.store.head_commit().seq;
                let through = advance_past_graphrag(&ds.store, source_commit, head);
                handle.discard_through(through);
            }
            EnsureCircuit::NotLive => {}
            EnsureCircuit::Failed(e) => {
                tracing::warn!(
                    target: "sparkles::graphrag",
                    dataset = %ds.name,
                    "automatic refresh circuit: {e}"
                );
            }
        },
        Ok(CircuitPlan::Empty | CircuitPlan::Uncountable(_)) => {}
        Err(e) => {
            tracing::warn!(
                target: "sparkles::graphrag",
                dataset = %ds.name,
                "automatic refresh circuit: {e}"
            );
        }
    }
}

enum AttachState {
    Active(Box<GraphragSettings>, u64),
    Inactive,
    NotLive,
}

enum DueOutcome {
    Reason(String, Box<GraphragSettings>),
    None(Box<GraphragSettings>),
    NotAuto,
    NotLive,
}

/// Keep the internal accumulator attached ahead of the debounce window whenever `ds` has
/// a `main` build, without reading `state.json` on idle ticks.
fn ensure_attached(
    st: &AppState,
    ds: &Dataset,
    settings: &GraphragSettings,
    head: u64,
    w: &mut Watch,
) -> AttachState {
    let epoch = st.access_epoch();
    let settings_same = w.attached_settings.as_ref() == Some(settings);
    let epoch_same = w.attached_epoch == Some(epoch);
    let evidence_moved = settings.review_policy.is_some() && w.attached_head != Some(head);
    let has_matching = st
        .graphrag
        .has_matching_circuit(&ds.name, ds, settings, epoch);
    if settings_same
        && epoch_same
        && !evidence_moved
        && (has_matching
            || (w.attached_uncountable && !st.graphrag.has_refresh_circuit(&ds.name, ds)))
    {
        return AttachState::Active(Box::new(settings.clone()), head);
    }
    if !st.graphrag.has_refresh_circuit(&ds.name, ds)
        && w.no_build_head == Some(head)
        && settings_same
        && epoch_same
    {
        return AttachState::Active(Box::new(settings.clone()), head);
    }
    #[cfg(test)]
    fire_tick_hook(TickHookPoint::BeforeEnsureLock, &ds.name);
    let lock = st.graphrag.circuit_lock(&ds.name);
    let _held = lock.lock();
    if !is_live_dataset(st, ds) {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
        w.clear_attached();
        return AttachState::NotLive;
    }
    // Re-read live settings, access epoch, head and build state under the circuit lock so
    // an older tick snapshot never clobbers or detaches a circuit published by a newer
    // build or settings change.
    let live_settings: GraphragSettings =
        crate::settings::effective(st, ds, &crate::settings::GRAPHRAG);
    if !live_settings.enabled || live_settings.refresh.mode != RefreshMode::Auto {
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
        w.clear_attached();
        return AttachState::Inactive;
    }
    let epoch = st.access_epoch();
    let head = ds.store.head_commit().seq;
    let Ok(state) = build::state_of(st, ds) else {
        return AttachState::Active(Box::new(live_settings), head);
    };
    let Some(b) = state
        .last_build
        .as_ref()
        .filter(|b| b.branch == sparkles::branch::MAIN)
    else {
        w.no_build_head = Some(head);
        w.attached_settings = Some(live_settings.clone());
        w.attached_epoch = Some(epoch);
        w.attached_head = Some(head);
        w.attached_uncountable = false;
        st.graphrag.detach_circuit_for_locked(&ds.name, ds);
        return AttachState::Active(Box::new(live_settings), head);
    };
    w.no_build_head = None;
    let evidence = match resolve_evidence(st, ds, &live_settings) {
        Ok(e) => e,
        Err(_) => {
            st.graphrag.detach_circuit_for_locked(&ds.name, ds);
            w.clear_attached();
            return AttachState::Active(Box::new(live_settings), head);
        }
    };
    match plan_circuit_locked(st, ds, &live_settings, evidence.as_ref()) {
        Ok(CircuitPlan::Attach {
            spec,
            access,
            access_fp,
        }) => match ensure_circuit_locked(st, ds, &live_settings, epoch, spec, access, access_fp) {
            EnsureCircuit::Attached(handle) => {
                let through = advance_past_graphrag(&ds.store, b.source_commit, head);
                handle.discard_through(through);
                w.attached_settings = Some(live_settings.clone());
                w.attached_epoch = Some(epoch);
                w.attached_head = Some(head);
                w.attached_uncountable = false;
                AttachState::Active(Box::new(live_settings), head)
            }
            EnsureCircuit::NotLive => {
                w.clear_attached();
                AttachState::NotLive
            }
            EnsureCircuit::Failed(_) => {
                w.clear_attached();
                AttachState::Active(Box::new(live_settings), head)
            }
        },
        Ok(CircuitPlan::Empty | CircuitPlan::Uncountable(_)) | Err(_) => {
            w.attached_settings = Some(live_settings.clone());
            w.attached_epoch = Some(epoch);
            w.attached_head = Some(head);
            w.attached_uncountable = true;
            AttachState::Active(Box::new(live_settings), head)
        }
    }
}

/// Why the dataset needs a refresh now, if it does.
pub(crate) fn due_reason(
    st: &AppState,
    ds: &Dataset,
    settings: &GraphragSettings,
    w: &mut Watch,
) -> Result<Option<String>, String> {
    match due_outcome(st, ds, settings, w, false)? {
        DueOutcome::Reason(r, _) => Ok(Some(r)),
        DueOutcome::None(_) | DueOutcome::NotAuto | DueOutcome::NotLive => Ok(None),
    }
}

fn due_outcome(
    st: &AppState,
    ds: &Dataset,
    _settings: &GraphragSettings,
    w: &mut Watch,
    require_auto: bool,
) -> Result<DueOutcome, String> {
    struct WaitStep {
        live_settings: GraphragSettings,
        state: BuildState,
        evidence: Option<super::evidence::Evidence>,
        head: u64,
        effective_after: u64,
        handle: InternalCircuit,
    }

    enum LockStep {
        Early {
            outcome: DueOutcome,
            evidence: Option<Box<super::evidence::Evidence>>,
        },
        Fallback {
            live_settings: Box<GraphragSettings>,
            evidence: Option<Box<super::evidence::Evidence>>,
            head: u64,
            msg: String,
        },
        Wait(Box<WaitStep>),
    }

    let step = {
        let lock = st.graphrag.circuit_lock(&ds.name);
        let _held = lock.lock();
        if !is_live_dataset(st, ds) {
            st.graphrag.detach_circuit_for_locked(&ds.name, ds);
            w.clear_attached();
            return Ok(DueOutcome::NotLive);
        }
        let live_settings: GraphragSettings =
            crate::settings::effective(st, ds, &crate::settings::GRAPHRAG);
        if !live_settings.enabled
            || (require_auto && live_settings.refresh.mode != RefreshMode::Auto)
        {
            st.graphrag.detach_circuit_for_locked(&ds.name, ds);
            w.clear_attached();
            return Ok(DueOutcome::NotAuto);
        }
        let state = build::state_of(st, ds)?;
        let Some(b) = &state.last_build else {
            return Ok(DueOutcome::None(Box::new(live_settings)));
        };
        if b.branch != sparkles::branch::MAIN {
            return Ok(DueOutcome::None(Box::new(live_settings)));
        }
        w.has_pending = !b.pending.is_empty();
        w.had_policy = b.policy.is_some();
        // the policy the reports read under: enabling, switching or removing it, or a new
        // version, rewrites every report (spec C25, "Reports on accepted evidence")
        let evidence = match resolve_evidence(st, ds, &live_settings) {
            Ok(e) => e,
            Err(e) => {
                st.graphrag.detach_circuit_for_locked(&ds.name, ds);
                w.clear_attached();
                return Err(e);
            }
        };
        let policy_reason = match (&evidence, &b.policy) {
            (None, None) => None,
            (None, Some(_)) => Some("the reports no longer read under a review policy".into()),
            (Some(e), stored) => (!stored.as_ref().is_some_and(|p| {
                p.name == e.policy.name && p.iri == e.policy.iri && p.version == e.policy.version
            }))
            .then(|| {
                format!(
                    "the reports read under review policy {} at version {}",
                    e.policy.name, e.policy.version
                )
            }),
        };
        if let Some(reason) = policy_reason {
            LockStep::Early {
                outcome: DueOutcome::Reason(reason, Box::new(live_settings)),
                evidence: evidence.map(Box::new),
            }
        } else if !b.pending.is_empty() && w.retried_day != Some(build::today()) {
            w.retried_day = Some(build::today());
            LockStep::Early {
                outcome: DueOutcome::Reason(
                    format!(
                        "{} communities are pending from an earlier run",
                        b.pending.len()
                    ),
                    Box::new(live_settings),
                ),
                evidence: evidence.map(Box::new),
            }
        } else if !source_changed(&ds.store, b.source_commit) {
            // only GraphRAG's own commits since then: nothing the reports read changed,
            // which also keeps a refresh from following its own commit when counts are
            // unavailable
            LockStep::Early {
                outcome: DueOutcome::None(Box::new(live_settings)),
                evidence: evidence.map(Box::new),
            }
        } else {
            let head = ds.store.head_commit().seq;
            let effective_after = advance_past_graphrag(&ds.store, b.source_commit, head);
            let epoch = st.access_epoch();
            match plan_circuit_locked(st, ds, &live_settings, evidence.as_ref())? {
                CircuitPlan::Empty => LockStep::Early {
                    outcome: DueOutcome::None(Box::new(live_settings)),
                    evidence: evidence.map(Box::new),
                },
                CircuitPlan::Uncountable(why) => LockStep::Fallback {
                    live_settings: Box::new(live_settings),
                    evidence: evidence.map(Box::new),
                    head,
                    msg: format!("the incremental refresh circuit cannot count the changes: {why}"),
                },
                CircuitPlan::Attach {
                    spec,
                    access,
                    access_fp,
                } => {
                    if live_settings.refresh.mode == RefreshMode::Auto {
                        match ensure_circuit_locked(
                            st,
                            ds,
                            &live_settings,
                            epoch,
                            spec,
                            access,
                            access_fp,
                        ) {
                            EnsureCircuit::Attached(handle) => {
                                w.no_build_head = None;
                                w.attached_settings = Some(live_settings.clone());
                                w.attached_epoch = Some(epoch);
                                w.attached_head = Some(head);
                                w.attached_uncountable = false;
                                LockStep::Wait(Box::new(WaitStep {
                                    live_settings,
                                    state,
                                    evidence,
                                    head,
                                    effective_after,
                                    handle,
                                }))
                            }
                            EnsureCircuit::NotLive => {
                                w.clear_attached();
                                return Ok(DueOutcome::NotLive);
                            }
                            EnsureCircuit::Failed(e) => {
                                w.clear_attached();
                                LockStep::Fallback {
                                    live_settings: Box::new(live_settings),
                                    evidence: evidence.map(Box::new),
                                    head,
                                    msg: format!(
                                        "the incremental refresh circuit could not attach: {e}"
                                    ),
                                }
                            }
                        }
                    } else {
                        match st.graphrag.refresh_circuit(&ds.name, ds, &spec, access_fp) {
                            Some(handle) => LockStep::Wait(Box::new(WaitStep {
                                live_settings,
                                state,
                                evidence,
                                head,
                                effective_after,
                                handle,
                            })),
                            None => LockStep::Fallback {
                                live_settings: Box::new(live_settings),
                                evidence: evidence.map(Box::new),
                                head,
                                msg:
                                    "the incremental refresh circuit is not attached in manual mode"
                                        .into(),
                            },
                        }
                    }
                }
            }
        }
    };

    let fallback = |w: &mut Watch,
                    has_evidence: bool,
                    head: u64,
                    live_settings: Box<GraphragSettings>,
                    msg: String|
     -> Result<DueOutcome, String> {
        if has_evidence {
            w.private.uncounted(head);
            Ok(DueOutcome::None(live_settings))
        } else {
            Ok(DueOutcome::Reason(msg, live_settings))
        }
    };

    match step {
        LockStep::Early { outcome, evidence } => {
            if !is_live_dataset(st, ds) {
                detach_if_not_live(st, ds);
                return Ok(DueOutcome::NotLive);
            }
            if let Some(e) = &evidence {
                note_private(st, ds, e, w);
            }
            Ok(outcome)
        }
        LockStep::Fallback {
            live_settings,
            evidence,
            head,
            msg,
        } => {
            if !is_live_dataset(st, ds) {
                detach_if_not_live(st, ds);
                return Ok(DueOutcome::NotLive);
            }
            if let Some(e) = &evidence {
                note_private(st, ds, e, w);
            }
            fallback(w, evidence.is_some(), head, live_settings, msg)
        }
        LockStep::Wait(wait) => {
            let WaitStep {
                live_settings,
                state,
                evidence,
                head,
                effective_after,
                handle,
            } = *wait;
            if !is_live_dataset(st, ds) {
                detach_if_not_live(st, ds);
                return Ok(DueOutcome::NotLive);
            }
            if let Some(e) = &evidence {
                note_private(st, ds, e, w);
            }
            let caught_up = handle.wait(head, CIRCUIT_WAIT_TIMEOUT);
            if !is_live_dataset(st, ds) {
                detach_if_not_live(st, ds);
                return Ok(DueOutcome::NotLive);
            }
            match handle.counts(effective_after) {
                Ok(counts) if caught_up && counts.through >= head => {
                    if let Some(why) = counts.unknown {
                        fallback(
                            w,
                            evidence.is_some(),
                            head,
                            Box::new(live_settings),
                            format!(
                                "the incremental refresh circuit cannot count every commit: {why}"
                            ),
                        )
                    } else {
                        let mut counter = Counter::new(&state, &live_settings);
                        if let Some(joined) = counter.apply_counts(&counts) {
                            return Ok(DueOutcome::Reason(joined, Box::new(live_settings)));
                        }
                        match counter.verdict(live_settings.refresh.min_changed_fraction) {
                            Some(reason) => Ok(DueOutcome::Reason(reason, Box::new(live_settings))),
                            None => Ok(DueOutcome::None(Box::new(live_settings))),
                        }
                    }
                }
                Ok(_) => Err(format!(
                    "the incremental refresh circuit did not reach commit {head} in time (at {})",
                    handle.commit()
                )),
                Err(e) => {
                    handle.rebuild();
                    fallback(
                        w,
                        evidence.is_some(),
                        head,
                        Box::new(live_settings),
                        format!("the incremental refresh circuit failed: {e}"),
                    )
                }
            }
        }
    }
}

/// Advance `after` past any contiguous GraphRAG-only commits starting at `after + 1`.
/// GraphRAG commits write only to guarded graphs, which the projection never reads, so
/// skipping them aligns an accumulator attached after a build with its baseline without
/// skipping any source commit.
fn advance_past_graphrag(store: &sparkles::store::Store, after: u64, head: u64) -> u64 {
    let mut cur = after;
    let limit = head.min(after.saturating_add(MAX_KIND_CHECKS));
    while cur < limit {
        match store.commit(cur + 1) {
            Some(c) if c.kind == CommitKind::Graphrag => cur += 1,
            _ => break,
        }
    }
    cur
}

/// Whether a commit after `after` other than GraphRAG's own may have changed data. A
/// commit whose metadata is gone counts as a change, and so does a long run of commits.
fn source_changed(store: &sparkles::store::Store, after: u64) -> bool {
    let head = store.head_commit().seq;
    if head.saturating_sub(after) > MAX_KIND_CHECKS {
        return true;
    }
    ((after + 1)..=head).any(|seq| {
        store
            .commit(seq)
            .is_none_or(|c| c.kind != CommitKind::Graphrag)
    })
}

/// The most commits whose kinds [`source_changed`] reads one by one.
const MAX_KIND_CHECKS: u64 = 10_000;

/// The changed facts of each stored community.
struct Counter<'a> {
    /// the communities whose reports have fact digests, and their sizes
    communities: Vec<(&'a str, usize)>,
    /// each member's communities that have digests
    of: HashMap<&'a str, Vec<usize>>,
    /// every member of a stored community
    known: HashSet<&'a str>,
    #[cfg(test)]
    excluded: HashSet<String>,
    changed: Vec<usize>,
}

impl<'a> Counter<'a> {
    fn new(state: &'a BuildState, settings: &GraphragSettings) -> Counter<'a> {
        #[cfg(not(test))]
        let _ = settings;
        let mut c = Counter {
            communities: Vec::new(),
            of: HashMap::new(),
            known: HashSet::new(),
            #[cfg(test)]
            excluded: settings
                .projection
                .exclude
                .iter()
                .filter_map(|p| oxrdf::NamedNode::new(p.as_str()).ok())
                .map(|n| n.to_string())
                .collect(),
            changed: Vec::new(),
        };
        for s in state.last_build.iter().flat_map(|b| b.communities.iter()) {
            for m in &s.members {
                c.known.insert(m.as_str());
            }
            if s.facts.is_empty() {
                continue;
            }
            let i = c.communities.len();
            c.communities.push((s.iri.as_str(), s.facts.len()));
            for m in &s.members {
                c.of.entry(m.as_str()).or_default().push(i);
            }
        }
        c.changed = vec![0; c.communities.len()];
        c
    }

    /// Total the accumulator's per-key counts onto each stored community. Returns `Some`
    /// when an IRI-to-IRI edge links a stored community member with an entity no stored
    /// community holds, choosing the lexicographically smallest joining entity.
    fn apply_counts(&mut self, counts: &Counts) -> Option<String> {
        let mut joined: Option<String> = None;
        for (key, n) in &counts.keys {
            if *n == 0 {
                continue;
            }
            let Some(Some(s_term)) = key.first() else {
                continue;
            };
            let st = s_term.to_string();
            // Key layout: `[s, peer]` when rdf:type is excluded, or `[s, peer, nonfact]`
            // where `nonfact` is bound (`Some(false)`) only for non-excluded rdf:type.
            let is_fact = key.get(2).is_none_or(Option::is_none);
            if is_fact && let Some(cs) = self.of.get(st.as_str()) {
                let delta = usize::try_from(*n).unwrap_or(usize::MAX);
                for &i in cs {
                    self.changed[i] = self.changed[i].saturating_add(delta);
                }
            }
            if let Some(Some(Term::NamedNode(peer))) = key.get(1)
                && matches!(s_term, Term::NamedNode(_))
            {
                let ot = peer.to_string();
                if st != ot {
                    let (a, b) = (
                        self.known.contains(st.as_str()),
                        self.known.contains(ot.as_str()),
                    );
                    if a != b {
                        let new = if a { ot } else { st };
                        if joined.as_ref().is_none_or(|cur| new < *cur) {
                            joined = Some(new);
                        }
                    }
                }
            }
        }
        joined.map(|new| format!("{new} joined a community"))
    }

    /// Count one added or removed quad (reference oracle used by tests).
    #[cfg(test)]
    fn change(&mut self, s: &oxrdf::NamedOrBlankNode, p: &str, o: &Term) -> Option<String> {
        if self.excluded.contains(p) {
            return None;
        }
        let st = s.to_string();
        // a fact of a member as the build reads it: neither rdf:type nor a blank or
        // triple object
        let fact = p != RDF_TYPE && matches!(o, Term::NamedNode(_) | Term::Literal(_));
        if fact && let Some(cs) = self.of.get(st.as_str()) {
            for &i in cs {
                self.changed[i] += 1;
            }
        }
        if let (oxrdf::NamedOrBlankNode::NamedNode(_), Term::NamedNode(_)) = (s, o) {
            let ot = o.to_string();
            if st != ot {
                let (a, b) = (
                    self.known.contains(st.as_str()),
                    self.known.contains(ot.as_str()),
                );
                if a != b {
                    let new = if a { ot } else { st };
                    return Some(format!("{new} joined a community"));
                }
            }
        }
        None
    }

    /// Whether some community's changed facts reach `fraction` of its facts.
    fn verdict(&self, fraction: f64) -> Option<String> {
        self.communities
            .iter()
            .zip(&self.changed)
            .find(|((_, n), changed)| **changed > 0 && **changed as f64 >= fraction * *n as f64)
            .map(|((iri, n), changed)| format!("{changed} of the {n} facts of <{iri}> changed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphrag::settings::is_guarded;
    use crate::graphrag::state::{BuildRecord, StoredCommunity};
    use oxrdf::NamedOrBlankNode;

    fn state() -> BuildState {
        BuildState {
            last_build: Some(BuildRecord {
                source_commit: 1,
                branch: "main".into(),
                finished_at: String::new(),
                communities: vec![StoredCommunity {
                    depth: 0,
                    iri: "urn:c".into(),
                    parent: None,
                    members: vec!["<http://ex/a>".into(), "<http://ex/b>".into()],
                    report: None,
                    facts: (0..10).collect(),
                    policy: false,
                    pairs: None,
                }],
                pending: Vec::new(),
                tokens: 0,
                reports_graph: String::new(),
                footprint: String::new(),
                policy: None,
                pairs: Vec::new(),
            }),
            ..Default::default()
        }
    }

    fn iri(s: &str) -> oxrdf::NamedNode {
        oxrdf::NamedNode::new(s).unwrap()
    }

    #[test]
    fn a_community_is_due_when_its_changed_facts_reach_the_fraction() {
        let st = state();
        let mut c = Counter::new(&st, &GraphragSettings::default());
        let a = NamedOrBlankNode::NamedNode(iri("http://ex/a"));
        let lit = Term::Literal(oxrdf::Literal::new_simple_literal("x"));
        assert_eq!(c.change(&a, "<http://ex/name>", &lit), None);
        assert_eq!(c.verdict(0.2), None);
        assert_eq!(c.change(&a, "<http://ex/name>", &lit), None);
        assert!(
            c.verdict(0.2)
                .is_some_and(|r| r.starts_with("2 of the 10 facts"))
        );
        // rdf:type, excluded predicates and blank objects are not facts of a report
        let mut c = Counter::new(&st, &GraphragSettings::default());
        let blank = Term::BlankNode(oxrdf::BlankNode::default());
        c.change(&a, RDF_TYPE, &Term::NamedNode(iri("http://ex/T")));
        c.change(&a, "<http://ex/p>", &blank);
        assert_eq!(c.verdict(0.1), None);
    }

    #[test]
    fn an_entity_joining_a_community_is_due() {
        let st = state();
        let mut c = Counter::new(&st, &GraphragSettings::default());
        let a = NamedOrBlankNode::NamedNode(iri("http://ex/a"));
        assert!(
            c.change(
                &a,
                "<http://ex/knows>",
                &Term::NamedNode(iri("http://ex/new"))
            )
            .is_some_and(|r| r.contains("http://ex/new"))
        );
        // an edge between members, or between two unknown entities, joins nobody
        assert_eq!(
            c.change(
                &a,
                "<http://ex/knows>",
                &Term::NamedNode(iri("http://ex/b"))
            ),
            None
        );
        let x = NamedOrBlankNode::NamedNode(iri("http://ex/x"));
        assert_eq!(
            c.change(
                &x,
                "<http://ex/knows>",
                &Term::NamedNode(iri("http://ex/y"))
            ),
            None
        );
    }

    /// The counts of the feed-based counter after commit `after`, per community.
    fn feed_counter(
        s: &sparkles::store::Store,
        st: &BuildState,
        settings: &GraphragSettings,
        after: u64,
    ) -> Vec<usize> {
        let mut counter = Counter::new(st, settings);
        let page = s.changes(after, &ChangesOptions::default()).unwrap();
        for c in &page.commits {
            assert!(c.complete());
            for (_, q) in c.iter() {
                let GraphName::NamedNode(g) = &q.graph_name else {
                    continue;
                };
                if is_guarded(g.as_str()) || !super::super::projection::named(settings, g.as_str())
                {
                    continue;
                }
                counter.change(&q.subject, &q.predicate.to_string(), &q.object);
            }
        }
        counter.changed
    }

    /// An accumulator on an internal circuit (spec F13 §3.6) counts what the feed-based
    /// counter of step 1 counts for every community, through churn across commits, a
    /// replacement within one commit and deletion-only commits, whether the runtime
    /// follows each commit or applies a run of them (ROADMAP item 414).
    #[test]
    fn an_internal_accumulator_counts_as_the_feed_counter_does() {
        use sparkles::incr::{InternalAccess, InternalSpec, OpenHost, Runtime};
        use sparkles::io::{RdfFormat, Source};
        let mut settings = GraphragSettings::default();
        settings.projection.graphs = vec!["urn:g:*".into()];
        let st = state();
        // the facts of each subject in the projection's graphs, as the counter reads
        // them: neither rdf:type nor a blank or triple object
        let query = format!(
            "SELECT ?s ?g ?p ?o WHERE {{ GRAPH ?g {{ ?s ?p ?o }} \
             FILTER(STRSTARTS(STR(?g), \"urn:g:\") && ?p != {RDF_TYPE} \
             && (isIRI(?o) || isLiteral(?o))) }}"
        );
        let workloads: [&[&str]; 3] = [
            // churn: facts added by one commit and removed by a later one
            &[
                "INSERT DATA { GRAPH <urn:g:a> { <http://ex/a> <http://ex/p> 1 } }",
                "DELETE DATA { GRAPH <urn:g:a> { <http://ex/a> <http://ex/p> 1 } }",
                "INSERT DATA { GRAPH <urn:g:a> { <http://ex/b> <http://ex/p> 2 . <http://ex/x> <http://ex/p> 3 } }",
                "DELETE DATA { GRAPH <urn:g:a> { <http://ex/b> <http://ex/p> 2 } }",
            ],
            // replacements within one commit
            &[
                "DELETE { GRAPH <urn:g:a> { <http://ex/a> <http://ex/name> \"A\" } } \
                 INSERT { GRAPH <urn:g:a> { <http://ex/a> <http://ex/name> \"A2\" } } WHERE {}",
                "DELETE { GRAPH <urn:g:a> { <http://ex/b> <http://ex/name> \"B\" } } \
                 INSERT { GRAPH <urn:g:b> { <http://ex/b> <http://ex/name> \"B\" } } WHERE {}",
            ],
            // deletions only, with changes outside the projection and of non-facts
            &[
                "DELETE DATA { GRAPH <urn:g:a> { <http://ex/a> <http://ex/age> 30 } }",
                "DELETE DATA { GRAPH <urn:g:a> { <http://ex/b> <http://ex/age> 40 . <http://ex/a> a <http://ex/T> } }",
                "DELETE DATA { GRAPH <urn:other> { <http://ex/a> <http://ex/age> 1 } }",
            ],
        ];
        for (n, workload) in workloads.iter().enumerate() {
            for per_commit in [true, false] {
                let s = sparkles::store::Store::in_memory(Default::default());
                s.load(&[Source::from_bytes(
                    b"<urn:g:a> { <http://ex/a> <http://ex/name> \"A\" ; <http://ex/age> 30 ; a <http://ex/T> . \
                      <http://ex/b> <http://ex/name> \"B\" ; <http://ex/age> 40 . } \
                      <urn:other> { <http://ex/a> <http://ex/age> 1 . }"
                        .to_vec(),
                    RdfFormat::TriG,
                    None,
                )])
                .unwrap();
                let rt = Runtime::open(None, s.dataset_id(), Default::default());
                rt.set_host(Arc::new(OpenHost));
                let c = rt
                    .attach_internal(
                        &s,
                        InternalSpec::accumulator("graphrag-refresh", query.clone(), ["s"]),
                        InternalAccess::everything(),
                    )
                    .unwrap();
                let after = s.head_commit().seq;
                let opts = sparkles::sparql::QueryOptions::default();
                for u in workload.iter() {
                    sparkles::sparql::update::update(&s, u, &opts).unwrap();
                    if per_commit {
                        rt.round(&s);
                    }
                }
                rt.round(&s);
                let counts = c.counts(after).unwrap();
                assert!(counts.complete(), "{counts:?}");
                // the counts per subject, totalled per community as the counter does
                let counter = Counter::new(&st, &settings);
                let mut got = vec![0usize; counter.communities.len()];
                for (key, n) in &counts.keys {
                    let Some(Term::NamedNode(sub)) = &key[0] else {
                        continue;
                    };
                    for &i in counter
                        .of
                        .get(sub.to_string().as_str())
                        .into_iter()
                        .flatten()
                    {
                        got[i] += *n as usize;
                    }
                }
                let want = feed_counter(&s, &st, &settings, after);
                assert!(want.iter().any(|&n| n > 0), "workload {n} changes facts");
                assert_eq!(got, want, "workload {n}, per commit: {per_commit}");
            }
        }
    }

    #[test]
    fn graphrag_commits_alone_change_no_source() {
        use sparkles::io::{RdfFormat, Source};
        let s = sparkles::store::Store::in_memory(Default::default());
        let load = |t: &str| {
            s.load(&[Source::from_bytes(
                t.as_bytes().to_vec(),
                RdfFormat::TriG,
                None,
            )])
            .unwrap();
        };
        load("<urn:g:a> { <http://ex/a> <http://ex/p> <http://ex/b> . }");
        let after = s.head_commit().seq;
        assert!(!source_changed(&s, after));
        let opts = sparkles::sparql::QueryOptions::default();
        sparkles::sparql::update::update_as(
            &s,
            "INSERT DATA { GRAPH <urn:x-sparkles:graphrag:reports> { <http://ex/r> <http://ex/p> 1 } }",
            &opts,
            CommitKind::Graphrag,
        )
        .unwrap();
        assert!(
            !source_changed(&s, after),
            "a GraphRAG commit is not a source change"
        );
        load("<urn:g:a> { <http://ex/a> <http://ex/p> <http://ex/c> . }");
        assert!(source_changed(&s, after));
    }

    #[test]
    fn pending_communities_and_failures_are_retried_without_a_commit() {
        let d = Duration::from_secs(10);
        let t0 = Instant::now();
        let mut w = Watch::default();
        assert!(!w.wants_check(5, t0, d, 100));
        assert!(w.wants_check(5, t0 + d, d, 100));
        w.checked();
        // pending communities: once per day, with no new commit
        w.has_pending = true;
        w.retried_day = Some(100);
        assert!(!w.wants_check(5, t0 + d * 2, d, 100));
        assert!(w.wants_check(5, t0 + d * 2, d, 101));
        // a backoff holds back the pending retry and new commits alike
        w.failed(t0, d);
        assert!(!w.wants_check(5, t0 + d, d, 101));
        assert!(!w.wants_check(6, t0 + d, d, 101));
        assert!(w.wants_check(6, t0 + d * 2, d, 101));
        w.checked();
        w.failures = 0;
        w.has_pending = false;
        // a failure is retried after a doubling backoff
        w.failed(t0, d);
        assert!(!w.wants_check(5, t0 + d, d, 101));
        assert!(w.wants_check(5, t0 + d * 2, d, 101));
        w.checked();
        w.failed(t0, d);
        assert!(!w.wants_check(5, t0 + d * 3, d, 101));
        assert!(w.wants_check(5, t0 + d * 4, d, 101));
        for _ in 0..10 {
            w.failed(t0, d);
        }
        assert_eq!(w.retry_at, Some(t0 + d * 64));
    }

    /// A private check that fails backs off on its own, and never holds back the loop's
    /// other triggers.
    #[test]
    fn private_checks_back_off_on_their_own() {
        let d = Duration::from_secs(10);
        let t0 = Instant::now();
        let mut w = Watch::default();
        assert!(!w.private.ready(t0));
        w.private.due = true;
        assert!(w.private.ready(t0));
        w.private.failed(t0, d);
        assert!(!w.private.ready(t0 + d));
        assert!(w.private.ready(t0 + d * 2));
        assert_eq!(w.retry_at, None, "the loop's own backoff is untouched");
        assert!(!w.wants_check(5, t0, d, 100));
        assert!(w.wants_check(5, t0 + d, d, 100));
        w.private.failed(t0, d);
        assert!(!w.private.ready(t0 + d * 3));
        assert!(w.private.ready(t0 + d * 4));
        w.private.done();
        assert!(!w.private.ready(t0 + d * 4));
        assert_eq!(w.private.failures, 0);
    }

    /// Only a refresh that ended well clears a due private check, and refreshes that
    /// keep failing back off further each time.
    #[test]
    fn a_due_check_waits_for_a_refresh_that_ends_well() {
        let d = Duration::from_secs(10);
        let t0 = Instant::now();
        for (state, cleared) in [
            (Some(task_state::DONE), true),
            (Some(task_state::CANCELLED), false),
            (Some(task_state::FAILED), false),
            (None, false),
        ] {
            let mut w = Watch {
                task: Some("t".into()),
                ..Default::default()
            };
            w.private.due = true;
            assert!(!w.follow(state, 5, t0, d), "{state:?}");
            assert_eq!(w.private.due, !cleared, "{state:?}");
            assert_eq!(w.task, None);
        }
        let mut w = Watch {
            task: Some("t".into()),
            ..Default::default()
        };
        assert!(w.follow(Some(task_state::RUNNING), 5, t0, d));
        assert!(w.task.is_some());
        // consecutive failures, with no reset between them, double the backoff
        let mut w = Watch::default();
        w.follow(Some(task_state::FAILED), 5, t0, d);
        assert_eq!(w.retry_at, Some(t0 + d * 2));
        w.follow(Some(task_state::FAILED), 5, t0, d);
        assert_eq!(w.retry_at, Some(t0 + d * 4));
    }

    /// A feed that cannot count the changes makes a check due once per head.
    #[test]
    fn an_uncountable_feed_is_checked_once_per_head() {
        let mut p = Private::default();
        p.uncounted(7);
        assert!(p.due);
        p.done();
        p.covered = Some(7);
        p.uncounted(7);
        assert!(!p.due, "the check already covered head 7");
        p.uncounted(8);
        assert!(p.due);
    }

    #[test]
    fn a_check_is_due_after_the_debounce_or_ten_periods() {
        let d = Duration::from_secs(10);
        let t0 = Instant::now();
        let mut w = Watch::default();
        // first sight waits one debounce
        assert!(!w.due(5, t0, d));
        assert!(w.due(5, t0 + d, d));
        w.checked();
        assert!(!w.due(5, t0 + d * 2, d));
        // commits keep coming: due after ten periods at the latest
        let start = t0 + d * 3;
        for i in 0..10 {
            assert!(!w.due(6 + i, start + Duration::from_secs(i * 9), d), "{i}");
        }
        assert!(w.due(16, start + d * 10, d));
    }

    /// `build_accumulator_query` + `Counter::apply_counts` match the reference feed
    /// counter across single-graph, multi-graph UNION, wildcard-prefix, excluded-graph,
    /// custom-exclude (`rdf:type` included), multi-community and entity-join workloads.
    #[test]
    fn accumulator_query_matches_feed_counter_across_projection_shapes() {
        use sparkles::incr::{InternalAccess, InternalSpec, OpenHost, Runtime};
        use sparkles::io::{RdfFormat, Source};

        let multi_state = BuildState {
            last_build: Some(BuildRecord {
                source_commit: 1,
                branch: "main".into(),
                finished_at: String::new(),
                communities: vec![
                    StoredCommunity {
                        depth: 0,
                        iri: "urn:x-sparkles:graphrag:ds:0:c1".into(),
                        parent: None,
                        members: vec!["<http://ex/a>".into(), "<http://ex/b>".into()],
                        report: None,
                        facts: (0..10).collect(),
                        policy: false,
                        pairs: None,
                    },
                    StoredCommunity {
                        depth: 0,
                        iri: "urn:x-sparkles:graphrag:ds:0:c2".into(),
                        parent: None,
                        members: vec!["<http://ex/c>".into(), "<http://ex/d>".into()],
                        report: None,
                        facts: (0..10).collect(),
                        policy: false,
                        pairs: None,
                    },
                ],
                pending: Vec::new(),
                tokens: 0,
                reports_graph: String::new(),
                footprint: String::new(),
                policy: None,
                pairs: Vec::new(),
            }),
            ..Default::default()
        };

        struct Shape<'a> {
            label: &'a str,
            graphs: &'a [&'a str],
            exclude: &'a [&'a str],
            output: Option<&'a str>,
            excluded_graphs: &'a [&'a str],
        }
        let shapes = [
            Shape {
                label: "single-exact",
                graphs: &["urn:g:a"],
                exclude: &["http://www.w3.org/1999/02/22-rdf-syntax-ns#type"],
                output: None,
                excluded_graphs: &[],
            },
            Shape {
                label: "multi-exact-union",
                graphs: &["urn:g:a", "urn:g:b"],
                exclude: &[
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                    "http://ex/ignored",
                ],
                output: None,
                excluded_graphs: &[],
            },
            Shape {
                label: "wildcard-with-policy-extra",
                graphs: &["urn:g:*"],
                exclude: &["http://www.w3.org/1999/02/22-rdf-syntax-ns#type"],
                output: Some("urn:ev:out"),
                excluded_graphs: &["urn:g:excluded"],
            },
            Shape {
                label: "rdf-type-not-excluded",
                graphs: &["urn:g:a", "urn:g:b"],
                exclude: &["http://ex/ignored"],
                output: None,
                excluded_graphs: &[],
            },
        ];

        let updates: &[&str] = &[
            // insertions & deletions across both communities
            "INSERT DATA { GRAPH <urn:g:a> { <http://ex/a> <http://ex/p> 1 . <http://ex/c> <http://ex/p> 2 } }",
            "DELETE DATA { GRAPH <urn:g:a> { <http://ex/a> <http://ex/p> 1 } }",
            // replacement within one commit and cross-graph move
            "DELETE { GRAPH <urn:g:a> { <http://ex/b> <http://ex/name> \"B\" } } \
             INSERT { GRAPH <urn:g:b> { <http://ex/b> <http://ex/name> \"B2\" } } WHERE {}",
            // add+delete same quad in one commit (net 0)
            "INSERT DATA { GRAPH <urn:g:a> { <http://ex/d> <http://ex/tmp> 9 } } ; \
             DELETE DATA { GRAPH <urn:g:a> { <http://ex/d> <http://ex/tmp> 9 } }",
            // ignored predicate, guarded graph, excluded graph, self-loop, blank object
            "INSERT DATA { \
               GRAPH <urn:g:a> { <http://ex/a> <http://ex/ignored> 1 . <http://ex/a> <http://ex/self> <http://ex/a> . <http://ex/a> <http://ex/bn> _:b1 } \
               GRAPH <urn:x-sparkles:graphrag:reports> { <http://ex/a> <http://ex/p> 99 } \
               GRAPH <urn:g:excluded> { <http://ex/a> <http://ex/p> 99 } \
               GRAPH <urn:ev:out> { <http://ex/c> <http://ex/outProp> \"ok\" } \
             }",
            // rdf:type on a known member to another known member (never counts as a fact)
            "INSERT DATA { GRAPH <urn:g:a> { <http://ex/a> a <http://ex/b> } }",
        ];

        for Shape {
            label,
            graphs,
            exclude,
            output,
            excluded_graphs,
        } in shapes
        {
            let mut settings = GraphragSettings::default();
            settings.projection.graphs = graphs.iter().map(|s| s.to_string()).collect();
            settings.projection.exclude = exclude.iter().map(|s| s.to_string()).collect();
            let excl_set: BTreeSet<String> =
                excluded_graphs.iter().map(|s| s.to_string()).collect();
            let extra = super::super::projection::Extra {
                output,
                excluded: (!excl_set.is_empty()).then_some(&excl_set),
            };
            let (query, keys) = build_accumulator_query(&settings, &extra).unwrap();

            for per_commit in [true, false] {
                let s = sparkles::store::Store::in_memory(Default::default());
                s.load(&[Source::from_bytes(
                    b"<urn:g:a> { <http://ex/a> <http://ex/name> \"A\" . <http://ex/b> <http://ex/name> \"B\" . } \
                      <urn:g:b> { <http://ex/c> <http://ex/name> \"C\" . }"
                        .to_vec(),
                    RdfFormat::TriG,
                    None,
                )])
                .unwrap();
                let rt = Runtime::open(None, s.dataset_id(), Default::default());
                rt.set_host(Arc::new(OpenHost));
                let c = rt
                    .attach_internal(
                        &s,
                        InternalSpec::accumulator(CIRCUIT_ID, query.clone(), keys.clone()),
                        InternalAccess::everything(),
                    )
                    .unwrap();
                let after = s.head_commit().seq;
                let opts = sparkles::sparql::QueryOptions::default();
                for u in updates {
                    sparkles::sparql::update::update(&s, u, &opts).unwrap();
                    if per_commit {
                        rt.round(&s);
                    }
                }
                rt.round(&s);
                let counts = c.counts(after).unwrap();
                assert!(counts.complete(), "{label}: {counts:?}");

                let mut acc_counter = Counter::new(&multi_state, &settings);
                let acc_joined = acc_counter.apply_counts(&counts);

                let mut ref_counter = Counter::new(&multi_state, &settings);
                let mut ref_joined = None;
                let page = s.changes(after, &ChangesOptions::default()).unwrap();
                for commit in &page.commits {
                    for (_, q) in commit.iter() {
                        let GraphName::NamedNode(g) = &q.graph_name else {
                            continue;
                        };
                        if !extra.reads(&settings, g.as_str()) {
                            continue;
                        }
                        if let Some(j) =
                            ref_counter.change(&q.subject, &q.predicate.to_string(), &q.object)
                            && ref_joined.is_none()
                        {
                            ref_joined = Some(j);
                        }
                    }
                }
                assert_eq!(acc_joined, ref_joined, "{label} per_commit={per_commit}");
                assert_eq!(
                    acc_counter.changed, ref_counter.changed,
                    "{label} per_commit={per_commit}"
                );
                assert!(
                    acc_counter.changed.iter().any(|&n| n > 0),
                    "{label} should observe changed facts"
                );
            }
        }

        // Verify entity joining in both directions (`member -> new` and `new -> member`),
        // and that `unknown -> unknown` does not trigger a join.
        for (update, want_join) in [
            (
                "INSERT DATA { GRAPH <urn:g:a> { <http://ex/a> <http://ex/knows> <http://ex/new1> } }",
                Some("<http://ex/new1> joined a community"),
            ),
            (
                "INSERT DATA { GRAPH <urn:g:a> { <http://ex/new2> <http://ex/knows> <http://ex/c> } }",
                Some("<http://ex/new2> joined a community"),
            ),
            (
                "INSERT DATA { GRAPH <urn:g:a> { <http://ex/u1> <http://ex/knows> <http://ex/u2> } }",
                None,
            ),
        ] {
            let mut settings = GraphragSettings::default();
            settings.projection.graphs = vec!["urn:g:a".into()];
            let (query, keys) =
                build_accumulator_query(&settings, &super::super::projection::Extra::default())
                    .unwrap();
            let s = sparkles::store::Store::in_memory(Default::default());
            let rt = Runtime::open(None, s.dataset_id(), Default::default());
            rt.set_host(Arc::new(OpenHost));
            let c = rt
                .attach_internal(
                    &s,
                    InternalSpec::accumulator(CIRCUIT_ID, query, keys),
                    InternalAccess::everything(),
                )
                .unwrap();
            let after = s.head_commit().seq;
            sparkles::sparql::update::update(&s, update, &Default::default()).unwrap();
            rt.round(&s);
            let counts = c.counts(after).unwrap();
            assert!(counts.complete());
            let mut counter = Counter::new(&multi_state, &settings);
            assert_eq!(
                counter.apply_counts(&counts).as_deref(),
                want_join,
                "{update}"
            );
        }
    }

    /// Exact `urn:x-arq:*` graph IRIs must route through `GRAPH ?g` physical-name
    /// filtering rather than `GRAPH <iri>`, staying stateless (no `Recompute` scan) and
    /// never counting quads from unrelated named graphs or the default graph.
    #[test]
    fn accumulator_query_routes_arq_special_graphs_through_physical_filter() {
        use sparkles::incr::{InternalAccess, InternalSpec, OpenHost, Runtime};
        use sparkles::io::{RdfFormat, Source};

        let mut settings = GraphragSettings::default();
        settings.projection.graphs = vec![
            "urn:x-arq:UnionGraph".into(),
            "urn:x-arq:DefaultGraph".into(),
            "urn:x-arq:custom".into(),
        ];
        let (query, keys) =
            build_accumulator_query(&settings, &super::super::projection::Extra::default())
                .unwrap();
        assert!(
            query.contains("GRAPH ?g"),
            "urn:x-arq:* IRIs must use physical GRAPH ?g filter: {query}"
        );
        let s = sparkles::store::Store::in_memory(Default::default());
        s.load(&[Source::from_bytes(
            b"<urn:other> { <http://ex/a> <http://ex/name> \"A\" . } \
              <http://ex/a> <http://ex/def> \"D\" ."
                .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
        let rt = Runtime::open(None, s.dataset_id(), Default::default());
        rt.set_host(Arc::new(OpenHost));
        let c = rt
            .attach_internal(
                &s,
                InternalSpec::accumulator(CIRCUIT_ID, query, keys),
                InternalAccess::everything(),
            )
            .unwrap();
        assert_eq!(
            c.bytes(),
            0,
            "urn:x-arq:* filter path must compile statelessly with zero retained state"
        );
        let after = s.head_commit().seq;
        // Writes to unrelated named graphs and the default graph must NOT be counted
        // despite `urn:x-arq:UnionGraph` and `urn:x-arq:DefaultGraph` in projection.graphs.
        sparkles::sparql::update::update(
            &s,
            "INSERT DATA { \
               GRAPH <urn:unrelated> { <http://ex/a> <http://ex/p> \"u\" } \
               <http://ex/a> <http://ex/p> \"d\" \
             }",
            &Default::default(),
        )
        .unwrap();
        rt.round(&s);
        let counts = c.counts(after).unwrap();
        assert!(counts.complete());
        assert!(
            counts.keys.is_empty(),
            "unrelated named graph and default graph must not match ARQ aliases: {counts:?}"
        );
        // Writes to the physical named graph `<urn:x-arq:custom>` ARE counted.
        sparkles::sparql::update::update(
            &s,
            "INSERT DATA { GRAPH <urn:x-arq:custom> { <http://ex/a> <http://ex/p> \"hit\" } }",
            &Default::default(),
        )
        .unwrap();
        rt.round(&s);
        let counts = c.counts(after).unwrap();
        assert!(counts.complete());
        let st = state();
        let mut counter = Counter::new(&st, &settings);
        counter.apply_counts(&counts);
        assert_eq!(counter.changed, vec![1]);
    }

    /// Blank-node subjects are filtered out by `isIRI(?s)` so they never pollute the
    /// bounded accumulator tally under either `rdf:type` exclusion setting.
    #[test]
    fn accumulator_query_excludes_blank_node_subjects_from_tally() {
        use sparkles::incr::{InternalAccess, InternalSpec, OpenHost, Runtime};

        for exclude in [
            vec!["http://www.w3.org/1999/02/22-rdf-syntax-ns#type".to_string()],
            Vec::new(),
        ] {
            let mut settings = GraphragSettings::default();
            settings.projection.graphs = vec!["urn:g:a".into()];
            settings.projection.exclude = exclude;
            let (query, keys) =
                build_accumulator_query(&settings, &super::super::projection::Extra::default())
                    .unwrap();
            let s = sparkles::store::Store::in_memory(Default::default());
            let rt = Runtime::open(None, s.dataset_id(), Default::default());
            rt.set_host(Arc::new(OpenHost));
            let c = rt
                .attach_internal(
                    &s,
                    InternalSpec::accumulator(CIRCUIT_ID, query, keys),
                    InternalAccess::everything(),
                )
                .unwrap();
            let after = s.head_commit().seq;
            sparkles::sparql::update::update(
                &s,
                "INSERT DATA { GRAPH <urn:g:a> { \
                   _:b1 <http://ex/name> \"anon\" . \
                   _:b2 <http://ex/knows> <http://ex/a> . \
                   _:b3 a <http://ex/Thing> \
                 } }",
                &Default::default(),
            )
            .unwrap();
            rt.round(&s);
            let counts = c.counts(after).unwrap();
            assert!(counts.complete());
            assert!(
                counts.keys.is_empty(),
                "blank-node subjects must not enter the accumulator tally: {counts:?}"
            );
        }
    }

    /// Exact graph lists compile to per-graph `UNION` up to 16 distinct IRIs (after
    /// deduplication and `extra.output` insertion) and switch to `GRAPH ?g` at 17, while
    /// both produce identical counts.
    #[test]
    fn accumulator_query_exact_graph_cutoff_at_16_and_17() {
        use sparkles::incr::{InternalAccess, InternalSpec, OpenHost, Runtime};

        // 15 unique graphs + 1 duplicate + 1 distinct extra.output => 16 unique IRIs (UNION).
        let mut settings16 = GraphragSettings::default();
        settings16.projection.graphs = (0..15).map(|i| format!("urn:g:{i}")).collect();
        settings16.projection.graphs.push("urn:g:0".into());
        let extra16 = super::super::projection::Extra {
            output: Some("urn:ev:out"),
            excluded: None,
        };
        let (q16, k16) = build_accumulator_query(&settings16, &extra16).unwrap();
        assert!(
            !q16.contains("GRAPH ?g") && q16.contains("UNION"),
            "16 distinct exact graphs should compile to per-graph UNION: {q16}"
        );

        // 16 unique graphs + 1 distinct extra.output => 17 unique IRIs (GRAPH ?g filter).
        let mut settings17 = GraphragSettings::default();
        settings17.projection.graphs = (0..16).map(|i| format!("urn:g:{i}")).collect();
        let extra17 = super::super::projection::Extra {
            output: Some("urn:ev:out"),
            excluded: None,
        };
        let (q17, k17) = build_accumulator_query(&settings17, &extra17).unwrap();
        assert!(
            q17.contains("GRAPH ?g"),
            "17 distinct exact graphs should compile to GRAPH ?g filter: {q17}"
        );

        let st = state();
        for (label, settings, query, keys) in [
            ("16-exact", &settings16, q16, k16),
            ("17-exact", &settings17, q17, k17),
        ] {
            let s = sparkles::store::Store::in_memory(Default::default());
            let rt = Runtime::open(None, s.dataset_id(), Default::default());
            rt.set_host(Arc::new(OpenHost));
            let c = rt
                .attach_internal(
                    &s,
                    InternalSpec::accumulator(CIRCUIT_ID, query, keys),
                    InternalAccess::everything(),
                )
                .unwrap();
            let after = s.head_commit().seq;
            sparkles::sparql::update::update(
                &s,
                "INSERT DATA { \
                   GRAPH <urn:g:0> { <http://ex/a> <http://ex/p> \"1\" } \
                   GRAPH <urn:ev:out> { <http://ex/a> <http://ex/p> \"2\" } \
                   GRAPH <urn:outside> { <http://ex/a> <http://ex/p> \"3\" } \
                 }",
                &Default::default(),
            )
            .unwrap();
            rt.round(&s);
            let counts = c.counts(after).unwrap();
            assert!(counts.complete(), "{label}");
            let mut counter = Counter::new(&st, settings);
            counter.apply_counts(&counts);
            assert_eq!(counter.changed, vec![2], "{label}");
        }
    }
}
