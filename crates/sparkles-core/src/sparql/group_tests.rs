//! The accumulators of the grouping kernel: folding the rows of one pass in two parts and
//! merging them gives the answer of one pass over all the rows, DISTINCT passes each
//! value of a group once, and a retracted row leaves the state it had before it was
//! added.

use super::group::acc::{Accumulator, Arg, Batch, Concat, Distinct, Running};
use super::plan::Agg;
use super::table::Table;
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
fn fold(ctx: &Ctx, acc: &mut dyn Accumulator, values: &[Id], slots: &[u32]) {
    let table = Table::new(vec![0]);
    let batch = Batch {
        table: &table,
        arg: Arg::Column(values),
        rows: None,
        base: 0,
    };
    let mut noop = |_| Ok(());
    let mut budget = super::group::op::BatchBudget::Retain(&mut noop);
    acc.add_batch(ctx, &batch, slots, &mut budget).unwrap();
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
    left.merge(&ctx, &mut right, &[0]).unwrap();
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
