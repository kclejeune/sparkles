//! A change feed: the commits after a given one, each with its net changes
//! ([`Store::changes`]).
//!
//! A page lists the commits after `after` in order, at most `max_commits` of them. Each
//! commit's changes are the quads it added and removed relative to its parent, the diff
//! of one commit. Within the retained generations' logs, one walk reads every commit of
//! the page. A bulk commit has no log records, so its changes come from comparing the
//! two states. Commits whose generations are gone are read from the change log
//! ([`ChangeLog`]). A client that saw commit `n` asks for the commits after `n`, so the
//! feed resumes from any commit either source holds.

use super::changelog::{ChangeCommit, LogCommit};
use super::diff::{Keys, QuadKey, Step, key_quad};
use super::*;
use crate::history::At;
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;
use std::time::Instant;

/// Options of [`Store::changes`].
#[derive(Clone)]
pub struct ChangesOptions {
    /// the most commits a page lists (at least one)
    pub max_commits: usize,
    /// The most changes a page lists, all its commits together (0: no limit). A page ends
    /// before the commit that would pass it, and a commit whose changes alone pass it is
    /// listed without them.
    pub max_quads: u64,
    pub cancel: Option<Arc<AtomicBool>>,
    pub deadline: Option<Instant>,
    /// The graphs the caller may read (`None`: every graph). Each commit lists only its
    /// changes in these graphs, and they alone count against `max_quads`.
    pub graphs: Option<Arc<crate::access::GraphAccess>>,
}

impl Default for ChangesOptions {
    fn default() -> ChangesOptions {
        ChangesOptions {
            max_commits: 100,
            max_quads: 0,
            cancel: None,
            deadline: None,
            graphs: None,
        }
    }
}

/// One commit of a change feed and its net changes relative to its parent.
pub struct CommitChanges {
    pub commit: CommitInfo,
    pub added: u64,
    pub removed: u64,
    /// `None` when the changes alone pass the page's limit
    changes: Option<Vec<(QuadKey, DiffOp)>>,
}

impl CommitChanges {
    /// Whether the changes are listed. A commit whose changes pass the limit is listed
    /// with its counts only, and a client reads the state at the commit instead.
    pub fn complete(&self) -> bool {
        self.changes.is_some()
    }

    /// The changes, removals first, then additions, each ordered by graph, subject,
    /// predicate and object (none when not [`complete`](Self::complete)).
    pub fn iter(&self) -> impl Iterator<Item = (DiffOp, Quad)> + '_ {
        self.changes
            .iter()
            .flatten()
            .filter_map(|(k, op)| key_quad(k).map(|q| (*op, q)))
    }

    /// The changes as vocabulary keys, in the order of [`iter`](Self::iter).
    pub(crate) fn keys(&self) -> impl Iterator<Item = (&QuadKey, DiffOp)> + '_ {
        self.changes.iter().flatten().map(|(k, op)| (k, *op))
    }

    fn from_net(commit: CommitInfo, net: FxHashMap<QuadKey, bool>) -> CommitChanges {
        let mut changes: Vec<(QuadKey, DiffOp)> = net
            .into_iter()
            .map(|(k, added)| (k, if added { DiffOp::Add } else { DiffOp::Remove }))
            .collect();
        changes
            .sort_unstable_by(|x, y| (x.1 == DiffOp::Add, &x.0).cmp(&(y.1 == DiffOp::Add, &y.0)));
        let added = changes.iter().filter(|c| c.1 == DiffOp::Add).count() as u64;
        CommitChanges {
            commit,
            added,
            removed: changes.len() as u64 - added,
            changes: Some(changes),
        }
    }

    /// A commit listed with its counts only.
    fn counts_only(commit: CommitInfo) -> CommitChanges {
        CommitChanges {
            commit,
            added: commit.inserted,
            removed: commit.deleted,
            changes: None,
        }
    }

    fn len(&self) -> u64 {
        self.changes.as_ref().map_or(0, |c| c.len() as u64)
    }
}

/// A page of a change feed.
pub struct ChangePage {
    /// the commit the page starts after
    pub after: u64,
    /// the head when the page was read
    pub head: CommitInfo,
    pub commits: Vec<CommitChanges>,
}

impl ChangePage {
    /// The commit the next page starts after: the last one listed.
    pub fn next(&self) -> u64 {
        self.commits.last().map_or(self.after, |c| c.commit.seq)
    }
}

/// Collects the commits of a page within its limits.
struct Page<'o> {
    o: &'o ChangesOptions,
    commits: Vec<CommitChanges>,
    listed: u64,
}

impl Page<'_> {
    /// Add a commit; `false` when the page is full (the commit is not added).
    fn push(&mut self, c: CommitChanges) -> bool {
        if self.commits.len() >= self.o.max_commits.max(1) {
            return false;
        }
        let max = self.o.max_quads;
        let c = if max > 0 && c.len() > max {
            CommitChanges::counts_only(c.commit)
        } else {
            c
        };
        if max > 0 && !self.commits.is_empty() && self.listed + c.len() > max {
            return false;
        }
        self.listed += c.len();
        self.commits.push(c);
        true
    }

    fn full(&self) -> bool {
        self.commits.len() >= self.o.max_commits.max(1)
    }
}

impl ChangesOptions {
    fn diff_options(&self, max_quads: u64) -> DiffOptions {
        DiffOptions {
            graph: None,
            max_quads,
            cancel: self.cancel.clone(),
            deadline: self.deadline,
            graphs: self.graphs.clone(),
            terms: None,
        }
    }
}

impl Store {
    /// The commits after commit `after`, each with its net changes, in order: a page of
    /// at most `o.max_commits` commits (none when `after` is the head). `after` must be
    /// readable, like every commit listed: a commit past the head is
    /// [`Error::NotFound`]. The commits the write-ahead logs no longer hold are read
    /// from the change log, and a commit neither has is [`Error::HistoryGone`]. A page
    /// ends early where one source does, so the next page reads from the other or
    /// reports the commit that cannot be read.
    pub fn changes(&self, after: u64, o: &ChangesOptions) -> Result<ChangePage> {
        // protections that depend on the data would need the view of every commit
        if o.graphs
            .as_ref()
            .and_then(|a| a.triples.as_ref())
            .is_some_and(|t| t.hides() && !t.state_independent())
        {
            return Err(Error::NotPermitted(
                "the change feed is not available to a caller whose protections depend on \
                 the data (classes or patterns); the diff between two commits is"
                    .into(),
            ));
        }
        // Bound the upper commit BEFORE checking family evidence admission. Because
        // `enable_managed` publishes `OwnerKind::Evidence` strictly before any accepted
        // projection commit can advance `head`, reading `head` first guarantees either
        // that `head` predates any evidence projection (bounding the raw log range to
        // commits <= `head.seq`) or that the subsequent Acquire load below observes
        // `OwnerKind::Evidence` and refuses.
        let head = self.head_commit();
        #[cfg(test)]
        changes_admission_tests::observe(changes_admission_tests::Phase::BeforeAdmission);
        if o.graphs
            .as_ref()
            .is_some_and(|a| self.has_evidence_visibility_for_access(a))
        {
            return Err(Error::NotPermitted(
                "the change feed is not available to a caller whose view is subject to \
                 support-aware projection visibility; the diff between two commits is"
                    .into(),
            ));
        }
        #[cfg(test)]
        changes_admission_tests::observe(changes_admission_tests::Phase::AfterAdmission);
        if after > head.seq {
            return Err(Error::NotFound(format!(
                "no commit {after} (head is {})",
                head.seq
            )));
        }
        let mut page = Page {
            o,
            commits: Vec::new(),
            listed: 0,
        };
        let mut last = head
            .seq
            .min(after.saturating_add(o.max_commits.max(1) as u64));
        if after == head.seq {
            return Ok(ChangePage {
                after,
                head,
                commits: Vec::new(),
            });
        }
        let meta = |seq: u64| {
            self.commit(seq)
                .ok_or_else(|| Error::NotFound(format!("commit {seq} is not in the catalog")))
        };
        let Some(hist) = &self.history else {
            // in-memory: the kept states, commit by commit
            for s in after + 1..=last {
                let d = match self.diff(&At::Commit(s - 1), &At::Commit(s), &o.diff_options(0)) {
                    Ok(d) => d,
                    Err(e) if page.commits.is_empty() => {
                        return match self.changes_from_log(after, last, head, o)? {
                            Some(p) => Ok(p),
                            None => Err(e),
                        };
                    }
                    Err(_) => break,
                };
                let c = CommitChanges {
                    commit: d.to.commit,
                    added: d.added,
                    removed: d.removed,
                    changes: Some(d.changes),
                };
                if !page.push(c) {
                    break;
                }
            }
            return Ok(ChangePage {
                after,
                head,
                commits: page.commits,
            });
        };
        // the page stays within one readable range
        let unreadable = {
            let current = commit::generation_number(&self.snapshot().generation.name);
            let h = hist.lock();
            let ranges = h.reconstructable(current, head.seq);
            match ranges.iter().find(|r| r.0 <= after && after <= r.1) {
                Some(r) if r.1 > after => {
                    last = last.min(r.1);
                    None
                }
                found => Some(if found.is_some() { after + 1 } else { after }),
            }
        };
        if let Some(seq) = unreadable {
            // the write-ahead logs no longer hold the next commit: the change log may
            if let Some(p) = self.changes_from_log(after, last, head, o)? {
                return Ok(p);
            }
            let h = hist.lock();
            return Err(self.history_gone(&h, seq, head.seq, None, self.commit(seq)));
        }
        for step in self.diff_plan(after, last)? {
            match step {
                Step::Log {
                    generation,
                    after: a,
                    through,
                } => {
                    let (gen_, mut cursor) = self.open_log(generation, a)?;
                    let mut keys = Keys::new(&gen_);
                    let mut local: FxHashMap<[Id; 4], bool> = FxHashMap::default();
                    let view = o.graphs.as_ref().filter(|a| !a.reads_everything());
                    let mut readable: FxHashMap<Arc<[u8]>, bool> = FxHashMap::default();
                    loop {
                        let Some((seq, txn)) = cursor.next()? else {
                            return Err(Error::Corrupt(format!(
                                "the log of generation {generation} ends before commit {through}"
                            )));
                        };
                        if seq > a && seq <= through {
                            let ddo = o.diff_options(0);
                            ddo.check()?;
                            local.clear();
                            for d in txn.as_chunks::<WAL_REC>().0 {
                                if !wal_is_quad(d[0]) {
                                    continue;
                                }
                                match local.entry(wal::record_quad(d)) {
                                    Entry::Occupied(e) => {
                                        e.remove();
                                    }
                                    Entry::Vacant(e) => {
                                        e.insert(d[0] == WAL_INSERT);
                                    }
                                }
                            }
                            let mut net: FxHashMap<QuadKey, bool> = FxHashMap::default();
                            for (q, added) in local.drain() {
                                let k = keys.quad(&q)?;
                                // a graph view lists the changes of its graphs only
                                if view.is_some_and(|a| {
                                    !super::diff::visible_key(a, &mut readable, &k)
                                }) {
                                    continue;
                                }
                                net.insert(k, added);
                            }
                            if !page.push(CommitChanges::from_net(meta(seq)?, net)) {
                                return Ok(ChangePage {
                                    after,
                                    head,
                                    commits: page.commits,
                                });
                            }
                        }
                        if seq >= through {
                            break;
                        }
                    }
                }
                Step::Compare { a, b } => {
                    // a bulk commit (the page has no gaps): compare its two states
                    let max = o.max_quads;
                    let c = match self.diff(&At::Commit(a), &At::Commit(b), &o.diff_options(max)) {
                        Ok(d) => CommitChanges {
                            commit: d.to.commit,
                            added: d.added,
                            removed: d.removed,
                            changes: Some(d.changes),
                        },
                        Err(Error::BudgetExceeded(_)) => CommitChanges::counts_only(meta(b)?),
                        Err(e) => return Err(e),
                    };
                    if !page.push(c) {
                        break;
                    }
                }
            }
            if page.full() {
                break;
            }
        }
        Ok(ChangePage {
            after,
            head,
            commits: page.commits,
        })
    }

    /// A page read from the change log, for commits whose generations are gone. It
    /// ends before the first commit the log does not record, and is `None` when the
    /// log does not record the commit after `after`. A bulk commit too large to record
    /// is compared between its two states while they are kept, and is otherwise listed
    /// with its counts only.
    fn changes_from_log(
        &self,
        after: u64,
        last: u64,
        head: CommitInfo,
        o: &ChangesOptions,
    ) -> Result<Option<ChangePage>> {
        let Some(log) = self.changelog.as_ref().filter(|l| l.is_enabled()) else {
            return Ok(None);
        };
        log.flush(false)?;
        let mut page = Page {
            o,
            commits: Vec::new(),
            listed: 0,
        };
        let view = o.graphs.as_ref().filter(|a| !a.reads_everything());
        let mut readable: FxHashMap<Arc<[u8]>, bool> = FxHashMap::default();
        let ddo = o.diff_options(0);
        let meta = |c: &ChangeCommit| self.commit(c.seq).unwrap_or_else(|| c.info());
        let mut next = after + 1;
        while next <= last && !page.full() {
            // a summary is resolved outside the scan, which holds the log's lock
            let mut summary: Option<Arc<ChangeCommit>> = None;
            log.commits(next, last, &mut || ddo.check(), &mut |e| match e {
                LogCommit::Changes(c, changes) => {
                    let mut net: FxHashMap<QuadKey, bool> = FxHashMap::default();
                    for (add, k) in changes {
                        // a graph view lists the changes of its graphs only
                        if view.is_some_and(|a| !super::diff::visible_key(a, &mut readable, &k)) {
                            continue;
                        }
                        net.insert(k, add);
                    }
                    if !page.push(CommitChanges::from_net(meta(&c), net)) {
                        return Ok(false);
                    }
                    next = c.seq + 1;
                    Ok(!page.full())
                }
                LogCommit::Summary(c) => {
                    summary = Some(c);
                    Ok(false)
                }
                LogCommit::Missing => Ok(false),
            })?;
            let Some(c) = summary else {
                break;
            };
            let max = o.max_quads;
            let d = self.diff(
                &At::Commit(c.seq - 1),
                &At::Commit(c.seq),
                &o.diff_options(max),
            );
            let cc = match d {
                Ok(d) => CommitChanges {
                    commit: d.to.commit,
                    added: d.added,
                    removed: d.removed,
                    changes: Some(d.changes),
                },
                Err(e @ (Error::Cancelled | Error::Timeout)) => return Err(e),
                Err(_) => CommitChanges::counts_only(meta(&c)),
            };
            if !page.push(cc) {
                break;
            }
            next = c.seq + 1;
        }
        if page.commits.is_empty() {
            return Ok(None);
        }
        Ok(Some(ChangePage {
            after,
            head,
            commits: page.commits,
        }))
    }
}

#[cfg(test)]
mod changes_admission_tests {
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
    fn changes_bounds_head_before_evidence_admission() {
        const OUTPUT: &str = "urn:ev:accepted";
        const PUBLIC: &str = "urn:public";
        let access = Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new([OUTPUT, PUBLIC], &[])),
            Graphs::none(),
        ));

        // Interleaving A: Evidence is enabled and a projection commits between
        // `head_commit()` and `has_evidence_visibility_for_access()`. Admission MUST refuse.
        let dir_a = tempfile::tempdir().unwrap();
        let store_a = Arc::new(Store::open(dir_a.path(), StoreOptions::default()).unwrap());
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
                .changes(
                    0,
                    &ChangesOptions {
                        graphs: Some(access.clone()),
                        ..Default::default()
                    },
                )
                .err();
            assert!(
                matches!(err, Some(Error::NotPermitted(_))),
                "expected NotPermitted when Evidence enables before admission, got {err:?}"
            );
        }

        // Interleaving B: Evidence is enabled and a projection commits immediately AFTER
        // `has_evidence_visibility_for_access()`. Because `head` was bounded beforehand,
        // the WAL scan MUST stop at pre-enablement head `c1` and never leak the projection.
        let dir_b = tempfile::tempdir().unwrap();
        let store_b = Arc::new(Store::open(dir_b.path(), StoreOptions::default()).unwrap());
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
            let page = store_b
                .changes(
                    0,
                    &ChangesOptions {
                        graphs: Some(access),
                        ..Default::default()
                    },
                )
                .unwrap();
            assert_eq!(page.head.seq, c1);
            assert!(
                store_b.head_commit().seq > c1,
                "post-admission projection commit must have advanced store head"
            );
            for cc in &page.commits {
                assert!(
                    cc.commit.seq <= c1,
                    "bounded change page must not include post-enablement commit {}",
                    cc.commit.seq
                );
                for (_, q) in cc.iter() {
                    assert_ne!(q.subject.to_string(), "<urn:secret:s>");
                }
            }
        }
    }
}
