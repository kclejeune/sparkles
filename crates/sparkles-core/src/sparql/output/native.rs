//! Byte-only receiving capability over an existing actual accounting owner.
use super::{Error, OutputBudget, OutputReservation, Result};
use crate::access::{Preparation, Reservation};
use std::{alloc::Layout, sync::atomic::AtomicUsize};

#[derive(Clone)]
enum Receiver {
    Query(OutputBudget),
    Prepared(Preparation),
}

/// An existing query or preparation's real byte receiver. This does not create an
/// allowance, source snapshot, graph authority or replacement query context.
#[derive(Clone)]
pub struct NativeBudget {
    receiver: Receiver,
}
impl NativeBudget {
    pub fn from_output(output: &OutputBudget) -> Result<Self> {
        output.check()?;
        Ok(Self {
            receiver: Receiver::Query(output.clone()),
        })
    }

    pub fn from_preparation(preparation: &Preparation) -> Result<Self> {
        preparation.check()?;
        Ok(Self {
            receiver: Receiver::Prepared(preparation.clone()),
        })
    }

    pub fn text(&self, text: &str) -> Result<super::OwnedText> {
        self.format_text(text)
    }

    pub fn check(&self) -> Result<()> {
        match &self.receiver {
            Receiver::Query(output) => output.check(),
            Receiver::Prepared(preparation) => preparation.check(),
        }
    }

    /// Admit the concrete physical buffer before allocation. A zero reservation
    /// keeps the real owner without promoting a query ledger or making credit.
    pub fn reserve(&self, bytes: u64) -> Result<NativeReservation> {
        self.check()?;
        Ok(NativeReservation {
            budget: self.clone(),
            credit: if bytes == 0 {
                None
            } else {
                Some(self.allocate(bytes)?)
            },
        })
    }

    fn allocate(&self, bytes: u64) -> Result<Credit> {
        self.check()?;
        match &self.receiver {
            Receiver::Query(output) => {
                // Preserve the builders' original ordinary query contribution;
                // do not manufacture a separate receiving allowance.
                let credit = output.ctx.charge(bytes)?.into_output(&output.ctx);
                self.check()?;
                Ok(Credit::Query(OutputReservation { credit }))
            }
            Receiver::Prepared(preparation) => Ok(Credit::Prepared(preparation.reserve(bytes)?)),
        }
    }

    pub(super) fn overflow(&self) -> Error {
        match &self.receiver {
            Receiver::Query(output) => output.overflow(),
            Receiver::Prepared(preparation) => preparation.exhausted(),
        }
    }

    pub(super) fn arc_bytes<T>(&self) -> Result<u64> {
        let (layout, _) = Layout::new::<[AtomicUsize; 2]>()
            .extend(Layout::new::<T>())
            .map_err(|_| self.overflow())?;
        u64::try_from(layout.pad_to_align().size()).map_err(|_| self.overflow())
    }
}

enum Credit {
    Query(OutputReservation),
    Prepared(Reservation),
}

/// Original admission for a concrete native allocation. Put the physical payload
/// before this non-Clone final field and grow before allocating new capacity.
pub struct NativeReservation {
    budget: NativeBudget,
    credit: Option<Credit>,
}
impl NativeReservation {
    pub fn bytes(&self) -> u64 {
        match &self.credit {
            Some(Credit::Query(credit)) => credit.bytes(),
            Some(Credit::Prepared(credit)) => credit.bytes(),
            None => 0,
        }
    }

    pub fn grow_by(&mut self, bytes: u64) -> Result<()> {
        self.grow_to(
            self.bytes()
                .checked_add(bytes)
                .ok_or_else(|| self.budget.overflow())?,
        )
    }

    pub fn grow_to(&mut self, bytes: u64) -> Result<()> {
        self.budget.check()?;
        match &mut self.credit {
            Some(Credit::Query(credit)) => credit.grow_to(bytes),
            Some(Credit::Prepared(credit)) => credit.grow_to(bytes),
            None if bytes != 0 => {
                self.credit = Some(self.budget.allocate(bytes)?);
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Release only after the corresponding physical capacity has retired.
    pub fn release_to(&mut self, bytes: u64) -> Result<()> {
        if bytes > self.bytes() {
            return Err(Error::invalid("native release cannot grow admission"));
        }
        match &mut self.credit {
            Some(Credit::Query(credit)) => credit.release_to(bytes),
            Some(Credit::Prepared(credit)) => {
                credit.release_to(bytes);
                Ok(())
            }
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
