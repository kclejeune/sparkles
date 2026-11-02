//! Where the server's threads run on a processor with performance and efficiency cores.
//!
//! A request runs its sequential parts on the thread that serves it, a runtime worker or
//! a blocking-pool thread, and hands its parallel parts to the query pool. On a hybrid
//! processor whose every logical CPU holds a query thread, a request thread that wakes
//! after a parallel part often finds only an efficiency core or a busy hyperthread free
//! and stays there. On forge, an Intel Core i5-13500 with six performance cores of two
//! threads and eight efficiency cores, that took BSBM BI `q3` at 10M from 22 to 42 ms in
//! some server processes and not in others (roadmap item 449). So on such a processor
//! request work runs on the performance cores, and the query pool keeps fewer threads
//! than logical CPUs, so that a request thread finds a free CPU. The query pool itself
//! runs on every CPU, since its work steals and shares out across cores of either kind.
//!
//! The performance cores hold only so many requests at once. On atlas, an i5-13500 like
//! forge, sixteen readers and one writer with every request thread on the twelve logical
//! CPUs of the performance cores kept those CPUs 95% busy and the efficiency cores 30%
//! busy, and the request threads waited 131 s in the run queues over 30 s against 30 s
//! when they ran anywhere. That load answered 2,015 reads and 240 writes a second
//! against 2,345 and 362 (roadmap item 583). So request work runs on the performance
//! cores only while at most one request per performance core is in flight, and anywhere
//! when more are. Each thread changes its CPUs when it starts a request's work and its
//! placement differs from the one the load asks for, so a steady load changes nothing.
//!
//! Linux names the two kinds of core in `/sys/devices/cpu_core/cpus` and
//! `/sys/devices/cpu_atom/cpus`. Elsewhere, on a processor of one kind of core, and when
//! the process may run on cores of one kind only, every thread may run on every CPU and
//! the query pool has one thread per logical CPU, as before.

// Elsewhere than on Linux no `Hybrid` is ever made, so its helpers go unused there.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// The CPUs this process may run on, split by kind of core, when it may run on both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hybrid {
    /// logical CPUs of performance cores
    pub performance: Vec<usize>,
    /// logical CPUs of efficiency cores
    pub efficiency: Vec<usize>,
}

impl Hybrid {
    /// The cores of this machine that the process may run on, or `None` when they are
    /// not of two kinds.
    pub fn detect() -> Option<Hybrid> {
        #[cfg(target_os = "linux")]
        {
            let read = |p: &str| std::fs::read_to_string(p).ok();
            let allowed = linux::allowed()?;
            Hybrid::from_lists(
                &read("/sys/devices/cpu_core/cpus")?,
                &read("/sys/devices/cpu_atom/cpus")?,
                &allowed,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    /// The split of `allowed` by the CPU lists of the two kinds of core, in the kernel's
    /// list format (`0-11`, `0,2-5`).
    pub fn from_lists(core: &str, atom: &str, allowed: &[usize]) -> Option<Hybrid> {
        let core = parse_list(core)?;
        let atom = parse_list(atom)?;
        let performance: Vec<usize> = allowed
            .iter()
            .copied()
            .filter(|c| core.contains(c))
            .collect();
        let efficiency: Vec<usize> = allowed
            .iter()
            .copied()
            .filter(|c| atom.contains(c))
            .collect();
        (!performance.is_empty() && !efficiency.is_empty()).then_some(Hybrid {
            performance,
            efficiency,
        })
    }

    /// The default size of the query pool: one thread per logical CPU, less one for
    /// every four efficiency cores or part of four, which Intel's hybrid processors
    /// group into clusters of four. On forge's 20 logical CPUs, 18 threads measured as
    /// fast as 16 on the BSBM BI templates and faster on DBpedia's `births-by-decade`,
    /// and both were faster than 12, 14 and 20 (item 449).
    pub fn query_threads(&self) -> usize {
        let all = self.performance.len() + self.efficiency.len();
        all.saturating_sub(self.efficiency.len().div_ceil(4)).max(1)
    }
}

/// Where request work runs on a hybrid processor: on the performance cores while at most
/// `limit` requests are in flight, and on every CPU the process may use when more are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    performance: Vec<usize>,
    every: Vec<usize>,
    limit: usize,
}

impl Placement {
    /// The placement of `hybrid`, with one request in flight per performance core.
    pub fn new(hybrid: &Hybrid, performance_cores: usize) -> Placement {
        let mut every: Vec<usize> = hybrid
            .performance
            .iter()
            .chain(&hybrid.efficiency)
            .copied()
            .collect();
        every.sort_unstable();
        Placement {
            performance: hybrid.performance.clone(),
            every,
            limit: performance_cores.clamp(1, hybrid.performance.len()),
        }
    }

    /// The most requests in flight at which request work runs on the performance cores.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Whether request work runs on the performance cores with `active` requests in
    /// flight, the caller's included.
    pub fn on_performance(&self, active: usize) -> bool {
        active <= self.limit
    }

    /// Run the calling thread where request work goes with `active` requests in flight.
    /// A thread changes its CPUs only when its placement differs from the one it last
    /// set, and a thread that has set none sets one.
    fn place(&self, active: usize) {
        self.set(if self.on_performance(active) {
            Placed::Performance
        } else {
            Placed::Every
        });
    }

    fn set(&self, want: Placed) {
        PLACED.with(|this| {
            if this.placed.get() != want {
                // A thread is listed while it runs on the performance cores, so that a
                // sweep leaves it there.
                let mut listed = ON_PERFORMANCE.lock();
                if want == Placed::Performance {
                    listed.insert(this.tid);
                    pin_current_thread(&self.performance);
                } else {
                    pin_current_thread(&self.every);
                    if listed.remove(&this.tid) {
                        // threads it started while listed may still hold its CPUs
                        SWEEP.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                this.placed.set(want);
            }
        });
    }

    /// Move to every CPU each of `threads` that runs on the performance cores without
    /// having placed itself there. Such a thread inherited its CPUs from a request thread
    /// that started it, as the block decoders, the vocabulary sync worker and the change
    /// log publisher are started by the first request that needs them, and it would
    /// otherwise stay on the performance cores for its life.
    ///
    /// The list's lock is held for each thread, so a thread that places itself on the
    /// performance cores meanwhile, which lists itself before it moves, is left there. A
    /// thread id is acted on only through a pidfd that keeps it from being reused, and
    /// only once the id is known to be a thread of this process.
    fn sweep(&self, threads: impl IntoIterator<Item = i32>) -> usize {
        let mut moved = 0;
        for tid in threads {
            let listed = ON_PERFORMANCE.lock();
            if listed.contains(&tid) {
                continue;
            }
            let Some(_held) = own_thread(tid) else {
                continue;
            };
            if thread_cpus(tid).is_some_and(|c| c == self.performance)
                && pin_thread(tid, &self.every)
            {
                moved += 1;
            }
            drop(listed);
        }
        moved
    }
}

/// Whether a sweep is due: a thread runs on the performance cores by its own placement,
/// and may start threads that inherit them, or a thread left them since the last sweep.
fn sweep_due() -> bool {
    let left = SWEEP.swap(false, std::sync::atomic::Ordering::Relaxed);
    left || !ON_PERFORMANCE.lock().is_empty()
}

/// Where a thread last placed itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placed {
    /// Nowhere: the thread runs on the CPUs it inherited from the thread that created it,
    /// which may be a request thread on the performance cores.
    Unknown,
    Performance,
    Every,
}

static PLACEMENT: std::sync::OnceLock<Placement> = std::sync::OnceLock::new();

/// The threads that run on the performance cores because they placed themselves there.
static ON_PERFORMANCE: parking_lot::Mutex<std::collections::BTreeSet<i32>> =
    parking_lot::Mutex::new(std::collections::BTreeSet::new());

/// Whether a thread left the performance cores since the last sweep.
static SWEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How often the server looks for threads that inherited the performance cores, while
/// any thread runs there by its own placement.
const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// A thread's placement, which leaves the list of threads on the performance cores when
/// the thread ends, before the kernel may give its id to another thread.
struct ThreadPlacement {
    tid: i32,
    placed: std::cell::Cell<Placed>,
}

impl Drop for ThreadPlacement {
    fn drop(&mut self) {
        if self.placed.get() == Placed::Performance {
            ON_PERFORMANCE.lock().remove(&self.tid);
        }
    }
}

thread_local! {
    static PLACED: ThreadPlacement = ThreadPlacement {
        tid: current_tid(),
        placed: std::cell::Cell::new(Placed::Unknown),
    };
}

/// Place request work by `placement` from now on. The server calls it once at start, on
/// a hybrid processor with `--request-cpus auto`. A thread then sweeps the process's
/// threads once a second while a sweep is due (see [`sweep_due`]).
pub fn start(placement: Placement) {
    if PLACEMENT.set(placement).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("cpu-placement".into())
        .spawn(|| {
            let p = PLACEMENT.get().expect("placement set");
            loop {
                std::thread::sleep(SWEEP_INTERVAL);
                if sweep_due() {
                    let moved = p.sweep(process_threads());
                    if moved > 0 {
                        tracing::debug!(moved, "threads moved off the performance cores");
                    }
                }
            }
        });
}

/// Run a thread the runtime starts on every CPU. A new thread inherits the CPUs of the
/// thread that creates it, and the runtime creates threads from request threads, such as
/// the worker that replaces one running a request in place, so without this a thread
/// could stay on the performance cores under any load.
pub fn thread_started() {
    if let Some(p) = PLACEMENT.get() {
        p.set(Placed::Every);
    }
}

/// Run the calling thread where a request's work goes now, by the requests in flight.
/// Without a placement, as on a processor of one kind of core, it does nothing.
pub fn place_request() {
    if let Some(p) = PLACEMENT.get() {
        p.place(crate::alloc::active());
    }
}

/// The physical cores among `performance`, by the kernel's topology of each logical CPU,
/// or one per logical CPU where the kernel does not say.
pub fn physical_cores(performance: &[usize]) -> usize {
    #[cfg(target_os = "linux")]
    {
        let mut cores = std::collections::HashSet::new();
        for c in performance {
            let path = format!("/sys/devices/system/cpu/cpu{c}/topology/core_cpus_list");
            match std::fs::read_to_string(path)
                .ok()
                .and_then(|s| parse_list(&s))
            {
                Some(siblings) => {
                    cores.insert(siblings);
                }
                None => return performance.len(),
            }
        }
        cores.len()
    }
    #[cfg(not(target_os = "linux"))]
    {
        performance.len()
    }
}

/// The CPUs of a kernel CPU list such as `0-11` or `0,2-5,8`.
pub fn parse_list(s: &str) -> Option<Vec<usize>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        let cpu = |s: &str| s.trim().parse::<usize>().ok();
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (cpu(a)?, cpu(b)?),
            None => {
                let a = cpu(part)?;
                (a, a)
            }
        };
        if b < a || b - a > 1 << 16 {
            return None;
        }
        out.extend(a..=b);
    }
    Some(out)
}

/// Run the calling thread only on `cpus`. Where the system has no such control, or it
/// refuses, the thread keeps running where it may.
pub fn pin_current_thread(cpus: &[usize]) {
    #[cfg(target_os = "linux")]
    linux::pin(0, cpus);
    #[cfg(not(target_os = "linux"))]
    let _ = cpus;
}

/// Run the thread `tid` of this process only on `cpus`, and whether that succeeded.
fn pin_thread(tid: i32, cpus: &[usize]) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::pin(tid, cpus)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (tid, cpus);
        false
    }
}

/// The CPUs the thread `tid` of this process may run on.
fn thread_cpus(tid: i32) -> Option<Vec<usize>> {
    #[cfg(target_os = "linux")]
    {
        linux::allowed_of(tid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = tid;
        None
    }
}

/// A hold on the thread `tid` that keeps its id from being given to another thread, once
/// the id is known to name a thread of this process, or `None` when it does not or the
/// kernel cannot hold a thread (Linux before 6.9).
fn own_thread(tid: i32) -> Option<ThreadHold> {
    #[cfg(target_os = "linux")]
    {
        linux::own_thread(tid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = tid;
        None
    }
}

#[cfg(target_os = "linux")]
type ThreadHold = std::os::fd::OwnedFd;
#[cfg(not(target_os = "linux"))]
type ThreadHold = ();

/// The kernel's id of the calling thread.
fn current_tid() -> i32 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: gettid has no preconditions.
        unsafe { libc::gettid() }
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// The kernel's ids of this process's threads.
fn process_threads() -> Vec<i32> {
    std::fs::read_dir("/proc/self/task")
        .map(|d| {
            d.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
mod linux {
    /// A pidfd of the thread `tid`, which holds its id while it is open, once the id is
    /// known to name a thread of this process.
    pub fn own_thread(tid: i32) -> Option<std::os::fd::OwnedFd> {
        use std::os::fd::FromRawFd;
        // PIDFD_THREAD, which lets a pidfd name a thread other than the leader
        const PIDFD_THREAD: libc::c_uint = libc::O_EXCL as libc::c_uint;
        // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, tid, PIDFD_THREAD) };
        if fd < 0 {
            return None;
        }
        // SAFETY: the descriptor is new and owned by nothing else.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) };
        // the id cannot change hands while the pidfd is open, so this check holds
        std::path::Path::new(&format!("/proc/self/task/{tid}"))
            .exists()
            .then_some(fd)
    }

    /// The CPUs the calling thread may run on.
    pub fn allowed() -> Option<Vec<usize>> {
        allowed_of(0)
    }

    /// The CPUs the thread `tid` may run on, or the calling thread's for 0.
    pub fn allowed_of(tid: i32) -> Option<Vec<usize>> {
        // SAFETY: a zeroed cpu_set_t is an empty set, and sched_getaffinity writes at
        // most size_of::<cpu_set_t>() bytes into it.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
                return None;
            }
            Some(
                (0..libc::CPU_SETSIZE as usize)
                    .filter(|&c| libc::CPU_ISSET(c, &set))
                    .collect(),
            )
        }
    }

    /// Run the thread `tid`, or the calling thread for 0, only on `cpus`, and whether
    /// the kernel took it. A thread that has ended is refused.
    pub fn pin(tid: i32, cpus: &[usize]) -> bool {
        // SAFETY: CPU_SET writes within the zeroed set for CPUs below CPU_SETSIZE, and
        // sched_setaffinity reads size_of::<cpu_set_t>() bytes of it.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            for &c in cpus.iter().filter(|&&c| c < libc::CPU_SETSIZE as usize) {
                libc::CPU_SET(c, &mut set);
            }
            if libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
                tracing::debug!(
                    "could not place a thread: {}",
                    std::io::Error::last_os_error()
                );
                return false;
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kernel_cpu_lists() {
        assert_eq!(parse_list("0-3\n"), Some(vec![0, 1, 2, 3]));
        assert_eq!(parse_list("0,2-4,7"), Some(vec![0, 2, 3, 4, 7]));
        assert_eq!(parse_list("5"), Some(vec![5]));
        assert_eq!(parse_list(""), None);
        assert_eq!(parse_list("3-1"), None);
        assert_eq!(parse_list("a-b"), None);
    }

    #[test]
    fn splits_the_allowed_cpus_by_kind() {
        let all: Vec<usize> = (0..20).collect();
        let h = Hybrid::from_lists("0-11", "12-19", &all).unwrap();
        assert_eq!(h.performance, (0..12).collect::<Vec<_>>());
        assert_eq!(h.efficiency, (12..20).collect::<Vec<_>>());
        // forge: 20 logical CPUs, eight of them efficiency cores
        assert_eq!(h.query_threads(), 18);
        // a process confined to cores of one kind is not hybrid
        assert_eq!(Hybrid::from_lists("0-11", "12-19", &[0, 1, 2]), None);
        assert_eq!(Hybrid::from_lists("0-11", "12-19", &[12, 13]), None);
        // a mixed affinity keeps both kinds, and one efficiency core costs a thread
        let h = Hybrid::from_lists("0-11", "12-19", &[0, 1, 12]).unwrap();
        assert_eq!(
            (h.performance.len(), h.efficiency.len(), h.query_threads()),
            (2, 1, 2)
        );
        // sixteen efficiency cores leave four CPUs free
        let h = Hybrid::from_lists("0-15", "16-31", &(0..32).collect::<Vec<_>>()).unwrap();
        assert_eq!(h.query_threads(), 28);
        // an unreadable list is not hybrid
        assert_eq!(Hybrid::from_lists("", "12-19", &all), None);
    }

    #[test]
    fn request_work_leaves_the_performance_cores_past_one_request_per_core() {
        let all: Vec<usize> = (0..20).collect();
        let h = Hybrid::from_lists("0-11", "12-19", &all).unwrap();
        // forge and atlas: six performance cores of two threads each
        let p = Placement::new(&h, 6);
        assert_eq!(p.limit(), 6);
        assert!(p.on_performance(1));
        assert!(p.on_performance(6));
        assert!(!p.on_performance(7));
        assert!(!p.on_performance(17));
        assert_eq!(p.every, all);
        // the limit stays between one and the logical CPUs of the performance cores
        assert_eq!(Placement::new(&h, 0).limit(), 1);
        assert_eq!(Placement::new(&h, 40).limit(), 12);
        // efficiency cores listed first still give one sorted set
        let h = Hybrid::from_lists("4-7", "0-3", &(0..8).collect::<Vec<_>>()).unwrap();
        assert_eq!(Placement::new(&h, 4).every, (0..8).collect::<Vec<_>>());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn placing_request_work_follows_the_requests_in_flight() {
        let allowed = linux::allowed().unwrap();
        if allowed.len() < 2 {
            return;
        }
        let p = Placement {
            performance: vec![allowed[0]],
            every: allowed.clone(),
            limit: 2,
        };
        let got = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for active in [3, 1, 2, 5, 1] {
                p.place(active);
                seen.push(linux::allowed().unwrap());
            }
            seen
        })
        .join()
        .unwrap();
        let one = vec![allowed[0]];
        assert_eq!(
            got,
            vec![allowed.clone(), one.clone(), one.clone(), allowed, one]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_thread_created_by_a_placed_thread_leaves_the_performance_cores() {
        let allowed = linux::allowed().unwrap();
        if allowed.len() < 2 {
            return;
        }
        let p = std::sync::Arc::new(Placement {
            performance: vec![allowed[0]],
            every: allowed.clone(),
            limit: 2,
        });
        let got = std::thread::spawn(move || {
            // a request thread on the performance cores at low load
            p.place(1);
            let inherited = linux::allowed().unwrap();
            // a thread it creates, as the runtime's replacement worker would be, places
            // itself under load although it never set a placement
            let q = p.clone();
            let placed = std::thread::spawn(move || {
                q.place(5);
                linux::allowed().unwrap()
            })
            .join()
            .unwrap();
            // and a thread the runtime starts runs on every CPU from its start
            let q = p.clone();
            let started = std::thread::spawn(move || {
                q.set(Placed::Every);
                let at_start = linux::allowed().unwrap();
                q.place(5);
                (at_start, linux::allowed().unwrap())
            })
            .join()
            .unwrap();
            (inherited, placed, started)
        })
        .join()
        .unwrap();
        assert_eq!(got.0, vec![allowed[0]]);
        assert_eq!(got.1, allowed);
        assert_eq!(got.2, (allowed.clone(), allowed));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sweep_moves_threads_that_inherited_the_performance_cores() {
        let allowed = linux::allowed().unwrap();
        if allowed.len() < 2 {
            return;
        }
        let p = std::sync::Arc::new(Placement {
            performance: vec![allowed[0]],
            every: allowed.clone(),
            limit: 2,
        });
        let (to_test, from_parent) = std::sync::mpsc::channel::<(&str, i32)>();
        let (to_parent, from_test) = std::sync::mpsc::channel::<()>();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let q = p.clone();
        let parent = std::thread::spawn(move || {
            // a request thread on the performance cores
            q.place(1);
            to_test.send(("parent", current_tid())).unwrap();
            // it starts a long-lived worker only after a sweep has run
            from_test.recv().unwrap();
            let sent = to_test.clone();
            let child = std::thread::spawn(move || {
                sent.send(("child", current_tid())).unwrap();
                wait.recv().unwrap();
                linux::allowed().unwrap()
            });
            from_test.recv().unwrap();
            // and then leaves the performance cores under load
            q.place(5);
            to_test.send(("left", 0)).unwrap();
            child
        });
        let parent_tid = from_parent.recv().unwrap().1;
        assert!(ON_PERFORMANCE.lock().contains(&parent_tid));
        // a sweep stays due while a thread is listed
        assert!(sweep_due());
        assert_eq!(p.sweep(process_threads()), 0);
        to_parent.send(()).unwrap();
        let child_tid = from_parent.recv().unwrap().1;
        assert_eq!(thread_cpus(child_tid), Some(vec![allowed[0]]));
        assert!(sweep_due());
        assert_eq!(p.sweep(process_threads()), 1);
        assert_eq!(thread_cpus(child_tid), Some(allowed.clone()));
        to_parent.send(()).unwrap();
        assert_eq!(from_parent.recv().unwrap().0, "left");
        // leaving the performance cores makes a sweep due, for threads started meanwhile
        assert!(!ON_PERFORMANCE.lock().contains(&parent_tid));
        assert!(sweep_due());
        let child = parent.join().unwrap();
        release.send(()).unwrap();
        assert_eq!(child.join().unwrap(), allowed);

        // a thread on the performance cores by its own placement stays there, and leaves
        // the list when it ends, also by a panic
        for panics in [false, true] {
            let q = p.clone();
            let (tids, got_tids) = std::sync::mpsc::channel();
            let (release, wait) = std::sync::mpsc::channel::<()>();
            let placed = std::thread::spawn(move || {
                q.place(1);
                tids.send(current_tid()).unwrap();
                wait.recv().unwrap();
                assert!(!panics, "a request that panics");
                linux::allowed().unwrap()
            });
            let tid = got_tids.recv().unwrap();
            assert_eq!(p.sweep([tid]), 0);
            assert_eq!(thread_cpus(tid), Some(vec![allowed[0]]));
            release.send(()).unwrap();
            assert_eq!(placed.join().ok(), (!panics).then(|| vec![allowed[0]]));
            assert!(!ON_PERFORMANCE.lock().contains(&tid));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sweep_leaves_other_processes_alone() {
        let allowed = linux::allowed().unwrap();
        if allowed.len() < 2 {
            return;
        }
        let p = Placement {
            performance: vec![allowed[0]],
            every: allowed.clone(),
            limit: 2,
        };
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = other.id() as i32;
        assert!(linux::pin(pid, &[allowed[0]]));
        assert_eq!(p.sweep([pid]), 0);
        assert_eq!(linux::allowed_of(pid), Some(vec![allowed[0]]));
        let _ = other.kill();
        let _ = other.wait();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn counts_the_physical_cores_of_logical_cpus() {
        let allowed = linux::allowed().unwrap();
        let n = physical_cores(&allowed);
        assert!(n >= 1 && n <= allowed.len());
        assert_eq!(physical_cores(&allowed[..1]), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pinning_a_thread_limits_it_to_those_cpus() {
        let allowed = linux::allowed().unwrap();
        let first = allowed[0];
        let got = std::thread::spawn(move || {
            pin_current_thread(&[first]);
            linux::allowed().unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(got, vec![first]);
        // the calling thread keeps its own CPUs
        assert_eq!(linux::allowed().unwrap(), allowed);
    }
}
