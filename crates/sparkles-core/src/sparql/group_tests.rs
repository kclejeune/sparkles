//! The accumulators of the grouping kernel: folding the rows of one pass in two parts and
//! merging them gives the answer of one pass over all the rows, DISTINCT passes each
//! value of a group once, and a retracted row leaves the state it had before it was
//! added.

use super::group::acc::{Accumulator, Arg, Batch, Concat, Distinct, Running, retractable};
use super::plan::Agg;
use super::table::{Table, VarId};
use super::value::Value;
use super::*;
use crate::store::{Store, StoreOptions};
use spargebra::algebra::AggregateFunction;

fn agg(func: AggregateFunction, expr: bool, distinct: bool) -> Agg {
    Agg {
        func,
        expr: expr.then(|| super::expr::Expr::Var(0)),
        distinct,
        fold: None,
        registered: None,
    }
}

fn ctx() -> Ctx {
    Ctx::new(Store::in_memory(StoreOptions::default()).snapshot())
}

fn int(ctx: &Ctx, i: i64) -> Id {
    ctx.intern_value(&Value::Integer(i.into()))
}

/// One pass of `acc` over `values`, row `i` going to group `slots[i]`, with `groups` groups.
fn add_batch_ok(
    ctx: &Ctx,
    acc: &mut dyn Accumulator,
    batch: &Batch<'_>,
    slots: &[u32],
) -> crate::error::Result<()> {
    let mut noop = |_| Ok(());
    let mut budget = super::group::op::BatchBudget::Retain(&mut noop);
    acc.add_batch(ctx, batch, slots, &mut budget)
}

fn fold(ctx: &Ctx, acc: &mut dyn Accumulator, values: &[Id], slots: &[u32]) {
    let table = Table::new(vec![0]);
    let batch = Batch {
        table: &table,
        arg: Arg::Column(values),
        rows: None,
        weights: None,
        base: 0,
    };
    add_batch_ok(ctx, acc, &batch, slots).unwrap();
}

fn results(ctx: &Ctx, acc: &mut dyn Accumulator, groups: usize) -> Vec<String> {
    (0..groups)
        .map(|g| {
            let id = acc.finish(ctx, g).unwrap();
            ctx.value(id)
                .and_then(|v| v.lexical().ok().map(|l| l.to_string()))
                .unwrap_or_else(|| "unbound".to_string())
        })
        .collect()
}

#[test]
fn merging_two_passes_gives_one_pass() {
    let ctx = ctx();
    let values: Vec<Id> = [5, 3, 9, 3, 7, 1, 8, 2, 6, 4]
        .iter()
        .map(|&i| int(&ctx, i))
        .collect();
    let slots: Vec<u32> = vec![0, 1, 0, 1, 2, 2, 0, 1, 2, 0];
    for func in [
        AggregateFunction::Count,
        AggregateFunction::Sum,
        AggregateFunction::Avg,
        AggregateFunction::Min,
        AggregateFunction::Max,
        AggregateFunction::Sample,
    ] {
        let agg = agg(func, true, false);
        let mut whole = Running::new(&agg, true);
        whole.init(3);
        fold(&ctx, &mut whole, &values, &slots);
        for split in [0, 1, 4, 10] {
            let (mut left, mut right) = (Running::new(&agg, true), Running::new(&agg, true));
            left.init(3);
            right.init(3);
            fold(&ctx, &mut left, &values[..split], &slots[..split]);
            fold(&ctx, &mut right, &values[split..], &slots[split..]);
            assert!(left.mergeable());
            left.merge(&ctx, &mut right, &[0, 1, 2], &mut |_| Ok(()))
                .unwrap();
            let mut again = Running::new(&agg, true);
            again.init(3);
            fold(&ctx, &mut again, &values, &slots);
            assert_eq!(
                results(&ctx, &mut left, 3),
                results(&ctx, &mut again, 3),
                "{:?} split at {split}",
                agg.func
            );
        }
        let _ = results(&ctx, &mut whole, 3);
    }
}

#[test]
fn a_merge_maps_the_groups_of_the_second_pass_onto_the_first() {
    let ctx = ctx();
    let agg = agg(AggregateFunction::Sum, true, false);
    let (mut a, mut b) = (Running::new(&agg, true), Running::new(&agg, true));
    a.init(2);
    b.init(2);
    let one = int(&ctx, 1);
    fold(&ctx, &mut a, &[one, one], &[0, 1]);
    fold(&ctx, &mut b, &[one, one, one], &[0, 0, 1]);
    // b's group 0 is a's group 1, and b's group 1 is a's new group 2
    a.init(1);
    a.merge(&ctx, &mut b, &[1, 2], &mut |_| Ok(())).unwrap();
    assert_eq!(results(&ctx, &mut a, 3), vec!["1", "3", "1"]);
}

#[test]
fn statistics_aggregates_do_not_merge() {
    let stdev = Running::new(
        &agg(
            AggregateFunction::Custom(
                oxrdf::NamedNode::new(format!(
                    "{}stdev",
                    spargebra::algebra::ARQ_AGGREGATE_NAMESPACE
                ))
                .unwrap(),
            ),
            true,
            false,
        ),
        true,
    );
    assert!(!stdev.mergeable());
}

#[test]
fn a_sum_that_became_generic_merges_in_row_order() {
    let ctx = ctx();
    let agg = agg(AggregateFunction::Sum, true, false);
    let half = ctx.intern_value(&Value::Double(0.5f64.into()));
    let (mut a, mut b) = (Running::new(&agg, true), Running::new(&agg, true));
    a.init(1);
    b.init(1);
    fold(&ctx, &mut a, &[int(&ctx, 2), half], &[0, 0]);
    fold(&ctx, &mut b, &[int(&ctx, 3)], &[0]);
    a.merge(&ctx, &mut b, &[0], &mut |_| Ok(())).unwrap();
    let id = a.finish(&ctx, 0).unwrap();
    assert_eq!(&*ctx.value(id).unwrap().lexical().unwrap(), "5.5");
}

#[test]
fn group_concat_merges_in_row_order() {
    let ctx = ctx();
    let agg = agg(
        AggregateFunction::GroupConcat {
            separator: Some("-".into()),
        },
        true,
        false,
    );
    let ids: Vec<Id> = (1..=5).map(|i| int(&ctx, i)).collect();
    let slots = vec![0, 0, 1, 0, 1];
    let mut whole = Concat::new(&agg);
    whole.init(2);
    fold(&ctx, &mut whole, &ids, &slots);
    let (mut a, mut b) = (Concat::new(&agg), Concat::new(&agg));
    a.init(2);
    b.init(2);
    fold(&ctx, &mut a, &ids[..3], &slots[..3]);
    fold(&ctx, &mut b, &ids[3..], &slots[3..]);
    a.merge(&ctx, &mut b, &[0, 1], &mut |_| Ok(())).unwrap();
    assert_eq!(results(&ctx, &mut a, 2), results(&ctx, &mut whole, 2));
    assert_eq!(
        results(&ctx, &mut whole, 2)[0],
        "unbound",
        "a finished group is taken out"
    );
}

#[test]
fn distinct_passes_each_value_of_a_group_once() {
    let ctx = ctx();
    let values: Vec<Id> = [1, 1, 2, 1, 2, 2].iter().map(|&i| int(&ctx, i)).collect();
    let slots = [0, 0, 0, 1, 1, 1];
    let a = agg(AggregateFunction::Count, true, true);
    let mut count = Distinct::new(Box::new(Running::new(&a, true)), false);
    count.init(2);
    // two calls, so that the set carries over batches
    fold(&ctx, &mut count, &values[..4], &slots[..4]);
    fold(&ctx, &mut count, &values[4..], &slots[4..]);
    assert_eq!(results(&ctx, &mut count, 2), vec!["2", "2"]);
    assert!(count.take_growth() > 0);
    // an error is never a duplicate of another
    let mut count = Distinct::new(Box::new(Running::new(&a, true)), false);
    count.init(1);
    fold(&ctx, &mut count, &[Id::UNDEF, Id::UNDEF], &[0, 0]);
    assert_eq!(results(&ctx, &mut count, 1), vec!["0"]);
}

#[test]
fn count_distinct_star_tells_rows_apart() {
    let ctx = ctx();
    let a = agg(AggregateFunction::Count, false, true);
    let mut table = Table::new(vec![0, 1]);
    for row in [[1, 1], [1, 1], [1, 2], [2, 2]] {
        table.push_row(&[Id(row[0]), Id(row[1])]);
    }
    let mut count = Distinct::new(Box::new(Running::new(&a, true)), true);
    count.init(1);
    let batch = Batch {
        table: &table,
        arg: Arg::Star,
        rows: None,
        weights: None,
        base: 0,
    };
    let mut noop = |_| Ok(());
    let mut budget = super::group::op::BatchBudget::Retain(&mut noop);
    count
        .add_batch(&ctx, &batch, &[0, 0, 0, 0], &mut budget)
        .unwrap();
    assert_eq!(results(&ctx, &mut count, 1), vec!["3"]);
}

#[test]
fn retract_undoes_an_add_for_counts_and_integer_sums() {
    let ctx = ctx();
    for func in [
        AggregateFunction::Count,
        AggregateFunction::Sum,
        AggregateFunction::Avg,
    ] {
        let agg = agg(func, true, false);
        let values: Vec<Id> = [4, 6, 10].iter().map(|&i| int(&ctx, i)).collect();
        let mut acc = Running::new(&agg, true);
        acc.init(1);
        assert!(acc.retractable());
        fold(&ctx, &mut acc, &values, &[0, 0, 0]);
        let table = Table::new(vec![0]);
        let batch = Batch {
            table: &table,
            arg: Arg::Column(&values[1..]),
            rows: None,
            weights: None,
            base: 0,
        };
        assert!(acc.retract(&ctx, &batch, &[0, 0]).unwrap());
        let mut only = Running::new(&agg, true);
        only.init(1);
        fold(&ctx, &mut only, &values[..1], &[0]);
        assert_eq!(
            results(&ctx, &mut acc, 1),
            results(&ctx, &mut only, 1),
            "{:?}",
            agg.func
        );
    }
    // a minimum cannot take a row back, and a sum of doubles cannot either
    let min = Running::new(&agg(AggregateFunction::Min, true, false), true);
    assert!(!min.retractable());
    let sum = agg(AggregateFunction::Sum, true, false);
    let mut acc = Running::new(&sum, true);
    acc.init(1);
    let half = ctx.intern_value(&Value::Double(0.5f64.into()));
    fold(&ctx, &mut acc, &[half], &[0]);
    let table = Table::new(vec![0]);
    let batch = Batch {
        table: &table,
        arg: Arg::Column(&[half]),
        rows: None,
        weights: None,
        base: 0,
    };
    assert!(!acc.retract(&ctx, &batch, &[0]).unwrap());
}

/// The slots and values a DISTINCT passes on are charged while the aggregate underneath
/// reads them, also when every row repeats one value and the set stays small.
#[test]
fn distinct_charges_what_it_passes_on() {
    let ctx = ctx();
    let rows = 65_536;
    let values = vec![int(&ctx, 7); rows];
    let slots = vec![0u32; rows];
    let agg = agg(AggregateFunction::Count, true, true);
    let mut acc = Distinct::new(Box::new(Running::new(&agg, true)), false);
    acc.init(1);
    let before = ctx.mem_peak();
    fold(&ctx, &mut acc, &values, &slots);
    assert!(
        ctx.mem_peak() >= before + rows as u64 * 12,
        "peak {} after {before}",
        ctx.mem_peak()
    );
    assert_eq!(results(&ctx, &mut acc, 1), ["1"]);
}

/// A tracked SUM/AVG running accumulator reports heap growth for big-number subtotals
/// both during `add_batch` and across `merge`.
#[test]
fn running_sum_tracks_big_number_subtotal_growth() {
    let ctx = ctx();
    let huge = "9".repeat(4_000);
    let big_id = ctx.intern_value(&Value::from_typed(
        &huge,
        oxrdf::vocab::xsd::INTEGER.as_str(),
    ));
    let sum_agg = agg(AggregateFunction::Sum, true, false);
    let mut acc = Running::new(&sum_agg, true);
    acc.init(2);
    fold(&ctx, &mut acc, &[big_id, big_id], &[0, 1]);
    let growth = acc.take_growth();
    assert!(
        growth >= 3_000,
        "expected big-number SUM growth for two 4000-digit groups, got {growth}"
    );
    assert_eq!(acc.take_growth(), 0);

    // Merging a big-number subtotal into an empty group also charges its heap growth.
    let mut left = Running::new(&sum_agg, true);
    left.init(1);
    let mut right = Running::new(&sum_agg, true);
    right.init(1);
    fold(&ctx, &mut right, &[big_id], &[0]);
    left.merge(&ctx, &mut right, &[0], &mut |_| Ok(())).unwrap();
    assert!(
        left.take_growth() >= 1_500,
        "expected merge growth for big-number SUM"
    );
}

#[test]
fn running_decimal_avg_uses_the_final_subtotal_precision() {
    let ctx = ctx();
    let decimal = |s| ctx.intern_value(&Value::from_typed(s, oxrdf::vocab::xsd::DECIMAL.as_str()));
    let a = decimal("100000000000000000000.0");
    let neg = decimal("-100000000000000000000.0");
    let zero = decimal("0.0");
    let tiny = decimal("0.000000000000000001");
    let avg = agg(AggregateFunction::Avg, true, false);
    for inputs in [[a, a, neg, neg, zero, tiny], [a, neg, a, neg, zero, tiny]] {
        let mut acc = Running::new(&avg, false);
        acc.init(1);
        fold(&ctx, &mut acc, &inputs, &[0; 6]);
        assert_eq!(results(&ctx, &mut acc, 1), ["0"]);
    }
}

#[test]
fn spill_group_replays_widening_subtotals_from_later_batches() {
    use super::group::op::Grouping;
    use crate::spill::{Spill, SpillOptions};

    let dir = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: dir.path().to_path_buf(),
        max_bytes: 4 << 30,
        query_bytes: 4 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();
    let mut ctx = ctx();
    ctx.mem_limit = 1 << 20;
    ctx.spill = Some(std::sync::Arc::new(spill.query(None)));
    let key = ctx.var("key");
    let value = ctx.var("value");
    let sum = ctx.var("sum");
    let ctx = std::sync::Arc::new(ctx);
    ctx.attach_owner();

    let mut table = Table::new(vec![key, value]);
    for group in 0..1024 {
        table.push_row(&[int(&ctx, group), int(&ctx, 0)]);
    }
    let mut agg = agg(AggregateFunction::Sum, true, false);
    agg.expr = Some(super::expr::Expr::Var(value));
    let mut oracle = Running::new(&agg, false);
    oracle.init(1);
    let mut grouping = Grouping::new(&ctx, &[key], &[(sum, agg)], &table.vars, true, true).unwrap();
    let mut report = super::exprcache::Report::default();
    grouping.start_pass(&ctx);
    grouping.add(&ctx, &table, &mut report).unwrap();

    assert_eq!(
        spill.written_bytes(),
        0,
        "inline subtotals fit without spill"
    );

    let big = ctx.intern_value(&Value::from_typed(
        &"9".repeat(4096),
        oxrdf::vocab::xsd::INTEGER.as_str(),
    ));
    fold(&ctx, &mut oracle, &[int(&ctx, 0), big], &[0, 0]);
    let expected = oracle.finish(&ctx, 0).unwrap();
    assert_eq!(expected, big);
    table.cols[1].fill(big);
    grouping.add(&ctx, &table, &mut report).unwrap();
    grouping.end_input().unwrap();
    assert!(grouping.spilled > 0);
    let mut found = Vec::new();
    while let Some(output) = grouping.output(&ctx, &[key, sum], 97).unwrap() {
        assert!(output.cols[1].iter().all(|&id| id == expected));
        found.extend(output.cols[0].iter().map(|id| id.as_i64()));
    }
    assert!(grouping.next_pass(&ctx).unwrap().is_none());
    assert_eq!(found, (0..1024).collect::<Vec<_>>());
    assert_eq!(spill.reserved_bytes(), 0);
}

#[test]
fn spill_group_admits_many_fixed_width_aggregates() {
    use super::group::op::Grouping;
    use crate::spill::{Spill, SpillOptions};

    let dir = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: dir.path().to_path_buf(),
        max_bytes: 1 << 30,
        query_bytes: 1 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();
    let mut ctx = ctx();
    ctx.mem_limit = 1 << 20;
    ctx.spill = Some(std::sync::Arc::new(spill.query(None)));
    let key = ctx.var("key");
    let value = ctx.var("value");
    let aggs: Vec<_> = (0..300)
        .map(|i| {
            let func = if i % 2 == 0 {
                AggregateFunction::Sum
            } else {
                AggregateFunction::Avg
            };
            let mut agg = agg(func, true, false);
            agg.expr = Some(super::expr::Expr::Var(value));
            (ctx.var(&format!("out{i}")), agg)
        })
        .collect();
    let ctx = std::sync::Arc::new(ctx);
    ctx.attach_owner();

    let mut table = Table::new(vec![key, value]);
    table.push_row(&[int(&ctx, 1), int(&ctx, 2)]);
    let four = ctx.intern_value(&Value::Decimal(oxsdatatypes::Decimal::from(4)));
    table.push_row(&[int(&ctx, 1), four]);
    let mut grouping = Grouping::new(&ctx, &[key], &aggs, &table.vars, true, true).unwrap();
    grouping.start_pass(&ctx);
    grouping.add(&ctx, &table, &mut Default::default()).unwrap();
    grouping.end_input().unwrap();
    let vars: Vec<_> = std::iter::once(key)
        .chain(aggs.iter().map(|&(var, _)| var))
        .collect();
    let output = grouping.output(&ctx, &vars, 1).unwrap().unwrap();
    assert_eq!(output.len(), 1);
    for (i, column) in output.cols[1..].iter().enumerate() {
        let want = if i % 2 == 0 {
            Value::Decimal(oxsdatatypes::Decimal::from(6))
        } else {
            Value::Decimal(oxsdatatypes::Decimal::from(3))
        };
        assert_eq!(column[0], ctx.intern_value(&want));
    }
    assert_eq!(spill.written_bytes(), 0);
}

#[test]
fn spill_group_distinct_avg_replays_arguments_once_in_order() {
    use super::group::op::Grouping;
    use crate::spill::{Spill, SpillOptions};

    let dir = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: dir.path().to_path_buf(),
        max_bytes: 1 << 30,
        query_bytes: 1 << 30,
        min_free_bytes: 0,
        encrypted: true,
    })
    .unwrap();
    let mut ctx = ctx();
    ctx.mem_limit = 1 << 20;
    ctx.spill = Some(std::sync::Arc::new(spill.query(None)));
    let key = ctx.var("key");
    let value = ctx.var("value");
    let avg = ctx.var("avg");
    let sum = ctx.var("sum");
    let ctx = std::sync::Arc::new(ctx);
    ctx.attach_owner();

    let mut table = Table::new(vec![key, value]);
    for group in 0..512 {
        table.push_row(&[int(&ctx, group), int(&ctx, 6)]);
    }
    let aggs = [(avg, AggregateFunction::Avg), (sum, AggregateFunction::Sum)].map(|(var, func)| {
        let mut agg = agg(func, true, true);
        agg.expr = Some(super::expr::Expr::Var(value));
        (var, agg)
    });
    let mut grouping = Grouping::new(&ctx, &[key], &aggs, &table.vars, true, true).unwrap();
    let mut report = super::exprcache::Report::default();
    grouping.start_pass(&ctx);
    grouping.add(&ctx, &table, &mut report).unwrap();

    let digits = "9".repeat(4096);
    let datatype = oxrdf::vocab::xsd::INTEGER.as_str();
    let positive = ctx.intern_value(&Value::from_typed(&digits, datatype));
    let negative = ctx.intern_value(&Value::from_typed(&format!("-{digits}"), datatype));
    let mut single = Table::new(table.vars.clone());
    single.push_row(&[int(&ctx, 0), positive]);
    assert_eq!(spill.written_bytes(), 0);
    // The first write opens the deferred argument stream. Since only one existing
    // group received an argument in that call, the positive value for it was deferred.
    let replayed = (0..511)
        .find(|&group| {
            single.cols[0][0] = int(&ctx, group);
            grouping.add(&ctx, &single, &mut report).unwrap();
            spill.written_bytes() > 0
        })
        .expect("a wide positive argument was deferred");
    table.cols[1].fill(positive);
    grouping.add(&ctx, &table, &mut report).unwrap();
    // A repeated wide value crossed the spill boundary for some groups. DISTINCT
    // still filters it, then the remaining negative argument cancels the first.
    table.cols[0].reverse();
    grouping.add(&ctx, &table, &mut report).unwrap();
    let mut error = Table::new(table.vars.clone());
    error.push_row(&[int(&ctx, 511), Id::UNDEF]);
    grouping.add(&ctx, &error, &mut report).unwrap();
    table.cols[1].fill(negative);
    grouping.add(&ctx, &table, &mut report).unwrap();
    // Promoting to float before cancellation would convert a wide operand to
    // infinity. The correct replay cancels the integers before seeing this zero.
    single.cols[1][0] = ctx.intern_value(&Value::Float(0.0f32.into()));
    grouping.add(&ctx, &single, &mut report).unwrap();
    grouping.end_input().unwrap();
    assert!(grouping.spilled > 0);
    let expected_avg = ctx.intern_value(&Value::Decimal(oxsdatatypes::Decimal::from(2)));
    let expected_sum = int(&ctx, 6);
    let float_avg = ctx.intern_value(&Value::Float(1.5f32.into()));
    let float_sum = ctx.intern_value(&Value::Float(6.0f32.into()));
    let mut groups = 0;
    while let Some(output) = grouping.output(&ctx, &[key, avg, sum], 83).unwrap() {
        for row in 0..output.len() {
            let slot = output.cols[0][row].as_i64();
            assert_eq!(slot, groups);
            let expected = if slot == 511 {
                (Id::UNDEF, Id::UNDEF)
            } else if slot == replayed {
                (float_avg, float_sum)
            } else {
                (expected_avg, expected_sum)
            };
            assert_eq!(
                (output.cols[1][row], output.cols[2][row]),
                expected,
                "group {slot}"
            );
            groups += 1;
        }
    }
    assert_eq!(groups, 512);
    assert!(grouping.next_pass(&ctx).unwrap().is_none());
    assert_eq!(spill.reserved_bytes(), 0);
}

#[test]
fn spill_group_sum_and_avg_use_speculative_group_headroom() {
    use super::bignum::AGG_SUM_SCRATCH_BYTES;
    use super::group::op::Grouping;
    use crate::spill::{Spill, SpillOptions};

    let dir = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: dir.path().to_path_buf(),
        max_bytes: 1 << 30,
        query_bytes: 1 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();

    for (func, expected_str) in [
        (
            AggregateFunction::Sum,
            "20000000000000000000000000000000000000004",
        ),
        (
            AggregateFunction::Avg,
            "10000000000000000000000000000000000000002",
        ),
    ] {
        // Measure base + slots + first group's exact retained size first, then set a
        // budget with 72 KiB of headroom so GroupMemory's 64 KiB speculative step succeeds
        // and leaves only ~8 KiB (< AGG_SUM_SCRATCH_BYTES = 18,660 B) on the query ledger.
        let mut ctx = ctx();
        ctx.spill = Some(std::sync::Arc::new(spill.query(None)));
        let key = ctx.var("key");
        let value = ctx.var("value");
        let out = ctx.var("out");
        let a = ctx.intern_value(&Value::from_typed(
            "10000000000000000000000000000000000000001",
            oxrdf::vocab::xsd::INTEGER.as_str(),
        ));
        let b = ctx.intern_value(&Value::from_typed(
            "10000000000000000000000000000000000000003",
            oxrdf::vocab::xsd::INTEGER.as_str(),
        ));
        let _ = ctx.value_checked(a).unwrap();
        let _ = ctx.value_checked(b).unwrap();

        let mut table = Table::new(vec![key, value]);
        table.push_row(&[int(&ctx, 1), a]);
        table.push_row(&[int(&ctx, 1), b]);

        let mut agg = agg(func.clone(), true, false);
        agg.expr = Some(super::expr::Expr::Var(value));

        let slots_bytes = table.len as u64 * 4 + 64;
        let base_bytes = 256 + 64 + 1024;
        let per_group_approx =
            13 * 2 + 8 + (size_of::<super::exec::AggState>() as u64 * 2 + 8) + 16;
        let setup_live = ctx.live_bytes();
        // 72 KiB after base + slots + first group: 64 KiB goes to GroupMemory's
        // speculative step, leaving ~8 KiB < AGG_SUM_SCRATCH_BYTES on the query ledger.
        let headroom: u64 = 72 << 10;
        assert!(headroom - (64 << 10) < AGG_SUM_SCRATCH_BYTES);
        ctx.mem_limit = setup_live + base_bytes + slots_bytes + per_group_approx + headroom;
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();

        let mut grouping =
            Grouping::new(&ctx, &[key], &[(out, agg)], &table.vars, true, true).unwrap();
        grouping.start_pass(&ctx);
        grouping.add(&ctx, &table, &mut Default::default()).unwrap();
        grouping.end_input().unwrap();
        let result = grouping.output(&ctx, &[key, out], 1).unwrap().unwrap();
        assert_eq!(result.len(), 1);
        let got = ctx
            .value(result.cols[1][0])
            .unwrap()
            .lexical()
            .unwrap()
            .to_string();
        assert_eq!(got, expected_str, "{func:?}");
    }
}

#[test]
fn big_avg_finalization_charges_division_scratch_deferred_and_direct() {
    use super::bignum::{AGG_SUM_PAYLOAD_BYTES, AGG_SUM_SCRATCH_BYTES};
    use super::group::op::Grouping;
    use crate::spill::{Spill, SpillOptions};

    let big_str = format!("1{}1", "0".repeat(3_998));

    // Direct (non-deferred) AVG finalization of a 4,000-digit integer over 3 rows takes
    // the non-terminating BigDec::checked_div path and must charge division scratch.
    let mut ctx_direct = ctx();
    ctx_direct.mem_limit = 512 << 10;
    let big = ctx_direct.intern_value(&Value::from_typed(
        &big_str,
        oxrdf::vocab::xsd::INTEGER.as_str(),
    ));
    let avg_agg = agg(AggregateFunction::Avg, true, false);
    let mut acc = Running::new(&avg_agg, false);
    acc.init(1);
    fold(&ctx_direct, &mut acc, &[big, big, big], &[0, 0, 0]);
    let pin = ctx_direct
        .charge(ctx_direct.memory_room().saturating_sub(1024))
        .unwrap();
    let err = acc.finish(&ctx_direct, 0).unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
        ),
        "expected memory refusal during non-deferred big AVG finish, got {err:?}"
    );
    drop(pin);

    // With room for division scratch, the same non-deferred AVG produces the exact quotient.
    let mut acc_ok = Running::new(&avg_agg, false);
    acc_ok.init(1);
    fold(&ctx_direct, &mut acc_ok, &[big, big, big], &[0, 0, 0]);
    let expected_id = acc_ok.finish(&ctx_direct, 0).unwrap();
    let expected_lex = ctx_direct
        .value(expected_id)
        .unwrap()
        .lexical()
        .unwrap()
        .to_string();
    assert_eq!(expected_lex, big_str);

    // Non-terminating division: 10^3999 + 1 + 0 + 0 = 10^3999 + 1 over 3 rows.
    let zero = int(&ctx_direct, 0);
    let mut acc_nt = Running::new(&avg_agg, false);
    acc_nt.init(1);
    fold(&ctx_direct, &mut acc_nt, &[big, zero, zero], &[0, 0, 0]);
    let pin_nt = ctx_direct
        .charge(ctx_direct.memory_room().saturating_sub(1024))
        .unwrap();
    assert!(acc_nt.finish(&ctx_direct, 0).is_err());
    drop(pin_nt);

    let mut acc_nt_ok = Running::new(&avg_agg, false);
    acc_nt_ok.init(1);
    fold(&ctx_direct, &mut acc_nt_ok, &[big, zero, zero], &[0, 0, 0]);
    let nt_id = acc_nt_ok.finish(&ctx_direct, 0).unwrap();
    let nt_lex = ctx_direct
        .value(nt_id)
        .unwrap()
        .lexical()
        .unwrap()
        .to_string();
    assert!(nt_lex.starts_with("3333333333"), "got {nt_lex}");

    // Deferred AVG finalization must hold both the expanded subtotal charge and division
    // scratch simultaneously, and replay must reach division rather than failing earlier.
    let make_deferred_avg = || {
        let dir = tempfile::tempdir().unwrap();
        let spill = Spill::open(&SpillOptions {
            dir: dir.path().to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let mut ctx_def = ctx();
        ctx_def.mem_limit = 512 << 10;
        ctx_def.spill = Some(std::sync::Arc::new(spill.query(None)));
        let key = ctx_def.var("key");
        let value = ctx_def.var("value");
        let avg_var = ctx_def.var("avg");
        let ctx_def = std::sync::Arc::new(ctx_def);
        ctx_def.attach_owner();

        let big_def = ctx_def.intern_value(&Value::from_typed(
            &big_str,
            oxrdf::vocab::xsd::INTEGER.as_str(),
        ));
        let zero_def = int(&ctx_def, 0);
        let mut table = Table::new(vec![key, value]);
        for group in 0..512 {
            table.push_row(&[int(&ctx_def, group), zero_def]);
        }
        let mut g_agg = agg(AggregateFunction::Avg, true, false);
        g_agg.expr = Some(super::expr::Expr::Var(value));
        let mut grouping = Grouping::new(
            &ctx_def,
            &[key],
            &[(avg_var, g_agg)],
            &table.vars,
            true,
            true,
        )
        .unwrap();
        let mut report = super::exprcache::Report::default();
        grouping.start_pass(&ctx_def);
        grouping.add(&ctx_def, &table, &mut report).unwrap();

        let mut single = Table::new(table.vars.clone());
        single.push_row(&[int(&ctx_def, 0), big_def]);
        assert_eq!(spill.written_bytes(), 0);
        let deferred_group = (0..512)
            .find(|&group| {
                single.cols[0][0] = int(&ctx_def, group);
                grouping.add(&ctx_def, &single, &mut report).unwrap();
                spill.written_bytes() > 0
            })
            .expect("expected a deferred AVG subtotal");
        single.cols[0][0] = int(&ctx_def, deferred_group);
        single.cols[1][0] = zero_def;
        grouping.add(&ctx_def, &single, &mut report).unwrap();
        grouping.end_input().unwrap();
        assert!(grouping.spilled > 0, "expected deferred AVG subtotals");

        // Drain the non-deferred prefix so the next emitted group is `deferred_group`.
        let prefix = grouping
            .output(&ctx_def, &[key, avg_var], deferred_group as usize)
            .unwrap()
            .unwrap();
        assert_eq!(prefix.len(), deferred_group as usize);
        (dir, ctx_def, grouping, key, avg_var, deferred_group)
    };

    // Case 1: observe the division charge during deferred finalization after replay
    // finishes and drops its reader guard, proving `_subtotal` stays live alongside it.
    {
        struct ObserveDiv {
            seen: Option<(u64, u64, u64)>,
        }
        impl super::exec::ScratchAdmit for ObserveDiv {
            fn charge_scratch<'a>(
                &mut self,
                ctx: &'a Ctx,
                bytes: u64,
            ) -> crate::error::Result<super::ctx::Charge<'a>> {
                let before = ctx.live_bytes();
                let charge = ctx.charge(bytes)?;
                let after = ctx.live_bytes();
                self.seen = Some((bytes, before, after));
                Ok(charge)
            }
        }

        let (_dir, ctx_def, mut grouping, key, avg_var, deferred_group) = make_deferred_avg();
        let base_live = ctx_def.live_bytes();
        let mut observe = ObserveDiv { seen: None };
        let out = grouping
            .output_with_scratch(&ctx_def, &[key, avg_var], 1, &mut observe)
            .unwrap()
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out.cols[0][0].as_i64(), deferred_group);
        let (requested, live_before, live_after) = observe
            .seen
            .expect("deferred AVG replay must reach division scratch admission");
        assert_eq!(requested, AGG_SUM_SCRATCH_BYTES);
        assert_eq!(
            live_before,
            base_live + AGG_SUM_PAYLOAD_BYTES,
            "expanded subtotal guard must stay live across division finalization"
        );
        assert_eq!(
            live_after,
            base_live + AGG_SUM_PAYLOAD_BYTES + AGG_SUM_SCRATCH_BYTES,
            "subtotal and division scratch guards must coexist during deferred AVG division"
        );
        let got_lex = ctx_def
            .value(out.cols[1][0])
            .unwrap()
            .lexical()
            .unwrap()
            .to_string();
        assert_eq!(got_lex, nt_lex);
    }

    // Case 2: replay succeeds with ample room, then pinning room just below
    // `AGG_SUM_SCRATCH_BYTES` when division runs refuses specifically at division.
    {
        struct FailAtDiv {
            called: bool,
        }
        impl super::exec::ScratchAdmit for FailAtDiv {
            fn charge_scratch<'a>(
                &mut self,
                ctx: &'a Ctx,
                bytes: u64,
            ) -> crate::error::Result<super::ctx::Charge<'a>> {
                self.called = true;
                assert_eq!(bytes, AGG_SUM_SCRATCH_BYTES);
                let pin = ctx.charge(ctx.memory_room().saturating_sub(bytes - 1))?;
                let res = ctx.charge(bytes);
                drop(pin);
                res
            }
        }

        let (_dir, ctx_def, mut grouping, key, avg_var, _deferred_group) = make_deferred_avg();
        let mut fail_div = FailAtDiv { called: false };
        let err = grouping
            .output_with_scratch(&ctx_def, &[key, avg_var], 1, &mut fail_div)
            .unwrap_err();
        assert!(
            fail_div.called,
            "deferred AVG replay must reach division before refusing"
        );
        assert!(
            matches!(
                err,
                crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
            ),
            "expected memory refusal specifically at deferred AVG division, got {err:?}"
        );
    }
}

#[test]
fn spill_group_big_avg_releases_speculative_headroom_before_interning() {
    use super::bignum::AGG_SUM_SCRATCH_BYTES;
    use super::group::op::Grouping;
    use crate::spill::{Spill, SpillOptions};

    let dir = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: dir.path().to_path_buf(),
        max_bytes: 1 << 30,
        query_bytes: 1 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();
    let big_str = format!("1{}1", "0".repeat(3_998));

    // Case 1: fixed budget where GroupMemory takes its 64 KiB speculative step during fold
    // and leaves only `AGG_SUM_SCRATCH_BYTES + 1024` on the ledger. Trimming speculative
    // headroom before output makes that credit available to both division and novel
    // decimal vocabulary admission.
    {
        let mut ctx = ctx();
        ctx.spill = Some(std::sync::Arc::new(spill.query(None)));
        let key = ctx.var("key");
        let value = ctx.var("value");
        let avg_var = ctx.var("avg");
        let big = ctx.intern_value(&Value::from_typed(
            &big_str,
            oxrdf::vocab::xsd::INTEGER.as_str(),
        ));
        let _ = ctx.value_checked(big).unwrap();

        let mut table = Table::new(vec![key, value]);
        table.push_row(&[int(&ctx, 1), big]);

        let mut g_agg = agg(AggregateFunction::Avg, true, false);
        g_agg.expr = Some(super::expr::Expr::Var(value));

        let base_bytes = 256 + 64 + 1024;
        let per_group = 13 * 2 + 8 + (size_of::<super::exec::AggState>() as u64 * 2 + 8) + 16;
        let setup_live = ctx.live_bytes();
        ctx.mem_limit =
            setup_live + base_bytes + per_group + (64 << 10) + AGG_SUM_SCRATCH_BYTES + 1024;
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();

        let mut grouping =
            Grouping::new(&ctx, &[key], &[(avg_var, g_agg)], &table.vars, true, true).unwrap();
        grouping.start_pass(&ctx);
        grouping.add(&ctx, &table, &mut Default::default()).unwrap();
        let room_before_end = ctx.memory_room();
        assert!(
            room_before_end <= AGG_SUM_SCRATCH_BYTES + 1024,
            "expected speculative reservation to leave <= AGG_SUM_SCRATCH_BYTES + 1024, got {room_before_end}"
        );
        grouping.end_input().unwrap();
        assert!(
            ctx.memory_room() >= room_before_end + (60 << 10),
            "expected end_input to release speculative group headroom"
        );
        let out = grouping.output(&ctx, &[key, avg_var], 1).unwrap().unwrap();
        assert_eq!(out.len(), 1);
        let val = ctx.value(out.cols[1][0]).unwrap();
        assert_eq!(val.lexical().unwrap().as_ref(), big_str);
        assert_eq!(
            val.datatype().unwrap().as_str(),
            oxrdf::vocab::xsd::DECIMAL.as_str()
        );
    }

    // Case 2: calling `output` directly without `end_input` (e.g. `whole`) while the
    // ledger has only `AGG_SUM_SCRATCH_BYTES + 1024` free must also trim speculative
    // headroom before finalizing and interning the novel 4,000-digit decimal result.
    {
        let mut ctx = ctx();
        ctx.mem_limit = 512 << 10;
        ctx.spill = Some(std::sync::Arc::new(spill.query(None)));
        let key = ctx.var("key");
        let value = ctx.var("value");
        let avg_var = ctx.var("avg");
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();

        let big = ctx.intern_value(&Value::from_typed(
            &big_str,
            oxrdf::vocab::xsd::INTEGER.as_str(),
        ));
        let _ = ctx.value_checked(big).unwrap();

        let mut table = Table::new(vec![key, value]);
        table.push_row(&[int(&ctx, 1), big]);

        let mut g_agg = agg(AggregateFunction::Avg, true, false);
        g_agg.expr = Some(super::expr::Expr::Var(value));

        let mut grouping =
            Grouping::new(&ctx, &[key], &[(avg_var, g_agg)], &table.vars, true, true).unwrap();
        grouping.start_pass(&ctx);
        grouping.add(&ctx, &table, &mut Default::default()).unwrap();

        let pin = ctx
            .charge(
                ctx.memory_room()
                    .saturating_sub(AGG_SUM_SCRATCH_BYTES + 1024),
            )
            .unwrap();
        let out = grouping.output(&ctx, &[key, avg_var], 1).unwrap().unwrap();
        drop(pin);
        assert_eq!(out.len(), 1);
        let val = ctx.value(out.cols[1][0]).unwrap();
        assert_eq!(val.lexical().unwrap().as_ref(), big_str);
        assert_eq!(
            val.datatype().unwrap().as_str(),
            oxrdf::vocab::xsd::DECIMAL.as_str()
        );
    }
}

#[test]
fn big_sum_finalization_charges_intern_scratch_on_vocab_hit_and_miss() {
    use super::bignum::intern_scratch_bytes;
    use super::group::op::Grouping;
    use crate::spill::{Spill, SpillOptions};

    struct ObserveScratch {
        charges: Vec<u64>,
    }
    impl super::exec::ScratchAdmit for ObserveScratch {
        fn charge_scratch<'a>(
            &mut self,
            ctx: &'a Ctx,
            bytes: u64,
        ) -> crate::error::Result<super::ctx::Charge<'a>> {
            self.charges.push(bytes);
            ctx.charge(bytes)
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: dir.path().to_path_buf(),
        max_bytes: 1 << 30,
        query_bytes: 1 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();

    let big_int_str = format!("1{}1", "0".repeat(3_998));
    let big_dec_str = format!("1{}.5", "0".repeat(3_997));

    for (label, lex, dt) in [
        (
            "BigInt",
            big_int_str.as_str(),
            oxrdf::vocab::xsd::INTEGER.as_str(),
        ),
        (
            "BigDec",
            big_dec_str.as_str(),
            oxrdf::vocab::xsd::DECIMAL.as_str(),
        ),
    ] {
        // Vocabulary hit: single-row SUM equals the already-interned input term, yet
        // formatting and key encoding must still require scratch credit before lookup.
        let mut ctx = ctx();
        ctx.mem_limit = 512 << 10;
        ctx.spill = Some(std::sync::Arc::new(spill.query(None)));
        let key = ctx.var("key");
        let value = ctx.var("value");
        let sum_var = ctx.var("sum");
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();

        let term_id = ctx.intern_value(&Value::from_typed(lex, dt));
        let _ = ctx.value_checked(term_id).unwrap();

        let mut table = Table::new(vec![key, value]);
        table.push_row(&[int(&ctx, 1), term_id]);

        let mut g_agg = agg(AggregateFunction::Sum, true, false);
        g_agg.expr = Some(super::expr::Expr::Var(value));

        let mut grouping = Grouping::new(
            &ctx,
            &[key],
            &[(sum_var, g_agg.clone())],
            &table.vars,
            true,
            true,
        )
        .unwrap();
        grouping.start_pass(&ctx);
        grouping.add(&ctx, &table, &mut Default::default()).unwrap();
        grouping.end_input().unwrap();

        let pin = ctx.charge(ctx.memory_room().saturating_sub(1024)).unwrap();
        let err = grouping.output(&ctx, &[key, sum_var], 1).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
            ),
            "{label} vocab-hit SUM should refuse formatting/key scratch with only 1 KiB left, got {err:?}"
        );
        drop(pin);

        // With ample room, verify the exact intern scratch charge and vocab hit.
        let mut grouping_ok = Grouping::new(
            &ctx,
            &[key],
            &[(sum_var, g_agg.clone())],
            &table.vars,
            true,
            true,
        )
        .unwrap();
        grouping_ok.start_pass(&ctx);
        grouping_ok
            .add(&ctx, &table, &mut Default::default())
            .unwrap();
        grouping_ok.end_input().unwrap();
        let mut obs = ObserveScratch {
            charges: Vec::new(),
        };
        let out = grouping_ok
            .output_with_scratch(&ctx, &[key, sum_var], 1, &mut obs)
            .unwrap()
            .unwrap();
        assert_eq!(
            obs.charges,
            vec![intern_scratch_bytes(lex.len())],
            "{label}"
        );
        assert_eq!(out.cols[1][0], term_id, "{label}: expected vocabulary hit");

        // Vocabulary miss: two-row SUM produces a novel large term and charges intern
        // scratch before admitting the new vocabulary entry.
        table.push_row(&[int(&ctx, 1), term_id]);
        let mut grouping_miss =
            Grouping::new(&ctx, &[key], &[(sum_var, g_agg)], &table.vars, true, true).unwrap();
        grouping_miss.start_pass(&ctx);
        grouping_miss
            .add(&ctx, &table, &mut Default::default())
            .unwrap();
        grouping_miss.end_input().unwrap();
        let mut obs_miss = ObserveScratch {
            charges: Vec::new(),
        };
        let out_miss = grouping_miss
            .output_with_scratch(&ctx, &[key, sum_var], 1, &mut obs_miss)
            .unwrap()
            .unwrap();
        let out_id = out_miss.cols[1][0];
        assert_ne!(out_id, term_id, "{label}: expected novel vocabulary entry");
        let out_lex = ctx.value(out_id).unwrap().lexical().unwrap().to_string();
        assert_eq!(
            obs_miss.charges,
            vec![intern_scratch_bytes(out_lex.len())],
            "{label} miss"
        );
    }
}

#[test]
fn mixed_numeric_float_and_double_promotion_charges_conversion_scratch() {
    let mut ctx = ctx();
    ctx.mem_limit = 512 << 10;
    // 30-digit values are arbitrary-precision (> i64/Decimal) yet finite in both f32 and f64,
    // so to_f32_exact and to_f64_exact allocate their 150-byte and 820-byte decimal strings.
    let big_int = ctx.intern_value(&Value::from_typed(
        "100000000000000000000000000001",
        oxrdf::vocab::xsd::INTEGER.as_str(),
    ));
    let big_dec = ctx.intern_value(&Value::from_typed(
        "100000000000000000000000000001.5",
        oxrdf::vocab::xsd::DECIMAL.as_str(),
    ));
    let flt = ctx.intern_value(&Value::Float(1.0f32.into()));
    let dbl = ctx.intern_value(&Value::Double(1.0f64.into()));
    for id in [big_int, big_dec, flt, dbl] {
        let _ = ctx.value_checked(id).unwrap();
    }

    let sum_agg = agg(AggregateFunction::Sum, true, false);
    let cases = [
        ("BigInt + Float", big_int, flt),
        ("BigInt + Double", big_int, dbl),
        ("BigDec + Float", big_dec, flt),
        ("BigDec + Double", big_dec, dbl),
        ("Float + BigInt", flt, big_int),
        ("Float + BigDec", flt, big_dec),
        ("Double + BigInt", dbl, big_int),
        ("Double + BigDec", dbl, big_dec),
    ];

    for (label, first, second) in cases {
        let mut acc = Running::new(&sum_agg, false);
        acc.init(1);
        fold(&ctx, &mut acc, &[first], &[0]);
        let table = Table::new(vec![0]);
        let values = [second];
        let batch = Batch {
            table: &table,
            arg: Arg::Column(&values),
            rows: None,
            weights: None,
            base: 0,
        };
        // Leave only 64 bytes free (< TO_F32_SCRATCH_BYTES = 256 and TO_F64_SCRATCH_BYTES = 1024).
        let pin = ctx.charge(ctx.memory_room().saturating_sub(64)).unwrap();
        let mut noop = |_| Ok(());
        let mut budget = super::group::op::BatchBudget::Retain(&mut noop);
        let err = acc.add_batch(&ctx, &batch, &[0], &mut budget).unwrap_err();
        assert!(
            matches!(
                err,
                crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
            ),
            "{label}: expected memory refusal for conversion scratch, got {err:?}"
        );
        drop(pin);

        // After releasing the pin, retrying the refused row succeeds.
        let mut noop = |_| Ok(());
        let mut budget = super::group::op::BatchBudget::Retain(&mut noop);
        acc.add_batch(&ctx, &batch, &[0], &mut budget).unwrap();
        let out = acc.finish(&ctx, 0).unwrap();
        assert_ne!(out, Id::UNDEF, "{label}: expected numeric result");
    }
}

#[test]
fn weighted_add_and_signed_retract_across_retractable_accumulators() {
    let ctx = ctx();
    let v2 = int(&ctx, 2);
    let v5 = int(&ctx, 5);
    let v10 = int(&ctx, 10);
    let table = Table::new(vec![0]);

    let read_str = |acc: &mut dyn Accumulator, slot: usize| -> String {
        let id = acc.read(&ctx, slot).unwrap();
        ctx.value(id)
            .and_then(|v| v.lexical().ok().map(|l| l.to_string()))
            .unwrap_or_else(|| "unbound".to_string())
    };

    let specs = [
        (agg(AggregateFunction::Count, true, false), "5", "2"),
        (agg(AggregateFunction::Count, true, true), "3", "2"),
        (agg(AggregateFunction::Sum, true, false), "21", "12"),
        (agg(AggregateFunction::Sum, true, true), "17", "12"),
        (agg(AggregateFunction::Avg, true, false), "4.2", "6"),
        (agg(AggregateFunction::Min, true, false), "2", "2"),
        (agg(AggregateFunction::Max, true, false), "10", "10"),
        (
            agg(
                AggregateFunction::GroupConcat {
                    separator: Some(",".into()),
                },
                true,
                false,
            ),
            "2,2,2,5,10",
            "2,10",
        ),
        (
            agg(
                AggregateFunction::GroupConcat {
                    separator: Some(",".into()),
                },
                true,
                true,
            ),
            "2,5,10",
            "2,10",
        ),
    ];

    for (a, after_add, after_retract) in specs {
        let mut acc = retractable(&a, 1).unwrap();
        assert!(acc.retractable());
        acc.init(2);
        let add_vals = [v2, v5, v10, v5];
        let add_weights = [3i64, 1, 1, 2];
        let add_slots = [0u32, 0, 0, 1];
        let add_batch = Batch {
            table: &table,
            arg: Arg::Column(&add_vals),
            rows: None,
            weights: Some(&add_weights),
            base: 0,
        };
        add_batch_ok(&ctx, &mut *acc, &add_batch, &add_slots).unwrap();
        assert_eq!(read_str(&mut *acc, 0), after_add, "{:?}", a.func);
        assert!(acc.retained_bytes() > 0);

        let neg_vals = [v2, v5];
        let neg_weights = [-2i64, -1];
        let neg_slots = [0u32, 0];
        let neg_batch = Batch {
            table: &table,
            arg: Arg::Column(&neg_vals),
            rows: None,
            weights: Some(&neg_weights),
            base: 0,
        };
        assert!(acc.retract(&ctx, &neg_batch, &neg_slots).unwrap());
        assert_eq!(read_str(&mut *acc, 0), after_retract, "{:?}", a.func);

        // Removing slot 0 swaps slot 1 into slot 0 and frees slot 0's state.
        acc.swap_remove(0);
        acc.swap_remove(0);
        assert_eq!(acc.retained_bytes(), 0);
    }

    // Overflowing i64 integer sum mixed with a double matches Running (AggState::Sum).
    let max_i = ctx.intern_value(&Value::Integer(i64::MAX.into()));
    let one_d = ctx.intern_value(&Value::Double(1.0f64.into()));
    let sum_agg = agg(AggregateFunction::Sum, true, false);
    let mut running = Running::new(&sum_agg, false);
    let mut ret = retractable(&sum_agg, 1).unwrap();
    running.init(1);
    ret.init(1);
    let vals = [max_i, max_i, one_d];
    let slots = [0u32, 0, 0];
    fold(&ctx, &mut running, &vals, &slots);
    fold(&ctx, &mut *ret, &vals, &slots);
    let got_run = running.finish(&ctx, 0).unwrap();
    let got_ret = ret.read(&ctx, 0).unwrap();
    assert!(ctx.term(got_ret).is_some());
    assert_eq!(ctx.term(got_ret), ctx.term(got_run));
}

#[test]
fn retractable_sum_decimal_overflow_with_float_or_double() {
    let ctx = ctx();
    let big_dec = ctx.intern_value(&Value::Decimal("100000000000000000000.0".parse().unwrap()));
    let zero_d = ctx.intern_value(&Value::Double(0.0f64.into()));
    let zero_f = ctx.intern_value(&Value::Float(0.0f32.into()));
    let max_i = ctx.intern_value(&Value::Integer(i64::MAX.into()));
    let table = Table::new(vec![0]);

    for func in [AggregateFunction::Sum, AggregateFunction::Avg] {
        let a = agg(func.clone(), true, false);
        let mut running = Running::new(&a, false);
        let mut ret = retractable(&a, 1).unwrap();
        running.init(1);
        ret.init(1);

        // 0.0e0 followed by two 1e20 decimals: subtotal 2e20 exceeds fixed xsd:decimal,
        // enters double arithmetic and stays finite in both Running and RetractableSum.
        let vals = [zero_d, big_dec, big_dec];
        let slots = [0u32, 0, 0];
        fold(&ctx, &mut running, &vals, &slots);
        fold(&ctx, &mut *ret, &vals, &slots);
        let got_run = running.finish(&ctx, 0).unwrap();
        let got_ret = ret.read(&ctx, 0).unwrap();
        assert!(!got_ret.is_undef(), "{func:?} should stay finite");
        assert_eq!(ctx.term(got_ret), ctx.term(got_run), "{func:?}");

        // Retracting 0.0e0 demotes back to exact arbitrary-precision decimal matching Running.
        let retract_d = Batch {
            table: &table,
            arg: Arg::Column(&[zero_d]),
            rows: None,
            weights: Some(&[-1]),
            base: 0,
        };
        assert!(ret.retract(&ctx, &retract_d, &[0]).unwrap());
        let mut run_dec = Running::new(&a, false);
        run_dec.init(1);
        fold(&ctx, &mut run_dec, &[big_dec, big_dec], &[0, 0]);
        assert_eq!(
            ctx.term(ret.read(&ctx, 0).unwrap()),
            ctx.term(run_dec.finish(&ctx, 0).unwrap()),
            "{func:?} demotes to exact BigDecimal"
        );

        // Retracting one 1e20 recovers to exact decimal 1e20.
        let retract_dec = Batch {
            table: &table,
            arg: Arg::Column(&[big_dec]),
            rows: None,
            weights: Some(&[-1]),
            base: 0,
        };
        assert!(ret.retract(&ctx, &retract_dec, &[0]).unwrap());
        let recovered = ret.read(&ctx, 0).unwrap();
        assert_eq!(ctx.term(recovered), ctx.term(big_dec), "{func:?} recovers");

        // Float promotion on overflowing fixed decimal subtotal.
        let mut run_f = Running::new(&a, false);
        let mut ret_f = retractable(&a, 1).unwrap();
        run_f.init(1);
        ret_f.init(1);
        let fvals = [zero_f, big_dec, big_dec];
        fold(&ctx, &mut run_f, &fvals, &slots);
        fold(&ctx, &mut *ret_f, &fvals, &slots);
        assert_eq!(
            ctx.term(ret_f.read(&ctx, 0).unwrap()),
            ctx.term(run_f.finish(&ctx, 0).unwrap()),
            "{func:?} float"
        );

        // Integer subtotal overflowing both i64 and fixed xsd:decimal (20 * i64::MAX > 1.7e20)
        // stays finite when a double is present and demotes to exact arbitrary-precision
        // integer/decimal when retracted.
        let mut ret_i = retractable(&a, 1).unwrap();
        ret_i.init(1);
        let add_i = Batch {
            table: &table,
            arg: Arg::Column(&[max_i, zero_d]),
            rows: None,
            weights: Some(&[20, 1]),
            base: 0,
        };
        add_batch_ok(&ctx, &mut *ret_i, &add_i, &[0, 0]).unwrap();
        let got_i = ret_i.read(&ctx, 0).unwrap();
        assert!(
            matches!(ctx.value(got_i), Some(Value::Double(d)) if f64::from(d).is_finite()),
            "{func:?} large int + double: {:?}",
            ctx.value(got_i)
        );
        assert!(ret_i.retract(&ctx, &retract_d, &[0]).unwrap());
        let mut run_i = Running::new(&a, false);
        run_i.init(1);
        let twenty_max = vec![max_i; 20];
        let twenty_slots = vec![0u32; 20];
        fold(&ctx, &mut run_i, &twenty_max, &twenty_slots);
        assert_eq!(
            ctx.term(ret_i.read(&ctx, 0).unwrap()),
            ctx.term(run_i.finish(&ctx, 0).unwrap()),
            "{func:?} large int demotes to exact BigInt/BigDec"
        );

        // Mixed DOUBLE + overflowing fixed DECIMAL + overflowing repeated FLOAT (2 * 2e38f > f32::MAX):
        // widening float contributions to f64 keeps SUM and AVG finite and matches Running.
        let big_f = ctx.intern_value(&Value::Float(2e38_f32.into()));
        let big_f2 = ctx.intern_value(&Value::Float(2.5e38_f32.into()));
        for (label, vals) in [
            (
                "double+2*dec+2*float",
                vec![zero_d, big_dec, big_dec, big_f, big_f],
            ),
            ("double+2*float", vec![zero_d, big_f, big_f]),
            ("double+2*distinct_float", vec![zero_d, big_f, big_f2]),
        ] {
            let mut run_m = Running::new(&a, false);
            let mut ret_m = retractable(&a, 1).unwrap();
            run_m.init(1);
            ret_m.init(1);
            let mslots = vec![0u32; vals.len()];
            fold(&ctx, &mut run_m, &vals, &mslots);
            fold(&ctx, &mut *ret_m, &vals, &mslots);
            let got_run = run_m.finish(&ctx, 0).unwrap();
            let got_ret = ret_m.read(&ctx, 0).unwrap();
            assert!(
                matches!(ctx.value(got_ret), Some(Value::Double(d)) if f64::from(d).is_finite()),
                "{func:?} {label} should stay finite double, got {:?}",
                ctx.value(got_ret)
            );
            assert_eq!(ctx.term(got_ret), ctx.term(got_run), "{func:?} {label}");

            // Retracting 0.0e0 demotes back to xsd:float (+inf).
            assert!(ret_m.retract(&ctx, &retract_d, &[0]).unwrap());
            let after_demote = ret_m.read(&ctx, 0).unwrap();
            assert!(
                matches!(ctx.value(after_demote), Some(Value::Float(f)) if f32::from(f).is_infinite()),
                "{func:?} {label} after retracting double should demote to float inf, got {:?}",
                ctx.value(after_demote)
            );
        }
    }
}

#[test]
fn sum_replacement_stays_charged_across_admit_and_drop() {
    use super::exec::{AggState, ScratchAdmit, SumAdmit};

    struct ObserveAdmit<'a> {
        ctx: &'a Ctx,
        accept: bool,
        seen: Option<(u64, u64, u64)>,
    }

    impl ScratchAdmit for ObserveAdmit<'_> {}

    impl SumAdmit for ObserveAdmit<'_> {
        fn admit(&mut self, old: u64, new: u64) -> crate::error::Result<bool> {
            self.seen = Some((old, new, self.ctx.live_bytes()));
            Ok(self.accept)
        }
    }

    let ctx = ctx();
    let small_big = ctx.intern_value(&Value::from_typed(
        &format!("1{}", "0".repeat(1_500)),
        oxrdf::vocab::xsd::INTEGER.as_str(),
    ));
    let large_big = ctx.intern_value(&Value::from_typed(
        &format!("1{}", "0".repeat(3_999)),
        oxrdf::vocab::xsd::INTEGER.as_str(),
    ));
    let one = int(&ctx, 1);
    for id in [small_big, large_big, one] {
        let _ = ctx.value_checked(id).unwrap();
    }

    let sum_agg = agg(AggregateFunction::Sum, true, false);
    let mut state = AggState::new(&sum_agg);
    state
        .add(&ctx, &sum_agg, Some(small_big), &mut |_| Ok(()))
        .unwrap();
    let live_before = ctx.live_bytes();

    // Refusal path: candidate must remain charged while `admit` inspects it, then both
    // candidate and scratch drop cleanly and the old subtotal stays intact.
    let mut refuse = ObserveAdmit {
        ctx: &ctx,
        accept: false,
        seen: None,
    };
    let accepted = state
        .add_admitted(&ctx, &sum_agg, Some(large_big), &mut refuse)
        .unwrap();
    assert!(!accepted);
    let (old_b, new_b, live_during) = refuse.seen.expect("admit called on growing replacement");
    assert!(new_b > old_b);
    assert!(
        live_during >= live_before + new_b,
        "candidate ({new_b} B) must stay charged during admit (before {live_before}, during {live_during})"
    );
    assert_eq!(ctx.live_bytes(), live_before);

    // Acceptance path: candidate remains charged across `admit`, then scratch drops only
    // after the old subtotal is replaced and dropped.
    let mut accept = ObserveAdmit {
        ctx: &ctx,
        accept: true,
        seen: None,
    };
    let accepted = state
        .add_admitted(&ctx, &sum_agg, Some(large_big), &mut accept)
        .unwrap();
    assert!(accepted);
    let (old_b, new_b, live_during) = accept.seen.expect("admit called on growing replacement");
    assert!(new_b > old_b);
    assert!(
        live_during >= live_before + new_b,
        "candidate ({new_b} B) must stay charged during admit (before {live_before}, during {live_during})"
    );
    assert_eq!(ctx.live_bytes(), live_before);

    // Same-capacity replacement skips `admit`, but still keeps scratch alive across
    // replacement and releases it cleanly afterwards.
    let mut same_cap = ObserveAdmit {
        ctx: &ctx,
        accept: true,
        seen: None,
    };
    let accepted = state
        .add_admitted(&ctx, &sum_agg, Some(one), &mut same_cap)
        .unwrap();
    assert!(accepted);
    assert!(
        same_cap.seen.is_none(),
        "expected same-capacity replacement to skip admit"
    );
    assert_eq!(ctx.live_bytes(), live_before);

    let final_id = state.finish(&ctx, &sum_agg).unwrap();
    let lex = ctx.value(final_id).unwrap().lexical().unwrap().to_string();
    assert!(lex.starts_with('1') && lex.ends_with('1'));
    assert_eq!(lex.len(), 4_000);
}

#[test]
fn retract_rejects_i64_min_without_mutating_state() {
    let ctx = ctx();
    let v5 = int(&ctx, 5);
    let mut table = Table::new(vec![0]);
    table.push_row(&[v5]);

    let cases = [
        agg(AggregateFunction::Sum, true, false),
        agg(AggregateFunction::Avg, true, false),
        agg(AggregateFunction::Min, true, false),
        agg(
            AggregateFunction::GroupConcat {
                separator: Some(",".into()),
            },
            true,
            false,
        ),
        agg(AggregateFunction::Count, true, true),
        agg(AggregateFunction::Count, false, true),
    ];

    for a in cases {
        let mut acc = retractable(&a, 1).unwrap();
        acc.init(1);
        let arg = if a.expr.is_some() {
            Arg::Column(&[v5])
        } else {
            Arg::Star
        };
        let add = Batch {
            table: &table,
            arg,
            rows: None,
            weights: Some(&[3]),
            base: 0,
        };
        add_batch_ok(&ctx, &mut *acc, &add, &[0]).unwrap();
        let before = acc.read(&ctx, 0).unwrap();

        let bad = Batch {
            table: &table,
            arg,
            rows: None,
            weights: Some(&[i64::MIN]),
            base: 0,
        };
        assert!(!acc.retract(&ctx, &bad, &[0]).unwrap(), "{:?}", a.func);
        assert_eq!(acc.read(&ctx, 0).unwrap(), before, "{:?}", a.func);
    }

    // Running COUNT handles i64::MIN without overflowing.
    let mut count = retractable(&agg(AggregateFunction::Count, true, false), 1).unwrap();
    count.init(1);
    let add = Batch {
        table: &table,
        arg: Arg::Column(&[v5]),
        rows: None,
        weights: Some(&[3]),
        base: 0,
    };
    add_batch_ok(&ctx, &mut *count, &add, &[0]).unwrap();
    let bad = Batch {
        table: &table,
        arg: Arg::Column(&[v5]),
        rows: None,
        weights: Some(&[i64::MIN]),
        base: 0,
    };
    assert!(count.retract(&ctx, &bad, &[0]).unwrap());
    assert_eq!(count.read(&ctx, 0).unwrap(), int(&ctx, 0));
}

#[test]
fn running_weighted_tracks_best_payload_and_polls_cancellation() {
    let ctx = ctx();
    let big_str = ctx.intern_value(&Value::Str("a".repeat(2048).into()));
    let dec = ctx.intern_value(&Value::Decimal("1.5".parse().unwrap()));
    let table = Table::new(vec![0]);

    // Weighted MIN with track = true accounts for decoded best payload and runs in O(1).
    let mut min_acc = Running::new(&agg(AggregateFunction::Min, true, false), true);
    min_acc.init(1);
    let batch = Batch {
        table: &table,
        arg: Arg::Column(&[big_str]),
        rows: None,
        weights: Some(&[1_000_000]),
        base: 0,
    };
    add_batch_ok(&ctx, &mut min_acc, &batch, &[0]).unwrap();
    assert!(min_acc.take_growth() >= 2048);
    assert_eq!(min_acc.read(&ctx, 0).unwrap(), big_str);

    // Weighted SAMPLE completes in O(1).
    let mut sample_acc = Running::new(&agg(AggregateFunction::Sample, true, false), false);
    sample_acc.init(1);
    add_batch_ok(&ctx, &mut sample_acc, &batch, &[0]).unwrap();
    assert_eq!(sample_acc.read(&ctx, 0).unwrap(), big_str);

    // Weighted SUM polls cancellation inside its fallback loop.
    ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut sum_acc = Running::new(&agg(AggregateFunction::Sum, true, false), false);
    sum_acc.init(1);
    let sum_batch = Batch {
        table: &table,
        arg: Arg::Column(&[dec]),
        rows: None,
        weights: Some(&[1_000_000]),
        base: 0,
    };
    let err = add_batch_ok(&ctx, &mut sum_acc, &sum_batch, &[0]).unwrap_err();
    assert!(matches!(err, crate::Error::Cancelled), "got {err:?}");
}

#[test]
fn retractable_retained_bytes_charges_capacity_and_reclaims_on_drain() {
    let count_agg = agg(AggregateFunction::Count, false, false);
    let mut acc = retractable(&count_agg, 1).unwrap();
    acc.init(512);
    let peak = acc.retained_bytes();
    assert!(
        peak >= 512 * std::mem::size_of::<super::exec::AggState>() as u64,
        "COUNT retained_bytes {peak} must charge full AggState capacity"
    );
    for _ in 0..450 {
        acc.swap_remove(0);
    }
    acc.shrink_unused();
    let shrunk = acc.retained_bytes();
    assert!(
        shrunk < peak / 2,
        "expected shrunk retained_bytes ({shrunk}) < half of peak ({peak})"
    );
    for _ in 0..62 {
        acc.swap_remove(0);
    }
    assert_eq!(acc.retained_bytes(), 0);
}

#[test]
fn ordered_concat_admits_growth_before_allocating_and_polls_repetitions() {
    let concat_agg = agg(
        AggregateFunction::GroupConcat {
            separator: Some(",".into()),
        },
        true,
        false,
    );
    let table = Table::new(vec![0]);

    // 1. One distinct string with huge weight under a tight memory budget must fail before
    //    allocating past `mem_limit`.
    let mut tight = ctx();
    tight.mem_limit = 4096;
    let tight = tight.into_shared();
    let s = tight.intern_value(&Value::Str("payload-0123456789abcdef".into()));
    let mut acc = retractable(&concat_agg, 1).unwrap();
    acc.init(1);
    let batch = Batch {
        table: &table,
        arg: Arg::Column(&[s]),
        rows: None,
        weights: Some(&[100_000]),
        base: 0,
    };
    add_batch_ok(&tight, &mut *acc, &batch, &[0]).unwrap();
    let err = acc
        .read(&tight, 0)
        .expect_err("expected BudgetExceeded before expanding repetitions");
    assert!(
        matches!(err, crate::Error::BudgetExceeded(_)),
        "expected BudgetExceeded, got {err:?}"
    );
    assert!(
        tight.mem_peak() <= 4096,
        "mem_peak {} exceeded limit 4096 before admission",
        tight.mem_peak()
    );

    // 2. Expanding many repetitions of one distinct string polls `ctx.check()` every 1,024 reps.
    let ok = ctx().into_shared();
    let short = ok.intern_value(&Value::Str("a".into()));
    let mut acc_ok = retractable(&concat_agg, 1).unwrap();
    acc_ok.init(1);
    let rep_batch = Batch {
        table: &table,
        arg: Arg::Column(&[short]),
        rows: None,
        weights: Some(&[5_000]),
        base: 0,
    };
    add_batch_ok(&ok, &mut *acc_ok, &rep_batch, &[0]).unwrap();
    let before_checks = crate::sparql::ctx::CHECKS.with(|n| n.get());
    let out = acc_ok.read(&ok, 0).unwrap();
    let checks = crate::sparql::ctx::CHECKS.with(|n| n.get()) - before_checks;
    assert!(
        checks >= 5,
        "expected repetition loop to poll check() across chunks, got {checks}"
    );
    assert_eq!(
        ok.value(out)
            .and_then(|v| v.lexical().ok().map(|l| l.len())),
        Some(5_000 * 2 - 1)
    );

    // 3. Cancellation on one distinct string with huge weight aborts immediately without allocation.
    let cancelled = ctx().into_shared();
    let s2 = cancelled.intern_value(&Value::Str("abcd".into()));
    let mut acc_cancel = retractable(&concat_agg, 1).unwrap();
    acc_cancel.init(1);
    let big_batch = Batch {
        table: &table,
        arg: Arg::Column(&[s2]),
        rows: None,
        weights: Some(&[10_000_000]),
        base: 0,
    };
    add_batch_ok(&cancelled, &mut *acc_cancel, &big_batch, &[0]).unwrap();
    let peak_before = cancelled.mem_peak();
    cancelled
        .cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let err = acc_cancel
        .read(&cancelled, 0)
        .expect_err("expected Cancelled");
    assert!(matches!(err, crate::Error::Cancelled), "got {err:?}");
    assert_eq!(
        cancelled.mem_peak(),
        peak_before,
        "cancelled GROUP_CONCAT must not allocate output buffer"
    );

    // 4. Empty string + empty separator with huge weight finishes in O(1).
    let empty_agg = agg(
        AggregateFunction::GroupConcat {
            separator: Some(String::new()),
        },
        true,
        false,
    );
    let empty_str = ok.intern_value(&Value::Str("".into()));
    let mut acc_empty = retractable(&empty_agg, 1).unwrap();
    acc_empty.init(1);
    let empty_batch = Batch {
        table: &table,
        arg: Arg::Column(&[empty_str]),
        rows: None,
        weights: Some(&[10_000_000]),
        base: 0,
    };
    add_batch_ok(&ok, &mut *acc_empty, &empty_batch, &[0]).unwrap();
    let empty_out = acc_empty.read(&ok, 0).unwrap();
    assert_eq!(
        ok.value(empty_out)
            .and_then(|v| v.lexical().ok().map(|l| l.to_string())),
        Some(String::new())
    );

    // 5. Finalization under tight memory budgets for a 48 KiB result must not allocate
    //    unbudgeted intermediate copies or FSST base-vocabulary lookup scratch: an
    //    already-stored FSST-coded base-vocab key succeeds within budget, and a brand-new
    //    key is refused before allocating its retained vocabulary copy (both `Ordered`
    //    and `Concat`, with `Concat`'s text growth charged as `Grouping::add` does).
    let chunk_a = "abcdefgh".repeat(128);
    assert_eq!(chunk_a.len(), 1024);
    let chunk_miss = format!("{}abcdefgz", "abcdefgh".repeat(127));
    assert_eq!(chunk_miss.len(), 1024);
    let existing_48k = chunk_a.repeat(48);
    let store = Store::in_memory(StoreOptions {
        build: crate::builder::BuildOptions {
            vocab_codec: Some(crate::builder::VocabCodec::Fsst),
            ..Default::default()
        },
        ..Default::default()
    });
    let nt = format!("<urn:s> <urn:p> \"a0\" .\n<urn:s> <urn:p> \"{existing_48k}\" .\n");
    store
        .load(&[crate::io::Source::from_bytes(
            nt.into_bytes(),
            oxrdfio::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let snap = store.snapshot();
    assert_eq!(
        snap.generation.vocab_codec(),
        crate::builder::VocabCodec::Fsst
    );
    let existing_key = crate::id::term_key(&oxrdf::Term::Literal(
        oxrdf::Literal::new_simple_literal(&existing_48k),
    ));
    let (existing_id, hit_lookup_stats) =
        crate::test_alloc::record_alloc_stats(|| snap.lookup_key(&existing_key));
    let existing_id = existing_id.expect("48 KiB literal stored in FSST base vocabulary");
    assert_eq!(existing_id.tag(), crate::id::Tag::Vocab);
    assert!(
        !(existing_id.payload() as usize).is_multiple_of(crate::vocab::FC_BLOCK),
        "48 KiB literal must be a non-first entry in its FSST block"
    );
    assert!(
        snap.generation.vocab.is_entry_coded(existing_id.payload()),
        "48 KiB literal must have an FSST-coded suffix"
    );
    assert_eq!(
        hit_lookup_stats.total_alloc, 0,
        "FSST hit lookup allocated uncharged scratch: {hit_lookup_stats:?}"
    );
    let mut miss_last_byte_key = existing_key.clone();
    let last_lex = miss_last_byte_key.len() - 2;
    miss_last_byte_key[last_lex] = b'z';
    let (miss_lookup, miss_lookup_stats) =
        crate::test_alloc::record_alloc_stats(|| snap.lookup_key(&miss_last_byte_key));
    assert_eq!(miss_lookup, None);
    assert_eq!(
        miss_lookup_stats.total_alloc, 0,
        "FSST miss lookup allocated uncharged scratch: {miss_lookup_stats:?}"
    );

    // Retractable `Ordered::read` hit on the FSST base-vocabulary entry under 64 KiB.
    let mut existing_ctx = Ctx::new(snap.clone());
    existing_ctx.mem_limit = 64 * 1024;
    let existing_ctx = existing_ctx.into_shared();
    let chunk_a_id = existing_ctx.intern_value(&Value::Str(chunk_a.clone().into()));
    let mut acc_existing = retractable(&empty_agg, 1).unwrap();
    acc_existing.init(1);
    add_batch_ok(
        &existing_ctx,
        &mut *acc_existing,
        &Batch {
            table: &table,
            arg: Arg::Column(&[chunk_a_id]),
            rows: None,
            weights: Some(&[48]),
            base: 0,
        },
        &[0],
    )
    .unwrap();
    let (got_existing, ret_hit_stats) =
        crate::test_alloc::record_alloc_stats(|| acc_existing.read(&existing_ctx, 0));
    assert_eq!(got_existing.unwrap(), existing_id);
    assert!(
        existing_ctx.mem_peak() <= 64 * 1024,
        "existing 48 KiB GROUP_CONCAT exceeded 64 KiB budget: {}",
        existing_ctx.mem_peak()
    );
    assert!(
        ret_hit_stats.peak_live < 56 * 1024 && ret_hit_stats.total_alloc < 56 * 1024,
        "retractable hit allocated extra copies or lookup scratch: {ret_hit_stats:?}"
    );

    // Retractable `Ordered::read` miss with matching prefix against the FSST entry under 64 KiB.
    let mut new_ctx = Ctx::new(snap.clone());
    new_ctx.mem_limit = 64 * 1024;
    let new_ctx = new_ctx.into_shared();
    let chunk_miss_id = new_ctx.intern_value(&Value::Str(chunk_miss.clone().into()));
    let mut acc_new = retractable(&empty_agg, 1).unwrap();
    acc_new.init(1);
    add_batch_ok(
        &new_ctx,
        &mut *acc_new,
        &Batch {
            table: &table,
            arg: Arg::Column(&[chunk_miss_id]),
            rows: None,
            weights: Some(&[48]),
            base: 0,
        },
        &[0],
    )
    .unwrap();
    let (err, ret_miss_stats) = crate::test_alloc::record_alloc_stats(|| acc_new.read(&new_ctx, 0));
    let err = err.expect_err("new 48 KiB key plus 48 KiB key buffer must exceed 64 KiB budget");
    assert!(
        matches!(err, crate::Error::BudgetExceeded(_)),
        "got {err:?}"
    );
    assert!(
        new_ctx.mem_peak() <= 64 * 1024,
        "mem_peak {} exceeded 64 KiB budget during finalization",
        new_ctx.mem_peak()
    );
    assert!(
        ret_miss_stats.peak_live < 56 * 1024 && ret_miss_stats.total_alloc < 56 * 1024,
        "retractable miss allocated extra copies or lookup scratch: {ret_miss_stats:?}"
    );

    // One-off `Concat::finish` hit and miss with `take_growth()` charged (mirroring
    // `Grouping::add`): folding 48 x 1 KiB chunks grows the String capacity to 64 KiB
    // (`take_growth() == 128 KiB`), so a 160 KiB limit leaves ~30 KiB (< 48 KiB) of
    // headroom during finalization.
    const ONE_OFF_BUDGET: u64 = 160 * 1024;
    let mut one_off_existing_ctx = Ctx::new(snap.clone());
    one_off_existing_ctx.mem_limit = ONE_OFF_BUDGET;
    let one_off_existing_ctx = one_off_existing_ctx.into_shared();
    let chunk_a_one_off = one_off_existing_ctx.intern_value(&Value::Str(chunk_a.clone().into()));
    let mut concat_acc = Concat::new(&empty_agg);
    concat_acc.init(1);
    let vals_48 = vec![chunk_a_one_off; 48];
    let slots_48 = vec![0u32; 48];
    fold(&one_off_existing_ctx, &mut concat_acc, &vals_48, &slots_48);
    let growth = concat_acc.take_growth();
    assert!(
        growth >= 96 * 1024,
        "expected text growth charge, got {growth}"
    );
    let _growth = one_off_existing_ctx.charge(growth).unwrap();
    let (got_one_off, one_off_hit_stats) =
        crate::test_alloc::record_alloc_stats(|| concat_acc.finish(&one_off_existing_ctx, 0));
    assert_eq!(got_one_off.unwrap(), existing_id);
    assert!(
        one_off_existing_ctx.mem_peak() <= ONE_OFF_BUDGET,
        "one-off Concat::finish exceeded budget: {}",
        one_off_existing_ctx.mem_peak()
    );
    assert!(
        one_off_hit_stats.peak_live < 4096 && one_off_hit_stats.total_alloc < 4096,
        "one-off hit allocated copies or lookup scratch: {one_off_hit_stats:?}"
    );

    // One-off `Concat::finish` miss differing only on the final byte of the 48 KiB key.
    let mut one_off_miss_ctx = Ctx::new(snap.clone());
    one_off_miss_ctx.mem_limit = ONE_OFF_BUDGET;
    let one_off_miss_ctx = one_off_miss_ctx.into_shared();
    let chunk_a_miss = one_off_miss_ctx.intern_value(&Value::Str(chunk_a.into()));
    let chunk_b_miss = one_off_miss_ctx.intern_value(&Value::Str(chunk_miss.into()));
    let mut concat_miss = Concat::new(&empty_agg);
    concat_miss.init(1);
    let mut vals_miss = vec![chunk_a_miss; 47];
    vals_miss.push(chunk_b_miss);
    fold(&one_off_miss_ctx, &mut concat_miss, &vals_miss, &slots_48);
    let miss_growth = concat_miss.take_growth();
    assert!(
        miss_growth >= 96 * 1024,
        "expected text growth charge, got {miss_growth}"
    );
    let _miss_growth = one_off_miss_ctx.charge(miss_growth).unwrap();
    let (err, one_off_miss_stats) =
        crate::test_alloc::record_alloc_stats(|| concat_miss.finish(&one_off_miss_ctx, 0));
    let err = err.expect_err("one-off miss with charged text buffer must exceed budget");
    assert!(
        matches!(err, crate::Error::BudgetExceeded(_)),
        "got {err:?}"
    );
    assert!(
        one_off_miss_ctx.mem_peak() <= ONE_OFF_BUDGET,
        "one-off miss mem_peak {} exceeded budget",
        one_off_miss_ctx.mem_peak()
    );
    assert!(
        one_off_miss_stats.peak_live < 4096 && one_off_miss_stats.total_alloc < 4096,
        "one-off miss allocated copies or lookup scratch: {one_off_miss_stats:?}"
    );

    // One-off `Concat::finish` when `text.capacity() < text.len() + 2` (exact 48 KiB
    // single-chunk buffer): framing growth is admitted before reallocating, so a budget
    // without room for the reallocation refuses before growing the buffer.
    let mut tight_ctx = Ctx::new(snap);
    tight_ctx.mem_limit = 128 * 1024;
    let tight_ctx = tight_ctx.into_shared();
    let mut concat_tight = Concat::new(&empty_agg);
    concat_tight.init(1);
    fold(&tight_ctx, &mut concat_tight, &[existing_id], &[0]);
    let tight_growth = concat_tight.take_growth();
    assert_eq!(tight_growth, (48 * 1024 * 2) as u64);
    let _tight_growth = tight_ctx.charge(tight_growth).unwrap();
    let (err, tight_stats) =
        crate::test_alloc::record_alloc_stats(|| concat_tight.finish(&tight_ctx, 0));
    let err = err.expect_err("capacity-tight one-off framing growth must exceed budget");
    assert!(
        matches!(err, crate::Error::BudgetExceeded(_)),
        "got {err:?}"
    );
    assert!(
        tight_stats.peak_live < 4096 && tight_stats.total_alloc < 4096,
        "capacity-tight refusal allocated before rejecting: {tight_stats:?}"
    );

    // 6. Unrepresentable lengths (> isize::MAX) are rejected before the first reservation even
    //    under a permissive memory limit.
    let permissive = ctx().into_shared();
    let two_bytes = permissive.intern_value(&Value::Str("ab".into()));
    let one_byte = permissive.intern_value(&Value::Str("a".into()));
    let mut acc_overflow = retractable(&empty_agg, 1).unwrap();
    acc_overflow.init(2);
    add_batch_ok(
        &permissive,
        &mut *acc_overflow,
        &Batch {
            table: &table,
            arg: Arg::Column(&[two_bytes, one_byte]),
            rows: None,
            weights: Some(&[(isize::MAX / 2) as i64 + 1, isize::MAX as i64]),
            base: 0,
        },
        &[0, 1],
    )
    .unwrap();
    let peak_before = permissive.mem_peak();
    for slot in [0, 1] {
        let err = acc_overflow
            .read(&permissive, slot)
            .expect_err("expected BudgetExceeded for unrepresentable String/Vec length");
        assert!(
            matches!(err, crate::Error::BudgetExceeded(_)),
            "got {err:?}"
        );
        assert_eq!(
            permissive.mem_peak(),
            peak_before,
            "unrepresentable GROUP_CONCAT length (slot {slot}) must not charge or allocate"
        );
    }
}

#[test]
fn weighted_running_count_add_and_retract_poll_cancellation() {
    let ctx = ctx();
    let v1 = int(&ctx, 1);
    let table = Table::new(vec![0]);
    let slots = vec![0u32; 5_000];
    let vals = vec![v1; 5_000];
    let pos_weights = vec![1i64; 5_000];
    let neg_weights = vec![-1i64; 5_000];

    for (expr, arg_star) in [(false, true), (true, false)] {
        let count_agg = agg(AggregateFunction::Count, expr, false);
        let mut acc = Running::new(&count_agg, false);
        acc.init(1);
        let arg = if arg_star {
            Arg::Star
        } else {
            Arg::Column(&vals)
        };
        let add_batch = Batch {
            table: &table,
            arg,
            rows: None,
            weights: Some(&pos_weights),
            base: 0,
        };
        let before = crate::sparql::ctx::CHECKS.with(|n| n.get());
        add_batch_ok(&ctx, &mut acc, &add_batch, &slots).unwrap();
        let checks = crate::sparql::ctx::CHECKS.with(|n| n.get()) - before;
        assert!(
            checks >= 5,
            "weighted COUNT (expr={expr}) add_batch must poll check(), got {checks}"
        );

        let ret_batch = Batch {
            table: &table,
            arg,
            rows: None,
            weights: Some(&neg_weights),
            base: 0,
        };
        let before_ret = crate::sparql::ctx::CHECKS.with(|n| n.get());
        assert!(acc.retract(&ctx, &ret_batch, &slots).unwrap());
        let ret_checks = crate::sparql::ctx::CHECKS.with(|n| n.get()) - before_ret;
        assert!(
            ret_checks >= 5,
            "weighted COUNT (expr={expr}) retract must poll check(), got {ret_checks}"
        );

        ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let err = add_batch_ok(&ctx, &mut acc, &add_batch, &slots).unwrap_err();
        assert!(matches!(err, crate::Error::Cancelled), "got {err:?}");
        let err = acc.retract(&ctx, &ret_batch, &slots).unwrap_err();
        assert!(matches!(err, crate::Error::Cancelled), "got {err:?}");
        ctx.cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

fn dec(ctx: &Ctx, raw: i128) -> Id {
    ctx.intern_value(&Value::Decimal(super::value::decimal_from_raw(raw)))
}

fn dec_str(ctx: &Ctx, s: &str) -> Id {
    ctx.intern_value(&Value::Decimal(s.parse().unwrap()))
}

fn dbl(ctx: &Ctx, x: f64) -> Id {
    ctx.intern_value(&Value::Double(x.into()))
}

fn flt(ctx: &Ctx, x: f32) -> Id {
    ctx.intern_value(&Value::Float(x.into()))
}

fn exact_key(ctx: &Ctx, id: Id) -> String {
    if id.is_undef() {
        return "UNDEF".to_string();
    }
    match ctx.value(id) {
        Some(Value::Float(f)) => {
            let v = f32::from(f);
            if v.is_nan() {
                "float:NaN".to_string()
            } else {
                format!("float:{:#010x}", v.to_bits())
            }
        }
        Some(Value::Double(d)) => {
            let v = f64::from(d);
            if v.is_nan() {
                "double:NaN".to_string()
            } else {
                format!("double:{:#018x}", v.to_bits())
            }
        }
        Some(v) => format!("{:?}", v.to_term()),
        None => "NONE".to_string(),
    }
}

fn single_pass_result(ctx: &Ctx, agg: &Agg, values: &[Id]) -> String {
    let mut one = Running::new(agg, true);
    one.init(1);
    let slots = vec![0u32; values.len()];
    fold(ctx, &mut one, values, &slots);
    let id = one.finish(ctx, 0).unwrap();
    exact_key(ctx, id)
}

#[test]
fn add_scaled_int_handles_exact_i128_boundaries() {
    use super::value::{DECIMAL_SCALE, add_scaled_int};
    let q_max = i128::MAX.div_euclid(DECIMAL_SCALE);
    let r_max = i128::MAX.rem_euclid(DECIMAL_SCALE);
    let q_min = i128::MIN.div_euclid(DECIMAL_SCALE);
    let r_min = i128::MIN.rem_euclid(DECIMAL_SCALE);

    assert_eq!(add_scaled_int(r_max, q_max), Some(i128::MAX));
    assert_eq!(add_scaled_int(r_max + 1, q_max), None);
    assert_eq!(add_scaled_int(i128::MAX, 1), None);
    assert_eq!(add_scaled_int(r_min, q_min), Some(i128::MIN));
    assert_eq!(add_scaled_int(r_min - 1, q_min), None);
    assert_eq!(add_scaled_int(i128::MIN, -1), None);
    // int * DECIMAL_SCALE alone overflows i128, yet dec + int * DECIMAL_SCALE fits.
    let big = 2 * q_max;
    assert!(big.checked_mul(DECIMAL_SCALE).is_none());
    assert_eq!(
        add_scaled_int(i128::MAX, -big),
        Some(i128::MAX - q_max * DECIMAL_SCALE - q_max * DECIMAL_SCALE)
    );
    assert_eq!(
        add_scaled_int(i128::MIN, big),
        Some(i128::MIN + q_max * DECIMAL_SCALE + q_max * DECIMAL_SCALE)
    );
}

#[test]
fn sum_and_avg_merge_refuses_non_associative_float_and_double_subtotals() {
    let ctx = ctx();
    let cases: Vec<(&str, Vec<Id>, Vec<Id>)> = vec![
        (
            "double non-associative",
            vec![dbl(&ctx, 1e16)],
            vec![dbl(&ctx, 1.0), dbl(&ctx, 1.0)],
        ),
        (
            "float non-associative",
            vec![flt(&ctx, 16_777_216.0)],
            vec![flt(&ctx, 1.0), flt(&ctx, 1.0)],
        ),
        (
            "double plus multi-row ints",
            vec![dbl(&ctx, 1e16)],
            vec![int(&ctx, 1), int(&ctx, 1)],
        ),
        (
            "int plus multi-row floats",
            vec![int(&ctx, 16_777_216)],
            vec![flt(&ctx, 1.0), flt(&ctx, 1.0)],
        ),
        (
            "double plus multi-row float precision",
            vec![dbl(&ctx, 0.0)],
            vec![flt(&ctx, 16_777_216.0), flt(&ctx, 1.0)],
        ),
        (
            "float intermediate infinity",
            vec![flt(&ctx, 3e38)],
            vec![flt(&ctx, 3e38), flt(&ctx, -3e38)],
        ),
        (
            "double nan multi-row",
            vec![dbl(&ctx, f64::NAN)],
            vec![dbl(&ctx, 1.0), dbl(&ctx, 2.0)],
        ),
        (
            "double inf minus inf multi-row",
            vec![dbl(&ctx, f64::INFINITY)],
            vec![dbl(&ctx, f64::NEG_INFINITY), dbl(&ctx, 1.0)],
        ),
    ];

    for func in [AggregateFunction::Sum, AggregateFunction::Avg] {
        let a = agg(func, true, false);
        for (name, left_vals, right_vals) in &cases {
            let (mut left, mut right) = (Running::new(&a, true), Running::new(&a, true));
            left.init(1);
            right.init(1);
            fold(&ctx, &mut left, left_vals, &vec![0; left_vals.len()]);
            fold(&ctx, &mut right, right_vals, &vec![0; right_vals.len()]);
            let err = left
                .merge(&ctx, &mut right, &[0], &mut |_| Ok(()))
                .expect_err(&format!("{name} ({:?}) must refuse merge", a.func));
            assert!(
                matches!(err, crate::error::Error::Unsupported(_)),
                "{name}: expected Unsupported, got {err:?}"
            );
            // Atomic refusal: `left` must still match its pre-merge state.
            let got_left = exact_key(&ctx, left.finish(&ctx, 0).unwrap());
            assert_eq!(got_left, single_pass_result(&ctx, &a, left_vals), "{name}");
        }
    }

    // Multi-group atomic refusal: group 0 is mergeable (Int), group 1 refuses (Double multi-row).
    let sum = agg(AggregateFunction::Sum, true, false);
    let (mut left, mut right) = (Running::new(&sum, true), Running::new(&sum, true));
    left.init(2);
    right.init(2);
    fold(&ctx, &mut left, &[int(&ctx, 10), dbl(&ctx, 1e16)], &[0, 1]);
    fold(
        &ctx,
        &mut right,
        &[int(&ctx, 5), dbl(&ctx, 1.0), dbl(&ctx, 1.0)],
        &[0, 1, 1],
    );
    assert!(matches!(
        left.merge(&ctx, &mut right, &[0, 1], &mut |_| Ok(())),
        Err(crate::error::Error::Unsupported(_))
    ));
    assert_eq!(results(&ctx, &mut left, 2)[0], "10");

    // Single-row right with special IEEE values (-0.0, INF, -INF, NaN) merges bit-identically.
    for (l, r) in [
        (dbl(&ctx, 0.0), dbl(&ctx, -0.0)),
        (dbl(&ctx, -0.0), dbl(&ctx, -0.0)),
        (flt(&ctx, -0.0), flt(&ctx, -0.0)),
        (dbl(&ctx, f64::INFINITY), dbl(&ctx, f64::NEG_INFINITY)),
        (dbl(&ctx, f64::INFINITY), dbl(&ctx, 1.0)),
        (int(&ctx, 5), dbl(&ctx, f64::NAN)),
    ] {
        for func in [AggregateFunction::Sum, AggregateFunction::Avg] {
            let a = agg(func, true, false);
            let (mut left, mut right) = (Running::new(&a, true), Running::new(&a, true));
            left.init(1);
            right.init(1);
            fold(&ctx, &mut left, &[l], &[0]);
            fold(&ctx, &mut right, &[r], &[0]);
            left.merge(&ctx, &mut right, &[0], &mut |_| Ok(())).unwrap();
            let got = exact_key(&ctx, left.finish(&ctx, 0).unwrap());
            assert_eq!(got, single_pass_result(&ctx, &a, &[l, r]));
        }
    }
}

#[test]
fn sum_and_avg_merge_handles_integer_subtotals_beyond_decimal_range() {
    let ctx = ctx();
    let max_i = int(&ctx, i64::MAX);
    let min_i = int(&ctx, i64::MIN);
    let one_d = dbl(&ctx, 1.0);

    for func in [AggregateFunction::Sum, AggregateFunction::Avg] {
        let a = agg(func, true, false);

        // [1.0_f64] + [i64::MAX; 20]: single pass promotes to Double; merge refuses.
        let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
        l.init(1);
        r.init(1);
        fold(&ctx, &mut l, &[one_d], &[0]);
        fold(&ctx, &mut r, &[max_i; 20], &[0; 20]);
        assert!(matches!(
            l.merge(&ctx, &mut r, &[0], &mut |_| Ok(())),
            Err(crate::error::Error::Unsupported(_))
        ));

        // [i64::MAX; 10] + [i64::MAX; 10, 1.0_f64]: multi-row right Double refuses merge.
        let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
        l.init(1);
        r.init(1);
        let mut right_vals = vec![max_i; 10];
        right_vals.push(one_d);
        fold(&ctx, &mut l, &[max_i; 10], &[0; 10]);
        fold(&ctx, &mut r, &right_vals, &vec![0; right_vals.len()]);
        assert!(matches!(
            l.merge(&ctx, &mut r, &[0], &mut |_| Ok(())),
            Err(crate::error::Error::Unsupported(_))
        ));

        // [i64::MAX; 20] + [1.0_f64] (single-row right): merges to finite Double, matching single pass.
        let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
        l.init(1);
        r.init(1);
        let mut all = vec![max_i; 20];
        fold(&ctx, &mut l, &all, &[0; 20]);
        fold(&ctx, &mut r, &[one_d], &[0]);
        l.merge(&ctx, &mut r, &[0], &mut |_| Ok(())).unwrap();
        all.push(one_d);
        let expected = single_pass_result(&ctx, &a, &all);
        assert!(expected.starts_with("double:"), "got {expected}");
        assert_eq!(exact_key(&ctx, l.finish(&ctx, 0).unwrap()), expected);

        // Pure integers [i64::MAX; 20] + [i64::MIN; 20] = -20: merges exactly.
        let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
        l.init(1);
        r.init(1);
        let lv = [max_i; 20];
        let rv = [min_i; 20];
        fold(&ctx, &mut l, &lv, &[0; 20]);
        fold(&ctx, &mut r, &rv, &[0; 20]);
        l.merge(&ctx, &mut r, &[0], &mut |_| Ok(())).unwrap();
        let mut all = lv.to_vec();
        all.extend_from_slice(&rv);
        assert_eq!(
            exact_key(&ctx, l.finish(&ctx, 0).unwrap()),
            single_pass_result(&ctx, &a, &all)
        );
    }
}

#[test]
fn sum_and_avg_merge_matches_single_pass_on_decimal_overflows() {
    let ctx = ctx();
    let scale = super::value::DECIMAL_SCALE;
    let d_max = dec(&ctx, i128::MAX);
    let d_neg_max = dec(&ctx, -i128::MAX);
    let d_near_max = dec(&ctx, i128::MAX - 50 * scale);
    let d_100 = dec_str(&ctx, "100.0");
    let d_neg_100 = dec_str(&ctx, "-100.0");
    let d_half = dec_str(&ctx, "0.5");
    let d_1e19 = dec(&ctx, 10_000_000_000_000_000_000_i128 * scale);
    let d_neg_1e20 = dec(&ctx, -100_000_000_000_000_000_000_i128 * scale);
    let max_i = int(&ctx, i64::MAX);
    let min_i = int(&ctx, i64::MIN);
    let neg_max_i = int(&ctx, -i64::MAX);

    // Cases that still merge exactly within fixed i128 Decimal or via right HardError.
    let exact_merge_cases: Vec<(&str, Vec<Id>, Vec<Id>)> = vec![
        (
            "int plus decimal temporary int excursion returns to range",
            vec![max_i; 10],
            [vec![max_i; 10], vec![neg_max_i; 10], vec![d_half]].concat(),
        ),
        (
            "right wide decimal with UNDEF becomes hard error",
            vec![dbl(&ctx, 1.0)],
            vec![d_max, d_max, Id::UNDEF],
        ),
    ];

    // Cases whose merged subtotal promotes out of fixed i128 Decimal into BigDec (Item 1065)
    // or where right promoted to BigDec: merge refuses cleanly so the caller re-folds sequentially.
    let bigdec_refusal_cases: Vec<(&str, Vec<Id>, Vec<Id>)> = vec![
        (
            "decimal peak promotes to BigDec partway through right",
            vec![d_100],
            vec![d_near_max, d_neg_100],
        ),
        (
            "decimal plus right leading integers promote to BigDec",
            vec![d_1e19],
            [vec![max_i; 18], vec![neg_max_i; 18], vec![d_half]].concat(),
        ),
        (
            "decimal plus pure int excursion promotes to BigDec",
            vec![d_half],
            [vec![max_i; 20], vec![min_i; 20]].concat(),
        ),
        (
            "int plus decimal promotes to BigDec at transition",
            vec![max_i; 10],
            [vec![max_i; 10], vec![d_neg_1e20]].concat(),
        ),
        (
            "canceled wide decimal promotion",
            vec![d_neg_max],
            vec![d_max, d_max],
        ),
        (
            "double avoids wide decimal promotion",
            vec![dbl(&ctx, 1.0)],
            vec![d_max, d_max],
        ),
    ];

    for func in [AggregateFunction::Sum, AggregateFunction::Avg] {
        let a = agg(func, true, false);
        for (name, lv, rv) in &exact_merge_cases {
            let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
            l.init(1);
            r.init(1);
            fold(&ctx, &mut l, lv, &vec![0; lv.len()]);
            fold(&ctx, &mut r, rv, &vec![0; rv.len()]);
            l.merge(&ctx, &mut r, &[0], &mut |_| Ok(()))
                .unwrap_or_else(|e| {
                    panic!("{name} ({:?}): expected exact merge, got {e:?}", a.func)
                });
            let mut all = lv.clone();
            all.extend_from_slice(rv);
            assert_eq!(
                exact_key(&ctx, l.finish(&ctx, 0).unwrap()),
                single_pass_result(&ctx, &a, &all),
                "{name} ({:?})",
                a.func
            );
        }

        for (name, lv, rv) in &bigdec_refusal_cases {
            let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
            l.init(1);
            r.init(1);
            fold(&ctx, &mut l, lv, &vec![0; lv.len()]);
            fold(&ctx, &mut r, rv, &vec![0; rv.len()]);
            assert!(
                matches!(
                    l.merge(&ctx, &mut r, &[0], &mut |_| Ok(())),
                    Err(crate::error::Error::Unsupported(_))
                ),
                "{name} ({:?}) must refuse merge",
                a.func
            );
        }
    }

    // AVG over all-UNDEF respects AggregateErrors readings across merges.
    for reading in [
        super::AggregateErrors::Jena,
        super::AggregateErrors::Sparkles,
        super::AggregateErrors::Spec,
    ] {
        let mut rctx = self::ctx();
        rctx.aggregate_errors = reading;
        let avg = agg(AggregateFunction::Avg, true, false);
        let (mut l, mut r) = (Running::new(&avg, true), Running::new(&avg, true));
        l.init(1);
        r.init(1);
        fold(&rctx, &mut l, &[Id::UNDEF, Id::UNDEF], &[0, 0]);
        fold(&rctx, &mut r, &[Id::UNDEF], &[0]);
        l.merge(&rctx, &mut r, &[0], &mut |_| Ok(())).unwrap();
        assert_eq!(
            exact_key(&rctx, l.finish(&rctx, 0).unwrap()),
            single_pass_result(&rctx, &avg, &[Id::UNDEF, Id::UNDEF, Id::UNDEF]),
            "reading {reading:?}"
        );
    }
}

#[test]
fn sum_and_avg_merge_never_disagrees_with_single_pass_across_all_splits() {
    let ctx = ctx();
    let scale = super::value::DECIMAL_SCALE;
    let d_half_max = dec(&ctx, i128::MAX / 2 + 10 * scale);
    let d_neg_half_max = dec(&ctx, -(i128::MAX / 2 + 10 * scale));
    let str_err = ctx.intern_value(&Value::Str("oops".into()));
    let sequences: Vec<Vec<Id>> = vec![
        vec![
            int(&ctx, 1),
            int(&ctx, -3),
            dec_str(&ctx, "2.5"),
            int(&ctx, 4),
        ],
        vec![
            dec_str(&ctx, "1.25"),
            int(&ctx, 10),
            dec_str(&ctx, "-3.75"),
            int(&ctx, -2),
        ],
        vec![d_half_max, d_half_max, d_neg_half_max, dec_str(&ctx, "1.0")],
        vec![d_neg_half_max, d_half_max, d_half_max, d_neg_half_max],
        vec![
            int(&ctx, i64::MAX),
            int(&ctx, i64::MAX),
            dec_str(&ctx, "0.5"),
            int(&ctx, i64::MIN),
        ],
        vec![
            dbl(&ctx, 1e16),
            int(&ctx, 1),
            int(&ctx, 1),
            dbl(&ctx, -1e16),
        ],
        vec![
            flt(&ctx, 3e38),
            flt(&ctx, 3e38),
            flt(&ctx, -3e38),
            dbl(&ctx, 1.0),
        ],
        vec![int(&ctx, 5), str_err, int(&ctx, 7)],
        vec![int(&ctx, 5), Id::UNDEF, dec_str(&ctx, "1.0")],
        vec![d_half_max, d_half_max, str_err, Id::UNDEF],
    ];

    for func in [AggregateFunction::Sum, AggregateFunction::Avg] {
        let a = agg(func, true, false);
        for seq in &sequences {
            let expected = single_pass_result(&ctx, &a, seq);
            for split in 0..=seq.len() {
                let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
                l.init(1);
                r.init(1);
                fold(&ctx, &mut l, &seq[..split], &vec![0; split]);
                fold(&ctx, &mut r, &seq[split..], &vec![0; seq.len() - split]);
                match l.merge(&ctx, &mut r, &[0], &mut |_| Ok(())) {
                    Ok(()) => {
                        let got = exact_key(&ctx, l.finish(&ctx, 0).unwrap());
                        assert_eq!(
                            got,
                            expected,
                            "{:?} split {split} of len {}",
                            a.func,
                            seq.len()
                        );
                    }
                    Err(crate::error::Error::Unsupported(_)) => {
                        // Refused cleanly: left state must remain intact.
                        let got_left = exact_key(&ctx, l.finish(&ctx, 0).unwrap());
                        assert_eq!(got_left, single_pass_result(&ctx, &a, &seq[..split]));
                    }
                    Err(other) => panic!("unexpected merge error: {other:?}"),
                }
            }
        }
    }
}

/// Exact row-by-row, column-by-column comparison when `got` and `expected` were produced
/// on two separate `Ctx` instances (each decoded against its own local term table).
fn assert_tables_exact_eq_cross_ctx(
    got_ctx: &Ctx,
    got: &Table,
    expected_ctx: &Ctx,
    expected: &Table,
    label: &str,
) {
    assert_eq!(got.vars, expected.vars, "{label}: vars mismatch");
    assert_eq!(got.len, expected.len, "{label}: row count mismatch");
    for row in 0..expected.len {
        for col in 0..expected.cols.len() {
            let g = exact_key(got_ctx, got.cols[col][row]);
            let e = exact_key(expected_ctx, expected.cols[col][row]);
            assert_eq!(g, e, "{label}: mismatch at row {row} col {col}");
        }
    }
}

/// Exact row-by-row, column-by-column comparison of two grouping output tables, including
/// exact row order and bit-level float/double equality.
fn assert_tables_exact_eq(ctx: &Ctx, got: &Table, expected: &Table, label: &str) {
    assert_tables_exact_eq_cross_ctx(ctx, got, ctx, expected, label);
}

fn run_whole_in_pool(
    threads: usize,
    ctx: &Ctx,
    table: &Table,
    keys: &[VarId],
    aggs: &[(VarId, Agg)],
    charged: bool,
) -> crate::error::Result<(Table, super::exprcache::Report)> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    pool.install(|| {
        let mut report = super::exprcache::Report::default();
        let out = super::group::whole(ctx, table, keys, aggs, charged, true, &mut report)?;
        Ok((out, report))
    })
}

#[test]
fn parallel_whole_matches_sequential_ungrouped_edge_cases() {
    let ctx = ctx();
    let n = 20_000usize;
    let mut table = Table::new(vec![0, 1, 2, 3]);
    let d_max = dec(&ctx, i128::MAX);
    let d_neg_max = dec(&ctx, -i128::MAX);
    let d_100 = dec_str(&ctx, "100.0");
    let d_near_max = dec(&ctx, i128::MAX - 50 * super::value::DECIMAL_SCALE);
    let str_a = ctx.intern_value(&Value::Str("a".into()));
    let str_b = ctx.intern_value(&Value::Str("b".into()));

    for i in 0..n {
        let int_v = int(&ctx, (i as i64 % 97) - 40);
        // Non-associative doubles: 1e16 first, then many 1.0s, plus -0.0
        let dbl_v = if i == 0 {
            dbl(&ctx, 1e16)
        } else if i == 1 {
            dbl(&ctx, -0.0)
        } else {
            dbl(&ctx, 1.0)
        };
        // Decimal that overflows mid-pass in right slice if not careful
        let dec_v = match i {
            0 => d_100,
            10_000 => d_near_max,
            10_001 => d_neg_max,
            10_002 => d_max,
            _ => dec_str(&ctx, "0.25"),
        };
        let str_v = if i % 2 == 0 { str_a } else { str_b };
        table.push_row(&[int_v, dbl_v, dec_v, str_v]);
    }

    let aggs = vec![
        (10, agg(AggregateFunction::Count, false, false)),
        (
            11,
            Agg {
                func: AggregateFunction::Sum,
                expr: Some(super::expr::Expr::Var(0)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            12,
            Agg {
                func: AggregateFunction::Avg,
                expr: Some(super::expr::Expr::Var(0)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            13,
            Agg {
                func: AggregateFunction::Sum,
                expr: Some(super::expr::Expr::Var(1)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            14,
            Agg {
                func: AggregateFunction::Avg,
                expr: Some(super::expr::Expr::Var(1)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            15,
            Agg {
                func: AggregateFunction::Sum,
                expr: Some(super::expr::Expr::Var(2)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            16,
            Agg {
                func: AggregateFunction::Min,
                expr: Some(super::expr::Expr::Var(0)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            17,
            Agg {
                func: AggregateFunction::Max,
                expr: Some(super::expr::Expr::Var(0)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            18,
            Agg {
                func: AggregateFunction::Sample,
                expr: Some(super::expr::Expr::Var(3)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            19,
            Agg {
                func: AggregateFunction::GroupConcat {
                    separator: Some(",".into()),
                },
                expr: Some(super::expr::Expr::Var(3)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
    ];

    let (seq, _) = run_whole_in_pool(1, &ctx, &table, &[], &aggs, false).unwrap();
    let (par, _) = run_whole_in_pool(4, &ctx, &table, &[], &aggs, false).unwrap();
    assert_tables_exact_eq(&ctx, &par, &seq, "ungrouped parallel vs sequential");
}

#[test]
fn parallel_whole_matches_sequential_low_cardinality_and_skew_fallback() {
    let ctx = ctx();
    let n = 20_000usize;

    // 1. Low cardinality (8 groups -> hits Range-Split + merge, including float refusal fallback).
    let mut low = Table::new(vec![0, 1, 2]);
    for i in 0..n {
        let k = int(&ctx, ((i * 7 + 3) % 8) as i64);
        let iv = int(&ctx, (i as i64 % 53) - 20);
        let fv = match i % 8 {
            0 if i < 16 => dbl(&ctx, 1e16),
            1 if i == 1 => dbl(&ctx, f64::NAN),
            2 if i == 2 => dbl(&ctx, f64::INFINITY),
            2 if i == 10 => dbl(&ctx, f64::NEG_INFINITY),
            3 if i == 3 => dbl(&ctx, -0.0),
            4 if i == 12_000 => Id::UNDEF,
            _ => dbl(&ctx, 1.0),
        };
        low.push_row(&[k, iv, fv]);
    }
    let aggs = vec![
        (10, agg(AggregateFunction::Count, false, false)),
        (
            11,
            Agg {
                func: AggregateFunction::Sum,
                expr: Some(super::expr::Expr::Var(1)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            12,
            Agg {
                func: AggregateFunction::Avg,
                expr: Some(super::expr::Expr::Var(1)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            13,
            Agg {
                func: AggregateFunction::Sum,
                expr: Some(super::expr::Expr::Var(2)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            14,
            Agg {
                func: AggregateFunction::Avg,
                expr: Some(super::expr::Expr::Var(2)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            15,
            Agg {
                func: AggregateFunction::Sample,
                expr: Some(super::expr::Expr::Var(1)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
    ];
    let (seq, _) = run_whole_in_pool(1, &ctx, &low, &[0], &aggs, false).unwrap();
    let (par, _) = run_whole_in_pool(4, &ctx, &low, &[0], &aggs, false).unwrap();
    assert_tables_exact_eq(&ctx, &par, &seq, "low-cardinality range-split");

    // 2. Adversarial skew: mostly one key, with a burst of 2,500 unique keys inside a
    // single worker's slice (5_000..7_500 inside worker 1's 5_000..10_000 slice for 4
    // threads) so sample_distinct <= 64 enters range-split, worker 1 exceeds
    // RANGE_SPLIT_MAX_GROUPS (2,048), and range-split aborts to key-hash partitioning.
    let mut skewed = Table::new(vec![0, 1]);
    for i in 0..n {
        let k = if (5_000..7_500).contains(&i) {
            int(&ctx, i as i64)
        } else {
            int(&ctx, 0)
        };
        skewed.push_row(&[k, int(&ctx, (i % 19) as i64)]);
    }
    let skew_aggs = vec![
        (10, agg(AggregateFunction::Count, false, false)),
        (
            11,
            Agg {
                func: AggregateFunction::Sum,
                expr: Some(super::expr::Expr::Var(1)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
    ];
    let pool4 = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let hit_cap = pool4
        .install(|| {
            super::group::par::range_split_hits_too_many_groups(&ctx, &skewed, &[0], &skew_aggs, 4)
        })
        .unwrap();
    assert!(
        hit_cap,
        "skewed table must actually hit RANGE_SPLIT_MAX_GROUPS in range-split"
    );
    let (seq, _) = run_whole_in_pool(1, &ctx, &skewed, &[0], &skew_aggs, false).unwrap();
    let (par, _) = run_whole_in_pool(4, &ctx, &skewed, &[0], &skew_aggs, false).unwrap();
    assert_tables_exact_eq(&ctx, &par, &seq, "skew fallback to hash partitioning");
}

#[test]
fn parallel_whole_matches_sequential_high_cardinality_all_running_aggregates() {
    let ctx = ctx();
    let v_k1 = ctx.var("k1");
    let v_k2 = ctx.var("k2");
    let v_val = ctx.var("val");
    let v_txt = ctx.var("txt");
    let v_missing = ctx.var("missing");
    let n = 22_000usize;
    let mut table = Table::new(vec![v_k1, v_k2, v_val, v_txt]);
    let s0 = ctx.intern_value(&Value::Str("alpha".into()));
    let s1 = ctx.intern_value(&Value::Str("beta".into()));
    let s2 = ctx.intern_value(&Value::Str("gamma".into()));

    for i in 0..n {
        let k1 = if i % 503 == 0 {
            Id::UNDEF
        } else {
            int(&ctx, ((i * 131 + 17) % 1_500) as i64)
        };
        let k2 = int(&ctx, ((i * 7) % 3) as i64);
        let val = match i % 11 {
            0 => dbl(&ctx, 1e16),
            1 => dbl(&ctx, 1.0),
            2 => flt(&ctx, 0.5),
            3 => Id::UNDEF,
            _ => int(&ctx, (i as i64 % 37) - 10),
        };
        let txt = match i % 3 {
            0 => s0,
            1 => s1,
            _ => s2,
        };
        table.push_row(&[k1, k2, val, txt]);
    }

    let stdev_iri = oxrdf::NamedNode::new(format!(
        "{}stdev",
        spargebra::algebra::ARQ_AGGREGATE_NAMESPACE
    ))
    .unwrap();
    let expr_arg = super::expr::Expr::Arith(
        Box::new(super::expr::Expr::Var(v_val)),
        Box::new(super::expr::Expr::Const(int(&ctx, 1))),
        super::expr::ArithOp::Add,
    );
    let aggs = vec![
        (10, agg(AggregateFunction::Count, false, false)),
        (11, agg(AggregateFunction::Count, false, true)),
        (
            12,
            Agg {
                func: AggregateFunction::Count,
                expr: Some(super::expr::Expr::Var(v_val)),
                distinct: true,
                fold: None,
                registered: None,
            },
        ),
        (
            13,
            Agg {
                func: AggregateFunction::Sum,
                expr: Some(super::expr::Expr::Var(v_val)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            14,
            Agg {
                func: AggregateFunction::Sum,
                expr: Some(super::expr::Expr::Var(v_val)),
                distinct: true,
                fold: None,
                registered: None,
            },
        ),
        (
            15,
            Agg {
                func: AggregateFunction::Avg,
                expr: Some(expr_arg),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            16,
            Agg {
                func: AggregateFunction::Min,
                expr: Some(super::expr::Expr::Var(v_val)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            17,
            Agg {
                func: AggregateFunction::Max,
                expr: Some(super::expr::Expr::Var(v_val)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            18,
            Agg {
                func: AggregateFunction::Sample,
                expr: Some(super::expr::Expr::Var(v_txt)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            19,
            Agg {
                func: AggregateFunction::GroupConcat {
                    separator: Some("|".into()),
                },
                expr: Some(super::expr::Expr::Var(v_txt)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
        (
            20,
            Agg {
                func: AggregateFunction::GroupConcat {
                    separator: Some(";".into()),
                },
                expr: Some(super::expr::Expr::Var(v_txt)),
                distinct: true,
                fold: None,
                registered: None,
            },
        ),
        (
            21,
            Agg {
                func: AggregateFunction::Custom(stdev_iri),
                expr: Some(super::expr::Expr::Var(v_val)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
    ];

    // Single key (with UNDEF keys), two keys, and three keys including an unbound variable.
    let shared_ctx = ctx.into_shared();
    for keys in [&[v_k1][..], &[v_k1, v_k2][..], &[v_k1, v_k2, v_missing][..]] {
        for charged in [false, true] {
            let (seq, seq_rep) =
                run_whole_in_pool(1, &shared_ctx, &table, keys, &aggs, charged).unwrap();
            let (par, par_rep) =
                run_whole_in_pool(4, &shared_ctx, &table, keys, &aggs, charged).unwrap();
            assert_tables_exact_eq(
                &shared_ctx,
                &par,
                &seq,
                &format!("hash-part keys={keys:?} charged={charged}"),
            );
            assert_eq!(par_rep.note(), seq_rep.note(), "ExprReport must match");
        }
    }
}

#[test]
fn parallel_whole_honors_memory_fallback_row_limit_and_cancellation() {
    let n = 20_000usize;
    let base_ctx = ctx();
    let mut table = Table::new(vec![0, 1]);
    for i in 0..n {
        table.push_row(&[
            int(&base_ctx, (i % 500) as i64),
            int(&base_ctx, (i % 17) as i64),
        ]);
    }
    let aggs = vec![
        (10, agg(AggregateFunction::Count, false, false)),
        (
            11,
            Agg {
                func: AggregateFunction::Sum,
                expr: Some(super::expr::Expr::Var(1)),
                distinct: false,
                fold: None,
                registered: None,
            },
        ),
    ];

    // 1. Tight memory budget refuses parallel radix scratch (8*N = 160 KiB) but allows
    // sequential whole() (4*N = 80 KiB slots + 500 groups): must fall back and succeed!
    let mut tight_ctx = ctx();
    tight_ctx.mem_limit = 140 * 1024;
    let (seq, _) = run_whole_in_pool(1, &base_ctx, &table, &[0], &aggs, false).unwrap();
    let (par_fallback, _) = run_whole_in_pool(4, &tight_ctx, &table, &[0], &aggs, false).unwrap();
    assert_tables_exact_eq(
        &base_ctx,
        &par_fallback,
        &seq,
        "memory fallback to sequential",
    );

    // 2. Row limit must still be enforced.
    let mut row_ctx = ctx();
    row_ctx.max_rows = 100;
    let err = run_whole_in_pool(4, &row_ctx, &table, &[0], &aggs, false).unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Rows
        ),
        "expected BudgetExceeded(Rows), got {err:?}"
    );

    // 3. Cancellation must propagate immediately.
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut cancel_ctx = ctx();
    cancel_ctx.cancel = cancel;
    let err = run_whole_in_pool(4, &cancel_ctx, &table, &[0], &aggs, false).unwrap_err();
    assert!(
        matches!(err, crate::error::Error::Cancelled),
        "expected Cancelled, got {err:?}"
    );
}

#[test]
fn range_split_min_max_matches_sequential_on_non_transitive_mixed_and_incomparable_values() {
    let ctx = ctx();
    // Mixed numeric cycle where order_cmp is non-transitive:
    // a = -200000001 (int) < b = -199999999 (int) by exact integer comparison,
    // while both round to c = -200000000 (float) in f32 so lexical tie-breaking gives b < c < a.
    let a = int(&ctx, -200_000_001);
    let b = int(&ctx, -199_999_999);
    let c = flt(&ctx, -200_000_000.0);

    // Direct merge refusal on mixed exact/float numerics.
    for func in [AggregateFunction::Min, AggregateFunction::Max] {
        let ag = agg(func, true, false);
        let (mut l, mut r) = (Running::new(&ag, false), Running::new(&ag, false));
        l.init(1);
        r.init(1);
        fold(&ctx, &mut l, &[a], &[0]);
        fold(&ctx, &mut r, &[c, b], &[0, 0]);
        assert!(
            matches!(
                l.merge(&ctx, &mut r, &[0], &mut |_| Ok(())),
                Err(crate::error::Error::Unsupported(_))
            ),
            "{:?} must refuse mixed exact/float merge",
            ag.func
        );
        // Refusal is atomic: left and right retain their winners.
        assert_eq!(
            exact_key(&ctx, l.finish(&ctx, 0).unwrap()),
            exact_key(&ctx, a)
        );
        let expected_r = single_pass_result(&ctx, &ag, &[c, b]);
        assert_eq!(exact_key(&ctx, r.finish(&ctx, 0).unwrap()), expected_r);
    }

    // Full parallel range-split with 2 workers on 16,384 ungrouped rows (MIN & MAX cycles).
    let n = 16_384usize;
    let half = n / 2;
    let mut min_table = Table::new(vec![0]);
    let mut max_table = Table::new(vec![0]);
    for i in 0..n {
        let min_v = if i < half {
            a
        } else if i == half {
            c
        } else {
            b
        };
        let max_v = if i < half {
            b
        } else if i == half {
            c
        } else {
            a
        };
        min_table.push_row(&[min_v]);
        max_table.push_row(&[max_v]);
    }
    let min_aggs = vec![(10, agg(AggregateFunction::Min, true, false))];
    let max_aggs = vec![(10, agg(AggregateFunction::Max, true, false))];
    let (seq_min, _) = run_whole_in_pool(1, &ctx, &min_table, &[], &min_aggs, false).unwrap();
    let (par_min, _) = run_whole_in_pool(2, &ctx, &min_table, &[], &min_aggs, false).unwrap();
    assert_tables_exact_eq(&ctx, &par_min, &seq_min, "non-transitive mixed MIN");
    assert_eq!(exact_key(&ctx, par_min.cols[0][0]), exact_key(&ctx, b));

    let (seq_max, _) = run_whole_in_pool(1, &ctx, &max_table, &[], &max_aggs, false).unwrap();
    let (par_max, _) = run_whole_in_pool(2, &ctx, &max_table, &[], &max_aggs, false).unwrap();
    assert_tables_exact_eq(&ctx, &par_max, &seq_max, "non-transitive mixed MAX");
    assert_eq!(exact_key(&ctx, par_max.cols[0][0]), exact_key(&ctx, a));

    // Incomparable DateTime (tz vs no-tz) and Duration (P30D / P0Y31D / P1M) also refuse merge.
    let dt_a = ctx.intern_term(&oxrdf::Term::Literal(oxrdf::Literal::new_typed_literal(
        "2000-01-01T06:00:00+02:00",
        oxrdf::vocab::xsd::DATE_TIME,
    )));
    let dt_b = ctx.intern_term(&oxrdf::Term::Literal(oxrdf::Literal::new_typed_literal(
        "2000-01-01T05:00:00Z",
        oxrdf::vocab::xsd::DATE_TIME,
    )));
    let dt_c = ctx.intern_term(&oxrdf::Term::Literal(oxrdf::Literal::new_typed_literal(
        "2000-01-01T05:30:00",
        oxrdf::vocab::xsd::DATE_TIME,
    )));
    let mut dt_table = Table::new(vec![0]);
    for i in 0..n {
        let v = if i < half {
            dt_b
        } else if i == half {
            dt_a
        } else {
            dt_c
        };
        dt_table.push_row(&[v]);
    }
    let (seq_dt, _) = run_whole_in_pool(1, &ctx, &dt_table, &[], &min_aggs, false).unwrap();
    let (par_dt, _) = run_whole_in_pool(2, &ctx, &dt_table, &[], &min_aggs, false).unwrap();
    assert_tables_exact_eq(&ctx, &par_dt, &seq_dt, "incomparable DateTime MIN");

    // Non-transitive Duration cycle: P30D < P0Y31D < P1M < P30D (P30D and P0Y31D compare
    // by value, while P1M is incomparable with both and falls back to lexical order:
    // "P0Y31D" < "P1M" < "P30D"). Without merge refusal, splitting across 2 workers yields
    // P30D instead of P1M for MIN and P30D instead of P0Y31D for MAX.
    let dur_30d = ctx.intern_term(&oxrdf::Term::Literal(oxrdf::Literal::new_typed_literal(
        "P30D",
        oxrdf::vocab::xsd::DURATION,
    )));
    let dur_31d = ctx.intern_term(&oxrdf::Term::Literal(oxrdf::Literal::new_typed_literal(
        "P0Y31D",
        oxrdf::vocab::xsd::DURATION,
    )));
    let dur_1m = ctx.intern_term(&oxrdf::Term::Literal(oxrdf::Literal::new_typed_literal(
        "P1M",
        oxrdf::vocab::xsd::DURATION,
    )));
    let mut dur_min_table = Table::new(vec![0]);
    let mut dur_max_table = Table::new(vec![0]);
    for i in 0..n {
        let min_v = if i < half {
            dur_30d
        } else if i == half {
            dur_31d
        } else {
            dur_1m
        };
        let max_v = if i < half {
            dur_30d
        } else if i == half {
            dur_1m
        } else {
            dur_31d
        };
        dur_min_table.push_row(&[min_v]);
        dur_max_table.push_row(&[max_v]);
    }
    let (seq_dur_min, _) =
        run_whole_in_pool(1, &ctx, &dur_min_table, &[], &min_aggs, false).unwrap();
    let (par_dur_min, _) =
        run_whole_in_pool(2, &ctx, &dur_min_table, &[], &min_aggs, false).unwrap();
    assert_tables_exact_eq(
        &ctx,
        &par_dur_min,
        &seq_dur_min,
        "non-transitive Duration MIN",
    );
    assert_eq!(
        exact_key(&ctx, par_dur_min.cols[0][0]),
        exact_key(&ctx, dur_1m)
    );

    let (seq_dur_max, _) =
        run_whole_in_pool(1, &ctx, &dur_max_table, &[], &max_aggs, false).unwrap();
    let (par_dur_max, _) =
        run_whole_in_pool(2, &ctx, &dur_max_table, &[], &max_aggs, false).unwrap();
    assert_tables_exact_eq(
        &ctx,
        &par_dur_max,
        &seq_dur_max,
        "non-transitive Duration MAX",
    );
    assert_eq!(
        exact_key(&ctx, par_dur_max.cols[0][0]),
        exact_key(&ctx, dur_31d)
    );

    // Homogeneous exact numerics (Integer + Decimal) and finite Float + Double still merge.
    let ag_min = agg(AggregateFunction::Min, true, false);
    let (mut l, mut r) = (Running::new(&ag_min, false), Running::new(&ag_min, false));
    l.init(1);
    r.init(1);
    fold(
        &ctx,
        &mut l,
        &[int(&ctx, 10), dec_str(&ctx, "5.5")],
        &[0, 0],
    );
    fold(&ctx, &mut r, &[int(&ctx, 3)], &[0]);
    l.merge(&ctx, &mut r, &[0], &mut |_| Ok(())).unwrap();
    assert_eq!(
        exact_key(&ctx, l.finish(&ctx, 0).unwrap()),
        exact_key(&ctx, int(&ctx, 3))
    );
}

#[test]
fn parallel_scratch_releases_before_finish_under_query_and_server_limits() {
    let n = 16_384usize;
    let base_ctx = ctx();
    let v_k = base_ctx.var("k");
    let v_v = base_ctx.var("v");
    let s = base_ctx.intern_value(&Value::Str("abcdefgh".into()));
    let mut table = Table::new(vec![v_k, v_v]);
    for i in 0..n {
        table.push_row(&[int(&base_ctx, (i % 2_000) as i64), s]);
    }

    // 1. Ungrouped GROUP_CONCAT(STR(?v)): expression scratch (~131 KiB) + final result
    // (~148 KiB) exceed 270 KiB when held simultaneously, but fit once scratch drops
    // (including the 64 KiB initial retained-reservation step under a server pool).
    let str_expr = super::expr::Expr::Call(
        super::expr::Func::Builtin(spargebra::algebra::Function::Str),
        vec![super::expr::Expr::Var(v_v)],
    );
    let concat_expr_agg = vec![(
        10,
        Agg {
            func: AggregateFunction::GroupConcat {
                separator: Some(",".into()),
            },
            expr: Some(str_expr),
            distinct: false,
            fold: None,
            registered: None,
        },
    )];
    let (seq_ungrouped, _) =
        run_whole_in_pool(1, &base_ctx, &table, &[], &concat_expr_agg, false).unwrap();

    // Query memory limit
    let mut q_ctx = ctx();
    let qk = q_ctx.var("k");
    let qv = q_ctx.var("v");
    assert_eq!((qk, qv), (v_k, v_v));
    q_ctx.mem_limit = 270_000;
    let s_q = q_ctx.intern_value(&Value::Str("abcdefgh".into()));
    let mut q_table = Table::new(vec![qk, qv]);
    for i in 0..n {
        q_table.push_row(&[int(&q_ctx, (i % 2_000) as i64), s_q]);
    }
    let (par_q, _) = run_whole_in_pool(4, &q_ctx, &q_table, &[], &concat_expr_agg, false).unwrap();
    q_ctx
        .check()
        .expect("no sticky memory_failure under query limit");
    assert_tables_exact_eq_cross_ctx(
        &q_ctx,
        &par_q,
        &base_ctx,
        &seq_ungrouped,
        "ungrouped concat query limit",
    );

    // Server memory pool limit
    let pool = super::pool::QueryPool::new(270_000);
    let grant = std::sync::Arc::new(pool.empty_grant());
    let mut p_ctx = ctx();
    let pk = p_ctx.var("k");
    let pv = p_ctx.var("v");
    assert_eq!((pk, pv), (v_k, v_v));
    p_ctx.set_grant(Some(grant));
    let s_p = p_ctx.intern_value(&Value::Str("abcdefgh".into()));
    let mut p_table = Table::new(vec![pk, pv]);
    for i in 0..n {
        p_table.push_row(&[int(&p_ctx, (i % 2_000) as i64), s_p]);
    }
    let (par_p, _) = run_whole_in_pool(4, &p_ctx, &p_table, &[], &concat_expr_agg, false).unwrap();
    p_ctx
        .check()
        .expect("no sticky memory_failure under server pool limit");
    assert_tables_exact_eq_cross_ctx(
        &p_ctx,
        &par_p,
        &base_ctx,
        &seq_ungrouped,
        "ungrouped concat pool limit",
    );

    // 2. Hash-partitioned GROUP_CONCAT(?k) over 4,000 groups with distinct result strings:
    // ~520 KiB of finalized local terms fit sequentially once slots drop, while holding
    // `_order_charge` (~48 KiB) + output (~64 KiB) + finalized terms exceeds 550 KiB.
    // Hash Pass 3 must refuse BEFORE calling infallible `finish()`, falling back cleanly
    // without latching `ctx.memory_failure` under both query and server pool limits.
    let mut distinct_tbl = Table::new(vec![0]);
    for i in 0..n {
        distinct_tbl.push_row(&[int(&base_ctx, 10_000 + (i % 4_000) as i64)]);
    }
    let concat_key_agg = vec![(
        10,
        Agg {
            func: AggregateFunction::GroupConcat {
                separator: Some("----".into()),
            },
            expr: Some(super::expr::Expr::Var(0)),
            distinct: false,
            fold: None,
            registered: None,
        },
    )];
    let (seq_grouped, _) =
        run_whole_in_pool(1, &base_ctx, &distinct_tbl, &[0], &concat_key_agg, false).unwrap();

    let mut hq_ctx = ctx();
    hq_ctx.mem_limit = 550_000;
    let (par_hq, _) =
        run_whole_in_pool(4, &hq_ctx, &distinct_tbl, &[0], &concat_key_agg, false).unwrap();
    hq_ctx
        .check()
        .expect("hash fallback must not latch query memory_failure");
    assert_tables_exact_eq_cross_ctx(
        &hq_ctx,
        &par_hq,
        &base_ctx,
        &seq_grouped,
        "hash concat query limit",
    );

    let hpool = super::pool::QueryPool::new(550_000);
    let hgrant = std::sync::Arc::new(hpool.empty_grant());
    let mut hp_ctx = ctx();
    hp_ctx.set_grant(Some(hgrant));
    let (par_hp, _) =
        run_whole_in_pool(4, &hp_ctx, &distinct_tbl, &[0], &concat_key_agg, false).unwrap();
    hp_ctx
        .check()
        .expect("hash fallback must not latch server memory_failure");
    assert_tables_exact_eq_cross_ctx(
        &hp_ctx,
        &par_hp,
        &base_ctx,
        &seq_grouped,
        "hash concat pool limit",
    );
}

#[test]
fn hash_path_admits_bucket_capacities_and_combined_assembly_peak() {
    // 1. Skewed radix buckets: 2 workers, 4 partitions, 65,538 rows, 2 keys landing in
    // the same bucket, non-mergeable ARQ STDEV(?v) so the hash path is selected while
    // sequential needs only 262 KiB of slots. Budget 600,000 B is above the old
    // 528,912 B underestimate and below the 622,592 B retained bucket capacity, so
    // parallel radix must refuse admission and fall back cleanly to sequential.
    let base_ctx = ctx();
    let k0 = int(&base_ctx, 0);
    let mut k1 = int(&base_ctx, 1);
    let mix = |mut x: u64| {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    };
    let target_bucket = (mix(k0.0) as usize) & 3;
    for candidate in 1..1000i64 {
        let id = int(&base_ctx, candidate);
        if ((mix(id.0) as usize) & 3) == target_bucket {
            k1 = id;
            break;
        }
    }
    let n = 65_538usize;
    let mut skewed = Table::new(vec![0, 1]);
    for i in 0..n {
        let k = if i < n / 2 { k0 } else { k1 };
        skewed.push_row(&[k, int(&base_ctx, (i % 31) as i64)]);
    }
    let stdev_iri = oxrdf::NamedNode::new(format!(
        "{}stdev",
        spargebra::algebra::ARQ_AGGREGATE_NAMESPACE
    ))
    .unwrap();
    let non_mergeable_aggs = vec![(
        10,
        Agg {
            func: AggregateFunction::Custom(stdev_iri),
            expr: Some(super::expr::Expr::Var(1)),
            distinct: false,
            fold: None,
            registered: None,
        },
    )];
    let (seq, _) =
        run_whole_in_pool(1, &base_ctx, &skewed, &[0], &non_mergeable_aggs, false).unwrap();
    let mut tight = ctx();
    tight.mem_limit = 600_000;
    let (par, _) = run_whole_in_pool(2, &tight, &skewed, &[0], &non_mergeable_aggs, false).unwrap();
    assert_tables_exact_eq_cross_ctx(
        &tight,
        &par,
        &base_ctx,
        &seq,
        "skewed radix bucket admission",
    );
    assert!(tight.mem_peak() <= 600_000);

    // 2. Hash assembly combined peak: 100,000 unique keys, width 2 -> Pass 1 (~1.34 MB)
    // and Pass 2 (~2.63 MB) fit under 2.70 MB, and in Pass 3 `order_map` (1.20 MB) and
    // `output` (1.60 MB) each fit alone, but combined `order_map + output` (2.80 MB)
    // exceeds 2.70 MB and must refuse and fall back cleanly to sequential (~2.00 MB).
    let m = 100_000usize;
    let mut wide = Table::new(vec![0]);
    for i in 0..m {
        wide.push_row(&[int(&base_ctx, i as i64)]);
    }
    let count_aggs = vec![(10, agg(AggregateFunction::Count, false, false))];
    let (seq_w, _) = run_whole_in_pool(1, &base_ctx, &wide, &[0], &count_aggs, false).unwrap();
    let mut asm_ctx = ctx();
    asm_ctx.mem_limit = 2_700_000;
    let (par_w, _) = run_whole_in_pool(2, &asm_ctx, &wide, &[0], &count_aggs, false).unwrap();
    assert_tables_exact_eq_cross_ctx(
        &asm_ctx,
        &par_w,
        &base_ctx,
        &seq_w,
        "hash assembly combined peak",
    );
    // `check_output` records the attempted combined peak (`order_map + output = 2,800,064`)
    // before refusing and triggering the sequential fallback, proving Pass 3 admitted
    // `order_map + output` together rather than checking output before `order_map`.
    assert!(
        asm_ctx.mem_peak() >= 2_800_000,
        "expected combined assembly peak check >= 2,800,000, got {}",
        asm_ctx.mem_peak()
    );
}

#[test]
fn decimal_sum_box_is_charged_in_running_add_and_merge() {
    let ctx = ctx();
    let dec_box = super::exec::DECIMAL_SUM_BYTES;
    assert!(dec_box >= 96, "DecimalSum box size should be tracked");
    let sum_agg = agg(AggregateFunction::Sum, true, false);

    // add_batch reports Box<DecimalSum> growth once per group transitioning to decimal.
    let mut acc = Running::new(&sum_agg, true);
    acc.init(2);
    fold(&ctx, &mut acc, &[int(&ctx, 1), int(&ctx, 2)], &[0, 1]);
    assert_eq!(acc.take_growth(), 0);
    fold(&ctx, &mut acc, &[dec_str(&ctx, "1.5")], &[0]);
    assert_eq!(acc.take_growth(), dec_box);
    fold(&ctx, &mut acc, &[dec_str(&ctx, "2.5")], &[0]);
    assert_eq!(acc.take_growth(), 0);
    fold(&ctx, &mut acc, &[dec_str(&ctx, "3.5")], &[1]);
    assert_eq!(acc.take_growth(), dec_box);

    // merge reports Box<DecimalSum> growth on empty-left adoption and Int->Decimal promotion,
    // and reports 0 when left already owns a DecimalSum box or transitions to HardError.
    let (mut left, mut right) = (Running::new(&sum_agg, true), Running::new(&sum_agg, true));
    left.init(4);
    right.init(4);
    // Group 0: empty + Decimal -> adopts box (+dec_box)
    // Group 1: Int + Decimal -> promotes to Decimal (+dec_box)
    // Group 2: Decimal + Decimal -> reuses left box (+0)
    // Group 3: Int(i64::MAX) + UNDEF -> transitions to HardError (+0)
    fold(
        &ctx,
        &mut left,
        &[int(&ctx, 10), dec_str(&ctx, "1.0"), int(&ctx, i64::MAX)],
        &[1, 2, 3],
    );
    let _ = left.take_growth();
    fold(
        &ctx,
        &mut right,
        &[
            dec_str(&ctx, "1.0"),
            dec_str(&ctx, "2.0"),
            dec_str(&ctx, "3.0"),
            Id::UNDEF,
        ],
        &[0, 1, 2, 3],
    );
    let _ = right.take_growth();
    left.merge(&ctx, &mut right, &[0, 1, 2, 3], &mut |_| Ok(()))
        .unwrap();
    assert_eq!(left.take_growth(), dec_box * 2);

    // Retracted decimal prefix in right side must be refused for (Int, Decimal).
    let (mut l_ret, mut r_ret) = (Running::new(&sum_agg, true), Running::new(&sum_agg, true));
    l_ret.init(1);
    r_ret.init(1);
    fold(&ctx, &mut l_ret, &[int(&ctx, 5)], &[0]);
    fold(&ctx, &mut r_ret, &[int(&ctx, 10)], &[0]);
    let t = Table::new(vec![0]);
    let ret_vals = [int(&ctx, 10)];
    let batch = Batch {
        table: &t,
        arg: Arg::Column(&ret_vals),
        rows: None,
        weights: None,
        base: 0,
    };
    assert!(r_ret.retract(&ctx, &batch, &[0]).unwrap());
    fold(&ctx, &mut r_ret, &[dec_str(&ctx, "1.5")], &[0]);
    assert!(matches!(
        l_ret.merge(&ctx, &mut r_ret, &[0], &mut |_| Ok(())),
        Err(crate::error::Error::Unsupported(_))
    ));
}

#[test]
fn merge_refuses_repeated_and_out_of_bounds_destinations_atomically() {
    let ctx = ctx();
    let sum_agg = agg(AggregateFunction::Sum, true, false);
    let (mut left, mut right) = (Running::new(&sum_agg, false), Running::new(&sum_agg, false));
    left.init(1);
    right.init(2);
    fold(
        &ctx,
        &mut right,
        &[dbl(&ctx, 1e16), dbl(&ctx, 1.0), dbl(&ctx, 1.0)],
        &[0, 1, 1],
    );
    // Repeated destination [0, 0] must refuse before taking/mutating either side.
    assert!(matches!(
        left.merge(&ctx, &mut right, &[0, 0], &mut |_| Ok(())),
        Err(crate::error::Error::Unsupported(_))
    ));
    assert_eq!(results(&ctx, &mut left, 1), vec!["0"]);
    assert_eq!(
        exact_key(&ctx, right.finish(&ctx, 0).unwrap()),
        exact_key(&ctx, dbl(&ctx, 1e16))
    );
    assert_eq!(
        exact_key(&ctx, right.finish(&ctx, 1).unwrap()),
        exact_key(&ctx, dbl(&ctx, 2.0))
    );

    // Out-of-bounds destination on Running and repeated destination on Concat.
    let (mut l2, mut r2) = (Running::new(&sum_agg, false), Running::new(&sum_agg, false));
    l2.init(1);
    r2.init(1);
    fold(&ctx, &mut r2, &[int(&ctx, 7)], &[0]);
    assert!(matches!(
        l2.merge(&ctx, &mut r2, &[1], &mut |_| Ok(())),
        Err(crate::error::Error::Unsupported(_))
    ));
    assert_eq!(results(&ctx, &mut r2, 1), vec!["7"]);

    let concat_agg = agg(
        AggregateFunction::GroupConcat {
            separator: Some(",".into()),
        },
        true,
        false,
    );
    let (mut cl, mut cr) = (Concat::new(&concat_agg), Concat::new(&concat_agg));
    cl.init(1);
    cr.init(2);
    let sa = ctx.intern_value(&Value::Str("a".into()));
    let sb = ctx.intern_value(&Value::Str("b".into()));
    fold(&ctx, &mut cr, &[sa, sb], &[0, 1]);
    assert!(matches!(
        cl.merge(&ctx, &mut cr, &[0, 0], &mut |_| Ok(())),
        Err(crate::error::Error::Unsupported(_))
    ));
    assert_eq!(results(&ctx, &mut cr, 2), vec!["a", "b"]);
}

#[test]
fn hash_assembly_sort_and_key_only_group_poll_cancellation_and_timeout() {
    // 1. Direct verification of cancellable in-place sort_order_map correctness, mid-sort
    // polling, and immediate cancellation.
    let ctx_ok = ctx();
    let mut map: Vec<(u32, u16, u32)> = (0..25_000u32)
        .map(|i| ((i.wrapping_mul(2654435761)) % 100_003, (i % 16) as u16, i))
        .collect();
    // Deduplicate first_row keys while keeping > 16,384 unique entries.
    let mut seen = rustc_hash::FxHashSet::default();
    map.retain(|&(r, _, _)| seen.insert(r));
    let mut expected = map.clone();
    expected.sort_unstable_by_key(|&(r, _, _)| r);
    let max_row = map.iter().map(|&(r, _, _)| r).max().unwrap_or(0);
    let sort_checks = {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.install(|| {
            let before = super::ctx::CHECKS.with(std::cell::Cell::get);
            super::group::par::sort_order_map(&ctx_ok, &mut map, max_row).unwrap();
            super::ctx::CHECKS.with(std::cell::Cell::get) - before
        })
    };
    assert_eq!(map, expected);
    assert!(
        sort_checks >= 10,
        "expected mid-sort polling across partitions and leaves, got {sort_checks} checks"
    );

    // Cancelled context aborts sort_order_map immediately.
    let mut cancel_ctx = ctx();
    cancel_ctx.cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let err = super::group::par::sort_order_map(&cancel_ctx, &mut map, max_row).unwrap_err();
    assert!(matches!(err, crate::error::Error::Cancelled));

    // 2. `finish_intern_bytes` scans poll in bounded chunks and abort on cancellation.
    let groups = 20_000usize;
    let avg_agg = agg(AggregateFunction::Avg, true, false);
    let mut run_acc = Running::new(&avg_agg, false);
    run_acc.init(groups);
    let before = super::ctx::CHECKS.with(std::cell::Cell::get);
    let _ = run_acc.finish_intern_bytes(&ctx_ok).unwrap();
    let run_checks = super::ctx::CHECKS.with(std::cell::Cell::get) - before;
    assert!(
        run_checks >= groups / 1024,
        "expected Running::finish_intern_bytes to poll in chunks, got {run_checks}"
    );
    assert!(matches!(
        run_acc.finish_intern_bytes(&cancel_ctx),
        Err(crate::error::Error::Cancelled)
    ));

    let concat_agg = agg(
        AggregateFunction::GroupConcat {
            separator: Some(",".into()),
        },
        true,
        false,
    );
    let mut concat_acc = Concat::new(&concat_agg);
    concat_acc.init(groups);
    let before = super::ctx::CHECKS.with(std::cell::Cell::get);
    let _ = concat_acc.finish_intern_bytes(&ctx_ok).unwrap();
    let concat_checks = super::ctx::CHECKS.with(std::cell::Cell::get) - before;
    assert!(
        concat_checks >= groups / 1024,
        "expected Concat::finish_intern_bytes to poll in chunks, got {concat_checks}"
    );
    assert!(matches!(
        concat_acc.finish_intern_bytes(&cancel_ctx),
        Err(crate::error::Error::Cancelled)
    ));

    // 3. Key-only GROUP BY and AVG hash assembly poll proportionally in Pass 3 and respect
    // expired deadlines.
    let n = 20_000usize;
    let mut table = Table::new(vec![0, 1]);
    for i in 0..n {
        table.push_row(&[int(&ctx_ok, i as i64), int(&ctx_ok, (i % 17) as i64)]);
    }
    let (seq, _) = run_whole_in_pool(1, &ctx_ok, &table, &[0], &[], false).unwrap();
    let (par, _) = run_whole_in_pool(4, &ctx_ok, &table, &[0], &[], false).unwrap();
    assert_tables_exact_eq(&ctx_ok, &par, &seq, "key-only parallel GROUP BY");

    let avg_aggs = vec![(10, avg_agg)];
    let par_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let asm_checks = par_pool.install(|| {
        let before = super::ctx::CHECKS.with(std::cell::Cell::get);
        let mut rep = super::exprcache::Report::default();
        let _ =
            super::group::whole(&ctx_ok, &table, &[0], &avg_aggs, false, true, &mut rep).unwrap();
        super::ctx::CHECKS.with(std::cell::Cell::get) - before
    });
    // Coordinator thread runs Pass 3 (finish_intern_bytes + order_map build + gather + finish).
    assert!(
        asm_checks >= (n / 1024) * 3,
        "expected Pass 3 coordinator polling across estimate, build, gather, and finish, got {asm_checks}"
    );

    let mut timeout_ctx = ctx();
    timeout_ctx.deadline = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    let err = run_whole_in_pool(4, &timeout_ctx, &table, &[0], &[], false).unwrap_err();
    assert!(
        matches!(err, crate::error::Error::Timeout),
        "expected Timeout, got {err:?}"
    );
}

#[test]
fn parallel_expr_cache_report_matches_sequential_at_threshold_boundary() {
    let n = super::exec::PAR_THRESHOLD;
    let base_ctx = ctx();
    let v_id = base_ctx.var("v");
    let mut table = Table::new(vec![v_id]);
    let v0 = int(&base_ctx, 42);
    for _ in 0..n {
        table.push_row(&[v0]);
    }
    let expr = super::expr::Expr::Arith(
        Box::new(super::expr::Expr::Var(v_id)),
        Box::new(super::expr::Expr::Const(int(&base_ctx, 1))),
        super::expr::ArithOp::Add,
    );
    let aggs = vec![(
        10,
        Agg {
            func: AggregateFunction::Sum,
            expr: Some(expr),
            distinct: false,
            fold: None,
            registered: None,
        },
    )];

    // 1. Uncharged (`charged = false`): enough headroom for expr_scratch + exprcache
    // (128 * n + 1024), but less than expr_scratch + exprcache + sequential slot charge.
    let expr_scratch = n as u64 * 8 + 64;
    let cache_res = n as u64 * 128 + 1024;
    let slots = n as u64 * 4 + 64;
    let limit = expr_scratch + cache_res + slots / 2;

    let mut seq_ctx = ctx();
    assert_eq!(seq_ctx.var("v"), v_id);
    seq_ctx.mem_limit = limit;
    let mut par_ctx = ctx();
    assert_eq!(par_ctx.var("v"), v_id);
    par_ctx.mem_limit = limit;

    let (seq, seq_rep) = run_whole_in_pool(1, &seq_ctx, &table, &[], &aggs, false).unwrap();
    let (par, par_rep) = run_whole_in_pool(4, &par_ctx, &table, &[], &aggs, false).unwrap();
    assert_tables_exact_eq_cross_ctx(
        &par_ctx,
        &par,
        &seq_ctx,
        &seq,
        "expr-cache threshold parity",
    );
    assert_eq!(seq_rep.note(), None, "sequential declines cache admission");
    assert_eq!(
        par_rep.note(),
        seq_rep.note(),
        "parallel and sequential ExprReport must match under tight exprcache headroom"
    );

    // With room for slots + expr_scratch + cache_res, both strategies admit reuse.
    let mut seq_ok = ctx();
    assert_eq!(seq_ok.var("v"), v_id);
    seq_ok.mem_limit = limit + slots;
    let mut par_ok = ctx();
    assert_eq!(par_ok.var("v"), v_id);
    par_ok.mem_limit = limit + slots;
    let (_, seq_rep_ok) = run_whole_in_pool(1, &seq_ok, &table, &[], &aggs, false).unwrap();
    let (_, par_rep_ok) = run_whole_in_pool(4, &par_ok, &table, &[], &aggs, false).unwrap();
    assert!(seq_rep_ok.note().is_some());
    assert_eq!(par_rep_ok.note(), seq_rep_ok.note());

    // 2. Charged (`charged = true` on shared `Ctx`): at exact `slots + expr_scratch + cache_res`
    // (2,294,912 B for n = 16,384), sequential Grouping::new's retained base + ungrouped group
    // state pushes cache admission past the limit, so both sequential and parallel must decline.
    let exact_limit = slots + expr_scratch + cache_res;
    assert_eq!(exact_limit, 2_294_912);
    let mut seq_charged = ctx();
    assert_eq!(seq_charged.var("v"), v_id);
    seq_charged.mem_limit = exact_limit;
    let seq_charged = seq_charged.into_shared();
    let mut par_charged = ctx();
    assert_eq!(par_charged.var("v"), v_id);
    par_charged.mem_limit = exact_limit;
    let par_charged = par_charged.into_shared();

    let (seq_c, seq_rep_c) = run_whole_in_pool(1, &seq_charged, &table, &[], &aggs, true).unwrap();
    let (par_c, par_rep_c) = run_whole_in_pool(4, &par_charged, &table, &[], &aggs, true).unwrap();
    assert_tables_exact_eq_cross_ctx(
        &par_charged,
        &par_c,
        &seq_charged,
        &seq_c,
        "charged expr-cache threshold parity",
    );
    assert_eq!(
        seq_rep_c.note(),
        None,
        "charged sequential declines cache admission at exact_limit"
    );
    assert_eq!(
        par_rep_c.note(),
        seq_rep_c.note(),
        "charged parallel and sequential ExprReport must match"
    );

    // With headroom for the retained base + first-group step, both admit reuse.
    let mut seq_charged_ok = ctx();
    assert_eq!(seq_charged_ok.var("v"), v_id);
    seq_charged_ok.mem_limit = exact_limit + 100_000;
    let seq_charged_ok = seq_charged_ok.into_shared();
    let mut par_charged_ok = ctx();
    assert_eq!(par_charged_ok.var("v"), v_id);
    par_charged_ok.mem_limit = exact_limit + 100_000;
    let par_charged_ok = par_charged_ok.into_shared();
    let (_, seq_rep_cok) = run_whole_in_pool(1, &seq_charged_ok, &table, &[], &aggs, true).unwrap();
    let (_, par_rep_cok) = run_whole_in_pool(4, &par_charged_ok, &table, &[], &aggs, true).unwrap();
    assert!(seq_rep_cok.note().is_some());
    assert_eq!(par_rep_cok.note(), seq_rep_cok.note());
}

#[test]
fn parallel_range_split_row_limits_and_charged_fallback() {
    let n = 16_384usize;
    let base_ctx = ctx();
    let mut table = Table::new(vec![0, 1]);
    for i in 0..n {
        table.push_row(&[int(&base_ctx, (i % 20) as i64), int(&base_ctx, 1)]);
    }
    let aggs = vec![(
        10,
        Agg {
            func: AggregateFunction::Sum,
            expr: Some(super::expr::Expr::Var(1)),
            distinct: false,
            fold: None,
            registered: None,
        },
    )];

    // 1. max_rows enforced in range-split Pass 1.
    let mut rows_ctx = ctx();
    rows_ctx.max_rows = 5;
    let err = run_whole_in_pool(4, &rows_ctx, &table, &[0], &aggs, false).unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Rows
        ),
        "expected BudgetKind::Rows, got {err:?}"
    );

    // 2. max_rows_produced enforced in range-split Pass 1.
    let mut prod_ctx = ctx();
    prod_ctx.max_rows_produced = 5;
    let err = run_whole_in_pool(4, &prod_ctx, &table, &[0], &aggs, false).unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::RowsProduced
        ),
        "expected BudgetKind::RowsProduced, got {err:?}"
    );

    // 3. charged = true (`into_shared()`) fallback under tight memory succeeds when
    // sequential fits, and returns unlatched BudgetKind::Memory when even sequential
    // does not fit (so group_or_spill can partition to disk).
    let mut high_table = Table::new(vec![0, 1]);
    for i in 0..n {
        high_table.push_row(&[int(&base_ctx, (i % 400) as i64), int(&base_ctx, 1)]);
    }
    let (seq, _) = run_whole_in_pool(1, &base_ctx, &high_table, &[0], &aggs, false).unwrap();

    let mut fit_ctx = ctx();
    fit_ctx.mem_limit = 220_000;
    let fit_shared = fit_ctx.into_shared();
    let (par_fit, _) = run_whole_in_pool(4, &fit_shared, &high_table, &[0], &aggs, true).unwrap();
    fit_shared
        .check()
        .expect("charged fallback must not latch memory_failure");
    assert_tables_exact_eq_cross_ctx(
        &fit_shared,
        &par_fit,
        &base_ctx,
        &seq,
        "charged=true fallback",
    );

    let mut spill_ctx = ctx();
    spill_ctx.mem_limit = 32_000;
    let spill_shared = spill_ctx.into_shared();
    let err = run_whole_in_pool(4, &spill_shared, &high_table, &[0], &aggs, true).unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
        ),
        "expected BudgetKind::Memory for spill handoff, got {err:?}"
    );
    spill_shared
        .check()
        .expect("refused charged grouping must not latch sticky memory_failure");

    // 4. Precomputed sequential fallback enforces `max_rows_produced` during `slots()` /
    // `insert()` rather than deferring through the fold or hitting retained memory first.
    let mut fb_ctx = ctx();
    let vk = fb_ctx.var("k");
    let vv = fb_ctx.var("v");
    // 16,384 unique keys with SUM(?v + 1): expr_scratch (131 KiB) + eval_slots (65 KiB)
    // + eval_group (67 KiB) fits under 320 KiB, hash radix admission (>260 KiB on top of
    // expr_scratch) refuses and falls back to `whole_sequential_with_computed`, and
    // retaining all 16,384 groups would exceed 320 KiB — yet `max_rows_produced = 10`
    // must fail first inside `insert()`.
    fb_ctx.mem_limit = 320_000;
    fb_ctx.max_rows_produced = 10;
    let fb_shared = fb_ctx.into_shared();
    let mut uniq_table = Table::new(vec![vk, vv]);
    for i in 0..n {
        uniq_table.push_row(&[int(&fb_shared, i as i64), int(&fb_shared, 1)]);
    }
    let expr_aggs = vec![(
        10,
        Agg {
            func: AggregateFunction::Sum,
            expr: Some(super::expr::Expr::Arith(
                Box::new(super::expr::Expr::Var(vv)),
                Box::new(super::expr::Expr::Const(int(&fb_shared, 1))),
                super::expr::ArithOp::Add,
            )),
            distinct: false,
            fold: None,
            registered: None,
        },
    )];
    let err = run_whole_in_pool(4, &fb_shared, &uniq_table, &[vk], &expr_aggs, true).unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::RowsProduced
        ),
        "expected BudgetKind::RowsProduced before fold/memory refusal, got {err:?}"
    );
}

#[test]
fn non_inline_double_finalization_admission_and_fallible_interning() {
    // 1. Verify `double_intern_bytes` upper-bounds actual `intern_key` charges across all
    // IEEE-754 categories, including tiny subnormals and ~1e-250 / ~1e-308 fixed-point forms.
    let samples = [
        0.0_f64,
        -0.0_f64,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::from_bits(1),
        f64::from_bits(0x8000_0000_0000_0001),
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::from_bits(f64::MIN_POSITIVE.to_bits() | 1),
        -f64::from_bits(f64::MIN_POSITIVE.to_bits() | 1),
        f64::from_bits((1e-250_f64).to_bits() | 1),
        -f64::from_bits((1e-250_f64).to_bits() | 1),
        f64::MAX,
        -f64::MAX,
        1.0 + f64::EPSILON,
        -(1.0 + f64::EPSILON),
        1.1,
        -0.999_999_999_999_999_9,
    ];
    for x in samples {
        let bound = super::exec::double_intern_bytes(x);
        let term = Value::Double(x.into()).to_term();
        let actual = if crate::id::inline_id(&term).is_some() {
            0
        } else {
            crate::id::term_key(&term).len() as u64 + 96
        };
        assert!(
            bound >= actual,
            "double_intern_bytes({x:?}) = {bound} < actual {actual}"
        );
    }

    // 2. Hash-partitioned AVG(?v) over 8,192 keys × 2 rows (one distinct tiny ~1e-250 double
    // with nonzero low mantissa bit + one 0.0 double, so every finalized AVG is a fresh
    // non-inline double with >250-byte lexical form and ~400-byte intern charge).
    let keys_n = 8_192usize;
    let tiny_base = (1e-250_f64).to_bits();
    let intern_cost = |x: f64| -> u64 {
        let term = Value::Double(x.into()).to_term();
        if crate::id::inline_id(&term).is_some() {
            0
        } else {
            crate::id::term_key(&term).len() as u64 + 96
        }
    };
    let mut input_retained = 0u64;
    let mut output_retained = 0u64;
    for k in 0..keys_n {
        let x = f64::from_bits(tiny_base + ((k as u64) << 4) + 1);
        input_retained += intern_cost(x);
        output_retained += intern_cost(x / 2.0);
    }
    assert!(
        output_retained > keys_n as u64 * 350,
        "expected >350 B per distinct tiny double, got {output_retained}"
    );
    let build_tiny_table = |c: &Ctx| {
        let zero = dbl(c, 0.0);
        let mut t = Table::new(vec![0, 1]);
        for k in 0..keys_n {
            let x = f64::from_bits(tiny_base + ((k as u64) << 4) + 1);
            let k_id = int(c, k as i64);
            t.push_row(&[k_id, dbl(c, x)]);
            t.push_row(&[k_id, zero]);
        }
        t
    };
    let avg_aggs = vec![(
        10,
        Agg {
            func: AggregateFunction::Avg,
            expr: Some(super::expr::Expr::Var(1)),
            distinct: false,
            fold: None,
            registered: None,
        },
    )];

    // Measure sequential's exact peak requirement (including decoded ValueCache and
    // retained-term growth) with a bounded budget so `reservation_step()` matches
    // `EAGER_STEP` identically across sequential and parallel runs.
    let mut seq_ctx = ctx();
    seq_ctx.mem_limit = input_retained + output_retained + 2_000_000;
    let seq_table = build_tiny_table(&seq_ctx);
    let (seq_out, _) = run_whole_in_pool(1, &seq_ctx, &seq_table, &[0], &avg_aggs, false).unwrap();
    assert_eq!(
        seq_out.cols[1][0].tag(),
        crate::id::Tag::Local,
        "AVG of tiny non-inline double must produce a query-local term"
    );
    let seq_peak = seq_ctx.mem_peak();
    assert!(
        seq_ctx.live_bytes() >= input_retained + output_retained,
        "expected retained bytes >= {}, got {}",
        input_retained + output_retained,
        seq_ctx.live_bytes()
    );

    // Budget strictly above sequential's peak and strictly below sequential's retained
    // requirement plus the 98,368-byte order_map: hash Pass 3 probe must refuse BEFORE
    // committing (with the old 256 B/double underestimate it passed and then latched
    // `memory_failure` during finish) and fall back cleanly to sequential.
    let budget = seq_peak + 40_000;

    let mut hq_ctx = ctx();
    hq_ctx.mem_limit = budget;
    let hq_table = build_tiny_table(&hq_ctx);
    let (par_hq, _) = run_whole_in_pool(4, &hq_ctx, &hq_table, &[0], &avg_aggs, false).unwrap();
    hq_ctx
        .check()
        .expect("tiny-double hash fallback must not latch query memory_failure");
    assert_tables_exact_eq_cross_ctx(
        &hq_ctx,
        &par_hq,
        &seq_ctx,
        &seq_out,
        "tiny-double AVG hash query budget boundary",
    );

    // Same boundary under a server QueryPool grant.
    let pool = super::pool::QueryPool::new(budget);
    let grant = std::sync::Arc::new(pool.empty_grant());
    let mut hp_ctx = ctx();
    hp_ctx.mem_limit = budget;
    hp_ctx.set_grant(Some(grant));
    let hp_table = build_tiny_table(&hp_ctx);
    let (par_hp, _) = run_whole_in_pool(4, &hp_ctx, &hp_table, &[0], &avg_aggs, false).unwrap();
    hp_ctx
        .check()
        .expect("tiny-double hash fallback must not latch server memory_failure");
    assert_tables_exact_eq_cross_ctx(
        &hp_ctx,
        &par_hp,
        &seq_ctx,
        &seq_out,
        "tiny-double AVG hash pool budget boundary",
    );

    // 3. Range-split finalization refusal under tight memory must return an unlatched
    // BudgetKind::Memory error (leaving `ctx.check()` clean for spill retry).
    let range_keys = 256usize;
    let range_rows = 16_384usize;
    let r_input_retained: u64 = (0..range_keys)
        .map(|k| intern_cost(f64::from_bits(tiny_base + ((k as u64) << 4) + 1)))
        .sum();
    let mut r_ctx = ctx();
    // 2 workers × (8,192 * 4 + 64 = 32,832 B) = 65,664 B + ~15 KiB ValueCache fits in
    // +95,000 B headroom, while interning 256 distinct halved tiny doubles (~104 KiB)
    // exceeds it during finish().
    r_ctx.mem_limit = r_input_retained + 95_000;
    let zero = dbl(&r_ctx, 0.0);
    let mut r_table = Table::new(vec![0, 1]);
    for i in 0..range_rows {
        let k = i % range_keys;
        let v = if i < range_keys {
            dbl(&r_ctx, f64::from_bits(tiny_base + ((k as u64) << 4) + 1))
        } else {
            zero
        };
        r_table.push_row(&[int(&r_ctx, k as i64), v]);
    }
    let r_before = r_ctx.live_bytes();
    let err = run_whole_in_pool(2, &r_ctx, &r_table, &[0], &avg_aggs, false).unwrap_err();
    assert!(
        matches!(
            err,
            crate::error::Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory
        ),
        "expected BudgetKind::Memory during range-split double finalization, got {err:?}"
    );
    assert!(
        r_ctx.live_bytes() > r_before,
        "expected refusal to occur during finish() interning after Pass 1 succeeded"
    );
    r_ctx
        .check()
        .expect("range-split finalization refusal must not latch sticky memory_failure");
}
