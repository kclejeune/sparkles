//! The reductions of item 805 (F24, "Nominals in the disjunctive rules"): a rooted run
//! takes keys over object properties and the universal property by rewriting the axioms
//! before anything reads them.
//!
//! The universal property `owl:topObjectProperty` becomes a fresh property `U` that is
//! symmetric and transitive, with `⊤ ⊑ ∃U.{h}` for a fresh individual `h`, so that every
//! element reaches `h` and back and `U` relates every pair of elements. An inclusion or a
//! chain whose superproperty is the universal property, and a symmetry, transitivity or
//! reflexivity of it, hold of every ontology and are dropped. A domain or range of the
//! property holds of every element, and a number restriction, functionality, asymmetry,
//! irreflexivity or disjointness of it has no replacement, which leaves the ontology to the
//! run without the reductions.
//!
//! A key `HasKey(C (r_1 … r_m) ())` holds only of named individuals, and its values are
//! named too. For each named individual `b` other than the first and each key property
//! `r_j`, the axiom `{b} ⊓ C ⊑ ∀r_j.V_jb` marks the values of `b` when `b` is a `C`. For
//! each pair of distinct named individuals `a` and `b`, each `r_j` and each named individual
//! `c`, the axiom `{a} ⊓ ∃r_j.({c} ⊓ V_jb) ⊑ M_jab` marks that `a` has a named value of `b`,
//! and `{a} ⊓ C ⊓ M_1ab ⊓ … ⊓ M_mab ⊑ {b}` makes them equal. A model of the ontology
//! extends to the fresh classes, with `V_jb` the `r_j`-values of `b` when `b` is a `C` and
//! empty otherwise and `M_jab` the set `{a}` when `a` has a named `r_j`-value in `V_jb`, and
//! a model of the rewritten axioms satisfies the key. The marks stay on the values, so no
//! axiom links every element to the individuals, which the reduction through a fresh
//! property `W_b` with `⊤ ⊑ ∃W_b.{b}` did and which took most generated ontologies with
//! keys past the rooted run's bound. The reduction has about `m·n·n(n-1)/2` axioms for `n`
//! named individuals, and an ontology whose keys pass the bound is left to the run without
//! the reductions.
//!
//! Every fresh name has the IRI prefix [`PREFIX`], and an ontology that names anything with
//! it is left to the run without the reductions. The run hides the classes among them from
//! the taxonomy, a partial answer and a description, and the hub individual from a
//! description's instances. The axioms the rewriting makes stand for the original axioms in
//! the run's report.

use super::dl;
use super::index::Gap;
use super::{Error, Excluded};
use crate::model::*;
use crate::vocab;
use std::collections::{BTreeMap, BTreeSet};

/// The IRI prefix of the names that the reductions make up.
pub const PREFIX: &str = "urn:sparkles:internal:";
const U_IRI: &str = "urn:sparkles:internal:u";
const HUB_IRI: &str = "urn:sparkles:internal:hub";

/// The most axioms that the keys' reductions make, whatever the memory budget.
const KEY_AXIOMS: usize = 1 << 17;

/// The bytes of memory budget that one reduction axiom may take.
const AXIOM_BYTES: usize = 2048;

/// Cooperative polling cadence inside wide axioms, signature scans, retargeting, and key
/// expansion.
const POLL_WORK: usize = if cfg!(test) { 64 } else { 1024 };

/// Outcome of item 805 reduction planning.
pub enum Plan {
    /// No reduction is needed, or the ontology is structurally ineligible or Horn-only.
    NotNeeded,
    /// Reduction was needed and structurally eligible, but declined by key/memory budget.
    Declined,
    /// Reduction succeeded and produced rewritten axioms.
    Ready(Reduction),
}

/// The axioms of a rewritten ontology.
pub struct Reduction {
    /// the axioms that the rewriting makes, each standing for the original axiom at the
    /// same place of `origin`
    pub axioms: Vec<AxiomKind>,
    pub origin: Vec<usize>,
    /// the original axioms that the rewriting replaces or drops
    pub replaced: Vec<bool>,
    /// the generated hub individual if this reduction introduced `owl:topObjectProperty`
    /// via `⊤ ⊑ ∃U.{hub}`, identifying the generated hub by provenance rather than prefix
    pub hub: Option<NamedIndividual>,
    /// accumulated heap bytes owned inside `self.axioms` elements (strings, `BTreeSet`s,
    /// `Box`ed expressions, and chain vectors), excluding `self.axioms`'s own `Vec` buffer
    axiom_heap_bytes: usize,
}

impl Reduction {
    /// Retained heap bytes of this reduction's containers, AST payloads, and hub IRI.
    pub fn retained_bytes(&self) -> usize {
        self.axioms
            .capacity()
            .saturating_mul(std::mem::size_of::<AxiomKind>())
            .saturating_add(self.axiom_heap_bytes)
            .saturating_add(
                self.origin
                    .capacity()
                    .saturating_mul(std::mem::size_of::<usize>()),
            )
            .saturating_add(self.replaced.capacity())
            .saturating_add(self.hub.as_ref().map_or(0, |h| h.as_str().len()))
    }

    /// The number of axiom references that [`Reduction::all`] will collect.
    pub fn all_len(&self, axioms: &[&AxiomKind]) -> usize {
        let kept = self.replaced.iter().filter(|r| !**r).count();
        debug_assert_eq!(self.replaced.len(), axioms.len());
        kept.saturating_add(self.axioms.len())
    }

    /// The axioms of the rewritten ontology.
    pub fn all<'a>(&'a self, axioms: &[&'a AxiomKind]) -> Vec<&'a AxiomKind> {
        let mut all: Vec<&AxiomKind> = Vec::with_capacity(self.all_len(axioms));
        all.extend(
            axioms
                .iter()
                .zip(&self.replaced)
                .filter(|(_, r)| !**r)
                .map(|(k, _)| *k),
        );
        all.extend(self.axioms.iter());
        all
    }

    /// Restores the original axioms in a sorted report. A rewritten axiom can stand
    /// for several originals, and is also kept when it was stated unchanged.
    pub fn restore(
        &self,
        axioms: &[&AxiomKind],
        excluded: &mut Vec<Excluded>,
        admit: &mut impl FnMut(usize) -> Result<(), Error>,
    ) -> Result<(), Error> {
        use std::mem::size_of;

        let start = |k: &AxiomKind| excluded.partition_point(|e| e.axiom < *k);
        let reported = |k: &AxiomKind| excluded.get(start(k)).is_some_and(|e| e.axiom == *k);
        if excluded.is_empty() {
            return Ok(());
        }
        let matches = self.axioms.iter().filter(|k| reported(k)).count();
        if matches == 0 {
            return Ok(());
        }

        let scratch = btree_map_bytes::<usize, [usize; 2]>(matches.min(axioms.len()))
            .saturating_add(excluded.len().saturating_mul(size_of::<bool>()));
        admit(scratch)?;
        let mut dropped = vec![false; excluded.len()];
        let mut first = BTreeMap::new();
        let range = |k: &AxiomKind| start(k)..excluded.partition_point(|e| e.axiom <= *k);
        for (k, &origin) in self.axioms.iter().zip(&self.origin) {
            let entries = range(k);
            if entries.is_empty() {
                continue;
            }
            let first = first.entry(origin).or_insert([usize::MAX; 2]);
            for i in entries {
                dropped[i] = true;
                // The first sorted exclusion of each kind supplies the original's gap.
                let kind = usize::from(matches!(excluded[i].gap, Gap::Partial(_)));
                first[kind] = first[kind].min(i);
            }
        }
        for (k, replaced) in axioms.iter().zip(&self.replaced) {
            if !*replaced {
                dropped[range(k)].fill(false);
            }
        }
        let report = excluded.as_slice();
        let restored = || {
            first.iter().flat_map(|(&origin, indices)| {
                indices
                    .iter()
                    .filter_map(move |&i| report.get(i).map(|e| (axioms[origin], &e.gap)))
            })
        };
        let (entries, bytes) = restored().fold(
            (dropped.iter().filter(|&&drop| !drop).count(), 0usize),
            |(entries, bytes), (k, gap)| {
                let text = match gap {
                    Gap::Outside { rule, message } => rule.len().saturating_add(message.len()),
                    Gap::Partial(_) => 0,
                };
                (
                    entries + 1,
                    bytes
                        .saturating_add(axiom_retained_heap_bytes(k))
                        .saturating_add(text),
                )
            },
        );
        admit(
            scratch
                .saturating_add(entries.saturating_mul(size_of::<Excluded>()))
                .saturating_add(bytes),
        )?;
        let mut out = Vec::with_capacity(entries);
        out.extend(restored().map(|(axiom, gap)| Excluded {
            axiom: axiom.clone(),
            gap: gap.clone(),
        }));
        out.extend(
            excluded
                .drain(..)
                .zip(dropped)
                .filter_map(|(e, drop)| (!drop).then_some(e)),
        );
        out.sort_unstable();
        out.dedup();
        *excluded = out;
        Ok(())
    }

    /// Which original axioms stand for a rewritten chain or are a chain as stated.
    pub fn chain_origins<'a>(
        &'a self,
        axioms: &'a [&'a AxiomKind],
        is_chain: impl Fn(&AxiomKind) -> bool + Copy + 'a,
    ) -> impl Iterator<Item = usize> + Clone + 'a {
        self.axioms
            .iter()
            .zip(&self.origin)
            .filter_map(move |(k, &i)| is_chain(k).then_some(i))
            .chain(
                axioms
                    .iter()
                    .zip(&self.replaced)
                    .enumerate()
                    .filter_map(move |(i, (k, &replaced))| (!replaced && is_chain(k)).then_some(i)),
            )
    }

    /// What the original axioms outside OWL 2 EL need, for the run's statistics, with the
    /// report of the axioms the run left out.
    /// `chains` are the original chains outside OWL 2 EL for their ranges alone, which are
    /// Horn, as the run without the rewriting counts them.
    pub fn outside_el(
        &self,
        axioms: &[&AxiomKind],
        chains: &[usize],
        excluded: &[Excluded],
    ) -> dl::OutsideEl {
        let mut out = dl::OutsideEl::default();
        let mut seen: BTreeSet<&AxiomKind> = BTreeSet::new();
        for k in axioms {
            if super::logical(k) && super::el_break(k).is_some() && seen.insert(k) {
                out.add(dl::shape(k));
            }
        }
        for &i in chains {
            if seen.insert(axioms[i]) {
                out.horn += 1;
            }
        }
        let left: BTreeSet<&AxiomKind> = excluded
            .iter()
            .filter(|e| matches!(e.gap, Gap::Outside { .. }))
            .map(|e| &e.axiom)
            .filter(|k| seen.contains(k))
            .collect();
        out.taken = seen.len() - left.len();
        out
    }
}

fn is_top(p: &ObjectPropertyExpression) -> bool {
    p.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY
}

fn u() -> ObjectProperty {
    ObjectProperty::new(U_IRI)
}

fn hub() -> Individual {
    Individual::named(HUB_IRI)
}

fn exact_k_class(keys: usize, len: usize) -> Class {
    use std::fmt::Write;
    let mut s = String::with_capacity(len);
    let _ = write!(&mut s, "{PREFIX}k{keys}");
    debug_assert_eq!(s.capacity(), len);
    Class::new(s)
}

fn exact_v_class(k: usize, j: usize, b: usize, len: usize) -> Class {
    use std::fmt::Write;
    let mut s = String::with_capacity(len);
    let _ = write!(&mut s, "{PREFIX}v{k}-{j}-{b}");
    debug_assert_eq!(s.capacity(), len);
    Class::new(s)
}

fn exact_m_class(k: usize, j: usize, a: usize, b: usize, len: usize) -> Class {
    use std::fmt::Write;
    let mut s = String::with_capacity(len);
    let _ = write!(&mut s, "{PREFIX}m{k}-{j}-{a}-{b}");
    debug_assert_eq!(s.capacity(), len);
    Class::new(s)
}

struct Poller<'a, E, F: FnMut() -> Result<(), E>> {
    steps: usize,
    poll: &'a mut F,
}

impl<E, F: FnMut() -> Result<(), E>> Poller<'_, E, F> {
    #[inline]
    fn tick(&mut self) -> Result<(), E> {
        let s = self.steps;
        self.steps = self.steps.wrapping_add(1);
        if s.is_multiple_of(POLL_WORK) {
            (self.poll)()?;
        }
        Ok(())
    }
}

fn ce_has_top_polled<E, F: FnMut() -> Result<(), E>>(
    c: &ClassExpression,
    poller: &mut Poller<'_, E, F>,
) -> Result<bool, E> {
    use ClassExpression as C;
    poller.tick()?;
    match c {
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            for elem in cs {
                if ce_has_top_polled(elem, poller)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        C::ObjectComplementOf(f) => ce_has_top_polled(f, poller),
        C::ObjectSomeValuesFrom(p, f) | C::ObjectAllValuesFrom(p, f) => {
            Ok(is_top(p) || ce_has_top_polled(f, poller)?)
        }
        C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => Ok(is_top(p)),
        C::ObjectMinCardinality(_, p, f)
        | C::ObjectMaxCardinality(_, p, f)
        | C::ObjectExactCardinality(_, p, f) => {
            if is_top(p) {
                return Ok(true);
            }
            match f {
                Some(inner) => ce_has_top_polled(inner, poller),
                None => Ok(false),
            }
        }
        _ => Ok(false),
    }
}

fn ax_has_top_polled<E, F: FnMut() -> Result<(), E>>(
    k: &AxiomKind,
    poller: &mut Poller<'_, E, F>,
) -> Result<bool, E> {
    use AxiomKind as K;
    poller.tick()?;
    match k {
        K::SubClassOf(a, b) => Ok(ce_has_top_polled(a, poller)? || ce_has_top_polled(b, poller)?),
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
            for c in cs {
                if ce_has_top_polled(c, poller)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        K::ClassAssertion(c, _) => ce_has_top_polled(c, poller),
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
            Ok(is_top(p) || ce_has_top_polled(c, poller)?)
        }
        K::SubObjectPropertyOf(s, p) => {
            if is_top(p) {
                return Ok(true);
            }
            match s {
                SubObjectPropertyExpression::Expression(q) => Ok(is_top(q)),
                SubObjectPropertyExpression::Chain(qs) => {
                    for q in qs {
                        poller.tick()?;
                        if is_top(q) {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }
            }
        }
        K::EquivalentObjectProperties(ps) | K::DisjointObjectProperties(ps) => {
            for p in ps {
                poller.tick()?;
                if is_top(p) {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        K::InverseObjectProperties(p, q) => Ok(is_top(p) || is_top(q)),
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => Ok(is_top(p)),
        K::ObjectPropertyAssertion(p, ..) | K::NegativeObjectPropertyAssertion(p, ..) => {
            Ok(is_top(p))
        }
        K::HasKey(c, ops, _) => {
            if ce_has_top_polled(c, poller)? {
                return Ok(true);
            }
            for p in ops {
                poller.tick()?;
                if is_top(p) {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// The state of a rewriting: whether the fresh property `U` is in use.
#[derive(Default)]
struct Cx {
    used: bool,
}

impl Cx {
    fn ope(&mut self, p: &ObjectPropertyExpression) -> ObjectPropertyExpression {
        if !is_top(p) {
            return p.clone();
        }
        self.used = true;
        match p {
            ObjectPropertyExpression::Property(_) => ObjectPropertyExpression::Property(u()),
            ObjectPropertyExpression::InverseOf(_) => ObjectPropertyExpression::InverseOf(u()),
        }
    }

    fn ces_polled<E, F: FnMut() -> Result<(), E>>(
        &mut self,
        cs: &BTreeSet<ClassExpression>,
        poller: &mut Poller<'_, E, F>,
    ) -> Result<Result<BTreeSet<ClassExpression>, ()>, E> {
        let mut out = BTreeSet::new();
        for c in cs {
            let Ok(d) = self.ce_polled(c, poller)? else {
                return Ok(Err(()));
            };
            out.insert(d);
        }
        Ok(Ok(out))
    }

    fn dr_clone_polled<E, F: FnMut() -> Result<(), E>>(
        r: &DataRange,
        poller: &mut Poller<'_, E, F>,
    ) -> Result<DataRange, E> {
        poller.tick()?;
        Ok(match r {
            DataRange::Datatype(d) => DataRange::Datatype(d.clone()),
            DataRange::IntersectionOf(rs) => {
                let mut out = BTreeSet::new();
                for elem in rs {
                    out.insert(Self::dr_clone_polled(elem, poller)?);
                }
                DataRange::IntersectionOf(out)
            }
            DataRange::UnionOf(rs) => {
                let mut out = BTreeSet::new();
                for elem in rs {
                    out.insert(Self::dr_clone_polled(elem, poller)?);
                }
                DataRange::UnionOf(out)
            }
            DataRange::ComplementOf(inner) => {
                DataRange::ComplementOf(Box::new(Self::dr_clone_polled(inner, poller)?))
            }
            DataRange::OneOf(ls) => {
                let mut out = BTreeSet::new();
                for l in ls {
                    poller.tick()?;
                    out.insert(l.clone());
                }
                DataRange::OneOf(out)
            }
            DataRange::DatatypeRestriction(d, fs) => {
                let mut out = BTreeSet::new();
                for f in fs {
                    poller.tick()?;
                    out.insert(f.clone());
                }
                DataRange::DatatypeRestriction(d.clone(), out)
            }
        })
    }

    fn ce_polled<E, F: FnMut() -> Result<(), E>>(
        &mut self,
        c: &ClassExpression,
        poller: &mut Poller<'_, E, F>,
    ) -> Result<Result<ClassExpression, ()>, E> {
        use ClassExpression as C;
        poller.tick()?;
        Ok(Ok(match c {
            C::Class(cl) => C::Class(cl.clone()),
            C::ObjectIntersectionOf(cs) => {
                let Ok(set) = self.ces_polled(cs, poller)? else {
                    return Ok(Err(()));
                };
                C::ObjectIntersectionOf(set)
            }
            C::ObjectUnionOf(cs) => {
                let Ok(set) = self.ces_polled(cs, poller)? else {
                    return Ok(Err(()));
                };
                C::ObjectUnionOf(set)
            }
            C::ObjectComplementOf(f) => {
                let Ok(inner) = self.ce_polled(f, poller)? else {
                    return Ok(Err(()));
                };
                C::ObjectComplementOf(Box::new(inner))
            }
            C::ObjectOneOf(is) => {
                let mut out = BTreeSet::new();
                #[cfg(test)]
                let mut copied = 0usize;
                for i in is {
                    #[cfg(test)]
                    {
                        if copied == 16 {
                            super::admission_test::merge_phase_retarget();
                        }
                        copied += 1;
                    }
                    poller.tick()?;
                    out.insert(i.clone());
                }
                #[cfg(test)]
                if copied >= 16 {
                    super::admission_test::merge_phase_retarget_done();
                }
                C::ObjectOneOf(out)
            }
            C::ObjectSomeValuesFrom(p, f) => {
                let Ok(inner) = self.ce_polled(f, poller)? else {
                    return Ok(Err(()));
                };
                C::ObjectSomeValuesFrom(self.ope(p), Box::new(inner))
            }
            C::ObjectAllValuesFrom(p, f) => {
                let Ok(inner) = self.ce_polled(f, poller)? else {
                    return Ok(Err(()));
                };
                C::ObjectAllValuesFrom(self.ope(p), Box::new(inner))
            }
            C::ObjectHasValue(p, i) => C::ObjectHasValue(self.ope(p), i.clone()),
            C::ObjectHasSelf(p) if is_top(p) => return Ok(Err(())),
            C::ObjectHasSelf(p) => C::ObjectHasSelf(p.clone()),
            // at least none is every element, and at least one is an existential
            C::ObjectMinCardinality(0, p, _) if is_top(p) => C::Class(Class::thing()),
            C::ObjectMinCardinality(1, p, f) if is_top(p) => {
                let inner = match f {
                    Some(f) => {
                        let Ok(inner) = self.ce_polled(f, poller)? else {
                            return Ok(Err(()));
                        };
                        inner
                    }
                    None => C::Class(Class::thing()),
                };
                C::ObjectSomeValuesFrom(self.ope(p), Box::new(inner))
            }
            C::ObjectMinCardinality(_, p, _) if is_top(p) => return Ok(Err(())),
            C::ObjectMinCardinality(n, p, f) => {
                let inner = match f {
                    Some(f) => {
                        let Ok(inner) = self.ce_polled(f, poller)? else {
                            return Ok(Err(()));
                        };
                        Some(Box::new(inner))
                    }
                    None => None,
                };
                C::ObjectMinCardinality(*n, p.clone(), inner)
            }
            C::ObjectMaxCardinality(_, p, _) | C::ObjectExactCardinality(_, p, _) if is_top(p) => {
                return Ok(Err(()));
            }
            C::ObjectMaxCardinality(n, p, f) => {
                let inner = match f {
                    Some(f) => {
                        let Ok(inner) = self.ce_polled(f, poller)? else {
                            return Ok(Err(()));
                        };
                        Some(Box::new(inner))
                    }
                    None => None,
                };
                C::ObjectMaxCardinality(*n, p.clone(), inner)
            }
            C::ObjectExactCardinality(n, p, f) => {
                let inner = match f {
                    Some(f) => {
                        let Ok(inner) = self.ce_polled(f, poller)? else {
                            return Ok(Err(()));
                        };
                        Some(Box::new(inner))
                    }
                    None => None,
                };
                C::ObjectExactCardinality(*n, p.clone(), inner)
            }
            C::DataSomeValuesFrom(ps, r) => {
                let mut out_ps = Vec::with_capacity(ps.len());
                for p in ps {
                    poller.tick()?;
                    out_ps.push(p.clone());
                }
                C::DataSomeValuesFrom(out_ps, Self::dr_clone_polled(r, poller)?)
            }
            C::DataAllValuesFrom(ps, r) => {
                let mut out_ps = Vec::with_capacity(ps.len());
                for p in ps {
                    poller.tick()?;
                    out_ps.push(p.clone());
                }
                C::DataAllValuesFrom(out_ps, Self::dr_clone_polled(r, poller)?)
            }
            C::DataHasValue(p, l) => C::DataHasValue(p.clone(), l.clone()),
            C::DataMinCardinality(n, p, r) => C::DataMinCardinality(
                *n,
                p.clone(),
                match r {
                    Some(r) => Some(Self::dr_clone_polled(r, poller)?),
                    None => None,
                },
            ),
            C::DataMaxCardinality(n, p, r) => C::DataMaxCardinality(
                *n,
                p.clone(),
                match r {
                    Some(r) => Some(Self::dr_clone_polled(r, poller)?),
                    None => None,
                },
            ),
            C::DataExactCardinality(n, p, r) => C::DataExactCardinality(
                *n,
                p.clone(),
                match r {
                    Some(r) => Some(Self::dr_clone_polled(r, poller)?),
                    None => None,
                },
            ),
        }))
    }
}

#[inline]
fn dec_len(mut n: usize) -> usize {
    let mut d = 1usize;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Safe upper bound on heap bytes owned by a `BTreeSet<T>` of `len` elements at any height.
///
/// With Rust's `B = 6` (`MIN_LEN = 5`, `CAPACITY = 11`), inserting 12 ordered items and
/// removing one leaves 5 + 1 + 5 keys across 2 leaves and 1 internal root, a 3-node shape
/// preserved by `clone()`. Only `len <= 10` is guaranteed to fit in a single leaf.
#[inline]
pub(super) fn btree_set_bytes<T>(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let elem = std::mem::size_of::<T>();
    let leaf = 32usize.saturating_add(11usize.saturating_mul(elem));
    if len <= 10 {
        return leaf;
    }
    let leaves = len.div_ceil(5);
    let internals = (leaves / 4).saturating_add(1);
    leaves.saturating_mul(leaf).saturating_add(
        internals.saturating_mul(128usize.saturating_add(11usize.saturating_mul(elem))),
    )
}

/// Safe upper bound on heap bytes owned by a `BTreeMap<K, V>` of `len` entries at any height.
///
/// Only `len <= 10` is guaranteed to occupy a single leaf; `len == 11` can retain a 3-node
/// shape (5 + 1 + 5) after removal and across `clone()`.
#[inline]
fn btree_map_bytes<K, V>(len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let entry = std::mem::size_of::<K>().saturating_add(std::mem::size_of::<V>());
    let leaf = 32usize.saturating_add(11usize.saturating_mul(entry));
    if len <= 10 {
        return leaf;
    }
    let leaves = len.div_ceil(5);
    let internals = (leaves / 4).saturating_add(1);
    leaves.saturating_mul(leaf).saturating_add(
        internals.saturating_mul(128usize.saturating_add(11usize.saturating_mul(entry))),
    )
}

#[inline]
fn ind_retained_bytes(i: &Individual) -> usize {
    match i {
        Individual::Named(n) => n.as_str().len(),
        Individual::Anonymous(a) => a.0.capacity(),
    }
}

#[inline]
fn lit_retained_bytes(l: &Literal) -> usize {
    l.value()
        .len()
        .saturating_add(l.datatype().len())
        .saturating_add(l.language().map_or(0, str::len))
}

fn dr_retained_bytes(r: &DataRange) -> usize {
    match r {
        DataRange::Datatype(d) => d.as_str().len(),
        DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
            btree_set_bytes::<DataRange>(rs.len())
                .saturating_add(rs.iter().map(dr_retained_bytes).sum::<usize>())
        }
        DataRange::ComplementOf(inner) => {
            std::mem::size_of::<DataRange>().saturating_add(dr_retained_bytes(inner))
        }
        DataRange::OneOf(ls) => btree_set_bytes::<Literal>(ls.len())
            .saturating_add(ls.iter().map(lit_retained_bytes).sum::<usize>()),
        DataRange::DatatypeRestriction(d, fs) => d
            .as_str()
            .len()
            .saturating_add(btree_set_bytes::<FacetRestriction>(fs.len()))
            .saturating_add(
                fs.iter()
                    .map(|f| {
                        f.facet
                            .as_str()
                            .len()
                            .saturating_add(lit_retained_bytes(&f.value))
                    })
                    .sum::<usize>(),
            ),
    }
}

fn ce_retained_bytes(c: &ClassExpression) -> usize {
    use ClassExpression as C;
    match c {
        C::Class(cl) => cl.as_str().len(),
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            btree_set_bytes::<ClassExpression>(cs.len())
                .saturating_add(cs.iter().map(ce_retained_bytes).sum::<usize>())
        }
        C::ObjectComplementOf(f) => {
            std::mem::size_of::<ClassExpression>().saturating_add(ce_retained_bytes(f))
        }
        C::ObjectOneOf(is) => btree_set_bytes::<Individual>(is.len())
            .saturating_add(is.iter().map(ind_retained_bytes).sum::<usize>()),
        C::ObjectSomeValuesFrom(p, f) | C::ObjectAllValuesFrom(p, f) => p
            .named()
            .as_str()
            .len()
            .saturating_add(std::mem::size_of::<ClassExpression>())
            .saturating_add(ce_retained_bytes(f)),
        C::ObjectHasValue(p, i) => p
            .named()
            .as_str()
            .len()
            .saturating_add(ind_retained_bytes(i)),
        C::ObjectHasSelf(p) => p.named().as_str().len(),
        C::ObjectMinCardinality(_, p, f)
        | C::ObjectMaxCardinality(_, p, f)
        | C::ObjectExactCardinality(_, p, f) => {
            p.named()
                .as_str()
                .len()
                .saturating_add(f.as_deref().map_or(0, |inner| {
                    std::mem::size_of::<ClassExpression>().saturating_add(ce_retained_bytes(inner))
                }))
        }
        C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => ps
            .capacity()
            .saturating_mul(std::mem::size_of::<DataProperty>())
            .saturating_add(ps.iter().map(|p| p.as_str().len()).sum::<usize>())
            .saturating_add(dr_retained_bytes(r)),
        C::DataHasValue(p, l) => p.as_str().len().saturating_add(lit_retained_bytes(l)),
        C::DataMinCardinality(_, p, r)
        | C::DataMaxCardinality(_, p, r)
        | C::DataExactCardinality(_, p, r) => p
            .as_str()
            .len()
            .saturating_add(r.as_ref().map_or(0, dr_retained_bytes)),
    }
}

/// Exact allocation-free upper bound on heap bytes owned inside `k` (all strings,
/// anonymous blank-node labels, literals, `BTreeSet` nodes, `Box`ed subexpressions, `Vec`
/// capacities, and repeated AST payloads without deduplication), excluding the inline
/// `size_of::<AxiomKind>()` itself.
pub(super) fn axiom_retained_heap_bytes(k: &AxiomKind) -> usize {
    use AxiomKind as K;
    match k {
        K::Declaration(e) => e.iri().as_str().len(),
        K::SubClassOf(a, b) => ce_retained_bytes(a).saturating_add(ce_retained_bytes(b)),
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) => {
            btree_set_bytes::<ClassExpression>(cs.len())
                .saturating_add(cs.iter().map(ce_retained_bytes).sum::<usize>())
        }
        K::DisjointUnion(c, cs) => c
            .as_str()
            .len()
            .saturating_add(btree_set_bytes::<ClassExpression>(cs.len()))
            .saturating_add(cs.iter().map(ce_retained_bytes).sum::<usize>()),
        K::SubObjectPropertyOf(s, sup) => sup.named().as_str().len().saturating_add(match s {
            SubObjectPropertyExpression::Expression(p) => p.named().as_str().len(),
            SubObjectPropertyExpression::Chain(ps) => ps
                .capacity()
                .saturating_mul(std::mem::size_of::<ObjectPropertyExpression>())
                .saturating_add(ps.iter().map(|p| p.named().as_str().len()).sum::<usize>()),
        }),
        K::EquivalentObjectProperties(ps) | K::DisjointObjectProperties(ps) => {
            btree_set_bytes::<ObjectPropertyExpression>(ps.len())
                .saturating_add(ps.iter().map(|p| p.named().as_str().len()).sum::<usize>())
        }
        K::InverseObjectProperties(a, b) => a
            .named()
            .as_str()
            .len()
            .saturating_add(b.named().as_str().len()),
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => p
            .named()
            .as_str()
            .len()
            .saturating_add(ce_retained_bytes(c)),
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => p.named().as_str().len(),
        K::SubDataPropertyOf(a, b) => a.as_str().len().saturating_add(b.as_str().len()),
        K::EquivalentDataProperties(ps) | K::DisjointDataProperties(ps) => {
            btree_set_bytes::<DataProperty>(ps.len())
                .saturating_add(ps.iter().map(|p| p.as_str().len()).sum::<usize>())
        }
        K::DataPropertyDomain(p, c) => p.as_str().len().saturating_add(ce_retained_bytes(c)),
        K::DataPropertyRange(p, r) => p.as_str().len().saturating_add(dr_retained_bytes(r)),
        K::FunctionalDataProperty(p) => p.as_str().len(),
        K::DatatypeDefinition(d, r) => d.as_str().len().saturating_add(dr_retained_bytes(r)),
        K::HasKey(c, ops, dps) => ce_retained_bytes(c)
            .saturating_add(btree_set_bytes::<ObjectPropertyExpression>(ops.len()))
            .saturating_add(ops.iter().map(|p| p.named().as_str().len()).sum::<usize>())
            .saturating_add(btree_set_bytes::<DataProperty>(dps.len()))
            .saturating_add(dps.iter().map(|p| p.as_str().len()).sum::<usize>()),
        K::SameIndividual(is) | K::DifferentIndividuals(is) => {
            btree_set_bytes::<Individual>(is.len())
                .saturating_add(is.iter().map(ind_retained_bytes).sum::<usize>())
        }
        K::ClassAssertion(c, i) => ce_retained_bytes(c).saturating_add(ind_retained_bytes(i)),
        K::ObjectPropertyAssertion(p, a, b) | K::NegativeObjectPropertyAssertion(p, a, b) => p
            .named()
            .as_str()
            .len()
            .saturating_add(ind_retained_bytes(a))
            .saturating_add(ind_retained_bytes(b)),
        K::DataPropertyAssertion(p, a, l) | K::NegativeDataPropertyAssertion(p, a, l) => p
            .as_str()
            .len()
            .saturating_add(ind_retained_bytes(a))
            .saturating_add(lit_retained_bytes(l)),
        K::AnnotationAssertion(p, s, v) => p
            .as_str()
            .len()
            .saturating_add(match s {
                AnnotationSubject::Iri(i) => i.as_str().len(),
                AnnotationSubject::Anonymous(a) => a.0.capacity(),
            })
            .saturating_add(match v {
                AnnotationValue::Iri(i) => i.as_str().len(),
                AnnotationValue::Literal(l) => lit_retained_bytes(l),
                AnnotationValue::Anonymous(a) => a.0.capacity(),
            }),
        K::SubAnnotationPropertyOf(a, b) => a.as_str().len().saturating_add(b.as_str().len()),
        K::AnnotationPropertyDomain(p, d) | K::AnnotationPropertyRange(p, d) => {
            p.as_str().len().saturating_add(d.as_str().len())
        }
    }
}

#[inline]
fn ope_rewritten_bytes(p: &ObjectPropertyExpression) -> usize {
    if is_top(p) {
        PREFIX.len() + 1
    } else {
        p.named().as_str().len()
    }
}

fn estimate_dr_polled<E, F: FnMut() -> Result<(), E>>(
    r: &DataRange,
    poller: &mut Poller<'_, E, F>,
) -> Result<usize, E> {
    poller.tick()?;
    Ok(match r {
        DataRange::Datatype(d) => d.as_str().len(),
        DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
            let mut sum = btree_set_bytes::<DataRange>(rs.len());
            for elem in rs {
                sum = sum.saturating_add(estimate_dr_polled(elem, poller)?);
            }
            sum
        }
        DataRange::ComplementOf(inner) => {
            std::mem::size_of::<DataRange>().saturating_add(estimate_dr_polled(inner, poller)?)
        }
        DataRange::OneOf(ls) => {
            let mut sum = btree_set_bytes::<Literal>(ls.len());
            for l in ls {
                poller.tick()?;
                sum = sum.saturating_add(lit_retained_bytes(l));
            }
            sum
        }
        DataRange::DatatypeRestriction(d, fs) => {
            let mut sum = d
                .as_str()
                .len()
                .saturating_add(btree_set_bytes::<FacetRestriction>(fs.len()));
            for f in fs {
                poller.tick()?;
                sum = sum.saturating_add(
                    f.facet
                        .as_str()
                        .len()
                        .saturating_add(lit_retained_bytes(&f.value)),
                );
            }
            sum
        }
    })
}

/// Allocation-free, cooperatively polled eligibility check and rewritten heap-byte upper
/// bound for `Cx::ce_polled(c)`. Returns `Ok(Err(()))` when `c` cannot be retargeted.
fn estimate_ce_polled<E, F: FnMut() -> Result<(), E>>(
    c: &ClassExpression,
    poller: &mut Poller<'_, E, F>,
) -> Result<Result<usize, ()>, E> {
    use ClassExpression as C;
    poller.tick()?;
    Ok(Ok(match c {
        C::Class(cl) => cl.as_str().len(),
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            let mut sum = btree_set_bytes::<ClassExpression>(cs.len());
            for elem in cs {
                let Ok(h) = estimate_ce_polled(elem, poller)? else {
                    return Ok(Err(()));
                };
                sum = sum.saturating_add(h);
            }
            sum
        }
        C::ObjectComplementOf(f) => {
            let Ok(h) = estimate_ce_polled(f, poller)? else {
                return Ok(Err(()));
            };
            std::mem::size_of::<ClassExpression>().saturating_add(h)
        }
        C::ObjectOneOf(is) => {
            let mut sum = btree_set_bytes::<Individual>(is.len());
            for i in is {
                poller.tick()?;
                sum = sum.saturating_add(ind_retained_bytes(i));
            }
            sum
        }
        C::ObjectSomeValuesFrom(p, f) | C::ObjectAllValuesFrom(p, f) => {
            let Ok(h) = estimate_ce_polled(f, poller)? else {
                return Ok(Err(()));
            };
            ope_rewritten_bytes(p)
                .saturating_add(std::mem::size_of::<ClassExpression>())
                .saturating_add(h)
        }
        C::ObjectHasValue(p, i) => ope_rewritten_bytes(p).saturating_add(ind_retained_bytes(i)),
        C::ObjectHasSelf(p) if is_top(p) => return Ok(Err(())),
        C::ObjectHasSelf(p) => p.named().as_str().len(),
        C::ObjectMinCardinality(0, p, _) if is_top(p) => vocab::OWL_THING.len(),
        C::ObjectMinCardinality(1, p, f) if is_top(p) => {
            let inner = match f {
                Some(inner) => {
                    let Ok(h) = estimate_ce_polled(inner, poller)? else {
                        return Ok(Err(()));
                    };
                    h
                }
                None => vocab::OWL_THING.len(),
            };
            (PREFIX.len() + 1)
                .saturating_add(std::mem::size_of::<ClassExpression>())
                .saturating_add(inner)
        }
        C::ObjectMinCardinality(_, p, _) if is_top(p) => return Ok(Err(())),
        C::ObjectMaxCardinality(_, p, _) | C::ObjectExactCardinality(_, p, _) if is_top(p) => {
            return Ok(Err(()));
        }
        C::ObjectMinCardinality(_, p, f)
        | C::ObjectMaxCardinality(_, p, f)
        | C::ObjectExactCardinality(_, p, f) => {
            let inner = match f {
                Some(inner) => {
                    let Ok(h) = estimate_ce_polled(inner, poller)? else {
                        return Ok(Err(()));
                    };
                    std::mem::size_of::<ClassExpression>().saturating_add(h)
                }
                None => 0,
            };
            p.named().as_str().len().saturating_add(inner)
        }
        C::DataSomeValuesFrom(ps, r) | C::DataAllValuesFrom(ps, r) => {
            let mut sum = ps.len().saturating_mul(std::mem::size_of::<DataProperty>());
            for p in ps {
                poller.tick()?;
                sum = sum.saturating_add(p.as_str().len());
            }
            sum.saturating_add(estimate_dr_polled(r, poller)?)
        }
        C::DataHasValue(p, l) => p.as_str().len().saturating_add(lit_retained_bytes(l)),
        C::DataMinCardinality(_, p, r)
        | C::DataMaxCardinality(_, p, r)
        | C::DataExactCardinality(_, p, r) => {
            let r_bytes = match r {
                Some(r) => estimate_dr_polled(r, poller)?,
                None => 0,
            };
            p.as_str().len().saturating_add(r_bytes)
        }
    }))
}

/// Allocation-free, cooperatively polled eligibility check for retargeting `k`.
fn retarget_eligible_polled<E, F: FnMut() -> Result<(), E>>(
    k: &AxiomKind,
    poller: &mut Poller<'_, E, F>,
) -> Result<bool, E> {
    use AxiomKind as K;
    poller.tick()?;
    Ok(match k {
        K::SubClassOf(a, b) => {
            estimate_ce_polled(a, poller)?.is_ok() && estimate_ce_polled(b, poller)?.is_ok()
        }
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
            for c in cs {
                if estimate_ce_polled(c, poller)?.is_err() {
                    return Ok(false);
                }
            }
            true
        }
        K::ClassAssertion(c, _) | K::ObjectPropertyDomain(_, c) | K::ObjectPropertyRange(_, c) => {
            estimate_ce_polled(c, poller)?.is_ok()
        }
        K::SubObjectPropertyOf(s, sup) => {
            if !is_top(sup)
                && let SubObjectPropertyExpression::Chain(ps) = s
            {
                for _ in ps {
                    poller.tick()?;
                }
            }
            true
        }
        K::EquivalentObjectProperties(ps) => {
            for _ in ps {
                poller.tick()?;
            }
            true
        }
        K::InverseObjectProperties(p, q) => is_top(p) || is_top(q),
        K::SymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
        | K::ObjectPropertyAssertion(p, ..)
            if is_top(p) =>
        {
            true
        }
        K::NegativeObjectPropertyAssertion(..) => true,
        _ => false,
    })
}

struct PlanBudget<'a, F: FnMut(usize) -> bool> {
    live_base: usize,
    held: usize,
    admit: &'a mut F,
}

impl<F: FnMut(usize) -> bool> PlanBudget<'_, F> {
    fn try_temp(&mut self, extra: usize) -> bool {
        let Some(peak) = self
            .live_base
            .checked_add(self.held)
            .and_then(|b| b.checked_add(extra))
        else {
            return false;
        };
        (self.admit)(peak)
    }

    fn try_add(&mut self, bytes: usize) -> bool {
        let Some(next) = self.held.checked_add(bytes) else {
            return false;
        };
        let Some(peak) = self.live_base.checked_add(next) else {
            return false;
        };
        if !(self.admit)(peak) {
            return false;
        }
        self.held = next;
        true
    }

    fn release(&mut self, bytes: usize) {
        self.held = self.held.saturating_sub(bytes);
    }
}

/// Pre-admits an axiom's new heap bytes (`ax_heap - preheld`) and any `out.axioms` /
/// `out.origin` vector growth BEFORE running `make`. `preheld` is the portion of
/// `ax_heap` already accounted in `budget.held` and transferred by move into `out`.
fn push_axiom_polled<E, F: FnMut(usize) -> bool>(
    out: &mut Reduction,
    budget: &mut PlanBudget<'_, F>,
    origin_i: usize,
    ax_heap: usize,
    preheld: usize,
    make: impl FnOnce() -> Result<AxiomKind, E>,
) -> Result<bool, E> {
    let ax_grow = if out.axioms.len() == out.axioms.capacity() {
        let Some(new_cap) =
            super::dl_clause::next_vec_cap(out.axioms.capacity(), std::mem::size_of::<AxiomKind>())
        else {
            return Ok(false);
        };
        let Some(bytes) = new_cap.checked_mul(std::mem::size_of::<AxiomKind>()) else {
            return Ok(false);
        };
        bytes
    } else {
        0
    };
    let orig_grow = if out.origin.len() == out.origin.capacity() {
        let Some(new_cap) =
            super::dl_clause::next_vec_cap(out.origin.capacity(), std::mem::size_of::<usize>())
        else {
            return Ok(false);
        };
        let Some(bytes) = new_cap.checked_mul(std::mem::size_of::<usize>()) else {
            return Ok(false);
        };
        bytes
    } else {
        0
    };
    let extra_ax = ax_heap.saturating_sub(preheld);
    let Some(peak_add) = extra_ax
        .checked_add(ax_grow)
        .and_then(|b| b.checked_add(orig_grow))
    else {
        return Ok(false);
    };
    if !budget.try_temp(peak_add) {
        return Ok(false);
    }
    let ax = make()?;
    let old_ax_cap = out.axioms.capacity();
    let old_orig_cap = out.origin.capacity();
    out.axioms.push(ax);
    out.origin.push(origin_i);
    let ax_delta = out
        .axioms
        .capacity()
        .saturating_sub(old_ax_cap)
        .saturating_mul(std::mem::size_of::<AxiomKind>());
    let orig_delta = out
        .origin
        .capacity()
        .saturating_sub(old_orig_cap)
        .saturating_mul(std::mem::size_of::<usize>());
    budget.held = budget
        .held
        .saturating_add(extra_ax)
        .saturating_add(ax_delta)
        .saturating_add(orig_delta);
    out.axiom_heap_bytes = out.axiom_heap_bytes.saturating_add(ax_heap);
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn disjoint_budgeted<E, F: FnMut(usize) -> bool, P: FnMut() -> Result<(), E>>(
    cx: &mut Cx,
    cs: &BTreeSet<ClassExpression>,
    union_class: Option<&Class>,
    origin_i: usize,
    out: &mut Reduction,
    budget: &mut PlanBudget<'_, F>,
    poller: &mut Poller<'_, E, P>,
) -> Result<bool, E> {
    let mut images: BTreeSet<ClassExpression> = BTreeSet::new();
    let mut empty: BTreeMap<ClassExpression, usize> = BTreeMap::new();
    let mut images_elem_bytes = 0usize;
    let mut empty_elem_bytes = 0usize;
    let mut images_tree_bytes = 0usize;
    let mut empty_tree_bytes = 0usize;

    for (idx, elem) in cs.iter().enumerate() {
        #[cfg(test)]
        if idx == 1 {
            super::admission_test::merge_phase_retarget();
        }
        #[cfg(not(test))]
        let _ = idx;
        poller.tick()?;
        let Ok(d_bytes) = estimate_ce_polled(elem, poller)? else {
            unreachable!("retarget_eligible_polled already validated disjoint operands");
        };
        let img_next = btree_set_bytes::<ClassExpression>(images.len() + 1);
        let emp_next = btree_map_bytes::<ClassExpression, usize>(empty.len() + 1);
        let max_tree_delta = img_next
            .saturating_sub(images_tree_bytes)
            .max(emp_next.saturating_sub(empty_tree_bytes));
        let Some(need) = d_bytes.checked_add(max_tree_delta) else {
            return Ok(false);
        };
        if !budget.try_temp(need) {
            return Ok(false);
        }
        let Ok(d) = cx.ce_polled(elem, poller)? else {
            unreachable!("retarget_eligible_polled already validated disjoint operands");
        };
        if images.contains(&d) {
            if let std::collections::btree_map::Entry::Vacant(e) = empty.entry(d) {
                e.insert(d_bytes);
                let delta = emp_next.saturating_sub(empty_tree_bytes);
                empty_tree_bytes = emp_next;
                empty_elem_bytes = empty_elem_bytes.saturating_add(d_bytes);
                budget.held = budget.held.saturating_add(d_bytes).saturating_add(delta);
            }
        } else {
            images.insert(d);
            let delta = img_next.saturating_sub(images_tree_bytes);
            images_tree_bytes = img_next;
            images_elem_bytes = images_elem_bytes.saturating_add(d_bytes);
            budget.held = budget.held.saturating_add(d_bytes).saturating_add(delta);
        }
    }
    #[cfg(test)]
    if cs.len() >= 2 {
        super::admission_test::merge_phase_retarget_done();
    }

    for (d, d_h) in empty {
        poller.tick()?;
        let transfer = d_h.min(empty_elem_bytes);
        empty_elem_bytes -= transfer;
        let ax_heap = d_h.saturating_add(vocab::OWL_NOTHING.len());
        if !push_axiom_polled(out, budget, origin_i, ax_heap, transfer, || {
            Ok(AxiomKind::SubClassOf(
                d,
                ClassExpression::Class(Class::nothing()),
            ))
        })? {
            return Ok(false);
        }
    }
    budget.release(empty_elem_bytes.saturating_add(empty_tree_bytes));

    match union_class {
        None => {
            if images.len() >= 2 {
                let preheld = images_tree_bytes.saturating_add(images_elem_bytes);
                if !push_axiom_polled(out, budget, origin_i, preheld, preheld, || {
                    Ok(AxiomKind::DisjointClasses(images))
                })? {
                    return Ok(false);
                }
            } else {
                drop(images);
                budget.release(images_tree_bytes.saturating_add(images_elem_bytes));
            }
        }
        Some(c) => {
            if images.len() >= 2 {
                let preheld = images_tree_bytes.saturating_add(images_elem_bytes);
                let ax_heap = preheld.saturating_add(c.as_str().len());
                if !push_axiom_polled(out, budget, origin_i, ax_heap, preheld, || {
                    Ok(AxiomKind::DisjointUnion(c.clone(), images))
                })? {
                    return Ok(false);
                }
            } else if let Some(d) = images.into_iter().next() {
                budget.release(images_tree_bytes);
                if !matches!(&d, ClassExpression::Class(dc) if dc == c) {
                    let d_h = images_elem_bytes;
                    let ax_heap = btree_set_bytes::<ClassExpression>(2)
                        .saturating_add(c.as_str().len())
                        .saturating_add(d_h);
                    if !push_axiom_polled(out, budget, origin_i, ax_heap, d_h, || {
                        Ok(AxiomKind::EquivalentClasses(
                            [ClassExpression::Class(c.clone()), d].into(),
                        ))
                    })? {
                        return Ok(false);
                    }
                } else {
                    drop(d);
                    budget.release(images_elem_bytes);
                }
            } else {
                budget.release(images_tree_bytes.saturating_add(images_elem_bytes));
            }
        }
    }
    Ok(true)
}

/// Streams retargeted axioms for a pre-validated `k` directly into `out`, pre-admitting
/// every rewritten payload and overlapping buffer before construction and polling
/// cooperatively per operand/emitted axiom. Returns `Ok(false)` when `budget` declines.
#[allow(clippy::too_many_arguments)]
fn retarget_into<E, F: FnMut(usize) -> bool, P: FnMut() -> Result<(), E>>(
    cx: &mut Cx,
    k: &AxiomKind,
    origin_i: usize,
    out: &mut Reduction,
    budget: &mut PlanBudget<'_, F>,
    poller: &mut Poller<'_, E, P>,
) -> Result<bool, E> {
    use AxiomKind as K;
    let u_len = PREFIX.len() + 1;
    match k {
        K::SubClassOf(a, b) => {
            let Ok(ah) = estimate_ce_polled(a, poller)? else {
                unreachable!("pre-validated");
            };
            let Ok(bh) = estimate_ce_polled(b, poller)? else {
                unreachable!("pre-validated");
            };
            let ax_heap = ah.saturating_add(bh);
            push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                let ra = cx.ce_polled(a, poller)?.expect("pre-validated");
                let rb = cx.ce_polled(b, poller)?.expect("pre-validated");
                Ok(K::SubClassOf(ra, rb))
            })
        }
        K::EquivalentClasses(cs) => {
            let mut ax_heap = btree_set_bytes::<ClassExpression>(cs.len());
            for (idx, c) in cs.iter().enumerate() {
                #[cfg(test)]
                if idx == 1 {
                    super::admission_test::merge_phase_retarget();
                }
                #[cfg(not(test))]
                let _ = idx;
                let Ok(h) = estimate_ce_polled(c, poller)? else {
                    unreachable!("pre-validated");
                };
                ax_heap = ax_heap.saturating_add(h);
            }
            #[cfg(test)]
            if cs.len() >= 2 {
                super::admission_test::merge_phase_retarget_done();
            }
            push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                let rcs = cx.ces_polled(cs, poller)?.expect("pre-validated");
                Ok(K::EquivalentClasses(rcs))
            })
        }
        K::DisjointClasses(cs) => disjoint_budgeted(cx, cs, None, origin_i, out, budget, poller),
        K::DisjointUnion(c, cs) => {
            disjoint_budgeted(cx, cs, Some(c), origin_i, out, budget, poller)
        }
        K::ClassAssertion(c, i) => {
            let Ok(ch) = estimate_ce_polled(c, poller)? else {
                unreachable!("pre-validated");
            };
            let ax_heap = ch.saturating_add(ind_retained_bytes(i));
            push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                let rc = cx.ce_polled(c, poller)?.expect("pre-validated");
                Ok(K::ClassAssertion(rc, i.clone()))
            })
        }
        // every element is in the domain and in the range of the universal property
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) if is_top(p) => {
            let Ok(ch) = estimate_ce_polled(c, poller)? else {
                unreachable!("pre-validated");
            };
            let ax_heap = vocab::OWL_THING.len().saturating_add(ch);
            push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                let rc = cx.ce_polled(c, poller)?.expect("pre-validated");
                Ok(K::SubClassOf(ClassExpression::Class(Class::thing()), rc))
            })
        }
        K::ObjectPropertyDomain(p, c) => {
            let Ok(ch) = estimate_ce_polled(c, poller)? else {
                unreachable!("pre-validated");
            };
            let ax_heap = p.named().as_str().len().saturating_add(ch);
            push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                let rc = cx.ce_polled(c, poller)?.expect("pre-validated");
                Ok(K::ObjectPropertyDomain(p.clone(), rc))
            })
        }
        K::ObjectPropertyRange(p, c) => {
            let Ok(ch) = estimate_ce_polled(c, poller)? else {
                unreachable!("pre-validated");
            };
            let ax_heap = p.named().as_str().len().saturating_add(ch);
            push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                let rc = cx.ce_polled(c, poller)?.expect("pre-validated");
                Ok(K::ObjectPropertyRange(p.clone(), rc))
            })
        }
        K::SubObjectPropertyOf(_, sup) if is_top(sup) => Ok(true),
        K::SubObjectPropertyOf(s, sup) => {
            let sub_heap = match s {
                SubObjectPropertyExpression::Expression(p) => ope_rewritten_bytes(p),
                SubObjectPropertyExpression::Chain(ps) => {
                    let mut sum = ps
                        .len()
                        .saturating_mul(std::mem::size_of::<ObjectPropertyExpression>());
                    for p in ps {
                        poller.tick()?;
                        sum = sum.saturating_add(ope_rewritten_bytes(p));
                    }
                    sum
                }
            };
            let ax_heap = sup.named().as_str().len().saturating_add(sub_heap);
            push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                let rs = match s {
                    SubObjectPropertyExpression::Expression(p) => {
                        SubObjectPropertyExpression::Expression(cx.ope(p))
                    }
                    SubObjectPropertyExpression::Chain(ps) => {
                        let mut chain = Vec::with_capacity(ps.len());
                        for (idx, p) in ps.iter().enumerate() {
                            #[cfg(test)]
                            if idx == 1 {
                                super::admission_test::merge_phase_retarget();
                            }
                            #[cfg(not(test))]
                            let _ = idx;
                            poller.tick()?;
                            chain.push(cx.ope(p));
                        }
                        #[cfg(test)]
                        if ps.len() >= 2 {
                            super::admission_test::merge_phase_retarget_done();
                        }
                        SubObjectPropertyExpression::Chain(chain)
                    }
                };
                Ok(K::SubObjectPropertyOf(rs, sup.clone()))
            })
        }
        // the properties equivalent to the universal property are below it
        K::EquivalentObjectProperties(ps) => {
            #[cfg(test)]
            let mut emitted = 0usize;
            for p in ps {
                #[cfg(test)]
                if emitted == 1 {
                    super::admission_test::merge_phase_retarget();
                }
                poller.tick()?;
                if is_top(p) {
                    continue;
                }
                let ax_heap = u_len.saturating_add(p.named().as_str().len());
                if !push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                    cx.used = true;
                    Ok(K::SubObjectPropertyOf(
                        SubObjectPropertyExpression::Expression(
                            ObjectPropertyExpression::Property(u()),
                        ),
                        p.clone(),
                    ))
                })? {
                    return Ok(false);
                }
                #[cfg(test)]
                {
                    emitted += 1;
                }
            }
            #[cfg(test)]
            if emitted >= 2 {
                super::admission_test::merge_phase_retarget_done();
            }
            Ok(true)
        }
        K::InverseObjectProperties(p, q) => match (is_top(p), is_top(q)) {
            (true, true) => Ok(true),
            (true, false) | (false, true) => {
                let other = if is_top(p) { q } else { p };
                let ax_heap = u_len.saturating_add(other.named().as_str().len());
                push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                    cx.used = true;
                    Ok(K::SubObjectPropertyOf(
                        SubObjectPropertyExpression::Expression(
                            ObjectPropertyExpression::Property(u()),
                        ),
                        other.clone(),
                    ))
                })
            }
            _ => unreachable!("pre-validated"),
        },
        K::SymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
            if is_top(p) =>
        {
            Ok(true)
        }
        K::ObjectPropertyAssertion(p, ..) if is_top(p) => Ok(true),
        K::NegativeObjectPropertyAssertion(p, a, b) => {
            let ax_heap = ope_rewritten_bytes(p)
                .saturating_add(ind_retained_bytes(a))
                .saturating_add(ind_retained_bytes(b));
            push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                Ok(K::NegativeObjectPropertyAssertion(
                    cx.ope(p),
                    a.clone(),
                    b.clone(),
                ))
            })
        }
        _ => unreachable!("pre-validated"),
    }
}

/// Outcome of scanning one axiom during `plan_budgeted`.
enum Blocked {
    Ok,
    Ineligible,
    BudgetDeclined,
}

enum VisitStop<E> {
    Ineligible,
    BudgetDeclined,
    Poll(E),
}

/// Borrowed signature key for restoring classes and named individuals of replaced axioms
/// without cloning IRI strings before `push_axiom_polled` admits them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SigRef<'a> {
    Class(&'a Class),
    NamedIndividual(&'a NamedIndividual),
}

/// Whether the axioms hold something that keeps a rooted run of Tier D from them whatever
/// the rewriting does, data, the bottom property or a reflexive property, or a name with
/// the prefix of the made-up names, admitting each newly added canonical individual
/// reference into `inds` against `budget` without cloning IRIs and with per-entity
/// cooperative polling.
fn blocked<'a, E, F: FnMut(usize) -> bool, P: FnMut() -> Result<(), E>>(
    k: &'a AxiomKind,
    inds: &mut BTreeSet<&'a NamedIndividual>,
    canon: Option<&'a super::index::CanonMap>,
    budget: &mut PlanBudget<'_, F>,
    poller: &mut Poller<'_, E, P>,
) -> Result<Blocked, E> {
    if matches!(k, AxiomKind::ReflexiveObjectProperty(p) if !is_top(p))
        || matches!(k, AxiomKind::HasKey(_, _, dps) if !dps.is_empty())
    {
        return Ok(Blocked::Ineligible);
    }
    // Pass A: zero-allocation eligibility check + per-entity polling before admitting
    // any individual reference, so ineligible axioms always report `Ineligible`.
    #[cfg(test)]
    let mut visited = 0usize;
    let check = visit_axiom_entities(k, &mut |e| {
        #[cfg(test)]
        {
            if visited == 16 {
                super::admission_test::merge_phase_blocked_scan();
            }
            visited += 1;
        }
        poller.tick().map_err(VisitStop::Poll)?;
        if e.iri_str().is_some_and(|s| s.starts_with(PREFIX)) {
            return Err(VisitStop::Ineligible);
        }
        match e {
            EntityRef::DataProperty(_) | EntityRef::Datatype(_) => Err(VisitStop::Ineligible),
            EntityRef::ObjectProperty(p) if p.as_str() == vocab::OWL_BOTTOM_OBJECT_PROPERTY => {
                Err(VisitStop::Ineligible)
            }
            _ => Ok(()),
        }
    });
    match check {
        Ok(()) => {}
        Err(VisitStop::Ineligible) => return Ok(Blocked::Ineligible),
        Err(VisitStop::Poll(e)) => return Err(e),
        Err(VisitStop::BudgetDeclined) => unreachable!(),
    }
    #[cfg(test)]
    if visited >= 16 {
        super::admission_test::merge_phase_blocked_scan_done();
    }
    // Pass B: collect borrowed canonical `&'a NamedIndividual` references with exact
    // `BTreeSet` node pre-admission and zero string cloning.
    let collect = visit_axiom_entities(k, &mut |e| {
        poller.tick().map_err(VisitStop::Poll)?;
        if let EntityRef::NamedIndividual(i) = e {
            let rep = canon.and_then(|m| m.get(i)).unwrap_or(i);
            if !inds.contains(&rep) {
                let old_tree = btree_set_bytes::<&NamedIndividual>(inds.len());
                let new_tree = btree_set_bytes::<&NamedIndividual>(inds.len() + 1);
                let add = new_tree.saturating_sub(old_tree);
                if !budget.try_add(add) {
                    return Err(VisitStop::BudgetDeclined);
                }
                inds.insert(rep);
            }
        }
        Ok(())
    });
    match collect {
        Ok(()) => Ok(Blocked::Ok),
        Err(VisitStop::BudgetDeclined) => Ok(Blocked::BudgetDeclined),
        Err(VisitStop::Poll(e)) => Err(e),
        Err(VisitStop::Ineligible) => unreachable!(),
    }
}

/// The rewriting of the axioms for a rooted run, or `None` when no axiom needs it, when
/// the ontology has no axiom that needs case analysis, which the Horn rules take, when
/// something keeps Tier D from it anyway, or when an axiom has no reduction or the keys'
/// reductions pass the bound. `memory` is the run's memory budget. When `canon` is given,
/// key subjects and object-key values range over the distinct canonical images of the
/// original named signature.
#[cfg_attr(not(test), allow(dead_code))]
pub fn plan(
    axioms: &[&AxiomKind],
    memory: Option<usize>,
    canon: Option<&super::index::CanonMap>,
) -> Option<Reduction> {
    match plan_budgeted(
        axioms,
        memory,
        canon,
        0,
        &mut || Ok::<(), ()>(()),
        &mut |_| true,
    ) {
        Ok(Plan::Ready(red)) => Some(red),
        _ => None,
    }
}

/// Plans item 805 reductions with per-entity/per-operand cooperative polling and exact
/// memory pre-admission against `live_base` (including any live merge partition and
/// canonical map) without allocating uncharged scratch signatures or intermediate
/// rewritten buffers.
pub fn plan_budgeted<'a, E>(
    axioms: &[&'a AxiomKind],
    memory: Option<usize>,
    canon: Option<&'a super::index::CanonMap>,
    live_base: usize,
    poll: &mut impl FnMut() -> Result<(), E>,
    admit: &mut impl FnMut(usize) -> bool,
) -> Result<Plan, E> {
    let mut poller = Poller { steps: 0, poll };
    // an inclusion in the top property alone needs no rewriting, since the run takes it
    // as an axiom that adds nothing (roadmap item 1282)
    let mut needed = false;
    for &k in axioms {
        poller.tick()?;
        if (!super::index::top_inclusion(k) && ax_has_top_polled(k, &mut poller)?)
            || matches!(k, AxiomKind::HasKey(_, ops, dps) if !ops.is_empty() && dps.is_empty())
        {
            needed = true;
            break;
        }
    }
    if !needed {
        return Ok(Plan::NotNeeded);
    }
    let mut budget = PlanBudget {
        live_base,
        held: 0,
        admit,
    };
    let mut inds: BTreeSet<&'a NamedIndividual> = BTreeSet::new();
    for &k in axioms {
        poller.tick()?;
        // an inclusion in a top property is dropped below, and its subproperty, such as
        // a data property, keeps nothing from the rewriting
        if super::index::top_inclusion(k) {
            continue;
        }
        match blocked(k, &mut inds, canon, &mut budget, &mut poller)? {
            Blocked::Ok => {}
            Blocked::Ineligible => return Ok(Plan::NotNeeded),
            Blocked::BudgetDeclined => return Ok(Plan::Declined),
        }
    }
    let mut has_disj = false;
    for &k in axioms {
        poller.tick()?;
        #[cfg(test)]
        let mut horn_ops = 0usize;
        let horn = dl::is_horn_polled(k, &mut || {
            #[cfg(test)]
            {
                if horn_ops == 16 {
                    super::admission_test::merge_phase_horn_check();
                }
                horn_ops += 1;
            }
            poller.tick()
        })?;
        #[cfg(test)]
        if horn_ops >= 16 {
            super::admission_test::merge_phase_horn_check_done();
        }
        if !horn {
            has_disj = true;
            break;
        }
    }
    if !has_disj {
        return Ok(Plan::NotNeeded);
    }
    let inds_tree_bytes = btree_set_bytes::<&NamedIndividual>(inds.len());
    let Some(inds_vec_bytes) = inds
        .len()
        .checked_mul(std::mem::size_of::<&NamedIndividual>())
    else {
        return Ok(Plan::Declined);
    };
    if !budget.try_add(inds_vec_bytes) {
        return Ok(Plan::Declined);
    }
    let mut inds_vec: Vec<&'a NamedIndividual> = Vec::with_capacity(inds.len());
    #[cfg(test)]
    let mut drained = 0usize;
    for ind in inds {
        #[cfg(test)]
        {
            if drained == 16 {
                super::admission_test::merge_phase_inds_drain();
            }
            drained += 1;
        }
        poller.tick()?;
        inds_vec.push(ind);
    }
    #[cfg(test)]
    if drained >= 16 {
        super::admission_test::merge_phase_inds_drain_done();
    }
    let inds = inds_vec;
    budget.release(inds_tree_bytes);

    let n = inds.len();
    let pairs = n * n.saturating_sub(1) / 2;
    let limit = memory.map_or(KEY_AXIOMS, |m| (m / AXIOM_BYTES).min(KEY_AXIOMS));
    if !budget.try_add(axioms.len()) {
        return Ok(Plan::Declined);
    }
    let mut cx = Cx::default();
    let mut out = Reduction {
        axioms: Vec::new(),
        origin: Vec::new(),
        replaced: vec![false; axioms.len()],
        hub: None,
        axiom_heap_bytes: 0,
    };
    let mut any_replaced = false;
    let mut first_use = None;
    let mut keys = 0usize;
    let mut total = 0usize;
    for (i, &k) in axioms.iter().enumerate() {
        poller.tick()?;
        if let AxiomKind::HasKey(c, ops, _) = k
            && !ops.is_empty()
        {
            let m = ops.len();
            total = total.saturating_add(m.saturating_mul(n).saturating_mul(pairs + 1) + pairs);
            if total > limit {
                return Ok(Plan::Declined);
            }
            let Ok(c_rewritten_heap) = estimate_ce_polled(c, &mut poller)? else {
                return Ok(Plan::NotNeeded);
            };
            let mut ops_has_top = false;
            for p in ops {
                poller.tick()?;
                if is_top(p) {
                    ops_has_top = true;
                }
            }
            let before = cx.used;
            // a class expression other than a name is named once, so that the many
            // axioms of the key hold a name and not a copy of the expression; when the
            // rewrite itself produces a named class (e.g., `≥0 topObjectProperty` ->
            // `owl:Thing`), reuse that name directly without emitting a fresh equivalence
            let (owned_name, owned_name_len) = match c {
                ClassExpression::Class(_) => {
                    if ops_has_top {
                        cx.used = true;
                    }
                    if cx.used && !before {
                        first_use.get_or_insert(i);
                    }
                    (None, 0)
                }
                ClassExpression::ObjectMinCardinality(0, p, _) if is_top(p) => {
                    if ops_has_top {
                        cx.used = true;
                    }
                    if cx.used && !before {
                        first_use.get_or_insert(i);
                    }
                    if inds.len() < 2 {
                        (None, 0)
                    } else {
                        let thing_len = vocab::OWL_THING.len();
                        if !budget.try_add(thing_len) {
                            return Ok(Plan::Declined);
                        }
                        (Some(ClassExpression::Class(Class::thing())), thing_len)
                    }
                }
                _ => {
                    if !budget.try_add(c_rewritten_heap) {
                        return Ok(Plan::Declined);
                    }
                    let rewritten_c = cx.ce_polled(c, &mut poller)?.expect("pre-validated");
                    if ops_has_top {
                        cx.used = true;
                    }
                    if cx.used && !before {
                        first_use.get_or_insert(i);
                    }
                    if matches!(rewritten_c, ClassExpression::Class(_)) {
                        (Some(rewritten_c), c_rewritten_heap)
                    } else {
                        let name_len = PREFIX.len() + 1 + dec_len(keys);
                        if !budget.try_add(name_len) {
                            return Ok(Plan::Declined);
                        }
                        let name = ClassExpression::Class(exact_k_class(keys, name_len));
                        let eq_heap = btree_set_bytes::<ClassExpression>(2)
                            .saturating_add(name_len)
                            .saturating_add(c_rewritten_heap);
                        if !push_axiom_polled(
                            &mut out,
                            &mut budget,
                            i,
                            eq_heap,
                            c_rewritten_heap,
                            || {
                                Ok(AxiomKind::EquivalentClasses(
                                    [name.clone(), rewritten_c].into(),
                                ))
                            },
                        )? {
                            return Ok(Plan::Declined);
                        }
                        (Some(name), name_len)
                    }
                }
            };
            let key_c = owned_name.as_ref().unwrap_or(c);
            let ok = key_budgeted(
                keys,
                key_c,
                ops,
                &inds,
                i,
                &mut out,
                &mut budget,
                &mut poller,
            )?;
            drop(owned_name);
            budget.release(owned_name_len);
            if !ok {
                return Ok(Plan::Declined);
            }
            keys += 1;
            out.replaced[i] = true;
            any_replaced = true;
            continue;
        }
        if !ax_has_top_polled(k, &mut poller)? {
            continue;
        }
        if !retarget_eligible_polled(k, &mut poller)? {
            return Ok(Plan::NotNeeded);
        }
        let before = cx.used;
        if !retarget_into(&mut cx, k, i, &mut out, &mut budget, &mut poller)? {
            return Ok(Plan::Declined);
        }
        if cx.used && !before {
            first_use.get_or_insert(i);
        }
        out.replaced[i] = true;
        any_replaced = true;
    }
    drop(inds);
    budget.release(inds_vec_bytes);

    if let Some(i) = first_use {
        let hub_len = HUB_IRI.len();
        if !budget.try_add(hub_len) {
            return Ok(Plan::Declined);
        }
        out.hub = Some(NamedIndividual::new(HUB_IRI));
        let u_len = U_IRI.len();
        let reach_heap = vocab::OWL_THING
            .len()
            .saturating_add(u_len)
            .saturating_add(std::mem::size_of::<ClassExpression>())
            .saturating_add(btree_set_bytes::<Individual>(1))
            .saturating_add(hub_len);
        if !push_axiom_polled(&mut out, &mut budget, i, reach_heap, 0, || {
            Ok(AxiomKind::SubClassOf(
                ClassExpression::Class(Class::thing()),
                ClassExpression::ObjectSomeValuesFrom(
                    ObjectPropertyExpression::Property(u()),
                    Box::new(ClassExpression::ObjectOneOf([hub()].into())),
                ),
            ))
        })? {
            return Ok(Plan::Declined);
        }
        if !push_axiom_polled(&mut out, &mut budget, i, u_len, 0, || {
            Ok(AxiomKind::SymmetricObjectProperty(
                ObjectPropertyExpression::Property(u()),
            ))
        })? {
            return Ok(Plan::Declined);
        }
        if !push_axiom_polled(&mut out, &mut budget, i, u_len, 0, || {
            Ok(AxiomKind::TransitiveObjectProperty(
                ObjectPropertyExpression::Property(u()),
            ))
        })? {
            return Ok(Plan::Declined);
        }
    }
    // the classes and individuals of the replaced axioms stay in the signature, though the
    // rewriting may drop them
    let mut sig_at: BTreeMap<SigRef<'a>, usize> = BTreeMap::new();
    for (i, &k) in axioms.iter().enumerate() {
        poller.tick()?;
        if out.replaced[i] {
            let res = visit_axiom_entities(k, &mut |e| {
                poller.tick().map_err(VisitStop::Poll)?;
                let key = match e {
                    EntityRef::Class(c) => SigRef::Class(c),
                    EntityRef::NamedIndividual(n) => SigRef::NamedIndividual(n),
                    _ => return Ok(()),
                };
                if !sig_at.contains_key(&key) {
                    let old_tree = btree_map_bytes::<SigRef<'_>, usize>(sig_at.len());
                    let new_tree = btree_map_bytes::<SigRef<'_>, usize>(sig_at.len() + 1);
                    let add = new_tree.saturating_sub(old_tree);
                    if !budget.try_add(add) {
                        return Err(VisitStop::BudgetDeclined);
                    }
                    sig_at.insert(key, i);
                }
                Ok(())
            });
            match res {
                Ok(()) => {}
                Err(VisitStop::BudgetDeclined) => return Ok(Plan::Declined),
                Err(VisitStop::Poll(e)) => return Err(e),
                Err(VisitStop::Ineligible) => unreachable!(),
            }
        }
    }
    let sig_tree_bytes = btree_map_bytes::<SigRef<'_>, usize>(sig_at.len());
    for (key, origin_i) in sig_at {
        poller.tick()?;
        let ok = match key {
            SigRef::Class(c) => {
                let h = c.as_str().len();
                push_axiom_polled(&mut out, &mut budget, origin_i, h, 0, || {
                    Ok(AxiomKind::Declaration(Entity::Class(Class::new(
                        c.as_str().to_owned(),
                    ))))
                })?
            }
            SigRef::NamedIndividual(n) => {
                let h = vocab::OWL_THING.len().saturating_add(n.as_str().len());
                push_axiom_polled(&mut out, &mut budget, origin_i, h, 0, || {
                    Ok(AxiomKind::ClassAssertion(
                        ClassExpression::Class(Class::thing()),
                        Individual::Named(super::index::exact_individual(n)),
                    ))
                })?
            }
        };
        if !ok {
            return Ok(Plan::Declined);
        }
    }
    budget.release(sig_tree_bytes);
    if any_replaced {
        Ok(Plan::Ready(out))
    } else {
        Ok(Plan::NotNeeded)
    }
}

/// Emits the class axioms that stand for the key `HasKey(c (ops) ())`, number `k`, over the
/// named individuals `inds`, admitting every axiom and temporary buffer against `budget`
/// and polling `poller` cooperatively on every emitted axiom and every key role inside the
/// final key-body construction. Returns `Ok(false)` when `budget` declines.
#[allow(clippy::too_many_arguments)]
fn key_budgeted<E, F: FnMut(usize) -> bool, P: FnMut() -> Result<(), E>>(
    k: usize,
    c: &ClassExpression,
    ops: &BTreeSet<ObjectPropertyExpression>,
    inds: &[&NamedIndividual],
    origin_i: usize,
    out: &mut Reduction,
    budget: &mut PlanBudget<'_, F>,
    poller: &mut Poller<'_, E, P>,
) -> Result<bool, E> {
    use ClassExpression as C;
    if inds.len() < 2 {
        return Ok(true);
    }
    let one = |i: &NamedIndividual| {
        C::ObjectOneOf([Individual::Named(super::index::exact_individual(i))].into())
    };
    let make_ope = |p: &ObjectPropertyExpression| {
        if is_top(p) {
            match p {
                ObjectPropertyExpression::Property(_) => ObjectPropertyExpression::Property(u()),
                ObjectPropertyExpression::InverseOf(_) => ObjectPropertyExpression::InverseOf(u()),
            }
        } else {
            p.clone()
        }
    };
    let one_node = btree_set_bytes::<Individual>(1);
    let inter2_node = btree_set_bytes::<ClassExpression>(2);
    let box_ce = std::mem::size_of::<ClassExpression>();
    let c_heap = ce_retained_bytes(c);
    let v_prefix = PREFIX.len() + 1 + dec_len(k) + 1;
    let m_prefix = PREFIX.len() + 1 + dec_len(k) + 1;
    // V_jb holds the r_j-values of b when b is a C
    let v_len = |j: usize, b: usize| v_prefix + dec_len(j) + 1 + dec_len(b);
    let m_len =
        |j: usize, a: usize, b: usize| m_prefix + dec_len(j) + 1 + dec_len(a) + 1 + dec_len(b);
    let v = |j: usize, b: usize| exact_v_class(k, j, b, v_len(j, b));
    let m_cls = |j: usize, a: usize, b: usize| exact_m_class(k, j, a, b, m_len(j, a, b));
    for (b, &ib) in inds.iter().enumerate().skip(1) {
        let ib_len = ib.as_str().len();
        for (j, r) in ops.iter().enumerate() {
            poller.tick()?;
            let ax_heap = inter2_node
                .saturating_add(one_node)
                .saturating_add(ib_len)
                .saturating_add(c_heap)
                .saturating_add(ope_rewritten_bytes(r))
                .saturating_add(box_ce)
                .saturating_add(v_len(j, b));
            if !push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                Ok(AxiomKind::SubClassOf(
                    C::ObjectIntersectionOf([one(ib), c.clone()].into()),
                    C::ObjectAllValuesFrom(make_ope(r), Box::new(C::Class(v(j, b)))),
                ))
            })? {
                return Ok(false);
            }
        }
    }
    let body_node = btree_set_bytes::<ClassExpression>(2usize.saturating_add(ops.len()));
    for (a, &ia) in inds.iter().enumerate() {
        let ia_len = ia.as_str().len();
        for (b, &ib) in inds.iter().enumerate().skip(a + 1) {
            let mut m_total_len = 0usize;
            for (j, r) in ops.iter().enumerate() {
                let r_len = ope_rewritten_bytes(r);
                let vj_len = v_len(j, b);
                let mj_len = m_len(j, a, b);
                m_total_len = m_total_len.saturating_add(mj_len);
                let tmp_len = mj_len.saturating_add(vj_len);
                if !budget.try_add(tmp_len) {
                    return Ok(false);
                }
                // M_jab: a has a named r_j-value that b, a C, has too
                let m = m_cls(j, a, b);
                let vj = v(j, b);
                for &ic in inds {
                    poller.tick()?;
                    let ax_heap = inter2_node
                        .saturating_add(one_node)
                        .saturating_add(ia_len)
                        .saturating_add(r_len)
                        .saturating_add(box_ce)
                        .saturating_add(inter2_node)
                        .saturating_add(one_node)
                        .saturating_add(ic.as_str().len())
                        .saturating_add(vj_len)
                        .saturating_add(mj_len);
                    if !push_axiom_polled(out, budget, origin_i, ax_heap, 0, || {
                        Ok(AxiomKind::SubClassOf(
                            C::ObjectIntersectionOf(
                                [
                                    one(ia),
                                    C::ObjectSomeValuesFrom(
                                        make_ope(r),
                                        Box::new(C::ObjectIntersectionOf(
                                            [one(ic), C::Class(vj.clone())].into(),
                                        )),
                                    ),
                                ]
                                .into(),
                            ),
                            C::Class(m.clone()),
                        ))
                    })? {
                        return Ok(false);
                    }
                }
                drop((m, vj));
                budget.release(tmp_len);
            }
            poller.tick()?;
            let final_heap = body_node
                .saturating_add(one_node)
                .saturating_add(ia_len)
                .saturating_add(c_heap)
                .saturating_add(m_total_len)
                .saturating_add(one_node)
                .saturating_add(ib.as_str().len());
            if !push_axiom_polled(out, budget, origin_i, final_heap, 0, || {
                let mut body = BTreeSet::new();
                body.insert(one(ia));
                body.insert(c.clone());
                for j in 0..ops.len() {
                    #[cfg(test)]
                    if j == 1 {
                        super::admission_test::merge_phase_key_body();
                    }
                    poller.tick()?;
                    body.insert(C::Class(m_cls(j, a, b)));
                }
                #[cfg(test)]
                if ops.len() >= 2 {
                    super::admission_test::merge_phase_key_body_done();
                }
                Ok(AxiomKind::SubClassOf(
                    C::ObjectIntersectionOf(body),
                    one(ib),
                ))
            })? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
