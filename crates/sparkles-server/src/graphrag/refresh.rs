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
}

impl Watch {
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
        let retry = self.retry_at.is_some_and(|t| now >= t);
        let pending = self.has_pending && self.retried_day != Some(today);
        commits || retry || pending
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
                Some(task_state::FAILED) => w.failed(now, debounce),
                _ => w.failures = 0,
            }
            w.task = None;
        }
        if !w.wants_check(head, now, debounce, build::today()) {
            continue;
        }
        // a running or queued build or refresh goes first, and the check waits for it
        if st.active_task(build::TASK_KIND, &name).is_some() || st.task_room().is_err() {
            continue;
        }
        w.checked();
        let reason = match due_reason(st, &ds, &settings, w) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(target: "sparkles::graphrag", dataset = %name, "automatic refresh: {e}");
                w.failed(now, debounce);
                continue;
            }
        };
        let Some(reason) = reason else {
            w.failures = 0;
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

/// Why the dataset needs a refresh now, if it does.
fn due_reason(
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
                return Ok(Some(format!(
                    "commit {} changed too many facts to count",
                    c.commit.seq
                )));
            }
            for (_, q) in c.iter() {
                let GraphName::NamedNode(g) = &q.graph_name else {
                    continue;
                };
                if is_guarded(g.as_str()) || !super::projection::named(settings, g.as_str()) {
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
                }],
                pending: Vec::new(),
                tokens: 0,
                reports_graph: String::new(),
                footprint: String::new(),
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
