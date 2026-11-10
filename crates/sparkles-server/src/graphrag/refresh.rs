//! Automatic refresh from the change feed (spec C22 §5, Phase 3).
//!
//! A loop on the server's runtime looks at every dataset whose GraphRAG settings have
//! `refresh.mode: "auto"` once a [`TICK`]. A dataset is due after `debounceSeconds`
//! without a new commit, or after ten debounce periods while commits continue. A due
//! dataset reads the change feed from the commit of its last build or refresh, through
//! the settings' principal's view, and counts the changed facts of each community from
//! the communities the build stored. When a community's changed facts reach
//! `minChangedFraction` of the facts its report was written from, when an entity joins a
//! community, or when the feed cannot list the changes, the loop starts a refresh task.
//! The refresh compares the fact digests of every report, as a manual refresh does, so
//! it rewrites exactly the reports that changed enough and keeps the rest. It needs no
//! confirmation, and the daily token budget bounds it.
//!
//! The loop never starts a first build, never runs on a branch other than main, and
//! leaves a dataset alone while one of its GraphRAG tasks is queued or running. A run
//! that the budget stopped with pending communities is retried once a day.
//!
//! Under a review policy, a report's input can change without any change that the
//! principal's feed lists (spec C25, "Refresh"). The loop then runs a private check in
//! place of a refresh: when a commit changes the policy's decisions or supports graph or
//! adds a withdrawal to a proposal graph, which it reads from the feed without the
//! principal's view, when the selected evidence configuration or the access policy's
//! generation differs from the one it last checked, when it first sees the dataset, and
//! when the principal's feed cannot list the changes. The check is a refresh's
//! preparation and plan with no model call, commit, state change or task, and it starts
//! a refresh only for a change of a report's input. It keeps its own backoff, so a check
//! that fails never holds back what the principal's feed starts.

use super::build::{self, Mode};
use super::settings::{GraphragSettings, RefreshMode, is_guarded};
use super::state::BuildState;
use crate::auth::Endpoint;
use crate::state::{AppState, Dataset, task_state};
use oxrdf::{GraphName, NamedOrBlankNode, Term};
use sparkles::commit::CommitKind;
use sparkles::store::ChangesOptions;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the loop looks at the datasets.
pub const TICK: Duration = Duration::from_secs(1);

/// The most commits one page of the feed lists.
const PAGE_COMMITS: usize = 1000;

/// The most changes one page of the feed lists. A commit with more is listed with its
/// counts only, and the loop then refreshes rather than counting.
const PAGE_QUADS: u64 = 200_000;

/// How long one reading of the feed may take.
const FEED_DEADLINE: Duration = Duration::from_secs(60);

const RDF_TYPE: &str = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";

/// The class of a claim's withdrawal.
const WITHDRAWAL: &str = "urn:x-sparkles:evidence:Withdrawal";

/// What the loop remembers of one dataset.
#[derive(Debug, Default)]
pub struct Watch {
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
}

impl Watch {
    /// The private check's record, for tests.
    pub(crate) fn private(&self) -> &Private {
        &self.private
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

/// One look at every dataset: start the refreshes that are due.
pub fn tick_once(st: &Arc<AppState>, watches: &mut HashMap<String, Watch>, now: Instant) {
    let datasets = st.datasets();
    watches.retain(|name, _| datasets.contains_key(name));
    for (name, ds) in datasets {
        let settings: GraphragSettings =
            crate::settings::effective(st, &ds, &crate::settings::GRAPHRAG);
        if !settings.enabled || settings.refresh.mode != RefreshMode::Auto {
            watches.remove(&name);
            continue;
        }
        let head = ds.store.head_commit().seq;
        let debounce = Duration::from_secs(settings.refresh.debounce_seconds);
        let w = watches.entry(name.clone()).or_default();
        // the refresh this loop started: wait for it, and back off when it failed
        if let Some(id) = &w.task {
            let state = st
                .tasks
                .lock()
                .iter()
                .find(|t| &t.id == id)
                .map(|t| t.state.clone());
            match state.as_deref() {
                Some(task_state::QUEUED | task_state::RUNNING) => {
                    w.due(head, now, debounce);
                    continue;
                }
                Some(task_state::FAILED) => {
                    w.failed(now, debounce);
                    // the failed run used no daily retry of pending communities
                    w.retried_day = None;
                }
                // the refresh read everything a due private check would have read
                _ => {
                    w.failures = 0;
                    w.private.done();
                }
            }
            w.task = None;
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
        let reason = match due_reason(st, &ds, &settings, w) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(target: "sparkles::graphrag", dataset = %name, "automatic refresh: {e}");
                w.failed(now, debounce);
                continue;
            }
        };
        let reason = match reason {
            Some(r) => Some(r),
            None => {
                w.failures = 0;
                private_check(st, &ds, &settings, w, now, debounce)
            }
        };
        let Some(reason) = reason else {
            continue;
        };
        tracing::info!(target: "sparkles::graphrag", dataset = %name, "automatic refresh: {reason}");
        match build::spawn(st, ds.clone(), settings, Mode::Refresh, true, Some(reason)) {
            Ok(t) => w.task = Some(t.id),
            // a build or refresh started meanwhile: check again after it
            Err(_) => w.retry_at = Some(now + debounce),
        }
    }
}

/// The reason a refresh starts after a private check finds a changed input. It names no
/// record and no trigger, since the change may be one the principal cannot see.
pub const CHECK_REASON: &str = "a check under the review policy found reports whose input changed";

/// Run the private check when one is due and its backoff has passed. `Some` when it
/// found a change of a report's input; nothing else the check does is visible.
fn private_check(
    st: &Arc<AppState>,
    ds: &Arc<Dataset>,
    settings: &GraphragSettings,
    w: &mut Watch,
    now: Instant,
    debounce: Duration,
) -> Option<String> {
    if settings.review_policy.is_none() || !w.private.ready(now) {
        return None;
    }
    w.private.runs += 1;
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
            w.private.done();
            None
        }
        Ok(Some(why)) => {
            // the refresh it starts clears the check once it ends well
            tracing::debug!(target: "sparkles::graphrag", dataset = %ds.name, "automatic refresh check: {why}");
            Some(CHECK_REASON.into())
        }
        Err(e) => {
            tracing::warn!(target: "sparkles::graphrag", dataset = %ds.name, "automatic refresh check: {e}");
            w.private.failed(now, debounce);
            None
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

/// Why the dataset needs a refresh now, if it does.
pub(crate) fn due_reason(
    st: &AppState,
    ds: &Dataset,
    settings: &GraphragSettings,
    w: &mut Watch,
) -> Result<Option<String>, String> {
    let state = build::state_of(st, ds)?;
    let Some(b) = &state.last_build else {
        return Ok(None);
    };
    if b.branch != sparkles::branch::MAIN {
        return Ok(None);
    }
    w.has_pending = !b.pending.is_empty();
    w.had_policy = b.policy.is_some();
    // the policy the reports read under: enabling, switching or removing it, or a new
    // version, rewrites every report (spec C25, "Reports on accepted evidence")
    let evidence = match &settings.review_policy {
        Some(name) => Some(
            super::evidence::Evidence::admit(
                ds,
                Some(name),
                None,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Default::default(),
            )
            .map_err(|e| e.to_string())?
            .expect("a named policy"),
        ),
        None => None,
    };
    if let Some(e) = &evidence {
        note_private(st, ds, e, w);
    }
    match (&evidence, &b.policy) {
        (None, None) => {}
        (None, Some(_)) => {
            return Ok(Some(
                "the reports no longer read under a review policy".into(),
            ));
        }
        (Some(e), stored) => {
            if !stored.as_ref().is_some_and(|p| {
                p.name == e.policy.name && p.iri == e.policy.iri && p.version == e.policy.version
            }) {
                return Ok(Some(format!(
                    "the reports read under review policy {} at version {}",
                    e.policy.name, e.policy.version
                )));
            }
        }
    }
    if !b.pending.is_empty() {
        let today = build::today();
        if w.retried_day != Some(today) {
            w.retried_day = Some(today);
            return Ok(Some(format!(
                "{} communities are pending from an earlier run",
                b.pending.len()
            )));
        }
    }
    // only GraphRAG's own commits since then: nothing the reports read changed, which
    // also keeps a refresh from following its own commit when the feed is unavailable
    if !source_changed(&ds.store, b.source_commit) {
        return Ok(None);
    }
    let principal = build::authorized_principal(st, ds, &settings.principal)?;
    let o = ChangesOptions {
        max_commits: PAGE_COMMITS,
        max_quads: PAGE_QUADS,
        cancel: None,
        deadline: Some(Instant::now() + FEED_DEADLINE),
        graphs: principal.view(&ds.name, Endpoint::Query),
    };
    let mut counter = Counter::new(&state, settings);
    let mut after = b.source_commit;
    loop {
        let page = match ds.store.changes(after, &o) {
            Ok(p) => p,
            // the feed cannot list what changed, so the digests decide
            Err(
                e @ (sparkles::Error::HistoryGone(_)
                | sparkles::Error::NotPermitted(_)
                | sparkles::Error::NotFound(_)),
            ) => {
                // under a review policy, the private check decides
                if evidence.is_some() {
                    w.private.due = true;
                    return Ok(None);
                }
                return Ok(Some(format!(
                    "the change feed cannot list the changes: {e}"
                )));
            }
            Err(e) => return Err(e.to_string()),
        };
        for c in &page.commits {
            if c.commit.kind == CommitKind::Graphrag {
                continue;
            }
            if !c.complete() {
                if evidence.is_some() {
                    w.private.due = true;
                    return Ok(None);
                }
                return Ok(Some(format!(
                    "commit {} changed too many facts to count",
                    c.commit.seq
                )));
            }
            for (_, q) in c.iter() {
                let GraphName::NamedNode(g) = &q.graph_name else {
                    continue;
                };
                // a decision, support or withdrawal, seen or not, is the private
                // check's to weigh (`note_private`)
                let output = evidence.as_ref().map(|e| e.policy.output.as_str());
                if is_guarded(g.as_str())
                    || !(super::projection::named(settings, g.as_str())
                        || output == Some(g.as_str()))
                {
                    continue;
                }
                if let Some(r) = counter.change(&q.subject, &q.predicate.to_string(), &q.object) {
                    return Ok(Some(r));
                }
            }
        }
        let next = page.next();
        if page.commits.is_empty() || next >= page.head.seq {
            break;
        }
        after = next;
    }
    Ok(counter.verdict(settings.refresh.min_changed_fraction))
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
    excluded: HashSet<String>,
    changed: Vec<usize>,
}

impl<'a> Counter<'a> {
    fn new(state: &'a BuildState, settings: &GraphragSettings) -> Counter<'a> {
        let mut c = Counter {
            communities: Vec::new(),
            of: HashMap::new(),
            known: HashSet::new(),
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

    /// Count one added or removed quad. `Some` when an entity joins a stored community:
    /// an edge between resources links a member with an entity no community holds.
    fn change(&mut self, s: &NamedOrBlankNode, p: &str, o: &Term) -> Option<String> {
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
        if let (NamedOrBlankNode::NamedNode(_), Term::NamedNode(_)) = (s, o) {
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
    use crate::graphrag::state::{BuildRecord, StoredCommunity};

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
}
