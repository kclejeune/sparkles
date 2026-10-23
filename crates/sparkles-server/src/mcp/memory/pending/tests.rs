use super::*;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Point {
    Counted,
    Contended,
    PruneCandidates,
}

pub(crate) type Hook = dyn Fn(Point, &str) + Send + Sync;

pub(crate) struct ScopedHook {
    st: Arc<AppState>,
    previous: Option<Arc<Hook>>,
}

impl ScopedHook {
    pub(crate) fn new(
        st: &Arc<AppState>,
        hook: impl Fn(Point, &str) + Send + Sync + 'static,
    ) -> Self {
        let previous = st.ingest.review.hook.lock().replace(Arc::new(hook));
        Self {
            st: st.clone(),
            previous,
        }
    }
}

impl Drop for ScopedHook {
    fn drop(&mut self) {
        *self.st.ingest.review.hook.lock() = self.previous.take();
    }
}

pub(crate) fn receive<T>(rx: &Receiver<T>) -> T {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("ordering watchdog expired")
}

pub(crate) fn idle(st: &Arc<AppState>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !st.ingest.review.busy.lock().is_empty() {
        assert!(Instant::now() < deadline, "touch worker did not finish");
        std::thread::yield_now();
    }
}

/// The old refresh pauses before publishing. On the baseline a new caller finishes
/// first; with per-name serialization it reports contention, allowing the old holder
/// to finish before the new caller. Neither schedule depends on elapsed time.
pub(crate) struct Paused {
    pub(crate) _hook: ScopedHook,
    entered: Receiver<()>,
    release: Sender<()>,
    events: Receiver<()>,
}

impl Paused {
    pub(crate) fn new(st: &Arc<AppState>, old_thread: &'static str) -> Self {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let (event_tx, events) = mpsc::channel();
        let paused = AtomicBool::new(false);
        let hook = ScopedHook::new(st, move |point, name| {
            if name != "org" {
                return;
            }
            let thread = std::thread::current();
            if point == Point::Counted
                && thread.name() == Some(old_thread)
                && !paused.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                entered_tx.send(()).unwrap();
                receive(&release_rx.lock());
            }
            if point == Point::Contended && thread.name() == Some("fresh-refresh") {
                event_tx.send(()).unwrap();
            }
        });
        Self {
            _hook: hook,
            entered,
            release,
            events,
        }
    }

    pub(crate) fn entered(&self) {
        receive(&self.entered);
    }

    pub(crate) fn fresh(
        &self,
        st: &Arc<AppState>,
        ds: &Arc<Dataset>,
    ) -> JoinHandle<Option<Arc<Counts>>> {
        let st = st.clone();
        let ds = ds.clone();
        // Completion and contention are equivalent scheduling events for releasing
        // the old publisher.
        let (done_tx, done_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("fresh-refresh".into())
            .spawn(move || {
                let result = refresh(&st, &ds, None);
                let _ = done_tx.send(());
                result
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.events.try_recv().is_ok() || done_rx.try_recv().is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "fresh refresh neither finished nor reached its gate"
            );
            std::thread::yield_now();
        }
        self.release.send(()).unwrap();
        thread
    }

    pub(crate) fn release(&self) {
        self.release.send(()).unwrap();
    }
}

pub(crate) fn old_refresh(
    st: &Arc<AppState>,
    ds: &Arc<Dataset>,
) -> JoinHandle<Option<Arc<Counts>>> {
    let st = st.clone();
    let ds = ds.clone();
    std::thread::Builder::new()
        .name("old-refresh".into())
        .spawn(move || refresh(&st, &ds, None))
        .unwrap()
}

fn configured_dataset(st: &Arc<AppState>, name: &str) -> Arc<Dataset> {
    let ds = st.attach(name, crate::state::DbType::Mem, None).unwrap();
    crate::assist::write_file(
        st,
        &ds,
        "memory.json",
        &json!({
            "agentGraphs": ["http://example.org/agents/*"],
            "consolidatedGraph": "http://example.org/reviewed"
        }),
    )
    .unwrap();
    ds
}

fn fixture() -> (Arc<AppState>, Arc<Dataset>) {
    let st = Arc::new(AppState::standalone(
        Default::default(),
        Duration::from_secs(30),
    ));
    let ds = configured_dataset(&st, "org");
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            b"<http://example.org/agents/s1> { <urn:s> <urn:p> <urn:o> . }".to_vec(),
            sparkles::io::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    ds.dataset
        .create_branch("consolidation.test", &Default::default())
        .unwrap();
    ds.dataset
        .branch("consolidation.test")
        .unwrap()
        .store()
        .load(&[sparkles::io::Source::from_bytes(
            b"<http://example.org/reviewed> { <urn:s> <urn:p> <urn:o> . }".to_vec(),
            sparkles::io::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    (st, ds)
}

#[test]
fn different_datasets_refresh_concurrently() {
    let (st, ds) = fixture();
    let other = configured_dataset(&st, "other");
    let paused = Paused::new(&st, "old-refresh");
    let old = old_refresh(&st, &ds);
    paused.entered();
    let (tx, rx) = mpsc::channel();
    let st2 = st.clone();
    let fresh = std::thread::spawn(move || {
        let counted = refresh(&st2, &other, None).unwrap();
        tx.send(()).unwrap();
        counted
    });
    receive(&rx);
    paused.release();
    old.join().unwrap();
    fresh.join().unwrap();
}

#[test]
fn disabled_settings_cannot_be_resurrected_by_old_count() {
    let (st, ds) = fixture();
    let paused = Paused::new(&st, "old-refresh");
    let old = old_refresh(&st, &ds);
    paused.entered();
    crate::assist::write_file(&st, &ds, "memory.json", &json!({})).unwrap();
    let fresh = paused.fresh(&st, &ds);
    old.join().unwrap();
    assert!(fresh.join().unwrap().is_none());
    assert!(st.ingest.review.get("org").is_none());
    assert!(series(&st).is_empty());
}

#[test]
fn replacement_touch_resolves_the_new_dataset() {
    let (st, old) = fixture();
    let paused = Paused::new(&st, "review-count");
    touch(&st, "org");
    paused.entered();
    assert!(st.delete("org").unwrap());
    let new = configured_dataset(&st, "org");
    touch(&st, "org"); // Coalesces while the old instance's worker is still busy.
    paused.release();
    idle(&st);
    let cached = st.ingest.review.get("org").unwrap();
    assert_eq!(
        cached.fingerprint,
        fingerprint(&new, &crate::assist::memory_settings(&st, &new))
    );
    assert!(cached.kinds(|_| true).values().all(|k| k.open == 0));
    assert!(
        refresh(&st, &old, None).is_none(),
        "obsolete work was accepted"
    );
    assert!(Arc::ptr_eq(&st.ingest.review.get("org").unwrap(), &cached));
}

#[test]
fn pruning_rechecks_live_names_and_cache_hits_are_reused() {
    let (st, ds) = fixture();
    let first = refresh(&st, &ds, None).unwrap();
    assert!(Arc::ptr_eq(&first, &refresh(&st, &ds, None).unwrap()));
    assert!(st.delete("org").unwrap());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let _hook = ScopedHook::new(&st, move |point, _| {
        if point == Point::PruneCandidates {
            entered_tx.send(()).unwrap();
            receive(&release_rx.lock());
        }
    });
    let st2 = st.clone();
    let pruning = std::thread::spawn(move || refresh_all(&st2));
    receive(&entered_rx);
    let replacement = configured_dataset(&st, "org");
    let new = refresh(&st, &replacement, None).unwrap();
    release_tx.send(()).unwrap();
    pruning.join().unwrap();
    assert!(Arc::ptr_eq(
        &st.ingest
            .review
            .get("org")
            .expect("replacement cache was pruned"),
        &new
    ));
    drop(_hook);
    assert!(st.delete("org").unwrap());
    refresh_all(&st);
    assert!(st.ingest.review.get("org").is_none());
}

#[test]
fn expired_gates_are_pruned_and_fallback_never_uses_an_old_instance() {
    let (st, ds) = fixture();
    let initial = refresh(&st, &ds, None).unwrap();
    assert!(st.ingest.review.gates.lock()["org"].upgrade().is_none());
    let branch = st.branch_dataset(&ds, "consolidation.test").unwrap();
    assert!(
        refresh(&st, &branch, None).is_some(),
        "live branch owner was rejected"
    );
    let initial_main = refresh(&st, &ds, None).unwrap();
    assert_eq!(initial_main.fingerprint, initial.fingerprint);
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            b"<urn:extra> <urn:p> <urn:o> .".to_vec(),
            sparkles::io::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    st.ingest
        .review
        .fail_count
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(Arc::ptr_eq(
        &initial_main,
        &refresh(&st, &ds, None).unwrap()
    ));
    assert!(
        Arc::ptr_eq(&initial_main, &refresh(&st, &branch, None).unwrap()),
        "a live branch must retain same-main fallback"
    );
    assert!(st.delete("org").unwrap());
    assert!(
        Arc::ptr_eq(&initial_main, &st.ingest.review.get("org").unwrap()),
        "the failed replacement must exercise an existing old-owner cache"
    );
    let new = configured_dataset(&st, "org");
    assert!(
        refresh(&st, &new, None).is_none(),
        "replacement received old-instance fallback"
    );
    assert!(
        st.ingest.review.get("org").is_none(),
        "failed replacement count retained the old owner's cache"
    );
    assert!(series(&st).iter().all(|(name, _, _, _)| name != "org"));
    let metrics = crate::obs::metrics_json(&st);
    assert!(
        metrics["memoryReview"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["dataset"] != "org"),
        "{metrics}"
    );
    st.ingest
        .review
        .fail_count
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let recovered = refresh(&st, &new, None).unwrap();
    assert_eq!(
        recovered.fingerprint,
        fingerprint(&new, &crate::assist::memory_settings(&st, &new))
    );
    assert!(
        recovered
            .kinds(|_| true)
            .values()
            .all(|kind| kind.open == 0)
    );
    assert!(
        series(&st)
            .iter()
            .filter(|(name, _, _, _)| name == "org")
            .all(|(_, _, open, _)| *open == 0)
    );
    assert!(
        refresh(&st, &ds, None).is_none(),
        "stale caller must still be refused"
    );
    assert!(
        Arc::ptr_eq(&recovered, &st.ingest.review.get("org").unwrap()),
        "stale caller purged replacement's successful cache"
    );
    assert!(st.delete("org").unwrap());
    refresh_all(&st);
    assert!(st.ingest.review.get("org").is_none());
    let held = st.ingest.review.gate("held");
    let waiting = st.ingest.review.gate("held");
    assert!(Arc::ptr_eq(&held, &waiting));
    refresh_all(&st);
    assert!(st.ingest.review.gates.lock()["held"].upgrade().is_some());
    drop(held);
    drop(waiting);
    refresh_all(&st);
    assert!(st.ingest.review.gates.lock().is_empty());
}
