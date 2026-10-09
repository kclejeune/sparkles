//! Lower bounds of one sorted id column in another, the first step of merge left joins
//! and merge anti joins.

use super::ctx::Ctx;
use super::exec::gallop;
use crate::error::Result;
use crate::id::Id;

/// Zipper steps between cancellation checks.
const ZIP_CHECK_STEPS: usize = 65536;
/// Zippers run side by side over separate parts of the inputs.
const ZIP_LANES: usize = 4;
/// Inputs with fewer rows than this are merged by one zipper.
const ZIP_LANES_MIN_ROWS: usize = 4096;

/// For each id of the sorted column `a`, the index of the first id of the sorted column
/// `b` that is not less than it (`b.len()` when there is none).
///
/// When `b` is much larger than `a`, it gallops over `b`. Otherwise it runs a branch-free
/// zipper, where each step advances the side with the smaller id. A zipper step depends on
/// the ids that the previous step read, so one zipper waits for a load on every step.
/// Larger inputs are therefore split at [`ZIP_LANES`] points of the merged order, and the
/// independent zippers of the parts advance in the same loop, which lets the CPU overlap
/// their loads. Each part's rows of `b` end at the lower bound of the next part's first
/// row of `a`, which bounds every lower bound inside the part. The inner loops stop for a
/// cancellation check every [`ZIP_CHECK_STEPS`] steps and make no calls.
pub(super) fn lower_bounds(ctx: &Ctx, a: &[Id], b: &[Id]) -> Result<Vec<u32>> {
    let mut first = vec![b.len() as u32; a.len()];
    if b.len() > 8 * a.len() {
        let mut j = 0;
        for (i, &x) in a.iter().enumerate() {
            if j < b.len() && b[j] < x {
                j = gallop(b, j, x);
            }
            first[i] = j as u32;
        }
        return Ok(first);
    }
    if a.len() + b.len() < ZIP_LANES_MIN_ROWS {
        zip_lower_bounds(ctx, a, b, &mut first, 0, 0)?;
        return Ok(first);
    }
    // Part k starts at the first row of `a` at which the zipper has taken k/ZIP_LANES of
    // its a.len() + b.len() steps. The step count before row i is i plus its lower bound.
    let total = a.len() + b.len();
    let mut start = [(0, 0); ZIP_LANES + 1];
    for (k, s) in start.iter_mut().enumerate().skip(1) {
        let target = total * k / ZIP_LANES;
        let (mut lo, mut hi) = (0, a.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if mid + b.partition_point(|y| *y < a[mid]) < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        *s = if lo < a.len() {
            (lo, b.partition_point(|y| *y < a[lo]))
        } else {
            (a.len(), b.len())
        };
    }
    let (mut i, mut j) = ([0; ZIP_LANES], [0; ZIP_LANES]);
    let (mut end_a, mut end_b) = ([0; ZIP_LANES], [0; ZIP_LANES]);
    for k in 0..ZIP_LANES {
        (i[k], j[k]) = start[k];
        (end_a[k], end_b[k]) = start[k + 1];
    }
    // all parts together until one of them runs out of rows on either side
    loop {
        ctx.check()?;
        let mut steps = ZIP_CHECK_STEPS / ZIP_LANES;
        for k in 0..ZIP_LANES {
            steps = steps.min(end_a[k] - i[k]).min(end_b[k] - j[k]);
        }
        if steps == 0 {
            break;
        }
        for _ in 0..steps {
            for k in 0..ZIP_LANES {
                let (x, y) = (a[i[k]], b[j[k]]);
                first[i[k]] = j[k] as u32;
                i[k] += (x <= y) as usize;
                j[k] += (x > y) as usize;
            }
        }
    }
    // then each part's remaining rows alone
    for k in 0..ZIP_LANES {
        let (ea, eb) = (end_a[k], end_b[k]);
        zip_lower_bounds(ctx, &a[..ea], &b[..eb], &mut first[..ea], i[k], j[k])?;
    }
    Ok(first)
}

/// One zipper of [`lower_bounds`] from rows `i` of `a` and `j` of `b`. Rows of `a` left
/// when `b` runs out get `b.len()`.
fn zip_lower_bounds(
    ctx: &Ctx,
    a: &[Id],
    b: &[Id],
    first: &mut [u32],
    mut i: usize,
    mut j: usize,
) -> Result<()> {
    while i < a.len() && j < b.len() {
        ctx.check()?;
        // every step advances i or j by one, so these many steps stay in bounds
        let steps = (a.len() - i).min(b.len() - j).min(ZIP_CHECK_STEPS);
        for _ in 0..steps {
            let (x, y) = (a[i], b[j]);
            first[i] = j as u32;
            i += (x <= y) as usize;
            j += (x > y) as usize;
        }
    }
    first[i..a.len()].fill(b.len() as u32);
    Ok(())
}
