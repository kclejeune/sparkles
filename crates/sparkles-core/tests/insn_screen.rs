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
//! other threads during the case, and a check fails when it is more than 20 ms, since
//! work moved to another thread would otherwise look like a saving.
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
    const FORMAT_TOTAL_TIME_ENABLED: u64 = 1;
    const FORMAT_TOTAL_TIME_RUNNING: u64 = 2;
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
                    read_format: FORMAT_TOTAL_TIME_ENABLED | FORMAT_TOTAL_TIME_RUNNING,
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

        /// Time that a counted interval may miss without counting as multiplexed.
        const SLACK_NS: u64 = 20_000;

        /// Each counter's value, time enabled and time running. A reset clears the value
        /// but not the times, so a measurement takes the difference of two reads.
        fn read(&self) -> Vec<[u64; 3]> {
            self.fds
                .iter()
                .map(|fd| {
                    use std::io::Read as _;
                    let mut buf = [0u8; 24];
                    let mut file = std::fs::File::from(fd.try_clone().unwrap());
                    file.read_exact(&mut buf).unwrap();
                    std::array::from_fn(|i| {
                        u64::from_ne_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap())
                    })
                })
                .collect()
        }

        /// The instructions `f` executed on this thread, and the share of the time the
        /// counters were enabled in this interval that one of them was counting. A thread
        /// runs on one core type at a time, so the share is 1 unless the kernel
        /// multiplexed the counters with other events and missed part of the work. The
        /// kernel's accounting at enable, disable and a move between core types leaves
        /// gaps of a few microseconds, which `SLACK_NS` forgives. Multiplexing rotates
        /// events at the scheduler tick, so it misses at least a millisecond.
        pub fn count<R>(&self, f: impl FnOnce() -> R) -> (u64, f64, R) {
            self.ioctl(IOC_RESET);
            let before = self.read();
            self.ioctl(IOC_ENABLE);
            let r = std::hint::black_box(f());
            self.ioctl(IOC_DISABLE);
            let after = self.read();
            let (mut total, mut enabled, mut running) = (0, 0, 0);
            for (b, a) in before.iter().zip(&after) {
                total += a[0] - b[0];
                enabled = enabled.max(a[1] - b[1]);
                running += a[2] - b[2];
            }
            let coverage = if enabled == 0 {
                1.0
            } else {
                ((running + Self::SLACK_NS) as f64 / enabled as f64).min(1.0)
            };
            (total, coverage, r)
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

    fn check(&mut self, name: &str, ok: bool, detail: String) {
        if !ok {
            eprintln!("check failed: {name}: {detail}");
        }
        self.line(serde_json::json!({ "check": name, "ok": ok, "detail": detail }));
    }

    /// A check whose fingerprint, such as a digest of loaded or recovered contents, a
    /// comparison requires to equal the baseline's.
    fn check_fp(&mut self, name: &str, ok: bool, detail: String, fingerprint: String) {
        if !ok {
            eprintln!("check failed: {name}: {detail}");
        }
        self.line(serde_json::json!({
            "check": name, "ok": ok, "detail": detail, "fingerprint": fingerprint,
        }));
    }

    fn case(&mut self, key: &str, mut counts: Vec<u64>, other_us: u64, coverage: f64) {
        counts.sort_unstable();
        let median = counts[counts.len() / 2];
        let other_ms = other_us as f64 / 1000.0;
        println!(
            "| {key} | {median} | {} | {} | {other_ms:.1} |",
            counts[0],
            counts[counts.len() - 1],
        );
        self.line(serde_json::json!({
            "key": key, "value": median, "unit": "instructions", "better": "lower",
            "samples": counts,
        }));
        self.check(
            &format!("{key}/single-thread"),
            other_us <= OTHER_THREADS_US,
            format!("{other_ms:.1} ms of CPU time on other threads"),
        );
        self.check(
            &format!("{key}/counter-coverage"),
            coverage >= 0.99,
            format!("the counters ran for {coverage:.4} of the enabled time"),
        );
    }
}

/// The CPU time that the process's other threads may use during a case, in microseconds.
const OTHER_THREADS_US: u64 = 20_000;

/// A CPU-time clock in microseconds.
fn cpu_us(clock: libc::clockid_t) -> i64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid clock id and a pointer to a live timespec.
    assert_eq!(unsafe { libc::clock_gettime(clock, &mut t) }, 0);
    t.tv_sec * 1_000_000 + t.tv_nsec / 1000
}

/// The CPU time in microseconds of this process's threads other than the calling one,
/// including threads that have exited: the process's CPU clock, which keeps the time of
/// joined threads, less the calling thread's own. Both clocks count nanoseconds, so the
/// difference is off only by the calling thread's time between the two reads, which is
/// signed; `us_since` clamps an interval at zero.
fn other_thread_us() -> i64 {
    let process = cpu_us(libc::CLOCK_PROCESS_CPUTIME_ID);
    process - cpu_us(libc::CLOCK_THREAD_CPUTIME_ID)
}

/// Wait until the process's other threads have used less than 0.1 ms of CPU time in 50 ms,
/// at most five seconds, so that a dropped store's threads winding down or a pool's
/// workers spinning after a query are not counted as work of the next case.
fn quiesce() {
    for _ in 0..100 {
        let t = other_thread_us();
        std::thread::sleep(std::time::Duration::from_millis(50));
        if other_thread_us() - t < 100 {
            return;
        }
    }
}

/// Other-thread microseconds since `before`, a value of `other_thread_us`.
fn us_since(before: i64) -> u64 {
    (other_thread_us() - before).max(0) as u64
}

/// Count `f` `reps` times and check that every run returned `expected`, which the caller
/// computed another way, so that a reader that breaks cannot pass as a cheaper one.
fn measure<R: PartialEq + std::fmt::Debug>(
    out: &mut Out,
    c: &counter::Instructions,
    key: &str,
    reps: usize,
    expected: R,
    mut f: impl FnMut() -> R,
) {
    let mut counts = Vec::with_capacity(reps);
    let mut coverage = 1f64;
    let mut wrong = None;
    quiesce();
    let before = other_thread_us();
    for _ in 0..reps {
        let (n, share, r) = c.count(&mut f);
        counts.push(n);
        coverage = coverage.min(share);
        if r != expected {
            wrong = Some(r);
        }
    }
    let other = us_since(before);
    let detail = match &wrong {
        None => format!("{expected:?}"),
        Some(r) => format!("{r:?}, expected {expected:?}"),
    };
    out.check(&format!("{key}/result"), wrong.is_none(), detail);
    out.case(key, counts, other, coverage);
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
        assert_eq!(v.is_fsst(), fsst, "{name}: not built with its codec");
        let n = v.len();
        assert!(n > items as u64, "{name}: {n} keys");
        // every key, read once by the scan and compared with lookups by id
        let mut all: Vec<Vec<u8>> = Vec::with_capacity(n as usize);
        v.for_each(|i, k| {
            assert_eq!(
                i,
                all.len() as u64,
                "{name}: the scan's ids are not in order"
            );
            all.push(k.to_vec());
        });
        assert_eq!(all.len() as u64, n, "{name}: the scan missed keys");
        for (i, k) in all.iter().enumerate().step_by(97) {
            assert_eq!(
                v.get(i as u64).as_deref(),
                Some(k.as_slice()),
                "{name}: key {i}"
            );
        }
        assert!(all.is_sorted(), "{name}: the keys are not in order");
        // the vocabulary's contents, which a comparison anchors to the baseline's
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for k in &all {
            for &b in k.iter().chain(&[0]) {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        out.check_fp(
            &format!("vocab/{name}/keys"),
            true,
            format!("{n} keys"),
            format!("{n} keys, {h:016x}"),
        );
        let lens: Vec<usize> = all.iter().map(Vec::len).collect();
        let keys: Vec<Vec<u8>> = all.iter().step_by(13).cloned().collect();
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
        let sorted: Vec<u64> = (0..n).step_by(7).collect();
        let random = ids(n, 50_000);
        // each prefix's range, from the scanned keys, before the ranges are counted
        for p in &prefixes {
            let lo = all.partition_point(|k| k.as_slice() < p.as_slice()) as u64;
            let hi = lo
                + all[lo as usize..]
                    .iter()
                    .take_while(|k| k.starts_with(p))
                    .count() as u64;
            assert_eq!(v.prefix_range(p), (lo, hi), "{name}: prefix range");
        }
        // the ids and keys of the sorted and random reads, compared in full before they
        // are counted
        let mut got = Vec::with_capacity(sorted.len());
        v.get_sorted(&sorted, |i, k| got.push((i, k.to_vec())));
        let want: Vec<(u64, Vec<u8>)> = sorted
            .iter()
            .map(|&i| (i, all[i as usize].clone()))
            .collect();
        assert!(got == want, "{name}: get_sorted returned other ids or keys");
        for &i in &random {
            assert_eq!(
                v.get(i).as_deref(),
                Some(all[i as usize].as_slice()),
                "{name}: key {i}"
            );
        }
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(v.find(k), Ok(13 * i as u64), "{name}: find");
        }
        // the expected results, from the scanned keys
        let id_sum: u64 = (0..n).step_by(13).sum();
        let prefix_total: u64 = prefixes
            .iter()
            .map(|p| {
                let lo = all.partition_point(|k| k.as_slice() < p.as_slice());
                all[lo..].iter().take_while(|k| k.starts_with(p)).count() as u64
            })
            .sum();
        let sorted_bytes: usize = sorted.iter().map(|&i| lens[i as usize]).sum();
        let random_bytes: usize = random.iter().map(|&i| lens[i as usize]).sum();
        let p = format!("vocab/{name}");
        measure(out, c, &format!("{p}/open"), reps, n, || {
            Vocab::open_codec(&gen_dir, fsst).unwrap().len()
        });
        measure(
            out,
            c,
            &format!("{p}/for-each"),
            reps,
            lens.iter().sum(),
            || {
                let mut bytes = 0usize;
                v.for_each(|_, k| bytes += k.len());
                bytes
            },
        );
        measure(
            out,
            c,
            &format!("{p}/get-sorted"),
            reps,
            sorted_bytes,
            || {
                let mut bytes = 0usize;
                v.get_sorted(&sorted, |_, k| bytes += k.len());
                bytes
            },
        );
        measure(
            out,
            c,
            &format!("{p}/get-random"),
            reps,
            random_bytes,
            || {
                random
                    .iter()
                    .map(|&i| v.get(i).map_or(0, |k| k.len()))
                    .sum::<usize>()
            },
        );
        measure(out, c, &format!("{p}/find-present"), reps, id_sum, || {
            keys.iter()
                .map(|k| v.find(k).unwrap_or(u64::MAX / 4))
                .sum::<u64>()
        });
        measure(out, c, &format!("{p}/find-missing"), reps, 0, || {
            missing.iter().filter(|k| v.find(k).is_ok()).count()
        });
        measure(
            out,
            c,
            &format!("{p}/prefix-range"),
            reps,
            prefix_total,
            || {
                prefixes
                    .iter()
                    .map(|k| {
                        let (lo, hi) = v.prefix_range(k);
                        hi - lo
                    })
                    .sum::<u64>()
            },
        );
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

/// A store's commit, its quads and an FNV-1a digest of every quad in sorted order, made
/// outside the counted interval.
type Content = (u64, u64, u64);

fn content(s: &Store) -> Content {
    let r = sparkles_core::sparql::query(
        s.snapshot(),
        "SELECT ?g ?s ?p ?o WHERE { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }",
        &QueryOptions {
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let mut rows: Vec<String> = r
        .rows()
        .unwrap()
        .into_iter()
        .map(|row| {
            row.values()
                .iter()
                .map(|t| t.as_ref().map_or("-".into(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    rows.sort_unstable();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for row in &rows {
        for b in row.bytes().chain(*b"\n") {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    let snap = s.snapshot();
    assert_eq!(rows.len() as u64, snap.len(), "the digest missed quads");
    (snap.commit, snap.len(), h)
}

/// The counts of opening fresh copies of a store, and whether each recovered it.
struct Opens {
    counts: Vec<u64>,
    other_us: u64,
    coverage: f64,
    recovered: bool,
}

fn opens(c: &counter::Instructions, src: &Path, reps: usize, want: &Content) -> Opens {
    let mut o = Opens {
        counts: Vec::new(),
        other_us: 0,
        coverage: 1.0,
        recovered: true,
    };
    for _ in 0..reps {
        let scratch = tempfile::tempdir().unwrap();
        let dir: PathBuf = scratch.path().join("db");
        copy_dir(src, &dir);
        quiesce();
        let before = other_thread_us();
        let (n, share, s) = c.count(|| Store::open(&dir, StoreOptions::default()).unwrap());
        o.other_us += us_since(before);
        o.coverage = o.coverage.min(share);
        o.recovered &= content(&s) == *want;
        o.counts.push(n);
        drop(s);
    }
    o
}

/// The `i`th of the commits that WAL replay reads.
fn commit(i: usize, items: usize) -> String {
    let p = "PREFIX ex: <http://example.org/> ";
    match i % 4 {
        // new terms
        0 | 1 => format!("{p}INSERT DATA {{ ex:new{i} ex:label \"new item {i}\" ; ex:size {i} }}"),
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
    }
}

/// FNV-1a over `parts`, each followed by a zero byte.
fn fnv(parts: &[&str]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for p in parts {
        for b in p.bytes().chain([0]) {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// The cases' parameters beyond the data and the commits, which a change to the screen
/// bumps so that a comparison with an older baseline fails.
const CASES: &str = "vocab: open, for-each, get-sorted every 7th, get-random 50000, find every 13th, \
                     missing, prefix 12 bytes of every 130th; wal: open, open with commits, replay; v1";

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
    let before = content(&s);
    drop(s);
    let empty_wal = wal_bytes(&base_dir);
    let changed = tempfile::tempdir().unwrap();
    let changed_dir = changed.path().join("db");
    copy_dir(&base_dir, &changed_dir);
    let s = Store::open(&changed_dir, StoreOptions::default()).unwrap();
    let o = QueryOptions::default();
    for i in 0..commits {
        update(&s, &commit(i, items), &o).unwrap();
    }
    let after = content(&s);
    drop(s);
    assert_ne!(before, after, "the commits changed nothing");
    let wal = wal_bytes(&changed_dir);
    assert!(
        wal > empty_wal + commits as u64 * 16,
        "the commits are not in the WAL ({empty_wal} bytes before, {wal} after)"
    );
    out.check(
        "wal/replay/commits-in-wal",
        true,
        format!("{commits} commits, {wal} WAL bytes against {empty_wal} without them"),
    );
    let empty = opens(c, &base_dir, reps, &before);
    let replay = opens(c, &changed_dir, reps, &after);
    let key = format!("wal/open-with-{commits}-commits");
    // the contents before and after the commits, which a comparison anchors to the
    // baseline's, so that a loader that loses facts in both stores alike fails
    out.check_fp(
        "wal/open-without-wal/recovered",
        empty.recovered,
        format!("{before:?}"),
        format!("{before:?}"),
    );
    out.check_fp(
        &format!("{key}/recovered"),
        replay.recovered,
        format!("{after:?}"),
        format!("{after:?}"),
    );
    let mut sorted = empty.counts.clone();
    sorted.sort_unstable();
    let empty_median = sorted[sorted.len() / 2];
    let diff: Vec<u64> = replay
        .counts
        .iter()
        .map(|r| r.saturating_sub(empty_median))
        .collect();
    out.case(
        "wal/open-without-wal",
        empty.counts,
        empty.other_us,
        empty.coverage,
    );
    out.case(&key, replay.counts, replay.other_us, replay.coverage);
    // the open of the same store without the log, subtracted
    let coverage = empty.coverage.min(replay.coverage);
    out.case(&format!("wal/replay-{commits}-commits"), diff, 0, coverage);
}

/// The other-thread guard sees a worker that ran and was joined while a case ran.
#[test]
fn joined_threads_count_as_other_threads() {
    let before = other_thread_us();
    std::thread::spawn(|| {
        // 300 ms of this thread's own CPU time, however busy the machine is
        let start = cpu_us(libc::CLOCK_THREAD_CPUTIME_ID);
        let mut x = 0u64;
        while cpu_us(libc::CLOCK_THREAD_CPUTIME_ID) - start < 300_000 {
            x = std::hint::black_box(x.wrapping_add(1));
        }
    })
    .join()
    .unwrap();
    let other = us_since(before);
    assert!(other >= 100_000, "{other} us");
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
    // the workload, so that a comparison with a baseline of other data, commits or cases
    // fails even when every result check holds
    let commits = env("INSN_SCREEN_COMMITS", 2000);
    let all: String = (0..commits).map(|i| commit(i, items)).collect();
    let digest = fnv(&[&data(items), &all, CASES]);
    out.line(serde_json::json!({
        "check": "workload", "ok": true, "fingerprint": format!("{digest:016x}"),
        "detail": format!("{items} items, {commits} commits"),
    }));
    vocab_cases(&mut out, &c, reps, items);
    wal_cases(&mut out, &c, reps, items);
}
