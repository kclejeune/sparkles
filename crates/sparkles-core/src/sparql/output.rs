//! Immutable decoded query output. The final physical owner retains both its
//! payload and its original query credit. Borrowing does not transfer ownership;
//! an independent bounded copy must be admitted by the receiving OutputBudget.
use super::ctx::{Charge, Ctx, OutputCredit};
use super::depth;
use crate::error::{Error, Result};
use crate::id::Id;
use oxrdf::{NamedOrBlankNode, Term, TermRef};
use std::sync::Arc;

/// A receiving query's real accounting owner. This is not a numeric domain token.
/// Cloning it does not copy output or grant additional credit.
#[derive(Clone)]
pub struct OutputBudget {
    ctx: Arc<Ctx>,
}

impl OutputBudget {
    pub(crate) fn new(ctx: Arc<Ctx>) -> Self {
        Self { ctx }
    }

    pub(super) fn decode(&self, id: Id) -> Result<Option<OwnedTerm>> {
        let rows = self.build(1, 1, |_, _, credit| self.ctx.output_term(id, credit))?;
        Ok(rows.inner.terms[0].as_ref().map(|_| OwnedTerm {
            inner: rows.inner.clone(),
            cell: 0,
        }))
    }

    pub(super) fn decode_rows(
        &self,
        rows: usize,
        width: usize,
        mut id: impl FnMut(usize, usize) -> Id,
    ) -> Result<OwnedRows> {
        self.build(rows, width, |r, c, credit| {
            self.ctx.output_term(id(r, c), credit)
        })
    }

    fn build(
        &self,
        rows: usize,
        width: usize,
        mut decode: impl FnMut(usize, usize, &Charge<'_>) -> Result<Option<Term>>,
    ) -> Result<OwnedRows> {
        self.ctx.check_memory_failure()?;
        let count = rows.checked_mul(width).ok_or_else(|| self.overflow())?;
        let bytes = count
            .checked_mul(std::mem::size_of::<Option<Term>>())
            .and_then(|n| n.checked_add(std::mem::size_of::<RowsBacking>()))
            .and_then(|n| n.checked_add(2 * std::mem::size_of::<usize>()))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| self.overflow())?;
        let credit = self.ctx.charge(bytes)?;
        // Declaration order also protects partial construction on a decoding error.
        let mut partial = PartialRows {
            terms: Vec::with_capacity(count),
            nesting: 0,
        };
        // `with_capacity` may provide more than requested. Cover that physical
        // storage before filling it, without discarding the admitted container.
        let extra = partial
            .terms
            .capacity()
            .checked_sub(count)
            .and_then(|n| n.checked_mul(std::mem::size_of::<Option<Term>>()))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| self.overflow())?;
        credit.add(extra)?;
        for r in 0..rows {
            for c in 0..width {
                let term = decode(r, c, &credit)?;
                if let Some(term) = &term {
                    partial.nesting = partial.nesting.max(term_depth(term));
                }
                partial.terms.push(term);
            }
        }
        self.ctx.check_memory_failure()?;
        Ok(OwnedRows {
            inner: Arc::new(RowsBacking {
                terms: std::mem::take(&mut partial.terms),
                rows,
                width,
                nesting: partial.nesting,
                _credit: credit.into_output(&self.ctx),
            }),
        })
    }

    fn copy<'a>(
        &self,
        rows: usize,
        width: usize,
        mut cell: impl FnMut(usize, usize) -> Option<&'a Term>,
    ) -> Result<OwnedRows> {
        self.build(rows, width, |r, c, credit| {
            let Some(term) = cell(r, c) else {
                return Ok(None);
            };
            credit.add(term_bytes(term).ok_or_else(|| self.overflow())?)?;
            Ok(Some(depth::with_stack(term_depth(term), || term.clone())))
        })
    }

    fn overflow(&self) -> Error {
        self.ctx.memory_exceeded(u64::MAX)
    }
}

// This guard also protects nested partial terms when a later cell refuses.
struct PartialRows {
    terms: Vec<Option<Term>>,
    nesting: usize,
}
impl Drop for PartialRows {
    fn drop(&mut self) {
        depth::with_stack(self.nesting, || self.terms.clear());
    }
}

// Payload first, account credit last. No field is exposed by ownership.
struct RowsBacking {
    terms: Vec<Option<Term>>,
    rows: usize,
    width: usize,
    nesting: usize,
    _credit: OutputCredit,
}

impl Drop for RowsBacking {
    fn drop(&mut self) {
        depth::with_stack(self.nesting, || self.terms.clear());
    }
}

/// A term view retaining the complete immutable allocation which owns its bytes.
/// Keeping one cell can retain the rest of its row collection; use try_copy_in
/// when that tradeoff is inappropriate and a destination budget can admit it.
#[derive(Clone)]
pub struct OwnedTerm {
    inner: Arc<RowsBacking>,
    cell: usize,
}

impl OwnedTerm {
    pub fn as_term(&self) -> &Term {
        self.inner.terms[self.cell].as_ref().expect("bound cell")
    }

    pub fn try_copy_in(&self, budget: &OutputBudget) -> Result<Self> {
        let rows = budget.copy(1, 1, |_, _| Some(self.as_term()))?;
        Ok(Self {
            inner: rows.inner,
            cell: 0,
        })
    }
}

impl std::fmt::Debug for OwnedTerm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_term().fmt(f)
    }
}

/// A shallow row handle. Accessors borrow values; they cannot detach its Vec.
#[derive(Clone)]
pub struct OwnedRow {
    inner: Arc<RowsBacking>,
    row: usize,
}

impl OwnedRow {
    pub fn values(&self) -> &[Option<Term>] {
        let start = self.row * self.inner.width;
        &self.inner.terms[start..start + self.inner.width]
    }

    pub fn get(&self, column: usize) -> Result<Option<TermRef<'_>>> {
        self.as_row().get(column)
    }

    pub fn term(&self, column: usize) -> Result<Option<OwnedTerm>> {
        Ok(self.get(column)?.map(|_| OwnedTerm {
            inner: self.inner.clone(),
            cell: self.row * self.inner.width + column,
        }))
    }

    pub fn as_row(&self) -> RowRef<'_> {
        RowRef {
            terms: self.values(),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.width
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn try_copy_in(&self, budget: &OutputBudget) -> Result<Self> {
        let rows = budget.copy(1, self.inner.width, |_, c| self.values()[c].as_ref())?;
        Ok(Self {
            inner: rows.inner,
            row: 0,
        })
    }
}

/// A non-owning row view. Its terms cannot outlive the physical row owner.
#[derive(Clone, Copy)]
pub struct RowRef<'a> {
    terms: &'a [Option<Term>],
}

impl<'a> RowRef<'a> {
    pub fn len(self) -> usize {
        self.terms.len()
    }
    pub fn is_empty(self) -> bool {
        self.terms.is_empty()
    }

    pub fn get(self, column: usize) -> Result<Option<TermRef<'a>>> {
        self.terms
            .get(column)
            .map(|term| term.as_ref().map(Term::as_ref))
            .ok_or_else(|| Error::invalid("query output column is out of bounds"))
    }

    pub fn iter(self) -> impl ExactSizeIterator<Item = Option<TermRef<'a>>> + 'a {
        self.terms
            .iter()
            .map(|term| term.as_ref().map(Term::as_ref))
    }
}

impl std::fmt::Debug for OwnedRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.values().fmt(f)
    }
}

/// Immutable paid rows, including zero-width rows. Clones share physical storage.
#[derive(Clone)]
pub struct OwnedRows {
    inner: Arc<RowsBacking>,
}

impl OwnedRows {
    pub fn len(&self) -> usize {
        self.inner.rows
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn width(&self) -> usize {
        self.inner.width
    }
    pub fn row(&self, row: usize) -> Result<RowRef<'_>> {
        if row >= self.len() {
            return Err(Error::invalid("query output row is out of bounds"));
        }
        let start = row * self.width();
        Ok(RowRef {
            terms: &self.inner.terms[start..start + self.width()],
        })
    }
    pub fn row_handle(&self, row: usize) -> Result<OwnedRow> {
        self.row(row)?;
        Ok(OwnedRow {
            inner: self.inner.clone(),
            row,
        })
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = OwnedRow> + '_ {
        (0..self.len()).map(|row| OwnedRow {
            inner: self.inner.clone(),
            row,
        })
    }
    pub fn try_copy_in(&self, budget: &OutputBudget) -> Result<Self> {
        budget.copy(self.len(), self.width(), |r, c| {
            self.inner.terms[r * self.width() + c].as_ref()
        })
    }
}

impl std::fmt::Debug for OwnedRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

fn term_depth(mut term: &Term) -> usize {
    let mut nesting = 0;
    while let Term::Triple(triple) = term {
        nesting += 1;
        term = &triple.object;
    }
    nesting
}

// Conservative owned string/box payload envelope. The outer Term slots are paid
// by build; no allocation occurs while calculating this admission.
fn term_bytes(mut term: &Term) -> Option<u64> {
    let mut bytes = 0u64;
    loop {
        let text = match term {
            Term::NamedNode(node) => node.as_str().len(),
            Term::BlankNode(node) => node.as_str().len(),
            Term::Literal(literal) => literal
                .value()
                .len()
                .checked_add(literal.datatype().as_str().len())?
                .checked_add(literal.language().map_or(0, str::len))?,
            Term::Triple(triple) => {
                let subject = match &triple.subject {
                    NamedOrBlankNode::NamedNode(node) => node.as_str().len(),
                    NamedOrBlankNode::BlankNode(node) => node.as_str().len(),
                };
                let node = subject
                    .checked_add(triple.predicate.as_str().len())?
                    .checked_add(std::mem::size_of::<oxrdf::Triple>())?;
                bytes = bytes.checked_add(u64::try_from(node).ok()?.checked_add(128)?)?;
                term = &triple.object;
                continue;
            }
        };
        return bytes.checked_add(u64::try_from(text).ok()?.checked_add(128)?);
    }
}

#[cfg(test)]
mod tests;
