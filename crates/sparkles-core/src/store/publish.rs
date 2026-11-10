//! The shared tail of a commit: what every path does once the write that makes a commit
//! durable has returned (X16 phase 2).
//!
//! The serial path, the group leader and the bulk rebuild publish a [logical
//! commit](Publication::Commit), and a compaction publishes a [generation
//! switch](Publication::Switch). Each path still performs its own write and its own
//! synchronization before it calls the tail, so the tail moves no write. It publishes
//! nothing for a commit that a crash could lose, and `send_replace` follows
//! `current.store`, so a waiter never sees a commit number whose snapshot is not yet
//! current.
//!
//! The guard learns of a commit before the commit is published. A writer that waits for
//! the publication of commit N and then validates must check against the state that N
//! left, and only this order guarantees it. The compaction tap is not part of the tail:
//! the serial and group paths push it under the writer lock before their write, and a
//! tail that ran after the fence would change that contract.

use super::*;

/// What a path publishes through [`Store::publish_committed`]. It is only ever an
/// argument on the stack, so the size difference between its variants costs nothing.
#[allow(clippy::large_enum_variant)]
pub(super) enum Publication<'a> {
    /// A logical commit, which the serial, group and bulk paths publish once the write
    /// that makes it durable has returned.
    Commit(Committed<'a>),
    /// A generation switch, which a compaction publishes once `CURRENT` names the new
    /// generation: the commit the store already has, shown by `snapshot` of the new
    /// generation in place of `previous`. It appends no catalog entry, pushes no change
    /// log commit and announces no delta, and the guard translates its ids between
    /// `current.store` and `send_replace`.
    Switch {
        previous: &'a Snapshot,
        snapshot: Arc<Snapshot>,
    },
}

/// A logical commit for [`Store::publish_committed`].
pub(super) struct Committed<'a> {
    pub info: CommitInfo,
    /// the snapshot that shows the commit, with its index views maintained
    pub snapshot: Arc<Snapshot>,
    pub configuration: crate::history::ConfigurationBinding,
    /// where the commit's records end in its generation's log, when it wrote records
    /// (a bulk commit and a commit of an in-memory store write none)
    pub log_end: Option<u64>,
    /// the commit's changes, from which the view runtimes and the embedding worker
    /// read; a bulk commit has none and schedules that work itself
    pub changes: Option<&'a [(u8, [Id; 4])]>,
    /// what the change log records of the commit, when it is on
    pub change: Option<changelog::Pending>,
    /// the guard checked the commit and learns of its publication
    pub validated: bool,
}

impl Store {
    /// The change log's record of commit `c`, whose changes are `changes` in the ids of
    /// `generation`, when the log is on. The background writer turns the ids into keys.
    pub(super) fn change_of_log(
        &self,
        c: &CommitInfo,
        author: Option<Arc<str>>,
        message: Option<Arc<str>>,
        generation: &Arc<Generation>,
        changes: &[(u8, [Id; 4])],
    ) -> Option<changelog::Pending> {
        self.changelog
            .as_ref()
            .filter(|l| l.is_enabled())
            .map(|_| changelog::Pending {
                commit: changelog::ChangeCommit::of(c, author, message),
                body: changelog::PendingBody::Log {
                    generation: generation.clone(),
                    changes: changes.to_vec(),
                },
            })
    }

    /// Publish a commit whose durable write has returned (see the module documentation).
    /// Call with the writer lock held.
    pub(super) fn publish_committed(&self, p: Publication<'_>) {
        match p {
            Publication::Commit(c) => self.publish_commit(c),
            Publication::Switch { previous, snapshot } => {
                self.current.store(snapshot.clone());
                // the guard keeps ids of the old generation and translates them while the
                // writer lock is held, before any waiter learns of the switch
                if let Some(g) = self.guard() {
                    g.rekey(previous, &snapshot);
                }
                drop(snapshot);
                self.commits.send_replace(previous.commit);
            }
        }
    }

    fn publish_commit(&self, c: Committed<'_>) {
        let seq = c.info.seq;
        let generation = &c.snapshot.generation;
        if let Some(end) = c.log_end {
            self.wal_end.store(end, Ordering::Release);
            if let Some(ix) = generation.wal_index.lock().as_mut() {
                ix.note(wal::WalPoint {
                    seq,
                    offset: end,
                    folding: false,
                });
            }
        }
        if let Some(change) = c.change
            && let Some(log) = self.changelog.as_ref().filter(|l| l.is_enabled())
        {
            log.push(change);
        }
        self.catalog.lock().append(c.info);
        // An in-memory store keeps annotations and past states only for the window its
        // catalog keeps. A store with a log keeps them on disk, so it has neither step.
        if self.root.is_none() {
            self.forget_annotations();
        }
        // The guard learns of the commit before anyone can see it published, so a writer
        // that waited for the publication checks against the state this commit left.
        if c.validated {
            self.guard_committed(seq);
        }
        if self.root.is_none() {
            self.remember_past(seq);
        }
        // before the publication: a view runtime woken by it finds the changes
        if let Some(changes) = c.changes {
            self.deltas.noted(seq, generation, changes);
        }
        self.note_configuration(seq, c.configuration);
        self.current.store(c.snapshot.clone());
        self.commits.send_replace(seq);
        self.failpoint("commit-published");
        // after the publication: a worker that takes these pairs reads this state
        if let Some(changes) = c.changes {
            self.embed_noted(&c.snapshot, changes, seq, c.info.kind);
        }
    }
}
