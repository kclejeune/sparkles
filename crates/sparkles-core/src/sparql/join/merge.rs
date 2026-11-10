//! The merge join kernel: two key columns sorted the same way, joined run by run.
//!
//! The kernel takes the lower bound of every left key in the right column from the
//! branch-free zipper of [`super::zipper`], which gallops when one side is much larger,
//! and then expands, counts or decides each left row against the run of right rows that
//! starts at its lower bound. A compare per row would mispredict when the two sides'
//! keys interleave, so the decisions are made without one where they can be. The
//! eager executor hands the kernel whole columns, and the cursor hands it the keys of
//! the left rows a loaded right batch decides, keeping the runs that cross its batch
//! boundaries and the spooling of long runs to itself.
use super::hash::NO_ROW;
use super::zipper::lower_bounds;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::Ctx;
use crate::sparql::exec::run_end;

/// The lower bound of every key of `lk` in `rk`, both sorted: the first right row whose
/// key is not less than the left row's, or `rk.len()`.
pub(crate) fn bounds(ctx: &Ctx, lk: &[Id], rk: &[Id]) -> Result<Vec<u32>> {
    lower_bounds(ctx, lk, rk)
}

/// Pair the left rows from `from` with the right runs of their keys, given the lower
/// bounds `first` of the left keys in `rk`: a left row whose key the right column holds
/// pairs with every row of the run that `compatible` accepts, as (left row, right
/// row), and in an OPTIONAL join a left row with no accepted pair pairs with
/// [`NO_ROW`]. At most `room` pairs are appended to `lrows` and `rrows`, a run whose
/// length does not fit is left whole for the next call, and the number of left rows
/// decided is returned, so that the caller resumes at `from` plus it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expand(
    ctx: &Ctx,
    lk: &[Id],
    rk: &[Id],
    first: &[u32],
    from: usize,
    optional: bool,
    room: usize,
    compatible: impl Fn(usize, usize) -> bool,
    lrows: &mut Vec<u32>,
    rrows: &mut Vec<u32>,
) -> Result<usize> {
    let limit = lrows.len().saturating_add(room);
    let mut run = (usize::MAX, 0usize);
    let mut done = 0;
    // candidates tested since the last cancellation check, since a rejected candidate
    // takes no room
    let mut steps = 0usize;
    for (j, (&at, &key)) in first[from..].iter().zip(&lk[from..]).enumerate() {
        if j.is_multiple_of(4096) {
            ctx.check()?;
        }
        let at = at as usize;
        let left = from + j;
        let row = left as u32;
        if at < rk.len() && rk[at] == key {
            if run.0 != at {
                run = (at, run_end(rk, at));
            }
            let n = run.1 - at;
            if lrows.len() + n > limit {
                break;
            }
            let before = lrows.len();
            for right in at..run.1 {
                steps += 1;
                if steps.is_multiple_of(POLL) {
                    ctx.check()?;
                }
                if compatible(left, right) {
                    lrows.push(row);
                    rrows.push(right as u32);
                }
            }
            if optional && lrows.len() == before {
                lrows.push(row);
                rrows.push(NO_ROW);
            }
        } else if optional {
            if lrows.len() == limit {
                break;
            }
            lrows.push(row);
            rrows.push(NO_ROW);
        }
        done = j + 1;
    }
    Ok(done)
}

/// Candidates tested between two checks of cancellation and the deadline.
const POLL: usize = 4096;

/// Pair the left row `left` with the right rows `rows`, one run of its key, from
/// `rows.start`, as [`expand`] does for a run that fits its room, but at most `room`
/// candidates at a time: the pairs that `compatible` accepts are appended, and the
/// position after the last candidate tested is returned, so that the caller resumes a
/// run longer than its room there and checks its output between the calls. The caller
/// extends an OPTIONAL row that no candidate of the whole run matched.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expand_run(
    ctx: &Ctx,
    left: usize,
    rows: std::ops::Range<usize>,
    room: usize,
    compatible: impl Fn(usize, usize) -> bool,
    lrows: &mut Vec<u32>,
    rrows: &mut Vec<u32>,
) -> Result<usize> {
    let end = rows.end.min(rows.start.saturating_add(room.max(1)));
    for right in rows.start..end {
        if (right - rows.start).is_multiple_of(POLL) {
            ctx.check()?;
        }
        if compatible(left, right) {
            lrows.push(left as u32);
            rrows.push(right as u32);
        }
    }
    Ok(end)
}

/// How many rows the join of `lk` and `rk` has, given the lower bounds `first` of the
/// left keys in `rk`: each left row adds the length of its key's right run, and in an
/// OPTIONAL join one when the right column lacks its key. The run lengths come from one
/// pass over the right column, so no row is paired and no branch depends on a key.
pub(crate) fn count(ctx: &Ctx, lk: &[Id], rk: &[Id], first: &[u32], optional: bool) -> Result<u64> {
    // The length of the run of equal keys that starts at each right row.
    let _scratch = ctx.charge(rk.len() as u64 * 4 + 64)?;
    let mut runs = vec![1u32; rk.len() + 1];
    runs[rk.len()] = 0;
    for j in (0..rk.len().saturating_sub(1)).rev() {
        runs[j] = 1 + u32::from(rk[j] == rk[j + 1]) * runs[j + 1];
    }
    let unmatched = u64::from(optional);
    let mut n = 0u64;
    for (k, (&at, &key)) in first.iter().zip(lk).enumerate() {
        if k.is_multiple_of(65536) {
            ctx.check()?;
        }
        let at = at as usize;
        let hit = at < rk.len() && rk[at.min(rk.len().saturating_sub(1))] == key;
        n = n
            .checked_add(u64::from(hit) * u64::from(runs[at]) + u64::from(!hit) * unmatched)
            .ok_or_else(|| Error::invalid("join count overflow"))?;
    }
    Ok(n)
}

/// Which left rows pass a semi join (`keep_matched`) or an anti join or MINUS (not
/// `keep_matched`) on the key, given the lower bounds `first` of the left keys in `rk`:
/// a left row is matched when the right row at its lower bound holds its key. The
/// verdicts are computed without a branch per row.
pub(crate) fn filter(lk: &[Id], rk: &[Id], first: &[u32], keep_matched: bool) -> Vec<bool> {
    first
        .iter()
        .zip(lk)
        .map(|(&at, &key)| {
            let at = at as usize;
            let hit = at < rk.len() && rk[at.min(rk.len().saturating_sub(1))] == key;
            hit == keep_matched
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoreOptions};

    fn ids(values: &[i64]) -> Vec<Id> {
        values.iter().map(|v| Id::from_i64(*v).unwrap()).collect()
    }

    /// The expanded pairs, the count and the filter agree with a nested loop over the
    /// keys, with runs on both sides and keys only one side has.
    #[test]
    fn expansion_count_and_filter_agree_with_a_nested_loop() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let lk = ids(&[1, 2, 2, 4, 5, 5, 5, 9]);
        let rk = ids(&[0, 2, 2, 2, 3, 5, 6, 6, 9, 9]);
        let first = bounds(&ctx, &lk, &rk).unwrap();
        let mut expected = Vec::new();
        for (i, l) in lk.iter().enumerate() {
            for (j, r) in rk.iter().enumerate() {
                if l == r {
                    expected.push((i as u32, j as u32));
                }
            }
        }
        for optional in [false, true] {
            let mut want = expected.clone();
            if optional {
                for i in [0u32, 3] {
                    want.push((i, NO_ROW));
                }
                want.sort_by_key(|&(i, j)| (i, j == NO_ROW, j));
            }
            for room in [1usize, 2, 3, 7, 1000] {
                let (mut lrows, mut rrows) = (Vec::new(), Vec::new());
                let mut from = 0;
                while from < lk.len() {
                    let done = expand(
                        &ctx,
                        &lk,
                        &rk,
                        &first,
                        from,
                        optional,
                        room,
                        |_, _| true,
                        &mut lrows,
                        &mut rrows,
                    )
                    .unwrap();
                    if done == 0 {
                        // a run larger than the room is expanded with a room that holds it
                        let n = run_end(&rk, first[from] as usize) - first[from] as usize;
                        assert!(n > room, "room {room} from {from}");
                        from += expand(
                            &ctx,
                            &lk,
                            &rk,
                            &first,
                            from,
                            optional,
                            n,
                            |_, _| true,
                            &mut lrows,
                            &mut rrows,
                        )
                        .unwrap();
                        continue;
                    }
                    from += done;
                }
                let got: Vec<(u32, u32)> = lrows.into_iter().zip(rrows).collect();
                assert_eq!(got, want, "optional {optional} room {room}");
            }
            assert_eq!(
                count(&ctx, &lk, &rk, &first, optional).unwrap(),
                want.len() as u64,
                "optional {optional}"
            );
        }
        let matched: Vec<bool> = lk.iter().map(|l| rk.contains(l)).collect();
        assert_eq!(filter(&lk, &rk, &first, true), matched);
        assert_eq!(
            filter(&lk, &rk, &first, false),
            matched.iter().map(|m| !m).collect::<Vec<_>>()
        );
    }

    /// Pairs a second shared variable rejects are not produced, so a run's length bounds
    /// the room it needs while the pairs it gives count only when accepted, and an
    /// OPTIONAL row none of whose candidates passes is kept alone.
    #[test]
    fn incompatible_pairs_of_a_run_are_dropped_before_they_count() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let lk = ids(&[1, 1, 1, 2]);
        let rk = ids(&[1, 1, 1, 2]);
        let first = bounds(&ctx, &lk, &rk).unwrap();
        // left row i pairs with right row j only when i == j, and row 3 with none
        let same = |l: usize, r: usize| l == r && l != 3;
        for optional in [false, true] {
            let (mut l, mut r) = (Vec::new(), Vec::new());
            let mut from = 0;
            while from < lk.len() {
                from += expand(
                    &ctx, &lk, &rk, &first, from, optional, 3, same, &mut l, &mut r,
                )
                .unwrap();
            }
            let mut want = vec![(0, 0), (1, 1), (2, 2)];
            if optional {
                want.push((3, NO_ROW));
            }
            assert_eq!(l.into_iter().zip(r).collect::<Vec<_>>(), want);
        }
    }

    /// A run expanded a piece at a time through `expand_run` gives the pairs `expand`
    /// gives it whole, and a run of rejected candidates stops at a cancellation within
    /// one poll of candidates, whatever room the caller gave.
    #[test]
    fn a_run_expands_in_pieces_and_stops_at_a_cancellation() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let lk = ids(&[5]);
        let rk = ids(&vec![5i64; 10_000]);
        let first = bounds(&ctx, &lk, &rk).unwrap();
        let odd = |_: usize, r: usize| r % 2 == 1;
        let (mut l, mut r) = (Vec::new(), Vec::new());
        assert_eq!(
            expand(
                &ctx,
                &lk,
                &rk,
                &first,
                0,
                false,
                rk.len(),
                odd,
                &mut l,
                &mut r
            )
            .unwrap(),
            1
        );
        let (mut pl, mut pr) = (Vec::new(), Vec::new());
        let mut at = 0;
        while at < rk.len() {
            at = expand_run(&ctx, 0, at..rk.len(), 300, odd, &mut pl, &mut pr).unwrap();
        }
        assert_eq!((pl, pr), (l, r));
        let tested = std::cell::Cell::new(0usize);
        let cancelling = |_: usize, _: usize| {
            tested.set(tested.get() + 1);
            if tested.get() == 10 {
                ctx.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            false
        };
        let (mut l, mut r) = (Vec::new(), Vec::new());
        let stopped = expand(
            &ctx,
            &lk,
            &rk,
            &first,
            0,
            false,
            rk.len(),
            cancelling,
            &mut l,
            &mut r,
        );
        assert!(matches!(stopped, Err(Error::Cancelled)), "{stopped:?}");
        assert!(tested.get() <= 10 + POLL, "{}", tested.get());
        let stopped = expand_run(&ctx, 0, 0..rk.len(), rk.len(), cancelling, &mut l, &mut r);
        assert!(matches!(stopped, Err(Error::Cancelled)), "{stopped:?}");
    }

    #[test]
    fn an_empty_right_column_matches_nothing() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let lk = ids(&[1, 2]);
        let first = bounds(&ctx, &lk, &[]).unwrap();
        assert_eq!(count(&ctx, &lk, &[], &first, false).unwrap(), 0);
        assert_eq!(count(&ctx, &lk, &[], &first, true).unwrap(), 2);
        assert_eq!(filter(&lk, &[], &first, false), vec![true, true]);
        let (mut l, mut r) = (Vec::new(), Vec::new());
        assert_eq!(
            expand(
                &ctx,
                &lk,
                &[],
                &first,
                0,
                true,
                10,
                |_, _| true,
                &mut l,
                &mut r
            )
            .unwrap(),
            2
        );
        assert_eq!((l, r), (vec![0, 1], vec![NO_ROW, NO_ROW]));
    }
}
