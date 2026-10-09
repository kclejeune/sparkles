//! The sort key and the top-k heap against `order_cmp`, which stays the reference.

use super::*;
use crate::sparql::value::Value;
use oxrdf::vocab::xsd;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

fn typed(lex: &str, dt: &str) -> Value {
    Value::from_typed(lex, dt)
}

/// Values of every class `order_cmp` distinguishes, with ties, equal values of different
/// types, signed zeros, NaN, huge numbers, dates and times with and without timezones
/// near each other, durations of both kinds, composite literals and triple terms.
fn value(r: &mut Rng) -> Option<Value> {
    let small = r.below(9) as i64 - 4;
    Some(match r.below(26) {
        0 => return None,
        1 => Value::BNode(format!("b{}", r.below(4)).into()),
        2 => Value::Iri(
            r.pick(&["http://a", "http://a/b", "http://b", "urn:x", "http://a#"])
                .into(),
        ),
        3 => Value::Str(r.pick(&["", "a", "ab", "b", "B", "é", "a\u{0}"]).into()),
        4 => Value::Lang(
            r.pick(&["a", "ab", "b"]).into(),
            r.pick(&["en", "EN", "en-gb", "fr"]).into(),
        ),
        5 => Value::LangDir(
            r.pick(&["a", "b"]).into(),
            r.pick(&["en", "ar"]).into(),
            if r.below(2) == 0 {
                oxrdf::BaseDirection::Ltr
            } else {
                oxrdf::BaseDirection::Rtl
            },
        ),
        6 => typed(&small.to_string(), xsd::INTEGER.as_str()),
        7 => typed(
            r.pick(&[
                "9223372036854775807",
                "-9223372036854775808",
                "9007199254740993",
                "9007199254740992",
                "16777217",
            ]),
            xsd::INTEGER.as_str(),
        ),
        8 => typed(
            &format!("{small}.{}", r.below(3) * 5),
            xsd::DECIMAL.as_str(),
        ),
        9 => typed(
            r.pick(&[
                "0.1",
                "0.100000001490116",
                "16777216.0",
                "-0.0",
                "123456789012345678.5",
            ]),
            xsd::DECIMAL.as_str(),
        ),
        10 => typed(
            &format!("{small}.{}e0", r.below(3) * 5),
            xsd::DOUBLE.as_str(),
        ),
        11 => typed(
            r.pick(&[
                "NaN",
                "INF",
                "-INF",
                "0.0",
                "-0.0",
                "0.1",
                "9007199254740992",
                "1e300",
            ]),
            xsd::DOUBLE.as_str(),
        ),
        12 => typed(
            r.pick(&["NaN", "0.1", "-0.0", "0.0", "16777216", "2.5", "-1"]),
            xsd::FLOAT.as_str(),
        ),
        13 => typed(r.pick(&["true", "false", "1", "0"]), xsd::BOOLEAN.as_str()),
        14 => typed(
            r.pick(&[
                "2020-01-02T00:00:00+14:00",
                "2020-01-01T15:00:00",
                "2020-01-01T12:00:00Z",
                "2020-01-01T14:00:00+02:00",
                "2020-01-01T12:00:00",
                "2019-12-31T23:00:00-14:00",
                "2021-06-01T00:00:00",
                "2020-01-01T12:00:00.000Z",
            ]),
            xsd::DATE_TIME.as_str(),
        ),
        15 => typed(
            r.pick(&[
                "2020-01-01",
                "2020-01-01Z",
                "2020-01-02+14:00",
                "2019-12-31-10:00",
            ]),
            xsd::DATE.as_str(),
        ),
        16 => typed(
            r.pick(&["12:00:00", "12:00:00Z", "23:00:00-05:00", "01:00:00+14:00"]),
            xsd::TIME.as_str(),
        ),
        17 => typed(
            r.pick(&[
                "P1M", "P30D", "P31D", "P1Y", "PT24H", "P1D", "P0D", "P1MT1H",
            ]),
            xsd::DURATION.as_str(),
        ),
        18 => typed(
            r.pick(&["P1M", "P2M", "P1Y"]),
            xsd::YEAR_MONTH_DURATION.as_str(),
        ),
        19 => typed(
            r.pick(&["P1D", "PT1H", "P30D"]),
            xsd::DAY_TIME_DURATION.as_str(),
        ),
        20 => typed(
            r.pick(&["[1, 2]", "[1]", "[\"a\"]", "[]", "not a list"]),
            super::super::cdt::LIST,
        ),
        21 => typed(
            r.pick(&["{1: 2}", "{}", "{\"a\": 1}"]),
            super::super::cdt::MAP,
        ),
        22 => typed(r.pick(&["x", "y", "1"]), r.pick(&["urn:dt1", "urn:dt2"])),
        23 => typed(
            r.pick(&["abc", "1.5", "99999999999999999999999999"]),
            xsd::INTEGER.as_str(),
        ),
        24 => typed(&small.to_string(), "http://www.w3.org/2001/XMLSchema#int"),
        _ => Value::Triple(std::sync::Arc::new(oxrdf::Triple::new(
            oxrdf::NamedNode::new_unchecked(r.pick(&["http://a", "http://b"])),
            oxrdf::NamedNode::new_unchecked(RDF_TYPE),
            oxrdf::Literal::new_typed_literal(small.to_string(), xsd::INTEGER),
        ))),
    })
}

fn pool(seed: u64, n: usize) -> Vec<Option<Value>> {
    let mut r = Rng(seed);
    (0..n).map(|_| value(&mut r)).collect()
}

#[test]
fn key_order_is_order_cmp_on_every_pair() {
    for seed in 0..4 {
        let values = pool(seed, 600);
        let keys: Vec<SortKey> = values.iter().cloned().map(SortKey::new).collect();
        for (a, ka) in values.iter().zip(&keys) {
            for (b, kb) in values.iter().zip(&keys) {
                let expected = order_cmp(a.as_ref(), b.as_ref());
                assert_eq!(ka.cmp(kb), expected, "{a:?} vs {b:?}");
            }
        }
    }
}

/// The row comparator of the full ORDER BY sort before the keys: `order_cmp` per key,
/// each ascending or descending, ties by row.
fn reference(rows: &[Vec<Option<Value>>], asc: &[bool]) -> impl Fn(usize, usize) -> Ordering {
    move |a, b| {
        for (k, up) in asc.iter().enumerate() {
            let o = order_cmp(rows[a][k].as_ref(), rows[b][k].as_ref());
            let o = if *up { o } else { o.reverse() };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    }
}

fn rows(r: &mut Rng, n: usize, width: usize, families: &[u64]) -> Vec<Vec<Option<Value>>> {
    (0..n)
        .map(|_| {
            (0..width)
                .map(|_| {
                    loop {
                        let v = value(r);
                        if families.is_empty() || families.contains(&class_of(&v)) {
                            break v;
                        }
                    }
                })
                .collect()
        })
        .collect()
}

fn class_of(v: &Option<Value>) -> u64 {
    u64::from(SortKey::new(v.clone()).class)
}

fn keyed(rows: &[Vec<Option<Value>>]) -> Vec<Vec<SortKey>> {
    rows.iter()
        .map(|row| row.iter().cloned().map(SortKey::new).collect())
        .collect()
}

#[test]
fn full_sort_by_keys_is_the_comparator_sort() {
    // Same comparisons, same algorithm: the same permutation, totally ordered or not.
    let mut r = Rng(7);
    for case in 0..200 {
        let width = 1 + case % 3;
        let n = [0, 1, 2, 5, 40, 300, 2500][case % 7];
        let asc: Vec<bool> = (0..width).map(|_| r.below(2) == 0).collect();
        let rows = rows(&mut r, n, width, &[]);
        let keys = keyed(&rows);
        let expected = sort_positions(n, reference(&rows, &asc));
        let actual = sort_positions(n, |a, b| cmp_keys(&keys[a], &keys[b], &asc));
        assert_eq!(actual, expected, "case {case}");
    }
}

/// Whether `order` is consistent with every pairwise comparison, which makes it the only
/// order a correct sort or selection can produce.
fn consistent(order: &[usize], cmp: &impl Fn(usize, usize) -> Ordering) -> bool {
    for (i, &a) in order.iter().enumerate() {
        for &b in &order[i + 1..] {
            if cmp(a, b).then(a.cmp(&b)) != Ordering::Less {
                return false;
            }
        }
    }
    true
}

fn top_k(keys: &[Vec<SortKey>], asc: &[bool], k: usize, lazy: bool) -> Vec<usize> {
    let mut heap = TopK::new(k, asc.to_vec());
    for (i, row) in keys.iter().enumerate() {
        if lazy && heap.screen(&row[0]) == Screen::Reject {
            heap.skip();
            continue;
        }
        heap.offer(row.clone(), i);
    }
    heap.into_sorted().into_iter().map(|e| e.payload).collect()
}

#[test]
fn heap_is_the_prefix_of_the_sort_when_values_are_totally_ordered() {
    // Families whose order is total among themselves: unbound, blank nodes, IRIs,
    // strings, language strings, booleans and other literals; numbers are drawn
    // separately below.
    let total = [0, 1, 2, 3, 4, 6, 13];
    let mut r = Rng(11);
    let mut checked = 0;
    for case in 0..400 {
        let width = 1 + case % 3;
        let n = [1, 3, 17, 64, 500][case % 5];
        let asc: Vec<bool> = (0..width).map(|_| r.below(2) == 0).collect();
        let families: &[u64] = if case % 4 == 0 { &[5] } else { &total };
        let rows = rows(&mut r, n, width, families);
        let keys = keyed(&rows);
        let cmp = reference(&rows, &asc);
        let sorted = sort_positions(n, &cmp);
        if !consistent(&sorted, &cmp) {
            continue;
        }
        checked += 1;
        for k in [1, 2, 7, n / 2, n.saturating_sub(1), n, n + 3] {
            if k == 0 {
                continue;
            }
            let want = &sorted[..k.min(n)];
            assert_eq!(top_k(&keys, &asc, k, false), want, "case {case} k {k}");
            assert_eq!(top_k(&keys, &asc, k, true), want, "case {case} k {k} lazy");
        }
    }
    assert!(checked > 300, "{checked}");
}

#[test]
fn heap_ties_keep_input_order() {
    let v = |i: i64| Some(typed(&i.to_string(), xsd::INTEGER.as_str()));
    let rows: Vec<Vec<Option<Value>>> = [3, 1, 3, 2, 1, 3, 1].iter().map(|&i| vec![v(i)]).collect();
    let keys = keyed(&rows);
    assert_eq!(top_k(&keys, &[true], 4, true), [1, 4, 6, 3]);
    assert_eq!(top_k(&keys, &[false], 4, true), [0, 2, 5, 3]);
    // equal numbers of different types are ordered by lexical form, then datatype
    let rows = vec![
        vec![Some(typed("1.0e0", xsd::DOUBLE.as_str()))],
        vec![Some(typed("1", xsd::INTEGER.as_str()))],
        vec![Some(typed("1.0", xsd::DECIMAL.as_str()))],
        vec![Some(typed("01", xsd::INTEGER.as_str()))],
    ];
    let keys = keyed(&rows);
    let sorted = sort_positions(rows.len(), reference(&rows, &[true]));
    assert_eq!(top_k(&keys, &[true], 3, false), sorted[..3]);
}

#[test]
fn errors_and_unbound_sort_first_and_tie() {
    let rows = vec![
        vec![
            Some(Value::Str("a".into())),
            Some(Value::Iri("http://b".into())),
        ],
        vec![None, Some(Value::Iri("http://c".into()))],
        vec![Some(Value::Iri("http://a".into())), None],
        vec![None, Some(Value::Iri("http://a".into()))],
        vec![None, None],
    ];
    let keys = keyed(&rows);
    assert_eq!(top_k(&keys, &[true, true], 3, true), [4, 3, 1]);
    assert_eq!(top_k(&keys, &[false, true], 2, true), [0, 2]);
    assert_eq!(top_k(&keys, &[true, false], 5, true), [1, 3, 4, 2, 0]);
}

#[test]
fn partial_dates_are_compared_by_the_comparator() {
    // 12:00Z < 15:00 (no timezone; incomparable, by lexical form) < 00:00+14:00, which is
    // 10:00Z and so before 12:00Z: a cycle, so no total key exists for these rows
    let dates = [
        "2020-01-02T00:00:00+14:00",
        "2020-01-01T15:00:00",
        "2020-01-01T12:00:00Z",
    ];
    let values: Vec<Option<Value>> = dates
        .iter()
        .map(|d| Some(typed(d, xsd::DATE_TIME.as_str())))
        .collect();
    let keys: Vec<SortKey> = values.iter().cloned().map(SortKey::new).collect();
    for (a, ka) in values.iter().zip(&keys) {
        for (b, kb) in values.iter().zip(&keys) {
            assert_eq!(ka.cmp(kb), order_cmp(a.as_ref(), b.as_ref()));
        }
    }
    assert_eq!(keys[2].cmp(&keys[1]), Ordering::Less);
    assert_eq!(keys[1].cmp(&keys[0]), Ordering::Less);
    assert_eq!(keys[0].cmp(&keys[2]), Ordering::Less);
    // the heap is deterministic for each input order, and keeps the heap's answer
    // whether rows arrive one at a time or are offered after a screen
    for perm in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let rows: Vec<Vec<SortKey>> = [perm[0], perm[0], perm[1], perm[2]]
            .iter()
            .map(|&i| vec![keys[i].clone()])
            .collect();
        for k in 1..=4 {
            let a = top_k(&rows, &[true], k, false);
            assert_eq!(a, top_k(&rows, &[true], k, true), "{perm:?} {k}");
            assert_eq!(a.len(), k);
        }
    }
}

#[test]
fn descending_multiple_keys_match_the_comparator() {
    let mut r = Rng(23);
    for case in 0..300 {
        let rows = rows(&mut r, 120, 3, &[0, 2, 3, 5]);
        let asc = [case % 2 == 0, case % 3 == 0, case % 5 == 0];
        let keys = keyed(&rows);
        let cmp = reference(&rows, &asc);
        let sorted = sort_positions(rows.len(), &cmp);
        if !consistent(&sorted, &cmp) {
            continue;
        }
        for k in [1, 10, 119, 120] {
            assert_eq!(
                top_k(&keys, &asc, k, true),
                sorted[..k],
                "case {case} k {k}"
            );
        }
    }
}
