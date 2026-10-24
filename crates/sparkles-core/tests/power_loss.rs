//! Durability of acknowledged commits under lost writes and failed synchronizations, with
//! and without group commit (spec X09 sections 4.3 and 10).
//!
//! Process exit leaves written pages in the page cache, so it cannot show a lost write.
//! These tests run a writer process under `tests/support/iofault.c`, an `LD_PRELOAD` shim
//! that records every write, truncation and synchronization of the store's files in the
//! order the process issued them, and that can fail chosen calls with `EIO`. Two kinds of
//! test use it.
//!
//!  * `crash_images_keep_every_acknowledged_commit` replays the log to the files as a power
//!    loss after any call would have left them. A write that no synchronization covered yet
//!    may be lost, kept, or kept in part (any subset of its 4 KiB pages), and a
//!    synchronization or a write-through (`O_DSYNC`) write that had not returned may have
//!    covered nothing. Every image must open, hold a prefix of the commits that includes
//!    every commit acknowledged before the crash, pass `sparkles check`, and take a new
//!    write at the next sequence number. The shim also records the creation, renaming and
//!    unlinking of files. Such an operation is durable once a synchronization of its
//!    directory has returned, and of those that are not, an image keeps a prefix, as a
//!    file system that journals its metadata in order would.
//!    `crash_images_of_a_dataset_with_full_text_search` runs the same images on a dataset
//!    with full-text search, whose index replaces its metadata file by a rename, and checks
//!    that the index never names a commit above the recovered log and that its results
//!    match the recovered data (X09 section 17.8). A separate background-checkpoint
//!    case requires acknowledged publication during actual text-file synchronization;
//!    a bounded fixture handshake starts a designated transaction after sync entry.
//!  * `failed_synchronizations_*` fail one call and check that every commit that was not
//!    acknowledged fails, that nothing past the last acknowledged commit is published, and
//!    that the store reopens to a prefix that holds every acknowledged commit.
//!
//! The model of directory operations has limits, so it covers the reorderings that the
//! store's own files meet, not every one a file system allows. The shim records a file's
//! creation after its `open` returns, and decides that a file is new by a check before
//! the open, so a concurrent first write to the new file can be logged before its
//! creation. A lost rename is modelled for the exact path it moved, so writes under a
//! renamed directory are not redirected, and no test renames a directory. Writes to a
//! file after its unlink are logged under its `(deleted)` path. `mkdir` is not recorded,
//! so a lost directory creation is not modelled. Pending directory operations survive as
//! an in-order prefix, as in the metadata journals of ext4 and XFS.
//!
//! `SPARKLES_POWER_LOSS_POINTS` sets the number of crash points per configuration (default
//! 60) and `SPARKLES_POWER_LOSS_SEED` the seed of the random choices. Setting
//! `SPARKLES_POWER_LOSS_KEEP` retains successful image runs and their raw I/O logs
//! under the test target directory for inspection; failed runs are always retained.
#![cfg(target_os = "linux")]

use oxrdf::{GraphName, Literal, NamedNode, Quad};
use sparkles_core::check::{CheckOptions, Status, check};
use sparkles_core::commit::CommitKind;
use sparkles_core::store::{Store, StoreOptions};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

const CHILD: &str = "SPARKLES_PL_CHILD";
const BACKGROUND: &str = "SPARKLES_PL_BACKGROUND_TEXT";

fn work_root() -> PathBuf {
    let p = Path::new(env!("CARGO_TARGET_TMPDIR")).join("power-loss");
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn shim() -> &'static Path {
    static SHIM: OnceLock<PathBuf> = OnceLock::new();
    SHIM.get_or_init(|| {
        let out = work_root().join(format!("libiofault-{}.so", std::process::id()));
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/iofault.c");
        let status = Command::new("cc")
            .args(["-shared", "-fPIC", "-O1", "-o"])
            .arg(&out)
            .arg(&src)
            .arg("-ldl")
            .status()
            .expect("a C compiler (cc) builds the I/O fault shim");
        assert!(status.success(), "the I/O fault shim did not build");
        out
    })
}

fn subject(t: u32, k: u32) -> String {
    format!("urn:s-{t}-{k}")
}

fn quad(t: u32, k: u32) -> Quad {
    Quad::new(
        NamedNode::new(subject(t, k)).unwrap(),
        NamedNode::new("urn:p").unwrap(),
        Literal::new_simple_literal(format!("value {t} {k}")),
        GraphName::DefaultGraph,
    )
}

fn message(t: u32, k: u32) -> String {
    format!("commit {t} {k}")
}

/// Whether `s` holds the commit that thread `t` made as its `k`th.
fn holds(s: &Store, t: u32, k: u32) -> bool {
    let snap = s.snapshot();
    let q = quad(t, k);
    let (Some(a), Some(b), Some(c)) = (
        snap.lookup_iri(&subject(t, k)),
        snap.lookup_iri("urn:p"),
        snap.lookup_term(&q.object),
    ) else {
        return false;
    };
    snap.contains(&[a, b, c, sparkles_core::id::Id::DEFAULT_GRAPH])
        .unwrap()
}

fn base_commit(s: &Store, n: u32) {
    for k in 0..n {
        let mut t = s.write_as(CommitKind::Update);
        let ids = t.encode_quad(&quad(0, k), &mut Default::default()).unwrap();
        assert!(t.insert(ids).unwrap());
        t.commit().unwrap();
    }
}

#[derive(Clone, Copy)]
struct Config {
    group: bool,
    direct: bool,
    idle_direct: bool,
    threads: u32,
    commits: u32,
}

impl Config {
    fn options(&self) -> StoreOptions {
        StoreOptions {
            group_commit: self.group,
            wal_direct_writes: self.direct,
            group_commit_idle_direct: self.idle_direct,
            ..Default::default()
        }
    }
}

/// The writer process: `threads` threads make `commits` commits each. A thread writes an
/// acknowledgement record to the ack file after each receipt, and stops at the first
/// error. Prints one line per commit (`ok THREAD K SEQ` or `err THREAD K MESSAGE`) and the
/// published head at the end.
#[test]
fn child_writer() {
    let Ok(spec) = std::env::var(CHILD) else {
        return;
    };
    let f: Vec<&str> = spec.split(',').collect();
    assert_eq!(
        f.len(),
        5,
        "group, WAL direct, threads, commits, idle direct"
    );
    let cfg = Config {
        group: f[0] == "1",
        direct: f[1] == "1",
        idle_direct: f[4] == "1",
        threads: f[2].parse().unwrap(),
        commits: f[3].parse().unwrap(),
    };
    let db = PathBuf::from(std::env::var("SPARKLES_PL_DB").unwrap());
    let ack = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::var("SPARKLES_PL_ACK").unwrap())
        .unwrap();
    let ack = Arc::new(Mutex::new(ack));
    let s = Arc::new(Store::open(&db, cfg.options()).unwrap());
    let background = cfg!(feature = "text") && std::env::var_os(BACKGROUND).is_some();
    // This trace event precedes write_with, so a matching receipt inside the same
    // sync interval proves a fresh transaction could publish during checkpoint I/O.
    let begin = background.then(|| {
        Arc::new(Mutex::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(
                    PathBuf::from(std::env::var("SPARKLES_PL_ACK").unwrap())
                        .with_file_name("begin.log"),
                )
                .unwrap(),
        ))
    });
    // faults count the calls after the store has opened
    if let Ok(arm) = std::env::var("SPARKLES_IO_ARM") {
        std::fs::write(arm, b"armed").unwrap();
    }
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut hs = Vec::new();
    for t in 1..=cfg.threads {
        let (s, ack, out, begin) = (s.clone(), ack.clone(), out.clone(), begin.clone());
        hs.push(std::thread::spawn(move || {
            for k in 0..cfg.commits {
                let designated = background && t == 1 && k == 0;
                if designated {
                    let entered = PathBuf::from(std::env::var("SPARKLES_IO_SYNC_ENTERED").unwrap());
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    while !entered.is_file() {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "no armed text-file sync entered"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
                if let Some(begin) = &begin {
                    let mut rec = [0u8; 8];
                    rec[..4].copy_from_slice(&t.to_le_bytes());
                    rec[4..].copy_from_slice(&k.to_le_bytes());
                    begin.lock().unwrap().write_all(&rec).unwrap();
                }
                // every commit has a message, so the annotation of each acknowledged
                // commit is checked after the crash
                let mut w = s.write_with(
                    CommitKind::Update,
                    sparkles_core::guard::WriteOptions {
                        message: Some(message(t, k).into()),
                        ..Default::default()
                    },
                );
                // a transaction that begins on a poisoned store may already fail here
                let staged = w
                    .encode_quad(&quad(t, k), &mut Default::default())
                    .and_then(|ids| w.insert(ids));
                match staged.and_then(|_| w.commit()) {
                    Ok(r) => {
                        let mut rec = [0u8; 16];
                        rec[..8].copy_from_slice(&r.commit.seq.to_le_bytes());
                        rec[8..12].copy_from_slice(&t.to_le_bytes());
                        rec[12..].copy_from_slice(&k.to_le_bytes());
                        ack.lock().unwrap().write_all(&rec).unwrap();
                        out.lock()
                            .unwrap()
                            .push(format!("ok {t} {k} {}", r.commit.seq));
                        if designated {
                            std::fs::write(
                                std::env::var("SPARKLES_IO_SYNC_RELEASE").unwrap(),
                                b"1",
                            )
                            .unwrap();
                            out.lock()
                                .unwrap()
                                .push("background-gate-released 1 0".into());
                        }
                    }
                    Err(e) => {
                        out.lock().unwrap().push(format!("err {t} {k} {e}"));
                        return;
                    }
                }
            }
        }));
    }
    // A side thread drives checkpointing without waiting for the one-second tick.
    // Background mode uses the actual unlocked tick and never calls the explicit
    // checkpoint. The other mode also checkpoints at each group fence before its
    // synchronization, when an index applied before its fence would name commits
    // that a crash can still lose. Both modes check against the recovered log.
    #[cfg(all(feature = "text", feature = "failpoints"))]
    if s.text_enabled() && !background {
        s.set_failpoint(
            "group-sealed",
            Some(Arc::new(|s: &Store| {
                let _ = s.checkpoint_text();
            })),
        );
    }
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(feature = "text")]
    let checkpoints = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    #[cfg(feature = "text")]
    let checkpointer = s.text_enabled().then(|| {
        let (s, done) = (s.clone(), done.clone());
        #[cfg(feature = "failpoints")]
        let checkpoints = checkpoints.clone();
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if background {
                    #[cfg(feature = "failpoints")]
                    if s.checkpoint_text_background_for_test() {
                        checkpoints.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    #[cfg(not(feature = "failpoints"))]
                    panic!("the background crash fixture needs failpoints");
                } else if s.checkpoint_text().is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
    });
    // In background mode a reader searches the index throughout, as the server's readers
    // do. Each search seals the open batch and commits it to the index's files, so
    // commits race the unlocked checkpoint passes, and the tick follows a run of raced
    // passes with a second pass that holds the live writer lock and synchronizes only
    // what the first pass left.
    #[cfg(feature = "text")]
    let reader = background.then(|| {
        let (s, done) = (s.clone(), done.clone());
        std::thread::spawn(move || {
            let q = "PREFIX text:<http://jena.apache.org/text#> \
                     SELECT (COUNT(*) AS ?n) { ?s text:query \"value\" }";
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if sparkles_core::sparql::query(s.snapshot(), q, &Default::default()).is_err() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        })
    });
    for h in hs {
        h.join().unwrap();
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    #[cfg(feature = "text")]
    if let Some(c) = checkpointer {
        c.join().unwrap();
    }
    #[cfg(feature = "text")]
    if let Some(r) = reader {
        r.join().unwrap();
    }
    #[cfg(feature = "text")]
    if background {
        println!(
            "LINE background-checkpoints {}",
            checkpoints.load(std::sync::atomic::Ordering::Relaxed)
        );
        #[cfg(feature = "failpoints")]
        {
            let [locked, locked_cleared, cleared] = s.text_background_checkpoints_for_test();
            println!("LINE background-locked {locked} {locked_cleared} {cleared}");
        }
    }
    for l in out.lock().unwrap().iter() {
        println!("LINE {l}");
    }
    println!("LINE head {}", s.snapshot().commit);
    // an error poisons the store; it must also refuse a later write
    let mut w = s.write_as(CommitKind::Update);
    let after = w
        .encode_quad(&quad(99, 99), &mut Default::default())
        .and_then(|ids| w.insert(ids))
        .is_ok()
        && w.commit().is_ok();
    println!("LINE after {after}");
    // leave without closing: nothing is trimmed or synced on the way out
    std::process::exit(0);
}

struct Run {
    root: PathBuf,
    lines: Vec<String>,
}

impl Run {
    fn db(&self) -> PathBuf {
        self.root.join("db")
    }
    fn log(&self) -> PathBuf {
        self.root.join("io.log")
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (a, b) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            copy_dir(&a, &b);
        } else {
            std::fs::copy(&a, &b).unwrap();
        }
    }
}

/// Prepare a store of `base` commits, copy it to `base-db`, and run the writer on the
/// original under the shim, recording its I/O when `record` is set and failing the calls
/// named by `fault`.
fn run_writer(
    name: &str,
    cfg: Config,
    base: u32,
    record: bool,
    fault: &str,
    text: bool,
    background: bool,
) -> Run {
    let root = work_root().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("ack")).unwrap();
    let db = root.join("db");
    {
        let s = Store::open(&db, StoreOptions::default()).unwrap();
        base_commit(&s, base);
        // the writer opens the full-text index from its checkpoint and maintains it
        if text {
            #[cfg(feature = "text")]
            s.enable_text(Default::default()).unwrap();
        }
    }
    copy_dir(&db, &root.join("base-db"));
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "child_writer", "--nocapture"])
        .env(
            CHILD,
            format!(
                "{},{},{},{},{}",
                u8::from(cfg.group),
                u8::from(cfg.direct),
                cfg.threads,
                cfg.commits,
                u8::from(cfg.idle_direct)
            ),
        )
        .env("SPARKLES_PL_DB", &db)
        .env("SPARKLES_PL_ACK", root.join("ack/ack.log"))
        .env("SPARKLES_IO_ROOT", &root)
        .env("SPARKLES_IO_ARM", root.join("armed"))
        .env("LD_PRELOAD", shim())
        .env_remove(BACKGROUND)
        .env_remove("SPARKLES_IO_DELAY_US")
        .env_remove("SPARKLES_IO_SYNC_ENTERED")
        .env_remove("SPARKLES_IO_SYNC_RELEASE");
    if record {
        cmd.env("SPARKLES_IO_LOG", root.join("io.log"));
    }
    if background {
        assert!(text && record && fault.is_empty());
        // The first armed text-file sync waits for writer1's first receipt. That
        // transaction starts after sync entry, so a delayed earlier receipt cannot
        // satisfy the overlap oracle. Both sides have finite timeouts.
        cmd.env(BACKGROUND, "1")
            .env(
                "SPARKLES_IO_SYNC_ENTERED",
                root.join("ack/text-sync-entered"),
            )
            .env(
                "SPARKLES_IO_SYNC_RELEASE",
                root.join("ack/text-sync-release"),
            );
    }
    if !fault.is_empty() {
        cmd.env("SPARKLES_IO_FAULT", fault);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "writer failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("LINE ").map(str::to_string))
        .collect();
    Run { root, lines }
}

// ----------------------------------------------------------------- the I/O log

enum Rec {
    Write {
        path: String,
        off: u64,
        data: Vec<u8>,
        dsync: bool,
    },
    WriteEnd {
        path: String,
        off: u64,
        failed: bool,
    },
    Trunc {
        path: String,
        len: u64,
    },
    Sync {
        path: String,
    },
    SyncEnd {
        path: String,
        failed: bool,
    },
    Rename {
        path: String,
        to: String,
    },
    Unlink {
        path: String,
    },
    Create {
        path: String,
    },
}

fn parse_log(buf: &[u8]) -> Vec<Rec> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 4 <= buf.len() {
        let (kind, flag) = (buf[p], buf[p + 1]);
        let pl = u16::from_le_bytes([buf[p + 2], buf[p + 3]]) as usize;
        let hdr = 4 + pl + 16;
        if p + hdr > buf.len() {
            break;
        }
        let path = String::from_utf8_lossy(&buf[p + 4..p + 4 + pl]).to_string();
        let off = u64::from_le_bytes(buf[p + 4 + pl..p + 12 + pl].try_into().unwrap());
        let len = u64::from_le_bytes(buf[p + 12 + pl..p + 20 + pl].try_into().unwrap());
        p += hdr;
        out.push(match kind {
            1 => {
                let data = buf[p..p + len as usize].to_vec();
                p += len as usize;
                Rec::Write {
                    path,
                    off,
                    data,
                    dsync: flag == 1,
                }
            }
            2 => Rec::WriteEnd {
                path,
                off,
                failed: flag == 1,
            },
            3 => Rec::Trunc { path, len },
            4 => Rec::Sync { path },
            5 => Rec::SyncEnd {
                path,
                failed: flag == 1,
            },
            6 => {
                let to = String::from_utf8_lossy(&buf[p..p + len as usize]).to_string();
                p += len as usize;
                Rec::Rename { path, to }
            }
            7 => Rec::Unlink { path },
            8 => Rec::Create { path },
            k => panic!("unknown record kind {k}"),
        });
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Pending,
    Durable,
    Dropped,
}

struct Op {
    off: u64,
    /// `None` for a truncation to `off`
    data: Option<Vec<u8>>,
    state: State,
    dsync: bool,
    returned: bool,
}

#[derive(Default)]
struct FileModel {
    ops: Vec<Op>,
    /// the ops that had returned when the running synchronization began
    covered: Vec<usize>,
}

enum DirKind {
    Create,
    Rename(String),
    Unlink,
}

/// A directory operation: durable once a synchronization of its directory that began
/// after it succeeds.
struct DirOp {
    kind: DirKind,
    path: String,
    state: State,
}

impl DirOp {
    /// The directory whose synchronization makes the operation durable: the new name's
    /// for a rename.
    fn dir(&self) -> &str {
        let p = match &self.kind {
            DirKind::Rename(to) => to,
            _ => &self.path,
        };
        Path::new(p).parent().and_then(Path::to_str).unwrap_or("")
    }
}

/// One step of the replay, in the order the process issued it.
enum Ev {
    /// the `n`th operation on a file's data
    Data(String, usize),
    /// the `n`th directory operation
    Dir(usize),
}

/// The files of the store as the writer process left them in the page cache and on the
/// device, rebuilt record by record.
struct Model {
    files: BTreeMap<String, FileModel>,
    dirs: Vec<DirOp>,
    /// per directory, the directory operations its running synchronization covers
    dir_covered: BTreeMap<String, Vec<usize>>,
    events: Vec<Ev>,
}

impl Model {
    fn new() -> Model {
        Model {
            files: BTreeMap::new(),
            dirs: Vec::new(),
            dir_covered: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    fn push_op(&mut self, path: &str, op: Op) {
        let f = self.files.entry(path.to_string()).or_default();
        f.ops.push(op);
        self.events
            .push(Ev::Data(path.to_string(), f.ops.len() - 1));
    }

    fn push_dir(&mut self, kind: DirKind, path: &str) {
        self.dirs.push(DirOp {
            kind,
            path: path.to_string(),
            state: State::Pending,
        });
        self.events.push(Ev::Dir(self.dirs.len() - 1));
    }

    fn apply(&mut self, r: &Rec) {
        match r {
            Rec::Write {
                path,
                off,
                data,
                dsync,
            } => self.push_op(
                path,
                Op {
                    off: *off,
                    data: Some(data.clone()),
                    state: State::Pending,
                    dsync: *dsync,
                    returned: false,
                },
            ),
            Rec::WriteEnd { path, off, failed } => {
                let f = self.files.entry(path.clone()).or_default();
                if let Some(op) = f
                    .ops
                    .iter_mut()
                    .find(|o| !o.returned && o.data.is_some() && o.off == *off)
                {
                    op.returned = true;
                    if *failed {
                        op.state = State::Dropped;
                    } else if op.dsync {
                        op.state = State::Durable;
                    }
                }
            }
            Rec::Trunc { path, len } => self.push_op(
                path,
                Op {
                    off: *len,
                    data: None,
                    state: State::Pending,
                    dsync: false,
                    returned: true,
                },
            ),
            Rec::Sync { path } => {
                let f = self.files.entry(path.clone()).or_default();
                f.covered = (0..f.ops.len())
                    .filter(|i| f.ops[*i].returned && f.ops[*i].state == State::Pending)
                    .collect();
                let covered = (0..self.dirs.len())
                    .filter(|i| {
                        self.dirs[*i].state == State::Pending && self.dirs[*i].dir() == path
                    })
                    .collect();
                self.dir_covered.insert(path.clone(), covered);
            }
            Rec::SyncEnd { path, failed } => {
                let covered = self.dir_covered.remove(path).unwrap_or_default();
                if !failed {
                    let f = self.files.entry(path.clone()).or_default();
                    for i in std::mem::take(&mut f.covered) {
                        f.ops[i].state = State::Durable;
                    }
                    for i in covered {
                        self.dirs[i].state = State::Durable;
                    }
                }
            }
            Rec::Rename { path, to } => self.push_dir(DirKind::Rename(to.clone()), path),
            Rec::Unlink { path } => self.push_dir(DirKind::Unlink, path),
            Rec::Create { path } => self.push_dir(DirKind::Create, path),
        }
    }

    /// The number of directory operations not yet durable.
    fn pending_dirs(&self) -> usize {
        self.dirs
            .iter()
            .filter(|d| d.state == State::Pending)
            .count()
    }

    /// The files of the store: the base content, then the durable operations, then the
    /// pending ones that `keep` chooses (or part of them, per 4 KiB page), in the order the
    /// process issued them. Of the directory operations that are not durable, the first
    /// `dir_prefix` survive and the rest are lost, as a file system that journals its
    /// metadata in order keeps a prefix of it. Data written to a file whose creation was
    /// lost is lost with it. Pending page selections are independent per write, a
    /// conservative overapproximation that need not describe a coherent physical page or
    /// sector snapshot.
    fn image(
        &self,
        base: &Path,
        root: &Path,
        out: &Path,
        dir_prefix: usize,
        mut keep: impl FnMut(&Op) -> Option<Vec<bool>>,
    ) {
        let _ = std::fs::remove_dir_all(out);
        copy_dir(base, out);
        let db = root.join("db");
        let rel = |p: &str| -> Option<PathBuf> {
            Path::new(p).strip_prefix(&db).ok().map(Path::to_path_buf)
        };
        // the image's files by their path under `out`: `None` once removed
        let mut ns: BTreeMap<PathBuf, Option<Vec<u8>>> = BTreeMap::new();
        fn load<'a>(
            ns: &'a mut BTreeMap<PathBuf, Option<Vec<u8>>>,
            out: &Path,
            r: &Path,
        ) -> &'a mut Option<Vec<u8>> {
            ns.entry(r.to_path_buf()).or_insert_with(|| {
                let p = out.join(r);
                if p.is_file() {
                    Some(std::fs::read(p).unwrap())
                } else {
                    None
                }
            })
        }
        // every file under `r` that the base copy holds and the map does not yet
        fn load_tree(ns: &mut BTreeMap<PathBuf, Option<Vec<u8>>>, out: &Path, r: &Path) {
            let p = out.join(r);
            if p.is_dir() {
                for e in std::fs::read_dir(&p).unwrap() {
                    let e = e.unwrap();
                    load_tree(ns, out, &r.join(e.file_name()));
                }
            } else if p.is_file() {
                load(ns, out, r);
            }
        }
        let mut pending_dirs = 0usize;
        // names whose rename was lost: the data the process wrote under the new name
        // went to the file that is still under the old one
        let mut alias: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
        // names whose creation was lost
        let mut lost: std::collections::BTreeSet<PathBuf> = Default::default();
        for ev in &self.events {
            match ev {
                Ev::Data(path, i) => {
                    let Some(r) = rel(path) else { continue };
                    let op = &self.files[path].ops[*i];
                    let pages = match op.state {
                        State::Dropped => continue,
                        State::Durable => None,
                        State::Pending => match keep(op) {
                            None => continue,
                            some => some,
                        },
                    };
                    let r = alias.get(&r).cloned().unwrap_or(r);
                    if lost.contains(&r) || out.join(&r).is_dir() {
                        continue;
                    }
                    let bytes = load(&mut ns, out, &r).get_or_insert_with(Vec::new);
                    match &op.data {
                        None => bytes.resize(op.off as usize, 0),
                        Some(d) => {
                            let end = op.off as usize + d.len();
                            if bytes.len() < end {
                                bytes.resize(end, 0);
                            }
                            match &pages {
                                None => bytes[op.off as usize..end].copy_from_slice(d),
                                Some(chosen) => {
                                    // chosen[i]: the i'th 4 KiB page the write touches
                                    let first = op.off as usize / 4096;
                                    let n = end.div_ceil(4096) - first;
                                    for i in 0..n {
                                        // an empty list keeps every page
                                        if !chosen.get(i).copied().unwrap_or(chosen.is_empty()) {
                                            continue;
                                        }
                                        let lo = ((first + i) * 4096).max(op.off as usize);
                                        let hi = ((first + i + 1) * 4096).min(end);
                                        bytes[lo..hi].copy_from_slice(
                                            &d[lo - op.off as usize..hi - op.off as usize],
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                Ev::Dir(i) => {
                    let d = &self.dirs[*i];
                    let Some(r) = rel(&d.path) else { continue };
                    let kept = match d.state {
                        State::Durable => true,
                        _ => {
                            pending_dirs += 1;
                            pending_dirs <= dir_prefix
                        }
                    };
                    match (&d.kind, kept) {
                        (DirKind::Create, true) => {
                            lost.remove(&r);
                            let f = load(&mut ns, out, &r);
                            if f.is_none() {
                                *f = Some(Vec::new());
                            }
                        }
                        (DirKind::Create, false) => {
                            // a file of the same name that existed before stays
                            if load(&mut ns, out, &r).is_none() {
                                lost.insert(r);
                            }
                        }
                        (DirKind::Unlink, true) => {
                            load_tree(&mut ns, out, &r);
                            let under: Vec<PathBuf> =
                                ns.keys().filter(|k| k.starts_with(&r)).cloned().collect();
                            for k in under {
                                ns.insert(k, None);
                            }
                            ns.insert(r, None);
                        }
                        (DirKind::Unlink, false) => {}
                        (DirKind::Rename(to), true) => {
                            let Some(to) = rel(to) else { continue };
                            load_tree(&mut ns, out, &r);
                            load_tree(&mut ns, out, &to);
                            // the target is replaced, a directory with its contents
                            let old: Vec<PathBuf> =
                                ns.keys().filter(|k| k.starts_with(&to)).cloned().collect();
                            for k in old {
                                ns.insert(k, None);
                            }
                            let moved: Vec<PathBuf> =
                                ns.keys().filter(|k| k.starts_with(&r)).cloned().collect();
                            for k in moved {
                                let v = ns.insert(k.clone(), None).flatten();
                                let dest = to.join(k.strip_prefix(&r).unwrap());
                                ns.insert(dest, v);
                            }
                            alias.remove(&to);
                            if lost.remove(&r) {
                                lost.insert(to);
                            }
                        }
                        (DirKind::Rename(to), false) => {
                            if let Some(to) = rel(to) {
                                alias.insert(to, r);
                            }
                        }
                    }
                }
            }
        }
        for (r, v) in ns {
            let p = out.join(&r);
            match v {
                Some(bytes) => {
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent).unwrap();
                    }
                    if p.is_dir() {
                        std::fs::remove_dir_all(&p).unwrap();
                    }
                    std::fs::write(&p, bytes).unwrap();
                }
                None if p.is_dir() => std::fs::remove_dir_all(&p).unwrap(),
                None => {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn coin(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The commit the full-text index's metadata names, when it can be read.
#[cfg(feature = "text")]
fn text_payload_seq(db: &Path) -> Option<u64> {
    let meta = std::fs::read(db.join("text/meta.json")).ok()?;
    let meta: serde_json::Value = serde_json::from_slice(&meta).ok()?;
    let payload: serde_json::Value = serde_json::from_str(meta["payload"].as_str()?).ok()?;
    payload["seq"].as_u64()
}

/// Wait for the full-text index of `s` to be ready, then check that a search for the word
/// every value has finds exactly the subjects that hold one.
#[cfg(feature = "text")]
fn check_text(s: &Store, what: &str) {
    use sparkles_core::sparql::query;
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let st = s.text_status().expect("full-text search is configured");
        if st.state == "ready" {
            break;
        }
        assert!(
            std::time::Instant::now() < until && st.state == "rebuilding",
            "{what}: the full-text index is {} ({:?})",
            st.state,
            st.message
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let subjects = |q: &str| -> std::collections::BTreeSet<String> {
        query(s.snapshot(), q, &Default::default())
            .unwrap_or_else(|e| panic!("{what}: {q}: {e}"))
            .rows()
            .into_iter()
            .filter_map(|r| r[0].as_ref().map(|t| t.to_string()))
            .collect()
    };
    assert_eq!(
        subjects(
            "PREFIX text:<http://jena.apache.org/text#> SELECT ?s { ?s text:query \"value\" }"
        ),
        subjects("SELECT ?s { ?s <urn:p> ?o }"),
        "{what}: full-text search after recovery"
    );
}

/// Check one image against the commits acknowledged before the crash. Returns the
/// commit that the image's full-text checkpoint names, if any.
fn check_image(
    img: &Path,
    base: u32,
    acked: &[(u64, u32, u32)],
    text: bool,
    what: &str,
) -> Option<u64> {
    // The full-text index's checkpoint never names a commit above the recovered log, so
    // the index either opens from it and catches up, or is rebuilt when its files were
    // left unsynced. Either way its results match the recovered data.
    #[cfg(feature = "text")]
    let payload = text.then(|| text_payload_seq(img)).flatten();
    #[cfg(not(feature = "text"))]
    let (_, payload) = (text, None::<u64>);
    let s = Store::open(img, StoreOptions::default())
        .unwrap_or_else(|e| panic!("{what}: the image does not open: {e}"));
    #[cfg(feature = "text")]
    if text {
        let head = s.snapshot().commit;
        if let Some(seq) = payload {
            assert!(
                seq <= head,
                "{what}: the text index names {seq}, the log ends at {head}"
            );
        }
        check_text(&s, what);
    }
    let snap = s.snapshot();
    let head = snap.commit;
    let top = acked
        .iter()
        .map(|a| a.0)
        .max()
        .unwrap_or(0)
        .max(u64::from(base));
    assert!(
        head >= top,
        "{what}: head {head} is before the last acknowledged commit {top}"
    );
    assert_eq!(
        snap.len(),
        head,
        "{what}: the head {head} is not a prefix of one-quad commits"
    );
    for (seq, t, k) in acked {
        assert!(
            holds(&s, *t, *k),
            "{what}: acknowledged commit {seq} ({t},{k}) is missing"
        );
        assert_eq!(
            s.annotation(*seq).and_then(|a| a.message).as_deref(),
            Some(message(*t, *k).as_str()),
            "{what}: acknowledged commit {seq} ({t},{k}) lost its message"
        );
    }
    drop(snap);
    let report = check(img, &CheckOptions::default()).unwrap();
    assert!(
        report.status != Status::Error,
        "{what}: sparkles check fails:\n{}",
        report.to_text()
    );
    // the store takes a write at the next sequence number
    let mut w = s.write_as(CommitKind::Update);
    let ids = w
        .encode_quad(&quad(98, 98), &mut Default::default())
        .unwrap();
    w.insert(ids).unwrap();
    assert_eq!(w.commit().unwrap().commit.seq, head + 1, "{what}");
    payload
}

/// Require a transaction's begin and receipt in one successfully completed sync.
/// An ack alone can be delayed after publication, and an adjacent sync is not the same
/// interval. Repeated paths reset their admitted pairs conservatively.
fn background_publications(log: &[Rec]) -> usize {
    type Transactions = std::collections::BTreeSet<(u32, u32)>;
    let mut syncing = BTreeMap::<String, (Transactions, Transactions)>::new();
    let mut published = Transactions::new();
    for rec in log {
        match rec {
            Rec::Sync { path }
                if Path::new(path)
                    .parent()
                    .is_some_and(|parent| parent.ends_with("text")) =>
            {
                syncing.insert(path.clone(), Default::default());
            }
            Rec::SyncEnd { path, failed } => {
                if let Some((_, acknowledged)) = syncing.remove(path)
                    && !failed
                {
                    published.extend(acknowledged);
                }
            }
            Rec::Write { path, data, .. } if path.ends_with("begin.log") && data.len() == 8 => {
                let key = (
                    u32::from_le_bytes(data[..4].try_into().unwrap()),
                    u32::from_le_bytes(data[4..].try_into().unwrap()),
                );
                for (begun, _) in syncing.values_mut() {
                    begun.insert(key);
                }
            }
            Rec::Write { path, data, .. } if path.ends_with("ack.log") && data.len() == 16 => {
                let key = (
                    u32::from_le_bytes(data[8..12].try_into().unwrap()),
                    u32::from_le_bytes(data[12..].try_into().unwrap()),
                );
                for (begun, acknowledged) in syncing.values_mut() {
                    if begun.contains(&key) {
                        acknowledged.insert(key);
                    }
                }
            }
            _ => {}
        }
    }
    published.len()
}

#[test]
fn background_oracle_requires_a_fresh_transaction_in_one_successful_sync() {
    let sync = || Rec::Sync {
        path: "/db/text/a.seg".into(),
    };
    let end = |failed| Rec::SyncEnd {
        path: "/db/text/a.seg".into(),
        failed,
    };
    let begin = |t: u32, k: u32| {
        let mut data = t.to_le_bytes().to_vec();
        data.extend_from_slice(&k.to_le_bytes());
        Rec::Write {
            path: "/ack/begin.log".into(),
            off: 0,
            data,
            dsync: false,
        }
    };
    let ack = |t: u32, k: u32| {
        let mut data = 4u64.to_le_bytes().to_vec();
        data.extend_from_slice(&t.to_le_bytes());
        data.extend_from_slice(&k.to_le_bytes());
        Rec::Write {
            path: "/ack/ack.log".into(),
            off: 0,
            data,
            dsync: false,
        }
    };
    assert_eq!(
        background_publications(&[begin(1, 1), sync(), ack(1, 1), end(false)]),
        0,
        "a receipt delayed after earlier publication is not a fresh transaction"
    );
    assert_eq!(
        background_publications(&[sync(), begin(1, 1), ack(1, 1), end(false)]),
        1
    );
    assert_eq!(
        background_publications(&[
            sync(),
            begin(1, 1),
            end(false),
            sync(),
            ack(1, 1),
            end(false)
        ]),
        0,
        "the begin and receipt must be inside the same sync interval"
    );
    assert_eq!(
        background_publications(&[sync(), begin(1, 1), ack(1, 2), end(false)]),
        0,
        "the receipt must match the transaction that began inside sync"
    );
    assert_eq!(
        background_publications(&[sync(), begin(1, 1), end(false), ack(1, 1)]),
        0
    );
    assert_eq!(
        background_publications(&[sync(), begin(1, 1), ack(1, 1), end(true)]),
        0,
        "a failed sync cannot establish the successful-I/O interleaving"
    );
}

fn crash_images(name: &str, cfg: Config, text: bool, background: bool) {
    let base = 3;
    let run = run_writer(name, cfg, base, true, "", text, background);
    let log = parse_log(&std::fs::read(run.log()).unwrap());
    if background {
        let count = run
            .lines
            .iter()
            .find_map(|line| line.strip_prefix("background-checkpoints "))
            .and_then(|count| count.parse::<usize>().ok())
            .expect("the writer must report its background checkpoint attempts");
        assert!(count > 0, "{name}: no background checkpoint ran");
        assert!(
            run.lines
                .iter()
                .any(|line| line == "background-gate-released 1 0"),
            "{name}: designated transaction did not release the text-file sync"
        );
        let overlaps = background_publications(&log);
        let [locked, locked_cleared, cleared]: [usize; 3] = run
            .lines
            .iter()
            .find_map(|line| line.strip_prefix("background-locked "))
            .map(|l| {
                let n: Vec<usize> = l.split(' ').map(|n| n.parse().unwrap()).collect();
                n.try_into().unwrap()
            })
            .expect("the writer must report its locked checkpoint passes");
        println!(
            "{name}: {count} actual background checkpoints, {locked} locked passes ({locked_cleared} cleared), {cleared} passes that cleared the marker, {overlaps} fresh acknowledged transactions during successful text-file sync"
        );
        // Every pass synchronizes only the files written since a pass last synchronized
        // them, so the images after a pass cleared the marker check that nothing the
        // index refers to was left out. The reader's commits race the unlocked passes, so
        // the locked pass that follows a run of raced ones runs too.
        assert!(
            locked > 0 && cleared > 0,
            "{name}: {locked} locked passes and {cleared} passes that cleared the marker"
        );
        assert!(
            overlaps > 0,
            "{name}: no fresh transaction began and was acknowledged during one successful text-file synchronization"
        );
    }
    let root = run.root.canonicalize().unwrap();
    // the paths in the log are canonical
    let mut model = Model::new();
    let points = env_num("SPARKLES_POWER_LOSS_POINTS", 60) as usize;
    let stride = (log.len() / points.max(1)).max(1);
    let mut rng = Rng(env_num("SPARKLES_POWER_LOSS_SEED", 0x9e3779b97f4a7c15) | 1);
    let mut acked: Vec<(u64, u32, u32)> = Vec::new();
    let img = run.root.join("image");
    let mut images = 0;
    // the highest commit an image's full-text checkpoint named
    let mut named = 0;
    let base_db = run.root.join("base-db");
    for (i, rec) in log.iter().enumerate() {
        if let Rec::Write { path, data, .. } = rec
            && path.ends_with("ack.log")
        {
            let seq = u64::from_le_bytes(data[..8].try_into().unwrap());
            let t = u32::from_le_bytes(data[8..12].try_into().unwrap());
            let k = u32::from_le_bytes(data[12..16].try_into().unwrap());
            acked.push((seq, t, k));
        }
        model.apply(rec);
        if i % stride != 0 && i + 1 != log.len() {
            continue;
        }
        let what = format!("{name}: crash after record {i} of {}", log.len());
        // everything still unsynchronized is lost
        model.image(&base_db, &root, &img, 0, |_| None);
        let p = check_image(
            &img,
            base,
            &acked,
            text,
            &format!("{what}, unsynced writes lost"),
        );
        named = named.max(p.unwrap_or(0));
        // everything is kept, as after a process crash
        model.image(&base_db, &root, &img, usize::MAX, |_| Some(Vec::new()));
        let p = check_image(&img, base, &acked, text, &format!("{what}, nothing lost"));
        named = named.max(p.unwrap_or(0));
        // a random part of what is pending survives, in whole writes or single pages, and
        // a random prefix of the directory operations
        for _ in 0..2 {
            let dirs = (rng.next() % (model.pending_dirs() as u64 + 1)) as usize;
            model.image(&base_db, &root, &img, dirs, |op| {
                if !rng.coin(50) {
                    return None;
                }
                let n = op.data.as_ref().map_or(1, |d| {
                    (op.off as usize + d.len()).div_ceil(4096) - op.off as usize / 4096
                });
                Some(if rng.coin(60) {
                    vec![true; n]
                } else {
                    (0..n).map(|_| rng.coin(50)).collect()
                })
            });
            let p = check_image(
                &img,
                base,
                &acked,
                text,
                &format!("{what}, random pages lost"),
            );
            named = named.max(p.unwrap_or(0));
        }
        images += 4;
    }
    println!(
        "{name}: checked {images} crash images from {} I/O records, {} acknowledgements, highest text checkpoint {named}",
        log.len(),
        acked.len()
    );
    assert!(images > 0);
    assert!(!acked.is_empty(), "the writer acknowledged no commit");
    // The full-text index makes segment files and replaces its metadata by a rename,
    // and the images held checkpoints of the run's own commits, so the bound on the
    // checkpoint was checked against commits that a crash could lose.
    if text {
        assert!(
            named > u64::from(base),
            "{name}: no image held a full-text checkpoint past the setup's commit {base}"
        );
        let renames = model
            .dirs
            .iter()
            .filter(|d| matches!(&d.kind, DirKind::Rename(to) if to.ends_with("meta.json")))
            .count();
        let creates = model
            .dirs
            .iter()
            .filter(|d| matches!(d.kind, DirKind::Create))
            .count();
        assert!(
            renames > 0 && creates > 0,
            "{name}: {renames} renames of meta.json and {creates} creations recorded"
        );
    }
    if std::env::var_os("SPARKLES_POWER_LOSS_KEEP").is_some() {
        println!(
            "{name}: retained raw I/O and crash fixture at {}",
            run.root.display()
        );
    } else {
        let _ = std::fs::remove_dir_all(&run.root);
    }
}

#[test]
fn crash_images_keep_every_acknowledged_commit() {
    for (name, cfg) in [
        (
            "group-direct",
            Config {
                group: true,
                direct: true,
                idle_direct: true,
                threads: 4,
                commits: 25,
            },
        ),
        (
            "group-buffered",
            Config {
                group: true,
                direct: false,
                idle_direct: true,
                threads: 4,
                commits: 25,
            },
        ),
        (
            "ordinary-direct",
            Config {
                group: false,
                direct: true,
                idle_direct: true,
                threads: 2,
                commits: 25,
            },
        ),
        (
            "ordinary-buffered",
            Config {
                group: false,
                direct: false,
                idle_direct: true,
                threads: 2,
                commits: 25,
            },
        ),
    ] {
        crash_images(name, cfg, false, false);
    }
}

/// The crash images of a dataset with full-text search (X09 section 17.8). The writer's
/// commits group, and the full-text index applies them only after their fence. Each image
/// checks, besides the commits, that the index's checkpoint names no commit above the
/// recovered log and that full-text search finds exactly the recovered data. The writer
/// checkpoints the index at every fence before its synchronization and from a side
/// thread, so the images hold checkpoints of the run's own commits. Applying text changes
/// before their fence makes this test fail. The checkpoint at each fence needs the
/// `failpoints` feature, which a workspace build turns on through `sparkles-shacl`.
/// Without it the images would hold no checkpoint taken between a fence's apply and its
/// synchronization, and the test would pass that mutation, so it is ignored instead.
#[cfg(feature = "text")]
#[cfg_attr(
    not(feature = "failpoints"),
    ignore = "needs the failpoints feature to checkpoint the index at each fence"
)]
#[test]
fn crash_images_of_a_dataset_with_full_text_search() {
    for (name, cfg) in [
        (
            "text-group",
            Config {
                group: true,
                direct: false,
                idle_direct: false,
                threads: 4,
                commits: 25,
            },
        ),
        (
            "text-ordinary",
            Config {
                group: false,
                direct: false,
                idle_direct: true,
                threads: 2,
                commits: 15,
            },
        ),
    ] {
        crash_images(name, cfg, true, false);
    }
}

/// Exercise the actual unlocked background path separately from the fence/explicit
/// checkpoint cases above. A bounded shim handshake starts a designated transaction
/// after sync entry and holds that sync until its receipt. The test rejects a run
/// without either a real checkpoint or fresh publication inside one successful sync.
#[cfg(all(feature = "text", feature = "failpoints"))]
#[test]
fn crash_images_during_background_full_text_checkpoints() {
    for group in [false, true] {
        crash_images(
            if group {
                "background-text-group"
            } else {
                "background-text-ordinary"
            },
            Config {
                group,
                direct: false,
                idle_direct: false,
                threads: 4,
                commits: 50,
            },
            true,
            true,
        );
    }
}

// ------------------------------------------------------- a failed first annotation sync

const RETRY: &str = "SPARKLES_PL_RETRY";

/// The writer process of `a_failed_first_annotation_sync_keeps_later_messages`: two serial
/// updates that carry a message each. Prints `LINE first ...` and `LINE second ...`.
#[test]
fn child_annotation_retry() {
    let Ok(spec) = std::env::var(RETRY) else {
        return;
    };
    let db = PathBuf::from(std::env::var("SPARKLES_PL_DB").unwrap());
    let s = Store::open(
        &db,
        StoreOptions {
            group_commit: spec == "group",
            group_commit_idle_direct: true,
            ..Default::default()
        },
    )
    .unwrap();
    // faults count the calls after the store has opened
    std::fs::write(std::env::var("SPARKLES_IO_ARM").unwrap(), b"armed").unwrap();
    for (name, k) in [("first", 0), ("second", 1)] {
        let mut w = s.write_with(
            CommitKind::Update,
            sparkles_core::guard::WriteOptions {
                message: Some(name.into()),
                ..Default::default()
            },
        );
        let ids = w.encode_quad(&quad(1, k), &mut Default::default()).unwrap();
        w.insert(ids).unwrap();
        match w.commit() {
            Ok(r) => println!("LINE {name} ok {}", r.commit.seq),
            Err(e) => println!("LINE {name} err {e}"),
        }
    }
    std::process::exit(0);
}

/// The first annotation of a database creates `annotations.bin` and its header. When the
/// sync of that first record fails, the record is taken back and the header stays, so the
/// next message lands after the header and survives a reopen.
fn annotation_retry(name: &str, mode: &str) {
    let root = work_root().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("db");
    // a database that has no annotation file yet
    drop(Store::open(&db, StoreOptions::default()).unwrap());
    assert!(!db.join("annotations.bin").exists(), "{name}");
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_annotation_retry", "--nocapture"])
        .env(RETRY, mode)
        .env("SPARKLES_PL_DB", &db)
        .env("SPARKLES_IO_ROOT", &root)
        .env("SPARKLES_IO_ARM", root.join("armed"))
        .env("SPARKLES_IO_FAULT", "fdatasync:annotations.bin:1")
        .env("LD_PRELOAD", shim())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{name}: writer failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("LINE ").map(str::to_string))
        .collect();
    assert!(
        lines.iter().any(|l| l.starts_with("first err ")),
        "{name}: the first message was acknowledged: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l == "second ok 1"),
        "{name}: the second message did not take sequence 1: {lines:?}"
    );
    let s = Store::open(&db, StoreOptions::default()).unwrap();
    assert_eq!(s.snapshot().commit, 1, "{name}");
    assert_eq!(
        s.annotation(1).and_then(|a| a.message).as_deref(),
        Some("second"),
        "{name}: the acknowledged message was lost"
    );
    drop(s);
    let report = check(&db, &CheckOptions::default()).unwrap();
    assert!(
        report.status != Status::Error,
        "{name}: sparkles check fails:\n{}",
        report.to_text()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failed_first_annotation_sync_keeps_later_messages() {
    annotation_retry("ann-retry-ordinary", "ordinary");
    // the idle direct path of group commit, which is on by default
    annotation_retry("ann-retry-group", "group");
}

// ------------------------------------------------------------ failed synchronizations

/// Fail the calls named by `fault` in a run of `cfg`, and check the guarantees of spec X09
/// section 4.3.
fn failed_sync(name: &str, cfg: Config, fault: &str) {
    let base = 3;
    let run = run_writer(name, cfg, base, false, fault, false, false);
    let mut acked = Vec::new();
    let (mut head, mut after) = (None, None);
    let mut errors = 0;
    for l in &run.lines {
        let f: Vec<&str> = l.splitn(4, ' ').collect();
        match f[0] {
            "ok" => acked.push((
                f[3].parse::<u64>().unwrap(),
                f[1].parse().unwrap(),
                f[2].parse().unwrap(),
            )),
            "err" => {
                errors += 1;
                assert!(
                    f[3].contains("poison")
                        || f[3].contains("write-ahead log failed")
                        || f[3].contains("I/O")
                        || f[3].contains("Input/output")
                        || f[3].contains("os error 5"),
                    "{name}: unexpected error {l}"
                );
            }
            "head" => head = Some(f[1].parse::<u64>().unwrap()),
            "after" => after = Some(f[1] == "true"),
            _ => panic!("{l}"),
        }
    }
    assert!(
        errors > 0,
        "{name}: the fault {fault} failed no commit:\n{:?}",
        run.lines
    );
    // contiguous acknowledgements, and nothing published beyond the last of them
    let mut seqs: Vec<u64> = acked.iter().map(|a| a.0).collect();
    seqs.sort_unstable();
    let top = seqs.last().copied().unwrap_or(0).max(u64::from(base));
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "{name}: acknowledged commits are not contiguous: {seqs:?}"
    );
    assert_eq!(
        head,
        Some(top),
        "{name}: a commit past the last acknowledged one was published"
    );
    assert_eq!(
        after,
        Some(false),
        "{name}: the poisoned store took a write"
    );
    // the files left behind: a prefix that holds every acknowledged commit
    let s = Store::open(&run.db(), StoreOptions::default()).unwrap();
    let snap = s.snapshot();
    assert!(snap.commit >= top, "{name}");
    assert_eq!(snap.len(), snap.commit, "{name}");
    drop(snap);
    for (seq, t, k) in &acked {
        assert!(
            holds(&s, *t, *k),
            "{name}: acknowledged commit {seq} is missing"
        );
        assert_eq!(
            s.annotation(*seq).and_then(|a| a.message).as_deref(),
            Some(message(*t, *k).as_str()),
            "{name}: acknowledged commit {seq} ({t},{k}) lost its message"
        );
    }
    drop(s);
    let _ = std::fs::remove_dir_all(&run.root);
}

#[test]
fn failed_synchronizations_fail_unacknowledged_commits_and_publish_nothing() {
    let grouped = Config {
        group: true,
        direct: false,
        idle_direct: true,
        threads: 4,
        commits: 12,
    };
    let ordinary = Config {
        group: false,
        direct: false,
        idle_direct: true,
        threads: 1,
        commits: 12,
    };
    let direct = Config {
        group: false,
        direct: true,
        idle_direct: true,
        threads: 1,
        commits: 12,
    };
    // the first sync, one during a run of fences, the log alone, the vocabulary alone,
    // and both at once
    failed_sync("g-first", grouped, "fdatasync:wal.log:1");
    failed_sync("g-later", grouped, "fdatasync:wal.log:6");
    failed_sync("g-vocab", grouped, "fdatasync:delta.vocab:4");
    failed_sync(
        "g-both",
        grouped,
        "fdatasync:wal.log:5,fdatasync:delta.vocab:5",
    );
    failed_sync("g-wal-from", grouped, "fdatasync:wal.log:3+");
    failed_sync("o-first", ordinary, "fdatasync:wal.log:1");
    failed_sync("o-later", ordinary, "fdatasync:wal.log:7");
    failed_sync("o-vocab", ordinary, "fdatasync:delta.vocab:5");
    failed_sync("d-write", direct, "pwrite:wal.log:5");
    failed_sync("d-vocab", direct, "pwrite:delta.vocab:5");
    // the annotation file alone, in a run of fences
    failed_sync(
        "g-ann",
        Config {
            idle_direct: false,
            ..grouped
        },
        "fdatasync:annotations.bin:2",
    );
}

#[test]
fn a_single_writer_annotation_fence_fault_is_poisoning() {
    failed_sync(
        "g-ann-single",
        Config {
            group: true,
            direct: false,
            idle_direct: false,
            threads: 1,
            commits: 12,
        },
        "fdatasync:annotations.bin:2",
    );
}
