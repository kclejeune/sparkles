//! A hook that the engine's long-lived threads call when they start.
//!
//! A new thread runs on the CPUs of the thread that created it. Some engine threads live
//! as long as the process but start with the first call that needs them, such as the
//! block decoders, the change log writer, the vocabulary sync worker, the text index's
//! checkpoint ticker and recovery workers, the vector catch-up worker and the workers of
//! incremental views, and a program that confines its request threads to some CPUs would
//! otherwise confine these too. The engine also calls the hook on a thread that is about
//! to start threads it does not control, such as Tantivy's workers. The Sparkles server
//! confines request work to the performance cores of a hybrid processor at low load, and
//! its hook moves the calling thread to every CPU.

use std::sync::OnceLock;

static START: OnceLock<fn()> = OnceLock::new();

/// Call `hook` at the start of each long-lived engine thread started from now on. The
/// first hook set stays, and the call answers whether this one was set.
pub fn set_start_hook(hook: fn()) -> bool {
    START.set(hook).is_ok()
}

/// Run the start hook, if one is set, on the calling thread.
pub(crate) fn started() {
    if let Some(hook) = START.get() {
        hook();
    }
}

/// Run the start hook on a thread that is about to start long-lived threads it does not
/// control, such as the workers that a Tantivy writer starts when it opens and after
/// each commit, so that they inherit the CPUs the hook gives.
pub(crate) fn spawning() {
    started();
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    /// The test hook: it counts the starts of the hook test's own thread, since the
    /// engine threads that other tests of the process start call the hook too, and on
    /// Linux it moves the calling thread to CPU 0 and 1, as the server's hook moves a
    /// thread to every CPU.
    fn hook() {
        if std::thread::current().name() == Some("threads-hook-test") {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }
        #[cfg(target_os = "linux")]
        linux::pin(&[0, 1]);
    }

    #[test]
    fn the_first_hook_runs_at_each_thread_start() {
        if !super::set_start_hook(hook) {
            // another test of this process set a hook first
            return;
        }
        assert!(!super::set_start_hook(|| {}));
        for _ in 0..2 {
            std::thread::Builder::new()
                .name("threads-hook-test".into())
                .spawn(super::started)
                .unwrap()
                .join()
                .unwrap();
        }
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        // Tantivy's workers, started from a thread on CPU 0 alone, take the CPUs the hook
        // gives when the engine calls it before it opens a writer
        #[cfg(all(target_os = "linux", feature = "text"))]
        if linux::cpus(0).is_some_and(|c| c.len() >= 2 && c.starts_with(&[0, 1])) {
            let workers = |spawning: bool| {
                std::thread::spawn(move || {
                    linux::pin(&[0]);
                    let before = linux::threads();
                    if spawning {
                        super::spawning();
                    }
                    let mut schema = tantivy::schema::Schema::builder();
                    schema.add_text_field("t", tantivy::schema::TEXT);
                    let index = tantivy::Index::create_in_ram(schema.build());
                    let writer: tantivy::IndexWriter =
                        index.writer_with_num_threads(1, 15_000_000).unwrap();
                    let started: Vec<_> = linux::threads()
                        .into_iter()
                        .filter(|t| !before.contains(t))
                        .filter_map(linux::cpus)
                        .collect();
                    drop(writer);
                    started
                })
                .join()
                .unwrap()
            };
            let inherited = workers(false);
            assert!(!inherited.is_empty());
            assert!(inherited.iter().all(|c| c == &[0]), "{inherited:?}");
            let placed = workers(true);
            assert!(!placed.is_empty());
            assert!(placed.iter().all(|c| c == &[0, 1]), "{placed:?}");
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        /// Run the calling thread only on `cpus`.
        pub fn pin(cpus: &[usize]) {
            // SAFETY: CPU_SET writes within the zeroed set, and sched_setaffinity reads
            // size_of::<cpu_set_t>() bytes of it.
            unsafe {
                let mut set: libc::cpu_set_t = std::mem::zeroed();
                for &c in cpus {
                    libc::CPU_SET(c, &mut set);
                }
                libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
            }
        }

        /// The CPUs the thread `tid` of this process may run on, or the calling thread's
        /// for 0.
        pub fn cpus(tid: i32) -> Option<Vec<usize>> {
            // SAFETY: a zeroed cpu_set_t is an empty set, and sched_getaffinity writes at
            // most size_of::<cpu_set_t>() bytes into it.
            unsafe {
                let mut set: libc::cpu_set_t = std::mem::zeroed();
                if libc::sched_getaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), &mut set)
                    != 0
                {
                    return None;
                }
                Some(
                    (0..libc::CPU_SETSIZE as usize)
                        .filter(|&c| libc::CPU_ISSET(c, &set))
                        .collect(),
                )
            }
        }

        /// The ids of this process's threads.
        pub fn threads() -> Vec<i32> {
            std::fs::read_dir("/proc/self/task")
                .unwrap()
                .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        }
    }
}
