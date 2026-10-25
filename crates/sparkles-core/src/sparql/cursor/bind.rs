//! Query-owned exact-ID reuse for pure, repeated single-input BIND expressions.
//! Dense inline integers retain their cheap admission. Other inputs need observed
//! repetition in a bounded pilot. Optional cache entries never weaken query budgets.
use crate::error::Result;
use crate::id::{Id, Tag};
use crate::index::{O, P, Perm, S};
use crate::sparql::ctx::RetainedCharge;
use crate::sparql::expr::{Expr, Row, eval};
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::VarId;
use crate::sparql::{Ctx, Table};
use rustc_hash::{FxHashMap, FxHashSet};

pub(super) struct Admission {
    input: VarId,
    expected_reuse: bool,
}

pub(super) struct Reuse {
    input: VarId,
    values: FxHashMap<Id, Id>,
    charge: Option<RetainedCharge>,
    general_declined: bool,
    expected_reuse: bool,
    max_entries: usize,
    #[cfg(test)]
    evaluations: usize,
}
impl Reuse {
    pub(super) fn prepare(ctx: &Ctx, expr: &Expr, source: Option<&Node>) -> Option<Admission> {
        if !ctx.opt.expr_cache {
            return None;
        }
        let input = crate::sparql::exprcache::eligible(&[expr]).ok().flatten()?;
        Some(Admission {
            input,
            expected_reuse: source.is_some_and(|source| {
                source.est.is_finite()
                    && source.est > 0.0
                    && distinct_bound(ctx, source, input, &mut 64)
                        .is_some_and(|distinct| distinct as f64 <= source.est * 0.8)
            }),
        })
    }
    pub(super) fn new(ctx: &Ctx, admission: Admission) -> Option<Self> {
        // Compute the hint before building children, but retain optional memory
        // only afterward. Optional caching must not starve child construction.
        let charge = ctx.retained_charge(512).ok()?;
        Some(Self {
            input: admission.input,
            values: Default::default(),
            charge,
            general_declined: false,
            expected_reuse: admission.expected_reuse,
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
                // Cheap, mostly distinct inputs do not repay per-row map work.
                // Require substantial observed reuse, or a repeated pilot key
                // together with snapshot evidence of cross-morsel reuse. The
                // estimate affects profit only, never physical budget admission.
                let sample = table.len.min(4096);
                let Ok(_pilot) = ctx.retained_charge(512 + sample as u64 * 32) else {
                    self.general_declined = true;
                    return Ok(false);
                };
                let mut seen = FxHashSet::with_capacity_and_hasher(sample, Default::default());
                let mut repeats = 0;
                for (row, id) in table.cols[column].iter().take(sample).enumerate() {
                    if row.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    if !seen.insert(*id) {
                        repeats += 1;
                    }
                }
                if repeats == 0 || (repeats * 4 < sample && !self.expected_reuse) {
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

/// A bounded profitability hint, not a resource or uniqueness proof. Unknown
/// shapes and changed snapshots decline the hint; dense observed reuse remains
/// eligible without statistics. Inner joins cannot add new values to a bound key.
fn distinct_bound(ctx: &Ctx, node: &Node, input: VarId, left: &mut usize) -> Option<u64> {
    if *left == 0 || !ctx.snap.delta.is_empty() {
        return None;
    }
    *left -= 1;
    match &node.kind {
        Kind::Scan(scan) if scan.cols.len() <= 4 && scan.prefix.len() <= 4 => {
            let order = scan.perm.order();
            let pcol = order.iter().position(|col| *col == P)?;
            let predicate = *scan.prefix.get(pcol)?;
            ctx.snap.predicate_count(Perm::Pso, &[predicate])?;
            let stats = ctx.snap.generation.stats.predicate(predicate)?;
            let (col, _) = scan.cols.iter().find(|(_, var)| *var == input)?;
            let distinct = match *order.get(*col)? {
                S => stats.distinct_subjects,
                O => stats.distinct_objects,
                _ => return None,
            };
            (distinct > 0 && distinct <= stats.count).then_some(distinct)
        }
        Kind::Join { .. } if node.children.len() == 2 => {
            let a = distinct_bound(ctx, &node.children[0], input, left);
            let b = distinct_bound(ctx, &node.children[1], input, left);
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        Kind::Project(vars)
            if vars.len() <= 64 && vars.contains(&input) && node.children.len() == 1 =>
        {
            distinct_bound(ctx, &node.children[0], input, left)
        }
        Kind::Filter(_)
        | Kind::Sort(_)
        | Kind::OrderBy { .. }
        | Kind::Slice { .. }
        | Kind::Distinct
            if node.children.len() == 1 =>
        {
            distinct_bound(ctx, &node.children[0], input, left)
        }
        Kind::Extend(var, _) | Kind::Assign(var, _)
            if *var != input && node.children.len() == 1 =>
        {
            distinct_bound(ctx, &node.children[0], input, left)
        }
        _ => None,
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

    fn new_reuse(ctx: &Ctx, expr: &Expr, source: Option<&Node>) -> Option<Reuse> {
        Reuse::new(ctx, Reuse::prepare(ctx, expr, source)?)
    }

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
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
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
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(!reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert!(reuse.values.is_empty());
        assert_eq!(reuse.evaluations, 0);
        ctx.check().unwrap();
        assert!(
            new_reuse(&ctx, &Expr::Var(input), None).is_none(),
            "an identity copy does not need a map"
        );
        let effect = Expr::Arith(
            Box::new(Expr::Var(input)),
            Box::new(Expr::Call(Func::Builtin(Function::Rand), vec![])),
            crate::sparql::expr::ArithOp::Add,
        );
        assert!(new_reuse(&ctx, &effect, None).is_none());

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
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
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
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
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
    fn near_unique_generic_inputs_decline_optional_retention() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = cursor_context(&store, 16 << 20);
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let keys = (0..4096)
            .map(|i| {
                ctx.intern_term(&Term::Literal(Literal::new_simple_literal(format!(
                    "value{i}"
                ))))
            })
            .collect::<Vec<_>>();
        for stride in [usize::MAX, 64] {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..4096)
                .map(|i| {
                    if i == 1 || (i > 0 && i % stride == 0) {
                        keys[i - 1]
                    } else {
                        keys[i]
                    }
                })
                .collect();
            table.len = 4096;
            let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
            assert!(
                !reuse.apply(&ctx, &mut table, target, &expr).unwrap(),
                "isolated or infrequent repetition must not retain mostly distinct results"
            );
            assert!(reuse.values.is_empty());
            assert_eq!(reuse.evaluations, 0);
            assert_eq!(table.cols.len(), 1);
            ctx.check().unwrap();
        }
        let mut table = Table::new(vec![input]);
        table.cols[0] = (0..4096).map(|i| keys[i % 3072]).collect();
        table.len = 4096;
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        assert!(reuse.apply(&ctx, &mut table, target, &expr).unwrap());
        assert_eq!(
            reuse.evaluations, 3072,
            "substantial general reuse remains eligible"
        );
        assert_eq!(
            table.cols[0], table.cols[1],
            "STR of strings keeps the exact values"
        );
    }

    #[test]
    fn domain_hint_is_snapshot_local_and_plan_walk_is_bounded() {
        use crate::sparql::plan::{GraphFilter, ScanSpec};
        let store = Store::in_memory(StoreOptions::default());
        let data = (0..4096)
            .map(|i| {
                format!(
                    "<urn:s{i:05}> <urn:p> \"value{}\" .\n",
                    if i == 1 { 0 } else { i % 3000 }
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
        let ctx = cursor_context(&store, 16 << 20);
        assert!(ctx.snap.delta.is_empty());
        let input = ctx.var("value");
        let target = ctx.var("bound");
        let subject = ctx.var("subject");
        let predicate = ctx
            .snap
            .lookup_term(&Term::NamedNode(oxrdf::NamedNode::new("urn:p").unwrap()))
            .unwrap();
        let source = Node::leaf(
            Kind::Scan(ScanSpec {
                perm: Perm::Pso,
                prefix: vec![predicate.0],
                cols: vec![(1, subject), (2, input)],
                eqs: vec![],
                graph: GraphFilter::Default,
                graph_col: 3,
                dedup: false,
                src: None,
            }),
            vec![subject, input],
            4096.0,
            String::new(),
        );
        let expr = Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(input)]);
        let before = ctx.live_bytes();
        let admission = Reuse::prepare(&ctx, &expr, Some(&source)).unwrap();
        assert_eq!(
            ctx.live_bytes(),
            before,
            "metadata preparation must not retain optional memory before child construction"
        );
        let mut reuse = Reuse::new(&ctx, admission).unwrap();
        assert!(reuse.expected_reuse);
        let mut bind = Node::leaf(
            Kind::Extend(target, expr.clone()),
            vec![subject, input, target],
            4096.0,
            String::new(),
        );
        bind.children.push(source.clone());
        let mut operator = super::super::Operator::build(
            &ctx,
            bind,
            super::super::FallbackPolicy::RejectMaterialization,
        )
        .unwrap();
        assert!(
            operator.bind_reuse.as_ref().unwrap().expected_reuse,
            "operator construction must inspect the input before replacing child plans with shells"
        );
        let options = super::super::CursorOptions {
            batch_rows: 1024,
            ..Default::default()
        };
        let mut rows = 0;
        while let Some(batch) = operator.next(&ctx, &options, 1024).unwrap() {
            rows += batch.table.len;
            assert_eq!(batch.table.cols[1], batch.table.cols[2]);
        }
        assert_eq!(rows, 4096);
        assert_eq!(
            operator.bind_reuse.as_ref().unwrap().evaluations,
            3000,
            "a sparse first pilot must still reuse the bounded global domain across morsels"
        );
        let table = || {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..1024)
                .map(|i| {
                    let i = if i == 1 { 0 } else { i };
                    ctx.snap
                        .lookup_term(&Term::Literal(Literal::new_simple_literal(format!(
                            "value{i}"
                        ))))
                        .unwrap()
                })
                .collect();
            table.len = 1024;
            table
        };
        assert!(reuse.apply(&ctx, &mut table(), target, &expr).unwrap());
        let mut deep = source.clone();
        for _ in 0..64 {
            let mut parent =
                Node::leaf(Kind::Sort(vec![input]), vec![input], 4096.0, String::new());
            parent.children.push(deep);
            deep = parent;
        }
        assert!(!new_reuse(&ctx, &expr, Some(&deep)).unwrap().expected_reuse);
        let mut hidden = Node::leaf(Kind::Project(vec![]), vec![], 4096.0, String::new());
        hidden.children.push(source.clone());
        assert!(
            !new_reuse(&ctx, &expr, Some(&hidden))
                .unwrap()
                .expected_reuse
        );
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:new> <urn:p> \"new value\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let changed = cursor_context(&store, 16 << 20);
        assert!(!changed.snap.delta.is_empty());
        assert_eq!(changed.var("value"), input);
        let mut declined = new_reuse(&changed, &expr, Some(&source)).unwrap();
        assert!(!declined.expected_reuse);
        assert!(
            !declined
                .apply(&changed, &mut table(), target, &expr)
                .unwrap()
        );
        assert!(
            new_reuse(&ctx, &expr, Some(&source))
                .unwrap()
                .expected_reuse,
            "the retained old snapshot keeps its own unchanged metadata"
        );
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
        let mut reuse = new_reuse(&ctx, &expr, None).unwrap();
        let mut retained = None;
        for epoch in 0..4 {
            let mut table = Table::new(vec![input]);
            table.cols[0] = (0..1024)
                .map(|row| {
                    if row < 257 {
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
            assert!(table.cols[1][257..].iter().all(|id| *id == Id::UNDEF));
            assert!(ctx.live_bytes() <= 64 << 10);
            ctx.check().unwrap();
            if let Some(entries) = retained {
                assert_eq!(
                    reuse.values.len(),
                    entries,
                    "a refused optional growth keeps the existing bounded entries"
                );
            } else {
                assert!(reuse.values.len() < 768);
                retained = Some(reuse.values.len());
            }
        }
        assert_eq!(
            reuse.evaluations,
            1 + 4 * 767,
            "uncached keys still evaluate normally after the cache fills"
        );
    }
}
