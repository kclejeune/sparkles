//! A hook that the engine's long-lived threads call when they start.
//!
//! A new thread runs on the CPUs of the thread that created it. Some engine threads live
//! as long as the process but start with the first call that needs them, such as the
//! block decoders, the change log writer, the vocabulary sync worker, the text index's
//! checkpoint ticker and recovery workers, the vector catch-up worker and the workers of
//! incremental views, and a program
//! that confines its request threads to some CPUs would otherwise confine these too. The
//! Sparkles server confines request work to the performance cores of a hybrid processor
//! at low load, and its hook moves each such thread to every CPU when it starts.

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
}
