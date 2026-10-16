//! One byte counter per conversion. The paths that can make the output much bigger than
//! the input charge it before they allocate, so a small file cannot ask for far more
//! memory than [`Limits::max_output`](crate::Limits::max_output).

use crate::{Error, Limits};
use std::time::Instant;

/// What one stored cell or table slot costs beyond its text.
pub(crate) const SLOT: usize = 32;

pub(crate) struct Budget {
    used: usize,
    max: usize,
    deadline: Option<Instant>,
    /// calls of `charge` and `tick`, so that the clock is read once in 1,024
    calls: u32,
}

impl Budget {
    pub(crate) fn new(limits: &Limits) -> Budget {
        Budget {
            used: 0,
            max: limits.max_output,
            deadline: limits.deadline,
            calls: 0,
        }
    }

    /// Fail with [`Error::Timeout`] when the deadline has passed. The streaming readers
    /// call it once per event. The clock is read on the first call and then once in 1,024.
    pub(crate) fn tick(&mut self) -> Result<(), Error> {
        let n = self.calls;
        self.calls = n.wrapping_add(1);
        if n.is_multiple_of(1024) {
            check_deadline(self.deadline)?;
        }
        Ok(())
    }

    /// Account for `n` more bytes, or refuse with [`Error::TooLarge`].
    pub(crate) fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.tick()?;
        self.used = self.used.saturating_add(n);
        if self.used > self.max {
            return Err(Error::TooLarge(format!(
                "the converted document needs more than {} bytes",
                self.max
            )));
        }
        Ok(())
    }

    /// Give back `n` bytes that were charged and have been dropped.
    pub(crate) fn release(&mut self, n: usize) {
        self.used = self.used.saturating_sub(n);
    }
}

/// Fail with [`Error::Timeout`] when `deadline` has passed.
pub(crate) fn check_deadline(deadline: Option<Instant>) -> Result<(), Error> {
    match deadline {
        Some(d) if Instant::now() >= d => Err(Error::Timeout),
        _ => Ok(()),
    }
}
