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
    #[cfg(test)]
    fences: AtomicUsize,
}

impl Coordinator {
    pub fn failed(&self) -> bool {
        self.state.lock().failed
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
            let mut s = self.state.lock();
            if ticket.0.load(Ordering::Acquire) == 0 {
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
        let synced = sync_commit_reused(
            &last.wal,
            &last.snapshot.generation.dvocab,
            &mut self.vocab.lock(),
        );
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
                s.commits -= batch.len();
                s.bytes -= batch.iter().map(|p| p.bytes).sum::<u64>();
                Ok(())
            }
            Err(e) => {
                Self::fail_locked(&mut s);
                Err(e)
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
    /// Publish only a successfully fenced prefix, in commit order. Aux indexes and
    /// annotation/digest/guard paths are excluded at admission and drain first.
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
                commit: changelog::ChangeCommit::of(&c, p.author.clone(), None),
                body: changelog::PendingBody::Log {
                    generation: generation.clone(),
                    changes: p.changes.clone(),
                },
            });
        }
        self.catalog.lock().append(c);
        self.compaction.committed(c.timestamp_ms);
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

    pub(super) fn publish_grouped(&mut self) -> Result<Receipt> {
        let store = self.store;
        let group = store.group.as_ref().expect("grouped admission enabled");
        // The commit record counts the commits before it that may not be durable yet
        // in one byte, which a full queue keeps far below its limit.
        if group.full(self.wal_bytes()) || self.unfenced(store) >= MAX_UNFENCED {
            store.drain_group(&mut self.guard, Some(&self.opts))?;
        }
        // Publication only lowers the count, so reading it early stays conservative.
        let unfenced = u8::try_from(self.unfenced(store)).expect("drained above");
        self.check_storage()?;
        let generation = self.base.generation.clone();
        let c = self.next_commit(None);
        let bytes = self.wal_bytes();
        let prealloc = self.wal_prealloc();
        // the terms are in the file before the records that name them
        let vocab_before = generation.dvocab.allocated();
        generation.dvocab.set_prealloc(prealloc);
        if generation.dvocab.needs_sync() {
            generation.dvocab.flush()?;
        }
        let w = &mut **self.guard;
        let wal = w.wal.as_mut().expect("persistent grouped transaction");
        // The descriptor clone fails before any byte of this transaction is written.
        let owned_wal = wal.get_ref().try_clone()?;
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
            0,
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
        group.wait(store, &ticket)?;
        Ok(Receipt {
            dataset_id: store.owner_dataset_id(),
            committed: true,
            commit: c,
            validation: None,
            annotation: Default::default(),
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
    fn message_fallback_waits_and_keeps_its_annotation() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one").unwrap())
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let (done, received) = mpsc::channel();
        let second = {
            let s = s.clone();
            thread::spawn(move || {
                let mut t = s.write_with(
                    CommitKind::Update,
                    crate::guard::WriteOptions {
                        message: Some("kept message".into()),
                        ..Default::default()
                    },
                );
                let q = Quad::new(
                    NamedNode::new("urn:two").unwrap(),
                    NamedNode::new("urn:p").unwrap(),
                    NamedNode::new("urn:o").unwrap(),
                    GraphName::DefaultGraph,
                );
                let q = t.encode_quad(&q, &mut Default::default()).unwrap();
                t.insert(q).unwrap();
                done.send(t.commit()).unwrap();
            })
        };
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        go.send(()).unwrap();
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .commit
                .seq,
            2
        );
        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(
            s.annotation(2).unwrap().message.as_deref(),
            Some("kept message")
        );
        assert_eq!(s.group.as_ref().unwrap().fences.load(Ordering::Relaxed), 1);
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

    #[test]
    fn precondition_fallback_observes_only_a_durable_head() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), options()).unwrap());
        let (hit, go) = pause(&s, "group-sealed");
        let first = {
            let s = s.clone();
            thread::spawn(move || insert(&s, "one").unwrap())
        };
        hit.recv_timeout(Duration::from_secs(5)).unwrap();
        let checked = Arc::new(AtomicBool::new(false));
        let opts = crate::guard::WriteOptions {
            precondition: Some(crate::guard::Precondition::new({
                let checked = checked.clone();
                move |snap| {
                    assert_eq!(snap.commit, 1);
                    assert_eq!(snap.len(), 1);
                    checked.store(true, Ordering::Relaxed);
                    Ok(())
                }
            })),
            ..Default::default()
        };
        let (done, received) = mpsc::channel();
        let next = {
            let s = s.clone();
            thread::spawn(move || {
                done.send(s.try_write_with(CommitKind::Update, opts).unwrap().commit())
                    .unwrap()
            })
        };
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(!checked.load(Ordering::Relaxed));
        go.send(()).unwrap();
        assert!(
            !received
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .committed
        );
        next.join().unwrap();
        first.join().unwrap();
        assert!(checked.load(Ordering::Relaxed));
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
            if rec[0] == WAL_COMMIT {
                out.push(start..i + 1);
                start = i + 1;
            }
        }
        out
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
}
