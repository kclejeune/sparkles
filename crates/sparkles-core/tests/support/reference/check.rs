//! The comparison of an engine's SELECT answer with the reference evaluator's, which the
//! optimizer tests and the seeded cursor differential share.
//!
//! The comparison passes no answer that the specification forbids, and where it cannot
//! tell, it says how much it compared instead of passing the answer as compared. A column
//! whose values the specification leaves open, such as a SAMPLE or a RAND, is compared
//! only by its place in the rows, which makes the comparison partial. A column that only
//! computed values bind compares numbers by datatype and value, since the lexical form of
//! a computed number is the implementation's. Every other term must be the same term,
//! except that a column bound by both copied terms and computed values, where a number
//! that differs only in its lexical form may be either, makes the comparison partial.
//! Where the query orders or slices its answer, the engine's rows must be a slice, at the
//! query's offset, of an order that ORDER BY allows. Where an ORDER BY condition reads an
//! open value, the conditions after it can still order the rows that tie before it, so
//! such an order is not checked and a slice of it is not compared, beyond failing an
//! answer that the conditions before it forbid. An answer whose rows themselves are open,
//! because an open value decides which rows there are, is not compared.

use super::{AggregateErrors, Error, Notes, Provenance, Reference, Selected, value};

/// The engine's default reading of the aggregates over errors and non-strings, which the
/// comparisons take. Roadmap item 1064's setting makes Jena's reading the engine's
/// default, and the engine runs under it whenever a query chooses no other.
pub const ENGINE_AGGREGATE_ERRORS: AggregateErrors = AggregateErrors::Jena;
use oxrdf::Term;
use std::collections::{BTreeSet, HashMap};

/// The reference's answer to a query that an engine answered.
#[derive(Debug, Clone)]
pub enum Expected {
    Select(Selected),
    /// An ASK answer, with its notes, whose `chose` is set where it is open.
    Ask(bool, Notes),
    /// A query form that the comparisons do not check.
    Other,
}

/// The reference's answer to `text` under the engine's readings of the questions where
/// the specification and the engine differ, with the names of the readings it took. Where
/// the query meets one, it is evaluated again with the engine's reading, so that the
/// comparison is exact under it. These are the aggregates over errors and non-strings
/// under [`ENGINE_AGGREGATE_ERRORS`], the default of item 1064's setting, and string
/// arguments compatible whatever their base direction (item 1071). An
/// evaluation under one reading can meet another item that the first did not reach, so
/// the evaluations go on until one meets no item that the readings do not cover.
pub fn under_engine_readings(
    r: &Reference,
    text: &str,
) -> Result<(Expected, Vec<&'static str>), Error> {
    let query = super::parse(text, None)?;
    let run = || -> Result<Expected, Error> {
        Ok(match r.evaluate_all(&query)? {
            (_, _, Some(s)) => Expected::Select(s),
            (super::Answer::Ask(b), notes, None) => Expected::Ask(b, notes),
            _ => Expected::Other,
        })
    };
    let notes_of = |e: &Expected| match e {
        Expected::Select(s) => Some(s.notes),
        Expected::Ask(_, n) => Some(*n),
        Expected::Other => None,
    };
    let (mut aggregates, mut direction) = (false, false);
    let set = |aggregates: bool, direction: bool| {
        r.set_aggregate_errors(if aggregates {
            ENGINE_AGGREGATE_ERRORS
        } else {
            AggregateErrors::Spec
        });
        r.set_ignoring_direction_in_compatibility(direction);
    };
    let mut answer = run();
    // each round turns on at least one of the two readings, so there are at most three
    while let Some(notes) = answer.as_ref().ok().and_then(notes_of) {
        let a = aggregates
            || notes.min_max_error
            || notes.avg_of_errors
            || notes.group_concat_non_string;
        let d = direction || notes.direction_in_compatibility;
        if (a, d) == (aggregates, direction) {
            break;
        }
        (aggregates, direction) = (a, d);
        set(aggregates, direction);
        answer = run();
    }
    set(false, false);
    let mut readings = Vec::new();
    if aggregates {
        readings.push("the engine's aggregates over errors and non-strings");
    }
    if direction {
        readings.push("string arguments ignore the base direction");
    }
    Ok((answer?, readings))
}

/// How a comparison ended when it found no disagreement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The rows and their order were compared.
    Compared,
    /// The rows were compared, and their order was not, for the reason given.
    OrderUnchecked(String),
    /// The rows and their order were compared, and some of their cells only in part, for
    /// the reason given.
    Partial(String),
    /// Nothing was compared, for the reason given.
    Skipped(String),
}

/// The most strict-order tests that one order check makes before it gives up.
const ORDER_BUDGET: u64 = 50_000_000;

/// Compare the engine's rows, under `engine_vars`, with the reference's answer.
pub fn check_select(
    expected: &Selected,
    engine_vars: &[String],
    engine_rows: &[Vec<Option<Term>>],
) -> Result<Outcome, String> {
    if expected.rows_open {
        return Ok(Outcome::Skipped(
            "an open value decides which rows there are".into(),
        ));
    }
    if expected.notes.beyond_minimum_precision {
        return Ok(Outcome::Skipped(
            "the query reads a number of more than 16 digits".into(),
        ));
    }
    if engine_vars.len() != expected.variables.len() {
        return Err(format!(
            "columns {engine_vars:?} against the reference's {:?}",
            expected.variables
        ));
    }
    let columns: Vec<usize> = expected
        .variables
        .iter()
        .map(|v| {
            engine_vars
                .iter()
                .position(|x| x == v)
                .ok_or_else(|| format!("the engine lacks the column {v}"))
        })
        .collect::<Result<_, _>>()?;
    let compare = |relax_mixed: bool| -> Result<Outcome, String> {
        let mask = |row: &[Option<Term>]| -> String {
            (0..row.len())
                .map(|c| cell(&row[c], expected.open[c], expected.computed[c], relax_mixed))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let got: Vec<String> = engine_rows
            .iter()
            .map(|row| {
                let ordered: Vec<Option<Term>> = columns.iter().map(|&i| row[i].clone()).collect();
                mask(&ordered)
            })
            .collect();
        let Some(order) = &expected.order else {
            let mut a = got;
            let mut b: Vec<String> = expected.rows.iter().map(|r| mask(r)).collect();
            a.sort();
            b.sort();
            return if a != b {
                Err(bag_difference(&a, &b))
            } else if expected.order_unchecked {
                Ok(Outcome::OrderUnchecked(
                    "DISTINCT kept a row whose copies have different sort keys".into(),
                ))
            } else {
                Ok(Outcome::Compared)
            };
        };
        let items: Vec<String> = order.items.iter().map(|(r, _)| mask(r)).collect();
        let outcome = check_order(order, &items, &got)?;
        // The keys of an order with an open condition tell which orders are forbidden and
        // not which are allowed, so an answer that breaks no constraint is not passed.
        Ok(match outcome {
            Outcome::Compared | Outcome::OrderUnchecked(_)
                if order.open_conditions && got.len() < order.items.len() =>
            {
                Outcome::Skipped(
                    "an ORDER BY condition reads an open value, which can decide the rows of \
                     the slice"
                        .into(),
                )
            }
            Outcome::Compared if order.open_conditions => Outcome::OrderUnchecked(
                "an ORDER BY condition reads an open value, and the conditions after it can \
                 order rows that tie before it"
                    .into(),
            ),
            // where the slice cuts rows, an order that is not established leaves its rows
            // open too
            Outcome::OrderUnchecked(why) if got.len() < order.items.len() => {
                Outcome::Skipped(format!("the rows of the slice are not established: {why}"))
            }
            other => other,
        })
    };
    let outcome = match compare(false) {
        Ok(o) => o,
        Err(e) => {
            // a number of another lexical form in a column that copies terms and computes
            // values may be allowed or not
            let mixed = expected.computed.contains(&Provenance::Mixed);
            return match compare(true) {
                Ok(Outcome::Compared) if mixed => Ok(Outcome::Partial(format!(
                    "a column copies terms and computes values, and the answer agrees only \
                     where its numbers are compared by value: {e}"
                ))),
                // a weaker outcome under the relaxed comparison stays as weak
                Ok(Outcome::OrderUnchecked(why)) if mixed => Ok(Outcome::OrderUnchecked(format!(
                    "{why}, and a column that copies terms and computes values agrees only \
                     where its numbers are compared by value"
                ))),
                Ok(weaker) if mixed => Ok(weaker),
                _ => Err(e),
            };
        }
    };
    let open: Vec<&str> = expected
        .variables
        .iter()
        .zip(&expected.open)
        .filter(|(_, o)| **o)
        .map(|(v, _)| v.as_str())
        .collect();
    Ok(match outcome {
        Outcome::Compared if !open.is_empty() && !expected.rows.is_empty() => Outcome::Partial(
            format!("the columns {open:?} hold open values, compared only by their place"),
        ),
        other => other,
    })
}

/// One cell of a row as the comparison sees it. Numbers compare by datatype and value in
/// a computed column, and in a mixed one when `relax_mixed`.
fn cell(t: &Option<Term>, open: bool, provenance: Provenance, relax_mixed: bool) -> String {
    if open {
        return "*".into();
    }
    let relax = match provenance {
        Provenance::Computed => true,
        Provenance::Mixed => relax_mixed,
        Provenance::Copied => false,
    };
    match t {
        None => "UNDEF".into(),
        Some(Term::Literal(l)) if relax => match value::numeric(l) {
            Some(n) => format!("\"{}\"^^<{}>", n.canonical_lexical(), l.datatype().as_str()),
            None => l.to_string(),
        },
        Some(t) => t.to_string(),
    }
}

/// A short description of how two sorted bags differ.
fn bag_difference(got: &[String], expected: &[String]) -> String {
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for r in got {
        *counts.entry(r).or_default() += 1;
    }
    for r in expected {
        *counts.entry(r).or_default() -= 1;
    }
    let mut extra: Vec<&str> = counts
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(r, _)| *r)
        .collect();
    let mut missing: Vec<&str> = counts
        .iter()
        .filter(|(_, n)| **n < 0)
        .map(|(r, _)| *r)
        .collect();
    extra.sort();
    missing.sort();
    extra.truncate(5);
    missing.truncate(5);
    format!(
        "{} rows against the reference's {}; the engine's rows not in the reference's \
         answer include {extra:?}, and the reference's rows that the engine lacks include \
         {missing:?}",
        got.len(),
        expected.len()
    )
}

/// Check that `got` is a slice, at the query's offset, of an order of the reference's
/// rows that ORDER BY allows.
///
/// Rows with the same sort key form a block, and the strict order is tested between
/// every two blocks that the check meets, in both directions, so the check does not rely
/// on the reference's order extending the specification's. The engine's rows are
/// matched to the reference's rows, each to the first unused row with the same cells.
/// The returned rows must be in an allowed order among themselves. The other rows that
/// the order puts strictly before a returned row must all be cut by the offset, and those
/// strictly after one must all follow the slice, so the two sets cannot meet, the first
/// must fit in the offset and the second in the rows after the slice. Strict order need
/// not be transitive across these checks, where promotion makes two values equal, so a
/// slice that passes them is compared only when the reference's other rows, placed
/// around it in their own order, complete it to an allowed order, and its order is
/// unchecked otherwise.
fn check_order(
    order: &super::TopOrder,
    items: &[String],
    got: &[String],
) -> Result<Outcome, String> {
    let n = items.len();
    let from = order.offset.min(n);
    let expected_len = order.limit.map_or(n - from, |l| l.min(n - from));
    if got.len() != expected_len {
        return Err(format!(
            "{} rows against the reference's {expected_len}",
            got.len()
        ));
    }
    // blocks of equal keys, in the reference's order
    let mut block = Vec::with_capacity(n);
    let mut keys: Vec<Option<&super::SortKey>> = Vec::new();
    for (i, (_, k)) in order.items.iter().enumerate() {
        let same = i > 0 && k.is_some() && order.items[i - 1].1.as_ref() == k.as_ref();
        if !same {
            keys.push(k.as_ref());
        }
        block.push(keys.len() - 1);
    }
    let mut budget = ORDER_BUDGET;
    let mut before = |a: usize, b: usize| -> Option<bool> {
        budget = budget.checked_sub(1)?;
        Some(match (keys[a], keys[b]) {
            (Some(x), Some(y)) => super::strictly_before(x, y, &order.descending, order.term_ties),
            _ => false,
        })
    };
    // match the engine's rows to the reference's
    let mut by_row: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, r) in items.iter().enumerate().rev() {
        by_row.entry(r.as_str()).or_default().push(i);
    }
    // the rows whose copies fall in different blocks, where the matching chooses
    let spread: std::collections::HashSet<&str> = by_row
        .iter()
        .filter(|(_, is)| is.iter().any(|&k| block[k] != block[is[0]]))
        .map(|(r, _)| *r)
        .collect();
    let mut ambiguous = false;
    let mut chosen = Vec::with_capacity(got.len());
    for (j, r) in got.iter().enumerate() {
        let Some(free) = by_row.get_mut(r.as_str()) else {
            return Err(format!("row {j} {r} is not in the reference's answer"));
        };
        let Some(i) = free.pop() else {
            return Err(format!(
                "row {j} {r} occurs more often than in the reference's answer"
            ));
        };
        if spread.contains(r.as_str()) {
            ambiguous = true;
        }
        chosen.push(i);
    }
    let fail = |message: String| -> Result<Outcome, String> {
        if ambiguous {
            Ok(Outcome::OrderUnchecked(format!(
                "equal rows with different sort keys, and the first matching found {message}"
            )))
        } else {
            Err(message)
        }
    };
    let unchecked = || {
        Ok(Outcome::OrderUnchecked(
            "the order check's budget ran out".into(),
        ))
    };
    // the returned rows among themselves: a row after one it belongs before
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    for (j, &i) in chosen.iter().enumerate() {
        let b = block[i];
        for &earlier in &seen {
            match before(b, earlier) {
                None => return unchecked(),
                Some(true) => {
                    return fail(format!("row {j} {} belongs before an earlier row", got[j]));
                }
                Some(false) => {}
            }
        }
        seen.insert(b);
    }
    if got.len() == n {
        return Ok(Outcome::Compared);
    }
    // the rows outside the slice that the order places before or after a returned row
    let mut used = vec![false; n];
    for &i in &chosen {
        used[i] = true;
    }
    let returned: Vec<usize> = seen.iter().copied().collect();
    let mut forced_before = 0usize;
    let mut forced_after = 0usize;
    let mut verdict: HashMap<usize, (bool, bool)> = HashMap::new();
    for i in (0..n).filter(|&i| !used[i]) {
        let b = block[i];
        let (is_before, is_after) = match verdict.get(&b) {
            Some(v) => *v,
            None => {
                let mut is_before = false;
                let mut is_after = false;
                for &r in &returned {
                    if !is_before {
                        match before(b, r) {
                            None => return unchecked(),
                            Some(x) => is_before = x,
                        }
                    }
                    if !is_after {
                        match before(r, b) {
                            None => return unchecked(),
                            Some(x) => is_after = x,
                        }
                    }
                }
                verdict.insert(b, (is_before, is_after));
                (is_before, is_after)
            }
        };
        if is_before && is_after {
            return fail(format!(
                "the reference's row {} belongs both before and after returned rows",
                items[i]
            ));
        }
        forced_before += usize::from(is_before);
        forced_after += usize::from(is_after);
    }
    let rest = n - got.len();
    if forced_before > from {
        return fail(format!(
            "{forced_before} rows belong before the returned rows, more than the offset {from}"
        ));
    }
    if from + forced_after > rest {
        return fail(format!(
            "{forced_after} rows belong after the returned rows, more than the {} rows after the slice",
            rest - from
        ));
    }
    // A slice without ORDER BY has no constraints between its rows.
    if keys.iter().all(Option::is_none) {
        return Ok(Outcome::Compared);
    }
    // The checks above test the returned rows against each other row on its own, and the
    // strict order between the rows left out can still force a returned row out of the
    // slice. The answer is compared when one order that ORDER BY allows holds it, the
    // order that puts the rows left out around it in the reference's order, the first
    // `from` before it and the others after it.
    let unused: Vec<usize> = (0..n).filter(|&i| !used[i]).collect();
    let mut sequence: Vec<usize> = unused[..from]
        .iter()
        .chain(&chosen)
        .chain(&unused[from..])
        .map(|&i| block[i])
        .collect();
    sequence.dedup();
    for (j, &later) in sequence.iter().enumerate() {
        for &earlier in &sequence[..j] {
            match before(later, earlier) {
                None => return unchecked(),
                Some(true) => {
                    return Ok(Outcome::OrderUnchecked(
                        "the order that completes the slice with the reference's other rows \
                         breaks a constraint between them, and no other completion is tried"
                            .into(),
                    ));
                }
                Some(false) => {}
            }
        }
    }
    Ok(Outcome::Compared)
}
