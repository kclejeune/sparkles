//! Caller-led durable prefix publication for every persistent WAL commit (X16 Phase 6).
//! No collection delay: the first caller seals immediately; subsequent grouped callers
//! can append during that fence. A publisher never takes the writer mutex. General writer
//! operations drain the coordinator before inspecting the writer's private speculative
//! head. Solo commits (when `group_commit` is off or a commit falls back) drain the
//! coordinator and lead a group-of-one fence while holding the writer lock.
//!
//! A persistent WAL transaction does not write directly to the log. It encodes its
//! records, hands them to the coordinator with the commit and leaves the log's tail
//! ([`wal::Tail`]) to the leader of its fence, which writes the whole prefix's records
//! and terms in one go, through the write-through handles when they fit in preallocated
//! space (X09 item 446). A fence then makes its prefix durable with FUA writes and no
//! cache flush. When the prefix does not fit, or the file system refuses direct I/O, the
//! leader writes it with buffered writes and synchronizes the files with `fdatasync`.
//!
//! A fence has two stages. The leader writes and synchronizes its prefix (`syncing`), and
//! then publishes it (`publishing`), which applies its commits' full-text changes. The
//! next fence may seal and synchronize while the last one publishes, so index work never
//! delays a synchronization (X09 item 447), but it publishes only after the last one has
//! published, so publication keeps commit order.
use super::*;
use parking_lot::Condvar;
use std::collections::VecDeque;
use std::sync::atomic::AtomicU8;
use std::time::Duration;

pub(super) const MAX_BYTES: u64 = 4 << 20;
const MAX_COMMITS: usize = 64;
/// The most commits a WAL commit record can say may not be durable before it.
const MAX_UNFENCED: u64 = u8::MAX as u64;

#[derive(Default)]
pub(super) struct Ticket(AtomicU8);

#[cfg(feature = "text")]
pub(super) enum PendingText {
    /// Steady-state full-text changes staged before the fence and applied at publication.
    Staged(crate::text::Staged),
    /// Solo fallback during full-text recovery or online rebuild: maintain directly at
    /// publication under the writer lock.
    Maintain,
}

#[cfg(feature = "text")]
impl PendingText {
    fn bytes(&self) -> u64 {
        match self {
            PendingText::Staged(s) => s.bytes(),
            PendingText::Maintain => 0,
        }
    }
}

pub(super) struct Pending {
    pub info: CommitInfo,
    pub snapshot: Arc<Snapshot>,
    pub changes: Vec<(u8, [Id; 4])>,
    pub configuration: crate::history::ConfigurationBinding,
    _managed_admission: Option<super::managed::AdmittedManaged>,
    pub author: Option<Arc<str>>,
    /// a descriptor of the log, for the leader's buffered write and synchronization
    pub wal: Arc<File>,
    /// where the commit's records start in the log
    pub wal_start: u64,
    /// and where they end
    pub wal_end: u64,
    /// the commit's records, which the leader of its fence writes to the log
    pub data: Vec<u8>,
    /// the log's tail that the records are written at
    pub tail: Arc<wal::Tail>,
    /// the most the leader preallocates in the log after the records
    pub prealloc: u64,
    /// what the commit counts against the queue's limit: its records and the documents
    /// it staged for the full-text index
    pub bytes: u64,
    /// what the quota counts for the commit until its fence writes it: its records and
    /// its new terms, released when the commit is dropped unwritten
    pub reserved: quota::Reservation,
    pub ticket: Arc<Ticket>,
    /// what the write guard found, told to the guard once the commit is published
    pub validation: Option<Arc<crate::guard::ValidationSummary>>,
    /// the commit's message and change digest
    pub annotation: crate::annotations::Annotation,
    /// the commit's record in the annotations file, which the leader of its fence writes
    pub ann: Option<crate::annotations::Queued>,
    /// a merge record already durable in `merges.bin`, noted in memory at publication
    pub merge: Option<branching::MergeRec>,
    /// the new control keyspace state, published immediately after the snapshot (C26 §7.1)
    pub control: Option<crate::control::ControlState>,
    /// true when admitted to group commit; false for solo fences under the writer lock
    pub grouped: bool,
    /// true when this commit belongs to the Evidence managed-graph owner
    #[cfg(any(test, feature = "failpoints"))]
    pub evidence_commit: bool,
    /// the commit's full-text changes, applied to the live index at publication
    #[cfg(feature = "text")]
    pub text: Option<PendingText>,
    /// Retained through publication, refusal and destruction of pending resources.
    _family: Arc<family::FamilyPermit>,
}

/// A sealed prefix in one of the two stages of its fence.
#[derive(Default)]
struct Stage {
    /// the tickets of its commits not yet failed
    tickets: Vec<Arc<Ticket>>,
    /// its commits and bytes, still counted in the queue's totals
    commits: usize,
    bytes: u64,
    /// a failure took its commits back: the leader must not publish them
    revoked: bool,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Pending>,
    /// the prefix whose fence writes and synchronizes it
    syncing: Option<Stage>,
    /// the prefix that is synchronized and being published
    publishing: Option<Stage>,
    failed: bool,
    /// commits and bytes queued or in a stage
    commits: usize,
    bytes: u64,
}

impl State {
    /// A fence runs in either stage.
    fn busy(&self) -> bool {
        self.syncing.is_some() || self.publishing.is_some()
    }

    fn idle(&self) -> bool {
        !self.busy() && self.queue.is_empty() && !self.failed
    }
}

#[derive(Default)]
pub(super) struct Coordinator {
    state: Mutex<State>,
    changed: Condvar,
    pub(super) vocab: Mutex<crate::vocab::LazySync>,
    /// the next fence's synchronization fails, as a failed `fdatasync` would
    #[cfg(any(test, feature = "failpoints"))]
    fail_next_sync: AtomicBool,
    #[cfg(test)]
    fences: AtomicUsize,
    /// fences whose records went through the log's write-through handle
    #[cfg(test)]
    direct_fences: AtomicUsize,
    /// fences whose annotation records went through the annotations file's write-through
    /// handle
    #[cfg(test)]
    ann_direct_fences: AtomicUsize,
    /// times a waiter parked with its commit still pending
    #[cfg(test)]
    parks: AtomicUsize,
}

impl Coordinator {
    pub fn failed(&self) -> bool {
        self.state.lock().failed
    }

    /// No fence runs and nothing is queued, so a commit made now would be the only one
    /// in its group.
    pub fn idle(&self) -> bool {
        self.state.lock().idle()
    }

    /// Check for a healthy idle queue without waiting for a fence.
    pub(super) fn try_idle(&self) -> bool {
        self.state.try_lock().is_some_and(|state| state.idle())
    }

    /// Wait, without leading a fence, until no commit is in flight: no fence runs, and
    /// the queue is empty or failed. The caller holds the writer lock, so nothing new is
    /// queued, and the thread of each queued commit leads its own fence, so the wait
    /// ends however long the fences take. A background installer that holds no `Store`
    /// to drain with, such as the spatial index build, uses this before it installs a
    /// view, so that no grouped commit publishes an older view over it. `false` when
    /// `stall` passes with no commit leaving the group. The caller then releases the
    /// writer lock and tries again, since a commit whose thread panicked after queueing
    /// it waits for the next writer to lead it. The bound is on a stall rather than on
    /// the whole wait, because an installer that gave up after a fixed time and released
    /// the lock would let new commits in, and with fences longer than that time it would
    /// never install (review P2-2 of item 86).
    #[cfg_attr(not(feature = "geo"), allow(dead_code))]
    pub fn settle(&self, stall: Duration) -> bool {
        let mut s = self.state.lock();
        let mut left = s.commits;
        let mut deadline = std::time::Instant::now() + stall;
        loop {
            if !s.busy() && (s.failed || s.queue.is_empty()) {
                return true;
            }
            if s.commits < left {
                left = s.commits;
                deadline = std::time::Instant::now() + stall;
            }
            if self.changed.wait_until(&mut s, deadline).timed_out() && s.commits >= left {
                return !s.busy() && (s.failed || s.queue.is_empty());
            }
        }
    }

    /// Commits waiting in the queue, not yet sealed (for tests).
    #[cfg(test)]
    pub fn queued(&self) -> usize {
        self.state.lock().queue.len()
    }

    pub fn full(&self, bytes: u64) -> bool {
        let s = self.state.lock();
        s.commits >= MAX_COMMITS || s.bytes.saturating_add(bytes) > MAX_BYTES
    }

    pub fn enqueue(&self, p: Pending) -> Result<()> {
        let mut s = self.state.lock();
        if s.failed {
            return Err(Error::Poisoned);
        }
        s.commits += 1;
        s.bytes += p.bytes;
        s.queue.push_back(p);
        self.changed.notify_all();
        Ok(())
    }

    /// Atomically enqueue and seal a single commit on an idle coordinator. The caller
    /// holds the writer lock after draining or confirming `idle()`, so no fence runs and
    /// nothing is queued; sealing here guarantees this caller leads its own group of one
    /// without ever parking.
    pub fn enqueue_and_seal_idle(&self, p: Pending) -> Result<Vec<Pending>> {
        let mut s = self.state.lock();
        if s.failed {
            return Err(Error::Poisoned);
        }
        debug_assert!(
            !s.busy() && s.queue.is_empty(),
            "solo or no_wait seal on a non-idle coordinator"
        );
        s.commits += 1;
        s.bytes += p.bytes;
        s.syncing = Some(Stage {
            tickets: vec![p.ticket.clone()],
            commits: 1,
            bytes: p.bytes,
            revoked: false,
        });
        Ok(vec![p])
    }

    /// A later transaction failed after its commit number was taken. No fence runs again,
    /// so the queued commits fail. A prefix whose fence is already running keeps that
    /// fence's outcome: when the sync succeeds, its commits are durable and are
    /// acknowledged.
    pub fn fail(&self) {
        let mut s = self.state.lock();
        s.failed = true;
        Self::fail_queue(&mut s);
        self.changed.notify_all();
    }

    /// Take a stage's commits back: fail the tickets not yet acknowledged, and remove
    /// its commits from the totals.
    fn revoke(stage: &mut Stage, commits: &mut usize, bytes: &mut u64) {
        for ticket in stage.tickets.drain(..) {
            let _ = ticket
                .0
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        }
        *commits -= stage.commits;
        *bytes -= stage.bytes;
        stage.commits = 0;
        stage.bytes = 0;
        stage.revoked = true;
    }

    /// The prefix being synchronized cannot be published: its synchronization failed,
    /// its leader panicked, or the prefix before it was not published. That prefix and
    /// the queue fail, and no fence runs again. A prefix that is already synchronized and
    /// publishing is durable and keeps its outcome.
    fn fail_locked(s: &mut State) {
        s.failed = true;
        let State {
            syncing,
            commits,
            bytes,
            ..
        } = s;
        if let Some(stage) = syncing.as_mut() {
            Self::revoke(stage, commits, bytes);
        }
        Self::fail_queue(s);
    }

    /// The publishing leader panicked: the commits it had not published fail, and so does
    /// everything after them, since nothing may be published past the gap.
    fn fail_all(s: &mut State) {
        let State {
            publishing,
            commits,
            bytes,
            ..
        } = s;
        if let Some(stage) = publishing.as_mut() {
            Self::revoke(stage, commits, bytes);
        }
        Self::fail_locked(s);
    }

    fn fail_queue(s: &mut State) {
        for p in s.queue.drain(..) {
            let _ = p
                .ticket
                .0
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
            s.commits -= 1;
            s.bytes -= p.bytes;
        }
    }

    fn in_flight(s: &State, ticket: &Ticket) -> bool {
        [&s.syncing, &s.publishing]
            .into_iter()
            .flatten()
            .any(|st| st.tickets.iter().any(|t| std::ptr::eq(&**t, ticket)))
    }

    pub fn wait(&self, store: &Store, ticket: &Ticket) -> Result<()> {
        loop {
            match ticket.0.load(Ordering::Acquire) {
                1 => return Ok(()),
                2 => return Err(Error::Poisoned),
                _ => {}
            }
            store.failpoint("group-wait-observed");
            match self.drive(store, Some(ticket)) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(_) if ticket.0.load(Ordering::Acquire) == 1 => return Ok(()),
                Err(e) => return Err(e),
            }
            store.failpoint("group-wait-parking");
            // Park only while a fence is running. If the fence that kept `drive` from
            // leading finished after it returned, nobody leads the queued commit, so this
            // waiter must loop and lead it instead of sleeping out the poll.
            let mut s = self.state.lock();
            if ticket.0.load(Ordering::Acquire) == 0 && s.busy() {
                #[cfg(test)]
                self.parks.fetch_add(1, Ordering::Relaxed);
                self.changed.wait_for(&mut s, Duration::from_millis(20));
            }
        }
    }

    pub fn drain(&self, store: &Store, o: Option<&crate::guard::WriteOptions>) -> Result<()> {
        store.failpoint("group-draining");
        loop {
            if let Some(o) = o {
                o.check()?;
            }
            {
                let mut s = self.state.lock();
                if s.failed {
                    // A running fence can still publish its prefix. The caller resets
                    // the writer's head to the durable one, so it waits for that.
                    if !s.busy() {
                        return Err(Error::Poisoned);
                    }
                    self.changed.wait_for(&mut s, Duration::from_millis(20));
                    continue;
                }
                if !s.busy() && s.queue.is_empty() {
                    return Ok(());
                }
                if o.is_some_and(|o| o.no_wait) {
                    return Err(Error::WriterBusy);
                }
            }
            match self.drive(store, None) {
                Ok(true) => continue,
                Ok(false) => {}
                // The fence this caller led failed. A prefix before it may still be
                // publishing, and the caller, which may go on to install a view, must not
                // return before it has: the failed state above waits for it.
                Err(_) => continue,
            }
            let mut s = self.state.lock();
            if s.busy() {
                self.changed.wait_for(&mut s, Duration::from_millis(20));
            }
        }
    }

    /// Seal exactly the ready prefix, write and synchronize it, and publish it after the
    /// prefix before it. Appends during the fence belong to the next fence.
    fn drive(&self, store: &Store, ticket: Option<&Ticket>) -> Result<bool> {
        let batch = {
            let mut s = self.state.lock();
            // Completion and prefix selection share this mutex. An old waiter
            // must not become responsible for an unrelated later prefix.
            if ticket.is_some_and(|t| t.0.load(Ordering::Acquire) != 0) {
                return Ok(false);
            }
            if s.failed {
                // a sealed commit waits for its running fence, which may still succeed
                if ticket.is_some_and(|t| Self::in_flight(&s, t)) {
                    return Ok(false);
                }
                return Err(Error::Poisoned);
            }
            // A waiter whose commit is publishing must not lead the next prefix: if that
            // fence failed, the waiter would report the failure for a commit that its own
            // fence still publishes. The queued commits' own threads lead it.
            if s.syncing.is_some()
                || s.queue.is_empty()
                || ticket.is_some_and(|t| Self::in_flight(&s, t))
            {
                return Ok(false);
            }
            let batch: Vec<_> = s.queue.drain(..).collect();
            s.syncing = Some(Stage {
                tickets: batch.iter().map(|p| p.ticket.clone()).collect(),
                commits: batch.len(),
                bytes: batch.iter().map(|p| p.bytes).sum(),
                revoked: false,
            });
            batch
        };
        self.drive_sealed(store, batch, None)
    }

    /// Write, synchronize and publish an already-sealed prefix (`s.syncing`).
    ///
    /// When `solo_sync` is `Some`, this is a solo group-of-one fence running under the
    /// writer lock (when `group_commit` is off or a commit fell back): its `Direct` or
    /// `Buffered` sample is recorded into `solo_sync`, group histograms/failpoints are
    /// skipped, and a write failure returns its underlying error after poisoning.
    fn drive_sealed(
        &self,
        store: &Store,
        mut batch: Vec<Pending>,
        solo_sync: Option<&mut Option<(SyncPath, Duration)>>,
    ) -> Result<bool> {
        let grouped = batch[0].grouped;
        let has_control = batch.iter().any(|p| p.control.is_some());
        #[cfg(any(test, feature = "failpoints"))]
        let has_evidence = batch.iter().any(|p| p.evidence_commit);
        let mut leader = Leader {
            group: self,
            publishing: false,
            complete: false,
        };
        if grouped {
            store.failpoint("group-sealed");
        }
        let started = std::time::Instant::now();
        #[cfg(any(test, feature = "failpoints"))]
        let injected = self.fail_next_sync.swap(false, Ordering::AcqRel);
        #[cfg(not(any(test, feature = "failpoints")))]
        let injected = false;
        let synced = if injected {
            Err(std::io::Error::from_raw_os_error(5).into())
        } else {
            store.write_prefix(&batch, &mut self.vocab.lock(), solo_sync)
        };
        if grouped {
            store
                .commit_stats
                .synced(SyncPath::Group, started.elapsed());
            #[cfg(test)]
            {
                self.fences.fetch_add(1, Ordering::Relaxed);
                if matches!(synced, Ok(true)) {
                    self.direct_fences.fetch_add(1, Ordering::Relaxed);
                }
            }
            store.failpoint("group-synced");
        }
        match synced {
            // A later write may have failed during the fence. That poisons the
            // writer and fails the queue, but this prefix is durable all the same.
            //
            // The prefix is published outside the state mutex (X09 section 17.4),
            // because publication applies the commits' full-text changes and schedules
            // their embedding work, and `enqueue` takes the same mutex while its caller
            // holds the writer lock. Only one prefix publishes at a time, and a prefix
            // takes over publication only from the one before it, since the next fence
            // seals only once this one has left `syncing`. So publication keeps commit
            // order. Each ticket is set under the mutex, so a waiter that checked its
            // ticket before parking is woken, and an old waiter never leads an unrelated
            // prefix.
            Ok(_) => {
                if has_control {
                    store.failpoint("control-commit-synced");
                }
                #[cfg(any(test, feature = "failpoints"))]
                if has_evidence {
                    store.failpoint("evidence-wal-synced");
                }
                {
                    let mut s = self.state.lock();
                    while s.publishing.is_some() && !s.syncing.as_ref().is_some_and(|st| st.revoked)
                    {
                        self.changed.wait_for(&mut s, Duration::from_millis(20));
                    }
                    let stage = s.syncing.take().expect("this fence's stage");
                    if stage.revoked {
                        // The publisher of the prefix before this one failed. These
                        // commits are durable, but a reader must never see them without
                        // the commits before them, so they report the failure.
                        leader.complete = true;
                        self.changed.notify_all();
                        return Err(Error::Poisoned);
                    }
                    s.publishing = Some(stage);
                    leader.publishing = true;
                    // the next fence may seal and synchronize while this one publishes
                    self.changed.notify_all();
                }
                for p in &mut batch {
                    store.publish_group_prefix(p);
                    {
                        let _s = self.state.lock();
                        p.ticket.0.store(1, Ordering::Release);
                        self.changed.notify_all();
                    }
                    if grouped {
                        store.failpoint("group-published");
                    }
                }
                let mut s = self.state.lock();
                if grouped {
                    store.commit_stats.fenced(batch.len());
                }
                let stage = s.publishing.take().expect("this fence's stage");
                s.commits -= stage.commits;
                s.bytes -= stage.bytes;
                leader.complete = true;
                self.changed.notify_all();
                Ok(true)
            }
            // Every commit of a grouped fence reports the same failure, the one that led
            // it included. The cause is logged, since the error says only that the writer
            // is poisoned. A solo fence returns the underlying error to its sole caller
            // after poisoning the coordinator so subsequent writes receive Poisoned.
            Err(e) => {
                let mut s = self.state.lock();
                if grouped {
                    store.commit_stats.failed();
                }
                Self::fail_locked(&mut s);
                s.syncing = None;
                tracing::error!(
                    target: "sparkles::store",
                    error = %e,
                    commits = batch.len(),
                    first = batch[0].info.seq,
                    last = batch[batch.len() - 1].info.seq,
                    "commit fence failed to synchronize; writes are refused until the store is reopened"
                );
                leader.complete = true;
                self.changed.notify_all();
                if grouped {
                    Err(Error::Poisoned)
                } else {
                    Err(e)
                }
            }
        }
    }
}

struct Leader<'a> {
    group: &'a Coordinator,
    /// the fence's prefix left `syncing` for `publishing`
    publishing: bool,
    complete: bool,
}
impl Drop for Leader<'_> {
    fn drop(&mut self) {
        if !self.complete {
            let mut s = self.group.state.lock();
            if self.publishing {
                Coordinator::fail_all(&mut s);
                s.publishing = None;
            } else {
                Coordinator::fail_locked(&mut s);
                s.syncing = None;
            }
            self.group.changed.notify_all();
        }
    }
}

impl Store {
    /// Make the next group commit fence fail its synchronization (for tests).
    #[cfg(any(test, feature = "failpoints"))]
    #[doc(hidden)]
    pub fn fail_next_group_sync(&self) {
        if let Some(g) = &self.group {
            g.fail_next_sync.store(true, Ordering::Release);
        }
    }

    /// Write a sealed prefix's records and the terms they name, and make both durable: the
    /// I/O of a fence (X09 item 446). `Ok(true)` when the records went through the log's
    /// write-through handle.
    ///
    /// The records go to the log at its tail, which only this leader writes while the
    /// prefix is in flight, so they start where the last fence's write ended. When they fit
    /// in the space the log has preallocated, they go through the write-through handle, and
    /// the terms through the vocabulary's on its worker at the same time, as an ordinary
    /// commit's do in [`commit_direct`]. Both are `O_DSYNC` writes into allocated space,
    /// which a device with FUA persists without a cache flush. Otherwise the terms and then
    /// the records are written with buffered writes, and both files are synchronized with
    /// `fdatasync`, as [`sync_commit_reused`] does. Either way only the terms of the prefix
    /// are written, those before the vocabulary entry its last commit's snapshot ends at,
    /// and not the terms of a transaction that runs meanwhile, which could still roll them
    /// back (see [`DeltaVocab::sync_on`]).
    ///
    /// A crash during the write can leave any part of the prefix's records and terms on
    /// disk. None of its commits is acknowledged yet, and every commit record of the prefix
    /// counts the commits before it back to the published head as possibly not durable,
    /// so replay treats the damage as a torn tail, as it does for a buffered prefix whose
    /// pages reach the disk in any order.
    fn write_prefix(
        &self,
        batch: &[Pending],
        worker: &mut crate::vocab::LazySync,
        mut solo_sync: Option<&mut Option<(SyncPath, Duration)>>,
    ) -> Result<bool> {
        let (first, last) = (&batch[0], batch.last().expect("nonempty sealed prefix"));
        // the annotation records of the prefix's commits, in commit order
        let anns: Vec<&crate::annotations::Queued> =
            batch.iter().filter_map(|p| p.ann.as_ref()).collect();
        let generation = &last.snapshot.generation;
        let dvocab = &generation.dvocab;
        let upto = last.snapshot.dvocab_len;
        let tail = &last.tail;
        let mut io = tail.lock();
        let at = first.wal_start;
        // The prefix must start where the log's file ends and be contiguous. Anything else
        // would write over or past records that are not this prefix's.
        if io.end != at
            || batch.windows(2).any(|w| w[1].wal_start != w[0].wal_end)
            || batch.iter().any(|p| !Arc::ptr_eq(&p.tail, tail))
        {
            return Err(Error::Corrupt(format!(
                "a group commit fence would write at byte {at} of a log written up to byte {}",
                io.end
            )));
        }
        let joined;
        let data: &[u8] = if batch.len() == 1 {
            &first.data
        } else {
            joined = batch
                .iter()
                .flat_map(|p| p.data.iter().copied())
                .collect::<Vec<u8>>();
            &joined
        };
        let path = generation
            .dir
            .as_deref()
            .filter(|_| self.opts.wal_direct_writes)
            .map(|d| d.join("wal.log"));
        // The preallocation was allowed when the last commit queued, before the commits
        // still queued around this prefix were counted. The log and the vocabulary each
        // preallocate after the whole prefix, and so does the annotations file when the
        // prefix has records for it, so the reserve must hold that as well as every
        // queued commit's bytes (this prefix's among them), or none preallocates. The
        // fence promises the space before it checks, and a commit queued meanwhile counts
        // the promise, until the write below is done.
        let ann_prealloc = if anns.is_empty() {
            0
        } else {
            crate::annotations::MAX_PREALLOC
        };
        let promise = (last.prealloc > 0).then(|| {
            self.quota.promise(
                2 * last.prealloc.saturating_add(4096)
                    + if ann_prealloc > 0 {
                        ann_prealloc + 4096
                    } else {
                        0
                    },
            )
        });
        let promise = promise.filter(|_| self.check_disk(self.quota.unwritten()).is_ok());
        let prealloc = if promise.is_some() { last.prealloc } else { 0 };
        let ann_prealloc = if promise.is_some() { ann_prealloc } else { 0 };
        dvocab.set_prealloc(prealloc);
        self.failpoint("group-preallocating");
        let measures = self.quota.measures();
        let (alloc_before, vocab_before) = (io.alloc, dvocab.allocated());
        let mut log_write = || -> Result<bool> {
            if wal::DirectLog::fits(at, data.len(), io.alloc)
                && let Some(direct) = io.direct.handle(path.as_deref(), &last.wal)
            {
                let t = solo_sync.as_ref().map(|_| std::time::Instant::now());
                let direct_written = !(solo_sync.is_some()
                    && ordinary_sync_probe::skip_direct(self))
                    && commit_direct(direct, at, data, dvocab, worker, upto)?;
                if direct_written {
                    if let Some((slot, t)) = solo_sync.as_deref_mut().zip(t) {
                        *slot = Some((SyncPath::Direct, t.elapsed()));
                        ordinary_sync_probe::captured(self);
                    }
                    return Ok(true);
                }
                // the file system refused direct I/O: buffered from now on
                io.direct = wal::Direct::Off;
            }
            // the terms are in the file before the records that name them
            if dvocab.needs_sync_to(upto) {
                dvocab.flush_to(upto)?;
            }
            wal::write_commit(&last.wal, at, &mut io.alloc, data, prealloc)?;
            let t = solo_sync.as_ref().map(|_| std::time::Instant::now());
            let synced = sync_commit_reused(&last.wal, dvocab, worker, upto);
            if let Some((slot, t)) = solo_sync.as_deref_mut().zip(t) {
                *slot = Some((SyncPath::Buffered, t.elapsed()));
                ordinary_sync_probe::captured(self);
            }
            synced?;
            Ok(false)
        };
        // The prefix's annotation records are written at the same time as its records and
        // terms, through the annotations file's write-through handle when they fit in its
        // preallocated zeros (X09 item 501). The commits are acknowledged only once all
        // three writes are durable, and each commit record that expects an annotation
        // says so, so a crash between the writes leaves a torn tail (see `annotations`).
        let (written, ann_written) = if anns.is_empty() {
            (log_write(), None)
        } else {
            let write_anns = || {
                self.failpoint("group-annotations-writing");
                crate::annotations::write_queued(&anns, ann_prealloc)
            };
            std::thread::scope(|sc| {
                let ann = std::thread::Builder::new()
                    .name("annotations-write".into())
                    .spawn_scoped(sc, write_anns);
                let log = log_write();
                let ann = match ann {
                    Ok(h) => h.join().unwrap_or_else(|_| {
                        Err(std::io::Error::other("the annotations write panicked").into())
                    }),
                    Err(_) => write_anns(),
                };
                (log, Some(ann))
            })
        };
        tail.release(&io);
        // every change of the annotations file reaches the quota, a failed write's too
        let ann_prealloc = (!anns.is_empty()).then(|| anns[0].tail.take_prealloc());
        let failed = |_: &Error| {
            if let Some((was, now)) = ann_prealloc {
                self.quota.move_ann_preallocated(was, now);
                self.quota.invalidate();
            }
        };
        let direct = written.inspect_err(failed)?;
        let ann_written = ann_written.transpose().inspect_err(failed)?;
        #[cfg(test)]
        if first.grouped
            && ann_written.is_some_and(|w| w.direct)
            && let Some(g) = &self.group
        {
            g.ann_direct_fences.fetch_add(1, Ordering::Relaxed);
        }
        self.failpoint("group-written");
        io.end = last.wal_end;
        self.quota.written(
            batch.iter().map(|p| &p.reserved),
            (io.alloc - alloc_before)
                + dvocab.allocated().saturating_sub(vocab_before)
                + ann_written.map_or(0, |w| w.grew),
            io.alloc - io.end,
            dvocab.preallocated(),
            ann_prealloc,
        );
        // the growth is on disk now
        drop(promise);
        // A writer that checked its quota meanwhile may have walked the directory with
        // this growth in it, which the lines above counted again.
        if self.quota.measures() != measures {
            self.quota.remeasure();
        }
        Ok(direct)
    }

    /// Publish only a successfully fenced prefix, in commit order. The commit's full-text
    /// changes reach the live index first, so the published snapshot's view is resolved
    /// before any reader sees it, and its embedding work is scheduled after publication,
    /// so the worker reads the state that names its pairs (X09 section 17.4). The spatial
    /// view was computed when the commit was queued and is already in the snapshot.
    /// Grouped guard notifications run before publication.
    fn publish_group_prefix(&self, p: &mut Pending) {
        let c = p.info;
        #[cfg(feature = "text")]
        match p.text.take() {
            Some(PendingText::Staged(staged)) => {
                staged.apply(&p.snapshot);
            }
            Some(PendingText::Maintain) => {
                self.maintain_text(Arc::make_mut(&mut p.snapshot), &p.changes);
            }
            None => {}
        }
        if let Some(m) = p.merge
            && let Some(set) = self.branching.set()
        {
            set.note_merges(self.dataset_id, &[m], false);
        }
        let change = self.change_of_log(
            &c,
            p.author.clone(),
            p.annotation.message.clone(),
            &p.snapshot.generation,
            &p.changes,
        );
        self.compaction.committed(c.timestamp_ms);
        self.publish_committed(Publication::Commit(Committed {
            info: c,
            snapshot: p.snapshot.clone(),
            configuration: p.configuration,
            log_end: Some(p.wal_end),
            changes: Some(&p.changes),
            change,
            validated: p.validation.is_some(),
        }));
        // The control state becomes visible strictly after the snapshot (spec C26 §7.1).
        if let Some(state) = p.control.take() {
            self.control.store(Some(Arc::new(state)));
        }
    }
}

impl WriteTxn<'_> {
    /// Commits written before the next one that may not be durable yet. Only fenced
    /// commits are published, so the published head is durable.
    fn unfenced(&self, store: &Store) -> u64 {
        self.guard
            .head
            .seq
            .saturating_sub(store.current.load().commit)
    }

    pub(super) fn publish_grouped(
        &mut self,
        validation: Option<Arc<crate::guard::ValidationSummary>>,
        grouped: bool,
        sync: &mut Option<(SyncPath, Duration)>,
    ) -> Result<Receipt> {
        let store = self.store;
        let group = store
            .group
            .as_ref()
            .expect("persistent store has a commit coordinator");
        let message = match &self.opts.message {
            Some(m) => crate::annotations::validate_message(m)?,
            None => None,
        };
        let generation = self.base.generation.clone();
        let bytes = self.wal_bytes();
        // The commit's new terms stay in memory until its fence writes them with its
        // records, so the quota and the free-space reserve count them with the records
        // until the fence has written both. The commit reserves them before it checks,
        // so that the check of a racing fence's preallocation sees them, or this check
        // sees its promise (see `Quota::unwritten`).
        let terms = generation.dvocab.key_bytes(self.mark.entries());
        // So does the commit's annotation record, which its fence writes too.
        let ann_bytes = crate::annotations::record_len(message.as_deref(), store.commit_digests());
        let reserved = store.quota.reserve(bytes + terms + ann_bytes);
        self.check_storage_for(self.net_ins > 0, 0)?;
        let c = self.next_commit(validation.as_deref());
        // the control frame the commit carries, which names it (spec C26 §7.1)
        let control = self.control_frame(c.seq, c.timestamp_ms)?;
        let control_bytes = match &control {
            Some((frame, _)) => frame.encode()?,
            None => Vec::new(),
        };
        let mut snapshot = SnapshotBuilder::after(&self.base, c.seq)
            .delta(std::mem::take(&mut self.delta))
            .build();
        // The index state of the speculative snapshot (X09 section 17.4, step 1). The
        // spatial view is a value computed from the base's, which a failed fence drops
        // with the snapshot. The full-text changes are only staged here, and nothing of
        // either reaches the disk. They are computed before the commit is admitted to the
        // queue, so that the staged documents count in its limit. Solo fallbacks during
        // full-text recovery or online rebuild maintain the index directly at publication.
        store.maintain_geo(&mut snapshot, &self.base, &self.log);
        // A panic while staging poisons the writer, as a failed write does, so that the
        // index's state after it is never built on.
        #[cfg(feature = "text")]
        let text = if !grouped && store.text_needs_direct_maintain() {
            Some(PendingText::Maintain)
        } else {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                store.stage_text(&mut snapshot, &self.log)
            })) {
                Ok(text) => text.map(PendingText::Staged),
                Err(_) => {
                    tracing::error!(
                        target: "sparkles::store",
                        seq = c.seq,
                        "staging a grouped commit's full-text changes panicked; writes are refused until the store is reopened"
                    );
                    self.guard.poisoned = true;
                    group.fail();
                    return Err(Error::Poisoned);
                }
            }
        };
        // Staged documents hold their whole literals, so they count against the queue's
        // limit with the records (X09 item 447). A commit larger than the limit on its
        // own drains the queue and is queued alone.
        #[cfg(feature = "text")]
        let staged = text.as_ref().map_or(0, PendingText::bytes);
        #[cfg(not(feature = "text"))]
        let staged = 0;
        // The commit record counts the commits before it that may not be durable yet
        // in one byte, which a full queue keeps far below its limit.
        if grouped {
            let full = if self.idle_group {
                bytes + staged > MAX_BYTES
            } else {
                group.full(bytes + staged) || self.unfenced(store) >= MAX_UNFENCED
            };
            if full {
                self.drain_predecessors()?;
            }
        }
        // Publication only lowers the count, so reading it early stays conservative.
        let unfenced = if self.idle_group {
            0
        } else {
            u8::try_from(self.unfenced(store)).expect("drained above")
        };
        let prealloc = self.wal_prealloc();
        let owned_wal = self
            .guard
            .wal
            .as_ref()
            .expect("persistent WAL transaction")
            .clone();
        // Encode and validate the WAL records before mutating `merges.bin`,
        // `annotations.bin`, or writer offsets, so a fallible encoding error (such as an
        // invalid configuration binding) leaves the store untouched and unpoisoned.
        // The record says an annotation is expected when one will be queued, so replay
        // treats a flagged tail commit without its annotation as torn.
        let will_annotate = message.is_some() || store.commit_digests();
        let annotated = if will_annotate {
            commit::WAL_FLAG_ANNOTATED
        } else {
            0
        };
        let data = wal::encode_txn_with_control(
            &self.log,
            &control_bytes,
            self.configuration,
            self.guard.next_bnode,
            wal::Seal::of(&c, annotated, unfenced),
        )?;
        let data_len = data.len() as u64;
        if control_bytes.is_empty() {
            debug_assert_eq!(data_len, bytes, "the queued bytes are the records'");
        } else {
            debug_assert!(
                data_len <= bytes,
                "control_wal_bytes ({bytes}) underestimated encoded records ({data_len})"
            );
        }
        // A merge's record is durable before its commit (open drops one above the head).
        if let Some(m) = self.merge.as_mut() {
            m.seq = c.seq;
            let rec = *m;
            // Arm poison before touching `merges.bin`: a sync error or an unwind at
            // `merge-recorded` can leave a complete record for `c.seq` on disk, so no
            // later commit may reuse `c.seq` before reopen truncates it.
            self.guard.poisoned = true;
            if let Err(e) = store.branching.merges.lock().append(rec) {
                group.fail();
                return Err(e);
            }
            store.failpoint("merge-recorded");
        }
        // The annotation record is queued with the WAL records, and the fence writes both:
        // the commit record says an annotation is expected. Nothing after this takes the
        // commit back without poisoning the writer, and a failed fence poisons it too, so
        // the record is never taken back: if the commit is recovered, its annotation is
        // there.
        let (annotation, ann) =
            match store.annotate_queued(&c, message, || Some(self.change_lines())) {
                Ok(a) => a,
                Err(e) => {
                    if self.merge.is_some() {
                        self.guard.poisoned = true;
                        group.fail();
                    }
                    return Err(e);
                }
            };
        debug_assert_eq!(ann.is_some(), will_annotate);
        let family = self.guard.family.clone();
        let w = &mut **self.guard;
        // Every solo commit arms the writer poison flag before advancing `wal_len` or
        // driving its fence, and disarms it only after publication returns `Ok`, so an
        // unwind during the fence cannot leave `w.poisoned == false` with unpublished
        // offsets. In a store with a control keyspace, this also serves as the C26 §7.1
        // uncertainty guard before any byte reaches the log.
        let arm_control = !grouped && store.control_enabled();
        if !grouped {
            w.poisoned = true;
            if arm_control {
                store.failpoint("control-commit-armed");
            }
        }
        // The records are not written here: the leader of the commit's fence writes them
        // with the rest of its prefix (X09 item 446). From here on the commit holds its
        // number and its place in the log, and a failure poisons the writer.
        let wal_start = w.wal_len;
        w.wal_len += data_len;
        let snapshot = Arc::new(snapshot);
        // A compaction that is building carries this commit into its new generation. It
        // takes commits only after the writer lock's drain has made the queue durable, so
        // a commit that a failed fence drops never reaches the new generation: the drain
        // fails first, and the compaction ends without switching.
        if let Some(tap) = w.tap.as_mut() {
            if tap.active.load(Ordering::Acquire) {
                tap.commits.push(compaction::TapCommit {
                    info: c,
                    next_bnode: w.next_bnode,
                    changes: self.log.clone(),
                    configuration: self.configuration,
                });
            } else {
                w.tap = None;
            }
        }
        let ticket = Arc::new(Ticket::default());
        if grouped {
            w.staged = Some(snapshot.clone());
        }
        let pending = Pending {
            info: c,
            snapshot,
            changes: std::mem::take(&mut self.log),
            configuration: self.configuration,
            _managed_admission: self.managed_admission.take(),
            author: self.opts.author.clone(),
            wal: owned_wal,
            wal_start,
            wal_end: w.wal_len,
            data,
            tail: w.tail.clone(),
            prealloc,
            bytes: data_len + staged,
            reserved,
            ticket: ticket.clone(),
            validation: validation.clone(),
            annotation: annotation.clone(),
            ann,
            merge: self.merge,
            control: control.map(|(_, state)| state),
            grouped,
            #[cfg(any(test, feature = "failpoints"))]
            evidence_commit: self
                .managed
                .as_ref()
                .is_some_and(|cap| cap.owner() == crate::managed::OwnerKind::Evidence),
            #[cfg(feature = "text")]
            text,
            _family: family,
        };
        if !grouped {
            // Reconcile `w.head` to the published snapshot even if `drive_sealed`
            // unwinds after `publish_commit` stores the new snapshot (e.g. at
            // `commit-published` or `embed_noted`), while leaving `w.poisoned = true`
            // and keeping the previous head for unwinds before snapshot publication.
            struct SoloHead<'a> {
                store: &'a Store,
                w: &'a mut WriterState,
                committed: CommitInfo,
            }
            impl Drop for SoloHead<'_> {
                fn drop(&mut self) {
                    if self.store.current.load().commit == self.committed.seq {
                        self.w.head = self.committed;
                    }
                }
            }
            let batch = group.enqueue_and_seal_idle(pending)?;
            let solo_head = SoloHead {
                store,
                w,
                committed: c,
            };
            group.drive_sealed(store, batch, Some(sync))?;
            drop(solo_head);
            debug_assert_eq!(w.head.seq, c.seq);
            if arm_control {
                store.failpoint("control-commit-published");
            }
            w.poisoned = false;
        } else if self.opts.no_wait {
            w.head = c;
            // The coordinator was verified idle under the writer lock: seal our group of
            // one before releasing the writer lock so this caller leads immediately
            // without ever parking, while arriving writers can still enqueue for the next
            // fence during I/O.
            let batch = match group.enqueue_and_seal_idle(pending) {
                Ok(batch) => batch,
                Err(e) => {
                    w.poisoned = true;
                    return Err(e);
                }
            };
            drop(self.guard.0.take());
            let queued = std::time::Instant::now();
            group.drive_sealed(store, batch, None)?;
            store.commit_stats.waited(queued.elapsed());
        } else {
            w.head = c;
            if let Err(e) = group.enqueue(pending) {
                w.poisoned = true;
                return Err(e);
            }
            store.failpoint("group-enqueued-writer-held");
            // Nothing below accesses the writer guard. Waiting keeps Store borrowed,
            // but releases the single writer for later speculative transactions.
            drop(self.guard.0.take());
            store.failpoint("group-appended");
            let queued = std::time::Instant::now();
            group.wait(store, &ticket)?;
            store.commit_stats.waited(queued.elapsed());
        }
        Ok(Receipt {
            dataset_id: store.owner_dataset_id(),
            committed: true,
            commit: c,
            validation,
            annotation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn options() -> StoreOptions {
        StoreOptions {
            group_commit: true,
            ..Default::default()
        }
    }
    /// [`options`] without the change log, whose background appends grow the directory
    /// outside any fence, for tests that compare the quota's count with a walk.
    fn quota_options() -> StoreOptions {
        StoreOptions {
            change_log: false,
            ..options()
        }
    }
    /// A transaction of the kind that may begin from an unsynced group head.
    fn speculative(s: &Store) -> WriteTxn<'_> {
        s.write_with(CommitKind::Update, Default::default())
    }
    fn insert(s: &Store, name: &str) -> Result<Receipt> {
        insert_author(s, name, None)
    }
    fn insert_author(s: &Store, name: &str, author: Option<Arc<str>>) -> Result<Receipt> {
        let mut t = s.write_with(
            CommitKind::Update,
            crate::guard::WriteOptions {
                author,
                ..Default::default()
            },
        );
        let q = Quad::new(
            NamedNode::new(format!("urn:{name}")).unwrap(),
            NamedNode::new("urn:p").unwrap(),
            NamedNode::new("urn:o").unwrap(),
            GraphName::DefaultGraph,
        );
        let ids = t.encode_quad(&q, &mut Default::default())?;
        assert!(t.insert(ids)?);
        t.commit()
    }
    /// An insert that asks not to wait for the writer or for a fence.
    fn no_wait_insert(s: &Store, name: &str) -> Result<Receipt> {
        let mut t = s.try_write_with(
            CommitKind::Update,
            crate::guard::WriteOptions {
                no_wait: true,
                ..Default::default()
            },
        )?;
        let q = Quad::new(
            NamedNode::new(format!("urn:{name}")).unwrap(),
            NamedNode::new("urn:p").unwrap(),
            NamedNode::new("urn:o").unwrap(),
            GraphName::DefaultGraph,
        );
        let ids = t.encode_quad(&q, &mut Default::default())?;
        assert!(t.insert(ids)?);
        t.commit()
    }

    /// After a failed fence the writer is poisoned for good, so a write that will not wait
    /// is refused as poisoned, not as busy, which an embedder would retry (rereview N1).
    #[test]
    fn a_no_wait_write_on_a_failed_group_is_poisoned() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), options()).unwrap();
        insert(&s, "seed").unwrap();
        s.fail_next_group_sync();
        assert!(matches!(insert(&s, "lost"), Err(Error::Poisoned)));
        let r = no_wait_insert(&s, "after");
        assert!(matches!(r, Err(Error::Poisoned)), "{r:?}");
    }

    /// A write that will not wait commits directly as a group of one on an idle
    /// coordinator without ever parking, and is refused with `WriterBusy` behind a
    /// running fence (X16 Phase 6).
    #[test]
    fn a_no_wait_write_commits_on_an_idle_group_without_parking() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), options()).unwrap();
        insert(&s, "seed").unwrap();
        let seeded = s.snapshot().commit;
        let parks_before = s.group.as_ref().unwrap().parks.load(Ordering::Relaxed);
        let r = no_wait_insert(&s, "direct").unwrap();
        assert_eq!(r.commit.seq, seeded + 1);
        assert_eq!(s.snapshot().commit, seeded + 1);
        assert_eq!(s.snapshot().len(), 2);
        assert_eq!(
            s.group.as_ref().unwrap().parks.load(Ordering::Relaxed),
            parks_before,
            "an idle no_wait commit must not park"
        );
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 2);
        insert(&s, "later").unwrap();
        assert_eq!(s.snapshot().commit, seeded + 2);
    }

    /// A waiter whose fence completes between its own `drive` and its park must lead the
    /// next fence at once, not sleep out the 20 ms poll with nobody leading. The waiter is
    /// released only after that fence finished, so it must not park at all.
    #[test]
    fn a_waiter_does_not_sleep_through_a_finished_fence() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        insert(&s, "seed").unwrap();
        let (sealed, release_a) = pause_thread(&s, "group-sealed", "lw-a");
        let a = thread::Builder::new()
            .name("lw-a".into())
            .spawn({
                let s = s.clone();
                move || insert(&s, "a").unwrap()
            })
            .unwrap();
        sealed.recv_timeout(Duration::from_secs(5)).unwrap();
        let (parked, release_b) = pause_thread(&s, "group-wait-parking", "lw-b");
        let b = thread::Builder::new()
            .name("lw-b".into())
            .spawn({
                let s = s.clone();
                move || insert(&s, "b").unwrap()
            })
            .unwrap();
        parked.recv_timeout(Duration::from_secs(5)).unwrap();
        release_a.send(()).unwrap();
        a.join().unwrap();
        let parks = || s.group.as_ref().unwrap().parks.load(Ordering::Relaxed);
        let before = parks();
        release_b.send(()).unwrap();
        let receipt = b.join().unwrap();
        assert_eq!(receipt.commit.seq, s.snapshot().commit);
        assert_eq!(
            parks() - before,
            0,
            "the waiter parked with its commit queued and no fence running"
        );
    }

    /// A write that asked not to wait (the server's in-place updates) is refused with
    /// `WriterBusy` instead of parking in another commit's fence (review X09-1).
    #[test]
    fn a_no_wait_write_never_parks_in_a_fence() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        insert(&s, "seed").unwrap();
        let seeded = s.snapshot().commit;
        let (sealed, release) = pause_thread(&s, "group-sealed", "nw-a");
        let a = thread::Builder::new()
            .name("nw-a".into())
            .spawn({
                let s = s.clone();
                move || insert(&s, "a").unwrap()
            })
            .unwrap();
        sealed.recv_timeout(Duration::from_secs(5)).unwrap();
        let (tx, rx) = mpsc::channel();
        let b = thread::spawn({
            let s = s.clone();
            move || {
                let r = (|| {
                    let mut t = s.try_write_with(
                        CommitKind::Update,
                        crate::guard::WriteOptions {
                            no_wait: true,
                            ..Default::default()
                        },
                    )?;
                    let q = Quad::new(
                        NamedNode::new("urn:b").unwrap(),
                        NamedNode::new("urn:p").unwrap(),
                        NamedNode::new("urn:o").unwrap(),
                        GraphName::DefaultGraph,
                    );
                    let ids = t.encode_quad(&q, &mut Default::default())?;
                    t.insert(ids)?;
                    t.commit().map(|_| ())
                })();
                let _ = tx.send(r);
            }
        });
        let r = rx.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        a.join().unwrap();
        b.join().unwrap();
        assert!(
            matches!(r, Ok(Err(Error::WriterBusy))),
            "a no_wait write behind a running fence: {r:?}"
        );
        assert_eq!(s.snapshot().commit, seeded + 1);
    }

    fn pause(s: &Store, name: &'static str) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (hit, observed) = mpsc::channel();
        let (resume, go) = mpsc::channel();
        let go = Mutex::new(go);
        let once = AtomicBool::new(false);
        s.set_failpoint(
            name,
            Some(Arc::new(move |_| {
                if !once.swap(true, Ordering::Relaxed) {
                    hit.send(()).unwrap();
                    go.lock()
                        .recv_timeout(Duration::from_secs(10))
                        .expect("release paused fence");
                }
            })),
        );
        (observed, resume)
    }
    fn pause_thread(
        s: &Store,
        point: &'static str,
        thread_name: &'static str,
    ) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (hit, observed) = mpsc::channel();
        let (resume, go) = mpsc::channel();
        let go = Mutex::new(go);
        let once = AtomicBool::new(false);
        s.set_failpoint(
            point,
            Some(Arc::new(move |_| {
                if thread::current().name() == Some(thread_name)
                    && !once.swap(true, Ordering::Relaxed)
                {
                    hit.send(()).unwrap();
                    go.lock().recv_timeout(Duration::from_secs(10)).unwrap();
                }
            })),
        );
        (observed, resume)
    }
    fn queued(s: &Store, n: usize) {
        let until = Instant::now() + Duration::from_secs(5);
        while s.group.as_ref().unwrap().state.lock().queue.len() < n {
            assert!(Instant::now() < until, "transactions did not queue");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum AbortCheck {
        Plain,
        Discarded,
        Ignored,
        Foreign,
        Leading,
        Late,
        Core,
    }

    #[derive(Debug)]
    enum AbortEvent {
        Draining,
        Done,
    }

    fn patch_abort_case(check: AbortCheck, fail: bool, no_wait: bool, dry: bool) {
        use crate::guard::{Precondition, WriteOptions};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Arc::new(Store::open(&root, options()).unwrap());
        let prior = kind_insert(
            &s,
            CommitKind::Update,
            message_options("published"),
            in_graph("published", "urn:g"),
        )
        .unwrap();
        let expected = prior.commit.seq + 1;
        let (hit_tx, hit) = mpsc::channel();
        let (go_tx, go) = mpsc::channel();
        let go = Mutex::new(go);
        let first = AtomicBool::new(true);
        let expired = Arc::new(AtomicBool::new(false));
        let deadline = expired.clone();
        s.set_failpoint(
            "group-sealed",
            Some(Arc::new(move |_| {
                if first.swap(false, Ordering::SeqCst) {
                    let _ = hit_tx.send(());
                    if go.lock().recv_timeout(Duration::from_secs(10)).is_err() {
                        deadline.store(true, Ordering::SeqCst);
                    }
                }
            })),
        );
        let held = thread::spawn({
            let s = s.clone();
            move || {
                kind_insert(
                    &s,
                    CommitKind::Update,
                    message_options("unpublished"),
                    in_graph("unpublished", "urn:g"),
                )
            }
        });
        let (tx, events) = mpsc::channel();
        let notify = tx.clone();
        s.set_failpoint(
            "group-draining",
            Some(Arc::new(move |_| {
                if thread::current().name() == Some("patch-abort") {
                    let _ = notify.send(AbortEvent::Draining);
                }
            })),
        );
        let done = Arc::new(AtomicBool::new(false));
        let mut abort = None;
        let observed = (|| -> std::result::Result<_, String> {
            hit.recv_timeout(Duration::from_secs(5))
                .map_err(|e| format!("predecessor seal: {e}"))?;
            let own = format!("urn:uuid:{}#commit:{expected}", s.dataset_id());
            let text = match check {
                AbortCheck::Leading => format!("H prev <{own}> .\nTX .\nTA .\n"),
                AbortCheck::Late => format!("TX .\nH prev <{own}> .\nTA .\n"),
                AbortCheck::Ignored => "H prev <urn:foreign> .\nTX .\nTA .\n".into(),
                AbortCheck::Foreign => "H prev <urn:uuid:00000000-0000-0000-0000-000000000000#commit:999> .\nTX .\nTA .\n".into(),
                AbortCheck::Discarded => "TX .\nA <urn:discarded> <urn:p> <urn:o> .\nPA \"discarded\" <urn:discarded:> .\nTA .\n".into(),
                _ => "TX .\nTA .\n".into(),
            };
            let mut write = WriteOptions {
                no_wait,
                dry_run: dry.then(Default::default),
                ..Default::default()
            };
            if matches!(check, AbortCheck::Core) {
                write.precondition = Some(Precondition::new(move |head| {
                    if head.commit == expected {
                        Ok(())
                    } else {
                        Err(Error::PreconditionFailed(
                            "expected speculative head".into(),
                        ))
                    }
                }));
            }
            abort = Some(
                thread::Builder::new()
                    .name("patch-abort".into())
                    .spawn({
                        let s = s.clone();
                        let done = done.clone();
                        move || {
                            let result = s.apply_patch(
                                text.as_bytes(),
                                &PatchOptions {
                                    write,
                                    ..Default::default()
                                },
                            );
                            done.store(true, Ordering::SeqCst);
                            let _ = tx.send(AbortEvent::Done);
                            result
                        }
                    })
                    .map_err(|e| e.to_string())?,
            );
            let first = events
                .recv_timeout(Duration::from_secs(5))
                .map_err(|e| format!("abort progress: {e}"))?;
            if matches!(first, AbortEvent::Draining) && no_wait {
                let next = events
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|e| format!("no_wait result: {e}"))?;
                if !matches!(next, AbortEvent::Done) {
                    return Err(format!("unexpected event {next:?}"));
                }
            }
            Ok((first, done.load(Ordering::SeqCst), s.snapshot().commit))
        })();
        if fail {
            s.fail_next_group_sync();
        }
        let _ = go_tx.send(());
        s.set_failpoint("group-sealed", None);
        s.set_failpoint("group-draining", None);
        let held = held.join().unwrap();
        let result = abort.map(|w| w.join().unwrap());
        assert!(!expired.load(Ordering::SeqCst), "predecessor fence expired");
        let (first, completed, published) = observed.unwrap();
        eprintln!(
            "abort case {check:?} fail={fail} no_wait={no_wait} dry={dry}: observed {first:?}, published before={published}, published after={}, result={result:?}",
            s.snapshot().commit
        );
        assert_eq!(published, prior.commit.seq);
        let waits = dry
            || matches!(
                check,
                AbortCheck::Leading | AbortCheck::Late | AbortCheck::Core
            );
        if no_wait {
            // a write that will not wait is refused at once while a fence runs, before it
            // could start from the speculative head (review X09-1)
            assert!(
                matches!(first, AbortEvent::Done),
                "a no_wait abort must not drain: {check:?}, {first:?}"
            );
            assert!(completed);
        } else if waits {
            assert!(
                matches!(first, AbortEvent::Draining),
                "checked abort must wait: {check:?}, {first:?}"
            );
            assert!(!completed);
        } else {
            assert!(
                matches!(first, AbortEvent::Done),
                "plain abort must finish before release: {first:?}"
            );
            assert!(completed);
        }
        if fail {
            assert!(matches!(held, Err(Error::Poisoned)), "{held:?}");
        } else {
            assert_eq!(held.unwrap().commit.seq, expected);
        }
        let result = result.unwrap();
        if no_wait {
            assert!(matches!(result, Err(Error::WriterBusy)), "{result:?}");
        } else if waits && fail {
            assert!(matches!(result, Err(Error::Poisoned)), "{result:?}");
        } else {
            let out = result.unwrap();
            let seq = if waits { expected } else { prior.commit.seq };
            assert!(out.aborted);
            assert_eq!(
                out.rows,
                match check {
                    AbortCheck::Plain | AbortCheck::Core => 2,
                    AbortCheck::Discarded => 4,
                    _ => 3,
                }
            );
            assert!(!out.receipt.committed);
            assert_eq!(
                out.receipt.commit.seq, seq,
                "abort reports only published identity"
            );
            assert_eq!(out.receipt.commit, s.commit(seq).unwrap());
            assert_eq!(
                out.receipt.annotation.message.as_deref(),
                Some(if waits { "unpublished" } else { "published" })
            );
            assert_eq!(
                out.prev_checked,
                matches!(check, AbortCheck::Late) || (!dry && matches!(check, AbortCheck::Leading))
            );
            assert_eq!(
                (
                    out.inserted,
                    out.deleted,
                    out.prefixes_set,
                    out.prefixes_removed
                ),
                (0, 0, 0, 0)
            );
            assert_eq!(s.snapshot().len(), if fail { 1 } else { 2 });
            assert!(!s.prefixes().contains_key("discarded"));
            drop(s);
            let reopened = Store::open(&root, Default::default()).unwrap();
            assert_eq!(reopened.commit(seq), Some(out.receipt.commit));
            assert_eq!(
                reopened.annotation(seq).unwrap().message,
                out.receipt.annotation.message
            );
        }
    }

    macro_rules! abort_case {
        ($name:ident, $check:ident, $fail:expr, $no_wait:expr, $dry:expr) => {
            #[test]
            fn $name() {
                patch_abort_case(AbortCheck::$check, $fail, $no_wait, $dry);
            }
        };
    }
    abort_case!(
        plain_patch_abort_reports_published_success,
        Plain,
        false,
        false,
        false
    );
    abort_case!(
        plain_patch_abort_reports_published_failure,
        Plain,
        true,
        false,
        false
    );
    abort_case!(plain_patch_abort_no_wait_is_busy, Plain, false, true, false);
    abort_case!(
        plain_patch_abort_no_wait_is_busy_before_a_failed_fence,
        Plain,
        true,
        true,
        false
    );
    abort_case!(
        discarded_patch_abort_reports_published_success,
        Discarded,
        false,
        false,
        false
    );
    abort_case!(
        discarded_patch_abort_reports_published_failure,
        Discarded,
        true,
        false,
        false
    );
    abort_case!(
        ignored_prev_patch_abort_reports_published,
        Ignored,
        true,
        false,
        false
    );
    abort_case!(
        foreign_prev_patch_abort_reports_published,
        Foreign,
        true,
        false,
        false
    );
    abort_case!(
        leading_prev_patch_abort_waits_for_success,
        Leading,
        false,
        false,
        false
    );
    abort_case!(
        leading_prev_patch_abort_propagates_failure,
        Leading,
        true,
        false,
        false
    );
    abort_case!(
        leading_prev_patch_abort_no_wait_is_busy,
        Leading,
        false,
        true,
        false
    );
    abort_case!(
        late_prev_patch_abort_waits_for_success,
        Late,
        false,
        false,
        false
    );
    abort_case!(
        late_prev_patch_abort_propagates_failure,
        Late,
        true,
        false,
        false
    );
    abort_case!(
        late_prev_patch_abort_no_wait_is_busy,
        Late,
        false,
        true,
        false
    );
    abort_case!(
        precondition_patch_abort_waits_for_success,
        Core,
        false,
        false,
        false
    );
    abort_case!(
        precondition_patch_abort_propagates_failure,
        Core,
        true,
        false,
        false
    );
    abort_case!(
        precondition_patch_abort_no_wait_is_busy,
        Core,
        false,
        true,
        false
    );
    abort_case!(
        leading_dry_patch_abort_reports_unchecked_after_admission,
        Leading,
        false,
        false,
        true
    );
    abort_case!(
        late_dry_patch_abort_reports_checked_after_admission,
        Late,
        false,
        false,
        true
    );
    abort_case!(
        leading_dry_patch_abort_admission_propagates_failure,
        Leading,
        true,
        false,
        true
    );
    abort_case!(
        late_dry_patch_abort_admission_propagates_failure,
        Late,
        true,
        false,
        true
    );
    abort_case!(
        leading_dry_patch_abort_admission_no_wait_is_busy,
        Leading,
        false,
        true,
        true
    );
    abort_case!(
        late_dry_patch_abort_admission_no_wait_is_busy,
        Late,
        false,
        true,
        true
    );

    fn fresh_prefix_case(fail: bool) {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let prior = insert(&s, "prior").unwrap().commit.seq;
        let (hit, observed) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let resume = Mutex::new(resume);
        let once = AtomicBool::new(false);
        s.snapshot()
            .generation
            .dvocab
            .set_sync_hook(Arc::new(move || {
                if !once.swap(true, Ordering::Relaxed) {
                    hit.send(()).unwrap();
                    resume.lock().recv_timeout(Duration::from_secs(5)).unwrap();
                }
            }));
        if fail {
            s.snapshot().generation.dvocab.fail_next_sync();
        }
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "first-fresh"))
        };
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let (staged, seen) = mpsc::channel();
        s.set_failpoint(
            "group-appended",
            Some(Arc::new(move |_| {
                staged.send(()).unwrap();
            })),
        );
        let second = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "second-fresh"))
        };
        let progress = seen.recv_timeout(Duration::from_secs(1));
        assert_eq!(
            s.snapshot().commit,
            prior,
            "in-flight prefixes stay unpublished"
        );
        release.send(()).unwrap();
        let a = first.join().unwrap();
        let b = second.join().unwrap();
        assert!(
            progress.is_ok(),
            "next transaction could not mark/encode/append during vocabulary fence"
        );
        if fail {
            assert!(a.is_err() && b.is_err());
            assert_eq!(s.snapshot().commit, prior);
            assert!(insert(&s, "poisoned").is_err());
        } else {
            assert_eq!(a.unwrap().commit.seq, prior + 1);
            assert_eq!(b.unwrap().commit.seq, prior + 2);
            assert_eq!(s.snapshot().len(), 3);
        }
        drop(s);
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        assert!(reopened.snapshot().commit >= prior);
        if !fail {
            assert_eq!(reopened.snapshot().len(), 3);
        }
    }

    #[test]
    fn fresh_term_prefix_fence_allows_next_writer_to_stage() {
        fresh_prefix_case(false);
    }

    #[test]
    fn failed_fresh_term_prefix_poisoned_staged_suffix() {
        fresh_prefix_case(true);
    }

    /// A fence writes only its prefix's terms (X09 item 446). A transaction that starts
    /// from the speculative head marks the vocabulary before the fence has written the
    /// prefix's terms, so its mark places them after its rollback point. If the fence also
    /// wrote that transaction's own new term, its rollback would cut the chunk, the
    /// acknowledged commit's term with it, and a crash before the next fence would lose
    /// that commit. The image taken right after the rollback must hold the commit.
    #[test]
    fn a_rollback_after_a_fence_keeps_the_fenced_terms() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Arc::new(Store::open(&root, options()).unwrap());
        insert(&s, "prior").unwrap();
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "fenced"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        // the next transaction marks the vocabulary and adds a term of its own while the
        // fenced commit's term is still unwritten
        let mut t = speculative(&s);
        assert_eq!(
            t.base().commit,
            2,
            "the transaction starts from the queued commit"
        );
        t.encode_quad(
            &Quad::new(
                NamedNode::new("urn:rolled-back").unwrap(),
                NamedNode::new("urn:p").unwrap(),
                NamedNode::new("urn:o").unwrap(),
                GraphName::DefaultGraph,
            ),
            &mut Default::default(),
        )
        .unwrap();
        go.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap().commit.seq, 2);
        // the transaction is abandoned and its term rolled back, as a dry run or a failed
        // commit does
        let generation = t.base.generation.clone();
        generation.dvocab.rollback(&t.mark).unwrap();
        assert_eq!(
            generation.dvocab.len(),
            t.mark.entries(),
            "the rolled back term is gone"
        );
        drop(t);
        // power loss now: only what the fences and the rollback synchronized is on disk
        let crashed = dir.path().join("crashed");
        copy_dir(&root, &crashed);
        drop(s);
        let s = Store::open(&crashed, Default::default()).unwrap();
        let snap = s.snapshot();
        assert_eq!(snap.commit, 2, "the acknowledged commit survives");
        assert!(has(&snap, "prior") && has(&snap, "fenced"));
        assert!(snap.lookup_iri("urn:rolled-back").is_none());
    }

    #[test]
    fn aborted_uncommitted_suffix_does_not_clean_or_publish_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, observed) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let resume = Mutex::new(resume);
        let once = AtomicBool::new(false);
        s.snapshot()
            .generation
            .dvocab
            .set_sync_hook(Arc::new(move || {
                if !once.swap(true, Ordering::Relaxed) {
                    hit.send(()).unwrap();
                    resume.lock().recv_timeout(Duration::from_secs(5)).unwrap();
                }
            }));
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "acknowledged"))
        };
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done, seen) = mpsc::channel();
        let suffix = {
            let s = s.clone();
            thread::spawn(move || {
                let mut t = speculative(&s);
                t.encode_quad(
                    &Quad::new(
                        NamedNode::new("urn:aborted").unwrap(),
                        NamedNode::new("urn:p").unwrap(),
                        NamedNode::new("urn:o").unwrap(),
                        GraphName::DefaultGraph,
                    ),
                    &mut Default::default(),
                )
                .unwrap();
                drop(t);
                done.send(()).unwrap();
            })
        };
        let progress = seen.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
        suffix.join().unwrap();
        assert!(progress.is_ok());
        assert_eq!(s.snapshot().len(), 1);
        assert!(s.snapshot().generation.dvocab.needs_sync());
        insert(&s, "later").unwrap();
        assert!(!s.snapshot().generation.dvocab.needs_sync());
        drop(s);
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(reopened.snapshot().len(), 2);
        assert_eq!(reopened.snapshot().commit, 2);
    }

    #[test]
    fn singleton_is_immediate_and_default_memory_paths_remain_ordinary() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), options()).unwrap();
        assert_eq!(insert(&s, "one").unwrap().commit.seq, 1);
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 1);
        assert_eq!(s.snapshot().commit, 1);
        drop(s);
        // group commit is the default for a persistent store
        let s = Store::open(dir.path(), Default::default()).unwrap();
        assert!(s.group.is_some());
        drop(s);
        let s = Store::open(
            dir.path(),
            StoreOptions {
                group_commit: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!s.groups_commits());
        assert!(s.group.is_some());
        assert_eq!(s.snapshot().len(), 1);
        assert_eq!(insert(&s, "two").unwrap().commit.seq, 2);
        let memory = Store::in_memory(options());
        assert!(memory.group.is_none());
        assert_eq!(insert(&memory, "memory").unwrap().commit.seq, 1);
    }

    fn quad(name: &str) -> Quad {
        Quad::new(
            NamedNode::new(format!("urn:{name}")).unwrap(),
            NamedNode::new("urn:p").unwrap(),
            NamedNode::new("urn:o").unwrap(),
            GraphName::DefaultGraph,
        )
    }

    fn synced(s: &Store, path: SyncPath) -> u64 {
        s.commit_metrics().sync[path as usize].count()
    }

    #[test]
    fn idle_writes_skip_draining_and_keep_prepublication_errors() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), options()).unwrap();
        assert_eq!(insert(&s, "seed").unwrap().commit.seq, 1);
        let drains = Arc::new(AtomicUsize::new(0));
        s.set_failpoint(
            "group-draining",
            Some(Arc::new({
                let drains = drains.clone();
                move |_| {
                    drains.fetch_add(1, Ordering::Relaxed);
                }
            })),
        );

        let t = s.write();
        assert!(t.idle_group && t.guard.staged.is_none());
        assert_eq!(t.view().commit, 1);
        assert!(!t.commit().unwrap().committed);

        let mut t = s
            .try_write_with(
                CommitKind::Update,
                crate::guard::WriteOptions {
                    before_publish: Some(crate::guard::PublishHook::new(|| Err(Error::Cancelled))),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(t.idle_group);
        let ids = t
            .encode_quad(&quad("retry"), &mut Default::default())
            .unwrap();
        assert!(t.insert(ids).unwrap());
        assert!(matches!(t.commit(), Err(Error::Cancelled)));
        assert_eq!(s.snapshot().commit, 1);
        assert_eq!(insert(&s, "retry").unwrap().commit.seq, 2);
        assert_eq!(synced(&s, SyncPath::Group), 2);
        assert_eq!(drains.load(Ordering::Relaxed), 0);
        s.set_failpoint("group-draining", None);
    }

    #[test]
    fn the_only_writer_commits_as_a_group_of_one_and_a_contended_commit_groups() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(
            Store::open(
                dir.path(),
                StoreOptions {
                    group_commit: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        // alone, each commit leads its own group-of-one fence without parking
        for n in 0..3 {
            assert_eq!(insert(&s, &format!("alone{n}")).unwrap().commit.seq, n + 1);
        }
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 3);
        assert_eq!(s.group.as_ref().unwrap().parks.load(Ordering::Relaxed), 0);
        assert_eq!(synced(&s, SyncPath::Group), 3);
        assert_eq!(
            synced(&s, SyncPath::Direct) + synced(&s, SyncPath::Buffered),
            0
        );
        assert_eq!(s.commit_metrics().size.count(), 3);
        // a commit that finds a writer waiting for the lock groups with it
        let mut first = s.write_with(CommitKind::Update, Default::default());
        let ids = first
            .encode_quad(&quad("first"), &mut Default::default())
            .unwrap();
        first.insert(ids).unwrap();
        let second = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "second"))
        };
        let until = Instant::now() + Duration::from_secs(5);
        while s.writers_waiting() == 0 {
            assert!(Instant::now() < until, "the second writer did not wait");
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(first.commit().unwrap().commit.seq, 4);
        assert_eq!(second.join().unwrap().unwrap().commit.seq, 5);
        let fences = s.group.as_ref().unwrap().fences.load(Ordering::Relaxed);
        assert!((4..=5).contains(&fences), "{fences} fences");
        assert_eq!(synced(&s, SyncPath::Group), fences as u64);
        assert_eq!(s.commit_metrics().size.count(), fences as u64);
        assert!(s.commit_metrics().wait.count() >= 5);
        drop(s);
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(reopened.snapshot().len(), 5);
    }

    #[test]
    fn fallbacks_are_counted_by_the_first_failing_condition() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), options()).unwrap();
        insert(&s, "plain").unwrap();
        assert_eq!(s.commit_metrics().fallbacks, [0; Fallback::ALL.len()]);
        let mut t = s.write_with(CommitKind::Reason, Default::default());
        let ids = t
            .encode_quad(&quad("inferred"), &mut Default::default())
            .unwrap();
        t.insert(ids).unwrap();
        t.commit().unwrap();
        // a transaction without a net change is counted as a no-op
        let t = s.write_with(CommitKind::Update, Default::default());
        t.commit().unwrap();
        let m = s.commit_metrics();
        assert!(m.group_commit);
        assert_eq!(m.fallbacks[Fallback::Kind as usize], 1);
        assert_eq!(m.fallbacks[Fallback::Noop as usize], 1);
        assert_eq!(m.fallbacks.iter().sum::<u64>(), 2);
        // every reason has a producer in admission (review X09-4)
        assert_eq!(
            Fallback::ALL.map(Fallback::as_str),
            [
                "poisoned",
                "kind",
                "text-recovering",
                "text-rebuilding",
                "geo-unavailable",
                "bulk",
                "size",
                "noop"
            ]
        );
        // a store without group commit counts nothing
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(
            dir.path(),
            StoreOptions {
                group_commit: false,
                ..Default::default()
            },
        )
        .unwrap();
        insert(&s, "plain").unwrap();
        let m = s.commit_metrics();
        assert!(!m.group_commit);
        assert_eq!(m.fallbacks.iter().sum::<u64>(), 0);
        assert_eq!(m.sync[SyncPath::Group as usize].count(), 0);
    }

    #[test]
    fn prior_receipt_and_publication_do_not_wait_for_retained_next_writer() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-synced");
        let (done, received) = mpsc::channel();
        let first = {
            let s = s.clone();
            thread::spawn(move || done.send(insert(&s, "one")).unwrap())
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(s.snapshot().commit, 0, "synced but not yet published");
        let held = speculative(&s);
        assert!(!held.idle_group, "a synchronized fence is still active");
        assert_eq!(held.view().len(), 1, "private writer sees its predecessor");
        go.send(()).unwrap();
        let receipt = received
            .recv_timeout(Duration::from_secs(5))
            .expect("receipt while next writer held")
            .unwrap();
        assert_eq!(receipt.commit.seq, 1);
        assert_eq!(s.snapshot().commit, 1);
        drop(held);
        first.join().unwrap();
        let next = speculative(&s);
        assert!(next.idle_group);
        assert_eq!(next.view().commit, 1);
    }

    #[test]
    fn concurrent_prefixes_group_without_merging_receipts_and_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert_author(&s, "one", Some("one".into())).unwrap())
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let others: Vec<_> = ["two", "three", "four"]
            .into_iter()
            .map(|name| {
                let s = s.clone();
                thread::spawn(move || insert_author(&s, name, Some(name.into())).unwrap())
            })
            .collect();
        queued(&s, 3);
        assert_eq!(s.snapshot().len(), 0, "staged records are not public");
        go.send(()).unwrap();
        let mut receipts = vec![first.join().unwrap()];
        receipts.extend(others.into_iter().map(|t| t.join().unwrap()));
        receipts.sort_by_key(|r| r.commit.seq);
        assert_eq!(
            receipts.iter().map(|r| r.commit.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(
            receipts
                .iter()
                .all(|r| r.commit.inserted == 1 && r.committed)
        );
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 2);
        assert_eq!(s.snapshot().len(), 4);
        assert_eq!(s.head_commit().seq, 4);
        s.changelog.as_ref().unwrap().flush(true).unwrap();
        let history = s.history_changes(&Default::default()).unwrap();
        assert!(history.unrecorded.is_empty());
        for change in &history.changes {
            let NamedOrBlankNode::NamedNode(subject) = &change.quad.subject else {
                panic!("named fixture")
            };
            assert_eq!(
                change.commit.author.as_deref(),
                subject.as_str().strip_prefix("urn:")
            );
        }
        assert_eq!(
            history
                .changes
                .iter()
                .map(|ch| ch.commit.seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            s.snapshot_at(&crate::history::At::Commit(2), &Default::default())
                .unwrap()
                .0
                .len(),
            2
        );
        drop(s);
        let s = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(s.snapshot().commit, 4);
        assert_eq!(s.snapshot().len(), 4);
        assert_eq!(insert(&s, "five").unwrap().commit.seq, 5);
    }

    #[test]
    fn dependent_noop_waits_for_prefix_durability() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one").unwrap())
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let (done, received) = mpsc::channel();
        let noop = {
            let s = s.clone();
            thread::spawn(move || {
                let t = s.write();
                assert_eq!(t.view().len(), 1);
                done.send(t.commit()).unwrap();
            })
        };
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(s.snapshot().commit, 0);
        go.send(()).unwrap();
        let receipt = received
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(!receipt.committed);
        assert_eq!(receipt.commit.seq, 1);
        first.join().unwrap();
        noop.join().unwrap();
    }

    #[test]
    fn a_later_write_failure_keeps_the_running_fence_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "sealed"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let queued_commit = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "queued"))
        };
        queued(&s, 1);
        // A later transaction fails after taking its commit number, as one whose staging
        // panicked does. Writers no longer write to the log, so this is the failure a
        // writer can meet once its commit holds a place in the log.
        {
            let mut w = s.writer.lock();
            w.poisoned = true;
            s.group.as_ref().unwrap().fail();
        }
        go.send(()).unwrap();
        // The sealed prefix's fence succeeded, so its commit is durable and acknowledged.
        assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
        // The queued commit is never fenced.
        assert!(matches!(
            queued_commit.join().unwrap(),
            Err(Error::Poisoned)
        ));
        assert_eq!(s.snapshot().commit, 1);
        assert_eq!(s.head_commit().seq, 1);
        assert!(matches!(insert(&s, "later"), Err(Error::Poisoned)));
        drop(s);
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        assert!(reopened.snapshot().commit >= 1);
        assert!(has(&reopened.snapshot(), "sealed"));
    }

    #[test]
    fn explicit_transaction_never_sees_a_prefix_whose_fence_fails() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        s.snapshot().generation.dvocab.fail_next_sync();
        let (began, seen) = mpsc::channel();
        let reader = {
            let s = s.clone();
            thread::spawn(move || {
                // read, then dropped without committing
                let t = s.write();
                began.send(t.view().len()).unwrap();
            })
        };
        // The transaction waits for the pending fence before it exposes any state.
        assert!(seen.recv_timeout(Duration::from_millis(50)).is_err());
        go.send(()).unwrap();
        assert!(first.join().unwrap().is_err());
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
        reader.join().unwrap();
        assert_eq!(s.snapshot().commit, 0);
    }

    #[test]
    fn preview_and_capture_admission_drain_the_durable_prefix() {
        for capture in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = Arc::new(Store::open(dir.path(), options()).unwrap());
            let (hit, go) = pause(&s, "group-sealed");
            let first = {
                let s = s.clone();
                thread::spawn(move || insert(&s, "one").unwrap())
            };
            hit.recv_timeout(Duration::from_secs(5)).unwrap();
            let (done, received) = mpsc::channel();
            let next = {
                let s = s.clone();
                thread::spawn(move || {
                    if capture {
                        let c = s.backup_capture("group-test").unwrap();
                        let temp = tempfile::tempdir().unwrap();
                        let restored = temp.path().join("restored");
                        c.write_to(&restored).unwrap();
                        let reopened = Store::open(&restored, Default::default()).unwrap();
                        assert_eq!(reopened.snapshot().len(), 1);
                        assert_eq!(reopened.snapshot().commit, c.commit.seq);
                        done.send(c.commit.seq).unwrap();
                    } else {
                        // Switch to a preview after beginning from the staged head.
                        let t = speculative(&s);
                        assert_eq!(t.view().len(), 1);
                        let preview = t.preview(Default::default()).unwrap();
                        done.send(preview.head.seq).unwrap();
                    }
                })
            };
            assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
            go.send(()).unwrap();
            assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
            first.join().unwrap();
            next.join().unwrap();
            assert_eq!(s.snapshot().commit, 1);
        }
    }

    #[test]
    fn failed_fence_poisoned_suffix_never_publishes_or_acknowledges() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(no_wait_insert(&s, "busy"), Err(Error::WriterBusy)));
        let next = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "two"))
        };
        queued(&s, 1);
        s.snapshot().generation.dvocab.fail_next_sync();
        go.send(()).unwrap();
        assert!(first.join().unwrap().is_err());
        assert!(next.join().unwrap().is_err());
        assert_eq!(s.snapshot().commit, 0);
        assert_eq!(s.head_commit().seq, 0);
        assert!(matches!(insert(&s, "three"), Err(Error::Poisoned)));
        assert!(matches!(no_wait_insert(&s, "three"), Err(Error::Poisoned)));
        drop(s);
        // As with ordinary failed sync, complete but unacknowledged WAL records
        // may recover. Nothing that returned success can disappear. The failed fence
        // wrote the first commit's records and terms, and the second commit, queued
        // behind it, was never written: writers leave the log to the leaders.
        let s = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(s.snapshot().commit, 1);
        assert_eq!(s.snapshot().len(), 1);
    }

    #[test]
    fn concurrent_existing_and_fresh_terms_have_exact_receipts_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let s = s.clone();
                thread::spawn(move || {
                    let mut receipts = Vec::new();
                    for n in 0..25 {
                        let name = format!("worker{worker}-{n}");
                        receipts.push(insert(&s, &name).unwrap().commit.seq);
                        if n % 2 == 0 {
                            let mut t = s.write();
                            let view = t.view();
                            let q = [
                                view.lookup_iri(&format!("urn:{name}")).unwrap(),
                                view.lookup_iri("urn:p").unwrap(),
                                view.lookup_iri("urn:o").unwrap(),
                                Id::DEFAULT_GRAPH,
                            ];
                            assert!(t.delete(q).unwrap());
                            receipts.push(t.commit().unwrap().commit.seq);
                        }
                    }
                    receipts
                })
            })
            .collect();
        let mut receipts: Vec<_> = workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect();
        receipts.sort_unstable();
        assert_eq!(receipts, (1..=152).collect::<Vec<_>>());
        assert_eq!(s.snapshot().len(), 48);
        assert_eq!(s.snapshot().commit, 152);
        drop(s);
        let s = Store::open(dir.path(), options()).unwrap();
        assert_eq!(s.snapshot().commit, 152);
        assert_eq!(s.snapshot().len(), 48);
        s.compact().unwrap();
        assert_eq!(insert(&s, "after-compaction").unwrap().commit.seq, 153);
    }

    #[test]
    fn publisher_panic_fails_waiters_without_stranding_the_coordinator() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        s.set_failpoint(
            "group-synced",
            Some(Arc::new(|_| panic!("injected publisher failure"))),
        );
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let (done, received) = mpsc::channel();
        let next = {
            let s = s.clone();
            thread::spawn(move || done.send(insert(&s, "two")).unwrap())
        };
        queued(&s, 1);
        go.send(()).unwrap();
        assert!(first.join().is_err());
        assert!(matches!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err(Error::Poisoned)
        ));
        next.join().unwrap();
        assert_eq!(s.snapshot().commit, 0);
        assert_eq!(s.head_commit().seq, 0);
        assert!(matches!(insert(&s, "three"), Err(Error::Poisoned)));
    }
    /// Wait until `n` fences have finished their synchronization.
    fn fenced(s: &Store, n: usize) {
        let until = Instant::now() + Duration::from_secs(5);
        while s.group.as_ref().unwrap().fences.load(Ordering::Relaxed) < n {
            assert!(Instant::now() < until, "the fence did not synchronize");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// The next fence seals and synchronizes while a prefix publishes, so the index work
    /// of publication never delays a synchronization, and it publishes only after the
    /// prefix before it (X09 item 447).
    #[test]
    fn the_next_fence_synchronizes_while_a_prefix_publishes() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-published");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let second = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "two"))
        };
        // the first prefix is still publishing, and the second is synchronized
        fenced(&s, 2);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(
            s.snapshot().commit,
            1,
            "the second prefix publishes after the first"
        );
        go.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
        assert_eq!(second.join().unwrap().unwrap().commit.seq, 2);
        assert_eq!(s.snapshot().commit, 2);
        let g = s.group.as_ref().unwrap();
        assert!(g.idle());
        let st = g.state.lock();
        assert_eq!((st.commits, st.bytes), (0, 0));
    }

    /// A waiter whose commit is in the prefix being published never leads the next one. If
    /// it did and that fence failed, it would report the failure for a commit that its own
    /// prefix then publishes.
    #[test]
    fn a_waiter_whose_commit_publishes_never_leads_the_next_fence() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit0, go0) = pause(&s, "group-sealed");
        let zero = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "zero"))
        };
        hit0.recv_timeout(Duration::from_secs(5)).unwrap();
        let a_paused = Arc::new(AtomicBool::new(false));
        let y_go = Arc::new(AtomicBool::new(false));
        {
            let (a_paused, y_go) = (a_paused.clone(), y_go.clone());
            s.set_failpoint(
                "group-wait-observed",
                Some(Arc::new(move |_| {
                    let until = Instant::now() + Duration::from_secs(10);
                    let flag = match thread::current().name() {
                        // a2 leaves the next fence to a1, so a2's commit waits in it
                        Some("a2") => &a_paused,
                        Some("y") => &y_go,
                        _ => return,
                    };
                    while !flag.load(Ordering::SeqCst) && Instant::now() < until {
                        thread::sleep(Duration::from_millis(1));
                    }
                })),
            );
        }
        let named = |name: &'static str| {
            let s = s.clone();
            thread::Builder::new()
                .name(name.into())
                .spawn(move || insert(&s, name))
                .unwrap()
        };
        let a1 = named("a1");
        queued(&s, 1);
        let a2 = named("a2");
        queued(&s, 2);
        let (hit_tx, hit) = mpsc::channel();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        {
            let a_paused = a_paused.clone();
            let once = AtomicBool::new(false);
            s.set_failpoint(
                "group-published",
                Some(Arc::new(move |st: &Store| {
                    if st.snapshot().commit == 2 && !once.swap(true, Ordering::SeqCst) {
                        a_paused.store(true, Ordering::SeqCst);
                        hit_tx.send(()).unwrap();
                        let _ = go_rx.lock().recv_timeout(Duration::from_secs(10));
                    }
                })),
            );
        }
        go0.send(()).unwrap();
        assert_eq!(zero.join().unwrap().unwrap().commit.seq, 1);
        // a1 leads the prefix of a1 and a2 and stops after publishing a1
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        s.fail_next_group_sync();
        let y = named("y");
        queued(&s, 1);
        // a2 polls while its prefix publishes and must leave y's commit alone
        thread::sleep(Duration::from_millis(100));
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 2);
        y_go.store(true, Ordering::SeqCst);
        assert!(matches!(y.join().unwrap(), Err(Error::Poisoned)));
        go_tx.send(()).unwrap();
        assert_eq!(a1.join().unwrap().unwrap().commit.seq, 2);
        assert_eq!(a2.join().unwrap().unwrap().commit.seq, 3);
        assert_eq!(s.snapshot().commit, 3);
    }

    /// A caller that drains the group and leads a fence that fails returns only once the
    /// prefix before it has published, so that an installer that drains never installs a
    /// view that the earlier prefix's publication then replaces.
    #[test]
    fn a_failed_drain_waits_for_the_prefix_still_publishing() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-published");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        // a queued commit whose thread leaves the fence to the draining caller
        let y_go = Arc::new(AtomicBool::new(false));
        {
            let y_go = y_go.clone();
            s.set_failpoint(
                "group-wait-observed",
                Some(Arc::new(move |_| {
                    let until = Instant::now() + Duration::from_secs(10);
                    if thread::current().name() == Some("y") {
                        while !y_go.load(Ordering::SeqCst) && Instant::now() < until {
                            thread::sleep(Duration::from_millis(1));
                        }
                    }
                })),
            );
        }
        let y = {
            let s = s.clone();
            thread::Builder::new()
                .name("y".into())
                .spawn(move || insert(&s, "two"))
                .unwrap()
        };
        queued(&s, 1);
        s.fail_next_group_sync();
        let (done, finished) = mpsc::channel();
        let drainer = {
            let s = s.clone();
            thread::spawn(move || {
                let mut w = s.writer.lock();
                let r = s.drain_group(&mut w, None);
                done.send(()).unwrap();
                r
            })
        };
        // the drainer led the failed fence, and the first prefix still publishes
        let until = Instant::now() + Duration::from_secs(5);
        while s.group.as_ref().unwrap().fences.load(Ordering::Relaxed) < 2 {
            assert!(Instant::now() < until, "the drainer did not lead the fence");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            finished.recv_timeout(Duration::from_millis(200)).is_err(),
            "the drain returned while the first prefix was publishing"
        );
        go.send(()).unwrap();
        assert!(matches!(drainer.join().unwrap(), Err(Error::Poisoned)));
        assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
        y_go.store(true, Ordering::SeqCst);
        assert!(matches!(y.join().unwrap(), Err(Error::Poisoned)));
        assert_eq!(s.snapshot().commit, 1);
    }

    /// The quota counts a queued commit's new terms as well as its records, though its
    /// fence writes both later. Commits of long new literals queued behind a running fence
    /// reach the quota while the fence still runs.
    #[test]
    fn queued_terms_count_against_the_quota() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        insert(&s, "seed").unwrap();
        s.set_quota(Some(s.disk_usage() + (1 << 20))).unwrap();
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut waiting = Vec::new();
        let mut refused = None;
        for i in 0..8 {
            let (done, finished) = mpsc::channel();
            let t = {
                let s = s.clone();
                thread::spawn(move || {
                    let u = format!(
                        "INSERT DATA {{ <urn:long{i}> <urn:p> \"{}{i}\" }}",
                        "x".repeat(300 << 10)
                    );
                    let r = crate::sparql::update::update(&s, &u, &Default::default());
                    let _ = done.send(());
                    r.map(|_| ())
                })
            };
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if s.group.as_ref().unwrap().queued() > waiting.len() {
                    waiting.push(t);
                    break;
                }
                if finished.try_recv().is_ok() {
                    refused = Some(t.join().unwrap());
                    break;
                }
                assert!(
                    Instant::now() < until,
                    "commit {i} neither queued nor finished"
                );
                thread::sleep(Duration::from_millis(1));
            }
            if refused.is_some() {
                break;
            }
        }
        let refused = refused.expect("the quota never refused a queued commit's terms");
        assert!(
            matches!(&refused, Err(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::DatasetBytes),
            "{refused:?}"
        );
        assert!(
            waiting.len() < 4,
            "{} commits of 300 KiB fit in 1 MiB",
            waiting.len()
        );
        go.send(()).unwrap();
        first.join().unwrap().unwrap();
        for t in waiting {
            t.join().unwrap().unwrap();
        }
    }

    /// A directory walk for the quota during a fence sees the files the fence grew, and the
    /// fence then adds the same growth. The fence makes the next use of the quota walk the
    /// directory again, so the count is not left inflated.
    #[test]
    fn a_quota_walk_during_a_fence_is_not_counted_twice() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), quota_options()).unwrap());
        s.set_quota(Some(1 << 40)).unwrap();
        s.set_failpoint(
            "group-written",
            Some(Arc::new(|st: &Store| {
                st.quota.remeasure();
                st.quota.used();
            })),
        );
        // the first fence preallocates, so it grows the log by a step
        insert(&s, "one").unwrap();
        s.set_failpoint("group-written", None);
        let counted = s.quota.used();
        s.quota.remeasure();
        let measured = s.quota.used();
        assert_eq!(counted, measured, "the fence's growth was counted twice");
    }

    /// Commits queued behind a fence count against the free-space reserve, since their
    /// records and terms are not on disk yet.
    #[test]
    fn queued_commits_count_against_the_free_space_reserve() {
        const RESERVE: u64 = 1 << 20;
        let dir = tempfile::tempdir().unwrap();
        let _disk = crate::disk::fake::install(dir.path(), RESERVE + (8 << 20));
        let s = Arc::new(
            Store::open(
                dir.path(),
                StoreOptions {
                    min_free_disk_bytes: Some(RESERVE),
                    ..options()
                },
            )
            .unwrap(),
        );
        insert(&s, "seed").unwrap();
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut waiting = Vec::new();
        let mut refused = None;
        for i in 0..16 {
            let (done, finished) = mpsc::channel();
            let t = {
                let s = s.clone();
                thread::spawn(move || {
                    let u = format!(
                        "INSERT DATA {{ <urn:long{i}> <urn:p> \"{}{i}\" }}",
                        "x".repeat(1 << 20)
                    );
                    let r = crate::sparql::update::update(&s, &u, &Default::default());
                    let _ = done.send(());
                    r.map(|_| ())
                })
            };
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if s.group.as_ref().unwrap().queued() > waiting.len() {
                    waiting.push(t);
                    break;
                }
                if finished.try_recv().is_ok() {
                    refused = Some(t.join().unwrap());
                    break;
                }
                assert!(
                    Instant::now() < until,
                    "commit {i} neither queued nor finished"
                );
                thread::sleep(Duration::from_millis(1));
            }
            if refused.is_some() {
                break;
            }
        }
        let refused = refused.expect("the free-space reserve never refused a queued commit");
        assert!(
            matches!(&refused, Err(Error::StorageFull(_))),
            "{refused:?}"
        );
        assert!(
            waiting.len() < 8,
            "{} commits of 1 MiB fit in 8 MiB",
            waiting.len()
        );
        go.send(()).unwrap();
        first.join().unwrap().unwrap();
        for t in waiting {
            t.join().unwrap().unwrap();
        }
    }

    /// A fence preallocates after its whole prefix only when the free-space reserve holds
    /// the prefix and the preallocation, so the files it grows stay within the space the
    /// reserve leaves.
    #[test]
    fn a_fence_preallocates_only_within_the_free_space_reserve() {
        const STEP: u64 = 256 << 10;
        const HEADROOM: u64 = 320 << 10;
        let dir = tempfile::tempdir().unwrap();
        let opts = || StoreOptions {
            wal_prealloc_bytes: STEP,
            ..options()
        };
        let commit = |s: &Store, i: usize| {
            let quads: String = (0..300)
                .map(|j| format!("<urn:c{i}-{j}> <urn:p> <urn:o> . "))
                .collect();
            crate::sparql::update::update(
                s,
                &format!("INSERT DATA {{ {quads} }}"),
                &Default::default(),
            )
            .map(|_| ())
        };
        {
            // a log longer than the step, so the step is the whole preallocation
            let s = Store::open(dir.path(), opts()).unwrap();
            // the change log grows in the background, which no check counts
            s.set_change_log_settings(crate::store::ChangeLogSettings {
                enabled: Some(false),
                ..Default::default()
            })
            .unwrap();
            for i in 0..40 {
                commit(&s, i).unwrap();
            }
        }
        const RESERVE: u64 = 1 << 20;
        let held = crate::store::dir_size(dir.path());
        let _disk = crate::disk::fake::install(dir.path(), held + RESERVE + HEADROOM);
        let s = Arc::new(
            Store::open(
                dir.path(),
                StoreOptions {
                    min_free_disk_bytes: Some(RESERVE),
                    ..opts()
                },
            )
            .unwrap(),
        );
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || commit(&s, 100))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut waiting = Vec::new();
        for i in 0..16 {
            let t = {
                let s = s.clone();
                thread::spawn(move || commit(&s, 101 + i))
            };
            let until = Instant::now() + Duration::from_secs(5);
            while s.group.as_ref().unwrap().queued() <= waiting.len() {
                assert!(!t.is_finished(), "commit {i} was refused: {:?}", t.join());
                assert!(Instant::now() < until, "commit {i} never queued");
                thread::sleep(Duration::from_millis(1));
            }
            waiting.push(t);
        }
        let queued = s.quota.queued();
        assert!(
            queued + STEP > HEADROOM,
            "the queue of {queued} bytes leaves room for the step"
        );
        go.send(()).unwrap();
        first.join().unwrap().unwrap();
        for t in waiting {
            t.join().unwrap().unwrap();
        }
        let free = crate::disk::free_bytes(dir.path()).unwrap();
        // the catalog and the other small files grow by a few bytes a commit, which no
        // check counts, on the ordinary path as well
        assert!(
            free + (32 << 10) >= RESERVE,
            "the fences left {free} bytes free, under the reserve of {RESERVE}"
        );
    }

    /// A commit queued while a fence decides to preallocate counts the space the fence
    /// may preallocate, so the two together stay within the free-space reserve.
    #[test]
    fn a_commit_queued_during_a_fence_counts_its_preallocation() {
        const STEP: u64 = 256 << 10;
        const HEADROOM: u64 = 600 << 10;
        const RESERVE: u64 = 1 << 20;
        let dir = tempfile::tempdir().unwrap();
        let opts = || StoreOptions {
            wal_prealloc_bytes: STEP,
            ..options()
        };
        {
            // a log longer than the step, so the step is the whole preallocation
            let s = Store::open(dir.path(), opts()).unwrap();
            // the change log grows in the background, which no check counts
            s.set_change_log_settings(crate::store::ChangeLogSettings {
                enabled: Some(false),
                ..Default::default()
            })
            .unwrap();
            for i in 0..40 {
                let quads: String = (0..300)
                    .map(|j| format!("<urn:c{i}-{j}> <urn:p> <urn:o> . "))
                    .collect();
                crate::sparql::update::update(
                    &s,
                    &format!("INSERT DATA {{ {quads} }}"),
                    &Default::default(),
                )
                .unwrap();
            }
        }
        let held = crate::store::dir_size(dir.path());
        let _disk = crate::disk::fake::install(dir.path(), held + RESERVE + HEADROOM);
        let s = Arc::new(
            Store::open(
                dir.path(),
                StoreOptions {
                    min_free_disk_bytes: Some(RESERVE),
                    ..opts()
                },
            )
            .unwrap(),
        );
        let (hit, go) = pause(&s, "group-preallocating");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "first"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        // a commit of a long literal queues while the fence has decided
        let second = {
            let s = s.clone();
            thread::spawn(move || {
                let u = format!(
                    "INSERT DATA {{ <urn:long> <urn:p> \"{}\" }}",
                    "x".repeat(560 << 10)
                );
                crate::sparql::update::update(&s, &u, &Default::default()).map(|_| ())
            })
        };
        let until = Instant::now() + Duration::from_secs(5);
        while s.group.as_ref().unwrap().queued() == 0 && !second.is_finished() {
            assert!(
                Instant::now() < until,
                "the second commit neither queued nor ended"
            );
            thread::sleep(Duration::from_millis(1));
        }
        go.send(()).unwrap();
        first.join().unwrap().unwrap();
        let second = second.join().unwrap();
        assert!(
            matches!(&second, Ok(()) | Err(Error::StorageFull(_))),
            "{second:?}"
        );
        let free = crate::disk::free_bytes(dir.path()).unwrap();
        // the catalog and the other small files grow by a few bytes a commit, which no
        // check counts, on the ordinary path as well
        assert!(
            free + (32 << 10) >= RESERVE,
            "the fences left {free} bytes free, under the reserve of {RESERVE} ({second:?})"
        );
    }

    /// A walk that began before a fence read the walk count, and read the directory
    /// after the fence grew it, also makes the fence measure again.
    #[test]
    fn a_quota_walk_running_across_a_fence_is_not_counted_twice() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), quota_options()).unwrap());
        s.set_quota(Some(1 << 40)).unwrap();
        let (hit, go) = pause(&s, "group-sealed");
        let fenced = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        // a walk starts while the fence is sealed and reads the directory only once the
        // fence has written
        let (walking_tx, walking) = mpsc::channel();
        let (grown_tx, grown) = mpsc::channel::<()>();
        s.quota.on_next_walk(move || {
            walking_tx.send(()).unwrap();
            let _ = grown.recv_timeout(Duration::from_secs(10));
        });
        let walker = {
            let s = s.clone();
            thread::spawn(move || {
                s.quota.remeasure();
                s.quota.used()
            })
        };
        walking.recv_timeout(Duration::from_secs(5)).unwrap();
        let grown_tx = Mutex::new(Some(grown_tx));
        s.set_failpoint(
            "group-written",
            Some(Arc::new(move |_| {
                grown_tx.lock().take();
            })),
        );
        go.send(()).unwrap();
        fenced.join().unwrap().unwrap();
        walker.join().unwrap();
        s.set_failpoint("group-sealed", None);
        s.set_failpoint("group-written", None);
        let counted = s.quota.used();
        s.quota.remeasure();
        let measured = s.quota.used();
        assert_eq!(counted, measured, "the fence's growth was counted twice");
    }

    /// A fence releases its commits' quota reservations as it counts their growth, in one
    /// step, with the growth and preallocation of the annotations file that it wrote too,
    /// so no writer's check counts the prefix twice or not at all.
    #[test]
    fn a_fence_releases_its_reservation_with_its_growth() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), quota_options()).unwrap());
        insert(&s, "seed").unwrap();
        let (seen_tx, seen) = mpsc::channel();
        s.set_failpoint(
            "group-synced",
            Some(Arc::new(move |st: &Store| {
                let until = Instant::now() + Duration::from_secs(2);
                while st.quota.queued() > 0 && Instant::now() < until {
                    thread::sleep(Duration::from_millis(1));
                }
                let counted = st.quota.used();
                st.quota.remeasure();
                let _ = seen_tx.send((st.quota.queued(), counted, st.quota.used()));
            })),
        );
        let mut t = s.write_with(
            CommitKind::Update,
            crate::guard::WriteOptions {
                message: Some("annotated".into()),
                ..Default::default()
            },
        );
        let ids = t
            .encode_quad(
                &Quad::new(
                    NamedNode::new("urn:annotated").unwrap(),
                    NamedNode::new("urn:p").unwrap(),
                    oxrdf::Literal::new_simple_literal("a new term"),
                    GraphName::DefaultGraph,
                ),
                &mut Default::default(),
            )
            .unwrap();
        t.insert(ids).unwrap();
        t.commit().unwrap();
        let (queued, counted, measured) = seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(queued, 0, "the reservation outlived the fence's write");
        assert_eq!(counted, measured, "the prefix was counted twice");
    }

    /// Commits that a failed fence discards, queued behind it or in it, release their
    /// quota reservations.
    #[test]
    fn a_failed_fence_releases_the_reservations_it_discards() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        insert(&s, "seed").unwrap();
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let later: Vec<_> = ["two", "three"]
            .into_iter()
            .map(|n| {
                let s = s.clone();
                thread::spawn(move || insert(&s, n))
            })
            .collect();
        queued(&s, 2);
        assert!(s.quota.queued() > 0);
        s.fail_next_group_sync();
        go.send(()).unwrap();
        assert!(first.join().unwrap().is_err());
        for t in later {
            assert!(t.join().unwrap().is_err());
        }
        assert_eq!(s.quota.queued(), 0);
    }

    /// A publisher that panics fails the prefix synchronized behind it: those commits are
    /// durable, but a reader must not see them without the commits before them.
    #[test]
    fn a_failed_publisher_revokes_the_next_synchronized_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit_tx, hit) = mpsc::channel();
        let (go_tx, go) = mpsc::channel::<()>();
        let go = Mutex::new(go);
        let once = AtomicBool::new(false);
        s.set_failpoint(
            "group-published",
            Some(Arc::new(move |_| {
                if !once.swap(true, Ordering::Relaxed) {
                    hit_tx.send(()).unwrap();
                    let _ = go.lock().recv_timeout(Duration::from_secs(10));
                    panic!("injected publisher failure");
                }
            })),
        );
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let second = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "two"))
        };
        fenced(&s, 2);
        go_tx.send(()).unwrap();
        assert!(first.join().is_err());
        assert!(matches!(second.join().unwrap(), Err(Error::Poisoned)));
        assert_eq!(s.snapshot().commit, 1);
        assert!(matches!(insert(&s, "three"), Err(Error::Poisoned)));
        drop(s);
        // both prefixes were synchronized, so both are durable
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(reopened.snapshot().commit, 2);
        assert!(has(&reopened.snapshot(), "two"));
    }

    #[test]
    fn cancelled_capture_waiting_for_a_prefix_releases_its_writer() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one").unwrap())
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let (entered, admitted) = mpsc::channel();
        s.set_failpoint(
            "group-draining",
            Some(Arc::new(move |_| {
                let _ = entered.send(());
            })),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let opts = crate::guard::WriteOptions {
            cancel: Some(cancel.clone()),
            ..Default::default()
        };
        let (done, received) = mpsc::channel();
        let capture = {
            let s = s.clone();
            thread::spawn(move || {
                done.send(s.backup_capture_with("cancelled", &opts).map(|_| ()))
                    .unwrap()
            })
        };
        admitted.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            received.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err(Error::Cancelled)
        ));
        capture.join().unwrap();
        assert_eq!(s.snapshot().commit, 0);
        // Cancellation did not retain the writer or damage the speculative head.
        let held = speculative(&s);
        assert_eq!(held.view().len(), 1);
        go.send(()).unwrap();
        first.join().unwrap();
        drop(held);
        assert_eq!(insert(&s, "two").unwrap().commit.seq, 2);
    }

    #[cfg(unix)]
    #[test]
    fn failed_wal_fence_preserves_the_acknowledged_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let second = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "two"))
        };
        queued(&s, 1);
        // Inject an actual fdatasync error for only the second sealed prefix.
        // Its original records still exist in the real WAL; they cannot be acked
        // merely because the first fence may happen to cover a later append.
        // The fence writes through the log's tail, so the write-through handle is turned
        // off to make it write and synchronize through this descriptor.
        s.group.as_ref().unwrap().state.lock().queue[0].wal =
            Arc::new(File::open("/dev/null").unwrap());
        s.writer.lock().tail.lock().direct = wal::Direct::Off;
        go.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
        assert!(second.join().unwrap().is_err());
        assert_eq!(s.snapshot().commit, 1);
        assert_eq!(s.snapshot().len(), 1);
        assert_eq!(s.head_commit().seq, 1);
        assert!(matches!(insert(&s, "three"), Err(Error::Poisoned)));
        drop(s);
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        assert!(reopened.snapshot().commit >= 1);
        assert!(
            reopened
                .snapshot()
                .contains(&[
                    reopened.snapshot().lookup_iri("urn:one").unwrap(),
                    reopened.snapshot().lookup_iri("urn:p").unwrap(),
                    reopened.snapshot().lookup_iri("urn:o").unwrap(),
                    Id::DEFAULT_GRAPH
                ])
                .unwrap()
        );
    }

    fn precondition_on(commit: u64, seen: Arc<AtomicBool>) -> crate::guard::WriteOptions {
        crate::guard::WriteOptions {
            precondition: Some(crate::guard::Precondition::new(move |snap| {
                seen.store(true, Ordering::Relaxed);
                if snap.commit == commit {
                    Ok(())
                } else {
                    Err(Error::PreconditionFailed(format!(
                        "head is {}",
                        snap.commit
                    )))
                }
            })),
            ..Default::default()
        }
    }

    #[test]
    fn a_precondition_that_holds_for_the_speculative_head_joins_the_group() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one").unwrap())
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        // commit 1 is written but not durable: the precondition names it as the head
        let seen = Arc::new(AtomicBool::new(false));
        let next = {
            let s = s.clone();
            let opts = precondition_on(1, seen.clone());
            thread::spawn(move || {
                kind_insert(&s, CommitKind::Update, opts, in_graph("two", "urn:g"))
            })
        };
        queued(&s, 1);
        assert!(seen.load(Ordering::Relaxed));
        assert_eq!(
            s.snapshot().commit,
            0,
            "nothing is published before the fence"
        );
        go.send(()).unwrap();
        assert_eq!(next.join().unwrap().unwrap().commit.seq, 2);
        first.join().unwrap();
        assert_eq!(s.snapshot().commit, 2);
        assert_eq!(s.commit_metrics().fallbacks.iter().sum::<u64>(), 0);
    }

    #[test]
    fn a_precondition_refusal_waits_for_the_durable_head() {
        for fence_fails in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = Arc::new(Store::open(dir.path(), options()).unwrap());
            let (hit, go) = pause(&s, "group-sealed");
            let first = {
                let s = s.clone();
                thread::spawn(move || insert(&s, "one"))
            };
            hit.recv_timeout(Duration::from_secs(5)).unwrap();
            // the tag matches the durable head (0) but not the speculative one (1)
            let (done, received) = mpsc::channel();
            let next = {
                let s = s.clone();
                let opts = precondition_on(0, Default::default());
                thread::spawn(move || {
                    done.send(s.try_write_with(CommitKind::Update, opts).map(|_| ()))
                        .unwrap()
                })
            };
            // the refusal is not reported while the head it was judged against may
            // still be lost
            assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
            if fence_fails {
                s.fail_next_group_sync();
            }
            go.send(()).unwrap();
            let answer = received.recv_timeout(Duration::from_secs(5)).unwrap();
            if fence_fails {
                assert!(matches!(answer, Err(Error::Poisoned)), "{answer:?}");
                assert!(first.join().unwrap().is_err());
            } else {
                assert!(
                    matches!(answer, Err(Error::PreconditionFailed(_))),
                    "{answer:?}"
                );
                assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
            }
            next.join().unwrap();
        }
    }

    #[test]
    fn validation_bypass_never_bypasses_a_write_precondition() {
        for enabled in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = Store::open(
                dir.path(),
                StoreOptions {
                    group_commit: enabled,
                    ..Default::default()
                },
            )
            .unwrap();
            let opts = crate::guard::WriteOptions {
                bypass_validation: true,
                precondition: Some(crate::guard::Precondition::new(|_| {
                    Err(Error::PreconditionFailed("stale head".into()))
                })),
                ..Default::default()
            };
            let error = s
                .try_write_with(CommitKind::Update, opts)
                .map(|_| ())
                .unwrap_err();
            assert!(matches!(error, Error::PreconditionFailed(_)));
            assert_eq!(s.snapshot().commit, 0);
            assert_eq!(s.snapshot().len(), 0);
        }
    }
    #[test]
    fn bounded_queue_drains_without_blocking_a_prior_receipt_on_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "first").unwrap())
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let queued_writers: Vec<_> = (1..MAX_COMMITS)
            .map(|n| {
                let s = s.clone();
                thread::spawn(move || insert(&s, &format!("queued-{n}")).unwrap())
            })
            .collect();
        queued(&s, MAX_COMMITS - 1);
        let (entered, admitted) = mpsc::channel();
        s.set_failpoint(
            "group-draining",
            Some(Arc::new(move |_| {
                let _ = entered.send(());
            })),
        );
        let last = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "last").unwrap())
        };
        admitted.recv_timeout(Duration::from_secs(5)).unwrap();
        {
            let state = s.group.as_ref().unwrap().state.lock();
            assert_eq!(state.commits, MAX_COMMITS);
            assert_eq!(state.queue.len(), MAX_COMMITS - 1);
            assert!(state.bytes <= MAX_BYTES);
        }
        // The last writer waits for backpressure while retaining its lock; the
        // earlier publisher must remain independent of that writer admission.
        go.send(()).unwrap();
        let mut receipts = vec![first.join().unwrap().commit.seq];
        receipts.extend(
            queued_writers
                .into_iter()
                .map(|w| w.join().unwrap().commit.seq),
        );
        receipts.push(last.join().unwrap().commit.seq);
        receipts.sort_unstable();
        assert_eq!(receipts, (1..=65).collect::<Vec<_>>());
        assert_eq!(s.snapshot().len(), 65);
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 3);
        assert_eq!(s.group.as_ref().unwrap().state.lock().commits, 0);
    }
    #[cfg(unix)]
    #[test]
    fn completed_receipt_never_drives_or_inherits_an_unrelated_suffix_failure() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        assert_eq!(insert(&s, "one").unwrap().commit.seq, 1);
        let (observed, resume) = pause_thread(&s, "group-wait-observed", "old-waiter");
        let old = {
            let s = s.clone();
            thread::Builder::new()
                .name("old-waiter".into())
                .spawn(move || insert(&s, "two"))
                .unwrap()
        };
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            insert(&s, "three").unwrap().commit.seq,
            3,
            "another caller fences the old ticket"
        );
        let (appended, run_suffix) = pause_thread(&s, "group-appended", "unrelated-suffix");
        let suffix = {
            let s = s.clone();
            thread::Builder::new()
                .name("unrelated-suffix".into())
                .spawn(move || insert(&s, "four"))
                .unwrap()
        };
        appended.recv_timeout(Duration::from_secs(5)).unwrap();
        // The fence writes through the log's tail, so the write-through handle is turned
        // off to make it write and synchronize through this descriptor.
        s.group.as_ref().unwrap().state.lock().queue[0].wal =
            Arc::new(File::open("/dev/null").unwrap());
        s.writer.lock().tail.lock().direct = wal::Direct::Off;
        resume.send(()).unwrap();
        assert_eq!(old.join().unwrap().unwrap().commit.seq, 2);
        assert_eq!(
            s.snapshot().commit,
            3,
            "old waiter did not drive the unrelated prefix"
        );
        run_suffix.send(()).unwrap();
        assert!(suffix.join().unwrap().is_err());
        assert_eq!(s.snapshot().commit, 3);
    }

    #[test]
    fn published_success_is_terminal_when_the_rest_of_a_prefix_panics() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        assert_eq!(insert(&s, "one").unwrap().commit.seq, 1);
        let (observed, resume) = pause_thread(&s, "group-wait-observed", "published-ticket");
        let old = {
            let s = s.clone();
            thread::Builder::new()
                .name("published-ticket".into())
                .spawn(move || insert(&s, "two"))
                .unwrap()
        };
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        s.set_failpoint(
            "group-published",
            Some(Arc::new(|s| {
                if s.current.load().commit == 2 {
                    panic!("unwind after first prefix entry is published")
                }
            })),
        );
        let rest = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "three"))
        };
        assert!(rest.join().is_err());
        resume.send(()).unwrap();
        assert_eq!(old.join().unwrap().unwrap().commit.seq, 2);
        assert_eq!(s.snapshot().commit, 2);
        assert_eq!(s.head_commit().seq, 2);
        assert!(matches!(insert(&s, "four"), Err(Error::Poisoned)));
    }
    #[test]
    fn process_interruption_preserves_every_acknowledged_prefix() {
        const ROOT: &str = "SPARKLES_GROUP_INTERRUPTION_TEST_ROOT";
        const POINT: &str = "SPARKLES_GROUP_INTERRUPTION_TEST_POINT";
        if let Ok(root) = std::env::var(ROOT) {
            let s = Store::open(Path::new(&root), options()).unwrap();
            assert_eq!(insert(&s, "acknowledged").unwrap().commit.seq, 1);
            let point = match std::env::var(POINT).unwrap().as_str() {
                "appended" => "group-appended",
                "sealed" => "group-sealed",
                "synced" => "group-synced",
                "published" => "group-published",
                _ => panic!("unknown test checkpoint"),
            };
            s.set_failpoint(point, Some(Arc::new(|_| std::process::exit(77))));
            insert(&s, "unacknowledged").unwrap();
            panic!("interruption checkpoint was not reached");
        }
        for point in ["appended", "sealed", "synced", "published"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("db");
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "store::group::tests::process_interruption_preserves_every_acknowledged_prefix",
                    "--nocapture",
                ])
                .env(ROOT, &root)
                .env(POINT, point)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let until = Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() > until {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("child did not reach {point}")
                }
                thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(status.code(), Some(77));
            let s = Store::open(&root, Default::default()).unwrap();
            assert!((1..=2).contains(&s.snapshot().commit), "{point}");
            let snap = s.snapshot();
            let q = [
                snap.lookup_iri("urn:acknowledged").unwrap(),
                snap.lookup_iri("urn:p").unwrap(),
                snap.lookup_iri("urn:o").unwrap(),
                Id::DEFAULT_GRAPH,
            ];
            assert!(
                snap.contains(&q).unwrap(),
                "acked commit must survive {point}"
            );
            assert_eq!(snap.len(), snap.commit);
            if matches!(point, "synced" | "published") {
                assert_eq!(snap.commit, 2)
            }
            // Process exit is not a power-loss simulation: complete unacked
            // suffixes may recover, but no acknowledged prefix may disappear.
        }
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn wal_path(root: &Path) -> PathBuf {
        let cur = std::fs::read_to_string(root.join("CURRENT")).unwrap();
        root.join(cur.trim()).join("wal.log")
    }

    /// The record range of each transaction in a WAL, its commit record included.
    fn transactions(buf: &[u8]) -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        let mut start = 0;
        for (i, rec) in buf.as_chunks::<WAL_REC>().0.iter().enumerate() {
            if i == 0 && rec[0] == WAL_HEADER {
                start = 1;
                continue;
            }
            if rec[0] == WAL_COMMIT {
                out.push(start..i + 1);
                start = i + 1;
            }
        }
        out
    }

    #[test]
    fn damage_to_the_first_transaction_preserves_the_log_header() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let store = Store::open(&root, options()).unwrap();
        insert(&store, "one").unwrap();
        insert(&store, "two").unwrap();
        drop(store);
        let mut bad = std::fs::read(wal_path(&root)).unwrap();
        assert_eq!(&bad[..WAL_REC], &wal::header_record());
        let txns = transactions(&bad);
        assert_eq!(txns.len(), 2);
        let first = &txns[0];
        assert_eq!(first.start, 1);
        bad[first.start * WAL_REC + 5] ^= 0x55;
        assert_eq!(&bad[..WAL_REC], &wal::header_record());
        std::fs::write(wal_path(&root), &bad).unwrap();
        assert_eq!(wal_check(&root), crate::check::Status::Error);
        let error = Store::open(&root, Default::default()).err().unwrap();
        assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
        assert!(error.to_string().contains("checksum mismatch"), "{error}");
    }

    fn metadata_fixture() -> (tempfile::TempDir, PathBuf, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let store = Store::open(&root, options()).unwrap();
        for name in ["one", "two", "three", "four"] {
            kind_insert(
                &store,
                CommitKind::Update,
                message_options(name),
                in_graph(name, "urn:g"),
            )
            .unwrap();
        }
        drop(store);
        let buf = std::fs::read(wal_path(&root)).unwrap();
        (dir, root, buf)
    }

    fn reseal_transaction(buf: &mut [u8], txn: &std::ops::Range<usize>, unfenced: u8) {
        let at = (txn.end - 1) * WAL_REC;
        let data = &buf[txn.start * WAL_REC..at];
        let mut rec: [u8; WAL_REC] = buf[at..at + WAL_REC].try_into().unwrap();
        let Some(Ok((seq, ts, kind, flags))) = commit::open_wal_commit(&rec, data) else {
            panic!("fixture commit must have verified metadata");
        };
        commit::seal_wal_commit_unfenced(&mut rec, seq, ts, kind, flags, unfenced, data);
        buf[at..at + WAL_REC].copy_from_slice(&rec);
    }

    fn zero_commit_metadata(buf: &mut [u8], txn: &std::ops::Range<usize>) {
        let at = (txn.end - 1) * WAL_REC;
        buf[at + 1..at + WAL_REC].fill(0);
        assert_eq!(buf[at], WAL_COMMIT);
    }

    fn torn_metadata_case(hole: bool, later_proof: bool) {
        let (dir, root, mut buf) = metadata_fixture();
        let txns = transactions(&buf);
        let damaged = 0usize;
        let prefix = 0u64;
        let partial = usize::from(hole);
        for (i, txn) in txns.iter().enumerate() {
            reseal_transaction(&mut buf, txn, i as u8);
        }
        if hole {
            buf[txns[damaged].start * WAL_REC..txns[damaged].end * WAL_REC].fill(0);
        }
        zero_commit_metadata(&mut buf, &txns[partial]);
        let end = if later_proof {
            reseal_transaction(&mut buf, &txns[partial + 1], 0);
            txns[partial + 1].end
        } else {
            txns[partial].end
        };
        buf.truncate(end * WAL_REC);
        let case = dir.path().join("case");
        copy_dir(&root, &case);
        std::fs::write(wal_path(&case), &buf).unwrap();
        if later_proof {
            assert_eq!(wal_check(&case), crate::check::Status::Error);
            assert!(matches!(
                Store::open(&case, Default::default()),
                Err(Error::Corrupt(_))
            ));
            return;
        }
        assert_eq!(wal_check(&case), crate::check::Status::Warning);
        let store = Store::open(&case, Default::default()).unwrap();
        assert_eq!(store.snapshot().commit, prefix);
        assert_eq!(store.snapshot().len(), prefix);
        assert!(store.annotation(prefix + 1).is_none());
        let expected_len = txns[damaged].start * WAL_REC;
        assert_eq!(
            std::fs::metadata(wal_path(&case)).unwrap().len(),
            expected_len as u64
        );
        assert_eq!(insert(&store, "next").unwrap().commit.seq, prefix + 1);
        drop(store);
        let reopened = Store::open(&case, Default::default()).unwrap();
        assert_eq!(reopened.snapshot().commit, prefix + 1);
        assert_eq!(reopened.snapshot().len(), prefix + 1);
    }

    #[test]
    fn torn_metadata_after_a_header_does_not_prove_an_earlier_hole_durable() {
        torn_metadata_case(true, false);
    }

    #[test]
    fn torn_metadata_after_a_header_is_not_reconstructed() {
        torn_metadata_case(false, false);
    }

    #[test]
    fn torn_metadata_with_a_later_verified_proof_stays_corruption() {
        for hole in [false, true] {
            torn_metadata_case(hole, true);
        }
    }

    /// A log whose header a crash tore is removed and created again at the open. The
    /// file's sync does not make its directory entry durable, so the open syncs the
    /// generation's directory with the log in it before the writer starts, or a power
    /// loss after the first commits would lose the log with them. Every writable open
    /// syncs it, a plain reopen included, since an open that failed after creating the
    /// log leaves the entry unsynced for the open that follows it.
    #[cfg(unix)]
    #[test]
    fn a_recreated_log_is_durable_in_its_directory_before_the_first_commit() {
        for torn in [
            None,
            Some(&[][..]),
            Some(&[0u8; WAL_REC][..]),
            Some(&wal::MAGIC[..5]),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("db");
            drop(Store::open(&root, options()).unwrap());
            let path = wal_path(&root);
            let gen_dir = path.parent().unwrap().to_path_buf();
            if let Some(torn) = torn {
                std::fs::write(&path, torn).unwrap();
            }
            crate::fsutil::take_dir_syncs();
            let store = Store::open(&root, options()).unwrap();
            let syncs = crate::fsutil::take_dir_syncs();
            let with_log = syncs
                .iter()
                .position(|(d, names)| *d == gen_dir && names.iter().any(|n| n == "wal.log"));
            assert!(
                with_log.is_some(),
                "no sync of {} with the new log in it: {syncs:?}",
                gen_dir.display()
            );
            insert(&store, "one").unwrap();
            drop(store);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(&bytes[..WAL_REC], &wal::header_record());
            assert_eq!(Store::open(&root, options()).unwrap().snapshot().len(), 1);
        }
    }

    /// `sparkles check` reports what open refuses, as an error: a `meta.json` without its
    /// `features` list, which every format 5 generation has, and a log whose first record
    /// is neither the header nor the zeros of a header torn at the log's creation. The
    /// zeros alone are the torn creation that open heals, not an error.
    #[test]
    fn check_reports_a_missing_features_list_and_a_refused_first_record() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let store = Store::open(&root, options()).unwrap();
        insert(&store, "one").unwrap();
        drop(store);
        let gen_dir = wal_path(&root).parent().unwrap().to_path_buf();
        let opts = crate::check::CheckOptions { quick: false };
        let check = |name: &str| {
            let report = crate::check::check(&root, &opts).unwrap();
            let c = report.get(name).expect("the check ran").clone();
            (
                c.status,
                c.issues
                    .iter()
                    .map(|i| i.message.clone())
                    .collect::<Vec<_>>(),
            )
        };
        let meta_path = gen_dir.join("meta.json");
        let meta = std::fs::read(&meta_path).unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&meta).unwrap();
        v.as_object_mut().unwrap().remove("features").unwrap();
        std::fs::write(&meta_path, serde_json::to_vec(&v).unwrap()).unwrap();
        let (status, issues) = check("generation");
        assert_eq!(status, crate::check::Status::Error, "{issues:?}");
        assert!(
            issues.iter().any(|m| m.contains("features is missing")),
            "{issues:?}"
        );
        std::fs::write(&meta_path, &meta).unwrap();
        assert_eq!(check("generation").0, crate::check::Status::Ok);
        let good = std::fs::read(wal_path(&root)).unwrap();
        // a zero record with bytes after it, and the pre-release headerless log
        for (bad, text) in [
            (
                [&[0u8; WAL_REC][..], &[1u8][..]].concat(),
                "bytes follow it",
            ),
            (good[WAL_REC..].to_vec(), "wal version 0"),
        ] {
            std::fs::write(wal_path(&root), &bad).unwrap();
            let (status, issues) = check("wal");
            assert_eq!(status, crate::check::Status::Error, "{issues:?}");
            assert!(
                issues
                    .iter()
                    .any(|m| m.contains(text) && m.contains("open refuses")),
                "{issues:?}"
            );
            assert!(Store::open(&root, options()).is_err());
            assert_eq!(std::fs::read(wal_path(&root)).unwrap(), bad);
        }
        std::fs::write(wal_path(&root), [0u8; WAL_REC]).unwrap();
        let (status, issues) = check("wal");
        assert_ne!(status, crate::check::Status::Error, "{issues:?}");
        assert_eq!(Store::open(&root, options()).unwrap().snapshot().len(), 0);
    }

    /// A power loss can keep the first 4 KiB block of a record that straddles two blocks
    /// and drop the second, as a `log-writes` replay of X09 section 10.2 did. A record after
    /// the last commit then holds a type byte, a few id bytes and zeros. Open truncates the
    /// incomplete transaction whole, so `sparkles check` must not call its ids an error.
    #[test]
    fn a_torn_record_after_the_last_commit_is_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let store = Store::open(&root, options()).unwrap();
        insert(&store, "one").unwrap();
        insert(&store, "two").unwrap();
        drop(store);
        let buf = std::fs::read(wal_path(&root)).unwrap();
        let txns = transactions(&buf);
        assert_eq!(txns.len(), 2);
        let first_of_two = &buf[txns[1].start * WAL_REC..(txns[1].start + 1) * WAL_REC];
        assert!(matches!(first_of_two[0], WAL_INSERT | WAL_DELETE));
        let mut bad = buf[..txns[0].end * WAL_REC].to_vec();
        bad.extend_from_slice(&first_of_two[..3]);
        bad.resize(bad.len() + WAL_REC - 3, 0);
        let case = dir.path().join("case");
        copy_dir(&root, &case);
        std::fs::write(wal_path(&case), &bad).unwrap();
        assert_eq!(wal_check(&case), crate::check::Status::Warning);
        let store = Store::open(&case, Default::default()).unwrap();
        assert_eq!(store.snapshot().commit, 1);
        drop(store);
        assert_eq!(wal_check(&case), crate::check::Status::Ok);
    }

    /// What `sparkles check` says about a WAL, which must agree with open.
    fn wal_check(root: &Path) -> crate::check::Status {
        let opts = crate::check::CheckOptions { quick: false };
        let report = crate::check::check(root, &opts).unwrap();
        report.get("wal").expect("a wal check").status
    }

    fn has(snap: &Snapshot, name: &str) -> bool {
        let Some(s) = snap.lookup_iri(&format!("urn:{name}")) else {
            return false;
        };
        let q = [
            s,
            snap.lookup_iri("urn:p").unwrap(),
            snap.lookup_iri("urn:o").unwrap(),
            Id::DEFAULT_GRAPH,
        ];
        snap.contains(&q).unwrap()
    }

    #[test]
    fn damage_inside_an_unfenced_suffix_is_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Arc::new(Store::open(&root, options()).unwrap());
        insert(&s, "one").unwrap();
        let (hit, go) = pause(&s, "group-sealed");
        let second = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "two"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let later: Vec<_> = ["three", "four", "five"]
            .into_iter()
            .map(|name| {
                let s = s.clone();
                thread::spawn(move || insert(&s, name))
            })
            .collect();
        queued(&s, 3);
        // Power loss during the fences of commits 2 to 5 could keep any subset of the
        // pages of their records, which no fence had made durable yet. Only commit 1 was
        // acknowledged. Writers leave the log to the leaders, so the records reach the
        // file with their fences, and the image is the files once all four are written.
        go.send(()).unwrap();
        second.join().unwrap().unwrap();
        for t in later {
            t.join().unwrap().unwrap();
        }
        let crashed = dir.path().join("crashed");
        copy_dir(&root, &crashed);
        let buf = std::fs::read(wal_path(&crashed)).unwrap();
        let txns = transactions(&buf);
        assert_eq!(txns.len(), 5);
        type Damage = fn(&mut [u8]);
        let damages: [(&str, Damage); 2] = [
            ("zeroed pages", |b| b.fill(0)),
            ("a checksum mismatch", |b| b[5] ^= 0x55),
        ];
        for (what, damage) in damages {
            let case = dir.path().join(what.replace(' ', "-"));
            copy_dir(&crashed, &case);
            let mut bad = buf.clone();
            let third = &txns[2];
            damage(&mut bad[third.start * WAL_REC..third.end * WAL_REC]);
            std::fs::write(wal_path(&case), &bad).unwrap();
            assert_eq!(wal_check(&case), crate::check::Status::Warning, "{what}");
            let s = Store::open(&case, Default::default())
                .unwrap_or_else(|e| panic!("{what} in the unfenced suffix: {e}"));
            let snap = s.snapshot();
            assert_eq!(snap.commit, 2, "{what}");
            assert!(has(&snap, "one") && has(&snap, "two"), "{what}");
            assert_eq!(snap.len(), 2, "{what}");
            assert_eq!(
                std::fs::metadata(wal_path(&case)).unwrap().len(),
                (txns[1].end * WAL_REC) as u64,
                "{what}"
            );
        }
    }

    #[test]
    fn damage_a_later_grouped_record_proves_durable_is_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        {
            let s = Store::open(&root, options()).unwrap();
            for name in ["one", "two", "three"] {
                insert(&s, name).unwrap();
            }
        }
        let path = wal_path(&root);
        let buf = std::fs::read(&path).unwrap();
        let txns = transactions(&buf);
        assert_eq!(txns.len(), 3);
        // Commit 3 was written after commit 2 was acknowledged, and its record says so.
        let mut bad = buf.clone();
        bad[txns[1].start * WAL_REC..txns[1].end * WAL_REC].fill(0);
        std::fs::write(&path, &bad).unwrap();
        assert_eq!(wal_check(&root), crate::check::Status::Error);
        match Store::open(&root, Default::default()) {
            Err(Error::Corrupt(m)) => assert!(m.contains("wal.log"), "{m}"),
            Err(e) => panic!("expected corruption, got {e}"),
            Ok(_) => panic!("damage before the durable point must not be truncated"),
        }
    }

    #[test]
    fn an_idle_direct_commit_after_grouped_ones_proves_them_durable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        {
            let s = Arc::new(Store::open(&root, options()).unwrap());
            insert(&s, "one").unwrap();
            let (hit, go) = pause(&s, "group-sealed");
            let second = {
                let s = s.clone();
                thread::spawn(move || insert(&s, "two"))
            };
            hit.recv_timeout(Duration::from_secs(5)).unwrap();
            let later: Vec<_> = ["three", "four", "five"]
                .into_iter()
                .map(|name| {
                    let s = s.clone();
                    thread::spawn(move || insert(&s, name))
                })
                .collect();
            queued(&s, 3);
            go.send(()).unwrap();
            second.join().unwrap().unwrap();
            for t in later {
                t.join().unwrap().unwrap();
            }
        }
        // the only writer of a reopened store commits as a group of one, with unfenced == 0
        // proving every commit before it is durable
        {
            let s = Store::open(
                &root,
                StoreOptions {
                    group_commit: true,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(insert(&s, "six").unwrap().commit.seq, 6);
            assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 1);
            assert_eq!(s.commit_metrics().sync[SyncPath::Group as usize].count(), 1);
        }
        let buf = std::fs::read(wal_path(&root)).unwrap();
        let txns = transactions(&buf);
        assert_eq!(txns.len(), 6);
        let case = |name: &str, txn: usize| {
            let case = dir.path().join(name);
            copy_dir(&root, &case);
            let mut bad = buf.clone();
            // a checksum mismatch, which is not taken for preallocated zeros
            bad[txns[txn].start * WAL_REC + 5] ^= 0x55;
            std::fs::write(wal_path(&case), &bad).unwrap();
            case
        };
        // damage inside the grouped region, before a record that proves it durable
        let bad = case("grouped", 2);
        assert_eq!(wal_check(&bad), crate::check::Status::Error);
        match Store::open(&bad, Default::default()) {
            Err(Error::Corrupt(m)) => assert!(m.contains("wal.log"), "{m}"),
            Err(e) => panic!("expected corruption, got {e}"),
            Ok(_) => panic!("damage before the durable point must not be truncated"),
        }
        // damage in the last commit is a torn tail
        let torn = case("direct", 5);
        assert_eq!(wal_check(&torn), crate::check::Status::Warning);
        let s = Store::open(&torn, Default::default()).unwrap();
        assert_eq!(s.snapshot().commit, 5);
        assert!(has(&s.snapshot(), "five") && !has(&s.snapshot(), "six"));
    }

    fn kind_insert(
        s: &Store,
        kind: CommitKind,
        o: crate::guard::WriteOptions,
        q: Quad,
    ) -> Result<Receipt> {
        let mut t = s.try_write_with(kind, o)?;
        let ids = t.encode_quad(&q, &mut Default::default())?;
        t.insert(ids)?;
        t.commit()
    }

    fn in_graph(name: &str, graph: &str) -> Quad {
        Quad::new(
            NamedNode::new(format!("urn:{name}")).unwrap(),
            NamedNode::new("urn:p").unwrap(),
            NamedNode::new("urn:o").unwrap(),
            NamedNode::new(graph).unwrap(),
        )
    }

    #[test]
    fn graph_store_upload_patch_bypass_and_restricted_writes_join_the_group() {
        use crate::access::{GraphAccess, GraphRule, Graphs};
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "first"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let restricted = crate::guard::WriteOptions {
            graphs: Some(Arc::new(GraphAccess::graphs(
                Graphs::All,
                Graphs::Only(GraphRule::new(["urn:g"], &[] as &[&str])),
            ))),
            ..Default::default()
        };
        let bypass = crate::guard::WriteOptions {
            bypass_validation: true,
            ..Default::default()
        };
        let writes = [
            (CommitKind::GspPut, Default::default()),
            (CommitKind::GspPost, bypass),
            (CommitKind::GspDelete, Default::default()),
            (CommitKind::Upload, Default::default()),
            (CommitKind::Patch, Default::default()),
            (CommitKind::Update, restricted),
        ];
        let handles: Vec<_> = writes
            .into_iter()
            .enumerate()
            .map(|(n, (kind, o))| {
                let s = s.clone();
                thread::spawn(move || kind_insert(&s, kind, o, in_graph(&format!("w{n}"), "urn:g")))
            })
            .collect();
        queued(&s, writes_len());
        go.send(()).unwrap();
        first.join().unwrap().unwrap();
        let receipts: Vec<Receipt> = handles
            .into_iter()
            .map(|h| h.join().unwrap().unwrap())
            .collect();
        // one fence for the first commit, and one for the six that queued behind it
        let m = s.commit_metrics();
        assert_eq!(m.fallbacks.iter().sum::<u64>(), 0, "{:?}", m.fallbacks);
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 2);
        // a bypassing write is recorded as unvalidated, in its receipt and in the log
        let bypassed = receipts.iter().find(|r| r.validation.is_some()).unwrap();
        assert_eq!(
            bypassed.validation.as_ref().unwrap().status,
            crate::guard::GuardStatus::Bypassed
        );
        assert!(bypassed.commit.unvalidated);
        let seq = bypassed.commit.seq;
        drop(s);
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(reopened.snapshot().len(), 7);
        assert!(reopened.catalog.lock().get(seq).unwrap().unvalidated);
        let kinds: Vec<_> = (2..=7)
            .map(|n| reopened.catalog.lock().get(n).unwrap().kind)
            .collect();
        for k in [CommitKind::GspPut, CommitKind::Upload, CommitKind::Patch] {
            assert!(kinds.contains(&k), "{k:?} in {kinds:?}");
        }
    }

    fn writes_len() -> usize {
        6
    }

    #[test]
    fn a_refused_quad_of_a_restricted_write_never_reaches_the_log() {
        use crate::access::{GraphAccess, GraphRule, Graphs};
        for grouped in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = Store::open(
                dir.path(),
                StoreOptions {
                    group_commit: grouped,
                    ..Default::default()
                },
            )
            .unwrap();
            insert(&s, "base").unwrap();
            let wal_before = std::fs::metadata(wal_path(dir.path())).unwrap().len();
            let only_g = crate::guard::WriteOptions {
                graphs: Some(Arc::new(GraphAccess::graphs(
                    Graphs::All,
                    Graphs::Only(GraphRule::new(["urn:g"], &[] as &[&str])),
                ))),
                ..Default::default()
            };
            let refused = kind_insert(
                &s,
                CommitKind::Update,
                only_g.clone(),
                in_graph("refused", "urn:other"),
            );
            assert!(
                matches!(refused, Err(Error::NotPermitted(_))),
                "{refused:?}"
            );
            assert_eq!(s.snapshot().commit, 1);
            assert_eq!(
                std::fs::metadata(wal_path(dir.path())).unwrap().len(),
                wal_before,
                "grouped={grouped}"
            );
            // an allowed write of the same restriction commits
            let allowed = kind_insert(&s, CommitKind::Update, only_g, in_graph("allowed", "urn:g"));
            assert_eq!(allowed.unwrap().commit.seq, 2);
            drop(s);
            let reopened = Store::open(dir.path(), Default::default()).unwrap();
            assert_eq!(reopened.snapshot().len(), 2);
            assert!(!has(&reopened.snapshot(), "refused"));
        }
    }

    fn current_gen(root: &Path) -> String {
        std::fs::read_to_string(root.join("CURRENT"))
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn writes_during_a_compaction_group_and_a_failed_fence_aborts_it() {
        for fence_fails in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("db");
            let s = Arc::new(Store::open(&root, options()).unwrap());
            for n in 0..3 {
                insert(&s, &format!("seed{n}")).unwrap();
            }
            let before = current_gen(&root);
            let (hit, go) = pause(&s, "compact-built");
            let compaction = {
                let s = s.clone();
                thread::spawn(move || s.compact())
            };
            hit.recv_timeout(Duration::from_secs(10)).unwrap();
            // writes made while the new generation builds are grouped, and the compaction
            // carries them over
            let fences = s.group.as_ref().unwrap().fences.load(Ordering::Relaxed);
            if fence_fails {
                s.fail_next_group_sync();
                assert!(insert(&s, "during").is_err());
            } else {
                for n in 0..3 {
                    insert(&s, &format!("during{n}")).unwrap();
                }
                let after = s.group.as_ref().unwrap().fences.load(Ordering::Relaxed);
                assert_eq!(after, fences + 3);
                assert_eq!(s.commit_metrics().fallbacks.iter().sum::<u64>(), 0);
            }
            go.send(()).unwrap();
            let report = compaction.join().unwrap();
            if fence_fails {
                // the writer lock's drain finds the failed fence before any switch
                assert!(report.is_err(), "{report:?}");
                assert_eq!(current_gen(&root), before, "no generation was switched in");
                assert!(matches!(insert(&s, "later"), Err(Error::Poisoned)));
            } else {
                report.unwrap();
                assert_ne!(current_gen(&root), before);
                assert_eq!(s.snapshot().commit, 6);
            }
            drop(s);
            let reopened = Store::open(&root, Default::default()).unwrap();
            let snap = reopened.snapshot();
            if fence_fails {
                assert_eq!(snap.len(), snap.commit);
                assert!(snap.commit >= 3);
            } else {
                assert_eq!(snap.commit, 6);
                assert_eq!(snap.len(), 6);
                for name in ["seed0", "seed2", "during0", "during2"] {
                    assert!(has(&snap, name), "{name}");
                }
            }
        }
    }

    fn message_options(message: &str) -> crate::guard::WriteOptions {
        crate::guard::WriteOptions {
            message: Some(message.into()),
            ..Default::default()
        }
    }

    #[test]
    fn commit_messages_and_digests_join_the_group_and_keep_their_annotations() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(
            Store::open(
                dir.path(),
                StoreOptions {
                    commit_digests: true,
                    ..options()
                },
            )
            .unwrap(),
        );
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one").unwrap())
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let writes: Vec<_> = ["two", "three", "four"]
            .into_iter()
            .map(|name| {
                let s = s.clone();
                thread::spawn(move || {
                    let o = message_options(&format!("message {name}"));
                    kind_insert(&s, CommitKind::Update, o, in_graph(name, "urn:g"))
                })
            })
            .collect();
        // they queue behind the paused fence instead of waiting for it
        queued(&s, 3);
        assert_eq!(s.snapshot().commit, 0);
        go.send(()).unwrap();
        first.join().unwrap();
        let receipts: Vec<Receipt> = writes
            .into_iter()
            .map(|h| h.join().unwrap().unwrap())
            .collect();
        let mut by_seq: Vec<_> = receipts.iter().map(|r| (r.commit.seq, r)).collect();
        by_seq.sort_by_key(|r| r.0);
        for (seq, r) in &by_seq {
            let a = s.annotation(*seq).unwrap();
            assert!(a.message.as_deref().unwrap().starts_with("message "));
            assert_eq!(r.annotation, a, "the receipt carries the annotation");
            assert!(a.digest.is_some());
        }
        // the digests chain from one commit to the next
        let digests: Vec<_> = (1..=4).map(|n| s.annotation(n).unwrap().digest).collect();
        assert!(digests.iter().all(Option::is_some));
        assert_eq!(
            digests
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            4
        );
        let m = s.commit_metrics();
        assert_eq!(m.fallbacks.iter().sum::<u64>(), 0, "{:?}", m.fallbacks);
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 2);
        drop(s);
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        for n in 1..=4 {
            let a = reopened.annotation(n).unwrap();
            assert_eq!(a.digest, digests[n as usize - 1]);
        }
        let mut messages: Vec<_> = (2..=4)
            .map(|n| reopened.annotation(n).unwrap().message.unwrap().to_string())
            .collect();
        messages.sort();
        assert_eq!(messages, ["message four", "message three", "message two"]);
    }

    /// Branch stores share their dataset's quota, which sums the zeros after the records
    /// of every store's annotations file, and a close takes them back.
    #[test]
    fn the_quota_sums_the_annotation_zeros_of_every_branch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ds");
        let s = Store::open(&root, Default::default()).unwrap();
        kind_insert(
            &s,
            CommitKind::Update,
            message_options("main"),
            in_graph("a", "urn:g"),
        )
        .unwrap();
        s.create_branch("dev", &Default::default()).unwrap();
        let dev = s.branch("dev").unwrap();
        kind_insert(
            &dev,
            CommitKind::Update,
            message_options("dev"),
            in_graph("b", "urn:g"),
        )
        .unwrap();
        fn files(p: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(p).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    files(&p, out);
                } else if p.file_name() == Some(std::ffi::OsStr::new(crate::annotations::FILE)) {
                    out.push(p);
                }
            }
        }
        let mut found = Vec::new();
        files(&root, &mut found);
        assert_eq!(found.len(), 2, "{found:?}");
        let zeros: Vec<u64> = found
            .iter()
            .map(|p| {
                let b = std::fs::read(p).unwrap();
                (b.len() - crate::annotations::records_end(&b).unwrap()) as u64
            })
            .collect();
        assert!(zeros.iter().all(|z| *z > 0), "{zeros:?}");
        assert_eq!(s.quota.ann_preallocated(), zeros.iter().sum::<u64>());
        drop(dev);
        drop(s);
        let s = Store::open(&root, Default::default()).unwrap();
        assert_eq!(s.quota.ann_preallocated(), 0);
    }

    /// Once the annotations file has preallocated space, a fence writes its commits'
    /// annotation records through the write-through handle, as it writes their log
    /// records, and no longer synchronizes the file (X09 item 501). The records survive a
    /// reopen, which cuts the preallocated zeros.
    #[test]
    fn fences_write_their_annotation_records_through() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(
            Store::open(
                dir.path(),
                StoreOptions {
                    commit_digests: true,
                    ..options()
                },
            )
            .unwrap(),
        );
        let writers: Vec<_> = (0..4)
            .map(|t| {
                let s = s.clone();
                thread::spawn(move || {
                    for k in 0..15 {
                        let name = format!("w{t}-{k}");
                        let o = message_options(&format!("message {name}"));
                        kind_insert(&s, CommitKind::Update, o, in_graph(&name, "urn:g")).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let g = s.group.as_ref().unwrap();
        let (fences, direct, ann_direct) = (
            g.fences.load(Ordering::Relaxed),
            g.direct_fences.load(Ordering::Relaxed),
            g.ann_direct_fences.load(Ordering::Relaxed),
        );
        assert!(fences > 0);
        let path = dir.path().join(crate::annotations::FILE);
        let bytes = std::fs::read(&path).unwrap();
        let end = crate::annotations::records_end(&bytes).unwrap();
        // the log's own write-through fences show that this file system takes direct I/O
        if direct > 0 {
            assert!(
                ann_direct > 0 && ann_direct + 2 >= direct,
                "{ann_direct} of {fences} fences wrote annotations through, {direct} the log"
            );
            assert!(end < bytes.len(), "no preallocated zeros after the records");
            assert_eq!(s.quota.ann_preallocated(), (bytes.len() - end) as u64);
        }
        let messages: Vec<_> = (1..=60)
            .map(|n| s.annotation(n).unwrap().message.unwrap())
            .collect();
        drop(s);
        let reopened = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(reopened.snapshot().commit, 60);
        for (n, m) in (1..=60).zip(&messages) {
            assert_eq!(reopened.annotation(n).unwrap().message.as_ref(), Some(m));
            assert!(reopened.annotation(n).unwrap().digest.is_some());
        }
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            end as u64,
            "the reopen cut the preallocated zeros"
        );
    }

    /// Commits 2 to 4 of a store with a message each. Commits 3 and 4 queue behind commit
    /// 2's fence, and the files are copied while the fence is held, when none of the three
    /// has written anything yet. The log, the vocabulary and the annotations file are
    /// copied again once the fences are done. Returns the directory of the store and of
    /// the copy.
    fn annotated_suffix(dir: &Path) -> (PathBuf, PathBuf) {
        let root = dir.join("db");
        let s = Arc::new(Store::open(&root, options()).unwrap());
        insert(&s, "one").unwrap();
        let (hit_tx, hit) = mpsc::channel();
        let (go_tx, go) = mpsc::channel();
        let go = Mutex::new(go);
        let first = AtomicBool::new(true);
        let expired = Arc::new(AtomicBool::new(false));
        let deadline = expired.clone();
        s.set_failpoint(
            "group-sealed",
            Some(Arc::new(move |_| {
                if first.swap(false, Ordering::SeqCst) {
                    let _ = hit_tx.send(());
                    if go.lock().recv_timeout(Duration::from_secs(10)).is_err() {
                        deadline.store(true, Ordering::SeqCst);
                    }
                }
            })),
        );
        let mut writes = Vec::new();
        let spawn = |writes: &mut Vec<_>, name: &'static str| {
            let s = s.clone();
            let w = thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    kind_insert(
                        &s,
                        CommitKind::Update,
                        message_options(name),
                        in_graph(name, "urn:g"),
                    )
                })
                .map_err(|e| e.to_string())?;
            writes.push(w);
            Ok::<_, String>(())
        };
        let crashed = dir.join("crashed");
        let observed = (|| -> std::result::Result<_, String> {
            // No later writer starts until m2 owns the held fence.
            spawn(&mut writes, "m2")?;
            hit.recv_timeout(Duration::from_secs(5))
                .map_err(|e| format!("m2 seal: {e}"))?;
            let (tx, appended) = mpsc::channel();
            s.set_failpoint(
                "group-appended",
                Some(Arc::new(move |_| match thread::current().name() {
                    Some("m3") => {
                        let _ = tx.send("m3");
                    }
                    Some("m4") => {
                        let _ = tx.send("m4");
                    }
                    _ => {}
                })),
            );
            for name in ["m3", "m4"] {
                spawn(&mut writes, name)?;
                let actual = appended
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|e| format!("{name} append: {e}"))?;
                if actual != name {
                    return Err(format!("expected {name}, observed {actual}"));
                }
            }
            let queued = s.group.as_ref().unwrap().state.lock().queue.len();
            // If copying panics, still release and join every writer before unwinding.
            let copied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                copy_dir(&root, &crashed)
            }));
            Ok((queued, copied))
        })();
        let _ = go_tx.send(());
        s.set_failpoint("group-sealed", None);
        s.set_failpoint("group-appended", None);
        let writes: Vec<_> = writes.into_iter().map(|w| w.join()).collect();
        assert!(
            !expired.load(Ordering::SeqCst),
            "annotation fence exceeded its deadline"
        );
        let (queued, copied) = observed.unwrap();
        assert_eq!(queued, 2, "m3 and m4 queued behind m2");
        for w in writes {
            w.unwrap().unwrap();
        }
        if let Err(panic) = copied {
            std::panic::resume_unwind(panic);
        }
        // Writers leave the log and the annotations file to the leader, so the records of
        // m3 and m4 reach them only with their own fence. The image is the crash right
        // after that fence's writes: the log, the vocabulary and the annotations file as
        // they are now. The tests take records out of the annotations file to model a
        // crash between its write and the log's.
        let log = wal_path(&root);
        std::fs::copy(&log, wal_path(&crashed)).unwrap();
        let vocab = log.with_file_name("delta.vocab");
        std::fs::copy(&vocab, wal_path(&crashed).with_file_name("delta.vocab")).unwrap();
        std::fs::copy(
            root.join(crate::annotations::FILE),
            crashed.join(crate::annotations::FILE),
        )
        .unwrap();
        (root, crashed)
    }

    #[test]
    fn a_commit_that_reached_the_disk_without_its_annotation_is_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let (_root, crashed) = annotated_suffix(dir.path());
        // the records of commits 3 and 4 never reached annotations.bin: 17 bytes each,
        // before the zeros the file preallocated
        let path = crashed.join(crate::annotations::FILE);
        let end = crate::annotations::records_end(&std::fs::read(&path).unwrap()).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(end as u64 - 34)
            .unwrap();
        // no later record shows commits 3 and 4 were durable: both are cut off, and the
        // check reports it as a torn tail rather than damage
        assert_eq!(wal_check(&crashed), crate::check::Status::Warning);
        let s = Store::open(&crashed, Default::default()).unwrap();
        let snap = s.snapshot();
        assert_eq!(snap.commit, 2);
        assert_eq!(snap.len(), 2);
        assert_eq!(s.annotation(2).unwrap().message.as_deref(), Some("m2"));
        assert!(s.annotation(3).is_none());
    }

    #[test]
    fn a_lost_annotation_of_a_commit_a_later_record_shows_durable_keeps_the_commit() {
        let dir = tempfile::tempdir().unwrap();
        let (root, _crashed) = annotated_suffix(dir.path());
        {
            // a later commit, made after the fence: its record says commits 2 to 4 are durable
            let s = Store::open(&root, options()).unwrap();
            kind_insert(
                &s,
                CommitKind::Update,
                message_options("m5"),
                in_graph("m5", "urn:g"),
            )
            .unwrap();
        }
        // the middle record is lost from the file afterwards (17 bytes for each message)
        let path = root.join(crate::annotations::FILE);
        let mut bytes = std::fs::read(&path).unwrap();
        let first = crate::annotations::records_end(&bytes).unwrap() - 4 * 17;
        bytes.drain(first + 17..first + 34);
        std::fs::write(&path, bytes).unwrap();
        // the commit stays, without its message
        assert_eq!(wal_check(&root), crate::check::Status::Warning);
        let s = Store::open(&root, Default::default()).unwrap();
        assert_eq!(s.snapshot().commit, 5);
        assert_eq!(s.snapshot().len(), 5);
        assert!(s.annotation(3).is_none());
        assert_eq!(s.annotation(4).unwrap().message.as_deref(), Some("m4"));
    }

    #[test]
    fn a_missing_annotations_file_does_not_truncate_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let (_root, crashed) = annotated_suffix(dir.path());
        std::fs::remove_file(crashed.join(crate::annotations::FILE)).unwrap();
        let s = Store::open(&crashed, Default::default()).unwrap();
        assert!(s.snapshot().commit >= 2);
        assert_eq!(s.snapshot().len(), s.snapshot().commit);
    }

    /// X16 Phase 6: a group of one (direct or buffered, grouped or solo) carries
    /// `WAL_FLAG_ANNOTATED` when it carries an annotation, so a crash that leaves its log
    /// record on disk without its annotation record is truncated as a torn tail on reopen
    /// and accepts the next commit.
    #[test]
    fn a_group_of_one_without_its_annotation_is_a_torn_tail_for_direct_and_buffered() {
        for group_commit in [true, false] {
            for direct in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let tag = format!(
                    "{}-{}",
                    if group_commit { "grouped" } else { "solo" },
                    if direct { "direct" } else { "buffered" }
                );
                let root = dir.path().join(&tag);
                {
                    let opts = StoreOptions {
                        group_commit,
                        wal_direct_writes: direct,
                        ..options()
                    };
                    let s = Store::open(&root, opts).unwrap();
                    for name in ["m1", "m2"] {
                        let r = kind_insert(
                            &s,
                            CommitKind::Update,
                            message_options(name),
                            in_graph(name, "urn:g"),
                        )
                        .unwrap();
                        assert!(r.committed, "{tag}");
                        assert_eq!(r.annotation.message.as_deref(), Some(name), "{tag}");
                    }
                    let g = s.group.as_ref().unwrap();
                    if group_commit {
                        assert_eq!(g.fences.load(Ordering::Relaxed), 2, "{tag}");
                        if direct && s.wal_direct_active() {
                            assert_eq!(g.direct_fences.load(Ordering::Relaxed), 1, "{tag}");
                            assert_eq!(g.ann_direct_fences.load(Ordering::Relaxed), 1, "{tag}");
                        } else if !direct {
                            assert!(!s.wal_direct_active(), "{tag}");
                            assert_eq!(g.direct_fences.load(Ordering::Relaxed), 0, "{tag}");
                            assert_eq!(g.ann_direct_fences.load(Ordering::Relaxed), 0, "{tag}");
                        }
                    } else {
                        assert_eq!(g.fences.load(Ordering::Relaxed), 0, "{tag}");
                        assert_eq!(g.direct_fences.load(Ordering::Relaxed), 0, "{tag}");
                        assert_eq!(g.ann_direct_fences.load(Ordering::Relaxed), 0, "{tag}");
                        if direct && s.wal_direct_active() {
                            assert_eq!(synced(&s, SyncPath::Direct), 1, "{tag}");
                            assert_eq!(synced(&s, SyncPath::Buffered), 1, "{tag}");
                        } else if !direct {
                            assert!(!s.wal_direct_active(), "{tag}");
                            assert_eq!(synced(&s, SyncPath::Direct), 0, "{tag}");
                            assert_eq!(synced(&s, SyncPath::Buffered), 2, "{tag}");
                        }
                    }
                }
                // drop the last commit's annotation record while keeping commit 1's (17 bytes each)
                let path = root.join(crate::annotations::FILE);
                let end = crate::annotations::records_end(&std::fs::read(&path).unwrap()).unwrap();
                OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(end as u64 - 17)
                    .unwrap();
                assert_eq!(
                    wal_check(&root),
                    crate::check::Status::Warning,
                    "{tag}: missing tail annotation must be reported as a torn tail"
                );
                let s = Store::open(&root, Default::default()).unwrap();
                let snap = s.snapshot();
                assert_eq!(snap.commit, 1, "{tag}");
                assert_eq!(snap.len(), 1, "{tag}");
                assert_eq!(s.annotation(1).unwrap().message.as_deref(), Some("m1"));
                assert!(s.annotation(2).is_none());
                // the reopened store continues cleanly at sequence 2
                let next = kind_insert(
                    &s,
                    CommitKind::Update,
                    message_options("m2-retry"),
                    in_graph("m2-retry", "urn:g"),
                )
                .unwrap();
                assert_eq!(next.commit.seq, 2, "{tag}");
                assert_eq!(next.annotation.message.as_deref(), Some("m2-retry"));
                drop(s);
                let reopened = Store::open(&root, Default::default()).unwrap();
                assert_eq!(reopened.snapshot().commit, 2, "{tag}");
                assert_eq!(
                    reopened.annotation(2).unwrap().message.as_deref(),
                    Some("m2-retry")
                );
            }
        }
    }

    /// An unwind inside a non-control solo fence poisons the writer immediately without
    /// exposing the unpublished head, so subsequent writes are rejected before interning
    /// any term even when `group_commit` is enabled.
    #[test]
    fn solo_fence_unwind_poisons_writer_and_keeps_published_head() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        for group_commit in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir
                .path()
                .join(if group_commit { "grouped" } else { "solo" });
            let opts = StoreOptions {
                group_commit,
                ..Default::default()
            };
            let s = Store::open(&root, opts.clone()).unwrap();
            let r1 = insert(&s, "first").unwrap();
            assert_eq!(r1.commit.seq, 1);

            s.set_failpoint(
                "group-preallocating",
                Some(Arc::new(|_: &Store| panic!("solo fence unwind"))),
            );
            // `CommitKind::Load` takes the solo fallback even when `group_commit` is true.
            let panicked = catch_unwind(AssertUnwindSafe(|| {
                kind_insert(
                    &s,
                    CommitKind::Load,
                    Default::default(),
                    in_graph("second", "urn:g"),
                )
            }));
            assert!(panicked.is_err(), "group_commit={group_commit}");
            s.set_failpoint("group-preallocating", None);

            assert_eq!(s.snapshot().commit, 1, "group_commit={group_commit}");
            assert_eq!(s.head_commit().seq, 1, "group_commit={group_commit}");
            assert!(
                matches!(
                    s.try_write_with(CommitKind::Update, Default::default()),
                    Err(Error::Poisoned)
                ),
                "group_commit={group_commit}: try_write_with must fail immediately after solo unwind"
            );
            let mut txn = s.write_as(CommitKind::Update);
            let term = Term::NamedNode(NamedNode::new("urn:after-unwind").unwrap());
            assert!(
                matches!(txn.intern(&term), Err(Error::Poisoned)),
                "group_commit={group_commit}: intern must reject on poisoned writer"
            );
            drop(txn);
            drop(s);

            let reopened = Store::open(&root, opts).unwrap();
            assert_eq!(reopened.snapshot().commit, 1, "group_commit={group_commit}");
            let r2 = insert(&reopened, "second-after-reopen").unwrap();
            assert_eq!(r2.commit.seq, 2, "group_commit={group_commit}");
        }
    }

    /// An unwind inside a solo fence after snapshot publication reconciles the writer's
    /// head to the published snapshot before releasing the writer lock while keeping the
    /// writer poisoned and the coordinator failed.
    #[test]
    fn solo_fence_unwind_after_publication_reconciles_head_and_keeps_poison() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        for group_commit in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir
                .path()
                .join(if group_commit { "grouped" } else { "solo" });
            let opts = StoreOptions {
                group_commit,
                ..Default::default()
            };
            let s = Store::open(&root, opts.clone()).unwrap();
            let r1 = insert(&s, "first").unwrap();
            assert_eq!(r1.commit.seq, 1);

            let mut commits = s.subscribe_commits();
            s.set_failpoint(
                "commit-published",
                Some(Arc::new(|_: &Store| {
                    panic!("solo fence unwind after publication")
                })),
            );
            // `CommitKind::Load` takes the solo fallback even when `group_commit` is true.
            let panicked = catch_unwind(AssertUnwindSafe(|| {
                kind_insert(
                    &s,
                    CommitKind::Load,
                    Default::default(),
                    in_graph("second", "urn:g"),
                )
            }));
            assert!(panicked.is_err(), "group_commit={group_commit}");
            s.set_failpoint("commit-published", None);

            // Check snapshot, watch receiver, writer head, and non-draining historical
            // resolution immediately after the caught unwind, before any operation drains
            // the coordinator.
            assert_eq!(s.snapshot().commit, 2, "group_commit={group_commit}");
            assert_eq!(
                *commits.borrow_and_update(),
                2,
                "group_commit={group_commit}: watch receiver must see commit 2"
            );
            {
                let w = s.writer.lock();
                assert_eq!(
                    w.head.seq, 2,
                    "group_commit={group_commit}: writer head must match published snapshot immediately"
                );
                assert!(
                    w.poisoned,
                    "group_commit={group_commit}: writer must remain poisoned after post-publication unwind"
                );
            }
            assert!(
                s.group.as_ref().unwrap().failed(),
                "group_commit={group_commit}: coordinator must remain failed"
            );
            let (at2, _) = s
                .snapshot_at(&crate::history::At::Commit(2), &Default::default())
                .unwrap_or_else(|e| {
                    panic!(
                        "group_commit={group_commit}: snapshot_at(2) must succeed before drain: {e}"
                    )
                });
            assert_eq!(at2.commit, 2, "group_commit={group_commit}");
            assert_eq!(s.head_commit().seq, 2, "group_commit={group_commit}");
            assert!(
                matches!(
                    s.try_write_with(CommitKind::Update, Default::default()),
                    Err(Error::Poisoned)
                ),
                "group_commit={group_commit}: try_write_with must stay poisoned"
            );
            let mut txn = s.write_as(CommitKind::Update);
            let term = Term::NamedNode(NamedNode::new("urn:after-pub-unwind").unwrap());
            assert!(
                matches!(txn.intern(&term), Err(Error::Poisoned)),
                "group_commit={group_commit}: intern must stay poisoned"
            );
            drop(txn);
            drop(s);

            let reopened = Store::open(&root, opts).unwrap();
            assert_eq!(reopened.snapshot().commit, 2, "group_commit={group_commit}");
            let r3 = insert(&reopened, "third-after-reopen").unwrap();
            assert_eq!(r3.commit.seq, 3, "group_commit={group_commit}");
        }
    }

    /// A fallible WAL encoding error occurs before mutating annotations or poisoning the
    /// writer, so subsequent annotated commits succeed and reopen cleanly.
    #[test]
    fn encode_failure_leaves_annotations_and_writer_clean() {
        for group_commit in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir
                .path()
                .join(if group_commit { "grouped" } else { "solo" });
            let opts = StoreOptions {
                group_commit,
                commit_digests: true,
                ..Default::default()
            };
            let s = Store::open(&root, opts.clone()).unwrap();
            let mut txn = s.write_with(CommitKind::Update, message_options("should-fail"));
            txn.configuration = crate::history::ConfigurationBinding::ImportedUnbound {
                source: crate::history::ConfigurationIdentity {
                    dataset_id: uuid::Uuid::nil(),
                    lineage_id: uuid::Uuid::nil(),
                    generation: crate::evidence::Sha256Digest::from_digest_bytes([0u8; 32]),
                },
            };
            let ids = txn
                .encode_quad(&in_graph("bad-cfg", "urn:g"), &mut Default::default())
                .unwrap();
            txn.insert(ids).unwrap();
            let mut sync = None;
            let err = txn.publish_grouped(None, group_commit, &mut sync);
            drop(txn);
            assert!(
                matches!(err, Err(Error::Corrupt(_))),
                "group_commit={group_commit}: {err:?}"
            );
            assert_eq!(s.snapshot().commit, 0, "group_commit={group_commit}");
            assert!(s.annotation(1).is_none(), "group_commit={group_commit}");

            let r1 = kind_insert(
                &s,
                CommitKind::Update,
                message_options("after-encode-err"),
                in_graph("ok-after", "urn:g"),
            )
            .unwrap();
            assert_eq!(r1.commit.seq, 1, "group_commit={group_commit}");
            assert_eq!(r1.annotation.message.as_deref(), Some("after-encode-err"));
            drop(s);

            let reopened = Store::open(&root, opts).unwrap();
            assert_eq!(reopened.snapshot().commit, 1, "group_commit={group_commit}");
            assert_eq!(
                reopened.annotation(1).unwrap().message.as_deref(),
                Some("after-encode-err")
            );
        }
    }

    /// X16 Phase 6 regression: single-writer ("group of one") commits preserve exact WAL
    /// ordering (unfenced == 0 on idle, WAL_FLAG_ANNOTATED when annotated), commit messages
    /// and change digests across reopen, pre-enqueue error propagation without advancing the
    /// log or poisoning the store, and fence-sync error propagation with clean poisoning.
    #[test]
    fn group_of_one_preserves_wal_order_annotations_and_error_propagation() {
        for group_commit in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir
                .path()
                .join(if group_commit { "grouped" } else { "solo" });
            let opts = StoreOptions {
                group_commit,
                commit_digests: true,
                ..Default::default()
            };
            let s = Store::open(&root, opts.clone()).unwrap();
            s.set_guard(Some(Arc::new(Strict { speculative: true })));

            // 1. Valid single-writer commits with messages and digests
            let r1 = kind_insert(
                &s,
                CommitKind::Update,
                message_options("first"),
                in_graph("g1", "urn:g"),
            )
            .unwrap();
            let r2 = kind_insert(
                &s,
                CommitKind::Update,
                message_options("second"),
                in_graph("g2", "urn:g"),
            )
            .unwrap();
            assert_eq!((r1.commit.seq, r2.commit.seq), (1, 2));
            assert_eq!(r1.annotation.message.as_deref(), Some("first"));
            assert_eq!(r2.annotation.message.as_deref(), Some("second"));
            assert!(r1.annotation.digest.is_some());
            assert!(r2.annotation.digest.is_some());
            assert_ne!(r1.annotation.digest, r2.annotation.digest);

            // 2. Pre-enqueue errors (guard rejection & invalid commit message) propagate
            //    without advancing the WAL or poisoning the store
            let wal_before = std::fs::read(wal_path(&root)).unwrap();
            let rejected = kind_insert(
                &s,
                CommitKind::Update,
                message_options("rejected"),
                in_graph("bad", "urn:g"),
            );
            assert!(
                matches!(rejected, Err(Error::Rejected(_))),
                "group_commit={group_commit}: {rejected:?}"
            );
            let bad_msg = kind_insert(
                &s,
                CommitKind::Update,
                message_options("bad\0msg"),
                in_graph("g3", "urn:g"),
            );
            assert!(
                matches!(bad_msg, Err(Error::Invalid(_))),
                "group_commit={group_commit}: {bad_msg:?}"
            );
            assert_eq!(s.snapshot().commit, 2);
            assert_eq!(std::fs::read(wal_path(&root)).unwrap(), wal_before);

            // 3. Subsequent valid commit still takes seq 3 and chains its digest
            let r3 = kind_insert(
                &s,
                CommitKind::Update,
                message_options("third"),
                in_graph("g3", "urn:g"),
            )
            .unwrap();
            assert_eq!(r3.commit.seq, 3);
            assert_eq!(r3.annotation.message.as_deref(), Some("third"));
            if group_commit {
                assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 3);
                assert_eq!(s.commit_metrics().sync[SyncPath::Group as usize].count(), 3);
            } else {
                assert_eq!(s.commit_metrics().sync[SyncPath::Group as usize].count(), 0);
                assert_eq!(
                    synced(&s, SyncPath::Direct) + synced(&s, SyncPath::Buffered),
                    3
                );
            }

            // 4. Inspect WAL commit records: exact order 1..=3, unfenced == 0, annotated flag set
            let wal_bytes = std::fs::read(wal_path(&root)).unwrap();
            let txns = transactions(&wal_bytes);
            assert_eq!(txns.len(), 3, "group_commit={group_commit}");
            for (idx, txn) in txns.iter().enumerate() {
                let commit_rec = &wal_bytes[(txn.end - 1) * WAL_REC..txn.end * WAL_REC];
                assert_eq!(commit_rec[0], WAL_COMMIT);
                let seq = u64::from_le_bytes(commit_rec[9..17].try_into().unwrap());
                assert_eq!(seq, (idx + 1) as u64);
                assert_ne!(
                    commit_rec[27] & commit::WAL_FLAG_ANNOTATED,
                    0,
                    "group_commit={group_commit}: commit {seq} must carry WAL_FLAG_ANNOTATED"
                );
                assert_eq!(
                    commit_rec[28], 0,
                    "group_commit={group_commit}: idle group-of-one commit {seq} has unfenced == 0"
                );
            }

            // 5. Fence sync failure fails the commit, publishes nothing, and poisons further writes
            s.fail_next_group_sync();
            let failed = kind_insert(
                &s,
                CommitKind::Update,
                message_options("fourth"),
                in_graph("g4", "urn:g"),
            );
            if group_commit {
                assert!(matches!(failed, Err(Error::Poisoned)), "{failed:?}");
            } else {
                assert!(failed.is_err(), "{failed:?}");
            }
            assert_eq!(s.snapshot().commit, 3);
            assert!(matches!(insert(&s, "after-fail"), Err(Error::Poisoned)));
            drop(s);

            // 6. Reopen recovers all 3 acknowledged commits with exact messages and digests
            assert_eq!(wal_check(&root), crate::check::Status::Ok);
            let reopened = Store::open(&root, opts).unwrap();
            assert_eq!(reopened.snapshot().commit, 3);
            assert_eq!(reopened.snapshot().len(), 3);
            for (seq, msg, dig) in [
                (1, "first", r1.annotation.digest),
                (2, "second", r2.annotation.digest),
                (3, "third", r3.annotation.digest),
            ] {
                let a = reopened.annotation(seq).unwrap();
                assert_eq!(a.message.as_deref(), Some(msg));
                assert_eq!(a.digest, dig);
            }
            // And continues cleanly at seq 4
            let r4 = kind_insert(
                &reopened,
                CommitKind::Update,
                message_options("fourth-reopen"),
                in_graph("g4", "urn:g"),
            )
            .unwrap();
            assert_eq!(r4.commit.seq, 4);
        }
    }

    /// Rejects every state that holds `urn:bad`.
    struct Strict {
        speculative: bool,
    }

    impl crate::guard::CommitGuard for Strict {
        fn check(
            &self,
            c: &crate::guard::Candidate<'_>,
        ) -> Result<crate::guard::ValidationSummary> {
            use crate::guard::{GuardMode, GuardStatus, Severity, ValidationSummary};
            let bad = c.view.lookup_iri("urn:bad");
            let hit = matches!(
                &c.changes,
                crate::guard::Changes::Log(log) if log.iter().any(|(_, q)| Some(q[0]) == bad)
            );
            let status = if hit {
                GuardStatus::Rejected
            } else {
                GuardStatus::Passed
            };
            Ok(ValidationSummary::empty(
                status,
                GuardMode::Reject,
                Severity::Violation,
            ))
        }
        fn describe(&self) -> String {
            "strict".into()
        }
        fn speculative(&self) -> bool {
            self.speculative
        }
    }

    #[test]
    fn a_guard_rejection_waits_for_the_state_it_was_judged_against_to_be_durable() {
        for speculative in [true, false] {
            for fence_fails in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let s = Arc::new(Store::open(dir.path(), options()).unwrap());
                s.set_guard(Some(Arc::new(Strict { speculative })));
                let (hit, go) = pause(&s, "group-sealed");
                let first = {
                    let s = s.clone();
                    thread::spawn(move || insert(&s, "one"))
                };
                hit.recv_timeout(Duration::from_secs(5)).unwrap();
                let (done, received) = mpsc::channel();
                let bad = {
                    let s = s.clone();
                    thread::spawn(move || done.send(insert(&s, "bad")).unwrap())
                };
                // judged against a state that is not durable yet, or kept waiting for it
                assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
                if fence_fails {
                    s.fail_next_group_sync();
                }
                go.send(()).unwrap();
                let answer = received.recv_timeout(Duration::from_secs(5)).unwrap();
                if fence_fails {
                    assert!(matches!(answer, Err(Error::Poisoned)), "{answer:?}");
                    assert!(first.join().unwrap().is_err());
                } else {
                    assert!(matches!(answer, Err(Error::Rejected(_))), "{answer:?}");
                    assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
                    assert_eq!(s.snapshot().commit, 1);
                    // the write is refused for good, and the next accepted one follows
                    assert_eq!(insert(&s, "two").unwrap().commit.seq, 2);
                }
                bad.join().unwrap();
            }
        }
    }

    #[test]
    fn a_speculative_guard_follows_the_commits_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        s.set_guard(Some(Arc::new(Strict { speculative: true })));
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let others: Vec<_> = ["two", "three", "four"]
            .into_iter()
            .map(|name| {
                let s = s.clone();
                thread::spawn(move || insert(&s, name).unwrap())
            })
            .collect();
        queued(&s, 3);
        assert_eq!(s.snapshot().commit, 0);
        go.send(()).unwrap();
        first.join().unwrap().unwrap();
        for o in others {
            let r = o.join().unwrap();
            assert_eq!(
                r.validation.as_ref().map(|v| v.status),
                Some(crate::guard::GuardStatus::Passed)
            );
        }
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 2);
        assert_eq!(s.commit_metrics().fallbacks.iter().sum::<u64>(), 0);
    }

    #[test]
    fn a_patch_refused_for_the_speculative_head_waits_for_the_fence() {
        use crate::store::PatchOptions;
        for fence_fails in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = Arc::new(Store::open(dir.path(), options()).unwrap());
            let id = s.dataset_id();
            let (hit, go) = pause(&s, "group-sealed");
            let first = {
                let s = s.clone();
                thread::spawn(move || insert(&s, "one"))
            };
            hit.recv_timeout(Duration::from_secs(5)).unwrap();
            // a `prev` after the first data row names the durable head, and the head the
            // patch starts from is the one that is not durable yet
            let (done, received) = mpsc::channel();
            let patch = {
                let s = s.clone();
                thread::spawn(move || {
                    let text = format!("A <urn:d> <urn:p> 1 .\nH prev <urn:uuid:{id}#commit:0> .");
                    done.send(s.apply_patch(text.as_bytes(), &PatchOptions::default()))
                        .unwrap()
                })
            };
            assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
            if fence_fails {
                s.fail_next_group_sync();
            }
            go.send(()).unwrap();
            let answer = received.recv_timeout(Duration::from_secs(5)).unwrap();
            if fence_fails {
                assert!(matches!(answer, Err(Error::Poisoned)), "{answer:?}");
                assert!(first.join().unwrap().is_err());
            } else {
                assert!(matches!(answer, Err(Error::Patch(_))), "{answer:?}");
                assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
            }
            patch.join().unwrap();
        }
    }

    /// A guard whose check fails with an error that is not a rejection once the state it
    /// judges holds a commit, which a speculative base does while the first fence is
    /// pending.
    struct Broken;

    impl crate::guard::CommitGuard for Broken {
        fn check(
            &self,
            c: &crate::guard::Candidate<'_>,
        ) -> Result<crate::guard::ValidationSummary> {
            use crate::guard::{GuardMode, GuardStatus, Severity, ValidationSummary};
            if c.base.commit >= 1 {
                return Err(Error::Invalid("the shapes no longer parse".into()));
            }
            Ok(ValidationSummary::empty(
                GuardStatus::Passed,
                GuardMode::Reject,
                Severity::Violation,
            ))
        }
        fn describe(&self) -> String {
            "broken".into()
        }
        fn speculative(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_guard_error_other_than_rejection_waits() {
        for fence_fails in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = Arc::new(Store::open(dir.path(), options()).unwrap());
            s.set_guard(Some(Arc::new(Broken)));
            let (hit, go) = pause(&s, "group-sealed");
            let first = {
                let s = s.clone();
                thread::spawn(move || insert(&s, "one"))
            };
            hit.recv_timeout(Duration::from_secs(5)).unwrap();
            let (done, received) = mpsc::channel();
            let next = {
                let s = s.clone();
                thread::spawn(move || done.send(insert(&s, "two")).unwrap())
            };
            // the guard failed against a state that is not durable yet
            assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
            if fence_fails {
                s.fail_next_group_sync();
            }
            go.send(()).unwrap();
            let answer = received.recv_timeout(Duration::from_secs(5)).unwrap();
            if fence_fails {
                assert!(matches!(answer, Err(Error::Poisoned)), "{answer:?}");
                assert!(first.join().unwrap().is_err());
            } else {
                assert!(matches!(answer, Err(Error::Invalid(_))), "{answer:?}");
                assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
            }
            next.join().unwrap();
        }
    }

    /// A view that hides `urn:p` on subjects of `urn:Hidden`, which a write may not touch.
    fn hidden_class_access() -> Arc<crate::access::GraphAccess> {
        use crate::access::{Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules};
        let protection = Protection {
            name: "hidden".into(),
            predicates: Some(vec!["urn:p".into()]),
            classes: Some(vec!["urn:Hidden".into()]),
            subclasses: true,
            graphs: None,
            pattern: None,
            prefixes: Default::default(),
            hide_inferences: false,
        };
        Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(protection),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        ))
    }

    #[test]
    fn a_class_protection_refusal_waits_for_the_durable_head() {
        for fence_fails in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = Arc::new(Store::open(dir.path(), options()).unwrap());
            let (hit, go) = pause(&s, "group-sealed");
            // commit 1 makes `urn:s` a protected subject, and is not durable yet
            let first = {
                let s = s.clone();
                thread::spawn(move || {
                    crate::sparql::update::update(
                        &s,
                        "INSERT DATA { <urn:s> a <urn:Hidden> }",
                        &Default::default(),
                    )
                })
            };
            hit.recv_timeout(Duration::from_secs(5)).unwrap();
            let (done, received) = mpsc::channel();
            let next = {
                let s = s.clone();
                thread::spawn(move || {
                    let opts = crate::sparql::QueryOptions {
                        graphs: Some(hidden_class_access()),
                        ..Default::default()
                    };
                    done.send(crate::sparql::update::update(
                        &s,
                        "INSERT DATA { <urn:s> <urn:p> \"x\" }",
                        &opts,
                    ))
                    .unwrap()
                })
            };
            // refused for a subject that only the pending commit makes protected
            assert!(received.recv_timeout(Duration::from_millis(200)).is_err());
            if fence_fails {
                s.fail_next_group_sync();
            }
            go.send(()).unwrap();
            let answer = received.recv_timeout(Duration::from_secs(5)).unwrap();
            if fence_fails {
                assert!(matches!(answer, Err(Error::Poisoned)), "{answer:?}");
                assert!(first.join().unwrap().is_err());
            } else {
                assert!(matches!(answer, Err(Error::NotPermitted(_))), "{answer:?}");
                first.join().unwrap().unwrap();
            }
            next.join().unwrap();
        }
    }

    #[test]
    fn a_patch_syntax_error_is_reported_while_a_fence_runs() {
        use crate::store::PatchOptions;
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one"))
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        // the error says nothing about the data, so it does not wait for the fence
        let opts = PatchOptions {
            write: crate::guard::WriteOptions {
                no_wait: true,
                ..Default::default()
            },
            binary: false,
        };
        let answer = s.apply_patch(&b"BOGUS\n"[..], &opts);
        assert!(matches!(answer, Err(Error::Patch(_))), "{answer:?}");
        go.send(()).unwrap();
        assert_eq!(first.join().unwrap().unwrap().commit.seq, 1);
    }

    /// A guard that keeps one state: it must be told of every commit before the next is
    /// checked.
    struct Settled {
        told: AtomicU64,
        stale: AtomicU64,
    }

    impl crate::guard::CommitGuard for Settled {
        fn check(
            &self,
            c: &crate::guard::Candidate<'_>,
        ) -> Result<crate::guard::ValidationSummary> {
            use crate::guard::{GuardMode, GuardStatus, Severity, ValidationSummary};
            if self.told.load(Ordering::SeqCst) != c.base.commit {
                self.stale.fetch_add(1, Ordering::SeqCst);
            }
            Ok(ValidationSummary::empty(
                GuardStatus::Passed,
                GuardMode::Reject,
                Severity::Violation,
            ))
        }
        fn committed(&self, seq: u64) {
            self.told.store(seq, Ordering::SeqCst);
        }
        fn describe(&self) -> String {
            "settled".into()
        }
    }

    #[test]
    fn a_guard_that_keeps_one_state_is_told_of_each_commit_before_the_next_check() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let guard = Arc::new(Settled {
            told: AtomicU64::new(0),
            stale: AtomicU64::new(0),
        });
        s.set_guard(Some(guard.clone()));
        let workers: Vec<_> = (0..4)
            .map(|t| {
                let s = s.clone();
                thread::spawn(move || {
                    for k in 0..15 {
                        insert(&s, &format!("w{t}-{k}")).unwrap();
                    }
                })
            })
            .collect();
        for w in workers {
            w.join().unwrap();
        }
        assert_eq!(s.snapshot().commit, 60);
        assert_eq!(guard.told.load(Ordering::SeqCst), 60);
        assert_eq!(guard.stale.load(Ordering::SeqCst), 0);
    }

    /// A closed store whose generation's `commit.json` is edited by `edit`.
    fn edited_commit_json(
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let store = Store::open(&root, Default::default()).unwrap();
        insert(&store, "one").unwrap();
        drop(store);
        let path = wal_path(&root).parent().unwrap().join("commit.json");
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        edit(&mut v);
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        (dir, root, path)
    }

    fn fork_of(root: &Path) -> commit::ForkedFrom {
        commit::ForkedFrom {
            id: commit::read_dataset_file(root).unwrap().unwrap(),
            seq: 1,
        }
    }

    #[test]
    fn reidentify_refuses_a_newer_commit_json_format() {
        let (_dir, root, path) = edited_commit_json(|v| v["format"] = 2.into());
        let before = std::fs::read(&path).unwrap();
        let e = commit::reidentify(&root, uuid::Uuid::new_v4(), fork_of(&root)).unwrap_err();
        assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn rewriting_commit_json_keeps_unknown_members() {
        let (_dir, root, path) = edited_commit_json(|v| {
            v["futureMember"] = 7.into();
            v["commit"]["futureCommitMember"] = "x".into();
        });
        let member = |p: &Path| {
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            (
                v.get("futureMember").cloned(),
                v["commit"].get("futureCommitMember").cloned(),
            )
        };
        let kept = (Some(7.into()), Some("x".into()));
        commit::reidentify(&root, uuid::Uuid::new_v4(), fork_of(&root)).unwrap();
        assert_eq!(member(&path), kept);
    }
}
