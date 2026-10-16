//! One byte counter per conversion. The paths that can make the output much bigger than
//! the input charge it before they allocate, so a small file cannot ask for far more
//! memory than [`Limits::max_output`](crate::Limits::max_output).

use crate::{Error, Limits};
use std::time::Instant;

/// What one stored cell or table slot costs beyond its text.
pub(crate) const SLOT: usize = 32;

/// The longest identifier kept: a style id or name, a relationship id. A longer one is
/// dropped where it is read, so that a value copied or hashed once per reference to it
/// costs a bounded amount each time.
pub(crate) const MAX_NAME: usize = 256;

/// Calls of `tick` between two readings of the clock. Every call between two readings is
/// a bounded step (an XML event, a node, a stored value), so the deadline is overshot by
/// at most this many steps. Reading the clock costs about 25 ns.
const TICKS_PER_CHECK: u32 = 16;

/// A charge of at least this many bytes reads the clock at once, because copying that
/// much costs as much as many ordinary steps.
const LARGE_CHARGE: usize = 64 << 10;

pub(crate) struct Budget {
    used: usize,
    max: usize,
    deadline: Option<Instant>,
    /// calls of `charge` and `tick`, so that the clock is read once in `TICKS_PER_CHECK`
    calls: u32,
    /// bytes of the Markdown links written so far, and the most the text may hold
    links: usize,
    max_text: usize,
}

impl Budget {
    pub(crate) fn new(limits: &Limits) -> Budget {
        Budget {
            used: 0,
            max: limits.max_output,
            deadline: limits.deadline,
            calls: 0,
            links: 0,
            max_text: limits.max_text,
        }
    }

    /// Fail with [`Error::Timeout`] when the deadline has passed. The readers call it once
    /// per event, node or stored value. The clock is read on the first call and then once
    /// in `TICKS_PER_CHECK`.
    pub(crate) fn tick(&mut self) -> Result<(), Error> {
        let n = self.calls;
        self.calls = n.wrapping_add(1);
        if n.is_multiple_of(TICKS_PER_CHECK) {
            check_deadline(self.deadline)?;
        }
        Ok(())
    }

    /// Account for `n` more bytes, or refuse with [`Error::TooLarge`]. A large charge
    /// reads the clock whatever the count of calls.
    pub(crate) fn charge(&mut self, n: usize) -> Result<(), Error> {
        if n >= LARGE_CHARGE {
            check_deadline(self.deadline)?;
        }
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

    /// Account for a Markdown link of `n` bytes. A link target is copied once per link
    /// that names it, so the links are charged like any output, and because every link
    /// ends up in the text, links longer than [`Limits::max_text`] together are refused
    /// before the paragraph that holds them grows past it.
    pub(crate) fn link(&mut self, n: usize) -> Result<(), Error> {
        self.charge(n)?;
        self.links = self.links.saturating_add(n);
        if self.links > self.max_text {
            return Err(Error::TooLarge(format!(
                "the document's links are longer than {} bytes of text",
                self.max_text
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
