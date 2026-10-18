//! The model-based stress test of X09 section 10.1. Threads issue random small
//! transactions against a persistent store with group commit on: inserts, deletes,
//! multi-quad transactions, no-ops, dry runs and writes with a precondition that holds or
//! fails, some with a commit message. A run may arm one fault: a panic at one of the
//! group's failpoints, or a failed synchronization of the log or of the vocabulary. After
//! each run the guarantees of section 3 are checked against a sequential model built from
//! the receipts, and the reopened store against the same model.
//!
//! `SPARKLES_GROUP_MODEL_RUNS` sets the number of runs (default 24) and
//! `SPARKLES_GROUP_MODEL_SEED` the seed of the first one. A failure names the seed of its
//! run. `SPARKLES_GROUP_MODEL_SEED` with one run repeats that run's inputs, its threads,
//! operations and fault, but not the interleaving that the scheduler chose.
use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::time::Duration;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Insert,
    Delete,
    Multi,
    NoOp,
    DryRun,
    Holds,
    Fails,
}

impl Kind {
    /// whether the operation makes a commit when it succeeds
    fn commits(self) -> bool {
        matches!(
            self,
            Kind::Insert | Kind::Delete | Kind::Multi | Kind::Holds
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    /// acknowledged with this commit number
    Acked(u64),
    /// made no commit, as the operation asked or because it was refused before writing
    NoCommit,
    /// failed or panicked where it may have reached the log
    Uncertain(String),
}

#[derive(Clone, Debug)]
struct Op {
    thread: u32,
    k: u32,
    kind: Kind,
    inserts: Vec<String>,
    deletes: Vec<String>,
    message: Option<String>,
    outcome: Outcome,
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    None,
    Panic(&'static str),
    GroupSync,
    VocabSync,
}

impl Fault {
    fn syncs(self) -> bool {
        matches!(self, Fault::GroupSync | Fault::VocabSync)
    }
}

fn quad(name: &str) -> Quad {
    Quad::new(
        NamedNode::new(format!("urn:m:{name}")).unwrap(),
        NamedNode::new("urn:m:p").unwrap(),
        NamedNode::new("urn:m:o").unwrap(),
        GraphName::DefaultGraph,
    )
}

fn holds(snap: &Snapshot, name: &str) -> bool {
    let Some(s) = snap.lookup_iri(&format!("urn:m:{name}")) else {
        return false;
    };
    let (Some(p), Some(o)) = (snap.lookup_iri("urn:m:p"), snap.lookup_iri("urn:m:o")) else {
        return false;
    };
    snap.contains(&[s, p, o, Id::DEFAULT_GRAPH]).unwrap()
}

/// Run one operation, returning its outcome.
fn run_op(s: &Store, op: &Op) -> Outcome {
    let mut opts = crate::guard::WriteOptions {
        message: op.message.as_deref().map(Into::into),
        ..Default::default()
    };
    match op.kind {
        Kind::DryRun => {
            opts.dry_run = Some(crate::preview::DryRun {
                changes: 0,
                all_changes: false,
                max_changes: 0,
            })
        }
        Kind::Holds => {
            opts.precondition = Some(crate::guard::Precondition::new(|_| Ok(())));
        }
        Kind::Fails => {
            opts.precondition = Some(crate::guard::Precondition::new(|snap| {
                Err(Error::PreconditionFailed(format!(
                    "head is {}",
                    snap.commit
                )))
            }))
        }
        _ => {}
    }
    let r = catch_unwind(AssertUnwindSafe(|| {
        // the fallible form, which checks the options (`write_with` takes none of them)
        let mut t = s.try_write_with(CommitKind::Update, opts)?;
        for n in &op.inserts {
            let ids = t.encode_quad(&quad(n), &mut Default::default())?;
            t.insert(ids)?;
        }
        for n in &op.deletes {
            let ids = t.encode_quad(&quad(n), &mut Default::default())?;
            t.delete(ids)?;
        }
        t.commit()
    }));
    match r {
        Err(_) => Outcome::Uncertain("panicked".into()),
        Ok(Ok(r)) if r.committed => {
            assert!(op.kind.commits(), "{op:?} made commit {}", r.commit.seq);
            Outcome::Acked(r.commit.seq)
        }
        Ok(Ok(_)) => {
            assert_eq!(op.kind, Kind::NoOp, "{op:?} made no commit");
            Outcome::NoCommit
        }
        Ok(Err(Error::DryRun(_))) if op.kind == Kind::DryRun => Outcome::NoCommit,
        Ok(Err(Error::PreconditionFailed(_))) if op.kind == Kind::Fails => Outcome::NoCommit,
        // nothing it does can reach the log
        Ok(Err(e)) if !op.kind.commits() => Outcome::Uncertain(format!("refused: {e}")),
        Ok(Err(e)) => Outcome::Uncertain(e.to_string()),
    }
}

/// What thread `t` does: `n` operations on its own quads, stopping at the first failure.
#[allow(clippy::too_many_arguments)]
fn worker(
    s: &Store,
    t: u32,
    n: u32,
    seed: u64,
    messages: bool,
    counter: &AtomicU64,
    fault_at: u64,
    fault: Fault,
) -> Vec<Op> {
    let mut rng = Rng(seed | 1);
    let mut live: Vec<String> = Vec::new();
    let mut next = 0u32;
    let mut ops = Vec::new();
    for k in 0..n {
        if counter.fetch_add(1, Ordering::SeqCst) == fault_at {
            arm(s, fault);
        }
        let mut fresh = || {
            next += 1;
            format!("{t}-{next}")
        };
        let kind = match rng.below(100) {
            0..=34 => Kind::Insert,
            35..=49 if !live.is_empty() => Kind::Delete,
            35..=49 => Kind::Insert,
            50..=64 => Kind::Multi,
            65..=72 if !live.is_empty() => Kind::NoOp,
            65..=72 => Kind::Insert,
            73..=82 => Kind::DryRun,
            83..=91 => Kind::Holds,
            _ => Kind::Fails,
        };
        let (inserts, deletes) = match kind {
            Kind::Insert | Kind::Holds | Kind::DryRun | Kind::Fails => (vec![fresh()], vec![]),
            Kind::Multi => (vec![fresh(), fresh(), fresh()], vec![]),
            Kind::Delete => {
                let i = rng.below(live.len() as u64) as usize;
                (vec![], vec![live.swap_remove(i)])
            }
            Kind::NoOp => {
                let i = rng.below(live.len() as u64) as usize;
                (vec![live[i].clone()], vec![])
            }
        };
        let message = (messages && kind.commits()).then(|| format!("m {t} {k}"));
        let mut op = Op {
            thread: t,
            k,
            kind,
            inserts,
            deletes,
            message,
            outcome: Outcome::NoCommit,
        };
        op.outcome = run_op(s, &op);
        match &op.outcome {
            Outcome::Acked(_) if op.kind.commits() => live.extend(op.inserts.iter().cloned()),
            Outcome::Acked(_) | Outcome::NoCommit => {}
            Outcome::Uncertain(_) => {
                ops.push(op);
                break;
            }
        }
        ops.push(op);
    }
    ops
}

fn arm(s: &Store, fault: Fault) {
    match fault {
        Fault::None => {}
        Fault::Panic(name) => {
            let once = Arc::new(AtomicBool::new(false));
            s.set_failpoint(
                name,
                Some(Arc::new(move |_| {
                    if !once.swap(true, Ordering::SeqCst) {
                        panic!("injected panic");
                    }
                })),
            );
        }
        Fault::GroupSync => s.fail_next_group_sync(),
        Fault::VocabSync => s.snapshot().generation.dvocab.fail_next_sync(),
    }
}

struct RunResult {
    ops: Vec<Op>,
    /// (commit, quads) of every snapshot a reader saw
    seen: Vec<(u64, u64)>,
}

fn run(dir: &Path, seed: u64, opts: StoreOptions, messages: bool, fault: Fault) -> RunResult {
    let mut rng = Rng(seed | 1);
    let threads = 4 + rng.below(5) as u32;
    let per = 20 + rng.below(41) as u32;
    let fault_at = rng.below(u64::from(threads * per));
    let s = Arc::new(Store::open(dir, opts).unwrap());
    let counter = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (s, stop) = (s.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                let snap = s.snapshot();
                seen.push((snap.commit, snap.len()));
                std::thread::sleep(Duration::from_micros(50));
            }
            seen
        })
    };
    let hs: Vec<_> = (1..=threads)
        .map(|t| {
            let (s, counter) = (s.clone(), counter.clone());
            let wseed = rng.next();
            std::thread::spawn(move || {
                worker(&s, t, per, wseed, messages, &counter, fault_at, fault)
            })
        })
        .collect();
    let mut ops = Vec::new();
    for h in hs {
        ops.extend(h.join().expect("a worker catches its panics"));
    }
    stop.store(true, Ordering::Relaxed);
    let seen = reader.join().unwrap();
    // guarantee 6: after a failed synchronization the writer stays poisoned
    let failed = ops
        .iter()
        .any(|o| matches!(&o.outcome, Outcome::Uncertain(e) if e != "panicked"));
    if fault.syncs() && failed {
        let mut w = s.write_as(CommitKind::Update);
        let late = w
            .encode_quad(&quad("late"), &mut Default::default())
            .and_then(|ids| w.insert(ids))
            .and_then(|_| w.commit());
        assert!(matches!(late, Err(Error::Poisoned)), "{late:?}");
    }
    drop(s);
    RunResult { ops, seen }
}

/// The names that hold after applying `ops` in order.
fn apply<'a>(ops: impl IntoIterator<Item = &'a Op>) -> BTreeSet<String> {
    let mut state = BTreeSet::new();
    for op in ops {
        for n in &op.inserts {
            state.insert(n.clone());
        }
        for n in &op.deletes {
            state.remove(n);
        }
    }
    state
}

fn check_run(dir: &Path, r: &RunResult, messages: bool, fault: Fault, what: &str) {
    let acked: Vec<(u64, &Op)> = r
        .ops
        .iter()
        .filter_map(|o| match o.outcome {
            Outcome::Acked(seq) => Some((seq, o)),
            _ => None,
        })
        .collect();
    // guarantee 2: commit numbers are unique
    let mut seqs: Vec<u64> = acked.iter().map(|a| a.0).collect();
    seqs.sort_unstable();
    let before = seqs.len();
    seqs.dedup();
    assert_eq!(
        before,
        seqs.len(),
        "{what}: a commit number was acknowledged twice"
    );
    let top = seqs.last().copied().unwrap_or(0);
    let s = Store::open(dir, StoreOptions::default())
        .unwrap_or_else(|e| panic!("{what}: the store does not reopen: {e}"));
    let snap = s.snapshot();
    let head = snap.commit;
    // guarantee 4: the head is at or after the last acknowledged commit
    assert!(
        head >= top,
        "{what}: head {head} is before acknowledged commit {top}"
    );
    let uncertain: Vec<&Op> = r
        .ops
        .iter()
        .filter(|o| matches!(o.outcome, Outcome::Uncertain(_)) && o.kind.commits())
        .collect();
    // guarantee 3: a commit whose synchronization failed is never published. Only a panic
    // after publication can show readers a commit that is not acknowledged.
    if fault.syncs() && !uncertain.is_empty() {
        let seen = r.seen.iter().map(|s| s.0).max().unwrap_or(0);
        assert!(
            seen <= top,
            "{what}: a reader saw commit {seen}, past the last acknowledged commit {top}"
        );
    }
    // every name an operation touched
    let names: BTreeSet<&String> = r
        .ops
        .iter()
        .flat_map(|o| o.inserts.iter().chain(&o.deletes))
        .collect();
    let model: Vec<&Op> = if messages {
        // Every commit carries its operation's message, so the log names the operation
        // behind each commit number: the sequence is a prefix of acknowledged and
        // uncertain operations, with each acknowledged one at its own number.
        let by_msg: BTreeMap<&str, &Op> = r
            .ops
            .iter()
            .filter_map(|o| o.message.as_deref().map(|m| (m, o)))
            .collect();
        let mut used = BTreeSet::new();
        let mut order = Vec::new();
        // the commit number of each uncertain operation found in the log
        let mut uncertain_at = Vec::new();
        for seq in 1..=head {
            // Every commit's annotation reaches the disk before the commit does (the
            // ordinary path syncs it first and keeps it when its own sync fails, and a
            // grouped commit without it is a torn tail), so every commit has its message.
            let m = s
                .annotation(seq)
                .and_then(|a| a.message)
                .unwrap_or_else(|| panic!("{what}: commit {seq} has no message"));
            let op = by_msg
                .get(m.as_ref())
                .unwrap_or_else(|| panic!("{what}: commit {seq} has an unknown message {m}"));
            assert!(
                used.insert((op.thread, op.k)),
                "{what}: {m} committed twice"
            );
            match op.outcome {
                Outcome::Acked(a) => assert_eq!(a, seq, "{what}: {m} acknowledged as {a}"),
                Outcome::Uncertain(_) => uncertain_at.push(seq),
                Outcome::NoCommit => panic!("{what}: {m} made no commit but is commit {seq}"),
            }
            order.push(*op);
        }
        // guarantee 1: every acknowledged commit is in the log
        for (seq, op) in &acked {
            assert!(
                *seq <= head && used.contains(&(op.thread, op.k)),
                "{what}: {seq} lost"
            );
        }
        // guarantee 2: after a failed synchronization no later commit is acknowledged
        if fault.syncs() {
            for &u in &uncertain_at {
                assert!(
                    top < u,
                    "{what}: commit {top} was acknowledged after uncertain commit {u}"
                );
            }
        }
        // guarantee 3: every snapshot a reader saw was the model's state at its commit
        let mut lens = vec![0u64];
        let mut state = BTreeSet::new();
        for op in &order {
            for n in &op.inserts {
                state.insert(n.clone());
            }
            for n in &op.deletes {
                state.remove(n);
            }
            lens.push(state.len() as u64);
        }
        for &(c, len) in &r.seen {
            assert!(
                c <= head,
                "{what}: a reader saw commit {c} past the recovered head {head}"
            );
            assert_eq!(
                lens[c as usize], len,
                "{what}: a reader saw commit {c} with {len} quads"
            );
        }
        order
    } else {
        // Without messages the commits name no operation. Each thread stops at its first
        // uncertain operation, which reached the log whole or not at all, and the count of
        // commits must match what reached it.
        let mut applied: Vec<&Op> = acked.iter().map(|a| a.1).collect();
        applied.sort_by_key(|o| match o.outcome {
            Outcome::Acked(s) => s,
            _ => unreachable!(),
        });
        for op in &uncertain {
            let present = |n: &String| holds(&snap, n);
            let effect = op.inserts.iter().all(present) && !op.deletes.iter().any(present);
            let none = !op.inserts.iter().any(present) && op.deletes.iter().all(present);
            assert!(effect || none, "{what}: {op:?} reached the log in part");
            if effect && !none {
                applied.push(op);
            }
        }
        assert_eq!(
            head,
            applied.len() as u64,
            "{what}: {} commits reached the log, {} operations did",
            head,
            applied.len()
        );
        for &(c, _) in &r.seen {
            assert!(
                c <= head,
                "{what}: a reader saw commit {c} past the recovered head {head}"
            );
        }
        applied
    };
    let state = apply(model.iter().copied());
    for n in &names {
        assert_eq!(holds(&snap, n), state.contains(*n), "{what}: quad {n}");
    }
    assert_eq!(snap.len(), state.len() as u64, "{what}: quad count");
    // guarantee 1: acknowledged messages survive
    for (seq, op) in &acked {
        if let Some(m) = &op.message {
            assert_eq!(
                s.annotation(*seq).and_then(|a| a.message).as_deref(),
                Some(m.as_str()),
                "{what}: the message of {seq}"
            );
        }
    }
    drop(snap);
    let report = crate::check::check(dir, &crate::check::CheckOptions { quick: false }).unwrap();
    assert!(
        report.status != crate::check::Status::Error,
        "{what}: sparkles check fails:\n{}",
        report.to_text()
    );
    // the recovered store takes the next commit
    let mut w = s.write_as(CommitKind::Update);
    let ids = w
        .encode_quad(&quad("after"), &mut Default::default())
        .unwrap();
    w.insert(ids).unwrap();
    assert_eq!(w.commit().unwrap().commit.seq, head + 1, "{what}");
}

const FAULTS: [Fault; 7] = [
    Fault::None,
    Fault::Panic("group-appended"),
    Fault::Panic("group-sealed"),
    Fault::Panic("group-synced"),
    Fault::Panic("group-published"),
    Fault::GroupSync,
    Fault::VocabSync,
];

#[test]
fn concurrent_random_writes_match_a_sequential_model() {
    let runs = env_num("SPARKLES_GROUP_MODEL_RUNS", 24);
    let first = env_num("SPARKLES_GROUP_MODEL_SEED", 0x005e_ed0f_9a7e);
    let mut seeds = Rng(first | 1);
    for i in 0..runs {
        let seed = if i == 0 { first } else { seeds.next() };
        let mut rng = Rng(seed | 1);
        let messages = rng.below(4) != 0;
        let fault = FAULTS[rng.below(FAULTS.len() as u64) as usize];
        let opts = StoreOptions {
            group_commit: true,
            group_commit_idle_direct: rng.below(2) == 0,
            wal_direct_writes: rng.below(2) == 0,
            ..Default::default()
        };
        let what = format!(
            "seed {seed:#x} (messages {messages}, {fault:?}, idle direct {}, direct {})",
            opts.group_commit_idle_direct, opts.wal_direct_writes
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        // a hung fence must fail the test, not stall it
        let (tx, rx) = mpsc::channel();
        let w = what.clone();
        std::thread::spawn(move || {
            let r = run(&path, seed, opts, messages, fault);
            check_run(&path, &r, messages, fault, &w);
            if std::env::var_os("SPARKLES_GROUP_MODEL_VERBOSE").is_some() {
                let acked = r
                    .ops
                    .iter()
                    .filter(|o| matches!(o.outcome, Outcome::Acked(_)))
                    .count();
                let uncertain: Vec<_> = r
                    .ops
                    .iter()
                    .filter_map(|o| match &o.outcome {
                        Outcome::Uncertain(e) => Some(e.as_str()),
                        _ => None,
                    })
                    .collect();
                eprintln!(
                    "{w}: {} operations, {acked} acknowledged, uncertain {uncertain:?}",
                    r.ops.len()
                );
            }
            let _ = tx.send(());
        });
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what}: failed (see above)"),
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("{what}: did not finish"),
        }
    }
}
