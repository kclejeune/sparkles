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
                l.merge(&ctx, &mut r, &[0]),
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
    l.merge(&ctx, &mut r, &[0]).unwrap();
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
    // and reports 0 when left already owns a DecimalSum box or overflows.
    let (mut left, mut right) = (Running::new(&sum_agg, true), Running::new(&sum_agg, true));
    left.init(4);
    right.init(4);
    // Group 0: empty + Decimal -> adopts box (+dec_box)
    // Group 1: Int + Decimal -> promotes to Decimal (+dec_box)
    // Group 2: Decimal + Decimal -> reuses left box (+0)
    // Group 3: Int(i64::MAX) + Decimal(MAX) -> overflows to Overflow (+0)
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
            dec(&ctx, i128::MAX),
        ],
        &[0, 1, 2, 3],
    );
    let _ = right.take_growth();
    left.merge(&ctx, &mut right, &[0, 1, 2, 3]).unwrap();
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
        base: 0,
    };
    assert!(r_ret.retract(&ctx, &batch, &[0]).unwrap());
    fold(&ctx, &mut r_ret, &[dec_str(&ctx, "1.5")], &[0]);
    assert!(matches!(
        l_ret.merge(&ctx, &mut r_ret, &[0]),
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
        left.merge(&ctx, &mut right, &[0, 0]),
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
        l2.merge(&ctx, &mut r2, &[1]),
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
        cl.merge(&ctx, &mut cr, &[0, 0]),
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
