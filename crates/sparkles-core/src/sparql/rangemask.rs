//! FILTER comparisons of one column with constants, decided on the column's ids.
//!
//! A conjunct `?v op c` with a numeric, `xsd:date` or `xsd:dateTime` constant `c`
//! accepts or rejects most ids without decoding them. Inline integers and decimals of
//! one scale sort by value, so the id ranges of the numeric range scan decide them
//! (see `numeric_ranges` in the planner). Inline dates and dateTimes sort by their
//! local calendar fields. A value whose local day lies three days or more from the
//! constant's day is earlier or later than the constant in every timezone, since
//! timezones span 14 hours on either side. Ids of other kinds of value cannot compare
//! with the constant, so the conjunct is false for them. Vocabulary and computed values,
//! and inline values near the constant, are evaluated as usual.

use super::ctx::Ctx;
use super::exec::{PAR_THRESHOLD, filter_mask};
use super::expr::Expr;
use super::exprcache::Report;
use super::plan::{IdRange, RangeOp, compare_atom, numeric_ranges};
use super::table::{Table, VarId};
use super::value::Value;
use crate::error::Result;
use crate::id::{Id, Tag};

/// Fewer rows are evaluated as usual: classifying them would save little.
const MIN_ROWS: usize = 1024;
/// Rows classified by one task.
const PIECE: usize = 16_384;

/// What the ids decide for a row.
#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(u8)]
enum Class {
    Pass,
    Fail,
    Test,
}

/// How the constants of the comparisons classify an id.
enum Rule {
    /// numeric constants: the id ranges of the numeric range scan, in id order
    Numeric(Vec<IdRange>),
    /// date or dateTime constants: the payloads of the constants' kind of id cut into
    /// pieces in which every comparison is decided the same way, and the class of each
    /// other tag
    Calendar {
        tag: Tag,
        /// the first payload of every piece but the first, ascending
        cuts: Vec<u64>,
        /// the class of each piece
        pieces: Vec<Class>,
        others: [Class; 16],
    },
}

impl Rule {
    #[inline]
    fn class(&self, id: Id) -> Class {
        match self {
            Rule::Numeric(ranges) => {
                match id.tag() {
                    // computed values are not among the scan's ranges
                    Tag::Local => return Class::Test,
                    Tag::Int | Tag::Double | Tag::Vocab | Tag::Delta | Tag::Decimal => {}
                    _ => return Class::Fail,
                }
                let i = ranges.partition_point(|r| r.hi < id.0);
                match ranges.get(i) {
                    Some(r) if r.lo <= id.0 => {
                        if r.exact {
                            Class::Pass
                        } else {
                            Class::Test
                        }
                    }
                    _ => Class::Fail,
                }
            }
            Rule::Calendar {
                tag,
                cuts,
                pieces,
                others,
            } => {
                let t = id.tag();
                if t != *tag {
                    return others[t as usize];
                }
                let p = id.payload();
                // the piece, counted without branches: dates come in any order
                let i: usize = cuts.iter().map(|&c| (p >= c) as usize).sum();
                pieces[i]
            }
        }
    }
}

/// The class of payload `p` of the constants' kind under the comparisons `bounds`
/// (operator, payloads below which a value is earlier, payloads from which it is
/// later).
fn calendar_class(p: u64, bounds: &[(RangeOp, u64, u64)]) -> Class {
    let mut out = Class::Pass;
    for &(op, below, above) in bounds {
        let ok = if p < below {
            matches!(op, RangeOp::Lt | RangeOp::Le)
        } else if p >= above {
            matches!(op, RangeOp::Gt | RangeOp::Ge)
        } else {
            out = Class::Test;
            continue;
        };
        if !ok {
            return Class::Fail;
        }
    }
    out
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The proleptic Gregorian date of a day number of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The payload of an inline date or dateTime at the start of a local day, or the
/// lowest or highest payload when the day lies outside the inline years.
fn day_payload(y: i64, m: i64, d: i64) -> u64 {
    let y = y + 8192;
    if y < 0 {
        return 0;
    }
    if y >= 1 << 14 {
        return u64::MAX;
    }
    (y as u64) << 43 | (m as u64) << 39 | (d as u64) << 34
}

/// The day of a constant date or dateTime, and the tag of the ids of its kind.
fn calendar_day(c: &Value) -> Option<(Tag, i64, i64, i64)> {
    match c {
        Value::Date(d) => Some((Tag::Date, d.year(), d.month() as i64, d.day() as i64)),
        Value::DateTime(d) => Some((Tag::DateTime, d.year(), d.month() as i64, d.day() as i64)),
        _ => None,
    }
}

/// Whether `c` is a constant that [`rule`] can decide comparisons with.
fn decidable(c: &Value) -> bool {
    c.is_numeric() || matches!(c, Value::Date(_) | Value::DateTime(_))
}

/// Days on either side of a constant's day within which the timezones can decide
/// either way.
const MARGIN_DAYS: i64 = 3;

fn rule(atoms: &[(RangeOp, Value)], ctx: &Ctx) -> Option<Rule> {
    if atoms.iter().all(|(_, c)| c.is_numeric()) {
        return Some(Rule::Numeric(numeric_ranges(atoms, ctx)));
    }
    calendar_rule(atoms)
}

/// The rule of comparisons that all have date constants, or all dateTime constants.
fn calendar_rule(atoms: &[(RangeOp, Value)]) -> Option<Rule> {
    let mut tag = None;
    let mut bounds = Vec::with_capacity(atoms.len());
    for (op, c) in atoms {
        let (t, y, m, d) = calendar_day(c)?;
        if *tag.get_or_insert(t) != t {
            return None;
        }
        let day = days_from_civil(y, m, d);
        let (ly, lm, ld) = civil_from_days(day - MARGIN_DAYS + 1);
        let (hy, hm, hd) = civil_from_days(day + MARGIN_DAYS);
        bounds.push((*op, day_payload(ly, lm, ld), day_payload(hy, hm, hd)));
    }
    let mut cuts: Vec<u64> = bounds.iter().flat_map(|b| [b.1, b.2]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let pieces = std::iter::once(0)
        .chain(cuts.iter().copied())
        .map(|p| calendar_class(p, &bounds))
        .collect();
    // values of other kinds never compare with the constants; stored and computed
    // literals may be dates in other forms
    let mut others = [Class::Fail; 16];
    for t in [Tag::Vocab, Tag::Delta, Tag::Local] {
        others[t as usize] = Class::Test;
    }
    Some(Rule::Calendar {
        tag: tag?,
        cuts,
        pieces,
        others,
    })
}

/// The conjuncts of `exprs` that compare one variable with constants, the mask of the
/// rows of `t` that pass them, and the other conjuncts. `None` when no such conjunct
/// applies, or the table is small.
pub(super) fn mask(
    ctx: &Ctx,
    t: &Table,
    exprs: &[Expr],
    report: &mut Report,
) -> Result<Option<(Vec<bool>, Vec<Expr>)>> {
    if !ctx.opt.range_pushdown || t.len() < MIN_ROWS {
        return Ok(None);
    }
    let conj: Vec<Expr> = exprs.iter().cloned().flat_map(Expr::conjuncts).collect();
    let atom = |e: &Expr, v: VarId| compare_atom(e, v, ctx, decidable);
    // the variable of the first conjunct that is a comparison with a constant
    let Some((v, c)) = conj.iter().find_map(|e| {
        let mut vars = Vec::new();
        e.vars(&mut vars);
        match vars[..] {
            [v] => atom(e, v).and_then(|_| Some((v, t.col_of(v)?))),
            _ => None,
        }
    }) else {
        return Ok(None);
    };
    let (ranged, rest): (Vec<Expr>, Vec<Expr>) =
        conj.into_iter().partition(|e| atom(e, v).is_some());
    let atoms: Vec<(RangeOp, Value)> = ranged.iter().filter_map(|e| atom(e, v)).collect();
    let Some(rule) = rule(&atoms, ctx) else {
        return Ok(None);
    };
    let col = &t.cols[c];
    let mut keep = vec![false; col.len()];
    // pieces of the column, classified in parallel; each lists its undecided rows
    let classify = |k: usize, part: &[Id], out: &mut [bool]| -> Result<Vec<usize>> {
        ctx.check()?;
        let mut tested = Vec::new();
        for (j, (&id, o)) in part.iter().zip(out.iter_mut()).enumerate() {
            let class = rule.class(id);
            *o = class == Class::Pass;
            if class == Class::Test {
                tested.push(k * PIECE + j);
            }
        }
        Ok(tested)
    };
    let pieces: Vec<Vec<usize>> = if col.len() > PAR_THRESHOLD {
        use rayon::prelude::*;
        col.par_chunks(PIECE)
            .zip(keep.par_chunks_mut(PIECE))
            .enumerate()
            .map(|(k, (part, out))| classify(k, part, out))
            .collect::<Result<_>>()?
    } else {
        col.chunks(PIECE)
            .zip(keep.chunks_mut(PIECE))
            .enumerate()
            .map(|(k, (part, out))| classify(k, part, out))
            .collect::<Result<_>>()?
    };
    let tested: Vec<usize> = pieces.concat();
    report.decided(ctx, &ranged, col.len(), tested.len());
    if !tested.is_empty() {
        // the undecided rows, evaluated once per distinct value where the expression
        // cache admits the conjuncts
        let sub = t.take_rows(&tested);
        let pass = match super::exprcache::filter(ctx, &sub, &ranged, report)? {
            Some((mut pass, rest)) => {
                if !rest.is_empty() {
                    for (p, q) in pass.iter_mut().zip(filter_mask(ctx, &sub, &rest)?) {
                        *p &= q;
                    }
                }
                pass
            }
            None => filter_mask(ctx, &sub, &ranged)?,
        };
        for (&i, ok) in tested.iter().zip(pass) {
            keep[i] = ok;
        }
    }
    Ok(Some((keep, rest)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn holds(op: RangeOp, o: Option<Ordering>) -> bool {
        match (op, o) {
            (RangeOp::Lt, Some(Ordering::Less)) => true,
            (RangeOp::Le, Some(Ordering::Less | Ordering::Equal)) => true,
            (RangeOp::Gt, Some(Ordering::Greater)) => true,
            (RangeOp::Ge, Some(Ordering::Greater | Ordering::Equal)) => true,
            (RangeOp::Eq, Some(Ordering::Equal)) => true,
            _ => false,
        }
    }

    fn lex(y: i64, m: u64, d: u64, h: u64, mi: u64, tz: Option<i64>, date: bool) -> String {
        let mut s = format!("{y:04}-{m:02}-{d:02}");
        if !date {
            s.push_str(&format!("T{h:02}:{mi:02}:00"));
        }
        match tz {
            None => {}
            Some(0) => s.push('Z'),
            Some(t) => s.push_str(&format!(
                "{}{:02}:{:02}",
                if t < 0 { '-' } else { '+' },
                t.abs() / 60,
                t.abs() % 60
            )),
        }
        s
    }

    /// Every decided id agrees with the value comparison, over values with and
    /// without timezones on either side of the constants, across month and year ends.
    #[test]
    fn calendar_classes_agree_with_value_comparison() {
        use oxrdf::vocab::xsd;
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        let tz = |a: u64, b: u64| match a {
            0 => None,
            1 => Some(0),
            _ => Some((b as i64 - 56) * 15),
        };
        let ops = [
            RangeOp::Lt,
            RangeOp::Le,
            RangeOp::Gt,
            RangeOp::Ge,
            RangeOp::Eq,
        ];
        let (mut decided, mut tested) = (0, 0);
        for date in [true, false] {
            let dt = if date { xsd::DATE } else { xsd::DATE_TIME };
            for _ in 0..200 {
                let day = days_from_civil(2008, 1, 1) + next(800) as i64 - 400;
                let (y, m, d) = civil_from_days(day);
                let (h, mi, a, b) = (next(24), next(60), next(4), next(113));
                let c_lex = lex(y, m as u64, d as u64, h, mi, tz(a, b), date);
                let c = Value::from_literal(&oxrdf::Literal::new_typed_literal(&c_lex, dt));
                let atoms = [
                    (ops[next(5) as usize], c.clone()),
                    (ops[next(5) as usize], c),
                ];
                let rule = calendar_rule(&atoms).expect("a calendar rule");
                for _ in 0..60 {
                    let (vy, vm, vd) = civil_from_days(day + next(16) as i64 - 8);
                    let (h, mi, a, b) = (next(24), next(60), next(4), next(113));
                    let v_lex = lex(vy, vm as u64, vd as u64, h, mi, tz(a, b), date);
                    let Some(id) = crate::id::inline_literal(&v_lex, dt.as_str()) else {
                        continue;
                    };
                    let v = Value::from_literal(&oxrdf::Literal::new_typed_literal(&v_lex, dt));
                    let truth = atoms.iter().all(|(op, c)| {
                        holds(*op, super::super::value::compare(&v, c).ok().flatten())
                    });
                    match rule.class(id) {
                        Class::Pass => assert!(truth, "{v_lex} {atoms:?}"),
                        Class::Fail => assert!(!truth, "{v_lex} {atoms:?}"),
                        Class::Test => tested += 1,
                    }
                    decided += 1;
                }
            }
        }
        assert!(decided > tested * 2, "{decided} {tested}");
    }

    #[test]
    fn other_kinds_of_value_fail_or_are_tested() {
        let c = Value::from_literal(&oxrdf::Literal::new_typed_literal(
            "2008-02-29",
            oxrdf::vocab::xsd::DATE,
        ));
        let rule = calendar_rule(&[(RangeOp::Ge, c)]).unwrap();
        assert_eq!(rule.class(Id::from_i64(20080229).unwrap()), Class::Fail);
        assert_eq!(rule.class(Id::UNDEF), Class::Fail);
        assert_eq!(rule.class(Id::new(Tag::Vocab, 7)), Class::Test);
        assert_eq!(rule.class(Id::new(Tag::Delta, 7)), Class::Test);
        assert_eq!(rule.class(Id::new(Tag::Local, 7)), Class::Test);
        let dt =
            crate::id::inline_literal("2009-01-01T00:00:00", oxrdf::vocab::xsd::DATE_TIME.as_str())
                .unwrap();
        assert_eq!(rule.class(dt), Class::Fail);
    }

    #[test]
    fn civil_days_round_trip() {
        for z in [
            -800_000, -719_468, -1, 0, 1, 59, 60, 365, 10_957, 13_939, 2_932_896,
        ] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z, "{z}: {y}-{m}-{d}");
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(
            days_from_civil(2008, 2, 29) + 1,
            days_from_civil(2008, 3, 1)
        );
        assert_eq!(
            days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28),
            2
        );
        assert_eq!(
            days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28),
            1
        );
    }
}
