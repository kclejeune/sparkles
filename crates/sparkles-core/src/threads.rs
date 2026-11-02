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
#[cfg(feature = "text")]
pub(crate) fn spawning() {
    started();
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Counts the starts of this test's own thread only, since the engine threads that
    /// other tests of the process start call the hook too.
    fn count() {
        if std::thread::current().name() == Some("threads-hook-test") {
            CALLS.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn the_first_hook_runs_at_each_thread_start() {
        if !super::set_start_hook(count) {
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
    }

    /// Set in the process that [`tantivy_workers_take_the_hooks_cpus`] starts.
    #[cfg(all(target_os = "linux", feature = "text"))]
    const ALONE: &str = "SPARKLES_THREADS_TEST_ALONE";

    /// Tantivy's workers, started from a thread on CPU 0 alone when a writer opens and
    /// after it commits, take the CPUs that the hook gives when the engine runs it first.
    /// The check reads the CPUs of every thread that the writer starts, so it runs in a
    /// process of its own, where no other test starts threads.
    #[cfg(all(target_os = "linux", feature = "text"))]
    #[test]
    fn tantivy_workers_take_the_hooks_cpus() {
        if std::env::var_os(ALONE).is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "threads::tests::tantivy_workers_take_the_hooks_cpus",
                    "--test-threads",
                    "1",
                ])
                .env(ALONE, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.contains(" 1 passed"),
                "{stdout}{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        if !linux::cpus(0).is_some_and(|c| c.starts_with(&[0, 1])) {
            return;
        }
        fn widen() {
            linux::pin(&[0, 1]);
        }
        assert!(super::set_start_hook(widen));
        // the CPUs of the threads that `f` starts from a thread on CPU 0 alone
        let started = |spawning: bool| {
            std::thread::spawn(move || {
                linux::pin(&[0]);
                let mut schema = tantivy::schema::Schema::builder();
                let t = schema.add_text_field("t", tantivy::schema::TEXT);
                let index = tantivy::Index::create_in_ram(schema.build());
                let mut seen = Vec::new();
                let new_threads = |before: &[i32], seen: &mut Vec<Vec<usize>>| {
                    for tid in linux::threads().into_iter().filter(|t| !before.contains(t)) {
                        seen.extend(linux::cpus(tid));
                    }
                };
                let before = linux::threads();
                if spawning {
                    super::spawning();
                }
                let mut writer: tantivy::IndexWriter =
                    index.writer_with_num_threads(1, 15_000_000).unwrap();
                new_threads(&before, &mut seen);
                linux::pin(&[0]);
                writer.add_document(tantivy::doc!(t => "a")).unwrap();
                let before = linux::threads();
                if spawning {
                    super::spawning();
                }
                writer.prepare_commit().unwrap().commit().unwrap();
                new_threads(&before, &mut seen);
                drop(writer);
                seen
            })
            .join()
            .unwrap()
        };
        let inherited = started(false);
        assert!(!inherited.is_empty());
        assert!(inherited.iter().all(|c| c == &[0]), "{inherited:?}");
        let placed = started(true);
        assert!(!placed.is_empty());
        assert!(placed.iter().all(|c| c == &[0, 1]), "{placed:?}");
    }

    #[cfg(all(target_os = "linux", feature = "text"))]
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
