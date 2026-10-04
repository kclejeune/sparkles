//! The commit graph of a dataset's branches ([F09](../../../../docs/specs/F09-branches-and-merges.md)
//! §2.9): the own commits of several branches in time order, newest first, each with its
//! parents, one page at a time. The UI draws it as `git log --graph` does.
//!
//! A branch's own commits have seqs in `(from.seq, head]` (all of them for `main`), and
//! their timestamps never decrease along the sequence. The page order sorts commits by
//! the key `(timestamp, ordinal, seq)`, newest first. The key grows with the seq on each
//! branch, so a page is a merge of the branches' runs, and a cursor (the key of the last
//! commit of a page) finds where each run continues with a binary search.

use super::*;
use crate::annotations::Annotation;
use crate::branch::{self, CommitRef, MAIN, NamedCommitRef};

/// The position after which the next page of a commit graph starts: the key of the
/// last commit a page listed. Written `TIMESTAMP_MS.ORDINAL.SEQ`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphCursor {
    pub timestamp_ms: i64,
    pub ordinal: u16,
    pub seq: u64,
}

impl std::fmt::Display for GraphCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.timestamp_ms, self.ordinal, self.seq)
    }
}

impl std::str::FromStr for GraphCursor {
    type Err = Error;
    fn from_str(s: &str) -> Result<GraphCursor> {
        let bad = || Error::Invalid(format!("invalid commit graph cursor '{s}'"));
        let mut it = s.split('.');
        let (Some(t), Some(o), Some(q), None) = (it.next(), it.next(), it.next(), it.next()) else {
            return Err(bad());
        };
        Ok(GraphCursor {
            timestamp_ms: t.parse().map_err(|_| bad())?,
            ordinal: o.parse().map_err(|_| bad())?,
            seq: q.parse().map_err(|_| bad())?,
        })
    }
}

/// A branch of a commit graph: where it starts and where its head is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphBranch {
    pub name: String,
    pub id: uuid::Uuid,
    pub ordinal: u16,
    /// the head commit
    pub head: CommitInfo,
    /// the commit it started from (`None` for `main`)
    pub from: Option<NamedCommitRef>,
    /// the branch it was created from (`None` for `main`)
    pub upstream: Option<String>,
    pub created_ms: i64,
}

/// A commit of a commit graph.
#[derive(Clone, Debug)]
pub struct GraphCommit {
    pub commit: CommitInfo,
    /// the branch that made it
    pub branch: String,
    pub branch_id: uuid::Uuid,
    /// the first parent, then a merge commit's merged commit, each on the branch that
    /// made it (none for commit 0 of `main`)
    pub parents: Vec<NamedCommitRef>,
    /// a merge commit's merged commit
    pub merged_from: Option<NamedCommitRef>,
    pub annotation: Option<Annotation>,
}

/// One page of a commit graph.
#[derive(Clone, Debug)]
pub struct CommitGraph {
    /// the branches of the graph, `main` first, then by name
    pub branches: Vec<GraphBranch>,
    /// newest first
    pub commits: Vec<GraphCommit>,
    /// the cursor of the next page, when there are older commits
    pub next: Option<GraphCursor>,
}

/// Options of [`Store::commit_graph`].
#[derive(Clone, Debug, Default)]
pub struct CommitGraphOptions {
    /// the branches to draw (`None`: every branch)
    pub branches: Option<Vec<String>>,
    /// list the commits older than this position (`None`: from the newest)
    pub before: Option<GraphCursor>,
    /// commits per page (0 is taken as 1)
    pub limit: usize,
}

/// A branch's run of own commits.
struct Run<'a> {
    store: BranchStore<'a>,
    info: GraphBranch,
    /// the lowest own seq
    low: u64,
}

impl Run<'_> {
    fn key(&self, c: &CommitInfo) -> GraphCursor {
        GraphCursor {
            timestamp_ms: c.timestamp_ms,
            ordinal: self.info.ordinal,
            seq: c.seq,
        }
    }

    /// The newest own seq whose key is below `before` (every seq when `None`), or
    /// `None` when there is none. Commits the catalog no longer keeps are the oldest, so
    /// they count as below any cursor.
    fn top(&self, before: Option<GraphCursor>) -> Option<u64> {
        let head = self.info.head.seq;
        if head < self.low {
            return None;
        }
        let Some(cur) = before else {
            return Some(head);
        };
        let below = |s: u64| self.store.commit(s).is_none_or(|c| self.key(&c) < cur);
        if !below(self.low) {
            return None;
        }
        // the largest s in [low, head] with below(s): below is true, then false
        let (mut lo, mut hi) = (self.low, head);
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if below(mid) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Some(lo)
    }
}

impl Store {
    /// A page of the commit graph of the branches `o.branches` (every branch when
    /// `None`): their own commits, newest first by time, each with its first parent and
    /// a merge commit's merged commit, and each branch's starting commit and head.
    /// Commits the catalog no longer keeps are left out. A branch that is listed but
    /// cannot be read is left out too.
    pub fn commit_graph(&self, o: &CommitGraphOptions) -> Result<CommitGraph> {
        let set = self.owned_set()?;
        let limit = o.limit.max(1);
        let mut entries = set.readable_entries();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some(want) = &o.branches {
            for n in want {
                if n != MAIN && !entries.iter().any(|e| &e.0 == n) {
                    return Err(branch::no_such_branch(n));
                }
            }
        }
        let wanted = |n: &str| o.branches.as_ref().is_none_or(|w| w.iter().any(|x| x == n));
        let mut runs: Vec<Run<'_>> = Vec::new();
        if wanted(MAIN) {
            let created_ms = commit::read_dataset_created(self.root.as_deref())
                .unwrap_or_else(|| self.catalog.lock().first().map_or(0, |c| c.timestamp_ms));
            runs.push(Run {
                store: BranchStore::Main(self),
                info: GraphBranch {
                    name: MAIN.into(),
                    id: self.dataset_id,
                    ordinal: 0,
                    head: self.head_commit(),
                    from: None,
                    upstream: None,
                    created_ms,
                },
                low: 0,
            });
        }
        for (name, id, ordinal, from, created) in entries {
            if !wanted(&name) {
                continue;
            }
            let store = match self.branch_by_id(id) {
                Ok(s) => s,
                // deleted or damaged since the table was read
                Err(Error::Branch(_)) => continue,
                Err(e) => return Err(e),
            };
            let head = store.head_commit();
            runs.push(Run {
                info: GraphBranch {
                    name,
                    id,
                    ordinal,
                    head,
                    from: Some(set.named(from)),
                    upstream: set.name_of(from.branch_id),
                    created_ms: commit::parse_rfc3339_ms(&created).unwrap_or(0),
                },
                store,
                low: from.seq + 1,
            });
        }

        // up to `limit` commits of each run below the cursor, then the newest of them all
        let mut picked: Vec<(GraphCursor, usize, CommitInfo)> = Vec::new();
        let mut more = false;
        for (i, run) in runs.iter().enumerate() {
            let Some(top) = run.top(o.before) else {
                continue;
            };
            let mut s = top;
            let mut n = 0;
            while let Some(c) = run.store.commit(s) {
                if n == limit {
                    more = true;
                    break;
                }
                picked.push((run.key(&c), i, c));
                n += 1;
                if s == run.low {
                    break;
                }
                s -= 1;
            }
        }
        picked.sort_by_key(|p| std::cmp::Reverse(p.0));
        if picked.len() > limit {
            picked.truncate(limit);
            more = true;
        }
        let next = if more {
            picked.last().map(|p| p.0)
        } else {
            None
        };
        let commits = picked
            .into_iter()
            .map(|(_, i, c)| {
                let run = &runs[i];
                let id = run.info.id;
                let mut parents = Vec::new();
                if c.seq > run.low {
                    parents.push(NamedCommitRef {
                        branch: Some(run.info.name.clone()),
                        branch_id: id,
                        seq: c.seq - 1,
                    });
                } else if let Some(f) = &run.info.from {
                    let r = set.normalize(CommitRef {
                        branch_id: f.branch_id,
                        seq: f.seq,
                    });
                    parents.push(set.named(r));
                }
                let merged_from = run
                    .store
                    .merge_record(c.seq)
                    .map(|m| set.named(set.normalize(m.source)));
                if let Some(m) = &merged_from {
                    parents.push(m.clone());
                }
                GraphCommit {
                    annotation: run.store.annotation(c.seq),
                    commit: c,
                    branch: run.info.name.clone(),
                    branch_id: id,
                    parents,
                    merged_from,
                }
            })
            .collect();
        Ok(CommitGraph {
            branches: runs.into_iter().map(|r| r.info).collect(),
            commits,
            next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::branch::{BranchOptions, MergeOptions, MergeOutcome};

    fn update(s: &Store, text: &str) {
        crate::sparql::update::update(s, text, &Default::default()).unwrap();
    }

    #[test]
    fn cursors_read_back() {
        let c = GraphCursor {
            timestamp_ms: 1_759_000_000_123,
            ordinal: 3,
            seq: 42,
        };
        assert_eq!(c.to_string().parse::<GraphCursor>().unwrap(), c);
        for bad in ["", "1.2", "1.2.3.4", "a.1.2", "1.70000.2"] {
            assert!(bad.parse::<GraphCursor>().is_err(), "{bad}");
        }
    }

    #[test]
    fn pages_of_branches_with_forks_and_merges() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
        update(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
        s.create_branch("dev", &BranchOptions::default()).unwrap();
        let dev = s.branch("dev").unwrap();
        update(&dev, "INSERT DATA { <urn:b> <urn:p> 2 }");
        update(&dev, "INSERT DATA { <urn:c> <urn:p> 3 }");
        update(&s, "INSERT DATA { <urn:d> <urn:p> 4 }");
        let r = s.merge("dev", MAIN, &MergeOptions::default()).unwrap();
        assert!(matches!(r, MergeOutcome::Merged(_)));

        let all = s
            .commit_graph(&CommitGraphOptions {
                limit: 100,
                ..Default::default()
            })
            .unwrap();
        let names: Vec<_> = all.branches.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, [MAIN, "dev"]);
        assert_eq!(all.branches[1].from.as_ref().unwrap().seq, 1);
        assert_eq!(all.branches[1].upstream.as_deref(), Some(MAIN));
        let order: Vec<_> = all
            .commits
            .iter()
            .map(|c| (c.branch.as_str(), c.commit.seq))
            .collect();
        // main 0, 1, 2 (insert), 3 (merge); dev 2, 3: newest first
        assert_eq!(order.len(), 6, "{order:?}");
        assert_eq!(order[0], (MAIN, 3));
        assert_eq!(*order.last().unwrap(), (MAIN, 0));
        let merge = &all.commits[0];
        assert_eq!(merge.parents.len(), 2);
        assert_eq!(merge.parents[0].seq, 2);
        assert_eq!(merge.parents[1].branch.as_deref(), Some("dev"));
        assert_eq!(merge.parents[1].seq, 3);
        // dev's first own commit has main's commit 1 as its parent
        let first = all
            .commits
            .iter()
            .find(|c| c.branch == "dev" && c.commit.seq == 2)
            .unwrap();
        assert_eq!(first.parents[0].branch.as_deref(), Some(MAIN));
        assert_eq!(first.parents[0].seq, 1);
        assert!(all.next.is_none());

        // the same commits in pages of two
        let mut paged = Vec::new();
        let mut before = None;
        for _ in 0..10 {
            let p = s
                .commit_graph(&CommitGraphOptions {
                    before,
                    limit: 2,
                    ..Default::default()
                })
                .unwrap();
            assert!(p.commits.len() <= 2);
            paged.extend(p.commits.iter().map(|c| (c.branch.clone(), c.commit.seq)));
            match p.next {
                Some(n) => before = Some(n),
                None => break,
            }
        }
        let want: Vec<_> = order.iter().map(|(b, s)| (b.to_string(), *s)).collect();
        assert_eq!(paged, want);

        // one branch alone, and an unknown one
        let only = s
            .commit_graph(&CommitGraphOptions {
                branches: Some(vec!["dev".into()]),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(only.branches.len(), 1);
        assert_eq!(only.commits.len(), 2);
        let e = s
            .commit_graph(&CommitGraphOptions {
                branches: Some(vec!["nope".into()]),
                limit: 10,
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(&e, Error::Branch(b) if b.code == "no-such-branch"));
    }
}
