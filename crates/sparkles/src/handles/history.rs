//! Named snapshots and the commit history.

use crate::Dataset;
use crate::annotations::Annotation;
use crate::commit::{AnnotatedCommit, CommitInfo, CommitPage, CommitRange};
use crate::error::{Error, Result};
use crate::history::{At, HistoryStatus, NamedSnapshot, SnapshotOptions, TickReport};
use crate::store::{ChangePage, ChangesOptions, Diff, DiffOptions, HistoryQuery, HistoryResult};
use std::time::{Duration, Instant};

/// The dataset's named snapshots (from [`Dataset::snapshots`]).
#[derive(Clone)]
pub struct Snapshots {
    pub(crate) ds: Dataset,
}

impl Snapshots {
    /// Every named snapshot.
    pub fn list(&self) -> Vec<NamedSnapshot> {
        self.ds.store().snapshots()
    }

    /// The snapshot `name`, if there is one.
    pub fn get(&self, name: &str) -> Option<NamedSnapshot> {
        self.ds.store().named_snapshot(name)
    }

    /// Pin commit `at` under `name`. Returns the snapshot and whether it was created:
    /// `false` when `name` already pins that commit. A name that pins another commit
    /// fails with [`Error::Conflict`].
    pub fn create(
        &self,
        name: &str,
        at: &At,
        opts: &SnapshotOptions,
    ) -> Result<(NamedSnapshot, bool)> {
        self.ds.store().create_snapshot_opts(name, at, opts)
    }

    /// Remove the snapshot `name`; whether it existed.
    pub fn delete(&self, name: &str) -> Result<bool> {
        self.ds.store().delete_snapshot(name)
    }
}

/// A commit as `/$/commits/{ds}/{reference}` names it: `head`, a commit number, or a
/// commit IRI (`urn:x-sparkles:commit:{dataset id}:{seq}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitRef {
    Head,
    Seq(u64),
    Iri(String),
}

impl std::str::FromStr for CommitRef {
    type Err = Error;

    fn from_str(s: &str) -> Result<CommitRef> {
        if s == "head" {
            return Ok(CommitRef::Head);
        }
        if let Ok(n) = s.parse() {
            return Ok(CommitRef::Seq(n));
        }
        if crate::store::parse_commit_iri(s).is_some() {
            return Ok(CommitRef::Iri(s.to_string()));
        }
        Err(Error::invalid(format!("invalid commit reference '{s}'")))
    }
}

/// A commit with its annotation (message and change digest). Its serde form is the
/// commit's members plus `message` and `digest` when it has them.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CommitDetail {
    pub commit: CommitInfo,
    pub annotation: Option<Annotation>,
}

impl serde::Serialize for CommitDetail {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        AnnotatedCommit {
            commit: &self.commit,
            annotation: self.annotation.as_ref(),
        }
        .serialize(s)
    }
}

/// The dataset's commit history (from [`Dataset::history`]).
#[derive(Clone)]
pub struct History {
    pub(crate) ds: Dataset,
}

impl History {
    /// What history is kept: the reconstructable commits, the retention window, the
    /// catalog horizon and the cache.
    pub fn status(&self) -> HistoryStatus {
        self.ds.store().history()
    }

    /// One commit with its annotation. `Ok(None)` when there is no such commit (after
    /// the head, or an IRI of another dataset); [`Error::NotFound`] when its metadata is
    /// no longer retained.
    pub fn commit(&self, reference: &CommitRef) -> Result<Option<CommitDetail>> {
        let store = self.ds.store();
        let head = store.head_commit().seq;
        let seq = match reference {
            CommitRef::Head => head,
            CommitRef::Seq(n) => *n,
            CommitRef::Iri(iri) => match crate::store::parse_commit_iri(iri) {
                Some((id, n)) if id == store.dataset_id() => n,
                _ => return Ok(None),
            },
        };
        if seq > head {
            return Ok(None);
        }
        let commit = store.commit(seq).ok_or_else(|| {
            Error::NotFound(format!(
                "commit metadata before {} is no longer retained",
                seq + 1
            ))
        })?;
        Ok(Some(CommitDetail {
            commit,
            annotation: store.annotation(seq),
        }))
    }

    /// A page of commit metadata (newest first for [`CommitRange::Latest`] and
    /// [`CommitRange::Before`], oldest first for [`CommitRange::After`]).
    pub fn commits(&self, range: CommitRange, limit: usize) -> CommitPage {
        self.ds.store().commits(range, limit)
    }

    /// The quads added and removed between two states.
    pub fn diff(&self, from: &At, to: &At, opts: &DiffOptions) -> Result<Diff> {
        self.ds.store().diff(from, to, opts)
    }

    /// The change feed: the changes of the commits after `after`.
    pub fn changes(&self, after: u64, opts: &ChangesOptions) -> Result<ChangePage> {
        self.ds.store().changes(after, opts)
    }

    /// Block until a commit after `after` is published, and return the new head, or
    /// `None` once `timeout` has passed.
    pub fn wait_for_commit(&self, after: u64, timeout: Duration) -> Option<u64> {
        use std::future::Future;
        use std::task::{Context, Poll, Wake, Waker};
        struct Unpark(std::thread::Thread);
        impl Wake for Unpark {
            fn wake(self: std::sync::Arc<Self>) {
                self.0.unpark();
            }
        }
        let deadline = Instant::now() + timeout;
        let mut rx = self.ds.store().subscribe_commits();
        let waker = Waker::from(std::sync::Arc::new(Unpark(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        loop {
            let head = *rx.borrow_and_update();
            if head > after {
                return Some(head);
            }
            let changed = rx.changed();
            let mut changed = std::pin::pin!(changed);
            loop {
                match changed.as_mut().poll(&mut cx) {
                    Poll::Ready(Ok(())) => break,
                    Poll::Ready(Err(_)) => return None,
                    Poll::Pending => {
                        let now = Instant::now();
                        if now >= deadline {
                            return None;
                        }
                        std::thread::park_timeout(deadline - now);
                    }
                }
            }
        }
    }

    /// The history of matching quads: when each was added and removed.
    pub fn query(&self, q: &HistoryQuery) -> Result<HistoryResult> {
        self.ds.store().history_changes(q)
    }

    /// Drop the commit records that the catalog horizon no longer keeps; how many.
    pub fn prune(&self) -> Result<u64> {
        self.ds.store().prune_commits()
    }

    /// Take the named snapshots that the dataset's schedules make due, and expire old
    /// ones. The server calls this from its periodic loop; an embedder calls it when it
    /// likes.
    pub fn tick(&self) -> Result<TickReport> {
        self.ds.store().history_tick()
    }
}
