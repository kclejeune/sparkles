//! The order in which a commit becomes visible (see [`super::publish`]).
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Weak, mpsc};
use std::thread;

/// A guard that remembers the last commit it was told of and the published head at
/// that moment.
struct Told {
    seq: AtomicU64,
    head_when_told: AtomicU64,
    store: std::sync::OnceLock<Weak<Store>>,
}

impl crate::guard::CommitGuard for Told {
    fn check(&self, _: &crate::guard::Candidate<'_>) -> Result<crate::guard::ValidationSummary> {
        use crate::guard::{GuardMode, GuardStatus, Severity, ValidationSummary};
        Ok(ValidationSummary::empty(
            GuardStatus::Passed,
            GuardMode::Reject,
            Severity::Violation,
        ))
    }
    fn committed(&self, seq: u64) {
        if let Some(s) = self.store.get().and_then(Weak::upgrade) {
            self.head_when_told
                .store(s.snapshot().commit, Ordering::SeqCst);
        }
        self.seq.store(seq, Ordering::SeqCst);
    }
    fn describe(&self) -> String {
        "told".into()
    }
}

fn insert(s: &Store, name: &str) -> Result<Receipt> {
    let mut t = s.write_with(CommitKind::Update, Default::default());
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

/// A writer that waits on `commits` for commit N and validates at once must check
/// against the guard's state of N, so the guard is told of a serial commit before the
/// commit is published. The committer is held right after its publication, while a
/// waiter woken by it reads what the guard knows.
#[test]
fn a_serial_commit_tells_the_guard_before_it_is_published() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(
        Store::open(
            dir.path(),
            StoreOptions {
                group_commit: false,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let guard = Arc::new(Told {
        seq: AtomicU64::new(0),
        head_when_told: AtomicU64::new(u64::MAX),
        store: Default::default(),
    });
    guard.store.set(Arc::downgrade(&s)).unwrap();
    s.set_guard(Some(guard.clone()));
    let (published_tx, published_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    s.set_failpoint(
        "commit-published",
        Some(Arc::new(move |_: &Store| {
            published_tx.send(()).unwrap();
            go_rx.lock().recv().unwrap();
        })),
    );
    let committer = {
        let s = s.clone();
        thread::spawn(move || insert(&s, "a").unwrap())
    };
    // the waiter sees commit 1 through the watch and reads the guard at once
    let mut commits = s.subscribe_commits();
    published_rx.recv().unwrap();
    while *commits.borrow_and_update() < 1 {
        thread::yield_now();
    }
    assert_eq!(s.snapshot().commit, 1);
    let told = guard.seq.load(Ordering::SeqCst);
    let head_when_told = guard.head_when_told.load(Ordering::SeqCst);
    go_tx.send(()).unwrap();
    let receipt = committer.join().unwrap();
    assert_eq!(receipt.commit.seq, 1);
    assert_eq!(
        told, 1,
        "the guard was not told of commit 1 before it was published"
    );
    assert_eq!(
        head_when_told, 0,
        "the guard was told after the snapshot of commit 1 was current"
    );
    s.set_failpoint("commit-published", None);
}

/// The bulk rebuild publishes through the same tail, so its guard learns of the commit
/// before the new generation's snapshot is current, where it used to learn of it after
/// the old generation was removed.
#[test]
fn a_bulk_commit_tells_the_guard_before_it_is_published() {
    let dir = tempfile::tempdir().unwrap();
    let nt = dir.path().join("in.nt");
    std::fs::write(&nt, "<urn:a> <urn:p> <urn:o> .\n").unwrap();
    let s = Arc::new(
        Store::open(
            &dir.path().join("db"),
            StoreOptions {
                group_commit: false,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let guard = Arc::new(Told {
        seq: AtomicU64::new(0),
        head_when_told: AtomicU64::new(u64::MAX),
        store: Default::default(),
    });
    guard.store.set(Arc::downgrade(&s)).unwrap();
    s.set_guard(Some(guard.clone()));
    let (published_tx, published_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    s.set_failpoint(
        "commit-published",
        Some(Arc::new(move |_: &Store| {
            published_tx.send(()).unwrap();
            go_rx.lock().recv().unwrap();
        })),
    );
    let committer = {
        let s = s.clone();
        // a load into an empty store builds a generation: the bulk path
        thread::spawn(move || s.load(&[Source::from_path(&nt, None).unwrap()]).unwrap())
    };
    let mut commits = s.subscribe_commits();
    published_rx.recv().unwrap();
    while *commits.borrow_and_update() < 1 {
        thread::yield_now();
    }
    let head = s.snapshot();
    assert_eq!((head.commit, head.len()), (1, 1));
    let told = guard.seq.load(Ordering::SeqCst);
    let head_when_told = guard.head_when_told.load(Ordering::SeqCst);
    go_tx.send(()).unwrap();
    assert_eq!(committer.join().unwrap(), 1);
    assert_eq!(
        told, 1,
        "the guard was not told of the bulk commit before it was published"
    );
    assert_eq!(
        head_when_told, 0,
        "the guard was told after the snapshot of the bulk commit was current"
    );
    s.set_failpoint("commit-published", None);
}
