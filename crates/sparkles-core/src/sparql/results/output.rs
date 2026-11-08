//! Call-owned scratch for the checked eager SELECT writers. Public result/cell
//! ownership and large parallel chunks are separate, still-gated migrations.
use super::QueryResult;
use crate::error::Result;
use crate::sparql::ctx::OutputCredit;
use oxrdf::Term;

/// One writer's reused row. Each decode first drops the previous row's terms, so the
/// credit's high-water mark bounds what the writer holds at any time.
pub(super) struct ScratchRow<'a> {
    // Rust destroys fields in declaration order: free all strings and nested terms
    // before their actual query/parent credit can notify a returning owner.
    pub(super) terms: Vec<Option<Term>>,
    credit: OutputCredit<'a>,
    keys: OutputCredit<'a>,
    r: &'a QueryResult,
}

impl<'a> ScratchRow<'a> {
    /// A scratch row for the writer of `r`, admitting the row's term slots.
    pub(super) fn new(r: &'a QueryResult) -> Result<Self> {
        r.ctx.check_memory_failure()?;
        let credit = OutputCredit::new(&r.ctx);
        let width = r.table.width();
        credit.add(Self::slots(r, width)?)?;
        Ok(Self {
            terms: Vec::with_capacity(width),
            credit,
            keys: OutputCredit::new(&r.ctx),
            r,
        })
    }

    fn slots(r: &QueryResult, width: usize) -> Result<u64> {
        width
            .checked_mul(std::mem::size_of::<Option<Term>>())
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| r.ctx.memory_exceeded(u64::MAX))
    }

    /// Decode row `index` into this scratch, replacing the previous row.
    pub(super) fn load(&mut self, index: usize) -> Result<&[Option<Term>]> {
        let r = self.r;
        // A refusal recorded while the last row was written stays the writer's error.
        r.ctx.check_memory_failure()?;
        self.terms.clear();
        self.credit.reset();
        self.credit.add(Self::slots(r, r.table.width())?)?;
        for col in &r.table.cols {
            self.terms
                .push(r.ctx.output_term(col[index], &self.credit, &self.keys)?);
        }
        r.ctx.check_memory_failure()?;
        Ok(&self.terms)
    }

    /// Decode one row into its own scratch.
    #[cfg(test)]
    pub(super) fn decode(r: &'a QueryResult, index: usize) -> Result<Self> {
        let mut row = Self::new(r)?;
        row.load(index)?;
        Ok(row)
    }
}
