//! Call-owned scratch for the checked eager SELECT writers. Public result/cell
//! ownership and large parallel chunks are separate, still-gated migrations.
use super::QueryResult;
use crate::error::Result;
use crate::sparql::ctx::Charge;
use oxrdf::Term;

pub(super) struct ScratchRow<'a> {
    // Rust destroys fields in declaration order: free all strings and nested terms
    // before their actual query/parent credit can notify a returning owner.
    pub(super) terms: Vec<Option<Term>>,
    _credit: Charge<'a>,
}

impl<'a> ScratchRow<'a> {
    pub(super) fn decode(r: &'a QueryResult, index: usize) -> Result<Self> {
        r.ctx.check_memory_failure()?;
        let width = r.table.width();
        let bytes = width
            .checked_mul(std::mem::size_of::<Option<Term>>())
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| r.ctx.memory_exceeded(u64::MAX))?;
        let credit = r.ctx.charge(bytes)?;
        let mut row = Self {
            terms: Vec::with_capacity(width),
            _credit: credit,
        };
        for col in &r.table.cols {
            row.terms.push(r.ctx.output_term(col[index], &row._credit)?);
        }
        r.ctx.check_memory_failure()?;
        Ok(row)
    }
}
