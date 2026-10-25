//! Query-owned exact-ID reuse for pure, repeated single-input BIND expressions.
//! Dense inline integers retain their cheap admission. Other inputs need observed
//! repetition in a bounded pilot. Optional cache entries never weaken query budgets.
use crate::error::Result;
use crate::id::{Id, Tag};
use crate::sparql::ctx::RetainedCharge;
use crate::sparql::expr::{Expr, Row, eval};
use crate::sparql::table::VarId;
use crate::sparql::{Ctx, Table};
use rustc_hash::{FxHashMap, FxHashSet};

pub(super) struct Reuse {
    input: VarId,
    values: FxHashMap<Id, Id>,
    charge: Option<RetainedCharge>,
    general_declined: bool,
    max_entries: usize,
    #[cfg(test)]
    evaluations: usize,
}
impl Reuse {
    pub(super) fn new(ctx: &Ctx, expr: &Expr) -> Option<Self> {
        if !ctx.opt.expr_cache {
            return None;
        }
        let input = crate::sparql::exprcache::eligible(&[expr]).ok().flatten()?;
        let charge = ctx.retained_charge(512).ok()?;
        Some(Self {
            input,
            values: Default::default(),
            charge,
            general_declined: false,
            max_entries: 16384,
            #[cfg(test)]
            evaluations: 0,
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
            if self.general_declined || table.len < crate::sparql::exprcache::MIN_ROWS {
                return Ok(false);
            }
            let (mut lo, mut hi) = (u64::MAX, 0);
            let mut integers = true;
            for (row, id) in table.cols[column].iter().enumerate() {
                if row.is_multiple_of(1024) {
                    ctx.check()?;
                }
                if id.tag() != Tag::Int {
                    integers = false;
                    break;
                }
                lo = lo.min(id.payload());
                hi = hi.max(id.payload());
            }
            if integers {
                if hi.saturating_sub(lo) >= (table.len / 2).min(65536) as u64 {
                    return Ok(false);
                }
            } else {
                // A bounded, charged pilot avoids installing an auxiliary map for
                // entirely distinct keys. It affects only optional reuse, never
                // correctness or admission to the query's physical work budgets.
                let sample = table.len.min(4096);
                let Ok(_pilot) = ctx.retained_charge(512 + sample as u64 * 32) else {
                    self.general_declined = true;
                    return Ok(false);
                };
                let mut seen = FxHashSet::with_capacity_and_hasher(sample, Default::default());
                let mut repeated = false;
                for (row, id) in table.cols[column].iter().take(sample).enumerate() {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    if !seen.insert(*id) {
                        repeated = true;
                        break;
                    }
                }
                if !repeated {
                    self.general_declined = true;
                    return Ok(false);
                }
                self.max_entries = 65536;
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
                #[cfg(test)]
                {
                    self.evaluations += 1;
                }
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
                if self.values.len() < self.max_entries
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::Source;
    use crate::sparql::expr::Func;
    use crate::store::{Store, StoreOptions};
    use oxrdf::{Literal, Term, vocab::xsd};
    use oxrdfio::RdfFormat;
    use spargebra::algebra::Function;

    #[test]
    fn stored_temporal_bind_reuses_exact_ids_across_morsels() {
        let store = Store::in_memory(StoreOptions::default());
        let lexicals = (1950..1982)
            .map(|year| format!("{year}-01-01+00:00"))
            .collect::<Vec<_>>();
        let data = lexicals
            .iter()
            .enumerate()
            .map(|(i, date)| {
                format!(
                    "<urn:s{i}> <urn:p> \"{date}\"^^<http://www.w3.org/2001/XMLSchema#date> .\n"
                )
            })
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let mut ctx = Ctx::new(store.snapshot());
        ctx.enable_cursor_accounting().unwrap();
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_cursor_owner();
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let ids = lexicals
            .iter()
            .map(|date| {
                ctx.snap
                    .lookup_term(&Term::Literal(Literal::new_typed_literal(date, xsd::DATE)))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(ids.iter().all(|id| id.tag() == Tag::Vocab));
        let mut reuse = Reuse::new(&ctx, &expr).unwrap();
        for epoch in 0..3 {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..4096)
                .map(|i| ids[(i * 7 + epoch * 3) % ids.len()])
                .collect();
            table.len = 4096;
            assert!(
                reuse.apply(&ctx, &mut table, target, &expr).unwrap(),
                "stored temporal keys must reuse their expression results"
            );
            for row in 0..table.len {
                let index = (row * 7 + epoch * 3) % ids.len();
                assert_eq!(
                    ctx.term(table.cols[1][row]),
                    Some(Term::Literal(Literal::from(1950 + index as i64)))
                );
            }
        }
        assert_eq!(
            reuse.evaluations, 32,
            "each exact date ID is evaluated only once across three morsels"
        );
    }

    fn cursor_context(store: &Store, memory: u64) -> std::sync::Arc<Ctx> {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = memory;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_cursor_owner();
        ctx
    }

    #[test]
    fn generic_reuse_keeps_optional_refusal_identity_and_effect_boundaries() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 10);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let date = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
            "2000-01-01+00:00",
            xsd::DATE,
        )));
        assert_eq!(date.tag(), Tag::Local);
        let mut table = Table::new(vec![input]);
        table.cols[0] = vec![date; 4096];
        table.len = 4096;
        let mut reuse = Reuse::new(&ctx, &expr).unwrap();
        assert!(!reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert!(reuse.values.is_empty());
        assert_eq!(reuse.evaluations, 0);
        ctx.check().unwrap();
        assert!(
            Reuse::new(&ctx, &Expr::Var(input)).is_none(),
            "an identity copy does not need a map"
        );
        let effect = Expr::Arith(
            Box::new(Expr::Var(input)),
            Box::new(Expr::Call(Func::Builtin(Function::Rand), vec![])),
            crate::sparql::expr::ArithOp::Add,
        );
        assert!(Reuse::new(&ctx, &effect).is_none());

        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let mut table = Table::new(vec![input]);
        table.cols[0] = (10000..14096)
            .map(|year| {
                ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
                    format!("{year}-01-01+00:00"),
                    xsd::DATE,
                )))
            })
            .collect();
        table.len = 4096;
        let mut reuse = Reuse::new(&ctx, &expr).unwrap();
        assert!(!reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert!(reuse.values.is_empty());
        // An all-distinct pilot declines the optional map for the query. Later
        // skew still runs normally, without retaining every new term seen.
        let first = table.cols[0][0];
        table.cols[0].fill(first);
        assert!(!reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert!(reuse.values.is_empty());
    }

    #[test]
    fn exact_id_cache_keeps_errors_and_term_identity_and_checks_all_hit_cancellation() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let terms = [
            Literal::new_typed_literal("2000-01-01+00:00", xsd::DATE),
            Literal::new_typed_literal("2000-01-01Z", xsd::DATE),
            Literal::new_typed_literal("bad-date", xsd::DATE),
            Literal::new_simple_literal("2000-01-01"),
        ];
        let keys = terms
            .iter()
            .map(|t| ctx.intern_term(&Term::Literal(t.clone())))
            .chain([Id::UNDEF])
            .collect::<Vec<_>>();
        assert_ne!(
            keys[0], keys[1],
            "equal date values with different lexical forms are distinct terms"
        );
        let mut reuse = Reuse::new(&ctx, &expr).unwrap();
        for epoch in 0..2 {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..4096)
                .map(|row| keys[(row + epoch) % keys.len()])
                .collect();
            table.len = 4096;
            assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
            for row in 0..table.len {
                if (row + epoch) % keys.len() < 2 {
                    assert_eq!(
                        ctx.term(table.cols[1][row]),
                        Some(Term::Literal(Literal::from(2000)))
                    );
                } else {
                    assert_eq!(table.cols[1][row], Id::UNDEF);
                }
            }
        }
        assert_eq!(reuse.evaluations, keys.len());
        let mut hits = Table::new(vec![input]);
        hits.cols[0] = vec![keys[0]; 4096];
        hits.len = 4096;
        ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(
            reuse.apply(&ctx, &mut hits, target, &expr),
            Err(crate::Error::Cancelled)
        ));
    }

    #[test]
    fn cache_growth_refusal_retains_a_bounded_partial_map_and_normal_evaluation() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 64 << 10);
        let input = ctx.var("date");
        let target = ctx.var("year");
        let expr = Expr::Call(Func::Builtin(Function::Year), vec![Expr::Var(input)]);
        let date = ctx.intern_term(&Term::Literal(Literal::new_typed_literal(
            "2000-01-01+00:00",
            xsd::DATE,
        )));
        let mut reuse = Reuse::new(&ctx, &expr).unwrap();
        let mut retained = None;
        for epoch in 0..4 {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..1024)
                .map(|row| {
                    if row < 2 {
                        date
                    } else {
                        Id::from_i64((epoch * 1024 + row) as i64).unwrap()
                    }
                })
                .collect();
            table.len = 1024;
            assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
            assert_eq!(
                ctx.term(table.cols[1][0]),
                Some(Term::Literal(Literal::from(2000)))
            );
            assert!(table.cols[1][2..].iter().all(|id| *id == Id::UNDEF));
            assert!(ctx.live_bytes() <= 64 << 10);
            ctx.check().unwrap();
            if let Some(entries) = retained {
                assert_eq!(
                    reuse.values.len(),
                    entries,
                    "a refused optional growth keeps the existing bounded entries"
                );
            } else {
                assert!(reuse.values.len() < 1023);
                retained = Some(reuse.values.len());
            }
        }
        assert!(
            reuse.evaluations > 4000,
            "uncached keys still evaluate normally after the cache fills"
        );
    }
}
