//! Bounded derived-index recovery. A job owns no Store and queued jobs pin no snapshot.
use super::*;
use crate::text::{TextConfig, TextIndex, TextStatus, TextView};
use parking_lot::Condvar;
use std::sync::{OnceLock, Weak, mpsc};
use std::time::Duration;

const QUEUE: usize = 16;
const JOURNAL: usize = 131_072;
const FINAL_TAIL: usize = 1_024;
const ATTEMPTS: usize = 3;
const ROUNDS: usize = 8;

type IndexSlot = arc_swap::ArcSwapOption<TextIndex>;
type RecoverySlot = arc_swap::ArcSwapOption<Recovery>;

#[derive(Default)]
struct Journal {
    generation: Option<u64>,
    invalid: bool,
    quads: Vec<[Id; 4]>,
}

#[derive(Default)]
struct State {
    running: bool,
    done: bool,
    cancelled: bool,
    failure: Option<&'static str>,
}

pub(super) struct Recovery {
    root: PathBuf,
    config: TextConfig,
    owner: u64,
    active: AtomicBool,
    ready: AtomicBool,
    journal: Mutex<Journal>,
    state: Mutex<State>,
    done: Condvar,
    writer: Weak<Mutex<WriterState>>,
    current: Weak<ArcSwap<Snapshot>>,
    index: Weak<IndexSlot>,
    slot: Weak<RecoverySlot>,
    quota: Weak<quota::Quota>,
    reserve: Option<u64>,
}

impl Recovery {
    fn new(store: &Store, config: TextConfig) -> Arc<Self> {
        Arc::new(Self {
            root: store.root.clone().expect("persistent recovery"),
            config,
            owner: crate::text::next_owner(),
            active: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            journal: Default::default(),
            state: Default::default(),
            done: Condvar::new(),
            writer: Arc::downgrade(&store.writer),
            current: Arc::downgrade(&store.current),
            index: Arc::downgrade(&store.text),
            slot: Arc::downgrade(&store.text_recovery),
            quota: Arc::downgrade(&store.quota),
            reserve: store.opts.min_free_disk_bytes,
        })
    }

    pub(super) fn pending(&self) -> bool {
        !self.ready.load(Ordering::Acquire)
    }

    pub(super) fn view(&self, seq: u64) -> Arc<TextView> {
        TextView::recovering(seq, self.owner, self.state.lock().failure.is_some())
    }

    pub(super) fn record(&self, snapshot: &Snapshot, log: &[(u8, [Id; 4])]) {
        let mut j = self.journal.lock();
        if let Some(generation) = j.generation {
            if generation != snapshot.generation.uid
                || log.len() > JOURNAL.saturating_sub(j.quads.len())
            {
                j.invalid = true;
                j.quads.clear();
            } else if !j.invalid && self.active.load(Ordering::Acquire) {
                j.quads.extend(log.iter().map(|(_, q)| *q));
            }
        }
    }

    pub(super) fn changed_generation(&self) {
        let mut journal = self.journal.lock();
        journal.invalid = true;
        journal.quads.clear();
    }

    /// Stop recording before waiting. No Store writer admission is needed to cancel.
    pub(super) fn cancel_and_wait(&self) {
        let mut state = self.state.lock();
        self.active.store(false, Ordering::Release);
        state.cancelled = true;
        if !state.running {
            state.done = true;
        }
        self.done.notify_all();
        *self.journal.lock() = Journal::default();
        while state.running {
            self.done.wait(&mut state);
        }
    }

    pub(super) fn wait(&self) -> Result<()> {
        let mut state = self.state.lock();
        while !state.done {
            self.done.wait(&mut state);
        }
        if state.cancelled {
            return Err(Error::Cancelled);
        }
        match state.failure {
            Some(reason) => Err(Error::TextUnavailable(reason.into())),
            None => Ok(()),
        }
    }

    fn check(&self) -> Result<()> {
        if !self.active.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        match self.reserve {
            Some(r) => crate::disk::check_reserve(&self.root, r, 0, false),
            None => Ok(()),
        }
    }

    fn lock<'a>(
        &self,
        writer: &'a Mutex<WriterState>,
    ) -> Result<parking_lot::MutexGuard<'a, WriterState>> {
        loop {
            self.check()?;
            if let Some(w) = writer.try_lock_for(Duration::from_millis(10)) {
                if w.closed {
                    return Err(Error::Cancelled);
                }
                if w.poisoned {
                    return Err(Error::Poisoned);
                }
                return Ok(w);
            }
        }
    }

    fn owns(self: &Arc<Self>, slot: &RecoverySlot) -> Result<()> {
        self.check()?;
        if !slot.load().as_ref().is_some_and(|c| Arc::ptr_eq(c, self)) {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    fn run(self: &Arc<Self>) -> Result<()> {
        let writer = self.writer.upgrade().ok_or(Error::Cancelled)?;
        let current = self.current.upgrade().ok_or(Error::Cancelled)?;
        let slot = self.slot.upgrade().ok_or(Error::Cancelled)?;
        let index = self.index.upgrade().ok_or(Error::Cancelled)?;
        for _attempt in 0..ATTEMPTS {
            self.check()?;
            // Only an admitted worker retains a snapshot and reserves journal capacity.
            let start = {
                let _w = self.lock(&writer)?;
                self.owns(&slot)?;
                let snap = current.load_full();
                *self.journal.lock() = Journal {
                    generation: Some(snap.generation.uid),
                    invalid: false,
                    quads: Vec::with_capacity(JOURNAL),
                };
                snap
            };
            hook(&self.root, "admitted");
            let mut built =
                TextIndex::recovery_build(&self.root, &self.config, &start, &|| self.check())?;
            hook(&self.root, "built");
            let mut retry = false;
            for _round in 0..ROUNDS {
                let (head, changes) = {
                    let _w = self.lock(&writer)?;
                    self.owns(&slot)?;
                    let head = current.load_full();
                    let mut j = self.journal.lock();
                    if j.invalid || head.generation.uid != start.generation.uid {
                        retry = true;
                        break;
                    }
                    (
                        head,
                        std::mem::replace(&mut j.quads, Vec::with_capacity(JOURNAL)),
                    )
                };
                TextIndex::recovery_catch_up(&mut built, &self.config, &head, &changes, &|| {
                    self.check()
                })?;
                hook(&self.root, "caught-up");
                let _w = self.lock(&writer)?;
                self.owns(&slot)?;
                let head = current.load_full();
                let mut journal = self.journal.lock();
                if journal.invalid || head.generation.uid != start.generation.uid {
                    retry = true;
                    break;
                }
                if journal.quads.len() > FINAL_TAIL {
                    continue;
                }
                let tail = std::mem::take(&mut journal.quads);
                // The final writer admission bounds the residual record count; after
                // cancellation check the admitted directory durability fence completes.
                drop(journal);
                self.check()?;
                hook(&self.root, "publishing");
                TextIndex::recovery_catch_up(&mut built, &self.config, &head, &tail, &|| Ok(()))?;
                if let Some(quota) = self.quota.upgrade() {
                    quota.check_rebuild(
                        Some(&self.root.join("text")),
                        &self.root.join("text.new"),
                    )?;
                }
                let (ti, view) =
                    TextIndex::recovery_install(&self.root, self.config.clone(), built, &head)?;
                index.store(Some(Arc::new(ti)));
                let mut snapshot = (*head).clone();
                snapshot.text = Some(view);
                current.store(Arc::new(snapshot));
                self.ready.store(true, Ordering::Release);
                *self.journal.lock() = Journal::default();
                return Ok(());
            }
            drop(built);
            *self.journal.lock() = Journal::default();
            crate::text::cleanup_staging(&self.root)?;
            if !retry {
                return Err(Error::TextUnavailable(
                    "text recovery catch-up budget exceeded; retry rebuild".into(),
                ));
            }
        }
        Err(Error::TextUnavailable(
            "text recovery restart budget exceeded; retry rebuild".into(),
        ))
    }

    fn execute(self: Arc<Self>) {
        {
            let mut state = self.state.lock();
            if !self.active.load(Ordering::Acquire) {
                state.done = true;
                self.done.notify_all();
                return;
            }
            state.running = true;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run()));
        *self.journal.lock() = Journal::default();
        // A run retains the root capability indirectly: Store Drop/config change joins
        // it before releasing the Store lock or allowing another staging owner.
        match &result {
            Ok(Err(error)) => {
                tracing::warn!(target: "sparkles::text", root = %self.root.display(), %error, "background text recovery stopped")
            }
            Err(panic) => {
                let cause = panic
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("non-string panic");
                tracing::error!(target: "sparkles::text", root = %self.root.display(), %cause, "background text recovery panicked");
            }
            Ok(Ok(())) => {}
        }
        let cleanup = crate::text::cleanup_staging(&self.root);
        if let Err(error) = &cleanup {
            tracing::warn!(target: "sparkles::text", root = %self.root.display(), %error, "background text recovery cleanup failed");
        }
        if let Some(quota) = self.quota.upgrade() {
            quota.invalidate();
        }
        let mut state = self.state.lock();
        if (!matches!(result, Ok(Ok(()))) || cleanup.is_err())
            && self.active.load(Ordering::Acquire)
        {
            state.failure = Some("full-text recovery failed; retry rebuild");
        }
        state.running = false;
        state.done = true;
        self.done.notify_all();
    }

    pub(super) fn status(&self, seq: u64) -> TextStatus {
        let failed = self.state.lock().failure;
        TextStatus {
            enabled: true,
            state: if failed.is_some() {
                "failed"
            } else {
                "rebuilding"
            }
            .into(),
            docs: 0,
            seq,
            store_seq: seq,
            epoch: 0,
            disk_bytes: 0,
            segments: 0,
            config: self.config.clone(),
            format_version: 2,
            last_rebuild: None,
            message: failed.map(str::to_owned),
        }
    }
}

fn enqueue(job: &Arc<Recovery>) -> bool {
    static WORKER: OnceLock<Mutex<Option<mpsc::SyncSender<Weak<Recovery>>>>> = OnceLock::new();
    let mut worker = WORKER.get_or_init(Default::default).lock();
    if let Some(sender) = worker.as_ref() {
        return sender.try_send(Arc::downgrade(job)).is_ok();
    }
    let (tx, rx) = mpsc::sync_channel::<Weak<Recovery>>(QUEUE);
    if std::thread::Builder::new()
        .name("sparkles-text-recovery".into())
        .spawn(move || {
            while let Ok(weak) = rx.recv() {
                if let Some(job) = weak.upgrade() {
                    job.execute();
                }
            }
        })
        .is_err()
    {
        return false;
    }
    let queued = tx.try_send(Arc::downgrade(job)).is_ok();
    *worker = Some(tx);
    queued
}

impl Store {
    pub(super) fn register_text_recovery(&self, config: TextConfig) -> Arc<Recovery> {
        let job = Recovery::new(self, config);
        self.text_recovery.store(Some(job.clone()));
        // Retry enrollment changes only text metadata. Preserve a concurrent RDF
        // commit (or geo publication) instead of storing a cloned obsolete head.
        loop {
            let previous = self.current.load_full();
            let mut snapshot = (*previous).clone();
            snapshot.text = Some(job.view(snapshot.commit));
            hook(&job.root, "registered-snapshot");
            let observed = self.current.compare_and_swap(&previous, Arc::new(snapshot));
            if Arc::ptr_eq(&observed, &previous) {
                break;
            }
        }
        job
    }

    pub(super) fn enqueue_text_recovery(&self, job: &Arc<Recovery>) {
        if !enqueue(job) {
            let mut state = job.state.lock();
            state.failure = Some("full-text recovery queue unavailable; retry rebuild");
            state.done = true;
        }
    }

    pub(super) fn start_text_recovery(&self, config: TextConfig) -> Arc<Recovery> {
        let job = self.register_text_recovery(config);
        self.enqueue_text_recovery(&job);
        job
    }

    pub(super) fn stop_text_recovery(&self) {
        if let Some(job) = self.text_recovery.load_full() {
            job.cancel_and_wait();
        }
        self.text_recovery.store(None);
    }

    pub(super) fn retry_text_recovery(&self) -> Option<Arc<Recovery>> {
        let job = self.text_recovery.load_full()?;
        if !job.pending() {
            return None;
        }
        let config = job.config.clone();
        let done = job.state.lock().done;
        Some(if done {
            // Keep the old auxiliary slot installed until atomic replacement, so
            // no grouped commit can slip through an index/recovery-disabled gap.
            job.cancel_and_wait();
            hook(&job.root, "retry-replacing");
            self.start_text_recovery(config)
        } else {
            job
        })
    }
}

#[cfg(not(test))]
pub(crate) fn hook(_root: &Path, _name: &'static str) {}

#[cfg(test)]
pub(crate) fn hook(root: &Path, name: &'static str) {
    let f = hooks().lock().get(&(root.to_path_buf(), name)).cloned();
    if let Some(f) = f {
        f();
    }
}
#[cfg(test)]
type Hook = Arc<dyn Fn() + Send + Sync>;
#[cfg(test)]
fn hooks() -> &'static Mutex<BTreeMap<(PathBuf, &'static str), Hook>> {
    static HOOKS: OnceLock<Mutex<BTreeMap<(PathBuf, &'static str), Hook>>> = OnceLock::new();
    HOOKS.get_or_init(Default::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{RdfFormat, Source};
    use crate::sparql::{QueryOptions, query};
    use std::sync::mpsc;

    fn serial() -> parking_lot::MutexGuard<'static, ()> {
        static TEST: Mutex<()> = Mutex::new(());
        TEST.lock()
    }

    const QUERY: &str =
        "PREFIX text:<http://jena.apache.org/text#> SELECT ?s { ?s text:query \"fox\" }";
    fn populate(root: &Path) {
        let s = Store::open(root, Default::default()).unwrap();
        s.load(&[Source::from_bytes(
            br#"<http://ex/a> <http://www.w3.org/2000/01/rdf-schema#label> "fox"@en ."#.to_vec(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        s.enable_text(Default::default()).unwrap();
    }
    fn update(s: &Store, text: &str) {
        crate::sparql::update::update(s, text, &Default::default()).unwrap();
    }
    fn count(s: &Store) -> usize {
        query(s.snapshot(), QUERY, &Default::default())
            .unwrap()
            .rows()
            .len()
    }
    fn wait(s: &Store) {
        if let Some(job) = s.text_recovery.load_full() {
            job.wait().unwrap();
        }
        assert_eq!(s.text_status().unwrap().state, "ready");
    }

    struct Pause {
        root: PathBuf,
        name: &'static str,
        ready: mpsc::Receiver<()>,
        release: Option<mpsc::Sender<()>>,
    }
    impl Pause {
        fn new(root: &Path, name: &'static str) -> Self {
            let (notify, ready) = mpsc::channel();
            let (release, rx) = mpsc::channel();
            let rx = Mutex::new(rx);
            let once = AtomicBool::new(false);
            hooks().lock().insert(
                (root.to_path_buf(), name),
                Arc::new(move || {
                    if !once.swap(true, Ordering::SeqCst) {
                        notify.send(()).unwrap();
                        rx.lock().recv_timeout(Duration::from_secs(10)).unwrap();
                    }
                }),
            );
            Self {
                root: root.to_path_buf(),
                name,
                ready,
                release: Some(release),
            }
        }
        fn reached(&self) {
            self.ready.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        fn resume(&mut self) {
            self.release.take().unwrap().send(()).unwrap();
        }
    }
    impl Drop for Pause {
        fn drop(&mut self) {
            if let Some(tx) = self.release.take() {
                let _ = tx.send(());
            }
            hooks().lock().remove(&(self.root.clone(), self.name));
        }
    }

    #[test]
    fn automatic_recovery_quota_refusal_cleans_staging_and_can_retry() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let mut pause = Pause::new(root, "publishing");
        let store = Store::open(
            root,
            StoreOptions {
                max_disk_bytes: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        pause.reached();
        let before = store.snapshot();
        assert!(root.join("text.new").exists());
        let job = store.text_recovery.load_full().unwrap();
        pause.resume();
        assert!(matches!(job.wait(), Err(Error::TextUnavailable(_))));
        assert_eq!(store.text_status().unwrap().state, "failed");
        assert!(!job.ready.load(Ordering::Acquire));
        assert!(store.text.load().is_none());
        assert!(!root.join("text").exists());
        assert!(!root.join("text.new").exists());
        assert_eq!(store.snapshot().commit, before.commit);
        assert_eq!(store.snapshot().len(), before.len());
        assert!(
            query(store.snapshot(), "ASK {?s ?p ?o}", &Default::default())
                .unwrap()
                .boolean
        );
        assert!(matches!(
            query(store.snapshot(), QUERY, &Default::default()),
            Err(Error::TextUnavailable(_))
        ));
        store.set_quota(Some(0)).unwrap();
        store.rebuild_text().unwrap();
        assert_eq!(count(&store), 1);
        drop(store);
        let reopened = Store::open(root, Default::default()).unwrap();
        assert_eq!(reopened.snapshot().commit, before.commit);
        assert_eq!(count(&reopened), 1);
    }

    #[cfg(unix)]
    #[test]
    fn automatic_recovery_disk_reserve_refusal_preserves_rdf_and_reopens() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let store = Store::open(
            root,
            StoreOptions {
                min_free_disk_bytes: Some(u64::MAX / 2),
                ..Default::default()
            },
        )
        .unwrap();
        let before = store.snapshot();
        let job = store.text_recovery.load_full().unwrap();
        assert!(matches!(job.wait(), Err(Error::TextUnavailable(_))));
        assert_eq!(store.text_status().unwrap().state, "failed");
        assert!(store.text.load().is_none());
        assert!(!root.join("text.new").exists());
        assert_eq!(store.snapshot().commit, before.commit);
        assert_eq!(store.snapshot().len(), before.len());
        assert!(
            query(store.snapshot(), "ASK {?s ?p ?o}", &Default::default())
                .unwrap()
                .boolean
        );
        assert!(matches!(
            query(store.snapshot(), QUERY, &Default::default()),
            Err(Error::TextUnavailable(_))
        ));
        assert!(matches!(
            store.rebuild_text(),
            Err(Error::TextUnavailable(_))
        ));
        drop(store);
        let reopened = Store::open(root, Default::default()).unwrap();
        wait(&reopened);
        assert_eq!(reopened.snapshot().commit, before.commit);
        assert_eq!(count(&reopened), 1);
    }

    #[test]
    fn missing_and_damaged_open_publish_unavailable_then_catch_concurrent_writes() {
        let _serial = serial();
        for damaged in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            populate(root);
            if damaged {
                std::fs::write(root.join("text/meta.json"), b"invalid").unwrap();
            } else {
                std::fs::remove_dir_all(root.join("text")).unwrap();
            }
            let mut pause = Pause::new(root, "admitted");
            let s = Store::open(root, Default::default()).unwrap();
            pause.reached();
            assert!(s.text_enabled());
            assert_eq!(s.text_status().unwrap().state, "rebuilding");
            let held = s.snapshot();
            assert!(matches!(
                query(held.clone(), QUERY, &Default::default()),
                Err(Error::TextUnavailable(_))
            ));
            assert!(
                query(held.clone(), "ASK {?s ?p ?o}", &Default::default())
                    .unwrap()
                    .boolean
            );
            update(
                &s,
                r#"DELETE DATA { <http://ex/a> <http://www.w3.org/2000/01/rdf-schema#label> "fox"@en }; INSERT DATA { <http://ex/b> <http://www.w3.org/2000/01/rdf-schema#label> "fox cub" }"#,
            );
            update(
                &s,
                r#"INSERT DATA { <http://ex/a> <http://www.w3.org/2000/01/rdf-schema#label> "fox"@en; <http://ex/code> "unchanged" }"#,
            );
            update(
                &s,
                r#"INSERT DATA { _:kitten <http://www.w3.org/2000/01/rdf-schema#label> "fox kitten"@fr . GRAPH <http://ex/g> { <http://ex/named> <http://www.w3.org/2000/01/rdf-schema#label> "fox named" } }"#,
            );
            let head = s.snapshot().commit;
            pause.resume();
            wait(&s);
            assert_eq!(count(&s), 3);
            assert_eq!(query(s.snapshot(), "PREFIX text:<http://jena.apache.org/text#> SELECT ?s { GRAPH <http://ex/g> { ?s text:query \"fox\" } }", &Default::default()).unwrap().rows().len(), 1);
            assert_eq!(s.snapshot().commit, head);
            assert!(matches!(
                query(held, QUERY, &Default::default()),
                Err(Error::TextUnavailable(_))
            ));
            let bag = |snapshot| {
                let mut rows = query(snapshot, QUERY, &Default::default())
                    .unwrap()
                    .rows()
                    .into_iter()
                    .map(|row| format!("{row:?}"))
                    .collect::<Vec<_>>();
                rows.sort();
                rows
            };
            let recovered = bag(s.snapshot());
            s.rebuild_text().unwrap();
            assert_eq!(bag(s.snapshot()), recovered);
        }
    }

    #[test]
    fn generation_change_and_bounded_journal_restart_without_missing_data() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let mut pause = Pause::new(root, "built");
        let s = Store::open(root, Default::default()).unwrap();
        pause.reached();
        let old = s.snapshot().generation.uid;
        update(
            &s,
            r#"INSERT DATA { <http://ex/b> <http://www.w3.org/2000/01/rdf-schema#label> "fox" }"#,
        );
        s.compact().unwrap();
        assert_ne!(s.snapshot().generation.uid, old);
        let job = s.text_recovery.load_full().unwrap();
        // Overflow is detected before reserve/extend. Valid RDF writes are independent.
        job.record(
            &s.snapshot(),
            &vec![(WAL_INSERT, [Id::DEFAULT_GRAPH; 4]); JOURNAL + 1],
        );
        assert!(job.journal.lock().invalid);
        assert!(job.journal.lock().quads.is_empty());
        pause.resume();
        wait(&s);
        assert_eq!(count(&s), 2);
    }

    #[test]
    fn exhausted_restart_is_failed_and_explicit_retry_succeeds() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let mut pause = Pause::new(root, "admitted");
        let s = Store::open(root, Default::default()).unwrap();
        pause.reached();
        let job = s.text_recovery.load_full().unwrap();
        let weak = Arc::downgrade(&job);
        hooks().lock().insert(
            (root.to_path_buf(), "built"),
            Arc::new(move || {
                if let Some(job) = weak.upgrade() {
                    job.changed_generation();
                }
            }),
        );
        pause.resume();
        assert!(job.wait().is_err());
        assert_eq!(s.text_status().unwrap().state, "failed");
        assert!(matches!(
            query(s.snapshot(), QUERY, &Default::default()),
            Err(Error::TextUnavailable(_))
        ));
        update(
            &s,
            r#"INSERT DATA { <http://ex/b> <http://www.w3.org/2000/01/rdf-schema#label> "fox" }"#,
        );
        hooks().lock().remove(&(root.to_path_buf(), "built"));
        assert_eq!(s.rebuild_text().unwrap().state, "ready");
        assert_eq!(count(&s), 2);
    }

    #[test]
    fn cancellation_does_not_wait_for_retained_rdf_writer() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let mut pause = Pause::new(root, "built");
        let s = Store::open(root, Default::default()).unwrap();
        pause.reached();
        let mut writer = s.write();
        let job = s.text_recovery.load_full().unwrap();
        let (tx, rx) = mpsc::channel();
        let join = std::thread::spawn(move || {
            job.cancel_and_wait();
            tx.send(()).unwrap();
        });
        pause.resume();
        rx.recv_timeout(Duration::from_secs(3))
            .expect("cancel must not wait for retained writer");
        join.join().unwrap();
        let quad = oxrdf::Quad::new(
            oxrdf::NamedNode::new("http://ex/retained").unwrap(),
            oxrdf::NamedNode::new("http://ex/p").unwrap(),
            oxrdf::Literal::new_simple_literal("ok"),
            oxrdf::GraphName::DefaultGraph,
        );
        let ids = writer.encode_quad(&quad, &mut Default::default()).unwrap();
        writer.insert(ids).unwrap();
        writer.commit().unwrap();
        assert_eq!(s.rebuild_text().unwrap().state, "ready");
        drop(s);
        let reopened = Store::open(root, Default::default()).unwrap();
        assert_eq!(count(&reopened), 1);
    }

    #[test]
    fn closing_active_recovery_joins_cleanup_before_releasing_root() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let mut pause = Pause::new(root, "built");
        let store = Store::open(root, Default::default()).unwrap();
        pause.reached();
        let (closed, completion) = mpsc::channel();
        let closing = std::thread::spawn(move || {
            drop(store);
            closed.send(()).unwrap();
        });
        assert!(completion.recv_timeout(Duration::from_millis(30)).is_err());
        assert!(
            lock_dir(root).is_err(),
            "root released before recovery cleanup"
        );
        pause.resume();
        completion
            .recv_timeout(Duration::from_secs(3))
            .expect("close did not join cancelled recovery");
        closing.join().unwrap();
        assert!(!root.join("text.new").exists());
        assert!(!root.join("text.old").exists());
        drop(lock_dir(root).expect("closed recovery retained dataset lock"));
        let reopened = Store::open(root, Default::default()).unwrap();
        wait(&reopened);
        assert_eq!(count(&reopened), 1);
    }

    #[test]
    fn publication_fence_finishes_before_cancel_returns() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let mut pause = Pause::new(root, "publishing");
        let s = Store::open(root, Default::default()).unwrap();
        pause.reached();
        let job = s.text_recovery.load_full().unwrap();
        let held = s.snapshot();
        let (tx, rx) = mpsc::channel();
        let join = std::thread::spawn(move || {
            job.cancel_and_wait();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        pause.resume();
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        join.join().unwrap();
        assert_eq!(count(&s), 1);
        assert!(matches!(
            query(held, QUERY, &Default::default()),
            Err(Error::TextUnavailable(_))
        ));
        drop(s);
        assert_eq!(count(&Store::open(root, Default::default()).unwrap()), 1);
    }

    #[test]
    fn disable_and_reconfigure_fence_old_owner_and_cache_identity() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let mut pause = Pause::new(root, "built");
        let s = Arc::new(Store::open(root, Default::default()).unwrap());
        pause.reached();
        let other = s.clone();
        let join = std::thread::spawn(move || other.disable_text());
        pause.resume();
        join.join().unwrap().unwrap();
        assert!(!s.text_enabled());
        assert!(!root.join("text.new").exists());
        assert!(!root.join("text").exists());
        s.enable_text(Default::default()).unwrap();
        let held = s.snapshot();
        let owner = held.text.as_ref().unwrap().owner;
        assert_eq!(count(&s), 1);
        let commit = s.snapshot().commit;
        let cfg = TextConfig {
            predicates: crate::text::PredicateSet::Only(vec!["http://ex/code".into()]),
            ..Default::default()
        };
        s.enable_text(cfg).unwrap();
        assert_eq!(count(&s), 0);
        assert_eq!(s.snapshot().commit, commit);
        assert_ne!(owner, s.snapshot().text.as_ref().unwrap().owner);
        assert_eq!(
            query(held, QUERY, &QueryOptions::default())
                .unwrap()
                .rows()
                .len(),
            1
        );
        s.enable_text(Default::default()).unwrap();
        assert_eq!(count(&s), 1);
    }

    #[test]
    fn queue_bound_and_queued_close_release_store_lock_without_waiting() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let busy = dir.path().join("busy");
        std::fs::create_dir(&busy).unwrap();
        write_atomic(
            &busy.join("text.json"),
            &serde_json::to_vec(&TextConfig::default()).unwrap(),
        )
        .unwrap();
        let mut pause = Pause::new(&busy, "admitted");
        let first = Store::open(&busy, Default::default()).unwrap();
        pause.reached();
        let mut stores = Vec::new();
        for i in 0..QUEUE + 1 {
            let root = dir.path().join(format!("queued-{i}"));
            std::fs::create_dir(&root).unwrap();
            write_atomic(
                &root.join("text.json"),
                &serde_json::to_vec(&TextConfig::default()).unwrap(),
            )
            .unwrap();
            let store = Store::open(&root, Default::default()).unwrap();
            assert_eq!(
                store.text_status().unwrap().state,
                if i == QUEUE { "failed" } else { "rebuilding" }
            );
            assert!(
                store
                    .text_recovery
                    .load_full()
                    .unwrap()
                    .journal
                    .lock()
                    .generation
                    .is_none()
            );
            stores.push(store);
        }
        let closed = dir.path().join("queued-0");
        drop(stores.remove(0));
        // Acquiring the existing OS lock is enough proof; do not queue another job.
        let lock = lock_dir(&closed).unwrap();
        drop(lock);
        pause.resume();
        wait(&first);
        for s in &stores {
            s.rebuild_text().unwrap();
        }
    }

    #[test]
    fn install_failure_keeps_rdf_and_retryable_unavailable_state() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::write(root.join("text/meta.json"), b"damaged").unwrap();
        let mut pause = Pause::new(root, "publishing");
        let s = Store::open(root, Default::default()).unwrap();
        pause.reached();
        std::fs::write(root.join("text.old"), b"publication obstruction").unwrap();
        let job = s.text_recovery.load_full().unwrap();
        pause.resume();
        assert!(job.wait().is_err());
        assert_eq!(s.text_status().unwrap().state, "failed");
        assert!(
            query(s.snapshot(), "ASK {?s ?p ?o}", &Default::default())
                .unwrap()
                .boolean
        );
        std::fs::remove_file(root.join("text.old")).unwrap();
        s.rebuild_text().unwrap();
        assert_eq!(count(&s), 1);
    }

    #[test]
    fn interrupted_recovery_child() {
        let Some(root) = std::env::var_os("SPARKLES_TEXT_RECOVERY_CHILD") else {
            return;
        };
        let root = PathBuf::from(root);
        let phase = std::env::var("SPARKLES_TEXT_RECOVERY_PHASE").unwrap();
        let name: &'static str = match phase.as_str() {
            "built" => "built",
            "old-renamed" => "old-renamed",
            "current-renamed" => "current-renamed",
            _ => panic!("unknown phase"),
        };
        hooks()
            .lock()
            .insert((root.clone(), name), Arc::new(|| std::process::exit(71)));
        let s = Store::open(&root, Default::default()).unwrap();
        let job = s.text_recovery.load_full().expect("damaged index recovers");
        job.wait().unwrap();
        panic!("interruption hook did not run");
    }

    #[test]
    fn interrupted_build_and_directory_publication_reopen_from_rdf() {
        let _serial = serial();
        for phase in ["built", "old-renamed", "current-renamed"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            populate(root);
            let before = Store::open(root, Default::default()).unwrap();
            let identity = before.dataset_id();
            let head = before.snapshot().commit;
            drop(before);
            std::fs::write(root.join("text/meta.json"), b"damaged").unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("store::text_recovery::tests::interrupted_recovery_child")
                .arg("--nocapture")
                .env("SPARKLES_TEXT_RECOVERY_CHILD", root)
                .env("SPARKLES_TEXT_RECOVERY_PHASE", phase)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let until = std::time::Instant::now() + Duration::from_secs(10);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if std::time::Instant::now() > until {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("interruption child timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(status.code(), Some(71));
            let reopened = Store::open(root, Default::default()).unwrap();
            wait(&reopened);
            assert_eq!(reopened.dataset_id(), identity);
            assert_eq!(reopened.snapshot().commit, head);
            assert_eq!(count(&reopened), 1);
            assert!(!root.join("text.new").exists());
            assert!(!root.join("text.old").exists());
        }
    }

    #[test]
    fn format_configuration_ahead_and_uncovered_indexes_recover() {
        let _serial = serial();
        for reason in ["format", "configuration", "ahead", "uncovered"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            populate(root);
            if reason == "uncovered" {
                let s = Store::open(root, Default::default()).unwrap();
                s.compact().unwrap();
            }
            let path = root.join("text/meta.json");
            let mut meta: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let mut payload: serde_json::Value =
                serde_json::from_str(meta["payload"].as_str().unwrap()).unwrap();
            match reason {
                "format" => payload["format"] = 0.into(),
                "configuration" => payload["config"] = "different".into(),
                "ahead" => payload["seq"] = 999.into(),
                "uncovered" => payload["seq"] = 0.into(),
                _ => unreachable!(),
            }
            meta["payload"] = serde_json::to_string(&payload).unwrap().into();
            std::fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
            let mut pause = Pause::new(root, "admitted");
            let s = Store::open(root, Default::default()).unwrap();
            pause.reached();
            assert_eq!(s.text_status().unwrap().state, "rebuilding");
            pause.resume();
            wait(&s);
            assert_eq!(count(&s), 1);
        }
    }

    #[test]
    fn recovery_classification_never_expands_lazy_wal_or_cleans_staging() {
        let _serial = serial();
        for kind in ["missing", "invalid", "uncovered", "ahead"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            populate(root);
            let store = Store::open(root, Default::default()).unwrap();
            let snapshot = store.snapshot();
            drop(store);
            let path = root.join("text/meta.json");
            match kind {
                "missing" => std::fs::remove_dir_all(root.join("text")).unwrap(),
                "invalid" => std::fs::write(&path, b"invalid metadata").unwrap(),
                "uncovered" | "ahead" => {
                    let mut meta: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    let mut payload: serde_json::Value =
                        serde_json::from_str(meta["payload"].as_str().unwrap()).unwrap();
                    payload["seq"] = if kind == "ahead" {
                        (snapshot.commit + 1).into()
                    } else {
                        0.into()
                    };
                    meta["payload"] = serde_json::to_string(&payload).unwrap().into();
                    std::fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
                }
                _ => unreachable!(),
            }
            std::fs::create_dir(root.join("text.new")).unwrap();
            let retained = root.join("text.new/retained");
            std::fs::write(&retained, b"untouched").unwrap();
            assert!(
                TextIndex::open_ready(
                    root,
                    Default::default(),
                    &snapshot,
                    Some(snapshot.commit + 1),
                    || panic!("{kind} classification expanded inherited WAL"),
                )
                .unwrap()
                .is_none()
            );
            assert_eq!(std::fs::read(retained).unwrap(), b"untouched");
        }
    }

    #[test]
    fn reusable_linked_index_loads_lazy_wal_once_and_catches_up() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        let store = Store::open(root, Default::default()).unwrap();
        store.create_branch("linked", &Default::default()).unwrap();
        let work = store.branch("linked").unwrap();
        wait(&work);
        update(
            &work,
            r#"INSERT DATA { <http://ex/linked> <http://www.w3.org/2000/01/rdf-schema#label> "fox linked" }"#,
        );
        let snapshot = work.snapshot();
        assert!(snapshot.generation.linked().is_some());
        let branch_root = work.root.clone().unwrap();
        let changed = [
            snapshot.lookup_iri("http://ex/linked").unwrap(),
            snapshot
                .lookup_iri("http://www.w3.org/2000/01/rdf-schema#label")
                .unwrap(),
            snapshot
                .lookup_term(&oxrdf::Literal::from("fox linked").into())
                .unwrap(),
            Id::DEFAULT_GRAPH,
        ];
        drop(work);
        drop(store);
        let path = branch_root.join("text/meta.json");
        let mut meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut payload: serde_json::Value =
            serde_json::from_str(meta["payload"].as_str().unwrap()).unwrap();
        payload["seq"] = (snapshot.commit - 1).into();
        meta["payload"] = serde_json::to_string(&payload).unwrap().into();
        std::fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
        let calls = std::cell::Cell::new(0);
        let (index, view) = TextIndex::open_ready(
            &branch_root,
            Default::default(),
            &snapshot,
            Some(snapshot.commit),
            || {
                calls.set(calls.get() + 1);
                Ok(std::borrow::Cow::Owned(vec![(
                    snapshot.commit,
                    vec![changed],
                )]))
            },
        )
        .unwrap()
        .expect("covered linked index is reused");
        assert_eq!(calls.get(), 1);
        assert_eq!(
            index.status(Some(&view), snapshot.commit).seq,
            snapshot.commit
        );
        assert!(
            index
                .status(Some(&view), snapshot.commit)
                .last_rebuild
                .is_none()
        );
        let mut caught_up = (*snapshot).clone();
        caught_up.text = Some(view);
        assert_eq!(
            query(Arc::new(caught_up), QUERY, &Default::default())
                .unwrap()
                .rows()
                .len(),
            2
        );
    }

    #[test]
    fn queued_manual_join_does_not_prevent_disable_or_hold_root() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let busy = dir.path().join("busy");
        let queued = dir.path().join("queued");
        for root in [&busy, &queued] {
            std::fs::create_dir(root).unwrap();
            write_atomic(
                &root.join("text.json"),
                &serde_json::to_vec(&TextConfig::default()).unwrap(),
            )
            .unwrap();
        }
        let mut pause = Pause::new(&busy, "admitted");
        let first = Store::open(&busy, Default::default()).unwrap();
        pause.reached();
        let second = Arc::new(Store::open(&queued, Default::default()).unwrap());
        let mut manual = Pause::new(&queued, "manual-join");
        let other = second.clone();
        let (finished, done) = mpsc::channel();
        let join = std::thread::spawn(move || {
            finished.send(other.rebuild_text()).unwrap();
        });
        manual.reached();
        let other = second.clone();
        let (disabled, disabled_rx) = mpsc::channel();
        let disable = std::thread::spawn(move || {
            disabled.send(other.disable_text()).unwrap();
        });
        disabled_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("queued join holds lifecycle")
            .unwrap();
        disable.join().unwrap();
        manual.resume();
        assert!(matches!(
            done.recv_timeout(Duration::from_secs(2))
                .expect("cancelled queued join did not finish"),
            Err(Error::Cancelled)
        ));
        join.join().unwrap();
        assert!(!second.text_enabled());
        drop(second);
        let lock = lock_dir(&queued).unwrap();
        drop(lock);
        pause.resume();
        wait(&first);
    }

    #[test]
    fn retry_metadata_registration_never_overwrites_concurrent_rdf_commit() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let mut built = Pause::new(root, "built");
        let s = Arc::new(
            Store::open(
                root,
                StoreOptions {
                    experimental_group_commit: true,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        built.reached();
        let job = s.text_recovery.load_full().unwrap();
        let clone = job.clone();
        let cancel = std::thread::spawn(move || clone.cancel_and_wait());
        built.resume();
        cancel.join().unwrap();
        let mut replacing = Pause::new(root, "retry-replacing");
        let mut registration = Pause::new(root, "registered-snapshot");
        let clone = s.clone();
        let retry = std::thread::spawn(move || clone.rebuild_text());
        replacing.reached();
        {
            let w = s.guarded_writer();
            assert!(!s.group_admission(CommitKind::Transaction, &Default::default(), &w));
        }
        replacing.resume();
        registration.reached();
        update(
            &s,
            r#"INSERT DATA { <http://ex/concurrent> <http://www.w3.org/2000/01/rdf-schema#label> "fox concurrent" }"#,
        );
        let expected = s.snapshot();
        registration.resume();
        retry.join().unwrap().unwrap();
        assert_eq!(s.snapshot().commit, expected.commit);
        assert_eq!(s.snapshot().len(), expected.len());
        assert_eq!(count(&s), 2);
        drop(s);
        let reopened = Store::open(root, Default::default()).unwrap();
        assert_eq!(reopened.snapshot().commit, expected.commit);
        assert_eq!(count(&reopened), 2);
    }

    #[test]
    fn recovering_text_operations_refuse_same_family_callbacks_before_waiting() {
        use crate::sparql::extensions::{
            ExtensionRegistry, ScalarContext, ScalarDescriptor, ScalarResult,
        };
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        populate(root);
        std::fs::remove_dir_all(root.join("text")).unwrap();
        let mut pause = Pause::new(root, "built");
        let s = Arc::new(Store::open(root, Default::default()).unwrap());
        pause.reached();
        let job = s.text_recovery.load_full().unwrap();
        let initial = s.snapshot();
        for operation in 0..3 {
            let captured = s.clone();
            let runner = s.clone();
            let (send, receive) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let mut registry = ExtensionRegistry::builder();
                registry
                    .register_scalar(
                        ScalarDescriptor::new(
                            "urn:test:recovery-operation",
                            0..=0,
                            move |_: &ScalarContext<'_>, _: &[oxrdf::Term]| -> ScalarResult {
                                let result = match operation {
                                    0 => captured.rebuild_text(),
                                    1 => captured.enable_text(Default::default()),
                                    _ => captured
                                        .disable_text()
                                        .map(|()| unreachable!("same-family disable must fail")),
                                };
                                assert!(result.is_err());
                                Ok(oxrdf::Literal::from("suppressed").into())
                            },
                        )
                        .unwrap(),
                    )
                    .unwrap();
                let options = QueryOptions {
                    extensions: Some(registry.build()),
                    ..Default::default()
                };
                let mut transaction = runner.write();
                let quad = oxrdf::Quad::new(
                    oxrdf::NamedNode::new("urn:pending").unwrap(),
                    oxrdf::NamedNode::new("urn:p").unwrap(),
                    oxrdf::Literal::from("pending"),
                    oxrdf::GraphName::DefaultGraph,
                );
                let ids = transaction
                    .encode_quad(&quad, &mut Default::default())
                    .unwrap();
                transaction.insert(ids).unwrap();
                let result = query(
                    Arc::new(transaction.view()),
                    "SELECT (<urn:test:recovery-operation>() AS ?x) {}",
                    &options,
                );
                let commit = transaction.commit();
                send.send((result.is_err(), commit.is_err())).unwrap();
            });
            let result = match receive.recv_timeout(Duration::from_secs(2)) {
                Ok(result) => result,
                Err(_) => {
                    // If the assertion regresses, cancel the owned recovery to unblock
                    // its join and let the test thread release its retained writer.
                    pause.resume();
                    job.cancel_and_wait();
                    let _ = receive.recv_timeout(Duration::from_secs(2));
                    worker.join().unwrap();
                    panic!("text operation waited on its own callback writer");
                }
            };
            worker.join().unwrap();
            assert_eq!(result, (true, true));
            assert_eq!(s.snapshot().commit, initial.commit);
            assert_eq!(s.snapshot().len(), initial.len());
        }
        pause.resume();
        wait(&s);
        assert_eq!(count(&s), 1);
    }
}
