//! The comparison of an engine's SELECT answer with the reference evaluator's, which the
//! optimizer tests and the seeded cursor differential share.
//!
//! The comparison passes no answer that the specification forbids, and where it cannot
//! tell, it says how much it compared instead of passing the answer as compared. A column
//! whose values the specification leaves open, such as a SAMPLE or a RAND, is compared
//! only by its place in the rows, which makes the comparison partial. Computed
//! numeric bindings compare by datatype and value. Copied bindings compare exactly.
//! A lexical constructor can carry the spelling of a computed number. Another
//! spelling in the source datatype's lexical space makes the comparison partial,
//! with the result's datatype, language tag and base direction still exact.
//! Where the query orders or slices its answer, the engine's rows must be a slice, at the
//! query's offset, of an order that ORDER BY allows. Where an ORDER BY condition reads an
//! open value, the conditions after it can still order the rows that tie before it, so
//! such an order is not checked and a slice of it is not compared, beyond failing an
//! answer that the conditions before it forbid. An answer whose rows themselves are open,
//! because an open value decides which rows there are, is not compared.

use super::{
    AggregateErrors, Error, Notes, Origin, Reference, Selected, SortKey, SpellingKey, TopOrder,
    value,
};

/// The engine's default reading of the aggregates over errors and non-strings, which the
/// comparisons take. Roadmap item 1064's setting makes Jena's reading the engine's
/// default, and the engine runs under it whenever a query chooses no other.
pub const ENGINE_AGGREGATE_ERRORS: AggregateErrors = AggregateErrors::Jena;
use oxrdf::Term;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

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
    let got: Vec<Vec<Option<Term>>> = engine_rows
        .iter()
        .map(|row| columns.iter().map(|&i| row[i].clone()).collect())
        .collect();
    let rows: Vec<_> = match &expected.order {
        Some(order) => order.items.iter().map(|(row, _)| row.as_slice()).collect(),
        None => expected.rows.iter().map(Vec::as_slice).collect(),
    };
    let compare = |relax_lexical| -> Result<Outcome, String> {
        let order = expected.order.as_ref();
        let size = order.map_or(rows.len(), |o| {
            let available = rows.len().saturating_sub(o.offset);
            o.limit.map_or(available, |l| l.min(available))
        });
        if got.len() != size {
            return Err(format!("{} rows against the reference's {size}", got.len()));
        }
        let matching = match_rows(expected, &rows, &got, relax_lexical)?;
        let Some(order) = order else {
            return Ok(Outcome::Compared);
        };
        let keys = observed_keys(order, &expected.variables, &matching.chosen, &got);
        let outcome = match check_order(order, &keys.terms, &matching.chosen) {
            Err(message) if matching.ambiguous => {
                check_alternative_keys(order, &expected.variables, &got, &matching, message)?
            }
            other => other?,
        };
        let open_conditions = order.open_conditions || keys.incomplete;
        // The keys of an order with an open condition tell which orders are forbidden and
        // not which are allowed, so an answer that breaks no constraint is not passed.
        Ok(match outcome {
            Outcome::Compared | Outcome::OrderUnchecked(_)
                if open_conditions && got.len() < order.items.len() =>
            {
                Outcome::Skipped(
                    "an ORDER BY key has an unknown suffix, which can decide the rows of the slice"
                        .into(),
                )
            }
            Outcome::Compared if open_conditions => Outcome::OrderUnchecked(
                "an ORDER BY key has an unknown suffix, which can order rows that tie on its prefix"
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
            if !expected
                .origins
                .iter()
                .flatten()
                .any(|o| matches!(o, Origin::NumericLexical(_)))
            {
                return Err(e);
            }
            return match compare(true) {
                Ok(Outcome::Compared) => Ok(Outcome::Partial(format!(
                    "the answer agrees only when a computed numeric spelling is compared by value: {e}"
                ))),
                Ok(Outcome::OrderUnchecked(why)) => Ok(Outcome::OrderUnchecked(format!(
                    "{why}, and the answer agrees only when a computed numeric spelling is compared by value"
                ))),
                Ok(weaker) => Ok(weaker),
                Err(_) => Err(e),
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

/// The expected binding supplies the comparison policy for both cells.
fn cell(t: &Option<Term>, open: bool, origin: Origin) -> String {
    if open {
        return "*".into();
    }
    let Some(term) = t else {
        return "UNDEF".into();
    };
    if let Term::Literal(l) = term {
        match origin {
            Origin::ComputedNumber => {
                if let Some(n) = value::numeric(l) {
                    return format!("\"{}\"^^<{}>", n.canonical_lexical(), l.datatype().as_str());
                }
            }
            Origin::NumericLexical(kind) => {
                if let Some(canon) = kind.lexical_key(l.value()) {
                    return format!(
                        "#{canon}^^<{}>@{:?}--{:?}",
                        l.datatype().as_str(),
                        l.language(),
                        l.direction()
                    );
                }
            }
            Origin::Exact => {}
        }
    }
    term.to_string()
}

fn row_key(row: &[Option<Term>], open: &[bool], origins: &[Origin]) -> String {
    row.iter()
        .zip(open)
        .zip(origins)
        .map(|((term, &open), &origin)| cell(term, open, origin))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Default)]
struct RowGroup {
    expected: Vec<usize>,
    matched: Vec<usize>,
}

struct Matching {
    chosen: Vec<usize>,
    groups: Vec<RowGroup>,
    candidates: Vec<Vec<usize>>,
    ambiguous: bool,
}

/// Index expected rows by their cell policies and the keys under those policies.
/// Groups of identical rows share a capacity. A match can be reassigned if a later
/// row has fewer choices, as when an engine row can match both a copied term and a
/// computed spelling but a later row can only match the computed spelling.
fn match_rows(
    expected: &Selected,
    rows: &[&[Option<Term>]],
    got: &[Vec<Option<Term>>],
    relax_lexical: bool,
) -> Result<Matching, String> {
    let mut by_policy: HashMap<Vec<Origin>, HashMap<String, usize>> = HashMap::new();
    let mut groups: Vec<RowGroup> = Vec::new();
    for (i, (row, origins)) in rows.iter().zip(&expected.origins).enumerate() {
        let policy: Vec<_> = origins
            .iter()
            .zip(&expected.open)
            .map(|(&origin, &open)| match origin {
                _ if open => Origin::Exact,
                Origin::NumericLexical(_) if !relax_lexical => Origin::Exact,
                other => other,
            })
            .collect();
        let key = row_key(row, &expected.open, &policy);
        let group = *by_policy
            .entry(policy)
            .or_default()
            .entry(key)
            .or_insert_with(|| {
                groups.push(RowGroup::default());
                groups.len() - 1
            });
        groups[group].expected.push(i);
    }

    let candidates: Vec<Vec<usize>> = got
        .iter()
        .map(|row| {
            let mut ids: Vec<_> = by_policy
                .iter()
                .filter_map(|(policy, index)| {
                    index.get(&row_key(row, &expected.open, policy)).copied()
                })
                .collect();
            ids.sort_unstable();
            ids
        })
        .collect();
    for (i, options) in candidates.iter().enumerate() {
        if let Some(group) = options
            .iter()
            .filter(|&&g| groups[g].matched.len() < groups[g].expected.len())
            .min_by_key(|&&g| groups[g].expected[groups[g].matched.len()])
        {
            groups[*group].matched.push(i);
        } else if !reassign(i, &candidates, &mut groups) {
            return Err(format!(
                "row {i} {:?} has no unused match in the reference's answer",
                got[i]
            ));
        }
    }

    let mut chosen = vec![0; got.len()];
    for group in &groups {
        for (&actual, &reference) in group.matched.iter().zip(&group.expected) {
            chosen[actual] = reference;
        }
    }
    let ambiguous = expected.order.as_ref().is_some_and(|order| {
        let spread: Vec<_> = groups
            .iter()
            .map(|g| {
                let first = &order.items[g.expected[0]].1;
                g.expected.iter().any(|&i| &order.items[i].1 != first)
            })
            .collect();
        candidates.iter().enumerate().any(|(i, options)| {
            let first = &order.items[chosen[i]].1;
            options
                .iter()
                .any(|&g| spread[g] || &order.items[groups[g].expected[0]].1 != first)
        })
    });
    Ok(Matching {
        chosen,
        groups,
        candidates,
        ambiguous,
    })
}

/// Find a free group by following alternating paths through occupied matches.
fn reassign(i: usize, candidates: &[Vec<usize>], groups: &mut [RowGroup]) -> bool {
    if candidates[i].is_empty() {
        return false;
    }
    let mut queue = VecDeque::from([(i, None)]);
    let mut via = HashMap::new();
    while let Some((row, parent)) = queue.pop_front() {
        for &g in &candidates[row] {
            if via.contains_key(&g) {
                continue;
            }
            via.insert(g, (row, parent));
            let group = &groups[g];
            if group.matched.len() < group.expected.len() {
                groups[g].matched.push(row);
                let mut step = parent;
                while let Some((previous, slot)) = step {
                    let (row, parent) = via[&previous];
                    groups[previous].matched[slot] = row;
                    step = parent;
                }
                return true;
            }
            queue.extend(
                group
                    .matched
                    .iter()
                    .enumerate()
                    .map(|(slot, &row)| (row, Some((g, slot)))),
            );
        }
    }
    false
}

struct ObservedKeys {
    terms: Vec<Option<KeyTerms>>,
    incomplete: bool,
}

type KeyTerms = Vec<Option<Term>>;

/// Apply one possible reference row's key to an engine row, keeping only the
/// conditions whose values are known.
fn observe_key(
    key: &SortKey,
    variables: &[String],
    row: Option<&[Option<Term>]>,
) -> (KeyTerms, bool) {
    let mut terms = key.terms.clone();
    for (i, source) in key.spelling.iter().enumerate() {
        let Some(source) = source else { continue };
        let cell = |v: &oxrdf::Variable| {
            let column = variables.iter().position(|name| name == v.as_str())?;
            Some(&row?[column])
        };
        let observed = match source {
            SpellingKey::Variable(v) => cell(v).cloned(),
            SpellingKey::StrOf(v) => {
                cell(v).map(|cell| cell.as_ref().and_then(value::lexical_string))
            }
            SpellingKey::Unobserved => None,
        };
        let Some(observed) = observed else {
            terms.truncate(i);
            return (terms, true);
        };
        terms[i] = observed;
    }
    (terms, false)
}

fn observed_keys(
    order: &TopOrder,
    variables: &[String],
    chosen: &[usize],
    got: &[Vec<Option<Term>>],
) -> ObservedKeys {
    let mut observed = vec![None; order.items.len()];
    for (&i, row) in chosen.iter().zip(got) {
        observed[i] = Some(row.as_slice());
    }
    let mut incomplete = false;
    let terms = order
        .items
        .iter()
        .zip(observed)
        .map(|((_, key), row)| {
            let (terms, open) = observe_key(key.as_ref()?, variables, row);
            incomplete |= open;
            Some(terms)
        })
        .collect();
    ObservedKeys { terms, incomplete }
}

/// Raw reference keys can differ even when the observed ORDER BY values agree.
/// Only weaken a failure if a choice of matching could affect the constraints.
fn check_alternative_keys(
    order: &TopOrder,
    variables: &[String],
    got: &[Vec<Option<Term>>],
    matching: &Matching,
    message: String,
) -> Result<Outcome, String> {
    let group_keys: Vec<HashSet<_>> = matching
        .groups
        .iter()
        .map(|group| {
            group
                .expected
                .iter()
                .map(|&i| order.items[i].1.as_ref())
                .collect()
        })
        .collect();
    let observed = |key: Option<&SortKey>, row| key.map(|k| observe_key(k, variables, row).0);
    let sliced = got.len() < order.items.len();
    let mut varies = false;
    let mut budget = ORDER_BUDGET;
    let unchecked = || {
        Ok(Outcome::OrderUnchecked(
            "the order check's budget ran out".into(),
        ))
    };
    let mut alternatives: Vec<Vec<Option<KeyTerms>>> = Vec::new();
    for (i, row) in got.iter().enumerate() {
        let chosen = order.items[matching.chosen[i]].1.as_ref();
        let outside = observed(chosen, None);
        let mut keys = HashSet::new();
        for &g in &matching.candidates[i] {
            for &key in &group_keys[g] {
                let Some(left) = budget.checked_sub(1) else {
                    return unchecked();
                };
                budget = left;
                keys.insert(observed(key, Some(row.as_slice())));
                // For a slice, exchanging rows between the returned and omitted
                // sets can alter the constraints even when the returned keys agree.
                varies |= sliced && observed(key, None) != outside;
            }
        }
        varies |= keys.len() > 1;
        alternatives.push(keys.into_iter().collect());
    }
    if !varies {
        return Err(message);
    }
    let mut all_before = |left: &[Option<KeyTerms>], right: &[Option<KeyTerms>]| -> Option<bool> {
        for a in left {
            for b in right {
                budget = budget.checked_sub(1)?;
                if !matches!((a, b), (Some(a), Some(b))
                    if super::strictly_before(a, b, &order.descending, order.term_ties))
                {
                    return Some(false);
                }
            }
        }
        Some(true)
    };
    // Keys are chosen independently here. An inversion that holds under every
    // such choice also holds under every consistent matching of the whole bag.
    for (j, later) in alternatives.iter().enumerate() {
        for earlier in &alternatives[..j] {
            match all_before(later, earlier) {
                None => return unchecked(),
                Some(true) => return Err(format!("row {j} belongs before an earlier row")),
                Some(false) => {}
            }
        }
    }
    if sliced {
        let mut possible_returns = vec![0; matching.groups.len()];
        for &g in matching.candidates.iter().flatten() {
            possible_returns[g] += 1;
        }
        let mut forced_before = 0;
        let mut forced_after = 0;
        for (group, max_returned) in matching.groups.iter().zip(possible_returns) {
            if group.expected.len() <= max_returned {
                continue;
            }
            let mut before = 0;
            let mut after = 0;
            let mut both = 0;
            for &i in &group.expected {
                let outside = observed(order.items[i].1.as_ref(), None);
                let outside = std::slice::from_ref(&outside);
                let mut is_before = false;
                let mut is_after = false;
                for returned in &alternatives {
                    if !is_before {
                        let Some(forced) = all_before(outside, returned) else {
                            return unchecked();
                        };
                        is_before = forced;
                    }
                    if !is_after {
                        let Some(forced) = all_before(returned, outside) else {
                            return unchecked();
                        };
                        is_after = forced;
                    }
                }
                before += usize::from(is_before);
                after += usize::from(is_after);
                both += usize::from(is_before && is_after);
            }
            // At most this many rows in the group can be used by any matching.
            // A larger set of forced rows must leave some outside the slice.
            if both > max_returned {
                return Err(
                    "an omitted reference row belongs both before and after returned rows".into(),
                );
            }
            forced_before += before.saturating_sub(max_returned);
            forced_after += after.saturating_sub(max_returned);
        }
        let n = order.items.len();
        check_slice_capacity(
            order.offset.min(n),
            n - got.len(),
            forced_before,
            forced_after,
        )?;
    }
    Ok(Outcome::OrderUnchecked(format!(
        "equal rows can have different sort keys, and the first matching found {message}"
    )))
}

/// Check that `got` is a slice, at the query's offset, of an order of the reference's
/// rows that ORDER BY allows.
///
/// Rows with the same sort key form a block, and the strict order is tested between
/// every two blocks that the check meets, in both directions, so the check does not rely
/// on the reference's order extending the specification's. The engine's rows are
/// matched to the reference's rows, using the match established by the row comparison.
/// The returned rows must be in an allowed order among themselves. The other rows that
/// the order puts strictly before a returned row must all be cut by the offset, and those
/// strictly after one must all follow the slice, so the two sets cannot meet, the first
/// must fit in the offset and the second in the rows after the slice. Strict order need
/// not be transitive across these checks, where promotion makes two values equal, so a
/// slice that passes them is compared only when the reference's other rows, placed
/// around it in their own order, complete it to an allowed order, and its order is
/// unchecked otherwise.
fn check_order(
    order: &TopOrder,
    row_keys: &[Option<KeyTerms>],
    chosen: &[usize],
) -> Result<Outcome, String> {
    let n = row_keys.len();
    let from = order.offset.min(n);
    // blocks of equal keys, in the reference's order
    let mut block = Vec::with_capacity(n);
    let mut keys = Vec::new();
    for (i, k) in row_keys.iter().enumerate() {
        let same = i > 0 && k.is_some() && &row_keys[i - 1] == k;
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
                    return Err(format!("row {j} belongs before an earlier row"));
                }
                Some(false) => {}
            }
        }
        seen.insert(b);
    }
    if chosen.len() == n {
        return Ok(Outcome::Compared);
    }
    // the rows outside the slice that the order places before or after a returned row
    let mut used = vec![false; n];
    for &i in chosen {
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
            return Err(format!(
                "the reference's row {} belongs both before and after returned rows",
                i
            ));
        }
        forced_before += usize::from(is_before);
        forced_after += usize::from(is_after);
    }
    let rest = n - chosen.len();
    check_slice_capacity(from, rest, forced_before, forced_after)?;
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
        .chain(chosen)
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

fn check_slice_capacity(
    from: usize,
    rest: usize,
    forced_before: usize,
    forced_after: usize,
) -> Result<(), String> {
    if forced_before > from {
        return Err(format!(
            "{forced_before} rows belong before the returned rows, more than the offset {from}"
        ));
    }
    if from + forced_after > rest {
        return Err(format!(
            "{forced_after} rows belong after the returned rows, more than the {} rows after the slice",
            rest - from
        ));
    }
    Ok(())
}
