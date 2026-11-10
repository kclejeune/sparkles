//! Counted text formatting into one immutable, physically credited buffer.
use super::{Error, NativeBudget, OutputBudget, OwnedText, Result, TextBacking};
use std::{fmt, sync::Arc};

impl OutputBudget {
    pub fn format_text<T: fmt::Display + ?Sized>(&self, value: &T) -> Result<OwnedText> {
        self.native_budget()?.format_text(value)
    }
}

impl NativeBudget {
    /// Format borrowed input twice, without an intermediate String. The formatter
    /// must keep its output stable and pay any independent workspace it allocates.
    /// The writing pass cannot grow beyond the admitted count. A changed count,
    /// cancellation or formatter error publishes no partial output.
    pub fn format_text<T: fmt::Display + ?Sized>(&self, value: &T) -> Result<OwnedText> {
        self.check()?;
        let mut counter = Counter {
            budget: self,
            bytes: 0,
            first: None,
        };
        let counted = fmt::write(&mut counter, format_args!("{value}"));
        if let Some(error) = counter.first.take() {
            return Err(error);
        }
        self.check()?;
        counted.map_err(|_| Error::invalid("output text counting failed"))?;
        let bytes = counter.bytes;
        let header = self.arc_bytes::<TextBacking>()?;
        let credit = self.reserve(
            header
                .checked_add(u64::try_from(bytes).map_err(|_| self.overflow())?)
                .ok_or_else(|| self.overflow())?,
        )?;
        #[cfg(test)]
        {
            use crate::algo::memory::probe;
            probe::at(probe::Point::Allocation("owned-output-text", 1));
            if probe::fails("owned-output-text") {
                return Err(Error::invalid("injected output text allocation failure"));
            }
        }
        self.check()?;
        // This owner drops before credit on every error and unwind.
        let mut writer = Bounded {
            text: String::new(),
            bytes,
            budget: self,
            first: None,
        };
        writer
            .text
            .try_reserve_exact(bytes)
            .map_err(|_| Error::invalid("output text allocation failed"))?;
        if writer.text.capacity() != bytes {
            return Err(Error::invalid("output text capacity exceeds admission"));
        }
        let written = fmt::write(&mut writer, format_args!("{value}"));
        if let Some(error) = writer.first.take() {
            return Err(error);
        }
        self.check()?;
        written.map_err(|_| Error::invalid("output text formatting failed"))?;
        if writer.text.len() != bytes {
            return Err(Error::invalid("output text differs from counted size"));
        }
        Ok(OwnedText {
            inner: Arc::new(TextBacking {
                text: writer.text,
                _credit: credit,
            }),
        })
    }
}

struct Counter<'a> {
    budget: &'a NativeBudget,
    bytes: usize,
    first: Option<Error>,
}
impl fmt::Write for Counter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.first.is_some() {
            return Err(fmt::Error);
        }
        let next = self.budget.check().and_then(|()| {
            self.bytes
                .checked_add(text.len())
                .filter(|&n| n <= isize::MAX as usize)
                .ok_or_else(|| self.budget.overflow())
        });
        match next {
            Ok(bytes) => {
                self.bytes = bytes;
                Ok(())
            }
            Err(error) => {
                self.first = Some(error);
                Err(fmt::Error)
            }
        }
    }
}

struct Bounded<'a> {
    text: String,
    bytes: usize,
    budget: &'a NativeBudget,
    first: Option<Error>,
}
impl fmt::Write for Bounded<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.first.is_some() {
            return Err(fmt::Error);
        }
        let next = self.budget.check().and_then(|()| {
            self.text
                .len()
                .checked_add(text.len())
                .filter(|&n| n <= self.bytes)
                .ok_or_else(|| Error::invalid("output text exceeds counted size"))
        });
        match next {
            Ok(_) => {
                // Admission and the exact capacity check happened before allocation;
                // the bound above prevents push_str from reallocating.
                self.text.push_str(text);
                match self.budget.check() {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        self.first = Some(error);
                        Err(fmt::Error)
                    }
                }
            }
            Err(error) => {
                self.first = Some(error);
                Err(fmt::Error)
            }
        }
    }
}

#[cfg(test)]
mod tests;
