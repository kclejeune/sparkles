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
//!    write at the next sequence number.
//!  * `failed_synchronizations_*` fail one call and check that every commit that was not
//!    acknowledged fails, that nothing past the last acknowledged commit is published, and
//!    that the store reopens to a prefix that holds every acknowledged commit.
//!
//! `SPARKLES_POWER_LOSS_POINTS` sets the number of crash points per configuration (default
//! 60) and `SPARKLES_POWER_LOSS_SEED` the seed of the random choices.
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
    threads: u32,
    commits: u32,
}

impl Config {
    fn options(&self) -> StoreOptions {
        StoreOptions {
            experimental_group_commit: self.group,
            wal_direct_writes: self.direct,
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
    let cfg = Config {
        group: f[0] == "1",
        direct: f[1] == "1",
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
    // faults count the calls after the store has opened
    if let Ok(arm) = std::env::var("SPARKLES_IO_ARM") {
        std::fs::write(arm, b"armed").unwrap();
    }
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut hs = Vec::new();
    for t in 1..=cfg.threads {
        let (s, ack, out) = (s.clone(), ack.clone(), out.clone());
        hs.push(std::thread::spawn(move || {
            for k in 0..cfg.commits {
                let mut w = s.write_as(CommitKind::Update);
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
                    }
                    Err(e) => {
                        out.lock().unwrap().push(format!("err {t} {k} {e}"));
                        return;
                    }
                }
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
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
fn run_writer(name: &str, cfg: Config, base: u32, record: bool, fault: &str) -> Run {
    let root = work_root().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("ack")).unwrap();
    let db = root.join("db");
    {
        let s = Store::open(&db, StoreOptions::default()).unwrap();
        base_commit(&s, base);
    }
    copy_dir(&db, &root.join("base-db"));
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "child_writer", "--nocapture"])
        .env(
            CHILD,
            format!(
                "{},{},{},{}",
                u8::from(cfg.group),
                u8::from(cfg.direct),
                cfg.threads,
                cfg.commits
            ),
        )
        .env("SPARKLES_PL_DB", &db)
        .env("SPARKLES_PL_ACK", root.join("ack/ack.log"))
        .env("SPARKLES_IO_ROOT", &root)
        .env("SPARKLES_IO_ARM", root.join("armed"))
        .env("LD_PRELOAD", shim());
    if record {
        cmd.env("SPARKLES_IO_LOG", root.join("io.log"));
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

/// The files of the store as the writer process left them in the page cache and on the
/// device, rebuilt record by record.
struct Model {
    files: BTreeMap<String, FileModel>,
}

impl Model {
    fn apply(&mut self, r: &Rec) {
        match r {
            Rec::Write {
                path,
                off,
                data,
                dsync,
            } => self.files.entry(path.clone()).or_default().ops.push(Op {
                off: *off,
                data: Some(data.clone()),
                state: State::Pending,
                dsync: *dsync,
                returned: false,
            }),
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
            Rec::Trunc { path, len } => self.files.entry(path.clone()).or_default().ops.push(Op {
                off: *len,
                data: None,
                state: State::Pending,
                dsync: false,
                returned: true,
            }),
            Rec::Sync { path } => {
                let f = self.files.entry(path.clone()).or_default();
                f.covered = (0..f.ops.len())
                    .filter(|i| f.ops[*i].returned && f.ops[*i].state == State::Pending)
                    .collect();
            }
            Rec::SyncEnd { path, failed } => {
                if !failed {
                    let f = self.files.entry(path.clone()).or_default();
                    for i in std::mem::take(&mut f.covered) {
                        f.ops[i].state = State::Durable;
                    }
                }
            }
        }
    }

    /// The bytes of every tracked file: the base content, then the durable operations,
    /// then the pending ones that `keep` chooses (or part of them, per 4 KiB page).
    fn image(
        &self,
        base: &Path,
        root: &Path,
        out: &Path,
        mut keep: impl FnMut(&Op) -> Option<Vec<bool>>,
    ) {
        let _ = std::fs::remove_dir_all(out);
        copy_dir(base, out);
        for (path, f) in &self.files {
            let Ok(rel) = Path::new(path).strip_prefix(root.join("db")) else {
                continue;
            };
            let target = out.join(rel);
            // a directory that was only synchronized
            if f.ops.is_empty() || target.is_dir() {
                continue;
            }
            let mut bytes = std::fs::read(&target).unwrap_or_default();
            for op in &f.ops {
                let pages = match op.state {
                    State::Dropped => continue,
                    State::Durable => None,
                    State::Pending => match keep(op) {
                        None => continue,
                        some => some,
                    },
                };
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
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&target, bytes).unwrap();
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

/// Check one image against the commits acknowledged before the crash.
fn check_image(img: &Path, base: u32, acked: &[(u64, u32, u32)], what: &str) {
    let s = Store::open(img, StoreOptions::default())
        .unwrap_or_else(|e| panic!("{what}: the image does not open: {e}"));
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
}

fn crash_images(name: &str, cfg: Config) {
    let base = 3;
    let run = run_writer(name, cfg, base, true, "");
    let log = parse_log(&std::fs::read(run.log()).unwrap());
    let root = run.root.canonicalize().unwrap();
    // the paths in the log are canonical
    let mut model = Model {
        files: BTreeMap::new(),
    };
    let points = env_num("SPARKLES_POWER_LOSS_POINTS", 60) as usize;
    let stride = (log.len() / points.max(1)).max(1);
    let mut rng = Rng(env_num("SPARKLES_POWER_LOSS_SEED", 0x9e3779b97f4a7c15) | 1);
    let mut acked: Vec<(u64, u32, u32)> = Vec::new();
    let img = run.root.join("image");
    let mut images = 0;
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
        model.image(&base_db, &root, &img, |_| None);
        check_image(&img, base, &acked, &format!("{what}, unsynced writes lost"));
        // everything is kept, as after a process crash
        model.image(&base_db, &root, &img, |_| Some(Vec::new()));
        check_image(&img, base, &acked, &format!("{what}, nothing lost"));
        // a random part of what is pending survives, in whole writes or single pages
        for _ in 0..2 {
            model.image(&base_db, &root, &img, |op| {
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
            check_image(&img, base, &acked, &format!("{what}, random pages lost"));
        }
        images += 4;
    }
    assert!(images > 0);
    assert!(!acked.is_empty(), "the writer acknowledged no commit");
    let _ = std::fs::remove_dir_all(&run.root);
}

#[test]
fn crash_images_keep_every_acknowledged_commit() {
    for (name, cfg) in [
        (
            "group-direct",
            Config {
                group: true,
                direct: true,
                threads: 4,
                commits: 25,
            },
        ),
        (
            "group-buffered",
            Config {
                group: true,
                direct: false,
                threads: 4,
                commits: 25,
            },
        ),
        (
            "ordinary-direct",
            Config {
                group: false,
                direct: true,
                threads: 2,
                commits: 25,
            },
        ),
        (
            "ordinary-buffered",
            Config {
                group: false,
                direct: false,
                threads: 2,
                commits: 25,
            },
        ),
    ] {
        crash_images(name, cfg);
    }
}

// ------------------------------------------------------------ failed synchronizations

/// Fail the calls named by `fault` in a run of `cfg`, and check the guarantees of spec X09
/// section 4.3.
fn failed_sync(name: &str, cfg: Config, fault: &str) {
    let base = 3;
    let run = run_writer(name, cfg, base, false, fault);
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
    }
    drop(s);
    let _ = std::fs::remove_dir_all(&run.root);
}

#[test]
fn failed_synchronizations_fail_unacknowledged_commits_and_publish_nothing() {
    let grouped = Config {
        group: true,
        direct: false,
        threads: 4,
        commits: 12,
    };
    let ordinary = Config {
        group: false,
        direct: false,
        threads: 1,
        commits: 12,
    };
    let direct = Config {
        group: false,
        direct: true,
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
}
