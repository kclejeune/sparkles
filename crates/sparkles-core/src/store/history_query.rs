//! History queries: the recorded changes of a range of commits, filtered by subject,
//! predicate, object, graph and operation ([`Store::history_changes`]), read from the
//! change log ([`ChangeLog`]).
//!
//! The model follows a system-versioned table (SQL:2011) and an append-only log of
//! assertions and retractions: each row is one net change a commit made, an addition or
//! a removal of a quad, with the commit's number, time, kind, author and message. A
//! query names the commits by number or time, and its terms are looked up in the log's
//! index of subjects, predicates, objects and graphs, so it reads only the records that
//! may match.

use super::changelog::{LogFilter, Unrecorded};
use super::diff::{QuadKey, key_quad, visible_key};
use super::*;
use rustc_hash::FxHashMap;
use std::time::Instant;

/// A test-only observation of the existing native history scan check.
#[cfg(feature = "failpoints")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryCheckOutcome {
    Before,
    Passed,
    Cancelled,
    Timeout,
    OtherError,
}

#[cfg(feature = "failpoints")]
pub type HistoryCheckObserver =
    Arc<dyn Fn(u64, HistoryCheckOutcome, Option<Instant>) + Send + Sync>;

#[cfg(feature = "failpoints")]
thread_local! {
    static HISTORY_CHECK_OBSERVER: std::cell::RefCell<Option<HistoryCheckObserver>> = const { std::cell::RefCell::new(None) };
}

/// Install an observer on this thread only. It runs under the change-log lock.
#[cfg(feature = "failpoints")]
pub fn set_history_check_observer(observer: Option<HistoryCheckObserver>) {
    HISTORY_CHECK_OBSERVER.with(|slot| *slot.borrow_mut() = observer);
}

#[cfg(feature = "failpoints")]
fn observe_history_check(n: u64, outcome: HistoryCheckOutcome, deadline: Option<Instant>) {
    let observer = HISTORY_CHECK_OBSERVER.with(|slot| slot.borrow().clone());
    if let Some(observer) = observer {
        observer(n, outcome, deadline);
    }
}

/// One end of the range of commits a history query reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryBound {
    /// a commit number
    Commit(u64),
    /// the commits made at or after (for `from`) or at or before (for `to`) this time,
    /// in milliseconds since the Unix epoch
    Time(i64),
    /// a selector of [`At`](crate::history::At) (`head`, `commit:N`, `time:…`,
    /// `snapshot:NAME`)
    At(crate::history::At),
}

/// What a history query asks for. Empty term lists match anything.
#[derive(Clone, Default)]
pub struct HistoryQuery {
    pub subjects: Vec<Term>,
    pub predicates: Vec<NamedNode>,
    pub objects: Vec<Term>,
    /// the graphs (the default graph included) whose changes are listed
    pub graphs: Vec<GraphName>,
    /// the first commit whose changes are listed (default: the first)
    pub from: Option<HistoryBound>,
    /// the last commit whose changes are listed (default: the head)
    pub to: Option<HistoryBound>,
    /// only additions or only removals
    pub op: Option<DiffOp>,
    /// the most changes listed (0: no limit)
    pub limit: usize,
    /// newest commits first
    pub descending: bool,
    /// the graphs and triples the caller may read (`None`: everything)
    pub access: Option<Arc<crate::access::GraphAccess>>,
    pub cancel: Option<Arc<AtomicBool>>,
    /// Additional immutable cancellation sources, checked during the native log scan.
    pub cancellation: Option<crate::task::Cancellation>,
    pub deadline: Option<Instant>,
}

/// One recorded change.
#[derive(Clone, Debug)]
pub struct HistoryChange {
    pub commit: Arc<ChangeCommit>,
    pub op: DiffOp,
    pub quad: Quad,
}

/// The changes a history query found.
#[derive(Clone, Debug)]
pub struct HistoryResult {
    /// the range of commits read (`from > to` when it is empty)
    pub from: u64,
    pub to: u64,
    /// the newest commit the query could see
    pub head: u64,
    pub changes: Vec<HistoryChange>,
    /// commits in the range whose changes are not recorded
    pub unrecorded: Vec<Unrecorded>,
    /// the limit cut the list short
    pub truncated: bool,
}

/// The vocabulary key a term has in a recorded change. A literal with an inline id is
/// recorded in its canonical form.
fn filter_key(t: &Term) -> Vec<u8> {
    if let Term::Literal(_) = t
        && let Some(l) = id::inline_id(t).and_then(id::inline_to_literal)
    {
        let mut k = Vec::new();
        id::write_literal_key(&l, &mut k);
        return k;
    }
    id::term_key(t)
}

fn graph_filter_key(g: &GraphName) -> Vec<u8> {
    match g {
        GraphName::DefaultGraph => Vec::new(),
        GraphName::NamedNode(n) => id::iri_key(n.as_str()),
        GraphName::BlankNode(b) => id::term_key(&Term::BlankNode(b.clone())),
    }
}

impl HistoryQuery {
    pub(super) fn filter(&self) -> LogFilter {
        let list = |v: Vec<Vec<u8>>| (!v.is_empty()).then_some(v);
        LogFilter {
            keys: [
                list(self.graphs.iter().map(graph_filter_key).collect()),
                list(self.subjects.iter().map(filter_key).collect()),
                list(
                    self.predicates
                        .iter()
                        .map(|p| id::iri_key(p.as_str()))
                        .collect(),
                ),
                list(self.objects.iter().map(filter_key).collect()),
            ],
        }
    }

    pub(super) fn check(&self) -> Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
            || self
                .cancellation
                .as_ref()
                .is_some_and(|c| c.check().is_err())
        {
            return Err(Error::Cancelled);
        }
        if self.deadline.is_some_and(|t| Instant::now() > t) {
            return Err(Error::Timeout);
        }
        Ok(())
    }
}

/// Refuse a caller whose triple protections depend on the data (classes or patterns):
/// which of a past triple's changes it may see would need the state of every commit.
pub(crate) fn check_history_access(a: Option<&Arc<crate::access::GraphAccess>>) -> Result<()> {
    if a.and_then(|a| a.triples.as_ref())
        .is_some_and(|t| t.hides() && !t.state_independent())
    {
        return Err(Error::NotPermitted(
            "history queries are not available to a caller whose protections depend on the \
             data (classes or patterns)"
                .into(),
        ));
    }
    Ok(())
}

impl ChangeLog {
    /// The checks a history query makes before it reads, and its range of commits:
    /// `(from, to, upto)`, with `upto` clipped to what a read-only log holds. It flushes
    /// the queued commits, so the log covers the state the caller reads.
    pub(crate) fn range(&self, q: &HistoryQuery, upto: u64) -> Result<(u64, u64, u64)> {
        let upto = self.prepare(q, upto, None)?;
        let (from, to) = self.bounds(q, upto, &|| q.check(), None)?;
        Ok((from, to, upto))
    }

    /// The checks of [`ChangeLog::range`] and its flush, which can run retention.
    /// Returns `upto` clipped to what a read-only log holds.
    pub(super) fn prepare(
        &self,
        q: &HistoryQuery,
        upto: u64,
        check: Option<&dyn Fn() -> Result<()>>,
    ) -> Result<u64> {
        let through = self.read_only_through();
        let upto = through.map_or(upto, |through| upto.min(through));
        if through.is_some() {
            match check {
                Some(check) => check()?,
                None => q.check()?,
            }
        }
        if !self.is_enabled() {
            return Err(Error::HistoryUnsupported(
                "the change log of this dataset is off; history queries need it".into(),
            ));
        }
        check_history_access(q.access.as_ref())?;
        #[cfg(test)]
        super::changelog::open_tests::observe(super::changelog::open_tests::Event::BeforeFlush);
        if through.is_none()
            && let Some(check) = check
        {
            check()?;
        }
        // Write what the commits up to now queued.
        self.flush(false)?;
        Ok(upto)
    }

    /// The range of commits of [`ChangeLog::range`], once [`ChangeLog::prepare`] has
    /// run: `(from, to)`.
    pub(super) fn bounds(
        &self,
        q: &HistoryQuery,
        upto: u64,
        mut check: &dyn Fn() -> Result<()>,
        budget: Option<&super::changelog::MemoryBudget>,
    ) -> Result<(u64, u64)> {
        let from = match &q.from {
            None => 1,
            Some(HistoryBound::Commit(n)) => (*n).max(1),
            Some(HistoryBound::Time(ms)) => self
                .commit_at_time_checked(ms.saturating_sub(1), &mut check, budget)?
                .map_or(1, |c| c + 1),
            Some(HistoryBound::At(a)) => {
                return Err(Error::Invalid(format!(
                    "the selector {a} must be resolved by the store"
                )));
            }
        };
        let to = match &q.to {
            None => upto,
            Some(HistoryBound::Commit(n)) => (*n).min(upto),
            Some(HistoryBound::Time(ms)) => self
                .commit_at_time_checked(*ms, &mut check, budget)?
                .unwrap_or(0)
                .min(upto),
            Some(HistoryBound::At(a)) => {
                return Err(Error::Invalid(format!(
                    "the selector {a} must be resolved by the store"
                )));
            }
        };
        Ok((from, to))
    }

    /// Run a history query against the commits up to `upto` (the state the caller
    /// reads). Bounds given as [`HistoryBound::At`] must already be resolved.
    pub fn query(&self, q: &HistoryQuery, upto: u64) -> Result<HistoryResult> {
        self.query_with(q, upto, false, None)
    }

    pub(super) fn query_with(
        &self,
        q: &HistoryQuery,
        upto: u64,
        no_wait: bool,
        max_bytes: Option<u64>,
    ) -> Result<HistoryResult> {
        let bounded = no_wait || max_bytes.is_some();
        let budget = max_bytes.map(super::changelog::MemoryBudget::new);
        let (from, to, upto) = if no_wait {
            q.check()?;
            if !self.is_enabled() {
                return Err(Error::HistoryUnsupported("the change log is off".into()));
            }
            check_history_access(q.access.as_ref())?;
            let upto = self
                .read_only_through()
                .map_or(upto, |through| upto.min(through));
            let (from, to) = self.bounds(q, upto, &|| q.check(), budget.as_ref())?;
            (from, to, upto)
        } else if max_bytes.is_some() {
            let check_fn = || q.check();
            let upto = self.prepare(q, upto, Some(&check_fn))?;
            let (from, to) = self.bounds(q, upto, &check_fn, budget.as_ref())?;
            (from, to, upto)
        } else {
            self.range(q, upto)?
        };
        let filter = q.filter();
        let access = q.access.as_ref().filter(|a| !a.reads_everything());
        let mut readable: FxHashMap<Arc<[u8]>, bool> = FxHashMap::default();
        let mut changes: Vec<HistoryChange> = Vec::new();
        let mut memo_guard = budget.as_ref().map(super::changelog::MemoryBudget::guard);
        let mut page_guard = budget.as_ref().map(super::changelog::MemoryBudget::guard);
        let mut memo_content_bytes = 0u64;
        let mut page_content_bytes = 0u64;
        let mut truncated = false;
        let mut n = 0u64;
        let mut check = || {
            n += 1;
            if bounded || self.read_only_through().is_some() {
                return q.check();
            }
            if n & 0xFF != 0 {
                return Ok(());
            }
            #[cfg(feature = "failpoints")]
            observe_history_check(n, HistoryCheckOutcome::Before, q.deadline);
            let result = q.check();
            #[cfg(feature = "failpoints")]
            observe_history_check(
                n,
                match &result {
                    Ok(()) => HistoryCheckOutcome::Passed,
                    Err(Error::Cancelled) => HistoryCheckOutcome::Cancelled,
                    Err(Error::Timeout) => HistoryCheckOutcome::Timeout,
                    Err(_) => HistoryCheckOutcome::OtherError,
                },
                q.deadline,
            );
            result
        };
        let unrecorded = self.scan(
            from,
            to,
            &filter,
            q.descending,
            true,
            no_wait,
            budget.as_ref(),
            &mut check,
            None,
            &mut |c, add, k: &QuadKey| {
                let op = if add { DiffOp::Add } else { DiffOp::Remove };
                if q.op.is_some_and(|o| o != op) {
                    return Ok(true);
                }
                if let Some(a) = access {
                    if let Some(guard) = memo_guard.as_mut()
                        && !a.read.is_all()
                        && !readable.contains_key(&k[0])
                    {
                        let next_content = memo_content_bytes
                            .saturating_add(super::changelog::arc_slice_bytes(k[0].len()));
                        if readable.len() == readable.capacity() {
                            let old = super::changelog::memo_table_bytes(readable.capacity());
                            let new = super::changelog::memo_table_bytes(readable.capacity() + 1);
                            guard.resize(next_content.saturating_add(old).saturating_add(new))?;
                            readable.try_reserve(1).map_err(|_| {
                                Error::HistoryUnsupported(
                                    "history access memo allocation was refused".into(),
                                )
                            })?;
                        }
                        memo_content_bytes = next_content;
                        guard.resize(memo_content_bytes.saturating_add(
                            super::changelog::memo_table_bytes(readable.capacity()),
                        ))?;
                    }
                    if !visible_key(a, &mut readable, k) {
                        return Ok(true);
                    }
                }
                if q.limit > 0 && changes.len() >= q.limit {
                    truncated = true;
                    return Ok(false);
                }
                if let Some(guard) = page_guard.as_mut() {
                    let mut add_bytes = super::changelog::quad_bytes(k);
                    if changes.last().is_none_or(|l| !Arc::ptr_eq(&l.commit, c)) {
                        let commit_size = std::mem::size_of::<ChangeCommit>();
                        add_bytes = add_bytes
                            .saturating_add(super::changelog::arc_slice_bytes(commit_size))
                            .saturating_add(
                                [&c.author, &c.message]
                                    .into_iter()
                                    .flatten()
                                    .map(|s| super::changelog::arc_slice_bytes(s.len()))
                                    .sum::<u64>(),
                            );
                    }
                    let elem_size = std::mem::size_of::<HistoryChange>() as u64;
                    if changes.len() == changes.capacity() {
                        let old_vec = (changes.capacity() as u64).saturating_mul(elem_size);
                        let cap_more = if q.limit > 0 {
                            q.limit.saturating_sub(changes.len()).max(1)
                        } else {
                            usize::MAX
                        };
                        let more = changes.capacity().max(64).min(cap_more);
                        let new_vec = (changes.capacity().saturating_add(more) as u64)
                            .saturating_mul(elem_size);
                        guard.resize(
                            page_content_bytes
                                .saturating_add(old_vec)
                                .saturating_add(new_vec),
                        )?;
                        changes.try_reserve_exact(more).map_err(|_| {
                            Error::HistoryUnsupported(
                                "history result allocation was refused".into(),
                            )
                        })?;
                    }
                    let vec_bytes = (changes.capacity() as u64).saturating_mul(elem_size);
                    let next_content = page_content_bytes.saturating_add(add_bytes);
                    guard.resize(next_content.saturating_add(vec_bytes))?;
                    page_content_bytes = next_content;
                }
                if let Some(quad) = key_quad(k) {
                    changes.push(HistoryChange {
                        commit: c.clone(),
                        op,
                        quad,
                    });
                }
                Ok(true)
            },
        )?;
        drop(memo_guard);
        drop(page_guard);
        Ok(HistoryResult {
            from,
            to,
            head: upto,
            changes,
            unrecorded,
            truncated,
        })
    }
}

impl Store {
    /// The recorded changes that match `q`, read from the change log (see
    /// [`HistoryQuery`]). Changes in graphs or of triples the caller may not read are
    /// left out. Commits whose changes the log does not hold are listed in
    /// [`HistoryResult::unrecorded`].
    pub fn history_changes(&self, q: &HistoryQuery) -> Result<HistoryResult> {
        // Bound `head` BEFORE checking family evidence admission so an already-admitted
        // call can never expand its raw change-log range to include a commit published
        // after Evidence is enabled.
        let head = self.head_commit();
        #[cfg(test)]
        history_admission_tests::observe(history_admission_tests::Phase::BeforeAdmission);
        if q.access
            .as_ref()
            .is_some_and(|a| self.has_evidence_visibility_for_access(a))
        {
            return Err(Error::NotPermitted(
                "history queries are not available to a caller whose view is subject to \
                 support-aware projection visibility"
                    .into(),
            ));
        }
        #[cfg(test)]
        history_admission_tests::observe(history_admission_tests::Phase::AfterAdmission);
        let Some(log) = &self.changelog else {
            return Err(Error::HistoryUnsupported(
                "this store has no change log".into(),
            ));
        };
        let resolve = |b: &Option<HistoryBound>| -> Result<Option<HistoryBound>> {
            Ok(match b {
                Some(HistoryBound::At(a)) => Some(match a {
                    crate::history::At::Time(ms) => HistoryBound::Time(*ms),
                    a => HistoryBound::Commit(self.resolve_seq(a, head)?),
                }),
                b => b.clone(),
            })
        };
        let q = HistoryQuery {
            from: resolve(&q.from)?,
            to: resolve(&q.to)?,
            ..q.clone()
        };
        log.query(&q, head.seq)
    }

    /// Read commit or time bounds through the published head without taking the
    /// writer. Any staged or unpublished changes are beyond the log's requested bound.
    #[doc(hidden)]
    pub fn published_history_changes(
        &self,
        q: &HistoryQuery,
        max_memory_bytes: u64,
    ) -> Result<HistoryResult> {
        let Some(log) = &self.changelog else {
            return Err(Error::HistoryUnsupported(
                "this store has no change log".into(),
            ));
        };
        log.query_with(q, self.snapshot().commit, false, Some(max_memory_bytes))
    }

    /// Read commit bounds through the published head without flushing queued records
    /// or waiting for the log lock. Unflushed records are reported as gaps.
    #[doc(hidden)]
    pub fn try_published_history_changes(
        &self,
        q: &HistoryQuery,
        max_memory_bytes: u64,
    ) -> Result<HistoryResult> {
        if q.from
            .iter()
            .chain(&q.to)
            .any(|bound| !matches!(bound, HistoryBound::Commit(_)))
        {
            return Err(Error::invalid("this history scan requires commit bounds"));
        }
        let Some(log) = &self.changelog else {
            return Err(Error::HistoryUnsupported(
                "this store has no change log".into(),
            ));
        };
        log.query_with(q, self.snapshot().commit, true, Some(max_memory_bytes))
    }

    /// The commit a selector names, without asking that its state be readable.
    fn resolve_seq(&self, a: &crate::history::At, head: CommitInfo) -> Result<u64> {
        use crate::history::At;
        match a {
            At::Head => Ok(head.seq),
            At::Commit(n) => Ok(*n),
            At::Time(ms) => Ok(self.catalog.lock().at_time(*ms).map_or(0, |c| c.seq)),
            At::Snapshot(_) => Ok(self.resolve_with(a, head)?.commit.seq),
        }
    }
}

#[cfg(test)]
mod history_admission_tests {
    use super::*;
    use crate::access::{GraphAccess, GraphRule, Graphs};
    use crate::managed::{OwnerEntry, OwnerKind};
    use crate::task::Control;
    use oxrdf::{NamedNode, Term};
    use std::cell::RefCell;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum Phase {
        BeforeAdmission,
        AfterAdmission,
    }

    type Hook = Box<dyn FnMut(Phase)>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(super) fn observe(phase: Phase) {
        HOOK.with(|h| {
            if let Some(cb) = h.borrow_mut().as_mut() {
                cb(phase);
            }
        });
    }

    struct HookGuard;
    impl Drop for HookGuard {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
        }
    }

    fn set_hook(cb: impl FnMut(Phase) + 'static) -> HookGuard {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(cb)));
        HookGuard
    }

    fn enable_evidence_and_project(store: &Store, output_iri: &str) {
        use crate::evidence::{ClaimKind, ConflictMode, Policy};
        let settings = crate::evidence::Settings {
            enabled: true,
            source_records: vec!["urn:ev:sources".to_string()],
            proposals: vec!["urn:ev:proposals".to_string()],
            decisions: vec!["urn:ev:decisions".to_string()],
            supports: vec!["urn:ev:supports".to_string()],
            projections: vec![output_iri.to_string()],
            policies: [(
                "main".to_string(),
                Policy {
                    version: 1,
                    admitted_kinds: vec![ClaimKind::Reported],
                    required_scope: Vec::new(),
                    conflicts: ConflictMode::AdmitLabelled,
                    boundary: vec!["urn:ev:sources".to_string(), "urn:ev:proposals".to_string()],
                    decisions: "urn:ev:decisions".to_string(),
                    supports: "urn:ev:supports".to_string(),
                    output: output_iri.to_string(),
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let canonical = settings.canonical(&Control::none()).unwrap();
        let bytes = canonical.bytes().to_vec();
        store
            .enable_managed(
                OwnerKind::Evidence,
                OwnerEntry::for_settings(1, settings.managed_graphs(), &bytes),
                &bytes,
            )
            .unwrap();
        let cap = store
            .managed_writer(OwnerKind::Evidence, uuid::Uuid::new_v4())
            .unwrap();
        let mut txn = store.write();
        txn.set_managed(cap).unwrap();
        let s = txn
            .intern(&Term::NamedNode(NamedNode::new_unchecked("urn:secret:s")))
            .unwrap();
        let p = txn
            .intern(&Term::NamedNode(NamedNode::new_unchecked("urn:secret:p")))
            .unwrap();
        let o = txn
            .intern(&Term::NamedNode(NamedNode::new_unchecked("urn:secret:o")))
            .unwrap();
        let g = txn
            .intern(&Term::NamedNode(NamedNode::new_unchecked(output_iri)))
            .unwrap();
        txn.insert([s, p, o, g]).unwrap();
        txn.commit().unwrap();
    }

    #[test]
    fn history_changes_bounds_head_before_evidence_admission() {
        const OUTPUT: &str = "urn:ev:accepted";
        const PUBLIC: &str = "urn:public";
        let access = Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new([OUTPUT, PUBLIC], &[])),
            Graphs::none(),
        ));

        // Interleaving A: Evidence enables + projects before admission -> NotPermitted.
        let dir_a = tempfile::tempdir().unwrap();
        let store_a = Arc::new(Store::open(dir_a.path(), StoreOptions::default()).unwrap());
        store_a
            .set_change_log_settings(crate::store::ChangeLogSettings {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
        crate::sparql::update::update(
            &store_a,
            &format!("INSERT DATA {{ GRAPH <{PUBLIC}> {{ <urn:a> <urn:p> 1 }} }}"),
            &Default::default(),
        )
        .unwrap();
        {
            let s = store_a.clone();
            let mut fired = false;
            let _g = set_hook(move |phase| {
                if phase == Phase::BeforeAdmission && !fired {
                    fired = true;
                    enable_evidence_and_project(&s, OUTPUT);
                }
            });
            let err = store_a
                .history_changes(&HistoryQuery {
                    access: Some(access.clone()),
                    ..Default::default()
                })
                .err();
            assert!(
                matches!(err, Some(Error::NotPermitted(_))),
                "expected NotPermitted when Evidence enables before admission, got {err:?}"
            );
        }

        // Interleaving B: Evidence enables + projects immediately AFTER admission -> bounded
        // to pre-enablement head `c1`, never reading the newly accepted projection commit.
        let dir_b = tempfile::tempdir().unwrap();
        let store_b = Arc::new(Store::open(dir_b.path(), StoreOptions::default()).unwrap());
        store_b
            .set_change_log_settings(crate::store::ChangeLogSettings {
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
        crate::sparql::update::update(
            &store_b,
            &format!("INSERT DATA {{ GRAPH <{PUBLIC}> {{ <urn:b> <urn:p> 1 }} }}"),
            &Default::default(),
        )
        .unwrap();
        let c1 = store_b.head_commit().seq;
        {
            let s = store_b.clone();
            let mut fired = false;
            let _g = set_hook(move |phase| {
                if phase == Phase::AfterAdmission && !fired {
                    fired = true;
                    enable_evidence_and_project(&s, OUTPUT);
                }
            });
            let res = store_b
                .history_changes(&HistoryQuery {
                    access: Some(access),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(res.head, c1);
            assert!(
                store_b.head_commit().seq > c1,
                "post-admission projection commit must have advanced store head"
            );
            for c in &res.changes {
                assert!(
                    c.commit.seq <= c1,
                    "bounded history result must not include post-enablement commit {}",
                    c.commit.seq
                );
                assert_ne!(c.quad.subject.to_string(), "<urn:secret:s>");
            }
        }
    }
}
