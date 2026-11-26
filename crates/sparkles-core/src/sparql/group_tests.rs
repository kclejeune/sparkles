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

        // Mixed DOUBLE + overflowing DECIMAL + overflowing repeated FLOAT (2 * 2e38f > f32::MAX):
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
    acc.add_batch(&tight, &batch, &[0]).unwrap();
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
    acc_ok.add_batch(&ok, &rep_batch, &[0]).unwrap();
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
    acc_cancel.add_batch(&cancelled, &big_batch, &[0]).unwrap();
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
    acc_empty.add_batch(&ok, &empty_batch, &[0]).unwrap();
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
    acc_existing
        .add_batch(
            &existing_ctx,
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
    acc_new
        .add_batch(
            &new_ctx,
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
    acc_overflow
        .add_batch(
            &permissive,
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
        acc.add_batch(&ctx, &add_batch, &slots).unwrap();
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
        let err = acc.add_batch(&ctx, &add_batch, &slots).unwrap_err();
        assert!(matches!(err, crate::Error::Cancelled), "got {err:?}");
        let err = acc.retract(&ctx, &ret_batch, &slots).unwrap_err();
        assert!(matches!(err, crate::Error::Cancelled), "got {err:?}");
        ctx.cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}
