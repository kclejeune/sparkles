//! Cooperatively polled structural hashing, equality, ordering, and cached-hash sets for
//! [`AxiomKind`] and [`ClassExpression`]. Resizing uses only cached `u64` hashes without
//! structurally rehashing keys, and all structural traversals poll the caller's budget
//! every [`RANGE_POLL_STEP`] steps.

use super::{Error, RANGE_POLL_STEP, ScratchBudget, hash_table_bytes};
use crate::model::*;
use hashbrown::HashTable;
use rustc_hash::FxHasher;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

const STR_CHUNK_BYTES: usize = 4096;

#[inline]
fn tick(steps: &mut usize, budget: &impl ScratchBudget) -> Result<(), Error> {
    *steps += 1;
    if (*steps).is_multiple_of(RANGE_POLL_STEP) {
        budget.poll()?;
    }
    Ok(())
}

fn hash_str_polled(
    s: &str,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    s.len().hash(h);
    if s.is_empty() {
        return tick(steps, budget);
    }
    for chunk in s.as_bytes().chunks(STR_CHUNK_BYTES) {
        tick(steps, budget)?;
        chunk.hash(h);
    }
    Ok(())
}

fn eq_str_polled(
    a: &str,
    b: &str,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    if a.len() != b.len() {
        tick(steps, budget)?;
        return Ok(false);
    }
    if std::ptr::eq(a.as_ptr(), b.as_ptr()) || a.is_empty() {
        tick(steps, budget)?;
        return Ok(true);
    }
    for (ca, cb) in a
        .as_bytes()
        .chunks(STR_CHUNK_BYTES)
        .zip(b.as_bytes().chunks(STR_CHUNK_BYTES))
    {
        tick(steps, budget)?;
        if ca != cb {
            return Ok(false);
        }
    }
    Ok(true)
}

fn cmp_str_polled(
    a: &str,
    b: &str,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    if a.len() == b.len() && (std::ptr::eq(a.as_ptr(), b.as_ptr()) || a.is_empty()) {
        tick(steps, budget)?;
        return Ok(Ordering::Equal);
    }
    let mut ia = a.as_bytes().chunks(STR_CHUNK_BYTES);
    let mut ib = b.as_bytes().chunks(STR_CHUNK_BYTES);
    loop {
        match (ia.next(), ib.next()) {
            (Some(ca), Some(cb)) => {
                tick(steps, budget)?;
                let ord = ca.cmp(cb);
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
            }
            (None, None) => return Ok(Ordering::Equal),
            (None, Some(_)) => return Ok(Ordering::Less),
            (Some(_), None) => return Ok(Ordering::Greater),
        }
    }
}

fn hash_ind_polled(
    ind: &Individual,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    match ind {
        Individual::Named(n) => {
            0u8.hash(h);
            hash_str_polled(n.as_str(), h, steps, budget)
        }
        Individual::Anonymous(a) => {
            1u8.hash(h);
            hash_str_polled(a.label(), h, steps, budget)
        }
    }
}

fn eq_ind_polled(
    a: &Individual,
    b: &Individual,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    match (a, b) {
        (Individual::Named(x), Individual::Named(y)) => {
            eq_str_polled(x.as_str(), y.as_str(), steps, budget)
        }
        (Individual::Anonymous(x), Individual::Anonymous(y)) => {
            eq_str_polled(x.label(), y.label(), steps, budget)
        }
        _ => Ok(false),
    }
}

fn cmp_ind_polled(
    a: &Individual,
    b: &Individual,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    match (a, b) {
        (Individual::Named(x), Individual::Named(y)) => {
            cmp_str_polled(x.as_str(), y.as_str(), steps, budget)
        }
        (Individual::Anonymous(x), Individual::Anonymous(y)) => {
            cmp_str_polled(x.label(), y.label(), steps, budget)
        }
        (Individual::Named(_), Individual::Anonymous(_)) => Ok(Ordering::Less),
        (Individual::Anonymous(_), Individual::Named(_)) => Ok(Ordering::Greater),
    }
}

fn hash_lit_polled(
    l: &Literal,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    hash_str_polled(l.value(), h, steps, budget)?;
    hash_str_polled(l.datatype(), h, steps, budget)?;
    match l.language() {
        None => 0u8.hash(h),
        Some(lang) => {
            1u8.hash(h);
            hash_str_polled(lang, h, steps, budget)?;
        }
    }
    l.as_rdf().direction().hash(h);
    Ok(())
}

fn eq_lit_polled(
    a: &Literal,
    b: &Literal,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    if a.as_rdf().direction() != b.as_rdf().direction() {
        tick(steps, budget)?;
        return Ok(false);
    }
    if !eq_str_polled(a.value(), b.value(), steps, budget)? {
        return Ok(false);
    }
    if !eq_str_polled(a.datatype(), b.datatype(), steps, budget)? {
        return Ok(false);
    }
    match (a.language(), b.language()) {
        (None, None) => Ok(true),
        (Some(x), Some(y)) => eq_str_polled(x, y, steps, budget),
        _ => Ok(false),
    }
}

fn cmp_lit_polled(
    a: &Literal,
    b: &Literal,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    let ord = cmp_str_polled(a.value(), b.value(), steps, budget)?;
    if ord != Ordering::Equal {
        return Ok(ord);
    }
    let ord = cmp_str_polled(a.datatype(), b.datatype(), steps, budget)?;
    if ord != Ordering::Equal {
        return Ok(ord);
    }
    match (a.language(), b.language()) {
        (None, None) => Ok(Ordering::Equal),
        (None, Some(_)) => Ok(Ordering::Less),
        (Some(_), None) => Ok(Ordering::Greater),
        (Some(x), Some(y)) => cmp_str_polled(x, y, steps, budget),
    }
}

fn hash_ope_polled(
    p: &ObjectPropertyExpression,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    match p {
        ObjectPropertyExpression::Property(p) => {
            0u8.hash(h);
            hash_str_polled(p.as_str(), h, steps, budget)
        }
        ObjectPropertyExpression::InverseOf(p) => {
            1u8.hash(h);
            hash_str_polled(p.as_str(), h, steps, budget)
        }
    }
}

fn eq_ope_polled(
    a: &ObjectPropertyExpression,
    b: &ObjectPropertyExpression,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    match (a, b) {
        (ObjectPropertyExpression::Property(x), ObjectPropertyExpression::Property(y))
        | (ObjectPropertyExpression::InverseOf(x), ObjectPropertyExpression::InverseOf(y)) => {
            eq_str_polled(x.as_str(), y.as_str(), steps, budget)
        }
        _ => Ok(false),
    }
}

fn cmp_ope_polled(
    a: &ObjectPropertyExpression,
    b: &ObjectPropertyExpression,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    match (a, b) {
        (ObjectPropertyExpression::Property(x), ObjectPropertyExpression::Property(y))
        | (ObjectPropertyExpression::InverseOf(x), ObjectPropertyExpression::InverseOf(y)) => {
            cmp_str_polled(x.as_str(), y.as_str(), steps, budget)
        }
        (ObjectPropertyExpression::Property(_), ObjectPropertyExpression::InverseOf(_)) => {
            Ok(Ordering::Less)
        }
        (ObjectPropertyExpression::InverseOf(_), ObjectPropertyExpression::Property(_)) => {
            Ok(Ordering::Greater)
        }
    }
}

fn hash_facet_polled(
    f: &FacetRestriction,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    hash_str_polled(f.facet.as_str(), h, steps, budget)?;
    hash_lit_polled(&f.value, h, steps, budget)
}

fn eq_facet_polled(
    a: &FacetRestriction,
    b: &FacetRestriction,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    Ok(
        eq_str_polled(a.facet.as_str(), b.facet.as_str(), steps, budget)?
            && eq_lit_polled(&a.value, &b.value, steps, budget)?,
    )
}

fn cmp_facet_polled(
    a: &FacetRestriction,
    b: &FacetRestriction,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    let ord = cmp_str_polled(a.facet.as_str(), b.facet.as_str(), steps, budget)?;
    if ord != Ordering::Equal {
        return Ok(ord);
    }
    cmp_lit_polled(&a.value, &b.value, steps, budget)
}

fn hash_dr_polled(
    r: &DataRange,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    tick(steps, budget)?;
    match r {
        DataRange::Datatype(d) => {
            0u8.hash(h);
            hash_str_polled(d.as_str(), h, steps, budget)
        }
        DataRange::IntersectionOf(rs) => {
            1u8.hash(h);
            rs.len().hash(h);
            for x in rs {
                hash_dr_polled(x, h, steps, budget)?;
            }
            Ok(())
        }
        DataRange::UnionOf(rs) => {
            2u8.hash(h);
            rs.len().hash(h);
            for x in rs {
                hash_dr_polled(x, h, steps, budget)?;
            }
            Ok(())
        }
        DataRange::ComplementOf(x) => {
            3u8.hash(h);
            hash_dr_polled(x, h, steps, budget)
        }
        DataRange::OneOf(ls) => {
            4u8.hash(h);
            ls.len().hash(h);
            for l in ls {
                hash_lit_polled(l, h, steps, budget)?;
            }
            Ok(())
        }
        DataRange::DatatypeRestriction(d, fs) => {
            5u8.hash(h);
            hash_str_polled(d.as_str(), h, steps, budget)?;
            fs.len().hash(h);
            for f in fs {
                hash_facet_polled(f, h, steps, budget)?;
            }
            Ok(())
        }
    }
}

fn eq_dr_polled(
    a: &DataRange,
    b: &DataRange,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    if std::ptr::eq(a, b) {
        return Ok(true);
    }
    tick(steps, budget)?;
    match (a, b) {
        (DataRange::Datatype(x), DataRange::Datatype(y)) => {
            eq_str_polled(x.as_str(), y.as_str(), steps, budget)
        }
        (DataRange::IntersectionOf(xs), DataRange::IntersectionOf(ys))
        | (DataRange::UnionOf(xs), DataRange::UnionOf(ys)) => {
            eq_set_polled(xs, ys, steps, budget, eq_dr_polled)
        }
        (DataRange::ComplementOf(x), DataRange::ComplementOf(y)) => {
            eq_dr_polled(x, y, steps, budget)
        }
        (DataRange::OneOf(xs), DataRange::OneOf(ys)) => {
            eq_set_polled(xs, ys, steps, budget, eq_lit_polled)
        }
        (DataRange::DatatypeRestriction(d1, fs1), DataRange::DatatypeRestriction(d2, fs2)) => {
            Ok(eq_str_polled(d1.as_str(), d2.as_str(), steps, budget)?
                && eq_set_polled(fs1, fs2, steps, budget, eq_facet_polled)?)
        }
        _ => Ok(false),
    }
}

fn dr_disc(r: &DataRange) -> u8 {
    match r {
        DataRange::Datatype(_) => 0,
        DataRange::IntersectionOf(_) => 1,
        DataRange::UnionOf(_) => 2,
        DataRange::ComplementOf(_) => 3,
        DataRange::OneOf(_) => 4,
        DataRange::DatatypeRestriction(..) => 5,
    }
}

fn cmp_dr_polled(
    a: &DataRange,
    b: &DataRange,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    if std::ptr::eq(a, b) {
        return Ok(Ordering::Equal);
    }
    tick(steps, budget)?;
    let ord = dr_disc(a).cmp(&dr_disc(b));
    if ord != Ordering::Equal {
        return Ok(ord);
    }
    match (a, b) {
        (DataRange::Datatype(x), DataRange::Datatype(y)) => {
            cmp_str_polled(x.as_str(), y.as_str(), steps, budget)
        }
        (DataRange::IntersectionOf(xs), DataRange::IntersectionOf(ys))
        | (DataRange::UnionOf(xs), DataRange::UnionOf(ys)) => {
            cmp_set_polled(xs, ys, steps, budget, cmp_dr_polled)
        }
        (DataRange::ComplementOf(x), DataRange::ComplementOf(y)) => {
            cmp_dr_polled(x, y, steps, budget)
        }
        (DataRange::OneOf(xs), DataRange::OneOf(ys)) => {
            cmp_set_polled(xs, ys, steps, budget, cmp_lit_polled)
        }
        (DataRange::DatatypeRestriction(d1, fs1), DataRange::DatatypeRestriction(d2, fs2)) => {
            let ord = cmp_str_polled(d1.as_str(), d2.as_str(), steps, budget)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            cmp_set_polled(fs1, fs2, steps, budget, cmp_facet_polled)
        }
        _ => unreachable!(),
    }
}

fn eq_set_polled<T, B: ScratchBudget>(
    a: &BTreeSet<T>,
    b: &BTreeSet<T>,
    steps: &mut usize,
    budget: &B,
    eq: fn(&T, &T, &mut usize, &B) -> Result<bool, Error>,
) -> Result<bool, Error> {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (x, y) in a.iter().zip(b.iter()) {
        if !eq(x, y, steps, budget)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn cmp_set_polled<T, B: ScratchBudget>(
    a: &BTreeSet<T>,
    b: &BTreeSet<T>,
    steps: &mut usize,
    budget: &B,
    cmp: fn(&T, &T, &mut usize, &B) -> Result<Ordering, Error>,
) -> Result<Ordering, Error> {
    let mut ia = a.iter();
    let mut ib = b.iter();
    loop {
        match (ia.next(), ib.next()) {
            (Some(x), Some(y)) => {
                let ord = cmp(x, y, steps, budget)?;
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
            }
            (None, None) => return Ok(Ordering::Equal),
            (None, Some(_)) => return Ok(Ordering::Less),
            (Some(_), None) => return Ok(Ordering::Greater),
        }
    }
}

fn eq_slice_polled<T, B: ScratchBudget>(
    a: &[T],
    b: &[T],
    steps: &mut usize,
    budget: &B,
    eq: fn(&T, &T, &mut usize, &B) -> Result<bool, Error>,
) -> Result<bool, Error> {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (x, y) in a.iter().zip(b.iter()) {
        if !eq(x, y, steps, budget)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn cmp_slice_polled<T, B: ScratchBudget>(
    a: &[T],
    b: &[T],
    steps: &mut usize,
    budget: &B,
    cmp: fn(&T, &T, &mut usize, &B) -> Result<Ordering, Error>,
) -> Result<Ordering, Error> {
    let mut ia = a.iter();
    let mut ib = b.iter();
    loop {
        match (ia.next(), ib.next()) {
            (Some(x), Some(y)) => {
                let ord = cmp(x, y, steps, budget)?;
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
            }
            (None, None) => return Ok(Ordering::Equal),
            (None, Some(_)) => return Ok(Ordering::Less),
            (Some(_), None) => return Ok(Ordering::Greater),
        }
    }
}

fn eq_dp_polled(
    a: &DataProperty,
    b: &DataProperty,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    eq_str_polled(a.as_str(), b.as_str(), steps, budget)
}

fn cmp_dp_polled(
    a: &DataProperty,
    b: &DataProperty,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    cmp_str_polled(a.as_str(), b.as_str(), steps, budget)
}

fn hash_ce_polled(
    c: &ClassExpression,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    use ClassExpression as C;
    tick(steps, budget)?;
    match c {
        C::Class(x) => {
            0u8.hash(h);
            hash_str_polled(x.as_str(), h, steps, budget)
        }
        C::ObjectIntersectionOf(cs) => {
            1u8.hash(h);
            cs.len().hash(h);
            for x in cs {
                hash_ce_polled(x, h, steps, budget)?;
            }
            Ok(())
        }
        C::ObjectUnionOf(cs) => {
            2u8.hash(h);
            cs.len().hash(h);
            for x in cs {
                hash_ce_polled(x, h, steps, budget)?;
            }
            Ok(())
        }
        C::ObjectComplementOf(x) => {
            3u8.hash(h);
            hash_ce_polled(x, h, steps, budget)
        }
        C::ObjectOneOf(is) => {
            4u8.hash(h);
            is.len().hash(h);
            for i in is {
                hash_ind_polled(i, h, steps, budget)?;
            }
            Ok(())
        }
        C::ObjectSomeValuesFrom(p, x) => {
            5u8.hash(h);
            hash_ope_polled(p, h, steps, budget)?;
            hash_ce_polled(x, h, steps, budget)
        }
        C::ObjectAllValuesFrom(p, x) => {
            6u8.hash(h);
            hash_ope_polled(p, h, steps, budget)?;
            hash_ce_polled(x, h, steps, budget)
        }
        C::ObjectHasValue(p, i) => {
            7u8.hash(h);
            hash_ope_polled(p, h, steps, budget)?;
            hash_ind_polled(i, h, steps, budget)
        }
        C::ObjectHasSelf(p) => {
            8u8.hash(h);
            hash_ope_polled(p, h, steps, budget)
        }
        C::ObjectMinCardinality(n, p, f) => {
            9u8.hash(h);
            n.hash(h);
            hash_ope_polled(p, h, steps, budget)?;
            match f.as_deref() {
                None => 0u8.hash(h),
                Some(x) => {
                    1u8.hash(h);
                    hash_ce_polled(x, h, steps, budget)?;
                }
            }
            Ok(())
        }
        C::ObjectMaxCardinality(n, p, f) => {
            10u8.hash(h);
            n.hash(h);
            hash_ope_polled(p, h, steps, budget)?;
            match f.as_deref() {
                None => 0u8.hash(h),
                Some(x) => {
                    1u8.hash(h);
                    hash_ce_polled(x, h, steps, budget)?;
                }
            }
            Ok(())
        }
        C::ObjectExactCardinality(n, p, f) => {
            11u8.hash(h);
            n.hash(h);
            hash_ope_polled(p, h, steps, budget)?;
            match f.as_deref() {
                None => 0u8.hash(h),
                Some(x) => {
                    1u8.hash(h);
                    hash_ce_polled(x, h, steps, budget)?;
                }
            }
            Ok(())
        }
        C::DataSomeValuesFrom(ps, r) => {
            12u8.hash(h);
            ps.len().hash(h);
            for p in ps {
                hash_str_polled(p.as_str(), h, steps, budget)?;
            }
            hash_dr_polled(r, h, steps, budget)
        }
        C::DataAllValuesFrom(ps, r) => {
            13u8.hash(h);
            ps.len().hash(h);
            for p in ps {
                hash_str_polled(p.as_str(), h, steps, budget)?;
            }
            hash_dr_polled(r, h, steps, budget)
        }
        C::DataHasValue(p, l) => {
            14u8.hash(h);
            hash_str_polled(p.as_str(), h, steps, budget)?;
            hash_lit_polled(l, h, steps, budget)
        }
        C::DataMinCardinality(n, p, r) => {
            15u8.hash(h);
            n.hash(h);
            hash_str_polled(p.as_str(), h, steps, budget)?;
            match r {
                None => 0u8.hash(h),
                Some(x) => {
                    1u8.hash(h);
                    hash_dr_polled(x, h, steps, budget)?;
                }
            }
            Ok(())
        }
        C::DataMaxCardinality(n, p, r) => {
            16u8.hash(h);
            n.hash(h);
            hash_str_polled(p.as_str(), h, steps, budget)?;
            match r {
                None => 0u8.hash(h),
                Some(x) => {
                    1u8.hash(h);
                    hash_dr_polled(x, h, steps, budget)?;
                }
            }
            Ok(())
        }
        C::DataExactCardinality(n, p, r) => {
            17u8.hash(h);
            n.hash(h);
            hash_str_polled(p.as_str(), h, steps, budget)?;
            match r {
                None => 0u8.hash(h),
                Some(x) => {
                    1u8.hash(h);
                    hash_dr_polled(x, h, steps, budget)?;
                }
            }
            Ok(())
        }
    }
}

fn eq_opt_ce_polled(
    a: Option<&ClassExpression>,
    b: Option<&ClassExpression>,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    match (a, b) {
        (None, None) => Ok(true),
        (Some(x), Some(y)) => eq_ce_polled(x, y, steps, budget),
        _ => Ok(false),
    }
}

fn cmp_opt_ce_polled(
    a: Option<&ClassExpression>,
    b: Option<&ClassExpression>,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    match (a, b) {
        (None, None) => Ok(Ordering::Equal),
        (None, Some(_)) => Ok(Ordering::Less),
        (Some(_), None) => Ok(Ordering::Greater),
        (Some(x), Some(y)) => cmp_ce_polled(x, y, steps, budget),
    }
}

fn eq_opt_dr_polled(
    a: Option<&DataRange>,
    b: Option<&DataRange>,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    match (a, b) {
        (None, None) => Ok(true),
        (Some(x), Some(y)) => eq_dr_polled(x, y, steps, budget),
        _ => Ok(false),
    }
}

fn cmp_opt_dr_polled(
    a: Option<&DataRange>,
    b: Option<&DataRange>,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    match (a, b) {
        (None, None) => Ok(Ordering::Equal),
        (None, Some(_)) => Ok(Ordering::Less),
        (Some(_), None) => Ok(Ordering::Greater),
        (Some(x), Some(y)) => cmp_dr_polled(x, y, steps, budget),
    }
}

fn eq_ce_polled(
    a: &ClassExpression,
    b: &ClassExpression,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    use ClassExpression as C;
    if std::ptr::eq(a, b) {
        return Ok(true);
    }
    tick(steps, budget)?;
    match (a, b) {
        (C::Class(x), C::Class(y)) => eq_str_polled(x.as_str(), y.as_str(), steps, budget),
        (C::ObjectIntersectionOf(xs), C::ObjectIntersectionOf(ys))
        | (C::ObjectUnionOf(xs), C::ObjectUnionOf(ys)) => {
            eq_set_polled(xs, ys, steps, budget, eq_ce_polled)
        }
        (C::ObjectComplementOf(x), C::ObjectComplementOf(y)) => eq_ce_polled(x, y, steps, budget),
        (C::ObjectOneOf(xs), C::ObjectOneOf(ys)) => {
            eq_set_polled(xs, ys, steps, budget, eq_ind_polled)
        }
        (C::ObjectSomeValuesFrom(p1, x), C::ObjectSomeValuesFrom(p2, y))
        | (C::ObjectAllValuesFrom(p1, x), C::ObjectAllValuesFrom(p2, y)) => {
            Ok(eq_ope_polled(p1, p2, steps, budget)? && eq_ce_polled(x, y, steps, budget)?)
        }
        (C::ObjectHasValue(p1, i1), C::ObjectHasValue(p2, i2)) => {
            Ok(eq_ope_polled(p1, p2, steps, budget)? && eq_ind_polled(i1, i2, steps, budget)?)
        }
        (C::ObjectHasSelf(p1), C::ObjectHasSelf(p2)) => eq_ope_polled(p1, p2, steps, budget),
        (C::ObjectMinCardinality(n1, p1, f1), C::ObjectMinCardinality(n2, p2, f2))
        | (C::ObjectMaxCardinality(n1, p1, f1), C::ObjectMaxCardinality(n2, p2, f2))
        | (C::ObjectExactCardinality(n1, p1, f1), C::ObjectExactCardinality(n2, p2, f2)) => Ok(n1
            == n2
            && eq_ope_polled(p1, p2, steps, budget)?
            && eq_opt_ce_polled(f1.as_deref(), f2.as_deref(), steps, budget)?),
        (C::DataSomeValuesFrom(ps1, r1), C::DataSomeValuesFrom(ps2, r2))
        | (C::DataAllValuesFrom(ps1, r1), C::DataAllValuesFrom(ps2, r2)) => {
            Ok(eq_slice_polled(ps1, ps2, steps, budget, eq_dp_polled)?
                && eq_dr_polled(r1, r2, steps, budget)?)
        }
        (C::DataHasValue(p1, l1), C::DataHasValue(p2, l2)) => {
            Ok(eq_dp_polled(p1, p2, steps, budget)? && eq_lit_polled(l1, l2, steps, budget)?)
        }
        (C::DataMinCardinality(n1, p1, r1), C::DataMinCardinality(n2, p2, r2))
        | (C::DataMaxCardinality(n1, p1, r1), C::DataMaxCardinality(n2, p2, r2))
        | (C::DataExactCardinality(n1, p1, r1), C::DataExactCardinality(n2, p2, r2)) => Ok(n1
            == n2
            && eq_dp_polled(p1, p2, steps, budget)?
            && eq_opt_dr_polled(r1.as_ref(), r2.as_ref(), steps, budget)?),
        _ => Ok(false),
    }
}

fn ce_disc(c: &ClassExpression) -> u8 {
    use ClassExpression as C;
    match c {
        C::Class(_) => 0,
        C::ObjectIntersectionOf(_) => 1,
        C::ObjectUnionOf(_) => 2,
        C::ObjectComplementOf(_) => 3,
        C::ObjectOneOf(_) => 4,
        C::ObjectSomeValuesFrom(..) => 5,
        C::ObjectAllValuesFrom(..) => 6,
        C::ObjectHasValue(..) => 7,
        C::ObjectHasSelf(_) => 8,
        C::ObjectMinCardinality(..) => 9,
        C::ObjectMaxCardinality(..) => 10,
        C::ObjectExactCardinality(..) => 11,
        C::DataSomeValuesFrom(..) => 12,
        C::DataAllValuesFrom(..) => 13,
        C::DataHasValue(..) => 14,
        C::DataMinCardinality(..) => 15,
        C::DataMaxCardinality(..) => 16,
        C::DataExactCardinality(..) => 17,
    }
}

/// Cooperatively polled equivalent of [`ClassExpression::cmp`].
pub(super) fn cmp_ce_polled(
    a: &ClassExpression,
    b: &ClassExpression,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<Ordering, Error> {
    use ClassExpression as C;
    if std::ptr::eq(a, b) {
        return Ok(Ordering::Equal);
    }
    tick(steps, budget)?;
    let ord = ce_disc(a).cmp(&ce_disc(b));
    if ord != Ordering::Equal {
        return Ok(ord);
    }
    match (a, b) {
        (C::Class(x), C::Class(y)) => cmp_str_polled(x.as_str(), y.as_str(), steps, budget),
        (C::ObjectIntersectionOf(xs), C::ObjectIntersectionOf(ys))
        | (C::ObjectUnionOf(xs), C::ObjectUnionOf(ys)) => {
            cmp_set_polled(xs, ys, steps, budget, cmp_ce_polled)
        }
        (C::ObjectComplementOf(x), C::ObjectComplementOf(y)) => cmp_ce_polled(x, y, steps, budget),
        (C::ObjectOneOf(xs), C::ObjectOneOf(ys)) => {
            cmp_set_polled(xs, ys, steps, budget, cmp_ind_polled)
        }
        (C::ObjectSomeValuesFrom(p1, x), C::ObjectSomeValuesFrom(p2, y))
        | (C::ObjectAllValuesFrom(p1, x), C::ObjectAllValuesFrom(p2, y)) => {
            let ord = cmp_ope_polled(p1, p2, steps, budget)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            cmp_ce_polled(x, y, steps, budget)
        }
        (C::ObjectHasValue(p1, i1), C::ObjectHasValue(p2, i2)) => {
            let ord = cmp_ope_polled(p1, p2, steps, budget)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            cmp_ind_polled(i1, i2, steps, budget)
        }
        (C::ObjectHasSelf(p1), C::ObjectHasSelf(p2)) => cmp_ope_polled(p1, p2, steps, budget),
        (C::ObjectMinCardinality(n1, p1, f1), C::ObjectMinCardinality(n2, p2, f2))
        | (C::ObjectMaxCardinality(n1, p1, f1), C::ObjectMaxCardinality(n2, p2, f2))
        | (C::ObjectExactCardinality(n1, p1, f1), C::ObjectExactCardinality(n2, p2, f2)) => {
            let ord = n1.cmp(n2);
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            let ord = cmp_ope_polled(p1, p2, steps, budget)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            cmp_opt_ce_polled(f1.as_deref(), f2.as_deref(), steps, budget)
        }
        (C::DataSomeValuesFrom(ps1, r1), C::DataSomeValuesFrom(ps2, r2))
        | (C::DataAllValuesFrom(ps1, r1), C::DataAllValuesFrom(ps2, r2)) => {
            let ord = cmp_slice_polled(ps1, ps2, steps, budget, cmp_dp_polled)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            cmp_dr_polled(r1, r2, steps, budget)
        }
        (C::DataHasValue(p1, l1), C::DataHasValue(p2, l2)) => {
            let ord = cmp_dp_polled(p1, p2, steps, budget)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            cmp_lit_polled(l1, l2, steps, budget)
        }
        (C::DataMinCardinality(n1, p1, r1), C::DataMinCardinality(n2, p2, r2))
        | (C::DataMaxCardinality(n1, p1, r1), C::DataMaxCardinality(n2, p2, r2))
        | (C::DataExactCardinality(n1, p1, r1), C::DataExactCardinality(n2, p2, r2)) => {
            let ord = n1.cmp(n2);
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            let ord = cmp_dp_polled(p1, p2, steps, budget)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            cmp_opt_dr_polled(r1.as_ref(), r2.as_ref(), steps, budget)
        }
        _ => unreachable!(),
    }
}

fn hash_entity_polled(
    e: &Entity,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    (e.kind() as u8).hash(h);
    hash_str_polled(e.iri().as_str(), h, steps, budget)
}

fn eq_entity_polled(
    a: &Entity,
    b: &Entity,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    Ok(a.kind() == b.kind() && eq_str_polled(a.iri().as_str(), b.iri().as_str(), steps, budget)?)
}

fn hash_sub_ope_polled(
    s: &SubObjectPropertyExpression,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    match s {
        SubObjectPropertyExpression::Expression(p) => {
            0u8.hash(h);
            hash_ope_polled(p, h, steps, budget)
        }
        SubObjectPropertyExpression::Chain(ps) => {
            1u8.hash(h);
            ps.len().hash(h);
            for p in ps {
                hash_ope_polled(p, h, steps, budget)?;
            }
            Ok(())
        }
    }
}

fn eq_sub_ope_polled(
    a: &SubObjectPropertyExpression,
    b: &SubObjectPropertyExpression,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    match (a, b) {
        (
            SubObjectPropertyExpression::Expression(x),
            SubObjectPropertyExpression::Expression(y),
        ) => eq_ope_polled(x, y, steps, budget),
        (SubObjectPropertyExpression::Chain(xs), SubObjectPropertyExpression::Chain(ys)) => {
            eq_slice_polled(xs, ys, steps, budget, eq_ope_polled)
        }
        _ => Ok(false),
    }
}

fn hash_ann_subject_polled(
    s: &AnnotationSubject,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    match s {
        AnnotationSubject::Iri(i) => {
            0u8.hash(h);
            hash_str_polled(i.as_str(), h, steps, budget)
        }
        AnnotationSubject::Anonymous(a) => {
            1u8.hash(h);
            hash_str_polled(a.label(), h, steps, budget)
        }
    }
}

fn eq_ann_subject_polled(
    a: &AnnotationSubject,
    b: &AnnotationSubject,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    match (a, b) {
        (AnnotationSubject::Iri(x), AnnotationSubject::Iri(y)) => {
            eq_str_polled(x.as_str(), y.as_str(), steps, budget)
        }
        (AnnotationSubject::Anonymous(x), AnnotationSubject::Anonymous(y)) => {
            eq_str_polled(x.label(), y.label(), steps, budget)
        }
        _ => Ok(false),
    }
}

fn hash_ann_value_polled(
    v: &AnnotationValue,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    match v {
        AnnotationValue::Iri(i) => {
            0u8.hash(h);
            hash_str_polled(i.as_str(), h, steps, budget)
        }
        AnnotationValue::Literal(l) => {
            1u8.hash(h);
            hash_lit_polled(l, h, steps, budget)
        }
        AnnotationValue::Anonymous(a) => {
            2u8.hash(h);
            hash_str_polled(a.label(), h, steps, budget)
        }
    }
}

fn eq_ann_value_polled(
    a: &AnnotationValue,
    b: &AnnotationValue,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    match (a, b) {
        (AnnotationValue::Iri(x), AnnotationValue::Iri(y)) => {
            eq_str_polled(x.as_str(), y.as_str(), steps, budget)
        }
        (AnnotationValue::Literal(x), AnnotationValue::Literal(y)) => {
            eq_lit_polled(x, y, steps, budget)
        }
        (AnnotationValue::Anonymous(x), AnnotationValue::Anonymous(y)) => {
            eq_str_polled(x.label(), y.label(), steps, budget)
        }
        _ => Ok(false),
    }
}

fn hash_axiom_polled(
    ax: &AxiomKind,
    h: &mut FxHasher,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<(), Error> {
    use AxiomKind as K;
    tick(steps, budget)?;
    (ax.axiom_type() as u8).hash(h);
    match ax {
        K::Declaration(e) => hash_entity_polled(e, h, steps, budget),
        K::SubClassOf(a, b) => {
            hash_ce_polled(a, h, steps, budget)?;
            hash_ce_polled(b, h, steps, budget)
        }
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) => {
            cs.len().hash(h);
            for c in cs {
                hash_ce_polled(c, h, steps, budget)?;
            }
            Ok(())
        }
        K::DisjointUnion(c, cs) => {
            hash_str_polled(c.as_str(), h, steps, budget)?;
            cs.len().hash(h);
            for x in cs {
                hash_ce_polled(x, h, steps, budget)?;
            }
            Ok(())
        }
        K::SubObjectPropertyOf(sub, sup) => {
            hash_sub_ope_polled(sub, h, steps, budget)?;
            hash_ope_polled(sup, h, steps, budget)
        }
        K::EquivalentObjectProperties(ps) | K::DisjointObjectProperties(ps) => {
            ps.len().hash(h);
            for p in ps {
                hash_ope_polled(p, h, steps, budget)?;
            }
            Ok(())
        }
        K::InverseObjectProperties(a, b) => {
            hash_ope_polled(a, h, steps, budget)?;
            hash_ope_polled(b, h, steps, budget)
        }
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
            hash_ope_polled(p, h, steps, budget)?;
            hash_ce_polled(c, h, steps, budget)
        }
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => hash_ope_polled(p, h, steps, budget),
        K::SubDataPropertyOf(a, b) => {
            hash_str_polled(a.as_str(), h, steps, budget)?;
            hash_str_polled(b.as_str(), h, steps, budget)
        }
        K::EquivalentDataProperties(ps) | K::DisjointDataProperties(ps) => {
            ps.len().hash(h);
            for p in ps {
                hash_str_polled(p.as_str(), h, steps, budget)?;
            }
            Ok(())
        }
        K::DataPropertyDomain(p, c) => {
            hash_str_polled(p.as_str(), h, steps, budget)?;
            hash_ce_polled(c, h, steps, budget)
        }
        K::DataPropertyRange(p, r) => {
            hash_str_polled(p.as_str(), h, steps, budget)?;
            hash_dr_polled(r, h, steps, budget)
        }
        K::FunctionalDataProperty(p) => hash_str_polled(p.as_str(), h, steps, budget),
        K::DatatypeDefinition(d, r) => {
            hash_str_polled(d.as_str(), h, steps, budget)?;
            hash_dr_polled(r, h, steps, budget)
        }
        K::HasKey(c, ops, dps) => {
            hash_ce_polled(c, h, steps, budget)?;
            ops.len().hash(h);
            for p in ops {
                hash_ope_polled(p, h, steps, budget)?;
            }
            dps.len().hash(h);
            for p in dps {
                hash_str_polled(p.as_str(), h, steps, budget)?;
            }
            Ok(())
        }
        K::SameIndividual(is) | K::DifferentIndividuals(is) => {
            is.len().hash(h);
            for i in is {
                hash_ind_polled(i, h, steps, budget)?;
            }
            Ok(())
        }
        K::ClassAssertion(c, i) => {
            hash_ce_polled(c, h, steps, budget)?;
            hash_ind_polled(i, h, steps, budget)
        }
        K::ObjectPropertyAssertion(p, a, b) | K::NegativeObjectPropertyAssertion(p, a, b) => {
            hash_ope_polled(p, h, steps, budget)?;
            hash_ind_polled(a, h, steps, budget)?;
            hash_ind_polled(b, h, steps, budget)
        }
        K::DataPropertyAssertion(p, a, l) | K::NegativeDataPropertyAssertion(p, a, l) => {
            hash_str_polled(p.as_str(), h, steps, budget)?;
            hash_ind_polled(a, h, steps, budget)?;
            hash_lit_polled(l, h, steps, budget)
        }
        K::AnnotationAssertion(p, s, v) => {
            hash_str_polled(p.as_str(), h, steps, budget)?;
            hash_ann_subject_polled(s, h, steps, budget)?;
            hash_ann_value_polled(v, h, steps, budget)
        }
        K::SubAnnotationPropertyOf(a, b) => {
            hash_str_polled(a.as_str(), h, steps, budget)?;
            hash_str_polled(b.as_str(), h, steps, budget)
        }
        K::AnnotationPropertyDomain(p, i) | K::AnnotationPropertyRange(p, i) => {
            hash_str_polled(p.as_str(), h, steps, budget)?;
            hash_str_polled(i.as_str(), h, steps, budget)
        }
    }
}

fn eq_axiom_polled(
    a: &AxiomKind,
    b: &AxiomKind,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<bool, Error> {
    use AxiomKind as K;
    if std::ptr::eq(a, b) {
        return Ok(true);
    }
    tick(steps, budget)?;
    match (a, b) {
        (K::Declaration(x), K::Declaration(y)) => eq_entity_polled(x, y, steps, budget),
        (K::SubClassOf(a1, b1), K::SubClassOf(a2, b2)) => {
            Ok(eq_ce_polled(a1, a2, steps, budget)? && eq_ce_polled(b1, b2, steps, budget)?)
        }
        (K::EquivalentClasses(xs), K::EquivalentClasses(ys))
        | (K::DisjointClasses(xs), K::DisjointClasses(ys)) => {
            eq_set_polled(xs, ys, steps, budget, eq_ce_polled)
        }
        (K::DisjointUnion(c1, xs), K::DisjointUnion(c2, ys)) => {
            Ok(eq_str_polled(c1.as_str(), c2.as_str(), steps, budget)?
                && eq_set_polled(xs, ys, steps, budget, eq_ce_polled)?)
        }
        (K::SubObjectPropertyOf(s1, p1), K::SubObjectPropertyOf(s2, p2)) => {
            Ok(eq_sub_ope_polled(s1, s2, steps, budget)? && eq_ope_polled(p1, p2, steps, budget)?)
        }
        (K::EquivalentObjectProperties(xs), K::EquivalentObjectProperties(ys))
        | (K::DisjointObjectProperties(xs), K::DisjointObjectProperties(ys)) => {
            eq_set_polled(xs, ys, steps, budget, eq_ope_polled)
        }
        (K::InverseObjectProperties(a1, b1), K::InverseObjectProperties(a2, b2)) => {
            Ok(eq_ope_polled(a1, a2, steps, budget)? && eq_ope_polled(b1, b2, steps, budget)?)
        }
        (K::ObjectPropertyDomain(p1, c1), K::ObjectPropertyDomain(p2, c2))
        | (K::ObjectPropertyRange(p1, c1), K::ObjectPropertyRange(p2, c2)) => {
            Ok(eq_ope_polled(p1, p2, steps, budget)? && eq_ce_polled(c1, c2, steps, budget)?)
        }
        (K::FunctionalObjectProperty(x), K::FunctionalObjectProperty(y))
        | (K::InverseFunctionalObjectProperty(x), K::InverseFunctionalObjectProperty(y))
        | (K::ReflexiveObjectProperty(x), K::ReflexiveObjectProperty(y))
        | (K::IrreflexiveObjectProperty(x), K::IrreflexiveObjectProperty(y))
        | (K::SymmetricObjectProperty(x), K::SymmetricObjectProperty(y))
        | (K::AsymmetricObjectProperty(x), K::AsymmetricObjectProperty(y))
        | (K::TransitiveObjectProperty(x), K::TransitiveObjectProperty(y)) => {
            eq_ope_polled(x, y, steps, budget)
        }
        (K::SubDataPropertyOf(a1, b1), K::SubDataPropertyOf(a2, b2)) => {
            Ok(eq_dp_polled(a1, a2, steps, budget)? && eq_dp_polled(b1, b2, steps, budget)?)
        }
        (K::EquivalentDataProperties(xs), K::EquivalentDataProperties(ys))
        | (K::DisjointDataProperties(xs), K::DisjointDataProperties(ys)) => {
            eq_set_polled(xs, ys, steps, budget, eq_dp_polled)
        }
        (K::DataPropertyDomain(p1, c1), K::DataPropertyDomain(p2, c2)) => {
            Ok(eq_dp_polled(p1, p2, steps, budget)? && eq_ce_polled(c1, c2, steps, budget)?)
        }
        (K::DataPropertyRange(p1, r1), K::DataPropertyRange(p2, r2)) => {
            Ok(eq_dp_polled(p1, p2, steps, budget)? && eq_dr_polled(r1, r2, steps, budget)?)
        }
        (K::FunctionalDataProperty(x), K::FunctionalDataProperty(y)) => {
            eq_dp_polled(x, y, steps, budget)
        }
        (K::DatatypeDefinition(d1, r1), K::DatatypeDefinition(d2, r2)) => {
            Ok(eq_str_polled(d1.as_str(), d2.as_str(), steps, budget)?
                && eq_dr_polled(r1, r2, steps, budget)?)
        }
        (K::HasKey(c1, ops1, dps1), K::HasKey(c2, ops2, dps2)) => {
            Ok(eq_ce_polled(c1, c2, steps, budget)?
                && eq_set_polled(ops1, ops2, steps, budget, eq_ope_polled)?
                && eq_set_polled(dps1, dps2, steps, budget, eq_dp_polled)?)
        }
        (K::SameIndividual(xs), K::SameIndividual(ys))
        | (K::DifferentIndividuals(xs), K::DifferentIndividuals(ys)) => {
            eq_set_polled(xs, ys, steps, budget, eq_ind_polled)
        }
        (K::ClassAssertion(c1, i1), K::ClassAssertion(c2, i2)) => {
            Ok(eq_ce_polled(c1, c2, steps, budget)? && eq_ind_polled(i1, i2, steps, budget)?)
        }
        (K::ObjectPropertyAssertion(p1, a1, b1), K::ObjectPropertyAssertion(p2, a2, b2))
        | (
            K::NegativeObjectPropertyAssertion(p1, a1, b1),
            K::NegativeObjectPropertyAssertion(p2, a2, b2),
        ) => Ok(eq_ope_polled(p1, p2, steps, budget)?
            && eq_ind_polled(a1, a2, steps, budget)?
            && eq_ind_polled(b1, b2, steps, budget)?),
        (K::DataPropertyAssertion(p1, a1, l1), K::DataPropertyAssertion(p2, a2, l2))
        | (
            K::NegativeDataPropertyAssertion(p1, a1, l1),
            K::NegativeDataPropertyAssertion(p2, a2, l2),
        ) => Ok(eq_dp_polled(p1, p2, steps, budget)?
            && eq_ind_polled(a1, a2, steps, budget)?
            && eq_lit_polled(l1, l2, steps, budget)?),
        (K::AnnotationAssertion(p1, s1, v1), K::AnnotationAssertion(p2, s2, v2)) => {
            Ok(eq_str_polled(p1.as_str(), p2.as_str(), steps, budget)?
                && eq_ann_subject_polled(s1, s2, steps, budget)?
                && eq_ann_value_polled(v1, v2, steps, budget)?)
        }
        (K::SubAnnotationPropertyOf(a1, b1), K::SubAnnotationPropertyOf(a2, b2)) => {
            Ok(eq_str_polled(a1.as_str(), a2.as_str(), steps, budget)?
                && eq_str_polled(b1.as_str(), b2.as_str(), steps, budget)?)
        }
        (K::AnnotationPropertyDomain(p1, i1), K::AnnotationPropertyDomain(p2, i2))
        | (K::AnnotationPropertyRange(p1, i1), K::AnnotationPropertyRange(p2, i2)) => {
            Ok(eq_str_polled(p1.as_str(), p2.as_str(), steps, budget)?
                && eq_str_polled(i1.as_str(), i2.as_str(), steps, budget)?)
        }
        _ => Ok(false),
    }
}

pub(super) trait PolledKey {
    fn hash_polled(
        &self,
        h: &mut FxHasher,
        steps: &mut usize,
        budget: &impl ScratchBudget,
    ) -> Result<(), Error>;

    fn eq_polled(
        &self,
        other: &Self,
        steps: &mut usize,
        budget: &impl ScratchBudget,
    ) -> Result<bool, Error>;
}

impl PolledKey for ClassExpression {
    fn hash_polled(
        &self,
        h: &mut FxHasher,
        steps: &mut usize,
        budget: &impl ScratchBudget,
    ) -> Result<(), Error> {
        hash_ce_polled(self, h, steps, budget)
    }

    fn eq_polled(
        &self,
        other: &Self,
        steps: &mut usize,
        budget: &impl ScratchBudget,
    ) -> Result<bool, Error> {
        eq_ce_polled(self, other, steps, budget)
    }
}

impl PolledKey for AxiomKind {
    fn hash_polled(
        &self,
        h: &mut FxHasher,
        steps: &mut usize,
        budget: &impl ScratchBudget,
    ) -> Result<(), Error> {
        hash_axiom_polled(self, h, steps, budget)
    }

    fn eq_polled(
        &self,
        other: &Self,
        steps: &mut usize,
        budget: &impl ScratchBudget,
    ) -> Result<bool, Error> {
        eq_axiom_polled(self, other, steps, budget)
    }
}

fn hash_one_polled<T: PolledKey>(
    item: &T,
    steps: &mut usize,
    budget: &impl ScratchBudget,
) -> Result<u64, Error> {
    let mut h = FxHasher::default();
    item.hash_polled(&mut h, steps, budget)?;
    Ok(h.finish())
}

/// Cached-hash set for borrowed structural keys. All hashing and equality checks poll
/// `budget` cooperatively, resizing rehashes only cached `u64` digests, and table growth
/// admits old-plus-replacement overlap before reallocation.
pub(super) struct PolledSet<'a, T> {
    table: HashTable<(u64, &'a T)>,
    charged_bytes: usize,
}

impl<T> Default for PolledSet<'_, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, T> PolledSet<'a, T> {
    pub fn new() -> Self {
        Self {
            table: HashTable::new(),
            charged_bytes: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn clear(&mut self) {
        self.table.clear();
    }

    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.table.capacity()
    }
}

impl<'a, T: PolledKey> PolledSet<'a, T> {
    pub fn contains_polled(
        &self,
        item: &'a T,
        steps: &mut usize,
        budget: &impl ScratchBudget,
    ) -> Result<bool, Error> {
        if self.table.is_empty() {
            return Ok(false);
        }
        let h = hash_one_polled(item, steps, budget)?;
        let mut err: Option<Error> = None;
        let found = self.table.find(h, |&(eh, ev)| {
            if err.is_some() {
                return true;
            }
            if eh != h {
                return false;
            }
            match ev.eq_polled(item, steps, budget) {
                Ok(eq) => eq,
                Err(e) => {
                    err = Some(e);
                    true
                }
            }
        });
        if let Some(e) = err {
            return Err(e);
        }
        Ok(found.is_some())
    }

    /// Inserts `item` if absent, returning `Ok(true)` when newly inserted and `Ok(false)`
    /// when an equal key was already present.
    pub fn insert_polled(
        &mut self,
        item: &'a T,
        steps: &mut usize,
        budget: &mut impl ScratchBudget,
    ) -> Result<bool, Error> {
        let h = hash_one_polled(item, steps, budget)?;
        let mut err: Option<Error> = None;
        let found = self.table.find(h, |&(eh, ev)| {
            if err.is_some() {
                return true;
            }
            if eh != h {
                return false;
            }
            match ev.eq_polled(item, steps, budget) {
                Ok(eq) => eq,
                Err(e) => {
                    err = Some(e);
                    true
                }
            }
        });
        if let Some(e) = err {
            return Err(e);
        }
        if found.is_some() {
            return Ok(false);
        }
        if self.table.len() == self.table.capacity() {
            let old_cap = self.table.capacity();
            let next_cap = old_cap
                .checked_mul(2)
                .ok_or_else(|| budget.overflow())?
                .max(14);
            let entry_bytes = std::mem::size_of::<(u64, &'a T)>() + 1;
            let new_bytes =
                hash_table_bytes(next_cap, entry_bytes).ok_or_else(|| budget.overflow())?;
            let old_bytes = self.charged_bytes;
            budget.grow(new_bytes)?;
            self.table
                .reserve(next_cap - self.table.len(), |&(eh, _)| eh);
            self.charged_bytes = new_bytes;
            budget.release(old_bytes);
        }
        self.table.insert_unique(h, (h, item), |&(eh, _)| eh);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::cell::Cell;

    struct Unlimited;
    impl ScratchBudget for Unlimited {
        fn poll(&self) -> Result<(), Error> {
            Ok(())
        }
        fn grow(&mut self, _more: usize) -> Result<(), Error> {
            Ok(())
        }
        fn release(&mut self, _less: usize) {}
        fn overflow(&self) -> Error {
            Error::Memory {
                limit: usize::MAX,
                used: usize::MAX,
            }
        }
    }

    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

    fn ind() -> impl Strategy<Value = Individual> {
        prop_oneof![
            3 => (0..3u8).prop_map(|i| Individual::Named(NamedIndividual::new(format!("urn:i{i}")))),
            1 => (0..2u8).prop_map(|i| Individual::anonymous(format!("b{i}"))),
        ]
    }

    fn ope() -> impl Strategy<Value = ObjectPropertyExpression> {
        prop_oneof![
            3 => (0..3u8).prop_map(|p| ObjectPropertyExpression::Property(ObjectProperty::new(format!("urn:p{p}")))),
            1 => (0..2u8).prop_map(|p| ObjectPropertyExpression::InverseOf(ObjectProperty::new(format!("urn:p{p}")))),
        ]
    }

    fn dp() -> impl Strategy<Value = DataProperty> {
        (0..2u8).prop_map(|p| DataProperty::new(format!("urn:d{p}")))
    }

    fn datatype() -> impl Strategy<Value = Datatype> {
        prop::sample::select(vec!["integer", "double", "string"])
            .prop_map(|d| Datatype::new(format!("{XSD}{d}")))
    }

    fn literal() -> impl Strategy<Value = Literal> {
        prop_oneof![
            Just(Literal::simple("a")),
            Just(Literal::lang("a", "en").unwrap()),
            Just(Literal::from(
                oxrdf::Literal::new_directional_language_tagged_literal(
                    "a",
                    "en",
                    oxrdf::BaseDirection::Ltr,
                )
                .unwrap()
            )),
            Just(Literal::from(
                oxrdf::Literal::new_directional_language_tagged_literal(
                    "a",
                    "en",
                    oxrdf::BaseDirection::Rtl,
                )
                .unwrap()
            )),
            prop::sample::select(vec!["integer", "string"])
                .prop_map(|d| Literal::typed("1", &format!("{XSD}{d}"))),
        ]
    }

    fn data_range() -> impl Strategy<Value = DataRange> {
        let leaf = prop_oneof![
            datatype().prop_map(DataRange::Datatype),
            prop::collection::btree_set(literal(), 1..3).prop_map(DataRange::OneOf),
            (datatype(), literal()).prop_map(|(d, v)| {
                DataRange::DatatypeRestriction(
                    d,
                    BTreeSet::from([FacetRestriction {
                        facet: Iri::new("http://www.w3.org/2001/XMLSchema#minInclusive").unwrap(),
                        value: v,
                    }]),
                )
            }),
        ];
        leaf.prop_recursive(2, 6, 2, |inner| {
            prop_oneof![
                prop::collection::btree_set(inner.clone(), 1..3)
                    .prop_map(DataRange::IntersectionOf),
                prop::collection::btree_set(inner.clone(), 1..3).prop_map(DataRange::UnionOf),
                inner.prop_map(|r| DataRange::ComplementOf(Box::new(r))),
            ]
        })
    }

    fn class_expression() -> impl Strategy<Value = ClassExpression> {
        use ClassExpression as C;
        let leaf = prop_oneof![
            3 => (0..3u8).prop_map(|c| C::Class(Class::new(format!("urn:c{c}")))),
            1 => ope().prop_map(C::ObjectHasSelf),
            1 => (ope(), ind()).prop_map(|(p, i)| C::ObjectHasValue(p, i)),
            1 => prop::collection::btree_set(ind(), 1..3).prop_map(C::ObjectOneOf),
            1 => (dp(), literal()).prop_map(|(p, l)| C::DataHasValue(p, l)),
            1 => (dp(), data_range()).prop_map(|(p, r)| C::DataSomeValuesFrom(vec![p], r)),
            1 => (dp(), data_range()).prop_map(|(p, r)| C::DataAllValuesFrom(vec![p], r)),
            1 => (0..2u32, dp(), prop::option::of(data_range()))
                .prop_map(|(n, p, r)| C::DataMinCardinality(n, p, r)),
            1 => (0..2u32, dp(), prop::option::of(data_range()))
                .prop_map(|(n, p, r)| C::DataMaxCardinality(n, p, r)),
            1 => (0..2u32, dp(), prop::option::of(data_range()))
                .prop_map(|(n, p, r)| C::DataExactCardinality(n, p, r)),
        ];
        leaf.prop_recursive(2, 10, 3, |inner| {
            prop_oneof![
                2 => prop::collection::btree_set(inner.clone(), 1..3).prop_map(C::ObjectIntersectionOf),
                2 => prop::collection::btree_set(inner.clone(), 1..3).prop_map(C::ObjectUnionOf),
                1 => inner.clone().prop_map(|c| C::ObjectComplementOf(Box::new(c))),
                2 => (ope(), inner.clone()).prop_map(|(p, c)| C::ObjectSomeValuesFrom(p, Box::new(c))),
                1 => (ope(), inner.clone()).prop_map(|(p, c)| C::ObjectAllValuesFrom(p, Box::new(c))),
                1 => (0..2u32, ope(), prop::option::of(inner.clone()))
                    .prop_map(|(n, p, c)| C::ObjectMinCardinality(n, p, c.map(Box::new))),
                1 => (0..2u32, ope(), prop::option::of(inner.clone()))
                    .prop_map(|(n, p, c)| C::ObjectMaxCardinality(n, p, c.map(Box::new))),
                1 => (0..2u32, ope(), prop::option::of(inner))
                    .prop_map(|(n, p, c)| C::ObjectExactCardinality(n, p, c.map(Box::new))),
            ]
        })
    }

    fn axiom() -> impl Strategy<Value = AxiomKind> {
        use AxiomKind as K;
        let ces = || prop::collection::btree_set(class_expression(), 1..3);
        let inds = || prop::collection::btree_set(ind(), 1..3);
        prop_oneof![
            3 => (class_expression(), class_expression()).prop_map(|(a, b)| K::SubClassOf(a, b)),
            1 => ces().prop_map(K::EquivalentClasses),
            1 => ces().prop_map(K::DisjointClasses),
            1 => (0..2u8, ces()).prop_map(|(c, cs)| K::DisjointUnion(Class::new(format!("urn:c{c}")), cs)),
            1 => (ope(), ope()).prop_map(|(a, b)| K::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(a), b)),
            1 => (prop::collection::vec(ope(), 1..3), ope())
                .prop_map(|(ps, b)| K::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), b)),
            1 => prop::collection::btree_set(ope(), 1..3).prop_map(K::EquivalentObjectProperties),
            1 => (ope(), class_expression()).prop_map(|(p, c)| K::ObjectPropertyRange(p, c)),
            1 => (dp(), data_range()).prop_map(|(p, r)| K::DataPropertyRange(p, r)),
            1 => (datatype(), data_range()).prop_map(|(d, r)| K::DatatypeDefinition(d, r)),
            1 => (class_expression(), prop::collection::btree_set(ope(), 0..2), prop::collection::btree_set(dp(), 0..2))
                .prop_map(|(c, ops, dps)| K::HasKey(c, ops, dps)),
            1 => (class_expression(), ind()).prop_map(|(c, i)| K::ClassAssertion(c, i)),
            1 => inds().prop_map(K::SameIndividual),
            1 => (dp(), ind(), literal()).prop_map(|(p, a, l)| K::DataPropertyAssertion(p, a, l)),
            1 => literal().prop_map(|l| {
                K::AnnotationAssertion(
                    AnnotationProperty::new("urn:ap"),
                    AnnotationSubject::Iri(Iri::new("urn:s").unwrap()),
                    AnnotationValue::Literal(l),
                )
            }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]
        #[test]
        fn polled_ce_cmp_eq_and_hash_match_derived(a in class_expression(), b in class_expression()) {
            let mut steps = 0usize;
            let eq = a.eq_polled(&b, &mut steps, &Unlimited).unwrap();
            prop_assert_eq!(eq, a == b);
            let ord = cmp_ce_polled(&a, &b, &mut steps, &Unlimited).unwrap();
            prop_assert_eq!(ord, a.cmp(&b));
            if a == b {
                let ha = hash_one_polled(&a, &mut steps, &Unlimited).unwrap();
                let hb = hash_one_polled(&b, &mut steps, &Unlimited).unwrap();
                prop_assert_eq!(ha, hb);
            }
        }

        #[test]
        fn polled_axiom_eq_and_hash_match_derived(a in axiom(), b in axiom()) {
            let mut steps = 0usize;
            let eq = a.eq_polled(&b, &mut steps, &Unlimited).unwrap();
            prop_assert_eq!(eq, a == b);
            if a == b {
                let ha = hash_one_polled(&a, &mut steps, &Unlimited).unwrap();
                let hb = hash_one_polled(&b, &mut steps, &Unlimited).unwrap();
                prop_assert_eq!(ha, hb);
            }
        }
    }

    #[test]
    fn polled_set_resizes_using_cached_hashes_without_rehashing_keys() {
        struct CountedKey {
            id: usize,
            hashes: Cell<usize>,
        }
        impl PolledKey for CountedKey {
            fn hash_polled(
                &self,
                h: &mut FxHasher,
                _steps: &mut usize,
                _budget: &impl ScratchBudget,
            ) -> Result<(), Error> {
                self.hashes.set(self.hashes.get() + 1);
                self.id.hash(h);
                Ok(())
            }
            fn eq_polled(
                &self,
                other: &Self,
                _steps: &mut usize,
                _budget: &impl ScratchBudget,
            ) -> Result<bool, Error> {
                Ok(self.id == other.id)
            }
        }

        let keys: Vec<CountedKey> = (0..64)
            .map(|id| CountedKey {
                id,
                hashes: Cell::new(0),
            })
            .collect();
        let mut set: PolledSet<'_, CountedKey> = PolledSet::new();
        let mut steps = 0usize;
        let mut budget = Unlimited;
        for k in &keys {
            assert!(set.insert_polled(k, &mut steps, &mut budget).unwrap());
        }
        // Multiple resizes happened (0 -> 14 -> 28 -> 56 -> 112), yet every key was
        // hashed exactly once (at insertion) and never rehashed during table growth.
        assert!(set.capacity() >= 64);
        for k in &keys {
            assert_eq!(k.hashes.get(), 1, "key {} rehashed during resize", k.id);
        }
    }
}
