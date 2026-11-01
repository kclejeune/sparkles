//! The instruction-count screen of ROADMAP item 473: user-space instructions of
//! vocabulary scans and of WAL replay, counted with the CPU's performance counters, so
//! that a change to these readers can be checked below the noise of a timing. Run it in
//! release mode on Linux:
//!
//! ```text
//! mise run bench:insn
//! cargo test --release -p sparkles-core --test insn_screen -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Each case runs `INSN_SCREEN_REPS` times (default 5) on the calling thread with a
//! counter of the thread's user-space instructions, and the median is reported. On a
//! hybrid Intel CPU a counter is opened on each core type's PMU and the counts are summed,
//! so the thread may move between cores. The counter covers the calling thread only,
//! which is where these readers run. Each case also records the CPU time of the process's
//! other threads during the case, and a check fails when it is more than two clock ticks,
//! since work moved to another thread would otherwise look like a saving.
//!
//! `INSN_SCREEN_ITEMS` sets the size (20,000 items, about 160,000 triples, by default),
//! `INSN_SCREEN_COMMITS` the commits that WAL replay reads (2,000 by default) and
//! `INSN_SCREEN_JSON=path` appends each metric and check as a JSON line for
//! `scripts/bench-gate.py metrics`. The counter needs `kernel.perf_event_paranoid` at 2
//! or less, and the test fails when it cannot open one. It builds on Linux only.
#![cfg(target_os = "linux")]

use sparkles_core::builder::{BuildOptions, VocabCodec};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::vocab::Vocab;
use std::io::Write as _;
use std::path::{Path, PathBuf};

mod counter {
    use std::os::fd::{FromRawFd, OwnedFd};

    /// `struct perf_event_attr` up to `config1` (PERF_ATTR_SIZE_VER0).
    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        kind: u32,
        size: u32,
        config: u64,
        sample_period: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup_events: u32,
        bp_type: u32,
        config1: u64,
    }

    const PERF_TYPE_HARDWARE: u32 = 0;
    const PERF_COUNT_HW_INSTRUCTIONS: u64 = 1;
    const DISABLED: u64 = 1;
    const EXCLUDE_KERNEL: u64 = 1 << 5;
    const EXCLUDE_HV: u64 = 1 << 6;
    const IOC_ENABLE: u64 = 0x2400;
    const IOC_DISABLE: u64 = 0x2401;
    const IOC_RESET: u64 = 0x2403;

    /// Counters of the calling thread's user-space instructions, one per core PMU.
    pub struct Instructions {
        fds: Vec<OwnedFd>,
        pub pmus: Vec<String>,
    }

    impl Instructions {
        pub fn open() -> std::io::Result<Instructions> {
            // A hybrid CPU names a PMU per core type, and a generic hardware event then
            // takes the PMU's type in the upper half of its config.
            let mut targets = Vec::new();
            for pmu in ["cpu_core", "cpu_atom"] {
                let path = format!("/sys/bus/event_source/devices/{pmu}/type");
                if let Ok(t) = std::fs::read_to_string(path) {
                    let t: u64 = t.trim().parse().map_err(std::io::Error::other)?;
                    targets.push((pmu.to_string(), (t << 32) | PERF_COUNT_HW_INSTRUCTIONS));
                }
            }
            if targets.is_empty() {
                targets.push(("cpu".to_string(), PERF_COUNT_HW_INSTRUCTIONS));
            }
            let mut fds = Vec::new();
            let mut pmus = Vec::new();
            for (pmu, config) in targets {
                let attr = Attr {
                    kind: PERF_TYPE_HARDWARE,
                    size: std::mem::size_of::<Attr>() as u32,
                    config,
                    flags: DISABLED | EXCLUDE_KERNEL | EXCLUDE_HV,
                    ..Default::default()
                };
                // SAFETY: the attribute is a valid PERF_ATTR_SIZE_VER0 struct that outlives
                // the call; pid 0 and cpu -1 count the calling thread on any CPU.
                let fd = unsafe {
                    libc::syscall(
                        libc::SYS_perf_event_open,
                        &attr as *const Attr,
                        0 as libc::pid_t,
                        -1 as libc::c_int,
                        -1 as libc::c_int,
                        0 as libc::c_ulong,
                    )
                };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // SAFETY: the kernel returned a new descriptor that nothing else owns.
                fds.push(unsafe { OwnedFd::from_raw_fd(fd as i32) });
                pmus.push(pmu);
            }
            Ok(Instructions { fds, pmus })
        }

        fn ioctl(&self, request: u64) {
            for fd in &self.fds {
                use std::os::fd::AsRawFd;
                // SAFETY: a perf event descriptor and a request that takes no argument.
                let r = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, 0) };
                assert_eq!(r, 0, "perf ioctl: {}", std::io::Error::last_os_error());
            }
        }

        /// The instructions `f` executed on this thread.
        pub fn count<R>(&self, f: impl FnOnce() -> R) -> (u64, R) {
            self.ioctl(IOC_RESET);
            self.ioctl(IOC_ENABLE);
            let r = std::hint::black_box(f());
            self.ioctl(IOC_DISABLE);
            let mut total = 0;
            for fd in &self.fds {
                use std::io::Read as _;
                let mut buf = [0u8; 8];
                let mut file = std::fs::File::from(fd.try_clone().unwrap());
                file.read_exact(&mut buf).unwrap();
                total += u64::from_ne_bytes(buf);
            }
            (total, r)
        }
    }
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Literals, IRIs, numbers, dates and quoted triples, so that every vocabulary section
/// has keys.
fn data(items: usize) -> String {
    let mut s = String::from(
        "@prefix ex: <http://example.org/> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for i in 0..items {
        s.push_str(&format!(
            "ex:item{i} ex:label \"Item number {i} of the collection\"@en ;\n\
             \tex:note \"A longer note about item {i}, which repeats words that FSST codes\" ;\n\
             \tex:size {} ; ex:when \"2020-01-{:02}\"^^xsd:date ;\n\
             \tex:link <http://dbpedia.org/resource/Thing_{}> , ex:item{} .\n",
            i * 7,
            i % 28 + 1,
            i * 7919 % 100_003,
            (i * 31 + 7) % items,
        ));
        if i % 100 == 0 {
            s.push_str(&format!(
                "ex:item{i} ex:says <<( ex:item{i} ex:size {} )>> .\n",
                i * 7
            ));
        }
    }
    s
}

fn options(codec: Option<VocabCodec>) -> StoreOptions {
    StoreOptions {
        build: BuildOptions {
            vocab_codec: codec,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Output of the screen: a Markdown table on stdout and JSON lines for the gate record.
struct Out {
    json: Option<std::fs::File>,
}

impl Out {
    fn new() -> Out {
        let json = std::env::var_os("INSN_SCREEN_JSON").map(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .unwrap()
        });
        println!("| case | instructions (median) | min | max | other threads ms |");
        println!("|---|--:|--:|--:|--:|");
        Out { json }
    }

    fn line(&mut self, v: serde_json::Value) {
        if let Some(f) = &mut self.json {
            writeln!(f, "{v}").unwrap();
        }
    }

    fn case(&mut self, key: &str, mut counts: Vec<u64>, other_ticks: u64) {
        counts.sort_unstable();
        let median = counts[counts.len() / 2];
        let tick_ms = 1000 / clock_ticks();
        println!(
            "| {key} | {median} | {} | {} | {} |",
            counts[0],
            counts[counts.len() - 1],
            other_ticks * tick_ms
        );
        self.line(serde_json::json!({
            "key": key, "value": median, "unit": "instructions", "better": "lower",
            "samples": counts,
        }));
        self.line(serde_json::json!({
            "check": format!("{key}/single-thread"), "ok": other_ticks <= 2,
            "detail": format!("{} ms of CPU time on other threads", other_ticks * tick_ms),
        }));
    }
}

fn clock_ticks() -> u64 {
    // SAFETY: sysconf has no preconditions.
    (unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).max(1) as u64
}

/// User and system clock ticks of this process's threads other than the calling one.
fn other_thread_ticks() -> u64 {
    // SAFETY: gettid has no preconditions.
    let me = unsafe { libc::gettid() }.to_string();
    let mut total = 0;
    for e in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        if e.file_name().to_string_lossy() == me {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else {
            continue;
        };
        // the fields after the command, which is in parentheses
        let rest = &stat[stat.rfind(')').unwrap() + 2..];
        let f: Vec<&str> = rest.split(' ').collect();
        total += f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap();
    }
    total
}

fn measure<R>(
    out: &mut Out,
    c: &counter::Instructions,
    key: &str,
    reps: usize,
    mut f: impl FnMut() -> R,
) {
    let mut counts = Vec::with_capacity(reps);
    let before = other_thread_ticks();
    for _ in 0..reps {
        counts.push(c.count(&mut f).0);
    }
    out.case(key, counts, other_thread_ticks() - before);
}

/// A deterministic sequence of ids below `n`.
fn ids(n: u64, count: usize) -> Vec<u64> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..count)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        })
        .collect()
}

fn vocab_cases(out: &mut Out, c: &counter::Instructions, reps: usize, items: usize) {
    for (name, codec) in [
        ("front-coded", Some(VocabCodec::Front)),
        ("fsst", Some(VocabCodec::Fsst)),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), options(codec)).unwrap();
        s.load(&[Source::from_bytes(
            data(items).into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let gen_dir = s.snapshot().generation.dir.clone().unwrap();
        // the store's background threads would run beside the counted cases
        drop(s);
        let fsst = codec == Some(VocabCodec::Fsst);
        let v = Vocab::open_codec(&gen_dir, fsst).unwrap();
        assert_eq!(
            v.is_fsst(),
            fsst,
            "{name}: the store was not built with its codec"
        );
        let n = v.len();
        assert!(n > items as u64, "{name}: {n} keys");
        let mut keys = Vec::new();
        v.for_each(|i, k| {
            if i % 13 == 0 {
                keys.push(k.to_vec());
            }
        });
        let missing: Vec<Vec<u8>> = keys
            .iter()
            .map(|k| {
                let mut k = k.clone();
                k.push(1);
                k
            })
            .collect();
        let prefixes: Vec<Vec<u8>> = keys
            .iter()
            .step_by(10)
            .map(|k| k[..k.len().min(12)].to_vec())
            .collect();
        let mut sorted: Vec<u64> = (0..n).step_by(7).collect();
        sorted.sort_unstable();
        let random = ids(n, 50_000);
        let p = format!("vocab/{name}");
        measure(out, c, &format!("{p}/open"), reps, || {
            Vocab::open_codec(&gen_dir, fsst).unwrap().len()
        });
        measure(out, c, &format!("{p}/for-each"), reps, || {
            let mut bytes = 0usize;
            v.for_each(|_, k| bytes += k.len());
            bytes
        });
        measure(out, c, &format!("{p}/get-sorted"), reps, || {
            let mut bytes = 0usize;
            v.get_sorted(&sorted, |_, k| bytes += k.len());
            bytes
        });
        measure(out, c, &format!("{p}/get-random"), reps, || {
            random
                .iter()
                .map(|&i| v.get(i).map_or(0, |k| k.len()))
                .sum::<usize>()
        });
        measure(out, c, &format!("{p}/find-present"), reps, || {
            keys.iter().filter(|k| v.find(k).is_ok()).count()
        });
        measure(out, c, &format!("{p}/find-missing"), reps, || {
            missing.iter().filter(|k| v.find(k).is_ok()).count()
        });
        measure(out, c, &format!("{p}/prefix-range"), reps, || {
            prefixes
                .iter()
                .map(|k| {
                    let (lo, hi) = v.prefix_range(k);
                    hi - lo
                })
                .sum::<u64>()
        });
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), &target).unwrap();
        }
    }
}

/// The bytes of every `wal.log` under `dir`.
fn wal_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if e.file_type().unwrap().is_dir() {
            total += wal_bytes(&p);
        } else if e.file_name() == "wal.log" {
            total += e.metadata().unwrap().len();
        }
    }
    total
}

/// Open fresh copies of `src` and count each open.
fn opens(c: &counter::Instructions, src: &Path, reps: usize, seq: u64) -> (Vec<u64>, u64) {
    let mut counts = Vec::new();
    let mut ticks = 0;
    for _ in 0..reps {
        let scratch = tempfile::tempdir().unwrap();
        let dir: PathBuf = scratch.path().join("db");
        copy_dir(src, &dir);
        let before = other_thread_ticks();
        let (n, s) = c.count(|| Store::open(&dir, StoreOptions::default()).unwrap());
        ticks += other_thread_ticks() - before;
        assert_eq!(
            s.snapshot().commit,
            seq,
            "the copy opened at another commit"
        );
        counts.push(n);
        drop(s);
    }
    (counts, ticks)
}

fn wal_cases(out: &mut Out, c: &counter::Instructions, reps: usize, items: usize) {
    let commits = env("INSN_SCREEN_COMMITS", 2000);
    let base = tempfile::tempdir().unwrap();
    let base_dir = base.path().join("db");
    let s = Store::open(&base_dir, StoreOptions::default()).unwrap();
    s.load(&[Source::from_bytes(
        data(items).into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    let seq0 = s.snapshot().commit;
    drop(s);
    let empty_wal = wal_bytes(&base_dir);
    let changed = tempfile::tempdir().unwrap();
    let changed_dir = changed.path().join("db");
    copy_dir(&base_dir, &changed_dir);
    let s = Store::open(&changed_dir, StoreOptions::default()).unwrap();
    let o = QueryOptions::default();
    let p = "PREFIX ex: <http://example.org/> ";
    for i in 0..commits {
        let u = match i % 4 {
            // new terms
            0 | 1 => {
                format!("{p}INSERT DATA {{ ex:new{i} ex:label \"new item {i}\" ; ex:size {i} }}")
            }
            // known terms
            2 => format!(
                "{p}INSERT DATA {{ ex:item{} ex:link ex:item{} }}",
                i % items,
                (i * 7) % items
            ),
            _ => format!(
                "{p}DELETE DATA {{ ex:item{} ex:size {} }}",
                i % items,
                (i % items) * 7
            ),
        };
        update(&s, &u, &o).unwrap();
    }
    let seq = s.snapshot().commit;
    drop(s);
    let wal = wal_bytes(&changed_dir);
    assert!(
        wal > empty_wal + commits as u64 * 16,
        "the commits are not in the WAL ({empty_wal} bytes before, {wal} after)"
    );
    out.line(serde_json::json!({
        "check": "wal/replay/commits-in-wal", "ok": true,
        "detail": format!("{commits} commits, {wal} WAL bytes against {empty_wal} without them"),
    }));
    let (empty, t0) = opens(c, &base_dir, reps, seq0);
    let (replay, t1) = opens(c, &changed_dir, reps, seq);
    let med = |v: &[u64]| {
        let mut v = v.to_vec();
        v.sort_unstable();
        v[v.len() / 2]
    };
    let diff: Vec<u64> = replay
        .iter()
        .map(|r| r.saturating_sub(med(&empty)))
        .collect();
    out.case("wal/open-without-wal", empty, t0);
    out.case(&format!("wal/open-with-{commits}-commits"), replay, t1);
    // the open of the same store without the log, subtracted
    out.case(&format!("wal/replay-{commits}-commits"), diff, 0);
}

#[test]
#[ignore]
fn instruction_counts() {
    let c = counter::Instructions::open().unwrap_or_else(|e| {
        panic!(
            "cannot open a performance counter ({e}); kernel.perf_event_paranoid must be 2 or less"
        )
    });
    eprintln!("counting instructions:u on {:?}", c.pmus);
    let reps = env("INSN_SCREEN_REPS", 5);
    let items = env("INSN_SCREEN_ITEMS", 20_000);
    let mut out = Out::new();
    vocab_cases(&mut out, &c, reps, items);
    wal_cases(&mut out, &c, reps, items);
}
