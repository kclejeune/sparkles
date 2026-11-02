//! Fence-shaped I/O on two files, to measure write-through and flushing fences with the
//! drive in each state. Build: rustc --edition 2024 -O fence-io-probe.rs
//!
//! usage: fence-io-probe DIR DEVICE SCHEDULE...
//!   SCHEDULE items are PATTERN:SECONDS, run in order. Patterns:
//!     wt2   two O_DIRECT|O_DSYNC writes in parallel, one per file (a write-through fence)
//!     wt2s  the same two writes one after the other
//!     wt1   one O_DIRECT|O_DSYNC write (an ordinary write-through commit of the log alone)
//!     fl2   two buffered writes, then fdatasync of both files in parallel (a flushing fence)
//!     fl1   one buffered write and fdatasync
//!     idle  sleep
//!   Each fence writes the 4 KiB block that holds each file's logical end, which advances
//!   by STEP bytes (env FENCE_STEP, default 300) per fence, into preallocated zeros.
//! Prints one line per second: elapsed, pattern, fences, mean/p50/p90/max fence ms, and the
//! device's writes, mean write ms, flushes and mean flush ms from /proc/diskstats.
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const O_DIRECT: i32 = 0o40000;
const O_DSYNC: i32 = 0o10000;
const BLOCK: usize = 4096;
const SIZE: u64 = 256 << 20;

struct Aligned(*mut u8);
unsafe impl Send for Aligned {}
impl Aligned {
    fn new() -> Aligned {
        let l = std::alloc::Layout::from_size_align(2 * BLOCK, BLOCK).unwrap();
        // SAFETY: nonzero size
        let p = unsafe { std::alloc::alloc_zeroed(l) };
        assert!(!p.is_null());
        Aligned(p)
    }
    fn buf(&self, n: usize) -> &[u8] {
        // SAFETY: 2*BLOCK bytes allocated
        unsafe { std::slice::from_raw_parts(self.0, n) }
    }
}

struct Files {
    buffered: File,
    direct: File,
    pos: u64,
}

fn open(dir: &str, name: &str) -> Files {
    let path = format!("{dir}/{name}");
    let mut f = OpenOptions::new().create(true).truncate(true).read(true).write(true).open(&path).unwrap();
    let zeros = vec![0u8; 1 << 20];
    for _ in 0..(SIZE >> 20) {
        f.write_all(&zeros).unwrap();
    }
    f.sync_all().unwrap();
    let d = OpenOptions::new().read(true).write(true).custom_flags(O_DIRECT | O_DSYNC).open(&path).unwrap();
    Files { buffered: f, direct: d, pos: 0 }
}

fn diskstats(dev: &str) -> [u64; 4] {
    let s = std::fs::read_to_string("/proc/diskstats").unwrap();
    for l in s.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() > 2 && f[2] == dev {
            let n = |i: usize| f.get(i).and_then(|v| v.parse().ok()).unwrap_or(0u64);
            // writes completed, ms writing, flushes completed, ms flushing
            return [n(7), n(10), n(18), n(19)];
        }
    }
    panic!("no device {dev} in /proc/diskstats");
}

enum Op {
    Direct(u64, usize),
    Buffered(u64, usize),
    Sync,
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (dir, dev) = (&a[1], &a[2]);
    let step: u64 = std::env::var("FENCE_STEP").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let mut log = open(dir, "probe-log.bin");
    let vocab = open(dir, "probe-vocab.bin");
    // the second file lives on a worker, as the vocabulary's writes do
    let (tx, rx) = mpsc::sync_channel::<Op>(1);
    let (done_tx, done) = mpsc::sync_channel::<()>(1);
    let worker = std::thread::spawn(move || {
        let v = vocab;
        let b = Aligned::new();
        for op in rx {
            match op {
                Op::Direct(at, n) => v.direct.write_all_at(b.buf(n), at).unwrap(),
                Op::Buffered(at, n) => v.buffered.write_all_at(&b.buf(n)[..n], at).unwrap(),
                Op::Sync => v.buffered.sync_data().unwrap(),
            }
            done_tx.send(()).unwrap();
        }
    });
    let b = Aligned::new();
    let start = Instant::now();
    let mut vpos = 0u64;
    for item in &a[3..] {
        let (pat, secs) = item.split_once(':').unwrap();
        let secs: u64 = secs.parse().unwrap();
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            let sec_end = (Instant::now() + Duration::from_secs(1)).min(end);
            let ds0 = diskstats(dev);
            let mut lat = Vec::new();
            if pat == "idle" {
                std::thread::sleep(sec_end - Instant::now());
            }
            while pat != "idle" && Instant::now() < sec_end {
                let at = log.pos - log.pos % BLOCK as u64;
                let n = if (log.pos + step) as usize / BLOCK > at as usize / BLOCK { 2 * BLOCK } else { BLOCK };
                let vat = vpos - vpos % BLOCK as u64;
                let vn = if (vpos + step) as usize / BLOCK > vat as usize / BLOCK { 2 * BLOCK } else { BLOCK };
                let t = Instant::now();
                match pat {
                    "wt2" => {
                        tx.send(Op::Direct(vat, vn)).unwrap();
                        log.direct.write_all_at(b.buf(n), at).unwrap();
                        done.recv().unwrap();
                    }
                    "wt2s" => {
                        log.direct.write_all_at(b.buf(n), at).unwrap();
                        tx.send(Op::Direct(vat, vn)).unwrap();
                        done.recv().unwrap();
                    }
                    "wt1" => log.direct.write_all_at(b.buf(n), at).unwrap(),
                    "fl2" => {
                        log.buffered.write_all_at(&b.buf(n)[..n], at).unwrap();
                        tx.send(Op::Buffered(vat, vn)).unwrap();
                        done.recv().unwrap();
                        tx.send(Op::Sync).unwrap();
                        log.buffered.sync_data().unwrap();
                        done.recv().unwrap();
                    }
                    "fl1" => {
                        log.buffered.write_all_at(&b.buf(n)[..n], at).unwrap();
                        log.buffered.sync_data().unwrap();
                    }
                    p => panic!("unknown pattern {p}"),
                }
                lat.push(t.elapsed().as_secs_f64() * 1000.0);
                log.pos = (log.pos + step) % (SIZE - 4 * BLOCK as u64);
                vpos = (vpos + step) % (SIZE - 4 * BLOCK as u64);
            }
            let ds1 = diskstats(dev);
            let d: Vec<u64> = (0..4).map(|i| ds1[i] - ds0[i]).collect();
            lat.sort_by(|x, y| x.partial_cmp(y).unwrap());
            let q = |p: f64| if lat.is_empty() { 0.0 } else { lat[((lat.len() - 1) as f64 * p) as usize] };
            let mean = if lat.is_empty() { 0.0 } else { lat.iter().sum::<f64>() / lat.len() as f64 };
            println!(
                "{:7.1} {pat:5} fences={:5} mean={:.3} p50={:.3} p90={:.3} max={:.3} dev_w={} wms={:.3} dev_fl={} fms={:.3}",
                start.elapsed().as_secs_f64(),
                lat.len(), mean, q(0.5), q(0.9), q(1.0),
                d[0], if d[0] > 0 { d[1] as f64 / d[0] as f64 } else { 0.0 },
                d[2], if d[2] > 0 { d[3] as f64 / d[2] as f64 } else { 0.0 },
            );
            std::io::stdout().flush().unwrap();
        }
    }
    drop(tx);
    worker.join().unwrap();
    let _ = std::fs::remove_file(format!("{dir}/probe-log.bin"));
    let _ = std::fs::remove_file(format!("{dir}/probe-vocab.bin"));
}
