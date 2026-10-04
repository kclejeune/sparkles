//! Cancellation and progress of long-running calls.
//!
//! A [`Control`] travels with a call that may run for a long time, such as a compaction,
//! a clone, a reasoning run or a backup. Its [`Cancel`] flag stops the call at its next
//! check, its [`Progress`] receives the fraction done and a short message, and its
//! optional deadline ends the call with [`Error::Timeout`]. The server builds one from
//! each task, and an embedding program or a binding builds one from its own cancel token
//! and callback.
//!
//! The option structs of the store and of the satellite crates keep their `cancel` and
//! `progress` fields, and a `Control` fills them through [`Cancel::flag`] and
//! [`Progress::as_fn`].

use crate::error::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// A progress callback: the fraction done in `[0, 1]` and a short message.
pub type ProgressFn = Arc<dyn Fn(f32, &str) + Send + Sync>;

/// A cancellation flag shared between the caller and the work.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A flag that is not set.
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// A flag that follows `flag`, such as a server task's cancel flag.
    pub fn from_flag(flag: Arc<AtomicBool>) -> Cancel {
        Cancel(flag)
    }

    /// The shared flag, for the `cancel` fields of the option structs.
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }

    /// Ask the work to stop at its next check.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Err(Error::Cancelled)` once the flag is set.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl std::fmt::Debug for Cancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Cancel").field(&self.is_cancelled()).finish()
    }
}

/// Where progress reports go. The default discards them.
#[derive(Clone, Default)]
pub struct Progress(Option<ProgressFn>);

impl Progress {
    /// Reports go to `f`, on the thread doing the work.
    pub fn new(f: impl Fn(f32, &str) + Send + Sync + 'static) -> Progress {
        Progress(Some(Arc::new(f)))
    }

    /// Reports go to `f`, if given.
    pub fn from_fn(f: Option<ProgressFn>) -> Progress {
        Progress(f)
    }

    /// Report the fraction done, clamped to `[0, 1]`, with a short message.
    pub fn report(&self, fraction: f32, message: &str) {
        if let Some(f) = &self.0 {
            f(fraction.clamp(0.0, 1.0), message);
        }
    }

    /// A step of a larger operation: its reports from 0 to 1 become reports from `from`
    /// to `to` of this one.
    pub fn part(&self, from: f32, to: f32) -> Progress {
        match &self.0 {
            None => Progress(None),
            Some(f) => {
                let f = f.clone();
                Progress::new(move |p, m| f(from + p.clamp(0.0, 1.0) * (to - from), m))
            }
        }
    }

    /// The callback, for the `progress` fields of the option structs.
    pub fn as_fn(&self) -> Option<ProgressFn> {
        self.0.clone()
    }

    /// Whether reports go anywhere.
    pub fn is_some(&self) -> bool {
        self.0.is_some()
    }
}

impl std::fmt::Debug for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Progress").field(&self.0.is_some()).finish()
    }
}

/// Cancellation, progress and a deadline for one long-running call.
#[derive(Clone, Default, Debug)]
pub struct Control {
    pub cancel: Cancel,
    pub progress: Progress,
    /// the call fails with [`Error::Timeout`] once this instant has passed
    pub deadline: Option<Instant>,
}

impl Control {
    /// No cancellation, no progress and no deadline.
    pub fn none() -> Control {
        Control::default()
    }

    /// A control that follows `flag` and reports nowhere.
    pub fn with_cancel(flag: Arc<AtomicBool>) -> Control {
        Control {
            cancel: Cancel::from_flag(flag),
            ..Control::default()
        }
    }

    /// `Err(Error::Cancelled)` once cancelled, `Err(Error::Timeout)` past the deadline.
    pub fn check(&self) -> Result<()> {
        self.cancel.check()?;
        match self.deadline {
            Some(d) if Instant::now() >= d => Err(Error::Timeout),
            _ => Ok(()),
        }
    }

    /// The same control for a step of a larger operation (see [`Progress::part`]).
    pub fn part(&self, from: f32, to: f32) -> Control {
        Control {
            cancel: self.cancel.clone(),
            progress: self.progress.part(from, to),
            deadline: self.deadline,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn parts_scale_reports_and_share_the_flag() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let ctl = Control {
            progress: Progress::new(move |p, m| s2.lock().unwrap().push((p, m.to_string()))),
            ..Control::none()
        };
        let part = ctl.part(0.5, 0.9);
        part.progress.report(0.0, "a");
        part.progress.report(0.5, "b");
        part.progress.report(2.0, "c");
        let got = seen.lock().unwrap().clone();
        assert_eq!(got[0], (0.5, "a".to_string()));
        assert!((got[1].0 - 0.7).abs() < 1e-6);
        assert!((got[2].0 - 0.9).abs() < 1e-6);
        assert!(ctl.check().is_ok());
        part.cancel.cancel();
        assert!(matches!(ctl.check(), Err(Error::Cancelled)));
        assert!(ctl.cancel.flag().load(Ordering::Relaxed));
        let late = Control {
            deadline: Some(Instant::now()),
            ..Control::none()
        };
        assert!(matches!(late.check(), Err(Error::Timeout)));
        Progress::default().report(0.5, "nowhere");
    }
}
