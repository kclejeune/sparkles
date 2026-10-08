//! Query-owned ID reuse for pure, repeated integer BIND inputs. Admission is
//! optional: failure to reserve cache entries falls back to ordinary evaluation.
use crate::error::Result;
use crate::id::{Id, Tag};
use crate::sparql::ctx::RetainedCharge;
use crate::sparql::expr::{Expr, Row, eval};
use crate::sparql::table::VarId;
use crate::sparql::{Ctx, Table};
use rustc_hash::FxHashMap;

pub(super) struct Reuse {
    input: VarId,
    values: FxHashMap<Id, Id>,
    charge: Option<RetainedCharge>,
}
impl Reuse {
    pub(super) fn new(ctx: &Ctx, expr: &Expr) -> Option<Self> {
        if !ctx.opt.expr_cache {
            return None;
        }
        let input = crate::sparql::exprcache::input(&[expr]).ok().flatten()?;
        let charge = ctx.retained_charge(512).ok()?;
        Some(Self {
            input,
            values: Default::default(),
            charge,
        })
    }
    pub(super) fn apply(
        &mut self,
        ctx: &Ctx,
        table: &mut Table,
        target: VarId,
        expr: &Expr,
    ) -> Result<bool> {
        let Some(column) = table.col_of(self.input) else {
            return Ok(false);
        };
        if self.values.is_empty() {
            if table.len < crate::sparql::exprcache::MIN_ROWS {
                return Ok(false);
            }
            let (mut lo, mut hi) = (u64::MAX, 0);
            for (row, id) in table.cols[column].iter().enumerate() {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                if id.tag() != Tag::Int {
                    return Ok(false);
                }
                lo = lo.min(id.payload());
                hi = hi.max(id.payload());
            }
            if hi.saturating_sub(lo) >= (table.len / 2).min(65536) as u64 {
                return Ok(false);
            }
        }
        let map = table.var_map(ctx.nvars());
        let mut output = Vec::with_capacity(table.len);
        for row in 0..table.len {
            if row.is_multiple_of(1024) {
                ctx.check()?;
            }
            let key = table.cols[column][row];
            let value = if let Some(value) = self.values.get(&key) {
                *value
            } else {
                let value = eval(
                    expr,
                    &Row {
                        table,
                        i: row,
                        map: &map,
                        dec: None,
                    },
                    ctx,
                )
                .ok()
                .map_or(Id::UNDEF, |value| value.into_id(ctx));
                ctx.check()?;
                if self.values.len() < 16384
                    && self.charge.as_mut().is_some_and(|charge| {
                        charge
                            .resize(512 + (self.values.len() as u64 + 1) * 96)
                            .is_ok()
                    })
                {
                    self.values.insert(key, value);
                }
                value
            };
            output.push(value);
        }
        table.vars.push(target);
        table.cols.push(output);
        Ok(true)
    }
}
