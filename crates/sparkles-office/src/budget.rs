//! One byte counter per conversion. Every path that makes the output bigger than the
//! input charges it before it allocates, so a small file cannot ask for far more memory
//! than [`Limits::max_output`](crate::Limits::max_output).

use crate::Error;

/// What one stored cell or table slot costs beyond its text.
pub(crate) const SLOT: usize = 32;

pub(crate) struct Budget {
    used: usize,
    max: usize,
}

impl Budget {
    pub(crate) fn new(max: usize) -> Budget {
        Budget { used: 0, max }
    }

    /// Account for `n` more bytes, or refuse with [`Error::TooLarge`].
    pub(crate) fn charge(&mut self, n: usize) -> Result<(), Error> {
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
