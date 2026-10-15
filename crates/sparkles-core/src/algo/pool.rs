//! The threads of the algorithm library (spec F14 §5).
//!
//! Algorithms run on rayon pools of their own, apart from the reasoner's, so that a long
//! algorithm leaves the query threads free. A process-wide count of threads in use
//! (`--algo-threads`, half the cores by default) is shared by every call, and a call
//! waits for the threads it asks for, checking its cancel flag and deadline while it does.

use super::Control;
use crate::error::Result;
use parking_lot::{Condvar, Mutex};
use rayon::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static CAPACITY: AtomicUsize = AtomicUsize::new(0);
static PER_CALL: AtomicUsize = AtomicUsize::new(4);
static EXACT_BETWEENNESS_MAX: AtomicUsize = AtomicUsize::new(100_000);

struct Slots {
    in_use: Mutex<usize>,
    freed: Condvar,
}

static SLOTS: Slots = Slots {
    in_use: Mutex::new(0),
    freed: Condvar::new(),
};

/// Set the threads algorithms may use together (`--algo-threads`) and the most one call
/// may take (`--algo-max-threads-per-call`). Zero keeps the default.
pub fn configure(threads: usize, per_call: usize) {
    CAPACITY.store(threads, Ordering::Relaxed);
    if per_call > 0 {
        PER_CALL.store(per_call, Ordering::Relaxed);
    }
}

/// Set the most nodes betweenness runs on without sampling (`--algo-exact-betweenness-max`).
pub fn set_exact_betweenness_max(n: usize) {
    EXACT_BETWEENNESS_MAX.store(n, Ordering::Relaxed);
}

/// The most nodes betweenness runs on without sampling.
pub fn max_exact_betweenness() -> usize {
    EXACT_BETWEENNESS_MAX.load(Ordering::Relaxed)
}

/// The threads all algorithm calls may use together.
pub fn capacity() -> usize {
    match CAPACITY.load(Ordering::Relaxed) {
        0 => (std::thread::available_parallelism().map_or(2, |n| n.get()) / 2).max(1),
        n => n,
    }
}

/// The most threads one call may take.
pub fn max_per_call() -> usize {
    PER_CALL.load(Ordering::Relaxed).max(1)
}

fn build_pool(threads: usize) -> Option<rayon::ThreadPool> {
    #[cfg(test)]
    match tests::build_mode() {
        tests::BuildMode::Normal => {}
        tests::BuildMode::Error => {
            return rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .spawn_handler(|_| Err(std::io::Error::other("injected pool spawn failure")))
                .build()
                .ok();
        }
        tests::BuildMode::Unwind => panic!("injected pool construction unwind"),
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .ok()
}

/// The threads of one call. The threads go back to the process when it drops.
pub struct Par {
    threads: usize,
    pool: Option<rayon::ThreadPool>,
    held: usize,
}

impl Par {
    /// One thread outside process admission, which runs every loop in the caller.
    pub fn single() -> Par {
        Par {
            threads: 1,
            pool: None,
            held: 0,
        }
    }

    /// A call with exactly `n` threads, outside the process-wide count (tests, benches).
    pub fn with_threads(n: usize) -> Par {
        let n = n.max(1);
        if n == 1 {
            return Par::single();
        }
        Par {
            threads: n,
            pool: rayon::ThreadPoolBuilder::new().num_threads(n).build().ok(),
            held: 0,
        }
    }

    /// Take `requested` threads (0: the call limit), capped by the call limit and the
    /// process capacity, waiting while they are in use. A one-thread call runs in the
    /// caller without a Rayon pool, but holds one slot until it drops.
    pub fn acquire(requested: usize, ctl: &Control) -> Result<Par> {
        ctl.check()?;
        let cap = capacity();
        let want = match requested {
            0 => max_per_call(),
            n => n.min(max_per_call()),
        }
        .min(cap)
        .max(1);
        let mut in_use = SLOTS.in_use.lock();
        loop {
            ctl.check()?;
            if *in_use <= cap - want {
                *in_use += want;
                break;
            }
            #[cfg(test)]
            tests::waiting();
            SLOTS.freed.wait_for(&mut in_use, Duration::from_millis(20));
            #[cfg(test)]
            tests::woken(*in_use);
        }
        // Establish the release guard before pool construction, including unwinding.
        let mut par = Par {
            threads: want,
            pool: None,
            held: want,
        };
        drop(in_use);
        if want > 1 {
            par.pool = build_pool(want);
        }
        Ok(par)
    }

    /// The threads of this call.
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Run `f(lo, hi)` for each block of `block` indexes of `0..n`, on the call's
    /// threads, and return the results in block order. Fails with the first error.
    pub fn blocks<T: Send>(
        &self,
        n: usize,
        block: usize,
        f: impl Fn(usize, usize) -> Result<T> + Sync + Send,
    ) -> Result<Vec<T>> {
        let block = block.max(1);
        let nb = n.div_ceil(block);
        let run = |b: usize| f(b * block, ((b + 1) * block).min(n));
        match &self.pool {
            Some(p) if nb > 1 => p.install(|| (0..nb).into_par_iter().map(run).collect()),
            _ => (0..nb).map(run).collect(),
        }
    }

    /// Sort `data`; equal elements are indistinguishable to the caller.
    pub fn sort<T: Ord + Send>(&self, data: &mut [T]) {
        match &self.pool {
            Some(p) if data.len() > 1 << 14 => p.install(|| data.par_sort_unstable()),
            _ => data.sort_unstable(),
        }
    }

    /// Run `f(lo, chunk)` on disjoint chunks of `data` of `block` elements, `lo` being
    /// the index of the chunk's first element.
    pub fn chunks_mut<T: Send>(
        &self,
        data: &mut [T],
        block: usize,
        f: impl Fn(usize, &mut [T]) -> Result<()> + Sync + Send,
    ) -> Result<()> {
        let block = block.max(1);
        let run = |(i, c): (usize, &mut [T])| f(i * block, c);
        match &self.pool {
            Some(p) if data.len() > block => p.install(|| {
                data.par_chunks_mut(block)
                    .enumerate()
                    .map(run)
                    .collect::<Result<Vec<()>>>()
                    .map(|_| ())
            }),
            _ => data.chunks_mut(block).enumerate().try_for_each(run),
        }
    }
}

impl Drop for Par {
    fn drop(&mut self) {
        if self.held > 0 {
            let mut in_use = SLOTS.in_use.lock();
            *in_use = in_use.saturating_sub(self.held);
            drop(in_use);
            SLOTS.freed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("pool_admission_tests.rs");

    #[test]
    fn blocks_keep_order() {
        for par in [Par::single(), Par::with_threads(3)] {
            let r = par.blocks(10, 3, |lo, hi| Ok((lo, hi))).unwrap();
            assert_eq!(r, vec![(0, 3), (3, 6), (6, 9), (9, 10)]);
        }
    }

    #[test]
    fn chunks_fill() {
        let par = Par::with_threads(2);
        let mut v = vec![0usize; 10];
        par.chunks_mut(&mut v, 4, |lo, c| {
            for (i, x) in c.iter_mut().enumerate() {
                *x = lo + i;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(v, (0..10).collect::<Vec<_>>());
    }
}
