//! The theory rule for a context that holds only units, as the Horn tier's contexts do
//! (F24, "The EL and Horn tiers").
//!
//! The context's data atoms in one component are one set S. The rule tests S for a
//! conflict, gives each body atom that S entails, gives a class when the remaining atoms
//! whose only negative occurrences are told subsumptions by that class cannot all be
//! false, and says whether a cover remains among the atoms that are not free.

use super::range::Ranges;
use super::solve::{Answer, Atom, Bounds, MemScope, Problem, Why, check_bounded};

/// What a body atom's negative occurrences make of it, which the caller reads from the
/// index and the context's units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    /// every negative occurrence is a told subsumption whose subsumer the context holds,
    /// so the atom's truth changes no conclusion
    Free,
    /// every negative occurrence is a told subsumption by this class or by a unit of
    /// the context
    Only(u32),
    /// any other atom
    Other,
}

/// The conclusions of the rule on one set of units.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Units {
    /// the set is unsatisfiable, so the context is `owl:Nothing`
    pub conflict: bool,
    /// the body atoms the set entails, by index
    pub entailed: Vec<usize>,
    /// the classes given by a cover of atoms that each give the class
    pub classes: Vec<u32>,
    /// a cover remains, which needs a case analysis
    pub cover: bool,
}

/// Whether the units with the negations of `negs` are unsatisfiable.
fn refutes(
    r: &mut Ranges,
    base: &Problem,
    negs: &[Atom],
    b: &Bounds,
    stop: &dyn Fn() -> bool,
    admit_ranges: &dyn Fn(usize) -> bool,
    mem: &dyn Fn(isize) -> bool,
) -> Result<bool, Why> {
    let mut scope = MemScope::new(mem);
    let props_bytes: usize = base
        .props
        .iter()
        .map(|p| {
            std::mem::size_of::<super::solve::Prop>()
                + p.supers.len() * std::mem::size_of::<usize>()
                + p.ranges.len() * 4
        })
        .sum();
    let clone_est = base
        .atoms
        .len()
        .saturating_add(negs.len())
        .saturating_mul(std::mem::size_of::<Atom>())
        .saturating_add(props_bytes)
        .saturating_add(base.disjoint.len().saturating_mul(16));
    if !scope.take(clone_est) {
        return Err(Why::Stopped);
    }
    let mut p = base.clone();
    for &q in negs {
        match q.negate() {
            Some(n) => p.atoms.push(n),
            // the atom holds of every element, so its negation is unsatisfiable
            None => return Ok(true),
        }
    }
    match check_bounded(r, &p, b, stop, admit_ranges, mem) {
        Answer::Sat => Ok(false),
        Answer::Unsat => Ok(true),
        Answer::Unknown(w) => Err(w),
    }
}

/// The rule on the units `base.atoms`, with the component's properties and disjointness
/// in `base`, and the body atoms that the context does not hold with their groups. An
/// answer the checker cannot give is returned as its reason, which the caller reports as
/// used in part, or as a stop when it is [`Why::Stopped`].
pub fn units(
    r: &mut Ranges,
    base: &Problem,
    body: &[(Atom, Group)],
    b: &Bounds,
    stop: &dyn Fn() -> bool,
) -> Result<Units, Why> {
    units_bounded(r, base, body, b, stop, &|_| true, &|_| true)
}

/// [`units`], admitting shared [`Ranges`] growth via `admit_ranges` and scoped temporary
/// solver allocations via `mem` (`+n` reserves, `-n` releases), returning [`Why::Stopped`]
/// on refusal.
pub fn units_bounded(
    r: &mut Ranges,
    base: &Problem,
    body: &[(Atom, Group)],
    b: &Bounds,
    stop: &dyn Fn() -> bool,
    admit_ranges: &dyn Fn(usize) -> bool,
    mem: &dyn Fn(isize) -> bool,
) -> Result<Units, Why> {
    let mut out = Units::default();
    if refutes(r, base, &[], b, stop, admit_ranges, mem)? {
        out.conflict = true;
        return Ok(out);
    }
    let mut body_scope = MemScope::new(mem);
    let body_est = body
        .len()
        .saturating_mul(std::mem::size_of::<(Atom, Group)>() + std::mem::size_of::<usize>() + 16);
    if !body_scope.take(body_est) {
        return Err(Why::Stopped);
    }
    let mut left: Vec<(Atom, Group)> = Vec::new();
    for (i, &(q, g)) in body.iter().enumerate() {
        if refutes(r, base, &[q], b, stop, admit_ranges, mem)? {
            out.entailed.push(i);
        } else if g != Group::Free {
            left.push((q, g));
        }
    }
    if left.len() < 2 {
        return Ok(out);
    }
    let mut classes: Vec<u32> = left
        .iter()
        .filter_map(|&(_, g)| match g {
            Group::Only(c) => Some(c),
            _ => None,
        })
        .collect();
    classes.sort_unstable();
    classes.dedup();
    for c in classes {
        let w: Vec<Atom> = left
            .iter()
            .filter(|&&(_, g)| g == Group::Only(c))
            .map(|&(q, _)| q)
            .collect();
        if w.len() >= 2 && refutes(r, base, &w, b, stop, admit_ranges, mem)? {
            out.classes.push(c);
            left.retain(|&(_, g)| g != Group::Only(c));
        }
    }
    if left.len() >= 2 {
        let w: Vec<Atom> = left.iter().map(|&(q, _)| q).collect();
        out.cover = refutes(r, base, &w, b, stop, admit_ranges, mem)?;
    }
    Ok(out)
}
