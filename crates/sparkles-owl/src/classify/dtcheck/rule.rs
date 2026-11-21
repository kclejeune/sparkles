//! The theory rule for a context that holds only units, as the Horn tier's contexts do
//! (F24, "The EL and Horn tiers").
//!
//! The context's data atoms in one component are one set S. The rule tests S for a
//! conflict, gives each body atom that S entails, gives a class when the remaining atoms
//! whose only negative occurrences are told subsumptions by that class cannot all be
//! false, and says whether a cover remains among the atoms that are not free.

use super::range::Ranges;
use super::solve::{Answer, Atom, Bounds, Problem, Why, check_stopping};

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
) -> Result<bool, Why> {
    let mut p = base.clone();
    for &q in negs {
        match q.negate() {
            Some(n) => p.atoms.push(n),
            // the atom holds of every element, so its negation is unsatisfiable
            None => return Ok(true),
        }
    }
    match check_stopping(r, &p, b, stop) {
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
    let mut out = Units::default();
    if refutes(r, base, &[], b, stop)? {
        out.conflict = true;
        return Ok(out);
    }
    let mut left: Vec<(Atom, Group)> = Vec::new();
    for (i, &(q, g)) in body.iter().enumerate() {
        if refutes(r, base, &[q], b, stop)? {
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
        if w.len() >= 2 && refutes(r, base, &w, b, stop)? {
            out.classes.push(c);
            left.retain(|&(_, g)| g != Group::Only(c));
        }
    }
    if left.len() >= 2 {
        let w: Vec<Atom> = left.iter().map(|&(q, _)| q).collect();
        out.cover = refutes(r, base, &w, b, stop)?;
    }
    Ok(out)
}
