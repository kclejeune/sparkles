//! A change feed: the commits after a given one, each with its net changes
//! ([`Store::changes`]).
//!
//! A page lists the commits after `after` in order, at most `max_commits` of them. Each
//! commit's changes are the quads it added and removed relative to its parent, the diff
//! of one commit. Within the retained generations' logs, one walk reads every commit of
//! the page. A bulk commit has no log records, so its changes come from comparing the
//! two states. A client that saw commit `n` asks for the commits after `n`, so the feed
//! resumes from any readable commit.

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
        }
    }
}

impl Store {
    /// The commits after commit `after`, each with its net changes, in order: a page of
    /// at most `o.max_commits` commits (none when `after` is the head). `after` must be
    /// readable, like every commit listed: a commit past the head is
    /// [`Error::NotFound`], and one whose state is no longer kept is
    /// [`Error::HistoryGone`]. A page ends early where the readable history does, so
    /// the next page reports the commit that cannot be read.
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
        let head = self.head_commit();
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
                    Err(e) if page.commits.is_empty() => return Err(e),
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
        {
            let current = commit::generation_number(&self.snapshot().generation.name);
            let h = hist.lock();
            let ranges = h.reconstructable(current, head.seq);
            match ranges.iter().find(|r| r.0 <= after && after <= r.1) {
                Some(r) if r.1 > after => last = last.min(r.1),
                found => {
                    let seq = if found.is_some() { after + 1 } else { after };
                    return Err(self.history_gone(&h, seq, head.seq, None, self.commit(seq)));
                }
            }
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
}
