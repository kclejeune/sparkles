//! The accumulators of the grouping kernel: folding the rows of one pass in two parts and
//! merging them gives the answer of one pass over all the rows, DISTINCT passes each
//! value of a group once, and a retracted row leaves the state it had before it was
//! added.

use super::group::acc::{Accumulator, Arg, Batch, Concat, Distinct, Running, retractable};
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
        weights: None,
        base: 0,
    };
    acc.add_batch(ctx, &batch, slots).unwrap();
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
            left.merge(&ctx, &mut right, &[0, 1, 2]).unwrap();
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
    a.merge(&ctx, &mut b, &[1, 2]).unwrap();
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
    a.merge(&ctx, &mut b, &[0]).unwrap();
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
    a.merge(&ctx, &mut b, &[0, 1]).unwrap();
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
    count.add_batch(&ctx, &batch, &[0, 0, 0, 0]).unwrap();
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
        acc.add_batch(&ctx, &add_batch, &add_slots).unwrap();
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

        // 0.0e0 followed by two 1e20 decimals: subtotal 2e20 exceeds xsd:decimal,
        // but enters double arithmetic and stays finite in both Running and RetractableSum.
        let vals = [zero_d, big_dec, big_dec];
        let slots = [0u32, 0, 0];
        fold(&ctx, &mut running, &vals, &slots);
        fold(&ctx, &mut *ret, &vals, &slots);
        let got_run = running.finish(&ctx, 0).unwrap();
        let got_ret = ret.read(&ctx, 0).unwrap();
        assert!(!got_ret.is_undef(), "{func:?} should stay finite");
        assert_eq!(ctx.term(got_ret), ctx.term(got_run), "{func:?}");

        // Retracting 0.0e0 leaves pure overflowing decimals -> UNDEF.
        let retract_d = Batch {
            table: &table,
            arg: Arg::Column(&[zero_d]),
            rows: None,
            weights: Some(&[-1]),
            base: 0,
        };
        assert!(ret.retract(&ctx, &retract_d, &[0]).unwrap());
        assert!(ret.read(&ctx, 0).unwrap().is_undef(), "{func:?} overflows");

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

        // Float promotion on overflowing decimal subtotal.
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

        // Integer subtotal overflowing both i64 and xsd:decimal (20 * i64::MAX > 1.7e20)
        // stays finite when a double is present and turns UNDEF when retracted.
        let mut ret_i = retractable(&a, 1).unwrap();
        ret_i.init(1);
        let add_i = Batch {
            table: &table,
            arg: Arg::Column(&[max_i, zero_d]),
            rows: None,
            weights: Some(&[20, 1]),
            base: 0,
        };
        ret_i.add_batch(&ctx, &add_i, &[0, 0]).unwrap();
        let got_i = ret_i.read(&ctx, 0).unwrap();
        assert!(
            matches!(ctx.value(got_i), Some(Value::Double(d)) if f64::from(d).is_finite()),
            "{func:?} large int + double: {:?}",
            ctx.value(got_i)
        );
        assert!(ret_i.retract(&ctx, &retract_d, &[0]).unwrap());
        assert!(ret_i.read(&ctx, 0).unwrap().is_undef());
    }
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
        acc.add_batch(&ctx, &add, &[0]).unwrap();
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
    count.add_batch(&ctx, &add, &[0]).unwrap();
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
    min_acc.add_batch(&ctx, &batch, &[0]).unwrap();
    assert!(min_acc.take_growth() >= 2048);
    assert_eq!(min_acc.read(&ctx, 0).unwrap(), big_str);

    // Weighted SAMPLE completes in O(1).
    let mut sample_acc = Running::new(&agg(AggregateFunction::Sample, true, false), false);
    sample_acc.init(1);
    sample_acc.add_batch(&ctx, &batch, &[0]).unwrap();
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
    let err = sum_acc.add_batch(&ctx, &sum_batch, &[0]).unwrap_err();
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
