//! Opt-in caller-led durable prefix publication. No collection delay: the first
//! caller seals immediately; subsequent callers can append during that fence.
//! A publisher never takes the writer mutex. General writer operations drain the
//! coordinator before inspecting the writer's private speculative head.
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

pub(super) struct Pending {
    pub info: CommitInfo,
    pub snapshot: Arc<Snapshot>,
    pub changes: Vec<(u8, [Id; 4])>,
    pub author: Option<Arc<str>>,
    pub wal: File,
    pub wal_end: u64,
    pub bytes: u64,
    pub ticket: Arc<Ticket>,
    /// what the write guard found, told to the guard once the commit is published
    pub validation: Option<Arc<crate::guard::ValidationSummary>>,
    /// the commit's message and change digest
    pub annotation: crate::annotations::Annotation,
    /// the annotations file, when this commit's record in it is not synced yet
    pub ann_file: Option<Arc<File>>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Pending>,
    active: bool,
    inflight: Vec<Arc<Ticket>>,
    failed: bool,
    commits: usize,
    bytes: u64,
}

#[derive(Default)]
pub(super) struct Coordinator {
    state: Mutex<State>,
    changed: Condvar,
    vocab: Mutex<crate::vocab::LazySync>,
    /// the next fence's synchronization fails, as a failed `fdatasync` would
    #[cfg(any(test, feature = "failpoints"))]
    fail_next_sync: AtomicBool,
    #[cfg(test)]
    fences: AtomicUsize,
}

impl Coordinator {
    pub fn failed(&self) -> bool {
        self.state.lock().failed
    }

    /// No fence runs and nothing is queued, so a commit made now would be the only one
    /// in its group.
    pub fn idle(&self) -> bool {
        let s = self.state.lock();
        !s.active && s.queue.is_empty() && !s.failed
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

    /// A later transaction's WAL write failed. No fence runs again, so the queued
    /// commits fail. A prefix whose fence is already running keeps that fence's
    /// outcome: when the sync succeeds, its commits are durable and are acknowledged.
    pub fn fail(&self) {
        let mut s = self.state.lock();
        s.failed = true;
        Self::fail_queue(&mut s);
        self.changed.notify_all();
    }

    fn fail_locked(s: &mut State) {
        s.failed = true;
        for ticket in &s.inflight {
            let _ = ticket
                .0
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        }
        Self::fail_queue(s);
        s.commits = 0;
        s.bytes = 0;
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
        s.inflight.iter().any(|t| std::ptr::eq(&**t, ticket))
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
            if ticket.0.load(Ordering::Acquire) == 0 && s.active {
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
                    if !s.active {
                        return Err(Error::Poisoned);
                    }
                    self.changed.wait_for(&mut s, Duration::from_millis(20));
                    continue;
                }
                if !s.active && s.queue.is_empty() {
                    return Ok(());
                }
                if o.is_some_and(|o| o.no_wait) {
                    return Err(Error::WriterBusy);
                }
            }
            if self.drive(store, None)? {
                continue;
            }
            let mut s = self.state.lock();
            if s.active {
                self.changed.wait_for(&mut s, Duration::from_millis(20));
            }
        }
    }

    /// Seal exactly the ready prefix. Appends during the fence belong to the next
    /// fence even if the kernel happened to sync their bytes too.
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
            if s.active || s.queue.is_empty() {
                return Ok(false);
            }
            s.active = true;
            let batch: Vec<_> = s.queue.drain(..).collect();
            s.inflight = batch.iter().map(|p| p.ticket.clone()).collect();
            batch
        };
        let mut leader = Leader {
            group: self,
            complete: false,
        };
        store.failpoint("group-sealed");
        let last = batch.last().expect("nonempty sealed prefix");
        let started = std::time::Instant::now();
        #[cfg(any(test, feature = "failpoints"))]
        let injected = self.fail_next_sync.swap(false, Ordering::AcqRel);
        #[cfg(not(any(test, feature = "failpoints")))]
        let injected = false;
        let ann_file = batch.iter().rev().find_map(|p| p.ann_file.clone());
        let sync_log = || {
            sync_commit_reused(
                &last.wal,
                &last.snapshot.generation.dvocab,
                &mut self.vocab.lock(),
            )
        };
        let synced = if injected {
            Err(std::io::Error::from_raw_os_error(5).into())
        } else if let Some(ann) = ann_file {
            // the annotations of the commits are synced with their records and terms
            std::thread::scope(|sc| {
                let annotations = std::thread::Builder::new()
                    .name("annotations-sync".into())
                    .spawn_scoped(sc, || ann.sync_data());
                let log = sync_log();
                let annotations = match annotations {
                    Ok(h) => h.join().unwrap_or_else(|_| {
                        Err(std::io::Error::other("the annotations sync panicked"))
                    }),
                    Err(_) => ann.sync_data(),
                };
                log.and(annotations.map_err(Error::from))
            })
        } else {
            sync_log()
        };
        store
            .commit_stats
            .synced(SyncPath::Group, started.elapsed());
        #[cfg(test)]
        self.fences.fetch_add(1, Ordering::Relaxed);
        store.failpoint("group-synced");
        let mut s = self.state.lock();
        let outcome = match synced {
            // A later write may have failed during the fence. That poisons the
            // writer and fails the queue, but this prefix is durable all the same.
            Ok(()) => {
                // Keep failure/admission state serialized with publication. The
                // writer itself remains free, including while a later txn is held.
                for p in &batch {
                    store.publish_group_prefix(p);
                    p.ticket.0.store(1, Ordering::Release);
                    store.failpoint("group-published");
                }
                store.commit_stats.fenced(batch.len());
                s.commits -= batch.len();
                s.bytes -= batch.iter().map(|p| p.bytes).sum::<u64>();
                Ok(())
            }
            // Every commit of the fence reports the same failure, the one that led it
            // included. The cause is logged, since the error says only that the writer
            // is poisoned.
            Err(e) => {
                store.commit_stats.failed();
                Self::fail_locked(&mut s);
                tracing::error!(
                    target: "sparkles::store",
                    error = %e,
                    commits = batch.len(),
                    first = batch[0].info.seq,
                    last = last.info.seq,
                    "group commit fence failed to synchronize; writes are refused until the store is reopened"
                );
                Err(Error::Poisoned)
            }
        };
        s.inflight.clear();
        s.active = false;
        leader.complete = true;
        self.changed.notify_all();
        outcome.map(|_| true)
    }
}

struct Leader<'a> {
    group: &'a Coordinator,
    complete: bool,
}
impl Drop for Leader<'_> {
    fn drop(&mut self) {
        if !self.complete {
            let mut s = self.group.state.lock();
            Coordinator::fail_locked(&mut s);
            s.inflight.clear();
            s.active = false;
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

    /// Publish only a successfully fenced prefix, in commit order. Auxiliary indexes
    /// remain excluded at admission and drain first. Grouped guard notifications run
    /// before publication.
    fn publish_group_prefix(&self, p: &Pending) {
        let c = p.info;
        let generation = &p.snapshot.generation;
        self.wal_end.store(p.wal_end, Ordering::Release);
        if let Some(ix) = generation.wal_index.lock().as_mut() {
            ix.note(wal::WalPoint {
                seq: c.seq,
                offset: p.wal_end,
                folding: false,
            });
        }
        if let Some(log) = self.changelog.as_ref().filter(|l| l.is_enabled()) {
            log.push(changelog::Pending {
                commit: changelog::ChangeCommit::of(
                    &c,
                    p.author.clone(),
                    p.annotation.message.clone(),
                ),
                body: changelog::PendingBody::Log {
                    generation: generation.clone(),
                    changes: p.changes.clone(),
                },
            });
        }
        self.catalog.lock().append(c);
        self.compaction.committed(c.timestamp_ms);
        // The guard learns of the commit before anyone can see it published, so a writer
        // that waited for the publication checks against the state this commit left.
        if p.validation.is_some() {
            self.guard_committed(c.seq);
        }
        self.current.store(p.snapshot.clone());
        self.commits.send_replace(c.seq);
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
    ) -> Result<Receipt> {
        let store = self.store;
        let group = store.group.as_ref().expect("grouped admission enabled");
        let message = match &self.opts.message {
            Some(m) => crate::annotations::validate_message(m)?,
            None => None,
        };
        // The commit record counts the commits before it that may not be durable yet
        // in one byte, which a full queue keeps far below its limit.
        if group.full(self.wal_bytes()) || self.unfenced(store) >= MAX_UNFENCED {
            store.drain_group(&mut self.guard, Some(&self.opts))?;
        }
        // Publication only lowers the count, so reading it early stays conservative.
        let unfenced = u8::try_from(self.unfenced(store)).expect("drained above");
        self.check_storage()?;
        let generation = self.base.generation.clone();
        let c = self.next_commit(validation.as_deref());
        let bytes = self.wal_bytes();
        let prealloc = self.wal_prealloc();
        // the terms are in the file before the records that name them
        let vocab_before = generation.dvocab.allocated();
        generation.dvocab.set_prealloc(prealloc);
        if generation.dvocab.needs_sync() {
            generation.dvocab.flush()?;
        }
        // The descriptor clone fails before any byte of this transaction is written.
        let owned_wal = self
            .guard
            .wal
            .as_ref()
            .expect("persistent grouped transaction")
            .get_ref()
            .try_clone()?;
        // The annotation record is written, not synced, before the WAL records: the fence
        // syncs both files, and the commit record says an annotation is expected. Nothing
        // that can fail comes between this and the WAL write, and a failed write poisons
        // the writer, so the record is never taken back: if the commit is recovered, its
        // annotation is there.
        let (annotation, ann_file) =
            store.annotate_unsynced(&c, message, || Some(self.change_lines()))?;
        let w = &mut **self.guard;
        let wal = w.wal.as_mut().expect("persistent grouped transaction");
        let mut data = Vec::with_capacity(bytes as usize);
        let mut rec = [0u8; WAL_REC];
        for (op, q) in &self.log {
            rec[0] = *op;
            for j in 0..4 {
                rec[1 + j * 8..9 + j * 8].copy_from_slice(&q[j].0.to_le_bytes());
            }
            data.extend_from_slice(&rec);
        }
        rec[0] = WAL_COMMIT;
        rec[1..9].copy_from_slice(&w.next_bnode.to_le_bytes());
        commit::seal_wal_commit_unfenced(
            &mut rec,
            c.seq,
            c.timestamp_ms,
            c.kind,
            (if c.unvalidated {
                commit::WAL_FLAG_UNVALIDATED
            } else {
                0
            }) | (if ann_file.is_some() {
                commit::WAL_FLAG_ANNOTATED
            } else {
                0
            }),
            unfenced,
            &data,
        );
        data.extend_from_slice(&rec);
        let before = w.wal_alloc;
        if let Err(e) = wal.flush().and_then(|_| {
            wal::write_commit(wal.get_ref(), w.wal_len, &mut w.wal_alloc, &data, prealloc)
        }) {
            w.poisoned = true;
            group.fail();
            return Err(e.into());
        }
        store.quota.add(w.wal_alloc - before);
        store
            .quota
            .add(generation.dvocab.allocated().saturating_sub(vocab_before));
        w.wal_len += bytes;
        store.quota.set_preallocated(w.wal_alloc - w.wal_len);
        store
            .quota
            .set_vocab_preallocated(generation.dvocab.preallocated());
        let snapshot = Arc::new(Snapshot {
            dataset_id: self.base.dataset_id,
            generation: generation.clone(),
            delta: std::mem::take(&mut self.delta),
            version: self.base.version + 1,
            cache: self.base.cache.clone(),
            results: self.base.results.clone(),
            dvocab_len: generation.dvocab.len(),
            commit: c.seq,
            text: None,
            geo: None,
            union_default_graph: self.base.union_default_graph,
            geo_op_vertices: self.base.geo_op_vertices,
            counts: Default::default(),
            mask: None,
            historical: false,
            change_log: self.base.change_log.clone(),
        });
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
                });
            } else {
                w.tap = None;
            }
        }
        let ticket = Arc::new(Ticket::default());
        let pending = Pending {
            info: c,
            snapshot: snapshot.clone(),
            changes: std::mem::take(&mut self.log),
            author: self.opts.author.clone(),
            wal: owned_wal,
            wal_end: w.wal_len,
            bytes,
            ticket: ticket.clone(),
            validation: validation.clone(),
            annotation: annotation.clone(),
            ann_file,
        };
        w.head = c;
        w.staged = Some(snapshot);
        if let Err(e) = group.enqueue(pending) {
            w.poisoned = true;
            return Err(e);
        }
        // Nothing below accesses the writer guard. Waiting keeps Store borrowed,
        // but releases the single writer for later speculative transactions.
        drop(self.guard.0.take());
        store.failpoint("group-appended");
        let queued = std::time::Instant::now();
        group.wait(store, &ticket)?;
        store.commit_stats.waited(queued.elapsed());
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
            experimental_group_commit: true,
            group_commit_idle_direct: false,
            ..Default::default()
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
    /// A waiter whose fence completes between its own `drive` and its park must lead the
    /// next fence at once, not sleep out the 20 ms poll with nobody leading.
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
                move || {
                    insert(&s, "b").unwrap();
                    Instant::now()
                }
            })
            .unwrap();
        parked.recv_timeout(Duration::from_secs(5)).unwrap();
        release_a.send(()).unwrap();
        a.join().unwrap();
        let resumed = Instant::now();
        release_b.send(()).unwrap();
        let done = b.join().unwrap();
        let stalled = done.duration_since(resumed);
        assert!(
            stalled < Duration::from_millis(10),
            "the waiter slept {stalled:?} with its commit queued and no fence running"
        );
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
        if waits {
            assert!(
                matches!(first, AbortEvent::Draining),
                "checked abort must wait: {check:?}, {first:?}"
            );
            assert_eq!(completed, no_wait);
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
        if waits && no_wait {
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
    abort_case!(
        plain_patch_abort_no_wait_reports_published_success,
        Plain,
        false,
        true,
        false
    );
    abort_case!(
        plain_patch_abort_no_wait_reports_published_failure,
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
        let s = Store::open(dir.path(), Default::default()).unwrap();
        assert!(s.group.is_none());
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
    fn the_only_writer_commits_directly_and_a_contended_commit_groups() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(
            Store::open(
                dir.path(),
                StoreOptions {
                    experimental_group_commit: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        // alone, a commit does not wait for a fence
        for n in 0..3 {
            assert_eq!(insert(&s, &format!("alone{n}")).unwrap().commit.seq, n + 1);
        }
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 0);
        assert_eq!(synced(&s, SyncPath::Group), 0);
        assert_eq!(
            synced(&s, SyncPath::Direct) + synced(&s, SyncPath::Buffered),
            3
        );
        assert_eq!(s.commit_metrics().size.count(), 0);
        // a commit that finds a writer waiting for the lock joins a group
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
        // the second writer starts while the first one's fence runs, so it groups as well
        // unless that fence has finished
        let fences = s.group.as_ref().unwrap().fences.load(Ordering::Relaxed);
        assert!((1..=2).contains(&fences), "{fences} fences");
        assert_eq!(synced(&s, SyncPath::Group), fences as u64);
        assert_eq!(s.commit_metrics().size.count(), fences as u64);
        assert!(s.commit_metrics().wait.count() >= 1);
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
        // a store without group commit counts nothing
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), Default::default()).unwrap();
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
        // The next transaction's WAL write fails: its descriptor is read-only.
        let read_only = File::open(wal_path(dir.path())).unwrap();
        {
            let mut w = s.writer.lock();
            w.wal = Some(BufWriter::new(read_only));
            w.wal_direct = Default::default();
        }
        assert!(insert(&s, "failed").is_err());
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
        drop(s);
        // As with ordinary failed sync, complete but unacknowledged WAL records
        // may recover. Nothing that returned success can disappear.
        let s = Store::open(dir.path(), Default::default()).unwrap();
        assert_eq!(s.snapshot().commit, 2);
        assert_eq!(s.snapshot().len(), 2);
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
        s.group.as_ref().unwrap().state.lock().queue[0].wal = File::open("/dev/null").unwrap();
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
                    experimental_group_commit: enabled,
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
        s.group.as_ref().unwrap().state.lock().queue[0].wal = File::open("/dev/null").unwrap();
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
        let buf = std::fs::read(wal_path(&root)).unwrap();
        assert_eq!(&buf[..WAL_REC], &wal::header_record());
        for header in [true, false] {
            let case = dir.path().join(if header { "header" } else { "legacy" });
            copy_dir(&root, &case);
            let mut bad = if header {
                buf.clone()
            } else {
                buf[WAL_REC..].to_vec()
            };
            let txns = transactions(&bad);
            assert_eq!(txns.len(), 2);
            let first = &txns[0];
            bad[first.start * WAL_REC + 5] ^= 0x55;
            if header {
                assert_eq!(&bad[..WAL_REC], &wal::header_record());
                assert_eq!(first.start, 1);
            } else {
                assert_eq!(first.start, 0);
            }
            std::fs::write(wal_path(&case), &bad).unwrap();
            assert_eq!(wal_check(&case), crate::check::Status::Error);
            let error = Store::open(&case, Default::default()).err().unwrap();
            assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
            assert!(error.to_string().contains("checksum mismatch"), "{error}");
        }
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

    fn torn_metadata_case(header: bool, hole: bool, later_proof: bool) {
        let (dir, root, buf) = metadata_fixture();
        let mut buf = if header { buf } else { buf[WAL_REC..].to_vec() };
        let txns = transactions(&buf);
        // Only the header establishes framing. A preceding v2 record in the headerless
        // case must not remove the legacy interpretation of later records.
        let damaged = usize::from(!header);
        let prefix = damaged as u64;
        let partial = damaged + usize::from(hole);
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
        if !header {
            if hole {
                assert_eq!(wal_check(&case), crate::check::Status::Error);
                assert!(matches!(
                    Store::open(&case, Default::default()),
                    Err(Error::Corrupt(_))
                ));
            } else {
                // With no framing, CI 5.6 requires this byte-identical record to keep
                // its legacy interpretation even after a verified modern commit.
                let store = Store::open(&case, Default::default()).unwrap();
                let head = partial + 1 + usize::from(later_proof);
                assert_eq!(store.snapshot().commit, head as u64);
                assert_eq!(store.snapshot().len(), head as u64);
                assert_eq!(wal_check(&case), crate::check::Status::Ok);
            }
            return;
        }
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
        if prefix == 1 {
            assert_eq!(store.annotation(1).unwrap().message.as_deref(), Some("one"));
        }
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
        torn_metadata_case(true, true, false);
    }

    #[test]
    fn torn_metadata_in_a_headerless_log_preserves_legacy_durability_proof() {
        torn_metadata_case(false, true, false);
    }

    #[test]
    fn torn_metadata_after_a_header_is_not_reconstructed() {
        torn_metadata_case(true, false, false);
    }

    #[test]
    fn torn_metadata_in_a_headerless_log_keeps_legacy_reconstruction() {
        torn_metadata_case(false, false, false);
    }

    #[test]
    fn torn_metadata_with_a_later_verified_proof_stays_corruption() {
        for header in [false, true] {
            for hole in [false, true] {
                torn_metadata_case(header, hole, true);
            }
        }
    }

    #[test]
    fn torn_metadata_without_any_modern_boundary_stays_conservative() {
        let (dir, root, buf) = metadata_fixture();
        let mut buf = buf[WAL_REC..].to_vec();
        let txns = transactions(&buf);
        zero_commit_metadata(&mut buf, &txns[0]);
        buf[txns[1].start * WAL_REC..txns[1].end * WAL_REC].fill(0);
        zero_commit_metadata(&mut buf, &txns[2]);
        buf.truncate(txns[2].end * WAL_REC);
        // These bytes are equally a real legacy commit or a first modern commit
        // whose metadata was lost. With no boundary, preserve strict legacy refusal.
        let case = dir.path().join("legacy");
        copy_dir(&root, &case);
        std::fs::write(wal_path(&case), &buf).unwrap();
        assert_eq!(wal_check(&case), crate::check::Status::Error);
        assert!(matches!(
            Store::open(&case, Default::default()),
            Err(Error::Corrupt(_))
        ));
    }

    #[test]
    fn torn_metadata_scan_preserves_headerless_legacy_proof() {
        let mut buf = vec![0; WAL_REC];
        let mut data = [0; WAL_REC];
        data[0] = WAL_INSERT;
        let mut rec = [0; WAL_REC];
        rec[0] = WAL_COMMIT;
        commit::seal_wal_commit_unfenced(&mut rec, 2, 0, CommitKind::Update, 0, 1, &data);
        buf.extend_from_slice(&data);
        buf.extend_from_slice(&rec);
        let mut partial = [0; WAL_REC];
        partial[0] = WAL_COMMIT;
        buf.extend_from_slice(&partial);
        assert!(!wal_torn_from(&buf, 0, 1, false));
        assert!(wal_torn_from(&buf, 0, 1, true));
        let mut proof = [0; WAL_REC];
        proof[0] = WAL_COMMIT;
        commit::seal_wal_commit(&mut proof, 4, 0, CommitKind::Update, 0, &[]);
        buf.extend_from_slice(&proof);
        assert!(!wal_torn_from(&buf, 0, 1, true));
        // A version byte alone is not proof when its CRC fails.
        *buf.last_mut().unwrap() ^= 0x55;
        assert!(wal_torn_from(&buf, 0, 1, true));
    }

    fn mixed_headerless_history(resumed: bool) {
        let (dir, root, buf) = metadata_fixture();
        let mut buf = buf[WAL_REC..].to_vec();
        let txns = transactions(&buf);
        let legacy = (txns[1].end - 1) * WAL_REC;
        // The pinned pre-identity writer kept opcode and next_bnode and wrote zeros
        // in the spare metadata bytes. Its real distinct quad data remains intact.
        buf[legacy + 9..legacy + WAL_REC].fill(0);
        let head = if resumed { 3 } else { 2 };
        buf.truncate(txns[head - 1].end * WAL_REC);
        let case = dir.path().join("mixed");
        copy_dir(&root, &case);
        std::fs::write(wal_path(&case), &buf).unwrap();
        // The older writer did not append catalog metadata for its transaction. Keep
        // only the header, root and first modern commit so reconstruction is visible.
        OpenOptions::new()
            .write(true)
            .open(case.join("commits.bin"))
            .unwrap()
            .set_len((3 * commit::REC) as u64)
            .unwrap();
        let store = Store::open(&case, Default::default()).unwrap();
        assert_eq!(store.snapshot().commit, head as u64);
        assert_eq!(store.snapshot().len(), head as u64);
        let legacy = store.catalog.lock().get(2).unwrap();
        let parent = store.catalog.lock().get(1).unwrap();
        assert_eq!(legacy.kind, CommitKind::Unknown);
        assert!(legacy.reconstructed);
        assert_eq!(legacy.timestamp_ms, parent.timestamp_ms);
        let verify = |store: &Store| {
            let snap = store.snapshot();
            for name in ["one", "two", "three"].into_iter().take(head) {
                let ids = [
                    snap.lookup_iri(&format!("urn:{name}")).unwrap(),
                    snap.lookup_iri("urn:p").unwrap(),
                    snap.lookup_iri("urn:o").unwrap(),
                    snap.lookup_iri("urn:g").unwrap(),
                ];
                assert!(snap.contains(&ids).unwrap(), "lost {name}");
            }
        };
        verify(&store);
        assert_eq!(wal_check(&case), crate::check::Status::Ok);
        drop(store);
        let store = Store::open(&case, Default::default()).unwrap();
        verify(&store);
        assert_eq!(store.snapshot().commit, head as u64);
        assert_eq!(insert(&store, "next").unwrap().commit.seq, head as u64 + 1);
        drop(store);
        let reopened = Store::open(&case, Default::default()).unwrap();
        verify(&reopened);
        assert_eq!(reopened.snapshot().commit, head as u64 + 1);
        let (historical, _) = reopened
            .snapshot_at(&crate::history::At::Commit(2), &Default::default())
            .unwrap();
        assert_eq!(historical.len(), 2);
        assert_eq!(std::fs::read(wal_path(&case)).unwrap()[0], WAL_INSERT);
    }

    #[test]
    fn mixed_headerless_v2_then_legacy_keeps_the_final_payload() {
        mixed_headerless_history(false);
    }

    #[test]
    fn mixed_headerless_v2_legacy_and_resumed_v2_keep_every_commit() {
        mixed_headerless_history(true);
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
        // Power loss now could keep any subset of the pages of commits 2 to 5,
        // which no sync has covered yet. Only commit 1 was acknowledged.
        let crashed = dir.path().join("crashed");
        copy_dir(&root, &crashed);
        go.send(()).unwrap();
        second.join().unwrap().unwrap();
        for t in later {
            t.join().unwrap().unwrap();
        }
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
        // the only writer of a reopened store commits directly, with a record that says
        // every commit before it is durable
        {
            let s = Store::open(
                &root,
                StoreOptions {
                    experimental_group_commit: true,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(insert(&s, "six").unwrap().commit.seq, 6);
            assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 0);
            assert_eq!(s.commit_metrics().sync[SyncPath::Group as usize].count(), 0);
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
                    experimental_group_commit: grouped,
                    group_commit_idle_direct: false,
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

    /// Commits 2 to 4 of a store with a message each. Commits 3 and 4 queue behind commit
    /// 2's fence, and the files are copied while the fence is held: the records of all
    /// three are in them, unsynced. Returns the directory of the copy and the store.
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
        (root, crashed)
    }

    #[test]
    fn a_commit_that_reached_the_disk_without_its_annotation_is_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let (_root, crashed) = annotated_suffix(dir.path());
        // the records of commits 3 and 4 never reached annotations.bin: 17 bytes each
        let path = crashed.join(crate::annotations::FILE);
        let len = std::fs::metadata(&path).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(len - 34)
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
        let first = bytes.len() - 4 * 17;
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
}
