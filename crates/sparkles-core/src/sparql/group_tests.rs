//! The accumulators of the grouping kernel: folding the rows of one pass in two parts and
//! merging them gives the answer of one pass over all the rows, DISTINCT passes each
//! value of a group once, and a retracted row leaves the state it had before it was
//! added.

use super::group::acc::{Accumulator, Arg, Batch, Concat, Distinct, Running};
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
                .merge(&ctx, &mut right, &[0])
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
        left.merge(&ctx, &mut right, &[0, 1]),
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
            left.merge(&ctx, &mut right, &[0]).unwrap();
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
            l.merge(&ctx, &mut r, &[0]),
            Err(crate::error::Error::Unsupported(_))
        ));

        // [i64::MAX; 10] + [i64::MAX; 10, 1.0_f64]: single pass fails exact_sum; merge refuses.
        let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
        l.init(1);
        r.init(1);
        let mut right_vals = vec![max_i; 10];
        right_vals.push(one_d);
        fold(&ctx, &mut l, &[max_i; 10], &[0; 10]);
        fold(&ctx, &mut r, &right_vals, &vec![0; right_vals.len()]);
        assert!(matches!(
            l.merge(&ctx, &mut r, &[0]),
            Err(crate::error::Error::Unsupported(_))
        ));

        // [i64::MAX; 20] + [1.0_f64] (single-row right): merges to UNDEF, matching single pass.
        let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
        l.init(1);
        r.init(1);
        let mut all = vec![max_i; 20];
        fold(&ctx, &mut l, &all, &[0; 20]);
        fold(&ctx, &mut r, &[one_d], &[0]);
        l.merge(&ctx, &mut r, &[0]).unwrap();
        all.push(one_d);
        assert_eq!(
            exact_key(&ctx, l.finish(&ctx, 0).unwrap()),
            single_pass_result(&ctx, &a, &all)
        );
        assert_eq!(single_pass_result(&ctx, &a, &all), "UNDEF");

        // Pure integers [i64::MAX; 20] + [i64::MIN; 20] = -20: merges exactly.
        let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
        l.init(1);
        r.init(1);
        let lv = [max_i; 20];
        let rv = [min_i; 20];
        fold(&ctx, &mut l, &lv, &[0; 20]);
        fold(&ctx, &mut r, &rv, &[0; 20]);
        l.merge(&ctx, &mut r, &[0]).unwrap();
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

    let exact_merge_cases: Vec<(&str, Vec<Id>, Vec<Id>)> = vec![
        (
            "decimal peak overflows partway through right",
            vec![d_100],
            vec![d_near_max, d_neg_100],
        ),
        (
            "decimal plus right leading integers overflow",
            vec![d_1e19],
            [vec![max_i; 18], vec![neg_max_i; 18], vec![d_half]].concat(),
        ),
        (
            "decimal plus pure int excursion overflow",
            vec![d_half],
            [vec![max_i; 20], vec![min_i; 20]].concat(),
        ),
        (
            "int plus decimal fails exact_sum at transition",
            vec![max_i; 10],
            [vec![max_i; 10], vec![d_neg_1e20]].concat(),
        ),
        (
            "int plus decimal temporary int excursion returns to range",
            vec![max_i; 10],
            [vec![max_i; 10], vec![neg_max_i; 10], vec![d_half]].concat(),
        ),
        (
            "right overflow with UNDEF becomes hard error",
            vec![dbl(&ctx, 1.0)],
            vec![d_max, d_max, Id::UNDEF],
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
            l.merge(&ctx, &mut r, &[0]).unwrap_or_else(|e| {
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

        // Right-side decimal overflow that a single pass avoids MUST refuse merge.
        for (name, lv, rv) in [
            (
                "canceled decimal overflow",
                vec![d_neg_max],
                vec![d_max, d_max],
            ),
            (
                "double avoids decimal overflow",
                vec![dbl(&ctx, 1.0)],
                vec![d_max, d_max],
            ),
        ] {
            let (mut l, mut r) = (Running::new(&a, true), Running::new(&a, true));
            l.init(1);
            r.init(1);
            fold(&ctx, &mut l, &lv, &vec![0; lv.len()]);
            fold(&ctx, &mut r, &rv, &vec![0; rv.len()]);
            assert!(
                matches!(
                    l.merge(&ctx, &mut r, &[0]),
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
        l.merge(&rctx, &mut r, &[0]).unwrap();
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
                match l.merge(&ctx, &mut r, &[0]) {
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

/// Exact row-by-row, column-by-column comparison of two grouping output tables, including
/// exact row order and bit-level float/double equality.
fn assert_tables_exact_eq(ctx: &Ctx, got: &Table, expected: &Table, label: &str) {
    assert_eq!(got.vars, expected.vars, "{label}: vars mismatch");
    assert_eq!(got.len, expected.len, "{label}: row count mismatch");
    for row in 0..expected.len {
        for col in 0..expected.cols.len() {
            let g = exact_key(ctx, got.cols[col][row]);
            let e = exact_key(ctx, expected.cols[col][row]);
            assert_eq!(g, e, "{label}: mismatch at row {row} col {col}");
        }
    }
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

    // 2. Adversarial skew: mostly one key, with a burst of 4,000 unique keys triggering
    // RANGE_SPLIT_MAX_GROUPS abort -> Key-Hash Partitioning fallback.
    let mut skewed = Table::new(vec![0, 1]);
    for i in 0..n {
        let k = if (8_000..12_000).contains(&i) {
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
