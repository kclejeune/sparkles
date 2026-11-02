//! A hook that the engine's long-lived threads call when they start.
//!
//! A new thread runs on the CPUs of the thread that created it. Some engine threads live
//! as long as the process but start with the first call that needs them, such as the
//! block decoders, the change log writer and the vocabulary sync worker, and a program
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

    fn count() {
        CALLS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn the_first_hook_runs_at_each_thread_start() {
        let before = CALLS.load(Ordering::SeqCst);
        if !super::set_start_hook(count) {
            // another test of this process set a hook first
            return;
        }
        assert!(!super::set_start_hook(|| {}));
        std::thread::spawn(super::started).join().unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), before + 1);
    }
}
