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
    let slots = vec![0, 0, 0, 1, 1, 1];
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
