//! What a build writes and reads, per phase, for its log.
//!
//! Each phase of a build ends with one `sparkles::builder` log line. It gives the
//! phase's wall time, the bytes the process sent to storage and read from it (from
//! `/proc/self/io` on Linux), and the bytes the phase wrote to each kind of temporary
//! file. The storage counters include the bytes of temporary files that were deleted
//! before they reached the device, which the kernel reports as cancelled writes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// The kinds of temporary file a build writes.
#[derive(Clone, Copy, Debug)]
pub(super) enum Tmp {
    /// a batch's quads in batch-local ids (`b*.q`)
    Quads,
    /// a batch's sorted distinct keys (`b*.voc`)
    PartialVocab,
    /// the hot ids a batch uses (`b*.h`)
    HotLists,
    /// a batch's map from key rank to global id (`b*.map`)
    Maps,
    /// a range of the merged vocabulary (`vocab-range-*`)
    VocabRanges,
    /// a sorted run of one order (`run-*`)
    Runs,
    /// a part of a permutation's regrouped first column (`*-spill-*`)
    Spills,
}

const KINDS: [(Tmp, &str); 7] = [
    (Tmp::Quads, "quads"),
    (Tmp::PartialVocab, "partial vocabularies"),
    (Tmp::HotLists, "hot key lists"),
    (Tmp::Maps, "maps"),
    (Tmp::VocabRanges, "vocabulary ranges"),
    (Tmp::Runs, "runs"),
    (Tmp::Spills, "spills"),
];

/// Bytes written to temporary files, by kind.
#[derive(Default)]
pub(super) struct TmpBytes([AtomicU64; KINDS.len()]);

impl TmpBytes {
    pub(super) fn add(&self, kind: Tmp, n: u64) {
        self.0[kind as usize].fetch_add(n, Ordering::Relaxed);
    }

    fn get(&self) -> [u64; KINDS.len()] {
        std::array::from_fn(|i| self.0[i].load(Ordering::Relaxed))
    }
}

/// The storage counters of `/proc/self/io`.
#[derive(Clone, Copy, Default)]
struct ProcIo {
    read: u64,
    write: u64,
    cancelled: u64,
}

fn proc_io() -> Option<ProcIo> {
    let s = std::fs::read_to_string("/proc/self/io").ok()?;
    let mut io = ProcIo::default();
    for line in s.lines() {
        let (k, v) = line.split_once(':')?;
        let v: u64 = v.trim().parse().ok()?;
        match k {
            "read_bytes" => io.read = v,
            "write_bytes" => io.write = v,
            "cancelled_write_bytes" => io.cancelled = v,
            _ => {}
        }
    }
    Some(io)
}

/// The start of the current phase.
pub(super) struct Phases {
    at: Instant,
    io: Option<ProcIo>,
    tmp: [u64; KINDS.len()],
}

impl Phases {
    pub(super) fn start() -> Phases {
        Phases {
            at: Instant::now(),
            io: proc_io(),
            tmp: [0; KINDS.len()],
        }
    }

    /// Log the phase `name` that ends now, and start the next one.
    pub(super) fn end(&mut self, name: &str, tmp: &TmpBytes) {
        let now = Phases {
            at: Instant::now(),
            io: proc_io(),
            tmp: tmp.get(),
        };
        let mb = |b: u64| b as f64 / 1e6;
        let mut msg = format!(
            "build phase {name}: {:.2}s",
            now.at.duration_since(self.at).as_secs_f64()
        );
        if let (Some(a), Some(b)) = (self.io, now.io) {
            msg += &format!(
                ", storage written {:.1} MB (cancelled {:.1} MB), read {:.1} MB",
                mb(b.write.saturating_sub(a.write)),
                mb(b.cancelled.saturating_sub(a.cancelled)),
                mb(b.read.saturating_sub(a.read)),
            );
        }
        let parts: Vec<String> = KINDS
            .iter()
            .filter_map(|&(k, n)| {
                let d = now.tmp[k as usize] - self.tmp[k as usize];
                (d > 0).then(|| format!("{n} {:.1} MB", mb(d)))
            })
            .collect();
        if !parts.is_empty() {
            msg += &format!(", temporary files written: {}", parts.join(", "));
        }
        tracing::info!(target: "sparkles::builder", "{msg}");
        *self = now;
    }
}
